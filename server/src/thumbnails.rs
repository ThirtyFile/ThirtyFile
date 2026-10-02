//! Thumbnails: made on the server for pictures, uploaded by the browser for PDFs and videos, and kept in a cache by
//! content

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use tokio::io::AsyncReadExt;

use crate::{
    auth::User,
    error::{AppError, AppResult},
    files::Source,
    state::AppState,
    tree::{self, Node},
    util::new_id,
};

/// What thumbnails keep in memory (a part of `AppState`)
pub struct Memory {
    /// Thumbnails made at the same time
    pub permits: tokio::sync::Semaphore,
    /// Most memory one thumbnail may use to decode its image
    pub decode_bytes: u64,
}

impl Memory {
    pub fn new(jobs: usize, decode_bytes: u64) -> Memory {
        Memory { permits: tokio::sync::Semaphore::new(jobs), decode_bytes }
    }
}

/// Size limit for thumbnail source files: the source is read entirely into memory (`THIRTYFILE_THUMBNAIL_JOBS` at a time)
const MAX_THUMB_SOURCE: i64 = 20 * 1024 * 1024;
/// Decoding limit: keeps malicious images that are tiny on disk but huge when decoded (e.g. a 30000×30000 PNG) from
/// exhausting memory. Lowered on servers with little memory (`thumb_decode_bytes`)
const MAX_THUMB_PIXELS_SIDE: u32 = 12_000;
pub const MAX_THUMB_DECODE_BYTES: u64 = 256 * 1024 * 1024;
const THUMB_SIZE: u32 = 320;

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

/// Where a file's thumbnail is cached, and the content's size as it is now. Stored content the server draws itself is
/// keyed by its hash (the same picture wherever it is). A thumbnail a browser drew (`browser_thumbnailable`) is kept
/// for the space only, `<hash>.<space>`: people elsewhere with the same content never see one someone else uploaded.
/// Both go with their content (`forget`). A folder space's file is keyed by its space, identity, size and time,
/// `<sha256>.f`, so a changed file gets a new thumbnail; those go once they haven't been shown for a while (`sweep`).
async fn thumb_key(st: &AppState, n: &Node) -> AppResult<(Source, u64, String)> {
    let source = Source::resolve(st, n).await?;
    let (size, tag) = source.describe(n.size as u64).await?;
    let hash = match &source {
        Source::Stored { hash, .. } if !browser_thumbnailable(n) => hash.clone(),
        Source::Stored { hash, .. } => format!("{hash}.{}", n.drive()),
        Source::File(_) => format!("{}.f", crate::util::sha256_hex(format!("{}:{tag}", n.drive()).as_bytes())),
    };
    Ok((source, size, hash))
}

/// Thumbnails not shown for this long are removed by `sweep` (made again when they are)
const UNUSED_DAYS: u64 = 30;
/// Temporary files of thumbnails being written that are older than this were left by a stop
const TMP_GRACE: std::time::Duration = std::time::Duration::from_secs(3600);

/// Thumbnails shown since the last `sweep`, whose time on disk was moved to now: once a day at most for each
static SHOWN: std::sync::Mutex<Option<std::collections::HashSet<String>>> = std::sync::Mutex::new(None);

/// A thumbnail is shown (or asked for again, whether or not the browser had it): its time on disk becomes now, so that
/// `sweep` keeps it, once a day at most
fn shown(st: &AppState, key: &str) {
    {
        let mut shown = SHOWN.lock().unwrap_or_else(|e| e.into_inner());
        let shown = shown.get_or_insert_default();
        // (a day of a very large server: started over, which only dates some of them again)
        if shown.len() >= 100_000 {
            shown.clear();
        }
        if !shown.insert(key.to_string()) {
            return;
        }
    }
    let path = st.thumb_path(key);
    tokio::task::spawn_blocking(move || {
        if let Ok(f) = std::fs::File::options().write(true).open(&path) {
            let _ = f.set_modified(std::time::SystemTime::now());
        }
    });
}

