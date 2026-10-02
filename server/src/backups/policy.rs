//! Backup policies: a set (`backup_sets`, kind 'policy') whose spaces are snapshotted again and again (Control panel ›
//! Backups). A scheduler looks at every policy every few seconds:
//! - soon after changes ('realtime'): the triggers of `space_changes` count every change of a space in the transaction
//!   that makes it; a space whose count is higher than what the newest complete snapshot holds (`backup_captured`) has
//!   changes to back up. Changes are gathered for a few seconds, then one snapshot is made; changes during it make one
//!   more after it (`catch_up`), never two at once;
//! - on a schedule: every N minutes, daily or on some days of the week at a time, in a named time zone. A time a
//!   change to summer time skips is skipped; a time that happens twice is run once. After ThirtyFile was stopped, one
//!   snapshot catches up, not one per time missed;
//! - checks that read the backup back, every few days;
//! - how it is doing: administrators are told once when snapshots fail, wait for a location that can't be reached, or
//!   are overdue, and once when it is fine again.
//!
//! After each complete snapshot, snapshots older than the policy keeps are deleted (never the newest `keep_min`, so
//! never the last complete one), then content no remaining snapshot holds. A snapshot that fails deletes nothing.

use std::collections::{HashMap, HashSet};

use jiff::{Timestamp, civil, tz::TimeZone};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::SqliteConnection;

use super::{
    Set,
    layout::{self, Line},
    runner::{ACTIVE, Job, JobState},
};
use crate::{
    error::{AppError, AppResult},
    state::AppState,
    util::{new_id, now},
};

/// How long changes are gathered before a snapshot is made for them
pub const BATCH_SECONDS: i64 = 5;
/// A failed snapshot of a policy is tried again after this long; a location that can't be reached, sooner
const RETRY_FAILED: i64 = 3600;
const RETRY_WAITING: i64 = 300;
/// Content no snapshot holds is kept this long after it was stored (a snapshot being made may be about to use it)
const GC_GRACE: i64 = 3600;

/// When snapshots are made on a schedule
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Schedule {
    /// Every so many minutes
    Every(i64),
    /// Every day at a time (hour, minute)
    Daily(i8, i8),
    /// On some days of the week (Monday is 1) at a time
    Weekly(i8, i8, Vec<i8>),
}

impl Schedule {
    pub fn parse(v: &Value) -> AppResult<Schedule> {
        let bad = || AppError::bad_request("The schedule isn't valid");
        let time = |s: &str| -> AppResult<(i8, i8)> {
            let (h, m) = s.split_once(':').ok_or_else(bad)?;
            let (h, m): (i8, i8) = (h.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?);
            if !(0..24).contains(&h) || !(0..60).contains(&m) {
                return Err(bad());
            }
            Ok((h, m))
        };
        if let Some(n) = v.get("every").and_then(Value::as_i64) {
            if !(5..=7 * 24 * 60).contains(&n) {
                return Err(AppError::bad_request("Snapshots can be made every 5 minutes to every 7 days"));
            }
            return Ok(Schedule::Every(n));
        }
        if let Some(t) = v.get("daily").and_then(Value::as_str) {
            let (h, m) = time(t)?;
            return Ok(Schedule::Daily(h, m));
        }
        if let Some(t) = v.get("weekly").and_then(Value::as_str) {
            let (h, m) = time(t)?;
            let mut days: Vec<i8> = v["days"].as_array().ok_or_else(bad)?.iter().filter_map(Value::as_i64).map(|d| d as i8).collect();
            days.sort_unstable();
            days.dedup();
            if days.is_empty() || days.iter().any(|d| !(1..=7).contains(d)) {
                return Err(AppError::bad_request("Choose the days of the week"));
            }
            return Ok(Schedule::Weekly(h, m, days));
        }
        Err(bad())
    }

    pub fn to_json(&self) -> Value {
        match self {
            Schedule::Every(n) => json!({ "every": n }),
            Schedule::Daily(h, m) => json!({ "daily": format!("{h:02}:{m:02}") }),
            Schedule::Weekly(h, m, days) => json!({ "weekly": format!("{h:02}:{m:02}"), "days": days }),
        }
    }
}

