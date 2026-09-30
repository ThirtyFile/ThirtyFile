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
/// A file in the folder of every folder space, holding the space's id: written when the space is created (and by a scan
/// when it is missing). A folder without it that is suddenly empty is a disk or share that isn't mounted, not one whose
/// items were all deleted; and nothing is written into a space's folder unless it holds this space's (fsops.rs), so
/// another disk mounted at the same place, with a folder of the same name, is left alone.
pub const MARKER: &str = ".thirtyfile-space";

/// The space id in the marker of the space folder `root`; None when there is none. Only an ordinary file is read, and
/// only its start: a link, a named pipe or a device put there instead is no marker.
pub fn space_marker(root: &crate::beneath::Pinned) -> std::io::Result<Option<String>> {
    use std::io::Read;
    let file = match root.join(MARKER)?.open_file() {
        Ok(f) => f,
        Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => return Ok(None),
        #[cfg(unix)]
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut b = Vec::new();
    file.take(MAX_MARKER).read_to_end(&mut b)?;
    Ok(Some(String::from_utf8_lossy(&b).trim().to_string()))
}

/// Bytes of a space's marker read at most (it holds a space id)
const MAX_MARKER: u64 = 4096;

/// A path below the folder of the space `drive`, looked up (`pin`) only when it is used: long lists of them keep no
/// folder open
#[derive(Clone, Debug)]
pub struct Below {
    pub root: std::path::PathBuf,
    pub drive: String,
    pub rel: String,
}

impl Below {
    pub fn new(root: impl Into<std::path::PathBuf>, drive: impl Into<String>, rel: impl Into<String>) -> Below {
        Below { root: root.into(), drive: drive.into(), rel: rel.into() }
    }

    /// In the space's own folder, which must hold its marker (`open_space`)
    pub fn pin(&self) -> std::io::Result<crate::beneath::Pinned> {
        open_space(&self.root, &self.drive, false)?.join(&self.rel)
    }
}

/// The folder of the space `drive_id` at `root`, opened, for reading what is in it. Someone who can write where the
/// folder is could put another folder, or a link to one, in its place; what is read through the result is in the
/// folder whose marker was checked. It must hold the space's marker; a read-only space's folder may have none (the
/// marker can't always be written there), but never another space's.
pub fn open_space(root: &Path, drive_id: &str, read_only: bool) -> std::io::Result<crate::beneath::Pinned> {
    let pinned = crate::beneath::Pinned::root(root)?;
    match space_marker(&pinned)? {
        Some(id) if id == drive_id => Ok(pinned),
        None if read_only => Ok(pinned),
        _ => Err(std::io::Error::new(std::io::ErrorKind::NotFound, "not the space's folder")),
    }
}

/// Gives the folder `source` of a new space `drive_id` the space's marker, replacing one left by a deleted space (a
/// folder another space uses can't be chosen: `check_new_source`)
pub(crate) fn mark_space(source: &Path, drive_id: &str) -> std::io::Result<()> {
    let marker = crate::beneath::Pinned::root(source)?.join(MARKER)?;
    match std::fs::remove_file(marker.as_path()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    crate::beneath::write_new(&marker, drive_id.as_bytes())
}

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
    /// When it was created, where the file system tells (`birth_ns`)
    birth_ns: Option<i64>,
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

/// When an item was created (it never changes, also not when the item is renamed or edited); None where the file
/// system doesn't tell
pub(crate) fn birth_ns(meta: &std::fs::Metadata) -> Option<i64> {
    meta.created().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos() as i64)
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

/// A file as it was read: its identity, size and modification time
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seen {
    pub dev: i64,
    pub ino: i64,
    pub size: i64,
    pub mtime_ns: i64,
}

pub fn seen(meta: &std::fs::Metadata) -> Seen {
    let (dev, ino) = identity(meta);
    Seen { dev, ino, size: meta.len() as i64, mtime_ns: mtime_ns(meta) }
}

/// What people are told when a file changed while it was copied
pub const CHANGED: &str = "The file changed while it was being copied";

/// Reads a file of a folder into a temp file, hashing it on the way; fails (`Unusable::Changed`) when it changes while
/// it is read
pub fn read_file(root: &crate::beneath::Pinned, rel: &str, tmp: &std::path::Path) -> std::io::Result<(String, Seen)> {
    let file = root.join(rel)?;
    let mut src = file.open_file()?;
    let before = seen(&src.metadata()?);
    let mut out = std::fs::File::create_new(tmp)?;
    let (hash, len) = crate::hashing::copy(&mut src, &mut out)?;
    out.sync_all()?;
    let after = seen(&std::fs::symlink_metadata(file.as_path())?);
    if after != before || len as i64 != before.size {
        return Err(crate::hashing::unusable(crate::hashing::Unusable::Changed, CHANGED));
    }
    Ok((hash, before))
}

/// What `walk` read: the folder's marker and its items
struct Walked {
    marker: Option<String>,
    entries: Vec<Entry>,
}

/// Reads the folder: every item below `root` (or only the items directly in `only`, a path below it), parents
/// before their contents, and the space's marker. Everything is read through the folder opened once, so the marker
/// and the items are those of the same folder, whatever is put in its place meanwhile. Runs on a blocking thread.
fn walk(root: &Path, only: Option<&str>, report: &mut ScanReport, found: Option<&dyn Fn(usize)>) -> std::io::Result<Walked> {
    let mut entries = Vec::new();
    let (_, marker) = walk_each(root, only, report, &mut |_, items| {
        entries.extend(items);
        if let Some(found) = found {
            found(entries.len());
        }
        true
    })?;
    Ok(Walked { marker, entries })
}

/// `walk`, handing the items of each folder read to `visit` (the folder's path, its items) instead of keeping them
/// all; `visit` returns false to stop. Returns the folder opened and its marker.
fn walk_each(
    root: &Path,
    only: Option<&str>,
    report: &mut ScanReport,
    visit: &mut dyn FnMut(&str, Vec<Entry>) -> bool,
) -> std::io::Result<(crate::beneath::Pinned, Option<String>)> {
    // Each folder is reached without following a symbolic link on the way (one could replace a folder meanwhile)
    let pinned = crate::beneath::Pinned::root(root)?;
    let root_meta = std::fs::metadata(pinned.as_path())?;
    if !root_meta.is_dir() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "The folder doesn't exist"));
    }
    let (root_dev, _) = identity(&root_meta);
    let marker = space_marker(&pinned)?;
    let settle_after = (now() - SETTLE_SECONDS) as i128 * 1_000_000_000;
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
                birth_ns: birth_ns(&meta),
                settling: !meta.is_dir() && (mtime as i128) > settle_after,
            });
        }
        children.sort_by(|a, b| a.name.cmp(&b.name));
        if only.is_none() {
            queue.extend(children.iter().filter(|c| c.is_dir).map(|c| c.rel.clone()));
        }
        if !visit(&dir_rel, children) {
            break;
        }
    }
    Ok((pinned, marker))
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
    fs_birth_ns: Option<i64>,
}

enum Op {
    Create { id: String, parent: String, e: Entry },
    Update { id: String, e: Entry },
    /// Records when an unchanged item was created (the index didn't have it yet)
    Birth { id: String, birth: i64 },
    /// First step of a move: a name no other item has, so moves in any order can't collide
    Park { id: String },
    Move { id: String, parent: String, e: Entry },
    Remove { id: String },
    /// What is below a moved folder (`old`, a path below the space's folder) is below its new path now
    Repath { old: String, new: String },
}

