//! The Storage usage API (administrators): what each location holds now and how its operations went in the last
//! hour, its history over a chosen range, and the thresholds that raise alerts. Everything is read from the samples
//! and the counters in memory: a page refresh never scans a disk or a bucket.

use std::collections::HashMap;

use axum::{
    Json,
    extract::{Query, State},
};
use serde::{Deserialize, Serialize};

use super::{
    meter::{Active, Op, Window, Windows, Work},
    sample::{CAPACITY_SPAN, Capacity, DAY, HOUR, OPS_SPAN, OpsRow},
};
use crate::{
    auth::Admin,
    error::{AppError, AppResult},
    state::AppState,
    util::now,
};

/// Operations of the overview cover the last hour
const RECENT: i64 = HOUR;
/// Fewer operations than this in the last hour don't make an error rate worth an alert
const MIN_OPS_FOR_ALERT: u64 = 20;

// ───────────── Thresholds ─────────────

/// When the page raises alerts (0 turns one off). Stored in the settings table.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Thresholds {
    /// A disk more full than this (percent)
    pub disk_percent: f64,
    /// More failed or timed-out operations than this in the last hour (percent)
    pub error_percent: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds { disk_percent: 90.0, error_percent: 5.0 }
    }
}

const THRESHOLDS_KEY: &str = "usage_thresholds";

async fn thresholds(st: &AppState) -> AppResult<Thresholds> {
    let saved = crate::db::get_setting(&st.db, THRESHOLDS_KEY).await?;
    Ok(saved.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default())
}

pub async fn set_thresholds(State(st): State<AppState>, _: Admin, Json(t): Json<Thresholds>) -> AppResult<Json<Thresholds>> {
    let valid = |v: f64| v.is_finite() && (0.0..=100.0).contains(&v);
    if !valid(t.disk_percent) || !valid(t.error_percent) {
        return Err(AppError::bad_request("Enter a percentage from 0 to 100"));
    }
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = crate::db::set_setting(&mut tx, THRESHOLDS_KEY, &serde_json::to_string(&t).unwrap()).await.map_err(AppError::from);
    crate::db::settle(tx, res).await?;
    Ok(Json(t))
}

// ───────────── Summaries ─────────────

/// Operations of one kind, summed over a period
#[derive(Debug, Serialize, PartialEq)]
pub struct Summary {
    pub op: &'static str,
    pub work: &'static str,
    #[serde(flatten)]
    pub figures: Figures,
}

/// Counted operations, with their durations in milliseconds; the durations are None without any operation (idle is
/// not "0 ms")
#[derive(Debug, Serialize, PartialEq)]
pub struct Figures {
    pub count: u64,
    pub errors: u64,
    pub timeouts: u64,
    pub bytes: u64,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub mean_ms: Option<f64>,
    pub max_ms: Option<f64>,
}

impl Figures {
    pub fn of(w: &Window) -> Figures {
        let ms = |us: f64| (us / 10.0).round() / 100.0;
        let timed = w.hist.count() > 0;
        Figures {
            count: w.count,
            errors: w.errors,
            timeouts: w.timeouts,
            bytes: w.bytes,
            p50_ms: w.percentile(0.5).map(ms),
            p95_ms: w.percentile(0.95).map(ms),
            mean_ms: timed.then(|| ms(w.total_us as f64 / w.hist.count() as f64)),
            max_ms: timed.then(|| ms(w.max_us as f64)),
        }
    }
}

fn summaries(per: &HashMap<(Op, Work), Window>) -> Vec<Summary> {
    let mut out: Vec<Summary> = per.iter().map(|((op, work), w)| Summary { op: op.as_str(), work: work.as_str(), figures: Figures::of(w) }).collect();
    out.sort_by_key(|s| (s.work, s.op));
    out
}

#[derive(sqlx::FromRow)]
struct KeyedOps {
    location_id: String,
    op: String,
    work: String,
    #[sqlx(flatten)]
    row: OpsRow,
}

