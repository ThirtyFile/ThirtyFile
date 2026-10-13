//! Files use the same staged writes, quota/version policy and trash as the web interface.

use axum::http::{HeaderMap, StatusCode};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{Stream, StreamExt};
use s3s::{S3Request, S3Response, S3Result, s3_error, dto::*};
use tokio_util::io::ReaderStream;
use crate::{access, state::AppState, tree::Need, util::now};
use super::credentials::{Identity, request_identity};

pub fn timestamp(seconds: i64) -> Timestamp { (std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds.max(0) as u64)).into() }

pub fn unsupported(headers: &HeaderMap) -> S3Result<()> {
    for name in headers.keys() {
        let name = name.as_str();
        if name.starts_with("x-amz-grant-") || name.starts_with("x-amz-server-side-encryption") || name.starts_with("x-amz-object-lock") || name.starts_with("x-amz-meta-") || matches!(name, "x-amz-acl" | "x-amz-tagging" | "x-amz-tagging-directive" | "x-amz-website-redirect-location") {
            return Err(s3_error!(NotImplemented, "This S3 setting is not supported"));
        }
    }
    Ok(())
}

fn etag_matches(value: &str, etag: &ETag) -> bool { value == "*" || value.split(',').any(|v| v.trim() == format!("\"{}\"", etag.value())) }

pub fn condition(headers: &HeaderMap, etag: &ETag, modified: i64, copy: bool) -> S3Result<Option<StatusCode>> {
    let text = |name| headers.get(name).and_then(|v| v.to_str().ok());
    let (match_name, none_name, unmodified, modified_name) = if copy { ("x-amz-copy-source-if-match", "x-amz-copy-source-if-none-match", "x-amz-copy-source-if-unmodified-since", "x-amz-copy-source-if-modified-since") } else { ("if-match", "if-none-match", "if-unmodified-since", "if-modified-since") };
    if let Some(v) = text(match_name) { if !etag_matches(v, etag) { return Err(s3_error!(PreconditionFailed)); } }
    else if let Some(date) = text(unmodified).and_then(|v| httpdate::parse_http_date(v).ok()) {
        if timestamp(modified) > Timestamp::from(date) { return Err(s3_error!(PreconditionFailed)); }
    }
    if let Some(v) = text(none_name) {
        if etag_matches(v, etag) { return if copy { Err(s3_error!(PreconditionFailed)) } else { Ok(Some(StatusCode::NOT_MODIFIED)) }; }
    } else if let Some(date) = text(modified_name).and_then(|v| httpdate::parse_http_date(v).ok()) {
        if timestamp(modified) <= Timestamp::from(date) { return if copy { Err(s3_error!(PreconditionFailed)) } else { Ok(Some(StatusCode::NOT_MODIFIED)) }; }
    }
    Ok(None)
}

pub async fn get(st: &AppState, req: S3Request<GetObjectInput>) -> S3Result<S3Response<GetObjectOutput>> {
    let identity = request_identity(st, &req, Some(&req.input.bucket), false).await?;
    unsupported(&req.headers)?;
    if req.input.version_id.is_some() || req.input.part_number.is_some() { return Err(s3_error!(NotImplemented)); }
    let node = access::rooted_item(st, &identity.user, &identity.root.id, &req.input.key, Need::Read).await.map_err(super::map_error)?;
    let description = super::etag::describe_node(st, &node).await?;
    let etag = description.etag.clone();
    let mut out = GetObjectOutput { e_tag: Some(etag.clone()), last_modified: Some(timestamp(node.updated_at)), accept_ranges: Some("bytes".into()), ..Default::default() };
    if let Some(status) = condition(&req.headers, &etag, node.updated_at, false)? {
        let mut response = S3Response::new(out); response.status = Some(status); return Ok(response);
    }
    let size = description.size;
    let range = match &req.input.range { Some(range) => range.check(size).map_err(|_| s3_error!(InvalidRange))?, None => 0..size };
    out.content_length = Some((range.end - range.start) as i64);
    out.content_type = Some(node.mime.parse().unwrap_or_default());
    if !node.is_folder() {
        let source = description.source.ok_or_else(|| s3_error!(NoSuchKey))?;
        out.body = Some(StreamingBlob::from(s3s::Body::http_body_unsync(axum::body::Body::from_stream(ReaderStream::with_capacity(source.open(st, range.start, range.end - range.start).await.map_err(super::internal)?, 256 * 1024)))));
    } else { out.body = Some(StreamingBlob::from_bytes(bytes::Bytes::new())); }
    let ranged = req.input.range.is_some();
    if ranged { out.content_range = Some(format!("bytes {}-{}/{size}", range.start, range.end - 1)); }
    let mut response = S3Response::new(out);
    if ranged { response.status = Some(StatusCode::PARTIAL_CONTENT); }
    // S3 payloads must not be compressed or rendered as active content on this origin.
    response.headers.insert("content-disposition", axum::http::HeaderValue::from_static("attachment"));
    response.headers.insert("x-content-type-options", axum::http::HeaderValue::from_static("nosniff"));
    Ok(response)
}

