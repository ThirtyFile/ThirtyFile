//! `/metrics`: who may ask, and the page. Every series is listed in the monitoring guide (site/docs/monitoring.html);
//! a name that has been released is not changed.

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use subtle::ConstantTimeEq;

use super::{Memory, collect::Snapshot, expose::Text, instruments::HTTP_BOUNDS};
use crate::{
    auth::{attempt_succeeded, attempts_exhausted, begin_attempt, client_ip, limit_key_ip},
    state::AppState,
    usage::meter::{Op, Window},
    util::sha256_hex,
};

/// Wrong tokens one address may send in the window of the sign-in limits before it is turned away
const WRONG_TOKENS: usize = 20;

/// The Prometheus text format, version 0.0.4
const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Upper bounds of the histogram of how long a storage operation took, in seconds
const STORAGE_BOUNDS: [f64; 12] = [0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 10.0, 60.0];

pub async fn scrape(State(st): State<AppState>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    let memory = st.part::<Memory>();
    let settings = memory.settings.read().clone();
    // Off: as if there were no such page
    if !settings.enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    let ip = client_ip(&st, addr, &headers);
    let allowed = ip.parse::<std::net::IpAddr>().is_ok_and(|ip| settings.allows(ip));
    if !allowed {
        memory.scrape_outcome("forbidden");
        return StatusCode::FORBIDDEN.into_response();
    }
    let key = format!("metrics:{}", limit_key_ip(&ip));
    if attempts_exhausted(&st, &key, WRONG_TOKENS) {
        memory.scrape_outcome("too_many");
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let given = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).map(str::trim).unwrap_or_default();
    let matches = !given.is_empty() && bool::from(sha256_hex(given.as_bytes()).as_bytes().ct_eq(settings.token_hash.as_bytes()));
    if !matches {
        // Counted like a wrong password: a token can't be guessed by asking again and again
        begin_attempt(&st, &key, WRONG_TOKENS);
        memory.scrape_outcome("unauthorized");
        return (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, "Bearer realm=\"metrics\"")]).into_response();
    }
    attempt_succeeded(&st, &key);
    memory.scrape_outcome("ok");
    ([(header::CONTENT_TYPE, CONTENT_TYPE), (header::CACHE_CONTROL, "no-store")], render(&st)).into_response()
}

/// The page: everything counted, and the figures last looked at
pub fn render(st: &AppState) -> String {
    let memory = st.part::<Memory>();
    let snapshot = memory.snapshot.read().clone();
    let mut t = Text::new();
    process(&mut t, memory, &snapshot);
    requests(&mut t, memory);
    sign_ins(&mut t, memory, &snapshot);
    storage_operations(st, &mut t);
    if snapshot.taken_at > 0 {
        locations(&mut t, &snapshot);
        disks_and_database(&mut t, &snapshot);
        policies_and_queues(&mut t, &snapshot);
    }
    work(st, &mut t, memory);
    t.finish()
}

/// The server itself: its version, how long it has run, and what it holds
fn process(t: &mut Text, memory: &Memory, snapshot: &Snapshot) {
    t.gauge("thirtyfile_build_info", "The version of the server; the value is always 1").sample(&[("version", crate::VERSION)], 1.0);
    t.gauge("thirtyfile_process_start_time_seconds", "When the server started, in seconds since 1970").sample(&[], memory.started_at as f64);
    t.gauge("thirtyfile_process_uptime_seconds", "Seconds since the server started").sample(&[], memory.started.elapsed().as_secs_f64());
    if let Some(v) = snapshot.memory_bytes {
        t.gauge("thirtyfile_process_resident_memory_bytes", "Memory the server holds in RAM (Linux)").sample(&[], v as f64);
    }
    if let Some(v) = snapshot.open_files {
        t.gauge("thirtyfile_process_open_files", "Files and connections the server has open (Linux)").sample(&[], v as f64);
    }
    if let Some(v) = snapshot.threads {
        t.gauge("thirtyfile_process_threads", "Threads of the server (Linux)").sample(&[], v as f64);
    }
}

/// Requests answered
fn requests(t: &mut Text, memory: &Memory) {
    let http = memory.http.figures();
    {
        let mut f = t.counter("thirtyfile_http_requests_total", "Requests answered, by method and class of status");
        for (method, class, n) in &http.requests {
            f.sample(&[("method", method), ("code", class)], *n as f64);
        }
    }
    t.histogram("thirtyfile_http_request_duration_seconds", "Seconds from a request to the start of its answer (a download goes on after it)").histogram(
        &[],
        &HTTP_BOUNDS,
        &http.cumulative,
        http.sum_seconds,
        http.count,
    );
    t.gauge("thirtyfile_http_requests_in_flight", "Requests being answered now").sample(&[], http.in_flight as f64);
}

