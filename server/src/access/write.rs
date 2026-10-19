//! Writing files: where a file goes, receiving its content within the limits, and storing it as a new file or as new
//! content of the file of that name

use std::path::{Path, PathBuf};

use axum::{body::Bytes, http::StatusCode};
use futures_util::Stream;
use tokio::io::AsyncWriteExt;

use crate::{
    auth::User,
    content,
    error::{AppError, AppResult},
    logs,
    paths::child_named,
    state::AppState,
    tree::{self, Need, Node},
    util::new_id,
};

/// Largest file when no upload size limit is set (sizes are summed as i64 for quotas)
pub const MAX_FILE: u64 = 1 << 50;

/// Refuses a file larger than the upload size limit
pub fn check_size(st: &AppState, size: u64) -> AppResult<()> {
    if size > MAX_FILE || (st.max_upload > 0 && size > st.max_upload) {
        return Err(AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "The file exceeds the upload size limit"));
    }
    Ok(())
}

/// Where a file is about to be written: the folder, its name there, and the file it replaces (if any)
pub struct Upload {
    pub parent: Node,
    pub name: String,
    pub existing: Option<Node>,
}

impl Upload {
    /// Whether a file of `size` bytes may be stored here: within the upload size limit, and the room its space has
    /// left, where the file it replaces counts as freed. Room on the disk it is received on is checked as it arrives
    /// (`DiskRoom`)
    pub async fn fits(&self, st: &AppState, size: u64) -> AppResult<()> {
        check_size(st, size)?;
        let replaced = self.existing.as_ref().map_or(0, |n| n.size);
        tree::check_quota(&mut *st.db.acquire().await?, self.parent.drive(), size as i64 - replaced).await
    }
}

/// Where a file `name` in `parent` goes, looked at before anything is received: the parent needs Add, so contributors
/// can add files to folders other people made. A folder of that name is refused; replacing a file requires Write on
/// that item. Both checks are repeated when the received content is committed.
pub async fn upload_to(st: &AppState, user: &User, parent: &Node, name: String) -> AppResult<Upload> {
    let parent = tree::folder_for(&mut *st.db.acquire().await?, user, &parent.id, Need::Add).await?;
    let mut c = st.db.acquire().await?;
    let mut existing = child_named(&mut c, &parent.id, &name).await?;
    // Only when the index and the folder on the server disagree about this name is the folder looked at again:
    // copying thousands of files into one folder mustn't re-read it for each
    if parent.in_folder_space() {
        // (on a blocking thread, within a few seconds: when the disk doesn't answer, the index is taken as it is)
        let (p, n) = (parent.clone(), name.clone());
        let look = move || p.fs_pinned().and_then(|p| p.join(&n)).is_ok_and(|p| std::fs::symlink_metadata(p.as_path()).is_ok());
        let on_disk = crate::util::blocking_within(format!("check of {name} in {}", parent.id), std::time::Duration::from_secs(5), look).await;
        if on_disk.is_some_and(|on_disk| on_disk != existing.is_some()) {
            drop(c);
            crate::folders::sync_folder(st, &parent).await;
            c = st.db.acquire().await?;
            existing = child_named(&mut c, &parent.id, &name).await?;
        }
    }
    if let Some(n) = &existing {
        if n.is_folder() {
            return Err(AppError::new(StatusCode::METHOD_NOT_ALLOWED, "A folder has this name"));
        }
        tree::node_for(&mut c, user, &n.id, Need::Write).await?;
    }
    Ok(Upload { parent, name, existing })
}

/// Receives a file's content and stores it where `upload_to` said; true when the file was created. `size`: its length
/// when the protocol says it beforehand, checked against the limits before anything is received; without it,
/// receiving stops as soon as more arrived than the space can take.
pub async fn write<S, E>(st: &AppState, user: &User, up: Upload, body: S, size: Option<u64>) -> AppResult<bool>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
{
    let replaced = up.existing.as_ref().map_or(0, |n| n.size);
    let mut room = None;
    if let Some(len) = size {
        up.fits(st, len).await?;
        crate::util::DiskRoom::check(&st.tmp_dir(), len).await?;
    } else if let Some((left, drive)) = tree::room_left(&mut *st.db.acquire().await?, up.parent.drive(), None).await? {
        room = Some((u64::try_from(left + replaced).unwrap_or(0), drive));
    }
    let tmp = st.tmp_dir().join(format!("dav-{}", new_id()));
    // Content for the content store is hashed as it arrives, so storing it doesn't read the file again
    let (size, hash) = match receive(st, body, &tmp, !up.parent.in_folder_space(), room).await {
        Ok(received) => received,
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e);
        }
    };
    // Stored in a task of its own, so a dropped connection can't stop it halfway
    tokio::spawn(store(st.clone(), user.clone(), up.parent, up.name, tmp, size, hash)).await.map_err(AppError::internal)?
}

