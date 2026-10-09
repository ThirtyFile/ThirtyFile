//! Creating folders, renaming, moving, copying and putting items in the trash

use super::*;

#[derive(Deserialize)]
pub struct CreateFolderReq {
    pub(super) parent_id: String,
    pub(super) name: String,
}

pub async fn create_folder(State(st): State<AppState>, user: User, Json(req): Json<CreateFolderReq>) -> AppResult<Json<Node>> {
    let name = validate_name(&req.name)?;
    let locks = fsops::lock(&st, &user, &[&req.parent_id]).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let parent = tree::folder_for(&mut tx, &user, &req.parent_id, Need::Write).await?;
    locks.check(&parent)?;
    if tree::name_taken(&mut tx, &parent.id, &name).await? {
        return Err(AppError::conflict(format!("\"{name}\" already exists")));
    }
    let id = crate::content::create_folder(&mut tx, user.id, &parent.id, &name).await?;
    let node = tree::get_node(&mut tx, &id).await?.unwrap();
    // On a disk that ignores letter case, the folder is there already by a name in other case
    if node.name != name {
        return Err(AppError::conflict(format!("\"{}\" already exists", node.name)));
    }
    logs::record_activity(&mut tx, &user, Some(&node), "create_folder", "").await?;
    tx.commit().await?;
    Ok(Json(node))
}

#[derive(Deserialize)]
pub struct RenameReq {
    pub(super) name: String,
}

pub async fn rename(State(st): State<AppState>, user: User, Path(id): Path<String>, Json(req): Json<RenameReq>) -> AppResult<Json<Node>> {
    let name = validate_name(&req.name)?;
    let locks = fsops::lock(&st, &user, &[&id]).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let node = tree::node_for(&mut tx, &user, &id, Need::Write).await?;
    locks.check(&node)?;
    let parent = node.parent_id.clone().ok_or_else(|| AppError::bad_request("The root folder of a space can't be renamed"))?;
    if name == node.name {
        return Ok(Json(node));
    }
    // Changing only the letter case isn't a conflict (in a folder space, other letter case is another name). The same
    // rule as the names' unique key (`unicode_lower`), so "été" can become "Été".
    let case_only = !node.in_folder_space() && name.to_lowercase() == node.name.to_lowercase();
    if !case_only && tree::name_taken(&mut tx, &parent, &name).await? {
        return Err(AppError::conflict(format!("\"{name}\" already exists")));
    }
    if node.in_folder_space() {
        let folder = tree::get_node(&mut tx, &parent).await?.ok_or_else(|| AppError::not_found("Folder not found"))?;
        fsops::rename(&mut tx, &locks, &node, &folder, &name).await?;
    }
    let mime = if node.is_folder() { String::new() } else { crate::util::guess_mime(&name) };
    // The time only moves forward: it is the version the editors send back to detect changes by someone else, and
    // saving several times a second can put it slightly ahead of the clock
    sqlx::query("UPDATE nodes SET name = ?, mime = ?, updated_at = MAX(?, updated_at + 1) WHERE id = ?")
        .bind(&name)
        .bind(mime)
        .bind(now())
        .bind(&node.id)
        .execute(&mut *tx)
        .await?;
    logs::record_activity(&mut tx, &user, Some(&node), "rename", &format!("→ {name}")).await?;
    let node = tree::get_node(&mut tx, &node.id).await?.unwrap();
    tx.commit().await?;
    locks.committed();
    Ok(Json(node))
}

#[derive(Deserialize, Default)]
pub struct BatchReq {
    pub(super) ids: Vec<String>,
    pub(super) dest_id: Option<String>,
    /// What to do with each item (by id) whose name the destination already has; the browser asks first (`conflicts`)
    #[serde(default)]
    pub(super) resolutions: HashMap<String, Resolution>,
}

/// The answer to "the destination already has an item with this name"
#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Resolution {
    /// The item already there goes to the trash, and this one takes its place
    Replace,
    /// This item is left where it is
    Skip,
    /// Both stay: this one gets a number ("Report (1).docx")
    Keep,
}

