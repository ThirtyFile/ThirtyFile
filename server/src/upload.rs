//! tus 1.0 resumable uploads (core + creation + termination).
//! The frontend uses tus-js-client; after an interruption (including a browser refresh) the upload resumes where it left off.
//! Visitors of a share link that accepts files upload the same way (see `shares`): their files belong to the link's creator.

use std::{io::SeekFrom, path::PathBuf};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::Engine;
use futures_util::StreamExt;
use sha2::{
    Digest, Sha256,
    digest::common::hazmat::{SerializableState, SerializedState},
};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use crate::{
    auth::User,
    content,
    error::{AppError, AppResult},
    logs,
    state::AppState,
    tree,
    util::{new_id, now, validate_name},
};

const TUS_VERSION: &str = "1.0.0";
pub use crate::tree::UPLOAD_TTL;
const MAX_UPLOAD_LENGTH: u64 = 1 << 50;
/// Folder depth an uploaded folder tree may create below the target (each level costs queries under the write lock)
const MAX_REL_DEPTH: usize = 64;

#[derive(sqlx::FromRow)]
struct Upload {
    id: String,
    owner_id: i64,
    parent_id: String,
    rel_path: String,
    name: String,
    size: i64,
    offset: i64,
    drive_id: Option<String>,
    /// Uploads started together (one folder dropped or picked at once); empty for older clients
    batch: String,
    /// The file this upload became, once finished
    node_id: Option<String>,
    /// "replace": a file with the same name gets the new content; otherwise both are kept (the uploads table in migrations/0001_init.sql)
    on_conflict: String,
    /// The SHA-256 state after the first `hashed` bytes (the uploads table in migrations/0001_init.sql)
    hash_state: Option<Vec<u8>>,
    hashed: i64,
}

/// Who is uploading: a signed-in person, or a visitor of a share link. The file belongs to `user` either way (the
/// link's creator for a link), and every permission is checked against that account.
#[derive(Clone)]
pub struct Uploader {
    pub user: User,
    pub share: Option<ShareUpload>,
}

/// An upload through a share link
#[derive(Clone)]
pub struct ShareUpload {
    /// The link's token
    pub id: String,
    /// The shared folder: uploads stay inside it
    pub root: String,
    /// Visitors can't see the folder: files go into the shared folder itself
    pub drop_only: bool,
    pub visitor: crate::logs::Visitor,
}

impl Uploader {
    fn signed_in(user: User) -> Uploader {
        Uploader { user, share: None }
    }
    /// The activity log's detail for the uploaded file
    fn log_detail(&self) -> String {
        self.share.as_ref().map(|s| format!("Through share link /share/{}", s.id)).unwrap_or_default()
    }
    fn share_id(&self) -> Option<&str> {
        self.share.as_ref().map(|s| s.id.as_str())
    }
    /// Where the client finds the upload: the link's own address for visitors
    fn location(&self, id: &str) -> String {
        match &self.share {
            Some(s) => format!("/api/public/shares/{}/uploads/{id}", s.id),
            None => format!("/api/uploads/{id}"),
        }
    }
}

/// Uploads through one share link that may be in progress at once: each one reserves its size in the space and a
/// temporary file, so a visitor can't start any number of them
pub const MAX_PENDING_PER_SHARE: i64 = 200;
/// Bytes those uploads may hold together (their temporary files are in the data folder)
pub const MAX_PENDING_BYTES_PER_SHARE: u64 = 20 * 1024 * 1024 * 1024;
/// A day without progress ends an upload through a link (people's own uploads stay resumable for `UPLOAD_TTL`)
const LINK_UPLOAD_IDLE: i64 = 86400;

/// How long a finished upload is remembered, so a client that lost the last response learns the result
const FINISHED_TTL: i64 = 24 * 3600;

fn tus(res: &mut Response) {
    res.headers_mut().insert(HeaderName::from_static("tus-resumable"), HeaderValue::from_static(TUS_VERSION));
}

fn header_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

fn upload_path(st: &AppState, id: &str) -> PathBuf {
    st.tmp_dir().join(format!("upload-{id}"))
}

/// Upload-Metadata: `key base64,key base64`
fn parse_metadata(headers: &HeaderMap) -> std::collections::HashMap<String, String> {
    let raw = headers.get("upload-metadata").and_then(|v| v.to_str().ok()).unwrap_or_default();
    raw.split(',')
        .filter_map(|pair| {
            let mut it = pair.trim().splitn(2, ' ');
            let key = it.next()?.to_string();
            let value =
                it.next().and_then(|v| base64::engine::general_purpose::STANDARD.decode(v.trim()).ok()).and_then(|b| String::from_utf8(b).ok()).unwrap_or_default();
            Some((key, value))
        })
        .collect()
}

pub async fn options(State(st): State<AppState>) -> Response {
    let mut res = StatusCode::NO_CONTENT.into_response();
    tus(&mut res);
    let h = res.headers_mut();
    h.insert("tus-version", HeaderValue::from_static(TUS_VERSION));
    h.insert("tus-extension", HeaderValue::from_static("creation,termination"));
    if st.max_upload > 0 {
        h.insert("tus-max-size", st.max_upload.into());
    }
    res
}

pub async fn create(State(st): State<AppState>, user: User, headers: HeaderMap) -> AppResult<Response> {
    create_as(&st, &Uploader::signed_in(user), &headers).await
}

