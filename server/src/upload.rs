//! tus 1.0 resumable uploads (core + creation + termination).
//! The frontend uses tus-js-client; after an interruption (including a browser refresh) the upload resumes where it left off.

use std::{io::SeekFrom, path::PathBuf};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::Engine;
use futures_util::StreamExt;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use crate::{
    auth::User,
    error::{AppError, AppResult},
    files::hash_file,
    state::AppState,
    tree,
    util::{guess_mime, new_id, now, validate_name},
};

const TUS_VERSION: &str = "1.0.0";
pub const UPLOAD_TTL: i64 = 7 * 86400;
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
}

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
            let value = it
                .next()
                .and_then(|v| base64::engine::general_purpose::STANDARD.decode(v.trim()).ok())
                .and_then(|b| String::from_utf8(b).ok())
                .unwrap_or_default();
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
    let size = header_u64(&headers, "upload-length").ok_or_else(|| AppError::bad_request("Missing Upload-Length"))?;
    // Sizes are stored as i64 and summed for quotas: refuse absurd values before they can overflow (1 PiB is far beyond any single file)
    if size > MAX_UPLOAD_LENGTH {
        return Err(AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "Invalid Upload-Length"));
    }
    if st.max_upload > 0 && size > st.max_upload {
        return Err(AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "The file exceeds the upload size limit"));
    }
    let meta = parse_metadata(&headers);
    let name = validate_name(meta.get("filename").map(String::as_str).unwrap_or_default())?;
    let parent_id = meta.get("parentId").cloned().unwrap_or_else(|| "root".into());
    let mut rel_parts = Vec::new();
    for part in meta.get("relativePath").map(String::as_str).unwrap_or_default().split('/').filter(|p| !p.is_empty()) {
        if rel_parts.len() >= MAX_REL_DEPTH {
            return Err(AppError::bad_request("The folder path is too deep"));
        }
        rel_parts.push(validate_name(part)?);
    }
    let rel_path = rel_parts.join("/");
    let batch = meta
        .get("batchId")
        .filter(|b| (1..=64).contains(&b.len()) && b.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .cloned()
        .unwrap_or_default();

    let id = new_id();
    {
        let _w = st.write_lock.lock().await;
        let mut tx = st.db.begin().await?;
        let parent = tree::folder_for(&mut tx, &user, &parent_id, tree::Need::Write).await?;
        tree::check_quota(&mut tx, parent.drive(), size as i64).await?;
        let ts = now();
        sqlx::query(
            "INSERT INTO uploads (id, owner_id, parent_id, rel_path, name, size, offset, created_at, expires_at, drive_id, batch)
             VALUES (?, ?, ?, ?, ?, ?, 0, ?, ?, ?, ?)",
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
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
    }
    // Create the temp file only after the database record succeeds; if creation fails, undo the record, leaving neither uploads without a file nor unrecorded temp files
    if let Err(e) = tokio::fs::File::create(upload_path(&st, &id)).await {
        let _w = st.write_lock.lock().await;
        let _ = sqlx::query("DELETE FROM uploads WHERE id = ?").bind(&id).execute(&st.db).await;
        return Err(e.into());
    }

    let mut res = StatusCode::CREATED.into_response();
    if size == 0 {
        let upload = load(&st, &user, &id).await?;
        let guard = ActiveGuard::claim(&st, &id).ok_or_else(|| AppError::new(StatusCode::LOCKED, "This file is already being uploaded"))?;
        let node_id = finish(&st, &user, upload, guard).await?;
        res.headers_mut().insert("x-node-id", HeaderValue::from_str(&node_id).unwrap());
    }
    tus(&mut res);
    res.headers_mut().insert(header::LOCATION, HeaderValue::from_str(&format!("/api/uploads/{id}")).unwrap());
    Ok(res)
}

async fn load(st: &AppState, user: &User, id: &str) -> AppResult<Upload> {
    sqlx::query_as("SELECT id, owner_id, parent_id, rel_path, name, size, offset, drive_id, batch, node_id FROM uploads WHERE id = ? AND owner_id = ?")
        .bind(id)
        .bind(user.id)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("The upload doesn't exist or has expired"))
}

