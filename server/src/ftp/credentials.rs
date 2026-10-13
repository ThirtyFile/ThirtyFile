//! Only a receive-folder row and its generation are carried across requests. Passwords never become file actors.

use crate::{
    auth::{self, User},
    error::{AppError, AppResult},
    tree::{self, LinkMakers, Need, Node, Role, SpaceKind},
};
use serde::Serialize;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug, Clone)]
pub struct Credential {
    pub id: String,
    pub generation: i64,
    pub alive: Arc<AtomicBool>,
    pub transfer: Option<Arc<AtomicBool>>,
}

#[derive(Clone, Serialize, sqlx::FromRow)]
pub struct Folder {
    pub id: String,
    pub node_id: String,
    pub creator_id: i64,
    pub name: String,
    pub username: String,
    #[serde(skip)]
    pub password_hash: String,
    #[serde(skip)]
    pub generation: i64,
    pub enabled: bool,
    pub allow_list: bool,
    pub allow_read: bool,
    pub expires_at: Option<i64>,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

pub async fn publisher(conn: &mut sqlx::SqliteConnection, user: &User, id: &str, managers: bool) -> AppResult<Node> {
    if user.guest {
        return Err(AppError::forbidden("Guest accounts can't publish receive folders"));
    }
    let node = tree::get_node(conn, id).await?.ok_or_else(|| AppError::not_found("Folder not found"))?;
    let drive = tree::get_drive(conn, node.drive()).await?.ok_or_else(|| AppError::not_found("Space not found"))?;
    if !node.is_folder() || drive.kind == SpaceKind::Personal || drive.disabled || drive.link_makers == LinkMakers::Nobody {
        return Err(AppError::forbidden("This folder can't be published as an FTP receive folder"));
    }
    let (trashed,): (bool,) = sqlx::query_as("WITH RECURSIVE up(id,parent_id,trash) AS (SELECT id,parent_id,trashed_at IS NOT NULL FROM nodes WHERE id=? UNION ALL SELECT n.id,n.parent_id,n.trashed_at IS NOT NULL FROM nodes n JOIN up ON n.id=up.parent_id) SELECT COALESCE(MAX(trash),1) FROM up").bind(id).fetch_one(&mut *conn).await?;
    if trashed {
        return Err(AppError::not_found("Folder not found"));
    }
    if !user.is_admin() && (!managers || !user.can_share || !tree::live_role_on(conn, user, &node).await?.is_some_and(|r| r >= Role::Manager)) {
        return Err(AppError::forbidden("Only allowed space managers can publish FTP receive folders"));
    }
    if node.space_read_only {
        return Err(tree::read_only_space());
    }
    tree::allows(user, Role::Manager, Need::Add)?;
    Ok(node)
}

pub async fn actor_in_transaction(conn: &mut sqlx::SqliteConnection, actor: &User, need: Need) -> AppResult<User> {
    let credential = actor.ftp_credential.as_ref().ok_or_else(AppError::unauthorized)?;
    if !credential.alive.load(Ordering::Acquire) || credential.transfer.as_ref().is_some_and(|t| !t.load(Ordering::Acquire)) {
        return Err(AppError::unauthorized());
    }
    let row: Folder = sqlx::query_as("SELECT * FROM ftp_receive_folders WHERE id=? AND generation=? AND enabled=1 AND (expires_at IS NULL OR expires_at>?)")
        .bind(&credential.id)
        .bind(credential.generation)
        .bind(crate::util::now())
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(AppError::unauthorized)?;
    if actor.id != row.creator_id || actor.protocol_root.as_deref() != Some(&row.node_id) {
        return Err(AppError::unauthorized());
    }
    let cfg = super::config::from_connection(conn).await?;
    if !cfg.enabled {
        return Err(AppError::forbidden("FTP access is disabled"));
    }
    let sql = format!("SELECT {} FROM users u WHERE u.id=? AND u.disabled=0", auth::USER_COLS);
    let mut current: User = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(actor.id).fetch_optional(&mut *conn).await?.ok_or_else(AppError::unauthorized)?;
    publisher(conn, &current, &row.node_id, cfg.managers_publish).await?;
    if matches!(need, Need::Delete | Need::Move | Need::Share) {
        return Err(AppError::forbidden("FTP receive folders only accept uploads"));
    }
    current.protocol_root = actor.protocol_root.clone();
    current.ftp_credential = actor.ftp_credential.clone();
    current.activity_source = Some(format!("FTP receive folder {}", row.name));
    Ok(current)
}

pub async fn access(st: &crate::state::AppState, actor: &User, listing: bool, reading: bool) -> AppResult<User> {
    let mut conn = st.db.acquire().await?;
    let user = actor_in_transaction(&mut conn, actor, Need::Read).await?;
    let row: Folder = sqlx::query_as("SELECT * FROM ftp_receive_folders WHERE id=?")
        .bind(&actor.ftp_credential.as_ref().ok_or_else(AppError::unauthorized)?.id)
        .fetch_one(&mut *conn)
        .await?;
    if (listing && !row.allow_list) || (reading && !row.allow_read) {
        return Err(AppError::forbidden("This FTP receive folder is upload-only"));
    }
    Ok(user)
}
