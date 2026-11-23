//! Page operations on PDFs in a space, as background jobs (jobs.rs): organize (keep, reorder and turn pages) saved as a
//! new file or as a new version of the same file, extract pages into a new file, split into several files, and combine
//! files. The page work itself is pdf/ops.rs; this reads the files, checks what a person may do, and stores results the
//! way an upload does (content.rs), so quotas, versions and folder spaces behave as they do everywhere.
//!
//! The file is read into memory (at most `MAX_PDF_BYTES` each, `MAX_TOTAL_BYTES` together), and the new file is built
//! on a blocking thread.

use std::sync::Arc;

use axum::{Json, body::Bytes, extract::State};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use super::{PdfError, Pick, arrange, combine, open, parse_ranges};
use crate::{
    archive,
    auth::User,
    error::{AppError, AppResult},
    files::Source,
    jobs::{self, Job, Limit, Outcome, Tracker},
    state::AppState,
    tree::{self, Need, Node},
    util::{new_id, split_name, validate_name},
};

/// Largest PDF these operations read
pub const MAX_PDF_BYTES: u64 = 256 * 1024 * 1024;
/// Largest total of the files combined
pub const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
/// Most files combined at once
pub const MAX_COMBINE: usize = 100;
/// Most files one split makes
pub const MAX_SPLIT_FILES: usize = 500;

/// Whether a file is taken for a PDF
pub fn is_pdf(n: &Node) -> bool {
    !n.is_folder() && (n.mime == "application/pdf" || n.name.to_lowercase().ends_with(".pdf"))
}

fn refused(e: PdfError) -> AppError {
    AppError::bad_request(e.message())
}

/// A page of the new file, as the page sends it: which page of the original (counting from 0) and how many degrees
/// clockwise it is turned
#[derive(Deserialize, Clone, Copy)]
pub struct PagePick {
    pub page: usize,
    #[serde(default)]
    pub rotate: i32,
}

#[derive(Deserialize)]
pub struct PagesReq {
    pub id: String,
    /// The pages of the new file, in order. Or `ranges`, written like "1-3, 5"
    pub pages: Option<Vec<PagePick>>,
    pub ranges: Option<String>,
    /// "new": a new file next to the original. "version": the original gets this as a new version.
    pub save: String,
    /// The new file's name (a new file only)
    pub name: Option<String>,
    /// The `updated_at` of the file when its pages were shown: a version isn't saved over someone else's newer save
    pub base_version: Option<i64>,
}

#[derive(Deserialize)]
pub struct SplitReq {
    pub id: String,
    /// One file per range ("1-3, 4-6"); without ranges, one file per page
    pub ranges: Option<String>,
}

#[derive(Deserialize)]
pub struct CombineReq {
    /// The files, in the order of their pages in the result
    pub ids: Vec<String>,
    pub name: Option<String>,
}

/// The file `id`, which must be a PDF the person can read (and change, with `need`)
async fn pdf_node(st: &AppState, user: &User, id: &str, need: Need) -> AppResult<Node> {
    let mut c = st.db.acquire().await?;
    let node = tree::node_for(&mut c, user, id, need).await?;
    if !is_pdf(&node) {
        return Err(AppError::bad_request("This is not a PDF file"));
    }
    tree::require_subtree(&mut c, user, &node, Need::Read).await?;
    if node.size.max(0) as u64 > MAX_PDF_BYTES {
        return Err(AppError::bad_request("This PDF is too large for page changes (the limit is 256 MB)"));
    }
    Ok(node)
}

/// A PDF's bytes
async fn read(st: &AppState, node: &Node) -> AppResult<Vec<u8>> {
    let source = Source::resolve(st, node).await?;
    let mut reader = source.open(st, 0, MAX_PDF_BYTES + 1).await.map_err(|_| AppError::not_found("File not found"))?;
    let mut data = Vec::with_capacity(node.size.max(0) as usize);
    reader.read_to_end(&mut data).await?;
    if data.len() as u64 > MAX_PDF_BYTES {
        return Err(AppError::bad_request("This PDF is too large for page changes (the limit is 256 MB)"));
    }
    Ok(data)
}

