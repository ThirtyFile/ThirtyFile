//! The passkeys kept for accounts

use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;

use crate::{error::AppResult, util::now};

/// Passkeys one account may have
pub const MAX_PER_ACCOUNT: i64 = 20;
/// The ways a device can be reached that the standard names; anything else a browser sends is dropped
const TRANSPORTS: [&str; 6] = ["usb", "nfc", "ble", "internal", "hybrid", "smart-card"];

/// What identifies an account to its passkeys (the "user handle"): not a name or an address, as the standard asks, and
/// the same every time
pub fn user_handle(user_id: i64) -> Vec<u8> {
    Sha256::digest(format!("thirtyfile:passkey-user:{user_id}")).to_vec()
}

#[derive(Serialize, sqlx::FromRow)]
pub struct Row {
    pub id: i64,
    pub name: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    /// Kept in a phone's keychain or a password manager, so it is on more than one device
    pub backup_eligible: bool,
}

pub async fn list(conn: &mut SqliteConnection, user_id: i64) -> AppResult<Vec<Row>> {
    Ok(sqlx::query_as("SELECT id, name, created_at, last_used_at, backup_eligible FROM passkeys WHERE user_id = ? ORDER BY created_at, id")
        .bind(user_id)
        .fetch_all(conn)
        .await?)
}

pub async fn count(conn: &mut SqliteConnection, user_id: i64) -> AppResult<i64> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM passkeys WHERE user_id = ?").bind(user_id).fetch_one(conn).await?)
}

/// The credential ids of an account, with how their devices are reached
pub async fn credentials(conn: &mut SqliteConnection, user_id: i64) -> AppResult<Vec<(Vec<u8>, Vec<String>)>> {
    let rows: Vec<(Vec<u8>, String)> =
        sqlx::query_as("SELECT credential_id, transports FROM passkeys WHERE user_id = ? ORDER BY id").bind(user_id).fetch_all(conn).await?;
    Ok(rows.into_iter().map(|(id, transports)| (id, serde_json::from_str(&transports).unwrap_or_default())).collect())
}

/// A passkey found by what the device calls it
pub struct Found {
    pub id: i64,
    pub user_id: i64,
    pub public_key: Vec<u8>,
    pub counter: u32,
}

pub async fn find(conn: &mut SqliteConnection, credential_id: &[u8]) -> AppResult<Option<Found>> {
    let row: Option<(i64, i64, Vec<u8>, i64)> =
        sqlx::query_as("SELECT id, user_id, public_key, sign_count FROM passkeys WHERE credential_id = ?").bind(credential_id).fetch_optional(conn).await?;
    Ok(row.map(|(id, user_id, public_key, counter)| Found { id, user_id, public_key, counter: u32::try_from(counter).unwrap_or(u32::MAX) }))
}

/// Keeps a new passkey; false when a passkey with this credential id exists (already added, or someone else's)
pub async fn insert(conn: &mut SqliteConnection, user_id: i64, name: &str, reg: &super::webauthn::Registered, transports: &[String]) -> AppResult<bool> {
    let transports: Vec<&str> = TRANSPORTS.iter().copied().filter(|t| transports.iter().any(|s| s == t)).collect();
    let res = sqlx::query(
        "INSERT OR IGNORE INTO passkeys (user_id, credential_id, name, public_key, sign_count, transports, backup_eligible, backup_state, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(user_id)
    .bind(&reg.credential_id)
    .bind(name)
    .bind(&reg.public_key)
    .bind(i64::from(reg.counter))
    .bind(serde_json::to_string(&transports).unwrap_or_else(|_| "[]".into()))
    .bind(reg.backup_eligible)
    .bind(reg.backup_state)
    .bind(now())
    .execute(conn)
    .await?;
    Ok(res.rows_affected() == 1)
}

/// Moves the counter on after a sign-in, only if nobody else did meanwhile (two sign-ins with the same answer: one wins)
pub async fn used(conn: &mut SqliteConnection, id: i64, was: u32, counter: u32, backup_state: bool) -> AppResult<bool> {
    let res = sqlx::query("UPDATE passkeys SET sign_count = ?, backup_state = ?, last_used_at = ? WHERE id = ? AND sign_count = ?")
        .bind(i64::from(counter))
        .bind(backup_state)
        .bind(now())
        .bind(id)
        .bind(i64::from(was))
        .execute(conn)
        .await?;
    Ok(res.rows_affected() == 1)
}

pub async fn rename(conn: &mut SqliteConnection, user_id: i64, id: i64, name: &str) -> AppResult<bool> {
    let res = sqlx::query("UPDATE passkeys SET name = ? WHERE id = ? AND user_id = ?").bind(name).bind(id).bind(user_id).execute(conn).await?;
    Ok(res.rows_affected() == 1)
}

pub async fn delete(conn: &mut SqliteConnection, user_id: i64, id: i64) -> AppResult<bool> {
    let res = sqlx::query("DELETE FROM passkeys WHERE id = ? AND user_id = ?").bind(id).bind(user_id).execute(conn).await?;
    Ok(res.rows_affected() == 1)
}

/// Removes every passkey of an account (a lost device: an administrator resets its second factors)
pub async fn delete_all(conn: &mut SqliteConnection, user_id: i64) -> AppResult<u64> {
    Ok(sqlx::query("DELETE FROM passkeys WHERE user_id = ?").bind(user_id).execute(conn).await?.rows_affected())
}
