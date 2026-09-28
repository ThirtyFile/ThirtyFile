//! `thirtyfile convert`: turns the spaces 0.1 and 0.2 keep in the content store on this server's disks into folder
//! spaces (folders.rs), whose files are ordinary files in ordinary folders.
//!
//! - Each space gets a folder in the folder of its storage location (the storage folder for the built-in one):
//!   `company`, `teams/<space name>` or `users/<user name>`
//! - Every file is written to its path: a hard link to the stored content where both are on the same disk, so it takes
//!   no extra space (content several files share becomes several links to one copy), else a copy checked against its
//!   SHA-256. The trash goes to `.thirtyfile-trash/<trash id>/` and earlier versions to
//!   `.thirtyfile-versions/<file id>/<version id>`, as in every folder space
//! - Names a folder can't hold as they are (two that differ only in letter case, names scans skip, names too long for
//!   the disk) get another one, listed in the report
//! - Once everything of a space is in place and checked, the space becomes a folder space in one transaction. Items
//!   keep their ids, and with them their shares, permissions, favourites and versions; content nothing uses any more is
//!   then removed from the content store
//! - What was written is recorded (`convert_files`, migration 0071), so a conversion that stopped halfway skips it when
//!   it runs again. Until the space is converted its folder belongs to the conversion: whatever else is in it goes
//! - Spaces on S3, SFTP or FTP stay as they are. Files in the storage folders that the database doesn't know are
//!   listed in the report and never touched
//!
//! It runs with ThirtyFile stopped (the data folder's lock, see main.rs), so nothing changes the spaces meanwhile.

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::{
    folders::ignored,
    fsops::{self, TRASH_DIR},
    storage::{Storage, is_hash},
    util::{new_id, now, numbered_name, split_name},
    versions::VERSIONS_DIR,
};

type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Files recorded per transaction
const BATCH: usize = 500;
/// The longest name most file systems hold, in bytes
const MAX_NAME_BYTES: usize = 255;

#[derive(Debug, Default)]
pub struct Report {
    pub spaces: Vec<SpaceReport>,
    /// Items in the storage folders the database doesn't know: left as they are
    pub unknown: Vec<PathBuf>,
}

#[derive(Debug, Default)]
pub struct SpaceReport {
    pub name: String,
    pub kind: String,
    /// The space's folder
    pub folder: Option<PathBuf>,
    /// Why the space stays in the content store
    pub skipped: Option<String>,
    pub converted: bool,
    /// Files hard-linked, copied, and already in place from an earlier run
    pub linked: usize,
    pub copied: usize,
    pub already: usize,
    /// Items that got another name on disk
    pub renamed: Vec<String>,
    /// What keeps the space from being converted this time
    pub problems: Vec<String>,
}

impl Report {
    /// Spaces that couldn't be converted because of a problem (not those that stay by design)
    pub fn problems(&self) -> usize {
        self.spaces.iter().filter(|s| !s.problems.is_empty()).count()
    }
}

