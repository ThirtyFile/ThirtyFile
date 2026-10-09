//! Jobs that run in the background in the order they were asked for, and are kept afterwards as their history:
//! backups/ (`backup_jobs`), replicas/ (`replica_jobs`) and moves/ (`space_moves`) each have a queue of their own, and
//! an engine that runs their kinds of job. Backups and replicas run one job at a time, moves as many as the Moves page
//! allows. A job that stops (paused, failed, or ThirtyFile restarted) continues where it was, from what it recorded.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crate::sync::Mutex;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use sqlx::SqliteConnection;

use crate::{
    backups::Memory,
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
    /// The queue of `table`, whose jobs are read with the columns `cols`
    pub(crate) fn new(table: &'static str, cols: &'static str) -> Queue {
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

/// A job as its queue's table has it
pub trait QueueJob: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> + Send + Sync + Unpin + 'static {
    fn id(&self) -> &str;
    /// What it is, in the server's log ("snapshot", "the space Sales")
    fn describe(&self) -> String;
    /// Bytes per second it may copy (0: no limit)
    fn rate_limit(&self) -> i64 {
        0
    }
}

/// What runs the jobs of a queue
pub trait Engine: Send + Sync + 'static {
    type Job: QueueJob;
    fn queue<'a>(&self, st: &'a AppState) -> &'a Queue;
    /// Jobs of the queue that run at the same time
    fn limit(&self, _st: &AppState) -> usize {
        1
    }
    /// Runs a job to its end, or until it is asked to stop
    fn run<'a>(&'a self, cx: &'a Ctx<'a, Self::Job>) -> BoxFuture<'a, AppResult<Stop>>;
    /// In the transaction that marks a job as running: what else starts with it
    fn taken<'a>(&'a self, _conn: &'a mut SqliteConnection, _job: &'a Self::Job) -> BoxFuture<'a, AppResult<()>> {
        Box::pin(async { Ok(()) })
    }
    /// A job asked to be cancelled: records it as cancelled, and undoes what the kind of job undoes
    fn cancelled<'a>(&'a self, st: &'a AppState, job: &'a Self::Job) -> BoxFuture<'a, AppResult<()>>;
    /// Whether a job that failed waits instead (a location that can't be reached, tried again by itself)
    fn waits<'a>(&'a self, st: &'a AppState, job: &'a Self::Job, error: &'a AppError) -> BoxFuture<'a, bool>;
    /// The spaces whose items aren't named in the job's failures (personal spaces)
    fn private<'a>(&'a self, st: &'a AppState, job: &'a Self::Job) -> BoxFuture<'a, AppResult<HashSet<String>>>;
    /// In the transaction that records that a job failed: its entry in the activity log
    fn log_failure<'a>(&'a self, conn: &'a mut SqliteConnection, job: &'a Self::Job, error: &'a str) -> BoxFuture<'a, AppResult<()>>;
    /// After a restart, once the jobs that were running wait for their turn again: what else is finished first
    fn recovered<'a>(&'a self, _st: &'a AppState) -> BoxFuture<'a, AppResult<()>> {
        Box::pin(async { Ok(()) })
    }
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
pub async fn job_in<J: QueueJob>(conn: &mut SqliteConnection, q: &Queue, id: &str) -> AppResult<Option<J>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {} FROM {} WHERE id = ?", q.cols, q.table))).bind(id).fetch_optional(conn).await?)
}

impl QueueJob for Job {
    fn id(&self) -> &str {
        &self.id
    }

    fn describe(&self) -> String {
        self.kind.clone()
    }

    fn rate_limit(&self) -> i64 {
        serde_json::from_str::<serde_json::Value>(&self.params).ok().and_then(|p| p["rate_limit"].as_i64()).unwrap_or(0)
    }
}

/// A running job, as its engine sees it
pub struct Ctx<'a, J = Job> {
    pub st: &'a AppState,
    pub job: &'a J,
    pub(crate) ctl: &'a Control,
    /// The table of the job
    pub(crate) table: &'static str,
    /// Personal spaces' items aren't named in failures
    pub private: std::collections::HashSet<String>,
    /// Bytes per second the job may copy (0: no limit), and when it started copying
    pub rate_limit: i64,
    pub started: Instant,
}

