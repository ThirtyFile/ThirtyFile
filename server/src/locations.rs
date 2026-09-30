//! Storage locations (System settings › Storage locations). Moving spaces between them is in moves/.

use std::{collections::HashMap, path::Path as FsPath, sync::Arc, time::Duration};

use axum::{
    Json,
    extract::{Path, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{SqliteConnection, SqlitePool};

use crate::{
    auth::Admin,
    error::{AppError, AppResult},
    logs,
    state::{AppState, LocationHealth},
    storage::{self, Storage},
    tree,
    util::{self, new_id, now, validate_name},
};

/// The id of the built-in location
pub use crate::storage::BUILTIN;

#[derive(sqlx::FromRow)]
struct LocationRow {
    id: String,
    name: String,
    kind: String,
    config: String,
    is_default: bool,
}

/// A location's stored settings, with its passwords and keys decrypted (secrets.rs; each bound to the location and
/// field it is stored for)
pub(crate) fn config_json(id: &str, raw: &str) -> Value {
    let mut cfg: Value = serde_json::from_str(raw).unwrap_or_else(|_| json!({}));
    if let Some(obj) = cfg.as_object_mut() {
        for field in SECRET_FIELDS {
            if let Some(Value::String(v)) = obj.get_mut(field) {
                match crate::secrets::open(&format!("location:{id}:{field}"), v) {
                    Ok(plain) => *v = plain,
                    Err(e) => {
                        tracing::error!("A saved {field} of a storage location can't be read ({e}); enter it again");
                        v.clear();
                    }
                }
            }
        }
    }
    cfg
}

/// Settings as stored: passwords and keys encrypted
fn sealed_config(id: &str, cfg: &Value) -> String {
    let mut cfg = cfg.clone();
    if let Some(obj) = cfg.as_object_mut() {
        for field in SECRET_FIELDS {
            if let Some(Value::String(v)) = obj.get_mut(field) {
                *v = crate::secrets::seal(&format!("location:{id}:{field}"), v);
            }
        }
    }
    cfg.to_string()
}

/// Loads all storage locations at startup; locations that can't be built are logged as warnings and skipped (reading their files reports "unavailable").
/// Nothing is created: a Local folder location whose folder isn't there (or lacks its marker) is loaded, and the
/// health check reports it unavailable until the folder is back.
pub async fn load_all(db: &SqlitePool, storage_dir: &FsPath) -> Result<HashMap<String, Arc<dyn Storage>>, sqlx::Error> {
    let rows: Vec<LocationRow> = sqlx::query_as("SELECT id, name, kind, config, is_default FROM storage_locations").fetch_all(db).await?;
    let mut map = HashMap::new();
    for r in rows {
        match storage::build(&r.id, &r.kind, &config_json(&r.id, &r.config), storage_dir) {
            Ok(s) => {
                map.insert(r.id, s);
            }
            Err(e) => tracing::warn!("Storage location \"{}\" is unavailable: {e}", r.name),
        }
    }
    Ok(map)
}

/// The default storage location: where new spaces are created. A space records its location when it is created
/// (`db::create_drive`), so changing the default later doesn't move existing spaces.
pub async fn default_location(conn: &mut SqliteConnection) -> Result<String, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as("SELECT id FROM storage_locations WHERE is_default = 1 LIMIT 1").fetch_optional(conn).await?;
    Ok(row.map_or_else(|| BUILTIN.to_string(), |r| r.0))
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
    /// Space used on the location: the content store there, plus the indexed size of the folder spaces on it
    used_bytes: i64,
    /// Of `used_bytes`, the folder spaces' part
    folder_bytes: i64,
    /// Files in the content store there
    blob_count: i64,
    /// Spaces on the location (`drives.location_id`), of every kind and mode
    drive_count: i64,
    /// Locations on this server's disks (built-in and Local folder): free and total bytes of the disk holding the
    /// folder, when the system tells
    disk_free_bytes: Option<u64>,
    disk_total_bytes: Option<u64>,
}

/// Fields that aren't returned to the browser (leaving them blank when editing keeps the existing value), and are
/// stored encrypted
pub const SECRET_FIELDS: [&str; 4] = ["secret_access_key", "password", "private_key", "key_passphrase"];

/// Settings with secrets removed
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

/// Free and total bytes of the disk holding `path`; None when the system doesn't tell within a few seconds (a NAS
/// that stopped answering must not hold the list)
async fn disk_of(path: std::path::PathBuf) -> Option<(u64, u64)> {
    util::disk_space_soon(&path).await
}

pub async fn list(State(st): State<AppState>, _: Admin) -> AppResult<Json<Vec<LocationInfo>>> {
    let rows: Vec<LocationRow> =
        sqlx::query_as("SELECT id, name, kind, config, is_default FROM storage_locations ORDER BY (id = 'local') DESC, created_at")
            .fetch_all(&st.db)
            .await?;
    // Totals of every location in three grouped queries (blobs by the covering index blobs_location_size; spaces are
    // few). A folder space's size is its index's (`drives.used_bytes`, kept up to date by every change and scan).
    let db = &st.db;
    let totals = |sql: &'static str| async move {
        let rows: Vec<(String, i64, i64)> = sqlx::query_as(sql).fetch_all(db).await?;
        AppResult::Ok(rows.into_iter().map(|(id, a, b)| (id, (a, b))).collect::<std::collections::HashMap<_, _>>())
    };
    let blobs = totals("SELECT location_id, COALESCE(SUM(size), 0), COUNT(*) FROM blobs GROUP BY location_id").await?;
    let drives = totals(
        "SELECT location_id, COUNT(*), COALESCE(SUM(CASE WHEN mode = 'folder' THEN used_bytes ELSE 0 END), 0)
         FROM drives WHERE location_id IS NOT NULL GROUP BY location_id",
    )
    .await?;
    let pending = totals("SELECT location_id, COUNT(*), 0 FROM pending_blob_deletes GROUP BY location_id").await?;
    let mut out = Vec::new();
    let mut disks = Vec::new();
    for r in rows {
        let (store_bytes, blob_count) = blobs.get(&r.id).copied().unwrap_or_default();
        let (drive_count, folder_bytes) = drives.get(&r.id).copied().unwrap_or_default();
        let pending_deletes = pending.get(&r.id).map_or(0, |d| d.0);
        let (mut config, has_secret) = public_config(&config_json(&r.id, &r.config));
        if r.id == BUILTIN {
            // Shown in the list; the built-in location's folder is set with THIRTYFILE_STORAGE
            config["path"] = st.storage_dir.display().to_string().into();
        }
        // On this server's disks: the disk holding the folder (a Local folder location without a folder uses the
        // storage folder, as space_folders.rs does)
        disks.push((r.kind == "local").then(|| {
            let path = config["path"].as_str().map(str::trim).filter(|p| !p.is_empty());
            path.map_or_else(|| st.storage_dir.clone(), std::path::PathBuf::from)
        }));
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
            used_bytes: store_bytes + folder_bytes,
            folder_bytes,
            blob_count,
            drive_count,
            disk_free_bytes: None,
            disk_total_bytes: None,
        });
    }
    let sizes = futures_util::future::join_all(disks.into_iter().map(|p| async move { disk_of(p?).await })).await;
    for (info, size) in out.iter_mut().zip(sizes) {
        (info.disk_free_bytes, info.disk_total_bytes) = size.unzip();
    }
    Ok(Json(out))
}

/// A space on a storage location, as its list shows it: what the space is, not what is in it (administrators don't
/// see into personal spaces)
#[derive(Serialize, sqlx::FromRow)]
pub struct LocationSpace {
    id: String,
    name: String,
    kind: String,
    /// "store" or "folder"
    mode: String,
    /// The owner's user name (personal spaces)
    owner_name: String,
    used_bytes: i64,
    /// The server folder of a folder space ("Move everything to…" names it)
    #[serde(skip_serializing_if = "Option::is_none")]
    source_path: Option<String>,
}

