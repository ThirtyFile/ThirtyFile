//! Moving spaces to another storage location (Control panel › Moves).
//!
//! A move is a job in the database (`space_moves`), one per space. A runner starts the queued jobs in the order they
//! were asked for, at most the number set on the Moves page at a time (1 by default: a small server has one CPU, and
//! copying is mostly waiting for disks and networks). Each job:
//!
//! 1. copies the space's content to the new location, checking every copy, and records what it copied
//!    (`space_move_items`): a job that stops (paused, failed, or ThirtyFile restarted) continues where it was;
//! 2. switches the space over in one transaction once everything is copied (whatever changed meanwhile is copied
//!    first);
//! 3. removes the originals, which nothing touched until then.
//!
//! Until the switch the space is where it was, so a failed or cancelled move leaves it as it was; cancelling also
//! removes what was copied. Items that can't be copied are tried a few times, then listed, and the move stops as
//! failed until an administrator resumes or cancels it.
//!
//! A move that copies from or to a folder makes the space read-only meanwhile (`drives.moving`): the folder must stay
//! as it was copied. Changes made in a folder from outside ThirtyFile are found by a scan before the switch.
//!
//! One engine per direction: store.rs moves a space between content stores, to_store.rs a folder space into a content
//! store.

mod between_folders;
mod store;
mod to_folder;
pub(crate) mod to_store;

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::{Path, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{SqliteConnection, SqlitePool};

use crate::{
    auth::Admin,
    error::{AppError, AppResult},
    state::AppState,
    tree::SpaceMode,
    util::{new_id, now},
};

/// Most moves the Moves page lets run at the same time
pub const MAX_JOBS: i64 = 8;
/// States of a move that isn't over: the space is still being moved (or waits to be)
pub(super) const ACTIVE: &str = "('queued', 'running', 'paused', 'failed')";
/// Items listed in a move's failures (all are counted)
const MAX_FAILURES: usize = 100;
/// Tries for each item before it counts as failed
const TRIES: u32 = 3;
/// How often a running move writes its progress to the database
const FLUSH_EVERY: Duration = Duration::from_secs(2);
/// Moves listed on the Moves page
const HISTORY: i64 = 500;

/// The moves running now, and the runner's wake-up call
#[derive(Default)]
pub struct Moves {
    running: Mutex<HashMap<String, Arc<Control>>>,
    wake: tokio::sync::Notify,
}

/// What a running move is asked to do, and how far it got
#[derive(Default)]
pub struct Control {
    pause: AtomicBool,
    cancel: AtomicBool,
    progress: Mutex<Progress>,
}

#[derive(Default)]
struct Progress {
    files_done: i64,
    bytes_done: i64,
    files_total: i64,
    bytes_total: i64,
    failed: i64,
    failures: Vec<Failure>,
    /// When progress was last written to the database, and the bytes done then
    flushed: Option<(Instant, i64)>,
    /// Bytes copied per second, smoothed
    rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Failure {
    /// Which item (its path or name); None in personal spaces, whose content administrators don't see
    item: Option<String>,
    error: String,
}

/// Where a move is (`space_moves.state`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum MoveState {
    /// Waiting for its turn
    Queued,
    Running,
    /// Paused by an administrator
    Paused,
    /// Stopped by an error; it can be resumed
    Failed,
    /// The space is on its new location
    Done,
    Cancelled,
}

/// Why a job's run ended without an error
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// The space is on the new location
    Done,
    Paused,
    Cancelled,
}

/// A move as the database has it
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Job {
    pub id: String,
    pub drive_id: String,
    pub space_name: String,
    pub space_kind: String,
    pub from_location: Option<String>,
    pub from_name: String,
    pub from_mode: SpaceMode,
    pub from_path: Option<String>,
    pub to_location: String,
    pub to_name: String,
    pub to_mode: SpaceMode,
    pub created_by: Option<i64>,
    pub created_by_name: String,
}

const JOB_COLS: &str =
    "id, drive_id, space_name, space_kind, from_location, from_name, from_mode, from_path, to_location, to_name, to_mode, created_by, created_by_name";

async fn job(conn: &mut SqliteConnection, id: &str) -> AppResult<Option<Job>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {JOB_COLS} FROM space_moves WHERE id = ?"))).bind(id).fetch_optional(conn).await?)
}

