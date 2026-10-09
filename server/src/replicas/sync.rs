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

use sqlx::Row;

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
                // Written in full first, then put in the content's place (`repair_file`): what is there stays until
                // then. Each attempt writes a link to the temp file, so one that doesn't read back whole is tried again
                // with the content still at hand.
                let attempt = crate::storage::attempt_of(tmp).await?;
                let repaired = dst.repair_file(hash, &attempt).await;
                if repaired.is_err() {
                    let _ = tokio::fs::remove_file(&attempt).await;
                }
                repaired?;
                match read_back(dst.as_ref(), hash, size).await? {
                    (h, n) if h == hash && n == size as u64 => Ok(()),
                    _ => Err(crate::hashing::unusable(crate::hashing::Unusable::Damaged, crate::backups::capture::DAMAGED)),
                }
            },
        )
        .await;
    match put {
        Ok(()) => {
            let _ = tokio::fs::remove_file(tmp).await;
            Ok(Ok(()))
        }
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
    let seqs_before = seqs.clone();
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
    let repaired = match recheck_copies(cx, &location, &dst).await? {
        Ok(n) => n,
        Err(stop) => return Ok(stop),
    };
    // Checked: an old primary counts again from here
    for t in targets.iter_mut().filter(|t| t.location_id == location) {
        t.state = "active".into();
    }
    // 2. What the target should hold and doesn't
    let mut copied = match copy_missing(cx, &policy, &targets, &spaces, &location, &dst).await? {
        Ok(n) => n,
        Err(stop) => return Ok(stop),
    };
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
    // What the Replicas page shows of the target, and of the copies there no policy wants: each reads every content of
    // the spaces, so it is worked out again only when the sync may have changed it, and kept
    let stamp = super::stamp(&mut *st.db.acquire().await?).await?;
    let (unneeded_known,): (bool,) =
        sqlx::query_as("SELECT EXISTS (SELECT 1 FROM replica_unneeded WHERE location_id = ?)").bind(&location).fetch_one(&st.db).await?;
    let recount =
        !unchanged || seqs != seqs_before || copied + repaired + released + changing.kept + changing.not_copied > 0 || target.held.is_none() || !unneeded_known;
    let counts = if recount { Some((count(st, &policy, &targets).await?, super::api::unneeded_count(st, &location).await?)) } else { None };
    // 4. Held: the target is current, and counts again after a promotion
    let synced = Synced { copied, repaired, released, changing, counts, stamp, seqs, all };
    finish_sync(cx, &policy, &location, &synced).await?;
    Ok(Stop::Done)
}

/// Step 1 of a sync: the copies on the target that must be checked before they count (an old primary's, and those a
/// check found damaged) are read back; a missing or damaged one goes, to be copied again. Returns how many went.
async fn recheck_copies(cx: &Ctx<'_>, location: &str, dst: &Arc<dyn Storage>) -> AppResult<Result<i64, Stop>> {
    let st = cx.st;
    let mut repaired = 0i64;
    loop {
        let rows: Vec<(String, i64, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT hash, size, state FROM replica_copies WHERE location_id = ? AND state IN ('stale', 'corrupt') ORDER BY hash LIMIT {PAGE}"
        )))
        .bind(location)
        .fetch_all(&st.db)
        .await?;
        if rows.is_empty() {
            break;
        }
        for (hash, size, state) in rows {
            if let Some(stop) = cx.stop() {
                return Ok(Err(stop));
            }
            let whole = state == "stale" && matches!(read_back(dst.as_ref(), &hash, size).await, Ok((h, n)) if h == hash && n == size as u64);
            if whole {
                let _w = st.write_lock.lock().await;
                sqlx::query("UPDATE replica_copies SET state = 'verified', verified_at = ? WHERE hash = ? AND location_id = ?")
                    .bind(now())
                    .bind(&hash)
                    .bind(location)
                    .execute(&st.db)
                    .await?;
            } else {
                // Missing or damaged: it goes (deleted like content nothing uses, never the primary), and is copied
                // again below if the target should hold it
                drop_copies(st, location, vec![hash.clone()]).await?;
                repaired += 1;
            }
            cx.done(1, size).await?;
        }
    }
    Ok(Ok(repaired))
}

