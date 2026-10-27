//! Control panel › Dashboard: how the server is doing, for an administrator who opens the page.
//!
//! A task writes one figure per five-minute period for a fixed list of series (`SERIES`) into `metrics_samples`, and
//! keeps them eight days; the page asks for the last day or week with the state of the server now: whether it is well
//! (the health check's answer), what alerts are firing, a few numbers, and the latest server errors and failed sign-ins.
//! It works whether or not `/metrics` is turned on.

use std::{collections::BTreeMap, time::Duration};

use axum::{
    Json,
    extract::{Query, State},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Memory, instruments::HTTP_BOUNDS};
use crate::{auth::Admin, error::AppResult, state::AppState, util::now};

/// How long a period is, in seconds
pub const STEP: i64 = 300;
/// How long figures are kept
const KEEP: i64 = 8 * 86400;
/// The sign-in events that count as a failed sign-in
const FAILED: [&str; 7] = ["bad_password", "unknown_user", "locked", "2fa_failed", "app_password_failed", "ftp_failed", "password_off"];

/// How the figures of a series become one when the page asks for a week (30 minutes at a time)
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Fold {
    Sum,
    Max,
    Min,
}

/// The series: their names, and how they are folded
pub const SERIES: [(&str, Fold); 10] = [
    ("requests", Fold::Sum),
    ("errors", Fold::Sum),
    ("latency_p95_ms", Fold::Max),
    ("sessions", Fold::Max),
    ("tasks_running", Fold::Max),
    ("uploads", Fold::Max),
    ("storage_ops", Fold::Sum),
    ("storage_errors", Fold::Sum),
    ("failed_sign_ins", Fold::Sum),
    ("disk_free_percent", Fold::Min),
];

/// What the counters said at one moment: the differences between two of these are what happened in between
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Counts {
    pub requests: u64,
    /// Answered with a status of 500 or more
    pub errors: u64,
    /// How many requests answered under each of `HTTP_BOUNDS`
    pub cumulative: Vec<u64>,
    pub storage_ops: u64,
    /// Storage operations that failed or timed out
    pub storage_errors: u64,
    pub failed_sign_ins: u64,
}

/// The counters now
pub fn counts(st: &AppState) -> Counts {
    let memory = st.part::<Memory>();
    let http = memory.http.figures();
    let (mut storage_ops, mut storage_errors) = (0, 0);
    for per in st.part::<crate::usage::Memory>().meters.totals().values() {
        for w in per.values() {
            storage_ops += w.count;
            storage_errors += w.errors + w.timeouts;
        }
    }
    Counts {
        requests: http.count,
        errors: http.requests.iter().filter(|(_, class, _)| *class == "5xx").map(|(_, _, n)| n).sum(),
        cumulative: http.cumulative,
        storage_ops,
        storage_errors,
        failed_sign_ins: memory.sign_ins().iter().filter(|(event, _)| FAILED.contains(event)).map(|(_, n)| n).sum(),
    }
}

/// What the counters say happened between two looks (a counter that went back, the server having restarted, is
/// counted from nothing)
pub fn deltas(before: &Counts, after: &Counts) -> Vec<(&'static str, f64)> {
    let d = |a: u64, b: u64| a.saturating_sub(b);
    let requests = d(after.requests, before.requests);
    let mut out = vec![
        ("requests", requests as f64),
        ("errors", d(after.errors, before.errors) as f64),
        ("storage_ops", d(after.storage_ops, before.storage_ops) as f64),
        ("storage_errors", d(after.storage_errors, before.storage_errors) as f64),
        ("failed_sign_ins", d(after.failed_sign_ins, before.failed_sign_ins) as f64),
    ];
    if requests > 0 {
        // The first bound that 95 of 100 answers came under; slower than every bound is said as the last bound
        let wanted = (requests * 95).div_ceil(100);
        let under = |i: usize| d(after.cumulative.get(i).copied().unwrap_or(0), before.cumulative.get(i).copied().unwrap_or(0));
        let bound = (0..HTTP_BOUNDS.len()).find(|i| under(*i) >= wanted).map_or(HTTP_BOUNDS[HTTP_BOUNDS.len() - 1], |i| HTTP_BOUNDS[i]);
        out.push(("latency_p95_ms", bound * 1000.0));
    }
    out
}

