//! Earlier versions of files (the node_versions table in migrations/0001_init.sql): the content a file had before it was saved over in the editor,
//! replaced by an upload, or restored to another version.
//!
//! In the content store the file's reference to its old content moves to the version, so nothing is copied. In folder
//! spaces the old file is kept, under a hard link where the disk allows one, in the space's `.thirtyfile-versions`
//! folder, which scans never index; the new content is then renamed over the file as before. How many versions each
//! file keeps, and for how many days, is a system setting; the maintenance loop removes the ones no longer kept.

use std::path::{Path, PathBuf};

use axum::{
    Json,
    extract::{Path as UrlPath, Query, State},
    http::HeaderMap,
    response::Response,
};
use serde::{Deserialize, Serialize};
use sqlx::SqliteConnection;
use tokio::io::AsyncWriteExt;

use crate::{
    auth::User,
    beneath::{Below, Pinned},
    error::{AppError, AppResult},
    files::{Blob, Source, serve_blob},
    fsops,
    logs,
    state::AppState,
    tree::{self, BlobRef, Need, Node},
    util::{new_id, now},
};

/// Folder of a folder space holding its files' earlier versions: `.thirtyfile-versions/<file id>/<version id>`
pub const VERSIONS_DIR: &str = ".thirtyfile-versions";
pub const DEFAULT_KEEP: i64 = 20;
pub const DEFAULT_DAYS: i64 = 90;
pub const MAX_KEEP: i64 = 1000;
pub const MAX_DAYS: i64 = 3650;

/// How many earlier versions each file keeps (0 = none) and for how many days after they were replaced (0 = no limit)
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub keep: i64,
    pub days: i64,
}

impl Policy {
    pub fn of(st: &AppState) -> Policy {
        let s = st.system.read().unwrap();
        Policy { keep: s.version_keep, days: s.version_days }
    }
}

/// What goes when versions are removed: content no longer used, and files in folder spaces
#[derive(Default)]
#[must_use]
pub struct Removed {
    pub blobs: Vec<BlobRef>,
    pub files: Vec<Below>,
}

impl Removed {
    /// After the transaction is committed
    pub fn finish(self, st: &AppState) {
        tree::schedule_blob_removal(st, self.blobs);
        fsops::remove_below_later(self.files);
    }
}

/// Who wrote a file's current content: whoever last gave it new content, else whoever created it
pub async fn content_author(conn: &mut SqliteConnection, node: &Node) -> AppResult<(i64, String)> {
    let row: (i64, String) = sqlx::query_as(
        "SELECT COALESCE(n.content_by, n.owner_id), COALESCE((SELECT username FROM users WHERE id = COALESCE(n.content_by, n.owner_id)), '')
         FROM nodes n WHERE n.id = ?",
    )
    .bind(&node.id)
    .fetch_one(conn)
    .await?;
    Ok(row)
}

