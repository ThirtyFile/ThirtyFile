//! The step-by-step test of a storage location: connect, write and read back a small file, write and read back a
//! larger one through the path uploads take (on S3 a multipart upload), then delete both and check they are gone.
//! Keys that may write but not delete fail here, instead of later when files are deleted.

use std::{
    io,
    path::Path as FsPath,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::{Path, State},
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{CHECK_DIR, location};
use crate::{
    auth::Admin,
    error::AppResult,
    locations::{self, describe},
    state::AppState,
    storage::{self, Storage},
    util::new_id,
};

/// Size of the larger test file: above the 16 MiB from which uploads to S3 go in parts
pub const LARGE: u64 = 24 << 20;
/// Time for every step before deleting: with `DELETE_LIMIT` the test ends well within the API's 120-second limit
const BUDGET: Duration = Duration::from_secs(90);
/// Time for deleting the test files and checking they are gone
const DELETE_LIMIT: Duration = Duration::from_secs(20);
/// Most time for connecting, or writing or reading the small file
const STEP_LIMIT: Duration = Duration::from_secs(20);
/// A step with the larger file doesn't start with less time than this left
const LARGE_MIN: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Ok,
    Error,
    Skipped,
}

#[derive(Debug, Serialize)]
pub struct Step {
    /// connect, write, read, write_large, read_large, delete, cleanup
    pub id: &'static str,
    pub outcome: Outcome,
    /// How long it took, in milliseconds
    pub ms: u64,
    /// Bytes written or read (the larger file)
    pub bytes: Option<u64>,
    /// Megabytes (10^6 bytes) per second
    pub speed: Option<f64>,
    /// Why it failed or was skipped
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub ok: bool,
    pub steps: Vec<Step>,
}

#[cfg(test)]
impl Report {
    pub fn step(&self, id: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.id == id)
    }
}

/// Runs the test on a location's saved settings, with a connection of its own. When every step passes and the
/// location wasn't connected (its settings couldn't be loaded at start), it is connected from now on.
pub async fn test_steps(State(st): State<AppState>, _: Admin, Path(id): Path<String>) -> AppResult<Json<Report>> {
    let loc = location(&st, &id).await?;
    let backend = match storage::build(&loc.id, &loc.kind, &loc.config, &st.storage_dir) {
        Ok(b) => b,
        Err(e) => {
            let mut steps = Steps::new();
            steps.push("connect", Outcome::Error, Duration::ZERO, Some(format!("Invalid settings: {e}")));
            return Ok(Json(steps.finish()));
        }
    };
    // Counted as a test, apart from what people do (Storage usage)
    let metered = crate::usage::Metered::wrap(backend.clone(), st.usage.clone(), &loc.id);
    let report = crate::usage::probe(run(&st, metered, &loc.kind, LARGE)).await;
    if report.ok {
        st.storages.write().unwrap().entry(id.clone()).or_insert(backend);
        let _ = locations::probe(&st, &id).await;
    }
    Ok(Json(report))
}

struct Steps {
    started: Instant,
    list: Vec<Step>,
}

impl Steps {
    fn new() -> Steps {
        Steps { started: Instant::now(), list: Vec::new() }
    }

    /// Time left for the steps before deleting
    fn left(&self) -> Duration {
        (self.started + BUDGET).saturating_duration_since(Instant::now())
    }

    fn failed(&self) -> bool {
        self.list.iter().any(|s| s.outcome == Outcome::Error)
    }

    fn push(&mut self, id: &'static str, outcome: Outcome, took: Duration, message: Option<String>) -> &mut Step {
        self.list.push(Step { id, outcome, ms: took.as_millis() as u64, bytes: None, speed: None, message });
        self.list.last_mut().unwrap()
    }

