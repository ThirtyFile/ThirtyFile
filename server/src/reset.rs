//! Resetting a forgotten password by email, and setting one's own password after an administrator chose it.
//!
//! - "Forgot password" on the sign-in page (only while an email server is set up): the person enters their username or
//!   email address, and an account with a password and an email address gets a single-use link valid for an hour. The
//!   answer is the same whether or not such an account exists, and requests are limited per address and per account.
//! - The link sets a new password; every sign-in and app password of the account ends, as when an administrator resets
//!   it.
//! - A password an administrator chose (a new account, a reset) must be changed at the next sign-in: until then the
//!   session only reaches what changing it needs (`auth::User`).

use std::net::SocketAddr;

use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::HeaderMap,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    auth::{self, client_ip, hash_password, min_password, validate_password},
    error::{AppError, AppResult},
    logs,
    state::AppState,
    util::{now, random_token, sha256_hex},
};

/// How long a link works
const LINK_TTL: i64 = 3600;
/// Requests per address, and per account, in the sign-in failure window (15 minutes)
const PER_ADDRESS: usize = 5;
const PER_ACCOUNT: usize = 3;
/// Wrong links per address in the same window
const WRONG_LINKS: usize = 20;

/// Where reset links point: the "Site URL" setting. Never the address a request came with, which whoever sends the
/// request chooses; without the setting, resetting by email isn't offered.
fn link_base(st: &AppState) -> Option<String> {
    Some(st.system.read().unwrap().public_url.clone()).filter(|u| !u.is_empty())
}

/// What the sign-in page offers
pub async fn options(State(st): State<AppState>) -> AppResult<Json<Value>> {
    Ok(Json(json!({ "password_reset": crate::mail::load(&st.db).await.enabled && link_base(&st).is_some() })))
}

#[derive(Deserialize)]
pub struct ForgotReq {
    /// A username or an email address
    account: String,
}

