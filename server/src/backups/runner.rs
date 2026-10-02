//! Jobs that run in the background one at a time, in the order they were asked for, and are kept afterwards as their
//! history: backups/ (`backup_jobs`) and replicas/ (`replica_jobs`) each have a queue of their own, and an engine that
//! runs their kinds of job. A job that stops (paused, failed, or ThirtyFile restarted) continues where it was, from
//! what it recorded. Modelled on moves/, whose jobs switch spaces over and remove originals: these never do either.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use sqlx::SqliteConnection;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
    util::now,
};

/// Where a backup or replica job is (`backup_jobs.state`, `replica_jobs.state`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum JobState {
    /// Waiting for its turn
    Queued,
    Running,
    /// Paused by an administrator
    Paused,
    /// Waits for a storage location that can't be reached, and runs again once it can
    Waiting,
    /// Stopped by an error; it can be resumed
    Failed,
    Done,
    Cancelled,
}

/// States of a job that isn't over
pub const ACTIVE: &str = "('queued', 'running', 'paused', 'waiting', 'failed')";
/// Items listed in a job's failures (all are counted)
const MAX_FAILURES: usize = 100;
/// Tries for each item before it counts as failed
const TRIES: u32 = 3;
/// How often a running job writes its progress to the database
const FLUSH_EVERY: Duration = Duration::from_secs(2);

/// A table of jobs: the jobs of it running now, and the runner's wake-up call
pub struct Queue {
    /// The table, and its columns as `Job` reads them
    table: &'static str,
    cols: &'static str,
    pub(crate) running: Mutex<HashMap<String, Arc<Control>>>,
    pub(crate) wake: tokio::sync::Notify,
    /// Wakes the scheduler of the policies of the queue (backup or replica policies)
    pub(crate) policies: tokio::sync::Notify,
}

impl Queue {
    fn new(table: &'static str, cols: &'static str) -> Queue {
        Queue { table, cols, running: Default::default(), wake: Default::default(), policies: Default::default() }
    }

    /// The jobs of backups/: `set_id` is the set, `snapshot_id` the snapshot
    pub fn backups() -> Queue {
        Queue::new("backup_jobs", "id, kind, set_id, snapshot_id, params, label, created_by, created_by_name")
    }

    /// The jobs of replicas/: `set_id` is the replica policy, `snapshot_id` the location the job works on
    pub fn replicas() -> Queue {
        Queue::new("replica_jobs", "id, kind, policy_id AS set_id, location_id AS snapshot_id, params, label, created_by, created_by_name")
    }
}

/// What runs the jobs of a queue
pub trait Engine: Send + Sync + 'static {
    fn queue<'a>(&self, st: &'a AppState) -> &'a Queue;
    /// Runs a job to its end, or until it is asked to stop
    fn run<'a>(&'a self, cx: &'a Ctx<'a>) -> BoxFuture<'a, AppResult<Stop>>;
    /// A job asked to be cancelled: records it as cancelled, and undoes what the kind of job undoes
    fn cancelled<'a>(&'a self, st: &'a AppState, job: &'a Job) -> BoxFuture<'a, AppResult<()>>;
    /// Whether a job that failed waits instead (a location that can't be reached, tried again by itself)
    fn waits<'a>(&'a self, st: &'a AppState, job: &'a Job, error: &'a AppError) -> BoxFuture<'a, bool>;
    /// The spaces whose items aren't named in the job's failures (personal spaces)
    fn private<'a>(&'a self, st: &'a AppState, job: &'a Job) -> BoxFuture<'a, AppResult<HashSet<String>>>;
    /// The activity log action of a job that failed
    fn failed_action(&self) -> &'static str;
}

/// What a running job is asked to do, and how far it got
#[derive(Default)]
pub struct Control {
    pub(crate) pause: AtomicBool,
    pub(crate) cancel: AtomicBool,
    pub(crate) progress: Mutex<Progress>,
}