pub async fn create_as(st: &AppState, up: &Uploader, headers: &HeaderMap) -> AppResult<Response> {
    let user = &up.user;
    let size = header_u64(headers, "upload-length").ok_or_else(|| AppError::bad_request("Missing Upload-Length"))?;
    // Sizes are stored as i64 and summed for quotas: refuse absurd values before they can overflow (1 PiB is far beyond any single file)
    if size > MAX_UPLOAD_LENGTH {
        return Err(AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "Invalid Upload-Length"));
    }
    if st.max_upload > 0 && size > st.max_upload {
        return Err(AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "The file exceeds the upload size limit"));
    }
    let meta = parse_metadata(headers);
    let name = validate_name(meta.get("filename").map(String::as_str).unwrap_or_default())?;
    let parent_id = match &up.share {
        // "root" is the shared folder here, never the creator's own files
        Some(s) => meta.get("parentId").filter(|p| !p.is_empty() && *p != "root").cloned().unwrap_or_else(|| s.root.clone()),
        None => meta.get("parentId").cloned().unwrap_or_else(|| "root".into()),
    };
    let mut rel_parts = Vec::new();
    // A link that only accepts files puts every file into the shared folder itself: a folder path would reach
    // folders there that its visitors can't see
    let rel_path = if up.share.as_ref().is_some_and(|s| s.drop_only) { "" } else { meta.get("relativePath").map(String::as_str).unwrap_or_default() };
    for part in rel_path.split('/').filter(|p| !p.is_empty()) {
        if rel_parts.len() >= MAX_REL_DEPTH {
            return Err(AppError::bad_request("The folder path is too deep"));
        }
        rel_parts.push(validate_name(part)?);
    }
    let rel_path = rel_parts.join("/");
    let batch = meta.get("batchId").filter(|b| (1..=64).contains(&b.len()) && b.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).cloned().unwrap_or_default();
    // Visitors of a share link never replace files: they may not even see what's there
    let on_conflict = match meta.get("onConflict").map(String::as_str) {
        Some("replace") if up.share.is_none() => "replace",
        _ => "keep",
    };

    let id = new_id();
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let parent = tree::folder_for(&mut tx, user, &parent_id, tree::Need::Write).await?;
        // Refused before anything is sent, rather than when the upload is put in place
        if parent.in_folder_space() {
            for n in rel_parts.iter().chain(std::iter::once(&name)) {
                crate::fsops::check_name(n)?;
            }
        }
        if let Some(s) = &up.share {
            check_in_share(&mut tx, s, &parent.id).await?;
            let (pending, bytes): (i64, i64) =
                sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(size), 0) FROM uploads WHERE share_id = ? AND node_id IS NULL AND expires_at > ?")
                    .bind(&s.id)
                    .bind(now())
                    .fetch_one(&mut *tx)
                    .await?;
            if pending >= MAX_PENDING_PER_SHARE || bytes as u64 + size > MAX_PENDING_BYTES_PER_SHARE {
                return Err(AppError::new(StatusCode::TOO_MANY_REQUESTS, "Too many uploads at once through this link. Try again later."));
            }
        }
        tree::check_quota(&mut tx, parent.drive(), size as i64).await?;
        let ts = now();
        sqlx::query(
            "INSERT INTO uploads (id, owner_id, parent_id, rel_path, name, size, offset, created_at, expires_at, drive_id, batch, share_id, on_conflict)
             VALUES (?, ?, ?, ?, ?, ?, 0, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(user.id)
        .bind(&parent.id)
        .bind(&rel_path)
        .bind(&name)
        .bind(size as i64)
        .bind(ts)
        .bind(ts + UPLOAD_TTL)
        .bind(&parent.drive_id)
        .bind(&batch)
        .bind(up.share_id())
        .bind(on_conflict)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
    }
    // Create the temp file only after the database record succeeds; if creation fails, undo the record, leaving neither uploads without a file nor unrecorded temp files
    if let Err(e) = tokio::fs::File::create(upload_path(st, &id)).await {
        let _w = st.write_lock.lock().await;
        forget_upload(st, &id).await;
        return Err(e.into());
    }

    let mut res = StatusCode::CREATED.into_response();
    if size == 0 {
        let upload = load(st, up, &id).await?;
        let guard = ActiveGuard::claim(st, &id).ok_or_else(|| AppError::new(StatusCode::LOCKED, "This file is already being uploaded"))?;
        let node_id = finish(st, up, upload, guard).await?;
        finished_headers(st, up, res.headers_mut(), &node_id).await?;
    }
    tus(&mut res);
    res.headers_mut().insert(header::LOCATION, HeaderValue::from_str(&up.location(&id)).unwrap());
    Ok(res)
}

/// The folder is inside the shared folder (and, for a link that only accepts files, is the shared folder itself)
async fn check_in_share(conn: &mut sqlx::SqliteConnection, share: &ShareUpload, folder: &str) -> AppResult<()> {
    let inside = if share.drop_only { folder == share.root } else { tree::is_within(conn, folder, &share.root).await? };
    if !inside {
        return Err(AppError::not_found("Item not found"));
    }
    Ok(())
}

/// An upload of this person, or of this share link: a signed-in person can't continue a visitor's upload and the other way round
async fn load(st: &AppState, up: &Uploader, id: &str) -> AppResult<Upload> {
    sqlx::query_as(
        "SELECT id, owner_id, parent_id, rel_path, name, size, offset, drive_id, batch, node_id, on_conflict, hash_state, hashed FROM uploads
         WHERE id = ?1 AND ((?3 IS NULL AND share_id IS NULL AND owner_id = ?2) OR share_id = ?3)",
    )
    .bind(id)
    .bind(up.user.id)
    .bind(up.share_id())
    .fetch_optional(&st.db)
    .await?
    .ok_or_else(|| AppError::not_found("The upload doesn't exist or has expired"))
}

