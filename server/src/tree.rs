//! Shared file tree logic: node queries, permissions (space and folder grants), ancestors/subtrees, name conflicts, blob reference counting, quotas.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};

use crate::{
    auth::User,
    error::{AppError, AppResult},
    state::AppState,
    util::{now, numbered_name},
};

pub const NODE_COLS: &str =
    "n.id, n.owner_id, n.parent_id, n.kind, n.name, n.blob_hash, n.size, n.mime, n.created_at, n.updated_at, n.trashed_at, n.drive_id,
     COALESCE((SELECT username FROM users WHERE id = n.owner_id), '') AS owner_name,
     (SELECT location_id FROM blobs WHERE hash = n.blob_hash) AS blob_location,
     n.fs_path, (SELECT source_path FROM drives WHERE id = n.drive_id AND mode = 'folder') AS fs_root,
     (SELECT read_only FROM drives WHERE id = n.drive_id) AS space_read_only";

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Node {
    pub id: String,
    #[serde(skip)]
    #[allow(dead_code)]
    pub owner_id: i64,
    pub parent_id: Option<String>,
    pub kind: String,
    pub name: String,
    #[serde(skip)]
    pub blob_hash: Option<String>,
    /// Storage location of the physical file
    #[serde(skip)]
    pub blob_location: Option<String>,
    pub size: i64,
    pub mime: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub trashed_at: Option<i64>,
    /// The space it belongs to
    pub drive_id: Option<String>,
    /// Uploader / creator
    pub owner_name: String,
    /// Whether the current user has favorited it; filled in by mark_favorites
    #[sqlx(default)]
    pub is_favorite: bool,
    /// Folder spaces: the path below the space's folder
    #[serde(skip)]
    #[sqlx(default)]
    pub fs_path: Option<String>,
    /// Folder spaces: the space's folder on the server
    #[serde(skip)]
    #[sqlx(default)]
    pub fs_root: Option<String>,
    /// The space is read-only: browse, download and share only
    #[serde(skip)]
    #[sqlx(default)]
    pub space_read_only: bool,
}

impl Node {
    pub fn is_folder(&self) -> bool {
        self.kind == "folder"
    }
    pub fn hash(&self) -> AppResult<&str> {
        self.blob_hash.as_deref().ok_or_else(|| AppError::bad_request("This isn't a file"))
    }
    /// (hash, storage location) of the file content
    pub fn blob(&self) -> AppResult<(&str, &str)> {
        Ok((self.hash()?, self.blob_location.as_deref().unwrap_or("local")))
    }
    pub fn drive(&self) -> &str {
        self.drive_id.as_deref().unwrap_or_default()
    }
    /// The file on the server, for items of a folder space
    pub fn fs_file(&self) -> Option<std::path::PathBuf> {
        let (root, rel) = (self.fs_root.as_deref()?, self.fs_path.as_deref()?);
        // Paths come from scanning the folder; never step outside it whatever they say
        if rel.split('/').any(|part| part == ".." || part == "." ) || rel.starts_with('/') {
            return None;
        }
        Some(if rel.is_empty() { std::path::PathBuf::from(root) } else { std::path::Path::new(root).join(rel) })
    }
    /// Whether it belongs to a folder space (changed on the server's folder, not through the content store)
    pub fn in_folder_space(&self) -> bool {
        self.fs_root.is_some()
    }
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Crumb {
    pub id: String,
    pub name: String,
}

// ───────────── Roles and permissions ─────────────

/// Role on a space or folder, from lowest to highest
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Viewer,
    Editor,
    Manager,
    Owner,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "viewer" => Some(Role::Viewer),
            "editor" => Some(Role::Editor),
            "manager" => Some(Role::Manager),
            "owner" => Some(Role::Owner),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Editor => "editor",
            Role::Manager => "manager",
            Role::Owner => "owner",
        }
    }
}

/// Capability an operation requires
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    Read,
    /// Upload, create, rename, move, edit
    Write,
    /// Move to trash, delete permanently
    Delete,
    /// Create public share links
    Share,
}