/// The spaces on a storage location (Control panel › Storage locations)
pub async fn spaces(State(st): State<AppState>, _: Admin, Path(id): Path<String>) -> AppResult<Json<Vec<LocationSpace>>> {
    let exists: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM storage_locations WHERE id = ?").bind(&id).fetch_optional(&st.db).await?;
    if exists.is_none() {
        return Err(AppError::not_found("Storage location not found"));
    }
    let list: Vec<LocationSpace> = sqlx::query_as(
        "SELECT d.id, d.name, d.kind, d.mode, CASE WHEN d.kind = 'personal' THEN COALESCE(u.username, '') ELSE '' END AS owner_name,
                d.used_bytes, CASE WHEN d.mode = 'folder' THEN d.source_path END AS source_path
         FROM drives d LEFT JOIN users u ON u.id = d.owner_id
         WHERE d.location_id = ?
         ORDER BY CASE d.kind WHEN 'company' THEN 0 WHEN 'team' THEN 1 ELSE 2 END, d.name, owner_name",
    )
    .bind(&id)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(list))
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
    let Some((old_kind, old)) = old.map(|(k, c)| (k, config_json(id, &c))) else { return Ok(config) };
    let wanted = SECRET_FIELDS.iter().any(|f| {
        config.get(*f).and_then(Value::as_str).is_none_or(str::is_empty) && old.get(*f).and_then(Value::as_str).is_some_and(|s| !s.is_empty())
    });
    if !wanted {
        return Ok(config);
    }
    let normalized = storage::normalize(kind, config.clone()).await;
    // Trusting another SFTP host key (or none yet, to record whatever answers) or no longer checking a certificate
    // would hand the secrets to whoever sits in between, like another server would
    let host_key = |c: &Value| c.get("host_key").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string();
    let insecure = |c: &Value| c.get("tls_insecure").and_then(Value::as_bool).unwrap_or(false);
    if old_kind != kind
        || TARGET_FIELDS.iter().any(|f| normalized.get(*f) != old.get(*f))
        || host_key(&normalized) != host_key(&old)
        || insecure(&normalized) != insecure(&old)
    {
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
pub(crate) fn describe(e: &std::io::Error) -> String {
    if let Some(se) = e.get_ref().and_then(|i| i.downcast_ref::<storage::StorageError>())
        && se.message != storage::UNAVAILABLE
        && se.message != storage::DENIED_KEYS
    {
        return se.text().into_owned();
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

// ───────────── Markers of S3, SFTP and FTP locations ─────────────
//
// Like a Local folder location's folder (storage.rs, `LOCATION_MARKER`), the place of an S3, SFTP or FTP location holds
// a `.thirtyfile-location` file: the location's id on the first line, this installation's id on the second. It is
// written when the location is added (or its first health check or cleanup finds it missing, for a location added
// before markers were written), and "Remove unused content" refuses to run in a place whose marker names another
// location or another installation: that content isn't unused, it is someone else's.

/// The id of this installation of ThirtyFile, made once and kept in the settings
pub async fn install_id(st: &AppState) -> AppResult<String> {
    if let Some(id) = crate::db::get_setting(&st.db, "install_id").await? {
        return Ok(id);
    }
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        sqlx::query("INSERT OR IGNORE INTO settings (key, value) VALUES ('install_id', ?)").bind(new_id()).execute(&mut *tx).await?;
        let (id,): (String,) = sqlx::query_as("SELECT value FROM settings WHERE key = 'install_id'").fetch_one(&mut *tx).await?;
        AppResult::Ok(id)
    }
    .await;
    crate::db::settle(tx, res).await
}

/// What the marker in a remote location's place says
#[derive(Debug, PartialEq, Eq)]
pub enum Marker {
    /// There is none (or it names a location of this installation that no longer exists)
    Missing,
    /// It names this location (and this installation, or none)
    Ours,
    /// It names another location of this installation, or another installation
    Taken,
}

/// Reads the marker in the place of `s`; None when there is none
async fn read_marker(s: &dyn Storage) -> std::io::Result<Option<(String, String)>> {
    let Some(entry) = s.stat(storage::LOCATION_MARKER).await? else { return Ok(None) };
    let mut body = Vec::new();
    let reader = s.open_at(storage::LOCATION_MARKER, 0, entry.size.min(4096)).await?;
    tokio::io::AsyncReadExt::read_to_end(&mut tokio::io::AsyncReadExt::take(reader, 4096), &mut body).await?;
    let text = String::from_utf8_lossy(&body);
    let mut lines = text.lines().map(str::trim);
    Ok(Some((lines.next().unwrap_or_default().to_string(), lines.next().unwrap_or_default().to_string())))
}

/// Whose the place of `s` is, for the location `id` (None: a location not added yet)
pub async fn marker_state(st: &AppState, id: Option<&str>, s: &dyn Storage) -> AppResult<Marker> {
    let read = read_marker(s).await.map_err(|e| AppError::bad_request(describe(&e)))?;
    let Some((location, install)) = read else { return Ok(Marker::Missing) };
    let ours = install_id(st).await?;
    if !install.is_empty() && install != ours {
        return Ok(Marker::Taken);
    }
    if Some(location.as_str()) == id {
        return Ok(Marker::Ours);
    }
    // A location of this installation that was deleted without its marker being removed: the place is free
    let exists: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM storage_locations WHERE id = ?").bind(&location).fetch_optional(&st.db).await?;
    Ok(if exists.is_some() { Marker::Taken } else { Marker::Missing })
}

/// Writes the marker of the location `id` in the place of `s`
async fn write_marker(st: &AppState, id: &str, s: &dyn Storage) -> AppResult<()> {
    let body = format!("{id}\n{}\n", install_id(st).await?);
    let tmp = st.data_dir.join("tmp").join(format!("marker-{}", new_id()));
    tokio::fs::write(&tmp, body).await?;
    let put = s.put_at(storage::LOCATION_MARKER, &tmp).await;
    let _ = tokio::fs::remove_file(&tmp).await;
    put.map_err(|e| AppError::bad_request(describe(&e)))
}

/// Remote locations whose marker was found or written since the server started (health checks read it once)
static MARKED: std::sync::Mutex<std::collections::BTreeSet<String>> = std::sync::Mutex::new(std::collections::BTreeSet::new());

/// Makes the place of `s` the location `id`'s: refused when its marker names another location or installation,
/// written when there is none (and completed with this installation's id). Returns whether it was written.
pub async fn claim_place(st: &AppState, id: &str, s: &dyn Storage) -> AppResult<bool> {
    match marker_state(st, Some(id), s).await? {
        Marker::Taken => Err(AppError::conflict(storage::PLACE_TAKEN)),
        Marker::Ours if read_marker(s).await.ok().flatten().is_some_and(|(_, install)| !install.is_empty()) => {
            MARKED.lock().unwrap().insert(id.to_string());
            Ok(false)
        }
        _ => {
            write_marker(st, id, s).await?;
            MARKED.lock().unwrap().insert(id.to_string());
            Ok(true)
        }
    }
}

/// Removes the marker of the location `id` from the place of `s`, when it holds that one (the location is deleted,
/// adding it failed, or it moved to another place)
async fn release_place(st: &AppState, id: &str, s: &dyn Storage) {
    MARKED.lock().unwrap().remove(id);
    if matches!(marker_state(st, Some(id), s).await, Ok(Marker::Ours))
        && let Err(e) = s.delete_at(storage::LOCATION_MARKER).await
    {
        tracing::warn!("Couldn't remove the {} file of storage location {id}: {e}", storage::LOCATION_MARKER);
    }
}

/// Before content of the location `id` is looked for or removed as unused: an S3, SFTP or FTP location's place must
/// be its own (a Local folder location's folder is checked on every use)
pub async fn require_own_place(st: &AppState, id: &str, kind: &str, s: &dyn Storage) -> AppResult<()> {
    if kind == "local" {
        return Ok(());
    }
    claim_place(st, id, s).await.map(|_| ())
}

/// After a successful health check: an S3, SFTP or FTP location added before markers were written gets one
async fn mark_after_check(st: &AppState, id: &str, s: &dyn Storage) {
    if MARKED.lock().unwrap().contains(id) {
        return;
    }
    let kind: Option<(String,)> = sqlx::query_as("SELECT kind FROM storage_locations WHERE id = ?").bind(id).fetch_optional(&st.db).await.ok().flatten();
    match kind {
        Some((kind,)) if kind != "local" => match claim_place(st, id, s).await {
            Ok(true) => tracing::info!("Wrote the {} file of storage location {id}", storage::LOCATION_MARKER),
            Ok(false) => {}
            Err(e) => tracing::warn!("Storage location {id}: {}", e.message),
        },
        _ => {
            MARKED.lock().unwrap().insert(id.to_string());
        }
    }
}

/// Where a location keeps its files, for telling two locations in the same place apart: the kind, the service and the
/// bucket and prefix (S3), or the server and folder (SFTP, FTP; with the account when the folder is relative to its
/// home folder). None for Local folder locations, whose folders hold a marker checked on every use.
fn place_of(kind: &str, cfg: &Value) -> Option<String> {
    let text = |f: &str| cfg.get(f).and_then(Value::as_str).unwrap_or_default().trim().to_string();
    match kind {
        "s3" => {
            let mut endpoint = text("endpoint").trim_end_matches('/').to_ascii_lowercase();
            let mut bucket = text("bucket");
            // Pasted as https://host/bucket (as storage::normalize reads it)
            if let Some((scheme, rest)) = endpoint.clone().split_once("://") {
                let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
                if bucket.is_empty() {
                    bucket = path.split('/').next().unwrap_or_default().to_string();
                }
                endpoint = format!("{scheme}://{host}");
            }
            Some(format!("s3 {endpoint} {bucket} {}", text("prefix").trim_matches('/')))
        }
        "sftp" | "ftp" => {
            let cfg = storage::normalize_host(cfg.clone());
            let text = |f: &str| cfg.get(f).and_then(Value::as_str).unwrap_or_default().trim().to_string();
            let default_port = if kind == "sftp" { 22 } else { 21 };
            let port = cfg.get("port").and_then(Value::as_u64).filter(|p| *p != 0).unwrap_or(default_port);
            let path = text("path");
            let path = path.trim_end_matches('/');
            let path = if path == "." { "" } else { path };
            // A relative folder is in the account's home folder
            let account = if path.starts_with('/') { String::new() } else { text("username") };
            Some(format!("{kind} {} {port} {account} {path}", text("host").to_ascii_lowercase()))
        }
        _ => None,
    }
}

/// Refuses settings whose place (`place_of`) another location already uses: "Remove unused content" in one would
/// delete the other's files
async fn check_place_free(conn: &mut SqliteConnection, id: Option<&str>, kind: &str, cfg: &Value) -> AppResult<()> {
    let Some(place) = place_of(kind, cfg) else { return Ok(()) };
    let rows: Vec<(String, String, String)> = sqlx::query_as("SELECT id, name, config FROM storage_locations WHERE kind = ?").bind(kind).fetch_all(conn).await?;
    for (other, name, raw) in rows {
        if Some(other.as_str()) != id && place_of(kind, &config_json(&other, &raw)).as_deref() == Some(place.as_str()) {
            return Err(AppError::conflict(format!("The storage location \"{name}\" already uses this place (the same bucket and prefix, or the same server and folder)")));
        }
    }
    Ok(())
}

/// How the places of two locations relate, for copies, backups and replicas between them
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    /// Apart: another disk or another service
    Apart,
    /// On the same disk of this server, or the same server or bucket of a storage service: a copy on one doesn't
    /// survive a failure of the other
    Shared,
    /// The same place, or one inside the other: refused
    Nested,
}

/// Where a location keeps its files: what it is on (a disk of this server, or a server, bucket or account of a
/// storage service) and its folder or prefix there, as parts
async fn physical(st: &AppState, id: &str) -> AppResult<(String, Vec<String>)> {
    let (kind, raw): (String, String) = sqlx::query_as("SELECT kind, config FROM storage_locations WHERE id = ?")
        .bind(id)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Storage location not found"))?;
    let cfg = config_json(id, &raw);
    let parts = |path: &str| path.split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".").map(str::to_string).collect::<Vec<_>>();
    if kind == "local" {
        let root = storage::local_root(id, &cfg, &st.storage_dir).map_err(|e| AppError::bad_request(e.to_string()))?;
        let abs = std::path::absolute(&root).unwrap_or(root);
        let text = abs.to_string_lossy().to_string();
        // Windows doesn't tell letter case apart in paths
        let text = if cfg!(windows) { text.to_lowercase() } else { text };
        let disk = crate::usage::sample::disk_of_path(&abs).await.unwrap_or_default();
        return Ok((format!("disk {disk}"), parts(&text)));
    }
    let place = place_of(&kind, &cfg).unwrap_or_default();
    // "s3 <endpoint> <bucket> <prefix>" and "<kind> <host> <port> <account> <path>": the service, then the folder
    let fields: Vec<&str> = place.splitn(if kind == "s3" { 4 } else { 5 }, ' ').collect();
    let (service, folder) = fields.split_at(fields.len().saturating_sub(1));
    Ok((service.join(" "), parts(folder.first().copied().unwrap_or_default())))
}

