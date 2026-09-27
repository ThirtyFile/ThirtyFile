mod admin;
mod auth;
mod db;
mod drives;
mod error;
mod files;
mod folders;
mod fsops;
mod locations;
mod logs;
mod branding;
mod ftp;
mod sftp;
mod sso;
mod nodes;
#[cfg(unix)]
mod privileges;
mod shares;
mod state;
mod storage;
#[cfg(test)]
mod testutil;
mod tree;
mod upload;
mod util;
mod web;
mod zip;

use std::{path::PathBuf, sync::Arc, time::Duration};

use axum::{
    Router,
    extract::{DefaultBodyLimit, Request},
    http::{Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, head, patch, post, put},
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
    /// Check whether the running service is healthy (for Docker HEALTHCHECK): exit code 0 when healthy
    Health,
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
    let cfg = Config::parse();
    let runtime = || tokio::runtime::Builder::new_multi_thread().enable_all().build();

    // The health check only connects to the running service and doesn't touch the data directory
    if let Some(Command::Health) = &cfg.command {
        std::process::exit(if runtime()?.block_on(health_probe(&cfg.addr)) { 0 } else { 1 });
    }

    let storage = storage_dir(&cfg.data, cfg.storage.as_deref());

    // Before the runtime starts its threads, so that all of them run as the new user
    #[cfg(unix)]
    if let Some(user) = &cfg.run_as {
        privileges::drop_to(user, &[&cfg.data, &storage])?;
    }

    runtime()?.block_on(run(cfg, storage))
}

/// Where the built-in storage location keeps file contents. Version 0.1.0 always used blobs in the
/// data directory; when files are still there, they stay in use so that nothing seems to disappear.
fn storage_dir(data: &std::path::Path, configured: Option<&std::path::Path>) -> PathBuf {
    let legacy = data.join("blobs");
    let Some(dir) = configured.filter(|d| *d != legacy) else {
        return legacy;
    };
    let legacy_in_use = std::fs::read_dir(&legacy).is_ok_and(|mut entries| entries.next().is_some());
    if legacy_in_use {
        tracing::warn!(
            "Files are still kept in {}, so that folder is used instead of {}. To use {1}, stop ThirtyFile, move everything from {0} into {1} and start it again.",
            legacy.display(),
            dir.display()
        );
        return legacy;
    }
    dir.to_path_buf()
}

