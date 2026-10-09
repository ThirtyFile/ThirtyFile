//! Folder spaces in replicas. Their files are in a folder, which other programs change too, so they are read from it:
//! each file and earlier version is hashed once as it is, and its content kept on the targets like any content (by its
//! SHA-256, where the target keeps content). A target holds the same kind of copy for both kinds of space, and a
//! promotion can make a folder space a content-store space on the target.
//!
//! - What was read is recorded per item (`replica_folder_files`): where, its size and modification time, and the
//!   SHA-256 of its content. A file whose record isn't what the index has now is read again: a file another program
//!   changed is read again once the check for changes (folders/) has seen it, which is how far a folder space's
//!   replicas can be behind, besides the sync's own delay. Each sync checks the folder for changes first.
//! - A file that keeps changing (a log another program writes, say) is handled as backups handle it: a file read whole
//!   and unchanged while it was read is copied as read, even when the index has it otherwise by then; one that changed
//!   every time it was read keeps the copy of its content as last read whole. Either is listed as changed while it was
//!   copied, not as failed (never by name in a personal space), and read again by the next sync. The content as last
//!   read stays on the targets until a later read replaces it: it is all there is of the file there meanwhile.
//! - When the space's folder can't be opened (a disk or share that isn't mounted, a location offline), a file whose
//!   record still matches the index is read from a checked copy (`fallback`).
//! - A promotion makes a folder space all of whose files and versions have a checked copy on the target a content-store
//!   space there (`promote`), in the promotion's transaction, like a move into a content store (moves/to_store.rs). Its
//!   folder stays as it was. A folder space not wholly there stays where it is, as it is.

use std::{path::Path, sync::Arc};

use sqlx::SqliteConnection;

use super::{Policy, Target, sync::put_verified};
use crate::{
    backups::runner::{Ctx, Stop},
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
/// What a sync lists for a folder file that kept changing while it was copied: its copy is of the content as last read
/// whole (as backups say), or there is no copy of it yet
pub(crate) const KEPT_AS_READ: &str = crate::backups::capture::KEPT_AS_READ;
pub(crate) const NOT_COPIED: &str = "Changed while it was copied: no copy yet, tried again at the next sync";

/// The content of folder spaces' files and versions as last read, the index having them so or not any more (a file
/// that kept changing is kept as it was last read). `spaces`: the SQL parameter with the spaces (a JSON list); None:
/// every folder space.
pub fn current_hashes(spaces: Option<&str>) -> String {
    let (n, v) = match spaces {
        Some(p) => (format!("n.drive_id IN (SELECT value FROM json_each({p}))"), format!("v.drive_id IN (SELECT value FROM json_each({p}))")),
        None => ("n.drive_id IS NOT NULL".to_string(), "v.drive_id IS NOT NULL".to_string()),
    };
    format!(
        "SELECT f.hash FROM replica_folder_files f JOIN nodes n ON n.id = f.item_id WHERE {n}
         UNION SELECT f.hash FROM replica_folder_files f JOIN node_versions v ON v.id = f.item_id WHERE {v}"
    )
}

/// Files that kept changing while a sync read them
#[derive(Debug, Default)]
pub struct Changing {
    /// Copied as last read whole
    pub kept: i64,
    /// Never read whole: no copy yet
    pub not_copied: i64,
}

impl Changing {
    fn kept(&mut self, cx: &Ctx<'_>, space: &str, shown: &str) {
        self.kept += 1;
        cx.noted(space, Some(shown.to_string()), KEPT_AS_READ.to_string());
    }

    fn not_copied(&mut self, cx: &Ctx<'_>, space: &str, shown: &str) {
        self.not_copied += 1;
        cx.noted(space, Some(shown.to_string()), NOT_COPIED.to_string());
    }

    /// What the job's note says of them
    pub fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        if self.kept > 0 {
            notes.push(if self.kept == 1 {
                "1 file kept changing while it was copied: it is kept as it was last read".to_string()
            } else {
                format!("{} files kept changing while they were copied: each is kept as it was last read", self.kept)
            });
        }
        if self.not_copied > 0 {
            notes.push(if self.not_copied == 1 {
                "1 file kept changing while it was copied and has no copy yet: it is tried again at the next sync".to_string()
            } else {
                format!("{} files kept changing while they were copied and have no copy yet: they are tried again at the next sync", self.not_copied)
            });
        }
        notes
    }
}

