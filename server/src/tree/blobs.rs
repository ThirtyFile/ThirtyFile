//! Content storage: blob reference counting, staging uploads, and deleting content that is no longer used.
//!
//! Physical files and thumbnails that are no longer referenced are deleted in the background (doesn't block the
//! caller or hold the global write lock while calling the storage service). Each file is re-checked under the write
//! lock before deletion: it's kept if it has been referenced again or someone is uploading the same content.

use std::collections::HashMap;

use sqlx::SqliteConnection;

use super::{Node, adjust_usage};
use crate::{
    error::AppResult,
    state::AppState,
    util::now,
};

/// Physical file (blob): represented as (hash, storage location)
pub type BlobRef = (String, String);

/// Adds a reference. If the blob doesn't exist yet, it's created in `location`.
pub async fn add_blob_ref(conn: &mut SqliteConnection, hash: &str, size: i64, location: &str) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO blobs (hash, size, refcount, created_at, location_id) VALUES (?, ?, 1, ?, ?)
         ON CONFLICT (hash) DO UPDATE SET refcount = refcount + 1",
    )
    .bind(hash)
    .bind(size)
    .bind(now())
    .bind(location)
    .execute(conn)
    .await?;
    Ok(())
}

/// Removes a reference; returns blobs that are no longer referenced and whose physical files must be deleted.
pub async fn release_blobs(conn: &mut SqliteConnection, hashes: &[String]) -> AppResult<Vec<BlobRef>> {
    if hashes.is_empty() {
        return Ok(Vec::new());
    }
    // One statement per step for the whole list (a hash listed n times loses n references)
    let list = serde_json::to_string(hashes).unwrap();
    sqlx::query(
        "UPDATE blobs SET refcount = refcount - d.n
         FROM (SELECT value AS hash, COUNT(*) AS n FROM json_each(?) GROUP BY value) d WHERE blobs.hash = d.hash",
    )
    .bind(&list)
    .execute(&mut *conn)
    .await?;
    let orphans: Vec<BlobRef> = sqlx::query_as(
        "DELETE FROM blobs WHERE hash IN (SELECT value FROM json_each(?)) AND refcount <= 0 RETURNING hash, location_id",
    )
    .bind(&list)
    .fetch_all(&mut *conn)
    .await?;
    Ok(orphans)
}

/// Gives a file of the content store new content, keeping its id (and with it its shares, permissions and favourites).
/// The caller has recorded the reference to the new content (`commit_blob`). The content it had becomes an earlier
/// version (see versions.rs); returns what is no longer used, to remove after the commit.
pub async fn set_content(
    conn: &mut SqliteConnection,
    policy: crate::versions::Policy,
    node: &Node,
    hash: &str,
    size: i64,
    by: i64,
) -> AppResult<crate::versions::Removed> {
    let author = crate::versions::content_author(conn, node).await?;
    sqlx::query("UPDATE nodes SET blob_hash = ?, size = ?, updated_at = ?, content_by = ? WHERE id = ?")
        .bind(hash)
        .bind(size)
        .bind(now().max(node.updated_at + 1))
        .bind(by)
        .bind(&node.id)
        .execute(&mut *conn)
        .await?;
    adjust_usage(conn, node.drive(), size - node.size).await?;
    // After the file lets go of its old content, which may be released when no version keeps it
    crate::versions::keep_stored(conn, policy, node, author).await
}

/// Adds a reference for each file of a copy, one statement for the whole list: (hash, size, location when new)
pub async fn add_blob_refs(conn: &mut SqliteConnection, blobs: &[(String, i64, String)]) -> AppResult<()> {
    if blobs.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO blobs (hash, size, refcount, created_at, location_id)
         SELECT json_extract(value, '$[0]'), MAX(json_extract(value, '$[1]')), COUNT(*), ?2, MIN(json_extract(value, '$[2]'))
         FROM json_each(?1) WHERE true GROUP BY json_extract(value, '$[0]')
         ON CONFLICT (hash) DO UPDATE SET refcount = refcount + excluded.refcount",
    )
    .bind(serde_json::to_string(blobs).unwrap())
    .bind(now())
    .execute(conn)
    .await?;
    Ok(())
}