#[derive(sqlx::FromRow)]
struct TimedOps {
    at: i64,
    op: String,
    #[sqlx(flatten)]
    row: OpsRow,
}

/// The last hour's operations by location: the rows written, and what is counted since the last of them
async fn recent(st: &AppState, now: i64) -> AppResult<Windows> {
    let rows: Vec<KeyedOps> =
        sqlx::query_as("SELECT location_id, op, work, count, errors, timeouts, bytes, total_us, max_us, hist FROM usage_ops WHERE span = ? AND at >= ?")
            .bind(OPS_SPAN)
            .bind(now - RECENT)
            .fetch_all(&st.db)
            .await?;
    let mut all = st.usage.snapshot();
    for r in rows {
        let (Some(op), Some(work)) = (Op::parse(&r.op), Work::parse(&r.work)) else { continue };
        all.entry(r.location_id).or_default().entry((op, work)).or_default().merge(&r.row.window());
    }
    Ok(all)
}

// ───────────── Overview ─────────────

/// A capacity sample, and whether it is older than the sampler should have left it
#[derive(Debug, Serialize)]
pub struct Sampled {
    #[serde(flatten)]
    pub capacity: Capacity,
    pub stale: bool,
}

#[derive(Debug, Serialize)]
pub struct LocationUsage {
    pub id: String,
    pub name: String,
    pub kind: String,
    /// Can be used now (settings loaded, the most recent check succeeded)
    pub online: bool,
    pub error: Option<String>,
    pub checked_at: Option<i64>,
    /// The most recent sample; None before the first one
    pub capacity: Option<Sampled>,
    /// Other locations on the same disk (its capacity is theirs too)
    pub same_disk_as: Vec<String>,
    pub active: Active,
    pub recent: Vec<Summary>,
}