impl BatchReq {
    pub(super) fn resolution(&self, id: &str) -> Option<Resolution> {
        self.resolutions.get(id).copied()
    }
    /// The selected ids, each once
    pub(super) fn ids(&self) -> AppResult<Vec<String>> {
        if self.ids.is_empty() || self.ids.len() > MAX_BATCH {
            return Err(AppError::bad_request("Select 1 to 1000 items"));
        }
        let mut seen = HashSet::new();
        Ok(self.ids.iter().filter(|id| seen.insert(id.as_str())).cloned().collect())
    }
    pub(super) fn dest(&self) -> AppResult<&str> {
        self.dest_id.as_deref().ok_or_else(|| AppError::bad_request("Select a destination folder"))
    }
}

/// The selected ids without duplicates and without items inside other selected items: those are trashed, deleted or
/// restored together with the folder around them, and handling them again on their own would fail.
pub async fn outermost(conn: &mut SqliteConnection, ids: &[String]) -> AppResult<Vec<String>> {
    let list = serde_json::to_string(ids).unwrap();
    let nested: Vec<(String,)> = sqlx::query_as(
        "WITH RECURSIVE up(start, id) AS (
           SELECT s.value, n.parent_id FROM json_each(?1) s JOIN nodes n ON n.id = s.value
           UNION ALL
           SELECT up.start, n.parent_id FROM up JOIN nodes n ON n.id = up.id
         )
         SELECT DISTINCT start FROM up WHERE id IN (SELECT value FROM json_each(?1))",
    )
    .bind(list)
    .fetch_all(&mut *conn)
    .await?;
    let nested: HashSet<String> = nested.into_iter().map(|(id,)| id).collect();
    let mut seen = HashSet::new();
    Ok(ids.iter().filter(|id| !nested.contains(*id) && seen.insert(id.as_str())).cloned().collect())
}

pub(super) fn not_root(n: &Node) -> AppResult<()> {
    if n.parent_id.is_none() { Err(AppError::bad_request("This can't be done on the root folder of a space")) } else { Ok(()) }
}

/// Moves items. Moving to or from a folder space copies (or renames) content, which can take long: that part runs as a
/// job the page follows (jobs.rs)
pub async fn move_nodes(State(st): State<AppState>, user: User, Json(req): Json<BatchReq>) -> AppResult<Json<Job>> {
    let pending = jobs::reserve(&st, user.id, "move", Limit::Changes)?;
    let across = move_items(&st, &user, &req).await?;
    Ok(Json(run_across(&st, &user, pending, across).await?))
}

/// Copies items; as for moves, copies to or from a folder space run as a job
pub async fn copy_nodes(State(st): State<AppState>, user: User, Json(req): Json<BatchReq>) -> AppResult<Json<Job>> {
    let pending = jobs::reserve(&st, user.id, "copy", Limit::Changes)?;
    let across = copy_items(&st, &user, &req).await?;
    Ok(Json(run_across(&st, &user, pending, across).await?))
}

pub(super) async fn run_across(st: &AppState, user: &User, pending: jobs::Pending, across: Option<fsops::Across>) -> AppResult<Job> {
    let Some(across) = across else { return Ok(Job::done(pending.kind())) };
    let (st, user) = (st.clone(), user.clone());
    pending.run(jobs::wait(), move |t| async move { run_content(&st, &user, across, &t).await.map(|()| Outcome::default()) }).await
}

/// Moves or copies the content of items going to or from a folder space (`Across`). Should that fail, the items they
/// were to replace come back from the trash, unless something has their name now: the person asked for a replace,
/// not a delete.
pub async fn run_content(st: &AppState, user: &User, across: fsops::Across, progress: &crate::jobs::Tracker) -> AppResult<()> {
    let (dest, replaced) = (across.dest().id.clone(), across.replaced().to_vec());
    let result = across.run(st, user, progress).await;
    if result.is_err() {
        for id in replaced {
            if let Err(e) = put_back_replaced(st, user, &dest, &id).await {
                tracing::warn!("Couldn't take an item that was to be replaced out of the trash again: {}", e.message);
            }
        }
    }
    result
}

/// Takes `id` out of the trash again, back into `dest`, if it is still there as the replace left it and its name is free
async fn put_back_replaced(st: &AppState, user: &User, dest: &str, id: &str) -> AppResult<()> {
    let mut c = st.db.acquire().await?;
    let Some(node) = tree::get_node(&mut c, id).await? else { return Ok(()) };
    if node.trashed_at.is_none() || node.parent_id.as_deref() != Some(dest) || tree::find_child(&mut c, dest, &node.name).await?.is_some() {
        return Ok(());
    }
    drop(c);
    let req = BatchReq { ids: vec![id.to_string()], dest_id: None, resolutions: HashMap::new() };
    super::restore(State(st.clone()), user.clone(), Json(req)).await.map(|_| ())
}

