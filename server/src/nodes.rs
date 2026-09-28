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
    fsops, paths,
    logs,
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
    /// The path that names this item for the user, as typed into the address bar or used over WebDAV (see paths.rs):
    /// `["My files", "Reports"]`; None when no path reaches it
    location: Option<Vec<String>>,
    /// Reason the storage location holding the content is offline (files: where the content is; folders: the space's location)
    offline: Option<String>,
    /// A read-only space: browse, download and share only
    read_only: bool,
}

/// The path visible to the user: space members see the full path; people with shared access only see from the shared folder down
async fn visible_path(conn: &mut SqliteConnection, user: &User, node: &Node) -> AppResult<(Vec<Crumb>, bool)> {
    let path = tree::path_of(conn, &node.id).await?;
    let member_of = tree::member_of(conn, user).await?;
    if member_of.iter().any(|d| d == node.drive()) {
        return Ok((path, false));
    }
    let shared = tree::shared_ids(conn, user, &member_of).await?;
    let start = tree::shared_start(&path, &shared).unwrap_or(0);
    Ok((path.into_iter().skip(start).collect(), true))
}

pub async fn get(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<Json<NodeInfo>> {
    let mut c = st.db.acquire().await?;
    let (mut node, role) = tree::node_with_role(&mut c, &user, &id).await?;
    let drive = tree::get_drive(&mut c, node.drive()).await?.ok_or_else(|| AppError::not_found("Space not found"))?;
    let (path, via_share) = visible_path(&mut c, &user, &node).await?;
    let location = paths::location_of(&mut c, &user, &node).await?;
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
        location,
        offline,
        read_only,
    }))
}

#[derive(Deserialize)]
pub struct ContentsReq {
    ids: Vec<String>,
}

/// What a set of folders holds, like Size and Contains in the Windows properties: everything inside them, at any depth
#[derive(Serialize, Debug, PartialEq)]
pub struct Contents {
    /// Bytes of the files inside
    size: i64,
    files: i64,
    folders: i64,
}

/// Size and number of items inside the given folders (the items themselves are not counted; a file holds nothing).
/// Items in the trash are left out. For the Details pane, of one folder or of a selection of several.
pub async fn contents(State(st): State<AppState>, user: User, Json(req): Json<ContentsReq>) -> AppResult<Json<Contents>> {
    let ids = BatchReq { ids: req.ids, dest_id: None, resolutions: Default::default() }.ids()?;
    let mut c = st.db.acquire().await?;
    let mut folders = Vec::with_capacity(ids.len());
    for id in &ids {
        let (node, _) = tree::node_with_role(&mut c, &user, id).await?;
        if node.is_folder() {
            folders.push(node.id);
        }
    }
    // A folder selected together with a folder around it would be counted twice
    let folders = outermost(&mut c, &folders).await?;
    // Only the columns needed, and only folders are expanded, so even a large space is summed quickly
    let (size, files, folders): (i64, i64, i64) = sqlx::query_as(
        "WITH RECURSIVE sub(id, kind, size) AS (
           SELECT n.id, n.kind, n.size FROM nodes n
           WHERE n.parent_id IN (SELECT value FROM json_each(?1)) AND n.trashed_at IS NULL
           UNION ALL
           SELECT n.id, n.kind, n.size FROM nodes n JOIN sub ON n.parent_id = sub.id
           WHERE sub.kind = 'folder' AND n.trashed_at IS NULL
         )
         SELECT COALESCE(SUM(CASE WHEN kind = 'file' THEN size ELSE 0 END), 0),
                COALESCE(SUM(kind = 'file'), 0), COALESCE(SUM(kind = 'folder'), 0) FROM sub",
    )
    .bind(serde_json::to_string(&folders).unwrap())
    .fetch_one(&mut *c)
    .await?;
    Ok(Json(Contents { size, files, folders }))
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    sort: Option<String>,
    order: Option<String>,
    folders_only: Option<bool>,
    /// Items per page (at most MAX_PAGE); without it, the whole list comes at once
    limit: Option<i64>,
    /// The `next` of the previous page
    after: Option<String>,
}

/// Largest page of a folder or trash listing
const MAX_PAGE: i64 = 5000;

/// A whole listing, or one page of it when the request gave a `limit`
#[derive(Serialize, Debug)]
#[serde(untagged)]
pub enum Listing<T> {
    All(Vec<T>),
    Page {
        items: Vec<T>,
        /// `after` for the next page; None on the last page
        next: Option<String>,
    },
}

impl<T> Listing<T> {
    fn new(items: Vec<T>, limit: Option<i64>, next: Option<String>) -> Self {
        if limit.is_some() { Listing::Page { items, next } } else { Listing::All(items) }
    }

    /// Changes the items, keeping the page
    pub fn map<U>(self, f: impl FnOnce(Vec<T>) -> Vec<U>) -> Listing<U> {
        match self {
            Listing::All(items) => Listing::All(f(items)),
            Listing::Page { items, next } => Listing::Page { items: f(items), next },
        }
    }

    pub fn items_mut(&mut self) -> &mut Vec<T> {
        match self {
            Listing::All(items) | Listing::Page { items, .. } => items,
        }
    }

    #[cfg(test)]
    pub fn into_items(self) -> Vec<T> {
        match self {
            Listing::All(items) | Listing::Page { items, .. } => items,
        }
    }
}

/// Where a folder page ends: the sort values of its last item, so the next page starts right after it even when items
/// were added or removed in between (keyset paging). The browser gets it as opaque text.
#[derive(Serialize, Deserialize)]
struct Cursor {
    folder: bool,
    key: SortValue,
    name: String,
    id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum SortValue {
    Int(i64),
    Text(String),
}

fn encode_cursor<T: Serialize>(c: &T) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(c).unwrap())
}

fn decode_cursor<T: serde::de::DeserializeOwned>(s: &str) -> AppResult<T> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(|| AppError::bad_request("This list has changed. Reload it."))
}

/// The page size asked for, and where the page starts (only with a size)
fn page_of<T: serde::de::DeserializeOwned>(limit: Option<i64>, after: Option<&str>) -> AppResult<(Option<i64>, Option<T>)> {
    let limit = limit.map(|l| l.clamp(1, MAX_PAGE));
    let after = match (after, limit) {
        (Some(a), Some(_)) => Some(decode_cursor(a)?),
        _ => None,
    };
    Ok((limit, after))
}

#[derive(Clone, Copy, PartialEq)]
enum SortCol {
    Name,
    Size,
    Updated,
    Created,
    Type,
}

impl SortCol {
    fn parse(sort: Option<&str>) -> Self {
        match sort {
            Some("size") => Self::Size,
            Some("updated") => Self::Updated,
            Some("created") => Self::Created,
            Some("type") => Self::Type,
            _ => Self::Name,
        }
    }

    fn expr(self) -> &'static str {
        const EXT: &str = "CASE WHEN n.kind = 'file' AND length(rtrim(n.name, replace(n.name, '.', ''))) > 1
            THEN lower(substr(n.name, length(rtrim(n.name, replace(n.name, '.', ''))) + 1)) ELSE '' END";
        match self {
            Self::Size => "n.size",
            Self::Updated => "n.updated_at",
            Self::Created => "n.created_at",
            Self::Type => EXT,
            Self::Name => "n.name COLLATE natural_name",
        }
    }
}

/// Sorting of folder listings, folders first. Names sort naturally ("File 2" before "File 10", the `natural_name`
/// collation), and Type sorts by extension, as the column shows it (`extOf` in the browser: after the last dot, unless
/// the name starts with it). The id comes last so every item has a fixed place, which paging relies on.
pub fn order_clause(sort: Option<&str>, order: Option<&str>) -> String {
    let col = SortCol::parse(sort).expr();
    let dir = if order == Some("desc") { "DESC" } else { "ASC" };
    format!("ORDER BY (n.kind = 'folder') DESC, {col} {dir}, n.name COLLATE natural_name, n.id")
}

