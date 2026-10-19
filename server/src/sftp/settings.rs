//! SFTP's two switches, kept as one JSON setting (`sftp`) as S3's and FTP's are. The listener reads them when they
//! change (`Memory::changed`) and every few seconds; every sign-in and request checks them again.

use crate::{
    auth::Admin,
    db,
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub keys_enabled: bool,
}

impl Config {
    /// SSH keys sign in only while SFTP is on as well
    pub fn keys(&self) -> bool {
        self.enabled && self.keys_enabled
    }
}

/// The setting as saved (in the transaction of `conn`)
pub async fn from_connection(conn: &mut sqlx::SqliteConnection) -> AppResult<Config> {
    Ok(db::json_setting(conn, "sftp").await?)
}

pub async fn load(st: &AppState) -> AppResult<Config> {
    from_connection(&mut *st.db.acquire().await?).await
}

#[derive(Serialize)]
pub struct Info {
    #[serde(flatten)]
    pub config: Config,
    pub address: String,
    pub fingerprint: String,
    pub error: Option<String>,
}

pub async fn get(State(st): State<AppState>, _: Admin) -> AppResult<Json<Info>> {
    let memory = st.part::<super::Memory>();
    Ok(Json(Info {
        config: load(&st).await?,
        address: memory.address.read().clone(),
        fingerprint: memory.fingerprint.read().clone(),
        error: memory.error.read().clone(),
    }))
}

pub async fn update(State(st): State<AppState>, Admin(user): Admin, Json(req): Json<Config>) -> AppResult<Json<Info>> {
    if req.enabled && st.part::<super::Memory>().fingerprint.read().is_empty() {
        return Err(AppError::conflict("The SFTP listener is unavailable. Check the server log."));
    }
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    crate::auth::require_admin(&mut tx, &user).await?;
    db::set_json_setting(&mut tx, "sftp", &req).await?;
    crate::logs::record_activity(&mut tx, &user, None, "settings", "Updated SFTP settings").await?;
    tx.commit().await?;
    drop(_w);
    st.part::<super::Memory>().changed.notify_one();
    get(State(st), Admin(user)).await
}
