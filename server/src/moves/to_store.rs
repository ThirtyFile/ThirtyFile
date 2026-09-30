//! A folder space to a content store (the built-in disk to S3, say). The space is read-only for people meanwhile, as
//! its folder is copied; changes made in the folder from outside (over SMB, by a scanner) are found by a scan right
//! before the switch and copied too.
//!
//! 1. Copy: each file of the folder, earlier versions (`.thirtyfile-versions`) and the trash (`.thirtyfile-trash`)
//!    included, is read once: hashed while it is copied to a temp file, then stored at the target unless the target has
//!    that content already. Each copy records the file's path, identity, size and modification time.
//! 2. Before the switch the folder is scanned, and what changed since it was copied is copied again. Then, holding the
//!    space (no scan, no change from the web meanwhile), one transaction points every file and version at its content,
//!    counts the references, names apart items whose names differ only in letter case (the content store doesn't tell
//!    them apart), and makes the space a content store space on the new location.
//! 3. The folder is removed: each file only when it is still what was copied, then the folders left empty. What stays
//!    (changed after the last scan, or never shown in ThirtyFile) is named in the move's note.

use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    path::PathBuf,
    sync::Arc,
};

use sha2::{Digest, Sha256};

use super::{Ctx, Job, Stop};
use crate::{
    beneath::Pinned,
    error::{AppError, AppResult},
    folders::MARKER,
    fsops::disk_error,
    state::AppState,
    storage::Storage,
    tree::{self, REMOVAL_GRACE},
    util::{new_id, now, numbered_name},
};

/// Files looked at per page
const PAGE: i64 = 100;
/// Scan-and-copy rounds before the switch: each copies what changed in the folder during the one before
const ROUNDS: usize = 10;
/// Copied content not recorded yet (ThirtyFile stopped in between) is deleted after this long, unless used by then
const UNRECORDED_GRACE: i64 = 24 * 3600;

/// Files of the space (the trash included) whose copy is missing or no longer what the index has; `?3`: after this id
const PENDING_FILES: &str = "FROM nodes n LEFT JOIN space_move_items i ON i.move_id = ?1 AND i.item_id = n.id
     WHERE n.drive_id = ?2 AND n.kind = 'file' AND n.id > ?3
       AND (i.item_id IS NULL OR i.size IS NOT n.fs_size OR i.src_mtime_ns IS NOT n.fs_mtime_ns OR i.path IS NOT n.fs_path)";
/// Copies of the move `?1` that found their content at the target `?2` already, and it isn't there any more (each
/// looked up by its hash, not by reading every content at the target)
pub(super) const NO_LONGER_THERE: &str = "WHERE move_id = ?1 AND uploaded = 0 AND NOT EXISTS (SELECT 1 FROM blobs b WHERE b.hash = space_move_items.hash AND b.location_id = ?2)";
/// Earlier versions kept in the folder that aren't copied yet
const PENDING_VERSIONS: &str = "FROM node_versions v LEFT JOIN space_move_items i ON i.move_id = ?1 AND i.item_id = v.id
     WHERE v.drive_id = ?2 AND v.fs_path IS NOT NULL AND v.id > ?3 AND i.item_id IS NULL";

/// The space's folder, when it is there and is the space's (its marker names it): a disk that isn't mounted is never
/// taken for an empty folder
pub fn source(job: &Job) -> AppResult<Pinned> {
    let not_mounted = || AppError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, crate::storage::NOT_MOUNTED);
    let path = job.from_path.as_deref().ok_or_else(not_mounted)?;
    let root = Pinned::root(std::path::Path::new(path)).map_err(|_| not_mounted())?;
    match crate::folders::space_marker(&root) {
        Ok(Some(id)) if id == job.drive_id => Ok(root),
        _ => Err(not_mounted()),
    }
}

