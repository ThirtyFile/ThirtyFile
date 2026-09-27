use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use axum::{
    Json,
    extract::{ConnectInfo, FromRequestParts, State},
    http::{HeaderMap, header, request::Parts},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};

use crate::{
    error::{AppError, AppResult},
    logs,
    state::AppState,
    tree,
    util::{now, random_token, sha256_hex},
};

pub const SESSION_COOKIE: &str = "tf_session";
const SESSION_TTL: i64 = 30 * 24 * 3600;
const FAIL_WINDOW: i64 = 15 * 60;
/// Limit on consecutive failures for one username from one IP (counted per username + IP, so an attacker can't use it to lock the real user out from elsewhere)
const FAIL_LIMIT: usize = 5;
/// Limit on total failures from one IP within the time window (blocks attempts against many usernames)
const IP_FAIL_LIMIT: usize = 30;
/// Failures against one account (from any address) before each further attempt has to wait
const ACCOUNT_FREE_FAILURES: usize = 10;
/// The longest wait between attempts against one account; the account itself is never locked
const ACCOUNT_MAX_DELAY: i64 = 60;

pub async fn hash_password(password: String) -> AppResult<String> {
    tokio::task::spawn_blocking(move || {
        // password-hash generates a random 16-byte salt from the OS RNG
        Argon2::default().hash_password(password.as_bytes()).map(|h| h.to_string()).map_err(AppError::internal)
    })
    .await?
}

/// Whether the cookie carries `expected` (an HMAC value): compared in constant time, so response timing can't be used to guess it
pub fn cookie_matches(headers: &HeaderMap, name: &str, expected: &str) -> bool {
    use subtle::ConstantTimeEq;
    get_cookie(headers, name).is_some_and(|v| v.len() == expected.len() && v.as_bytes().ct_eq(expected.as_bytes()).into())
}

/// Password hashing runs on at most this many threads at once: a flood of sign-in attempts queues instead of taking
/// every blocking thread (and all CPU cores) for ~100 ms each
fn hash_permits() -> &'static tokio::sync::Semaphore {
    static S: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    S.get_or_init(|| tokio::sync::Semaphore::new(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(2, 8)))
}

/// A valid hash no password matches, checked instead of a missing or unusable one, so the time a check takes doesn't
/// reveal whether an account exists or signs in only through single sign-on
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHQ$dGhpc2lzbm90YXJlYWxoYXNodGhpc2lzbm90";

pub async fn verify_password(password: String, hash: String) -> AppResult<bool> {
    let _permit = hash_permits().acquire().await.map_err(AppError::internal)?;
    Ok(tokio::task::spawn_blocking(move || {
        let (usable, parsed) = match PasswordHash::new(&hash) {
            Ok(p) => (true, p),
            Err(_) => (false, PasswordHash::new(DUMMY_HASH).expect("valid dummy hash")),
        };
        // Always run the check: `usable && …` would skip it and answer at once
        let matches = Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok();
        usable && matches
    })
    .await?)
}

pub fn validate_password(p: &str) -> AppResult<()> {
    if p.chars().count() < 6 {
        return Err(AppError::bad_request("Password must be at least 6 characters"));
    }
    Ok(())
}

pub fn get_cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

pub fn cookie_header(st: &AppState, name: &str, value: &str, path: &str, max_age: i64) -> String {
    let secure = if st.secure_cookie { "; Secure" } else { "" };
    format!("{name}={value}; Path={path}; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}")
}

pub const USER_COLS: &str = "u.id, u.username, u.display_name, u.role, u.can_write, u.can_delete, u.can_share, u.quota_bytes, u.root_id";

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct User {
    pub id: i64,
    pub username: String,
    /// Shown next to the username; may be blank
    pub display_name: String,
    pub role: String,
    pub can_write: bool,
    pub can_delete: bool,
    pub can_share: bool,
    pub quota_bytes: i64,
    pub root_id: String,
    /// Root folder id of the shared space; None when the shared space is disabled
    #[sqlx(skip)]
    pub shared_root: Option<String>,
}

