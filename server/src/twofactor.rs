//! Two-factor sign-in: after a correct password, a code from an authenticator app (TOTP, RFC 6238: HMAC-SHA-1, six
//! digits, 30-second steps, one step of clock difference either way, each code accepted once) or a single-use recovery
//! code.
//!
//! Signing in: a correct password gets a short-lived ticket instead of a session (`after_password`); the ticket and a
//! code then open the session (`login_code`). When an administrator requires two-factor sign-in, an account without it
//! uses its ticket to set it up first (`login_setup`), so it is never signed in without a second factor. Single sign-on
//! relies on the provider, and app passwords don't ask for a code (that's their purpose).

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use data_encoding::BASE32_NOPAD;
use hmac::{Hmac, KeyInit, Mac};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha1::Sha1;
use subtle::ConstantTimeEq;

use crate::{
    auth::{self, Admin, User, client_ip},
    error::{AppError, AppResult},
    logs,
    state::AppState,
    util::{now, random_token, sha256_hex},
};

/// What two-factor sign-in keeps in memory (a part of `AppState`)
#[derive(Default)]
pub struct Memory {
    /// Two-factor sign-in being set up from the account menu
    pub setups: SetupMap,
    /// Sign-ins waiting for their second factor
    pub logins: Logins,
}

const STEP: i64 = 30;
/// How long the ticket from a correct password stays valid
const TICKET_TTL: Duration = Duration::from_secs(5 * 60);
/// Wrong codes one ticket may take before the password has to be entered again
const TICKET_TRIES: u32 = 5;
/// Wrong codes for one account (from any ticket or address) within the sign-in failure window (15 minutes)
const ACCOUNT_LIMIT: usize = 10;
/// How long a secret shown for setting up waits for its first code
const SETUP_TTL: Duration = Duration::from_secs(10 * 60);
const RECOVERY_CODES: usize = 10;
/// Tickets kept at most (sign-ins in progress); beyond this the oldest are dropped
const MAX_PENDING: usize = 5000;

// ───────────── Codes ─────────────

/// The six-digit code of one 30-second step
fn code_at(secret: &[u8], step: i64) -> String {
    let mut mac = Hmac::<Sha1>::new_from_slice(secret).expect("HMAC takes keys of any length");
    mac.update(&(step as u64).to_be_bytes());
    let h = mac.finalize().into_bytes();
    let offset = (h[h.len() - 1] & 0x0f) as usize;
    let bin = u32::from_be_bytes([h[offset] & 0x7f, h[offset + 1], h[offset + 2], h[offset + 3]]);
    format!("{:06}", bin % 1_000_000)
}

/// The step a code belongs to (the current one, or one either side for clocks that are a little off), if it is later
/// than `after`: a code accepted once can't be used again, nor can an older one
fn matching_step(secret: &[u8], code: &str, at: i64, after: i64) -> Option<i64> {
    let current = at.div_euclid(STEP);
    let mut found = None;
    // Every candidate is compared, in constant time, so the timing doesn't tell which step matched
    for step in current - 1..=current + 1 {
        let same: bool = code_at(secret, step).as_bytes().ct_eq(code.as_bytes()).into();
        if same && step > after && found.is_none() {
            found = Some(step);
        }
    }
    found
}

/// A new random secret (160 bits, as RFC 4226 recommends), in base32 as authenticator apps expect it
fn new_secret() -> String {
    BASE32_NOPAD.encode(&rand::random::<[u8; 20]>())
}

fn secret_bytes(secret: &str) -> Vec<u8> {
    BASE32_NOPAD.decode(secret.as_bytes()).unwrap_or_default()
}

/// The digits of a code as typed ("123 456" → "123456")
fn digits(code: &str) -> String {
    code.chars().filter(|c| !c.is_whitespace()).collect()
}

/// What an authenticator app needs: the secret as text, the otpauth:// link and a QR code of it (SVG)
fn setup_view(st: &AppState, username: &str, secret: &str) -> AppResult<Value> {
    let issuer = st.part::<crate::branding::Memory>().settings.read().unwrap().site_name.trim().to_string();
    let issuer = if issuer.is_empty() { "ThirtyFile".to_string() } else { issuer };
    let enc = |s: &str| utf8_percent_encode(s, NON_ALPHANUMERIC).to_string();
    let uri = format!("otpauth://totp/{}:{}?secret={secret}&issuer={}&algorithm=SHA1&digits=6&period=30", enc(&issuer), enc(username), enc(&issuer));
    let qr = qrcode::QrCode::new(uri.as_bytes()).map_err(AppError::internal)?;
    let svg = qr.render::<qrcode::render::svg::Color>().min_dimensions(192, 192).build();
    Ok(json!({ "secret": secret, "uri": uri, "qr_svg": svg }))
}

// ───────────── Recovery codes ─────────────

/// 32 letters and digits, without the ones easily mistaken for each other (0/o, 1/l)
const RECOVERY_ALPHABET: &[u8; 32] = b"abcdefghijkmnpqrstuvwxyz23456789";

