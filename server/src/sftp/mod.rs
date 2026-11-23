//! SSH file access only: no shell, commands, links or forwarding. Revocation is checked on every request.

pub mod keys;
mod session;
pub mod settings;
#[cfg(test)]
mod tests;

use crate::{
    auth::{
        self, User,
        app_passwords::{self, Credential},
    },
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{
    extract::ConnectInfo,
    http::{Method, Request},
};
use std::{collections::HashMap, io, net::SocketAddr, sync::Arc, time::Duration};
use thirtysftp::server::ServerConfig;
use thirtyssh::{
    keys::{HashAlg, PrivateKey, PrivateKeyReader, PublicKey},
    server, session as ssh,
};

/// How often the settings are read again when nothing says they changed
const IDLE: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct Memory {
    /// Told when the settings change (`settings::update`), so the listener follows at once
    pub changed: tokio::sync::Notify,
    pub address: crate::sync::RwLock<String>,
    pub fingerprint: crate::sync::RwLock<String>,
    pub error: crate::sync::RwLock<Option<String>>,
    sessions: crate::sync::Mutex<HashMap<i64, usize>>,
}

struct Permit {
    st: AppState,
    user: i64,
}
impl Drop for Permit {
    fn drop(&mut self) {
        let mut sessions = self.st.part::<Memory>().sessions.lock();
        if let Some(n) = sessions.get_mut(&self.user) {
            *n -= 1;
            if *n == 0 {
                sessions.remove(&self.user);
            }
        }
    }
}

/// Tests: how many times each account was checked in full (`Login::user`)
#[cfg(test)]
pub(crate) static CHECKS: crate::sync::Mutex<std::collections::BTreeMap<String, usize>> = crate::sync::Mutex::new(std::collections::BTreeMap::new());

#[derive(Clone)]
enum Login {
    Password { username: String, token: String },
    Key { username: String, key_id: String },
}

impl Login {
    async fn user(&self, st: &AppState, peer: SocketAddr, write: bool) -> AppResult<User> {
        #[cfg(test)]
        {
            let name = match self {
                Self::Password { username, .. } | Self::Key { username, .. } => username.clone(),
            };
            *CHECKS.lock().entry(name).or_default() += 1;
        }
        let cfg = settings::load(st).await?;
        if !cfg.enabled {
            return Err(AppError::unauthorized());
        }
        let mut user = match self {
            Self::Password { username, token } => {
                let mut req = Request::builder().method(if write { Method::PUT } else { Method::GET }).body(()).map_err(AppError::internal)?;
                req.extensions_mut().insert(ConnectInfo(peer));
                let (parts, _) = req.into_parts();
                let mut user = app_passwords::authenticate(&parts, st, Credential::Basic { username: username.clone(), token: token.clone() }).await?;
                let id = token
                    .strip_prefix(app_passwords::PREFIX)
                    .and_then(|value| value.split_once('_'))
                    .map(|(id, _)| id.to_string())
                    .ok_or_else(AppError::unauthorized)?;
                user.sftp_credential = Some(auth::sftp::SftpCredential::AppPassword(id));
                user
            }
            Self::Key { username, key_id } => {
                if !cfg.keys() {
                    return Err(AppError::unauthorized());
                }
                let row: Option<(i64, String)> = sqlx::query_as(
                    "SELECT u.id, k.id FROM sftp_keys k JOIN users u ON u.id = k.user_id WHERE u.username = ? COLLATE NOCASE AND k.id = ? AND u.disabled = 0",
                )
                .bind(username)
                .bind(key_id)
                .fetch_optional(&st.db)
                .await?;
                let (user_id, id) = row.ok_or_else(AppError::unauthorized)?;
                let mut c = st.db.acquire().await?;
                let mut user = auth::user_by_id(st, &mut c, user_id).await?.ok_or_else(AppError::unauthorized)?;
                user.sftp_credential = Some(auth::sftp::SftpCredential::Key(id.clone()));
                drop(c);
                sqlx::query("UPDATE sftp_keys SET last_used_at = ? WHERE id = ? AND (last_used_at IS NULL OR last_used_at < ?)")
                    .bind(crate::util::now())
                    .bind(id)
                    .bind(crate::util::now() - auth::SESSION_TOUCH)
                    .execute(&st.db)
                    .await?;
                user
            }
        };
        user.activity_source = Some("SFTP".to_string());
        Ok(user)
    }
}

pub struct Listener {
    stop: tokio::sync::watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl Listener {
    pub fn stop(&self) {
        let _ = self.stop.send(true);
    }
    pub async fn finish(mut self) {
        self.stop();
        if let Some(mut task) = self.task.take()
            && tokio::time::timeout(Duration::from_secs(3), &mut task).await.is_err()
        {
            task.abort();
        }
    }
}
impl Drop for Listener {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Optional file access must not keep HTTP from starting. Retry its configured address after a bind failure.
pub fn supervise(st: AppState, address: &str) -> Listener {
    *st.part::<Memory>().address.write() = address.to_string();
    let address = address.to_string();
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(async move {
        match host_key(&st).map_err(|error| error.to_string()) {
            Ok(key) => *st.part::<Memory>().fingerprint.write() = key.public_key().fingerprint(HashAlg::Sha256).to_string(),
            Err(error) => {
                tracing::warn!("SFTP host key is unavailable: {error}; HTTP remains available");
                *st.part::<Memory>().error.write() = Some(error);
            }
        }
        let mut active: Option<Listener> = None;
        let mut retry = tokio::time::Instant::now();
        let (mut first, mut enabled) = (true, false);
        loop {
            // The settings are read right away, then when they change, or now and then: every half second while a
            // start that failed waits to be tried again, else every few seconds
            let wait = if std::mem::take(&mut first) {
                Duration::ZERO
            } else if enabled && active.is_none() {
                Duration::from_millis(500)
            } else {
                IDLE
            };
            tokio::select! {
                _ = stopped.changed() => break,
                _ = tokio::time::sleep(wait) => (),
                _ = st.part::<Memory>().changed.notified() => (),
            }
            enabled = settings::load(&st).await.is_ok_and(|c| c.enabled);
            if !enabled {
                if let Some(listener) = active.take() {
                    listener.finish().await;
                    *st.part::<Memory>().address.write() = address.clone();
                }
                continue;
            }
            if active.is_none() && tokio::time::Instant::now() >= retry {
                // Keep non-Send error values out of the spawned task across an await.
                match start(st.clone(), &address).await.map_err(|error| error.to_string()) {
                    Ok(listener) => {
                        *st.part::<Memory>().error.write() = None;
                        active = Some(listener);
                    }
                    Err(error) => {
                        if st.part::<Memory>().error.read().as_ref() != Some(&error) {
                            tracing::warn!("SFTP is unavailable: {error}; HTTP remains available");
                        }
                        *st.part::<Memory>().error.write() = Some(error);
                        retry = tokio::time::Instant::now() + Duration::from_secs(5);
                    }
                }
            }
        }
        if let Some(listener) = active {
            listener.finish().await;
        }
    });
    Listener { stop, task: Some(task) }
}

fn host_key(st: &AppState) -> Result<PrivateKey, Box<dyn std::error::Error>> {
    let path = st.data_dir.join("sftp_host_ed25519");
    Ok(if path.exists() {
        PrivateKeyReader::new().file(&path)?
    } else {
        let key = PrivateKey::from(thirtyssh::keys::ssh_key::private::Ed25519Keypair::from_seed(&rand::random()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let mut file = options.open(&path)?;
        file.write_all(key.to_openssh(thirtyssh::keys::ssh_key::LineEnding::LF)?.as_bytes())?;
        file.sync_all()?;
        key
    })
}

pub async fn start(st: AppState, address: &str) -> Result<Listener, Box<dyn std::error::Error>> {
    let key = host_key(&st)?;
    let socket = tokio::net::TcpListener::bind(address).await?;
    *st.part::<Memory>().address.write() = socket.local_addr()?.to_string();
    *st.part::<Memory>().fingerprint.write() = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let mut config = server::Config::new(key);
    config.authentication.max_attempts = std::num::NonZeroUsize::new(5).expect("positive attempt limit");
    config.authentication.rejection_delay = Duration::from_millis(500);
    config.inactivity_timeout = Some(Duration::from_secs(300));
    config.handshake_timeout = Duration::from_secs(10);
    config.authentication_timeout = Duration::from_secs(20);
    let config = Arc::new(config);
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let _task = tokio::spawn(async move {
        let slots = Arc::new(tokio::sync::Semaphore::new(256));
        let pending = auth::pending::Pending::default();
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                _ = stopped.changed() => break,
                Some(_) = tasks.join_next(), if !tasks.is_empty() => (),
                accepted = socket.accept() => match accepted {
                    Ok((stream, peer)) => {
                        let Some(waiting) = pending.admit(peer.ip()) else { continue };
                        let Ok(slot) = slots.clone().try_acquire_owned() else { continue };
                        let (st, config, stopped) = (st.clone(), config.clone(), stopped.clone());
                        tasks.spawn(serve_connection(st, config, stream, peer, stopped, (slot, waiting)));
                    },
                    Err(e) => { tracing::error!("SFTP listener failed: {e}"); break; },
                }
            }
        }
        // Signal every connection, then give it time to send SSH disconnect and release staged files.
        while tokio::time::timeout(Duration::from_secs(2), tasks.join_next()).await.is_ok_and(|r| r.is_some()) {}
    });
    Ok(Listener { stop, task: Some(_task) })
}

async fn serve_connection(
    st: AppState,
    config: Arc<server::Config<PrivateKey>>,
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    mut stopped: tokio::sync::watch::Receiver<bool>,
    (_slot, mut waiting): (tokio::sync::OwnedSemaphorePermit, auth::pending::PendingGuard),
) {
    let connection = Connection { st, peer, login: None, permit: None, channel_used: false };
    // The handshake and the sign-in are limited by the server's settings (10 and 20 seconds)
    let accepted = tokio::select! {
        accepted = server::accept_over(config, stream, Some(peer), connection) => accepted,
        _ = stopped.changed() => return,
    };
    let Ok(mut running) = accepted else { return };
    waiting.signed_in();
    tokio::select! {
        _ = &mut running => {}
        _ = stopped.changed() => {
            let _ = tokio::time::timeout(Duration::from_secs(2), running.disconnect()).await;
        }
    }
}

struct Connection {
    st: AppState,
    peer: SocketAddr,
    login: Option<Login>,
    permit: Option<Arc<Permit>>,
    channel_used: bool,
}

impl Connection {
    /// Whether the sign-in is accepted (and the account has a free session)
    async fn accept(&mut self, login: Login) -> bool {
        let Ok(user) = login.user(&self.st, self.peer, false).await else { return false };
        let mut sessions = self.st.part::<Memory>().sessions.lock();
        let count = sessions.entry(user.id).or_default();
        if *count >= 5 {
            return false;
        }
        *count += 1;
        self.permit = Some(Arc::new(Permit { st: self.st.clone(), user: user.id }));
        self.login = Some(login);
        true
    }
}

impl server::AuthPolicy for Connection {
    async fn password(&mut self, username: &str, password: &[u8]) -> io::Result<bool> {
        let Ok(token) = std::str::from_utf8(password) else { return Ok(false) };
        Ok(self.accept(Login::Password { username: username.to_string(), token: token.to_string() }).await)
    }
    /// A key offered without a signature yet (an SSH agent offers each of its keys in turn): accepted only if it is one
    /// of the account's, without counting the others as failed sign-ins
    async fn public_key_probe(&mut self, username: &str, key: &PublicKey) -> io::Result<bool> {
        let ip = self.peer.ip().to_string();
        if auth::attempts_exhausted(&self.st, &format!("ip:{}", auth::limit_key_ip(&ip)), auth::IP_FAIL_LIMIT) {
            return Ok(false);
        }
        let (known,): (bool,) = sqlx::query_as(
            "SELECT EXISTS(SELECT 1 FROM sftp_keys k JOIN users u ON u.id = k.user_id WHERE u.username = ? COLLATE NOCASE AND k.fingerprint = ? AND u.disabled = 0)",
        )
        .bind(username)
        .bind(key.fingerprint(HashAlg::Sha256).to_string())
        .fetch_one(&self.st.db)
        .await
        .unwrap_or((false,));
        Ok(known)
    }
    async fn public_key_verified(&mut self, username: &str, key: &PublicKey) -> io::Result<bool> {
        let ip = self.peer.ip().to_string();
        let limit = format!("ip:{}", auth::limit_key_ip(&ip));
        if auth::attempts_exhausted(&self.st, &limit, auth::IP_FAIL_LIMIT) {
            return Ok(false);
        }
        let key_id: Option<(String,)> = sqlx::query_as(
            "SELECT k.id FROM sftp_keys k JOIN users u ON u.id = k.user_id WHERE u.username = ? COLLATE NOCASE AND k.fingerprint = ? AND u.disabled = 0",
        )
        .bind(username)
        .bind(key.fingerprint(HashAlg::Sha256).to_string())
        .fetch_optional(&self.st.db)
        .await
        .unwrap_or(None);
        // An unknown key isn't counted as a failed sign-in: nobody guesses a private key, and an SSH agent tries each
        // of its keys in turn (the connection's own limit on attempts still applies)
        Ok(match key_id {
            Some((key_id,)) => self.accept(Login::Key { username: username.to_string(), key_id }).await,
            None => false,
        })
    }
}

impl ssh::Handler for Connection {
    /// One session channel for SFTP: no shell, no commands, no forwarding. The subsystem request is answered by the
    /// SFTP server, which refuses every other request.
    async fn channel_open(&mut self, request: ssh::IncomingChannel, _: &ssh::Context) -> io::Result<()> {
        if !self.channel_used
            && let (Some(login), Some(permit)) = (self.login.clone(), self.permit.clone())
        {
            self.channel_used = true;
            let files = Arc::new(session::Files::new(self.st.clone(), self.peer, login, permit));
            let config = ServerConfig::default()
                .with_max_packet(512 * 1024)
                .with_max_handles(session::MAX_HANDLES)
                .with_max_read_length(256 * 1024)
                .with_max_write_length(256 * 1024);
            thirtysftp::ssh::accept_session(request, files, config).await
        } else {
            // Administratively prohibited
            request.refuse(1).await
        }
    }
}
