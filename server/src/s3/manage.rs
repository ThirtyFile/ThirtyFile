//! Browser-session management. The wire API never creates or changes bucket credentials.

use axum::{Json, extract::{State, Path, Query}};
use serde::{Deserialize, Serialize};
use crate::{auth::{Admin, User}, error::{AppError, AppResult}, state::AppState, util::{new_id, now, random_token}};
use super::credentials::{Bucket, Key, publisher};

#[derive(Default, Deserialize)]
pub struct ListQuery { pub node_id: Option<String>, pub search: Option<String> }

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
pub struct FolderBuckets { pub can_publish: bool, pub buckets: Vec<BucketInfo> }

pub async fn list(State(st): State<AppState>, user: User, Query(q): Query<ListQuery>) -> AppResult<Json<FolderBuckets>> {
    let config = super::config::load(&st).await?;
    let mut conn = st.db.acquire().await?;
    let can_publish = if let Some(node) = &q.node_id { publisher(&mut conn, &user, node, config.managers_publish).await.is_ok() && config.enabled } else { false };
    if q.node_id.is_none() && !user.is_admin() { return Err(AppError::forbidden("Administrator permission required")); }
    // Someone seeing a folder without permission to publish sees no keys or credential descriptions.
    if q.node_id.is_some() && !can_publish { return Ok(Json(FolderBuckets { can_publish, buckets: vec![] })); }
    let buckets: Vec<Bucket> = sqlx::query_as("SELECT * FROM s3_buckets WHERE (?1 IS NULL OR node_id = ?1) AND (?2 IS NULL OR instr(name, ?2) > 0) ORDER BY created_at DESC LIMIT 1000")
        .bind(q.node_id).bind(q.search).fetch_all(&mut *conn).await?;
    let mut rows = Vec::new();
    for bucket in buckets {
        let creator = crate::auth::user_by_id(&st, &mut conn, bucket.creator_id).await?;
        let active = config.enabled && bucket.enabled && match &creator { Some(u) => publisher(&mut conn, u, &bucket.node_id, config.managers_publish).await.is_ok(), None => false };
        let folder = crate::tree::get_node(&mut conn, &bucket.node_id).await?.map(|n| n.name).unwrap_or_default();
        let keys = sqlx::query_as("SELECT * FROM s3_keys WHERE bucket_id = ? ORDER BY created_at DESC").bind(&bucket.id).fetch_all(&mut *conn).await?;
        rows.push(BucketInfo { bucket, creator: creator.map(|u| u.username).unwrap_or_default(), folder, keys, active });
    }
    Ok(Json(FolderBuckets { can_publish, buckets: rows }))
}

#[derive(Deserialize)]
pub struct Publish { pub node_id: String, pub name: String }

pub fn bucket_name(name: &str) -> bool {
    (3..=63).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-') && !name.ends_with('-') && name.parse::<std::net::IpAddr>().is_err()
        && !name.starts_with("xn--") && !name.ends_with("-s3alias") && !name.ends_with("--ol-s3") && !name.ends_with("--x-s3") && !name.ends_with("--table-s3")
}

pub async fn create(State(st): State<AppState>, user: User, Json(req): Json<Publish>) -> AppResult<Json<Bucket>> {
    let config = super::config::load(&st).await?;
    if !config.enabled { return Err(AppError::forbidden("S3 access is disabled")); }
    if !bucket_name(&req.name) { return Err(AppError::bad_request("Use 3–63 lowercase letters, digits and hyphens for the bucket name")); }
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    publisher(&mut tx, &user, &req.node_id, config.managers_publish).await?;
    let bucket = Bucket { id: new_id(), name: req.name, node_id: req.node_id, creator_id: user.id, enabled: true, created_at: now(), last_used_at: None };
    let result = sqlx::query("INSERT INTO s3_buckets (id, name, node_id, creator_id, created_at) VALUES (?, ?, ?, ?, ?)").bind(&bucket.id).bind(&bucket.name).bind(&bucket.node_id).bind(user.id).bind(bucket.created_at).execute(&mut *tx).await;
    if result.as_ref().is_err_and(|e| e.as_database_error().is_some_and(|e| e.is_unique_violation())) { return Err(AppError::conflict("An S3 bucket has this name")); }
    result?;
    tx.commit().await?;
    Ok(Json(bucket))
}