pub async fn run(cx: &Ctx<'_>) -> AppResult<Stop> {
    let (st, job) = (cx.st, cx.job);
    let dst = st.storage(&job.to_location)?;
    let root = source(job)?;
    let (files_done, bytes_done): (i64, i64) = sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(size), 0) FROM space_move_items WHERE move_id = ?")
        .bind(&job.id)
        .fetch_one(&st.db)
        .await?;
    let (files, bytes) = left(st, job).await?;
    cx.set_counts(files_done, bytes_done, files_done + files, bytes_done + bytes);
    cx.flush().await?;
    if let Some(stop) = copy_left(cx, &dst, &root, false).await? {
        return Ok(stop);
    }
    for _ in 0..ROUNDS {
        // Changes made in the folder meanwhile, from outside ThirtyFile
        let report = crate::folders::scan(st, &job.drive_id).await?;
        if let Some(e) = report.error {
            return Err(AppError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, e));
        }
        let (files, bytes) = left(st, job).await?;
        cx.add_total(files, bytes);
        if let Some(stop) = copy_left(cx, &dst, &root, true).await? {
            return Ok(stop);
        }
        let failed = cx.failed_count();
        if failed > 0 {
            return Err(AppError::new(
                axum::http::StatusCode::BAD_GATEWAY,
                if failed == 1 { "1 file couldn't be copied".to_string() } else { format!("{failed} files couldn't be copied") },
            ));
        }
        if let Some(stop) = cx.stop() {
            return Ok(stop);
        }
        if switch(cx).await? {
            super::clean_up(st, job).await;
            return Ok(Stop::Done);
        }
        // Files changed within the last seconds wait for a later scan
        tokio::time::sleep(std::time::Duration::from_secs(if cfg!(test) { 0 } else { 5 })).await;
    }
    Err(AppError::conflict("The space kept changing while it was being moved. Try again later."))
}

