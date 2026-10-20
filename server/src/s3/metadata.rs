//! The sole first-stage metadata exception is rclone's modification time. General custom metadata stays unsupported.

use thirtys3_core::s3_error::code;

use super::{S3Result, failed};
use crate::state::AppState;

/// The modification time a client gave as `x-amz-meta-mtime` (the user metadata the service passes on, names in lower case)
pub fn mtime(metadata: &[(String, String)]) -> S3Result<Option<String>> {
    let Some((_, value)) = metadata.iter().find(|(name, _)| name == "mtime") else { return Ok(None) };
    if value.len() > 64 || value.trim() != value || value.chars().any(char::is_control) {
        return Err(failed(code::INVALID_ARGUMENT, "Invalid modification time"));
    }
    let seconds = value.parse::<f64>().map_err(|_| failed(code::INVALID_ARGUMENT, "Invalid modification time"))?;
    if !seconds.is_finite() || seconds < jiff::Timestamp::MIN.as_second() as f64 || seconds > crate::util::LAST_TIME as f64 {
        return Err(failed(code::INVALID_ARGUMENT, "Invalid modification time"));
    }
    Ok(Some(value.clone()))
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

/// The user metadata an object is answered with
pub fn response(mtime: Option<String>) -> Vec<(String, String)> {
    mtime.map(|value| ("mtime".to_string(), value)).into_iter().collect()
}

#[cfg(test)]
mod tests {
    fn given(value: &str) -> Vec<(String, String)> {
        vec![("other".into(), "x".into()), ("mtime".into(), value.into())]
    }

    #[test]
    fn modification_times_accept_pre_epoch_files_but_reject_unbounded_values() {
        for value in ["-1.123456789", "0", "1700000000.123456789"] {
            assert_eq!(super::mtime(&given(value)).unwrap().as_deref(), Some(value));
        }
        for value in ["NaN", "inf", "-9999999999999999", "1e999", " padded "] {
            assert!(super::mtime(&given(value)).is_err(), "{value}");
        }
        assert_eq!(super::mtime(&[]).unwrap(), None);
        assert_eq!(super::mtime(&[("md5chksum".into(), "abc".into())]).unwrap(), None);
    }
}