/// Effective capability = role ∩ the user account's own permissions
pub fn allows(user: &User, role: Role, need: Need) -> AppResult<()> {
    let ok = match need {
        Need::Read => true,
        Need::Write => role >= Role::Editor && (user.can_write || user.is_admin()),
        Need::Delete => role >= Role::Editor && (user.can_delete || user.is_admin()),
        Need::Share => role >= Role::Editor && (user.can_share || user.is_admin()),
    };
    if ok {
        return Ok(());
    }
    Err(AppError::forbidden(match need {
        Need::Read => "You don't have access",
        Need::Write => "You don't have permission to make changes here",
        Need::Delete => "You don't have permission to delete here",
        Need::Share => "You don't have permission to share here",
    }))
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Drive {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub root_id: String,
    pub owner_id: Option<i64>,
    pub quota_bytes: i64,
    pub disabled: bool,
    /// Bytes of all file nodes in the space, including the trash (kept up to date by `adjust_usage`)
    pub used_bytes: i64,
    /// "store" (content store) or "folder" (a folder on the server)
    pub mode: String,
    /// Folder spaces: the folder
    pub source_path: Option<String>,
    /// Browse, download and share only
    pub read_only: bool,
}

impl Drive {
    pub fn is_folder(&self) -> bool {
        self.mode == "folder"
    }
}

pub const DRIVE_COLS: &str = "d.id, d.name, d.kind, d.root_id, d.owner_id, d.quota_bytes, d.disabled, d.used_bytes, d.mode, d.source_path, d.read_only";

/// The grant's principal matches the current user (?2 = user id, ?3 = current time)
const PRINCIPAL_MATCH: &str = "(g.expires_at IS NULL OR g.expires_at > ?3)
    AND (g.principal_type = 'everyone'
      OR (g.principal_type = 'user' AND g.principal_id = ?2)
      OR (g.principal_type = 'group' AND g.principal_id IN (SELECT group_id FROM group_members WHERE user_id = ?2)))";

pub async fn get_drive(conn: &mut SqliteConnection, id: &str) -> AppResult<Option<Drive>> {
    let sql = format!("SELECT {DRIVE_COLS} FROM drives d WHERE d.id = ?");
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(id).fetch_optional(conn).await?)
}

/// The user's role on a node: the highest grant among all ancestors (including itself); None means no access
pub async fn role_on(conn: &mut SqliteConnection, user: &User, node: &Node) -> AppResult<Option<Role>> {
    let Some(drive) = get_drive(conn, node.drive()).await? else { return Ok(None) };
    if drive.disabled {
        return Ok(None);
    }
    // Administrators manage the company shared space (personal spaces are unaffected, for privacy)
    let mut best = (drive.kind == "company" && user.is_admin()).then_some(Role::Manager);
    let sql = format!(
        "WITH RECURSIVE up(id, parent_id) AS (
           SELECT id, parent_id FROM nodes WHERE id = ?1
           UNION ALL SELECT n.id, n.parent_id FROM nodes n JOIN up ON n.id = up.parent_id
         )
         SELECT g.role FROM grants g JOIN up ON g.node_id = up.id WHERE {PRINCIPAL_MATCH}"
    );
    let rows: Vec<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(&node.id).bind(user.id).bind(now()).fetch_all(conn).await?;
    for (r,) in rows {
        best = best.max(Role::parse(&r));
    }
    Ok(best)
}

pub fn resolve_alias<'a>(user: &'a User, id: &'a str) -> AppResult<&'a str> {
    Ok(match id {
        "root" => user.root_id.as_str(),
        "shared" => user.shared_root.as_deref().ok_or_else(|| AppError::not_found("The shared space isn't enabled"))?,
        _ => id,
    })
}

pub async fn get_node(conn: &mut SqliteConnection, id: &str) -> AppResult<Option<Node>> {
    let sql = format!("SELECT {NODE_COLS} FROM nodes n WHERE n.id = ?");
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(id).fetch_optional(conn).await?)
}

/// Gets a node the user can access that isn't in the trash, together with the user's role.
/// `root` means the user's own personal space, `shared` means the "All files" company space.
pub async fn node_with_role(conn: &mut SqliteConnection, user: &User, id: &str) -> AppResult<(Node, Role)> {
    let id = resolve_alias(user, id)?;
    if let Some(n) = get_node(conn, id).await?
        && n.trashed_at.is_none()
            && let Some(role) = role_on(conn, user, &n).await? {
                return Ok((n, role));
            }
    Err(AppError::not_found("Item not found"))
}

/// Gets a node and checks the required capability
pub async fn node_for(conn: &mut SqliteConnection, user: &User, id: &str, need: Need) -> AppResult<Node> {
    let (n, role) = node_with_role(conn, user, id).await?;
    allows(user, role, need)?;
    if matches!(need, Need::Write | Need::Delete) && n.space_read_only {
        return Err(read_only_space());
    }
    Ok(n)
}

/// A read-only space can be browsed, downloaded and shared
pub fn read_only_space() -> AppError {
    AppError::forbidden("This space is read-only")
}

pub async fn folder_for(conn: &mut SqliteConnection, user: &User, id: &str, need: Need) -> AppResult<Node> {
    let n = node_for(conn, user, id, need).await?;
    if !n.is_folder() {
        return Err(AppError::bad_request("The destination isn't a folder"));
    }
    Ok(n)
}

/// Read access is enough
pub async fn owned_node(conn: &mut SqliteConnection, user: &User, id: &str) -> AppResult<Node> {
    node_for(conn, user, id, Need::Read).await
}

