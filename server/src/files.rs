//! File content: downloads (with Range support), saving from the online editor, thumbnails, ZIP downloads.

use std::{collections::HashMap, io::Read, path::PathBuf};

use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use futures_util::StreamExt;
use tokio_util::io::ReaderStream;

use crate::{
    auth::User,
    error::{AppError, AppResult},
    state::AppState,
    tree::{self, Node},
    util::{content_disposition, new_id, now},
    zip::ZipWriter,
};

pub const MAX_EDIT_BYTES: usize = 20 * 1024 * 1024;
/// Size limit for thumbnail source files: the source is read entirely into memory (at most two at a time)
const MAX_THUMB_SOURCE: i64 = 20 * 1024 * 1024;
/// Decoding limit: keeps malicious images that are tiny on disk but huge when decoded (e.g. a 30000×30000 PNG) from exhausting memory
const MAX_THUMB_PIXELS_SIDE: u32 = 12_000;
const MAX_THUMB_DECODE_BYTES: u64 = 256 * 1024 * 1024;
const THUMB_SIZE: u32 = 320;

/// Parses a single Range. Ok(None) = return the whole file; Err = 416.
fn parse_range(value: &str, size: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(spec) = value.trim().strip_prefix("bytes=") else { return Ok(None) };
    if spec.contains(',') {
        return Ok(None); // Multiple ranges aren't supported; per the spec we may return the whole file
    }
    let (a, b) = spec.split_once('-').ok_or(())?;
    let (a, b) = (a.trim(), b.trim());
    let range = if a.is_empty() {
        let n: u64 = b.parse().map_err(|_| ())?;
        if n == 0 {
            return Err(());
        }
        (size.saturating_sub(n), size.checked_sub(1).ok_or(())?)
    } else {
        let start: u64 = a.parse().map_err(|_| ())?;
        let end = if b.is_empty() { size.saturating_sub(1) } else { b.parse::<u64>().map_err(|_| ())?.min(size.saturating_sub(1)) };
        (start, end)
    };
    if range.0 > range.1 || range.0 >= size {
        return Err(());
    }
    Ok(Some(range))
}

/// Start offset the request asks for: 0 for a full download or a Range starting at byte 0, otherwise the Range start
/// (an unsatisfiable or multi-part Range counts as a full request, matching `serve_blob`)
pub fn range_start(headers: &HeaderMap, size: u64) -> u64 {
    match headers.get(header::RANGE).and_then(|v| v.to_str().ok()).map(|r| parse_range(r, size)) {
        Some(Ok(Some((start, _)))) => start,
        _ => 0,
    }
}

pub struct Blob<'a> {
    pub hash: &'a str,
    pub size: u64,
    pub name: &'a str,
    pub mime: &'a str,
    /// The storage location it's in
    pub location: &'a str,
}

/// Types a browser would run or apply when a page loads the file as a script, style sheet or module. Stored files are
/// served with them as plain text, so an uploaded file can't become code on this site whatever the page includes.
fn runs_as_code(mime: &str) -> bool {
    let m = mime.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
    m.contains("javascript") || m.contains("ecmascript") || m == "text/css" || m == "application/wasm" || m == "text/jscript"
}