pub async fn head(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<Response> {
    let upload = load(&st, &user, &id).await?;
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
        node_id = Some(finish(&st, &user, upload, guard).await?);
    }
    let mut res = StatusCode::OK.into_response();
    tus(&mut res);
    let h = res.headers_mut();
    // A finished upload reports everything as received, whatever the row said before finishing
    h.insert("upload-offset", if node_id.is_some() { size } else { offset }.into());
    h.insert("upload-length", size.into());
    if let Some(n) = &node_id {
        h.insert("x-node-id", HeaderValue::from_str(n).unwrap());
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
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

pub async fn patch(
    State(st): State<AppState>,
    user: User,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> AppResult<Response> {
    if headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()) != Some("application/offset+octet-stream") {
        return Err(AppError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Content-Type must be application/offset+octet-stream"));
    }
    // Ownership first, so other users can't mark someone else's upload as active
    load(&st, &user, &id).await?;
    let guard = ActiveGuard::claim(&st, &id).ok_or_else(|| AppError::new(StatusCode::LOCKED, "This file is already being uploaded"))?;
    // Read the offset only while holding the guard: a retried request must not work from the offset before the
    // previous request finished
    let upload = load(&st, &user, &id).await?;
    if let Some(node_id) = &upload.node_id {
        let mut res = StatusCode::NO_CONTENT.into_response();
        tus(&mut res);
        res.headers_mut().insert("upload-offset", upload.size.into());
        res.headers_mut().insert("x-node-id", HeaderValue::from_str(node_id).unwrap());
        return Ok(res);
    }
    // The deadline moves when data starts arriving, so the hourly cleanup can't remove an upload that is being received
    {
        let _w = st.write_lock.lock().await;
        sqlx::query("UPDATE uploads SET expires_at = ? WHERE id = ?").bind(now() + UPLOAD_TTL).bind(&id).execute(&st.db).await?;
    }

    let client_offset = header_u64(&headers, "upload-offset").ok_or_else(|| AppError::bad_request("Missing Upload-Offset"))?;
    if client_offset != upload.offset as u64 {
        return Err(AppError::conflict("Upload-Offset mismatch"));
    }

    let size = upload.size as u64;
    let mut offset = upload.offset as u64;
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
        // Progress extends the deadline: a large upload that keeps resuming isn't discarded after 7 days
        sqlx::query("UPDATE uploads SET offset = ?, expires_at = ? WHERE id = ?")
            .bind(offset as i64)
            .bind(now() + UPLOAD_TTL)
            .bind(&id)
            .execute(&st.db)
            .await?;
    }
    if let Some(e) = failure {
        return Err(e);
    }

    let mut res = StatusCode::NO_CONTENT.into_response();
    if offset == size {
        let upload = load(&st, &user, &id).await?;
        let node_id = finish(&st, &user, upload, guard).await?;
        res.headers_mut().insert("x-node-id", HeaderValue::from_str(&node_id).unwrap());
    }
    tus(&mut res);
    res.headers_mut().insert("upload-offset", offset.into());
    Ok(res)
}

/// Finishes an upload in a task of its own: storing the content and creating the file must not stop halfway when the
/// client or a proxy drops the request (which would leave content in storage without a file, or a file the client
/// never hears about). The guard stays held until the task is done.
async fn finish(st: &AppState, user: &User, upload: Upload, guard: ActiveGuard) -> AppResult<String> {
    let (st, user) = (st.clone(), user.clone());
    tokio::spawn(async move {
        let result = finalize(&st, &user, upload).await;
        drop(guard);
        result
    })
    .await
    .map_err(AppError::internal)?
}

/// Upload finished: compute the hash, put it in storage, create the file node
async fn finalize(st: &AppState, user: &User, upload: Upload) -> AppResult<String> {
    let path = upload_path(st, &upload.id);
    if let Some(id) = finalize_in_folder(st, user, &upload, &path).await? {
        return Ok(id);
    }
    let (hash, size) = hash_file(path.clone()).await?;
    if size != upload.size as u64 {
        return Err(AppError::bad_request("File size mismatch"));
    }
    // First store it in the space's storage location (S3 may take a while, so don't hold the write lock)
    let staged = tree::stage_blob(st, upload.drive_id.as_deref().unwrap_or_default(), hash.clone(), size as i64, path.clone()).await?;
    let _w = st.write_lock.lock().await;
    match commit_upload(st, user, &upload, &staged, &hash, size).await {
        Ok((id, extra)) => {
            tree::finish_staged(st, staged, extra).await;
            Ok(id)
        }
        Err(e) => {
            // Transaction failed: discard the staging; the content just uploaded to the storage location is left for background cleanup so no unreferenced files remain.
            // The temporary file is gone by now, so the upload can't be resumed: drop its row (it would otherwise count against the quota for 7 days)
            tree::abandon_staged(st, staged).await;
            let _ = sqlx::query("DELETE FROM uploads WHERE id = ?").bind(&upload.id).execute(&st.db).await;
            Err(AppError::new(e.status, format!("{} The upload was discarded; start it again.", e.message)).with_code("upload_discarded"))
        }
    }
}

/// Upload into a folder space: the file goes into the folder (under a name scans ignore, then renamed into place) and
/// is indexed; its content isn't hashed. None when the target folder isn't in a folder space any more (the upload then
/// goes where uploads go when their folder is gone).
async fn finalize_in_folder(st: &AppState, user: &User, upload: &Upload, path: &std::path::Path) -> AppResult<Option<String>> {
    let parent = tree::get_node(&mut *st.db.acquire().await?, &upload.parent_id).await?;
    let Some(parent) = parent.filter(|p| p.in_folder_space() && p.is_folder() && p.trashed_at.is_none()) else { return Ok(None) };
    let size = tokio::fs::metadata(path).await?.len();
    if size != upload.size as u64 {
        return Err(AppError::bad_request("File size mismatch"));
    }
    let discarded = |e: AppError| AppError::new(e.status, format!("{} The upload was discarded; start it again.", e.message)).with_code("upload_discarded");
    let staged = match crate::fsops::stage_upload(&parent, path, size).await {
        Ok(s) => s,
        Err(e) => {
            let _w = st.write_lock.lock().await;
            let _ = sqlx::query("DELETE FROM uploads WHERE id = ?").bind(&upload.id).execute(&st.db).await;
            return Err(discarded(e));
        }
    };
    let _space = crate::fsops::lock_space(parent.drive()).await;
    let _w = st.write_lock.lock().await;
    let result = async {
        let mut tx = st.db.begin().await?;
        // The account's upload permission, or the folder, may have changed while the upload was running
        if !user.can_write && !user.is_admin() {
            return Err(AppError::forbidden("You no longer have permission to upload files"));
        }
        let target = tree::folder_for(&mut tx, user, &upload.parent_id, tree::Need::Write).await?;
        if target.drive() != parent.drive() {
            return Err(AppError::conflict("Something changed at the same time. Try again."));
        }
        let folder_id = tree::ensure_folders(&mut tx, upload.owner_id, &target.id, &upload.rel_path, &upload.batch).await?;
        let folder = tree::get_node(&mut tx, &folder_id).await?.ok_or_else(|| AppError::not_found("Folder not found"))?;
        let name = crate::fsops::free_name(&mut tx, &folder, &upload.name, false).await?;
        let id = crate::fsops::place_file(&mut tx, &staged, upload.owner_id, &folder, &name).await?;
        let ts = now();
        sqlx::query("UPDATE uploads SET node_id = ?, offset = size, expires_at = ? WHERE id = ?")
            .bind(&id)
            .bind(ts + FINISHED_TTL)
            .bind(&upload.id)
            .execute(&mut *tx)
            .await?;
        tree::touch(&mut tx, &folder.id).await?;
        if let Some(n) = tree::get_node(&mut tx, &id).await? {
            tree::adjust_usage(&mut tx, n.drive(), n.size).await?;
            tree::log(&mut tx, user, Some(&n), "upload", "").await?;
        }
        tx.commit().await?;
        Ok(id)
    }
    .await;
    match result {
        Ok(id) => Ok(Some(id)),
        Err(e) => {
            // Renamed into place already when only the index failed: the next scan shows it
            let _ = tokio::fs::remove_file(&staged).await;
            let _ = sqlx::query("DELETE FROM uploads WHERE id = ?").bind(&upload.id).execute(&st.db).await;
            Err(discarded(e))
        }
    }
}

/// Creates the node and records the content reference while holding the write lock
async fn commit_upload(
    st: &AppState,
    user: &User,
    upload: &Upload,
    staged: &tree::StagedBlob,
    hash: &str,
    size: u64,
) -> AppResult<(String, Option<tree::BlobRef>)> {
    let mut tx = st.db.begin().await?;
    // The account's upload permission may have been removed while the upload was running
    if !user.can_write && !user.is_admin() {
        return Err(AppError::forbidden("You no longer have permission to upload files"));
    }
    // The target folder may have been deleted during the upload; in that case put it in the root folder
    let parent_id = match tree::get_node(&mut tx, &upload.parent_id).await? {
        Some(p) if p.trashed_at.is_none() && p.is_folder() => {
            let role = tree::role_on(&mut tx, user, &p).await?;
            match role {
                Some(r) if tree::allows(user, r, tree::Need::Write).is_ok() => p.id,
                _ => user.root_id.clone(),
            }
        }
        _ => user.root_id.clone(),
    };
    if parent_id == user.root_id {
        // Fell back to the personal space: the quota was checked against the original space, so check the personal one now
        let personal = tree::get_node(&mut tx, &user.root_id).await?.and_then(|n| n.drive_id).unwrap_or_default();
        if upload.drive_id.as_deref() != Some(personal.as_str()) {
            tree::check_quota(&mut tx, &personal, size as i64).await?;
        }
    }
    if tree::get_node(&mut tx, &parent_id).await?.is_some_and(|p| p.in_folder_space()) {
        return Err(AppError::conflict("Something changed at the same time. Try again."));
    }
    let folder = tree::ensure_folders(&mut tx, upload.owner_id, &parent_id, &upload.rel_path, &upload.batch).await?;
    let name = tree::unique_name(&mut tx, &folder, &upload.name, false).await?;
    let extra = tree::commit_blob(st, &mut tx, staged).await?;
    let id = new_id();
    let ts = now();
    sqlx::query(
        "INSERT INTO nodes (id, owner_id, parent_id, kind, name, blob_hash, size, mime, drive_id, created_at, updated_at)
         SELECT ?1, ?2, ?3, 'file', ?4, ?5, ?6, ?7, drive_id, ?8, ?8 FROM nodes WHERE id = ?3",
    )
    .bind(&id)
    .bind(upload.owner_id)
    .bind(&folder)
    .bind(&name)
    .bind(hash)
    .bind(size as i64)
    .bind(guess_mime(&name))
    .bind(ts)
    .execute(&mut *tx)
    .await?;
    // Kept for a day with the new file, for clients that lost the response
    sqlx::query("UPDATE uploads SET node_id = ?, offset = size, expires_at = ? WHERE id = ?")
        .bind(&id)
        .bind(ts + FINISHED_TTL)
        .bind(&upload.id)
        .execute(&mut *tx)
        .await?;
    tree::touch(&mut tx, &folder).await?;
    if let Some(n) = tree::get_node(&mut tx, &id).await? {
        tree::adjust_usage(&mut tx, n.drive(), size as i64).await?;
        tree::log(&mut tx, user, Some(&n), "upload", "").await?;
    }
    tx.commit().await?;
    Ok((id, extra))
}

pub async fn delete(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<Response> {
    let upload = load(&st, &user, &id).await?;
    {
        let _w = st.write_lock.lock().await;
        sqlx::query("DELETE FROM uploads WHERE id = ?").bind(&upload.id).execute(&st.db).await?;
    }
    let _ = tokio::fs::remove_file(upload_path(&st, &upload.id)).await;
    let mut res = StatusCode::NO_CONTENT.into_response();
    tus(&mut res);
    Ok(res)
}

/// Cleans up expired unfinished uploads
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
    let known: std::collections::HashSet<String> = sqlx::query_as::<_, (String,)>("SELECT id FROM uploads")
        .fetch_all(&st.db)
        .await?
        .into_iter()
        .map(|(id,)| format!("upload-{id}"))
        .collect();
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

pub async fn purge_expired(st: &AppState) -> AppResult<usize> {
    let ids: Vec<(String,)> = {
        let _w = st.write_lock.lock().await;
        let ids = sqlx::query_as("DELETE FROM uploads WHERE expires_at < ? RETURNING id").bind(now()).fetch_all(&st.db).await?;
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
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        let mut h = HeaderMap::new();
        h.insert("upload-length", len.to_string().parse().unwrap());
        h.insert("upload-metadata", format!("filename {},parentId {}", b64(name), b64(parent)).parse().unwrap());
        let res = create(State(env.st.clone()), user.clone(), h).await.unwrap();
        res.headers()[header::LOCATION].to_str().unwrap().rsplit('/').next().unwrap().to_string()
    }

    async fn send(env: &testutil::TestEnv, user: &User, id: &str, offset: usize, data: &'static [u8]) -> AppResult<Response> {
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_TYPE, "application/offset+octet-stream".parse().unwrap());
        h.insert("upload-offset", offset.to_string().parse().unwrap());
        patch(State(env.st.clone()), user.clone(), Path(id.to_string()), h, Body::from(data)).await
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

    #[tokio::test]
    async fn finishing_continues_when_the_request_is_dropped() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let id = begin(&env, &amy, "big.txt", 5).await;
        tokio::fs::write(upload_path(&env.st, &id), b"hello").await.unwrap();
        sqlx::query("UPDATE uploads SET offset = 5 WHERE id = ?").bind(&id).execute(&env.st.db).await.unwrap();
        let upload = load(&env.st, &amy, &id).await.unwrap();
        let guard = ActiveGuard::claim(&env.st, &id).unwrap();
        // Hold the write lock, so finishing stops right before recording the file (the content is already stored),
        // and drop the request there, as a proxy closing the connection would
        let lock = env.st.write_lock.lock().await;
        let request = finish(&env.st, &amy, upload, guard);
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
        let leftovers: Vec<_> = std::fs::read_dir(&space.dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(".thirtyfile-upload")).collect();
        assert!(leftovers.is_empty());
    }
}
