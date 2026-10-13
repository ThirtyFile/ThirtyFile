//! The maintained s3s adapter verifies canonical SigV4, parses bounded XML and streams content. This module supplies
//! the folder mapping and permission checks; anonymous access, SigV2 and unrelated APIs are never enabled.

pub mod config;
pub(crate) mod credentials;
pub mod manage;
mod objects;
mod listing;
mod multipart;
pub(crate) mod etag;

use std::sync::Arc;
use axum::{extract::{State, Request, ConnectInfo}, middleware::Next, response::{Response, IntoResponse}, http::{StatusCode, header}};
use s3s::{S3Error, S3ErrorCode, service::S3ServiceBuilder, config::{S3Config, StaticConfigProvider}};
use crate::{auth, error::AppError, state::AppState};

#[derive(Clone)]
pub struct Service(pub AppState);

pub fn internal(e: impl std::fmt::Display) -> S3Error {
    tracing::warn!("S3 operation failed: {}", crate::redact::redact(&e.to_string()));
    s3s::s3_error!(InternalError)
}

pub fn map_error(e: AppError) -> S3Error {
    let code = match e.status {
        StatusCode::NOT_FOUND => S3ErrorCode::NoSuchKey,
        StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => S3ErrorCode::AccessDenied,
        StatusCode::PAYLOAD_TOO_LARGE => S3ErrorCode::EntityTooLarge,
        StatusCode::CONFLICT => S3ErrorCode::InvalidRequest,
        StatusCode::INSUFFICIENT_STORAGE => S3ErrorCode::QuotaExceeded,
        StatusCode::BAD_REQUEST | StatusCode::METHOD_NOT_ALLOWED => S3ErrorCode::InvalidArgument,
        _ => S3ErrorCode::InternalError,
    };
    S3Error::with_message(code, e.message)
}

fn wire_error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, [(header::CONTENT_TYPE, "application/xml")], format!("<?xml version=\"1.0\"?><Error><Code>{code}</Code><Message>{message}</Message></Error>")).into_response()
}

pub async fn dispatch(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let cfg = match config::load(&st).await { Ok(c) => c, Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response() };
    let host = req.headers().get(header::HOST).and_then(|v| v.to_str().ok()).and_then(|v| v.split(':').next()).unwrap_or_default();
    if cfg.hostname.is_empty() || !host.eq_ignore_ascii_case(&cfg.hostname) { return next.run(req).await; }
    if !cfg.enabled { return wire_error(StatusCode::FORBIDDEN, "AccessDenied", "S3 access is disabled"); }
    let peer = req.extensions().get::<ConnectInfo<std::net::SocketAddr>>().map(|v| v.0).unwrap_or_else(|| ([127,0,0,1], 0).into());
    let key = auth::limit_key_ip(&auth::client_ip(&st, peer, req.headers()));
    // Reserve first; concurrent signature failures cannot all pass the shared sign-in limit.
    if !auth::begin_attempt(&st, &key, auth::IP_FAIL_LIMIT) { return wire_error(StatusCode::TOO_MANY_REQUESTS, "SlowDown", "Too many failed sign-ins. Try again later."); }
    if !time_ok(&req) { return wire_error(StatusCode::FORBIDDEN, "RequestTimeTooSkewed", "The request time differs from the server by more than 15 minutes"); }
    let mut config = S3Config::default();
    config.xml_max_body_size = 1024 * 1024;
    config.aws_chunked_stream_max_chunk_size = 8 * 1024 * 1024;
    config.expected_region = cfg.region.parse().ok();
    config.sig_v4_allowed_services = vec!["s3".into()];
    config.put_object_max_size = Some(if st.max_upload > 0 { st.max_upload.min(5 * 1024 * 1024 * 1024) } else { 5 * 1024 * 1024 * 1024 });
    let mut builder = S3ServiceBuilder::new(Service(st.clone()));
    builder.set_auth(credentials::Auth(st.clone()));
    builder.set_config(Arc::new(StaticConfigProvider::new(Arc::new(config))));
    let (parts, body) = req.into_parts();
    let req = axum::http::Request::from_parts(parts, s3s::Body::http_body_unsync(body));
    match builder.build().call(req).await {
        Ok(res) => {
            // Authenticated operation errors (missing objects, quota…) are not failed sign-ins.
            if !matches!(res.status(), StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) { auth::attempt_succeeded(&st, &key); }
            let (parts, body) = res.into_parts();
            Response::from_parts(parts, axum::body::Body::new(body))
        }
        Err(_) => wire_error(StatusCode::BAD_REQUEST, "InvalidRequest", "The S3 request could not be read"),
    }
}

/// s3s checks presigned expiry; signed headers additionally need an explicit replay window.
fn time_ok(req: &Request) -> bool {
    if req.uri().query().is_some_and(|q| form_urlencoded::parse(q.as_bytes()).any(|(k, _)| k == "X-Amz-Algorithm")) { return true; }
    let Some(date) = req.headers().get("x-amz-date").and_then(|v| v.to_str().ok()) else { return false };
    if date.len() != 16 || !date.is_ascii() { return false; }
    let iso = format!("{}-{}-{}T{}:{}:{}Z", &date[0..4], &date[4..6], &date[6..8], &date[9..11], &date[11..13], &date[13..15]);
    let Ok(ts) = iso.parse::<jiff::Timestamp>() else { return false };
    (crate::util::now() - ts.as_second()).unsigned_abs() <= 900
}

