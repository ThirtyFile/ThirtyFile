//! Browser-session management. The wire API never creates or changes bucket credentials.

use super::credentials::{Bucket, Key, publisher};
use crate::{
    auth::User,
    error::{AppError, AppResult},
    state::AppState,
    util::{new_id, now, random_token},
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::{Deserialize, Serialize};

#[derive(Default, Deserialize)]
pub struct ListQuery {
    pub node_id: Option<String>,
    pub search: Option<String>,
}

#[derive(Serialize)]
pub struct BucketInfo {
    #[serde(flatten)]
    pub bucket: Bucket,
    pub creator: String,
    pub folder: String,
    pub keys: Vec<Key>,
    pub active: bool,
}

#[derive(Serialize)]
pub struct FolderBuckets {
    pub can_publish: bool,
    pub buckets: Vec<BucketInfo>,
}

pub async fn list(State(st): State<AppState>, user: User, Query(q): Query<ListQuery>) -> AppResult<Json<FolderBuckets>> {
    let config = super::config::load(&st).await?;
    let mut conn = st.db.acquire().await?;
    let can_manage = if let Some(node) = &q.node_id { publisher(&mut conn, &user, node, config.managers_publish).await.is_ok() } else { false };
    let can_publish = can_manage && config.enabled;
    if q.node_id.is_none() && !user.is_admin() {
        return Err(AppError::forbidden("Administrator permission required"));
    }
    // Someone seeing a folder without permission to publish sees no keys or credential descriptions.
    if q.node_id.is_some() && !can_manage {
        return Ok(Json(FolderBuckets { can_publish, buckets: vec![] }));
    }
    let buckets: Vec<Bucket> =
        // The bucket's name, its creator's or its folder's
        sqlx::query_as(
            "SELECT * FROM s3_buckets b WHERE (?1 IS NULL OR b.node_id = ?1)
               AND (?2 IS NULL OR instr(b.name, ?2) > 0
                 OR EXISTS(SELECT 1 FROM users u WHERE u.id = b.creator_id AND instr(unicode_lower(u.username), ?2) > 0)
                 OR EXISTS(SELECT 1 FROM nodes n WHERE n.id = b.node_id AND instr(unicode_lower(n.name), ?2) > 0))
             ORDER BY b.created_at DESC LIMIT 1000",
        )
        .bind(q.node_id)
        .bind(q.search.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_lowercase))
            .fetch_all(&mut *conn)
            .await?;
    let mut rows = Vec::new();
    for bucket in buckets {
        let creator = crate::auth::user_by_id(&st, &mut conn, bucket.creator_id).await?;
        let active = config.enabled
            && bucket.enabled
            && match &creator {
                Some(u) => publisher(&mut conn, u, &bucket.node_id, config.managers_publish).await.is_ok(),
                None => false,
            };
        let folder = crate::tree::get_node(&mut conn, &bucket.node_id).await?.map(|n| n.name).unwrap_or_default();
        let keys = sqlx::query_as("SELECT * FROM s3_keys WHERE bucket_id = ? ORDER BY created_at DESC").bind(&bucket.id).fetch_all(&mut *conn).await?;
        rows.push(BucketInfo { bucket, creator: creator.map(|u| u.username).unwrap_or_default(), folder, keys, active });
    }
    Ok(Json(FolderBuckets { can_publish, buckets: rows }))
}

#[derive(Deserialize)]
pub struct Publish {
    pub node_id: String,
    pub name: String,
    /// Who is publishing, asked again (`tokens::confirm_identity`)
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
}

/// Credentials outlast the browser session that makes them: who is making them is asked again, as for app passwords
async fn confirm(st: &AppState, user: &User, password: Option<String>, code: Option<&str>) -> AppResult<()> {
    crate::tokens::confirm_identity(st, user, password, code, "Sign out and sign in again, then manage S3 keys within 10 minutes").await
}

/// Records a change to a bucket on its folder, so the entry follows the folder's access
async fn record(conn: &mut sqlx::SqliteConnection, user: &User, bucket: &Bucket, detail: &str) -> AppResult<()> {
    let folder = crate::tree::get_node(conn, &bucket.node_id).await?;
    crate::logs::record_activity(conn, user, folder.as_ref().map(|n| n as _), "settings", detail).await
}

pub fn bucket_name(name: &str) -> bool {
    (3..=63).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name.parse::<std::net::IpAddr>().is_err()
        && !name.starts_with("xn--")
        && !name.ends_with("-s3alias")
        && !name.ends_with("--ol-s3")
        && !name.ends_with("--x-s3")
        && !name.ends_with("--table-s3")
}

