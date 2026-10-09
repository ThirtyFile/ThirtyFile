//! Shared file tree logic: node queries, ancestors/subtrees, name conflicts and folder creation. Permissions,
//! content storage (blob reference counting) and quotas are in the submodules, re-exported here.

mod blobs;
pub mod changes;
mod permissions;
mod quota;

pub use blobs::*;
pub use permissions::*;
pub use quota::*;

use std::collections::HashMap;

use serde::Serialize;
use sqlx::SqliteConnection;

use crate::{
    auth::User,
    error::{AppError, AppResult},
    util::{now, numbered_name},
};

pub const NODE_COLS: &str = "n.id, n.owner_id, n.parent_id, n.kind, n.name, n.blob_hash, n.size, n.mime, n.created_at, n.updated_at, n.trashed_at, n.drive_id,
     CASE WHEN n.found THEN '' ELSE COALESCE((SELECT username FROM users WHERE id = n.owner_id), '') END AS owner_name,
     (SELECT location_id FROM blobs WHERE hash = n.blob_hash) AS blob_location,
     n.fs_path, (SELECT source_path FROM drives WHERE id = n.drive_id AND mode = 'folder') AS fs_root,
     (SELECT read_only OR moving FROM drives WHERE id = n.drive_id) AS space_read_only,
     (SELECT moving FROM drives WHERE id = n.drive_id) AS space_moving";

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Node {
    pub id: String,
    #[serde(skip)]
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
    /// Uploader / creator; empty for an item the check of a folder space found on its disk
    pub owner_name: String,
    /// Whether the current user has favorited it; filled in by mark_own
    #[sqlx(default)]
    pub is_favorite: bool,
    /// The current user's tags on it (their ids), never anyone else's; filled in by mark_own
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[sqlx(skip)]
    pub tags: Vec<i64>,
    /// Folder spaces: the path below the space's folder
    #[serde(skip)]
    #[sqlx(default)]
    pub fs_path: Option<String>,
    /// Folder spaces: the space's folder on the server
    #[serde(skip)]
    #[sqlx(default)]
    pub fs_root: Option<String>,
    /// The space is read-only: browse, download and share only (also while it is being moved, `space_moving`)
    #[serde(skip)]
    #[sqlx(default)]
    pub space_read_only: bool,
    /// The space is being moved to another storage location, and is read-only until the move is over
    #[serde(skip)]
    #[sqlx(default)]
    pub space_moving: bool,
    /// In listings of folders only (the navigation pane): whether the folder has folders in it, so it shows an arrow
    /// to expand only then
    #[serde(skip_serializing_if = "Option::is_none")]
    #[sqlx(default)]
    pub has_folders: Option<bool>,
}

impl crate::logs::Item for Node {
    fn id(&self) -> &str {
        &self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn space(&self) -> Option<&str> {
        self.drive_id.as_deref()
    }
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
    /// The file on the server, for items of a folder space (tests: to look at it; the server uses `fs_pinned`)
    #[cfg(test)]
    pub fn fs_file(&self) -> Option<std::path::PathBuf> {
        let (root, rel) = (self.fs_root.as_deref()?, self.fs_path.as_deref()?);
        // Paths come from scanning the folder; never step outside it whatever they say
        if rel.split('/').any(|part| part == ".." || part == ".") || rel.starts_with('/') {
            return None;
        }
        Some(if rel.is_empty() { std::path::PathBuf::from(root) } else { std::path::Path::new(root).join(rel) })
    }
    /// The file on the server, for items of a folder space, reached without following a symbolic link on the way, in
    /// the space's own folder (`folders::open_space`)
    pub fn fs_pinned(&self) -> std::io::Result<crate::beneath::Pinned> {
        let (Some(root), Some(rel)) = (self.fs_root.as_deref(), self.fs_path.as_deref()) else {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "not in a folder space"));
        };
        crate::folders::open_space(std::path::Path::new(root), self.drive(), self.space_read_only)?.join(rel)
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

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Drive {
    pub id: String,
    pub name: String,
    pub kind: SpaceKind,
    pub root_id: String,
    pub owner_id: Option<i64>,
    pub quota_bytes: i64,
    pub disabled: bool,
    /// Bytes of all file nodes in the space, including the trash (kept up to date by `adjust_usage`)
    pub used_bytes: i64,
    pub mode: SpaceMode,
    /// Folder spaces: the folder
    pub source_path: Option<String>,
    /// Browse, download and share only
    pub read_only: bool,
    /// Being moved to another storage location: read-only until the move is over (moves/)
    pub moving: bool,
}

impl Drive {
    pub fn is_folder(&self) -> bool {
        self.mode == SpaceMode::Folder
    }
}

/// What a space is (`drives.kind`), in the order spaces are listed
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum SpaceKind {
    /// Someone's "My files"
    Personal,
    /// "All files", which everyone may use
    Company,
    /// A space for a team
    Team,
}

/// How a space keeps its files (`drives.mode`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum SpaceMode {
    /// In the content store of its storage location, named by their SHA-256
    Store,
    /// As they are, in a folder on the server (a folder space)
    Folder,
}

