//! Snapshots: copying spaces into a set, then writing what they hold.
//!
//! A run goes in rounds, each:
//! 1. Folder spaces: the folder is scanned (changes made by other programs), then every file and earlier version that
//!    the set hasn't read as it is now (`backup_folder_files`: path, size, modification time, identity) is read once:
//!    hashed while it is copied to a temp file, checked not to have changed meanwhile, and stored in the set unless the
//!    set has that content already.
//! 2. The manifest is written from one consistent view of the database (`cutoff`). Content of content-store spaces that
//!    the set doesn't hold yet is pinned (`backup_pending`): background deletion leaves it alone until it is copied. A
//!    folder file that changed after it was read is left for the next round, which reads it again.
//! 3. The pinned content is copied from wherever it is kept, checked against its SHA-256.
//! 4. The manifest is stored on the destination, then `complete.json`; the snapshot is complete.
//!
//! Content-store content never changes, so a snapshot holds exactly what the spaces had at the cutoff. A folder file is
//! held as it was read and checked unchanged, and as the index had it at the cutoff: each file is whole and
//! unchanged, but files written by other programs at different times aren't one instant together. Items that can't be
//! read are listed, and the run fails: an incomplete snapshot is never a restore point.

use std::{collections::HashMap, io::Write as _, path::Path, sync::Arc};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{
    Set, SpaceInfo,
    layout::{self, Line},
    runner::{Ctx, Stop},
};
use crate::{
    beneath::Pinned,
    error::{AppError, AppResult},
    state::AppState,
    storage::Storage,
    util::{new_id, now},
};

/// Rounds before giving up on spaces that keep changing
const ROUNDS: usize = 10;
/// Items looked at per page
const PAGE: i64 = 200;
/// Pins written per transaction
const PIN_BATCH: usize = 500;
pub(super) const CHANGED: &str = "The file changed while it was being copied";
pub(super) const DAMAGED: &str = "The content read didn't match its SHA-256";
/// What a snapshot can promise, as its manifest says
const CONSISTENCY: &str = "Content-store spaces: as recorded at the cutoff, each file with the content it had then. Folder spaces: each file as read and checked unchanged while it was copied, as indexed at the cutoff; files changed by other programs at different times are not one instant together.";

/// What a snapshot job copies
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Params {
    pub spaces: Vec<String>,
    #[serde(default = "yes")]
    pub versions: bool,
    #[serde(default = "yes")]
    pub trash: bool,
}

fn yes() -> bool {
    true
}

pub async fn run(cx: &Ctx<'_>) -> AppResult<Stop> {
    let (st, job) = (cx.st, cx.job);
    let p: Params = serde_json::from_str(&job.params).map_err(|_| AppError::internal("a snapshot job without its spaces"))?;
    let set = super::load_set(&st.db, &job.set_id).await?;
    if set.removing {
        return Err(AppError::conflict("This copy is being deleted"));
    }
    crate::locations::probe(st, &set.dest_location)
        .await
        .map_err(|e| AppError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, format!("The destination can't be reached: {e}")))?;
    let dst = st.storage(&set.dest_location)?;
    write_set_file(st, &set, dst.as_ref()).await?;
    let snapshot = snapshot_of(cx).await?;
    cx.set_counts(0, 0, 0, 0);
    cx.flush().await?;
    for _ in 0..ROUNDS {
        if let Some(stop) = read_folder_spaces(cx, &set, &dst, &p).await? {
            return Ok(stop);
        }
        if let Some(e) = cx.failures_error() {
            return Err(e);
        }
        if let Some(stop) = cx.stop() {
            return Ok(stop);
        }
        let m = write_manifest(cx, &set, &snapshot, &p).await?;
        if m.stale > 0 {
            // Folder files changed since they were read: read again
            continue;
        }
        let (stop, vanished) = copy_pinned(cx, &set, &dst).await?;
        if let Some(stop) = stop {
            return Ok(stop);
        }
        if let Some(e) = cx.failures_error() {
            return Err(e);
        }
        if vanished > 0 {
            // Deleted after the spaces were read and before the content was kept for the snapshot: read them again
            continue;
        }
        if let Some(stop) = cx.stop() {
            return Ok(stop);
        }
        publish(cx, &set, &snapshot, dst.as_ref(), &m).await?;
        return Ok(Stop::Done);
    }
    Err(AppError::conflict("The spaces kept changing while they were copied. Try again later."))
}

/// set.json, written once when the set is first written to
async fn write_set_file(st: &AppState, set: &Set, dst: &dyn Storage) -> AppResult<()> {
    let key = layout::set_file(&set.id);
    if dst.stat(&key).await.map_err(|e| unreachable_dest(&e))?.is_some() {
        return Ok(());
    }
    let body = serde_json::to_vec_pretty(&layout::SetFile {
        format: layout::FORMAT,
        kind: set.kind.clone(),
        id: set.id.clone(),
        name: set.name.clone(),
        source: set.source_name.clone(),
        installation: crate::locations::install_id(st).await?,
        created_at: set.created_at,
    })
    .unwrap();
    layout::write_small(st, dst, &key, &body).await.map_err(|e| unreachable_dest(&e))?;
    Ok(())
}

