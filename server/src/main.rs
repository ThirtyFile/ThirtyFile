mod admin;
mod archive;
mod auth;
mod beneath;
mod dav;
mod db;
mod downloads;
mod drives;
mod error;
mod files;
mod folders;
mod fsops;
mod location_tools;
mod locations;
mod logs;
mod mail;
mod notify;
mod branding;
mod check;
mod ftp;
mod sftp;
mod sso;
mod nodes;
mod paths;
mod personal;
#[cfg(unix)]
mod privileges;
mod reset;
mod secrets;
mod sessions;
mod shares;
mod space_folders;
mod state;
mod storage;
#[cfg(test)]
mod testutil;
mod thumbnails;
mod tokens;
mod tree;
mod twofactor;
mod upload;
mod util;
#[cfg(target_os = "linux")]
mod watch;
mod versions;
mod web;
mod zip;

use std::{path::PathBuf, sync::Arc, time::Duration};

use axum::{
    Router,
    extract::{DefaultBodyLimit, Request},
    http::{Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, delete, get, head, patch, post, put},
};
use clap::{Parser, Subcommand};
use tower::ServiceExt;
use tower_http::{CompressionLevel, compression::{CompressionLayer, predicate::Predicate}, timeout::TimeoutLayer, trace::TraceLayer};
use tracing_subscriber::EnvFilter;

use crate::{
    state::{AppState, Inner},
};