pub async fn head(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<Response> {
    head_as(&st, &Uploader::signed_in(user), &id).await
}

pub async fn head_as(st: &AppState, up: &Uploader, id: &str) -> AppResult<Response> {
    let (st, id) = (st.clone(), id.to_string());
    let upload = load(&st, up, &id).await?;
    let (size, offset, mut node_id) = (upload.size, upload.offset, upload.node_id.clone());
    if node_id.is_none() && offset == size {
        // Everything arrived but the file wasn't created: the server stopped before finishing. Finish now if the data
        // is still there; otherwise the client must start over, rather than being told the upload succeeded.
        let guard = ActiveGuard::claim(&st, &id).ok_or_else(|| AppError::new(StatusCode::LOCKED, "This file is already being uploaded"))?;
        if !tokio::fs::try_exists(upload_path(&st, &id)).await.unwrap_or(false) {
            let _w = st.write_lock.lock().await;
            sqlx::query("DELETE FROM uploads WHERE id = ?").bind(&id).execute(&st.db).await?;
            return Err(AppError::not_found("The upload doesn't exist or has expired"));
        }
        node_id = Some(finish(&st, up, upload, guard).await?);
    }
    let mut res = StatusCode::OK.into_response();
    tus(&mut res);
    if let Some(n) = &node_id {
        finished_headers(&st, up, res.headers_mut(), n).await?;
    }
    let h = res.headers_mut();
    // A finished upload reports everything as received, whatever the row said before finishing
    h.insert("upload-offset", if node_id.is_some() { size } else { offset }.into());
    h.insert("upload-length", size.into());
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

/// Tells the client which file the upload became: its id, and its name (percent-encoded), which differs from the
/// uploaded one when both files were kept ("Report (1).docx"). Visitors of a link that only accepts files aren't told
/// the name: a changed one would say which names are taken in a folder they can't see.
async fn finished_headers(st: &AppState, up: &Uploader, h: &mut HeaderMap, node_id: &str) -> AppResult<()> {
    h.insert("x-node-id", HeaderValue::from_str(node_id).map_err(AppError::internal)?);
    if up.share.as_ref().is_some_and(|s| s.drop_only) {
        return Ok(());
    }
    let name: Option<(String,)> = sqlx::query_as("SELECT name FROM nodes WHERE id = ?").bind(node_id).fetch_optional(&st.db).await?;
    if let Some((name,)) = name {
        let encoded = percent_encoding::utf8_percent_encode(&name, percent_encoding::NON_ALPHANUMERIC).to_string();
        h.insert("x-node-name", HeaderValue::from_str(&encoded).map_err(AppError::internal)?);
    }
    Ok(())
}

/// Makes sure the same upload isn't written or finished twice at the same time. It owns the state, so it can move
/// into the task that finishes the upload and stay held until that is done, even if the request is dropped.
struct ActiveGuard {
    st: AppState,
    id: String,
}

impl ActiveGuard {
    fn claim(st: &AppState, id: &str) -> Option<ActiveGuard> {
        st.active_uploads.lock().unwrap().insert(id.to_string()).then(|| ActiveGuard { st: st.clone(), id: id.to_string() })
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.st.active_uploads.lock().unwrap().remove(&self.id);
    }
}

pub async fn patch(State(st): State<AppState>, user: User, Path(id): Path<String>, headers: HeaderMap, body: Body) -> AppResult<Response> {
    patch_as(&st, &Uploader::signed_in(user), &id, &headers, body).await
}

pub async fn patch_as(st: &AppState, up: &Uploader, id: &str, headers: &HeaderMap, body: Body) -> AppResult<Response> {
    let (st, id) = (st.clone(), id.to_string());
    if headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()) != Some("application/offset+octet-stream") {
        return Err(AppError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Content-Type must be application/offset+octet-stream"));
    }
    // Ownership first, so other users can't mark someone else's upload as active
    load(&st, up, &id).await?;
    let guard = ActiveGuard::claim(&st, &id).ok_or_else(|| AppError::new(StatusCode::LOCKED, "This file is already being uploaded"))?;
    // Read the offset only while holding the guard: a retried request must not work from the offset before the
    // previous request finished
    let upload = load(&st, up, &id).await?;
    if let Some(node_id) = &upload.node_id {
        let mut res = StatusCode::NO_CONTENT.into_response();
        tus(&mut res);
        res.headers_mut().insert("upload-offset", upload.size.into());
        finished_headers(&st, up, res.headers_mut(), node_id).await?;
        return Ok(res);
    }
    // Only data arriving moves the deadline (below), so requests that send nothing can't keep space reserved. The
    // hourly cleanup leaves an upload that is being received alone (`purge_expired`).
    let client_offset = header_u64(headers, "upload-offset").ok_or_else(|| AppError::bad_request("Missing Upload-Offset"))?;
    if client_offset != upload.offset as u64 {
        return Err(AppError::conflict("Upload-Offset mismatch"));
    }

    let size = upload.size as u64;
    let mut offset = upload.offset as u64;
    // Content-store uploads are hashed as the data arrives, continuing from the part received before
    let mut hasher = None;
    if stored_by_hash(&st, &upload).await? {
        match hash_so_far(&st, &upload).await {
            Ok(h) => hasher = Some(h),
            Err(e) => {
                // The part received so far is gone: the client has to start over
                let _w = st.write_lock.lock().await;
                sqlx::query("DELETE FROM uploads WHERE id = ?").bind(&id).execute(&st.db).await?;
                let _ = tokio::fs::remove_file(upload_path(&st, &id)).await;
                return Err(discarded(e));
            }
        }
    }
    let mut file = tokio::fs::OpenOptions::new().write(true).open(upload_path(&st, &id)).await?;
    file.set_len(offset).await?;
    file.seek(SeekFrom::Start(offset)).await?;

    let mut stream = body.into_data_stream();
    let mut failure: Option<AppError> = None;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                if offset + bytes.len() as u64 > size {
                    failure = Some(AppError::bad_request("The uploaded data exceeds the declared file size"));
                    break;
                }
                if let Err(e) = file.write_all(&bytes).await {
                    failure = Some(e.into());
                    break;
                }
                if let Some(h) = &mut hasher {
                    h.update(&bytes);
                }
                offset += bytes.len() as u64;
            }
            Err(_) => {
                // Connection interrupted: keep what was received so the upload can resume later
                failure = Some(AppError::bad_request("Connection interrupted"));
                break;
            }
        }
    }
    file.flush().await?;
    file.sync_data().await?;
    drop(file);
    {
        let _w = st.write_lock.lock().await;
        // Progress extends the deadline: a large upload that keeps resuming isn't discarded after 7 days (a request
        // that brought nothing doesn't). The hash state is saved with the offset (the data is on disk by now), so a
        // resumed upload continues from it.
        sqlx::query(
            "UPDATE uploads SET offset = ?1, expires_at = CASE WHEN ?1 > offset THEN ?2 ELSE expires_at END, hash_state = ?3,
             hashed = CASE WHEN ?3 IS NULL THEN 0 ELSE ?1 END WHERE id = ?4",
        )
        .bind(offset as i64)
        .bind(now() + UPLOAD_TTL)
        .bind(hasher.as_ref().map(|h| h.serialize().to_vec()))
        .bind(&id)
        .execute(&st.db)
        .await?;
    }
    if let Some(e) = failure {
        return Err(e);
    }

    let mut res = StatusCode::NO_CONTENT.into_response();
    if offset == size {
        let upload = load(&st, up, &id).await?;
        let node_id = finish(&st, up, upload, guard).await?;
        finished_headers(&st, up, res.headers_mut(), &node_id).await?;
    }
    tus(&mut res);
    res.headers_mut().insert("upload-offset", offset.into());
    Ok(res)
}

