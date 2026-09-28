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
/// Size limit for thumbnail source files: the source is read entirely into memory (`THIRTYFILE_THUMBNAIL_JOBS` at a time)
const MAX_THUMB_SOURCE: i64 = 20 * 1024 * 1024;
/// Decoding limit: keeps malicious images that are tiny on disk but huge when decoded (e.g. a 30000×30000 PNG) from
/// exhausting memory. Lowered on servers with little memory (`thumb_decode_bytes`)
const MAX_THUMB_PIXELS_SIDE: u32 = 12_000;
pub const MAX_THUMB_DECODE_BYTES: u64 = 256 * 1024 * 1024;
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

/// Where a file's content is
#[derive(Clone, Debug)]
pub enum Source {
    /// In a storage location, named by its hash
    Stored { hash: String, location: String },
    /// A file in a folder space
    File(PathBuf),
}

impl Source {
    pub fn of(n: &Node) -> AppResult<Source> {
        if n.in_folder_space() {
            return n.fs_file().map(Source::File).ok_or_else(|| AppError::not_found("File not found"));
        }
        let (hash, location) = n.blob()?;
        Ok(Source::Stored { hash: hash.to_string(), location: location.to_string() })
    }

    /// The size and a version tag of the content as it is now. A stored content never changes; a file in a folder
    /// space may have changed since it was indexed, so it is looked at again
    pub async fn describe(&self, indexed_size: u64) -> AppResult<(u64, String)> {
        match self {
            Source::Stored { hash, .. } => Ok((indexed_size, hash.clone())),
            Source::File(path) => {
                let meta = tokio::fs::metadata(path).await.map_err(|_| AppError::not_found("File not found"))?;
                if !meta.is_file() {
                    return Err(AppError::not_found("File not found"));
                }
                Ok((meta.len(), file_tag(&meta)))
            }
        }
    }

    /// Reads the content in [start, start + len)
    pub async fn open(&self, st: &AppState, start: u64, len: u64) -> std::io::Result<crate::storage::BoxReader> {
        match self {
            Source::Stored { hash, location } => match st.storage(location) {
                Ok(s) => s.open(hash, start, len).await,
                Err(e) => Err(std::io::Error::other(e.message)),
            },
            Source::File(path) => {
                use tokio::io::AsyncSeekExt;
                let mut f = tokio::fs::File::open(path).await?;
                if start > 0 {
                    f.seek(std::io::SeekFrom::Start(start)).await?;
                }
                Ok(Box::pin(f.take(len)))
            }
        }
    }
}

/// A tag that changes whenever the file does: its identity, size and modification time
fn file_tag(meta: &std::fs::Metadata) -> String {
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
    #[cfg(unix)]
    let ino = std::os::unix::fs::MetadataExt::ino(meta);
    #[cfg(not(unix))]
    let ino = 0u64;
    format!("f{ino:x}-{:x}-{mtime:x}", meta.len())
}

pub struct Blob<'a> {
    pub source: Source,
    /// The size known from the index (a folder space's file is looked at again)
    pub size: u64,
    pub name: &'a str,
    pub mime: &'a str,
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
    let (size, tag) = b.source.describe(b.size).await?;
    let etag = format!("\"{tag}\"");
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response());
    }
    let if_range_ok = headers.get(header::IF_RANGE).map(|v| v.to_str().ok() == Some(etag.as_str())).unwrap_or(true);
    let range = match headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        Some(r) if if_range_ok => parse_range(r, size),
        _ => Ok(None),
    };
    let (status, start, end) = match range {
        Ok(Some((s, e))) => (StatusCode::PARTIAL_CONTENT, s, e),
        Ok(None) => (StatusCode::OK, 0, size.saturating_sub(1)),
        Err(()) => {
            return Ok((StatusCode::RANGE_NOT_SATISFIABLE, [(header::CONTENT_RANGE, format!("bytes */{size}"))]).into_response());
        }
    };
    let len = if size == 0 { 0 } else { end - start + 1 };
    let reader = b.source.open(st, start, len).await?;

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
        h.insert(header::CONTENT_RANGE, HeaderValue::from_str(&format!("bytes {start}-{end}/{size}")).unwrap());
    }
    Ok(res)
}