impl User {
    pub fn is_admin(&self) -> bool {
        self.role == "admin"
    }
}

impl FromRequestParts<AppState> for User {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        let token = get_cookie(&parts.headers, SESSION_COOKIE).ok_or_else(AppError::unauthorized)?;
        let sql = format!(
            "SELECT {USER_COLS} FROM sessions s JOIN users u ON u.id = s.user_id
             WHERE s.token_hash = ? AND s.expires_at > ? AND u.disabled = 0"
        );
        let mut user = sqlx::query_as::<_, User>(sqlx::AssertSqlSafe(sql.as_str()))
            .bind(sha256_hex(token.as_bytes()))
            .bind(now())
            .fetch_optional(&st.db)
            .await?
            .ok_or_else(AppError::unauthorized)?;
        user.shared_root = st.shared_root();
        Ok(user)
    }
}

/// Loads a (non-disabled) user by id, for permission checks that don't go through a sign-in session, e.g. public share links
pub async fn user_by_id(st: &AppState, conn: &mut sqlx::SqliteConnection, id: i64) -> AppResult<Option<User>> {
    let sql = format!("SELECT {USER_COLS} FROM users u WHERE u.id = ? AND u.disabled = 0");
    let mut user = sqlx::query_as::<_, User>(sqlx::AssertSqlSafe(sql.as_str())).bind(id).fetch_optional(conn).await?;
    if let Some(u) = user.as_mut() {
        u.shared_root = st.shared_root();
    }
    Ok(user)
}

/// Extractor that only allows administrators
pub struct Admin(pub User);

impl FromRequestParts<AppState> for Admin {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        let user = User::from_request_parts(parts, st).await?;
        if !user.is_admin() {
            return Err(AppError::forbidden("Administrator permission required"));
        }
        Ok(Admin(user))
    }
}

#[derive(Serialize)]
pub struct Me {
    #[serde(flatten)]
    pub user: User,
    pub used_bytes: i64,
    /// Can create team spaces
    pub can_create_drive: bool,
    /// The site's public URL (for building share links); blank = use the browser's current URL
    pub public_url: String,
    /// Days before trashed items are deleted for good (0 = kept until the trash is emptied)
    pub trash_days: i64,
}

async fn me_of(st: &AppState, user: User) -> AppResult<Me> {
    let used_bytes = tree::used_bytes(&st.db, user.id).await?;
    let (can_create_drive, public_url) = {
        let s = st.system.read().unwrap();
        (user.is_admin() || s.allow_user_drives, s.public_url.clone())
    };
    Ok(Me { user, used_bytes, can_create_drive, public_url, trash_days: st.trash_days })
}

#[derive(Deserialize)]
pub struct LoginReq {
    username: String,
    password: String,
}

/// Counts an attempt against `key` *before* the password is checked, so parallel requests can't all slip past the
/// limit while the first one is still hashing. Returns false (nothing recorded) when the limit is already reached.
/// A successful attempt is taken back with `attempt_succeeded`.
pub fn begin_attempt(st: &AppState, key: &str, limit: usize) -> bool {
    let mut map = st.login_failures.lock().unwrap();
    let cutoff = now() - FAIL_WINDOW;
    let list = map.entry(key.to_string()).or_default();
    list.retain(|t| *t > cutoff);
    if list.len() >= limit {
        return false;
    }
    list.push(now());
    true
}

/// Counts an attempt against one account from any address. After `ACCOUNT_FREE_FAILURES` failures within the window,
/// each attempt must wait twice as long after the previous one as the one before (up to `ACCOUNT_MAX_DELAY`), which
/// slows down guessing from many addresses without letting anyone lock the real user out. Returns the seconds to wait.
pub fn begin_account_attempt(st: &AppState, key: &str) -> Result<(), i64> {
    let mut map = st.login_failures.lock().unwrap();
    let ts = now();
    let list = map.entry(key.to_string()).or_default();
    list.retain(|t| *t > ts - FAIL_WINDOW);
    if list.len() >= ACCOUNT_FREE_FAILURES {
        let extra = (list.len() - ACCOUNT_FREE_FAILURES).min(6) as u32;
        let delay = (1i64 << extra).min(ACCOUNT_MAX_DELAY);
        let since = ts - list.last().copied().unwrap_or(0);
        if since < delay {
            return Err(delay - since);
        }
    }
    list.push(ts);
    Ok(())
}

