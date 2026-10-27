//! What takes the space of a space, as one person is allowed to see it: totals by kind of file, the trash and earlier
//! versions, the largest files, files with the same content, folders nobody has touched, and the tree of folders by size.
//!
//! One pass over the space's items, streamed: memory holds the folders (a record each), the top lists and the
//! counters, never the files. Items the person can't see (restricted folders, other people's personal spaces) are
//! left out of every figure, with the same predicate the lists use.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BinaryHeap, HashMap},
};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sqlx::SqliteConnection;

use crate::{
    auth::User,
    error::AppResult,
    state::AppState,
    tree::{self, Drive, SpaceKind},
    util::now,
};

/// Files in the largest list
pub const LARGEST: usize = 100;
/// Groups of copies listed, and copies shown of each
pub const GROUPS: usize = 100;
pub const COPIES_SHOWN: usize = 10;
/// Folders kept for the list of folders by size (the page filters them by age)
pub const FOLDERS_LISTED: usize = 300;
/// The treemap: how many folders deep, and how many children a folder shows before the rest are one "other"
pub const TREE_DEPTH: usize = 3;
pub const TREE_WIDTH: usize = 20;
/// A folder space has no hashes: files that share a size are read to compare them, up to this many files and bytes
const HASH_FILES: usize = 2000;
const HASH_BYTES: u64 = 20 * 1024 * 1024 * 1024;
/// Files smaller than this aren't looked at for copies (a few hundred bytes of the same content is not worth a line)
const MIN_DUPLICATE: i64 = 1024;
const YEAR: i64 = 365 * 86400;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct KindFigure {
    pub kind: String,
    pub files: i64,
    pub bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct FileRow {
    pub id: String,
    pub name: String,
    pub folder_id: Option<String>,
    /// Where it is, from the space's root ("/", "/Projects/2024")
    pub folder: String,
    pub owner: String,
    pub size: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Group {
    pub size: i64,
    pub copies: i64,
    /// What the copies besides one take: (copies - 1) × size
    pub wasted: i64,
    pub files: Vec<FileRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Duplicates {
    pub groups: Vec<Group>,
    pub wasted_bytes: i64,
    /// A folder space: files with the same size were read to compare them, up to a budget that was reached
    pub partial: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct FolderRow {
    pub id: String,
    pub path: String,
    pub bytes: i64,
    pub files: i64,
    /// The newest change of a file below it (Unix seconds); 0 when it holds none
    pub newest: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TreeNode {
    /// The folder, none for "other" (the folders that didn't fit) and "files here"
    pub id: Option<String>,
    pub name: String,
    pub bytes: i64,
    pub files: i64,
    pub children: Vec<TreeNode>,
    /// "folder", "other" or "files"
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Snapshot {
    pub drive_id: String,
    pub name: String,
    pub taken_at: i64,
    /// A folder space: its files have no hashes
    pub folder_space: bool,
    pub files: i64,
    pub folders: i64,
    pub live_bytes: i64,
    pub kinds: Vec<KindFigure>,
    pub trash_bytes: i64,
    pub trash_files: i64,
    pub version_bytes: i64,
    /// Of `version_bytes`: earlier versions that were replaced more than a year ago
    pub old_version_bytes: i64,
    /// Files not changed for a year
    pub stale_bytes: i64,
    pub stale_files: i64,
    pub largest: Vec<FileRow>,
    pub duplicates: Duplicates,
    pub folders_by_size: Vec<FolderRow>,
    pub tree: TreeNode,
}

/// The kind of a file, for the overview: from its type, else its extension
pub fn kind_of(name: &str, mime: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    if mime.starts_with("image/")
        || ["jpg", "jpeg", "png", "gif", "webp", "bmp", "heic", "heif", "tif", "tiff", "svg", "raw", "cr2", "nef", "arw", "dng"].contains(&ext.as_str())
    {
        "pictures"
    } else if mime.starts_with("video/") || ["mp4", "mov", "mkv", "avi", "wmv", "webm", "m4v", "mpg", "mpeg", "flv"].contains(&ext.as_str()) {
        "video"
    } else if mime.starts_with("audio/") || ["mp3", "wav", "flac", "m4a", "aac", "ogg", "opus", "wma"].contains(&ext.as_str()) {
        "audio"
    } else if ["zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "zst", "iso", "dmg"].contains(&ext.as_str()) || mime.contains("zip") && !mime.contains("openxml")
    {
        "archives"
    } else if mime.starts_with("text/")
        || mime == "application/pdf"
        || mime.contains("officedocument")
        || mime.contains("opendocument")
        || mime.contains("msword")
        || mime.contains("ms-excel")
        || mime.contains("ms-powerpoint")
        || ["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "txt", "md", "csv", "rtf", "json", "xml", "html"].contains(&ext.as_str())
    {
        "documents"
    } else {
        "other"
    }
}

/// The kinds, in the order the page lists them
pub const KINDS: [&str; 5] = ["pictures", "video", "audio", "documents", "archives"];

#[derive(sqlx::FromRow)]
struct Item {
    id: String,
    parent_id: Option<String>,
    kind: String,
    name: String,
    size: i64,
    mime: String,
    updated_at: i64,
    owner_id: i64,
    trashed: bool,
}

/// A folder, with what is in it directly and (after `settle`) below
#[derive(Default, Clone)]
struct Folder {
    parent: Option<String>,
    name: String,
    bytes: i64,
    files: i64,
    newest: i64,
    /// Below, once settled
    total_bytes: i64,
    total_files: i64,
    total_newest: i64,
    children: Vec<String>,
}

/// The path of a folder from the root ("/" for the root itself)
fn path_of(folders: &HashMap<String, Folder>, id: &str, root: &str) -> String {
    let mut names = Vec::new();
    let mut at = id;
    while at != root {
        let Some(f) = folders.get(at) else { break };
        names.push(f.name.as_str());
        match &f.parent {
            Some(p) => at = p,
            None => break,
        }
    }
    names.reverse();
    format!("/{}", names.join("/"))
}

/// Totals of every folder, from the leaves up: the folders' own figures into their parents'
fn settle(folders: &mut HashMap<String, Folder>, root: &str) {
    let ids: Vec<String> = folders.keys().cloned().collect();
    for id in &ids {
        let parent = folders[id].parent.clone();
        if let Some(p) = parent
            && let Some(parent) = folders.get_mut(&p)
        {
            parent.children.push(id.clone());
        }
    }
    // Post-order from the root, without recursion (a deep tree must not overflow the stack)
    let mut order = Vec::with_capacity(folders.len());
    let mut stack = vec![root.to_string()];
    while let Some(id) = stack.pop() {
        if let Some(f) = folders.get(&id) {
            stack.extend(f.children.iter().cloned());
            order.push(id);
        }
    }
    for id in order.iter().rev() {
        let (children, bytes, files, newest) = {
            let f = &folders[id];
            (f.children.clone(), f.bytes, f.files, f.newest)
        };
        let (mut tb, mut tf, mut tn) = (bytes, files, newest);
        for c in children {
            let child = &folders[&c];
            tb += child.total_bytes;
            tf += child.total_files;
            tn = tn.max(child.total_newest);
        }
        let f = folders.get_mut(id).expect("a folder of the order");
        (f.total_bytes, f.total_files, f.total_newest) = (tb, tf, tn);
    }
}

/// The folders as a tree for the treemap, `TREE_DEPTH` levels deep and `TREE_WIDTH` wide
fn tree_of(folders: &HashMap<String, Folder>, id: &str, name: &str, depth: usize) -> TreeNode {
    let f = &folders[id];
    let mut node =
        TreeNode { id: Some(id.to_string()), name: name.to_string(), bytes: f.total_bytes, files: f.total_files, children: vec![], kind: "folder".into() };
    if depth >= TREE_DEPTH {
        return node;
    }
    let mut kids: Vec<&String> = f.children.iter().filter(|c| folders[*c].total_bytes > 0 || folders[*c].total_files > 0).collect();
    kids.sort_by_key(|c| (Reverse(folders[*c].total_bytes), folders[*c].name.clone()));
    for c in kids.iter().take(TREE_WIDTH) {
        node.children.push(tree_of(folders, c, &folders[*c].name, depth + 1));
    }
    let (rest_bytes, rest_files) = kids.iter().skip(TREE_WIDTH).fold((0, 0), |(b, n), c| (b + folders[*c].total_bytes, n + folders[*c].total_files));
    if rest_files > 0 || rest_bytes > 0 {
        node.children.push(TreeNode { id: None, name: String::new(), bytes: rest_bytes, files: rest_files, children: vec![], kind: "other".into() });
    }
    if f.files > 0 {
        node.children.push(TreeNode { id: None, name: String::new(), bytes: f.bytes, files: f.files, children: vec![], kind: "files".into() });
    }
    node
}

/// The figures of the space `drive`, for `user`, who has to be allowed to see them (the caller checks)
pub async fn compute(st: &AppState, conn: &mut SqliteConnection, user: &User, drive: &Drive) -> AppResult<Snapshot> {
    let t = now();
    let folder_space = drive.is_folder();
    // A personal space is its owner's alone; in the others each item is checked, as in every list
    let visible = if drive.kind == SpaceKind::Personal { "1".to_string() } else { tree::visible_sql(user, "n.id") };
    let sql = format!(
        "SELECT n.id, n.parent_id, n.kind, n.name, n.size, n.mime, n.updated_at, n.owner_id, n.trashed_at IS NOT NULL AS trashed
         FROM nodes n WHERE n.drive_id = ?1 AND {visible}"
    );
    let mut folders: HashMap<String, Folder> = HashMap::new();
    let mut kinds: BTreeMap<&'static str, KindFigure> = BTreeMap::new();
    let mut heap: BinaryHeap<Reverse<(i64, String)>> = BinaryHeap::new();
    let mut largest: HashMap<String, Item> = HashMap::new();
    let mut s = Snapshot { drive_id: drive.id.clone(), name: drive.name.clone(), taken_at: t, folder_space, ..Default::default() };
    {
        let mut rows = sqlx::query_as::<_, Item>(sqlx::AssertSqlSafe(sql.as_str())).bind(&drive.id).fetch(&mut *conn);
        while let Some(row) = rows.next().await {
            let n = row?;
            if n.trashed {
                if n.kind == "file" {
                    s.trash_bytes += n.size;
                    s.trash_files += 1;
                }
                continue;
            }
            if n.kind == "folder" {
                s.folders += 1;
                let f = folders.entry(n.id.clone()).or_default();
                f.parent = n.parent_id.clone();
                f.name = n.name.clone();
                continue;
            }
            s.files += 1;
            s.live_bytes += n.size;
            let kind = kind_of(&n.name, &n.mime);
            let k = kinds.entry(kind).or_insert_with(|| KindFigure { kind: kind.to_string(), ..Default::default() });
            k.files += 1;
            k.bytes += n.size;
            if t - n.updated_at > YEAR {
                s.stale_bytes += n.size;
                s.stale_files += 1;
            }
            if let Some(p) = &n.parent_id {
                let f = folders.entry(p.clone()).or_default();
                f.bytes += n.size;
                f.files += 1;
                f.newest = f.newest.max(n.updated_at);
            }
            // The largest: a heap of the smallest of them, which the next one replaces
            if heap.len() < LARGEST || heap.peek().is_some_and(|Reverse((size, _))| n.size > *size) {
                if heap.len() >= LARGEST
                    && let Some(Reverse((_, gone))) = heap.pop()
                {
                    largest.remove(&gone);
                }
                heap.push(Reverse((n.size, n.id.clone())));
                largest.insert(n.id.clone(), n);
            }
        }
    }
    s.kinds = KINDS.iter().chain(["other"].iter()).map(|k| kinds.remove(k).unwrap_or_else(|| KindFigure { kind: (*k).to_string(), ..Default::default() })).collect();

    // Earlier versions, in what the person may see
    let versions_sql = format!(
        "SELECT COALESCE(SUM(v.size), 0), COALESCE(SUM(CASE WHEN v.created_at < ?2 THEN v.size ELSE 0 END), 0)
         FROM node_versions v JOIN nodes n ON n.id = v.node_id WHERE n.drive_id = ?1 AND {visible}"
    );
    let (all, old): (i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(versions_sql.as_str())).bind(&drive.id).bind(t - YEAR).fetch_one(&mut *conn).await?;
    (s.version_bytes, s.old_version_bytes) = (all, old);

    let root = drive.root_id.clone();
    settle(&mut folders, &root);
    s.tree = if folders.contains_key(&root) {
        tree_of(&folders, &root, &drive.name, 0)
    } else {
        TreeNode { name: drive.name.clone(), kind: "folder".into(), ..Default::default() }
    };
    let mut by_size: Vec<(&String, &Folder)> = folders.iter().filter(|(id, f)| **id != root && f.total_files > 0).collect();
    by_size.sort_by_key(|(id, f)| (Reverse(f.total_bytes), (*id).clone()));
    s.folders_by_size = by_size
        .into_iter()
        .take(FOLDERS_LISTED)
        .map(|(id, f)| FolderRow { id: id.clone(), path: path_of(&folders, id, &root), bytes: f.total_bytes, files: f.total_files, newest: f.total_newest })
        .collect();

    let owners = owner_names(conn, largest.values().map(|n| n.owner_id)).await?;
    let mut top: Vec<&Item> = largest.values().collect();
    top.sort_by_key(|n| (Reverse(n.size), n.id.clone()));
    s.largest = top.into_iter().map(|n| file_row(n, &folders, &root, &owners)).collect();

    s.duplicates = if folder_space {
        duplicates_in_folders(st, conn, drive, &visible, &folders, &root).await?
    } else {
        duplicates_in_store(conn, drive, &visible, &folders, &root).await?
    };
    Ok(s)
}

fn file_row(n: &Item, folders: &HashMap<String, Folder>, root: &str, owners: &HashMap<i64, String>) -> FileRow {
    FileRow {
        id: n.id.clone(),
        name: n.name.clone(),
        folder_id: n.parent_id.clone(),
        folder: n.parent_id.as_deref().map_or_else(|| "/".to_string(), |p| path_of(folders, p, root)),
        owner: owners.get(&n.owner_id).cloned().unwrap_or_default(),
        size: n.size,
        updated_at: n.updated_at,
    }
}

async fn owner_names(conn: &mut SqliteConnection, ids: impl Iterator<Item = i64>) -> AppResult<HashMap<i64, String>> {
    let ids: std::collections::BTreeSet<i64> = ids.collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    let sql = format!("SELECT id, COALESCE(NULLIF(display_name, ''), username) FROM users WHERE id IN ({list})");
    let rows: Vec<(i64, String)> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).fetch_all(conn).await?;
    Ok(rows.into_iter().collect())
}

/// The files of one group of copies, newest first, up to `COPIES_SHOWN`
async fn copies(
    conn: &mut SqliteConnection,
    drive: &Drive,
    visible: &str,
    where_sql: &str,
    args: &[&str],
    folders: &HashMap<String, Folder>,
    root: &str,
) -> AppResult<Vec<FileRow>> {
    let sql = format!(
        "SELECT n.id, n.parent_id, n.kind, n.name, n.size, n.mime, n.updated_at, n.owner_id, n.trashed_at IS NOT NULL AS trashed
         FROM nodes n WHERE n.drive_id = ?1 AND n.kind = 'file' AND n.trashed_at IS NULL AND {where_sql} AND {visible} ORDER BY n.updated_at DESC, n.id LIMIT {COPIES_SHOWN}"
    );
    let mut q = sqlx::query_as::<_, Item>(sqlx::AssertSqlSafe(sql.as_str())).bind(&drive.id);
    for a in args {
        q = q.bind(*a);
    }
    let items = q.fetch_all(&mut *conn).await?;
    let owners = owner_names(conn, items.iter().map(|n| n.owner_id)).await?;
    Ok(items.iter().map(|n| file_row(n, folders, root, &owners)).collect())
}

/// The content store keeps each content once, so copies are files that share a hash: they take quota, not disk
async fn duplicates_in_store(conn: &mut SqliteConnection, drive: &Drive, visible: &str, folders: &HashMap<String, Folder>, root: &str) -> AppResult<Duplicates> {
    let sql = format!(
        "SELECT n.blob_hash, COUNT(*), MAX(n.size) FROM nodes n
         WHERE n.drive_id = ?1 AND n.kind = 'file' AND n.trashed_at IS NULL AND n.blob_hash IS NOT NULL AND n.size >= {MIN_DUPLICATE} AND {visible}
         GROUP BY n.blob_hash HAVING COUNT(*) > 1 ORDER BY (COUNT(*) - 1) * MAX(n.size) DESC, n.blob_hash LIMIT {GROUPS}"
    );
    let found: Vec<(String, i64, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(&drive.id).fetch_all(&mut *conn).await?;
    let mut out = Duplicates::default();
    for (hash, count, size) in found {
        let files = copies(conn, drive, visible, "n.blob_hash = ?2", &[&hash], folders, root).await?;
        let wasted = (count - 1) * size;
        out.wasted_bytes += wasted;
        out.groups.push(Group { size, copies: count, wasted, files });
    }
    Ok(out)
}

/// A folder space's files have no hashes: those that share a size are read and hashed (within a budget), and the
/// ones with the same hash are copies
async fn duplicates_in_folders(
    st: &AppState,
    conn: &mut SqliteConnection,
    drive: &Drive,
    visible: &str,
    folders: &HashMap<String, Folder>,
    root: &str,
) -> AppResult<Duplicates> {
    let sizes_sql = format!(
        "SELECT n.size, COUNT(*) FROM nodes n WHERE n.drive_id = ?1 AND n.kind = 'file' AND n.trashed_at IS NULL AND n.size >= {MIN_DUPLICATE} AND {visible}
         GROUP BY n.size HAVING COUNT(*) > 1 ORDER BY n.size * COUNT(*) DESC, n.size LIMIT 400"
    );
    let candidates: Vec<(i64, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(sizes_sql.as_str())).bind(&drive.id).fetch_all(&mut *conn).await?;
    let (mut read_files, mut read_bytes, mut partial) = (0usize, 0u64, false);
    let mut groups: Vec<(i64, Vec<String>)> = Vec::new();
    'sizes: for (size, _) in candidates {
        let ids: Vec<(String,)> =
            sqlx::query_as("SELECT n.id FROM nodes n WHERE n.drive_id = ?1 AND n.kind = 'file' AND n.trashed_at IS NULL AND n.size = ?2 ORDER BY n.id")
                .bind(&drive.id)
                .bind(size)
                .fetch_all(&mut *conn)
                .await?;
        let mut by_hash: HashMap<String, Vec<String>> = HashMap::new();
        for (id,) in ids {
            if read_files >= HASH_FILES || read_bytes + size as u64 > HASH_BYTES {
                partial = true;
                break 'sizes;
            }
            let Some(node) = tree::get_node(conn, &id).await? else { continue };
            let Ok(source) = crate::files::Source::of(&node) else { continue };
            let Ok(mut reader) = source.open(st, 0, size as u64).await else { continue };
            let Ok((hash, read)) = crate::hashing::read_async(&mut reader).await else { continue };
            read_files += 1;
            read_bytes += read;
            // What changed on the disk since it was indexed isn't the size the group is about
            if read as i64 == size {
                by_hash.entry(hash).or_default().push(id);
            }
        }
        groups.extend(by_hash.into_values().filter(|ids| ids.len() > 1).map(|ids| (size, ids)));
    }
    groups.sort_by_key(|(size, ids)| (Reverse((ids.len() as i64 - 1) * size), ids[0].clone()));
    let mut out = Duplicates { partial, ..Default::default() };
    for (size, ids) in groups.into_iter().take(GROUPS) {
        let list = ids.iter().map(|i| format!("'{}'", i.replace('\'', "''"))).collect::<Vec<_>>().join(",");
        let files = copies(conn, drive, visible, &format!("n.id IN ({list})"), &[], folders, root).await?;
        let wasted = (ids.len() as i64 - 1) * size;
        out.wasted_bytes += wasted;
        out.groups.push(Group { size, copies: ids.len() as i64, wasted, files });
    }
    Ok(out)
}