/// Finishes an upload in a task of its own: storing the content and creating the file must not stop halfway when the
/// client or a proxy drops the request (which would leave content in storage without a file, or a file the client
/// never hears about). The guard stays held until the task is done.
async fn finish(st: &AppState, up: &Uploader, upload: Upload, guard: ActiveGuard) -> AppResult<String> {
    let (st, up) = (st.clone(), up.clone());
    tokio::spawn(async move {
        let result = finalize(&st, &up, upload).await;
        if let (Ok(id), Some(share)) = (&result, &up.share) {
            // The share's access log shows the file each visitor sent (the file exists either way)
            let node = match st.db.acquire().await {
                Ok(mut c) => tree::get_node(&mut c, id).await.ok().flatten(),
                Err(_) => None,
            };
            crate::logs::record_share_access(&st, &share.id, up.user.id, node.as_ref(), "upload", &share.visitor);
            // The link's creator is told the file arrived
            if let Some(node) = &node
                && let Err(e) = crate::notify::link_upload(&st, up.user.id, &share.id, node).await
            {
                tracing::warn!("Couldn't tell about a file received through a link: {}", e.message);
            }
        }
        drop(guard);
        result
    })
    .await
    .map_err(AppError::internal)?
}

/// The upload's folder was deleted or put in the trash while it was running. It used to land in the personal space
/// instead, where nobody looked for it: now it fails and says why.
fn folder_gone() -> AppError {
    AppError::not_found("The folder you were uploading to was deleted or moved to the trash")
}

fn discarded(e: AppError) -> AppError {
    AppError::new(e.status, format!("{}. The upload was discarded; start it again.", e.message.trim_end_matches('.'))).with_code("upload_discarded")
}

/// Whether the upload becomes content of the content store, named by its hash (files of folder spaces aren't hashed)
async fn stored_by_hash(st: &AppState, upload: &Upload) -> AppResult<bool> {
    let mode: Option<(String,)> = sqlx::query_as("SELECT mode FROM drives WHERE id = ?").bind(&upload.drive_id).fetch_optional(&st.db).await?;
    Ok(mode.is_none_or(|(m,)| m != "folder"))
}

/// The hash of the part received so far as saved with it, if the saved state belongs to that part
fn saved_hasher(upload: &Upload) -> Option<Sha256> {
    let state = upload.hash_state.as_deref().filter(|_| upload.hashed == upload.offset)?;
    Sha256::deserialize(&SerializedState::<Sha256>::try_from(state).ok()?).ok()
}

/// The hash of the part received so far: the saved state, else (an upload started before states were saved, or one
/// whose state doesn't match) that part read once
async fn hash_so_far(st: &AppState, upload: &Upload) -> AppResult<Sha256> {
    if let Some(h) = saved_hasher(upload) {
        return Ok(h);
    }
    let (path, len) = (upload_path(st, &upload.id), upload.offset.max(0) as u64);
    let hashed = tokio::task::spawn_blocking(move || -> std::io::Result<(Sha256, u64)> {
        use std::io::Read;
        let mut hasher = Sha256::new();
        if len == 0 {
            return Ok((hasher, 0));
        }
        let mut f = std::fs::File::open(path)?.take(len);
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
        Ok((hasher, total))
    })
    .await
    .map_err(AppError::internal)?;
    match hashed {
        Ok((h, n)) if n == len => Ok(h),
        _ => Err(AppError::not_found("The part of the file received so far is missing")),
    }
}

/// Drops the row of an upload that can't go on (its temp file is gone), so it no longer counts against the space's size
/// limit. The caller holds the write lock. When that fails (the disk is full, say), it is tried again in the background
/// for a while, rather than the row reserving the space until the upload expires.
async fn forget_upload(st: &AppState, id: &str) {
    let delete = |st: AppState, id: String| async move { sqlx::query("DELETE FROM uploads WHERE id = ?").bind(id).execute(&st.db).await };
    let Err(e) = delete(st.clone(), id.to_string()).await else { return };
    tracing::warn!("Couldn't remove an upload that stopped: {e}");
    let (st, id) = (st.clone(), id.to_string());
    tokio::spawn(async move {
        let wait = std::time::Duration::from_millis(if cfg!(test) { 50 } else { 30_000 });
        for _ in 0..20 {
            tokio::time::sleep(wait).await;
            let _w = st.write_lock.lock().await;
            if delete(st.clone(), id.clone()).await.is_ok() {
                return;
            }
        }
        tracing::warn!("Gave up removing an upload that stopped: it counts against its space's size limit until it expires");
    });
}

/// Upload finished: store the content where the space keeps it, then create the file (or give an existing one the new
/// content) through content.rs
async fn finalize(st: &AppState, up: &Uploader, upload: Upload) -> AppResult<String> {
    let path = upload_path(st, &upload.id);
    let parent = tree::get_node(&mut *st.db.acquire().await?, &upload.parent_id).await?;
    let Some(parent) = parent.filter(|p| p.is_folder() && p.trashed_at.is_none()) else {
        let _w = st.write_lock.lock().await;
        forget_upload(st, &upload.id).await;
        let _ = tokio::fs::remove_file(&path).await;
        return Err(discarded(folder_gone()));
    };
    let size = tokio::fs::metadata(&path).await?.len();
    if size != upload.size as u64 {
        return Err(AppError::bad_request("File size mismatch"));
    }
    // Content for the content store was hashed while it arrived; it is read again only without a saved state that
    // matches the whole file
    let hash = saved_hasher(&upload).filter(|_| upload.offset == upload.size).map(|h| hex::encode(h.finalize()));
    // Stored first, without holding the write lock (S3 may take a while). Should that fail, the temporary file is
    // still there and finishing is tried again (`head_as`)
    let staged = content::stage(st, &parent, content::Received { path, size, hash }).await?;
    let mut turn = staged.turn(st).await;
    let _w = st.write_lock.lock().await;
    let result = async {
        turn.ready()?;
        commit_upload(st, up, &upload, &staged, size as i64).await
    }
    .await;
    match result {
        Ok((id, written)) => {
            staged.finish(st, written).await;
            Ok(id)
        }
        Err(e) => {
            // The staged content goes, so the upload can't be resumed: drop its row (it would otherwise count against
            // the quota for 7 days)
            staged.abandon(st).await;
            forget_upload(st, &upload.id).await;
            Err(discarded(e))
        }
    }
}