pub async fn serve_blob(st: &AppState, headers: &HeaderMap, b: Blob<'_>, download: bool) -> AppResult<Response> {
    // A page loading a stored file as a script, style sheet or worker (never how this app uses them)
    if let Some(dest) = headers.get("sec-fetch-dest").and_then(|v| v.to_str().ok())
        && matches!(dest, "script" | "style" | "worker" | "sharedworker" | "serviceworker" | "audioworklet" | "paintworklet")
    {
        return Err(AppError::forbidden("Files can't be loaded as scripts or styles"));
    }
    let etag = format!("\"{}\"", b.hash);
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response());
    }
    let if_range_ok = headers.get(header::IF_RANGE).map(|v| v.to_str().ok() == Some(etag.as_str())).unwrap_or(true);
    let range = match headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        Some(r) if if_range_ok => parse_range(r, b.size),
        _ => Ok(None),
    };
    let (status, start, end) = match range {
        Ok(Some((s, e))) => (StatusCode::PARTIAL_CONTENT, s, e),
        Ok(None) => (StatusCode::OK, 0, b.size.saturating_sub(1)),
        Err(()) => {
            return Ok((StatusCode::RANGE_NOT_SATISFIABLE, [(header::CONTENT_RANGE, format!("bytes */{}", b.size))]).into_response());
        }
    };
    let len = if b.size == 0 { 0 } else { end - start + 1 };
    let reader = st.storage(b.location)?.open(b.hash, start, len).await?;

    let mime = if b.mime.is_empty() {
        "application/octet-stream"
    } else if runs_as_code(b.mime) {
        "text/plain; charset=utf-8"
    } else {
        b.mime
    };
    let mut res = Response::new(Body::from_stream(ReaderStream::with_capacity(reader, 256 * 1024)));
    *res.status_mut() = status;
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_str(mime).unwrap_or(HeaderValue::from_static("application/octet-stream")));
    h.insert(header::CONTENT_LENGTH, len.into());
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(header::ETAG, HeaderValue::from_str(&etag).unwrap());
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-cache"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    let disposition = content_disposition(if download { "attachment" } else { "inline" }, b.name);
    if let Ok(v) = HeaderValue::from_str(&disposition) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    // User-uploaded HTML / SVG must not run scripts on this site's origin; PDFs need the browser's built-in viewer, so they can't be sandboxed
    if mime != "application/pdf" {
        h.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("sandbox; default-src 'none'; img-src 'self' data:; media-src 'self'; style-src 'unsafe-inline'"),
        );
    }
    if status == StatusCode::PARTIAL_CONTENT {
        h.insert(header::CONTENT_RANGE, HeaderValue::from_str(&format!("bytes {start}-{end}/{}", b.size)).unwrap());
    }
    Ok(res)
}

pub fn node_blob(n: &Node) -> AppResult<Blob<'_>> {
    let (hash, location) = n.blob()?;
    Ok(Blob { hash, size: n.size as u64, name: &n.name, mime: &n.mime, location })
}

#[derive(Deserialize)]
pub struct ContentQuery {
    download: Option<u8>,
}

pub async fn content(
    State(st): State<AppState>,
    user: User,
    Path(id): Path<String>,
    Query(q): Query<ContentQuery>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let node = tree::owned_node(&mut *st.db.acquire().await?, &user, &id).await?;
    let mut res = serve_blob(&st, &headers, node_blob(&node)?, q.download == Some(1)).await?;
    // The version this content belongs to: the editor sends it back as X-Base-Version when saving
    res.headers_mut().insert("x-version", HeaderValue::from(node.updated_at));
    Ok(res)
}

/// Computes a file's sha256 and size
pub async fn hash_file(path: PathBuf) -> AppResult<(String, u64)> {
    Ok(tokio::task::spawn_blocking(move || -> std::io::Result<(String, u64)> {
        let mut f = std::fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1024 * 1024];
        let mut total = 0u64;
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            total += n as u64;
        }
        Ok((hex::encode(hasher.finalize()), total))
    })
    .await??)
}

/// Save from the online editor: replaces the file with new content.
/// With `X-Base-Version` (the updated_at when the file was opened), returns 409 without overwriting if someone else changed the file in the meantime
pub async fn save_content(
    State(st): State<AppState>,
    user: User,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Json<Node>> {
    if body.len() > MAX_EDIT_BYTES {
        return Err(AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "The file is too large to edit online"));
    }
    let base: Option<i64> = headers.get("x-base-version").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
    let conflict = || AppError::new(StatusCode::CONFLICT, "Someone else changed this file while you were editing it. Reload the latest version and edit again.");
    // Check write permission before uploading anything to the storage location (a viewer must not be able to make the server write there)
    let node = tree::node_for(&mut *st.db.acquire().await?, &user, &id, tree::Need::Write).await?;
    if base.is_some_and(|b| b != node.updated_at) {
        return Err(conflict());
    }
    let hash = hex::encode(Sha256::digest(&body));
    if node.blob_hash.as_deref() == Some(hash.as_str()) {
        return Ok(Json(node));
    }
    // Store and record in a task of its own, so a dropped request can't stop it between the two
    let drive = node.drive().to_string();
    tokio::spawn(store_content(st, user, id, body, hash, base, drive)).await.map_err(AppError::internal)?
}