pub async fn head(st: &AppState, req: S3Request<HeadObjectInput>) -> S3Result<S3Response<HeadObjectOutput>> {
    let identity = request_identity(st, &req, Some(&req.input.bucket), false).await?;
    unsupported(&req.headers)?;
    if req.input.version_id.is_some() || req.input.part_number.is_some() { return Err(s3_error!(NotImplemented)); }
    let node = access::rooted_item(st, &identity.user, &identity.root.id, &req.input.key, Need::Read).await.map_err(super::map_error)?;
    let description = super::etag::describe_node(st, &node).await?;
    let etag = description.etag.clone();
    let mut response = S3Response::new(HeadObjectOutput { content_length: Some(description.size as i64), content_type: Some(node.mime.parse().unwrap_or_default()), e_tag: Some(etag.clone()), last_modified: Some(timestamp(node.updated_at)), accept_ranges: Some("bytes".into()), ..Default::default() });
    response.status = condition(&req.headers, &etag, node.updated_at, false)?;
    if let Some(range) = req.input.range {
        let range = range.check(description.size).map_err(|_| s3_error!(InvalidRange))?;
        response.output.content_length = Some((range.end - range.start) as i64);
        response.output.content_range = Some(format!("bytes {}-{}/{}", range.start, range.end - 1, description.size));
    }
    Ok(response)
}

pub async fn receive<S, E>(st: &AppState, identity: &Identity, parent: &crate::tree::Node, body: S, size: Option<i64>, md5: Option<&str>, headers: &HeaderMap, trailers: Option<&s3s::TrailingHeaders>, replaced: u64) -> S3Result<(super::temp::Temporary, u64, Option<String>, ETag)>
where S: Stream<Item = Result<bytes::Bytes, E>> + Unpin {
    let size = size.map(|n| u64::try_from(n).map_err(|_| s3_error!(InvalidArgument))).transpose()?;
    if let Some(size) = size { access::check_size(st, size).map_err(super::map_error)?; }
    let room = {
        let mut c = st.db.acquire().await.map_err(super::internal)?;
        crate::tree::room_left(&mut c, parent.drive(), None).await.map_err(super::map_error)?.map(|(n, drive)| ((n.max(0) as u64).saturating_add(replaced), drive))
    };
    let tmp = super::temp::Temporary::new(st, "receive");
    let sums = std::sync::Arc::new(crate::sync::Mutex::new(super::checksums::hasher(headers)));
    let sums_in_body = sums.clone();
    let body = body.inspect(move |chunk| { if let Ok(bytes) = chunk { sums_in_body.lock().update(bytes); } });
    let result = async {
        let (received, hash) = access::receive(st, body, tmp.path(), !parent.in_folder_space(), room).await.map_err(super::map_error)?;
        if size.is_some_and(|n| n != received) { return Err(s3_error!(IncompleteBody)); }
        // The active key and creator are checked after the body as well, before a staged write can start.
        super::credentials::identity(st, &identity.key.access_key, true).await?;
        let sums = std::sync::Arc::try_unwrap(sums).map_err(|_| s3_error!(InternalError))?.into_inner().finalize();
        super::checksums::verify(headers, trailers, &sums)?;
        let digest = hex::encode(STANDARD.decode(sums.checksum_md5.unwrap_or_default()).map_err(super::internal)?);
        if let Some(wanted) = md5 {
            let decoded = STANDARD.decode(wanted).map_err(|_| s3_error!(InvalidDigest))?;
            if decoded != hex::decode(&digest).map_err(super::internal)? { return Err(s3_error!(BadDigest)); }
        }
        Ok((received, hash, ETag::Strong(digest)))
    }.await;
    let (received, hash, etag) = result?;
    Ok((tmp, received, hash, etag))
}

pub async fn put(st: &AppState, mut req: S3Request<PutObjectInput>) -> S3Result<S3Response<PutObjectOutput>> {
    let identity = request_identity(st, &req, Some(&req.input.bucket), true).await?;
    unsupported(&req.headers)?;
    let (parent, name) = access::rooted_parent(st, &identity.user, &identity.root.id, &req.input.key).await.map_err(super::map_error)?;
    let up = access::upload_to(st, &identity.user, &parent, name.clone()).await;
    let replaced = if !req.input.key.ends_with('/') { up.map_err(super::map_error)?.existing.map_or(0, |n| n.size.max(0) as u64) } else { 0 };
    let body = req.input.body.take().unwrap_or_else(|| StreamingBlob::from_bytes(bytes::Bytes::new()));
    let (tmp, size, hash, etag) = receive(st, &identity, &parent, body, req.input.content_length, req.input.content_md5.as_deref(), &req.headers, req.trailing_headers.as_ref(), replaced).await?;
    let result = async {
        if req.input.key.ends_with('/') {
            if size != 0 { return Err(s3_error!(InvalidArgument, "Folder objects must be empty")); }
            if access::rooted_item(st, &identity.user, &identity.root.id, &req.input.key, Need::Read).await.is_err() {
                access::make_folder(st, &identity.user, &parent, &name).await.map_err(super::map_error)?;
            }
        } else {
            commit(st.clone(), identity.user.clone(), parent, name, tmp, size, hash, identity.root.id.clone()).await?;
        }
        Ok(S3Response::new(PutObjectOutput { e_tag: Some(etag), ..Default::default() }))
    }.await;
    result
}

