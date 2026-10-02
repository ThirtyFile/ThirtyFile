//! Content store to content store (S3, SFTP, FTP, or a local content store): every content the space uses that isn't
//! at the target yet is copied there and checked against its SHA-256. The space stays in use meanwhile: what it gets
//! while its content is copied is copied too, before the switch. The switch points the content at the target in one
//! transaction; the old copies are deleted a minute later, so downloads reading them can finish.
//!
//! Content is shared by every file with the same content (deduplicated across the system): content this space shares
//! with spaces elsewhere moves with it, and they read it from the target from then on.

use std::sync::Arc;

use sqlx::SqliteConnection;

use super::{Ctx, Job, Stop};
use crate::{
    error::{AppError, AppResult},
    state::AppState,
    storage::Storage,
    tree::{self, REMOVAL_GRACE},
    util::{new_id, now},
};

/// Content the space `?2` uses (its files and their earlier versions) that isn't at the target `?1`, after the hash
/// `?3`, and that the move `?4` hasn't copied from where it is now
const PENDING: &str = "FROM blobs b WHERE b.location_id != ?1 AND b.hash > ?3
     AND (EXISTS (SELECT 1 FROM nodes n WHERE n.blob_hash = b.hash AND n.drive_id = ?2)
          OR EXISTS (SELECT 1 FROM node_versions v JOIN nodes n ON n.id = v.node_id WHERE v.blob_hash = b.hash AND n.drive_id = ?2))
     AND NOT EXISTS (SELECT 1 FROM space_move_items i WHERE i.move_id = ?4 AND i.item_id = b.hash AND i.from_location = b.location_id)";
/// The same content as `PENDING`, found from the space's files and versions rather than by reading every content in the
/// system: for counting and for the checks made under the write lock (before the switch, when a move is asked for).
/// `?3` isn't used.
pub(super) const PENDING_IN_SPACE: &str = "FROM blobs b
     WHERE b.hash IN (SELECT blob_hash FROM nodes WHERE drive_id = ?2 AND blob_hash IS NOT NULL
                      UNION SELECT v.blob_hash FROM node_versions v JOIN nodes n ON n.id = v.node_id WHERE n.drive_id = ?2 AND v.blob_hash IS NOT NULL)
       AND b.location_id != ?1
       AND NOT EXISTS (SELECT 1 FROM space_move_items i WHERE i.move_id = ?4 AND i.item_id = b.hash AND i.from_location = b.location_id)";
/// Content copied per page
const PAGE: i64 = 50;
/// Copy rounds before the switch: each copies what the space got during the one before
const ROUNDS: usize = 10;
/// Copied content not recorded yet (ThirtyFile stopped in between) is deleted after this long, unless used by then
const UNRECORDED_GRACE: i64 = 24 * 3600;

/// Whether some content of a space on `location` is kept elsewhere
pub async fn scattered(conn: &mut SqliteConnection, drive_id: &str, location: &str) -> AppResult<bool> {
    let sql = format!("SELECT 1 {PENDING_IN_SPACE} LIMIT 1");
    let row: Option<(i64,)> = sqlx::query_as(sqlx::AssertSqlSafe(sql)).bind(location).bind(drive_id).bind("").bind("").fetch_optional(conn).await?;
    Ok(row.is_some())
}

pub async fn run(cx: &Ctx<'_>) -> AppResult<Stop> {
    let (st, job) = (cx.st, cx.job);
    let dst = st.storage(&job.to_location)?;
    // What an earlier run copied, and what is left
    let (files_done, bytes_done): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(size), 0) FROM space_move_items WHERE move_id = ?").bind(&job.id).fetch_one(&st.db).await?;
    let (files, bytes) = left(st, job).await?;
    cx.set_counts(files_done, bytes_done, files_done + files, bytes_done + bytes);
    cx.flush().await?;
    for round in 0..ROUNDS {
        if round > 0 {
            let (files, bytes) = left(st, job).await?;
            cx.add_total(files, bytes);
        }
        if let Some(stop) = copy_left(cx, &dst).await? {
            return Ok(stop);
        }
        let failed = cx.failed_count();
        if failed > 0 {
            return Err(AppError::new(
                axum::http::StatusCode::BAD_GATEWAY,
                if failed == 1 { "1 file couldn't be copied".to_string() } else { format!("{failed} files couldn't be copied") },
            ));
        }
        if let Some(stop) = cx.stop() {
            return Ok(stop);
        }
        if switch(cx).await? {
            return Ok(Stop::Done);
        }
    }
    Err(AppError::conflict("The space kept changing while it was being moved. Try again later."))
}

