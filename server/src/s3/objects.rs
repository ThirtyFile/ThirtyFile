//! Files use the same staged writes, quota/version policy and trash as the web interface.

use super::credentials::{Identity, request_identity};
use crate::{access, state::AppState, tree::Need, util::now};
use axum::http::{HeaderMap, StatusCode};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{Stream, StreamExt};
use s3s::{S3Request, S3Response, S3Result, dto::*, s3_error};
use tokio_util::io::ReaderStream;

pub fn timestamp(seconds: i64) -> Timestamp {
    (std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds.max(0) as u64)).into()
}

/// Refuses settings a write can't keep. Custom metadata other than `mtime` is accepted and not kept, as clients add it
/// on their own (rclone's `md5chksum` on large uploads): refusing it failed their uploads.
pub fn unsupported(headers: &HeaderMap) -> S3Result<()> {
    for name in headers.keys() {
        let name = name.as_str();
        if name.starts_with("x-amz-grant-")
            || name.starts_with("x-amz-server-side-encryption")
            || name.starts_with("x-amz-object-lock")
            || matches!(name, "x-amz-acl" | "x-amz-tagging" | "x-amz-tagging-directive" | "x-amz-website-redirect-location")
        {
            return Err(s3_error!(NotImplemented, "This S3 setting is not supported"));
        }
    }
    Ok(())
}

fn etag_matches(value: &str, etag: &ETag) -> bool {
    value == "*" || value.split(',').any(|v| v.trim() == format!("\"{}\"", etag.value()))
}

pub fn condition(headers: &HeaderMap, etag: &ETag, modified: i64, copy: bool) -> S3Result<Option<StatusCode>> {
    let text = |name| headers.get(name).and_then(|v| v.to_str().ok());
    let (match_name, none_name, unmodified, modified_name) = if copy {
        ("x-amz-copy-source-if-match", "x-amz-copy-source-if-none-match", "x-amz-copy-source-if-unmodified-since", "x-amz-copy-source-if-modified-since")
    } else {
        ("if-match", "if-none-match", "if-unmodified-since", "if-modified-since")
    };
    if let Some(v) = text(match_name) {
        if !etag_matches(v, etag) {
            return Err(s3_error!(PreconditionFailed));
        }
    } else if let Some(date) = text(unmodified).and_then(|v| httpdate::parse_http_date(v).ok())
        && timestamp(modified) > Timestamp::from(date)
    {
        return Err(s3_error!(PreconditionFailed));
    }
    if let Some(v) = text(none_name) {
        if etag_matches(v, etag) {
            return if copy { Err(s3_error!(PreconditionFailed)) } else { Ok(Some(StatusCode::NOT_MODIFIED)) };
        }
    } else if let Some(date) = text(modified_name).and_then(|v| httpdate::parse_http_date(v).ok())
        && timestamp(modified) <= Timestamp::from(date)
    {
        return if copy { Err(s3_error!(PreconditionFailed)) } else { Ok(Some(StatusCode::NOT_MODIFIED)) };
    }
    Ok(None)
}