/// Permanently deletes a subtree (including share links and earlier versions of its files), returning the physical
/// files to delete. Versions kept in a folder space's folder are removed from disk by its next scan.
pub async fn purge_subtree(conn: &mut SqliteConnection, id: &str) -> AppResult<Vec<BlobRef>> {
    // Only the columns needed: which content the files use, and how much space they free per space
    // (id, space, kind, size, content)
    type Row = (String, Option<String>, String, i64, Option<String>);
    let rows: Vec<Row> = sqlx::query_as(
        "WITH RECURSIVE sub(id) AS (
           SELECT ?1 UNION ALL SELECT c.id FROM nodes c JOIN sub ON c.parent_id = sub.id
         )
         SELECT n.id, n.drive_id, n.kind, n.size, n.blob_hash FROM sub JOIN nodes n ON n.id = sub.id",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let mut freed: HashMap<String, i64> = HashMap::new();
    let mut hashes = Vec::new();
    let mut files = Vec::new();
    for (id, drive, kind, size, hash) in rows {
        if kind != "folder" {
            *freed.entry(drive.unwrap_or_default()).or_default() += size;
            files.push(id);
        }
        hashes.extend(hash);
    }
    let mut orphans = crate::versions::purge_nodes(conn, &serde_json::to_string(&files).unwrap()).await?;
    for (drive, bytes) in freed {
        adjust_usage(conn, &drive, -bytes).await?;
    }
    sqlx::query(
        "WITH RECURSIVE sub(id) AS (
           SELECT ?1 UNION ALL SELECT c.id FROM nodes c JOIN sub ON c.parent_id = sub.id
         )
         DELETE FROM nodes WHERE id IN (SELECT id FROM sub)",
    )
    .bind(id)
    .execute(&mut *conn)
    .await?;
    orphans.extend(release_blobs(conn, &hashes).await?);
    Ok(orphans)
}

/// Nodes deleted per transaction when purging the content of deleted spaces
const DETACHED_BATCH: i64 = 2000;

/// Deletes, in the background, the content of spaces whose space row is gone: deleting a space or a user only
/// removes the space (the content can no longer be reached) and leaves the files to this, a batch per transaction,
/// so a space with hundreds of thousands of files doesn't hold the write lock for minutes. Also run at startup, for
/// content left when the server stopped halfway.
pub fn purge_detached_later(st: &AppState) {
    use std::sync::atomic::Ordering::SeqCst;
    // One purge at a time; a request while it runs makes it look again when it's done
    let (running, again) = &st.detached_purge;
    again.store(true, SeqCst);
    if running.swap(true, SeqCst) {
        return;
    }
    let st = st.clone();
    tokio::spawn(async move {
        let (running, again) = &st.detached_purge;
        while again.swap(false, SeqCst) {
            if let Err(e) = purge_detached(&st).await {
                tracing::warn!("Failed to delete the content of deleted spaces, will retry at the next start: {e:?}");
            }
        }
        running.store(false, SeqCst);
    });
}

async fn purge_detached(st: &AppState) -> AppResult<()> {
    let drives: Vec<(String,)> =
        sqlx::query_as("SELECT DISTINCT drive_id FROM nodes WHERE drive_id IS NOT NULL AND drive_id NOT IN (SELECT id FROM drives)")
            .fetch_all(&st.db)
            .await?;
    for (drive,) in drives {
        let mut total = 0;
        loop {
            let _w = st.write_lock.lock().await;
            let mut tx = st.db.begin().await?;
            // Leaves first (files, then folders once they're empty): a node's children must go before it
            let deleted: Vec<(String, Option<String>)> = sqlx::query_as(
                "DELETE FROM nodes WHERE id IN (
                   SELECT n.id FROM nodes n WHERE n.drive_id = ?1 AND NOT EXISTS (SELECT 1 FROM nodes c WHERE c.parent_id = n.id) LIMIT ?2
                 ) RETURNING id, blob_hash",
            )
            .bind(&drive)
            .bind(DETACHED_BATCH)
            .fetch_all(&mut *tx)
            .await?;
            if deleted.is_empty() {
                break;
            }
            total += deleted.len();
            let ids: Vec<&str> = deleted.iter().map(|(id, _)| id.as_str()).collect();
            let mut orphans = crate::versions::purge_nodes(&mut tx, &serde_json::to_string(&ids).unwrap()).await?;
            let hashes: Vec<String> = deleted.into_iter().filter_map(|(_, h)| h).collect();
            orphans.extend(release_blobs(&mut tx, &hashes).await?);
            tx.commit().await?;
            schedule_blob_removal(st, orphans);
        }
        tracing::info!("Deleted {total} files and folders of a deleted space");
    }
    Ok(())
}

