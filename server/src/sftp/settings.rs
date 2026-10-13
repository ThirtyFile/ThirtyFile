use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use crate::{admin::Admin, error::{AppError, AppResult}, state::AppState};

#[derive(Serialize)]
pub struct Info { pub enabled: bool, pub keys_enabled: bool, pub address: String, pub fingerprint: String }

pub async fn flag(st: &AppState, key: &str) -> AppResult<bool> {
    let value: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key = ?").bind(key).fetch_optional(&st.db).await?;
    Ok(value.is_some_and(|(v,)| v == "1"))
}

pub async fn get(State(st): State<AppState>, _: Admin) -> AppResult<Json<Info>> {
    let memory = st.part::<super::Memory>();
    Ok(Json(Info { enabled: flag(&st, "sftp_enabled").await?, keys_enabled: flag(&st, "sftp_keys_enabled").await?, address: memory.address.read().clone(), fingerprint: memory.fingerprint.read().clone() }))
}

#[derive(Deserialize)]
pub struct Change { enabled: bool, keys_enabled: bool }

pub async fn update(State(st): State<AppState>, Admin(user): Admin, Json(req): Json<Change>) -> AppResult<Json<Info>> {
    if req.enabled && st.part::<super::Memory>().fingerprint.read().is_empty() { return Err(AppError::conflict("The SFTP listener is unavailable. Check the server log.")); }
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    for (key, value) in [("sftp_enabled", req.enabled), ("sftp_keys_enabled", req.keys_enabled)] {
        sqlx::query("INSERT INTO settings(key, value) VALUES(?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value").bind(key).bind(if value { "1" } else { "0" }).execute(&mut *tx).await?;
    }
    crate::logs::record_activity(&mut tx, &user, None, "settings", &format!("SFTP: {}; SSH keys: {}", req.enabled, req.keys_enabled)).await?;
    tx.commit().await?;
    drop(_w);
    get(State(st), Admin(user)).await
}