async fn managed(st: &AppState, user: &User, id: &str) -> AppResult<Bucket> {
    let config = super::config::load(st).await?;
    let mut conn = st.db.acquire().await?;
    let bucket: Bucket = sqlx::query_as("SELECT * FROM s3_buckets WHERE id = ?").bind(id).fetch_optional(&mut *conn).await?.ok_or_else(|| AppError::not_found("Bucket not found"))?;
    if !user.is_admin() { publisher(&mut conn, user, &bucket.node_id, config.managers_publish).await?; }
    Ok(bucket)
}

#[derive(Deserialize)]
pub struct Enabled { pub enabled: bool }

pub async fn update(State(st): State<AppState>, user: User, Path(id): Path<String>, Json(req): Json<Enabled>) -> AppResult<()> {
    managed(&st, &user, &id).await?;
    sqlx::query("UPDATE s3_buckets SET enabled = ? WHERE id = ?").bind(req.enabled).bind(id).execute(&st.db).await?;
    Ok(())
}

pub async fn delete(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<()> {
    managed(&st, &user, &id).await?;
    sqlx::query("DELETE FROM s3_buckets WHERE id = ?").bind(id).execute(&st.db).await?;
    Ok(())
}

#[derive(Deserialize)]
pub struct NewKey { pub description: String, pub writable: bool, pub expires_at: Option<i64> }

#[derive(Serialize)]
pub struct CreatedKey { #[serde(flatten)] pub key: Key, pub secret_key: String }

pub async fn new_key(State(st): State<AppState>, user: User, Path(bucket_id): Path<String>, Json(req): Json<NewKey>) -> AppResult<Json<CreatedKey>> {
    let bucket = managed(&st, &user, &bucket_id).await?;
    let config = super::config::load(&st).await?;
    let mut conn = st.db.acquire().await?;
    let creator = crate::auth::user_by_id(&st, &mut conn, bucket.creator_id).await?.ok_or_else(|| AppError::forbidden("The bucket's creator no longer has access"))?;
    let root = publisher(&mut conn, &creator, &bucket.node_id, config.managers_publish).await?;
    if req.writable && root.space_read_only { return Err(crate::tree::read_only_space()); }
    if req.description.trim().is_empty() || req.description.len() > 160 || req.description.chars().any(char::is_control) { return Err(AppError::bad_request("Enter a key description of at most 160 characters")); }
    if req.expires_at.is_some_and(|t| t <= now()) { return Err(AppError::bad_request("The key's end date must be in the future")); }
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM s3_keys WHERE bucket_id = ?").bind(&bucket_id).fetch_one(&mut *conn).await?;
    if count >= 100 { return Err(AppError::bad_request("This bucket already has 100 keys")); }
    let id = new_id();
    let secret_key = random_token(40);
    let key = Key { id: id.clone(), bucket_id, access_key: format!("TF{}", hex::encode(rand::random::<[u8; 10]>()).to_uppercase()), secret: crate::secrets::seal(&format!("s3-key:{id}"), &secret_key), description: req.description.trim().into(), writable: req.writable, enabled: true, expires_at: req.expires_at, created_at: now(), last_used_at: None };
    sqlx::query("INSERT INTO s3_keys (id,bucket_id,access_key,secret,description,writable,expires_at,created_at) VALUES (?,?,?,?,?,?,?,?)").bind(&key.id).bind(&key.bucket_id).bind(&key.access_key).bind(&key.secret).bind(&key.description).bind(key.writable).bind(key.expires_at).bind(key.created_at).execute(&mut *conn).await?;
    Ok(Json(CreatedKey { key, secret_key }))
}

pub async fn revoke_key(State(st): State<AppState>, user: User, Path((bucket, key)): Path<(String, String)>, Json(req): Json<Enabled>) -> AppResult<()> {
    managed(&st, &user, &bucket).await?;
    sqlx::query("UPDATE s3_keys SET enabled = ? WHERE id = ? AND bucket_id = ?").bind(req.enabled).bind(key).bind(bucket).execute(&st.db).await?;
    Ok(())
}

pub async fn delete_key(State(st): State<AppState>, user: User, Path((bucket, key)): Path<(String, String)>) -> AppResult<()> {
    managed(&st, &user, &bucket).await?;
    sqlx::query("DELETE FROM s3_keys WHERE id = ? AND bucket_id = ?").bind(key).bind(bucket).execute(&st.db).await?;
    Ok(())
}

pub async fn admin_list(st: State<AppState>, Admin(user): Admin, q: Query<ListQuery>) -> AppResult<Json<FolderBuckets>> { list(st, user, q).await }
