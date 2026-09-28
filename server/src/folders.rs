//! Folder spaces: a space whose files are an ordinary folder on the server.
//!
//! The folder is the source of truth; the nodes of the space are an index of it. Scanning brings the index up to date
//! with changes made anywhere (the web interface, SMB, rsync, a scanner):
//! - an item is found by its path below the space's folder and recognised by its file system identity (device and
//!   inode), so a path that disappears while the same identity shows up elsewhere is a move, and the node (with its
//!   shares, permissions and favourites) moves along;
//! - a file counts as changed when its size or modification time changed; contents are never read while scanning;
//! - files changed within the last few seconds are left for the next scan (they may still be being written);
//! - temporary files of office programs and operating systems are ignored; names that aren't valid text, symbolic
//!   links and other file systems mounted inside are skipped and listed in the scan report.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use serde::Serialize;
use sqlx::SqliteConnection;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
    tree::{self, Drive, Node},
    util::{guess_mime, new_id, now},
};

/// Files changed more recently than this are still being written, maybe: they are indexed by a later scan
const SETTLE_SECONDS: i64 = 10;
/// Database changes per transaction: other changes get a turn in between
const BATCH: usize = 500;
/// Skipped items listed in a scan report
const MAX_REPORTED: usize = 200;
/// A file in the folder of every folder space once it has been scanned: a folder without it that is suddenly empty is
/// a disk or share that isn't mounted, not one whose items were all deleted
const MARKER: &str = ".thirtyfile-space";

/// One item found in the folder
#[derive(Debug, Clone)]
struct Entry {
    /// Path below the space's folder, with '/'
    rel: String,
    parent_rel: String,
    name: String,
    is_dir: bool,
    dev: i64,
    ino: i64,
    size: i64,
    mtime_ns: i64,
    /// Changed within the last few seconds
    settling: bool,
}

/// What a scan did, kept with the space and shown in the Control panel
#[derive(Debug, Default, Clone, Serialize, serde::Deserialize)]
pub struct ScanReport {
    pub at: i64,
    pub added: usize,
    pub changed: usize,
    pub moved: usize,
    pub removed: usize,
    /// Items that couldn't be indexed, with the reason
    pub skipped: Vec<String>,
    pub error: Option<String>,
    /// How long reading the folder and updating the index took (milliseconds)
    #[serde(default)]
    pub read_ms: u64,
    #[serde(default)]
    pub index_ms: u64,
    /// Folders that couldn't be read (paths below the space's folder): what the index has in them stays
    #[serde(skip)]
    unreadable: Vec<String>,
    /// What changes left behind when they stopped halfway (`fsops::is_leftover`)
    #[serde(skip)]
    leftovers: Vec<String>,
}

/// A scan in progress, shown in the Control panel
#[derive(Debug, Clone, Serialize)]
pub struct ScanProgress {
    /// "reading" the folder, then "indexing" the changes
    pub phase: &'static str,
    /// Items read so far
    pub found: usize,
    /// Index changes made so far, and in all
    pub done: usize,
    pub total: usize,
    pub started_at: i64,
}

fn progress_map() -> &'static Mutex<HashMap<String, ScanProgress>> {
    static MAP: OnceLock<Mutex<HashMap<String, ScanProgress>>> = OnceLock::new();
    MAP.get_or_init(Default::default)
}

/// The scan of this space running now, if any
pub fn progress(drive_id: &str) -> Option<ScanProgress> {
    progress_map().lock().unwrap().get(drive_id).cloned()
}

fn set_progress(drive_id: &str, f: impl FnOnce(&mut ScanProgress)) {
    let mut map = progress_map().lock().unwrap();
    let p = map
        .entry(drive_id.to_string())
        .or_insert_with(|| ScanProgress { phase: "reading", found: 0, done: 0, total: 0, started_at: now() });
    f(p);
}

/// Removes the progress entry when a scan ends, however it ends
struct ProgressGuard(String);

impl Drop for ProgressGuard {
    fn drop(&mut self) {
        progress_map().lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0);
    }
}

impl ScanReport {
    fn any_change(&self) -> bool {
        self.added + self.changed + self.moved + self.removed > 0
    }
    fn skip(&mut self, what: String) {
        if self.skipped.len() < MAX_REPORTED {
            self.skipped.push(what);
        }
    }
}