/// Spaces the user can access (space-level grants) and their roles
pub async fn user_drives(conn: &mut SqliteConnection, user: &User) -> AppResult<Vec<(Drive, Role)>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        drive: Drive,
        role: String,
    }
    let sql = format!(
        "SELECT {DRIVE_COLS}, g.role FROM drives d JOIN grants g ON g.node_id = d.root_id
         WHERE d.disabled = 0 AND ?1 = ?1 AND {PRINCIPAL_MATCH}"
    );
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(0).bind(user.id).bind(now()).fetch_all(&mut *conn).await?;
    let mut map: HashMap<String, (Drive, Role)> = HashMap::new();
    for r in rows {
        let Some(role) = Role::parse(&r.role) else { continue };
        map.entry(r.drive.id.clone()).and_modify(|e| e.1 = e.1.max(role)).or_insert((r.drive, role));
    }
    if user.is_admin() {
        let sql = format!("SELECT {DRIVE_COLS} FROM drives d WHERE d.kind = 'company' AND d.disabled = 0");
        let company: Vec<Drive> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).fetch_all(&mut *conn).await?;
        for d in company {
            map.entry(d.id.clone()).and_modify(|e| e.1 = e.1.max(Role::Manager)).or_insert((d, Role::Manager));
        }
    }
    let mut out: Vec<(Drive, Role)> = map.into_values().collect();
    let rank = |k: &str| match k {
        "personal" => 0,
        "company" => 1,
        _ => 2,
    };
    out.sort_by(|a, b| rank(&a.0.kind).cmp(&rank(&b.0.kind)).then_with(|| a.0.name.cmp(&b.0.name)));
    Ok(out)
}

/// Folders / files others shared with me (excluding items in spaces I'm already a member of): (node, role, sharer)
pub async fn shared_with_me(conn: &mut SqliteConnection, user: &User) -> AppResult<Vec<(Node, Role, String)>> {
    let member_of: Vec<String> = user_drives(conn, user).await?.into_iter().map(|(d, _)| d.id).collect();
    shared_with_me_outside(conn, user, &member_of).await
}

/// Same as `shared_with_me`, for callers that already have the user's space list (saves the grants query)
pub async fn shared_with_me_outside(conn: &mut SqliteConnection, user: &User, member_of: &[String]) -> AppResult<Vec<(Node, Role, String)>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        node: Node,
        role: String,
        sharer: String,
    }
    let sql = format!(
        "SELECT {NODE_COLS}, g.role, COALESCE((SELECT username FROM users WHERE id = g.granted_by), '') AS sharer
         FROM grants g JOIN nodes n ON n.id = g.node_id JOIN drives d ON d.id = n.drive_id
         WHERE n.parent_id IS NOT NULL AND n.trashed_at IS NULL AND d.disabled = 0 AND ?1 = ?1 AND {PRINCIPAL_MATCH}
         ORDER BY g.created_at DESC"
    );
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(0).bind(user.id).bind(now()).fetch_all(&mut *conn).await?;
    let mut map: HashMap<String, (Node, Role, String)> = HashMap::new();
    for r in rows {
        let Some(role) = Role::parse(&r.role) else { continue };
        if member_of.iter().any(|d| d == r.node.drive()) {
            continue;
        }
        map.entry(r.node.id.clone()).and_modify(|e| e.1 = e.1.max(role)).or_insert((r.node, role, r.sharer));
    }
    Ok(map.into_values().collect())
}

/// Builds the SQL condition "node is within the user's accessible scope"; the bound values are two JSON arrays (space ids, shared folder ids)
pub fn scope_sql(drives_param: usize, folders_param: usize) -> String {
    format!(
        "(n.drive_id IN (SELECT value FROM json_each(?{drives_param}))
          OR n.id IN (WITH RECURSIVE s(id) AS (
                SELECT value FROM json_each(?{folders_param})
                UNION ALL SELECT c.id FROM nodes c JOIN s ON c.parent_id = s.id
             ) SELECT id FROM s))"
    )
}

/// The user's accessible scope: (space id JSON, shared folder id JSON)
pub async fn scope(conn: &mut SqliteConnection, user: &User) -> AppResult<(String, String)> {
    let drives: Vec<String> = user_drives(conn, user).await?.into_iter().map(|(d, _)| d.id).collect();
    let folders: Vec<String> = shared_with_me_outside(conn, user, &drives).await?.into_iter().map(|(n, _, _)| n.id).collect();
    Ok((serde_json::to_string(&drives).unwrap(), serde_json::to_string(&folders).unwrap()))
}

/// Fills in the current user's favorite status.
/// Takes the caller's connection: taking a second one from the pool while holding one can use up the pool under load.
pub async fn mark_favorites<'a>(conn: &mut SqliteConnection, user_id: i64, nodes: impl IntoIterator<Item = &'a mut Node>) -> AppResult<()> {
    let favs: Vec<(String,)> = sqlx::query_as("SELECT node_id FROM favorites WHERE user_id = ?").bind(user_id).fetch_all(&mut *conn).await?;
    let favs: std::collections::HashSet<String> = favs.into_iter().map(|(id,)| id).collect();
    for n in nodes {
        n.is_favorite = favs.contains(&n.id);
    }
    Ok(())
}