    /// Records a step with the larger file: its bytes and speed when it passed
    fn push_transfer(&mut self, id: &'static str, took: Duration, bytes: u64, result: Result<(), String>) {
        let step = match result {
            Ok(()) => self.push(id, Outcome::Ok, took, None),
            Err(e) => self.push(id, Outcome::Error, took, Some(e)),
        };
        if step.outcome == Outcome::Ok {
            step.bytes = Some(bytes);
            // A local folder on the same disk as the temp folder takes the file by renaming it: no speed to tell
            if took >= Duration::from_millis(10) {
                step.speed = Some(bytes as f64 / took.as_secs_f64() / 1e6);
            }
        }
    }

    fn skip(&mut self, id: &'static str, message: Option<String>) {
        self.push(id, Outcome::Skipped, Duration::ZERO, message);
    }

    /// Every step in order, those that didn't run as skipped; the clean-up after a failure last
    fn finish(mut self) -> Report {
        let ok = !self.failed() && !self.list.is_empty();
        let mut steps = Vec::new();
        for id in ["connect", "write", "read", "write_large", "read_large", "delete", "cleanup"] {
            match self.list.iter().position(|s| s.id == id) {
                Some(i) => steps.push(self.list.swap_remove(i)),
                None if id != "cleanup" => {
                    steps.push(Step { id, outcome: Outcome::Skipped, ms: 0, bytes: None, speed: None, message: None })
                }
                None => {}
            }
        }
        Report { ok, steps }
    }
}

/// Runs the steps on `s` (a location of kind `kind`), with a larger file of `large` bytes
pub async fn run(st: &AppState, s: Arc<dyn Storage>, kind: &str, large: u64) -> Report {
    let mut steps = Steps::new();
    let tag = new_id();
    let small_key = format!("{CHECK_DIR}/{tag}-small");
    let large_key = format!("{CHECK_DIR}/{tag}-large");

    // 1. Connect (a light request; on local folders a small write)
    let t = Instant::now();
    match tokio::time::timeout(STEP_LIMIT, s.ping()).await {
        Ok(Ok(())) => steps.push("connect", Outcome::Ok, t.elapsed(), None),
        Ok(Err(e)) => steps.push("connect", Outcome::Error, t.elapsed(), Some(describe(&e))),
        Err(_) => steps.push("connect", Outcome::Error, t.elapsed(), Some("Connection timed out. Check that the service is running.".into())),
    };
    if steps.failed() {
        // Nothing was written
        return steps.finish();
    }

    let tmp_small = st.tmp_dir().join(format!("storage-test-{tag}-small"));
    let tmp_large = st.tmp_dir().join(format!("storage-test-{tag}-large"));
    data_steps(&s, &mut steps, (&small_key, &tmp_small), (&large_key, &tmp_large), large).await;
    let _ = tokio::fs::remove_file(&tmp_small).await;
    let _ = tokio::fs::remove_file(&tmp_large).await;

    // 6. Delete both and check they are gone; after a failure, the same as clean-up
    let t = Instant::now();
    let deleted = match tokio::time::timeout(DELETE_LIMIT, delete_and_verify(s.as_ref(), kind, &[&small_key, &large_key])).await {
        Ok(r) => r,
        Err(_) => Err("Deleting the test files timed out".to_string()),
    };
    let id = if steps.failed() { "cleanup" } else { "delete" };
    match deleted {
        Ok(()) => steps.push(id, Outcome::Ok, t.elapsed(), None),
        Err(e) => steps.push(id, Outcome::Error, t.elapsed(), Some(e)),
    };
    steps.finish()
}

