//! The trash: listing, restoring, deleting for good and emptying it

use std::{collections::HashMap, future::Future, pin::Pin};

use axum::{
    Json,
    extract::{Query, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqliteConnection;

use crate::{
    auth::User,
    error::{AppError, AppResult},
    fsops,
    jobs::{self, Job, Limit, Outcome, Tracker},
    logs,
    state::AppState,
    tree::{
        self, NODE_COLS, Need, Node, Role,
        changes::{self, NOT_PURGING},
    },
    util::now,
};

use super::{BatchReq, Listing, Located, Resolution, encode_cursor, locate, outermost, page_of, replace_existing};

/// Where a trashed item goes back to: the nearest folder above it that is out of the trash (the space's root folder
/// when the folders above it are gone), and the names of the folders in the trash on the way from there down to the
/// item. Those are made again when it is restored, as in Windows.
pub(super) struct Place {
    pub(super) folder: String,
    pub(super) missing: Vec<String>,
}

/// Spaces whose trash the user works with: their own spaces (at least `min_role`), plus every team space for administrators
pub(super) async fn trash_drives(conn: &mut SqliteConnection, user: &User, min_role: Role) -> AppResult<Vec<String>> {
    let current = crate::auth::current_user(conn, user).await?;
    let user = &current;
    let mut ids: Vec<String> = tree::user_drives(conn, user).await?.into_iter().filter(|(_, r)| *r >= min_role).map(|(d, _)| d.id).collect();
    if user.is_admin() {
        let all: Vec<(String,)> = sqlx::query_as("SELECT id FROM drives WHERE kind != 'personal' AND disabled = 0").fetch_all(conn).await?;
        for (id,) in all {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    Ok(ids)
}

#[derive(Deserialize, Default)]
pub struct TrashQuery {
    /// Items per page (at most MAX_PAGE); without it, the whole list comes at once
    pub(super) limit: Option<i64>,
    /// The `next` of the previous page
    pub(super) after: Option<String>,
    /// Only items the user deleted (Deleted by me); otherwise everyone's
    pub(super) mine: Option<bool>,
}

/// Where a trash page ends (newest deleted first)
#[derive(Serialize, Deserialize)]
pub(super) struct TrashCursor {
    pub(super) at: i64,
    pub(super) id: String,
}

/// Trash: deleted items in spaces I'm a member of
pub async fn list_trash(State(st): State<AppState>, user: User, Query(q): Query<TrashQuery>) -> AppResult<Json<Listing<Located>>> {
    // A page at a time, also when no size is asked for: a large trash isn't listed and located whole
    let (limit, after) = page_of::<TrashCursor>(Some(q.limit.unwrap_or(super::MAX_PAGE)), q.after.as_deref())?;
    let drive_ids = trash_drives(&mut *st.db.acquire().await?, &user, Role::Viewer).await?;
    let keyset = if after.is_some() { "AND (n.trashed_at < ?2 OR (n.trashed_at = ?2 AND n.id < ?3))" } else { "" };
    // The filter is one more condition on the same order, so paging works the same with it
    let mine = if q.mine == Some(true) { "AND n.trashed_by = ?5" } else { "" };
    let sql = format!(
        "SELECT {NODE_COLS}, (SELECT username FROM users WHERE id = n.trashed_by) AS deleted_by FROM nodes n
         WHERE n.trash_root = 1 AND n.drive_id IN (SELECT value FROM json_each(?1)) AND {NOT_PURGING} AND {} {keyset} {mine}
         ORDER BY n.trashed_at DESC, n.id DESC LIMIT ?4",
        trash_visible(&user)
    );
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        node: Node,
        deleted_by: Option<String>,
    }
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
        .bind(serde_json::to_string(&drive_ids).unwrap())
        .bind(after.as_ref().map(|c| c.at))
        .bind(after.as_ref().map(|c| c.id.clone()))
        .bind(limit.unwrap_or(-1))
        .bind(user.id)
        .fetch_all(&st.db)
        .await?;
    let next = match (limit, rows.last()) {
        (Some(l), Some(last)) if rows.len() as i64 == l => {
            Some(encode_cursor(&TrashCursor { at: last.node.trashed_at.unwrap_or_default(), id: last.node.id.clone() }))
        }
        _ => None,
    };
    let mut deleted_by: HashMap<String, String> = HashMap::new();
    let mut nodes = Vec::with_capacity(rows.len());
    for r in rows {
        if let Some(name) = r.deleted_by {
            deleted_by.insert(r.node.id.clone(), name);
        }
        nodes.push(r.node);
    }
    let mut located = locate(&st, &user, nodes).await?;
    for l in &mut located {
        l.deleted_by = deleted_by.remove(&l.node.id);
    }
    Ok(Json(Listing::new(located, limit, next)))
}

/// The user's role for trash operations: their role on the item, and administrators manage the trash of every space
/// that isn't personal (the same rule as managing the space itself), whether or not they were given access to it
pub(super) async fn trash_role(conn: &mut SqliteConnection, user: &User, node: &Node) -> AppResult<Option<Role>> {
    let current = crate::auth::current_user(conn, user).await?;
    let user = &current;
    let role = tree::role_on(conn, user, node).await?;
    if user.is_admin()
        && let Some(drive) = tree::get_drive(conn, node.drive()).await?
        && drive.kind != tree::SpaceKind::Personal
        && !drive.disabled
    {
        return Ok(role.max(Some(Role::Manager)));
    }
    Ok(role)
}

fn trash_visible(user: &User) -> String {
    let visible = tree::visible_sql(user, "n.id");
    if user.is_admin() { format!("({visible} OR EXISTS(SELECT 1 FROM drives WHERE id = n.drive_id AND kind <> 'personal' AND NOT disabled))") } else { visible }
}

/// A trashed item and where it goes back to (`Place`). The user needs `need` on the folder there as well: a folder
/// that was merely shared with them gives no rights over the owner's parent folder, so they can't restore or destroy
/// the shared folder itself, only items inside it.
pub(super) async fn trash_root(conn: &mut SqliteConnection, user: &User, id: &str, need: Need) -> AppResult<(Node, Place)> {
    let current = crate::auth::current_user(conn, user).await?;
    let user = &current;
    let not_found = || AppError::not_found("Item not found in trash");
    let node = tree::get_node(conn, id).await?.ok_or_else(not_found)?;
    // Not one being deleted for good already
    let sql = format!("SELECT n.trash_root AND {} FROM nodes n WHERE n.id = ?", changes::NOT_PURGING);
    let (is_root,): (bool,) = sqlx::query_as(sqlx::AssertSqlSafe(sql)).bind(id).fetch_one(&mut *conn).await?;
    if !is_root {
        return Err(not_found());
    }
    let role = trash_role(conn, user, &node).await?.ok_or_else(not_found)?;
    tree::allows_on(conn, user, role, need, &node).await?;
    tree::require_subtree(conn, user, &node, need).await?;
    if node.space_read_only {
        return Err(tree::read_only_error(&node));
    }
    // Up through the folders in the trash, to the first one that isn't
    let (mut missing, mut up, mut parent) = (Vec::new(), node.parent_id.clone(), None);
    while let Some(id) = up {
        let Some(p) = tree::get_node(conn, &id).await? else { break };
        if p.trashed_at.is_none() {
            parent = Some(p);
            break;
        }
        missing.push(p.name.clone());
        up = p.parent_id.clone();
    }
    missing.reverse();
    let dest = match parent {
        Some(p) => p,
        None => {
            missing.clear();
            let root_id = tree::get_drive(conn, node.drive()).await?.map(|d| d.root_id).ok_or_else(not_found)?;
            tree::get_node(conn, &root_id).await?.ok_or_else(not_found)?
        }
    };
    let dest_role = trash_role(conn, user, &dest).await?.ok_or_else(|| AppError::forbidden("You don't have permission on the folder this item belongs to"))?;
    // Restoring adds to it (folders on the way made again included)
    tree::allows(user, dest_role, if need == Need::Read { Need::Read } else { Need::Add })?;
    Ok((node, Place { folder: dest.id, missing }))
}

/// The folder a trashed item goes into, when the folders on the way to it are all there (out of the trash): None while
/// one of them would be made
pub(super) async fn existing_folder(conn: &mut SqliteConnection, place: &Place) -> AppResult<Option<String>> {
    let mut folder = place.folder.clone();
    for name in &place.missing {
        match tree::find_child(conn, &folder, name).await? {
            Some(f) if f.is_folder() => folder = f.id,
            _ => return Ok(None),
        }
    }
    Ok(Some(folder))
}

/// What the activity log says of a folder made again
const MADE_AGAIN: &str = "Made again for an item restored from the trash";

/// The folder a restored item goes into: each folder on the way that is in the trash is made again, new and empty,
/// unless one of that name is there already (a file of that name gives the new folder a number)
async fn make_way(conn: &mut SqliteConnection, user: &User, locks: &fsops::SpaceLocks, place: &Place) -> AppResult<String> {
    let mut folder = place.folder.clone();
    for name in &place.missing {
        if let Some(f) = tree::find_child(conn, &folder, name).await?.filter(|f| f.is_folder()) {
            // Gone into only where the person may add, as uploads do
            folder = tree::folder_for(conn, user, &f.id, Need::Add).await?.id;
            continue;
        }
        let parent = tree::get_node(conn, &folder).await?.ok_or_else(|| AppError::not_found("Folder not found"))?;
        let id = if parent.in_folder_space() {
            let name = fsops::free_name(conn, &parent, name, true).await?;
            fsops::make_folder(conn, locks, user.id, &parent, &name).await?
        } else {
            let name = tree::unique_name(conn, &parent.id, name, true).await?;
            crate::content::create_folder(conn, user.id, &parent.id, &name).await?
        };
        tree::touch(conn, &parent.id).await?;
        let made = tree::get_node(conn, &id).await?;
        logs::record_activity(conn, user, made.as_ref().map(|n| n as _), "create_folder", MADE_AGAIN).await?;
        folder = id;
    }
    Ok(folder)
}

/// The ids, folders before what was in them (those with fewer folders above them first): an item restored together
/// with its folder goes back into it, whatever the order they were chosen in
async fn outer_first(conn: &mut SqliteConnection, ids: Vec<String>) -> AppResult<Vec<String>> {
    let mut by_depth = Vec::with_capacity(ids.len());
    for id in ids {
        let (depth,): (i64,) = sqlx::query_as(
            "WITH RECURSIVE up(id) AS (SELECT parent_id FROM nodes WHERE id = ?1 UNION ALL SELECT n.parent_id FROM nodes n JOIN up ON n.id = up.id)
             SELECT count(*) FROM up WHERE id IS NOT NULL",
        )
        .bind(&id)
        .fetch_one(&mut *conn)
        .await?;
        by_depth.push((depth, id));
    }
    by_depth.sort_by_key(|(depth, _)| *depth);
    Ok(by_depth.into_iter().map(|(_, id)| id).collect())
}

/// Whether a folder has something of its own that merging it into another folder would take away, or widen to what
/// that folder holds: it is restricted, or has grants, share links, an S3 bucket or an FTP receive folder on it
async fn keeps_its_own(conn: &mut SqliteConnection, folder: &Node) -> AppResult<bool> {
    if folder.restricted {
        return Ok(true);
    }
    let (own,): (bool,) = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM grants WHERE node_id = ?1) OR EXISTS(SELECT 1 FROM shares WHERE node_id = ?1)
           OR EXISTS(SELECT 1 FROM s3_buckets WHERE node_id = ?1) OR EXISTS(SELECT 1 FROM ftp_receive_folders WHERE node_id = ?1)",
    )
    .bind(&folder.id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(own)
}

/// Items a merge moves per transaction: in a folder space each is a rename on disk, written into the journal first
const MERGE_BATCH: i64 = 500;

/// A folder in the trash merged into the folder of its name, a batch per transaction (`merge`)
pub struct Merging {
    from: String,
    into: String,
    replace: bool,
    trash_id: Option<String>,
}

/// What people put on a folder for themselves goes to the folder it is merged into: favorites, recent items and tags
/// (one that folder has already stays as it is)
async fn hand_over(conn: &mut SqliteConnection, from: &str, into: &str) -> AppResult<()> {
    for table in ["favorites", "recent_files", "tagged"] {
        sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE OR IGNORE {table} SET node_id = ?1 WHERE node_id = ?2"))).bind(into).bind(from).execute(&mut *conn).await?;
    }
    Ok(())
}

/// Merges the folder `from`, in the trash, into the folder `into` of the same name, as Windows does: what went to the
/// trash with `from` goes into `into`, out of the trash (the folders it holds merged the same way into folders of the
/// same name, unless they have something of their own, `keeps_its_own`), then `from` itself goes. A file whose name
/// `into` has already takes the place of the one there, which goes to the trash (`m.replace`), or gets a number.
/// Items put in the trash on their own from inside `from` go back into `into` when they are restored; in a folder
/// space, what is on disk in `from` that the index doesn't have goes into `into` too.
/// At most `budget` items are moved (it counts down): returns whether `from` is done, else the next batch goes on.
fn merge<'a>(
    conn: &'a mut SqliteConnection,
    user: &'a User,
    locks: &'a fsops::SpaceLocks,
    m: &'a Merging,
    from: &'a Node,
    into: &'a Node,
    budget: &'a mut i64,
) -> Pin<Box<dyn Future<Output = AppResult<bool>> + Send + 'a>> {
    Box::pin(async move {
        let sql = format!("SELECT {NODE_COLS} FROM nodes n WHERE n.parent_id = ? AND n.trash_root = 0 ORDER BY n.rowid LIMIT ?");
        loop {
            if *budget <= 0 {
                return Ok(false);
            }
            let items: Vec<Node> = sqlx::query_as(sqlx::AssertSqlSafe(sql.clone())).bind(&from.id).bind(*budget).fetch_all(&mut *conn).await?;
            if items.is_empty() {
                break;
            }
            for item in &items {
                if *budget <= 0 {
                    return Ok(false);
                }
                let taken = tree::find_child(conn, &into.id, &item.name).await?;
                if let Some(there) = &taken
                    && there.is_folder()
                    && item.is_folder()
                    && !keeps_its_own(conn, item).await?
                {
                    let there = tree::folder_for(conn, user, &there.id, Need::Add).await?;
                    if !merge(conn, user, locks, m, item, &there, budget).await? {
                        return Ok(false);
                    }
                    continue;
                }
                if let Some(there) = &taken
                    && m.replace
                {
                    replace_existing(conn, user, locks, there, item).await?;
                }
                let name = if into.in_folder_space() {
                    // Also when the index has no item of that name: the disk may have one (that scans skip, or in other
                    // letter case on a disk that ignores it)
                    fsops::free_name(conn, into, &item.name, item.is_folder()).await?
                } else if taken.is_some() && !m.replace {
                    tree::unique_name(conn, &into.id, &item.name, item.is_folder()).await?
                } else {
                    item.name.clone()
                };
                if item.in_folder_space() {
                    fsops::rename(conn, locks, item, into, &name).await?;
                }
                // Out of the trash at once, so the next names taken see it
                sqlx::query("UPDATE nodes SET parent_id = ?, name = ?, trashed_at = NULL, trash_root = 0, trash_id = NULL, trashed_by = NULL WHERE id = ?")
                    .bind(&into.id)
                    .bind(&name)
                    .bind(&item.id)
                    .execute(&mut *conn)
                    .await?;
                *budget -= 1;
                // What is in it comes out with it, as much as the batch has room for; the rest right after
                if item.is_folder()
                    && let Some(trash_id) = &m.trash_id
                {
                    let (n, left) = changes::restore_up_to(conn, &item.id, trash_id, *budget).await?;
                    *budget -= n;
                    locks.later(left);
                }
            }
        }
        // Done with what the index has in it: what was put in the trash on its own from inside it, and what is only on
        // disk, goes into `into`, and so does what people put on it for themselves
        sqlx::query("UPDATE nodes SET parent_id = ? WHERE parent_id = ?").bind(&into.id).bind(&from.id).execute(&mut *conn).await?;
        if from.in_folder_space() {
            fsops::move_unindexed(conn, locks, from, into).await?;
        }
        hand_over(conn, &from.id, &into.id).await?;
        // Empty now (in a folder space, its folder in the trash goes with the trash folder, `fsops::clean_trash`)
        sqlx::query("DELETE FROM nodes WHERE id = ?").bind(&from.id).execute(&mut *conn).await?;
        *budget -= 1;
        Ok(true)
    })
}

