//! App passwords: tokens people create under "My account › App passwords" for scripts, backups and file clients, and
//! for accounts that only sign in through single sign-on and have no password.
//!
//! A token is `tfa_<id>_<secret>`; only its SHA-256 is stored, and it is shown once when created. It is sent as
//! `Authorization: Bearer <token>`, or as the password of HTTP Basic sign-in (username + app password). Tokens only work
//! on the routes that allow them (file operations, see `allow` in main.rs): the account itself, sign-in methods, sharing
//! and administration always need a browser session. A read-only token is refused for anything but GET and HEAD.

use std::net::SocketAddr;

use axum::{
    Json,
    extract::{ConnectInfo, Path, Request, State},
    http::{HeaderMap, Method, StatusCode, header, request::Parts},
    middleware::Next,
    response::Response,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    auth::{self, User, client_ip, limit_key_ip},
    error::{AppError, AppResult},
    logs,
    state::AppState,
    util::{now, random_token, sha256_hex},
};

const PREFIX: &str = "tfa_";
const ID_LEN: usize = 12;
const SECRET_LEN: usize = 40;
/// App passwords one person may have
pub const MAX_PER_USER: i64 = 50;
const MAX_DAYS: i64 = 3650;

/// Marks a request on a route that accepts app passwords (see `allow`)
#[derive(Clone, Copy)]
pub struct AllowAppPasswords;

/// Middleware of the routes that accept app passwords (file operations): lets the `User` extractor accept them, and
/// makes sure a response to an app password never sets a cookie
pub async fn allow(mut req: Request, next: Next) -> Response {
    let with_credential = credential(req.headers()).is_some();
    req.extensions_mut().insert(AllowAppPasswords);
    let mut res = next.run(req).await;
    if with_credential {
        res.headers_mut().remove(header::SET_COOKIE);
    }
    res
}

pub enum Credential {
    Bearer(String),
    /// HTTP Basic sign-in: the username and an app password (never the account's own password)
    Basic { username: String, password: String },
}

/// The app password a request carries in its Authorization header, if any
pub fn credential(headers: &HeaderMap) -> Option<Credential> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?.trim();
    let (scheme, rest) = value.split_once(' ')?;
    let rest = rest.trim();
    if scheme.eq_ignore_ascii_case("bearer") {
        return Some(Credential::Bearer(rest.to_string()));
    }
    if scheme.eq_ignore_ascii_case("basic") {
        let decoded = String::from_utf8(STANDARD.decode(rest).ok()?).ok()?;
        let (username, password) = decoded.split_once(':')?;
        return Some(Credential::Basic { username: username.to_string(), password: password.to_string() });
    }
    None
}

struct Found {
    id: String,
    user_id: i64,
    username: String,
    scope: String,
    last_used_at: Option<i64>,
    /// The secret is right, the token hasn't expired and the account isn't disabled
    valid: bool,
}

/// Looks the token up by its public id and compares the hash of the whole token in constant time
async fn find(st: &AppState, token: &str) -> AppResult<Option<Found>> {
    use subtle::ConstantTimeEq;
    let Some((id, _)) = token.strip_prefix(PREFIX).and_then(|t| t.split_once('_')) else { return Ok(None) };
    if id.len() != ID_LEN || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Ok(None);
    }
    #[derive(sqlx::FromRow)]
    struct Row {
        user_id: i64,
        username: String,
        token_hash: String,
        scope: String,
        expires_at: Option<i64>,
        last_used_at: Option<i64>,
        disabled: bool,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT t.user_id, u.username, t.token_hash, t.scope, t.expires_at, t.last_used_at, u.disabled
         FROM app_passwords t JOIN users u ON u.id = t.user_id WHERE t.id = ?",
    )
    .bind(id)
    .fetch_optional(&st.db)
    .await?;
    let Some(r) = row else { return Ok(None) };
    let hash = sha256_hex(token.as_bytes());
    let same: bool = hash.len() == r.token_hash.len() && hash.as_bytes().ct_eq(r.token_hash.as_bytes()).into();
    let valid = same && r.expires_at.is_none_or(|e| e > now()) && !r.disabled;
    Ok(Some(Found { id: id.to_string(), user_id: r.user_id, username: r.username, scope: r.scope, last_used_at: r.last_used_at, valid }))
}

