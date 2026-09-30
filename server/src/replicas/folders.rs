//! Folder spaces in replicas. Their files are in a folder, which other programs change too, so they are read from it:
//! each file and earlier version is hashed once as it is, and its content kept on the targets like any content (by its
//! SHA-256, where the target keeps content). A target holds the same kind of copy for both kinds of space, and a
//! promotion can make a folder space a content-store space on the target.
//!
//! - What was read is recorded per item (`replica_folder_files`): where, its size and modification time, and the
//!   SHA-256 of its content. A record counts only while the index still has the item as it was read. A file another
//!   program changed is read again once the check for changes (folders.rs) has seen it: that is how far a folder
//!   space's replicas can be behind, besides the sync's own delay. Each sync checks the folder for changes first.
//! - A file that changes while it is read is read again, and a file the folder has differently than the index is left
//!   for the next check for changes: a copy is never recorded for content the file doesn't have.
//! - When the space's folder can't be opened (a disk or share that isn't mounted, a location offline), a file whose
//!   record still matches the index is read from a checked copy (`fallback`).
//! - A promotion makes a folder space all of whose files and versions have a checked copy on the target a content-store
//!   space there (`promote`), in the promotion's transaction, like a move into a content store (moves/to_store.rs). Its
//!   folder stays as it was. A folder space not wholly there stays where it is, as it is.

use std::{path::Path, sync::Arc};

use sqlx::SqliteConnection;

use super::{Policy, Target, sync::put_verified};
use crate::{
    backups::{
        capture::{CHANGED, read_file},
        runner::{Ctx, Stop},
    },
    beneath::Pinned,
    error::{AppError, AppResult},
    state::AppState,
    storage::Storage,
    tree::{self, Node},
    util::{new_id, now},
};

/// Items looked at per page
const PAGE: i64 = 200;
/// A copy stored and not recorded yet (ThirtyFile stopped in between) is deleted after this long, unless recorded
const UNRECORDED_GRACE: i64 = 24 * 3600;

/// The content of folder spaces' files and versions as last read, while the index still has them as read. `spaces`:
/// the SQL parameter with the spaces (a JSON list); None: every folder space.
pub fn current_hashes(spaces: Option<&str>) -> String {
    let (n, v) = match spaces {
        Some(p) => (format!("n.drive_id IN (SELECT value FROM json_each({p}))"), format!("v.drive_id IN (SELECT value FROM json_each({p}))")),
        None => ("n.drive_id IS NOT NULL".to_string(), "v.drive_id IS NOT NULL".to_string()),
    };
    format!(
        "SELECT f.hash FROM replica_folder_files f JOIN nodes n ON n.id = f.item_id
         WHERE {n} AND f.path IS n.fs_path AND f.size IS n.fs_size AND f.mtime_ns IS n.fs_mtime_ns
         UNION SELECT f.hash FROM replica_folder_files f JOIN node_versions v ON v.id = f.item_id
         WHERE {v} AND f.path IS v.fs_path AND f.size IS v.size"
    )
}

/// Whether the record `f` still matches its item
const MATCHES: &str = "(EXISTS (SELECT 1 FROM nodes n WHERE n.id = f.item_id AND f.path IS n.fs_path AND f.size IS n.fs_size AND f.mtime_ns IS n.fs_mtime_ns)
      OR EXISTS (SELECT 1 FROM node_versions v WHERE v.id = f.item_id AND f.path IS v.fs_path AND f.size IS v.size))";

/// Files of a space (`?1`, the trash included) not read as the index has them, after the id `?2`
const UNREAD_FILES: &str = "FROM nodes n LEFT JOIN replica_folder_files f ON f.item_id = n.id
     WHERE n.drive_id = ?1 AND n.kind = 'file' AND n.fs_path IS NOT NULL AND n.id > ?2
       AND (f.item_id IS NULL OR f.path IS NOT n.fs_path OR f.size IS NOT n.fs_size OR f.mtime_ns IS NOT n.fs_mtime_ns)";
/// Earlier versions kept in a space's folder, likewise
const UNREAD_VERSIONS: &str = "FROM node_versions v LEFT JOIN replica_folder_files f ON f.item_id = v.id
     WHERE v.drive_id = ?1 AND v.fs_path IS NOT NULL AND v.id > ?2 AND (f.item_id IS NULL OR f.path IS NOT v.fs_path OR f.size IS NOT v.size)";