/// The time zone of a name, refused when the name isn't known
pub fn time_zone(name: &str) -> AppResult<TimeZone> {
    TimeZone::get(name).map_err(|_| AppError::bad_request(format!("Unknown time zone: {name}")))
}

/// The first time on the schedule after `after` (Unix seconds). A local time that doesn't exist that day (clocks going
/// forward) is skipped; one that happens twice (clocks going back) counts once, the first time.
pub fn next_after(s: &Schedule, tz: &TimeZone, after: i64) -> Option<i64> {
    let (h, m, days) = match s {
        Schedule::Every(n) => return Some(after + n * 60),
        Schedule::Daily(h, m) => (*h, *m, None),
        Schedule::Weekly(h, m, days) => (*h, *m, Some(days)),
    };
    let start = Timestamp::from_second(after).ok()?.to_zoned(tz.clone()).date();
    let mut date = start.yesterday().ok()?;
    for _ in 0..16 {
        if days.is_none_or(|d| d.contains(&date.weekday().to_monday_one_offset())) {
            let at: civil::DateTime = date.at(h, m, 0, 0);
            let ambiguous = tz.to_ambiguous_zoned(at);
            if !matches!(ambiguous.offset(), jiff::tz::AmbiguousOffset::Gap { .. })
                && let Ok(z) = ambiguous.earlier()
                && z.timestamp().as_second() > after
            {
                return Some(z.timestamp().as_second());
            }
        }
        date = date.tomorrow().ok()?;
    }
    None
}

/// A policy as the database has it
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Policy {
    #[serde(skip)]
    pub set_id: String,
    pub enabled: bool,
    pub mode: String,
    #[serde(skip)]
    pub schedule: String,
    pub tz: String,
    pub all_spaces: bool,
    pub versions: bool,
    pub trash: bool,
    pub keep_days: i64,
    pub keep_min: i64,
    pub rate_limit: i64,
    pub alert_hours: i64,
    pub verify_days: i64,
    pub next_run_at: Option<i64>,
    pub last_run_at: Option<i64>,
    pub last_verify_at: Option<i64>,
    #[serde(skip)]
    pub catch_up: bool,
    #[serde(skip)]
    pub alerted: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Policy {
    pub fn schedule(&self) -> Option<Schedule> {
        (self.mode != "realtime").then(|| serde_json::from_str::<Value>(&self.schedule).ok().and_then(|v| Schedule::parse(&v).ok())).flatten()
    }
    fn realtime(&self) -> bool {
        self.mode != "scheduled"
    }
}

const POLICY_COLS: &str = "set_id, enabled, mode, schedule, tz, all_spaces, versions, trash, keep_days, keep_min, rate_limit, alert_hours, verify_days,
     next_run_at, last_run_at, last_verify_at, catch_up, alerted, created_at, updated_at";

pub async fn load(conn: &mut SqliteConnection, set: &str) -> AppResult<Option<Policy>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {POLICY_COLS} FROM backup_policies WHERE set_id = ?"))).bind(set).fetch_optional(conn).await?)
}

/// The spaces a policy backs up now: every space on its location, or the ones chosen that are still there
pub async fn scope(conn: &mut SqliteConnection, set: &str) -> AppResult<Vec<String>> {
    Ok(sqlx::query_as::<_, (String,)>(
        "SELECT d.id FROM drives d JOIN backup_sets s ON s.id = ?1 JOIN backup_policies p ON p.set_id = s.id
         WHERE (p.all_spaces = 1 AND d.location_id = s.source_location)
            OR (p.all_spaces = 0 AND d.id IN (SELECT drive_id FROM backup_policy_spaces WHERE set_id = ?1))
         ORDER BY d.id",
    )
    .bind(set)
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|(id,)| id)
    .collect())
}

