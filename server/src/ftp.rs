//! FTP / FTPS storage location (common on older NAS devices and web hosts).
//!
//! - An FTP connection can only do one thing at a time: a connection pool (up to 4) serves multiple requests concurrently, reusing idle connections
//! - Downloads are read from FTP by a background task and forwarded to the caller, finishing cleanly whether fully read, partially read, or canceled midway by the caller
//! - FTPS (AUTH TLS) is verified against the operating system's certificates; verification can optionally be skipped for the self-signed certificates common on NAS devices
//! - Writes go to a temporary name first and are then renamed, so a mid-transfer disconnect never leaves an incomplete file that's mistaken for the correct content

use std::{
    collections::HashSet,
    io,
    path::Path,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};

use futures_util::future::BoxFuture;
use rustls::{
    ClientConfig, DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::CryptoProvider,
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use serde::{Deserialize, Serialize};
use suppaftp::{
    FtpError, Status,
    tokio::{AsyncRustlsConnector, AsyncRustlsFtpStream, AsyncRustlsStream, TransferStream},
    types::FileType,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Semaphore,
};

use crate::storage::{BoxReader, Storage, StorageError, UNAVAILABLE, valid_hash};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum time to wait for a response to each command
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CONNECTIONS: usize = 4;
/// How long a download may wait for the reader to take more data before its connection is given back
const STALLED_READER: Duration = Duration::from_secs(120);
/// Connections idle longer than this are checked to be alive before reuse
const IDLE_CHECK: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FtpConfig {
    #[serde(default)]
    pub host: String,
    /// 0 = 21
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    /// Folder to store files in (blank = the default folder after signing in)
    #[serde(default)]
    pub path: String,
    /// Use an FTPS (AUTH TLS) encrypted connection
    #[serde(default)]
    pub tls: bool,
    /// Don't verify the server certificate (for self-signed certificates; the connection is still encrypted but not protected from man-in-the-middle attacks)
    #[serde(default)]
    pub tls_insecure: bool,
}

impl FtpConfig {
    pub fn port(&self) -> u16 {
        if self.port == 0 { 21 } else { self.port }
    }
}

fn unavailable(detail: impl std::fmt::Display) -> io::Error {
    io::Error::other(StorageError { message: UNAVAILABLE, detail: detail.to_string() })
}

fn ftp_err(e: FtpError) -> io::Error {
    match &e {
        FtpError::UnexpectedResponse(r) if r.status == Status::FileUnavailable => io::Error::new(io::ErrorKind::NotFound, e.to_string()),
        FtpError::UnexpectedResponse(r) if r.status == Status::NotLoggedIn => io::Error::other(StorageError {
            message: "Incorrect username or password. Couldn't sign in to FTP.",
            detail: e.to_string(),
        }),
        FtpError::SecureError(_) => io::Error::other(StorageError {
            message: "FTPS encrypted connection failed: the server may not support TLS, or its certificate couldn't be verified (for self-signed certificates, select \"Skip certificate verification\")",
            detail: e.to_string(),
        }),
        _ => unavailable(e),
    }
}

/// A command timeout is also treated as the service being unavailable
async fn timed<T>(f: impl std::future::Future<Output = Result<T, FtpError>>) -> io::Result<T> {
    match tokio::time::timeout(COMMAND_TIMEOUT, f).await {
        Ok(r) => r.map_err(ftp_err),
        Err(_) => Err(unavailable("FTP command timed out")),
    }
}

/// Skips certificate verification (self-signed certificates); signatures are still checked as usual
#[derive(Debug)]
struct AcceptAnyCert(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn tls_connector(insecure: bool) -> io::Result<AsyncRustlsConnector> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let verifier: Arc<dyn ServerCertVerifier> = if insecure {
        Arc::new(AcceptAnyCert(provider.clone()))
    } else {
        Arc::new(rustls_platform_verifier::Verifier::new(provider.clone()).map_err(io::Error::other)?)
    };
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(tokio_rustls::TlsConnector::from(Arc::new(config)).into())
}

/// Finishes an upload: after sending the close signal (TLS close_notify), reads any remaining data from the server (e.g. TLS 1.3 session tickets) before closing.
/// Closing directly makes the OS reset the connection (RST) because of unread data, and the server may then discard the last data it received
async fn end_upload(mut out: TransferStream<AsyncRustlsStream>) -> io::Result<()> {
    out.flush().await.map_err(unavailable)?;
    let _ = out.shutdown().await;
    let mut sink = [0u8; 4096];
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        while let Ok(n) = out.read(&mut sink).await {
            if n == 0 {
                break;
            }
        }
    })
    .await;
    timed(out.finish()).await
}

struct Conn {
    ftp: AsyncRustlsFtpStream,
    last_used: Instant,
}

