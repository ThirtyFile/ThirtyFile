//! Measured storage operations: every call to a storage location's backend (`Metered`, handed out by
//! `Inner::storage`) and every file of a folder space read or stored is timed and counted here, in memory. Nothing is
//! written on the request path: the sampler (sample.rs) takes the counters every five minutes and writes them.
//!
//! Counters are kept per location, operation and kind of work. Durations go into a histogram of fixed buckets
//! (`Hist`), so memory stays the same however many operations there are, and percentiles can be merged across
//! periods.

use std::{
    collections::HashMap,
    future::Future,
    io,
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::{Duration, Instant},
};

use futures_util::future::BoxFuture;
use tokio::io::{AsyncRead, ReadBuf};

use crate::storage::{BoxReader, Entry, Storage};

/// What an operation does
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Op {
    /// Opening content to read it (timed to when it can be read; the bytes are counted as they are read)
    Read,
    /// Storing content
    Write,
    Delete,
    /// Listing, looking up and measuring what is stored
    List,
    /// Connection checks
    Check,
}

impl Op {
    pub const ALL: [Op; 5] = [Op::Read, Op::Write, Op::Delete, Op::List, Op::Check];
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Read => "read",
            Op::Write => "write",
            Op::Delete => "delete",
            Op::List => "list",
            Op::Check => "check",
        }
    }
    pub fn parse(s: &str) -> Option<Op> {
        Op::ALL.into_iter().find(|o| o.as_str() == s)
    }
}

/// Why an operation runs, so a move or a test doesn't make what people wait for impossible to read
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Work {
    /// People using their files (the default)
    Foreground,
    /// Moves of spaces, compressing and extracting, deleting unused content, scans
    Background,
    /// Health checks and connection tests
    Probe,
}

impl Work {
    pub const ALL: [Work; 3] = [Work::Foreground, Work::Background, Work::Probe];
    pub fn as_str(self) -> &'static str {
        match self {
            Work::Foreground => "foreground",
            Work::Background => "background",
            Work::Probe => "probe",
        }
    }
    pub fn parse(s: &str) -> Option<Work> {
        Work::ALL.into_iter().find(|w| w.as_str() == s)
    }
}

tokio::task_local! {
    static WORK: Work;
}

/// The kind of work the current task does
pub fn current_work() -> Work {
    WORK.try_with(|w| *w).unwrap_or(Work::Foreground)
}

/// Runs `fut` as background work: its storage operations are counted apart from what people do
pub async fn background<F: Future>(fut: F) -> F::Output {
    WORK.scope(Work::Background, fut).await
}

/// Runs `fut` as a health check or test
pub async fn probe<F: Future>(fut: F) -> F::Output {
    WORK.scope(Work::Probe, fut).await
}

// ───────────── Histogram ─────────────

/// Buckets of the duration histogram: the first holds everything under `BASE_US`, then four per doubling (each about
/// 19% wider than the one before) up to about seven minutes, and the last everything longer
pub const BUCKETS: usize = 90;
const BASE_US: f64 = 100.0;
const PER_DOUBLING: f64 = 4.0;

/// The bucket of a duration
pub fn bucket_of(us: u64) -> usize {
    if (us as f64) < BASE_US {
        return 0;
    }
    let i = ((us as f64 / BASE_US).log2() * PER_DOUBLING).floor() as usize + 1;
    i.min(BUCKETS - 1)
}

/// Where bucket `i` starts (microseconds)
fn lower(i: usize) -> f64 {
    if i == 0 { 0.0 } else { BASE_US * 2f64.powf((i - 1) as f64 / PER_DOUBLING) }
}

/// The value a bucket stands for: the middle of its range on a log scale (at most 9% off anything in it)
fn middle(i: usize) -> f64 {
    match i {
        0 => BASE_US / 2.0,
        i if i == BUCKETS - 1 => lower(i),
        i => (lower(i) * lower(i + 1)).sqrt(),
    }
}

/// Counts of durations by bucket
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Hist(Vec<u64>);