/// Moves items in the index; what goes to or from a folder space is returned, for its content to be moved next
pub async fn move_items(st: &AppState, user: &User, req: &BatchReq) -> AppResult<Option<fsops::Across>> {
    let (st, user) = (st.clone(), user.clone());
    let ids = req.ids()?;
    let locks = fsops::lock(&st, &user, &locked_ids(req.dest()?, &ids)).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let dest = tree::folder_for(&mut tx, &user, req.dest()?, Need::Write).await?;
    locks.check(&dest)?;
    // Moves between spaces that involve a folder space copy content: done after this transaction, item by item
    let mut across = Vec::new();
    let mut replaced = Vec::new();
    let mut across_items = 0usize;
    for id in &ids {
        let mut node = tree::node_for(&mut tx, &user, id, Need::Write).await?;
        changes::refuse_busy(&mut tx, &node.id).await?;
        locks.check(&node)?;
        not_root(&node)?;
        if node.parent_id.as_deref() == Some(dest.id.as_str()) {
            continue;
        }
        if tree::is_within(&mut tx, &dest.id, &node.id).await? {
            return Err(AppError::bad_request(format!("Can't move \"{}\" into its own subfolder", node.name)));
        }
        // The name the item has in the destination
        let mut name = node.name.clone();
        let mut replacing = None;
        if let Some(existing) = tree::find_child(&mut tx, &dest.id, &node.name).await? {
            match req.resolution(&node.id) {
                None => return Err(AppError::conflict(format!("The destination folder already contains \"{}\"", node.name))),
                Some(Resolution::Skip) => continue,
                Some(Resolution::Keep) if dest.in_folder_space() => name = fsops::free_name(&mut tx, &dest, &node.name, node.is_folder()).await?,
                Some(Resolution::Keep) => name = tree::unique_name(&mut tx, &dest.id, &node.name, node.is_folder()).await?,
                Some(Resolution::Replace) => {
                    replace_existing(&mut tx, &user, &locks, &existing, &node).await?;
                    replacing = Some(existing.id);
                }
            }
        }
        if node.drive_id != dest.drive_id {
            // Moving out of the original space is like deleting from it: the user must be a member of that space (with delete permission on its root);
            // edit permission obtained only through a folder share can't move someone else's folder into your own space
            let src_drive = tree::get_drive(&mut tx, node.drive()).await?.ok_or_else(|| AppError::not_found("Source space not found"))?;
            let src_root = tree::get_node(&mut tx, &src_drive.root_id).await?.ok_or_else(|| AppError::not_found("Source space not found"))?;
            let member_role = tree::role_on(&mut tx, &user, &src_root).await?;
            if member_role.is_none_or(|r| tree::allows(&user, r, Need::Delete).is_err()) {
                return Err(AppError::forbidden(format!("\"{}\" was shared with you and can't be moved to another space. Use \"Copy\" instead.", node.name)));
            }
            // Cross-space move: move the whole subtree to the target space and check the target space's quota (added up
            // in the database: this holds the write lock, and the subtree may be large)
            let (bytes, live) = tree::subtree_totals(&mut tx, &node.id).await?;
            tree::check_quota(&mut tx, dest.drive(), bytes).await?;
            if node.in_folder_space() || dest.in_folder_space() {
                across_items += live as usize;
                if across_items > MAX_COPY_ITEMS {
                    return Err(AppError::bad_request("Move at most 20,000 items at once to or from a folder on the server"));
                }
                let subtree = tree::subtree(&mut tx, &node.id).await?;
                let mut nodes = live_items(subtree);
                // The first one is the item itself: it goes in under its name in the destination
                nodes[0].name = name;
                across.push(nodes);
                replaced.extend(replacing);
                continue;
            }
            sqlx::query(
                "WITH RECURSIVE sub(id) AS (SELECT ?1 UNION ALL SELECT c.id FROM nodes c JOIN sub ON c.parent_id = sub.id)
                 UPDATE nodes SET drive_id = ?2 WHERE id IN (SELECT id FROM sub)",
            )
            .bind(&node.id)
            .bind(&dest.drive_id)
            .execute(&mut *tx)
            .await?;
            tree::adjust_usage(&mut tx, node.drive(), -bytes).await?;
            tree::adjust_usage(&mut tx, dest.drive(), bytes).await?;
        } else if node.in_folder_space() {
            fsops::rename(&mut tx, &locks, &node, &dest, &name).await?;
        }
        sqlx::query("UPDATE nodes SET parent_id = ?, name = ? WHERE id = ?").bind(&dest.id).bind(&name).bind(&node.id).execute(&mut *tx).await?;
        node.name = name;
        tree::touch(&mut tx, node.parent_id.as_deref().unwrap()).await?;
        let to = tree::place_name(&mut tx, &dest).await?;
        logs::record_activity(&mut tx, &user, Some(&node), "move", &format!("→ {to}")).await?;
    }
    tree::touch(&mut tx, &dest.id).await?;
    tx.commit().await?;
    locks.committed();
    Ok(fsops::Across::new(dest, across, true).map(|a| a.replacing(replaced)))
}

