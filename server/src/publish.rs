//! Publishing a folder to other programs: as an S3 bucket, or as an FTP receive folder. Which folders can be, and who
//! may: one check for both, made again on every request with what the account and the space are now.

use sqlx::SqliteConnection;

use crate::{
    auth::{self, User},
    error::{AppError, AppResult},
    tree::{self, LinkMakers, Need, Node, Role, SpaceKind},
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Publication {
    S3Bucket,
    FtpReceiveFolder,
}

impl Publication {
    fn unpublishable(self) -> &'static str {
        match self {
            Self::S3Bucket => "This folder can't be published as an S3 bucket",
            Self::FtpReceiveFolder => "This folder can't be published as an FTP receive folder",
        }
    }
    fn managers_only(self) -> &'static str {
        match self {
            Self::S3Bucket => "Only allowed space managers can publish S3 buckets",
            Self::FtpReceiveFolder => "Only allowed space managers can publish FTP receive folders",
        }
    }
}

/// The folder `id`, when `user` may publish it as `kind`: not as a guest; a folder of a team or company space that is
/// on and lets someone make share links, out of the trash with every folder it is in; and the user an administrator,
/// or, when `managers` may publish, a manager there whose account may share. An FTP receive folder only takes uploads,
/// so it also needs a space that takes changes and a publisher who may add files there (`may_receive`); a bucket of a
/// read-only space can still have read-only keys.
pub async fn publisher(conn: &mut SqliteConnection, user: &User, id: &str, kind: Publication, managers: bool) -> AppResult<Node> {
    let current = auth::current_user(conn, user).await?;
    let user = &current;
    if user.guest {
        return Err(AppError::forbidden("Guest accounts can't publish folders"));
    }
    let node = tree::get_node(conn, id).await?.ok_or_else(|| AppError::not_found("Folder not found"))?;
    let drive = tree::get_drive(conn, node.drive()).await?.ok_or_else(|| AppError::not_found("Space not found"))?;
    if !node.is_folder() || drive.kind == SpaceKind::Personal || drive.disabled || drive.link_makers == LinkMakers::Nobody {
        return Err(AppError::forbidden(kind.unpublishable()));
    }
    let (trashed,): (bool,) = sqlx::query_as(
        "WITH RECURSIVE up(id, parent_id, trash) AS (
           SELECT id, parent_id, trashed_at IS NOT NULL FROM nodes WHERE id = ?
           UNION ALL SELECT n.id, n.parent_id, n.trashed_at IS NOT NULL FROM nodes n JOIN up ON n.id = up.parent_id
         ) SELECT COALESCE(MAX(trash), 1) FROM up",
    )
    .bind(&node.id)
    .fetch_one(&mut *conn)
    .await?;
    if trashed {
        return Err(AppError::not_found("Folder not found"));
    }
    if !user.is_admin() && (!managers || !user.can_share || !tree::live_role_on(conn, user, &node).await?.is_some_and(|r| r >= Role::Manager)) {
        return Err(AppError::forbidden(kind.managers_only()));
    }
    if kind == Publication::FtpReceiveFolder {
        may_receive(user, &node)?;
    }
    Ok(node)
}

/// Whether what is published from `node` by `publisher` may take files: the space takes changes, and the publisher's
/// account may add files (an FTP receive folder, a writable S3 key)
pub fn may_receive(publisher: &User, node: &Node) -> AppResult<()> {
    if node.space_read_only {
        return Err(tree::read_only_space());
    }
    tree::allows(publisher, Role::Manager, Need::Add)
}
