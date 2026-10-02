//! The scheduler of replica policies: every few seconds, for each target of each policy, a sync soon after the spaces
//! change (or on the target's schedule), a sync of an old primary until it counts again, a check every few days; and
//! how each is doing, told to administrators once when it starts failing and once when it works again.

use serde::Serialize;
use serde_json::json;

use super::{Policy, Target};
use crate::{
    backups::{
        policy::{Schedule, next_after, time_zone},
        runner::{ACTIVE, JobState},
    },
    error::AppResult,
    replicas::Memory,
    state::AppState,
    util::{new_id, now},
};

/// How long changes are gathered before a sync is made for them
pub const BATCH_SECONDS: i64 = 5;
/// A sync that failed is tried again after this long; one waiting for its location, sooner
const RETRY_FAILED: i64 = 3600;
const RETRY_WAITING: i64 = 300;

/// Looks at every policy every few seconds (and when woken)
pub fn spawn_scheduler(st: AppState) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = tick(&st, now()).await {
                tracing::warn!("Replica policies: {}", e.message);
            }
            tokio::select! {
                _ = st.part::<Memory>().queue.policies.notified() => {}
                _ = tokio::time::sleep(std::time::Duration::from_secs(BATCH_SECONDS as u64)) => {}
            }
        }
    });
}

/// One look at every policy, as of `t`
pub async fn tick(st: &AppState, t: i64) -> AppResult<()> {
    let policies: Vec<Policy> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {} FROM replica_policies", super::POLICY_COLS))).fetch_all(&st.db).await?;
    for p in &policies {
        if let Err(e) = look_at(st, p, t).await {
            tracing::warn!("Replica policy {}: {}", p.name, e.message);
        }
    }
    recount(st, &policies).await
}

/// Works out what the Replicas page shows where it isn't known (a new target or policy, a policy changed or deleted, a
/// promotion, a space that left a policy, an upgrade): how many contents each target holds of those it should, and the
/// copies on each location no policy wants. Each reads every content of the spaces, so it is kept, and worked out again
/// only when it may have changed (also by the syncs).
async fn recount(st: &AppState, policies: &[Policy]) -> AppResult<()> {
    for p in policies {
        let targets = super::targets(&mut *st.db.acquire().await?, &p.id).await?;
        if targets.iter().all(|t| t.held.is_some()) {
            continue;
        }
        let stamp = super::stamp(&mut *st.db.acquire().await?).await?;
        let counts = super::sync::count(st, p, &targets).await?;
        keep(st, &stamp, async |tx| super::sync::keep_counts(tx, &p.id, &counts).await).await?;
    }
    let locations: Vec<(String,)> = sqlx::query_as(
        "SELECT l.id FROM storage_locations l
         WHERE EXISTS (SELECT 1 FROM replica_copies c WHERE c.location_id = l.id) AND NOT EXISTS (SELECT 1 FROM replica_unneeded u WHERE u.location_id = l.id)",
    )
    .fetch_all(&st.db)
    .await?;
    for (l,) in locations {
        let stamp = super::stamp(&mut *st.db.acquire().await?).await?;
        let (copies, bytes) = super::api::unneeded_count(st, &l).await?;
        keep(st, &stamp, async |tx| super::sync::keep_unneeded(tx, &l, copies, bytes).await).await?;
    }
    Ok(())
}

/// Keeps what was worked out, unless the policies changed since `stamp` (it is worked out again then)
async fn keep(st: &AppState, stamp: &str, write: impl AsyncFnOnce(&mut sqlx::SqliteConnection) -> AppResult<()>) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        if super::stamp(&mut tx).await? == stamp {
            write(&mut tx).await?;
        }
        AppResult::Ok(())
    }
    .await;
    crate::db::settle(tx, res).await
}

/// Spaces of the policy with changes a target's last sync doesn't hold (spaces never synced included)
pub(super) async fn changed(st: &AppState, p: &Policy, location: &str, spaces: &[String]) -> AppResult<Vec<String>> {
    Ok(sqlx::query_as::<_, (String,)>(
        "SELECT d.value FROM json_each(?3) d
         LEFT JOIN space_changes c ON c.drive_id = d.value
         LEFT JOIN replica_captured k ON k.policy_id = ?1 AND k.location_id = ?2 AND k.drive_id = d.value
         WHERE k.seq IS NULL OR COALESCE(c.seq, 0) > k.seq",
    )
    .bind(&p.id)
    .bind(location)
    .bind(serde_json::to_string(spaces).unwrap())
    .fetch_all(&st.db)
    .await?
    .into_iter()
    .map(|(d,)| d)
    .collect())
}

