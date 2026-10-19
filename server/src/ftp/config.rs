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

/// The setting as saved (in the transaction of `conn`)
pub async fn from_connection(conn: &mut sqlx::SqliteConnection) -> AppResult<Config> {
    Ok(db::json_setting(conn, "ftp").await?)
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
    /// When the certificate the listener serves ends (Unix seconds)
    pub cert_expires_at: Option<i64>,
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
        cert_expires_at: status.cert_expires_at,
        listening: status.listening,
        error: status.error,
    }))
}

pub async fn save(State(st): State<AppState>, Admin(user): Admin, Json(config): Json<Config>) -> AppResult<Json<Info>> {
    let _w = st.write_lock.lock().await;
    let mut tx = db::begin_write(&st.db).await?;
    let user = crate::auth::require_admin(&mut tx, &user).await?;
    db::set_json_setting(&mut tx, "ftp", &config).await?;
    crate::logs::record_activity(&mut tx, &user, None, "settings", "Updated FTP settings").await?;
    tx.commit().await?;
    drop(_w);
    st.part::<super::Memory>().changed.notify_one();
    get(State(st), Admin(user)).await
}

pub async fn options(State(st): State<AppState>, _: crate::auth::User) -> AppResult<Json<Config>> {
    Ok(Json(load(&st).await?))
}