fn unreachable_dest(e: &std::io::Error) -> AppError {
    AppError::new(axum::http::StatusCode::BAD_GATEWAY, format!("Couldn't write to the destination: {}", crate::locations::describe(e)))
}

/// The snapshot this job makes: made at its first run, the same one when it runs again
async fn snapshot_of(cx: &Ctx<'_>) -> AppResult<String> {
    let st = cx.st;
    let _w = st.write_lock.lock().await;
    let current: Option<(Option<String>,)> = sqlx::query_as("SELECT snapshot_id FROM backup_jobs WHERE id = ?").bind(&cx.job.id).fetch_optional(&st.db).await?;
    if let Some((Some(id),)) = current {
        let exists: Option<(String,)> = sqlx::query_as("SELECT state FROM backup_snapshots WHERE id = ?").bind(&id).fetch_optional(&st.db).await?;
        if exists.is_some() {
            return Ok(id);
        }
    }
    let id = new_id();
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        sqlx::query("INSERT INTO backup_snapshots (id, set_id, created_at) VALUES (?, ?, ?)").bind(&id).bind(&cx.job.set_id).bind(now()).execute(&mut *tx).await?;
        sqlx::query("UPDATE backup_jobs SET snapshot_id = ? WHERE id = ?").bind(&id).bind(&cx.job.id).execute(&mut *tx).await?;
        AppResult::Ok(())
    }
    .await;
    crate::db::settle(tx, res).await?;
    Ok(id)
}

// ───────────── Folder spaces ─────────────

/// Files of a folder space (`?2`) the set (`?1`) hasn't read as the index has them now, after the id `?3`; the trash
/// only when `?4`
const FOLDER_FILES: &str = "FROM nodes n LEFT JOIN backup_folder_files f ON f.set_id = ?1 AND f.item_id = n.id
     WHERE n.drive_id = ?2 AND n.kind = 'file' AND n.fs_path IS NOT NULL AND n.id > ?3 AND (?4 OR n.trashed_at IS NULL)
       AND (f.item_id IS NULL OR f.path IS NOT n.fs_path OR f.size IS NOT n.fs_size OR f.mtime_ns IS NOT n.fs_mtime_ns
            OR NOT EXISTS (SELECT 1 FROM backup_objects o WHERE o.set_id = ?1 AND o.hash = f.hash))";
/// Earlier versions kept in a folder space's folder, likewise
const FOLDER_VERSIONS: &str = "FROM node_versions v LEFT JOIN backup_folder_files f ON f.set_id = ?1 AND f.item_id = v.id
     WHERE v.drive_id = ?2 AND v.fs_path IS NOT NULL AND v.id > ?3
       AND (?4 OR NOT EXISTS (SELECT 1 FROM nodes n WHERE n.id = v.node_id AND n.trashed_at IS NOT NULL))
       AND (f.item_id IS NULL OR f.path IS NOT v.fs_path OR f.size IS NOT v.size
            OR NOT EXISTS (SELECT 1 FROM backup_objects o WHERE o.set_id = ?1 AND o.hash = f.hash))";

#[derive(sqlx::FromRow)]
struct FolderSpace {
    id: String,
    source_path: Option<String>,
    read_only: bool,
    moving: bool,
}

/// Reads the files of the folder spaces that changed since the set last read them
async fn read_folder_spaces(cx: &Ctx<'_>, set: &Set, dst: &Arc<dyn Storage>, p: &Params) -> AppResult<Option<Stop>> {
    let st = cx.st;
    let spaces: Vec<FolderSpace> = sqlx::query_as(
        "SELECT id, source_path, read_only, moving FROM drives WHERE mode = 'folder' AND id IN (SELECT value FROM json_each(?)) ORDER BY id",
    )
    .bind(serde_json::to_string(&p.spaces).unwrap())
    .fetch_all(&st.db)
    .await?;
    for space in spaces {
        // Changes made by other programs, so the index is what the folder holds now
        let report = crate::folders::scan(st, &space.id).await?;
        if let Some(e) = report.error {
            return Err(AppError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, e));
        }
        let not_mounted = || AppError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, crate::storage::NOT_MOUNTED);
        let path = space.source_path.clone().ok_or_else(not_mounted)?;
        let (sp, id, ro) = (path.clone(), space.id.clone(), space.read_only || space.moving);
        let root = tokio::task::spawn_blocking(move || crate::folders::open_space(Path::new(&sp), &id, ro))
            .await
            .map_err(|e| AppError::internal(e.to_string()))?
            .map_err(|_| not_mounted())?;
        for kind in ["file", "version"] {
            if kind == "version" && !p.versions {
                continue;
            }
            let from = if kind == "file" { FOLDER_FILES } else { FOLDER_VERSIONS };
            let size = if kind == "file" { "n.fs_size" } else { "v.size" };
            let (n, bytes): (i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT COUNT(*), COALESCE(SUM({size}), 0) {from}")))
                .bind(&set.id)
                .bind(&space.id)
                .bind("")
                .bind(p.trash)
                .fetch_one(&st.db)
                .await?;
            cx.add_total(n, bytes);
            let mut last = String::new();
            loop {
                let sql = if kind == "file" {
                    format!("SELECT n.id, n.fs_path, n.name {from} ORDER BY n.id LIMIT {PAGE}")
                } else {
                    format!("SELECT v.id, v.fs_path, COALESCE((SELECT name FROM nodes WHERE id = v.node_id), '') {from} ORDER BY v.id LIMIT {PAGE}")
                };
                let rows: Vec<(String, String, String)> =
                    sqlx::query_as(sqlx::AssertSqlSafe(sql)).bind(&set.id).bind(&space.id).bind(&last).bind(p.trash).fetch_all(&st.db).await?;
                let Some((id, ..)) = rows.last() else { break };
                last = id.clone();
                for (item, rel, name) in rows {
                    if let Some(stop) = cx.stop() {
                        return Ok(Some(stop));
                    }
                    let shown = if kind == "version" { format!("{name} (an earlier version)") } else { rel.clone() };
                    if let Some(stop) = read_one(cx, set, dst, &root, &space.id, &item, &rel, &shown).await? {
                        return Ok(Some(stop));
                    }
                }
            }
        }
    }
    Ok(None)
}