impl Hist {
    pub fn add(&mut self, us: u64) {
        if self.0.is_empty() {
            self.0 = vec![0; BUCKETS];
        }
        self.0[bucket_of(us)] += 1;
    }

    pub fn merge(&mut self, other: &Hist) {
        if other.0.is_empty() {
            return;
        }
        if self.0.is_empty() {
            self.0 = vec![0; BUCKETS];
        }
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            *a += b;
        }
    }

    pub fn count(&self) -> u64 {
        self.0.iter().sum()
    }

    /// The duration below which a share `q` (0 to 1) of the operations took, in microseconds; None without any
    pub fn quantile(&self, q: f64) -> Option<f64> {
        let total = self.count();
        if total == 0 {
            return None;
        }
        let rank = ((q.clamp(0.0, 1.0) * total as f64).ceil() as u64).max(1);
        let mut seen = 0;
        for (i, n) in self.0.iter().enumerate() {
            seen += n;
            if seen >= rank {
                return Some(middle(i));
            }
        }
        None
    }

    /// As stored: "bucket:count" of the buckets that aren't empty, comma-separated
    pub fn encode(&self) -> String {
        let parts: Vec<String> = self.0.iter().enumerate().filter(|(_, n)| **n > 0).map(|(i, n)| format!("{i}:{n}")).collect();
        parts.join(",")
    }

    /// Reads `encode`'s text; parts that don't make sense are left out
    pub fn decode(s: &str) -> Hist {
        let mut h = Hist::default();
        for part in s.split(',').filter(|p| !p.is_empty()) {
            if let Some((i, n)) = part.split_once(':')
                && let (Ok(i), Ok(n)) = (i.parse::<usize>(), n.parse::<u64>())
                && i < BUCKETS
            {
                if h.0.is_empty() {
                    h.0 = vec![0; BUCKETS];
                }
                h.0[i] += n;
            }
        }
        h
    }
}

// ───────────── Counters ─────────────

/// Operations of one kind on one location in a period
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Window {
    pub count: u64,
    /// Of `count`: failed
    pub errors: u64,
    /// Of `count`: timed out, or abandoned before they finished (the caller gave up, a person left)
    pub timeouts: u64,
    pub bytes: u64,
    pub total_us: u64,
    pub max_us: u64,
    pub hist: Hist,
}

impl Window {
    pub fn merge(&mut self, o: &Window) {
        self.count += o.count;
        self.errors += o.errors;
        self.timeouts += o.timeouts;
        self.bytes += o.bytes;
        self.total_us += o.total_us;
        self.max_us = self.max_us.max(o.max_us);
        self.hist.merge(&o.hist);
    }

    /// A percentile of the durations (microseconds), never above the longest one seen
    pub fn percentile(&self, q: f64) -> Option<f64> {
        self.hist.quantile(q).map(|v| v.min(self.max_us as f64))
    }
}

/// How an operation ended
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Error,
    Timeout,
}

/// Operations running now on a location
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Active {
    /// Calls waiting for the storage
    pub calls: i64,
    /// Content being read (downloads, previews, copies)
    pub transfers: i64,
}

/// Counted operations per location, operation and kind of work
pub type Windows = HashMap<String, HashMap<(Op, Work), Window>>;

/// All counters of the server (`Inner::usage`)
#[derive(Default)]
pub struct Meters {
    windows: Mutex<Windows>,
    active: Mutex<HashMap<String, Active>>,
    /// Folder spaces' folders → their storage location (sample.rs, `folder_location`)
    pub(super) folders: Mutex<super::sample::FolderMap>,
}

impl Meters {
    fn with(&self, location: &str, op: Op, work: Work, f: impl FnOnce(&mut Window)) {
        let mut all = self.windows.lock().unwrap();
        let per = match all.get_mut(location) {
            Some(per) => per,
            None => all.entry(location.to_string()).or_default(),
        };
        f(per.entry((op, work)).or_default());
    }