/// Spaces with changes the newest complete snapshot doesn't hold (spaces never snapshotted included)
pub async fn changed(conn: &mut SqliteConnection, set: &str, spaces: &[String]) -> AppResult<Vec<String>> {
    Ok(sqlx::query_as::<_, (String,)>(
        "SELECT d.value FROM json_each(?2) d
         LEFT JOIN space_changes c ON c.drive_id = d.value LEFT JOIN backup_captured k ON k.set_id = ?1 AND k.drive_id = d.value
         WHERE k.seq IS NULL OR COALESCE(c.seq, 0) > k.seq",
    )
    .bind(set)
    .bind(serde_json::to_string(spaces).unwrap())
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|(d,)| d)
    .collect())
}

/// Queues a snapshot of a policy for `trigger` ('schedule', 'change', 'manual'). One at a time: when one isn't over,
/// a queued one is left as it is, a failed or waiting one is tried again (not more often than `RETRY_*`), and a
/// running or paused one gets one more after it. Returns the job queued, if any.
pub async fn trigger(st: &AppState, set: &str, trigger: &str, by: Option<(i64, String)>) -> AppResult<Option<String>> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        let Some(p) = load(&mut tx, set).await? else { return Ok(None) };
        let (name, removing): (String, bool) = sqlx::query_as("SELECT name, removing FROM backup_sets WHERE id = ?").bind(set).fetch_one(&mut *tx).await?;
        if removing {
            return Ok(None);
        }
        let active: Option<(String, JobState)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT id, state FROM backup_jobs WHERE set_id = ? AND kind = 'snapshot' AND state IN {ACTIVE} ORDER BY created_at DESC LIMIT 1"
        )))
        .bind(set)
        .fetch_optional(&mut *tx)
        .await?;
        let t = now();
        match active {
            Some((_, JobState::Queued)) => Ok(None),
            Some((id, state @ (JobState::Failed | JobState::Waiting))) => {
                let wait = if state == JobState::Failed { RETRY_FAILED } else { RETRY_WAITING };
                if trigger == "manual" || p.last_run_at.is_none_or(|l| t - l >= wait) {
                    sqlx::query("UPDATE backup_jobs SET state = 'queued', error = NULL WHERE id = ? AND state IN ('failed', 'waiting')")
                        .bind(&id)
                        .execute(&mut *tx)
                        .await?;
                    sqlx::query("UPDATE backup_policies SET last_run_at = ? WHERE set_id = ?").bind(t).bind(set).execute(&mut *tx).await?;
                    return Ok(Some(id));
                }
                Ok(None)
            }
            Some(_) => {
                sqlx::query("UPDATE backup_policies SET catch_up = 1 WHERE set_id = ?").bind(set).execute(&mut *tx).await?;
                Ok(None)
            }
            None => {
                let spaces = scope(&mut tx, set).await?;
                let id = new_id();
                let params = json!({ "spaces": spaces, "versions": p.versions, "trash": p.trash, "trigger": trigger, "rate_limit": p.rate_limit });
                let (by_id, by_name) = by.map_or((None, String::new()), |(i, n)| (Some(i), n));
                sqlx::query(
                    "INSERT INTO backup_jobs (id, kind, set_id, params, label, created_by, created_by_name, created_at) VALUES (?, 'snapshot', ?, ?, ?, ?, ?, ?)",
                )
                .bind(&id)
                .bind(set)
                .bind(params.to_string())
                .bind(&name)
                .bind(by_id)
                .bind(&by_name)
                .bind(t)
                .execute(&mut *tx)
                .await?;
                sqlx::query("UPDATE backup_policies SET last_run_at = ?, catch_up = 0 WHERE set_id = ?").bind(t).bind(set).execute(&mut *tx).await?;
                Ok(Some(id))
            }
        }
    }
    .await;
    let queued = crate::db::settle(tx, res).await?;
    if queued.is_some() {
        st.backups.wake.notify_one();
    }
    Ok(queued)
}

// ───────────── Scheduler ─────────────

/// Looks at every policy every few seconds (and when woken)
pub fn spawn_scheduler(st: AppState) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = tick(&st, now()).await {
                tracing::warn!("Backup policies: {}", e.message);
            }
            tokio::select! {
                _ = st.backups.policies.notified() => {}
                _ = tokio::time::sleep(std::time::Duration::from_secs(BATCH_SECONDS as u64)) => {}
            }
        }
    });
}

