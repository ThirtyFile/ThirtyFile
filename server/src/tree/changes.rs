//! Changes to an item and everything in it, made a batch at a time: deleting for good, moving to the trash, restoring,
//! and renaming or moving in a folder space (whose items keep their paths). Every change takes the write lock, and one
//! transaction for a folder of a million items would hold it for minutes, during which nothing else can be written; a
//! deletion that ran into the request's time limit rolled back and could never finish.
//!
//! So the change itself (in its transaction) makes the first batch, and when more is left it records a row in
//! `tree_changes` in the same transaction. The rest follows a batch per transaction, in the background (holding the
//! scan locks of the folder spaces the change took, so no scan or other change to them comes in between), or in a job
//! the page follows (deleting for good). The row goes with the last batch; rows left by a stop are finished at the
//! next start, before folder spaces are scanned (`resume`).
//!
//! Meanwhile: an item being deleted for good isn't listed in the trash; an item whose trash, restore or path change
//! isn't finished can't be moved, copied, restored or deleted (`refuse_busy`).

use std::collections::{HashMap, VecDeque};

use sqlx::SqliteConnection;
use tokio::sync::OwnedMutexGuard;

use super::{BlobRef, adjust_usage, release_blobs, schedule_blob_removal};
use crate::{
    error::{AppError, AppResult},
    jobs::Tracker,
    state::AppState,
    util::{new_id, now},
};

/// Items changed per transaction: a few tens of milliseconds of the write lock
const BATCH: i64 = 2000;

#[cfg(test)]
thread_local! {
    /// Tests: a smaller batch, to make a few items take several
    static TEST_BATCH: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
}

fn batch() -> i64 {
    #[cfg(test)]
    if let Some(b) = TEST_BATCH.with(|b| b.get()) {
        return b;
    }
    BATCH
}

/// Tests: changes `batch` items per transaction until the guard is dropped
#[cfg(test)]
pub fn small_batches(n: i64) -> impl Drop {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_BATCH.with(|b| b.set(None));
        }
    }
    TEST_BATCH.with(|b| b.set(Some(n)));
    Reset
}

/// Hides items from the trash listing (and from Delete forever, Restore and Empty trash) while they are deleted
pub const NOT_PURGING: &str = "NOT EXISTS (SELECT 1 FROM tree_changes c WHERE c.node_id = n.id AND c.kind = 'purge')";

#[derive(Debug, Clone, sqlx::FromRow)]
struct Change {
    id: String,
    kind: String,
    node_id: String,
    drive_id: Option<String>,
    trash_id: Option<String>,
    trashed_at: Option<i64>,
    old_path: Option<String>,
    new_path: Option<String>,
}

/// A change left to finish after the transaction that started it, with where it was (the folders still to go through,
/// for moving to the trash)
#[derive(Debug)]
pub struct Unfinished {
    id: String,
    frontier: Option<VecDeque<String>>,
}

/// What batches leave to do after their commit: content no longer used, and trash folders and versions' files of
/// folder spaces to remove
#[derive(Default)]
struct Leftovers {
    blobs: Vec<BlobRef>,
    on_disk: Vec<crate::folders::Below>,
}

async fn record(conn: &mut SqliteConnection, c: &Change) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO tree_changes (id, kind, node_id, drive_id, trash_id, trashed_at, old_path, new_path, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&c.id)
    .bind(&c.kind)
    .bind(&c.node_id)
    .bind(&c.drive_id)
    .bind(&c.trash_id)
    .bind(c.trashed_at)
    .bind(&c.old_path)
    .bind(&c.new_path)
    .bind(now())
    .execute(conn)
    .await?;
    Ok(())
}

fn change(kind: &str, node_id: &str) -> Change {
    Change { id: new_id(), kind: kind.into(), node_id: node_id.into(), drive_id: None, trash_id: None, trashed_at: None, old_path: None, new_path: None }
}

/// Refuses a change to an item whose own change, or one of an item it is in, isn't finished yet
pub async fn refuse_busy(conn: &mut SqliteConnection, node_id: &str) -> AppResult<()> {
    let (busy,): (bool,) = sqlx::query_as(
        "WITH RECURSIVE up(id) AS (SELECT ?1 UNION ALL SELECT n.parent_id FROM nodes n JOIN up ON n.id = up.id WHERE n.parent_id IS NOT NULL)
         SELECT EXISTS (SELECT 1 FROM tree_changes WHERE node_id IN (SELECT id FROM up))",
    )
    .bind(node_id)
    .fetch_one(conn)
    .await?;
    if busy {
        return Err(AppError::conflict("This item is still being changed. Try again in a moment."));
    }
    Ok(())
}