/// Records an activity
pub async fn log(conn: &mut SqliteConnection, user: &User, node: Option<&Node>, action: &str, detail: &str) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO activity (at, user_id, username, drive_id, node_id, node_name, action, detail) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(now())
    .bind(user.id)
    .bind(&user.username)
    .bind(node.and_then(|n| n.drive_id.clone()))
    .bind(node.map(|n| n.id.clone()))
    .bind(node.map(|n| n.name.clone()).unwrap_or_default())
    .bind(action)
    .bind(detail)
    .execute(conn)
    .await?;
    Ok(())
}

/// Path from the root to the node (inclusive); the root folder itself isn't included.
pub async fn path_of(conn: &mut SqliteConnection, id: &str) -> AppResult<Vec<Crumb>> {
    Ok(sqlx::query_as(
        "WITH RECURSIVE up(id, parent_id, name, depth) AS (
           SELECT id, parent_id, name, 0 FROM nodes WHERE id = ?1
           UNION ALL
           SELECT n.id, n.parent_id, n.name, up.depth + 1 FROM nodes n JOIN up ON n.id = up.parent_id
         )
         SELECT id, name FROM up WHERE parent_id IS NOT NULL ORDER BY depth DESC",
    )
    .bind(id)
    .fetch_all(conn)
    .await?)
}

/// Paths of several nodes in one query (see `path_of`): node id → crumbs from the space root (excluded) to the node (included).
/// Used by listings that show a location for every row (search, recent, favorites, trash), instead of one query per row.
pub async fn paths_of(conn: &mut SqliteConnection, ids: &[String]) -> AppResult<HashMap<String, Vec<Crumb>>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        start: String,
        id: String,
        name: String,
    }
    let mut out: HashMap<String, Vec<Crumb>> = HashMap::with_capacity(ids.len());
    if ids.is_empty() {
        return Ok(out);
    }
    let rows: Vec<Row> = sqlx::query_as(
        "WITH RECURSIVE up(start, id, parent_id, name, depth) AS (
           SELECT id, id, parent_id, name, 0 FROM nodes WHERE id IN (SELECT value FROM json_each(?1))
           UNION ALL
           SELECT up.start, n.id, n.parent_id, n.name, up.depth + 1 FROM nodes n JOIN up ON n.id = up.parent_id
         )
         SELECT start, id, name FROM up WHERE parent_id IS NOT NULL ORDER BY start, depth DESC",
    )
    .bind(serde_json::to_string(ids).unwrap())
    .fetch_all(conn)
    .await?;
    for r in rows {
        out.entry(r.start).or_default().push(Crumb { id: r.id, name: r.name });
    }
    Ok(out)
}

/// Whether `ancestor` is `id` itself or one of its ancestors
pub async fn is_within(conn: &mut SqliteConnection, id: &str, ancestor: &str) -> AppResult<bool> {
    let row: Option<(i64,)> = sqlx::query_as(
        "WITH RECURSIVE up(id, parent_id) AS (
           SELECT id, parent_id FROM nodes WHERE id = ?1
           UNION ALL
           SELECT n.id, n.parent_id FROM nodes n JOIN up ON n.id = up.parent_id
         )
         SELECT 1 FROM up WHERE id = ?2 LIMIT 1",
    )
    .bind(id)
    .bind(ancestor)
    .fetch_optional(conn)
    .await?;
    Ok(row.is_some())
}

/// The whole subtree (including itself and children already in the trash), sorted by depth: parents always come before their children.
pub async fn subtree(conn: &mut SqliteConnection, id: &str) -> AppResult<Vec<(Node, i64)>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        node: Node,
        depth: i64,
    }
    let sql = format!(
        "WITH RECURSIVE sub(id, depth) AS (
           SELECT ?1, 0
           UNION ALL
           SELECT c.id, sub.depth + 1 FROM nodes c JOIN sub ON c.parent_id = sub.id
         )
         SELECT {NODE_COLS}, sub.depth FROM sub JOIN nodes n ON n.id = sub.id ORDER BY sub.depth"
    );
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(id).fetch_all(conn).await?;
    Ok(rows.into_iter().map(|r| (r.node, r.depth)).collect())
}

/// Whether the folder has an item with this name: regardless of letter case in the content store, exactly in folder
/// spaces (a folder on disk can hold both "A.txt" and "a.txt")
pub async fn name_taken(conn: &mut SqliteConnection, parent_id: &str, name: &str) -> AppResult<bool> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM nodes WHERE parent_id = ?1 AND name_key = CASE WHEN fs_path IS NULL THEN unicode_lower(?2) ELSE ?2 END AND trashed_at IS NULL LIMIT 1",
    )
    .bind(parent_id)
    .bind(name)
    .fetch_optional(conn)
    .await?;
    Ok(row.is_some())
}

/// The items of a folder (not in the trash) that have one of these names, matched the way `name_taken` matches:
/// (the name asked about, the item already there)
pub async fn find_children(conn: &mut SqliteConnection, parent_id: &str, names: &[String]) -> AppResult<Vec<(String, Node)>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        asked: String,
        #[sqlx(flatten)]
        node: Node,
    }
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT j.value AS asked, {NODE_COLS} FROM json_each(?2) j
         JOIN nodes n ON n.parent_id = ?1 AND n.trashed_at IS NULL
                     AND n.name_key = CASE WHEN n.fs_path IS NULL THEN unicode_lower(j.value) ELSE j.value END
         ORDER BY j.key"
    );
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
        .bind(parent_id)
        .bind(serde_json::to_string(names).unwrap())
        .fetch_all(conn)
        .await?;
    Ok(rows.into_iter().map(|r| (r.asked, r.node)).collect())
}