/// The version: the release number the image was built for (THIRTYFILE_VERSION at build time), else Cargo.toml's
pub const VERSION: &str = match option_env!("THIRTYFILE_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

#[derive(Parser)]
#[command(name = "thirtyfile", version = VERSION, about = "ThirtyFile — lightweight cloud file management system")]
struct Config {
    /// Listen address
    #[arg(long, env = "THIRTYFILE_ADDR", default_value = "0.0.0.0:8080")]
    addr: String,
    /// Data directory (database, files, thumbnails)
    #[arg(long, env = "THIRTYFILE_DATA", default_value = "./data")]
    data: PathBuf,
    /// Folder for the file contents of the built-in storage location (default: blobs in the data directory)
    #[arg(long, env = "THIRTYFILE_STORAGE")]
    storage: Option<PathBuf>,
    /// Administrator password on first startup; if not set, a random one is generated and printed to the log
    #[arg(long, env = "THIRTYFILE_ADMIN_PASSWORD", hide_env_values = true)]
    admin_password: Option<String>,
    /// A file holding the administrator password for the first startup (for Docker secrets)
    #[arg(long, env = "THIRTYFILE_ADMIN_PASSWORD_FILE")]
    admin_password_file: Option<PathBuf>,
    /// Key that encrypts the passwords and keys saved in the database (64 hex characters); default: secret.key in the
    /// data folder, created on first start
    #[arg(long, env = "THIRTYFILE_SECRET_KEY", hide_env_values = true)]
    secret_key: Option<String>,
    /// A file holding that key instead (for example a Docker secret)
    #[arg(long, env = "THIRTYFILE_SECRET_KEY_FILE")]
    secret_key_file: Option<PathBuf>,
    /// Enable when serving over HTTPS; cookies get the Secure attribute (true/false, also 1/0, yes/no, on/off)
    #[arg(long, env = "THIRTYFILE_SECURE_COOKIE", default_value = "false", value_parser = clap::builder::BoolishValueParser::new(), action = clap::ArgAction::Set)]
    secure_cookie: bool,
    /// Enable when behind a reverse proxy (nginx, Caddy…): take the user's real IP from X-Forwarded-For (for sign-in rate
    /// limiting). `true` trusts proxies on private and loopback addresses; or list the proxies' addresses or networks
    #[arg(long, env = "THIRTYFILE_TRUST_PROXY", default_value = "false", value_parser = auth::TrustProxy::parse)]
    trust_proxy: auth::TrustProxy,
    /// Days to keep items in the trash, 0 = until emptied
    #[arg(long, env = "THIRTYFILE_TRASH_DAYS", default_value_t = 30, value_parser = clap::value_parser!(i64).range(0..=36500))]
    trash_days: i64,
    /// Upload size limit per file (MB), 0 = unlimited
    #[arg(long, env = "THIRTYFILE_MAX_UPLOAD_MB", default_value_t = 0)]
    max_upload_mb: u64,
    /// SQLite page cache per database connection (MB); up to 8 connections are open
    #[arg(long, env = "THIRTYFILE_DB_CACHE_MB", default_value_t = 16, value_parser = clap::value_parser!(u32).range(1..=1024))]
    db_cache_mb: u32,
    /// Thumbnails made at the same time (default: 1 with less than 2 GB of memory, else 2)
    #[arg(long, env = "THIRTYFILE_THUMBNAIL_JOBS", value_parser = clap::value_parser!(u32).range(1..=16))]
    thumbnail_jobs: Option<u32>,
    /// When started as root: give the data directory to this user (`uid` or `uid:gid`) and run as that user (set in the Docker image)
    #[arg(long, env = "THIRTYFILE_RUN_AS")]
    #[cfg_attr(not(unix), allow(dead_code))]
    run_as: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Reset a user's password (for when the administrator password is forgotten). Without the password, it is read
    /// from the input, so it doesn't end up in the shell history or the process list
    ResetPassword { username: String, password: Option<String> },
    /// Turn a user's two-factor sign-in off (for when the phone and the recovery codes are lost)
    ResetTwoFactor { username: String },
    /// Check whether the running service is healthy (for Docker HEALTHCHECK): exit code 0 when healthy
    Health,
    /// Write a consistent copy of the database to a new file, also while ThirtyFile is running
    /// (e.g. `docker exec thirtyfile thirtyfile backup /data/backups/drive.db`)
    Backup { file: PathBuf },
    /// Encrypt the saved passwords and keys with a new key. Stop ThirtyFile first. With a key file, the file is
    /// replaced; with THIRTYFILE_SECRET_KEY, give the new key in THIRTYFILE_NEW_SECRET_KEY and set it afterwards
    RotateSecretKey,
    /// Compare the stored file contents with the database: missing, changed or unknown files, for every storage
    /// location. Unknown files are only listed, never deleted
    Check {
        /// Also read every file back and compare its hash (slow: reads everything)
        #[arg(long)]
        verify: bool,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::IsTerminal;
    // Colours only on a terminal (not in `docker logs` or a log collector); THIRTYFILE_LOG_FORMAT=json for JSON lines
    let logs = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "thirtyfile=info,tower_http=warn".into()))
        .with_ansi(std::io::stdout().is_terminal());
    if std::env::var("THIRTYFILE_LOG_FORMAT").is_ok_and(|f| f.eq_ignore_ascii_case("json")) {
        logs.json().init();
    } else {
        logs.init();
    }
    let runtime = || tokio::runtime::Builder::new_multi_thread().enable_all().build();
    let cfg = match Config::try_parse() {
        Ok(cfg) => cfg,
        // The health check only needs the address: a mistyped setting (which stops the server with a clear message)
        // mustn't also make Docker report the check itself as broken
        Err(e) if std::env::args().nth(1).as_deref() == Some("health") && e.kind() != clap::error::ErrorKind::DisplayHelp => {
            let addr = std::env::var("THIRTYFILE_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
            std::process::exit(if runtime()?.block_on(health_probe(&addr)) { 0 } else { 1 });
        }
        Err(e) => e.exit(),
    };

    // The health check only connects to the running service and doesn't touch the data directory
    if let Some(Command::Health) = &cfg.command {
        std::process::exit(if runtime()?.block_on(health_probe(&cfg.addr)) { 0 } else { 1 });
    }

    // The built-in storage location's folder
    let storage = cfg.storage.clone().unwrap_or_else(|| cfg.data.join("blobs"));

    // Before the runtime starts its threads, so that all of them run as the new user
    #[cfg(unix)]
    if let Some(user) = &cfg.run_as {
        privileges::drop_to(user, &[cfg.data.as_path(), storage.as_path()])?;
    }

    runtime()?.block_on(run(cfg, storage))
}

/// `thirtyfile rotate-secret-key`: re-encrypts the saved secrets with a new key. The new key file is written first
/// (as `<file>.new`) and moved into place once the database is updated, so a stop halfway leaves both keys on disk.
async fn rotate_secret_key(db: &sqlx::SqlitePool, source: &secrets::KeySource) -> Result<(), Box<dyn std::error::Error>> {
    let given = std::env::var("THIRTYFILE_NEW_SECRET_KEY").ok().filter(|k| !k.trim().is_empty());
    let new_key = match (&given, source) {
        (Some(k), _) => secrets::KeySource::Env(k.clone()).load()?,
        (None, secrets::KeySource::Env(_)) => {
            return Err("The key is set with THIRTYFILE_SECRET_KEY: put the new key (64 hex characters, e.g. from `openssl rand -hex 32`) in THIRTYFILE_NEW_SECRET_KEY, run this again, then set THIRTYFILE_SECRET_KEY to it".into());
        }
        (None, secrets::KeySource::File(_)) => rand::random(),
    };
    let staged = match source {
        secrets::KeySource::File(path) => {
            let staged = path.with_extension("key.new");
            secrets::write_key_file(&staged, &new_key)?;
            Some((staged, path.clone()))
        }
        secrets::KeySource::Env(_) => None,
    };
    db::reseal_secrets(db, &new_key).await?;
    if let Some((staged, path)) = staged {
        std::fs::rename(&staged, &path)?;
        println!("The saved passwords and keys are encrypted with a new key, saved in {}. Back it up again.", path.display());
    } else {
        println!("The saved passwords and keys are encrypted with the new key. Now set THIRTYFILE_SECRET_KEY to it before starting ThirtyFile.");
    }
    Ok(())
}

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

async fn run(cfg: Config, storage: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    for dir in ["tmp", "thumbs"] {
        std::fs::create_dir_all(cfg.data.join(dir))?;
    }
    std::fs::create_dir_all(&storage)?;
    let key_source = secrets::KeySource::from_settings(cfg.secret_key.clone(), cfg.secret_key_file.clone(), &cfg.data);
    let key = key_source.load()?;
    secrets::init(&key);
    let db = db::connect(&cfg.data.join("drive.db"), cfg.db_cache_mb).await?;

    if let Some(Command::Check { verify }) = &cfg.command {
        let storages = locations::load_all(&db, &storage).await?;
        let reports = check::run(&db, &storages, *verify, |id, n| eprintln!("Checking storage location {id} ({n} file(s))…")).await?;
        let problems = check::print(&reports);
        std::process::exit(if problems == 0 { 0 } else { 1 });
    }

    if let Some(Command::RotateSecretKey) = &cfg.command {
        return rotate_secret_key(&db, &key_source).await;
    }

    if let Some(Command::Backup { file }) = &cfg.command {
        if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        db::backup_to(&db, file).await?;
        println!("Database saved to {}", file.display());
        return Ok(());
    }

    if let Some(Command::ResetTwoFactor { username }) = &cfg.command {
        let res = sqlx::query("UPDATE users SET totp_secret = NULL, totp_last_step = 0 WHERE username = ?").bind(username).execute(&db).await?;
        if res.rows_affected() == 0 {
            return Err(format!("User not found: {username}").into());
        }
        sqlx::query("DELETE FROM recovery_codes WHERE user_id = (SELECT id FROM users WHERE username = ?)").bind(username).execute(&db).await?;
        println!("Two-factor sign-in is turned off for this account");
        return Ok(());
    }
    if let Some(Command::ResetPassword { username, password }) = &cfg.command {
        let password = match password {
            Some(p) => p.clone(),
            None => {
                eprintln!("Type the new password and press Enter:");
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                line.trim_end_matches(['\r', '\n']).to_string()
            }
        };
        let password = &password;
        auth::validate_password(password, auth::MIN_PASSWORD).map_err(|e| e.message)?;
        let hash = auth::hash_password(password.clone()).await.map_err(|e| e.message)?;
        let res = sqlx::query("UPDATE users SET password_hash = ?, disabled = 0 WHERE username = ?")
            .bind(hash)
            .bind(username)
            .execute(&db)
            .await?;
        if res.rows_affected() == 0 {
            return Err(format!("User not found: {username}").into());
        }
        sqlx::query("UPDATE users SET must_change_password = 1 WHERE username = ?").bind(username).execute(&db).await?;
        for table in ["sessions", "app_passwords"] {
            sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table} WHERE user_id = (SELECT id FROM users WHERE username = ?)")))
                .bind(username)
                .execute(&db)
                .await?;
        }
        println!("Password reset");
        return Ok(());
    }

    let _lock = lock_data(&cfg.data)?;
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
    let state = AppState(Arc::new(Inner {
        db,
        storages: std::sync::RwLock::new(storages),
        migrations: Default::default(),
        data_dir: cfg.data.clone(),
        storage_dir: storage,
        space_folders: Some(space_folders),
        secret,
        secure_cookie: cfg.secure_cookie,
        trash_days: cfg.trash_days,
        trust_proxy: cfg.trust_proxy,
        max_upload: cfg.max_upload_mb.checked_mul(1024 * 1024).ok_or("THIRTYFILE_MAX_UPLOAD_MB is too large")?,
        write_lock: tokio::sync::Mutex::new(()),
        active_uploads: Default::default(),
        login_failures: Default::default(),
        detached_purge: Default::default(),
        thumb_permits: tokio::sync::Semaphore::new(thumb_jobs as usize),
        thumb_decode_bytes,
        system: std::sync::RwLock::new(system),
        blob_guard: Default::default(),
        logs: std::sync::RwLock::new(log_settings),
        branding: std::sync::RwLock::new(branding),
        location_health: Default::default(),
        sso: std::sync::RwLock::new(sso_settings),
        sso_pending: Default::default(),
        twofactor_setups: Default::default(),
        archive_lock: Default::default(),
        share_views: Default::default(),
        download_links: Default::default(),
        jobs: Default::default(),
        log_tx,
    }));

    let log_writer = logs::spawn_writer(state.clone(), log_rx);
    if let Err(e) = tree::recompute_usage(&state).await {
        tracing::warn!("Couldn't recompute space usage: {}", e.message);
    }
    match upload::clean_tmp(&state).await {
        Ok(n) if n > 0 => tracing::info!("Removed {n} leftover temporary files"),
        Err(e) => tracing::warn!("Couldn't clean the temporary directory: {}", e.message),
        _ => {}
    }
    spawn_maintenance(state.clone(), cfg.trash_days);
    locations::spawn_health_monitor(state.clone());
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

/// All routes: the API (with or without the request timeout) and the web interface
fn router(state: AppState) -> Router {
    Router::new()
        .nest("/api", api().layer(TimeoutLayer::with_status_code(StatusCode::GATEWAY_TIMEOUT, Duration::from_secs(120))).merge(untimed()))
        // WebDAV (dav.rs): outside the request timeout too, as it receives and sends whole files
        .route(dav::PREFIX, any(dav::handle))
        .route("/dav/", any(dav::handle))
        .route("/dav/{*path}", any(dav::handle))
        .route("/", any(dav::server_root))
        .fallback(web::serve)
        .layer(middleware::from_fn_with_state(state.clone(), same_origin))
        .layer(middleware::from_fn_with_state(state.clone(), forwarding))
        // gzip / brotli for JSON, HTML, JS, CSS and SVG (see `Compressible`); file contents and other downloads are never compressed
        // Level 4: brotli's default (11) spends far more CPU per response than it saves on JSON and HTML; gzip 4 is likewise the sweet spot
        .layer(CompressionLayer::new().gzip(true).br(true).quality(CompressionLevel::Precise(4)).compress_when(Compressible))
        .layer(TraceLayer::new_for_http())
        // A bug hit by one request answers that request with an error instead of stopping the server for everyone
        .layer(tower_http::catch_panic::CatchPanicLayer::new())
        .with_state(state)
}

/// Ctrl+C or SIGTERM (sent by `docker stop` or when systemd stops the service): stop accepting new connections and wait for in-flight requests to finish
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = term => {}
    }
    tracing::info!("Received shutdown signal, shutting down…");
}