// ───────────── Starting a change, in its transaction ─────────────

/// Items of a folder space at `old` (a path below the space's folder) and below it are at `new` now. Returns what is
/// left to do after this transaction, if anything.
pub async fn repath(conn: &mut SqliteConnection, node_id: &str, drive_id: &str, old: &str, new: &str) -> AppResult<Option<Unfinished>> {
    if old == new {
        return Ok(None);
    }
    let (_, done) = repath_batch(conn, drive_id, old, new, batch()).await?;
    if done {
        return Ok(None);
    }
    let c = Change { drive_id: Some(drive_id.into()), old_path: Some(old.into()), new_path: Some(new.into()), ..change("repath", node_id) };
    record(conn, &c).await?;
    Ok(Some(Unfinished { id: c.id, frontier: None }))
}

/// Marks everything in `node_id` (not in the trash already) as in the trash with it; the item itself is marked by the
/// caller
pub async fn trash(conn: &mut SqliteConnection, node_id: &str, trash_id: &str, at: i64) -> AppResult<Option<Unfinished>> {
    let mut frontier = VecDeque::from([node_id.to_string()]);
    let (_, done) = trash_batch(conn, &mut frontier, trash_id, at, batch()).await?;
    if done {
        return Ok(None);
    }
    let c = Change { trash_id: Some(trash_id.into()), trashed_at: Some(at), ..change("trash", node_id) };
    record(conn, &c).await?;
    Ok(Some(Unfinished { id: c.id, frontier: Some(frontier) }))
}

/// Takes everything that went to the trash with `node_id` out of it; the item itself is taken out by the caller
pub async fn restore(conn: &mut SqliteConnection, node_id: &str, trash_id: &str) -> AppResult<Option<Unfinished>> {
    let (_, done) = restore_batch(conn, trash_id, batch()).await?;
    if done {
        return Ok(None);
    }
    let c = Change { trash_id: Some(trash_id.into()), ..change("restore", node_id) };
    record(conn, &c).await?;
    Ok(Some(Unfinished { id: c.id, frontier: None }))
}

/// Starts deleting an item in the trash, and everything in it, for good: the trash no longer lists it. The deleting
/// itself is `run`, after the transaction.
pub async fn purge(conn: &mut SqliteConnection, node_id: &str) -> AppResult<Unfinished> {
    let c = change("purge", node_id);
    record(conn, &c).await?;
    Ok(Unfinished { id: c.id, frontier: None })
}

/// Starts deleting, for good, the trash items `select` finds (their ids, from `?1` = `param`), all at once
pub async fn purge_selected(conn: &mut SqliteConnection, select: &str, param: &str) -> AppResult<Vec<Unfinished>> {
    let rows: Vec<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "INSERT INTO tree_changes (id, kind, node_id, created_at)
         SELECT lower(hex(randomblob(16))), 'purge', id, ?2 FROM ({select}) WHERE true
         RETURNING id"
    )))
    .bind(param)
    .bind(now())
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(|(id,)| Unfinished { id, frontier: None }).collect())
}

/// How many items the deletions `rows` have left to delete (read without the write lock, for their progress)
pub async fn items_left(st: &AppState, rows: &[Unfinished]) -> AppResult<u64> {
    let ids = serde_json::to_string(&rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()).unwrap();
    let (n,): (i64,) = sqlx::query_as(
        "WITH RECURSIVE sub(id) AS (
           SELECT node_id FROM tree_changes WHERE id IN (SELECT value FROM json_each(?1))
           UNION SELECT c.id FROM nodes c JOIN sub ON c.parent_id = sub.id
         )
         SELECT COUNT(*) FROM sub",
    )
    .bind(ids)
    .fetch_one(&st.db)
    .await?;
    Ok(n as u64)
}

// ───────────── Batches ─────────────

/// One batch of a path change; returns how many items changed and whether that was the last of them
async fn repath_batch(conn: &mut SqliteConnection, drive_id: &str, old: &str, new: &str, limit: i64) -> AppResult<(i64, bool)> {
    // A range rather than `substr`, so the index on (drive_id, fs_path) finds the items ('0' comes right after '/').
    // Items changed are out of the range: a path never moves below itself.
    let n = sqlx::query(
        "UPDATE nodes SET fs_path = ?3 || substr(fs_path, length(?2) + 1)
         WHERE rowid IN (
           SELECT rowid FROM nodes
           WHERE drive_id = ?1 AND fs_path IS NOT NULL AND (fs_path = ?2 OR (fs_path >= ?2 || '/' AND fs_path < ?2 || '0'))
           LIMIT ?4
         )",
    )
    .bind(drive_id)
    .bind(old)
    .bind(new)
    .bind(limit)
    .execute(conn)
    .await?
    .rows_affected() as i64;
    Ok((n, n < limit))
}

