//! Browsing what a storage location holds, read-only: one folder level (or prefix) at a time, page by page. Content
//! of the content store is shown with the space and file it belongs to; a folder space's folder with its space.
//!
//! Administrators don't see the files in other people's personal spaces (as in Space management): the space is
//! named, its files aren't, its folder can't be opened here, and its content can't be downloaded. That holds for any
//! spelling of the folder's path (letter case, compared by the folder's identity on the disk), and while the space
//! is being moved: the folder it is copied into, the content copied so far, and the old folder until it is cleaned up.

use std::{
    collections::{HashMap, HashSet},
    path::{Path as FsPath, PathBuf},
};

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::Response,
};
use serde::{Deserialize, Serialize};
use sqlx::SqliteConnection;
use tokio_util::io::ReaderStream;

use super::location;
use crate::{
    auth::Admin,
    error::{AppError, AppResult},
    locations::describe,
    state::AppState,
    storage::{self, Entry, EntryKind},
    tree,
};

/// Items per page unless asked otherwise, and at most
const PAGE: usize = 200;
const MAX_PAGE: usize = 1000;

#[derive(Deserialize)]
pub struct BrowseQuery {
    /// The folder ("" for the top), parts with '/' between them
    #[serde(default)]
    path: String,
    /// Continue after the item with this name (the `next` of the page before)
    after: Option<String>,
    limit: Option<usize>,
}

/// A space, as the tools show it
#[derive(Debug, Clone, Serialize)]
pub struct SpaceRef {
    pub id: String,
    pub name: String,
    /// personal, company or team
    pub kind: String,
    /// The owner's username (personal spaces)
    pub owner: String,
    /// Someone else's personal space: its files aren't shown
    pub private: bool,
}

/// What uses a content
#[derive(Debug, Serialize)]
pub struct Usage {
    /// used (by a file), version (an earlier version of a file), trash (a file in the trash), replica (a copy kept as a
    /// replica of content kept elsewhere), pending (waiting to be deleted) or unused
    pub status: &'static str,
    pub space: Option<SpaceRef>,
    /// The file's path in its space; None in someone else's personal space
    pub file: Option<String>,
    /// How many files and versions use it
    pub uses: i64,
}

#[derive(Debug, Serialize)]
pub struct Item {
    #[serde(flatten)]
    pub entry: Entry,
    /// Its path in the location
    pub path: String,
    /// content (a folder of the content store, or content), internal (ThirtyFile's own files) or space (a folder space's folder)
    pub role: Option<&'static str>,
    pub space: Option<SpaceRef>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Serialize)]
pub struct Page {
    pub path: String,
    pub items: Vec<Item>,
    /// Pass as `after` for the next page; None on the last one
    pub next: Option<String>,
    /// The folder space this folder is in, if any
    pub space: Option<SpaceRef>,
}

pub async fn browse(State(st): State<AppState>, Admin(me): Admin, Path(id): Path<String>, Query(q): Query<BrowseQuery>) -> AppResult<Json<Page>> {
    let loc = location(&st, &id).await?;
    let path = q.path.trim_matches('/').to_string();
    storage::key_parts(&path).map_err(|_| AppError::bad_request("Invalid path"))?;
    if in_copies(&path) {
        return Err(copies_closed());
    }
    let mut c = st.db.acquire().await?;
    let folder = loc.folder(&st);
    let spaces = match &folder {
        Some(folder) => folder_spaces(&mut c, me.id, &id, folder).await?,
        None => Vec::new(),
    };
    if let Some(folder) = &folder
        && let Some(s) = private_space_at(&spaces, folder, &path).await
    {
        return Err(private_space(&s));
    }
    let within = space_at(&spaces, &path);
    let backend = st.storage(&id)?;
    let mut entries = backend.list_dir(&path).await.map_err(|e| read_error(&e))?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let start = q.after.as_deref().map_or(0, |after| entries.partition_point(|e| e.name.as_str() <= after));
    let limit = q.limit.unwrap_or(PAGE).clamp(1, MAX_PAGE);
    let end = (start + limit).min(entries.len());
    let next = (end < entries.len()).then(|| entries[end - 1].name.clone());

    let content_dir = backend.content_dir();
    let mut items: Vec<Item> = entries
        .drain(start..end)
        .map(|entry| {
            let key = storage::join_key(&[&path, &entry.name]);
            let space = (entry.kind == EntryKind::Folder).then(|| spaces.iter().find(|f| f.path.to_lowercase() == key.to_lowercase()).map(|f| f.space.clone())).flatten();
            let role = if space.is_some() {
                Some("space")
            } else if entry.name.starts_with(".thirtyfile") {
                Some("internal")
            } else if match entry.kind {
                EntryKind::Folder => is_content_folder(&key, content_dir),
                _ => storage::content_hash(&key, content_dir).is_some(),
            } {
                Some("content")
            } else {
                None
            };
            Item { entry, path: key, role, space, usage: None }
        })
        .collect();

    let hashes: Vec<String> = items.iter().filter_map(|i| storage::content_hash(&i.path, content_dir).filter(|_| i.entry.kind == EntryKind::File)).map(str::to_string).collect();
    let mut usage = usage_of(&mut c, me.id, &id, &hashes).await?;
    for item in &mut items {
        if item.entry.kind == EntryKind::File
            && let Some(hash) = storage::content_hash(&item.path, content_dir)
        {
            item.usage = usage.remove(hash);
        }
    }
    Ok(Json(Page { path, items, next, space: within }))
}