async fn look_at(st: &AppState, p: &Policy, t: i64) -> AppResult<()> {
    let targets = super::targets(&mut *st.db.acquire().await?, &p.id).await?;
    if p.enabled {
        let spaces = super::scope(&mut *st.db.acquire().await?, &p.id).await?;
        left_scope(st, p, &spaces).await?;
        for target in &targets {
            let l = &target.location_id;
            let changed = changed(st, p, l, &spaces).await?;
            if !changed.is_empty() {
                let _w = st.write_lock.lock().await;
                sqlx::query("INSERT OR IGNORE INTO replica_dirty (policy_id, location_id, drive_id, since) SELECT ?1, ?2, value, ?3 FROM json_each(?4)")
                    .bind(&p.id)
                    .bind(l)
                    .bind(t)
                    .bind(serde_json::to_string(&changed).unwrap())
                    .execute(&st.db)
                    .await?;
            }
            let mut due = None;
            if target.state == "stale" {
                // An old primary: checked, then brought up to date, before it counts again
                due = Some("reconcile");
            } else if target.mode == "realtime" {
                let oldest = behind_since(st, &p.id, l, &spaces).await?;
                if (!changed.is_empty() && oldest.is_some_and(|o| t - o >= BATCH_SECONDS)) || target.synced_at.is_none() {
                    due = Some("change");
                }
            } else if let Some(s) = serde_json::from_str(&target.schedule).ok().and_then(|v| Schedule::parse(&v).ok()) {
                let tz = time_zone(&target.tz)?;
                match target.next_run_at {
                    Some(next) if next <= t => {
                        due = Some("schedule");
                        set_next(st, &p.id, l, next_after(&s, &tz, t)).await?;
                    }
                    None => {
                        set_next(st, &p.id, l, next_after(&s, &tz, t)).await?;
                        if target.synced_at.is_none() {
                            due = Some("schedule");
                        }
                    }
                    _ => {}
                }
            }
            if due.is_none() && target.catch_up {
                due = Some("change");
            }
            match due {
                Some(why) => {
                    trigger(st, &p.id, l, why, None).await?;
                }
                None => {
                    // A sync that failed, or waits for its location, is tried again now and then. Looked for first
                    // without the write lock: an idle target takes none.
                    let stuck: Option<(i64,)> = sqlx::query_as(
                        "SELECT 1 FROM replica_jobs WHERE policy_id = ? AND location_id = ? AND kind = 'sync' AND state IN ('failed', 'waiting')
                         ORDER BY created_at DESC LIMIT 1",
                    )
                    .bind(&p.id)
                    .bind(l)
                    .fetch_optional(&st.db)
                    .await?;
                    if stuck.is_some() {
                        trigger(st, &p.id, l, "retry", None).await?;
                    }
                }
            }
            if p.verify_days > 0 && target.state == "active" && target.synced_at.is_some() && target.last_verify_at.is_none_or(|v| t - v >= p.verify_days * 86400) {
                queue_verify(st, p, l, t).await?;
            }
        }
    }
    alert(st, p, &targets, t).await
}

/// Spaces deleted, or taken out of the policy, since a target's sync: never synced again, so what was recorded of them
/// goes (as the next sync would do), and what the page shows is worked out again. Looked for first without the write
/// lock: usually there are none.
async fn left_scope(st: &AppState, p: &Policy, spaces: &[String]) -> AppResult<()> {
    let list = serde_json::to_string(spaces).unwrap();
    let gone: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM replica_captured WHERE policy_id = ? AND drive_id NOT IN (SELECT value FROM json_each(?)) LIMIT 1")
        .bind(&p.id)
        .bind(&list)
        .fetch_optional(&st.db)
        .await?;
    if gone.is_none() {
        return Ok(());
    }
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        for table in ["replica_dirty", "replica_captured"] {
            sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table} WHERE policy_id = ?1 AND drive_id NOT IN (SELECT value FROM json_each(?2))")))
                .bind(&p.id)
                .bind(&list)
                .execute(&mut *tx)
                .await?;
        }
        super::forget_counts(&mut tx, Some(&p.id)).await?;
        AppResult::Ok(())
    }
    .await;
    crate::db::settle(tx, res).await
}