/// Scans a whole folder space and brings its index up to date (after a scan of it already running, if any)
pub async fn scan(st: &AppState, drive_id: &str) -> AppResult<ScanReport> {
    let lock = scan_lock(drive_id);
    let _scanning = lock.lock().await;
    run_scan(st, drive_id).await
}

/// Folders read on one blocking call while the index is brought up to date
const FOLDERS_PER_READ: usize = 64;

/// `scan`, with the space's scan lock already held. It goes through the space's folder one folder at a time, so what
/// it keeps in memory doesn't grow with the space (a million files would otherwise need gigabytes):
/// 1. The folder is read without the space's lock, so uploads and other changes to the space don't wait for a long
///    read, and each folder is compared with what the index has in it: which folders differ.
/// 2. With the space's lock held, each of those is read again and the index brought up to date with it. An item new
///    in a folder that the index has elsewhere (the same file system identity, created at the same time) was moved
///    there; one gone from a folder, and not found elsewhere by then, was removed.
async fn run_scan(st: &AppState, drive_id: &str) -> AppResult<ScanReport> {
    let drive = folder_drive(st, drive_id).await?;
    let root = PathBuf::from(drive.source_path.clone().unwrap_or_default());
    let mut report = ScanReport { at: now(), ..Default::default() };
    // A space on a location: the location's folder must be its own (storage.rs, `LOCATION_MARKER`). Another disk
    // mounted there, with a folder at the same place, would make every item look deleted.
    if let Some(e) = location_unavailable(st, drive_id).await? {
        report.error = Some(e);
        save_report(st, &drive, &report).await?;
        return Ok(report);
    }
    set_progress(drive_id, |_| {});
    let _progress = ProgressGuard(drive_id.to_string());
    let started = std::time::Instant::now();

    // 1. Which folders differ from the index
    let compared = match compare(st, &drive, &root, &mut report).await? {
        Ok(c) => c,
        Err(e) => {
            // The folder is gone (an unmounted disk, say): keep the index rather than removing everything
            report.error = Some(format!("Can't read {}: {e}", root.display()));
            save_report(st, &drive, &report).await?;
            return Ok(report);
        }
    };
    report.read_ms = started.elapsed().as_millis() as u64;
    // An empty folder where the index has items is what a disk or share that isn't mounted looks like (its mount
    // point is an empty folder): only a folder that has the marker is really empty
    if compared.marker.as_ref().is_some_and(|id| *id != drive.id) {
        // Another space's folder: a different disk mounted at the same place, say
        report.error = Some(format!(
            "{} holds another space's .thirtyfile-space file: if a different disk is mounted there, mount the right one and check again.",
            root.display()
        ));
        save_report(st, &drive, &report).await?;
        return Ok(report);
    }
    let marked = compared.marker.is_some();
    let (has_items,): (bool,) =
        sqlx::query_as("SELECT EXISTS (SELECT 1 FROM nodes WHERE drive_id = ? AND trashed_at IS NULL AND fs_path IS NOT NULL AND fs_path <> '')")
            .bind(&drive.id)
            .fetch_one(&st.db)
            .await?;
    if compared.root_empty && has_items && !marked {
        report.error = Some(format!(
            "{} is empty, but the space still has items: if it is on a disk or network share that isn't mounted, mount it and check again. To empty the space, delete its items in ThirtyFile.",
            root.display()
        ));
        save_report(st, &drive, &report).await?;
        return Ok(report);
    }
    // Without its marker, the folder is taken for the space's only when it holds an item the index knows by its
    // identity (the marker was deleted, say): another folder put in its place (a link elsewhere) is never indexed
    if has_items && !marked && !compared.same_folder {
        report.error = Some(format!(
            "{} doesn't hold this space's .thirtyfile-space file, nor the items the space has: if another folder or disk is there now, put the right one back and check again.",
            root.display()
        ));
        save_report(st, &drive, &report).await?;
        return Ok(report);
    }
    if !marked
        && let Err(e) = compared.root.join(MARKER).and_then(|m| crate::beneath::write_new(&m, drive.id.as_bytes()))
    {
        tracing::debug!("Couldn't write the marker file in {}: {e}", root.display());
    }

    // 2. The index brought up to date, folder by folder
    let indexing = std::time::Instant::now();
    if !compared.differing.is_empty() {
        let _changing = drive_lock(&drive.id).lock_owned().await;
        update_index(st, &drive, &root, compared.differing, compared.any_gone, &mut report).await?;
    }
    crate::fsops::clean_trash(st, &drive.id, &root).await?;
    // In the folder read (whose marker was checked)
    let leftovers: Vec<crate::beneath::Pinned> = report.leftovers.iter().filter_map(|rel| compared.root.join(rel).ok()).collect();
    if !leftovers.is_empty() {
        tokio::task::spawn_blocking(move || crate::fsops::clean_leftovers(leftovers, crate::fsops::TRASH_GRACE)).await.map_err(AppError::internal)?;
    }
    crate::versions::clean_folder(st, &drive.id, &root).await?;
    report.index_ms = indexing.elapsed().as_millis() as u64;
    if report.read_ms + report.index_ms > 10_000 {
        tracing::info!(
            "Scanned the folder space {}: {} items read in {} s, index updated in {} s",
            drive.name,
            compared.found,
            report.read_ms / 1000,
            report.index_ms / 1000
        );
    }
    finish(st, &drive, &report).await?;
    Ok(report)
}

/// What reading the whole folder found (`compare`)
struct Compared {
    /// The folder, opened when it was read, and the marker it held
    root: crate::beneath::Pinned,
    marker: Option<String>,
    /// Items found
    found: usize,
    /// The folder itself holds nothing
    root_empty: bool,
    /// An item was found at its indexed path with its indexed identity (or there are no identities to tell by)
    same_folder: bool,
    /// Folders whose items differ from what the index has in them (paths below the space's folder), parents first
    differing: Vec<String>,
    /// Whether an indexed item is gone from where the index has it: only then can an item have moved
    any_gone: bool,
}

/// Reads the space's whole folder without its lock, one folder at a time, comparing each with the index. The inner
/// result is the error of a folder that can't be read at all.
async fn compare(st: &AppState, drive: &Drive, root: &Path, report: &mut ScanReport) -> AppResult<std::io::Result<Compared>> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(String, Vec<Entry>)>(4);
    let (root, id) = (root.to_path_buf(), drive.id.clone());
    let walker = tokio::task::spawn_blocking(move || {
        let mut r = ScanReport::default();
        let res = walk_each(&root, None, &mut r, &mut |rel, items| tx.blocking_send((rel.to_string(), items)).is_ok());
        res.map(|opened| (opened, r))
    });
    let (mut found, mut root_empty, mut matched, mut identities, mut any_gone) = (0, false, false, false, false);
    let mut differing = Vec::new();
    while let Some((rel, items)) = rx.recv().await {
        if rel.is_empty() {
            root_empty = items.is_empty();
        }
        found += items.len();
        set_progress(&id, |p| p.found = found);
        identities |= items.iter().any(|e| e.ino != 0);
        let children = indexed_in(st, drive, &rel).await?;
        let (differ, gone) = differs(children.as_deref(), &items, &mut matched);
        any_gone |= gone;
        if differ {
            differing.push(rel);
        }
    }
    let ((opened, marker), r) = match walker.await.map_err(AppError::internal)? {
        Ok(w) => w,
        Err(e) => return Ok(Err(e)),
    };
    report.skipped = r.skipped;
    report.unreadable = r.unreadable;
    report.leftovers = r.leftovers;
    Ok(Ok(Compared { root: opened, marker, found, root_empty, same_folder: matched || !identities, differing, any_gone }))
}

