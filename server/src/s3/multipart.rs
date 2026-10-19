//! Multipart parts stay in owned temporary files. Per-upload locks make replacing parts, completing and cleanup
//! mutually exclusive; no request holds the application's write lock while receiving network content.

use super::credentials::{Identity, request_identity};
use crate::{
    access,
    state::AppState,
    util::{new_id, now},
};
use s3s::{S3Request, S3Response, S3Result, dto::*, s3_error};
use std::{
    collections::HashMap,
    sync::{Arc, OnceLock, Weak},
};
use tokio::io::AsyncWriteExt;

pub(super) fn lock(id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<crate::sync::Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS.get_or_init(Default::default).lock();
    locks.retain(|_, l| l.strong_count() > 0);
    if let Some(lock) = locks.get(id).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(id.into(), Arc::downgrade(&lock));
    lock
}

#[derive(sqlx::FromRow)]
struct Upload {
    id: String,
    bucket_id: String,
    key_id: String,
    object_key: String,
    mtime: Option<String>,
    created_at: i64,
}

#[derive(sqlx::FromRow)]
struct StoredPart {
    number: i32,
    file_id: String,
    size: i64,
    md5: String,
    created_at: i64,
}

fn path(st: &AppState, id: &str) -> std::path::PathBuf {
    st.tmp_dir().join(format!("s3-part-{id}"))
}

async fn upload(st: &AppState, identity: &Identity, id: &str, key: &str) -> S3Result<Upload> {
    let row: Upload = sqlx::query_as("SELECT * FROM s3_uploads WHERE id = ? AND created_at > ?")
        .bind(id)
        .bind(now() - 86400)
        .fetch_optional(&st.db)
        .await
        .map_err(super::internal)?
        .ok_or_else(|| s3_error!(NoSuchUpload))?;
    if row.bucket_id != identity.bucket.id || row.key_id != identity.key.id || row.object_key != key {
        return Err(s3_error!(NoSuchUpload));
    }
    Ok(row)
}

pub async fn create(st: &AppState, req: S3Request<CreateMultipartUploadInput>) -> S3Result<S3Response<CreateMultipartUploadOutput>> {
    let identity = request_identity(st, &req, Some(&req.input.bucket), true).await?;
    super::objects::unsupported(&req.headers)?;
    let mtime = super::metadata::mtime(&req.headers)?;
    if req.input.key.ends_with('/') {
        return Err(s3_error!(InvalidArgument));
    }
    access::relative_parts(&req.input.key).map_err(super::map_error)?;
    let (parent, name) = access::rooted_parent(st, &identity.user, &identity.root.id, &req.input.key).await.map_err(super::map_error)?;
    super::objects::prepare_upload(st, &identity, &parent, name).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await.map_err(super::internal)?;
    crate::tree::node_for(&mut tx, &identity.user, &parent.id, crate::tree::Need::Add).await.map_err(super::map_error)?;
    let (all, own, bucket): (i64, i64, i64) = sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(key_id=?1),0),COALESCE(SUM(bucket_id=?2),0) FROM s3_uploads")
        .bind(&identity.key.id)
        .bind(&identity.bucket.id)
        .fetch_one(&mut *tx)
        .await
        .map_err(super::internal)?;
    if all >= 512 || own >= 32 || bucket >= 128 {
        return Err(s3_error!(SlowDown, "Too many pending multipart uploads"));
    }
    let id = new_id();
    sqlx::query("INSERT INTO s3_uploads(id,bucket_id,key_id,object_key,created_at,mtime) VALUES(?,?,?,?,?,?)")
        .bind(&id)
        .bind(&identity.bucket.id)
        .bind(&identity.key.id)
        .bind(&req.input.key)
        .bind(now())
        .bind(mtime)
        .execute(&mut *tx)
        .await
        .map_err(super::internal)?;
    tx.commit().await.map_err(super::internal)?;
    Ok(S3Response::new(CreateMultipartUploadOutput { bucket: Some(req.input.bucket), key: Some(req.input.key), upload_id: Some(id), ..Default::default() }))
}

pub async fn part(st: &AppState, req: S3Request<UploadPartInput>) -> S3Result<S3Response<UploadPartOutput>> {
    tokio::spawn(part_inner(st.clone(), req)).await.map_err(super::internal)?
}