pub fn node_blob(n: &Node) -> AppResult<Blob<'_>> {
    if n.is_folder() {
        return Err(AppError::bad_request("This isn't a file"));
    }
    Ok(Blob { source: Source::of(n)?, size: n.size as u64, name: &n.name, mime: &n.mime })
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
    let download = q.download == Some(1);
    let mut res = serve_blob(&st, &headers, node_blob(&node)?, download).await?;
    // Opened or previewed in the browser: listed in Recent. Downloads and app passwords (sync tools, backups) aren't
    // opening, and recording happens after the answer so it never slows the file down
    if !download && user.session_id.is_some() {
        let (st, node_id) = (st.clone(), node.id.clone());
        tokio::spawn(async move {
            if let Err(e) = crate::nodes::record_open(&st, user.id, &node_id).await {
                tracing::warn!("Couldn't remember an opened file for Recent: {}", e.message);
            }
        });
    }
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
    // Check write permission before uploading anything to the storage location (a viewer must not be able to make the server write there)
    let node = tree::node_for(&mut *st.db.acquire().await?, &user, &id, tree::Need::Write).await?;
    if base.is_some_and(|b| b != node.updated_at) {
        return Err(edit_conflict());
    }
    if node.in_folder_space() {
        // Written in a task of its own too, so a dropped request can't stop it halfway
        return tokio::spawn(async move { crate::fsops::save(&st, &user, &id, &body, base, edit_conflict).await.map(Json) })
            .await
            .map_err(AppError::internal)?;
    }
    // Up to 20 MB: hashed on a blocking thread, not on the async worker every other request shares
    let hash = {
        let body = body.clone();
        tokio::task::spawn_blocking(move || hex::encode(Sha256::digest(&body))).await.map_err(AppError::internal)?
    };
    if node.blob_hash.as_deref() == Some(hash.as_str()) {
        return Ok(Json(node));
    }
    // Store and record in a task of its own, so a dropped request can't stop it between the two
    let drive = node.drive().to_string();
    tokio::spawn(store_content(st, user, id, body, hash, base, drive)).await.map_err(AppError::internal)?
}

/// Someone else saved the file since it was opened in the editor
fn edit_conflict() -> AppError {
    AppError::new(StatusCode::CONFLICT, "Someone else changed this file while you were editing it. Reload the latest version and edit again.")
}

async fn store_content(st: AppState, user: User, id: String, body: Bytes, hash: String, base: Option<i64>, drive: String) -> AppResult<Json<Node>> {
    let conflict = edit_conflict;
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
        node.hash()?;
        tree::check_quota(&mut tx, node.drive(), body.len() as i64 - node.size).await?;
        let extra = tree::commit_blob(&st, &mut tx, &staged).await?;
        let removed = tree::set_content(&mut tx, crate::versions::Policy::of(&st), &node, &hash, body.len() as i64, user.id).await?;
        tree::log(&mut tx, &user, Some(&node), "edit", "").await?;
        let node = tree::get_node(&mut tx, &node.id).await?.unwrap();
        tx.commit().await?;
        Ok((node, extra, removed))
    }
    .await;
    match result {
        Ok((node, extra, removed)) => {
            tree::finish_staged(&st, staged, extra).await;
            removed.finish(&st);
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
        && (n.blob_hash.is_some() || n.in_folder_space())
}

/// PDFs and videos: the server has no decoder for them, so the browser draws the thumbnail (the first page, a frame)
/// and uploads it (`upload_thumbnail`); the server only keeps it in the thumbnail cache
fn browser_thumbnailable(n: &Node) -> bool {
    !n.is_folder() && (n.mime == "application/pdf" || n.mime.starts_with("video/")) && (n.blob_hash.is_some() || n.in_folder_space())
}

/// Where a file's thumbnail is cached, and the content's size as it is now. Stored content is keyed by its hash; a
/// folder space's file by its identity, size and time, so a changed file gets a new thumbnail
async fn thumb_key(n: &Node) -> AppResult<(Source, u64, String)> {
    let source = Source::of(n)?;
    let (size, tag) = source.describe(n.size as u64).await?;
    let hash = match &source {
        Source::Stored { hash, .. } => hash.clone(),
        Source::File(_) => crate::util::sha256_hex(tag.as_bytes()),
    };
    Ok((source, size, hash))
}

/// A cached thumbnail, with the headers that let the browser keep it
fn thumb_reply(data: Vec<u8>, etag: String) -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/jpeg".to_string()),
            (header::CACHE_CONTROL, "private, max-age=604800".to_string()),
            (header::ETAG, etag),
        ],
        data,
    )
        .into_response()
}