/// A running job, as its engine sees it: the job, the server, and the job's controls
pub struct Ctx<'a> {
    pub st: &'a AppState,
    pub job: &'a Job,
    ctl: &'a Control,
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

    /// Sets what was done before and what there is in all (at the start of each run)
    pub fn set_counts(&self, files_done: i64, bytes_done: i64, files_total: i64, bytes_total: i64) {
        let mut p = self.ctl.progress.lock().unwrap();
        (p.files_done, p.bytes_done, p.files_total, p.bytes_total) = (files_done, bytes_done, files_total, bytes_total);
        p.flushed = None;
    }

    /// More to copy than counted at the start (changes made meanwhile)
    pub fn add_total(&self, files: i64, bytes: i64) {
        let mut p = self.ctl.progress.lock().unwrap();
        p.files_total += files;
        p.bytes_total += bytes;
    }

    /// An item was copied: counted, and written to the database every few seconds
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

    /// An item that couldn't be copied after a few tries. `item` names it (left out in personal spaces).
    pub fn failed(&self, item: Option<String>, error: String) {
        tracing::warn!("Moving a space: an item couldn't be copied: {error}");
        let item = item.filter(|_| self.job.space_kind != "personal");
        let mut p = self.ctl.progress.lock().unwrap();
        p.failed += 1;
        if p.failures.len() < MAX_FAILURES {
            p.failures.push(Failure { item, error });
        }
    }

    pub fn failed_count(&self) -> i64 {
        self.ctl.progress.lock().unwrap().failed
    }

    /// The progress as it is written to the database: files and bytes done, in all, failed items and the list of them
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
            "UPDATE space_moves SET files_done = ?, bytes_done = ?, files_total = ?, bytes_total = ?, failed_items = ?, failures = ?
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

    /// Waits before trying an item again (`attempt` from 1); None when the job is asked to stop meanwhile
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

    /// Tries `f` up to `TRIES` times while it fails with an error `retry` accepts. Err(Ok(stop)) when the job is
    /// asked to stop in between.
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

/// Content copied to `location` by a move that hasn't ended: the move uses it once the space switches over, or removes
/// it when it is cancelled, so background deletion leaves it alone (tree::claim_for_deletion)
pub async fn copied_for_move(db: &SqlitePool, hash: &str, location: &str) -> Result<bool, sqlx::Error> {
    let row: Option<(i64,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT 1 FROM space_move_items i JOIN space_moves m ON m.id = i.move_id
         WHERE i.hash = ? AND m.to_location = ? AND m.state IN {ACTIVE} LIMIT 1"
    )))
    .bind(hash)
    .bind(location)
    .fetch_optional(db)
    .await?;
    Ok(row.is_some())
}

impl Job {
    /// Whether the space must stay as it is while its files are copied: a folder is copied, or written
    fn locks_space(&self) -> bool {
        self.from_mode == SpaceMode::Folder || self.to_mode == SpaceMode::Folder
    }
}

/// Whether a space is being moved, or waits to be (a move that isn't over)
pub async fn drive_busy(conn: &mut SqliteConnection, drive_id: &str) -> Result<bool, sqlx::Error> {
    let row: Option<(i64,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT 1 FROM space_moves WHERE drive_id = ? AND state IN {ACTIVE} LIMIT 1")))
        .bind(drive_id)
        .fetch_optional(conn)
        .await?;
    Ok(row.is_some())
}

/// Refuses a change to a space that is being moved (deleting it, say)
pub async fn refuse_busy(conn: &mut SqliteConnection, drive_id: &str) -> AppResult<()> {
    if drive_busy(conn, drive_id).await? {
        return Err(AppError::conflict("This space is being moved to another storage location. Wait until the move finishes, or cancel it."));
    }
    Ok(())
}

/// Whether a move that isn't over goes from or to this location
pub async fn location_busy(conn: &mut SqliteConnection, location: &str) -> Result<bool, sqlx::Error> {
    let row: Option<(i64,)> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT 1 FROM space_moves WHERE (from_location = ?1 OR to_location = ?1) AND state IN {ACTIVE} LIMIT 1")))
            .bind(location)
            .fetch_optional(conn)
            .await?;
    Ok(row.is_some())
}

/// How a space keeps its files once it is on `location`, as a space created there would: a folder on the built-in
/// storage and on Local folder locations, else the content store
async fn mode_on(st: &AppState, conn: &mut SqliteConnection, location: &str) -> AppResult<SpaceMode> {
    let (kind,): (String,) = sqlx::query_as("SELECT kind FROM storage_locations WHERE id = ?")
        .bind(location)
        .fetch_optional(conn)
        .await?
        .ok_or_else(|| AppError::not_found("Storage location not found"))?;
    Ok(if kind == "local" && st.space_folders.is_some() { SpaceMode::Folder } else { SpaceMode::Store })
}

/// Writes an activity log entry about a move, as done by whoever asked for it
async fn log(conn: &mut SqliteConnection, job: &Job, action: &str, detail: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO activity (at, user_id, username, drive_id, node_id, node_name, action, detail)
         SELECT ?, ?, ?, d.id, d.root_id, COALESCE((SELECT name FROM nodes WHERE id = d.root_id), ''), ?, ? FROM drives d WHERE d.id = ?",
    )
    .bind(now())
    .bind(job.created_by)
    .bind(&job.created_by_name)
    .bind(action)
    .bind(detail)
    .bind(&job.drive_id)
    .execute(conn)
    .await?;
    Ok(())
}