/// One batch of moving to the trash: the items in the folders of `frontier`, and in the folders found in them, in
/// turn. Returns how many items were marked, and whether that was all.
async fn trash_batch(conn: &mut SqliteConnection, frontier: &mut VecDeque<String>, trash_id: &str, at: i64, limit: i64) -> AppResult<(i64, bool)> {
    let (mut marked, mut work) = (0, 0);
    while work < limit
        && let Some(folder) = frontier.front().cloned()
    {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "UPDATE nodes SET trashed_at = ?1, trash_id = ?2
             WHERE rowid IN (SELECT rowid FROM nodes WHERE parent_id = ?3 AND trashed_at IS NULL LIMIT ?4)
             RETURNING id, kind",
        )
        .bind(at)
        .bind(trash_id)
        .bind(&folder)
        .bind(limit - work)
        .fetch_all(&mut *conn)
        .await?;
        let n = rows.len() as i64;
        if n < limit - work {
            frontier.pop_front();
        }
        frontier.extend(rows.into_iter().filter(|(_, kind)| kind == "folder").map(|(id, _)| id));
        marked += n;
        // Looking into a folder counts too, so a transaction never goes through too many empty ones
        work += n.max(1);
    }
    Ok((marked, frontier.is_empty()))
}

/// One batch of restoring from the trash
async fn restore_batch(conn: &mut SqliteConnection, trash_id: &str, limit: i64) -> AppResult<(i64, bool)> {
    let n = sqlx::query(
        "UPDATE nodes SET trashed_at = NULL, trash_root = 0, trash_id = NULL, trashed_by = NULL
         WHERE rowid IN (SELECT rowid FROM nodes WHERE trash_id = ?1 LIMIT ?2)",
    )
    .bind(trash_id)
    .bind(limit)
    .execute(conn)
    .await?
    .rows_affected() as i64;
    Ok((n, n < limit))
}

/// One batch of deleting `top` and everything in it for good, going down its folders depth first (`stack`, the folders
/// on the way down, starts with `top`): a folder's files, then its folders, then the folder itself once it is empty.
/// Returns how many items went, and whether `top` went too.
async fn purge_batch(conn: &mut SqliteConnection, top: &str, stack: &mut VecDeque<String>, limit: i64, out: &mut Leftovers) -> AppResult<(i64, bool)> {
    if stack.is_empty() {
        stack.push_back(top.to_string());
    }
    // (id, space, kind, size, content)
    type Row = (String, Option<String>, String, i64, Option<String>);
    let mut freed: HashMap<String, i64> = HashMap::new();
    let mut hashes = Vec::new();
    let (mut deleted, mut work, mut top_gone) = (0, 0, false);
    while work < limit
        && let Some(folder) = stack.back().cloned()
    {
        let files: Vec<Row> = sqlx::query_as("SELECT id, drive_id, kind, size, blob_hash FROM nodes WHERE parent_id = ? AND kind <> 'folder' LIMIT ?")
            .bind(&folder)
            .bind(limit - work)
            .fetch_all(&mut *conn)
            .await?;
        let gone = if !files.is_empty() {
            files
        } else {
            let folders: Vec<(String,)> = sqlx::query_as("SELECT id FROM nodes WHERE parent_id = ? AND kind = 'folder'").bind(&folder).fetch_all(&mut *conn).await?;
            if !folders.is_empty() {
                stack.extend(folders.into_iter().map(|(id,)| id));
                work += 1;
                continue;
            }
            // Empty now: the folder itself goes (or the item, when `top` is a file)
            stack.pop_back();
            let Some(row): Option<Row> =
                sqlx::query_as("SELECT id, drive_id, kind, size, blob_hash FROM nodes WHERE id = ?").bind(&folder).fetch_optional(&mut *conn).await?
            else {
                top_gone |= folder == top;
                continue;
            };
            if folder == top {
                top_gone = true;
                if let Some(node) = super::get_node(&mut *conn, top).await? {
                    out.on_disk.extend(crate::fsops::trash_folder(&node));
                }
            }
            vec![row]
        };
        let mut ids = Vec::with_capacity(gone.len());
        let mut files = Vec::new();
        for (id, drive, kind, size, hash) in gone {
            if kind != "folder" {
                *freed.entry(drive.unwrap_or_default()).or_default() += size;
                files.push(id.clone());
            }
            hashes.extend(hash);
            ids.push(id);
        }
        let versions = crate::versions::purge_nodes(conn, &serde_json::to_string(&files).unwrap()).await?;
        out.blobs.extend(versions.blobs);
        out.on_disk.extend(versions.files);
        sqlx::query("DELETE FROM nodes WHERE id IN (SELECT value FROM json_each(?))").bind(serde_json::to_string(&ids).unwrap()).execute(&mut *conn).await?;
        deleted += ids.len() as i64;
        work += ids.len() as i64;
    }
    for (drive, bytes) in freed {
        adjust_usage(conn, &drive, -bytes).await?;
    }
    out.blobs.extend(release_blobs(conn, &hashes).await?);
    Ok((deleted, top_gone || stack.is_empty()))
}

