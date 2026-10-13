//! Public keys belong to an account and can be revoked without waiting for SSH sessions to end.

use axum::{
    Json,
    extract::{Path, State},
};
use russh::keys::{PublicKey, ssh_key::HashAlg};
use serde::{Deserialize, Serialize};

use crate::{
    auth::User,
    error::{AppError, AppResult},
    state::AppState,
    util::{new_id, now},
};

#[derive(Serialize, sqlx::FromRow)]
pub struct KeyInfo {
    id: String,
    name: String,
    fingerprint: String,
    created_at: i64,
    last_used_at: Option<i64>,
}

#[derive(Deserialize)]
pub struct AddKey {
    name: String,
    public_key: String,
    password: Option<String>,
    code: Option<String>,
}

pub async fn list(State(st): State<AppState>, user: User) -> AppResult<Json<Vec<KeyInfo>>> {
    Ok(Json(
        sqlx::query_as("SELECT id, name, fingerprint, created_at, last_used_at FROM sftp_keys WHERE user_id = ? ORDER BY created_at DESC")
            .bind(user.id)
            .fetch_all(&st.db)
            .await?,
    ))
}

pub async fn add(State(st): State<AppState>, user: User, Json(req): Json<AddKey>) -> AppResult<Json<KeyInfo>> {
    crate::tokens::confirm_identity(&st, &user, req.password, req.code.as_deref(), "Sign out and sign in again, then add the SSH key within 10 minutes").await?;
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(AppError::bad_request("Enter a key name (up to 80 characters)"));
    }
    if req.public_key.len() > 16_384 {
        return Err(AppError::bad_request("The SSH public key is too long"));
    }
    let public = PublicKey::from_openssh(req.public_key.trim()).map_err(|_| AppError::bad_request("Enter an SSH public key, not a private key"))?;
    let fingerprint = public.fingerprint(HashAlg::Sha256).to_string();
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sftp_keys WHERE user_id = ?").bind(user.id).fetch_one(&mut *tx).await?;
    if count >= 10 {
        return Err(AppError::bad_request("Remove an SSH key before adding another (limit: 10)"));
    }
    let (exists,): (bool,) =
        sqlx::query_as("SELECT EXISTS(SELECT 1 FROM sftp_keys WHERE user_id = ? AND fingerprint = ?)").bind(user.id).bind(&fingerprint).fetch_one(&mut *tx).await?;
    if exists {
        return Err(AppError::conflict("This SSH key has already been added"));
    }
    let info = KeyInfo { id: new_id(), name: name.to_string(), fingerprint, created_at: now(), last_used_at: None };
    sqlx::query("INSERT INTO sftp_keys(id, user_id, name, public_key, fingerprint, created_at) VALUES(?, ?, ?, ?, ?, ?)")
        .bind(&info.id)
        .bind(user.id)
        .bind(name)
        .bind(public.to_openssh().map_err(AppError::internal)?)
        .bind(&info.fingerprint)
        .bind(info.created_at)
        .execute(&mut *tx)
        .await?;
    crate::logs::record_activity(&mut tx, &user, None, "settings", &format!("Added SFTP key: {name}")).await?;
    tx.commit().await?;
    Ok(Json(info))
}

pub async fn remove(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<Json<serde_json::Value>> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let name: Option<(String,)> =
        sqlx::query_as("DELETE FROM sftp_keys WHERE id = ? AND user_id = ? RETURNING name").bind(id).bind(user.id).fetch_optional(&mut *tx).await?;
    let (name,) = name.ok_or_else(|| AppError::not_found("SSH key not found"))?;
    crate::logs::record_activity(&mut tx, &user, None, "settings", &format!("Removed SFTP key: {name}")).await?;
    tx.commit().await?;
    Ok(Json(serde_json::json!({"ok": true})))
}
