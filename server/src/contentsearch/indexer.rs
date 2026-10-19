//! The job that reads files: it takes the files in `content_queue` a few at a time, reads each one's text
//! (extract.rs) and stores it in `content_fts`. It runs on its own, one file at a time with a pause between them, so a
//! small machine stays responsive, and it never runs inside a request.
//!
//! A file is read again when its content changes: what it was read from is kept as its `stamp` (the stored content's
//! hash, or the size and time of a folder space's file), and a file whose stamp is still the same is not read.

use std::sync::atomic::Ordering;
use std::time::Duration;

use sqlx::SqliteConnection;
use tokio::io::AsyncReadExt;

use super::{Config, Memory, SETTING, extract};
use crate::{
    db,
    error::AppResult,
    files::Source,
    state::AppState,
    tree::{self, Node},
    util::now,
};

/// Why a file was left alone for its size
pub(super) const TOO_LARGE: &str = "Larger than the largest file read";
/// Files taken from the queue at a time
const BATCH: i64 = 8;
/// The wait after each file, which leaves the disks and the database to the people using them
const BETWEEN_FILES: Duration = Duration::from_millis(25);
/// How long the job rests when there is nothing to read, if nothing wakes it
const IDLE: Duration = Duration::from_secs(5);
/// A storage location that doesn't answer is tried again after this many seconds times the tries so far, five times
const RETRY_SECONDS: i64 = 300;
const MAX_TRIES: i64 = 5;

/// The wait after each file (none in tests)
fn between_files() -> Duration {
    if cfg!(test) { Duration::ZERO } else { BETWEEN_FILES }
}

pub fn spawn(st: AppState) {
    crate::util::supervise("Search inside files", move |_| {
        let st = st.clone();
        async move {
            loop {
                let busy = match step(&st).await {
                    Ok(busy) => busy,
                    Err(e) => {
                        tracing::warn!("Search inside files: {}", e.message);
                        false
                    }
                };
                if busy {
                    tokio::time::sleep(between_files()).await;
                } else {
                    tokio::select! {
                        _ = st.part::<Memory>().wake.notified() => {}
                        _ = tokio::time::sleep(IDLE) => {}
                    }
                }
            }
        }
    });
}

/// The text kept is counted again (a file was deleted, the index was cleared)
pub fn forget_total(st: &AppState) {
    st.part::<Memory>().total.store(-1, Ordering::Relaxed);
}

async fn total_bytes(st: &AppState, conn: &mut SqliteConnection) -> AppResult<i64> {
    let memory = st.part::<Memory>();
    let known = memory.total.load(Ordering::Relaxed);
    if known >= 0 {
        return Ok(known);
    }
    let (sum,): (i64,) = sqlx::query_as("SELECT COALESCE(SUM(bytes), 0) FROM content_index").fetch_one(conn).await?;
    memory.total.store(sum, Ordering::Relaxed);
    Ok(sum)
}

/// Whether the text kept has reached its limit. Files may have been deleted since it was last counted, so it is
/// counted again before waiting on the limit.
async fn full(st: &AppState, config: &Config, conn: &mut SqliteConnection) -> AppResult<bool> {
    if total_bytes(st, conn).await? < config.max_total_bytes() {
        return Ok(false);
    }
    forget_total(st);
    Ok(total_bytes(st, conn).await? >= config.max_total_bytes())
}