/// Names that are never indexed: temporary and bookkeeping files of office programs, operating systems and ThirtyFile
pub(crate) fn ignored(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    name.starts_with("~$")
        || name.starts_with(".~lock")
        || lower.starts_with(".thirtyfile-")
        || lower.ends_with(".part")
        || lower.ends_with(".crdownload")
        || matches!(lower.as_str(), "thumbs.db" | ".ds_store" | "desktop.ini" | ".smbdelete")
        || lower.starts_with(".smbdelete")
}

pub(crate) fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos() as i64).unwrap_or(0)
}

#[cfg(unix)]
pub(crate) fn identity(meta: &std::fs::Metadata) -> (i64, i64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev() as i64, meta.ino() as i64)
}

#[cfg(not(unix))]
pub(crate) fn identity(_: &std::fs::Metadata) -> (i64, i64) {
    // Without inodes, a move is seen as removing and adding
    (0, 0)
}

/// Reads the folder: every item below `root` (or only the items directly in `only`, a path below it), parents
/// before their contents. Runs on a blocking thread.
fn walk(root: &Path, only: Option<&str>, report: &mut ScanReport, found: Option<&dyn Fn(usize)>) -> std::io::Result<Vec<Entry>> {
    let root_meta = std::fs::metadata(root)?;
    if !root_meta.is_dir() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "The folder doesn't exist"));
    }
    let (root_dev, _) = identity(&root_meta);
    // Each folder is reached without following a symbolic link on the way (one could replace a folder meanwhile)
    let pinned = crate::beneath::Pinned::root(root)?;
    let settle_after = (now() - SETTLE_SECONDS) as i128 * 1_000_000_000;
    let mut out = Vec::new();
    let mut queue = std::collections::VecDeque::from([only.unwrap_or("").to_string()]);
    let mut first = true;
    while let Some(dir_rel) = queue.pop_front() {
        let asked_for = std::mem::take(&mut first);
        let dir = if dir_rel.is_empty() { Ok(pinned.clone()) } else { pinned.join(&dir_rel).and_then(|d| d.dir()) };
        // The folder stays open while its items are read (their paths lead through it)
        let (_open, read) = match dir.and_then(|d| std::fs::read_dir(d.as_path()).map(|r| (d, r))) {
            Ok(r) => r,
            // The folder asked for can't be read at all: nothing can be concluded about what it holds
            Err(e) if asked_for => return Err(e),
            Err(e) => {
                report.skip(format!("{}: {e}", if dir_rel.is_empty() { "/" } else { &dir_rel }));
                report.unreadable.push(dir_rel);
                continue;
            }
        };
        let mut children = Vec::new();
        for item in read.flatten() {
            let Ok(name) = item.file_name().into_string() else {
                report.skip(format!("{}: a name that isn't valid text", show(&dir_rel, &item.file_name().to_string_lossy())));
                continue;
            };
            let rel = if dir_rel.is_empty() { name.clone() } else { format!("{dir_rel}/{name}") };
            if ignored(&name) {
                if crate::fsops::is_leftover(&name) {
                    report.leftovers.push(rel);
                }
                continue;
            }
            if name.chars().any(|c| c.is_control()) {
                report.skip(format!("{rel}: the name contains control characters"));
                continue;
            }
            let Ok(meta) = std::fs::symlink_metadata(item.path()) else { continue };
            if meta.file_type().is_symlink() {
                report.skip(format!("{rel}: a symbolic link"));
                continue;
            }
            if !meta.is_dir() && !meta.is_file() {
                continue;
            }
            let (dev, ino) = identity(&meta);
            if cfg!(unix) && dev != root_dev {
                report.skip(format!("{rel}: another file system mounted inside"));
                continue;
            }
            let mtime = mtime_ns(&meta);
            children.push(Entry {
                rel: rel.clone(),
                parent_rel: dir_rel.clone(),
                name,
                is_dir: meta.is_dir(),
                dev,
                ino,
                size: if meta.is_dir() { 0 } else { meta.len() as i64 },
                mtime_ns: mtime,
                settling: !meta.is_dir() && (mtime as i128) > settle_after,
            });
        }
        children.sort_by(|a, b| a.name.cmp(&b.name));
        for c in children {
            if c.is_dir && only.is_none() {
                queue.push_back(c.rel.clone());
            }
            out.push(c);
        }
        if let Some(found) = found {
            found(out.len());
        }
    }
    Ok(out)
}

fn show(dir_rel: &str, name: &str) -> String {
    if dir_rel.is_empty() { name.to_string() } else { format!("{dir_rel}/{name}") }
}

