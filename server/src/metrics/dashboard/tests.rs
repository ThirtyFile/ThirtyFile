use std::time::Duration;

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
};
use serde_json::{Value, json};

use super::*;
use crate::{
    auth::Admin,
    state::LocationHealth,
    testutil::{self, TestEnv},
};

fn counts_with(requests: u64, errors: u64, cumulative: &[u64]) -> Counts {
    Counts { requests, errors, cumulative: cumulative.to_vec(), ..Default::default() }
}

fn value(figures: &[(&'static str, f64)], name: &str) -> Option<f64> {
    figures.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
}

async fn page(env: &TestEnv, span: Option<i64>) -> Value {
    let Json(v) = get(State(env.st.clone()), Admin(env.admin().await), Query(Span { span })).await.unwrap();
    v
}

async fn put(env: &TestEnv, series: &str, at: i64, value: f64) {
    let name = SERIES.iter().find(|(n, _)| *n == series).unwrap().0;
    write(&env.st, at, &[(name, value)]).await.unwrap();
}

#[test]
fn what_happened_is_the_difference_of_two_looks() {
    // 100 requests, then 300: 200 more, of which 10 were errors; the cumulative counts are under each bound
    let before = counts_with(100, 4, &[10, 40, 60, 80, 90, 95, 100, 100, 100, 100, 100, 100, 100, 100]);
    let after = counts_with(300, 14, &[10, 40, 160, 270, 290, 295, 300, 300, 300, 300, 300, 300, 300, 300]);
    let figures = deltas(&before, &after);
    assert_eq!(value(&figures, "requests"), Some(200.0));
    assert_eq!(value(&figures, "errors"), Some(10.0));
    // 95 of every 100 of the 200 is 190: the first bound with that many new answers under it is 0.05 s (270 - 80)
    assert_eq!(value(&figures, "latency_p95_ms"), Some(50.0));
}

#[test]
fn a_period_with_no_requests_has_no_latency_and_a_restart_counts_from_nothing() {
    let quiet = counts_with(50, 1, &[1; 14]);
    assert_eq!(value(&deltas(&quiet, &quiet), "latency_p95_ms"), None, "nothing answered, nothing to say about how long");
    assert_eq!(value(&deltas(&quiet, &quiet), "requests"), Some(0.0));
    // The counters went back (the server restarted): no negative figures, no panic
    let restarted = counts_with(3, 0, &[3; 14]);
    let figures = deltas(&quiet, &restarted);
    assert_eq!((value(&figures, "requests"), value(&figures, "errors")), (Some(0.0), Some(0.0)));
    // More than every bound: said as the last one
    let slow = counts_with(10, 0, &[0; 14]);
    assert_eq!(value(&deltas(&Counts::default(), &slow), "latency_p95_ms"), Some(120_000.0));
}

#[tokio::test]
async fn a_sample_writes_what_was_counted_and_what_is_looked_at() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    env.sign_in(&amy, "Browser").await;
    let http = &env.st.part::<Memory>().http;
    let before = counts(&env.st);
    for _ in 0..8 {
        http.finished(&axum::http::Method::GET, 200, Duration::from_millis(20));
    }
    http.finished(&axum::http::Method::GET, 500, Duration::from_millis(900));
    http.finished(&axum::http::Method::POST, 502, Duration::from_millis(30));
    crate::metrics::sign_in_event(&env.st, "bad_password");
    crate::metrics::sign_in_event(&env.st, "login");
    env.st.part::<crate::usage::Memory>().meters.record(
        "local",
        crate::usage::meter::Op::Write,
        crate::usage::meter::Work::Foreground,
        Duration::from_millis(5),
        crate::usage::meter::Outcome::Error,
        0,
    );
    let t = 1_700_000_300;
    sample(&env.st, &before, t).await;
    let rows: Vec<(String, i64, f64)> = sqlx::query_as("SELECT series, at, value FROM metrics_samples ORDER BY series").fetch_all(&env.st.db).await.unwrap();
    let got = |name: &str| rows.iter().find(|r| r.0 == name).map(|r| (r.1, r.2));
    assert_eq!(got("requests"), Some((t - STEP, 10.0)));
    assert_eq!(got("errors").map(|r| r.1), Some(2.0));
    assert_eq!(got("failed_sign_ins").map(|r| r.1), Some(1.0), "a login that worked is not a failed one");
    assert_eq!(got("storage_ops").map(|r| r.1), Some(1.0));
    assert_eq!(got("storage_errors").map(|r| r.1), Some(1.0));
    assert_eq!(got("latency_p95_ms").map(|r| r.1), Some(1000.0), "9 of 10 under 50 ms, the 95th is the slow one: under 1 s");
    assert_eq!(got("sessions").map(|r| r.1), Some(1.0));
    assert_eq!(got("tasks_running").map(|r| r.1), Some(0.0));
    assert!(got("disk_free_percent").is_some_and(|(_, p)| (0.0..=100.0).contains(&p)));
    // Every series is one that is known
    assert!(rows.iter().all(|r| SERIES.iter().any(|(n, _)| *n == r.0)), "{rows:?}");
}

#[tokio::test]
async fn what_is_older_than_eight_days_is_removed() {
    let env = testutil::env().await;
    let t = 1_700_000_000;
    put(&env, "requests", t - 9 * 86400, 1.0).await;
    put(&env, "requests", t - 7 * 86400, 2.0).await;
    put(&env, "requests", t, 3.0).await;
    assert_eq!(prune(&env.st, t).await.unwrap(), 1);
    let left: Vec<(f64,)> = sqlx::query_as("SELECT value FROM metrics_samples ORDER BY at").fetch_all(&env.st.db).await.unwrap();
    assert_eq!(left, [(2.0,), (3.0,)]);
}

#[tokio::test]
async fn a_week_is_folded_into_half_hours_by_what_each_series_means() {
    let env = testutil::env().await;
    let t = period(now());
    let base = period(t - 3 * 86400) - period(t - 3 * 86400).rem_euclid(1800);
    // Six five-minute periods of one half hour, and one of the next
    for (i, (requests, sessions, free)) in
        [(10.0, 3.0, 50.0), (20.0, 7.0, 40.0), (30.0, 5.0, 45.0), (1.0, 1.0, 60.0), (2.0, 2.0, 61.0), (3.0, 4.0, 62.0)].into_iter().enumerate()
    {
        put(&env, "requests", base + i as i64 * STEP, requests).await;
        put(&env, "sessions", base + i as i64 * STEP, sessions).await;
        put(&env, "disk_free_percent", base + i as i64 * STEP, free).await;
    }
    put(&env, "requests", base + 1800, 100.0).await;
    let week = page(&env, Some(7 * 86400)).await;
    assert_eq!((week["span"].clone(), week["step"].clone()), (json!(7 * 86400), json!(1800)));
    let s = &week["series"];
    assert_eq!(s["requests"], json!([[base, 66.0], [base + 1800, 100.0]]), "counts add up");
    assert_eq!(s["sessions"], json!([[base, 7.0]]), "the most of sessions");
    assert_eq!(s["disk_free_percent"], json!([[base, 40.0]]), "the least free disk");
    // The day keeps the five minutes, and a figure older than the day is not in it
    put(&env, "requests", t - 2 * 86400, 5.0).await;
    let day = page(&env, None).await;
    assert_eq!(day["step"], json!(STEP));
    assert_eq!(day["series"]["requests"], json!([]), "all three days back, none today");
    // Any other span asked for is a day
    assert_eq!(page(&env, Some(5)).await["span"], json!(86400));
}

#[tokio::test]
async fn the_page_tells_how_the_server_is_and_what_is_firing() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    env.sign_in(&amy, "Browser").await;
    let ok = page(&env, None).await;
    assert_eq!((ok["status"].clone(), ok["warnings"].clone()), (json!("ok"), json!([])));
    assert_eq!((ok["numbers"]["sessions"].clone(), ok["numbers"]["active_accounts"].clone()), (json!(1), json!(1)));
    assert!(ok["numbers"]["disk_free_bytes"].as_u64().is_some_and(|b| b > 0));

    // A storage location that can't be reached, and an alert for it
    env.st.location_health.lock().insert("nas".into(), LocationHealth { ok: false, error: Some("down".into()), checked_at: 1 });
    sqlx::query(
        "INSERT INTO alert_conditions (key, kind, params, since) VALUES ('offline:nas', 'offline', '{\"location\":\"NAS\"}', 7), ('old', 'offline', '{}', 1)",
    )
    .execute(&env.st.db)
    .await
    .unwrap();
    sqlx::query("UPDATE alert_conditions SET active = 0 WHERE key = 'old'").execute(&env.st.db).await.unwrap();
    let degraded = page(&env, None).await;
    assert_eq!(degraded["status"], "degraded");
    assert!(degraded["warnings"][0].as_str().unwrap().contains("nas"));
    assert_eq!(degraded["alerts"], json!([{ "kind": "offline", "params": { "location": "NAS" }, "since": 7 }]), "only what is firing now");
    assert_eq!(degraded["monitoring"], json!(false));
}

