//! The S3 service of ThirtyS3 (protocol, signatures, XML, conditions and ranges) over our folders: this module supplies the mapping of buckets and
//! keys to folders and the permission checks. Anonymous access, SigV2 and unrelated APIs are never enabled.

#![allow(
    clippy::result_large_err,
    reason = "`S3Error` is the error of every operation and carries what a response needs, so it is not small; the service's own code allows it too"
)]

pub mod config;
pub(crate) mod credentials;
pub(crate) mod etag;
mod listing;
pub mod manage;
pub(crate) mod metadata;
mod multipart;
mod objects;
mod temp;
#[cfg(test)]
mod tests;

use crate::{auth, error::AppError, state::AppState};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use thirtys3_core::{S3Error, s3_error::code};
use thirtys3_server::{
    AuthInfo, AuthOutcome, Backend, BucketInfo, CompleteMultipartUploadOutput, CompleteMultipartUploadRequest, CopyObjectOutput, CopyObjectRequest,
    CreateMultipartUploadRequest, Credential, Ctx, DeleteTarget, ListBucketsOutput, ListBucketsRequest, ListPage, ListPartsOutput, ListPartsRequest, ListRequest,
    ListUploadsOutput, ListUploadsRequest, ObjectData, ObjectInfo, PutObjectOutput, PutObjectRequest, S3Service, ServerConfig, UploadPartOutput, UploadPartRequest,
};

pub type S3Result<T> = Result<T, S3Error>;

/// An S3 error with this code and message (the status is that of the code)
pub fn failed(code: &str, message: &str) -> S3Error {
    S3Error::new(code, message)
}

pub fn internal(e: impl std::fmt::Display) -> S3Error {
    tracing::warn!("S3 operation failed: {}", crate::redact::redact(&e.to_string()));
    failed(code::INTERNAL_ERROR, "We encountered an internal error. Please try again.")
}

pub fn map_error(e: AppError) -> S3Error {
    let code = match e.status {
        StatusCode::NOT_FOUND => code::NO_SUCH_KEY,
        StatusCode::PRECONDITION_FAILED => code::PRECONDITION_FAILED,
        StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => code::ACCESS_DENIED,
        StatusCode::PAYLOAD_TOO_LARGE => code::ENTITY_TOO_LARGE,
        StatusCode::CONFLICT => code::INVALID_REQUEST,
        StatusCode::INSUFFICIENT_STORAGE => {
            let mut error = failed("QuotaExceeded", &e.message);
            error.status = 507;
            return error;
        }
        StatusCode::BAD_REQUEST | StatusCode::METHOD_NOT_ALLOWED => code::INVALID_ARGUMENT,
        _ => code::INTERNAL_ERROR,
    };
    failed(code, &e.message)
}

/// What a request is counted under in the limit on failed sign-ins: its address
#[derive(Clone)]
struct RateKey(String);

/// The buckets and keys of this server, as a backend of the S3 service
pub struct Gateway(pub AppState);

#[async_trait::async_trait]
impl Backend for Gateway {
    async fn secret_key(&self, access_key: &str) -> Result<Option<Credential>, S3Error> {
        credentials::secret(&self.0, access_key).await
    }

    /// Reserves an attempt of the sign-in limit; it is given back unless the request turns out to be a bad signature or an unknown key
    async fn before_authentication(&self, info: &AuthInfo<'_>) -> Result<(), S3Error> {
        let Some(RateKey(key)) = info.extensions.get::<RateKey>() else { return Err(internal("a request came without its address")) };
        if !auth::begin_attempt(&self.0, key, auth::IP_FAIL_LIMIT) {
            return Err(failed(code::SLOW_DOWN, "Too many failed sign-ins. Try again later."));
        }
        Ok(())
    }

    async fn after_authentication(&self, info: &AuthInfo<'_>, outcome: AuthOutcome) {
        let Some(RateKey(key)) = info.extensions.get::<RateKey>() else { return };
        // A guess at a secret or at a key stays counted. A key that is known and refused, and a request that couldn't be read, are no guess
        if !matches!(outcome, AuthOutcome::BadSignature | AuthOutcome::UnknownKey) {
            auth::attempt_succeeded(&self.0, key);
        }
    }