    /// Counts an operation that took `took`
    pub fn record(&self, location: &str, op: Op, work: Work, took: Duration, outcome: Outcome, bytes: u64) {
        let us = u64::try_from(took.as_micros()).unwrap_or(u64::MAX);
        self.with(location, op, work, |w| {
            w.count += 1;
            match outcome {
                Outcome::Ok => w.bytes += bytes,
                Outcome::Error => w.errors += 1,
                Outcome::Timeout => w.timeouts += 1,
            }
            w.total_us = w.total_us.saturating_add(us);
            w.max_us = w.max_us.max(us);
            w.hist.add(us);
        });
    }

    fn add_bytes(&self, location: &str, op: Op, work: Work, bytes: u64) {
        self.with(location, op, work, |w| w.bytes += bytes);
    }

    /// A read that failed after it started: an error, not another operation
    fn failed_midway(&self, location: &str, op: Op, work: Work) {
        self.with(location, op, work, |w| w.errors += 1);
    }

    /// The counters so far, which start again from nothing
    pub fn take(&self) -> Windows {
        std::mem::take(&mut *self.windows.lock().unwrap())
    }

    /// The counters so far, left as they are
    pub fn snapshot(&self) -> Windows {
        self.windows.lock().unwrap().clone()
    }

    /// Puts counters back (writing them failed: they are written with the next ones)
    pub fn restore(&self, windows: Windows) {
        let mut all = self.windows.lock().unwrap();
        for (location, per) in windows {
            let mine = all.entry(location).or_default();
            for (k, w) in per {
                mine.entry(k).or_default().merge(&w);
            }
        }
    }

    /// Operations running now, by location
    pub fn active(&self) -> HashMap<String, Active> {
        self.active.lock().unwrap().iter().filter(|(_, a)| a.calls > 0 || a.transfers > 0).map(|(k, v)| (k.clone(), *v)).collect()
    }

    fn change_active(&self, location: &str, f: impl FnOnce(&mut Active)) {
        let mut all = self.active.lock().unwrap();
        match all.get_mut(location) {
            Some(a) => f(a),
            None => f(all.entry(location.to_string()).or_default()),
        }
    }

    /// Times `fut` as an operation on `location`; `bytes` are counted when it succeeds. An operation the storage can't
    /// do isn't counted.
    pub async fn timed<T>(&self, location: &str, op: Op, bytes: u64, fut: impl Future<Output = io::Result<T>>) -> io::Result<T> {
        let mut call = Call::start(self, location, op);
        let res = fut.await;
        call.end(res.as_ref().err(), bytes);
        res
    }

    /// Counts the bytes read from `reader` as they are read, and the reader as a transfer while it is open
    pub fn count_reads(self: &Arc<Self>, location: &str, reader: BoxReader) -> BoxReader {
        self.change_active(location, |a| a.transfers += 1);
        Box::pin(Counted { inner: reader, meters: self.clone(), location: location.to_string(), work: current_work(), pending: 0 })
    }
}

/// An operation in progress: counted as timed out when it is dropped before it ends (a timeout around it, a request
/// that was given up)
struct Call<'a> {
    meters: &'a Meters,
    location: &'a str,
    op: Op,
    work: Work,
    started: Instant,
    ended: bool,
}

impl<'a> Call<'a> {
    fn start(meters: &'a Meters, location: &'a str, op: Op) -> Self {
        meters.change_active(location, |a| a.calls += 1);
        Call { meters, location, op, work: current_work(), started: Instant::now(), ended: false }
    }

    fn end(&mut self, err: Option<&io::Error>, bytes: u64) {
        self.ended = true;
        let outcome = match err.map(io::Error::kind) {
            None => Outcome::Ok,
            Some(io::ErrorKind::Unsupported) => return,
            Some(io::ErrorKind::TimedOut) => Outcome::Timeout,
            Some(_) => Outcome::Error,
        };
        self.meters.record(self.location, self.op, self.work, self.started.elapsed(), outcome, bytes);
    }
}

impl Drop for Call<'_> {
    fn drop(&mut self) {
        self.meters.change_active(self.location, |a| a.calls -= 1);
        if !self.ended {
            self.meters.record(self.location, self.op, self.work, self.started.elapsed(), Outcome::Timeout, 0);
        }
    }
}