/// The items of a subtree (sorted by depth, the item itself first) that aren't in the trash, nor inside a folder that
/// is: what is in a folder that went to the trash goes with it, even before the change has marked all of it
pub(super) fn live_items(subtree: Vec<(Node, i64)>) -> Vec<Node> {
    let mut kept: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(subtree.len());
    for (i, (n, _)) in subtree.into_iter().enumerate() {
        if n.trashed_at.is_some() || (i > 0 && !n.parent_id.as_ref().is_some_and(|p| kept.contains(p))) {
            continue;
        }
        kept.insert(n.id.clone());
        out.push(n);
    }
    out
}

/// The items a move or copy touches, for locking their folder spaces
pub(super) fn locked_ids<'a>(dest: &'a str, ids: &'a [String]) -> Vec<&'a str> {
    std::iter::once(dest).chain(ids.iter().map(String::as_str)).collect()
}

/// Copies items in the index; what goes to or from a folder space is returned, for its content to be copied next
pub async fn copy_items(st: &AppState, user: &User, req: &BatchReq) -> AppResult<Option<fsops::Across>> {
    let (st, user) = (st.clone(), user.clone());
    let ids = req.ids()?;
    let locks = fsops::lock(&st, &user, &locked_ids(req.dest()?, &ids)).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let dest = tree::folder_for(&mut tx, &user, req.dest()?, Need::Write).await?;
    locks.check(&dest)?;
    let mut plans = Vec::new();
    let mut replaced = Vec::new();
    let mut total = 0i64;
    let mut items = 0usize;
    for id in &ids {
        let node = tree::node_for(&mut tx, &user, id, Need::Read).await?;
        changes::refuse_busy(&mut tx, &node.id).await?;
        locks.check(&node)?;
        not_root(&node)?;
        if tree::is_within(&mut tx, &dest.id, &node.id).await? {
            return Err(AppError::bad_request(format!("Can't copy \"{}\" into its own subfolder", node.name)));
        }
        // Without an answer the copy gets a number, as it always did; a copy into its own folder always does
        if node.parent_id.as_deref() != Some(dest.id.as_str())
            && let Some(existing) = tree::find_child(&mut tx, &dest.id, &node.name).await?
        {
            match req.resolution(&node.id) {
                Some(Resolution::Skip) => continue,
                Some(Resolution::Replace) => {
                    replace_existing(&mut tx, &user, &locks, &existing, &node).await?;
                    if node.in_folder_space() || dest.in_folder_space() {
                        replaced.push(existing.id);
                    }
                }
                Some(Resolution::Keep) | None => {}
            }
        }
        // Copies run in one transaction while every other change waits: keep each one to a bounded size (counted before
        // the items are read, so a very large folder isn't read to be refused)
        items += tree::subtree_totals(&mut tx, &node.id).await?.1 as usize;
        if items > MAX_COPY_ITEMS {
            return Err(AppError::bad_request("Copy at most 20,000 items at once"));
        }
        let nodes = live_items(tree::subtree(&mut tx, &node.id).await?);
        total += nodes.iter().map(|n| n.size).sum::<i64>();
        plans.push(nodes);
    }
    tree::check_quota(&mut tx, dest.drive(), total).await?;

    // Copies to or from a folder space copy content: done after this transaction, item by item
    let (across, plans): (Vec<Vec<Node>>, Vec<Vec<Node>>) = plans.into_iter().partition(|nodes| nodes[0].in_folder_space() || dest.in_folder_space());
    let total: i64 = plans.iter().flatten().map(|n| n.size).sum();
    let ts = now();
    for nodes in plans {
        let mut ids: HashMap<String, String> = HashMap::new();
        let mut rows = Vec::with_capacity(nodes.len());
        let mut blobs = Vec::new();
        for (i, n) in nodes.iter().enumerate() {
            let new_id = new_id();
            let (parent, name) = if i == 0 {
                (dest.id.clone(), tree::unique_name(&mut tx, &dest.id, &n.name, n.is_folder()).await?)
            } else {
                // The subtree is sorted by depth, so parents have always been copied already (`live_items`)
                let Some(parent) = n.parent_id.as_ref().and_then(|p| ids.get(p)) else { continue };
                (parent.clone(), n.name.clone())
            };
            rows.push(json!([new_id, parent, n.kind, name, n.blob_hash, n.size, n.mime]));
            if let Some(hash) = &n.blob_hash {
                blobs.push((hash.clone(), n.size, n.blob_location.clone().unwrap_or_else(|| "local".into())));
            }
            ids.insert(n.id.clone(), new_id);
        }
        // One statement for the whole subtree, in depth order (parents are inserted before their children)
        sqlx::query(
            "INSERT INTO nodes (id, owner_id, parent_id, kind, name, blob_hash, size, mime, drive_id, created_at, updated_at)
             SELECT json_extract(value, '$[0]'), ?2, json_extract(value, '$[1]'), json_extract(value, '$[2]'),
                    json_extract(value, '$[3]'), json_extract(value, '$[4]'), json_extract(value, '$[5]'),
                    json_extract(value, '$[6]'), ?3, ?4, ?4
             FROM json_each(?1) ORDER BY key",
        )
        .bind(serde_json::to_string(&rows).unwrap())
        .bind(user.id)
        .bind(&dest.drive_id)
        .bind(ts)
        .execute(&mut *tx)
        .await?;
        tree::add_blob_refs(&mut tx, &blobs).await?;
        // The copies get the copier's tags of the originals (each person's tags are their own)
        crate::tags::copy_tags(&mut tx, user.id, ids.iter().map(|(from, to)| (from.as_str(), to.as_str()))).await?;
        let to = tree::place_name(&mut tx, &dest).await?;
        logs::record_activity(&mut tx, &user, Some(&nodes[0]), "copy", &format!("→ {to}")).await?;
    }
    tree::adjust_usage(&mut tx, dest.drive(), total).await?;
    tree::touch(&mut tx, &dest.id).await?;
    tx.commit().await?;
    locks.committed();
    Ok(fsops::Across::new(dest, across, false).map(|a| a.replacing(replaced)))
}