/// Step 2 of a sync: the content of the content-store spaces the target should hold and doesn't is copied there.
/// Returns how many were copied.
async fn copy_missing(
    cx: &Ctx<'_>,
    policy: &super::Policy,
    targets: &[Target],
    spaces: &[String],
    location: &str,
    dst: &Arc<dyn Storage>,
) -> AppResult<Result<i64, Stop>> {
    let st = cx.st;
    let mut copied = 0i64;
    let mut last = String::new();
    loop {
        let rows: Vec<(String, i64, String)> =
            sqlx::query_as(sqlx::AssertSqlSafe(missing_page())).bind(serde_json::to_string(spaces).unwrap()).bind(location).bind(&last).fetch_all(&st.db).await?;
        let Some((h, ..)) = rows.last() else { break };
        last = h.clone();
        for (hash, size, primary) in rows {
            if let Some(stop) = cx.stop() {
                return Ok(Err(stop));
            }
            // Only where the target is one of the copies the content should have
            if !super::required(targets, policy.copies, &primary).contains(&location) {
                continue;
            }
            match copy_one(cx, policy, dst, location, &hash, size, &primary).await? {
                Ok(true) => copied += 1,
                Ok(false) => {}
                Err(stop) => return Ok(Err(stop)),
            }
            cx.done(1, size).await?;
        }
    }
    Ok(Ok(copied))
}

/// What the Replicas page shows: (contents held and wanted, by target), and (copies, bytes) no policy wants on a location
type PageCounts = (HashMap<String, (i64, i64)>, (i64, i64));

/// What a sync did, recorded when it ends (`finish_sync`)
struct Synced {
    copied: i64,
    /// Missing or damaged copies that were replaced
    repaired: i64,
    /// Copies nothing uses any more that were let go
    released: i64,
    changing: super::folders::Changing,
    /// What the Replicas page shows of the policy's targets and of the copies on the target no policy wants, when it
    /// was worked out again, as of the policies' `stamp`
    counts: Option<PageCounts>,
    stamp: String,
    /// What each space held when the sync began (a folder space: what its check for changes found)
    seqs: Vec<(String, i64)>,
    /// The spaces of the policy now
    all: Vec<String>,
}

/// Step 4 of a sync: the target holds the spaces as they were, is current, and counts again after a promotion
async fn finish_sync(cx: &Ctx<'_>, policy: &super::Policy, location: &str, s: &Synced) -> AppResult<()> {
    let st = cx.st;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        let (epoch,): (i64,) = sqlx::query_as("SELECT epoch FROM replica_policies WHERE id = ?").bind(&policy.id).fetch_one(&mut *tx).await?;
        if epoch != policy.epoch {
            return Err(AppError::conflict("The replicas were promoted meanwhile"));
        }
        // Not when the policies changed meanwhile: then they are worked out again soon (policy.rs)
        if let Some((counts, (copies, bytes))) = &s.counts
            && super::stamp(&mut tx).await? == s.stamp
        {
            keep_counts(&mut tx, &policy.id, counts).await?;
            keep_unneeded(&mut tx, location, *copies, *bytes).await?;
        }
        sqlx::query(
            "INSERT INTO replica_captured (policy_id, location_id, drive_id, seq) SELECT ?1, ?2, json_extract(value, '$[0]'), json_extract(value, '$[1]') FROM json_each(?3) WHERE true
             ON CONFLICT (policy_id, location_id, drive_id) DO UPDATE SET seq = excluded.seq",
        )
        .bind(&policy.id)
        .bind(location)
        .bind(serde_json::to_string(&s.seqs).unwrap())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM replica_dirty WHERE policy_id = ?1 AND location_id = ?2 AND drive_id IN (
               SELECT k.drive_id FROM replica_captured k LEFT JOIN space_changes c ON c.drive_id = k.drive_id
               WHERE k.policy_id = ?1 AND k.location_id = ?2 AND COALESCE(c.seq, 0) <= k.seq)",
        )
        .bind(&policy.id)
        .bind(location)
        .execute(&mut *tx)
        .await?;
        // Spaces deleted, or taken out of the policy, since: never synced again, so what was recorded of them goes
        for table in ["replica_dirty", "replica_captured"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {table} WHERE policy_id = ?1 AND location_id = ?2 AND drive_id NOT IN (SELECT value FROM json_each(?3))"
            )))
            .bind(&policy.id)
            .bind(location)
            .bind(serde_json::to_string(&s.all).unwrap())
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("DELETE FROM replica_folder_files WHERE drive_id NOT IN (SELECT id FROM drives)").execute(&mut *tx).await?;
        sqlx::query("UPDATE replica_targets SET synced_at = ?, state = 'active' WHERE policy_id = ? AND location_id = ?")
            .bind(now())
            .bind(&policy.id)
            .bind(location)
            .execute(&mut *tx)
            .await?;
        let note = s.note();
        crate::backups::runner::finish(&mut tx, cx, note.as_deref()).await?;
        AppResult::Ok(())
    }
    .await;
    crate::db::settle(tx, res).await
}

