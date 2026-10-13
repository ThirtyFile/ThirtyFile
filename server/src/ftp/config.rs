//! The optional listener is configured separately from publication permissions.

use crate::{auth::Admin, db, error::AppResult, state::AppState};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub managers_publish: bool,
    pub allow_private_plaintext: bool,
}

pub async fn from_connection(conn: &mut sqlx::SqliteConnection) -> AppResult<Config> {
    let row: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key='ftp'").fetch_optional(conn).await?;
    Ok(row.and_then(|(v,)| serde_json::from_str(&v).ok()).unwrap_or_default())
}

pub async fn load(st: &AppState) -> AppResult<Config> {
    from_connection(&mut *st.db.acquire().await?).await
}

#[derive(Serialize)]
pub struct Info {
    #[serde(flatten)]
    pub config: Config,
    pub address: String,
    pub passive_ports: String,
    pub passive_host: String,
    pub tls_ready: bool,
    pub listening: bool,
    pub error: Option<String>,
}

pub async fn get(State(st): State<AppState>, _: Admin) -> AppResult<Json<Info>> {
    let status = st.part::<super::Memory>().status.read().clone();
    Ok(Json(Info {
        config: load(&st).await?,
        address: status.address,
        passive_ports: status.passive_ports,
        passive_host: status.passive_host,
        tls_ready: status.tls_ready,
        listening: status.listening,
        error: status.error,
    }))
}

pub async fn save(State(st): State<AppState>, Admin(user): Admin, Json(config): Json<Config>) -> AppResult<Json<Info>> {
    let _w = st.write_lock.lock().await;
    let mut tx = db::begin_write(&st.db).await?;
    db::set_setting(&mut tx, "ftp", &serde_json::to_string(&config).map_err(crate::error::AppError::internal)?).await?;
    crate::logs::record_activity(&mut tx, &user, None, "settings", "Updated FTP settings").await?;
    tx.commit().await?;
    drop(_w);
    st.part::<super::Memory>().changed.notify_one();
    get(State(st), Admin(user)).await
}

pub async fn options(State(st): State<AppState>, _: crate::auth::User) -> AppResult<Json<Config>> {
    Ok(Json(load(&st).await?))
}