/// Merges the folder `m.from` into `m.into` (`merge`), a batch per transaction, so a folder of many items neither
/// holds the write lock for long nor runs into a request's time limit. Each batch is whole: should one fail, the
/// batches before it stay, and the folder in the trash holds the rest, which restoring it again merges.
async fn merge_all(st: &AppState, user: &User, m: &Merging, progress: &Tracker) -> AppResult<()> {
    loop {
        let locks = fsops::lock(st, user, &[&m.from, &m.into]).await?;
        let size = changes::batch().min(MERGE_BATCH);
        let mut budget = size;
        let done = {
            let _w = st.write_lock.lock().await;
            let mut tx = crate::db::begin_write(&st.db).await?;
            // Restored or deleted for good meanwhile: nothing is left to merge
            let Some(from) = tree::get_node(&mut tx, &m.from).await?.filter(|n| n.trashed_at.is_some()) else { return Ok(()) };
            let into = tree::node_for(&mut tx, user, &m.into, Need::Add).await?;
            locks.check(&from)?;
            locks.check(&into)?;
            let done = merge(&mut tx, user, &locks, m, &from, &into, &mut budget).await?;
            tree::touch(&mut tx, &into.id).await?;
            locks.commit(tx).await?;
            done
        };
        progress.add((size - budget).max(0) as u64);
        // Dropping the locks finishes what the batch left (what is in large folders that came out) before the next
        drop(locks);
        if done {
            return Ok(());
        }
        tokio::task::yield_now().await;
    }
}