/// Writes the content to a file, within the upload size limit and the `room` the space has left; returns its size,
/// and with `hash` its SHA-256
pub async fn receive<S, E>(st: &AppState, mut body: S, path: &Path, hash: bool, room: Option<(u64, tree::Drive)>) -> AppResult<(u64, Option<String>)>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
{
    use sha2::Digest;
    // Received in the data folder first, whatever space it is for
    let mut disk = crate::util::DiskRoom::check(&st.tmp_dir(), 0).await?;
    let mut file = tokio::fs::File::create(path).await?;
    let mut size = 0u64;
    let mut hasher = hash.then(sha2::Sha256::new);
    while let Some(chunk) = crate::http_body::next_chunk(&mut body).await? {
        size += chunk.len() as u64;
        check_size(st, size)?;
        if let Some((left, drive)) = &room
            && size > *left
        {
            return Err(tree::quota_error(drive));
        }
        disk.before(chunk.len() as u64).await?;
        file.write_all(&chunk).await?;
        if let Some(h) = &mut hasher {
            h.update(&chunk);
        }
    }
    file.flush().await?;
    file.sync_data().await?;
    Ok((size, hasher.map(|h| hex::encode(h.finalize()))))
}

/// Stores a received file as `name` in `parent`, replacing a file of that name, and removes the received file; true
/// when it was created. `hash`: the content's SHA-256 when it was computed while receiving it
pub async fn store(st: AppState, user: User, parent: Node, name: String, tmp: PathBuf, size: u64, hash: Option<String>) -> AppResult<bool> {
    let result = store_file(&st, &user, &parent, &name, &tmp, size, (hash, false, None)).await;
    let _ = tokio::fs::remove_file(&tmp).await;
    result
}

/// Commits a create-only upload, checking the name again after staging and waiting for the space's turn.
pub async fn store_new(st: AppState, user: User, parent: Node, name: String, tmp: PathBuf, size: u64) -> AppResult<bool> {
    let result = store_file(&st, &user, &parent, &name, &tmp, size, (None, true, None)).await;
    let _ = tokio::fs::remove_file(&tmp).await;
    result
}

/// Stores an upload only if the destination is still inside the protocol's published folder.
pub struct ScopedWrite {
    pub root: String,
    pub mtime: Option<String>,
    /// What the write expects to find there, checked under the write lock (S3's conditional writes)
    pub expect: Expect,
}

/// What a conditional write expects at its name
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Expect {
    /// Whatever is there
    #[default]
    Any,
    /// Nothing: the write only creates (`If-None-Match: *`)
    Absent,
    /// This file, with this content (blob, size and change time): `If-Match` with its ETag
    Same { id: String, blob_hash: Option<String>, size: i64, updated_at: i64 },
}

/// The refusal of a conditional write whose condition no longer holds
fn precondition_failed() -> AppError {
    AppError::new(StatusCode::PRECONDITION_FAILED, "The object changed since its condition was checked")
}

pub async fn store_in_scope(st: AppState, user: User, parent: Node, name: String, received: content::Received, scope: ScopedWrite) -> AppResult<bool> {
    let result = store_file(&st, &user, &parent, &name, &received.path, received.size, (received.hash, false, Some(&scope))).await;
    let _ = tokio::fs::remove_file(received.path).await;
    result
}