/// Steps 2 to 5: the small file, then the larger one. Stops at the first failure.
async fn data_steps(s: &Arc<dyn Storage>, steps: &mut Steps, small: (&str, &FsPath), large: (&str, &FsPath), size: u64) {
    let body = format!("ThirtyFile storage test {}\n", small.0).into_bytes();
    let small_hash = crate::util::sha256_hex(&body);

    // 2. Write a small file
    let t = Instant::now();
    let written = match tokio::fs::write(small.1, &body).await {
        Ok(()) => put_detached(s.clone(), small.0, small.1, STEP_LIMIT.min(steps.left())).await,
        Err(e) => Some(Err(e)),
    };
    match written {
        Some(Ok(())) => steps.push("write", Outcome::Ok, t.elapsed(), None),
        Some(Err(e)) => steps.push("write", Outcome::Error, t.elapsed(), Some(describe(&e))),
        None => steps.push("write", Outcome::Error, t.elapsed(), Some("Writing a small file timed out".into())),
    };
    if steps.failed() {
        return;
    }

    // 3. Read it back and compare
    let t = Instant::now();
    let read = tokio::time::timeout(STEP_LIMIT.min(steps.left()), read_hash(s.as_ref(), small.0, body.len() as u64)).await;
    match compare(read, &small_hash, body.len() as u64) {
        Ok(()) => steps.push("read", Outcome::Ok, t.elapsed(), None),
        Err(e) => steps.push("read", Outcome::Error, t.elapsed(), Some(e)),
    };
    if steps.failed() {
        return;
    }

    // 4. Write a larger file
    if steps.left() < LARGE_MIN {
        return steps.skip("write_large", Some("Skipped: the steps before took most of the time available".into()));
    }
    let path = large.1.to_path_buf();
    let hash = match tokio::task::spawn_blocking(move || make_file(&path, size)).await.map_err(io::Error::other).and_then(|r| r) {
        Ok(h) => h,
        Err(e) => {
            steps.push("write_large", Outcome::Error, Duration::ZERO, Some(format!("Couldn't prepare the test file: {e}")));
            return;
        }
    };
    let limit = steps.left();
    let t = Instant::now();
    match put_detached(s.clone(), large.0, large.1, limit).await {
        Some(r) => steps.push_transfer("write_large", t.elapsed(), size, r.map_err(|e| describe(&e))),
        None => {
            let message = format!("Skipped: writing {} MB took longer than {} seconds", size / 1_000_000, limit.as_secs());
            steps.push("write_large", Outcome::Skipped, t.elapsed(), Some(message));
            return;
        }
    }
    if steps.failed() {
        return;
    }

    // 5. Read it back and compare
    if steps.left() < LARGE_MIN {
        return steps.skip("read_large", Some("Skipped: the steps before took most of the time available".into()));
    }
    let limit = steps.left();
    let t = Instant::now();
    match tokio::time::timeout(limit, read_hash(s.as_ref(), large.0, size)).await {
        Err(_) => {
            let message = format!("Skipped: reading {} MB took longer than {} seconds", size / 1_000_000, limit.as_secs());
            steps.push("read_large", Outcome::Skipped, t.elapsed(), Some(message));
        }
        read => steps.push_transfer("read_large", t.elapsed(), size, compare(read, &hash, size)),
    }
}

/// Writes the temp file `src` to `key` in a task of its own, None when it takes longer than `limit`. Stopping a
/// write halfway could leave a part under a temporary name, so the task finishes and then deletes what it wrote.
async fn put_detached(s: Arc<dyn Storage>, key: &str, src: &FsPath, limit: Duration) -> Option<io::Result<()>> {
    let (key, src) = (key.to_string(), src.to_path_buf());
    let mut task = {
        let (s, key, src) = (s.clone(), key.clone(), src.clone());
        tokio::spawn(async move { s.put_at(&key, &src).await })
    };
    match tokio::time::timeout(limit, &mut task).await {
        Ok(joined) => Some(joined.unwrap_or_else(|e| Err(io::Error::other(e)))),
        Err(_) => {
            tokio::spawn(async move {
                if matches!(task.await, Ok(Ok(()))) {
                    let _ = s.delete_at(&key).await;
                }
                let _ = tokio::fs::remove_file(&src).await;
            });
            None
        }
    }
}

/// SHA-256 and length of the first `len` bytes of the file at `key`
async fn read_hash(s: &dyn Storage, key: &str, len: u64) -> io::Result<(String, u64)> {
    let mut reader = s.open_at(key, 0, len).await?;
    crate::hashing::read_async(&mut reader).await
}

