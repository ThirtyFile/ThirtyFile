//! Where a received file becomes a file in a folder: the only place that happens, for both kinds of space. Uploads,
//! WebDAV, saving from the editor, restoring an earlier version and making a ZIP file all go through here.
//!
//! - A space that keeps its files in the content store: the content is stored on the space's storage location first,
//!   named by its SHA-256 (`stage`, without the write lock: S3 may take a while); the file records a reference to it
//!   in the write transaction (`create`, `replace`); after the commit, content no longer used goes (`Staged::finish`).
//! - A folder space: the file is written into the space's folder under a name scans ignore (`stage`), then renamed
//!   into place and indexed in the write transaction (`create`, `replace`), while the change holds the space's lock
//!   (`Staged::turn`), so a scan never sees half of it.
//!
//! Either way, the change is made in the space the content was staged for, kept the same way (`Staged::check`); a
//! new file counts in its space's usage, and the content a file had is kept as an earlier version.

use std::path::PathBuf;

use sqlx::SqliteConnection;
use tokio::sync::OwnedMutexGuard;

use crate::{
    beneath::Pinned,
    error::{AppError, AppResult},
    fsops,
    state::AppState,
    tree::{self, BlobRef, Node},
    util::{guess_mime, new_id, now},
    versions,
};

/// A file received into a temporary file of the data folder: its size, and its SHA-256 when it was computed while it
/// arrived
pub struct Received {
    pub path: PathBuf,
    pub size: u64,
    pub hash: Option<String>,
}

/// Received content put where its space keeps content, ready to become a file
pub struct Staged {
    drive: String,
    kind: Kind,
}

enum Kind {
    /// In the content store, on the space's storage location
    Store(tree::StagedBlob),
    /// In the space's folder, under a name scans ignore
    Folder(Pinned),
}

/// What a change wrote that is to be finished after the commit: a redundant copy of the content, and what earlier
/// versions no longer kept leave behind
#[derive(Default)]
#[must_use]
pub struct Written {
    extra: Option<BlobRef>,
    removed: versions::Removed,
}

/// Puts received content where the space of `into` (a folder or file of it) keeps content, outside the write lock.
/// The temporary file is taken over when this succeeds; when it fails, it is left where it is.
pub async fn stage(st: &AppState, into: &Node, received: Received) -> AppResult<Staged> {
    let kind = if into.in_folder_space() {
        Kind::Folder(fsops::stage_upload(st, into, &received.path, received.size).await?)
    } else {
        let hash = match received.hash {
            Some(hash) => hash,
            None => {
                let (hash, hashed) = crate::files::hash_file(received.path.clone()).await?;
                if hashed != received.size {
                    return Err(AppError::bad_request("File size mismatch"));
                }
                hash
            }
        };
        Kind::Store(tree::stage_blob(st, into.drive(), hash, received.size as i64, received.path).await?)
    };
    Ok(Staged { drive: into.drive().to_string(), kind })
}

/// What a change to a space holds while it takes the write lock and writes: for a folder space, the space's lock
/// (scans wait), taken once its folder was found to answer
pub struct Turn {
    _space: Option<OwnedMutexGuard<()>>,
    ready: AppResult<()>,
}

impl Turn {
    /// Whether the space's folder answered (checked before the write lock was taken)
    pub fn ready(&mut self) -> AppResult<()> {
        std::mem::replace(&mut self.ready, Ok(()))
    }
}

impl Staged {
    /// Takes the space's turn, before the write lock
    pub async fn turn(&self, st: &AppState) -> Turn {
        match self.kind {
            Kind::Store(_) => Turn { _space: None, ready: Ok(()) },
            Kind::Folder(_) => {
                let space = fsops::lock_space(&self.drive).await;
                let ready = fsops::ready(st, &self.drive).await;
                Turn { _space: Some(space), ready }
            }
        }
    }

    /// The content's SHA-256, for content of the content store
    pub fn hash(&self) -> Option<&str> {
        match &self.kind {
            Kind::Store(b) => Some(&b.hash),
            Kind::Folder(_) => None,
        }
    }

    /// Checks, in the transaction, that `target` (the folder or file the content goes to, as it is now) is in the
    /// space the content was staged for, kept the same way and writable: the folder may have been moved to another
    /// space meanwhile, or the space made a folder space or read-only
    pub fn check(&self, target: &Node) -> AppResult<()> {
        let folder = matches!(self.kind, Kind::Folder(_));
        if target.drive() != self.drive || target.in_folder_space() != folder || target.space_read_only {
            return Err(AppError::conflict("Something changed at the same time. Try again."));
        }
        Ok(())
    }