/// Whether a path is in the folder of copies ThirtyFile keeps on a location (backups/). Their lists of files name the
/// files of every space copied, personal spaces' too: nothing in it is listed or served here.
fn in_copies(key: &str) -> bool {
    key.split('/').next().is_some_and(|first| first.eq_ignore_ascii_case(crate::backups::layout::ROOT))
}

fn copies_closed() -> AppError {
    AppError::forbidden("This folder holds copies ThirtyFile keeps. They are managed in Control panel › Backups.")
}

fn private_space(s: &SpaceRef) -> AppError {
    AppError::forbidden(format!("This is the personal space of {}. Administrators can't see the files in it.", s.owner))
}

fn read_error(e: &std::io::Error) -> AppError {
    match e.kind() {
        std::io::ErrorKind::NotFound => AppError::not_found("This folder isn't there"),
        std::io::ErrorKind::InvalidInput | std::io::ErrorKind::NotADirectory => AppError::bad_request("This isn't a folder"),
        // The storage service's own explanation (can't connect, keys refused…)
        _ if e.get_ref().is_some_and(|i| i.is::<storage::StorageError>()) => AppError::new(StatusCode::BAD_GATEWAY, describe(e)),
        _ => AppError::new(StatusCode::BAD_GATEWAY, format!("Couldn't read this: {e}")),
    }
}