/// Folder files a test has keep changing whenever they are read: (space, path)
#[cfg(test)]
pub(super) static KEEPS_CHANGING: crate::sync::Mutex<Vec<(String, String)>> = crate::sync::Mutex::new(Vec::new());

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
/// contents were copied, the changes of each space it holds (`space_changes`, as of its check for changes), and the
/// files that kept changing; Err(stop) inside when the job is asked to stop.
pub async fn sync(
    cx: &Ctx<'_>,
    policy: &Policy,
    targets: &[Target],
    location: &str,
    dst: &Arc<dyn Storage>,
) -> AppResult<Result<(i64, Vec<(String, i64)>, Changing), Stop>> {
    let st = cx.st;
    let mut seqs = Vec::new();
    let mut changing = Changing::default();
    // Only a target that should hold the policy's content
    if !super::required(targets, policy.copies, &policy.source_location).contains(&location) {
        return Ok(Ok((0, seqs, changing)));
    }
    let ids = super::folder_scope(&mut *st.db.acquire().await?, &policy.id).await?;
    let spaces: Vec<Space> =
        sqlx::query_as("SELECT id, source_path, read_only, moving FROM drives WHERE mode = 'folder' AND id IN (SELECT value FROM json_each(?)) ORDER BY id")
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
            match read_unread(cx, policy, dst, location, root, &space.id, &mut changing).await? {
                Ok(n) => copied += n,
                Err(stop) => return Ok(Err(stop)),
            }
        }
        match copy_missing(cx, policy, dst, location, root.as_ref(), &space.id, &mut changing).await? {
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
    Ok(Ok((copied, seqs, changing)))
}

/// Reads the files and versions of a space not read as the index has them, and copies their content to the target
async fn read_unread(
    cx: &Ctx<'_>,
    policy: &Policy,
    dst: &Arc<dyn Storage>,
    location: &str,
    root: &Pinned,
    space: &str,
    changing: &mut Changing,
) -> AppResult<Result<i64, Stop>> {
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
                let one = Item { space, id: &item, kind, rel: &rel, shown: &shown };
                match read_one(cx, policy, dst, location, root, &one, changing).await? {
                    Ok(true) => copied += 1,
                    Ok(false) => {}
                    Err(stop) => return Ok(Err(stop)),
                }
            }
        }
    }
    Ok(Ok(copied))
}

/// What reading a file of the folder found
enum Read {
    /// Read whole and unchanged while it was read, into the temp file: its SHA-256, and what it was then
    Whole(String, crate::folders::Seen),
    /// Not there: moved or deleted since the check for changes (the next one finds it)
    Gone,
    /// It changed each time it was read
    Changing,
    /// It can't be read
    Failed(std::io::Error),
}

/// Reads a file of the folder into a temp file, again a few times while it changes as it is read. Err(stop) when the
/// job is asked to stop meanwhile. (`space`: tests have files of it keep changing.)
#[cfg_attr(not(test), allow(unused_variables, reason = "`space` is for tests"))]
async fn read_to(cx: &Ctx<'_>, root: &Pinned, space: &str, rel: &str, tmp: &Path) -> Result<Read, Stop> {
    let changed = |e: &std::io::Error| crate::hashing::unusable_kind(e) == Some(crate::hashing::Unusable::Changed);
    let read = cx
        .tries(changed, || {
            #[cfg(test)]
            let keeps_changing = KEEPS_CHANGING.lock().iter().any(|(s, p)| s == space && p == rel);
            let (root, rel, tmp) = (root.clone(), rel.to_string(), tmp.to_path_buf());
            async move {
                let _ = std::fs::remove_file(&tmp);
                let read = tokio::task::spawn_blocking(move || crate::folders::read_file(&root, &rel, &tmp)).await.map_err(std::io::Error::other)?;
                #[cfg(test)]
                if keeps_changing && read.is_ok() {
                    return Err(crate::hashing::unusable(crate::hashing::Unusable::Changed, crate::folders::CHANGED));
                }
                read
            }
        })
        .await;
    match read {
        Ok((hash, seen)) => Ok(Read::Whole(hash, seen)),
        Err(Ok(stop)) => {
            let _ = tokio::fs::remove_file(tmp).await;
            Err(stop)
        }
        Err(Err(e)) => {
            let _ = tokio::fs::remove_file(tmp).await;
            Ok(if changed(&e) {
                Read::Changing
            } else if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) {
                Read::Gone
            } else {
                Read::Failed(e)
            })
        }
    }
}

/// A file or earlier version of a folder space, as a sync reads it
struct Item<'a> {
    space: &'a str,
    id: &'a str,
    /// 'file' or 'version'
    kind: &'a str,
    /// Its path in the folder
    rel: &'a str,
    /// How lists name it
    shown: &'a str,
}

