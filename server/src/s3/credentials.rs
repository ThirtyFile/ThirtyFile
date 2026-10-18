//! Bucket ownership and keys are checked again on every operation, including presigned requests.

use s3s::{
    S3Request, S3Result,
    auth::{S3Auth, SecretKey},
    s3_error,
};
use serde::Serialize;
use sqlx::SqliteConnection;

use crate::{
    auth::{self, User},
    error::{AppError, AppResult},
    state::AppState,
    tree::{self, LinkMakers, Node, Role, SpaceKind},
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

pub struct Identity {
    pub bucket: Bucket,
    pub key: Key,
    pub user: User,
    pub root: Node,
}

/// Administrators can publish team spaces even without a membership grant; personal spaces never become buckets.
pub async fn publisher(conn: &mut SqliteConnection, user: &User, node_id: &str, managers: bool) -> AppResult<Node> {
    let current = auth::current_user(conn, user).await?;
    let user = &current;
    if user.guest {
        return Err(AppError::forbidden("Guests can't publish protocol folders"));
    }
    let node = tree::get_node(conn, node_id).await?.ok_or_else(|| AppError::not_found("Folder not found"))?;
    let drive = tree::get_drive(conn, node.drive()).await?.ok_or_else(|| AppError::not_found("Space not found"))?;
    if !node.is_folder() || drive.kind == SpaceKind::Personal || drive.disabled || drive.link_makers == LinkMakers::Nobody {
        return Err(AppError::forbidden("This folder can't be published as an S3 bucket"));
    }
    let (in_trash,): (bool,) = sqlx::query_as("WITH RECURSIVE up(id, parent_id, trash) AS (SELECT id, parent_id, trashed_at IS NOT NULL FROM nodes WHERE id = ? UNION ALL SELECT n.id, n.parent_id, n.trashed_at IS NOT NULL FROM nodes n JOIN up ON n.id = up.parent_id) SELECT COALESCE(MAX(trash), 1) FROM up")
        .bind(&node.id).fetch_one(&mut *conn).await?;
    if in_trash {
        return Err(AppError::not_found("Folder not found"));
    }
    if !user.is_admin() && (!managers || !user.can_share || !tree::live_role_on(conn, user, &node).await?.is_some_and(|r| r >= Role::Manager)) {
        return Err(AppError::forbidden("Only allowed space managers can publish S3 buckets"));
    }
    Ok(node)
}

pub async fn identity(st: &AppState, access_key: &str, write: bool) -> S3Result<Identity> {
    let cfg = super::config::load(st).await.map_err(super::map_error)?;
    if !cfg.enabled {
        return Err(s3_error!(AccessDenied, "S3 access is disabled"));
    }
    let mut conn = st.db.acquire().await.map_err(super::internal)?;
    let key: Key = sqlx::query_as("SELECT * FROM s3_keys WHERE access_key = ? AND enabled = 1 AND (expires_at IS NULL OR expires_at > ?)")
        .bind(access_key)
        .bind(now())
        .fetch_optional(&mut *conn)
        .await
        .map_err(super::internal)?
        .ok_or_else(|| s3_error!(InvalidAccessKeyId))?;
    if write && !key.writable {
        return Err(s3_error!(AccessDenied, "This Access Key is read-only"));
    }
    let bucket: Bucket = sqlx::query_as("SELECT * FROM s3_buckets WHERE id = ? AND enabled = 1")
        .bind(&key.bucket_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(super::internal)?
        .ok_or_else(|| s3_error!(AccessDenied))?;
    let mut user = auth::user_by_id(st, &mut conn, bucket.creator_id).await.map_err(super::map_error)?.ok_or_else(|| s3_error!(AccessDenied))?;
    let root = publisher(&mut conn, &user, &bucket.node_id, cfg.managers_publish).await.map_err(super::map_error)?;
    if write && root.space_read_only {
        return Err(s3_error!(AccessDenied, "This space is read-only"));
    }
    user.protocol_key = Some(key.id.clone());
    user.protocol_root = Some(root.id.clone());
    user.activity_source = Some(format!("S3 bucket {}, key {}", bucket.name, key.description));
    Ok(Identity { bucket, key, user, root })
}

pub async fn request_identity<T>(st: &AppState, req: &S3Request<T>, bucket: Option<&str>, write: bool) -> S3Result<Identity> {
    let creds = req.credentials.as_ref().ok_or_else(|| s3_error!(AccessDenied))?;
    let identity = identity(st, &creds.access_key, write).await?;
    if let Some(expiry) = identity.key.expires_at {
        let values = req.uri.query().map(|q| form_urlencoded::parse(q.as_bytes()).collect::<std::collections::HashMap<_, _>>()).unwrap_or_default();
        if values.contains_key("X-Amz-Algorithm") {
            let date = values.get("X-Amz-Date").ok_or_else(|| s3_error!(AccessDenied))?;
            let seconds: i64 = values.get("X-Amz-Expires").and_then(|v| v.parse().ok()).ok_or_else(|| s3_error!(AccessDenied))?;
            if date.len() != 16 || !date.is_ascii() {
                return Err(s3_error!(AccessDenied));
            }
            let date = format!("{}-{}-{}T{}:{}:{}Z", &date[0..4], &date[4..6], &date[6..8], &date[9..11], &date[11..13], &date[13..15]);
            let date = date.parse::<jiff::Timestamp>().map_err(|_| s3_error!(AccessDenied))?;
            if date.as_second().checked_add(seconds).is_none_or(|until| until > expiry) {
                return Err(s3_error!(AccessDenied, "The presigned URL outlives this Access Key"));
            }
        }
    }
    if bucket.is_some_and(|name| name != identity.bucket.name) {
        return Err(s3_error!(AccessDenied));
    }
    let ts = now();
    sqlx::query("UPDATE s3_keys SET last_used_at = ? WHERE id = ? AND (last_used_at IS NULL OR last_used_at < ?)")
        .bind(ts)
        .bind(&identity.key.id)
        .bind(ts - 300)
        .execute(&st.db)
        .await
        .map_err(super::internal)?;
    sqlx::query("UPDATE s3_buckets SET last_used_at = ? WHERE id = ? AND (last_used_at IS NULL OR last_used_at < ?)")
        .bind(ts)
        .bind(&identity.bucket.id)
        .bind(ts - 300)
        .execute(&st.db)
        .await
        .map_err(super::internal)?;
    Ok(identity)
}

/// `.1` is set when the Access Key exists but is refused (stopped, ended, its bucket or creator no longer allowed):
/// that is no guess, and isn't counted as a failed sign-in
pub struct Auth(pub AppState, pub std::sync::Arc<std::sync::atomic::AtomicBool>);

#[async_trait::async_trait]
impl S3Auth for Auth {
    async fn get_secret_key(&self, access_key: &str) -> S3Result<SecretKey> {
        let identity = match identity(&self.0, access_key, false).await {
            Ok(identity) => identity,
            Err(e) => {
                let mut conn = self.0.db.acquire().await.map_err(super::internal)?;
                let (known,): (bool,) = sqlx::query_as("SELECT EXISTS(SELECT 1 FROM s3_keys WHERE access_key = ?)")
                    .bind(access_key)
                    .fetch_one(&mut *conn)
                    .await
                    .map_err(super::internal)?;
                if known {
                    self.1.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                return Err(e);
            }
        };
        crate::secrets::open(&format!("s3-key:{}", identity.key.id), &identity.key.secret).map(SecretKey::from).map_err(super::internal)
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
    let cfg: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key='s3'").fetch_optional(&mut *conn).await?;
    let cfg: super::config::Config = cfg.and_then(|(s,)| serde_json::from_str(&s).ok()).unwrap_or_default();
    if !cfg.enabled {
        return Err(AppError::forbidden("S3 access is disabled"));
    }
    let root = publisher(conn, &current, &bucket.node_id, cfg.managers_publish).await?;
    if matches!(need, tree::Need::Add | tree::Need::Write | tree::Need::Delete) {
        if !key.writable || root.space_read_only {
            return Err(AppError::forbidden("This S3 key or space is read-only"));
        }
        tree::allows(&current, Role::Manager, need)?;
    }
    current.protocol_root = actor.protocol_root.clone();
    current.protocol_key = actor.protocol_key.clone();
    current.activity_source = actor.activity_source.clone();
    Ok(current)
}