#[derive(sqlx::FromRow)]
struct Space {
    id: String,
    name: String,
    kind: String,
    root_id: String,
    /// Chosen by an earlier run that didn't finish
    source_path: Option<String>,
    location: String,
    owner: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct Item {
    id: String,
    parent_id: Option<String>,
    kind: String,
    name: String,
    blob_hash: Option<String>,
    size: i64,
    trash_id: Option<String>,
    trash_root: bool,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct Version {
    id: String,
    node_id: String,
    blob_hash: String,
    size: i64,
}

/// A file to put in the space's folder: a node's content, or one of its versions
#[derive(Debug)]
struct Want {
    node: String,
    version: Option<String>,
    rel: String,
    hash: Option<String>,
    size: i64,
}

#[derive(Debug, Default)]
struct Plan {
    /// Folders, parents before their contents: (node id, path)
    folders: Vec<(String, String)>,
    files: Vec<Want>,
    /// Every folder on disk, the trash and versions folders included
    dirs: HashSet<String>,
    /// Nodes with another name on disk
    names: HashMap<String, String>,
}

/// Spaces that could be converted: in the content store on a folder of this server, with at least one file
pub async fn pending(db: &SqlitePool) -> Result<i64, sqlx::Error> {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM drives d
         JOIN storage_locations l ON l.id = COALESCE(d.location_id, (SELECT id FROM storage_locations WHERE is_default = 1), 'local')
         WHERE d.mode = 'store' AND l.kind = 'local' AND EXISTS (SELECT 1 FROM nodes n WHERE n.drive_id = d.id AND n.kind = 'file')",
    )
    .fetch_one(db)
    .await?;
    Ok(n)
}

/// Converts every space it can. `builtin`: the folder the built-in location's spaces go to (its storage folder; for
/// 0.1, whose content is in /data/blobs, the storage folder set now). `dry_run`: only works out what would happen.
pub async fn run(
    db: &SqlitePool,
    storages: &HashMap<String, Arc<dyn Storage>>,
    builtin: &Path,
    data_dir: &Path,
    dry_run: bool,
    progress: impl Fn(&str),
) -> Res<Report> {
    let kinds: HashMap<String, String> = sqlx::query_as("SELECT id, kind FROM storage_locations").fetch_all(db).await?.into_iter().collect();
    let spaces: Vec<Space> = sqlx::query_as(
        "SELECT d.id, d.name, d.kind, d.root_id, d.source_path,
                COALESCE(d.location_id, (SELECT id FROM storage_locations WHERE is_default = 1), 'local') AS location,
                COALESCE((SELECT username FROM users WHERE id = d.owner_id), '') AS owner
         FROM drives d WHERE d.mode = 'store' ORDER BY d.created_at, d.id",
    )
    .fetch_all(db)
    .await?;
    let mut report = Report::default();
    for space in spaces {
        progress(&shown_space(&space));
        let r = match convert_space(db, storages, &kinds, builtin, data_dir, &space, dry_run).await {
            Ok(r) => r,
            Err(e) => SpaceReport { name: shown_space(&space), kind: space.kind.clone(), problems: vec![e.to_string()], ..Default::default() },
        };
        report.spaces.push(r);
    }
    // Where every space's content is gone, so are the store's own folders (ab/cd/)
    let mut roots: Vec<PathBuf> = storages.values().filter_map(|s| s.local_root().map(Path::to_path_buf)).collect();
    if !dry_run {
        for root in &roots {
            remove_empty_store_folders(root);
        }
    }
    roots.push(builtin.to_path_buf());
    roots.sort();
    roots.dedup();
    let folders: Vec<PathBuf> = sqlx::query_as::<_, (String,)>("SELECT source_path FROM drives WHERE source_path IS NOT NULL")
        .fetch_all(db)
        .await?
        .into_iter()
        .map(|(p,)| PathBuf::from(p))
        .collect();
    let mut known: HashSet<String> = sqlx::query_as::<_, (String,)>("SELECT hash FROM blobs").fetch_all(db).await?.into_iter().map(|(h,)| h).collect();
    known.extend(sqlx::query_as::<_, (String,)>("SELECT hash FROM pending_blob_deletes").fetch_all(db).await?.into_iter().map(|(h,)| h));
    for root in roots {
        unknown_in(&root, &folders, &known, true, &mut report.unknown);
    }
    Ok(report)
}

async fn convert_space(
    db: &SqlitePool,
    storages: &HashMap<String, Arc<dyn Storage>>,
    kinds: &HashMap<String, String>,
    builtin: &Path,
    data_dir: &Path,
    space: &Space,
    dry_run: bool,
) -> Res<SpaceReport> {
    let mut r = SpaceReport { name: shown_space(space), kind: space.kind.clone(), ..Default::default() };
    if kinds.get(&space.location).map(String::as_str) != Some("local") {
        r.skipped = Some("its files are kept on S3, SFTP or FTP".into());
        return Ok(r);
    }
    let root = if space.location == crate::locations::BUILTIN {
        builtin.to_path_buf()
    } else {
        match storages.get(&space.location).and_then(|s| s.local_root()) {
            Some(p) => p.to_path_buf(),
            None => {
                r.skipped = Some("its storage location can't be used right now".into());
                return Ok(r);
            }
        }
    };
    let items: Vec<Item> = sqlx::query_as("SELECT id, parent_id, kind, name, blob_hash, size, trash_id, trash_root FROM nodes WHERE drive_id = ?")
        .bind(&space.id)
        .fetch_all(db)
        .await?;
    let versions: Vec<Version> = sqlx::query_as(
        "SELECT v.id, v.node_id, v.blob_hash, v.size FROM node_versions v JOIN nodes n ON n.id = v.node_id
         WHERE n.drive_id = ? AND v.blob_hash IS NOT NULL ORDER BY v.node_id, v.created_at, v.id",
    )
    .bind(&space.id)
    .fetch_all(db)
    .await?;
    // Every content the space uses must be in a folder of this server
    let used: Vec<(String, String)> = sqlx::query_as(
        "SELECT hash, location_id FROM blobs WHERE hash IN (SELECT blob_hash FROM nodes WHERE drive_id = ?1)
            OR hash IN (SELECT v.blob_hash FROM node_versions v JOIN nodes n ON n.id = v.node_id WHERE n.drive_id = ?1)",
    )
    .bind(&space.id)
    .fetch_all(db)
    .await?;
    let mut sources: HashMap<String, PathBuf> = HashMap::new();
    for (hash, location) in used {
        match storages.get(&location).and_then(|s| s.local_file(&hash)) {
            Some(p) => {
                sources.insert(hash, p);
            }
            None => {
                r.skipped = Some(if kinds.get(&location).is_some_and(|k| k == "local") {
                    "some of its files are in a storage location that can't be used right now".into()
                } else {
                    "some of its files are kept on S3, SFTP or FTP".into()
                });
                return Ok(r);
            }
        }
    }

    let plan = plan(&space.root_id, &items, &versions, &mut r);
    let folder = match &space.source_path {
        Some(p) => PathBuf::from(p),
        None => choose_folder(db, &root, space, dry_run).await?,
    };
    r.folder = Some(folder.clone());
    if dry_run {
        return Ok(r);
    }

    // Folders first, parents before their contents
    std::fs::create_dir_all(&folder)?;
    for (_, rel) in &plan.folders {
        make_dir(&under(&folder, rel))?;
    }
    let records: HashMap<String, (String, fsops::Stat)> = sqlx::query_as::<_, (String, String, i64, i64, i64, i64)>(
        "SELECT fs_path, hash, fs_dev, fs_ino, fs_size, fs_mtime_ns FROM convert_files WHERE drive_id = ?",
    )
    .bind(&space.id)
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|(rel, hash, dev, ino, size, mtime_ns)| (rel, (hash, fsops::Stat { is_dir: false, dev, ino, size, mtime_ns })))
    .collect();
    let mut written = Vec::new();
    for w in &plan.files {
        let dest = under(&folder, &w.rel);
        let hash = w.hash.clone().unwrap_or_default();
        if let Some((h, s)) = records.get(&w.rel)
            && *h == hash
            && fsops::stat(&dest).is_ok_and(|now| same(&now, s))
        {
            r.already += 1;
            continue;
        }
        let src = match &w.hash {
            Some(h) => match sources.get(h) {
                Some(p) => Some(p.as_path()),
                None => {
                    r.problems.push(format!("{}: the database has no record of its content", shown(w)));
                    continue;
                }
            },
            None if w.size == 0 => None,
            None => {
                r.problems.push(format!("{}: it has no content", shown(w)));
                continue;
            }
        };
        match put(src, &dest, w.hash.as_deref(), w.size) {
            Ok(true) => r.linked += 1,
            Ok(false) => r.copied += 1,
            Err(e) => {
                r.problems.push(format!("{}: {e}", shown(w)));
                continue;
            }
        }
        written.push((w.rel.clone(), hash, fsops::stat(&dest)?));
        if written.len() >= BATCH {
            record(db, &space.id, std::mem::take(&mut written)).await?;
        }
    }
    record(db, &space.id, written).await?;
    // The folder is the conversion's: what doesn't belong there (left by an earlier run for items renamed or deleted
    // since) goes
    let files: HashSet<&str> = plan.files.iter().map(|w| w.rel.as_str()).collect();
    prune(&folder, "", &plan.dirs, &files)?;
    // Checked once more before the space relies on it
    for w in &plan.files {
        match std::fs::symlink_metadata(under(&folder, &w.rel)) {
            Ok(m) if m.is_file() && m.len() == w.size as u64 => {}
            _ if r.problems.iter().any(|p| p.starts_with(&shown(w))) => {}
            _ => r.problems.push(format!("{}: it isn't complete in the folder", shown(w))),
        }
    }
    if !r.problems.is_empty() {
        return Ok(r);
    }
    let removed = switch(db, space, &folder, &plan).await?;
    r.converted = true;
    // The content nothing uses any more goes from the store; what can't be removed now stays on the list of pending
    // deletions, which the server works through
    for (hash, location) in removed {
        let Some(storage) = storages.get(&location) else { continue };
        if storage.delete(&hash).await.is_ok() {
            sqlx::query("DELETE FROM pending_blob_deletes WHERE hash = ? AND location_id = ?").bind(&hash).bind(&location).execute(db).await?;
            if is_hash(&hash) {
                let _ = std::fs::remove_file(data_dir.join("thumbs").join(&hash[0..2]).join(format!("{hash}.jpg")));
            }
        }
    }
    Ok(r)
}