/// Removes the attempt recorded by `begin_attempt` (the password was right)
pub fn attempt_succeeded(st: &AppState, key: &str) {
    let mut map = st.login_failures.lock().unwrap();
    if let Some(list) = map.get_mut(key) {
        list.pop();
        if list.is_empty() {
            map.remove(key);
        }
    }
}

/// Clears expired failed sign-in records (runs periodically, so large numbers of random usernames can't exhaust memory)
pub fn prune_login_failures(st: &AppState) {
    let cutoff = now() - FAIL_WINDOW;
    st.login_failures.lock().unwrap().retain(|_, list| {
        list.retain(|t| *t > cutoff);
        !list.is_empty()
    });
}

/// Which reverse proxies may tell us the visitor's address (`THIRTYFILE_TRUST_PROXY`)
#[derive(Clone, Debug, Default, PartialEq)]
pub enum TrustProxy {
    /// X-Forwarded-For is ignored
    #[default]
    Off,
    /// `true`: proxies connecting from a private or loopback address (the same host or Docker network)
    Private,
    /// A list of addresses or networks (`203.0.113.7, 10.1.0.0/16`)
    Listed(Vec<(std::net::IpAddr, u8)>),
}

impl TrustProxy {
    pub fn parse(value: &str) -> Result<TrustProxy, String> {
        let v = value.trim().to_ascii_lowercase();
        match v.as_str() {
            "" | "false" | "0" | "no" | "off" => return Ok(TrustProxy::Off),
            "true" | "1" | "yes" | "on" => return Ok(TrustProxy::Private),
            _ => {}
        }
        let invalid = |part: &str| format!("not true, false, or a list of addresses and networks: {part:?}");
        let mut list = Vec::new();
        for part in v.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (ip, bits) = part.split_once('/').unwrap_or((part, ""));
            let ip: std::net::IpAddr = ip.parse().map_err(|_| invalid(part))?;
            let max = if ip.is_ipv4() { 32 } else { 128 };
            let bits = if bits.is_empty() { max } else { bits.parse::<u8>().ok().filter(|b| *b <= max).ok_or_else(|| invalid(part))? };
            list.push((ip.to_canonical(), bits));
        }
        Ok(TrustProxy::Listed(list))
    }

    pub fn enabled(&self) -> bool {
        *self != TrustProxy::Off
    }

    /// Whether a connection from `peer` may set the visitor's address
    pub fn trusts(&self, peer: std::net::IpAddr) -> bool {
        let peer = peer.to_canonical();
        match self {
            TrustProxy::Off => false,
            TrustProxy::Private => match peer {
                std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
                std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
            },
            TrustProxy::Listed(list) => list.iter().any(|(net, bits)| in_network(peer, *net, *bits)),
        }
    }
}

fn in_network(ip: std::net::IpAddr, net: std::net::IpAddr, bits: u8) -> bool {
    let mask = |len: u32, bits: u8| if bits == 0 { 0u128 } else { u128::MAX << (len - bits as u32) };
    match (ip, net) {
        (std::net::IpAddr::V4(a), std::net::IpAddr::V4(b)) => {
            let m = mask(32, bits) as u32;
            u32::from(a) & m == u32::from(b) & m
        }
        (std::net::IpAddr::V6(a), std::net::IpAddr::V6(b)) => {
            let m = mask(128, bits);
            u128::from(a) & m == u128::from(b) & m
        }
        _ => false,
    }
}