pub async fn get(st: &AppState, req: S3Request<GetObjectInput>) -> S3Result<S3Response<GetObjectOutput>> {
    let identity = request_identity(st, &req, Some(&req.input.bucket), false).await?;
    unsupported(&req.headers)?;
    if req.input.version_id.is_some() || req.input.part_number.is_some() {
        return Err(s3_error!(NotImplemented));
    }
    let node = access::rooted_item(st, &identity.user, &identity.root.id, &req.input.key, Need::Read).await.map_err(super::map_error)?;
    let description = super::etag::describe_node(st, &node).await?;
    let etag = description.etag.clone();
    let mut out =
        GetObjectOutput { e_tag: Some(etag.clone()), last_modified: Some(timestamp(node.updated_at)), accept_ranges: Some("bytes".into()), ..Default::default() };
    out.metadata = super::metadata::response(description.mtime);
    if condition(&req.headers, &etag, node.updated_at, false)?.is_some() {
        return Err(not_modified(&etag, node.updated_at));
    }
    let size = description.size;
    let range = match &req.input.range {
        Some(range) => range.check(size).map_err(|_| s3_error!(InvalidRange))?,
        None => 0..size,
    };
    out.content_length = Some((range.end - range.start) as i64);
    out.content_type = Some(node.mime.parse().unwrap_or_default());
    if !node.is_folder() {
        let source = description.source.ok_or_else(|| s3_error!(NoSuchKey))?;
        out.body = Some(StreamingBlob::from(s3s::Body::http_body_unsync(axum::body::Body::from_stream(ReaderStream::with_capacity(
            source.open(st, range.start, range.end - range.start).await.map_err(super::internal)?,
            256 * 1024,
        )))));
    } else {
        out.body = Some(StreamingBlob::from_bytes(bytes::Bytes::new()));
    }
    let ranged = req.input.range.is_some();
    if ranged {
        out.content_range = Some(format!("bytes {}-{}/{size}", range.start, range.end - 1));
    }
    let mut response = S3Response::new(out);
    if ranged {
        response.status = Some(StatusCode::PARTIAL_CONTENT);
    }
    // S3 payloads must not be compressed or rendered as active content on this origin.
    response.headers.insert("content-disposition", axum::http::HeaderValue::from_static("attachment"));
    response.headers.insert("x-content-type-options", axum::http::HeaderValue::from_static("nosniff"));
    Ok(response)
}

pub async fn head(st: &AppState, req: S3Request<HeadObjectInput>) -> S3Result<S3Response<HeadObjectOutput>> {
    let identity = request_identity(st, &req, Some(&req.input.bucket), false).await?;
    unsupported(&req.headers)?;
    if req.input.version_id.is_some() || req.input.part_number.is_some() {
        return Err(s3_error!(NotImplemented));
    }
    let node = access::rooted_item(st, &identity.user, &identity.root.id, &req.input.key, Need::Read).await.map_err(super::map_error)?;
    let description = super::etag::describe_node(st, &node).await?;
    let etag = description.etag.clone();
    let mut response = S3Response::new(HeadObjectOutput {
        content_length: Some(description.size as i64),
        content_type: Some(node.mime.parse().unwrap_or_default()),
        e_tag: Some(etag.clone()),
        last_modified: Some(timestamp(node.updated_at)),
        accept_ranges: Some("bytes".into()),
        metadata: super::metadata::response(description.mtime),
        ..Default::default()
    });
    if condition(&req.headers, &etag, node.updated_at, false)?.is_some() {
        return Err(not_modified(&etag, node.updated_at));
    }
    if let Some(range) = req.input.range {
        let range = range.check(description.size).map_err(|_| s3_error!(InvalidRange))?;
        response.output.content_length = Some((range.end - range.start) as i64);
        response.output.content_range = Some(format!("bytes {}-{}/{}", range.start, range.end - 1, description.size));
    }
    Ok(response)
}

pub struct Incoming<'a> {
    pub size: Option<i64>,
    pub md5: Option<&'a str>,
    pub headers: &'a HeaderMap,
    pub trailers: Option<&'a s3s::TrailingHeaders>,
    pub replaced: u64,
}

