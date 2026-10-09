//! Tasks that go on after the request that asked for them has answered: compressing and extracting (archive.rs),
//! moving and copying to or from folder spaces, deleting for good and emptying the trash (nodes/), checking a folder
//! space for changes (drives.rs), and removing a user or their "My files" (admin/, personal/).
//!
//! A request has a time limit, and a browser or WebDAV client can give up on it at any moment; work cut off in the
//! middle would leave things half done. So the work runs as a job of its own: the request waits a moment for it (most
//! changes are done by then, and answer as before, errors included), then answers with the job, which the page
//! follows (`GET /api/jobs/{id}`) until it is done.
//!
//! Jobs are kept in memory: a restart forgets them (what a job leaves unfinished then is dealt with as after any stop:
//! see fsops/ and tree/changes.rs).

use std::time::Duration;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Serialize;
use serde_json::Value;

use crate::{
    auth::User,
    error::{AppError, AppResult},
    state::AppState,
    util::{new_id, now},
};

/// How long a request waits for its job before answering with the job instead: long enough for ordinary changes to
/// answer as before, short enough that the page can show the progress of a long one
pub const WAIT: Duration = Duration::from_secs(2);

#[cfg(test)]
thread_local! {
    /// Tests: how long requests wait for their jobs
    static TEST_WAIT: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

/// How long a request waits for its job (`WAIT`; in tests long enough for a busy machine, unless `short_wait`)
pub fn wait() -> Duration {
    #[cfg(test)]
    return TEST_WAIT.with(|w| w.get()).unwrap_or(Duration::from_secs(30));
    #[cfg(not(test))]
    WAIT
}

/// Tests: requests wait only briefly for their jobs until the guard is dropped
#[cfg(test)]
pub fn short_wait() -> impl Drop {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_WAIT.with(|w| w.set(None));
        }
    }
    TEST_WAIT.with(|w| w.set(Some(Duration::from_millis(200))));
    Reset
}
/// What jobs keep in memory (a part of `AppState`)
#[derive(Default)]
pub struct Memory {
    /// Tasks running or recently finished, by id
    pub jobs: crate::sync::Mutex<std::collections::HashMap<String, Job>>,
}

/// How long a finished job can still be looked up
const KEEP_FINISHED_SECS: i64 = 3600;

/// How many jobs of a kind one person may run at the same time
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    /// Compressing and extracting: they use the processor and temporary space
    Archive,
    /// Other changes started from the page
    Changes,
    /// WebDAV requests (each waits for its own job; a client sends several at once)
    None,
}

