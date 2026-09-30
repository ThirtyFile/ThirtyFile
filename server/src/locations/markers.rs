//! Markers of S3, SFTP and FTP locations.
//!
//! Like a Local folder location's folder (storage/markers.rs, `LOCATION_MARKER`), the place of an S3, SFTP or FTP location holds
//! a `.thirtyfile-location` file: the location's id on the first line, this installation's id on the second. It is
//! written when the location is added (or its first health check or cleanup finds it missing, for a location added
//! before markers were written), and "Remove unused content" refuses to run in a place whose marker names another
//! location or another installation: that content isn't unused, it is someone else's.

use super::*;

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
pub(super) async fn read_marker(s: &dyn Storage) -> std::io::Result<Option<(String, String)>> {
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
pub(super) async fn write_marker(st: &AppState, id: &str, s: &dyn Storage) -> AppResult<()> {
    let body = format!("{id}\n{}\n", install_id(st).await?);
    let tmp = st.data_dir.join("tmp").join(format!("marker-{}", new_id()));
    tokio::fs::write(&tmp, body).await?;
    let put = s.put_at(storage::LOCATION_MARKER, &tmp).await;
    let _ = tokio::fs::remove_file(&tmp).await;
    put.map_err(|e| AppError::bad_request(describe(&e)))
}

/// Remote locations whose marker was found or written since the server started (health checks read it once)
pub(super) static MARKED: std::sync::Mutex<std::collections::BTreeSet<String>> = std::sync::Mutex::new(std::collections::BTreeSet::new());

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
pub(super) async fn release_place(st: &AppState, id: &str, s: &dyn Storage) {
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
pub(super) async fn mark_after_check(st: &AppState, id: &str, s: &dyn Storage) {
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
pub(super) fn place_of(kind: &str, cfg: &Value) -> Option<String> {
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
pub(super) async fn check_place_free(conn: &mut SqliteConnection, id: Option<&str>, kind: &str, cfg: &Value) -> AppResult<()> {
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
pub(super) async fn physical(st: &AppState, id: &str) -> AppResult<(String, Vec<String>)> {
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