async fn set_next(st: &AppState, policy: &str, location: &str, next: Option<i64>) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    sqlx::query("UPDATE replica_targets SET next_run_at = ? WHERE policy_id = ? AND location_id = ?").bind(next).bind(policy).bind(location).execute(&st.db).await?;
    Ok(())
}

/// Queues a sync of a target for `why` ('change', 'schedule', 'reconcile', 'repair', 'manual', or 'retry' for one
/// that failed or waits). One at a time: a queued one is left as it is, a failed or waiting one is tried again (not
/// more often than `RETRY_*`, unless asked for), a running or paused one gets one more after it.
pub async fn trigger(st: &AppState, policy: &str, location: &str, why: &str, by: Option<(i64, String)>) -> AppResult<Option<String>> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        let Some(p) = super::load(&mut tx, policy).await? else { return Ok(None) };
        let active: Option<(String, JobState)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT id, state FROM replica_jobs WHERE policy_id = ? AND location_id = ? AND kind = 'sync' AND state IN {ACTIVE} ORDER BY created_at DESC LIMIT 1"
        )))
        .bind(policy)
        .bind(location)
        .fetch_optional(&mut *tx)
        .await?;
        let (last_run,): (Option<i64>,) =
            sqlx::query_as("SELECT last_run_at FROM replica_targets WHERE policy_id = ? AND location_id = ?").bind(policy).bind(location).fetch_one(&mut *tx).await?;
        let t = now();
        match active {
            Some((_, JobState::Queued)) => Ok(None),
            Some((id, state @ (JobState::Failed | JobState::Waiting))) => {
                let wait = if state == JobState::Failed { RETRY_FAILED } else { RETRY_WAITING };
                // A location that works again, as checked since the job last ran, is tried again at once
                let back = state == JobState::Waiting
                    && st.location_health.lock().unwrap().get(location).is_some_and(|h| h.ok && last_run.is_some_and(|l| h.checked_at > l));
                if why == "manual" || back || last_run.is_none_or(|l| t - l >= wait) {
                    sqlx::query("UPDATE replica_jobs SET state = 'queued', error = NULL, params = json_set(params, '$.epoch', ?) WHERE id = ?")
                        .bind(p.epoch)
                        .bind(&id)
                        .execute(&mut *tx)
                        .await?;
                    sqlx::query("UPDATE replica_targets SET last_run_at = ? WHERE policy_id = ? AND location_id = ?").bind(t).bind(policy).bind(location).execute(&mut *tx).await?;
                    return Ok(Some(id));
                }
                Ok(None)
            }
            Some(_) => {
                sqlx::query("UPDATE replica_targets SET catch_up = 1 WHERE policy_id = ? AND location_id = ?").bind(policy).bind(location).execute(&mut *tx).await?;
                Ok(None)
            }
            None if why == "retry" => Ok(None),
            None => {
                let (name,): (String,) = sqlx::query_as("SELECT name FROM storage_locations WHERE id = ?").bind(location).fetch_optional(&mut *tx).await?.unwrap_or_default();
                let id = new_id();
                let (by_id, by_name) = by.map_or((None, String::new()), |(i, n)| (Some(i), n));
                sqlx::query(
                    "INSERT INTO replica_jobs (id, kind, policy_id, location_id, params, label, created_by, created_by_name, created_at) VALUES (?, 'sync', ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&id)
                .bind(policy)
                .bind(location)
                .bind(json!({ "epoch": p.epoch, "trigger": why, "rate_limit": p.rate_limit }).to_string())
                .bind(format!("{} → {name}", p.name))
                .bind(by_id)
                .bind(&by_name)
                .bind(t)
                .execute(&mut *tx)
                .await?;
                sqlx::query("UPDATE replica_targets SET last_run_at = ?, catch_up = 0 WHERE policy_id = ? AND location_id = ?")
                    .bind(t)
                    .bind(policy)
                    .bind(location)
                    .execute(&mut *tx)
                    .await?;
                Ok(Some(id))
            }
        }
    }
    .await;
    let queued = crate::db::settle(tx, res).await?;
    if queued.is_some() {
        st.part::<Memory>().queue.wake.notify_one();
    }
    Ok(queued)
}