impl<J: QueueJob> Ctx<'_, J> {
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
        let mut p = self.ctl.progress.lock();
        (p.files_done, p.bytes_done, p.files_total, p.bytes_total) = (files_done, bytes_done, files_total, bytes_total);
        p.flushed = None;
    }

    /// What is left is `files` and `bytes`: the totals are what was done so far and that
    pub fn set_left(&self, files: i64, bytes: i64) {
        let mut p = self.ctl.progress.lock();
        p.files_total = p.files_done + files;
        p.bytes_total = p.bytes_done + bytes;
    }

    pub fn add_total(&self, files: i64, bytes: i64) {
        let mut p = self.ctl.progress.lock();
        p.files_total += files;
        p.bytes_total += bytes;
    }

    /// An item was done: counted, and written to the database every few seconds
    pub async fn done(&self, files: i64, bytes: i64) -> AppResult<()> {
        let due = {
            let mut p = self.ctl.progress.lock();
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
        let done = self.ctl.progress.lock().bytes_done.max(0) as f64;
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
        let mut p = self.ctl.progress.lock();
        p.failed += 1;
        if p.failures.len() < MAX_FAILURES {
            p.failures.push(Failure { item, error });
        }
    }

    /// An item to tell about that isn't a failure (a file that kept changing, say): listed with the failures, not
    /// counted. Items of personal spaces aren't named.
    pub fn noted(&self, space: &str, item: Option<String>, note: String) {
        let item = item.filter(|_| !self.private.contains(space));
        let mut p = self.ctl.progress.lock();
        if p.failures.len() < MAX_FAILURES {
            p.failures.push(Failure { item, error: note });
        }
    }

    pub fn failed_count(&self) -> i64 {
        self.ctl.progress.lock().failed
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
        let mut p = self.ctl.progress.lock();
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
        .bind(self.job.id())
        .execute(&self.st.db)
        .await?;
        Ok(())
    }

    /// Waits before trying an item again (`attempt` from 1), longer each time; None when the job is asked to stop
    /// meanwhile
    pub async fn wait_to_retry(&self, attempt: u32) -> Option<Stop> {
        let wait = Duration::from_secs(if cfg!(test) { 0 } else { 2u64.pow(attempt) });
        let until = Instant::now() + wait;
        while Instant::now() < until {
            if let Some(stop) = self.stop() {
                return Some(stop);
            }
            tokio::time::sleep(Duration::from_millis(200).min(until - Instant::now())).await;
        }
        self.stop()
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
                    if let Some(stop) = self.wait_to_retry(attempt).await {
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
    type Job = Job;

    fn queue<'a>(&self, st: &'a AppState) -> &'a Queue {
        &st.part::<Memory>().queue
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

    fn log_failure<'a>(&'a self, conn: &'a mut SqliteConnection, job: &'a Job, error: &'a str) -> BoxFuture<'a, AppResult<()>> {
        Box::pin(async move { Ok(super::log(conn, job, "backup_failed", &format!("{}: {error}", job.label)).await?) })
    }
}

/// Starts the runner of backups/ and the scheduler of backup policies
pub fn spawn_runner(st: AppState) {
    super::policy::spawn_scheduler(st.clone());
    spawn(st, &BACKUPS);
}

/// Starts a queue's runner: jobs that were running when ThirtyFile stopped continue, then queued jobs start in turn
pub fn spawn<E: Engine>(st: AppState, engine: &'static E) {
    crate::util::supervise("job queue", move |restarted| run_queue(st.clone(), engine, restarted));
}

/// A queue's runner; after a restart of the runner alone, the jobs marked running are still running
async fn run_queue<E: Engine>(st: AppState, engine: &'static E, restarted: bool) {
    {
        if !restarted && let Err(e) = recover_with(&st, engine).await {
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
    }
}

/// After a restart: jobs of backups/ that were running wait for their turn again
#[cfg(test)]
pub(super) async fn recover(st: &AppState) -> AppResult<()> {
    recover_with(st, &BACKUPS).await
}

/// After a restart: jobs that were running wait for their turn again, ahead of newer ones
pub(crate) async fn recover_in(st: &AppState, q: &Queue) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE {} SET state = 'queued' WHERE state = 'running'", q.table))).execute(&st.db).await?;
    Ok(())
}

/// After a restart: jobs that were running wait for their turn again, then the engine finishes what else it left
pub(crate) async fn recover_with<E: Engine>(st: &AppState, engine: &'static E) -> AppResult<()> {
    recover_in(st, engine.queue(st)).await?;
    engine.recovered(st).await
}

/// Jobs recorded as running that no task runs (recording how one ended failed, on a full disk say, or it panicked):
/// they wait for their turn again, as after a restart, rather than stay running with pause, resume and cancel refused
async fn requeue_orphans(st: &AppState, q: &Queue) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    // Read with the write lock held: a task takes its place in the list before its job is marked running (`take_in`),
    // and leaves it only after its last write (`Running`)
    let running: Vec<String> = q.running.lock().keys().cloned().collect();
    let orphans =
        sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE {} SET state = 'queued' WHERE state = 'running' AND id NOT IN (SELECT value FROM json_each(?))", q.table)))
            .bind(serde_json::to_string(&running).unwrap())
            .execute(&st.db)
            .await?
            .rows_affected();
    if orphans > 0 {
        tracing::warn!("{orphans} jobs of {} were left running without anything running them: they continue in turn", q.table);
    }
    Ok(())
}

