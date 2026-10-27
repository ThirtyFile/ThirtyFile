//! What is counted as it happens: requests, sign-in events, tasks finished, the last time each background loop did its
//! work. Cheap on the path of a request (atomics, or a lock held for one addition), and kept in memory: a restart
//! starts the counters again, as Prometheus expects of a process.

use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicI64, AtomicU64, Ordering},
    time::{Duration, Instant},
};

use axum::{
    extract::{Request, State},
    http::Method,
    middleware::Next,
    response::Response,
};

use super::{collect::Snapshot, settings::Settings};
use crate::{
    state::AppState,
    sync::{Mutex, RwLock},
    util::now,
};

/// The request methods counted apart; any other is "other"
pub const METHODS: [&str; 8] = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "other"];
/// The classes of status
pub const CLASSES: [&str; 5] = ["1xx", "2xx", "3xx", "4xx", "5xx"];
/// Upper bounds of the histogram of how long a request took to answer, in seconds
pub const HTTP_BOUNDS: [f64; 14] = [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0];

/// Requests the server answered, counted without a lock
#[derive(Default)]
pub struct Http {
    requests: [[AtomicU64; 5]; 8],
    /// Requests by the first bound they fit under; the last holds those slower than every bound
    buckets: [AtomicU64; HTTP_BOUNDS.len() + 1],
    sum_micros: AtomicU64,
    in_flight: AtomicI64,
}

/// A request in progress: counted until it is dropped
pub struct InFlight<'a>(&'a AtomicI64);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// What `Http` counted
pub struct HttpFigures {
    /// Method, class of status, count (only those that happened)
    pub requests: Vec<(&'static str, &'static str, u64)>,
    /// How many answered under each of `HTTP_BOUNDS`
    pub cumulative: Vec<u64>,
    pub count: u64,
    pub sum_seconds: f64,
    pub in_flight: i64,
}

impl Http {
    pub fn begin(&self) -> InFlight<'_> {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        InFlight(&self.in_flight)
    }

    /// Counts a request that was answered with `status` after `took`
    pub fn finished(&self, method: &Method, status: u16, took: Duration) {
        let m = METHODS.iter().position(|m| *m == method.as_str()).unwrap_or(METHODS.len() - 1);
        let class = usize::from(status / 100).clamp(1, 5) - 1;
        self.requests[m][class].fetch_add(1, Ordering::Relaxed);
        let seconds = took.as_secs_f64();
        let bucket = HTTP_BOUNDS.iter().position(|b| seconds <= *b).unwrap_or(HTTP_BOUNDS.len());
        self.buckets[bucket].fetch_add(1, Ordering::Relaxed);
        self.sum_micros.fetch_add(u64::try_from(took.as_micros()).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    pub fn figures(&self) -> HttpFigures {
        let mut requests = Vec::new();
        for (m, row) in self.requests.iter().enumerate() {
            for (c, n) in row.iter().enumerate() {
                let n = n.load(Ordering::Relaxed);
                if n > 0 {
                    requests.push((METHODS[m], CLASSES[c], n));
                }
            }
        }
        let per_bucket: Vec<u64> = self.buckets.iter().map(|b| b.load(Ordering::Relaxed)).collect();
        let count = per_bucket.iter().sum();
        let mut running = 0;
        let cumulative = per_bucket[..HTTP_BOUNDS.len()]
            .iter()
            .map(|n| {
                running += n;
                running
            })
            .collect();
        HttpFigures {
            requests,
            cumulative,
            count,
            sum_seconds: self.sum_micros.load(Ordering::Relaxed) as f64 / 1e6,
            in_flight: self.in_flight.load(Ordering::Relaxed),
        }
    }
}

/// Counts every request, from the first byte asked to the answer's headers (a download goes on after that)
pub async fn track(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let http = &st.part::<Memory>().http;
    let method = req.method().clone();
    let started = Instant::now();
    let _in_flight = http.begin();
    let res = next.run(req).await;
    http.finished(&method, res.status().as_u16(), started.elapsed());
    res
}

/// What monitoring keeps in memory (a part of `AppState`)
pub struct Memory {
    pub settings: RwLock<Settings>,
    pub http: Http,
    pub(super) started: Instant,
    pub(super) started_at: i64,
    sign_ins: Mutex<BTreeMap<&'static str, u64>>,
    tasks: Mutex<BTreeMap<(&'static str, &'static str), u64>>,
    beats: Mutex<BTreeMap<&'static str, i64>>,
    scrapes: Mutex<BTreeMap<&'static str, u64>>,
    pub(super) snapshot: RwLock<Snapshot>,
}

/// Emails sent and failed since the server started (counted where they are sent, which knows nothing of the state)
static EMAILS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

pub fn email_sent() {
    EMAILS[0].fetch_add(1, Ordering::Relaxed);
}

pub fn email_failed() {
    EMAILS[1].fetch_add(1, Ordering::Relaxed);
}

impl Memory {
    pub fn new(settings: Settings) -> Memory {
        Memory {
            settings: RwLock::new(settings),
            http: Http::default(),
            started: Instant::now(),
            started_at: now(),
            sign_ins: Mutex::default(),
            tasks: Mutex::default(),
            beats: Mutex::default(),
            scrapes: Mutex::default(),
            snapshot: RwLock::default(),
        }
    }

    pub(super) fn sign_ins(&self) -> Vec<(&'static str, u64)> {
        self.sign_ins.lock().iter().map(|(k, v)| (*k, *v)).collect()
    }

    pub(super) fn tasks(&self) -> Vec<(&'static str, &'static str, u64)> {
        self.tasks.lock().iter().map(|((k, o), v)| (*k, *o, *v)).collect()
    }

    pub(super) fn beats(&self) -> Vec<(&'static str, i64)> {
        self.beats.lock().iter().map(|(k, v)| (*k, *v)).collect()
    }

    pub(super) fn scrapes(&self) -> Vec<(&'static str, u64)> {
        self.scrapes.lock().iter().map(|(k, v)| (*k, *v)).collect()
    }

    pub(super) fn emails(&self) -> (u64, u64) {
        (EMAILS[0].load(Ordering::Relaxed), EMAILS[1].load(Ordering::Relaxed))
    }

    pub(super) fn scrape_outcome(&self, outcome: &'static str) {
        *self.scrapes.lock().entry(outcome).or_default() += 1;
    }
}

/// A sign-in related event happened (the names are those of the sign-in log: a fixed set of words)
pub fn sign_in_event(st: &AppState, event: &'static str) {
    *st.part::<Memory>().sign_ins.lock().entry(event).or_default() += 1;
}

/// A task (the kinds of `jobs::Job`) ended
pub fn task_finished(st: &AppState, kind: &'static str, ok: bool) {
    *st.part::<Memory>().tasks.lock().entry((kind, if ok { "done" } else { "failed" })).or_default() += 1;
}

/// A background loop did its work: how long ago is what a monitoring system watches
pub fn beat(st: &AppState, job: &'static str) {
    st.part::<Memory>().beats.lock().insert(job, now());
}