/// A node as the index knows it
#[derive(Debug, Clone, sqlx::FromRow)]
struct Indexed {
    id: String,
    parent_id: Option<String>,
    kind: String,
    fs_path: Option<String>,
    fs_dev: Option<i64>,
    fs_ino: Option<i64>,
    fs_size: Option<i64>,
    fs_mtime_ns: Option<i64>,
}

enum Op {
    Create { id: String, parent: String, e: Entry },
    Update { id: String, e: Entry },
    /// First step of a move: a name no other item has, so moves in any order can't collide
    Park { id: String },
    Move { id: String, parent: String, e: Entry },
    Remove { id: String },
}

/// Scans a whole folder space and brings its index up to date
pub async fn scan(st: &AppState, drive_id: &str) -> AppResult<ScanReport> {
    let lock = drive_lock(drive_id);
    let _scanning = lock.lock().await;
    scan_locked(st, drive_id).await
}

/// `scan`, with the space's scan lock already held
async fn scan_locked(st: &AppState, drive_id: &str) -> AppResult<ScanReport> {
    let drive = folder_drive(st, drive_id).await?;
    let root = PathBuf::from(drive.source_path.clone().unwrap_or_default());
    let mut report = ScanReport { at: now(), ..Default::default() };
    set_progress(drive_id, |_| {});
    let _progress = ProgressGuard(drive_id.to_string());
    let started = std::time::Instant::now();
    let walked = {
        let (root, id) = (root.clone(), drive_id.to_string());
        let mut r = ScanReport::default();
        let res = tokio::task::spawn_blocking(move || {
            let found = |n: usize| set_progress(&id, |p| p.found = n);
            walk(&root, None, &mut r, Some(&found)).map(|e| (e, r))
        })
        .await
        .map_err(AppError::internal)?;
        res.map(|(e, r)| {
            report.skipped = r.skipped;
            report.unreadable = r.unreadable;
            report.leftovers = r.leftovers;
            e
        })
    };
    let entries = match walked {
        Ok(e) => e,
        Err(e) => {
            // The folder is gone (an unmounted disk, say): keep the index rather than removing everything
            report.error = Some(format!("Can't read {}: {e}", root.display()));
            save_report(st, &drive, &report).await?;
            return Ok(report);
        }
    };
    let indexed: Vec<Indexed> = sqlx::query_as(
        "SELECT id, parent_id, kind, fs_path, fs_dev, fs_ino, fs_size, fs_mtime_ns FROM nodes WHERE drive_id = ? AND trashed_at IS NULL",
    )
    .bind(&drive.id)
    .fetch_all(&st.db)
    .await?;
    report.read_ms = started.elapsed().as_millis() as u64;
    // An empty folder where the index has items is what a disk or share that isn't mounted looks like (its mount
    // point is an empty folder): only a folder that has the marker is really empty
    let marker = crate::beneath::Pinned::root(&root).and_then(|r| r.join(MARKER));
    let marked = marker.as_ref().is_ok_and(|m| std::fs::symlink_metadata(m.as_path()).is_ok_and(|x| x.is_file()));
    let has_items = indexed.iter().any(|n| n.fs_path.as_deref().is_some_and(|p| !p.is_empty()));
    if entries.is_empty() && has_items && !marked {
        report.error = Some(format!(
            "{} is empty, but the space still has items: if it is on a disk or network share that isn't mounted, mount it and check again. To empty the space, delete its items in ThirtyFile.",
            root.display()
        ));
        save_report(st, &drive, &report).await?;
        return Ok(report);
    }
    if !marked
        && let Err(e) = marker.and_then(|m| crate::beneath::write_new(&m, drive.id.as_bytes()))
    {
        tracing::debug!("Couldn't write the marker file in {}: {e}", root.display());
    }
    let indexing = std::time::Instant::now();
    let ops = plan(&drive, &indexed, &entries, true, &mut report);
    set_progress(drive_id, |p| {
        p.phase = "indexing";
        p.total = ops.len();
    });
    apply(st, &drive, ops).await?;
    crate::fsops::clean_trash(st, &drive.id, &root).await?;
    let leftovers: Vec<crate::beneath::Pinned> =
        crate::beneath::Pinned::root(&root).map(|r| report.leftovers.iter().filter_map(|rel| r.join(rel).ok()).collect()).unwrap_or_default();
    if !leftovers.is_empty() {
        tokio::task::spawn_blocking(move || crate::fsops::clean_leftovers(leftovers, crate::fsops::TRASH_GRACE)).await.map_err(AppError::internal)?;
    }
    crate::versions::clean_folder(st, &drive.id, &root).await?;
    report.index_ms = indexing.elapsed().as_millis() as u64;
    if report.read_ms + report.index_ms > 10_000 {
        tracing::info!(
            "Scanned the folder space {}: {} items read in {} s, index updated in {} s",
            drive.name,
            entries.len(),
            report.read_ms / 1000,
            report.index_ms / 1000
        );
    }
    finish(st, &drive, &report).await?;
    Ok(report)
}