/// How the places of the locations `a` and `b` relate (`Relation`)
pub async fn relation(st: &AppState, a: &str, b: &str) -> AppResult<Relation> {
    if a == b {
        return Ok(Relation::Nested);
    }
    let ((sa, pa), (sb, pb)) = (physical(st, a).await?, physical(st, b).await?);
    let local = |s: &str| s.starts_with("disk ");
    if local(&sa) && local(&sb) {
        // Folders of this server: nested whatever the disks say
        if pa.starts_with(&pb) || pb.starts_with(&pa) {
            return Ok(Relation::Nested);
        }
        return Ok(if sa == sb && sa != "disk " { Relation::Shared } else { Relation::Apart });
    }
    if sa != sb {
        // The same server under another account or bucket still fails with it
        let server = |s: &str| s.split(' ').take(2).collect::<Vec<_>>().join(" ");
        return Ok(if !local(&sa) && server(&sa) == server(&sb) { Relation::Shared } else { Relation::Apart });
    }
    Ok(if pa.starts_with(&pb) || pb.starts_with(&pa) { Relation::Nested } else { Relation::Shared })
}

// ───────────── Connection health monitoring ─────────────

/// Sends just one HEAD request every 30 seconds (about US$0.03 per month on AWS); a disconnect shows as offline in the UI within half a minute
const HEALTH_INTERVAL: Duration = Duration::from_secs(30);
/// Shorter check interval while a location is offline, so it's usable again soon after recovering
const OFFLINE_INTERVAL: Duration = Duration::from_secs(20);

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

fn set_health(st: &AppState, id: &str, error: Option<String>) {
    st.location_health.lock().unwrap().insert(id.to_string(), LocationHealth { ok: error.is_none(), error, checked_at: now() });
}

/// Whether a location can be reached now (a lightweight check that writes no data), within `PROBE_TIMEOUT`. The check
/// runs as a task of its own: one that doesn't come back (a mounted share whose server went away blocks a thread) is
/// never joined by another, so a location that stays unreachable ties up one thread, not one more every check.
pub async fn ping(st: &AppState, id: &str) -> Result<(), String> {
    let storage = st.storage(id).map_err(|e| e.message)?;
    // Counted apart from what people do (Storage usage)
    let check = async move { crate::usage::probe(storage.ping()).await.map_err(|e| describe(&e)) };
    // (the data folder tells servers apart, as tests run several at once)
    match util::within(format!("check of location {id} of {}", st.data_dir.display()), PROBE_TIMEOUT, check).await {
        Some(res) => res,
        None => Err("Connection timed out. Check that the service is running.".to_string()),
    }
}

/// Checks whether a location can be reached (`ping`) and updates its health status
pub async fn probe(st: &AppState, id: &str) -> Result<(), String> {
    let storage = st.storage(id).map_err(|e| e.message)?;
    let res = ping(st, id).await;
    if res.is_ok() {
        let _ = tokio::time::timeout(PROBE_TIMEOUT, crate::usage::probe(mark_after_check(st, id, storage.as_ref()))).await;
    }
    let was_ok = st.location_health.lock().unwrap().get(id).is_none_or(|h| h.ok);
    set_health(st, id, res.clone().err());
    match (&res, was_ok) {
        (Err(e), true) => tracing::warn!("Storage location {id} is unreachable: {e}"),
        (Ok(()), false) => tracing::info!("Storage location {id} is reachable again"),
        _ => {}
    }
    res
}

/// Retries a location's failed deletions in a task of its own, so a long batch (a slow or refusing storage service)
/// doesn't hold up the health checks of the other locations. At most one batch per location runs at a time.
fn retry_in_background(st: &AppState, id: &str) {
    static RUNNING: std::sync::Mutex<std::collections::BTreeSet<String>> = std::sync::Mutex::new(std::collections::BTreeSet::new());
    if !RUNNING.lock().unwrap().insert(id.to_string()) {
        return;
    }
    // Released when the task ends, also if it panics
    struct Running(String);
    impl Drop for Running {
        fn drop(&mut self) {
            RUNNING.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0);
        }
    }
    let (st, running) = (st.clone(), Running(id.to_string()));
    tokio::spawn(async move {
        let id = &running.0;
        // One line per location: failures of single files are only logged at debug level
        let (n, failed) = tree::retry_pending_deletes(&st, id).await;
        if n > 0 {
            tracing::info!("Retried deleting {n} physical files whose deletion failed earlier ({id}), {failed} failed again");
        }
    });
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
                    _ = crate::storage::RECHECK.notified() => tokio::time::sleep(Duration::from_secs(1)).await,
                }
            }
            first = false;
            let ids: Vec<String> = st.storages.read().unwrap().keys().cloned().collect();
            let results = futures_util::future::join_all(ids.iter().map(|id| probe(&st, id))).await;
            for (id, res) in ids.iter().zip(results) {
                if res.is_ok() {
                    retry_in_background(&st, id);
                }
            }
        }
    });
}