/// Reads one file or version, records what it holds, and copies its content to the target when the target doesn't
/// hold it. Ok(true) when a copy was made.
async fn read_one(
    cx: &Ctx<'_>,
    policy: &Policy,
    dst: &Arc<dyn Storage>,
    location: &str,
    root: &Pinned,
    it: &Item<'_>,
    changing: &mut Changing,
) -> AppResult<Result<bool, Stop>> {
    let st = cx.st;
    let tmp = st.tmp_dir().join(format!("replica-{}", new_id()));
    let (hash, seen) = match read_to(cx, root, it.space, it.rel, &tmp).await {
        Ok(Read::Whole(hash, seen)) => (hash, seen),
        Ok(Read::Gone) => return Ok(Ok(false)),
        Ok(Read::Failed(e)) => {
            cx.failed(it.space, Some(it.shown.to_string()), crate::fsops::disk_error(e).message);
            return Ok(Ok(false));
        }
        Ok(Read::Changing) => {
            // Not read whole this time: the target keeps its content as last read whole, from another checked copy
            // when it hasn't got one, and the next sync reads it again
            let last: Option<(String, i64)> =
                sqlx::query_as("SELECT hash, size FROM replica_folder_files WHERE item_id = ?").bind(it.id).fetch_optional(&st.db).await?;
            let held = match &last {
                Some((h, size)) => match hold(cx, policy, dst, location, h, *size).await? {
                    Ok(held) => held,
                    Err(stop) => return Ok(Err(stop)),
                },
                None => false,
            };
            if held {
                changing.kept(cx, it.space, it.shown);
            } else {
                if last.is_some() {
                    // That content can't be had anywhere any more: as if it was never read
                    let _w = st.write_lock.lock().await;
                    sqlx::query("DELETE FROM replica_folder_files WHERE item_id = ?").bind(it.id).execute(&st.db).await?;
                }
                changing.not_copied(cx, it.space, it.shown);
            }
            cx.done(1, 0).await?;
            return Ok(Ok(false));
        }
        Err(stop) => return Ok(Err(stop)),
    };
    // The index still has it there (else it moved since the check for changes: the next one finds it). It may have
    // another size or modification time by now: another program changed it after the check for changes, and it was
    // read whole and unchanged since. It is copied as read, and read again by the next sync.
    let indexed: Option<(Option<String>, Option<i64>, Option<i64>)> = if it.kind == "file" {
        sqlx::query_as("SELECT fs_path, fs_size, fs_mtime_ns FROM nodes WHERE id = ?").bind(it.id).fetch_optional(&st.db).await?
    } else {
        sqlx::query_as("SELECT fs_path, size, NULL FROM node_versions WHERE id = ?").bind(it.id).fetch_optional(&st.db).await?
    };
    let Some((_, size, mtime)) = indexed.filter(|(p, ..)| p.as_deref() == Some(it.rel)) else {
        let _ = tokio::fs::remove_file(&tmp).await;
        cx.done(1, seen.size).await?;
        return Ok(Ok(false));
    };
    let as_indexed = size == Some(seen.size) && (it.kind != "file" || mtime == Some(seen.mtime_ns));
    {
        let _w = st.write_lock.lock().await;
        sqlx::query(
            "INSERT OR REPLACE INTO replica_folder_files (item_id, drive_id, path, size, mtime_ns, dev, ino, hash, read_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(it.id)
        .bind(it.space)
        .bind(it.rel)
        .bind(seen.size)
        .bind((it.kind == "file").then_some(seen.mtime_ns))
        .bind(seen.dev)
        .bind(seen.ino)
        .bind(&hash)
        .bind(now())
        .execute(&st.db)
        .await?;
    }
    let made = if held(st, &hash, location).await? { Ok(false) } else { store(cx, policy, dst, location, &hash, seen.size, &tmp).await? };
    let _ = tokio::fs::remove_file(&tmp).await;
    if made.is_ok() {
        if !as_indexed {
            changing.kept(cx, it.space, it.shown);
        }
        cx.done(1, seen.size).await?;
    }
    Ok(made)
}

/// Whether the target holds a checked copy of a content
async fn held(st: &AppState, hash: &str, location: &str) -> AppResult<bool> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM replica_copies WHERE hash = ? AND location_id = ? AND state = 'verified'")
        .bind(hash)
        .bind(location)
        .fetch_optional(&st.db)
        .await?;
    Ok(row.is_some())
}