#[derive(Default)]
pub(crate) struct Progress {
    pub files_done: i64,
    pub bytes_done: i64,
    pub files_total: i64,
    pub bytes_total: i64,
    failed: i64,
    failures: Vec<Failure>,
    flushed: Option<(Instant, i64)>,
    /// Bytes copied per second, smoothed
    pub rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Failure {
    /// Which item (its path or name); None in personal spaces, whose content administrators don't see
    pub item: Option<String>,
    pub error: String,
}

/// Why a job's run ended without an error
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Done,
    Paused,
    Cancelled,
}

/// A job as the database has it
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub set_id: String,
    pub snapshot_id: Option<String>,
    pub params: String,
    pub label: String,
    pub created_by: Option<i64>,
    pub created_by_name: String,
}

/// A job of backups/
pub async fn job(conn: &mut SqliteConnection, id: &str) -> AppResult<Option<Job>> {
    job_in(conn, &Queue::backups(), id).await
}

/// A job of a queue
pub async fn job_in(conn: &mut SqliteConnection, q: &Queue, id: &str) -> AppResult<Option<Job>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {} FROM {} WHERE id = ?", q.cols, q.table))).bind(id).fetch_optional(conn).await?)
}

/// A running job, as its engine sees it
pub struct Ctx<'a> {
    pub st: &'a AppState,
    pub job: &'a Job,
    pub(crate) ctl: &'a Control,
    /// The table of the job
    pub(crate) table: &'static str,
    /// Personal spaces' items aren't named in failures
    pub private: std::collections::HashSet<String>,
    /// Bytes per second the job may copy (0: no limit), and when it started copying
    pub rate_limit: i64,
    pub started: Instant,
}

