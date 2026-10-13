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
use russh::{
    Channel, ChannelId,
    keys::{PrivateKey, PublicKey, ssh_key::HashAlg},
    server::{Auth, ChannelOpenHandle, Msg, Session},
};
use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};

#[derive(Default)]
pub struct Memory {
    pub address: crate::sync::RwLock<String>,
    pub fingerprint: crate::sync::RwLock<String>,
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

#[derive(Clone)]
enum Login {
    Password { username: String, token: String },
    Key { username: String, key_id: String },
}

impl Login {
    async fn user(&self, st: &AppState, peer: SocketAddr, write: bool) -> AppResult<User> {
        if !settings::flag(st, "sftp_enabled").await? {
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
                if !settings::flag(st, "sftp_keys_enabled").await? {
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

pub async fn start(st: AppState, address: &str) -> Result<Listener, Box<dyn std::error::Error>> {
    let path = st.data_dir.join("sftp_host_ed25519");
    let key = if path.exists() {
        russh::keys::load_secret_key(&path, None)?
    } else {
        let key = PrivateKey::from(russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&rand::random()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let mut file = options.open(&path)?;
        file.write_all(key.to_openssh(russh::keys::ssh_key::LineEnding::LF)?.as_bytes())?;
        file.sync_all()?;
        key
    };
    let socket = tokio::net::TcpListener::bind(address).await?;
    *st.part::<Memory>().address.write() = socket.local_addr()?.to_string();
    *st.part::<Memory>().fingerprint.write() = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let config = Arc::new(russh::server::Config {
        keys: vec![key],
        max_auth_attempts: 5,
        inactivity_timeout: Some(Duration::from_secs(300)),
        auth_rejection_time: Duration::from_millis(500),
        ..Default::default()
    });
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let _task = tokio::spawn(async move {
        let slots = Arc::new(tokio::sync::Semaphore::new(256));
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                _ = stopped.changed() => break,
                Some(_) = tasks.join_next(), if !tasks.is_empty() => (),
                accepted = socket.accept() => match accepted {
                    Ok((stream, peer)) => {
                        let Ok(slot) = slots.clone().try_acquire_owned() else { continue };
                        let (st, config, stopped) = (st.clone(), config.clone(), stopped.clone());
                        tasks.spawn(serve_connection(st, config, stream, peer, stopped, slot));
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
    config: Arc<russh::server::Config>,
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    mut stopped: tokio::sync::watch::Receiver<bool>,
    _slot: tokio::sync::OwnedSemaphorePermit,
) {
    let authenticated = Arc::new(tokio::sync::Notify::new());
    let connection = Connection { st, peer, login: None, channels: HashMap::new(), permit: None, authenticated: authenticated.clone(), channel_used: false };
    let running = tokio::time::timeout(Duration::from_secs(20), russh::server::run_stream(config, stream, connection)).await;
    let Ok(Ok(mut running)) = running else { return };
    let handle = running.handle();
    let completed = tokio::select! {
        _ = &mut running => true,
        _ = stopped.changed() => false,
        _ = tokio::time::sleep(Duration::from_secs(30)) => false,
        _ = authenticated.notified() => {
            tokio::select! { _ = &mut running => true, _ = stopped.changed() => false }
        }
    };
    if !completed {
        let _ = handle.disconnect(russh::Disconnect::ByApplication, "Session ended".into(), "en".into()).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), running).await;
    }
}

struct Connection {
    st: AppState,
    peer: SocketAddr,
    login: Option<Login>,
    channels: HashMap<ChannelId, Channel<Msg>>,
    permit: Option<Arc<Permit>>,
    authenticated: Arc<tokio::sync::Notify>,
    channel_used: bool,
}

impl Connection {
    async fn accept(&mut self, login: Login) -> Auth {
        let user = match login.user(&self.st, self.peer, false).await {
            Ok(u) => u,
            Err(_) => return Auth::Reject { proceed_with_methods: None, partial_success: false },
        };
        let mut sessions = self.st.part::<Memory>().sessions.lock();
        let count = sessions.entry(user.id).or_default();
        if *count >= 5 {
            return Auth::Reject { proceed_with_methods: None, partial_success: false };
        }
        *count += 1;
        self.permit = Some(Arc::new(Permit { st: self.st.clone(), user: user.id }));
        self.login = Some(login);
        self.authenticated.notify_one();
        Auth::Accept
    }
}

impl russh::server::Handler for Connection {
    type Error = russh::Error;
    async fn auth_password(&mut self, username: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(self.accept(Login::Password { username: username.to_string(), token: password.to_string() }).await)
    }
    async fn auth_publickey(&mut self, username: &str, public: &PublicKey) -> Result<Auth, Self::Error> {
        let ip = self.peer.ip().to_string();
        let key = format!("ip:{}", auth::limit_key_ip(&ip));
        if auth::attempts_exhausted(&self.st, &key, auth::IP_FAIL_LIMIT) {
            return Ok(Auth::Reject { proceed_with_methods: None, partial_success: false });
        }
        let key_id: Option<(String,)> = sqlx::query_as(
            "SELECT k.id FROM sftp_keys k JOIN users u ON u.id = k.user_id WHERE u.username = ? COLLATE NOCASE AND k.fingerprint = ? AND u.disabled = 0",
        )
        .bind(username)
        .bind(public.fingerprint(HashAlg::Sha256).to_string())
        .fetch_optional(&self.st.db)
        .await
        .unwrap_or(None);
        let answer = if let Some((key_id,)) = key_id {
            self.accept(Login::Key { username: username.to_string(), key_id }).await
        } else {
            Auth::Reject { proceed_with_methods: None, partial_success: false }
        };
        if !matches!(answer, Auth::Accept) {
            auth::begin_attempt(&self.st, &key, auth::IP_FAIL_LIMIT);
        }
        Ok(answer)
    }
    async fn channel_open_session(&mut self, channel: Channel<Msg>, reply: ChannelOpenHandle, _: &mut Session) -> Result<(), Self::Error> {
        if !self.channel_used && self.login.is_some() {
            self.channel_used = true;
            self.channels.insert(channel.id(), channel);
            reply.accept().await;
        } else {
            reply.reject(russh::ChannelOpenFailure::AdministrativelyProhibited).await;
        }
        Ok(())
    }
    async fn subsystem_request(&mut self, channel: ChannelId, name: &str, ssh: &mut Session) -> Result<(), Self::Error> {
        if name == "sftp"
            && let Some(ch) = self.channels.remove(&channel)
            && let (Some(login), Some(permit)) = (self.login.clone(), self.permit.clone())
        {
            ssh.channel_success(channel)?;
            russh_sftp::server::run_with_config(
                ch.into_stream(),
                session::Files::new(self.st.clone(), self.peer, login, permit),
                russh_sftp::server::Config { max_client_packet_len: 512 * 1024 },
            )
            .await;
        } else {
            ssh.channel_failure(channel)?;
        }
        Ok(())
    }
    async fn shell_request(&mut self, channel: ChannelId, ssh: &mut Session) -> Result<(), Self::Error> {
        ssh.channel_failure(channel)?;
        Ok(())
    }
    async fn exec_request(&mut self, channel: ChannelId, _: &[u8], ssh: &mut Session) -> Result<(), Self::Error> {
        ssh.channel_failure(channel)?;
        Ok(())
    }
    async fn channel_eof(&mut self, channel: ChannelId, ssh: &mut Session) -> Result<(), Self::Error> {
        ssh.close(channel)?;
        Ok(())
    }
}