/// Starts the oldest queued jobs while fewer than the engine's limit are running (backups and replicas one at a time,
/// so copying never takes more than one transfer from what people do)
pub(crate) async fn start_due_in<E: Engine>(st: &AppState, engine: &'static E) -> AppResult<()> {
    let q = engine.queue(st);
    requeue_orphans(st, q).await?;
    loop {
        let busy: Vec<String> = q.running.lock().keys().cloned().collect();
        if busy.len() >= engine.limit(st) {
            return Ok(());
        }
        let next: Option<E::Job> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {} FROM {} WHERE state = 'queued' AND id NOT IN (SELECT value FROM json_each(?)) ORDER BY created_at, rowid LIMIT 1",
            q.cols, q.table
        )))
        .bind(serde_json::to_string(&busy).unwrap())
        .fetch_optional(&st.db)
        .await?;
        let Some(job) = next else { return Ok(()) };
        let Some(ctl) = take_in(st, engine, &job).await? else { continue };
        let st = st.clone();
        // Counted apart from what people do (Storage usage)
        tokio::spawn(crate::usage::background(async move { run_in(&st, engine, &job, &ctl).await }));
    }
}

/// Marks a queued job of backups/ as running (tests)
#[cfg(test)]
pub(super) async fn take(st: &AppState, job: &Job) -> AppResult<Option<Arc<Control>>> {
    take_in(st, &BACKUPS, job).await
}

/// Marks a queued job as running, with its controls in place first (a pause asked for right away finds them); None
/// when it was paused or cancelled meanwhile
pub(crate) async fn take_in<E: Engine>(st: &AppState, engine: &'static E, job: &E::Job) -> AppResult<Option<Arc<Control>>> {
    let q = engine.queue(st);
    let ctl = Arc::new(Control::default());
    q.running.lock().insert(job.id().to_string(), ctl.clone());
    let taken = async {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            let taken = sqlx::query(sqlx::AssertSqlSafe(format!(
                "UPDATE {} SET state = 'running', started_at = COALESCE(started_at, ?), error = NULL WHERE id = ? AND state = 'queued'",
                q.table
            )))
            .bind(now())
            .bind(job.id())
            .execute(&mut *tx)
            .await?
            .rows_affected()
                == 1;
            if taken {
                engine.taken(&mut tx, job).await?;
            }
            AppResult::Ok(taken)
        }
        .await;
        crate::db::settle(tx, res).await
    }
    .await;
    if !matches!(taken, Ok(true)) {
        q.running.lock().remove(job.id());
    }
    Ok(taken?.then_some(ctl))
}