/// Brings one folder of a folder space up to date when it is opened: new and changed items right away. Anything
/// that disappeared may have moved elsewhere, which only a scan of the whole space can tell, so that runs in the
/// background.
pub async fn sync_folder(st: &AppState, folder: &Node) {
    if let Err(e) = try_sync_folder(st, folder).await {
        tracing::warn!("Couldn't check the folder \"{}\" for changes: {}", folder.name, e.message);
    }
}

async fn try_sync_folder(st: &AppState, folder: &Node) -> AppResult<()> {
    let (Some(rel), true) = (folder.fs_path.clone(), folder.is_folder()) else { return Ok(()) };
    let drive = folder_drive(st, folder.drive()).await?;
    // A full scan running now covers this folder too
    let lock = drive_lock(&drive.id);
    let Ok(_scanning) = lock.try_lock() else { return Ok(()) };
    let root = PathBuf::from(drive.source_path.clone().unwrap_or_default());
    let mut report = ScanReport::default();
    let entries = {
        let rel = rel.clone();
        let mut r = ScanReport::default();
        let res = tokio::task::spawn_blocking(move || walk(&root, Some(&rel), &mut r, None)).await.map_err(AppError::internal)?;
        match res {
            Ok(e) => e,
            Err(_) => return Ok(()),
        }
    };
    let mut indexed: Vec<Indexed> = sqlx::query_as(
        "SELECT id, parent_id, kind, fs_path, fs_dev, fs_ino, fs_size, fs_mtime_ns FROM nodes WHERE (parent_id = ?1 OR id = ?1) AND trashed_at IS NULL",
    )
    .bind(&folder.id)
    .fetch_all(&st.db)
    .await?;
    // New items that already exist elsewhere in the space were moved here: leave them to the full scan
    let known: HashSet<&str> = indexed.iter().filter_map(|n| n.fs_path.as_deref()).collect();
    let mut moved_in = false;
    for e in &entries {
        if known.contains(e.rel.as_str()) || e.ino == 0 {
            continue;
        }
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE drive_id = ? AND fs_dev = ? AND fs_ino = ? AND trashed_at IS NULL")
            .bind(&drive.id)
            .bind(e.dev)
            .bind(e.ino)
            .fetch_one(&st.db)
            .await?;
        moved_in |= n > 0;
    }
    // Paths above this folder aren't part of this look: the planner must not treat them as gone
    indexed.retain(|n| n.parent_id.as_deref() == Some(folder.id.as_str()) || n.id == folder.id);
    let ops = plan(&drive, &indexed, &entries, false, &mut report);
    let gone = ops.iter().any(|op| matches!(op, Op::Remove { .. })) || moved_in || report.removed > 0;
    // Removals wait for the full scan (the item may have moved elsewhere), except where the path now holds the other
    // kind (a file replaced by a folder of the same name): the new item needs its place
    let present: HashSet<&str> = entries.iter().map(|e| e.rel.as_str()).collect();
    let replaced: HashSet<&str> =
        indexed.iter().filter(|n| n.fs_path.as_deref().is_some_and(|p| present.contains(p))).map(|n| n.id.as_str()).collect();
    let ops: Vec<Op> = if moved_in {
        // Something came from elsewhere: the full scan works out the whole picture
        Vec::new()
    } else {
        ops.into_iter().filter(|op| !matches!(op, Op::Remove { id } if !replaced.contains(id.as_str()))).collect()
    };
    let changed = !ops.is_empty();
    if let Err(e) = apply(st, &drive, ops).await {
        drop(_scanning);
        scan_later(st, &drive.id);
        return Err(e);
    }
    if changed {
        refresh_usage(st, &drive).await?;
    }
    drop(_scanning);
    if gone {
        scan_later(st, &drive.id);
    }
    Ok(())
}

/// Folder spaces were added, changed or removed: file system watching follows right away
pub fn spaces_changed() {
    #[cfg(target_os = "linux")]
    crate::watch::spaces_changed();
}

