use axum::http::header;

use super::*;
use crate::{
    passkeys::{
        rp::from_url,
        testkit::{Alg, Device},
        webauthn::{UP, UV},
    },
    signin::LoginReq,
    testutil,
};

const SITE: &str = "https://files.example.com";

fn addr() -> ConnectInfo<SocketAddr> {
    ConnectInfo("203.0.113.9:5000".parse().unwrap())
}

fn rp() -> RelyingParty {
    from_url(SITE).unwrap()
}

async fn env() -> testutil::TestEnv {
    let env = testutil::env().await;
    env.st.system.write().public_url = SITE.into();
    env
}

async fn body(res: Response) -> Value {
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn challenge_of(options: &Value) -> Vec<u8> {
    decode(options["challenge"].as_str().unwrap()).unwrap()
}

fn confirmed() -> ConfirmReq {
    ConfirmReq { password: Some(testutil::password().into()), code: None }
}

/// Adds a passkey of `device` to `user`, as the page does: the challenge, the device's answer, then the answer sent
async fn add_passkey(env: &testutil::TestEnv, user: &User, device: &mut Device, name: &str) -> AppResult<i64> {
    let Json(options) = add_options(State(env.st.clone()), user.clone(), Json(confirmed())).await?;
    add_answer(env, user, device, name, &challenge_of(&options)).await
}

async fn add_answer(env: &testutil::TestEnv, user: &User, device: &mut Device, name: &str, challenge: &[u8]) -> AppResult<i64> {
    let made = device.register(&rp(), challenge, UP | UV);
    let req = AddReq {
        name: name.into(),
        client_data_json: b64(&made.client_data_json),
        attestation_object: b64(&made.attestation_object),
        transports: vec!["usb".into(), "not-a-transport".into()],
    };
    let Json(v) = add(State(env.st.clone()), user.clone(), addr(), HeaderMap::new(), Json(req)).await?;
    Ok(v["id"].as_i64().unwrap())
}

fn answer(device: &mut Device, challenge: &[u8], flags: u8, handle_of: Option<i64>) -> AssertionReq {
    let a = device.assert(&rp(), challenge, flags);
    AssertionReq {
        credential_id: b64(&a.credential_id),
        client_data_json: b64(&a.client_data_json),
        authenticator_data: b64(&a.authenticator_data),
        signature: b64(&a.signature),
        user_handle: handle_of.map(|id| b64(&store::user_handle(id))),
    }
}

async fn sign_in(env: &testutil::TestEnv, device: &mut Device, flags: u8) -> AppResult<Response> {
    let Json(options) = login_options(State(env.st.clone())).await?;
    let req = answer(device, &challenge_of(&options), flags, None);
    login(State(env.st.clone()), addr(), HeaderMap::new(), Json(req)).await
}

async fn sessions(env: &testutil::TestEnv, user: &User) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE user_id = ?").bind(user.id).fetch_one(&env.st.db).await.unwrap()
}

async fn password_login(env: &testutil::TestEnv, name: &str) -> Value {
    let req = LoginReq { username: name.into(), password: testutil::password().into() };
    body(crate::signin::login(State(env.st.clone()), addr(), HeaderMap::new(), Json(req)).await.unwrap()).await
}

async fn second_step(env: &testutil::TestEnv, ticket: &str, device: &mut Device, flags: u8) -> AppResult<Response> {
    let Json(options) = second_options(State(env.st.clone()), Json(TicketReq { ticket: ticket.into() })).await?;
    let assertion = answer(device, &challenge_of(&options), flags, None);
    second(State(env.st.clone()), addr(), HeaderMap::new(), Json(SecondReq { ticket: ticket.into(), assertion })).await
}

