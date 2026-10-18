//! Authentication uses the actual TCP peer and the common sign-in address budget.

use super::credentials::{Credential, Folder};
use crate::{auth, state::AppState};
use std::{
    fmt,
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use unftp_core::auth::{AuthenticationError, Authenticator, ChannelEncryptionState, Credentials, Principal, UserDetail, UserDetailError, UserDetailProvider};

pub struct Login {
    pub state: AppState,
    pub peer: IpAddr,
    pub alive: Arc<AtomicBool>,
    pub plain: bool,
    pub authenticated: Arc<AtomicBool>,
    /// Told when the device has signed in
    pub signed_in: Arc<tokio::sync::Notify>,
    pub user: crate::sync::Mutex<Option<Device>>,
}

/// The address a connection comes from when Docker's port proxy relays it: the default gateway of the system ThirtyFile
/// runs in (on Linux; read once). A connection from it may come from anywhere, so it never counts as the local network
/// for plain FTP. On a host without Docker that is the router, from which devices don't connect.
fn proxy_address() -> Option<std::net::Ipv4Addr> {
    static GATEWAY: std::sync::OnceLock<Option<std::net::Ipv4Addr>> = std::sync::OnceLock::new();
    *GATEWAY.get_or_init(|| default_gateway(&std::fs::read_to_string("/proc/net/route").ok()?))
}

/// The default gateway in the text of `/proc/net/route` (addresses as little-endian hex)
fn default_gateway(routes: &str) -> Option<std::net::Ipv4Addr> {
    routes.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        (fields.get(1) == Some(&"00000000"))
            .then(|| u32::from_str_radix(fields.get(2)?, 16).ok())
            .flatten()
            .filter(|gateway| *gateway != 0)
            .map(|gateway| std::net::Ipv4Addr::from(gateway.to_le_bytes()))
    })
}

impl fmt::Debug for Login {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FtpLogin").field("peer", &self.peer).finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct Device {
    pub actor: auth::User,
    pub username: String,
    pub _permit: Arc<tokio::sync::OwnedSemaphorePermit>,
}
impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.username)
    }
}
impl fmt::Debug for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FtpDevice").field("username", &self.username).finish_non_exhaustive()
    }
}
impl UserDetail for Device {
    fn account_enabled(&self) -> bool {
        self.actor.ftp_credential.as_ref().is_some_and(|c| c.alive.load(Ordering::Acquire))
    }
}

pub fn private_peer(ip: IpAddr) -> bool {
    // Docker's port proxy connects from the container's gateway, whoever the client is
    if proxy_address().is_some_and(|proxy| ip.to_canonical() == IpAddr::V4(proxy)) {
        return false;
    }
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map(|v| private_peer(IpAddr::V4(v))).unwrap_or_else(|| ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00),
    }
}

#[async_trait::async_trait]
impl Authenticator for Login {
    async fn authenticate(&self, username: &str, creds: &Credentials) -> Result<Principal, AuthenticationError> {
        let denied = || AuthenticationError::BadPassword;
        if !self.alive.load(Ordering::Acquire) || creds.source_ip != self.peer || (creds.command_channel_security != ChannelEncryptionState::Tls && !self.plain) {
            return Err(denied());
        }
        let key = format!("ip:{}", auth::limit_key_ip(&self.peer.to_string()));
        // Reserve the attempt before Argon2, as browser sign-in does for concurrent requests.
        if !auth::begin_attempt(&self.state, &key, auth::IP_FAIL_LIMIT) {
            return Err(denied());
        }
        if username.len() > 100 || creds.password.as_ref().is_none_or(|p| p.len() > 256) {
            return Err(denied());
        }
        let row: Option<Folder> =
            sqlx::query_as("SELECT * FROM ftp_receive_folders WHERE username=?").bind(username).fetch_optional(&self.state.db).await.map_err(|_| denied())?;
        let hash = row.as_ref().map(|r| r.password_hash.clone()).unwrap_or_else(|| auth::DUMMY_HASH.to_string());
        let valid = auth::verify_password(creds.password.clone().unwrap_or_default(), hash).await.map_err(|_| denied())?;
        let Some(row) = row.filter(|r| valid && r.enabled && r.expires_at.is_none_or(|t| t > crate::util::now())) else {
            crate::logs::record_login_via(&self.state, None, username, "ftp_failed", "ftp", &self.peer.to_string(), &axum::http::HeaderMap::new());
            return Err(denied());
        };
        let mut conn = self.state.db.acquire().await.map_err(|_| denied())?;
        let mut actor = auth::user_by_id(&self.state, &mut conn, row.creator_id).await.map_err(|_| denied())?.ok_or_else(denied)?;
        actor.protocol_root = Some(row.node_id.clone());
        actor.ftp_credential = Some(Credential { id: row.id.clone(), generation: row.generation, alive: self.alive.clone(), transfer: None });
        actor = super::credentials::actor_in_transaction(&mut conn, &actor, crate::tree::Need::Add).await.map_err(|_| denied())?;
        drop(conn);
        let semaphore = {
            let mut sessions = self.state.part::<super::Memory>().sessions.lock();
            sessions.retain(|_, s| s.strong_count() > 0);
            sessions.get(&row.id).and_then(std::sync::Weak::upgrade).unwrap_or_else(|| {
                let s = Arc::new(tokio::sync::Semaphore::new(5));
                sessions.insert(row.id.clone(), Arc::downgrade(&s));
                s
            })
        };
        let permit = semaphore.try_acquire_owned().map_err(|_| AuthenticationError::new("This receive folder already has 5 sessions"))?;
        let principal = Principal { username: format!("{}:{}", row.id, row.generation) };
        *self.user.lock() = Some(Device { actor, username: row.username, _permit: Arc::new(permit) });
        self.authenticated.store(true, Ordering::Release);
        self.signed_in.notify_one();
        sqlx::query("UPDATE ftp_receive_folders SET last_used_at=? WHERE id=? AND generation=?")
            .bind(crate::util::now())
            .bind(row.id)
            .bind(row.generation)
            .execute(&self.state.db)
            .await
            .map_err(|_| denied())?;
        auth::attempt_succeeded(&self.state, &key);
        Ok(principal)
    }
}
#[async_trait::async_trait]
impl UserDetailProvider for Login {
    type User = Device;
    async fn provide_user_detail(&self, principal: &Principal) -> Result<Device, UserDetailError> {
        let denied = || UserDetailError::new("Receive folder unavailable");
        let user = self.user.lock().clone().ok_or_else(denied)?;
        let marker = user.actor.ftp_credential.as_ref().ok_or_else(denied)?;
        if principal.username != format!("{}:{}", marker.id, marker.generation) {
            return Err(denied());
        }
        super::credentials::access(&self.state, &user.actor, false, false).await.map_err(|_| denied())?;
        Ok(user)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_default_gateway_is_read_from_the_routing_table() {
        let routes = [
            "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT",
            "eth0\t00000000\t010011AC\t0003\t0\t0\t0\t00000000\t0\t0\t0",
            "eth0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0",
        ]
        .join("\n");
        assert_eq!(super::default_gateway(&routes), Some(std::net::Ipv4Addr::new(172, 17, 0, 1)));
        assert_eq!(super::default_gateway("Iface\tDestination\tGateway\n"), None);
    }
}