/// Signs a request in with an app password (called by the `User` extractor on routes that allow them)
pub async fn authenticate(parts: &Parts, st: &AppState, credential: Credential) -> AppResult<User> {
    let ip = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| client_ip(st, c.0, &parts.headers)).unwrap_or_default();
    // Guessing is limited like signing in: failures per address, the same window and limit as wrong passwords
    let key = format!("app:{}", limit_key_ip(&ip));
    if auth::attempts_exhausted(st, &key, auth::IP_FAIL_LIMIT) {
        return Err(AppError::new(StatusCode::TOO_MANY_REQUESTS, "Too many failed sign-in attempts. Try again in 15 minutes."));
    }
    let (username, token) = match credential {
        Credential::Bearer(token) => (None, token),
        Credential::Basic { username, password } => (Some(username), password),
    };
    let found = find(st, &token).await?;
    // Basic sign-in names the account too: it must be the one the token belongs to
    let right_user = |f: &Found| username.as_deref().is_none_or(|u| u.trim().eq_ignore_ascii_case(&f.username));
    let found = match found {
        Some(f) if f.valid && right_user(&f) => f,
        owner => {
            auth::begin_attempt(st, &key, auth::IP_FAIL_LIMIT);
            let locked = auth::attempts_exhausted(st, &key, auth::IP_FAIL_LIMIT);
            // A known token with a wrong secret (or an expired one) is recorded in its owner's sign-in log
            let user_id = owner.as_ref().map(|f| f.user_id);
            let name = username.or(owner.map(|f| f.username)).unwrap_or_default();
            logs::record_login_via(st, user_id, &name, "app_password_failed", "app_password", &ip, &parts.headers);
            if locked {
                logs::record_login_via(st, user_id, &name, "locked", "app_password", &ip, &parts.headers);
            }
            return Err(AppError::new(StatusCode::UNAUTHORIZED, "Wrong or expired app password"));
        }
    };
    if found.scope == "read" && !matches!(parts.method, Method::GET | Method::HEAD) {
        return Err(AppError::forbidden("This app password can only read files"));
    }
    if found.last_used_at.is_none_or(|t| now() - t >= auth::SESSION_TOUCH) {
        touch(st.clone(), found.id.clone(), ip);
    }
    let mut conn = st.db.acquire().await?;
    auth::user_by_id(st, &mut conn, found.user_id).await?.ok_or_else(AppError::unauthorized)
}

/// Records that an app password was used (in the background, at most every few minutes)
fn touch(st: AppState, id: String, ip: String) {
    tokio::spawn(async move {
        let _w = st.write_lock.lock().await;
        let res = sqlx::query("UPDATE app_passwords SET last_used_at = ?, last_ip = ? WHERE id = ?").bind(now()).bind(ip).bind(&id).execute(&st.db).await;
        if let Err(e) = res {
            tracing::debug!("Couldn't record the use of an app password: {e}");
        }
    });
}

// ───────────── My app passwords ─────────────

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct AppPassword {
    id: String,
    name: String,
    /// read or write
    scope: String,
    created_at: i64,
    expires_at: Option<i64>,
    last_used_at: Option<i64>,
    last_ip: String,
}

const COLS: &str = "id, name, scope, created_at, expires_at, last_used_at, last_ip";

pub async fn list(State(st): State<AppState>, user: User) -> AppResult<Json<Vec<AppPassword>>> {
    let sql = format!("SELECT {COLS} FROM app_passwords WHERE user_id = ? ORDER BY created_at DESC, id");
    Ok(Json(sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(user.id).fetch_all(&st.db).await?))
}

#[derive(Deserialize)]
pub struct CreateReq {
    name: String,
    /// read or write
    scope: String,
    /// Days until it stops working; None = never
    #[serde(default)]
    expires_days: Option<i64>,
}