/// How many items the merges `merging` have to move: everything in the folders in the trash
async fn merge_size(st: &AppState, merging: &[Merging]) -> AppResult<u64> {
    let ids = serde_json::to_string(&merging.iter().map(|m| m.from.as_str()).collect::<Vec<_>>()).unwrap();
    let (n,): (i64,) = sqlx::query_as(
        "WITH RECURSIVE sub(id) AS (
           SELECT value FROM json_each(?1)
           UNION SELECT c.id FROM nodes c JOIN sub ON c.parent_id = sub.id WHERE c.trash_root = 0
         )
         SELECT COUNT(*) FROM sub",
    )
    .bind(ids)
    .fetch_one(&st.db)
    .await?;
    Ok(n as u64)
}

/// Merges what `restore_items` left to merge, counting the items into `progress`
pub async fn finish_merges(st: &AppState, user: &User, merging: Vec<Merging>, progress: &Tracker) -> AppResult<()> {
    if merging.is_empty() {
        return Ok(());
    }
    progress.set_total(merge_size(st, &merging).await?);
    for m in &merging {
        merge_all(st, user, m, progress).await?;
    }
    Ok(())
}

pub async fn restore(State(st): State<AppState>, user: User, Json(req): Json<BatchReq>) -> AppResult<Json<Job>> {
    let pending = jobs::reserve(&st, user.id, "restore", Limit::Changes)?;
    let merging = restore_items(&st, &user, &req).await?;
    // A folder merged into one of its name may hold many items: the page follows the rest as a job
    let job = pending.run(jobs::wait(), move |t| async move { finish_merges(&st, &user, merging, &t).await.map(|()| Outcome::default()) }).await?;
    Ok(Json(job))
}