/// The item of a folder (not in the trash) that has this name
pub async fn find_child(conn: &mut SqliteConnection, parent_id: &str, name: &str) -> AppResult<Option<Node>> {
    Ok(find_children(conn, parent_id, &[name.to_string()]).await?.into_iter().next().map(|(_, n)| n))
}

/// Automatically adds a number on name conflicts
pub async fn unique_name(conn: &mut SqliteConnection, parent_id: &str, name: &str, is_folder: bool) -> AppResult<String> {
    if !name_taken(conn, parent_id, name).await? {
        return Ok(name.to_string());
    }
    // One query for every "name (n)" already there, then the lowest free number is picked in memory
    let pattern = {
        let (stem, ext) = crate::util::split_name(name, is_folder);
        format!("{} (%){}", crate::util::like_escape(stem), crate::util::like_escape(ext))
    };
    // `LIKE` alone only ignores the case of A–Z; the name key is lower case in every language (exact in folder spaces,
    // where this finds more names than needed, which only skips numbers)
    let taken: Vec<(String,)> = sqlx::query_as(
        "SELECT name FROM nodes WHERE parent_id = ? AND name_key LIKE unicode_lower(?) ESCAPE '\\' AND trashed_at IS NULL",
    )
        .bind(parent_id)
        .bind(pattern)
        .fetch_all(conn)
        .await?;
    let taken: std::collections::HashSet<String> = taken.into_iter().map(|(n,)| n.to_lowercase()).collect();
    (1..10_000)
        .map(|n| numbered_name(name, n, is_folder))
        .find(|candidate| !taken.contains(&candidate.to_lowercase()))
        .ok_or_else(|| AppError::conflict("Too many items with the same name"))
}

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

/// Deletes physical files and thumbnails that are no longer referenced in the background (doesn't block the caller or hold the global write lock while calling the storage service).
/// Each file is re-checked under the write lock before deletion: it's kept if it has been referenced again or someone is uploading the same content.
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
        let still_used = {
            let _w = st.write_lock.lock().await;
            let current: Option<(String,)> = match sqlx::query_as("SELECT location_id FROM blobs WHERE hash = ?").bind(&hash).fetch_optional(&st.db).await
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("Failed to check whether physical file {hash} is still referenced: {e}");
                    continue;
                }
            };
            if current.as_ref().is_some_and(|(loc,)| *loc == location) {
                // The content is in use again (e.g. the same file was re-uploaded), so it no longer needs deleting
                let _ = forget_pending(st, &hash, &location).await;
                continue;
            }
            let mut g = st.blob_guard.lock().unwrap();
            if g.staging.contains_key(&hash) {
                continue;
            }
            *g.deleting.entry(hash.clone()).or_default() += 1;
            current.is_some()
        };
        // Released when this iteration ends, also when the task is cancelled while the storage service is being called
        let _deleting = DeletingGuard { st: st.clone(), hash: hash.clone() };
        // The write lock has been released: S3 may be slow, so don't make every write in the system wait for it
        let failed = match st.storage(&location) {
            Ok(storage) => storage.delete(&hash).await.err().map(|e| e.to_string()),
            Err(e) => Some(e.message),
        };
        {
            let _w = st.write_lock.lock().await;
            let res = match &failed {
                // Couldn't delete it (e.g. the storage service is disconnected): record it and retry once the location
                // is reachable again, to avoid leaving orphaned objects taking up space. Each failure doubles the wait
                // (created_at is the time of the next attempt), up to a day: a bucket that never allows deleting isn't
                // asked every 30 seconds.
                Some(err) => {
                    tracing::debug!("Failed to delete physical file {hash} ({location}), will retry later: {err}");
                    failures.push(err.clone());
                    sqlx::query(
                        "INSERT INTO pending_blob_deletes (hash, location_id, created_at, attempts, last_error) VALUES (?1, ?2, ?3 + ?5, 1, ?4)
                         ON CONFLICT (hash, location_id) DO UPDATE SET attempts = attempts + 1, last_error = excluded.last_error,
                           created_at = ?3 + MIN(?5 << MIN(attempts, 12), ?6)",
                    )
                    .bind(&hash)
                    .bind(&location)
                    .bind(crate::util::now())
                    .bind(err.chars().take(300).collect::<String>())
                    .bind(RETRY_BASE)
                    .bind(RETRY_MAX)
                    .execute(&st.db)
                    .await
                    .map(|_| ())
                }
                None => sqlx::query("DELETE FROM pending_blob_deletes WHERE hash = ? AND location_id = ?")
                    .bind(&hash)
                    .bind(&location)
                    .execute(&st.db)
                    .await
                    .map(|_| ()),
            };
            if let Err(e) = res {
                tracing::warn!("Failed to update the pending deletion list: {e}");
            }
        }
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

