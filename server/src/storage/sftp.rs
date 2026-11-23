//! SFTP storage location: stores files on a Linux host or NAS over SSH.
//!
//! - One SSH connection and one SFTP session are shared by all requests (SFTP can handle multiple requests at once); after a disconnect it reconnects automatically on next use
//! - Host key: the fingerprint (SHA256) is recorded on the first connection, and later mismatches are refused, preventing man-in-the-middle attacks
//! - Writes go to a temporary name first and are then renamed, so a mid-transfer disconnect never leaves an incomplete file that's mistaken for the correct content

use std::{
    collections::HashSet,
    future::Future,
    io::{self, SeekFrom},
    path::Path,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use crate::sync::Mutex as StdMutex;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use thirtysftp::{
    Error as SftpError,
    client::{Config as SftpClientConfig, Dir, File, Session as SftpSession, fs as sftp_fs},
    ssh as sftp_ssh,
    wire::{Attributes, StatusCode},
};
use thirtyssh::{
    client,
    keys::{Algorithm, Credential, HashAlg, PrivateKeyReader, PublicKey},
    session,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, ReadBuf},
    sync::{Mutex, oneshot},
};

use crate::storage::{BoxReader, Entry, EntryKind, Storage, StorageError, UNAVAILABLE, valid_hash};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum time to wait for a response to a single SFTP request. Transfers have no overall limit: a connection that
/// stops answering is dropped by the keepalive probes instead.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

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
    match e.status_code() {
        Some(StatusCode::NoSuchFile) => io::Error::new(io::ErrorKind::NotFound, e.to_string()),
        Some(StatusCode::PermissionDenied) => denied("The storage service denied access. Make sure the account has read and write permission for this folder.", e),
        _ => unavailable(e),
    }
}

fn is_not_found(e: &SftpError) -> bool {
    e.status_code() == Some(StatusCode::NoSuchFile)
}

/// Gives a single SFTP request a time limit
async fn limited<T>(request: impl Future<Output = Result<T, SftpError>>) -> Result<T, SftpError> {
    match tokio::time::timeout(REQUEST_TIMEOUT, request).await {
        Ok(result) => result,
        Err(_) => Err(SftpError::Io(Arc::new(io::Error::new(io::ErrorKind::TimedOut, "The request timed out")))),
    }
}

/// SHA256 fingerprint of the host key (the same as shown by `ssh-keygen -lf`)
fn fingerprint(key: &Credential) -> String {
    PublicKey::from(key.key_data().clone()).fingerprint(HashAlg::Sha256).to_string()
}

struct Handler {
    expected: Option<String>,
    seen: Arc<StdMutex<Option<String>>>,
}

impl session::Handler for Handler {}

impl client::Handler for Handler {
    async fn verify_host(&mut self, host: &Credential, _: &Algorithm) -> io::Result<bool> {
        let fp = fingerprint(host);
        let ok = self.expected.as_deref().is_none_or(|e| e == fp);
        *self.seen.lock() = Some(fp);
        Ok(ok)
    }
}

/// The SFTP session, and the SSH connection under it: dropping it ends the connection
struct Conn {
    sftp: SftpSession,
    /// Set when the SSH connection has ended
    closed: Arc<AtomicBool>,
    _stop: oneshot::Sender<()>,
}

impl Conn {
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }
}