/// The file whose content an upload replaces: one with the same name in `folder` when the upload was told to replace
/// (a folder with that name can't take a file's content, so both are kept then)
async fn replaced_file(conn: &mut sqlx::SqliteConnection, user: &User, upload: &Upload, folder: &str) -> AppResult<Option<tree::Node>> {
    if upload.on_conflict != "replace" {
        return Ok(None);
    }
    match tree::find_child(conn, folder, &upload.name).await?.filter(|n| !n.is_folder()) {
        Some(existing) => Ok(Some(tree::node_for(conn, user, &existing.id, tree::Need::Write).await?)),
        None => Ok(None),
    }
}

/// Creates the file (or gives the file it replaces the new content) while holding the write lock; returns the file's
/// id and what to finish after the commit
async fn commit_upload(st: &AppState, up: &Uploader, upload: &Upload, staged: &content::Staged, size: i64) -> AppResult<(String, content::Written)> {
    let user = &up.user;
    let mut tx = crate::db::begin_write(&st.db).await?;
    // The account's upload permission may have been removed while the upload was running
    if !user.can_write && !user.is_admin() {
        return Err(AppError::forbidden("You no longer have permission to upload files"));
    }
    // Through a share link, the file goes into the folder as it is now: still inside the shared folder, and still
    // writable by the link's creator; it never falls back to the creator's own files
    if let Some(s) = &up.share {
        let target = tree::folder_for(&mut tx, user, &upload.parent_id, tree::Need::Write).await?;
        check_in_share(&mut tx, s, &target.id).await?;
    }
    // The target folder may have been deleted, or the permission to it removed, during the upload
    let parent = tree::get_node(&mut tx, &upload.parent_id).await?.filter(|p| p.trashed_at.is_none() && p.is_folder()).ok_or_else(folder_gone)?;
    let role = tree::role_on(&mut tx, user, &parent).await?;
    if role.is_none_or(|r| tree::allows(user, r, tree::Need::Write).is_err()) {
        return Err(AppError::forbidden("You no longer have permission to upload to this folder"));
    }
    staged.check(&parent)?;
    let folder_id = crate::content::ensure_folders(&mut tx, upload.owner_id, &parent.id, &upload.rel_path, &upload.batch).await?;
    let folder = tree::get_node(&mut tx, &folder_id).await?.ok_or_else(|| AppError::not_found("Folder not found"))?;
    let replaced = replaced_file(&mut tx, user, upload, &folder.id).await?;
    // Checked again now: an upload idle for a day stopped holding its space (it may be taken by now), and a folder
    // moved to another space meanwhile must have room there. Its own reservation isn't counted twice.
    let grows = size - replaced.as_ref().map_or(0, |r| r.size);
    tree::check_quota_except(&mut tx, folder.drive(), grows, Some(&upload.id)).await?;
    let (id, written) = match replaced {
        Some(existing) => {
            let written = content::replace(&mut tx, st, staged, &existing, user.id).await?;
            logs::record_activity(&mut tx, user, Some(&existing), "upload", "Replaced the existing file").await?;
            (existing.id.clone(), written)
        }
        None => {
            let name = content::free_name(&mut tx, &folder, &upload.name).await?;
            let (id, written) = content::create(&mut tx, staged, upload.owner_id, &folder, &name, None).await?;
            if let Some(n) = tree::get_node(&mut tx, &id).await? {
                logs::record_activity(&mut tx, user, Some(&n), "upload", &up.log_detail()).await?;
            }
            (id, written)
        }
    };
    // Kept for a day with the file, for clients that lost the response
    sqlx::query("UPDATE uploads SET node_id = ?, offset = size, expires_at = ? WHERE id = ?")
        .bind(&id)
        .bind(now() + FINISHED_TTL)
        .bind(&upload.id)
        .execute(&mut *tx)
        .await?;
    tree::touch(&mut tx, &folder.id).await?;
    tx.commit().await?;
    Ok((id, written))
}

