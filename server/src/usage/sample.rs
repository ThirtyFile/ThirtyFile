//! The sampler: every five minutes it writes the operation counters (meter.rs), every fifteen it measures how much
//! each storage location holds, and every hour it deletes rows that are older than their tier keeps.
//!
//! A sample is written into three tiers at once (fine, hours, days), so nothing has to be summed up later: in the
//! hour and day tiers, operation counters add up and their histograms merge, and a capacity sample replaces the one
//! before it (the last sample of an hour or day stands for it). Days are UTC days.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde::Serialize;

use super::meter::{Hist, Op, Window, Windows};
use crate::usage::Memory;
use crate::{error::AppResult, state::AppState, util::now};

/// Operation counters are written this often (seconds), at multiples of it
pub const OPS_SPAN: i64 = 300;
/// Capacity is measured this often
pub const CAPACITY_SPAN: i64 = 900;
pub const HOUR: i64 = 3600;
pub const DAY: i64 = 86400;
/// How long each tier is kept (span, seconds)
pub const RETENTION: [(i64, i64); 4] = [(OPS_SPAN, 2 * DAY), (CAPACITY_SPAN, 2 * DAY), (HOUR, 45 * DAY), (DAY, 400 * DAY)];

/// Start of the period of `span` seconds that holds `at`
pub fn bucket(at: i64, span: i64) -> i64 {
    at - at.rem_euclid(span)
}

/// Starts the sampler: capacity is measured shortly after the start, then everything at its own multiple of five
/// minutes
pub fn spawn(st: AppState) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(10)).await;
        if let Err(e) = sample_capacity(&st, now()).await {
            tracing::warn!("Couldn't measure how much the storage locations hold: {}", e.message);
        }
        loop {
            // A few seconds after the end of the period, so what happened in it is all counted
            let t = now();
            let next = bucket(t, OPS_SPAN) + OPS_SPAN;
            tokio::time::sleep(Duration::from_secs((next - t) as u64 + 2)).await;
            tick(&st, next).await;
        }
    });
}

/// What the sampler does at the end of the period that ends at `end`
async fn tick(st: &AppState, end: i64) {
    let windows = st.part::<Memory>().meters.take();
    if let Err(e) = write_ops(st, &windows, end - OPS_SPAN).await {
        tracing::warn!("Couldn't write the storage operation counters, will try again: {}", e.message);
        st.part::<Memory>().meters.restore(windows);
    }
    if end % CAPACITY_SPAN == 0
        && let Err(e) = sample_capacity(st, end).await
    {
        tracing::warn!("Couldn't measure how much the storage locations hold: {}", e.message);
    }
    if end % HOUR == 0
        && let Err(e) = prune(st, end).await
    {
        tracing::warn!("Couldn't delete old storage usage samples: {}", e.message);
    }
}

// ───────────── Operations ─────────────

/// Adds the counters of the period that started at `at` to the rows of every tier
pub async fn write_ops(st: &AppState, windows: &Windows, at: i64) -> AppResult<()> {
    if windows.values().all(HashMap::is_empty) {
        return Ok(());
    }
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        for (location, per) in windows {
            for ((op, work), w) in per {
                for span in [OPS_SPAN, HOUR, DAY] {
                    let at = bucket(at, span);
                    let old: Option<OpsRow> = sqlx::query_as(
                        "SELECT count, errors, timeouts, bytes, total_us, max_us, hist FROM usage_ops
                         WHERE location_id = ? AND span = ? AND at = ? AND op = ? AND work = ?",
                    )
                    .bind(location)
                    .bind(span)
                    .bind(at)
                    .bind(op.as_str())
                    .bind(work.as_str())
                    .fetch_optional(&mut *tx)
                    .await?;
                    let mut sum = old.map(OpsRow::window).unwrap_or_default();
                    sum.merge(w);
                    sqlx::query(
                        "INSERT OR REPLACE INTO usage_ops (location_id, span, at, op, work, count, errors, timeouts, bytes, total_us, max_us, hist)
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    )
                    .bind(location)
                    .bind(span)
                    .bind(at)
                    .bind(op.as_str())
                    .bind(work.as_str())
                    .bind(clamp(sum.count))
                    .bind(clamp(sum.errors))
                    .bind(clamp(sum.timeouts))
                    .bind(clamp(sum.bytes))
                    .bind(clamp(sum.total_us))
                    .bind(clamp(sum.max_us))
                    .bind(sum.hist.encode())
                    .execute(&mut *tx)
                    .await?;
                }
            }
        }
        AppResult::Ok(())
    }
    .await;
    crate::db::settle(tx, res).await
}

