//! What the schedulers of backup policies (policy.rs) and replica policies (replicas/policy.rs) share, so they can't
//! drift apart: looking at the policies every few seconds, when a run is due, what a run asked for does about one that
//! isn't over, and telling administrators once when a policy's state changes. Each keeps its own tables: a backup
//! policy has one run at a time, a replica policy one per target.

use serde_json::Value;

use super::{
    policy::{Schedule, next_after},
    runner::JobState,
};
use crate::{error::AppResult, state::AppState};

/// How long changes are gathered before a run is made for them
pub const BATCH_SECONDS: i64 = 5;
/// A run that failed is tried again after this long; one waiting for a location that can't be reached, sooner
pub const RETRY_FAILED: i64 = 3600;
pub const RETRY_WAITING: i64 = 300;

/// Looks at the policies every few seconds, and when woken (`Queue::policies`)
pub fn spawn<W, T, F>(st: AppState, what: &'static str, wake: W, tick: T)
where
    W: Fn(&AppState) -> &tokio::sync::Notify + Send + 'static,
    T: Fn(AppState, i64) -> F + Send + 'static,
    F: Future<Output = AppResult<()>> + Send,
{
    tokio::spawn(async move {
        loop {
            if let Err(e) = tick(st.clone(), crate::util::now()).await {
                tracing::warn!("{what}: {}", e.message);
            }
            tokio::select! {
                _ = wake(&st).notified() => {}
                _ = tokio::time::sleep(std::time::Duration::from_secs(BATCH_SECONDS as u64)) => {}
            }
        }
    });
}

/// A backup policy, or a target of a replica policy, as its scheduler looks at it
pub struct Slot {
    /// Runs soon after changes
    pub realtime: bool,
    /// Runs on a schedule, in its time zone
    pub schedule: Option<(Schedule, jiff::tz::TimeZone)>,
    pub next_run_at: Option<i64>,
    /// Changes a run doesn't hold yet wait, and since when the oldest does
    pub changed: bool,
    pub behind_since: Option<i64>,
    /// Changes (or the schedule) came during the last run: one more follows it
    pub catch_up: bool,
    /// It never ran: the first run is due at once (replica targets)
    pub never_ran: bool,
}

/// What is due at `t`: the run and why ('change', 'schedule'), and the next scheduled time to keep when it changes
pub fn due(s: &Slot, t: i64) -> (Option<&'static str>, Option<Option<i64>>) {
    let mut why = None;
    let mut next = None;
    // Changes: backed up once they had a few seconds to settle
    if s.realtime && ((s.changed && s.behind_since.is_some_and(|o| t - o >= BATCH_SECONDS)) || s.never_ran) {
        why = Some("change");
    }
    if let Some((schedule, tz)) = &s.schedule {
        match s.next_run_at {
            Some(at) if at <= t => {
                why = Some("schedule");
                // One catch-up after downtime, then the next time from now
                next = Some(next_after(schedule, tz, t));
            }
            None => {
                next = Some(next_after(schedule, tz, t));
                if s.never_ran {
                    why = Some("schedule");
                }
            }
            _ => {}
        }
    }
    // One more after a run that had changes (or the schedule) come during it
    if why.is_none() && s.catch_up {
        why = Some("change");
    }
    (why, next)
}

/// What a run asked for does about the newest run of the same policy (or target) that isn't over
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing: one is queued already, or a failed or waiting one isn't due to be tried again
    Leave,
    /// The failed or waiting one is queued again (`requeue`)
    Requeue(String),
    /// One is running or paused: one more follows it
    CatchUp,
    /// None isn't over: a new one is queued
    Queue,
}

/// One run at a time: a queued one is left as it is; a failed or waiting one is tried again when asked for by hand,
/// when the location it waits for works again (`back`), or once it last ran long enough ago (`RETRY_*`); a running or
/// paused one gets one more after it. A retry ('retry') only ever tries again: it never queues a new run.
pub fn step(active: Option<(String, JobState)>, why: &str, last_run: Option<i64>, back: bool, t: i64) -> Step {
    match active {
        Some((_, JobState::Queued)) => Step::Leave,
        Some((id, state @ (JobState::Failed | JobState::Waiting))) => {
            let wait = if state == JobState::Failed { RETRY_FAILED } else { RETRY_WAITING };
            let back = back && state == JobState::Waiting;
            if why == "manual" || back || last_run.is_none_or(|l| t - l >= wait) { Step::Requeue(id) } else { Step::Leave }
        }
        Some(_) => Step::CatchUp,
        None if why == "retry" => Step::Leave,
        None => Step::Queue,
    }
}

/// Queues a failed or waiting run of `table` again (`Step::Requeue`), with `params` merged into its parameters; false
/// when it is no longer failed or waiting
pub async fn requeue(conn: &mut sqlx::SqliteConnection, table: &str, id: &str, params: &Value) -> AppResult<bool> {
    let n = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE {table} SET state = 'queued', error = NULL, params = json_patch(params, ?) WHERE id = ? AND state IN ('failed', 'waiting')"
    )))
    .bind(params.to_string())
    .bind(id)
    .execute(conn)
    .await?
    .rows_affected();
    Ok(n == 1)
}