/// Ten codes like `k7m2p-x9qfa` (50 random bits each)
fn new_recovery_codes() -> Vec<String> {
    (0..RECOVERY_CODES)
        .map(|_| {
            let chars: String = rand::random::<[u8; 10]>().iter().map(|b| RECOVERY_ALPHABET[(b & 31) as usize] as char).collect();
            format!("{}-{}", &chars[..5], &chars[5..])
        })
        .collect()
}

/// A recovery code as typed, reduced to what is hashed: lower case, without the dash and spaces
fn normalize_recovery(code: &str) -> String {
    code.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

async fn store_recovery_codes(conn: &mut sqlx::SqliteConnection, user_id: i64, codes: &[String]) -> AppResult<()> {
    sqlx::query("DELETE FROM recovery_codes WHERE user_id = ?").bind(user_id).execute(&mut *conn).await?;
    for code in codes {
        sqlx::query("INSERT INTO recovery_codes (user_id, code_hash, created_at) VALUES (?, ?, ?)")
            .bind(user_id)
            .bind(sha256_hex(normalize_recovery(code).as_bytes()))
            .bind(now())
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// Uses up a recovery code; false when it isn't one of the account's unused codes
async fn use_recovery_code(st: &AppState, user_id: i64, code: &str) -> AppResult<bool> {
    let norm = normalize_recovery(code);
    if norm.len() != 10 {
        return Ok(false);
    }
    let hash = sha256_hex(norm.as_bytes());
    let unused: Vec<(String,)> =
        sqlx::query_as("SELECT code_hash FROM recovery_codes WHERE user_id = ? AND used_at IS NULL").bind(user_id).fetch_all(&st.db).await?;
    // Compared with every unused code in constant time
    let matched = unused.iter().fold(false, |acc, (h,)| acc | bool::from(h.as_bytes().ct_eq(hash.as_bytes())));
    if !matched {
        return Ok(false);
    }
    let _w = st.write_lock.lock().await;
    let res = sqlx::query("UPDATE recovery_codes SET used_at = ? WHERE user_id = ? AND code_hash = ? AND used_at IS NULL")
        .bind(now())
        .bind(user_id)
        .bind(&hash)
        .execute(&st.db)
        .await?;
    Ok(res.rows_affected() == 1)
}

/// Accepts a code from the authenticator app once; false when it's wrong, too old or already used
async fn use_totp(st: &AppState, user_id: i64, code: &str) -> AppResult<bool> {
    let row: Option<(Option<String>, i64)> =
        sqlx::query_as("SELECT totp_secret, totp_last_step FROM users WHERE id = ?").bind(user_id).fetch_optional(&st.db).await?;
    let Some((Some(stored), last)) = row else { return Ok(false) };
    // Stored encrypted like the other secrets (secrets.rs); one saved with another key can't be checked
    let Ok(secret) = crate::secrets::open(&format!("user:{user_id}:totp"), &stored) else {
        tracing::error!("The two-factor secret of user {user_id} can't be read with the current key; reset their two-factor sign-in");
        return Ok(false);
    };
    let Some(step) = matching_step(&secret_bytes(&secret), &digits(code), now(), last) else { return Ok(false) };
    // Two requests with the same code at once: only one of them moves the step on
    let _w = st.write_lock.lock().await;
    let res = sqlx::query("UPDATE users SET totp_last_step = ? WHERE id = ? AND totp_last_step < ?").bind(step).bind(user_id).bind(step).execute(&st.db).await?;
    Ok(res.rows_affected() == 1)
}

#[derive(Debug, PartialEq)]
enum Accepted {
    Totp,
    Recovery,
}

/// Checks a second-factor code of an account that has two-factor sign-in: six digits from the app, or a recovery code
async fn check_code(st: &AppState, user_id: i64, code: &str) -> AppResult<Option<Accepted>> {
    let d = digits(code);
    if d.len() == 6 && d.chars().all(|c| c.is_ascii_digit()) {
        return Ok(use_totp(st, user_id, &d).await?.then_some(Accepted::Totp));
    }
    Ok(use_recovery_code(st, user_id, code).await?.then_some(Accepted::Recovery))
}

/// Before a sensitive change by a signed-in user (creating an app password): when their account has two-factor
/// sign-in, a code from the app or a recovery code must come with it. Limited like codes at sign-in.
pub async fn confirm_code(st: &AppState, user_id: i64, code: Option<&str>) -> AppResult<()> {
    let (secret,): (Option<String>,) = sqlx::query_as("SELECT totp_secret FROM users WHERE id = ?").bind(user_id).fetch_one(&st.db).await?;
    if secret.is_none() {
        return Ok(());
    }
    let Some(code) = code.filter(|c| !c.trim().is_empty()) else {
        return Err(AppError::bad_request("Enter a code from your authenticator app").with_code("two_factor_code"));
    };
    let key = format!("2fa:{user_id}");
    if !auth::begin_attempt(st, &key, ACCOUNT_LIMIT) {
        return Err(too_many());
    }
    if check_code(st, user_id, code).await?.is_none() {
        return Err(AppError::bad_request("Wrong code. Check that the time on your phone is right, and try again.").with_code("two_factor_code"));
    }
    auth::attempt_succeeded(st, &key);
    Ok(())
}

/// Turns two-factor sign-in on with a confirmed secret, returning new recovery codes
async fn turn_on(st: &AppState, user_id: i64, secret: &str, step: i64) -> AppResult<Vec<String>> {
    let codes = new_recovery_codes();
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    sqlx::query("UPDATE users SET totp_secret = ?, totp_last_step = ? WHERE id = ?")
        .bind(crate::secrets::seal(&format!("user:{user_id}:totp"), secret))
        .bind(step)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    store_recovery_codes(&mut tx, user_id, &codes).await?;
    tx.commit().await?;
    Ok(codes)
}

async fn turn_off(st: &AppState, user_id: i64) -> AppResult<bool> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let off = turn_off_in(&mut tx, user_id).await?;
    tx.commit().await?;
    Ok(off)
}

/// Turns two-factor sign-in off for an account and removes its recovery codes (also `thirtyfile reset-two-factor`):
/// whether it was on
pub async fn turn_off_in(conn: &mut sqlx::SqliteConnection, user_id: i64) -> AppResult<bool> {
    let res =
        sqlx::query("UPDATE users SET totp_secret = NULL, totp_last_step = 0 WHERE id = ? AND totp_secret IS NOT NULL").bind(user_id).execute(&mut *conn).await?;
    sqlx::query("DELETE FROM recovery_codes WHERE user_id = ?").bind(user_id).execute(&mut *conn).await?;
    Ok(res.rows_affected() > 0)
}

fn too_many() -> AppError {
    AppError::new(StatusCode::TOO_MANY_REQUESTS, "Too many wrong codes. Try again in 15 minutes.")
}

// ───────────── Signing in ─────────────

#[derive(Clone)]
pub enum Step {
    /// Two-factor sign-in is set up: a code is asked for
    Code,
    /// Required but not set up yet: the secret shown for setting it up, once asked for
    Setup(Option<String>),
}

/// A correct password waiting for its second factor
pub struct Pending {
    user_id: i64,
    step: Step,
    /// The password the ticket was given for (`credential`): once it changes or is reset, the ticket stops working
    credential: String,
    created: Instant,
    tries: u32,
}

/// What a ticket is bound to: a digest of the account's stored password hash, which every password change and reset
/// replaces. None for an account that is gone or disabled.
async fn credential(st: &AppState, user_id: i64) -> AppResult<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as("SELECT password_hash FROM users WHERE id = ? AND disabled = 0").bind(user_id).fetch_optional(&st.db).await?;
    Ok(row.map(|(h,)| sha256_hex(h.as_bytes())))
}

/// Sign-ins in progress, by the SHA-256 of their ticket (the ticket itself is only known to the browser)
pub type Logins = Mutex<HashMap<String, Pending>>;

/// After a correct password: when the account has (or must set up) two-factor sign-in, a ticket for the second step
/// instead of a session
pub async fn after_password(st: &AppState, user_id: i64) -> AppResult<Option<Value>> {
    let (secret,): (Option<String>,) = sqlx::query_as("SELECT totp_secret FROM users WHERE id = ?").bind(user_id).fetch_one(&st.db).await?;
    let step = match secret {
        Some(_) => Step::Code,
        None if st.system.read().unwrap().require_two_factor => Step::Setup(None),
        None => return Ok(None),
    };
    let kind = if matches!(step, Step::Code) { "code" } else { "setup" };
    let Some(credential) = credential(st, user_id).await? else { return Err(expired()) };
    let ticket = random_token(43);
    {
        let mut map = st.part::<Memory>().logins.lock().unwrap();
        map.retain(|_, p| p.created.elapsed() < TICKET_TTL);
        while map.len() >= MAX_PENDING {
            let Some(oldest) = map.iter().min_by_key(|(_, p)| p.created).map(|(k, _)| k.clone()) else { break };
            map.remove(&oldest);
        }
        map.insert(sha256_hex(ticket.as_bytes()), Pending { user_id, step, credential, created: Instant::now(), tries: 0 });
    }
    Ok(Some(json!({ "two_factor": kind, "ticket": ticket })))
}

fn expired() -> AppError {
    AppError::new(StatusCode::UNAUTHORIZED, "The sign-in has expired. Enter your password again.").with_code("two_factor_expired")
}

/// The account, step and credential of a ticket that is still valid: not too old, and given for the account's current
/// password (a password changed or reset since, e.g. to lock someone out, ends the sign-ins it started)
async fn ticket(st: &AppState, hash: &str) -> AppResult<(i64, Step, String)> {
    let found = {
        let mut map = st.part::<Memory>().logins.lock().unwrap();
        match map.get(hash) {
            Some(p) if p.created.elapsed() < TICKET_TTL => Some((p.user_id, p.step.clone(), p.credential.clone())),
            _ => {
                map.remove(hash);
                None
            }
        }
    };
    let Some((user_id, step, cred)) = found else { return Err(expired()) };
    if !still_valid(st, user_id, &cred).await? {
        st.part::<Memory>().logins.lock().unwrap().remove(hash);
        return Err(expired());
    }
    Ok((user_id, step, cred))
}

/// Whether the account still has the password a ticket was given for
async fn still_valid(st: &AppState, user_id: i64, cred: &str) -> AppResult<bool> {
    Ok(credential(st, user_id).await?.is_some_and(|c| c.as_bytes().ct_eq(cred.as_bytes()).into()))
}

#[derive(Deserialize)]
pub struct TicketReq {
    ticket: String,
}

/// Sign-in of an account that must set up two-factor sign-in: the secret to add to the authenticator app
pub async fn login_setup(State(st): State<AppState>, Json(req): Json<TicketReq>) -> AppResult<Json<Value>> {
    let hash = sha256_hex(req.ticket.as_bytes());
    let (user_id, step, _) = ticket(&st, &hash).await?;
    let Step::Setup(existing) = step else { return Err(AppError::bad_request("Two-factor sign-in is already set up for this account")) };
    let secret = match existing {
        Some(s) => s,
        None => {
            let s = new_secret();
            if let Some(p) = st.part::<Memory>().logins.lock().unwrap().get_mut(&hash) {
                p.step = Step::Setup(Some(s.clone()));
            }
            s
        }
    };
    let (username,): (String,) = sqlx::query_as("SELECT username FROM users WHERE id = ?").bind(user_id).fetch_one(&st.db).await?;
    Ok(Json(setup_view(&st, &username, &secret)?))
}

#[derive(Deserialize)]
pub struct CodeReq {
    ticket: String,
    code: String,
}

/// The second step of password sign-in: a code (or recovery code) with the ticket from the password. When setting up,
/// the answer also carries the new recovery codes.
pub async fn login_code(
    State(st): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<CodeReq>,
) -> AppResult<Response> {
    let ip = client_ip(&st, addr, &headers);
    let hash = sha256_hex(req.ticket.as_bytes());
    let (user_id, step, cred) = ticket(&st, &hash).await?;
    let row: Option<(String, bool)> = sqlx::query_as("SELECT username, disabled FROM users WHERE id = ?").bind(user_id).fetch_optional(&st.db).await?;
    let Some((username, false)) = row else {
        st.part::<Memory>().logins.lock().unwrap().remove(&hash);
        return Err(expired());
    };
    // Counted before the check, like passwords; a right code takes it back
    let key = format!("2fa:{user_id}");
    if !auth::begin_attempt(&st, &key, ACCOUNT_LIMIT) {
        return Err(too_many());
    }
    let (accepted, setup) = match &step {
        Step::Code => (check_code(&st, user_id, &req.code).await?, None),
        Step::Setup(Some(secret)) => {
            let step = matching_step(&secret_bytes(secret), &digits(&req.code), now(), 0);
            (step.map(|_| Accepted::Totp), step.map(|s| (secret.clone(), s)))
        }
        Step::Setup(None) => {
            auth::attempt_succeeded(&st, &key);
            return Err(AppError::bad_request("Set up two-factor sign-in first"));
        }
    };
    let Some(accepted) = accepted else {
        let ticket_used_up = {
            let mut map = st.part::<Memory>().logins.lock().unwrap();
            let used_up = map.get_mut(&hash).is_none_or(|p| {
                p.tries += 1;
                p.tries >= TICKET_TRIES
            });
            if used_up {
                map.remove(&hash);
            }
            used_up
        };
        logs::record_login(&st, Some(user_id), &username, "2fa_failed", &ip, &headers);
        if auth::attempts_exhausted(&st, &key, ACCOUNT_LIMIT) {
            logs::record_login(&st, Some(user_id), &username, "locked", &ip, &headers);
        }
        return Err(if ticket_used_up {
            AppError::new(StatusCode::UNAUTHORIZED, "Too many wrong codes. Enter your password again.").with_code("two_factor_expired")
        } else {
            AppError::new(StatusCode::UNAUTHORIZED, "Wrong code")
        });
    };
    auth::attempt_succeeded(&st, &key);
    // A ticket signs in once
    if st.part::<Memory>().logins.lock().unwrap().remove(&hash).is_none() {
        return Err(expired());
    }
    // The password may have changed while the code was checked
    if !still_valid(&st, user_id, &cred).await? {
        return Err(expired());
    }
    let codes = match setup {
        Some((secret, step)) => {
            let codes = turn_on(&st, user_id, &secret, step).await?;
            logs::record_login(&st, Some(user_id), &username, "2fa_enabled", &ip, &headers);
            Some(codes)
        }
        None => None,
    };
    if accepted == Accepted::Recovery {
        logs::record_login(&st, Some(user_id), &username, "recovery_code_used", &ip, &headers);
    }
    crate::signin::finish_login(&st, user_id, &username, &ip, &headers, codes).await
}

// ───────────── My two-factor sign-in ─────────────

/// Secrets shown for setting up from the account menu, waiting for their first code: user → (secret, shown at)
pub type SetupMap = Mutex<HashMap<i64, (String, Instant)>>;

#[derive(Serialize)]
pub struct Status {
    enabled: bool,
    recovery_codes_left: i64,
    /// The administrator requires it for password accounts
    required: bool,
    /// Accounts without a password (single sign-on only) have nothing for a second factor to protect
    has_password: bool,
}

pub async fn status(State(st): State<AppState>, user: User) -> AppResult<Json<Status>> {
    let (enabled, left, hash): (bool, i64, String) = sqlx::query_as(
        "SELECT totp_secret IS NOT NULL, (SELECT COUNT(*) FROM recovery_codes WHERE user_id = users.id AND used_at IS NULL), password_hash
         FROM users WHERE id = ?",
    )
    .bind(user.id)
    .fetch_one(&st.db)
    .await?;
    let required = st.system.read().unwrap().require_two_factor;
    Ok(Json(Status { enabled, recovery_codes_left: left, required, has_password: hash != crate::auth::NO_PASSWORD }))
}

#[derive(Deserialize)]
pub struct PasswordReq {
    password: String,
    /// Once two-factor sign-in is on, a code from the app (or a recovery code) too: the password alone doesn't
    /// replace the app, renew the recovery codes or turn it off
    #[serde(default)]
    code: Option<String>,
}

/// Starts setting up (or replacing the authenticator app): after the password, a new secret to add to the app
pub async fn start_setup(State(st): State<AppState>, user: User, Json(req): Json<PasswordReq>) -> AppResult<Json<Value>> {
    let (hash,): (String,) = sqlx::query_as("SELECT password_hash FROM users WHERE id = ?").bind(user.id).fetch_one(&st.db).await?;
    if hash == crate::auth::NO_PASSWORD {
        return Err(AppError::bad_request("This account has no password: it signs in with single sign-on, whose own two-factor sign-in applies."));
    }
    auth::confirm_password(&st, user.id, req.password).await?;
    confirm_code(&st, user.id, req.code.as_deref()).await?;
    let secret = new_secret();
    {
        let mut map = st.part::<Memory>().setups.lock().unwrap();
        map.retain(|_, (_, at)| at.elapsed() < SETUP_TTL);
        map.insert(user.id, (secret.clone(), Instant::now()));
    }
    Ok(Json(setup_view(&st, &user.username, &secret)?))
}

#[derive(Deserialize)]
pub struct EnableReq {
    code: String,
}

/// Confirms the setup with a code from the app: turns two-factor sign-in on and returns the recovery codes
pub async fn enable(
    State(st): State<AppState>,
    user: User,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<EnableReq>,
) -> AppResult<Json<Value>> {
    let secret = st.part::<Memory>().setups.lock().unwrap().get(&user.id).filter(|(_, at)| at.elapsed() < SETUP_TTL).map(|(s, _)| s.clone());
    let Some(secret) = secret else { return Err(AppError::bad_request("The setup has expired. Start again.")) };
    let key = format!("2fa:{}", user.id);
    if !auth::begin_attempt(&st, &key, ACCOUNT_LIMIT) {
        return Err(too_many());
    }
    let Some(step) = matching_step(&secret_bytes(&secret), &digits(&req.code), now(), 0) else {
        return Err(AppError::bad_request("Wrong code. Check that the time on your phone is right, and try again."));
    };
    auth::attempt_succeeded(&st, &key);
    st.part::<Memory>().setups.lock().unwrap().remove(&user.id);
    let codes = turn_on(&st, user.id, &secret, step).await?;
    logs::record_login(&st, Some(user.id), &user.username, "2fa_enabled", &client_ip(&st, addr, &headers), &headers);
    Ok(Json(json!({ "recovery_codes": codes })))
}

pub async fn disable(
    State(st): State<AppState>,
    user: User,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PasswordReq>,
) -> AppResult<Json<Value>> {
    if st.system.read().unwrap().require_two_factor {
        return Err(AppError::bad_request("Your administrator requires two-factor sign-in, so it can't be turned off."));
    }
    auth::confirm_password(&st, user.id, req.password).await?;
    confirm_code(&st, user.id, req.code.as_deref()).await?;
    if turn_off(&st, user.id).await? {
        logs::record_login(&st, Some(user.id), &user.username, "2fa_disabled", &client_ip(&st, addr, &headers), &headers);
    }
    Ok(Json(json!({ "ok": true })))
}

/// Replaces the recovery codes (the old ones stop working)
pub async fn new_recovery_codes_for_me(
    State(st): State<AppState>,
    user: User,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PasswordReq>,
) -> AppResult<Json<Value>> {
    let (enabled,): (bool,) = sqlx::query_as("SELECT totp_secret IS NOT NULL FROM users WHERE id = ?").bind(user.id).fetch_one(&st.db).await?;
    if !enabled {
        return Err(AppError::bad_request("Two-factor sign-in isn't turned on"));
    }
    auth::confirm_password(&st, user.id, req.password).await?;
    confirm_code(&st, user.id, req.code.as_deref()).await?;
    let codes = new_recovery_codes();
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        store_recovery_codes(&mut tx, user.id, &codes).await?;
        tx.commit().await?;
    }
    logs::record_login(&st, Some(user.id), &user.username, "recovery_codes_new", &client_ip(&st, addr, &headers), &headers);
    Ok(Json(json!({ "recovery_codes": codes })))
}

