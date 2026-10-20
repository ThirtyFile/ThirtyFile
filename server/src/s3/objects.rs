//! Files use the same staged writes, quota/version policy and trash as the web interface.

use std::{ops::Range, sync::Arc};

use axum::http::{HeaderMap, HeaderValue, StatusCode};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use thirtys3_core::{S3Error, Timestamp, s3_error::code};
use thirtys3_server::{
    ByteStream, CopyObjectOutput, CopyObjectRequest, Ctx, ObjectData, ObjectInfo, ObjectSource, PutObjectOutput, PutObjectRequest, WriteCondition,
};
use tokio_util::io::ReaderStream;

use super::{
    S3Result,
    credentials::{Identity, request_identity},
    etag::{Described, describe_node, quoted},
    failed, internal, map_error,
};
use crate::{access, files::Source, state::AppState, tree::Need, util::now};

pub fn timestamp(seconds: i64) -> Timestamp {
    Timestamp(seconds.max(0))
}

/// What is known about a file for a download. Unless a signed request asks for something else with a `response-*` parameter (the service applies
/// those, as S3 does), a download is an attachment that the browser may not take for another type
fn info_of(node: &crate::tree::Node, described: &Described) -> ObjectInfo {
    let mut info = ObjectInfo::new(described.size, quoted(&described.etag), timestamp(node.updated_at));
    info.content_type = Some(node.mime.clone());
    info.metadata = super::metadata::response(described.mtime.clone());
    info.headers.insert("content-disposition", HeaderValue::from_static("attachment"));
    info.headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    info
}

/// The data of a file as `range` of it, once the service has checked the conditions against the info that came with the source
struct FileSource {
    st: AppState,
    source: Option<Source>,
}

#[async_trait::async_trait]
impl ObjectSource for FileSource {
    async fn open(self: Box<Self>, range: Range<u64>) -> Result<ByteStream, S3Error> {
        let Some(source) = self.source else { return Ok(Box::pin(futures_util::stream::empty())) };
        let reader = source.open(&self.st, range.start, range.end - range.start).await.map_err(internal)?;
        Ok(Box::pin(ReaderStream::with_capacity(reader, 256 * 1024)))
    }
}

pub async fn get(st: &AppState, ctx: &Ctx<'_>, bucket: &str, key: &str) -> S3Result<ObjectData> {
    let identity = request_identity(st, ctx, Some(bucket), false).await?;
    let node = access::rooted_item(st, &identity.user, &identity.root.id, key, Need::Read).await.map_err(map_error)?;
    let described = describe_node(st, &node).await?;
    let info = info_of(&node, &described);
    Ok(ObjectData { info, source: Box::new(FileSource { st: st.clone(), source: described.source }) })
}

pub async fn head(st: &AppState, ctx: &Ctx<'_>, bucket: &str, key: &str) -> S3Result<ObjectInfo> {
    let identity = request_identity(st, ctx, Some(bucket), false).await?;
    let node = access::rooted_item(st, &identity.user, &identity.root.id, key, Need::Read).await.map_err(map_error)?;
    let described = describe_node(st, &node).await?;
    Ok(info_of(&node, &described))
}

pub struct Incoming {
    /// The number of bytes the request says there are
    pub declared: Option<u64>,
    /// The size of the file this one replaces, which the quota counts as freed
    pub replaced: u64,
}

/// Receives `body` into a temporary file, checking the size limit and the room as it arrives. The service has checked the body itself (its length, its
/// digests and signatures) by the time it ends; an error of the body is the error of the request
pub async fn receive<S>(
    st: &AppState,
    identity: &Identity,
    parent: &crate::tree::Node,
    body: S,
    incoming: Incoming,
) -> S3Result<(super::temp::Temporary, u64, Option<String>)>
where
    S: Stream<Item = Result<Bytes, S3Error>> + Unpin,
{
    let Incoming { declared, replaced } = incoming;
    if let Some(size) = declared {
        access::check_size(st, size).map_err(map_error)?;
    }
    let room = {
        let mut c = st.db.acquire().await.map_err(internal)?;
        crate::tree::room_left(&mut c, parent.drive(), None).await.map_err(map_error)?.map(|(n, drive)| ((n.max(0) as u64).saturating_add(replaced), drive))
    };
    let tmp = super::temp::Temporary::new(st, "receive");
    let failure: Arc<crate::sync::Mutex<Option<S3Error>>> = Arc::default();
    let slot = failure.clone();
    let body = body.map(move |piece| {
        piece.map_err(|e| {
            *slot.lock() = Some(e);
            std::io::Error::other("the body of the request failed")
        })
    });
    let (received, hash) = match access::receive(st, body, tmp.path(), !parent.in_folder_space(), room).await {
        Ok(received) => received,
        Err(e) => return Err(failure.lock().take().unwrap_or_else(|| map_error(e))),
    };
    // The active key and creator are checked after the body as well, before a staged write can start.
    super::credentials::identity(st, &identity.key.access_key, true).await?;
    Ok((tmp, received, hash))
}

