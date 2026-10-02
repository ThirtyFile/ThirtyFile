//! Checking the connection to each storage location

use super::*;

/// Sends just one HEAD request every 30 seconds (about US$0.03 per month on AWS); a disconnect shows as offline in the UI within half a minute
pub(super) const HEALTH_INTERVAL: Duration = Duration::from_secs(30);
/// Shorter check interval while a location is offline, so it's usable again soon after recovering
pub(super) const OFFLINE_INTERVAL: Duration = Duration::from_secs(20);

pub(super) const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

pub(super) fn set_health(st: &AppState, id: &str, error: Option<String>) {
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
pub(super) fn retry_in_background(st: &AppState, id: &str) {
    if !st.location_retries.lock().unwrap().insert(id.to_string()) {
        return;
    }
    // Released when the task ends, also if it panics
    struct Running(AppState, String);
    impl Drop for Running {
        fn drop(&mut self) {
            self.0.location_retries.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.1);
        }
    }
    let (st, running) = (st.clone(), Running(st.clone(), id.to_string()));
    tokio::spawn(async move {
        let id = &running.1;
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
                    _ = st.recheck.notified() => tokio::time::sleep(Duration::from_secs(1)).await,
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

/// What connecting does with a Local folder location's folder (storage/markers.rs, `LOCATION_MARKER`)
#[derive(Clone, Copy)]
pub(super) enum Folder<'a> {
    /// Used as it is: it must hold the marker of the location with this id
    Existing(&'a str),
    /// An administrator adds the location with this id, or changes its folder: the folder is created when it isn't
    /// there, and gets the location's marker
    Claim(&'a str),
    /// Settings tried before they are saved (the location's id when it is being edited): nothing is created
    Try(Option<&'a str>),
}

/// Tidies up the settings, builds the backend and runs a connection test (write, read back and delete a small file); returns the tidied settings to save
pub(super) async fn connect(st: &AppState, kind: &str, config: Value, folder: Folder<'_>) -> AppResult<(Arc<dyn Storage>, Value)> {
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
pub(super) async fn try_folder(root: &FsPath, id: &str) -> std::io::Result<()> {
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
pub(super) fn folder_changed(kind: &str, saved: &Value, config: &Value) -> bool {
    let path = |c: &Value| c["path"].as_str().map(str::trim).unwrap_or_default().to_string();
    kind == "local" && path(saved) != path(config)
}

#[derive(Deserialize)]
pub struct TestReq {
    /// Sent when editing an existing location, to keep its saved secrets
    pub(super) id: Option<String>,
    pub(super) kind: String,
    pub(super) config: Value,
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

pub async fn update(State(st): State<AppState>, Admin(user): Admin, Path(id): Path<String>, Json(req): Json<LocationReq>) -> AppResult<Json<Value>> {
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
            crate::folders::spaces_changed(&st);
        }
        // An S3, SFTP or FTP location moved to another place: the old one is free again
        if place_moved && let Some(old) = old {
            release_place(&st, &id, old.as_ref()).await;
            st.location_marked.lock().unwrap().insert(id.clone());
        }
    }
    Ok(Json(json!({ "ok": true })))
}

/// Shown when a Local folder location's folder is changed while spaces or content are on it, to a folder that isn't
/// a copy of it
pub(super) const IN_USE: &str = "Spaces or files are on this location, so its folder can only change to a copy of it. Copy everything in the folder to the new one first, including its .thirtyfile-location file, then change the folder.";

/// Whether spaces or content are on the location `id`, or being moved to or from it
pub(super) async fn in_use(conn: &mut SqliteConnection, id: &str) -> AppResult<bool> {
    let (n,): (i64,) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM drives WHERE location_id = ?1) + (SELECT COUNT(*) FROM blobs WHERE location_id = ?1)")
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    Ok(n > 0
        || crate::moves::location_busy(conn, id).await?
        || crate::backups::sets_on(conn, id).await? > 0
        || crate::replicas::location_used(conn, id).await? != (0, false))
}

/// The folder spaces of the location `id` follow its folder from `from` to `to`
pub(super) async fn follow_folder(conn: &mut SqliteConnection, id: &str, from: &FsPath, to: &FsPath) -> AppResult<()> {
    if crate::moves::location_busy(&mut *conn, id).await? {
        return Err(AppError::conflict("A space is being moved to or from this location. Wait until the move finishes, or cancel it."));
    }
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT id, source_path FROM drives WHERE location_id = ? AND source_path IS NOT NULL").bind(id).fetch_all(&mut *conn).await?;
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