/// The children of a folder (not in the trash) in the order of `order_clause`; with a limit, one page of them.
/// SQLite runs NODE_COLS' subqueries after sorting, only for the rows it returns: measured on a folder of 50,000
/// items, that is faster than joining the same tables for every row, with or without a limit.
pub async fn list_children(conn: &mut SqliteConnection, parent_id: &str, q: &ListQuery) -> AppResult<Listing<Node>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        node: Node,
        /// The extension when sorting by type (the other sort values are columns of the node)
        ext: Option<String>,
    }
    let sort = SortCol::parse(q.sort.as_deref());
    let desc = q.order.as_deref() == Some("desc");
    let (limit, after) = page_of::<Cursor>(q.limit, q.after.as_deref())?;
    let kind_filter = if q.folders_only == Some(true) { "AND n.kind = 'folder'" } else { "" };
    // Everything after the cursor in that order: folders before files, then the sort column, the name and the id
    let col = sort.expr();
    let keyset = if after.is_some() {
        let op = if desc { "<" } else { ">" };
        format!(
            "AND ((n.kind = 'folder') < ?2 OR ((n.kind = 'folder') = ?2 AND ({col} {op} ?3 OR ({col} = ?3
                 AND (n.name COLLATE natural_name > ?4 OR (n.name COLLATE natural_name = ?4 AND n.id > ?5))))))"
        )
    } else {
        String::new()
    };
    let ext = if sort == SortCol::Type { col } else { "NULL" };
    let sql = format!(
        "SELECT {NODE_COLS}, {ext} AS ext FROM nodes n
         WHERE n.parent_id = ?1 AND n.trashed_at IS NULL {kind_filter} {keyset} {} LIMIT ?6",
        order_clause(q.sort.as_deref(), q.order.as_deref())
    );
    let query = sqlx::query_as::<_, Row>(sqlx::AssertSqlSafe(sql.as_str())).bind(parent_id);
    let query = match &after {
        Some(c) => {
            let query = query.bind(c.folder);
            let query = match &c.key {
                SortValue::Int(v) => query.bind(*v),
                SortValue::Text(v) => query.bind(v.clone()),
            };
            query.bind(c.name.clone()).bind(c.id.clone())
        }
        None => query.bind(None::<bool>).bind(None::<i64>).bind(None::<String>).bind(None::<String>),
    };
    // SQLite reads a negative limit as no limit
    let rows = query.bind(limit.unwrap_or(-1)).fetch_all(&mut *conn).await?;
    let next = match (limit, rows.last()) {
        (Some(l), Some(last)) if rows.len() as i64 == l => Some(encode_cursor(&Cursor {
            folder: last.node.is_folder(),
            key: match sort {
                SortCol::Size => SortValue::Int(last.node.size),
                SortCol::Updated => SortValue::Int(last.node.updated_at),
                SortCol::Created => SortValue::Int(last.node.created_at),
                SortCol::Type => SortValue::Text(last.ext.clone().unwrap_or_default()),
                SortCol::Name => SortValue::Text(last.node.name.clone()),
            },
            name: last.node.name.clone(),
            id: last.node.id.clone(),
        })),
        _ => None,
    };
    Ok(Listing::new(rows.into_iter().map(|r| r.node).collect(), limit, next))
}

pub async fn children(
    State(st): State<AppState>,
    user: User,
    Path(id): Path<String>,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Listing<Node>>> {
    let folder = tree::folder_for(&mut *st.db.acquire().await?, &user, &id, Need::Read).await?;
    if folder.in_folder_space() && q.after.is_none() {
        // Changes made on the server's folder show up when the folder is opened (not again for each further page).
        // No connection is held meanwhile: syncing takes its own, and many folders opened at once would otherwise
        // use up the pool while each waits for a second one
        crate::folders::sync_folder(&st, &folder).await;
    }
    let mut c = st.db.acquire().await?;
    let mut list = list_children(&mut c, &folder.id, &q).await?;
    tree::mark_favorites(&mut c, user.id, list.items_mut()).await?;
    Ok(Json(list))
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
    logs::record_activity(&mut tx, &user, Some(&node), "create_folder", "").await?;
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
    ids: Vec<String>,
    dest_id: Option<String>,
    /// What to do with each item (by id) whose name the destination already has; the browser asks first (`conflicts`)
    #[serde(default)]
    resolutions: HashMap<String, Resolution>,
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
    fn resolution(&self, id: &str) -> Option<Resolution> {
        self.resolutions.get(id).copied()
    }
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
        let mut node = tree::node_for(&mut tx, &user, id, Need::Write).await?;
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
        if let Some(existing) = tree::find_child(&mut tx, &dest.id, &node.name).await? {
            match req.resolution(&node.id) {
                None => return Err(AppError::conflict(format!("The destination folder already contains \"{}\"", node.name))),
                Some(Resolution::Skip) => continue,
                Some(Resolution::Keep) if dest.in_folder_space() => name = fsops::free_name(&mut tx, &dest, &node.name, node.is_folder()).await?,
                Some(Resolution::Keep) => name = tree::unique_name(&mut tx, &dest.id, &node.name, node.is_folder()).await?,
                Some(Resolution::Replace) => replace_existing(&mut tx, &user, &locks, &existing, &node).await?,
            }
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
                let mut nodes: Vec<Node> = subtree.into_iter().filter(|n| n.trashed_at.is_none()).collect();
                // The first one is the item itself: it goes in under its name in the destination
                nodes[0].name = name;
                across_items += nodes.len();
                if across_items > MAX_COPY_ITEMS {
                    return Err(AppError::bad_request("Move at most 20,000 items at once to or from a folder on the server"));
                }
                across.push(nodes);
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
        logs::record_activity(&mut tx, &user, Some(&node), "move", &format!("→ {}", if dest.parent_id.is_none() { "Root folder" } else { &dest.name })).await?;
    }
    tree::touch(&mut tx, &dest.id).await?;
    tx.commit().await?;
    locks.committed();
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
        // Without an answer the copy gets a number, as it always did; a copy into its own folder always does
        if node.parent_id.as_deref() != Some(dest.id.as_str())
            && let Some(existing) = tree::find_child(&mut tx, &dest.id, &node.name).await?
        {
            match req.resolution(&node.id) {
                Some(Resolution::Skip) => continue,
                Some(Resolution::Replace) => replace_existing(&mut tx, &user, &locks, &existing, &node).await?,
                Some(Resolution::Keep) | None => {}
            }
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
        let mut rows = Vec::with_capacity(nodes.len());
        let mut blobs = Vec::new();
        for (i, n) in nodes.iter().enumerate() {
            let new_id = new_id();
            let (parent, name) = if i == 0 {
                (dest.id.clone(), tree::unique_name(&mut tx, &dest.id, &n.name, n.is_folder()).await?)
            } else {
                // The subtree is sorted by depth, so parents have always been copied already
                (ids[n.parent_id.as_ref().unwrap()].clone(), n.name.clone())
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
        logs::record_activity(&mut tx, &user, Some(&nodes[0]), "copy", &format!("→ {}", if dest.parent_id.is_none() { "Root folder" } else { &dest.name })).await?;
    }
    tree::adjust_usage(&mut tx, dest.drive(), total).await?;
    tree::touch(&mut tx, &dest.id).await?;
    tx.commit().await?;
    locks.committed();
    drop(_w);
    fsops::copy_across(&st, &user, &dest, across).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn trash(State(st): State<AppState>, user: User, Json(req): Json<BatchReq>) -> AppResult<Json<Value>> {
    let ids = req.ids()?;
    let locks = fsops::lock(&st, &user, &ids.iter().map(String::as_str).collect::<Vec<_>>()).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
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
async fn trash_one(conn: &mut SqliteConnection, user: &User, locks: &fsops::SpaceLocks, node: &Node, detail: &str) -> AppResult<()> {
    not_root(node)?;
    let trash_id = new_id();
    if node.in_folder_space() {
        // Into the space's trash folder on disk, so it can be restored
        fsops::trash(conn, locks, node, &trash_id).await?;
    }
    sqlx::query(
        "WITH RECURSIVE sub(id) AS (
           SELECT ?1 UNION ALL
           SELECT c.id FROM nodes c JOIN sub ON c.parent_id = sub.id WHERE c.trashed_at IS NULL
         )
         UPDATE nodes SET trashed_at = ?2, trash_id = ?3 WHERE id IN (SELECT id FROM sub)",
    )
    .bind(&node.id)
    .bind(now())
    .bind(&trash_id)
    .execute(&mut *conn)
    .await?;
    sqlx::query("UPDATE nodes SET trash_root = 1, trashed_by = ? WHERE id = ?").bind(user.id).bind(&node.id).execute(&mut *conn).await?;
    tree::touch(conn, node.parent_id.as_deref().unwrap()).await?;
    logs::record_activity(conn, user, Some(node), "trash", detail).await?;
    Ok(())
}

/// "Replace": the item already in the destination goes to the trash so `incoming` can take its name. The user needs
/// delete permission on it, and it can't be a folder holding `incoming` itself.
async fn replace_existing(conn: &mut SqliteConnection, user: &User, locks: &fsops::SpaceLocks, existing: &Node, incoming: &Node) -> AppResult<()> {
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
    dest_id: Option<String>,
    /// Names of items about to be uploaded
    #[serde(default)]
    names: Vec<String>,
    /// Items about to be moved, copied (with a destination) or restored (without one)
    #[serde(default)]
    ids: Vec<String>,
}

#[derive(Serialize)]
pub struct Conflict {
    /// The item being moved, copied or restored (none for uploads)
    id: Option<String>,
    /// Its name (for uploads, the name asked about)
    name: String,
    kind: Option<String>,
    size: Option<i64>,
    updated_at: Option<i64>,
    /// The item with the same name already there
    existing: Node,
}

/// Uploaded names checked at once (the top-level items of a drop)
const MAX_CONFLICT_NAMES: usize = 10_000;

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
                out.push(Conflict { id: Some(node.id.clone()), name: node.name.clone(), kind: Some(node.kind.clone()), size: Some(node.size), updated_at: Some(node.updated_at), existing });
            }
        }
    } else {
        for id in &req.ids {
            let (node, parent) = trash_root(&mut c, &user, id, Need::Read).await?;
            if let Some(existing) = tree::find_child(&mut c, &parent, &node.name).await? {
                out.push(Conflict { id: Some(node.id.clone()), name: node.name.clone(), kind: Some(node.kind.clone()), size: Some(node.size), updated_at: Some(node.updated_at), existing });
            }
        }
    }
    Ok(Json(out))
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
    /// Trash: who moved the item there; None when unknown (deleted by a removed account)
    #[serde(skip_serializing_if = "Option::is_none")]
    deleted_by: Option<String>,
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
        tree::shared_ids(&mut c, user, &member_of).await?
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
        let start = if space.is_some() { 0 } else { tree::shared_start(&path, &shared).unwrap_or(path.len()) };
        let location_path: Vec<String> = path[start..].iter().map(|p| p.name.clone()).collect();
        let first = space.as_ref().map_or(SHARED_WITH_ME, |s| s.name.as_str());
        let location = std::iter::once(first).chain(location_path.iter().map(String::as_str)).collect::<Vec<_>>().join("/");
        out.push(Located { node, location, location_space: space, location_path, deleted_by: None });
    }
    Ok(out)
}

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