/// Restores the items `req` names, in one transaction; a folder whose name a folder has in its place now is merged
/// into that folder afterwards (`finish_merges`, with what this returns)
pub async fn restore_items(st: &AppState, user: &User, req: &BatchReq) -> AppResult<Vec<Merging>> {
    let ids = req.ids()?;
    let locks = fsops::lock_seen(st, user, &ids.iter().map(String::as_str).collect::<Vec<_>>()).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let mut merging = Vec::new();
    for id in &outer_first(&mut tx, ids).await? {
        // Restored into the original folder, made again (with the folders above it) if it is in the trash too
        let (node, place) = trash_root(&mut tx, user, id, Need::Move).await?;
        locks.check(&node)?;
        changes::refuse_busy(&mut tx, &node.id).await?;
        let parent_id = make_way(&mut tx, user, &locks, &place).await?;
        // Without an answer the item gets a number when its name is taken, as it always did
        if let Some(existing) = tree::find_child(&mut tx, &parent_id, &node.name).await? {
            // A folder where a folder has its name now is merged into it, whatever the answer but Skip; unless it has
            // something of its own, which it keeps: it then comes back with a number, as a file does
            if node.is_folder() && existing.is_folder() && !keeps_its_own(&mut tx, &node).await? {
                let resolution = req.resolution(&node.id);
                if resolution == Some(Resolution::Skip) {
                    continue;
                }
                let into = tree::node_for(&mut tx, user, &existing.id, Need::Add).await?;
                locks.check(&into)?;
                changes::refuse_busy(&mut tx, &into.id).await?;
                let (trash_id,): (Option<String>,) = sqlx::query_as("SELECT trash_id FROM nodes WHERE id = ?").bind(&node.id).fetch_one(&mut *tx).await?;
                logs::record_activity(&mut tx, user, Some(&node), "restore", "").await?;
                merging.push(Merging { from: node.id.clone(), into: into.id, replace: resolution == Some(Resolution::Replace), trash_id });
                continue;
            }
            match req.resolution(&node.id) {
                Some(Resolution::Skip) => continue,
                Some(Resolution::Replace) => replace_existing(&mut tx, user, &locks, &existing, &node).await?,
                Some(Resolution::Keep) | None => {}
            }
        }
        let name = if node.in_folder_space() {
            let dest = tree::get_node(&mut tx, &parent_id).await?.ok_or_else(|| AppError::not_found("Folder not found"))?;
            let name = fsops::free_name(&mut tx, &dest, &node.name, node.is_folder()).await?;
            fsops::restore(&mut tx, &locks, &node, &dest, &name).await?;
            name
        } else {
            tree::unique_name(&mut tx, &parent_id, &node.name, node.is_folder()).await?
        };
        let (trash_id,): (Option<String>,) = sqlx::query_as("SELECT trash_id FROM nodes WHERE id = ?").bind(&node.id).fetch_one(&mut *tx).await?;
        sqlx::query("UPDATE nodes SET parent_id = ?, name = ?, trashed_at = NULL, trash_root = 0, trash_id = NULL, trashed_by = NULL WHERE id = ?")
            .bind(&parent_id)
            .bind(&name)
            .bind(&node.id)
            .execute(&mut *tx)
            .await?;
        // Everything that went to the trash with it comes back; a large folder a batch at a time (tree/changes.rs)
        if let Some(trash_id) = trash_id {
            locks.later(changes::restore(&mut tx, &node.id, &trash_id).await?);
        }
        tree::touch(&mut tx, &parent_id).await?;
        logs::record_activity(&mut tx, user, Some(&node), "restore", "").await?;
    }
    locks.commit(tx).await?;
    Ok(merging)
}