fn compare(read: Result<io::Result<(String, u64)>, tokio::time::error::Elapsed>, hash: &str, len: u64) -> Result<(), String> {
    match read {
        Err(_) => Err("Reading the test file back timed out".into()),
        Ok(Err(e)) => Err(describe(&e)),
        Ok(Ok((h, n))) if h == hash && n == len => Ok(()),
        Ok(Ok(_)) => Err("The content read back didn't match".into()),
    }
}

/// Writes `size` bytes that don't compress (a transfer can't look faster than it is) to `path`; returns their SHA-256
fn make_file(path: &FsPath, size: u64) -> io::Result<String> {
    use std::io::Write;
    let mut f = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(path)?);
    let mut hasher = Sha256::new();
    // splitmix64: fast, and plenty for bytes nobody reads
    let mut x: u64 = rand::random();
    let mut buf = vec![0u8; 1 << 20];
    let mut left = size;
    while left > 0 {
        let n = buf.len().min(left as usize);
        for chunk in buf[..n].chunks_mut(8) {
            x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            chunk.copy_from_slice(&z.to_le_bytes()[..chunk.len()]);
        }
        hasher.update(&buf[..n]);
        f.write_all(&buf[..n])?;
        left -= n as u64;
    }
    f.flush()?;
    Ok(hex::encode(hasher.finalize()))
}

/// Deletes the files and checks each is gone. A file that isn't there (never written) counts as deleted.
async fn delete_and_verify(s: &dyn Storage, kind: &str, keys: &[&str]) -> Result<(), String> {
    for key in keys {
        s.delete_at(key).await.map_err(|e| cant_delete(kind, &e))?;
        match s.stat(key).await {
            Ok(None) => {}
            Ok(Some(_)) => return Err(format!("A test file is still there after it was deleted: {key}")),
            Err(e) => return Err(format!("Couldn't check that the test file was deleted: {}", describe(&e))),
        }
    }
    Ok(())
}