/// What was made of a file
pub(super) enum Outcome {
    Text(String),
    /// Nothing to read in it, and why
    Skipped(&'static str),
    /// It could not be read for now (a storage location that doesn't answer)
    Later,
    Failed(&'static str),
}

/// Reads a batch of the files waiting. True when there may be more to read.
pub(super) async fn step(st: &AppState) -> AppResult<bool> {
    let mut c = st.db.acquire().await?;
    let config: Config = db::json_setting(&mut c, SETTING).await?;
    // The triggers fill the queue by this switch: kept equal to the setting (a restored database may differ)
    let (on,): (bool,) = sqlx::query_as("SELECT on_ FROM content_switch").fetch_one(&mut *c).await?;
    if on != config.enabled {
        drop(c);
        let _w = st.write_lock.lock().await;
        let mut tx = db::begin_write(&st.db).await?;
        let res = async {
            sqlx::query("UPDATE content_switch SET on_ = ?").bind(config.enabled).execute(&mut *tx).await?;
            if config.enabled {
                super::queue_all(&mut tx).await?;
            } else {
                super::clear(&mut tx).await?;
            }
            Ok(())
        }
        .await;
        db::settle(tx, res).await?;
        forget_total(st);
        return Ok(config.enabled);
    }
    if !config.enabled || config.paused {
        return Ok(false);
    }
    if full(st, &config, &mut c).await? {
        return Ok(false);
    }
    let due: Vec<(String, i64)> = sqlx::query_as("SELECT node_id, attempts FROM content_queue WHERE not_before <= ? ORDER BY queued_at, node_id LIMIT ?")
        .bind(now())
        .bind(BATCH)
        .fetch_all(&mut *c)
        .await?;
    drop(c);
    let busy = !due.is_empty();
    for (id, attempts) in due {
        // The limit for all the text may be reached part-way through a batch
        if full(st, &config, &mut *st.db.acquire().await?).await? {
            return Ok(false);
        }
        if let Err(e) = one(st, &config, &id, attempts).await {
            tracing::warn!("Search inside files: {}", e.message);
        }
        tokio::time::sleep(between_files()).await;
    }
    Ok(busy)
}

/// What a file's content is now, for telling whether it was read already
async fn stamp_of(conn: &mut SqliteConnection, id: &str) -> AppResult<Option<String>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT COALESCE(blob_hash, 'f' || COALESCE(fs_size, 0) || '-' || COALESCE(fs_mtime_ns, 0)) FROM nodes WHERE id = ?")
            .bind(id)
            .fetch_optional(conn)
            .await?;
    Ok(row.map(|r| r.0))
}

async fn dequeue(conn: &mut SqliteConnection, id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM content_queue WHERE node_id = ?").bind(id).execute(conn).await?;
    Ok(())
}

/// Reads one file from the queue and stores what it holds
async fn one(st: &AppState, config: &Config, id: &str, attempts: i64) -> AppResult<()> {
    let (node, stamp, current) = {
        let mut c = st.db.acquire().await?;
        let Some(node) = tree::get_node(&mut c, id).await? else {
            dequeue(&mut c, id).await?;
            return Ok(());
        };
        // In the trash, or no longer a file: nothing to read (coming back from the trash queues it again)
        let stamp = stamp_of(&mut c, id).await?.unwrap_or_default();
        if node.kind != "file" || node.trashed_at.is_some() {
            dequeue(&mut c, id).await?;
            return Ok(());
        }
        let current: Option<(String,)> = sqlx::query_as("SELECT stamp FROM content_index WHERE node_id = ?").bind(id).fetch_optional(&mut *c).await?;
        (node, stamp, current.map(|r| r.0))
    };
    if current.as_deref() == Some(stamp.as_str()) {
        let mut c = st.db.acquire().await?;
        return dequeue(&mut c, id).await;
    }
    let outcome = read(st, config, &node).await;
    if matches!(outcome, Outcome::Later) {
        return later(st, id, attempts).await;
    }
    store(st, id, &stamp, outcome).await
}

async fn read(st: &AppState, config: &Config, node: &Node) -> Outcome {
    let Some(kind) = extract::kind_of(&node.name, &node.mime) else {
        return Outcome::Skipped("Not a kind of file that is read");
    };
    let cap = config.max_file_bytes();
    if node.size.max(0) as u64 > cap {
        return Outcome::Skipped(TOO_LARGE);
    }
    let Ok(source) = Source::resolve(st, node).await else {
        return Outcome::Later;
    };
    let mut data = Vec::new();
    match source.open(st, 0, cap + 1).await {
        Ok(mut reader) => {
            if reader.read_to_end(&mut data).await.is_err() {
                return Outcome::Later;
            }
        }
        Err(_) => return Outcome::Later,
    }
    if data.len() as u64 > cap {
        return Outcome::Skipped(TOO_LARGE);
    }
    match tokio::task::spawn_blocking(move || extract::read(kind, &data)).await {
        Ok(Ok(text)) => Outcome::Text(text),
        Ok(Err(reason)) if reason == "It could not be unpacked" => Outcome::Failed(reason),
        Ok(Err(reason)) => Outcome::Skipped(reason),
        Err(_) => Outcome::Failed("It could not be read"),
    }
}

/// The file could not be read for now: tried again later, and given up on after a few tries
async fn later(st: &AppState, id: &str, attempts: i64) -> AppResult<()> {
    if attempts + 1 >= MAX_TRIES {
        let stamp = {
            let mut c = st.db.acquire().await?;
            stamp_of(&mut c, id).await?
        };
        if let Some(stamp) = stamp {
            return store(st, id, &stamp, Outcome::Failed("It could not be read")).await;
        }
    }
    let _w = st.write_lock.lock().await;
    let mut tx = db::begin_write(&st.db).await?;
    let res = async {
        sqlx::query("UPDATE content_queue SET attempts = attempts + 1, not_before = ? WHERE node_id = ?")
            .bind(now() + RETRY_SECONDS * (attempts + 1))
            .bind(id)
            .execute(&mut *tx)
            .await?;
        Ok(())
    }
    .await;
    db::settle(tx, res).await
}

/// Stores what was made of a file read from content `stamp`, unless it has new content meanwhile (it then stays in the
/// queue to be read again)
pub(super) async fn store(st: &AppState, id: &str, stamp: &str, outcome: Outcome) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    let mut tx = db::begin_write(&st.db).await?;
    let res = async {
        if stamp_of(&mut tx, id).await?.as_deref() != Some(stamp) {
            return Ok(0);
        }
        let old: Option<(Option<i64>, i64)> =
            sqlx::query_as("SELECT fts_rowid, bytes FROM content_index WHERE node_id = ?").bind(id).fetch_optional(&mut *tx).await?;
        if let Some((Some(rowid), _)) = old {
            sqlx::query("DELETE FROM content_fts WHERE rowid = ?").bind(rowid).execute(&mut *tx).await?;
        }
        let (state, note, text) = match &outcome {
            Outcome::Text(text) => ("indexed", "", Some(text)),
            Outcome::Skipped(note) => ("skipped", *note, None),
            Outcome::Failed(note) => ("failed", *note, None),
            Outcome::Later => return Ok(0),
        };
        let mut rowid = None;
        if let Some(text) = text {
            rowid = Some(sqlx::query("INSERT INTO content_fts (body) VALUES (?)").bind(text).execute(&mut *tx).await?.last_insert_rowid());
        }
        let bytes = text.map_or(0, |t| t.len() as i64);
        sqlx::query(
            "INSERT INTO content_index (node_id, stamp, state, fts_rowid, bytes, note, indexed_at) VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (node_id) DO UPDATE SET stamp = excluded.stamp, state = excluded.state, fts_rowid = excluded.fts_rowid,
               bytes = excluded.bytes, note = excluded.note, indexed_at = excluded.indexed_at",
        )
        .bind(id)
        .bind(stamp)
        .bind(state)
        .bind(rowid)
        .bind(bytes)
        .bind(note)
        .bind(now())
        .execute(&mut *tx)
        .await?;
        dequeue(&mut tx, id).await?;
        Ok(bytes - old.map_or(0, |o| o.1))
    }
    .await;
    let delta = db::settle(tx, res).await?;
    let total = &st.part::<Memory>().total;
    if total.load(Ordering::Relaxed) >= 0 {
        total.fetch_add(delta, Ordering::Relaxed);
    }
    Ok(())
}
