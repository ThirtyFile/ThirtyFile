//! SFTP storage location: stores files on a Linux host or NAS over SSH.
//!
//! - One SSH connection and one SFTP session are shared by all requests (SFTP can handle multiple requests at once); after a disconnect it reconnects automatically on next use
//! - Host key: the fingerprint (SHA256) is recorded on the first connection, and later mismatches are refused, preventing man-in-the-middle attacks
//! - Writes go to a temporary name first and are then renamed, so a mid-transfer disconnect never leaves an incomplete file that's mistaken for the correct content

use std::{
    collections::HashSet,
    io::{self, SeekFrom},
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex as StdMutex},
    task::{Context, Poll},
    time::Duration,
};

use futures_util::future::BoxFuture;
use russh::{
    client,
    keys::{self, HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate},
};
use russh_sftp::{
    client::{SftpSession, error::Error as SftpError},
    protocol::StatusCode,
};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, ReadBuf},
    sync::Mutex,
};

use crate::storage::{BoxReader, Storage, StorageError, UNAVAILABLE, valid_hash};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum time to wait for a response to a single SFTP request (seconds)
const REQUEST_TIMEOUT: u64 = 20;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SftpConfig {
    #[serde(default)]
    pub host: String,
    /// 0 = 22
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    /// Either a password or a private key
    #[serde(default)]
    pub password: String,
    /// Private key content in OpenSSH or PEM format
    #[serde(default)]
    pub private_key: String,
    #[serde(default)]
    pub key_passphrase: String,
    /// Folder to store files in (blank = the default folder after signing in)
    #[serde(default)]
    pub path: String,
    /// Host key fingerprint (SHA256:…); recorded automatically on the first connection
    #[serde(default)]
    pub host_key: String,
}

impl SftpConfig {
    pub fn port(&self) -> u16 {
        if self.port == 0 { 22 } else { self.port }
    }
}

fn unavailable(detail: impl std::fmt::Display) -> io::Error {
    io::Error::other(StorageError { message: UNAVAILABLE, detail: detail.to_string() })
}

fn denied(message: &'static str, detail: impl std::fmt::Display) -> io::Error {
    io::Error::other(StorageError { message, detail: detail.to_string() })
}

fn sftp_err(e: SftpError) -> io::Error {
    match &e {
        SftpError::Status(s) if s.status_code == StatusCode::NoSuchFile => io::Error::new(io::ErrorKind::NotFound, e.to_string()),
        SftpError::Status(s) if s.status_code == StatusCode::PermissionDenied => {
            denied("The storage service denied access. Make sure the account has read and write permission for this folder.", e)
        }
        _ => unavailable(e),
    }
}

fn is_not_found(e: &SftpError) -> bool {
    matches!(e, SftpError::Status(s) if s.status_code == StatusCode::NoSuchFile)
}

/// SHA256 fingerprint of the host key (the same as shown by `ssh-keygen -lf`)
fn fingerprint(key: &PublicKeyOrCertificate) -> String {
    match key {
        PublicKeyOrCertificate::PublicKey { key, .. } => key.fingerprint(HashAlg::Sha256).to_string(),
        PublicKeyOrCertificate::Certificate(c) => c.public_key().fingerprint(HashAlg::Sha256).to_string(),
    }
}

struct Handler {
    expected: Option<String>,
    seen: Arc<StdMutex<Option<String>>>,
}

impl client::Handler for Handler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let fp = fingerprint(key);
        let ok = self.expected.as_deref().is_none_or(|e| e == fp);
        *self.seen.lock().unwrap() = Some(fp);
        Ok(ok)
    }
}

struct Conn {
    handle: client::Handle<Handler>,
    sftp: SftpSession,
}

pub struct SftpStorage {
    cfg: SftpConfig,
    root: String,
    conn: Mutex<Option<Arc<Conn>>>,
    /// Host key fingerprint the server presented on the most recent connection
    seen_key: Arc<StdMutex<Option<String>>>,
    /// Folders confirmed to exist
    dirs: StdMutex<HashSet<String>>,
}