pub struct SftpStorage {
    cfg: SftpConfig,
    root: String,
    conn: Mutex<Option<Arc<Conn>>>,
    /// Host key fingerprint the server presented on the most recent connection
    seen_key: Arc<StdMutex<Option<String>>>,
    /// Without a recorded key (a location just added), the key of the first connection that signed in: every later
    /// connection of this storage must show it too, as it will once the recorded key is read at the next start
    first_key: StdMutex<Option<String>>,
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
        Ok(Self { cfg: cfg.clone(), root, conn: Mutex::new(None), seen_key: Default::default(), first_key: Default::default(), dirs: Default::default() })
    }

    /// Host key fingerprint seen on the most recent connection (recorded when a location is added)
    pub fn host_key(&self) -> Option<String> {
        self.seen_key.lock().clone()
    }

    async fn conn(&self) -> io::Result<Arc<Conn>> {
        let mut slot = self.conn.lock().await;
        if let Some(c) = slot.as_ref()
            && !c.is_closed()
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
        let expected = Some(cfg.host_key.trim().to_string()).filter(|k| !k.is_empty()).or_else(|| self.first_key.lock().clone());
        let handler = Handler { expected: expected.clone(), seen: self.seen_key.clone() };
        let config = client::Config { inactivity_timeout: Some(Duration::from_secs(600)), keepalive_interval: Some(Duration::from_secs(30)), ..Default::default() };
        let addr = (cfg.host.trim().to_string(), cfg.port());
        let mut ssh = match tokio::time::timeout(CONNECT_TIMEOUT, client::connect(Arc::new(config), addr, handler)).await {
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
            Ok(Ok(c)) => c,
        };
        let outcome = if !cfg.private_key.trim().is_empty() {
            let pass = Some(cfg.key_passphrase.as_str()).filter(|p| !p.is_empty());
            let key = PrivateKeyReader::new()
                .passphrase(pass)
                .text(cfg.private_key.trim())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("Couldn't read the private key (invalid format or wrong passphrase): {e}")))?;
            ssh.authenticate(cfg.username.trim(), &key).await
        } else {
            ssh.authenticate(cfg.username.trim(), client::Method::Password(cfg.password.as_str())).await
        }
        .map_err(unavailable)?;
        if !outcome.is_accepted() {
            return Err(denied("Incorrect username, password, or private key. Couldn't sign in to SFTP.", "authentication failed"));
        }
        if expected.is_none() {
            *self.first_key.lock() = self.host_key();
        }
        let running = ssh.start().map_err(unavailable)?;
        let handle = running.handle();
        // The connection ends when `Conn` is dropped (its sender goes away) or when the server ends it
        let closed = Arc::new(AtomicBool::new(false));
        let (stop, stopped) = oneshot::channel::<()>();
        let done = closed.clone();
        tokio::spawn(async move {
            let mut running = running;
            tokio::select! {
                _ = &mut running => {}
                _ = stopped => {
                    let _ = running.disconnect().await;
                }
            }
            done.store(true, Ordering::Relaxed);
        });
        let sftp = match tokio::time::timeout(CONNECT_TIMEOUT, sftp_ssh::connect(&handle, SftpClientConfig::default())).await {
            Err(_) => return Err(unavailable("Timed out")),
            Ok(Err(e)) => return Err(sftp_err(e)),
            Ok(Ok(sftp)) => sftp,
        };
        Ok(Conn { sftp, closed, _stop: stop })
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
        if self.dirs.lock().contains(dir) {
            return Ok(());
        }
        for path in crate::storage::dir_levels(dir) {
            if self.dirs.lock().contains(&path) {
                continue;
            }
            if !limited(sftp_fs::try_exists(&conn.sftp, path.as_str())).await.map_err(sftp_err)? {
                // With concurrent uploads another request may have created it first
                if let Err(e) = limited(sftp_fs::create_dir(&conn.sftp, path.as_str())).await
                    && !limited(sftp_fs::try_exists(&conn.sftp, path.as_str())).await.unwrap_or(false)
                {
                    return Err(sftp_err(e));
                }
            }
            self.dirs.lock().insert(path);
        }
        Ok(())
    }

    async fn put(&self, hash: &str, src: &Path) -> io::Result<()> {
        self.put_to(&self.blob_dir(hash), &self.blob_path(hash)?, src, false).await
    }

    /// A path in the location (checked; "" is its folder)
    fn path_at(&self, key: &str) -> io::Result<String> {
        crate::storage::key_parts(key)?;
        Ok(if key.is_empty() { self.root.clone() } else { format!("{}/{key}", self.root) })
    }

    /// Refuses a symbolic link in `key` below the location's folder: in every folder on the way, and in the last part
    /// too when `last` is set. A link may point anywhere on that server, such as into a folder the location doesn't
    /// own.
    async fn no_links(&self, conn: &Conn, key: &str, last: bool) -> io::Result<()> {
        let parts = crate::storage::key_parts(key)?;
        let n = if last { parts.len() } else { parts.len().saturating_sub(1) };
        for i in 1..=n {
            let path = format!("{}/{}", self.root, parts[..i].join("/"));
            match limited(sftp_fs::symlink_metadata(&conn.sftp, path.as_str())).await {
                Ok(m) if m.is_symlink() => return Err(io::Error::new(io::ErrorKind::InvalidInput, "a symbolic link")),
                Ok(_) => {}
                // Nothing there: what comes next says so
                Err(e) if is_not_found(&e) => return Ok(()),
                Err(e) => return Err(sftp_err(e)),
            }
        }
        Ok(())
    }

    async fn open_path(&self, path: String, start: u64, len: u64) -> io::Result<BoxReader> {
        if len == 0 {
            return Ok(Box::pin(tokio::io::empty()) as BoxReader);
        }
        let conn = self.conn().await?;
        let opened = async {
            let mut f = limited(File::open(&conn.sftp, path.as_str())).await.map_err(sftp_err)?;
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
    }

    async fn delete_path(&self, path: &str) -> io::Result<()> {
        let conn = self.conn().await?;
        match limited(sftp_fs::remove_file(&conn.sftp, path)).await {
            Ok(()) => Ok(()),
            Err(e) if is_not_found(&e) => Ok(()),
            Err(e) => {
                self.reset().await;
                Err(sftp_err(e))
            }
        }
    }

    /// Writes a temp file to `path` in the folder `dir`, through a temporary name
    async fn put_to(&self, dir: &str, path: &str, src: &Path, repair: bool) -> io::Result<()> {
        let conn = self.conn().await?;
        self.ensure_dir(&conn, dir).await?;
        let tmp = format!("{path}.part-{}", uuid::Uuid::new_v4().simple());
        let mut input = tokio::fs::File::open(src).await?;
        let mut out = limited(File::create(&conn.sftp, tmp.as_str())).await.map_err(sftp_err)?;
        let written: io::Result<()> = async {
            tokio::io::copy(&mut input, &mut out).await.map_err(unavailable)?;
            out.shutdown().await.map_err(unavailable)?;
            Ok(())
        }
        .await;
        drop(out);
        if let Err(e) = written {
            let _ = limited(sftp_fs::remove_file(&conn.sftp, tmp.as_str())).await;
            return Err(e);
        }
        // SFTP v3 rename doesn't overwrite existing files. Ordinary writes keep the original, but a repair
        // replaces it only after the complete replacement has reached its temporary name.
        // (the plain request, not the replacing `posix-rename`)
        if let Err(e) = limited(conn.sftp.rename(tmp.as_str(), path)).await {
            let exists = limited(sftp_fs::try_exists(&conn.sftp, path)).await.unwrap_or(false);
            let result = if repair && exists {
                async {
                    limited(sftp_fs::remove_file(&conn.sftp, path)).await.map_err(sftp_err)?;
                    limited(conn.sftp.rename(tmp.as_str(), path)).await.map_err(sftp_err)
                }
                .await
            } else if exists {
                Ok(())
            } else {
                Err(sftp_err(e))
            };
            let _ = limited(sftp_fs::remove_file(&conn.sftp, tmp.as_str())).await;
            result?;
        }
        Ok(())
    }
}