/// "Local disk → S3"
fn route(job: &Job) -> String {
    format!("{} → {}", if job.from_name.is_empty() { "—" } else { &job.from_name }, job.to_name)
}

// ───────────── Runner ─────────────

/// Starts the runner: moves that were running when ThirtyFile stopped continue, then queued moves start in turn
pub fn spawn_runner(st: AppState) {
    tokio::spawn(async move {
        if let Err(e) = recover(&st).await {
            tracing::warn!("Couldn't continue the moves of spaces that were running: {}", e.message);
        }
        loop {
            if let Err(e) = start_due(&st).await {
                tracing::warn!("Couldn't start moving a space: {}", e.message);
            }
            tokio::select! {
                _ = st.moves.wake.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(30)) => {}
            }
        }
    });
}

/// After a restart: moves that were running wait for their turn again (ahead of newer ones, as they are older), and
/// cleanups that didn't finish are done (one that fails is tried again at the next start)
async fn recover(st: &AppState) -> AppResult<()> {
    {
        let _w = st.write_lock.lock().await;
        sqlx::query("UPDATE space_moves SET state = 'queued' WHERE state = 'running'").execute(&st.db).await?;
    }
    let unfinished: Vec<(String, MoveState)> = sqlx::query_as(
        "SELECT id, state FROM space_moves m
         WHERE state IN ('cancelled', 'done') AND (EXISTS (SELECT 1 FROM space_move_items i WHERE i.move_id = m.id) OR renamed = 1)",
    )
    .fetch_all(&st.db)
    .await?;
    for (id, state) in unfinished {
        if let Some(job) = job(&mut *st.db.acquire().await?, &id).await? {
            if state == MoveState::Done {
                // Switched over, but the old folder wasn't removed yet
                clean_up(st, &job).await;
            } else if let Err(e) = remove_copies(st, &job).await {
                tracing::warn!("Couldn't remove what a cancelled move of the space {} copied: {}", job.space_name, e.message);
            }
        }
    }
    Ok(())
}

/// After the switch: removes the old folder. The move is done whatever happens here; what isn't removed now is tried
/// again at the next start (`recover`).
async fn clean_up(st: &AppState, job: &Job) {
    if let Err(e) = to_store::cleanup(st, job).await {
        tracing::warn!("Couldn't remove the old folder of the space {} after moving it: {}", job.space_name, e.message);
    }
}

/// Moves recorded as running that no task runs (recording how one ended failed, on a full disk say, or it panicked):
/// they wait for their turn again, as after a restart, rather than stay running with pause, resume and cancel refused
/// and the space read-only
async fn requeue_orphans(st: &AppState) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    // Read with the write lock held: a task takes its place in the list before its move is marked running (`take`),
    // and leaves it only after its last write (`Running`)
    let running: Vec<String> = st.moves.running.lock().unwrap().keys().cloned().collect();
    let orphans = sqlx::query("UPDATE space_moves SET state = 'queued' WHERE state = 'running' AND id NOT IN (SELECT value FROM json_each(?))")
        .bind(serde_json::to_string(&running).unwrap())
        .execute(&st.db)
        .await?
        .rows_affected();
    if orphans > 0 {
        tracing::warn!("{orphans} moves of spaces were left running without anything running them: they continue in turn");
    }
    Ok(())
}