/// Content left to copy: how many, and their bytes
async fn left(st: &AppState, job: &Job) -> AppResult<(i64, i64)> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT COUNT(*), COALESCE(SUM(b.size), 0) {PENDING_IN_SPACE}")))
        .bind(&job.to_location)
        .bind(&job.drive_id)
        .bind("")
        .bind(&job.id)
        .fetch_one(&st.db)
        .await?)
}

/// Copies the content not copied yet, a page at a time in hash order (each page continues after the last hash, so a
/// page costs the same at the end of a large space as at the start)
async fn copy_left(cx: &Ctx<'_>, dst: &Arc<dyn Storage>) -> AppResult<Option<Stop>> {
    let (st, job) = (cx.st, cx.job);
    let mut last = String::new();
    loop {
        let rows: Vec<(String, i64, String)> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT b.hash, b.size, b.location_id {PENDING} ORDER BY b.hash LIMIT {PAGE}")))
                .bind(&job.to_location)
                .bind(&job.drive_id)
                .bind(&last)
                .bind(&job.id)
                .fetch_all(&st.db)
                .await?;
        let Some((hash, ..)) = rows.last() else { return Ok(None) };
        last = hash.clone();
        for (hash, size, from) in rows {
            if let Some(stop) = cx.stop() {
                return Ok(Some(stop));
            }
            if let Some(stop) = copy_one(cx, dst, &hash, size, &from).await? {
                return Ok(Some(stop));
            }
        }
    }
}

/// Copies one content to the target and records it
async fn copy_one(cx: &Ctx<'_>, dst: &Arc<dyn Storage>, hash: &str, size: i64, from: &str) -> AppResult<Option<Stop>> {
    let (st, job) = (cx.st, cx.job);
    // Held until the copy is recorded: a deletion of this content at the target still pending (from an earlier move
    // away from it) waits, or is waited for; once recorded, the move protects it (claim_for_deletion)
    let _staging = tree::stage_guard(st, hash).await;
    defer_removal(st, hash, &job.to_location, UNRECORDED_GRACE).await?;
    let src = st.storage(from)?;
    let copied = cx.tries(|e: &std::io::Error| e.kind() != std::io::ErrorKind::NotFound, || copy_verified(st, &src, dst, hash, size)).await;
    match copied {
        Ok(()) => {}
        Err(Ok(stop)) => return Ok(Some(stop)),
        Err(Err(e)) => {
            // Deleted meanwhile, or moved elsewhere: nothing to copy for it any more (a copy now at the target goes)
            let current: Option<(String,)> = sqlx::query_as("SELECT location_id FROM blobs WHERE hash = ?").bind(hash).fetch_optional(&st.db).await?;
            if current.as_ref().is_none_or(|(loc,)| loc != from) {
                cx.done(1, 0).await?;
                return Ok(None);
            }
            if e.kind() == std::io::ErrorKind::NotFound || crate::hashing::unusable_kind(&e) == Some(crate::hashing::Unusable::Damaged) {
                // The content itself is missing or damaged where it is: listed, and the other files are still copied
                let name: Option<(String,)> = sqlx::query_as("SELECT name FROM nodes WHERE blob_hash = ? AND drive_id = ? LIMIT 1")
                    .bind(hash)
                    .bind(&job.drive_id)
                    .fetch_optional(&st.db)
                    .await?;
                cx.failed(&cx.job.drive_id, name.map(|(n,)| n), e.to_string());
                return Ok(None);
            }
            return Err(AppError::new(axum::http::StatusCode::BAD_GATEWAY, format!("Failed to move files: {}", crate::locations::describe(&e))));
        }
    }
    {
        let _w = st.write_lock.lock().await;
        sqlx::query("INSERT OR REPLACE INTO space_move_items (move_id, item_id, hash, from_location, size) VALUES (?, ?, ?, ?, ?)")
            .bind(&job.id)
            .bind(hash)
            .bind(hash)
            .bind(from)
            .bind(size)
            .execute(&st.db)
            .await?;
    }
    cx.done(1, size).await?;
    Ok(None)
}

/// Lists content at a location for deletion after `delay` seconds, unless it is used by then (an earlier date stays)
pub(super) async fn defer_removal(st: &AppState, hash: &str, location: &str, delay: i64) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    sqlx::query(
        "INSERT INTO pending_blob_deletes (hash, location_id, created_at, attempts, last_error) VALUES (?, ?, ?, 0, 'deferred')
         ON CONFLICT (hash, location_id) DO UPDATE SET created_at = MIN(created_at, excluded.created_at)",
    )
    .bind(hash)
    .bind(location)
    .bind(now() + delay)
    .execute(&st.db)
    .await?;
    Ok(())
}