#[derive(sqlx::FromRow)]
struct Space {
    id: String,
    source_path: Option<String>,
    read_only: bool,
    moving: bool,
}

fn not_mounted() -> AppError {
    AppError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, crate::storage::NOT_MOUNTED)
}

/// Opens a folder space's folder, when it is there and is the space's
async fn open(space: &Space) -> AppResult<Pinned> {
    let path = space.source_path.clone().ok_or_else(not_mounted)?;
    let (id, ro) = (space.id.clone(), space.read_only || space.moving);
    tokio::task::spawn_blocking(move || crate::folders::open_space(Path::new(&path), &id, ro)).await.map_err(AppError::internal)?.map_err(|_| not_mounted())
}

/// Brings the target up to date with the policy's folder spaces: reads what isn't read as the index has it, copies what
/// the target doesn't hold. A space whose folder can't be read is listed as failed; the others go on. Returns how many
/// contents were copied, and the changes of each space it holds (`space_changes`, as of its check for changes); Err(stop)
/// inside when the job is asked to stop.
pub async fn sync(cx: &Ctx<'_>, policy: &Policy, targets: &[Target], location: &str, dst: &Arc<dyn Storage>) -> AppResult<Result<(i64, Vec<(String, i64)>), Stop>> {
    let st = cx.st;
    let mut seqs = Vec::new();
    // Only a target that should hold the policy's content
    if !super::required(targets, policy.copies, &policy.source_location).contains(&location) {
        return Ok(Ok((0, seqs)));
    }
    let ids = super::folder_scope(&mut *st.db.acquire().await?, &policy.id).await?;
    let spaces: Vec<Space> = sqlx::query_as("SELECT id, source_path, read_only, moving FROM drives WHERE mode = 'folder' AND id IN (SELECT value FROM json_each(?)) ORDER BY id")
        .bind(serde_json::to_string(&ids).unwrap())
        .fetch_all(&st.db)
        .await?;
    let mut copied = 0;
    for space in spaces {
        // Changes other programs made, so the index is what the folder holds now
        let root = match crate::folders::scan(st, &space.id).await {
            Ok(report) if report.error.is_some() => Err(AppError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, report.error.unwrap_or_default())),
            Ok(_) => open(&space).await,
            Err(e) => Err(e),
        };
        let (seq,): (i64,) = sqlx::query_as("SELECT COALESCE((SELECT seq FROM space_changes WHERE drive_id = ?), 0)").bind(&space.id).fetch_one(&st.db).await?;
        seqs.push((space.id.clone(), seq));
        let root = match root {
            Ok(r) => Some(r),
            Err(e) => {
                // Its content that has a checked copy elsewhere is still copied from there
                cx.failed(&space.id, None, e.message);
                None
            }
        };
        if let Some(root) = &root {
            match read_unread(cx, policy, dst, location, root, &space.id).await? {
                Ok(n) => copied += n,
                Err(stop) => return Ok(Err(stop)),
            }
        }
        match copy_missing(cx, policy, dst, location, root.as_ref(), &space.id).await? {
            Ok(n) => copied += n,
            Err(stop) => return Ok(Err(stop)),
        }
        // Records of items that are gone
        if root.is_some() {
            let _w = st.write_lock.lock().await;
            sqlx::query(
                "DELETE FROM replica_folder_files WHERE drive_id = ?1
                   AND NOT EXISTS (SELECT 1 FROM nodes n WHERE n.id = replica_folder_files.item_id AND n.drive_id = ?1)
                   AND NOT EXISTS (SELECT 1 FROM node_versions v WHERE v.id = replica_folder_files.item_id AND v.drive_id = ?1)",
            )
            .bind(&space.id)
            .execute(&st.db)
            .await?;
        }
    }
    Ok(Ok((copied, seqs)))
}