pub async fn receive<S, E>(
    st: &AppState,
    identity: &Identity,
    parent: &crate::tree::Node,
    body: S,
    incoming: Incoming<'_>,
) -> S3Result<(super::temp::Temporary, u64, Option<String>, ETag)>
where
    S: Stream<Item = Result<bytes::Bytes, E>> + Unpin,
{
    let Incoming { size, md5, headers, trailers, replaced } = incoming;
    let size = size.map(|n| u64::try_from(n).map_err(|_| s3_error!(InvalidArgument))).transpose()?;
    if let Some(size) = size {
        access::check_size(st, size).map_err(super::map_error)?;
    }
    let room = {
        let mut c = st.db.acquire().await.map_err(super::internal)?;
        crate::tree::room_left(&mut c, parent.drive(), None).await.map_err(super::map_error)?.map(|(n, drive)| ((n.max(0) as u64).saturating_add(replaced), drive))
    };
    let tmp = super::temp::Temporary::new(st, "receive");
    let sums = std::sync::Arc::new(crate::sync::Mutex::new(super::checksums::hasher(headers)));
    let sums_in_body = sums.clone();
    let body = body.inspect(move |chunk| {
        if let Ok(bytes) = chunk {
            sums_in_body.lock().update(bytes);
        }
    });
    let result = async {
        let (received, hash) = access::receive(st, body, tmp.path(), !parent.in_folder_space(), room).await.map_err(super::map_error)?;
        if size.is_some_and(|n| n != received) {
            return Err(s3_error!(IncompleteBody));
        }
        // The active key and creator are checked after the body as well, before a staged write can start.
        super::credentials::identity(st, &identity.key.access_key, true).await?;
        let sums = std::sync::Arc::try_unwrap(sums).map_err(|_| s3_error!(InternalError))?.into_inner().finalize();
        super::checksums::verify(headers, trailers, &sums)?;
        let digest = hex::encode(STANDARD.decode(sums.checksum_md5.unwrap_or_default()).map_err(super::internal)?);
        if let Some(wanted) = md5 {
            let decoded = STANDARD.decode(wanted).map_err(|_| s3_error!(InvalidDigest))?;
            if decoded != hex::decode(&digest).map_err(super::internal)? {
                return Err(s3_error!(BadDigest));
            }
        }
        Ok((received, hash, ETag::Strong(digest)))
    }
    .await;
    let (received, hash, etag) = result?;
    Ok((tmp, received, hash, etag))
}

/// What a write expects at `key` (`If-None-Match: *`, or `If-Match` with the ETag it has), checked again when it is
/// stored, under the write lock: a lock file of Terraform's is made only once, and never over someone else's
pub async fn expected(st: &AppState, identity: &Identity, key: &str, headers: &HeaderMap) -> S3Result<access::Expect> {
    let text = |name| headers.get(name).and_then(|v| v.to_str().ok());
    if let Some(none) = text("if-none-match") {
        return if none.trim() == "*" { Ok(access::Expect::Absent) } else { Err(s3_error!(NotImplemented, "If-None-Match takes only *")) };
    }
    let Some(wanted) = text("if-match") else { return Ok(access::Expect::Any) };
    let node = match access::rooted_item(st, &identity.user, &identity.root.id, key, Need::Read).await {
        Ok(node) if !node.is_folder() => node,
        Ok(_) => return Err(s3_error!(NoSuchKey)),
        Err(e) => return Err(super::map_error(e)),
    };
    if !etag_matches(wanted, &super::etag::describe_node(st, &node).await?.etag) {
        return Err(s3_error!(PreconditionFailed));
    }
    Ok(access::Expect::Same { id: node.id, blob_hash: node.blob_hash, size: node.size, updated_at: node.updated_at })
}