impl Ctx<'_> {
    /// Whether the job was asked to stop (checked between items)
    pub fn stop(&self) -> Option<Stop> {
        if self.ctl.cancel.load(Ordering::SeqCst) {
            Some(Stop::Cancelled)
        } else if self.ctl.pause.load(Ordering::SeqCst) {
            Some(Stop::Paused)
        } else {
            None
        }
    }

    pub fn set_counts(&self, files_done: i64, bytes_done: i64, files_total: i64, bytes_total: i64) {
        let mut p = self.ctl.progress.lock().unwrap();
        (p.files_done, p.bytes_done, p.files_total, p.bytes_total) = (files_done, bytes_done, files_total, bytes_total);
        p.flushed = None;
    }

    /// What is left is `files` and `bytes`: the totals are what was done so far and that
    pub fn set_left(&self, files: i64, bytes: i64) {
        let mut p = self.ctl.progress.lock().unwrap();
        p.files_total = p.files_done + files;
        p.bytes_total = p.bytes_done + bytes;
    }

    pub fn add_total(&self, files: i64, bytes: i64) {
        let mut p = self.ctl.progress.lock().unwrap();
        p.files_total += files;
        p.bytes_total += bytes;
    }

    /// An item was done: counted, and written to the database every few seconds
    pub async fn done(&self, files: i64, bytes: i64) -> AppResult<()> {
        let due = {
            let mut p = self.ctl.progress.lock().unwrap();
            p.files_done += files;
            p.bytes_done += bytes;
            p.files_total = p.files_total.max(p.files_done);
            p.bytes_total = p.bytes_total.max(p.bytes_done);
            p.flushed.is_none_or(|(at, _)| at.elapsed() >= FLUSH_EVERY)
        };
        if due {
            self.flush().await?;
        }
        self.throttle().await;
        Ok(())
    }

    /// Waits while the job copied more than its limit allows so far (a policy's bandwidth limit)
    async fn throttle(&self) {
        if self.rate_limit <= 0 {
            return;
        }
        let done = self.ctl.progress.lock().unwrap().bytes_done.max(0) as f64;
        let due = Duration::from_secs_f64(done / self.rate_limit as f64);
        while let Some(wait) = throttle_wait(due, self.started.elapsed())
            && self.stop().is_none()
        {
            tokio::time::sleep(wait).await;
        }
    }

    /// An item that couldn't be done after a few tries. `item` names it; `space` is the space it is in (items of
    /// personal spaces aren't named).
    pub fn failed(&self, space: &str, item: Option<String>, error: String) {
        tracing::warn!("{}: an item couldn't be copied: {error}", self.table);
        let item = item.filter(|_| !self.private.contains(space));
        let mut p = self.ctl.progress.lock().unwrap();
        p.failed += 1;
        if p.failures.len() < MAX_FAILURES {
            p.failures.push(Failure { item, error });
        }
    }

    /// An item to tell about that isn't a failure (a file that kept changing, say): listed with the failures, not
    /// counted. Items of personal spaces aren't named.
    pub fn noted(&self, space: &str, item: Option<String>, note: String) {
        let item = item.filter(|_| !self.private.contains(space));
        let mut p = self.ctl.progress.lock().unwrap();
        if p.failures.len() < MAX_FAILURES {
            p.failures.push(Failure { item, error: note });
        }
    }

    pub fn failed_count(&self) -> i64 {
        self.ctl.progress.lock().unwrap().failed
    }

    /// The failures so far as one error, None when there are none
    pub fn failures_error(&self) -> Option<AppError> {
        let failed = self.failed_count();
        (failed > 0).then(|| {
            AppError::new(
                axum::http::StatusCode::BAD_GATEWAY,
                if failed == 1 { "1 item couldn't be copied".to_string() } else { format!("{failed} items couldn't be copied") },
            )
        })
    }

    fn progress(&self) -> (i64, i64, i64, i64, i64, String) {
        let mut p = self.ctl.progress.lock().unwrap();
        let now = Instant::now();
        if let Some((at, bytes)) = p.flushed {
            let secs = now.duration_since(at).as_secs_f64();
            if secs > 0.2 {
                let rate = (p.bytes_done - bytes).max(0) as f64 / secs;
                p.rate = if p.rate == 0.0 { rate } else { p.rate * 0.7 + rate * 0.3 };
            }
        }
        p.flushed = Some((now, p.bytes_done));
        (p.files_done, p.bytes_done, p.files_total, p.bytes_total, p.failed, serde_json::to_string(&p.failures).unwrap())
    }

    /// Writes the progress to the database
    pub async fn flush(&self) -> AppResult<()> {
        let (files_done, bytes_done, files_total, bytes_total, failed, failures) = self.progress();
        let _w = self.st.write_lock.lock().await;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {} SET files_done = ?, bytes_done = ?, files_total = ?, bytes_total = ?, failed_items = ?, failures = ?
             WHERE id = ? AND state = 'running'",
            self.table
        )))
        .bind(files_done)
        .bind(bytes_done)
        .bind(files_total)
        .bind(bytes_total)
        .bind(failed)
        .bind(failures)
        .bind(&self.job.id)
        .execute(&self.st.db)
        .await?;
        Ok(())
    }

    /// Tries `f` up to `TRIES` times while it fails with an error `retry` accepts, waiting longer each time. Err(Ok(stop))
    /// when the job is asked to stop in between.
    pub async fn tries<T, E, F, Fut>(&self, retry: impl Fn(&E) -> bool, mut f: F) -> Result<T, Result<Stop, E>>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
    {
        let mut attempt = 1;
        loop {
            match f().await {
                Ok(v) => return Ok(v),
                Err(e) if attempt < TRIES && retry(&e) => {
                    let wait = Duration::from_secs(if cfg!(test) { 0 } else { 2u64.pow(attempt) });
                    let until = Instant::now() + wait;
                    while Instant::now() < until {
                        if let Some(stop) = self.stop() {
                            return Err(Ok(stop));
                        }
                        tokio::time::sleep(Duration::from_millis(200).min(until - Instant::now())).await;
                    }
                    if let Some(stop) = self.stop() {
                        return Err(Ok(stop));
                    }
                    attempt += 1;
                }
                Err(e) => return Err(Err(e)),
            }
        }
    }
}

/// How long a throttled job still waits, a little at a time, once it is `elapsed` into a run that may only be done at
/// `due`; None when it may go on. The time goes on while it is computed, so it never subtracts a later time.
fn throttle_wait(due: Duration, elapsed: Duration) -> Option<Duration> {
    let left = due.saturating_sub(elapsed);
    (!left.is_zero()).then(|| left.min(Duration::from_millis(500)))
}

