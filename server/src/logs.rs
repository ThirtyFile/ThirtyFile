//! Logs: querying and exporting the activity log, share link visit log, sign-in log, log settings, and periodic compressed archiving.
//!
//! The database keeps only recent records (180 days by default); older ones are compressed daily into `data/archives/*.jsonl.gz`
//! (one JSON record per line, importable with any text tool or spreadsheet). Archive files have their own retention period, so data doesn't grow forever.

use std::{
    io::Write,
    net::SocketAddr,
    path::PathBuf,
};

use axum::{
    Json,
    body::Body,
    extract::{ConnectInfo, FromRequestParts, Path, Query, State},
    http::{HeaderMap, HeaderValue, header, request::Parts},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

use crate::{
    auth::{Admin, User},
    db::{get_setting, set_setting},
    error::{AppError, AppResult},
    state::AppState,
    tree::{self, Node},
    util::{content_disposition, format_bytes, now},
};

const DAY: i64 = 86_400;
/// Maximum records per archive file (large volumes are split into several files)
const ARCHIVE_BATCH: i64 = 20_000;
/// Row limit for CSV exports
const EXPORT_LIMIT: i64 = 100_000;

// ───────────── Settings ─────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LogSettings {
    /// Days to keep activity log records in the database; 0 = never clean up automatically
    pub activity_days: i64,
    /// Days to keep share link visit records; 0 = never clean up automatically
    pub share_days: i64,
    /// Days to keep sign-in records; 0 = never clean up automatically
    pub login_days: i64,
    /// Records past the retention period: true = compress into an archive and remove from the database; false = delete directly
    pub archive: bool,
    /// Days to keep archive files; 0 = keep forever
    pub archive_keep_days: i64,
    /// Whether share link visit records include the visitor's IP and browser
    pub record_visitor: bool,
}

impl Default for LogSettings {
    fn default() -> Self {
        Self { activity_days: 180, share_days: 180, login_days: 365, archive: true, archive_keep_days: 1095, record_visitor: true }
    }
}

pub async fn load_settings(db: &SqlitePool) -> LogSettings {
    match get_setting(db, "log_settings").await {
        Ok(Some(v)) => serde_json::from_str(&v).unwrap_or_default(),
        _ => LogSettings::default(),
    }
}

// ───────────── Visitors (for the share link visit log) ─────────────

/// A visitor to a public share page: IP and browser (may not be recorded, depending on log settings)
pub struct Visitor {
    pub ip: String,
    pub user_agent: String,
}

impl FromRequestParts<AppState> for Visitor {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        if !st.logs.read().unwrap().record_visitor {
            return Ok(Visitor { ip: String::new(), user_agent: String::new() });
        }
        let addr = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0);
        let ip = match addr {
            Some(a) => crate::auth::client_ip(st, a, &parts.headers),
            None => String::new(),
        };
        let user_agent = parts.headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or_default().chars().take(300).collect();
        Ok(Visitor { ip, user_agent })
    }
}

// ───────────── Background log writer ─────────────

/// One sign-in or share-access event, queued for the background writer
pub enum LogEvent {
    ShareAccess { at: i64, share_id: String, owner_id: i64, node_id: Option<String>, node_name: String, event: &'static str, ip: String, user_agent: String },
    Login { at: i64, user_id: Option<i64>, username: String, event: &'static str, method: String, ip: String, user_agent: String },
}

/// Queue capacity: beyond this, events are dropped (counted in the log) rather than piling up tasks waiting for the write lock
const QUEUE: usize = 4096;
const BATCH: usize = 200;

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
    {
            let _w = st.write_lock.lock().await;
            let written: Result<(), sqlx::Error> = async {
                let mut tx = st.db.begin().await?;
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
                    }
                }
                tx.commit().await
            }
            .await;
            if let Err(e) = written {
                tracing::warn!("Failed to write {} log events: {e}", batch.len());
            }
    }
}

fn enqueue(st: &AppState, event: LogEvent) {
    static DROPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if st.log_tx.try_send(event).is_err() {
        let n = DROPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        if n == 1 || n.is_multiple_of(1000) {
            tracing::warn!("Log queue full: {n} events dropped so far");
        }
    }
}

/// Records one access to a share link (queued; downloads aren't slowed down)
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

// ───────────── Activity log queries ─────────────

#[derive(Serialize, sqlx::FromRow)]
pub struct ActivityRow {
    id: i64,
    at: i64,
    username: String,
    drive_id: Option<String>,
    drive_name: Option<String>,
    node_id: Option<String>,
    node_name: String,
    action: String,
    detail: String,
}

#[derive(Deserialize, Default)]
pub struct ActivityQuery {
    drive_id: Option<String>,
    /// Username (partial match)
    user: Option<String>,
    /// Actions, comma-separated
    action: Option<String>,
    /// Time range (Unix seconds, start inclusive, end exclusive)
    from: Option<i64>,
    to: Option<i64>,
    /// Keyword: item name or details
    q: Option<String>,
    /// Paging: only records with an id below this value
    before: Option<i64>,
    limit: Option<i64>,
    /// Time zone for exports (minutes, same as JavaScript's getTimezoneOffset)
    tz: Option<i64>,
}