pub async fn delete_key(st: &AppState, identity: &Identity, key: &str) -> S3Result<()> {
    match access::rooted_item(st, &identity.user, &identity.root.id, key, Need::Delete).await {
        Ok(node) => access::trash(st, &identity.user, &node.id).await.map_err(super::map_error),
        Err(e) if e.status == StatusCode::NOT_FOUND => Ok(()),
        Err(e) => Err(super::map_error(e)),
    }
}

pub async fn delete(st: &AppState, req: S3Request<DeleteObjectInput>) -> S3Result<S3Response<DeleteObjectOutput>> {
    let identity = request_identity(st, &req, Some(&req.input.bucket), true).await?;
    if req.input.version_id.is_some() { return Err(s3_error!(NotImplemented)); }
    delete_key(st, &identity, &req.input.key).await?;
    Ok(S3Response::new(DeleteObjectOutput::default()))
}

pub async fn delete_many(st: &AppState, req: S3Request<DeleteObjectsInput>) -> S3Result<S3Response<DeleteObjectsOutput>> {
    let identity = request_identity(st, &req, Some(&req.input.bucket), true).await?;
    if req.input.delete.objects.len() > 1000 { return Err(s3_error!(InvalidArgument)); }
    let mut deleted = Vec::new(); let mut errors = Vec::new();
    for object in req.input.delete.objects {
        let result = if object.version_id.is_some() { Err(s3_error!(NotImplemented)) } else { delete_key(st, &identity, &object.key).await };
        match result {
            Ok(()) if req.input.delete.quiet != Some(true) => deleted.push(DeletedObject { key: Some(object.key), ..Default::default() }),
            Ok(()) => (),
            Err(e) => errors.push(Error { key: Some(object.key), code: Some(e.code().as_str().into()), message: e.message().map(str::to_string), ..Default::default() }),
        }
    }
    Ok(S3Response::new(DeleteObjectsOutput { deleted: Some(deleted), errors: Some(errors), ..Default::default() }))
}

pub async fn copy(st: &AppState, req: S3Request<CopyObjectInput>) -> S3Result<S3Response<CopyObjectOutput>> {
    let identity = request_identity(st, &req, Some(&req.input.bucket), true).await?;
    unsupported(&req.headers)?;
    let CopySource::Bucket { bucket, key, version_id } = req.input.copy_source else { return Err(s3_error!(NotImplemented)); };
    if version_id.is_some() { return Err(s3_error!(NotImplemented)); }
    if bucket.as_ref() != identity.bucket.name { return Err(s3_error!(AccessDenied)); }
    let node = access::rooted_item(st, &identity.user, &identity.root.id, &key, Need::Read).await.map_err(super::map_error)?;
    if node.is_folder() { return Err(s3_error!(InvalidArgument)); }
    let description = super::etag::describe_node(st, &node).await?;
    let etag = description.etag.clone();
    condition(&req.headers, &etag, node.updated_at, true)?;
    let (parent, name) = access::rooted_parent(st, &identity.user, &identity.root.id, &req.input.key).await.map_err(super::map_error)?;
    let existing = access::upload_to(st, &identity.user, &parent, name.clone()).await.map_err(super::map_error)?;
    let replaced = existing.existing.map_or(0, |n| n.size.max(0) as u64);
    let source = description.source.ok_or_else(|| s3_error!(NoSuchKey))?;
    let body = ReaderStream::with_capacity(source.open(st, 0, description.size).await.map_err(super::internal)?, 256 * 1024);
    let (tmp, size, hash, received_etag) = receive(st, &identity, &parent, body, Some(description.size as i64), None, &HeaderMap::new(), None, replaced).await?;
    if received_etag != etag { return Err(s3_error!(PreconditionFailed)); }
    commit(st.clone(), identity.user, parent, name, tmp, size, hash, identity.root.id).await?;
    Ok(S3Response::new(CopyObjectOutput { copy_object_result: Some(CopyObjectResult { e_tag: Some(etag), last_modified: Some(timestamp(now())), ..Default::default() }), ..Default::default() }))
}

/// Once staging starts it finishes independently of the client connection, as web and WebDAV writes do.
pub async fn commit(st: AppState, user: crate::auth::User, parent: crate::tree::Node, name: String, tmp: super::temp::Temporary, size: u64, hash: Option<String>, root: String) -> S3Result<()> {
    tokio::spawn(async move {
        access::store_in_scope(st, user, parent, name, tmp.path().to_path_buf(), size, hash, &root).await.map_err(super::map_error)?;
        drop(tmp);
        Ok(())
    }).await.map_err(super::internal)?
}