pub async fn create(State(st): State<AppState>, user: User, Json(req): Json<Publish>) -> AppResult<Json<Bucket>> {
    let config = super::config::load(&st).await?;
    if !config.enabled {
        return Err(AppError::forbidden("S3 access is disabled"));
    }
    if !bucket_name(&req.name) {
        return Err(AppError::bad_request("Use 3–63 lowercase letters, digits and hyphens for the bucket name"));
    }
    confirm(&st, &user, req.password.clone(), req.code.as_deref()).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let current = crate::auth::user_by_id(&st, &mut tx, user.id).await?.ok_or_else(|| AppError::forbidden("The account no longer has access"))?;
    let config = super::config::from_connection(&mut tx).await?;
    if !config.enabled {
        return Err(AppError::forbidden("S3 access is disabled"));
    }
    publisher(&mut tx, &current, &req.node_id, config.managers_publish).await?;
    let bucket = Bucket { id: new_id(), name: req.name, node_id: req.node_id, creator_id: user.id, enabled: true, created_at: now(), last_used_at: None };
    let result = sqlx::query("INSERT INTO s3_buckets (id, name, node_id, creator_id, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(&bucket.id)
        .bind(&bucket.name)
        .bind(&bucket.node_id)
        .bind(user.id)
        .bind(bucket.created_at)
        .execute(&mut *tx)
        .await;
    if result.as_ref().is_err_and(|e| e.as_database_error().is_some_and(|e| e.is_unique_violation())) {
        return Err(AppError::conflict("An S3 bucket has this name"));
    }
    result?;
    record(&mut tx, &current, &bucket, &format!("Published S3 bucket {}", bucket.name)).await?;
    tx.commit().await?;
    Ok(Json(bucket))
}

async fn managed(st: &AppState, conn: &mut sqlx::SqliteConnection, user: &User, id: &str) -> AppResult<Bucket> {
    let current = crate::auth::user_by_id(st, conn, user.id).await?.ok_or_else(|| AppError::forbidden("The account no longer has access"))?;
    let config = super::config::from_connection(conn).await?;
    let bucket: Bucket =
        sqlx::query_as("SELECT * FROM s3_buckets WHERE id = ?").bind(id).fetch_optional(&mut *conn).await?.ok_or_else(|| AppError::not_found("Bucket not found"))?;
    if !current.is_admin() {
        publisher(conn, &current, &bucket.node_id, config.managers_publish).await?;
    }
    Ok(bucket)
}

#[derive(Deserialize)]
pub struct Enabled {
    pub enabled: bool,
}

pub async fn update(State(st): State<AppState>, user: User, Path(id): Path<String>, Json(req): Json<Enabled>) -> AppResult<Json<serde_json::Value>> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let bucket = managed(&st, &mut tx, &user, &id).await?;
    let current = crate::auth::current_user(&mut tx, &user).await?;
    if req.enabled && !bucket.enabled {
        crate::tokens::may_hand_out(&current, bucket.creator_id)?;
    }
    sqlx::query("UPDATE s3_buckets SET enabled = ? WHERE id = ?").bind(req.enabled).bind(id).execute(&mut *tx).await?;
    if req.enabled != bucket.enabled {
        let detail = if req.enabled { format!("Turned S3 bucket {} on", bucket.name) } else { format!("Stopped S3 bucket {}", bucket.name) };
        record(&mut tx, &current, &bucket, &detail).await?;
    }
    tx.commit().await?;
    super::multipart::cleanup_later(st.clone());
    Ok(Json(serde_json::json!({"ok": true})))
}

pub async fn delete(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<Json<serde_json::Value>> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let bucket = managed(&st, &mut tx, &user, &id).await?;
    let current = crate::auth::current_user(&mut tx, &user).await?;
    record(&mut tx, &current, &bucket, &format!("Removed S3 bucket {}", bucket.name)).await?;
    sqlx::query("DELETE FROM s3_buckets WHERE id = ?").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    super::multipart::cleanup_later(st.clone());
    Ok(Json(serde_json::json!({"ok": true})))
}

#[derive(Deserialize)]
pub struct NewKey {
    pub description: String,
    pub writable: bool,
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
}

#[derive(Serialize)]
pub struct CreatedKey {
    #[serde(flatten)]
    pub key: Key,
    pub secret_key: String,
}