/// The content `hash` is deleted for good: its thumbnails go too, the one the server drew and those browsers drew for
/// each space
pub async fn forget(st: &AppState, hash: &str) {
    let dir = st.thumb_path(hash).parent().map(std::path::Path::to_path_buf);
    let (Some(dir), prefix) = (dir, format!("{hash}.")) else { return };
    let _ = tokio::task::spawn_blocking(move || {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if e.file_name().to_str().is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".jpg") && !n.ends_with(".f.jpg")) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    })
    .await;
}

/// What `sweep` does with a file of the thumbnail folder (by its name): keeps it, removes it, or keeps it as long as
/// the content `hash` is there
#[derive(Debug, PartialEq)]
enum Swept<'a> {
    Keep,
    Remove,
    WhileStored(&'a str),
}

fn sweep_rule(name: &str, age: std::time::Duration) -> Swept<'_> {
    if name.ends_with(".tmp") {
        return if age > TMP_GRACE { Swept::Remove } else { Swept::Keep };
    }
    let Some(key) = name.strip_suffix(".jpg") else { return Swept::Remove };
    if age > std::time::Duration::from_secs(UNUSED_DAYS * 86400) {
        return Swept::Remove;
    }
    let (hash, rest) = key.split_once('.').map_or((key, None), |(h, r)| (h, Some(r)));
    if !crate::storage::is_hash(hash) {
        return Swept::Remove;
    }
    match rest {
        // A folder space's file
        Some("f") => Swept::Keep,
        // Content, as the server drew it or a browser drew it for a space
        None => Swept::WhileStored(hash),
        Some(space) if crate::util::is_new_id(space) => Swept::WhileStored(hash),
        Some(_) => Swept::Remove,
    }
}

/// Daily: removes the thumbnails nobody was shown for `UNUSED_DAYS` (those of folder spaces' files go this way, as
/// nothing tells when such a file is gone), those whose content is gone, and the temporary files a stop left
pub async fn sweep(st: &AppState) {
    SHOWN.lock().unwrap_or_else(|e| e.into_inner()).take();
    let root = st.data_dir.join("thumbs");
    let dirs: Vec<std::path::PathBuf> = match tokio::task::spawn_blocking(move || std::fs::read_dir(root).map(|r| r.flatten().map(|e| e.path()).collect())).await {
        Ok(Ok(d)) => d,
        _ => return,
    };
    let mut removed = 0;
    for dir in dirs {
        // Each folder holds the thumbnails of the keys starting with its name
        let read = tokio::task::spawn_blocking(move || -> Vec<(String, std::path::PathBuf, std::time::Duration)> {
            let now = std::time::SystemTime::now();
            std::fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| {
                    let age = e.metadata().ok()?.modified().ok().and_then(|t| now.duration_since(t).ok()).unwrap_or_default();
                    Some((e.file_name().into_string().ok()?, e.path(), age))
                })
                .collect()
        })
        .await
        .unwrap_or_default();
        let mut gone = Vec::new();
        let mut stored = Vec::new();
        for (name, path, age) in &read {
            match sweep_rule(name, *age) {
                Swept::Keep => {}
                Swept::Remove => gone.push(path.clone()),
                Swept::WhileStored(hash) => stored.push((hash.to_string(), path.clone())),
            }
        }
        if !stored.is_empty() {
            let hashes: Vec<&str> = stored.iter().map(|(h, _)| h.as_str()).collect();
            let missing: Result<Vec<(String,)>, _> =
                sqlx::query_as("SELECT j.value FROM json_each(?) j WHERE NOT EXISTS (SELECT 1 FROM blobs WHERE hash = j.value)")
                    .bind(serde_json::to_string(&hashes).unwrap())
                    .fetch_all(&st.db)
                    .await;
            let Ok(missing) = missing else { continue };
            let missing: std::collections::HashSet<String> = missing.into_iter().map(|(h,)| h).collect();
            gone.extend(stored.into_iter().filter(|(h, _)| missing.contains(h)).map(|(_, p)| p));
        }
        removed += gone.len();
        let _ = tokio::task::spawn_blocking(move || gone.iter().for_each(|p| drop(std::fs::remove_file(p)))).await;
    }
    if removed > 0 {
        tracing::info!("Removed {removed} thumbnail(s) no longer shown or whose file is gone");
    }
}

