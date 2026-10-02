//! Downloads of several items: packed into a streamed ZIP, and the short-lived download links a large selection goes
//! through

use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::{
    io::DuplexStream,
    sync::oneshot::{Receiver, Sender},
};
use tokio_util::io::ReaderStream;

use crate::{
    auth::User,
    error::{AppError, AppResult},
    files::{Source, node_blob, serve_blob},
    state::AppState,
    storage::BoxReader,
    tree::{self, Node},
    util::{content_disposition, now},
    zip::ZipWriter,
};

pub struct ZipItem {
    pub path: String,
    /// None for folders
    pub blob: Option<Source>,
    pub size: u64,
    pub mtime: i64,
}

/// What a ZIP of some items holds: every file and folder in them, with its path in the ZIP and the size it was measured
/// at. The selected items are gone through a folder at a time (`Lister`) to add up the ZIP's length, which a download
/// announces before it starts; the ZIP then holds exactly what was measured (`ZipWalk`), whatever changes in the
/// folders meanwhile. A ZIP of many items keeps that list in a temporary file rather than in memory, so a ZIP of a
/// million files doesn't hold a million rows (nor a million open folders).
pub struct ZipPlan {
    items: std::sync::Arc<Recorded>,
    /// The name each selected item has in the ZIP
    pub root_names: Vec<String>,
    /// The folder the items are in, when they are all in the same one
    pub parent_name: Option<String>,
    /// The ZIP's length (stored, as downloads are) and what its files hold
    pub len: u64,
    pub bytes: u64,
}

impl ZipPlan {
    /// The ZIP's name: the item's own for one item, else the folder's they are in
    pub fn file_name(&self, fallback: &str) -> String {
        match (self.root_names.as_slice(), &self.parent_name) {
            ([one], _) => format!("{one}.zip"),
            (_, Some(parent)) => format!("{parent}.zip"),
            _ => format!("{fallback}.zip"),
        }
    }

    /// What goes into the ZIP, one item at a time: what the plan measured
    pub fn walk(&self) -> ZipWalk {
        ZipWalk { items: self.items.clone(), next: 0, lines: None }
    }
}

/// An item of a ZIP as it was measured
#[derive(Clone, serde::Serialize, Deserialize)]
struct Planned {
    path: String,
    size: u64,
    mtime: i64,
    what: What,
}

#[derive(Clone, serde::Serialize, Deserialize)]
enum What {
    Folder,
    /// Content of a storage location
    Stored {
        hash: String,
        location: String,
    },
    /// A file of a folder space: the space's folder, and the file's path below it
    File {
        root: String,
        drive: String,
        read_only: bool,
        rel: String,
    },
}

/// Items a plan keeps in memory; a larger plan goes to a temporary file
#[cfg(not(test))]
const PLAN_IN_MEMORY: usize = 10_000;
/// Tests: plans of a few thousand items go to a file
#[cfg(test)]
const PLAN_IN_MEMORY: usize = 1_000;

/// The items of a plan, in order
enum Recorded {
    Memory(Vec<Planned>),
    /// One JSON line per item, in the data folder's tmp/ (removed when the plan and its walks are dropped)
    File(std::path::PathBuf),
}