fn cant_delete(kind: &str, e: &io::Error) -> String {
    let why = describe(e);
    match kind {
        "s3" => format!("The keys can't delete files: {why}"),
        "local" => format!("ThirtyFile can't delete files in this folder: {why}"),
        _ => format!("The account can't delete files: {why}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Test files a test left behind: none when it finished
    fn leftovers(dir: &FsPath) -> Vec<PathBuf> {
        std::fs::read_dir(dir.join(CHECK_DIR)).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default()
    }
    use crate::{
        storage::{BoxReader, Entry, LocalStorage},
        testutil,
    };
    use futures_util::future::BoxFuture;

    /// A local folder whose deletes can be refused, and whose reads can return other bytes
    struct Picky {
        inner: LocalStorage,
        refuse_delete: bool,
        garble: bool,
    }

    impl Storage for Picky {
        fn put_file<'a>(&'a self, hash: &'a str, src: &'a FsPath) -> BoxFuture<'a, io::Result<()>> {
            self.inner.put_file(hash, src)
        }
        fn open<'a>(&'a self, hash: &'a str, start: u64, len: u64) -> BoxFuture<'a, io::Result<BoxReader>> {
            self.inner.open(hash, start, len)
        }
        fn delete<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, io::Result<()>> {
            self.inner.delete(hash)
        }
        fn check(&self) -> BoxFuture<'_, io::Result<()>> {
            self.inner.check()
        }
        fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, io::Result<Option<Entry>>> {
            self.inner.stat(key)
        }
        fn put_at<'a>(&'a self, key: &'a str, src: &'a FsPath) -> BoxFuture<'a, io::Result<()>> {
            self.inner.put_at(key, src)
        }
        fn open_at<'a>(&'a self, key: &'a str, start: u64, len: u64) -> BoxFuture<'a, io::Result<BoxReader>> {
            if self.garble {
                return Box::pin(async move { Ok(Box::pin(std::io::Cursor::new(vec![b'x'; len as usize])) as BoxReader) });
            }
            self.inner.open_at(key, start, len)
        }
        fn delete_at<'a>(&'a self, key: &'a str) -> BoxFuture<'a, io::Result<()>> {
            if self.refuse_delete {
                return Box::pin(async { Err(io::Error::new(io::ErrorKind::PermissionDenied, "Access Denied")) });
            }
            self.inner.delete_at(key)
        }
    }

    fn picky(env: &testutil::TestEnv, refuse_delete: bool, garble: bool) -> (Arc<dyn Storage>, PathBuf) {
        let dir = env.dir.join(format!("picky-{}", new_id()));
        (Arc::new(Picky { inner: LocalStorage::create(dir.clone(), "picky").unwrap(), refuse_delete, garble }), dir)
    }

    fn outcomes(r: &Report) -> Vec<(&str, Outcome)> {
        r.steps.iter().map(|s| (s.id, s.outcome)).collect()
    }

    #[tokio::test]
    async fn every_step_passes_on_a_local_folder_and_nothing_is_left() {
        let env = testutil::env().await;
        let (s, dir) = picky(&env, false, false);
        let report = run(&env.st, s, "local", 3 << 20).await;
        assert!(report.ok, "{report:?}");
        assert_eq!(
            outcomes(&report),
            [("connect", Outcome::Ok), ("write", Outcome::Ok), ("read", Outcome::Ok), ("write_large", Outcome::Ok), ("read_large", Outcome::Ok), ("delete", Outcome::Ok)]
        );
        let large = report.step("write_large").unwrap();
        assert_eq!(large.bytes, Some(3 << 20));
        // A rename on the same disk has no speed to tell
        assert!(large.speed.is_none_or(|s| s > 0.0));
        let mut steps = Steps::new();
        steps.push_transfer("read_large", Duration::from_millis(500), 24_000_000, Ok(()));
        assert_eq!(steps.list[0].speed, Some(48.0));
        assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));
        let tmp: Vec<_> = std::fs::read_dir(env.st.tmp_dir()).unwrap().flatten().collect();
        assert!(tmp.is_empty(), "temp files are removed");
    }

    #[tokio::test]
    async fn keys_that_cant_delete_fail_the_test() {
        let env = testutil::env().await;
        let (s, _dir) = picky(&env, true, false);
        let report = run(&env.st, s, "s3", 1 << 20).await;
        assert!(!report.ok);
        let delete = report.step("delete").unwrap();
        assert_eq!(delete.outcome, Outcome::Error);
        assert!(delete.message.as_deref().unwrap().starts_with("The keys can't delete files: "), "{delete:?}");
    }

    #[tokio::test]
    async fn a_failed_step_stops_the_test_and_cleans_up() {
        let env = testutil::env().await;
        let (s, dir) = picky(&env, false, true);
        let report = run(&env.st, s, "sftp", 1 << 20).await;
        assert!(!report.ok);
        assert_eq!(
            outcomes(&report),
            [
                ("connect", Outcome::Ok),
                ("write", Outcome::Ok),
                ("read", Outcome::Error),
                ("write_large", Outcome::Skipped),
                ("read_large", Outcome::Skipped),
                ("delete", Outcome::Skipped),
                ("cleanup", Outcome::Ok)
            ]
        );
        assert_eq!(report.step("read").unwrap().message.as_deref(), Some("The content read back didn't match"));
        assert!(leftovers(&dir).is_empty(), "the small file written before is deleted");
    }

    #[tokio::test]
    async fn the_quick_check_reports_a_delete_it_couldnt_do() {
        // The same rule in every check(): deleting is part of passing
        let e = storage::delete_failed(io::Error::new(io::ErrorKind::PermissionDenied, "Access Denied"), storage::CANT_DELETE_S3);
        assert_eq!(describe(&e), storage::CANT_DELETE_S3);
        let unreachable = io::Error::other(storage::StorageError { message: storage::UNAVAILABLE, detail: "timed out".into() });
        assert_eq!(describe(&storage::delete_failed(unreachable, storage::CANT_DELETE_S3)), describe(&io::Error::other(storage::StorageError { message: storage::UNAVAILABLE, detail: "timed out".into() })));
    }
}