/// What the index has directly in the folder at `rel`; None when it has no folder there
async fn indexed_in(st: &AppState, drive: &Drive, rel: &str) -> AppResult<Option<Vec<Indexed>>> {
    let Some(folder) = folder_at(st, drive, rel).await? else { return Ok(None) };
    Ok(Some(
        sqlx::query_as(
            "SELECT id, parent_id, kind, fs_path, fs_dev, fs_ino, fs_size, fs_mtime_ns, fs_birth_ns FROM nodes WHERE parent_id = ? AND trashed_at IS NULL",
        )
        .bind(&folder)
        .fetch_all(&st.db)
        .await?,
    ))
}

/// The id of the indexed folder at `rel` ("" for the space's root folder)
async fn folder_at(st: &AppState, drive: &Drive, rel: &str) -> AppResult<Option<String>> {
    if rel.is_empty() {
        return Ok(Some(drive.root_id.clone()));
    }
    let row: Option<(String,)> =
        sqlx::query_as("SELECT id FROM nodes WHERE drive_id = ? AND fs_path = ? AND kind = 'folder' AND trashed_at IS NULL")
            .bind(&drive.id)
            .bind(rel)
            .fetch_optional(&st.db)
            .await?;
    Ok(row.map(|(id,)| id))
}

/// Whether the items of a folder differ from what the index has in it (`children`; None: it has no folder there), and
/// whether one of those is gone. An item found at its indexed path with its indexed identity sets `matched`.
fn differs(children: Option<&[Indexed]>, items: &[Entry], matched: &mut bool) -> (bool, bool) {
    let Some(children) = children else { return (true, false) };
    let by_path: HashMap<&str, &Indexed> = children.iter().filter_map(|n| n.fs_path.as_deref().map(|p| (p, n))).collect();
    let mut differ = false;
    for e in items {
        match by_path.get(e.rel.as_str()) {
            Some(n) if n.kind == e.kind() => {
                let same = e.dev == n.fs_dev.unwrap_or(-1) && e.ino == n.fs_ino.unwrap_or(-1);
                *matched |= same && e.ino != 0;
                let changed = !same || (!e.is_dir && (e.size != n.fs_size.unwrap_or(-1) || e.mtime_ns != n.fs_mtime_ns.unwrap_or(-1)));
                differ |= (changed && !e.settling) || (!changed && e.birth_ns.is_some_and(|b| n.fs_birth_ns != Some(b)));
            }
            Some(_) => differ = true,
            None => differ |= !e.settling,
        }
    }
    let present: HashMap<&str, &str> = items.iter().map(|e| (e.rel.as_str(), e.kind())).collect();
    // Gone, or the other kind now
    let gone = children.iter().any(|n| n.fs_path.as_deref().is_some_and(|p| !p.is_empty() && present.get(p).is_none_or(|k| *k != n.kind)));
    (differ || gone, gone)
}

