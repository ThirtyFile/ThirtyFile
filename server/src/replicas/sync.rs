//! Syncing a target: copying what it should hold, checking what it holds.
//!
//! A sync of a policy's target:
//! 1. An old primary (a target 'stale' after a promotion) is checked first: each copy it has is read back, and kept only
//!    when it is whole; a damaged one is replaced. Copies found damaged by a check are replaced the same way.
//! 2. Each content the spaces use that the target should hold and doesn't is copied from where it is kept (or, when
//!    that can't be read, from another checked replica), stored where the target keeps content, read back and checked,
//!    then recorded. Until it is recorded, the copy is listed for deletion a day later, so one a stop left unrecorded
//!    doesn't stay; content deleted meanwhile isn't recorded. Folder spaces are read from their folder (folders.rs).
//! 3. Copies of content nothing uses any more lose their row and are deleted like any content nothing uses.
//! 4. The changes it holds are recorded: the target is current until the spaces change again.
//!
//! A job asked for before a promotion is refused: the content it would copy may be somewhere else now.

use std::{collections::HashMap, sync::Arc};

use super::{SCOPE_HASHES, Target, in_scope};
use crate::{
    backups::runner::{Ctx, Stop},
    error::{AppError, AppResult},
    state::AppState,
    storage::Storage,
    tree::{self, REMOVAL_GRACE},
    util::{new_id, now},
};

/// Contents looked at per page
const PAGE: i64 = 200;
/// Copied content not recorded yet (ThirtyFile stopped in between) is deleted after this long, unless recorded by then
const UNRECORDED_GRACE: i64 = 24 * 3600;

/// The job's policy, target and the policy's targets; None when the job is out of date (the policy was promoted, or
/// the target removed, since it was asked for)
async fn context(cx: &Ctx<'_>) -> AppResult<Option<(super::Policy, Target, Vec<Target>)>> {
    let st = cx.st;
    let mut c = st.db.acquire().await?;
    let Some(policy) = super::load(&mut c, &cx.job.set_id).await? else { return Ok(None) };
    let params: serde_json::Value = serde_json::from_str(&cx.job.params).unwrap_or_default();
    if params["epoch"].as_i64() != Some(policy.epoch) {
        return Ok(None);
    }
    let targets = super::targets(&mut c, &policy.id).await?;
    let Some(target) = cx.job.snapshot_id.as_ref().and_then(|l| targets.iter().find(|t| &t.location_id == l)).cloned() else { return Ok(None) };
    Ok(Some((policy, target, targets)))
}

/// A location's name, as messages name it
pub(super) async fn name_of(st: &AppState, location: &str) -> String {
    let row: Option<(String,)> = sqlx::query_as("SELECT name FROM storage_locations WHERE id = ?").bind(location).fetch_optional(&st.db).await.unwrap_or(None);
    row.map_or_else(|| location.to_string(), |(n,)| n)
}

async fn unreachable(st: &AppState, location: &str, e: String) -> AppError {
    AppError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, format!("The storage location {} can't be reached: {e}", name_of(st, location).await))
}

/// Where a location keeps a content (for checking what is there)
fn content_key(s: &dyn Storage, hash: &str) -> String {
    crate::storage::join_key(&[s.content_dir(), &hash[0..2], &hash[2..4], hash])
}

/// Whether a location holds a content whole
pub(super) async fn whole(s: &dyn Storage, hash: &str, size: i64) -> bool {
    matches!(read_back(s, hash, size).await, Ok((h, n)) if h == hash && n == size as u64)
}