/// Once a file of the content store has new content (`node` as it was before): its old content becomes a version (the
/// file's reference to it moves to the version), or is released when versions are off. `author` wrote the old content.
pub async fn keep_stored(conn: &mut SqliteConnection, policy: Policy, node: &Node, author: (i64, String)) -> AppResult<Removed> {
    let Some(hash) = node.blob_hash.clone() else { return Ok(Removed::default()) };
    if policy.keep <= 0 {
        return Ok(Removed { blobs: tree::release_blobs(conn, &[hash]).await?, files: Vec::new() });
    }
    let (author_id, author_name) = author;
    sqlx::query(
        "INSERT INTO node_versions (id, node_id, blob_hash, size, author_id, author_name, modified_at, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(new_id())
    .bind(&node.id)
    .bind(&hash)
    .bind(node.size)
    .bind(author_id)
    .bind(author_name)
    .bind(node.updated_at)
    .bind(now())
    .execute(&mut *conn)
    .await?;
    prune_node(conn, &node.id, policy).await
}

/// A file of a folder space kept as an earlier version on disk (`keep_on_disk`), to record in the index
#[derive(Debug)]
pub struct KeptFile {
    id: String,
    rel: String,
    size: i64,
    at: Pinned,
    /// Where it was, when it was moved there rather than linked
    moved_from: Option<Pinned>,
}

/// Before the file of a folder space at `path` is replaced (renamed over): it is kept as a version, hard-linked into
/// the space's versions folder. Where the disk has no hard links, it is moved there instead (the replacement takes its
/// name right after): copying it would take long for a large file, while the change holds the write lock. The rename
/// that follows gives the name new content, so the version is the only name left for the old file, and writing to the
/// file in place later can't change it. None when versions are off, or there is no file. A blocking disk step.
pub fn keep_on_disk(policy: Policy, node: &Node, path: &Pinned) -> std::io::Result<Option<KeptFile>> {
    if policy.keep <= 0 {
        return Ok(None);
    }
    let Some(root) = node.fs_root.as_deref() else { return Ok(None) };
    let Ok(meta) = std::fs::symlink_metadata(path.as_path()) else { return Ok(None) };
    if !meta.is_file() {
        return Ok(None);
    }
    let id = new_id();
    let at = version_file(Path::new(root), node.drive(), &node.id, &id)?;
    // A hard link never follows a symbolic link (on Linux)
    #[cfg(test)]
    let linked = if fsops::testing::hard_links(node.drive()) { std::fs::hard_link(path.as_path(), at.as_path()) } else { Err(std::io::ErrorKind::Unsupported.into()) };
    #[cfg(not(test))]
    let linked = std::fs::hard_link(path.as_path(), at.as_path());
    let moved_from = match linked {
        Ok(()) => None,
        Err(_) => {
            fsops::rename_new(path.as_path(), at.as_path())?;
            Some(path.clone())
        }
    };
    Ok(Some(KeptFile { rel: format!("{VERSIONS_DIR}/{}/{id}", node.id), id, size: meta.len() as i64, at, moved_from }))
}

impl KeptFile {
    /// The replacement couldn't take the file's place: it is put back as it was
    pub fn undo(self) {
        match &self.moved_from {
            Some(from) => {
                if let Err(e) = fsops::rename_new(self.at.as_path(), from.as_path()) {
                    tracing::error!("Couldn't put a file back after it couldn't be replaced: {e}");
                }
            }
            None => {
                let _ = std::fs::remove_file(self.at.as_path());
            }
        }
    }
}

/// Records a file `keep_on_disk` kept, as an earlier version of `node`
pub async fn record_kept(conn: &mut SqliteConnection, policy: Policy, node: &Node, kept: Option<KeptFile>) -> AppResult<Removed> {
    let Some(kept) = kept else { return Ok(Removed::default()) };
    let (author_id, author_name) = content_author(conn, node).await?;
    sqlx::query(
        "INSERT INTO node_versions (id, node_id, drive_id, fs_path, size, author_id, author_name, modified_at, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&kept.id)
    .bind(&node.id)
    .bind(node.drive())
    .bind(&kept.rel)
    .bind(kept.size)
    .bind(author_id)
    .bind(author_name)
    .bind(node.updated_at)
    .bind(now())
    .execute(&mut *conn)
    .await?;
    prune_node(conn, &node.id, policy).await
}

/// Content and files of versions just deleted (`rows` from a `DELETE … RETURNING blob_hash, drive_id, fs_path`)
async fn released(conn: &mut SqliteConnection, rows: Vec<(Option<String>, Option<String>, Option<String>)>) -> AppResult<Removed> {
    if rows.is_empty() {
        return Ok(Removed::default());
    }
    let mut files = Vec::new();
    let mut hashes = Vec::new();
    let mut roots: std::collections::HashMap<String, Option<String>> = std::collections::HashMap::new();
    for (hash, drive, rel) in rows {
        hashes.extend(hash);
        if let (Some(drive), Some(rel)) = (drive, rel) {
            if !roots.contains_key(&drive) {
                let root: Option<(String,)> =
                    sqlx::query_as("SELECT source_path FROM drives WHERE id = ? AND mode = 'folder'").bind(&drive).fetch_optional(&mut *conn).await?;
                roots.insert(drive.clone(), root.map(|r| r.0));
            }
            if let Some(b) = version_path(roots[&drive].as_deref(), &drive, Some(&rel)) {
                files.push(b);
            }
        }
    }
    Ok(Removed { blobs: tree::release_blobs(conn, &hashes).await?, files })
}

/// A folder-space version's file, only ever inside the versions folder of the space `drive`
fn version_path(root: Option<&str>, drive: &str, rel: Option<&str>) -> Option<Below> {
    let (root, rel) = (root?, rel?);
    let mut parts = rel.split('/');
    if parts.next()? != VERSIONS_DIR || rel.split('/').any(|p| p.is_empty() || p == "." || p == "..") {
        return None;
    }
    Some(Below::new(root, drive, rel))
}

/// Where a new version of the file `node_id` goes: `.thirtyfile-versions/<node id>/<id>` in the folder of the space
/// `drive`, making the folders
fn version_file(root: &Path, drive: &str, node_id: &str, id: &str) -> std::io::Result<Pinned> {
    let folder = crate::folders::open_space(root, drive, false)?.join(VERSIONS_DIR)?;
    fsops::ensure_dir(&folder)?;
    let folder = folder.join(node_id)?;
    fsops::ensure_dir(&folder)?;
    folder.join(id)
}

/// Removes a file's versions beyond the number kept
async fn prune_node(conn: &mut SqliteConnection, node_id: &str, policy: Policy) -> AppResult<Removed> {
    let rows: Vec<(Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "DELETE FROM node_versions WHERE node_id = ?1 AND id NOT IN (
           SELECT id FROM node_versions WHERE node_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT ?2
         ) RETURNING blob_hash, drive_id, fs_path",
    )
    .bind(node_id)
    .bind(policy.keep.max(0))
    .fetch_all(&mut *conn)
    .await?;
    released(conn, rows).await
}

/// Deletes the versions of files deleted for good (`ids`, a JSON array); their files in folder spaces are left to
/// `clean_folder`, as the transaction may still fail
pub async fn purge_nodes(conn: &mut SqliteConnection, ids: &str) -> AppResult<Vec<BlobRef>> {
    let rows: Vec<(Option<String>,)> =
        sqlx::query_as("DELETE FROM node_versions WHERE node_id IN (SELECT value FROM json_each(?)) RETURNING blob_hash").bind(ids).fetch_all(&mut *conn).await?;
    let hashes: Vec<String> = rows.into_iter().filter_map(|(h,)| h).collect();
    tree::release_blobs(conn, &hashes).await
}

/// The maintenance loop: removes versions no longer kept (too many, too old, or turned off since) and those of files
/// that are gone (removed from a folder space on the server, say). Returns how many went.
pub async fn prune(st: &AppState) -> AppResult<usize> {
    let policy = Policy::of(st);
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let mut rows: Vec<(Option<String>, Option<String>, Option<String>)> =
        sqlx::query_as("DELETE FROM node_versions WHERE node_id NOT IN (SELECT id FROM nodes) RETURNING blob_hash, drive_id, fs_path")
            .fetch_all(&mut *tx)
            .await?;
    if policy.days > 0 {
        rows.extend(
            sqlx::query_as::<_, (Option<String>, Option<String>, Option<String>)>(
                "DELETE FROM node_versions WHERE created_at < ? RETURNING blob_hash, drive_id, fs_path",
            )
            .bind(now() - policy.days * 86400)
            .fetch_all(&mut *tx)
            .await?,
        );
    }
    rows.extend(
        sqlx::query_as::<_, (Option<String>, Option<String>, Option<String>)>(
            "DELETE FROM node_versions WHERE id IN (
               SELECT id FROM (SELECT id, ROW_NUMBER() OVER (PARTITION BY node_id ORDER BY created_at DESC, rowid DESC) AS n FROM node_versions)
               WHERE n > ?
             ) RETURNING blob_hash, drive_id, fs_path",
        )
        .bind(policy.keep.max(0))
        .fetch_all(&mut *tx)
        .await?,
    );
    let n = rows.len();
    let removed = released(&mut tx, rows).await?;
    tx.commit().await?;
    removed.finish(st);
    Ok(n)
}