#[async_trait::async_trait]
impl s3s::S3 for Service {
    async fn get_object(&self, req: s3s::S3Request<s3s::dto::GetObjectInput>) -> s3s::S3Result<s3s::S3Response<s3s::dto::GetObjectOutput>> { objects::get(&self.0, req).await }
    async fn head_object(&self, req: s3s::S3Request<s3s::dto::HeadObjectInput>) -> s3s::S3Result<s3s::S3Response<s3s::dto::HeadObjectOutput>> { objects::head(&self.0, req).await }
    async fn put_object(&self, req: s3s::S3Request<s3s::dto::PutObjectInput>) -> s3s::S3Result<s3s::S3Response<s3s::dto::PutObjectOutput>> { objects::put(&self.0, req).await }
    async fn copy_object(&self, req: s3s::S3Request<s3s::dto::CopyObjectInput>) -> s3s::S3Result<s3s::S3Response<s3s::dto::CopyObjectOutput>> { objects::copy(&self.0, req).await }
    async fn delete_object(&self, req: s3s::S3Request<s3s::dto::DeleteObjectInput>) -> s3s::S3Result<s3s::S3Response<s3s::dto::DeleteObjectOutput>> { objects::delete(&self.0, req).await }
    async fn delete_objects(&self, req: s3s::S3Request<s3s::dto::DeleteObjectsInput>) -> s3s::S3Result<s3s::S3Response<s3s::dto::DeleteObjectsOutput>> { objects::delete_many(&self.0, req).await }
    async fn list_objects_v2(&self, req: s3s::S3Request<s3s::dto::ListObjectsV2Input>) -> s3s::S3Result<s3s::S3Response<s3s::dto::ListObjectsV2Output>> { listing::v2(&self.0, req).await }
    async fn list_objects(&self, req: s3s::S3Request<s3s::dto::ListObjectsInput>) -> s3s::S3Result<s3s::S3Response<s3s::dto::ListObjectsOutput>> { listing::v1(&self.0, req).await }
    async fn create_multipart_upload(&self, req: s3s::S3Request<s3s::dto::CreateMultipartUploadInput>) -> s3s::S3Result<s3s::S3Response<s3s::dto::CreateMultipartUploadOutput>> { multipart::create(&self.0, req).await }
    async fn upload_part(&self, req: s3s::S3Request<s3s::dto::UploadPartInput>) -> s3s::S3Result<s3s::S3Response<s3s::dto::UploadPartOutput>> { multipart::part(&self.0, req).await }
    async fn complete_multipart_upload(&self, req: s3s::S3Request<s3s::dto::CompleteMultipartUploadInput>) -> s3s::S3Result<s3s::S3Response<s3s::dto::CompleteMultipartUploadOutput>> { multipart::complete(&self.0, req).await }
    async fn abort_multipart_upload(&self, req: s3s::S3Request<s3s::dto::AbortMultipartUploadInput>) -> s3s::S3Result<s3s::S3Response<s3s::dto::AbortMultipartUploadOutput>> { multipart::abort(&self.0, req).await }
    async fn list_parts(&self, req: s3s::S3Request<s3s::dto::ListPartsInput>) -> s3s::S3Result<s3s::S3Response<s3s::dto::ListPartsOutput>> { multipart::list_parts(&self.0, req).await }
    async fn list_multipart_uploads(&self, req: s3s::S3Request<s3s::dto::ListMultipartUploadsInput>) -> s3s::S3Result<s3s::S3Response<s3s::dto::ListMultipartUploadsOutput>> { multipart::list_uploads(&self.0, req).await }

    async fn list_buckets(&self, req: s3s::S3Request<s3s::dto::ListBucketsInput>) -> s3s::S3Result<s3s::S3Response<s3s::dto::ListBucketsOutput>> {
        let identity = credentials::request_identity(&self.0, &req, None, false).await?;
        let buckets = if req.input.prefix.as_ref().is_none_or(|prefix| identity.bucket.name.starts_with(prefix)) {
            vec![s3s::dto::Bucket { name: Some(identity.bucket.name), creation_date: Some(objects::timestamp(identity.bucket.created_at)), ..Default::default() }]
        } else { vec![] };
        Ok(s3s::S3Response::new(s3s::dto::ListBucketsOutput { buckets: Some(buckets), ..Default::default() }))
    }
    async fn head_bucket(&self, req: s3s::S3Request<s3s::dto::HeadBucketInput>) -> s3s::S3Result<s3s::S3Response<s3s::dto::HeadBucketOutput>> {
        credentials::request_identity(&self.0, &req, Some(&req.input.bucket), false).await?;
        let cfg = config::load(&self.0).await.map_err(map_error)?;
        Ok(s3s::S3Response::new(s3s::dto::HeadBucketOutput { bucket_region: Some(cfg.region), ..Default::default() }))
    }
    async fn get_bucket_location(&self, req: s3s::S3Request<s3s::dto::GetBucketLocationInput>) -> s3s::S3Result<s3s::S3Response<s3s::dto::GetBucketLocationOutput>> {
        credentials::request_identity(&self.0, &req, Some(&req.input.bucket), false).await?;
        let cfg = config::load(&self.0).await.map_err(map_error)?;
        Ok(s3s::S3Response::new(s3s::dto::GetBucketLocationOutput { location_constraint: if cfg.region == "us-east-1" { None } else { Some(cfg.region.into()) } }))
    }
}

pub async fn maintain(st: &AppState) {
    if let Err(e) = multipart::cleanup(st).await { tracing::warn!("S3 multipart cleanup failed: {e}"); }
}
