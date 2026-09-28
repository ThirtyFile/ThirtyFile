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
        && (n.blob_hash.is_some() || n.in_folder_space())
}

pub async fn thumbnail_response(st: &AppState, headers: &HeaderMap, n: &Node) -> AppResult<Response> {
    if !thumbnailable(n) {
        return Err(AppError::not_found("No thumbnail"));
    }
    let source = Source::of(n)?;
    let (size, tag) = source.describe(n.size as u64).await?;
    if size > MAX_THUMB_SOURCE as u64 {
        return Err(AppError::not_found("No thumbnail"));
    }
    // Stored content: its hash. A folder space's file: a key from its identity, size and time, so a changed file gets
    // a new thumbnail
    let hash = match &source {
        Source::Stored { hash, .. } => hash.clone(),
        Source::File(_) => crate::util::sha256_hex(tag.as_bytes()),
    };
    let hash = hash.as_str();
    // The thumbnail is derived from the content: a matching ETag means the browser's copy is current (no disk read, no body)
    let etag = format!("\"t{hash}\"");
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag), (header::CACHE_CONTROL, "private, max-age=604800".to_string())]).into_response());
    }
    let path = st.thumb_path(hash);
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
    /// None for folders
    blob: Option<Source>,
    size: u64,
    mtime: i64,
}

/// Packs multiple nodes (including folder contents) into a streamed ZIP. `tz` is the browser's time zone as JavaScript
/// reports it (minutes behind UTC): ZIP times are local times, and Windows shows them as such.
pub async fn zip_response(st: &AppState, roots: Vec<Node>, tz: i64) -> AppResult<Response> {
    let offset = -tz.clamp(-14 * 60, 14 * 60) * 60;
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
    let total_len = crate::zip::predicted_len(items.iter().map(|it| (it.path.as_str(), it.size, it.blob.is_none())));
    let filename = match (root_names.as_slice(), parent_name) {
        ([one], _) => format!("{one}.zip"),
        (_, Some(parent)) => format!("{parent}.zip"),
        _ => "download.zip".to_string(),
    };
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
}
