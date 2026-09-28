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
        for path in crate::storage::dir_levels(dir) {
            if self.dirs.lock().unwrap().contains(&path) {
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
            self.dirs.lock().unwrap().insert(path);
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

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        io::{Read, Seek, Write},
        path::PathBuf,
    };

    use russh::{
        Channel, ChannelId,
        server::{Auth, ChannelOpenHandle, Msg, Session},
    };
    use russh_sftp::protocol::{Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, Version};

    use super::*;

    /// The files of an SFTP session, in a folder: only the requests the storage makes. Like SFTP v3 servers, a rename
    /// doesn't replace an existing file.
    struct Files {
        root: PathBuf,
        open: HashMap<String, std::fs::File>,
        listings: HashMap<String, Vec<File>>,
        next: u64,
    }

    fn code(e: io::Error) -> StatusCode {
        match e.kind() {
            io::ErrorKind::NotFound => StatusCode::NoSuchFile,
            io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
            _ => StatusCode::Failure,
        }
    }

    fn ok(id: u32) -> Status {
        Status { id, status_code: StatusCode::Ok, error_message: "Ok".into(), language_tag: "en-US".into() }
    }

    impl Files {
        fn path(&self, p: &str) -> PathBuf {
            self.root.join(p.trim_start_matches('/'))
        }

        fn handle(&mut self) -> String {
            self.next += 1;
            self.next.to_string()
        }

        fn file(&mut self, handle: &str) -> Result<&mut std::fs::File, StatusCode> {
            self.open.get_mut(handle).ok_or(StatusCode::Failure)
        }
    }

    impl russh_sftp::server::Handler for Files {
        type Error = StatusCode;

        fn unimplemented(&self) -> StatusCode {
            StatusCode::OpUnsupported
        }

        async fn init(&mut self, _: u32, _: HashMap<String, String>) -> Result<Version, StatusCode> {
            Ok(Version::new())
        }

        async fn open(&mut self, id: u32, filename: String, pflags: OpenFlags, _: FileAttributes) -> Result<Handle, StatusCode> {
            let f = std::fs::OpenOptions::from(pflags).open(self.path(&filename)).map_err(code)?;
            let handle = self.handle();
            self.open.insert(handle.clone(), f);
            Ok(Handle { id, handle })
        }

        async fn close(&mut self, id: u32, handle: String) -> Result<Status, StatusCode> {
            self.open.remove(&handle);
            self.listings.remove(&handle);
            Ok(ok(id))
        }

        async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<Data, StatusCode> {
            let f = self.file(&handle)?;
            f.seek(SeekFrom::Start(offset)).map_err(code)?;
            let mut data = vec![0; len as usize];
            match f.read(&mut data).map_err(code)? {
                0 => Err(StatusCode::Eof),
                n => {
                    data.truncate(n);
                    Ok(Data { id, data })
                }
            }
        }

        async fn write(&mut self, id: u32, handle: String, offset: u64, data: Vec<u8>) -> Result<Status, StatusCode> {
            let f = self.file(&handle)?;
            f.seek(SeekFrom::Start(offset)).map_err(code)?;
            f.write_all(&data).map_err(code)?;
            Ok(ok(id))
        }

        async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
            let meta = std::fs::metadata(self.path(&path)).map_err(code)?;
            Ok(Attrs { id, attrs: FileAttributes::from(&meta) })
        }

        async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
            self.stat(id, path).await
        }

        async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, StatusCode> {
            let meta = self.file(&handle)?.metadata().map_err(code)?;
            Ok(Attrs { id, attrs: FileAttributes::from(&meta) })
        }

        async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusCode> {
            let mut files = Vec::new();
            for e in std::fs::read_dir(self.path(&path)).map_err(code)?.flatten() {
                files.push(File::new(e.file_name().to_string_lossy(), FileAttributes::from(&e.metadata().map_err(code)?)));
            }
            let handle = self.handle();
            self.listings.insert(handle.clone(), files);
            Ok(Handle { id, handle })
        }

        async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, StatusCode> {
            match self.listings.get_mut(&handle).map(std::mem::take) {
                Some(files) if !files.is_empty() => Ok(Name { id, files }),
                _ => Err(StatusCode::Eof),
            }
        }

        async fn remove(&mut self, id: u32, filename: String) -> Result<Status, StatusCode> {
            std::fs::remove_file(self.path(&filename)).map_err(code)?;
            Ok(ok(id))
        }

        async fn mkdir(&mut self, id: u32, path: String, _: FileAttributes) -> Result<Status, StatusCode> {
            std::fs::create_dir(self.path(&path)).map_err(code)?;
            Ok(ok(id))
        }

        async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
            std::fs::metadata(self.path(&path)).map_err(code)?;
            Ok(Name { id, files: vec![File::dummy(path)] })
        }

        async fn rename(&mut self, id: u32, oldpath: String, newpath: String) -> Result<Status, StatusCode> {
            if self.path(&newpath).exists() {
                return Err(StatusCode::Failure);
            }
            std::fs::rename(self.path(&oldpath), self.path(&newpath)).map_err(code)?;
            Ok(ok(id))
        }
    }

    /// One SSH connection: a password, and the SFTP subsystem over `root`
    struct Ssh {
        root: PathBuf,
        password: &'static str,
        channels: HashMap<ChannelId, Channel<Msg>>,
    }

    impl russh::server::Handler for Ssh {
        type Error = russh::Error;

        async fn auth_password(&mut self, _: &str, password: &str) -> Result<Auth, Self::Error> {
            Ok(if password == self.password { Auth::Accept } else { Auth::reject() })
        }

        async fn channel_open_session(&mut self, channel: Channel<Msg>, reply: ChannelOpenHandle, _: &mut Session) -> Result<(), Self::Error> {
            self.channels.insert(channel.id(), channel);
            reply.accept().await;
            Ok(())
        }

        async fn subsystem_request(&mut self, id: ChannelId, name: &str, session: &mut Session) -> Result<(), Self::Error> {
            match self.channels.remove(&id) {
                Some(channel) if name == "sftp" => {
                    session.channel_success(id)?;
                    let files = Files { root: self.root.clone(), open: HashMap::new(), listings: HashMap::new(), next: 0 };
                    russh_sftp::server::run(channel.into_stream(), files).await;
                }
                _ => session.channel_failure(id)?,
            }
            Ok(())
        }
    }

    /// An SSH server over a temporary folder, with a new host key
    struct Server {
        dir: PathBuf,
        port: u16,
        fingerprint: String,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn server(password: &'static str) -> Server {
        let dir = std::env::temp_dir().join(format!("thirtyfile-sftp-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("files")).unwrap();
        let key = keys::PrivateKey::from(keys::ssh_key::private::Ed25519Keypair::from_seed(&rand::random()));
        let fingerprint = key.public_key().fingerprint(HashAlg::Sha256).to_string();
        let config = Arc::new(russh::server::Config {
            keys: vec![key],
            auth_rejection_time: Duration::from_millis(10),
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let root = dir.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let ssh = Ssh { root: root.clone(), password, channels: HashMap::new() };
                let config = config.clone();
                tokio::spawn(async move {
                    if let Ok(session) = russh::server::run_stream(config, stream, ssh).await {
                        let _ = session.await;
                    }
                });
            }
        });
        Server { dir, port, fingerprint, task }
    }

    fn storage(s: &Server, password: &str, host_key: &str) -> SftpStorage {
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
        let s = server("secret").await;
        let st = storage(&s, "secret", "");
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

    /// The message a storage error shows people
    fn message(e: &io::Error) -> &'static str {
        e.get_ref().and_then(|e| e.downcast_ref::<StorageError>()).unwrap().message
    }

    #[tokio::test]
    async fn a_wrong_password_or_another_host_key_is_refused() {
        let s = server("secret").await;
        let err = storage(&s, "wrong", "").check().await.unwrap_err();
        assert!(message(&err).starts_with("Incorrect username, password"), "{}", message(&err));

        // The key recorded earlier is accepted; another one means another server (or someone in between)
        storage(&s, "secret", &s.fingerprint).ping().await.unwrap();
        let other = server("secret").await;
        let err = storage(&s, "secret", &other.fingerprint).ping().await.unwrap_err();
        assert!(message(&err).starts_with("The host key doesn't match"), "{}", message(&err));

        let port = s.port;
        drop(s);
        let cfg = SftpConfig { host: "127.0.0.1".into(), port, username: "backup".into(), password: "secret".into(), ..Default::default() };
        let err = SftpStorage::new(&cfg).unwrap().ping().await.unwrap_err();
        assert_eq!(message(&err), UNAVAILABLE);
    }
}