// ───────────── Administration ─────────────

/// Turns a user's two-factor sign-in off (a lost phone and recovery codes). With the requirement on, they set it up
/// again the next time they sign in.
pub async fn admin_reset(
    State(st): State<AppState>,
    Admin(me): Admin,
    Path(user_id): Path<i64>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> AppResult<Json<Value>> {
    let row: Option<(String,)> = sqlx::query_as("SELECT username FROM users WHERE id = ?").bind(user_id).fetch_optional(&st.db).await?;
    let Some((username,)) = row else { return Err(AppError::not_found("User not found")) };
    if turn_off(&st, user_id).await? {
        {
            let _w = st.write_lock.lock().await;
            let mut conn = st.db.acquire().await?;
            // Whatever may have been signed in with the lost phone stops working: signed-in devices and app passwords
            crate::auth::sign_out_everywhere(&mut conn, user_id, None).await?;
            logs::record_activity(&mut conn, &me, None, "user_update", &format!("{username}: reset two-factor sign-in")).await?;
        }
        logs::record_login(&st, Some(user_id), &username, "2fa_reset", &client_ip(&st, addr, &headers), &headers);
    }
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{signin::LoginReq, testutil};

    fn addr() -> ConnectInfo<SocketAddr> {
        ConnectInfo("203.0.113.9:5000".parse().unwrap())
    }

    async fn body(res: Response) -> Value {
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn login(env: &testutil::TestEnv, name: &str) -> Response {
        let req = LoginReq { username: name.into(), password: testutil::password().into() };
        crate::signin::login(State(env.st.clone()), addr(), HeaderMap::new(), Json(req)).await.unwrap()
    }

    async fn code(env: &testutil::TestEnv, ticket: &str, code: &str) -> Result<Response, AppError> {
        let req = CodeReq { ticket: ticket.into(), code: code.into() };
        login_code(State(env.st.clone()), addr(), HeaderMap::new(), Json(req)).await
    }

    /// Sets two-factor sign-in up from the account menu, returning the secret and the recovery codes
    async fn set_up(env: &testutil::TestEnv, user: &User) -> (Vec<u8>, Vec<String>) {
        let Json(v) = start_setup(State(env.st.clone()), user.clone(), Json(PasswordReq { password: testutil::password().into(), code: None })).await.unwrap();
        let secret = secret_bytes(v["secret"].as_str().unwrap());
        assert!(v["uri"].as_str().unwrap().starts_with("otpauth://totp/"));
        assert!(v["qr_svg"].as_str().unwrap().contains("<svg"));
        let now_code = code_at(&secret, now().div_euclid(STEP));
        let Json(v) = enable(State(env.st.clone()), user.clone(), addr(), HeaderMap::new(), Json(EnableReq { code: now_code })).await.unwrap();
        let codes: Vec<String> = serde_json::from_value(v["recovery_codes"].clone()).unwrap();
        (secret, codes)
    }

    /// Turns two-factor sign-in on for `user`; returns the recovery codes
    pub(crate) async fn set_up_for(env: &testutil::TestEnv, user: &User) -> Vec<String> {
        set_up(env, user).await.1
    }

    fn next_code(secret: &[u8]) -> String {
        code_at(secret, now().div_euclid(STEP) + 1)
    }

    #[test]
    fn codes_follow_rfc_6238() {
        // The SHA-1 test vectors of RFC 6238 (last six of the eight digits)
        let secret = b"12345678901234567890";
        assert_eq!(code_at(secret, 59 / STEP), "287082");
        assert_eq!(code_at(secret, 1111111109 / STEP), "081804");
        assert_eq!(code_at(secret, 1234567890 / STEP), "005924");
        // One step of clock difference either way, and never a step already used
        let at = 1111111109;
        let code = code_at(secret, at / STEP + 1);
        assert_eq!(matching_step(secret, &code, at, 0), Some(at / STEP + 1));
        assert_eq!(matching_step(secret, &code, at + 3 * STEP, 0), None);
        assert_eq!(matching_step(secret, &code, at + 2 * STEP, 0), Some(at / STEP + 1));
        assert_eq!(matching_step(secret, &code, at, at / STEP + 1), None);
        assert_eq!(matching_step(secret, "12345", at, 0), None);
    }

    #[test]
    fn recovery_codes_are_readable_and_distinct() {
        let codes = new_recovery_codes();
        assert_eq!(codes.len(), 10);
        assert!(codes.iter().all(|c| c.len() == 11 && c.as_bytes()[5] == b'-'));
        assert_eq!(normalize_recovery(" K7M2P-X9QFA "), "k7m2px9qfa");
        let unique: std::collections::HashSet<_> = codes.iter().collect();
        assert_eq!(unique.len(), 10);
    }

    #[tokio::test]
    async fn sign_in_asks_for_a_code_after_the_password() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let (secret, recovery) = set_up(&env, &amy).await;
        let (stored,): (String,) =
            sqlx::query_as("SELECT code_hash FROM recovery_codes WHERE user_id = ? LIMIT 1").bind(amy.id).fetch_one(&env.st.db).await.unwrap();
        assert!(!recovery.iter().any(|c| c == &stored), "recovery codes are stored hashed");

        // The password alone gets a ticket, not a session
        let res = login(&env, "amy").await;
        assert!(!res.headers().contains_key(axum::http::header::SET_COOKIE));
        let v = body(res).await;
        assert_eq!(v["two_factor"], "code");
        let ticket = v["ticket"].as_str().unwrap().to_string();
        assert_eq!(code(&env, &ticket, "000000").await.map(|_| ()).unwrap_err().status, StatusCode::UNAUTHORIZED);
        let good = next_code(&secret);
        let res = code(&env, &ticket, &good).await.unwrap();
        assert!(res.headers().contains_key(axum::http::header::SET_COOKIE));
        assert_eq!(body(res).await["username"], "amy");
        // The ticket worked once, and so does the code
        assert_eq!(code(&env, &ticket, &good).await.map(|_| ()).unwrap_err().code, Some("two_factor_expired"));
        let ticket = body(login(&env, "amy").await).await["ticket"].as_str().unwrap().to_string();
        assert!(code(&env, &ticket, &good).await.is_err());

        // A recovery code (typed in capitals) works once
        let ticket = body(login(&env, "amy").await).await["ticket"].as_str().unwrap().to_string();
        assert!(code(&env, &ticket, &recovery[0].to_uppercase()).await.is_ok());
        let ticket = body(login(&env, "amy").await).await["ticket"].as_str().unwrap().to_string();
        assert!(code(&env, &ticket, &recovery[0]).await.is_err());
        let Json(s) = status(State(env.st.clone()), amy).await.unwrap();
        assert!(s.enabled && s.recovery_codes_left == 9);
    }

    #[tokio::test]
    async fn guessing_codes_is_limited() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let (secret, _) = set_up(&env, &amy).await;
        // One ticket takes five wrong codes, then the password is needed again
        let ticket = body(login(&env, "amy").await).await["ticket"].as_str().unwrap().to_string();
        for _ in 0..TICKET_TRIES - 1 {
            assert_eq!(code(&env, &ticket, "abcde-fghij").await.map(|_| ()).unwrap_err().code, None);
        }
        assert_eq!(code(&env, &ticket, "abcde-fghij").await.map(|_| ()).unwrap_err().code, Some("two_factor_expired"));
        assert!(code(&env, &ticket, &next_code(&secret)).await.is_err());
        // Across tickets, the account takes ten within the window; then even the right code waits
        for _ in 0..ACCOUNT_LIMIT - TICKET_TRIES as usize {
            let ticket = body(login(&env, "amy").await).await["ticket"].as_str().unwrap().to_string();
            assert_eq!(code(&env, &ticket, "999999").await.map(|_| ()).unwrap_err().status, StatusCode::UNAUTHORIZED);
        }
        let ticket = body(login(&env, "amy").await).await["ticket"].as_str().unwrap().to_string();
        assert_eq!(code(&env, &ticket, &next_code(&secret)).await.map(|_| ()).unwrap_err().status, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn required_two_factor_is_set_up_before_signing_in() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let amy = env.user("amy", true).await;
        let settings = serde_json::from_value(json!({ "require_two_factor": true })).unwrap();
        let _ = crate::admin::update_settings(State(env.st.clone()), Admin(admin.clone()), Json(settings)).await.unwrap();

        let v = body(login(&env, "amy").await).await;
        assert_eq!(v["two_factor"], "setup");
        let ticket = v["ticket"].as_str().unwrap().to_string();
        // No session until the app is set up and a code confirms it
        assert!(code(&env, &ticket, "123456").await.is_err());
        let Json(view) = login_setup(State(env.st.clone()), Json(TicketReq { ticket: ticket.clone() })).await.unwrap();
        let secret = secret_bytes(view["secret"].as_str().unwrap());
        // Asking again shows the same secret
        let Json(again) = login_setup(State(env.st.clone()), Json(TicketReq { ticket: ticket.clone() })).await.unwrap();
        assert_eq!(again["secret"], view["secret"]);
        let res = code(&env, &ticket, &code_at(&secret, now().div_euclid(STEP))).await.unwrap();
        assert!(res.headers().contains_key(axum::http::header::SET_COOKIE));
        let v = body(res).await;
        assert_eq!(v["recovery_codes"].as_array().unwrap().len(), 10);
        let Json(s) = status(State(env.st.clone()), amy.clone()).await.unwrap();
        assert!(s.enabled && s.required);

        // It can't be turned off while required; an administrator can reset it
        let off =
            disable(State(env.st.clone()), amy.clone(), addr(), HeaderMap::new(), Json(PasswordReq { password: testutil::password().into(), code: None })).await;
        assert!(off.is_err());
        let _ = admin_reset(State(env.st.clone()), Admin(admin), Path(amy.id), addr(), HeaderMap::new()).await.unwrap();
        let Json(s) = status(State(env.st.clone()), amy).await.unwrap();
        assert!(!s.enabled && s.recovery_codes_left == 0);
        let (detail,): (String,) =
            sqlx::query_as("SELECT detail FROM activity WHERE action = 'user_update' ORDER BY id DESC LIMIT 1").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(detail, "amy: reset two-factor sign-in");
    }

    #[tokio::test]
    async fn changes_need_the_password_and_a_code() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let req = |password: &str, code: Option<&str>| Json(PasswordReq { password: password.into(), code: code.map(Into::into) });
        let (pw, wrong) = (testutil::password(), testutil::wrong_password());
        assert!(start_setup(State(env.st.clone()), amy.clone(), req(&wrong, None)).await.is_err());
        let (secret, old) = set_up(&env, &amy).await;
        // Once it is on, the password alone doesn't replace the app, renew the codes or turn it off
        assert!(start_setup(State(env.st.clone()), amy.clone(), req(pw, None)).await.is_err());
        assert!(new_recovery_codes_for_me(State(env.st.clone()), amy.clone(), addr(), HeaderMap::new(), req(pw, None)).await.is_err());
        assert!(disable(State(env.st.clone()), amy.clone(), addr(), HeaderMap::new(), req(pw, None)).await.is_err());
        assert!(new_recovery_codes_for_me(State(env.st.clone()), amy.clone(), addr(), HeaderMap::new(), req(&wrong, Some(&old[0]))).await.is_err());
        let Json(v) = new_recovery_codes_for_me(State(env.st.clone()), amy.clone(), addr(), HeaderMap::new(), req(pw, Some(&old[0]))).await.unwrap();
        assert_ne!(v["recovery_codes"][0].as_str().unwrap(), old[0]);
        // The old recovery codes stopped working
        assert!(!use_recovery_code(&env.st, amy.id, &old[1]).await.unwrap());
        // Replacing the app: a code from the current one
        assert!(start_setup(State(env.st.clone()), amy.clone(), req(pw, Some(&next_code(&secret)))).await.is_ok());
        let fresh = v["recovery_codes"][0].as_str().unwrap();
        assert!(disable(State(env.st.clone()), amy.clone(), addr(), HeaderMap::new(), req(&wrong, Some(fresh))).await.is_err());
        let _ = disable(State(env.st.clone()), amy.clone(), addr(), HeaderMap::new(), req(pw, Some(fresh))).await.unwrap();
        // Without it, the password signs in straight away
        let res = login(&env, "amy").await;
        assert!(res.headers().contains_key(axum::http::header::SET_COOKIE));
    }

    #[tokio::test]
    async fn accounts_without_a_password_have_nothing_to_set_up() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?").bind(crate::auth::NO_PASSWORD).bind(amy.id).execute(&env.st.db).await.unwrap();
        let err = start_setup(State(env.st.clone()), amy.clone(), Json(PasswordReq { password: String::new(), code: None })).await.unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        let Json(s) = status(State(env.st.clone()), amy).await.unwrap();
        assert!(!s.has_password);
    }

    #[tokio::test]
    async fn app_passwords_skip_the_second_factor() {
        use axum::extract::FromRequestParts;
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let (_, codes) = set_up(&env, &amy).await;
        let req = serde_json::from_value(json!({ "name": "Backup", "scope": "read", "password": testutil::password(), "code": codes[0] })).unwrap();
        let Json(v) = crate::tokens::create(State(env.st.clone()), amy.clone(), addr(), HeaderMap::new(), Json(req)).await.unwrap();
        let token = v["token"].as_str().unwrap();
        let req = axum::http::Request::builder().header(axum::http::header::AUTHORIZATION, format!("Bearer {token}")).body(()).unwrap();
        let (mut parts, _) = req.into_parts();
        parts.extensions.insert(crate::auth::app_passwords::AllowAppPasswords);
        assert_eq!(User::from_request_parts(&mut parts, &env.st).await.unwrap().id, amy.id);
    }
}