/// The engine of backups/
pub struct BackupEngine;

pub static BACKUPS: BackupEngine = BackupEngine;

impl Engine for BackupEngine {
    fn queue<'a>(&self, st: &'a AppState) -> &'a Queue {
        &st.backups
    }

    fn run<'a>(&'a self, cx: &'a Ctx<'a>) -> BoxFuture<'a, AppResult<Stop>> {
        Box::pin(async move {
            match cx.job.kind.as_str() {
                "snapshot" => super::capture::run(cx).await,
                "restore" => super::restore::run(cx).await,
                "verify" => super::tidy::verify(cx).await,
                "remove" => super::tidy::remove(cx).await,
                _ => Err(AppError::internal("unknown job")),
            }
        })
    }

    fn cancelled<'a>(&'a self, st: &'a AppState, job: &'a Job) -> BoxFuture<'a, AppResult<()>> {
        Box::pin(super::cancelled(st, job))
    }

    fn waits<'a>(&'a self, st: &'a AppState, job: &'a Job, e: &'a AppError) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            // A policy's snapshot waits for a location that can't be reached, and is tried again by itself
            if job.kind != "snapshot" {
                return false;
            }
            let Ok(mut c) = st.db.acquire().await else { return false };
            if !super::policy::is_policy(&mut c, &job.set_id).await.unwrap_or(false) {
                return false;
            }
            drop(c);
            e.status == axum::http::StatusCode::SERVICE_UNAVAILABLE || super::policy::waits(st, job).await
        })
    }

    fn private<'a>(&'a self, st: &'a AppState, job: &'a Job) -> BoxFuture<'a, AppResult<HashSet<String>>> {
        Box::pin(private_spaces(st, job))
    }

    fn failed_action(&self) -> &'static str {
        "backup_failed"
    }
}

/// Starts the runner of backups/ and the scheduler of backup policies
pub fn spawn_runner(st: AppState) {
    super::policy::spawn_scheduler(st.clone());
    spawn(st, &BACKUPS);
}

/// Starts a queue's runner: jobs that were running when ThirtyFile stopped continue, then queued jobs start in turn
pub fn spawn(st: AppState, engine: &'static dyn Engine) {
    tokio::spawn(async move {
        if let Err(e) = recover_in(&st, engine.queue(&st)).await {
            tracing::warn!("Couldn't continue the jobs of {} that were running: {}", engine.queue(&st).table, e.message);
        }
        loop {
            if let Err(e) = start_due_in(&st, engine).await {
                tracing::warn!("Couldn't start a job of {}: {}", engine.queue(&st).table, e.message);
            }
            tokio::select! {
                _ = engine.queue(&st).wake.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(30)) => {}
            }
        }
    });
}

/// After a restart: jobs of backups/ that were running wait for their turn again
#[cfg(test)]
pub(super) async fn recover(st: &AppState) -> AppResult<()> {
    recover_in(st, &st.backups).await
}

/// After a restart: jobs that were running wait for their turn again, ahead of newer ones
pub(crate) async fn recover_in(st: &AppState, q: &Queue) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE {} SET state = 'queued' WHERE state = 'running'", q.table))).execute(&st.db).await?;
    Ok(())
}

/// Jobs recorded as running that no task runs (recording how one ended failed, say): they wait for their turn again
async fn requeue_orphans(st: &AppState, q: &Queue) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    let running: Vec<String> = q.running.lock().unwrap().keys().cloned().collect();
    sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE {} SET state = 'queued' WHERE state = 'running' AND id NOT IN (SELECT value FROM json_each(?))", q.table)))
        .bind(serde_json::to_string(&running).unwrap())
        .execute(&st.db)
        .await?;
    Ok(())
}