async fn part_inner(st: AppState, mut req: S3Request<UploadPartInput>) -> S3Result<S3Response<UploadPartOutput>> {
    let identity = request_identity(&st, &req, Some(&req.input.bucket), true).await?;
    super::objects::unsupported(&req.headers)?;
    if req.headers.contains_key("x-amz-meta-mtime") {
        return Err(s3_error!(NotImplemented));
    }
    if !(1..=10000).contains(&req.input.part_number) {
        return Err(s3_error!(InvalidArgument));
    }
    // Parts arrive in parallel (aws about 10 at once, rclone 4): each is received on its own, and the upload's lock
    // is taken only to record it
    upload(&st, &identity, &req.input.upload_id, &req.input.key).await?;
    let (parent, name) = access::rooted_parent(&st, &identity.user, &identity.root.id, &req.input.key).await.map_err(super::map_error)?;
    let up = super::objects::prepare_upload(&st, &identity, &parent, name).await?;
    let body = req.input.body.take().unwrap_or_else(|| StreamingBlob::from_bytes(bytes::Bytes::new()));
    let (mut tmp, size, _, etag) = super::objects::receive(
        &st,
        &identity,
        &parent,
        body,
        super::objects::Incoming {
            size: req.input.content_length,
            md5: req.input.content_md5.as_deref(),
            headers: &req.headers,
            trailers: req.trailing_headers.as_ref(),
            replaced: up.existing.as_ref().map_or(0, |n| n.size.max(0) as u64),
        },
    )
    .await?;
    let file_id = new_id();
    let file = path(&st, &file_id);
    let turn = lock(&req.input.upload_id);
    let _turn = turn.lock().await;
    let result = async {
        // Still there: completed or aborted while this part arrived, it takes no part
        upload(&st, &identity, &req.input.upload_id, &req.input.key).await?;
        let (total,): (i64,) = sqlx::query_as("SELECT COALESCE(SUM(size),0) FROM s3_parts WHERE upload_id = ? AND number != ?").bind(&req.input.upload_id).bind(req.input.part_number).fetch_one(&st.db).await.map_err(super::internal)?;
        let total = (total as u64).checked_add(size).ok_or_else(|| s3_error!(EntityTooLarge))?;
        up.fits(&st, total).await.map_err(super::map_error)?;
        tmp.rename(file.clone()).await.map_err(super::internal)?;
        // The key may have been removed while this part was received; never leave an unreferenced new part.
        super::credentials::identity(&st, &identity.key.access_key, true).await?;
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await.map_err(super::internal)?;
        crate::tree::node_for(&mut tx, &identity.user, &parent.id, crate::tree::Need::Add).await.map_err(super::map_error)?;
        let pending: (i64,) = sqlx::query_as("SELECT COALESCE(SUM(p.size),0) FROM s3_parts p JOIN s3_uploads u ON u.id=p.upload_id JOIN s3_buckets b ON b.id=u.bucket_id JOIN nodes n ON n.id=b.node_id WHERE n.drive_id=?1 AND NOT(p.upload_id=?2 AND p.number=?3)").bind(parent.drive()).bind(&req.input.upload_id).bind(req.input.part_number).fetch_one(&mut *tx).await.map_err(super::internal)?;
        if let Some((left, drive)) = crate::tree::room_left(&mut tx, parent.drive(), None).await.map_err(super::map_error)? {
            let credit = up.existing.as_ref().map_or(0, |n| n.size.max(0));
            if pending.0.checked_add(size as i64).is_none_or(|n| n > left.saturating_add(credit)) { return Err(super::map_error(crate::tree::quota_error(&drive))); }
        }
        let old: Option<(String,)> = sqlx::query_as("SELECT file_id FROM s3_parts WHERE upload_id = ? AND number = ?").bind(&req.input.upload_id).bind(req.input.part_number).fetch_optional(&mut *tx).await.map_err(super::internal)?;
        sqlx::query("INSERT INTO s3_parts(upload_id,number,file_id,size,md5,created_at) VALUES(?,?,?,?,?,?) ON CONFLICT(upload_id,number) DO UPDATE SET file_id=excluded.file_id,size=excluded.size,md5=excluded.md5,created_at=excluded.created_at")
            .bind(&req.input.upload_id).bind(req.input.part_number).bind(&file_id).bind(size as i64).bind(etag.value()).bind(now()).execute(&mut *tx).await.map_err(super::internal)?;
        tx.commit().await.map_err(super::internal)?;
        if let Some((old,)) = old { let _ = tokio::fs::remove_file(path(&st, &old)).await; }
        Ok(S3Response::new(UploadPartOutput { e_tag: Some(etag), ..Default::default() }))
    }.await;
    if result.is_ok() {
        tmp.persist();
    }
    result
}