/// Reads the files and versions of a space not read as the index has them, and copies their content to the target
async fn read_unread(cx: &Ctx<'_>, policy: &Policy, dst: &Arc<dyn Storage>, location: &str, root: &Pinned, space: &str) -> AppResult<Result<i64, Stop>> {
    let st = cx.st;
    let mut copied = 0;
    for kind in ["file", "version"] {
        let (from, size) = if kind == "file" { (UNREAD_FILES, "n.fs_size") } else { (UNREAD_VERSIONS, "v.size") };
        let (n, bytes): (i64, i64) =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT COUNT(*), COALESCE(SUM({size}), 0) {from}"))).bind(space).bind("").fetch_one(&st.db).await?;
        cx.add_total(n, bytes);
        let mut last = String::new();
        loop {
            let sql = if kind == "file" {
                format!("SELECT n.id, n.fs_path, n.name {from} ORDER BY n.id LIMIT {PAGE}")
            } else {
                format!("SELECT v.id, v.fs_path, COALESCE((SELECT name FROM nodes WHERE id = v.node_id), '') {from} ORDER BY v.id LIMIT {PAGE}")
            };
            let rows: Vec<(String, String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(sql)).bind(space).bind(&last).fetch_all(&st.db).await?;
            let Some((id, ..)) = rows.last() else { break };
            last = id.clone();
            for (item, rel, name) in rows {
                if let Some(stop) = cx.stop() {
                    return Ok(Err(stop));
                }
                let shown = if kind == "version" { format!("{name} (an earlier version)") } else { rel.clone() };
                match read_one(cx, policy, dst, location, root, space, &item, kind, &rel, &shown).await? {
                    Ok(true) => copied += 1,
                    Ok(false) => {}
                    Err(stop) => return Ok(Err(stop)),
                }
            }
        }
    }
    Ok(Ok(copied))
}

/// Reads a file of the folder into a temp file: its SHA-256 and what it was when read. Ok(None) when it isn't there
/// (moved or deleted since the check for changes: the next one finds it) or can't be read (listed as failed).
async fn read_to(cx: &Ctx<'_>, root: &Pinned, space: &str, rel: &str, shown: &str, tmp: &Path) -> AppResult<Result<Option<(String, crate::backups::capture::Seen)>, Stop>> {
    let read = cx
        .tries(
            |e: &std::io::Error| e.to_string() == CHANGED,
            || {
                let (root, rel, tmp) = (root.clone(), rel.to_string(), tmp.to_path_buf());
                async move {
                    let _ = std::fs::remove_file(&tmp);
                    tokio::task::spawn_blocking(move || read_file(&root, &rel, &tmp)).await.map_err(std::io::Error::other)?
                }
            },
        )
        .await;
    match read {
        Ok(r) => Ok(Ok(Some(r))),
        Err(Ok(stop)) => {
            let _ = tokio::fs::remove_file(tmp).await;
            Ok(Err(stop))
        }
        Err(Err(e)) => {
            let _ = tokio::fs::remove_file(tmp).await;
            if !matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) {
                cx.failed(space, Some(shown.to_string()), crate::fsops::disk_error(e).message);
            }
            Ok(Ok(None))
        }
    }
}