/// Starts a full scan in the background unless one is running. The lock is taken before the task starts, so a scan
/// asked for afterwards waits for this one instead of possibly running first
pub fn scan_later(st: &AppState, drive_id: &str) {
    let Ok(scanning) = drive_lock(drive_id).try_lock_owned() else { return };
    let (st, id) = (st.clone(), drive_id.to_string());
    tokio::spawn(async move {
        let _scanning = scanning;
        if let Err(e) = scan_locked(&st, &id).await {
            tracing::warn!("Scanning a folder space failed: {}", e.message);
        }
    });
}

/// Scans every folder space whose last scan is older than the interval set in the Control panel (0 = never)
pub fn spawn_scanner(st: AppState) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let minutes = st.system.read().unwrap().scan_minutes;
            if minutes <= 0 {
                continue;
            }
            let due: Vec<(String,)> = match sqlx::query_as(
                "SELECT id FROM drives WHERE mode = 'folder' AND disabled = 0 AND COALESCE(last_scan_at, 0) <= ?",
            )
            .bind(now() - minutes * 60)
            .fetch_all(&st.db)
            .await
            {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("Couldn't list folder spaces to scan: {e}");
                    continue;
                }
            };
            for (id,) in due {
                if let Err(e) = scan(&st, &id).await {
                    tracing::warn!("Scanning a folder space failed: {}", e.message);
                }
            }
        }
    });
}

pub(crate) fn drive_lock(drive_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    LOCKS.get_or_init(Default::default).lock().unwrap().entry(drive_id.to_string()).or_default().clone()
}

async fn folder_drive(st: &AppState, drive_id: &str) -> AppResult<Drive> {
    let drive = tree::get_drive(&mut *st.db.acquire().await?, drive_id).await?.ok_or_else(|| AppError::not_found("Space not found"))?;
    if !drive.is_folder() {
        return Err(AppError::bad_request("This space doesn't show a folder"));
    }
    Ok(drive)
}

/// Works out what to change. `full`: the entries are the whole folder, so indexed items not found were removed.
fn plan(drive: &Drive, indexed: &[Indexed], entries: &[Entry], full: bool, report: &mut ScanReport) -> Vec<Op> {
    let by_path: HashMap<&str, &Indexed> = indexed.iter().filter_map(|n| n.fs_path.as_deref().map(|p| (p, n))).collect();
    // Path → kind of what the folder holds now (a map: looking entries up one by one made unchanged scans of large
    // folders quadratic, over five minutes for 200,000 items)
    let present: HashMap<&str, &str> = entries.iter().map(|e| (e.rel.as_str(), e.kind())).collect();
    // Indexed items whose path is gone (or now holds the other kind): candidates for a move, else removed
    let mut missing: HashMap<(i64, i64), &Indexed> = HashMap::new();
    let mut kind_changed = HashSet::new();
    for n in indexed {
        let Some(path) = n.fs_path.as_deref() else { continue };
        if path.is_empty() {
            continue;
        }
        let kind_differs = present.get(path).is_some_and(|k| *k != n.kind);
        if kind_differs {
            kind_changed.insert(n.id.clone());
        }
        if (!present.contains_key(path) || kind_differs)
            && let (Some(dev), Some(ino)) = (n.fs_dev, n.fs_ino)
            && ino != 0
        {
            missing.insert((dev, ino), n);
        }
    }

    let mut ids: HashMap<String, String> = HashMap::new();
    ids.insert(String::new(), drive.root_id.clone());
    for n in indexed {
        if let Some(p) = &n.fs_path
            && !kind_changed.contains(&n.id)
        {
            ids.insert(p.clone(), n.id.clone());
        }
    }
    let mut parks = Vec::new();
    let mut ops = Vec::new();
    let mut moved: HashSet<String> = HashSet::new();
    for id in &kind_changed {
        ops.push(Op::Remove { id: id.clone() });
        report.removed += 1;
    }
    for e in entries {
        let Some(parent) = ids.get(&e.parent_rel).cloned() else {
            report.skip(format!("{}: its folder couldn't be indexed", e.rel));
            continue;
        };
        match by_path.get(e.rel.as_str()) {
            Some(n) if !kind_changed.contains(&n.id) => {
                let changed = e.dev != n.fs_dev.unwrap_or(-1)
                    || e.ino != n.fs_ino.unwrap_or(-1)
                    || (!e.is_dir && (e.size != n.fs_size.unwrap_or(-1) || e.mtime_ns != n.fs_mtime_ns.unwrap_or(-1)));
                if changed && !e.settling {
                    ops.push(Op::Update { id: n.id.clone(), e: e.clone() });
                    if !e.is_dir {
                        report.changed += 1;
                    }
                }
            }
            _ => {
                if e.settling {
                    continue;
                }
                match missing.remove(&(e.dev, e.ino)).filter(|n| n.kind == e.kind()) {
                    Some(n) => {
                        parks.push(Op::Park { id: n.id.clone() });
                        ops.push(Op::Move { id: n.id.clone(), parent, e: e.clone() });
                        ids.insert(e.rel.clone(), n.id.clone());
                        moved.insert(n.id.clone());
                        report.moved += 1;
                    }
                    None => {
                        let id = new_id();
                        ids.insert(e.rel.clone(), id.clone());
                        ops.push(Op::Create { id, parent, e: e.clone() });
                        report.added += 1;
                    }
                }
            }
        }
    }
    if full {
        let by_id: HashMap<&str, &Indexed> = indexed.iter().map(|n| (n.id.as_str(), n)).collect();
        // Settling files keep their node: they are still there
        for n in indexed {
            let Some(path) = n.fs_path.as_deref() else { continue };
            if path.is_empty() || present.contains_key(path) || moved.contains(&n.id) || kind_changed.contains(&n.id) {
                continue;
            }
            // In a folder that couldn't be read: still there, as far as anyone can tell
            if report.unreadable.iter().any(|d| path.strip_prefix(d.as_str()).is_some_and(|rest| rest.starts_with('/'))) {
                continue;
            }
            // Only the topmost removed item: its contents go with it
            let parent_gone = n.parent_id.as_ref().and_then(|p| by_id.get(p.as_str())).is_some_and(|x| {
                x.fs_path.as_deref().is_some_and(|pp| !pp.is_empty() && !present.contains_key(pp) && !moved.contains(&x.id))
            });
            if !parent_gone {
                ops.push(Op::Remove { id: n.id.clone() });
            }
            report.removed += 1;
        }
    } else {
        for n in indexed {
            if let Some(path) = n.fs_path.as_deref()
                && !path.is_empty()
                && !present.contains_key(path)
                && !moved.contains(&n.id)
            {
                ops.push(Op::Remove { id: n.id.clone() });
            }
        }
    }
    // Moves first leave their place with a name nobody has, so the order of the moves doesn't matter
    parks.extend(ops);
    parks
}