/// One batch of `c`; returns how many items it changed and whether it is finished
async fn step(conn: &mut SqliteConnection, c: &Change, frontier: &mut Option<VecDeque<String>>, limit: i64, out: &mut Leftovers) -> AppResult<(i64, bool)> {
    match c.kind.as_str() {
        "repath" => {
            let (Some(drive), Some(old), Some(new)) = (&c.drive_id, &c.old_path, &c.new_path) else { return Ok((0, true)) };
            repath_batch(conn, drive, old, new, limit).await
        }
        "trash" => {
            let (Some(trash_id), Some(at)) = (&c.trash_id, c.trashed_at) else { return Ok((0, true)) };
            // After a stop, the folders to go through are found again: those already marked, whose items may not be
            let frontier = match frontier {
                Some(f) => f,
                None => {
                    let folders: Vec<(String,)> =
                        sqlx::query_as("SELECT id FROM nodes WHERE trash_id = ? AND kind = 'folder'").bind(trash_id).fetch_all(&mut *conn).await?;
                    frontier.insert(folders.into_iter().map(|(id,)| id).collect())
                }
            };
            trash_batch(conn, frontier, trash_id, at, limit).await
        }
        "restore" => {
            let Some(trash_id) = &c.trash_id else { return Ok((0, true)) };
            restore_batch(conn, trash_id, limit).await
        }
        "purge" => purge_batch(conn, &c.node_id, frontier.get_or_insert_with(VecDeque::new), limit, out).await,
        _ => Ok((0, true)),
    }
}

// ───────────── Finishing ─────────────

/// Finishes the changes `rows`, a batch per transaction, counting the items changed into `progress`
pub async fn run(st: &AppState, rows: Vec<Unfinished>, progress: &Tracker) -> AppResult<()> {
    let mut rows: VecDeque<Unfinished> = rows.into();
    while !rows.is_empty() {
        let mut out = Leftovers::default();
        {
            let _w = st.write_lock.lock().await;
            let mut tx = crate::db::begin_write(&st.db).await?;
            let mut left = batch();
            while left > 0
                && let Some(row) = rows.front_mut()
            {
                // Gone already: its item was deleted meanwhile (with everything in it), or it was finished elsewhere
                let Some(c): Option<Change> = sqlx::query_as("SELECT * FROM tree_changes WHERE id = ?").bind(&row.id).fetch_optional(&mut *tx).await? else {
                    rows.pop_front();
                    continue;
                };
                let (n, done) = step(&mut tx, &c, &mut row.frontier, left, &mut out).await?;
                progress.add(n as u64);
                left -= n.max(1);
                if done {
                    sqlx::query("DELETE FROM tree_changes WHERE id = ?").bind(&c.id).execute(&mut *tx).await?;
                    rows.pop_front();
                }
            }
            tx.commit().await?;
        }
        schedule_blob_removal(st, out.blobs);
        crate::fsops::remove_below_later(out.on_disk);
        // Whatever waits for the write lock, or for the one worker a small server has, gets its turn in between
        tokio::task::yield_now().await;
    }
    Ok(())
}

/// Finishes `rows` in the background, then lets go of `held` (the scan locks of the folder spaces they change)
pub fn finish_later(st: &AppState, rows: Vec<Unfinished>, held: Vec<OwnedMutexGuard<()>>) {
    if rows.is_empty() {
        return;
    }
    let st = st.clone();
    tokio::spawn(async move {
        if let Err(e) = run(&st, rows, &Tracker::default()).await {
            tracing::warn!("Couldn't finish a change of many items, will finish it at the next start: {}", e.message);
        }
        drop(held);
    });
}