pub struct FtpStorage {
    cfg: FtpConfig,
    root: String,
    /// Idle connections (background downloads also return theirs here when done)
    idle: Arc<StdMutex<Vec<Conn>>>,
    permits: Arc<Semaphore>,
    dirs: StdMutex<HashSet<String>>,
}

impl FtpStorage {
    pub fn new(cfg: &FtpConfig) -> io::Result<Self> {
        if cfg.host.trim().is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Enter a host"));
        }
        if cfg.username.trim().is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Enter a username"));
        }
        let root = cfg.path.trim().trim_end_matches('/').to_string();
        Ok(Self { cfg: cfg.clone(), root, idle: Default::default(), permits: Arc::new(Semaphore::new(MAX_CONNECTIONS)), dirs: Default::default() })
    }

    async fn connect(&self) -> io::Result<Conn> {
        let cfg = &self.cfg;
        let host = cfg.host.trim();
        let addr = tokio::net::lookup_host((host, cfg.port()))
            .await
            .map_err(unavailable)?
            .next()
            .ok_or_else(|| unavailable("Host not found"))?;
        let mut ftp = AsyncRustlsFtpStream::connect_timeout(addr, CONNECT_TIMEOUT).await.map_err(ftp_err)?;
        if cfg.tls {
            ftp = timed(ftp.into_secure(tls_connector(cfg.tls_insecure)?, host)).await?;
        }
        // When the server is behind NAT, passive mode may report an internal address: always connect back to the control connection's address
        ftp.set_passive_nat_workaround(true);
        timed(ftp.login(cfg.username.trim(), cfg.password.as_str())).await?;
        timed(ftp.transfer_type(FileType::Binary)).await?;
        Ok(Conn { ftp, last_used: Instant::now() })
    }

    /// Gets a connection (idle ones first; those unused for a while are checked to be alive)
    async fn checkout(&self) -> io::Result<(Conn, tokio::sync::OwnedSemaphorePermit)> {
        let permit = self.permits.clone().acquire_owned().await.map_err(unavailable)?;
        loop {
            let idle = self.idle.lock().unwrap().pop();
            let Some(mut c) = idle else { break };
            if c.last_used.elapsed() < IDLE_CHECK || timed(c.ftp.noop()).await.is_ok() {
                return Ok((c, permit));
            }
        }
        Ok((self.connect().await?, permit))
    }

    fn checkin(&self, mut c: Conn) {
        c.last_used = Instant::now();
        self.idle.lock().unwrap().push(c);
    }

    fn path(&self, rel: &str) -> String {
        if self.root.is_empty() { rel.to_string() } else { format!("{}/{rel}", self.root) }
    }

    fn blob_dir(&self, hash: &str) -> String {
        self.path(&format!("blobs/{}/{}", &hash[0..2], &hash[2..4]))
    }

    fn blob_path(&self, hash: &str) -> io::Result<String> {
        valid_hash(hash)?;
        Ok(format!("{}/{hash}", self.blob_dir(hash)))
    }

    /// Creates folders level by level (ignoring already-exists errors)
    async fn ensure_dir(&self, c: &mut Conn, dir: &str) -> io::Result<()> {
        if dir.is_empty() || self.dirs.lock().unwrap().contains(dir) {
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
            if self.dirs.lock().unwrap().contains(&path) {
                continue;
            }
            match timed(c.ftp.mkdir(path.as_str())).await {
                Ok(()) => {}
                // Already exists (550): make sure it really is a folder
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    let pwd = timed(c.ftp.pwd()).await?;
                    timed(c.ftp.cwd(path.as_str())).await.map_err(|_| io::Error::other(format!("Couldn't create folder {path}")))?;
                    timed(c.ftp.cwd(pwd.as_str())).await?;
                }
                Err(e) => return Err(e),
            }
            self.dirs.lock().unwrap().insert(path.clone());
        }
        Ok(())
    }

    async fn put(&self, c: &mut Conn, hash: &str, src: &Path) -> io::Result<()> {
        self.ensure_dir(c, &self.blob_dir(hash)).await?;
        let path = self.blob_path(hash)?;
        let tmp = format!("{path}.part-{}", uuid::Uuid::new_v4().simple());
        let mut input = tokio::fs::File::open(src).await?;
        let mut out = timed(c.ftp.put_with_stream(tmp.as_str())).await?;
        tokio::io::copy(&mut input, &mut out).await.map_err(unavailable)?;
        end_upload(out).await?;
        if let Err(e) = timed(c.ftp.rename(tmp.as_str(), path.as_str())).await {
            // Some servers don't allow rename to overwrite: if the same content already exists, keep the original
            let exists = timed(c.ftp.size(path.as_str())).await.is_ok();
            let _ = timed(c.ftp.rm(tmp.as_str())).await;
            if !exists {
                return Err(e);
            }
        }
        Ok(())
    }
}