/// A folder of the content store: `<content_dir>` itself, or `ab` and `ab/cd` below it
fn is_content_folder(key: &str, content_dir: &str) -> bool {
    let rest = if content_dir.is_empty() {
        key
    } else if key == content_dir {
        return true;
    } else {
        match key.strip_prefix(content_dir).and_then(|r| r.strip_prefix('/')) {
            Some(r) => r,
            None => return false,
        }
    };
    let parts: Vec<&str> = rest.split('/').collect();
    parts.len() <= 2 && parts.iter().all(|p| p.len() == 2 && p.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// A folder space's folder in a location's folder
struct SpaceFolder {
    /// Its path in the location's folder
    path: String,
    /// Its identity on the disk (`folder_id`), when it is there
    id: Option<FolderId>,
    space: SpaceRef,
}

/// The folder spaces on location `location` (as `drives.location_id` records it), with their folders in the location's
/// `folder`. A space being moved has more than one folder there until the move has cleaned up: the one it is being
/// copied into (`to_path`) and the one it leaves (`from_path`), which may still hold files after the move is done.
/// The space's own folder comes first.
async fn folder_spaces(c: &mut SqliteConnection, me: i64, location: &str, folder: &FsPath) -> AppResult<Vec<SpaceFolder>> {
    let rows: Vec<(String, String, String, Option<i64>, String, String)> = sqlx::query_as(
        "SELECT id, name, kind, owner_id, owner, path FROM (
           SELECT 0 AS o, d.id, d.name, d.kind, d.owner_id, COALESCE(u.username, '') AS owner, d.source_path AS path
           FROM drives d LEFT JOIN users u ON u.id = d.owner_id
           WHERE d.mode = 'folder' AND d.source_path IS NOT NULL AND d.location_id = ?1
           UNION ALL
           SELECT 1, d.id, d.name, d.kind, d.owner_id, COALESCE(u.username, ''), m.to_path
           FROM space_moves m JOIN drives d ON d.id = m.drive_id LEFT JOIN users u ON u.id = d.owner_id
           WHERE m.to_path IS NOT NULL AND m.to_location = ?1 AND m.state <> 'done'
           UNION ALL
           SELECT 1, d.id, d.name, d.kind, d.owner_id, COALESCE(u.username, ''), m.from_path
           FROM space_moves m JOIN drives d ON d.id = m.drive_id LEFT JOIN users u ON u.id = d.owner_id
           WHERE m.from_path IS NOT NULL AND m.from_location = ?1
         ) ORDER BY o",
    )
    .bind(location)
    .fetch_all(&mut *c)
    .await?;
    let mut out: Vec<SpaceFolder> = Vec::new();
    for (id, name, kind, owner_id, owner, source) in rows {
        let source = PathBuf::from(source);
        let source = std::path::absolute(&source).unwrap_or(source);
        let Ok(rel) = source.strip_prefix(folder) else { continue };
        let parts: Vec<String> = rel.components().map(|p| p.as_os_str().to_string_lossy().into_owned()).collect();
        if parts.is_empty() {
            continue;
        }
        let path = parts.join("/");
        if out.iter().any(|f| f.space.id == id && f.path == path) {
            continue;
        }
        let private = kind == "personal" && owner_id != Some(me);
        let found = source.clone();
        let fid = tokio::task::spawn_blocking(move || folder_id(&found)).await.ok().flatten();
        out.push(SpaceFolder { path, id: fid, space: SpaceRef { id, name, kind, owner, private } });
    }
    Ok(out)
}

/// A folder's identity on the disk: its device and inode, or (where the system doesn't tell them) its real path in
/// lower case, which letter case and short names don't change
#[derive(Debug, Clone, PartialEq, Eq)]
enum FolderId {
    #[cfg(unix)]
    Inode(u64, u64),
    #[cfg(not(unix))]
    Path(String),
}

/// The identity of the folder at `path` (a link is not followed: it is no folder); None when there is no folder there
fn folder_id(path: &FsPath) -> Option<FolderId> {
    let meta = std::fs::symlink_metadata(path).ok().filter(std::fs::Metadata::is_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(FolderId::Inode(meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        Some(FolderId::Path(std::fs::canonicalize(path).ok()?.to_string_lossy().to_lowercase()))
    }
}

/// Whether `key` is `path` or in it, ignoring letter case (file systems that ignore it, SMB, NTFS or macOS, open
/// `USERS/amy` for `users/amy`)
fn is_in(key: &str, path: &str) -> bool {
    let (key, path) = (key.to_lowercase(), path.to_lowercase());
    key == path || key.strip_prefix(path.as_str()).is_some_and(|r| r.starts_with('/'))
}

/// The folder space whose folder `key` is, or is in (the most specific one, the space's own folder first)
fn space_at(spaces: &[SpaceFolder], key: &str) -> Option<SpaceRef> {
    let mut best: Option<&SpaceFolder> = None;
    for f in spaces.iter().filter(|f| is_in(key, &f.path)) {
        if best.is_none_or(|b| f.path.len() > b.path.len()) {
            best = Some(f);
        }
    }
    best.map(|f| f.space.clone())
}

/// Someone else's personal space whose folder `key` (in the location's `folder`) is, or is in. Besides the path, each
/// folder on the way is compared by its identity on the disk, so another spelling of a folder's name can't get past.
async fn private_space_at(spaces: &[SpaceFolder], folder: &FsPath, key: &str) -> Option<SpaceRef> {
    if let Some(s) = space_at(spaces, key).filter(|s| s.private) {
        return Some(s);
    }
    let private: Vec<(FolderId, SpaceRef)> = spaces.iter().filter(|f| f.space.private).filter_map(|f| Some((f.id.clone()?, f.space.clone()))).collect();
    if private.is_empty() || key.is_empty() {
        return None;
    }
    let (folder, key) = (folder.to_path_buf(), key.to_string());
    tokio::task::spawn_blocking(move || {
        let root = crate::beneath::Pinned::root(&folder).ok()?;
        let parts: Vec<&str> = key.split('/').collect();
        for n in 1..=parts.len() {
            let Ok(at) = root.join(&parts[..n].join("/")) else { break };
            let Some(id) = folder_id(at.as_path()) else { break };
            if let Some((_, s)) = private.iter().find(|(p, _)| *p == id) {
                return Some(s.clone());
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

/// Who uses each content at `location`, for one page of content (a few queries for the page, not one per item)
pub async fn usage_of(c: &mut SqliteConnection, me: i64, location: &str, hashes: &[String]) -> AppResult<HashMap<String, Usage>> {
    let mut out = HashMap::new();
    if hashes.is_empty() {
        return Ok(out);
    }
    let list = serde_json::to_string(hashes).unwrap();
    let here: HashSet<String> = sqlx::query_as::<_, (String,)>("SELECT hash FROM blobs WHERE location_id = ?2 AND hash IN (SELECT value FROM json_each(?1))")
        .bind(&list)
        .bind(location)
        .fetch_all(&mut *c)
        .await?
        .into_iter()
        .map(|r| r.0)
        .collect();
    let pending: HashSet<String> =
        sqlx::query_as::<_, (String,)>("SELECT hash FROM pending_blob_deletes WHERE location_id = ?2 AND hash IN (SELECT value FROM json_each(?1))")
            .bind(&list)
            .bind(location)
            .fetch_all(&mut *c)
            .await?
            .into_iter()
            .map(|r| r.0)
            .collect();
    // Replicas kept here of content kept elsewhere (replicas/)
    let replicas: HashSet<String> =
        sqlx::query_as::<_, (String,)>("SELECT hash FROM replica_copies WHERE location_id = ?2 AND hash IN (SELECT value FROM json_each(?1))")
            .bind(&list)
            .bind(location)
            .fetch_all(&mut *c)
            .await?
            .into_iter()
            .map(|r| r.0)
            .collect();
    // One file per content: one outside the trash first, then one the administrator may see; and how many use it
    let private = "EXISTS (SELECT 1 FROM drives d WHERE d.id = n.drive_id AND d.kind = 'personal' AND d.owner_id IS NOT ?2)";
    let files: Vec<(String, String, Option<String>, bool, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT blob_hash, id, drive_id, trashed, uses FROM (
           SELECT n.blob_hash, n.id, n.drive_id, n.trashed_at IS NOT NULL AS trashed, COUNT(*) OVER (PARTITION BY n.blob_hash) AS uses,
                  ROW_NUMBER() OVER (PARTITION BY n.blob_hash ORDER BY n.trashed_at IS NOT NULL, {private}, n.id) AS rn
           FROM nodes n WHERE n.blob_hash IN (SELECT value FROM json_each(?1))
         ) WHERE rn = 1"
    )))
    .bind(&list)
    .bind(me)
    .fetch_all(&mut *c)
    .await?;
    let versions: Vec<(String, String, Option<String>, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT blob_hash, node_id, drive_id, uses FROM (
           SELECT v.blob_hash, v.node_id, n.drive_id, COUNT(*) OVER (PARTITION BY v.blob_hash) AS uses,
                  ROW_NUMBER() OVER (PARTITION BY v.blob_hash ORDER BY {private}, v.id) AS rn
           FROM node_versions v JOIN nodes n ON n.id = v.node_id WHERE v.blob_hash IN (SELECT value FROM json_each(?1))
         ) WHERE rn = 1"
    )))
    .bind(&list)
    .bind(me)
    .fetch_all(&mut *c)
    .await?;

    let drive_ids: Vec<&str> = files.iter().filter_map(|f| f.2.as_deref()).chain(versions.iter().filter_map(|v| v.2.as_deref())).collect();
    let drives = spaces_by_id(c, me, &drive_ids).await?;
    let files: HashMap<&str, _> = files.iter().map(|f| (f.0.as_str(), f)).collect();
    let versions: HashMap<&str, _> = versions.iter().map(|v| (v.0.as_str(), v)).collect();

    /// What a content's usage is made of, before the file's path is known
    struct Chosen<'a> {
        status: &'static str,
        node: Option<&'a str>,
        drive: Option<&'a str>,
        uses: i64,
    }
    // The files to name (not those in someone else's personal space)
    let mut shown: Vec<String> = Vec::new();
    let mut chosen: HashMap<&str, Chosen> = HashMap::new();
    for hash in hashes {
        let file = files.get(hash.as_str());
        let version = versions.get(hash.as_str());
        let uses = file.map_or(0, |f| f.4) + version.map_or(0, |v| v.3);
        let (status, node, drive) = if here.contains(hash) {
            match (file, version) {
                (Some(f), _) if !f.3 => ("used", Some(f.1.as_str()), f.2.as_deref()),
                (Some(f), _) => ("trash", Some(f.1.as_str()), f.2.as_deref()),
                (None, Some(v)) => ("version", Some(v.1.as_str()), v.2.as_deref()),
                (None, None) => ("unused", None, None),
            }
        } else if replicas.contains(hash) {
            ("replica", None, None)
        } else if pending.contains(hash) {
            ("pending", None, None)
        } else {
            // A copy nothing here points to: the content is elsewhere now, or was never recorded
            ("unused", None, None)
        };
        let visible = drive.and_then(|d| drives.get(d)).is_some_and(|s| !s.private);
        if let (Some(n), true) = (node, visible) {
            shown.push(n.to_string());
        }
        let uses = if matches!(status, "unused" | "pending" | "replica") { 0 } else { uses };
        chosen.insert(hash.as_str(), Chosen { status, node, drive, uses });
    }
    let paths = tree::paths_of(c, &shown).await?;
    for (hash, Chosen { status, node, drive, uses }) in chosen {
        let space = drive.and_then(|d| drives.get(d)).cloned();
        let file = node.and_then(|n| paths.get(n)).map(|crumbs| crumbs.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join("/"));
        out.insert(hash.to_string(), Usage { status, space, file, uses });
    }
    Ok(out)
}

async fn spaces_by_id(c: &mut SqliteConnection, me: i64, ids: &[&str]) -> AppResult<HashMap<String, SpaceRef>> {
    let rows: Vec<(String, String, String, Option<i64>, String)> = sqlx::query_as(
        "SELECT d.id, d.name, d.kind, d.owner_id, COALESCE(u.username, '')
         FROM drives d LEFT JOIN users u ON u.id = d.owner_id WHERE d.id IN (SELECT value FROM json_each(?))",
    )
    .bind(serde_json::to_string(ids).unwrap())
    .fetch_all(&mut *c)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, name, kind, owner_id, owner)| {
            let private = kind == "personal" && owner_id != Some(me);
            (id.clone(), SpaceRef { id, name, kind, owner, private })
        })
        .collect())
}