/// Starts the oldest queued job when none of the queue is running: one at a time, so copying never takes more than one
/// transfer from what people do
pub(crate) async fn start_due_in(st: &AppState, engine: &'static dyn Engine) -> AppResult<()> {
    let q = engine.queue(st);
    requeue_orphans(st, q).await?;
    if !q.running.lock().unwrap().is_empty() {
        return Ok(());
    }
    let next: Option<Job> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {} FROM {} WHERE state = 'queued' ORDER BY created_at, rowid LIMIT 1", q.cols, q.table)))
            .fetch_optional(&st.db)
            .await?;
    let Some(job) = next else { return Ok(()) };
    let Some(ctl) = take_in(st, engine, &job).await? else { return Ok(()) };
    let st = st.clone();
    tokio::spawn(crate::usage::background(async move { run_in(&st, engine, &job, &ctl).await }));
    Ok(())
}

/// Marks a queued job of backups/ as running (tests)
#[cfg(test)]
pub(super) async fn take(st: &AppState, job: &Job) -> AppResult<Option<Arc<Control>>> {
    take_in(st, &BACKUPS, job).await
}

/// Marks a queued job as running, with its controls in place first; None when it was paused or cancelled meanwhile
pub(crate) async fn take_in(st: &AppState, engine: &'static dyn Engine, job: &Job) -> AppResult<Option<Arc<Control>>> {
    let q = engine.queue(st);
    let ctl = Arc::new(Control::default());
    q.running.lock().unwrap().insert(job.id.clone(), ctl.clone());
    let taken = async {
        let _w = st.write_lock.lock().await;
        let n = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {} SET state = 'running', started_at = COALESCE(started_at, ?), error = NULL WHERE id = ? AND state = 'queued'",
            q.table
        )))
        .bind(now())
        .bind(&job.id)
        .execute(&st.db)
        .await?
        .rows_affected();
        AppResult::Ok(n == 1)
    }
    .await;
    if !matches!(taken, Ok(true)) {
        q.running.lock().unwrap().remove(&job.id);
    }
    Ok(taken?.then_some(ctl))
}

/// Takes a job off the running list when its run ends, however it ends
struct Running<'a>(&'a Queue, String);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.running.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.1);
        self.0.wake.notify_one();
    }
}

/// The personal spaces among the spaces a job of backups/ works on: their items aren't named in its failures
async fn private_spaces(st: &AppState, job: &Job) -> AppResult<HashSet<String>> {
    let params: serde_json::Value = serde_json::from_str(&job.params).unwrap_or_default();
    let mut ids: Vec<String> = params["spaces"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
    ids.extend(params["space"].as_str().map(str::to_string));
    ids.extend(params["target_drive"].as_str().map(str::to_string));
    let mut private: HashSet<String> = sqlx::query_as::<_, (String,)>("SELECT id FROM drives WHERE kind = 'personal' AND id IN (SELECT value FROM json_each(?))")
        .bind(serde_json::to_string(&ids).unwrap())
        .fetch_all(&st.db)
        .await?
        .into_iter()
        .map(|(id,)| id)
        .collect();
    // A restore's space as the snapshot has it (deleted since, maybe)
    if params["space_kind"].as_str() == Some("personal") {
        private.extend(params["space"].as_str().map(str::to_string));
    }
    // Spaces a snapshot lists as personal, for a verify or a restore of a space gone since
    if let Some(snap) = &job.snapshot_id {
        let list: Option<(String,)> = sqlx::query_as("SELECT space_list FROM backup_snapshots WHERE id = ?").bind(snap).fetch_optional(&st.db).await?;
        if let Some((list,)) = list {
            let spaces: Vec<super::SpaceInfo> = serde_json::from_str(&list).unwrap_or_default();
            private.extend(spaces.into_iter().filter(|s| s.kind == "personal").map(|s| s.id));
        }
    }
    Ok(private)
}

/// Runs a job of backups/ that was just marked as running (tests)
#[cfg(test)]
pub(super) async fn run(st: &AppState, job: &Job, ctl: &Control) {
    run_in(st, &BACKUPS, job, ctl).await
}

/// Runs a job that was just marked as running, and records how it ended
pub(crate) async fn run_in(st: &AppState, engine: &'static dyn Engine, job: &Job, ctl: &Control) {
    let q = engine.queue(st);
    let _running = Running(q, job.id.clone());
    let private = match engine.private(st, job).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("{}: couldn't read a job's spaces: {}", q.table, e.message);
            Default::default()
        }
    };
    let rate_limit = serde_json::from_str::<serde_json::Value>(&job.params).ok().and_then(|p| p["rate_limit"].as_i64()).unwrap_or(0);
    let cx = Ctx { st, job, ctl, table: q.table, private, rate_limit, started: Instant::now() };
    let res = engine.run(&cx).await;
    let ended = match res {
        Ok(Stop::Done) => Ok(()),
        Ok(Stop::Paused) => set_state(&cx, JobState::Paused, None, engine.failed_action()).await,
        Ok(Stop::Cancelled) => engine.cancelled(st, job).await,
        Err(e) => {
            tracing::warn!("A job of {} ({}) failed: {}", q.table, job.kind, e.message);
            let state = if engine.waits(st, job, &e).await { JobState::Waiting } else { JobState::Failed };
            set_state(&cx, state, Some(&e.message), engine.failed_action()).await
        }
    };
    q.policies.notify_one();
    if let Err(e) = ended {
        tracing::warn!("Couldn't record how a job of {} ended: {}", q.table, e.message);
    }
}