fn clamp(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Counters as stored
#[derive(sqlx::FromRow)]
pub struct OpsRow {
    pub count: i64,
    pub errors: i64,
    pub timeouts: i64,
    pub bytes: i64,
    pub total_us: i64,
    pub max_us: i64,
    pub hist: String,
}

impl OpsRow {
    pub fn window(self) -> Window {
        let u = |v: i64| v.max(0) as u64;
        Window {
            count: u(self.count),
            errors: u(self.errors),
            timeouts: u(self.timeouts),
            bytes: u(self.bytes),
            total_us: u(self.total_us),
            max_us: u(self.max_us),
            hist: Hist::decode(&self.hist),
        }
    }
}

// ───────────── Capacity ─────────────

/// How much a storage location holds (a row of usage_capacity; the migration says what each field is)
#[derive(Debug, Clone, Default, PartialEq, Serialize, sqlx::FromRow)]
pub struct Capacity {
    pub location_id: String,
    pub sampled_at: i64,
    pub live_bytes: i64,
    pub trash_bytes: i64,
    pub version_bytes: i64,
    pub store_bytes: i64,
    pub folder_bytes: i64,
    pub folder_version_bytes: i64,
    pub pending_deletes: i64,
    pub temp_bytes: i64,
    pub backup_bytes: Option<i64>,
    pub replica_bytes: Option<i64>,
    pub disk_free: Option<i64>,
    pub disk_total: Option<i64>,
    pub disk_id: Option<String>,
    pub online: bool,
    pub took_ms: i64,
}

/// Figures from the database, by location ('' for folder spaces on no location)
async fn totals(st: &AppState) -> AppResult<HashMap<String, Capacity>> {
    let mut by: HashMap<String, Capacity> = HashMap::new();
    let db = &st.db;
    // The content store: each content once, on the location that holds it (covering index blobs_location_size)
    let rows: Vec<(String, i64)> = sqlx::query_as("SELECT location_id, COALESCE(SUM(size), 0) FROM blobs GROUP BY location_id").fetch_all(db).await?;
    for (id, bytes) in rows {
        by.entry(id).or_default().store_bytes = bytes;
    }
    // Spaces: `used_bytes` counts every file of a space, the trash too
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT COALESCE(location_id, ''), COALESCE(SUM(used_bytes), 0), COALESCE(SUM(CASE WHEN mode = 'folder' THEN used_bytes ELSE 0 END), 0)
         FROM drives GROUP BY 1",
    )
    .fetch_all(db)
    .await?;
    for (id, used, folder) in rows {
        let c = by.entry(id).or_default();
        c.live_bytes = used;
        c.folder_bytes = folder;
    }
    // The trash: every item of a deleted folder has its trash_id (partial index nodes_trash_id), so only those are read
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT COALESCE(d.location_id, ''), COALESCE(SUM(n.size), 0) FROM nodes n JOIN drives d ON d.id = n.drive_id
         WHERE n.trash_id IS NOT NULL AND n.trashed_at IS NOT NULL AND n.kind = 'file' GROUP BY 1",
    )
    .fetch_all(db)
    .await?;
    for (id, trash) in rows {
        let c = by.entry(id).or_default();
        c.trash_bytes = trash;
        c.live_bytes -= trash;
    }
    // Earlier versions: where their content is (the content store's location, or the folder space's)
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT COALESCE(b.location_id, d.location_id, ''), COALESCE(SUM(v.size), 0),
                COALESCE(SUM(CASE WHEN v.blob_hash IS NULL THEN v.size ELSE 0 END), 0)
         FROM node_versions v LEFT JOIN blobs b ON b.hash = v.blob_hash LEFT JOIN drives d ON d.id = v.drive_id GROUP BY 1",
    )
    .fetch_all(db)
    .await?;
    for (id, all, in_folders) in rows {
        let c = by.entry(id).or_default();
        c.version_bytes = all;
        c.folder_version_bytes = in_folders;
    }
    let rows: Vec<(String, i64)> = sqlx::query_as("SELECT location_id, COUNT(*) FROM pending_blob_deletes GROUP BY location_id").fetch_all(db).await?;
    for (id, n) in rows {
        by.entry(id).or_default().pending_deletes = n;
    }
    // Copies kept on a location (backups/): each content once per copy. Locations holding none have none (NULL).
    for (id, bytes) in crate::backups::bytes_by_location(db).await? {
        by.entry(id).or_default().backup_bytes = Some(bytes);
    }
    // Replicas kept on a location (replicas/)
    for (id, bytes) in crate::replicas::bytes_by_location(db).await? {
        by.entry(id).or_default().replica_bytes = Some(bytes);
    }
    Ok(by)
}