impl Drop for Recorded {
    fn drop(&mut self) {
        if let Recorded::File(path) = self {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Writes a plan's items as they are measured
struct Recorder {
    tmp: std::path::PathBuf,
    memory: Vec<Planned>,
    file: Option<tokio::io::BufWriter<tokio::fs::File>>,
}

impl Recorder {
    async fn add(&mut self, item: Planned) -> AppResult<()> {
        if self.file.is_none() && self.memory.len() < PLAN_IN_MEMORY {
            self.memory.push(item);
            return Ok(());
        }
        if self.file.is_none() {
            self.file = Some(tokio::io::BufWriter::new(tokio::fs::File::create(&self.tmp).await?));
            for kept in std::mem::take(&mut self.memory) {
                Self::write(self.file.as_mut().unwrap(), &kept).await?;
            }
        }
        Self::write(self.file.as_mut().unwrap(), &item).await
    }

    async fn write(file: &mut tokio::io::BufWriter<tokio::fs::File>, item: &Planned) -> AppResult<()> {
        use tokio::io::AsyncWriteExt;
        let mut line = serde_json::to_vec(item).map_err(AppError::internal)?;
        line.push(b'\n');
        file.write_all(&line).await?;
        Ok(())
    }

    async fn finish(mut self) -> AppResult<Recorded> {
        use tokio::io::AsyncWriteExt;
        match self.file.take() {
            None => Ok(Recorded::Memory(std::mem::take(&mut self.memory))),
            Some(mut file) => {
                let recorded = Recorded::File(self.tmp.clone());
                file.flush().await?;
                Ok(recorded)
            }
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // Not finished (planning failed): its file goes
        if self.file.is_some() {
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

/// Goes through what a plan measured, in order
pub struct ZipWalk {
    items: std::sync::Arc<Recorded>,
    next: usize,
    lines: Option<tokio::io::Lines<tokio::io::BufReader<tokio::fs::File>>>,
}

impl ZipWalk {
    /// The next item, None at the end. A folder space's file must still have the size it was measured at: one that
    /// changed since fails the ZIP, which can't hold anything but what its length was worked out for.
    pub async fn next(&mut self, _st: &AppState) -> AppResult<Option<ZipItem>> {
        use tokio::io::AsyncBufReadExt;
        let planned = match &*self.items {
            Recorded::Memory(items) => {
                self.next += 1;
                items.get(self.next - 1).cloned()
            }
            Recorded::File(path) => {
                if self.lines.is_none() {
                    self.lines = Some(tokio::io::BufReader::new(tokio::fs::File::open(path).await?).lines());
                }
                match self.lines.as_mut().unwrap().next_line().await? {
                    Some(line) => Some(serde_json::from_str::<Planned>(&line).map_err(AppError::internal)?),
                    None => None,
                }
            }
        };
        let Some(p) = planned else { return Ok(None) };
        let blob = match p.what {
            What::Folder => None,
            What::Stored { hash, location } => Some(Source::Stored { hash, location }),
            What::File { root, drive, read_only, rel } => {
                let changed = || AppError::conflict(format!("\"{}\" changed while the ZIP file was being made. Try again.", p.path));
                let file = crate::folders::open_space(std::path::Path::new(&root), &drive, read_only).and_then(|r| r.join(&rel)).map_err(|_| changed())?;
                let source = Source::File(file);
                if source.describe(p.size).await.map(|(size, _)| size).ok() != Some(p.size) {
                    return Err(changed());
                }
                Some(source)
            }
        };
        Ok(Some(ZipItem { path: p.path, blob, size: p.size, mtime: p.mtime }))
    }
}

/// Items of a folder listed at once while going through it
const ZIP_PAGE: i64 = 500;

enum Todo {
    /// An item and its path in the ZIP
    Item(String, Box<Node>),
    /// The items of a folder after `after` (by id) still to list: the folder, its path in the ZIP
    Folder(Box<Node>, String, Option<String>),
}

/// Goes through the selected items for a plan, depth first, listing a folder's items a page at a time
struct Lister {
    offset: i64,
    todo: Vec<Todo>,
}

impl Lister {
    /// The next item, None at the end. A folder space's file is measured as it is now.
    async fn next(&mut self, st: &AppState) -> AppResult<Option<Planned>> {
        while let Some(todo) = self.todo.pop() {
            match todo {
                Todo::Folder(folder, path, after) => {
                    let page = children_page(st, &folder, after.as_deref()).await?;
                    if page.len() as i64 == ZIP_PAGE {
                        self.todo.push(Todo::Folder(folder, path.clone(), page.last().map(|n| n.id.clone())));
                    }
                    for n in page.into_iter().rev() {
                        self.todo.push(Todo::Item(format!("{path}/{}", n.name), Box::new(n)));
                    }
                }
                Todo::Item(path, n) => {
                    let mtime = n.updated_at + self.offset;
                    if n.is_folder() {
                        self.todo.push(Todo::Folder(n, path.clone(), None));
                        return Ok(Some(Planned { path, size: 0, mtime, what: What::Folder }));
                    }
                    let Ok(blob) = Source::resolve(st, &n).await else { continue };
                    // The length of the ZIP is announced up front: a folder space's file is measured as it is now
                    let (size, what) = match blob {
                        Source::File(_) => {
                            let size = blob.describe(n.size as u64).await.map(|(size, _)| size).unwrap_or(0);
                            let what = What::File {
                                root: n.fs_root.clone().unwrap_or_default(),
                                drive: n.drive().to_string(),
                                read_only: n.space_read_only,
                                rel: n.fs_path.clone().unwrap_or_default(),
                            };
                            (size, what)
                        }
                        Source::Stored { hash, location } => (n.size as u64, What::Stored { hash, location }),
                    };
                    return Ok(Some(Planned { path, size, mtime, what }));
                }
            }
        }
        Ok(None)
    }
}

/// A page of the items of `folder` (not in the trash), by id: as `Node`s of the folder's space, with only what a ZIP
/// needs read from the index
async fn children_page(st: &AppState, folder: &Node, after: Option<&str>) -> AppResult<Vec<Node>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: String,
        kind: String,
        name: String,
        size: i64,
        blob_hash: Option<String>,
        blob_location: Option<String>,
        fs_path: Option<String>,
        updated_at: i64,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT n.id, n.kind, n.name, n.size, n.blob_hash, (SELECT location_id FROM blobs WHERE hash = n.blob_hash) AS blob_location,
                n.fs_path, n.updated_at
         FROM nodes n WHERE n.parent_id = ?1 AND n.trashed_at IS NULL AND n.id > COALESCE(?2, '') ORDER BY n.id LIMIT ?3",
    )
    .bind(&folder.id)
    .bind(after)
    .bind(ZIP_PAGE)
    .fetch_all(&st.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Node {
            id: r.id,
            parent_id: Some(folder.id.clone()),
            kind: r.kind,
            name: r.name,
            blob_hash: r.blob_hash,
            blob_location: r.blob_location,
            size: r.size,
            fs_path: r.fs_path,
            updated_at: r.updated_at,
            created_at: r.updated_at,
            ..folder.clone()
        })
        .collect())
}

/// The contents of a ZIP of `roots` (including folder contents), with times shifted by `offset` seconds (ZIP times are
/// local times)
pub async fn zip_plan(st: &AppState, roots: Vec<Node>, offset: i64) -> AppResult<ZipPlan> {
    // Items inside other selected items come with them; selected twice counts once
    let roots = {
        let ids: Vec<String> = roots.iter().map(|n| n.id.clone()).collect();
        let keep: std::collections::HashSet<String> = crate::nodes::outermost(&mut *st.db.acquire().await?, &ids).await?.into_iter().collect();
        let mut seen = std::collections::HashSet::new();
        roots.into_iter().filter(|n| keep.contains(&n.id) && seen.insert(n.id.clone())).collect::<Vec<_>>()
    };
    let mut root_names: Vec<String> = Vec::new();
    let mut named = Vec::with_capacity(roots.len());
    // Multi-select download: the ZIP is named after the containing folder (the space name for a space's root folder)
    let mut parent_name = None;
    {
        let mut c = st.db.acquire().await?;
        if let Some(parent) = roots.first().and_then(|r| r.parent_id.clone())
            && roots.iter().all(|r| r.parent_id.as_deref() == Some(parent.as_str()))
            && let Some(p) = tree::get_node(&mut c, &parent).await?
        {
            parent_name = Some(if p.name.is_empty() { tree::get_drive(&mut c, p.drive()).await?.map(|d| d.name).unwrap_or_default() } else { p.name })
                .filter(|n| !n.is_empty());
        }
        for root in roots {
            if root.trashed_at.is_some() {
                continue;
            }
            // A space's root folder has no name: use the space name, otherwise paths in the ZIP would become "/filename"
            let mut root_name = if root.name.is_empty() {
                tree::get_drive(&mut c, root.drive()).await?.map(|d| d.name).unwrap_or_else(|| "download".into())
            } else {
                root.name.clone()
            };
            // Items with the same name from different folders (search results, favourites) get a number
            let base = root_name.clone();
            let mut n = 1;
            while root_names.iter().any(|r| r.eq_ignore_ascii_case(&root_name)) {
                root_name = crate::util::numbered_name(&base, n, root.is_folder());
                n += 1;
            }
            root_names.push(root_name.clone());
            named.push((root, root_name));
        }
    }
    // What goes in and its length, going through it once
    let mut len = crate::zip::Length::default();
    let mut bytes = 0;
    let mut lister = Lister { offset, todo: named.into_iter().rev().map(|(n, name)| Todo::Item(name, Box::new(n))).collect() };
    let mut recorder = Recorder { tmp: st.tmp_dir().join(format!("zip-plan-{}", crate::util::new_id())), memory: Vec::new(), file: None };
    while let Some(item) = lister.next(st).await? {
        if item.path.len() + 1 > u16::MAX as usize {
            return Err(AppError::bad_request("A folder path is too long to put in a ZIP file"));
        }
        len.add(&item.path, item.size, matches!(item.what, What::Folder));
        bytes += item.size;
        recorder.add(item).await?;
    }
    let items = std::sync::Arc::new(recorder.finish().await?);
    Ok(ZipPlan { items, root_names, parent_name, len: len.total(), bytes })
}

/// Packs multiple nodes (including folder contents) into a streamed ZIP. `tz` is the browser's time zone as JavaScript
/// reports it (minutes behind UTC): ZIP times are local times, and Windows shows them as such.
pub async fn zip_response(st: &AppState, roots: Vec<Node>, tz: i64) -> AppResult<Response> {
    let offset = -tz.clamp(-14 * 60, 14 * 60) * 60;
    let plan = zip_plan(st, roots, offset).await?;
    let filename = plan.file_name("download");
    let total_len = plan.len;
    // Open the first file before starting the response: if the storage service (e.g. S3) can't be reached, report the
    // error directly instead of sending an empty ZIP
    let mut walk = plan.walk();
    let mut before = Vec::new();
    let mut first = None;
    while let Some(item) = walk.next(st).await? {
        if let Some(source) = item.blob.clone() {
            let reader = open_source(st.clone(), source, item.size).await?;
            first = Some((item, reader));
            break;
        }
        before.push(item);
    }
    let (writer, reader) = tokio::io::duplex(512 * 1024);
    // If packing fails midway, notify the response stream so the connection ends with an error (the browser shows a failed download) rather than saving a truncated ZIP
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<bool>();
    tokio::spawn(pack_zip(st.clone(), before, first, walk, writer, done_tx));
    let body = within_length(ReaderStream::new(reader), total_len).chain(failure_tail(done_rx));
    Ok((
        [
            // Compute the total size in advance so the browser can show download progress and time remaining
            (header::CONTENT_LENGTH, total_len.to_string()),
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_DISPOSITION, content_disposition("attachment", &filename)),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        Body::from_stream(body),
    )
        .into_response())
}

/// Opens a file's whole content (an owned future, for the packing task). A folder space's file is read up to a byte
/// further than measured, so one that grew meanwhile fails the ZIP (`ZipWriter::add_file` checks the size) instead of
/// going into it cut short.
async fn open_source(st: AppState, source: Source, size: u64) -> std::io::Result<BoxReader> {
    let len = if matches!(source, Source::File(_)) { size + 1 } else { size };
    source.open(&st, 0, len).await
}

/// The body of a ZIP download, which must not run past its announced length: a longer body would be cut to that
/// length on the way, and arrive looking complete without the ZIP's directory. It fails instead.
fn within_length<S>(body: S, announced: u64) -> impl futures_util::Stream<Item = std::io::Result<Bytes>>
where
    S: futures_util::Stream<Item = std::io::Result<Bytes>>,
{
    let mut sent = 0u64;
    body.map(move |chunk| {
        let chunk = chunk?;
        sent += chunk.len() as u64;
        if sent > announced {
            return Err(std::io::Error::other("The ZIP file came out longer than announced"));
        }
        Ok(chunk)
    })
}

/// Writes the ZIP into `writer`: the folders `before` the first file, the first file (already opened), then the rest
/// of `walk`. Sends on `done` whether the ZIP was finished.
async fn pack_zip(st: AppState, before: Vec<ZipItem>, first: Option<(ZipItem, BoxReader)>, mut walk: ZipWalk, writer: DuplexStream, done: Sender<bool>) {
    let mut zip = ZipWriter::new(writer);
    let res = async {
        for item in before {
            zip.add_dir(&item.path, item.mtime).await?;
        }
        if let Some((item, reader)) = first {
            zip.add_file(&item.path, reader, item.size, item.mtime).await?;
        }
        while let Some(item) = walk.next(&st).await.map_err(|e| std::io::Error::other(e.message))? {
            match item.blob {
                None => zip.add_dir(&item.path, item.mtime).await?,
                Some(source) => {
                    let reader = open_source(st.clone(), source, item.size).await?;
                    zip.add_file(&item.path, reader, item.size, item.mtime).await?;
                }
            }
        }
        std::io::Result::Ok(())
    }
    .await;
    if let Err(e) = res {
        // The user canceled the download, or reading a file failed (e.g. the storage service disconnected)
        tracing::warn!("zip stream aborted: {e}");
        let _ = done.send(false);
        return;
    }
    let ok = match zip.finish().await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("zip stream aborted: {e}");
            false
        }
    };
    let _ = done.send(ok);
}

/// The end of a ZIP download's body: nothing when the ZIP was finished, an error otherwise, so the download fails
fn failure_tail(done: Receiver<bool>) -> impl futures_util::Stream<Item = std::io::Result<Bytes>> {
    futures_util::stream::once(async move {
        match done.await {
            Ok(true) => None,
            _ => Some(Err(std::io::Error::other("The zip download was interrupted"))),
        }
    })
    .filter_map(|x| async move { x })
}

/// Most items one download can hold: a larger selection is refused rather than left out of the ZIP
pub const MAX_DOWNLOAD_ITEMS: usize = 10_000;
/// How long a download link made with `POST /download` works, in seconds: long enough to start the download (and for
/// the page to hand it over to the browser), short enough that the link is of no use to anyone later
pub const DOWNLOAD_LINK_SECS: i64 = 120;
/// Download links kept per user or share link; making another one drops the oldest
const DOWNLOAD_LINKS_PER_OWNER: usize = 16;

/// What downloads keep in memory (a part of `AppState`)
#[derive(Default)]
pub struct Memory {
    /// Selections waiting to be downloaded through a short-lived link: link token → selection (see `store_download_link`)
    pub links: std::sync::Mutex<std::collections::HashMap<String, DownloadLink>>,
}

/// A selection of items to download, kept on the server so the ids don't have to fit in a URL
pub struct DownloadLink {
    /// Who may use the link: `user:<id>` or `share:<token>`
    owner: String,
    ids: Vec<String>,
    tz: i64,
    expires: i64,
    /// Order of creation, to drop the oldest
    seq: u64,
}

/// The ids of a download, without blanks and repeats. Nothing is dropped: a selection above the limit is refused
pub fn download_ids<S: AsRef<str>>(ids: impl IntoIterator<Item = S>) -> AppResult<Vec<String>> {
    let mut seen = std::collections::HashSet::new();
    let ids: Vec<String> = ids.into_iter().map(|s| s.as_ref().trim().to_string()).filter(|s| !s.is_empty() && seen.insert(s.clone())).collect();
    if ids.is_empty() {
        return Err(AppError::bad_request("Select items to download"));
    }
    if ids.len() > MAX_DOWNLOAD_ITEMS {
        return Err(AppError::bad_request(format!(
            "At most {MAX_DOWNLOAD_ITEMS} items can be downloaded at once. Download the folder they are in, or select fewer items."
        )));
    }
    Ok(ids)
}

/// Keeps a selection for a short while and returns the token of its download link
pub fn store_download_link(st: &AppState, owner: String, ids: Vec<String>, tz: i64) -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let token = crate::util::random_token(32);
    let t = now();
    let mut links = st.part::<Memory>().links.lock().unwrap();
    links.retain(|_, l| l.expires > t);
    let mut own: Vec<(u64, String)> = links.iter().filter(|(_, l)| l.owner == owner).map(|(k, l)| (l.seq, k.clone())).collect();
    if own.len() >= DOWNLOAD_LINKS_PER_OWNER {
        own.sort();
        for (_, k) in &own[..=own.len() - DOWNLOAD_LINKS_PER_OWNER] {
            links.remove(k);
        }
    }
    links.insert(token.clone(), DownloadLink { owner, ids, tz, expires: t + DOWNLOAD_LINK_SECS, seq });
    token
}

/// The selection behind a download link (its ids and time zone), when it belongs to `owner` and hasn't expired
pub fn download_link(st: &AppState, owner: &str, token: &str) -> AppResult<(Vec<String>, i64)> {
    let links = st.part::<Memory>().links.lock().unwrap();
    match links.get(token) {
        Some(l) if l.owner == owner && l.expires > now() => Ok((l.ids.clone(), l.tz)),
        _ => Err(AppError::not_found("This download link has expired. Start the download again.")),
    }
}

#[derive(Deserialize)]
pub struct DownloadQuery {
    pub ids: String,
    /// The browser's time zone (JavaScript's getTimezoneOffset), for the times inside a ZIP
    pub tz: Option<i64>,
}

#[derive(Deserialize)]
pub struct DownloadReq {
    pub ids: Vec<String>,
    /// The browser's time zone (JavaScript's getTimezoneOffset), for the times inside a ZIP
    pub tz: Option<i64>,
}

/// The items to download, each of which the user must be able to open
async fn owned_nodes(st: &AppState, user: &User, ids: &[String]) -> AppResult<Vec<Node>> {
    let mut c = st.db.acquire().await?;
    let mut roots = Vec::with_capacity(ids.len());
    for id in ids {
        roots.push(tree::owned_node(&mut c, user, id).await?);
    }
    Ok(roots)
}

/// A single file is downloaded directly, anything else is packed into a ZIP
async fn serve_download(st: &AppState, headers: &HeaderMap, roots: Vec<Node>, tz: i64) -> AppResult<Response> {
    if let [one] = roots.as_slice()
        && !one.is_folder()
    {
        return serve_blob(st, headers, node_blob(st, one).await?, true).await;
    }
    zip_response(st, roots, tz).await
}

/// Multi-select download with the ids in the URL, for a few items (any number goes through `create_download_link`)
pub async fn download(State(st): State<AppState>, user: User, Query(q): Query<DownloadQuery>, headers: HeaderMap) -> AppResult<Response> {
    let ids = download_ids(q.ids.split(','))?;
    let roots = owned_nodes(&st, &user, &ids).await?;
    serve_download(&st, &headers, roots, q.tz.unwrap_or(0)).await
}

/// Multi-select download of any size: the ids come in the body, and the answer is a short-lived link for this user that
/// the browser downloads from. The items are checked now, so a problem is reported here rather than as a failed download
pub async fn create_download_link(State(st): State<AppState>, user: User, Json(req): Json<DownloadReq>) -> AppResult<Json<serde_json::Value>> {
    let ids = download_ids(&req.ids)?;
    owned_nodes(&st, &user, &ids).await?;
    let token = store_download_link(&st, format!("user:{}", user.id), ids, req.tz.unwrap_or(0));
    Ok(Json(serde_json::json!({ "url": format!("/api/download/{token}") })))
}

/// Downloads the selection behind a link from `create_download_link`
pub async fn download_by_link(State(st): State<AppState>, user: User, Path(token): Path<String>, headers: HeaderMap) -> AppResult<Response> {
    let (ids, tz) = download_link(&st, &format!("user:{}", user.id), &token)?;
    let roots = owned_nodes(&st, &user, &ids).await?;
    serve_download(&st, &headers, roots, tz).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{self, blob_file};
    use axum::http::StatusCode;
    use std::io::Read;

    #[tokio::test]
    async fn large_selections_download_through_a_link_for_the_same_user() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let folder = env.folder(&amy, amy.root(), "Docs").await;
        let mut ids = Vec::new();
        for i in 0..300 {
            ids.push(env.folder(&amy, &folder, &format!("f{i}")).await);
        }
        let req = |ids: Vec<String>| Json(DownloadReq { ids, tz: Some(-480) });

        // 300 ids would make a URL of about 10 KB; in the body they are fine, and every item ends up in the ZIP
        let Json(res) = create_download_link(State(env.st.clone()), amy.clone(), req(ids.clone())).await.unwrap();
        let token = res["url"].as_str().unwrap().strip_prefix("/api/download/").unwrap().to_string();
        let get = |user: User, token: String| download_by_link(State(env.st.clone()), user, Path(token), HeaderMap::new());
        let zip = get(amy.clone(), token.clone()).await.unwrap();
        assert_eq!(zip.headers()[header::CONTENT_TYPE], "application/zip");
        let body = axum::body::to_bytes(zip.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!((0..300).all(|i| text.contains(&format!("f{i}/"))));

        // The link is only for the user who made it, and only for a short while
        assert_eq!(get(ben.clone(), token.clone()).await.unwrap_err().status, StatusCode::NOT_FOUND);
        env.st.part::<Memory>().links.lock().unwrap().get_mut(&token).unwrap().expires = now();
        assert_eq!(get(amy.clone(), token).await.unwrap_err().status, StatusCode::NOT_FOUND);

        // Items the user can't open are refused when the link is made
        assert!(create_download_link(State(env.st.clone()), ben.clone(), req(vec![folder.clone()])).await.is_err());
    }

    #[test]
    fn selections_above_the_limit_are_refused_not_cut_short() {
        let ids: Vec<String> = (0..=MAX_DOWNLOAD_ITEMS).map(|i| format!("id{i}")).collect();
        let err = download_ids(&ids).unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(err.message.starts_with("At most 10000 items"), "{}", err.message);
        assert_eq!(download_ids(&ids[..MAX_DOWNLOAD_ITEMS]).unwrap().len(), MAX_DOWNLOAD_ITEMS);
        // Blanks and repeats don't count
        assert_eq!(download_ids(["a", "", "b", "a"]).unwrap(), ["a", "b"]);
        assert!(download_ids([""]).is_err());
    }

    #[tokio::test]
    async fn each_owner_keeps_only_a_few_download_links() {
        let env = testutil::env().await;
        let link = |owner: &str| store_download_link(&env.st, owner.into(), vec!["x".into()], 0);
        let first = link("user:1");
        let tokens: Vec<String> = (0..DOWNLOAD_LINKS_PER_OWNER).map(|_| link("user:1")).collect();
        let other = link("user:2");
        let links = env.st.part::<Memory>().links.lock().unwrap();
        assert_eq!(links.values().filter(|l| l.owner == "user:1").count(), DOWNLOAD_LINKS_PER_OWNER);
        assert!(!links.contains_key(&first));
        assert!(tokens.iter().all(|t| links.contains_key(t)) && links.contains_key(&other));
    }

    /// Every entry of a ZIP, sorted: its name, and the content of a file
    fn unzip(bytes: &[u8]) -> Vec<(String, Option<Vec<u8>>)> {
        let mut cursor = std::io::Cursor::new(bytes.to_vec());
        let entries = crate::zip::read_entries(&mut cursor, 1000, 1 << 20).unwrap();
        let mut out = Vec::new();
        for e in &entries {
            let data = if e.is_dir {
                None
            } else {
                let mut data = Vec::new();
                crate::zip::open_entry(&mut cursor, e).unwrap().read_to_end(&mut data).unwrap();
                Some(data)
            };
            out.push((e.name.clone(), data));
        }
        out.sort();
        out
    }

    #[tokio::test]
    async fn zip_downloads_hold_nested_folders_but_not_the_trash() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let docs = env.folder(&amy, amy.root(), "Docs").await;
        let sub = env.folder(&amy, &docs, "Sub").await;
        env.folder(&amy, &sub, "Deep").await;
        env.stored_file(&amy, &docs, "a.txt", b"alpha").await;
        env.stored_file(&amy, &sub, "b.txt", b"beta").await;
        let top = env.stored_file(&amy, amy.root(), "top.txt", b"top").await;
        let binned = env.stored_file(&amy, &docs, "binned.txt", b"binned").await;
        let old = env.folder(&amy, &sub, "Old").await;
        env.stored_file(&amy, &old, "c.txt", b"gamma").await;
        let body = serde_json::json!({ "ids": [binned, old] });
        let _ = crate::nodes::trash(State(env.st.clone()), amy.clone(), Json(serde_json::from_value(body).unwrap())).await.unwrap();

        let q = Query(DownloadQuery { ids: format!("{docs},{top}"), tz: Some(0) });
        let res = download(State(env.st.clone()), amy.clone(), q, HeaderMap::new()).await.unwrap();
        assert_eq!(res.headers()[header::CONTENT_TYPE], "application/zip");
        let announced: usize = res.headers()[header::CONTENT_LENGTH].to_str().unwrap().parse().unwrap();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        assert_eq!(bytes.len(), announced);
        let file = |s: &str| Some(s.as_bytes().to_vec());
        assert_eq!(
            unzip(&bytes),
            [
                ("Docs/".to_string(), None),
                ("Docs/Sub/".into(), None),
                ("Docs/Sub/Deep/".into(), None),
                ("Docs/Sub/b.txt".into(), file("beta")),
                ("Docs/a.txt".into(), file("alpha")),
                ("top.txt".into(), file("top")),
            ]
        );
    }