#[derive(Deserialize, Default)]
pub struct TrashQuery {
    /// Items per page (at most MAX_PAGE); without it, the whole list comes at once
    limit: Option<i64>,
    /// The `next` of the previous page
    after: Option<String>,
    /// Only items the user deleted (Deleted by me); otherwise everyone's
    mine: Option<bool>,
}

/// Where a trash page ends (newest deleted first)
#[derive(Serialize, Deserialize)]
struct TrashCursor {
    at: i64,
    id: String,
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
         WHERE n.trash_root = 1 AND n.drive_id IN (SELECT value FROM json_each(?1)) {keyset} {mine}
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
        sqlx::query("UPDATE nodes SET parent_id = ?, name = ? WHERE id = ?")
            .bind(&parent_id)
            .bind(&name)
            .bind(&node.id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE nodes SET trashed_at = NULL, trash_root = 0, trash_id = NULL, trashed_by = NULL
             WHERE trash_id = (SELECT trash_id FROM nodes WHERE id = ?)",
        )
        .bind(&node.id)
        .execute(&mut *tx)
        .await?;
        tree::touch(&mut tx, &parent_id).await?;
        logs::record_activity(&mut tx, &user, Some(&node), "restore", "").await?;
    }
    tx.commit().await?;
    locks.committed();
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_forever(State(st): State<AppState>, user: User, Json(req): Json<BatchReq>) -> AppResult<Json<Value>> {
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    let mut orphans = Vec::new();
    let mut on_disk = Vec::new();
    for id in &outermost(&mut tx, &req.ids()?).await? {
        let (node, _) = trash_root(&mut tx, &user, id, Need::Delete).await?;
        logs::record_activity(&mut tx, &user, Some(&node), "delete", "").await?;
        on_disk.extend(fsops::trash_folder(&node));
        orphans.extend(tree::purge_subtree(&mut tx, &node.id).await?);
    }
    tx.commit().await?;
    tree::schedule_blob_removal(&st, orphans);
    fsops::remove_below_later(on_disk);
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
        logs::record_activity(&mut tx, &user, None, "empty_trash", &format!("{total} {}", if total == 1 { "item" } else { "items" })).await?;
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
        fsops::remove_below_later(on_disk);
        total += ids.len();
    }
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: String,
    /// Only this folder and what's below it (a folder id); without it, everything the user can open
    #[serde(rename = "in")]
    within: Option<String>,
    /// "folder" or "file"
    kind: Option<String>,
    /// Extensions without the dot, comma separated (files only)
    ext: Option<String>,
    /// Modified at or after / before (Unix seconds)
    from: Option<i64>,
    to: Option<i64>,
    /// Size in bytes (files only)
    min_size: Option<i64>,
    max_size: Option<i64>,
    /// Uploaded by (username, exact)
    owner: Option<String>,
}

/// Results returned at most
const SEARCH_LIMIT: i64 = 300;

#[derive(Serialize)]
pub struct SearchResult {
    items: Vec<Located>,
    /// More matches than returned
    truncated: bool,
}