/// Reads one file or version, records what it holds, and copies its content to the target when the target doesn't
/// hold it. Ok(true) when a copy was made.
#[allow(clippy::too_many_arguments)]
async fn read_one(
    cx: &Ctx<'_>,
    policy: &Policy,
    dst: &Arc<dyn Storage>,
    location: &str,
    root: &Pinned,
    space: &str,
    item: &str,
    kind: &str,
    rel: &str,
    shown: &str,
) -> AppResult<Result<bool, Stop>> {
    let st = cx.st;
    let tmp = st.tmp_dir().join(format!("replica-{}", new_id()));
    let (hash, seen) = match read_to(cx, root, space, rel, shown, &tmp).await? {
        Ok(Some(r)) => r,
        Ok(None) => return Ok(Ok(false)),
        Err(stop) => return Ok(Err(stop)),
    };
    // Only what the index has: a file the folder has differently waits for the next check for changes
    let indexed: Option<(Option<String>, Option<i64>, Option<i64>)> = if kind == "file" {
        sqlx::query_as("SELECT fs_path, fs_size, fs_mtime_ns FROM nodes WHERE id = ?").bind(item).fetch_optional(&st.db).await?
    } else {
        sqlx::query_as("SELECT fs_path, size, NULL FROM node_versions WHERE id = ?").bind(item).fetch_optional(&st.db).await?
    };
    let same = indexed.is_some_and(|(p, size, mtime)| p.as_deref() == Some(rel) && size == Some(seen.size) && (kind != "file" || mtime == Some(seen.mtime_ns)));
    if !same {
        let _ = tokio::fs::remove_file(&tmp).await;
        cx.done(1, seen.size).await?;
        return Ok(Ok(false));
    }
    {
        let _w = st.write_lock.lock().await;
        sqlx::query(
            "INSERT OR REPLACE INTO replica_folder_files (item_id, drive_id, path, size, mtime_ns, dev, ino, hash, read_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(item)
        .bind(space)
        .bind(rel)
        .bind(seen.size)
        .bind((kind == "file").then_some(seen.mtime_ns))
        .bind(seen.dev)
        .bind(seen.ino)
        .bind(&hash)
        .bind(now())
        .execute(&st.db)
        .await?;
    }
    let held: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM replica_copies WHERE hash = ? AND location_id = ? AND state = 'verified'")
        .bind(&hash)
        .bind(location)
        .fetch_optional(&st.db)
        .await?;
    let made = if held.is_none() { store(cx, policy, dst, location, &hash, seen.size, &tmp).await? } else { Ok(false) };
    let _ = tokio::fs::remove_file(&tmp).await;
    if made.is_ok() {
        cx.done(1, seen.size).await?;
    }
    Ok(made)
}

/// Copies to the target the content of the space's current records it doesn't hold: from the folder, else from
/// another checked copy
async fn copy_missing(cx: &Ctx<'_>, policy: &Policy, dst: &Arc<dyn Storage>, location: &str, root: Option<&Pinned>, space: &str) -> AppResult<Result<i64, Stop>> {
    let st = cx.st;
    let missing = format!(
        "FROM (SELECT f.hash, MAX(f.size) AS size FROM replica_folder_files f WHERE f.drive_id = ?1 AND {MATCHES} GROUP BY f.hash) m
         WHERE NOT EXISTS (SELECT 1 FROM replica_copies c WHERE c.hash = m.hash AND c.location_id = ?2 AND c.state = 'verified') AND m.hash > ?3"
    );
    let (n, bytes): (i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT COUNT(*), COALESCE(SUM(m.size), 0) {missing}")))
        .bind(space)
        .bind(location)
        .bind("")
        .fetch_one(&st.db)
        .await?;
    cx.add_total(n, bytes);
    let mut copied = 0;
    let mut last = String::new();
    loop {
        let rows: Vec<(String, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT m.hash, m.size {missing} ORDER BY m.hash LIMIT {PAGE}")))
            .bind(space)
            .bind(location)
            .bind(&last)
            .fetch_all(&st.db)
            .await?;
        let Some((h, _)) = rows.last() else { break };
        last = h.clone();
        for (hash, size) in rows {
            if let Some(stop) = cx.stop() {
                return Ok(Err(stop));
            }
            let tmp = st.tmp_dir().join(format!("replica-{}", new_id()));
            let mut got = false;
            // From the folder: a file whose record has this content, if it still has it
            if let Some(root) = root {
                let item: Option<(String,)> =
                    sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT f.path FROM replica_folder_files f WHERE f.drive_id = ? AND f.hash = ? AND {MATCHES} LIMIT 1")))
                        .bind(space)
                        .bind(&hash)
                        .fetch_optional(&st.db)
                        .await?;
                if let Some((rel,)) = item {
                    match read_to(cx, root, space, &rel, &rel, &tmp).await? {
                        Ok(Some((h, _))) if h == hash => got = true,
                        Ok(_) => {}
                        Err(stop) => return Ok(Err(stop)),
                    }
                }
            }
            // Else from another checked copy
            if !got {
                let others: Vec<(String,)> = sqlx::query_as("SELECT location_id FROM replica_copies WHERE hash = ? AND state = 'verified' AND location_id != ?")
                    .bind(&hash)
                    .bind(location)
                    .fetch_all(&st.db)
                    .await?;
                for (other,) in others {
                    let Ok(src) = st.storage(&other) else { continue };
                    let _ = tokio::fs::remove_file(&tmp).await;
                    if crate::backups::capture::fetch_verified(&src, &hash, size, &tmp).await.is_ok() {
                        got = true;
                        break;
                    }
                }
            }
            if !got {
                let _ = tokio::fs::remove_file(&tmp).await;
                cx.failed(space, None, format!("The content {} couldn't be read anywhere: {}", &hash[..12], crate::storage::NOT_MOUNTED));
                continue;
            }
            let made = store(cx, policy, dst, location, &hash, size, &tmp).await?;
            let _ = tokio::fs::remove_file(&tmp).await;
            match made {
                Ok(true) => copied += 1,
                Ok(false) => {}
                Err(stop) => return Ok(Err(stop)),
            }
            cx.done(1, size).await?;
        }
    }
    Ok(Ok(copied))
}

/// Stores a content read into a temp file on the target, checked, and records the copy. Content the target keeps as
/// its own already (a content-store space there has it) isn't written again: it is checked, and recorded so it stays
/// while the folder space needs it.
async fn store(cx: &Ctx<'_>, policy: &Policy, dst: &Arc<dyn Storage>, location: &str, hash: &str, size: i64, tmp: &Path) -> AppResult<Result<bool, Stop>> {
    let st = cx.st;
    let _staging = tree::stage_guard(st, hash).await;
    let own: Option<(String,)> = sqlx::query_as("SELECT location_id FROM blobs WHERE hash = ?").bind(hash).fetch_optional(&st.db).await?;
    let own = own.is_some_and(|(l,)| l == location);
    if own {
        if !super::sync::whole(dst.as_ref(), hash, size).await {
            cx.failed("", None, format!("The copy of {} is missing or damaged", &hash[..12]));
            return Ok(Ok(false));
        }
    } else {
        {
            let _w = st.write_lock.lock().await;
            sqlx::query(
                "INSERT INTO pending_blob_deletes (hash, location_id, created_at, attempts, last_error) VALUES (?, ?, ?, 0, 'deferred')
                 ON CONFLICT (hash, location_id) DO UPDATE SET created_at = MIN(created_at, excluded.created_at)",
            )
            .bind(hash)
            .bind(location)
            .bind(now() + UNRECORDED_GRACE)
            .execute(&st.db)
            .await?;
        }
        match put_verified(cx, dst, location, hash, size, tmp).await? {
            Ok(()) => {}
            Err(stop) => return Ok(Err(stop)),
        }
    }
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        let (epoch,): (i64,) = sqlx::query_as("SELECT epoch FROM replica_policies WHERE id = ?").bind(&policy.id).fetch_one(&mut *tx).await?;
        if epoch != policy.epoch {
            // Promoted meanwhile: left to deletion, checked again before
            return Ok(false);
        }
        sqlx::query("INSERT OR REPLACE INTO replica_copies (hash, location_id, size, state, created_at, verified_at) VALUES (?, ?, ?, 'verified', ?, ?)")
            .bind(hash)
            .bind(location)
            .bind(size)
            .bind(now())
            .bind(now())
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM pending_blob_deletes WHERE hash = ? AND location_id = ? AND last_error = 'deferred'").bind(hash).bind(location).execute(&mut *tx).await?;
        AppResult::Ok(!own)
    }
    .await;
    Ok(Ok(crate::db::settle(tx, res).await?))
}

/// How many items of the policy's folder spaces the target should hold, and holds: (held, wanted), counting each
/// content once, and each item not read yet as one more wanted
pub async fn coverage(st: &AppState, policy: &Policy, location: &str) -> AppResult<(i64, i64)> {
    let spaces = super::folder_scope(&mut *st.db.acquire().await?, &policy.id).await?;
    if spaces.is_empty() {
        return Ok((0, 0));
    }
    let list = serde_json::to_string(&spaces).unwrap();
    let current = current_hashes(Some("?1"));
    let (held, read): (i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT COALESCE(SUM(EXISTS (SELECT 1 FROM replica_copies c WHERE c.hash = h.hash AND c.location_id = ?2 AND c.state = 'verified')), 0), COUNT(*)
         FROM ({current}) h"
    )))
    .bind(&list)
    .bind(location)
    .fetch_one(&st.db)
    .await?;
    let (unread,): (i64,) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM nodes n LEFT JOIN replica_folder_files f ON f.item_id = n.id
                 WHERE n.drive_id IN (SELECT value FROM json_each(?1)) AND n.kind = 'file' AND n.fs_path IS NOT NULL
                   AND (f.item_id IS NULL OR f.path IS NOT n.fs_path OR f.size IS NOT n.fs_size OR f.mtime_ns IS NOT n.fs_mtime_ns))
              + (SELECT COUNT(*) FROM node_versions v LEFT JOIN replica_folder_files f ON f.item_id = v.id
                 WHERE v.drive_id IN (SELECT value FROM json_each(?1)) AND v.fs_path IS NOT NULL
                   AND (f.item_id IS NULL OR f.path IS NOT v.fs_path OR f.size IS NOT v.size))",
    )
    .bind(&list)
    .fetch_one(&st.db)
    .await?;
    Ok((held, read + unread))
}