pub const DRIVE_COLS: &str = "d.id, d.name, d.kind, d.root_id, d.owner_id, d.quota_bytes, d.disabled, d.used_bytes, d.mode, d.source_path, d.read_only, d.moving";

pub async fn get_drive(conn: &mut SqliteConnection, id: &str) -> AppResult<Option<Drive>> {
    let sql = format!("SELECT {DRIVE_COLS} FROM drives d WHERE d.id = ?");
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(id).fetch_optional(conn).await?)
}

/// A folder as the activity log names it: by its name, or a space's top folder by the space's name ("My files"), as
/// the pages show it
pub async fn place_name(conn: &mut SqliteConnection, folder: &Node) -> AppResult<String> {
    if folder.parent_id.is_some() {
        return Ok(folder.name.clone());
    }
    Ok(get_drive(conn, folder.drive()).await?.map(|d| d.name).unwrap_or_else(|| folder.name.clone()))
}

pub fn resolve_alias<'a>(user: &'a User, id: &'a str) -> AppResult<&'a str> {
    Ok(match id {
        // The personal space, which a user may not have (personal/): the web never asks them for it
        "root" => user.root_id.as_deref().ok_or_else(|| AppError::not_found("You don't have a personal space"))?,
        "shared" => user.shared_root.as_deref().ok_or_else(|| AppError::not_found("The shared space isn't enabled"))?,
        _ => id,
    })
}

pub async fn get_node(conn: &mut SqliteConnection, id: &str) -> AppResult<Option<Node>> {
    let sql = format!("SELECT {NODE_COLS} FROM nodes n WHERE n.id = ?");
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(id).fetch_optional(conn).await?)
}