/// Removes files in a folder space's versions folder that no version refers to any more (the file was deleted for
/// good, or removing it failed earlier). Run with each scan of the space. Only what ThirtyFile named there is looked
/// at: folders named by a file's id, holding files named by a version's id.
pub async fn clean_folder(st: &AppState, drive_id: &str, root: &Path) -> AppResult<()> {
    let top = root.to_path_buf();
    let found = tokio::task::spawn_blocking(move || -> Vec<String> {
        let Ok(dir) = Pinned::root(&top).and_then(|r| r.join(VERSIONS_DIR)).and_then(|d| d.dir()) else { return Vec::new() };
        let Ok(read) = std::fs::read_dir(dir.as_path()) else { return Vec::new() };
        let mut found = Vec::new();
        for node_dir in read.flatten() {
            let Ok(node) = node_dir.file_name().into_string() else { continue };
            if !crate::util::is_new_id(&node) {
                continue;
            }
            let Ok(node_dir) = dir.join(&node) else { continue };
            let Ok(inside) = node_dir.dir() else { continue };
            let Ok(files) = std::fs::read_dir(inside.as_path()) else { continue };
            let all: Vec<String> = files.flatten().filter_map(|f| f.file_name().into_string().ok()).collect();
            if all.is_empty() {
                let _ = std::fs::remove_dir(node_dir.as_path());
            }
            let names: Vec<String> = all.into_iter().filter(|n| crate::util::is_new_id(n)).collect();
            found.extend(names.into_iter().map(|n| format!("{VERSIONS_DIR}/{node}/{n}")));
        }
        found
    })
    .await
    .map_err(AppError::internal)?;
    if found.is_empty() {
        return Ok(());
    }
    let known: std::collections::HashSet<String> =
        sqlx::query_as::<_, (String,)>("SELECT fs_path FROM node_versions WHERE drive_id = ? AND fs_path IS NOT NULL")
            .bind(drive_id)
            .fetch_all(&st.db)
            .await?
            .into_iter()
            .map(|(p,)| p)
            .collect();
    fsops::remove_below_later(found.into_iter().filter(|rel| !known.contains(rel)).map(|rel| Below::new(root, drive_id, rel)).collect());
    Ok(())
}

