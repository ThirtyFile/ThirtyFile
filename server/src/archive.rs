//! "Compress to ZIP" and "Extract all": background jobs on the server, which the page polls for their progress.
//!
//! Compressing packs the selected items (as a download would, but deflated) into a temporary file and stores it as a
//! new file next to them, through the same staging and commit as an upload. Extracting copies the archive to a
//! temporary file, checks it against the limits below before extracting anything, extracts every file into its own
//! temporary file (counting what actually comes out, whatever the archive says), stores them, and creates the new
//! folder and everything in it in one transaction, so a failure leaves nothing half made.
//!
//! Jobs are kept in memory: a restart forgets them (and a job still running then leaves nothing behind but temporary
//! files, which the daily cleanup removes).

use std::{
    collections::HashMap,
    io::{Read, Write},
    path::PathBuf,
    pin::Pin,
    task::{Context, Poll},
};

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWriteExt, ReadBuf};

use crate::{
    auth::User,
    error::{AppError, AppResult},
    files::Source,
    logs,
    state::AppState,
    tree::{self, Need, Node},
    util::{format_bytes_u64, guess_mime, new_id, now, split_name, validate_name},
    zip::{ReadEntry, ZipWriter},
};

/// Most an archive may hold once extracted, whatever the space's quota
pub const MAX_EXTRACT_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// Most files and folders an archive may hold
pub const MAX_EXTRACT_ENTRIES: u64 = 20_000;
/// Most an entry (or the whole archive) may grow when extracted. Ordinary files compress to a tenth or so, a file of
/// zeros to a thousandth: that is how a small "ZIP bomb" fills a disk. Small entries are exempt
pub const MAX_RATIO: u64 = 250;
const RATIO_EXEMPT_BYTES: u64 = 1024 * 1024;
/// Largest list of entries read into memory
const MAX_DIRECTORY_BYTES: u64 = 64 * 1024 * 1024;
/// Jobs one person may run at the same time
const MAX_RUNNING_PER_USER: usize = 4;
/// How long a finished job can still be looked up
const KEEP_FINISHED_SECS: i64 = 3600;

#[derive(Debug, Clone, Serialize)]
pub struct Job {
    pub id: String,
    /// "compress" or "extract"
    pub kind: &'static str,
    /// "running", "done" or "failed"
    pub state: &'static str,
    /// Bytes handled so far, of `total` (compressing: the items' contents read; extracting: the contents written)
    pub done: u64,
    pub total: u64,
    pub error: Option<String>,
    /// The new ZIP file, or the new folder
    pub node_id: Option<String>,
    pub name: Option<String>,
    #[serde(skip)]
    owner: i64,
    #[serde(skip)]
    finished_at: Option<i64>,
}

/// Registers a new job for `user`; refused while they already have several running
fn start(st: &AppState, user: &User, kind: &'static str) -> AppResult<Job> {
    let mut jobs = st.jobs.lock().unwrap();
    let t = now();
    jobs.retain(|_, j| j.finished_at.is_none_or(|f| f > t - KEEP_FINISHED_SECS));
    if jobs.values().filter(|j| j.owner == user.id && j.finished_at.is_none()).count() >= MAX_RUNNING_PER_USER {
        return Err(AppError::new(StatusCode::TOO_MANY_REQUESTS, "Several ZIP files are being made or extracted already. Wait for one to finish."));
    }
    let job = Job { id: new_id(), kind, state: "running", done: 0, total: 0, error: None, node_id: None, name: None, owner: user.id, finished_at: None };
    jobs.insert(job.id.clone(), job.clone());
    Ok(job)
}

fn update(st: &AppState, id: &str, f: impl FnOnce(&mut Job)) {
    if let Some(j) = st.jobs.lock().unwrap().get_mut(id) {
        f(j);
    }
}

fn finish(st: &AppState, id: &str, result: AppResult<(String, String)>) {
    update(st, id, |j| {
        j.finished_at = Some(now());
        match result {
            Ok((node, name)) => {
                j.state = "done";
                j.done = j.total;
                j.node_id = Some(node);
                j.name = Some(name);
            }
            Err(e) => {
                j.state = "failed";
                j.error = Some(e.message);
            }
        }
    });
}

