use super::*;
use crate::{
    admin::{UpdateReq, update},
    auth::User,
    signin::{LoginReq, login},
    testutil::{self, TestEnv},
};
use axum::http::StatusCode;

fn addr() -> ConnectInfo<SocketAddr> {
    ConnectInfo("203.0.113.9:5000".parse().unwrap())
}

/// Settings with Google ready to sign in, and passwords as `mode` says
fn with_google(mode: PasswordSignIn) -> SsoSettings {
    let mut s = SsoSettings { password_sign_in: mode, ..Default::default() };
    s.google.enabled = true;
    s.google.client_id = "google-client".into();
    s.google.client_secret = testutil::password().into();
    s
}

/// Makes `s` the stored and the running settings
async fn apply(env: &TestEnv, s: SsoSettings) {
    let mut tx = crate::db::begin_write(&env.st.db).await.unwrap();
    store(&mut tx, &s).await.unwrap();
    tx.commit().await.unwrap();
    *env.st.part::<Memory>().settings.write() = s;
}

/// Links an account of Google to the user
async fn link(env: &TestEnv, user: &User, signed_in: bool) {
    sqlx::query("INSERT INTO user_identities (provider, subject, user_id, created_at, last_login_at) VALUES ('google', ?, ?, 1, ?)")
        .bind(format!("subject-{}", user.id))
        .bind(user.id)
        .bind(signed_in.then_some(2_i64))
        .execute(&env.st.db)
        .await
        .unwrap();
}

/// A second administrator
async fn another_admin(env: &TestEnv, name: &str) -> User {
    let user = env.user(name, true).await;
    sqlx::query("UPDATE users SET role = 'admin' WHERE id = ?").bind(user.id).execute(&env.st.db).await.unwrap();
    let mut conn = env.st.db.acquire().await.unwrap();
    crate::auth::user_by_id(&env.st, &mut conn, user.id).await.unwrap().unwrap()
}

async fn sign_in(env: &TestEnv, name: &str, password: &str) -> Result<(), AppError> {
    let req = LoginReq { username: name.into(), password: password.into() };
    login(State(env.st.clone()), addr(), HeaderMap::new(), Json(req)).await.map(|_| ())
}

fn update_req(json: Value) -> UpdateReq {
    let mut v = json;
    v["my_password"] = json!(testutil::password());
    serde_json::from_value(v).unwrap()
}

