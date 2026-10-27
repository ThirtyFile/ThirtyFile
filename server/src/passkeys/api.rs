//! The passkey endpoints: signing in with one (alone, or as the second step after a password), and the account's own
//! list of them.
//!
//! A passkey that signs in alone must have checked the person itself (a PIN, a fingerprint, a face): with that it is a
//! password and a second factor in one, so no code is asked for. After a password it needs only to be touched.
//!
//! A passkey is a credential kept by this site, like a password, so it follows the same setting, Control panel › Single
//! sign-on › Password sign-in: where passwords are off, so are passkeys, and the identity provider's rules apply.

use std::net::SocketAddr;

use axum::{
    Json,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    rp::relying_party,
    state::{Memory, Scope},
    store,
    webauthn::{self, RelyingParty, Stored},
};
use crate::{
    auth::{self, IP_FAIL_LIMIT, User, client_ip, limit_key_ip},
    error::{AppError, AppResult},
    logs,
    state::AppState,
    twofactor,
    util::sha256_hex,
};

/// How long a browser is told to wait for the person
const TIMEOUT_MS: u64 = 120_000;
const MAX_NAME: usize = 60;
/// Challenges one address may ask for within the failure window (15 minutes)
const OPTIONS_PER_WINDOW: usize = 300;
/// Wrong second factors for one account within the failure window (as codes)
const ACCOUNT_LIMIT: usize = 10;

fn relying_party_of(st: &AppState) -> AppResult<RelyingParty> {
    relying_party(st).ok_or_else(|| {
        AppError::bad_request("Passkeys need the site address (Control panel › General) to be an https address with a host name, not a number")
            .with_code("passkeys_unavailable")
    })
}

fn decode(value: &str) -> AppResult<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(value).map_err(|_| AppError::bad_request("That is not what a passkey sends"))
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn failed() -> AppError {
    AppError::new(StatusCode::UNAUTHORIZED, "The passkey was not accepted").with_code("passkey_failed")
}

fn too_many() -> AppError {
    AppError::new(StatusCode::TOO_MANY_REQUESTS, "Too many failed sign-in attempts. Try again in 15 minutes.")
}

/// What `navigator.credentials.get` is asked, as the page passes it on
fn request_options(rp: &RelyingParty, challenge: &[u8], allow: &[(Vec<u8>, Vec<String>)], user_verification: &str) -> Value {
    json!({
        "challenge": b64(challenge),
        "rp_id": rp.id,
        "timeout": TIMEOUT_MS,
        "user_verification": user_verification,
        "allow": allow.iter().map(|(id, transports)| json!({ "id": b64(id), "transports": transports })).collect::<Vec<_>>(),
    })
}

// ───────────── Signing in ─────────────

/// Whether passkeys can be offered: they need an address they can belong to
pub fn available(st: &AppState) -> bool {
    relying_party(st).is_some()
}

/// The challenge for signing in with a passkey, without a user name typed
pub async fn login_options(State(st): State<AppState>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap) -> AppResult<Json<Value>> {
    let rp = relying_party_of(&st)?;
    // Challenges are kept in memory, a limited number: one address can't ask for so many that the others' are pushed out
    if !auth::begin_attempt(&st, &format!("pk-options:{}", limit_key_ip(&client_ip(&st, addr, &headers))), OPTIONS_PER_WINDOW) {
        return Err(too_many());
    }
    let challenge = st.part::<Memory>().issue(Scope::Login);
    Ok(Json(request_options(&rp, &challenge, &[], "required")))
}

/// What `navigator.credentials.get` answered (all of it base64url)
#[derive(Deserialize)]
pub struct AssertionReq {
    credential_id: String,
    client_data_json: String,
    authenticator_data: String,
    signature: String,
    #[serde(default)]
    user_handle: Option<String>,
}

/// An answer that checked out: the passkey and what it said
struct Verified {
    passkey: store::Found,
    asserted: webauthn::Asserted,
}