// ───────────── API ─────────────

#[derive(Serialize, sqlx::FromRow)]
pub struct VersionInfo {
    id: String,
    size: i64,
    author_name: String,
    /// When the file got this content
    modified_at: i64,
    /// When it was replaced
    created_at: i64,
}

/// A file the user can read, not a folder
async fn file_for(conn: &mut SqliteConnection, user: &User, id: &str, need: Need) -> AppResult<Node> {
    let node = tree::node_for(conn, user, id, need).await?;
    if node.is_folder() {
        return Err(AppError::bad_request("This isn't a file"));
    }
    Ok(node)
}

/// A file's earlier versions, newest first
pub async fn list(State(st): State<AppState>, user: User, UrlPath(id): UrlPath<String>) -> AppResult<Json<Vec<VersionInfo>>> {
    let mut c = st.db.acquire().await?;
    let node = file_for(&mut c, &user, &id, Need::Read).await?;
    let rows = sqlx::query_as(
        "SELECT id, size, author_name, modified_at, created_at FROM node_versions WHERE node_id = ? ORDER BY created_at DESC, rowid DESC",
    )
    .bind(&node.id)
    .fetch_all(&mut *c)
    .await?;
    Ok(Json(rows))
}

#[derive(sqlx::FromRow)]
struct Version {
    size: i64,
    blob_hash: Option<String>,
    location: Option<String>,
    fs_path: Option<String>,
    fs_root: Option<String>,
    drive_id: Option<String>,
}

