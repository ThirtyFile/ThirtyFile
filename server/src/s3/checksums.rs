//! Current AWS clients send additional integrity headers or aws-chunked trailers. Verify them before committing,
//! rather than acknowledging a checksum without checking it. All selected digests are streamed in one pass.

use axum::http::HeaderMap;
use s3s::{S3Result, checksum::ChecksumHasher, dto::Checksum, s3_error};

pub fn hasher(headers: &HeaderMap) -> ChecksumHasher {
    let trailers = headers.get("x-amz-trailer").and_then(|v| v.to_str().ok()).unwrap_or_default();
    let wants = |name| headers.contains_key(name) || trailers.split(',').any(|t| t.trim() == name);
    ChecksumHasher {
        md5: Some(Default::default()),
        crc32: wants("x-amz-checksum-crc32").then(Default::default),
        crc32c: wants("x-amz-checksum-crc32c").then(Default::default),
        crc64nvme: wants("x-amz-checksum-crc64nvme").then(Default::default),
        sha1: wants("x-amz-checksum-sha1").then(Default::default),
        sha256: wants("x-amz-checksum-sha256").then(Default::default),
        sha512: wants("x-amz-checksum-sha512").then(Default::default),
        xxhash64: wants("x-amz-checksum-xxhash64").then(Default::default),
        xxhash3: wants("x-amz-checksum-xxhash3").then(Default::default),
        xxhash128: wants("x-amz-checksum-xxhash128").then(Default::default),
    }
}

pub fn verify(headers: &HeaderMap, trailers: Option<&s3s::TrailingHeaders>, computed: &Checksum) -> S3Result<()> {
    let mut expected = headers.clone();
    if let Some(trailers) = trailers.and_then(s3s::TrailingHeaders::take) {
        expected.extend(trailers);
    }
    let values = [
        ("x-amz-checksum-crc32", &computed.checksum_crc32),
        ("x-amz-checksum-crc32c", &computed.checksum_crc32c),
        ("x-amz-checksum-crc64nvme", &computed.checksum_crc64nvme),
        ("x-amz-checksum-sha1", &computed.checksum_sha1),
        ("x-amz-checksum-sha256", &computed.checksum_sha256),
        ("x-amz-checksum-sha512", &computed.checksum_sha512),
        ("x-amz-checksum-md5", &computed.checksum_md5),
        ("x-amz-checksum-xxhash64", &computed.checksum_xxhash64),
        ("x-amz-checksum-xxhash3", &computed.checksum_xxhash3),
        ("x-amz-checksum-xxhash128", &computed.checksum_xxhash128),
    ];
    for (name, actual) in values {
        if let Some(expected) = expected.get(name) {
            if expected.to_str().ok() != actual.as_deref() {
                return Err(s3_error!(BadDigest));
            }
        }
    }
    Ok(())
}
