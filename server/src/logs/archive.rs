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
                // Archives are for administrators: entries about personal spaces keep who did what there, not to what
                "SELECT id, at, user_id, username, drive_id, CASE WHEN private_to IS NULL THEN node_id END AS node_id,
                   CASE WHEN private_to IS NULL THEN node_name ELSE '' END AS node_name, action,
                   CASE WHEN private_to IS NULL THEN detail ELSE '' END AS detail
                 FROM activity WHERE at < ? ORDER BY id LIMIT ?",
                cutoff,
                |r| r.id,
                |r| r.at,
            )
            .await?
        }
        "share_access" => {
            fetch_batch::<AccessArchive>(
                st,
                // Nor visits to links in personal spaces which link or item they were
                "SELECT id, at, CASE WHEN private_to IS NULL THEN share_id ELSE '' END AS share_id, owner_id,
                   CASE WHEN private_to IS NULL THEN node_id END AS node_id, CASE WHEN private_to IS NULL THEN node_name ELSE '' END AS node_name,
                   event, ip, user_agent
                 FROM share_access WHERE at < ? ORDER BY id LIMIT ?",
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

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;
    use crate::testutil;

    /// Old entries: activity, sign-ins and errors from 400 days ago
    async fn old_entries(env: &testutil::TestEnv, activity: usize, logins: usize, errors: usize) {
        let at = now() - 400 * DAY;
        for i in 0..activity {
            sqlx::query("INSERT INTO activity (at, username, node_name, action, detail) VALUES (?, 'amy', ?, 'upload', '')")
                .bind(at + i as i64)
                .bind(format!("file-{i}.txt"))
                .execute(&env.st.db)
                .await
                .unwrap();
        }
        for _ in 0..logins {
            sqlx::query("INSERT INTO login_log (at, username, event) VALUES (?, 'amy', 'login')").bind(at).execute(&env.st.db).await.unwrap();
        }
        for _ in 0..errors {
            sqlx::query("INSERT INTO error_log (at, first_at, source, severity, fingerprint) VALUES (?, ?, 'backend', 'error', 'f')")
                .bind(at)
                .bind(at)
                .execute(&env.st.db)
                .await
                .unwrap();
        }
    }

    async fn logged(env: &testutil::TestEnv, action: &str) -> Vec<String> {
        sqlx::query_scalar("SELECT detail FROM activity WHERE action = ? ORDER BY id").bind(action).fetch_all(&env.st.db).await.unwrap()
    }

    fn english() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("tf_lang=en"));
        h
    }

    fn file_name(res: &Response) -> String {
        let d = res.headers()[header::CONTENT_DISPOSITION].to_str().unwrap();
        percent_encoding::percent_decode_str(d.split("filename*=UTF-8''").nth(1).unwrap()).decode_utf8().unwrap().into_owned()
    }

    #[tokio::test]
    async fn archiving_by_hand_files_old_entries_logs_the_run_and_the_archives_can_be_fetched_and_deleted() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        old_entries(&env, 3, 2, 1).await;

        let Json(v) = archive_now(State(env.st.clone()), Admin(admin.clone())).await.unwrap();
        let summary = "Archived 3 activity log entries, 2 sign-in log entries, 1 error log entry";
        assert_eq!(v["summary"], summary);
        assert_eq!(logged(&env, "log_archive").await, [summary]);
        // What is left is the run itself; the archives are listed, newest first, with their size
        assert_eq!((v["activity"]["rows"].as_i64(), v["login_log"]["rows"].as_i64(), v["error_log"]["rows"].as_i64()), (Some(1), Some(0), Some(0)));
        assert_eq!(v["archives"].as_array().unwrap().len(), 3);
        assert!(v["archive_bytes"].as_i64().unwrap() > 0);
        assert!(v["last_run"].as_i64().unwrap() >= now() - 60);
        let Json(status) = get_status(State(env.st.clone()), Admin(admin.clone())).await.unwrap();
        assert_eq!(status["archives"], v["archives"]);

        // Nothing more to archive
        let Json(again) = archive_now(State(env.st.clone()), Admin(admin.clone())).await.unwrap();
        assert_eq!(again["summary"], "No logs needed archiving");

        // Downloaded under a name in the interface language
        let (id, file): (i64, String) = sqlx::query_as("SELECT id, file FROM log_archives WHERE kind = 'login_log'").fetch_one(&env.st.db).await.unwrap();
        let day = format_time(now() - 400 * DAY, 0)[..10].to_string();
        let res = download_archive(State(env.st.clone()), Admin(admin.clone()), english(), Path(id)).await.unwrap();
        assert_eq!(res.headers()[header::CONTENT_TYPE], "application/gzip");
        assert_eq!(file_name(&res), format!("login-log-{day}-{day}.jsonl.gz"));
        let res = download_archive(State(env.st.clone()), Admin(admin.clone()), HeaderMap::new(), Path(id)).await.unwrap();
        assert_eq!(file_name(&res), format!("登入紀錄-{day}-{day}.jsonl.gz"));

        // Deleted: the file goes, and so does the row; the deletion is logged
        let Json(ok) = delete_archive(State(env.st.clone()), Admin(admin.clone()), Path(id)).await.unwrap();
        assert_eq!(ok["ok"], true);
        assert!(!env.dir.join("archives").join(&file).exists());
        assert_eq!(logged(&env, "log_archive_delete").await, [format!("{file} (2 records)")]);
        let missing = |r: AppResult<Response>| r.unwrap_err().status;
        assert_eq!(missing(download_archive(State(env.st.clone()), Admin(admin.clone()), HeaderMap::new(), Path(id)).await), StatusCode::NOT_FOUND);
        assert_eq!(delete_archive(State(env.st.clone()), Admin(admin.clone()), Path(id)).await.unwrap_err().status, StatusCode::NOT_FOUND);

        // A row whose file was removed by hand says so
        let (id, file): (i64, String) = sqlx::query_as("SELECT id, file FROM log_archives WHERE kind = 'activity'").fetch_one(&env.st.db).await.unwrap();
        std::fs::remove_file(env.dir.join("archives").join(&file)).unwrap();
        let err = download_archive(State(env.st.clone()), Admin(admin), HeaderMap::new(), Path(id)).await.unwrap_err();
        assert_eq!((err.status, err.message.as_str()), (StatusCode::NOT_FOUND, "The archive file no longer exists"));
    }

    #[tokio::test]
    async fn log_settings_are_checked_saved_and_described_in_the_log() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        for (bad, label) in [
            (LogSettings { activity_days: -1, ..Default::default() }, "Activity log retention (days)"),
            (LogSettings { login_days: 36_501, ..Default::default() }, "Sign-in log retention (days)"),
            (LogSettings { archive_keep_days: -5, ..Default::default() }, "Archive retention (days)"),
        ] {
            let err = update_settings(State(env.st.clone()), Admin(admin.clone()), Json(bad)).await.unwrap_err();
            assert_eq!(err.status, StatusCode::BAD_REQUEST);
            assert_eq!(err.message, format!("{label} must be between 0 and 36500"));
        }
        assert!(logged(&env, "settings").await.is_empty());

        let s = LogSettings { activity_days: 30, share_days: 0, login_days: 1, archive: false, archive_keep_days: 0, record_visitor: false };
        let Json(v) = update_settings(State(env.st.clone()), Admin(admin.clone()), Json(s)).await.unwrap();
        assert_eq!(v["settings"]["activity_days"], 30);
        let saved = super::super::load_settings(&env.st.db).await;
        assert_eq!((saved.activity_days, saved.share_days, saved.login_days, saved.archive, saved.record_visitor), (30, 0, 1, false, false));
        assert_eq!(env.st.logs.read().unwrap().login_days, 1, "in use at once");
        assert_eq!(
            logged(&env, "settings").await,
            ["Log settings: activity 30 days, share visits never cleaned up, sign-ins 1 day, delete directly, archives kept forever, visitor IPs not recorded"]
        );

        let s = LogSettings { activity_days: 1, archive_keep_days: 1, ..Default::default() };
        let _ = update_settings(State(env.st.clone()), Admin(admin), Json(s)).await.unwrap();
        assert_eq!(
            logged(&env, "settings").await.pop().unwrap(),
            "Log settings: activity 1 day, share visits 180 days, sign-ins 365 days, compress and archive, archives kept 1 day"
        );
    }

    #[tokio::test]
    async fn the_daily_run_waits_a_day_after_the_last_one() {
        let env = testutil::env().await;
        old_entries(&env, 2, 0, 0).await;
        let ran_at = |t: i64| {
            let st = env.st.clone();
            async move {
                let mut c = st.db.acquire().await.unwrap();
                set_setting(&mut c, "log_archived_at", &t.to_string()).await.unwrap();
            }
        };
        let old = || async { sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM activity").fetch_one(&env.st.db).await.unwrap() };
        ran_at(now() - 3600).await;
        daily_archive(&env.st).await;
        assert_eq!(old().await, 2);
        ran_at(now() - DAY - 1).await;
        daily_archive(&env.st).await;
        assert_eq!(old().await, 0);
        let last: i64 = get_setting(&env.st.db, "log_archived_at").await.unwrap().unwrap().parse().unwrap();
        assert!(last >= now() - 60);
    }
}