/// Runs a job in the background and records how it ended
fn spawn<F>(st: &AppState, id: &str, work: F)
where
    F: Future<Output = AppResult<(String, String)>> + Send + 'static,
{
    let (st, id) = (st.clone(), id.to_string());
    tokio::spawn(async move {
        let result = work.await;
        if let Err(e) = &result {
            tracing::info!("Compress or extract task {id} failed: {}", e.message);
        }
        finish(&st, &id, result);
    });
}

/// A job's progress; only its owner can see it
pub async fn get(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<Json<Job>> {
    match st.jobs.lock().unwrap().get(&id) {
        Some(j) if j.owner == user.id => Ok(Json(j.clone())),
        _ => Err(AppError::not_found("This task has finished or doesn't exist")),
    }
}

/// Counts what is read through it into the job's progress
struct Counting<R> {
    inner: R,
    st: AppState,
    job: String,
}

impl<R: AsyncRead + Unpin> AsyncRead for Counting<R> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let res = Pin::new(&mut self.inner).poll_read(cx, buf);
        let n = (buf.filled().len() - before) as u64;
        if n > 0 {
            update(&self.st, &self.job, |j| j.done += n);
        }
        res
    }
}

// ───────────── Compress to ZIP ─────────────

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
    let job = start(&st, &user, "compress")?;
    let offset = -req.tz.unwrap_or(0).clamp(-14 * 60, 14 * 60) * 60;
    spawn(&st, &job.id, run_compress(st.clone(), user, job.id.clone(), roots, dest.id, offset));
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

async fn run_compress(st: AppState, user: User, job: String, roots: Vec<Node>, dest_id: String, offset: i64) -> AppResult<(String, String)> {
    let plan = crate::downloads::zip_plan(&st, roots.clone(), offset).await?;
    let name = zip_name(&plan, &roots);
    let total: u64 = plan.items.iter().map(|it| it.size).sum();
    update(&st, &job, |j| j.total = total);

    let tmp = st.tmp_dir().join(format!("zip-{}", new_id()));
    let written = async {
        let file = tokio::fs::File::create(&tmp).await?;
        let mut zip = ZipWriter::deflating(tokio::io::BufWriter::with_capacity(256 * 1024, file));
        for item in plan.items {
            match item.blob {
                None => zip.add_dir(&item.path, item.mtime).await?,
                Some(source) => {
                    let reader = source.open(&st, 0, item.size).await?;
                    zip.add_file(&item.path, Counting { inner: reader, st: st.clone(), job: job.clone() }, item.size, item.mtime).await?;
                }
            }
        }
        let mut out = zip.finish().await?;
        out.flush().await?;
        out.into_inner().sync_all().await?;
        Ok::<_, AppError>(())
    }
    .await;
    if let Err(e) = written {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    let result = store_new_file(&st, &user, &dest_id, &name, &tmp, "compress").await;
    let _ = tokio::fs::remove_file(&tmp).await;
    result
}

/// Stores a finished temporary file as a new file in `folder_id` (numbered if the name is taken), like an upload:
/// staged in the space's storage location first, then recorded under the write lock. Returns its id and name
async fn store_new_file(st: &AppState, user: &User, folder_id: &str, name: &str, tmp: &std::path::Path, action: &str) -> AppResult<(String, String)> {
    let folder = tree::folder_for(&mut *st.db.acquire().await?, user, folder_id, Need::Write).await?;
    let size = tokio::fs::metadata(tmp).await?.len();
    if folder.in_folder_space() {
        // A folder space: the file goes into the folder on the disk under a name scans ignore, then renamed into place
        let staged = crate::fsops::stage_upload(&folder, tmp, size).await?;
        let _space = crate::fsops::lock_space(folder.drive()).await;
        let _w = st.write_lock.lock().await;
        let result = async {
            let mut tx = st.db.begin().await?;
            let folder = tree::folder_for(&mut tx, user, folder_id, Need::Write).await?;
            let name = crate::fsops::free_name(&mut tx, &folder, name, false).await?;
            let id = crate::fsops::place_file(&mut tx, &staged, user.id, &folder, &name).await?;
            tree::touch(&mut tx, &folder.id).await?;
            if let Some(n) = tree::get_node(&mut tx, &id).await? {
                tree::adjust_usage(&mut tx, n.drive(), n.size).await?;
                logs::record_activity(&mut tx, user, Some(&n), action, "").await?;
            }
            tx.commit().await?;
            Ok((id, name))
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&staged).await;
        }
        return result;
    }
    let (hash, size) = crate::files::hash_file(tmp.to_path_buf()).await?;
    // Takes the temporary file over (and removes it once stored or given up)
    let staged = tree::stage_blob(st, folder.drive(), hash.clone(), size as i64, tmp.to_path_buf()).await?;
    let _w = st.write_lock.lock().await;
    let result = async {
        let mut tx = st.db.begin().await?;
        let folder = tree::folder_for(&mut tx, user, folder_id, Need::Write).await?;
        if folder.in_folder_space() {
            return Err(AppError::conflict("Something changed at the same time. Try again."));
        }
        tree::check_quota(&mut tx, folder.drive(), size as i64).await?;
        let name = tree::unique_name(&mut tx, &folder.id, name, false).await?;
        let extra = tree::commit_blob(st, &mut tx, &staged).await?;
        let id = insert_file(&mut tx, user, &folder.id, &name, &hash, size as i64).await?;
        tree::touch(&mut tx, &folder.id).await?;
        tree::adjust_usage(&mut tx, folder.drive(), size as i64).await?;
        let node = tree::get_node(&mut tx, &id).await?;
        logs::record_activity(&mut tx, user, node.as_ref(), action, "").await?;
        tx.commit().await?;
        Ok((id, name, extra))
    }
    .await;
    match result {
        Ok((id, name, extra)) => {
            tree::finish_staged(st, staged, extra).await;
            Ok((id, name))
        }
        Err(e) => {
            tree::abandon_staged(st, staged).await;
            Err(e)
        }
    }
}