pub async fn trash(State(st): State<AppState>, user: User, Json(req): Json<BatchReq>) -> AppResult<Json<Value>> {
    let ids = req.ids()?;
    let locks = fsops::lock(&st, &user, &ids.iter().map(String::as_str).collect::<Vec<_>>()).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    for id in &outermost(&mut tx, &ids).await? {
        let node = tree::node_for(&mut tx, &user, id, Need::Delete).await?;
        locks.check(&node)?;
        trash_one(&mut tx, &user, &locks, &node, "").await?;
    }
    tx.commit().await?;
    locks.committed();
    Ok(Json(json!({ "ok": true })))
}

/// Moves an item (the user may delete it) and everything in it to the trash; `detail` goes into the activity log
pub(super) async fn trash_one(conn: &mut SqliteConnection, user: &User, locks: &fsops::SpaceLocks, node: &Node, detail: &str) -> AppResult<()> {
    not_root(node)?;
    changes::refuse_busy(conn, &node.id).await?;
    let (trash_id, at) = (new_id(), now());
    if node.in_folder_space() {
        // Into the space's trash folder on disk, so it can be restored
        fsops::trash(conn, locks, node, &trash_id).await?;
    }
    sqlx::query("UPDATE nodes SET trashed_at = ?, trash_id = ?, trash_root = 1, trashed_by = ? WHERE id = ?")
        .bind(at)
        .bind(&trash_id)
        .bind(user.id)
        .bind(&node.id)
        .execute(&mut *conn)
        .await?;
    // Everything in it goes with it; a large folder a batch at a time (tree/changes.rs)
    locks.later(changes::trash(conn, &node.id, &trash_id, at).await?);
    tree::touch(conn, node.parent_id.as_deref().unwrap()).await?;
    logs::record_activity(conn, user, Some(node), "trash", detail).await?;
    Ok(())
}