/// Brings the index up to date with the folders `differing` (parents first), with the space's lock held: each is read
/// again, a few at a time, and its changes made. Items gone from one are removed once all of them were gone through
/// (one may have moved to a folder further on).
/// `may_move`: whether an item may have moved (something was gone when the folder was read); else new items aren't looked
/// for elsewhere in the index, which saves a query per item when a large folder is scanned the first time.
async fn update_index(st: &AppState, drive: &Drive, root: &Path, differing: Vec<String>, may_move: bool, report: &mut ScanReport) -> AppResult<()> {
    set_progress(&drive.id, |p| {
        p.phase = "indexing";
        p.total = differing.len();
    });
    let mut gone: Vec<(String, String)> = Vec::new();
    let mut pending = Vec::new();
    for chunk in differing.chunks(FOLDERS_PER_READ) {
        // Read again, now that nothing else changes the space
        let (root, rels) = (root.to_path_buf(), chunk.to_vec());
        let read = tokio::task::spawn_blocking(move || {
            rels.into_iter()
                .map(|rel| {
                    let mut r = ScanReport::default();
                    let w = walk(&root, Some(&rel), &mut r, None);
                    (rel, w.map(|w| w.entries), r)
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(AppError::internal)?;
        for (rel, items, r) in read {
            for s in r.skipped {
                report.skip(s);
            }
            // (can't be read now: what the index has in it stays)
            let Ok(items) = items else { continue };
            let mut folder = folder_at(st, drive, &rel).await?;
            // Made by changes not made yet (a new folder in a folder gone through just before)
            if folder.is_none() && !pending.is_empty() {
                report.removed += apply(st, drive, std::mem::take(&mut pending)).await?;
                folder = folder_at(st, drive, &rel).await?;
            }
            let Some(folder) = folder else {
                report.skip(format!("{}: its folder couldn't be indexed", if rel.is_empty() { "/" } else { &rel }));
                continue;
            };
            let (ops, left) = reconcile(st, drive, &folder, items, may_move, report).await?;
            pending.extend(ops);
            gone.extend(left);
            // The changes of several folders together, a batch per transaction
            if pending.len() >= 10 * BATCH {
                report.removed += apply(st, drive, std::mem::take(&mut pending)).await?;
            }
            set_progress(&drive.id, |p| p.done += 1);
        }
    }
    report.removed += apply(st, drive, pending).await?;
    remove_gone(st, drive, root, gone, report).await
}

/// The changes that make the index's folder `folder` hold what `items` (read from its folder) says; and the items of
/// it that are gone from there, which may have moved elsewhere: (id, path)
async fn reconcile(
    st: &AppState,
    drive: &Drive,
    folder: &str,
    items: Vec<Entry>,
    may_move: bool,
    report: &mut ScanReport,
) -> AppResult<(Vec<Op>, Vec<(String, String)>)> {
    let children: Vec<Indexed> = sqlx::query_as(
        "SELECT id, parent_id, kind, fs_path, fs_dev, fs_ino, fs_size, fs_mtime_ns, fs_birth_ns FROM nodes WHERE parent_id = ? AND trashed_at IS NULL",
    )
    .bind(folder)
    .fetch_all(&st.db)
    .await?;
    let by_path: HashMap<&str, &Indexed> = children.iter().filter_map(|n| n.fs_path.as_deref().map(|p| (p, n))).collect();
    let present: HashMap<String, &'static str> = items.iter().map(|e| (e.rel.clone(), e.kind())).collect();
    let mut ops = Vec::new();
    // A path that holds the other kind now (a file replaced by a folder of the same name): the item goes first
    let mut kind_changed = HashSet::new();
    for n in &children {
        if let Some(p) = n.fs_path.as_deref()
            && present.get(p).is_some_and(|k| *k != n.kind)
        {
            kind_changed.insert(n.id.clone());
            ops.push(Op::Remove { id: n.id.clone() });
        }
    }
    // New items that the index has elsewhere by their identity: moved here, when their old path is gone
    let mut moves: HashMap<String, Indexed> = HashMap::new();
    let new: Vec<&Entry> =
        items.iter().filter(|e| may_move && !e.settling && e.ino != 0 && by_path.get(e.rel.as_str()).is_none_or(|n| kind_changed.contains(&n.id))).collect();
    for e in &new {
        let found: Vec<Indexed> = sqlx::query_as(
            "SELECT id, parent_id, kind, fs_path, fs_dev, fs_ino, fs_size, fs_mtime_ns, fs_birth_ns FROM nodes
             WHERE fs_dev = ? AND fs_ino = ? AND drive_id = ? AND trashed_at IS NULL",
        )
        .bind(e.dev)
        .bind(e.ino)
        .bind(&drive.id)
        .fetch_all(&st.db)
        .await?;
        if let Some(n) = found.into_iter().find(|n| n.kind == e.kind() && same_item(n, e) && n.fs_path.as_deref().is_some_and(|p| p != e.rel && !present.contains_key(p))) {
            moves.insert(e.rel.clone(), n);
        }
    }
    // Only those whose old path is gone: an item at two paths (a hard link) stays where it is
    if !moves.is_empty() {
        let root = drive.source_path.clone().unwrap_or_default();
        let old: Vec<(String, String, i64, i64)> = moves.iter().map(|(rel, n)| (rel.clone(), n.fs_path.clone().unwrap_or_default(), n.fs_dev.unwrap_or(0), n.fs_ino.unwrap_or(0))).collect();
        let still: HashSet<String> = tokio::task::spawn_blocking(move || {
            let Ok(top) = crate::beneath::Pinned::root(Path::new(&root)) else { return HashSet::new() };
            old.into_iter()
                .filter(|(_, path, dev, ino)| top.join(path).ok().and_then(|p| crate::fsops::stat(p.as_path()).ok()).is_some_and(|s| (s.dev, s.ino) == (*dev, *ino)))
                .map(|(rel, ..)| rel)
                .collect()
        })
        .await
        .map_err(AppError::internal)?;
        moves.retain(|rel, _| !still.contains(rel));
    }
    let mut moved = HashSet::new();
    for e in items {
        match by_path.get(e.rel.as_str()) {
            Some(n) if !kind_changed.contains(&n.id) => {
                let changed = e.dev != n.fs_dev.unwrap_or(-1)
                    || e.ino != n.fs_ino.unwrap_or(-1)
                    || (!e.is_dir && (e.size != n.fs_size.unwrap_or(-1) || e.mtime_ns != n.fs_mtime_ns.unwrap_or(-1)));
                if changed && !e.settling {
                    if !e.is_dir {
                        report.changed += 1;
                    }
                    ops.push(Op::Update { id: n.id.clone(), e });
                } else if let Some(birth) = e.birth_ns.filter(|b| !changed && n.fs_birth_ns != Some(*b)) {
                    ops.push(Op::Birth { id: n.id.clone(), birth });
                }
            }
            _ if e.settling => {}
            _ => match moves.remove(&e.rel) {
                Some(n) => {
                    report.moved += 1;
                    moved.insert(n.id.clone());
                    ops.push(Op::Park { id: n.id.clone() });
                    // What is in a moved folder is at new paths too
                    let old = n.fs_path.clone().unwrap_or_default();
                    let folder_moved = e.is_dir.then(|| (old, e.rel.clone()));
                    ops.push(Op::Move { id: n.id, parent: folder.to_string(), e });
                    if let Some((old, new)) = folder_moved {
                        ops.push(Op::Repath { old, new });
                    }
                }
                None => {
                    report.added += 1;
                    ops.push(Op::Create { id: new_id(), parent: folder.to_string(), e });
                }
            },
        }
    }
    let gone = children
        .into_iter()
        .filter(|n| !kind_changed.contains(&n.id) && !moved.contains(&n.id))
        .filter_map(|n| n.fs_path.filter(|p| !p.is_empty() && !present.contains_key(p.as_str())).map(|p| (n.id, p)))
        .collect();
    Ok((ops, gone))
}

/// Removes what was gone from its folder and wasn't found elsewhere by then: still at its old path in the index, and
/// not on the disk there. Only the topmost of them: what is in them goes with them.
async fn remove_gone(st: &AppState, drive: &Drive, root: &Path, mut gone: Vec<(String, String)>, report: &mut ScanReport) -> AppResult<()> {
    gone.sort_by(|a, b| a.1.cmp(&b.1));
    let mut ops = Vec::new();
    let mut last_removed: Option<String> = None;
    for (id, path) in gone {
        if last_removed.as_deref().is_some_and(|up| path.strip_prefix(up).is_some_and(|rest| rest.starts_with('/'))) {
            continue;
        }
        let now: Option<(Option<String>,)> = sqlx::query_as("SELECT fs_path FROM nodes WHERE id = ? AND trashed_at IS NULL").bind(&id).fetch_optional(&st.db).await?;
        if now.is_none_or(|(p,)| p.as_deref() != Some(path.as_str())) {
            continue;
        }
        let (root, rel) = (root.to_path_buf(), path.clone());
        let there = tokio::task::spawn_blocking(move || {
            crate::beneath::Pinned::root(&root).and_then(|t| t.join(&rel)).is_ok_and(|p| std::fs::symlink_metadata(p.as_path()).is_ok())
        })
        .await
        .map_err(AppError::internal)?;
        if there {
            continue;
        }
        ops.push(Op::Remove { id });
        last_removed = Some(path);
    }
    report.removed += apply(st, drive, ops).await?;
    Ok(())
}

/// A folder opened again within this long isn't read again: what changed on the server shows at the next opening
/// after that (or the next scan)
const REREAD_AFTER: std::time::Duration = std::time::Duration::from_secs(5);
/// How long opening a folder waits for its disk before showing the index as it is
const OPEN_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
/// How long bringing a folder up to date otherwise (the watcher, a WebDAV name check) waits for its disk
const SYNC_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// When each folder was last read from disk (by id), to leave out reading it again right away
fn last_reads() -> &'static Mutex<HashMap<String, std::time::Instant>> {
    static READS: OnceLock<Mutex<HashMap<String, std::time::Instant>>> = OnceLock::new();
    READS.get_or_init(Default::default)
}

/// Tests: every folder counts as not read for a while
#[cfg(test)]
pub fn forget_reads() {
    last_reads().lock().unwrap().clear();
}

/// A folder of a folder space is opened (listed on the web, or over WebDAV): new and changed items on the server show
/// right away, unless finding them would keep whoever opened it waiting. It isn't read when its space is watched for
/// changes (they are in the index already), when it was read a moment ago, while a change or scan holds its space (a
/// copy of a large folder into it, say), or when its disk doesn't answer within a few seconds: the listing then shows
/// the index as it is.
pub async fn sync_opened(st: &AppState, folder: &Node) {
    if watched(folder.drive()) || last_reads().lock().unwrap().get(&folder.id).is_some_and(|t| t.elapsed() < REREAD_AFTER) {
        return;
    }
    if let Err(e) = try_sync_folder(st, folder, false).await {
        tracing::warn!("Couldn't check the folder \"{}\" for changes: {}", folder.name, e.message);
    }
}

/// Brings one folder of a folder space up to date (a change was seen in it): new and changed items right away.
/// Anything that disappeared may have moved elsewhere, which only a scan of the whole space can tell, so that runs in
/// the background.
pub async fn sync_folder(st: &AppState, folder: &Node) {
    if let Err(e) = try_sync_folder(st, folder, true).await {
        tracing::warn!("Couldn't check the folder \"{}\" for changes: {}", folder.name, e.message);
    }
}

/// `wait`: for the space's lock and its disk; else the folder is left as the index has it when either is busy
async fn try_sync_folder(st: &AppState, folder: &Node, wait: bool) -> AppResult<()> {
    let (Some(rel), true) = (folder.fs_path.clone(), folder.is_folder()) else { return Ok(()) };
    let drive = folder_drive(st, folder.drive()).await?;
    // Takes turns with changes from the web and with scans updating the index (both hold the lock only briefly: a
    // scan reads the folder without it), so a change seen here is never skipped
    let lock = drive_lock(&drive.id);
    let _scanning = if wait {
        lock.lock_owned().await
    } else {
        match lock.try_lock_owned() {
            Ok(l) => l,
            Err(_) => return Ok(()),
        }
    };
    let root = PathBuf::from(drive.source_path.clone().unwrap_or_default());
    let mut report = ScanReport::default();
    let read_at = std::time::Instant::now();
    let entries = {
        let rel = rel.clone();
        let mut r = ScanReport::default();
        // On a blocking thread, within a time limit: a disk that doesn't answer leaves the index as it is
        let key = format!("read of a folder in {}", drive.id);
        let Some(res) = crate::util::blocking_within(key, if wait { SYNC_WAIT } else { OPEN_WAIT }, move || walk(&root, Some(&rel), &mut r, None)).await else {
            return Ok(());
        };
        match res {
            // Only the space's own folder (a scan looks at one without its marker)
            Ok(w) if w.marker.as_deref() == Some(drive.id.as_str()) => w.entries,
            Ok(_) => {
                drop(_scanning);
                scan_later(st, &drive.id);
                return Ok(());
            }
            Err(_) => return Ok(()),
        }
    };
    let mut indexed: Vec<Indexed> = sqlx::query_as(
        "SELECT id, parent_id, kind, fs_path, fs_dev, fs_ino, fs_size, fs_mtime_ns, fs_birth_ns FROM nodes WHERE (parent_id = ?1 OR id = ?1) AND trashed_at IS NULL",
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
    if let Err(e) = apply(st, &drive, ops).await.map(|_| ()) {
        drop(_scanning);
        scan_later(st, &drive.id);
        return Err(e);
    }
    if changed {
        refresh_usage(st, &drive).await?;
    }
    drop(_scanning);
    {
        let mut reads = last_reads().lock().unwrap();
        reads.retain(|_, t| t.elapsed() < REREAD_AFTER);
        reads.insert(folder.id.clone(), read_at);
    }
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
    let Ok(scanning) = scan_lock(drive_id).try_lock_owned() else { return };
    let (st, id) = (st.clone(), drive_id.to_string());
    tokio::spawn(async move {
        let _scanning = scanning;
        if let Err(e) = run_scan(&st, &id).await {
            tracing::warn!("Scanning a folder space failed: {}", e.message);
        }
    });
}

/// Spaces watched for changes are scanned this many times less often than the interval set in the Control panel
const WATCHED_SCAN_FACTOR: i64 = 4;

fn watched(drive_id: &str) -> bool {
    #[cfg(target_os = "linux")]
    return crate::watch::is_watched(drive_id);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = drive_id;
        false
    }
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
            let due: Vec<(String, i64)> = match sqlx::query_as(
                "SELECT id, COALESCE(last_scan_at, 0) FROM drives WHERE mode = 'folder' AND disabled = 0 AND COALESCE(last_scan_at, 0) <= ?",
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
            for (id, last) in due {
                // Watched spaces only need the regular scan for what watching can miss
                if watched(&id) && last > now() - minutes * 60 * WATCHED_SCAN_FACTOR {
                    continue;
                }
                if let Err(e) = scan(&st, &id).await {
                    tracing::warn!("Scanning a folder space failed: {}", e.message);
                }
            }
        }
    });
}

/// The lock a change to a folder space and the index update of a scan or sync take turns with
pub(crate) fn drive_lock(drive_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    LOCKS.get_or_init(Default::default).lock().unwrap().entry(drive_id.to_string()).or_default().clone()
}

/// Holds a folder space still while a move switches it over (moves/): no scan updates its index and no change from the
/// web is made meanwhile (both wait). The scan lock first, as everywhere.
pub(crate) async fn hold(drive_id: &str) -> (tokio::sync::OwnedMutexGuard<()>, tokio::sync::OwnedMutexGuard<()>) {
    let scanning = scan_lock(drive_id).lock_owned().await;
    let changing = drive_lock(drive_id).lock_owned().await;
    (scanning, changing)
}

/// Scans of a space, one at a time (a scan asked for while one runs waits for it, `scan_later` skips)
fn scan_lock(drive_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    LOCKS.get_or_init(Default::default).lock().unwrap().entry(drive_id.to_string()).or_default().clone()
}

/// Why the storage location a folder space is on can't be used now (its folder isn't there, or holds another
/// location's marker); None when it can, or the space is on no location
async fn location_unavailable(st: &AppState, drive_id: &str) -> AppResult<Option<String>> {
    let (location,): (Option<String>,) = sqlx::query_as("SELECT location_id FROM drives WHERE id = ?").bind(drive_id).fetch_one(&st.db).await?;
    let Some(location) = location else { return Ok(None) };
    // Within a time limit: a location that doesn't answer mustn't hold up the scans of every other space
    Ok(crate::locations::ping(st, &location).await.err())
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
                } else if let Some(birth) = e.birth_ns.filter(|b| !changed && n.fs_birth_ns != Some(*b)) {
                    ops.push(Op::Birth { id: n.id.clone(), birth });
                }
            }
            _ => {
                if e.settling {
                    continue;
                }
                match missing.remove(&(e.dev, e.ino)).filter(|n| n.kind == e.kind() && same_item(n, e)) {
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

/// Whether an item found at a new path is the indexed one whose path is gone, rather than a new item that got its inode
/// number after it was deleted (ext4 and XFS give them out again): created at the same time, where the file system
/// tells; else, for a file, of the same size and modification time, which moving or renaming it doesn't change. A
/// folder whose creation time isn't known can't be told apart: it counts as removed and added.
fn same_item(n: &Indexed, e: &Entry) -> bool {
    match (n.fs_birth_ns, e.birth_ns) {
        (Some(was), Some(now)) => was == now,
        _ => !e.is_dir && n.fs_size == Some(e.size) && n.fs_mtime_ns == Some(e.mtime_ns),
    }
}

impl Entry {
    fn kind(&self) -> &'static str {
        if self.is_dir { "folder" } else { "file" }
    }
    fn secs(&self) -> i64 {
        self.mtime_ns.div_euclid(1_000_000_000)
    }
}

/// Makes the changes `ops`, a batch per transaction; returns how many items were removed
async fn apply(st: &AppState, drive: &Drive, ops: Vec<Op>) -> AppResult<usize> {
    if ops.is_empty() {
        return Ok(0);
    }
    let owner = sqlx::query_as::<_, (i64,)>("SELECT owner_id FROM nodes WHERE id = ?").bind(&drive.root_id).fetch_one(&st.db).await?.0;
    let mut ops = ops.into_iter().peekable();
    let mut removed = 0;
    while ops.peek().is_some() {
        let mut n = 0;
        let mut after = None;
        {
            let _w = st.write_lock.lock().await;
            let mut tx = crate::db::begin_write(&st.db).await?;
            let mut unused = Vec::new();
            while n < BATCH
                && let Some(op) = ops.next()
            {
                n += 1;
                // An item gone from the folder goes with everything in it, and a moved folder takes what is in it to
                // its new path: that may be a lot, so a batch per transaction of its own (the space's lock is held, so
                // nothing else changes the space meanwhile)
                if matches!(op, Op::Remove { .. } | Op::Repath { .. }) {
                    after = Some(op);
                    break;
                }
                unused.extend(apply_one(&mut tx, drive, owner, op).await?);
            }
            tx.commit().await?;
            tree::schedule_blob_removal(st, unused);
        }
        match after {
            Some(Op::Remove { id }) => removed += tree::changes::purge_now(st, &id).await?,
            Some(Op::Repath { old, new }) => tree::changes::repath_now(st, &drive.id, &old, &new).await?,
            _ => {}
        }
    }
    Ok(removed)
}

/// Returns content of the content store no longer used (earlier versions of a removed file kept there, say), to remove
/// after the commit
async fn apply_one(conn: &mut SqliteConnection, drive: &Drive, owner: i64, op: Op) -> AppResult<Vec<tree::BlobRef>> {
    match op {
        Op::Create { id, parent, e } => {
            sqlx::query(
                "INSERT INTO nodes (id, owner_id, parent_id, kind, name, size, mime, drive_id, created_at, updated_at,
                                    fs_path, fs_dev, fs_ino, fs_size, fs_mtime_ns, fs_birth_ns)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
            .bind(e.birth_ns)
            .execute(&mut *conn)
            .await?;
        }
        Op::Update { id, e } => {
            // The time is the version the editors compare, so it only moves forward
            sqlx::query(
                "UPDATE nodes SET size = ?, updated_at = MAX(?, updated_at + 1), fs_dev = ?, fs_ino = ?, fs_size = ?, fs_mtime_ns = ?, fs_birth_ns = ?
                 WHERE id = ?",
            )
            .bind(e.size)
            .bind(e.secs())
            .bind(e.dev)
            .bind(e.ino)
            .bind(e.size)
            .bind(e.mtime_ns)
            .bind(e.birth_ns)
            .bind(&id)
            .execute(&mut *conn)
            .await?;
        }
        Op::Birth { id, birth } => {
            sqlx::query("UPDATE nodes SET fs_birth_ns = ? WHERE id = ?").bind(birth).bind(&id).execute(&mut *conn).await?;
        }
        Op::Park { id } => {
            sqlx::query("UPDATE nodes SET name = char(1) || id WHERE id = ?").bind(&id).execute(&mut *conn).await?;
        }
        Op::Move { id, parent, e } => {
            sqlx::query(
                "UPDATE nodes SET parent_id = ?, name = ?, mime = ?, size = ?, fs_path = ?, fs_dev = ?, fs_ino = ?, fs_size = ?, fs_mtime_ns = ?,
                                  fs_birth_ns = ?
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
            .bind(e.birth_ns)
            .bind(&id)
            .execute(&mut *conn)
            .await?;
        }
        // Removed a batch at a time by `apply` (tree/changes.rs): grants, shares and favourites of the removed items go
        // with them, and so do earlier versions from before a file came into the folder, kept in the content store.
        // Paths below a moved folder change a batch at a time as well.
        Op::Remove { .. } | Op::Repath { .. } => {}
    }
    Ok(Vec::new())
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
    for own in [&st.data_dir, &st.storage_dir] {
        if let Ok(own) = std::fs::canonicalize(own)
            && (real.starts_with(&own) || own.starts_with(&real))
        {
            return Err(AppError::bad_request("Choose a folder outside ThirtyFile's own data and storage folders"));
        }
    }
    if crate::util::system_folder(&real) {
        return Err(AppError::bad_request("Choose a folder outside the system's own folders"));
    }
    let real = real.to_string_lossy().into_owned();
    // Windows: show C:\folder rather than the \\?\C:\folder form canonicalize returns
    Ok(match real.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => rest.to_owned(),
        _ => real,
    })
}

/// The path with links resolved, as far as it exists (a folder that isn't made yet keeps the rest as given); on Windows
/// without the `\\?\` form, so paths compare
fn real_path(p: &str) -> PathBuf {
    let given = PathBuf::from(p);
    let mut rest = Vec::new();
    let mut at = given.as_path();
    let real = loop {
        if let Ok(r) = std::fs::canonicalize(at) {
            break r;
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                at = parent;
            }
            _ => return given,
        }
    };
    let real = match real.to_string_lossy().strip_prefix(r"\\?\") {
        Some(plain) if !plain.starts_with("UNC\\") => PathBuf::from(plain),
        _ => real,
    };
    rest.into_iter().rev().fold(real, |p, name| p.join(name))
}