    /// The process's own memory (Linux's RssAnon), in MB
    fn anon_mb() -> u64 {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        status.lines().find(|l| l.starts_with("RssAnon:")).and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok()).unwrap_or(0) / 1024
    }

    /// Working out a ZIP of a large folder (its length, and what goes in it), measured on Linux: the most memory the
    /// process held meanwhile. `ITEMS=200000 cargo test --release -- --ignored --nocapture measure_zip`
    #[tokio::test]
    #[ignore]
    async fn measure_zip_of_a_large_folder() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let n: usize = std::env::var("ITEMS").ok().and_then(|v| v.parse().ok()).unwrap_or(200_000);
        let top = env.folder(&amy, amy.root(), "Big").await;
        let mut tx = crate::db::begin_write(&env.st.db).await.unwrap();
        // One content for all of them
        sqlx::query("INSERT INTO blobs (hash, size, refcount, created_at, location_id) VALUES ('h', 1, 1, 0, 'local')").execute(&mut *tx).await.unwrap();
        for f in 0..n / 100 {
            let d = crate::content::create_folder(&mut tx, amy.id, &top, &format!("d{f}")).await.unwrap();
            let rows: Vec<serde_json::Value> = (0..99).map(|i| serde_json::json!([crate::util::new_id(), format!("file-with-a-longer-name-{i}.txt")])).collect();
            sqlx::query(
                "INSERT INTO nodes (id, owner_id, parent_id, kind, name, size, blob_hash, drive_id, created_at, updated_at)
                 SELECT json_extract(value, '$[0]'), ?2, ?3, 'file', json_extract(value, '$[1]'), 1, 'h',
                        (SELECT drive_id FROM nodes WHERE id = ?3), 0, 0
                 FROM json_each(?1)",
            )
            .bind(serde_json::to_string(&rows).unwrap())
            .bind(amy.id)
            .bind(&d)
            .execute(&mut *tx)
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
        let node = tree::get_node(&mut env.st.db.acquire().await.unwrap(), &top).await.unwrap().unwrap();
        let before = anon_mb();
        let peak = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(before));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (p, s) = (peak.clone(), stop.clone());
        let sampler = std::thread::spawn(move || {
            while !s.load(std::sync::atomic::Ordering::SeqCst) {
                p.fetch_max(anon_mb(), std::sync::atomic::Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        });
        let started = std::time::Instant::now();
        let plan = zip_plan(&env.st, vec![node], 0).await.unwrap();
        let took = started.elapsed();
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        sampler.join().unwrap();
        println!(
            "{n} files: ZIP of {} worked out in {:.1} s, memory {before} MB before, at most {} MB meanwhile",
            plan.file_name("x"),
            took.as_secs_f64(),
            peak.load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn a_zip_of_thousands_of_files_in_a_folder_space_is_made_a_folder_at_a_time() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let space = env.folder_space("Scans").await;
        // More files than a process may usually hold open: each one of them used to keep its folder open until the
        // ZIP was done
        for i in 0..3000 {
            testutil::write_old(&space.dir.join(format!("Big/d{:02}/f{i:04}.txt", i / 100)), b"x");
        }
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (big, _) = env.node_at(&space.drive, "Big").await.unwrap();
        let node = tree::get_node(&mut env.st.db.acquire().await.unwrap(), &big).await.unwrap().unwrap();
        let res = zip_response(&env.st, vec![node], 0).await.unwrap();
        let announced: usize = res.headers()[header::CONTENT_LENGTH].to_str().unwrap().parse().unwrap();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        assert_eq!(bytes.len(), announced);
        let mut cursor = std::io::Cursor::new(bytes.to_vec());
        let entries = crate::zip::read_entries(&mut cursor, 10_000, 1 << 22).unwrap();
        assert_eq!(entries.len(), 3000 + 30 + 1);
        // Its plan was kept in a temporary file, removed once the ZIP was made
        let plans = || std::fs::read_dir(env.st.tmp_dir()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("zip-plan-")).count();
        for _ in 0..100 {
            if plans() == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(plans(), 0);
        let _ = admin;
    }

    #[tokio::test]
    async fn a_zip_whose_content_cant_be_read_fails_instead_of_arriving_cut_short() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let docs = env.folder(&amy, amy.root(), "Docs").await;
        env.stored_file(&amy, &docs, "a.txt", b"alpha").await;
        env.stored_file(&amy, &docs, "b.txt", b"beta").await;
        let node = tree::get_node(&mut env.st.db.acquire().await.unwrap(), &docs).await.unwrap().unwrap();
        let plan = zip_plan(&env.st, vec![node.clone()], 0).await.unwrap();
        let content = |path: &str| if path.ends_with("a.txt") { &b"alpha"[..] } else { &b"beta"[..] };
        let mut walk = plan.walk();
        let mut files: Vec<&[u8]> = Vec::new();
        while let Some(it) = walk.next(&env.st).await.unwrap() {
            if it.blob.is_some() {
                files.push(content(&it.path));
            }
        }
        let (first, second) = (files[0], files[1]);

        // A later file is missing: the answer has started, so the download ends with an error
        let away = blob_file(&env, second).with_extension("away");
        std::fs::rename(blob_file(&env, second), &away).unwrap();
        let res = zip_response(&env.st, vec![node.clone()], 0).await.unwrap();
        assert!(axum::body::to_bytes(res.into_body(), usize::MAX).await.is_err());
        std::fs::rename(&away, blob_file(&env, second)).unwrap();

        // The first one is missing: reported before answering
        std::fs::rename(blob_file(&env, first), &away).unwrap();
        assert!(zip_response(&env.st, vec![node.clone()], 0).await.is_err());
        std::fs::rename(&away, blob_file(&env, first)).unwrap();
        let res = zip_response(&env.st, vec![node], 0).await.unwrap();
        assert_eq!(unzip(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap()).len(), 3);
    }

    #[tokio::test]
    async fn a_zip_body_never_runs_past_its_announced_length() {
        let chunks = || futures_util::stream::iter([Ok(Bytes::from_static(b"12345")), Ok(Bytes::from_static(b"678"))]);
        let all: Vec<_> = within_length(chunks(), 8).collect().await;
        assert!(all.iter().all(|c| c.is_ok()));
        let cut: Vec<_> = within_length(chunks(), 7).collect().await;
        assert!(cut[0].is_ok() && cut[1].is_err());
    }

    #[tokio::test]
    async fn a_zip_holds_what_was_planned_when_its_folder_changes_meanwhile() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let docs = env.folder(&amy, amy.root(), "Docs").await;
        let sub = env.folder(&amy, &docs, "Sub").await;
        env.stored_file(&amy, &docs, "a.txt", b"alpha").await;
        env.stored_file(&amy, &sub, "b.txt", b"beta").await;
        let node = tree::get_node(&mut env.st.db.acquire().await.unwrap(), &docs).await.unwrap().unwrap();
        let plan = zip_plan(&env.st, vec![node], 0).await.unwrap();
        // Added after the length was announced
        env.stored_file(&amy, &sub, "late.txt", b"added later").await;
        env.folder(&amy, &docs, "Later").await;

        let mut walk = plan.walk();
        let (mut paths, mut len) = (Vec::new(), crate::zip::Length::default());
        while let Some(item) = walk.next(&env.st).await.unwrap() {
            len.add(&item.path, item.size, item.blob.is_none());
            paths.push(item.path);
        }
        paths.sort();
        assert_eq!(paths, ["Docs", "Docs/Sub", "Docs/Sub/b.txt", "Docs/a.txt"]);
        assert_eq!(len.total(), plan.len);
    }

    #[tokio::test]
    async fn a_zip_whose_files_grow_while_it_is_sent_fails_rather_than_run_past_its_announced_length() {
        let env = testutil::env().await;
        let space = env.folder_space("Scans").await;
        testutil::write_old(&space.dir.join("Docs/a.txt"), b"alpha");
        testutil::write_old(&space.dir.join("Docs/b.txt"), b"beta");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (docs, _) = env.node_at(&space.drive, "Docs").await.unwrap();
        let node = tree::get_node(&mut env.st.db.acquire().await.unwrap(), &docs).await.unwrap().unwrap();
        let res = zip_response(&env.st, vec![node], 0).await.unwrap();
        let announced: usize = res.headers()[header::CONTENT_LENGTH].to_str().unwrap().parse().unwrap();
        // Written to on the server (over SMB, say) once the download started
        std::fs::write(space.dir.join("Docs/a.txt"), b"alpha, and a lot more").unwrap();
        std::fs::write(space.dir.join("Docs/b.txt"), b"beta, and a lot more").unwrap();
        // A body longer than announced would be cut to the announced length on the way, and look complete: it fails
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await;
        assert!(body.is_err(), "a ZIP of {} bytes, {announced} announced", body.map_or(0, |b| b.len()));
    }
}