    async fn list_buckets(&self, ctx: &Ctx<'_>, request: ListBucketsRequest) -> Result<ListBucketsOutput, S3Error> {
        let identity = credentials::request_identity(&self.0, ctx, None, false).await?;
        let buckets = if request.prefix.as_ref().is_none_or(|prefix| identity.bucket.name.starts_with(prefix)) {
            vec![BucketInfo { name: identity.bucket.name, created: objects::timestamp(identity.bucket.created_at) }]
        } else {
            vec![]
        };
        Ok(ListBucketsOutput { buckets, ..Default::default() })
    }

    async fn head_bucket(&self, ctx: &Ctx<'_>, bucket: &str) -> Result<(), S3Error> {
        credentials::request_identity(&self.0, ctx, Some(bucket), false).await.map(|_| ())
    }

    async fn get_object(&self, ctx: &Ctx<'_>, bucket: &str, key: &str) -> Result<ObjectData, S3Error> {
        objects::get(&self.0, ctx, bucket, key).await
    }

    /// Also what the service reads the source of a copy with: a key that may not read it gets no copy
    async fn head_object(&self, ctx: &Ctx<'_>, bucket: &str, key: &str) -> Result<ObjectInfo, S3Error> {
        objects::head(&self.0, ctx, bucket, key).await
    }

    async fn put_object(&self, ctx: &Ctx<'_>, request: PutObjectRequest) -> Result<PutObjectOutput, S3Error> {
        objects::put(&self.0, ctx, request).await
    }

    async fn copy_object(&self, ctx: &Ctx<'_>, request: CopyObjectRequest) -> Result<CopyObjectOutput, S3Error> {
        objects::copy(&self.0, ctx, request).await
    }

    async fn delete_object(&self, ctx: &Ctx<'_>, bucket: &str, key: &str) -> Result<(), S3Error> {
        objects::delete(&self.0, ctx, bucket, key).await
    }

    async fn delete_objects(&self, ctx: &Ctx<'_>, bucket: &str, targets: &[DeleteTarget]) -> Result<Vec<Result<(), S3Error>>, S3Error> {
        let identity = credentials::request_identity(&self.0, ctx, Some(bucket), true).await?;
        let mut results = Vec::with_capacity(targets.len());
        for target in targets {
            results.push(if target.version_id.is_some() {
                Err(failed(code::NOT_IMPLEMENTED, "Versions are not supported"))
            } else {
                objects::delete_key(&self.0, &identity, &target.key).await
            });
        }
        Ok(results)
    }

    async fn list_objects(&self, ctx: &Ctx<'_>, request: ListRequest) -> Result<ListPage, S3Error> {
        listing::list(&self.0, ctx, request).await
    }

    async fn create_multipart_upload(&self, ctx: &Ctx<'_>, request: CreateMultipartUploadRequest) -> Result<String, S3Error> {
        multipart::create(&self.0, ctx, request).await
    }

    async fn upload_part(&self, ctx: &Ctx<'_>, request: UploadPartRequest) -> Result<UploadPartOutput, S3Error> {
        multipart::part(&self.0, ctx, request).await
    }

    async fn complete_multipart_upload(&self, ctx: &Ctx<'_>, request: CompleteMultipartUploadRequest) -> Result<CompleteMultipartUploadOutput, S3Error> {
        multipart::complete(&self.0, ctx, request).await
    }

    async fn abort_multipart_upload(&self, ctx: &Ctx<'_>, bucket: &str, key: &str, upload_id: &str) -> Result<(), S3Error> {
        multipart::abort(&self.0, ctx, bucket, key, upload_id).await
    }

    async fn list_parts(&self, ctx: &Ctx<'_>, request: ListPartsRequest) -> Result<ListPartsOutput, S3Error> {
        multipart::list_parts(&self.0, ctx, request).await
    }

    async fn list_multipart_uploads(&self, ctx: &Ctx<'_>, request: ListUploadsRequest) -> Result<ListUploadsOutput, S3Error> {
        multipart::list_uploads(&self.0, ctx, request).await
    }
}

fn wire_error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, [(header::CONTENT_TYPE, "application/xml")], format!("<?xml version=\"1.0\"?><Error><Code>{code}</Code><Message>{message}</Message></Error>"))
        .into_response()
}