async fn remove(st: &AppState, id: &str) -> S3Result<()> {
    let (st, id) = (st.clone(), id.to_owned());
    tokio::spawn(async move { remove_inner(&st, &id).await }).await.map_err(super::internal)?
}

async fn remove_inner(st: &AppState, id: &str) -> S3Result<()> {
    let files: Vec<(String,)> = sqlx::query_as("SELECT file_id FROM s3_parts WHERE upload_id = ?").bind(id).fetch_all(&st.db).await.map_err(super::internal)?;
    sqlx::query("DELETE FROM s3_uploads WHERE id = ?").bind(id).execute(&st.db).await.map_err(super::internal)?;
    for (file,) in files {
        let _ = tokio::fs::remove_file(path(st, &file)).await;
    }
    Ok(())
}

pub async fn abort(st: &AppState, req: S3Request<AbortMultipartUploadInput>) -> S3Result<S3Response<AbortMultipartUploadOutput>> {
    let st = st.clone();
    tokio::spawn(async move { abort_inner(st, req).await }).await.map_err(super::internal)?
}

async fn abort_inner(st: AppState, req: S3Request<AbortMultipartUploadInput>) -> S3Result<S3Response<AbortMultipartUploadOutput>> {
    let identity = request_identity(&st, &req, Some(&req.input.bucket), true).await?;
    let turn = lock(&req.input.upload_id);
    let _turn = turn.lock().await;
    upload(&st, &identity, &req.input.upload_id, &req.input.key).await?;
    remove(&st, &req.input.upload_id).await?;
    Ok(S3Response::new(AbortMultipartUploadOutput::default()))
}

pub async fn complete(st: &AppState, req: S3Request<CompleteMultipartUploadInput>) -> S3Result<S3Response<CompleteMultipartUploadOutput>> {
    tokio::spawn(complete_inner(st.clone(), req)).await.map_err(super::internal)?
}

async fn complete_inner(st: AppState, req: S3Request<CompleteMultipartUploadInput>) -> S3Result<S3Response<CompleteMultipartUploadOutput>> {
    let identity = request_identity(&st, &req, Some(&req.input.bucket), true).await?;
    super::objects::unsupported(&req.headers)?;
    let turn = lock(&req.input.upload_id);
    let _turn = turn.lock().await;
    let upload = upload(&st, &identity, &req.input.upload_id, &req.input.key).await?;
    let expect = super::objects::expected(&st, &identity, &req.input.key, &req.headers).await?;
    let selected = req.input.multipart_upload.and_then(|u| u.parts).ok_or_else(|| s3_error!(InvalidPart))?;
    if selected.is_empty() || selected.len() > 10000 {
        return Err(s3_error!(InvalidPart));
    }
    let mut parts = Vec::with_capacity(selected.len());
    let mut previous = 0;
    let mut size = 0u64;
    let selected_len = selected.len();
    for (index, selected) in selected.iter().enumerate() {
        let number = selected.part_number.ok_or_else(|| s3_error!(InvalidPart))?;
        if number <= previous {
            return Err(s3_error!(InvalidPartOrder));
        }
        previous = number;
        let part: StoredPart = sqlx::query_as("SELECT * FROM s3_parts WHERE upload_id = ? AND number = ?")
            .bind(&req.input.upload_id)
            .bind(number)
            .fetch_optional(&st.db)
            .await
            .map_err(super::internal)?
            .ok_or_else(|| s3_error!(InvalidPart))?;
        if selected.e_tag.as_ref().map(ETag::value) != Some(part.md5.as_str()) {
            return Err(s3_error!(InvalidPart));
        }
        if index + 1 != selected_len && part.size < 5 * 1024 * 1024 {
            return Err(s3_error!(EntityTooSmall));
        }
        size = size.checked_add(part.size as u64).ok_or_else(|| s3_error!(EntityTooLarge))?;
        parts.push(part);
    }
    access::check_size(&st, size).map_err(super::map_error)?;
    let (parent, name) = access::rooted_parent(&st, &identity.user, &identity.root.id, &req.input.key).await.map_err(super::map_error)?;
    let up = super::objects::prepare_upload(&st, &identity, &parent, name.clone()).await?;
    up.fits(&st, size).await.map_err(super::map_error)?;
    let assembled = super::temp::Temporary::new(&st, "complete");

    async {
        let mut disk = crate::util::DiskRoom::check(&st.tmp_dir(), size).await.map_err(super::map_error)?;
        let mut out = tokio::fs::File::create(assembled.path()).await.map_err(super::internal)?;
        // The object's MD5 (its ETag) and SHA-256 (how the content store names it), worked out while the parts are
        // copied: a large object is read once, not again for each
        let (mut md5, mut sha) = (<md5::Md5 as md5::Digest>::new(), <sha2::Sha256 as sha2::Digest>::new());
        let mut buf = vec![0u8; 1024 * 1024];
        for part in &parts {
            disk.before(part.size as u64).await.map_err(super::map_error)?;
            let mut source = tokio::fs::File::open(path(&st, &part.file_id)).await.map_err(super::internal)?;
            let mut copied = 0u64;
            loop {
                let n = tokio::io::AsyncReadExt::read(&mut source, &mut buf).await.map_err(super::internal)?;
                if n == 0 {
                    break;
                }
                md5::Digest::update(&mut md5, &buf[..n]);
                sha2::Digest::update(&mut sha, &buf[..n]);
                out.write_all(&buf[..n]).await.map_err(super::internal)?;
                copied += n as u64;
            }
            if copied != part.size as u64 {
                return Err(s3_error!(InvalidPart));
            }
        }
        out.flush().await.map_err(super::internal)?;
        out.sync_data().await.map_err(super::internal)?;
        drop(out);
        let md5 = hex::encode(md5::Digest::finalize(md5));
        let etag = ETag::Strong(md5.clone());
        let latest = super::credentials::identity(&st, &identity.key.access_key, true).await?;
        super::objects::commit(
            st.clone(),
            latest.user,
            parent,
            name,
            (assembled, size, Some(hex::encode(sha2::Digest::finalize(sha))), Some(md5)),
            access::ScopedWrite { root: identity.root.id.clone(), mtime: upload.mtime, expect },
        )
        .await?;
        remove(&st, &req.input.upload_id).await?;
        Ok(S3Response::new(CompleteMultipartUploadOutput { bucket: Some(req.input.bucket), key: Some(req.input.key), e_tag: Some(etag), ..Default::default() }))
    }
    .await
}