/// Content no longer used is deleted after this long, so downloads and ZIPs already reading it can finish
pub const REMOVAL_GRACE: i64 = 60;

/// Queues content that is no longer used for deletion after `REMOVAL_GRACE`. The queue is in the database, so it
/// survives a restart; the storage health check works through it (only content still unused is deleted).
pub fn schedule_blob_removal(st: &AppState, blobs: Vec<BlobRef>) {
    if blobs.is_empty() {
        return;
    }
    let st = st.clone();
    tokio::spawn(async move { defer_blob_removal(&st, &blobs, REMOVAL_GRACE).await });
}

/// Deletes one by one: skipped when (hash, location) is still some blob's current location or is being staged
pub async fn remove_unreferenced(st: &AppState, blobs: Vec<BlobRef>) -> Vec<String> {
    let mut failures: Vec<String> = Vec::new();
    for (hash, location) in blobs {
        let Some(still_used) = claim_for_deletion(st, &hash, &location).await else { continue };
        // Released when this iteration ends, also when the task is cancelled while the storage service is being called
        let _deleting = BlobMark { st: st.clone(), hash: hash.clone(), deleting: true };
        // The write lock has been released: S3 may be slow, so don't make every write in the system wait for it
        let failed = match st.storage(&location) {
            Ok(storage) => storage.delete(&hash).await.err().map(|e| e.to_string()),
            Err(e) => Some(e.message),
        };
        if let Some(err) = &failed {
            failures.push(err.clone());
        }
        record_deletion(st, &hash, &location, failed.as_deref()).await;
        // Keep the thumbnail when the content is still used in another location (e.g. the old copy after a move)
        if !still_used {
            let _ = tokio::fs::remove_file(st.thumb_path(&hash)).await;
        }
    }
    if let Some(last) = failures.last() {
        tracing::warn!("Failed to delete {} physical file(s), will retry later: {last}", failures.len());
    }
    failures
}

/// Under the write lock, checks that (hash, location) may be deleted and counts it in as "being deleted" (the caller
/// releases that with a `BlobMark`). Returns whether the content is still used in another location, or None when it
/// must be left alone: it's in use there again, being staged, or the check failed.
async fn claim_for_deletion(st: &AppState, hash: &str, location: &str) -> Option<bool> {
    let _w = st.write_lock.lock().await;
    let current: Option<(String,)> = match sqlx::query_as("SELECT location_id FROM blobs WHERE hash = ?").bind(hash).fetch_optional(&st.db).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to check whether physical file {hash} is still referenced: {e}");
            return None;
        }
    };
    if current.as_ref().is_some_and(|(loc,)| *loc == location) {
        // The content is in use again (e.g. the same file was re-uploaded), so it no longer needs deleting
        let _ = forget_pending(st, hash, location).await;
        return None;
    }
    let mut g = st.blob_guard.lock().unwrap();
    if g.staging.contains_key(hash) {
        return None;
    }
    *g.deleting.entry(hash.to_string()).or_default() += 1;
    Some(current.is_some())
}

