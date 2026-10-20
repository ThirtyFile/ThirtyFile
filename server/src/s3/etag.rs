//! MD5 validators are cached against the immutable blob hash or current filesystem version tag.

use md5::{Digest, Md5};
use thirtys3_core::s3_error::code;
use tokio::io::{AsyncRead, AsyncReadExt};

use super::{S3Result, failed};
use crate::{error::AppResult, files::Source, state::AppState, tree::Node};

pub async fn reader(mut reader: impl AsyncRead + Unpin) -> AppResult<String> {
    let mut md5 = Md5::new();
    let mut buf = vec![0; 256 * 1024];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        md5.update(&buf[..n]);
    }
    Ok(hex::encode(md5.finalize()))
}

pub struct Described {
    pub source: Option<Source>,
    pub size: u64,
    /// The MD5 in hexadecimal, as the ETag has it between its quotes
    pub etag: String,
    pub mtime: Option<String>,
}

/// An ETag as S3 writes it: the MD5 between double quotes
pub fn quoted(md5: &str) -> String {
    format!("\"{md5}\"")
}

pub async fn describe_node(st: &AppState, node: &Node) -> S3Result<Described> {
    if node.is_folder() {
        return Ok(Described { source: None, size: 0, etag: hex::encode(Md5::digest([])), mtime: None });
    }
    let source = Source::resolve(st, node).await.map_err(super::map_error)?;
    let (size, version) = source.describe(node.size as u64).await.map_err(super::map_error)?;
    let mtime = super::metadata::read(st, &node.id, &version).await?;
    let cached: Option<(String,)> = sqlx::query_as("SELECT content_md5 FROM nodes WHERE id = ? AND md5_version = ? AND content_md5 IS NOT NULL")
        .bind(&node.id)
        .bind(&version)
        .fetch_optional(&st.db)
        .await
        .map_err(super::internal)?;
    if let Some((md5,)) = cached {
        return Ok(Described { source: Some(source), size, etag: md5, mtime });
    }
    let md5 = reader(source.open(st, 0, size).await.map_err(super::internal)?).await.map_err(super::map_error)?;
    let (_, after) = source.describe(size).await.map_err(super::map_error)?;
    if after != version {
        return Err(failed(code::INVALID_REQUEST, "The file changed while it was read"));
    }
    // A concurrent replacement must not cache an old digest on the new node content. Only a cache: should the database
    // be busy, the digest is worked out again next time rather than the request fail
    if let Err(e) = sqlx::query("UPDATE nodes SET content_md5 = ?, md5_version = ? WHERE id = ? AND blob_hash IS ? AND size = ? AND updated_at = ?")
        .bind(&md5)
        .bind(version)
        .bind(&node.id)
        .bind(&node.blob_hash)
        .bind(node.size)
        .bind(node.updated_at)
        .execute(&st.db)
        .await
    {
        tracing::debug!("Couldn't keep an S3 ETag: {e}");
    }
    Ok(Described { source: Some(source), size, etag: md5, mtime })
}