/// The user's IP: the TCP connection address by default; when the connection comes from a trusted reverse proxy, the
/// **last** address in X-Forwarded-For.
///
/// The last one is the connection address the reverse proxy itself saw; earlier addresses may be client-supplied (nginx's
/// `$proxy_add_x_forwarded_for` appends to the value the client sent), so they can't be used for sign-in rate limiting.
/// Connections from anywhere else can't set it, so publishing the port directly doesn't let visitors choose their address.
pub fn client_ip(st: &AppState, addr: std::net::SocketAddr, headers: &HeaderMap) -> String {
    if st.trust_proxy.trusts(addr.ip())
        && let Some(ip) = forwarded_ip(headers)
    {
        return ip;
    }
    addr.ip().to_canonical().to_string()
}

/// The part of an address used for rate limits: IPv6 addresses count per /64, the block a single connection usually gets
pub fn limit_key_ip(ip: &str) -> String {
    match ip.parse::<std::net::IpAddr>().map(|a| a.to_canonical()) {
        Ok(std::net::IpAddr::V6(v6)) => {
            let prefix = std::net::Ipv6Addr::from(u128::from(v6) & (u128::MAX << 64));
            format!("{prefix}/64")
        }
        Ok(v4) => v4.to_string(),
        Err(_) => ip.to_string(),
    }
}