    /// Whether `file` is on disk as it was indexed: a file of a folder space may have been changed there meanwhile
    /// (a file of the content store can't)
    pub async fn unchanged(&self, conn: &mut SqliteConnection, file: &Node) -> AppResult<bool> {
        match self.kind {
            Kind::Store(_) => Ok(true),
            Kind::Folder(_) => fsops::unchanged_on_disk(conn, file).await,
        }
    }

    /// After the commit: the temporary file goes, and content no longer used
    pub async fn finish(self, st: &AppState, written: Written) {
        if let Kind::Store(b) = self.kind {
            tree::finish_staged(st, b, written.extra).await;
        }
        written.removed.finish(st);
    }

    /// When the change wasn't made: the staged content goes (content of the content store once nothing uses it)
    pub async fn abandon(self, st: &AppState) {
        match self.kind {
            Kind::Store(b) => tree::abandon_staged(st, b).await,
            // Still under its temporary name, unless it was renamed into place and only the index failed (the next
            // scan shows it then)
            Kind::Folder(p) => {
                let _ = tokio::fs::remove_file(p.as_path()).await;
            }
        }
    }
}

/// A name for a new file in `folder` that neither the folder nor (in a folder space) its folder on disk has: `name`,
/// else "name (1)", "name (2)"…
pub async fn free_name(conn: &mut SqliteConnection, folder: &Node, name: &str) -> AppResult<String> {
    if folder.in_folder_space() { fsops::free_name(conn, folder, name, false).await } else { tree::unique_name(conn, &folder.id, name, false).await }
}

/// The staged content becomes the new file `name` in `folder` (a free name, see `free_name`), owned by `owner`, and
/// counts in the space's usage; returns the file's id. In the write transaction.
pub async fn create(conn: &mut SqliteConnection, staged: &Staged, owner: i64, folder: &Node, name: &str) -> AppResult<(String, Written)> {
    match &staged.kind {
        Kind::Store(blob) => {
            let extra = tree::commit_blob(conn, blob).await?;
            let id = new_id();
            sqlx::query(
                "INSERT INTO nodes (id, owner_id, parent_id, kind, name, blob_hash, size, mime, drive_id, created_at, updated_at)
                 SELECT ?1, ?2, ?3, 'file', ?4, ?5, ?6, ?7, drive_id, ?8, ?8 FROM nodes WHERE id = ?3",
            )
            .bind(&id)
            .bind(owner)
            .bind(&folder.id)
            .bind(name)
            .bind(&blob.hash)
            .bind(blob.size)
            .bind(guess_mime(name))
            .bind(now())
            .execute(&mut *conn)
            .await?;
            tree::adjust_usage(conn, folder.drive(), blob.size).await?;
            Ok((id, Written { extra, removed: Default::default() }))
        }
        Kind::Folder(path) => {
            let id = fsops::place_file(conn, path, owner, folder, name).await?;
            if let Some(n) = tree::get_node(conn, &id).await? {
                tree::adjust_usage(conn, n.drive(), n.size).await?;
            }
            Ok((id, Written::default()))
        }
    }
}

