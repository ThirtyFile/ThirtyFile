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

/// What a ZIP of some items holds: every file and folder in them, with its path in the ZIP. Only the selected items
/// are kept: what is in them is gone through a folder at a time (`ZipWalk`), once to add up the ZIP's length and again
/// to pack it, so a ZIP of a million files doesn't hold a million rows (nor a million open folders) in memory.
pub struct ZipPlan {
    roots: Vec<(Node, String)>,
    offset: i64,
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

    /// What goes into the ZIP, one item at a time
    pub fn walk(&self) -> ZipWalk {
        let todo = self.roots.iter().rev().map(|(n, name)| Todo::Item(name.clone(), Box::new(n.clone()))).collect();
        ZipWalk { offset: self.offset, todo }
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

/// Goes through what a ZIP holds, depth first, listing a folder's items a page at a time
pub struct ZipWalk {
    offset: i64,
    todo: Vec<Todo>,
}

impl ZipWalk {
    /// The next item, None at the end. A folder space's file is measured as it is now.
    pub async fn next(&mut self, st: &AppState) -> AppResult<Option<ZipItem>> {
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
                        return Ok(Some(ZipItem { path, blob: None, size: 0, mtime }));
                    }
                    let Ok(blob) = Source::of(&n) else { continue };
                    // The length of the ZIP is announced up front: a folder space's file is measured as it is now
                    let size = match &blob {
                        Source::File(_) => blob.describe(n.size as u64).await.map(|(size, _)| size).unwrap_or(0),
                        Source::Stored { .. } => n.size as u64,
                    };
                    return Ok(Some(ZipItem { path, blob: Some(blob), size, mtime }));
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
        let keep: std::collections::HashSet<String> =
            crate::nodes::outermost(&mut *st.db.acquire().await?, &ids).await?.into_iter().collect();
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
            parent_name = Some(if p.name.is_empty() {
                tree::get_drive(&mut c, p.drive()).await?.map(|d| d.name).unwrap_or_default()
            } else {
                p.name
            })
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
    let mut plan = ZipPlan { roots: named, offset, root_names, parent_name, len: 0, bytes: 0 };
    // Its length, going through it once
    let mut len = crate::zip::Length::default();
    let mut walk = plan.walk();
    while let Some(item) = walk.next(st).await? {
        if item.path.len() + 1 > u16::MAX as usize {
            return Err(AppError::bad_request("A folder path is too long to put in a ZIP file"));
        }
        len.add(&item.path, item.size, item.blob.is_none());
        plan.bytes += item.size;
    }
    plan.len = len.total();
    Ok(plan)
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
    let body = ReaderStream::new(reader).chain(failure_tail(done_rx));
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

/// Opens a file's whole content (an owned future, for the packing task)
async fn open_source(st: AppState, source: Source, size: u64) -> std::io::Result<BoxReader> {
    source.open(&st, 0, size).await
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
    let ids: Vec<String> =
        ids.into_iter().map(|s| s.as_ref().trim().to_string()).filter(|s| !s.is_empty() && seen.insert(s.clone())).collect();
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
    let mut links = st.download_links.lock().unwrap();
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
    let links = st.download_links.lock().unwrap();
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
        && !one.is_folder() {
            return serve_blob(st, headers, node_blob(one)?, true).await;
        }
    zip_response(st, roots, tz).await
}

/// Multi-select download with the ids in the URL, for a few items (any number goes through `create_download_link`)
pub async fn download(
    State(st): State<AppState>,
    user: User,
    Query(q): Query<DownloadQuery>,
    headers: HeaderMap,
) -> AppResult<Response> {
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
        env.st.download_links.lock().unwrap().get_mut(&token).unwrap().expires = now();
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
        let links = env.st.download_links.lock().unwrap();
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
            let d = tree::create_folder(&mut tx, amy.id, &top, &format!("d{f}")).await.unwrap();
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
}