/// Files and versions left to copy: how many, and their bytes
async fn left(st: &AppState, job: &Job) -> AppResult<(i64, i64)> {
    let (files, bytes): (i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT COUNT(*), COALESCE(SUM(n.size), 0) {PENDING_FILES}")))
        .bind(&job.id)
        .bind(&job.drive_id)
        .bind("")
        .fetch_one(&st.db)
        .await?;
    let (versions, vbytes): (i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT COUNT(*), COALESCE(SUM(v.size), 0) {PENDING_VERSIONS}")))
        .bind(&job.id)
        .bind(&job.drive_id)
        .bind("")
        .fetch_one(&st.db)
        .await?;
    Ok((files + versions, bytes + vbytes))
}

/// What is copied from the folder: a file (a node) or an earlier version
struct Item {
    id: String,
    kind: &'static str,
    path: String,
    /// A file's name, for the list of failures
    name: String,
}

/// Copies what isn't copied yet, a page at a time. `scanned`: the folder was just scanned, so a file the index has
/// and the folder doesn't is missing (before, it may have been moved, which the scan tells).
async fn copy_left(cx: &Ctx<'_>, dst: &Arc<dyn Storage>, root: &Pinned, scanned: bool) -> AppResult<Option<Stop>> {
    let (st, job) = (cx.st, cx.job);
    for kind in ["file", "version"] {
        let mut last = String::new();
        loop {
            let rows: Vec<(String, Option<String>, String)> = if kind == "file" {
                sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT n.id, n.fs_path, n.name {PENDING_FILES} ORDER BY n.id LIMIT {PAGE}")))
            } else {
                sqlx::query_as(sqlx::AssertSqlSafe(format!(
                    "SELECT v.id, v.fs_path, COALESCE((SELECT name FROM nodes WHERE id = v.node_id), '') {PENDING_VERSIONS} ORDER BY v.id LIMIT {PAGE}"
                )))
            }
            .bind(&job.id)
            .bind(&job.drive_id)
            .bind(&last)
            .fetch_all(&st.db)
            .await?;
            let Some((id, ..)) = rows.last() else { break };
            last = id.clone();
            for (id, path, name) in rows {
                if let Some(stop) = cx.stop() {
                    return Ok(Some(stop));
                }
                let Some(path) = path else { continue };
                let item = Item { id, kind, path, name };
                if let Some(stop) = copy_one(cx, dst, root, &item, scanned).await? {
                    return Ok(Some(stop));
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

/// Reads a file of the folder into a temp file, hashing it on the way; fails when it changes while it is read
fn read_file(root: &Pinned, rel: &str, tmp: &std::path::Path) -> std::io::Result<(String, Seen)> {
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

const CHANGED: &str = "The file changed while it was being copied";

/// Copies one file or version and records it
async fn copy_one(cx: &Ctx<'_>, dst: &Arc<dyn Storage>, root: &Pinned, item: &Item, scanned: bool) -> AppResult<Option<Stop>> {
    let (st, job) = (cx.st, cx.job);
    let tmp = st.tmp_dir().join(format!("move-{}", new_id()));
    let read = {
        let (root, rel, tmp) = (root.clone(), item.path.clone(), tmp.clone());
        cx.tries(
            |e: &std::io::Error| e.to_string() == CHANGED,
            || {
                let (root, rel, tmp) = (root.clone(), rel.clone(), tmp.clone());
                async move {
                    let _ = std::fs::remove_file(&tmp);
                    tokio::task::spawn_blocking(move || read_file(&root, &rel, &tmp)).await.map_err(std::io::Error::other)?
                }
            },
        )
        .await
    };
    let (hash, seen) = match read {
        Ok(r) => r,
        Err(Ok(stop)) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Ok(Some(stop));
        }
        Err(Err(e)) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            let missing = matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory);
            // Before a scan, a missing file may have been moved: the scan finds it
            if missing && !scanned && item.kind == "file" {
                return Ok(None);
            }
            let what = if item.kind == "version" { format!("{} (an earlier version)", item.name) } else { item.path.clone() };
            cx.failed(Some(what), if missing { "The file isn't in the folder".to_string() } else { disk_error(e).message });
            return Ok(None);
        }
    };
    // Held until the copy is recorded, like an upload's: a deletion of the same content at the target (still pending
    // from an earlier move away from it, say) waits, or is waited for; once recorded, the move protects it
    // (claim_for_deletion)
    let _staging = tree::stage_guard(st, &hash).await;
    let uploaded = match store(cx, dst, &hash, &tmp).await {
        Ok(Ok(u)) => u,
        Ok(Err(stop)) => return Ok(Some(stop)),
        Err(e) => return Err(e),
    };
    #[cfg(test)]
    {
        let hook = AFTER_STORE.lock().unwrap().iter().find(|(id, _)| *id == job.id).map(|(_, h)| h.clone());
        if let Some(hook) = hook {
            hook(hash.clone()).await;
        }
    }
    {
        let _w = st.write_lock.lock().await;
        sqlx::query(
            "INSERT OR REPLACE INTO space_move_items (move_id, item_id, kind, hash, size, path, src_dev, src_ino, src_mtime_ns, uploaded)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&job.id)
        .bind(&item.id)
        .bind(item.kind)
        .bind(&hash)
        .bind(seen.size)
        .bind(&item.path)
        .bind(seen.dev)
        .bind(seen.ino)
        .bind(seen.mtime_ns)
        .bind(uploaded)
        .execute(&st.db)
        .await?;
        // What was read is what the file is now (it didn't change while it was read): the index follows, rather than
        // waiting for a scan, which leaves files changed in the last seconds for later
        if item.kind == "file" {
            sqlx::query(
                "UPDATE nodes SET size = ?1, fs_size = ?1, fs_mtime_ns = ?2, fs_dev = ?3, fs_ino = ?4 WHERE id = ?5 AND fs_path = ?6
                   AND (fs_size IS NOT ?1 OR fs_mtime_ns IS NOT ?2)",
            )
            .bind(seen.size)
            .bind(seen.mtime_ns)
            .bind(seen.dev)
            .bind(seen.ino)
            .bind(&item.id)
            .bind(&item.path)
            .execute(&st.db)
            .await?;
        }
    }
    cx.done(1, seen.size).await?;
    Ok(None)
}

/// Tests: something to do after a content is stored, given its hash
#[cfg(test)]
pub type Hook = std::sync::Arc<dyn Fn(String) -> futures_util::future::BoxFuture<'static, ()> + Send + Sync>;

/// Tests: called for a move (by its id) between storing a content and recording the copy
#[cfg(test)]
pub static AFTER_STORE: std::sync::Mutex<Vec<(String, Hook)>> = std::sync::Mutex::new(Vec::new());

/// Stores a temp file at the target unless it has that content already (the temp file goes either way); returns
/// whether it was stored now. The caller holds the content's staging guard.
async fn store(cx: &Ctx<'_>, dst: &Arc<dyn Storage>, hash: &str, tmp: &std::path::Path) -> AppResult<Result<bool, Stop>> {
    let (st, job) = (cx.st, cx.job);
    let there: Option<(String,)> = sqlx::query_as("SELECT location_id FROM blobs WHERE hash = ?").bind(hash).fetch_optional(&st.db).await?;
    if there.is_some_and(|(loc,)| loc == job.to_location) {
        let _ = tokio::fs::remove_file(tmp).await;
        return Ok(Ok(false));
    }
    super::store::defer_removal(st, hash, &job.to_location, UNRECORDED_GRACE).await?;
    // The temp file is removed once stored, and kept when storing fails, for the next try
    let put = cx.tries(|_: &std::io::Error| true, || dst.put_file(hash, tmp)).await;
    let _ = tokio::fs::remove_file(tmp).await;
    match put {
        Ok(()) => Ok(Ok(true)),
        Err(Ok(stop)) => Ok(Err(stop)),
        Err(Err(e)) => Err(AppError::new(axum::http::StatusCode::BAD_GATEWAY, format!("Failed to move files: {}", crate::locations::describe(&e)))),
    }
}

/// An item of a space: (id, parent id, name, kind)
type Named = (String, Option<String>, String, String);

/// Names for the items of the space that are in the same folder with names differing only in letter case (a folder can
/// hold "A.txt" and "a.txt", the content store can't): (node id, new name)
fn case_apart(nodes: &[Named]) -> Vec<(String, String)> {
    let mut by_parent: HashMap<&str, Vec<&Named>> = HashMap::new();
    for n in nodes {
        by_parent.entry(n.1.as_deref().unwrap_or_default()).or_default().push(n);
    }
    let mut out = Vec::new();
    for (_, mut kids) in by_parent {
        kids.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
        let mut taken: HashSet<String> = HashSet::new();
        let mut clashing: Vec<&Named> = Vec::new();
        for k in &kids {
            if !taken.insert(k.2.to_lowercase()) {
                clashing.push(k);
            }
        }
        for k in clashing {
            let is_dir = k.3 == "folder";
            let name = (1..10_000).map(|n| numbered_name(&k.2, n, is_dir)).find(|c| !taken.contains(&c.to_lowercase())).unwrap_or_else(|| k.0.clone());
            taken.insert(name.to_lowercase());
            out.push((k.0.clone(), name));
        }
    }
    out
}

/// Switches the space to the content store in one transaction, holding it (no scan or change meanwhile). False when
/// something isn't copied as it is now (copied in the next round).
async fn switch(cx: &Ctx<'_>) -> AppResult<bool> {
    let (st, job) = (cx.st, cx.job);
    let _held = crate::folders::hold(&job.drive_id).await;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        for pending in [PENDING_FILES, PENDING_VERSIONS] {
            let row: Option<(i64,)> =
                sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT 1 {pending} LIMIT 1"))).bind(&job.id).bind(&job.drive_id).bind("").fetch_optional(&mut *tx).await?;
            if row.is_some() {
                return Ok(false);
            }
        }
        // Content that was at the target when it was copied, and isn't any more: copied again
        let gone = sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM space_move_items {NO_LONGER_THERE}")))
            .bind(&job.id)
            .bind(&job.to_location)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if gone > 0 {
            return Ok(false);
        }
        let now = now();
        // Items named apart where only letter case tells them apart (the trash keeps its names: it has no such rule)
        // Only the items of folders that have such names, found by the database (this holds the write lock, and the
        // space may be large)
        let nodes: Vec<Named> = sqlx::query_as(
            "SELECT id, parent_id, name, kind FROM nodes WHERE drive_id = ?1 AND trashed_at IS NULL AND parent_id IN (
               SELECT parent_id FROM nodes WHERE drive_id = ?1 AND trashed_at IS NULL GROUP BY parent_id, unicode_lower(name) HAVING COUNT(*) > 1
             )",
        )
        .bind(&job.drive_id)
        .fetch_all(&mut *tx)
        .await?;
        let renamed = case_apart(&nodes);
        for (id, _) in &renamed {
            sqlx::query("UPDATE nodes SET name = char(1) || id WHERE id = ?").bind(id).execute(&mut *tx).await?;
        }
        // The content of each file and version still in the space: a reference each, recorded at the target when it is
        // new there. Only items of the space: one moved to another space meanwhile keeps what it has there.
        sqlx::query(
            "INSERT INTO blobs (hash, size, refcount, created_at, location_id)
             SELECT i.hash, MAX(i.size), COUNT(*), ?3, ?2 FROM space_move_items i
             WHERE i.move_id = ?1 AND (EXISTS (SELECT 1 FROM nodes n WHERE n.id = i.item_id AND n.drive_id = ?4 AND i.kind = 'file')
                                       OR EXISTS (SELECT 1 FROM node_versions v WHERE v.id = i.item_id AND v.drive_id = ?4 AND i.kind = 'version'))
             GROUP BY i.hash
             ON CONFLICT (hash) DO UPDATE SET refcount = refcount + excluded.refcount",
        )
        .bind(&job.id)
        .bind(&job.to_location)
        .bind(now)
        .bind(&job.drive_id)
        .execute(&mut *tx)
        .await?;
        // Copies stored now that nothing uses at the target: the content was elsewhere already, or its file went
        sqlx::query(
            "INSERT INTO pending_blob_deletes (hash, location_id, created_at, attempts, last_error)
             SELECT DISTINCT i.hash, ?2, ?3, 0, 'deferred' FROM space_move_items i
             WHERE i.move_id = ?1 AND i.uploaded = 1 AND NOT EXISTS (SELECT 1 FROM blobs b WHERE b.hash = i.hash AND b.location_id = ?2)
             ON CONFLICT (hash, location_id) DO UPDATE SET created_at = MIN(created_at, excluded.created_at)",
        )
        .bind(&job.id)
        .bind(&job.to_location)
        .bind(now + REMOVAL_GRACE)
        .execute(&mut *tx)
        .await?;
        // The others are used now: the entries that would have removed them had the move stopped go
        sqlx::query(
            "DELETE FROM pending_blob_deletes WHERE location_id = ?2 AND last_error = 'deferred'
               AND hash IN (SELECT i.hash FROM space_move_items i JOIN blobs b ON b.hash = i.hash AND b.location_id = ?2 WHERE i.move_id = ?1)",
        )
        .bind(&job.id)
        .bind(&job.to_location)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE nodes SET blob_hash = i.hash, size = i.size FROM space_move_items i
             WHERE i.move_id = ?1 AND i.kind = 'file' AND nodes.id = i.item_id AND nodes.drive_id = ?2",
        )
        .bind(&job.id)
        .bind(&job.drive_id)
        .execute(&mut *tx)
        .await?;
        for (id, name) in &renamed {
            sqlx::query("UPDATE nodes SET name = ? WHERE id = ?").bind(name).bind(id).execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE nodes SET fs_path = NULL, fs_dev = NULL, fs_ino = NULL, fs_size = NULL, fs_mtime_ns = NULL, fs_birth_ns = NULL WHERE drive_id = ?")
            .bind(&job.drive_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE node_versions SET blob_hash = i.hash, drive_id = NULL, fs_path = NULL
             FROM space_move_items i WHERE i.move_id = ?1 AND i.kind = 'version' AND node_versions.id = i.item_id AND node_versions.drive_id = ?2",
        )
        .bind(&job.id)
        .bind(&job.drive_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE drives SET mode = 'store', location_id = ?2, source_path = NULL, last_scan_at = NULL, scan_report = NULL,
                               used_bytes = (SELECT COALESCE(SUM(size), 0) FROM nodes WHERE drive_id = ?1 AND kind = 'file')
             WHERE id = ?1",
        )
        .bind(&job.drive_id)
        .bind(&job.to_location)
        .execute(&mut *tx)
        .await?;
        let note = (!renamed.is_empty()).then(|| {
            if renamed.len() == 1 {
                "1 item got another name: its folder had an item with the same name in other letter case".to_string()
            } else {
                format!("{} items got another name: their folders had items with the same name in other letter case", renamed.len())
            }
        });
        super::finish(&mut tx, cx, true, note.as_deref()).await?;
        Ok(true)
    }
    .await;
    let switched = crate::db::settle(tx, res).await?;
    if switched {
        crate::folders::spaces_changed();
    }
    Ok(switched)
}

#[cfg(test)]
thread_local! {
    /// Tests: removing the old folder after the switch fails
    pub static FAIL_CLEANUP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A file copied from the folder: (item, path, device, inode, size, modification time)
type Copied = (String, String, Option<i64>, Option<i64>, i64, Option<i64>);

/// After the switch (and again at the next start when it stopped halfway): removes the old folder, each file only
/// when it is still what was copied, then the folders left empty and the space's marker. What stays is noted on the
/// move. The record of copies goes last.
pub async fn cleanup(st: &AppState, job: &Job) -> AppResult<()> {
    #[cfg(test)]
    if FAIL_CLEANUP.with(|f| f.get()) {
        return Err(AppError::internal("the old folder couldn't be removed (test)"));
    }
    let Some(folder) = job.from_path.clone() else { return Ok(()) };
    let mut left = 0usize;
    let mut after = String::new();
    loop {
        let rows: Vec<Copied> = sqlx::query_as(
            "SELECT item_id, path, src_dev, src_ino, size, src_mtime_ns FROM space_move_items
             WHERE move_id = ? AND path IS NOT NULL AND item_id > ? ORDER BY item_id LIMIT 1000",
        )
        .bind(&job.id)
        .bind(&after)
        .fetch_all(&st.db)
        .await?;
        let Some(last) = rows.last() else { break };
        after = last.0.clone();
        let folder = PathBuf::from(&folder);
        left += tokio::task::spawn_blocking(move || {
            let Ok(root) = Pinned::root(&folder) else { return 0 };
            let mut kept = 0;
            for (_, rel, dev, ino, size, mtime) in rows {
                let Ok(p) = root.join(&rel) else { continue };
                let Ok(meta) = std::fs::symlink_metadata(p.as_path()) else { continue };
                let now = seen(&meta);
                let same = now.size == size && Some(now.mtime_ns) == mtime && (now.ino == 0 || (Some(now.dev), Some(now.ino)) == (dev, ino));
                if !same || std::fs::remove_file(p.as_path()).is_err() {
                    kept += 1;
                }
            }
            kept
        })
        .await
        .map_err(AppError::internal)?;
    }
    let folder = PathBuf::from(&folder);
    let rest = tokio::task::spawn_blocking(move || remove_empty(&folder)).await.map_err(AppError::internal)?;
    let old = job.from_path.as_deref().unwrap_or_default();
    let note = match (left, rest.len()) {
        (0, 0) => None,
        // Administrators read the note: a personal space's file names stay private, only how many is told
        (_, n) if job.space_kind == "personal" => Some(match n.max(left) {
            1 => format!("1 item was left in the old folder {old}"),
            n => format!("{n} items were left in the old folder {old}"),
        }),
        _ => Some(format!("Some items were left in the old folder {old}: {}", rest.iter().take(5).cloned().collect::<Vec<_>>().join(", "))),
    };
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        if let Some(note) = &note {
            sqlx::query("UPDATE space_moves SET note = CASE WHEN note IS NULL THEN ?1 ELSE note || char(10) || ?1 END WHERE id = ?2")
                .bind(note)
                .bind(&job.id)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("DELETE FROM space_move_items WHERE move_id = ?").bind(&job.id).execute(&mut *tx).await?;
        AppResult::Ok(())
    }
    .await;
    crate::db::settle(tx, res).await
}

#[cfg(test)]
type DescentHook = Box<dyn Fn(&str)>;

#[cfg(test)]
thread_local! {
    /// Tests: called with a folder's path below the old folder before `remove_empty` goes into it
    static BEFORE_DESCENT: std::cell::RefCell<Option<DescentHook>> = const { std::cell::RefCell::new(None) };
}

/// Removes the folders of the old folder that are empty now, bottom up, and its marker, then itself when nothing else
/// is left; returns what is left (paths below it)
fn remove_empty(folder: &std::path::Path) -> Vec<String> {
    /// `dir` is an open folder: each folder in it is opened in turn without following a link, so one swapped for a
    /// link elsewhere meanwhile is never gone into or emptied
    fn walk(dir: &Pinned, rel: &str, left: &mut Vec<String>) {
        let Ok(read) = std::fs::read_dir(dir.as_path()) else { return };
        for e in read.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let child = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            if is_dir {
                #[cfg(test)]
                BEFORE_DESCENT.with(|h| {
                    if let Some(f) = h.borrow().as_ref() {
                        f(&child)
                    }
                });
                // Emptied first; one that isn't empty keeps what is listed below
                let Some(inside) = e.file_name().to_str().and_then(|n| dir.join(n).ok()) else { continue };
                if let Ok(open) = inside.dir() {
                    walk(&open, &child, left);
                }
                let _ = std::fs::remove_dir(inside.as_path());
            } else if !(rel.is_empty() && name == MARKER) && left.len() < 1000 {
                left.push(child);
            }
        }
    }
    let mut left = Vec::new();
    if let Ok(root) = Pinned::root(folder) {
        walk(&root, "", &mut left);
    }
    // It is no space's folder any more
    let _ = std::fs::remove_file(folder.join(MARKER));
    if left.is_empty() {
        let _ = std::fs::remove_dir(folder);
    }
    left
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    /// The note of a move whose old folder kept `names` after cleaning up
    async fn note_after_cleanup(env: &testutil::TestEnv, kind: &str, names: &[&str]) -> String {
        let folder = env.dir.join(format!("old-{}", new_id()));
        for name in names {
            testutil::write_old(&folder.join(name), b"kept");
        }
        let id = new_id();
        sqlx::query(
            "INSERT INTO space_moves (id, drive_id, space_name, space_kind, from_mode, from_path, to_location, to_mode, state, created_at)
             VALUES (?, 'd', 'S', ?, 'folder', ?, 'local', 'store', 'done', 0)",
        )
        .bind(&id)
        .bind(kind)
        .bind(folder.to_string_lossy())
        .execute(&env.st.db)
        .await
        .unwrap();
        let job = super::super::job(&mut env.st.db.acquire().await.unwrap(), &id).await.unwrap().unwrap();
        cleanup(&env.st, &job).await.unwrap();
        sqlx::query_as::<_, (String,)>("SELECT note FROM space_moves WHERE id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap().0
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn removing_the_old_folder_never_follows_a_folder_swapped_for_a_link() {
        let top = std::env::temp_dir().join(format!("thirtyfile-old-{}", new_id()));
        let (old, outside) = (top.join("old"), top.join("outside"));
        std::fs::create_dir_all(old.join("Sub/Empty")).unwrap();
        std::fs::create_dir_all(outside.join("Empty")).unwrap();
        std::fs::write(outside.join("secret.txt"), b"secret").unwrap();
        // Someone replaces a folder with a link elsewhere right as it is gone into
        let (o, out) = (old.clone(), outside.clone());
        BEFORE_DESCENT.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |rel: &str| {
                if rel == "Sub" {
                    std::fs::rename(o.join("Sub"), o.with_extension("was-sub")).unwrap();
                    std::os::unix::fs::symlink(&out, o.join("Sub")).unwrap();
                }
            }))
        });
        let left = remove_empty(&old);
        BEFORE_DESCENT.with(|h| *h.borrow_mut() = None);
        assert!(outside.join("Empty").is_dir(), "nothing outside is removed");
        assert!(!left.iter().any(|p| p.contains("secret")), "nor named: {left:?}");
        let _ = std::fs::remove_dir_all(&top);
    }

    #[tokio::test]
    async fn what_a_personal_space_leaves_behind_is_counted_not_named() {
        let env = testutil::env().await;
        let team = note_after_cleanup(&env, "team", &["plan.txt"]).await;
        assert!(team.ends_with(": plan.txt"), "{team}");
        let personal = note_after_cleanup(&env, "personal", &["diary.txt", "letters.txt"]).await;
        assert!(personal.starts_with("2 items were left in the old folder "), "{personal}");
        assert!(!personal.contains("diary") && !personal.contains("letters"), "{personal}");
        let one = note_after_cleanup(&env, "personal", &["diary.txt"]).await;
        assert!(one.starts_with("1 item was left in the old folder ") && !one.contains("diary"), "{one}");
    }
}