/// A folder space's file whose folder can't be opened: a checked copy of its content as last read (while the index
/// still has the file as read), on a location that works, of a policy that allows reading from copies. Noted like any
/// read from a replica.
pub async fn fallback(st: &AppState, n: &Node) -> Option<crate::files::Source> {
    let (hash, location) = copy_of(st, &mut *st.db.acquire().await.ok()?, &n.id).await.ok()??;
    let primary: Option<(Option<String>,)> = sqlx::query_as("SELECT location_id FROM drives WHERE id = ?").bind(n.drive()).fetch_optional(&st.db).await.ok()?;
    let primary = primary.and_then(|(l,)| l).unwrap_or_default();
    super::note_fallback(st, &primary, &location).await;
    Some(crate::files::Source::Stored { hash, location })
}

/// A checked copy of a folder space's file as last read, on a location that works: (hash, location)
pub async fn copy_of(st: &AppState, conn: &mut SqliteConnection, item: &str) -> AppResult<Option<(String, String)>> {
    let rows: Vec<(String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT f.hash, c.location_id FROM replica_folder_files f JOIN replica_copies c ON c.hash = f.hash AND c.state = 'verified'
         WHERE f.item_id = ? AND {MATCHES}
           AND EXISTS (SELECT 1 FROM replica_targets t JOIN replica_policies p ON p.id = t.policy_id WHERE t.location_id = c.location_id AND p.read_fallback = 1)
         ORDER BY c.verified_at DESC"
    )))
    .bind(item)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().find(|(_, l)| st.location_offline(l).is_none()))
}