/// Puts a content from a temp file on the target, and reads it back: it counts only once it is known to be whole. A
/// whole copy already there (stored by a sync that stopped before recording it, say) is kept as it is; one of another
/// length, or damaged, is replaced by the checked content, never deleted first: the target may have become where the
/// content is kept meanwhile (a promotion), and what is there must not be gone in between. The caller holds the
/// content's staging guard. Err(stop) inside when the job is asked to stop while it is tried again.
pub(super) async fn put_verified(cx: &Ctx<'_>, dst: &Arc<dyn Storage>, location: &str, hash: &str, size: i64, tmp: &std::path::Path) -> AppResult<Result<(), Stop>> {
    if whole(dst.as_ref(), hash, size).await {
        return Ok(Ok(()));
    }
    let put = cx
        .tries(
            |_: &std::io::Error| true,
            || async {
                // Written in full first, then put in the content's place (`repair_file`): what is there stays until then
                dst.repair_file(hash, tmp).await?;
                match read_back(dst.as_ref(), hash, size).await? {
                    (h, n) if h == hash && n == size as u64 => Ok(()),
                    _ => Err(crate::hashing::unusable(crate::hashing::Unusable::Damaged, crate::backups::capture::DAMAGED)),
                }
            },
        )
        .await;
    match put {
        Ok(()) => Ok(Ok(())),
        Err(Ok(stop)) => Ok(Err(stop)),
        Err(Err(e)) => Err(AppError::new(
            axum::http::StatusCode::BAD_GATEWAY,
            format!("Couldn't copy to {}: {}", name_of(cx.st, location).await, crate::locations::describe(&e)),
        )),
    }
}

/// SHA-256 and length of a content as a location has it; NotFound when it isn't there
async fn read_back(s: &dyn Storage, hash: &str, size: i64) -> std::io::Result<(String, u64)> {
    if let Ok(Some(e)) = s.stat(&content_key(s, hash)).await
        && e.size != size as u64
    {
        return Ok((String::new(), e.size));
    }
    let mut reader = s.open(hash, 0, size as u64).await?;
    crate::hashing::read_async(&mut reader).await
}

