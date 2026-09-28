//! Logs: the activity log, share link visit log and sign-in log, and their settings. Split into writing (`write`),
//! querying (`query`), CSV export (`export`) and periodic compressed archiving (`archive`).
//!
//! The database keeps only recent records (180 days by default); older ones are compressed daily into `data/archives/*.jsonl.gz`
//! (one JSON record per line, importable with any text tool or spreadsheet). Archive files have their own retention period, so data doesn't grow forever.

mod archive;
mod export;
mod query;
mod write;

pub use archive::*;
pub use export::*;
pub use query::*;
pub use write::*;

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, FromRequestParts},
    http::{header, request::Parts},
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::{
    db::get_setting,
    error::AppError,
    state::AppState,
};

const DAY: i64 = 86_400;

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
#[derive(Clone)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth::{Admin, User},
        testutil, tree,
        util::now,
    };
    use axum::{
        Json,
        extract::{Path, Query, State},
        http::{HeaderMap, HeaderValue},
    };
    use serde_json::{Value, json};
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

    #[tokio::test]
    async fn an_items_history_covers_what_is_inside_it_for_anyone_who_can_open_it() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let top = env.folder(&amy, &amy.root_id, "top").await;
        let sub = env.folder(&amy, &top, "sub").await;
        let a = env.file(&amy, &top, "a.txt").await;
        let b = env.file(&amy, &sub, "b.txt").await;
        let other = env.file(&amy, &amy.root_id, "other.txt").await;
        let log = |id: String, action: &'static str| {
            let (st, amy) = (env.st.clone(), amy.clone());
            async move {
                let mut c = st.db.acquire().await.unwrap();
                let node = tree::get_node(&mut c, &id).await.unwrap().unwrap();
                record_activity(&mut c, &amy, Some(&node), action, "").await.unwrap();
            }
        };
        log(a.clone(), "upload").await;
        log(b.clone(), "edit").await;
        log(top.clone(), "rename").await;
        log(top.clone(), "grant").await;
        log(other.clone(), "upload").await;
        let history = |who: User, id: &str| {
            let (st, id) = (env.st.clone(), id.to_string());
            async move { node_history(State(st), who, Path(id)).await.map(|Json(rows)| rows.into_iter().map(|r| format!("{} {}", r.action, r.node_name)).collect::<Vec<_>>()) }
        };

        // The folder's own entries and those of everything inside it, newest first; the owner sees permission changes too
        assert_eq!(history(amy.clone(), &top).await.unwrap(), ["grant top", "rename top", "edit b.txt", "upload a.txt"]);
        assert_eq!(history(amy.clone(), &a).await.unwrap(), ["upload a.txt"]);
        // The space's root folder: the whole space
        assert_eq!(history(amy.clone(), &amy.root_id).await.unwrap().len(), 5);
        // A trashed item's entries stay in its folder's history
        let _ = crate::nodes::trash(State(env.st.clone()), amy.clone(), Json(serde_json::from_value(json!({ "ids": [b] })).unwrap())).await.unwrap();
        assert_eq!(history(amy.clone(), &sub).await.unwrap(), ["trash b.txt", "edit b.txt"]);

        // Someone who can view the folder sees its history, without sharing and permission changes
        assert_eq!(history(ben.clone(), &top).await.unwrap_err().status, axum::http::StatusCode::NOT_FOUND);
        env.grant(&top, &ben, "viewer").await;
        assert_eq!(history(ben.clone(), &top).await.unwrap(), ["trash b.txt", "rename top", "edit b.txt", "upload a.txt"]);
        assert_eq!(history(ben.clone(), &other).await.unwrap_err().status, axum::http::StatusCode::NOT_FOUND);

        // Only the most recent entries
        for _ in 0..60 {
            log(a.clone(), "edit").await;
        }
        let rows = history(amy.clone(), &a).await.unwrap();
        assert_eq!(rows.len(), HISTORY_LIMIT as usize);
        assert!(rows.iter().all(|r| r == "edit a.txt"));
    }
}