/// A file as it was read: its identity, size and modification time
#[derive(Debug, Clone, Copy, PartialEq)]
struct Seen {
    dev: i64,
    ino: i64,
    size: i64,
    mtime_ns: i64,
}

fn seen(meta: &std::fs::Metadata) -> Seen {
    let (dev, ino) = crate::folders::identity(meta);
    Seen { dev, ino, size: meta.len() as i64, mtime_ns: crate::folders::mtime_ns(meta) }
}

/// Reads a file of a folder into a temp file, hashing it on the way; fails when it changes while it is read
fn read_file(root: &Pinned, rel: &str, tmp: &Path) -> std::io::Result<(String, Seen)> {
    use std::io::Read;
    let file = root.join(rel)?;
    let mut src = file.open_file()?;
    let before = seen(&src.metadata()?);
    let mut out = std::fs::File::create_new(tmp)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut len = 0i64;
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n])?;
        len += n as i64;
    }
    out.sync_all()?;
    let after = seen(&std::fs::symlink_metadata(file.as_path())?);
    if after != before || len != before.size {
        return Err(std::io::Error::other(CHANGED));
    }
    Ok((hex::encode(hasher.finalize()), before))
}

/// Reads one file or version of a folder space and records it
#[allow(clippy::too_many_arguments)]
async fn read_one(cx: &Ctx<'_>, set: &Set, dst: &Arc<dyn Storage>, root: &Pinned, space: &str, item: &str, rel: &str, shown: &str) -> AppResult<Option<Stop>> {
    let st = cx.st;
    let tmp = st.tmp_dir().join(format!("backup-{}", new_id()));
    let read = cx
        .tries(
            |e: &std::io::Error| e.to_string() == CHANGED,
            || {
                let (root, rel, tmp) = (root.clone(), rel.to_string(), tmp.clone());
                async move {
                    let _ = std::fs::remove_file(&tmp);
                    tokio::task::spawn_blocking(move || read_file(&root, &rel, &tmp)).await.map_err(std::io::Error::other)?
                }
            },
        )
        .await;
    let (hash, seen) = match read {
        Ok(r) => r,
        Err(Ok(stop)) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Ok(Some(stop));
        }
        Err(Err(e)) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            // Moved or deleted since the scan: the manifest finds the index out of date, and the next round scans again
            if !matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) {
                cx.failed(space, Some(shown.to_string()), crate::fsops::disk_error(e).message);
            }
            return Ok(None);
        }
    };
    let stored = put_object(cx, set, dst, &hash, seen.size, &tmp).await;
    let _ = tokio::fs::remove_file(&tmp).await;
    match stored? {
        Ok(()) => {}
        Err(stop) => return Ok(Some(stop)),
    }
    {
        let _w = st.write_lock.lock().await;
        sqlx::query(
            "INSERT OR REPLACE INTO backup_folder_files (set_id, item_id, drive_id, path, size, mtime_ns, dev, ino, hash) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&set.id)
        .bind(item)
        .bind(space)
        .bind(rel)
        .bind(seen.size)
        .bind(seen.mtime_ns)
        .bind(seen.dev)
        .bind(seen.ino)
        .bind(&hash)
        .execute(&st.db)
        .await?;
    }
    cx.done(1, seen.size).await?;
    Ok(None)
}

