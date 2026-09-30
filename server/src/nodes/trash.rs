//! The trash: listing, restoring, deleting for good and emptying it

use super::*;

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
        && drive.kind != "personal"
        && !drive.disabled
    {
        return Ok(role.max(Some(Role::Manager)));
    }
    Ok(role)
}

/// A trashed item and the folder it goes back to (its parent, or the space root when the parent was deleted too).
/// The user needs `need` on that folder as well: a folder that was merely shared with them gives no rights over the
/// owner's parent folder, so they can't restore or destroy the shared folder itself, only items inside it.
pub(super) async fn trash_root(conn: &mut SqliteConnection, user: &User, id: &str, need: Need) -> AppResult<(Node, String)> {
    let not_found = || AppError::not_found("Item not found in trash");
    let node = tree::get_node(conn, id).await?.ok_or_else(not_found)?;
    // Not one being deleted for good already
    let sql = format!("SELECT n.trash_root AND {} FROM nodes n WHERE n.id = ?", changes::NOT_PURGING);
    let (is_root,): (bool,) = sqlx::query_as(sqlx::AssertSqlSafe(sql)).bind(id).fetch_one(&mut *conn).await?;
    if !is_root {
        return Err(not_found());
    }
    let role = trash_role(conn, user, &node).await?.ok_or_else(not_found)?;
    tree::allows(user, role, need)?;
    if node.space_read_only {
        return Err(tree::read_only_error(&node));
    }
    let parent = match &node.parent_id {
        Some(p) => tree::get_node(conn, p).await?.filter(|p| p.trashed_at.is_none()),
        None => None,
    };
    let dest = match parent {
        Some(p) => p,
        None => {
            let root_id = tree::get_drive(conn, node.drive()).await?.map(|d| d.root_id).ok_or_else(not_found)?;
            tree::get_node(conn, &root_id).await?.ok_or_else(not_found)?
        }
    };
    let dest_role = trash_role(conn, user, &dest)
        .await?
        .ok_or_else(|| AppError::forbidden("You don't have permission on the folder this item belongs to"))?;
    tree::allows(user, dest_role, need)?;
    Ok((node, dest.id))
}

pub async fn restore(State(st): State<AppState>, user: User, Json(req): Json<BatchReq>) -> AppResult<Json<Value>> {
    let ids = req.ids()?;
    let locks = fsops::lock(&st, &user, &ids.iter().map(String::as_str).collect::<Vec<_>>()).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    for id in &ids {
        // Restored into the original folder, or the space's root folder if that was deleted too
        let (node, parent_id) = trash_root(&mut tx, &user, id, Need::Write).await?;
        locks.check(&node)?;
        changes::refuse_busy(&mut tx, &node.id).await?;
        // Without an answer the item gets a number when its name is taken, as it always did
        if let Some(existing) = tree::find_child(&mut tx, &parent_id, &node.name).await? {
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
        for id in &outermost(&mut tx, &req.ids()?).await? {
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
    let writable: Vec<(String,)> =
        sqlx::query_as("SELECT id FROM drives WHERE id IN (SELECT value FROM json_each(?)) AND read_only = 0 AND moving = 0")
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