fn like(s: &str) -> String {
    format!("%{}%", s.trim().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
}

fn activity_filters(qb: &mut QueryBuilder<Sqlite>, q: &ActivityQuery) {
    qb.push(" WHERE 1 = 1");
    if let Some(d) = &q.drive_id {
        qb.push(" AND a.drive_id = ").push_bind(d.as_str());
    }
    if let Some(u) = q.user.as_deref().filter(|u| !u.trim().is_empty()) {
        qb.push(" AND a.username LIKE ").push_bind(like(u)).push(" ESCAPE '\\'");
    }
    let actions: Vec<&str> = q.action.as_deref().unwrap_or_default().split(',').map(str::trim).filter(|a| !a.is_empty()).collect();
    if !actions.is_empty() {
        qb.push(" AND a.action IN (");
        let mut sep = qb.separated(", ");
        for a in actions {
            sep.push_bind(a);
        }
        qb.push(")");
    }
    if let Some(f) = q.from {
        qb.push(" AND a.at >= ").push_bind(f);
    }
    if let Some(t) = q.to {
        qb.push(" AND a.at < ").push_bind(t);
    }
    if let Some(k) = q.q.as_deref().filter(|k| !k.trim().is_empty()) {
        let pat = like(k);
        qb.push(" AND (a.node_name LIKE ").push_bind(pat.clone()).push(" ESCAPE '\\' OR a.detail LIKE ").push_bind(pat).push(" ESCAPE '\\')");
    }
    if let Some(b) = q.before {
        qb.push(" AND a.id < ").push_bind(b);
    }
}

async fn authorize_activity(st: &AppState, user: &User, q: &ActivityQuery) -> AppResult<()> {
    if let Some(drive_id) = &q.drive_id {
        crate::drives::manageable_drive(&mut *st.db.acquire().await?, user, drive_id).await?;
        Ok(())
    } else if user.is_admin() {
        Ok(())
    } else {
        Err(AppError::forbidden("Administrator permission required"))
    }
}

async fn query_activity(st: &AppState, q: &ActivityQuery, limit: i64) -> AppResult<Vec<ActivityRow>> {
    let mut qb = QueryBuilder::<Sqlite>::new(
        "SELECT a.id, a.at, a.username, a.drive_id, d.name AS drive_name, a.node_id, a.node_name, a.action, a.detail
         FROM activity a LEFT JOIN drives d ON d.id = a.drive_id",
    );
    activity_filters(&mut qb, q);
    qb.push(" ORDER BY a.id DESC LIMIT ").push_bind(limit);
    Ok(qb.build_query_as().fetch_all(&st.db).await?)
}

/// With a space: visible to the space's managers; without: administrators see everything. The returned next loads the following page
pub async fn activity(State(st): State<AppState>, user: User, Query(q): Query<ActivityQuery>) -> AppResult<Json<Value>> {
    authorize_activity(&st, &user, &q).await?;
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let items = query_activity(&st, &q, limit).await?;
    let next = (items.len() as i64 == limit).then(|| items.last().map(|r| r.id)).flatten();
    Ok(Json(json!({ "items": items, "next": next })))
}

/// Whether exports should be in English: the interface language set by the frontend (`tf_lang` cookie) is English.
/// Otherwise exported files are in Traditional Chinese (see [`ZH_TW`]).
fn english(headers: &HeaderMap) -> bool {
    crate::auth::get_cookie(headers, "tf_lang") == Some("en")
}

/// Localization data for exported files (CSV headers, action / event / method labels, file names):
/// English source text → Traditional Chinese (zh-TW). Used when the interface language isn't English.
const ZH_TW: &[(&str, &str)] = &[
    // CSV column headers
    ("Time", "時間"),
    ("User", "使用者"),
    ("Action", "動作"),
    ("Item", "項目"),
    ("Details", "細節"),
    ("Space", "空間"),
    ("Account", "帳號"),
    ("Event", "事件"),
    ("Method", "方式"),
    ("Browser", "瀏覽器"),
    // File names
    ("activity-log", "活動紀錄"),
    ("share-access-log", "分享訪問紀錄"),
    ("login-log", "登入紀錄"),
    // Activity actions
    ("Upload", "上傳"),
    ("Create folder", "建立資料夾"),
    ("Rename", "重新命名"),
    ("Move", "移動"),
    ("Copy", "複製"),
    ("Move to trash", "移至垃圾桶"),
    ("Restore", "還原"),
    ("Delete permanently", "永久刪除"),
    ("Empty trash", "清空垃圾桶"),
    ("Edit", "編輯"),
    ("Grant access", "授予存取權"),
    ("Remove access", "移除存取權"),
    ("Create share link", "建立分享連結"),
    ("Disable share link", "停用分享連結"),
    ("Create space", "建立空間"),
    ("Update space", "更新空間"),
    ("Delete space", "刪除空間"),
    ("Change storage location", "變更儲存位置"),
    ("Create group", "建立群組"),
    ("Update group", "更新群組"),
    ("Delete group", "刪除群組"),
    ("Add user", "新增使用者"),
    ("Edit user", "修改使用者"),
    ("Delete user", "刪除使用者"),
    ("Add storage location", "新增儲存位置"),
    ("Edit storage location", "修改儲存位置"),
    ("Delete storage location", "刪除儲存位置"),
    ("Set default storage location", "設定預設儲存位置"),
    ("System settings", "系統設定"),
    ("Archive logs", "封存紀錄"),
    ("Delete archive", "刪除封存檔"),
    // Sign-in events and methods
    ("Signed in", "登入成功"),
    ("Wrong password", "密碼錯誤"),
    ("Unknown account", "帳號不存在"),
    ("Account disabled", "帳號已停用"),
    ("Temporarily locked", "暫時鎖定"),
    ("Signed out", "登出"),
    ("Password changed", "變更密碼"),
    ("SSO sign-in denied", "三方登入被拒"),
    ("External account linked", "連結外部帳號"),
    ("External account unlinked", "取消連結外部帳號"),
    ("Device signed out", "登出裝置"),
    ("Signed out on other devices", "登出其他裝置"),
    ("Signed out by an administrator", "由管理員登出"),
    ("Wrong app password", "應用程式密碼錯誤"),
    ("App password created", "建立應用程式密碼"),
    ("App password removed", "移除應用程式密碼"),
    ("App password", "應用程式密碼"),
    ("Wrong two-factor code", "兩步驟驗證碼錯誤"),
    ("Two-factor sign-in turned on", "開啟兩步驟驗證"),
    ("Two-factor sign-in turned off", "關閉兩步驟驗證"),
    ("Two-factor sign-in reset by an administrator", "管理員重設兩步驟驗證"),
    ("Recovery code used", "使用復原碼"),
    ("New recovery codes", "產生新的復原碼"),
    ("Password", "帳號密碼"),
];

/// Localizes export text: English is returned as is; otherwise the zh-TW rendering (text missing from the table stays English)
fn localize(s: &str, en: bool) -> &str {
    if en {
        return s;
    }
    ZH_TW.iter().find(|(k, _)| *k == s).map_or(s, |&(_, v)| v)
}

/// Exports matching activity log entries (CSV with a BOM, so Excel opens non-ASCII text correctly)
pub async fn export_activity(State(st): State<AppState>, user: User, headers: HeaderMap, Query(q): Query<ActivityQuery>) -> AppResult<Response> {
    let en = english(&headers);
    authorize_activity(&st, &user, &q).await?;
    let rows = query_activity(&st, &q, EXPORT_LIMIT).await?;
    let offset = -tz_minutes(q.tz) * 60;
    let mut out = format!("\u{feff}{}\n", ["Time", "User", "Action", "Item", "Details", "Space"].map(|h| localize(h, en)).join(","));
    for r in rows {
        let action = localize(action_label(&r.action), en);
        let fields = [format_time(r.at, offset), r.username, action.to_string(), r.node_name, r.detail, r.drive_name.unwrap_or_default()];
        out.push_str(&fields.iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    let name = format!("{}-{}.csv", localize("activity-log", en), format_time(now(), offset).get(..10).unwrap_or("").replace('-', ""));
    Ok((
        [(header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()), (header::CONTENT_DISPOSITION, content_disposition("attachment", &name))],
        out,
    )
        .into_response())
}

/// Action names in exports (matching the names shown in the web interface)
fn action_label(a: &str) -> &str {
    match a {
        "upload" => "Upload",
        "create_folder" => "Create folder",
        "rename" => "Rename",
        "move" => "Move",
        "copy" => "Copy",
        "trash" => "Move to trash",
        "restore" => "Restore",
        "delete" => "Delete permanently",
        "empty_trash" => "Empty trash",
        "edit" => "Edit",
        "grant" => "Grant access",
        "revoke" => "Remove access",
        "share_create" => "Create share link",
        "share_delete" => "Disable share link",
        "drive_create" => "Create space",
        "drive_update" => "Update space",
        "drive_delete" => "Delete space",
        "drive_location" => "Change storage location",
        "group_create" => "Create group",
        "group_update" => "Update group",
        "group_delete" => "Delete group",
        "user_create" => "Add user",
        "user_update" => "Edit user",
        "user_delete" => "Delete user",
        "storage_create" => "Add storage location",
        "storage_update" => "Edit storage location",
        "storage_delete" => "Delete storage location",
        "storage_default" => "Set default storage location",
        "settings" => "System settings",
        "log_archive" => "Archive logs",
        "log_archive_delete" => "Delete archive",
        other => other,
    }
}

fn csv_field(s: &str) -> String {
    // Content starting with = + - @ may be run as a formula in spreadsheets, so prefix it with a single quote
    let safe = if s.starts_with(['=', '+', '-', '@']) { format!("'{s}") } else { s.to_string() };
    if safe.contains([',', '"', '\n', '\r']) { format!("\"{}\"", safe.replace('"', "\"\"")) } else { safe }
}

/// Unix seconds + time zone offset → YYYY-MM-DD HH:MM:SS
/// The client's UTC offset in minutes, kept within the real range so the arithmetic on timestamps can't overflow
fn tz_minutes(tz: Option<i64>) -> i64 {
    tz.unwrap_or(0).clamp(-14 * 60, 14 * 60)
}

pub fn format_time(ts: i64, offset: i64) -> String {
    let t = ts + offset;
    let (days, secs) = (t.div_euclid(DAY), t.rem_euclid(DAY));
    // Gregorian calendar conversion (Howard Hinnant's civil_from_days)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", secs / 3600, secs % 3600 / 60, secs % 60)
}

// ───────────── Share link visit log ─────────────

#[derive(Serialize, sqlx::FromRow)]
pub struct AccessRow {
    id: i64,
    at: i64,
    share_id: String,
    owner_name: Option<String>,
    node_id: Option<String>,
    node_name: String,
    event: String,
    ip: String,
    user_agent: String,
}

#[derive(Deserialize, Default)]
pub struct AccessQuery {
    share_id: Option<String>,
    /// Sharer's username (partial match, for administrators)
    owner: Option<String>,
    event: Option<String>,
    ip: Option<String>,
    /// Keyword: item name or link token
    q: Option<String>,
    from: Option<i64>,
    to: Option<i64>,
    before: Option<i64>,
    limit: Option<i64>,
}

async fn query_access(st: &AppState, q: &AccessQuery, owner_id: Option<i64>, limit: i64) -> AppResult<Vec<AccessRow>> {
    let mut qb = QueryBuilder::<Sqlite>::new(
        "SELECT a.id, a.at, a.share_id, u.username AS owner_name, a.node_id, a.node_name, a.event, a.ip, a.user_agent
         FROM share_access a LEFT JOIN users u ON u.id = a.owner_id WHERE 1 = 1",
    );
    if let Some(o) = owner_id {
        qb.push(" AND a.owner_id = ").push_bind(o);
    }
    if let Some(s) = &q.share_id {
        qb.push(" AND a.share_id = ").push_bind(s.as_str());
    }
    if let Some(o) = q.owner.as_deref().filter(|o| !o.trim().is_empty()) {
        qb.push(" AND u.username LIKE ").push_bind(like(o)).push(" ESCAPE '\\'");
    }
    let events: Vec<&str> = q.event.as_deref().unwrap_or_default().split(',').map(str::trim).filter(|e| !e.is_empty()).collect();
    if !events.is_empty() {
        qb.push(" AND a.event IN (");
        let mut sep = qb.separated(", ");
        for e in events {
            sep.push_bind(e);
        }
        qb.push(")");
    }
    if let Some(ip) = q.ip.as_deref().filter(|i| !i.trim().is_empty()) {
        qb.push(" AND a.ip LIKE ").push_bind(like(ip)).push(" ESCAPE '\\'");
    }
    if let Some(k) = q.q.as_deref().filter(|k| !k.trim().is_empty()) {
        let pat = like(k);
        qb.push(" AND (a.node_name LIKE ").push_bind(pat.clone()).push(" ESCAPE '\\' OR a.share_id LIKE ").push_bind(pat).push(" ESCAPE '\\')");
    }
    if let Some(f) = q.from {
        qb.push(" AND a.at >= ").push_bind(f);
    }
    if let Some(t) = q.to {
        qb.push(" AND a.at < ").push_bind(t);
    }
    if let Some(b) = q.before {
        qb.push(" AND a.id < ").push_bind(b);
    }
    qb.push(" ORDER BY a.id DESC LIMIT ").push_bind(limit);
    Ok(qb.build_query_as().fetch_all(&st.db).await?)
}

/// Share link access records: standard users only see links they created; administrators can query everything
pub async fn share_access(State(st): State<AppState>, user: User, Query(q): Query<AccessQuery>) -> AppResult<Json<Value>> {
    // Standard users can only query links they created (including records left by deleted links)
    let owner = if user.is_admin() { None } else { Some(user.id) };
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let items = query_access(&st, &q, owner, limit).await?;
    let next = (items.len() as i64 == limit).then(|| items.last().map(|r| r.id)).flatten();
    Ok(Json(json!({ "items": items, "next": next })))
}

// ───────────── Sign-in log ─────────────

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

#[derive(Serialize, sqlx::FromRow)]
pub struct LoginRow {
    id: i64,
    at: i64,
    user_id: Option<i64>,
    username: String,
    event: String,
    ip: String,
    user_agent: String,
    method: String,
}

#[derive(Deserialize, Default)]
pub struct LoginQuery {
    user_id: Option<i64>,
    /// Username (partial match)
    user: Option<String>,
    event: Option<String>,
    ip: Option<String>,
    from: Option<i64>,
    to: Option<i64>,
    before: Option<i64>,
    limit: Option<i64>,
    tz: Option<i64>,
}

async fn query_logins(st: &AppState, q: &LoginQuery, limit: i64) -> AppResult<Vec<LoginRow>> {
    let mut qb = QueryBuilder::<Sqlite>::new("SELECT id, at, user_id, username, event, ip, user_agent, method FROM login_log WHERE 1 = 1");
    if let Some(id) = q.user_id {
        qb.push(" AND user_id = ").push_bind(id);
    }
    if let Some(u) = q.user.as_deref().filter(|u| !u.trim().is_empty()) {
        qb.push(" AND username LIKE ").push_bind(like(u)).push(" ESCAPE '\\'");
    }
    let events: Vec<&str> = q.event.as_deref().unwrap_or_default().split(',').map(str::trim).filter(|e| !e.is_empty()).collect();
    if !events.is_empty() {
        qb.push(" AND event IN (");
        let mut sep = qb.separated(", ");
        for e in events {
            sep.push_bind(e);
        }
        qb.push(")");
    }
    if let Some(ip) = q.ip.as_deref().filter(|i| !i.trim().is_empty()) {
        qb.push(" AND ip LIKE ").push_bind(like(ip)).push(" ESCAPE '\\'");
    }
    if let Some(f) = q.from {
        qb.push(" AND at >= ").push_bind(f);
    }
    if let Some(t) = q.to {
        qb.push(" AND at < ").push_bind(t);
    }
    if let Some(b) = q.before {
        qb.push(" AND id < ").push_bind(b);
    }
    qb.push(" ORDER BY id DESC LIMIT ").push_bind(limit);
    Ok(qb.build_query_as().fetch_all(&st.db).await?)
}

/// Standard users can only see their own sign-in records; administrators can query everything
fn scope_logins(user: &User, q: &mut LoginQuery) {
    if !user.is_admin() {
        q.user_id = Some(user.id);
        q.user = None;
    }
}

pub async fn login_log(State(st): State<AppState>, user: User, Query(mut q): Query<LoginQuery>) -> AppResult<Json<Value>> {
    scope_logins(&user, &mut q);
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let items = query_logins(&st, &q, limit).await?;
    let next = (items.len() as i64 == limit).then(|| items.last().map(|r| r.id)).flatten();
    Ok(Json(json!({ "items": items, "next": next })))
}

pub async fn export_login_log(State(st): State<AppState>, user: User, headers: HeaderMap, Query(mut q): Query<LoginQuery>) -> AppResult<Response> {
    let en = english(&headers);
    scope_logins(&user, &mut q);
    let rows = query_logins(&st, &q, EXPORT_LIMIT).await?;
    let offset = -tz_minutes(q.tz) * 60;
    let mut out = format!("\u{feff}{}\n", ["Time", "Account", "Event", "Method", "IP", "Browser"].map(|h| localize(h, en)).join(","));
    for r in rows {
        let (event, method) = (localize(login_event_label(&r.event), en), localize(login_method_label(&r.method), en));
        let fields = [format_time(r.at, offset), r.username, event.to_string(), method.to_string(), r.ip, r.user_agent];
        out.push_str(&fields.iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    let name = format!("{}-{}.csv", localize("login-log", en), format_time(now(), offset).get(..10).unwrap_or("").replace('-', ""));
    Ok((
        [(header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()), (header::CONTENT_DISPOSITION, content_disposition("attachment", &name))],
        out,
    )
        .into_response())
}

fn login_method_label(m: &str) -> &str {
    match m {
        "password" => "Password",
        "microsoft" => "Microsoft",
        "google" => "Google",
        "github" => "GitHub",
        "app_password" => "App password",
        other => other,
    }
}

fn login_event_label(e: &str) -> &str {
    match e {
        "login" => "Signed in",
        "bad_password" => "Wrong password",
        "unknown_user" => "Unknown account",
        "disabled" => "Account disabled",
        "locked" => "Temporarily locked",
        "logout" => "Signed out",
        "password_change" => "Password changed",
        "sso_denied" => "SSO sign-in denied",
        "sso_provisioned" => "Account created by SSO",
        "sso_link" => "External account linked",
        "sso_unlink" => "External account unlinked",
        "device_signout" => "Device signed out",
        "signout_others" => "Signed out on other devices",
        "admin_signout" => "Signed out by an administrator",
        "app_password_failed" => "Wrong app password",
        "app_password_created" => "App password created",
        "app_password_revoked" => "App password removed",
        "2fa_failed" => "Wrong two-factor code",
        "2fa_enabled" => "Two-factor sign-in turned on",
        "2fa_disabled" => "Two-factor sign-in turned off",
        "2fa_reset" => "Two-factor sign-in reset by an administrator",
        "recovery_code_used" => "Recovery code used",
        "recovery_codes_new" => "New recovery codes",
        other => other,
    }
}

// ───────────── Log settings and archiving ─────────────

#[derive(Serialize, sqlx::FromRow)]
pub struct ArchiveRow {
    id: i64,
    kind: String,
    from_at: i64,
    to_at: i64,
    rows: i64,
    bytes: i64,
    created_at: i64,
}

async fn status(st: &AppState) -> AppResult<Value> {
    let (activity_rows, activity_oldest): (i64, Option<i64>) = sqlx::query_as("SELECT COUNT(*), MIN(at) FROM activity").fetch_one(&st.db).await?;
    let (share_rows, share_oldest): (i64, Option<i64>) = sqlx::query_as("SELECT COUNT(*), MIN(at) FROM share_access").fetch_one(&st.db).await?;
    let (login_rows, login_oldest): (i64, Option<i64>) = sqlx::query_as("SELECT COUNT(*), MIN(at) FROM login_log").fetch_one(&st.db).await?;
    let archives: Vec<ArchiveRow> = sqlx::query_as(
        "SELECT id, kind, from_at, to_at, rows, bytes, created_at FROM log_archives ORDER BY to_at DESC, id DESC",
    )
    .fetch_all(&st.db)
    .await?;
    let last_run: Option<i64> = get_setting(&st.db, "log_archived_at").await?.and_then(|v| v.parse().ok());
    Ok(json!({
        "settings": *st.logs.read().unwrap(),
        "activity": { "rows": activity_rows, "oldest": activity_oldest },
        "share_access": { "rows": share_rows, "oldest": share_oldest },
        "login_log": { "rows": login_rows, "oldest": login_oldest },
        "archives": archives,
        "archive_bytes": archives.iter().map(|a| a.bytes).sum::<i64>(),
        "last_run": last_run,
    }))
}

pub async fn get_status(State(st): State<AppState>, _: Admin) -> AppResult<Json<Value>> {
    Ok(Json(status(&st).await?))
}

pub async fn update_settings(State(st): State<AppState>, Admin(user): Admin, Json(s): Json<LogSettings>) -> AppResult<Json<Value>> {
    for (label, v) in [
        ("Activity log retention (days)", s.activity_days),
        ("Share link visit log retention (days)", s.share_days),
        ("Sign-in log retention (days)", s.login_days),
        ("Archive retention (days)", s.archive_keep_days),
    ] {
        if !(0..=36_500).contains(&v) {
            return Err(AppError::bad_request(format!("{label} must be between 0 and 36500")));
        }
    }
    {
        let _w = st.write_lock.lock().await;
        let mut tx = st.db.begin().await?;
        set_setting(&mut tx, "log_settings", &serde_json::to_string(&s).unwrap()).await?;
        let days = |d: i64| if d == 0 { "never cleaned up".to_string() } else { plural(d, "day", "days") };
        let detail = format!(
            "Log settings: activity {}, share visits {}, sign-ins {}, {}, archives {}{}",
            days(s.activity_days),
            days(s.share_days),
            days(s.login_days),
            if s.archive { "compress and archive" } else { "delete directly" },
            if s.archive_keep_days == 0 { "kept forever".to_string() } else { format!("kept {}", plural(s.archive_keep_days, "day", "days")) },
            if s.record_visitor { "" } else { ", visitor IPs not recorded" }
        );
        tree::log(&mut tx, &user, None, "settings", &detail).await?;
        tx.commit().await?;
    }
    *st.logs.write().unwrap() = s;
    Ok(Json(status(&st).await?))
}

/// Runs archiving immediately (without waiting for the daily schedule)
pub async fn archive_now(State(st): State<AppState>, Admin(user): Admin) -> AppResult<Json<Value>> {
    let summary = run_archive(&st).await?;
    {
        let _w = st.write_lock.lock().await;
        let mut tx = st.db.begin().await?;
        tree::log(&mut tx, &user, None, "log_archive", &summary.describe()).await?;
        tx.commit().await?;
    }
    let mut v = status(&st).await?;
    v["summary"] = json!(summary.describe());
    Ok(Json(v))
}

pub async fn download_archive(State(st): State<AppState>, _: Admin, headers: HeaderMap, Path(id): Path<i64>) -> AppResult<Response> {
    let (kind, file, from_at, to_at): (String, String, i64, i64) =
        sqlx::query_as("SELECT kind, file, from_at, to_at FROM log_archives WHERE id = ?").bind(id).fetch_optional(&st.db).await?.ok_or_else(|| AppError::not_found("Archive not found"))?;
    let path = archive_dir(&st).join(&file);
    let f = tokio::fs::File::open(&path).await.map_err(|_| AppError::not_found("The archive file no longer exists"))?;
    let slug = match kind.as_str() {
        "activity" => "activity-log",
        "share_access" => "share-access-log",
        _ => "login-log",
    };
    let label = localize(slug, english(&headers));
    let name = format!("{label}-{}-{}.jsonl.gz", &format_time(from_at, 0)[..10], &format_time(to_at, 0)[..10]);
    // The length up front lets the page show progress, and hand a large archive to the browser before downloading it
    let len = f.metadata().await.map_err(AppError::internal)?.len();
    let mut res = Response::new(Body::from_stream(tokio_util::io::ReaderStream::new(f)));
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/gzip"));
    res.headers_mut().insert(header::CONTENT_LENGTH, len.into());
    res.headers_mut().insert(header::CONTENT_DISPOSITION, HeaderValue::from_str(&content_disposition("attachment", &name)).unwrap());
    Ok(res)
}

pub async fn delete_archive(State(st): State<AppState>, Admin(user): Admin, Path(id): Path<i64>) -> AppResult<Json<Value>> {
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    let (file, rows): (String, i64) =
        sqlx::query_as("SELECT file, rows FROM log_archives WHERE id = ?").bind(id).fetch_optional(&mut *tx).await?.ok_or_else(|| AppError::not_found("Archive not found"))?;
    sqlx::query("DELETE FROM log_archives WHERE id = ?").bind(id).execute(&mut *tx).await?;
    tree::log(&mut tx, &user, None, "log_archive_delete", &format!("{file} ({})", plural(rows, "record", "records"))).await?;
    tx.commit().await?;
    let _ = tokio::fs::remove_file(archive_dir(&st).join(&file)).await;
    Ok(Json(json!({ "ok": true })))
}

const KINDS: [&str; 3] = ["activity", "share_access", "login_log"];

/// Singular and plural nouns for each kind of log, in the same order as `KINDS`
const KIND_NOUNS: [(&str, &str); 3] =
    [("activity log entry", "activity log entries"), ("share visit log entry", "share visit log entries"), ("sign-in log entry", "sign-in log entries")];

/// "1 day" / "2 days"
fn plural(n: i64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn archive_dir(st: &AppState) -> PathBuf {
    st.data_dir.join("archives")
}

#[derive(Default, Debug)]
pub struct ArchiveSummary {
    archived: [i64; 3],
    deleted: [i64; 3],
    files: i64,
    pruned_files: i64,
}

impl ArchiveSummary {
    /// e.g. "Archived 3 activity log entries, 1 share visit log entry; removed 2 expired archive files"
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        for (verb, counts) in [("Archived", &self.archived), ("Deleted", &self.deleted)] {
            let items: Vec<String> =
                counts.iter().zip(KIND_NOUNS).filter(|(n, _)| **n > 0).map(|(n, (one, many))| plural(*n, one, many)).collect();
            if !items.is_empty() {
                parts.push(format!("{verb} {}", items.join(", ")));
            }
        }
        if self.pruned_files > 0 {
            let verb = if parts.is_empty() { "Removed" } else { "removed" };
            parts.push(format!("{verb} {}", plural(self.pruned_files, "expired archive file", "expired archive files")));
        }
        if parts.is_empty() { "No logs needed archiving".into() } else { parts.join("; ") }
    }
}

#[derive(Serialize, sqlx::FromRow)]
struct ActivityArchive {
    id: i64,
    at: i64,
    user_id: Option<i64>,
    username: String,
    drive_id: Option<String>,
    node_id: Option<String>,
    node_name: String,
    action: String,
    detail: String,
}

#[derive(Serialize, sqlx::FromRow)]
struct LoginArchive {
    id: i64,
    at: i64,
    user_id: Option<i64>,
    username: String,
    event: String,
    ip: String,
    user_agent: String,
    method: String,
}

/// Takes the oldest batch and turns it into JSON lines; returns (lines, max id, earliest time, latest time)
async fn fetch_batch<T>(st: &AppState, sql: &'static str, cutoff: i64, id: fn(&T) -> i64, at: fn(&T) -> i64) -> AppResult<Option<(Vec<String>, i64, i64, i64)>>
where
    T: Serialize + Send + Unpin + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>,
{
    let rows: Vec<T> = sqlx::query_as(sql).bind(cutoff).bind(ARCHIVE_BATCH).fetch_all(&st.db).await?;
    let Some(last) = rows.last() else { return Ok(None) };
    let (from, to) = (rows.iter().map(at).min().unwrap(), rows.iter().map(at).max().unwrap());
    Ok(Some((rows.iter().map(|r| serde_json::to_string(r).unwrap()).collect(), id(last), from, to)))
}

#[derive(Serialize, sqlx::FromRow)]
struct AccessArchive {
    id: i64,
    at: i64,
    share_id: String,
    owner_id: Option<i64>,
    node_id: Option<String>,
    node_name: String,
    event: String,
    ip: String,
    user_agent: String,
}

/// Runs daily (or immediately from the log settings page): archives or deletes records past the retention period and removes expired archive files
pub async fn run_archive(st: &AppState) -> AppResult<ArchiveSummary> {
    // Two runs at once would archive the same rows into the same file and record it twice
    let Ok(_running) = st.archive_lock.try_lock() else {
        return Err(AppError::new(axum::http::StatusCode::CONFLICT, "Log archiving is already running"));
    };
    let cfg = st.logs.read().unwrap().clone();
    let mut sum = ArchiveSummary::default();
    tokio::fs::create_dir_all(archive_dir(st)).await?;
    for (i, (kind, days)) in KINDS.into_iter().zip([cfg.activity_days, cfg.share_days, cfg.login_days]).enumerate() {
        if days == 0 {
            continue;
        }
        let cutoff = now() - days * DAY;
        loop {
            let n = if cfg.archive { archive_batch(st, kind, cutoff).await? } else { delete_batch(st, kind, cutoff).await? };
            if n == 0 {
                break;
            }
            if cfg.archive {
                sum.archived[i] += n;
                sum.files += 1;
            } else {
                sum.deleted[i] += n;
            }
        }
    }
    if cfg.archive_keep_days > 0 {
        let cutoff = now() - cfg.archive_keep_days * DAY;
        let old: Vec<(i64, String)> = sqlx::query_as("SELECT id, file FROM log_archives WHERE to_at < ?").bind(cutoff).fetch_all(&st.db).await?;
        for (id, file) in old {
            {
                let _w = st.write_lock.lock().await;
                sqlx::query("DELETE FROM log_archives WHERE id = ?").bind(id).execute(&st.db).await?;
            }
            let _ = tokio::fs::remove_file(archive_dir(st).join(&file)).await;
            sum.pruned_files += 1;
        }
    }
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    set_setting(&mut tx, "log_archived_at", &now().to_string()).await?;
    tx.commit().await?;
    Ok(sum)
}

/// Takes the oldest batch and writes it to a compressed file, removing it from the database only after the write succeeds
async fn archive_batch(st: &AppState, kind: &str, cutoff: i64) -> AppResult<i64> {
    let batch = match kind {
        "activity" => {
            fetch_batch::<ActivityArchive>(
                st,
                "SELECT id, at, user_id, username, drive_id, node_id, node_name, action, detail FROM activity WHERE at < ? ORDER BY id LIMIT ?",
                cutoff,
                |r| r.id,
                |r| r.at,
            )
            .await?
        }
        "share_access" => {
            fetch_batch::<AccessArchive>(
                st,
                "SELECT id, at, share_id, owner_id, node_id, node_name, event, ip, user_agent FROM share_access WHERE at < ? ORDER BY id LIMIT ?",
                cutoff,
                |r| r.id,
                |r| r.at,
            )
            .await?
        }
        _ => {
            fetch_batch::<LoginArchive>(
                st,
                "SELECT id, at, user_id, username, event, ip, user_agent, method FROM login_log WHERE at < ? ORDER BY id LIMIT ?",
                cutoff,
                |r| r.id,
                |r| r.at,
            )
            .await?
        }
    };
    let Some((lines, max_id, from_at, to_at)) = batch else { return Ok(0) };
    let count = lines.len() as i64;
    let file = format!("{kind}-{}-{}-{max_id}.jsonl.gz", format_time(from_at, 0)[..10].replace('-', ""), format_time(to_at, 0)[..10].replace('-', ""));
    let dir = archive_dir(st);
    let (path, tmp) = (dir.join(&file), dir.join(format!("{file}.tmp")));
    let bytes = tokio::task::spawn_blocking(move || -> std::io::Result<u64> {
        let out = std::fs::File::create(&tmp)?;
        let mut gz = flate2::write::GzEncoder::new(std::io::BufWriter::new(out), flate2::Compression::default());
        for line in lines {
            gz.write_all(line.as_bytes())?;
            gz.write_all(b"\n")?;
        }
        gz.finish()?.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        std::fs::rename(&tmp, &path)?;
        Ok(std::fs::metadata(&path)?.len())
    })
    .await
    .map_err(AppError::internal)??;

    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {kind} WHERE id <= ? AND at < ?"))).bind(max_id).bind(cutoff).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO log_archives (kind, from_at, to_at, rows, bytes, file, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)")
        .bind(kind)
        .bind(from_at)
        .bind(to_at)
        .bind(count)
        .bind(bytes as i64)
        .bind(&file)
        .bind(now())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!("Archived {count} {kind} records ({})", format_bytes(bytes as i64));
    Ok(count)
}

async fn delete_batch(st: &AppState, kind: &str, cutoff: i64) -> AppResult<i64> {
    let _w = st.write_lock.lock().await;
    let res = sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {kind} WHERE id IN (SELECT id FROM {kind} WHERE at < ? ORDER BY id LIMIT ?)")))
        .bind(cutoff)
        .bind(ARCHIVE_BATCH)
        .execute(&st.db)
        .await?;
    Ok(res.rows_affected() as i64)
}

/// Once a day: runs only if more than 24 hours have passed since the last archive
pub async fn daily_archive(st: &AppState) {
    let last: i64 = get_setting(&st.db, "log_archived_at").await.ok().flatten().and_then(|v| v.parse().ok()).unwrap_or(0);
    if now() - last < DAY {
        return;
    }
    match run_archive(st).await {
        Ok(sum) if sum.archived.iter().chain(&sum.deleted).any(|n| *n > 0) || sum.pruned_files > 0 => tracing::info!("Log archiving: {}", sum.describe()),
        Ok(_) => {}
        Err(e) => tracing::warn!("Log archiving failed: {}", e.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;
    use axum::http::HeaderMap;
    use std::io::Read;

    async fn names(st: &AppState, f: impl FnOnce(&mut ActivityQuery)) -> Vec<String> {
        query_activity(st, &q(f), 100).await.unwrap().into_iter().map(|r| r.node_name).collect()
    }

    async fn insert_activity(env: &testutil::TestEnv, user: &User, at: i64, action: &str, name: &str) {
        sqlx::query("INSERT INTO activity (at, user_id, username, node_name, action, detail) VALUES (?, ?, ?, ?, ?, '')")
            .bind(at)
            .bind(user.id)
            .bind(&user.username)
            .bind(name)
            .bind(action)
            .execute(&env.st.db)
            .await
            .unwrap();
    }

    fn q(f: impl FnOnce(&mut ActivityQuery)) -> ActivityQuery {
        let mut q = ActivityQuery::default();
        f(&mut q);
        q
    }

    #[tokio::test]
    async fn queued_events_are_written_before_the_writer_stops() {
        let env = testutil::env().await;
        let (tx, rx) = channel();
        let writer = spawn_writer(env.st.clone(), rx);
        // More than one batch, queued faster than they're written, then an immediate shutdown
        for i in 0..(BATCH * 2 + 17) {
            tx.send(LogEvent::Login { at: i as i64, user_id: None, username: "x".into(), event: "failed", method: "password".into(), ip: String::new(), user_agent: String::new() })
                .await
                .unwrap();
        }
        writer.finish().await;
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM login_log WHERE username = 'x'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(n as usize, BATCH * 2 + 17);
    }

    #[tokio::test]
    async fn activity_filters_and_paging() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let t = now();
        insert_activity(&env, &amy, t - 3 * DAY, "upload", "Annual report.docx").await;
        insert_activity(&env, &amy, t - 2 * DAY, "rename", "Budget_100%.xlsx").await;
        insert_activity(&env, &ben, t - DAY, "upload", "Photo.jpg").await;
        insert_activity(&env, &ben, t, "trash", "Annual report.docx").await;

        assert_eq!(names(&env.st, |q| q.user = Some("am".into())).await.len(), 2);
        assert_eq!(names(&env.st, |q| q.action = Some("upload, trash".into())).await, ["Annual report.docx", "Photo.jpg", "Annual report.docx"]);
        assert_eq!(names(&env.st, |q| q.q = Some("Annual".into())).await.len(), 2);
        // % and _ are literal characters, not wildcards
        assert_eq!(names(&env.st, |q| q.q = Some("_100%".into())).await, ["Budget_100%.xlsx"]);
        assert_eq!(names(&env.st, |q| q.q = Some("%".into())).await.len(), 1);
        assert_eq!(names(&env.st, |q| { q.from = Some(t - 2 * DAY - 10); q.to = Some(t - 10) }).await, ["Photo.jpg", "Budget_100%.xlsx"]);

        // Paging: 2 per page, continuing with before
        let page1 = query_activity(&env.st, &q(|_| {}), 2).await.unwrap();
        let page2 = query_activity(&env.st, &q(|q| q.before = Some(page1[1].id)), 2).await.unwrap();
        assert_eq!(page1.len() + page2.len(), 4);
        assert!(page2[0].id < page1[1].id);
    }

    #[tokio::test]
    async fn archiving_does_not_run_twice_at_once() {
        let env = testutil::env().await;
        let running = env.st.archive_lock.lock().await;
        let err = run_archive(&env.st).await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::CONFLICT);
        drop(running);
        assert!(run_archive(&env.st).await.is_ok());
    }

    #[tokio::test]
    async fn old_logs_are_archived_compressed_and_pruned() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let t = now();
        for i in 0..50 {
            insert_activity(&env, &amy, t - 400 * DAY + i, "upload", &format!("old-file-{i}")).await;
        }
        insert_activity(&env, &amy, t - DAY, "upload", "new-file").await;
        sqlx::query("INSERT INTO share_access (at, share_id, owner_id, event, ip) VALUES (?, 'abc', ?, 'view', '10.0.0.1')")
            .bind(t - 400 * DAY)
            .bind(amy.id)
            .execute(&env.st.db)
            .await
            .unwrap();

        let sum = run_archive(&env.st).await.unwrap();
        assert_eq!(sum.archived, [50, 1, 0]);
        let (left,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM activity WHERE node_name LIKE 'old-file-%'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(left, 0, "archived records should be removed from the database");
        let (kept,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM activity WHERE node_name = 'new-file'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(kept, 1, "records within the retention period are untouched");

        // Archive file: gzip-compressed JSON Lines with complete content
        let (file, rows): (String, i64) = sqlx::query_as("SELECT file, rows FROM log_archives WHERE kind = 'activity'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(rows, 50);
        let mut text = String::new();
        flate2::read::GzDecoder::new(std::fs::File::open(env.dir.join("archives").join(&file)).unwrap()).read_to_string(&mut text).unwrap();
        let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 50);
        assert_eq!(lines[0]["node_name"], "old-file-0");

        // Downloading it announces its length
        let (id,): (i64,) = sqlx::query_as("SELECT id FROM log_archives WHERE kind = 'activity'").fetch_one(&env.st.db).await.unwrap();
        let res = download_archive(State(env.st.clone()), Admin(env.admin().await), HeaderMap::new(), Path(id)).await.unwrap();
        let len = std::fs::metadata(env.dir.join("archives").join(&file)).unwrap().len();
        assert_eq!(res.headers()[header::CONTENT_LENGTH], len.to_string().as_str());

        // Archive files are removed after their retention period
        env.st.logs.write().unwrap().archive_keep_days = 30;
        let sum = run_archive(&env.st).await.unwrap();
        assert_eq!(sum.pruned_files, 2);
        assert!(!env.dir.join("archives").join(&file).exists());

        // Archiving off: delete directly
        insert_activity(&env, &amy, t - 400 * DAY, "upload", "another-old-file").await;
        env.st.logs.write().unwrap().archive = false;
        let sum = run_archive(&env.st).await.unwrap();
        assert_eq!(sum.deleted[0], 1);
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM log_archives").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn share_access_is_recorded() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let folder = env.folder(&amy, &amy.root_id, "Shared").await;
        let req = serde_json::from_value(json!({ "node_id": folder })).unwrap();
        let Json(info) = crate::shares::create(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
        let token = serde_json::to_value(&info).unwrap()["id"].as_str().unwrap().to_string();
        let visitor = || Visitor { ip: "203.0.113.9".into(), user_agent: "TestBrowser/1.0".into() };
        let other_visitor = Visitor { ip: "198.51.100.7".into(), user_agent: "TestBrowser/1.0".into() };
        let from = |ip: &str| ConnectInfo(format!("{ip}:4000").parse::<SocketAddr>().unwrap());
        let _ = crate::shares::public_info(State(env.st.clone()), Path(token.clone()), from("203.0.113.9"), HeaderMap::new(), visitor()).await.unwrap();
        // The same visitor reloading within a minute isn't logged again; another address is
        let _ = crate::shares::public_info(State(env.st.clone()), Path(token.clone()), from("203.0.113.9"), HeaderMap::new(), visitor()).await.unwrap();
        let _ = crate::shares::public_info(State(env.st.clone()), Path(token.clone()), from("198.51.100.7"), HeaderMap::new(), other_visitor).await.unwrap();
        // Records are written in the background
        for _ in 0..50 {
            let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM share_access").fetch_one(&env.st.db).await.unwrap();
            if n == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let rows = query_access(&env.st, &AccessQuery { share_id: Some(token.clone()), ..Default::default() }, Some(amy.id), 10).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].event.as_str(), rows[0].ip.as_str(), rows[0].user_agent.as_str()), ("view", "198.51.100.7", "TestBrowser/1.0"));
        assert_eq!(rows[1].ip, "203.0.113.9");
        // Others can't see them
        let other = env.user("ben", true).await;
        assert!(query_access(&env.st, &AccessQuery { share_id: Some(token.clone()), ..Default::default() }, Some(other.id), 10).await.unwrap().is_empty());
        // The share list shows the view count
        let Json(list) = crate::shares::list(State(env.st.clone()), amy.clone(), Query(serde_json::from_value(json!({})).unwrap())).await.unwrap();
        assert_eq!(serde_json::to_value(&list).unwrap()[0]["views"], 2);
    }

    /// Waits until the background-written records reach the given count
    async fn wait_rows(st: &AppState, table: &str, n: i64) {
        for _ in 0..100 {
            let (c,): (i64,) = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}"))).fetch_one(&st.db).await.unwrap();
            if c >= n {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("{table} didn't reach {n} rows");
    }

    #[tokio::test]
    async fn login_events_are_recorded() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let addr: SocketAddr = "198.51.100.7:5000".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::USER_AGENT, HeaderValue::from_static("TestBrowser/2.0"));
        let login = |user: &str, pw: &str| {
            let req = serde_json::from_value(json!({ "username": user, "password": pw })).unwrap();
            crate::auth::login(State(env.st.clone()), ConnectInfo(addr), headers.clone(), Json(req))
        };

        assert!(login("amy", crate::testutil::password()).await.is_ok());
        assert!(login("nobody", "whatever").await.is_err());
        // 5 consecutive failures: the 5th records a "locked" entry, and blocked attempts after that aren't recorded
        for _ in 0..5 {
            assert!(login("amy", "wrong-password").await.is_err());
        }
        assert!(login("amy", "wrong-password").await.is_err());
        wait_rows(&env.st, "login_log", 8).await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let all = query_logins(&env.st, &LoginQuery::default(), 100).await.unwrap();
        let events: Vec<&str> = all.iter().rev().map(|r| r.event.as_str()).collect();
        assert_eq!(events, ["login", "unknown_user", "bad_password", "bad_password", "bad_password", "bad_password", "bad_password", "locked"]);
        assert_eq!((all[0].ip.as_str(), all[0].user_agent.as_str()), ("198.51.100.7", "TestBrowser/2.0"));
        assert_eq!(all.iter().find(|r| r.event == "unknown_user").unwrap().user_id, None);

        // Filtering
        let only = |f: fn(&mut LoginQuery)| {
            let mut q = LoginQuery::default();
            f(&mut q);
            q
        };
        assert_eq!(query_logins(&env.st, &only(|q| q.event = Some("login,unknown_user".into())), 100).await.unwrap().len(), 2);
        assert_eq!(query_logins(&env.st, &only(|q| q.user = Some("nob".into())), 100).await.unwrap().len(), 1);

        // Standard users only see their own records
        let mut q = LoginQuery { user: Some("amy".into()), ..Default::default() };
        scope_logins(&ben, &mut q);
        assert!(query_logins(&env.st, &q, 100).await.unwrap().is_empty());
        let mut q = LoginQuery::default();
        scope_logins(&amy, &mut q);
        assert_eq!(query_logins(&env.st, &q, 100).await.unwrap().len(), 7);

        // Last sign-in time
        let (last,): (Option<i64>,) = sqlx::query_as("SELECT last_login_at FROM users WHERE id = ?").bind(amy.id).fetch_one(&env.st.db).await.unwrap();
        assert!(last.is_some());

        // Sign-in records past the retention period are archived too
        sqlx::query("UPDATE login_log SET at = at - 400 * 86400").execute(&env.st.db).await.unwrap();
        let sum = run_archive(&env.st).await.unwrap();
        assert_eq!(sum.archived, [0, 0, 8]);
        assert!(sum.describe() == "Archived 8 sign-in log entries");
    }

    #[test]
    fn old_settings_without_login_days_use_default() {
        let s: LogSettings = serde_json::from_str(r#"{"activity_days":30,"share_days":60,"archive":false,"archive_keep_days":0,"record_visitor":false}"#).unwrap();
        assert_eq!((s.activity_days, s.login_days, s.archive), (30, 365, false));
    }

    #[test]
    fn time_and_csv_formatting() {
        assert_eq!(format_time(0, 0), "1970-01-01 00:00:00");
        assert_eq!(format_time(1_790_341_830, 8 * 3600), "2026-09-25 21:10:30");
        assert_eq!(format_time(951_782_400, 0), "2000-02-29 00:00:00");
        assert_eq!(csv_field("=HYPERLINK(\"x\")"), "\"'=HYPERLINK(\"\"x\"\")\"");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("plain text"), "plain text");
    }
}