/// Deletes an item and everything in it for good, a batch per transaction, without hiding it first: for items that
/// are gone from a folder space's folder, with its scan lock held. Returns how many items went.
pub async fn purge_now(st: &AppState, id: &str) -> AppResult<usize> {
    let mut stack = VecDeque::new();
    let mut deleted = 0;
    loop {
        let mut out = Leftovers::default();
        let done = {
            let _w = st.write_lock.lock().await;
            let mut tx = crate::db::begin_write(&st.db).await?;
            let (n, done) = purge_batch(&mut tx, id, &mut stack, batch(), &mut out).await?;
            tx.commit().await?;
            deleted += n as usize;
            done
        };
        schedule_blob_removal(st, out.blobs);
        crate::fsops::remove_below_later(out.on_disk);
        if done {
            return Ok(deleted);
        }
        tokio::task::yield_now().await;
    }
}

/// Items of a folder space at `old` and below it are at `new` now, a batch per transaction: for a folder a scan found
/// moved, with the space's scan lock held
pub async fn repath_now(st: &AppState, drive_id: &str, old: &str, new: &str) -> AppResult<()> {
    if old == new {
        return Ok(());
    }
    loop {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let (_, done) = repath_batch(&mut tx, drive_id, old, new, batch()).await?;
        tx.commit().await?;
        if done {
            return Ok(());
        }
        drop(_w);
        tokio::task::yield_now().await;
    }
}

/// At startup: finishes the changes a stop left unfinished, in the background, holding the scan locks of the folder
/// spaces they change from before any scan can start
pub async fn resume(st: &AppState) -> AppResult<()> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT c.id, CASE WHEN d.mode = 'folder' THEN d.id END FROM tree_changes c
         LEFT JOIN nodes n ON n.id = c.node_id LEFT JOIN drives d ON d.id = COALESCE(c.drive_id, n.drive_id)
         ORDER BY c.created_at, c.rowid",
    )
    .fetch_all(&st.db)
    .await?;
    if rows.is_empty() {
        return Ok(());
    }
    let mut drives: Vec<String> = rows.iter().filter_map(|(_, d)| d.clone()).collect();
    drives.sort();
    drives.dedup();
    let mut held = Vec::with_capacity(drives.len());
    for d in &drives {
        held.push(crate::fsops::lock_space(st, d).await);
    }
    tracing::info!("Finishing {} change(s) of many items that stopped when ThirtyFile stopped", rows.len());
    finish_later(st, rows.into_iter().map(|(id, _)| Unfinished { id, frontier: None }).collect(), held);
    Ok(())
}