/// Routes outside the API's request timeout (see `main`): they receive content and write it to the storage
/// location, or wait in the thumbnail queue, and legitimately take as long as that takes
fn untimed() -> Router<AppState> {
    // File operations: app passwords work here too
    let files = Router::new()
        .route("/files/{id}/content", put(files::save_content).layer(DefaultBodyLimit::max(files::MAX_EDIT_BYTES)))
        .route(
            "/files/{id}/thumbnail",
            get(thumbnails::thumbnail).merge(put(thumbnails::upload_thumbnail).layer(DefaultBodyLimit::max(thumbnails::MAX_THUMB_UPLOAD))),
        )
        .route("/files/{id}/versions/{version}/restore", post(versions::restore))
        .route("/uploads", post(upload::create).options(upload::options))
        .route(
            "/uploads/{id}",
            head(upload::head).patch(upload::patch).delete(upload::delete).layer(DefaultBodyLimit::disable()),
        )
        .route_layer(middleware::from_fn(tokens::allow));
    Router::new()
        .merge(files)
        .route("/public/shares/{token}/nodes/{id}/thumbnail", get(shares::public_thumbnail))
        // Visitors of a share link that accepts files
        .route("/public/shares/{token}/uploads", post(shares::public_upload_create).options(upload::options))
        .route(
            "/public/shares/{token}/uploads/{id}",
            head(shares::public_upload_head)
                .patch(shares::public_upload_patch)
                .delete(shares::public_upload_delete)
                .layer(DefaultBodyLimit::disable()),
        )
        .route("/admin/branding/logo/{variant}", put(branding::upload_logo).delete(branding::delete_logo))
        // One file of a storage location, as it is stored
        .route("/admin/storage/{id}/download", get(location_tools::download))
        .route("/admin/branding/background", put(branding::upload_background).delete(branding::delete_background).layer(DefaultBodyLimit::max(branding::MAX_BACKGROUND + 1024)))
}