/// Searches names in all spaces and shared folders I can access, or in one folder and below
pub async fn search(State(st): State<AppState>, user: User, Query(q): Query<SearchQuery>) -> AppResult<Json<SearchResult>> {
    let term = q.q.trim();
    if term.is_empty() {
        return Ok(Json(SearchResult { items: Vec::new(), truncated: false }));
    }
    let mut c = st.db.acquire().await?;
    let (drives, folders) = tree::scope(&mut c, &user).await?;
    let mut qb = sqlx::QueryBuilder::<sqlx::Sqlite>::new(format!("SELECT {NODE_COLS} FROM nodes n WHERE "));
    qb.push("(n.drive_id IN (SELECT value FROM json_each(").push_bind(drives).push("))");
    qb.push(" OR n.id IN (WITH RECURSIVE s(id) AS (SELECT value FROM json_each(").push_bind(folders);
    qb.push(") UNION ALL SELECT c.id FROM nodes c JOIN s ON c.parent_id = s.id) SELECT id FROM s))");
    qb.push(" AND n.trashed_at IS NULL AND n.parent_id IS NOT NULL");
    if let Some(within) = q.within.as_deref().filter(|w| !w.is_empty()) {
        let folder = tree::folder_for(&mut c, &user, within, Need::Read).await?;
        qb.push(" AND n.id IN (WITH RECURSIVE d(id) AS (SELECT id FROM nodes WHERE parent_id = ").push_bind(folder.id);
        qb.push(" AND trashed_at IS NULL UNION ALL SELECT c.id FROM nodes c JOIN d ON c.parent_id = d.id WHERE c.trashed_at IS NULL) SELECT id FROM d)");
    }
    if term.chars().count() >= 3 {
        // The trigram index: the term as one phrase (quotes inside it doubled)
        qb.push(" AND n.rowid IN (SELECT rowid FROM nodes_fts WHERE nodes_fts MATCH ").push_bind(format!("\"{}\"", term.replace('"', "\"\""))).push(")");
    } else {
        let escaped = crate::util::like_escape(&term.to_lowercase());
        qb.push(" AND unicode_lower(n.name) LIKE ").push_bind(format!("%{escaped}%")).push(" ESCAPE '\\'");
    }
    match q.kind.as_deref() {
        Some("folder") => {
            qb.push(" AND n.kind = 'folder'");
        }
        Some("file") => {
            qb.push(" AND n.kind = 'file'");
        }
        _ => {}
    }
    let exts: Vec<String> = q.ext.as_deref().unwrap_or_default().split(',').map(|e| e.trim().trim_start_matches('.').to_lowercase()).filter(|e| !e.is_empty()).collect();
    if !exts.is_empty() {
        qb.push(" AND n.kind = 'file' AND (");
        let mut sep = qb.separated(" OR ");
        for e in exts {
            sep.push("unicode_lower(n.name) LIKE ").push_bind_unseparated(format!("%.{}", e.replace(['%', '\\'], "").replace('_', "\\_"))).push_unseparated(" ESCAPE '\\'");
        }
        qb.push(")");
    }
    if let Some(f) = q.from {
        qb.push(" AND n.updated_at >= ").push_bind(f);
    }
    if let Some(t) = q.to {
        qb.push(" AND n.updated_at < ").push_bind(t);
    }
    if let Some(m) = q.min_size {
        qb.push(" AND n.kind = 'file' AND n.size >= ").push_bind(m);
    }
    if let Some(m) = q.max_size {
        qb.push(" AND n.kind = 'file' AND n.size <= ").push_bind(m);
    }
    if let Some(o) = q.owner.as_deref().filter(|o| !o.trim().is_empty()) {
        qb.push(" AND n.owner_id = (SELECT id FROM users WHERE username = ").push_bind(o.trim().to_string()).push(")");
    }
    qb.push(" ORDER BY (n.kind = 'folder') DESC, n.updated_at DESC LIMIT ").push_bind(SEARCH_LIMIT + 1);
    let mut nodes: Vec<Node> = qb.build_query_as().fetch_all(&mut *c).await?;
    drop(c);
    let truncated = nodes.len() as i64 > SEARCH_LIMIT;
    nodes.truncate(SEARCH_LIMIT as usize);
    Ok(Json(SearchResult { items: locate(&st, &user, nodes).await?, truncated }))
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

/// Files listed in Recent
const RECENT_LIMIT: i64 = 60;
/// Opened files remembered per person
const RECENT_OPENS_KEPT: i64 = 300;
/// An open is recorded again only after this many seconds, so a video streamed in many range requests writes once
const RECENT_OPEN_INTERVAL: i64 = 60;
/// Candidates taken from each source (own files, newest edits) before the access check, so a person with many files
/// or a long history doesn't make Recent slow
const RECENT_CANDIDATES: i64 = 500;

/// Remembers that the user opened a file (for Recent): at most once a minute per file, keeping the latest few hundred
pub async fn record_open(st: &AppState, user_id: i64, node_id: &str) -> AppResult<()> {
    let at = now();
    // Most requests for the same file come close together (range requests, reloads): a read settles them without writing
    let last: Option<(i64,)> =
        sqlx::query_as("SELECT at FROM recent_files WHERE user_id = ? AND node_id = ?").bind(user_id).bind(node_id).fetch_optional(&st.db).await?;
    if last.is_some_and(|(t,)| t > at - RECENT_OPEN_INTERVAL) {
        return Ok(());
    }
    // Like every other write: a transaction that reads before writing would otherwise fail when this commits in between
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    sqlx::query("INSERT INTO recent_files (user_id, node_id, at) VALUES (?1, ?2, ?3) ON CONFLICT (user_id, node_id) DO UPDATE SET at = ?3")
        .bind(user_id)
        .bind(node_id)
        .bind(at)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "DELETE FROM recent_files WHERE user_id = ?1 AND node_id NOT IN (
           SELECT node_id FROM recent_files WHERE user_id = ?1 ORDER BY at DESC LIMIT ?2
         )",
    )
    .bind(user_id)
    .bind(RECENT_OPENS_KEPT)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Recent: files I uploaded, edited or opened, wherever they are (also files of others in shared spaces and folders),