/// One look at every policy, as of `t`
pub async fn tick(st: &AppState, t: i64) -> AppResult<()> {
    let policies: Vec<Policy> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {POLICY_COLS} FROM backup_policies p WHERE EXISTS (SELECT 1 FROM backup_sets s WHERE s.id = p.set_id AND s.removing = 0)"
    )))
    .fetch_all(&st.db)
    .await?;
    for p in policies {
        if let Err(e) = look_at(st, &p, t).await {
            tracing::warn!("Backup policy {}: {}", p.set_id, e.message);
        }
    }
    Ok(())
}

async fn look_at(st: &AppState, p: &Policy, t: i64) -> AppResult<()> {
    let set = &p.set_id;
    if p.enabled {
        let mut due = None;
        // Changes: noted when first seen, backed up once they had a few seconds to settle
        if p.realtime() {
            let mut c = st.db.acquire().await?;
            let spaces = scope(&mut c, set).await?;
            let changed = changed(&mut c, set, &spaces).await?;
            drop(c);
            if !changed.is_empty() {
                let _w = st.write_lock.lock().await;
                sqlx::query("INSERT OR IGNORE INTO backup_dirty (set_id, drive_id, since) SELECT ?1, value, ?2 FROM json_each(?3)")
                    .bind(set)
                    .bind(t)
                    .bind(serde_json::to_string(&changed).unwrap())
                    .execute(&st.db)
                    .await?;
            }
            let oldest = behind_since(&mut *st.db.acquire().await?, set, &spaces).await?;
            if !changed.is_empty() && oldest.is_some_and(|o| t - o >= BATCH_SECONDS) {
                due = Some("change");
            }
        }
        if let Some(s) = p.schedule() {
            let tz = time_zone(&p.tz)?;
            match p.next_run_at {
                Some(next) if next <= t => {
                    due = Some("schedule");
                    // One catch-up after downtime, then the next time from now
                    set_next(st, set, next_after(&s, &tz, t)).await?;
                }
                None => set_next(st, set, next_after(&s, &tz, t)).await?,
                _ => {}
            }
        }
        // One more after a snapshot that had changes (or the schedule) come during it
        if due.is_none() && p.catch_up {
            due = Some("change");
        }
        if let Some(trigger) = due {
            self::trigger(st, set, trigger, None).await?;
        } else {
            // A snapshot that failed, or waits for a location, is tried again now and then
            let stuck: Option<(String,)> =
                sqlx::query_as("SELECT state FROM backup_jobs WHERE set_id = ? AND kind = 'snapshot' AND state IN ('failed', 'waiting') LIMIT 1")
                    .bind(set)
                    .fetch_optional(&st.db)
                    .await?;
            if stuck.is_some() {
                self::trigger(st, set, "retry", None).await?;
            }
        }
        // Reading the backup back every few days
        if p.verify_days > 0 && p.last_verify_at.is_none_or(|v| t - v >= p.verify_days * 86400) {
            queue_verify(st, set, t).await?;
        }
    }
    alert(st, p, t).await
}

async fn set_next(st: &AppState, set: &str, next: Option<i64>) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    sqlx::query("UPDATE backup_policies SET next_run_at = ? WHERE set_id = ?").bind(next).bind(set).execute(&st.db).await?;
    Ok(())
}

/// A check of the backup, when it has a complete snapshot and nothing else of it is under way
async fn queue_verify(st: &AppState, set: &str, t: i64) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        let (complete,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM backup_snapshots WHERE set_id = ? AND state = 'complete'").bind(set).fetch_one(&mut *tx).await?;
        let busy: Option<(i64,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT 1 FROM backup_jobs WHERE set_id = ? AND state IN {ACTIVE} LIMIT 1")))
            .bind(set)
            .fetch_optional(&mut *tx)
            .await?;
        if complete == 0 || busy.is_some() {
            return Ok(false);
        }
        let (name,): (String,) = sqlx::query_as("SELECT name FROM backup_sets WHERE id = ?").bind(set).fetch_one(&mut *tx).await?;
        sqlx::query("INSERT INTO backup_jobs (id, kind, set_id, params, label, created_at) VALUES (?, 'verify', ?, '{\"trigger\":\"schedule\"}', ?, ?)")
            .bind(new_id())
            .bind(set)
            .bind(name)
            .bind(t)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE backup_policies SET last_verify_at = ? WHERE set_id = ?").bind(t).bind(set).execute(&mut *tx).await?;
        AppResult::Ok(true)
    }
    .await;
    if crate::db::settle(tx, res).await? {
        st.backups.wake.notify_one();
    }
    Ok(())
}