fn forwarded_ip(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .next_back()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

pub async fn login(
    State(st): State<AppState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LoginReq>,
) -> AppResult<impl IntoResponse> {
    let ip = client_ip(&st, addr, &headers);
    let username = req.username.trim();
    let limit_ip = limit_key_ip(&ip);
    // Usernames are unique regardless of the case of A–Z only (`COLLATE NOCASE`), so the keys fold the same letters
    let key = format!("u:{}|{limit_ip}", username.to_ascii_lowercase());
    let ip_key = format!("ip:{limit_ip}");
    let account_key = format!("a:{}", username.to_ascii_lowercase());
    // Attempts during the lockout aren't logged individually (one "locked" entry was logged when the lockout began), so the log can't be flooded
    let too_many = AppError::new(axum::http::StatusCode::TOO_MANY_REQUESTS, "Too many failed sign-in attempts. Try again in 15 minutes.");
    if !begin_attempt(&st, &key, FAIL_LIMIT) {
        return Err(too_many);
    }
    if !begin_attempt(&st, &ip_key, IP_FAIL_LIMIT) {
        attempt_succeeded(&st, &key);
        return Err(too_many);
    }
    if let Err(wait) = begin_account_attempt(&st, &account_key) {
        attempt_succeeded(&st, &key);
        attempt_succeeded(&st, &ip_key);
        return Err(AppError::new(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            format!("Too many failed sign-in attempts for this account. Try again in {wait} seconds."),
        ));
    }
    let row: Option<(i64, String, bool)> = sqlx::query_as("SELECT id, password_hash, disabled FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(&st.db)
        .await?;
    // Hash once even when the username doesn't exist, so response timing doesn't reveal whether it exists
    let (id, hash, disabled) = row.unwrap_or((0, DUMMY_HASH.into(), false));
    let password_ok = verify_password(req.password, hash).await?;
    if !password_ok || id == 0 || disabled {
        // The attempt was already counted; the lockout begins when this failure was the last one allowed
        let locked = {
            let map = st.login_failures.lock().unwrap();
            map.get(&key).is_some_and(|l| l.len() == FAIL_LIMIT) || map.get(&ip_key).is_some_and(|l| l.len() == IP_FAIL_LIMIT)
        };
        let event = if id == 0 {
            "unknown_user"
        } else if !password_ok {
            "bad_password"
        } else {
            "disabled"
        };
        let user_id = (id > 0).then_some(id);
        logs::record_login(&st, user_id, username, event, &ip, &headers);
        if locked {
            logs::record_login(&st, user_id, username, "locked", &ip, &headers);
        }
        // The response doesn't distinguish the reason, to avoid revealing whether the username exists
        return Err(AppError::new(axum::http::StatusCode::UNAUTHORIZED, "Incorrect username or password"));
    }
    {
        let mut map = st.login_failures.lock().unwrap();
        map.remove(&key);
        map.remove(&account_key);
    }
    attempt_succeeded(&st, &ip_key);

    let cookie = open_session(&st, id).await?;
    logs::record_login(&st, Some(id), username, "login", &ip, &headers);
    let sql = format!("SELECT {USER_COLS} FROM users u WHERE u.id = ?");
    let mut user: User = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(id).fetch_one(&st.db).await?;
    user.shared_root = st.shared_root();
    Ok(([(header::SET_COOKIE, cookie)], Json(me_of(&st, user).await?)))
}

/// Creates a sign-in session and updates "last sign-in", returning the cookie to set (shared by password and third-party sign-in)
pub async fn open_session(st: &AppState, user_id: i64) -> AppResult<String> {
    let token = random_token(43);
    let ts = now();
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    sqlx::query("INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES (?, ?, ?, ?)")
        .bind(sha256_hex(token.as_bytes()))
        .bind(user_id)
        .bind(ts)
        .bind(ts + SESSION_TTL)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE users SET last_login_at = ? WHERE id = ?").bind(ts).bind(user_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(cookie_header(st, SESSION_COOKIE, &token, "/", SESSION_TTL))
}

pub async fn logout(
    State(st): State<AppState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
) -> AppResult<impl IntoResponse> {
    if let Some(token) = get_cookie(&headers, SESSION_COOKIE) {
        let hash = sha256_hex(token.as_bytes());
        let who: Option<(i64, String)> =
            sqlx::query_as("SELECT u.id, u.username FROM sessions s JOIN users u ON u.id = s.user_id WHERE s.token_hash = ?")
                .bind(&hash)
                .fetch_optional(&st.db)
                .await?;
        {
            let _w = st.write_lock.lock().await;
            sqlx::query("DELETE FROM sessions WHERE token_hash = ?").bind(&hash).execute(&st.db).await?;
        }
        if let Some((id, username)) = who {
            logs::record_login(&st, Some(id), &username, "logout", &client_ip(&st, addr, &headers), &headers);
        }
    }
    let cookie = cookie_header(&st, SESSION_COOKIE, "", "/", 0);
    Ok(([(header::SET_COOKIE, cookie)], Json(serde_json::json!({ "ok": true }))))
}

pub async fn me(State(st): State<AppState>, user: User) -> AppResult<Json<Me>> {
    Ok(Json(me_of(&st, user).await?))
}

#[derive(Deserialize)]
pub struct ChangePasswordReq {
    current: String,
    new: String,
}

pub async fn change_password(
    State(st): State<AppState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    user: User,
    Json(req): Json<ChangePasswordReq>,
) -> AppResult<Json<serde_json::Value>> {
    validate_password(&req.new)?;
    // Limited like signing in, so a session in the wrong hands can't be used to guess the password itself
    let key = format!("pw:{}", user.id);
    if !begin_attempt(&st, &key, FAIL_LIMIT) {
        return Err(AppError::new(axum::http::StatusCode::TOO_MANY_REQUESTS, "Too many failed attempts. Try again in 15 minutes."));
    }
    let (hash,): (String,) =
        sqlx::query_as("SELECT password_hash FROM users WHERE id = ?").bind(user.id).fetch_one(&st.db).await?;
    if !verify_password(req.current, hash).await? {
        return Err(AppError::bad_request("Current password is incorrect"));
    }
    st.login_failures.lock().unwrap().remove(&key);
    let new_hash = hash_password(req.new).await?;
    let current_token = get_cookie(&headers, SESSION_COOKIE).map(|t| sha256_hex(t.as_bytes())).unwrap_or_default();
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?").bind(new_hash).bind(user.id).execute(&mut *tx).await?;
    // Sign out other devices
    sqlx::query("DELETE FROM sessions WHERE user_id = ? AND token_hash != ?")
        .bind(user.id)
        .bind(current_token)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    logs::record_login(&st, Some(user.id), &user.username, "password_change", &client_ip(&st, addr, &headers), &headers);
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwarded_ip_uses_the_address_added_by_the_proxy() {
        let mut h = HeaderMap::new();
        assert_eq!(forwarded_ip(&h), None);
        // The client supplies 1.2.3.4 itself; nginx appends the actual connection address
        h.insert("x-forwarded-for", "1.2.3.4, 203.0.113.9".parse().unwrap());
        assert_eq!(forwarded_ip(&h).as_deref(), Some("203.0.113.9"));
        h.insert("x-forwarded-for", "198.51.100.7".parse().unwrap());
        assert_eq!(forwarded_ip(&h).as_deref(), Some("198.51.100.7"));
    }

    #[test]
    fn only_trusted_proxies_may_set_the_visitor_address() {
        let ip = |s: &str| s.parse::<std::net::IpAddr>().unwrap();
        for off in ["false", "", "0", "No"] {
            assert_eq!(TrustProxy::parse(off), Ok(TrustProxy::Off));
        }
        let private = TrustProxy::parse("TRUE").unwrap();
        assert_eq!(private, TrustProxy::Private);
        // The same host, a Docker network or a LAN proxy; not a visitor connecting straight to the published port
        for peer in ["127.0.0.1", "172.17.0.1", "10.0.0.5", "192.168.1.2", "::1", "fd00::1", "::ffff:172.18.0.3"] {
            assert!(private.trusts(ip(peer)), "{peer}");
        }
        for peer in ["203.0.113.9", "8.8.8.8", "2001:db8::1", "::ffff:203.0.113.9"] {
            assert!(!private.trusts(ip(peer)), "{peer}");
        }
        let listed = TrustProxy::parse("203.0.113.7, 198.51.100.0/24, 2001:db8::/32").unwrap();
        assert!(listed.trusts(ip("203.0.113.7")) && listed.trusts(ip("198.51.100.200")) && listed.trusts(ip("2001:db8:1::5")));
        assert!(!listed.trusts(ip("203.0.113.8")) && !listed.trusts(ip("10.0.0.1")));
        assert!(TrustProxy::parse("proxy.example.com").is_err());
        assert!(TrustProxy::parse("10.0.0.0/33").is_err());
        assert!(!TrustProxy::Off.trusts(ip("127.0.0.1")));
    }

    #[tokio::test]
    async fn accounts_without_a_password_take_as_long_to_check() {
        let time = |hash: &'static str| async move {
            let start = std::time::Instant::now();
            let ok = verify_password(crate::testutil::wrong_password(), hash.into()).await.unwrap();
            (ok, start.elapsed())
        };
        let (ok, real) = time(DUMMY_HASH).await;
        assert!(!ok);
        // Accounts that only sign in through single sign-on store "!" as their hash
        let (ok, none) = time("!").await;
        assert!(!ok);
        assert!(none * 3 > real, "an account without a password answers much faster: {none:?} vs {real:?}");
    }

    #[test]
    fn ipv6_addresses_share_one_limit_per_64_block() {
        assert_eq!(limit_key_ip("2001:db8:1:2:aaaa::1"), limit_key_ip("2001:db8:1:2:bbbb::9"));
        assert_ne!(limit_key_ip("2001:db8:1:2::1"), limit_key_ip("2001:db8:1:3::1"));
        assert_eq!(limit_key_ip("203.0.113.9"), "203.0.113.9");
        assert_eq!(limit_key_ip("::ffff:203.0.113.9"), "203.0.113.9");
    }
}

#[cfg(test)]
mod attempt_tests {
    use super::*;
    use crate::testutil;

    #[tokio::test]
    async fn guessing_one_account_from_many_addresses_slows_down() {
        let env = testutil::env().await;
        env.user("amy", true).await;
        let wrong = |i: u32| {
            let st = env.st.clone();
            async move {
                // A different address every time, so only the per-account limit applies
                let addr: std::net::SocketAddr = format!("203.0.{}.{}:5000", i / 250, i % 250 + 1).parse().unwrap();
                let req = LoginReq { username: "Amy".into(), password: testutil::wrong_password() };
                login(State(st), ConnectInfo(addr), HeaderMap::new(), Json(req)).await.map(|_| ()).unwrap_err().status
            }
        };
        for i in 0..ACCOUNT_FREE_FAILURES as u32 {
            assert_eq!(wrong(i).await, axum::http::StatusCode::UNAUTHORIZED);
        }
        // Every address counted against the one account; the next attempt right away has to wait (hashing may be slow
        // on a busy test machine, so the last failure is dated now rather than relying on timing)
        {
            let mut map = env.st.login_failures.lock().unwrap();
            let list = map.get_mut("a:amy").unwrap();
            assert_eq!(list.len(), ACCOUNT_FREE_FAILURES);
            *list.last_mut().unwrap() = now();
        }
        assert_eq!(wrong(100).await, axum::http::StatusCode::TOO_MANY_REQUESTS);
        // After the wait, the right password works and the count starts over
        env.st.login_failures.lock().unwrap().get_mut("a:amy").unwrap().iter_mut().for_each(|t| *t -= ACCOUNT_MAX_DELAY);
        let addr: std::net::SocketAddr = "198.51.100.1:5000".parse().unwrap();
        let req = LoginReq { username: "amy".into(), password: testutil::password().into() };
        assert!(login(State(env.st.clone()), ConnectInfo(addr), HeaderMap::new(), Json(req)).await.is_ok());
        assert!(!env.st.login_failures.lock().unwrap().contains_key("a:amy"));
    }

    #[tokio::test]
    async fn changing_the_password_is_limited_like_signing_in() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let addr: std::net::SocketAddr = "10.0.0.1:5000".parse().unwrap();
        let change = |current: &str| {
            let req = ChangePasswordReq { current: current.into(), new: testutil::wrong_password() };
            change_password(State(env.st.clone()), ConnectInfo(addr), HeaderMap::new(), amy.clone(), Json(req))
        };
        for _ in 0..FAIL_LIMIT {
            assert_eq!(change(&testutil::wrong_password()).await.unwrap_err().status, axum::http::StatusCode::BAD_REQUEST);
        }
        assert_eq!(change(testutil::password()).await.unwrap_err().status, axum::http::StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn parallel_wrong_passwords_are_counted_before_hashing() {
        let env = testutil::env().await;
        env.user("amy", true).await;
        let addr: std::net::SocketAddr = "10.0.0.1:5000".parse().unwrap();
        // Ten guesses at once: only FAIL_LIMIT of them get to run the hash, the rest are refused right away
        let results = futures_util::future::join_all((0..10).map(|_| {
            let st = env.st.clone();
            async move {
                let req = LoginReq { username: "amy".into(), password: testutil::wrong_password() };
                login(State(st), ConnectInfo(addr), HeaderMap::new(), Json(req)).await.map(|_| ()).unwrap_err().status
            }
        }))
        .await;
        let refused = results.iter().filter(|s| **s == axum::http::StatusCode::TOO_MANY_REQUESTS).count();
        let checked = results.iter().filter(|s| **s == axum::http::StatusCode::UNAUTHORIZED).count();
        assert_eq!((checked, refused), (FAIL_LIMIT, 10 - FAIL_LIMIT));

        // A correct password from another address isn't counted as a failure
        let other: std::net::SocketAddr = "10.0.0.2:5000".parse().unwrap();
        let req = LoginReq { username: "amy".into(), password: testutil::password().into() };
        assert!(login(State(env.st.clone()), ConnectInfo(other), HeaderMap::new(), Json(req)).await.is_ok());
        let map = env.st.login_failures.lock().unwrap();
        assert!(map.get("u:amy|10.0.0.2").is_none() && map.get("ip:10.0.0.2").is_none(), "{:?}", map.keys().collect::<Vec<_>>());
    }
}
