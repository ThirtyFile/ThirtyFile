//! Replicas (Control panel › Replicas): checked copies of the current content of spaces, kept on other storage
//! locations, so the files can still be read when their location fails, and so another location can take over as
//! theirs (a promotion).
//!
//! A replica policy takes the spaces of a location (its primary) and keeps each content they use on its first `copies`
//! targets, by priority, that aren't where that content is kept. Folder spaces are read from their folder, which other
//! programs change too (folders/). Copies are made soon after changes or on
//! a schedule, per target (sync.rs), each checked by reading it back, and read back again every few days (`verify`).
//! A copy goes where the target keeps content (its content store), so a promotion only has to point the content at it.
//!
//! What ThirtyFile keeps is recorded (`replica_copies`): background deletion and removing unused content leave a
//! recorded copy alone (tree::claim_for_deletion, location_tools), a location holding copies can't be deleted, and a
//! copy only goes once its row went first: when nothing uses its content any more, or when an administrator removes
//! copies no longer needed (never as a side effect of pausing a policy or removing a target).
//!
//! - Reads fall back to a checked copy of the same content when the primary can't be read (`open_fallback`): the same
//!   content, so never an older version of the file.
//! - A promotion (api.rs) makes a target the spaces' location in one transaction: the content with a checked copy there
//!   is pointed at it, the spaces follow, and the old primary becomes a target whose copies are checked before they
//!   count again. Content without a copy there stays where it was and is listed: a promotion is never called lossless
//!   when it isn't. Nothing is deleted from the old primary, and nothing fails back by itself.
//!
pub mod api;
pub mod folders;
mod policy;
mod sync;

#[cfg(test)]
mod tests;

use std::collections::HashSet;

use futures_util::future::BoxFuture;
use serde::Serialize;
use sqlx::{SqliteConnection, SqlitePool};

use crate::{
    backups::runner::{Ctx, Engine, Job, Queue, Stop},
    error::{AppError, AppResult},
    state::AppState,
    util::now,
};

pub use policy::spawn_scheduler;

/// What replicas keep in memory (a part of `AppState`)
pub struct Memory {
    /// Replica jobs running now
    pub queue: crate::backups::Queue,
    /// When reads last fell back to a replica, by location
    pub fallbacks: crate::sync::Mutex<std::collections::HashMap<String, i64>>,
}

impl Default for Memory {
    fn default() -> Memory {
        Memory { queue: crate::backups::Queue::replicas(), fallbacks: Default::default() }
    }
}

/// The engine of replica jobs
pub struct ReplicaEngine;

pub static REPLICAS: ReplicaEngine = ReplicaEngine;

impl Engine for ReplicaEngine {
    type Job = Job;

    fn queue<'a>(&self, st: &'a AppState) -> &'a Queue {
        &st.part::<Memory>().queue
    }

    fn run<'a>(&'a self, cx: &'a Ctx<'a>) -> BoxFuture<'a, AppResult<Stop>> {
        Box::pin(async move {
            match cx.job.kind.as_str() {
                "sync" => sync::sync(cx).await,
                "verify" => sync::verify(cx).await,
                _ => Err(AppError::internal("unknown job")),
            }
        })
    }

    fn cancelled<'a>(&'a self, st: &'a AppState, job: &'a Job) -> BoxFuture<'a, AppResult<()>> {
        Box::pin(async move {
            let _w = st.write_lock.lock().await;
            sqlx::query("UPDATE replica_jobs SET state = 'cancelled', finished_at = ?, error = NULL WHERE id = ?").bind(now()).bind(&job.id).execute(&st.db).await?;
            Ok(())
        })
    }

    fn waits<'a>(&'a self, st: &'a AppState, job: &'a Job, _e: &'a AppError) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let Some(target) = &job.snapshot_id else { return false };
            crate::locations::probe(st, target).await.is_err()
        })
    }

    fn private<'a>(&'a self, st: &'a AppState, _job: &'a Job) -> BoxFuture<'a, AppResult<HashSet<String>>> {
        // Contents are named by their hash; files of folder spaces by their path, except in personal spaces
        Box::pin(async move {
            let rows: Vec<(String,)> = sqlx::query_as("SELECT id FROM drives WHERE kind = 'personal'").fetch_all(&st.db).await?;
            Ok(rows.into_iter().map(|(d,)| d).collect())
        })
    }

    fn log_failure<'a>(&'a self, conn: &'a mut SqliteConnection, job: &'a Job, error: &'a str) -> BoxFuture<'a, AppResult<()>> {
        Box::pin(async move { Ok(crate::backups::log(conn, job, "replica_failed", &format!("{}: {error}", job.label)).await?) })
    }
}

