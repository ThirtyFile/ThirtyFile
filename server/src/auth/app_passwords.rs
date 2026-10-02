//! Signing a request in with an app password (tokens.rs: making and listing them). Tokens only work on the routes that
//! allow them (file operations, see `allow` in app/routes.rs), and a read-only token is refused for anything but reading
//! (see `reads_only`).

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Request},
    http::{HeaderMap, Method, StatusCode, header, request::Parts},
    middleware::Next,
    response::Response,
};
use base64::{Engine, engine::general_purpose::STANDARD};

use crate::{
    auth::{self, User, client_ip, limit_key_ip},
    error::{AppError, AppResult},
    logs,
    state::AppState,
    util::{now, sha256_hex},
};

/// A token is `tfa_<id>_<secret>` (tokens.rs)
pub const PREFIX: &str = "tfa_";
pub const ID_LEN: usize = 12;

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
    Basic {
        username: String,
        token: String,
    },
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
        // The password part is an app password: a token, never the account's own password
        let (username, token) = decoded.split_once(':')?;
        return Some(Credential::Basic { username: username.to_string(), token: token.to_string() });
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
        Credential::Basic { username, token } => (Some(username), token),
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
    if found.scope == "read" && !reads_only(&parts.method) {
        return Err(AppError::forbidden("This app password can only read files"));
    }
    if found.last_used_at.is_none_or(|t| now() - t >= auth::SESSION_TOUCH) {
        touch(st.clone(), found.id.clone(), ip);
    }
    let mut conn = st.db.acquire().await?;
    auth::user_by_id(st, &mut conn, found.user_id).await?.ok_or_else(AppError::unauthorized)
}

/// Methods that change nothing: all a read-only app password may use (OPTIONS and PROPFIND are WebDAV's way of
/// asking what is there, see dav/)
pub fn reads_only(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) || method.as_str() == "PROPFIND"
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