/// Whether a content is used in someone else's personal space (by a file or an earlier version), or was copied for
/// a move of one that hasn't cleaned up yet (into a content store, it is the space's only once the move switches)
async fn in_private_space(c: &mut SqliteConnection, me: i64, hash: &str) -> AppResult<bool> {
    let (found,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM nodes n JOIN drives d ON d.id = n.drive_id
                        WHERE n.blob_hash = ?1 AND d.kind = 'personal' AND d.owner_id IS NOT ?2)
             OR EXISTS (SELECT 1 FROM node_versions v JOIN nodes n ON n.id = v.node_id JOIN drives d ON d.id = n.drive_id
                        WHERE v.blob_hash = ?1 AND d.kind = 'personal' AND d.owner_id IS NOT ?2)
             OR EXISTS (SELECT 1 FROM space_move_items i JOIN space_moves m ON m.id = i.move_id JOIN drives d ON d.id = m.drive_id
                        WHERE i.hash = ?1 AND d.kind = 'personal' AND d.owner_id IS NOT ?2)",
    )
    .bind(hash)
    .bind(me)
    .fetch_one(&mut *c)
    .await?;
    Ok(found)
}

/// A key where a content is stored (`<content_dir>/ab/cd/<hash>`: true) or where a copy of it is being written
/// (`<hash>.part-…` or another name after the hash: false), with the hash; `key` in lower case
fn content_or_copy<'a>(key: &'a str, content_dir: &str) -> Option<(&'a str, bool)> {
    if let Some(hash) = storage::content_hash(key, content_dir) {
        return Some((hash, true));
    }
    let (dir, name) = key.rsplit_once('/')?;
    let hash = name.get(..64)?;
    let rest = &name[64..];
    let hex = hash.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'));
    let expected = if content_dir.is_empty() { format!("{}/{}", &hash[0..2], &hash[2..4]) } else { format!("{content_dir}/{}/{}", &hash[0..2], &hash[2..4]) };
    (hex && rest.starts_with('.') && dir == expected).then_some((hash, false))
}