impl SftpStorage {
    pub fn new(cfg: &SftpConfig) -> io::Result<Self> {
        if cfg.host.trim().is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Enter a host"));
        }
        if cfg.username.trim().is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Enter a username"));
        }
        if cfg.password.is_empty() && cfg.private_key.trim().is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Enter a password or private key"));
        }
        let root = cfg.path.trim().trim_end_matches('/');
        let root = if root.is_empty() { ".".to_string() } else { root.to_string() };
        Ok(Self { cfg: cfg.clone(), root, conn: Mutex::new(None), seen_key: Default::default(), dirs: Default::default() })
    }

    /// Host key fingerprint seen on the most recent connection (recorded when a location is added)
    pub fn host_key(&self) -> Option<String> {
        self.seen_key.lock().unwrap().clone()
    }

    async fn conn(&self) -> io::Result<Arc<Conn>> {
        let mut slot = self.conn.lock().await;
        if let Some(c) = slot.as_ref()
            && !c.handle.is_closed()
        {
            return Ok(c.clone());
        }
        *slot = None;
        let c = Arc::new(self.connect().await?);
        *slot = Some(c.clone());
        Ok(c)
    }

    /// Drops the connection when a transfer fails, reconnecting next time
    async fn reset(&self) {
        *self.conn.lock().await = None;
    }

    async fn connect(&self) -> io::Result<Conn> {
        let cfg = &self.cfg;
        let expected = Some(cfg.host_key.trim().to_string()).filter(|k| !k.is_empty());
        let handler = Handler { expected: expected.clone(), seen: self.seen_key.clone() };
        let config = client::Config {
            inactivity_timeout: Some(Duration::from_secs(600)),
            keepalive_interval: Some(Duration::from_secs(30)),
            ..Default::default()
        };
        let addr = (cfg.host.trim().to_string(), cfg.port());
        let mut handle = match tokio::time::timeout(CONNECT_TIMEOUT, client::connect(Arc::new(config), addr, handler)).await {
            Err(_) => return Err(unavailable("Timed out")),
            Ok(Err(e)) => {
                let seen = self.host_key();
                if let (Some(exp), Some(seen)) = (&expected, &seen)
                    && exp != seen
                {
                    return Err(denied(
                        "The host key doesn't match the one recorded earlier, so the connection was refused (the host may have been reinstalled, or the connection may have been intercepted). If this is expected, reset the host key in the storage location settings.",
                        format!("Recorded {exp}, current {seen}"),
                    ));
                }
                return Err(unavailable(e));
            }
            Ok(Ok(h)) => h,
        };
        let authed = if !cfg.private_key.trim().is_empty() {
            let pass = Some(cfg.key_passphrase.as_str()).filter(|p| !p.is_empty());
            let key = keys::decode_secret_key(cfg.private_key.trim(), pass)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("Couldn't read the private key (invalid format or wrong passphrase): {e}")))?;
            let hash = handle.best_supported_rsa_hash().await.ok().flatten().flatten();
            handle.authenticate_publickey(cfg.username.trim(), PrivateKeyWithHashAlg::new(Arc::new(key), hash)).await
        } else {
            handle.authenticate_password(cfg.username.trim(), cfg.password.as_str()).await
        }
        .map_err(unavailable)?;
        if !authed.success() {
            return Err(denied("Incorrect username, password, or private key. Couldn't sign in to SFTP.", "authentication failed"));
        }
        let channel = handle.channel_open_session().await.map_err(unavailable)?;
        channel.request_subsystem(true, "sftp").await.map_err(unavailable)?;
        let sftp = SftpSession::new(channel.into_stream()).await.map_err(sftp_err)?;
        sftp.set_timeout(REQUEST_TIMEOUT);
        Ok(Conn { handle, sftp })
    }

    fn blob_dir(&self, hash: &str) -> String {
        format!("{}/blobs/{}/{}", self.root, &hash[0..2], &hash[2..4])
    }

    fn blob_path(&self, hash: &str) -> io::Result<String> {
        valid_hash(hash)?;
        Ok(format!("{}/{hash}", self.blob_dir(hash)))
    }

    /// Creates folders level by level (skipping existing ones)
    async fn ensure_dir(&self, conn: &Conn, dir: &str) -> io::Result<()> {
        if self.dirs.lock().unwrap().contains(dir) {
            return Ok(());
        }
        let mut path = String::new();
        for part in dir.split('/') {
            if part.is_empty() {
                path.push('/');
                continue;
            }
            if !path.is_empty() && !path.ends_with('/') {
                path.push('/');
            }
            path.push_str(part);
            if part == "." || self.dirs.lock().unwrap().contains(&path) {
                continue;
            }
            if !conn.sftp.try_exists(path.as_str()).await.map_err(sftp_err)? {
                // With concurrent uploads another request may have created it first
                if let Err(e) = conn.sftp.create_dir(path.as_str()).await
                    && !conn.sftp.try_exists(path.as_str()).await.unwrap_or(false)
                {
                    return Err(sftp_err(e));
                }
            }
            self.dirs.lock().unwrap().insert(path.clone());
        }
        Ok(())
    }

    async fn put(&self, hash: &str, src: &Path) -> io::Result<()> {
        let conn = self.conn().await?;
        let dir = self.blob_dir(hash);
        self.ensure_dir(&conn, &dir).await?;
        let path = self.blob_path(hash)?;
        let tmp = format!("{path}.part-{}", uuid::Uuid::new_v4().simple());
        let mut input = tokio::fs::File::open(src).await?;
        let mut out = conn.sftp.create(tmp.as_str()).await.map_err(sftp_err)?;
        let written: io::Result<()> = async {
            tokio::io::copy(&mut input, &mut out).await.map_err(unavailable)?;
            out.shutdown().await.map_err(unavailable)?;
            Ok(())
        }
        .await;
        drop(out);
        if let Err(e) = written {
            let _ = conn.sftp.remove_file(tmp.as_str()).await;
            return Err(e);
        }
        // SFTP v3 rename doesn't overwrite existing files: if the same content already exists, keep the original
        if let Err(e) = conn.sftp.rename(tmp.as_str(), path.as_str()).await {
            let exists = conn.sftp.try_exists(path.as_str()).await.unwrap_or(false);
            let _ = conn.sftp.remove_file(tmp.as_str()).await;
            if !exists {
                return Err(sftp_err(e));
            }
        }
        Ok(())
    }
}

