//! Signing in with a password and out again, changing the password, and what the page learns about the person signed
//! in (`/api/auth/me`). What every request needs (its session or app password, the limits on attempts, password
//! hashing) is in auth/.

use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use crate::{
    auth::{
        DUMMY_HASH, FAIL_LIMIT, IP_FAIL_LIMIT, SESSION_COOKIE, SESSION_TTL, USER_COLS, User, attempt_succeeded, begin_account_attempt, begin_attempt, client_ip,
        confirm_password, cookie_header, get_cookie, hash_password, limit_key_ip, min_password, sign_out_everywhere, user_agent, validate_password, verify_password,
    },
    error::{AppError, AppResult},
    logs,
    state::AppState,
    tree,
    util::{now, random_token, sha256_hex},
};

#[derive(Serialize)]
pub struct Me {
    #[serde(flatten)]
    pub user: User,
    /// Used in their personal space (0 without one)
    pub used_bytes: i64,
    /// Their personal space is waiting for its storage location to be available (personal.rs)
    pub personal_pending: bool,
    /// Can create team spaces
    pub can_create_drive: bool,
    /// The site's public URL (for building share links); blank = use the browser's current URL
    pub public_url: String,
    /// Days before trashed items are deleted for good (0 = kept until the trash is emptied)
    pub trash_days: i64,
    /// Shortest password allowed
    pub min_password_length: usize,
    /// The rules for public share links, so the share dialog offers only what is allowed
    pub share_policy: crate::shares::SharePolicy,
    /// Earlier versions kept per file (0 = replacing a file's content keeps no version)
    pub version_keep: i64,
    /// Largest file that can be edited and saved online (bytes)
    pub max_edit_bytes: usize,
    /// The release this server runs (`dev` for a local build). Only people who are signed in get it.
    pub version: &'static str,
}

async fn me_of(st: &AppState, user: User) -> AppResult<Me> {
    let used_bytes = tree::used_bytes(&st.db, user.id).await?;
    let (personal_pending,): (bool,) =
        sqlx::query_as("SELECT personal_pending IS NOT NULL FROM users WHERE id = ?").bind(user.id).fetch_optional(&st.db).await?.unwrap_or((false,));
    let (can_create_drive, public_url, min_password_length, version_keep) = {
        let s = st.system.read().unwrap();
        (user.is_admin() || s.allow_user_drives, s.public_url.clone(), s.min_password_length, s.version_keep)
    };
    let share_policy = crate::shares::policy(st);
    Ok(Me {
        user,
        used_bytes,
        personal_pending,
        can_create_drive,
        public_url,
        trash_days: st.trash_days,
        min_password_length,
        share_policy,
        version_keep,
        max_edit_bytes: crate::files::MAX_EDIT_BYTES,
        version: crate::VERSION,
    })
}

#[derive(Deserialize)]
pub struct LoginReq {
    pub username: String,
    pub password: String,
}

/// Longest user name a sign-in is looked at with: twice the longest an account may have
const MAX_SIGN_IN_NAME: usize = 64;

pub async fn login(
    State(st): State<AppState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LoginReq>,
) -> AppResult<Response> {
    let ip = client_ip(&st, addr, &headers);
    let username = req.username.trim();
    let limit_ip = limit_key_ip(&ip);
    let ip_key = format!("ip:{limit_ip}");
    // Attempts during the lockout aren't logged individually (one "locked" entry was logged when the lockout began), so the log can't be flooded
    let too_many = AppError::new(axum::http::StatusCode::TOO_MANY_REQUESTS, "Too many failed sign-in attempts. Try again in 15 minutes.");
    // No account has a name this long (`validate_username`): refused before the name becomes a key of the limits kept
    // in memory, or an entry of the log, so long names can't fill either. It counts toward the address's limit.
    if username.chars().count() > MAX_SIGN_IN_NAME {
        if !begin_attempt(&st, &ip_key, IP_FAIL_LIMIT) {
            return Err(too_many);
        }
        return Err(AppError::new(axum::http::StatusCode::UNAUTHORIZED, "Incorrect username or password"));
    }
    // Usernames are unique regardless of the case of A–Z only (`COLLATE NOCASE`), so the keys fold the same letters
    let key = format!("u:{}|{limit_ip}", username.to_ascii_lowercase());
    let account_key = format!("a:{}", username.to_ascii_lowercase());
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
    let row: Option<(i64, String, bool)> =
        sqlx::query_as("SELECT id, password_hash, disabled FROM users WHERE username = ?").bind(username).fetch_optional(&st.db).await?;
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

    // With two-factor sign-in, the password only gets a short-lived ticket for the second step
    if let Some(pending) = crate::twofactor::after_password(&st, id).await? {
        return Ok(Json(pending).into_response());
    }
    finish_login(&st, id, username, &ip, &headers, None).await
}