/// Stores a temp file as the set's object `hash` unless the set holds it already, and records it. Err(stop) inside
/// when the job is asked to stop while it is tried again.
async fn put_object(cx: &Ctx<'_>, set: &Set, dst: &Arc<dyn Storage>, hash: &str, size: i64, tmp: &Path) -> AppResult<Result<(), Stop>> {
    let st = cx.st;
    let held: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM backup_objects WHERE set_id = ? AND hash = ?").bind(&set.id).bind(hash).fetch_optional(&st.db).await?;
    if held.is_some() {
        return Ok(Ok(()));
    }
    let key = layout::object_key(&set.id, hash);
    let put = cx
        .tries(
            |_: &std::io::Error| true,
            || async {
                dst.put_at(&key, tmp).await?;
                match dst.stat(&key).await? {
                    Some(e) if e.size == size as u64 => Ok(()),
                    _ => Err(std::io::Error::other("The content stored on the destination isn't complete")),
                }
            },
        )
        .await;
    match put {
        Ok(()) => {}
        Err(Ok(stop)) => return Ok(Err(stop)),
        Err(Err(e)) => return Err(unreachable_dest(&e)),
    }
    let _w = st.write_lock.lock().await;
    sqlx::query("INSERT OR IGNORE INTO backup_objects (set_id, hash, size, created_at) VALUES (?, ?, ?, ?)")
        .bind(&set.id)
        .bind(hash)
        .bind(size)
        .bind(now())
        .execute(&st.db)
        .await?;
    sqlx::query("DELETE FROM backup_pending WHERE job_id = ? AND hash = ?").bind(&cx.job.id).bind(hash).execute(&st.db).await?;
    Ok(Ok(()))
}

// ───────────── Manifest ─────────────

/// A manifest written on this server
pub(super) struct Manifest {
    path: std::path::PathBuf,
    sha256: String,
    size: u64,
    cutoff: i64,
    folders: i64,
    files: i64,
    versions: i64,
    logical_bytes: i64,
    spaces: Vec<SpaceInfo>,
    /// Folder files the set hasn't read as the index has them now
    stale: i64,
}

/// Writes lines to a local file, hashing them
struct Writer {
    out: tokio::io::BufWriter<tokio::fs::File>,
    hash: Sha256,
    size: u64,
}

impl Writer {
    async fn line(&mut self, line: &Line) -> std::io::Result<()> {
        let mut b = serde_json::to_vec(line).map_err(std::io::Error::other)?;
        b.push(b'\n');
        self.hash.update(&b);
        self.size += b.len() as u64;
        self.out.write_all(&b).await
    }
}

/// Content pinned for the job: written a batch at a time, apart from the view the manifest is read from
struct Pins<'a> {
    st: &'a AppState,
    job: &'a str,
    batch: Vec<(String, i64, String)>,
}

impl Pins<'_> {
    async fn add(&mut self, hash: &str, size: i64, location: &str) -> AppResult<()> {
        self.batch.push((hash.to_string(), size, location.to_string()));
        if self.batch.len() >= PIN_BATCH {
            self.flush().await?;
        }
        Ok(())
    }

    async fn flush(&mut self) -> AppResult<()> {
        if self.batch.is_empty() {
            return Ok(());
        }
        let list = serde_json::to_string(&std::mem::take(&mut self.batch)).unwrap();
        let _w = self.st.write_lock.lock().await;
        sqlx::query(
            "INSERT OR IGNORE INTO backup_pending (job_id, hash, size, location)
             SELECT ?1, json_extract(value, '$[0]'), json_extract(value, '$[1]'), json_extract(value, '$[2]') FROM json_each(?2)",
        )
        .bind(self.job)
        .bind(list)
        .execute(&self.st.db)
        .await?;
        Ok(())
    }
}

#[derive(sqlx::FromRow)]
struct SpaceRow {
    id: String,
    name: String,
    kind: String,
    owner_id: Option<i64>,
    owner: String,
    mode: String,
    quota_bytes: i64,
    root_id: String,
}