async fn store_content(st: AppState, user: User, id: String, body: Bytes, hash: String, base: Option<i64>, drive: String) -> AppResult<Json<Node>> {
    let conflict = || AppError::new(StatusCode::CONFLICT, "Someone else changed this file while you were editing it. Reload the latest version and edit again.");
    let tmp = st.tmp_dir().join(new_id());
    tokio::fs::write(&tmp, &body).await?;
    // First store it in the space's storage location (without holding the write lock)
    let staged = match tree::stage_blob(&st, &drive, hash.clone(), body.len() as i64, tmp.clone()).await {
        Ok(s) => s,
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e);
        }
    };

    let _w = st.write_lock.lock().await;
    let result = async {
        let mut tx = st.db.begin().await?;
        // Re-read while holding the write lock so concurrent saves don't overwrite each other
        let node = tree::node_for(&mut tx, &user, &id, tree::Need::Write).await?;
        if base.is_some_and(|b| b != node.updated_at) {
            return Err(conflict());
        }
        let old_hash = node.hash()?.to_string();
        tree::check_quota(&mut tx, node.drive(), body.len() as i64 - node.size).await?;
        let extra = tree::commit_blob(&st, &mut tx, &staged).await?;
        sqlx::query("UPDATE nodes SET blob_hash = ?, size = ?, updated_at = ? WHERE id = ?")
            .bind(&hash)
            .bind(body.len() as i64)
            .bind(now().max(node.updated_at + 1))
            .bind(&node.id)
            .execute(&mut *tx)
            .await?;
        let orphans = tree::release_blobs(&mut tx, &[old_hash]).await?;
        tree::adjust_usage(&mut tx, node.drive(), body.len() as i64 - node.size).await?;
        tree::log(&mut tx, &user, Some(&node), "edit", "").await?;
        let node = tree::get_node(&mut tx, &node.id).await?.unwrap();
        tx.commit().await?;
        Ok((node, extra, orphans))
    }
    .await;
    match result {
        Ok((node, extra, orphans)) => {
            tree::finish_staged(&st, staged, extra).await;
            tree::schedule_blob_removal(&st, orphans);
            Ok(Json(node))
        }
        Err(e) => {
            tree::abandon_staged(&st, staged).await;
            Err(e)
        }
    }
}

fn thumbnailable(n: &Node) -> bool {
    matches!(n.mime.as_str(), "image/jpeg" | "image/png" | "image/gif" | "image/webp" | "image/bmp")
        && n.size <= MAX_THUMB_SOURCE
        && n.blob_hash.is_some()
}

pub async fn thumbnail_response(st: &AppState, headers: &HeaderMap, n: &Node) -> AppResult<Response> {
    if !thumbnailable(n) {
        return Err(AppError::not_found("No thumbnail"));
    }
    let (hash, location) = n.blob()?;
    // The thumbnail is derived from the content hash: a matching ETag means the browser's copy is current (no disk read, no body)
    let etag = format!("\"t{hash}\"");
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag), (header::CACHE_CONTROL, "private, max-age=604800".to_string())]).into_response());
    }
    let path = st.thumb_path(hash);
    if !tokio::fs::try_exists(&path).await? {
        let _permit = st.thumb_permits.acquire().await.map_err(AppError::internal)?;
        if !tokio::fs::try_exists(&path).await? {
            let mut data = Vec::with_capacity(n.size as usize);
            st.storage(location)?.open(hash, 0, n.size as u64).await?.read_to_end(&mut data).await?;
            let jpeg = tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
                let mut reader = image::ImageReader::new(std::io::Cursor::new(data)).with_guessed_format().ok()?;
                let mut limits = image::Limits::default();
                limits.max_image_width = Some(MAX_THUMB_PIXELS_SIDE);
                limits.max_image_height = Some(MAX_THUMB_PIXELS_SIDE);
                limits.max_alloc = Some(MAX_THUMB_DECODE_BYTES);
                reader.limits(limits);
                let img = reader.decode().ok()?;
                let thumb = image::DynamicImage::ImageRgb8(img.thumbnail(THUMB_SIZE, THUMB_SIZE).to_rgb8());
                let mut out = Vec::new();
                thumb.write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80)).ok()?;
                Some(out)
            })
            .await?;
            tokio::fs::create_dir_all(path.parent().unwrap()).await?;
            // Write an empty file for images that can't be decoded, so we don't retry every time
            let tmp = path.with_extension(format!("{}.tmp", new_id()));
            tokio::fs::write(&tmp, jpeg.unwrap_or_default()).await?;
            tokio::fs::rename(&tmp, &path).await?;
        }
    }
    let data = tokio::fs::read(&path).await?;
    if data.is_empty() {
        return Err(AppError::not_found("No thumbnail"));
    }
    Ok((
        [
            (header::CONTENT_TYPE, "image/jpeg".to_string()),
            (header::CACHE_CONTROL, "private, max-age=604800".to_string()),
            (header::ETAG, etag),
        ],
        data,
    )
        .into_response())
}