/// Reads a content from a checked copy on another location than the target into a temp file; false when none can be
/// read
async fn from_copies(st: &AppState, hash: &str, size: i64, location: &str, tmp: &Path) -> AppResult<bool> {
    let others: Vec<(String,)> = sqlx::query_as("SELECT location_id FROM replica_copies WHERE hash = ? AND state = 'verified' AND location_id != ?")
        .bind(hash)
        .bind(location)
        .fetch_all(&st.db)
        .await?;
    for (other,) in others {
        let Ok(src) = st.storage(&other) else { continue };
        let _ = tokio::fs::remove_file(tmp).await;
        if crate::backups::capture::fetch_verified(&src, hash, size, tmp).await.is_ok() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Makes sure the target holds a checked copy of a content, copying it from another checked copy when it doesn't.
/// Ok(false) when it doesn't and no checked copy of it can be read.
async fn hold(cx: &Ctx<'_>, policy: &Policy, dst: &Arc<dyn Storage>, location: &str, hash: &str, size: i64) -> AppResult<Result<bool, Stop>> {
    let st = cx.st;
    if held(st, hash, location).await? {
        return Ok(Ok(true));
    }
    let tmp = st.tmp_dir().join(format!("replica-{}", new_id()));
    if !from_copies(st, hash, size, location, &tmp).await? {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Ok(Ok(false));
    }
    let made = store(cx, policy, dst, location, hash, size, &tmp).await;
    let _ = tokio::fs::remove_file(&tmp).await;
    if let Err(stop) = made? {
        return Ok(Err(stop));
    }
    Ok(Ok(held(st, hash, location).await?))
}

/// Copies to the target the content of the space's current records it doesn't hold: from the folder, else from
/// another checked copy
async fn copy_missing(
    cx: &Ctx<'_>,
    policy: &Policy,
    dst: &Arc<dyn Storage>,
    location: &str,
    root: Option<&Pinned>,
    space: &str,
    changing: &mut Changing,
) -> AppResult<Result<i64, Stop>> {
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
            // Why the folder didn't have it: the file moved, changed since it was read, or can't be read
            let (mut gone, mut changed, mut failed) = (false, None, None);
            // From the folder: a file whose record has this content, if it still has it
            if let Some(root) = root {
                let item: Option<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                    "SELECT f.path FROM replica_folder_files f WHERE f.drive_id = ? AND f.hash = ? AND {MATCHES} LIMIT 1"
                )))
                .bind(space)
                .bind(&hash)
                .fetch_optional(&st.db)
                .await?;
                if let Some((rel,)) = item {
                    match read_to(cx, root, space, &rel, &tmp).await {
                        Ok(Read::Whole(h, _)) if h == hash => got = true,
                        Ok(Read::Whole(..) | Read::Changing) => changed = Some(rel),
                        Ok(Read::Gone) => gone = true,
                        Ok(Read::Failed(e)) => failed = Some((rel, crate::fsops::disk_error(e).message)),
                        Err(stop) => return Ok(Err(stop)),
                    }
                }
            }
            // Else from another checked copy
            if !got {
                got = from_copies(st, &hash, size, location, &tmp).await?;
            }
            if !got {
                let _ = tokio::fs::remove_file(&tmp).await;
                match (changed, failed) {
                    // Read again by the next sync
                    (Some(rel), _) => changing.not_copied(cx, space, &rel),
                    (None, Some((rel, e))) => cx.failed(space, Some(rel), e),
                    // Moved since the check for changes: the next one finds it
                    _ if gone => {}
                    _ => cx.failed(space, None, format!("The content {} couldn't be read anywhere: {}", &hash[..12], crate::storage::NOT_MOUNTED)),
                }
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
        sqlx::query("DELETE FROM pending_blob_deletes WHERE hash = ? AND location_id = ? AND last_error = 'deferred'")
            .bind(hash)
            .bind(location)
            .execute(&mut *tx)
            .await?;
        AppResult::Ok(!own)
    }
    .await;
    Ok(Ok(crate::db::settle(tx, res).await?))
}

/// How many items of the policy's folder spaces the target should hold, and holds: (held, wanted), counting each
/// content as last read once (a file that kept changing counts with its copy as last read), and each item never read
/// as one more wanted
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
                 WHERE n.drive_id IN (SELECT value FROM json_each(?1)) AND n.kind = 'file' AND n.fs_path IS NOT NULL AND f.item_id IS NULL)
              + (SELECT COUNT(*) FROM node_versions v LEFT JOIN replica_folder_files f ON f.item_id = v.id
                 WHERE v.drive_id IN (SELECT value FROM json_each(?1)) AND v.fs_path IS NOT NULL AND f.item_id IS NULL)",
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
    sqlx::query(
        "UPDATE nodes SET blob_hash = f.hash, size = f.size FROM replica_folder_files f WHERE f.item_id = nodes.id AND nodes.drive_id = ? AND nodes.kind = 'file'",
    )
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
