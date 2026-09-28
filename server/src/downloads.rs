//! Downloads of several items: packed into a streamed ZIP, and the short-lived download links a large selection goes
//! through

use std::collections::HashMap;

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

/// What a ZIP of some items holds: every file and folder in them, with its path in the ZIP
pub struct ZipPlan {
    pub items: Vec<ZipItem>,
    /// The name each selected item has in the ZIP
    pub root_names: Vec<String>,
    /// The folder the items are in, when they are all in the same one
    pub parent_name: Option<String>,
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
    let mut items = Vec::new();
    let mut root_names: Vec<String> = Vec::new();
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
        for root in &roots {
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
            let mut paths: HashMap<String, String> = HashMap::new();
            for (n, depth) in tree::subtree(&mut c, &root.id).await? {
                if n.trashed_at.is_some() {
                    continue;
                }
                let path = if depth == 0 {
                    root_name.clone()
                } else {
                    match n.parent_id.as_ref().and_then(|p| paths.get(p)) {
                        Some(parent) => format!("{parent}/{}", n.name),
                        None => continue,
                    }
                };
                paths.insert(n.id.clone(), path.clone());
                let blob = if n.is_folder() { None } else { Source::of(&n).ok() };
                // The length of the ZIP is announced up front: a folder space's file is measured as it is now
                let size = match &blob {
                    Some(s @ Source::File(_)) => s.describe(n.size as u64).await.map(|(size, _)| size).unwrap_or(0),
                    _ => n.size as u64,
                };
                if !n.is_folder() && blob.is_none() {
                    continue;
                }
                items.push(ZipItem { path, blob, size, mtime: n.updated_at + offset });
            }
        }
    }
    if items.iter().any(|it| it.path.len() + 1 > u16::MAX as usize) {
        return Err(AppError::bad_request("A folder path is too long to put in a ZIP file"));
    }
    Ok(ZipPlan { items, root_names, parent_name })
}

/// Packs multiple nodes (including folder contents) into a streamed ZIP. `tz` is the browser's time zone as JavaScript
/// reports it (minutes behind UTC): ZIP times are local times, and Windows shows them as such.
pub async fn zip_response(st: &AppState, roots: Vec<Node>, tz: i64) -> AppResult<Response> {
    let offset = -tz.clamp(-14 * 60, 14 * 60) * 60;
    let plan = zip_plan(st, roots, offset).await?;
    let filename = plan.file_name("download");
    let items = plan.items;
    let total_len = crate::zip::predicted_len(items.iter().map(|it| (it.path.as_str(), it.size, it.blob.is_none())));
    // Open the first file before starting the response: if the storage service (e.g. S3) can't be reached, report the error directly instead of sending an empty ZIP
    let mut first = None;
    if let Some((i, item)) = items.iter().enumerate().find(|(_, it)| it.blob.is_some()) {
        let source = item.blob.clone().unwrap();
        first = Some((i, open_source(st.clone(), source, item.size).await?));
    }
    let (writer, reader) = tokio::io::duplex(512 * 1024);
    // If packing fails midway, notify the response stream so the connection ends with an error (the browser shows a failed download) rather than saving a truncated ZIP
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<bool>();
    tokio::spawn(pack_zip(st.clone(), items, first, writer, done_tx));
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

/// Writes the ZIP of `items` into `writer`; `first` is the first file, already opened (its index and reader). Sends on
/// `done` whether the ZIP was finished.
async fn pack_zip(st: AppState, items: Vec<ZipItem>, mut first: Option<(usize, BoxReader)>, writer: DuplexStream, done: Sender<bool>) {
    let mut zip = ZipWriter::new(writer);
    for (i, item) in items.into_iter().enumerate() {
        let res = match item.blob {
            None => zip.add_dir(&item.path, item.mtime).await,
            Some(source) => {
                let opened = match first.take() {
                    Some((j, r)) if j == i => Ok(r),
                    other => {
                        first = other;
                        open_source(st.clone(), source, item.size).await
                    }
                };
                match opened {
                    Ok(r) => zip.add_file(&item.path, r, item.size, item.mtime).await,
                    Err(e) => Err(e),
                }
            }
        };
        if let Err(e) = res {
            // The user canceled the download, or reading a file failed (e.g. the storage service disconnected)
            tracing::warn!("zip stream aborted: {e}");
            let _ = done.send(false);
            return;
        }
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
        let files: Vec<&[u8]> = plan.items.iter().filter(|it| it.blob.is_some()).map(|it| content(&it.path)).collect();
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