async fn run(cfg: Config, storage: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    for dir in ["tmp", "thumbs"] {
        std::fs::create_dir_all(cfg.data.join(dir))?;
    }
    std::fs::create_dir_all(&storage)?;
    let db = db::connect(&cfg.data.join("drive.db")).await?;

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
        auth::validate_password(password).map_err(|e| e.message)?;
        let hash = auth::hash_password(password.clone()).await.map_err(|e| e.message)?;
        let res = sqlx::query("UPDATE users SET password_hash = ?, disabled = 0 WHERE username = ?")
            .bind(hash)
            .bind(username)
            .execute(&db)
            .await?;
        if res.rows_affected() == 0 {
            return Err(format!("User not found: {username}").into());
        }
        sqlx::query("DELETE FROM sessions WHERE user_id = (SELECT id FROM users WHERE username = ?)")
            .bind(username)
            .execute(&db)
            .await?;
        println!("Password reset for {username}");
        return Ok(());
    }

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
    db::bootstrap_admin(&db, admin_password.as_deref()).await.map_err(|e| e.message)?;
    let secret = db::load_secret(&db).await?;
    let system = db::load_system_settings(&db).await?;
    // Settings that are probably wrong together: said once at startup
    if !cfg.secure_cookie && cfg.trust_proxy.enabled() {
        tracing::warn!("THIRTYFILE_TRUST_PROXY is on but THIRTYFILE_SECURE_COOKIE is off: if the proxy serves HTTPS, set THIRTYFILE_SECURE_COOKIE=true");
    }
    if !cfg.secure_cookie && system.public_url.starts_with("https://") {
        tracing::warn!("The site URL uses https but THIRTYFILE_SECURE_COOKIE is off: set THIRTYFILE_SECURE_COOKIE=true");
    }
    let (storages, default_location) = locations::load_all(&db, &storage).await?;
    let log_settings = logs::load_settings(&db).await;
    let branding = branding::load(&db).await;
    let sso_settings = sso::load(&db).await;
    let (log_tx, log_rx) = logs::channel();
    let state = AppState(Arc::new(Inner {
        db,
        storages: std::sync::RwLock::new(storages),
        default_location: std::sync::RwLock::new(default_location),
        migrations: Default::default(),
        data_dir: cfg.data.clone(),
        storage_dir: storage,
        secret,
        secure_cookie: cfg.secure_cookie,
        trust_proxy: cfg.trust_proxy,
        max_upload: cfg.max_upload_mb.checked_mul(1024 * 1024).ok_or("THIRTYFILE_MAX_UPLOAD_MB is too large")?,
        write_lock: tokio::sync::Mutex::new(()),
        active_uploads: Default::default(),
        login_failures: Default::default(),
        thumb_permits: tokio::sync::Semaphore::new(2),
        system: std::sync::RwLock::new(system),
        blob_guard: Default::default(),
        logs: std::sync::RwLock::new(log_settings),
        branding: std::sync::RwLock::new(branding),
        location_health: Default::default(),
        sso: std::sync::RwLock::new(sso_settings),
        sso_pending: Default::default(),
        archive_lock: Default::default(),
        share_views: Default::default(),
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
    folders::spawn_scanner(state.clone());

    // JSON requests that take longer than this are cut off (a stuck storage service, a slow provider). Requests that
    // carry a body to store, and thumbnails (which queue), are outside the limit (`untimed`); downloads stream after
    // the handler returned, so the limit doesn't apply to them either
    let db_pool = state.db.clone();
    let app = Router::new()
        .nest("/api", api().layer(TimeoutLayer::with_status_code(StatusCode::GATEWAY_TIMEOUT, Duration::from_secs(120))).merge(untimed()))
        .fallback(web::serve)
        .layer(middleware::from_fn_with_state(state.clone(), same_origin))
        // gzip / brotli for JSON, HTML, JS, CSS and SVG (see `Compressible`); file contents and other downloads are never compressed
        // Level 4: brotli's default (11) spends far more CPU per response than it saves on JSON and HTML; gzip 4 is likewise the sweet spot
        .layer(CompressionLayer::new().gzip(true).br(true).quality(CompressionLevel::Precise(4)).compress_when(Compressible))
        .layer(TraceLayer::new_for_http())
        // A bug hit by one request answers that request with an error instead of stopping the server for everyone
        .layer(tower_http::catch_panic::CatchPanicLayer::new())
        .with_state(state);

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
    Router::new()
        .route("/files/{id}/content", put(files::save_content).layer(DefaultBodyLimit::max(files::MAX_EDIT_BYTES)))
        .route("/files/{id}/thumbnail", get(files::thumbnail))
        .route("/public/shares/{token}/nodes/{id}/thumbnail", get(shares::public_thumbnail))
        .route("/admin/branding/logo/{variant}", put(branding::upload_logo).delete(branding::delete_logo))
        .route("/admin/branding/background", put(branding::upload_background).delete(branding::delete_background).layer(DefaultBodyLimit::max(branding::MAX_BACKGROUND + 1024)))
        .route("/uploads", post(upload::create).options(upload::options))
        .route(
            "/uploads/{id}",
            head(upload::head).patch(upload::patch).delete(upload::delete).layer(DefaultBodyLimit::disable()),
        )
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
async fn health(axum::extract::State(st): axum::extract::State<AppState>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let version = VERSION;
    match sqlx::query_scalar::<_, i64>("SELECT 1").fetch_one(&st.db).await {
        Ok(_) => axum::Json(serde_json::json!({ "status": "ok", "version": version })).into_response(),
        Err(e) => {
            tracing::warn!("health check failed: {e}");
            (axum::http::StatusCode::SERVICE_UNAVAILABLE, axum::Json(serde_json::json!({ "status": "error", "version": version }))).into_response()
        }
    }
}

fn api() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/me", get(auth::me))
        .route("/auth/password", axum::routing::put(auth::change_password))
        // File tree
        .route("/nodes/{id}", get(nodes::get).patch(nodes::rename))
        .route("/nodes/{id}/children", get(nodes::children))
        .route("/nodes/move", post(nodes::move_nodes))
        .route("/nodes/copy", post(nodes::copy_nodes))
        .route("/nodes/trash", post(nodes::trash))
        .route("/folders", post(nodes::create_folder))
        .route("/trash", get(nodes::list_trash))
        .route("/trash/restore", post(nodes::restore))
        .route("/trash/delete", post(nodes::delete_forever))
        .route("/trash/empty", post(nodes::empty_trash))
        .route("/search", get(nodes::search))
        .route("/recent", get(nodes::recent))
        .route("/favorites", get(nodes::favorites))
        .route("/nodes/favorite", post(nodes::set_favorite))
        .route("/shared-with-me", get(nodes::shared_with_me))
        // Spaces and access
        .route("/drives", get(drives::list).post(drives::create))
        .route("/drives/{id}", patch(drives::update).delete(drives::delete))
        .route("/nodes/{id}/access", get(drives::access).post(drives::grant))
        .route("/grants/{id}", delete(drives::revoke))
        .route("/directory", get(drives::directory))
        .route("/activity", get(logs::activity))
        .route("/activity/export", get(logs::export_activity))
        .route("/share-access", get(logs::share_access))
        .route("/auth/sso/providers", get(sso::providers))
        .route("/auth/sso/{provider}/start", get(sso::start))
        .route("/auth/sso/{provider}/link", post(sso::start_link))
        .route("/auth/sso/{provider}/callback", get(sso::callback))
        .route("/auth/identities", get(sso::my_identities))
        .route("/auth/identities/{provider}", delete(sso::unlink))
        .route("/admin/sso", get(sso::get_settings).put(sso::update_settings))
        .route("/branding", get(branding::get))
        .route("/branding.css", get(branding::css))
        .route("/branding/logo", get(branding::logo))
        .route("/branding/background", get(branding::background))
        .route("/admin/branding", put(branding::update))
        .route("/login-log", get(logs::login_log))
        .route("/login-log/export", get(logs::export_login_log))
        .route("/admin/logs", get(logs::get_status).put(logs::update_settings))
        .route("/admin/logs/archive", post(logs::archive_now))
        .route("/admin/logs/archives/{id}", get(logs::download_archive).delete(logs::delete_archive))
        // File content
        .route("/files/{id}/content", get(files::content))
        .route("/download", get(files::download))
        // Sharing
        .route("/shares", get(shares::list).post(shares::create))
        .route("/shares/{id}", delete(shares::delete))
        .route("/public/shares/{token}", get(shares::public_info))
        .route("/public/shares/{token}/unlock", post(shares::unlock))
        .route("/public/shares/{token}/download", get(shares::public_download))
        .route("/public/shares/{token}/nodes/{id}", get(shares::public_node))
        .route("/public/shares/{token}/nodes/{id}/children", get(shares::public_children))
        .route("/public/shares/{token}/nodes/{id}/content", get(shares::public_content))
        // Administration
        .route("/admin/users", get(admin::list).post(admin::create))
        .route("/admin/users/{id}", patch(admin::update).delete(admin::delete))
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

/// Basic CSRF protection: requests that modify data must have an Origin matching Host, if they carry one
/// (behind a reverse proxy with THIRTYFILE_TRUST_PROXY set, X-Forwarded-Host is accepted too).
async fn same_origin(axum::extract::State(st): axum::extract::State<AppState>, req: Request, next: Next) -> Response {
    if !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS)
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
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        let mut hours: u32 = 0;
        loop {
            tick.tick().await;
            hours += 1;
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

    #[test]
    fn storage_folder_is_separate_unless_files_are_still_in_the_data_folder() {
        let data = std::env::temp_dir().join(format!("thirtyfile-test-{}", util::new_id()));
        let storage = data.with_extension("storage");
        assert_eq!(storage_dir(&data, None), data.join("blobs"));
        assert_eq!(storage_dir(&data, Some(&storage)), storage);
        // An empty blobs folder left from version 0.1.0 doesn't count
        std::fs::create_dir_all(data.join("blobs")).unwrap();
        assert_eq!(storage_dir(&data, Some(&storage)), storage);
        std::fs::create_dir_all(data.join("blobs").join("ab")).unwrap();
        assert_eq!(storage_dir(&data, Some(&storage)), data.join("blobs"));
        std::fs::remove_dir_all(&data).unwrap();
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