impl Limit {
    fn of(self) -> Option<(usize, &'static str)> {
        match self {
            Limit::Archive => Some((4, "Several ZIP files are being made or extracted already. Wait for one to finish.")),
            Limit::Changes => Some((8, "Several changes are still being made. Wait for one to finish.")),
            Limit::None => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Job {
    pub id: String,
    /// "compress", "extract", "move", "copy", "delete", "empty_trash", "scan", "delete_user", "remove_personal" or
    /// "webdav"
    pub kind: &'static str,
    /// "running", "done" or "failed"
    pub state: &'static str,
    /// Work done so far, of `total` (bytes for compressing and extracting, bytes and items for moving and copying,
    /// items otherwise)
    pub done: u64,
    pub total: u64,
    pub error: Option<String>,
    /// The new item (ZIP file, folder), and its name
    pub node_id: Option<String>,
    pub name: Option<String>,
    /// What a finished job reports besides (a check of a folder space: its report)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip)]
    owner: i64,
    #[serde(skip)]
    limit: Limit,
    #[serde(skip)]
    finished_at: Option<i64>,
}

impl Job {
    /// A change that needed no job: done by the time the request answers
    pub fn done(kind: &'static str) -> Job {
        Job {
            id: String::new(),
            kind,
            state: "done",
            done: 0,
            total: 0,
            error: None,
            node_id: None,
            name: None,
            result: None,
            owner: 0,
            limit: Limit::None,
            finished_at: Some(now()),
        }
    }

    pub fn running(&self) -> bool {
        self.state == "running"
    }
}

/// What a finished job reports
#[derive(Debug, Default)]
pub struct Outcome {
    pub node_id: Option<String>,
    pub name: Option<String>,
    pub result: Option<Value>,
}

impl Outcome {
    pub fn node(id: String, name: String) -> Outcome {
        Outcome { node_id: Some(id), name: Some(name), result: None }
    }
}

/// Counts a job's progress; the default one belongs to no job (work done inside a request, or by the server itself)
#[derive(Clone, Default)]
pub struct Tracker(Option<(AppState, String)>);

impl Tracker {
    pub fn of(st: &AppState, job: &Job) -> Tracker {
        Tracker(Some((st.clone(), job.id.clone())))
    }

    fn update(&self, f: impl FnOnce(&mut Job)) {
        if let Some((st, id)) = &self.0 {
            update(st, id, f);
        }
    }

    /// More work to do than counted so far
    pub fn add_total(&self, n: u64) {
        self.update(|j| j.total += n);
    }

    pub fn set_total(&self, n: u64) {
        self.update(|j| j.total = n);
    }

    /// Work done
    pub fn add(&self, n: u64) {
        if n > 0 {
            self.update(|j| j.done += n);
        }
    }

    pub fn set(&self, done: u64, total: u64) {
        self.update(|j| (j.done, j.total) = (done, total));
    }
}

/// Registers a new job of `owner`; refused while they already run several of its kind
pub fn start(st: &AppState, owner: i64, kind: &'static str, limit: Limit) -> AppResult<Job> {
    let mut jobs = st.part::<Memory>().jobs.lock();
    let t = now();
    jobs.retain(|_, j| j.finished_at.is_none_or(|f| f > t - KEEP_FINISHED_SECS));
    if let Some((most, message)) = limit.of()
        && jobs.values().filter(|j| j.owner == owner && j.limit == limit && j.finished_at.is_none()).count() >= most
    {
        return Err(AppError::new(StatusCode::TOO_MANY_REQUESTS, message));
    }
    let job = Job { id: new_id(), kind, state: "running", done: 0, total: 0, error: None, node_id: None, name: None, result: None, owner, limit, finished_at: None };
    jobs.insert(job.id.clone(), job.clone());
    Ok(job)
}

fn update(st: &AppState, id: &str, f: impl FnOnce(&mut Job)) {
    if let Some(j) = st.part::<Memory>().jobs.lock().get_mut(id) {
        f(j);
    }
}

fn snapshot(st: &AppState, id: &str) -> Option<Job> {
    st.part::<Memory>().jobs.lock().get(id).cloned()
}

fn finish(st: &AppState, id: &str, result: &AppResult<Outcome>) {
    update(st, id, |j| {
        j.finished_at = Some(now());
        match result {
            Ok(o) => {
                j.state = "done";
                j.done = j.total;
                j.node_id = o.node_id.clone();
                j.name = o.name.clone();
                j.result = o.result.clone();
            }
            Err(e) => {
                j.state = "failed";
                j.error = Some(e.message.clone());
            }
        }
    });
}

/// Marks a job failed should its task end without recording how (a bug that panicked)
struct Unfinished(AppState, String);

impl Drop for Unfinished {
    fn drop(&mut self) {
        update(&self.0, &self.1, |j| {
            if j.finished_at.is_none() {
                j.finished_at = Some(now());
                j.state = "failed";
                j.error = Some("A server error occurred".into());
            }
        });
    }
}

/// Runs a job in the background and records how it ended
pub fn spawn<F>(st: &AppState, job: &Job, work: F)
where
    F: Future<Output = AppResult<Outcome>> + Send + 'static,
{
    drop(launch(st, job, work));
}

/// `spawn`, whose receiver gets the result too
fn launch<F>(st: &AppState, job: &Job, work: F) -> tokio::sync::oneshot::Receiver<AppResult<Outcome>>
where
    F: Future<Output = AppResult<Outcome>> + Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (st, id, kind) = (st.clone(), job.id.clone(), job.kind);
    tokio::spawn(async move {
        let _unfinished = Unfinished(st.clone(), id.clone());
        // Counted as background work, apart from what people wait for (Storage usage)
        let result = crate::usage::background(work).await;
        if let Err(e) = &result {
            tracing::info!("Task {id} ({kind}) failed: {}", e.message);
        }
        finish(&st, &id, &result);
        // Nobody waits any more once the request has answered
        let _ = tx.send(result);
    });
    rx
}

/// A job registered before the change that may need it is made, so that a change refused for having too many jobs
/// running is refused before anything changed. Forgotten again unless it is run.
pub struct Pending {
    st: AppState,
    job: Option<Job>,
}

/// Registers a job of `owner` to run once it is known what it has to do
pub fn reserve(st: &AppState, owner: i64, kind: &'static str, limit: Limit) -> AppResult<Pending> {
    Ok(Pending { st: st.clone(), job: Some(start(st, owner, kind, limit)?) })
}

impl Pending {
    pub fn kind(&self) -> &'static str {
        self.job.as_ref().map_or("", |j| j.kind)
    }

    /// Starts `work` as the job and waits up to `wait` for it: a job done by then answers as the change always did (its
    /// error, when it failed); else the running job is returned, for the page to follow. The work goes on either way,
    /// also when the request is given up.
    pub async fn run<F, Fut>(mut self, wait: Duration, work: F) -> AppResult<Job>
    where
        F: FnOnce(Tracker) -> Fut,
        Fut: Future<Output = AppResult<Outcome>> + Send + 'static,
    {
        let job = self.job.take().expect("a job runs once");
        let rx = launch(&self.st, &job, work(Tracker::of(&self.st, &job)));
        match tokio::time::timeout(wait, rx).await {
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(_)) => Err(AppError::internal("a task stopped without an answer")),
            Ok(Ok(Ok(_))) | Err(_) => Ok(snapshot(&self.st, &job.id).unwrap_or(job)),
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            self.st.part::<Memory>().jobs.lock().remove(&job.id);
        }
    }
}