/// What promoting would do with a folder space: its items, and those with a checked copy on the target
pub async fn readiness(conn: &mut SqliteConnection, space: &str, target: &str) -> AppResult<(i64, i64, i64)> {
    let (items, covered, bytes): (i64, i64, i64) = sqlx::query_as(
        "WITH items AS (
           SELECT n.id, n.fs_size AS size FROM nodes n WHERE n.drive_id = ?1 AND n.kind = 'file' AND n.fs_path IS NOT NULL
           UNION ALL SELECT v.id, v.size FROM node_versions v WHERE v.drive_id = ?1 AND v.fs_path IS NOT NULL
         ),
         covered AS (
           SELECT i.id, i.size FROM items i JOIN replica_folder_files f ON f.item_id = i.id
           WHERE EXISTS (SELECT 1 FROM replica_copies c WHERE c.hash = f.hash AND c.location_id = ?2 AND c.state = 'verified')
             AND (EXISTS (SELECT 1 FROM nodes n WHERE n.id = f.item_id AND f.path IS n.fs_path AND f.size IS n.fs_size AND f.mtime_ns IS n.fs_mtime_ns)
                  OR EXISTS (SELECT 1 FROM node_versions v WHERE v.id = f.item_id AND f.path IS v.fs_path AND f.size IS v.size))
         )
         SELECT (SELECT COUNT(*) FROM items), (SELECT COUNT(*) FROM covered), (SELECT COALESCE(SUM(size), 0) FROM covered)",
    )
    .bind(space)
    .bind(target)
    .fetch_one(conn)
    .await?;
    Ok((items, covered, bytes))
}