impl Entry {
    fn kind(&self) -> &'static str {
        if self.is_dir { "folder" } else { "file" }
    }
    fn secs(&self) -> i64 {
        self.mtime_ns.div_euclid(1_000_000_000)
    }
}

async fn apply(st: &AppState, drive: &Drive, ops: Vec<Op>) -> AppResult<()> {
    let owner = sqlx::query_as::<_, (i64,)>("SELECT owner_id FROM nodes WHERE id = ?").bind(&drive.root_id).fetch_one(&st.db).await?.0;
    let mut ops = ops.into_iter().peekable();
    while ops.peek().is_some() {
        let _w = st.write_lock.lock().await;
        let mut tx = st.db.begin().await?;
        let mut n = 0;
        for op in ops.by_ref().take(BATCH) {
            apply_one(&mut tx, drive, owner, op).await?;
            n += 1;
        }
        tx.commit().await?;
        if progress_map().lock().unwrap().contains_key(&drive.id) {
            set_progress(&drive.id, |p| p.done += n);
        }
    }
    Ok(())
}

async fn apply_one(conn: &mut SqliteConnection, drive: &Drive, owner: i64, op: Op) -> AppResult<()> {
    match op {
        Op::Create { id, parent, e } => {
            sqlx::query(
                "INSERT INTO nodes (id, owner_id, parent_id, kind, name, size, mime, drive_id, created_at, updated_at,
                                    fs_path, fs_dev, fs_ino, fs_size, fs_mtime_ns)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(owner)
            .bind(&parent)
            .bind(e.kind())
            .bind(&e.name)
            .bind(e.size)
            .bind(if e.is_dir { String::new() } else { guess_mime(&e.name) })
            .bind(&drive.id)
            .bind(e.secs())
            .bind(e.secs())
            .bind(&e.rel)
            .bind(e.dev)
            .bind(e.ino)
            .bind(e.size)
            .bind(e.mtime_ns)
            .execute(&mut *conn)
            .await?;
        }
        Op::Update { id, e } => {
            // The time is the version the editors compare, so it only moves forward
            sqlx::query(
                "UPDATE nodes SET size = ?, updated_at = MAX(?, updated_at + 1), fs_dev = ?, fs_ino = ?, fs_size = ?, fs_mtime_ns = ? WHERE id = ?",
            )
            .bind(e.size)
            .bind(e.secs())
            .bind(e.dev)
            .bind(e.ino)
            .bind(e.size)
            .bind(e.mtime_ns)
            .bind(&id)
            .execute(&mut *conn)
            .await?;
        }
        Op::Park { id } => {
            sqlx::query("UPDATE nodes SET name = char(1) || id WHERE id = ?").bind(&id).execute(&mut *conn).await?;
        }
        Op::Move { id, parent, e } => {
            sqlx::query(
                "UPDATE nodes SET parent_id = ?, name = ?, mime = ?, size = ?, fs_path = ?, fs_dev = ?, fs_ino = ?, fs_size = ?, fs_mtime_ns = ?
                 WHERE id = ?",
            )
            .bind(&parent)
            .bind(&e.name)
            .bind(if e.is_dir { String::new() } else { guess_mime(&e.name) })
            .bind(e.size)
            .bind(&e.rel)
            .bind(e.dev)
            .bind(e.ino)
            .bind(e.size)
            .bind(e.mtime_ns)
            .bind(&id)
            .execute(&mut *conn)
            .await?;
        }
        Op::Remove { id } => {
            // Folder spaces keep no content in the store, so nothing is released there; grants, shares and favourites
            // of the removed items go with them
            if tree::get_node(&mut *conn, &id).await?.is_some() {
                tree::purge_subtree(&mut *conn, &id).await?;
            }
        }
    }
    Ok(())
}