/// Signs a user in after every step of password sign-in passed: opens the session and answers with the user (and the
/// recovery codes when two-factor sign-in was just set up)
pub async fn finish_login(st: &AppState, id: i64, username: &str, ip: &str, headers: &HeaderMap, recovery_codes: Option<Vec<String>>) -> AppResult<Response> {
    let cookie = open_session(st, id, "password", ip, headers).await?;
    logs::record_login(st, Some(id), username, "login", ip, headers);
    let sql = format!("SELECT {USER_COLS} FROM users u WHERE u.id = ?");
    let mut user: User = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(id).fetch_one(&st.db).await?;
    user.shared_root = st.shared_root();
    let mut body = serde_json::to_value(me_of(st, user).await?).map_err(AppError::internal)?;
    if let Some(codes) = recovery_codes {
        body["recovery_codes"] = serde_json::json!(codes);
    }
    Ok(([(header::SET_COOKIE, cookie)], Json(body)).into_response())
}

/// Creates a sign-in session and updates "last sign-in", returning the cookie to set (shared by password and third-party
/// sign-in). `method` (password or the provider), the address and the browser are shown in the list of signed-in devices.
pub async fn open_session(st: &AppState, user_id: i64, method: &str, ip: &str, headers: &HeaderMap) -> AppResult<String> {
    // A personal space still waiting for its storage location is tried again first, so it is there when the page opens
    crate::personal::retry_pending(st, Some(user_id)).await;
    let token = random_token(43);
    let ts = now();
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    sqlx::query(
        "INSERT INTO sessions (token_hash, id, user_id, created_at, expires_at, user_agent, ip, method, last_used_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(sha256_hex(token.as_bytes()))
    .bind(crate::util::new_id())
    .bind(user_id)
    .bind(ts)
    .bind(ts + SESSION_TTL)
    .bind(user_agent(headers))
    .bind(ip)
    .bind(method)
    .bind(ts)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE users SET last_login_at = ? WHERE id = ?").bind(ts).bind(user_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(cookie_header(st, SESSION_COOKIE, &token, "/", SESSION_TTL))
}

pub async fn logout(State(st): State<AppState>, ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>, headers: HeaderMap) -> AppResult<impl IntoResponse> {
    if let Some(token) = get_cookie(&headers, SESSION_COOKIE) {
        let hash = sha256_hex(token.as_bytes());
        let who: Option<(i64, String)> = sqlx::query_as("SELECT u.id, u.username FROM sessions s JOIN users u ON u.id = s.user_id WHERE s.token_hash = ?")
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
    // The browser drops what it cached for this site (file contents, thumbnails), so the next person at a shared
    // computer doesn't find it there
    Ok(([(header::SET_COOKIE, cookie), (header::HeaderName::from_static("clear-site-data"), "\"cache\"".to_string())], Json(serde_json::json!({ "ok": true }))))
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
    validate_password(&req.new, min_password(&st))?;
    confirm_password(&st, user.id, req.current).await?;
    let new_hash = hash_password(req.new).await?;
    let current_token = get_cookie(&headers, SESSION_COOKIE).map(|t| sha256_hex(t.as_bytes())).unwrap_or_default();
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    sqlx::query("UPDATE users SET password_hash = ?, must_change_password = 0 WHERE id = ?").bind(new_hash).bind(user.id).execute(&mut *tx).await?;
    // Other devices and app passwords stop working: whoever may have known the old password is locked out
    sign_out_everywhere(&mut tx, user.id, Some(&current_token)).await?;
    tx.commit().await?;
    logs::record_login(&st, Some(user.id), &user.username, "password_change", &client_ip(&st, addr, &headers), &headers);
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[cfg(test)]
mod attempt_tests {
    use super::*;
    use crate::{
        auth::{ACCOUNT_FREE_FAILURES, ACCOUNT_MAX_DELAY},
        testutil,
    };

    #[tokio::test]
    async fn a_user_name_longer_than_any_account_has_is_refused_before_it_is_remembered() {
        let env = testutil::env().await;
        let addr: std::net::SocketAddr = "203.0.113.20:5000".parse().unwrap();
        let req = LoginReq { username: "x".repeat(100_000), password: testutil::wrong_password() };
        let err = login(State(env.st.clone()), ConnectInfo(addr), HeaderMap::new(), Json(req)).await.map(|_| ()).unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::UNAUTHORIZED);
        let longest = env.st.login_failures.lock().unwrap().keys().map(String::len).max().unwrap_or(0);
        assert!(longest < 300, "a key of {longest} bytes was kept");
        // It still counts toward the address's limit
        assert!(env.st.login_failures.lock().unwrap().contains_key("ip:203.0.113.20"));
    }

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
