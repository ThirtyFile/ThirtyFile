use std::{net::SocketAddr, time::Duration};

use axum::{
    Json,
    body::Body,
    extract::{ConnectInfo, State},
    http::{HeaderMap, HeaderValue, Request, StatusCode, header},
    response::Response,
};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{
    collect::{self, Location, Snapshot},
    expose::parse::{Family, parse},
    render::render,
    *,
};
use crate::{
    auth::Admin,
    state::AppState,
    testutil::{self, TestEnv},
    usage::meter::{Op, Outcome, Work},
};

fn addr(ip: &str) -> ConnectInfo<SocketAddr> {
    ConnectInfo(format!("{ip}:5000").parse().unwrap())
}

fn bearer(token: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(header::AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {token}")).unwrap());
    h
}

async fn ask(st: &AppState, ip: &str, headers: HeaderMap) -> Response {
    scrape(State(st.clone()), addr(ip), headers).await
}

async fn text(res: Response) -> String {
    String::from_utf8(axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap()
}

/// What an administrator does on the page: save, confirming with their password
async fn save(env: &TestEnv, body: Value) -> Result<Value, crate::error::AppError> {
    let mut body = body;
    body["my_password"] = json!(testutil::password());
    let req = serde_json::from_value(body).unwrap();
    update_settings(State(env.st.clone()), Admin(env.admin().await), HeaderMap::new(), Json(req)).await.map(|Json(v)| v)
}

/// Turns monitoring on and returns the token
async fn on(env: &TestEnv) -> String {
    save(env, json!({ "enabled": true, "new_token": true })).await.unwrap()["token"].as_str().unwrap().to_string()
}

fn family<'a>(families: &'a [Family], name: &str) -> &'a Family {
    families.iter().find(|f| f.name == name).unwrap_or_else(|| panic!("no {name} in {:?}", families.iter().map(|f| &f.name).collect::<Vec<_>>()))
}

/// The value of the series of `name` with exactly these labels
fn value(families: &[Family], name: &str, labels: &[(&str, &str)]) -> Option<f64> {
    families
        .iter()
        .flat_map(|f| &f.samples)
        .find(|s| s.name == name && s.labels.len() == labels.len() && labels.iter().all(|(k, v)| s.labels.get(*k).map(String::as_str) == Some(v)))
        .map(|s| s.value)
}

fn page(env: &TestEnv) -> Vec<Family> {
    let out = render(&env.st);
    parse(&out).unwrap_or_else(|e| panic!("{e}\n{out}"))
}

// ───────────── Who may ask ─────────────

