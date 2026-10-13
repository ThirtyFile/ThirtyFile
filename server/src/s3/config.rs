//! S3 is opt-in and uses a separate, path-style hostname on the normal HTTP listener.

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};

use crate::{
    auth::Admin,
    db,
    error::{AppError, AppResult},
    state::AppState,
};

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub hostname: String,
    pub region: String,
    pub managers_publish: bool,
}

pub async fn load(st: &AppState) -> AppResult<Config> {
    let mut config: Config = db::get_setting(&st.db, "s3").await?.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default();
    if config.region.is_empty() {
        config.region = "us-east-1".into();
    }
    Ok(config)
}

pub async fn get(State(st): State<AppState>, _admin: Admin) -> AppResult<Json<Config>> {
    Ok(Json(load(&st).await?))
}

pub async fn save(State(st): State<AppState>, Admin(user): Admin, Json(mut config): Json<Config>) -> AppResult<Json<Config>> {
    config.hostname = config.hostname.trim().to_ascii_lowercase();
    config.region = config.region.trim().to_string();
    if config.region.is_empty() {
        config.region = "us-east-1".into();
    }
    if !(config.hostname.is_empty() && !config.enabled)
        && (config.hostname.contains(['/', ':', '@'])
            || config.hostname.len() > 253
            || config
                .hostname
                .split('.')
                .any(|p| p.is_empty() || p.starts_with('-') || p.ends_with('-') || !p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
            || (config.enabled && !config.hostname.contains('.')))
    {
        return Err(AppError::bad_request("Enter the S3 hostname without a scheme, port or path"));
    }
    if config.region.len() > 64 || !config.region.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        return Err(AppError::bad_request("Invalid S3 Region"));
    }
    let mut tx = db::begin_write(&st.db).await?;
    crate::auth::require_admin(&mut tx, &user).await?;
    db::set_setting(&mut tx, "s3", &serde_json::to_string(&config).map_err(AppError::internal)?).await?;
    tx.commit().await?;
    *st.part::<Memory>().config.write() = Some(config.clone());
    Ok(Json(config))
}

pub async fn options(State(st): State<AppState>, _user: crate::auth::User) -> AppResult<Json<Config>> {
    Ok(Json(load(&st).await?))
}

#[derive(Default)]
pub struct Memory {
    config: crate::sync::RwLock<Option<Config>>,
}

/// Routing ordinary website requests is an in-memory host comparison after the first read. Authorization still
/// reads current settings in its transaction. Saving wins over a concurrent initial cache fill.
pub async fn routing(st: &AppState) -> AppResult<Config> {
    if let Some(config) = st.part::<Memory>().config.read().clone() {
        return Ok(config);
    }
    let config = load(st).await?;
    Ok(st.part::<Memory>().config.write().get_or_insert(config).clone())
}

pub async fn from_connection(conn: &mut sqlx::SqliteConnection) -> AppResult<Config> {
    let stored: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key='s3'").fetch_optional(conn).await?;
    let mut config: Config = stored.and_then(|(value,)| serde_json::from_str(&value).ok()).unwrap_or_default();
    if config.region.is_empty() {
        config.region = "us-east-1".into();
    }
    Ok(config)
}