/// What a write expects at `key` (`If-None-Match: *`, or `If-Match` with the ETag it has), checked again when it is
/// stored, under the write lock: a lock file of Terraform's is made only once, and never over someone else's
pub async fn expected(st: &AppState, identity: &Identity, key: &str, condition: &WriteCondition) -> S3Result<access::Expect> {
    let tags = match condition {
        WriteCondition::None => return Ok(access::Expect::Any),
        WriteCondition::Absent => return Ok(access::Expect::Absent),
        WriteCondition::Match(tags) => tags,
        _ => return Err(failed(code::NOT_IMPLEMENTED, "This condition is not supported")),
    };
    let node = match access::rooted_item(st, &identity.user, &identity.root.id, key, Need::Read).await {
        Ok(node) if !node.is_folder() => node,
        Ok(_) => return Err(failed(code::NO_SUCH_KEY, "The specified key does not exist.")),
        Err(e) => return Err(map_error(e)),
    };
    if !tags.matches(&quoted(&describe_node(st, &node).await?.etag), true) {
        return Err(failed(code::PRECONDITION_FAILED, "At least one of the pre-conditions you specified did not hold"));
    }
    Ok(access::Expect::Same { id: node.id, blob_hash: node.blob_hash, size: node.size, updated_at: node.updated_at })
}

pub async fn put(st: &AppState, ctx: &Ctx<'_>, req: PutObjectRequest) -> S3Result<PutObjectOutput> {
    let identity = request_identity(st, ctx, Some(&req.bucket), true).await?;
    let expect = expected(st, &identity, &req.key, &req.condition).await?;
    let mtime = super::metadata::mtime(&req.metadata)?;
    if req.key.ends_with('/') && mtime.is_some() {
        return Err(failed(code::NOT_IMPLEMENTED, "A folder has no modification time of its own"));
    }
    let (parent, name) = access::rooted_parent(st, &identity.user, &identity.root.id, &req.key).await.map_err(map_error)?;
    let up = prepare_upload(st, &identity, &parent, name.clone()).await;
    let replaced = if !req.key.ends_with('/') { up?.existing.map_or(0, |n| n.size.max(0) as u64) } else { 0 };
    let digests = req.body.handle();
    let (tmp, size, hash) = receive(st, &identity, &parent, req.body, Incoming { declared: req.content_length, replaced }).await?;
    let md5 = hex::encode(digests.get().ok_or_else(|| internal("the body of an upload ended without its digests"))?.md5);
    if req.key.ends_with('/') {
        if size != 0 {
            return Err(failed(code::INVALID_ARGUMENT, "Folder objects must be empty"));
        }
        if access::rooted_item(st, &identity.user, &identity.root.id, &req.key, Need::Read).await.is_err() {
            access::make_folder(st, &identity.user, &parent, &name).await.map_err(map_error)?;
        }
    } else {
        let scope = access::ScopedWrite { root: identity.root.id.clone(), mtime, expect };
        commit(st.clone(), identity.user.clone(), parent, name, (tmp, size, hash, Some(md5.clone())), scope).await?;
    }
    Ok(PutObjectOutput { etag: quoted(&md5), version_id: None, headers: HeaderMap::new() })
}