/// Bytes read are added to the counters in steps of this size (and when the reader ends)
const COUNT_STEP: u64 = 1 << 20;

struct Counted {
    inner: BoxReader,
    meters: Arc<Meters>,
    location: String,
    work: Work,
    pending: u64,
}

impl Counted {
    fn count(&mut self) {
        if self.pending > 0 {
            self.meters.add_bytes(&self.location, Op::Read, self.work, std::mem::take(&mut self.pending));
        }
    }
}

impl AsyncRead for Counted {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let res = this.inner.as_mut().poll_read(cx, buf);
        match &res {
            Poll::Ready(Ok(())) => {
                let n = (buf.filled().len() - before) as u64;
                this.pending += n;
                if n == 0 || this.pending >= COUNT_STEP {
                    this.count();
                }
            }
            Poll::Ready(Err(_)) => {
                this.count();
                this.meters.failed_midway(&this.location, Op::Read, this.work);
            }
            Poll::Pending => {}
        }
        res
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.count();
        self.meters.change_active(&self.location, |a| a.transfers -= 1);
    }
}

// ───────────── Storage backends ─────────────

/// A location's backend whose calls are counted (`Inner::storage`)
pub struct Metered {
    inner: Arc<dyn Storage>,
    meters: Arc<Meters>,
    location: String,
}

impl Metered {
    pub fn wrap(inner: Arc<dyn Storage>, meters: Arc<Meters>, location: &str) -> Arc<dyn Storage> {
        Arc::new(Metered { inner, meters, location: location.to_string() })
    }

    async fn timed<T>(&self, op: Op, bytes: u64, fut: impl Future<Output = io::Result<T>>) -> io::Result<T> {
        self.meters.timed(&self.location, op, bytes, fut).await
    }

    async fn read(&self, fut: impl Future<Output = io::Result<BoxReader>>) -> io::Result<BoxReader> {
        let reader = self.timed(Op::Read, 0, fut).await?;
        Ok(self.meters.count_reads(&self.location, reader))
    }

    /// Size of a temp file about to be stored (it is gone once stored)
    async fn size_of(src: &Path) -> u64 {
        tokio::fs::metadata(src).await.map_or(0, |m| m.len())
    }
}

