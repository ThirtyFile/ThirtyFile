//! Managing storage locations (Control panel › Storage locations)

use super::*;

#[derive(Serialize)]
pub struct LocationInfo {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) kind: String,
    pub(super) builtin: bool,
    pub(super) is_default: bool,
    /// Settings (without secrets)
    pub(super) config: Value,
    pub(super) has_secret: bool,
    /// Settings are loaded and the most recent connection check succeeded
    pub(super) connected: bool,
    /// Reason the most recent connection check failed
    pub(super) health_error: Option<String>,
    pub(super) checked_at: Option<i64>,
    /// Number of physical files whose deletion failed and is awaiting retry
    pub(super) pending_deletes: i64,
    /// Space used on the location: the content store there, plus the indexed size of the folder spaces on it
    pub(super) used_bytes: i64,
    /// Of `used_bytes`, the folder spaces' part
    pub(super) folder_bytes: i64,
    /// Files in the content store there
    pub(super) blob_count: i64,
    /// Spaces on the location (`drives.location_id`), of every kind and mode
    pub(super) drive_count: i64,
    /// Locations on this server's disks (built-in and Local folder): free and total bytes of the disk holding the
    /// folder, when the system tells
    pub(super) disk_free_bytes: Option<u64>,
    pub(super) disk_total_bytes: Option<u64>,
}

/// Fields that aren't returned to the browser (leaving them blank when editing keeps the existing value), and are
/// stored encrypted
pub const SECRET_FIELDS: [&str; 4] = ["secret_access_key", "password", "private_key", "key_passphrase"];

/// Settings with secrets removed
pub(super) fn public_config(cfg: &Value) -> (Value, bool) {
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
pub(super) async fn disk_of(path: std::path::PathBuf) -> Option<(u64, u64)> {
    util::disk_space_soon(&path).await
}

pub async fn list(State(st): State<AppState>, _: Admin) -> AppResult<Json<Vec<LocationInfo>>> {
    let rows: Vec<LocationRow> =
        sqlx::query_as("SELECT id, name, kind, config, is_default FROM storage_locations ORDER BY (id = 'local') DESC, created_at").fetch_all(&st.db).await?;
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
    pub(super) id: String,
    pub(super) name: String,
    pub(super) kind: String,
    /// "store" or "folder"
    pub(super) mode: String,
    /// The owner's user name (personal spaces)
    pub(super) owner_name: String,
    pub(super) used_bytes: i64,
    /// The server folder of a folder space ("Move everything to…" names it)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) source_path: Option<String>,
}

/// The spaces on a storage location (Control panel › Storage locations)
pub async fn spaces(State(st): State<AppState>, Admin(me): Admin, Path(id): Path<String>) -> AppResult<Json<Vec<LocationSpace>>> {
    let exists: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM storage_locations WHERE id = ?").bind(&id).fetch_optional(&st.db).await?;
    if exists.is_none() {
        return Err(AppError::not_found("Storage location not found"));
    }
    let list: Vec<LocationSpace> = sqlx::query_as(
        "SELECT d.id, d.name, d.kind, d.mode, CASE WHEN d.kind = 'personal' THEN COALESCE(u.username, '') ELSE '' END AS owner_name,
                d.used_bytes,
                -- Not where someone else's personal space is kept (as in Control panel › Spaces)
                CASE WHEN d.mode = 'folder' AND NOT (d.kind = 'personal' AND d.owner_id IS NOT ?2) THEN d.source_path END AS source_path
         FROM drives d LEFT JOIN users u ON u.id = d.owner_id
         WHERE d.location_id = ?1
         ORDER BY CASE d.kind WHEN 'company' THEN 0 WHEN 'team' THEN 1 ELSE 2 END, d.name, owner_name",
    )
    .bind(&id)
    .bind(me.id)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(list))
}

#[derive(Deserialize)]
pub struct LocationReq {
    pub(super) name: Option<String>,
    pub(super) kind: Option<String>,
    pub(super) config: Option<Value>,
}

/// Settings that decide where saved passwords and keys are sent
pub(super) const TARGET_FIELDS: [&str; 8] = ["host", "port", "endpoint", "bucket", "region", "username", "access_key_id", "tls"];

/// Keeps the existing secrets when none are entered (no need to re-enter them when editing), but only for the same
/// kind of storage and the same server, bucket and account: otherwise they would be sent to wherever the new settings
/// point, and anyone able to edit the settings could collect them
pub(super) async fn merged_config(st: &AppState, id: Option<&str>, kind: &str, config: Value) -> AppResult<Value> {
    let mut config = config;
    let Some(id) = id else { return Ok(config) };
    let old: Option<(String, String)> = sqlx::query_as("SELECT kind, config FROM storage_locations WHERE id = ?").bind(id).fetch_optional(&st.db).await?;
    let Some((old_kind, old)) = old.map(|(k, c)| (k, config_json(id, &c))) else { return Ok(config) };
    let wanted = SECRET_FIELDS
        .iter()
        .any(|f| config.get(*f).and_then(Value::as_str).is_none_or(str::is_empty) && old.get(*f).and_then(Value::as_str).is_some_and(|s| !s.is_empty()));
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
pub(super) fn friendly(detail: &str, err: &str) -> String {
    let e = detail.to_ascii_lowercase();
    let msg = if e.contains("signaturedoesnotmatch") || e.contains("invalidaccesskeyid") || e.contains("403 forbidden") {
        "Incorrect Access Key or Secret Key, or no permission for this bucket"
    } else if e.contains("permanentredirect")
        || e.contains("redirect")
        || e.contains("authorizationheadermalformed")
        || e.contains("301 moved")
        || e.contains("invalidregionname")
    {
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