/// Routes that also accept app passwords (`Authorization: Bearer` or HTTP Basic, see tokens.rs): file operations only.
/// Everything else (the account itself, sign-in methods, sharing, logs and administration) needs a browser session.
fn file_api() -> Router<AppState> {
    Router::new()
        .route("/auth/me", get(auth::me))
        .route("/nodes/{id}", get(nodes::get).patch(nodes::rename))
        .route("/nodes/{id}/children", get(nodes::children))
        .route("/nodes/move", post(nodes::move_nodes))
        .route("/nodes/copy", post(nodes::copy_nodes))
        .route("/nodes/conflicts", post(nodes::conflicts))
        .route("/nodes/trash", post(nodes::trash))
        .route("/nodes/contents", post(nodes::contents))
        .route("/nodes/find", post(paths::find))
        .route("/folders", post(nodes::create_folder))
        .route("/trash", get(nodes::list_trash))
        .route("/trash/restore", post(nodes::restore))
        .route("/trash/delete", post(nodes::delete_forever))
        .route("/trash/empty", get(nodes::empty_trash_preview).post(nodes::empty_trash))
        .route("/search", get(nodes::search))
        .route("/recent", get(nodes::recent))
        .route("/favorites", get(nodes::favorites))
        .route("/nodes/favorite", post(nodes::set_favorite))
        .route("/shared-with-me", get(nodes::shared_with_me))
        .route("/files/{id}/content", get(files::content))
        .route("/files/{id}/versions", get(versions::list))
        .route("/files/{id}/versions/{version}/content", get(versions::content))
        .route("/download", get(downloads::download).post(downloads::create_download_link))
        .route("/download/{link}", get(downloads::download_by_link))
        .route("/archive/compress", post(archive::compress))
        .route("/archive/extract", post(archive::extract))
        .route("/jobs/{id}", get(archive::get))
        .route_layer(middleware::from_fn(tokens::allow))
}