pub async fn put(st: &AppState, mut req: S3Request<PutObjectInput>) -> S3Result<S3Response<PutObjectOutput>> {
    let identity = request_identity(st, &req, Some(&req.input.bucket), true).await?;
    unsupported(&req.headers)?;
    let expect = expected(st, &identity, &req.input.key, &req.headers).await?;
    let mtime = super::metadata::mtime(&req.headers)?;
    if req.input.key.ends_with('/') && mtime.is_some() {
        return Err(s3_error!(NotImplemented));
    }
    let (parent, name) = access::rooted_parent(st, &identity.user, &identity.root.id, &req.input.key).await.map_err(super::map_error)?;
    let up = prepare_upload(st, &identity, &parent, name.clone()).await;
    let replaced = if !req.input.key.ends_with('/') { up?.existing.map_or(0, |n| n.size.max(0) as u64) } else { 0 };
    let body = req.input.body.take().unwrap_or_else(|| StreamingBlob::from_bytes(bytes::Bytes::new()));
    let (tmp, size, hash, etag) = receive(
        st,
        &identity,
        &parent,
        body,
        Incoming { size: req.input.content_length, md5: req.input.content_md5.as_deref(), headers: &req.headers, trailers: req.trailing_headers.as_ref(), replaced },
    )
    .await?;

    async {
        if req.input.key.ends_with('/') {
            if size != 0 {
                return Err(s3_error!(InvalidArgument, "Folder objects must be empty"));
            }
            if access::rooted_item(st, &identity.user, &identity.root.id, &req.input.key, Need::Read).await.is_err() {
                access::make_folder(st, &identity.user, &parent, &name).await.map_err(super::map_error)?;
            }
        } else {
            let scope = access::ScopedWrite { root: identity.root.id.clone(), mtime, expect };
            commit(st.clone(), identity.user.clone(), parent, name, (tmp, size, hash, Some(etag.value().to_string())), scope).await?;
        }
        Ok(S3Response::new(PutObjectOutput { e_tag: Some(etag), ..Default::default() }))
    }
    .await
}

/// Deletes the object `key`. A folder key (`dir/`) is S3's folder marker: only an empty folder goes, as in S3, where
/// deleting the marker leaves the objects below it; a folder with something in it stays as it is.
pub async fn delete_key(st: &AppState, identity: &Identity, key: &str) -> S3Result<()> {
    match access::rooted_item(st, &identity.user, &identity.root.id, key, Need::Delete).await {
        Ok(node) if node.is_folder() => match crate::nodes::trash_empty_folder(st, &identity.user, &node.id).await {
            Err(e) if e.message == crate::nodes::NOT_EMPTY => Ok(()),
            result => result.map_err(super::map_error),
        },
        Ok(node) => access::trash(st, &identity.user, &node.id).await.map_err(super::map_error),
        Err(e) if e.status == StatusCode::NOT_FOUND => Ok(()),
        Err(e) => Err(super::map_error(e)),
    }
}

pub async fn delete(st: &AppState, req: S3Request<DeleteObjectInput>) -> S3Result<S3Response<DeleteObjectOutput>> {
    let identity = request_identity(st, &req, Some(&req.input.bucket), true).await?;
    if req.input.version_id.is_some() {
        return Err(s3_error!(NotImplemented));
    }
    delete_key(st, &identity, &req.input.key).await?;
    Ok(S3Response::new(DeleteObjectOutput::default()))
}

pub async fn delete_many(st: &AppState, req: S3Request<DeleteObjectsInput>) -> S3Result<S3Response<DeleteObjectsOutput>> {
    let identity = request_identity(st, &req, Some(&req.input.bucket), true).await?;
    if req.input.delete.objects.len() > 1000 {
        return Err(s3_error!(InvalidArgument));
    }
    let mut deleted = Vec::new();
    let mut errors = Vec::new();
    for object in req.input.delete.objects {
        let result = if object.version_id.is_some() { Err(s3_error!(NotImplemented)) } else { delete_key(st, &identity, &object.key).await };
        match result {
            Ok(()) if req.input.delete.quiet != Some(true) => deleted.push(DeletedObject { key: Some(object.key), ..Default::default() }),
            Ok(()) => (),
            Err(e) => {
                errors.push(Error { key: Some(object.key), code: Some(e.code().as_str().into()), message: e.message().map(str::to_string), ..Default::default() })
            }
        }
    }
    Ok(S3Response::new(DeleteObjectsOutput { deleted: Some(deleted), errors: Some(errors), ..Default::default() }))
}