/// Deletes the object `key`. A folder key (`dir/`) is S3's folder marker: only an empty folder goes, as in S3, where
/// deleting the marker leaves the objects below it; a folder with something in it stays as it is.
pub async fn delete_key(st: &AppState, identity: &Identity, key: &str) -> S3Result<()> {
    match access::rooted_item(st, &identity.user, &identity.root.id, key, Need::Delete).await {
        Ok(node) if node.is_folder() => match crate::nodes::trash_empty_folder(st, &identity.user, &node.id).await {
            Err(e) if e.message == crate::nodes::NOT_EMPTY => Ok(()),
            result => result.map_err(map_error),
        },
        Ok(node) => access::trash(st, &identity.user, &node.id).await.map_err(map_error),
        Err(e) if e.status == StatusCode::NOT_FOUND => Ok(()),
        Err(e) => Err(map_error(e)),
    }
}

pub async fn delete(st: &AppState, ctx: &Ctx<'_>, bucket: &str, key: &str) -> S3Result<()> {
    let identity = request_identity(st, ctx, Some(bucket), true).await?;
    delete_key(st, &identity, key).await
}

pub async fn copy(st: &AppState, ctx: &Ctx<'_>, req: CopyObjectRequest) -> S3Result<CopyObjectOutput> {
    let identity = request_identity(st, ctx, Some(&req.bucket), true).await?;
    if req.source_bucket != identity.bucket.name {
        return Err(failed(code::ACCESS_DENIED, "Access Denied"));
    }
    let node = access::rooted_item(st, &identity.user, &identity.root.id, &req.source_key, Need::Read).await.map_err(map_error)?;
    if node.is_folder() {
        return Err(failed(code::INVALID_ARGUMENT, "A folder can't be copied"));
    }
    let description = describe_node(st, &node).await?;
    let mtime = if req.replace_metadata { super::metadata::mtime(&req.metadata)? } else { description.mtime.clone() };
    let etag = quoted(&description.etag);
    // The service checked these against what it read before; once more against what is about to be copied
    req.source_conditions.evaluate_copy(&etag, timestamp(node.updated_at))?;
    let expect = expected(st, &identity, &req.key, &req.condition).await?;
    let (parent, name) = access::rooted_parent(st, &identity.user, &identity.root.id, &req.key).await.map_err(map_error)?;
    let existing = prepare_upload(st, &identity, &parent, name.clone()).await?;
    let replaced = existing.existing.map_or(0, |n| n.size.max(0) as u64);
    let source = description.source.ok_or_else(|| failed(code::NO_SUCH_KEY, "The specified key does not exist."))?;
    let md5 = Arc::new(crate::sync::Mutex::new(<md5::Md5 as md5::Digest>::new()));
    let hashing = md5.clone();
    let body = ReaderStream::with_capacity(source.open(st, 0, description.size).await.map_err(internal)?, 256 * 1024).map(move |piece| match piece {
        Ok(bytes) => {
            md5::Digest::update(&mut *hashing.lock(), &bytes);
            Ok(bytes)
        }
        Err(e) => Err(internal(e)),
    });
    let (tmp, size, hash) = receive(st, &identity, &parent, body, Incoming { declared: Some(description.size), replaced }).await?;
    let copied = hex::encode(md5::Digest::finalize_reset(&mut *md5.lock()));
    if copied != description.etag {
        return Err(failed(code::PRECONDITION_FAILED, "The object changed while it was copied"));
    }
    let scope = access::ScopedWrite { root: identity.root.id, mtime, expect };
    commit(st.clone(), identity.user, parent, name, (tmp, size, hash, Some(description.etag)), scope).await?;
    Ok(CopyObjectOutput { etag, last_modified: timestamp(now()), headers: HeaderMap::new() })
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
            .map_err(map_error)?;
        drop(tmp);
        Ok(())
    })
    .await
    .map_err(internal)?
}

pub async fn prepare_upload(st: &AppState, identity: &Identity, parent: &crate::tree::Node, name: String) -> S3Result<access::Upload> {
    let up = access::upload_to(st, &identity.user, parent, name.clone()).await.map_err(map_error)?;
    if up.existing.as_ref().is_some_and(|n| n.name != name) {
        return Err(failed(code::INVALID_ARGUMENT, "Object keys that differ only in case conflict in this space"));
    }
    Ok(up)
}