/// Starts the runner of replica jobs and the scheduler of replica policies
pub fn spawn_runner(st: AppState) {
    spawn_scheduler(st.clone());
    crate::backups::runner::spawn(st, &REPLICAS);
}

/// A replica ThirtyFile keeps on a location: it goes only once its record went first, so background deletion forgets
/// it. And a location a replica took over from, not checked since, takes no new content (`fenced`).
pub const KEEPER: crate::tree::Keeper = crate::tree::Keeper { what: "a replica", holds, refuses: Some(refuses) };

fn holds<'a>(db: &'a SqlitePool, hash: &'a str, location: &'a str) -> BoxFuture<'a, Result<crate::tree::Hold, sqlx::Error>> {
    Box::pin(async move { Ok(if kept(db, hash, location).await? { crate::tree::Hold::ForGood } else { crate::tree::Hold::No }) })
}

fn refuses<'a>(conn: &'a mut SqliteConnection, location: &'a str) -> BoxFuture<'a, Result<bool, sqlx::Error>> {
    Box::pin(fenced(conn, location))
}

/// Whether ThirtyFile keeps a copy of this content on this location as a replica: it isn't deleted meanwhile
pub async fn kept(db: &SqlitePool, hash: &str, location: &str) -> Result<bool, sqlx::Error> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM replica_copies WHERE hash = ? AND location_id = ?").bind(hash).bind(location).fetch_optional(db).await?;
    Ok(row.is_some())
}

/// Replica copies kept on a location, and whether a policy replicates from or to it: such a location can't be deleted
pub async fn location_used(conn: &mut SqliteConnection, location: &str) -> Result<(i64, bool), sqlx::Error> {
    let (copies, policies): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM replica_copies WHERE location_id = ?1),
                (SELECT COUNT(*) FROM replica_policies WHERE source_location = ?1) + (SELECT COUNT(*) FROM replica_targets WHERE location_id = ?1)",
    )
    .bind(location)
    .fetch_one(conn)
    .await?;
    Ok((copies, policies > 0))
}

/// Whether a location is an old primary after a promotion, not checked since: new content isn't stored there, so it
/// can't take writes the new primary doesn't have (tree::commit_blob)
pub async fn fenced(conn: &mut SqliteConnection, location: &str) -> Result<bool, sqlx::Error> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM replica_targets WHERE location_id = ? AND state = 'stale' LIMIT 1").bind(location).fetch_optional(conn).await?;
    Ok(row.is_some())
}

/// The spaces a policy replicates now: every one on its location, or the chosen ones still there; `mode`: only
/// content-store spaces ('store') or folder spaces ('folder')
async fn scope_of(conn: &mut SqliteConnection, policy: &str, mode: Option<&str>) -> AppResult<Vec<String>> {
    Ok(sqlx::query_as::<_, (String,)>(
        "SELECT d.id FROM drives d JOIN replica_policies p ON p.id = ?1
         WHERE d.location_id = p.source_location AND (?2 IS NULL OR d.mode = ?2)
           AND (p.all_spaces = 1 OR d.id IN (SELECT drive_id FROM replica_policy_spaces WHERE policy_id = ?1))
         ORDER BY d.id",
    )
    .bind(policy)
    .bind(mode)
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|(d,)| d)
    .collect())
}

/// Every space a policy replicates now
pub async fn scope(conn: &mut SqliteConnection, policy: &str) -> AppResult<Vec<String>> {
    scope_of(conn, policy, None).await
}

/// The content-store spaces a policy replicates now
pub async fn store_scope(conn: &mut SqliteConnection, policy: &str) -> AppResult<Vec<String>> {
    scope_of(conn, policy, Some("store")).await
}

/// The folder spaces a policy replicates now
pub async fn folder_scope(conn: &mut SqliteConnection, policy: &str) -> AppResult<Vec<String>> {
    scope_of(conn, policy, Some("folder")).await
}