/// Starts queued moves while fewer than the setting are running
async fn start_due(st: &AppState) -> AppResult<()> {
    requeue_orphans(st).await?;
    loop {
        let limit = st.system.read().unwrap().move_jobs.clamp(1, MAX_JOBS) as usize;
        let busy: Vec<String> = st.moves.running.lock().unwrap().keys().cloned().collect();
        if busy.len() >= limit {
            return Ok(());
        }
        let next: Option<Job> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {JOB_COLS} FROM space_moves WHERE state = 'queued' AND id NOT IN (SELECT value FROM json_each(?))
             ORDER BY created_at, rowid LIMIT 1"
        )))
        .bind(serde_json::to_string(&busy).unwrap())
        .fetch_optional(&st.db)
        .await?;
        let Some(job) = next else { return Ok(()) };
        let Some(ctl) = take(st, &job).await? else { continue };
        let st = st.clone();
        // Counted apart from what people do (Storage usage)
        tokio::spawn(crate::usage::background(async move { run(&st, &job, &ctl).await }));
    }
}

/// Marks a queued job as running, with its controls in place first (a pause asked for right away finds them); None
/// when it was paused or cancelled meanwhile
async fn take(st: &AppState, job: &Job) -> AppResult<Option<Arc<Control>>> {
    let ctl = Arc::new(Control::default());
    st.moves.running.lock().unwrap().insert(job.id.clone(), ctl.clone());
    let taken = async {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            let taken =
                sqlx::query("UPDATE space_moves SET state = 'running', started_at = COALESCE(started_at, ?), error = NULL WHERE id = ? AND state = 'queued'")
                    .bind(now())
                    .bind(&job.id)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected()
                    == 1;
            // Read-only from the first start until the move is over
            if taken && job.locks_space() {
                sqlx::query("UPDATE drives SET moving = 1 WHERE id = ?").bind(&job.drive_id).execute(&mut *tx).await?;
            }
            AppResult::Ok(taken)
        }
        .await;
        crate::db::settle(tx, res).await
    }
    .await;
    if !matches!(taken, Ok(true)) {
        st.moves.running.lock().unwrap().remove(&job.id);
    }
    Ok(taken?.then_some(ctl))
}

/// Takes a job off the running list when its run ends, however it ends
struct Running<'a>(&'a AppState, String);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.moves.running.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.1);
        self.0.moves.wake.notify_one();
    }
}

/// Runs a job that was just marked as running, and records how it ended
async fn run(st: &AppState, job: &Job, ctl: &Control) {
    let _running = Running(st, job.id.clone());
    let cx = Ctx { st, job, ctl };
    let res = match prepare(&cx).await {
        Ok(()) => match (job.from_mode, job.to_mode) {
            (SpaceMode::Store, SpaceMode::Store) => store::run(&cx).await,
            (SpaceMode::Folder, SpaceMode::Store) => to_store::run(&cx).await,
            (SpaceMode::Store, SpaceMode::Folder) => to_folder::run(&cx).await,
            (SpaceMode::Folder, SpaceMode::Folder) => between_folders::run(&cx).await,
        },
        Err(e) => Err(e),
    };
    let ended = match res {
        Ok(Stop::Done) => Ok(()),
        Ok(Stop::Paused) => set_state(&cx, MoveState::Paused, None).await,
        Ok(Stop::Cancelled) => cancelled(st, job).await,
        Err(e) => {
            tracing::warn!("Moving the space {} failed: {}", job.space_name, e.message);
            set_state(&cx, MoveState::Failed, Some(&e.message)).await
        }
    };
    if let Err(e) = ended {
        tracing::warn!("Couldn't record how moving the space {} ended: {}", job.space_name, e.message);
    }
}

/// Before copying: the target can be reached, and a disk of this server has room for what is left to copy. A folder
/// moving to a folder is checked once it is clear that it can't simply be renamed (between_folders.rs).
async fn prepare(cx: &Ctx<'_>) -> AppResult<()> {
    crate::locations::probe(cx.st, &cx.job.to_location).await.map_err(|e| AppError::bad_request(format!("The target storage location can't be reached: {e}")))?;
    if !(cx.job.from_mode == SpaceMode::Folder && cx.job.to_mode == SpaceMode::Folder) {
        check_room(cx.st, &cx.job.to_location, left_to_copy(cx.st, cx.job).await?).await?;
    }
    if cx.job.from_mode == SpaceMode::Folder {
        // The location holding the folder (a disk that may not be mounted)
        if let Some(from) = &cx.job.from_location {
            crate::locations::probe(cx.st, from).await.map_err(|e| AppError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, e))?;
        }
    }
    if cx.job.from_mode == SpaceMode::Folder && cx.job.to_mode == SpaceMode::Store {
        // The folder itself (into a folder, it may be in its new place already: between_folders.rs looks)
        to_store::source(cx.job)?;
        // Each file goes through a temp file in the data folder: the largest must fit
        let (largest,): (i64,) =
            sqlx::query_as("SELECT COALESCE(MAX(size), 0) FROM nodes WHERE drive_id = ? AND kind = 'file'").bind(&cx.job.drive_id).fetch_one(&cx.st.db).await?;
        let free = free_space(cx.st.tmp_dir()).await;
        if let Some(free) = free.filter(|f| (*f as i64) < largest) {
            return Err(AppError::bad_request(format!(
                "There isn't enough free space in ThirtyFile's data folder for the largest file: {needed} is needed, {free} is free",
                needed = crate::util::format_bytes_u64(largest.max(0) as u64),
                free = crate::util::format_bytes_u64(free)
            )));
        }
    }
    Ok(())
}

