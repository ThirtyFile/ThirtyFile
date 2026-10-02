//! "Compress to ZIP" and "Extract all": background jobs on the server, which the page polls for their progress.
//!
//! Compressing packs the selected items (as a download would, but deflated) into a temporary file and stores it as a
//! new file next to them, through the same staging and commit as an upload. Extracting copies the archive to a
//! temporary file, checks it against the limits below before extracting anything, extracts every file into its own
//! temporary file (counting what actually comes out, whatever the archive says), stores them, and creates the new
//! folder and everything in it in one transaction, so a failure leaves nothing half made.
//!
//! Both run as jobs (jobs.rs). A restart forgets them, and a job still running then leaves nothing behind but temporary
//! files, which the daily cleanup removes.

use std::{
    collections::HashMap,
    io::{Read, Write},
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, ready},
};

use axum::{Json, extract::State};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};

use crate::{
    auth::User,
    content,
    error::{AppError, AppResult},
    files::Source,
    fsops,
    jobs::{self, Job, Limit, Outcome, Tracker},
    logs,
    state::AppState,
    tree::{self, Need, Node},
    util::{format_bytes_u64, new_id, split_name, validate_name},
    zip::{ReadEntry, ZipWriter},
};

/// Most an archive may hold once extracted, whatever the space's quota
pub const MAX_EXTRACT_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// Most files and folders an archive may hold
pub const MAX_EXTRACT_ENTRIES: u64 = 20_000;
/// Most folders a path in an archive may go down, like an uploaded folder (each level is created while every other
/// change waits)
pub const MAX_EXTRACT_DEPTH: usize = 64;
/// Most an entry (or the whole archive) may grow when extracted. Ordinary files compress to a tenth or so, a file of
/// zeros to a thousandth: that is how a small "ZIP bomb" fills a disk. Small entries are exempt
pub const MAX_RATIO: u64 = 250;
const RATIO_EXEMPT_BYTES: u64 = 1024 * 1024;
/// Largest list of entries read into memory
const MAX_DIRECTORY_BYTES: u64 = 64 * 1024 * 1024;
/// Counts what is read through it into the job's progress
struct Counting<R> {
    inner: R,
    progress: Tracker,
}

impl<R: AsyncRead + Unpin> AsyncRead for Counting<R> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let res = Pin::new(&mut self.inner).poll_read(cx, buf);
        let n = (buf.filled().len() - before) as u64;
        if n > 0 {
            self.progress.add(n);
        }
        res
    }
}

// ───────────── Compress to ZIP ─────────────

/// Free space a ZIP file being written leaves on the data disk, which also holds the database
const DISK_RESERVE: u64 = 64 * 1024 * 1024;
/// How often (in bytes written) the data disk's free space is looked at again while a ZIP file is written
const DISK_CHECK_EVERY: u64 = 64 * 1024 * 1024;