#[tokio::test]
async fn the_latest_server_errors_and_failed_sign_ins_are_listed_newest_first() {
    let env = testutil::env().await;
    for (i, severity) in ["error", "warning", "error", "error", "error", "error", "error"].iter().enumerate() {
        sqlx::query("INSERT INTO error_log (at, first_at, count, source, severity, kind, route, status, message, fingerprint) VALUES (?1, ?1, ?2, 'backend', ?3, 'server', '/api/x', 500, ?4, ?5)")
            .bind(100 + i as i64)
            .bind(i as i64 + 1)
            .bind(severity)
            .bind(format!("A server error occurred {i}"))
            .bind(format!("f{i}"))
            .execute(&env.st.db)
            .await
            .unwrap();
    }
    for (i, event) in ["login", "bad_password", "locked", "logout", "unknown_user"].iter().enumerate() {
        sqlx::query("INSERT INTO login_log (at, username, event, ip) VALUES (?, ?, ?, '203.0.113.9')")
            .bind(200 + i as i64)
            .bind(format!("name{i}"))
            .bind(event)
            .execute(&env.st.db)
            .await
            .unwrap();
    }
    let v = page(&env, None).await;
    let errors = v["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 5);
    assert_eq!(errors[0]["at"], 106);
    assert!(errors.iter().all(|e| e["at"] != 101), "a warning is not a server error");
    assert_eq!(errors[0]["message"], "A server error occurred 6");
    let failed: Vec<(&str, &str)> =
        v["failed_sign_ins"].as_array().unwrap().iter().map(|f| (f["event"].as_str().unwrap(), f["username"].as_str().unwrap())).collect();
    assert_eq!(failed, [("unknown_user", "name4"), ("locked", "name2"), ("bad_password", "name1")], "logins and logouts are not failures");
}

#[tokio::test]
async fn only_an_administrator_sees_it() {
    use tower::ServiceExt;
    let env = testutil::env().await;
    let app = crate::app::routes::router(env.st.clone());
    let amy = env.user("amy", true).await;
    let (_, amy_cookie) = env.sign_in(&amy, "Browser").await;
    let (_, admin_cookie) = env.sign_in(&env.admin().await, "Browser").await;
    let ask = |cookie: String| {
        let req = axum::http::Request::builder()
            .uri("/api/admin/dashboard")
            .header(axum::http::header::COOKIE, cookie)
            .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from(([10, 0, 0, 1], 5000))))
            .body(axum::body::Body::empty())
            .unwrap();
        app.clone().oneshot(req)
    };
    assert_eq!(ask(amy_cookie).await.unwrap().status(), StatusCode::FORBIDDEN);
    assert_eq!(ask("tf_session=nothing".into()).await.unwrap().status(), StatusCode::UNAUTHORIZED);
    assert_eq!(ask(admin_cookie).await.unwrap().status(), StatusCode::OK);
}
