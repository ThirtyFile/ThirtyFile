//! The sole first-stage metadata exception is rclone's modification time. General custom metadata stays unsupported.

use crate::state::AppState;
use axum::http::HeaderMap;
use s3s::{S3Result, s3_error};

pub fn mtime(headers: &HeaderMap) -> S3Result<Option<String>> {
    let Some(value) = headers.get("x-amz-meta-mtime") else { return Ok(None) };
    let value = value.to_str().map_err(|_| s3_error!(InvalidArgument))?;
    if value.len() > 64 || value.trim() != value || value.chars().any(char::is_control) {
        return Err(s3_error!(InvalidArgument));
    }
    let seconds = value.parse::<f64>().map_err(|_| s3_error!(InvalidArgument))?;
    if !seconds.is_finite() || seconds < jiff::Timestamp::MIN.as_second() as f64 || seconds > crate::util::LAST_TIME as f64 {
        return Err(s3_error!(InvalidArgument, "Invalid modification time"));
    }
    Ok(Some(value.into()))
}

pub async fn read(st: &AppState, id: &str, version: &str) -> S3Result<Option<String>> {
    let value: Option<(Option<String>,)> = sqlx::query_as("SELECT s3_mtime FROM nodes WHERE id=? AND s3_mtime_version=?")
        .bind(id)
        .bind(version)
        .fetch_optional(&st.db)
        .await
        .map_err(super::internal)?;
    Ok(value.and_then(|v| v.0))
}

pub fn response(mtime: Option<String>) -> Option<std::collections::HashMap<String, String>> {
    mtime.map(|value| [("mtime".into(), value)].into_iter().collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn modification_times_accept_pre_epoch_files_but_reject_unbounded_values() {
        for value in ["-1.123456789", "0", "1700000000.123456789"] {
            let mut headers = axum::http::HeaderMap::new();
            headers.insert("x-amz-meta-mtime", value.parse().unwrap());
            assert_eq!(super::mtime(&headers).unwrap().as_deref(), Some(value));
        }
        for value in ["NaN", "inf", "-9999999999999999", "1e999", " padded "] {
            let mut headers = axum::http::HeaderMap::new();
            headers.insert("x-amz-meta-mtime", value.parse().unwrap());
            assert!(super::mtime(&headers).is_err());
        }
    }
}