/// Sign-in events, sessions and accounts, and the requests for this page
fn sign_ins(t: &mut Text, memory: &Memory, snapshot: &Snapshot) {
    {
        let mut f = t.counter("thirtyfile_sign_in_events_total", "Sign-in related events, as the sign-in log names them (login, bad_password, locked, 2fa_failed…)");
        for (event, n) in memory.sign_ins() {
            f.sample(&[("event", event)], n as f64);
        }
    }
    if snapshot.taken_at > 0 {
        t.gauge("thirtyfile_sessions", "Sign-in sessions that have not ended").sample(&[], snapshot.sessions as f64);
        let mut f = t.gauge("thirtyfile_accounts", "Accounts, by whether they can sign in");
        f.sample(&[("state", "active")], snapshot.accounts_active as f64).sample(&[("state", "disabled")], snapshot.accounts_disabled as f64);
    }
    {
        let mut f = t.counter("thirtyfile_metrics_requests_total", "Requests for this page, by how they were answered");
        for (outcome, n) in memory.scrapes() {
            f.sample(&[("outcome", outcome)], n as f64);
        }
    }
}

/// The storage locations: whether they can be used, what they hold, and their disks
fn locations(t: &mut Text, snapshot: &Snapshot) {
    {
        let mut f = t.gauge("thirtyfile_storage_location_up", "1 when a storage location can be used, 0 when it can't");
        for l in &snapshot.locations {
            f.sample(&[("location", &l.name)], if l.online { 1.0 } else { 0.0 });
        }
    }
    for (name, help, pick) in [
        (
            "thirtyfile_storage_location_live_bytes",
            "Bytes of the files in spaces on a storage location",
            (|l: &super::collect::Location| Some(l.live_bytes)) as fn(&super::collect::Location) -> Option<i64>,
        ),
        ("thirtyfile_storage_location_trash_bytes", "Bytes of the files in the trash on a storage location", |l| Some(l.trash_bytes)),
        ("thirtyfile_storage_location_version_bytes", "Bytes of earlier versions on a storage location", |l| Some(l.version_bytes)),
        ("thirtyfile_storage_location_disk_free_bytes", "Free bytes of the disk a storage location is on", |l| l.disk_free),
        ("thirtyfile_storage_location_disk_total_bytes", "Size in bytes of the disk a storage location is on", |l| l.disk_total),
    ] {
        let mut f = t.gauge(name, help);
        for l in &snapshot.locations {
            if let Some(v) = pick(l) {
                f.sample(&[("location", &l.name)], v as f64);
            }
        }
    }
}

/// The disks of the data and storage folders, and the database
fn disks_and_database(t: &mut Text, snapshot: &Snapshot) {
    {
        let mut f = t.gauge("thirtyfile_disk_free_bytes", "Free bytes of the disk the data folder or the storage folder is on");
        for v in &snapshot.volumes {
            f.sample(&[("volume", v.volume)], v.free as f64);
        }
    }
    {
        let mut f = t.gauge("thirtyfile_disk_total_bytes", "Size in bytes of the disk the data folder or the storage folder is on");
        for v in &snapshot.volumes {
            f.sample(&[("volume", v.volume)], v.total as f64);
        }
    }
    t.gauge("thirtyfile_database_size_bytes", "Size of the database file").sample(&[], snapshot.database_bytes as f64);
    t.gauge("thirtyfile_database_wal_size_bytes", "Size of the database's write-ahead log").sample(&[], snapshot.wal_bytes as f64);
}

/// Alerts, backup and replica policies, and what waits in a queue
fn policies_and_queues(t: &mut Text, snapshot: &Snapshot) {
    {
        let mut f = t.gauge("thirtyfile_alerts_firing", "Administrator alerts firing now, by kind");
        for (kind, n) in &snapshot.alerts {
            f.sample(&[("kind", kind)], *n as f64);
        }
    }
    {
        let mut f =
            t.gauge("thirtyfile_backup_last_complete_age_seconds", "Seconds since a backup policy last made a complete snapshot; not there when it has made none");
        for (name, age) in &snapshot.backups {
            if let Some(age) = age {
                f.sample(&[("policy", name)], *age as f64);
            }
        }
    }
    {
        let mut f = t.gauge("thirtyfile_backup_policies", "Backup policies that are on, by whether they have made a complete snapshot");
        let made = snapshot.backups.iter().filter(|(_, a)| a.is_some()).count();
        f.sample(&[("snapshot", "made")], made as f64).sample(&[("snapshot", "none")], (snapshot.backups.len() - made) as f64);
    }
    {
        let mut f =
            t.gauge("thirtyfile_replica_sync_age_seconds", "Seconds since every target of a replica policy was last up to date; not there when one never was");
        for (name, age) in &snapshot.replicas {
            if let Some(age) = age {
                f.sample(&[("policy", name)], *age as f64);
            }
        }
    }
    t.gauge("thirtyfile_content_search_queue", "Files waiting for their text to be read for search").sample(&[], snapshot.search_queued as f64);
    t.gauge("thirtyfile_content_search_failed", "Files whose text could not be read for search").sample(&[], snapshot.search_failed as f64);
    t.gauge("thirtyfile_alert_deliveries_pending", "Alert messages (email, webhook) waiting to be sent").sample(&[], snapshot.deliveries_pending as f64);
    t.gauge("thirtyfile_alert_deliveries_failed", "Alert messages given up on").sample(&[], snapshot.deliveries_failed as f64);
    if let Some(seconds) = snapshot.certificate_seconds {
        t.gauge("thirtyfile_ftps_certificate_expiry_seconds", "Seconds until the FTPS certificate ends; negative when it has").sample(&[], seconds as f64);
    }
}