/// Deletes items in the trash for good. They leave the trash at once; deleting a large folder takes a while, so it runs
/// as a job the page follows, a batch per transaction (tree/changes.rs)
pub async fn delete_forever(State(st): State<AppState>, user: User, Json(req): Json<BatchReq>) -> AppResult<Json<Job>> {
    let pending = jobs::reserve(&st, user.id, "delete", Limit::Changes)?;
    let rows = {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let mut rows = Vec::new();
        // An item put in the trash on its own stays when the folder it was in goes (tree/changes.rs): one selected
        // with that folder is deleted itself, as are the outermost of the others
        let ids = req.ids()?;
        let mut selected = outermost(&mut tx, &ids).await?;
        let own: Vec<(String,)> = sqlx::query_as("SELECT id FROM nodes WHERE id IN (SELECT value FROM json_each(?)) AND trash_root = 1")
            .bind(serde_json::to_string(&ids).unwrap())
            .fetch_all(&mut *tx)
            .await?;
        for (id,) in own {
            if !selected.contains(&id) {
                selected.push(id);
            }
        }
        for id in &selected {
            let (node, _) = trash_root(&mut tx, &user, id, Need::Delete).await?;
            logs::record_activity(&mut tx, &user, Some(&node), "delete", "").await?;
            rows.push(changes::purge(&mut tx, &node.id).await?);
        }
        tx.commit().await?;
        rows
    };
    Ok(Json(purge_as_job(&st, pending, rows, None).await?))
}