/// Fills in what is the current user's own on these nodes: whether they favorited each, and their tags on it (never
/// anyone else's, tags.rs).
/// Takes the caller's connection: taking a second one from the pool while holding one can use up the pool under load.
pub async fn mark_own<'a>(conn: &mut SqliteConnection, user_id: i64, nodes: impl IntoIterator<Item = &'a mut Node>) -> AppResult<()> {
    let nodes: Vec<&mut Node> = nodes.into_iter().collect();
    if nodes.is_empty() {
        return Ok(());
    }
    let ids = serde_json::to_string(&nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>()).unwrap();
    let favs: Vec<(String,)> = sqlx::query_as("SELECT node_id FROM favorites WHERE user_id = ? AND node_id IN (SELECT value FROM json_each(?))")
        .bind(user_id)
        .bind(&ids)
        .fetch_all(&mut *conn)
        .await?;
    let favs: std::collections::HashSet<String> = favs.into_iter().map(|(id,)| id).collect();
    let tagged: Vec<(String, i64)> =
        sqlx::query_as("SELECT node_id, tag_id FROM tagged WHERE owner_id = ? AND node_id IN (SELECT value FROM json_each(?)) ORDER BY tag_id")
            .bind(user_id)
            .bind(&ids)
            .fetch_all(&mut *conn)
            .await?;
    let mut tags: HashMap<String, Vec<i64>> = HashMap::new();
    for (node, tag) in tagged {
        tags.entry(node).or_default().push(tag);
    }
    for n in nodes {
        n.is_favorite = favs.contains(&n.id);
        n.tags = tags.remove(&n.id).unwrap_or_default();
    }
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

/// What `id` and everything in it hold, added up without reading the items themselves: (the size of its files, the
/// items not in the trash)
pub async fn subtree_totals(conn: &mut SqliteConnection, id: &str) -> AppResult<(i64, i64)> {
    Ok(sqlx::query_as(
        "WITH RECURSIVE sub(id) AS (SELECT ?1 UNION ALL SELECT c.id FROM nodes c JOIN sub ON c.parent_id = sub.id)
         SELECT COALESCE(SUM(CASE WHEN n.kind = 'file' THEN n.size ELSE 0 END), 0), COUNT(*) - COUNT(n.trashed_at) FROM sub JOIN nodes n ON n.id = sub.id",
    )
    .bind(id)
    .fetch_one(conn)
    .await?)
}

/// The ids of `id` and everything in it that isn't in the trash
pub async fn live_subtree_ids(conn: &mut SqliteConnection, id: &str) -> AppResult<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "WITH RECURSIVE sub(id) AS (SELECT ?1 UNION ALL SELECT c.id FROM nodes c JOIN sub ON c.parent_id = sub.id)
         SELECT n.id FROM sub JOIN nodes n ON n.id = sub.id WHERE n.trashed_at IS NULL",
    )
    .bind(id)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// An item (of the folder `?1`, not in the trash) named `?2`: regardless of letter case in the content store, exactly in
/// folder spaces (a folder on disk can hold both "A.txt" and "a.txt").
///
/// Which key a name has depends on the item (`name_key`), so the `CASE` alone can't be looked up in `nodes_name_uq`:
/// it reads every item of the folder, 50,000 for every name in a large one. The `IN` looks up the two keys the name can
/// have in the index, and the `CASE` keeps the item whose key it is.
pub const NAMED: &str = "parent_id = ?1 AND trashed_at IS NULL AND name_key IN (unicode_lower(?2), ?2)
     AND name_key = CASE WHEN fs_path IS NULL THEN unicode_lower(?2) ELSE ?2 END";

/// Whether the folder has an item with this name (`NAMED`)
pub async fn name_taken(conn: &mut SqliteConnection, parent_id: &str, name: &str) -> AppResult<bool> {
    let row: Option<(i64,)> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT 1 FROM nodes WHERE {NAMED} LIMIT 1"))).bind(parent_id).bind(name).fetch_optional(conn).await?;
    Ok(row.is_some())
}

