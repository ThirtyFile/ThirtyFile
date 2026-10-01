//! Log settings, and archiving: records past the retention period are compressed into archive files (or deleted)

use std::{io::Write, path::PathBuf};

use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, header},
    response::Response,
};
use serde::Serialize;
use serde_json::{Value, json};

use super::{DAY, LogSettings, english, format_time, localize, record_activity};
use crate::{
    auth::Admin,
    db::{get_setting, set_setting},
    error::{AppError, AppResult},
    state::AppState,
    util::{content_disposition, format_bytes, now},
};

/// Maximum records per archive file (large volumes are split into several files)
const ARCHIVE_BATCH: i64 = 20_000;

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
    let (error_rows, error_oldest): (i64, Option<i64>) = sqlx::query_as("SELECT COUNT(*), MIN(at) FROM error_log").fetch_one(&st.db).await?;
    let archives: Vec<ArchiveRow> =
        sqlx::query_as("SELECT id, kind, from_at, to_at, rows, bytes, created_at FROM log_archives ORDER BY to_at DESC, id DESC").fetch_all(&st.db).await?;
    let last_run: Option<i64> = get_setting(&st.db, "log_archived_at").await?.and_then(|v| v.parse().ok());
    Ok(json!({
        "settings": *st.logs.read().unwrap(),
        "activity": { "rows": activity_rows, "oldest": activity_oldest },
        "share_access": { "rows": share_rows, "oldest": share_oldest },
        "login_log": { "rows": login_rows, "oldest": login_oldest },
        "error_log": { "rows": error_rows, "oldest": error_oldest },
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
        let mut tx = crate::db::begin_write(&st.db).await?;
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
        record_activity(&mut tx, &user, None, "settings", &detail).await?;
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
        let mut tx = crate::db::begin_write(&st.db).await?;
        record_activity(&mut tx, &user, None, "log_archive", &summary.describe()).await?;
        tx.commit().await?;
    }
    let mut v = status(&st).await?;
    v["summary"] = json!(summary.describe());
    Ok(Json(v))
}

pub async fn download_archive(State(st): State<AppState>, _: Admin, headers: HeaderMap, Path(id): Path<i64>) -> AppResult<Response> {
    let (kind, file, from_at, to_at): (String, String, i64, i64) = sqlx::query_as("SELECT kind, file, from_at, to_at FROM log_archives WHERE id = ?")
        .bind(id)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Archive not found"))?;
    let path = archive_dir(&st).join(&file);
    let f = tokio::fs::File::open(&path).await.map_err(|_| AppError::not_found("The archive file no longer exists"))?;
    let slug = match kind.as_str() {
        "activity" => "activity-log",
        "share_access" => "share-access-log",
        "error_log" => "error-log",
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
    let mut tx = crate::db::begin_write(&st.db).await?;
    let (file, rows): (String, i64) = sqlx::query_as("SELECT file, rows FROM log_archives WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::not_found("Archive not found"))?;
    sqlx::query("DELETE FROM log_archives WHERE id = ?").bind(id).execute(&mut *tx).await?;
    record_activity(&mut tx, &user, None, "log_archive_delete", &format!("{file} ({})", plural(rows, "record", "records"))).await?;
    tx.commit().await?;
    let _ = tokio::fs::remove_file(archive_dir(&st).join(&file)).await;
    Ok(Json(json!({ "ok": true })))
}

/// The error log is kept as long as the activity log (it has no retention setting of its own)
const KINDS: [&str; 4] = ["activity", "share_access", "login_log", "error_log"];

/// Singular and plural nouns for each kind of log, in the same order as `KINDS`
const KIND_NOUNS: [(&str, &str); 4] = [
    ("activity log entry", "activity log entries"),
    ("share visit log entry", "share visit log entries"),
    ("sign-in log entry", "sign-in log entries"),
    ("error log entry", "error log entries"),
];

/// "1 day" / "2 days"
fn plural(n: i64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn archive_dir(st: &AppState) -> PathBuf {
    st.data_dir.join("archives")
}

#[derive(Default, Debug)]
pub struct ArchiveSummary {
    pub(super) archived: [i64; 4],
    pub(super) deleted: [i64; 4],
    pub(super) files: i64,
    pub(super) pruned_files: i64,
}

impl ArchiveSummary {
    /// e.g. "Archived 3 activity log entries, 1 share visit log entry; removed 2 expired archive files"
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        for (verb, counts) in [("Archived", &self.archived), ("Deleted", &self.deleted)] {
            let items: Vec<String> = counts.iter().zip(KIND_NOUNS).filter(|(n, _)| **n > 0).map(|(n, (one, many))| plural(*n, one, many)).collect();
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
    for (i, (kind, days)) in KINDS.into_iter().zip([cfg.activity_days, cfg.share_days, cfg.login_days, cfg.activity_days]).enumerate() {
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
    let mut tx = crate::db::begin_write(&st.db).await?;
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
        "error_log" => {
            fetch_batch::<super::ErrorRow>(
                st,
                "SELECT id, at, first_at, count, source, severity, kind, user_id, username, operation, route, resource, status, code, message,
                   detail, request_id, client, version FROM error_log WHERE at < ? ORDER BY id LIMIT ?",
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
    let mut tx = crate::db::begin_write(&st.db).await?;
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
