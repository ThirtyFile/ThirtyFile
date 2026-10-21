//! Optional durable receipts for online editors. Reusing a key never writes a second version.
use axum::http::HeaderMap;
use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;

use crate::{
    error::{AppError, AppResult},
    tree::Node,
};

pub(super) struct Receipt {
    key: String,
    hash: String,
}

impl Receipt {
    pub fn parse(headers: &HeaderMap, content_hash: &str, base: Option<i64>) -> AppResult<Option<Self>> {
        let Some(key) = headers.get("idempotency-key") else { return Ok(None) };
        let key = key.to_str().map_err(|_| AppError::bad_request("Invalid save request key"))?;
        if key.is_empty() || key.len() > 128 || !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') || base.is_none() {
            return Err(AppError::bad_request("Invalid save request key or base version"));
        }
        let tag = headers.get("if-match").and_then(|v| v.to_str().ok()).unwrap_or_default();
        let hash = hex::encode(Sha256::digest(format!("{content_hash}\n{base:?}\n{tag}")));
        Ok(Some(Self { key: key.to_string(), hash }))
    }

    pub async fn replay(&self, conn: &mut SqliteConnection, user: i64, node: &mut Node, size: usize) -> AppResult<bool> {
        let found: Option<(String, i64)> =
            sqlx::query_as("SELECT request_hash, revision FROM content_save_receipts WHERE node_id = ? AND user_id = ? AND request_key = ?")
                .bind(&node.id)
                .bind(user)
                .bind(&self.key)
                .fetch_optional(conn)
                .await?;
        let Some((hash, revision)) = found else { return Ok(false) };
        if hash != self.hash {
            return Err(AppError::bad_request("The save request key was reused with different content"));
        }
        node.updated_at = revision;
        node.size = size as i64;
        Ok(true)
    }

    pub async fn commit(&self, conn: &mut SqliteConnection, user: i64, node: &Node) -> AppResult<()> {
        sqlx::query("INSERT INTO content_save_receipts (node_id, user_id, request_key, request_hash, revision) VALUES (?, ?, ?, ?, ?)")
            .bind(&node.id)
            .bind(user)
            .bind(&self.key)
            .bind(&self.hash)
            .bind(node.updated_at)
            .execute(conn)
            .await?;
        Ok(())
    }
}