/// Whether the spaces `?1` (JSON list) use the content `hash` (an SQL expression): a file or an earlier version of
/// theirs has it. Looked up by the content, so a walk of the content in hash order reads only the content it looks at,
/// instead of gathering every content of the spaces first.
fn in_scope(hash: &str) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM nodes n WHERE n.blob_hash = {hash} AND n.drive_id IN (SELECT value FROM json_each(?1)))
          OR EXISTS (SELECT 1 FROM node_versions v JOIN nodes n ON n.id = v.node_id
                     WHERE v.blob_hash = {hash} AND n.drive_id IN (SELECT value FROM json_each(?1))))"
    )
}

/// Content the spaces `?1` (JSON list) use: their files and earlier versions
const SCOPE_HASHES: &str = "SELECT blob_hash AS hash FROM nodes WHERE drive_id IN (SELECT value FROM json_each(?1)) AND blob_hash IS NOT NULL
     UNION SELECT v.blob_hash FROM node_versions v JOIN nodes n ON n.id = v.node_id WHERE n.drive_id IN (SELECT value FROM json_each(?1)) AND v.blob_hash IS NOT NULL";

/// The targets that should hold a content whose primary is `primary`: the first `copies` active targets, by
/// priority, that aren't where it is
pub fn required<'a>(targets: &'a [Target], copies: i64, primary: &str) -> Vec<&'a str> {
    targets.iter().filter(|t| t.state == "active" && t.location_id != primary).take(copies.max(0) as usize).map(|t| t.location_id.as_str()).collect()
}

/// A target of a policy
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Target {
    pub location_id: String,
    pub priority: i64,
    pub mode: String,
    #[serde(skip)]
    pub schedule: String,
    pub tz: String,
    pub next_run_at: Option<i64>,
    pub state: String,
    pub synced_at: Option<i64>,
    pub last_run_at: Option<i64>,
    pub last_verify_at: Option<i64>,
    #[serde(skip)]
    pub catch_up: bool,
    /// How many contents it holds of those it should, as last worked out (None: not yet); shown with its health
    #[serde(skip)]
    pub held: Option<i64>,
    #[serde(skip)]
    pub wanted: Option<i64>,
}

pub const TARGET_COLS: &str = "location_id, priority, mode, schedule, tz, next_run_at, state, synced_at, last_run_at, last_verify_at, catch_up, held, wanted";

pub async fn targets(conn: &mut SqliteConnection, policy: &str) -> AppResult<Vec<Target>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {TARGET_COLS} FROM replica_targets WHERE policy_id = ? ORDER BY priority, location_id")))
        .bind(policy)
        .fetch_all(conn)
        .await?)
}

/// Forgets the counts the Replicas page shows, to be worked out again soon (policy.rs): those of a policy's targets
/// (every policy's with None), and the copies no policy wants on each location
pub async fn forget_counts(conn: &mut SqliteConnection, policy: Option<&str>) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE replica_targets SET held = NULL, wanted = NULL WHERE ?1 IS NULL OR policy_id = ?1").bind(policy).execute(&mut *conn).await?;
    sqlx::query("DELETE FROM replica_unneeded").execute(&mut *conn).await?;
    Ok(())
}

/// What the replica policies and their targets are as of now: a count worked out under an earlier stamp is out of
/// date (a policy was made, changed, promoted or deleted meanwhile)
pub(crate) async fn stamp(conn: &mut SqliteConnection) -> Result<String, sqlx::Error> {
    let (s,): (Option<String>,) = sqlx::query_as(
        "SELECT (SELECT group_concat(id || ' ' || updated_at || ' ' || epoch || ' ' || copies || ' ' || all_spaces, ',') FROM replica_policies)
                || '|' || COALESCE((SELECT group_concat(policy_id || ' ' || location_id || ' ' || priority || ' ' || state, ',') FROM replica_targets), '')
                || '|' || COALESCE((SELECT group_concat(policy_id || ' ' || drive_id, ',') FROM replica_policy_spaces), '')",
    )
    .fetch_one(conn)
    .await?;
    Ok(s.unwrap_or_default())
}