/// Space used is what the folder holds, its trash included (like `tree::recompute_usage`)
async fn refresh_usage(st: &AppState, drive: &Drive) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    sqlx::query("UPDATE drives SET used_bytes = (SELECT COALESCE(SUM(size), 0) FROM nodes WHERE drive_id = ?1 AND kind = 'file') WHERE id = ?1")
        .bind(&drive.id)
        .execute(&st.db)
        .await?;
    Ok(())
}

async fn finish(st: &AppState, drive: &Drive, report: &ScanReport) -> AppResult<()> {
    refresh_usage(st, drive).await?;
    save_report(st, drive, report).await?;
    if report.any_change() {
        let _w = st.write_lock.lock().await;
        let detail = format!("{} added, {} changed, {} moved, {} removed", report.added, report.changed, report.moved, report.removed);
        sqlx::query("INSERT INTO activity (at, user_id, username, drive_id, node_id, node_name, action, detail) VALUES (?, NULL, '', ?, ?, '', 'scan', ?)")
            .bind(now())
            .bind(&drive.id)
            .bind(&drive.root_id)
            .bind(detail)
            .execute(&st.db)
            .await?;
    }
    Ok(())
}

async fn save_report(st: &AppState, drive: &Drive, report: &ScanReport) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    sqlx::query("UPDATE drives SET last_scan_at = ?, scan_report = ? WHERE id = ?")
        .bind(report.at)
        .bind(serde_json::to_string(report).unwrap())
        .bind(&drive.id)
        .execute(&st.db)
        .await?;
    Ok(())
}

/// Checks the folder a new folder space is to show: an existing folder, given as an absolute path, that isn't
/// ThirtyFile's own data or storage (or inside them)
pub fn check_source(st: &AppState, path: &str) -> AppResult<String> {
    let path = path.trim();
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(AppError::bad_request("Enter the folder's full path, for example /mnt/nas/shared"));
    }
    let real = std::fs::canonicalize(p).map_err(|_| AppError::bad_request("The folder doesn't exist on the server"))?;
    if !real.is_dir() {
        return Err(AppError::bad_request("The folder doesn't exist on the server"));
    }
    // (new spaces get their folders in the storage folder, which differs from `storage_dir` while 0.1's /data/blobs is
    // in use)
    for own in [Some(&st.data_dir), Some(&st.storage_dir), st.space_folders.as_ref()].into_iter().flatten() {
        if let Ok(own) = std::fs::canonicalize(own)
            && (real.starts_with(&own) || own.starts_with(&real))
        {
            return Err(AppError::bad_request("Choose a folder outside ThirtyFile's own data and storage folders"));
        }
    }
    let real = real.to_string_lossy().into_owned();
    // Windows: show C:\folder rather than the \\?\C:\folder form canonicalize returns
    Ok(match real.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => rest.to_owned(),
        _ => real,
    })
}