#[tokio::test]
async fn a_passkey_is_added_listed_renamed_and_removed() {
    let env = env().await;
    let amy = env.user("amy", true).await;
    let bob = env.user("bob", true).await;
    let mut device = Device::new(Alg::Es256);
    let id = add_passkey(&env, &amy, &mut device, "Work laptop").await.unwrap();

    let Json(v) = mine(State(env.st.clone()), amy.clone()).await.unwrap();
    assert_eq!(v["passkeys"][0]["name"], "Work laptop");
    assert_eq!((v["available"].clone(), v["has_password"].clone()), (json!(true), json!(true)));
    // The key itself and its id are never listed
    assert!(v["passkeys"][0].get("public_key").is_none() && v["passkeys"][0].get("credential_id").is_none());
    // Only the ways to reach a device the standard names are kept
    let transports: String = sqlx::query_scalar("SELECT transports FROM passkeys WHERE id = ?").bind(id).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(transports, r#"["usb"]"#);
    // The same passkey can't be added twice
    let again = add_passkey(&env, &amy, &mut device, "Again").await.unwrap_err();
    assert_eq!(again.status, StatusCode::CONFLICT);

    // Someone else's passkey is neither renamed nor removed
    let err = rename(State(env.st.clone()), bob.clone(), Path(id), Json(RenameReq { name: "Mine now".into() })).await.unwrap_err();
    assert_eq!(err.status, StatusCode::NOT_FOUND);
    let err = remove(State(env.st.clone()), bob.clone(), Path(id), addr(), HeaderMap::new(), Json(confirmed())).await.unwrap_err();
    assert_eq!(err.status, StatusCode::NOT_FOUND);

    let Json(_) = rename(State(env.st.clone()), amy.clone(), Path(id), Json(RenameReq { name: "  Home  ".into() })).await.unwrap();
    let Json(v) = mine(State(env.st.clone()), amy.clone()).await.unwrap();
    assert_eq!(v["passkeys"][0]["name"], "Home");
    for bad in ["", "   ", &"x".repeat(61), "line\nbreak"] {
        assert!(rename(State(env.st.clone()), amy.clone(), Path(id), Json(RenameReq { name: bad.into() })).await.is_err(), "{bad:?}");
    }

    // Removing asks for the password
    let wrong = ConfirmReq { password: Some(testutil::wrong_password()), code: None };
    assert!(remove(State(env.st.clone()), amy.clone(), Path(id), addr(), HeaderMap::new(), Json(wrong)).await.is_err());
    let Json(_) = remove(State(env.st.clone()), amy.clone(), Path(id), addr(), HeaderMap::new(), Json(confirmed())).await.unwrap();
    let Json(v) = mine(State(env.st.clone()), amy).await.unwrap();
    assert_eq!(v["passkeys"], json!([]));
}

#[tokio::test]
async fn adding_asks_for_the_password_and_takes_only_a_challenge_given_for_that_account() {
    let env = env().await;
    let amy = env.user("amy", true).await;
    let bob = env.user("bob", true).await;
    let wrong = ConfirmReq { password: Some(testutil::wrong_password()), code: None };
    assert!(add_options(State(env.st.clone()), amy.clone(), Json(wrong)).await.is_err());
    assert!(add_options(State(env.st.clone()), amy.clone(), Json(ConfirmReq::default())).await.is_err());

    // A challenge given to Amy can't add a passkey to Bob's account
    let Json(options) = add_options(State(env.st.clone()), amy.clone(), Json(confirmed())).await.unwrap();
    assert_eq!(options["rp"]["id"], "files.example.com");
    assert_eq!(options["user"]["name"], "amy");
    assert!(options["exclude"].as_array().unwrap().is_empty());
    let err = add_answer(&env, &bob, &mut Device::new(Alg::Es256), "Bob's", &challenge_of(&options)).await.unwrap_err();
    assert_eq!(err.code, Some("passkey_failed"));
    // ...and it was used up by trying: not even Amy can add one with it now
    assert!(add_answer(&env, &amy, &mut Device::new(Alg::Es256), "Amy's", &challenge_of(&options)).await.is_err());

    // A challenge for signing in is not one for adding
    let Json(login) = login_options(State(env.st.clone())).await.unwrap();
    assert!(add_answer(&env, &amy, &mut Device::new(Alg::Es256), "Amy's", &challenge_of(&login)).await.is_err());
    // A passkey already added is named in what the next one must not be (so a device isn't registered twice)
    let mut device = Device::new(Alg::Ed25519);
    add_passkey(&env, &amy, &mut device, "Amy's").await.unwrap();
    let Json(options) = add_options(State(env.st.clone()), amy, Json(confirmed())).await.unwrap();
    assert_eq!(options["exclude"][0]["id"], b64(&device.credential_id));
}

#[tokio::test]
async fn an_account_has_a_limited_number_of_passkeys() {
    let env = env().await;
    let amy = env.user("amy", true).await;
    for n in 0..store::MAX_PER_ACCOUNT {
        add_passkey(&env, &amy, &mut Device::new(Alg::Es256), &format!("Key {n}")).await.unwrap();
    }
    let err = add_passkey(&env, &amy, &mut Device::new(Alg::Es256), "One too many").await.unwrap_err();
    assert!(err.message.contains("20 passkeys at most"), "{}", err.message);
}

#[tokio::test]
async fn a_passkey_signs_in_and_the_session_says_how() {
    let env = env().await;
    let amy = env.user("amy", true).await;
    let mut device = Device::new(Alg::Es256);
    add_passkey(&env, &amy, &mut device, "Phone").await.unwrap();

    let res = sign_in(&env, &mut device, UP | UV).await.unwrap();
    let cookie = res.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().to_string();
    assert!(cookie.starts_with(&format!("{}=", crate::auth::SESSION_COOKIE)), "{cookie}");
    assert_eq!(body(res).await["username"], "amy");
    let method: String = sqlx::query_scalar("SELECT method FROM sessions WHERE user_id = ?").bind(amy.id).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(method, "passkey");
    let (count, used): (i64, Option<i64>) =
        sqlx::query_as("SELECT sign_count, last_used_at FROM passkeys WHERE user_id = ?").bind(amy.id).fetch_one(&env.st.db).await.unwrap();
    assert!(count >= 2 && used.is_some());

    // Again, with the counter going on
    sign_in(&env, &mut device, UP | UV).await.unwrap();
    assert_eq!(sessions(&env, &amy).await, 2);
}

#[tokio::test]
async fn a_passkey_alone_must_have_checked_the_person() {
    let env = env().await;
    let amy = env.user("amy", true).await;
    let mut device = Device::new(Alg::Es256);
    add_passkey(&env, &amy, &mut device, "Phone").await.unwrap();
    // Only touched: the holder of the device is not the person
    let err = sign_in(&env, &mut device, UP).await.unwrap_err();
    assert_eq!(err.code, Some("passkey_failed"));
    assert_eq!(sessions(&env, &amy).await, 0);
}

#[tokio::test]
async fn an_answer_is_used_once_and_only_for_what_it_was_asked() {
    let env = env().await;
    let amy = env.user("amy", true).await;
    let mut device = Device::new(Alg::Es256);
    add_passkey(&env, &amy, &mut device, "Phone").await.unwrap();

    // The same answer sent twice
    let Json(options) = login_options(State(env.st.clone())).await.unwrap();
    let first = answer(&mut device, &challenge_of(&options), UP | UV, None);
    let replay = AssertionReq {
        credential_id: first.credential_id.clone(),
        client_data_json: first.client_data_json.clone(),
        authenticator_data: first.authenticator_data.clone(),
        signature: first.signature.clone(),
        user_handle: None,
    };
    login(State(env.st.clone()), addr(), HeaderMap::new(), Json(first)).await.unwrap();
    assert!(login(State(env.st.clone()), addr(), HeaderMap::new(), Json(replay)).await.is_err());
    assert_eq!(sessions(&env, &amy).await, 1);

    // A challenge given for the second step of a password sign-in does not sign in alone
    let ticket = password_login(&env, "amy").await["ticket"].as_str().unwrap().to_string();
    let Json(options) = second_options(State(env.st.clone()), Json(TicketReq { ticket })).await.unwrap();
    let req = answer(&mut device, &challenge_of(&options), UP | UV, None);
    assert!(login(State(env.st.clone()), addr(), HeaderMap::new(), Json(req)).await.is_err());
    // One that was never given out
    let req = answer(&mut device, &[9; 32], UP | UV, None);
    assert!(login(State(env.st.clone()), addr(), HeaderMap::new(), Json(req)).await.is_err());
}

#[tokio::test]
async fn a_copied_passkey_an_unknown_one_and_another_users_handle_do_not_sign_in() {
    let env = env().await;
    let amy = env.user("amy", true).await;
    let bob = env.user("bob", true).await;
    let mut device = Device::new(Alg::Es256);
    add_passkey(&env, &amy, &mut device, "Phone").await.unwrap();
    sign_in(&env, &mut device, UP | UV).await.unwrap();

    // The count goes back: the key may have been copied
    let counter = device.counter;
    device.counter = 0;
    assert!(sign_in(&env, &mut device, UP | UV).await.is_err());
    device.counter = counter;
    // A passkey nobody added
    assert!(sign_in(&env, &mut Device::new(Alg::Ed25519), UP | UV).await.is_err());
    // Its own user handle is accepted, another account's is not
    let Json(options) = login_options(State(env.st.clone())).await.unwrap();
    let req = answer(&mut device, &challenge_of(&options), UP | UV, Some(bob.id));
    assert!(login(State(env.st.clone()), addr(), HeaderMap::new(), Json(req)).await.is_err());
    let Json(options) = login_options(State(env.st.clone())).await.unwrap();
    let req = answer(&mut device, &challenge_of(&options), UP | UV, Some(amy.id));
    login(State(env.st.clone()), addr(), HeaderMap::new(), Json(req)).await.unwrap();
    assert_eq!(sessions(&env, &amy).await, 2);
    assert_eq!(sessions(&env, &bob).await, 0);
}

#[tokio::test]
async fn a_disabled_account_does_not_sign_in_with_a_passkey() {
    let env = env().await;
    let amy = env.user("amy", true).await;
    let mut device = Device::new(Alg::Es256);
    add_passkey(&env, &amy, &mut device, "Phone").await.unwrap();
    sqlx::query("UPDATE users SET disabled = 1 WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
    assert!(sign_in(&env, &mut device, UP | UV).await.is_err());
    assert_eq!(sessions(&env, &amy).await, 0);
}

#[tokio::test]
async fn passkeys_are_offered_only_for_an_address_they_can_belong_to() {
    let env = env().await;
    assert!(available(&env.st));
    for url in ["", "http://files.example.com", "https://192.0.2.10"] {
        env.st.system.write().public_url = url.into();
        assert!(!available(&env.st), "{url}");
        let err = login_options(State(env.st.clone())).await.unwrap_err();
        assert_eq!(err.code, Some("passkeys_unavailable"));
    }
}

#[tokio::test]
async fn after_a_password_a_passkey_is_the_second_step() {
    let env = env().await;
    let amy = env.user("amy", true).await;
    let bob = env.user("bob", true).await;
    let mut amys = Device::new(Alg::Es256);
    let mut bobs = Device::new(Alg::Es256);
    add_passkey(&env, &amy, &mut amys, "Amy's").await.unwrap();
    add_passkey(&env, &bob, &mut bobs, "Bob's").await.unwrap();

    // The password is right: a ticket for the second step, which says what can answer it
    let v = password_login(&env, "amy").await;
    assert_eq!((v["two_factor"].clone(), v["totp"].clone(), v["passkey"].clone()), (json!("code"), json!(false), json!(true)));
    let ticket = v["ticket"].as_str().unwrap().to_string();
    assert_eq!(sessions(&env, &amy).await, 0);

    // Another account's passkey doesn't do
    assert!(second_step(&env, &ticket, &mut bobs, UP).await.is_err());
    assert_eq!(sessions(&env, &amy).await + sessions(&env, &bob).await, 0);
    // Hers does, only touched: the password was the first factor
    let res = second_step(&env, &ticket, &mut amys, UP).await.unwrap();
    assert_eq!(body(res).await["username"], "amy");
    assert_eq!(sessions(&env, &amy).await, 1);
    // The ticket signed in once
    assert!(second_step(&env, &ticket, &mut amys, UP).await.is_err());
}

#[tokio::test]
async fn the_second_step_takes_only_its_own_challenge_and_a_few_wrong_answers() {
    let env = env().await;
    let amy = env.user("amy", true).await;
    let mut device = Device::new(Alg::Es256);
    add_passkey(&env, &amy, &mut device, "Phone").await.unwrap();
    let ticket = password_login(&env, "amy").await["ticket"].as_str().unwrap().to_string();

    // A challenge for signing in without a password is not one for this ticket
    let Json(options) = login_options(State(env.st.clone())).await.unwrap();
    let assertion = answer(&mut device, &challenge_of(&options), UP | UV, None);
    let err = second(State(env.st.clone()), addr(), HeaderMap::new(), Json(SecondReq { ticket: ticket.clone(), assertion })).await.unwrap_err();
    assert_eq!(err.code, Some("passkey_failed"));
    // Nor is one given to another ticket
    let other = password_login(&env, "amy").await["ticket"].as_str().unwrap().to_string();
    let Json(options) = second_options(State(env.st.clone()), Json(TicketReq { ticket: other })).await.unwrap();
    let assertion = answer(&mut device, &challenge_of(&options), UP, None);
    assert!(second(State(env.st.clone()), addr(), HeaderMap::new(), Json(SecondReq { ticket: ticket.clone(), assertion })).await.is_err());

    // Wrong answers use the ticket up, as wrong codes do
    let mut last = None;
    for _ in 0..5 {
        let assertion = answer(&mut device, &[1; 32], UP, None);
        last = Some(second(State(env.st.clone()), addr(), HeaderMap::new(), Json(SecondReq { ticket: ticket.clone(), assertion })).await.unwrap_err());
    }
    // (the two wrong answers above counted too)
    assert_eq!(last.unwrap().code, Some("two_factor_expired"));
    assert_eq!(sessions(&env, &amy).await, 0);
    // A ticket that doesn't exist has nothing to ask
    assert!(second_options(State(env.st.clone()), Json(TicketReq { ticket: "no such ticket".into() })).await.is_err());
}

#[tokio::test]
async fn a_passkey_meets_the_requirement_of_a_second_factor() {
    let env = env().await;
    let amy = env.user("amy", true).await;
    env.user("bob", true).await;
    add_passkey(&env, &amy, &mut Device::new(Alg::Es256), "Phone").await.unwrap();
    env.st.system.write().require_two_factor = true;
    // With a passkey, the account has what is required; without anything it must set one up
    assert_eq!(password_login(&env, "amy").await["two_factor"], "code");
    assert_eq!(password_login(&env, "bob").await["two_factor"], "setup");
}

#[tokio::test]
async fn resetting_the_second_factors_takes_the_passkeys_away() {
    let env = env().await;
    let amy = env.user("amy", true).await;
    add_passkey(&env, &amy, &mut Device::new(Alg::Es256), "Phone").await.unwrap();
    let mut conn = env.st.db.acquire().await.unwrap();
    assert!(twofactor::reset_in(&mut conn, amy.id).await.unwrap());
    assert_eq!(store::count(&mut conn, amy.id).await.unwrap(), 0);
    // Nothing left to reset
    assert!(!twofactor::reset_in(&mut conn, amy.id).await.unwrap());
}

#[tokio::test]
async fn the_only_way_to_sign_in_is_not_removed() {
    let env = env().await;
    let amy = env.user("amy", true).await;
    let mut first = Device::new(Alg::Es256);
    let one = add_passkey(&env, &amy, &mut first, "First").await.unwrap();
    let two = add_passkey(&env, &amy, &mut Device::new(Alg::Es256), "Second").await.unwrap();
    // The account has no password (it was made by single sign-on)
    sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?").bind(auth::NO_PASSWORD).bind(amy.id).execute(&env.st.db).await.unwrap();
    let Json(v) = mine(State(env.st.clone()), amy.clone()).await.unwrap();
    assert_eq!(v["has_password"], json!(false));
    let nothing = || Json(ConfirmReq::default());
    // (it signed in just now: that is what an account without a password confirms by)
    let (amy, _) = env.sign_in(&amy, "Test browser").await;
    let Json(_) = remove(State(env.st.clone()), amy.clone(), Path(one), addr(), HeaderMap::new(), nothing()).await.unwrap();
    let err = remove(State(env.st.clone()), amy.clone(), Path(two), addr(), HeaderMap::new(), nothing()).await.unwrap_err();
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert_eq!(store::count(&mut env.st.db.acquire().await.unwrap(), amy.id).await.unwrap(), 1);
    // With a linked sign-in account, the last passkey may go
    sqlx::query("INSERT INTO user_identities (user_id, provider, subject, email, name, created_at) VALUES (?, 'github', 'x', 'amy@example.com', 'Amy', 0)")
        .bind(amy.id)
        .execute(&env.st.db)
        .await
        .unwrap();
    let Json(_) = remove(State(env.st.clone()), amy, Path(two), addr(), HeaderMap::new(), nothing()).await.unwrap();
}

async fn set_mode(env: &testutil::TestEnv, mode: &str) {
    sqlx::query("INSERT INTO settings (key, value) VALUES ('sso', ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
        .bind(json!({ "password_sign_in": mode }).to_string())
        .execute(&env.st.db)
        .await
        .unwrap();
}

#[tokio::test]
async fn passkeys_are_used_where_passwords_are() {
    let env = env().await;
    let amy = env.user("amy", true).await;
    let admin = env.admin().await;
    let (mut amys, mut admins) = (Device::new(Alg::Es256), Device::new(Alg::Es256));
    add_passkey(&env, &amy, &mut amys, "Phone").await.unwrap();
    add_passkey(&env, &admin, &mut admins, "Key").await.unwrap();
    assert!(offered(&env.st).await.unwrap());

    // Only administrators may use a password: only theirs signs in, as theirs does
    set_mode(&env, "administrators").await;
    assert!(offered(&env.st).await.unwrap());
    assert!(sign_in(&env, &mut amys, UP | UV).await.is_err());
    assert_eq!(sessions(&env, &amy).await, 0);
    sign_in(&env, &mut admins, UP | UV).await.unwrap();
    assert_eq!(sessions(&env, &admin).await, 1);

    // Nobody does: the identity provider's rules apply to everyone, and the page doesn't offer a passkey
    set_mode(&env, "nobody").await;
    assert!(!offered(&env.st).await.unwrap());
    assert!(sign_in(&env, &mut admins, UP | UV).await.is_err());

    set_mode(&env, "everyone").await;
    sign_in(&env, &mut amys, UP | UV).await.unwrap();
    assert_eq!(sessions(&env, &amy).await, 1);
}