async fn events(env: &TestEnv, n: usize) -> Vec<String> {
    for _ in 0..100 {
        let rows: Vec<(String,)> = sqlx::query_as("SELECT event FROM login_log ORDER BY id").fetch_all(&env.st.db).await.unwrap();
        if rows.len() >= n {
            return rows.into_iter().map(|(e,)| e).collect();
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the log didn't reach {n} entries");
}

#[tokio::test]
async fn the_mode_is_read_from_the_database_and_defaults_to_everyone() {
    let env = testutil::env().await;
    assert_eq!(mode(&env.st.db).await.unwrap(), PasswordSignIn::Everyone);
    apply(&env, with_google(PasswordSignIn::Nobody)).await;
    assert_eq!(mode(&env.st.db).await.unwrap(), PasswordSignIn::Nobody);
    // Changed from outside the running server (the command line): in force at once, without a restart
    sqlx::query("UPDATE settings SET value = json_set(value, '$.password_sign_in', 'administrators') WHERE key = 'sso'").execute(&env.st.db).await.unwrap();
    assert_eq!(mode(&env.st.db).await.unwrap(), PasswordSignIn::Administrators);
    // A value nobody knows is no reason to lock people out
    sqlx::query("UPDATE settings SET value = json_set(value, '$.password_sign_in', 'sometimes') WHERE key = 'sso'").execute(&env.st.db).await.unwrap();
    assert_eq!(mode(&env.st.db).await.unwrap(), PasswordSignIn::Everyone);
}

#[tokio::test]
async fn the_command_line_changes_only_the_mode_and_leaves_the_sealed_secrets_alone() {
    let env = testutil::env().await;
    // Nothing saved yet: the mode is saved, everything else stays at its defaults
    assert_eq!(set_from_command_line(&env.st.db, PasswordSignIn::Nobody).await.unwrap(), PasswordSignIn::Everyone);
    assert_eq!(mode(&env.st.db).await.unwrap(), PasswordSignIn::Nobody);

    apply(&env, with_google(PasswordSignIn::Nobody)).await;
    let raw = || async { sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = 'sso'").fetch_one(&env.st.db).await.unwrap() };
    let before: Value = serde_json::from_str(&raw().await).unwrap();
    assert_eq!(set_from_command_line(&env.st.db, PasswordSignIn::Everyone).await.unwrap(), PasswordSignIn::Nobody);
    let mut after: Value = serde_json::from_str(&raw().await).unwrap();
    assert_eq!(after["password_sign_in"], "everyone");
    after["password_sign_in"] = before["password_sign_in"].clone();
    assert_eq!(after, before, "only the mode changed (the client secret is still the sealed text it was)");
    assert_ne!(before["google"]["client_secret"], testutil::password(), "the secret is stored sealed");
}

#[test]
fn the_names_of_the_modes() {
    for (name, mode) in [("everyone", PasswordSignIn::Everyone), ("administrators", PasswordSignIn::Administrators), ("nobody", PasswordSignIn::Nobody)] {
        assert_eq!(PasswordSignIn::parse(name), Some(mode));
        assert_eq!(mode.name(), name);
    }
    assert_eq!(PasswordSignIn::parse("on"), Some(PasswordSignIn::Everyone));
    assert_eq!(PasswordSignIn::parse("off"), Some(PasswordSignIn::Nobody));
    assert_eq!(PasswordSignIn::parse("Everyone"), None);
    assert_eq!(PasswordSignIn::parse(""), None);
    assert!(PasswordSignIn::Administrators.allows(UserRole::Admin) && !PasswordSignIn::Administrators.allows(UserRole::User));
    assert!(!PasswordSignIn::Nobody.allows(UserRole::Admin) && !PasswordSignIn::Nobody.allows(UserRole::User));
}

#[tokio::test]
async fn a_password_signs_in_only_where_the_mode_allows() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    env.user("amy", true).await;
    for (mode, amy_in, admin_in) in [(PasswordSignIn::Everyone, true, true), (PasswordSignIn::Administrators, false, true), (PasswordSignIn::Nobody, false, false)] {
        apply(&env, with_google(mode)).await;
        let amy = sign_in(&env, "amy", testutil::password()).await;
        let boss = sign_in(&env, &admin.username, testutil::password()).await;
        assert_eq!((amy.is_ok(), boss.is_ok()), (amy_in, admin_in), "{mode:?}");
    }
}

#[tokio::test]
async fn a_refused_password_is_answered_like_a_wrong_one() {
    let env = testutil::env().await;
    env.user("amy", true).await;
    apply(&env, with_google(PasswordSignIn::Nobody)).await;
    let refused = sign_in(&env, "amy", testutil::password()).await.unwrap_err();
    let wrong = sign_in(&env, "amy", &testutil::wrong_password()).await.unwrap_err();
    let unknown = sign_in(&env, "nobody", testutil::password()).await.unwrap_err();
    for e in [&refused, &wrong, &unknown] {
        assert_eq!((e.status, e.message.as_str()), (StatusCode::UNAUTHORIZED, "Incorrect username or password"));
    }
    // It tells nothing to whoever tries, but the log has the reason (whatever the password was), and it counts like any
    // failure
    assert_eq!(events(&env, 3).await, ["password_off", "password_off", "unknown_user"]);
    let failures = env.st.part::<crate::auth::Memory>().login_failures.lock().get("u:amy|203.0.113.9").map(Vec::len);
    assert_eq!(failures, Some(2));
}

#[tokio::test]
async fn the_time_a_refused_password_takes_is_that_of_a_hashed_one() {
    let env = testutil::env().await;
    for name in ["amy", "ben", "cat", "dan"] {
        env.user(name, true).await;
    }
    apply(&env, with_google(PasswordSignIn::Nobody)).await;
    // (a refusal that skipped the hash would be several times faster than one that does it. Each time is the quickest of
    // four, a different account each, so a busy machine in the middle of one doesn't decide the test)
    let (mut refused, mut wrong) = (std::time::Duration::MAX, std::time::Duration::MAX);
    for name in ["amy", "ben", "cat", "dan"] {
        let started = std::time::Instant::now();
        sign_in(&env, name, testutil::password()).await.unwrap_err();
        refused = refused.min(started.elapsed());
        let started = std::time::Instant::now();
        sign_in(&env, name, &testutil::wrong_password()).await.unwrap_err();
        wrong = wrong.min(started.elapsed());
    }
    assert!(refused * 3 > wrong, "a refusal answers much faster: {refused:?} vs {wrong:?}");
}

#[tokio::test]
async fn a_password_can_be_changed_only_where_it_can_be_used() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let admin = env.admin().await;
    apply(&env, with_google(PasswordSignIn::Administrators)).await;
    let change = |user: &User| {
        let req = serde_json::from_value(json!({ "current": testutil::password(), "new": testutil::wrong_password() })).unwrap();
        crate::signin::change_password(State(env.st.clone()), addr(), HeaderMap::new(), user.clone(), Json(req))
    };
    let refused = change(&amy).await.unwrap_err();
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert!(change(&admin).await.is_ok());
}

// ───────────── Never nobody who can sign in ─────────────

async fn guard(env: &TestEnv, mode: PasswordSignIn) -> AppResult<()> {
    let s = SsoSettings { password_sign_in: mode, ..env.st.part::<Memory>().settings.read().clone() };
    let mut conn = env.st.db.acquire().await.unwrap();
    ensure_sign_in_possible(&mut conn, &s).await
}

#[tokio::test]
async fn the_guard_wants_an_enabled_administrator_linked_to_a_provider_that_is_on() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let amy = env.user("amy", true).await;
    apply(&env, with_google(PasswordSignIn::Everyone)).await;
    // Everyone may use a password: nothing to guard
    assert!(guard(&env, PasswordSignIn::Everyone).await.is_ok());
    for mode in [PasswordSignIn::Administrators, PasswordSignIn::Nobody] {
        assert_eq!(guard(&env, mode).await.unwrap_err().status, StatusCode::CONFLICT, "nobody is linked");
    }
    // A standard user linked does not make a way in
    link(&env, &amy, true).await;
    assert!(guard(&env, PasswordSignIn::Nobody).await.is_err());
    // An administrator linked does
    link(&env, &admin, false).await;
    assert!(guard(&env, PasswordSignIn::Nobody).await.is_ok());
    // …unless the account is disabled
    sqlx::query("UPDATE users SET disabled = 1 WHERE id = ?").bind(admin.id).execute(&env.st.db).await.unwrap();
    assert!(guard(&env, PasswordSignIn::Nobody).await.is_err());
    sqlx::query("UPDATE users SET disabled = 0 WHERE id = ?").bind(admin.id).execute(&env.st.db).await.unwrap();
    // …or the provider is off, or half set up
    let mut off = with_google(PasswordSignIn::Nobody);
    off.google.enabled = false;
    let mut conn = env.st.db.acquire().await.unwrap();
    assert!(ensure_sign_in_possible(&mut conn, &off).await.is_err());
    let mut no_secret = with_google(PasswordSignIn::Nobody);
    no_secret.google.client_secret.clear();
    assert!(ensure_sign_in_possible(&mut conn, &no_secret).await.is_err());
}