/// Work waiting or running that uses the storage
#[derive(Debug, Default, Serialize)]
pub struct Queue {
    pub moves_queued: i64,
    pub moves_running: i64,
    pub moves_paused: i64,
    /// Tasks running (compressing, extracting, long copies and moves, scans…)
    pub jobs_running: i64,
    pub pending_deletes: i64,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Alert {
    /// "disk" (fuller than the threshold), "errors" (more failures than the threshold), "offline" or "stale"
    pub kind: &'static str,
    /// The location ('' for the data folder)
    pub location: String,
    pub value: f64,
    pub threshold: f64,
}

#[derive(Debug, Serialize)]
pub struct Overview {
    pub now: i64,
    pub ops_span: i64,
    pub capacity_span: i64,
    /// Seconds the operation summaries cover, up to now
    pub recent_secs: i64,
    pub thresholds: Thresholds,
    /// Everything together: the logical totals, and the disk of the data folder
    pub total: Option<Sampled>,
    pub locations: Vec<LocationUsage>,
    /// Operations on folder spaces that are on no storage location
    pub unplaced: Vec<Summary>,
    pub queue: Queue,
    pub alerts: Vec<Alert>,
}

/// The most recent capacity sample of each location
async fn latest(st: &AppState) -> AppResult<HashMap<String, Capacity>> {
    let rows: Vec<Capacity> = sqlx::query_as(
        "SELECT c.location_id, c.sampled_at, c.live_bytes, c.trash_bytes, c.version_bytes, c.store_bytes, c.folder_bytes,
                c.folder_version_bytes, c.pending_deletes, c.temp_bytes, c.backup_bytes, c.replica_bytes, c.disk_free,
                c.disk_total, c.disk_id, c.online, c.took_ms
         FROM usage_capacity c
         JOIN (SELECT location_id, MAX(at) AS at FROM usage_capacity WHERE span = ?1 GROUP BY location_id) m
           ON m.location_id = c.location_id AND m.at = c.at
         WHERE c.span = ?1",
    )
    .bind(CAPACITY_SPAN)
    .fetch_all(&st.db)
    .await?;
    Ok(rows.into_iter().map(|c| (c.location_id.clone(), c)).collect())
}

/// A sample is stale when two samples should have been taken since (the sampler stopped, or a sample failed)
fn sampled(c: Capacity, now: i64) -> Sampled {
    let stale = c.sampled_at < now - 2 * CAPACITY_SPAN - 60;
    Sampled { capacity: c, stale }
}

fn used_percent(c: &Capacity) -> Option<f64> {
    let (free, total) = (c.disk_free?, c.disk_total?);
    (total > 0).then(|| (total - free).max(0) as f64 * 100.0 / total as f64)
}

pub async fn overview(State(st): State<AppState>, _: Admin) -> AppResult<Json<Overview>> {
    Ok(Json(build_overview(&st, now()).await?))
}

pub async fn build_overview(st: &AppState, now: i64) -> AppResult<Overview> {
    let thresholds = thresholds(st).await?;
    let mut latest = latest(st).await?;
    let mut recent = recent(st, now).await?;
    let active = st.usage.active();
    let rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT id, name, kind FROM storage_locations ORDER BY (id = 'local') DESC, created_at, name").fetch_all(&st.db).await?;
    let mut locations = Vec::new();
    for (id, name, kind) in rows {
        let health = st.location_health.lock().unwrap().get(&id).cloned();
        locations.push(LocationUsage {
            online: st.location_offline(&id).is_none(),
            error: st.location_offline(&id),
            checked_at: health.map(|h| h.checked_at),
            capacity: latest.remove(&id).map(|c| sampled(c, now)),
            same_disk_as: Vec::new(),
            active: active.get(&id).copied().unwrap_or_default(),
            recent: recent.remove(&id).map(|per| summaries(&per)).unwrap_or_default(),
            id,
            name,
            kind,
        });
    }
    // Locations on the same disk name each other
    let disks: Vec<(String, Option<String>)> = locations.iter().map(|l| (l.id.clone(), l.capacity.as_ref().and_then(|c| c.capacity.disk_id.clone()))).collect();
    for l in &mut locations {
        let mine = l.capacity.as_ref().and_then(|c| c.capacity.disk_id.clone());
        l.same_disk_as = disks.iter().filter(|(id, d)| *id != l.id && mine.is_some() && *d == mine).map(|(id, _)| id.clone()).collect();
    }
    let total = latest.remove("").map(|c| sampled(c, now));
    let unplaced = recent.remove("").map(|per| summaries(&per)).unwrap_or_default();

    let mut queue = Queue::default();
    let moves: Vec<(String, i64)> =
        sqlx::query_as("SELECT state, COUNT(*) FROM space_moves WHERE state IN ('queued', 'running', 'paused') GROUP BY state").fetch_all(&st.db).await?;
    for (state, n) in moves {
        match state.as_str() {
            "queued" => queue.moves_queued = n,
            "running" => queue.moves_running = n,
            _ => queue.moves_paused = n,
        }
    }
    queue.jobs_running = st.jobs.lock().unwrap().values().filter(|j| j.state == "running").count() as i64;
    (queue.pending_deletes,) = sqlx::query_as("SELECT COUNT(*) FROM pending_blob_deletes").fetch_one(&st.db).await?;

    let alerts = alerts(&thresholds, &locations, total.as_ref());
    Ok(Overview { now, ops_span: OPS_SPAN, capacity_span: CAPACITY_SPAN, recent_secs: RECENT, thresholds, total, locations, unplaced, queue, alerts })
}

/// What passes the thresholds, or needs attention anyway (offline, no recent sample)
fn alerts(t: &Thresholds, locations: &[LocationUsage], total: Option<&Sampled>) -> Vec<Alert> {
    let mut out = Vec::new();
    // A stale sample says so rather than how full the disk was then
    let disk = |location: &str, c: &Sampled, out: &mut Vec<Alert>| {
        if c.stale {
            out.push(Alert { kind: "stale", location: location.to_string(), value: c.capacity.sampled_at as f64, threshold: 0.0 });
        } else if let Some(p) = used_percent(&c.capacity)
            && t.disk_percent > 0.0
            && p >= t.disk_percent
        {
            out.push(Alert { kind: "disk", location: location.to_string(), value: p, threshold: t.disk_percent });
        }
    };
    if let Some(c) = total {
        disk("", c, &mut out);
    }
    for l in locations {
        if !l.online {
            out.push(Alert { kind: "offline", location: l.id.clone(), value: 0.0, threshold: 0.0 });
        }
        if let Some(c) = &l.capacity {
            disk(&l.id, c, &mut out);
        }
        // Failures of what people and background work did: health checks of an offline location are already said above
        let (count, failed) =
            l.recent.iter().filter(|s| s.work != Work::Probe.as_str()).fold((0, 0), |(n, f), s| (n + s.figures.count, f + s.figures.errors + s.figures.timeouts));
        let rate = if count > 0 { failed as f64 * 100.0 / count as f64 } else { 0.0 };
        if t.error_percent > 0.0 && count >= MIN_OPS_FOR_ALERT && rate >= t.error_percent {
            out.push(Alert { kind: "errors", location: l.id.clone(), value: rate, threshold: t.error_percent });
        }
    }
    out
}

// ───────────── History ─────────────

#[derive(Deserialize)]
pub struct HistoryQuery {
    /// A location's id; blank for all of them together
    #[serde(default)]
    location: String,
    /// "6h", "2d", "30d" or "1y"
    range: Option<String>,
    /// "foreground" (default), "background", "probe" or "all"
    work: Option<String>,
}

/// A capacity sample in a history: the period it stands for
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct CapacityPoint {
    pub at: i64,
    #[sqlx(flatten)]
    #[serde(flatten)]
    pub capacity: Capacity,
}

#[derive(Debug, Serialize)]
pub struct OpsPoint {
    pub at: i64,
    #[serde(flatten)]
    pub figures: Figures,
}

#[derive(Debug, Serialize)]
pub struct OpsSeries {
    pub op: &'static str,
    /// Only periods with operations: a period without any has no point
    pub points: Vec<OpsPoint>,
}

/// When the disk is expected to be full at the pace it filled over `window_days`
#[derive(Debug, Serialize, PartialEq)]
pub struct Forecast {
    /// Days until full; None when it can't be told (`reason`)
    pub days: Option<f64>,
    /// "ok", "unknown_capacity" (no disk figures), "short" (under three days of samples), "not_growing" or "unsteady"
    pub reason: &'static str,
    pub window_days: f64,
    pub points: usize,
}

#[derive(Debug, Serialize)]
pub struct History {
    pub location: String,
    pub range: String,
    pub work: String,
    pub from: i64,
    pub to: i64,
    /// Seconds each capacity point and each operations point stand for
    pub capacity_span: i64,
    pub ops_span: i64,
    pub capacity: Vec<CapacityPoint>,
    pub ops: Vec<OpsSeries>,
    pub forecast: Forecast,
}

/// Seconds a range covers, and the tiers it reads (capacity, operations): at most 576 points a series
fn range_of(range: &str) -> Option<(i64, i64, i64)> {
    Some(match range {
        "6h" => (6 * HOUR, CAPACITY_SPAN, OPS_SPAN),
        "2d" => (2 * DAY, CAPACITY_SPAN, OPS_SPAN),
        "30d" => (30 * DAY, HOUR, HOUR),
        "1y" => (365 * DAY, DAY, DAY),
        _ => return None,
    })
}

pub async fn history(State(st): State<AppState>, _: Admin, Query(q): Query<HistoryQuery>) -> AppResult<Json<History>> {
    Ok(Json(build_history(&st, q, now()).await?))
}

pub async fn build_history(st: &AppState, q: HistoryQuery, now: i64) -> AppResult<History> {
    let range = q.range.unwrap_or_else(|| "2d".into());
    let (secs, capacity_span, ops_span) = range_of(&range).ok_or_else(|| AppError::bad_request("Unknown time range"))?;
    let work = q.work.unwrap_or_else(|| "foreground".into());
    let works: Vec<&str> = match work.as_str() {
        "all" => vec!["foreground", "background", "probe"],
        w if Work::parse(w).is_some() => vec![w],
        _ => return Err(AppError::bad_request("Unknown kind of work")),
    };
    if !q.location.is_empty() {
        let known: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM storage_locations WHERE id = ?").bind(&q.location).fetch_optional(&st.db).await?;
        if known.is_none() {
            return Err(AppError::not_found("Storage location not found"));
        }
    }
    let from = now - secs;
    let capacity: Vec<CapacityPoint> = sqlx::query_as(
        "SELECT at, location_id, sampled_at, live_bytes, trash_bytes, version_bytes, store_bytes, folder_bytes, folder_version_bytes,
                pending_deletes, temp_bytes, backup_bytes, replica_bytes, disk_free, disk_total, disk_id, online, took_ms
         FROM usage_capacity WHERE location_id = ? AND span = ? AND at >= ? ORDER BY at",
    )
    .bind(&q.location)
    .bind(capacity_span)
    .bind(bucket_start(from, capacity_span))
    .fetch_all(&st.db)
    .await?;
    // All locations together: every location's rows (and those of folders on none) merged
    let rows: Vec<TimedOps> = sqlx::query_as(
        "SELECT at, op, count, errors, timeouts, bytes, total_us, max_us, hist FROM usage_ops
         WHERE span = ?1 AND at >= ?2 AND work IN (SELECT value FROM json_each(?3)) AND (?4 = '' OR location_id = ?4)
         ORDER BY at",
    )
    .bind(ops_span)
    .bind(bucket_start(from, ops_span))
    .bind(serde_json::to_string(&works).unwrap())
    .bind(&q.location)
    .fetch_all(&st.db)
    .await?;
    let mut by_op: HashMap<Op, Vec<(i64, Window)>> = HashMap::new();
    for r in rows {
        let Some(op) = Op::parse(&r.op) else { continue };
        let w = r.row.window();
        let points = by_op.entry(op).or_default();
        match points.last_mut() {
            Some((last, sum)) if *last == r.at => sum.merge(&w),
            _ => points.push((r.at, w)),
        }
    }
    let ops = Op::ALL
        .into_iter()
        .filter_map(|op| {
            let points = by_op.remove(&op)?;
            Some(OpsSeries { op: op.as_str(), points: points.into_iter().map(|(at, w)| OpsPoint { at, figures: Figures::of(&w) }).collect() })
        })
        .collect();
    // Always from the hours of the last 30 days, whatever the range shown
    let hours: Vec<(i64, Option<i64>, Option<i64>)> =
        sqlx::query_as("SELECT sampled_at, disk_free, disk_total FROM usage_capacity WHERE location_id = ? AND span = ? AND at >= ? ORDER BY at")
            .bind(&q.location)
            .bind(HOUR)
            .bind(now - 30 * DAY)
            .fetch_all(&st.db)
            .await?;
    let forecast = forecast(&hours);
    Ok(History { location: q.location, range, work, from, to: now, capacity_span, ops_span, capacity, ops, forecast })
}

fn bucket_start(at: i64, span: i64) -> i64 {
    super::sample::bucket(at, span)
}

/// Days until the disk is full, from a straight line through how much of it was used over time (least squares).
/// Unavailable with under three days or 24 samples, when it isn't filling, or when the line fits poorly (R² under
/// 0.5: use jumps up and down).
pub fn forecast(samples: &[(i64, Option<i64>, Option<i64>)]) -> Forecast {
    let known: Vec<(f64, f64, f64)> = samples
        .iter()
        .filter_map(|(t, free, total)| Some((*t as f64 / DAY as f64, (total.as_ref()? - free.as_ref()?) as f64, free.as_ref().copied()? as f64)))
        .collect();
    let points = known.len();
    let window_days = match (known.first(), known.last()) {
        (Some(a), Some(b)) => ((b.0 - a.0) * 10.0).round() / 10.0,
        _ => 0.0,
    };
    let none = |reason| Forecast { days: None, reason, window_days, points };
    if known.is_empty() {
        return none("unknown_capacity");
    }
    if window_days < 3.0 || points < 24 {
        return none("short");
    }
    let n = points as f64;
    let mx = known.iter().map(|p| p.0).sum::<f64>() / n;
    let my = known.iter().map(|p| p.1).sum::<f64>() / n;
    let sxy: f64 = known.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    let sxx: f64 = known.iter().map(|p| (p.0 - mx).powi(2)).sum();
    let syy: f64 = known.iter().map(|p| (p.1 - my).powi(2)).sum();
    let slope = sxy / sxx;
    if slope <= 0.0 || !slope.is_finite() {
        return none("not_growing");
    }
    let r2 = if syy > 0.0 { sxy * sxy / (sxx * syy) } else { 0.0 };
    if r2 < 0.5 {
        return none("unsteady");
    }
    let free = known.last().unwrap().2;
    Forecast { days: Some((free / slope * 10.0).round() / 10.0), reason: "ok", window_days, points }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{testutil, usage::sample};
    use axum::extract::State;

    fn days(n: i64, used: impl Fn(i64) -> i64) -> Vec<(i64, Option<i64>, Option<i64>)> {
        (0..n * 24).map(|h| (h * HOUR, Some(1000 - used(h)), Some(1000))).collect()
    }

    #[test]
    fn time_to_full_needs_a_steady_rise_over_three_days() {
        // 10 bytes a day, 900 free at the end: 90 days
        let f = forecast(&days(10, |h| 100 + h * 10 / 24));
        assert_eq!(f.reason, "ok");
        assert!((f.days.unwrap() - (1000.0 - 100.0 - 239.0 * 10.0 / 24.0) / 10.0).abs() < 1.0, "{f:?}");
        assert_eq!((f.points, f.window_days), (240, 10.0));
        assert_eq!(forecast(&days(2, |h| h)).reason, "short");
        assert_eq!(forecast(&days(10, |_| 500)).reason, "not_growing");
        assert_eq!(forecast(&days(10, |h| 500 - h)).reason, "not_growing");
        assert_eq!(forecast(&days(10, |h| if h % 2 == 0 { 100 } else { 900 } + h / 24)).reason, "unsteady");
        let unknown: Vec<_> = (0..100).map(|h| (h * HOUR, None, None)).collect();
        assert_eq!(forecast(&unknown), Forecast { days: None, reason: "unknown_capacity", window_days: 0.0, points: 0 });
    }

    #[tokio::test]
    async fn the_overview_shows_the_last_sample_and_hour_and_no_latency_when_idle() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let root = admin.root_id.clone().unwrap();
        let t = now();
        // Nothing sampled yet: nothing made up
        let o = build_overview(&env.st, t).await.unwrap();
        assert!(o.total.is_none() && o.locations[0].capacity.is_none());
        assert!(o.locations[0].recent.is_empty(), "no operations: no latency either");
        assert_eq!(o.thresholds, Thresholds::default());
        env.upload(&admin, &root, "a.txt", b"hello").await;
        sample::sample_capacity(&env.st, t).await.unwrap();
        // Written once, still counting since: both are in the last hour
        sample::write_ops(&env.st, &env.st.usage.take(), sample::bucket(t, OPS_SPAN)).await.unwrap();
        env.upload(&admin, &root, "b.txt", b"world!").await;
        let o = build_overview(&env.st, t).await.unwrap();
        let local = &o.locations[0];
        assert_eq!((local.id.as_str(), local.online), ("local", true));
        let c = local.capacity.as_ref().unwrap();
        assert!(!c.stale);
        assert_eq!(c.capacity.store_bytes, 5);
        let write = local.recent.iter().find(|s| s.op == "write" && s.work == "foreground").unwrap();
        assert_eq!((write.figures.count, write.figures.bytes), (2, 11));
        assert!(write.figures.p95_ms.is_some() && write.figures.p50_ms <= write.figures.p95_ms);
        assert!(o.total.is_some());
        assert!(o.alerts.is_empty(), "{:?}", o.alerts);
        // Hours later without a sample: stale, and said so
        let later = build_overview(&env.st, t + 2 * HOUR).await.unwrap();
        assert!(later.locations[0].capacity.as_ref().unwrap().stale);
        assert!(later.alerts.iter().any(|a| a.kind == "stale" && a.location == "local"));
        assert!(later.locations[0].recent.iter().all(|s| s.op != "write" || s.figures.count == 1), "only the last hour");
    }