/// Takes a job off the running list when its run ends, however it ends
struct Running<'a>(&'a Queue, String);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.running.lock().remove(&self.1);
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
pub(crate) async fn run_in<E: Engine>(st: &AppState, engine: &'static E, job: &E::Job, ctl: &Control) {
    let q = engine.queue(st);
    let _running = Running(q, job.id().to_string());
    let private = match engine.private(st, job).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("{}: couldn't read a job's spaces: {}", q.table, e.message);
            Default::default()
        }
    };
    let cx = Ctx { st, job, ctl, table: q.table, private, rate_limit: job.rate_limit(), started: Instant::now() };
    let res = engine.run(&cx).await;
    let ended = match res {
        Ok(Stop::Done) => Ok(()),
        Ok(Stop::Paused) => set_state(&cx, engine, JobState::Paused, None).await,
        Ok(Stop::Cancelled) => engine.cancelled(st, job).await,
        Err(e) => {
            tracing::warn!("A job of {} ({}) failed: {}", q.table, job.describe(), e.message);
            let state = if engine.waits(st, job, &e).await { JobState::Waiting } else { JobState::Failed };
            set_state(&cx, engine, state, Some(&e.message)).await
        }
    };
    q.policies.notify_one();
    if let Err(e) = ended {
        tracing::warn!("Couldn't record how a job of {} ({}) ended: {}", q.table, job.describe(), e.message);
    }
}

/// Records that a run stopped (paused, or failed with `error`), with its progress. A job that ended meanwhile stays as
/// it is (a move that switched its space over is done, and stays done).
async fn set_state<E: Engine>(cx: &Ctx<'_, E::Job>, engine: &E, state: JobState, error: Option<&str>) -> AppResult<()> {
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
        .bind(cx.job.id())
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if stopped && let Some(error) = error {
            engine.log_failure(&mut tx, cx.job, error).await?;
        }
        AppResult::Ok(())
    }
    .await;
    crate::db::settle(tx, res).await
}

/// In the transaction that ends a job of backups/ or replicas/ well: it is done, with its progress and `note`
pub(crate) async fn finish(conn: &mut SqliteConnection, cx: &Ctx<'_>, note: Option<&str>) -> AppResult<()> {
    let (files, bytes, failed, failures) = {
        let p = cx.ctl.progress.lock();
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

// ───────────── History ─────────────

/// Finished jobs (done or cancelled) are kept this many days as their history…
pub const HISTORY_DAYS: i64 = 90;
/// …the newest of each set, replica target or space whatever their age…
pub const HISTORY_MIN: i64 = 20;
/// …and never more than this many of each (a replica target synced soon after every change makes many)
pub const HISTORY_MAX: i64 = 1000;
/// Jobs deleted per transaction
const TRIM_BATCH: usize = 1000;

/// Deletes finished jobs past the history kept, of backups/, replicas/ and moves/: returns how many
pub async fn trim_histories(st: &AppState) -> AppResult<u64> {
    let t = now();
    let mut n = 0;
    for (table, group) in [("backup_jobs", "set_id"), ("replica_jobs", "policy_id, location_id"), ("space_moves", "drive_id")] {
        n += trim_history(st, table, group, t).await?;
    }
    Ok(n)
}

/// Deletes the finished jobs of `table` past the history kept as of `t`, its jobs grouped by `group`
async fn trim_history(st: &AppState, table: &str, group: &str, t: i64) -> AppResult<u64> {
    let ids: Vec<String> = sqlx::query_as::<_, (String,)>(sqlx::AssertSqlSafe(format!(
        "SELECT id FROM (
           SELECT id, COALESCE(finished_at, created_at) AS ended, ROW_NUMBER() OVER (PARTITION BY {group} ORDER BY created_at DESC, rowid DESC) AS r
           FROM {table} WHERE state IN ('done', 'cancelled'))
         WHERE r > ?1 OR (r > ?2 AND ended < ?3)"
    )))
    .bind(HISTORY_MAX)
    .bind(HISTORY_MIN)
    .bind(t - HISTORY_DAYS * 86400)
    .fetch_all(&st.db)
    .await?
    .into_iter()
    .map(|(id,)| id)
    .collect();
    let mut deleted = 0;
    for chunk in ids.chunks(TRIM_BATCH) {
        let _w = st.write_lock.lock().await;
        deleted += sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table} WHERE id IN (SELECT value FROM json_each(?)) AND state IN ('done', 'cancelled')")))
            .bind(serde_json::to_string(chunk).unwrap())
            .execute(&st.db)
            .await?
            .rows_affected();
    }
    Ok(deleted)
}

/// The progress of a running job, fresher than the database (written every few seconds): files and bytes done and in
/// all, and the speed
pub(crate) fn live(q: &Queue, id: &str) -> Option<(i64, i64, i64, i64, f64)> {
    let running = q.running.lock();
    let ctl = running.get(id)?;
    let p = ctl.progress.lock();
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
