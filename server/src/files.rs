//! File content: downloads (with Range support) and saving from the online editor. Thumbnails are in thumbnails.rs,
//! ZIP downloads of several items in downloads.rs.

use std::path::PathBuf;

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
use tokio_util::io::ReaderStream;

use crate::{
    auth::User,
    content,
    error::{AppError, AppResult},
    logs,
    state::AppState,
    tree::{self, Node},
    util::{content_disposition, new_id, split_name},
};

pub const MAX_EDIT_BYTES: usize = 20 * 1024 * 1024;

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
    File(crate::beneath::Pinned),
}

impl Source {
    pub fn of(n: &Node) -> AppResult<Source> {
        if n.in_folder_space() {
            return n.fs_pinned().map(Source::File).map_err(|_| AppError::not_found("File not found"));
        }
        let (hash, location) = n.blob()?;
        Ok(Source::Stored { hash: hash.to_string(), location: location.to_string() })
    }

    /// Where to read a file's content from: as `of`, except that a folder space's file whose folder can't be opened
    /// is read from a checked replica, when there is one (replicas/folders.rs)
    pub async fn resolve(st: &AppState, n: &Node) -> AppResult<Source> {
        match Source::of(n) {
            Err(e) if n.in_folder_space() => crate::replicas::folders::fallback(st, n).await.ok_or(e),
            found => found,
        }
    }