#[derive(Deserialize)]
pub struct DownloadQuery {
    path: String,
}

/// Downloads one file of a location as it is stored (outside the API's request timeout: it streams)
pub async fn download(State(st): State<AppState>, Admin(me): Admin, Path(id): Path<String>, Query(q): Query<DownloadQuery>) -> AppResult<Response> {
    let loc = location(&st, &id).await?;
    let key = q.path.trim_matches('/');
    if key.is_empty() || storage::key_parts(key).is_err() {
        return Err(AppError::bad_request("Invalid path"));
    }
    if in_copies(key) {
        return Err(copies_closed());
    }
    let mut c = st.db.acquire().await?;
    if let Some(folder) = loc.folder(&st)
        && let Some(s) = private_space_at(&folder_spaces(&mut c, me.id, &id, &folder).await?, &folder, key).await
    {
        return Err(private_space(&s));
    }
    let backend = st.storage(&id)?;
    // Where letter case is ignored, `AB/CD/<HASH>` opens the content `ab/cd/<hash>`
    let lower = key.to_ascii_lowercase();
    if let Some(hash) = storage::content_hash(&lower, backend.content_dir())
        && in_private_space(&mut c, me.id, hash).await?
    {
        return Err(AppError::forbidden("This content belongs to someone's personal space. Administrators can't download it."));
    }
    // Content waiting to be deleted no longer records whose it was (it may have been in a personal space), and a copy
    // still being written beside where content goes (`<hash>.part-…`) is someone's upload: neither is served
    if let Some((hash, whole)) = content_or_copy(&lower, backend.content_dir()) {
        let (pending,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM pending_blob_deletes WHERE hash = ? AND location_id = ?)")
            .bind(hash)
            .bind(&id)
            .fetch_one(&mut *c)
            .await?;
        if pending || !whole {
            return Err(AppError::forbidden("This content is being written or deleted, so it can't be downloaded"));
        }
    }
    drop(c);
    let entry = backend.stat(key).await.map_err(|e| read_error(&e))?.ok_or_else(|| AppError::not_found("File not found"))?;
    if entry.kind != EntryKind::File {
        return Err(AppError::bad_request("Only files can be downloaded"));
    }
    let reader = backend.open_at(key, 0, entry.size).await.map_err(|e| read_error(&e))?;
    let mut res = Response::new(Body::from_stream(ReaderStream::with_capacity(reader, 256 * 1024)));
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    h.insert(header::CONTENT_LENGTH, entry.size.into());
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox; default-src 'none'"));
    if let Ok(v) = HeaderValue::from_str(&crate::util::content_disposition("attachment", &entry.name)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    async fn page(env: &testutil::TestEnv, admin: &crate::auth::User, path: &str, after: Option<&str>, limit: usize) -> AppResult<Page> {
        let q = BrowseQuery { path: path.into(), after: after.map(str::to_string), limit: Some(limit) };
        browse(State(env.st.clone()), Admin(admin.clone()), Path("local".into()), Query(q)).await.map(|j| j.0)
    }

    fn names(p: &Page) -> Vec<&str> {
        p.items.iter().map(|i| i.entry.name.as_str()).collect()
    }

    #[tokio::test]
    async fn pages_follow_each_other_and_paths_cant_step_out() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let dir = env.dir.join("blobs").join("Some folder");
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..7 {
            std::fs::write(dir.join(format!("f{i}.txt")), b"x").unwrap();
        }
        let first = page(&env, &admin, "Some folder", None, 3).await.unwrap();
        assert_eq!(names(&first), ["f0.txt", "f1.txt", "f2.txt"]);
        assert_eq!(first.next.as_deref(), Some("f2.txt"));
        let second = page(&env, &admin, "/Some folder/", first.next.as_deref(), 3).await.unwrap();
        assert_eq!(names(&second), ["f3.txt", "f4.txt", "f5.txt"]);
        let last = page(&env, &admin, "Some folder", second.next.as_deref(), 3).await.unwrap();
        assert_eq!(names(&last), ["f6.txt"]);
        assert_eq!(last.next, None);
        assert_eq!(last.items[0].path, "Some folder/f6.txt");
        assert_eq!((last.items[0].entry.kind, last.items[0].entry.size), (EntryKind::File, 1));

        for bad in ["..", "../tmp", "Some folder/../..", "a//b", "./x"] {
            let e = page(&env, &admin, bad, None, 10).await.unwrap_err();
            assert_eq!(e.status, StatusCode::BAD_REQUEST, "{bad}");
        }
        let q = DownloadQuery { path: "../drive.db".into() };
        let e = download(State(env.st.clone()), Admin(admin.clone()), Path("local".into()), Query(q)).await.unwrap_err();
        assert_eq!(e.status, StatusCode::BAD_REQUEST);
        assert_eq!(page(&env, &admin, "Missing", None, 10).await.unwrap_err().status, StatusCode::NOT_FOUND);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn links_are_listed_but_never_followed() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let outside = env.dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), b"secret").unwrap();
        std::os::unix::fs::symlink(&outside, env.dir.join("blobs").join("away")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), env.dir.join("blobs").join("away.txt")).unwrap();
        let top = page(&env, &admin, "", None, 100).await.unwrap();
        let away = top.items.iter().find(|i| i.entry.name == "away").unwrap();
        assert_eq!(away.entry.kind, EntryKind::Link);
        assert!(page(&env, &admin, "away", None, 10).await.is_err(), "a link isn't opened as a folder");
        let q = DownloadQuery { path: "away.txt".into() };
        assert!(download(State(env.st.clone()), Admin(admin.clone()), Path("local".into()), Query(q)).await.is_err());
        let q = DownloadQuery { path: "away/secret.txt".into() };
        assert!(download(State(env.st.clone()), Admin(admin.clone()), Path("local".into()), Query(q)).await.is_err());
    }

    #[tokio::test]
    async fn content_shows_its_file_but_not_in_someone_elses_personal_space() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let amy = env.user("amy", false).await;
        let docs = env.folder(&admin, admin.root(), "Docs").await;
        env.stored_file(&admin, &docs, "report.txt", b"admin's report").await;
        env.stored_file(&amy, amy.root(), "diary.txt", b"amy's diary").await;
        // Content nothing records, and content only an earlier version uses
        let local = env.st.storage("local").unwrap();
        let orphan = crate::util::sha256_hex(b"orphan");
        let tmp = env.dir.join("tmp").join("orphan");
        std::fs::write(&tmp, b"orphan").unwrap();
        local.put_file(&orphan, &tmp).await.unwrap();

        let usage_for = |content: &[u8]| {
            let hash = crate::util::sha256_hex(content);
            let (env, admin) = (&env, &admin);
            async move {
                let dir = format!("{}/{}", &hash[0..2], &hash[2..4]);
                let p = page(env, admin, &dir, None, 100).await.unwrap();
                let item = p.items.into_iter().find(|i| i.entry.name == hash).unwrap();
                assert_eq!(item.role, Some("content"));
                item.usage.unwrap()
            }
        };
        let mine = usage_for(b"admin's report").await;
        assert_eq!((mine.status, mine.file.as_deref(), mine.uses), ("used", Some("Docs/report.txt"), 1));
        assert!(!mine.space.unwrap().private);
        let hers = usage_for(b"amy's diary").await;
        assert_eq!(hers.status, "used");
        let space = hers.space.unwrap();
        assert!(space.private && space.owner == "amy" && space.kind == "personal");
        assert_eq!(hers.file, None, "no file names from someone else's personal space");
        assert_eq!(usage_for(b"orphan").await.status, "unused");

        // Downloads: the administrator's own content and unused content, not Amy's
        let key = |content: &[u8]| {
            let h = crate::util::sha256_hex(content);
            format!("{}/{}/{h}", &h[0..2], &h[2..4])
        };
        let get = |path: String| download(State(env.st.clone()), Admin(admin.clone()), Path("local".into()), Query(DownloadQuery { path }));
        let res = get(key(b"admin's report")).await.unwrap();
        assert_eq!(axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().as_ref(), b"admin's report");
        assert!(get(key(b"orphan")).await.is_ok());
        assert_eq!(get(key(b"amy's diary")).await.unwrap_err().status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn content_waiting_to_be_deleted_and_copies_being_written_stay_closed() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let local = env.st.storage("local").unwrap();
        let put = |content: &'static [u8]| {
            let (local, tmp) = (local.clone(), env.dir.join("tmp").join(crate::util::new_id()));
            async move {
                let h = crate::util::sha256_hex(content);
                std::fs::write(&tmp, content).unwrap();
                local.put_file(&h, &tmp).await.unwrap();
                h
            }
        };
        // Deleted from someone's files (whose, nothing records any more), and waiting for the background deletion
        let gone = put(b"deleted diary").await;
        sqlx::query("INSERT INTO pending_blob_deletes (hash, location_id, created_at) VALUES (?, 'local', 0)").bind(&gone).execute(&env.st.db).await.unwrap();
        assert_eq!(get(&env, &admin, &format!("{}/{}/{gone}", &gone[0..2], &gone[2..4])).await.unwrap_err().status, StatusCode::FORBIDDEN);
        // A copy still being written next to where the content goes
        let h = crate::util::sha256_hex(b"being uploaded");
        let part = format!("{}/{}/{h}.part-{}", &h[0..2], &h[2..4], crate::util::new_id());
        testutil::write_old(&env.dir.join("blobs").join(&part), b"being uploaded");
        assert_eq!(get(&env, &admin, &part).await.unwrap_err().status, StatusCode::FORBIDDEN);
        assert_eq!(get(&env, &admin, &part.to_uppercase()).await.unwrap_err().status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn trash_versions_and_pending_deletions_are_told_apart() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let trashed = env.stored_file(&admin, admin.root(), "old.txt", b"trashed").await;
        sqlx::query("UPDATE nodes SET trashed_at = 1, trash_id = 't', trash_root = 1 WHERE id = ?").bind(&trashed).execute(&env.st.db).await.unwrap();
        let file = env.stored_file(&admin, admin.root(), "notes.txt", b"version one").await;
        // The file got new content: the old one is an earlier version
        let v1 = crate::util::sha256_hex(b"version one");
        sqlx::query("INSERT INTO node_versions (id, node_id, blob_hash, size, modified_at, created_at) VALUES ('v1', ?, ?, 11, 0, 0)")
            .bind(&file)
            .bind(&v1)
            .execute(&env.st.db)
            .await
            .unwrap();
        sqlx::query("UPDATE nodes SET blob_hash = NULL WHERE id = ?").bind(&file).execute(&env.st.db).await.unwrap();
        let gone = crate::util::sha256_hex(b"to delete");
        sqlx::query("INSERT INTO pending_blob_deletes (hash, location_id, created_at) VALUES (?, 'local', 0)").bind(&gone).execute(&env.st.db).await.unwrap();

        let mut c = env.st.db.acquire().await.unwrap();
        let hashes = vec![crate::util::sha256_hex(b"trashed"), v1.clone(), gone.clone()];
        let u = usage_of(&mut c, admin.id, "local", &hashes).await.unwrap();
        assert_eq!(u[&hashes[0]].status, "trash");
        assert_eq!((u[&v1].status, u[&v1].file.as_deref()), ("version", Some("notes.txt")));
        assert_eq!(u[&gone].status, "pending");
        // Elsewhere the same content isn't used by anything
        let u = usage_of(&mut c, admin.id, "second", &hashes).await.unwrap();
        assert!(u.values().all(|u| u.status == "unused"));
    }

    #[tokio::test]
    async fn folder_spaces_are_named_and_others_personal_ones_stay_closed() {
        let env = testutil::folders_env().await;
        let admin = env.admin().await;
        let _amy = env.user("amy", false).await;
        std::fs::write(env.dir.join("blobs").join("users").join("amy").join("diary.txt"), b"private").unwrap();
        let users = page(&env, &admin, "users", None, 100).await.unwrap();
        let hers = users.items.iter().find(|i| i.entry.name == "amy").unwrap();
        assert_eq!(hers.role, Some("space"));
        let space = hers.space.as_ref().unwrap();
        assert!(space.private && space.owner == "amy");
        let mine = users.items.iter().find(|i| i.entry.name == admin.username).unwrap();
        assert!(!mine.space.as_ref().unwrap().private);
        assert_eq!(page(&env, &admin, "users/amy", None, 100).await.unwrap_err().status, StatusCode::FORBIDDEN);
        let q = DownloadQuery { path: "users/amy/diary.txt".into() };
        let e = download(State(env.st.clone()), Admin(admin.clone()), Path("local".into()), Query(q)).await.unwrap_err();
        assert_eq!(e.status, StatusCode::FORBIDDEN);
        // The administrator's own space opens, and says which space it is
        let own = page(&env, &admin, &format!("users/{}", admin.username), None, 100).await.unwrap();
        assert_eq!(own.space.unwrap().owner, admin.username);
    }

    fn get(env: &testutil::TestEnv, admin: &crate::auth::User, path: &str) -> impl std::future::Future<Output = AppResult<Response>> {
        download(State(env.st.clone()), Admin(admin.clone()), Path("local".into()), Query(DownloadQuery { path: path.into() }))
    }

    #[tokio::test]
    async fn another_spelling_of_a_personal_spaces_folder_stays_closed() {
        let env = testutil::folders_env().await;
        let admin = env.admin().await;
        let _amy = env.user("amy", false).await;
        std::fs::write(env.dir.join("blobs/users/amy/diary.txt"), b"private").unwrap();
        // File systems that ignore letter case (NTFS, SMB, macOS) open these as users/amy
        for path in ["USERS/amy", "users/AMY", "Users/Amy"] {
            assert_eq!(page(&env, &admin, path, None, 100).await.unwrap_err().status, StatusCode::FORBIDDEN, "{path}");
            assert_eq!(get(&env, &admin, &format!("{path}/diary.txt")).await.unwrap_err().status, StatusCode::FORBIDDEN, "{path}");
        }
    }

    #[tokio::test]
    async fn content_keys_in_upper_case_are_checked_too() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let amy = env.user("amy", false).await;
        env.stored_file(&amy, amy.root(), "diary.txt", b"amy's diary").await;
        let h = crate::util::sha256_hex(b"amy's diary");
        for key in [format!("{}/{}/{}", &h[0..2], &h[2..4], h.to_uppercase()), format!("{}/{}/{h}", h[0..2].to_uppercase(), h[2..4].to_uppercase())] {
            assert_eq!(get(&env, &admin, &key).await.unwrap_err().status, StatusCode::FORBIDDEN, "{key}");
        }
    }

    #[tokio::test]
    async fn what_a_move_copies_for_someone_elses_personal_space_stays_closed() {
        let env = testutil::folders_env().await;
        let admin = env.admin().await;
        let amy = env.user("amy", false).await;
        let drive = env.drive_of(amy.root()).await;
        let blobs = env.dir.join("blobs");
        // A move into another folder of the location, not switched yet: the new folder is hers already
        let new = blobs.join("users/amy-moving");
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("diary.txt"), b"private").unwrap();
        sqlx::query(
            "INSERT INTO space_moves (id, drive_id, space_name, space_kind, from_location, from_mode, from_path, to_location, to_mode, to_path, state, created_at)
             VALUES ('m1', ?, 'My files', 'personal', 'local', 'folder', ?, 'local', 'folder', ?, 'running', 0)",
        )
        .bind(&drive)
        .bind(blobs.join("users/amy").to_string_lossy())
        .bind(new.to_string_lossy())
        .execute(&env.st.db)
        .await
        .unwrap();
        assert_eq!(page(&env, &admin, "users/amy-moving", None, 100).await.unwrap_err().status, StatusCode::FORBIDDEN);
        assert_eq!(get(&env, &admin, "users/amy-moving/diary.txt").await.unwrap_err().status, StatusCode::FORBIDDEN);
        let users = page(&env, &admin, "users", None, 100).await.unwrap();
        let moving = users.items.iter().find(|i| i.entry.name == "amy-moving").unwrap();
        assert!(moving.space.as_ref().is_some_and(|s| s.private && s.owner == "amy"));

        // A move into a content store: what it copied is hers before the switch
        let copied = crate::util::sha256_hex(b"copied for amy");
        let tmp = env.dir.join("tmp").join("copied");
        std::fs::write(&tmp, b"copied for amy").unwrap();
        env.st.storage("local").unwrap().put_file(&copied, &tmp).await.unwrap();
        sqlx::query("INSERT INTO space_move_items (move_id, item_id, kind, hash, from_location, size) VALUES ('m1', 'n1', 'file', ?, 'local', 14)")
            .bind(&copied)
            .execute(&env.st.db)
            .await
            .unwrap();
        let key = format!("{}/{}/{copied}", &copied[0..2], &copied[2..4]);
        assert_eq!(get(&env, &admin, &key).await.unwrap_err().status, StatusCode::FORBIDDEN);
    }
}