fn overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Folders on the server ThirtyFile already uses, besides its own data folder: the storage folder, each Local folder
/// location's folder (with the spaces made in it), and each folder shown as a space from elsewhere. A location with the
/// id `except` (the one being edited) is left out.
async fn claimed(st: &AppState, except: Option<&str>) -> AppResult<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mut locations = vec![real_path(&st.storage_dir.to_string_lossy())];
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT id, config FROM storage_locations WHERE kind = 'local'").fetch_all(&st.db).await?;
    for (id, config) in rows {
        let path = serde_json::from_str::<serde_json::Value>(&config).ok().and_then(|c| c["path"].as_str().map(str::trim).map(str::to_string));
        if let Some(path) = path.filter(|p| !p.is_empty() && except != Some(id.as_str())) {
            locations.push(real_path(&path));
        }
    }
    let spaces: Vec<(String,)> = sqlx::query_as("SELECT source_path FROM drives WHERE mode = 'folder' AND source_path IS NOT NULL").fetch_all(&st.db).await?;
    let shown: Vec<PathBuf> =
        spaces.into_iter().map(|(p,)| real_path(&p)).filter(|p| !locations.iter().any(|l| p.starts_with(l))).collect();
    Ok((locations, shown))
}

/// A folder to show as a space must not contain, or be inside, one ThirtyFile already uses: the same files would be in
/// two spaces, and deleting in one would change the other
pub async fn check_new_source(st: &AppState, path: &str) -> AppResult<()> {
    let p = real_path(path);
    let (locations, shown) = claimed(st, None).await?;
    if locations.iter().chain(&shown).any(|c| overlap(&p, c)) {
        return Err(AppError::conflict("This folder contains, or is inside, a folder that another space or storage location uses"));
    }
    Ok(())
}

