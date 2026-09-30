//! The jobs of backups/ (`backup_jobs`): a runner starts queued jobs one at a time, in the order they were asked for,
//! and records how each ended. A job that stops (paused, failed, or ThirtyFile restarted) continues where it was, from
//! what it recorded. Modelled on moves/, whose jobs switch spaces over and remove originals: these never do either.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use sqlx::SqliteConnection;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
    util::now,
};

/// States of a job that isn't over
pub const ACTIVE: &str = "('queued', 'running', 'paused', 'waiting', 'failed')";
/// Items listed in a job's failures (all are counted)
const MAX_FAILURES: usize = 100;
/// Tries for each item before it counts as failed
const TRIES: u32 = 3;
/// How often a running job writes its progress to the database
const FLUSH_EVERY: Duration = Duration::from_secs(2);

/// The jobs running now, and the runner's wake-up call
#[derive(Default)]
pub struct Backups {
    pub(super) running: Mutex<HashMap<String, Arc<Control>>>,
    pub(super) wake: tokio::sync::Notify,
}

/// What a running job is asked to do, and how far it got
#[derive(Default)]
pub struct Control {
    pub(super) pause: AtomicBool,
    pub(super) cancel: AtomicBool,
    pub(super) progress: Mutex<Progress>,
}

#[derive(Default)]
pub(super) struct Progress {
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

const JOB_COLS: &str = "id, kind, set_id, snapshot_id, params, label, created_by, created_by_name";

pub async fn job(conn: &mut SqliteConnection, id: &str) -> AppResult<Option<Job>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {JOB_COLS} FROM backup_jobs WHERE id = ?"))).bind(id).fetch_optional(conn).await?)
}

/// A running job, as its engine sees it
pub struct Ctx<'a> {
    pub st: &'a AppState,
    pub job: &'a Job,
    pub(super) ctl: &'a Control,
    /// Personal spaces' items aren't named in failures
    pub private: std::collections::HashSet<String>,
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
        Ok(())
    }

    /// An item that couldn't be done after a few tries. `item` names it; `space` is the space it is in (items of
    /// personal spaces aren't named).
    pub fn failed(&self, space: &str, item: Option<String>, error: String) {
        tracing::warn!("Backups: an item couldn't be copied: {error}");
        let item = item.filter(|_| !self.private.contains(space));
        let mut p = self.ctl.progress.lock().unwrap();
        p.failed += 1;
        if p.failures.len() < MAX_FAILURES {
            p.failures.push(Failure { item, error });
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
        sqlx::query(
            "UPDATE backup_jobs SET files_done = ?, bytes_done = ?, files_total = ?, bytes_total = ?, failed_items = ?, failures = ?
             WHERE id = ? AND state = 'running'",
        )
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

/// Starts the runner: jobs that were running when ThirtyFile stopped continue, then queued jobs start in turn
pub fn spawn_runner(st: AppState) {
    tokio::spawn(async move {
        if let Err(e) = recover(&st).await {
            tracing::warn!("Couldn't continue the backup jobs that were running: {}", e.message);
        }
        loop {
            if let Err(e) = start_due(&st).await {
                tracing::warn!("Couldn't start a backup job: {}", e.message);
            }
            tokio::select! {
                _ = st.backups.wake.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(30)) => {}
            }
        }
    });
}

/// After a restart: jobs that were running wait for their turn again, ahead of newer ones
pub(super) async fn recover(st: &AppState) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    sqlx::query("UPDATE backup_jobs SET state = 'queued' WHERE state = 'running'").execute(&st.db).await?;
    Ok(())
}

/// Jobs recorded as running that no task runs (recording how one ended failed, say): they wait for their turn again
async fn requeue_orphans(st: &AppState) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    let running: Vec<String> = st.backups.running.lock().unwrap().keys().cloned().collect();
    sqlx::query("UPDATE backup_jobs SET state = 'queued' WHERE state = 'running' AND id NOT IN (SELECT value FROM json_each(?))")
        .bind(serde_json::to_string(&running).unwrap())
        .execute(&st.db)
        .await?;
    Ok(())
}

/// Starts the oldest queued job when none is running: one at a time, so copying never takes more than one transfer
/// from what people do
pub(super) async fn start_due(st: &AppState) -> AppResult<()> {
    requeue_orphans(st).await?;
    if !st.backups.running.lock().unwrap().is_empty() {
        return Ok(());
    }
    let next: Option<Job> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {JOB_COLS} FROM backup_jobs WHERE state = 'queued' ORDER BY created_at, rowid LIMIT 1"
    )))
    .fetch_optional(&st.db)
    .await?;
    let Some(job) = next else { return Ok(()) };
    let Some(ctl) = take(st, &job).await? else { return Ok(()) };
    let st = st.clone();
    tokio::spawn(crate::usage::background(async move { run(&st, &job, &ctl).await }));
    Ok(())
}