pub async fn copy(st: &AppState, req: S3Request<CopyObjectInput>) -> S3Result<S3Response<CopyObjectOutput>> {
    let identity = request_identity(st, &req, Some(&req.input.bucket), true).await?;
    unsupported(&req.headers)?;
    let CopySource::Bucket { bucket, key, version_id } = req.input.copy_source else {
        return Err(s3_error!(NotImplemented));
    };
    if version_id.is_some() {
        return Err(s3_error!(NotImplemented));
    }
    if bucket.as_ref() != identity.bucket.name {
        return Err(s3_error!(AccessDenied));
    }
    let node = access::rooted_item(st, &identity.user, &identity.root.id, &key, Need::Read).await.map_err(super::map_error)?;
    if node.is_folder() {
        return Err(s3_error!(InvalidArgument));
    }
    let description = super::etag::describe_node(st, &node).await?;
    let mtime = match req.headers.get("x-amz-metadata-directive").and_then(|v| v.to_str().ok()).unwrap_or("COPY") {
        "COPY" => description.mtime.clone(),
        "REPLACE" => super::metadata::mtime(&req.headers)?,
        _ => return Err(s3_error!(InvalidArgument)),
    };
    let etag = description.etag.clone();
    condition(&req.headers, &etag, node.updated_at, true)?;
    let (parent, name) = access::rooted_parent(st, &identity.user, &identity.root.id, &req.input.key).await.map_err(super::map_error)?;
    let existing = prepare_upload(st, &identity, &parent, name.clone()).await?;
    let replaced = existing.existing.map_or(0, |n| n.size.max(0) as u64);
    let source = description.source.ok_or_else(|| s3_error!(NoSuchKey))?;
    let body = ReaderStream::with_capacity(source.open(st, 0, description.size).await.map_err(super::internal)?, 256 * 1024);
    let (tmp, size, hash, received_etag) =
        receive(st, &identity, &parent, body, Incoming { size: Some(description.size as i64), md5: None, headers: &HeaderMap::new(), trailers: None, replaced })
            .await?;
    if received_etag != etag {
        return Err(s3_error!(PreconditionFailed));
    }
    let scope = access::ScopedWrite { root: identity.root.id, mtime, expect: Default::default() };
    commit(st.clone(), identity.user, parent, name, (tmp, size, hash, Some(etag.value().to_string())), scope).await?;
    Ok(S3Response::new(CopyObjectOutput {
        copy_object_result: Some(CopyObjectResult { e_tag: Some(etag), last_modified: Some(timestamp(now())), ..Default::default() }),
        ..Default::default()
    }))
}

/// Once staging starts it finishes independently of the client connection, as web and WebDAV writes do. The content
/// comes with its SHA-256 and MD5 when receiving it worked them out, so it isn't read again for them.
pub async fn commit(
    st: AppState,
    user: crate::auth::User,
    parent: crate::tree::Node,
    name: String,
    (tmp, size, hash, md5): (super::temp::Temporary, u64, Option<String>, Option<String>),
    scope: access::ScopedWrite,
) -> S3Result<()> {
    tokio::spawn(async move {
        access::store_in_scope(st, user, parent, name, crate::content::Received { path: tmp.path().to_path_buf(), size, hash, md5 }, scope)
            .await
            .map_err(super::map_error)?;
        drop(tmp);
        Ok(())
    })
    .await
    .map_err(super::internal)?
}

pub async fn prepare_upload(st: &AppState, identity: &Identity, parent: &crate::tree::Node, name: String) -> S3Result<access::Upload> {
    let up = access::upload_to(st, &identity.user, parent, name.clone()).await.map_err(super::map_error)?;
    if up.existing.as_ref().is_some_and(|n| n.name != name) {
        return Err(s3_error!(InvalidArgument, "Object keys that differ only in case conflict in this space"));
    }
    Ok(up)
}

fn not_modified(etag: &ETag, modified: i64) -> s3s::S3Error {
    let mut error = s3_error!(NotModified);
    let mut headers = HeaderMap::new();
    if let Ok(value) = axum::http::HeaderValue::from_str(&format!("\"{}\"", etag.value())) {
        headers.insert("etag", value);
    }
    if let Ok(value) = axum::http::HeaderValue::from_str(&httpdate::fmt_http_date(std::time::UNIX_EPOCH + std::time::Duration::from_secs(modified.max(0) as u64))) {
        headers.insert("last-modified", value);
    }
    error.set_headers(headers);
    error
}