pub async fn delete(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<Response> {
    delete_as(&st, &Uploader::signed_in(user), &id).await
}

pub async fn delete_as(st: &AppState, up: &Uploader, id: &str) -> AppResult<Response> {
    let upload = load(st, up, id).await?;
    {
        let _w = st.write_lock.lock().await;
        sqlx::query("DELETE FROM uploads WHERE id = ?").bind(&upload.id).execute(&st.db).await?;
    }
    let _ = tokio::fs::remove_file(upload_path(st, &upload.id)).await;
    let mut res = StatusCode::NO_CONTENT.into_response();
    tus(&mut res);
    Ok(res)
}

/// Removes files in data/tmp that no upload, save or migration is using any more: upload parts whose row is gone
/// (the process stopped between deleting the row and the file) and anything else older than a day
pub async fn clean_tmp(st: &AppState) -> AppResult<usize> {
    // The folder is listed before the table is read: an upload's row is written before its file is created, so every
    // file seen here already has its row if it is going to have one. Files touched in the last hour are left alone anyway.
    let mut files = Vec::new();
    let mut dir = match tokio::fs::read_dir(st.tmp_dir()).await {
        Ok(d) => d,
        Err(_) => return Ok(0),
    };
    while let Ok(Some(entry)) = dir.next_entry().await {
        let modified = entry.metadata().await.and_then(|m| m.modified()).ok();
        files.push((entry.file_name().to_string_lossy().into_owned(), entry.path(), modified));
    }
    let known: std::collections::HashSet<String> =
        sqlx::query_as::<_, (String,)>("SELECT id FROM uploads").fetch_all(&st.db).await?.into_iter().map(|(id,)| format!("upload-{id}")).collect();
    let now = std::time::SystemTime::now();
    let older_than = |t: Option<std::time::SystemTime>, secs: u64| t.is_some_and(|t| t < now - std::time::Duration::from_secs(secs));
    let mut removed = 0;
    for (name, path, modified) in files {
        let stale = if name.starts_with("upload-") { !known.contains(&name) && older_than(modified, 3600) } else { older_than(modified, 86400) };
        if stale && tokio::fs::remove_file(path).await.is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Cleans up expired unfinished uploads, and uploads through a link without progress for a day. One that is receiving
/// data right now is left alone (its deadline moves when the data is in).
pub async fn purge_expired(st: &AppState) -> AppResult<usize> {
    let ids: Vec<(String,)> = {
        let _w = st.write_lock.lock().await;
        let ts = now();
        let expired: Vec<(String,)> =
            sqlx::query_as("SELECT id FROM uploads WHERE expires_at < ?1 OR (share_id IS NOT NULL AND node_id IS NULL AND expires_at < ?1 + ?2 - ?3)")
                .bind(ts)
                .bind(UPLOAD_TTL)
                .bind(LINK_UPLOAD_IDLE)
                .fetch_all(&st.db)
                .await?;
        let active = st.active_uploads.lock().unwrap().clone();
        let ids: Vec<(String,)> = expired.into_iter().filter(|(id,)| !active.contains(id)).collect();
        let list = serde_json::to_string(&ids.iter().map(|(id,)| id).collect::<Vec<_>>()).unwrap();
        sqlx::query("DELETE FROM uploads WHERE id IN (SELECT value FROM json_each(?))").bind(list).execute(&st.db).await?;
        // Batches are no longer needed once their uploads can't be finished any more
        sqlx::query("DELETE FROM upload_batch_folders WHERE created_at < ?").bind(now() - UPLOAD_TTL).execute(&st.db).await?;
        ids
    };
    for (id,) in &ids {
        let _ = tokio::fs::remove_file(upload_path(st, id)).await;
    }
    Ok(ids.len())
}

#[cfg(test)]
#[path = "upload_tests.rs"]
mod more_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[tokio::test]
    async fn upload_folder_paths_have_a_depth_limit() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        let start = |depth: usize| {
            let rel = vec!["d"; depth].join("/");
            let mut h = HeaderMap::new();
            h.insert("upload-length", "10".parse().unwrap());
            h.insert("upload-metadata", format!("filename {},parentId {},relativePath {}", b64("f.txt"), b64("root"), b64(&rel)).parse().unwrap());
            create(State(env.st.clone()), amy.clone(), h)
        };
        assert_eq!(start(MAX_REL_DEPTH).await.unwrap().status(), axum::http::StatusCode::CREATED);
        assert_eq!(start(MAX_REL_DEPTH + 1).await.unwrap_err().status, axum::http::StatusCode::BAD_REQUEST);
    }

    /// Starts an upload of `data` into Amy's root folder and returns its id
    async fn begin(env: &testutil::TestEnv, user: &User, name: &str, len: usize) -> String {
        begin_in(env, user, "root", name, len).await
    }

    async fn begin_in(env: &testutil::TestEnv, user: &User, parent: &str, name: &str, len: usize) -> String {
        begin_with(env, user, parent, name, len, "keep").await
    }

    /// Starts an upload that does `on_conflict` ("replace" or "keep") when the name is taken
    async fn begin_with(env: &testutil::TestEnv, user: &User, parent: &str, name: &str, len: usize, on_conflict: &str) -> String {
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        let mut h = HeaderMap::new();
        h.insert("upload-length", len.to_string().parse().unwrap());
        h.insert("upload-metadata", format!("filename {},parentId {},onConflict {}", b64(name), b64(parent), b64(on_conflict)).parse().unwrap());
        let res = create(State(env.st.clone()), user.clone(), h).await.unwrap();
        res.headers()[header::LOCATION].to_str().unwrap().rsplit('/').next().unwrap().to_string()
    }

    async fn send(env: &testutil::TestEnv, user: &User, id: &str, offset: usize, data: &'static [u8]) -> AppResult<Response> {
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_TYPE, "application/offset+octet-stream".parse().unwrap());
        h.insert("upload-offset", offset.to_string().parse().unwrap());
        patch(State(env.st.clone()), user.clone(), Path(id.to_string()), h, Body::from(data)).await
    }

    #[tokio::test]
    async fn an_upload_row_that_couldnt_be_removed_at_first_is_removed_later() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let id = begin(&env, &amy, "a.txt", 10).await;
        let exists = || async { sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM uploads WHERE id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap().0 == 1 };
        sqlx::query("CREATE TRIGGER keep_uploads BEFORE DELETE ON uploads BEGIN SELECT RAISE(ABORT, 'the disk is full'); END").execute(&env.st.db).await.unwrap();
        forget_upload(&env.st, &id).await;
        assert!(exists().await);
        sqlx::query("DROP TRIGGER keep_uploads").execute(&env.st.db).await.unwrap();
        for _ in 0..100 {
            if !exists().await {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!exists().await, "it no longer counts against the space's size limit");
    }

    async fn files_named(env: &testutil::TestEnv, name: &str) -> i64 {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE name LIKE ?").bind(format!("{name}%")).fetch_one(&env.st.db).await.unwrap();
        n
    }

    #[tokio::test]
    async fn a_client_that_lost_the_last_response_learns_the_upload_finished() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let id = begin(&env, &amy, "report.txt", 5).await;
        let done = send(&env, &amy, &id, 0, b"hello").await.unwrap();
        let node = done.headers()["x-node-id"].to_str().unwrap().to_string();

        // The response got lost: asking again reports the finished upload and the file it became
        let res = head(State(env.st.clone()), amy.clone(), Path(id.clone())).await.unwrap();
        assert_eq!(res.headers()["upload-offset"], "5");
        assert_eq!(res.headers()["x-node-id"].to_str().unwrap(), node);
        // Sending the last part again changes nothing
        let res = send(&env, &amy, &id, 0, b"hello").await.unwrap();
        assert_eq!(res.headers()["x-node-id"].to_str().unwrap(), node);
        assert_eq!(files_named(&env, "report").await, 1);
        // A finished upload no longer reserves space in the quota
        let (reserved,): (i64,) = sqlx::query_as("SELECT COALESCE(SUM(size), 0) FROM uploads WHERE node_id IS NULL").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(reserved, 0);
        // Nothing is left on the deletion list for the stored content
        let (pending,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pending_blob_deletes").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(pending, 0);
    }

    #[tokio::test]
    async fn an_upload_the_server_stopped_finishing_is_finished_or_started_over() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        // Everything arrived, but the file wasn't created (as if the server stopped right then)
        let id = begin(&env, &amy, "kept.txt", 5).await;
        tokio::fs::write(upload_path(&env.st, &id), b"hello").await.unwrap();
        sqlx::query("UPDATE uploads SET offset = 5 WHERE id = ?").bind(&id).execute(&env.st.db).await.unwrap();
        let res = head(State(env.st.clone()), amy.clone(), Path(id.clone())).await.unwrap();
        assert!(res.headers().contains_key("x-node-id"));
        assert_eq!(files_named(&env, "kept").await, 1);

        // The received data is gone too: the client is told to start over, not that the upload succeeded
        let id = begin(&env, &amy, "lost.txt", 5).await;
        tokio::fs::remove_file(upload_path(&env.st, &id)).await.unwrap();
        sqlx::query("UPDATE uploads SET offset = 5 WHERE id = ?").bind(&id).execute(&env.st.db).await.unwrap();
        let err = head(State(env.st.clone()), amy.clone(), Path(id.clone())).await.unwrap_err();
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(files_named(&env, "lost").await, 0);
    }

    async fn stored_hash(env: &testutil::TestEnv, node: &str) -> String {
        let (h,): (String,) = sqlx::query_as("SELECT blob_hash FROM nodes WHERE id = ?").bind(node).fetch_one(&env.st.db).await.unwrap();
        h
    }

    #[tokio::test]
    async fn uploads_are_hashed_as_they_arrive_and_resume_from_the_saved_state() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let id = begin(&env, &amy, "parts.txt", 11).await;
        send(&env, &amy, &id, 0, b"hello").await.unwrap();
        let (state, hashed): (Option<Vec<u8>>, i64) =
            sqlx::query_as("SELECT hash_state, hashed FROM uploads WHERE id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap();
        assert!(state.is_some());
        assert_eq!(hashed, 5);
        // The part received is changed on disk: the saved state is used, so the file isn't read again (a real change
        // would be found by `thirtyfile check --verify`)
        let path = upload_path(&env.st, &id);
        std::fs::write(&path, b"HELLO").unwrap();
        let done = send(&env, &amy, &id, 5, b" world").await.unwrap();
        let node = done.headers()["x-node-id"].to_str().unwrap().to_string();
        assert_eq!(stored_hash(&env, &node).await, crate::util::sha256_hex(b"hello world"));

        // Without a saved state, or with one that is behind, the received part is read once
        for stale in ["UPDATE uploads SET hash_state = NULL WHERE id = ?", "UPDATE uploads SET hashed = 0 WHERE id = ?"] {
            let id = begin(&env, &amy, "again.txt", 11).await;
            send(&env, &amy, &id, 0, b"hello").await.unwrap();
            sqlx::query(stale).bind(&id).execute(&env.st.db).await.unwrap();
            let done = send(&env, &amy, &id, 5, b" there").await.unwrap();
            let node = done.headers()["x-node-id"].to_str().unwrap().to_string();
            assert_eq!(stored_hash(&env, &node).await, crate::util::sha256_hex(b"hello there"), "{stale}");
        }

        // Data received after the offset was last saved (the server stopped in between) is dropped and not hashed
        let id = begin(&env, &amy, "cut.txt", 8).await;
        send(&env, &amy, &id, 0, b"abcd").await.unwrap();
        append(&upload_path(&env.st, &id), b"zz");
        let done = send(&env, &amy, &id, 4, b"efgh").await.unwrap();
        let node = done.headers()["x-node-id"].to_str().unwrap().to_string();
        assert_eq!(stored_hash(&env, &node).await, crate::util::sha256_hex(b"abcdefgh"));

        // The part received so far is gone: the client hears to start over
        let id = begin(&env, &amy, "gone.txt", 8).await;
        send(&env, &amy, &id, 0, b"abcd").await.unwrap();
        sqlx::query("UPDATE uploads SET hash_state = NULL WHERE id = ?").bind(&id).execute(&env.st.db).await.unwrap();
        std::fs::write(upload_path(&env.st, &id), b"").unwrap();
        let err = send(&env, &amy, &id, 4, b"efgh").await.unwrap_err();
        assert_eq!(err.code, Some("upload_discarded"));
        assert_eq!(files_named(&env, "gone").await, 0);
    }

    #[tokio::test]
    async fn an_upload_finished_after_a_restart_uses_the_saved_hash_state() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let id = begin(&env, &amy, "late.txt", 10).await;
        send(&env, &amy, &id, 0, b"first").await.unwrap();
        // Everything arrived and was saved, but the server stopped before the file was created
        let mut hasher = saved_hasher(&load(&env.st, &Uploader::signed_in(amy.clone()), &id).await.unwrap()).unwrap();
        hasher.update(b"-last");
        append(&upload_path(&env.st, &id), b"-last");
        sqlx::query("UPDATE uploads SET offset = 10, hashed = 10, hash_state = ? WHERE id = ?")
            .bind(hasher.serialize().to_vec())
            .bind(&id)
            .execute(&env.st.db)
            .await
            .unwrap();
        let res = head(State(env.st.clone()), amy.clone(), Path(id.clone())).await.unwrap();
        let node = res.headers()["x-node-id"].to_str().unwrap().to_string();
        assert_eq!(stored_hash(&env, &node).await, crate::util::sha256_hex(b"first-last"));
    }

    fn append(path: &std::path::Path, data: &[u8]) {
        let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        std::io::Write::write_all(&mut f, data).unwrap();
    }

    #[tokio::test]
    async fn finishing_continues_when_the_request_is_dropped() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let id = begin(&env, &amy, "big.txt", 5).await;
        tokio::fs::write(upload_path(&env.st, &id), b"hello").await.unwrap();
        sqlx::query("UPDATE uploads SET offset = 5 WHERE id = ?").bind(&id).execute(&env.st.db).await.unwrap();
        let up = Uploader::signed_in(amy.clone());
        let upload = load(&env.st, &up, &id).await.unwrap();
        let guard = ActiveGuard::claim(&env.st, &id).unwrap();
        // Hold the write lock, so finishing stops right before recording the file (the content is already stored),
        // and drop the request there, as a proxy closing the connection would
        let lock = env.st.write_lock.lock().await;
        let request = finish(&env.st, &up, upload, guard);
        assert!(tokio::time::timeout(std::time::Duration::from_millis(300), request).await.is_err());
        drop(lock);
        // The file still appears, and the upload can't be finished a second time meanwhile
        for _ in 0..100 {
            if files_named(&env, "big").await == 1 {
                let (node,): (Option<String>,) = sqlx::query_as("SELECT node_id FROM uploads WHERE id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap();
                assert!(node.is_some());
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("the upload was received but never finished");
    }

    #[tokio::test]
    async fn an_upload_into_a_folder_space_becomes_a_file_in_the_folder() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        for _ in 0..2 {
            let id = begin_in(&env, &admin, &space.root, "report.txt", 5).await;
            send(&env, &admin, &id, 0, b"hello").await.unwrap();
        }
        // The second one gets a free name; neither is kept in the content store
        assert_eq!(std::fs::read(space.dir.join("report.txt")).unwrap(), b"hello");
        assert_eq!(std::fs::read(space.dir.join("report (1).txt")).unwrap(), b"hello");
        assert!(env.node_at(&space.drive, "report (1).txt").await.is_some());
        let (blobs,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM blobs").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(blobs, 0);
        // Nothing left behind under a temporary name
        let leftovers: Vec<_> =
            std::fs::read_dir(&space.dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(".thirtyfile-upload")).collect();
        assert!(leftovers.is_empty());
    }

    /// Reads a file's content as the browser would
    async fn read(env: &testutil::TestEnv, user: &User, id: &str) -> Vec<u8> {
        let q = axum::extract::Query(serde_json::from_value(serde_json::json!({})).unwrap());
        let res = crate::files::content(State(env.st.clone()), user.clone(), Path(id.to_string()), q, HeaderMap::new()).await.unwrap();
        axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec()
    }

    #[tokio::test]
    async fn an_upload_replaces_a_file_with_the_same_name_or_keeps_both_and_says_so() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let id = begin(&env, &amy, "report.txt", 5).await;
        let first = send(&env, &amy, &id, 0, b"hello").await.unwrap();
        let original = first.headers()["x-node-id"].to_str().unwrap().to_string();
        assert_eq!(first.headers()["x-node-name"], "report%2Etxt");

        // Kept both: the new one gets a number, and the client hears which
        let id = begin_with(&env, &amy, "root", "Report.txt", 3, "keep").await;
        let kept = send(&env, &amy, &id, 0, b"two").await.unwrap();
        assert_eq!(kept.headers()["x-node-name"], "Report%20%281%29%2Etxt");
        assert_ne!(kept.headers()["x-node-id"], original.as_str());

        // Replaced: the same file (so its shares and permissions stay) with the new content
        let id = begin_with(&env, &amy, "root", "REPORT.txt", 6, "replace").await;
        let replaced = send(&env, &amy, &id, 0, b"world!").await.unwrap();
        assert_eq!(replaced.headers()["x-node-id"], original.as_str());
        assert_eq!(replaced.headers()["x-node-name"], "report%2Etxt");
        assert_eq!(read(&env, &amy, &original).await, b"world!");
        assert_eq!(files_named(&env, "report").await, 2);
        let (used,): (i64,) = sqlx::query_as("SELECT used_bytes FROM drives WHERE root_id = ?").bind(amy.root()).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(used, 6 + 3);
        // The old content is kept as an earlier version of the file
        let (blobs,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM blobs").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(blobs, 3);
        let (versions,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM node_versions WHERE node_id = ? AND blob_hash IS NOT NULL").bind(&original).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(versions, 1);
    }

    #[tokio::test]
    async fn an_upload_whose_folder_was_deleted_fails_instead_of_landing_elsewhere() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let folder = env.folder(&amy, amy.root(), "Projects").await;
        let id = begin_in(&env, &amy, &folder, "plan.txt", 5).await;
        let _ = crate::nodes::trash(State(env.st.clone()), amy.clone(), axum::Json(serde_json::from_value(serde_json::json!({ "ids": [folder] })).unwrap()))
            .await
            .unwrap();
        let err = send(&env, &amy, &id, 0, b"hello").await.unwrap_err();
        assert_eq!((err.status, err.code), (StatusCode::NOT_FOUND, Some("upload_discarded")));
        assert!(err.message.contains("deleted or moved to the trash"), "{}", err.message);
        assert_eq!(files_named(&env, "plan").await, 0);
        assert!(load(&env.st, &Uploader::signed_in(amy.clone()), &id).await.is_err(), "the upload is gone, not waiting to be resumed");

        // Or the permission to write there was taken away
        let ben = env.user("ben", true).await;
        let shared = env.folder(&amy, amy.root(), "Shared").await;
        env.grant(&shared, &ben, "editor").await;
        let id = begin_in(&env, &ben, &shared, "notes.txt", 5).await;
        env.revoke(&shared, &ben).await;
        let err = send(&env, &ben, &id, 0, b"hello").await.unwrap_err();
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert_eq!(files_named(&env, "notes").await, 0);
    }

    #[tokio::test]
    async fn an_upload_replaces_a_file_in_a_folder_space_in_place() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        let id = begin_in(&env, &admin, &space.root, "report.txt", 5).await;
        let first = send(&env, &admin, &id, 0, b"hello").await.unwrap();
        let original = first.headers()["x-node-id"].to_str().unwrap().to_string();
        let id = begin_with(&env, &admin, &space.root, "report.txt", 6, "replace").await;
        let replaced = send(&env, &admin, &id, 0, b"world!").await.unwrap();
        assert_eq!(replaced.headers()["x-node-id"], original.as_str());
        assert_eq!(std::fs::read(space.dir.join("report.txt")).unwrap(), b"world!");
        assert_eq!(env.node_at(&space.drive, "report.txt").await, Some((original, 6)));
        let r = crate::folders::scan(&env.st, &space.drive).await.unwrap();
        assert_eq!((r.added, r.changed, r.removed), (0, 0, 0), "the index already knows: {r:?}");
    }
}