/// Whether `location` was checked since `last_run` and works: a run waiting for it is tried again at once
pub fn back(st: &AppState, location: &str, last_run: Option<i64>) -> bool {
    st.location_health.lock().unwrap().get(location).is_some_and(|h| h.ok && last_run.is_some_and(|l| h.checked_at > l))
}

/// What administrators are told about a policy
pub struct Alert<'a> {
    /// What it is now: what is wrong, "" when nothing is; told when it isn't what they were last told (`alerted`)
    pub state: &'a str,
    pub alerted: &'a str,
    /// Saves `state` as what they were last told: `UPDATE … SET alerted = ?1 WHERE … = ?2`, with `id`
    pub record: &'static str,
    pub id: &'a str,
    /// The notice (`notify::Notice::kind`) and its data, to which `state` is added ("recovered" when nothing is wrong)
    pub notice: &'static str,
    pub data: Value,
    /// The activity log entry: its action, and the policy's name
    pub action: &'static str,
    pub name: &'a str,
}

/// Tells administrators once when a policy's state changes: in a notice, by email, and in the activity log
pub async fn alert(st: &AppState, a: Alert<'_>, t: i64) -> AppResult<()> {
    if a.state == a.alerted {
        return Ok(());
    }
    let kind = if a.state.is_empty() { "recovered" } else { a.state };
    let mut data = a.data;
    data["state"] = kind.into();
    let emails = {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            sqlx::query(a.record).bind(a.state).bind(a.id).execute(&mut *tx).await?;
            let admins: Vec<i64> = sqlx::query_as::<_, (i64,)>("SELECT id FROM users WHERE role = 'admin' AND disabled = 0")
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .map(|(i,)| i)
                .collect();
            let notice = crate::notify::Notice { kind: a.notice, node_id: None, data };
            sqlx::query("INSERT INTO activity (at, action, detail) VALUES (?, ?, ?)")
                .bind(t)
                .bind(a.action)
                .bind(format!("{}: {kind}", a.name))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn slot() -> Slot {
        Slot { realtime: false, schedule: None, next_run_at: None, changed: false, behind_since: None, catch_up: false, never_ran: false }
    }

    #[test]
    fn a_run_is_due_after_changes_settle_on_its_schedule_or_to_catch_up() {
        let t = 1_000_000;
        let tz = jiff::tz::TimeZone::UTC;
        // Changes wait a few seconds to settle
        let changes = |since: i64| Slot { realtime: true, changed: true, behind_since: Some(since), ..slot() };
        assert_eq!(due(&changes(t - 1), t), (None, None));
        assert_eq!(due(&changes(t - BATCH_SECONDS), t), (Some("change"), None));
        assert_eq!(due(&Slot { changed: true, behind_since: Some(0), ..slot() }, t), (None, None), "not soon after changes");
        // The schedule: its time came, the next one is kept; none kept yet, the next one is
        let every = Some((Schedule::Every(10), tz.clone()));
        assert_eq!(due(&Slot { schedule: every.clone(), next_run_at: Some(t), ..slot() }, t), (Some("schedule"), Some(Some(t + 600))));
        assert_eq!(due(&Slot { schedule: every.clone(), next_run_at: Some(t + 1), ..slot() }, t), (None, None));
        assert_eq!(due(&Slot { schedule: every.clone(), ..slot() }, t), (None, Some(Some(t + 600))));
        // Never ran: at once (replica targets)
        assert_eq!(due(&Slot { schedule: every, never_ran: true, ..slot() }, t), (Some("schedule"), Some(Some(t + 600))));
        assert_eq!(due(&Slot { realtime: true, never_ran: true, ..slot() }, t), (Some("change"), None));
        // Changes came during the last run
        assert_eq!(due(&Slot { catch_up: true, ..slot() }, t), (Some("change"), None));
    }

    #[test]
    fn one_run_at_a_time_and_a_retry_never_queues_a_new_one() {
        let t = 1_000_000;
        let run = |state| Some(("job".to_string(), state));
        assert_eq!(step(run(JobState::Queued), "change", None, false, t), Step::Leave);
        assert_eq!(step(run(JobState::Running), "change", None, false, t), Step::CatchUp);
        assert_eq!(step(run(JobState::Paused), "schedule", None, false, t), Step::CatchUp);
        // A failed one: tried again an hour after it last ran, or at once when asked for by hand
        assert_eq!(step(run(JobState::Failed), "retry", Some(t - 60), false, t), Step::Leave);
        assert_eq!(step(run(JobState::Failed), "retry", Some(t - RETRY_FAILED), false, t), Step::Requeue("job".into()));
        assert_eq!(step(run(JobState::Failed), "manual", Some(t - 60), false, t), Step::Requeue("job".into()));
        // A waiting one: sooner, and at once when its location works again
        assert_eq!(step(run(JobState::Waiting), "retry", Some(t - 60), false, t), Step::Leave);
        assert_eq!(step(run(JobState::Waiting), "retry", Some(t - RETRY_WAITING), false, t), Step::Requeue("job".into()));
        assert_eq!(step(run(JobState::Waiting), "retry", Some(t - 60), true, t), Step::Requeue("job".into()));
        assert_eq!(step(run(JobState::Failed), "retry", Some(t - 60), true, t), Step::Leave, "a failure isn't a location coming back");
        // Nothing open
        assert_eq!(step(None, "change", None, false, t), Step::Queue);
        assert_eq!(step(None, "retry", None, false, t), Step::Leave);
    }
}