/// Tests: waits until every change is finished
#[cfg(test)]
pub async fn settled(st: &AppState) {
    for _ in 0..500 {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tree_changes").fetch_one(&st.db).await.unwrap();
        if n == 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("changes of many items didn't finish");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{nodes, testutil};
    use axum::{Json, extract::State};
    use serde_json::json;

    fn req<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> Json<T> {
        Json(serde_json::from_value(v).unwrap())
    }

    /// A folder "Big" in `parent` with `folders` folders of `files` files each; returns its id
    async fn big_folder(env: &testutil::TestEnv, owner: &crate::auth::User, parent: &str, folders: usize, files: usize) -> String {
        let top = env.folder(owner, parent, "Big").await;
        for f in 0..folders {
            let d = env.folder(owner, &top, &format!("d{f}")).await;
            for i in 0..files {
                env.file(owner, &d, &format!("f{i}.txt")).await;
            }
        }
        top
    }

    /// (items in `id` and below, of them in the trash)
    async fn below(env: &testutil::TestEnv, id: &str) -> (i64, i64) {
        sqlx::query_as(
            "WITH RECURSIVE sub(id) AS (SELECT ?1 UNION ALL SELECT c.id FROM nodes c JOIN sub ON c.parent_id = sub.id)
             SELECT COUNT(*), COUNT(n.trashed_at) FROM sub JOIN nodes n ON n.id = sub.id",
        )
        .bind(id)
        .fetch_one(&env.st.db)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn deleting_a_large_folder_for_good_lets_other_changes_in_between() {
        let env = testutil::env().await;
        let amy = env.user("amy", false).await;
        let big = big_folder(&env, &amy, amy.root(), 5, 20).await;
        let _small = small_batches(10);
        let _ = nodes::trash(State(env.st.clone()), amy.clone(), req(json!({ "ids": [big] }))).await.unwrap();
        settled(&env.st).await;
        assert_eq!(below(&env, &big).await, (106, 106));

        let (st, a) = (env.st.clone(), amy.clone());
        let deleting = tokio::spawn(nodes::delete_forever(State(st), a, req(json!({ "ids": [big] }))));
        // Other changes get turns while it is deleted: they see it partly deleted
        let seen = turns_between(&env, &big, &deleting).await;
        assert!(steps(&seen) >= 5, "deleted in one go: {seen:?}");
        // It has left the trash already
        let Json(trash) = nodes::list_trash(State(env.st.clone()), amy.clone(), axum::extract::Query(Default::default())).await.unwrap();
        assert!(trash.into_items().is_empty());
        let Json(job) = deleting.await.unwrap().unwrap();
        let job = if job.running() { crate::jobs::wait_for(&env.st, &job.id).await } else { job };
        assert_eq!((job.state, job.done, job.total), ("done", 106, 106));
        assert_eq!(below(&env, &big).await, (0, 0));
        settled(&env.st).await;
    }

    /// How many different amounts `turns_between` saw: deleting a folder takes at least one transaction per level of
    /// folders in it, and a few more in batches
    fn steps(seen: &[i64]) -> usize {
        seen.iter().collect::<std::collections::BTreeSet<_>>().len()
    }

    /// Takes turns with the write lock until `task` is done and nothing is left of `id`: how many items were left of it
    /// at each turn
    async fn turns_between<T>(env: &testutil::TestEnv, id: &str, task: &tokio::task::JoinHandle<T>) -> Vec<i64> {
        let (id, env2) = (id.to_string(), env);
        turns_counting(env, task, 0, move || {
            let id = id.clone();
            async move { below(env2, &id).await.0 }
        })
        .await
    }

    /// Takes turns with the write lock until `task` is done and `count` gives `until`: what it gave at each turn
    async fn turns_counting<T, F, Fut>(env: &testutil::TestEnv, task: &tokio::task::JoinHandle<T>, until: i64, count: F) -> Vec<i64>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = i64>,
    {
        let mut seen = Vec::new();
        for _ in 0..10_000 {
            let n = {
                let _w = env.st.write_lock.lock().await;
                count().await
            };
            seen.push(n);
            if n == until && task.is_finished() {
                break;
            }
            tokio::task::yield_now().await;
        }
        seen
    }

    #[tokio::test]
    async fn moving_a_large_folder_to_the_trash_and_back_lets_other_changes_in_between() {
        let env = testutil::env().await;
        let amy = env.user("amy", false).await;
        let big = big_folder(&env, &amy, amy.root(), 5, 20).await;
        let _small = small_batches(10);
        let (st, a, id) = (env.st.clone(), amy.clone(), big.clone());
        let trashing = tokio::spawn(async move { nodes::trash(State(st), a, req(json!({ "ids": [id] }))).await.map(|_| ()) });
        let trashed = || async { below(&env, &big).await.1 };
        let seen = turns_counting(&env, &trashing, 106, trashed).await;
        assert!(steps(&seen) >= 5, "marked in one go: {seen:?}");
        trashing.await.unwrap().unwrap();
        settled(&env.st).await;
        assert_eq!(below(&env, &big).await, (106, 106));

        // Back out of the trash, a batch at a time as well
        let (st, a, id) = (env.st.clone(), amy.clone(), big.clone());
        let restoring = tokio::spawn(async move { nodes::restore(State(st), a, req(json!({ "ids": [id] }))).await.map(|_| ()) });
        let seen = turns_counting(&env, &restoring, 0, trashed).await;
        assert!(steps(&seen) >= 5, "restored in one go: {seen:?}");
        restoring.await.unwrap().unwrap();
        settled(&env.st).await;
        assert_eq!(below(&env, &big).await, (106, 0));
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE trash_id IS NOT NULL OR trash_root = 1").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn an_item_is_left_alone_until_the_rest_has_followed_it() {
        let env = testutil::env().await;
        let amy = env.user("amy", false).await;
        let big = big_folder(&env, &amy, amy.root(), 5, 20).await;
        let _small = small_batches(10);
        // In the trash, but not everything in it yet
        {
            let mut tx = crate::db::begin_write(&env.st.db).await.unwrap();
            sqlx::query("UPDATE nodes SET trashed_at = 5, trash_id = 't', trash_root = 1 WHERE id = ?").bind(&big).execute(&mut *tx).await.unwrap();
            assert!(trash(&mut tx, &big, "t", 5).await.unwrap().is_some());
            tx.commit().await.unwrap();
        }
        // Restoring it now would leave some of it behind
        let err = nodes::restore(State(env.st.clone()), amy.clone(), req(json!({ "ids": [big] }))).await.unwrap_err();
        assert_eq!((err.status, err.message.as_str()), (axum::http::StatusCode::CONFLICT, "This item is still being changed. Try again in a moment."));
        resume(&env.st).await.unwrap();
        settled(&env.st).await;
        assert_eq!(below(&env, &big).await, (106, 106));
        // Out of the trash, but not everything in it yet: moving or copying it would leave some of it behind
        {
            let mut tx = crate::db::begin_write(&env.st.db).await.unwrap();
            sqlx::query("UPDATE nodes SET trashed_at = NULL, trash_id = NULL, trash_root = 0 WHERE id = ?").bind(&big).execute(&mut *tx).await.unwrap();
            assert!(restore(&mut tx, &big, "t").await.unwrap().is_some());
            tx.commit().await.unwrap();
        }
        let other = env.folder(&amy, amy.root(), "Other").await;
        for copying in [true, false] {
            let r = req(json!({ "ids": [big], "dest_id": other }));
            let err = if copying {
                nodes::copy_nodes(State(env.st.clone()), amy.clone(), r).await
            } else {
                nodes::move_nodes(State(env.st.clone()), amy.clone(), r).await
            };
            assert_eq!(err.unwrap_err().status, axum::http::StatusCode::CONFLICT);
        }
        resume(&env.st).await.unwrap();
        settled(&env.st).await;
        assert_eq!(below(&env, &big).await, (106, 0));
        let _ = nodes::move_nodes(State(env.st.clone()), amy.clone(), req(json!({ "ids": [big], "dest_id": other }))).await.unwrap();
    }

    #[tokio::test]
    async fn renaming_a_large_folder_in_a_folder_space_changes_its_paths_a_batch_at_a_time() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let space = env.folder_space("Shared").await;
        for i in 0..30 {
            testutil::write_old(&space.dir.join(format!("Sub/Inner/f{i}.txt")), b"x");
        }
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (sub, _) = env.node_at(&space.drive, "Sub").await.unwrap();
        let _small = small_batches(10);
        let (st, a, id) = (env.st.clone(), admin.clone(), sub.clone());
        let renaming = tokio::spawn(async move { nodes::rename(State(st), a, axum::extract::Path(id), req(json!({ "name": "Renamed" }))).await.map(|_| ()) });
        let drive = space.drive.clone();
        let old_paths = || {
            let (st, drive) = (env.st.clone(), drive.clone());
            async move {
                let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE drive_id = ? AND (fs_path = 'Sub' OR fs_path LIKE 'Sub/%')")
                    .bind(&drive)
                    .fetch_one(&st.db)
                    .await
                    .unwrap();
                n
            }
        };
        let seen = turns_counting(&env, &renaming, 0, old_paths).await;
        assert!(steps(&seen) >= 4, "changed in one go: {seen:?}");
        renaming.await.unwrap().unwrap();
        settled(&env.st).await;
        assert!(space.dir.join("Renamed/Inner/f29.txt").is_file());
        assert_eq!(env.node_at(&space.drive, "Renamed/Inner/f29.txt").await.map(|(_, s)| s), Some(1));
        // The index matches the folder: a scan finds nothing to change
        let report = crate::folders::scan(&env.st, &space.drive).await.unwrap();
        assert_eq!((report.added, report.moved, report.removed), (0, 0, 0));
    }

    #[tokio::test]
    async fn a_folder_removed_on_the_server_leaves_the_index_a_batch_at_a_time() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let space = env.folder_space("Shared").await;
        for i in 0..30 {
            testutil::write_old(&space.dir.join(format!("Sub/f{i}.txt")), b"x");
        }
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (sub, _) = env.node_at(&space.drive, "Sub").await.unwrap();
        std::fs::remove_dir_all(space.dir.join("Sub")).unwrap();
        let _small = small_batches(10);
        let (st, drive) = (env.st.clone(), space.drive.clone());
        let scanning = tokio::spawn(async move { crate::folders::scan(&st, &drive).await.map(|_| ()) });
        let seen = turns_between(&env, &sub, &scanning).await;
        assert!(steps(&seen) >= 4, "removed in one go: {seen:?}");
        scanning.await.unwrap().unwrap();
        assert_eq!(below(&env, &sub).await.0, 0);
        let _ = admin;
    }

    #[tokio::test]
    async fn changes_a_stop_left_unfinished_are_finished_at_the_next_start() {
        let env = testutil::env().await;
        let amy = env.user("amy", false).await;
        let big = big_folder(&env, &amy, amy.root(), 5, 20).await;
        let old = env.folder(&amy, amy.root(), "Old").await;
        let gone = big_folder(&env, &amy, &old, 2, 10).await;
        let _small = small_batches(10);
        // As a stop leaves them: the first batch of each, and their rows
        {
            let mut tx = crate::db::begin_write(&env.st.db).await.unwrap();
            sqlx::query("UPDATE nodes SET trashed_at = 5, trash_id = 't', trash_root = 1 WHERE id = ?").bind(&big).execute(&mut *tx).await.unwrap();
            assert!(trash(&mut tx, &big, "t", 5).await.unwrap().is_some());
            let _ = purge(&mut tx, &gone).await.unwrap();
            tx.commit().await.unwrap();
        }
        // Still being changed
        let mut c = env.st.db.acquire().await.unwrap();
        assert!(refuse_busy(&mut c, &big).await.is_err());
        drop(c);
        resume(&env.st).await.unwrap();
        settled(&env.st).await;
        assert_eq!(below(&env, &big).await, (106, 106));
        assert_eq!(below(&env, &gone).await, (0, 0));
    }

    /// The issue's estimate (about 90 µs per item, so a million items hold the write lock for over 90 seconds), measured:
    /// `ITEMS=100000 cargo test --release -- --ignored --nocapture measure_deleting`
    #[tokio::test]
    #[ignore]
    async fn measure_deleting_a_large_folder() {
        let env = testutil::env().await;
        let amy = env.user("amy", false).await;
        let n: usize = std::env::var("ITEMS").ok().and_then(|v| v.parse().ok()).unwrap_or(100_000);
        let make = |name: &'static str| {
            let (st, root, owner) = (env.st.clone(), amy.root().to_string(), amy.id);
            async move {
                let top = crate::content::create_folder(&mut st.db.acquire().await.unwrap(), owner, &root, name).await.unwrap();
                let mut tx = crate::db::begin_write(&st.db).await.unwrap();
                for f in 0..n / 100 {
                    let d = crate::content::create_folder(&mut tx, owner, &top, &format!("d{f}")).await.unwrap();
                    let rows: Vec<serde_json::Value> = (0..99).map(|i| json!([new_id(), format!("f{i}.txt")])).collect();
                    sqlx::query(
                        "INSERT INTO nodes (id, owner_id, parent_id, kind, name, size, drive_id, created_at, updated_at)
                         SELECT json_extract(value, '$[0]'), ?2, ?3, 'file', json_extract(value, '$[1]'), 1, (SELECT drive_id FROM nodes WHERE id = ?3), 0, 0
                         FROM json_each(?1)",
                    )
                    .bind(serde_json::to_string(&rows).unwrap())
                    .bind(owner)
                    .bind(&d)
                    .execute(&mut *tx)
                    .await
                    .unwrap();
                }
                tx.commit().await.unwrap();
                top
            }
        };
        // As before: one transaction
        let one = make("One").await;
        let started = std::time::Instant::now();
        let mut tx = crate::db::begin_write(&env.st.db).await.unwrap();
        let _ = crate::tree::purge_subtree(&mut tx, &one).await.unwrap();
        tx.commit().await.unwrap();
        let whole = started.elapsed();
        // Now: a batch per transaction
        let batched = make("Batched").await;
        let (mut longest, mut batches, mut stack) = (std::time::Duration::ZERO, 0, VecDeque::new());
        let started = std::time::Instant::now();
        loop {
            let t = std::time::Instant::now();
            let mut out = Leftovers::default();
            let mut tx = crate::db::begin_write(&env.st.db).await.unwrap();
            let (_, done) = purge_batch(&mut tx, &batched, &mut stack, BATCH, &mut out).await.unwrap();
            tx.commit().await.unwrap();
            longest = longest.max(t.elapsed());
            batches += 1;
            if done {
                break;
            }
        }
        println!(
            "{n} items: one transaction {:.1} s ({:.0} µs per item); in batches {:.1} s in all, {batches} transactions, the longest {} ms",
            whole.as_secs_f64(),
            whole.as_micros() as f64 / n as f64,
            started.elapsed().as_secs_f64(),
            longest.as_millis()
        );
    }
}