/// Free and total bytes of the disk holding `path`, and which disk it is; None when the system doesn't tell within a
/// few seconds (a network share that stopped answering must not hold the sampler)
async fn disk(path: PathBuf) -> Option<(i64, i64, Option<String>)> {
    let task = tokio::task::spawn_blocking(move || {
        let (free, total) = crate::util::disk_space(&path)?;
        Some((clamp(free), clamp(total), disk_id(&path)))
    });
    tokio::time::timeout(Duration::from_secs(3), task).await.ok()?.ok()?
}

/// Which disk holds `path`, as `disk_id` names it; None when the system doesn't tell within a few seconds
pub async fn disk_of_path(path: &Path) -> Option<String> {
    disk(path.to_path_buf()).await.and_then(|(_, _, id)| id)
}

/// The disk holding `path`: its device number (Windows: its drive)
#[cfg(unix)]
fn disk_id(path: &Path) -> Option<String> {
    Some(std::os::unix::fs::MetadataExt::dev(&std::fs::metadata(path).ok()?).to_string())
}

#[cfg(not(unix))]
fn disk_id(path: &Path) -> Option<String> {
    let absolute = std::path::absolute(path).ok()?;
    let first = absolute.components().next()?;
    Some(first.as_os_str().to_string_lossy().to_uppercase())
}

/// Measures every location and everything together, and writes the samples into every tier
pub async fn sample_capacity(st: &AppState, at: i64) -> AppResult<Vec<Capacity>> {
    let started = Instant::now();
    let by = totals(st).await?;
    let locations: Vec<(String, String, String)> = sqlx::query_as("SELECT id, kind, config FROM storage_locations").fetch_all(&st.db).await?;
    let (temp,): (i64,) = sqlx::query_as("SELECT COALESCE(SUM(offset), 0) FROM uploads WHERE node_id IS NULL").fetch_one(&st.db).await?;
    // The disks of locations on this server, and of the data folder, measured together
    let mut paths: Vec<Option<PathBuf>> = locations
        .iter()
        .map(|(id, kind, config)| {
            (kind == "local").then(|| {
                let cfg = serde_json::from_str(config).unwrap_or_default();
                crate::storage::local_root(id, &cfg, &st.storage_dir).unwrap_or_else(|_| st.storage_dir.clone())
            })
        })
        .collect();
    paths.push(Some(st.data_dir.clone()));
    let disks = futures_util::future::join_all(paths.into_iter().map(|p| async move { disk(p?).await })).await;
    let sampled_at = now();
    let mut out = Vec::new();
    let mut all = Capacity { location_id: String::new(), sampled_at, temp_bytes: temp, online: true, ..Default::default() };
    for c in by.values() {
        all.live_bytes += c.live_bytes;
        all.trash_bytes += c.trash_bytes;
        all.version_bytes += c.version_bytes;
        all.store_bytes += c.store_bytes;
        all.folder_bytes += c.folder_bytes;
        all.folder_version_bytes += c.folder_version_bytes;
        all.pending_deletes += c.pending_deletes;
        if let Some(b) = c.backup_bytes {
            all.backup_bytes = Some(all.backup_bytes.unwrap_or_default() + b);
        }
        if let Some(b) = c.replica_bytes {
            all.replica_bytes = Some(all.replica_bytes.unwrap_or_default() + b);
        }
    }
    for ((id, _, _), disk) in locations.iter().zip(&disks) {
        let mut c = by.get(id).cloned().unwrap_or_default();
        c.location_id = id.clone();
        c.sampled_at = sampled_at;
        c.online = st.location_offline(id).is_none();
        if let Some((free, total, disk_id)) = disk.clone() {
            (c.disk_free, c.disk_total, c.disk_id) = (Some(free), Some(total), disk_id);
        }
        out.push(c);
    }
    if let Some(Some((free, total, disk_id))) = disks.last().cloned() {
        (all.disk_free, all.disk_total, all.disk_id) = (Some(free), Some(total), disk_id);
    }
    let took = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    all.took_ms = took;
    out.push(all);
    for c in &mut out {
        c.took_ms = took;
    }
    write_capacity(st, &out, at).await?;
    Ok(out)
}