/// A policy as the database has it
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Policy {
    pub id: String,
    pub name: String,
    pub source_location: String,
    pub enabled: bool,
    pub all_spaces: bool,
    pub copies: i64,
    pub read_fallback: bool,
    pub verify_days: i64,
    pub alert_hours: i64,
    pub rate_limit: i64,
    pub epoch: i64,
    #[serde(skip)]
    pub alerted: String,
    pub created_by_name: String,
    pub created_at: i64,
    pub updated_at: i64,
}

pub const POLICY_COLS: &str = "id, name, source_location, enabled, all_spaces, copies, read_fallback, verify_days, alert_hours, rate_limit, epoch, alerted, created_by_name, created_at, updated_at";

pub async fn load(conn: &mut SqliteConnection, id: &str) -> AppResult<Option<Policy>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {POLICY_COLS} FROM replica_policies WHERE id = ?"))).bind(id).fetch_optional(conn).await?)
}

/// Reads a content from a checked replica when its primary can't be read: the same content (it is named by its
/// SHA-256), so never an older version of the file. Only copies of policies that allow it are used; each fallback is
/// noted in the log, and in the activity log at most once in a while per location.
pub async fn open_fallback(st: &AppState, hash: &str, primary: &str, start: u64, len: u64) -> std::io::Result<crate::storage::BoxReader> {
    let copies: Vec<(String,)> = sqlx::query_as(
        "SELECT c.location_id FROM replica_copies c
         WHERE c.hash = ? AND c.state = 'verified' AND c.location_id != ?
           AND EXISTS (SELECT 1 FROM replica_targets t JOIN replica_policies p ON p.id = t.policy_id
                       WHERE t.location_id = c.location_id AND p.read_fallback = 1)
         ORDER BY c.verified_at DESC",
    )
    .bind(hash)
    .bind(primary)
    .fetch_all(&st.db)
    .await
    .map_err(std::io::Error::other)?;
    let mut last = std::io::Error::new(std::io::ErrorKind::NotFound, "no replica");
    for (location,) in copies {
        let Ok(storage) = st.storage(&location) else { continue };
        match storage.open(hash, start, len).await {
            Ok(r) => {
                note_fallback(st, primary, &location).await;
                return Ok(r);
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Whether a content whose primary can't be read can be read from a checked replica on a location that works: its
/// file then opens as usual instead of showing its location as offline
pub async fn readable_elsewhere(st: &AppState, conn: &mut SqliteConnection, hash: &str, primary: &str) -> AppResult<bool> {
    let copies: Vec<(String,)> = sqlx::query_as(
        "SELECT c.location_id FROM replica_copies c
         WHERE c.hash = ? AND c.state = 'verified' AND c.location_id != ?
           AND EXISTS (SELECT 1 FROM replica_targets t JOIN replica_policies p ON p.id = t.policy_id
                       WHERE t.location_id = c.location_id AND p.read_fallback = 1)",
    )
    .bind(hash)
    .bind(primary)
    .fetch_all(conn)
    .await?;
    Ok(copies.iter().any(|(l,)| st.location_offline(l).is_none()))
}

pub(super) async fn note_fallback(st: &AppState, primary: &str, replica: &str) {
    tracing::warn!("A file of the storage location {primary} was read from its replica on {replica}: {primary} couldn't be read");
    let due = {
        // When reads last fell back from each location: the activity log says so at most every ten minutes
        let key = primary.to_string();
        let mut seen = st.part::<Memory>().fallbacks.lock();
        let last = seen.get(&key).copied().unwrap_or(0);
        let due = now() - last >= 600;
        if due {
            seen.insert(key, now());
        }
        due
    };
    if !due {
        return;
    }
    let names: Vec<(String, String)> =
        sqlx::query_as("SELECT id, name FROM storage_locations WHERE id IN (?, ?)").bind(primary).bind(replica).fetch_all(&st.db).await.unwrap_or_default();
    let name = |id: &str| names.iter().find(|(i, _)| i == id).map_or(id.to_string(), |(_, n)| n.clone());
    let _w = st.write_lock.lock().await;
    let _ = sqlx::query("INSERT INTO activity (at, action, detail) VALUES (?, 'replica_read', ?)")
        .bind(now())
        .bind(format!("{} → {}", name(primary), name(replica)))
        .execute(&st.db)
        .await;
}
