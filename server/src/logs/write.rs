//! Writing the logs: the activity log (within the caller's transaction), and sign-in, share link and error events
//! (queued and written in batches by a background task).

use axum::http::HeaderMap;
use sqlx::SqliteConnection;

use super::Visitor;
use crate::{
    auth::User,
    error::AppResult,
    state::AppState,
    tree::Node,
    util::now,
};

// ───────────── Background log writer ─────────────

/// One sign-in, share-access or error event, queued for the background writer
pub enum LogEvent {
    ShareAccess { at: i64, share_id: String, owner_id: i64, node_id: Option<String>, node_name: String, event: &'static str, ip: String, user_agent: String },
    Login { at: i64, user_id: Option<i64>, username: String, event: &'static str, method: String, ip: String, user_agent: String },
    Error(Box<super::ErrorEvent>),
}

/// Queue capacity: beyond this, events are dropped (counted in the log) rather than piling up tasks waiting for the write lock
const QUEUE: usize = 4096;
pub(super) const BATCH: usize = 200;

pub fn channel() -> (tokio::sync::mpsc::Sender<LogEvent>, tokio::sync::mpsc::Receiver<LogEvent>) {
    tokio::sync::mpsc::channel(QUEUE)
}

/// Handle of the background log writer: `finish()` writes whatever is still queued before the process exits
pub struct LogWriter {
    stop: std::sync::Arc<tokio::sync::Notify>,
    handle: tokio::task::JoinHandle<()>,
}

impl LogWriter {
    /// Ask the writer to flush the queue and stop; waits a few seconds at most
    pub async fn finish(self) {
        self.stop.notify_one();
        if tokio::time::timeout(std::time::Duration::from_secs(5), self.handle).await.is_err() {
            tracing::warn!("Log writer didn't finish in time; some sign-in or share access events may be lost");
        }
    }
}

/// Writes queued events in batches: one transaction per batch, so heavy anonymous traffic on a share link costs one
/// write-lock acquisition per batch instead of one queued task per request
pub fn spawn_writer(st: AppState, mut rx: tokio::sync::mpsc::Receiver<LogEvent>) -> LogWriter {
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let stop_signal = stop.clone();
    let handle = tokio::spawn(async move {
        let mut stopping = false;
        loop {
            let first = if stopping {
                match rx.try_recv() {
                    Ok(e) => e,
                    Err(_) => break,
                }
            } else {
                tokio::select! {
                    e = rx.recv() => match e {
                        Some(e) => e,
                        None => break,
                    },
                    _ = stop_signal.notified() => {
                        stopping = true;
                        continue;
                    }
                }
            };
            let mut batch = vec![first];
            while batch.len() < BATCH {
                match rx.try_recv() {
                    Ok(e) => batch.push(e),
                    Err(_) => break,
                }
            }
            write_batch(&st, &batch).await;
        }
    });
    LogWriter { stop, handle }
}

async fn write_batch(st: &AppState, batch: &[LogEvent]) {
    let _w = st.write_lock.lock().await;
    let written: Result<(), sqlx::Error> = async {
        let mut tx = crate::db::begin_write(&st.db).await?;
        for e in batch {
            match e {
                LogEvent::ShareAccess { at, share_id, owner_id, node_id, node_name, event, ip, user_agent } => {
                    sqlx::query(
                        "INSERT INTO share_access (at, share_id, owner_id, node_id, node_name, event, ip, user_agent) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                    )
                    .bind(at)
                    .bind(share_id)
                    .bind(owner_id)
                    .bind(node_id)
                    .bind(node_name)
                    .bind(event)
                    .bind(ip)
                    .bind(user_agent)
                    .execute(&mut *tx)
                    .await?;
                }
                LogEvent::Login { at, user_id, username, event, method, ip, user_agent } => {
                    sqlx::query("INSERT INTO login_log (at, user_id, username, event, ip, user_agent, method) VALUES (?, ?, ?, ?, ?, ?, ?)")
                        .bind(at)
                        .bind(user_id)
                        .bind(username)
                        .bind(event)
                        .bind(ip)
                        .bind(user_agent)
                        .bind(method)
                        .execute(&mut *tx)
                        .await?;
                }
                LogEvent::Error(e) => super::errors::write_error(&mut tx, st, e).await?,
            }
        }
        tx.commit().await
    }
    .await;
    if let Err(e) = written {
        tracing::warn!("Failed to write {} log events: {e}", batch.len());
    }
}

pub(super) fn enqueue(st: &AppState, event: LogEvent) {
    static DROPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if st.log_tx.try_send(event).is_err() {
        let n = DROPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        if n == 1 || n.is_multiple_of(1000) {
            tracing::warn!("Log queue full: {n} events dropped so far");
        }
    }
}

/// Page views of a link from one address closer together than this (seconds) are logged once
const VIEW_INTERVAL: i64 = 60;

/// Whether a page view of `share_id` from `ip` should be logged: the first one, or the first after a minute of quiet
pub fn first_view_in_a_while(st: &AppState, share_id: &str, ip: &str) -> bool {
    let mut views = st.share_views.lock().unwrap();
    let t = now();
    let key = format!("{share_id}|{ip}");
    match views.get(&key) {
        Some(last) if t - *last < VIEW_INTERVAL => false,
        _ => {
            views.insert(key, t);
            true
        }
    }
}

/// Drops the view records that no longer suppress anything (hourly, so the map can't grow without bound)
pub fn prune_share_views(st: &AppState) {
    let cutoff = now() - VIEW_INTERVAL;
    st.share_views.lock().unwrap().retain(|_, t| *t > cutoff);
}

/// Records one access to a share link (queued; downloads aren't slowed down)
pub fn record_share_access(st: &AppState, share_id: &str, owner_id: i64, node: Option<&Node>, event: &'static str, v: &Visitor) {
    enqueue(
        st,
        LogEvent::ShareAccess {
            at: now(),
            share_id: share_id.to_string(),
            owner_id,
            node_id: node.map(|n| n.id.clone()),
            node_name: node.map(|n| n.name.clone()).unwrap_or_default(),
            event,
            ip: v.ip.clone(),
            user_agent: v.user_agent.clone(),
        },
    );
}

/// Records an activity (within the caller's transaction, unlike sign-in and share link events)
pub async fn record_activity(conn: &mut SqliteConnection, user: &User, node: Option<&Node>, action: &str, detail: &str) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO activity (at, user_id, username, drive_id, node_id, node_name, action, detail) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(now())
    .bind(user.id)
    .bind(&user.username)
    .bind(node.and_then(|n| n.drive_id.clone()))
    .bind(node.map(|n| n.id.clone()))
    .bind(node.map(|n| n.name.clone()).unwrap_or_default())
    .bind(action)
    .bind(detail)
    .execute(conn)
    .await?;
    Ok(())
}

/// Records one sign-in related event (written in the background so sign-in isn't slowed down)
pub fn record_login(st: &AppState, user_id: Option<i64>, username: &str, event: &'static str, ip: &str, headers: &HeaderMap) {
    record_login_via(st, user_id, username, event, "password", ip, headers);
}

/// Same as record_login, also recording the sign-in method (password, microsoft, google, github)
pub fn record_login_via(st: &AppState, user_id: Option<i64>, username: &str, event: &'static str, method: &str, ip: &str, headers: &HeaderMap) {
    let user_agent = crate::auth::user_agent(headers);
    enqueue(
        st,
        LogEvent::Login {
            at: now(),
            user_id,
            username: username.trim().chars().take(64).collect(),
            event,
            method: method.to_string(),
            ip: ip.to_string(),
            user_agent,
        },
    );
}