    /// The size and a version tag of the content as it is now. A stored content never changes; a file in a folder
    /// space may have changed since it was indexed, so it is looked at again
    pub async fn describe(&self, indexed_size: u64) -> AppResult<(u64, String)> {
        match self {
            Source::Stored { hash, .. } => Ok((indexed_size, hash.clone())),
            Source::File(path) => {
                // Not following a symbolic link: what the index has is a file
                let meta = tokio::fs::symlink_metadata(path.as_path()).await.map_err(|_| AppError::not_found("File not found"))?;
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
            Source::Stored { hash, location } => {
                let primary = match st.storage(location) {
                    Ok(s) => s.open(hash, start, len).await,
                    Err(e) => Err(std::io::Error::other(e.message)),
                };
                match primary {
                    Ok(r) => Ok(r),
                    // Its location can't be read: a checked replica of the same content, if there is one (replicas/)
                    Err(e) => match crate::replicas::open_fallback(st, hash, location, start, len).await {
                        Ok(r) => Ok(r),
                        Err(_) => Err(e),
                    },
                }
            }
            Source::File(path) => {
                use tokio::io::AsyncSeekExt;
                let f = path.clone();
                let open = async move {
                    let mut f = tokio::fs::File::from_std(tokio::task::spawn_blocking(move || f.open_file()).await.map_err(std::io::Error::other)??);
                    if start > 0 {
                        f.seek(std::io::SeekFrom::Start(start)).await?;
                    }
                    Ok(Box::pin(f.take(len)) as crate::storage::BoxReader)
                };
                crate::usage::sample::folder_read(st, path.as_path(), open).await
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

/// Types a browser opens as a page of this site. Even sandboxed, an uploaded page could look like one of the app's own
/// pages at the app's address, so they are shown as plain text (downloads keep their type: they are saved, not opened).
/// SVG pictures stay pictures.
fn opens_as_page(mime: &str) -> bool {
    let m = mime.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
    m == "text/html" || m == "text/xml" || m == "application/xml" || (m.ends_with("+xml") && m != "image/svg+xml")
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
    } else if runs_as_code(b.mime) || (!download && opens_as_page(b.mime)) {
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

pub async fn node_blob<'a>(st: &AppState, n: &'a Node) -> AppResult<Blob<'a>> {
    if n.is_folder() {
        return Err(AppError::bad_request("This isn't a file"));
    }
    Ok(Blob { source: Source::resolve(st, n).await?, size: n.size as u64, name: &n.name, mime: &n.mime })
}

#[derive(Deserialize)]
pub struct ContentQuery {
    download: Option<u8>,
}

pub async fn content(State(st): State<AppState>, user: User, Path(id): Path<String>, Query(q): Query<ContentQuery>, headers: HeaderMap) -> AppResult<Response> {
    let node = tree::owned_node(&mut *st.db.acquire().await?, &user, &id).await?;
    let download = q.download == Some(1);
    let mut res = serve_blob(&st, &headers, node_blob(&st, &node).await?, download).await?;
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
    Ok(crate::hashing::file(path).await?)
}

/// Save from the online editor: replaces the file with new content.
/// With `X-Base-Version` (the updated_at when the file was opened), returns 409 without overwriting if someone else changed the file in the meantime
pub async fn save_content(State(st): State<AppState>, user: User, Path(id): Path<String>, headers: HeaderMap, body: Bytes) -> AppResult<Json<Node>> {
    if body.len() > MAX_EDIT_BYTES {
        return Err(AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "The file is too large to edit online"));
    }
    let base: Option<i64> = headers.get("x-base-version").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
    // Check write permission before uploading anything to the storage location (a viewer must not be able to make the server write there)
    let node = tree::node_for(&mut *st.db.acquire().await?, &user, &id, tree::Need::Write).await?;
    if base.is_some_and(|b| b != node.updated_at) {
        return Err(edit_conflict());
    }
    // Content of the content store, up to 20 MB: hashed on a blocking thread, not on the async worker every other
    // request shares (files of folder spaces aren't hashed)
    let hash = if node.in_folder_space() {
        None
    } else {
        let body = body.clone();
        let hash = tokio::task::spawn_blocking(move || hex::encode(Sha256::digest(&body))).await.map_err(AppError::internal)?;
        if node.blob_hash.as_deref() == Some(hash.as_str()) {
            return Ok(Json(node));
        }
        Some(hash)
    };
    // Store and record in a task of its own, so a dropped request can't stop it between the two
    tokio::spawn(store_content(st, user, node, body, hash, base)).await.map_err(AppError::internal)?
}

/// Someone else saved the file since it was opened in the editor
fn edit_conflict() -> AppError {
    AppError::new(StatusCode::CONFLICT, "Someone else changed this file while you were editing it. Reload the latest version and edit again.")
}

/// Gives the file `before` the saved content (content.rs). A file of a folder space changed on the server since it was
/// indexed (or removed there) isn't overwritten: the new content is saved next to it as "name (conflict copy)" and the
/// save reports a conflict.
async fn store_content(st: AppState, user: User, before: Node, body: Bytes, hash: Option<String>, base: Option<i64>) -> AppResult<Json<Node>> {
    let tmp = st.tmp_dir().join(new_id());
    let received = async {
        tokio::fs::write(&tmp, &body).await?;
        content::stage(&st, &before, content::Received { path: tmp.clone(), size: body.len() as u64, hash }).await
    }
    .await;
    let staged = match received {
        Ok(s) => s,
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e);
        }
    };
    let size = body.len() as i64;
    let mut turn = staged.turn(&st).await;
    let _w = st.write_lock.lock().await;
    let result = async {
        turn.ready()?;
        let mut tx = crate::db::begin_write(&st.db).await?;
        // Re-read while holding the write lock so concurrent saves don't overwrite each other
        let node = tree::node_for(&mut tx, &user, &before.id, tree::Need::Write).await?;
        staged.check(&node)?;
        if base.is_some_and(|b| b != node.updated_at) {
            return Err(edit_conflict());
        }
        if !staged.unchanged(&mut tx, &node).await? {
            // All of it counts against the space's size limit, as a copy
            tree::check_quota(&mut tx, node.drive(), size).await?;
            let parent = tree::get_node(&mut tx, node.parent_id.as_deref().unwrap_or_default()).await?.ok_or_else(|| AppError::not_found("Folder not found"))?;
            let (stem, ext) = split_name(&node.name, false);
            let name = content::free_name(&mut tx, &parent, &format!("{stem} (conflict copy){ext}")).await?;
            let (copy_id, written) = content::create(&mut tx, &staged, user.id, &parent, &name, None).await?;
            let copy = tree::get_node(&mut tx, &copy_id).await?;
            logs::record_activity(&mut tx, &user, copy.as_ref(), "upload", "").await?;
            tx.commit().await?;
            return Ok(Err((name, written)));
        }
        // By how much it grows the file
        tree::check_quota(&mut tx, node.drive(), size - node.size).await?;
        let written = content::replace(&mut tx, &st, &staged, &node, user.id).await?;
        logs::record_activity(&mut tx, &user, Some(&node), "edit", "").await?;
        let node = tree::get_node(&mut tx, &node.id).await?.ok_or_else(|| AppError::not_found("Item not found"))?;
        tx.commit().await?;
        Ok(Ok((node, written)))
    }
    .await;
    match result {
        Ok(Ok((node, written))) => {
            staged.finish(&st, written).await;
            Ok(Json(node))
        }
        Ok(Err((name, written))) => {
            staged.finish(&st, written).await;
            Err(AppError::new(StatusCode::CONFLICT, format!("The file was changed on the server while you were editing it. Your version was saved as \"{name}\"."))
                .with_code("conflict_copy"))
        }
        Err(e) => {
            staged.abandon(&st).await;
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

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
    async fn pages_and_xml_open_as_plain_text_and_download_as_they_are() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        for (name, mime) in [
            ("page.html", "text/html"),
            ("page.xhtml", "application/xhtml+xml"),
            ("feed.xml", "text/xml"),
            ("data.xml", "application/xml"),
            ("news.rss", "application/rss+xml"),
        ] {
            let id = env.stored_file(&amy, amy.root(), name, b"<html><body>Sign in</body></html>").await;
            sqlx::query("UPDATE nodes SET mime = ? WHERE id = ?").bind(mime).bind(&id).execute(&env.st.db).await.unwrap();
            let (status, h, _) = fetch(&env, &amy, &id, &[]).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(h[header::CONTENT_TYPE], "text/plain; charset=utf-8", "{name}");
            let q = Query(ContentQuery { download: Some(1) });
            let res = content(State(env.st.clone()), amy.clone(), Path(id), q, HeaderMap::new()).await.unwrap();
            assert_eq!(res.headers()[header::CONTENT_TYPE], mime, "{name}");
            assert!(res.headers()[header::CONTENT_DISPOSITION].to_str().unwrap().starts_with("attachment"), "{name}");
        }
        // Pictures stay pictures
        let svg = env.stored_file(&amy, amy.root(), "logo.svg", b"<svg xmlns='http://www.w3.org/2000/svg'/>").await;
        assert_eq!(fetch(&env, &amy, &svg, &[]).await.1[header::CONTENT_TYPE], "image/svg+xml");
    }

    #[tokio::test]
    async fn downloads_answer_ranges_and_conditions() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let id = env.stored_file(&amy, amy.root(), "abc.txt", b"abcdefghij").await;

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
        let empty = env.stored_file(&amy, amy.root(), "empty.txt", b"").await;
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
        let id = env.stored_file(&amy, amy.root(), "notes.txt", b"first").await;
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
        let drive = env.drive_of(amy.root()).await;
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
}