pub async fn sync(cx: &Ctx<'_>) -> AppResult<Stop> {
    let st = cx.st;
    let Some((policy, target, mut targets)) = context(cx).await? else { return Ok(Stop::Cancelled) };
    let location = target.location_id;
    if let Err(e) = crate::locations::probe(st, &location).await {
        return Err(unreachable(st, &location, e).await);
    }
    let dst = st.storage(&location)?;
    let all = super::scope(&mut *st.db.acquire().await?, &policy.id).await?;
    let spaces = super::store_scope(&mut *st.db.acquire().await?, &policy.id).await?;
    // What the spaces hold now: recorded as held once everything is copied
    let mut seqs: Vec<(String, i64)> = sqlx::query_as("SELECT d.value, COALESCE((SELECT seq FROM space_changes WHERE drive_id = d.value), 0) FROM json_each(?) d")
        .bind(serde_json::to_string(&all).unwrap())
        .fetch_all(&st.db)
        .await?;
    // 1. Copies that must be checked before they count: an old primary's, and those a check found damaged
    let (recheck, recheck_bytes): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(size), 0) FROM replica_copies WHERE location_id = ? AND state IN ('stale', 'corrupt')")
            .bind(&location)
            .fetch_one(&st.db)
            .await?;
    // Counted for the progress only: not again when the target holds every change of the spaces already (each count
    // reads every content of the spaces)
    let unchanged = target.state == "active" && target.synced_at.is_some() && super::policy::changed(st, &policy, &location, &all).await?.is_empty();
    let pending = if unchanged { (0, 0) } else { pending_count(st, &spaces, &location).await? };
    cx.set_counts(0, 0, recheck + pending.0, recheck_bytes + pending.1);
    cx.flush().await?;
    let mut repaired = 0i64;
    loop {
        let rows: Vec<(String, i64, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT hash, size, state FROM replica_copies WHERE location_id = ? AND state IN ('stale', 'corrupt') ORDER BY hash LIMIT {PAGE}"
        )))
        .bind(&location)
        .fetch_all(&st.db)
        .await?;
        if rows.is_empty() {
            break;
        }
        for (hash, size, state) in rows {
            if let Some(stop) = cx.stop() {
                return Ok(stop);
            }
            let whole = state == "stale" && matches!(read_back(dst.as_ref(), &hash, size).await, Ok((h, n)) if h == hash && n == size as u64);
            if whole {
                let _w = st.write_lock.lock().await;
                sqlx::query("UPDATE replica_copies SET state = 'verified', verified_at = ? WHERE hash = ? AND location_id = ?")
                    .bind(now())
                    .bind(&hash)
                    .bind(&location)
                    .execute(&st.db)
                    .await?;
            } else {
                // Missing or damaged: it goes (deleted like content nothing uses, never the primary), and is copied
                // again below if the target should hold it
                drop_copies(st, &location, vec![hash.clone()]).await?;
                repaired += 1;
            }
            cx.done(1, size).await?;
        }
    }
    // Checked: an old primary counts again from here
    for t in targets.iter_mut().filter(|t| t.location_id == location) {
        t.state = "active".into();
    }
    // 2. What the target should hold and doesn't
    let mut copied = 0i64;
    let mut last = String::new();
    loop {
        let rows: Vec<(String, i64, String)> =
            sqlx::query_as(sqlx::AssertSqlSafe(missing_page())).bind(serde_json::to_string(&spaces).unwrap()).bind(&location).bind(&last).fetch_all(&st.db).await?;
        let Some((h, ..)) = rows.last() else { break };
        last = h.clone();
        for (hash, size, primary) in rows {
            if let Some(stop) = cx.stop() {
                return Ok(stop);
            }
            // Only where the target is one of the copies the content should have
            if !super::required(&targets, policy.copies, &primary).contains(&location.as_str()) {
                continue;
            }
            match copy_one(cx, &policy, &dst, &location, &hash, size, &primary).await? {
                Ok(true) => copied += 1,
                Ok(false) => {}
                Err(stop) => return Ok(stop),
            }
            cx.done(1, size).await?;
        }
    }
    // Folder spaces: read from their folder
    let changing = match super::folders::sync(cx, &policy, &targets, &location, &dst).await? {
        Ok((n, after_scan, changing)) => {
            copied += n;
            // A folder space holds what its check for changes found
            for (d, seq) in after_scan {
                if let Some(e) = seqs.iter_mut().find(|(x, _)| *x == d) {
                    e.1 = seq;
                }
            }
            changing
        }
        Err(stop) => return Ok(stop),
    };
    if let Some(e) = cx.failures_error() {
        return Err(e);
    }
    // 3. Copies of content nothing replicated uses any more
    let released = release(st, &location).await?;
    // 4. Held: the target is current, and counts again after a promotion
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        let (epoch,): (i64,) = sqlx::query_as("SELECT epoch FROM replica_policies WHERE id = ?").bind(&policy.id).fetch_one(&mut *tx).await?;
        if epoch != policy.epoch {
            return Err(AppError::conflict("The replicas were promoted meanwhile"));
        }
        sqlx::query(
            "INSERT INTO replica_captured (policy_id, location_id, drive_id, seq) SELECT ?1, ?2, json_extract(value, '$[0]'), json_extract(value, '$[1]') FROM json_each(?3) WHERE true
             ON CONFLICT (policy_id, location_id, drive_id) DO UPDATE SET seq = excluded.seq",
        )
        .bind(&policy.id)
        .bind(&location)
        .bind(serde_json::to_string(&seqs).unwrap())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM replica_dirty WHERE policy_id = ?1 AND location_id = ?2 AND drive_id IN (
               SELECT k.drive_id FROM replica_captured k LEFT JOIN space_changes c ON c.drive_id = k.drive_id
               WHERE k.policy_id = ?1 AND k.location_id = ?2 AND COALESCE(c.seq, 0) <= k.seq)",
        )
        .bind(&policy.id)
        .bind(&location)
        .execute(&mut *tx)
        .await?;
        // Spaces deleted, or taken out of the policy, since: never synced again, so what was recorded of them goes
        for table in ["replica_dirty", "replica_captured"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {table} WHERE policy_id = ?1 AND location_id = ?2 AND drive_id NOT IN (SELECT value FROM json_each(?3))"
            )))
            .bind(&policy.id)
            .bind(&location)
            .bind(serde_json::to_string(&all).unwrap())
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("DELETE FROM replica_folder_files WHERE drive_id NOT IN (SELECT id FROM drives)").execute(&mut *tx).await?;
        sqlx::query("UPDATE replica_targets SET synced_at = ?, state = 'active' WHERE policy_id = ? AND location_id = ?")
            .bind(now())
            .bind(&policy.id)
            .bind(&location)
            .execute(&mut *tx)
            .await?;
        let mut notes = Vec::new();
        if copied > 0 {
            notes.push(if copied == 1 { "1 content was copied".to_string() } else { format!("{copied} contents were copied") });
        }
        if repaired > 0 {
            notes.push(if repaired == 1 { "1 damaged or missing copy was replaced".to_string() } else { format!("{repaired} damaged or missing copies were replaced") });
        }
        if released > 0 {
            notes.push(if released == 1 { "1 copy nothing uses any more was let go".to_string() } else { format!("{released} copies nothing uses any more were let go") });
        }
        notes.extend(changing.notes());
        let note = (!notes.is_empty()).then(|| notes.join("\n"));
        crate::backups::runner::finish(&mut tx, cx, note.as_deref()).await?;
        AppResult::Ok(())
    }
    .await;
    crate::db::settle(tx, res).await?;
    Ok(Stop::Done)
}