/// Takes the challenge the answer is for (it has to be one given out for `want`) and checks the answer against the
/// passkey it names. `None` for anything wrong: the caller says only that the passkey was not accepted.
async fn verify(st: &AppState, rp: &RelyingParty, req: &AssertionReq, want: impl Fn(&Scope) -> bool) -> AppResult<Option<Verified>> {
    let (client_data, auth_data, signature, credential) =
        (decode(&req.client_data_json)?, decode(&req.authenticator_data)?, decode(&req.signature)?, decode(&req.credential_id)?);
    let Some(challenge) = webauthn::challenge_in(&client_data) else { return Ok(None) };
    let Some((challenge, scope)) = st.part::<Memory>().take(&challenge) else { return Ok(None) };
    if !want(&scope) {
        return Ok(None);
    }
    let Some(passkey) = store::find(&mut *st.db.acquire().await?, &credential).await? else { return Ok(None) };
    if let Some(handle) = req.user_handle.as_deref().filter(|h| !h.is_empty())
        && decode(handle)? != store::user_handle(passkey.user_id)
    {
        return Ok(None);
    }
    let Ok(key) = super::cose::PublicKey::from_cbor(&passkey.public_key) else { return Ok(None) };
    let stored = Stored { key, counter: passkey.counter };
    let Ok(asserted) = webauthn::verify_assertion(rp, &challenge, &client_data, &auth_data, &signature, &stored) else { return Ok(None) };
    Ok(Some(Verified { passkey, asserted }))
}

/// Moves a passkey's counter on; false when another sign-in with the same answer got there first
async fn mark_used(st: &AppState, v: &Verified) -> AppResult<bool> {
    let _w = st.write_lock.lock().await;
    store::used(&mut *st.db.acquire().await?, v.passkey.id, v.passkey.counter, v.asserted.counter, v.asserted.backup_state).await
}

async fn account(st: &AppState, user_id: i64) -> AppResult<Option<(String, bool)>> {
    Ok(sqlx::query_as("SELECT username, disabled FROM users WHERE id = ?").bind(user_id).fetch_optional(&st.db).await?)
}

/// Whether this site lets passkeys be used for sign-in at all: it needs an address they can belong to, and passwords
/// not to be off for everyone
pub async fn offered(st: &AppState) -> AppResult<bool> {
    Ok(available(st) && crate::sso::mode(&st.db).await? != crate::sso::PasswordSignIn::Nobody)
}

/// Signs in with a passkey
pub async fn login(
    State(st): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<AssertionReq>,
) -> AppResult<Response> {
    let rp = relying_party_of(&st)?;
    let ip = client_ip(&st, addr, &headers);
    let ip_key = format!("ip:{}", limit_key_ip(&ip));
    if !auth::begin_attempt(&st, &ip_key, IP_FAIL_LIMIT) {
        return Err(too_many());
    }
    // Alone, a passkey has to have checked the person: without it the touch of whoever holds the device would do
    let verified = verify(&st, &rp, &req, |s| *s == Scope::Login).await?.filter(|v| v.asserted.user_verified);
    let Some(v) = verified else {
        logs::record_login_via(&st, None, "", "passkey_failed", "passkey", &ip, &headers);
        return Err(failed());
    };
    let user_id = v.passkey.user_id;
    let Some((username, disabled)) = account(&st, user_id).await? else { return Err(failed()) };
    if disabled {
        logs::record_login_via(&st, Some(user_id), &username, "disabled", "passkey", &ip, &headers);
        return Err(failed());
    }
    // Where passwords aren't allowed for this person, neither are passkeys
    if !crate::sso::may_use_password(&st, user_id).await? {
        logs::record_login_via(&st, Some(user_id), &username, "password_off", "passkey", &ip, &headers);
        return Err(failed());
    }
    if !mark_used(&st, &v).await? {
        logs::record_login_via(&st, Some(user_id), &username, "passkey_failed", "passkey", &ip, &headers);
        return Err(failed());
    }
    auth::attempt_succeeded(&st, &ip_key);
    crate::signin::finish_login_by(&st, user_id, &username, "passkey", &ip, &headers, None).await
}

// ───────────── The second step after a password ─────────────

#[derive(Deserialize)]
pub struct TicketReq {
    ticket: String,
}