#[cfg(test)]
thread_local! {
    /// Tests: the data disk's free space (None: what the disk says)
    static FREE_SPACE: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Free space on the disk holding `dir`, when it can be told
async fn free_space(dir: PathBuf) -> Option<u64> {
    #[cfg(test)]
    if let Some(free) = FREE_SPACE.with(|f| f.get()) {
        return Some(free);
    }
    crate::util::disk_space_soon(&dir).await.map(|(free, _)| free)
}

/// The limit a ZIP file being written went past
#[derive(Clone, Copy, Debug, PartialEq)]
enum Over {
    Quota,
    Disk,
}

/// The temporary ZIP file, which stops growing past the room left in the destination space or on the data disk (less
/// DISK_RESERVE). The selection was checked against both before writing, but deflating adds a little, files may grow
/// meanwhile, and uploads and other jobs use the disk too: its free space is looked at again every `every` bytes.
struct Capped<W> {
    inner: W,
    written: u64,
    quota: Option<u64>,
    /// The size the file may reach as of the last look at the disk (None: the disk can't tell)
    disk: Option<u64>,
    dir: PathBuf,
    every: u64,
    next_check: u64,
    checking: Option<Pin<Box<dyn Future<Output = Option<u64>> + Send>>>,
    /// Which limit stopped the writing (the error the writer returns only says that it stopped)
    over: Arc<Mutex<Option<Over>>>,
}

impl<W> Capped<W> {
    fn new(inner: W, quota: Option<u64>, disk: Option<u64>, dir: PathBuf, over: Arc<Mutex<Option<Over>>>) -> Self {
        Capped { inner, written: 0, quota, disk, dir, every: DISK_CHECK_EVERY, next_check: DISK_CHECK_EVERY, checking: None, over }
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for Capped<W> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        let this = &mut *self;
        if this.checking.is_none() && this.written >= this.next_check {
            this.checking = Some(Box::pin(free_space(this.dir.clone())));
        }
        if let Some(check) = this.checking.as_mut() {
            let free = ready!(check.as_mut().poll(cx));
            this.checking = None;
            this.disk = free.map(|f| this.written + f.saturating_sub(DISK_RESERVE));
            this.next_check = this.written + this.every;
        }
        let end = this.written + buf.len() as u64;
        let over = if this.quota.is_some_and(|q| end > q) {
            Some(Over::Quota)
        } else if this.disk.is_some_and(|d| end > d) {
            Some(Over::Disk)
        } else {
            None
        };
        if let Some(over) = over {
            *this.over.lock().unwrap() = Some(over);
            return Poll::Ready(Err(std::io::Error::other("the ZIP file doesn't fit")));
        }
        let n = ready!(Pin::new(&mut this.inner).poll_write(cx, buf))?;
        this.written += n as u64;
        Poll::Ready(Ok(n))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

fn disk_error(needed: u64, free: u64) -> AppError {
    AppError::bad_request(format!(
        "There isn't enough free space in ThirtyFile's data folder to compress these items: {needed} is needed, {free} is free",
        needed = format_bytes_u64(needed),
        free = format_bytes_u64(free)
    ))
}

#[derive(Deserialize)]
pub struct CompressReq {
    pub ids: Vec<String>,
    /// The folder the ZIP file goes into (the one the items are shown in)
    pub parent_id: String,
    /// The browser's time zone (JavaScript's getTimezoneOffset), for the times inside the ZIP
    pub tz: Option<i64>,
}

pub async fn compress(State(st): State<AppState>, user: User, Json(req): Json<CompressReq>) -> AppResult<Json<Job>> {
    let mut seen = std::collections::HashSet::new();
    let ids: Vec<&String> = req.ids.iter().filter(|id| !id.is_empty() && seen.insert(*id)).collect();
    if ids.is_empty() {
        return Err(AppError::bad_request("Select items to compress"));
    }
    if ids.len() > crate::downloads::MAX_DOWNLOAD_ITEMS {
        return Err(AppError::bad_request(format!(
            "At most {} items can be compressed at once. Compress the folder they are in, or select fewer items.",
            crate::downloads::MAX_DOWNLOAD_ITEMS
        )));
    }
    // Checked now, so a problem is reported at once rather than as a failed task
    let (roots, dest) = {
        let mut c = st.db.acquire().await?;
        let dest = tree::folder_for(&mut c, &user, &req.parent_id, Need::Write).await?;
        let mut roots = Vec::with_capacity(ids.len());
        for id in ids {
            roots.push(tree::owned_node(&mut c, &user, id).await?);
        }
        (roots, dest)
    };
    let job = jobs::start(&st, user.id, "compress", Limit::Archive)?;
    let offset = -req.tz.unwrap_or(0).clamp(-14 * 60, 14 * 60) * 60;
    jobs::spawn(&st, &job, run_compress(st.clone(), user, Tracker::of(&st, &job), roots, dest.id, offset));
    Ok(Json(job))
}

/// Like Windows: one item gives "name.zip" (without a file's own extension), several are named after their folder
fn zip_name(plan: &crate::downloads::ZipPlan, roots: &[Node]) -> String {
    let name = match roots {
        [one] if !one.is_folder() && !plan.root_names.is_empty() => format!("{}.zip", split_name(&plan.root_names[0], false).0),
        _ => plan.file_name("Archive"),
    };
    validate_name(&name).unwrap_or_else(|_| "Archive.zip".into())
}

async fn run_compress(st: AppState, user: User, progress: Tracker, roots: Vec<Node>, dest_id: String, offset: i64) -> AppResult<Outcome> {
    let plan = crate::downloads::zip_plan(&st, roots.clone(), offset).await?;
    let name = zip_name(&plan, &roots);
    progress.set_total(plan.bytes);

    // Before writing: what is selected, as it is, must fit in the space and on the data disk, where the temporary file
    // goes (the ZIP is usually smaller, but how much smaller isn't known until it is written)
    let room = {
        let mut c = st.db.acquire().await?;
        let dest = tree::folder_for(&mut c, &user, &dest_id, Need::Write).await?;
        tree::room_left(&mut c, dest.drive(), None).await?
    };
    if let Some((left, drive)) = &room
        && plan.bytes > (*left).max(0) as u64
    {
        return Err(tree::quota_error(drive));
    }
    let free = free_space(st.tmp_dir()).await;
    if let Some(free) = free
        && plan.bytes.saturating_add(DISK_RESERVE) > free
    {
        return Err(disk_error(plan.bytes, free.saturating_sub(DISK_RESERVE)));
    }

    let tmp = st.tmp_dir().join(format!("zip-{}", new_id()));
    let over = Arc::new(Mutex::new(None));
    let written = async {
        let file = tokio::fs::File::create(&tmp).await?;
        let quota = room.as_ref().map(|(left, _)| (*left).max(0) as u64);
        let file = Capped::new(file, quota, free.map(|f| f.saturating_sub(DISK_RESERVE)), st.tmp_dir(), over.clone());
        let mut zip = ZipWriter::deflating(tokio::io::BufWriter::with_capacity(256 * 1024, file));
        let mut walk = plan.walk();
        while let Some(item) = walk.next(&st).await? {
            match item.blob {
                None => zip.add_dir(&item.path, item.mtime).await?,
                Some(source) => {
                    let reader = source.open(&st, 0, item.size).await?;
                    zip.add_file(&item.path, Counting { inner: reader, progress: progress.clone() }, item.size, item.mtime).await?;
                }
            }
        }
        let mut out = zip.finish().await?;
        out.flush().await?;
        out.into_inner().inner.sync_all().await?;
        Ok::<_, AppError>(())
    }
    .await;
    if let Err(e) = written {
        let _ = tokio::fs::remove_file(&tmp).await;
        let over = *over.lock().unwrap();
        return Err(match (over, &room) {
            (Some(Over::Quota), Some((_, drive))) => tree::quota_error(drive),
            (Some(Over::Disk), _) => AppError::bad_request("Compressing stopped because ThirtyFile's data folder is running out of free space"),
            _ => e,
        });
    }
    let result = store_new_file(&st, &user, &dest_id, &name, &tmp, "compress").await;
    let _ = tokio::fs::remove_file(&tmp).await;
    result.map(|(id, name)| Outcome::node(id, name))
}

/// Stores a finished temporary file as a new file in `folder_id` (numbered if the name is taken), like an upload
/// (content.rs). Returns its id and name
async fn store_new_file(st: &AppState, user: &User, folder_id: &str, name: &str, tmp: &std::path::Path, action: &str) -> AppResult<(String, String)> {
    let folder = tree::folder_for(&mut *st.db.acquire().await?, user, folder_id, Need::Write).await?;
    let size = tokio::fs::metadata(tmp).await?.len();
    // Checked first, so a file that can't fit isn't stored at all
    tree::check_quota(&mut *st.db.acquire().await?, folder.drive(), size as i64).await?;
    // Takes the temporary file over (the caller removes it when it is left)
    let staged = content::stage(st, &folder, content::Received { path: tmp.to_path_buf(), size, hash: None }).await?;
    let mut turn = staged.turn(st).await;
    let _w = st.write_lock.lock().await;
    let result = async {
        turn.ready()?;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let folder = tree::folder_for(&mut tx, user, folder_id, Need::Write).await?;
        staged.check(&folder)?;
        tree::check_quota(&mut tx, folder.drive(), size as i64).await?;
        let name = content::free_name(&mut tx, &folder, name).await?;
        let (id, written) = content::create(&mut tx, &staged, user.id, &folder, &name, None).await?;
        tree::touch(&mut tx, &folder.id).await?;
        let node = tree::get_node(&mut tx, &id).await?;
        logs::record_activity(&mut tx, user, node.as_ref(), action, "").await?;
        tx.commit().await?;
        Ok((id, name, written))
    }
    .await;
    match result {
        Ok((id, name, written)) => {
            staged.finish(st, written).await;
            Ok((id, name))
        }
        Err(e) => {
            staged.abandon(st).await;
            Err(e)
        }
    }
}

// ───────────── Extract all ─────────────

#[derive(Deserialize)]
pub struct ExtractReq {
    pub id: String,
}

/// Whether a file is taken for a ZIP archive
pub fn is_zip(n: &Node) -> bool {
    !n.is_folder() && (n.name.to_lowercase().ends_with(".zip") || matches!(n.mime.as_str(), "application/zip" | "application/x-zip-compressed"))
}

pub async fn extract(State(st): State<AppState>, user: User, Json(req): Json<ExtractReq>) -> AppResult<Json<Job>> {
    let (zip, parent) = {
        let mut c = st.db.acquire().await?;
        let zip = tree::owned_node(&mut c, &user, &req.id).await?;
        if !is_zip(&zip) {
            return Err(AppError::bad_request("Only ZIP files can be extracted"));
        }
        let parent_id = zip.parent_id.clone().ok_or_else(|| AppError::not_found("Folder not found"))?;
        let parent = tree::folder_for(&mut c, &user, &parent_id, Need::Write).await?;
        (zip, parent)
    };
    let job = jobs::start(&st, user.id, "extract", Limit::Archive)?;
    jobs::spawn(&st, &job, run_extract(st.clone(), user, Tracker::of(&st, &job), zip, parent.id));
    Ok(Json(job))
}

/// A file or folder to create, from an entry of the archive
struct Planned {
    /// Folder names on the way, from the new folder down
    dirs: Vec<String>,
    /// The file's name; None for a folder entry
    file: Option<String>,
    entry: ReadEntry,
}

/// Checks the archive's list of entries against the limits and turns their paths into names; nothing is extracted yet
fn plan_entries(entries: Vec<ReadEntry>, archive_size: u64) -> AppResult<(Vec<Planned>, u64)> {
    let mut out = Vec::with_capacity(entries.len());
    let mut total = 0u64;
    // The folders the paths imply, each once (by its parent's number and its name), counted with the files: an entry
    // "a/b/c/x" is four items, not one
    let mut folders: HashMap<(usize, String), usize> = HashMap::new();
    let mut files = 0u64;
    for entry in entries {
        let path = entry.name.replace('\\', "/");
        let shown = path.trim_end_matches('/').to_string();
        // Absolute paths ("/etc/…", "C:/…") and ".." would put files outside the new folder
        let absolute = path.starts_with('/') || path.as_bytes().get(1) == Some(&b':');
        let mut parts: Vec<String> = Vec::new();
        for part in path.split('/').filter(|p| !p.is_empty() && *p != ".") {
            if part == ".." || absolute {
                return Err(AppError::bad_request(format!("\"{shown}\" in the ZIP file points outside the folder it would be extracted to")));
            }
            let name = validate_name(part).map_err(|e| AppError::bad_request(format!("\"{shown}\" in the ZIP file can't be extracted: {}", e.message)))?;
            parts.push(name);
            // A file below the deepest folder allowed is one part more
            if parts.len() > MAX_EXTRACT_DEPTH + 1 {
                return Err(too_deep());
            }
        }
        let Some(last) = parts.pop() else { continue };
        if parts.len() + usize::from(entry.is_dir) > MAX_EXTRACT_DEPTH {
            return Err(too_deep());
        }
        let mut parent = 0;
        for name in parts.iter().chain(entry.is_dir.then_some(&last)) {
            let next = folders.len() + 1;
            parent = *folders.entry((parent, name.clone())).or_insert(next);
        }
        files += u64::from(!entry.is_dir);
        if folders.len() as u64 + files > MAX_EXTRACT_ENTRIES {
            return Err(AppError::bad_request(format!("This ZIP file holds more than {MAX_EXTRACT_ENTRIES} items, more than can be extracted at once")));
        }
        if entry.is_dir {
            parts.push(last);
            out.push(Planned { dirs: parts, file: None, entry });
            continue;
        }
        if !entry.supported() {
            return Err(AppError::bad_request(if entry.encrypted {
                format!("\"{shown}\" in the ZIP file is protected with a password, which isn't supported")
            } else {
                format!("\"{shown}\" in the ZIP file is compressed in a way that isn't supported")
            }));
        }
        if entry.size > RATIO_EXEMPT_BYTES && entry.size / MAX_RATIO > entry.csize {
            return Err(too_compressed());
        }
        total = total.saturating_add(entry.size);
        out.push(Planned { dirs: parts, file: Some(last), entry });
    }
    if total > MAX_EXTRACT_BYTES {
        return Err(AppError::bad_request(format!(
            "This ZIP file holds more than {} once extracted, more than can be extracted here",
            format_bytes_u64(MAX_EXTRACT_BYTES)
        )));
    }
    if total > RATIO_EXEMPT_BYTES && total / MAX_RATIO > archive_size {
        return Err(too_compressed());
    }
    Ok((out, total))
}

fn too_deep() -> AppError {
    AppError::bad_request(format!("A path in this ZIP file goes more than {MAX_EXTRACT_DEPTH} folders deep, more than can be extracted"))
}

fn too_compressed() -> AppError {
    AppError::bad_request("This ZIP file would grow far more than ordinary files do when extracted, so it isn't extracted")
}

fn damaged() -> AppError {
    AppError::bad_request("This ZIP file is damaged and can't be extracted")
}

/// A file extracted to a temporary file: its hash and size
struct Extracted {
    tmp: PathBuf,
    hash: String,
    size: u64,
}

/// Extracts one entry into a temporary file, stopping as soon as more comes out than it states (whatever the archive
/// says, never more than that is written), and checking its CRC
fn extract_entry(archive: &std::path::Path, entry: &ReadEntry, tmp: PathBuf, progress: &dyn Fn(u64)) -> AppResult<Extracted> {
    let mut file = std::io::BufReader::new(std::fs::File::open(archive)?);
    let reader = crate::zip::open_entry(&mut file, entry).map_err(|_| damaged())?;
    let mut limited = reader.take(entry.size + 1);
    let mut out = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
    let mut sha = Sha256::new();
    let mut crc = crc32fast::Hasher::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut size = 0u64;
    let result = (|| -> AppResult<()> {
        loop {
            let n = limited.read(&mut buf).map_err(|_| damaged())?;
            if n == 0 {
                break;
            }
            size += n as u64;
            if size > entry.size {
                return Err(damaged());
            }
            sha.update(&buf[..n]);
            crc.update(&buf[..n]);
            out.write_all(&buf[..n])?;
            progress(n as u64);
        }
        if size != entry.size || crc.finalize() != entry.crc {
            return Err(damaged());
        }
        // On disk before it is stored under its hash or renamed into a folder
        let file = out.into_inner().map_err(|e| e.into_error())?;
        crate::fsops::sync_file(&file, &tmp)?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(Extracted { tmp, hash: hex::encode(sha.finalize()), size })
}

async fn run_extract(st: AppState, user: User, progress: Tracker, zip: Node, parent_id: String) -> AppResult<Outcome> {
    // The archive is read in any order, so it is copied to a temporary file first (a folder space's file is read in place)
    let source = Source::resolve(&st, &zip).await?;
    // A folder space's file stays open meanwhile, so the path keeps leading to it
    let mut _open = None;
    let (archive, copied) = match &source {
        Source::File(path) => {
            let (file, stable) = path.open_stable().map_err(|_| AppError::not_found("File not found"))?;
            _open = Some(file);
            (stable, false)
        }
        Source::Stored { .. } => {
            let (size, _) = source.describe(zip.size as u64).await?;
            let tmp = st.tmp_dir().join(format!("unzip-{}", new_id()));
            let copy = async {
                let mut reader = source.open(&st, 0, size).await?;
                let mut out = tokio::fs::File::create(&tmp).await?;
                tokio::io::copy(&mut reader, &mut out).await?;
                out.flush().await?;
                Ok::<_, AppError>(())
            }
            .await;
            if let Err(e) = copy {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(e);
            }
            (tmp, true)
        }
    };
    let result = extract_into(&st, &user, &progress, &zip, &parent_id, &archive).await;
    if copied {
        let _ = tokio::fs::remove_file(&archive).await;
    }
    result.map(|(id, name)| Outcome::node(id, name))
}

async fn extract_into(st: &AppState, user: &User, progress: &Tracker, zip: &Node, parent_id: &str, archive: &std::path::Path) -> AppResult<(String, String)> {
    let path = archive.to_path_buf();
    let (plan, total) = tokio::task::spawn_blocking(move || -> AppResult<_> {
        let mut file = std::io::BufReader::new(std::fs::File::open(&path)?);
        let size = file.get_ref().metadata()?.len();
        let entries = crate::zip::read_entries(&mut file, MAX_EXTRACT_ENTRIES, MAX_DIRECTORY_BYTES).map_err(|e| {
            if e.kind() == std::io::ErrorKind::FileTooLarge {
                AppError::bad_request(format!("This ZIP file holds more than {MAX_EXTRACT_ENTRIES} items, more than can be extracted at once"))
            } else {
                damaged()
            }
        })?;
        plan_entries(entries, size)
    })
    .await??;
    progress.set_total(total);
    let parent = tree::folder_for(&mut *st.db.acquire().await?, user, parent_id, Need::Write).await?;
    let drive = parent.drive().to_string();
    // What the archive states is checked against the quota before anything is extracted
    tree::check_quota(&mut *st.db.acquire().await?, &drive, total as i64).await?;
    if parent.in_folder_space() {
        return extract_into_folder(st, user, progress, zip, &parent, archive, plan).await;
    }

    let mut staged: Vec<(usize, content::Staged)> = Vec::new();
    let mut written: HashMap<usize, content::Written> = HashMap::new();
    let result = async {
        let mut actual = 0i64;
        for (i, p) in plan.iter().enumerate() {
            if p.file.is_none() {
                continue;
            }
            let (archive, entry, tmp) = (archive.to_path_buf(), p.entry.clone(), st.tmp_dir().join(format!("unzip-{}", new_id())));
            let p = progress.clone();
            let x = tokio::task::spawn_blocking(move || extract_entry(&archive, &entry, tmp, &|n| p.add(n))).await??;
            actual += x.size as i64;
            let received = content::Received { path: x.tmp, size: x.size, hash: Some(x.hash) };
            staged.push((i, content::stage(st, &parent, received).await?));
        }
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let parent = tree::folder_for(&mut tx, user, parent_id, Need::Write).await?;
        for (_, s) in &staged {
            s.check(&parent)?;
        }
        tree::check_quota(&mut tx, parent.drive(), actual).await?;
        // The new folder is named after the archive
        let stem = validate_name(split_name(&zip.name, false).0).unwrap_or_else(|_| "Extracted".into());
        let name = tree::unique_name(&mut tx, &parent.id, &stem, true).await?;
        let root = crate::content::create_folder(&mut tx, user.id, &parent.id, &name).await?;
        let root_node = tree::get_node(&mut tx, &root).await?.ok_or_else(|| AppError::not_found("Folder not found"))?;
        let mut folders: HashMap<Vec<String>, Node> = HashMap::new();
        folders.insert(Vec::new(), root_node.clone());
        let blobs = staged.iter().map(|(i, s)| (*i, s)).collect::<HashMap<_, _>>();
        for (i, p) in plan.iter().enumerate() {
            // Folders on the way, created once each (a name taken by a file gets a number)
            let mut key = Vec::new();
            let mut folder = root_node.clone();
            for d in &p.dirs {
                key.push(d.clone());
                folder = match folders.get(&key) {
                    Some(n) => n.clone(),
                    None => {
                        let id = crate::content::ensure_folders(&mut tx, user.id, &folder.id, d, "").await?;
                        let n = tree::get_node(&mut tx, &id).await?.ok_or_else(|| AppError::not_found("Folder not found"))?;
                        folders.insert(key.clone(), n.clone());
                        n
                    }
                };
            }
            let (Some(file), Some(s)) = (&p.file, blobs.get(&i)) else { continue };
            let name = content::free_name(&mut tx, &folder, file).await?;
            let (_, w) = content::create(&mut tx, s, user.id, &folder, &name, None).await?;
            written.insert(i, w);
        }
        let node = tree::get_node(&mut tx, &root).await?;
        logs::record_activity(&mut tx, user, node.as_ref(), "extract", &zip.name).await?;
        tx.commit().await?;
        Ok((root, name))
    }
    .await;
    match result {
        Ok(done) => {
            // (copies stored twice, the same content in another location, are removed in the background)
            for (i, s) in staged {
                s.finish(st, written.remove(&i).unwrap_or_default()).await;
            }
            Ok(done)
        }
        Err(e) => {
            for (_, s) in staged {
                s.abandon(st).await;
            }
            Err(e)
        }
    }
}

/// Extracting into a folder space: the entries are written into a new folder under a name scans ignore, which is put in
/// place and indexed once everything is out (`fsops::place_folder`); should anything fail, it is removed again
async fn extract_into_folder(
    st: &AppState,
    user: &User,
    progress: &Tracker,
    zip: &Node,
    parent: &Node,
    archive: &std::path::Path,
    plan: Vec<Planned>,
) -> AppResult<(String, String)> {
    let staged = fsops::staging(parent).await?;
    for p in &plan {
        // Names a folder space can't show (.DS_Store, Thumbs.db, ThirtyFile's own…) are left out
        if p.dirs.iter().chain(&p.file).any(|n| crate::folders::ignored(n)) {
            continue;
        }
        let dir = fsops::make_dirs(&staged.top, &p.dirs).map_err(fsops::disk_error)?;
        let Some(file) = &p.file else { continue };
        let (archive, entry, tmp) = (archive.to_path_buf(), p.entry.clone(), st.tmp_dir().join(format!("unzip-{}", new_id())));
        let (p, file) = (progress.clone(), file.clone());
        tokio::task::spawn_blocking(move || -> AppResult<()> {
            let x = extract_entry(&archive, &entry, tmp, &|n| p.add(n))?;
            let moved = fsops::move_in(&x.tmp, &dir, &file);
            if moved.is_err() {
                let _ = std::fs::remove_file(&x.tmp);
            }
            moved.map_err(fsops::disk_error)
        })
        .await??;
    }
    // The new folder is named after the archive
    let stem = validate_name(split_name(&zip.name, false).0).ok().filter(|s| !crate::folders::ignored(s)).unwrap_or_else(|| "Extracted".into());
    fsops::place_folder(st, user, staged, &parent.id, &stem, "extract", &zip.name).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;
    use axum::{extract::Path, http::StatusCode};

    async fn wait(env: &testutil::TestEnv, id: &str) -> Job {
        jobs::wait_for(&env.st, id).await
    }

    /// The items in a folder: (name, kind, content)
    async fn listing(env: &testutil::TestEnv, folder: &str) -> Vec<(String, String, Option<String>)> {
        sqlx::query_as("SELECT name, kind, blob_hash FROM nodes WHERE parent_id = ? AND trashed_at IS NULL ORDER BY name")
            .bind(folder)
            .fetch_all(&env.st.db)
            .await
            .unwrap()
    }

    async fn id_of(env: &testutil::TestEnv, folder: &str, name: &str) -> String {
        let (id,): (String,) = sqlx::query_as("SELECT id FROM nodes WHERE parent_id = ? AND name = ? AND trashed_at IS NULL")
            .bind(folder)
            .bind(name)
            .fetch_one(&env.st.db)
            .await
            .unwrap();
        id
    }

    async fn compress_now(env: &testutil::TestEnv, user: &User, ids: &[&str], parent: &str) -> AppResult<Job> {
        let req = CompressReq { ids: ids.iter().map(|s| s.to_string()).collect(), parent_id: parent.into(), tz: Some(-480) };
        let Json(job) = compress(State(env.st.clone()), user.clone(), Json(req)).await?;
        Ok(wait(env, &job.id).await)
    }

    async fn extract_now(env: &testutil::TestEnv, user: &User, id: &str) -> AppResult<Job> {
        let Json(job) = extract(State(env.st.clone()), user.clone(), Json(ExtractReq { id: id.into() })).await?;
        Ok(wait(env, &job.id).await)
    }

    /// A ZIP made with the writer: (path, content); a path ending in "/" is a folder
    async fn zip_of(entries: &[(&str, &[u8])], deflate: bool) -> Vec<u8> {
        let zip = if deflate { ZipWriter::deflating(Vec::new()) } else { ZipWriter::new(Vec::new()) };
        // Written as given: some tests need names no ZIP made here would have
        let mut zip = zip.raw_names();
        for (path, data) in entries {
            if path.ends_with('/') {
                zip.add_dir(path, 1_700_000_000).await.unwrap();
            } else {
                zip.add_file(path, *data, data.len() as u64, 1_700_000_000).await.unwrap();
            }
        }
        zip.finish().await.unwrap()
    }

    #[tokio::test]
    async fn extracted_files_are_on_disk_before_they_take_their_name() {
        let env = testutil::env().await;
        let archive = env.st.tmp_dir().join(format!("in-{}", new_id()));
        std::fs::write(&archive, zip_of(&[("a.txt", b"alpha")], true).await).unwrap();
        let entries = crate::zip::read_entries(&mut std::io::BufReader::new(std::fs::File::open(&archive).unwrap()), 10, 1 << 20).unwrap();
        let tmp = env.st.tmp_dir().join(format!("unzip-{}", new_id()));
        let x = extract_entry(&archive, &entries[0], tmp.clone(), &|_| {}).unwrap();
        assert_eq!(std::fs::read(&x.tmp).unwrap(), b"alpha");
        assert!(crate::fsops::testing::was_synced(&tmp));
    }

    #[tokio::test]
    async fn items_are_compressed_next_to_them_and_extracted_back() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let docs = env.folder(&amy, amy.root(), "Docs").await;
        let text = "All work and no play. ".repeat(5000);
        let a = env.stored_file(&amy, &docs, "a.txt", text.as_bytes()).await;
        let sub = env.folder(&amy, &docs, "Sub").await;
        env.stored_file(&amy, &sub, "b.txt", b"inside").await;
        env.folder(&amy, &sub, "Empty").await;

        // Several items: named after their folder, compressed
        let job = compress_now(&env, &amy, &[&a, &sub], &docs).await.unwrap();
        assert_eq!((job.state, job.name.as_deref(), job.done, job.total), ("done", Some("Docs.zip"), text.len() as u64 + 6, text.len() as u64 + 6));
        let zip = job.node_id.unwrap();
        let node = tree::get_node(&mut env.st.db.acquire().await.unwrap(), &zip).await.unwrap().unwrap();
        assert_eq!((node.parent_id.as_deref(), node.mime.as_str()), (Some(docs.as_str()), "application/zip"));
        assert!(node.size < text.len() as i64 / 10, "compressed to {}", node.size);
        let (used,): (i64,) = sqlx::query_as("SELECT used_bytes FROM drives WHERE id = ?").bind(node.drive()).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(used, node.size, "the new file is counted in the space's usage");

        // Someone else can't follow the job, or compress items they can't open
        assert_eq!(jobs::get(State(env.st.clone()), ben.clone(), Path(job.id.clone())).await.unwrap_err().status, StatusCode::NOT_FOUND);
        assert!(compress_now(&env, &ben, &[&a], ben.root()).await.is_err());
        assert!(extract_now(&env, &ben, &zip).await.is_err());

        // Extracted into a new folder named after it: the same files, folders (empty ones too) and contents
        let job = extract_now(&env, &amy, &zip).await.unwrap();
        assert_eq!((job.state, job.name.as_deref(), job.error), ("done", Some("Docs"), None));
        let out = job.node_id.unwrap();
        let hash = |s: &[u8]| Some(crate::util::sha256_hex(s));
        assert_eq!(listing(&env, &out).await, [("Sub".into(), "folder".into(), None), ("a.txt".into(), "file".into(), hash(text.as_bytes()))]);
        let out_sub = id_of(&env, &out, "Sub").await;
        assert_eq!(listing(&env, &out_sub).await, [("Empty".into(), "folder".into(), None), ("b.txt".into(), "file".into(), hash(b"inside"))]);
        // Again: a numbered folder; one file gives "name.zip" without its own extension
        assert_eq!(extract_now(&env, &amy, &zip).await.unwrap().name.as_deref(), Some("Docs (1)"));
        assert_eq!(compress_now(&env, &amy, &[&a], &docs).await.unwrap().name.as_deref(), Some("a.zip"));
        assert_eq!(compress_now(&env, &amy, &[&a], &docs).await.unwrap().name.as_deref(), Some("a (1).zip"));

        // Both are in the activity log
        let actions: Vec<(String,)> = sqlx::query_as("SELECT action FROM activity WHERE action IN ('compress', 'extract')").fetch_all(&env.st.db).await.unwrap();
        assert_eq!(actions.len(), 5);
        // Content references are counted: the extracted a.txt shares the original's
        let (refs,): (i64,) = sqlx::query_as("SELECT refcount FROM blobs WHERE hash = ?").bind(hash(text.as_bytes())).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(refs, 3);
    }

    #[tokio::test]
    async fn dangerous_or_oversized_archives_are_refused() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let fails = |entries: Vec<(&'static str, Vec<u8>)>, deflate: bool, name: &'static str| {
            let (env, amy) = (&env, &amy);
            async move {
                let refs: Vec<(&str, &[u8])> = entries.iter().map(|(p, d)| (*p, d.as_slice())).collect();
                let bytes = zip_of(&refs, deflate).await;
                let id = env.stored_file(amy, amy.root(), name, &bytes).await;
                let job = extract_now(env, amy, &id).await.unwrap();
                assert_eq!(job.state, "failed", "{name}");
                job.error.unwrap()
            }
        };
        let before = listing(&env, amy.root()).await.len();
        assert!(fails(vec![("../evil.txt", b"x".to_vec())], false, "up.zip").await.contains("points outside"));
        assert!(fails(vec![("ok/../../evil.txt", b"x".to_vec())], false, "up2.zip").await.contains("points outside"));
        assert!(fails(vec![("/etc/evil", b"x".to_vec())], false, "abs.zip").await.contains("points outside"));
        assert!(fails(vec![("C:\\Windows\\evil.dll", b"x".to_vec())], false, "drive.zip").await.contains("points outside"));
        assert!(fails(vec![("docs/what?.txt", b"x".to_vec())], false, "name.zip").await.contains("can't be extracted: Name can't contain ?"));
        // 20 MB of zeros deflates to about 20 KB
        let bomb = fails(vec![("zeros.bin", vec![0u8; 20 * 1024 * 1024])], true, "bomb.zip").await;
        assert!(bomb.contains("grow far more"), "{bomb}");
        // Too many entries
        let many: Vec<(&'static str, Vec<u8>)> = (0..=MAX_EXTRACT_ENTRIES).map(|i| (&*Box::leak(format!("d{i}/").into_boxed_str()), Vec::new())).collect();
        assert!(fails(many, false, "many.zip").await.contains("more than 20000 items"));
        // Folders nested too deep, and more folders than items once the folders each path implies are counted
        let deep: &'static str = Box::leak(format!("{}x.txt", "a/".repeat(MAX_EXTRACT_DEPTH + 1)).into_boxed_str());
        assert!(fails(vec![(deep, b"x".to_vec())], false, "deep.zip").await.contains("folders deep"));
        let implied: Vec<(&'static str, Vec<u8>)> = (0..400).map(|i| (&*Box::leak(format!("e{i}/{}x.txt", "f/".repeat(59)).into_boxed_str()), Vec::new())).collect();
        assert!(fails(implied, false, "implied.zip").await.contains("more than 20000 items"));
        // Not a ZIP at all
        let junk = env.stored_file(&amy, amy.root(), "junk.zip", b"this is not a zip").await;
        assert!(extract_now(&env, &amy, &junk).await.unwrap().error.unwrap().contains("damaged"));

        // An entry stating less than it holds: never more than stated is written
        let mut bytes = zip_of(&[("x.txt", &[b'x'; 1000])], false).await;
        let cd = (0..bytes.len() - 4).rev().find(|&i| bytes[i..i + 4] == 0x0201_4b50u32.to_le_bytes()).unwrap();
        bytes[cd + 24..cd + 28].copy_from_slice(&10u32.to_le_bytes());
        let liar = env.stored_file(&amy, amy.root(), "liar.zip", &bytes).await;
        assert!(extract_now(&env, &amy, &liar).await.unwrap().error.unwrap().contains("damaged"));

        // What the archive holds counts against the quota before anything is extracted
        sqlx::query("UPDATE users SET quota_bytes = 50000 WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
        let big = fails(vec![("big.txt", vec![b'a'; 60_000])], false, "big.zip").await;
        assert!(big.starts_with("Not enough storage space"), "{big}");

        // Nothing was created by any of them, and no temporary files are left
        let names: Vec<String> = listing(&env, amy.root()).await.into_iter().map(|(n, _, _)| n).collect();
        assert_eq!(names.len(), before + 12, "{names:?}");
        assert!(names.iter().all(|n| n.ends_with(".zip")));
        let left: Vec<_> = std::fs::read_dir(env.st.tmp_dir()).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
        assert!(left.iter().all(|n| !n.to_string_lossy().starts_with("unzip-")), "{left:?}");
    }

    #[tokio::test]
    async fn a_zip_made_in_a_folder_space_must_fit_in_it() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let space = env.folder_space("Scans").await;
        testutil::write_old(&space.dir.join("page.txt"), crate::util::new_id().repeat(40).as_bytes());
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (page, size) = env.node_at(&space.drive, "page.txt").await.unwrap();
        sqlx::query("UPDATE drives SET quota_bytes = ? WHERE id = ?").bind(size + 10).bind(&space.drive).execute(&env.st.db).await.unwrap();
        let job = compress_now(&env, &admin, &[&page], &space.root).await.unwrap();
        assert!(job.error.as_deref().unwrap_or_default().starts_with("Not enough storage space"), "{job:?}");
        assert!(!space.dir.join("page.zip").exists());
        assert!(env.node_at(&space.drive, "page.zip").await.is_none());
    }

    #[tokio::test]
    async fn compressing_checks_the_room_left_before_writing_anything() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        // Deflates to a few hundred bytes
        let text = "All work and no play. ".repeat(3000);
        let a = env.stored_file(&amy, amy.root(), "a.txt", text.as_bytes()).await;
        let zips = || std::fs::read_dir(env.st.tmp_dir()).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().starts_with("zip-")).count();

        // The selection is larger than the room left in the space: refused before anything is read or written, even
        // though it would compress well
        let (used,): (i64,) = sqlx::query_as("SELECT used_bytes FROM drives WHERE id = ?").bind(env.drive_of(&a).await).fetch_one(&env.st.db).await.unwrap();
        sqlx::query("UPDATE users SET quota_bytes = ? WHERE id = ?").bind(used + text.len() as i64 / 2).bind(amy.id).execute(&env.st.db).await.unwrap();
        let job = compress_now(&env, &amy, &[&a], amy.root()).await.unwrap();
        assert!(job.error.as_deref().unwrap_or_default().starts_with("Not enough storage space"), "{job:?}");
        assert_eq!(job.done, 0);
        sqlx::query("UPDATE users SET quota_bytes = 0 WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();

        // The temporary file is written to the data disk, which holds the database: it must fit there too, with room to spare
        FREE_SPACE.set(Some(text.len() as u64 + DISK_RESERVE - 1));
        let job = compress_now(&env, &amy, &[&a], amy.root()).await.unwrap();
        assert!(job.error.as_deref().unwrap_or_default().contains("free space in ThirtyFile's data folder"), "{job:?}");
        assert_eq!(job.done, 0);
        FREE_SPACE.set(Some(text.len() as u64 + DISK_RESERVE));
        let job = compress_now(&env, &amy, &[&a], amy.root()).await.unwrap();
        assert_eq!((job.state, job.name.as_deref()), ("done", Some("a.zip")), "{job:?}");
        FREE_SPACE.set(None);
        assert_eq!(zips(), 0);
    }

    #[tokio::test]
    async fn a_zip_file_stops_growing_past_the_room_left() {
        let dir = std::env::temp_dir();
        let chunk = [7u8; 100];
        // The space's room
        let over = Arc::new(Mutex::new(None));
        let mut zip = Capped::new(Vec::new(), Some(250), None, dir.clone(), over.clone());
        zip.write_all(&chunk).await.unwrap();
        zip.write_all(&chunk).await.unwrap();
        assert!(zip.write_all(&chunk).await.is_err());
        assert_eq!((*over.lock().unwrap(), zip.inner.len()), (Some(Over::Quota), 200));

        // The data disk's free space, looked at again as the file grows (uploads and other jobs use the disk too)
        let over = Arc::new(Mutex::new(None));
        let mut zip = Capped::new(Vec::new(), None, Some(u64::MAX), dir, over.clone());
        zip.every = 100;
        zip.next_check = 100;
        FREE_SPACE.set(Some(DISK_RESERVE + 1000));
        zip.write_all(&chunk).await.unwrap();
        zip.write_all(&chunk).await.unwrap();
        FREE_SPACE.set(Some(DISK_RESERVE + 50));
        assert!(zip.write_all(&chunk).await.is_err());
        assert_eq!((*over.lock().unwrap(), zip.inner.len()), (Some(Over::Disk), 200));
        FREE_SPACE.set(None);
    }

    #[tokio::test]
    async fn zip_files_are_made_and_extracted_in_folder_spaces() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let space = env.folder_space("Scans").await;
        testutil::write_old(&space.dir.join("page.txt"), b"scanned page");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (page, _) = env.node_at(&space.drive, "page.txt").await.unwrap();

        let job = compress_now(&env, &admin, &[&page], &space.root).await.unwrap();
        assert_eq!((job.state, job.name.as_deref()), ("done", Some("page.zip")), "{:?}", job.error);
        let bytes = std::fs::read(space.dir.join("page.zip")).unwrap();
        let mut cursor = std::io::Cursor::new(bytes);
        let entries = crate::zip::read_entries(&mut cursor, 10, 1 << 20).unwrap();
        assert_eq!(entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["page.txt"]);
        assert!(env.node_at(&space.drive, "page.zip").await.is_some());

        // Extracted into a folder named after it, on disk and in the index; again: a numbered folder
        let zip = job.node_id.unwrap();
        let job = extract_now(&env, &admin, &zip).await.unwrap();
        assert_eq!((job.state, job.name.as_deref()), ("done", Some("page")), "{:?}", job.error);
        assert_eq!(std::fs::read(space.dir.join("page/page.txt")).unwrap(), b"scanned page");
        assert_eq!(env.node_at(&space.drive, "page").await.unwrap().0, job.node_id.clone().unwrap());
        assert!(env.node_at(&space.drive, "page/page.txt").await.is_some());
        let job = extract_now(&env, &admin, &zip).await.unwrap();
        assert_eq!(job.name.as_deref(), Some("page (1)"));
        // Nothing half-made is left once the job is done (not even for a moment), and a scan finds nothing new
        let names: Vec<String> = std::fs::read_dir(&space.dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert!(!names.iter().any(|n| n.starts_with(".thirtyfile-copy-")), "{names:?}");
        let r = crate::folders::scan(&env.st, &space.drive).await.unwrap();
        assert_eq!((r.added, r.removed), (0, 0), "{r:?}");

        // Names a folder space doesn't show are left out: a Mac's .DS_Store, ThirtyFile's own names
        let mac = zip_of(&[("photos/.DS_Store", b"finder"), ("photos/a.jpg", b"jpeg"), (".thirtyfile-trash/x/b.txt", b"b"), ("Thumbs.db", b"t")], false).await;
        testutil::write_old(&space.dir.join("mac.zip"), &mac);
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (mac_id, _) = env.node_at(&space.drive, "mac.zip").await.unwrap();
        let job = extract_now(&env, &admin, &mac_id).await.unwrap();
        assert_eq!((job.state, job.name.as_deref()), ("done", Some("mac")), "{:?}", job.error);
        let mut names: Vec<String> = std::fs::read_dir(space.dir.join("mac")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["photos"]);
        let names: Vec<String> = std::fs::read_dir(space.dir.join("mac/photos")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["a.jpg"]);

        // A damaged archive leaves nothing behind
        let bad = space.dir.join("bad.zip");
        let mut bytes = zip_of(&[("a/b.txt", b"hello")], false).await;
        let len = bytes.len();
        bytes[len / 2] ^= 0xff;
        std::fs::write(&bad, &bytes).unwrap();
        testutil::write_old(&bad, &bytes);
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (bad_id, _) = env.node_at(&space.drive, "bad.zip").await.unwrap();
        let job = extract_now(&env, &admin, &bad_id).await.unwrap();
        assert_eq!(job.state, "failed");
        assert!(!space.dir.join("bad").exists());
        for _ in 0..50 {
            if !std::fs::read_dir(&space.dir).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with(".thirtyfile-copy-")) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!std::fs::read_dir(&space.dir).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with(".thirtyfile-copy-")));
    }
}