impl Storage for Metered {
    fn put_file<'a>(&'a self, hash: &'a str, src: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let size = Self::size_of(src).await;
            self.timed(Op::Write, size, self.inner.put_file(hash, src)).await
        })
    }
    fn open<'a>(&'a self, hash: &'a str, start: u64, len: u64) -> BoxFuture<'a, io::Result<BoxReader>> {
        Box::pin(self.read(self.inner.open(hash, start, len)))
    }
    fn delete<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(self.timed(Op::Delete, 0, self.inner.delete(hash)))
    }
    fn check(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(self.timed(Op::Check, 0, self.inner.check()))
    }
    fn ping(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(self.timed(Op::Check, 0, self.inner.ping()))
    }
    fn host_key(&self) -> Option<String> {
        self.inner.host_key()
    }
    fn size<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, io::Result<Option<u64>>> {
        Box::pin(self.timed(Op::List, 0, self.inner.size(hash)))
    }
    fn list(&self) -> BoxFuture<'_, io::Result<Vec<String>>> {
        Box::pin(self.timed(Op::List, 0, self.inner.list()))
    }
    fn content_dir(&self) -> &'static str {
        self.inner.content_dir()
    }
    fn list_dir<'a>(&'a self, dir: &'a str) -> BoxFuture<'a, io::Result<Vec<Entry>>> {
        Box::pin(self.timed(Op::List, 0, self.inner.list_dir(dir)))
    }
    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, io::Result<Option<Entry>>> {
        Box::pin(self.timed(Op::List, 0, self.inner.stat(key)))
    }
    fn put_at<'a>(&'a self, key: &'a str, src: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let size = Self::size_of(src).await;
            self.timed(Op::Write, size, self.inner.put_at(key, src)).await
        })
    }
    fn open_at<'a>(&'a self, key: &'a str, start: u64, len: u64) -> BoxFuture<'a, io::Result<BoxReader>> {
        Box::pin(self.read(self.inner.open_at(key, start, len)))
    }
    fn delete_at<'a>(&'a self, key: &'a str) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(self.timed(Op::Delete, 0, self.inner.delete_at(key)))
    }
    fn list_content<'a>(&'a self, seen: &'a (dyn Fn(u64) + Send + Sync)) -> BoxFuture<'a, io::Result<Vec<Entry>>> {
        Box::pin(self.timed(Op::List, 0, self.inner.list_content(seen)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[test]
    fn buckets_grow_by_a_fifth_and_cover_every_duration() {
        assert_eq!(bucket_of(0), 0);
        assert_eq!(bucket_of(99), 0);
        assert_eq!(bucket_of(100), 1);
        assert_eq!(bucket_of(200), 5, "four buckets per doubling");
        assert_eq!(bucket_of(u64::MAX), BUCKETS - 1);
        // Every bucket's middle falls in it, and is within 10% of both of its ends
        for i in 1..BUCKETS - 1 {
            let (lo, hi, mid) = (lower(i), lower(i + 1), middle(i));
            assert_eq!(bucket_of(mid as u64), i, "{i}");
            assert!(mid / lo < 1.1 && hi / mid < 1.1, "{i}");
        }
        assert!(lower(BUCKETS - 1) > 400e6, "about seven minutes before the last bucket");
    }

    #[test]
    fn percentiles_come_from_the_histogram_within_ten_percent() {
        let mut w = Window::default();
        assert_eq!(w.percentile(0.5), None, "no operations: no latency, not zero");
        // 1 ms to 100 ms, one each: p50 about 50 ms, p95 about 95 ms
        for ms in 1..=100u64 {
            w.hist.add(ms * 1000);
            w.max_us = w.max_us.max(ms * 1000);
        }
        let near = |got: f64, want: f64| (got / want - 1.0).abs() < 0.1;
        assert!(near(w.percentile(0.5).unwrap(), 50_000.0), "{:?}", w.percentile(0.5));
        assert!(near(w.percentile(0.95).unwrap(), 95_000.0), "{:?}", w.percentile(0.95));
        assert!(w.percentile(1.0).unwrap() <= 100_000.0, "never above the longest");
        // A single slow operation among fast ones shows at p95 only when it is more than 5% of them
        let mut fast = Window::default();
        for _ in 0..99 {
            fast.hist.add(2_000);
        }
        fast.hist.add(5_000_000);
        fast.max_us = 5_000_000;
        assert!(fast.percentile(0.95).unwrap() < 2_500.0);
        assert!(fast.percentile(1.0).unwrap() > 4_000_000.0);
    }

    #[test]
    fn histograms_merge_and_survive_being_stored() {
        let (mut a, mut b) = (Hist::default(), Hist::default());
        for us in [50, 150, 150, 3_000] {
            a.add(us);
        }
        for us in [3_000, 9_000_000_000] {
            b.add(us);
        }
        assert_eq!(Hist::decode(&a.encode()), a);
        assert_eq!(Hist::decode(""), Hist::default());
        assert_eq!(Hist::decode("1:2,999:5,x:1,3"), {
            let mut h = Hist::default();
            h.add(100);
            h.add(100);
            h
        });
        let mut both = a.clone();
        both.merge(&b);
        assert_eq!(both.count(), 6);
        assert_eq!(Hist::decode(&both.encode()), both);
        let mut empty = Hist::default();
        empty.merge(&Hist::default());
        assert_eq!(empty.encode(), "");
    }

    /// A backend that answers after `delay`, and fails when `fail`
    struct Slow {
        delay: Duration,
        fail: bool,
    }
    impl Storage for Slow {
        fn put_file<'a>(&'a self, _: &'a str, _: &'a Path) -> BoxFuture<'a, io::Result<()>> {
            Box::pin(async move {
                tokio::time::sleep(self.delay).await;
                if self.fail { Err(io::Error::other("refused")) } else { Ok(()) }
            })
        }
        fn open<'a>(&'a self, _: &'a str, _: u64, len: u64) -> BoxFuture<'a, io::Result<BoxReader>> {
            Box::pin(async move {
                tokio::time::sleep(self.delay).await;
                Ok(Box::pin(std::io::Cursor::new(vec![7u8; len as usize])) as BoxReader)
            })
        }
        fn delete<'a>(&'a self, _: &'a str) -> BoxFuture<'a, io::Result<()>> {
            Box::pin(async { Err(io::Error::new(io::ErrorKind::TimedOut, "no answer")) })
        }
        fn check(&self) -> BoxFuture<'_, io::Result<()>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn every_call_is_timed_and_counted_by_location_operation_and_work() {
        let meters = Arc::new(Meters::default());
        let s = Metered::wrap(Arc::new(Slow { delay: Duration::from_millis(20), fail: false }), meters.clone(), "nas");
        let tmp = std::env::temp_dir().join(format!("thirtyfile-meter-{}", crate::util::new_id()));
        std::fs::write(&tmp, vec![1u8; 3000]).unwrap();
        s.put_file(&"a".repeat(64), &tmp).await.unwrap();
        std::fs::remove_file(&tmp).unwrap();
        // Read: timed until it can be read, the bytes counted as they are read, a transfer while it is open
        let mut r = s.open("h", 0, 5 << 20).await.unwrap();
        assert_eq!(meters.active()["nas"], Active { calls: 0, transfers: 1 });
        let mut got = Vec::new();
        r.read_to_end(&mut got).await.unwrap();
        drop(r);
        assert!(meters.active().is_empty());
        assert!(s.delete("h").await.is_err());
        // Not counted: an operation the storage can't do
        assert!(s.list().await.is_err());
        // Background work and probes are counted apart
        background(s.ping()).await.unwrap();
        probe(s.ping()).await.unwrap();
        let w = meters.take();
        let nas = &w["nas"];
        let write = &nas[&(Op::Write, Work::Foreground)];
        assert_eq!((write.count, write.errors, write.bytes), (1, 0, 3000));
        assert!(write.max_us >= 20_000 && write.total_us >= 20_000, "{write:?}");
        let read = &nas[&(Op::Read, Work::Foreground)];
        assert_eq!((read.count, read.bytes), (1, 5 << 20));
        let delete = &nas[&(Op::Delete, Work::Foreground)];
        assert_eq!((delete.count, delete.errors, delete.timeouts), (1, 0, 1));
        assert!(!nas.contains_key(&(Op::List, Work::Foreground)));
        assert_eq!(nas[&(Op::Check, Work::Background)].count, 1);
        assert_eq!(nas[&(Op::Check, Work::Probe)].count, 1);
        assert!(meters.take().is_empty(), "taking starts again from nothing");
    }

    #[tokio::test]
    async fn a_call_given_up_counts_as_timed_out_and_failures_as_errors() {
        let meters = Arc::new(Meters::default());
        let slow = Metered::wrap(Arc::new(Slow { delay: Duration::from_secs(30), fail: false }), meters.clone(), "s3");
        let tmp = std::env::temp_dir().join("thirtyfile-meter-missing");
        assert!(tokio::time::timeout(Duration::from_millis(30), slow.put_file(&"b".repeat(64), &tmp)).await.is_err());
        let failing = Metered::wrap(Arc::new(Slow { delay: Duration::ZERO, fail: true }), meters.clone(), "s3");
        assert!(failing.put_file(&"b".repeat(64), &tmp).await.is_err());
        let w = &meters.take()["s3"][&(Op::Write, Work::Foreground)];
        assert_eq!((w.count, w.errors, w.timeouts, w.bytes), (2, 1, 1, 0));
        assert!(meters.active().is_empty(), "nothing left running");
        // Counters that couldn't be written are kept for the next time
        let mut again = Windows::new();
        again.entry("s3".into()).or_default().insert((Op::Write, Work::Foreground), w.clone());
        meters.restore(again.clone());
        meters.restore(again);
        assert_eq!(meters.take()["s3"][&(Op::Write, Work::Foreground)].count, 4);
    }
}
