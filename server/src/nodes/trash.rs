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
    jobs::{self, Job, Limit, Outcome},
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
    let (limit, after) = page_of::<TrashCursor>(q.limit, q.after.as_deref())?;
    let drive_ids = trash_drives(&mut *st.db.acquire().await?, &user, Role::Viewer).await?;
    let keyset = if after.is_some() { "AND (n.trashed_at < ?2 OR (n.trashed_at = ?2 AND n.id < ?3))" } else { "" };
    // The filter is one more condition on the same order, so paging works the same with it
    let mine = if q.mine == Some(true) { "AND n.trashed_by = ?5" } else { "" };
    let sql = format!(
        "SELECT {NODE_COLS}, (SELECT username FROM users WHERE id = n.trashed_by) AS deleted_by FROM nodes n
         WHERE n.trash_root = 1 AND n.drive_id IN (SELECT value FROM json_each(?1)) AND {NOT_PURGING} {keyset} {mine}
         ORDER BY n.trashed_at DESC, n.id DESC LIMIT ?4"
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

/// A trashed item and where it goes back to (`Place`). The user needs `need` on the folder there as well: a folder
/// that was merely shared with them gives no rights over the owner's parent folder, so they can't restore or destroy
/// the shared folder itself, only items inside it.
pub(super) async fn trash_root(conn: &mut SqliteConnection, user: &User, id: &str, need: Need) -> AppResult<(Node, Place)> {
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
            folder = f.id;
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

/// Merges the folder `from`, in the trash, into the folder `into` of the same name, as Windows does: what went to the
/// trash with `from` goes into `into`, out of the trash (the folders it holds merged the same way into folders of the
/// same name), then `from` itself goes. A file whose name `into` has already takes the place of the one there, which
/// goes to the trash (`replace`), or gets a number. Items put in the trash on their own from inside `from` go back into
/// `into` when they are restored. What is inside the items that come out is left to the caller (`changes::restore`).
fn merge<'a>(
    conn: &'a mut SqliteConnection,
    user: &'a User,
    locks: &'a fsops::SpaceLocks,
    from: &'a Node,
    into: &'a Node,
    replace: bool,
) -> Pin<Box<dyn Future<Output = AppResult<()>> + Send + 'a>> {
    Box::pin(async move {
        let sql = format!("SELECT {NODE_COLS} FROM nodes n WHERE n.parent_id = ? AND n.trash_root = 0");
        let items: Vec<Node> = sqlx::query_as(sqlx::AssertSqlSafe(sql)).bind(&from.id).fetch_all(&mut *conn).await?;
        for item in &items {
            let name = match tree::find_child(conn, &into.id, &item.name).await? {
                Some(there) if there.is_folder() && item.is_folder() => {
                    merge(conn, user, locks, item, &there, replace).await?;
                    continue;
                }
                Some(there) if replace => {
                    replace_existing(conn, user, locks, &there, item).await?;
                    item.name.clone()
                }
                Some(_) if into.in_folder_space() => fsops::free_name(conn, into, &item.name, item.is_folder()).await?,
                Some(_) => tree::unique_name(conn, &into.id, &item.name, item.is_folder()).await?,
                None => item.name.clone(),
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
        }
        sqlx::query("UPDATE nodes SET parent_id = ? WHERE parent_id = ?").bind(&into.id).bind(&from.id).execute(&mut *conn).await?;
        // Empty now (in a folder space, its folder in the trash goes with the trash folder, `fsops::clean_trash`)
        sqlx::query("DELETE FROM nodes WHERE id = ?").bind(&from.id).execute(&mut *conn).await?;
        Ok(())
    })
}

pub async fn restore(State(st): State<AppState>, user: User, Json(req): Json<BatchReq>) -> AppResult<Json<Value>> {
    let ids = req.ids()?;
    let locks = fsops::lock_seen(&st, &user, &ids.iter().map(String::as_str).collect::<Vec<_>>()).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    for id in &outer_first(&mut tx, ids).await? {
        // Restored into the original folder, made again (with the folders above it) if it is in the trash too
        let (node, place) = trash_root(&mut tx, &user, id, Need::Move).await?;
        locks.check(&node)?;
        changes::refuse_busy(&mut tx, &node.id).await?;
        let parent_id = make_way(&mut tx, &user, &locks, &place).await?;
        // Without an answer the item gets a number when its name is taken, as it always did
        if let Some(existing) = tree::find_child(&mut tx, &parent_id, &node.name).await? {
            // A folder where a folder has its name now is merged into it, whatever the answer but Skip
            if node.is_folder() && existing.is_folder() {
                let resolution = req.resolution(&node.id);
                if resolution == Some(Resolution::Skip) {
                    continue;
                }
                let into = tree::node_for(&mut tx, &user, &existing.id, Need::Add).await?;
                locks.check(&into)?;
                changes::refuse_busy(&mut tx, &into.id).await?;
                let (trash_id,): (Option<String>,) = sqlx::query_as("SELECT trash_id FROM nodes WHERE id = ?").bind(&node.id).fetch_one(&mut *tx).await?;
                logs::record_activity(&mut tx, &user, Some(&node), "restore", "").await?;
                merge(&mut tx, &user, &locks, &node, &into, resolution == Some(Resolution::Replace)).await?;
                // What is inside the items that came out follows, as in a restore
                if let Some(trash_id) = trash_id {
                    locks.later(changes::restore(&mut tx, &into.id, &trash_id).await?);
                }
                tree::touch(&mut tx, &into.id).await?;
                continue;
            }
            match req.resolution(&node.id) {
                Some(Resolution::Skip) => continue,
                Some(Resolution::Replace) => replace_existing(&mut tx, &user, &locks, &existing, &node).await?,
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
        logs::record_activity(&mut tx, &user, Some(&node), "restore", "").await?;
    }
    tx.commit().await?;
    locks.committed();
    Ok(Json(json!({ "ok": true })))
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
         WHERE n.trash_root = 1 AND n.drive_id IN (SELECT value FROM json_each(?)) AND {NOT_PURGING}
         GROUP BY d.id ORDER BY CASE d.kind WHEN 'personal' THEN 0 WHEN 'company' THEN 1 ELSE 2 END, d.name"
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
    let drive_ids = empty_trash_drives(&mut *st.db.acquire().await?, &user).await?;
    let rows = {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let select = format!("SELECT id FROM nodes n WHERE n.trash_root = 1 AND n.drive_id IN (SELECT value FROM json_each(?1)) AND {NOT_PURGING}");
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