    #[tokio::test]
    async fn only_administrators_see_it_and_no_settings_are_in_it() {
        use tower::ServiceExt;
        let env = testutil::env().await;
        let amy = env.user("amy", false).await;
        let (_, amy) = env.sign_in(&amy, "test").await;
        let (_, admin) = env.sign_in(&env.admin().await, "test").await;
        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, created_at) VALUES ('b', 'Bucket', 's3', ?, 0)")
            .bind(serde_json::json!({ "bucket": "files", "access_key_id": "AKIDEXAMPLE", "secret_access_key": "sealed" }).to_string())
            .execute(&env.st.db)
            .await
            .unwrap();
        sample::sample_capacity(&env.st, now()).await.unwrap();
        let app = crate::app::routes::router(env.st.clone());
        let get = |uri: &str, cookie: &str| {
            let req = axum::http::Request::builder().uri(uri).header(axum::http::header::COOKIE, cookie).body(axum::body::Body::empty()).unwrap();
            app.clone().oneshot(req)
        };
        for uri in ["/api/admin/usage", "/api/admin/usage/history?range=2d"] {
            assert_eq!(get(uri, &amy).await.unwrap().status(), axum::http::StatusCode::FORBIDDEN, "{uri}");
            let res = get(uri, &admin).await.unwrap();
            assert_eq!(res.status(), axum::http::StatusCode::OK, "{uri}");
            let body = String::from_utf8(axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap();
            assert!(!body.contains("AKIDEXAMPLE") && !body.contains("sealed") && !body.contains("files"), "{body}");
        }
    }