/// Content the spaces `?1` use that the target `?2` doesn't hold a checked copy of (a condition on `blobs b`). The
/// content is walked in hash order, with its copy on the target, both from their indexes: only content without a copy
/// is read and looked up in the spaces' files and versions (`CASE` keeps SQLite from doing that first).
fn missing() -> String {
    format!(
        "LEFT JOIN replica_copies c ON c.hash = b.hash AND c.location_id = ?2 AND c.state = 'verified'
         WHERE b.hash > ?3 AND CASE WHEN c.hash IS NULL THEN b.location_id != ?2 AND {} ELSE 0 END",
        in_scope("b.hash")
    )
}

/// A page of the content the spaces `?1` use that the target `?2` doesn't hold a checked copy of, after the hash `?3`:
/// (hash, size, where it is kept). A page reads about as many contents as it returns, and a target that holds
/// everything is gone through once, from the indexes.
pub(super) fn missing_page() -> String {
    format!("SELECT b.hash, b.size, b.location_id FROM blobs b {} ORDER BY b.hash LIMIT {PAGE}", missing())
}

/// Content left to copy to the target: how many, and their bytes
async fn pending_count(st: &AppState, spaces: &[String], location: &str) -> AppResult<(i64, i64)> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT COUNT(*), COALESCE(SUM(b.size), 0) FROM blobs b {}", missing())))
        .bind(serde_json::to_string(spaces).unwrap())
        .bind(location)
        .bind("")
        .fetch_one(&st.db)
        .await?)
}