/// Wait before retrying a deletion that failed once; it doubles with every further failure, up to `RETRY_MAX`
const RETRY_BASE: i64 = 60;
const RETRY_MAX: i64 = 24 * 3600;

struct DeletingGuard {
    st: AppState,
    hash: String,
}

impl Drop for DeletingGuard {
    fn drop(&mut self) {
        let mut g = self.st.blob_guard.lock().unwrap();
        if let Some(n) = g.deleting.get_mut(&self.hash) {
            *n -= 1;
            if *n == 0 {
                g.deleting.remove(&self.hash);
            }
        }
    }
}

/// Keeps a hash marked as "being staged"; dropping it (commit, failure, or a cancelled request) releases the mark
pub struct StageGuard {
    st: AppState,
    hash: String,
}

impl Drop for StageGuard {
    fn drop(&mut self) {
        let mut g = self.st.blob_guard.lock().unwrap();
        if let Some(n) = g.staging.get_mut(&self.hash) {
            *n -= 1;
            if *n == 0 {
                g.staging.remove(&self.hash);
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
    guard: StageGuard,
}

/// Marks content as "being staged" until the guard is dropped: background deletion leaves it alone meanwhile. If the
/// same content is being deleted right now, waits for that to finish first, so content stored afterwards isn't deleted.
pub async fn stage_guard(st: &AppState, hash: &str) -> StageGuard {
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
    StageGuard { st: st.clone(), hash: hash.to_string() }
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

/// Adds `delta` bytes to a space's usage counter (call inside the transaction that adds, replaces or removes file nodes)
pub async fn adjust_usage(conn: &mut SqliteConnection, drive_id: &str, delta: i64) -> AppResult<()> {
    if delta == 0 || drive_id.is_empty() {
        return Ok(());
    }
    let after: Option<(i64,)> = sqlx::query_as("UPDATE drives SET used_bytes = used_bytes + ? WHERE id = ? RETURNING used_bytes")
        .bind(delta)
        .bind(drive_id)
        .fetch_optional(&mut *conn)
        .await?;
    if let Some((n,)) = after
        && n < 0
    {
        // A path that adds files without counting them; the daily recompute puts the counter right, but it shouldn't happen
        tracing::warn!("Space usage of {drive_id} went negative ({n}) after {delta:+}; clamped to 0");
        sqlx::query("UPDATE drives SET used_bytes = 0 WHERE id = ?").bind(drive_id).execute(conn).await?;
    }
    Ok(())
}

/// Recomputes every space's usage counter from the node table (startup and once a day, in case a counter drifted)
pub async fn recompute_usage(st: &AppState) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    sqlx::query("UPDATE drives SET used_bytes = (SELECT COALESCE(SUM(size), 0) FROM nodes WHERE drive_id = drives.id AND kind = 'file')")
        .execute(&st.db)
        .await?;
    Ok(())
}

/// Space used in the user's personal space
pub async fn used_bytes(db: &SqlitePool, user_id: i64) -> AppResult<i64> {
    let (used,): (i64,) = sqlx::query_as("SELECT COALESCE(SUM(used_bytes), 0) FROM drives WHERE kind = 'personal' AND owner_id = ?")
        .bind(user_id)
        .fetch_one(db)
        .await?;
    Ok(used)
}

/// Space quota (0 = unlimited): personal spaces use the owner account's quota
pub async fn drive_quota(conn: &mut SqliteConnection, drive: &Drive) -> AppResult<i64> {
    if drive.kind == "personal" {
        let (q,): (i64,) = sqlx::query_as("SELECT COALESCE((SELECT quota_bytes FROM users WHERE id = ?), 0)")
            .bind(drive.owner_id)
            .fetch_one(conn)
            .await?;
        return Ok(q);
    }
    Ok(drive.quota_bytes)
}

/// Checks that adding `extra` bytes to the space won't exceed its quota (including unfinished uploads).
pub async fn check_quota(conn: &mut SqliteConnection, drive_id: &str, extra: i64) -> AppResult<()> {
    if extra <= 0 {
        return Ok(());
    }
    let Some(drive) = get_drive(conn, drive_id).await? else { return Ok(()) };
    let quota = drive_quota(conn, &drive).await?;
    if quota <= 0 {
        return Ok(());
    }
    // Files already there plus uploads still in progress (they were admitted against the quota when they started)
    // Only uploads that received data within the last day hold space: an abandoned one can't block a space for days
    // (every request of an upload moves its expiry to UPLOAD_TTL from then)
    let active_since = crate::util::now() + crate::upload::UPLOAD_TTL - 86400;
    let (pending,): (i64,) =
        sqlx::query_as("SELECT COALESCE(SUM(size), 0) FROM uploads WHERE drive_id = ? AND node_id IS NULL AND expires_at > ?")
            .bind(drive_id)
            .bind(active_since)
            .fetch_one(conn)
            .await?;
    let used = drive.used_bytes + pending;
    if used + extra > quota {
        return Err(AppError::new(axum::http::StatusCode::PAYLOAD_TOO_LARGE, format!("Not enough storage space in \"{}\"", drive.name)).with_code("quota"));
    }
    Ok(())
}

/// Finds or creates folders under parent following a relative path (a/b/c), returning the id of the deepest folder.
///
/// When a name on the way is taken by a file, a numbered folder is created instead ("Photos (1)"). Every file of an
/// uploaded folder is its own upload, so with a `batch` the numbered folder is remembered and the other files of the
/// same batch go into it too, instead of each creating another one.
pub async fn ensure_folders(conn: &mut SqliteConnection, owner_id: i64, parent_id: &str, rel: &str, batch: &str) -> AppResult<String> {
    let mut current = parent_id.to_string();
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        let name = crate::util::validate_name(part)?;
        let existing: Option<(String, String)> = sqlx::query_as(
            "SELECT id, kind FROM nodes WHERE parent_id = ?1 AND name_key = CASE WHEN fs_path IS NULL THEN unicode_lower(?2) ELSE ?2 END AND trashed_at IS NULL",
        )
        .bind(&current)
        .bind(&name)
        .fetch_optional(&mut *conn)
        .await?;
        current = match existing {
            Some((id, kind)) if kind == "folder" => id,
            None => create_folder(conn, owner_id, &current, &name).await?,
            // Taken by a file
            Some(_) => {
                let key = name.to_lowercase();
                let known: Option<(String,)> = if batch.is_empty() {
                    None
                } else {
                    sqlx::query_as(
                        "SELECT b.folder_id FROM upload_batch_folders b
                         JOIN nodes n ON n.id = b.folder_id AND n.kind = 'folder' AND n.trashed_at IS NULL
                         WHERE b.batch = ? AND b.parent_id = ? AND b.name = ?",
                    )
                    .bind(batch)
                    .bind(&current)
                    .bind(&key)
                    .fetch_optional(&mut *conn)
                    .await?
                };
                match known {
                    Some((id,)) => id,
                    None => {
                        let numbered = unique_name(conn, &current, &name, true).await?;
                        let id = create_folder(conn, owner_id, &current, &numbered).await?;
                        if !batch.is_empty() {
                            sqlx::query(
                                "INSERT OR REPLACE INTO upload_batch_folders (batch, parent_id, name, folder_id, created_at) VALUES (?, ?, ?, ?, ?)",
                            )
                            .bind(batch)
                            .bind(&current)
                            .bind(&key)
                            .bind(&id)
                            .bind(crate::util::now())
                            .execute(&mut *conn)
                            .await?;
                        }
                        id
                    }
                }
            }
        };
    }
    Ok(current)
}

/// Creates a folder; in a folder space it is made on the disk first
pub async fn create_folder(conn: &mut SqliteConnection, owner_id: i64, parent_id: &str, name: &str) -> AppResult<String> {
    let id = crate::util::new_id();
    let ts = now();
    if let Some(parent) = get_node(conn, parent_id).await?.filter(|p| p.in_folder_space()) {
        let (rel, stat) = crate::fsops::make_dir(&parent, name)?;
        crate::fsops::insert(conn, &id, owner_id, &parent, name, &rel, &stat).await?;
        touch(conn, parent_id).await?;
        return Ok(id);
    }
    sqlx::query(
        "INSERT INTO nodes (id, owner_id, parent_id, kind, name, drive_id, created_at, updated_at)
         SELECT ?1, ?2, ?3, 'folder', ?4, drive_id, ?5, ?5 FROM nodes WHERE id = ?3",
    )
    .bind(&id)
    .bind(owner_id)
    .bind(parent_id)
    .bind(name)
    .bind(ts)
    .execute(&mut *conn)
    .await?;
    touch(conn, parent_id).await?;
    Ok(id)
}

pub async fn touch(conn: &mut SqliteConnection, id: &str) -> AppResult<()> {
    sqlx::query("UPDATE nodes SET updated_at = MAX(?, updated_at + 1) WHERE id = ?").bind(now()).bind(id).execute(conn).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[tokio::test]
    async fn an_upload_without_progress_for_a_day_no_longer_holds_space() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let drive = env.drive_of(&amy.root_id).await;
        // A personal space's quota is its owner's
        sqlx::query("UPDATE users SET quota_bytes = 1000 WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
        let ts = crate::util::now();
        sqlx::query("INSERT INTO uploads (id, owner_id, parent_id, rel_path, name, size, offset, created_at, expires_at, drive_id) VALUES ('u1', ?, ?, '', 'big.bin', 900, 0, ?, ?, ?)")
            .bind(amy.id)
            .bind(&amy.root_id)
            .bind(ts)
            .bind(ts + crate::upload::UPLOAD_TTL)
            .bind(&drive)
            .execute(&env.st.db)
            .await
            .unwrap();
        let mut c = env.st.db.acquire().await.unwrap();
        assert!(check_quota(&mut c, &drive, 200).await.is_err(), "a fresh upload reserves its size");
        sqlx::query("UPDATE uploads SET expires_at = ? WHERE id = 'u1'").bind(ts + crate::upload::UPLOAD_TTL - 2 * 86400).execute(&mut *c).await.unwrap();
        assert!(check_quota(&mut c, &drive, 200).await.is_ok(), "an upload idle for two days no longer does");
    }

    #[tokio::test]
    async fn files_of_one_uploaded_folder_stay_together_when_a_file_has_its_name() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        env.file(&amy, &amy.root_id, "Photos").await;
        let mut c = env.st.db.acquire().await.unwrap();
        let mut ensure = async |rel: &str, batch: &str| ensure_folders(&mut c, amy.id, &amy.root_id, rel, batch).await.unwrap();

        // Two files of one batch: "Photos (1)" is created once, and both land in it
        let a = ensure("Photos", "b1").await;
        let b = ensure("photos", "b1").await;
        assert_eq!(a, b);
        let sub = ensure("Photos/2026", "b1").await;
        assert_eq!(get_node(&mut c, &sub).await.unwrap().unwrap().parent_id.as_deref(), Some(a.as_str()));
        assert_eq!(get_node(&mut c, &a).await.unwrap().unwrap().name, "Photos (1)");

        // Another batch, or a client that sends none, gets a folder of its own as before
        let other = ensure_folders(&mut c, amy.id, &amy.root_id, "Photos", "b2").await.unwrap();
        let none = ensure_folders(&mut c, amy.id, &amy.root_id, "Photos", "").await.unwrap();
        assert!(other != a && none != a && none != other);
    }

    #[tokio::test]
    async fn numbering_picks_the_lowest_free_number_in_one_query() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        for name in ["a_b%.txt", "a_b% (1).txt", "A_B% (2).TXT", "a_b% (4).txt", "axb% (3).txt", "Report", "Report (1)"] {
            env.file(&amy, &amy.root_id, name).await;
        }
        let mut conn = env.st.db.acquire().await.unwrap();
        // Case-insensitive, and the LIKE wildcards in the name itself are escaped (axb% must not count)
        assert_eq!(unique_name(&mut conn, &amy.root_id, "a_b%.txt", false).await.unwrap(), "a_b% (3).txt");
        assert_eq!(unique_name(&mut conn, &amy.root_id, "Report", true).await.unwrap(), "Report (2)");
        assert_eq!(unique_name(&mut conn, &amy.root_id, "new.txt", false).await.unwrap(), "new.txt");
        // A name that itself looks numbered
        drop(conn);
        env.file(&amy, &amy.root_id, "file (0).txt").await;
        env.file(&amy, &amy.root_id, "file (0) (1).txt").await;
        let mut conn = env.st.db.acquire().await.unwrap();
        assert_eq!(unique_name(&mut conn, &amy.root_id, "file (0).txt", false).await.unwrap(), "file (0) (2).txt");
    }