/// Where a version's content is
async fn version_source(conn: &mut SqliteConnection, node: &Node, version: &str) -> AppResult<(Source, u64)> {
    let v: Version = sqlx::query_as(
        "SELECT v.size, v.blob_hash, (SELECT location_id FROM blobs WHERE hash = v.blob_hash) AS location, v.fs_path,
                (SELECT source_path FROM drives WHERE id = v.drive_id AND mode = 'folder') AS fs_root, v.drive_id
         FROM node_versions v WHERE v.id = ? AND v.node_id = ?",
    )
    .bind(version)
    .bind(&node.id)
    .fetch_optional(conn)
    .await?
    .ok_or_else(|| AppError::not_found("This version no longer exists"))?;
    let source = match (&v.blob_hash, version_path(v.fs_root.as_deref(), v.drive_id.as_deref().unwrap_or_default(), v.fs_path.as_deref())) {
        (Some(hash), _) => Source::Stored { hash: hash.clone(), location: v.location.unwrap_or_else(|| "local".into()) },
        (None, Some(b)) => Source::File(b.pin().map_err(|_| AppError::not_found("This version no longer exists"))?),
        (None, None) => return Err(AppError::not_found("This version no longer exists")),
    };
    Ok((source, v.size as u64))
}

#[derive(Deserialize)]
pub struct ContentQuery {
    download: Option<u8>,
}

/// A version's content, to preview or download (named like the file)
pub async fn content(
    State(st): State<AppState>,
    user: User,
    UrlPath((id, version)): UrlPath<(String, String)>,
    Query(q): Query<ContentQuery>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let mut c = st.db.acquire().await?;
    let node = file_for(&mut c, &user, &id, Need::Read).await?;
    let (source, size) = version_source(&mut c, &node, &version).await?;
    drop(c);
    serve_blob(&st, &headers, Blob { source, size, name: &node.name, mime: &node.mime }, q.download == Some(1)).await
}

/// Makes a version the file's content again; the content it had becomes a version too. Runs in a task of its own, so
/// a dropped request can't stop it halfway.
pub async fn restore(State(st): State<AppState>, user: User, UrlPath((id, version)): UrlPath<(String, String)>) -> AppResult<Json<Node>> {
    tokio::spawn(async move { restore_version(&st, &user, &id, &version).await.map(Json) }).await.map_err(AppError::internal)?
}

async fn restore_version(st: &AppState, user: &User, id: &str, version: &str) -> AppResult<Node> {
    let (node, source, size) = {
        let mut c = st.db.acquire().await?;
        let node = file_for(&mut c, user, id, Need::Write).await?;
        let (source, size) = version_source(&mut c, &node, version).await?;
        (node, source, size)
    };
    if let (Source::Stored { hash, .. }, Some(current)) = (&source, &node.blob_hash)
        && hash == current
    {
        return Ok(node);
    }
    // The version's content in a temporary file, then in like an upload that replaces the file
    let tmp = st.tmp_dir().join(new_id());
    let copied = async {
        let mut reader = source.open(st, 0, size).await?;
        let mut file = tokio::fs::File::create(&tmp).await?;
        let got = tokio::io::copy(&mut reader, &mut file).await?;
        file.flush().await?;
        file.sync_all().await?;
        if got != size {
            return Err(AppError::internal("a version was read incompletely"));
        }
        Ok::<_, AppError>(())
    }
    .await;
    if let Err(e) = copied {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    if node.in_folder_space() {
        restore_in_folder(st, user, &node, &tmp, size).await
    } else {
        restore_stored(st, user, &node, tmp).await
    }
}

const RESTORED: &str = "Restored an earlier version";

async fn restore_stored(st: &AppState, user: &User, node: &Node, tmp: PathBuf) -> AppResult<Node> {
    let staged = async {
        let (hash, size) = crate::files::hash_file(tmp.clone()).await?;
        tree::stage_blob(st, node.drive(), hash, size as i64, tmp.clone()).await
    }
    .await;
    let staged = match staged {
        Ok(s) => s,
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e);
        }
    };
    let _w = st.write_lock.lock().await;
    let result = async {
        let mut tx = crate::db::begin_write(&st.db).await?;
        let current = tree::node_for(&mut tx, user, &node.id, Need::Write).await?;
        if current.in_folder_space() || current.drive() != node.drive() {
            return Err(AppError::conflict("Something changed at the same time. Try again."));
        }
        tree::check_quota(&mut tx, current.drive(), staged.size - current.size).await?;
        let extra = tree::commit_blob(&mut tx, &staged).await?;
        let removed = tree::set_content(&mut tx, Policy::of(st), &current, &staged.hash, staged.size, user.id).await?;
        logs::record_activity(&mut tx, user, Some(&current), "edit", RESTORED).await?;
        let node = tree::get_node(&mut tx, &current.id).await?.ok_or_else(|| AppError::not_found("File not found"))?;
        tx.commit().await?;
        Ok((node, extra, removed))
    }
    .await;
    match result {
        Ok((node, extra, removed)) => {
            tree::finish_staged(st, staged, extra).await;
            removed.finish(st);
            Ok(node)
        }
        Err(e) => {
            tree::abandon_staged(st, staged).await;
            Err(e)
        }
    }
}