/// Copies one content to the target and records it: Ok(true) when copied, Ok(false) when there was nothing to do
/// (deleted meanwhile, or listed as failed)
async fn copy_one(
    cx: &Ctx<'_>,
    policy: &super::Policy,
    dst: &Arc<dyn Storage>,
    location: &str,
    hash: &str,
    size: i64,
    primary: &str,
) -> AppResult<Result<bool, Stop>> {
    let st = cx.st;
    // Held until recorded, like an upload's: a deletion of the same content there in progress finishes first
    let _staging = tree::stage_guard(st, hash).await;
    // A copy a stop leaves unrecorded doesn't stay (deletion skips it once it is recorded)
    {
        let _w = st.write_lock.lock().await;
        sqlx::query(
            "INSERT INTO pending_blob_deletes (hash, location_id, created_at, attempts, last_error) VALUES (?, ?, ?, 0, 'deferred')
             ON CONFLICT (hash, location_id) DO UPDATE SET created_at = MIN(created_at, excluded.created_at)",
        )
        .bind(hash)
        .bind(location)
        .bind(now() + UNRECORDED_GRACE)
        .execute(&st.db)
        .await?;
    }
    // From where it is kept, else from another checked copy
    let mut sources = vec![primary.to_string()];
    let others: Vec<(String,)> = sqlx::query_as("SELECT location_id FROM replica_copies WHERE hash = ? AND state = 'verified' AND location_id NOT IN (?, ?)")
        .bind(hash)
        .bind(primary)
        .bind(location)
        .fetch_all(&st.db)
        .await?;
    sources.extend(others.into_iter().map(|(l,)| l));
    let tmp = st.tmp_dir().join(format!("replica-{}", new_id()));
    let mut fetched = None;
    let mut last_err = String::new();
    for source in &sources {
        let Ok(src) = st.storage(source) else { continue };
        match cx
            .tries(
                |e: &std::io::Error| e.kind() != std::io::ErrorKind::NotFound && !(crate::hashing::unusable_kind(e) == Some(crate::hashing::Unusable::Damaged)),
                || crate::backups::capture::fetch_verified(&src, hash, size, &tmp),
            )
            .await
        {
            Ok(()) => {
                fetched = Some(source.clone());
                break;
            }
            Err(Ok(stop)) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Ok(Err(stop));
            }
            Err(Err(e)) => last_err = crate::locations::describe(&e),
        }
    }
    if fetched.is_none() {
        let _ = tokio::fs::remove_file(&tmp).await;
        let gone: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM blobs WHERE hash = ?").bind(hash).fetch_optional(&st.db).await?;
        if gone.is_some() {
            // No whole copy to take it from: said, never replaced by something else
            cx.failed("", None, format!("The content {} couldn't be read anywhere: {last_err}", &hash[..12]));
        }
        return Ok(Ok(false));
    }
    let put = put_verified(cx, dst, location, hash, size, &tmp).await;
    let _ = tokio::fs::remove_file(&tmp).await;
    if let Err(stop) = put? {
        return Ok(Err(stop));
    }
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        // Recorded only if still wanted: the content is still used and kept elsewhere, and no promotion came meanwhile
        let (epoch,): (i64,) = sqlx::query_as("SELECT epoch FROM replica_policies WHERE id = ?").bind(&policy.id).fetch_one(&mut *tx).await?;
        let current: Option<(String,)> = sqlx::query_as("SELECT location_id FROM blobs WHERE hash = ?").bind(hash).fetch_optional(&mut *tx).await?;
        match current {
            Some((loc,)) if loc == location => {
                // It is the primary there now: nothing to record, and nothing to delete
                sqlx::query("DELETE FROM pending_blob_deletes WHERE hash = ? AND location_id = ? AND last_error = 'deferred'")
                    .bind(hash)
                    .bind(location)
                    .execute(&mut *tx)
                    .await?;
                Ok(false)
            }
            Some(_) if epoch == policy.epoch => {
                sqlx::query("INSERT OR REPLACE INTO replica_copies (hash, location_id, size, state, created_at, verified_at) VALUES (?, ?, ?, 'verified', ?, ?)")
                    .bind(hash)
                    .bind(location)
                    .bind(size)
                    .bind(now())
                    .bind(now())
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("DELETE FROM pending_blob_deletes WHERE hash = ? AND location_id = ? AND last_error = 'deferred'")
                    .bind(hash)
                    .bind(location)
                    .execute(&mut *tx)
                    .await?;
                Ok(true)
            }
            // Deleted meanwhile, or promoted: the copy is left to deletion (checked again before)
            _ => {
                sqlx::query("UPDATE pending_blob_deletes SET created_at = ? WHERE hash = ? AND location_id = ?")
                    .bind(now() + REMOVAL_GRACE)
                    .bind(hash)
                    .bind(location)
                    .execute(&mut *tx)
                    .await?;
                Ok(false)
            }
        }
    }
    .await;
    Ok(Ok(crate::db::settle(tx, res).await?))
}