/// Accept loop with the limits `axum::serve` doesn't set: a connection that sends nothing, or doesn't finish sending
/// its request headers, within 30 seconds is dropped (so idle or slow connections can't pile up), and idle keep-alive
/// connections close too.
/// Shutdown waits for in-flight requests like `axum::serve` does.
async fn serve(listener: tokio::net::TcpListener, app: Router) -> Result<(), Box<dyn std::error::Error>> {
    use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
    let mut builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
    builder.http1().timer(TokioTimer::new()).header_read_timeout(Duration::from_secs(30));
    builder.http2().timer(TokioTimer::new()).keep_alive_interval(Duration::from_secs(60)).keep_alive_timeout(Duration::from_secs(20));
    let graceful = hyper_util::server::graceful::GracefulShutdown::new();
    let mut shutdown = std::pin::pin!(shutdown_signal());
    loop {
        let (stream, addr) = tokio::select! {
            res = listener.accept() => match res {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("Accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            _ = &mut shutdown => break,
        };
        let app = app.clone();
        let service = hyper::service::service_fn(move |mut req: axum::http::Request<hyper::body::Incoming>| {
            req.extensions_mut().insert(axum::extract::ConnectInfo(addr));
            app.clone().oneshot(req)
        });
        let builder = builder.clone();
        let watcher = graceful.watcher();
        tokio::spawn(async move {
            // The protocol is detected from the first bytes, before hyper's own header timer starts: wait for them here
            let mut first = [0u8; 1];
            if !matches!(tokio::time::timeout(Duration::from_secs(30), stream.peek(&mut first)).await, Ok(Ok(n)) if n > 0) {
                return;
            }
            let conn = builder.serve_connection_with_upgrades(TokioIo::new(stream), service);
            let conn = watcher.watch(conn.into_owned());
            if let Err(e) = conn.await
                && !e.to_string().contains("connection closed")
            {
                tracing::debug!("Connection from {addr} ended with an error: {e}");
            }
        });
    }
    // Stop listening, so new connections are refused instead of waiting in the backlog
    drop(listener);
    // Running requests get a moment to finish; large transfers may be cut off. 20 s leaves time within Docker's stop
    // timeout (30 s in compose.yaml) to write the last log entries and close the database
    if tokio::time::timeout(Duration::from_secs(20), graceful.shutdown()).await.is_err() {
        tracing::warn!("Some connections were still open after 20 seconds and were closed");
    }
    Ok(())
}

/// `thirtyfile health`: connects to the local listen address (0.0.0.0 becomes 127.0.0.1) and requests /api/health
async fn health_probe(addr: &str) -> bool {
    // A host name (localhost:8080) may resolve to several addresses (IPv6, IPv4): try each one
    let targets: Vec<std::net::SocketAddr> = match addr.parse::<std::net::SocketAddr>() {
        Ok(a) => vec![a],
        Err(_) => tokio::net::lookup_host(addr).await.map(|list| list.collect()).unwrap_or_default(),
    };
    if targets.is_empty() {
        eprintln!("Invalid listen address: {addr}");
        return false;
    }
    for mut target in targets {
        if target.ip().is_unspecified() {
            target.set_ip(if target.is_ipv4() { std::net::Ipv4Addr::LOCALHOST.into() } else { std::net::Ipv6Addr::LOCALHOST.into() });
        }
        if probe_once(target).await {
            return true;
        }
    }
    false
}

async fn probe_once(target: std::net::SocketAddr) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let probe = async {
        let mut stream = tokio::net::TcpStream::connect(target).await?;
        stream.write_all(b"GET /api/health HTTP/1.0\r\nHost: localhost\r\n\r\n").await?;
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await?;
        Ok::<_, std::io::Error>(String::from_utf8_lossy(&buf).into_owned())
    };
    match tokio::time::timeout(Duration::from_secs(5), probe).await {
        Ok(Ok(res)) => {
            let status = res.lines().next().unwrap_or_default().to_string();
            let body = res.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or_default();
            println!("{status} {body}");
            status.split_whitespace().nth(1) == Some("200")
        }
        Ok(Err(e)) => {
            eprintln!("Can't connect to {target}: {e}");
            false
        }
        Err(_) => {
            eprintln!("Connection to {target} timed out");
            false
        }
    }
}

/// Health check (for Docker HEALTHCHECK and load balancers): no sign-in required; checks that the database is readable
/// Free space below this (on /data or /storage) makes the health check report "degraded": a full disk is the most likely
/// outage of a file server, and SQLite fails when it can't write
const LOW_DISK: u64 = 1024 * 1024 * 1024;

async fn health(axum::extract::State(st): axum::extract::State<AppState>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let version = VERSION;
    if let Err(e) = sqlx::query_scalar::<_, i64>("SELECT 1").fetch_one(&st.db).await {
        tracing::warn!("health check failed: {e}");
        return (axum::http::StatusCode::SERVICE_UNAVAILABLE, axum::Json(serde_json::json!({ "status": "error", "version": version })))
            .into_response();
    }
    let mut warnings = Vec::new();
    let mut disks = serde_json::Map::new();
    for (name, path) in [("data", &st.data_dir), ("storage", &st.storage_dir)] {
        if let Some((free, total)) = util::disk_space(path) {
            if free < LOW_DISK {
                warnings.push(format!("{name}: {} free", util::format_bytes_u64(free)));
            }
            disks.insert(name.into(), serde_json::json!({ "free_bytes": free, "total_bytes": total }));
        }
    }
    // Only whether each storage location is reachable: the reasons can name servers, and this endpoint is public
    let locations: serde_json::Map<String, serde_json::Value> =
        st.location_health.lock().unwrap().iter().map(|(id, h)| (id.clone(), serde_json::Value::from(if h.ok { "ok" } else { "offline" }))).collect();
    for (id, s) in &locations {
        if s != "ok" {
            warnings.push(format!("storage location {id} is offline"));
        }
    }
    let status = if warnings.is_empty() { "ok" } else { "degraded" };
    if !warnings.is_empty() {
        // Docker asks every 30 s: warn at most once an hour
        static LAST: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
        let t = util::now();
        if t - LAST.load(std::sync::atomic::Ordering::Relaxed) >= 3600 {
            LAST.store(t, std::sync::atomic::Ordering::Relaxed);
            tracing::warn!("Health check degraded: {}", warnings.join("; "));
        }
    }
    // Still 200: restarting the container doesn't free disk space or bring a storage service back
    axum::Json(serde_json::json!({ "status": status, "version": version, "disks": disks, "locations": locations })).into_response()
}

fn api() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/auth/login", post(auth::login))
        .route("/auth/options", get(reset::options))
        .route("/auth/forgot", post(reset::forgot))
        .route("/auth/reset", post(reset::reset))
        .route("/auth/login/2fa", post(twofactor::login_code))
        .route("/auth/login/2fa/setup", post(twofactor::login_setup))
        .route("/auth/logout", post(auth::logout))
        .merge(file_api())
        .route("/auth/password", axum::routing::put(auth::change_password))
        .route("/auth/sessions", get(sessions::list))
        .route("/auth/sessions/others", post(sessions::sign_out_others))
        .route("/auth/sessions/{id}", delete(sessions::sign_out))
        .route("/auth/app-passwords", get(tokens::list).post(tokens::create))
        .route("/auth/app-passwords/{id}", delete(tokens::delete))
        .route("/auth/2fa", get(twofactor::status))
        .route("/auth/2fa/setup", post(twofactor::start_setup))
        .route("/auth/2fa/enable", post(twofactor::enable))
        .route("/auth/2fa/disable", post(twofactor::disable))
        .route("/auth/2fa/recovery-codes", post(twofactor::new_recovery_codes_for_me))
        // Spaces and access (listing the spaces works with an app password too)
        .route("/drives", get(drives::list).layer(middleware::from_fn(tokens::allow)).post(drives::create))
        .route("/drives/{id}", patch(drives::update).delete(drives::delete))
        .route("/nodes/{id}/access", get(drives::access).post(drives::grant))
        .route("/grants/{id}", delete(drives::revoke))
        .route("/directory", get(drives::directory))
        .route("/activity", get(logs::activity))
        .route("/activity/export", get(logs::export_activity))
        .route("/nodes/{id}/activity", get(logs::node_history))
        .route("/share-access", get(logs::share_access))
        .route("/auth/sso/providers", get(sso::providers))
        .route("/auth/sso/{provider}/start", get(sso::start))
        .route("/auth/sso/{provider}/link", post(sso::start_link))
        .route("/auth/sso/{provider}/callback", get(sso::callback))
        .route("/auth/identities", get(sso::my_identities))
        .route("/auth/identities/{provider}", delete(sso::unlink))
        .route("/admin/sso", get(sso::get_settings).put(sso::update_settings))
        .route("/admin/email", get(mail::get_settings).put(mail::update_settings))
        .route("/admin/email/test", post(mail::test))
        .route("/notifications", get(notify::list).delete(notify::clear))
        .route("/notifications/read", post(notify::mark_read))
        .route("/notifications/settings", get(notify::get_settings).put(notify::update_settings))
        .route("/notifications/{id}", delete(notify::delete))
        .route("/branding", get(branding::get))
        .route("/branding.css", get(branding::css))
        .route("/branding/logo", get(branding::logo))
        .route("/branding/background", get(branding::background))
        .route("/branding/manifest.webmanifest", get(branding::manifest))
        .route("/admin/branding", put(branding::update))
        .route("/login-log", get(logs::login_log))
        .route("/login-log/export", get(logs::export_login_log))
        .route("/admin/logs", get(logs::get_status).put(logs::update_settings))
        .route("/admin/logs/archive", post(logs::archive_now))
        .route("/admin/logs/archives/{id}", get(logs::download_archive).delete(logs::delete_archive))
        // Sharing
        .route("/shares", get(shares::list).post(shares::create))
        .route("/shares/{id}", patch(shares::update).delete(shares::delete))
        .route("/public/shares/{token}", get(shares::public_info))
        .route("/public/shares/{token}/unlock", post(shares::unlock))
        .route("/public/shares/{token}/download", get(shares::public_download).post(shares::create_public_download_link))
        .route("/public/shares/{token}/download/{link}", get(shares::public_download_by_link))
        .route("/public/shares/{token}/nodes/{id}", get(shares::public_node))
        .route("/public/shares/{token}/nodes/{id}/children", get(shares::public_children))
        .route("/public/shares/{token}/nodes/{id}/content", get(shares::public_content))
        // Administration
        .route("/admin/users", get(admin::list).post(admin::create))
        .route("/admin/users/{id}", patch(admin::update).delete(admin::delete))
        .route("/admin/users/{id}/sessions", get(sessions::admin_list).delete(sessions::admin_sign_out_all))
        .route("/admin/users/{id}/sessions/{session}", delete(sessions::admin_sign_out))
        .route("/admin/users/{id}/2fa", delete(twofactor::admin_reset))
        .route("/admin/users/{id}/personal-space", post(personal::add).delete(personal::remove))
        .route("/admin/settings", get(admin::get_settings).patch(admin::update_settings))
        .route("/admin/drives", get(drives::admin_list))
        .route("/admin/drives/{id}/scan", post(drives::scan))
        .route("/admin/drives/{id}/location", axum::routing::put(locations::set_drive_location))
        .route("/admin/drives/{id}/migrate", post(locations::migrate))
        .route("/admin/migrations", get(locations::migrations))
        .route("/admin/storage", get(locations::list).post(locations::create))
        .route("/admin/storage/test", post(locations::test))
        .route("/admin/storage/{id}", patch(locations::update).delete(locations::delete))
        .route("/admin/storage/{id}/test", post(locations::test_existing))
        .route("/admin/storage/{id}/default", post(locations::set_default))
        .route("/admin/storage/{id}/spaces", get(locations::spaces))
        .route("/admin/storage/{id}/test-steps", post(location_tools::test_steps))
        .route("/admin/storage/{id}/browse", get(location_tools::browse))
        .route("/admin/storage/{id}/unused", get(location_tools::unused_status).post(location_tools::find_unused))
        .route("/admin/storage/{id}/unused/remove", post(location_tools::remove_unused))
        .route("/admin/groups", get(drives::list_groups).post(drives::create_group))
        .route("/admin/groups/{id}", patch(drives::update_group).delete(drives::delete_group))
        .fallback(|| async { error::AppError::not_found("API not found") })
}