async fn restore_in_folder(st: &AppState, user: &User, node: &Node, tmp: &Path, size: u64) -> AppResult<Node> {
    let parent = tree::get_node(&mut *st.db.acquire().await?, node.parent_id.as_deref().unwrap_or_default()).await?;
    let Some(parent) = parent else {
        let _ = tokio::fs::remove_file(tmp).await;
        return Err(AppError::not_found("Folder not found"));
    };
    let staged = fsops::stage_upload(st, &parent, tmp, size).await?;
    let _space = fsops::lock_space(node.drive()).await;
    // Its folder answers, before the write lock is taken
    let ready = fsops::ready(st, node.drive()).await;
    let _w = st.write_lock.lock().await;
    let result = async {
        ready?;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let current = tree::node_for(&mut tx, user, &node.id, Need::Write).await?;
        if current.drive() != node.drive() || !current.in_folder_space() {
            return Err(AppError::conflict("Something changed at the same time. Try again."));
        }
        // The restored content counts against the space's size limit by how much it grows the file
        tree::check_quota(&mut tx, current.drive(), size as i64 - current.size).await?;
        let removed = fsops::replace_file(&mut tx, Policy::of(st), &staged, &current, user.id).await?;
        let now_size = tree::get_node(&mut tx, &current.id).await?.map_or(0, |n| n.size);
        tree::adjust_usage(&mut tx, current.drive(), now_size - current.size).await?;
        logs::record_activity(&mut tx, user, Some(&current), "edit", RESTORED).await?;
        let node = tree::get_node(&mut tx, &current.id).await?.ok_or_else(|| AppError::not_found("File not found"))?;
        tx.commit().await?;
        Ok((node, removed))
    }
    .await;
    match result {
        Ok((node, removed)) => {
            removed.finish(st);
            Ok(node)
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&staged).await;
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;
    use axum::{body::Bytes, http::header};

    /// A new empty file in `parent`, as the web page makes one (a zero-length upload)
    async fn new_file(env: &testutil::TestEnv, user: &User, parent: &str, name: &str) -> String {
        let b64 = |s: &str| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, s);
        let mut h = HeaderMap::new();
        h.insert("upload-length", "0".parse().unwrap());
        h.insert("upload-metadata", format!("filename {},parentId {}", b64(name), b64(parent)).parse().unwrap());
        let res = crate::upload::create(State(env.st.clone()), user.clone(), h).await.unwrap();
        res.headers()["x-node-id"].to_str().unwrap().to_string()
    }

    async fn save(env: &testutil::TestEnv, user: &User, id: &str, body: &'static [u8]) {
        let _ = crate::files::save_content(State(env.st.clone()), user.clone(), UrlPath(id.to_string()), HeaderMap::new(), Bytes::from_static(body)).await.unwrap();
    }

    async fn versions(env: &testutil::TestEnv, user: &User, id: &str) -> Vec<VersionInfo> {
        list(State(env.st.clone()), user.clone(), UrlPath(id.to_string())).await.unwrap().0
    }

    async fn body(res: Response) -> Vec<u8> {
        axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec()
    }

    async fn version_content(env: &testutil::TestEnv, user: &User, id: &str, version: &str) -> Vec<u8> {
        let q = Query(ContentQuery { download: Some(1) });
        body(content(State(env.st.clone()), user.clone(), UrlPath((id.to_string(), version.to_string())), q, HeaderMap::new()).await.unwrap()).await
    }

    async fn current(env: &testutil::TestEnv, user: &User, id: &str) -> Vec<u8> {
        let q = Query(serde_json::from_value(serde_json::json!({})).unwrap());
        body(crate::files::content(State(env.st.clone()), user.clone(), UrlPath(id.to_string()), q, HeaderMap::new()).await.unwrap()).await
    }

    async fn count(env: &testutil::TestEnv, sql: &str) -> i64 {
        let (n,): (i64,) = sqlx::query_as(sqlx::AssertSqlSafe(sql)).fetch_one(&env.st.db).await.unwrap();
        n
    }

    fn set_policy(env: &testutil::TestEnv, keep: i64, days: i64) {
        let mut s = env.st.system.write().unwrap();
        s.version_keep = keep;
        s.version_days = days;
    }

    #[tokio::test]
    async fn saving_keeps_the_earlier_content_and_restoring_brings_it_back() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let folder = env.folder(&amy, amy.root(), "Shared").await;
        env.grant(&folder, &ben, "editor").await;
        let id = new_file(&env, &amy, &folder, "notes.txt").await;
        save(&env, &amy, &id, b"one").await;
        save(&env, &ben, &id, b"two!").await;

        // Newest first, each with who wrote it; the empty file it started as is a version too
        let list = versions(&env, &amy, &id).await;
        assert_eq!(list.iter().map(|v| (v.size, v.author_name.as_str())).collect::<Vec<_>>(), [(3, "amy"), (0, "amy")]);
        assert_eq!(version_content(&env, &ben, &id, &list[0].id).await, b"one");

        // Restoring makes the current content a version too
        let Json(node) = restore(State(env.st.clone()), ben.clone(), UrlPath((id.clone(), list[0].id.clone()))).await.unwrap();
        assert_eq!(node.size, 3);
        assert_eq!(current(&env, &amy, &id).await, b"one");
        let list = versions(&env, &amy, &id).await;
        assert_eq!(list.len(), 3);
        assert_eq!((list[0].size, list[0].author_name.as_str()), (4, "ben"));
        assert_eq!(version_content(&env, &amy, &id, &list[0].id).await, b"two!");

        // Viewers see versions but can't restore them; others can't see them at all
        let cat = env.user("cat", true).await;
        assert!(list_err(&env, &cat, &id).await);
        env.grant(&folder, &cat, "viewer").await;
        assert_eq!(versions(&env, &cat, &id).await.len(), 3);
        let err = restore(State(env.st.clone()), cat.clone(), UrlPath((id.clone(), list[0].id.clone()))).await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::FORBIDDEN);
        let res = content(State(env.st.clone()), cat.clone(), UrlPath((id.clone(), list[0].id.clone())), Query(ContentQuery { download: None }), HeaderMap::new()).await.unwrap();
        assert_eq!(res.headers()[header::CONTENT_LENGTH], "4");
    }

    async fn list_err(env: &testutil::TestEnv, user: &User, id: &str) -> bool {
        list(State(env.st.clone()), user.clone(), UrlPath(id.to_string())).await.is_err()
    }

    #[tokio::test]
    async fn only_the_versions_the_settings_keep_stay_and_their_content_goes_with_them() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let id = new_file(&env, &amy, amy.root(), "a.txt").await;
        set_policy(&env, 2, 90);
        for body in [b"1".as_slice(), b"22", b"333", b"4444"] {
            let body: &'static [u8] = Box::leak(body.to_vec().into_boxed_slice());
            save(&env, &amy, &id, body).await;
        }
        assert_eq!(versions(&env, &amy, &id).await.iter().map(|v| v.size).collect::<Vec<_>>(), [3, 2]);
        // The current content and the two versions kept
        assert_eq!(count(&env, "SELECT COUNT(*) FROM blobs").await, 3);

        // Too old: removed by the maintenance loop
        sqlx::query("UPDATE node_versions SET created_at = created_at - 91 * 86400 WHERE size = 2").execute(&env.st.db).await.unwrap();
        assert_eq!(prune(&env.st).await.unwrap(), 1);
        assert_eq!(count(&env, "SELECT COUNT(*) FROM blobs").await, 2);

        // Turned off: nothing new is kept, and the next run removes the rest
        set_policy(&env, 0, 90);
        save(&env, &amy, &id, b"55555").await;
        assert_eq!(versions(&env, &amy, &id).await.len(), 1);
        assert_eq!(prune(&env.st).await.unwrap(), 1);
        assert_eq!(count(&env, "SELECT COUNT(*) FROM blobs").await, 1);
        assert_eq!(count(&env, "SELECT COUNT(*) FROM node_versions").await, 0);
    }

    #[tokio::test]
    async fn deleting_a_file_for_good_deletes_its_versions() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let id = new_file(&env, &amy, amy.root(), "a.txt").await;
        save(&env, &amy, &id, b"one").await;
        save(&env, &amy, &id, b"two").await;
        let req = |ids: &[&str]| Json(serde_json::from_value(serde_json::json!({ "ids": ids })).unwrap());
        let _ = crate::nodes::trash(State(env.st.clone()), amy.clone(), req(&[&id])).await.unwrap();
        assert_eq!(count(&env, "SELECT COUNT(*) FROM node_versions").await, 2, "the trash keeps them");
        let _ = crate::nodes::delete_forever(State(env.st.clone()), amy.clone(), req(&[&id])).await.unwrap();
        assert_eq!(count(&env, "SELECT COUNT(*) FROM node_versions").await, 0);
        assert_eq!(count(&env, "SELECT COUNT(*) FROM blobs").await, 0);
    }

    #[tokio::test]
    async fn folder_spaces_keep_versions_in_a_hidden_folder() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        testutil::write_old(&space.dir.join("notes.txt"), b"one");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (id, _) = env.node_at(&space.drive, "notes.txt").await.unwrap();
        save(&env, &admin, &id, b"two").await;
        assert_eq!(std::fs::read(space.dir.join("notes.txt")).unwrap(), b"two");
        let list = versions(&env, &admin, &id).await;
        assert_eq!(list.len(), 1);
        let kept = space.dir.join(VERSIONS_DIR).join(&id).join(&list[0].id);
        assert_eq!(std::fs::read(&kept).unwrap(), b"one");
        assert_eq!(version_content(&env, &admin, &id, &list[0].id).await, b"one");
        let r = crate::folders::scan(&env.st, &space.drive).await.unwrap();
        assert_eq!((r.added, r.changed, r.removed), (0, 0, 0), "the versions folder isn't indexed: {r:?}");

        // Restored in place; the content it had is kept too
        let _ = restore(State(env.st.clone()), admin.clone(), UrlPath((id.clone(), list[0].id.clone()))).await.unwrap();
        assert_eq!(std::fs::read(space.dir.join("notes.txt")).unwrap(), b"one");
        assert_eq!(env.node_at(&space.drive, "notes.txt").await, Some((id.clone(), 3)));
        assert_eq!(versions(&env, &admin, &id).await.len(), 2);

        // Deleted for good: the versions go, and the next scan removes their files
        let req = |ids: &[&str]| Json(serde_json::from_value(serde_json::json!({ "ids": ids })).unwrap());
        let _ = crate::nodes::trash(State(env.st.clone()), admin.clone(), req(&[&id])).await.unwrap();
        let _ = crate::nodes::delete_forever(State(env.st.clone()), admin.clone(), req(&[&id])).await.unwrap();
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        for _ in 0..100 {
            if !kept.exists() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("the version's file stayed");
    }
}