pub async fn thumbnail_response(st: &AppState, headers: &HeaderMap, n: &Node) -> AppResult<Response> {
    let made_here = thumbnailable(n);
    if !made_here && !browser_thumbnailable(n) {
        return Err(AppError::not_found("No thumbnail"));
    }
    let (source, size, hash) = thumb_key(n).await?;
    if made_here && size > MAX_THUMB_SOURCE as u64 {
        return Err(AppError::not_found("No thumbnail"));
    }
    let hash = hash.as_str();
    // The thumbnail is derived from the content: a matching ETag means the browser's copy is current (no disk read, no body)
    let etag = format!("\"t{hash}\"");
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag), (header::CACHE_CONTROL, "private, max-age=604800".to_string())]).into_response());
    }
    let path = st.thumb_path(hash);
    if !made_here {
        // Only one a browser uploaded; until there is one, the browser showing the file makes it
        return match tokio::fs::read(&path).await {
            Ok(data) if !data.is_empty() => Ok(thumb_reply(data, etag)),
            _ => Err(AppError::not_found("No thumbnail")),
        };
    }
    if !tokio::fs::try_exists(&path).await? {
        let _permit = st.thumb_permits.acquire().await.map_err(AppError::internal)?;
        if !tokio::fs::try_exists(&path).await? {
            let mut data = Vec::with_capacity(size as usize);
            source.open(st, 0, size).await?.read_to_end(&mut data).await?;
            let max_alloc = st.thumb_decode_bytes;
            let jpeg = tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
                let mut reader = image::ImageReader::new(std::io::Cursor::new(data)).with_guessed_format().ok()?;
                let mut limits = image::Limits::default();
                limits.max_image_width = Some(MAX_THUMB_PIXELS_SIDE);
                limits.max_image_height = Some(MAX_THUMB_PIXELS_SIDE);
                limits.max_alloc = Some(max_alloc);
                reader.limits(limits);
                thumb_jpeg(&reader.decode().ok()?)
            })
            .await?;
            // Write an empty file for images that can't be decoded, so we don't retry every time
            write_thumb(&path, &jpeg.unwrap_or_default()).await?;
        }
    }
    let data = tokio::fs::read(&path).await?;
    if data.is_empty() {
        return Err(AppError::not_found("No thumbnail"));
    }
    Ok(thumb_reply(data, etag))
}

/// Scales a picture down to the thumbnail size and encodes it as JPEG
fn thumb_jpeg(img: &image::DynamicImage) -> Option<Vec<u8>> {
    let thumb = image::DynamicImage::ImageRgb8(img.thumbnail(THUMB_SIZE, THUMB_SIZE).to_rgb8());
    let mut out = Vec::new();
    thumb.write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80)).ok()?;
    Some(out)
}

/// Puts a thumbnail in the cache in one step (written beside it, then renamed), so a reader never sees half of one
async fn write_thumb(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    tokio::fs::create_dir_all(path.parent().unwrap()).await?;
    let tmp = path.with_extension(format!("{}.tmp", new_id()));
    tokio::fs::write(&tmp, data).await?;
    tokio::fs::rename(&tmp, path).await
}

/// Largest thumbnail a browser may upload
pub const MAX_THUMB_UPLOAD: usize = 512 * 1024;
/// Largest side of an uploaded thumbnail (a browser makes them about THUMB_SIZE; it is scaled down to that)
const MAX_THUMB_UPLOAD_SIDE: u32 = 1024;