/// Updates the pending deletion list after a deletion attempt (`failed` is the error, if any)
async fn record_deletion(st: &AppState, hash: &str, location: &str, failed: Option<&str>) {
    let _w = st.write_lock.lock().await;
    let res = match failed {
        // Couldn't delete it (e.g. the storage service is disconnected): record it and retry once the location
        // is reachable again, to avoid leaving orphaned objects taking up space. Each failure doubles the wait
        // (created_at is the time of the next attempt), up to a day: a bucket that never allows deleting isn't
        // asked every 30 seconds.
        Some(err) => {
            tracing::debug!("Failed to delete physical file {hash} ({location}), will retry later: {err}");
            sqlx::query(
                "INSERT INTO pending_blob_deletes (hash, location_id, created_at, attempts, last_error) VALUES (?1, ?2, ?3 + ?5, 1, ?4)
                 ON CONFLICT (hash, location_id) DO UPDATE SET attempts = attempts + 1, last_error = excluded.last_error,
                   created_at = ?3 + MIN(?5 << MIN(attempts, 12), ?6)",
            )
            .bind(hash)
            .bind(location)
            .bind(crate::util::now())
            .bind(err.chars().take(300).collect::<String>())
            .bind(RETRY_BASE)
            .bind(RETRY_MAX)
            .execute(&st.db)
            .await
            .map(|_| ())
        }
        None => sqlx::query("DELETE FROM pending_blob_deletes WHERE hash = ? AND location_id = ?")
            .bind(hash)
            .bind(location)
            .execute(&st.db)
            .await
            .map(|_| ()),
    };
    if let Err(e) = res {
        tracing::warn!("Failed to update the pending deletion list: {e}");
    }
}

/// Wait before retrying a deletion that failed once; it doubles with every further failure, up to `RETRY_MAX`
const RETRY_BASE: i64 = 60;
const RETRY_MAX: i64 = 24 * 3600;


/// Keeps a hash marked as "being staged" or "being deleted" (the caller counted it in); dropping it (commit, failure,
/// or a cancelled request) releases the mark
pub struct BlobMark {
    st: AppState,
    hash: String,
    deleting: bool,
}

impl Drop for BlobMark {
    fn drop(&mut self) {
        let mut g = self.st.blob_guard.lock().unwrap();
        let marks = if self.deleting { &mut g.deleting } else { &mut g.staging };
        if let Some(n) = marks.get_mut(&self.hash) {
            *n -= 1;
            if *n == 0 {
                marks.remove(&self.hash);
            }
        }
    }
}

/// The caller already holds the write lock
async fn forget_pending(st: &AppState, hash: &str, location: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM pending_blob_deletes WHERE hash = ? AND location_id = ?").bind(hash).bind(location).execute(&st.db).await.map(|_| ())
}

/// Puts blobs on the pending deletion list to be deleted no earlier than `delay` seconds from now: for old copies after
/// a move, whose in-flight downloads should finish first. Durable, unlike the timer that normally deletes them.
pub async fn defer_blob_removal(st: &AppState, blobs: &[BlobRef], delay: i64) {
    let _w = st.write_lock.lock().await;
    for (hash, location) in blobs {
        let res = sqlx::query(
            "INSERT INTO pending_blob_deletes (hash, location_id, created_at, attempts, last_error) VALUES (?, ?, ?, 0, 'deferred')
             ON CONFLICT (hash, location_id) DO NOTHING",
        )
        .bind(hash)
        .bind(location)
        .bind(crate::util::now() + delay)
        .execute(&st.db)
        .await;
        if let Err(e) = res {
            tracing::warn!("Failed to record the deferred deletion of {hash}: {e}");
        }
    }
}

/// Retries physical files whose deletion failed earlier and whose next attempt is due (called by the health monitor
/// while a location is reachable); returns the number retried and the number that failed again
pub async fn retry_pending_deletes(st: &AppState, location: &str) -> (usize, usize) {
    // created_at is in the future for deferred deletions that must still wait
    let rows: Vec<(String,)> = sqlx::query_as("SELECT hash FROM pending_blob_deletes WHERE location_id = ? AND created_at <= ? ORDER BY created_at LIMIT 1000")
        .bind(location)
        .bind(crate::util::now())
        .fetch_all(&st.db)
        .await
        .unwrap_or_default();
    let n = rows.len();
    if n == 0 {
        return (0, 0);
    }
    let failed = remove_unreferenced(st, rows.into_iter().map(|(h,)| (h, location.to_string())).collect()).await.len();
    (n, failed)
}