/// A personal space by its user's name: they are all called "My files"
fn shown_space(space: &Space) -> String {
    if space.kind == "personal" && !space.owner.is_empty() { space.owner.clone() } else { space.name.clone() }
}

/// Where a space's files go: `company`, `teams/<name>` or `users/<user name>` in the location's folder, with a number
/// when that folder already exists (made by hand, or another space with the same name). Remembered at once, so a
/// conversion run again continues in the same folder.
async fn choose_folder(db: &SqlitePool, root: &Path, space: &Space, dry_run: bool) -> Res<PathBuf> {
    let (parent, name) = match space.kind.as_str() {
        "company" => (root.to_path_buf(), "company".to_string()),
        "team" => (root.join("teams"), folder_name(&space.name, &space.id)),
        _ => (root.join("users"), folder_name(if space.owner.is_empty() { &space.name } else { &space.owner }, &space.id)),
    };
    for n in 1..10_000u32 {
        let candidate = parent.join(if n == 1 { name.clone() } else { format!("{name} ({n})") });
        let path = candidate.to_string_lossy().into_owned();
        let (taken,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM drives WHERE source_path = ?").bind(&path).fetch_one(db).await?;
        if taken > 0 || std::fs::symlink_metadata(&candidate).is_ok() {
            continue;
        }
        if !dry_run {
            sqlx::query("UPDATE drives SET source_path = ? WHERE id = ? AND mode = 'store'").bind(&path).bind(&space.id).execute(db).await?;
        }
        return Ok(candidate);
    }
    Err(format!("no free folder for the space \"{}\" in {}", space.name, parent.display()).into())
}

/// A space's name as a folder name
fn folder_name(name: &str, id: &str) -> String {
    let (name, _) = disk_name(name.trim(), true);
    let name = name.trim_end_matches(['.', ' ']);
    if name.is_empty() { id.to_string() } else { name.to_string() }
}

/// Where every item goes, below the space's folder
fn plan(root_id: &str, items: &[Item], versions: &[Version], r: &mut SpaceReport) -> Plan {
    let mut children: HashMap<&str, Vec<&Item>> = HashMap::new();
    // The trash of the space, by trash id: each deleted item goes to a folder of its own
    let mut trash: BTreeMap<String, Vec<&Item>> = BTreeMap::new();
    for it in items.iter().filter(|it| it.id != root_id) {
        if it.trash_root {
            trash.entry(it.trash_id.clone().unwrap_or_else(|| it.id.clone())).or_default().push(it);
        } else if let Some(p) = &it.parent_id {
            children.entry(p.as_str()).or_default().push(it);
        }
    }
    let mut p = Plan::default();
    p.dirs.insert(String::new());
    p.folders.push((root_id.to_string(), String::new()));
    let mut queue: VecDeque<(&str, String)> = VecDeque::new();
    assign("", children.remove(root_id).unwrap_or_default(), &mut p, &mut queue, r);
    for (id, list) in trash {
        let dir = format!("{TRASH_DIR}/{id}");
        p.dirs.insert(TRASH_DIR.to_string());
        p.dirs.insert(dir.clone());
        assign(&dir, list, &mut p, &mut queue, r);
    }
    while let Some((id, rel)) = queue.pop_front() {
        assign(&rel, children.remove(id).unwrap_or_default(), &mut p, &mut queue, r);
    }
    let placed: HashSet<&str> = p.files.iter().map(|w| w.node.as_str()).chain(p.folders.iter().map(|(id, _)| id.as_str())).collect();
    for it in items.iter().filter(|it| !placed.contains(it.id.as_str())) {
        r.problems.push(format!("\"{}\": the folder it belongs to is missing", it.name));
    }
    let mut files = Vec::new();
    for v in versions.iter().filter(|v| placed.contains(v.node_id.as_str())) {
        let dir = format!("{VERSIONS_DIR}/{}", v.node_id);
        p.dirs.insert(VERSIONS_DIR.to_string());
        p.dirs.insert(dir.clone());
        let rel = format!("{dir}/{}", v.id);
        files.push(Want { node: v.node_id.clone(), version: Some(v.id.clone()), rel, hash: Some(v.blob_hash.clone()), size: v.size });
    }
    p.files.extend(files);
    p
}

/// Names the items of one folder on disk: unique regardless of letter case (so the folder also works on disks that
/// ignore it), and something a scan indexes
fn assign<'a>(dir: &str, mut kids: Vec<&'a Item>, p: &mut Plan, queue: &mut VecDeque<(&'a str, String)>, r: &mut SpaceReport) {
    kids.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    let mut taken: HashSet<String> = HashSet::new();
    for it in kids {
        let is_dir = it.kind == "folder";
        let (base, why) = disk_name(&it.name, is_dir);
        let mut name = base.clone();
        let mut n = 0;
        while taken.contains(&name.to_lowercase()) || ignored(&name) {
            n += 1;
            name = numbered_name(&base, n, is_dir);
        }
        taken.insert(name.to_lowercase());
        if name != it.name {
            let why = if n > 0 { "another item in the folder has the same name apart from letter case" } else { why.unwrap_or_default() };
            r.renamed.push(format!("{}: saved as \"{name}\" ({why})", fsops::child_rel(dir, &it.name)));
            p.names.insert(it.id.clone(), name.clone());
        }
        let rel = fsops::child_rel(dir, &name);
        if is_dir {
            p.dirs.insert(rel.clone());
            p.folders.push((it.id.clone(), rel.clone()));
            queue.push_back((it.id.as_str(), rel));
        } else {
            p.files.push(Want { node: it.id.clone(), version: None, rel, hash: it.blob_hash.clone(), size: it.size });
        }
    }
}

/// The name an item gets on disk, and why it differs: characters a file name can't have become `_`, names longer
/// than disks allow are shortened, and names scans skip (`Thumbs.db`, `~$…` and other temporary files) get an
/// underscore, as the item would otherwise disappear from the space
fn disk_name(name: &str, is_dir: bool) -> (String, Option<&'static str>) {
    let mut why = None;
    let mut out: String = name.chars().map(|c| if c.is_control() || c == '/' || c == '\\' { '_' } else { c }).collect();
    if out != name || out.is_empty() || out == "." || out == ".." {
        if out.is_empty() || out == "." || out == ".." {
            out = format!("_{out}");
        }
        why = Some("it has characters a file name can't have");
    }
    if out.len() > MAX_NAME_BYTES {
        let (stem, ext) = split_name(&out, is_dir);
        let ext = if ext.len() <= 32 { ext } else { "" };
        // Room for a number, should the shorter name be taken
        let mut cut = (MAX_NAME_BYTES - ext.len() - 12).min(stem.len());
        while !stem.is_char_boundary(cut) {
            cut -= 1;
        }
        out = format!("{}{ext}", &stem[..cut]);
        why = Some("the name is too long for the disk");
    }
    if ignored(&out) {
        out = format!("_{out}");
        if ignored(&out) {
            out.push('_');
        }
        why = Some("scans skip names like this one");
    }
    (out, why)
}

fn under(top: &Path, rel: &str) -> PathBuf {
    if rel.is_empty() { top.to_path_buf() } else { top.join(rel) }
}

fn shown(w: &Want) -> String {
    match &w.version {
        Some(_) => format!("{} (an earlier version of a file)", w.rel),
        None => w.rel.clone(),
    }
}

fn same(a: &fsops::Stat, b: &fsops::Stat) -> bool {
    (a.dev, a.ino, a.size, a.mtime_ns) == (b.dev, b.ino, b.size, b.mtime_ns)
}

/// A folder on disk; a file in its place (from an earlier run, for an item that was a file then) goes
fn make_dir(path: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(path).is_ok_and(|m| !m.is_dir()) {
        std::fs::remove_file(path)?;
    }
    std::fs::create_dir_all(path)
}

/// Puts content at `dest`: a hard link to the stored file where the disk allows one (true), else a copy checked
/// against the content's SHA-256 (false). It is written under a temporary name first, so a file at `dest` is always
/// complete.
fn put(src: Option<&Path>, dest: &Path, hash: Option<&str>, size: i64) -> io::Result<bool> {
    if let Some(dir) = dest.parent() {
        make_dir(dir)?;
    }
    let tmp = dest.with_file_name(format!(".thirtyfile-convert-{}", new_id()));
    let placed = write_temp(src, &tmp, hash, size).and_then(|linked| {
        if std::fs::symlink_metadata(dest).is_ok_and(|m| m.is_dir()) {
            std::fs::remove_dir_all(dest)?;
        }
        std::fs::rename(&tmp, dest)?;
        Ok(linked)
    });
    if placed.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    placed
}

fn write_temp(src: Option<&Path>, tmp: &Path, hash: Option<&str>, size: i64) -> io::Result<bool> {
    let Some(src) = src else {
        std::fs::File::create(tmp)?.sync_all()?;
        return Ok(false);
    };
    if std::fs::metadata(src).is_err() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "its content is missing from the storage folder"));
    }
    let linked = if other_disk() { Err(io::ErrorKind::CrossesDevices.into()) } else { std::fs::hard_link(src, tmp) };
    // Otherwise another disk, or one without hard links: copied
    if linked.is_ok() {
        if std::fs::metadata(tmp)?.len() != size as u64 {
            return Err(io::Error::other("the stored content has another size than the database says"));
        }
        return Ok(true);
    }
    let mut from = std::fs::File::open(src)?;
    let mut to = std::fs::File::create(tmp)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut len = 0u64;
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        to.write_all(&buf[..n])?;
        len += n as u64;
    }
    to.sync_all()?;
    if len != size as u64 || hash.is_some_and(|h| hex::encode(hasher.finalize()) != h) {
        return Err(io::Error::other("the stored content doesn't match its fingerprint (SHA-256); `thirtyfile check --verify` lists such files"));
    }
    Ok(false)
}