pub async fn list_parts(st: &AppState, req: S3Request<ListPartsInput>) -> S3Result<S3Response<ListPartsOutput>> {
    let identity = request_identity(st, &req, Some(&req.input.bucket), false).await?;
    let turn = lock(&req.input.upload_id);
    let _turn = turn.lock().await;
    upload(st, &identity, &req.input.upload_id, &req.input.key).await?;
    // As AWS does: more than 1000 at once is 1000
    let limit = req.input.max_parts.unwrap_or(1000).min(1000);
    if limit < 0 {
        return Err(s3_error!(InvalidArgument));
    }
    let marker = req.input.part_number_marker.unwrap_or(0);
    let mut rows: Vec<StoredPart> = sqlx::query_as("SELECT * FROM s3_parts WHERE upload_id = ? AND number > ? ORDER BY number LIMIT ?")
        .bind(&req.input.upload_id)
        .bind(marker)
        .bind(limit + 1)
        .fetch_all(&st.db)
        .await
        .map_err(super::internal)?;
    let truncated = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let next = rows.last().map(|p| p.number);
    let parts = rows
        .into_iter()
        .map(|p| Part {
            part_number: Some(p.number),
            size: Some(p.size),
            e_tag: Some(ETag::Strong(p.md5)),
            last_modified: Some(super::objects::timestamp(p.created_at)),
            ..Default::default()
        })
        .collect();
    Ok(S3Response::new(ListPartsOutput {
        bucket: Some(req.input.bucket),
        key: Some(req.input.key),
        upload_id: Some(req.input.upload_id),
        max_parts: Some(limit),
        part_number_marker: Some(marker),
        next_part_number_marker: next.filter(|_| truncated),
        is_truncated: Some(truncated),
        parts: Some(parts),
        ..Default::default()
    }))
}