async fn insert_file(conn: &mut sqlx::SqliteConnection, user: &User, parent: &str, name: &str, hash: &str, size: i64) -> AppResult<String> {
    let id = new_id();
    sqlx::query(
        "INSERT INTO nodes (id, owner_id, parent_id, kind, name, blob_hash, size, mime, drive_id, created_at, updated_at)
         SELECT ?1, ?2, ?3, 'file', ?4, ?5, ?6, ?7, drive_id, ?8, ?8 FROM nodes WHERE id = ?3",
    )
    .bind(&id)
    .bind(user.id)
    .bind(parent)
    .bind(name)
    .bind(hash)
    .bind(size)
    .bind(guess_mime(name))
    .bind(now())
    .execute(conn)
    .await?;
    Ok(id)
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
    if parent.in_folder_space() {
        return Err(AppError::bad_request(
            "ZIP files can't be extracted in a space that shows a folder on the server yet. Download the file and extract it on your computer.",
        ));
    }
    let job = start(&st, &user, "extract")?;
    spawn(&st, &job.id, run_extract(st.clone(), user, job.id.clone(), zip, parent.id));
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
        }
        let Some(last) = parts.pop() else { continue };
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
        return Err(AppError::bad_request(format!("This ZIP file holds more than {} once extracted, more than can be extracted here", format_bytes_u64(MAX_EXTRACT_BYTES))));
    }
    if total > RATIO_EXEMPT_BYTES && total / MAX_RATIO > archive_size {
        return Err(too_compressed());
    }
    Ok((out, total))
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
        out.flush()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(Extracted { tmp, hash: hex::encode(sha.finalize()), size })
}

