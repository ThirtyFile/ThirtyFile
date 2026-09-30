//! Starting the server: the data folder, the database, the settings and the background tasks, then serving until
//! a stop signal.

use std::path::PathBuf;

use crate::{
    backups, branding,
    cli::{self, Config},
    db, folders, locations, logs, moves, personal, replicas, secrets, sso,
    state::{AppState, Setup},
    thumbnails, tree, upload, usage, util,
};
#[cfg(target_os = "linux")]
use crate::watch;

use super::{maintenance::spawn_maintenance, routes::router, serve::serve};

/// Holds `thirtyfile.lock` in the data folder while the server runs, so two never run on the same data at the same
/// time. Where the file system can't lock files, nothing is held.
fn lock_data(data: &std::path::Path) -> Result<Option<std::fs::File>, Box<dyn std::error::Error>> {
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(data.join("thirtyfile.lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => {
            Err("ThirtyFile is already running with this data folder. Stop it first.".into())
        }
        Err(std::fs::TryLockError::Error(e)) => {
            tracing::warn!("Couldn't lock the data folder ({e}); make sure only one ThirtyFile uses it");
            Ok(None)
        }
    }
}

pub async fn run(cfg: Config, storage: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    for dir in ["tmp", "thumbs"] {
        std::fs::create_dir_all(cfg.data.join(dir))?;
    }
    let key_source = secrets::KeySource::from_settings(cfg.secret_key.clone(), cfg.secret_key_file.clone(), &cfg.data);
    let key = key_source.load()?;
    secrets::init(&key);
    let db = db::connect(&cfg.data.join("drive.db"), cfg.db_cache_mb).await?;

    if cli::run(&cfg, &db, &storage, &key_source).await? {
        return Ok(());
    }

    let _lock = lock_data(&cfg.data)?;
    // The storage folder: made with its marker on a new install. When the database records files or spaces there, a
    // missing or empty folder (a volume that isn't mounted) or one without the marker stops the start.
    let (recorded,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM blobs WHERE location_id = 'local') OR EXISTS (SELECT 1 FROM drives WHERE location_id = 'local')",
    )
    .fetch_one(&db)
    .await?;
    crate::storage::prepare_builtin(&storage, recorded)?;
    let admin_password = match (&cfg.admin_password, &cfg.admin_password_file) {
        (Some(p), _) => Some(p.clone()),
        (None, Some(file)) => Some(
            std::fs::read_to_string(file)
                .map_err(|e| format!("Can't read THIRTYFILE_ADMIN_PASSWORD_FILE {}: {e}", file.display()))?
                .trim_end_matches(['\r', '\n'])
                .to_string(),
        ),
        (None, None) => None,
    };
    // New spaces of the built-in location get their folders in the storage folder
    let space_folders = std::path::absolute(&storage)?;
    db::bootstrap_admin(&db, admin_password.as_deref(), Some(&space_folders)).await.map_err(|e| e.message)?;
    db::create_company_space(&db, Some(&space_folders)).await.map_err(|e| e.message)?;
    let secret = db::load_secret(&db).await?;
    let system = db::load_system_settings(&db).await?;
    // Settings that are probably wrong together: said once at startup
    if !cfg.secure_cookie && cfg.trust_proxy.enabled() {
        tracing::warn!("THIRTYFILE_TRUST_PROXY is on but THIRTYFILE_SECURE_COOKIE is off: if the proxy serves HTTPS, set THIRTYFILE_SECURE_COOKIE=true");
    }
    let storages = locations::load_all(&db, &storage).await?;
    let log_settings = logs::load_settings(&db).await;
    let branding = branding::load(&db).await;
    let sso_settings = sso::load(&db).await;
    let (log_tx, log_rx) = logs::channel();
    // Thumbnails read the image into memory and decode it: fewer at a time, and a lower decoding limit, on small servers
    let memory = util::memory_limit();
    let small = memory.is_some_and(|m| m < 2 * 1024 * 1024 * 1024);
    let thumb_jobs = cfg.thumbnail_jobs.unwrap_or(if small { 1 } else { 2 });
    let thumb_decode_bytes = memory.map_or(thumbnails::MAX_THUMB_DECODE_BYTES, |m| (m / 8).clamp(64 * 1024 * 1024, thumbnails::MAX_THUMB_DECODE_BYTES));
    tracing::info!(
        "Memory: {}, {thumb_jobs} thumbnail(s) at a time, {} MB database cache per connection",
        memory.map_or("unknown".to_string(), util::format_bytes_u64),
        cfg.db_cache_mb
    );
    let state = AppState::new(Setup {
        db,
        storages,
        data_dir: cfg.data.clone(),
        storage_dir: storage,
        space_folders: Some(space_folders),
        secret,
        secure_cookie: cfg.secure_cookie,
        trash_days: cfg.trash_days,
        trust_proxy: cfg.trust_proxy,
        max_upload: cfg.max_upload_mb.checked_mul(1024 * 1024).ok_or("THIRTYFILE_MAX_UPLOAD_MB is too large")?,
        thumb_jobs: thumb_jobs as usize,
        thumb_decode_bytes,
        system,
        logs: log_settings,
        branding,
        sso: sso_settings,
        log_tx,
    });

    let log_writer = logs::spawn_writer(state.clone(), log_rx);
    if let Err(e) = tree::recompute_usage(&state).await {
        tracing::warn!("Couldn't recompute space usage: {}", e.message);
    }
    match upload::clean_tmp(&state).await {
        Ok(n) if n > 0 => tracing::info!("Removed {n} leftover temporary files"),
        Err(e) => tracing::warn!("Couldn't clean the temporary directory: {}", e.message),
        _ => {}
    }
    // Changes of many items that a stop left unfinished: before anything scans the folder spaces they change
    if let Err(e) = tree::changes::resume(&state).await {
        tracing::warn!("Couldn't finish the changes left unfinished: {}", e.message);
    }
    spawn_maintenance(state.clone(), cfg.trash_days);
    locations::spawn_health_monitor(state.clone());
    usage::sample::spawn(state.clone());
    moves::spawn_runner(state.clone());
    backups::spawn_runner(state.clone());
    replicas::spawn_runner(state.clone());
    personal::spawn_retry(state.clone());
    folders::spawn_scanner(state.clone());
    // Folder spaces on local disks report changes as they happen
    #[cfg(target_os = "linux")]
    watch::spawn_watchers(state.clone());

    // JSON requests that take longer than this are cut off (a stuck storage service, a slow provider). Requests that
    // carry a body to store, and thumbnails (which queue), are outside the limit (`untimed`); downloads stream after
    // the handler returned, so the limit doesn't apply to them either
    let db_pool = state.db.clone();
    let app = router(state);

    let listener = tokio::net::TcpListener::bind(&cfg.addr).await?;
    tracing::info!("ThirtyFile started: http://{}", cfg.addr);
    serve(listener, app).await?;
    // Sign-in and share access events still queued are written before exiting
    log_writer.finish().await;
    // Let SQLite update its statistics and fold the write-ahead log into the database file
    let _ = sqlx::query("PRAGMA optimize").execute(&db_pool).await;
    db_pool.close().await;
    tracing::info!("Stopped");
    Ok(())
}