/// Which responses to compress: text-like bodies of at least 1 KB. File downloads (octet-stream, images, video, zip)
/// are already compressed or served with Range requests, and compressing them would break Content-Length and resumable downloads.
#[derive(Clone, Copy)]
struct Compressible;

impl Predicate for Compressible {
    fn should_compress<B: axum::body::HttpBody>(&self, response: &axum::http::Response<B>) -> bool {
        // File contents (served with Content-Disposition) are streamed with an exact Content-Length and an ETag per content:
        // compressing them would hide the length (no download progress) and make the ETag ambiguous
        if response.status() == StatusCode::PARTIAL_CONTENT
            || response.headers().contains_key(header::CONTENT_RANGE)
            || response.headers().contains_key(header::CONTENT_DISPOSITION)
        {
            return false;
        }
        // Content-Length isn't set yet at this layer for JSON bodies, so ask the body itself when it knows its size
        let len = response.body().size_hint().exact().or_else(|| {
            response.headers().get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok())
        });
        if len.is_some_and(|len| len < 1024) {
            return false;
        }
        let Some(ct) = response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()) else { return false };
        let ct = ct.split(';').next().unwrap_or_default().trim();
        ct.starts_with("text/")
            || matches!(ct, "application/json" | "application/javascript" | "application/xml" | "image/svg+xml" | "application/manifest+json")
    }
}

/// What a reverse proxy tells about the request (`X-Forwarded-Host`, `X-Forwarded-Proto`) counts only when it comes from
/// a trusted proxy address (THIRTYFILE_TRUST_PROXY), like the visitor's address in `X-Forwarded-For`: from anyone else
/// the headers are removed before anything reads them. An HTTPS site also tells browsers to use HTTPS only (HSTS).
async fn forwarding(axum::extract::State(st): axum::extract::State<AppState>, mut req: Request, next: Next) -> Response {
    let peer = req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip());
    if !peer.is_some_and(|p| st.trust_proxy.trusts(p)) {
        req.headers_mut().remove("x-forwarded-host");
        req.headers_mut().remove("x-forwarded-proto");
    }
    let mut res = next.run(req).await;
    if st.https() {
        res.headers_mut().insert(header::STRICT_TRANSPORT_SECURITY, header::HeaderValue::from_static("max-age=31536000"));
    }
    res
}

/// Basic CSRF protection: requests that modify data must have an Origin matching Host, if they carry one
/// (behind a reverse proxy with THIRTYFILE_TRUST_PROXY set, X-Forwarded-Host is accepted too).
/// A request signed in with an app password as a Bearer token and without a session cookie carries no credential a
/// browser adds by itself, so another website can't send it on someone's behalf: it isn't checked. Basic credentials
/// are always checked: WebDAV answers with a sign-in challenge, after which a browser remembers them and may send them
/// by itself, to `/dav` and possibly the rest of the site (WebDAV clients don't send an Origin).
async fn same_origin(axum::extract::State(st): axum::extract::State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let dav = path == dav::PREFIX || path.starts_with("/dav/");
    let app_password_only = !dav
        && matches!(tokens::credential(req.headers()), Some(tokens::Credential::Bearer(_)))
        && auth::get_cookie(req.headers(), auth::SESSION_COOKIE).is_none();
    if !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS)
        && !app_password_only
        && let Some(origin) = req.headers().get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
            let origin_host = origin.split_once("://").map(|(_, h)| h).unwrap_or(origin);
            let h = req.headers();
            let hosts = [h.get("x-forwarded-host").filter(|_| st.trust_proxy.enabled()), h.get(header::HOST)];
            let ok = hosts.iter().flatten().filter_map(|v| v.to_str().ok()).any(|host| host == origin_host);
            if !ok {
                return (StatusCode::FORBIDDEN, "cross-origin request blocked").into_response();
            }
        }
    next.run(req).await
}