impl Storage for FtpStorage {
    fn put_file<'a>(&'a self, hash: &'a str, src: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let (mut c, _permit) = self.checkout().await?;
            match self.put(&mut c, hash, src).await {
                Ok(()) => {
                    self.checkin(c);
                    let _ = tokio::fs::remove_file(src).await;
                    Ok(())
                }
                // A failed connection is in an unknown state, so don't return it to the pool
                Err(e) => Err(e),
            }
        })
    }

    fn open<'a>(&'a self, hash: &'a str, start: u64, len: u64) -> BoxFuture<'a, io::Result<BoxReader>> {
        Box::pin(async move {
            if len == 0 {
                return Ok(Box::pin(tokio::io::empty()) as BoxReader);
            }
            let path = self.blob_path(hash)?;
            let (mut c, permit) = self.checkout().await?;
            if start > 0 {
                timed(c.ftp.resume_transfer(start as usize)).await?;
            }
            let mut stream = match timed(c.ftp.retr_as_stream(path.as_str())).await {
                Ok(s) => s,
                Err(e) => {
                    if e.kind() == io::ErrorKind::NotFound {
                        self.checkin(c);
                    }
                    return Err(e);
                }
            };
            // The background task forwards the data to the caller; after reading the whole range it finishes and returns the connection to the pool
            let (mut tx, rx) = tokio::io::duplex(256 * 1024);
            let idle = self.idle.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let mut left = len;
                let mut buf = vec![0u8; 64 * 1024];
                let mut ok = true;
                while left > 0 {
                    let want = buf.len().min(left as usize);
                    let n = match tokio::time::timeout(COMMAND_TIMEOUT, stream.read(&mut buf[..want])).await {
                        Ok(Ok(0)) | Ok(Err(_)) | Err(_) => {
                            ok = false;
                            break;
                        }
                        Ok(Ok(n)) => n,
                    };
                    // The caller canceled the download, or stopped reading (a paused download or video): give the
                    // connection back instead of holding one of the few there are
                    if !matches!(tokio::time::timeout(STALLED_READER, tx.write_all(&buf[..n])).await, Ok(Ok(()))) {
                        ok = false;
                        break;
                    }
                    left -= n as u64;
                }
                drop(tx);
                if !ok {
                    return;
                }
                // Range fully read: if it reached the end of the file, finish normally and return the connection to the pool; if only part was read (e.g. video seeking), abort the transfer.
                // Servers respond inconsistently after an abort (possibly an extra 225/226 line), so the connection state is unreliable: close it instead of reusing it
                // Many FTPS servers don't send TLS close_notify when closing the data connection: such an ending after reading the expected length also counts as normal
                let mut probe = [0u8; 1];
                let at_end = match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut probe)).await {
                    Ok(Ok(0)) => true,
                    Ok(Err(e)) => e.kind() == io::ErrorKind::UnexpectedEof,
                    _ => false,
                };
                if at_end {
                    if timed(stream.finish()).await.is_ok() {
                        c.last_used = Instant::now();
                        idle.lock().unwrap().push(c);
                    }
                } else {
                    let _ = timed(c.ftp.abort(stream)).await;
                    let _ = timed(c.ftp.quit()).await;
                }
            });
            Ok(Box::pin(rx) as BoxReader)
        })
    }

    fn delete<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let path = self.blob_path(hash)?;
            let (mut c, _permit) = self.checkout().await?;
            match timed(c.ftp.rm(path.as_str())).await {
                Ok(()) => {
                    self.checkin(c);
                    Ok(())
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    self.checkin(c);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        })
    }

    fn check(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            let (mut c, _permit) = self.checkout().await?;
            self.ensure_dir(&mut c, &self.root).await?;
            let probe = self.path(&format!(".thirtyfile-check-{}", uuid::Uuid::new_v4().simple()));
            let result: io::Result<()> = async {
                let mut out = timed(c.ftp.put_with_stream(probe.as_str())).await?;
                out.write_all(b"ok").await.map_err(unavailable)?;
                end_upload(out).await?;
                let mut s = timed(c.ftp.retr_as_stream(probe.as_str())).await?;
                // Read only the expected length (the server may not send close_notify when closing the TLS data connection)
                let mut back = [0u8; 2];
                s.read_exact(&mut back).await.map_err(unavailable)?;
                timed(s.finish()).await?;
                if &back != b"ok" {
                    return Err(io::Error::other("The content read back didn't match"));
                }
                Ok(())
            }
            .await;
            let _ = timed(c.ftp.rm(probe.as_str())).await;
            if result.is_ok() {
                self.checkin(c);
            }
            result
        })
    }

    fn ping(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            // Every connection is in use, so they work: don't wait behind long downloads and report the server as down
            if self.permits.available_permits() == 0 {
                return Ok(());
            }
            let (mut c, _permit) = self.checkout().await?;
            timed(c.ftp.noop()).await?;
            self.checkin(c);
            Ok(())
        })
    }
}