/// newest first by the latest of those times. Only files I can still open and that aren't in the trash.
pub async fn recent(State(st): State<AppState>, user: User) -> AppResult<Json<Vec<Located>>> {
    let (drives, folders) = tree::scope(&mut *st.db.acquire().await?, &user).await?;
    let sql = format!(
        "WITH cand(id, at) AS (
           SELECT * FROM (SELECT id, updated_at FROM nodes
                          WHERE owner_id = ?3 AND kind = 'file' AND trashed_at IS NULL ORDER BY updated_at DESC LIMIT ?4)
           UNION ALL
           SELECT node_id, at FROM recent_files WHERE user_id = ?3
           UNION ALL
           SELECT * FROM (SELECT node_id, at FROM activity
                          WHERE user_id = ?3 AND action IN ('edit', 'upload') AND node_id IS NOT NULL ORDER BY id DESC LIMIT ?4)
         ),
         latest(id, at) AS (SELECT id, MAX(at) FROM cand GROUP BY id)
         SELECT {NODE_COLS} FROM latest JOIN nodes n ON n.id = latest.id
         WHERE {} AND n.kind = 'file' AND n.trashed_at IS NULL
         ORDER BY latest.at DESC LIMIT ?5",
        tree::scope_sql(1, 2)
    );
    let nodes: Vec<Node> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
        .bind(drives)
        .bind(folders)
        .bind(user.id)
        .bind(RECENT_CANDIDATES)
        .bind(RECENT_LIMIT)
        .fetch_all(&st.db)
        .await?;
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
        Json(BatchReq { ids: ids.iter().map(|s| s.to_string()).collect(), dest_id: Some(dest.to_string()), ..Default::default() })
    }

    fn ids(list: &[&str]) -> Json<BatchReq> {
        Json(BatchReq { ids: list.iter().map(|s| s.to_string()).collect(), dest_id: None, ..Default::default() })
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
                let q = ListQuery { sort: Some(sort.into()), ..Default::default() };
                let items = children(State(st), amy.clone(), Path(amy.root_id.clone()), Query(q)).await.unwrap().0.into_items();
                items.into_iter().map(|n| n.name).collect::<Vec<_>>()
            }
        };
        assert_eq!(list("name").await, ["b.pdf", "File 1.docx", "file 2.txt", "File 10.txt", "README"]);
        assert_eq!(list("type").await, ["README", "File 1.docx", "b.pdf", "file 2.txt", "File 10.txt"]);
    }

    /// Lists a folder page by page (`limit` items each) until the end
    async fn all_pages(env: &testutil::TestEnv, user: &User, folder: &str, sort: &str, order: &str, limit: i64) -> Vec<String> {
        let (mut names, mut after) = (Vec::new(), None);
        loop {
            let q = ListQuery { sort: Some(sort.into()), order: Some(order.into()), limit: Some(limit), after, ..Default::default() };
            let Json(page) = children(State(env.st.clone()), user.clone(), Path(folder.to_string()), Query(q)).await.unwrap();
            let Listing::Page { items, next } = page else { panic!("a limit gives a page") };
            assert!(items.len() as i64 <= limit);
            names.extend(items.into_iter().map(|n| n.name));
            match next {
                Some(n) => after = Some(n),
                None => return names,
            }
        }
    }

    #[tokio::test]
    async fn folders_list_page_by_page_in_every_sort_order() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        for name in ["Zeta", "alpha", "Folder 10", "Folder 9"] {
            env.folder(&amy, &amy.root_id, name).await;
        }
        // Equal sizes, times and extensions, so the ties are ordered by name and id across page boundaries
        for (i, name) in ["File 10.txt", "file 2.txt", "File 1.docx", "b.pdf", "README", "c.TXT", "d.pdf", "e", "a.docx", "f.txt", "g.png"].iter().enumerate() {
            let id = env.file(&amy, &amy.root_id, name).await;
            sqlx::query("UPDATE nodes SET size = ?, updated_at = ?, created_at = ? WHERE id = ?")
                .bind((i % 3) as i64 * 100)
                .bind(1_000 + (i % 4) as i64)
                .bind(500 + (i % 2) as i64)
                .bind(&id)
                .execute(&env.st.db)
                .await
                .unwrap();
        }
        for sort in ["name", "size", "updated", "created", "type"] {
            for order in ["asc", "desc"] {
                let q = ListQuery { sort: Some(sort.into()), order: Some(order.into()), ..Default::default() };
                let Json(whole) = children(State(env.st.clone()), amy.clone(), Path(amy.root_id.clone()), Query(q)).await.unwrap();
                let Listing::All(whole) = whole else { panic!("no limit gives the whole list") };
                let whole: Vec<String> = whole.into_iter().map(|n| n.name).collect();
                assert_eq!(whole.len(), 15);
                assert!(whole[..4].iter().all(|n| !n.contains('.') && n != "README" && n != "e"), "folders first: {whole:?}");
                for limit in [1, 2, 4, 15, 100] {
                    assert_eq!(all_pages(&env, &amy, &amy.root_id, sort, order, limit).await, whole, "{sort} {order}, {limit} per page");
                }
            }
        }
    }

    #[tokio::test]
    async fn the_next_page_starts_after_the_last_item_even_when_the_folder_changed() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        for i in 1..=6 {
            env.file(&amy, &amy.root_id, &format!("{i}.txt")).await;
        }
        let page = |after: Option<String>| {
            let (st, amy) = (env.st.clone(), amy.clone());
            async move {
                let q = ListQuery { limit: Some(3), after, ..Default::default() };
                match children(State(st), amy.clone(), Path(amy.root_id.clone()), Query(q)).await.unwrap().0 {
                    Listing::Page { items, next } => (items.into_iter().map(|n| n.name).collect::<Vec<_>>(), next),
                    Listing::All(_) => panic!("a limit gives a page"),
                }
            }
        };
        let (first, next) = page(None).await;
        assert_eq!(first, ["1.txt", "2.txt", "3.txt"]);
        // The last item of the page is renamed away and one is added before it: nothing is repeated or skipped
        let (three,): (String,) = sqlx::query_as("SELECT id FROM nodes WHERE name = '3.txt'").fetch_one(&env.st.db).await.unwrap();
        let _ = rename(State(env.st.clone()), amy.clone(), Path(three), Json(RenameReq { name: "9.txt".into() })).await.unwrap();
        env.file(&amy, &amy.root_id, "0.txt").await;
        let (second, next) = page(next).await;
        assert_eq!(second, ["4.txt", "5.txt", "6.txt"]);
        let (third, next) = page(next).await;
        assert_eq!((third, next), (vec!["9.txt".to_string()], None));

        // A cursor that wasn't made by the server
        let q = ListQuery { limit: Some(3), after: Some("not a cursor".into()), ..Default::default() };
        let err = children(State(env.st.clone()), amy.clone(), Path(amy.root_id.clone()), Query(q)).await.unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_listing_without_a_limit_is_a_plain_array() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        env.file(&amy, &amy.root_id, "a.txt").await;
        let Json(whole) = children(State(env.st.clone()), amy.clone(), Path(amy.root_id.clone()), Query(ListQuery::default())).await.unwrap();
        let v = serde_json::to_value(&whole).unwrap();
        assert_eq!((v.as_array().map(Vec::len), v[0]["name"].as_str(), v[0]["owner_name"].as_str()), (Some(1), Some("a.txt"), Some("amy")));
        let q = ListQuery { limit: Some(10), ..Default::default() };
        let Json(page) = children(State(env.st.clone()), amy.clone(), Path(amy.root_id.clone()), Query(q)).await.unwrap();
        let v = serde_json::to_value(&page).unwrap();
        assert_eq!((v["items"][0]["name"].as_str(), v["next"].is_null()), (Some("a.txt"), true));
    }

    #[tokio::test]
    async fn the_trash_lists_page_by_page_newest_first() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let mut files = Vec::new();
        for i in 0..7 {
            files.push(env.file(&amy, &amy.root_id, &format!("{i}.txt")).await);
        }
        let refs: Vec<&str> = files.iter().map(String::as_str).collect();
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&refs)).await.unwrap();
        // Deleted at different times, two of them at the same second
        for (i, id) in files.iter().enumerate() {
            sqlx::query("UPDATE nodes SET trashed_at = ? WHERE id = ?").bind(100 + (i as i64).min(5)).bind(id).execute(&env.st.db).await.unwrap();
        }
        let whole: Vec<String> =
            list_trash(State(env.st.clone()), amy.clone(), Query(TrashQuery::default())).await.unwrap().0.into_items().into_iter().map(|l| l.node.name).collect();
        assert_eq!(whole.len(), 7);
        assert_eq!(&whole[2..], ["4.txt", "3.txt", "2.txt", "1.txt", "0.txt"]);
        for limit in [1, 2, 3, 7] {
            let (mut names, mut after) = (Vec::new(), None);
            loop {
                let q = TrashQuery { limit: Some(limit), after, mine: None };
                let Json(Listing::Page { items, next }) = list_trash(State(env.st.clone()), amy.clone(), Query(q)).await.unwrap() else {
                    panic!("a limit gives a page")
                };
                names.extend(items.into_iter().map(|l| l.node.name));
                let Some(n) = next else { break };
                after = Some(n);
            }
            assert_eq!(names, whole, "{limit} per page");
        }
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
        let left = list_trash(State(env.st.clone()), bob.clone(), Query(TrashQuery::default())).await.unwrap().0.into_items();
        assert_eq!(left.len(), 1);
    }

    #[tokio::test]
    async fn copies_and_deleted_spaces_keep_content_references_right() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let team_root = {
            let mut conn = env.st.db.acquire().await.unwrap();
            let (_, root) = crate::db::create_drive(&mut conn, "Team", "team", amy.id, 0).await.unwrap();
            crate::db::add_grant(&mut conn, &root, "user", amy.id, "owner", Some(amy.id), None).await.unwrap();
            root
        };
        // Two files with the same content, one level down
        let folder = env.folder(&amy, &team_root, "Docs").await;
        let (a, b) = (env.file(&amy, &folder, "a.txt").await, env.file(&amy, &folder, "b.txt").await);
        let hash = "ab".repeat(32);
        {
            let mut c = env.st.db.acquire().await.unwrap();
            for id in [&a, &b] {
                tree::add_blob_ref(&mut c, &hash, 5, "local").await.unwrap();
                sqlx::query("UPDATE nodes SET blob_hash = ?, size = 5 WHERE id = ?").bind(&hash).bind(id).execute(&mut *c).await.unwrap();
            }
        }
        let refs = || async {
            sqlx::query_as::<_, (i64,)>("SELECT refcount FROM blobs WHERE hash = ?").bind(&hash).fetch_optional(&env.st.db).await.unwrap().map(|r| r.0)
        };
        // Copying the folder adds a reference per file, in one statement
        let _ = copy_nodes(State(env.st.clone()), amy.clone(), batch(&[&folder], &amy.root_id)).await.unwrap();
        assert_eq!(refs().await, Some(4));

        // Deleting the space removes it at once and its content in the background
        let team = env.drive_of(&team_root).await;
        let _ = crate::drives::delete(State(env.st.clone()), amy.clone(), Path(team.clone())).await.unwrap();
        for _ in 0..100 {
            let (left,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE drive_id = ?").bind(&team).fetch_one(&env.st.db).await.unwrap();
            if left == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (left,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE drive_id = ?").bind(&team).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(left, 0);
        assert_eq!(refs().await, Some(2), "the copies still use the content");
    }

    #[tokio::test]
    async fn search_finds_names_in_any_letter_case_and_filters() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let reports = env.folder(&amy, &amy.root_id, "Reports").await;
        env.file(&amy, &reports, "Été 2026.xlsx").await;
        env.file(&amy, &reports, "budget.docx").await;
        env.file(&amy, &amy.root_id, "ete-notes.txt").await;
        env.file(&amy, &amy.root_id, "AB.txt").await;
        let search = |q: &str, within: Option<&str>, ext: Option<&str>| {
            let (st, amy) = (env.st.clone(), amy.clone());
            let q = SearchQuery {
                q: q.into(),
                within: within.map(str::to_string),
                kind: None,
                ext: ext.map(str::to_string),
                from: None,
                to: None,
                min_size: None,
                max_size: None,
                owner: None,
            };
            async move {
                let Json(r) = search(State(st), amy, Query(q)).await.unwrap();
                let mut names: Vec<String> = r.items.into_iter().map(|l| l.node.name).collect();
                names.sort();
                names
            }
        };
        // Letter case and accents in any language, through the trigram index
        assert_eq!(search("ÉTÉ", None, None).await, ["ete-notes.txt", "Été 2026.xlsx"]);
        // Short terms scan, also ignoring case
        assert_eq!(search("ab", None, None).await, ["AB.txt"]);
        // Only below a folder, and by type
        assert_eq!(search("ete", Some(&reports), None).await, ["Été 2026.xlsx"]);
        assert_eq!(search("e", None, Some("docx")).await, ["budget.docx"]);
        // Renames are indexed
        let id = env.file(&amy, &amy.root_id, "old name.txt").await;
        let _ = rename(State(env.st.clone()), amy.clone(), Path(id), Json(RenameReq { name: "Quarterly.txt".into() })).await.unwrap();
        assert_eq!(search("quarter", None, None).await, ["Quarterly.txt"]);
        assert!(search("old name", None, None).await.is_empty());
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
        let listed = list_trash(State(env.st.clone()), amy.clone(), Query(TrashQuery::default())).await.unwrap().0.into_items();
        let mut names: Vec<String> = listed.into_iter().map(|l| l.node.name).collect();
        names.sort();
        assert_eq!(names, vec!["Folder", "b.txt"]);

        // Deleting for good: an item trashed on its own before its folder is listed separately; selecting both works
        let _ = restore(State(env.st.clone()), amy.clone(), ids(&[&folder])).await.unwrap();
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&doc])).await.unwrap();
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&folder])).await.unwrap();
        let listed = list_trash(State(env.st.clone()), amy.clone(), Query(TrashQuery::default())).await.unwrap().0.into_items();
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
                    let q = Query(ListQuery::default());
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
        let listed = list_trash(State(env.st.clone()), admin.clone(), Query(TrashQuery::default())).await.unwrap().0.into_items();
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

    #[tokio::test]
    async fn folder_contents_count_everything_inside_except_the_trash() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let top = env.folder(&amy, &amy.root_id, "top").await;
        let sub = env.folder(&amy, &top, "sub").await;
        let deep = env.folder(&amy, &sub, "deep").await;
        let gone = env.folder(&amy, &top, "gone").await;
        let other = env.folder(&amy, &amy.root_id, "other").await;
        for (parent, name, size) in [(&top, "a.txt", 100), (&sub, "b.txt", 20), (&deep, "c.txt", 3), (&gone, "d.txt", 4000), (&other, "e.txt", 5)] {
            let id = env.file(&amy, parent, name).await;
            sqlx::query("UPDATE nodes SET size = ? WHERE id = ?").bind(size).bind(&id).execute(&env.st.db).await.unwrap();
        }
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&gone])).await.unwrap();
        let of = |list: &[&str]| Json(ContentsReq { ids: list.iter().map(|s| s.to_string()).collect() });

        // The folder itself is not counted; the trashed folder and its file are left out
        let Json(c) = contents(State(env.st.clone()), amy.clone(), of(&[&top])).await.unwrap();
        assert_eq!(c, Contents { size: 123, files: 3, folders: 2 });
        // Several folders add up, a folder inside another selected one is counted once, and a file holds nothing
        let a = env.file(&amy, &amy.root_id, "loose.txt").await;
        let Json(c) = contents(State(env.st.clone()), amy.clone(), of(&[&top, &sub, &other, &a])).await.unwrap();
        assert_eq!(c, Contents { size: 128, files: 4, folders: 2 });
        let Json(c) = contents(State(env.st.clone()), amy.clone(), of(&[&deep])).await.unwrap();
        assert_eq!(c, Contents { size: 3, files: 1, folders: 0 });
        // Only for people who can see the folder
        let err = contents(State(env.st.clone()), ben.clone(), of(&[&top])).await.unwrap_err();
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        env.grant(&sub, &ben, "viewer").await;
        let Json(c) = contents(State(env.st.clone()), ben.clone(), of(&[&sub])).await.unwrap();
        assert_eq!(c, Contents { size: 23, files: 2, folders: 1 });
    }

    #[tokio::test]
    async fn the_trash_shows_who_deleted_each_item_and_filters_by_me() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        // A team space both are members of
        let shared = {
            let mut conn = env.st.db.acquire().await.unwrap();
            let (_, root) = crate::db::create_drive(&mut conn, "Team", "team", amy.id, 0).await.unwrap();
            crate::db::add_grant(&mut conn, &root, "user", amy.id, "owner", Some(amy.id), None).await.unwrap();
            crate::db::add_grant(&mut conn, &root, "user", ben.id, "editor", Some(amy.id), None).await.unwrap();
            root
        };
        let (mut by_amy, mut by_ben) = (Vec::new(), Vec::new());
        for i in 0..5 {
            by_amy.push(env.file(&amy, &shared, &format!("amy-{i}.txt")).await);
            by_ben.push(env.file(&amy, &shared, &format!("ben-{i}.txt")).await);
        }
        let old = env.file(&amy, &shared, "old.txt").await;
        for (who, list) in [(&amy, &by_amy), (&ben, &by_ben)] {
            let refs: Vec<&str> = list.iter().map(String::as_str).collect();
            let _ = trash(State(env.st.clone()), who.clone(), ids(&refs)).await.unwrap();
        }
        // Deleted before who deleted it was recorded
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&old])).await.unwrap();
        sqlx::query("UPDATE nodes SET trashed_by = NULL WHERE id = ?").bind(&old).execute(&env.st.db).await.unwrap();

        let all = list_trash(State(env.st.clone()), amy.clone(), Query(TrashQuery::default())).await.unwrap().0.into_items();
        assert_eq!(all.len(), 11);
        for l in &all {
            let expected = if l.node.name == "old.txt" { None } else { Some(&l.node.name[..3]) };
            assert_eq!(l.deleted_by.as_deref(), expected, "{}", l.node.name);
        }
        let unknown = all.iter().find(|l| l.node.name == "old.txt").unwrap();
        assert!(serde_json::to_value(unknown).unwrap().get("deleted_by").is_none());

        // Deleted by me, page by page
        for (who, prefix) in [(&amy, "amy-"), (&ben, "ben-")] {
            let (mut names, mut after) = (Vec::new(), None);
            loop {
                let q = TrashQuery { limit: Some(2), after, mine: Some(true) };
                let Json(Listing::Page { items, next }) = list_trash(State(env.st.clone()), who.clone(), Query(q)).await.unwrap() else {
                    panic!("a limit gives a page")
                };
                names.extend(items.into_iter().map(|l| l.node.name));
                let Some(n) = next else { break };
                after = Some(n);
            }
            names.sort();
            assert_eq!(names, (0..5).map(|i| format!("{prefix}{i}.txt")).collect::<Vec<_>>());
        }

        // Restoring forgets it; deleting again records the new person
        let _ = restore(State(env.st.clone()), amy.clone(), ids(&[&by_ben[0]])).await.unwrap();
        let (by,): (Option<i64>,) = sqlx::query_as("SELECT trashed_by FROM nodes WHERE id = ?").bind(&by_ben[0]).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(by, None);
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&by_ben[0]])).await.unwrap();
        let mine = list_trash(State(env.st.clone()), amy.clone(), Query(TrashQuery { mine: Some(true), ..Default::default() })).await.unwrap().0.into_items();
        assert!(mine.iter().any(|l| l.node.id == by_ben[0]));
        assert_eq!(mine.len(), 6);
    }

    #[tokio::test]
    async fn recent_lists_files_i_opened_or_edited_in_shared_spaces_too() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let team = {
            let mut conn = env.st.db.acquire().await.unwrap();
            let (_, root) = crate::db::create_drive(&mut conn, "Team", "team", amy.id, 0).await.unwrap();
            crate::db::add_grant(&mut conn, &root, "user", amy.id, "owner", Some(amy.id), None).await.unwrap();
            crate::db::add_grant(&mut conn, &root, "user", ben.id, "editor", Some(amy.id), None).await.unwrap();
            root
        };
        let t = now();
        let mine = env.file(&ben, &ben.root_id, "mine.txt").await;
        let opened = env.file(&amy, &team, "opened.txt").await;
        let edited = env.file(&amy, &team, "edited.txt").await;
        let _untouched = env.file(&amy, &team, "untouched.txt").await;
        sqlx::query("UPDATE nodes SET updated_at = ?").bind(t - 1000).execute(&env.st.db).await.unwrap();
        sqlx::query("UPDATE nodes SET updated_at = ? WHERE id = ?").bind(t - 300).bind(&mine).execute(&env.st.db).await.unwrap();
        record_open(&env.st, ben.id, &opened).await.unwrap();
        sqlx::query("UPDATE recent_files SET at = ?").bind(t - 100).execute(&env.st.db).await.unwrap();
        {
            let mut c = env.st.db.acquire().await.unwrap();
            let node = tree::get_node(&mut c, &edited).await.unwrap().unwrap();
            logs::record_activity(&mut c, &ben, Some(&node), "edit", "").await.unwrap();
        }
        sqlx::query("UPDATE activity SET at = ?").bind(t - 200).execute(&env.st.db).await.unwrap();
        let names = |who: User| {
            let st = env.st.clone();
            async move { recent(State(st), who).await.unwrap().0.into_iter().map(|l| l.node.name).collect::<Vec<_>>() }
        };

        // Newest first by the latest of: my own file's change, my open, my edit
        assert_eq!(names(ben.clone()).await, ["opened.txt", "edited.txt", "mine.txt"]);
        // Amy's own files are in hers; Ben opening them doesn't put them in hers
        assert!(!names(amy.clone()).await.contains(&"mine.txt".to_string()));

        // Opening again within a minute doesn't write; later it moves the file up
        let at = || async {
            let (at,): (i64,) = sqlx::query_as("SELECT at FROM recent_files WHERE user_id = ? AND node_id = ?").bind(ben.id).bind(&opened).fetch_one(&env.st.db).await.unwrap();
            at
        };
        sqlx::query("UPDATE recent_files SET at = ?").bind(t - 30).execute(&env.st.db).await.unwrap();
        record_open(&env.st, ben.id, &opened).await.unwrap();
        assert_eq!(at().await, t - 30);
        sqlx::query("UPDATE recent_files SET at = ?").bind(t - 120).execute(&env.st.db).await.unwrap();
        record_open(&env.st, ben.id, &opened).await.unwrap();
        assert!(at().await >= t);

        // Only files still reachable and not in the trash
        let _ = trash(State(env.st.clone()), ben.clone(), ids(&[&edited])).await.unwrap();
        assert_eq!(names(ben.clone()).await, ["opened.txt", "mine.txt"]);
        env.revoke(&team, &ben).await;
        assert_eq!(names(ben.clone()).await, ["mine.txt"]);

        // Only the latest few hundred opens are kept per person
        for i in 0..RECENT_OPENS_KEPT + 5 {
            let id = env.file(&ben, &ben.root_id, &format!("{i}.txt")).await;
            sqlx::query("INSERT INTO recent_files (user_id, node_id, at) VALUES (?, ?, ?)").bind(ben.id).bind(&id).bind(i).execute(&env.st.db).await.unwrap();
        }
        record_open(&env.st, ben.id, &mine).await.unwrap();
        let (kept, oldest): (i64, i64) =
            sqlx::query_as("SELECT COUNT(*), MIN(at) FROM recent_files WHERE user_id = ?").bind(ben.id).fetch_one(&env.st.db).await.unwrap();
        // With opened.txt and mine.txt, the 7 oldest go
        assert_eq!((kept, oldest), (RECENT_OPENS_KEPT, 7));
        // Deleting a file for good forgets it
        let _ = trash(State(env.st.clone()), ben.clone(), ids(&[&mine])).await.unwrap();
        let _ = delete_forever(State(env.st.clone()), ben.clone(), ids(&[&mine])).await.unwrap();
        let (left,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM recent_files WHERE node_id = ?").bind(&mine).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(left, 0);
    }

    fn resolved(ids: &[&str], dest: Option<&str>, answer: Resolution) -> Json<BatchReq> {
        Json(BatchReq {
            ids: ids.iter().map(|s| s.to_string()).collect(),
            dest_id: dest.map(str::to_string),
            resolutions: ids.iter().map(|s| (s.to_string(), answer)).collect(),
        })
    }

    async fn name_of(env: &testutil::TestEnv, id: &str) -> (String, Option<String>, bool) {
        let n = tree::get_node(&mut env.st.db.acquire().await.unwrap(), id).await.unwrap().unwrap();
        (n.name, n.parent_id, n.trashed_at.is_some())
    }

    #[tokio::test]
    async fn moving_onto_a_taken_name_asks_and_then_replaces_skips_or_keeps_both() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let st = || State(env.st.clone());
        let dest = env.folder(&amy, &amy.root_id, "Dest").await;
        let there = env.file(&amy, &dest, "Report.docx").await;
        let src = env.folder(&amy, &amy.root_id, "Src").await;
        let a = env.file(&amy, &src, "report.docx").await;

        // The browser learns about the clash first
        let Json(found) = conflicts(st(), amy.clone(), Json(ConflictsReq { dest_id: Some(dest.clone()), names: vec!["REPORT.docx".into(), "new.txt".into()], ids: vec![a.clone()] }))
            .await
            .unwrap();
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|c| c.existing.id == there));
        assert_eq!((found[0].id.as_deref(), found[1].id.as_deref()), (None, Some(a.as_str())));

        // Without an answer the move fails as before; skipped, nothing moves
        assert_eq!(move_nodes(st(), amy.clone(), batch(&[&a], &dest)).await.unwrap_err().status, StatusCode::CONFLICT);
        let _ = move_nodes(st(), amy.clone(), resolved(&[&a], Some(&dest), Resolution::Skip)).await.unwrap();
        assert_eq!(name_of(&env, &a).await.1.as_deref(), Some(src.as_str()));
        // Both kept: the moved one gets a number
        let _ = move_nodes(st(), amy.clone(), resolved(&[&a], Some(&dest), Resolution::Keep)).await.unwrap();
        assert_eq!(name_of(&env, &a).await, ("report (1).docx".into(), Some(dest.clone()), false));

        // Replaced: the item there goes to the trash, the moved one takes its place under its own name
        let b = env.file(&amy, &src, "Report.docx").await;
        let _ = move_nodes(st(), amy.clone(), resolved(&[&b], Some(&dest), Resolution::Replace)).await.unwrap();
        assert_eq!(name_of(&env, &b).await, ("Report.docx".into(), Some(dest.clone()), false));
        assert!(name_of(&env, &there).await.2, "the replaced file is in the trash");

        // A folder can't be replaced by something inside it
        let outer = env.folder(&amy, &amy.root_id, "Box").await;
        let inner = env.folder(&amy, &outer, "Box").await;
        let err = move_nodes(st(), amy.clone(), resolved(&[&inner], Some(&amy.root_id), Resolution::Replace)).await.unwrap_err();
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert!(!name_of(&env, &outer).await.2);
    }

    #[tokio::test]
    async fn copying_and_restoring_onto_a_taken_name_follow_the_answer() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let st = || State(env.st.clone());
        let dest = env.folder(&amy, &amy.root_id, "Dest").await;
        let there = env.file(&amy, &dest, "a.txt").await;
        let a = env.file(&amy, &amy.root_id, "a.txt").await;
        let count = |parent: String| {
            let db = env.st.db.clone();
            async move {
                let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE parent_id = ? AND trashed_at IS NULL").bind(parent).fetch_one(&db).await.unwrap();
                n
            }
        };
        let _ = copy_nodes(st(), amy.clone(), resolved(&[&a], Some(&dest), Resolution::Skip)).await.unwrap();
        assert_eq!(count(dest.clone()).await, 1);
        // Without an answer a copy gets a number, as it always did
        let _ = copy_nodes(st(), amy.clone(), batch(&[&a], &dest)).await.unwrap();
        assert_eq!(count(dest.clone()).await, 2);
        let _ = copy_nodes(st(), amy.clone(), resolved(&[&a], Some(&dest), Resolution::Replace)).await.unwrap();
        assert_eq!(count(dest.clone()).await, 2);
        assert!(name_of(&env, &there).await.2);

        // Restoring: another "a.txt" took the name meanwhile
        let _ = trash(st(), amy.clone(), ids(&[&a])).await.unwrap();
        let newer = env.file(&amy, &amy.root_id, "a.txt").await;
        let Json(found) = conflicts(st(), amy.clone(), Json(ConflictsReq { dest_id: None, names: vec![], ids: vec![a.clone()] })).await.unwrap();
        assert_eq!(found.iter().map(|c| c.existing.id.as_str()).collect::<Vec<_>>(), [newer.as_str()]);
        let _ = restore(st(), amy.clone(), resolved(&[&a], None, Resolution::Skip)).await.unwrap();
        assert!(name_of(&env, &a).await.2, "skipped: still in the trash");
        let _ = restore(st(), amy.clone(), resolved(&[&a], None, Resolution::Replace)).await.unwrap();
        assert_eq!(name_of(&env, &a).await, ("a.txt".into(), Some(amy.root_id.clone()), false));
        assert!(name_of(&env, &newer).await.2);
    }

    #[tokio::test]
    async fn replacing_in_a_folder_space_moves_the_item_there_to_its_trash() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        testutil::write_old(&space.dir.join("a.txt"), b"old");
        testutil::write_old(&space.dir.join("Sub/a.txt"), b"new");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (old, _) = env.node_at(&space.drive, "a.txt").await.unwrap();
        let (new, _) = env.node_at(&space.drive, "Sub/a.txt").await.unwrap();
        let _ = move_nodes(State(env.st.clone()), admin.clone(), resolved(&[&new], Some(&space.root), Resolution::Replace)).await.unwrap();
        assert_eq!(std::fs::read(space.dir.join("a.txt")).unwrap(), b"new");
        assert_eq!(env.node_at(&space.drive, "a.txt").await.unwrap().0, new);
        assert!(name_of(&env, &old).await.2);
        assert!(!space.dir.join("Sub/a.txt").exists());
    }

    #[tokio::test]
    async fn old_trash_is_purged_in_batches_with_its_content_references() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let (shared, alone) = ("ab".repeat(32), "cd".repeat(32));
        // A file with the given content (5 bytes each)
        let file = async |parent: &str, name: String, hash: &str| {
            let id = env.file(&amy, parent, &name).await;
            let mut c = env.st.db.acquire().await.unwrap();
            tree::add_blob_ref(&mut c, hash, 5, "local").await.unwrap();
            sqlx::query("UPDATE nodes SET blob_hash = ?, size = 5 WHERE id = ?").bind(hash).bind(&id).execute(&mut *c).await.unwrap();
            id
        };
        // More old items than one batch takes: files, and a folder with files inside
        let mut old = Vec::new();
        for i in 0..130 {
            old.push(file(&amy.root_id, format!("old{i}.txt"), &shared).await);
        }
        let folder = env.folder(&amy, &amy.root_id, "Old folder").await;
        for i in 0..3 {
            file(&folder, format!("inner{i}.txt"), &alone).await;
        }
        old.push(folder);
        let recent = file(&amy.root_id, "recent.txt".into(), &shared).await;
        let live = file(&amy.root_id, "live.txt".into(), &shared).await;
        tree::recompute_usage(&env.st).await.unwrap();
        let refs: Vec<&str> = old.iter().map(String::as_str).collect();
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&refs)).await.unwrap();
        sqlx::query("UPDATE nodes SET trashed_at = trashed_at - 40 * 86400 WHERE trashed_at IS NOT NULL").execute(&env.st.db).await.unwrap();
        let _ = trash(State(env.st.clone()), amy.clone(), ids(&[&recent])).await.unwrap();

        assert_eq!(purge_expired_trash(&env.st, 30).await.unwrap(), 131);
        let drive = env.drive_of(&amy.root_id).await;
        let (left,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE drive_id = ? AND parent_id IS NOT NULL").bind(&drive).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(left, 2, "only the recent item in the trash and the live file are left");
        assert!(name_of(&env, &recent).await.2 && !name_of(&env, &live).await.2);
        // The content still used keeps its two references; the other one is no longer recorded
        let refcount = "SELECT refcount FROM blobs WHERE hash = ?";
        let shared_refs: Option<(i64,)> = sqlx::query_as(refcount).bind(&shared).fetch_optional(&env.st.db).await.unwrap();
        let alone_refs: Option<(i64,)> = sqlx::query_as(refcount).bind(&alone).fetch_optional(&env.st.db).await.unwrap();
        assert_eq!((shared_refs, alone_refs), (Some((2,)), None));
        let (used,): (i64,) = sqlx::query_as("SELECT used_bytes FROM drives WHERE id = ?").bind(&drive).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(used, 10);
        tree::recompute_usage(&env.st).await.unwrap();
        let (again,): (i64,) = sqlx::query_as("SELECT used_bytes FROM drives WHERE id = ?").bind(&drive).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(again, used, "the counter agrees with the files");
        // Nothing more is old enough
        assert_eq!(purge_expired_trash(&env.st, 30).await.unwrap(), 0);
    }
}