/// The name of a new file: what was asked for, else `default`, always ending in ".pdf"
fn file_name(asked: Option<&str>, default: &str) -> AppResult<String> {
    let name = validate_name(asked.map(str::trim).filter(|n| !n.is_empty()).unwrap_or(default))?;
    Ok(if name.to_lowercase().ends_with(".pdf") { name } else { format!("{name}.pdf") })
}

fn stem(node: &Node) -> &str {
    split_name(&node.name, false).0
}

/// Writes a file made here to a temporary file and stores it next to `reads`, as an upload would
async fn store_new(st: &AppState, user: &User, folder_id: &str, name: &str, data: Vec<u8>, reads: &[Node]) -> AppResult<(String, String)> {
    let tmp = st.tmp_dir().join(format!("pdf-{}", new_id()));
    if let Err(e) = tokio::fs::write(&tmp, &data).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e.into());
    }
    drop(data);
    let result = archive::store_new_file(st, user, folder_id, name, &tmp, "upload", reads).await;
    let _ = tokio::fs::remove_file(&tmp).await;
    result
}

/// Runs the page work on a blocking thread
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T, AppError> + Send + 'static) -> AppResult<T> {
    tokio::task::spawn_blocking(work).await.map_err(AppError::internal)?
}

fn picks_of(req: &PagesReq, count: usize) -> AppResult<Vec<Pick>> {
    if let Some(pages) = &req.pages {
        return Ok(pages.iter().map(|p| Pick { page: p.page, rotate: p.rotate.rem_euclid(360) }).collect());
    }
    let ranges = parse_ranges(req.ranges.as_deref().unwrap_or(""), count).map_err(AppError::bad_request)?;
    Ok(ranges.into_iter().flat_map(|(from, to)| (from..=to).map(|page| Pick { page, rotate: 0 })).collect())
}

/// Organize or extract: a file of the chosen pages
pub async fn pages(State(st): State<AppState>, user: User, Json(req): Json<PagesReq>) -> AppResult<Json<Job>> {
    let version = match req.save.as_str() {
        "new" => false,
        "version" => true,
        _ => return Err(AppError::bad_request("Choose where to save the result")),
    };
    // Checked now, so a problem is reported at once rather than as a failed task
    let node = pdf_node(&st, &user, &req.id, if version { Need::Write } else { Need::Read }).await?;
    if version {
        if req.base_version.is_some_and(|b| b != node.updated_at) {
            return Err(AppError::conflict("Someone else changed this file after its pages were shown. Reload the file and organize its pages again."));
        }
    } else {
        let parent = node.parent_id.clone().unwrap_or_default();
        tree::folder_for(&mut *st.db.acquire().await?, &user, &parent, Need::Add).await?;
    }
    let name = if version { None } else { Some(file_name(req.name.as_deref(), &format!("{} (pages).pdf", stem(&node)))?) };
    let job = jobs::start(&st, user.id, "pdf_pages", Limit::Pdf)?;
    let tracker = Tracker::of(&st, &job);
    jobs::spawn(&st, &job, run_pages(st.clone(), user, tracker, node, req, name));
    Ok(Json(job))
}

async fn run_pages(st: AppState, user: User, progress: Tracker, node: Node, req: PagesReq, name: Option<String>) -> AppResult<Outcome> {
    progress.set_total(2);
    let base = req.base_version;
    let data = read(&st, &node).await?;
    progress.add(1);
    let out = blocking(move || {
        let opened = open(&data).map_err(refused)?;
        let picks = picks_of(&req, opened.pages.len())?;
        arrange(&opened, &picks).map_err(AppError::bad_request)
    })
    .await?;
    let result = match name {
        Some(name) => {
            let folder = node.parent_id.clone().unwrap_or_default();
            store_new(&st, &user, &folder, &name, out, std::slice::from_ref(&node)).await.map(|(id, name)| Outcome::node(id, name))
        }
        None => save_version(&st, &user, &node.id, out, base, node.updated_at).await,
    };
    progress.add(1);
    result
}