/// Which storage location a space's new files go to
pub async fn drive_location(st: &AppState, conn: &mut SqliteConnection, drive_id: &str) -> AppResult<String> {
    let row: Option<(Option<String>,)> = sqlx::query_as("SELECT location_id FROM drives WHERE id = ?").bind(drive_id).fetch_optional(conn).await?;
    Ok(row.and_then(|r| r.0).unwrap_or_else(|| st.default_location.read().unwrap().clone()))
}

/// A temp file about to be stored: uploaded to the storage location *before* taking the write lock (S3 may take a while)
pub struct StagedBlob {
    pub hash: String,
    pub size: i64,
    tmp: std::path::PathBuf,
    /// The location it was uploaded to (None when the content already existed)
    uploaded_to: Option<String>,
    /// Where the content already existed when staging started (its file is protected from background deletion while staged)
    existing_at: Option<String>,
    guard: BlobMark,
}

/// Marks content as "being staged" until the guard is dropped: background deletion leaves it alone meanwhile. If the
/// same content is being deleted right now, waits for that to finish first, so content stored afterwards isn't deleted.
pub async fn stage_guard(st: &AppState, hash: &str) -> BlobMark {
    loop {
        {
            let mut g = st.blob_guard.lock().unwrap();
            if !g.deleting.contains_key(hash) {
                *g.staging.entry(hash.to_string()).or_default() += 1;
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    BlobMark { st: st.clone(), hash: hash.to_string(), deleting: false }
}

pub async fn stage_blob(st: &AppState, drive_id: &str, hash: String, size: i64, tmp: std::path::PathBuf) -> AppResult<StagedBlob> {
    let guard = stage_guard(st, &hash).await;
    let result = async {
        let mut c = st.db.acquire().await?;
        let exists: Option<(String,)> = sqlx::query_as("SELECT location_id FROM blobs WHERE hash = ?").bind(&hash).fetch_optional(&mut *c).await?;
        if let Some((loc,)) = exists {
            return Ok((None, Some(loc)));
        }
        let location = drive_location(st, &mut c, drive_id).await?;
        drop(c);
        // If the server stops between storing and recording it, the content would stay in storage unreferenced: list
        // it for deletion a day from now; recording it removes the entry, and deletion skips content still in use
        defer_blob_removal(st, &[(hash.clone(), location.clone())], STAGED_GRACE).await;
        st.storage(&location)?.put_file(&hash, &tmp).await?;
        Ok::<_, crate::error::AppError>((Some(location), None))
    }
    .await;
    let (uploaded_to, existing_at) = result?;
    Ok(StagedBlob { hash, size, tmp, uploaded_to, existing_at, guard })
}

/// How long content stored for an upload may stay unrecorded before background deletion removes it
const STAGED_GRACE: i64 = 24 * 3600;

/// Records the reference within the transaction (holding the write lock); returns redundant copies to delete after commit
pub async fn commit_blob(st: &AppState, conn: &mut SqliteConnection, staged: &StagedBlob) -> AppResult<Option<BlobRef>> {
    if let Some(loc) = &staged.uploaded_to {
        sqlx::query("DELETE FROM pending_blob_deletes WHERE hash = ? AND location_id = ? AND last_error = 'deferred'")
            .bind(&staged.hash)
            .bind(loc)
            .execute(&mut *conn)
            .await?;
    }
    let current: Option<(String,)> = sqlx::query_as("SELECT location_id FROM blobs WHERE hash = ?").bind(&staged.hash).fetch_optional(&mut *conn).await?;
    match (&current, &staged.uploaded_to) {
        // The content already exists: keep its original location; if a copy was also uploaded elsewhere, that one is redundant
        (Some((loc,)), uploaded) => {
            add_blob_ref(conn, &staged.hash, staged.size, loc).await?;
            Ok(uploaded.as_ref().filter(|u| *u != loc).map(|u| (staged.hash.clone(), u.clone())))
        }
        (None, Some(loc)) => {
            add_blob_ref(conn, &staged.hash, staged.size, loc).await?;
            Ok(None)
        }
        // The content's last reference was released while we were staging: the file itself is still there, because
        // background deletion skips hashes that are being staged, so just register it again (no storage I/O under the lock)
        (None, None) => {
            let loc = staged.existing_at.clone().unwrap_or_else(|| st.default_location.read().unwrap().clone());
            add_blob_ref(conn, &staged.hash, staged.size, &loc).await?;
            Ok(None)
        }
    }
}

/// After commit: delete the temp file and leave redundant copies to background deletion
pub async fn finish_staged(st: &AppState, staged: StagedBlob, extra: Option<BlobRef>) {
    let _ = tokio::fs::remove_file(&staged.tmp).await;
    drop(staged.guard);
    schedule_blob_removal(st, extra.into_iter().collect());
}

/// When the transaction fails (e.g. over quota, version conflict): discard the temp file; content just uploaded but not recorded is left to background deletion (re-checked before deleting)
pub async fn abandon_staged(st: &AppState, staged: StagedBlob) {
    let _ = tokio::fs::remove_file(&staged.tmp).await;
    drop(staged.guard);
    // Content we uploaded, or content that was kept only for us: removed unless something references it (checked before deleting)
    if let Some(loc) = staged.uploaded_to.or(staged.existing_at) {
        schedule_blob_removal(st, vec![(staged.hash, loc)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    /// A storage location whose deletes can be made to fail (simulating an S3 disconnect)
    struct Flaky {
        inner: crate::storage::LocalStorage,
        down: std::sync::atomic::AtomicBool,
    }

    impl crate::storage::Storage for Flaky {
        fn put_file<'a>(&'a self, hash: &'a str, src: &'a std::path::Path) -> futures_util::future::BoxFuture<'a, std::io::Result<()>> {
            self.inner.put_file(hash, src)
        }
        fn open<'a>(&'a self, hash: &'a str, start: u64, len: u64) -> futures_util::future::BoxFuture<'a, std::io::Result<crate::storage::BoxReader>> {
            self.inner.open(hash, start, len)
        }
        fn delete<'a>(&'a self, hash: &'a str) -> futures_util::future::BoxFuture<'a, std::io::Result<()>> {
            if self.down.load(std::sync::atomic::Ordering::SeqCst) {
                return Box::pin(async { Err(std::io::Error::other("connection refused")) });
            }
            self.inner.delete(hash)
        }
        fn check(&self) -> futures_util::future::BoxFuture<'_, std::io::Result<()>> {
            self.inner.check()
        }
    }

    #[tokio::test]
    async fn failed_deletes_are_retried_after_the_location_recovers() {
        let env = testutil::env().await;
        let flaky = std::sync::Arc::new(Flaky {
            inner: crate::storage::LocalStorage::new(env.dir.join("flaky")).unwrap(),
            down: std::sync::atomic::AtomicBool::new(true),
        });
        env.st.storages.write().unwrap().insert("flaky".into(), flaky.clone());
        let hash = "cd".repeat(32);
        let blob = env.dir.join("flaky").join("cd").join("cd").join(&hash);
        let tmp = env.dir.join("flaky-src");
        std::fs::write(&tmp, b"orphan").unwrap();
        crate::storage::Storage::put_file(flaky.as_ref(), &hash, &tmp).await.unwrap();
        let pending = || async {
            let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pending_blob_deletes").fetch_one(&env.st.db).await.unwrap();
            n
        };

        // Can't delete while disconnected: it's recorded, not forgotten
        remove_unreferenced(&env.st, vec![(hash.clone(), "flaky".into())]).await;
        assert!(blob.exists());
        assert_eq!(pending().await, 1);
        // Not retried before its next attempt is due
        assert_eq!(retry_pending_deletes(&env.st, "flaky").await, (0, 0));
        let due = || async {
            sqlx::query("UPDATE pending_blob_deletes SET created_at = 0").execute(&env.st.db).await.unwrap();
        };
        due().await;
        assert_eq!(retry_pending_deletes(&env.st, "flaky").await, (1, 1), "still disconnected: retry fails and keeps waiting");
        assert_eq!(pending().await, 1);
        // Each failure waits twice as long
        let (attempts, wait): (i64, i64) = sqlx::query_as("SELECT attempts, created_at - ? FROM pending_blob_deletes")
            .bind(crate::util::now())
            .fetch_one(&env.st.db)
            .await
            .unwrap();
        assert_eq!(attempts, 2);
        assert!((RETRY_BASE * 2 - 5..=RETRY_BASE * 2).contains(&wait), "{wait}");

        // Retry after recovery: actually deleted and the list is cleared
        flaky.down.store(false, std::sync::atomic::Ordering::SeqCst);
        due().await;
        retry_pending_deletes(&env.st, "flaky").await;
        assert!(!blob.exists());
        assert_eq!(pending().await, 0);
    }

    #[tokio::test]
    async fn content_no_longer_used_waits_before_it_is_deleted() {
        let env = testutil::env().await;
        let hash = "ef".repeat(32);
        let blob = env.dir.join("blobs").join("ef").join("ef").join(&hash);
        let tmp = env.dir.join("tmp").join("grace");
        std::fs::write(&tmp, b"read by a download").unwrap();
        crate::storage::Storage::put_file(env.st.storage("local").unwrap().as_ref(), &hash, &tmp).await.unwrap();
        schedule_blob_removal(&env.st, vec![(hash.clone(), "local".into())]);
        // Queued, not deleted: downloads already reading it can finish
        for _ in 0..50 {
            let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pending_blob_deletes WHERE hash = ?").bind(&hash).fetch_one(&env.st.db).await.unwrap();
            if n == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(retry_pending_deletes(&env.st, "local").await, (0, 0), "not due yet");
        assert!(blob.exists());
        // After the grace period, the health check's retry deletes it
        sqlx::query("UPDATE pending_blob_deletes SET created_at = created_at - ?").bind(REMOVAL_GRACE).execute(&env.st.db).await.unwrap();
        assert_eq!(retry_pending_deletes(&env.st, "local").await, (1, 0));
        assert!(!blob.exists());
    }

    #[tokio::test]
    async fn background_removal_never_deletes_blobs_being_uploaded() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let drive = env.drive_of(&amy.root_id).await;
        let hash = "ab".repeat(32);
        let blob = env.dir.join("blobs").join("ab").join("ab").join(&hash);
        let tmp = env.dir.join("tmp").join("upload");
        std::fs::write(&tmp, b"hello").unwrap();

        // Uploading (stored in the storage location, reference not yet recorded): background deletion must skip it
        let staged = stage_blob(&env.st, &drive, hash.clone(), 5, tmp.clone()).await.unwrap();
        assert!(blob.exists());
        remove_unreferenced(&env.st, vec![(hash.clone(), "local".into())]).await;
        assert!(blob.exists(), "content being uploaded was deleted by mistake");

        // After the reference is recorded: still referenced, so not deleted
        {
            let _w = env.st.write_lock.lock().await;
            let mut c = env.st.db.acquire().await.unwrap();
            let extra = commit_blob(&env.st, &mut c, &staged).await.unwrap();
            drop(c);
            finish_staged(&env.st, staged, extra).await;
        }
        remove_unreferenced(&env.st, vec![(hash.clone(), "local".into())]).await;
        assert!(blob.exists());

        // Only actually deleted once nothing references it
        {
            let mut c = env.st.db.acquire().await.unwrap();
            let orphans = release_blobs(&mut c, std::slice::from_ref(&hash)).await.unwrap();
            assert_eq!(orphans.len(), 1);
        }
        remove_unreferenced(&env.st, vec![(hash.clone(), "local".into())]).await;
        assert!(!blob.exists());
    }

}