/// Creates an app password; the token is in the response and can't be shown again
pub async fn create(
    State(st): State<AppState>,
    user: User,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<CreateReq>,
) -> AppResult<Json<Value>> {
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 60 || name.chars().any(char::is_control) {
        return Err(AppError::bad_request("Give the app password a name of at most 60 characters"));
    }
    if !matches!(req.scope.as_str(), "read" | "write") {
        return Err(AppError::bad_request("Choose whether the app password may only read files or also change them"));
    }
    if req.expires_days.is_some_and(|d| !(1..=MAX_DAYS).contains(&d)) {
        return Err(AppError::bad_request("An app password can be valid for 1 to 3650 days"));
    }
    let id = random_token(ID_LEN);
    let token = format!("{PREFIX}{id}_{}", random_token(SECRET_LEN));
    let ts = now();
    {
        let _w = st.write_lock.lock().await;
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM app_passwords WHERE user_id = ?").bind(user.id).fetch_one(&st.db).await?;
        if count >= MAX_PER_USER {
            return Err(AppError::bad_request("You can have at most 50 app passwords. Remove one you no longer use first."));
        }
        sqlx::query("INSERT INTO app_passwords (id, user_id, name, token_hash, scope, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?)")
            .bind(&id)
            .bind(user.id)
            .bind(name)
            .bind(sha256_hex(token.as_bytes()))
            .bind(&req.scope)
            .bind(ts)
            .bind(req.expires_days.map(|d| ts + d * 86400))
            .execute(&st.db)
            .await?;
    }
    logs::record_login_via(&st, Some(user.id), &user.username, "app_password_created", "app_password", &client_ip(&st, addr, &headers), &headers);
    let sql = format!("SELECT {COLS} FROM app_passwords WHERE id = ?");
    let row: AppPassword = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(&id).fetch_one(&st.db).await?;
    Ok(Json(json!({ "token": token, "app_password": row })))
}