async fn run_extract(st: AppState, user: User, job: String, zip: Node, parent_id: String) -> AppResult<(String, String)> {
    // The archive is read in any order, so it is copied to a temporary file first (a folder space's file is read in place)
    let source = Source::of(&zip)?;
    let (archive, copied) = match &source {
        Source::File(path) => (path.clone(), false),
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
    let result = extract_into(&st, &user, &job, &zip, &parent_id, &archive).await;
    if copied {
        let _ = tokio::fs::remove_file(&archive).await;
    }
    result
}

async fn extract_into(st: &AppState, user: &User, job: &str, zip: &Node, parent_id: &str, archive: &std::path::Path) -> AppResult<(String, String)> {
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
    update(st, job, |j| j.total = total);
    let drive = tree::get_node(&mut *st.db.acquire().await?, parent_id).await?.map(|n| n.drive().to_string()).unwrap_or_default();
    // What the archive states is checked against the quota before anything is extracted
    tree::check_quota(&mut *st.db.acquire().await?, &drive, total as i64).await?;

    let mut staged: Vec<(usize, tree::StagedBlob)> = Vec::new();
    let result = async {
        for (i, p) in plan.iter().enumerate() {
            if p.file.is_none() {
                continue;
            }
            let (archive, entry, tmp) = (archive.to_path_buf(), p.entry.clone(), st.tmp_dir().join(format!("unzip-{}", new_id())));
            let (st2, job2) = (st.clone(), job.to_string());
            let x = tokio::task::spawn_blocking(move || extract_entry(&archive, &entry, tmp, &|n| update(&st2, &job2, |j| j.done += n))).await??;
            staged.push((i, tree::stage_blob(st, &drive, x.hash, x.size as i64, x.tmp).await?));
        }
        let _w = st.write_lock.lock().await;
        let mut tx = st.db.begin().await?;
        let parent = tree::folder_for(&mut tx, user, parent_id, Need::Write).await?;
        let actual: i64 = staged.iter().map(|(_, s)| s.size).sum();
        tree::check_quota(&mut tx, parent.drive(), actual).await?;
        // The new folder is named after the archive
        let stem = validate_name(split_name(&zip.name, false).0).unwrap_or_else(|_| "Extracted".into());
        let name = tree::unique_name(&mut tx, &parent.id, &stem, true).await?;
        let root = tree::create_folder(&mut tx, user.id, &parent.id, &name).await?;
        let mut folders: HashMap<Vec<String>, String> = HashMap::new();
        folders.insert(Vec::new(), root.clone());
        let mut extras = Vec::new();
        let mut blobs = staged.iter().map(|(i, s)| (*i, s)).collect::<HashMap<_, _>>();
        for (i, p) in plan.iter().enumerate() {
            // Folders on the way, created once each (a name taken by a file gets a number)
            let mut key = Vec::new();
            let mut folder = root.clone();
            for d in &p.dirs {
                key.push(d.clone());
                folder = match folders.get(&key) {
                    Some(id) => id.clone(),
                    None => {
                        let id = tree::ensure_folders(&mut tx, user.id, &folder, d, "").await?;
                        folders.insert(key.clone(), id.clone());
                        id
                    }
                };
            }
            let (Some(file), Some(blob)) = (&p.file, blobs.remove(&i)) else { continue };
            let name = tree::unique_name(&mut tx, &folder, file, false).await?;
            extras.extend(tree::commit_blob(st, &mut tx, blob).await?);
            insert_file(&mut tx, user, &folder, &name, &blob.hash, blob.size).await?;
        }
        tree::adjust_usage(&mut tx, parent.drive(), actual).await?;
        let node = tree::get_node(&mut tx, &root).await?;
        logs::record_activity(&mut tx, user, node.as_ref(), "extract", &zip.name).await?;
        tx.commit().await?;
        Ok((root, name, extras))
    }
    .await;
    match result {
        Ok((root, name, extras)) => {
            for (_, s) in staged {
                tree::finish_staged(st, s, None).await;
            }
            // Copies stored twice (the same content in another location) are removed in the background
            tree::schedule_blob_removal(st, extras);
            Ok((root, name))
        }
        Err(e) => {
            for (_, s) in staged {
                tree::abandon_staged(st, s).await;
            }
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    /// Waits for a job to finish
    async fn wait(env: &testutil::TestEnv, id: &str) -> Job {
        for _ in 0..1000 {
            let job = env.st.jobs.lock().unwrap().get(id).cloned().expect("the job is known");
            if job.state != "running" {
                return job;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the job didn't finish");
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
        let mut zip = if deflate { ZipWriter::deflating(Vec::new()) } else { ZipWriter::new(Vec::new()) };
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
    async fn items_are_compressed_next_to_them_and_extracted_back() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let docs = env.folder(&amy, &amy.root_id, "Docs").await;
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
        assert_eq!(get(State(env.st.clone()), ben.clone(), Path(job.id.clone())).await.unwrap_err().status, StatusCode::NOT_FOUND);
        assert!(compress_now(&env, &ben, &[&a], &ben.root_id).await.is_err());
        assert!(extract_now(&env, &ben, &zip).await.is_err());

        // Extracted into a new folder named after it: the same files, folders (empty ones too) and contents
        let job = extract_now(&env, &amy, &zip).await.unwrap();
        assert_eq!((job.state, job.name.as_deref(), job.error), ("done", Some("Docs"), None));
        let out = job.node_id.unwrap();
        let hash = |s: &[u8]| Some(crate::util::sha256_hex(s));
        assert_eq!(
            listing(&env, &out).await,
            [("Sub".into(), "folder".into(), None), ("a.txt".into(), "file".into(), hash(text.as_bytes()))]
        );
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
                let id = env.stored_file(amy, &amy.root_id, name, &bytes).await;
                let job = extract_now(env, amy, &id).await.unwrap();
                assert_eq!(job.state, "failed", "{name}");
                job.error.unwrap()
            }
        };
        let before = listing(&env, &amy.root_id).await.len();
        assert!(fails(vec![("../evil.txt", b"x".to_vec())], false, "up.zip").await.contains("points outside"));
        assert!(fails(vec![("ok/../../evil.txt", b"x".to_vec())], false, "up2.zip").await.contains("points outside"));
        assert!(fails(vec![("/etc/evil", b"x".to_vec())], false, "abs.zip").await.contains("points outside"));
        assert!(fails(vec![("C:\\Windows\\evil.dll", b"x".to_vec())], false, "drive.zip").await.contains("points outside"));
        assert!(fails(vec![("docs/what?.txt", b"x".to_vec())], false, "name.zip").await.contains("can't be extracted: Name can't contain ?"));
        // 20 MB of zeros deflates to about 20 KB
        let bomb = fails(vec![("zeros.bin", vec![0u8; 20 * 1024 * 1024])], true, "bomb.zip").await;
        assert!(bomb.contains("grow far more"), "{bomb}");
        // Too many entries
        let many: Vec<(&'static str, Vec<u8>)> =
            (0..=MAX_EXTRACT_ENTRIES).map(|i| (&*Box::leak(format!("d{i}/").into_boxed_str()), Vec::new())).collect();
        assert!(fails(many, false, "many.zip").await.contains("more than 20000 items"));
        // Not a ZIP at all
        let junk = env.stored_file(&amy, &amy.root_id, "junk.zip", b"this is not a zip").await;
        assert!(extract_now(&env, &amy, &junk).await.unwrap().error.unwrap().contains("damaged"));

        // An entry stating less than it holds: never more than stated is written
        let mut bytes = zip_of(&[("x.txt", &[b'x'; 1000])], false).await;
        let cd = (0..bytes.len() - 4).rev().find(|&i| bytes[i..i + 4] == 0x0201_4b50u32.to_le_bytes()).unwrap();
        bytes[cd + 24..cd + 28].copy_from_slice(&10u32.to_le_bytes());
        let liar = env.stored_file(&amy, &amy.root_id, "liar.zip", &bytes).await;
        assert!(extract_now(&env, &amy, &liar).await.unwrap().error.unwrap().contains("damaged"));

        // What the archive holds counts against the quota before anything is extracted
        sqlx::query("UPDATE users SET quota_bytes = 50000 WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
        let big = fails(vec![("big.txt", vec![b'a'; 60_000])], false, "big.zip").await;
        assert!(big.starts_with("Not enough storage space"), "{big}");

        // Nothing was created by any of them, and no temporary files are left
        let names: Vec<String> = listing(&env, &amy.root_id).await.into_iter().map(|(n, _, _)| n).collect();
        assert_eq!(names.len(), before + 10, "{names:?}");
        assert!(names.iter().all(|n| n.ends_with(".zip")));
        let left: Vec<_> = std::fs::read_dir(env.st.tmp_dir()).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
        assert!(left.iter().all(|n| !n.to_string_lossy().starts_with("unzip-")), "{left:?}");
    }

    #[tokio::test]
    async fn folder_spaces_take_new_zip_files_but_not_extraction() {
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

        let zip = job.node_id.unwrap();
        let err = extract(State(env.st.clone()), admin.clone(), Json(ExtractReq { id: zip })).await.unwrap_err();
        assert!(err.message.contains("can't be extracted in a space that shows a folder"), "{}", err.message);
    }
}