    async fn role(env: &testutil::TestEnv, user: &User, id: &str) -> Option<Role> {
        let mut c = env.st.db.acquire().await.unwrap();
        let node = get_node(&mut c, id).await.unwrap().unwrap();
        role_on(&mut c, user, &node).await.unwrap()
    }

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

    #[tokio::test]
    async fn role_on_follows_drives_and_folder_shares() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let admin = env.admin().await;
        let project = env.folder(&amy, &amy.root_id, "Projects").await;
        let sub = env.folder(&amy, &project, "Subfolder").await;
        let private = env.folder(&amy, &amy.root_id, "Private").await;

        // Personal space: only the owner; administrators can't see it either (for privacy)
        assert_eq!(role(&env, &amy, &project).await, Some(Role::Owner));
        assert_eq!(role(&env, &ben, &project).await, None);
        assert_eq!(role(&env, &admin, &project).await, None);

        // Folder sharing: grants are inherited downward without affecting sibling folders or the space root
        env.grant(&project, &ben, "editor").await;
        assert_eq!(role(&env, &ben, &sub).await, Some(Role::Editor));
        assert_eq!(role(&env, &ben, &private).await, None);
        assert_eq!(role(&env, &ben, &amy.root_id).await, None);

        // The higher role wins
        env.grant(&sub, &ben, "manager").await;
        assert_eq!(role(&env, &ben, &sub).await, Some(Role::Manager));

        // Takes effect immediately after revocation
        env.revoke(&project, &ben).await;
        env.revoke(&sub, &ben).await;
        assert_eq!(role(&env, &ben, &sub).await, None);

        // Expired grants are ignored
        let mut c = env.st.db.acquire().await.unwrap();
        crate::db::add_grant(&mut c, &project, "user", ben.id, "viewer", None, Some(now() - 10)).await.unwrap();
        drop(c);
        assert_eq!(role(&env, &ben, &project).await, None);

        // Company shared space: everyone can edit, administrators can manage
        let company = env.st.shared_root().unwrap();
        assert_eq!(role(&env, &ben, &company).await, Some(Role::Editor));
        assert_eq!(role(&env, &admin, &company).await, Some(Role::Manager));
    }
}