fn sftp_entry(name: String, m: &Attributes) -> Entry {
    let kind = if m.is_symlink() {
        EntryKind::Link
    } else if m.is_dir() {
        EntryKind::Folder
    } else {
        EntryKind::File
    };
    Entry { name, kind, size: if kind == EntryKind::File { m.size.unwrap_or(0) } else { 0 }, modified: m.times.map(|t| i64::from(t.modified)) }
}

/// The names (and attributes) in a folder, without `.` and `..`
async fn read_names(sftp: &SftpSession, path: &str) -> Result<Vec<(String, Attributes)>, SftpError> {
    let mut dir = limited(Dir::open(sftp, path)).await?;
    let mut out = Vec::new();
    while let Some(entry) = limited(dir.next_entry()).await? {
        let name = String::from_utf8_lossy(&entry.name).into_owned();
        if name != "." && name != ".." {
            out.push((name, entry.attrs));
        }
    }
    let _ = limited(dir.close()).await;
    Ok(out)
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
            match limited(sftp_fs::metadata(&conn.sftp, path.as_str())).await.map_err(sftp_err) {
                Ok(m) => Ok(Some(m.size.unwrap_or(0))),
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
                    match read_names(&conn.sftp, path.as_str()).await.map_err(sftp_err) {
                        Ok(dir) => Ok(dir.into_iter().map(|(name, _)| name).collect::<Vec<_>>()),
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
        Box::pin(async move { self.open_path(self.blob_path(hash)?, start, len).await })
    }

    fn delete<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move { self.delete_path(&self.blob_path(hash)?).await })
    }

    fn list_dir<'a>(&'a self, dir: &'a str) -> BoxFuture<'a, io::Result<Vec<Entry>>> {
        Box::pin(async move {
            let path = self.path_at(dir)?;
            let conn = self.conn().await?;
            self.no_links(&conn, dir, true).await?;
            let listed = read_names(&conn.sftp, path.as_str()).await.map_err(sftp_err)?;
            Ok(listed.into_iter().map(|(name, attrs)| sftp_entry(name, &attrs)).collect())
        })
    }

    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, io::Result<Option<Entry>>> {
        Box::pin(async move {
            let path = self.path_at(key)?;
            let conn = self.conn().await?;
            self.no_links(&conn, key, false).await?;
            // The item itself: a link isn't followed
            match limited(sftp_fs::symlink_metadata(&conn.sftp, path.as_str())).await {
                Ok(m) => Ok(Some(sftp_entry(key.rsplit('/').next().unwrap_or_default().to_string(), &m))),
                Err(e) if is_not_found(&e) => Ok(None),
                Err(e) => Err(sftp_err(e)),
            }
        })
    }

    fn repair_file<'a>(&'a self, hash: &'a str, src: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let path = self.blob_path(hash)?;
            let res = self.put_to(&self.blob_dir(hash), &path, src, true).await;
            if res.as_ref().is_err_and(|e| e.kind() != io::ErrorKind::InvalidInput) {
                self.reset().await;
            }
            res?;
            let _ = tokio::fs::remove_file(src).await;
            Ok(())
        })
    }

    fn put_at<'a>(&'a self, key: &'a str, src: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let path = self.path_at(key)?;
            let dir = path.rsplit_once('/').map_or(self.root.as_str(), |(d, _)| d).to_string();
            let res = self.put_to(&dir, &path, src, false).await;
            if res.as_ref().is_err_and(|e| e.kind() != io::ErrorKind::InvalidInput) {
                self.reset().await;
            }
            res?;
            let _ = tokio::fs::remove_file(src).await;
            Ok(())
        })
    }

    fn open_at<'a>(&'a self, key: &'a str, start: u64, len: u64) -> BoxFuture<'a, io::Result<BoxReader>> {
        Box::pin(async move {
            let path = self.path_at(key)?;
            let conn = self.conn().await?;
            // A link may point anywhere on that server, on the way or at the end: refused
            self.no_links(&conn, key, true).await?;
            limited(sftp_fs::symlink_metadata(&conn.sftp, path.as_str())).await.map_err(sftp_err)?;
            self.open_path(path, start, len).await
        })
    }

    fn delete_at<'a>(&'a self, key: &'a str) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move { self.delete_path(&self.path_at(key)?).await })
    }

    fn check(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            let conn = self.conn().await?;
            self.ensure_dir(&conn, &self.root).await?;
            let probe = format!("{}/.thirtyfile-check-{}", self.root, uuid::Uuid::new_v4().simple());
            let result: io::Result<()> = async {
                let mut f = limited(File::create(&conn.sftp, probe.as_str())).await.map_err(sftp_err)?;
                f.write_all(b"ok").await.map_err(unavailable)?;
                f.shutdown().await.map_err(unavailable)?;
                drop(f);
                let back = limited(sftp_fs::read(&conn.sftp, probe.as_str())).await.map_err(sftp_err)?;
                if back != b"ok" {
                    return Err(io::Error::other("The content read back didn't match"));
                }
                Ok(())
            }
            .await;
            // An account that can write but not delete would pass and fail later, when files are deleted
            let deleted = limited(sftp_fs::remove_file(&conn.sftp, probe.as_str())).await.map_err(sftp_err);
            let result = result.and_then(|()| deleted.map_err(|e| crate::storage::delete_failed(e, crate::storage::CANT_DELETE_ACCOUNT)));
            if result.is_err() {
                self.reset().await;
            }
            result
        })
    }

    fn ping(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            let conn = self.conn().await?;
            if let Err(e) = limited(sftp_fs::canonicalize(&conn.sftp, self.root.as_str())).await {
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

#[cfg(test)]
pub(crate) mod tests {
    use std::path::PathBuf;

    use thirtysftp::server::{LocalFs, ServerConfig};
    use thirtyssh::{keys, server, transport::HostKey};

    use super::*;
    use crate::testutil;

    /// One SSH connection: a password, and the SFTP subsystem over the test folder (a rename doesn't replace an
    /// existing file, like on SFTP v3 servers)
    #[derive(Clone)]
    struct Ssh {
        files: Arc<LocalFs>,
        password: &'static str,
    }

    impl server::AuthPolicy for Ssh {
        async fn password(&mut self, _: &str, password: &[u8]) -> io::Result<bool> {
            Ok(password == self.password.as_bytes())
        }
    }

    impl session::Handler for Ssh {
        async fn channel_open(&mut self, request: session::IncomingChannel, _: &session::Context) -> io::Result<()> {
            thirtysftp::ssh::accept_session(request, self.files.clone(), ServerConfig::default()).await
        }
    }

    struct Factory(Ssh);

    impl server::Connections for Factory {
        type Handler = Ssh;

        fn handler(&mut self, _: std::net::SocketAddr) -> Ssh {
            self.0.clone()
        }

        fn failed(&mut self, _: io::Error) {}
    }

    /// An SSH server over a temporary folder, with a new host key
    pub(crate) struct Server {
        pub dir: PathBuf,
        port: u16,
        pub fingerprint: String,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    pub(crate) async fn server(password: &'static str) -> Server {
        server_on(password, 0).await
    }

    /// `server` on a given port (0: any free one)
    async fn server_on(password: &'static str, port: u16) -> Server {
        let dir = std::env::temp_dir().join(format!("thirtyfile-sftp-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("files")).unwrap();
        let host = HostKey::generate().unwrap();
        let fingerprint = keys::public::from_ed25519(&host.public_key()).fingerprint(HashAlg::Sha256).to_string();
        let mut config = server::Config::new(host);
        config.authentication.rejection_delay = Duration::from_millis(10);
        let mut tries = 0;
        let listener = loop {
            // A port just given up may take a moment to be free again
            match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
                Ok(l) => break l,
                Err(_) if tries < 100 => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => panic!("{e}"),
            }
        };
        let port = listener.local_addr().unwrap().port();
        let ssh = Ssh { files: Arc::new(LocalFs::new(&dir).unwrap()), password };
        let listening = server::listen_on(Arc::new(config), listener, Factory(ssh)).unwrap();
        let task = tokio::spawn(async move {
            let _ = listening.await;
        });
        Server { dir, port, fingerprint, task }
    }

    pub(crate) fn storage(s: &Server, password: &str, host_key: &str) -> SftpStorage {
        let cfg = SftpConfig {
            host: "127.0.0.1".into(),
            port: s.port,
            username: "backup".into(),
            password: password.into(),
            path: "/files".into(),
            host_key: host_key.into(),
            ..Default::default()
        };
        SftpStorage::new(&cfg).unwrap()
    }

    async fn read(st: &SftpStorage, hash: &str, start: u64, len: u64) -> Vec<u8> {
        let mut out = Vec::new();
        st.open(hash, start, len).await.unwrap().read_to_end(&mut out).await.unwrap();
        out
    }

    /// Puts content into the storage from a temporary file, as uploads do; returns its hash
    async fn put(st: &SftpStorage, s: &Server, content: &[u8]) -> String {
        let hash = crate::util::sha256_hex(content);
        let src = s.dir.join(format!("src-{hash}"));
        std::fs::write(&src, content).unwrap();
        st.put_file(&hash, &src).await.unwrap();
        assert!(!src.exists(), "the temporary file is removed once stored");
        hash
    }

    #[tokio::test]
    async fn content_is_stored_read_listed_and_deleted_over_sftp() {
        let s = server(testutil::password()).await;
        let st = storage(&s, testutil::password(), "");
        st.check().await.unwrap();
        // The host key seen is recorded, to be checked on later connections
        assert_eq!(st.host_key(), Some(s.fingerprint.clone()));

        let hash = put(&st, &s, b"hello over sftp").await;
        // Written under its hash, two folder levels down, with nothing left under a temporary name
        let stored = s.dir.join(format!("files/blobs/{}/{}/{hash}", &hash[0..2], &hash[2..4]));
        assert_eq!(std::fs::read(&stored).unwrap(), b"hello over sftp");
        // The same content again: the rename is refused, and the file already there is kept
        assert_eq!(put(&st, &s, b"hello over sftp").await, hash);
        assert_eq!(std::fs::read_dir(stored.parent().unwrap()).unwrap().count(), 1);

        assert_eq!(read(&st, &hash, 0, 15).await, b"hello over sftp");
        assert_eq!(read(&st, &hash, 6, 4).await, b"over");
        assert_eq!(read(&st, &hash, 0, 0).await, b"");
        assert_eq!(st.size(&hash).await.unwrap(), Some(15));
        let other = put(&st, &s, b"second").await;
        let mut listed = st.list().await.unwrap();
        listed.sort();
        let mut want = vec![hash.clone(), other];
        want.sort();
        assert_eq!(listed, want);

        st.delete(&hash).await.unwrap();
        assert!(!stored.exists());
        assert_eq!(st.size(&hash).await.unwrap(), None);
        // Deleting what isn't there is fine; reading it says it isn't there
        st.delete(&hash).await.unwrap();
        assert_eq!(st.open(&hash, 0, 15).await.err().unwrap().kind(), io::ErrorKind::NotFound);
        st.ping().await.unwrap();
    }

    #[tokio::test]
    async fn items_are_listed_by_path_and_the_step_test_passes_over_sftp() {
        use crate::storage::EntryKind;
        let s = server(testutil::password()).await;
        let st = Arc::new(storage(&s, testutil::password(), ""));
        let hash = put(&st, &s, b"stored content").await;
        std::fs::write(s.dir.join("files/notes.txt"), b"hello").unwrap();

        let mut top = st.list_dir("").await.unwrap();
        top.sort_by(|a, b| a.name.cmp(&b.name));
        let top: Vec<_> = top.iter().map(|e| (e.name.as_str(), e.kind, e.size)).collect();
        assert_eq!(top, [("blobs", EntryKind::Folder, 0), ("notes.txt", EntryKind::File, 5)]);
        let notes = st.stat("notes.txt").await.unwrap().unwrap();
        assert!(notes.modified.is_some());
        assert!(st.stat("missing.txt").await.unwrap().is_none());
        assert!(st.list_dir("../outside").await.is_err());
        let content = st.list_content(&|_| {}).await.unwrap();
        assert_eq!(content.iter().map(|e| (e.name.as_str(), e.size)).collect::<Vec<_>>(), [(hash.as_str(), 14)]);

        let env = testutil::env().await;
        let report = crate::location_tools::steps::run(&env.st, st.clone(), "sftp", 1 << 20).await;
        assert!(report.ok, "{report:?}");
        assert_eq!(std::fs::read_dir(s.dir.join("files/.thirtyfile-check")).unwrap().count(), 0, "the test files are deleted");
    }

    #[tokio::test]
    async fn damaged_content_is_replaced_when_sftp_rename_refuses_overwrite() {
        let s = server(testutil::password()).await;
        let st = storage(&s, testutil::password(), "");
        let hash = put(&st, &s, b"healthy content").await;
        let stored = s.dir.join(format!("files/blobs/{}/{}/{hash}", &hash[..2], &hash[2..4]));
        std::fs::write(&stored, b"damaged content").unwrap();
        let src = s.dir.join("repair.tmp");
        std::fs::write(&src, b"healthy content").unwrap();
        st.repair_file(&hash, &src).await.unwrap();
        assert_eq!(read(&st, &hash, 0, 15).await, b"healthy content");
        assert!(!src.exists());
        assert_eq!(std::fs::read_dir(stored.parent().unwrap()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn links_on_the_way_are_never_followed_over_sftp() {
        use tokio::io::AsyncReadExt;
        let s = server(testutil::password()).await;
        let st = storage(&s, testutil::password(), "");
        // Outside the location's folder, on the same server
        let outside = s.dir.join("outside");
        std::fs::create_dir_all(outside.join("inner")).unwrap();
        std::fs::write(outside.join("inner/secret.txt"), b"secret").unwrap();
        std::fs::create_dir_all(s.dir.join("files/real")).unwrap();
        std::fs::write(s.dir.join("files/real/ok.txt"), b"ok").unwrap();
        std::os::unix::fs::symlink(&outside, s.dir.join("files/away")).unwrap();

        let mut out = Vec::new();
        st.open_at("real/ok.txt", 0, 2).await.unwrap().read_to_end(&mut out).await.unwrap();
        assert_eq!(out, b"ok");
        assert!(st.list_dir("away").await.is_err());
        assert!(st.list_dir("away/inner").await.is_err(), "a link in the middle of the path");
        assert!(st.stat("away/inner/secret.txt").await.is_err());
        assert!(st.open_at("away/inner/secret.txt", 0, 6).await.is_err());
        let top = st.list_dir("").await.unwrap();
        assert_eq!(top.iter().find(|e| e.name == "away").unwrap().kind, crate::storage::EntryKind::Link);
    }

    /// The message a storage error shows people
    fn message(e: &io::Error) -> &'static str {
        e.get_ref().and_then(|e| e.downcast_ref::<StorageError>()).unwrap().message
    }

    #[tokio::test]
    async fn a_wrong_password_or_another_host_key_is_refused() {
        let s = server(testutil::password()).await;
        let err = storage(&s, &testutil::wrong_password(), "").check().await.unwrap_err();
        assert!(message(&err).starts_with("Incorrect username, password"), "{}", message(&err));

        // The key recorded earlier is accepted; another one means another server (or someone in between)
        storage(&s, testutil::password(), &s.fingerprint).ping().await.unwrap();
        let other = server(testutil::password()).await;
        let err = storage(&s, testutil::password(), &other.fingerprint).ping().await.unwrap_err();
        assert!(message(&err).starts_with("The host key doesn't match"), "{}", message(&err));

        let port = s.port;
        drop(s);
        let cfg = SftpConfig { host: "127.0.0.1".into(), port, username: "backup".into(), password: testutil::wrong_password(), ..Default::default() };
        let err = SftpStorage::new(&cfg).unwrap().ping().await.unwrap_err();
        assert_eq!(message(&err), UNAVAILABLE);
    }

    #[tokio::test]
    async fn the_key_seen_first_is_the_one_every_reconnection_must_show() {
        let s = server(testutil::password()).await;
        // A new location: no key recorded yet, so the first connection records the one it sees
        let st = storage(&s, testutil::password(), "");
        st.check().await.unwrap();
        assert_eq!(st.host_key(), Some(s.fingerprint.clone()));
        // Another server (or someone in between) answers at the same address: the same storage, without a restart,
        // refuses it
        let port = s.port;
        drop(s);
        let other = server_on(testutil::password(), port).await;
        st.reset().await;
        let err = st.ping().await.unwrap_err();
        assert!(message(&err).starts_with("The host key doesn't match"), "{}", message(&err));
        drop(other);
    }
}
