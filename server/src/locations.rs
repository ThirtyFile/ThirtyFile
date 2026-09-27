//! Storage locations (System settings › Storage locations) and space move jobs.

use std::{collections::HashMap, path::Path as FsPath, sync::Arc, time::Duration};

use axum::{
    Json,
    extract::{Path, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{
    auth::Admin,
    error::{AppError, AppResult},
    state::{AppState, LocationHealth, MigrationStatus},
    storage::{self, Storage},
    tree,
    util::{new_id, now, validate_name},
};

pub const BUILTIN: &str = "local";

#[derive(sqlx::FromRow)]
struct LocationRow {
    id: String,
    name: String,
    kind: String,
    config: String,
    is_default: bool,
}

fn config_json(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| json!({}))
}

/// Loads all storage locations at startup; locations that can't be built are logged as warnings and skipped (reading their files reports "unavailable")
pub async fn load_all(db: &SqlitePool, storage_dir: &FsPath) -> Result<(HashMap<String, Arc<dyn Storage>>, String), sqlx::Error> {
    let rows: Vec<LocationRow> = sqlx::query_as("SELECT id, name, kind, config, is_default FROM storage_locations").fetch_all(db).await?;
    let mut map = HashMap::new();
    let mut default = BUILTIN.to_string();
    for r in rows {
        if r.is_default {
            default = r.id.clone();
        }
        match storage::build(&r.kind, &config_json(&r.config), storage_dir) {
            Ok(s) => {
                map.insert(r.id, s);
            }
            Err(e) => tracing::warn!("Storage location \"{}\" is unavailable: {e}", r.name),
        }
    }
    Ok((map, default))
}

// ───────────── Management API ─────────────

#[derive(Serialize)]
pub struct LocationInfo {
    id: String,
    name: String,
    kind: String,
    builtin: bool,
    is_default: bool,
    /// Settings (without secrets)
    config: Value,
    has_secret: bool,
    /// Settings are loaded and the most recent connection check succeeded
    connected: bool,
    /// Reason the most recent connection check failed
    health_error: Option<String>,
    checked_at: Option<i64>,
    /// Number of physical files whose deletion failed and is awaiting retry
    pending_deletes: i64,
    used_bytes: i64,
    blob_count: i64,
    drive_count: i64,
}

/// Settings with secrets removed
/// Fields that aren't returned to the browser; leaving them blank when editing keeps the existing value
const SECRET_FIELDS: [&str; 4] = ["secret_access_key", "password", "private_key", "key_passphrase"];

fn public_config(cfg: &Value) -> (Value, bool) {
    let mut c = cfg.clone();
    let mut has_secret = false;
    if let Some(obj) = c.as_object_mut() {
        for field in SECRET_FIELDS {
            if let Some(Value::String(s)) = obj.remove(field) {
                has_secret |= !s.is_empty();
            }
        }
    }
    (c, has_secret)
}

pub async fn list(State(st): State<AppState>, _: Admin) -> AppResult<Json<Vec<LocationInfo>>> {
    let rows: Vec<LocationRow> =
        sqlx::query_as("SELECT id, name, kind, config, is_default FROM storage_locations ORDER BY (id = 'local') DESC, created_at")
            .fetch_all(&st.db)
            .await?;
    // Totals of every location in three grouped queries (blobs by the covering index blobs_location_size)
    let db = &st.db;
    let totals = |sql: &'static str| async move {
        let rows: Vec<(String, i64, i64)> = sqlx::query_as(sql).fetch_all(db).await?;
        AppResult::Ok(rows.into_iter().map(|(id, a, b)| (id, (a, b))).collect::<std::collections::HashMap<_, _>>())
    };
    let blobs = totals("SELECT location_id, COALESCE(SUM(size), 0), COUNT(*) FROM blobs GROUP BY location_id").await?;
    let drives = totals("SELECT location_id, COUNT(*), 0 FROM drives WHERE location_id IS NOT NULL GROUP BY location_id").await?;
    let pending = totals("SELECT location_id, COUNT(*), 0 FROM pending_blob_deletes GROUP BY location_id").await?;
    let mut out = Vec::new();
    for r in rows {
        let (used_bytes, blob_count) = blobs.get(&r.id).copied().unwrap_or_default();
        let drive_count = drives.get(&r.id).map_or(0, |d| d.0);
        let pending_deletes = pending.get(&r.id).map_or(0, |d| d.0);
        let (mut config, has_secret) = public_config(&config_json(&r.config));
        if r.id == BUILTIN {
            // Shown in the list; the built-in location's folder is set with THIRTYFILE_STORAGE
            config["path"] = st.storage_dir.display().to_string().into();
        }
        let health = st.location_health.lock().unwrap().get(&r.id).cloned();
        let connected = st.storages.read().unwrap().contains_key(&r.id) && health.as_ref().is_none_or(|h| h.ok);
        out.push(LocationInfo {
            builtin: r.id == BUILTIN,
            id: r.id,
            name: r.name,
            kind: r.kind,
            is_default: r.is_default,
            config,
            has_secret,
            connected,
            health_error: health.as_ref().and_then(|h| h.error.clone()),
            checked_at: health.map(|h| h.checked_at),
            pending_deletes,
            used_bytes,
            blob_count,
            drive_count,
        });
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct LocationReq {
    name: Option<String>,
    kind: Option<String>,
    config: Option<Value>,
}

/// Settings that decide where saved passwords and keys are sent
const TARGET_FIELDS: [&str; 8] = ["host", "port", "endpoint", "bucket", "region", "username", "access_key_id", "tls"];

/// Keeps the existing secrets when none are entered (no need to re-enter them when editing), but only for the same
/// kind of storage and the same server, bucket and account: otherwise they would be sent to wherever the new settings
/// point, and anyone able to edit the settings could collect them
async fn merged_config(st: &AppState, id: Option<&str>, kind: &str, config: Value) -> AppResult<Value> {
    let mut config = config;
    let Some(id) = id else { return Ok(config) };
    let old: Option<(String, String)> =
        sqlx::query_as("SELECT kind, config FROM storage_locations WHERE id = ?").bind(id).fetch_optional(&st.db).await?;
    let Some((old_kind, old)) = old.map(|(k, c)| (k, config_json(&c))) else { return Ok(config) };
    let wanted = SECRET_FIELDS.iter().any(|f| {
        config.get(*f).and_then(Value::as_str).is_none_or(str::is_empty) && old.get(*f).and_then(Value::as_str).is_some_and(|s| !s.is_empty())
    });
    if !wanted {
        return Ok(config);
    }
    let normalized = storage::normalize(kind, config.clone()).await;
    if old_kind != kind || TARGET_FIELDS.iter().any(|f| normalized.get(*f) != old.get(*f)) {
        return Err(AppError::bad_request("Enter the password or key again: the server or account changed"));
    }
    if let Some(obj) = config.as_object_mut() {
        for field in SECRET_FIELDS {
            let empty = obj.get(field).and_then(Value::as_str).is_none_or(str::is_empty);
            if empty && let Some(secret) = old.get(field).cloned() {
                obj.insert(field.into(), secret);
            }
        }
    }
    Ok(config)
}

/// Explanation of a storage service error: use SFTP / FTP's specific reason (credentials, host key, TLS…) when there is one; otherwise infer it from the error content
fn describe(e: &std::io::Error) -> String {
    if let Some(se) = e.get_ref().and_then(|i| i.downcast_ref::<storage::StorageError>())
        && se.message != storage::UNAVAILABLE
        && se.message != storage::DENIED_KEYS
    {
        return se.message.to_string();
    }
    let full = e.to_string();
    // Look for keywords only in the underlying error, not in our own explanation text
    let detail = e.get_ref().and_then(|i| i.downcast_ref::<storage::StorageError>()).map_or(full.as_str(), |se| se.detail.as_str());
    friendly(detail, &full)
}

/// Turns S3 / file system errors into an understandable explanation; `err` is shown when nothing more specific applies
fn friendly(detail: &str, err: &str) -> String {
    let e = detail.to_ascii_lowercase();
    let msg = if e.contains("signaturedoesnotmatch") || e.contains("invalidaccesskeyid") || e.contains("403 forbidden") {
        "Incorrect Access Key or Secret Key, or no permission for this bucket"
    } else if e.contains("permanentredirect") || e.contains("redirect") || e.contains("authorizationheadermalformed") || e.contains("301 moved") || e.contains("invalidregionname") {
        "The region doesn't match the bucket's region. For AWS, leave the region blank to detect it automatically; for R2, enter auto."
    } else if e.contains("nosuchbucket") || e.contains("404 not found") {
        "Bucket not found. Create it in the storage service first."
    } else if e.contains(".r2.cloudflarestorage.com") && e.contains("error sending request") {
        "Can't connect to R2. Check the account ID in the endpoint (Cloudflare dashboard › R2 › Account details)."
    } else if e.contains("connection refused") || e.contains("error sending request") || e.contains("dns") || e.contains("connect") {
        "Can't connect to the server. Check the address, port, and network."
    } else if e.contains("certificate") || e.contains("tls") {
        "TLS certificate verification failed. Check that the endpoint URL (https) is correct."
    } else if e.contains("permission denied") || e.contains("access is denied") {
        "No read and write permission for this folder"
    } else {
        return format!("Connection test failed: {}", err.chars().take(160).collect::<String>());
    };
    msg.to_string()
}

// ───────────── Connection health monitoring ─────────────

/// Sends just one HEAD request every 30 seconds (about US$0.03 per month on AWS); a disconnect shows as offline in the UI within half a minute
const HEALTH_INTERVAL: Duration = Duration::from_secs(30);
/// Shorter check interval while a location is offline, so it's usable again soon after recovering
const OFFLINE_INTERVAL: Duration = Duration::from_secs(20);

static RECHECK: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Called when an operation hits a storage service error: recheck the connection right away so the UI soon shows "offline"
pub fn request_recheck() {
    RECHECK.notify_one();
}
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

fn set_health(st: &AppState, id: &str, error: Option<String>) {
    st.location_health.lock().unwrap().insert(id.to_string(), LocationHealth { ok: error.is_none(), error, checked_at: now() });
}

/// Checks whether a location can be reached (a lightweight check that writes no data) and updates its health status
pub async fn probe(st: &AppState, id: &str) -> Result<(), String> {
    let storage = st.storage(id).map_err(|e| e.message)?;
    let res = match tokio::time::timeout(PROBE_TIMEOUT, storage.ping()).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(describe(&e)),
        Err(_) => Err("Connection timed out. Check that the service is running.".to_string()),
    };
    let was_ok = st.location_health.lock().unwrap().get(id).is_none_or(|h| h.ok);
    set_health(st, id, res.clone().err());
    match (&res, was_ok) {
        (Err(e), true) => tracing::warn!("Storage location {id} is unreachable: {e}"),
        (Ok(()), false) => tracing::info!("Storage location {id} is reachable again"),
        _ => {}
    }
    res
}

/// Checks all storage locations every 30 seconds (every 20 seconds while any is offline), and immediately when an operation fails.
/// Reachable locations also retry files whose deletion failed earlier
pub fn spawn_health_monitor(st: AppState) {
    tokio::spawn(async move {
        let mut first = true;
        loop {
            if !first {
                let any_down = st.location_health.lock().unwrap().values().any(|h| !h.ok);
                tokio::select! {
                    _ = tokio::time::sleep(if any_down { OFFLINE_INTERVAL } else { HEALTH_INTERVAL }) => {}
                    // Several requests may fail at the same time: wait a moment and merge them into one check
                    _ = RECHECK.notified() => tokio::time::sleep(Duration::from_secs(1)).await,
                }
            }
            first = false;
            let ids: Vec<String> = st.storages.read().unwrap().keys().cloned().collect();
            let results = futures_util::future::join_all(ids.iter().map(|id| probe(&st, id))).await;
            for (id, res) in ids.iter().zip(results) {
                if res.is_ok() {
                    // One line per location: failures of single files are only logged at debug level
                    let (n, failed) = tree::retry_pending_deletes(&st, id).await;
                    if n > 0 {
                        tracing::info!("Retried deleting {n} physical files whose deletion failed earlier ({id}), {failed} failed again");
                    }
                }
            }
        }
    });
}

/// Tidies up the settings, builds the backend and runs a connection test (write, read back and delete a small file); returns the tidied settings to save
async fn connect(st: &AppState, kind: &str, config: Value) -> AppResult<(Arc<dyn Storage>, Value)> {
    let config = storage::normalize(kind, config).await;
    storage::check_insecure_target(kind, &config).await.map_err(AppError::bad_request)?;
    let backend = storage::build(kind, &config, &st.storage_dir).map_err(|e| AppError::bad_request(format!("Invalid settings: {e}")))?;
    tokio::time::timeout(Duration::from_secs(20), backend.check())
        .await
        .map_err(|_| AppError::bad_request("Connection timed out. Check the endpoint and network."))?
        .map_err(|e| {
            tracing::warn!("storage check failed: {e}");
            AppError::bad_request(describe(&e))
        })?;
    // SFTP: record the host key on the first connection; later connections must match it
    let mut config = config;
    if kind == "sftp"
        && config.get("host_key").and_then(Value::as_str).is_none_or(|k| k.trim().is_empty())
        && let (Some(key), Some(obj)) = (backend.host_key(), config.as_object_mut())
    {
        obj.insert("host_key".into(), key.into());
    }
    Ok((backend, config))
}

#[derive(Deserialize)]
pub struct TestReq {
    /// Sent when editing an existing location, to keep its saved secrets
    id: Option<String>,
    kind: String,
    config: Value,
}

pub async fn test(State(st): State<AppState>, _: Admin, Json(req): Json<TestReq>) -> AppResult<Json<Value>> {
    let config = merged_config(&st, req.id.as_deref(), &req.kind, req.config).await?;
    let (_, config) = connect(&st, &req.kind, config).await?;
    Ok(Json(json!({ "ok": true, "region": config.get("region"), "host_key": config.get("host_key") })))
}

pub async fn test_existing(State(st): State<AppState>, _: Admin, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let row: LocationRow = sqlx::query_as("SELECT id, name, kind, config, is_default FROM storage_locations WHERE id = ?")
        .bind(&id)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Storage location not found"))?;
    let backend = connect(&st, &row.kind, config_json(&row.config)).await;
    set_health(&st, &id, backend.as_ref().err().map(|e| e.message.clone()));
    match backend {
        Ok((b, _)) => {
            st.storages.write().unwrap().insert(id.clone(), b);
            // In the background: up to 1000 deletions on a slow storage service shouldn't hold the request
            let (st, id) = (st.clone(), id.clone());
            tokio::spawn(async move {
                tree::retry_pending_deletes(&st, &id).await;
            });
            Ok(Json(json!({ "ok": true })))
        }
        Err(e) => Err(e),
    }
}

pub async fn create(State(st): State<AppState>, Admin(user): Admin, Json(req): Json<LocationReq>) -> AppResult<Json<Value>> {
    let name = validate_name(req.name.as_deref().unwrap_or_default())?;
    let kind = req.kind.unwrap_or_default();
    let config = req.config.unwrap_or_else(|| json!({}));
    let (backend, config) = connect(&st, &kind, config).await?;
    let id = new_id();
    {
        let _w = st.write_lock.lock().await;
        let mut tx = st.db.begin().await?;
        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, is_default, created_at) VALUES (?, ?, ?, ?, 0, ?)")
            .bind(&id)
            .bind(&name)
            .bind(&kind)
            .bind(config.to_string())
            .bind(now())
            .execute(&mut *tx)
            .await?;
        tree::log(&mut tx, &user, None, "storage_create", &name).await?;
        tx.commit().await?;
    }
    st.storages.write().unwrap().insert(id.clone(), backend);
    Ok(Json(json!({ "id": id })))
}