const VERIFY_FAILED: &str = "Verification of the copied content failed";

/// Copies one content to `dst` through a temp file, checking its SHA-256 and size
async fn copy_verified(st: &AppState, src: &Arc<dyn Storage>, dst: &Arc<dyn Storage>, hash: &str, size: i64) -> std::io::Result<()> {
    let tmp = st.tmp_dir().join(format!("move-{}", new_id()));
    let copied = async {
        // Hashed while it is copied, so the content is read once before it is stored at the target
        let mut reader = src.open(hash, 0, size as u64).await?;
        crate::hashing::copy_checked(&mut reader, hash, size as u64, &tmp, VERIFY_FAILED).await?;
        dst.put_file(hash, &tmp).await
    }
    .await;
    let _ = tokio::fs::remove_file(&tmp).await;
    copied
}

/// Once everything is copied: points the copied content at the target and the space at the new location, in one
/// transaction. False when the space got content meanwhile that isn't copied yet (copied in the next round).
async fn switch(cx: &Ctx<'_>) -> AppResult<bool> {
    let (st, job) = (cx.st, cx.job);
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        let pending: Option<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT b.hash {PENDING_IN_SPACE} LIMIT 1")))
            .bind(&job.to_location)
            .bind(&job.drive_id)
            .bind("")
            .bind(&job.id)
            .fetch_optional(&mut *tx)
            .await?;
        if pending.is_some() {
            return Ok(false);
        }
        let due = now() + REMOVAL_GRACE;
        // The old copies of what switches go a minute from now, so downloads reading them can finish
        sqlx::query(
            "INSERT INTO pending_blob_deletes (hash, location_id, created_at, attempts, last_error)
             SELECT i.hash, i.from_location, ?2, 0, 'deferred' FROM space_move_items i JOIN blobs b ON b.hash = i.hash AND b.location_id = i.from_location
             WHERE i.move_id = ?1
             ON CONFLICT (hash, location_id) DO UPDATE SET created_at = MIN(created_at, excluded.created_at)",
        )
        .bind(&job.id)
        .bind(due)
        .execute(&mut *tx)
        .await?;
        // Copies nothing uses: their content was deleted, or moved elsewhere, meanwhile
        sqlx::query(
            "INSERT INTO pending_blob_deletes (hash, location_id, created_at, attempts, last_error)
             SELECT i.hash, ?2, ?3, 0, 'deferred' FROM space_move_items i
             WHERE i.move_id = ?1 AND NOT EXISTS (SELECT 1 FROM blobs b WHERE b.hash = i.hash AND b.location_id IN (i.from_location, ?2))
             ON CONFLICT (hash, location_id) DO UPDATE SET created_at = MIN(created_at, excluded.created_at)",
        )
        .bind(&job.id)
        .bind(&job.to_location)
        .bind(due)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE blobs SET location_id = ?2 FROM space_move_items i WHERE i.move_id = ?1 AND blobs.hash = i.hash AND blobs.location_id = i.from_location",
        )
        .bind(&job.id)
        .bind(&job.to_location)
        .execute(&mut *tx)
        .await?;
        // The copies are used now: the entries that would have removed them had the move stopped go
        sqlx::query(
            "DELETE FROM pending_blob_deletes WHERE location_id = ?2 AND last_error = 'deferred'
               AND hash IN (SELECT i.hash FROM space_move_items i JOIN blobs b ON b.hash = i.hash AND b.location_id = ?2 WHERE i.move_id = ?1)",
        )
        .bind(&job.id)
        .bind(&job.to_location)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE drives SET location_id = ? WHERE id = ?").bind(&job.to_location).bind(&job.drive_id).execute(&mut *tx).await?;
        super::finish(&mut tx, cx, false, None).await?;
        Ok(true)
    }
    .await;
    crate::db::settle(tx, res).await
}

/// Removes what a cancelled move copied: each copy is checked again before it is deleted (content that is at the
/// target anyway, moved there by something else meanwhile, stays)
pub async fn remove_copies(st: &AppState, job: &Job) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        sqlx::query(
            "INSERT INTO pending_blob_deletes (hash, location_id, created_at, attempts, last_error)
             SELECT hash, ?2, ?3, 0, 'deferred' FROM space_move_items WHERE move_id = ?1
             ON CONFLICT (hash, location_id) DO UPDATE SET created_at = MIN(created_at, excluded.created_at)",
        )
        .bind(&job.id)
        .bind(&job.to_location)
        .bind(now() + REMOVAL_GRACE)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM space_move_items WHERE move_id = ?").bind(&job.id).execute(&mut *tx).await?;
        AppResult::Ok(())
    }
    .await;
    crate::db::settle(tx, res).await
}