/// Deletes what `rows` started deleting, as the job `pending`; its result is `result`
pub(super) async fn purge_as_job(st: &AppState, pending: jobs::Pending, rows: Vec<changes::Unfinished>, result: Option<Value>) -> AppResult<Job> {
    let st = st.clone();
    pending
        .run(jobs::wait(), move |t| async move {
            t.set_total(changes::items_left(&st, &rows).await?);
            changes::run(&st, rows, &t).await?;
            Ok(Outcome { result, ..Default::default() })
        })
        .await
}

/// The spaces whose trash Empty trash deletes: those the user manages (or owns), except read-only spaces. The trash
/// also lists items of spaces the user can only view, which stay.
pub(super) async fn empty_trash_drives(conn: &mut SqliteConnection, user: &User) -> AppResult<Vec<String>> {
    if !(user.can_delete || user.is_admin()) {
        return Ok(Vec::new());
    }
    let ids = trash_drives(conn, user, Role::Manager).await?;
    let writable: Vec<(String,)> = sqlx::query_as("SELECT id FROM drives WHERE id IN (SELECT value FROM json_each(?)) AND read_only = 0 AND moving = 0")
        .bind(serde_json::to_string(&ids).unwrap())
        .fetch_all(conn)
        .await?;
    Ok(writable.into_iter().map(|(id,)| id).collect())
}