// ───────────── How a policy is doing ─────────────

/// How a policy is doing, as Control panel › Backups shows it
#[derive(Debug, Clone, Serialize)]
pub struct Health {
    /// protected, catching_up (changes wait for a snapshot), running, waiting (a location can't be reached), failing,
    /// overdue (no complete snapshot for longer than the policy allows), paused, never (no complete snapshot yet)
    pub state: &'static str,
    /// When the newest complete snapshot read the spaces: they are protected as they were then
    pub protected_through: Option<i64>,
    /// Since when changes wait for a snapshot (the oldest change not held yet, as seen)
    pub behind_since: Option<i64>,
    /// Spaces with changes no complete snapshot holds
    pub changed_spaces: i64,
    /// The error of the snapshot that failed or waits
    pub error: Option<String>,
}

pub async fn health(conn: &mut SqliteConnection, p: &Policy, t: i64) -> AppResult<Health> {
    let (protected_through,): (Option<i64>,) =
        sqlx::query_as("SELECT MAX(cutoff) FROM backup_snapshots WHERE set_id = ? AND state = 'complete'").bind(&p.set_id).fetch_one(&mut *conn).await?;
    let spaces = scope(conn, &p.set_id).await?;
    let behind_since = behind_since(conn, &p.set_id, &spaces).await?;
    let changed_spaces = changed(conn, &p.set_id, &spaces).await?.len() as i64;
    let job: Option<(String, Option<String>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT state, error FROM backup_jobs WHERE set_id = ? AND kind = 'snapshot' AND state IN {ACTIVE} ORDER BY created_at DESC LIMIT 1"
    )))
    .bind(&p.set_id)
    .fetch_optional(&mut *conn)
    .await?;
    let overdue = p.alert_hours > 0 && t - protected_through.unwrap_or(p.created_at) > p.alert_hours * 3600;
    let state = match (&job, p.enabled) {
        (_, false) => "paused",
        (Some((s, _)), _) if s == "failed" => "failing",
        (Some((s, _)), _) if s == "waiting" => "waiting",
        _ if overdue => "overdue",
        (Some(_), _) => "running",
        _ if protected_through.is_none() => "never",
        _ if changed_spaces > 0 && p.realtime() => "catching_up",
        _ => "protected",
    };
    Ok(Health { state, protected_through, behind_since, changed_spaces, error: job.and_then(|(_, e)| e) })
}

/// Since when changes of the policy's spaces (`spaces`, its scope now) wait for a snapshot. A space deleted or taken
/// out of the policy since doesn't count: it is never backed up again.
async fn behind_since(conn: &mut SqliteConnection, set: &str, spaces: &[String]) -> AppResult<Option<i64>> {
    let (since,): (Option<i64>,) = sqlx::query_as("SELECT MIN(since) FROM backup_dirty WHERE set_id = ? AND drive_id IN (SELECT value FROM json_each(?))")
        .bind(set)
        .bind(serde_json::to_string(spaces).unwrap())
        .fetch_one(conn)
        .await?;
    Ok(since)
}