/// A thumbnail the browser made for a PDF or video the user can open (see `browser_thumbnailable`). Only a small JPEG
/// or PNG is taken, decoded with tight limits and encoded again, so what is kept is always a plain JPEG of the usual
/// size. A thumbnail already in the cache is kept: it belongs to the same content.
pub async fn upload_thumbnail(State(st): State<AppState>, user: User, Path(id): Path<String>, body: Bytes) -> AppResult<StatusCode> {
    let node = tree::owned_node(&mut *st.db.acquire().await?, &user, &id).await?;
    if !browser_thumbnailable(&node) {
        return Err(AppError::bad_request("This file doesn't take a thumbnail"));
    }
    if body.len() > MAX_THUMB_UPLOAD {
        return Err(AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "The thumbnail is too large"));
    }
    let (_, _, hash) = thumb_key(&node).await?;
    let path = st.thumb_path(&hash);
    if tokio::fs::metadata(&path).await.is_ok_and(|m| m.len() > 0) {
        return Ok(StatusCode::NO_CONTENT);
    }
    let _permit = st.thumb_permits.acquire().await.map_err(AppError::internal)?;
    let jpeg = tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
        let format = image::guess_format(&body).ok()?;
        if !matches!(format, image::ImageFormat::Jpeg | image::ImageFormat::Png) {
            return None;
        }
        let mut reader = image::ImageReader::with_format(std::io::Cursor::new(body), format);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_THUMB_UPLOAD_SIDE);
        limits.max_image_height = Some(MAX_THUMB_UPLOAD_SIDE);
        limits.max_alloc = Some(16 * 1024 * 1024);
        reader.limits(limits);
        thumb_jpeg(&reader.decode().ok()?)
    })
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| AppError::bad_request("The thumbnail must be a JPEG or PNG picture of at most 1024 × 1024 pixels"))?;
    write_thumb(&path, &jpeg).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn thumbnail(State(st): State<AppState>, user: User, Path(id): Path<String>, headers: HeaderMap) -> AppResult<Response> {
    let node = tree::owned_node(&mut *st.db.acquire().await?, &user, &id).await?;
    thumbnail_response(&st, &headers, &node).await
}

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
    let open = |st: AppState, source: Source, size: u64| async move { source.open(&st, 0, size).await };
    let mut first = None;
    if let Some((i, item)) = items.iter().enumerate().find(|(_, it)| it.blob.is_some()) {
        let source = item.blob.clone().unwrap();
        first = Some((i, open(st.clone(), source, item.size).await?));
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
                Some(source) => {
                    let opened = match first.take() {
                        Some((j, r)) if j == i => Ok(r),
                        other => {
                            first = other;
                            open(st.clone(), source, item.size).await
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
    use crate::testutil;

    #[tokio::test]
    async fn large_selections_download_through_a_link_for_the_same_user() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let folder = env.folder(&amy, &amy.root_id, "Docs").await;
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

    /// A PNG of the given size, as a browser would upload it
    fn png(w: u32, h: u32) -> Bytes {
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(w, h).write_to(&mut out, image::ImageFormat::Png).unwrap();
        Bytes::from(out.into_inner())
    }

    #[tokio::test]
    async fn browsers_upload_thumbnails_of_pdfs_they_can_open() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let pdf = env.file(&amy, &amy.root_id, "Report.pdf").await;
        let hash = "ab".repeat(32);
        sqlx::query("INSERT INTO blobs (hash, size, refcount, created_at) VALUES (?, 1000, 1, 0)").bind(&hash).execute(&env.st.db).await.unwrap();
        sqlx::query("UPDATE nodes SET mime = 'application/pdf', blob_hash = ?, size = 1000 WHERE id = ?")
            .bind(&hash)
            .bind(&pdf)
            .execute(&env.st.db)
            .await
            .unwrap();
        let text = env.file(&amy, &amy.root_id, "notes.txt").await;
        let get = |user: User, id: String| thumbnail(State(env.st.clone()), user, Path(id), HeaderMap::new());
        let put = |user: User, id: String, body: Bytes| upload_thumbnail(State(env.st.clone()), user, Path(id), body);

        // No thumbnail until a browser makes one
        assert_eq!(get(amy.clone(), pdf.clone()).await.unwrap_err().status, StatusCode::NOT_FOUND);
        // Someone who can't open the file can't give it a thumbnail; nor can a file the server doesn't take them for
        assert!(put(ben.clone(), pdf.clone(), png(320, 200)).await.is_err());
        assert_eq!(put(amy.clone(), text, png(320, 200)).await.unwrap_err().status, StatusCode::BAD_REQUEST);
        // Only small JPEG or PNG pictures
        assert_eq!(put(amy.clone(), pdf.clone(), Bytes::from_static(b"<svg/>")).await.unwrap_err().status, StatusCode::BAD_REQUEST);
        let mut gif = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(10, 10).write_to(&mut gif, image::ImageFormat::Gif).unwrap();
        assert_eq!(put(amy.clone(), pdf.clone(), Bytes::from(gif.into_inner())).await.unwrap_err().status, StatusCode::BAD_REQUEST);
        assert_eq!(put(amy.clone(), pdf.clone(), png(4000, 10)).await.unwrap_err().status, StatusCode::BAD_REQUEST);
        let huge = Bytes::from(vec![0u8; MAX_THUMB_UPLOAD + 1]);
        assert_eq!(put(amy.clone(), pdf.clone(), huge).await.unwrap_err().status, StatusCode::PAYLOAD_TOO_LARGE);
        assert!(!env.st.thumb_path(&hash).exists());

        // It is encoded again as a JPEG no larger than the other thumbnails, and kept by content
        assert_eq!(put(amy.clone(), pdf.clone(), png(640, 400)).await.unwrap(), StatusCode::NO_CONTENT);
        let res = get(amy.clone(), pdf.clone()).await.unwrap();
        assert_eq!(res.headers()[header::CONTENT_TYPE], "image/jpeg");
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let img = image::load_from_memory_with_format(&body, image::ImageFormat::Jpeg).unwrap();
        assert_eq!((img.width(), img.height()), (THUMB_SIZE, 200));

        // A thumbnail already made stays (another reader's upload doesn't replace it)
        env.grant(&pdf, &ben, "viewer").await;
        assert_eq!(put(ben.clone(), pdf.clone(), png(100, 100)).await.unwrap(), StatusCode::NO_CONTENT);
        assert_eq!(std::fs::read(env.st.thumb_path(&hash)).unwrap(), body.to_vec());
    }

    #[test]
    fn ranges_are_read_as_the_http_spec_says() {
        assert_eq!(parse_range("bytes=2-4", 10), Ok(Some((2, 4))));
        assert_eq!(parse_range(" bytes= 2 - 4 ", 10), Ok(Some((2, 4))));
        // Open ended, past the end, and the last n bytes (more than there are is the whole file)
        assert_eq!(parse_range("bytes=8-", 10), Ok(Some((8, 9))));
        assert_eq!(parse_range("bytes=5-100", 10), Ok(Some((5, 9))));
        assert_eq!(parse_range("bytes=-3", 10), Ok(Some((7, 9))));
        assert_eq!(parse_range("bytes=-100", 10), Ok(Some((0, 9))));
        // Other units and several ranges: the whole file
        assert_eq!(parse_range("items=0-1", 10), Ok(None));
        assert_eq!(parse_range("bytes=0-1,4-5", 10), Ok(None));
        // Unsatisfiable or malformed, and any range of an empty file
        for bad in ["bytes=10-", "bytes=5-3", "bytes=-0", "bytes=abc", "bytes=1", "bytes=-x"] {
            assert_eq!(parse_range(bad, 10), Err(()), "{bad}");
        }
        for bad in ["bytes=0-", "bytes=0-0", "bytes=-5"] {
            assert_eq!(parse_range(bad, 0), Err(()), "{bad} of an empty file");
        }
    }

    /// Reads a file through `content` with these request headers: status, headers and body
    async fn fetch(env: &testutil::TestEnv, user: &User, id: &str, headers: &[(header::HeaderName, &str)]) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut h = HeaderMap::new();
        for (k, v) in headers {
            h.insert(k.clone(), v.parse().unwrap());
        }
        let q = Query(ContentQuery { download: None });
        let res = content(State(env.st.clone()), user.clone(), Path(id.to_string()), q, h).await.unwrap();
        let (parts, body) = res.into_parts();
        (parts.status, parts.headers, axum::body::to_bytes(body, usize::MAX).await.unwrap().to_vec())
    }

    #[tokio::test]
    async fn downloads_answer_ranges_and_conditions() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let id = env.stored_file(&amy, &amy.root_id, "abc.txt", b"abcdefghij").await;

        let (status, h, body) = fetch(&env, &amy, &id, &[]).await;
        assert_eq!((status, body.as_slice()), (StatusCode::OK, &b"abcdefghij"[..]));
        assert_eq!((&h[header::CONTENT_LENGTH], &h[header::ACCEPT_RANGES]), (&HeaderValue::from(10), &HeaderValue::from_static("bytes")));
        let etag = h[header::ETAG].to_str().unwrap().to_string();

        for (range, want, content_range) in [("bytes=2-4", &b"cde"[..], "bytes 2-4/10"), ("bytes=-3", b"hij", "bytes 7-9/10"), ("bytes=8-", b"ij", "bytes 8-9/10")] {
            let (status, h, body) = fetch(&env, &amy, &id, &[(header::RANGE, range)]).await;
            assert_eq!((status, body.as_slice()), (StatusCode::PARTIAL_CONTENT, want), "{range}");
            assert_eq!(h[header::CONTENT_RANGE], content_range);
            assert_eq!(h[header::CONTENT_LENGTH], want.len().to_string().as_str());
        }
        let (status, h, _) = fetch(&env, &amy, &id, &[(header::RANGE, "bytes=10-")]).await;
        assert_eq!((status, &h[header::CONTENT_RANGE]), (StatusCode::RANGE_NOT_SATISFIABLE, &HeaderValue::from_static("bytes */10")));

        // If-Range: the part only while the file is still the one the client has, otherwise all of it
        let (status, _, body) = fetch(&env, &amy, &id, &[(header::RANGE, "bytes=2-4"), (header::IF_RANGE, &etag)]).await;
        assert_eq!((status, body.as_slice()), (StatusCode::PARTIAL_CONTENT, &b"cde"[..]));
        for other in ["\"something-else\"", "Wed, 21 Oct 2015 07:28:00 GMT"] {
            let (status, _, body) = fetch(&env, &amy, &id, &[(header::RANGE, "bytes=2-4"), (header::IF_RANGE, other)]).await;
            assert_eq!((status, body.len()), (StatusCode::OK, 10), "{other}");
        }
        // The browser's copy is current
        let (status, _, body) = fetch(&env, &amy, &id, &[(header::IF_NONE_MATCH, &etag)]).await;
        assert_eq!((status, body.len()), (StatusCode::NOT_MODIFIED, 0));

        // An empty file has nothing to give a part of
        let empty = env.stored_file(&amy, &amy.root_id, "empty.txt", b"").await;
        let (status, h, body) = fetch(&env, &amy, &empty, &[]).await;
        assert_eq!((status, body.len(), &h[header::CONTENT_LENGTH]), (StatusCode::OK, 0, &HeaderValue::from(0)));
        for range in ["bytes=0-", "bytes=-5"] {
            let (status, h, _) = fetch(&env, &amy, &empty, &[(header::RANGE, range)]).await;
            assert_eq!((status, &h[header::CONTENT_RANGE]), (StatusCode::RANGE_NOT_SATISFIABLE, &HeaderValue::from_static("bytes */0")));
        }
    }

    async fn save(env: &testutil::TestEnv, user: &User, id: &str, base: Option<&str>, body: &'static [u8]) -> AppResult<Node> {
        let mut h = HeaderMap::new();
        if let Some(b) = base {
            h.insert("x-base-version", b.parse().unwrap());
        }
        save_content(State(env.st.clone()), user.clone(), Path(id.to_string()), h, Bytes::from_static(body)).await.map(|Json(n)| n)
    }

    async fn count(env: &testutil::TestEnv, sql: &'static str, id: &str) -> i64 {
        let (n,): (i64,) = sqlx::query_as(sql).bind(id).fetch_one(&env.st.db).await.unwrap();
        n
    }

    #[tokio::test]
    async fn saving_from_the_editor_checks_the_version_and_the_space_left() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let id = env.stored_file(&amy, &amy.root_id, "notes.txt", b"first").await;
        let versions = "SELECT COUNT(*) FROM node_versions WHERE node_id = ?";
        let (_, h, _) = fetch(&env, &amy, &id, &[]).await;
        let opened = h["x-version"].to_str().unwrap().to_string();

        // The same content again changes nothing: no new version, no new time
        let same = save(&env, &amy, &id, Some(&opened), b"first").await.unwrap();
        assert_eq!(same.updated_at.to_string(), opened);
        assert_eq!(count(&env, versions, &id).await, 0);

        // Saved from the version opened; a second editor still on that version is told someone else changed it
        let saved = save(&env, &amy, &id, Some(&opened), b"second").await.unwrap();
        assert_ne!(saved.updated_at.to_string(), opened);
        let err = save(&env, &amy, &id, Some(&opened), b"third").await.unwrap_err();
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert_eq!(fetch(&env, &amy, &id, &[]).await.2, b"second");
        assert_eq!(count(&env, versions, &id).await, 1);
        // Without a version (an older client) the save goes through
        save(&env, &amy, &id, None, b"third").await.unwrap();

        // Someone who may only read the file can't save it, and nothing is stored for them
        env.grant(&id, &ben, "viewer").await;
        assert!(save(&env, &ben, &id, None, b"ben was here").await.is_err());
        let stored = "SELECT COUNT(*) FROM blobs WHERE hash = ?";
        assert_eq!(count(&env, stored, &crate::util::sha256_hex(b"ben was here")).await, 0);

        // Only the growth counts against the quota: growing past it is refused, shrinking always works
        let drive = env.drive_of(&amy.root_id).await;
        let used = "SELECT used_bytes FROM drives WHERE id = ?";
        sqlx::query("UPDATE users SET quota_bytes = 8 WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
        tree::recompute_usage(&env.st).await.unwrap();
        save(&env, &amy, &id, None, b"12345678").await.unwrap();
        let err = save(&env, &amy, &id, None, b"123456789").await.unwrap_err();
        assert_eq!(err.status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(fetch(&env, &amy, &id, &[]).await.2, b"12345678");
        assert_eq!(count(&env, used, &drive).await, 8);
        assert_eq!(count(&env, stored, &crate::util::sha256_hex(b"123456789")).await, 0);
        save(&env, &amy, &id, None, b"1").await.unwrap();
        assert_eq!(count(&env, used, &drive).await, 1);
    }

    /// Where the local storage keeps a content
    fn blob_file(env: &testutil::TestEnv, content: &[u8]) -> std::path::PathBuf {
        let hash = crate::util::sha256_hex(content);
        env.dir.join("blobs").join(&hash[0..2]).join(&hash[2..4]).join(&hash)
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
        let docs = env.folder(&amy, &amy.root_id, "Docs").await;
        let sub = env.folder(&amy, &docs, "Sub").await;
        env.folder(&amy, &sub, "Deep").await;
        env.stored_file(&amy, &docs, "a.txt", b"alpha").await;
        env.stored_file(&amy, &sub, "b.txt", b"beta").await;
        let top = env.stored_file(&amy, &amy.root_id, "top.txt", b"top").await;
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
        let docs = env.folder(&amy, &amy.root_id, "Docs").await;
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

    #[tokio::test]
    async fn thumbnails_are_made_once_and_pictures_that_cant_be_read_are_remembered() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let get = |id: String, headers: HeaderMap| thumbnail(State(env.st.clone()), amy.clone(), Path(id), headers);

        let pic = env.stored_file(&amy, &amy.root_id, "pic.png", &png(640, 320)).await;
        let res = get(pic.clone(), HeaderMap::new()).await.unwrap();
        let etag = res.headers()[header::ETAG].clone();
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let img = image::load_from_memory_with_format(&body, image::ImageFormat::Jpeg).unwrap();
        assert_eq!((img.width(), img.height()), (THUMB_SIZE, THUMB_SIZE / 2));
        let mut h = HeaderMap::new();
        h.insert(header::IF_NONE_MATCH, etag);
        assert_eq!(get(pic, h).await.unwrap().status(), StatusCode::NOT_MODIFIED);

        // Not a picture after all, or one too wide to decode safely: no thumbnail, and an empty one in the cache
        for (name, data) in [("broken.png", b"not a png".to_vec()), ("wide.png", png(MAX_THUMB_PIXELS_SIDE + 1, 1).to_vec())] {
            let id = env.stored_file(&amy, &amy.root_id, name, &data).await;
            assert_eq!(get(id.clone(), HeaderMap::new()).await.unwrap_err().status, StatusCode::NOT_FOUND, "{name}");
            let cached = env.st.thumb_path(&crate::util::sha256_hex(&data));
            assert_eq!(std::fs::metadata(&cached).unwrap().len(), 0, "{name}");
            // Asked again, the cache answers: the content isn't read (it could no longer be there)
            std::fs::remove_file(blob_file(&env, &data)).unwrap();
            let err = get(id, HeaderMap::new()).await.unwrap_err();
            assert_eq!((err.status, err.message.as_str()), (StatusCode::NOT_FOUND, "No thumbnail"), "{name}");
        }

        // Too large a file isn't read at all
        let big = env.stored_file(&amy, &amy.root_id, "big.png", &png(8, 8)).await;
        sqlx::query("UPDATE nodes SET size = ? WHERE id = ?").bind(MAX_THUMB_SOURCE + 1).bind(&big).execute(&env.st.db).await.unwrap();
        assert_eq!(get(big, HeaderMap::new()).await.unwrap_err().status, StatusCode::NOT_FOUND);
    }
}