/// Keeps the connection alive while reading (the SSH connection closes when the connection object is dropped)
struct Held<R> {
    _conn: Arc<Conn>,
    inner: R,
}

impl<R: AsyncRead + Unpin> AsyncRead for Held<R> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl Storage for SftpStorage {
    fn size<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, io::Result<Option<u64>>> {
        Box::pin(async move {
            let path = self.blob_path(hash)?;
            let conn = self.conn().await?;
            match conn.sftp.metadata(path.as_str()).await.map_err(sftp_err) {
                Ok(m) => Ok(Some(m.len())),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            }
        })
    }

    fn list(&self) -> BoxFuture<'_, io::Result<Vec<String>>> {
        Box::pin(async move {
            let conn = self.conn().await?;
            let names = |path: String| {
                let conn = conn.clone();
                async move {
                    match conn.sftp.read_dir(path.as_str()).await.map_err(sftp_err) {
                        Ok(dir) => Ok(dir.map(|e| e.file_name()).collect::<Vec<_>>()),
                        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
                        Err(e) => Err(e),
                    }
                }
            };
            // blobs/ab/cd/<hash>
            let base = format!("{}/blobs", self.root);
            let mut out = Vec::new();
            for a in names(base.clone()).await?.into_iter().filter(|n| n.len() == 2) {
                for b in names(format!("{base}/{a}")).await?.into_iter().filter(|n| n.len() == 2) {
                    out.extend(names(format!("{base}/{a}/{b}")).await?.into_iter().filter(|n| crate::storage::is_hash(n)));
                }
            }
            Ok(out)
        })
    }

    fn put_file<'a>(&'a self, hash: &'a str, src: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let res = self.put(hash, src).await;
            if res.as_ref().is_err_and(|e| e.kind() != io::ErrorKind::InvalidInput) {
                self.reset().await;
            }
            res?;
            let _ = tokio::fs::remove_file(src).await;
            Ok(())
        })
    }

    fn open<'a>(&'a self, hash: &'a str, start: u64, len: u64) -> BoxFuture<'a, io::Result<BoxReader>> {
        Box::pin(async move {
            if len == 0 {
                return Ok(Box::pin(tokio::io::empty()) as BoxReader);
            }
            let path = self.blob_path(hash)?;
            let conn = self.conn().await?;
            let opened = async {
                let mut f = conn.sftp.open(path.as_str()).await.map_err(sftp_err)?;
                if start > 0 {
                    f.seek(SeekFrom::Start(start)).await.map_err(unavailable)?;
                }
                Ok::<_, io::Error>(f)
            }
            .await;
            match opened {
                Ok(f) => Ok(Box::pin(Held { _conn: conn, inner: f.take(len) }) as BoxReader),
                Err(e) => {
                    if e.kind() != io::ErrorKind::NotFound {
                        self.reset().await;
                    }
                    Err(e)
                }
            }
        })
    }

    fn delete<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let path = self.blob_path(hash)?;
            let conn = self.conn().await?;
            match conn.sftp.remove_file(path.as_str()).await {
                Ok(()) => Ok(()),
                Err(e) if is_not_found(&e) => Ok(()),
                Err(e) => {
                    self.reset().await;
                    Err(sftp_err(e))
                }
            }
        })
    }

    fn check(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            let conn = self.conn().await?;
            self.ensure_dir(&conn, &self.root).await?;
            let probe = format!("{}/.thirtyfile-check-{}", self.root, uuid::Uuid::new_v4().simple());
            let result: io::Result<()> = async {
                let mut f = conn.sftp.create(probe.as_str()).await.map_err(sftp_err)?;
                f.write_all(b"ok").await.map_err(unavailable)?;
                f.shutdown().await.map_err(unavailable)?;
                drop(f);
                let back = conn.sftp.read(probe.as_str()).await.map_err(sftp_err)?;
                if back != b"ok" {
                    return Err(io::Error::other("The content read back didn't match"));
                }
                Ok(())
            }
            .await;
            let _ = conn.sftp.remove_file(probe.as_str()).await;
            if result.is_err() {
                self.reset().await;
            }
            result
        })
    }

    fn ping(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            let conn = self.conn().await?;
            if let Err(e) = conn.sftp.canonicalize(self.root.as_str()).await {
                self.reset().await;
                return Err(sftp_err(e));
            }
            Ok(())
        })
    }

    fn host_key(&self) -> Option<String> {
        SftpStorage::host_key(self)
    }
}