#[derive(sqlx::FromRow)]
struct FileRow {
    id: String,
    parent_id: Option<String>,
    name: String,
    blob_hash: Option<String>,
    size: i64,
    mime: String,
    updated_at: i64,
    trashed_at: Option<i64>,
    fs_path: Option<String>,
    fs_size: Option<i64>,
    fs_mtime_ns: Option<i64>,
    location: Option<String>,
    held: bool,
    f_hash: Option<String>,
    f_path: Option<String>,
    f_size: Option<i64>,
    f_mtime_ns: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct VersionRow {
    id: String,
    node_id: String,
    blob_hash: Option<String>,
    fs_path: Option<String>,
    size: i64,
    modified_at: i64,
    created_at: i64,
    author_name: String,
    parent_id: Option<String>,
    trashed_at: Option<i64>,
    location: Option<String>,
    held: bool,
    f_hash: Option<String>,
    f_path: Option<String>,
    f_size: Option<i64>,
}

/// A folder as the manifest has it: its path and when it (or a folder it is in) went to the trash
#[derive(Clone)]
struct Place {
    path: String,
    trashed: Option<i64>,
}

/// A folder as read for the manifest: id, parent, name, when it changed, when it went to the trash
type FolderRow = (String, Option<String>, String, i64, Option<i64>);

/// The path of each folder of a space from its top folder, and whether it is in the trash; folders not reached from
/// the top are left out
fn places(root: &str, folders: &[FolderRow]) -> HashMap<String, Place> {
    let by_id: HashMap<&str, &FolderRow> = folders.iter().map(|f| (f.0.as_str(), f)).collect();
    let mut out: HashMap<String, Place> = HashMap::new();
    out.insert(root.to_string(), Place { path: String::new(), trashed: None });
    for f in folders {
        // Up to a folder already placed, then back down
        let mut chain = Vec::new();
        let mut at = f.0.as_str();
        let mut ok = false;
        while chain.len() <= folders.len() {
            if out.contains_key(at) {
                ok = true;
                break;
            }
            let Some(row) = by_id.get(at) else { break };
            chain.push(*row);
            let Some(parent) = row.1.as_deref() else { break };
            at = parent;
        }
        if !ok {
            continue;
        }
        for row in chain.into_iter().rev() {
            let parent = out[row.1.as_deref().unwrap_or_default()].clone();
            let path = if parent.path.is_empty() { row.2.clone() } else { format!("{}/{}", parent.path, row.2) };
            out.insert(row.0.clone(), Place { path, trashed: row.4.or(parent.trashed) });
        }
    }
    out
}

/// Writes the manifest of the job's spaces as they are now, pinning the content the set doesn't hold yet
async fn write_manifest(cx: &Ctx<'_>, set: &Set, snapshot: &str, p: &Params) -> AppResult<Manifest> {
    let st = cx.st;
    tokio::fs::create_dir_all(layout::cache_dir(st)).await?;
    let path = layout::cache_dir(st).join(format!("{snapshot}.jsonl.partial"));
    let file = tokio::fs::File::create(&path).await?;
    let mut w = Writer { out: tokio::io::BufWriter::new(file), hash: Sha256::new(), size: 0 };
    let mut pins = Pins { st, job: &cx.job.id, batch: Vec::new() };
    let mut m = Manifest { path: path.clone(), sha256: String::new(), size: 0, cutoff: now(), folders: 0, files: 0, versions: 0, logical_bytes: 0, spaces: Vec::new(), stale: 0 };
    let mut conn = st.db.acquire().await?;
    // Read-only: one consistent view of every space for the whole manifest, while changes go on
    #[allow(clippy::disallowed_methods)]
    let mut tx = sqlx::Acquire::begin(&mut *conn).await?;
    let spaces: Vec<SpaceRow> = sqlx::query_as(
        "SELECT d.id, d.name, d.kind, d.owner_id, CASE WHEN d.kind = 'personal' THEN COALESCE(u.username, '') ELSE '' END AS owner, d.mode, d.quota_bytes,
                d.root_id
         FROM drives d LEFT JOIN users u ON u.id = d.owner_id WHERE d.id IN (SELECT value FROM json_each(?)) ORDER BY d.id",
    )
    .bind(serde_json::to_string(&p.spaces).unwrap())
    .fetch_all(&mut *tx)
    .await?;
    m.cutoff = now();
    w.line(&Line::Header {
        format: layout::FORMAT,
        app: crate::VERSION.to_string(),
        set: set.id.clone(),
        snapshot: snapshot.to_string(),
        created_at: now(),
        cutoff: m.cutoff,
        consistency: CONSISTENCY.to_string(),
    })
    .await?;
    for s in &spaces {
        w.line(&Line::Space {
            id: s.id.clone(),
            name: s.name.clone(),
            kind: s.kind.clone(),
            owner: s.owner.clone(),
            owner_id: s.owner_id.filter(|_| s.kind == "personal"),
            mode: s.mode.clone(),
            quota: s.quota_bytes,
        })
        .await?;
    }
    for s in &spaces {
        let mut info = SpaceInfo {
            id: s.id.clone(),
            name: s.name.clone(),
            kind: s.kind.clone(),
            owner: s.owner.clone(),
            owner_id: s.owner_id.filter(|_| s.kind == "personal"),
            mode: s.mode.clone(),
            files: 0,
            bytes: 0,
        };
        let folders: Vec<FolderRow> =
            sqlx::query_as("SELECT id, parent_id, name, updated_at, trashed_at FROM nodes WHERE drive_id = ? AND kind = 'folder'")
                .bind(&s.id)
                .fetch_all(&mut *tx)
                .await?;
        let placed = places(&s.root_id, &folders);
        let mut listed: Vec<(&String, &Place, i64)> = folders
            .iter()
            .filter_map(|f| placed.get(&f.0).map(|pl| (&f.0, pl, f.3)))
            .filter(|(_, pl, _)| p.trash || pl.trashed.is_none())
            .collect();
        listed.sort_by(|a, b| a.1.path.cmp(&b.1.path));
        let root_modified: Option<(i64,)> = sqlx::query_as("SELECT updated_at FROM nodes WHERE id = ?").bind(&s.root_id).fetch_optional(&mut *tx).await?;
        w.line(&Line::Folder { space: s.id.clone(), id: s.root_id.clone(), parent: None, path: String::new(), modified: root_modified.map_or(0, |r| r.0), trashed: None })
            .await?;
        let parent_of: HashMap<&str, Option<&str>> = folders.iter().map(|f| (f.0.as_str(), f.1.as_deref())).collect();
        for (id, pl, modified) in &listed {
            if **id == s.root_id {
                continue;
            }
            let parent = parent_of.get(id.as_str()).copied().flatten().map(str::to_string);
            w.line(&Line::Folder { space: s.id.clone(), id: (*id).clone(), parent, path: pl.path.clone(), modified: *modified, trashed: pl.trashed }).await?;
            m.folders += 1;
        }
        // Files, a page at a time within the same view
        let mut last = String::new();
        loop {
            let rows: Vec<FileRow> = sqlx::query_as(
                "SELECT n.id, n.parent_id, n.name, n.blob_hash, n.size, n.mime, n.updated_at, n.trashed_at, n.fs_path, n.fs_size, n.fs_mtime_ns,
                        (SELECT location_id FROM blobs WHERE hash = n.blob_hash) AS location,
                        EXISTS (SELECT 1 FROM backup_objects o WHERE o.set_id = ?1 AND o.hash = n.blob_hash) AS held,
                        f.hash AS f_hash, f.path AS f_path, f.size AS f_size, f.mtime_ns AS f_mtime_ns
                 FROM nodes n LEFT JOIN backup_folder_files f ON f.set_id = ?1 AND f.item_id = n.id
                 WHERE n.drive_id = ?2 AND n.kind = 'file' AND n.id > ?3 ORDER BY n.id LIMIT 1000",
            )
            .bind(&set.id)
            .bind(&s.id)
            .bind(&last)
            .fetch_all(&mut *tx)
            .await?;
            let Some(r) = rows.last() else { break };
            last = r.id.clone();
            for r in rows {
                let Some(parent) = r.parent_id.as_deref().and_then(|id| placed.get(id)) else { continue };
                let trashed = r.trashed_at.or(parent.trashed);
                if trashed.is_some() && !p.trash {
                    continue;
                }
                let (hash, size) = match (&r.fs_path, &r.blob_hash) {
                    (Some(fs_path), _) => {
                        let current = r.f_path.as_deref() == Some(fs_path.as_str()) && r.f_size == r.fs_size && r.f_mtime_ns == r.fs_mtime_ns;
                        match (current, r.f_hash) {
                            (true, Some(h)) => (h, r.f_size.unwrap_or(r.size)),
                            _ => {
                                m.stale += 1;
                                continue;
                            }
                        }
                    }
                    (None, Some(h)) => {
                        if !r.held {
                            pins.add(h, r.size, r.location.as_deref().unwrap_or_default()).await?;
                        }
                        (h.clone(), r.size)
                    }
                    (None, None) => continue,
                };
                let path = if parent.path.is_empty() { r.name.clone() } else { format!("{}/{}", parent.path, r.name) };
                w.line(&Line::File {
                    space: s.id.clone(),
                    id: r.id.clone(),
                    parent: r.parent_id.clone().unwrap_or_default(),
                    path,
                    hash,
                    size,
                    mime: r.mime,
                    modified: r.updated_at,
                    trashed,
                })
                .await?;
                m.files += 1;
                m.logical_bytes += size;
                info.files += 1;
                info.bytes += size;
            }
        }
        if p.versions {
            let mut last = String::new();
            loop {
                let rows: Vec<VersionRow> = sqlx::query_as(
                    "SELECT v.id, v.node_id, v.blob_hash, v.fs_path, v.size, v.modified_at, v.created_at, v.author_name, n.parent_id, n.trashed_at,
                            (SELECT location_id FROM blobs WHERE hash = v.blob_hash) AS location,
                            EXISTS (SELECT 1 FROM backup_objects o WHERE o.set_id = ?1 AND o.hash = v.blob_hash) AS held,
                            f.hash AS f_hash, f.path AS f_path, f.size AS f_size
                     FROM node_versions v JOIN nodes n ON n.id = v.node_id LEFT JOIN backup_folder_files f ON f.set_id = ?1 AND f.item_id = v.id
                     WHERE n.drive_id = ?2 AND v.id > ?3 ORDER BY v.id LIMIT 1000",
                )
                .bind(&set.id)
                .bind(&s.id)
                .bind(&last)
                .fetch_all(&mut *tx)
                .await?;
                let Some(r) = rows.last() else { break };
                last = r.id.clone();
                for r in rows {
                    let Some(parent) = r.parent_id.as_deref().and_then(|id| placed.get(id)) else { continue };
                    if r.trashed_at.or(parent.trashed).is_some() && !p.trash {
                        continue;
                    }
                    let hash = match (&r.fs_path, &r.blob_hash) {
                        (Some(fs_path), _) => match (r.f_path.as_deref() == Some(fs_path.as_str()) && r.f_size == Some(r.size), r.f_hash) {
                            (true, Some(h)) => h,
                            _ => {
                                m.stale += 1;
                                continue;
                            }
                        },
                        (None, Some(h)) => {
                            if !r.held {
                                pins.add(h, r.size, r.location.as_deref().unwrap_or_default()).await?;
                            }
                            h.clone()
                        }
                        (None, None) => continue,
                    };
                    w.line(&Line::Version {
                        space: s.id.clone(),
                        file: r.node_id,
                        id: r.id,
                        hash,
                        size: r.size,
                        modified: r.modified_at,
                        replaced: r.created_at,
                        author: r.author_name,
                    })
                    .await?;
                    m.versions += 1;
                    m.logical_bytes += r.size;
                    info.bytes += r.size;
                }
            }
        }
        // Who had access (for the record: restores don't give it again)
        let grants: Vec<(String, String, String, String)> = sqlx::query_as(
            "SELECT g.node_id, g.principal_type,
                    CASE g.principal_type WHEN 'user' THEN COALESCE((SELECT username FROM users WHERE id = g.principal_id), '')
                                          WHEN 'group' THEN COALESCE((SELECT name FROM groups WHERE id = g.principal_id), '') ELSE '' END,
                    g.role
             FROM grants g JOIN nodes n ON n.id = g.node_id WHERE n.drive_id = ? ORDER BY g.id",
        )
        .bind(&s.id)
        .fetch_all(&mut *tx)
        .await?;
        for (node, principal, name, role) in grants {
            if placed.contains_key(&node) {
                w.line(&Line::Grant { space: s.id.clone(), node, principal, name, role }).await?;
            }
        }
        m.spaces.push(info);
    }
    tx.rollback().await?;
    drop(conn);
    pins.flush().await?;
    w.out.flush().await?;
    w.out.get_mut().sync_all().await?;
    m.sha256 = hex::encode(w.hash.finalize());
    m.size = w.size;
    Ok(m)
}

// ───────────── Content of content-store spaces ─────────────

/// What became of a content to copy
enum Copied {
    Done,
    Stop(Stop),
    /// Deleted for good before it was pinned (the files using it were deleted just after the manifest read them)
    Vanished,
}

/// Copies the content pinned for the job, a page at a time in hash order; returns how many were deleted before they
/// could be kept
async fn copy_pinned(cx: &Ctx<'_>, set: &Set, dst: &Arc<dyn Storage>) -> AppResult<(Option<Stop>, i64)> {
    let st = cx.st;
    let (n, bytes): (i64, i64) = sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(size), 0) FROM backup_pending WHERE job_id = ?")
        .bind(&cx.job.id)
        .fetch_one(&st.db)
        .await?;
    cx.set_left(n, bytes);
    let mut last = String::new();
    let mut vanished = 0;
    loop {
        let rows: Vec<(String, i64, String)> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT hash, size, location FROM backup_pending WHERE job_id = ? AND hash > ? ORDER BY hash LIMIT {PAGE}")))
                .bind(&cx.job.id)
                .bind(&last)
                .fetch_all(&st.db)
                .await?;
        let Some((hash, ..)) = rows.last() else { return Ok((None, vanished)) };
        last = hash.clone();
        for (hash, size, pinned_at) in rows {
            if let Some(stop) = cx.stop() {
                return Ok((Some(stop), vanished));
            }
            match copy_content(cx, set, dst, &hash, size, &pinned_at).await? {
                Copied::Done => {}
                Copied::Stop(stop) => return Ok((Some(stop), vanished)),
                Copied::Vanished => vanished += 1,
            }
        }
    }
}