/// The folder of a Local folder location (`id` when editing one) must not be inside ThirtyFile's data folder, nor
/// contain or be inside a folder shown as a space or another location's folder
pub async fn check_location_folder(st: &AppState, id: Option<&str>, kind: &str, config: &serde_json::Value) -> AppResult<()> {
    let path = config["path"].as_str().map(str::trim).unwrap_or_default();
    if kind != "local" || path.is_empty() {
        return Ok(());
    }
    let p = real_path(path);
    if overlap(&p, &real_path(&st.data_dir.to_string_lossy())) {
        return Err(AppError::bad_request("Choose a folder outside ThirtyFile's data folder"));
    }
    if crate::util::system_folder(&p) {
        return Err(AppError::bad_request("Choose a folder outside the system's own folders"));
    }
    let (locations, shown) = claimed(st, id).await?;
    if locations.iter().chain(&shown).any(|c| overlap(&p, c)) {
        return Err(AppError::conflict("This folder contains, or is inside, a folder that another space or storage location uses"));
    }
    Ok(())
}

/// Makes a new space a folder space showing `source` and creates its index root (called when the space is created).
/// `location`: the storage location whose folder holds `source` (space_folders.rs), None for a folder an administrator
/// chose, which is on no location.
pub async fn set_up(conn: &mut SqliteConnection, drive_id: &str, root_id: &str, source: &str, location: Option<&str>) -> AppResult<()> {
    sqlx::query("UPDATE drives SET mode = 'folder', source_path = ?, location_id = ? WHERE id = ?")
        .bind(source)
        .bind(location)
        .bind(drive_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query("UPDATE nodes SET fs_path = '' WHERE id = ?").bind(root_id).execute(&mut *conn).await?;
    // Before anything is written there (a folder that can't take it, read-only say, gets it from a scan if ever)
    if let Err(e) = mark_space(Path::new(source), drive_id) {
        tracing::debug!("Couldn't write the marker file in {source}: {e}");
    }
    Ok(())
}

#[cfg(test)]
mod overlap_tests {
    use super::*;
    use crate::testutil;

    #[tokio::test]
    async fn folders_already_in_use_cant_be_shown_again_or_used_for_a_location() {
        let env = testutil::env().await;
        let space = env.folder_space("NAS").await;
        std::fs::create_dir_all(space.dir.join("Inside")).unwrap();
        let inside = space.dir.join("Inside").to_string_lossy().into_owned();
        let parent = space.dir.parent().unwrap().to_string_lossy().into_owned();
        // Inside a space's folder, or holding it: no new space
        assert!(check_new_source(&env.st, &inside).await.is_err());
        assert!(check_new_source(&env.st, &parent).await.is_err());
        // Nor a Local folder location there, or in ThirtyFile's data folder
        let local = |p: &str| serde_json::json!({ "path": p });
        assert!(check_location_folder(&env.st, None, "local", &local(&inside)).await.is_err());
        assert!(check_location_folder(&env.st, None, "local", &local(&env.dir.join("data-inside").to_string_lossy())).await.is_err());
        // A folder of its own is fine
        let free = std::env::temp_dir().join(format!("thirtyfile-free-{}", new_id()));
        std::fs::create_dir_all(&free).unwrap();
        assert!(check_new_source(&env.st, &free.to_string_lossy()).await.is_ok());
        assert!(check_location_folder(&env.st, None, "local", &local(&free.to_string_lossy())).await.is_ok());
        let _ = std::fs::remove_dir_all(&free);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{self, write_old};
    use axum::extract::{Path as UrlPath, Query, State};

    #[tokio::test]
    async fn opening_a_folder_doesnt_wait_for_a_change_in_its_space_nor_read_it_again_right_away() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        write_old(&space.dir.join("a.txt"), b"a");
        let list = || {
            let (st, admin, root) = (env.st.clone(), admin.clone(), space.root.clone());
            async move {
                let axum::Json(l) = crate::nodes::children(State(st), admin, UrlPath(root), Query(Default::default())).await.unwrap();
                l.into_items().into_iter().map(|n| n.name).collect::<Vec<_>>()
            }
        };
        // A long change holds the space (copying a large folder into it, say): the listing answers meanwhile, from the
        // index as it is
        let held = crate::fsops::lock_space(&space.drive).await;
        let started = std::time::Instant::now();
        let names = tokio::time::timeout(std::time::Duration::from_secs(3), list()).await.expect("the listing answers");
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(names.is_empty());
        drop(held);
        // Opened again: what was added on the server shows
        assert_eq!(list().await, ["a.txt"]);
        // Opened again right away: not read again
        write_old(&space.dir.join("b.txt"), b"b");
        assert_eq!(list().await, ["a.txt"]);
        // A while later it is (and a scan shows it anyway)
        forget_reads();
        assert_eq!(list().await, ["a.txt", "b.txt"]);
    }

    /// The process's own memory (not files it maps, such as the database: Linux's RssAnon), in MB
    fn anon_mb() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|l| l.starts_with("RssAnon:"))?;
        line.split_whitespace().nth(1)?.parse::<u64>().ok().map(|kb| kb / 1024)
    }

    /// The issue's estimate (a first scan of a million files holds about 1 GB), measured on Linux: the most memory the
    /// process held while scanning, sampled every 10 ms.
    /// `ITEMS=200000 cargo test --release -- --ignored --nocapture measure_scanning`
    #[tokio::test]
    #[ignore]
    async fn measure_scanning_a_large_space() {
        let env = testutil::env().await;
        let space = env.folder_space("Big").await;
        let n: usize = std::env::var("ITEMS").ok().and_then(|v| v.parse().ok()).unwrap_or(200_000);
        for i in 0..n {
            write_old(&space.dir.join(format!("d{:04}/sub/file-with-a-longer-name-{i:07}.txt", i / 100)), b"x");
        }
        for what in ["first scan (everything new)", "scan with nothing changed"] {
            let before = anon_mb().unwrap_or(0);
            let peak = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(before));
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (p, s) = (peak.clone(), stop.clone());
            let sampler = std::thread::spawn(move || {
                while !s.load(std::sync::atomic::Ordering::SeqCst) {
                    p.fetch_max(anon_mb().unwrap_or(0), std::sync::atomic::Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            });
            let started = std::time::Instant::now();
            let r = scan(&env.st, &space.drive).await.unwrap();
            let took = started.elapsed();
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
            sampler.join().unwrap();
            println!(
                "{n} files, {what}: {:.1} s, memory {before} MB before, at most {} MB during, {} added",
                took.as_secs_f64(),
                peak.load(std::sync::atomic::Ordering::SeqCst),
                r.added
            );
        }
    }

    /// Opening a large folder of a folder space again and again (the web asks for its first page each time it is opened
    /// or refreshed): `ITEMS=20000 cargo test --release -- --ignored --nocapture measure_opening`
    #[tokio::test]
    #[ignore]
    async fn measure_opening_a_large_folder() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        let n: usize = std::env::var("ITEMS").ok().and_then(|v| v.parse().ok()).unwrap_or(20_000);
        for i in 0..n {
            write_old(&space.dir.join(format!("Big/f{i:06}.txt")), b"x");
        }
        scan(&env.st, &space.drive).await.unwrap();
        let (big, _) = env.node_at(&space.drive, "Big").await.unwrap();
        let open = || {
            let (st, admin, big) = (env.st.clone(), admin.clone(), big.clone());
            async move {
                let started = std::time::Instant::now();
                let q = Query(serde_json::from_value(serde_json::json!({ "limit": 100 })).unwrap());
                let _ = crate::nodes::children(State(st), admin, UrlPath(big), q).await.unwrap();
                started.elapsed()
            }
        };
        forget_reads();
        let first = open().await;
        let again = open().await;
        println!("{n} items: opened {} ms (reads the folder), opened again right away {} ms", first.as_millis(), again.as_millis());
    }

    #[tokio::test]
    async fn items_moved_between_folders_keep_their_ids_whichever_folder_is_read_first() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let (dir, drive) = (space.dir.clone(), space.drive.clone());
        write_old(&dir.join("A/x.txt"), b"x");
        write_old(&dir.join("B/keep.txt"), b"k");
        write_old(&dir.join("Z/deep/y.txt"), b"y");
        write_old(&dir.join("C/Inner/z.txt"), b"z");
        write_old(&dir.join("D/Old/w.txt"), b"w");
        scan(&env.st, &drive).await.unwrap();
        let id = |rel: &'static str| {
            let env = &env;
            let drive = drive.clone();
            async move { env.node_at(&drive, rel).await.map(|(id, _)| id) }
        };
        let (x, deep, y, inner, z) = (id("A/x.txt").await, id("Z/deep").await, id("Z/deep/y.txt").await, id("C/Inner").await, id("C/Inner/z.txt").await);
        // Into a folder read later, into one read earlier, a folder with what is in it into a folder further down, and
        // a folder removed with what is in it
        std::fs::rename(dir.join("A/x.txt"), dir.join("Z/x.txt")).unwrap();
        std::fs::rename(dir.join("Z/deep"), dir.join("B/deep")).unwrap();
        std::fs::rename(dir.join("C/Inner"), dir.join("A/Inner")).unwrap();
        std::fs::remove_dir_all(dir.join("D/Old")).unwrap();
        let r = scan(&env.st, &drive).await.unwrap();
        assert!(id("A/x.txt").await.is_none() && id("Z/deep").await.is_none() && id("C/Inner/z.txt").await.is_none() && id("D/Old/w.txt").await.is_none(), "{r:?}");
        let now = (id("Z/x.txt").await, id("B/deep").await, id("B/deep/y.txt").await, id("A/Inner").await, id("A/Inner/z.txt").await);
        assert!(now.0.is_some() && now.1.is_some() && now.2.is_some() && now.3.is_some() && now.4.is_some(), "{r:?}");
        if cfg!(unix) {
            // Recognised by their inodes: the same nodes, with their shares and permissions
            assert_eq!(now, (x, deep, y, inner, z));
            assert_eq!((r.moved, r.added, r.removed), (3, 0, 2), "{r:?}");
        }
        // Nothing left to do
        let again = scan(&env.st, &drive).await.unwrap();
        assert_eq!((again.added, again.moved, again.removed, again.changed), (0, 0, 0, 0), "{again:?}");
    }

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

    #[tokio::test]
    async fn an_item_is_taken_for_a_moved_one_only_when_it_is_provably_the_same() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let drive = folder_drive(&env.st, &space.drive).await.unwrap();
        // Items with the same device and inode number: what the index had, and what the folder has now
        let old = |kind: &str, rel: &str, size: i64, mtime: i64, birth: Option<i64>| Indexed {
            id: format!("id-{rel}"),
            parent_id: Some(drive.root_id.clone()),
            kind: kind.into(),
            fs_path: Some(rel.into()),
            fs_dev: Some(1),
            fs_ino: Some(7),
            fs_size: Some(size),
            fs_mtime_ns: Some(mtime),
            fs_birth_ns: birth,
        };
        let new = |kind: &str, rel: &str, size: i64, mtime: i64, birth: Option<i64>| Entry {
            rel: rel.into(),
            parent_rel: String::new(),
            name: rel.into(),
            is_dir: kind == "folder",
            dev: 1,
            ino: 7,
            size,
            mtime_ns: mtime,
            birth_ns: birth,
            settling: false,
        };
        // (moved, added, removed)
        let seen = |was: Indexed, now: Entry| {
            let mut report = ScanReport::default();
            let ops = plan(&drive, &[was], &[now], true, &mut report);
            assert_eq!(ops.iter().filter(|op| matches!(op, Op::Move { .. })).count(), report.moved);
            (report.moved, report.added, report.removed)
        };
        // Deleted, and a new file got its inode number (ext4 and XFS give them out again): a new file
        assert_eq!(seen(old("file", "a.xlsx", 10, 100, None), new("file", "notes.txt", 3, 200, None)), (0, 1, 1));
        // Renamed: the same size and date
        assert_eq!(seen(old("file", "a.xlsx", 10, 100, None), new("file", "b.xlsx", 10, 100, None)), (1, 0, 0));
        // Creation times, where known, tell: the same file renamed and edited, or another one
        assert_eq!(seen(old("file", "a.xlsx", 10, 100, Some(5)), new("file", "b.xlsx", 12, 300, Some(5))), (1, 0, 0));
        assert_eq!(seen(old("file", "a.xlsx", 10, 100, Some(5)), new("file", "b.xlsx", 10, 100, Some(6))), (0, 1, 1));
        // Folders only by their creation time
        assert_eq!(seen(old("folder", "Old", 0, 100, Some(5)), new("folder", "New", 0, 900, Some(5))), (1, 0, 0));
        assert_eq!(seen(old("folder", "Old", 0, 100, Some(5)), new("folder", "New", 0, 100, Some(6))), (0, 1, 1));
        assert_eq!(seen(old("folder", "Old", 0, 100, None), new("folder", "New", 0, 100, None)), (0, 1, 1));

        // A folder renamed on the server keeps its id where the file system tells when it was created
        std::fs::create_dir(space.dir.join("Sub")).unwrap();
        write_old(&space.dir.join("Sub/a.txt"), b"a");
        scan(&env.st, &space.drive).await.unwrap();
        let (sub, _) = env.node_at(&space.drive, "Sub").await.unwrap();
        let (a, _) = env.node_at(&space.drive, "Sub/a.txt").await.unwrap();
        std::fs::rename(space.dir.join("Sub"), space.dir.join("Moved")).unwrap();
        scan(&env.st, &space.drive).await.unwrap();
        if cfg!(unix) && std::fs::metadata(space.dir.join("Moved")).and_then(|m| m.created()).is_ok() {
            assert_eq!(env.node_at(&space.drive, "Moved").await.unwrap().0, sub);
        }
        if cfg!(unix) {
            assert_eq!(env.node_at(&space.drive, "Moved/a.txt").await.unwrap().0, a);
        }
    }

    #[test]
    fn only_a_small_file_is_read_as_the_spaces_marker() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-marker-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = crate::beneath::Pinned::root(&dir).unwrap();
        std::fs::write(dir.join(MARKER), "space-1\n").unwrap();
        assert_eq!(space_marker(&root).unwrap().as_deref(), Some("space-1"));
        // A large file isn't read whole
        std::fs::write(dir.join(MARKER), vec![b'x'; 8 << 20]).unwrap();
        assert!(space_marker(&root).unwrap().is_some_and(|m| m.len() <= 4096));
        // Something else than a file is no marker, and isn't waited on
        std::fs::remove_file(dir.join(MARKER)).unwrap();
        #[cfg(target_os = "linux")]
        {
            let fifo = std::ffi::CString::new(dir.join(MARKER).to_string_lossy().as_bytes()).unwrap();
            // SAFETY: a valid path
            assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
            let (tx, rx) = std::sync::mpsc::channel();
            let r = root.clone();
            std::thread::spawn(move || tx.send(space_marker(&r).ok().flatten()).unwrap());
            assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)), Ok(None));
            std::fs::remove_file(dir.join(MARKER)).unwrap();
            std::os::unix::fs::symlink(dir.with_extension("elsewhere"), dir.join(MARKER)).unwrap();
            std::fs::write(dir.with_extension("elsewhere"), "space-1").unwrap();
            assert_eq!(space_marker(&root).unwrap(), None);
            let _ = std::fs::remove_file(dir.with_extension("elsewhere"));
        }
        let _ = std::fs::remove_dir_all(&dir);
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