/// Tasks, background loops and emails
fn work(st: &AppState, t: &mut Text, memory: &Memory) {
    {
        let mut f = t.gauge("thirtyfile_tasks_running", "Tasks running now (compressing, extracting, copying, moving…), by kind");
        let mut running: std::collections::BTreeMap<&str, i64> = std::collections::BTreeMap::new();
        for job in st.part::<crate::jobs::Memory>().jobs.lock().values().filter(|j| j.state == "running") {
            *running.entry(job.kind).or_default() += 1;
        }
        for (kind, n) in running {
            f.sample(&[("kind", kind)], n as f64);
        }
    }
    {
        let mut f = t.counter("thirtyfile_tasks_finished_total", "Tasks that ended, by kind and how");
        for (kind, outcome, n) in memory.tasks() {
            f.sample(&[("kind", kind), ("outcome", outcome)], n as f64);
        }
    }
    {
        let mut f = t.gauge("thirtyfile_background_last_success_timestamp_seconds", "When a background loop last did its work, in seconds since 1970");
        for (job, at) in memory.beats() {
            f.sample(&[("job", job)], at as f64);
        }
    }
    let (sent, failed) = memory.emails();
    t.counter("thirtyfile_emails_total", "Emails sent, by whether the email server took them")
        .sample(&[("outcome", "sent")], sent as f64)
        .sample(&[("outcome", "failed")], failed as f64);
}

/// Storage operations since the server started: counts, bytes, and how long they took, by location and kind
fn storage_operations(st: &AppState, t: &mut Text) {
    let totals = st.part::<crate::usage::Memory>().meters.totals();
    // The location's name is its label; operations on folder spaces that are on no location have none
    let names: std::collections::HashMap<String, String> = st.part::<Memory>().snapshot.read().location_names.clone();
    let name_of = |id: &str| names.get(id).cloned().unwrap_or_else(|| if id.is_empty() { "unplaced".to_string() } else { id.to_string() });
    // Every kind of work (people's, background, tests) together, per kind of operation
    let mut by_location: std::collections::BTreeMap<String, std::collections::BTreeMap<&'static str, Window>> = Default::default();
    for (id, per) in &totals {
        for ((op, _work), w) in per {
            by_location.entry(name_of(id)).or_default().entry(op.as_str()).or_default().merge(w);
        }
    }
    {
        let mut f = t.counter("thirtyfile_storage_operations_total", "Operations on storage locations, by location, kind and how they ended");
        for (location, per) in &by_location {
            for (op, w) in per {
                let ok = w.count.saturating_sub(w.errors + w.timeouts);
                for (outcome, n) in [("ok", ok), ("error", w.errors), ("timeout", w.timeouts)] {
                    f.sample(&[("location", location), ("op", op), ("outcome", outcome)], n as f64);
                }
            }
        }
    }
    {
        let mut f = t.counter("thirtyfile_storage_bytes_total", "Bytes read from and written to storage locations");
        for (location, per) in &by_location {
            for (op, w) in per {
                if matches!(Op::parse(op), Some(Op::Read | Op::Write)) {
                    f.sample(&[("location", location), ("op", op)], w.bytes as f64);
                }
            }
        }
    }
    let bounds_us: Vec<f64> = STORAGE_BOUNDS.iter().map(|s| s * 1e6).collect();
    let mut f = t.histogram("thirtyfile_storage_operation_duration_seconds", "How long operations on storage locations took, by location and kind");
    for (location, per) in &by_location {
        for (op, w) in per {
            f.histogram(&[("location", location), ("op", op)], &STORAGE_BOUNDS, &w.hist.cumulative(&bounds_us), w.total_us as f64 / 1e6, w.count);
        }
    }
}
