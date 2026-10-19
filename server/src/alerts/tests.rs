//! Administrator alerts: what is looked at, told once and again when over, delivered, tried again, and the settings

use std::sync::atomic::Ordering;

use axum::{
    Router,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use tokio::sync::mpsc::UnboundedReceiver;

use super::{conditions::CERTIFICATE_DAYS, *};
use crate::{
    i18n::Lang,
    mail::tests as mail_tests,
    testutil::{self, TestEnv},
};

const SECRET_TEXT: &str = "a shared secret of 24 chars";
const T0: i64 = 1_800_000_000;

async fn set(env: &TestEnv, c: &Config) {
    db::set_json_setting(&mut env.st.db.acquire().await.unwrap(), SETTING, c).await.unwrap();
}

async fn set_secret(env: &TestEnv) {
    db::set_setting(&mut env.st.db.acquire().await.unwrap(), SECRET, &crate::secrets::seal(SEAL, SECRET_TEXT)).await.unwrap();
}

/// The certificate the FTPS listener serves ends in `days` days (from `T0`)
fn certificate_ends_in(env: &TestEnv, days: Option<i64>) {
    env.st.part::<crate::ftp::Memory>().status.write().cert_expires_at = days.map(|d| T0 + d * 86400 + 3600);
}

/// A webhook receiver on this computer: answers `status` and hands over each request (its headers and body)
async fn receiver(status: u16) -> (String, UnboundedReceiver<(HeaderMap, String)>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let app = Router::new().route(
        "/hook",
        post(move |headers: HeaderMap, body: String| {
            let tx = tx.clone();
            async move {
                let _ = tx.send((headers, body));
                StatusCode::from_u16(status).unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, rx)
}

/// The subject of a received message, decoded (the fake server decodes only the body)
fn subject_of(message: &str) -> String {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let line = message.lines().find(|l| l.starts_with("Subject: ")).unwrap_or_default();
    let encoded = line.trim_start_matches("Subject: ").trim_start_matches("=?UTF-8?B?").trim_end_matches("?=");
    String::from_utf8(STANDARD.decode(encoded).unwrap_or_default()).unwrap_or_default()
}

async fn rows(env: &TestEnv, filter: &str) -> Vec<(String, String, String, String)> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT channel, state, kind, key FROM alert_deliveries {filter} ORDER BY id"))).fetch_all(&env.st.db).await.unwrap()
}

async fn webhook_only(env: &TestEnv, url: &str) -> Config {
    set_secret(env).await;
    let c = Config { email: false, webhook_url: url.into(), allow_private: true, ..Config::default() };
    set(env, &c).await;
    c
}

// ───────────── What is looked at ─────────────

#[tokio::test]
async fn failed_sign_ins_passing_the_limit_are_told_by_address_and_by_existing_account_only() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let c = Config { sign_in_failures: 5, sign_in_minutes: 10, ..Config::default() };
    let fail = |at: i64, user: Option<i64>, name: &'static str, ip: &'static str| {
        sqlx::query("INSERT INTO login_log (at, user_id, username, event, ip) VALUES (?, ?, ?, 'failed', ?)")
            .bind(at)
            .bind(user)
            .bind(name)
            .bind(ip)
            .execute(&env.st.db)
    };
    for i in 0..6 {
        fail(T0 - 60 * i, Some(amy.id), "amy", "10.0.0.5").await.unwrap();
    }
    // Ten from another address for names that aren't accounts (maybe a password typed in the wrong box): the address only
    for i in 0..10 {
        fail(T0 - 30 * i, None, "hunter2-the-password", "203.0.113.9").await.unwrap();
    }
    // Old ones, and a few from a third address: under the limit
    for i in 0..9 {
        fail(T0 - 3600 - i, Some(amy.id), "amy", "198.51.100.1").await.unwrap();
    }
    for i in 0..3 {
        fail(T0 - i, None, "x", "192.0.2.7").await.unwrap();
    }
    let found = conditions::current(&env.st, &c, T0).await.unwrap();
    let mut keys: Vec<String> = found.iter().map(|x| x.key.clone()).collect();
    keys.sort();
    let mut expected = vec![format!("sign_ins_user:{}", amy.id), "sign_ins_ip:10.0.0.5".to_string(), "sign_ins_ip:203.0.113.9".to_string()];
    expected.sort();
    assert_eq!(keys, expected);
    assert!(found.iter().all(|x| !x.params.to_string().contains("hunter2")), "{found:?}");
    let user = found.iter().find(|x| x.kind == "sign_ins_user").unwrap();
    assert_eq!((user.params["username"].as_str(), user.params["count"].as_i64(), user.params["minutes"].as_i64()), (Some("amy"), Some(6), Some(10)));
    // Off
    assert!(conditions::current(&env.st, &Config { sign_ins: false, ..c }, T0).await.unwrap().iter().all(|x| !x.kind.starts_with("sign_ins")));
}

#[tokio::test]
async fn a_certificate_is_told_within_14_days_of_its_end_and_not_before() {
    let env = testutil::env().await;
    let c = Config::default();
    for (days, told) in [(None, false), (Some(CERTIFICATE_DAYS + 1), false), (Some(CERTIFICATE_DAYS), true), (Some(3), true), (Some(-2), true)] {
        certificate_ends_in(&env, days);
        let found = conditions::current(&env.st, &c, T0).await.unwrap();
        let cert: Vec<_> = found.iter().filter(|x| x.kind == "certificate").collect();
        assert_eq!(cert.len(), usize::from(told), "{days:?}");
        if let (Some(d), Some(x)) = (days, cert.first()) {
            assert_eq!(x.params["days"].as_i64(), Some(d.max(0)));
        }
    }
    certificate_ends_in(&env, Some(3));
    assert!(conditions::current(&env.st, &Config { certificate: false, ..c }, T0).await.unwrap().is_empty());
}

// ───────────── Told once, and again when over ─────────────

#[tokio::test]
async fn a_problem_is_told_once_when_it_starts_and_once_when_it_is_over() {
    let env = testutil::env().await;
    webhook_only(&env, "http://127.0.0.1:9/hook").await;
    certificate_ends_in(&env, Some(5));
    queue::evaluate(&env.st, T0).await.unwrap();
    queue::evaluate(&env.st, T0 + 60).await.unwrap();
    queue::evaluate(&env.st, T0 + 120).await.unwrap();
    assert_eq!(rows(&env, "").await, [("webhook".into(), "firing".into(), "certificate".into(), "certificate".into())], "told once while it lasts");
    // Over
    certificate_ends_in(&env, None);
    queue::evaluate(&env.st, T0 + 180).await.unwrap();
    queue::evaluate(&env.st, T0 + 240).await.unwrap();
    let states: Vec<String> = rows(&env, "").await.into_iter().map(|r| r.1).collect();
    assert_eq!(states, ["firing", "resolved"]);
    let (shown,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM alert_conditions WHERE active = 1").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(shown, 0);
}

#[tokio::test]
async fn a_problem_that_comes_back_within_the_cooldown_waits_for_it_and_one_never_told_says_nothing_when_over() {
    let env = testutil::env().await;
    webhook_only(&env, "http://127.0.0.1:9/hook").await;
    let cooldown = 30 * 60;
    certificate_ends_in(&env, Some(5));
    queue::evaluate(&env.st, T0).await.unwrap();
    certificate_ends_in(&env, None);
    queue::evaluate(&env.st, T0 + 100).await.unwrap();
    // Back after five minutes: the last message was sent at T0 + 100 (resolved), the cooldown isn't over
    certificate_ends_in(&env, Some(5));
    queue::evaluate(&env.st, T0 + 400).await.unwrap();
    assert_eq!(rows(&env, "").await.len(), 2, "firing and resolved, nothing more yet");
    // Over again before it was told: nothing is said
    certificate_ends_in(&env, None);
    queue::evaluate(&env.st, T0 + 500).await.unwrap();
    assert_eq!(rows(&env, "").await.len(), 2);
    // Back, and still there when the cooldown is over: told then
    certificate_ends_in(&env, Some(5));
    queue::evaluate(&env.st, T0 + 600).await.unwrap();
    assert_eq!(rows(&env, "").await.len(), 2);
    queue::evaluate(&env.st, T0 + 100 + cooldown).await.unwrap();
    let states: Vec<String> = rows(&env, "").await.into_iter().map(|r| r.1).collect();
    assert_eq!(states, ["firing", "resolved", "firing"]);
}

#[tokio::test]
async fn an_alert_turned_off_while_it_lasts_says_nothing_when_it_is_over() {
    let env = testutil::env().await;
    let c = webhook_only(&env, "http://127.0.0.1:9/hook").await;
    certificate_ends_in(&env, Some(5));
    queue::evaluate(&env.st, T0).await.unwrap();
    set(&env, &Config { certificate: false, ..c }).await;
    queue::evaluate(&env.st, T0 + 60).await.unwrap();
    let states: Vec<String> = rows(&env, "").await.into_iter().map(|r| r.1).collect();
    assert_eq!(states, ["firing"]);
}

#[tokio::test]
async fn backup_and_replica_problems_are_queued_as_they_happen_when_chosen() {
    let env = testutil::env().await;
    let c = webhook_only(&env, "http://127.0.0.1:9/hook").await;
    let data = json!({ "name": "Nightly", "state": "failing", "error": "Disk full" });
    notice(&env.st, "backup", true, &data).await;
    notice(&env.st, "replica", true, &json!({ "name": "Archive" })).await;
    notice(&env.st, "backup", false, &data).await;
    // Other notices are not alerts
    notice(&env.st, "shared", true, &data).await;
    let got: Vec<(String, String)> = rows(&env, "").await.into_iter().map(|r| (r.2, r.1)).collect();
    assert_eq!(got, [("backup".into(), "firing".into()), ("replica".into(), "firing".into()), ("backup".into(), "resolved".into())]);
    set(&env, &Config { backups: false, ..c }).await;
    notice(&env.st, "backup", true, &data).await;
    assert_eq!(rows(&env, "").await.len(), 3);
}

// ───────────── Delivered ─────────────

#[tokio::test]
async fn a_webhook_gets_a_signed_body_and_is_done_with() {
    let env = testutil::env().await;
    let (url, mut rx) = receiver(200).await;
    webhook_only(&env, &url).await;
    certificate_ends_in(&env, Some(5));
    queue::evaluate(&env.st, T0).await.unwrap();
    queue::deliver_due(&env.st, T0).await.unwrap();
    let (headers, body) = rx.recv().await.unwrap();
    let timestamp: i64 = headers["x-thirtyfile-timestamp"].to_str().unwrap().parse().unwrap();
    assert_eq!(headers["x-thirtyfile-signature"].to_str().unwrap(), webhook::signature(SECRET_TEXT, timestamp, &body));
    assert_ne!(webhook::signature("another secret of 24 chars", timestamp, &body), webhook::signature(SECRET_TEXT, timestamp, &body));
    // The format a receiver computes: HMAC-SHA256 over "<timestamp>.<body>" (checked against a value made elsewhere)
    assert_eq!(
        webhook::signature("a shared secret of 24 ch", 1_700_000_000, r#"{"a":1}"#),
        "sha256=b11e65f1f6d3b0fb5a045b0f676c2fc97dab116957cfbe6f7d450ecb8c8acd66"
    );
    let sent: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        (sent["event"].as_str(), sent["state"].as_str(), sent["kind"].as_str(), sent["key"].as_str()),
        (Some("alert"), Some("firing"), Some("certificate"), Some("certificate"))
    );
    assert_eq!(sent["title"].as_str(), Some("The FTP certificate ends in 5 days"));
    let (done, attempts): (Option<i64>, i64) = sqlx::query_as("SELECT done_at, attempts FROM alert_deliveries").fetch_one(&env.st.db).await.unwrap();
    assert_eq!((done, attempts), (Some(T0), 1));
    // Not sent again
    queue::deliver_due(&env.st, T0 + 10_000).await.unwrap();
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn a_webhook_that_fails_is_tried_again_with_longer_waits_and_then_given_up_on() {
    let env = testutil::env().await;
    let (url, mut rx) = receiver(500).await;
    webhook_only(&env, &url).await;
    certificate_ends_in(&env, Some(5));
    queue::evaluate(&env.st, T0).await.unwrap();
    let mut t = T0;
    let mut waits = vec![];
    for _ in 0..6 {
        queue::deliver_due(&env.st, t).await.unwrap();
        let (next, failed): (i64, bool) = sqlx::query_as("SELECT next_attempt_at, failed FROM alert_deliveries").fetch_one(&env.st.db).await.unwrap();
        waits.push(next - t);
        if failed {
            break;
        }
        // Not tried before its time
        queue::deliver_due(&env.st, next - 1).await.unwrap();
        t = next;
    }
    let mut seen = 0;
    while rx.try_recv().is_ok() {
        seen += 1;
    }
    assert_eq!(seen, 6, "the first try and five more");
    assert_eq!(&waits[..5], [60, 300, 1800, 7200, 21600]);
    let (failed, attempts, error): (bool, i64, Option<String>) =
        sqlx::query_as("SELECT failed, attempts, error FROM alert_deliveries").fetch_one(&env.st.db).await.unwrap();
    assert!(failed && attempts == 6, "{failed} {attempts}");
    assert!(error.is_some_and(|e| e.contains("500")));
}

#[tokio::test]
async fn a_webhook_on_the_private_network_is_refused_unless_allowed_and_plain_http_only_there() {
    let (url, _rx) = receiver(200).await;
    // This computer: refused, then allowed
    assert!(webhook::target(&url, false).await.unwrap_err().contains("private network"));
    assert!(webhook::target(&url, true).await.is_ok());
    for private in ["http://10.1.2.3/hook", "http://192.168.0.9/hook", "http://[::1]/hook", "http://169.254.1.1/hook", "http://100.64.0.1/hook"] {
        assert!(webhook::target(private, false).await.is_err(), "{private}");
        assert!(webhook::target(private, true).await.is_ok(), "{private}");
    }
    // The internet: https only
    assert!(webhook::target("https://8.8.8.8/hook", false).await.is_ok());
    assert!(webhook::target("http://8.8.8.8/hook", false).await.unwrap_err().contains("https"));
    assert!(webhook::target("http://8.8.8.8/hook", true).await.unwrap_err().contains("https"));
    // Not a webhook address
    for bad in ["ftp://example.com/x", "https://user:pass@example.com/x", "not a url", "file:///etc/passwd"] {
        assert!(webhook::target(bad, true).await.is_err(), "{bad}");
    }
}

#[tokio::test]
async fn a_refused_address_is_tried_again_not_forgotten_and_is_not_posted_to() {
    let env = testutil::env().await;
    let (url, mut rx) = receiver(200).await;
    set_secret(&env).await;
    set(&env, &Config { email: false, webhook_url: url.clone(), allow_private: false, ..Config::default() }).await;
    certificate_ends_in(&env, Some(5));
    queue::evaluate(&env.st, T0).await.unwrap();
    queue::deliver_due(&env.st, T0).await.unwrap();
    assert!(rx.try_recv().is_err());
    let (done, attempts, error): (Option<i64>, i64, Option<String>) =
        sqlx::query_as("SELECT done_at, attempts, error FROM alert_deliveries").fetch_one(&env.st.db).await.unwrap();
    assert!(done.is_none() && attempts == 1 && error.is_some_and(|e| e.contains("private network")));
    // Allowed since: the next try goes through
    set(&env, &Config { email: false, webhook_url: url, allow_private: true, ..Config::default() }).await;
    queue::deliver_due(&env.st, T0 + 60).await.unwrap();
    assert!(rx.recv().await.is_some());
}

#[tokio::test]
async fn administrators_with_an_address_get_the_email_in_their_language_and_others_do_not() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let second = env.user("second-admin", true).await;
    sqlx::query("UPDATE users SET role = 'admin' WHERE id = ?").bind(second.id).execute(&env.st.db).await.unwrap();
    sqlx::query("UPDATE users SET email = 'admin@example.com', chosen_lang = 'zh-TW' WHERE id = ?").bind(admin.id).execute(&env.st.db).await.unwrap();
    // No address
    let (port, mut inbox) = mail_tests::fake_server(true).await;
    crate::mail::store(&mut env.st.db.acquire().await.unwrap(), &mail_tests::settings(port)).await.unwrap();
    set(&env, &Config::default()).await;
    certificate_ends_in(&env, Some(5));
    queue::evaluate(&env.st, T0).await.unwrap();
    assert_eq!(rows(&env, "").await.len(), 1, "one email, for the administrator with an address");
    queue::deliver_due(&env.st, T0).await.unwrap();
    let (to, message) = inbox.recv().await.unwrap();
    assert_eq!(to, "admin@example.com");
    assert!(message.contains("FTP 憑證將在 5 天後到期"), "{message}");
    assert_eq!(subject_of(&message), "警示：FTP 憑證將在 5 天後到期");
    certificate_ends_in(&env, None);
    queue::evaluate(&env.st, T0 + 60).await.unwrap();
    queue::deliver_due(&env.st, T0 + 60).await.unwrap();
    let (_, resolved) = inbox.recv().await.unwrap();
    assert_eq!(subject_of(&resolved), "已解除：FTP 憑證將在 5 天後到期");
    // No email server set up: nothing is queued for email
    sqlx::query("DELETE FROM settings WHERE key = 'smtp'").execute(&env.st.db).await.unwrap();
    certificate_ends_in(&env, Some(2));
    queue::evaluate(&env.st, T0 + 4000).await.unwrap();
    assert_eq!(rows(&env, "WHERE id > 2").await.len(), 0);
}

#[test]
fn every_alert_says_what_it_is_about_in_each_language_with_what_it_holds() {
    let cases = [
        ("usage_disk", json!({ "location": "NAS", "percent": 93 })),
        ("data_disk", json!({ "percent": 97 })),
        ("usage_errors", json!({ "location": "NAS", "percent": 12 })),
        ("offline", json!({ "location": "NAS" })),
        ("sign_ins_ip", json!({ "ip": "203.0.113.9", "count": 14, "minutes": 10 })),
        ("sign_ins_user", json!({ "username": "amy", "count": 14, "minutes": 10 })),
        ("certificate", json!({ "days": 6 })),
        ("backup", json!({ "name": "Nightly" })),
        ("replica", json!({ "name": "Archive" })),
    ];
    let present = |kind: &str| -> &'static [&'static str] {
        match kind {
            "usage_disk" => &["NAS", "93"],
            "data_disk" => &["97"],
            "usage_errors" => &["NAS", "12"],
            "offline" => &["NAS"],
            "sign_ins_ip" => &["203.0.113.9", "14", "10"],
            "sign_ins_user" => &["amy", "14", "10"],
            "certificate" => &["6"],
            "backup" => &["Nightly"],
            _ => &["Archive"],
        }
    };
    for lang in [Lang::En, Lang::ZhTw, Lang::ZhCn, Lang::Ja] {
        for (kind, params) in &cases {
            let said = text::what(kind, params, lang);
            assert!(present(kind).iter().all(|p| said.contains(p)), "{lang:?} {kind}: {said}");
            let (subject, body) = text::email("firing", kind, params, lang);
            assert!(subject.contains(&said) && body.contains(&said), "{lang:?} {kind}");
            assert!(text::email("resolved", kind, params, lang).0.contains(&said));
        }
    }
    // The database backups' name is in the reader's language
    assert!(text::what("backup", &json!({ "name": "Database", "database": true }), Lang::ZhTw).contains("資料庫"));
    // The reason of a backup problem is in the email
    assert!(text::email("firing", "backup", &json!({ "name": "Nightly", "error": "Disk full" }), Lang::En).1.contains("Reason: Disk full"));
}

// ───────────── The settings ─────────────

#[tokio::test]
async fn settings_are_checked_the_secret_is_kept_sealed_and_a_new_one_is_optional() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let save =
        |c: Config, secret: Option<&str>| super::save(State(env.st.clone()), Admin(admin.clone()), Json(Change { config: c, secret: secret.map(str::to_string) }));
    let hook = || Config { webhook_url: "https://hooks.example.com/alerts".into(), ..Config::default() };
    assert!(save(hook(), None).await.is_err(), "a webhook needs a secret");
    assert!(save(hook(), Some("short")).await.is_err());
    assert!(save(Config { webhook_url: "ftp://example.com/x".into(), ..Config::default() }, Some(SECRET_TEXT)).await.is_err());
    assert!(save(Config { webhook_url: "https://u:p@example.com/x".into(), ..Config::default() }, Some(SECRET_TEXT)).await.is_err());
    assert!(save(Config { sign_in_failures: 1, ..Config::default() }, None).await.is_err());
    assert!(save(Config { sign_in_minutes: 0, ..Config::default() }, None).await.is_err());
    assert!(save(Config { cooldown_minutes: 5000, ..Config::default() }, None).await.is_err());
    let Json(saved) = save(hook(), Some(SECRET_TEXT)).await.unwrap();
    assert!(saved.has_secret);
    let stored = db::get_setting(&env.st.db, SECRET).await.unwrap().unwrap();
    assert!(!stored.contains(SECRET_TEXT));
    assert!(!serde_json::to_string(&saved).unwrap().contains(SECRET_TEXT));
    // Changed without a secret: the one saved stays
    let _ = save(Config { cooldown_minutes: 10, ..hook() }, None).await.unwrap();
    assert_eq!(db::get_setting(&env.st.db, SECRET).await.unwrap().unwrap(), stored);
    assert_eq!(queue::secret(&env.st.db).await.unwrap().as_deref(), Some(SECRET_TEXT));
    // Without a webhook, no secret is needed
    assert!(save(Config::default(), None).await.is_ok());
}

#[tokio::test]
async fn the_test_buttons_send_now_and_say_why_when_they_cannot() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let test = |channel: &str| super::test(State(env.st.clone()), Admin(admin.clone()), Json(Test { channel: channel.into() }));
    assert!(test("pigeon").await.is_err());
    assert!(test("webhook").await.unwrap_err().message.contains("webhook address"));
    let (url, mut rx) = receiver(200).await;
    set(&env, &Config { webhook_url: url.clone(), allow_private: true, ..Config::default() }).await;
    assert!(test("webhook").await.unwrap_err().message.contains("secret"));
    set_secret(&env).await;
    let _ = test("webhook").await.unwrap();
    let (_, body) = rx.recv().await.unwrap();
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["event"], "test");
    set(&env, &Config { webhook_url: url, allow_private: false, ..Config::default() }).await;
    assert!(test("webhook").await.unwrap_err().message.contains("private network"));
    // Email: needs the server and an address
    assert!(test("email").await.unwrap_err().message.contains("email server"));
    let (port, mut inbox) = mail_tests::fake_server(true).await;
    crate::mail::store(&mut env.st.db.acquire().await.unwrap(), &mail_tests::settings(port)).await.unwrap();
    assert!(test("email").await.unwrap_err().message.contains("no email address"));
    sqlx::query("UPDATE users SET email = 'admin@example.com' WHERE id = ?").bind(admin.id).execute(&env.st.db).await.unwrap();
    let _ = test("email").await.unwrap();
    assert_eq!(inbox.recv().await.unwrap().0, "admin@example.com");
}

#[tokio::test]
async fn it_looks_once_a_minute_and_sends_what_is_due_every_time() {
    let env = testutil::env().await;
    let (url, mut rx) = receiver(200).await;
    webhook_only(&env, &url).await;
    certificate_ends_in(&env, Some(5));
    let mem = env.st.part::<Memory>();
    mem.last_look.store(0, Ordering::Release);
    queue::tick(&env.st, T0).await.unwrap();
    assert!(rx.recv().await.is_some());
    // Within the minute it doesn't look again: a change isn't seen yet, but nothing waits to be sent
    certificate_ends_in(&env, None);
    queue::tick(&env.st, T0 + 30).await.unwrap();
    assert_eq!(rows(&env, "").await.len(), 1);
    queue::tick(&env.st, T0 + 61).await.unwrap();
    assert!(rx.recv().await.is_some());
    // Old messages are deleted
    queue::evaluate(&env.st, T0 + 40 * 86400).await.unwrap();
    assert_eq!(rows(&env, "").await.len(), 0);
}