impl Synced {
    /// What the sync's job tells: what was copied, replaced and let go, and the files that changed while copied
    fn note(&self) -> Option<String> {
        let (copied, repaired, released) = (self.copied, self.repaired, self.released);
        let mut notes = Vec::new();
        if copied > 0 {
            notes.push(if copied == 1 { "1 content was copied".to_string() } else { format!("{copied} contents were copied") });
        }
        if repaired > 0 {
            notes.push(if repaired == 1 {
                "1 damaged or missing copy was replaced".to_string()
            } else {
                format!("{repaired} damaged or missing copies were replaced")
            });
        }
        if released > 0 {
            notes.push(if released == 1 {
                "1 copy nothing uses any more was let go".to_string()
            } else {
                format!("{released} copies nothing uses any more were let go")
            });
        }
        notes.extend(self.changing.notes());
        (!notes.is_empty()).then(|| notes.join("\n"))
    }
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
    let tmp = st.tmp_dir().join(format!("replica-{}", new_id()));
    match fetch_from_any(cx, location, hash, size, primary, &tmp).await? {
        Fetched::Done => {}
        Fetched::Stop(stop) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Ok(Err(stop));
        }
        Fetched::Nowhere(last_err) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            let gone: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM blobs WHERE hash = ?").bind(hash).fetch_optional(&st.db).await?;
            if gone.is_some() {
                // No whole copy to take it from: said, never replaced by something else
                cx.failed("", None, format!("The content {} couldn't be read anywhere: {last_err}", &hash[..12]));
            }
            return Ok(Ok(false));
        }
    }
    let put = put_verified(cx, dst, location, hash, size, &tmp).await;
    let _ = tokio::fs::remove_file(&tmp).await;
    if let Err(stop) = put? {
        return Ok(Err(stop));
    }
    Ok(Ok(record_copy(st, policy, location, hash, size).await?))
}

/// What reading a content to copy came to
enum Fetched {
    /// In the temp file, checked
    Done,
    Stop(Stop),
    /// It couldn't be read anywhere: the last error
    Nowhere(String),
}

/// Reads a content into `tmp`, checked: from where it is kept (`primary`), else from another checked copy (not on the
/// target `location`)
async fn fetch_from_any(cx: &Ctx<'_>, location: &str, hash: &str, size: i64, primary: &str, tmp: &std::path::Path) -> AppResult<Fetched> {
    let st = cx.st;
    let mut sources = vec![primary.to_string()];
    let others: Vec<(String,)> = sqlx::query_as("SELECT location_id FROM replica_copies WHERE hash = ? AND state = 'verified' AND location_id NOT IN (?, ?)")
        .bind(hash)
        .bind(primary)
        .bind(location)
        .fetch_all(&st.db)
        .await?;
    sources.extend(others.into_iter().map(|(l,)| l));
    let mut last_err = String::new();
    for source in &sources {
        let Ok(src) = st.storage(source) else { continue };
        match cx
            .tries(
                |e: &std::io::Error| e.kind() != std::io::ErrorKind::NotFound && !(crate::hashing::unusable_kind(e) == Some(crate::hashing::Unusable::Damaged)),
                || crate::backups::capture::fetch_verified(&src, hash, size, tmp),
            )
            .await
        {
            Ok(()) => return Ok(Fetched::Done),
            Err(Ok(stop)) => return Ok(Fetched::Stop(stop)),
            Err(Err(e)) => last_err = crate::locations::describe(&e),
        }
    }
    Ok(Fetched::Nowhere(last_err))
}