#[tokio::test]
async fn it_is_off_until_an_administrator_turns_it_on_and_then_asks_for_the_token() {
    let env = testutil::env().await;
    // Off: as if there were no such page, whatever is sent
    assert_eq!(ask(&env.st, "203.0.113.9", HeaderMap::new()).await.status(), StatusCode::NOT_FOUND);
    // It can't be turned on without a token to ask for
    assert_eq!(save(&env, json!({ "enabled": true })).await.unwrap_err().status, StatusCode::BAD_REQUEST);

    let token = on(&env).await;
    assert!(token.starts_with("tfm_") && token.len() > 40);
    assert_eq!(ask(&env.st, "203.0.113.9", HeaderMap::new()).await.status(), StatusCode::UNAUTHORIZED);
    let none = ask(&env.st, "203.0.113.9", HeaderMap::new()).await;
    assert_eq!(none.headers()[header::WWW_AUTHENTICATE], "Bearer realm=\"metrics\"");
    let mut basic = HeaderMap::new();
    basic.insert(header::AUTHORIZATION, HeaderValue::from_str(&format!("Basic {token}")).unwrap());
    assert_eq!(ask(&env.st, "203.0.113.9", basic).await.status(), StatusCode::UNAUTHORIZED, "another scheme");
    assert_eq!(ask(&env.st, "203.0.113.9", bearer("tfm_wrong")).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(ask(&env.st, "203.0.113.9", bearer("")).await.status(), StatusCode::UNAUTHORIZED);
    // A part of the token, or the token and more, is not the token
    assert_eq!(ask(&env.st, "203.0.113.9", bearer(&token[..token.len() - 1])).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(ask(&env.st, "203.0.113.9", bearer(&format!("{token}x"))).await.status(), StatusCode::UNAUTHORIZED);

    let ok = ask(&env.st, "198.51.100.1", bearer(&token)).await;
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(ok.headers()[header::CONTENT_TYPE], "text/plain; version=0.0.4; charset=utf-8");
    assert_eq!(ok.headers()[header::CACHE_CONTROL], "no-store");
    let body = text(ok).await;
    assert!(parse(&body).is_ok(), "{body}");

    // Turned off again: gone again, and the token is kept for the next time
    save(&env, json!({ "enabled": false })).await.unwrap();
    assert_eq!(ask(&env.st, "203.0.113.9", bearer(&token)).await.status(), StatusCode::NOT_FOUND);
    save(&env, json!({ "enabled": true })).await.unwrap();
    assert_eq!(ask(&env.st, "203.0.113.9", bearer(&token)).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_token_is_kept_as_a_hash_and_shown_once() {
    let env = testutil::env().await;
    let first = on(&env).await;
    let stored: String = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'metrics'").fetch_one(&env.st.db).await.unwrap();
    assert!(!stored.contains(&first), "the token itself is not stored: {stored}");
    assert!(stored.contains(&crate::util::sha256_hex(first.as_bytes())));
    // Not in what the page shows later, nor after saving other changes
    let Json(view) = get_settings(State(env.st.clone()), Admin(env.admin().await)).await;
    assert_eq!((view["enabled"].clone(), view["has_token"].clone()), (json!(true), json!(true)));
    assert!(view.get("token").is_none() && !view.to_string().contains(&first));
    let after = save(&env, json!({ "enabled": true })).await.unwrap();
    assert!(after.get("token").is_none());

    // A new token replaces it: the old one stops working at once
    let second = save(&env, json!({ "enabled": true, "new_token": true })).await.unwrap()["token"].as_str().unwrap().to_string();
    assert_ne!(first, second);
    assert_eq!(ask(&env.st, "203.0.113.9", bearer(&first)).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(ask(&env.st, "203.0.113.9", bearer(&second)).await.status(), StatusCode::OK);

    // It is in the activity log, without the token
    let details: Vec<(String,)> = sqlx::query_as("SELECT detail FROM activity WHERE detail LIKE 'Monitoring:%' ORDER BY id").fetch_all(&env.st.db).await.unwrap();
    for _ in 0..50 {
        if details.len() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let details: Vec<(String,)> = sqlx::query_as("SELECT detail FROM activity WHERE detail LIKE 'Monitoring:%' ORDER BY id").fetch_all(&env.st.db).await.unwrap();
    assert_eq!(details.iter().map(|d| d.0.as_str()).collect::<Vec<_>>(), ["Monitoring: on, new token", "Monitoring: on", "Monitoring: on, new token"]);
}

#[tokio::test]
async fn only_an_administrator_who_confirms_who_they_are_changes_it() {
    let env = testutil::env().await;
    let mut body = json!({ "enabled": true, "new_token": true, "my_password": testutil::wrong_password() });
    let req = serde_json::from_value(body.take()).unwrap();
    let err = update_settings(State(env.st.clone()), Admin(env.admin().await), HeaderMap::new(), Json(req)).await.unwrap_err();
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert_eq!(ask(&env.st, "203.0.113.9", HeaderMap::new()).await.status(), StatusCode::NOT_FOUND, "nothing was turned on");
}

#[tokio::test]
async fn the_list_of_addresses_decides_who_is_let_in() {
    let env = testutil::env().await;
    let token = on(&env).await;
    save(&env, json!({ "enabled": true, "allow": ["198.51.100.0/24", " 203.0.113.7,203.0.113.7", "2001:db8::/32"] })).await.unwrap();
    let Json(view) = get_settings(State(env.st.clone()), Admin(env.admin().await)).await;
    assert_eq!(view["allow"], json!(["198.51.100.0/24", "203.0.113.7", "2001:db8::/32"]), "trimmed, split and without repeats");

    for (ip, status) in [
        ("198.51.100.77", StatusCode::OK),
        ("203.0.113.7", StatusCode::OK),
        ("203.0.113.8", StatusCode::FORBIDDEN),
        ("198.51.101.1", StatusCode::FORBIDDEN),
        ("2001:db8:1::5", StatusCode::OK),
        ("2001:db9::1", StatusCode::FORBIDDEN),
    ] {
        let ip = if ip.contains(':') { format!("[{ip}]") } else { ip.to_string() };
        assert_eq!(ask(&env.st, &ip, bearer(&token)).await.status(), status, "{ip}");
    }
    // The right token from an address that is not on the list is no more welcome
    assert_eq!(ask(&env.st, "203.0.113.200", HeaderMap::new()).await.status(), StatusCode::FORBIDDEN, "not even asked for the token");

    // Only addresses and networks are accepted
    for bad in ["true", "off", "10.0.0.0/33", "not-an-address", "10.0.0.0/-1", "1.2.3"] {
        assert_eq!(save(&env, json!({ "enabled": true, "allow": [bad] })).await.unwrap_err().status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let many: Vec<String> = (1..=21).map(|i| format!("10.0.0.{i}")).collect();
    assert_eq!(save(&env, json!({ "enabled": true, "allow": many })).await.unwrap_err().status, StatusCode::BAD_REQUEST);
    // An empty list lets anyone ask, with the token
    save(&env, json!({ "enabled": true, "allow": [] })).await.unwrap();
    assert_eq!(ask(&env.st, "203.0.113.200", bearer(&token)).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn wrong_tokens_are_counted_and_an_address_that_sends_too_many_waits() {
    let env = testutil::env().await;
    let token = on(&env).await;
    for _ in 0..20 {
        assert_eq!(ask(&env.st, "203.0.113.50", bearer("tfm_guess")).await.status(), StatusCode::UNAUTHORIZED);
    }
    // Even the right token, from that address, until the time is up
    assert_eq!(ask(&env.st, "203.0.113.50", bearer(&token)).await.status(), StatusCode::TOO_MANY_REQUESTS);
    // Others are not held up by it, and the monitoring system's own right tokens never count against it
    for _ in 0..30 {
        assert_eq!(ask(&env.st, "203.0.113.51", bearer(&token)).await.status(), StatusCode::OK);
    }
    let families = page(&env);
    assert_eq!(value(&families, "thirtyfile_metrics_requests_total", &[("outcome", "unauthorized")]), Some(20.0));
    assert_eq!(value(&families, "thirtyfile_metrics_requests_total", &[("outcome", "too_many")]), Some(1.0));
    assert_eq!(value(&families, "thirtyfile_metrics_requests_total", &[("outcome", "ok")]), Some(30.0));
}

// ───────────── What is on the page ─────────────

#[tokio::test]
async fn a_fresh_server_writes_a_page_a_strict_reader_accepts() {
    let env = testutil::env().await;
    let families = page(&env);
    for name in [
        "thirtyfile_build_info",
        "thirtyfile_process_start_time_seconds",
        "thirtyfile_process_uptime_seconds",
        "thirtyfile_http_request_duration_seconds",
        "thirtyfile_http_requests_in_flight",
        "thirtyfile_emails_total",
    ] {
        family(&families, name);
    }
    assert_eq!(value(&families, "thirtyfile_build_info", &[("version", crate::VERSION)]), Some(1.0));
    // Every name belongs to this program, and a counter ends in _total (the reader checks that last one)
    assert!(families.iter().all(|f| f.name.starts_with("thirtyfile_")), "{:?}", families.iter().map(|f| &f.name).collect::<Vec<_>>());
}

#[tokio::test]
async fn requests_are_counted_by_method_and_class_with_how_long_they_took() {
    let env = testutil::env().await;
    let app = crate::app::routes::router(env.st.clone());
    let get = |uri: &str| {
        let req = Request::builder().uri(uri).extension(addr("10.0.0.1")).body(Body::empty()).unwrap();
        app.clone().oneshot(req)
    };
    assert_eq!(get("/api/health").await.unwrap().status(), StatusCode::OK);
    assert_eq!(get("/api/health").await.unwrap().status(), StatusCode::OK);
    assert_eq!(get("/api/nodes/missing").await.unwrap().status(), StatusCode::UNAUTHORIZED);
    let post = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .extension(addr("10.0.0.1"))
        .body(Body::from("{}"))
        .unwrap();
    let status = app.clone().oneshot(post).await.unwrap().status();
    assert!(status.is_client_error());
    let families = page(&env);
    assert_eq!(value(&families, "thirtyfile_http_requests_total", &[("method", "GET"), ("code", "2xx")]), Some(2.0));
    assert_eq!(value(&families, "thirtyfile_http_requests_total", &[("method", "GET"), ("code", "4xx")]), Some(1.0));
    assert_eq!(value(&families, "thirtyfile_http_requests_total", &[("method", "POST"), ("code", "4xx")]), Some(1.0));
    assert_eq!(value(&families, "thirtyfile_http_request_duration_seconds_count", &[]), Some(4.0));
    assert!(value(&families, "thirtyfile_http_request_duration_seconds_sum", &[]).unwrap() > 0.0);
    assert_eq!(value(&families, "thirtyfile_http_requests_in_flight", &[]), Some(0.0), "none left running");
}

#[tokio::test]
async fn a_request_being_answered_counts_as_in_flight_until_it_is_done() {
    let env = testutil::env().await;
    let http = &env.st.part::<Memory>().http;
    let a = http.begin();
    let b = http.begin();
    assert_eq!(http.figures().in_flight, 2);
    drop(a);
    assert_eq!(http.figures().in_flight, 1);
    drop(b);
    assert_eq!(http.figures().in_flight, 0);
    // An odd method is "other"; a status that is not 1xx to 5xx is kept in the nearest class
    http.finished(&"PROPFIND".parse().unwrap(), 207, Duration::from_millis(3));
    http.finished(&axum::http::Method::GET, 999, Duration::from_secs(300));
    let figures = http.figures();
    assert!(figures.requests.contains(&("other", "2xx", 1)) && figures.requests.contains(&("GET", "5xx", 1)), "{:?}", figures.requests);
    assert_eq!(figures.cumulative.last().copied(), Some(1), "300 seconds is slower than every bound");
    assert_eq!(figures.count, 2);
}

#[tokio::test]
async fn sign_in_events_follow_what_happens() {
    let env = testutil::env().await;
    env.user("amy", true).await;
    let login = |name: &str, pw: &str| {
        let req = serde_json::from_value(json!({ "username": name, "password": pw })).unwrap();
        crate::signin::login(State(env.st.clone()), addr("10.0.0.7"), HeaderMap::new(), Json(req))
    };
    assert!(login("amy", testutil::password()).await.is_ok());
    assert!(login("amy", "wrong-password").await.is_err());
    assert!(login("amy", "wrong-password").await.is_err());
    assert!(login("nobody-by-that-name", "x").await.is_err());
    let families = page(&env);
    assert_eq!(value(&families, "thirtyfile_sign_in_events_total", &[("event", "login")]), Some(1.0));
    assert_eq!(value(&families, "thirtyfile_sign_in_events_total", &[("event", "bad_password")]), Some(2.0));
    assert_eq!(value(&families, "thirtyfile_sign_in_events_total", &[("event", "unknown_user")]), Some(1.0));
}

#[tokio::test]
async fn tasks_are_counted_when_they_end_and_while_they_run() {
    let env = testutil::env().await;
    let amy = env.user("amy", false).await;
    let done = crate::jobs::start(&env.st, amy.id, "scan", crate::jobs::Limit::Changes).unwrap();
    crate::jobs::spawn(&env.st, &done, async { Ok(Default::default()) });
    assert_eq!(crate::jobs::wait_for(&env.st, &done.id).await.state, "done");
    let failed = crate::jobs::start(&env.st, amy.id, "scan", crate::jobs::Limit::Changes).unwrap();
    crate::jobs::spawn(&env.st, &failed, async { Err(crate::error::AppError::conflict("An item with the same name already exists")) });
    assert_eq!(crate::jobs::wait_for(&env.st, &failed.id).await.state, "failed");
    let panicked = crate::jobs::start(&env.st, amy.id, "move", crate::jobs::Limit::Changes).unwrap();
    crate::jobs::spawn(&env.st, &panicked, async { panic!("a bug") });
    crate::jobs::wait_for(&env.st, &panicked.id).await;
    // One still running
    let (go, wait) = tokio::sync::oneshot::channel::<()>();
    let running = crate::jobs::start(&env.st, amy.id, "copy", crate::jobs::Limit::Changes).unwrap();
    crate::jobs::spawn(&env.st, &running, async move {
        let _ = wait.await;
        Ok(Default::default())
    });
    let families = page(&env);
    assert_eq!(value(&families, "thirtyfile_tasks_finished_total", &[("kind", "scan"), ("outcome", "done")]), Some(1.0));
    assert_eq!(value(&families, "thirtyfile_tasks_finished_total", &[("kind", "scan"), ("outcome", "failed")]), Some(1.0));
    assert_eq!(value(&families, "thirtyfile_tasks_finished_total", &[("kind", "move"), ("outcome", "failed")]), Some(1.0));
    assert_eq!(value(&families, "thirtyfile_tasks_running", &[("kind", "copy")]), Some(1.0));
    go.send(()).unwrap();
    crate::jobs::wait_for(&env.st, &running.id).await;
    assert_eq!(value(&page(&env), "thirtyfile_tasks_running", &[("kind", "copy")]), None);
}

#[tokio::test]
async fn storage_operations_are_totals_that_the_usage_sampler_does_not_reset() {
    let env = testutil::env().await;
    let meters = &env.st.part::<crate::usage::Memory>().meters;
    meters.record("local", Op::Write, Work::Foreground, Duration::from_millis(30), Outcome::Ok, 1000);
    meters.record("local", Op::Write, Work::Background, Duration::from_millis(700), Outcome::Ok, 500);
    meters.record("local", Op::Write, Work::Foreground, Duration::from_millis(2), Outcome::Error, 0);
    meters.record("local", Op::Read, Work::Foreground, Duration::from_secs(90), Outcome::Timeout, 0);
    let check = |families: &[Family]| {
        assert_eq!(value(families, "thirtyfile_storage_operations_total", &[("location", "local"), ("op", "write"), ("outcome", "ok")]), Some(2.0));
        assert_eq!(value(families, "thirtyfile_storage_operations_total", &[("location", "local"), ("op", "write"), ("outcome", "error")]), Some(1.0));
        assert_eq!(value(families, "thirtyfile_storage_operations_total", &[("location", "local"), ("op", "read"), ("outcome", "timeout")]), Some(1.0));
        assert_eq!(value(families, "thirtyfile_storage_bytes_total", &[("location", "local"), ("op", "write")]), Some(1500.0));
        assert_eq!(value(families, "thirtyfile_storage_operation_duration_seconds_count", &[("location", "local"), ("op", "write")]), Some(3.0));
        let under_a_second = value(families, "thirtyfile_storage_operation_duration_seconds_bucket", &[("location", "local"), ("op", "write"), ("le", "1")]);
        assert_eq!(under_a_second, Some(3.0));
        let under_50ms = value(families, "thirtyfile_storage_operation_duration_seconds_bucket", &[("location", "local"), ("op", "write"), ("le", "0.05")]);
        assert_eq!(under_50ms, Some(2.0), "the 30 ms and the 2 ms one");
    };
    check(&page(&env));
    // The sampler takes the window's counters every five minutes: the totals stay
    assert!(!meters.take().is_empty());
    check(&page(&env));
}

#[tokio::test]
async fn the_figures_looked_at_every_fifteen_seconds_follow_the_server() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    env.sign_in(&amy, "Browser").await;
    sqlx::query("UPDATE users SET disabled = 1 WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
    crate::metrics::beat(&env.st, "maintenance");
    let snapshot = collect::take(&env.st).await.unwrap();
    assert_eq!((snapshot.sessions, snapshot.accounts_active, snapshot.accounts_disabled), (1, 1, 1));
    assert!(snapshot.database_bytes > 0);
    assert!(snapshot.volumes.iter().any(|v| v.volume == "data" && v.total > 0 && v.free <= v.total), "{:?}", snapshot.volumes);
    assert!(snapshot.locations.iter().any(|l| l.online));
    *env.st.part::<Memory>().snapshot.write() = snapshot;
    let families = page(&env);
    assert_eq!(value(&families, "thirtyfile_sessions", &[]), Some(1.0));
    assert_eq!(value(&families, "thirtyfile_accounts", &[("state", "active")]), Some(1.0));
    assert_eq!(value(&families, "thirtyfile_accounts", &[("state", "disabled")]), Some(1.0));
    assert!(value(&families, "thirtyfile_database_size_bytes", &[]).unwrap() > 0.0);
    assert!(value(&families, "thirtyfile_disk_free_bytes", &[("volume", "data")]).is_some());
    let beat_at = value(&families, "thirtyfile_background_last_success_timestamp_seconds", &[("job", "maintenance")]).unwrap();
    assert!((beat_at - crate::util::now() as f64).abs() < 5.0);
}

#[tokio::test]
async fn backup_and_replica_ages_and_alerts_come_from_their_tables() {
    let env = testutil::env().await;
    let t = crate::util::now();
    sqlx::query("INSERT INTO backup_sets (id, kind, name, dest_location, created_at) VALUES ('s1', 'policy', 'Nightly', 'local', 1), ('s2', 'policy', 'Never made', 'local', 1)")
        .execute(&env.st.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO backup_policies (set_id, enabled, mode, created_at, updated_at) VALUES ('s1', 1, 'scheduled', 1, 1), ('s2', 1, 'scheduled', 1, 1)")
        .execute(&env.st.db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO backup_snapshots (id, set_id, state, created_at, completed_at) VALUES ('n1', 's1', 'complete', ?1, ?1), ('n2', 's1', 'making', ?2, NULL)",
    )
    .bind(t - 3600)
    .bind(t - 10)
    .execute(&env.st.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO alert_conditions (key, kind, since) VALUES ('offline:a', 'offline', 1), ('offline:b', 'offline', 1), ('certificate', 'certificate', 1)",
    )
    .execute(&env.st.db)
    .await
    .unwrap();
    let snapshot = collect::take(&env.st).await.unwrap();
    *env.st.part::<Memory>().snapshot.write() = snapshot;
    let families = page(&env);
    let age = value(&families, "thirtyfile_backup_last_complete_age_seconds", &[("policy", "Nightly")]).unwrap();
    assert!((3600.0..3700.0).contains(&age), "{age}: the snapshot still being made does not count");
    assert_eq!(value(&families, "thirtyfile_backup_last_complete_age_seconds", &[("policy", "Never made")]), None);
    assert_eq!(value(&families, "thirtyfile_backup_policies", &[("snapshot", "made")]), Some(1.0));
    assert_eq!(value(&families, "thirtyfile_backup_policies", &[("snapshot", "none")]), Some(1.0));
    assert_eq!(value(&families, "thirtyfile_alerts_firing", &[("kind", "offline")]), Some(2.0));
    assert_eq!(value(&families, "thirtyfile_alerts_firing", &[("kind", "certificate")]), Some(1.0));
}

// ───────────── Privacy and safety ─────────────

#[tokio::test]
async fn no_name_of_a_person_or_a_file_is_on_the_page() {
    let env = testutil::env().await;
    let evil = "evil\"name\\\n# HELP x y {a=\"b\"}";
    let amy = env.user("amy-the-secret-user", true).await;
    sqlx::query("UPDATE users SET display_name = ?, email = 'secret.person@example.com' WHERE id = ?").bind(evil).bind(amy.id).execute(&env.st.db).await.unwrap();
    let folder = env.folder(&amy, amy.root(), "Secret folder \"quoted\"").await;
    env.upload(&amy, &folder, "salary-2026-secret.txt", b"x").await;
    let (_, cookie) = env.sign_in(&amy, "Secret-Browser/9.9").await;
    assert!(!cookie.is_empty());
    // Sign-ins and requests naming them, from an address that is a name too
    let login = |name: &str| {
        let req = serde_json::from_value(json!({ "username": name, "password": "wrong" })).unwrap();
        crate::signin::login(State(env.st.clone()), addr("10.9.8.7"), HeaderMap::new(), Json(req))
    };
    assert!(login(evil).await.is_err());
    assert!(login("amy-the-secret-user").await.is_err());
    let app = crate::app::routes::router(env.st.clone());
    let req = Request::builder()
        .uri("/api/nodes/salary-2026-secret.txt?q=secret.person@example.com")
        .header(header::COOKIE, cookie)
        .extension(addr("10.9.8.7"))
        .body(Body::empty())
        .unwrap();
    let _ = app.oneshot(req).await.unwrap();
    *env.st.part::<Memory>().snapshot.write() = collect::take(&env.st).await.unwrap();

    let out = render(&env.st);
    parse(&out).unwrap_or_else(|e| panic!("{e}\n{out}"));
    for secret in ["amy-the-secret-user", "secret.person", "Secret folder", "salary-2026", "Secret-Browser", "10.9.8.7", "evil", "quoted", "cookie"] {
        assert!(!out.contains(secret), "{secret} is on the page:\n{out}");
    }
}

#[tokio::test]
async fn names_an_administrator_chose_are_escaped_and_read_back() {
    let env = testutil::env().await;
    let weird = "NAS \"two\"\\ \n} {x=\"y\"} 檔案";
    *env.st.part::<Memory>().snapshot.write() = Snapshot {
        taken_at: 1,
        locations: vec![Location { name: weird.into(), online: true, live_bytes: 5, ..Default::default() }],
        backups: vec![(weird.into(), Some(60))],
        replicas: vec![(weird.into(), Some(120))],
        ..Default::default()
    };
    let families = page(&env);
    assert_eq!(value(&families, "thirtyfile_storage_location_live_bytes", &[("location", weird)]), Some(5.0));
    assert_eq!(value(&families, "thirtyfile_backup_last_complete_age_seconds", &[("policy", weird)]), Some(60.0));
    assert_eq!(value(&families, "thirtyfile_replica_sync_age_seconds", &[("policy", weird)]), Some(120.0));
}

#[tokio::test]
async fn a_scrape_reads_memory_only() {
    let env = testutil::env().await;
    let token = on(&env).await;
    *env.st.part::<Memory>().snapshot.write() = collect::take(&env.st).await.unwrap();
    // With the write lock held, as a long change holds it, and with the database gone altogether, the page still comes
    let _writer = env.st.write_lock.lock().await;
    env.st.db.close().await;
    let res = tokio::time::timeout(Duration::from_secs(5), ask(&env.st, "203.0.113.9", bearer(&token))).await.expect("answered without waiting for the lock");
    assert_eq!(res.status(), StatusCode::OK);
    assert!(parse(&text(res).await).is_ok());
}

#[tokio::test]
async fn nothing_is_looked_at_while_monitoring_is_off() {
    let env = testutil::env().await;
    // The task that keeps the figures current only works while it is on: a page that is off has no snapshot to keep
    assert_eq!(env.st.part::<Memory>().snapshot.read().taken_at, 0);
    let families = page(&env);
    assert!(families.iter().all(|f| f.name != "thirtyfile_sessions" && f.name != "thirtyfile_database_size_bytes"), "no figures before the first look");
}
