//! Browsing and organizing files: listing, folders, rename, move, copy, trash, search, favorites, shared with me.
//! Permissions are determined by space and folder grants (see tree::role_on).

use std::collections::{HashMap, HashSet};

use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqliteConnection;

use crate::{
    auth::User,
    error::{AppError, AppResult},
    fsops,
    state::AppState,
    tree::{self, Crumb, NODE_COLS, Need, Node, Role},
    util::{new_id, now, validate_name},
};

const MAX_BATCH: usize = 1000;
/// Files and folders one copy may create (counted after expanding folders)
const MAX_COPY_ITEMS: usize = 20_000;
/// First segment of the location text for items accessed through a folder share (the browser uses its own text)
const SHARED_WITH_ME: &str = "Shared with me";

#[derive(Serialize)]
pub struct DriveBrief {
    id: String,
    name: String,
    kind: String,
    root_id: String,
}

#[derive(Serialize)]
pub struct NodeInfo {
    node: Node,
    /// Path from the space root (exclusive) to this node; when accessed through a folder share, starts at the shared folder
    path: Vec<Crumb>,
    is_root: bool,
    drive: DriveBrief,
    role: Role,
    /// Accessed through a folder share (rather than as a space member)
    via_share: bool,
    /// Reason the storage location holding the content is offline (files: where the content is; folders: the space's location)
    offline: Option<String>,
    /// A read-only space: browse, download and share only
    read_only: bool,
}

/// The path visible to the user: space members see the full path; people with shared access only see from the shared folder down
async fn visible_path(conn: &mut SqliteConnection, user: &User, node: &Node) -> AppResult<(Vec<Crumb>, bool)> {
    let path = tree::path_of(conn, &node.id).await?;
    let member_of: Vec<String> = tree::user_drives(conn, user).await?.into_iter().map(|(d, _)| d.id).collect();
    if member_of.iter().any(|d| d == node.drive()) {
        return Ok((path, false));
    }
    let shared: HashSet<String> = tree::shared_with_me_outside(conn, user, &member_of).await?.into_iter().map(|(n, _, _)| n.id).collect();
    let start = path.iter().position(|c| shared.contains(&c.id)).unwrap_or(0);
    Ok((path.into_iter().skip(start).collect(), true))
}