/// What a move has left to copy: the space's files and earlier versions, less what it copied already (bytes)
pub(super) async fn left_to_copy(st: &AppState, job: &Job) -> AppResult<i64> {
    let (left,): (i64,) = sqlx::query_as(
        "SELECT d.used_bytes + COALESCE((SELECT SUM(v.size) FROM node_versions v JOIN nodes n ON n.id = v.node_id WHERE n.drive_id = d.id), 0)
                - COALESCE((SELECT SUM(size) FROM space_move_items WHERE move_id = ?2 AND done = 1 AND kind != 'folder'), 0)
         FROM drives d WHERE d.id = ?1",
    )
    .bind(&job.drive_id)
    .bind(&job.id)
    .fetch_optional(&st.db)
    .await?
    .ok_or_else(|| AppError::not_found("Space not found"))?;
    Ok(left.max(0))
}

#[cfg(test)]
thread_local! {
    /// Tests: the free space every disk has (None: what the disk says)
    pub static FREE_SPACE: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Free space on the disk holding `path`, when it can be told
async fn free_space(path: std::path::PathBuf) -> Option<u64> {
    #[cfg(test)]
    if let Some(free) = FREE_SPACE.with(|f| f.get()) {
        return Some(free);
    }
    crate::util::disk_space_soon(&path).await.map(|(free, _)| free)
}

/// On a disk of this server (the built-in storage, a Local folder location): refuses when the disk has less free space
/// than `bytes`
pub(super) async fn check_room(st: &AppState, location: &str, bytes: i64) -> AppResult<()> {
    let row: Option<(String, String, String)> =
        sqlx::query_as("SELECT kind, config, name FROM storage_locations WHERE id = ?").bind(location).fetch_optional(&st.db).await?;
    let Some((kind, config, name)) = row else { return Err(AppError::not_found("Storage location not found")) };
    if kind != "local" || bytes <= 0 {
        return Ok(());
    }
    let Ok(root) = crate::storage::local_root(location, &crate::locations::config_json(location, &config), &st.storage_dir) else { return Ok(()) };
    let free = free_space(root).await;
    match free {
        Some(free) if (free as i64) < bytes => Err(AppError::bad_request(format!(
            "There isn't enough free space on {name}: {needed} is needed, {free} is free",
            needed = crate::util::format_bytes_u64(bytes.max(0) as u64),
            free = crate::util::format_bytes_u64(free)
        ))),
        _ => Ok(()),
    }
}

/// Records that a run stopped (paused, or failed with `error`), with its progress. A move that switched the space over
/// is done, and stays done: resuming or cancelling it would take the files it moved for copies.
async fn set_state(cx: &Ctx<'_>, state: MoveState, error: Option<&str>) -> AppResult<()> {
    let _w = cx.st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&cx.st.db).await?;
    let res = async {
        let (files_done, bytes_done, files_total, bytes_total, failed, failures) = cx.progress();
        let stopped = sqlx::query(
            "UPDATE space_moves SET state = ?, error = ?, files_done = ?, bytes_done = ?, files_total = ?, bytes_total = ?, failed_items = ?,
                                    failures = ?
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
        if !stopped {
            return Ok(());
        }
        if let Some(error) = error {
            log(&mut tx, cx.job, "move_failed", &format!("{}: {error}", route(cx.job))).await?;
        }
        AppResult::Ok(())
    }
    .await;
    crate::db::settle(tx, res).await
}

/// A job asked to be cancelled while it ran: it ends as cancelled, and what it copied goes
async fn cancelled(st: &AppState, job: &Job) -> AppResult<()> {
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = mark_cancelled(&mut tx, job).await;
        crate::db::settle(tx, res).await?;
    }
    remove_copies(st, job).await
}