/// Tells administrators once when a policy starts failing, waiting or being overdue, and once when it is fine again
async fn alert(st: &AppState, p: &Policy, t: i64) -> AppResult<()> {
    let h = health(&mut *st.db.acquire().await?, p, t).await?;
    let trouble = matches!(h.state, "failing" | "waiting" | "overdue");
    let now_state = if trouble { h.state } else { "" };
    if now_state == p.alerted {
        return Ok(());
    }
    // Back to fine: told only when trouble was told
    let kind = if trouble { h.state } else { "recovered" };
    let (name,): (String,) = sqlx::query_as("SELECT name FROM backup_sets WHERE id = ?").bind(&p.set_id).fetch_one(&st.db).await?;
    let emails = {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            sqlx::query("UPDATE backup_policies SET alerted = ? WHERE set_id = ?").bind(now_state).bind(&p.set_id).execute(&mut *tx).await?;
            if !trouble && p.alerted.is_empty() {
                return Ok(Vec::new());
            }
            let admins: Vec<i64> = sqlx::query_as::<_, (i64,)>("SELECT id FROM users WHERE role = 'admin' AND disabled = 0")
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .map(|(i,)| i)
                .collect();
            let notice = crate::notify::Notice {
                kind: "backup",
                node_id: None,
                data: json!({ "name": name, "state": kind, "error": h.error, "since": h.protected_through }),
            };
            let detail = format!("{name}: {kind}");
            sqlx::query("INSERT INTO activity (at, action, detail) VALUES (?, 'backup_alert', ?)").bind(t).bind(&detail).execute(&mut *tx).await?;
            crate::notify::add(&mut tx, &admins, &notice).await
        }
        .await;
        crate::db::settle(tx, res).await?
    };
    crate::notify::send_later(st, emails);
    Ok(())
}

// ───────────── After a snapshot ─────────────

/// After a complete snapshot of a policy: snapshots it no longer keeps are deleted, then content none of the others
/// holds. What can't be deleted now (a location that can't be reached) is tried again after the next snapshot.
pub async fn prune(st: &AppState, set: &Set) -> AppResult<()> {
    let Some(p) = load(&mut *st.db.acquire().await?, &set.id).await? else { return Ok(()) };
    let dst = st.storage(&set.dest_location)?;
    let snapshots: Vec<(String, i64)> =
        sqlx::query_as("SELECT id, completed_at FROM backup_snapshots WHERE set_id = ? AND state = 'complete' ORDER BY completed_at DESC, rowid DESC")
            .bind(&set.id)
            .fetch_all(&st.db)
            .await?;
    let t = now();
    let keep_min = p.keep_min.max(1) as usize;
    for (i, (id, completed)) in snapshots.iter().enumerate() {
        if i < keep_min || *completed >= t - p.keep_days * 86400 {
            continue;
        }
        // The completion marker first: a manifest without it is never taken for a restore point
        for key in [layout::complete_key(&set.id, id), layout::manifest_key(&set.id, id)] {
            dst.delete_at(&key).await.map_err(|e| AppError::new(axum::http::StatusCode::BAD_GATEWAY, crate::locations::describe(&e)))?;
        }
        {
            let _w = st.write_lock.lock().await;
            sqlx::query("DELETE FROM backup_snapshots WHERE id = ?").bind(id).execute(&st.db).await?;
        }
        let _ = tokio::fs::remove_file(layout::cached_manifest(st, id)).await;
        tracing::info!("Backup {}: a snapshot older than {} days was deleted", set.name, p.keep_days);
    }
    collect(st, set).await
}