pub async fn thumbnail(State(st): State<AppState>, user: User, Path(id): Path<String>, headers: HeaderMap) -> AppResult<Response> {
    let node = tree::owned_node(&mut *st.db.acquire().await?, &user, &id).await?;
    thumbnail_response(&st, &headers, &node).await
}

struct ZipItem {
    path: String,
    /// (hash, storage location); None for folders
    blob: Option<(String, String)>,
    size: u64,
    mtime: i64,
}

/// Packs multiple nodes (including folder contents) into a streamed ZIP
pub async fn zip_response(st: &AppState, roots: Vec<Node>) -> AppResult<Response> {
    let mut items = Vec::new();
    let mut root_names = Vec::new();
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
            let root_name = if root.name.is_empty() {
                tree::get_drive(&mut c, root.drive()).await?.map(|d| d.name).unwrap_or_else(|| "download".into())
            } else {
                root.name.clone()
            };
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
                let blob = n.blob().ok().map(|(h, l)| (h.to_string(), l.to_string()));
                items.push(ZipItem { path, blob, size: n.size as u64, mtime: n.updated_at });
            }
        }
    }
    if items.iter().any(|it| it.path.len() + 1 > u16::MAX as usize) {
        return Err(AppError::bad_request("A folder path is too long to put in a ZIP file"));
    }
    let total_len = crate::zip::predicted_len(items.iter().map(|it| (it.path.as_str(), it.size, it.blob.is_none())));
    let filename = match (root_names.as_slice(), parent_name) {
        ([one], _) => format!("{one}.zip"),
        (_, Some(parent)) => format!("{parent}.zip"),
        _ => "download.zip".to_string(),
    };
    // Open the first file before starting the response: if the storage service (e.g. S3) can't be reached, report the error directly instead of sending an empty ZIP
    let open = |st: AppState, hash: String, location: String, size: u64| async move {
        match st.storage(&location) {
            Ok(s) => s.open(&hash, 0, size).await,
            Err(e) => Err(std::io::Error::other(e.message)),
        }
    };
    let mut first = None;
    if let Some((i, item)) = items.iter().enumerate().find(|(_, it)| it.blob.is_some()) {
        let (hash, location) = item.blob.clone().unwrap();
        first = Some((i, open(st.clone(), hash, location, item.size).await?));
    }
    let (writer, reader) = tokio::io::duplex(512 * 1024);
    // If packing fails midway, notify the response stream so the connection ends with an error (the browser shows a failed download) rather than saving a truncated ZIP
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<bool>();
    let st = st.clone();
    tokio::spawn(async move {
        let mut zip = ZipWriter::new(writer);
        for (i, item) in items.into_iter().enumerate() {
            let res = match item.blob {
                None => zip.add_dir(&item.path, item.mtime).await,
                Some((hash, location)) => {
                    let opened = match first.take() {
                        Some((j, r)) if j == i => Ok(r),
                        other => {
                            first = other;
                            open(st.clone(), hash, location, item.size).await
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
                let _ = done_tx.send(false);
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
        let _ = done_tx.send(ok);
    });
    let tail = futures_util::stream::once(async move {
        match done_rx.await {
            Ok(true) => None,
            _ => Some(Err(std::io::Error::other("The zip download was interrupted"))),
        }
    })
    .filter_map(|x| async move { x });
    let body = ReaderStream::new(reader).chain(tail);
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

#[derive(Deserialize)]
pub struct DownloadQuery {
    ids: String,
}

/// Multi-select download: a single file is downloaded directly, anything else is packed into a ZIP
pub async fn download(
    State(st): State<AppState>,
    user: User,
    Query(q): Query<DownloadQuery>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let ids: Vec<&str> = q.ids.split(',').filter(|s| !s.is_empty()).take(1000).collect();
    if ids.is_empty() {
        return Err(AppError::bad_request("Select items to download"));
    }
    let mut roots = Vec::new();
    {
        let mut c = st.db.acquire().await?;
        for id in ids {
            roots.push(tree::owned_node(&mut c, &user, id).await?);
        }
    }
    if let [one] = roots.as_slice()
        && !one.is_folder() {
            return serve_blob(&st, &headers, node_blob(one)?, true).await;
        }
    zip_response(&st, roots).await
}