/// Records a copy made on the target `location`: only if still wanted (the content is still used and kept elsewhere,
/// and no promotion came meanwhile). Returns whether it was recorded.
async fn record_copy(st: &AppState, policy: &super::Policy, location: &str, hash: &str, size: i64) -> AppResult<bool> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
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
    crate::db::settle(tx, res).await
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

/// How many contents of the policy each of its targets holds of those it should hold: (held, wanted) by location, each
/// content counted once. It reads every content of the spaces, so it is worked out when it may have changed and kept with
/// the targets (`replica_targets.held`, `wanted`), never each time the Replicas page is looked at.
pub async fn count(st: &AppState, policy: &super::Policy, targets: &[Target]) -> AppResult<HashMap<String, (i64, i64)>> {
    let spaces = super::store_scope(&mut *st.db.acquire().await?, &policy.id).await?;
    // Content-store spaces: their content by where it is kept, and how much of it has a checked copy on each target
    let held: String = (0..targets.len())
        .map(|i| format!(", SUM(EXISTS (SELECT 1 FROM replica_copies c WHERE c.hash = b.hash AND c.location_id = ?{} AND c.state = 'verified'))", i + 2))
        .collect();
    let sql = format!("SELECT b.location_id, COUNT(*){held} FROM blobs b WHERE b.hash IN ({SCOPE_HASHES}) GROUP BY b.location_id");
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(serde_json::to_string(&spaces).unwrap());
    for t in targets {
        q = q.bind(&t.location_id);
    }
    let rows = q.fetch_all(&st.db).await?;
    let mut out: HashMap<String, (i64, i64)> = targets.iter().map(|t| (t.location_id.clone(), (0, 0))).collect();
    for row in rows {
        let (primary, n): (String, i64) = (row.try_get(0)?, row.try_get(1)?);
        let required = super::required(targets, policy.copies, &primary);
        for (i, t) in targets.iter().enumerate() {
            if required.contains(&t.location_id.as_str()) {
                let e = out.entry(t.location_id.clone()).or_default();
                (e.0, e.1) = (e.0 + row.try_get::<i64, _>(i + 2)?, e.1 + n);
            }
        }
    }
    // Folder spaces: their content as last read, and what isn't read yet
    for l in super::required(targets, policy.copies, &policy.source_location) {
        let (held, wanted) = super::folders::coverage(st, policy, l).await?;
        let e = out.entry(l.to_string()).or_default();
        (e.0, e.1) = (e.0 + held, e.1 + wanted);
    }
    Ok(out)
}

/// Keeps the counts of a policy's targets (`count`) with them
pub(super) async fn keep_counts(conn: &mut sqlx::SqliteConnection, policy: &str, counts: &HashMap<String, (i64, i64)>) -> AppResult<()> {
    for (location, (held, wanted)) in counts {
        sqlx::query("UPDATE replica_targets SET held = ?, wanted = ? WHERE policy_id = ? AND location_id = ?")
            .bind(held)
            .bind(wanted)
            .bind(policy)
            .bind(location)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// Keeps how many copies on a location no policy wants (`api::unneeded_on`), and their bytes
pub(super) async fn keep_unneeded(conn: &mut sqlx::SqliteConnection, location: &str, copies: i64, bytes: i64) -> AppResult<()> {
    sqlx::query("INSERT OR REPLACE INTO replica_unneeded (location_id, copies, bytes) VALUES (?, ?, ?)")
        .bind(location)
        .bind(copies)
        .bind(bytes)
        .execute(&mut *conn)
        .await?;
    Ok(())
}