/// Deletes content of a set that no complete snapshot holds, and that no snapshot being made may be about to use
pub async fn collect(st: &AppState, set: &Set) -> AppResult<()> {
    let dst = st.storage(&set.dest_location)?;
    let snapshots: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT id, manifest_sha256, manifest_size FROM backup_snapshots WHERE set_id = ? AND state = 'complete'")
            .bind(&set.id)
            .fetch_all(&st.db)
            .await?;
    let mut live: HashSet<[u8; 32]> = HashSet::new();
    let key = |h: &str| -> Option<[u8; 32]> { hex::decode(h).ok()?.try_into().ok() };
    for (id, sha, size) in &snapshots {
        let path = layout::manifest(st, dst.as_ref(), &set.id, id, sha, *size as u64).await?;
        let hashes = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<String>> {
            let mut out = Vec::new();
            for line in layout::lines(&path)? {
                match line? {
                    Line::File { hash, .. } | Line::Version { hash, .. } => out.push(hash),
                    _ => {}
                }
            }
            Ok(out)
        })
        .await
        .map_err(|e| AppError::internal(e.to_string()))??;
        live.extend(hashes.iter().filter_map(|h| key(h)));
    }
    // Pinned by a snapshot not over yet
    let pinned: Vec<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT p.hash FROM backup_pending p JOIN backup_jobs j ON j.id = p.job_id WHERE j.set_id = ? AND j.state IN {ACTIVE}"
    )))
    .bind(&set.id)
    .fetch_all(&st.db)
    .await?;
    live.extend(pinned.iter().filter_map(|(h,)| key(h)));
    let mut last = String::new();
    let mut deleted = 0u64;
    loop {
        let rows: Vec<(String, i64)> = sqlx::query_as("SELECT hash, created_at FROM backup_objects WHERE set_id = ? AND hash > ? ORDER BY hash LIMIT 500")
            .bind(&set.id)
            .bind(&last)
            .fetch_all(&st.db)
            .await?;
        let Some((h, _)) = rows.last() else { break };
        last = h.clone();
        let unused: Vec<String> =
            rows.into_iter().filter(|(h, created)| *created < now() - GC_GRACE && key(h).is_some_and(|k| !live.contains(&k))).map(|(h, _)| h).collect();
        for h in unused {
            if dst.delete_at(&layout::object_key(&set.id, &h)).await.is_err() {
                // Tried again after the next snapshot
                continue;
            }
            let _w = st.write_lock.lock().await;
            sqlx::query("DELETE FROM backup_objects WHERE set_id = ? AND hash = ?").bind(&set.id).bind(&h).execute(&st.db).await?;
            sqlx::query("DELETE FROM backup_folder_files WHERE set_id = ? AND hash = ?").bind(&set.id).bind(&h).execute(&st.db).await?;
            deleted += 1;
        }
    }
    if deleted > 0 {
        tracing::info!("Backup {}: {deleted} contents no snapshot holds any more were deleted", set.name);
    }
    Ok(())
}

/// In the transaction that completes a snapshot of a policy: what it captured is recorded, changes it holds stop
/// counting as waiting, and a snapshot asked for meanwhile follows. `changes` has every space the snapshot took: what
/// is recorded of other spaces (deleted, or taken out of the policy, since) goes.
pub async fn completed(conn: &mut SqliteConnection, set: &str, changes: &HashMap<String, i64>) -> AppResult<()> {
    let taken = serde_json::to_string(changes).unwrap();
    for table in ["backup_dirty", "backup_captured", "backup_folder_files"] {
        sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table} WHERE set_id = ?1 AND drive_id NOT IN (SELECT key FROM json_each(?2))")))
            .bind(set)
            .bind(&taken)
            .execute(&mut *conn)
            .await?;
    }
    sqlx::query(
        "INSERT INTO backup_captured (set_id, drive_id, seq) SELECT ?1, key, value FROM json_each(?2) WHERE true
         ON CONFLICT (set_id, drive_id) DO UPDATE SET seq = excluded.seq",
    )
    .bind(set)
    .bind(serde_json::to_string(changes).unwrap())
    .execute(&mut *conn)
    .await?;
    // Changes made after the snapshot read the spaces still wait
    sqlx::query(
        "DELETE FROM backup_dirty WHERE set_id = ?1 AND drive_id IN (
           SELECT k.drive_id FROM backup_captured k LEFT JOIN space_changes c ON c.drive_id = k.drive_id
           WHERE k.set_id = ?1 AND COALESCE(c.seq, 0) <= k.seq)",
    )
    .bind(set)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Whether a set is a policy's (its snapshots take the policy's spaces as they are when they start)
pub async fn is_policy(conn: &mut SqliteConnection, set: &str) -> AppResult<bool> {
    Ok(load(conn, set).await?.is_some())
}

/// Whether a job's set lives on a location that can't be reached now
pub async fn waits(st: &AppState, job: &Job) -> bool {
    let Ok(set) = super::load_set(&st.db, &job.set_id).await else { return false };
    crate::locations::probe(st, &set.dest_location).await.is_err()
}