#[tokio::test]
async fn disabling_demoting_or_resetting_the_last_provider_administrator_is_refused_and_rolled_back() {
    let env = testutil::env().await;
    let boss = env.admin().await;
    let sso_admin = another_admin(&env, "sam").await;
    link(&env, &sso_admin, true).await;
    apply(&env, with_google(PasswordSignIn::Nobody)).await;
    let change = |json: Value| update(State(env.st.clone()), Admin(boss.clone()), Path(sso_admin.id), Json(update_req(json)));

    for (what, json) in [
        ("disabled", json!({ "disabled": true })),
        ("demoted", json!({ "role": "user" })),
        ("password reset (which unlinks the account)", json!({ "password": "a-new-password-1" })),
    ] {
        let err = change(json).await.err().unwrap_or_else(|| panic!("{what} was allowed"));
        assert_eq!(err.status, StatusCode::CONFLICT, "{what}");
    }
    let (role, disabled, identities): (String, bool, i64) =
        sqlx::query_as("SELECT role, disabled, (SELECT COUNT(*) FROM user_identities WHERE user_id = users.id) FROM users WHERE id = ?")
            .bind(sso_admin.id)
            .fetch_one(&env.st.db)
            .await
            .unwrap();
    assert_eq!((role.as_str(), disabled, identities), ("admin", false, 1), "nothing changed");

    // Another changes that leave the way in alone are fine, and so is the same change once there is a second way in
    assert!(change(json!({ "display_name": "Sam" })).await.is_ok());
    link(&env, &boss, true).await;
    assert!(change(json!({ "disabled": true })).await.is_ok());
}

#[tokio::test]
async fn nothing_is_guarded_while_everyone_may_use_a_password() {
    let env = testutil::env().await;
    let boss = env.admin().await;
    let sso_admin = another_admin(&env, "sam").await;
    link(&env, &sso_admin, true).await;
    apply(&env, with_google(PasswordSignIn::Everyone)).await;
    let req = update_req(json!({ "disabled": true }));
    assert!(update(State(env.st.clone()), Admin(boss), Path(sso_admin.id), Json(req)).await.is_ok());
}