/// A check of a target, when nothing else of it is under way
pub async fn queue_verify(st: &AppState, p: &Policy, location: &str, t: i64) -> AppResult<Option<String>> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        let busy: Option<(i64,)> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT 1 FROM replica_jobs WHERE policy_id = ? AND location_id = ? AND state IN {ACTIVE} LIMIT 1")))
                .bind(&p.id)
                .bind(location)
                .fetch_optional(&mut *tx)
                .await?;
        if busy.is_some() {
            return Ok(None);
        }
        let (name,): (String,) =
            sqlx::query_as("SELECT name FROM storage_locations WHERE id = ?").bind(location).fetch_optional(&mut *tx).await?.unwrap_or_default();
        let id = new_id();
        sqlx::query("INSERT INTO replica_jobs (id, kind, policy_id, location_id, params, label, created_at) VALUES (?, 'verify', ?, ?, ?, ?, ?)")
            .bind(&id)
            .bind(&p.id)
            .bind(location)
            .bind(json!({ "epoch": p.epoch }).to_string())
            .bind(format!("{} → {name}", p.name))
            .bind(t)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE replica_targets SET last_verify_at = ? WHERE policy_id = ? AND location_id = ?")
            .bind(t)
            .bind(&p.id)
            .bind(location)
            .execute(&mut *tx)
            .await?;
        AppResult::Ok(Some(id))
    }
    .await;
    let queued = crate::db::settle(tx, res).await?;
    if queued.is_some() {
        st.part::<Memory>().queue.wake.notify_one();
    }
    Ok(queued)
}

// ───────────── How a policy is doing ─────────────

/// How a target is doing
#[derive(Debug, Clone, Serialize)]
pub struct TargetHealth {
    pub location_id: String,
    /// current (holds every change of the spaces), behind (changes wait), syncing, initializing (never synced), offline
    /// (can't be reached), failed (the last sync stopped by an error), corrupt (a check found damaged copies, not
    /// replaced yet), stale (an old primary not checked yet), paused
    pub state: &'static str,
    /// Since when changes wait (the oldest change not held yet, as seen)
    pub behind_since: Option<i64>,
    pub synced_at: Option<i64>,
    pub last_verify_at: Option<i64>,
    /// Contents of the policy the target holds, and should hold, as last worked out (None: not yet)
    pub held: Option<i64>,
    pub wanted: Option<i64>,
    pub damaged: i64,
    pub error: Option<String>,
}

/// How a policy is doing: its targets, how many copies are wanted and how many targets are current
#[derive(Debug, Clone, Serialize)]
pub struct Health {
    pub targets: Vec<TargetHealth>,
    /// Copies wanted besides the primary, and targets current now
    pub wanted: i64,
    pub current: i64,
    /// Fewer active targets than copies wanted: the policy can't keep them all
    pub shortfall: bool,
    /// The spaces' own location can't be reached now: "{name} can't be reached now" (files are read from replicas)
    pub source_offline: Option<String>,
    /// The worst: ok, behind, degraded (the spaces' location or a target not working), paused
    pub state: &'static str,
}

/// Since when changes of the policy's spaces (`spaces`, its scope now) wait for a sync of a target. A space deleted or
/// taken out of the policy since doesn't count: it is never synced again.
async fn behind_since(st: &AppState, policy: &str, location: &str, spaces: &[String]) -> AppResult<Option<i64>> {
    let (since,): (Option<i64>,) =
        sqlx::query_as("SELECT MIN(since) FROM replica_dirty WHERE policy_id = ? AND location_id = ? AND drive_id IN (SELECT value FROM json_each(?))")
            .bind(policy)
            .bind(location)
            .bind(serde_json::to_string(spaces).unwrap())
            .fetch_one(&st.db)
            .await?;
    Ok(since)
}