/// "Replace": the item already in the destination goes to the trash so `incoming` can take its name. The user needs
/// delete permission on it, and it can't be a folder holding `incoming` itself.
pub(super) async fn replace_existing(conn: &mut SqliteConnection, user: &User, locks: &fsops::SpaceLocks, existing: &Node, incoming: &Node) -> AppResult<()> {
    if existing.id == incoming.id || tree::is_within(conn, &incoming.id, &existing.id).await? {
        return Err(AppError::conflict(format!("\"{}\" can't be replaced: it holds the item that would replace it", existing.name)));
    }
    let existing = tree::node_for(conn, user, &existing.id, Need::Delete).await?;
    locks.check(&existing)?;
    trash_one(conn, user, locks, &existing, "Replaced").await
}

#[derive(Deserialize)]
pub struct ConflictsReq {
    /// The destination folder; without it, the items are in the trash and go back where they came from
    pub(super) dest_id: Option<String>,
    /// Names of items about to be uploaded
    #[serde(default)]
    pub(super) names: Vec<String>,
    /// Items about to be moved, copied (with a destination) or restored (without one)
    #[serde(default)]
    pub(super) ids: Vec<String>,
}

#[derive(Serialize)]
pub struct Conflict {
    /// The item being moved, copied or restored (none for uploads)
    pub(super) id: Option<String>,
    /// Its name (for uploads, the name asked about)
    pub(super) name: String,
    pub(super) kind: Option<String>,
    pub(super) size: Option<i64>,
    pub(super) updated_at: Option<i64>,
    /// The item with the same name already there
    pub(super) existing: Node,
}

/// Uploaded names checked at once (the top-level items of a drop)
pub(super) const MAX_CONFLICT_NAMES: usize = 10_000;

/// Which of these would land on a name the destination already has, so the browser can ask whether to replace, skip
/// or keep both before it starts
pub async fn conflicts(State(st): State<AppState>, user: User, Json(req): Json<ConflictsReq>) -> AppResult<Json<Vec<Conflict>>> {
    if req.names.len() > MAX_CONFLICT_NAMES || req.ids.len() > MAX_BATCH {
        return Err(AppError::bad_request("Too many items at once"));
    }
    let mut c = st.db.acquire().await?;
    let mut out = Vec::new();
    if let Some(dest) = &req.dest_id {
        let dest = tree::folder_for(&mut c, &user, dest, Need::Read).await?;
        for (name, existing) in tree::find_children(&mut c, &dest.id, &req.names).await? {
            out.push(Conflict { id: None, name, kind: None, size: None, updated_at: None, existing });
        }
        for id in &req.ids {
            let node = tree::node_for(&mut c, &user, id, Need::Read).await?;
            // Already there: a move changes nothing, and a copy into its own folder gets a number, as in Windows
            if node.parent_id.as_deref() == Some(dest.id.as_str()) {
                continue;
            }
            if let Some(existing) = tree::find_child(&mut c, &dest.id, &node.name).await?.filter(|e| e.id != node.id) {
                out.push(Conflict {
                    id: Some(node.id.clone()),
                    name: node.name.clone(),
                    kind: Some(node.kind.clone()),
                    size: Some(node.size),
                    updated_at: Some(node.updated_at),
                    existing,
                });
            }
        }
    } else {
        for id in &req.ids {
            let (node, parent) = trash_root(&mut c, &user, id, Need::Read).await?;
            if let Some(existing) = tree::find_child(&mut c, &parent, &node.name).await? {
                out.push(Conflict {
                    id: Some(node.id.clone()),
                    name: node.name.clone(),
                    kind: Some(node.kind.clone()),
                    size: Some(node.size),
                    updated_at: Some(node.updated_at),
                    existing,
                });
            }
        }
    }
    Ok(Json(out))
}