async fn write_capacity(st: &AppState, rows: &[Capacity], at: i64) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        for c in rows {
            for span in [CAPACITY_SPAN, HOUR, DAY] {
                sqlx::query(
                    "INSERT OR REPLACE INTO usage_capacity (location_id, span, at, sampled_at, live_bytes, trash_bytes, version_bytes,
                       store_bytes, folder_bytes, folder_version_bytes, pending_deletes, temp_bytes, backup_bytes, replica_bytes,
                       disk_free, disk_total, disk_id, online, took_ms)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&c.location_id)
                .bind(span)
                .bind(bucket(at, span))
                .bind(c.sampled_at)
                .bind(c.live_bytes)
                .bind(c.trash_bytes)
                .bind(c.version_bytes)
                .bind(c.store_bytes)
                .bind(c.folder_bytes)
                .bind(c.folder_version_bytes)
                .bind(c.pending_deletes)
                .bind(c.temp_bytes)
                .bind(c.backup_bytes)
                .bind(c.replica_bytes)
                .bind(c.disk_free)
                .bind(c.disk_total)
                .bind(&c.disk_id)
                .bind(c.online)
                .bind(c.took_ms)
                .execute(&mut *tx)
                .await?;
            }
        }
        AppResult::Ok(())
    }
    .await;
    crate::db::settle(tx, res).await
}

// ───────────── Retention ─────────────

/// Deletes the rows each tier no longer keeps, and those of deleted locations
pub async fn prune(st: &AppState, now: i64) -> AppResult<u64> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        let mut n = 0;
        for (span, keep) in RETENTION {
            for table in ["usage_capacity", "usage_ops"] {
                n += sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table} WHERE span = ? AND at < ?")))
                    .bind(span)
                    .bind(now - keep)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected();
            }
        }
        for table in ["usage_capacity", "usage_ops"] {
            n += sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table} WHERE location_id <> '' AND location_id NOT IN (SELECT id FROM storage_locations)")))
                .execute(&mut *tx)
                .await?
                .rows_affected();
        }
        AppResult::Ok(n)
    }
    .await;
    crate::db::settle(tx, res).await
}

// ───────────── Folder spaces ─────────────

/// Folder spaces' folders and the locations they are on, to count their files' operations with the right location
#[derive(Default)]
pub struct FolderMap {
    /// The folder as recorded, as the system resolves it, and its location ('' for none)
    folders: Vec<(PathBuf, PathBuf, String)>,
    loaded: Option<Instant>,
}

impl FolderMap {
    fn find(&self, path: &Path) -> Option<&str> {
        // The deepest folder that holds it: a folder space may be inside another's folder
        self.folders
            .iter()
            .filter(|(raw, real, _)| path.starts_with(raw) || path.starts_with(real))
            .max_by_key(|(raw, _, _)| raw.as_os_str().len())
            .map(|(_, _, loc)| loc.as_str())
    }
}

/// Looked up again at most this often when a folder isn't known (a space made since)
const RELOAD_AFTER: Duration = Duration::from_secs(10);