async fn mark_cancelled(conn: &mut SqliteConnection, job: &Job) -> AppResult<()> {
    sqlx::query("UPDATE space_moves SET state = 'cancelled', finished_at = ?, error = NULL WHERE id = ?").bind(now()).bind(&job.id).execute(&mut *conn).await?;
    sqlx::query("UPDATE drives SET moving = 0 WHERE id = ?").bind(&job.drive_id).execute(&mut *conn).await?;
    log(conn, job, "move_cancel", &route(job)).await?;
    Ok(())
}

/// In the transaction that switches the space over: the move is done, and the space can be changed again. Its record
/// of copies goes, unless `cleanup` still needs it (to remove the originals: to_store::cleanup). `note`: what the switch
/// changed that people should know.
async fn finish(conn: &mut SqliteConnection, cx: &Ctx<'_>, cleanup: bool, note: Option<&str>) -> AppResult<()> {
    let (files, bytes) = {
        let p = cx.ctl.progress.lock().unwrap();
        (p.files_done, p.bytes_done)
    };
    sqlx::query(
        "UPDATE space_moves SET state = 'done', finished_at = ?4, files_done = ?1, bytes_done = ?2, files_total = ?1, bytes_total = ?2,
                                failed_items = 0, failures = '[]', error = NULL, note = ?5
         WHERE id = ?3",
    )
    .bind(files)
    .bind(bytes)
    .bind(&cx.job.id)
    .bind(now())
    .bind(note)
    .execute(&mut *conn)
    .await?;
    sqlx::query("UPDATE drives SET moving = 0 WHERE id = ?").bind(&cx.job.drive_id).execute(&mut *conn).await?;
    if !cleanup {
        sqlx::query("DELETE FROM space_move_items WHERE move_id = ?").bind(&cx.job.id).execute(&mut *conn).await?;
    }
    log(conn, cx.job, "move_done", &route(cx.job)).await?;
    Ok(())
}

/// Removes what a cancelled job copied (its state is already `cancelled`, so nothing protects the copies any more)
async fn remove_copies(st: &AppState, job: &Job) -> AppResult<()> {
    match job.to_mode {
        SpaceMode::Store => store::remove_copies(st, job).await,
        SpaceMode::Folder => to_folder::remove_copies(st, job).await,
    }
}

// ───────────── API ─────────────

#[derive(Serialize, sqlx::FromRow)]
pub struct MoveInfo {
    id: String,
    drive_id: String,
    space_name: String,
    space_kind: String,
    owner_name: String,
    from_location: Option<String>,
    from_name: String,
    from_mode: SpaceMode,
    to_location: String,
    to_name: String,
    to_mode: SpaceMode,
    state: MoveState,
    files_total: i64,
    bytes_total: i64,
    files_done: i64,
    bytes_done: i64,
    failed_items: i64,
    #[serde(skip)]
    failures: String,
    error: Option<String>,
    note: Option<String>,
    created_by_name: String,
    created_at: i64,
    started_at: Option<i64>,
    finished_at: Option<i64>,
    /// Running moves: bytes copied per second lately
    #[sqlx(skip)]
    speed: Option<f64>,
    #[sqlx(skip)]
    #[serde(rename = "failures")]
    failure_list: Vec<Failure>,
}

#[derive(Serialize)]
pub struct MovesList {
    moves: Vec<MoveInfo>,
    /// Moves that run at the same time
    concurrency: i64,
}

/// Moves not over yet first, then the history, newest first
pub async fn list(State(st): State<AppState>, _: Admin) -> AppResult<Json<MovesList>> {
    let mut moves: Vec<MoveInfo> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT id, drive_id, space_name, space_kind, owner_name, from_location, from_name, from_mode, to_location, to_name, to_mode, state,
                files_total, bytes_total, files_done, bytes_done, failed_items, failures, error, note, created_by_name, created_at, started_at,
                finished_at
         FROM space_moves ORDER BY state IN {ACTIVE} DESC, created_at DESC, rowid DESC LIMIT {HISTORY}"
    )))
    .fetch_all(&st.db)
    .await?;
    let running = st.moves.running.lock().unwrap();
    for m in &mut moves {
        m.failure_list = serde_json::from_str(&m.failures).unwrap_or_default();
        if let Some(ctl) = running.get(&m.id) {
            let p = ctl.progress.lock().unwrap();
            m.speed = Some(p.rate);
            // Fresher than the database, which is written every few seconds
            (m.files_done, m.bytes_done, m.files_total, m.bytes_total) = (p.files_done, p.bytes_done, p.files_total, p.bytes_total);
        }
    }
    drop(running);
    Ok(Json(MovesList { moves, concurrency: st.system.read().unwrap().move_jobs }))
}