#[derive(Serialize)]
pub struct EmptyTrashSpace {
    pub(super) kind: String,
    pub(super) name: String,
    pub(super) items: i64,
}

/// What Empty trash would delete: the number of items per space, so the page can ask about exactly that
pub async fn empty_trash_preview(State(st): State<AppState>, user: User) -> AppResult<Json<Vec<EmptyTrashSpace>>> {
    let mut c = st.db.acquire().await?;
    let drive_ids = empty_trash_drives(&mut c, &user).await?;
    let rows: Vec<(String, String, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT d.kind, d.name, COUNT(*) FROM nodes n JOIN drives d ON d.id = n.drive_id
         WHERE n.trash_root = 1 AND n.drive_id IN (SELECT value FROM json_each(?)) AND {NOT_PURGING} AND {}
         GROUP BY d.id ORDER BY CASE d.kind WHEN 'personal' THEN 0 WHEN 'company' THEN 1 ELSE 2 END, d.name",
        trash_visible(&user)
    )))
    .bind(serde_json::to_string(&drive_ids).unwrap())
    .fetch_all(&mut *c)
    .await?;
    Ok(Json(rows.into_iter().map(|(kind, name, items)| EmptyTrashSpace { kind, name, items }).collect()))
}

/// Empty trash: spaces where I'm a manager (or owner). Everything leaves the trash at once, and is deleted as a job
/// the page follows; its result is how many items were in the trash (`deleted`)
pub async fn empty_trash(State(st): State<AppState>, user: User) -> AppResult<Json<Job>> {
    let pending = jobs::reserve(&st, user.id, "empty_trash", Limit::Changes)?;
    let rows = {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let current = crate::auth::current_user(&mut tx, &user).await?;
        let user = current;
        let drive_ids = empty_trash_drives(&mut tx, &user).await?;
        let select = format!(
            "SELECT id FROM nodes n WHERE n.trash_root = 1 AND n.drive_id IN (SELECT value FROM json_each(?1)) AND {NOT_PURGING} AND {}",
            trash_visible(&user)
        );
        let selected: Vec<(String,)> =
            sqlx::query_as(sqlx::AssertSqlSafe(select.as_str())).bind(serde_json::to_string(&drive_ids).unwrap()).fetch_all(&mut *tx).await?;
        for (id,) in selected {
            let node = tree::get_node(&mut tx, &id).await?.ok_or_else(|| AppError::conflict("The trash changed"))?;
            tree::require_subtree(&mut tx, &user, &node, Need::Delete).await?;
        }
        let rows = changes::purge_selected(&mut tx, &select, &serde_json::to_string(&drive_ids).unwrap()).await?;
        let total = rows.len();
        if total > 0 {
            logs::record_activity(&mut tx, &user, None, "empty_trash", &format!("{total} {}", if total == 1 { "item" } else { "items" })).await?;
        }
        tx.commit().await?;
        rows
    };
    let deleted = json!({ "deleted": rows.len() });
    Ok(Json(purge_as_job(&st, pending, rows, Some(deleted)).await?))
}

/// Automatically purges trash items older than the retention period; returns how many there were
pub async fn purge_expired_trash(st: &AppState, days: i64) -> AppResult<usize> {
    let cutoff = now() - days * 86400;
    let rows = {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let select = format!("SELECT id FROM nodes n WHERE n.trash_root = 1 AND n.trashed_at < ?1 AND {NOT_PURGING}");
        let rows = changes::purge_selected(&mut tx, &select, &cutoff.to_string()).await?;
        tx.commit().await?;
        rows
    };
    let total = rows.len();
    changes::run(st, rows, &Default::default()).await?;
    Ok(total)
}