/// `reserve` and `Pending::run` at once
pub async fn run<F, Fut>(st: &AppState, user: &User, kind: &'static str, limit: Limit, wait: Duration, work: F) -> AppResult<Job>
where
    F: FnOnce(Tracker) -> Fut,
    Fut: Future<Output = AppResult<Outcome>> + Send + 'static,
{
    reserve(st, user.id, kind, limit)?.run(wait, work).await
}

/// A job's progress; only its owner can see it
pub async fn get(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<Json<Job>> {
    match st.part::<Memory>().jobs.lock().get(&id) {
        Some(j) if j.owner == user.id => Ok(Json(j.clone())),
        _ => Err(AppError::not_found("This task has finished or doesn't exist")),
    }
}

/// The jobs kept now, for tests
#[cfg(test)]
pub fn all(st: &AppState) -> std::collections::HashMap<String, Job> {
    st.part::<Memory>().jobs.lock().clone()
}

/// Waits for a job to finish (tests)
#[cfg(test)]
pub async fn wait_for(st: &AppState, id: &str) -> Job {
    for _ in 0..3000 {
        let job = snapshot(st, id).expect("the job is known");
        if job.state != "running" {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the job didn't finish");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[tokio::test]
    async fn a_job_answers_the_request_when_done_in_time_and_goes_on_otherwise() {
        let env = testutil::env().await;
        let amy = env.user("amy", false).await;
        // Done at once: answered as done
        let job = run(&env.st, &amy, "move", Limit::Changes, WAIT, |_| async { Ok(Outcome::default()) }).await.unwrap();
        assert_eq!(job.state, "done");
        // Failed at once: the error is the answer
        let err =
            run(&env.st, &amy, "move", Limit::Changes, WAIT, |_| async { Err(AppError::conflict("An item with the same name already exists")) }).await.unwrap_err();
        assert_eq!((err.status, err.message.as_str()), (StatusCode::CONFLICT, "An item with the same name already exists"));
        // Longer: the running job is the answer, with its progress
        let (go, wait) = tokio::sync::oneshot::channel::<()>();
        let job = run(&env.st, &amy, "copy", Limit::Changes, Duration::from_millis(50), |t| async move {
            t.set_total(10);
            t.add(4);
            let _ = wait.await;
            Ok(Outcome::default())
        })
        .await
        .unwrap();
        assert_eq!(job.state, "running");
        assert_eq!((job.done, job.total), (4, 10));
        // Only its owner sees it
        let bob = env.user("bob", false).await;
        assert!(get(State(env.st.clone()), bob, Path(job.id.clone())).await.is_err());
        go.send(()).unwrap();
        let done = wait_for(&env.st, &job.id).await;
        assert_eq!((done.state, done.done), ("done", 10));
    }

    #[tokio::test]
    async fn a_task_that_stops_without_an_answer_is_reported_failed() {
        let env = testutil::env().await;
        let amy = env.user("amy", false).await;
        let job = start(&env.st, amy.id, "move", Limit::Changes).unwrap();
        spawn(&env.st, &job, async { panic!("a bug") });
        let done = wait_for(&env.st, &job.id).await;
        assert_eq!(done.state, "failed");
    }

    #[tokio::test]
    async fn a_person_runs_a_few_jobs_of_a_kind_at_a_time() {
        let env = testutil::env().await;
        let amy = env.user("amy", false).await;
        for _ in 0..4 {
            start(&env.st, amy.id, "compress", Limit::Archive).unwrap();
        }
        assert_eq!(start(&env.st, amy.id, "compress", Limit::Archive).unwrap_err().status, StatusCode::TOO_MANY_REQUESTS);
        // Other kinds are counted apart
        start(&env.st, amy.id, "move", Limit::Changes).unwrap();
        for _ in 0..20 {
            start(&env.st, amy.id, "webdav", Limit::None).unwrap();
        }
        assert_eq!(all(&env.st).len(), 25);
    }
}
