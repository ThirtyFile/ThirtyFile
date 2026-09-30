//! Shared file tree logic: node queries, ancestors/subtrees, name conflicts and folder creation. Permissions,
//! content storage (blob reference counting) and quotas are in the submodules, re-exported here.

mod blobs;
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

pub const NODE_COLS: &str =
    "n.id, n.owner_id, n.parent_id, n.kind, n.name, n.blob_hash, n.size, n.mime, n.created_at, n.updated_at, n.trashed_at, n.drive_id,
     COALESCE((SELECT username FROM users WHERE id = n.owner_id), '') AS owner_name,
     (SELECT location_id FROM blobs WHERE hash = n.blob_hash) AS blob_location,
     n.fs_path, (SELECT source_path FROM drives WHERE id = n.drive_id AND mode = 'folder') AS fs_root,
     (SELECT read_only OR moving FROM drives WHERE id = n.drive_id) AS space_read_only,
     (SELECT moving FROM drives WHERE id = n.drive_id) AS space_moving";

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
        if rel.split('/').any(|part| part == ".." || part == "." ) || rel.starts_with('/') {
            return None;
        }
        Some(if rel.is_empty() { std::path::PathBuf::from(root) } else { std::path::Path::new(root).join(rel) })
    }
    /// The file on the server, for items of a folder space, reached without following a symbolic link on the way
    pub fn fs_pinned(&self) -> std::io::Result<crate::beneath::Pinned> {
        let (Some(root), Some(rel)) = (self.fs_root.as_deref(), self.fs_path.as_deref()) else {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "not in a folder space"));
        };
        crate::beneath::Pinned::root(std::path::Path::new(root))?.join(rel)
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
    /// Being moved to another storage location: read-only until the move is over (moves/)
    pub moving: bool,
}

impl Drive {
    pub fn is_folder(&self) -> bool {
        self.mode == "folder"
    }
}

pub const DRIVE_COLS: &str = "d.id, d.name, d.kind, d.root_id, d.owner_id, d.quota_bytes, d.disabled, d.used_bytes, d.mode, d.source_path, d.read_only, d.moving";

pub async fn get_drive(conn: &mut SqliteConnection, id: &str) -> AppResult<Option<Drive>> {
    let sql = format!("SELECT {DRIVE_COLS} FROM drives d WHERE d.id = ?");
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(id).fetch_optional(conn).await?)
}

pub fn resolve_alias<'a>(user: &'a User, id: &'a str) -> AppResult<&'a str> {
    Ok(match id {
        // The personal space, which a user may not have (personal.rs): the web never asks them for it
        "root" => user.root_id.as_deref().ok_or_else(|| AppError::not_found("You don't have a personal space"))?,
        "shared" => user.shared_root.as_deref().ok_or_else(|| AppError::not_found("The shared space isn't enabled"))?,
        _ => id,
    })
}

pub async fn get_node(conn: &mut SqliteConnection, id: &str) -> AppResult<Option<Node>> {
    let sql = format!("SELECT {NODE_COLS} FROM nodes n WHERE n.id = ?");
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(id).fetch_optional(conn).await?)
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
    async fn files_of_one_uploaded_folder_stay_together_when_a_file_has_its_name() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        env.file(&amy, amy.root(), "Photos").await;
        let mut c = env.st.db.acquire().await.unwrap();
        let mut ensure = async |rel: &str, batch: &str| ensure_folders(&mut c, amy.id, amy.root(), rel, batch).await.unwrap();

        // Two files of one batch: "Photos (1)" is created once, and both land in it
        let a = ensure("Photos", "b1").await;
        let b = ensure("photos", "b1").await;
        assert_eq!(a, b);
        let sub = ensure("Photos/2026", "b1").await;
        assert_eq!(get_node(&mut c, &sub).await.unwrap().unwrap().parent_id.as_deref(), Some(a.as_str()));
        assert_eq!(get_node(&mut c, &a).await.unwrap().unwrap().name, "Photos (1)");

        // Another batch, or a client that sends none, gets a folder of its own as before
        let other = ensure_folders(&mut c, amy.id, amy.root(), "Photos", "b2").await.unwrap();
        let none = ensure_folders(&mut c, amy.id, amy.root(), "Photos", "").await.unwrap();
        assert!(other != a && none != a && none != other);
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