/// The storage location of the folder space holding `path` (its folder, or an item in it); '' when it is on none or
/// isn't known
pub async fn folder_location(st: &AppState, path: &Path) -> String {
    {
        let map = st.part::<Memory>().meters.folders.lock().unwrap();
        if let Some(loc) = map.find(path) {
            return loc.to_string();
        }
        if map.loaded.is_some_and(|t| t.elapsed() < RELOAD_AFTER) {
            return String::new();
        }
    }
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT source_path, COALESCE(location_id, '') FROM drives WHERE mode = 'folder' AND source_path IS NOT NULL")
        .fetch_all(&st.db)
        .await
        .unwrap_or_default();
    // As the system resolves them (links, `/proc/self/fd` paths on Linux): a disk that doesn't answer keeps the path
    let resolve = tokio::task::spawn_blocking(move || {
        rows.into_iter()
            .map(|(raw, loc)| {
                let raw = PathBuf::from(raw);
                let real = std::fs::canonicalize(&raw).unwrap_or_else(|_| raw.clone());
                (raw, real, loc)
            })
            .collect::<Vec<_>>()
    });
    let folders = tokio::time::timeout(Duration::from_secs(3), resolve).await.ok().and_then(Result::ok).unwrap_or_default();
    let mut map = st.part::<Memory>().meters.folders.lock().unwrap();
    *map = FolderMap { folders, loaded: Some(Instant::now()) };
    map.find(path).unwrap_or_default().to_string()
}

/// The path of an item of a folder space as the system resolves it: on Linux items are reached through
/// `/proc/self/fd/<folder>/<name>` (beneath.rs), whose folder is read back
pub fn real_path(path: &Path) -> PathBuf {
    #[cfg(target_os = "linux")]
    if path.starts_with("/proc/self/fd")
        && let (Some(dir), Some(name)) = (path.parent(), path.file_name())
        && let Ok(real) = std::fs::read_link(dir)
    {
        return real.join(name);
    }
    path.to_path_buf()
}

/// Counts a finished operation on a folder space's files (they don't go through a storage backend)
pub fn record_folder(st: &AppState, location: &str, op: Op, started: Instant, ok: bool, bytes: u64) {
    let outcome = if ok { super::meter::Outcome::Ok } else { super::meter::Outcome::Error };
    st.part::<Memory>().meters.record(location, op, super::meter::current_work(), started.elapsed(), outcome, if ok { bytes } else { 0 });
}