/// Makes a folder space all of whose files and versions have a checked copy on the target a content-store space there,
/// in the promotion's transaction (the caller holds the space: `folders::hold`). False, changing nothing, when an item
/// isn't wholly there. Its folder stays as it was.
pub async fn promote(conn: &mut SqliteConnection, space: &str, target: &str) -> AppResult<bool> {
    let (items, covered, _) = readiness(conn, space, target).await?;
    if items != covered {
        return Ok(false);
    }
    let t = now();
    // Items named apart where only letter case tells them apart (the content store doesn't tell them apart)
    let nodes: Vec<crate::moves::to_store::Named> = sqlx::query_as(
        "SELECT id, parent_id, name, kind FROM nodes WHERE drive_id = ?1 AND trashed_at IS NULL AND parent_id IN (
           SELECT parent_id FROM nodes WHERE drive_id = ?1 AND trashed_at IS NULL GROUP BY parent_id, unicode_lower(name) HAVING COUNT(*) > 1
         )",
    )
    .bind(space)
    .fetch_all(&mut *conn)
    .await?;
    let renamed = crate::moves::to_store::case_apart(&nodes);
    for (id, _) in &renamed {
        sqlx::query("UPDATE nodes SET name = char(1) || id WHERE id = ?").bind(id).execute(&mut *conn).await?;
    }
    // A reference for each file and version: the content is the target's own now (or stays where another space keeps it)
    sqlx::query(
        "INSERT INTO blobs (hash, size, refcount, created_at, location_id)
         SELECT f.hash, MAX(f.size), COUNT(*), ?3, ?2 FROM replica_folder_files f
         WHERE f.drive_id = ?1 AND (EXISTS (SELECT 1 FROM nodes n WHERE n.id = f.item_id AND n.drive_id = ?1 AND n.kind = 'file')
                                    OR EXISTS (SELECT 1 FROM node_versions v WHERE v.id = f.item_id AND v.drive_id = ?1))
         GROUP BY f.hash
         ON CONFLICT (hash) DO UPDATE SET refcount = refcount + excluded.refcount",
    )
    .bind(space)
    .bind(target)
    .bind(t)
    .execute(&mut *conn)
    .await?;
    sqlx::query("UPDATE nodes SET blob_hash = f.hash, size = f.size FROM replica_folder_files f WHERE f.item_id = nodes.id AND nodes.drive_id = ? AND nodes.kind = 'file'")
        .bind(space)
        .execute(&mut *conn)
        .await?;
    for (id, name) in &renamed {
        sqlx::query("UPDATE nodes SET name = ? WHERE id = ?").bind(name).bind(id).execute(&mut *conn).await?;
    }
    sqlx::query("UPDATE nodes SET fs_path = NULL, fs_dev = NULL, fs_ino = NULL, fs_size = NULL, fs_mtime_ns = NULL, fs_birth_ns = NULL WHERE drive_id = ?")
        .bind(space)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "UPDATE node_versions SET blob_hash = f.hash, drive_id = NULL, fs_path = NULL FROM replica_folder_files f
         WHERE f.item_id = node_versions.id AND node_versions.drive_id = ?",
    )
    .bind(space)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "UPDATE drives SET mode = 'store', location_id = ?2, source_path = NULL, last_scan_at = NULL, scan_report = NULL,
                           used_bytes = (SELECT COALESCE(SUM(size), 0) FROM nodes WHERE drive_id = ?1 AND kind = 'file')
         WHERE id = ?1",
    )
    .bind(space)
    .bind(target)
    .execute(&mut *conn)
    .await?;
    // The copies there are the content now, unless a folder space elsewhere still has records of it
    let hashes: Vec<(String,)> = sqlx::query_as("SELECT DISTINCT hash FROM replica_folder_files WHERE drive_id = ?").bind(space).fetch_all(&mut *conn).await?;
    sqlx::query("DELETE FROM replica_folder_files WHERE drive_id = ?").bind(space).execute(&mut *conn).await?;
    sqlx::query(
        "DELETE FROM replica_copies WHERE location_id = ?1 AND hash IN (SELECT value FROM json_each(?2))
           AND hash IN (SELECT hash FROM blobs WHERE location_id = ?1) AND hash NOT IN (SELECT hash FROM replica_folder_files)",
    )
    .bind(target)
    .bind(serde_json::to_string(&hashes.into_iter().map(|(h,)| h).collect::<Vec<_>>()).unwrap())
    .execute(&mut *conn)
    .await?;
    Ok(true)
}