    #[tokio::test]
    async fn alerts_follow_the_thresholds() {
        let env = testutil::env().await;
        sample::sample_capacity(&env.st, now()).await.unwrap();
        let admin = env.admin().await;
        // A disk more full than 0.001% of it, and failures above 1% of at least 20 operations
        let t = Thresholds { disk_percent: 0.001, error_percent: 1.0 };
        let _ = set_thresholds(State(env.st.clone()), Admin(admin.clone()), Json(t)).await.unwrap();
        for i in 0..20 {
            let outcome = if i < 2 { super::super::meter::Outcome::Error } else { super::super::meter::Outcome::Ok };
            env.st.usage.record("local", Op::Read, Work::Foreground, std::time::Duration::from_millis(3), outcome, 0);
        }
        // Health checks that failed don't count as failed operations
        for _ in 0..50 {
            env.st.usage.record("local", Op::Check, Work::Probe, std::time::Duration::from_millis(3), super::super::meter::Outcome::Error, 0);
        }
        let o = build_overview(&env.st, now()).await.unwrap();
        assert_eq!(o.thresholds, t);
        let errors = o.alerts.iter().find(|a| a.kind == "errors").unwrap();
        assert_eq!((errors.location.as_str(), errors.value), ("local", 10.0));
        if cfg!(any(unix, windows)) {
            assert!(o.alerts.iter().any(|a| a.kind == "disk" && a.location == "local"));
            assert!(o.alerts.iter().any(|a| a.kind == "disk" && a.location.is_empty()), "the data folder's disk");
        }
        // Turned off
        let off = Thresholds { disk_percent: 0.0, error_percent: 0.0 };
        let _ = set_thresholds(State(env.st.clone()), Admin(admin.clone()), Json(off)).await.unwrap();
        assert!(build_overview(&env.st, now()).await.unwrap().alerts.is_empty());
        let bad = Thresholds { disk_percent: 101.0, error_percent: 1.0 };
        assert!(set_thresholds(State(env.st.clone()), Admin(admin), Json(bad)).await.is_err());
    }