pub async fn delete(
    State(st): State<AppState>,
    user: User,
    Path(id): Path<String>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> AppResult<Json<Value>> {
    let removed = {
        let _w = st.write_lock.lock().await;
        sqlx::query("DELETE FROM app_passwords WHERE id = ? AND user_id = ?").bind(&id).bind(user.id).execute(&st.db).await?.rows_affected()
    };
    if removed == 0 {
        return Err(AppError::not_found("App password not found"));
    }
    logs::record_login_via(&st, Some(user.id), &user.username, "app_password_revoked", "app_password", &client_ip(&st, addr, &headers), &headers);
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    fn addr() -> ConnectInfo<SocketAddr> {
        ConnectInfo("203.0.113.9:5000".parse().unwrap())
    }

    async fn new_token(env: &testutil::TestEnv, user: &User, scope: &str, expires_days: Option<i64>) -> (String, String) {
        let req = CreateReq { name: "Backup".into(), scope: scope.into(), expires_days };
        let Json(v) = create(State(env.st.clone()), user.clone(), addr(), HeaderMap::new(), Json(req)).await.unwrap();
        (v["token"].as_str().unwrap().to_string(), v["app_password"]["id"].as_str().unwrap().to_string())
    }

    /// Runs the `User` extractor on a request to a route that accepts app passwords
    async fn with_header(env: &testutil::TestEnv, method: Method, value: &str) -> Result<User, StatusCode> {
        use axum::extract::FromRequestParts;
        let req = axum::http::Request::builder().method(method).header(header::AUTHORIZATION, value).body(()).unwrap();
        let (mut parts, _) = req.into_parts();
        parts.extensions.insert(AllowAppPasswords);
        parts.extensions.insert(addr());
        User::from_request_parts(&mut parts, &env.st).await.map_err(|e| e.status)
    }

    fn basic(user: &str, password: &str) -> String {
        format!("Basic {}", STANDARD.encode(format!("{user}:{password}")))
    }

    #[tokio::test]
    async fn app_passwords_sign_in_with_bearer_or_basic() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let bob = env.user("bob", true).await;
        let (token, id) = new_token(&env, &amy, "write", None).await;
        assert!(token.starts_with("tfa_") && token.len() > 50);

        // Only the hash is stored, and the list never shows the token
        let (hash,): (String,) = sqlx::query_as("SELECT token_hash FROM app_passwords WHERE id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(hash, sha256_hex(token.as_bytes()));
        let Json(list) = super::list(State(env.st.clone()), amy.clone()).await.unwrap();
        let shown = serde_json::to_string(&list).unwrap();
        assert!(!shown.contains(&token) && !shown.contains(&hash) && list.len() == 1);

        assert_eq!(with_header(&env, Method::GET, &format!("Bearer {token}")).await.unwrap().id, amy.id);
        assert_eq!(with_header(&env, Method::PUT, &basic("Amy", &token)).await.unwrap().id, amy.id);
        assert!(with_header(&env, Method::GET, &basic("amy", &token)).await.unwrap().session_id.is_none());
        // Someone else's username, the account's own password, or a changed secret: refused
        assert_eq!(with_header(&env, Method::GET, &basic("bob", &token)).await.unwrap_err(), StatusCode::UNAUTHORIZED);
        assert_eq!(with_header(&env, Method::GET, &basic("amy", testutil::password())).await.unwrap_err(), StatusCode::UNAUTHORIZED);
        let tampered = format!("{}x", &token[..token.len() - 1]);
        assert_eq!(with_header(&env, Method::GET, &format!("Bearer {tampered}")).await.unwrap_err(), StatusCode::UNAUTHORIZED);

        // Only its owner can remove it; afterwards it stops working
        assert_eq!(delete(State(env.st.clone()), bob, Path(id.clone()), addr(), HeaderMap::new()).await.unwrap_err().status, StatusCode::NOT_FOUND);
        let _ = delete(State(env.st.clone()), amy.clone(), Path(id), addr(), HeaderMap::new()).await.unwrap();
        assert_eq!(with_header(&env, Method::GET, &format!("Bearer {token}")).await.unwrap_err(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn read_only_expired_and_disabled() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let (read, _) = new_token(&env, &amy, "read", Some(30)).await;
        assert!(with_header(&env, Method::GET, &format!("Bearer {read}")).await.is_ok());
        assert!(with_header(&env, Method::HEAD, &format!("Bearer {read}")).await.is_ok());
        for m in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert_eq!(with_header(&env, m, &format!("Bearer {read}")).await.unwrap_err(), StatusCode::FORBIDDEN);
        }
        sqlx::query("UPDATE app_passwords SET expires_at = ?").bind(now() - 1).execute(&env.st.db).await.unwrap();
        assert_eq!(with_header(&env, Method::GET, &format!("Bearer {read}")).await.unwrap_err(), StatusCode::UNAUTHORIZED);

        let (write, _) = new_token(&env, &amy, "write", None).await;
        sqlx::query("UPDATE users SET disabled = 1 WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
        assert_eq!(with_header(&env, Method::GET, &format!("Bearer {write}")).await.unwrap_err(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn app_passwords_only_work_where_they_are_allowed() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let (token, _) = new_token(&env, &amy, "write", None).await;
        let req = axum::http::Request::builder().header(header::AUTHORIZATION, format!("Bearer {token}"));
        assert!(env.request_user(req).await.is_none());
    }

    #[tokio::test]
    async fn guessing_app_passwords_is_limited() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let (token, _) = new_token(&env, &amy, "write", None).await;
        for i in 0..auth::IP_FAIL_LIMIT {
            let guess = format!("Bearer tfa_{}_{}", random_token(ID_LEN), i);
            assert_eq!(with_header(&env, Method::GET, &guess).await.unwrap_err(), StatusCode::UNAUTHORIZED);
        }
        // Even the right token is refused from that address for a while
        assert_eq!(with_header(&env, Method::GET, &format!("Bearer {token}")).await.unwrap_err(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn names_scopes_and_expiry_are_checked() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let bad = |name: &str, scope: &str, days: Option<i64>| {
            let req = CreateReq { name: name.into(), scope: scope.into(), expires_days: days };
            create(State(env.st.clone()), amy.clone(), addr(), HeaderMap::new(), Json(req))
        };
        assert!(bad(" ", "read", None).await.is_err());
        assert!(bad(&"x".repeat(61), "read", None).await.is_err());
        assert!(bad("ok", "admin", None).await.is_err());
        assert!(bad("ok", "read", Some(0)).await.is_err());
        assert!(bad("ok", "read", Some(MAX_DAYS + 1)).await.is_err());
        let Json(v) = bad("ok", "read", Some(7)).await.unwrap();
        let expires = v["app_password"]["expires_at"].as_i64().unwrap();
        assert!((expires - now() - 7 * 86400).abs() < 5);
    }
}
