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
    Ok(tokio_rustls::TlsConnector::from(client_tls(insecure)?).into())
}

/// The TLS set-up is built once for each kind (verifying or not) and shared by every connection (FTPS here, email in
/// mail.rs): loading the platform's certificate verifier is slow
pub fn client_tls(insecure: bool) -> io::Result<Arc<ClientConfig>> {
    static CONFIGS: [std::sync::OnceLock<Arc<ClientConfig>>; 2] = [std::sync::OnceLock::new(), std::sync::OnceLock::new()];
    let slot = &CONFIGS[usize::from(insecure)];
    Ok(match slot.get() {
        Some(c) => c.clone(),
        None => {
            let c = tls_config(insecure)?;
            slot.get_or_init(|| c).clone()
        }
    })
}

fn tls_config(insecure: bool) -> io::Result<Arc<ClientConfig>> {
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
    Ok(Arc::new(config))
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
        for path in crate::storage::dir_levels(dir) {
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
            self.dirs.lock().unwrap().insert(path);
        }
        Ok(())
    }

    async fn put(&self, c: &mut Conn, hash: &str, src: &Path) -> io::Result<()> {
        self.ensure_dir(c, &self.blob_dir(hash)).await?;
        let path = self.blob_path(hash)?;
        let tmp = format!("{path}.part-{}", uuid::Uuid::new_v4().simple());
        let mut input = tokio::fs::File::open(src).await?;
        let mut out = timed(c.ftp.put_with_stream(tmp.as_str())).await?;
        let sent = async {
            tokio::io::copy(&mut input, &mut out).await.map_err(unavailable)?;
            end_upload(out).await
        }
        .await;
        if let Err(e) = sent {
            // Don't leave the half-written file on the server (best effort: the connection may be gone)
            let _ = timed(c.ftp.rm(tmp.as_str())).await;
            return Err(e);
        }
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
    fn size<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, io::Result<Option<u64>>> {
        Box::pin(async move {
            let path = self.blob_path(hash)?;
            let (mut c, _permit) = self.checkout().await?;
            let res = match timed(c.ftp.size(path.as_str())).await {
                Ok(n) => Ok(Some(n as u64)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => return Err(e),
            };
            self.checkin(c);
            res
        })
    }

    fn list(&self) -> BoxFuture<'_, io::Result<Vec<String>>> {
        Box::pin(async move {
            let (mut c, _permit) = self.checkout().await?;
            // Servers answer NLST with bare names or with paths: the last part is the name
            async fn names(c: &mut Conn, dir: &str) -> io::Result<Vec<String>> {
                match timed(c.ftp.nlst(Some(dir))).await {
                    Ok(list) => Ok(list.into_iter().map(|n| n.rsplit('/').next().unwrap_or_default().to_string()).collect()),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
                    Err(e) => Err(e),
                }
            }
            let base = self.path("blobs");
            let mut out = Vec::new();
            for a in names(&mut c, &base).await?.into_iter().filter(|n| n.len() == 2) {
                for b in names(&mut c, &format!("{base}/{a}")).await?.into_iter().filter(|n| n.len() == 2) {
                    out.extend(names(&mut c, &format!("{base}/{a}/{b}")).await?.into_iter().filter(|n| crate::storage::is_hash(n)));
                }
            }
            self.checkin(c);
            Ok(out)
        })
    }

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

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use tokio::{
        io::{AsyncBufReadExt, BufReader},
        net::{TcpListener, TcpStream},
    };

    use super::*;
    use crate::testutil;

    /// A small FTP server over a temporary folder: passive mode, plain connections, and only the commands the storage
    /// uses. It counts the connections it was given.
    struct Server {
        dir: PathBuf,
        port: u16,
        connections: Arc<AtomicUsize>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn server(password: &'static str) -> Server {
        let dir = std::env::temp_dir().join(format!("thirtyfile-ftp-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("files")).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let connections = Arc::new(AtomicUsize::new(0));
        let (root, count) = (dir.clone(), connections.clone());
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                count.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(session(stream, root.clone(), password));
            }
        });
        Server { dir, port, connections, task }
    }

    async fn session(stream: TcpStream, root: PathBuf, password: &'static str) -> io::Result<()> {
        let (r, mut w) = stream.into_split();
        let mut lines = BufReader::new(r).lines();
        w.write_all(b"220 Ready\r\n").await?;
        let (mut cwd, mut rest, mut from, mut passive) = ("/".to_string(), 0usize, None, None::<TcpListener>);
        while let Some(line) = lines.next_line().await? {
            let (cmd, arg) = line.split_once(' ').unwrap_or((line.as_str(), ""));
            let path = root.join(arg.trim_start_matches('/'));
            let reply = match cmd {
                "USER" => "331 Password please".to_string(),
                "PASS" if arg == password => "230 Signed in".into(),
                "PASS" => "530 Wrong password".into(),
                "TYPE" | "NOOP" => "200 OK".into(),
                "PWD" => format!("257 \"{cwd}\""),
                "CWD" if path.is_dir() => {
                    cwd = arg.to_string();
                    "250 OK".into()
                }
                "MKD" if std::fs::create_dir(&path).is_ok() => "257 Created".into(),
                "SIZE" if path.is_file() => format!("213 {}", path.metadata()?.len()),
                "DELE" if std::fs::remove_file(&path).is_ok() => "250 Deleted".into(),
                "RNFR" if path.exists() => {
                    from = Some(path);
                    "350 Go on".into()
                }
                "RNTO" if from.take().is_some_and(|f| std::fs::rename(f, &path).is_ok()) => "250 Renamed".into(),
                "REST" => {
                    rest = arg.parse().unwrap_or(0);
                    "350 Restarting".into()
                }
                "PASV" => {
                    let l = TcpListener::bind("127.0.0.1:0").await?;
                    let p = l.local_addr()?.port();
                    passive = Some(l);
                    format!("227 Entering Passive Mode (127,0,0,1,{},{})", p / 256, p % 256)
                }
                "RETR" | "STOR" | "NLST" if (cmd == "STOR" || path.exists()) && passive.is_some() => {
                    let (mut data, _) = passive.take().unwrap().accept().await?;
                    w.write_all(b"150 Opening data connection\r\n").await?;
                    // A client that stops reading (or aborts) just ends the transfer
                    let _ = match cmd {
                        "RETR" => data.write_all(&std::fs::read(&path)?[std::mem::take(&mut rest)..]).await,
                        "STOR" => {
                            let mut content = Vec::new();
                            data.read_to_end(&mut content).await?;
                            std::fs::write(&path, content)
                        }
                        _ => {
                            let names: Vec<String> =
                                std::fs::read_dir(&path)?.flatten().map(|e| format!("{}\r\n", e.file_name().to_string_lossy())).collect();
                            data.write_all(names.concat().as_bytes()).await
                        }
                    };
                    drop(data);
                    "226 Done".into()
                }
                "ABOR" => "226 Aborted".into(),
                "QUIT" => {
                    w.write_all(b"221 Bye\r\n").await?;
                    return Ok(());
                }
                _ => "550 Not possible".into(),
            };
            w.write_all(format!("{reply}\r\n").as_bytes()).await?;
        }
        Ok(())
    }

    fn storage(s: &Server, password: &str) -> FtpStorage {
        let cfg = FtpConfig { host: "127.0.0.1".into(), port: s.port, username: "backup".into(), password: password.into(), path: "/files".into(), ..Default::default() };
        FtpStorage::new(&cfg).unwrap()
    }

    async fn read(st: &FtpStorage, hash: &str, start: u64, len: u64) -> Vec<u8> {
        let mut out = Vec::new();
        st.open(hash, start, len).await.unwrap().read_to_end(&mut out).await.unwrap();
        out
    }

    /// Puts content into the storage from a temporary file, as uploads do; returns its hash
    async fn put(st: &FtpStorage, s: &Server, content: &[u8]) -> String {
        let hash = crate::util::sha256_hex(content);
        let src = s.dir.join(format!("src-{hash}"));
        std::fs::write(&src, content).unwrap();
        st.put_file(&hash, &src).await.unwrap();
        assert!(!src.exists(), "the temporary file is removed once stored");
        hash
    }

    #[tokio::test]
    async fn content_is_stored_read_listed_and_deleted_over_ftp() {
        let s = server(testutil::password()).await;
        let st = storage(&s, testutil::password());
        st.check().await.unwrap();

        let hash = put(&st, &s, b"hello over ftp").await;
        // Written under its hash, two folder levels down, with nothing left under a temporary name
        let stored = s.dir.join(format!("files/blobs/{}/{}/{hash}", &hash[0..2], &hash[2..4]));
        assert_eq!(std::fs::read(&stored).unwrap(), b"hello over ftp");
        assert_eq!(std::fs::read_dir(stored.parent().unwrap()).unwrap().count(), 1);
        // The same content again is fine
        assert_eq!(put(&st, &s, b"hello over ftp").await, hash);

        assert_eq!(read(&st, &hash, 0, 14).await, b"hello over ftp");
        assert_eq!(read(&st, &hash, 6, 8).await, b"over ftp");
        assert_eq!(read(&st, &hash, 0, 5).await, b"hello");
        assert_eq!(read(&st, &hash, 0, 0).await, b"");
        assert_eq!(st.size(&hash).await.unwrap(), Some(14));
        let other = put(&st, &s, b"second").await;
        let mut listed = st.list().await.unwrap();
        listed.sort();
        let mut want = vec![hash.clone(), other.clone()];
        want.sort();
        assert_eq!(listed, want);

        st.delete(&hash).await.unwrap();
        assert!(!stored.exists());
        assert_eq!(st.size(&hash).await.unwrap(), None);
        // Deleting what isn't there is fine; reading it says it isn't there
        st.delete(&hash).await.unwrap();
        assert_eq!(st.open(&hash, 0, 14).await.err().unwrap().kind(), io::ErrorKind::NotFound);
        // Once the downloads have given their connections back, the next requests reuse them
        for _ in 0..100 {
            if st.permits.available_permits() == MAX_CONNECTIONS {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let before = s.connections.load(Ordering::SeqCst);
        for _ in 0..5 {
            st.ping().await.unwrap();
            assert_eq!(st.size(&other).await.unwrap(), Some(6));
        }
        assert_eq!(s.connections.load(Ordering::SeqCst), before);
    }

    #[tokio::test]
    async fn requests_at_once_share_a_few_connections() {
        let s = server(testutil::password()).await;
        let st = Arc::new(storage(&s, testutil::password()));
        let hash = put(&st, &s, &[7u8; 100_000]).await;
        let reads: Vec<_> = (0..12u64)
            .map(|i| {
                let (st, hash) = (st.clone(), hash.clone());
                tokio::spawn(async move { read(&st, &hash, i * 1000, 100_000 - i * 1000).await.len() as u64 })
            })
            .collect();
        for (i, r) in reads.into_iter().enumerate() {
            assert_eq!(r.await.unwrap(), 100_000 - i as u64 * 1000);
        }
        let n = s.connections.load(Ordering::SeqCst);
        assert!(n <= MAX_CONNECTIONS, "{n} connections");
    }

    #[tokio::test]
    async fn a_wrong_password_or_a_server_that_is_gone_is_reported_as_such() {
        let s = server(testutil::password()).await;
        let err = storage(&s, &testutil::wrong_password()).check().await.unwrap_err();
        let inner = err.get_ref().and_then(|e| e.downcast_ref::<StorageError>()).unwrap();
        assert!(inner.message.starts_with("Incorrect username or password"), "{}", inner.message);

        let port = s.port;
        drop(s);
        let cfg = FtpConfig { host: "127.0.0.1".into(), port, username: "backup".into(), password: testutil::wrong_password(), ..Default::default() };
        let err = FtpStorage::new(&cfg).unwrap().ping().await.unwrap_err();
        let inner = err.get_ref().and_then(|e| e.downcast_ref::<StorageError>()).unwrap();
        assert_eq!(inner.message, UNAVAILABLE);
    }
}