pub async fn get(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<Json<NodeInfo>> {
    let mut c = st.db.acquire().await?;
    let (mut node, role) = tree::node_with_role(&mut c, &user, &id).await?;
    let drive = tree::get_drive(&mut c, node.drive()).await?.ok_or_else(|| AppError::not_found("Space not found"))?;
    let (path, via_share) = visible_path(&mut c, &user, &node).await?;
    tree::mark_favorites(&mut c, user.id, [&mut node]).await?;
    let is_root = node.parent_id.is_none();
    // Folder spaces are on the server itself: nothing to be offline
    let offline = if drive.is_folder() {
        None
    } else {
        let location = match node.blob() {
            Ok((_, loc)) => loc.to_string(),
            Err(_) => tree::drive_location(&st, &mut c, node.drive()).await?,
        };
        st.location_offline(&location)
    };
    let read_only = drive.read_only;
    Ok(Json(NodeInfo {
        node,
        path,
        is_root,
        drive: DriveBrief { id: drive.id, name: drive.name, kind: drive.kind, root_id: drive.root_id },
        role,
        via_share,
        offline,
        read_only,
    }))
}

#[derive(Deserialize)]
pub struct ListQuery {
    sort: Option<String>,
    order: Option<String>,
    folders_only: Option<bool>,
}

/// Sorting of folder listings, folders first. Names sort naturally ("File 2" before "File 10", the `natural_name`
/// collation), and Type sorts by extension, as the column shows it (`extOf` in the browser: after the last dot, unless
/// the name starts with it).
pub fn order_clause(sort: Option<&str>, order: Option<&str>) -> String {
    const EXT: &str = "CASE WHEN n.kind = 'file' AND length(rtrim(n.name, replace(n.name, '.', ''))) > 1
        THEN lower(substr(n.name, length(rtrim(n.name, replace(n.name, '.', ''))) + 1)) ELSE '' END";
    let col = match sort {
        Some("size") => "n.size",
        Some("updated") => "n.updated_at",
        Some("type") => EXT,
        _ => "n.name COLLATE natural_name",
    };
    let dir = if order == Some("desc") { "DESC" } else { "ASC" };
    format!("ORDER BY (n.kind = 'folder') DESC, {col} {dir}, n.name COLLATE natural_name")
}

pub async fn children(
    State(st): State<AppState>,
    user: User,
    Path(id): Path<String>,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Vec<Node>>> {
    let mut c = st.db.acquire().await?;
    let folder = tree::folder_for(&mut c, &user, &id, Need::Read).await?;
    if folder.in_folder_space() {
        // Changes made on the server's folder show up when the folder is opened
        crate::folders::sync_folder(&st, &folder).await;
    }
    let kind_filter = if q.folders_only == Some(true) { "AND n.kind = 'folder'" } else { "" };
    let sql = format!(
        "SELECT {NODE_COLS} FROM nodes n WHERE n.parent_id = ? AND n.trashed_at IS NULL {kind_filter} {}",
        order_clause(q.sort.as_deref(), q.order.as_deref())
    );
    let mut nodes: Vec<Node> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(&folder.id).fetch_all(&mut *c).await?;
    tree::mark_favorites(&mut c, user.id, &mut nodes).await?;
    Ok(Json(nodes))
}

#[derive(Deserialize)]
pub struct CreateFolderReq {
    parent_id: String,
    name: String,
}

pub async fn create_folder(State(st): State<AppState>, user: User, Json(req): Json<CreateFolderReq>) -> AppResult<Json<Node>> {
    let name = validate_name(&req.name)?;
    let locks = fsops::lock(&st, &user, &[&req.parent_id]).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    let parent = tree::folder_for(&mut tx, &user, &req.parent_id, Need::Write).await?;
    locks.check(&parent)?;
    if tree::name_taken(&mut tx, &parent.id, &name).await? {
        return Err(AppError::conflict(format!("\"{name}\" already exists")));
    }
    let id = tree::create_folder(&mut tx, user.id, &parent.id, &name).await?;
    let node = tree::get_node(&mut tx, &id).await?.unwrap();
    tree::log(&mut tx, &user, Some(&node), "create_folder", "").await?;
    tx.commit().await?;
    Ok(Json(node))
}

#[derive(Deserialize)]
pub struct RenameReq {
    name: String,
}

pub async fn rename(
    State(st): State<AppState>,
    user: User,
    Path(id): Path<String>,
    Json(req): Json<RenameReq>,
) -> AppResult<Json<Node>> {
    let name = validate_name(&req.name)?;
    let locks = fsops::lock(&st, &user, &[&id]).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    let node = tree::node_for(&mut tx, &user, &id, Need::Write).await?;
    locks.check(&node)?;
    let parent = node.parent_id.clone().ok_or_else(|| AppError::bad_request("The root folder of a space can't be renamed"))?;
    if name == node.name {
        return Ok(Json(node));
    }
    // Changing only the letter case isn't a conflict (in a folder space, other letter case is another name)
    let case_only = !node.in_folder_space() && name.eq_ignore_ascii_case(&node.name);
    if !case_only && tree::name_taken(&mut tx, &parent, &name).await? {
        return Err(AppError::conflict(format!("\"{name}\" already exists")));
    }
    if node.in_folder_space() {
        let folder = tree::get_node(&mut tx, &parent).await?.ok_or_else(|| AppError::not_found("Folder not found"))?;
        fsops::rename(&mut tx, &node, &folder, &name).await?;
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
    tree::log(&mut tx, &user, Some(&node), "rename", &format!("→ {name}")).await?;
    let node = tree::get_node(&mut tx, &node.id).await?.unwrap();
    tx.commit().await?;
    Ok(Json(node))
}

#[derive(Deserialize)]
pub struct BatchReq {
    ids: Vec<String>,
    dest_id: Option<String>,
}

impl BatchReq {
    /// The selected ids, each once
    fn ids(&self) -> AppResult<Vec<String>> {
        if self.ids.is_empty() || self.ids.len() > MAX_BATCH {
            return Err(AppError::bad_request("Select 1 to 1000 items"));
        }
        let mut seen = HashSet::new();
        Ok(self.ids.iter().filter(|id| seen.insert(id.as_str())).cloned().collect())
    }
    fn dest(&self) -> AppResult<&str> {
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

fn not_root(n: &Node) -> AppResult<()> {
    if n.parent_id.is_none() { Err(AppError::bad_request("This can't be done on the root folder of a space")) } else { Ok(()) }
}

pub async fn move_nodes(State(st): State<AppState>, user: User, Json(req): Json<BatchReq>) -> AppResult<Json<Value>> {
    let ids = req.ids()?;
    let locks = fsops::lock(&st, &user, &locked_ids(req.dest()?, &ids)).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    let dest = tree::folder_for(&mut tx, &user, req.dest()?, Need::Write).await?;
    locks.check(&dest)?;
    // Moves between spaces that involve a folder space copy content: done after this transaction, item by item
    let mut across = Vec::new();
    let mut across_items = 0usize;
    for id in &ids {
        let node = tree::node_for(&mut tx, &user, id, Need::Write).await?;
        locks.check(&node)?;
        not_root(&node)?;
        if node.parent_id.as_deref() == Some(dest.id.as_str()) {
            continue;
        }
        if tree::is_within(&mut tx, &dest.id, &node.id).await? {
            return Err(AppError::bad_request(format!("Can't move \"{}\" into its own subfolder", node.name)));
        }
        if tree::name_taken(&mut tx, &dest.id, &node.name).await? {
            return Err(AppError::conflict(format!("The destination folder already contains \"{}\"", node.name)));
        }
        if node.drive_id != dest.drive_id {
            // Moving out of the original space is like deleting from it: the user must be a member of that space (with delete permission on its root);
            // edit permission obtained only through a folder share can't move someone else's folder into your own space
            let src_drive = tree::get_drive(&mut tx, node.drive()).await?.ok_or_else(|| AppError::not_found("Source space not found"))?;
            let src_root = tree::get_node(&mut tx, &src_drive.root_id).await?.ok_or_else(|| AppError::not_found("Source space not found"))?;
            let member_role = tree::role_on(&mut tx, &user, &src_root).await?;
            if member_role.is_none_or(|r| tree::allows(&user, r, Need::Delete).is_err()) {
                return Err(AppError::forbidden(format!(
                    "\"{}\" was shared with you and can't be moved to another space. Use \"Copy\" instead.",
                    node.name
                )));
            }
            // Cross-space move: move the whole subtree to the target space and check the target space's quota
            let subtree: Vec<Node> = tree::subtree(&mut tx, &node.id).await?.into_iter().map(|(n, _)| n).collect();
            let bytes: i64 = subtree.iter().filter(|n| !n.is_folder()).map(|n| n.size).sum();
            tree::check_quota(&mut tx, dest.drive(), bytes).await?;
            if node.in_folder_space() || dest.in_folder_space() {
                let nodes: Vec<Node> = subtree.into_iter().filter(|n| n.trashed_at.is_none()).collect();
                across_items += nodes.len();
                if across_items > MAX_COPY_ITEMS {
                    return Err(AppError::bad_request("Move at most 20,000 items at once to or from a folder on the server"));
                }
                across.push(nodes);
                continue;
            }
            for n in &subtree {
                sqlx::query("UPDATE nodes SET drive_id = ? WHERE id = ?").bind(&dest.drive_id).bind(&n.id).execute(&mut *tx).await?;
            }
            tree::adjust_usage(&mut tx, node.drive(), -bytes).await?;
            tree::adjust_usage(&mut tx, dest.drive(), bytes).await?;
        } else if node.in_folder_space() {
            fsops::rename(&mut tx, &node, &dest, &node.name).await?;
        }
        sqlx::query("UPDATE nodes SET parent_id = ? WHERE id = ?").bind(&dest.id).bind(&node.id).execute(&mut *tx).await?;
        tree::touch(&mut tx, node.parent_id.as_deref().unwrap()).await?;
        tree::log(&mut tx, &user, Some(&node), "move", &format!("→ {}", if dest.parent_id.is_none() { "Root folder" } else { &dest.name })).await?;
    }
    tree::touch(&mut tx, &dest.id).await?;
    tx.commit().await?;
    drop(_w);
    fsops::move_across(&st, &user, &dest, across).await?;
    Ok(Json(json!({ "ok": true })))
}

/// The items a move or copy touches, for locking their folder spaces
fn locked_ids<'a>(dest: &'a str, ids: &'a [String]) -> Vec<&'a str> {
    std::iter::once(dest).chain(ids.iter().map(String::as_str)).collect()
}

pub async fn copy_nodes(State(st): State<AppState>, user: User, Json(req): Json<BatchReq>) -> AppResult<Json<Value>> {
    let ids = req.ids()?;
    let locks = fsops::lock(&st, &user, &locked_ids(req.dest()?, &ids)).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    let dest = tree::folder_for(&mut tx, &user, req.dest()?, Need::Write).await?;
    locks.check(&dest)?;
    let mut plans = Vec::new();
    let mut total = 0i64;
    let mut items = 0usize;
    for id in &ids {
        let node = tree::node_for(&mut tx, &user, id, Need::Read).await?;
        locks.check(&node)?;
        not_root(&node)?;
        if tree::is_within(&mut tx, &dest.id, &node.id).await? {
            return Err(AppError::bad_request(format!("Can't copy \"{}\" into its own subfolder", node.name)));
        }
        let nodes: Vec<Node> =
            tree::subtree(&mut tx, &node.id).await?.into_iter().map(|(n, _)| n).filter(|n| n.trashed_at.is_none()).collect();
        total += nodes.iter().map(|n| n.size).sum::<i64>();
        items += nodes.len();
        // Copies run in one transaction while every other change waits: keep each one to a bounded size
        if items > MAX_COPY_ITEMS {
            return Err(AppError::bad_request("Copy at most 20,000 items at once"));
        }
        plans.push(nodes);
    }
    tree::check_quota(&mut tx, dest.drive(), total).await?;

    // Copies to or from a folder space copy content: done after this transaction, item by item
    let (across, plans): (Vec<Vec<Node>>, Vec<Vec<Node>>) = plans.into_iter().partition(|nodes| nodes[0].in_folder_space() || dest.in_folder_space());
    let total: i64 = plans.iter().flatten().map(|n| n.size).sum();
    let ts = now();
    for nodes in plans {
        let mut ids: HashMap<String, String> = HashMap::new();
        for (i, n) in nodes.iter().enumerate() {
            let new_id = new_id();
            let (parent, name) = if i == 0 {
                (dest.id.clone(), tree::unique_name(&mut tx, &dest.id, &n.name, n.is_folder()).await?)
            } else {
                // The subtree is sorted by depth, so parents have always been copied already
                (ids[n.parent_id.as_ref().unwrap()].clone(), n.name.clone())
            };
            sqlx::query(
                "INSERT INTO nodes (id, owner_id, parent_id, kind, name, blob_hash, size, mime, drive_id, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&new_id)
            .bind(user.id)
            .bind(&parent)
            .bind(&n.kind)
            .bind(&name)
            .bind(&n.blob_hash)
            .bind(n.size)
            .bind(&n.mime)
            .bind(&dest.drive_id)
            .bind(ts)
            .bind(ts)
            .execute(&mut *tx)
            .await?;
            if let Some(hash) = &n.blob_hash {
                tree::add_blob_ref(&mut tx, hash, n.size, n.blob_location.as_deref().unwrap_or("local")).await?;
            }
            ids.insert(n.id.clone(), new_id);
        }
        tree::log(&mut tx, &user, Some(&nodes[0]), "copy", &format!("→ {}", if dest.parent_id.is_none() { "Root folder" } else { &dest.name })).await?;
    }
    tree::adjust_usage(&mut tx, dest.drive(), total).await?;
    tree::touch(&mut tx, &dest.id).await?;
    tx.commit().await?;
    drop(_w);
    fsops::copy_across(&st, &user, &dest, across).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn trash(State(st): State<AppState>, user: User, Json(req): Json<BatchReq>) -> AppResult<Json<Value>> {
    let ids = req.ids()?;
    let locks = fsops::lock(&st, &user, &ids.iter().map(String::as_str).collect::<Vec<_>>()).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    let ts = now();
    for id in &outermost(&mut tx, &ids).await? {
        let node = tree::node_for(&mut tx, &user, id, Need::Delete).await?;
        locks.check(&node)?;
        not_root(&node)?;
        let trash_id = new_id();
        if node.in_folder_space() {
            // Into the space's trash folder on disk, so it can be restored
            fsops::trash(&mut tx, &node, &trash_id).await?;
        }
        sqlx::query(
            "WITH RECURSIVE sub(id) AS (
               SELECT ?1 UNION ALL
               SELECT c.id FROM nodes c JOIN sub ON c.parent_id = sub.id WHERE c.trashed_at IS NULL
             )
             UPDATE nodes SET trashed_at = ?2, trash_id = ?3 WHERE id IN (SELECT id FROM sub)",
        )
        .bind(&node.id)
        .bind(ts)
        .bind(&trash_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE nodes SET trash_root = 1 WHERE id = ?").bind(&node.id).execute(&mut *tx).await?;
        tree::touch(&mut tx, node.parent_id.as_deref().unwrap()).await?;
        tree::log(&mut tx, &user, Some(&node), "trash", "").await?;
    }
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Serialize)]
pub struct Located {
    #[serde(flatten)]
    node: Node,
    /// Location, e.g. "All files/Projects/2026" (English; the browser builds its own text from the parts below)
    location: String,
    /// The space the item is in, or None when it is only reached through something shared with the user
    location_space: Option<SpaceRef>,
    /// Folders from the space root (or the shared folder) down to the item's parent
    location_path: Vec<String>,
}

#[derive(Serialize, Clone)]
pub struct SpaceRef {
    kind: String,
    name: String,
}

/// Attaches each node's location (space name + path) and marks favorites
async fn locate(st: &AppState, user: &User, mut nodes: Vec<Node>) -> AppResult<Vec<Located>> {
    let mut c = st.db.acquire().await?;
    tree::mark_favorites(&mut c, user.id, &mut nodes).await?;
    let mut drives: HashMap<String, SpaceRef> =
        tree::user_drives(&mut c, user).await?.into_iter().map(|(d, _)| (d.id, SpaceRef { kind: d.kind, name: d.name })).collect();
    if user.is_admin() && nodes.iter().any(|n| !drives.contains_key(n.drive())) {
        // Spaces an administrator manages without being a member (their trash is listed too)
        let all: Vec<(String, String, String)> =
            sqlx::query_as("SELECT id, kind, name FROM drives WHERE kind != 'personal' AND disabled = 0").fetch_all(&mut *c).await?;
        for (id, kind, name) in all {
            drives.entry(id).or_insert(SpaceRef { kind, name });
        }
    }
    let shared: HashSet<String> = if nodes.iter().any(|n| !drives.contains_key(n.drive())) {
        let member_of: Vec<String> = drives.keys().cloned().collect();
        tree::shared_with_me_outside(&mut c, user, &member_of).await?.into_iter().map(|(n, _, _)| n.id).collect()
    } else {
        HashSet::new()
    };
    // One query for the paths of all parents instead of one recursive query per row
    let parent_ids: Vec<String> = nodes.iter().filter_map(|n| n.parent_id.clone()).collect::<HashSet<_>>().into_iter().collect();
    let paths = tree::paths_of(&mut c, &parent_ids).await?;
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        let path = match &node.parent_id {
            Some(p) => paths.get(p).cloned().unwrap_or_default(),
            None => Vec::new(),
        };
        let space = drives.get(node.drive()).cloned();
        // Accessed through a folder share: only shown from the shared folder down
        let start = if space.is_some() { 0 } else { path.iter().position(|c| shared.contains(&c.id)).unwrap_or(path.len()) };
        let location_path: Vec<String> = path[start..].iter().map(|p| p.name.clone()).collect();
        let first = space.as_ref().map_or(SHARED_WITH_ME, |s| s.name.as_str());
        let location = std::iter::once(first).chain(location_path.iter().map(String::as_str)).collect::<Vec<_>>().join("/");
        out.push(Located { node, location, location_space: space, location_path });
    }
    Ok(out)
}

/// Trash: deleted items in spaces I'm a member of
/// Spaces whose trash the user works with: their own spaces (at least `min_role`), plus every team space for administrators
async fn trash_drives(conn: &mut SqliteConnection, user: &User, min_role: Role) -> AppResult<Vec<String>> {
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

pub async fn list_trash(State(st): State<AppState>, user: User) -> AppResult<Json<Vec<Located>>> {
    let drive_ids = trash_drives(&mut *st.db.acquire().await?, &user, Role::Viewer).await?;
    let sql = format!(
        "SELECT {NODE_COLS} FROM nodes n
         WHERE n.trash_root = 1 AND n.drive_id IN (SELECT value FROM json_each(?)) ORDER BY n.trashed_at DESC"
    );
    let nodes: Vec<Node> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(serde_json::to_string(&drive_ids).unwrap()).fetch_all(&st.db).await?;
    Ok(Json(locate(&st, &user, nodes).await?))
}

/// The user's role for trash operations: their role on the item, and administrators manage the trash of every space
/// that isn't personal (the same rule as managing the space itself), whether or not they were given access to it
async fn trash_role(conn: &mut SqliteConnection, user: &User, node: &Node) -> AppResult<Option<Role>> {
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
async fn trash_root(conn: &mut SqliteConnection, user: &User, id: &str, need: Need) -> AppResult<(Node, String)> {
    let not_found = || AppError::not_found("Item not found in trash");
    let node = tree::get_node(conn, id).await?.ok_or_else(not_found)?;
    let (is_root,): (bool,) = sqlx::query_as("SELECT trash_root FROM nodes WHERE id = ?").bind(id).fetch_one(&mut *conn).await?;
    if !is_root {
        return Err(not_found());
    }
    let role = trash_role(conn, user, &node).await?.ok_or_else(not_found)?;
    tree::allows(user, role, need)?;
    if node.space_read_only {
        return Err(tree::read_only_space());
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
    let mut tx = st.db.begin().await?;
    for id in &ids {
        // Restored into the original folder, or the space's root folder if that was deleted too
        let (node, parent_id) = trash_root(&mut tx, &user, id, Need::Write).await?;
        locks.check(&node)?;
        let name = if node.in_folder_space() {
            let dest = tree::get_node(&mut tx, &parent_id).await?.ok_or_else(|| AppError::not_found("Folder not found"))?;
            let name = fsops::free_name(&mut tx, &dest, &node.name, node.is_folder()).await?;
            fsops::restore(&mut tx, &node, &dest, &name).await?;
            name
        } else {
            tree::unique_name(&mut tx, &parent_id, &node.name, node.is_folder()).await?
        };
        sqlx::query("UPDATE nodes SET parent_id = ?, name = ? WHERE id = ?")
            .bind(&parent_id)
            .bind(&name)
            .bind(&node.id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE nodes SET trashed_at = NULL, trash_root = 0, trash_id = NULL
             WHERE trash_id = (SELECT trash_id FROM nodes WHERE id = ?)",
        )
        .bind(&node.id)
        .execute(&mut *tx)
        .await?;
        tree::touch(&mut tx, &parent_id).await?;
        tree::log(&mut tx, &user, Some(&node), "restore", "").await?;
    }
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_forever(State(st): State<AppState>, user: User, Json(req): Json<BatchReq>) -> AppResult<Json<Value>> {
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    let mut orphans = Vec::new();
    let mut on_disk = Vec::new();
    for id in &outermost(&mut tx, &req.ids()?).await? {
        let (node, _) = trash_root(&mut tx, &user, id, Need::Delete).await?;
        tree::log(&mut tx, &user, Some(&node), "delete", "").await?;
        on_disk.extend(fsops::trash_folder(&node));
        orphans.extend(tree::purge_subtree(&mut tx, &node.id).await?);
    }
    tx.commit().await?;
    tree::schedule_blob_removal(&st, orphans);
    fsops::remove_later(on_disk);
    Ok(Json(json!({ "ok": true })))
}

/// The spaces whose trash Empty trash deletes: those the user manages (or owns), except read-only spaces. The trash
/// also lists items of spaces the user can only view, which stay.
async fn empty_trash_drives(conn: &mut SqliteConnection, user: &User) -> AppResult<Vec<String>> {
    if !(user.can_delete || user.is_admin()) {
        return Ok(Vec::new());
    }
    let ids = trash_drives(conn, user, Role::Manager).await?;
    let writable: Vec<(String,)> =
        sqlx::query_as("SELECT id FROM drives WHERE id IN (SELECT value FROM json_each(?)) AND read_only = 0")
            .bind(serde_json::to_string(&ids).unwrap())
            .fetch_all(conn)
            .await?;
    Ok(writable.into_iter().map(|(id,)| id).collect())
}

#[derive(Serialize)]
pub struct EmptyTrashSpace {
    kind: String,
    name: String,
    items: i64,
}

/// What Empty trash would delete: the number of items per space, so the page can ask about exactly that
pub async fn empty_trash_preview(State(st): State<AppState>, user: User) -> AppResult<Json<Vec<EmptyTrashSpace>>> {
    let mut c = st.db.acquire().await?;
    let drive_ids = empty_trash_drives(&mut c, &user).await?;
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT d.kind, d.name, COUNT(*) FROM nodes n JOIN drives d ON d.id = n.drive_id
         WHERE n.trash_root = 1 AND n.drive_id IN (SELECT value FROM json_each(?))
         GROUP BY d.id ORDER BY CASE d.kind WHEN 'personal' THEN 0 WHEN 'company' THEN 1 ELSE 2 END, d.name",
    )
    .bind(serde_json::to_string(&drive_ids).unwrap())
    .fetch_all(&mut *c)
    .await?;
    Ok(Json(rows.into_iter().map(|(kind, name, items)| EmptyTrashSpace { kind, name, items }).collect()))
}

/// Empty trash: spaces where I'm a manager (or owner)
pub async fn empty_trash(State(st): State<AppState>, user: User) -> AppResult<Json<Value>> {
    let drive_ids = empty_trash_drives(&mut *st.db.acquire().await?, &user).await?;
    let total = purge_trash_in_batches(
        &st,
        "SELECT id FROM nodes WHERE trash_root = 1 AND drive_id IN (SELECT value FROM json_each(?1)) LIMIT ?2",
        serde_json::to_string(&drive_ids).unwrap(),
    )
    .await?;
    if total > 0 {
        let _w = st.write_lock.lock().await;
        let mut tx = st.db.begin().await?;
        tree::log(&mut tx, &user, None, "empty_trash", &format!("{total} {}", if total == 1 { "item" } else { "items" })).await?;
        tx.commit().await?;
    }
    Ok(Json(json!({ "ok": true, "deleted": total })))
}

/// Automatically purges trash items older than the retention period
pub async fn purge_expired_trash(st: &AppState, days: i64) -> AppResult<usize> {
    let cutoff = now() - days * 86400;
    purge_trash_in_batches(st, "SELECT id FROM nodes WHERE trash_root = 1 AND trashed_at < ?1 LIMIT ?2", cutoff.to_string()).await
}

/// Number of trash items purged per transaction
const PURGE_BATCH: i64 = 100;

/// Permanently deletes the trash roots selected by `select` (`?1` = parameter, `?2` = batch size), a batch per transaction,
/// so purging thousands of items doesn't hold the write lock (and block every upload and save) for the whole time.
/// Returns the number of trash roots purged.
async fn purge_trash_in_batches(st: &AppState, select: &'static str, param: String) -> AppResult<usize> {
    let mut total = 0;
    loop {
        let _w = st.write_lock.lock().await;
        let mut tx = st.db.begin().await?;
        let ids: Vec<(String,)> = sqlx::query_as(select).bind(&param).bind(PURGE_BATCH).fetch_all(&mut *tx).await?;
        if ids.is_empty() {
            return Ok(total);
        }
        let mut orphans = Vec::new();
        let mut on_disk = Vec::new();
        for (id,) in &ids {
            if let Some(node) = tree::get_node(&mut tx, id).await? {
                on_disk.extend(fsops::trash_folder(&node));
            }
            orphans.extend(tree::purge_subtree(&mut tx, id).await?);
        }
        tx.commit().await?;
        tree::schedule_blob_removal(st, orphans);
        fsops::remove_later(on_disk);
        total += ids.len();
    }
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: String,
}

/// Searches all spaces and shared folders I can access
pub async fn search(State(st): State<AppState>, user: User, Query(q): Query<SearchQuery>) -> AppResult<Json<Vec<Located>>> {
    let term = q.q.trim();
    if term.is_empty() {
        return Ok(Json(Vec::new()));
    }
    let (drives, folders) = tree::scope(&mut *st.db.acquire().await?, &user).await?;
    let escaped = term.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    let sql = format!(
        "SELECT {NODE_COLS} FROM nodes n
         WHERE {} AND n.trashed_at IS NULL AND n.parent_id IS NOT NULL AND n.name LIKE ?3 ESCAPE '\\'
         ORDER BY (n.kind = 'folder') DESC, n.updated_at DESC LIMIT 300",
        tree::scope_sql(1, 2)
    );
    let nodes: Vec<Node> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(drives).bind(folders).bind(format!("%{escaped}%")).fetch_all(&st.db).await?;
    Ok(Json(locate(&st, &user, nodes).await?))
}

#[derive(Deserialize)]
pub struct FavoriteReq {
    ids: Vec<String>,
    favorite: bool,
}

pub async fn set_favorite(State(st): State<AppState>, user: User, Json(req): Json<FavoriteReq>) -> AppResult<Json<Value>> {
    if req.ids.is_empty() || req.ids.len() > MAX_BATCH {
        return Err(AppError::bad_request("Select 1 to 1000 items"));
    }
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    for id in &req.ids {
        let node = tree::owned_node(&mut tx, &user, id).await?;
        not_root(&node)?;
        if req.favorite {
            sqlx::query("INSERT OR IGNORE INTO favorites (user_id, node_id, created_at) VALUES (?, ?, ?)")
                .bind(user.id)
                .bind(&node.id)
                .bind(now())
                .execute(&mut *tx)
                .await?;
        } else {
            sqlx::query("DELETE FROM favorites WHERE user_id = ? AND node_id = ?").bind(user.id).bind(&node.id).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn favorites(State(st): State<AppState>, user: User, Query(q): Query<ListQuery>) -> AppResult<Json<Vec<Located>>> {
    let (drives, folders) = tree::scope(&mut *st.db.acquire().await?, &user).await?;
    let sql = format!(
        "SELECT {NODE_COLS} FROM favorites f JOIN nodes n ON n.id = f.node_id
         WHERE f.user_id = ?3 AND {} AND n.trashed_at IS NULL {}",
        tree::scope_sql(1, 2),
        order_clause(q.sort.as_deref(), q.order.as_deref())
    );
    let nodes: Vec<Node> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(drives).bind(folders).bind(user.id).fetch_all(&st.db).await?;
    Ok(Json(locate(&st, &user, nodes).await?))
}

/// Recent: files I uploaded or modified (that I still have access to)
pub async fn recent(State(st): State<AppState>, user: User) -> AppResult<Json<Vec<Located>>> {
    let (drives, folders) = tree::scope(&mut *st.db.acquire().await?, &user).await?;
    let sql = format!(
        "SELECT {NODE_COLS} FROM nodes n
         WHERE n.owner_id = ?3 AND {} AND n.kind = 'file' AND n.trashed_at IS NULL
         ORDER BY n.updated_at DESC LIMIT 60",
        tree::scope_sql(1, 2)
    );
    let nodes: Vec<Node> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(drives).bind(folders).bind(user.id).fetch_all(&st.db).await?;
    Ok(Json(locate(&st, &user, nodes).await?))
}

#[derive(Serialize)]
pub struct SharedItem {
    #[serde(flatten)]
    located: Located,
    role: Role,
    sharer: String,
}

/// Shared with me: folders and files others shared with me
pub async fn shared_with_me(State(st): State<AppState>, user: User) -> AppResult<Json<Vec<SharedItem>>> {
    let items = tree::shared_with_me(&mut *st.db.acquire().await?, &user).await?;
    let mut roles: HashMap<String, (Role, String)> = HashMap::new();
    let mut nodes = Vec::with_capacity(items.len());
    for (n, role, sharer) in items {
        roles.insert(n.id.clone(), (role, sharer));
        nodes.push(n);
    }
    let located = locate(&st, &user, nodes).await?;
    Ok(Json(
        located
            .into_iter()
            .map(|l| {
                let (role, sharer) = roles.remove(&l.node.id).unwrap_or((Role::Viewer, String::new()));
                SharedItem { located: l, role, sharer }
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;
    use axum::http::StatusCode;

    fn batch(ids: &[&str], dest: &str) -> Json<BatchReq> {
        Json(BatchReq { ids: ids.iter().map(|s| s.to_string()).collect(), dest_id: Some(dest.to_string()) })
    }

    fn ids(list: &[&str]) -> Json<BatchReq> {
        Json(BatchReq { ids: list.iter().map(|s| s.to_string()).collect(), dest_id: None })
    }

    #[tokio::test]
    async fn names_differing_only_in_non_english_letter_case_are_the_same_name() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let folder = env.folder(&amy, &amy.root_id, "Été").await;
        let mut c = env.st.db.acquire().await.unwrap();
        assert!(tree::name_taken(&mut c, &amy.root_id, "été").await.unwrap());
        assert_eq!(tree::unique_name(&mut c, &amy.root_id, "ÉTÉ", true).await.unwrap(), "ÉTÉ (1)");
        // An uploaded folder "été/x" goes into the existing "Été"
        let found = tree::ensure_folders(&mut c, amy.id, &amy.root_id, "été", "").await.unwrap();
        assert_eq!(found, folder);
        // The database refuses a second one too
        let dup = sqlx::query(
            "INSERT INTO nodes (id, owner_id, parent_id, kind, name, drive_id, created_at, updated_at)
             SELECT 'x', owner_id, id, 'folder', 'été', drive_id, 0, 0 FROM nodes WHERE id = ?",
        )
        .bind(&amy.root_id)
        .execute(&mut *c)
        .await;
        assert!(dup.is_err());
    }

    #[tokio::test]
    async fn folders_list_names_in_natural_order_and_types_by_extension() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        for name in ["File 10.txt", "file 2.txt", "File 1.docx", "b.pdf", "README"] {
            env.file(&amy, &amy.root_id, name).await;
        }
        let list = |sort: &'static str| {
            let (st, amy) = (env.st.clone(), amy.clone());
            async move {
                let q = ListQuery { sort: Some(sort.into()), order: None, folders_only: None };
                let Json(items) = children(State(st), amy.clone(), Path(amy.root_id.clone()), Query(q)).await.unwrap();
                items.into_iter().map(|n| n.name).collect::<Vec<_>>()
            }
        };
        assert_eq!(list("name").await, ["b.pdf", "File 1.docx", "file 2.txt", "File 10.txt", "README"]);
        assert_eq!(list("type").await, ["README", "File 1.docx", "b.pdf", "file 2.txt", "File 10.txt"]);
    }

    #[tokio::test]
    async fn empty_trash_counts_and_deletes_only_what_the_user_manages() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let a = env.file(&amy, &amy.root_id, "a.txt").await;
        let b = env.file(&amy, &amy.root_id, "b.txt").await;
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&a, &b])).await.unwrap();
        let Json(preview) = empty_trash_preview(State(env.st.clone()), amy.clone()).await.unwrap();
        assert_eq!(preview.len(), 1);
        assert_eq!((preview[0].kind.as_str(), preview[0].items), ("personal", 2));

        // Without permission to delete, nothing is emptied and nothing is counted
        let mut bob = env.user("bob", false).await;
        let c = env.file(&bob, &bob.root_id, "c.txt").await;
        let _ = trash(State(env.st.clone()), bob.clone(), ids(&[&c])).await.unwrap();
        bob.can_delete = false;
        let Json(preview) = empty_trash_preview(State(env.st.clone()), bob.clone()).await.unwrap();
        assert!(preview.is_empty());

        let Json(done) = empty_trash(State(env.st.clone()), amy.clone()).await.unwrap();
        assert_eq!(done["deleted"], 2);
        let Json(left) = list_trash(State(env.st.clone()), bob.clone()).await.unwrap();
        assert_eq!(left.len(), 1);
    }

    #[tokio::test]
    async fn the_same_item_selected_twice_is_copied_once() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let doc = env.file(&amy, &amy.root_id, "a.txt").await;
        let dest = env.folder(&amy, &amy.root_id, "Copies").await;
        let _ = copy_nodes(State(env.st.clone()), amy.clone(), batch(&[&doc, &doc, &doc], &dest)).await.unwrap();
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE parent_id = ?").bind(&dest).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn a_file_version_never_goes_back() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let doc = env.file(&amy, &amy.root_id, "a.txt").await;
        // Saved several times within a second: the version is ahead of the clock
        let ahead = now() + 5;
        sqlx::query("UPDATE nodes SET updated_at = ? WHERE id = ?").bind(ahead).bind(&doc).execute(&env.st.db).await.unwrap();
        let _ = rename(State(env.st.clone()), amy.clone(), Path(doc.clone()), Json(RenameReq { name: "b.txt".into() })).await.unwrap();
        let (after,): (i64,) = sqlx::query_as("SELECT updated_at FROM nodes WHERE id = ?").bind(&doc).fetch_one(&env.st.db).await.unwrap();
        assert!(after > ahead, "renaming moved the version back from {ahead} to {after}");
    }

    #[tokio::test]
    async fn a_selection_with_a_folder_and_something_inside_it_can_be_trashed_and_deleted() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let folder = env.folder(&amy, &amy.root_id, "Folder").await;
        let inner = env.folder(&amy, &folder, "Inner").await;
        let doc = env.file(&amy, &inner, "a.txt").await;
        let other = env.file(&amy, &amy.root_id, "b.txt").await;

        // Moving to the trash: the folder, a file deep inside it, and a duplicate id
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&doc, &folder, &other, &folder])).await.unwrap();
        let Json(listed) = list_trash(State(env.st.clone()), amy.clone()).await.unwrap();
        let mut names: Vec<String> = listed.into_iter().map(|l| l.node.name).collect();
        names.sort();
        assert_eq!(names, vec!["Folder", "b.txt"]);

        // Deleting for good: an item trashed on its own before its folder is listed separately; selecting both works
        let _ = restore(State(env.st.clone()), amy.clone(), ids(&[&folder])).await.unwrap();
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&doc])).await.unwrap();
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&folder])).await.unwrap();
        let Json(listed) = list_trash(State(env.st.clone()), amy.clone()).await.unwrap();
        let all: Vec<String> = listed.into_iter().map(|l| l.node.id).collect();
        assert!(all.contains(&doc) && all.contains(&folder));
        let refs: Vec<&str> = all.iter().map(String::as_str).collect();
        let _ = delete_forever(State(env.st.clone()), amy.clone(), ids(&refs)).await.unwrap();
        let (left,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE id IN (?, ?, ?, ?)")
            .bind(&folder)
            .bind(&inner)
            .bind(&doc)
            .bind(&other)
            .fetch_one(&env.st.db)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }

    #[tokio::test]
    async fn many_listings_at_once_share_the_connection_pool() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let folder = env.folder(&amy, &amy.root_id, "docs").await;
        let doc = env.file(&amy, &folder, "a.txt").await;
        // Each listing used to hold one connection while waiting for a second one, so more listings than
        // connections at the same moment waited for each other until the pool timed out
        let listings = (0..32).map(|i| {
            let (st, amy, folder, doc) = (env.st.clone(), amy.clone(), folder.clone(), doc.clone());
            async move {
                if i % 2 == 0 {
                    let q = Query(ListQuery { sort: None, order: None, folders_only: None });
                    children(State(st), amy, Path(folder), q).await.map(|_| ())
                } else {
                    get(State(st), amy, Path(doc)).await.map(|_| ())
                }
            }
        });
        let all = futures_util::future::join_all(listings.map(tokio::spawn));
        let results = tokio::time::timeout(std::time::Duration::from_secs(10), all).await.expect("listings stalled");
        assert!(results.into_iter().all(|r| r.unwrap().is_ok()));
    }

    #[tokio::test]
    async fn folder_share_editor_can_only_restore_or_destroy_items_inside_the_share() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let shared = env.folder(&amy, &amy.root_id, "shared").await;
        let doc = env.file(&amy, &shared, "report.txt").await;
        env.grant(&shared, &ben, "editor").await;

        // Ben trashes a file inside the shared folder: he may restore it (the folder is his to edit)
        let _ = trash(State(env.st.clone()), ben.clone(), ids(&[&doc])).await.unwrap();
        let _ = restore(State(env.st.clone()), ben.clone(), ids(&[&doc])).await.unwrap();

        // Amy trashes the shared folder itself: Ben has no rights on Amy's root folder, so he can neither restore nor destroy it
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&shared])).await.unwrap();
        let err = restore(State(env.st.clone()), ben.clone(), ids(&[&shared])).await.unwrap_err();
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        let err = delete_forever(State(env.st.clone()), ben.clone(), ids(&[&shared])).await.unwrap_err();
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        // The owner can
        let _ = restore(State(env.st.clone()), amy.clone(), ids(&[&shared])).await.unwrap();
        assert_eq!(env.drive_of(&doc).await, env.drive_of(&amy.root_id).await);
    }

    #[tokio::test]
    async fn administrators_manage_the_trash_of_team_spaces_they_are_not_members_of() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let admin = env.admin().await;
        let (team_root, doc, other, private) = {
            let mut conn = env.st.db.acquire().await.unwrap();
            let (_, root) = crate::db::create_drive(&mut conn, "Team", "team", amy.id, 0).await.unwrap();
            crate::db::add_grant(&mut conn, &root, "user", amy.id, "owner", Some(amy.id), None).await.unwrap();
            drop(conn);
            let doc = env.file(&amy, &root, "plan.txt").await;
            let other = env.file(&amy, &root, "old.txt").await;
            let private = env.file(&amy, &amy.root_id, "diary.txt").await;
            (root, doc, other, private)
        };
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&doc, &other, &private])).await.unwrap();

        // Listed with the space's name, and each item can be restored or deleted, not only emptied as a whole
        let Json(listed) = list_trash(State(env.st.clone()), admin.clone()).await.unwrap();
        let plan = listed.iter().find(|l| l.node.name == "plan.txt").unwrap();
        let space = plan.location_space.as_ref().unwrap();
        assert_eq!((space.kind.as_str(), space.name.as_str(), plan.location_path.len()), ("team", "Team", 0));
        let listed: Vec<(String, String)> = listed.into_iter().map(|l| (l.node.name.clone(), l.location.clone())).collect();
        assert!(listed.contains(&("plan.txt".to_string(), "Team".to_string())), "{listed:?}");
        assert!(!listed.iter().any(|(n, _)| n == "diary.txt"), "personal spaces stay private: {listed:?}");
        let _ = restore(State(env.st.clone()), admin.clone(), ids(&[&doc])).await.unwrap();
        let _ = delete_forever(State(env.st.clone()), admin.clone(), ids(&[&other])).await.unwrap();
        assert_eq!(env.drive_of(&doc).await, env.drive_of(&team_root).await);
        // A personal space's trash is not the administrator's
        assert!(restore(State(env.st.clone()), admin.clone(), ids(&[&private])).await.is_err());
    }

    #[tokio::test]
    async fn folder_share_editor_cannot_take_folder_out_of_its_drive() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let shared = env.folder(&amy, &amy.root_id, "Shared with Ben").await;
        let inner = env.folder(&amy, &shared, "Inner").await;
        let doc = env.file(&amy, &shared, "report.txt").await;
        env.grant(&shared, &ben, "editor").await;
        let amy_drive = env.drive_of(&shared).await;

        // Ben is only an editor via a folder share: he can't move the folder or its files into his own space
        let err = move_nodes(State(env.st.clone()), ben.clone(), batch(&[&shared], &ben.root_id)).await.unwrap_err();
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        let err = move_nodes(State(env.st.clone()), ben.clone(), batch(&[&doc], &ben.root_id)).await.unwrap_err();
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert_eq!(env.drive_of(&shared).await, amy_drive);
        assert_eq!(env.drive_of(&doc).await, amy_drive);

        // Organizing within the shared folder is fine
        let _ = move_nodes(State(env.st.clone()), ben.clone(), batch(&[&doc], &inner)).await.unwrap();
        assert_eq!(env.drive_of(&doc).await, amy_drive);

        // A space member (the owner) can move across spaces
        let company = env.st.shared_root().unwrap();
        let _ = move_nodes(State(env.st.clone()), amy.clone(), batch(&[&shared], &company)).await.unwrap();
        assert_eq!(env.drive_of(&shared).await, env.drive_of(&company).await);
        assert_eq!(env.drive_of(&doc).await, env.drive_of(&company).await);

        // Members of the company space (everyone can edit) can move its items into their own space
        let _ = move_nodes(State(env.st.clone()), ben.clone(), batch(&[&doc], &ben.root_id)).await.unwrap();
        assert_eq!(env.drive_of(&doc).await, env.drive_of(&ben.root_id).await);
    }

    #[tokio::test]
    async fn space_usage_counter_follows_copies_moves_and_deletes() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let folder = env.folder(&amy, &amy.root_id, "docs").await;
        let doc = env.file(&amy, &folder, "a.bin").await;
        sqlx::query("UPDATE nodes SET size = 1000 WHERE id = ?").bind(&doc).execute(&env.st.db).await.unwrap();
        tree::recompute_usage(&env.st).await.unwrap();
        let db = env.st.db.clone();
        let used = |drive: String| {
            let db = db.clone();
            async move {
                let (u,): (i64,) = sqlx::query_as("SELECT used_bytes FROM drives WHERE id = ?").bind(drive).fetch_one(&db).await.unwrap();
                u
            }
        };
        let personal = env.drive_of(&amy.root_id).await;
        let company = env.drive_of(&env.st.shared_root().unwrap()).await;
        assert_eq!(used(personal.clone()).await, 1000);

        // Copy within the space: counted twice
        let _ = copy_nodes(State(env.st.clone()), amy.clone(), batch(&[&doc], &amy.root_id)).await.unwrap();
        assert_eq!(used(personal.clone()).await, 2000);
        // Move the folder to the company space: bytes follow
        let _ = move_nodes(State(env.st.clone()), amy.clone(), batch(&[&folder], &env.st.shared_root().unwrap())).await.unwrap();
        assert_eq!(used(personal.clone()).await, 1000);
        assert_eq!(used(company.clone()).await, 1000);
        // Trash keeps counting (it still takes space); permanent deletion frees it
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&folder])).await.unwrap();
        assert_eq!(used(company.clone()).await, 1000);
        let _ = delete_forever(State(env.st.clone()), amy.clone(), ids(&[&folder])).await.unwrap();
        assert_eq!(used(company.clone()).await, 0);
        // The counters agree with the node table
        tree::recompute_usage(&env.st).await.unwrap();
        assert_eq!(used(personal).await, 1000);
    }
}