/// Creates the index root of a new folder space (called when the space is created)
pub async fn set_up(conn: &mut SqliteConnection, drive_id: &str, root_id: &str, source: &str) -> AppResult<()> {
    sqlx::query("UPDATE drives SET mode = 'folder', source_path = ? WHERE id = ?").bind(source).bind(drive_id).execute(&mut *conn).await?;
    sqlx::query("UPDATE nodes SET fs_path = '' WHERE id = ?").bind(root_id).execute(&mut *conn).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{self, write_old};
    use axum::extract::{Path as UrlPath, Query, State};

    #[tokio::test]
    async fn a_folder_space_follows_changes_made_on_the_server() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        let (dir, drive, root) = (space.dir.clone(), space.drive.clone(), space.root.clone());
        write_old(&dir.join("a.txt"), b"one");
        write_old(&dir.join("Sub").join("b.txt"), b"two");
        write_old(&dir.join("~$a.docx"), b"office lock file");
        std::fs::write(dir.join("fresh.txt"), b"still being written").unwrap();

        let r = scan(&env.st, &drive).await.unwrap();
        assert_eq!((r.added, r.removed), (3, 0), "{r:?}");
        assert!(env.node_at(&drive, "a.txt").await.is_some() && env.node_at(&drive, "Sub/b.txt").await.is_some());
        assert!(env.node_at(&drive, "~$a.docx").await.is_none(), "temporary files are ignored");
        assert!(env.node_at(&drive, "fresh.txt").await.is_none(), "a file still being written waits");
        let (used,): (i64,) = sqlx::query_as("SELECT used_bytes FROM drives WHERE id = ?").bind(&drive).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(used, 6);

        // Changed, moved and removed on the server
        let (b_id, _) = env.node_at(&drive, "Sub/b.txt").await.unwrap();
        write_old(&dir.join("a.txt"), b"one, longer");
        std::fs::rename(dir.join("Sub"), dir.join("Moved")).unwrap();
        write_old(&dir.join("gone.txt"), b"x");
        scan(&env.st, &drive).await.unwrap();
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        let r = scan(&env.st, &drive).await.unwrap();
        assert_eq!(env.node_at(&drive, "a.txt").await.unwrap().1, 11);
        let (moved_id, _) = env.node_at(&drive, "Moved/b.txt").await.unwrap();
        if cfg!(unix) {
            // Recognised by its inode: the same node, so shares and permissions stay
            assert_eq!(moved_id, b_id);
        }
        assert!(env.node_at(&drive, "Sub/b.txt").await.is_none() && env.node_at(&drive, "gone.txt").await.is_none(), "{r:?}");

        // Browsing works; a read-only space can't be changed from the web
        let a = env.node_at(&drive, "a.txt").await.unwrap().0;
        let res = crate::files::content(State(env.st.clone()), admin.clone(), UrlPath(a.clone()), Query(serde_json::from_value(serde_json::json!({})).unwrap()), axum::http::HeaderMap::new()).await.unwrap();
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"one, longer");
        assert!(tree::node_for(&mut env.st.db.acquire().await.unwrap(), &admin, &a, tree::Need::Write).await.is_ok());
        let req = serde_json::from_value(serde_json::json!({ "read_only": true })).unwrap();
        let _ = crate::drives::update(State(env.st.clone()), admin.clone(), UrlPath(drive.clone()), axum::Json(req)).await.unwrap();
        let err = tree::node_for(&mut env.st.db.acquire().await.unwrap(), &admin, &a, tree::Need::Write).await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::FORBIDDEN);

        // Opening a folder shows what was added there since
        write_old(&dir.join("new.txt"), b"new");
        let root_node = tree::get_node(&mut env.st.db.acquire().await.unwrap(), &root).await.unwrap().unwrap();
        sync_folder(&env.st, &root_node).await;
        assert!(env.node_at(&drive, "new.txt").await.is_some());
    }

    #[tokio::test]
    async fn a_folder_space_needs_a_folder_outside_thirtyfiles_own() {
        let env = testutil::env().await;
        assert!(check_source(&env.st, "relative/path").is_err());
        assert!(check_source(&env.st, &env.dir.join("missing").to_string_lossy()).is_err());
        assert!(check_source(&env.st, &env.dir.to_string_lossy()).is_err(), "the data folder itself");
        assert!(check_source(&env.st, &env.dir.join("blobs").to_string_lossy()).is_err(), "the storage folder");
    }

    #[test]
    fn temporary_files_are_ignored() {
        for name in ["~$Report.docx", ".~lock.Budget.xlsx#", "Thumbs.db", ".DS_Store", "desktop.ini", "movie.mp4.part", ".thirtyfile-trash"] {
            assert!(ignored(name), "{name}");
        }
        for name in ["Report.docx", "part.txt", "thumbs.png"] {
            assert!(!ignored(name), "{name}");
        }
    }
}