#[cfg(test)]
thread_local! {
    /// Tests: treat the spaces' folders as being on another disk than the stored content, so files are copied
    static OTHER_DISK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn other_disk() -> bool {
    OTHER_DISK.with(|d| d.get())
}

#[cfg(not(test))]
fn other_disk() -> bool {
    false
}

/// Records files written, so a run after a stop skips them
async fn record(db: &SqlitePool, drive_id: &str, written: Vec<(String, String, fsops::Stat)>) -> Res<()> {
    if written.is_empty() {
        return Ok(());
    }
    let mut tx = db.begin().await?;
    for (rel, hash, s) in written {
        sqlx::query(
            "INSERT INTO convert_files (drive_id, fs_path, hash, fs_dev, fs_ino, fs_size, fs_mtime_ns) VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (drive_id, fs_path) DO UPDATE SET hash = excluded.hash, fs_dev = excluded.fs_dev, fs_ino = excluded.fs_ino,
               fs_size = excluded.fs_size, fs_mtime_ns = excluded.fs_mtime_ns",
        )
        .bind(drive_id)
        .bind(&rel)
        .bind(&hash)
        .bind(s.dev)
        .bind(s.ino)
        .bind(s.size)
        .bind(s.mtime_ns)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Removes what doesn't belong in the space's folder
fn prune(top: &Path, rel: &str, dirs: &HashSet<String>, files: &HashSet<&str>) -> io::Result<()> {
    for entry in std::fs::read_dir(under(top, rel))? {
        let entry = entry?;
        let meta = std::fs::symlink_metadata(entry.path())?;
        let keep = match entry.file_name().to_str() {
            Some(name) => {
                let child = fsops::child_rel(rel, name);
                if meta.is_dir() && dirs.contains(&child) {
                    prune(top, &child, dirs, files)?;
                    true
                } else {
                    !meta.is_dir() && files.contains(child.as_str())
                }
            }
            None => false,
        };
        if !keep {
            if meta.is_dir() { std::fs::remove_dir_all(entry.path())? } else { std::fs::remove_file(entry.path())? }
        }
    }
    Ok(())
}

/// Makes the space a folder space: every item is indexed at its path, versions point to their files, and the content
/// store lets go of the content (returned: content nothing uses any more, to remove from storage)
async fn switch(db: &SqlitePool, space: &Space, folder: &Path, plan: &Plan) -> Res<Vec<(String, String)>> {
    let mut tx = db.begin().await?;
    let mut hashes = HashSet::new();
    let node_rows = plan.folders.iter().map(|(id, rel)| (id, rel)).chain(plan.files.iter().filter(|w| w.version.is_none()).map(|w| (&w.node, &w.rel)));
    for (id, rel) in node_rows {
        let s = fsops::stat(&under(folder, rel))?;
        sqlx::query(
            "UPDATE nodes SET name = COALESCE(?, name), fs_path = ?, fs_dev = ?, fs_ino = ?, fs_size = ?, fs_mtime_ns = ?, blob_hash = NULL
             WHERE id = ?",
        )
        .bind(plan.names.get(id))
        .bind(rel)
        .bind(s.dev)
        .bind(s.ino)
        .bind(s.size)
        .bind(s.mtime_ns)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    for w in &plan.files {
        hashes.extend(w.hash.clone());
        if let Some(v) = &w.version {
            sqlx::query("UPDATE node_versions SET drive_id = ?, fs_path = ?, blob_hash = NULL WHERE id = ?")
                .bind(&space.id)
                .bind(&w.rel)
                .bind(v)
                .execute(&mut *tx)
                .await?;
        }
    }
    // References counted again from what still uses each content (other spaces of the content store, their versions)
    let list = serde_json::to_string(&hashes.into_iter().collect::<Vec<_>>()).unwrap();
    sqlx::query(
        "UPDATE blobs SET refcount = (SELECT COUNT(*) FROM nodes n WHERE n.blob_hash = blobs.hash)
                                   + (SELECT COUNT(*) FROM node_versions v WHERE v.blob_hash = blobs.hash)
         WHERE hash IN (SELECT value FROM json_each(?))",
    )
    .bind(&list)
    .execute(&mut *tx)
    .await?;
    let removed: Vec<(String, String)> =
        sqlx::query_as("DELETE FROM blobs WHERE hash IN (SELECT value FROM json_each(?)) AND refcount <= 0 RETURNING hash, location_id")
            .bind(&list)
            .fetch_all(&mut *tx)
            .await?;
    // Listed for deletion in the same transaction: should removing them fail or stop, the server does it later
    for (hash, location) in &removed {
        sqlx::query(
            "INSERT INTO pending_blob_deletes (hash, location_id, created_at, attempts, last_error) VALUES (?, ?, ?, 0, '')
             ON CONFLICT (hash, location_id) DO NOTHING",
        )
        .bind(hash)
        .bind(location)
        .bind(now())
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("UPDATE drives SET mode = 'folder', source_path = ?, last_scan_at = NULL, scan_report = NULL WHERE id = ?")
        .bind(folder.to_string_lossy())
        .bind(&space.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM convert_files WHERE drive_id = ?").bind(&space.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(removed)
}

/// Removes the content store's folders (`ab/cd/`) that are empty now
fn remove_empty_store_folders(root: &Path) {
    let Ok(read) = std::fs::read_dir(root) else { return };
    for a in read.flatten().filter(|e| store_folder(&e.file_name())) {
        if let Ok(inner) = std::fs::read_dir(a.path()) {
            for b in inner.flatten().filter(|e| store_folder(&e.file_name())) {
                let _ = std::fs::remove_dir(b.path());
            }
        }
        let _ = std::fs::remove_dir(a.path());
    }
}

fn store_folder(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|n| n.len() == 2 && n.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Lists what is in a storage folder without the database knowing it: anything besides the content store's folders and
/// the spaces' folders, and stored content no file uses. `top`: the location's own folder (the content store is only there)
fn unknown_in(dir: &Path, spaces: &[PathBuf], known: &HashSet<String>, top: bool, out: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = read.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        let name = e.file_name();
        if top && store_folder(&name) && path.is_dir() {
            let Ok(inner) = std::fs::read_dir(&path) else { continue };
            for b in inner.flatten() {
                if !(store_folder(&b.file_name()) && b.path().is_dir()) {
                    out.push(b.path());
                    continue;
                }
                let Ok(files) = std::fs::read_dir(b.path()) else { continue };
                for f in files.flatten() {
                    if !f.file_name().to_str().is_some_and(|n| is_hash(n) && known.contains(n)) {
                        out.push(f.path());
                    }
                }
            }
        } else if spaces.contains(&path) || name.to_string_lossy().starts_with(".thirtyfile-") {
            continue;
        } else if spaces.iter().any(|s| s.starts_with(&path)) {
            unknown_in(&path, spaces, known, false, out);
        } else {
            out.push(path);
        }
    }
}

/// Prints the report; returns the number of spaces that have a problem
pub fn print(report: &Report, dry_run: bool) -> usize {
    for s in &report.spaces {
        let kind = match s.kind.as_str() {
            "company" => "company space",
            "team" => "team space",
            _ => "personal space",
        };
        let folder = s.folder.as_ref().map(|f| format!(" → {}", f.display())).unwrap_or_default();
        let state = match (&s.skipped, s.converted, dry_run) {
            (Some(why), ..) => format!("stays as it is: {why}"),
            (None, true, _) => format!("converted ({} linked, {} copied, {} already in place)", s.linked, s.copied, s.already),
            (None, false, true) if s.problems.is_empty() => "would be converted".to_string(),
            (None, false, _) => "not converted yet:".to_string(),
        };
        println!("{} ({kind}){folder}: {state}", s.name);
        for p in &s.problems {
            println!("  - {p}");
        }
        for n in &s.renamed {
            println!("  renamed: {n}");
        }
    }
    if !report.unknown.is_empty() {
        println!("In the storage folders but not known to ThirtyFile (left as they are):");
        for p in &report.unknown {
            println!("  {}", p.display());
        }
    }
    report.problems()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{auth::User, testutil};
    use axum::{
        body::Bytes,
        extract::{Path as UrlPath, Query, State},
        http::HeaderMap,
    };

    fn storages(env: &testutil::TestEnv) -> HashMap<String, Arc<dyn Storage>> {
        env.st.storages.read().unwrap().clone()
    }

    async fn convert(env: &testutil::TestEnv) -> Report {
        run(&env.st.db, &storages(env), &env.dir.join("blobs"), &env.dir, false, |_| {}).await.unwrap()
    }

    async fn read(env: &testutil::TestEnv, user: &User, id: &str) -> Vec<u8> {
        let q = Query(serde_json::from_value(serde_json::json!({})).unwrap());
        let res = crate::files::content(State(env.st.clone()), user.clone(), UrlPath(id.to_string()), q, HeaderMap::new()).await.unwrap();
        axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec()
    }

    async fn count(env: &testutil::TestEnv, sql: &str) -> i64 {
        let (n,): (i64,) = sqlx::query_as(sqlx::AssertSqlSafe(sql)).fetch_one(&env.st.db).await.unwrap();
        n
    }

    async fn mode(env: &testutil::TestEnv, user: &User) -> String {
        let (m,): (String,) = sqlx::query_as("SELECT mode FROM drives WHERE root_id = ?").bind(&user.root_id).fetch_one(&env.st.db).await.unwrap();
        m
    }

    #[cfg(unix)]
    fn inode(p: &Path) -> u64 {
        std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(p).unwrap())
    }

    #[tokio::test]
    async fn a_space_becomes_a_folder_and_keeps_its_ids_shares_trash_and_versions() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let docs = env.folder(&amy, &amy.root_id, "Docs").await;
        let report = env.stored_file(&amy, &docs, "report.txt", b"hello").await;
        // The same content twice: one copy in the store, two links on disk
        let copy = env.stored_file(&amy, &amy.root_id, "copy.txt", b"hello").await;
        env.grant(&docs, &ben, "viewer").await;
        let body = |b: &'static [u8]| Bytes::from_static(b);
        let _ = crate::files::save_content(State(env.st.clone()), amy.clone(), UrlPath(report.clone()), HeaderMap::new(), body(b"hello again")).await.unwrap();
        let old = env.stored_file(&amy, &amy.root_id, "old.txt", b"deleted").await;
        let req = serde_json::from_value(serde_json::json!({ "ids": [old] })).unwrap();
        let _ = crate::nodes::trash(State(env.st.clone()), amy.clone(), axum::Json(req)).await.unwrap();
        // Content another space still uses stays in the store
        env.stored_file(&ben, &ben.root_id, "ben.txt", b"hello").await;

        let dry = run(&env.st.db, &storages(&env), &env.dir.join("blobs"), &env.dir, true, |_| {}).await.unwrap();
        assert!(dry.spaces.iter().all(|s| !s.converted) && !env.dir.join("blobs/users").exists(), "a dry run writes nothing");

        let r = convert(&env).await;
        assert_eq!(r.problems(), 0, "{r:?}");
        assert_eq!(mode(&env, &amy).await, "folder");
        let dir = env.dir.join("blobs/users/amy");
        assert_eq!(std::fs::read(dir.join("Docs/report.txt")).unwrap(), b"hello again");
        assert_eq!(std::fs::read(dir.join("copy.txt")).unwrap(), b"hello");
        #[cfg(unix)]
        assert_eq!(inode(&dir.join("copy.txt")), inode(&env.dir.join("blobs/users/ben/ben.txt")));
        // Same ids: the file, its share with Ben, its version and the trash
        assert_eq!(read(&env, &amy, &report).await, b"hello again");
        assert_eq!(read(&env, &ben, &report).await, b"hello again");
        assert_eq!(read(&env, &amy, &copy).await, b"hello");
        let axum::Json(versions) = crate::versions::list(State(env.st.clone()), amy.clone(), UrlPath(report.clone())).await.unwrap();
        let versions = serde_json::to_value(&versions).unwrap();
        let version = versions[0]["id"].as_str().unwrap().to_string();
        assert_eq!(std::fs::read(dir.join(VERSIONS_DIR).join(&report).join(&version)).unwrap(), b"hello");
        let (trash_id,): (String,) = sqlx::query_as("SELECT trash_id FROM nodes WHERE id = ?").bind(&old).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(std::fs::read(dir.join(TRASH_DIR).join(&trash_id).join("old.txt")).unwrap(), b"deleted");
        let req = serde_json::from_value(serde_json::json!({ "ids": [old] })).unwrap();
        let _ = crate::nodes::restore(State(env.st.clone()), amy.clone(), axum::Json(req)).await.unwrap();
        assert!(dir.join("old.txt").is_file(), "restored from the trash folder");
        // The store lets go of content once no space uses it
        assert_eq!(count(&env, "SELECT COUNT(*) FROM blobs").await, 0, "every space was converted");
        assert_eq!(count(&env, "SELECT COUNT(*) FROM nodes WHERE blob_hash IS NOT NULL").await, 0);
        assert_eq!(count(&env, "SELECT COUNT(*) FROM convert_files").await, 0);
        assert!(storages(&env)["local"].list().await.unwrap().is_empty(), "the stored content is removed");
        // A scan finds the index as it should be
        let (drive,): (String,) = sqlx::query_as("SELECT id FROM drives WHERE root_id = ?").bind(&amy.root_id).fetch_one(&env.st.db).await.unwrap();
        let s = crate::folders::scan(&env.st, &drive).await.unwrap();
        assert_eq!((s.added, s.changed, s.moved, s.removed), (0, 0, 0, 0), "{s:?}");
    }

    #[tokio::test]
    async fn a_conversion_stopped_halfway_continues_and_skips_what_is_done() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let a = env.stored_file(&amy, &amy.root_id, "a.txt", b"first").await;
        let b = env.stored_file(&amy, &amy.root_id, "b.txt", b"second").await;
        let gone = env.stored_file(&amy, &amy.root_id, "gone.txt", b"third").await;
        // The content of one file can't be read: the others are written, the space stays as it is
        let (hash,): (String,) = sqlx::query_as("SELECT blob_hash FROM nodes WHERE id = ?").bind(&b).fetch_one(&env.st.db).await.unwrap();
        let stored = storages(&env)["local"].local_file(&hash).unwrap();
        let aside = env.dir.join("aside");
        std::fs::rename(&stored, &aside).unwrap();
        let r = convert(&env).await;
        let space = r.spaces.iter().find(|s| s.folder.as_ref().is_some_and(|f| f.ends_with("users/amy"))).unwrap();
        assert!(!space.converted && space.problems.iter().any(|p| p.contains("b.txt")), "{r:?}");
        assert_eq!(space.linked, 2);
        assert_eq!(mode(&env, &amy).await, "store");
        assert_eq!(read(&env, &amy, &a).await, b"first", "the space works as before meanwhile");

        // Meanwhile the server ran: a file was renamed and another deleted for good
        sqlx::query("UPDATE nodes SET name = 'renamed.txt' WHERE id = ?").bind(&a).execute(&env.st.db).await.unwrap();
        let mut c = env.st.db.acquire().await.unwrap();
        let orphans = crate::tree::purge_subtree(&mut c, &gone).await.unwrap();
        drop(c);
        for (h, _) in &orphans {
            storages(&env)["local"].delete(h).await.unwrap();
        }
        // (its emptied folder in the store was removed meanwhile)
        std::fs::create_dir_all(stored.parent().unwrap()).unwrap();
        std::fs::rename(&aside, &stored).unwrap();
        let r = convert(&env).await;
        let space = r.spaces.iter().find(|s| s.converted).unwrap();
        assert_eq!((space.linked, space.already), (2, 0), "{r:?}");
        let dir = env.dir.join("blobs/users/amy");
        let mut names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["b.txt", "renamed.txt"], "what an earlier run left for items changed since is gone");
        assert_eq!(read(&env, &amy, &a).await, b"first");
        assert_eq!(read(&env, &amy, &b).await, b"second");
    }

    #[tokio::test]
    async fn files_already_written_are_skipped_when_run_again() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        env.stored_file(&amy, &amy.root_id, "a.txt", b"one").await;
        let b = env.stored_file(&amy, &amy.root_id, "b.txt", b"two").await;
        sqlx::query("UPDATE nodes SET blob_hash = NULL, size = 3 WHERE id = ?").bind(&b).execute(&env.st.db).await.unwrap();
        let r = convert(&env).await;
        assert_eq!(r.spaces.iter().map(|s| s.linked).sum::<usize>(), 1, "{r:?}");
        let (hash,): (String,) = sqlx::query_as("SELECT hash FROM blobs WHERE hash != (SELECT blob_hash FROM nodes WHERE name = 'a.txt')").fetch_one(&env.st.db).await.unwrap();
        sqlx::query("UPDATE nodes SET blob_hash = ? WHERE id = ?").bind(&hash).bind(&b).execute(&env.st.db).await.unwrap();
        let r = convert(&env).await;
        let space = r.spaces.iter().find(|s| s.converted).unwrap();
        assert_eq!((space.already, space.linked), (1, 1), "{r:?}");
    }

    #[tokio::test]
    async fn on_another_disk_files_are_copied_and_checked() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let a = env.stored_file(&amy, &amy.root_id, "a.txt", b"one").await;
        let b = env.stored_file(&amy, &amy.root_id, "b.txt", b"two").await;
        // The stored content of one file changed on the disk: it no longer matches its fingerprint
        let (hash,): (String,) = sqlx::query_as("SELECT blob_hash FROM nodes WHERE id = ?").bind(&b).fetch_one(&env.st.db).await.unwrap();
        let stored = storages(&env)["local"].local_file(&hash).unwrap();
        std::fs::write(&stored, b"TWO").unwrap();
        OTHER_DISK.with(|d| d.set(true));
        let r = convert(&env).await;
        let space = r.spaces.iter().find(|s| s.folder.as_ref().is_some_and(|f| f.ends_with("users/amy"))).unwrap();
        assert!(!space.converted && space.problems.iter().any(|p| p.contains("b.txt") && p.contains("SHA-256")), "{r:?}");
        assert_eq!(space.copied, 1);
        std::fs::write(&stored, b"two").unwrap();
        let r = convert(&env).await;
        OTHER_DISK.with(|d| d.set(false));
        let space = r.spaces.iter().find(|s| s.converted).unwrap();
        assert_eq!((space.copied, space.already), (1, 1), "{r:?}");
        let dir = env.dir.join("blobs/users/amy");
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::MetadataExt::nlink(&std::fs::metadata(dir.join("a.txt")).unwrap()), 1, "a copy, not a link");
        assert_eq!(read(&env, &amy, &a).await, b"one");
        assert_eq!(read(&env, &amy, &b).await, b"two");
    }

    #[test]
    fn names_a_folder_cant_hold_get_another_one() {
        let item = |id: &str, name: &str, kind: &str| Item {
            id: id.into(),
            parent_id: Some("root".into()),
            kind: kind.into(),
            name: name.into(),
            blob_hash: None,
            size: 0,
            trash_id: None,
            trash_root: false,
        };
        let long = format!("{}.txt", "x".repeat(300));
        let items = vec![
            item("root", "", "folder"),
            item("1", "Notes.txt", "file"),
            item("2", "notes.txt", "file"),
            item("3", "Thumbs.db", "file"),
            item("4", "~$draft.docx", "file"),
            item("5", "movie.mp4.part", "file"),
            item("6", &long, "file"),
            item("7", "Plans", "folder"),
            item("8", "plans", "folder"),
        ];
        let mut r = SpaceReport::default();
        let p = plan("root", &items, &[], &mut r);
        let mut rels: Vec<&str> = p.files.iter().map(|w| w.rel.as_str()).chain(p.folders.iter().map(|(_, rel)| rel.as_str())).collect();
        rels.sort();
        let short = format!("{}.txt", "x".repeat(MAX_NAME_BYTES - 4 - 12));
        let mut want = vec!["", "Notes.txt", "notes (1).txt", "_Thumbs.db", "_~$draft.docx", "_movie.mp4.part_", short.as_str(), "Plans", "plans (1)"];
        want.sort();
        assert_eq!(rels, want);
        assert_eq!(r.renamed.len(), 6, "{:?}", r.renamed);
        assert!(r.problems.is_empty());
        for rel in rels {
            assert!(!ignored(rel), "{rel}");
        }
    }

    #[tokio::test]
    async fn spaces_on_other_storage_and_files_placed_by_hand_are_left_alone() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        env.stored_file(&amy, &amy.root_id, "a.txt", b"one").await;
        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, is_default, created_at) VALUES ('s3', 'Bucket', 's3', '{}', 0, 0)")
            .execute(&env.st.db)
            .await
            .unwrap();
        sqlx::query("UPDATE drives SET location_id = 's3' WHERE root_id = ?").bind(&amy.root_id).execute(&env.st.db).await.unwrap();
        std::fs::write(env.dir.join("blobs/by hand.txt"), b"mine").unwrap();
        let r = convert(&env).await;
        let space = r.spaces.iter().find(|s| s.kind == "personal" && s.skipped.is_some()).unwrap();
        assert!(space.skipped.as_deref().unwrap().contains("S3"));
        assert_eq!(mode(&env, &amy).await, "store");
        assert_eq!(r.unknown, [env.dir.join("blobs/by hand.txt")]);
        assert_eq!(std::fs::read(env.dir.join("blobs/by hand.txt")).unwrap(), b"mine");
        // Content of the store stays where it is
        assert_eq!(storages(&env)["local"].list().await.unwrap().len(), 1);
    }
}