fn spawn_maintenance(st: AppState, trash_days: i64) {
    // Content of spaces deleted while the server stopped before it was all removed
    tree::purge_detached_later(&st);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        let mut hours: u32 = 0;
        loop {
            tick.tick().await;
            hours += 1;
            if hours.is_multiple_of(24) {
                db::optimize(&st.db).await;
            }
            // Once a day: put the usage counters back in step with the node table, should one ever drift
            if hours.is_multiple_of(24)
                && let Err(e) = tree::recompute_usage(&st).await
            {
                tracing::warn!("Couldn't recompute space usage: {}", e.message);
            }
            if trash_days > 0 {
                match nodes::purge_expired_trash(&st, trash_days).await {
                    Ok(n) if n > 0 => tracing::info!("Automatically purged {n} expired trash items"),
                    Err(e) => tracing::warn!("Failed to purge the trash: {}", e.message),
                    _ => {}
                }
            }
            // Spaces that are almost full and access that ends soon (#64)
            if let Err(e) = notify::check(&st).await {
                tracing::warn!("Couldn't check for notifications: {}", e.message);
            }
            match versions::prune(&st).await {
                Ok(n) if n > 0 => tracing::info!("Removed {n} earlier versions of files that are no longer kept"),
                Err(e) => tracing::warn!("Couldn't remove earlier versions of files: {}", e.message),
                _ => {}
            }
            if let Err(e) = upload::purge_expired(&st).await {
                tracing::warn!("Failed to clean up expired uploads: {}", e.message);
            }
            if let Err(e) = upload::clean_tmp(&st).await {
                tracing::warn!("Couldn't clean the temporary directory: {}", e.message);
            }
            auth::prune_login_failures(&st);
            logs::prune_share_views(&st);
            logs::daily_archive(&st).await;
            let _w = st.write_lock.lock().await;
            let _ = sqlx::query("DELETE FROM sessions WHERE expires_at < ?").bind(util::now()).execute(&st.db).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[tokio::test]
    async fn health_reports_disks_and_offline_storage_locations() {
        let env = crate::testutil::env().await;
        let body = |res: Response| async { serde_json::from_slice::<serde_json::Value>(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap() };
        let res = health(axum::extract::State(env.st.clone())).await;
        assert_eq!(res.status(), StatusCode::OK);
        let v = body(res).await;
        // Disk space is only read on Unix
        assert!(!cfg!(unix) || v["disks"]["data"]["total_bytes"].as_u64().unwrap() > 0);
        env.st.location_health.lock().unwrap().insert("nas".into(), state::LocationHealth { ok: false, error: Some("secret host".into()), checked_at: 0 });
        let v = body(health(axum::extract::State(env.st.clone())).await).await;
        assert_eq!((v["status"].as_str(), v["locations"]["nas"].as_str()), (Some("degraded"), Some("offline")));
        assert!(!v.to_string().contains("secret host"), "reasons stay private");
    }

    #[test]
    fn settings_accept_common_ways_of_writing_true_and_refuse_impossible_values() {
        let parse = |args: &[&str]| Config::try_parse_from(std::iter::once("thirtyfile").chain(args.iter().copied()));
        for yes in ["true", "TRUE", "1", "yes", "on"] {
            assert!(parse(&["--secure-cookie", yes]).unwrap().secure_cookie, "{yes}");
        }
        for no in ["false", "0", "no", "off"] {
            assert!(!parse(&["--secure-cookie", no]).unwrap().secure_cookie, "{no}");
        }
        assert!(parse(&["--trash-days=-5"]).is_err(), "a negative number of days would turn off emptying the trash");
        assert_eq!(parse(&["--trash-days", "0"]).unwrap().trash_days, 0);
    }

    #[test]
    fn help_doesnt_show_the_administrator_password() {
        use clap::CommandFactory;
        let cmd = Config::command();
        let arg = cmd.get_arguments().find(|a| a.get_id() == "admin_password").unwrap();
        assert!(arg.is_hide_env_values_set());
    }

    fn response(status: StatusCode, headers: &[(header::HeaderName, &str)]) -> axum::http::Response<String> {
        response_of(status, headers, 4096)
    }

    fn response_of(status: StatusCode, headers: &[(header::HeaderName, &str)], body_len: usize) -> axum::http::Response<String> {
        let mut r = axum::http::Response::new("x".repeat(body_len));
        *r.status_mut() = status;
        for (k, v) in headers {
            r.headers_mut().insert(k.clone(), v.parse().unwrap());
        }
        r
    }

    async fn call(app: &Router, method: Method, uri: &str, headers: &[(header::HeaderName, String)], body: Option<serde_json::Value>) -> Response {
        let mut req = axum::http::Request::builder().method(method).uri(uri).extension(axum::extract::ConnectInfo(std::net::SocketAddr::from(([10, 0, 0, 1], 5000))));
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let req = match body {
            Some(b) => req.header(header::CONTENT_TYPE, "application/json").body(axum::body::Body::from(b.to_string())),
            None => req.body(axum::body::Body::empty()),
        };
        app.clone().oneshot(req.unwrap()).await.unwrap()
    }

    async fn app_password(env: &testutil::TestEnv, user: &auth::User, scope: &str) -> String {
        let (_, cookie) = env.sign_in(user, "Test").await;
        let res = call(&router(env.st.clone()), Method::POST, "/api/auth/app-passwords", &[(header::COOKIE, cookie)], Some(serde_json::json!({ "name": "Script", "scope": scope, "password": testutil::password() }))).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["token"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn app_passwords_work_for_file_operations_only() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let app = router(env.st.clone());
        let token = app_password(&env, &admin, "write").await;
        let bearer = || vec![(header::AUTHORIZATION, format!("Bearer {token}"))];

        let res = call(&app, Method::GET, "/api/auth/me", &bearer(), None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!res.headers().contains_key(header::SET_COOKIE));
        assert_eq!(call(&app, Method::GET, "/api/drives", &bearer(), None).await.status(), StatusCode::OK);
        assert_eq!(call(&app, Method::GET, &format!("/api/nodes/{}/children", admin.root()), &bearer(), None).await.status(), StatusCode::OK);
        // Scripts send no Origin; a token request that does isn't a cross-site forgery either
        let mut with_origin = bearer();
        with_origin.push((header::ORIGIN, "https://elsewhere.example".into()));
        let folder = serde_json::json!({ "parent_id": admin.root(), "name": "From a script" });
        assert_eq!(call(&app, Method::POST, "/api/folders", &with_origin, Some(folder)).await.status(), StatusCode::OK);

        // The account, sign-in methods, sharing and administration need a browser session
        for (method, uri, body) in [
            (Method::GET, "/api/auth/sessions", None),
            (Method::GET, "/api/auth/app-passwords", None),
            (Method::POST, "/api/auth/app-passwords", Some(serde_json::json!({ "name": "x", "scope": "write" }))),
            (Method::PUT, "/api/auth/password", Some(serde_json::json!({ "current": "a", "new": "abcdefgh" }))),
            (Method::GET, "/api/auth/identities", None),
            (Method::POST, "/api/auth/sso/google/link", Some(serde_json::json!({}))),
            (Method::GET, "/api/shares", None),
            (Method::GET, "/api/admin/users", None),
            (Method::PATCH, "/api/admin/settings", Some(serde_json::json!({ "allow_user_drives": true }))),
            (Method::POST, "/api/drives", Some(serde_json::json!({ "name": "Team" }))),
        ] {
            assert_eq!(call(&app, method, uri, &bearer(), body).await.status(), StatusCode::UNAUTHORIZED, "{uri}");
        }
    }

    #[tokio::test]
    async fn read_only_app_passwords_cant_change_files() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let app = router(env.st.clone());
        let token = app_password(&env, &amy, "read").await;
        let auth = vec![(header::AUTHORIZATION, format!("Basic {}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, format!("amy:{token}"))))];
        assert_eq!(call(&app, Method::GET, &format!("/api/nodes/{}/children", amy.root()), &auth, None).await.status(), StatusCode::OK);
        let folder = serde_json::json!({ "parent_id": amy.root(), "name": "Nope" });
        assert_eq!(call(&app, Method::POST, "/api/folders", &auth, Some(folder)).await.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn proxy_headers_count_only_from_a_trusted_proxy_and_https_sites_ask_for_https() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let app = router(env.st.clone());
        let (_, cookie) = env.sign_in(&amy, "Test").await;
        // Not from a trusted proxy (THIRTYFILE_TRUST_PROXY is off): X-Forwarded-Host can't make another site's request look
        // like this one's
        let forged = vec![
            (header::COOKIE, cookie.clone()),
            (header::ORIGIN, "https://evil.example".into()),
            (header::HeaderName::from_static("x-forwarded-host"), "evil.example".into()),
        ];
        let folder = serde_json::json!({ "parent_id": amy.root(), "name": "Forged" });
        assert_eq!(call(&app, Method::POST, "/api/folders", &forged, Some(folder)).await.status(), StatusCode::FORBIDDEN);
        assert!(call(&app, Method::GET, "/api/auth/me", &[(header::COOKIE, cookie.clone())], None).await.headers().get(header::STRICT_TRANSPORT_SECURITY).is_none());

        // An https Site URL: cookies are Secure and browsers are told to keep to HTTPS; signing out clears their cache
        env.st.system.write().unwrap().public_url = "https://drive.example.com".into();
        let res = call(&app, Method::GET, "/api/auth/me", &[(header::COOKIE, cookie.clone())], None).await;
        assert_eq!(res.headers()[header::STRICT_TRANSPORT_SECURITY], "max-age=31536000");
        assert!(auth::cookie_header(&env.st, "x", "y", "/", 1).ends_with("; Secure"));
        let res = call(&app, Method::POST, "/api/auth/logout", &[(header::COOKIE, cookie)], None).await;
        assert_eq!(res.headers()["clear-site-data"], "\"cache\"");
    }

    #[tokio::test]
    async fn sessions_still_need_the_same_origin() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let app = router(env.st.clone());
        let token = app_password(&env, &amy, "write").await;
        let (_, cookie) = env.sign_in(&amy, "Test").await;
        // A cookie together with some token: the browser would add the cookie by itself, so the origin is checked
        let headers = vec![(header::COOKIE, cookie), (header::AUTHORIZATION, format!("Bearer {token}")), (header::ORIGIN, "https://elsewhere.example".into())];
        let folder = serde_json::json!({ "parent_id": amy.root(), "name": "Forged" });
        assert_eq!(call(&app, Method::POST, "/api/folders", &headers, Some(folder)).await.status(), StatusCode::FORBIDDEN);

        // Basic credentials a browser remembered from the WebDAV sign-in prompt: checked too, without a cookie
        let basic = format!("Basic {}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, format!("amy:{token}")));
        let headers = vec![(header::AUTHORIZATION, basic.clone()), (header::ORIGIN, "https://elsewhere.example".into())];
        let folder = serde_json::json!({ "parent_id": amy.root(), "name": "Forged" });
        assert_eq!(call(&app, Method::POST, "/api/folders", &headers, Some(folder.clone())).await.status(), StatusCode::FORBIDDEN);
        // A script sends no Origin, or a Bearer token from anywhere: both work
        assert_eq!(call(&app, Method::POST, "/api/folders", &[(header::AUTHORIZATION, basic)], Some(folder)).await.status(), StatusCode::OK);
        let headers = vec![(header::AUTHORIZATION, format!("Bearer {token}")), (header::ORIGIN, "https://elsewhere.example".into())];
        let folder = serde_json::json!({ "parent_id": amy.root(), "name": "From a script" });
        assert_eq!(call(&app, Method::POST, "/api/folders", &headers, Some(folder)).await.status(), StatusCode::OK);
    }

    #[test]
    fn only_text_like_full_responses_are_compressed() {
        let ok = |ct: &str| Compressible.should_compress(&response(StatusCode::OK, &[(header::CONTENT_TYPE, ct)]));
        assert!(ok("application/json"));
        assert!(ok("text/html; charset=utf-8"));
        assert!(ok("text/csv; charset=utf-8"));
        assert!(ok("image/svg+xml"));
        assert!(!ok("application/octet-stream"));
        assert!(!ok("application/zip"));
        assert!(!ok("video/mp4"));
        assert!(!ok("image/jpeg"));
        assert!(!Compressible.should_compress(&response(StatusCode::OK, &[(header::CONTENT_TYPE, "text/plain"), (header::CONTENT_DISPOSITION, "inline; filename=\"a.txt\"")])));
        // Range responses and tiny bodies are left alone
        assert!(!Compressible.should_compress(&response(StatusCode::PARTIAL_CONTENT, &[(header::CONTENT_TYPE, "text/plain")])));
        assert!(!Compressible.should_compress(&response_of(StatusCode::OK, &[(header::CONTENT_TYPE, "text/plain")], 20)));
        assert!(Compressible.should_compress(&response_of(StatusCode::OK, &[(header::CONTENT_TYPE, "text/plain")], 4096)));
    }
}