/// What connecting does with a Local folder location's folder (storage.rs, `LOCATION_MARKER`)
#[derive(Clone, Copy)]
enum Folder<'a> {
    /// Used as it is: it must hold the marker of the location with this id
    Existing(&'a str),
    /// An administrator adds the location with this id, or changes its folder: the folder is created when it isn't
    /// there, and gets the location's marker
    Claim(&'a str),
    /// Settings tried before they are saved (the location's id when it is being edited): nothing is created
    Try(Option<&'a str>),
}

/// Tidies up the settings, builds the backend and runs a connection test (write, read back and delete a small file); returns the tidied settings to save
async fn connect(st: &AppState, kind: &str, config: Value, folder: Folder<'_>) -> AppResult<(Arc<dyn Storage>, Value)> {
    let config = storage::normalize(kind, config).await;
    storage::check_insecure_target(kind, &config).await.map_err(AppError::bad_request)?;
    let id = match folder {
        Folder::Existing(id) | Folder::Claim(id) => id,
        Folder::Try(id) => id.unwrap_or_default(),
    };
    let invalid = |e: std::io::Error| AppError::bad_request(format!("Invalid settings: {e}"));
    let backend = storage::build(id, kind, &config, &st.storage_dir).map_err(invalid)?;
    let checked = async {
        if kind != "local" {
            return backend.check().await;
        }
        let root = storage::local_root(id, &config, &st.storage_dir)?;
        match folder {
            Folder::Existing(_) => backend.check().await,
            Folder::Claim(id) => {
                let (dir, owner) = (root.clone(), id.to_string());
                let wrote = tokio::task::spawn_blocking(move || storage::claim_folder(&dir, &owner)).await.map_err(std::io::Error::other)??;
                let checked = backend.check().await;
                if checked.is_err() && wrote {
                    storage::release_folder(&root, id).await;
                }
                checked
            }
            Folder::Try(_) => try_folder(&root, id).await,
        }
    };
    tokio::time::timeout(Duration::from_secs(20), checked)
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

/// Tries a Local folder location's folder before its settings are saved, creating nothing: a folder of another
/// location is refused; a folder that is there is written to; a folder that isn't there yet (it is created when the
/// settings are saved) is tried in the nearest folder above it that is
async fn try_folder(root: &FsPath, id: &str) -> std::io::Result<()> {
    match storage::marker_of(root).await? {
        Some(m) if !id.is_empty() && m == id => storage::LocalStorage::new(root.to_path_buf(), id).check().await,
        Some(_) => Err(std::io::Error::other(storage::StorageError { message: storage::FOLDER_TAKEN, detail: root.display().to_string() })),
        None => {
            let mut dir = root;
            while !tokio::fs::metadata(dir).await.is_ok_and(|m| m.is_dir()) {
                dir = dir.parent().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no folder on the way is there"))?;
            }
            storage::write_probe(dir).await
        }
    }
}

/// Whether the settings of the location `id` point at another folder than the saved ones (Local folder locations)
fn folder_changed(kind: &str, saved: &Value, config: &Value) -> bool {
    let path = |c: &Value| c["path"].as_str().map(str::trim).unwrap_or_default().to_string();
    kind == "local" && path(saved) != path(config)
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
    // An existing location's folder, unchanged: it must be there with its marker
    let saved = match req.id.as_deref() {
        Some(id) => sqlx::query_as::<_, (String,)>("SELECT config FROM storage_locations WHERE id = ?")
            .bind(id)
            .fetch_optional(&st.db)
            .await?
            .map(|(c,)| config_json(id, &c)),
        None => None,
    };
    let folder = match (req.id.as_deref(), saved) {
        (Some(id), Some(saved)) if !folder_changed(&req.kind, &saved, &config) => Folder::Existing(id),
        (id, _) => Folder::Try(id),
    };
    check_place_free(&mut *st.db.acquire().await?, req.id.as_deref(), &req.kind, &config).await?;
    let (backend, config) = connect(&st, &req.kind, config, folder).await?;
    // Nothing is written: a place that holds another location's marker is refused
    if req.kind != "local" && marker_state(&st, req.id.as_deref(), backend.as_ref()).await? == Marker::Taken {
        return Err(AppError::conflict(storage::PLACE_TAKEN));
    }
    Ok(Json(json!({ "ok": true, "region": config.get("region"), "host_key": config.get("host_key") })))
}

pub async fn test_existing(State(st): State<AppState>, _: Admin, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let row: LocationRow = sqlx::query_as("SELECT id, name, kind, config, is_default FROM storage_locations WHERE id = ?")
        .bind(&id)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Storage location not found"))?;
    let backend = connect(&st, &row.kind, config_json(&row.id, &row.config), Folder::Existing(&id)).await;
    set_health(&st, &id, backend.as_ref().err().map(|e| e.message.clone()));
    match backend {
        Ok((b, _)) => {
            if row.kind != "local" {
                claim_place(&st, &id, b.as_ref()).await?;
            }
            st.storages.write().unwrap().insert(id.clone(), b);
            // In the background: up to 1000 deletions on a slow storage service shouldn't hold the request
            retry_in_background(&st, &id);
            Ok(Json(json!({ "ok": true })))
        }
        Err(e) => Err(e),
    }
}

pub async fn create(State(st): State<AppState>, Admin(user): Admin, Json(req): Json<LocationReq>) -> AppResult<Json<Value>> {
    let name = validate_name(req.name.as_deref().unwrap_or_default())?;
    let kind = req.kind.unwrap_or_default();
    let config = req.config.unwrap_or_else(|| json!({}));
    crate::folders::check_location_folder(&st, None, &kind, &config).await?;
    check_place_free(&mut *st.db.acquire().await?, None, &kind, &config).await?;
    let id = new_id();
    // A Local folder location's folder is created here, with the location's marker
    let (backend, config) = connect(&st, &kind, config, Folder::Claim(&id)).await?;
    // An S3, SFTP or FTP location's place gets its marker too
    let wrote = if kind == "local" { false } else { claim_place(&st, &id, backend.as_ref()).await? };
    let saved = async {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            check_place_free(&mut tx, None, &kind, &config).await?;
            sqlx::query("INSERT INTO storage_locations (id, name, kind, config, is_default, created_at) VALUES (?, ?, ?, ?, 0, ?)")
                .bind(&id)
                .bind(&name)
                .bind(&kind)
                .bind(sealed_config(&id, &config))
                .bind(now())
                .execute(&mut *tx)
                .await?;
            logs::record_activity(&mut tx, &user, None, "storage_create", &name).await?;
            AppResult::Ok(())
        }
        .await;
        crate::db::settle(tx, res).await
    }
    .await;
    if let Err(e) = saved {
        if kind == "local"
            && let Ok(root) = storage::local_root(&id, &config, &st.storage_dir)
        {
            storage::release_folder(&root, &id).await;
        }
        if wrote {
            release_place(&st, &id, backend.as_ref()).await;
        }
        return Err(e);
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
    let saved = config_json(&id, &row.config);
    // A Local folder location's folder changed while spaces or content are on it: from the old folder to the new one
    let mut moved: Option<(std::path::PathBuf, std::path::PathBuf)> = None;
    let mut changed_folder = false;
    // The built-in local location can only be renamed
    let new_backend = match (&req.config, id == BUILTIN) {
        (Some(cfg), false) => {
            let merged = merged_config(&st, Some(&id), &row.kind, cfg.clone()).await?;
            crate::folders::check_location_folder(&st, Some(&id), &row.kind, &merged).await?;
            check_place_free(&mut *st.db.acquire().await?, Some(&id), &row.kind, &merged).await?;
            changed_folder = folder_changed(&row.kind, &saved, &merged);
            let folder = if !changed_folder {
                Folder::Existing(&id)
            } else if in_use(&mut *st.db.acquire().await?, &id).await? {
                // The spaces' folders and the content are in the old folder: the new one must be a copy of it,
                // marker included, and the spaces' folders follow
                let invalid = |e: std::io::Error| AppError::bad_request(e.to_string());
                let new_root = storage::local_root(&id, &merged, &st.storage_dir).map_err(invalid)?;
                if storage::marker_of(&new_root).await.ok().flatten().as_deref() != Some(id.as_str()) {
                    return Err(AppError::conflict(IN_USE));
                }
                moved = Some((storage::local_root(&id, &saved, &st.storage_dir).map_err(invalid)?, new_root));
                Folder::Existing(&id)
            } else {
                // Nothing on it: the new folder is created here, with the marker
                Folder::Claim(&id)
            };
            let (backend, config) = connect(&st, &row.kind, merged, folder).await?;
            if row.kind != "local" {
                claim_place(&st, &id, backend.as_ref()).await?;
            }
            Some((backend, config))
        }
        _ => None,
    };
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            sqlx::query("UPDATE storage_locations SET name = ? WHERE id = ?").bind(&name).bind(&id).execute(&mut *tx).await?;
            if let Some((_, cfg)) = &new_backend {
                check_place_free(&mut tx, Some(&id), &row.kind, cfg).await?;
                match &moved {
                    Some((from, to)) => follow_folder(&mut tx, &id, from, to).await?,
                    // Something was put on it meanwhile
                    None if changed_folder && in_use(&mut tx, &id).await? => return Err(AppError::conflict(IN_USE)),
                    None => {}
                }
                sqlx::query("UPDATE storage_locations SET config = ? WHERE id = ?").bind(sealed_config(&id, cfg)).bind(&id).execute(&mut *tx).await?;
            }
            logs::record_activity(&mut tx, &user, None, "storage_update", &name).await?;
            AppResult::Ok(())
        }
        .await;
        crate::db::settle(tx, res).await?;
    }
    if let Some((backend, cfg)) = new_backend {
        let place_moved = row.kind != "local" && place_of(&row.kind, &saved) != place_of(&row.kind, &cfg);
        let old = st.storages.write().unwrap().insert(id.clone(), backend);
        // Just checked
        set_health(&st, &id, None);
        if moved.is_some() {
            crate::folders::spaces_changed();
        }
        // An S3, SFTP or FTP location moved to another place: the old one is free again
        if place_moved && let Some(old) = old {
            release_place(&st, &id, old.as_ref()).await;
            MARKED.lock().unwrap().insert(id.clone());
        }
    }
    Ok(Json(json!({ "ok": true })))
}

/// Shown when a Local folder location's folder is changed while spaces or content are on it, to a folder that isn't
/// a copy of it
const IN_USE: &str = "Spaces or files are on this location, so its folder can only change to a copy of it. Copy everything in the folder to the new one first, including its .thirtyfile-location file, then change the folder.";

/// Whether spaces or content are on the location `id`, or being moved to or from it
async fn in_use(conn: &mut SqliteConnection, id: &str) -> AppResult<bool> {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM drives WHERE location_id = ?1) + (SELECT COUNT(*) FROM blobs WHERE location_id = ?1)",
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(n > 0
        || crate::moves::location_busy(conn, id).await?
        || crate::backups::sets_on(conn, id).await? > 0
        || crate::replicas::location_used(conn, id).await? != (0, false))
}