/// The figures that are looked at rather than counted
async fn gauges(st: &AppState, t: i64) -> AppResult<Vec<(&'static str, f64)>> {
    let (sessions,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sessions WHERE expires_at > ?").bind(t).fetch_one(&st.db).await?;
    let (uploads,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM uploads WHERE expires_at > ?").bind(t).fetch_one(&st.db).await?;
    let running = st.part::<crate::jobs::Memory>().jobs.lock().values().filter(|j| j.state == "running").count();
    let mut out = vec![("sessions", sessions as f64), ("uploads", uploads as f64), ("tasks_running", running as f64)];
    if let Some(free) = free_percent(st).await {
        out.push(("disk_free_percent", free));
    }
    Ok(out)
}

/// The least free space of the disks of the data and storage folders, in percent
async fn free_percent(st: &AppState) -> Option<f64> {
    let mut least: Option<f64> = None;
    for path in [&st.data_dir, &st.storage_dir] {
        if let Some((free, total)) = crate::util::disk_space_soon(path).await
            && total > 0
        {
            let percent = free as f64 * 100.0 / total as f64;
            least = Some(least.map_or(percent, |l| l.min(percent)));
        }
    }
    least
}

/// Writes the figures of the period that starts at `at`
pub async fn write(st: &AppState, at: i64, figures: &[(&'static str, f64)]) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    for (series, value) in figures {
        sqlx::query("INSERT OR REPLACE INTO metrics_samples (series, at, value) VALUES (?, ?, ?)").bind(series).bind(at).bind(value).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Removes what is older than is kept
pub async fn prune(st: &AppState, t: i64) -> AppResult<u64> {
    let _w = st.write_lock.lock().await;
    Ok(sqlx::query("DELETE FROM metrics_samples WHERE at < ?").bind(t - KEEP).execute(&st.db).await?.rows_affected())
}

/// The start of the period `t` is in
fn period(t: i64) -> i64 {
    t - t.rem_euclid(STEP)
}

/// Writes the figures of the period that ended at `end`, given the counters at its start; returns the counters now
pub async fn sample(st: &AppState, before: &Counts, end: i64) -> Counts {
    let after = counts(st);
    let mut figures = deltas(before, &after);
    match gauges(st, end).await {
        Ok(g) => figures.extend(g),
        Err(e) => tracing::warn!("Couldn't look at the figures of the dashboard: {}", e.message),
    }
    if let Err(e) = write(st, end - STEP, &figures).await {
        tracing::warn!("Couldn't write the figures of the dashboard: {}", e.message);
    }
    after
}

pub fn spawn(st: AppState) {
    crate::util::supervise("dashboard samples", move |_| run(st.clone()));
}

async fn run(st: AppState) {
    let mut before = counts(&st);
    loop {
        // A few seconds after the end of the period, so what happened in it is all counted
        let t = now();
        let end = period(t) + STEP;
        tokio::time::sleep(Duration::from_secs((end - t) as u64 + 2)).await;
        before = sample(&st, &before, end).await;
        if (end / STEP) % 12 == 0
            && let Err(e) = prune(&st, end).await
        {
            tracing::warn!("Couldn't delete old figures of the dashboard: {}", e.message);
        }
    }
}

// ───────────── The page ─────────────

#[derive(Deserialize, Default)]
pub struct Span {
    /// Seconds to look back: a day (the default) or a week
    span: Option<i64>,
}

/// The figures of the last `span` seconds, folded into `bucket`-second periods: series → (start, value)
async fn series(st: &AppState, from: i64, bucket: i64) -> AppResult<BTreeMap<&'static str, Vec<(i64, f64)>>> {
    let rows: Vec<(String, i64, f64)> =
        sqlx::query_as("SELECT series, at, value FROM metrics_samples WHERE at >= ? ORDER BY at").bind(from).fetch_all(&st.db).await?;
    let mut out = BTreeMap::new();
    for (name, fold) in SERIES {
        let mut folded: Vec<(i64, f64)> = Vec::new();
        for (_, at, value) in rows.iter().filter(|(series, ..)| series == name) {
            let start = at - at.rem_euclid(bucket);
            match folded.last_mut() {
                Some((s, v)) if *s == start => {
                    *v = match fold {
                        Fold::Sum => *v + value,
                        Fold::Max => v.max(*value),
                        Fold::Min => v.min(*value),
                    }
                }
                _ => folded.push((start, *value)),
            }
        }
        out.insert(name, folded);
    }
    Ok(out)
}

pub async fn get(State(st): State<AppState>, _: Admin, Query(q): Query<Span>) -> AppResult<Json<Value>> {
    let t = now();
    let span = if q.span == Some(7 * 86400) { 7 * 86400 } else { 86400 };
    let bucket = if span > 86400 { 1800 } else { STEP };
    let from = period(t - span);

    let report = crate::app::health::report(&st).await;
    let (status, warnings) = match &report {
        Ok(r) => (r.status(), r.warnings.clone()),
        Err(_) => ("error", vec![]),
    };
    let alerts: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT kind, params, since FROM alert_conditions WHERE active = 1 ORDER BY since").fetch_all(&st.db).await?;
    let alerts: Vec<Value> = alerts
        .into_iter()
        .map(|(kind, params, since)| json!({ "kind": kind, "params": serde_json::from_str::<Value>(&params).unwrap_or(Value::Null), "since": since }))
        .collect();

    let (sessions,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sessions WHERE expires_at > ?").bind(t).fetch_one(&st.db).await?;
    let (active,): (i64,) =
        sqlx::query_as("SELECT COUNT(DISTINCT user_id) FROM sessions WHERE COALESCE(last_used_at, created_at) > ?").bind(t - 86400).fetch_one(&st.db).await?;
    let gauges = gauges(&st, t).await?;
    let gauge = |name: &str| gauges.iter().find(|(n, _)| *n == name).map(|(_, v)| *v);
    let free_bytes = report.as_ref().ok().and_then(|r| r.disks.values().filter_map(|d| d["free_bytes"].as_u64()).min());

    let errors: Vec<(i64, String, String, Option<i64>, i64, String)> =
        sqlx::query_as("SELECT at, kind, route, status, count, substr(message, 1, 160) FROM error_log WHERE severity = 'error' ORDER BY at DESC LIMIT 5")
            .fetch_all(&st.db)
            .await?;
    let failed: Vec<(i64, String, String, String)> = sqlx::query_as(
        "SELECT at, event, username, ip FROM login_log WHERE event IN ('bad_password', 'unknown_user', 'locked', '2fa_failed', 'app_password_failed', 'ftp_failed', 'password_off') ORDER BY id DESC LIMIT 5",
    )
    .fetch_all(&st.db)
    .await?;

    Ok(Json(json!({
        "now": t,
        "span": span,
        "step": bucket,
        "from": from,
        "status": status,
        "warnings": warnings,
        "alerts": alerts,
        "numbers": {
            "sessions": sessions,
            "active_accounts": active,
            "tasks_running": gauge("tasks_running"),
            "uploads": gauge("uploads"),
            "disk_free_percent": gauge("disk_free_percent"),
            "disk_free_bytes": free_bytes,
        },
        "series": series(&st, from, bucket).await?,
        "errors": errors.into_iter().map(|(at, kind, route, status, count, message)| json!({ "at": at, "kind": kind, "route": route, "status": status, "count": count, "message": message })).collect::<Vec<_>>(),
        "failed_sign_ins": failed.into_iter().map(|(at, event, username, ip)| json!({ "at": at, "event": event, "username": username, "ip": ip })).collect::<Vec<_>>(),
        "monitoring": st.part::<Memory>().settings.read().enabled,
    })))
}

#[cfg(test)]
mod tests;
