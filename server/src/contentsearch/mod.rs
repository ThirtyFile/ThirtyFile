//! Search inside files (Control panel › General › Search): the text of files, found as names are, through a trigram
//! index (any part of a word matches, in every language).
//!
//! - Off until an administrator turns it on. Turning it on queues every file; turning it off deletes the index.
//! - What is read (extract.rs): plain text, Markdown, CSV and code, web pages, and the text of DOCX, XLSX, PPTX and
//!   OpenDocument files. Pictures, scanned pages and PDFs hold nothing it reads (yet). Files over a size limit are left
//!   alone, and the text kept is limited per file and in all.
//! - A background job reads the files (indexer.rs): never inside a request, a few files at a time with a pause between
//!   them, and it can be paused. Triggers on `nodes` (migration 0036) queue a file when it is added, gets new content,
//!   is renamed or leaves the trash, and clear it when it is deleted, wherever the change comes from.
//! - Results follow the same visibility as names: the search only looks among the items the person can open
//!   (`nodes::find::Criteria::held`), and a snippet is made only for an item that is returned (search.rs).

mod extract;
mod indexer;
mod search;
mod zipread;

#[cfg(test)]
mod tests;

use std::sync::atomic::AtomicI64;

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};

pub use indexer::spawn;
pub use search::{enabled, match_expr, snippets};

use crate::{
    auth::Admin,
    db,
    error::{AppError, AppResult},
    state::AppState,
};

const SETTING: &str = "content_search";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    /// Nothing new is read; what was read stays searchable
    pub paused: bool,
    /// The largest file read, in MB
    pub max_file_mb: u32,
    /// The most text kept in all, in MB (the index takes about three times that on the disk)
    pub max_total_mb: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config { enabled: false, paused: false, max_file_mb: 25, max_total_mb: 500 }
    }
}

impl Config {
    pub fn max_file_bytes(&self) -> u64 {
        u64::from(self.max_file_mb) * 1024 * 1024
    }

    pub fn max_total_bytes(&self) -> i64 {
        i64::from(self.max_total_mb) * 1024 * 1024
    }
}

/// Search inside files' part of `AppState`
pub struct Memory {
    /// Wakes the job: the settings changed
    pub wake: tokio::sync::Notify,
    /// Bytes of text kept, as the job counts them; negative until counted
    total: AtomicI64,
}

impl Default for Memory {
    fn default() -> Self {
        Memory { wake: tokio::sync::Notify::new(), total: AtomicI64::new(-1) }
    }
}

#[derive(Serialize)]
pub struct Info {
    #[serde(flatten)]
    config: Config,
    /// Files waiting to be read
    queued: i64,
    /// Files whose text is searchable
    indexed: i64,
    /// Files with nothing to read in them (other kinds of file, too large, no text)
    skipped: i64,
    /// Files that could not be read
    failed: i64,
    /// Bytes of text kept
    bytes: i64,
    /// The limit for all the text was reached: files wait in the queue
    full: bool,
}

async fn info(st: &AppState) -> AppResult<Info> {
    let mut c = st.db.acquire().await?;
    let config: Config = db::json_setting(&mut c, SETTING).await?;
    let rows: Vec<(String, i64, i64)> =
        sqlx::query_as("SELECT state, COUNT(*), COALESCE(SUM(bytes), 0) FROM content_index GROUP BY state").fetch_all(&mut *c).await?;
    let (queued,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM content_queue").fetch_one(&mut *c).await?;
    let count = |state: &str| rows.iter().find(|(s, ..)| s == state).map_or(0, |r| r.1);
    let bytes = rows.iter().map(|r| r.2).sum();
    Ok(Info {
        full: config.enabled && queued > 0 && bytes >= config.max_total_bytes(),
        config,
        queued,
        indexed: count("indexed"),
        skipped: count("skipped"),
        failed: count("failed"),
        bytes,
    })
}

pub async fn get(State(st): State<AppState>, _: Admin) -> AppResult<Json<Info>> {
    Ok(Json(info(&st).await?))
}

fn check(c: &Config) -> AppResult<()> {
    if !(1..=100).contains(&c.max_file_mb) {
        return Err(AppError::bad_request("The largest file read is 1 to 100 MB"));
    }
    if !(10..=100_000).contains(&c.max_total_mb) {
        return Err(AppError::bad_request("The text kept is 10 to 100,000 MB in all"));
    }
    Ok(())
}

/// Queues every file that is not in the trash
async fn queue_all(conn: &mut sqlx::SqliteConnection) -> AppResult<()> {
    sqlx::query("INSERT OR IGNORE INTO content_queue (node_id, queued_at) SELECT id, unixepoch() FROM nodes WHERE kind = 'file' AND trashed_at IS NULL")
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Deletes everything read so far, and what waits to be
async fn clear(conn: &mut sqlx::SqliteConnection) -> AppResult<()> {
    sqlx::query("DELETE FROM content_fts").execute(&mut *conn).await?;
    sqlx::query("DELETE FROM content_index").execute(&mut *conn).await?;
    sqlx::query("DELETE FROM content_queue").execute(&mut *conn).await?;
    Ok(())
}

pub async fn save(State(st): State<AppState>, _: Admin, Json(config): Json<Config>) -> AppResult<Json<Info>> {
    check(&config)?;
    {
        let _w = st.write_lock.lock().await;
        let mut tx = db::begin_write(&st.db).await?;
        let res = async {
            let before: Config = db::json_setting(&mut tx, SETTING).await?;
            db::set_json_setting(&mut tx, SETTING, &config).await?;
            sqlx::query("UPDATE content_switch SET on_ = ?").bind(config.enabled).execute(&mut *tx).await?;
            if config.enabled && !before.enabled {
                queue_all(&mut tx).await?;
            } else if !config.enabled && before.enabled {
                clear(&mut tx).await?;
            } else if config.enabled && config.max_file_mb > before.max_file_mb {
                // Files that were too large for the old limit may fit the new one
                sqlx::query("UPDATE content_index SET stamp = '' WHERE state = 'skipped' AND note = ?").bind(indexer::TOO_LARGE).execute(&mut *tx).await?;
                sqlx::query("INSERT OR IGNORE INTO content_queue (node_id, queued_at) SELECT node_id, unixepoch() FROM content_index WHERE stamp = ''")
                    .execute(&mut *tx)
                    .await?;
            }
            Ok(())
        }
        .await;
        db::settle(tx, res).await?;
    }
    indexer::forget_total(&st);
    st.part::<Memory>().wake.notify_one();
    Ok(Json(info(&st).await?))
}

/// Reads every file again: what was found is deleted first
pub async fn rebuild(State(st): State<AppState>, _: Admin) -> AppResult<Json<Info>> {
    {
        let _w = st.write_lock.lock().await;
        let mut tx = db::begin_write(&st.db).await?;
        let res = async {
            let config: Config = db::json_setting(&mut tx, SETTING).await?;
            if !config.enabled {
                return Err(AppError::bad_request("Search inside files is off"));
            }
            clear(&mut tx).await?;
            queue_all(&mut tx).await
        }
        .await;
        db::settle(tx, res).await?;
    }
    indexer::forget_total(&st);
    st.part::<Memory>().wake.notify_one();
    Ok(Json(info(&st).await?))
}

/// Tests: lets the job read everything that is waiting
#[cfg(test)]
pub(crate) async fn read_everything(st: &AppState) {
    for _ in 0..200 {
        if !indexer::step(st).await.unwrap() {
            return;
        }
    }
    panic!("the job did not finish");
}