/// A cached thumbnail, with the headers that let the browser keep it
fn thumb_reply(data: Vec<u8>, etag: String) -> Response {
    ([(header::CONTENT_TYPE, "image/jpeg".to_string()), (header::CACHE_CONTROL, "private, max-age=604800".to_string()), (header::ETAG, etag)], data).into_response()
}

pub async fn thumbnail_response(st: &AppState, headers: &HeaderMap, n: &Node) -> AppResult<Response> {
    let made_here = thumbnailable(n);
    if !made_here && !browser_thumbnailable(n) {
        return Err(AppError::not_found("No thumbnail"));
    }
    let (source, size, hash) = thumb_key(st, n).await?;
    if made_here && size > MAX_THUMB_SOURCE as u64 {
        return Err(AppError::not_found("No thumbnail"));
    }
    let hash = hash.as_str();
    shown(st, hash);
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
        // Made in a task of its own, which holds its turn (a permit) until the thumbnail is in the cache: a browser
        // that stops waiting neither frees the turn while the picture is still being decoded nor throws the work away
        let (st, path) = (st.clone(), path.clone());
        tokio::spawn(async move {
            let _permit = st.part::<Memory>().permits.acquire().await.map_err(AppError::internal)?;
            if tokio::fs::try_exists(&path).await? {
                return Ok(());
            }
            let mut data = Vec::with_capacity(size as usize);
            source.open(&st, 0, size).await?.read_to_end(&mut data).await?;
            let max_alloc = st.part::<Memory>().decode_bytes;
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
            AppResult::Ok(())
        })
        .await??;
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

/// A thumbnail the browser made for a PDF or video the user may change (see `browser_thumbnailable`): whoever sees
/// it could change the file itself anyway, so a viewer can't choose the picture others see. Only a small JPEG or PNG
/// is taken, decoded with tight limits and encoded again, so what is kept is always a plain JPEG of the usual size. A
/// thumbnail already in the cache is kept: it belongs to the same content.
pub async fn upload_thumbnail(State(st): State<AppState>, user: User, Path(id): Path<String>, body: Bytes) -> AppResult<StatusCode> {
    let node = tree::node_for(&mut *st.db.acquire().await?, &user, &id, tree::Need::Write).await?;
    if !browser_thumbnailable(&node) {
        return Err(AppError::bad_request("This file doesn't take a thumbnail"));
    }
    if body.len() > MAX_THUMB_UPLOAD {
        return Err(AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "The thumbnail is too large"));
    }
    let (_, _, hash) = thumb_key(&st, &node).await?;
    let path = st.thumb_path(&hash);
    if tokio::fs::metadata(&path).await.is_ok_and(|m| m.len() > 0) {
        return Ok(StatusCode::NO_CONTENT);
    }
    let _permit = st.part::<Memory>().permits.acquire().await.map_err(AppError::internal)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{self, blob_file};

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
        let pdf = env.file(&amy, amy.root(), "Report.pdf").await;
        let hash = "ab".repeat(32);
        sqlx::query("INSERT INTO blobs (hash, size, refcount, created_at) VALUES (?, 1000, 1, 0)").bind(&hash).execute(&env.st.db).await.unwrap();
        sqlx::query("UPDATE nodes SET mime = 'application/pdf', blob_hash = ?, size = 1000 WHERE id = ?").bind(&hash).bind(&pdf).execute(&env.st.db).await.unwrap();
        // Kept for Amy's space only: the same content elsewhere doesn't get it
        let content = hash;
        let hash = format!("{content}.{}", env.drive_of(&pdf).await);
        let text = env.file(&amy, amy.root(), "notes.txt").await;
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

        // Someone who may only view the file can't choose the picture others see; an editor's upload doesn't replace it
        env.grant(&pdf, &ben, "viewer").await;
        assert_eq!(put(ben.clone(), pdf.clone(), png(100, 100)).await.unwrap_err().status, StatusCode::FORBIDDEN);
        env.grant(&pdf, &ben, "editor").await;
        assert_eq!(put(ben.clone(), pdf.clone(), png(100, 100)).await.unwrap(), StatusCode::NO_CONTENT);
        assert_eq!(std::fs::read(env.st.thumb_path(&hash)).unwrap(), body.to_vec());
        assert!(!env.st.thumb_path(&content).exists(), "not kept by content alone");
    }

    #[tokio::test]
    async fn a_thumbnail_is_finished_when_the_browser_stops_waiting_for_it() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let data = png(4000, 3000);
        let pic = env.stored_file(&amy, amy.root(), "large.png", &data).await;
        // The browser goes away while the picture is being decoded: the decoding keeps its turn until it is done, and
        // its thumbnail is kept, so asking again doesn't decode it again
        let asked = tokio::spawn(thumbnail(State(env.st.clone()), amy.clone(), Path(pic.clone()), HeaderMap::new()));
        while env.st.part::<Memory>().permits.available_permits() == 2 {
            assert!(!asked.is_finished(), "the thumbnail was made before its turn was seen");
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        asked.abort();
        let _ = asked.await;
        assert_eq!(env.st.part::<Memory>().permits.available_permits(), 1, "the turn was given back while the picture is still decoded");
        let cached = env.st.thumb_path(&crate::util::sha256_hex(&data));
        for _ in 0..3000 {
            if std::fs::metadata(&cached).is_ok_and(|m| m.len() > 0) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(std::fs::metadata(&cached).is_ok_and(|m| m.len() > 0), "the thumbnail was dropped with the request");
        for _ in 0..500 {
            if env.st.part::<Memory>().permits.available_permits() == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(env.st.part::<Memory>().permits.available_permits(), 2);
    }

    #[tokio::test]
    async fn thumbnails_are_made_once_and_pictures_that_cant_be_read_are_remembered() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let get = |id: String, headers: HeaderMap| thumbnail(State(env.st.clone()), amy.clone(), Path(id), headers);

        let pic = env.stored_file(&amy, amy.root(), "pic.png", &png(640, 320)).await;
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
            let id = env.stored_file(&amy, amy.root(), name, &data).await;
            assert_eq!(get(id.clone(), HeaderMap::new()).await.unwrap_err().status, StatusCode::NOT_FOUND, "{name}");
            let cached = env.st.thumb_path(&crate::util::sha256_hex(&data));
            assert_eq!(std::fs::metadata(&cached).unwrap().len(), 0, "{name}");
            // Asked again, the cache answers: the content isn't read (it could no longer be there)
            std::fs::remove_file(blob_file(&env, &data)).unwrap();
            let err = get(id, HeaderMap::new()).await.unwrap_err();
            assert_eq!((err.status, err.message.as_str()), (StatusCode::NOT_FOUND, "No thumbnail"), "{name}");
        }

        // Too large a file isn't read at all
        let big = env.stored_file(&amy, amy.root(), "big.png", &png(8, 8)).await;
        sqlx::query("UPDATE nodes SET size = ? WHERE id = ?").bind(MAX_THUMB_SOURCE + 1).bind(&big).execute(&env.st.db).await.unwrap();
        assert_eq!(get(big, HeaderMap::new()).await.unwrap_err().status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn the_thumbnail_folder_is_swept_by_what_each_file_is() {
        let (day, hash, space) = (std::time::Duration::from_secs(86400), "ab".repeat(32), crate::util::new_id());
        let fresh = std::time::Duration::from_secs(60);
        assert_eq!(sweep_rule(&format!("{hash}.jpg"), fresh), Swept::WhileStored(&hash));
        assert_eq!(sweep_rule(&format!("{hash}.{space}.jpg"), fresh), Swept::WhileStored(&hash));
        assert_eq!(sweep_rule(&format!("{hash}.f.jpg"), fresh), Swept::Keep);
        // Not shown for UNUSED_DAYS: removed, whatever it is
        assert_eq!(sweep_rule(&format!("{hash}.f.jpg"), day * 31), Swept::Remove);
        assert_eq!(sweep_rule(&format!("{hash}.jpg"), day * 31), Swept::Remove);
        // Left by a stop while it was written
        assert_eq!(sweep_rule(&format!("{hash}.{space}.tmp"), fresh), Swept::Keep);
        assert_eq!(sweep_rule(&format!("{hash}.{space}.tmp"), day), Swept::Remove);
        // Not ThirtyFile's
        assert_eq!(sweep_rule("notes.txt", fresh), Swept::Remove);
        assert_eq!(sweep_rule(&format!("{hash}.x.jpg"), fresh), Swept::Remove);
    }

    /// The thumbnails of folder spaces' files in the cache
    fn folder_thumbs(env: &testutil::TestEnv) -> usize {
        let dirs = std::fs::read_dir(env.dir.join("thumbs")).unwrap().flatten();
        dirs.flat_map(|d| std::fs::read_dir(d.path()).unwrap().flatten()).filter(|e| e.file_name().to_string_lossy().ends_with(".f.jpg")).count()
    }

    #[tokio::test]
    async fn thumbnails_go_with_their_content_and_when_nobody_was_shown_them_for_a_month() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let get = |id: String| thumbnail(State(env.st.clone()), amy.clone(), Path(id), HeaderMap::new());
        // A picture the server drew, and a PDF a browser drew, of content in the content store
        let data = png(64, 64);
        let pic = env.stored_file(&amy, amy.root(), "pic.png", &data).await;
        get(pic.clone()).await.unwrap();
        let drawn = env.st.thumb_path(&crate::util::sha256_hex(&data));
        let pdf = env.stored_file(&amy, amy.root(), "Report.pdf", b"%PDF-1.7 report").await;
        sqlx::query("UPDATE nodes SET mime = 'application/pdf' WHERE id = ?").bind(&pdf).execute(&env.st.db).await.unwrap();
        upload_thumbnail(State(env.st.clone()), amy.clone(), Path(pdf.clone()), png(320, 200)).await.unwrap();
        let uploaded = env.st.thumb_path(&format!("{}.{}", crate::util::sha256_hex(b"%PDF-1.7 report"), env.drive_of(&pdf).await));
        assert!(drawn.is_file() && uploaded.is_file());
        // A folder space's picture, a thumbnail nobody was shown for a month, and what a stop left
        let space = env.folder_space("Disk").await;
        testutil::write_old(&space.dir.join("photo.png"), &png(32, 32));
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (photo, _) = env.node_at(&space.drive, "photo.png").await.unwrap();
        thumbnail(State(env.st.clone()), env.admin().await, Path(photo), HeaderMap::new()).await.unwrap();
        assert_eq!(folder_thumbs(&env), 1);
        let old = env.st.thumb_path(&"cd".repeat(32));
        write_thumb(&old, b"old").await.unwrap();
        let month_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(31 * 86400);
        std::fs::File::options().write(true).open(&old).unwrap().set_modified(month_ago).unwrap();
        let left = old.with_extension("x.tmp");
        std::fs::write(&left, b"half").unwrap();
        std::fs::File::options().write(true).open(&left).unwrap().set_modified(month_ago).unwrap();
        sweep(&env.st).await;
        assert!(drawn.is_file() && uploaded.is_file() && folder_thumbs(&env) == 1, "thumbnails in use are kept");
        assert!(!old.exists() && !left.exists());

        // Shown, a thumbnail of a month ago is kept: it is dated today again
        std::fs::File::options().write(true).open(&drawn).unwrap().set_modified(month_ago).unwrap();
        get(pic.clone()).await.unwrap();
        for _ in 0..100 {
            if std::fs::metadata(&drawn).unwrap().modified().unwrap() > month_ago + std::time::Duration::from_secs(86400) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        sweep(&env.st).await;
        assert!(drawn.is_file());

        // The content deleted for good: its thumbnails go with it, the server's and the browsers'
        for id in [&pic, &pdf] {
            let mut c = env.st.db.acquire().await.unwrap();
            let blobs = crate::tree::purge_subtree(&mut c, id).await.unwrap();
            drop(c);
            crate::tree::remove_unreferenced(&env.st, blobs).await;
        }
        assert!(!drawn.exists() && !uploaded.exists());
    }
}