/// The staged content becomes the content of the file `existing`, which keeps its id (and with it its shares,
/// permissions and favourites); the content it had is kept as an earlier version, and the space's usage follows the
/// change in size. In the write transaction.
pub async fn replace(conn: &mut SqliteConnection, st: &AppState, staged: &Staged, existing: &Node, by: i64) -> AppResult<Written> {
    if existing.is_folder() {
        return Err(AppError::bad_request("This isn't a file"));
    }
    let policy = versions::Policy::of(st);
    match &staged.kind {
        Kind::Store(blob) => {
            let extra = tree::commit_blob(conn, blob).await?;
            let removed = tree::set_content(conn, policy, existing, &blob.hash, blob.size, by).await?;
            Ok(Written { extra, removed })
        }
        Kind::Folder(path) => {
            let removed = fsops::replace_file(conn, policy, path, existing, by).await?;
            let size = tree::get_node(conn, &existing.id).await?.map_or(existing.size, |n| n.size);
            tree::adjust_usage(conn, existing.drive(), size - existing.size).await?;
            Ok(Written { extra: None, removed })
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;
    use crate::testutil::{self, TestEnv};

    async fn node(env: &TestEnv, id: &str) -> Node {
        tree::get_node(&mut *env.st.db.acquire().await.unwrap(), id).await.unwrap().unwrap()
    }

    /// A file received into the data folder's tmp/
    fn received(env: &TestEnv, body: &[u8]) -> Received {
        let path = env.st.tmp_dir().join(new_id());
        std::fs::write(&path, body).unwrap();
        Received { path, size: body.len() as u64, hash: None }
    }

    async fn used(env: &TestEnv, drive: &str) -> i64 {
        sqlx::query_scalar("SELECT used_bytes FROM drives WHERE id = ?").bind(drive).fetch_one(&env.st.db).await.unwrap()
    }

    #[tokio::test]
    async fn content_goes_only_into_the_space_it_was_staged_for_kept_the_same_way() {
        let env = testutil::env().await;
        let (amy, ben) = (env.user("amy", true).await, env.user("ben", true).await);
        let space = env.folder_space("Shared").await;
        let (mine, theirs, folders) = (node(&env, amy.root()).await, node(&env, ben.root()).await, node(&env, &space.root).await);
        let conflict = |r: AppResult<()>| r.unwrap_err().status == StatusCode::CONFLICT;

        let staged = stage(&env.st, &mine, received(&env, b"stored")).await.unwrap();
        assert_eq!(staged.hash(), Some(crate::util::sha256_hex(b"stored").as_str()));
        staged.check(&mine).unwrap();
        assert!(conflict(staged.check(&theirs)), "another space");
        assert!(conflict(staged.check(&folders)), "a folder space");
        sqlx::query("UPDATE drives SET read_only = 1 WHERE id = ?").bind(mine.drive()).execute(&env.st.db).await.unwrap();
        assert!(conflict(staged.check(&node(&env, amy.root()).await)), "a read-only space");
        staged.abandon(&env.st).await;

        let staged = stage(&env.st, &folders, received(&env, b"on disk")).await.unwrap();
        assert_eq!(staged.hash(), None);
        staged.check(&folders).unwrap();
        assert!(conflict(staged.check(&theirs)), "a space of the content store");
        staged.abandon(&env.st).await;
        // Nothing is left in the space's folder
        assert!(!std::fs::read_dir(&space.dir).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".thirtyfile-upload-")));
    }

    #[tokio::test]
    async fn new_files_and_new_content_count_in_the_space_and_keep_the_earlier_content_in_both_kinds_of_space() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let space = env.folder_space("Shared").await;
        for folder in [node(&env, amy.root()).await, node(&env, &space.root).await] {
            let before = used(&env, folder.drive()).await;
            let staged = stage(&env.st, &folder, received(&env, b"hello")).await.unwrap();
            let turn = staged.turn(&env.st).await;
            let mut tx = crate::db::begin_write(&env.st.db).await.unwrap();
            let name = free_name(&mut tx, &folder, "notes.txt").await.unwrap();
            let (id, written) = create(&mut tx, &staged, amy.id, &folder, &name).await.unwrap();
            tx.commit().await.unwrap();
            drop(turn);
            staged.finish(&env.st, written).await;
            assert_eq!(used(&env, folder.drive()).await, before + 5);
            let file = node(&env, &id).await;
            assert_eq!((file.name.as_str(), file.size), ("notes.txt", 5));

            let staged = stage(&env.st, &folder, received(&env, b"hello again")).await.unwrap();
            let turn = staged.turn(&env.st).await;
            let mut tx = crate::db::begin_write(&env.st.db).await.unwrap();
            assert!(staged.unchanged(&mut tx, &file).await.unwrap());
            let written = replace(&mut tx, &env.st, &staged, &file, amy.id).await.unwrap();
            tx.commit().await.unwrap();
            drop(turn);
            staged.finish(&env.st, written).await;
            assert_eq!(used(&env, folder.drive()).await, before + 11);
            assert_eq!(node(&env, &id).await.size, 11);
            let versions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM node_versions WHERE node_id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap();
            assert_eq!(versions, 1, "the earlier content is kept");

            // A folder has no content to replace
            let staged = stage(&env.st, &folder, received(&env, b"x")).await.unwrap();
            let mut tx = crate::db::begin_write(&env.st.db).await.unwrap();
            assert!(replace(&mut tx, &env.st, &staged, &folder, amy.id).await.is_err());
            drop(tx);
            staged.abandon(&env.st).await;
        }
    }
}