/// Records that a run stopped (paused, or failed with `error`), with its progress
async fn set_state(cx: &Ctx<'_>, state: JobState, error: Option<&str>, action: &str) -> AppResult<()> {
    let (files_done, bytes_done, files_total, bytes_total, failed, failures) = cx.progress();
    let _w = cx.st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&cx.st.db).await?;
    let res = async {
        let stopped = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {} SET state = ?, error = ?, files_done = ?, bytes_done = ?, files_total = ?, bytes_total = ?, failed_items = ?, failures = ?
             WHERE id = ? AND state = 'running'",
            cx.table
        )))
        .bind(state)
        .bind(error)
        .bind(files_done)
        .bind(bytes_done)
        .bind(files_total)
        .bind(bytes_total)
        .bind(failed)
        .bind(failures)
        .bind(&cx.job.id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if stopped && let Some(error) = error {
            super::log(&mut tx, cx.job, action, &format!("{}: {error}", cx.job.label)).await?;
        }
        AppResult::Ok(())
    }
    .await;
    crate::db::settle(tx, res).await
}

/// In the transaction that ends a job well: it is done, with its progress and `note`
pub(crate) async fn finish(conn: &mut SqliteConnection, cx: &Ctx<'_>, note: Option<&str>) -> AppResult<()> {
    let (files, bytes, failed, failures) = {
        let p = cx.ctl.progress.lock().unwrap();
        (p.files_done, p.bytes_done, p.failed, serde_json::to_string(&p.failures).unwrap())
    };
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE {} SET state = 'done', finished_at = ?4, files_done = ?1, bytes_done = ?2, files_total = ?1, bytes_total = ?2,
                       failed_items = ?5, failures = ?6, error = NULL, note = ?7
         WHERE id = ?3",
        cx.table
    )))
    .bind(files)
    .bind(bytes)
    .bind(&cx.job.id)
    .bind(now())
    .bind(failed)
    .bind(failures)
    .bind(note)
    .execute(conn)
    .await?;
    Ok(())
}

/// The progress of a running job, fresher than the database (written every few seconds): files and bytes done and in
/// all, and the speed
pub(crate) fn live(q: &Queue, id: &str) -> Option<(i64, i64, i64, i64, f64)> {
    let running = q.running.lock().unwrap();
    let ctl = running.get(id)?;
    let p = ctl.progress.lock().unwrap();
    Some((p.files_done, p.bytes_done, p.files_total, p.bytes_total, p.rate))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    #[test]
    fn a_throttle_never_waits_a_negative_time() {
        let due = Duration::from_millis(1000);
        assert_eq!(super::throttle_wait(due, Duration::from_millis(200)), Some(Duration::from_millis(500)));
        assert_eq!(super::throttle_wait(due, Duration::from_millis(900)), Some(Duration::from_millis(100)));
        // Past the time it was due by when it is computed again: no wait, and no panic
        assert_eq!(super::throttle_wait(due, due), None);
        assert_eq!(super::throttle_wait(due, Duration::from_millis(1001)), None);
    }
}