/// Marks a queued job as running, with its controls in place first; None when it was paused or cancelled meanwhile
pub(super) async fn take(st: &AppState, job: &Job) -> AppResult<Option<Arc<Control>>> {
    let ctl = Arc::new(Control::default());
    st.backups.running.lock().unwrap().insert(job.id.clone(), ctl.clone());
    let taken = async {
        let _w = st.write_lock.lock().await;
        let n = sqlx::query("UPDATE backup_jobs SET state = 'running', started_at = COALESCE(started_at, ?), error = NULL WHERE id = ? AND state = 'queued'")
            .bind(now())
            .bind(&job.id)
            .execute(&st.db)
            .await?
            .rows_affected();
        AppResult::Ok(n == 1)
    }
    .await;
    if !matches!(taken, Ok(true)) {
        st.backups.running.lock().unwrap().remove(&job.id);
    }
    Ok(taken?.then_some(ctl))
}

/// Takes a job off the running list when its run ends, however it ends
struct Running<'a>(&'a AppState, String);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.backups.running.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.1);
        self.0.backups.wake.notify_one();
    }
}

/// The personal spaces among the spaces a job works on: their items aren't named in its failures
async fn private_spaces(st: &AppState, job: &Job) -> AppResult<std::collections::HashSet<String>> {
    let params: serde_json::Value = serde_json::from_str(&job.params).unwrap_or_default();
    let mut ids: Vec<String> = params["spaces"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
    ids.extend(params["space"].as_str().map(str::to_string));
    ids.extend(params["target_drive"].as_str().map(str::to_string));
    let mut private: std::collections::HashSet<String> = sqlx::query_as::<_, (String,)>(
        "SELECT id FROM drives WHERE kind = 'personal' AND id IN (SELECT value FROM json_each(?))",
    )
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

/// Runs a job that was just marked as running, and records how it ended
pub(super) async fn run(st: &AppState, job: &Job, ctl: &Control) {
    let _running = Running(st, job.id.clone());
    let private = match private_spaces(st, job).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Backups: couldn't read a job's spaces: {}", e.message);
            Default::default()
        }
    };
    let cx = Ctx { st, job, ctl, private };
    let res = match job.kind.as_str() {
        "snapshot" => super::capture::run(&cx).await,
        "restore" => super::restore::run(&cx).await,
        "verify" => super::tidy::verify(&cx).await,
        "remove" => super::tidy::remove(&cx).await,
        _ => Err(AppError::bad_request("Unknown job")),
    };
    let ended = match res {
        Ok(Stop::Done) => Ok(()),
        Ok(Stop::Paused) => set_state(&cx, "paused", None).await,
        Ok(Stop::Cancelled) => super::cancelled(st, job).await,
        Err(e) => {
            tracing::warn!("A backup job ({}) failed: {}", job.kind, e.message);
            set_state(&cx, "failed", Some(&e.message)).await
        }
    };
    if let Err(e) = ended {
        tracing::warn!("Couldn't record how a backup job ended: {}", e.message);
    }
}

/// Records that a run stopped (paused, or failed with `error`), with its progress
async fn set_state(cx: &Ctx<'_>, state: &str, error: Option<&str>) -> AppResult<()> {
    let (files_done, bytes_done, files_total, bytes_total, failed, failures) = cx.progress();
    let _w = cx.st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&cx.st.db).await?;
    let res = async {
        let stopped = sqlx::query(
            "UPDATE backup_jobs SET state = ?, error = ?, files_done = ?, bytes_done = ?, files_total = ?, bytes_total = ?, failed_items = ?, failures = ?
             WHERE id = ? AND state = 'running'",
        )
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
            super::log(&mut tx, cx.job, "backup_failed", &format!("{}: {error}", cx.job.label)).await?;
        }
        AppResult::Ok(())
    }
    .await;
    crate::db::settle(tx, res).await
}

/// In the transaction that ends a job well: it is done, with its progress and `note`
pub(super) async fn finish(conn: &mut SqliteConnection, cx: &Ctx<'_>, note: Option<&str>) -> AppResult<()> {
    let (files, bytes, failed, failures) = {
        let p = cx.ctl.progress.lock().unwrap();
        (p.files_done, p.bytes_done, p.failed, serde_json::to_string(&p.failures).unwrap())
    };
    sqlx::query(
        "UPDATE backup_jobs SET state = 'done', finished_at = ?4, files_done = ?1, bytes_done = ?2, files_total = ?1, bytes_total = ?2,
                                failed_items = ?5, failures = ?6, error = NULL, note = ?7
         WHERE id = ?3",
    )
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

/// The progress of running jobs, fresher than the database (written every few seconds): files and bytes done and in
/// all, and the speed
pub(super) fn live(st: &AppState, id: &str) -> Option<(i64, i64, i64, i64, f64)> {
    let running = st.backups.running.lock().unwrap();
    let ctl = running.get(id)?;
    let p = ctl.progress.lock().unwrap();
    Some((p.files_done, p.bytes_done, p.files_total, p.bytes_total, p.rate))
}