/// The challenge for the second step: for the passkeys of the account the password was right for
pub async fn second_options(State(st): State<AppState>, Json(req): Json<TicketReq>) -> AppResult<Json<Value>> {
    let rp = relying_party_of(&st)?;
    let hash = sha256_hex(req.ticket.as_bytes());
    let (user_id, _, _) = twofactor::ticket(&st, &hash).await?;
    let allow = store::credentials(&mut *st.db.acquire().await?, user_id).await?;
    if allow.is_empty() {
        return Err(AppError::bad_request("This account has no passkeys"));
    }
    let challenge = st.part::<Memory>().issue(Scope::Second { ticket: hash });
    Ok(Json(request_options(&rp, &challenge, &allow, "preferred")))
}

#[derive(Deserialize)]
pub struct SecondReq {
    ticket: String,
    #[serde(flatten)]
    assertion: AssertionReq,
}

/// The second step with a passkey, after a correct password
pub async fn second(State(st): State<AppState>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap, Json(req): Json<SecondReq>) -> AppResult<Response> {
    let rp = relying_party_of(&st)?;
    let ip = client_ip(&st, addr, &headers);
    let hash = sha256_hex(req.ticket.as_bytes());
    let (user_id, _, cred) = twofactor::ticket(&st, &hash).await?;
    let Some((username, false)) = account(&st, user_id).await? else { return Err(failed()) };
    let key = format!("2fa:{user_id}");
    if !auth::begin_attempt(&st, &key, ACCOUNT_LIMIT) {
        return Err(AppError::new(StatusCode::TOO_MANY_REQUESTS, "Too many wrong codes. Try again in 15 minutes."));
    }
    // It has to be a passkey of this account, answering what was given out for this ticket
    let verified = verify(&st, &rp, &req.assertion, |s| matches!(s, Scope::Second { ticket } if *ticket == hash)).await?;
    let verified = verified.filter(|v| v.passkey.user_id == user_id);
    let Some(v) = verified else {
        let used_up = twofactor::count_wrong(&st, &hash);
        logs::record_login_via(&st, Some(user_id), &username, "2fa_failed", "passkey", &ip, &headers);
        return Err(if used_up { twofactor::wrong_codes_message() } else { failed() });
    };
    if !mark_used(&st, &v).await? {
        twofactor::count_wrong(&st, &hash);
        return Err(failed());
    }
    auth::attempt_succeeded(&st, &key);
    twofactor::use_ticket(&st, &hash, user_id, &cred).await?;
    crate::signin::finish_login(&st, user_id, &username, &ip, &headers, None).await
}

// ───────────── My passkeys ─────────────

pub async fn mine(State(st): State<AppState>, user: User) -> AppResult<Json<Value>> {
    let list = store::list(&mut *st.db.acquire().await?, user.id).await?;
    let (hash,): (String,) = sqlx::query_as("SELECT password_hash FROM users WHERE id = ?").bind(user.id).fetch_one(&st.db).await?;
    Ok(Json(json!({ "passkeys": list, "available": available(&st), "has_password": hash != auth::NO_PASSWORD, "max": store::MAX_PER_ACCOUNT })))
}

/// Who is changing the list of passkeys, asked again: a stolen session shouldn't be able to add a way in for good
#[derive(Deserialize, Default)]
pub struct ConfirmReq {
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    code: Option<String>,
}

/// The password (and a code, with two-factor sign-in); an account without a password (single sign-on only) must have
/// signed in a few minutes ago
async fn confirm(st: &AppState, user: &User, req: ConfirmReq) -> AppResult<()> {
    crate::tokens::confirm_identity(st, user, req.password, req.code.as_deref(), crate::admin::SIGN_IN_AGAIN).await
}

fn limit_reached() -> AppError {
    let max = store::MAX_PER_ACCOUNT;
    AppError::bad_request(format!("An account can have {max} passkeys at most"))
}