/// A read of a folder space's file: timed until it can be read, the bytes counted as they are read
pub async fn folder_read(
    st: &AppState,
    path: &Path,
    open: impl std::future::Future<Output = std::io::Result<crate::storage::BoxReader>>,
) -> std::io::Result<crate::storage::BoxReader> {
    let location = folder_location(st, &real_path(path)).await;
    let reader = st.part::<Memory>().meters.timed(&location, Op::Read, 0, open).await?;
    Ok(st.part::<Memory>().meters.count_reads(&location, reader))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{testutil, usage::meter::Work};
    use axum::{Json, extract::State};

    fn row<'a>(rows: &'a [Capacity], id: &str) -> &'a Capacity {
        rows.iter().find(|c| c.location_id == id).unwrap()
    }

    #[test]
    fn periods_start_at_multiples_of_their_span_in_utc() {
        assert_eq!(bucket(1_000_000, OPS_SPAN), 999_900);
        assert_eq!(bucket(999_900, OPS_SPAN), 999_900);
        assert_eq!(bucket(86_399, DAY), 0);
        assert_eq!(bucket(86_400 * 3 + 5, DAY), 86_400 * 3, "UTC days, whatever the server's time zone");
        assert_eq!(bucket(-1, HOUR), -HOUR);
    }

    #[tokio::test]
    async fn capacity_counts_identical_content_once_and_keeps_the_trash_and_versions_apart() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let root = admin.root_id.clone().unwrap();
        env.upload(&admin, &root, "a.txt", b"abc").await;
        env.upload(&admin, &root, "b.txt", b"abc").await;
        let gone = env.upload(&admin, &root, "c.txt", b"hello").await;
        let req = serde_json::from_value(serde_json::json!({ "ids": [gone] })).unwrap();
        let _ = crate::nodes::trash(State(env.st.clone()), admin.clone(), Json(req)).await.unwrap();
        // An earlier version of a.txt, holding the same content once more
        let hash = crate::util::sha256_hex(b"abc");
        sqlx::query("INSERT INTO node_versions (id, node_id, blob_hash, size, modified_at, created_at) SELECT 'v1', id, ?, 3, 0, 0 FROM nodes WHERE name = 'a.txt'")
            .bind(&hash)
            .execute(&env.st.db)
            .await
            .unwrap();
        // Another location on the same disk, and one whose capacity can't be known
        let nas = env.dir.join("nas");
        std::fs::create_dir_all(&nas).unwrap();
        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, created_at) VALUES ('nas', 'NAS', 'local', ?, 0), ('bucket', 'Bucket', 's3', '{}', 0)")
            .bind(serde_json::json!({ "path": nas.to_string_lossy() }).to_string())
            .execute(&env.st.db)
            .await
            .unwrap();
        let rows = sample_capacity(&env.st, now()).await.unwrap();
        let local = row(&rows, "local");
        assert_eq!((local.live_bytes, local.trash_bytes, local.version_bytes), (6, 5, 3), "as people see them: each file counts");
        assert_eq!(local.store_bytes, 8, "stored: identical content once, the trash's too");
        assert_eq!((local.folder_bytes, local.folder_version_bytes, local.pending_deletes), (0, 0, 0));
        assert_eq!((local.backup_bytes, local.replica_bytes), (None, None), "no copies are kept there");
        assert!(local.online);
        let all = row(&rows, "");
        assert_eq!((all.live_bytes, all.trash_bytes, all.version_bytes, all.store_bytes), (6, 5, 3, 8));
        if cfg!(any(unix, windows)) {
            assert!(local.disk_total.unwrap() >= local.disk_free.unwrap());
            assert!(local.disk_id.is_some());
            assert_eq!(row(&rows, "nas").disk_id, local.disk_id, "the same disk: never counted twice");
            assert!(all.disk_total.is_some(), "the data folder's disk");
        }
        assert!(!row(&rows, "nas").online, "not connected");
        let s3 = row(&rows, "bucket");
        assert_eq!((s3.disk_free, s3.disk_total, s3.store_bytes), (None, None, 0), "unknown, not zero");
        // Written into every tier
        let tiers: Vec<(i64,)> = sqlx::query_as("SELECT span FROM usage_capacity WHERE location_id = 'local' ORDER BY span").fetch_all(&env.st.db).await.unwrap();
        assert_eq!(tiers, [(CAPACITY_SPAN,), (HOUR,), (DAY,)]);
        // The last sample of a day stands for it
        env.upload(&admin, &root, "d.txt", b"more!").await;
        let later = bucket(now(), DAY) + DAY - CAPACITY_SPAN;
        sample_capacity(&env.st, later).await.unwrap();
        let (live, days): (i64, i64) = sqlx::query_as(
            "SELECT live_bytes, (SELECT COUNT(*) FROM usage_capacity WHERE location_id = 'local' AND span = ?1)
             FROM usage_capacity WHERE location_id = 'local' AND span = ?1 AND at = ?2",
        )
        .bind(DAY)
        .bind(bucket(later, DAY))
        .fetch_one(&env.st.db)
        .await
        .unwrap();
        assert_eq!((live, days), (11, 1));
    }

    #[tokio::test]
    async fn folder_spaces_are_counted_on_their_location_and_their_files_measured() {
        let env = testutil::folders_env().await;
        let admin = env.admin().await;
        let (company,): (String,) = sqlx::query_as("SELECT root_id FROM drives WHERE kind = 'company'").fetch_one(&env.st.db).await.unwrap();
        let id = env.upload(&admin, &company, "a.txt", b"12345").await;
        // A folder an administrator chose: on no location
        let chosen = env.folder_space("Scans").await;
        testutil::write_old(&chosen.dir.join("scan.txt"), b"1234567");
        crate::folders::scan(&env.st, &chosen.drive).await.unwrap();
        let rows = sample_capacity(&env.st, now()).await.unwrap();
        let local = row(&rows, "local");
        assert_eq!((local.live_bytes, local.folder_bytes, local.store_bytes), (5, 5, 0));
        assert_eq!(row(&rows, "").folder_bytes, 12, "everything together has the chosen folder too");
        // The upload stored in the folder, and reading it back, count on the built-in location
        let read = |id: String| {
            let st = env.st.clone();
            async move {
                let node = crate::tree::get_node(&mut st.db.acquire().await.unwrap(), &id).await.unwrap().unwrap();
                let mut reader = crate::files::Source::of(&node).unwrap().open(&st, 0, node.size as u64).await.unwrap();
                let mut got = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut got).await.unwrap();
                got.len()
            }
        };
        assert_eq!(read(id).await, 5);
        let (scan,): (String,) = sqlx::query_as("SELECT id FROM nodes WHERE name = 'scan.txt'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(read(scan).await, 7);
        let w = env.st.part::<Memory>().meters.take();
        let local = &w["local"];
        let write = &local[&(Op::Write, Work::Foreground)];
        assert_eq!((write.count, write.bytes), (1, 5));
        let read = &local[&(Op::Read, Work::Foreground)];
        assert_eq!((read.count, read.bytes), (1, 5));
        assert_eq!(w[""][&(Op::Read, Work::Foreground)].bytes, 7, "a chosen folder is on no location");
        assert!(env.st.part::<Memory>().meters.active().is_empty());
    }

    fn window(count: u64, bytes: u64, durations_us: &[u64]) -> Window {
        let mut w = Window { count, bytes, ..Default::default() };
        for &us in durations_us {
            w.hist.add(us);
            w.total_us += us;
            w.max_us = w.max_us.max(us);
        }
        w
    }

    type Row = (i64, i64, i64, i64, i64, String);

    async fn ops_rows(env: &testutil::TestEnv, span: i64) -> Vec<Row> {
        sqlx::query_as("SELECT at, count, errors, bytes, max_us, hist FROM usage_ops WHERE span = ? ORDER BY at").bind(span).fetch_all(&env.st.db).await.unwrap()
    }

    #[tokio::test]
    async fn counters_add_up_in_hours_and_days_and_old_rows_go() {
        let env = testutil::env().await;
        let day = bucket(now(), DAY) - DAY;
        let one = |w: Window| -> Windows {
            let mut all = Windows::new();
            all.entry("local".into()).or_default().insert((Op::Read, Work::Foreground), w);
            all
        };
        // Two periods of the same hour, and one of the next hour
        write_ops(&env.st, &one(window(2, 100, &[1_000, 3_000])), day).await.unwrap();
        let mut failed = window(1, 50, &[40_000]);
        failed.errors = 1;
        write_ops(&env.st, &one(failed), day + OPS_SPAN).await.unwrap();
        write_ops(&env.st, &one(window(1, 10, &[2_000])), day + HOUR).await.unwrap();
        write_ops(&env.st, &Windows::new(), day + HOUR).await.unwrap();
        let fine = ops_rows(&env, OPS_SPAN).await;
        assert_eq!(fine.iter().map(|r| (r.0 - day, r.1)).collect::<Vec<_>>(), [(0, 2), (OPS_SPAN, 1), (HOUR, 1)]);
        let hours = ops_rows(&env, HOUR).await;
        assert_eq!(hours.iter().map(|r| (r.0 - day, r.1, r.2, r.3, r.4)).collect::<Vec<_>>(), [(0, 3, 1, 150, 40_000), (HOUR, 1, 0, 10, 2_000)]);
        let days = ops_rows(&env, DAY).await;
        assert_eq!(days.len(), 1);
        let merged = Hist::decode(&days[0].5);
        assert_eq!(merged.count(), 4, "the histograms merged");
        // p50 of 1, 2, 3 and 40 ms is about 2 ms
        let p50 = Window { hist: merged, max_us: 40_000, ..Default::default() }.percentile(0.5).unwrap();
        assert!((1_800.0..2_200.0).contains(&p50), "{p50}");

        // Old rows of each tier go, and those of deleted locations
        let t = day + 2 * DAY;
        sqlx::query(
            "INSERT INTO usage_ops (location_id, span, at, op, work, count, errors, timeouts, bytes, total_us, max_us, hist)
             VALUES ('gone', ?, ?, 'read', 'foreground', 1, 0, 0, 0, 0, 0, '')",
        )
        .bind(DAY)
        .bind(t)
        .execute(&env.st.db)
        .await
        .unwrap();
        sample_capacity(&env.st, t).await.unwrap();
        let removed = prune(&env.st, t + DAY + OPS_SPAN).await.unwrap();
        assert_eq!(removed, 4, "the fine rows more than two days old, and the deleted location's");
        assert!(ops_rows(&env, OPS_SPAN).await.is_empty());
        assert_eq!(ops_rows(&env, HOUR).await.len(), 2, "hours are kept 45 days");
        let (capacity,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM usage_capacity").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(capacity, 6, "the recent capacity samples of 'local' and '' stay");
        assert_eq!(prune(&env.st, t + 400 * DAY + 1).await.unwrap(), 9, "everything, in the end");
    }
}