    #[tokio::test]
    async fn history_reads_the_tier_of_the_range_and_merges_locations() {
        let env = testutil::env().await;
        let t = now();
        let one = |location: &str, op: Op, work: Work, count: u64, ms: u64| {
            let mut all = Windows::new();
            let mut w = Window { count, bytes: count * 100, ..Default::default() };
            for _ in 0..count {
                w.hist.add(ms * 1000);
                w.total_us += ms * 1000;
            }
            w.max_us = ms * 1000;
            all.entry(location.to_string()).or_default().insert((op, work), w);
            all
        };
        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, created_at) VALUES ('nas', 'NAS', 'local', '{}', 0)").execute(&env.st.db).await.unwrap();
        let at = sample::bucket(t, OPS_SPAN) - OPS_SPAN;
        sample::write_ops(&env.st, &one("local", Op::Read, Work::Foreground, 3, 2), at).await.unwrap();
        sample::write_ops(&env.st, &one("nas", Op::Read, Work::Foreground, 1, 40), at).await.unwrap();
        sample::write_ops(&env.st, &one("nas", Op::Write, Work::Background, 5, 9), at).await.unwrap();
        sample::sample_capacity(&env.st, t).await.unwrap();
        let q = |location: &str, range: &str, work: &str| HistoryQuery { location: location.into(), range: Some(range.into()), work: Some(work.into()) };
        // One location, what people did: the read only
        let h = build_history(&env.st, q("nas", "2d", "foreground"), t).await.unwrap();
        assert_eq!((h.capacity_span, h.ops_span), (CAPACITY_SPAN, OPS_SPAN));
        assert_eq!(h.ops.len(), 1);
        assert_eq!((h.ops[0].op, h.ops[0].points[0].figures.count), ("read", 1));
        assert_eq!(h.capacity.len(), 1);
        // All locations: merged into one point per period, the percentiles too
        let h = build_history(&env.st, q("", "6h", "foreground"), t).await.unwrap();
        let read = &h.ops[0].points;
        assert_eq!((read.len(), read[0].figures.count, read[0].figures.bytes), (1, 4, 400));
        assert!(read[0].figures.p50_ms.unwrap() < 3.0 && read[0].figures.max_ms == Some(40.0));
        assert_eq!(h.capacity[0].capacity.location_id, "");
        // Every kind of work, over 30 days: the hours
        let h = build_history(&env.st, q("nas", "30d", "all"), t).await.unwrap();
        assert_eq!((h.capacity_span, h.ops.len()), (HOUR, 2));
        assert_eq!(h.forecast.reason, "short");
        let h = build_history(&env.st, q("nas", "1y", "background"), t).await.unwrap();
        assert_eq!((h.ops_span, h.ops[0].op, h.ops[0].points[0].figures.count), (DAY, "write", 5));
        // Nothing made up for an idle location, and what isn't asked for is refused
        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, created_at) VALUES ('idle', 'Idle', 's3', '{}', 0)").execute(&env.st.db).await.unwrap();
        assert!(build_history(&env.st, q("idle", "2d", "foreground"), t).await.unwrap().ops.is_empty());
        assert!(build_history(&env.st, q("nope", "2d", "foreground"), t).await.is_err());
        assert!(build_history(&env.st, q("", "5y", "foreground"), t).await.is_err());
        assert!(build_history(&env.st, q("", "2d", "everything"), t).await.is_err());
    }
}