/// `find_children`: every name of the JSON array `?2` (the outer loop, which CROSS JOIN keeps), looked up as `NAMED`
fn children_named_sql() -> String {
    format!(
        "SELECT j.value AS asked, {NODE_COLS} FROM json_each(?2) j
         CROSS JOIN nodes n ON n.parent_id = ?1 AND n.trashed_at IS NULL AND n.name_key IN (unicode_lower(j.value), j.value)
                           AND n.name_key = CASE WHEN n.fs_path IS NULL THEN unicode_lower(j.value) ELSE j.value END
         ORDER BY j.key"
    )
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
    let sql = children_named_sql();
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(parent_id).bind(serde_json::to_string(names).unwrap()).fetch_all(conn).await?;
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
    let taken: Vec<(String,)> = sqlx::query_as("SELECT name FROM nodes WHERE parent_id = ? AND name_key LIKE unicode_lower(?) ESCAPE '\\' AND trashed_at IS NULL")
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

pub async fn touch(conn: &mut SqliteConnection, id: &str) -> AppResult<()> {
    sqlx::query("UPDATE nodes SET updated_at = MAX(?, updated_at + 1) WHERE id = ?").bind(now()).bind(id).execute(conn).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    /// The values the schema allows in a column (`CHECK (column IN ('a', 'b'))`)
    async fn allowed(db: &sqlx::SqlitePool, table: &str, column: &str) -> Vec<String> {
        let (sql,): (String,) = sqlx::query_as("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?").bind(table).fetch_one(db).await.unwrap();
        let start = sql.find(&format!("CHECK ({column} IN (")).unwrap_or_else(|| panic!("{table}.{column} has no list of values")) + column.len() + 12;
        sql[start..start + sql[start..].find(')').unwrap()].split(',').map(|v| v.trim().trim_matches('\'').to_string()).collect()
    }

    /// Reads each value the schema allows as `T`, and checks that anything else is refused rather than taken for one
    async fn read_all<T>(db: &sqlx::SqlitePool, table: &str, column: &str) -> Vec<T>
    where
        T: for<'r> sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite> + Send + Unpin,
    {
        let mut out = Vec::new();
        for value in allowed(db, table, column).await {
            out.push(sqlx::query_as::<_, (T,)>("SELECT ?").bind(&value).fetch_one(db).await.unwrap_or_else(|e| panic!("{table}.{column} = {value}: {e}")).0);
        }
        assert!(sqlx::query_as::<_, (T,)>("SELECT 'something else'").fetch_one(db).await.is_err(), "{table}.{column}: an unknown value is refused");
        out
    }

    #[tokio::test]
    async fn closed_sets_of_values_are_read_as_their_enums_and_nothing_else() {
        let env = testutil::env().await;
        let db = &env.st.db;
        assert_eq!(read_all::<SpaceKind>(db, "drives", "kind").await, [SpaceKind::Personal, SpaceKind::Company, SpaceKind::Team]);
        assert_eq!(read_all::<SpaceMode>(db, "drives", "mode").await.len(), 2);
        assert_eq!(read_all::<Role>(db, "grants", "role").await, [Role::Viewer, Role::Editor, Role::Manager, Role::Owner]);
        let types = read_all::<PrincipalType>(db, "grants", "principal_type").await;
        assert_eq!(types, [PrincipalType::User, PrincipalType::Group, PrincipalType::Everyone]);
        assert!(types.iter().all(|t| PrincipalType::parse(t.as_str()) == Some(*t)));
        assert_eq!(read_all::<crate::auth::UserRole>(db, "users", "role").await, [crate::auth::UserRole::Admin, crate::auth::UserRole::User]);
        assert_eq!(read_all::<changes::ChangeKind>(db, "tree_changes", "kind").await.len(), 4);
        assert_eq!(read_all::<crate::backups::runner::JobState>(db, "backup_jobs", "state").await.len(), 7);
        assert_eq!(read_all::<crate::backups::runner::JobState>(db, "replica_jobs", "state").await.len(), 7);
    }

    #[tokio::test]
    async fn files_of_one_uploaded_folder_stay_together_when_a_file_has_its_name() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        env.file(&amy, amy.root(), "Photos").await;
        let mut c = env.st.db.acquire().await.unwrap();
        let mut ensure = async |rel: &str, batch: &str| crate::content::ensure_folders(&mut c, amy.id, amy.root(), rel, batch).await.unwrap();

        // Two files of one batch: "Photos (1)" is created once, and both land in it
        let a = ensure("Photos", "b1").await;
        let b = ensure("photos", "b1").await;
        assert_eq!(a, b);
        let sub = ensure("Photos/2026", "b1").await;
        assert_eq!(get_node(&mut c, &sub).await.unwrap().unwrap().parent_id.as_deref(), Some(a.as_str()));
        assert_eq!(get_node(&mut c, &a).await.unwrap().unwrap().name, "Photos (1)");

        // Another batch, or a client that sends none, gets a folder of its own as before
        let other = crate::content::ensure_folders(&mut c, amy.id, amy.root(), "Photos", "b2").await.unwrap();
        let none = crate::content::ensure_folders(&mut c, amy.id, amy.root(), "Photos", "").await.unwrap();
        assert!(other != a && none != a && none != other);
    }

    #[tokio::test]
    async fn names_are_looked_up_in_the_index_without_reading_the_whole_folder() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        env.file(&amy, amy.root(), "Été.txt").await;
        let space = env.folder_space("Disk").await;
        let admin = env.admin().await;
        env.file(&admin, &space.root, "A.txt").await;
        sqlx::query("UPDATE nodes SET fs_path = name WHERE parent_id = ?").bind(&space.root).execute(&env.st.db).await.unwrap();
        let mut c = env.st.db.acquire().await.unwrap();
        // Any letter case in the content store, exactly in folder spaces
        assert!(name_taken(&mut c, amy.root(), "éTÉ.txt").await.unwrap());
        assert!(name_taken(&mut c, &space.root, "A.txt").await.unwrap());
        assert!(!name_taken(&mut c, &space.root, "a.txt").await.unwrap());
        let names = ["a.txt", "ÉTÉ.TXT", "A.txt"].map(String::from);
        let found = find_children(&mut c, amy.root(), &names).await.unwrap();
        assert_eq!(found.iter().map(|(asked, n)| (asked.as_str(), n.name.as_str())).collect::<Vec<_>>(), [("ÉTÉ.TXT", "Été.txt")]);
        let found = find_children(&mut c, &space.root, &names).await.unwrap();
        assert_eq!(found.iter().map(|(asked, n)| (asked.as_str(), n.name.as_str())).collect::<Vec<_>>(), [("A.txt", "A.txt")]);

        // Each name is a lookup in the index: before, every item of the folder was read for every name
        let plan = async |c: &mut SqliteConnection, sql: &str| -> String {
            let rows: Vec<(i64, i64, i64, String)> =
                sqlx::query_as(sqlx::AssertSqlSafe(format!("EXPLAIN QUERY PLAN {sql}"))).bind("p").bind(r#"["a"]"#).fetch_all(c).await.unwrap();
            rows.into_iter().map(|r| r.3).collect::<Vec<_>>().join("; ")
        };
        let one = plan(&mut c, &format!("SELECT 1 FROM nodes WHERE {NAMED} LIMIT 1")).await;
        assert!(one.contains("USING INDEX nodes_name_uq (parent_id=? AND name_key=?)"), "{one}");
        let many = plan(&mut c, &children_named_sql()).await;
        assert!(many.contains("SCAN j") && many.contains("USING INDEX nodes_name_uq (parent_id=? AND name_key=?)"), "{many}");
    }

    #[tokio::test]
    async fn numbering_picks_the_lowest_free_number_in_one_query() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        for name in ["a_b%.txt", "a_b% (1).txt", "A_B% (2).TXT", "a_b% (4).txt", "axb% (3).txt", "Report", "Report (1)"] {
            env.file(&amy, amy.root(), name).await;
        }
        let mut conn = env.st.db.acquire().await.unwrap();
        // Case-insensitive, and the LIKE wildcards in the name itself are escaped (axb% must not count)
        assert_eq!(unique_name(&mut conn, amy.root(), "a_b%.txt", false).await.unwrap(), "a_b% (3).txt");
        assert_eq!(unique_name(&mut conn, amy.root(), "Report", true).await.unwrap(), "Report (2)");
        assert_eq!(unique_name(&mut conn, amy.root(), "new.txt", false).await.unwrap(), "new.txt");
        // A name that itself looks numbered
        drop(conn);
        env.file(&amy, amy.root(), "file (0).txt").await;
        env.file(&amy, amy.root(), "file (0) (1).txt").await;
        let mut conn = env.st.db.acquire().await.unwrap();
        assert_eq!(unique_name(&mut conn, amy.root(), "file (0).txt", false).await.unwrap(), "file (0) (2).txt");
    }
}