/// Copies one content of a content store into the set, from where it is kept now (or was kept when it was pinned)
async fn copy_content(cx: &Ctx<'_>, set: &Set, dst: &Arc<dyn Storage>, hash: &str, size: i64, pinned_at: &str) -> AppResult<Copied> {
    let st = cx.st;
    let current: Option<(String,)> = sqlx::query_as("SELECT location_id FROM blobs WHERE hash = ?").bind(hash).fetch_optional(&st.db).await?;
    let recorded = current.is_some();
    let mut sources: Vec<String> = current.map(|(l,)| l).into_iter().collect();
    if !pinned_at.is_empty() && !sources.iter().any(|s| s == pinned_at) {
        sources.push(pinned_at.to_string());
    }
    let tmp = st.tmp_dir().join(format!("backup-{}", new_id()));
    let mut last_err: Option<std::io::Error> = None;
    for loc in &sources {
        let src = match st.storage(loc) {
            Ok(s) => s,
            Err(e) => {
                last_err = Some(std::io::Error::other(e.message));
                continue;
            }
        };
        let fetched = cx.tries(|e: &std::io::Error| e.kind() != std::io::ErrorKind::NotFound && e.to_string() != DAMAGED, || fetch_verified(&src, hash, size, &tmp)).await;
        match fetched {
            Ok(()) => {
                let stored = put_object(cx, set, dst, hash, size, &tmp).await;
                let _ = tokio::fs::remove_file(&tmp).await;
                return match stored? {
                    Ok(()) => {
                        cx.done(1, size).await?;
                        Ok(Copied::Done)
                    }
                    Err(stop) => Ok(Copied::Stop(stop)),
                };
            }
            Err(Ok(stop)) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Ok(Copied::Stop(stop));
            }
            Err(Err(e)) => last_err = Some(e),
        }
    }
    let _ = tokio::fs::remove_file(&tmp).await;
    let e = last_err.unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "not kept anywhere"));
    if e.kind() == std::io::ErrorKind::NotFound && !recorded {
        // Nothing records it any more: its files were deleted, before the pin could keep it
        let _w = st.write_lock.lock().await;
        sqlx::query("DELETE FROM backup_pending WHERE job_id = ? AND hash = ?").bind(&cx.job.id).bind(hash).execute(&st.db).await?;
        return Ok(Copied::Vanished);
    }
    if e.kind() == std::io::ErrorKind::NotFound || e.to_string() == DAMAGED || sources.is_empty() {
        // The content itself is missing or damaged: listed (the snapshot can't be complete without it)
        let named: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT n.name, n.drive_id FROM nodes n WHERE n.blob_hash = ?1 AND n.drive_id IN (SELECT value FROM json_each(?2))
             UNION ALL SELECT n.name, n.drive_id FROM node_versions v JOIN nodes n ON n.id = v.node_id WHERE v.blob_hash = ?1 AND n.drive_id IN (SELECT value FROM json_each(?2))
             LIMIT 1",
        )
        .bind(hash)
        .bind(serde_json::from_str::<serde_json::Value>(&cx.job.params).ok().map(|p| p["spaces"].to_string()).unwrap_or_default())
        .fetch_optional(&st.db)
        .await?;
        let (name, space) = named.map_or((None, String::new()), |(n, d)| (Some(n), d.unwrap_or_default()));
        let why = if e.to_string() == DAMAGED { "The content is damaged where it is kept" } else { "The content isn't where it is kept" };
        cx.failed(&space, name, why.to_string());
        return Ok(Copied::Done);
    }
    Err(AppError::new(axum::http::StatusCode::BAD_GATEWAY, format!("Couldn't read the files to copy: {}", crate::locations::describe(&e))))
}