/// How a policy is doing, with the contents each target holds and should hold as last worked out (`recount`)
pub async fn health(st: &AppState, p: &Policy, targets: &[Target], t: i64) -> AppResult<Health> {
    let spaces = super::scope(&mut *st.db.acquire().await?, &p.id).await?;
    let mut out = Vec::new();
    for target in targets {
        let l = &target.location_id;
        let behind_since = behind_since(st, &p.id, l, &spaces).await?;
        let (damaged,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM replica_copies WHERE location_id = ? AND state = 'corrupt'").bind(l).fetch_one(&st.db).await?;
        let job: Option<(String, Option<String>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT state, error FROM replica_jobs WHERE policy_id = ? AND location_id = ? AND kind = 'sync' AND state IN {ACTIVE} ORDER BY created_at DESC LIMIT 1"
        )))
        .bind(&p.id)
        .bind(l)
        .fetch_optional(&st.db)
        .await?;
        let (held, wanted) = (target.held, target.wanted);
        let offline = st.location_offline(l).is_some();
        let state = match (&job, target.state.as_str()) {
            _ if !p.enabled => "paused",
            _ if offline => "offline",
            (_, "stale") => "stale",
            (Some((s, _)), _) if s == "failed" => "failed",
            (Some((s, _)), _) if s == "waiting" => "offline",
            _ if damaged > 0 => "corrupt",
            (Some((s, _)), _) if s == "running" && target.synced_at.is_none() => "initializing",
            _ if target.synced_at.is_none() => "initializing",
            _ if held.zip(wanted).is_some_and(|(h, w)| h < w) || behind_since.is_some() => "behind",
            _ => "current",
        };
        out.push(TargetHealth {
            location_id: l.clone(),
            state,
            behind_since,
            synced_at: target.synced_at,
            last_verify_at: target.last_verify_at,
            held,
            wanted,
            damaged,
            error: job.and_then(|(_, e)| e),
        });
    }
    let active = targets.iter().filter(|t| t.state == "active").count() as i64;
    let current = out.iter().filter(|h| h.state == "current").count() as i64;
    let overdue = |h: &TargetHealth| p.alert_hours > 0 && h.behind_since.is_some_and(|s| t - s > p.alert_hours * 3600);
    let source_offline = match st.location_offline(&p.source_location) {
        Some(_) => Some(format!("{} can't be reached now", super::sync::name_of(st, &p.source_location).await)),
        None => None,
    };
    let state = if !p.enabled {
        "paused"
    } else if source_offline.is_some() || out.iter().any(|h| matches!(h.state, "offline" | "failed" | "corrupt") || overdue(h)) || active < p.copies {
        "degraded"
    } else if current < p.copies.min(active) {
        "behind"
    } else {
        "ok"
    };
    Ok(Health { targets: out, wanted: p.copies, current, shortfall: active < p.copies, source_offline, state })
}

/// Tells administrators once when a policy stops keeping its copies (a target not working, damaged copies, too few
/// targets, or behind for longer than it allows), and once when it does again
async fn alert(st: &AppState, p: &Policy, targets: &[Target], t: i64) -> AppResult<()> {
    let h = health(st, p, targets, t).await?;
    let now_state = if h.state == "degraded" { "degraded" } else { "" };
    if now_state == p.alerted {
        return Ok(());
    }
    let error = h.source_offline.clone().or_else(|| h.targets.iter().find_map(|x| x.error.clone()));
    let emails = {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            sqlx::query("UPDATE replica_policies SET alerted = ? WHERE id = ?").bind(now_state).bind(&p.id).execute(&mut *tx).await?;
            let kind = if now_state.is_empty() { "recovered" } else { "degraded" };
            let admins: Vec<i64> = sqlx::query_as::<_, (i64,)>("SELECT id FROM users WHERE role = 'admin' AND disabled = 0")
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .map(|(i,)| i)
                .collect();
            let notice = crate::notify::Notice {
                kind: "replica",
                node_id: None,
                data: json!({ "name": p.name, "state": kind, "error": error, "current": h.current, "wanted": h.wanted }),
            };
            sqlx::query("INSERT INTO activity (at, action, detail) VALUES (?, 'replica_alert', ?)")
                .bind(t)
                .bind(format!("{}: {kind}", p.name))
                .execute(&mut *tx)
                .await?;
            crate::notify::add(&mut tx, &admins, &notice).await
        }
        .await;
        crate::db::settle(tx, res).await?
    };
    crate::notify::send_later(st, emails);
    Ok(())
}