pub async fn list_uploads(st: &AppState, req: S3Request<ListMultipartUploadsInput>) -> S3Result<S3Response<ListMultipartUploadsOutput>> {
    let identity = request_identity(st, &req, Some(&req.input.bucket), false).await?;
    // As AWS does: more than 1000 at once is 1000
    let limit = req.input.max_uploads.unwrap_or(1000).min(1000);
    if limit < 0 {
        return Err(s3_error!(InvalidArgument));
    }
    let prefix = req.input.prefix.as_deref().unwrap_or_default();
    let delimiter = req.input.delimiter.as_deref().unwrap_or_default();
    let marker = req.input.key_marker.as_deref().unwrap_or_default();
    let upload_marker = req.input.upload_id_marker.as_deref().unwrap_or_default();
    if prefix.len() > 1024 || delimiter.len() > 64 || marker.len() > 1024 {
        return Err(s3_error!(InvalidArgument));
    }
    // There can be at most 128 pending uploads in this bucket, regardless of key count.
    let rows: Vec<Upload> = sqlx::query_as("SELECT * FROM s3_uploads WHERE bucket_id=?1 AND key_id=?2 AND created_at>?3 AND substr(object_key,1,length(?4))=?4 AND (object_key COLLATE BINARY>?5 OR (object_key=?5 AND id>?6)) ORDER BY object_key COLLATE BINARY,id LIMIT 129")
        .bind(&identity.bucket.id).bind(&identity.key.id).bind(now()-86400).bind(prefix).bind(marker).bind(upload_marker).fetch_all(&st.db).await.map_err(super::internal)?;
    let mut uploads = Vec::new();
    let mut groups = Vec::new();
    let mut previous = String::new();
    let mut next = None;
    let mut truncated = false;
    for row in rows {
        let group = if delimiter.is_empty() {
            None
        } else {
            row.object_key[prefix.len()..].find(delimiter).map(|n| row.object_key[..prefix.len() + n + delimiter.len()].to_string())
        };
        if let Some(group) = &group {
            if *group == previous || group.as_bytes() <= marker.as_bytes() {
                continue;
            }
            previous = group.clone();
        }
        if uploads.len() + groups.len() == limit as usize {
            truncated = true;
            break;
        }
        match group {
            Some(group) => {
                next = Some((group.clone(), String::new()));
                groups.push(CommonPrefix { prefix: Some(group) });
            }
            None => {
                next = Some((row.object_key.clone(), row.id.clone()));
                uploads.push(MultipartUpload {
                    key: Some(row.object_key),
                    upload_id: Some(row.id),
                    initiated: Some(super::objects::timestamp(row.created_at)),
                    ..Default::default()
                });
            }
        }
    }
    let next = next.filter(|_| truncated);
    Ok(S3Response::new(ListMultipartUploadsOutput {
        bucket: Some(req.input.bucket),
        uploads: Some(uploads),
        common_prefixes: Some(groups),
        prefix: req.input.prefix,
        delimiter: req.input.delimiter,
        max_uploads: Some(limit),
        key_marker: req.input.key_marker,
        upload_id_marker: req.input.upload_id_marker,
        next_key_marker: next.as_ref().map(|p| p.0.clone()),
        next_upload_id_marker: next.map(|p| p.1),
        is_truncated: Some(truncated),
        ..Default::default()
    }))
}

pub async fn cleanup(st: &AppState) -> S3Result<()> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT u.id,k.access_key FROM s3_uploads u JOIN s3_keys k ON k.id=u.key_id").fetch_all(&st.db).await.map_err(super::internal)?;
    for (id, key) in rows {
        let turn = lock(&id);
        let Ok(_turn) = turn.try_lock() else { continue };
        let (expired,): (bool,) = sqlx::query_as("SELECT COALESCE(MAX(created_at <= ?),1) FROM s3_uploads WHERE id=?")
            .bind(now() - 86400)
            .bind(&id)
            .fetch_one(&st.db)
            .await
            .map_err(super::internal)?;
        // Its key stopped, ended or no longer allowed to write: a passing database error isn't that
        let refused = matches!(super::credentials::identity(st, &key, true).await, Err(e) if *e.code() != s3s::S3ErrorCode::InternalError);
        if expired || refused {
            remove(st, &id).await?;
        }
    }
    drain_gc(st).await
}

pub fn cleanup_later(st: AppState) {
    tokio::spawn(async move {
        if let Err(e) = cleanup(&st).await {
            tracing::warn!("S3 multipart cleanup failed: {e}");
        }
    });
}

/// Removing the file first makes retries safe even if the cleanup future is cancelled at its database await.
pub async fn drain_gc(st: &AppState) -> S3Result<()> {
    let files: Vec<(String,)> = sqlx::query_as("SELECT file_id FROM s3_part_gc LIMIT 1024").fetch_all(&st.db).await.map_err(super::internal)?;
    for (file,) in files {
        match tokio::fs::remove_file(path(st, &file)).await {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => {
                tracing::warn!("S3 temporary file cleanup failed: {e}");
                continue;
            }
        }
        sqlx::query("DELETE FROM s3_part_gc WHERE file_id=?").bind(file).execute(&st.db).await.map_err(super::internal)?;
    }
    Ok(())
}