/// Reads one content of a content store into a temp file, checking its SHA-256 and size
pub(super) async fn fetch_verified(src: &Arc<dyn Storage>, hash: &str, size: i64, tmp: &Path) -> std::io::Result<()> {
    let _ = tokio::fs::remove_file(tmp).await;
    let mut reader = src.open(hash, 0, size as u64).await?;
    copy_checked(&mut reader, hash, size, tmp).await
}

/// Copies a reader into a temp file, checking that it is `size` bytes with the SHA-256 `hash`
pub(super) async fn copy_checked(reader: &mut crate::storage::BoxReader, hash: &str, size: i64, tmp: &Path) -> std::io::Result<()> {
    let mut file = tokio::fs::File::create(tmp).await?;
    let mut hasher = Sha256::new();
    let mut len = 0u64;
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).await?;
        len += n as u64;
    }
    file.flush().await?;
    file.sync_all().await?;
    drop(file);
    if hex::encode(hasher.finalize()) != hash || len != size as u64 {
        return Err(std::io::Error::other(DAMAGED));
    }
    Ok(())
}

// ───────────── Publishing ─────────────

/// Stores the manifest on the destination, then the completion marker; then the snapshot is complete
async fn publish(cx: &Ctx<'_>, set: &Set, snapshot: &str, dst: &dyn Storage, m: &Manifest) -> AppResult<()> {
    let st = cx.st;
    // The manifest stays on this server too (restores read it from there): the copy sent is a temp file
    let copy = st.tmp_dir().join(format!("backup-manifest-{}", new_id()));
    tokio::fs::copy(&m.path, &copy).await?;
    let key = layout::manifest_key(&set.id, snapshot);
    let put = async {
        dst.put_at(&key, &copy).await?;
        match dst.stat(&key).await? {
            Some(e) if e.size == m.size => Ok(()),
            _ => Err(std::io::Error::other("The list of files stored on the destination isn't complete")),
        }
    }
    .await;
    let _ = tokio::fs::remove_file(&copy).await;
    put.map_err(|e| unreachable_dest(&e))?;
    let completed_at = now();
    let complete = layout::Complete {
        format: layout::FORMAT,
        set: set.id.clone(),
        snapshot: snapshot.to_string(),
        manifest_sha256: m.sha256.clone(),
        manifest_size: m.size,
        cutoff: m.cutoff,
        completed_at,
        folders: m.folders,
        files: m.files,
        versions: m.versions,
        logical_bytes: m.logical_bytes,
    };
    layout::write_small(st, dst, &layout::complete_key(&set.id, snapshot), &serde_json::to_vec_pretty(&complete).unwrap())
        .await
        .map_err(|e| unreachable_dest(&e))?;
    tokio::fs::rename(&m.path, layout::cached_manifest(st, snapshot)).await?;
    let note = (m.spaces.len() < serde_json::from_str::<Params>(&cx.job.params).map_or(0, |p| p.spaces.len()))
        .then_some("Spaces deleted while they were being copied aren't in the copy");
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        sqlx::query(
            "UPDATE backup_snapshots SET state = 'complete', cutoff = ?, space_list = ?, folders = ?, files = ?, versions = ?, logical_bytes = ?,
                                         manifest_sha256 = ?, manifest_size = ?, completed_at = ?
             WHERE id = ?",
        )
        .bind(m.cutoff)
        .bind(serde_json::to_string(&m.spaces).unwrap())
        .bind(m.folders)
        .bind(m.files)
        .bind(m.versions)
        .bind(m.logical_bytes)
        .bind(&m.sha256)
        .bind(m.size as i64)
        .bind(completed_at)
        .bind(snapshot)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM backup_pending WHERE job_id = ?").bind(&cx.job.id).execute(&mut *tx).await?;
        super::runner::finish(&mut tx, cx, note).await?;
        super::log(&mut tx, cx.job, "backup_done", &cx.job.label).await?;
        AppResult::Ok(())
    }
    .await;
    crate::db::settle(tx, res).await
}