pub async fn forgot(
    State(st): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<ForgotReq>,
) -> AppResult<Json<Value>> {
    let smtp = crate::mail::load(&st.db).await;
    let Some(base) = link_base(&st).filter(|_| smtp.enabled) else {
        return Err(AppError::bad_request("Ask your administrator to reset your password"));
    };
    let ip = client_ip(&st, addr, &headers);
    if !auth::begin_attempt(&st, &format!("forgot-ip:{}", auth::limit_key_ip(&ip)), PER_ADDRESS) {
        return Err(AppError::new(axum::http::StatusCode::TOO_MANY_REQUESTS, "Too many requests. Try again in 15 minutes."));
    }
    let account = req.account.trim();
    // Accounts that sign in with a password (not only through single sign-on) and have somewhere to send it
    let found: Option<(i64, String, String, String, String)> = sqlx::query_as(
        "SELECT id, username, email, chosen_lang, lang FROM users
         WHERE (username = ?1 OR (email != '' AND lower(email) = lower(?1))) AND disabled = 0 AND password_hash != ?2 AND email != ''
         ORDER BY username = ?1 DESC LIMIT 1",
    )
    .bind(account)
    .bind(crate::auth::NO_PASSWORD)
    .fetch_optional(&st.db)
    .await?;
    if let Some((id, username, email, chosen_lang, last_lang)) = found
        && auth::begin_attempt(&st, &format!("forgot:{id}"), PER_ACCOUNT)
    {
        let token = random_token(43);
        let ts = now();
        {
            let _w = st.write_lock.lock().await;
            let mut tx = crate::db::begin_write(&st.db).await?;
            // Only the newest link works
            sqlx::query("DELETE FROM password_resets WHERE user_id = ? OR expires_at < ?").bind(id).bind(ts).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO password_resets (token_hash, user_id, created_at, expires_at) VALUES (?, ?, ?, ?)")
                .bind(sha256_hex(token.as_bytes()))
                .bind(id)
                .bind(ts)
                .bind(ts + LINK_TTL)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
        logs::record_login(&st, Some(id), &username, "password_reset_requested", &ip, &headers);
        let site = st.part::<crate::branding::Memory>().settings.read().unwrap().site_name.clone();
        let link = format!("{base}/reset-password?token={token}");
        // In the account's language, not the one of whoever asked
        let lang = crate::i18n::recipient(&chosen_lang, &last_lang, crate::i18n::system_default(&st));
        let (subject, body) = message(lang == crate::i18n::Lang::ZhTw, &site, &username, &link);
        // Sent after answering: how long the email server takes mustn't tell whether the account exists
        tokio::spawn(async move {
            if let Err(e) = crate::mail::send(&smtp, &site, &crate::mail::Message { to: &email, subject: &subject, body: &body }).await {
                tracing::warn!("Couldn't send a password reset email: {e}");
            }
        });
    }
    Ok(Json(json!({ "ok": true })))
}

fn message(zh: bool, site: &str, username: &str, link: &str) -> (String, String) {
    if zh {
        (
            format!("重設 {site} 的密碼"),
            format!(
                "有人要求重設 {site} 帳號「{username}」的密碼。\n\n在一小時內開啟這個連結，設定新密碼（只能使用一次）：\n{link}\n\n如果不是你要求的，請忽略這封郵件，你的密碼不會改變。\n"
            ),
        )
    } else {
        (
            format!("Reset your password for {site}"),
            format!(
                "Someone asked to reset the password of the account \"{username}\" on {site}.\n\nOpen this link within an hour to choose a new password (it works once):\n{link}\n\nIf it wasn't you, ignore this email: your password stays as it is.\n"
            ),
        )
    }
}

#[derive(Deserialize)]
pub struct ResetReq {
    token: String,
    new: String,
}

pub async fn reset(State(st): State<AppState>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap, Json(req): Json<ResetReq>) -> AppResult<Json<Value>> {
    validate_password(&req.new, min_password(&st))?;
    let ip = client_ip(&st, addr, &headers);
    let key = format!("reset-ip:{}", auth::limit_key_ip(&ip));
    if auth::attempts_exhausted(&st, &key, WRONG_LINKS) {
        return Err(AppError::new(axum::http::StatusCode::TOO_MANY_REQUESTS, "Too many requests. Try again in 15 minutes."));
    }
    let found: Option<(i64, String)> = sqlx::query_as(
        "SELECT u.id, u.username FROM password_resets r JOIN users u ON u.id = r.user_id WHERE r.token_hash = ? AND r.expires_at > ? AND u.disabled = 0",
    )
    .bind(sha256_hex(req.token.trim().as_bytes()))
    .bind(now())
    .fetch_optional(&st.db)
    .await?;
    let Some((id, username)) = found else {
        auth::begin_attempt(&st, &key, WRONG_LINKS);
        return Err(AppError::bad_request("This link has expired or was already used. Ask for a new one."));
    };
    let hash = hash_password(req.new).await?;
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        // The link is used up in the transaction that sets the password, and checked again there (the check above ran
        // before hashing and without the lock): of two requests with the same link, the second finds it gone
        let used = sqlx::query(
            "DELETE FROM password_resets WHERE token_hash = ? AND user_id = ? AND expires_at > ?
               AND EXISTS (SELECT 1 FROM users WHERE id = password_resets.user_id AND disabled = 0)",
        )
        .bind(sha256_hex(req.token.trim().as_bytes()))
        .bind(id)
        .bind(now())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if used != 1 {
            return Err(AppError::bad_request("This link has expired or was already used. Ask for a new one."));
        }
        sqlx::query("UPDATE users SET password_hash = ?, must_change_password = 0 WHERE id = ?").bind(hash).bind(id).execute(&mut *tx).await?;
        // The account's other links go too (`sign_out_everywhere`)
        auth::sign_out_everywhere(&mut tx, id, None).await?;
        tx.commit().await?;
    }
    logs::record_login(&st, Some(id), &username, "password_reset", &ip, &headers);
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    fn addr() -> ConnectInfo<SocketAddr> {
        ConnectInfo("203.0.113.9:5000".parse().unwrap())
    }

    #[tokio::test]
    async fn a_reset_link_sets_a_new_password_once_and_signs_out_everywhere() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        sqlx::query("UPDATE users SET email = 'amy@example.com' WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
        let _ = env.sign_in(&amy, "Browser").await;
        let forgot_req = |account: &str| forgot(State(env.st.clone()), addr(), HeaderMap::new(), Json(ForgotReq { account: account.into() }));

        // Without an email server: not offered
        let Json(o) = options(State(env.st.clone())).await.unwrap();
        assert_eq!(o["password_reset"], false);
        assert!(forgot_req("amy").await.is_err());
        env.st.system.write().unwrap().public_url = "https://files.example.com".into();
        email_on(&env, 1).await;
        let Json(o) = options(State(env.st.clone())).await.unwrap();
        assert_eq!(o["password_reset"], true);

        // The same answer for an account that doesn't exist; a link only for Amy (by her email address)
        assert!(forgot_req("nobody").await.is_ok());
        assert!(forgot_req("Amy@Example.com").await.is_ok());
        let (links,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM password_resets WHERE user_id = ?").bind(amy.id).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(links, 1);

        // A known token (the one stored is only its hash): replaced by one the test knows
        let token = random_token(43);
        sqlx::query("UPDATE password_resets SET token_hash = ? WHERE user_id = ?")
            .bind(sha256_hex(token.as_bytes()))
            .bind(amy.id)
            .execute(&env.st.db)
            .await
            .unwrap();
        let reset_req = |token: &str, new: &str| reset(State(env.st.clone()), addr(), HeaderMap::new(), Json(ResetReq { token: token.into(), new: new.into() }));
        // (passwords made for the test, not written in the code)
        let (new, another, short) = (random_token(20), random_token(20), random_token(3));
        assert!(reset_req(&token, &short).await.is_err(), "too short");
        assert!(reset_req("wrong", &new).await.is_err());
        assert!(reset_req(&token, &new).await.is_ok());
        assert!(reset_req(&token, &another).await.is_err(), "once only");
        let (sessions,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sessions WHERE user_id = ?").bind(amy.id).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(sessions, 0);
        assert!(auth::confirm_password(&env.st, amy.id, new).await.is_ok());
    }

    /// Turns on an email server at `port` (1: nothing listens, so emails fail quietly)
    async fn email_on(env: &testutil::TestEnv, port: u16) {
        let mut smtp = crate::mail::tests::settings(port);
        smtp.enabled = true;
        let mut tx = crate::db::begin_write(&env.st.db).await.unwrap();
        crate::mail::store(&mut tx, &smtp).await.unwrap();
        tx.commit().await.unwrap();
    }

    /// Amy, with an email address, and a reset link whose token the test knows
    async fn amy_with_link(env: &testutil::TestEnv) -> (auth::User, String) {
        let amy = env.user("amy", true).await;
        sqlx::query("UPDATE users SET email = 'amy@example.com' WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
        let token = add_link(env, amy.id).await;
        (amy, token)
    }

    /// A reset link for the user, as if asked for by email; returns its token
    async fn add_link(env: &testutil::TestEnv, user_id: i64) -> String {
        let token = random_token(43);
        sqlx::query("INSERT INTO password_resets (token_hash, user_id, created_at, expires_at) VALUES (?, ?, ?, ?)")
            .bind(sha256_hex(token.as_bytes()))
            .bind(user_id)
            .bind(now())
            .bind(now() + LINK_TTL)
            .execute(&env.st.db)
            .await
            .unwrap();
        token
    }

    fn reset_req(env: &testutil::TestEnv, token: &str, new: &str) -> impl std::future::Future<Output = AppResult<Json<Value>>> {
        reset(State(env.st.clone()), addr(), HeaderMap::new(), Json(ResetReq { token: token.into(), new: new.into() }))
    }

    #[tokio::test]
    async fn reset_links_use_the_site_url_and_never_the_request_host() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        sqlx::query("UPDATE users SET email = 'amy@example.com' WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
        let (port, mut mails) = crate::mail::tests::fake_server(true).await;
        email_on(&env, port).await;
        let mut headers = HeaderMap::new();
        headers.insert(axum::http::header::HOST, "untrusted.example.com".parse().unwrap());
        let forgot_req = || forgot(State(env.st.clone()), addr(), headers.clone(), Json(ForgotReq { account: "amy".into() }));

        // Without a site URL there is nothing trusted to build the link from: not offered
        let Json(o) = options(State(env.st.clone())).await.unwrap();
        assert_eq!(o["password_reset"], false);
        assert!(forgot_req().await.is_err());
        let (links,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM password_resets").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(links, 0);

        env.st.system.write().unwrap().public_url = "https://files.example.com".into();
        let Json(o) = options(State(env.st.clone())).await.unwrap();
        assert_eq!(o["password_reset"], true);
        assert!(forgot_req().await.is_ok());
        let (to, text) = tokio::time::timeout(std::time::Duration::from_secs(10), mails.recv()).await.unwrap().unwrap();
        assert_eq!(to, "amy@example.com");
        assert!(text.contains("https://files.example.com/reset-password?token="), "{text}");
        assert!(!text.contains("untrusted.example.com"), "{text}");
    }

    #[tokio::test]
    async fn a_reset_link_used_twice_at_once_works_once() {
        let env = testutil::env().await;
        let (amy, token) = amy_with_link(&env).await;
        let (first, second) = (random_token(20), random_token(20));
        let (a, b) = tokio::join!(reset_req(&env, &token, &first), reset_req(&env, &token, &second));
        assert_eq!(a.is_ok() as u8 + b.is_ok() as u8, 1, "exactly one of them sets the password");
        let kept = if a.is_ok() { first } else { second };
        assert!(auth::confirm_password(&env.st, amy.id, kept).await.is_ok());
    }

    #[tokio::test]
    async fn changing_the_password_ends_reset_links() {
        let env = testutil::env().await;
        let (amy, token) = amy_with_link(&env).await;
        let (_, cookie) = env.sign_in(&amy, "Browser").await;
        let mut headers = HeaderMap::new();
        headers.insert(axum::http::header::COOKIE, cookie.parse().unwrap());
        let req = serde_json::from_value(json!({ "current": testutil::password(), "new": random_token(20) })).unwrap();
        let _ = crate::signin::change_password(State(env.st.clone()), addr(), headers, amy.clone(), Json(req)).await.unwrap();
        assert!(reset_req(&env, &token, &random_token(20)).await.is_err(), "a link asked for before the change");

        // The same when an administrator resets the password
        let token = add_link(&env, amy.id).await;
        let admin = env.admin().await;
        let req = serde_json::from_value(json!({ "password": random_token(20) })).unwrap();
        let _ = crate::admin::update(State(env.st.clone()), auth::Admin(admin), axum::extract::Path(amy.id), Json(req)).await.unwrap();
        assert!(reset_req(&env, &token, &random_token(20)).await.is_err());
    }

    #[tokio::test]
    async fn a_new_password_ends_sign_ins_waiting_for_their_second_step() {
        let env = testutil::env().await;
        let (amy, token) = amy_with_link(&env).await;
        env.st.system.write().unwrap().require_two_factor = true;
        let login = |password: &str| {
            let req = crate::signin::LoginReq { username: "amy".into(), password: password.into() };
            crate::signin::login(State(env.st.clone()), addr(), HeaderMap::new(), Json(req))
        };
        let body = |res: axum::response::Response| async move {
            let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
            serde_json::from_slice::<Value>(&bytes).unwrap()
        };
        // A ticket from the old password, not used before the password is reset by email
        let v = body(login(testutil::password()).await.unwrap()).await;
        assert_eq!(v["two_factor"], "setup");
        let ticket = v["ticket"].as_str().unwrap().to_string();
        let new = random_token(20);
        assert!(reset_req(&env, &token, &new).await.is_ok());
        let setup = |ticket: &str| crate::twofactor::login_setup(State(env.st.clone()), Json(serde_json::from_value(json!({ "ticket": ticket })).unwrap()));
        assert_eq!(setup(&ticket).await.map(|_| ()).unwrap_err().code, Some("two_factor_expired"));
        // A ticket from the new password works
        let v = body(login(&new).await.unwrap()).await;
        assert!(setup(v["ticket"].as_str().unwrap()).await.is_ok());
        let _ = amy;
    }

    #[tokio::test]
    async fn a_password_an_administrator_chose_must_be_changed_first() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let (first, own) = (random_token(20), random_token(20));
        let req = serde_json::from_value(json!({ "username": "ben", "password": first, "role": "user", "can_write": true, "can_delete": true, "can_share": true }))
            .unwrap();
        let _ = crate::admin::create(State(env.st.clone()), auth::Admin(admin), Json(req)).await.unwrap();
        let (id,): (i64,) = sqlx::query_as("SELECT id FROM users WHERE username = 'ben'").fetch_one(&env.st.db).await.unwrap();
        let ben = auth::user_by_id(&env.st, &mut env.st.db.acquire().await.unwrap(), id).await.unwrap().unwrap();
        assert!(ben.must_change_password);
        let (_, cookie) = env.sign_in(&ben, "Browser").await;
        let request = |path: &str| axum::http::Request::builder().uri(path).header(axum::http::header::COOKIE, &cookie);
        // Only what changing the password needs
        assert!(env.request_user(request("/api/auth/me")).await.is_some());
        assert!(env.request_user(request("/api/nodes/root/children")).await.is_none());
        // Changed: everything again
        let mut headers = HeaderMap::new();
        headers.insert(axum::http::header::COOKIE, cookie.parse().unwrap());
        let me = env.request_user(request("/api/auth/password")).await.unwrap();
        let req = serde_json::from_value(json!({ "current": first, "new": own })).unwrap();
        let _ = crate::signin::change_password(State(env.st.clone()), addr(), headers, me, Json(req)).await.unwrap();
        assert!(env.request_user(request("/api/nodes/root/children")).await.is_some());
    }
}