/// Lets go of the copies on a location that nothing replicated uses any more: content deleted for good, content
/// whose primary is there now, content no policy's spaces use. Their rows go first, then the copies are deleted like
/// any content nothing uses (checked again before). Copies of content still used stay, also when no policy asks for
/// them any more (a target removed, fewer copies wanted): only an administrator removes those (`purge`).
pub async fn release(st: &AppState, location: &str) -> AppResult<i64> {
    // Every policy's content-store spaces, whatever their targets
    let policies: Vec<(String,)> = sqlx::query_as("SELECT id FROM replica_policies").fetch_all(&st.db).await?;
    let mut spaces = Vec::new();
    for (p,) in policies {
        spaces.extend(super::store_scope(&mut *st.db.acquire().await?, &p).await?);
    }
    // Content whose primary is there now (few: from the location's own content), and content no policy's spaces use
    // (deleted for good included); content of a folder space's files as last read stays, whatever policy took it: its
    // folder may be the one that failed
    let unused: Vec<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT hash FROM (
           SELECT b.hash FROM blobs b WHERE b.location_id = ?2 AND EXISTS (SELECT 1 FROM replica_copies c WHERE c.hash = b.hash AND c.location_id = ?2)
           UNION SELECT c.hash FROM replica_copies c WHERE c.location_id = ?2 AND NOT {})
         WHERE hash NOT IN ({})",
        in_scope("c.hash"),
        super::folders::current_hashes(None)
    )))
    .bind(serde_json::to_string(&spaces).unwrap())
    .bind(location)
    .fetch_all(&st.db)
    .await?;
    let n = unused.len() as i64;
    drop_copies(st, location, unused.into_iter().map(|(h,)| h).collect()).await?;
    Ok(n)
}

/// Takes the rows of these copies away, and lists them for deletion (checked again before: content that is the
/// primary there, or that something else keeps, stays)
pub async fn drop_copies(st: &AppState, location: &str, hashes: Vec<String>) -> AppResult<()> {
    for chunk in hashes.chunks(500) {
        let list = serde_json::to_string(chunk).unwrap();
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            sqlx::query("DELETE FROM replica_copies WHERE location_id = ?1 AND hash IN (SELECT value FROM json_each(?2))")
                .bind(location)
                .bind(&list)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT INTO pending_blob_deletes (hash, location_id, created_at, attempts, last_error)
                 SELECT value, ?1, ?3, 0, 'deferred' FROM json_each(?2) WHERE true
                 ON CONFLICT (hash, location_id) DO UPDATE SET created_at = MIN(created_at, excluded.created_at)",
            )
            .bind(location)
            .bind(&list)
            .bind(now() + REMOVAL_GRACE)
            .execute(&mut *tx)
            .await?;
            AppResult::Ok(())
        }
        .await;
        crate::db::settle(tx, res).await?;
    }
    Ok(())
}

