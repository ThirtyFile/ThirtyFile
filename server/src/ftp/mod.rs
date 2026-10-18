//! Optional device uploads. The HTTP service starts first and remains available if FTP cannot listen.

pub mod config;
pub mod credentials;
mod login;
pub mod manage;
mod storage;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tls_tests;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};
use libunftp::{
    ServerBuilder,
    options::{ActivePassiveMode, FtpsRequired, PassiveHost},
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{
    collections::HashMap,
    net::{IpAddr, Shutdown},
    path::PathBuf,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[derive(Clone, Default)]
pub struct Status {
    pub address: String,
    pub passive_ports: String,
    pub passive_host: String,
    pub tls_ready: bool,
    pub listening: bool,
    pub error: Option<String>,
}
#[derive(Default)]
pub struct Memory {
    pub status: crate::sync::RwLock<Status>,
    pub changed: tokio::sync::Notify,
    pub sessions: crate::sync::Mutex<HashMap<String, Weak<tokio::sync::Semaphore>>>,
    /// Tests: how many times the supervisor has looked at the settings, to wait for its next look rather than a time
    #[cfg(test)]
    pub looks: std::sync::atomic::AtomicU64,
}
#[derive(Clone)]
pub struct Settings {
    pub address: String,
    pub passive: String,
    pub host: String,
    pub cert: Option<PathBuf>,
    pub key: Option<PathBuf>,
}
struct Transport {
    plain: bool,
    tls: Option<Arc<rustls::ServerConfig>>,
    host: PassiveHost,
    ports: std::ops::RangeInclusive<u16>,
}
pub struct Listener {
    stop: tokio::sync::watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl Listener {
    pub async fn finish(mut self) {
        let _ = self.stop.send(true);
        if let Some(mut task) = self.task.take()
            && tokio::time::timeout(Duration::from_secs(5), &mut task).await.is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
}
impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

/// Tests: counts one look of the supervisor once it is done with it (`Memory::looks`)
#[cfg(test)]
struct Looked<'a>(&'a AppState);

#[cfg(test)]
impl Drop for Looked<'_> {
    fn drop(&mut self) {
        self.0.part::<Memory>().looks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

fn passive_ports(value: &str) -> AppResult<std::ops::RangeInclusive<u16>> {
    let (from, to) = value.split_once('-').ok_or_else(|| AppError::bad_request("Use a passive port range such as 30000-30009"))?;
    let from: u16 = from.parse().map_err(|_| AppError::bad_request("Invalid FTP passive port range"))?;
    let to: u16 = to.parse().map_err(|_| AppError::bad_request("Invalid FTP passive port range"))?;
    // Tests: "0-0" lets the system choose a free port for each passive connection (the reply names the one it chose),
    // so tests running at once never try to take the same one
    #[cfg(test)]
    if (from, to) == (0, 0) {
        return Ok(0..=0);
    }
    if from == 0 || to < from || u32::from(to) - u32::from(from) > 999 {
        return Err(AppError::bad_request("Use 1–1000 FTP passive ports"));
    }
    Ok(from..=to)
}
fn tls(settings: &Settings, private_plain: bool) -> AppResult<Option<Arc<rustls::ServerConfig>>> {
    let (cert, key) = match (&settings.cert, &settings.key) {
        (Some(c), Some(k)) => (c, k),
        (None, None) if private_plain => return Ok(None),
        _ => return Err(AppError::bad_request("Mount an FTP TLS certificate and key, or allow plaintext on private networks")),
    };
    let certs = CertificateDer::pem_reader_iter(std::fs::File::open(cert)?).collect::<Result<Vec<_>, _>>().map_err(AppError::internal)?;
    let key = PrivateKeyDer::from_pem_reader(std::fs::File::open(key)?).map_err(|e| match e {
        rustls::pki_types::pem::Error::NoItemsFound => AppError::bad_request("The FTP TLS key is missing"),
        e => AppError::internal(e),
    })?;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(AppError::internal)?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(AppError::internal)?;
    config.session_storage = rustls::server::ServerSessionMemoryCache::new(256);
    Ok(Some(Arc::new(config)))
}

/// How often the settings are read again when nothing says they changed
const IDLE: Duration = Duration::from_secs(5);

/// How long a signed-in FTP session may last. Idle ones end after two minutes (`idle_session_timeout`); this only ends
/// one that never stops, and is long enough for a camera's slow upload of a whole day
const SESSION_MAX: Duration = Duration::from_secs(12 * 3600);

pub fn supervise(st: AppState, settings: Settings) -> Listener {
    *st.part::<Memory>().status.write() =
        Status { address: settings.address.clone(), passive_ports: settings.passive.clone(), passive_host: settings.host.clone(), ..Default::default() };
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(async move {
        let mut active: Option<Listener> = None;
        let mut current = None;
        let mut retry = tokio::time::Instant::now();
        let mut first = true;
        loop {
            // The settings are read right away, then when they change (`changed`), or now and then: every half second
            // while a start that failed waits to be tried again, else every few seconds
            let failing = active.is_none() && current.as_ref().is_some_and(|c: &config::Config| c.enabled);
            let wait = if std::mem::take(&mut first) {
                Duration::ZERO
            } else if failing {
                Duration::from_millis(500)
            } else {
                IDLE
            };
            tokio::select! {_=stopped.changed()=>break,_=tokio::time::sleep(wait)=>(),_=st.part::<Memory>().changed.notified()=>()}
            let cfg = config::load(&st).await.unwrap_or_default();
            #[cfg(test)]
            let _looked = Looked(&st);
            if current.as_ref() != Some(&cfg) {
                if let Some(listener) = active.take() {
                    listener.finish().await;
                }
                st.part::<Memory>().status.write().listening = false;
                current = Some(cfg.clone());
                retry = tokio::time::Instant::now();
            }
            if !cfg.enabled {
                continue;
            }
            if active.is_none() && tokio::time::Instant::now() >= retry {
                match start(st.clone(), settings.clone(), cfg).await {
                    Ok(listener) => {
                        let mut status = st.part::<Memory>().status.write();
                        status.error = None;
                        status.listening = true;
                        active = Some(listener);
                    }
                    Err(e) => {
                        let mut status = st.part::<Memory>().status.write();
                        if status.error.as_ref() != Some(&e.message) {
                            tracing::warn!("FTP is unavailable: {}; HTTP remains available", e.message);
                        }
                        status.error = Some(e.message);
                        retry = tokio::time::Instant::now() + Duration::from_secs(5);
                    }
                }
            }
        }
        if let Some(listener) = active {
            listener.finish().await;
        }
        st.part::<Memory>().status.write().listening = false;
    });
    Listener { stop, task: Some(task) }
}

pub async fn start(st: AppState, settings: Settings, cfg: config::Config) -> AppResult<Listener> {
    let ports = passive_ports(&settings.passive)?;
    let tls = tls(&settings, cfg.allow_private_plaintext)?;
    let host = if settings.host.is_empty() {
        PassiveHost::FromConnection
    } else if let Ok(ip) = settings.host.parse::<std::net::Ipv4Addr>() {
        PassiveHost::Ip(ip)
    } else {
        if settings.host.len() > 253 || settings.host.contains(['/', ':', '@', ' ', '\n', '\r']) {
            return Err(AppError::bad_request("Enter an FTP passive hostname without a scheme, port or path"));
        }
        PassiveHost::Dns(settings.host.clone())
    };
    let socket = tokio::net::TcpListener::bind(&settings.address).await?;
    {
        let mut status = st.part::<Memory>().status.write();
        status.address = socket.local_addr()?.to_string();
        status.tls_ready = tls.is_some();
    }
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(async move {
        let slots = Arc::new(tokio::sync::Semaphore::new(128));
        let pending = crate::auth::pending::Pending::default();
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                _=stopped.changed()=>break,
                Some(_)=tasks.join_next(),if !tasks.is_empty()=>(),
                result=socket.accept()=>{
                    let Ok((stream,peer))=result else{break};
                    let Some(waiting)=pending.admit(peer.ip()) else{continue};
                    let Ok(permit)=slots.clone().try_acquire_owned() else{continue};
                    let plain=cfg.allow_private_plaintext && login::private_peer(peer.ip());
                    if !plain && tls.is_none(){continue;}
                    let (st,tls,host,ports,stop)=(st.clone(),tls.clone(),host.clone(),ports.clone(),stopped.clone());
                    tasks.spawn(connection(st,stream,peer.ip(),Transport { plain, tls, host, ports },stop,(permit,waiting)));
                }
            }
        }
        drop(socket);
        while tasks.join_next().await.is_some() {}
    });
    Ok(Listener { stop, task: Some(task) })
}

async fn connection(
    st: AppState,
    stream: tokio::net::TcpStream,
    peer: IpAddr,
    transport: Transport,
    mut stopped: tokio::sync::watch::Receiver<bool>,
    (_permit, mut waiting): (tokio::sync::OwnedSemaphorePermit, crate::auth::pending::PendingGuard),
) {
    let Transport { plain, tls, host, ports } = transport;
    let Ok(wire) = stream.into_std() else { return };
    let Ok(shutdown) = wire.try_clone() else { return };
    let Ok(stream) = tokio::net::TcpStream::from_std(wire) else { return };
    let alive = Arc::new(AtomicBool::new(true));
    let authenticated = Arc::new(AtomicBool::new(false));
    let signed_in = Arc::new(tokio::sync::Notify::new());
    let login = Arc::new(login::Login {
        state: st.clone(),
        peer,
        alive: alive.clone(),
        plain,
        authenticated: authenticated.clone(),
        signed_in: signed_in.clone(),
        user: Default::default(),
    });
    let files = st.clone();
    let mut builder = ServerBuilder::with_user_detail_provider(Box::new(move || storage::Files(files.clone())), login.clone())
        .authenticator(login)
        .greeting("ThirtyFile receive folder")
        .active_passive_mode(ActivePassiveMode::PassiveOnly)
        .idle_session_timeout(120)
        .passive_host(host)
        .passive_ports(ports)
        .ftps_required(if plain { FtpsRequired::None } else { FtpsRequired::All }, if plain { FtpsRequired::None } else { FtpsRequired::All });
    if let Some(tls) = tls {
        builder = builder.ftps_manual::<PathBuf>(tls);
    }
    let Ok(server) = builder.build() else { return };
    let mut service = tokio::spawn(server.service(stream));
    let deadline = tokio::time::sleep(Duration::from_secs(20));
    tokio::pin!(deadline);
    let mut finished = false;
    tokio::select! {
        _=&mut service=>finished=true,
        _=stopped.changed()=>(),
        _=signed_in.notified()=>{
            waiting.signed_in();
            tokio::select!{_=&mut service=>finished=true,_=stopped.changed()=>(),_=tokio::time::sleep(SESSION_MAX)=>()}
        }
        _=&mut deadline=>{
            if authenticated.load(Ordering::Acquire){
                waiting.signed_in();
                tokio::select!{_=&mut service=>finished=true,_=stopped.changed()=>(),_=tokio::time::sleep(SESSION_MAX)=>()}
            }
        }
    }
    alive.store(false, Ordering::Release);
    let _ = shutdown.shutdown(Shutdown::Both);
    if !finished && tokio::time::timeout(Duration::from_secs(2), &mut service).await.is_err() {
        service.abort();
        let _ = service.await;
    }
}