pub async fn new_key(State(st): State<AppState>, user: User, Path(bucket_id): Path<String>, Json(req): Json<NewKey>) -> AppResult<Json<CreatedKey>> {
    confirm(&st, &user, req.password.clone(), req.code.as_deref()).await?;
    let _w = st.write_lock.lock().await;
    let mut conn = crate::db::begin_write(&st.db).await?;
    let bucket = managed(&st, &mut conn, &user, &bucket_id).await?;
    let current = crate::auth::current_user(&mut conn, &user).await?;
    crate::tokens::may_hand_out(&current, bucket.creator_id)?;
    let config = super::config::from_connection(&mut conn).await?;
    let creator =
        crate::auth::user_by_id(&st, &mut conn, bucket.creator_id).await?.ok_or_else(|| AppError::forbidden("The bucket's creator no longer has access"))?;
    let root = publisher(&mut conn, &creator, &bucket.node_id, config.managers_publish).await?;
    if req.writable {
        crate::publish::may_receive(&creator, &root)?;
    }
    if req.description.trim().is_empty() || req.description.len() > 160 || req.description.chars().any(char::is_control) {
        return Err(AppError::bad_request("Enter a key description of at most 160 characters"));
    }
    if req.expires_at.is_some_and(|t| t <= now()) {
        return Err(AppError::bad_request("The key's end date must be in the future"));
    }
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM s3_keys WHERE bucket_id = ?").bind(&bucket_id).fetch_one(&mut *conn).await?;
    if count >= 100 {
        return Err(AppError::bad_request("This bucket already has 100 keys"));
    }
    let id = new_id();
    let secret_key = random_token(40);
    let key = Key {
        id: id.clone(),
        bucket_id,
        access_key: format!("TF{}", hex::encode(rand::random::<[u8; 10]>()).to_uppercase()),
        secret: crate::secrets::seal(&format!("s3-key:{id}"), &secret_key),
        description: req.description.trim().into(),
        writable: req.writable,
        enabled: true,
        expires_at: req.expires_at,
        created_at: now(),
        last_used_at: None,
    };
    sqlx::query("INSERT INTO s3_keys (id,bucket_id,access_key,secret,description,writable,expires_at,created_at) VALUES (?,?,?,?,?,?,?,?)")
        .bind(&key.id)
        .bind(&key.bucket_id)
        .bind(&key.access_key)
        .bind(&key.secret)
        .bind(&key.description)
        .bind(key.writable)
        .bind(key.expires_at)
        .bind(key.created_at)
        .execute(&mut *conn)
        .await?;
    let detail = if key.writable {
        format!("Made S3 key {} to read and change bucket {}", key.description, bucket.name)
    } else {
        format!("Made S3 key {} to read bucket {}", key.description, bucket.name)
    };
    record(&mut conn, &current, &bucket, &detail).await?;
    conn.commit().await?;
    Ok(Json(CreatedKey { key, secret_key }))
}

pub async fn revoke_key(
    State(st): State<AppState>,
    user: User,
    Path((bucket, key)): Path<(String, String)>,
    Json(req): Json<Enabled>,
) -> AppResult<Json<serde_json::Value>> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let published = managed(&st, &mut tx, &user, &bucket).await?;
    let current = crate::auth::current_user(&mut tx, &user).await?;
    let found: Key = sqlx::query_as("SELECT * FROM s3_keys WHERE id = ? AND bucket_id = ?")
        .bind(&key)
        .bind(&bucket)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::not_found("Key not found"))?;
    if req.enabled && !found.enabled {
        crate::tokens::may_hand_out(&current, published.creator_id)?;
    }
    sqlx::query("UPDATE s3_keys SET enabled = ? WHERE id = ? AND bucket_id = ?").bind(req.enabled).bind(key).bind(bucket).execute(&mut *tx).await?;
    if req.enabled != found.enabled {
        let detail = if req.enabled { format!("Turned S3 key {} on", found.description) } else { format!("Stopped S3 key {}", found.description) };
        record(&mut tx, &current, &published, &detail).await?;
    }
    tx.commit().await?;
    super::multipart::cleanup_later(st.clone());
    Ok(Json(serde_json::json!({"ok": true})))
}

pub async fn delete_key(State(st): State<AppState>, user: User, Path((bucket, key)): Path<(String, String)>) -> AppResult<Json<serde_json::Value>> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let published = managed(&st, &mut tx, &user, &bucket).await?;
    let current = crate::auth::current_user(&mut tx, &user).await?;
    let found: Option<(String,)> =
        sqlx::query_as("SELECT description FROM s3_keys WHERE id = ? AND bucket_id = ?").bind(&key).bind(&bucket).fetch_optional(&mut *tx).await?;
    sqlx::query("DELETE FROM s3_keys WHERE id = ? AND bucket_id = ?").bind(key).bind(bucket).execute(&mut *tx).await?;
    if let Some((description,)) = found {
        record(&mut tx, &current, &published, &format!("Deleted S3 key {description}")).await?;
    }
    tx.commit().await?;
    super::multipart::cleanup_later(st.clone());
    Ok(Json(serde_json::json!({"ok": true})))
}
