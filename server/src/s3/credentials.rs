//! Bucket ownership and keys are checked again on every operation, including presigned requests.

use serde::Serialize;
use sqlx::SqliteConnection;
use thirtys3_core::{S3Error, s3_error::code};
use thirtys3_server::{Credential, Ctx};

use super::{S3Result, failed};
use crate::{
    auth::{self, User},
    error::{AppError, AppResult},
    state::AppState,
    tree::{self, Node, Role},
    util::now,
};

#[derive(Clone, Serialize, sqlx::FromRow)]
pub struct Bucket {
    pub id: String,
    pub name: String,
    pub node_id: String,
    pub creator_id: i64,
    pub enabled: bool,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

#[derive(Clone, Serialize, sqlx::FromRow)]
pub struct Key {
    pub id: String,
    pub bucket_id: String,
    pub access_key: String,
    #[serde(skip)]
    pub secret: String,
    pub description: String,
    pub writable: bool,
    pub enabled: bool,
    pub expires_at: Option<i64>,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

#[derive(Clone)]
pub struct Identity {
    pub bucket: Bucket,
    pub key: Key,
    pub user: User,
    pub root: Node,
}

/// The folder `node_id`, when `user` may publish it as a bucket (`crate::publish::publisher`): administrators can
/// publish team spaces even without a membership grant; personal spaces never become buckets
pub async fn publisher(conn: &mut SqliteConnection, user: &User, node_id: &str, managers: bool) -> AppResult<Node> {
    crate::publish::publisher(conn, user, node_id, crate::publish::Publication::S3Bucket, managers).await
}

pub async fn identity(st: &AppState, access_key: &str, write: bool) -> S3Result<Identity> {
    let cfg = super::config::load(st).await.map_err(super::map_error)?;
    if !cfg.enabled {
        return Err(failed(code::ACCESS_DENIED, "S3 access is disabled"));
    }
    let mut conn = st.db.acquire().await.map_err(super::internal)?;
    let key: Key = sqlx::query_as("SELECT * FROM s3_keys WHERE access_key = ? AND enabled = 1 AND (expires_at IS NULL OR expires_at > ?)")
        .bind(access_key)
        .bind(now())
        .fetch_optional(&mut *conn)
        .await
        .map_err(super::internal)?
        .ok_or_else(|| failed(code::INVALID_ACCESS_KEY_ID, "The AWS Access Key Id you provided does not exist in our records."))?;
    if write && !key.writable {
        return Err(failed(code::ACCESS_DENIED, "This Access Key is read-only"));
    }
    let bucket: Bucket = sqlx::query_as("SELECT * FROM s3_buckets WHERE id = ? AND enabled = 1")
        .bind(&key.bucket_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(super::internal)?
        .ok_or_else(|| failed(code::ACCESS_DENIED, "Access Denied"))?;
    let mut user =
        auth::user_by_id(st, &mut conn, bucket.creator_id).await.map_err(super::map_error)?.ok_or_else(|| failed(code::ACCESS_DENIED, "Access Denied"))?;
    let root = publisher(&mut conn, &user, &bucket.node_id, cfg.managers_publish).await.map_err(super::map_error)?;
    if write && root.space_read_only {
        return Err(failed(code::ACCESS_DENIED, "This space is read-only"));
    }
    user.protocol_key = Some(key.id.clone());
    user.protocol_root = Some(root.id.clone());
    user.activity_source = Some(format!("S3 bucket {}, key {}", bucket.name, key.description));
    Ok(Identity { bucket, key, user, root })
}

/// Who a request is for, from the key that signed it: `bucket`, when the request names one, must be the key's own
pub async fn request_identity(st: &AppState, ctx: &Ctx<'_>, bucket: Option<&str>, write: bool) -> S3Result<Identity> {
    let identity = identity(st, ctx.access_key, write).await?;
    if let Some(expiry) = identity.key.expires_at
        && ctx.presigned
    {
        let values = ctx.uri.query().map(|q| form_urlencoded::parse(q.as_bytes()).collect::<std::collections::HashMap<_, _>>()).unwrap_or_default();
        let date = values.get("X-Amz-Date").ok_or_else(|| failed(code::ACCESS_DENIED, "Access Denied"))?;
        let seconds: i64 = values.get("X-Amz-Expires").and_then(|v| v.parse().ok()).ok_or_else(|| failed(code::ACCESS_DENIED, "Access Denied"))?;
        if date.len() != 16 || !date.is_ascii() {
            return Err(failed(code::ACCESS_DENIED, "Access Denied"));
        }
        let date = format!("{}-{}-{}T{}:{}:{}Z", &date[0..4], &date[4..6], &date[6..8], &date[9..11], &date[11..13], &date[13..15]);
        let date = date.parse::<jiff::Timestamp>().map_err(|_| failed(code::ACCESS_DENIED, "Access Denied"))?;
        if date.as_second().checked_add(seconds).is_none_or(|until| until > expiry) {
            return Err(failed(code::ACCESS_DENIED, "The presigned URL outlives this Access Key"));
        }
    }
    if bucket.is_some_and(|name| name != identity.bucket.name) {
        return Err(failed(code::ACCESS_DENIED, "Access Denied"));
    }
    // When they were last used, at most every five minutes: bookkeeping, which never fails the request (the database
    // may be busy with a long change)
    let ts = now();
    for (table, id) in [("s3_keys", &identity.key.id), ("s3_buckets", &identity.bucket.id)] {
        let sql = format!("UPDATE {table} SET last_used_at = ? WHERE id = ? AND (last_used_at IS NULL OR last_used_at < ?)");
        if let Err(e) = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(ts).bind(id).bind(ts - 300).execute(&st.db).await {
            tracing::debug!("Couldn't note when an S3 key was used: {e}");
        }
    }
    Ok(identity)
}

/// The secret of an Access Key. `Ok(None)` is a key nobody has (a guess, which counts as a failed sign-in); an error is a key that exists and is
/// refused (stopped, ended, its bucket or creator no longer allowed), which is no guess and isn't counted
pub async fn secret(st: &AppState, access_key: &str) -> Result<Option<Credential>, S3Error> {
    match identity(st, access_key, false).await {
        Ok(identity) => {
            crate::secrets::open(&format!("s3-key:{}", identity.key.id), &identity.key.secret).map(|secret| Some(Credential::new(secret))).map_err(super::internal)
        }
        Err(e) if e.code == code::INVALID_ACCESS_KEY_ID => {
            let mut conn = st.db.acquire().await.map_err(super::internal)?;
            let (known,): (bool,) =
                sqlx::query_as("SELECT EXISTS(SELECT 1 FROM s3_keys WHERE access_key = ?)").bind(access_key).fetch_one(&mut *conn).await.map_err(super::internal)?;
            if known { Err(e) } else { Ok(None) }
        }
        Err(e) => Err(e),
    }
}

/// Runs in the caller's transaction, including the final upload commit. Revoking a key, creator or share permission
/// while content is arriving therefore prevents the write, without a second connection outside that transaction.
pub async fn actor_in_transaction(conn: &mut SqliteConnection, actor: &User, need: tree::Need) -> AppResult<User> {
    let id = actor.protocol_key.as_deref().ok_or_else(|| AppError::forbidden("S3 credentials are missing"))?;
    let key: Key = sqlx::query_as("SELECT * FROM s3_keys WHERE id=? AND enabled=1 AND (expires_at IS NULL OR expires_at>?)")
        .bind(id)
        .bind(now())
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| AppError::forbidden("The S3 key is no longer active"))?;
    let bucket: Bucket = sqlx::query_as("SELECT * FROM s3_buckets WHERE id=? AND enabled=1")
        .bind(&key.bucket_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| AppError::forbidden("The S3 bucket is no longer active"))?;
    if actor.id != bucket.creator_id || actor.protocol_root.as_deref() != Some(&bucket.node_id) {
        return Err(AppError::forbidden("S3 credentials changed"));
    }
    let sql = format!("SELECT {} FROM users u WHERE u.id=? AND u.disabled=0", auth::USER_COLS);
    let mut current: User = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
        .bind(actor.id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| AppError::forbidden("The bucket's creator no longer has access"))?;
    let cfg = super::config::from_connection(conn).await?;
    if !cfg.enabled {
        return Err(AppError::forbidden("S3 access is disabled"));
    }
    let root = publisher(conn, &current, &bucket.node_id, cfg.managers_publish).await?;
    if matches!(need, tree::Need::Add | tree::Need::Write | tree::Need::Delete) {
        if !key.writable || crate::publish::may_receive(&current, &root).is_err() {
            return Err(AppError::forbidden("This S3 key or space is read-only"));
        }
        tree::allows(&current, Role::Manager, need)?;
    }
    current.protocol_root = actor.protocol_root.clone();
    current.protocol_key = actor.protocol_key.clone();
    current.activity_source = actor.activity_source.clone();
    Ok(current)
}
