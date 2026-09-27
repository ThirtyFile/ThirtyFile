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

pub async fn verify_password(password: String, hash: String) -> AppResult<bool> {
    let _permit = hash_permits().acquire().await.map_err(AppError::internal)?;
    Ok(tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash)
            .map(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
            .unwrap_or(false)
    })
    .await?)
}

pub fn validate_password(p: &str) -> AppResult<()> {
    if p.chars().count() < 8 {
        return Err(AppError::bad_request("Password must be at least 8 characters"));
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
}

async fn me_of(st: &AppState, user: User) -> AppResult<Me> {
    let used_bytes = tree::used_bytes(&st.db, user.id).await?;
    let (can_create_drive, public_url) = {
        let s = st.system.read().unwrap();
        (user.is_admin() || s.allow_user_drives, s.public_url.clone())
    };
    Ok(Me { user, used_bytes, can_create_drive, public_url })
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

/// The user's IP: the TCP connection address by default; behind a reverse proxy, the **last** address in X-Forwarded-For.
///
/// The last one is the connection address the reverse proxy itself saw; earlier addresses may be client-supplied (nginx's
/// `$proxy_add_x_forwarded_for` appends to the value the client sent), so they can't be used for sign-in rate limiting.
pub fn client_ip(st: &AppState, addr: std::net::SocketAddr, headers: &HeaderMap) -> String {
    if st.trust_proxy
        && let Some(ip) = forwarded_ip(headers)
    {
        return ip;
    }
    addr.ip().to_string()
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
    let key = format!("u:{}|{ip}", username.to_lowercase());
    let ip_key = format!("ip:{ip}");
    // Attempts during the lockout aren't logged individually (one "locked" entry was logged when the lockout began), so the log can't be flooded
    let too_many = AppError::new(axum::http::StatusCode::TOO_MANY_REQUESTS, "Too many failed sign-in attempts. Try again in 15 minutes.");
    if !begin_attempt(&st, &key, FAIL_LIMIT) {
        return Err(too_many);
    }
    if !begin_attempt(&st, &ip_key, IP_FAIL_LIMIT) {
        attempt_succeeded(&st, &key);
        return Err(too_many);
    }
    let row: Option<(i64, String, bool)> = sqlx::query_as("SELECT id, password_hash, disabled FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(&st.db)
        .await?;
    // Hash once even when the username doesn't exist, so response timing doesn't reveal whether it exists
    let (id, hash, disabled) =
        row.unwrap_or((0, "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHQ$dGhpc2lzbm90YXJlYWxoYXNodGhpc2lzbm90".into(), false));
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
    st.login_failures.lock().unwrap().remove(&key);
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
    let (hash,): (String,) =
        sqlx::query_as("SELECT password_hash FROM users WHERE id = ?").bind(user.id).fetch_one(&st.db).await?;
    if !verify_password(req.current, hash).await? {
        return Err(AppError::bad_request("Current password is incorrect"));
    }
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
}

#[cfg(test)]
mod attempt_tests {
    use super::*;
    use crate::testutil;

    #[tokio::test]
    async fn parallel_wrong_passwords_are_counted_before_hashing() {
        let env = testutil::env().await;
        env.user("amy", true).await;
        let addr: std::net::SocketAddr = "10.0.0.1:5000".parse().unwrap();
        // Ten guesses at once: only FAIL_LIMIT of them get to run the hash, the rest are refused right away
        let results = futures_util::future::join_all((0..10).map(|i| {
            let st = env.st.clone();
            async move {
                let req = LoginReq { username: "amy".into(), password: format!("wrong-{i}") };
                login(State(st), ConnectInfo(addr), HeaderMap::new(), Json(req)).await.map(|_| ()).unwrap_err().status
            }
        }))
        .await;
        let refused = results.iter().filter(|s| **s == axum::http::StatusCode::TOO_MANY_REQUESTS).count();
        let checked = results.iter().filter(|s| **s == axum::http::StatusCode::UNAUTHORIZED).count();
        assert_eq!((checked, refused), (FAIL_LIMIT, 10 - FAIL_LIMIT));

        // A correct password from another address isn't counted as a failure
        let other: std::net::SocketAddr = "10.0.0.2:5000".parse().unwrap();
        let req = LoginReq { username: "amy".into(), password: "password-1234".into() };
        assert!(login(State(env.st.clone()), ConnectInfo(other), HeaderMap::new(), Json(req)).await.is_ok());
        let map = env.st.login_failures.lock().unwrap();
        assert!(map.get("u:amy|10.0.0.2").is_none() && map.get("ip:10.0.0.2").is_none(), "{:?}", map.keys().collect::<Vec<_>>());
    }
}