/// The file gets the new content as a new version (the one it had stays, to restore). `read_at`: its `updated_at` when
/// its pages were read: a save of someone else since then is not overwritten.
async fn save_version(st: &AppState, user: &User, id: &str, data: Vec<u8>, base: Option<i64>, read_at: i64) -> AppResult<Outcome> {
    let node = tree::node_for(&mut *st.db.acquire().await?, user, id, Need::Write).await?;
    let base = Some(base.unwrap_or(read_at));
    let body;
    let hash = if node.in_folder_space() {
        body = Bytes::from(data);
        None
    } else {
        let data = Arc::new(data);
        let hashed = data.clone();
        let hash = tokio::task::spawn_blocking(move || hex::encode(Sha256::digest(hashed.as_slice()))).await.map_err(AppError::internal)?;
        body = Bytes::from(Arc::try_unwrap(data).unwrap_or_else(|d| (*d).clone()));
        Some(hash)
    };
    let Json(saved) = tokio::spawn(crate::files::store_content(st.clone(), user.clone(), node, body, hash, base)).await.map_err(AppError::internal)??;
    Ok(Outcome::node(saved.id, saved.name))
}

/// Split into one file per page or per range
pub async fn split(State(st): State<AppState>, user: User, Json(req): Json<SplitReq>) -> AppResult<Json<Job>> {
    let node = pdf_node(&st, &user, &req.id, Need::Read).await?;
    let parent = node.parent_id.clone().unwrap_or_default();
    tree::folder_for(&mut *st.db.acquire().await?, &user, &parent, Need::Add).await?;
    let job = jobs::start(&st, user.id, "pdf_split", Limit::Pdf)?;
    let tracker = Tracker::of(&st, &job);
    jobs::spawn(&st, &job, run_split(st.clone(), user, tracker, node, req));
    Ok(Json(job))
}

async fn run_split(st: AppState, user: User, progress: Tracker, node: Node, req: SplitReq) -> AppResult<Outcome> {
    let data = read(&st, &node).await?;
    let (stem, folder) = (stem(&node).to_string(), node.parent_id.clone().unwrap_or_default());
    // Each file as it is made is stored at once, so the memory held is one file's, however many there are
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AppResult<(String, Vec<u8>)>>(2);
    let ranges = req.ranges.clone();
    let made = tokio::task::spawn_blocking(move || {
        let result = split_files(&data, ranges.as_deref(), |label, bytes| tx.blocking_send(Ok((label, bytes))).is_ok());
        if let Err(e) = result {
            let _ = tx.blocking_send(Err(e));
        }
    });
    let mut first: Option<(String, String)> = None;
    let mut count = 0u64;
    while let Some(item) = rx.recv().await {
        let (label, bytes) = item?;
        let name = file_name(None, &format!("{stem} - {label}.pdf"))?;
        let stored = store_new(&st, &user, &folder, &name, bytes, std::slice::from_ref(&node)).await?;
        first.get_or_insert(stored);
        count += 1;
        progress.set(count, count.max(1));
    }
    made.await.map_err(AppError::internal)?;
    let (id, name) = first.ok_or_else(|| AppError::bad_request("Choose at least one page"))?;
    Ok(Outcome { node_id: Some(id), name: Some(name), result: Some(serde_json::json!({ "files": count })) })
}