pub async fn dispatch(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    let cfg = match config::routing(&st).await {
        Ok(c) => c,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let host = req.headers().get(header::HOST).and_then(|v| v.to_str().ok()).and_then(|v| v.split(':').next()).unwrap_or_default();
    if cfg.hostname.is_empty() || !host.eq_ignore_ascii_case(&cfg.hostname) {
        return next.run(req).await;
    }
    if !cfg.enabled {
        return wire_error(StatusCode::FORBIDDEN, "AccessDenied", "S3 access is disabled");
    }
    let peer = req.extensions().get::<ConnectInfo<std::net::SocketAddr>>().map(|v| v.0).unwrap_or_else(|| ([127, 0, 0, 1], 0).into());
    let key = format!("ip:{}", auth::limit_key_ip(&auth::client_ip(&st, peer, req.headers())));
    req.extensions_mut().insert(RateKey(key));
    // Browser form uploads (POST policy) and the rest of S3 aren't offered: the service answers them NotImplemented
    let config = ServerConfig {
        region: cfg.region,
        max_object_size: if st.max_upload > 0 { st.max_upload.min(5 * 1024 * 1024 * 1024) } else { 5 * 1024 * 1024 * 1024 },
        ..ServerConfig::default()
    };
    let answer = S3Service::new(Gateway(st), config).handle(req).await;
    let (parts, body) = answer.into_parts();
    Response::from_parts(parts, axum::body::Body::new(body))
}

pub async fn maintain(st: &AppState) {
    if let Err(e) = multipart::cleanup(st).await {
        tracing::warn!("S3 multipart cleanup failed: {e}");
    }
    if let Err(e) = fill_digests(st, DIGEST_BYTES).await {
        tracing::warn!("Couldn't work out S3 ETags ahead: {e}");
    }
    if let Err(e) = expiry_notices(st).await {
        tracing::warn!("S3 expiry notification failed: {}", e.message);
    }
}

/// Bytes read per maintenance run to work out the ETags files in published buckets don't have yet
const DIGEST_BYTES: i64 = 4 << 30;

/// Works out the MD5 (the ETag) of files in published buckets that don't have it yet (made before S3 was turned on,
/// or changed outside S3), up to about `budget` bytes: otherwise the first listing reads every one of them while its
/// client waits. Returns how many it worked out.
pub(crate) async fn fill_digests(st: &AppState, budget: i64) -> Result<usize, String> {
    if !config::routing(st).await.map_err(|e| e.message)?.enabled {
        return Ok(0);
    }
    let rows: Vec<crate::tree::Node> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "WITH RECURSIVE sub(id) AS (
           SELECT node_id FROM s3_buckets WHERE enabled = 1
           UNION SELECT c.id FROM nodes c JOIN sub ON c.parent_id = sub.id WHERE c.trashed_at IS NULL AND c.kind = 'folder'
         )
         SELECT {} FROM nodes n WHERE n.parent_id IN (SELECT id FROM sub) AND n.kind = 'file' AND n.trashed_at IS NULL
           AND n.content_md5 IS NULL ORDER BY n.size LIMIT 1000",
        crate::tree::NODE_COLS
    )))
    .fetch_all(&st.db)
    .await
    .map_err(|e| e.to_string())?;
    let (mut read, mut filled) = (0i64, 0);
    for node in rows {
        if read >= budget {
            break;
        }
        // One that can't be read now (changed meanwhile, its disk away) waits for the next run
        if etag::describe_node(st, &node).await.is_ok() {
            filled += 1;
        }
        read += node.size.max(1);
        tokio::task::yield_now().await;
    }
    Ok(filled)
}

async fn expiry_notices(st: &AppState) -> crate::error::AppResult<()> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let rows: Vec<(String, i64, String, String, String, i64, bool)> = sqlx::query_as("SELECT k.id,b.creator_id,b.node_id,b.name,k.description,k.expires_at,k.writable FROM s3_keys k JOIN s3_buckets b ON b.id=k.bucket_id WHERE k.enabled=1 AND b.enabled=1 AND k.expiry_notified=0 AND k.expires_at>? AND k.expires_at<=? LIMIT 1000").bind(crate::util::now()).bind(crate::util::now()+crate::notify::EXPIRY_NOTICE).fetch_all(&mut *tx).await?;
    let mut outgoing = Vec::new();
    for (id, creator, node, bucket, description, expiry, writable) in rows {
        outgoing.extend(crate::notify::add(&mut tx, &[creator], &crate::notify::Notice { kind: "access_expiring", node_id: Some(node), data: serde_json::json!({"name":format!("S3: {bucket} ({description})"), "item":"folder", "role":if writable {"editor"} else {"viewer"},"expires_at":expiry}) }).await?);
        sqlx::query("UPDATE s3_keys SET expiry_notified=1 WHERE id=?").bind(id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    crate::notify::send_later(st, outgoing);
    Ok(())
}