#[derive(Deserialize)]
pub struct CreateReq {
    drive_ids: Vec<String>,
    location_id: String,
}

/// Queues a move of each space to the location, in the order given (they run one after the other, or a few at a time);
/// returns the moves' ids. Nothing is queued when one of them can't be moved.
pub async fn create(State(st): State<AppState>, Admin(user): Admin, Json(mut req): Json<CreateReq>) -> AppResult<Json<Value>> {
    let mut seen = std::collections::HashSet::new();
    req.drive_ids.retain(|id| seen.insert(id.clone()));
    if req.drive_ids.is_empty() {
        return Err(AppError::bad_request("Choose the spaces to move"));
    }
    let target = req.location_id;
    let (to_name,): (String,) = sqlx::query_as("SELECT name FROM storage_locations WHERE id = ?")
        .bind(&target)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Storage location not found"))?;
    crate::locations::probe(&st, &target).await.map_err(|e| AppError::bad_request(format!("The target storage location can't be reached: {e}")))?;
    let to_mode = mode_on(&st, &mut *st.db.acquire().await?, &target).await?;
    // The whole size of the spaces, versions included, must fit on a disk of this server
    let (bytes,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(d.used_bytes), 0) + (SELECT COALESCE(SUM(v.size), 0) FROM node_versions v JOIN nodes n ON n.id = v.node_id
                                                  WHERE n.drive_id IN (SELECT value FROM json_each(?1)))
         FROM drives d WHERE d.id IN (SELECT value FROM json_each(?1))",
    )
    .bind(serde_json::to_string(&req.drive_ids).unwrap())
    .fetch_one(&st.db)
    .await?;
    check_room(&st, &target, bytes).await?;
    // The folder of a folder space must be there: a disk that isn't mounted mustn't look like an empty space. Looked
    // at before the write lock is taken, on a blocking thread that is given up should the disk not answer.
    for drive_id in &req.drive_ids {
        crate::fsops::ready(&st, drive_id).await?;
    }

    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        let mut ids = Vec::new();
        for drive_id in &req.drive_ids {
            ids.push(queue(&mut tx, &user, drive_id, &target, &to_name, to_mode).await?);
        }
        AppResult::Ok(ids)
    }
    .await;
    let ids = crate::db::settle(tx, res).await?;
    st.moves.wake.notify_one();
    Ok(Json(json!({ "ids": ids })))
}