/// The files of a split, each given to `emit` (which says whether to go on) with the pages it holds as its label
fn split_files(data: &[u8], ranges: Option<&str>, mut emit: impl FnMut(String, Vec<u8>) -> bool) -> AppResult<()> {
    let opened = open(data).map_err(refused)?;
    let count = opened.pages.len();
    let parts: Vec<(usize, usize)> = match ranges.map(str::trim).filter(|r| !r.is_empty()) {
        Some(text) => parse_ranges(text, count).map_err(AppError::bad_request)?,
        None => (0..count).map(|p| (p, p)).collect(),
    };
    if parts.len() > MAX_SPLIT_FILES {
        return Err(AppError::bad_request(format!("A PDF can be split into at most {MAX_SPLIT_FILES} files at once")));
    }
    for (from, to) in parts {
        let picks: Vec<Pick> = (from..=to).map(|page| Pick { page, rotate: 0 }).collect();
        let label = if from == to { format!("{}", from + 1) } else { format!("{}-{}", from + 1, to + 1) };
        if !emit(label, arrange(&opened, &picks).map_err(AppError::bad_request)?) {
            break;
        }
    }
    Ok(())
}

/// Combine PDFs, in the order given, into a new file next to the first
pub async fn combine_files(State(st): State<AppState>, user: User, Json(req): Json<CombineReq>) -> AppResult<Json<Job>> {
    if req.ids.len() < 2 {
        return Err(AppError::bad_request("Select at least two PDF files to combine"));
    }
    if req.ids.len() > MAX_COMBINE {
        return Err(AppError::bad_request(format!("At most {MAX_COMBINE} PDF files can be combined at once")));
    }
    let mut nodes = Vec::new();
    for id in &req.ids {
        nodes.push(pdf_node(&st, &user, id, Need::Read).await?);
    }
    if nodes.iter().map(|n| n.size.max(0) as u64).sum::<u64>() > MAX_TOTAL_BYTES {
        return Err(AppError::bad_request("These PDFs are too large to combine (the limit is 512 MB together)"));
    }
    let folder = nodes[0].parent_id.clone().unwrap_or_default();
    tree::folder_for(&mut *st.db.acquire().await?, &user, &folder, Need::Add).await?;
    let name = file_name(req.name.as_deref(), "Combined.pdf")?;
    let job = jobs::start(&st, user.id, "pdf_combine", Limit::Pdf)?;
    let tracker = Tracker::of(&st, &job);
    jobs::spawn(&st, &job, run_combine(st.clone(), user, tracker, nodes, folder, name));
    Ok(Json(job))
}

async fn run_combine(st: AppState, user: User, progress: Tracker, nodes: Vec<Node>, folder: String, name: String) -> AppResult<Outcome> {
    progress.set_total(nodes.len() as u64 + 1);
    let mut files = Vec::new();
    let mut total = 0u64;
    for node in &nodes {
        let data = read(&st, node).await?;
        total += data.len() as u64;
        if total > MAX_TOTAL_BYTES {
            return Err(AppError::bad_request("These PDFs are too large to combine (the limit is 512 MB together)"));
        }
        files.push((node.name.clone(), data));
        progress.add(1);
    }
    let out = blocking(move || {
        let mut opened = Vec::with_capacity(files.len());
        for (name, data) in &files {
            // The file is named in the message, so it is known which one can't be used
            opened.push(open(data).map_err(|e| AppError::bad_request(format!("\"{name}\" can't be combined: {}", e.message())))?);
        }
        if opened.iter().map(|o| o.pages.len()).sum::<usize>() >= super::MAX_PAGES {
            return Err(refused(PdfError::TooManyPages));
        }
        combine(&opened).map_err(AppError::bad_request)
    })
    .await?;
    let (id, name) = store_new(&st, &user, &folder, &name, out, &nodes).await?;
    progress.add(1);
    Ok(Outcome::node(id, name))
}

/// A stored PDF's bytes (tests)
#[cfg(test)]
pub async fn read_for_tests(st: &AppState, node: &Node) -> Vec<u8> {
    read(st, node).await.unwrap()
}