#[tokio::test]
async fn unlinking_the_last_provider_administrators_account_is_refused() {
    let env = testutil::env().await;
    let sso_admin = another_admin(&env, "sam").await;
    link(&env, &sso_admin, true).await;
    apply(&env, with_google(PasswordSignIn::Administrators)).await;
    let unlink = || crate::sso::unlink(State(env.st.clone()), sso_admin.clone(), Path("google".into()), addr(), HeaderMap::new());
    assert_eq!(unlink().await.unwrap_err().status, StatusCode::CONFLICT);
    let (linked,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM user_identities WHERE user_id = ?").bind(sso_admin.id).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(linked, 1);
    apply(&env, with_google(PasswordSignIn::Everyone)).await;
    assert!(unlink().await.is_ok());
}

// ───────────── Saving the setting ─────────────

async fn save(env: &TestEnv, who: &User, s: SsoSettings) -> AppResult<()> {
    let req = SaveReq { settings: s, me: testutil::confirmed() };
    update_settings(State(env.st.clone()), Admin(who.clone()), HeaderMap::new(), Json(req)).await.map(|_| ())
}

#[tokio::test]
async fn the_setting_is_saved_only_by_an_administrator_who_can_sign_in_through_a_provider() {
    let env = testutil::env().await;
    let boss = env.admin().await;
    // No provider ready
    let err = save(&env, &boss, SsoSettings { password_sign_in: PasswordSignIn::Administrators, ..Default::default() }).await.unwrap_err();
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    // A provider, but the administrator is not linked to it
    let err = save(&env, &boss, with_google(PasswordSignIn::Administrators)).await.unwrap_err();
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert_eq!(mode(&env.st.db).await.unwrap(), PasswordSignIn::Everyone, "nothing was saved");
    // Linked: administrators-only is fine; nobody needs a sign-in through the provider first
    link(&env, &boss, false).await;
    assert!(save(&env, &boss, with_google(PasswordSignIn::Administrators)).await.is_ok());
    assert_eq!(mode(&env.st.db).await.unwrap(), PasswordSignIn::Administrators);
    let err = save(&env, &boss, with_google(PasswordSignIn::Nobody)).await.unwrap_err();
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert_eq!(mode(&env.st.db).await.unwrap(), PasswordSignIn::Administrators);
    sqlx::query("UPDATE user_identities SET last_login_at = 5 WHERE user_id = ?").bind(boss.id).execute(&env.st.db).await.unwrap();
    assert!(save(&env, &boss, with_google(PasswordSignIn::Nobody)).await.is_ok());
    assert_eq!(mode(&env.st.db).await.unwrap(), PasswordSignIn::Nobody);
    // Back to everyone is always possible
    assert!(save(&env, &boss, with_google(PasswordSignIn::Everyone)).await.is_ok());
    assert_eq!(mode(&env.st.db).await.unwrap(), PasswordSignIn::Everyone);
}

#[tokio::test]
async fn another_change_to_the_settings_only_has_to_leave_a_way_in() {
    let env = testutil::env().await;
    let boss = env.admin().await;
    let sso_admin = another_admin(&env, "sam").await;
    link(&env, &sso_admin, true).await;
    apply(&env, with_google(PasswordSignIn::Administrators)).await;
    // The administrator saving is not linked, but the mode is not what changes: saved
    let mut s = with_google(PasswordSignIn::Administrators);
    s.allowed_domains = vec!["example.com".into()];
    assert!(save(&env, &boss, s).await.is_ok());
    // Turning off the provider that is the way in is refused, whoever asks, and nothing is saved
    let mut off = with_google(PasswordSignIn::Administrators);
    off.google.enabled = false;
    let err = save(&env, &boss, off).await.unwrap_err();
    assert_eq!(err.status, StatusCode::CONFLICT);
    assert!(env.st.part::<Memory>().settings.read().google.enabled);
    // Back to everyone first, and it can be turned off
    let mut free = with_google(PasswordSignIn::Everyone);
    free.google.enabled = false;
    assert!(save(&env, &boss, free).await.is_ok());
}

#[tokio::test]
async fn the_settings_page_lists_who_can_sign_in_through_a_provider() {
    let env = testutil::env().await;
    let boss = env.admin().await;
    let sso_admin = another_admin(&env, "sam").await;
    link(&env, &sso_admin, false).await;
    apply(&env, with_google(PasswordSignIn::Everyone)).await;
    let Json(view) = get_settings(State(env.st.clone()), Admin(boss), HeaderMap::new()).await.unwrap();
    assert_eq!(view["password_sign_in"], "everyone");
    assert_eq!(view["sso_admins"], json!([{ "id": sso_admin.id, "username": "sam", "signed_in": false }]));
}