/// Adds one move to the queue
async fn queue(conn: &mut SqliteConnection, user: &crate::auth::User, drive_id: &str, target: &str, to_name: &str, to_mode: SpaceMode) -> AppResult<String> {
    #[derive(sqlx::FromRow)]
    struct Space {
        name: String,
        kind: String,
        owner_name: String,
        location_id: Option<String>,
        location_name: String,
        mode: SpaceMode,
        source_path: Option<String>,
    }
    let space: Space = sqlx::query_as(
        "SELECT d.name, d.kind, COALESCE(u.username, '') AS owner_name, d.location_id, COALESCE(l.name, '') AS location_name, d.mode,
                d.source_path
         FROM drives d LEFT JOIN users u ON u.id = d.owner_id AND d.kind = 'personal' LEFT JOIN storage_locations l ON l.id = d.location_id
         WHERE d.id = ?",
    )
    .bind(drive_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| AppError::not_found("Space not found"))?;
    let shown = if space.kind == "personal" && !space.owner_name.is_empty() { format!("{} · {}", space.name, space.owner_name) } else { space.name.clone() };
    if drive_busy(conn, drive_id).await? {
        return Err(AppError::conflict(format!("\"{shown}\" is already being moved")));
    }
    // Its folder was found there before the write lock was taken (`create`)
    let from_path = space.source_path.clone().filter(|_| space.mode == SpaceMode::Folder);
    // Already there: only content still kept elsewhere (from an earlier move, say) is gathered there
    if space.location_id.as_deref() == Some(target) && space.mode == to_mode && !store::scattered(conn, drive_id, target).await? {
        return Err(AppError::bad_request(format!("\"{shown}\" is already on this location")));
    }
    let id = new_id();
    sqlx::query(
        "INSERT INTO space_moves (id, drive_id, space_name, space_kind, owner_name, from_location, from_name, from_mode, from_path, to_location,
                                  to_name, to_mode, created_by, created_by_name, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(drive_id)
    .bind(&space.name)
    .bind(&space.kind)
    .bind(&space.owner_name)
    .bind(&space.location_id)
    .bind(&space.location_name)
    .bind(space.mode)
    .bind(&from_path)
    .bind(target)
    .bind(to_name)
    .bind(to_mode)
    .bind(user.id)
    .bind(&user.username)
    .bind(now())
    .execute(&mut *conn)
    .await?;
    let job = job(conn, &id).await?.ok_or_else(|| AppError::internal("the move just added is gone"))?;
    log(conn, &job, "move_start", &route(&job)).await?;
    Ok(id)
}

/// A move's job, for the actions below
async fn job_for(st: &AppState, id: &str) -> AppResult<(Job, String)> {
    let mut c = st.db.acquire().await?;
    let job = job(&mut c, id).await?.ok_or_else(|| AppError::not_found("This move no longer exists"))?;
    let (state,): (String,) = sqlx::query_as("SELECT state FROM space_moves WHERE id = ?").bind(id).fetch_one(&mut *c).await?;
    Ok((job, state))
}

/// Pauses a move: a running one stops after the item it is copying, keeping what it copied
pub async fn pause(State(st): State<AppState>, _: Admin, Path(id): Path<String>) -> AppResult<Json<Value>> {
    if let Some(ctl) = st.moves.running.lock().unwrap().get(&id) {
        ctl.pause.store(true, Ordering::SeqCst);
        return Ok(Json(json!({ "ok": true })));
    }
    let _w = st.write_lock.lock().await;
    let changed = sqlx::query("UPDATE space_moves SET state = 'paused' WHERE id = ? AND state = 'queued'").bind(&id).execute(&st.db).await?.rows_affected();
    if changed == 0 {
        return Err(AppError::conflict("This move can't be paused now"));
    }
    Ok(Json(json!({ "ok": true })))
}

/// Resumes a paused or failed move: it waits for its turn, then continues where it stopped
pub async fn resume(State(st): State<AppState>, _: Admin, Path(id): Path<String>) -> AppResult<Json<Value>> {
    {
        let _w = st.write_lock.lock().await;
        let changed = sqlx::query("UPDATE space_moves SET state = 'queued', error = NULL WHERE id = ? AND state IN ('paused', 'failed')")
            .bind(&id)
            .execute(&st.db)
            .await?
            .rows_affected();
        if changed == 0 {
            return Err(AppError::conflict("This move can't be resumed now"));
        }
    }
    st.moves.wake.notify_one();
    Ok(Json(json!({ "ok": true })))
}

/// Cancels a move: the space stays where it was, and what was copied is removed
pub async fn cancel(State(st): State<AppState>, _: Admin, Path(id): Path<String>) -> AppResult<Json<Value>> {
    if let Some(ctl) = st.moves.running.lock().unwrap().get(&id) {
        ctl.cancel.store(true, Ordering::SeqCst);
        return Ok(Json(json!({ "ok": true })));
    }
    let (job, _) = job_for(&st, &id).await?;
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            let (state,): (MoveState,) = sqlx::query_as("SELECT state FROM space_moves WHERE id = ?").bind(&id).fetch_one(&mut *tx).await?;
            if !matches!(state, MoveState::Queued | MoveState::Paused | MoveState::Failed) {
                return Err(AppError::conflict("This move can't be cancelled now"));
            }
            mark_cancelled(&mut tx, &job).await
        }
        .await;
        crate::db::settle(tx, res).await?;
    }
    remove_copies(&st, &job).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct SettingsReq {
    /// Moves that run at the same time
    concurrency: i64,
}

pub async fn update_settings(State(st): State<AppState>, Admin(user): Admin, Json(req): Json<SettingsReq>) -> AppResult<Json<Value>> {
    if !(1..=MAX_JOBS).contains(&req.concurrency) {
        return Err(AppError::bad_request(format!("Enter a number from 1 to {MAX_JOBS}")));
    }
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            crate::db::set_setting(&mut tx, "move_jobs", &req.concurrency.to_string()).await?;
            crate::logs::record_activity(&mut tx, &user, None, "settings", &format!("Moves at the same time: {}", req.concurrency)).await?;
            AppResult::Ok(())
        }
        .await;
        crate::db::settle(tx, res).await?;
    }
    st.system.write().unwrap().move_jobs = req.concurrency;
    st.moves.wake.notify_one();
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests;
