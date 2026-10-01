//! Copies of spaces kept on another storage location (Control panel › Backups).
//!
//! "Copy everything to…" (a storage location's menu) copies the spaces of a location, with their trash and earlier
//! versions, into a folder of their own on another location: a *set* (`backup_sets`), kept in
//! `.thirtyfile-backups/<set id>/` there (layout.rs). The source isn't changed: no file is removed, no space switches
//! location, and the default location stays. A copy is used by restoring a space of it into a new folder (restore.rs).
//!
//! The work is done by jobs in the background, one at a time (runner.rs), each resumable after a pause, a failure or a
//! restart:
//! - `snapshot` (capture.rs): copies the content of the spaces and writes a snapshot's manifest, then its completion
//!   marker. Content the manifest records is kept from deletion until it is copied (`pinned`), so the snapshot holds
//!   what the spaces had when their files were listed;
//! - `restore` (restore.rs), `verify` and `remove` (tidy.rs).
//!
//! Copies are ThirtyFile's own: removing unused content and deleting content nothing uses only look at a location's
//! content store, never in `.thirtyfile-backups`, and a location holding copies can't be deleted.
//!
//! Administrators see which spaces a copy holds (a personal space as "My files" and its owner), never names inside
//! personal spaces: failures don't name their items, and a personal space is only restored whole, into its owner's
//! personal space.

pub mod api;
pub(crate) mod capture;
pub mod layout;
pub mod policy;
mod restore;
pub(crate) mod runner;
mod tidy;

#[cfg(test)]
mod policy_tests;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};

use runner::{ACTIVE, Job};
pub use runner::{Queue, spawn_runner};

use crate::{
    error::{AppError, AppResult},
    state::AppState,
    util::now,
};

/// A space in a snapshot, as listed without reading its manifest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceInfo {
    pub id: String,
    pub name: String,
    pub kind: String,
    /// Personal spaces: the owner's user name
    #[serde(default)]
    pub owner: String,
    /// Personal spaces: the owner's account (a personal space is only restored into that account's)
    #[serde(default)]
    pub owner_id: Option<i64>,
    pub mode: String,
    /// Files (the trash included) and their bytes, earlier versions included
    #[serde(default)]
    pub files: i64,
    #[serde(default)]
    pub bytes: i64,
}

/// A set as the database has it
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Set {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub source_name: String,
    pub dest_location: String,
    pub removing: bool,
    pub created_at: i64,
}

pub async fn load_set(db: &SqlitePool, id: &str) -> AppResult<Set> {
    sqlx::query_as("SELECT id, kind, name, source_name, dest_location, removing, created_at FROM backup_sets WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::not_found("This copy no longer exists"))
}

/// Content that a snapshot job which isn't over still has to copy: background deletion leaves it alone until then
/// (tree::claim_for_deletion), so the snapshot can hold what its manifest records
pub async fn pinned(db: &SqlitePool, hash: &str) -> Result<bool, sqlx::Error> {
    let row: Option<(i64,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT 1 FROM backup_pending p JOIN backup_jobs j ON j.id = p.job_id WHERE p.hash = ? AND j.state IN {ACTIVE} LIMIT 1"
    )))
    .bind(hash)
    .fetch_optional(db)
    .await?;
    Ok(row.is_some())
}

/// How many sets are kept on a location (a location holding some can't be deleted, nor its folder changed)
pub async fn sets_on(conn: &mut SqliteConnection, location: &str) -> Result<i64, sqlx::Error> {
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM backup_sets WHERE dest_location = ?").bind(location).fetch_one(conn).await?;
    Ok(n)
}

/// Whether a job that isn't over reads from this location's spaces or writes to it
pub async fn location_busy(conn: &mut SqliteConnection, location: &str) -> Result<bool, sqlx::Error> {
    let row: Option<(i64,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT 1 FROM backup_jobs j JOIN backup_sets s ON s.id = j.set_id
         WHERE (s.dest_location = ?1 OR s.source_location = ?1) AND j.state IN {ACTIVE} LIMIT 1"
    )))
    .bind(location)
    .fetch_optional(conn)
    .await?;
    Ok(row.is_some())
}

/// Bytes of the sets kept on each location (Storage usage)
pub async fn bytes_by_location(db: &SqlitePool) -> Result<Vec<(String, i64)>, sqlx::Error> {
    sqlx::query_as("SELECT s.dest_location, COALESCE(SUM(o.size), 0) FROM backup_sets s LEFT JOIN backup_objects o ON o.set_id = s.id GROUP BY s.dest_location")
        .fetch_all(db)
        .await
}

/// Writes an activity log entry about a job, as done by whoever asked for it
async fn log(conn: &mut SqliteConnection, job: &Job, action: &str, detail: &str) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO activity (at, user_id, username, action, detail) VALUES (?, ?, ?, ?, ?)")
        .bind(now())
        .bind(job.created_by)
        .bind(&job.created_by_name)
        .bind(action)
        .bind(detail)
        .execute(conn)
        .await?;
    Ok(())
}

/// A job asked to be cancelled: it ends as cancelled. A snapshot's content stops being kept for it, and a copy that
/// never completed is removed from its destination (the source was never changed); a restore keeps what it brought
/// back.
pub(crate) async fn cancelled(st: &AppState, job: &Job) -> AppResult<()> {
    let remove_set = {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            sqlx::query("UPDATE backup_jobs SET state = 'cancelled', finished_at = ?, error = NULL WHERE id = ?")
                .bind(now())
                .bind(&job.id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM backup_pending WHERE job_id = ?").bind(&job.id).execute(&mut *tx).await?;
            sqlx::query("DELETE FROM backup_restored WHERE job_id = ?").bind(&job.id).execute(&mut *tx).await?;
            let mut remove_set = false;
            if job.kind == "snapshot" {
                sqlx::query("DELETE FROM backup_snapshots WHERE id = (SELECT snapshot_id FROM backup_jobs WHERE id = ?) AND state = 'making'")
                    .bind(&job.id)
                    .execute(&mut *tx)
                    .await?;
                // A copy is one snapshot: without it there is nothing to keep
                let (kind, complete): (String, i64) = sqlx::query_as(
                    "SELECT kind, (SELECT COUNT(*) FROM backup_snapshots WHERE set_id = s.id AND state = 'complete') FROM backup_sets s WHERE id = ?",
                )
                .bind(&job.set_id)
                .fetch_optional(&mut *tx)
                .await?
                .unwrap_or_default();
                remove_set = kind == "copy" && complete == 0;
            }
            log(&mut tx, job, "backup_cancel", &job.label).await?;
            if remove_set {
                api::queue_remove(&mut tx, job.created_by, &job.created_by_name, &job.set_id).await?;
            }
            AppResult::Ok(remove_set)
        }
        .await;
        crate::db::settle(tx, res).await?
    };
    if job.kind == "snapshot" {
        let snap: Option<(Option<String>,)> = sqlx::query_as("SELECT snapshot_id FROM backup_jobs WHERE id = ?").bind(&job.id).fetch_optional(&st.db).await?;
        if let Some((Some(snap),)) = snap {
            let _ = tokio::fs::remove_file(layout::cached_manifest(st, &snap)).await;
        }
    }
    if remove_set {
        st.backups.wake.notify_one();
    }
    Ok(())
}