/// The folder spaces of the location `id` follow its folder from `from` to `to`
async fn follow_folder(conn: &mut SqliteConnection, id: &str, from: &FsPath, to: &FsPath) -> AppResult<()> {
    if crate::moves::location_busy(&mut *conn, id).await? {
        return Err(AppError::conflict("A space is being moved to or from this location. Wait until the move finishes, or cancel it."));
    }
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT id, source_path FROM drives WHERE location_id = ? AND source_path IS NOT NULL")
        .bind(id)
        .fetch_all(&mut *conn)
        .await?;
    let from_abs = std::path::absolute(from).unwrap_or_else(|_| from.to_path_buf());
    for (drive, source) in rows {
        let source = std::path::PathBuf::from(source);
        let rel = source
            .strip_prefix(from)
            .or_else(|_| source.strip_prefix(&from_abs))
            .map_err(|_| AppError::internal(format!("The folder of space {drive} isn't in its location's folder")))?;
        sqlx::query("UPDATE drives SET source_path = ? WHERE id = ?").bind(to.join(rel).to_string_lossy()).bind(&drive).execute(&mut *conn).await?;
    }
    Ok(())
}

pub async fn set_default(State(st): State<AppState>, Admin(user): Admin, Path(id): Path<String>) -> AppResult<Json<Value>> {
    if !st.storages.read().unwrap().contains_key(&id) {
        return Err(AppError::bad_request("This storage location can't be reached right now, so it can't be set as the default"));
    }
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    // Check that it exists first (the UPDATE affects all rows, so rows_affected can't tell)
    let (name,): (String,) = sqlx::query_as("SELECT name FROM storage_locations WHERE id = ?")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::not_found("Storage location not found"))?;
    sqlx::query("UPDATE storage_locations SET is_default = (id = ?)").bind(&id).execute(&mut *tx).await?;
    logs::record_activity(&mut tx, &user, None, "storage_default", &name).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete(State(st): State<AppState>, Admin(user): Admin, Path(id): Path<String>) -> AppResult<Json<Value>> {
    if id == BUILTIN {
        return Err(AppError::bad_request("The built-in local disk can't be deleted"));
    }
    let w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let (name, kind, config, is_default, blobs, drives): (String, String, String, bool, i64, i64) = sqlx::query_as(
        "SELECT name, kind, config, is_default, (SELECT COUNT(*) FROM blobs WHERE location_id = ?1), (SELECT COUNT(*) FROM drives WHERE location_id = ?1)
         FROM storage_locations WHERE id = ?1",
    )
    .bind(&id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("Storage location not found"))?;
    if is_default {
        return Err(AppError::bad_request("Set another location as the default first"));
    }
    // Every space records its location (folder spaces on a Local folder location too), so this counts them all
    if drives > 0 {
        return Err(AppError::bad_request(if drives == 1 {
            format!("{drives} space still uses this location. Move it to another location first.")
        } else {
            format!("{drives} spaces still use this location. Move them to another location first.")
        }));
    }
    // A space being moved to it isn't on it yet, but content is being copied there
    if crate::moves::location_busy(&mut tx, &id).await? {
        return Err(AppError::bad_request("A space is being moved to or from this location. Wait until the move finishes, or cancel it."));
    }
    if blobs > 0 {
        return Err(AppError::bad_request(format!(
            "This location still stores {blobs} {}. Move the spaces that use it to another location first.",
            if blobs == 1 { "file" } else { "files" }
        )));
    }
    // Copies kept there would be lost, and jobs reading from it would fail
    let sets = crate::backups::sets_on(&mut tx, &id).await?;
    if sets > 0 {
        return Err(AppError::bad_request(if sets == 1 {
            "This location holds 1 copy. Delete it in Control panel › Backups first.".to_string()
        } else {
            format!("This location holds {sets} copies. Delete them in Control panel › Backups first.")
        }));
    }
    // Replicas kept there, or a replica policy from or to it
    let (copies, replicated) = crate::replicas::location_used(&mut tx, &id).await?;
    if replicated {
        return Err(AppError::bad_request("A replica policy copies from or to this location. Change or delete it in Control panel › Replicas first."));
    }
    if copies > 0 {
        return Err(AppError::bad_request("This location holds replicas. Remove them in Control panel › Replicas first."));
    }
    if crate::backups::location_busy(&mut tx, &id).await? {
        return Err(AppError::bad_request("A copy is being made from or to this location. Wait until it finishes, or cancel it."));
    }
    sqlx::query("DELETE FROM storage_locations WHERE id = ?").bind(&id).execute(&mut *tx).await?;
    // Content that couldn't be deleted there stays in that storage: ThirtyFile no longer connects to it
    sqlx::query("DELETE FROM pending_blob_deletes WHERE location_id = ?").bind(&id).execute(&mut *tx).await?;
    logs::record_activity(&mut tx, &user, None, "storage_delete", &name).await?;
    tx.commit().await?;
    drop(w);
    let backend = st.storages.write().unwrap().remove(&id);
    st.location_health.lock().unwrap().remove(&id);
    // An S3, SFTP or FTP location's place no longer names it either
    if kind != "local"
        && let Some(backend) = backend
    {
        release_place(&st, &id, backend.as_ref()).await;
    }
    // Its folder no longer names it, so a location can be added there again
    if kind == "local"
        && let Ok(root) = storage::local_root(&id, &config_json(&id, &config), &st.storage_dir)
    {
        storage::release_folder(&root, &id).await;
    }
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    /// Adds a storage location (in the database and connected); `folder`: a Local folder location's folder, else one
    /// that works like a bucket (its content in a folder of the test)
    async fn add_location(env: &testutil::TestEnv, id: &str, folder: Option<&FsPath>) {
        let (kind, config) = match folder {
            Some(dir) => {
                std::fs::create_dir_all(dir).unwrap();
                ("local", json!({ "path": dir.to_string_lossy() }))
            }
            None => ("s3", json!({ "bucket": "files" })),
        };
        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, is_default, created_at) VALUES (?, ?, ?, ?, 0, 0)")
            .bind(id)
            .bind(id.to_uppercase())
            .bind(kind)
            .bind(config.to_string())
            .execute(&env.st.db)
            .await
            .unwrap();
        let backend = crate::storage::LocalStorage::create(folder.map_or_else(|| env.dir.join(id), FsPath::to_path_buf), id).unwrap();
        env.st.storages.write().unwrap().insert(id.into(), Arc::new(backend));
    }

    async fn make_default(env: &testutil::TestEnv, id: &str) {
        let _ = set_default(State(env.st.clone()), Admin(env.admin().await), Path(id.into())).await.unwrap();
    }

    /// A space's recorded location and mode
    async fn placed(env: &testutil::TestEnv, root_id: &str) -> (Option<String>, String) {
        sqlx::query_as("SELECT location_id, mode FROM drives WHERE root_id = ?").bind(root_id).fetch_one(&env.st.db).await.unwrap()
    }

    async fn new_team(env: &testutil::TestEnv, name: &str) -> String {
        let req = serde_json::from_value(json!({ "name": name })).unwrap();
        let Json(info) = crate::drives::create(State(env.st.clone()), env.admin().await, Json(req)).await.unwrap();
        serde_json::to_value(&info).unwrap()["root_id"].as_str().unwrap().to_string()
    }

    fn at(location: &str, mode: &str) -> (Option<String>, String) {
        (Some(location.into()), mode.into())
    }

    #[tokio::test]
    async fn spaces_keep_the_location_they_were_created_on() {
        let env = testutil::folders_env().await;
        let admin = env.admin().await;
        let amy = env.user("amy", true).await;
        let sales = new_team(&env, "Sales").await;
        // Every kind of space, on the built-in location
        for root in [admin.root(), amy.root(), &env.st.shared_root().unwrap(), &sales] {
            assert_eq!(placed(&env, root).await, at(BUILTIN, "folder"));
        }
        // A folder the administrator chose is on no location
        let shown = env.folder_space("Scans").await;
        assert_eq!(placed(&env, &shown.root).await, (None, "folder".into()));

        // A Local folder location as the default: new spaces get their folder there, the earlier ones stay
        let nas = env.dir.join("nas");
        add_location(&env, "nas", Some(&nas)).await;
        make_default(&env, "nas").await;
        let ben = env.user("ben", true).await;
        let plans = new_team(&env, "Plans").await;
        assert_eq!(placed(&env, ben.root()).await, at("nas", "folder"));
        assert_eq!(placed(&env, &plans).await, at("nas", "folder"));
        assert!(nas.join("users").join("ben").is_dir() && nas.join("teams").join("Plans").is_dir());
        assert_eq!(placed(&env, amy.root()).await, at(BUILTIN, "folder"));
        assert_eq!(placed(&env, &sales).await, at(BUILTIN, "folder"));

        // A bucket as the default: new spaces keep the content store there
        add_location(&env, "bucket", None).await;
        make_default(&env, "bucket").await;
        let carl = env.user("carl", true).await;
        assert_eq!(placed(&env, carl.root()).await, at("bucket", "store"));

        // Changing the default again moves nothing: Carl's new files still go to the bucket
        make_default(&env, BUILTIN).await;
        assert_eq!(placed(&env, carl.root()).await, at("bucket", "store"));
        assert_eq!(placed(&env, ben.root()).await, at("nas", "folder"));
        let carls = env.drive_of(carl.root()).await;
        assert_eq!(tree::drive_location(&mut env.st.db.acquire().await.unwrap(), &carls).await.unwrap(), "bucket");
        let id = env.upload(&carl, carl.root(), "a.txt", b"carl's").await;
        let (location,): (String,) = sqlx::query_as("SELECT b.location_id FROM nodes n JOIN blobs b ON b.hash = n.blob_hash WHERE n.id = ?")
            .bind(&id)
            .fetch_one(&env.st.db)
            .await
            .unwrap();
        assert_eq!(location, "bucket");
    }

    #[tokio::test]
    async fn the_list_counts_folder_spaces_and_the_content_store() {
        let env = testutil::folders_env().await;
        let amy = env.user("amy", true).await;
        env.upload(&amy, amy.root(), "notes.txt", b"twelve bytes").await;
        let company = env.st.shared_root().unwrap();
        env.upload(&env.admin().await, &company, "plan.txt", b"plan").await;
        // A space on a bucket, whose files are in the content store there
        add_location(&env, "bucket", None).await;
        make_default(&env, "bucket").await;
        let ben = env.user("ben", true).await;
        env.upload(&ben, ben.root(), "a.txt", b"ben's file").await;
        // A folder chosen by the administrator counts on no location
        let shown = env.folder_space("Scans").await;
        testutil::write_old(&shown.dir.join("scan.pdf"), b"%PDF-1.7 elsewhere");
        crate::folders::scan(&env.st, &shown.drive).await.unwrap();

        let Json(list) = list(State(env.st.clone()), Admin(env.admin().await)).await.unwrap();
        let list = serde_json::to_value(&list).unwrap();
        let find = |id: &str| list.as_array().unwrap().iter().find(|l| l["id"] == id).unwrap().clone();
        let local = find(BUILTIN);
        // "My files" of the administrator and Amy, and "All files"
        assert_eq!((local["drive_count"].as_i64(), local["used_bytes"].as_i64(), local["folder_bytes"].as_i64()), (Some(3), Some(16), Some(16)));
        assert_eq!(local["blob_count"], 0);
        assert!(!cfg!(any(unix, windows)) || local["disk_total_bytes"].as_u64().unwrap() > 0);
        assert!(!cfg!(any(unix, windows)) || local["disk_free_bytes"].as_u64().is_some());
        let bucket = find("bucket");
        assert_eq!((bucket["drive_count"].as_i64(), bucket["used_bytes"].as_i64(), bucket["blob_count"].as_i64()), (Some(1), Some(10), Some(1)));
        assert_eq!(bucket["folder_bytes"], 0);
        assert!(bucket["disk_total_bytes"].is_null(), "a bucket isn't a disk of this server");

        // The spaces on a location: what they are and their size
        let Json(on_local) = spaces(State(env.st.clone()), Admin(env.admin().await), Path(BUILTIN.into())).await.unwrap();
        let on_local = serde_json::to_value(&on_local).unwrap();
        let rows: Vec<(String, String, i64)> = on_local
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["kind"].as_str().unwrap().into(), s["owner_name"].as_str().unwrap().into(), s["used_bytes"].as_i64().unwrap()))
            .collect();
        assert_eq!(rows, [("company".into(), "".into(), 4), ("personal".into(), "admin".into(), 0), ("personal".into(), "amy".into(), 12)]);
        let Json(on_bucket) = spaces(State(env.st.clone()), Admin(env.admin().await), Path("bucket".into())).await.unwrap();
        assert_eq!(serde_json::to_value(&on_bucket).unwrap()[0]["mode"], "store");
    }

    #[tokio::test]
    async fn a_location_holding_a_folder_space_cant_be_deleted() {
        let env = testutil::folders_env().await;
        let nas = env.dir.join("nas");
        add_location(&env, "nas", Some(&nas)).await;
        make_default(&env, "nas").await;
        let amy = env.user("amy", true).await;
        make_default(&env, BUILTIN).await;
        // Nothing of Amy's is in the content store: her space alone keeps the location
        let (blobs,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM blobs").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(blobs, 0);
        let err = delete(State(env.st.clone()), Admin(env.admin().await), Path("nas".into())).await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err.message, "1 space still uses this location. Move it to another location first.");
        assert_eq!(placed(&env, amy.root()).await, at("nas", "folder"));
    }

    /// A folder outside the test's data folder (Local folder locations can't be inside it), removed when dropped
    struct Outside(std::path::PathBuf);

    impl Outside {
        fn new() -> Outside {
            let dir = std::env::temp_dir().join(format!("thirtyfile-disk-{}", new_id()));
            std::fs::create_dir_all(&dir).unwrap();
            Outside(dir)
        }
    }

    impl Drop for Outside {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn local(dir: &FsPath) -> Value {
        json!({ "kind": "local", "config": { "path": dir.to_string_lossy() } })
    }

    async fn add_local(env: &testutil::TestEnv, name: &str, dir: &FsPath) -> AppResult<String> {
        let mut req = local(dir);
        req["name"] = name.into();
        let Json(v) = create(State(env.st.clone()), Admin(env.admin().await), Json(serde_json::from_value(req).unwrap())).await?;
        Ok(v["id"].as_str().unwrap().to_string())
    }

    async fn move_to(env: &testutil::TestEnv, id: &str, dir: &FsPath) -> AppResult<()> {
        let req = serde_json::from_value(json!({ "config": { "path": dir.to_string_lossy() } })).unwrap();
        update(State(env.st.clone()), Admin(env.admin().await), Path(id.into()), Json(req)).await.map(|_| ())
    }

    fn marker(dir: &FsPath) -> Option<String> {
        std::fs::read_to_string(dir.join(storage::LOCATION_MARKER)).ok()
    }

    #[tokio::test]
    async fn a_local_folder_location_is_used_only_while_its_folder_holds_its_marker() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let disk = Outside::new();
        let dir = disk.0.join("nas").join("thirtyfile");
        let away = disk.0.join("away");

        // Trying the settings before adding creates nothing; adding creates the folder with the location's marker
        let _ = test(State(env.st.clone()), Admin(admin.clone()), Json(serde_json::from_value(local(&dir)).unwrap())).await.unwrap();
        assert!(!disk.0.join("nas").exists());
        let id = add_local(&env, "NAS", &dir).await.unwrap();
        assert_eq!(marker(&dir).as_deref(), Some(id.as_str()));
        probe(&env.st, &id).await.unwrap();
        make_default(&env, &id).await;
        let team = new_team(&env, "Sales").await;
        make_default(&env, BUILTIN).await;
        env.upload(&admin, &team, "before.txt", b"before").await;

        // Not mounted: the folder is gone. The location is unavailable, and nothing is made or written.
        std::fs::rename(&dir, &away).unwrap();
        let unmounted = |what: &str| (axum::http::StatusCode::SERVICE_UNAVAILABLE, storage::NOT_MOUNTED.to_string(), what.to_string());
        for what in ["missing", "an empty mount point", "another location's folder"] {
            match what {
                "an empty mount point" => std::fs::create_dir_all(&dir).unwrap(),
                "another location's folder" => std::fs::write(dir.join(storage::LOCATION_MARKER), "another").unwrap(),
                _ => {}
            }
            assert_eq!(probe(&env.st, &id).await.unwrap_err(), storage::NOT_MOUNTED, "{what}");
            assert_eq!(env.st.location_offline(&id).as_deref(), Some(storage::NOT_MOUNTED));
            let err = env.try_upload(&admin, &team, "after.txt", b"after").await.unwrap_err();
            assert_eq!((err.status, err.message, what.to_string()), unmounted(what));
            let err = test_existing(State(env.st.clone()), Admin(admin.clone()), Path(id.clone())).await.unwrap_err();
            assert_eq!(err.message, storage::NOT_MOUNTED, "{what}");
            // Its settings can't be saved either while the folder isn't there
            assert!(move_to(&env, &id, &dir).await.is_err(), "{what}");
            let steps = crate::location_tools::test_steps(State(env.st.clone()), Admin(admin.clone()), Path(id.clone())).await.unwrap();
            assert!(!steps.ok && steps.step("connect").unwrap().message.as_deref() == Some(storage::NOT_MOUNTED), "{what}");
        }
        assert_eq!(std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect::<Vec<_>>(), [storage::LOCATION_MARKER]);
        std::fs::remove_dir_all(&dir).unwrap();

        // Mounted again: available at the next check, without a restart
        std::fs::rename(&away, &dir).unwrap();
        probe(&env.st, &id).await.unwrap();
        assert_eq!(env.st.location_offline(&id), None);
        env.upload(&admin, &team, "after.txt", b"after").await;
    }

    #[tokio::test]
    async fn a_local_folder_location_takes_only_a_folder_of_its_own() {
        let env = testutil::env().await;
        let disk = Outside::new();
        let (first, second, other) = (disk.0.join("first"), disk.0.join("second"), disk.0.join("other"));
        let id = add_local(&env, "First", &first).await.unwrap();

        // A folder holding another location's marker is refused, when adding a location and when changing its folder
        storage::claim_folder(&other, "someone-else").unwrap();
        let err = add_local(&env, "Other", &other).await.unwrap_err();
        assert_eq!(err.message, storage::FOLDER_TAKEN);
        assert_eq!(move_to(&env, &id, &other).await.unwrap_err().message, storage::FOLDER_TAKEN);
        assert_eq!(marker(&other).as_deref(), Some("someone-else"));

        // Another folder is created with the marker; its earlier folder, which still holds its marker, is taken back
        move_to(&env, &id, &second).await.unwrap();
        assert_eq!(marker(&second).as_deref(), Some(id.as_str()));
        move_to(&env, &id, &first).await.unwrap();
        probe(&env.st, &id).await.unwrap();

        // A deleted location leaves its folder free for another
        let _ = delete(State(env.st.clone()), Admin(env.admin().await), Path(id.clone())).await.unwrap();
        assert_eq!(marker(&first), None);
        let again = add_local(&env, "Again", &first).await.unwrap();
        assert_eq!(marker(&first).as_deref(), Some(again.as_str()));
    }

    #[tokio::test]
    async fn a_folder_space_whose_folder_is_missing_is_never_made_again() {
        let env = testutil::folders_env().await;
        let admin = env.admin().await;
        let nas = env.dir.join("nas");
        add_location(&env, "nas", Some(&nas)).await;
        make_default(&env, "nas").await;
        let team = new_team(&env, "Plans").await;
        make_default(&env, BUILTIN).await;
        let drive = env.drive_of(&team).await;
        // The space's folder is marked as its own when it is created, before anything is written there
        assert_eq!(std::fs::read_to_string(nas.join("teams/Plans").join(crate::folders::MARKER)).unwrap(), drive);
        env.upload(&admin, &team, "a.txt", b"one").await;
        assert!(nas.join("teams/Plans/a.txt").is_file());

        let away = env.dir.join("nas-away");
        std::fs::rename(&nas, &away).unwrap();
        let _ = probe(&env.st, "nas").await;
        for mount_point in [false, true] {
            if mount_point {
                std::fs::create_dir(&nas).unwrap();
            }
            // Shown as offline, and changes are refused
            let Json(info) = crate::nodes::get(State(env.st.clone()), admin.clone(), Path(team.clone())).await.unwrap();
            assert_eq!(serde_json::to_value(&info).unwrap()["offline"], storage::NOT_MOUNTED);
            let err = env.try_upload(&admin, &team, "b.txt", b"two").await.unwrap_err();
            assert_eq!(err.status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
            assert!(err.message.starts_with(storage::NOT_MOUNTED), "{}", err.message);
            let req = serde_json::from_value(json!({ "parent_id": team, "name": "Docs" })).unwrap();
            let err = crate::nodes::create_folder(State(env.st.clone()), admin.clone(), Json(req)).await.unwrap_err();
            assert_eq!(err.message, storage::NOT_MOUNTED);
            // A scan keeps the index: the files aren't deleted, just not there right now
            let report = crate::folders::scan(&env.st, &drive).await.unwrap();
            assert!(report.error.is_some());
            assert!(env.node_at(&drive, "a.txt").await.is_some());
            assert_eq!(nas.exists(), mount_point, "nothing made");
            if mount_point {
                assert_eq!(std::fs::read_dir(&nas).unwrap().count(), 0, "nothing made in the empty mount point");
                std::fs::remove_dir(&nas).unwrap();
            }
        }

        // Another disk mounted there, with a folder at the same place: a scan doesn't take its items for the space's
        std::fs::create_dir_all(nas.join("teams/Plans")).unwrap();
        std::fs::write(nas.join(storage::LOCATION_MARKER), "another").unwrap();
        testutil::write_old(&nas.join("teams/Plans/other.txt"), b"other");
        let report = crate::folders::scan(&env.st, &drive).await.unwrap();
        assert_eq!(report.error.as_deref(), Some(storage::NOT_MOUNTED));
        assert!(env.node_at(&drive, "a.txt").await.is_some() && env.node_at(&drive, "other.txt").await.is_none());
        // ...and nothing is written into it, even with its own location's marker there: the folder isn't the space's
        std::fs::write(nas.join(storage::LOCATION_MARKER), "nas").unwrap();
        let err = env.try_upload(&admin, &team, "b.txt", b"two").await.unwrap_err();
        assert!(err.message.starts_with(storage::NOT_MOUNTED), "{}", err.message);
        let req = serde_json::from_value(json!({ "parent_id": team, "name": "Docs" })).unwrap();
        assert_eq!(crate::nodes::create_folder(State(env.st.clone()), admin.clone(), Json(req)).await.unwrap_err().message, storage::NOT_MOUNTED);
        let names = |dir: std::path::PathBuf| std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect::<Vec<_>>();
        assert_eq!(names(nas.join("teams/Plans")), ["other.txt"]);
        std::fs::remove_dir_all(&nas).unwrap();

        std::fs::rename(&away, &nas).unwrap();
        probe(&env.st, "nas").await.unwrap();
        env.upload(&admin, &team, "b.txt", b"two").await;
        assert!(nas.join("teams/Plans/b.txt").is_file());
    }

    #[tokio::test]
    async fn a_local_folder_location_in_use_moves_only_to_a_copy_of_its_folder() {
        let env = testutil::folders_env().await;
        let admin = env.admin().await;
        let disk = Outside::new();
        let (first, second, copy) = (disk.0.join("first"), disk.0.join("second"), disk.0.join("copy"));
        let id = add_local(&env, "NAS", &first).await.unwrap();
        make_default(&env, &id).await;
        let team = new_team(&env, "Sales").await;
        make_default(&env, BUILTIN).await;
        env.upload(&admin, &team, "plan.txt", b"the plan").await;
        let drive = env.drive_of(&team).await;

        // Another folder, which isn't a copy of it: the space's folder would stay behind
        let err = move_to(&env, &id, &second).await.unwrap_err();
        assert_eq!(err.message, IN_USE);
        assert!(!second.exists(), "nothing made");
        let saved: (String,) = sqlx::query_as("SELECT config FROM storage_locations WHERE id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap();
        assert!(saved.0.contains("first"));

        // A copy of the folder, marker included: the location and its spaces' folders follow
        copy_dir(&first, &copy);
        move_to(&env, &id, &copy).await.unwrap();
        let (source,): (String,) = sqlx::query_as("SELECT source_path FROM drives WHERE id = ?").bind(&drive).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(std::path::PathBuf::from(source), copy.join("teams").join("Sales"));
        std::fs::remove_dir_all(&first).unwrap();
        env.upload(&admin, &team, "after.txt", b"after").await;
        assert!(copy.join("teams/Sales/after.txt").is_file());
    }

    fn copy_dir(from: &FsPath, to: &FsPath) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                copy_dir(&e.path(), &to.join(e.file_name()));
            } else {
                std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn two_locations_never_share_a_place() {
        let env = testutil::env().await;
        let s3 = |endpoint: &str, bucket: &str, prefix: &str| json!({ "endpoint": endpoint, "bucket": bucket, "prefix": prefix });
        let place = |kind: &str, cfg: Value| place_of(kind, &cfg).unwrap();
        // The same bucket and prefix, written differently
        assert_eq!(place("s3", s3("https://S3.example.com/", "files", "/drive/")), place("s3", s3("https://s3.example.com/files", "", "drive")));
        assert_ne!(place("s3", s3("https://s3.example.com", "files", "drive")), place("s3", s3("https://s3.example.com", "files", "other")));
        assert_ne!(place("s3", s3("https://s3.example.com", "files", "")), place("s3", s3("https://s3.example.com", "photos", "")));
        // The same server and folder; a relative folder is in the account's home folder
        let host = |host: &str, user: &str, path: &str| json!({ "host": host, "username": user, "path": path });
        assert_eq!(place("sftp", host("NAS.example.com", "a", "/data/")), place("sftp", host("sftp://b@nas.example.com:22/data", "", "")));
        assert_ne!(place("sftp", host("nas.example.com", "a", "data")), place("sftp", host("nas.example.com", "b", "data")));
        assert_ne!(place("sftp", host("nas.example.com", "a", "/data")), place("ftp", host("nas.example.com", "a", "/data")));
        assert!(place_of("local", &json!({ "path": "/mnt/nas" })).is_none());

        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, is_default, created_at) VALUES ('b1', 'Bucket', 's3', ?, 0, 0)")
            .bind(s3("https://s3.example.com", "files", "drive").to_string())
            .execute(&env.st.db)
            .await
            .unwrap();
        let req = json!({ "name": "Again", "kind": "s3", "config": s3("https://s3.example.com/files", "", "/drive") });
        let err = create(State(env.st.clone()), Admin(env.admin().await), Json(serde_json::from_value(req).unwrap())).await.unwrap_err();
        assert_eq!((err.status, err.message.as_str()), (axum::http::StatusCode::CONFLICT, "The storage location \"Bucket\" already uses this place (the same bucket and prefix, or the same server and folder)"));
        // Editing a location to point there is refused too; itself is no duplicate
        let mut c = env.st.db.acquire().await.unwrap();
        assert!(check_place_free(&mut c, Some("b2"), "s3", &s3("https://s3.example.com", "files", "drive")).await.is_err());
        check_place_free(&mut c, Some("b1"), "s3", &s3("https://s3.example.com", "files", "drive")).await.unwrap();
    }

    #[tokio::test]
    async fn a_remote_place_holds_a_marker_naming_its_location_and_installation() {
        let env = testutil::env().await;
        // Ids of their own: health checks remember the locations they marked
        let (id, other) = (format!("b{}", new_id()), format!("o{}", new_id()));
        // Works like a bucket; a location added before markers were written has none with this installation's id
        add_location(&env, &id, None).await;
        let s = env.st.storage(&id).unwrap();
        let file = env.dir.join(&id).join(storage::LOCATION_MARKER);
        // Its first successful health check writes it
        probe(&env.st, &id).await.unwrap();
        let install = install_id(&env.st).await.unwrap();
        assert_eq!(install_id(&env.st).await.unwrap(), install, "made once");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), format!("{id}\n{install}\n"));
        assert_eq!(marker_state(&env.st, Some(&id), s.as_ref()).await.unwrap(), Marker::Ours);
        // Another location of this installation, or another installation: taken
        add_location(&env, &other, None).await;
        assert_eq!(marker_state(&env.st, Some(&other), s.as_ref()).await.unwrap(), Marker::Taken);
        std::fs::write(&file, format!("{id}\nsomeone-else\n")).unwrap();
        assert_eq!(marker_state(&env.st, Some(&id), s.as_ref()).await.unwrap(), Marker::Taken);
        // A location of this installation that is gone: free
        std::fs::write(&file, format!("{id}\n{install}\n")).unwrap();
        assert_eq!(marker_state(&env.st, Some("new"), s.as_ref()).await.unwrap(), Marker::Taken);
        sqlx::query("DELETE FROM storage_locations WHERE id = ?").bind(&id).execute(&env.st.db).await.unwrap();
        assert_eq!(marker_state(&env.st, Some("new"), s.as_ref()).await.unwrap(), Marker::Missing);
    }

    #[tokio::test]
    async fn saved_passwords_are_only_reused_for_the_same_server_and_account() {
        let env = testutil::env().await;
        let saved = json!({ "host": "files.example.com", "port": 22, "username": "backup", "password": testutil::password(), "host_key": "k" });
        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, is_default, created_at) VALUES ('nas', 'NAS', 'sftp', ?, 0, 0)")
            .bind(sealed_config("nas", &saved))
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
        // Trusting another host key (or any, to record it again), or no longer checking the certificate: the same
        let cleared = json!({ "host": "files.example.com", "port": 22, "username": "backup", "password": "", "host_key": "" });
        assert!(merged_config(&env.st, Some("nas"), "sftp", cleared).await.is_err());
        let other_key = json!({ "host": "files.example.com", "port": 22, "username": "backup", "password": "", "host_key": "k2" });
        assert!(merged_config(&env.st, Some("nas"), "sftp", other_key).await.is_err());
        let ftps = json!({ "host": "files.example.com", "port": 21, "username": "backup", "password": testutil::password(), "tls": true });
        sqlx::query("UPDATE storage_locations SET kind = 'ftp', config = ? WHERE id = 'nas'").bind(sealed_config("nas", &ftps)).execute(&env.st.db).await.unwrap();
        let unchecked = json!({ "host": "files.example.com", "port": 21, "username": "backup", "password": "", "tls": true, "tls_insecure": true });
        assert!(merged_config(&env.st, Some("nas"), "ftp", unchecked).await.is_err());
        let checked = json!({ "host": "files.example.com", "port": 21, "username": "backup", "password": "", "tls": true, "tls_insecure": false });
        assert_eq!(merged_config(&env.st, Some("nas"), "ftp", checked).await.unwrap()["password"], testutil::password());
        // A password entered anew is used as it is
        let fresh = json!({ "host": "elsewhere.example.com", "port": 22, "username": "backup", "password": testutil::wrong_password() });
        assert!(merged_config(&env.st, Some("nas"), "sftp", fresh).await.is_ok());
    }
}