pub async fn update(
    State(st): State<AppState>,
    Admin(user): Admin,
    Path(id): Path<String>,
    Json(req): Json<LocationReq>,
) -> AppResult<Json<Value>> {
    let row: LocationRow = sqlx::query_as("SELECT id, name, kind, config, is_default FROM storage_locations WHERE id = ?")
        .bind(&id)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Storage location not found"))?;
    let name = match &req.name {
        Some(n) => validate_name(n)?,
        None => row.name.clone(),
    };
    // The built-in local location can only be renamed
    let new_backend = match (&req.config, id == BUILTIN) {
        (Some(cfg), false) => {
            let merged = merged_config(&st, Some(&id), &row.kind, cfg.clone()).await?;
            Some(connect(&st, &row.kind, merged).await?)
        }
        _ => None,
    };
    {
        let _w = st.write_lock.lock().await;
        let mut tx = st.db.begin().await?;
        sqlx::query("UPDATE storage_locations SET name = ? WHERE id = ?").bind(&name).bind(&id).execute(&mut *tx).await?;
        if let Some((_, cfg)) = &new_backend {
            sqlx::query("UPDATE storage_locations SET config = ? WHERE id = ?").bind(cfg.to_string()).bind(&id).execute(&mut *tx).await?;
        }
        tree::log(&mut tx, &user, None, "storage_update", &name).await?;
        tx.commit().await?;
    }
    if let Some((backend, _)) = new_backend {
        st.storages.write().unwrap().insert(id, backend);
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn set_default(State(st): State<AppState>, Admin(user): Admin, Path(id): Path<String>) -> AppResult<Json<Value>> {
    if !st.storages.read().unwrap().contains_key(&id) {
        return Err(AppError::bad_request("This storage location can't be reached right now, so it can't be set as the default"));
    }
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    // Check that it exists first (the UPDATE affects all rows, so rows_affected can't tell)
    let (name,): (String,) = sqlx::query_as("SELECT name FROM storage_locations WHERE id = ?")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::not_found("Storage location not found"))?;
    sqlx::query("UPDATE storage_locations SET is_default = (id = ?)").bind(&id).execute(&mut *tx).await?;
    tree::log(&mut tx, &user, None, "storage_default", &name).await?;
    tx.commit().await?;
    *st.default_location.write().unwrap() = id;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete(State(st): State<AppState>, Admin(user): Admin, Path(id): Path<String>) -> AppResult<Json<Value>> {
    if id == BUILTIN {
        return Err(AppError::bad_request("The built-in local disk can't be deleted"));
    }
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    let (name, is_default, blobs, drives): (String, bool, i64, i64) = sqlx::query_as(
        "SELECT name, is_default, (SELECT COUNT(*) FROM blobs WHERE location_id = ?1), (SELECT COUNT(*) FROM drives WHERE location_id = ?1)
         FROM storage_locations WHERE id = ?1",
    )
    .bind(&id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("Storage location not found"))?;
    if is_default {
        return Err(AppError::bad_request("Set another location as the default first"));
    }
    if drives > 0 {
        return Err(AppError::bad_request(if drives == 1 {
            format!("{drives} space still uses this location. Change it first.")
        } else {
            format!("{drives} spaces still use this location. Change them first.")
        }));
    }
    if blobs > 0 {
        return Err(AppError::bad_request(format!(
            "This location still stores {blobs} {}. Move the spaces that use it to another location first.",
            if blobs == 1 { "file" } else { "files" }
        )));
    }
    sqlx::query("DELETE FROM storage_locations WHERE id = ?").bind(&id).execute(&mut *tx).await?;
    // Content that couldn't be deleted there stays in that storage: ThirtyFile no longer connects to it
    sqlx::query("DELETE FROM pending_blob_deletes WHERE location_id = ?").bind(&id).execute(&mut *tx).await?;
    tree::log(&mut tx, &user, None, "storage_delete", &name).await?;
    tx.commit().await?;
    st.storages.write().unwrap().remove(&id);
    st.location_health.lock().unwrap().remove(&id);
    Ok(Json(json!({ "ok": true })))
}

// ───────────── Space storage locations and moves ─────────────

#[derive(Deserialize)]
pub struct DriveLocationReq {
    /// null means use the default location
    location_id: Option<String>,
    /// Also move existing files to the new location
    #[serde(default)]
    migrate: bool,
}

pub async fn set_drive_location(
    State(st): State<AppState>,
    Admin(user): Admin,
    Path(drive_id): Path<String>,
    Json(req): Json<DriveLocationReq>,
) -> AppResult<Json<Value>> {
    refuse_folder_space(&st, &drive_id).await?;
    if let Some(loc) = &req.location_id {
        let exists: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM storage_locations WHERE id = ?").bind(loc).fetch_optional(&st.db).await?;
        if exists.is_none() {
            return Err(AppError::bad_request("Storage location not found"));
        }
        if !st.storages.read().unwrap().contains_key(loc) {
            return Err(AppError::bad_request("This storage location can't be reached right now"));
        }
    }
    if st.migrations.lock().unwrap().get(&drive_id).is_some_and(|j| j.running) {
        return Err(AppError::conflict("This space is being moved. Wait until the move finishes before changing it."));
    }
    // First make sure the target location is really reachable; otherwise later uploads to this space would all fail
    let target = req.location_id.clone().unwrap_or_else(|| st.default_location.read().unwrap().clone());
    probe(&st, &target).await.map_err(|e| AppError::bad_request(format!("The target storage location can't be reached, so nothing was changed: {e}")))?;
    {
        let _w = st.write_lock.lock().await;
        let mut tx = st.db.begin().await?;
        let drive = tree::get_drive(&mut tx, &drive_id).await?.ok_or_else(|| AppError::not_found("Space not found"))?;
        sqlx::query("UPDATE drives SET location_id = ? WHERE id = ?").bind(&req.location_id).bind(&drive_id).execute(&mut *tx).await?;
        let root = tree::get_node(&mut tx, &drive.root_id).await?;
        tree::log(&mut tx, &user, root.as_ref(), "drive_location", req.location_id.as_deref().unwrap_or("Default")).await?;
        tx.commit().await?;
    }
    if req.migrate {
        start_migration(&st, &drive_id).await?;
    }
    Ok(Json(json!({ "ok": true })))
}

/// A folder space's files are a folder on the server: they can't be moved to a storage location
async fn refuse_folder_space(st: &AppState, drive_id: &str) -> AppResult<()> {
    let (mode,): (String,) = sqlx::query_as("SELECT mode FROM drives WHERE id = ?")
        .bind(drive_id)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Space not found"))?;
    if mode == "folder" {
        return Err(AppError::bad_request("This space shows a folder on the server; its files can't be moved to a storage location"));
    }
    Ok(())
}

pub async fn migrate(State(st): State<AppState>, _: Admin, Path(drive_id): Path<String>) -> AppResult<Json<Value>> {
    refuse_folder_space(&st, &drive_id).await?;
    let target = tree::drive_location(&st, &mut *st.db.acquire().await?, &drive_id).await?;
    probe(&st, &target).await.map_err(|e| AppError::bad_request(format!("The target storage location can't be reached: {e}")))?;
    start_migration(&st, &drive_id).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn migrations(State(st): State<AppState>, _: Admin) -> AppResult<Json<Vec<MigrationStatus>>> {
    let mut list: Vec<MigrationStatus> = st.migrations.lock().unwrap().values().cloned().collect();
    list.sort_by_key(|m| std::cmp::Reverse(m.started_at));
    Ok(Json(list))
}

async fn start_migration(st: &AppState, drive_id: &str) -> AppResult<()> {
    let target = tree::drive_location(st, &mut *st.db.acquire().await?, drive_id).await?;
    st.storage(&target)?;
    {
        let mut jobs = st.migrations.lock().unwrap();
        if jobs.get(drive_id).is_some_and(|j| j.running) {
            return Err(AppError::conflict("This space is being moved"));
        }
        jobs.insert(
            drive_id.to_string(),
            MigrationStatus { drive_id: drive_id.to_string(), target: target.clone(), running: true, started_at: now(), ..Default::default() },
        );
    }
    let st = st.clone();
    let drive_id = drive_id.to_string();
    tokio::spawn(async move {
        let result = run_migration(&st, &drive_id, &target).await;
        let mut jobs = st.migrations.lock().unwrap();
        if let Some(job) = jobs.get_mut(&drive_id) {
            job.running = false;
            job.finished_at = Some(now());
            if let Err(e) = result {
                tracing::warn!("Moving space {drive_id} failed: {}", e.message);
                job.error = Some(e.message);
            }
        }
    });
    Ok(())
}

fn update_job(st: &AppState, drive_id: &str, f: impl FnOnce(&mut MigrationStatus)) {
    if let Some(job) = st.migrations.lock().unwrap().get_mut(drive_id) {
        f(job);
    }
}

/// Moves file content used by the space that isn't in the target location yet. Verifies the sha256 after copying, then switches the reference;
/// the copy in the old location is deleted only after a minute (letting in-flight downloads finish).
async fn run_migration(st: &AppState, drive_id: &str, target: &str) -> AppResult<()> {
    // Content of the space not yet at the target, walked in hash order: each page continues after the last hash
    // (`?3`), so a page costs the same at the end of a large space as at the start
    const PENDING: &str = "FROM blobs b WHERE b.location_id != ?1 AND b.hash > ?3
         AND EXISTS (SELECT 1 FROM nodes n WHERE n.blob_hash = b.hash AND n.drive_id = ?2)";
    let (files, bytes): (i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT COUNT(*), COALESCE(SUM(b.size), 0) {PENDING}")))
        .bind(target)
        .bind(drive_id)
        .bind("")
        .fetch_one(&st.db)
        .await?;
    update_job(st, drive_id, |j| {
        j.total_files = files;
        j.total_bytes = bytes;
    });
    let dst = st.storage(target)?;
    let mut moved: Vec<(String, String)> = Vec::new();
    let mut last = String::new();
    loop {
        let rows: Vec<(String, i64, String)> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT b.hash, b.size, b.location_id {PENDING} ORDER BY b.hash LIMIT 50")))
                .bind(target)
                .bind(drive_id)
                .bind(&last)
                .fetch_all(&st.db)
                .await?;
        let Some((hash, ..)) = rows.last() else { break };
        last = hash.clone();
        for (hash, size, from) in rows {
            // Held from copying until the reference is switched: a deletion of this content still pending at the
            // target (from an earlier move away from it) must not remove the copy this move keeps or writes there
            let _staging = tree::stage_guard(st, &hash).await;
            let src = st.storage(&from)?;
            let tmp = st.tmp_dir().join(format!("migrate-{}", new_id()));
            let copied = async {
                // Hashed while it is copied, so the content is read once before it is stored at the target
                let mut reader = src.open(&hash, 0, size as u64).await?;
                let mut file = tokio::fs::File::create(&tmp).await?;
                let mut hasher = Sha256::new();
                let mut len = 0u64;
                let mut buf = vec![0u8; 256 * 1024];
                loop {
                    let n = reader.read(&mut buf).await?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buf[..n]);
                    file.write_all(&buf[..n]).await?;
                    len += n as u64;
                }
                file.flush().await?;
                drop(file);
                if hex::encode(hasher.finalize()) != hash || len != size as u64 {
                    return Err(std::io::Error::other("Verification of the copied content failed"));
                }
                dst.put_file(&hash, &tmp).await
            }
            .await;
            let _ = tokio::fs::remove_file(&tmp).await;
            if let Err(e) = copied {
                // Deleted by someone during the move (or moved elsewhere meanwhile): nothing left to move for it
                let current: Option<(String,)> =
                    sqlx::query_as("SELECT location_id FROM blobs WHERE hash = ?").bind(&hash).fetch_optional(&st.db).await?;
                if e.kind() == std::io::ErrorKind::NotFound && current.is_none_or(|(loc,)| loc != from) {
                    tree::schedule_blob_removal(st, vec![(hash.clone(), target.to_string())]);
                    update_job(st, drive_id, |j| {
                        j.done_files += 1;
                        j.done_bytes += size;
                    });
                    continue;
                }
                return Err(AppError::new(axum::http::StatusCode::BAD_GATEWAY, format!("Failed to move files: {e}")));
            }

            let switched = {
                let _w = st.write_lock.lock().await;
                sqlx::query("UPDATE blobs SET location_id = ? WHERE hash = ? AND location_id = ?")
                    .bind(target)
                    .bind(&hash)
                    .bind(&from)
                    .execute(&st.db)
                    .await?
                    .rows_affected()
                    == 1
            };
            if switched {
                // Recorded right away: if a later file fails and the job stops, the copies already moved are still cleaned up
                tree::defer_blob_removal(st, &[(hash.clone(), from.clone())], 60).await;
                moved.push((hash.clone(), from));
            } else {
                // The blob was released or moved elsewhere while it was being copied: the copy just written to the
                // target is unreferenced (remove_unreferenced checks again before deleting)
                tree::schedule_blob_removal(st, vec![(hash.clone(), target.to_string())]);
            }
            update_job(st, drive_id, |j| {
                j.done_files += 1;
                j.done_bytes += size;
            });
        }
    }
    // Old copies are deleted later (letting in-flight downloads finish); this doesn't affect whether the job completes.
    // Before deleting, confirm the old location is no longer current (don't delete if it was moved back meanwhile), and don't hold the write lock while calling the storage service
    if !moved.is_empty() {
        let st = st.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(60)).await;
            tree::remove_unreferenced(&st, moved).await;
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[tokio::test]
    async fn moving_a_space_moves_every_file_across_pages() {
        let env = testutil::env().await;
        let amy = env.user("amy", false).await;
        env.st.storages.write().unwrap().insert("second".into(), Arc::new(crate::storage::LocalStorage::new(env.dir.join("second")).unwrap()));
        let local = env.st.storage("local").unwrap();
        let mut hashes = Vec::new();
        // More than one page of 50
        for i in 0..60 {
            let content = format!("file {i}");
            let hash = hex::encode(Sha256::digest(content.as_bytes()));
            let tmp = env.dir.join("tmp").join(&hash);
            std::fs::write(&tmp, &content).unwrap();
            local.put_file(&hash, &tmp).await.unwrap();
            let id = env.file(&amy, &amy.root_id, &format!("f{i}.txt")).await;
            let mut c = env.st.db.acquire().await.unwrap();
            tree::add_blob_ref(&mut c, &hash, content.len() as i64, "local").await.unwrap();
            sqlx::query("UPDATE nodes SET blob_hash = ?, size = ? WHERE id = ?").bind(&hash).bind(content.len() as i64).bind(&id).execute(&mut *c).await.unwrap();
            hashes.push(hash);
        }
        let drive = env.drive_of(&amy.root_id).await;
        run_migration(&env.st, &drive, "second").await.unwrap();
        let (left,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM blobs WHERE location_id != 'second'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(left, 0);
        let second = env.st.storage("second").unwrap();
        for hash in &hashes {
            let mut content = String::new();
            second.open(hash, 0, 64).await.unwrap().read_to_string(&mut content).await.unwrap();
            assert!(content.starts_with("file "), "{hash} wasn't copied");
        }
    }

    #[tokio::test]
    async fn saved_passwords_are_only_reused_for_the_same_server_and_account() {
        let env = testutil::env().await;
        let saved = json!({ "host": "files.example.com", "port": 22, "username": "backup", "password": testutil::password(), "host_key": "k" });
        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, is_default, created_at) VALUES ('nas', 'NAS', 'sftp', ?, 0, 0)")
            .bind(saved.to_string())
            .execute(&env.st.db)
            .await
            .unwrap();
        // Editing something else: the password is filled in
        let same = json!({ "host": "files.example.com", "port": 22, "username": "backup", "password": "", "host_key": "k" });
        let merged = merged_config(&env.st, Some("nas"), "sftp", same).await.unwrap();
        assert_eq!(merged["password"], testutil::password());
        // Another server, another account or another kind: it must be entered again
        for (kind, cfg) in [
            ("sftp", json!({ "host": "elsewhere.example.com", "port": 22, "username": "backup", "password": "" })),
            ("sftp", json!({ "host": "files.example.com", "port": 22, "username": "someone", "password": "" })),
            ("ftp", json!({ "host": "files.example.com", "port": 22, "username": "backup", "password": "" })),
        ] {
            let err = merged_config(&env.st, Some("nas"), kind, cfg).await.unwrap_err();
            assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        }
        // A password entered anew is used as it is
        let fresh = json!({ "host": "elsewhere.example.com", "port": 22, "username": "backup", "password": testutil::wrong_password() });
        assert!(merged_config(&env.st, Some("nas"), "sftp", fresh).await.is_ok());
    }
}