/// Reads every copy a target holds back and checks it; a missing or damaged one is marked, and the next sync replaces
/// it from a checked copy
pub async fn verify(cx: &Ctx<'_>) -> AppResult<Stop> {
    let st = cx.st;
    let Some((policy, target, _)) = context(cx).await? else { return Ok(Stop::Cancelled) };
    let location = target.location_id.clone();
    if let Err(e) = crate::locations::probe(st, &location).await {
        return Err(unreachable(st, &location, e).await);
    }
    let dst = st.storage(&location)?;
    let (files, bytes): (i64, i64) = sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(size), 0) FROM replica_copies WHERE location_id = ? AND state = 'verified'")
        .bind(&location)
        .fetch_one(&st.db)
        .await?;
    cx.set_counts(0, 0, files, bytes);
    cx.flush().await?;
    let mut damaged = 0i64;
    let mut last = String::new();
    loop {
        let rows: Vec<(String, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT hash, size FROM replica_copies WHERE location_id = ? AND state = 'verified' AND hash > ? ORDER BY hash LIMIT {PAGE}"
        )))
        .bind(&location)
        .bind(&last)
        .fetch_all(&st.db)
        .await?;
        let Some((h, _)) = rows.last() else { break };
        last = h.clone();
        for (hash, size) in rows {
            if let Some(stop) = cx.stop() {
                return Ok(stop);
            }
            let read = cx.tries(|e: &std::io::Error| e.kind() != std::io::ErrorKind::NotFound, || read_back(dst.as_ref(), &hash, size)).await;
            let whole = match read {
                Ok((h, n)) => h == hash && n == size as u64,
                Err(Ok(stop)) => return Ok(stop),
                Err(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(Err(e)) => return Err(unreachable(st, &location, crate::locations::describe(&e)).await),
            };
            let _w = st.write_lock.lock().await;
            if whole {
                sqlx::query("UPDATE replica_copies SET verified_at = ? WHERE hash = ? AND location_id = ?")
                    .bind(now())
                    .bind(&hash)
                    .bind(&location)
                    .execute(&st.db)
                    .await?;
            } else {
                damaged += 1;
                sqlx::query("UPDATE replica_copies SET state = 'corrupt' WHERE hash = ? AND location_id = ?").bind(&hash).bind(&location).execute(&st.db).await?;
                cx.failed("", None, format!("The copy of {} is missing or damaged", &hash[..12]));
            }
            drop(_w);
            cx.done(1, size).await?;
        }
    }
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        sqlx::query("UPDATE replica_targets SET last_verify_at = ? WHERE policy_id = ? AND location_id = ?")
            .bind(now())
            .bind(&policy.id)
            .bind(&location)
            .execute(&mut *tx)
            .await?;
        let note = (damaged > 0).then(|| {
            if damaged == 1 {
                "1 copy was missing or damaged; the next sync replaces it".to_string()
            } else {
                format!("{damaged} copies were missing or damaged; the next sync replaces them")
            }
        });
        crate::backups::runner::finish(&mut tx, cx, note.as_deref()).await?;
        AppResult::Ok(())
    }
    .await;
    crate::db::settle(tx, res).await?;
    drop(_w);
    if damaged > 0 {
        // Repaired soon
        super::policy::trigger(st, &policy.id, &location, "repair", None).await?;
    }
    Ok(Stop::Done)
}

/// How many copies of the policy's content each target holds, and how many it should: (location, held, wanted)
pub async fn coverage(st: &AppState, policy: &super::Policy, targets: &[Target]) -> AppResult<HashMap<String, (i64, i64)>> {
    let spaces = super::store_scope(&mut *st.db.acquire().await?, &policy.id).await?;
    let rows: Vec<(String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT b.hash, b.location_id FROM blobs b WHERE b.hash IN ({SCOPE_HASHES})")))
        .bind(serde_json::to_string(&spaces).unwrap())
        .fetch_all(&st.db)
        .await?;
    let held: std::collections::HashSet<(String, String)> = sqlx::query_as::<_, (String, String)>(
        "SELECT hash, location_id FROM replica_copies WHERE state = 'verified' AND location_id IN (SELECT location_id FROM replica_targets WHERE policy_id = ?)",
    )
    .bind(&policy.id)
    .fetch_all(&st.db)
    .await?
    .into_iter()
    .collect();
    let mut out: HashMap<String, (i64, i64)> = targets.iter().map(|t| (t.location_id.clone(), (0, 0))).collect();
    for (hash, primary) in rows {
        for l in super::required(targets, policy.copies, &primary) {
            let e = out.entry(l.to_string()).or_default();
            e.1 += 1;
            if held.contains(&(hash.clone(), l.to_string())) {
                e.0 += 1;
            }
        }
    }
    // Folder spaces: their content, and what isn't read yet
    for l in super::required(targets, policy.copies, &policy.source_location) {
        let (held, wanted) = super::folders::coverage(st, policy, l).await?;
        let e = out.entry(l.to_string()).or_default();
        (e.0, e.1) = (e.0 + held, e.1 + wanted);
    }
    Ok(out)
}