/// Makes the received file `name` in `parent` (content.rs), or gives the file of that name its content; true when
/// it was created
async fn store_file(
    st: &AppState,
    user: &User,
    parent: &Node,
    name: &str,
    tmp: &Path,
    size: u64,
    (hash, create_only, scope): (Option<String>, bool, Option<&ScopedWrite>),
) -> AppResult<bool> {
    let staged = content::stage(st, parent, content::Received { path: tmp.to_path_buf(), size, hash, md5: None }).await?;
    let size = size as i64;
    let mut turn = staged.turn(st).await;
    let _w = st.write_lock.lock().await;
    let result = async {
        turn.ready()?;
        let mut tx = crate::db::begin_write(&st.db).await?;
        // Looked at again under the write lock: the folder or the file may have changed meanwhile
        let folder = tree::folder_for(&mut tx, user, &parent.id, Need::Add).await?;
        staged.check(&folder)?;
        if let Some(scope) = scope {
            tree::folder_for(&mut tx, user, &scope.root, Need::Add).await?;
            if !super::inside(&mut tx, &scope.root, &folder.id).await? {
                return Err(AppError::forbidden("Object is outside the bucket"));
            }
        }
        let existing = child_named(&mut tx, &folder.id, name).await?;
        match (scope.map(|s| &s.expect), &existing) {
            (Some(Expect::Absent), Some(_)) => return Err(precondition_failed()),
            (Some(Expect::Same { .. }), None) => return Err(precondition_failed()),
            (Some(Expect::Same { id, blob_hash, size, updated_at }), Some(n))
                if (&n.id, &n.blob_hash, n.size, n.updated_at) != (id, blob_hash, *size, *updated_at) =>
            {
                return Err(precondition_failed());
            }
            _ => {}
        }
        let (created, written) = match existing {
            Some(_) if create_only => return Err(AppError::conflict(format!("\"{name}\" already exists"))),
            Some(n) if n.is_folder() => return Err(AppError::new(StatusCode::METHOD_NOT_ALLOWED, "A folder has this name")),
            Some(n) => {
                if scope.is_some() && n.name != name {
                    return Err(AppError::conflict("Object keys that differ only in case conflict in this space"));
                }
                let n = tree::node_for(&mut tx, user, &n.id, Need::Write).await?;
                // The same content again (clients often save a file twice): nothing changes
                if staged.hash().is_some() && n.blob_hash.as_deref() == staged.hash() {
                    scoped_metadata(&mut tx, scope, &n.id).await?;
                    if scope.is_some() {
                        logs::record_activity(&mut tx, user, Some(&n), "edit", "").await?;
                    }
                    tx.commit().await?;
                    return Ok((false, content::Written::default()));
                }
                tree::check_quota(&mut tx, n.drive(), size - n.size).await?;
                // The content it had is kept as an earlier version
                let written = content::replace(&mut tx, st, &staged, &n, user.id).await?;
                scoped_metadata(&mut tx, scope, &n.id).await?;
                tree::touch(&mut tx, &folder.id).await?;
                logs::record_activity(&mut tx, user, Some(&n), "edit", "").await?;
                (false, written)
            }
            None => {
                tree::check_quota(&mut tx, folder.drive(), size).await?;
                let (id, written) = content::create(&mut tx, &staged, user.id, &folder, name, None).await?;
                scoped_metadata(&mut tx, scope, &id).await?;
                tree::touch(&mut tx, &folder.id).await?;
                let node = tree::get_node(&mut tx, &id).await?;
                logs::record_activity(&mut tx, user, node.as_ref().map(|n| n as _), "upload", "").await?;
                (true, written)
            }
        };
        tx.commit().await?;
        Ok((created, written))
    }
    .await;
    match result {
        Ok((created, written)) => {
            staged.finish(st, written).await;
            Ok(created)
        }
        Err(e) => {
            staged.abandon(st).await;
            Err(e)
        }
    }
}

async fn scoped_metadata(conn: &mut sqlx::SqliteConnection, scope: Option<&ScopedWrite>, id: &str) -> AppResult<()> {
    let Some(scope) = scope else {
        sqlx::query("UPDATE nodes SET s3_mtime=NULL,s3_mtime_version=NULL WHERE id=?").bind(id).execute(conn).await?;
        return Ok(());
    };
    let node = tree::get_node(conn, id).await?.ok_or_else(|| AppError::not_found("File not found"))?;
    // Inside the write lock: a disk that doesn't answer leaves the time out, as a change from elsewhere does
    let version = crate::files::Source::of(&node)?.version_within(node.drive()).await;
    let mtime = scope.mtime.as_ref().filter(|_| version.is_some());
    sqlx::query("UPDATE nodes SET s3_mtime=?,s3_mtime_version=? WHERE id=?").bind(mtime).bind(version).bind(id).execute(conn).await?;
    Ok(())
}