/// The challenge for adding a passkey: after the password (and the code, with two-factor sign-in)
pub async fn add_options(State(st): State<AppState>, user: User, Json(req): Json<ConfirmReq>) -> AppResult<Json<Value>> {
    let rp = relying_party_of(&st)?;
    confirm(&st, &user, req).await?;
    let mut conn = st.db.acquire().await?;
    if store::count(&mut conn, user.id).await? >= store::MAX_PER_ACCOUNT {
        return Err(limit_reached());
    }
    let existing = store::credentials(&mut conn, user.id).await?;
    let challenge = st.part::<Memory>().issue(Scope::Register { user: user.id });
    let site = st.part::<crate::branding::Memory>().settings.read().site_name.trim().to_string();
    Ok(Json(json!({
        "challenge": b64(&challenge),
        "rp": { "id": rp.id, "name": if site.is_empty() { "ThirtyFile".to_string() } else { site } },
        "user": { "id": b64(&store::user_handle(user.id)), "name": user.username, "display_name": user.username },
        "timeout": TIMEOUT_MS,
        // ES256, EdDSA and RS256: the keys `cose` verifies
        "algorithms": [-7, -8, -257],
        "exclude": existing.iter().map(|(id, transports)| json!({ "id": b64(id), "transports": transports })).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
pub struct AddReq {
    name: String,
    client_data_json: String,
    attestation_object: String,
    #[serde(default)]
    transports: Vec<String>,
}

pub async fn add(
    State(st): State<AppState>,
    user: User,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<AddReq>,
) -> AppResult<Json<Value>> {
    let rp = relying_party_of(&st)?;
    let name = clean_name(&req.name)?;
    let (client_data, attestation) = (decode(&req.client_data_json)?, decode(&req.attestation_object)?);
    let bad = || AppError::bad_request("The passkey could not be added. Try again.").with_code("passkey_failed");
    let challenge = webauthn::challenge_in(&client_data).ok_or_else(bad)?;
    let (challenge, scope) = st.part::<Memory>().take(&challenge).ok_or_else(bad)?;
    if scope != (Scope::Register { user: user.id }) {
        return Err(bad());
    }
    let registered = webauthn::verify_registration(&rp, &challenge, &client_data, &attestation).map_err(|_| bad())?;
    let id = {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        if store::count(&mut tx, user.id).await? >= store::MAX_PER_ACCOUNT {
            return Err(limit_reached());
        }
        if !store::insert(&mut tx, user.id, &name, &registered, &req.transports).await? {
            return Err(AppError::conflict("This passkey is already added"));
        }
        let (id,): (i64,) = sqlx::query_as("SELECT id FROM passkeys WHERE credential_id = ?").bind(&registered.credential_id).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        id
    };
    logs::record_login_via(&st, Some(user.id), &user.username, "passkey_added", "passkey", &client_ip(&st, addr, &headers), &headers);
    Ok(Json(json!({ "id": id })))
}

fn clean_name(name: &str) -> AppResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME || name.chars().any(char::is_control) {
        let max = MAX_NAME;
        return Err(AppError::bad_request(format!("Give the passkey a name of up to {max} characters")));
    }
    Ok(name.to_string())
}

#[derive(Deserialize)]
pub struct RenameReq {
    name: String,
}

pub async fn rename(State(st): State<AppState>, user: User, Path(id): Path<i64>, Json(req): Json<RenameReq>) -> AppResult<Json<Value>> {
    let name = clean_name(&req.name)?;
    let _w = st.write_lock.lock().await;
    if !store::rename(&mut *st.db.acquire().await?, user.id, id, &name).await? {
        return Err(AppError::not_found("Passkey not found"));
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn remove(
    State(st): State<AppState>,
    user: User,
    Path(id): Path<i64>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<ConfirmReq>,
) -> AppResult<Json<Value>> {
    confirm(&st, &user, req).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    // Without a password the passkeys may be the only way in, unless a linked sign-in account is
    let (hash, linked, passkeys): (String, i64, i64) = sqlx::query_as(
        "SELECT password_hash, (SELECT COUNT(*) FROM user_identities WHERE user_id = users.id), (SELECT COUNT(*) FROM passkeys WHERE user_id = users.id)
         FROM users WHERE id = ?",
    )
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await?;
    if hash == auth::NO_PASSWORD && linked == 0 && passkeys <= 1 {
        return Err(AppError::bad_request("This is the only way to sign in to this account. Ask an administrator to set a password first."));
    }
    if !store::delete(&mut tx, user.id, id).await? {
        return Err(AppError::not_found("Passkey not found"));
    }
    tx.commit().await?;
    logs::record_login_via(&st, Some(user.id), &user.username, "passkey_removed", "passkey", &client_ip(&st, addr, &headers), &headers);
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests;
