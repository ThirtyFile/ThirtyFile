//! Restores: a space of a complete snapshot, a folder of it, or some items of a folder, brought back into a space.
//!
//! - Into a new folder (the default): everything goes into a folder made for the restore, which nothing else is in, so
//!   nothing there is replaced.
//! - Into their original place, in the space itself: folders that are still there are used, missing ones are made, and
//!   a file whose name is taken is skipped, kept beside the one there (with a number), or replaces its content (which
//!   becomes an earlier version of it), as the administrator chose after seeing how many there are.
//!
//! Restored items take the permissions of where they go: the access the snapshot recorded isn't given again, so a
//! restore never widens who can see the files. Files that were in the trash are left out unless asked for; earlier
//! versions stay in the snapshot.
//!
//! Each file's content comes from the set (checked against its SHA-256), or, in a content store that holds that
//! content already, isn't copied at all. Everything restored is recorded (`backup_restored`), so a restore that stops
//! continues without making anything twice.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::{
    layout::{self, Line},
    runner::{Ctx, Stop},
};
use crate::{
    content,
    error::{AppError, AppResult},
    state::AppState,
    storage::Storage,
    tree::{self, Node},
    util::new_id,
};

/// What a restore brings back, and where
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Params {
    /// The space in the snapshot, and its kind there
    pub space: String,
    pub space_kind: String,
    /// A folder of it (its id in the snapshot): what it holds is restored, or `items` of it; None: the whole space
    #[serde(default)]
    pub folder: Option<String>,
    /// Items chosen in that folder (files and folders, by their id in the snapshot); None: all of it
    #[serde(default)]
    pub items: Option<Vec<String>>,
    /// The space restored into, and the folder there the new folder goes in (None: its top folder)
    pub target_drive: String,
    #[serde(default)]
    pub target_parent: Option<String>,
    /// 'new_folder' (the default) or 'original'
    #[serde(default = "new_folder")]
    pub mode: String,
    /// Into the original place, for a file whose name is taken: 'skip', 'keep' (both, the restored one numbered) or
    /// 'replace' (its content, which becomes an earlier version)
    #[serde(default = "keep")]
    pub on_conflict: String,
    /// The new folder's name
    pub folder_name: String,
    /// Items that were in the trash too
    #[serde(default)]
    pub trash: bool,
}

fn new_folder() -> String {
    "new_folder".into()
}

fn keep() -> String {
    "keep".into()
}

impl Params {
    fn original(&self) -> bool {
        self.mode == "original"
    }
}

/// The id recorded for the new folder the restore made
const TOP: &str = "";

/// What a restore brings back, from a first pass over the manifest
#[derive(Debug, Default)]
pub struct Plan {
    pub files: i64,
    pub bytes: i64,
    /// The chosen items (or the folder), by id, with their paths in the space
    roots: HashMap<String, String>,
    /// Folders on the way to them (for a restore into the original place)
    ancestors: HashSet<String>,
    /// Paths of the files, below the space's top folder (for a restore into the original place, the first
    /// `MAX_CHECKED` of them): which are taken there
    pub paths: Vec<String>,
}

/// Files whose place is checked before a restore into the original place
pub const MAX_CHECKED: usize = 20_000;

/// The chosen items, or the folder, or the space's top folder
fn chosen(p: &Params, root: &str) -> HashSet<String> {
    match (&p.items, &p.folder) {
        (Some(items), _) => items.iter().cloned().collect(),
        (None, Some(folder)) => HashSet::from([folder.clone()]),
        (None, None) => HashSet::from([root.to_string()]),
    }
}

/// A first pass over the manifest: what is restored
pub async fn plan(manifest: std::path::PathBuf, p: Params) -> AppResult<Plan> {
    tokio::task::spawn_blocking(move || -> AppResult<Plan> {
        let mut plan = Plan::default();
        let mut inside: HashSet<String> = HashSet::new();
        let mut folders: HashMap<String, String> = HashMap::new();
        let mut roots: Option<HashSet<String>> = None;
        for line in layout::lines(&manifest)? {
            match line? {
                Line::Folder { space, id, parent, path, trashed, .. } if space == p.space => {
                    let roots = roots.get_or_insert_with(|| chosen(&p, &id));
                    folders.insert(path.clone(), id.clone());
                    if trashed.is_some() && !p.trash {
                        continue;
                    }
                    if roots.contains(&id) {
                        plan.roots.insert(id.clone(), path);
                        inside.insert(id);
                    } else if parent.is_some_and(|pid| inside.contains(&pid)) {
                        inside.insert(id);
                    }
                }
                Line::File { space, id, parent, path, size, trashed, .. } if space == p.space => {
                    if trashed.is_some() && !p.trash {
                        continue;
                    }
                    let root = roots.as_ref().is_some_and(|r| r.contains(&id));
                    if root || inside.contains(&parent) {
                        if root {
                            plan.roots.insert(id, path.clone());
                        }
                        plan.files += 1;
                        plan.bytes += size;
                        if p.original() && plan.paths.len() < MAX_CHECKED {
                            plan.paths.push(path);
                        }
                    }
                }
                _ => {}
            }
        }
        // The folders on the way to each chosen item, from the top
        for path in plan.roots.values() {
            let parts: Vec<&str> = path.split('/').collect();
            for n in 0..parts.len() {
                if let Some(id) = folders.get(&parts[..n].join("/")) {
                    plan.ancestors.insert(id.clone());
                }
            }
        }
        Ok(plan)
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
}

/// How many of `paths` (below the top of the space `drive`) have an item there now: files a restore into the original
/// place would find in its way
pub async fn taken(st: &AppState, drive: &tree::Drive, paths: &[String]) -> AppResult<i64> {
    let mut c = st.db.acquire().await?;
    let mut folders: HashMap<String, Option<String>> = HashMap::from([(String::new(), Some(drive.root_id.clone()))]);
    let mut n = 0;
    for path in paths {
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        if !folders.contains_key(dir) {
            // Each folder on the way, found once
            let mut at = String::new();
            for part in dir.split('/') {
                let next = if at.is_empty() { part.to_string() } else { format!("{at}/{part}") };
                if !folders.contains_key(&next) {
                    let parent = folders.get(&at).cloned().flatten();
                    let found = match parent {
                        Some(parent) => tree::find_child(&mut c, &parent, part).await?.filter(Node::is_folder).map(|n| n.id),
                        None => None,
                    };
                    folders.insert(next.clone(), found);
                }
                at = next;
            }
        }
        if let Some(Some(parent)) = folders.get(dir)
            && tree::find_child(&mut c, parent, name).await?.is_some()
        {
            n += 1;
        }
    }
    Ok(n)
}

/// What became of a file
enum Outcome {
    Restored { renamed: bool },
    Replaced,
    Skipped,
}

pub async fn run(cx: &Ctx<'_>) -> AppResult<Stop> {
    let (st, job) = (cx.st, cx.job);
    let p: Params = serde_json::from_str(&job.params).map_err(|_| AppError::internal("a restore without its space"))?;
    let snapshot = job.snapshot_id.as_deref().ok_or_else(|| AppError::internal("a restore without its snapshot"))?;
    let set = super::load_set(&st.db, &job.set_id).await?;
    if set.removing {
        return Err(AppError::conflict("This copy is being deleted"));
    }
    let (state, sha, size): (String, Option<String>, Option<i64>) =
        sqlx::query_as("SELECT state, manifest_sha256, manifest_size FROM backup_snapshots WHERE id = ?")
            .bind(snapshot)
            .fetch_optional(&st.db)
            .await?
            .ok_or_else(|| AppError::not_found("This snapshot no longer exists"))?;
    let (Some(sha), Some(size)) = (sha, size) else { return Err(AppError::conflict("This snapshot isn't complete")) };
    if state != "complete" {
        return Err(AppError::conflict("This snapshot isn't complete"));
    }
    crate::locations::probe(st, &set.dest_location)
        .await
        .map_err(|e| AppError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, format!("The copy's location can't be reached: {e}")))?;
    let dst = st.storage(&set.dest_location)?;
    let manifest = layout::manifest(st, dst.as_ref(), &set.id, snapshot, &sha, size as u64).await?;
    let target = target_space(st, &p).await?;
    let plan = plan(manifest.clone(), p.clone()).await?;
    let restored: HashMap<String, String> = sqlx::query_as::<_, (String, String)>("SELECT source_id, node_id FROM backup_restored WHERE job_id = ?")
        .bind(&job.id)
        .fetch_all(&st.db)
        .await?
        .into_iter()
        .collect();
    cx.set_counts(0, 0, plan.files, plan.bytes);
    cx.flush().await?;
    // Into a new folder: made once (a restore that stopped finds it again)
    let top = if p.original() {
        target.root_id.clone()
    } else {
        match restored.get(TOP) {
            Some(id) => id.clone(),
            None => make_top(cx, &p, &target).await?,
        }
    };
    let mut places: HashMap<String, String> = restored;
    let mut inside: HashSet<String> = HashSet::new();
    let (mut renamed, mut skipped, mut replaced) = (0i64, 0i64, 0i64);
    let mut lines = read_lines(manifest);
    let mut roots: Option<HashSet<String>> = None;
    while let Some(line) = lines.recv().await {
        let line = line?;
        if let Some(stop) = cx.stop() {
            return Ok(stop);
        }
        match line {
            Line::Folder { space, id, parent, path, trashed, .. } if space == p.space => {
                let roots = roots.get_or_insert_with(|| chosen(&p, &id));
                let is_root = roots.contains(&id);
                let within = is_root || parent.as_ref().is_some_and(|pid| inside.contains(pid));
                let on_the_way = p.original() && plan.ancestors.contains(&id);
                if trashed.is_some() && !p.trash && !on_the_way {
                    continue;
                }
                if within {
                    inside.insert(id.clone());
                }
                if !within && !on_the_way {
                    continue;
                }
                if places.contains_key(&id) {
                    continue;
                }
                // Where it goes: the space's top folder into itself (original place); the folder chosen into the new
                // folder; chosen items into the new folder; anything else into where its folder went
                let into = if parent.is_none() || (!p.original() && is_root && p.items.is_none()) {
                    places.insert(id, top.clone());
                    continue;
                } else if !p.original() && is_root {
                    top.clone()
                } else {
                    match parent.and_then(|pid| places.get(&pid).cloned()) {
                        Some(into) => into,
                        None => continue,
                    }
                };
                let name = path.rsplit('/').next().unwrap_or_default().to_string();
                match make_folder(cx, &target, &into, &name, &id, p.original()).await {
                    Ok((node, again)) => {
                        renamed += again as i64;
                        places.insert(id, node);
                    }
                    Err(e) => cx.failed(&p.space, Some(path), e.message),
                }
            }
            Line::File { space, id, parent, path, hash, size, trashed, modified, .. } if space == p.space => {
                if trashed.is_some() && !p.trash || places.contains_key(&id) {
                    continue;
                }
                let is_root = roots.as_ref().is_some_and(|r| r.contains(&id));
                if !is_root && !inside.contains(&parent) {
                    continue;
                }
                let into = if !p.original() && is_root { Some(top.clone()) } else { places.get(&parent).cloned() };
                let Some(into) = into else { continue };
                let name = path.rsplit('/').next().unwrap_or_default().to_string();
                let conflict = if p.original() { p.on_conflict.as_str() } else { "keep" };
                match restore_file(cx, &set.id, dst.as_ref(), &target, &into, &name, &id, &hash, size, modified, conflict).await {
                    Ok(Ok(outcome)) => {
                        match outcome {
                            Outcome::Restored { renamed: again } => renamed += again as i64,
                            Outcome::Replaced => replaced += 1,
                            Outcome::Skipped => skipped += 1,
                        }
                        places.insert(id, String::new());
                        cx.done(1, size).await?;
                    }
                    Ok(Err(stop)) => return Ok(stop),
                    Err(e) if e.status == axum::http::StatusCode::INSUFFICIENT_STORAGE || e.status == axum::http::StatusCode::PAYLOAD_TOO_LARGE => {
                        return Err(e);
                    }
                    Err(e) if e.status.is_server_error() && e.status != axum::http::StatusCode::INTERNAL_SERVER_ERROR => return Err(e),
                    Err(e) => cx.failed(&p.space, Some(path), e.message),
                }
            }
            _ => {}
        }
    }
    if let Some(e) = cx.failures_error() {
        return Err(e);
    }
    let mut notes = Vec::new();
    if renamed > 0 {
        notes.push(if renamed == 1 {
            "1 item was given a new name: its name was taken in its folder, or needed changing there".to_string()
        } else {
            format!("{renamed} items were given new names: their names were taken in their folders, or needed changing there")
        });
    }
    if skipped > 0 {
        notes.push(if skipped == 1 { "1 file was skipped: its place was taken".to_string() } else { format!("{skipped} files were skipped: their places were taken") });
    }
    if replaced > 0 {
        notes.push(if replaced == 1 {
            "1 file was replaced; what it had is kept as an earlier version".to_string()
        } else {
            format!("{replaced} files were replaced; what they had is kept as earlier versions")
        });
    }
    notes.push("Earlier versions and permissions aren't restored".to_string());
    let note = notes.join("\n");
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        sqlx::query("DELETE FROM backup_restored WHERE job_id = ?").bind(&job.id).execute(&mut *tx).await?;
        super::runner::finish(&mut tx, cx, Some(&note)).await?;
        super::log(&mut tx, job, "backup_restored", &job.label).await?;
        AppResult::Ok(())
    }
    .await;
    crate::db::settle(tx, res).await?;
    Ok(Stop::Done)
}

/// The lines of a manifest, read on a thread of their own and handed over a few at a time
pub(super) fn read_lines(path: std::path::PathBuf) -> tokio::sync::mpsc::Receiver<AppResult<Line>> {
    let (tx, rx) = tokio::sync::mpsc::channel(256);
    tokio::task::spawn_blocking(move || {
        let lines = match layout::lines(&path) {
            Ok(l) => l,
            Err(e) => {
                let _ = tx.blocking_send(Err(e.into()));
                return;
            }
        };
        for line in lines {
            let failed = line.is_err();
            if tx.blocking_send(line.map_err(AppError::from)).is_err() || failed {
                return;
            }
        }
    });
    rx
}

/// The space restored into, as it is now
pub(super) async fn target_space(st: &AppState, p: &Params) -> AppResult<tree::Drive> {
    let drive = tree::get_drive(&mut *st.db.acquire().await?, &p.target_drive)
        .await?
        .ok_or_else(|| AppError::not_found("The space to restore into no longer exists"))?;
    if drive.disabled || drive.read_only || drive.moving {
        return Err(AppError::conflict("The space to restore into can't be changed now (it is disabled, read-only, or being moved)"));
    }
    Ok(drive)
}

/// The folder that is `parent` in the target (its id), as a node
async fn folder_node(st: &AppState, id: &str) -> AppResult<Node> {
    tree::get_node(&mut *st.db.acquire().await?, id)
        .await?
        .filter(|n| n.is_folder() && n.trashed_at.is_none())
        .ok_or_else(|| AppError::conflict("The folder being restored into was deleted"))
}

/// Who new items belong to: the owner of a personal space, else whoever asked for the restore
fn owner_of(cx: &Ctx<'_>, target: &tree::Drive) -> i64 {
    if target.kind == tree::SpaceKind::Personal { target.owner_id.unwrap_or_default() } else { cx.job.created_by.unwrap_or_default() }
}

/// Makes the new folder everything goes into, and records it
async fn make_top(cx: &Ctx<'_>, p: &Params, target: &tree::Drive) -> AppResult<String> {
    let parent = p.target_parent.clone().unwrap_or_else(|| target.root_id.clone());
    let (id, _) = make_folder(cx, target, &parent, &p.folder_name, TOP, false).await?;
    Ok(id)
}

/// Makes a folder `name` in the target's folder `parent` (with a number when the name is taken; `reuse`: a folder of
/// that name there is used instead), and records it as the restore of `source`; returns its id and whether it got
/// another name
async fn make_folder(cx: &Ctx<'_>, target: &tree::Drive, parent: &str, name: &str, source: &str, reuse: bool) -> AppResult<(String, bool)> {
    let st = cx.st;
    let name = crate::util::validate_name(name)?;
    let _space = if target.is_folder() { Some(crate::fsops::lock_space(st, &target.id).await) } else { None };
    let parent = folder_node(st, parent).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        let there = if reuse { tree::find_child(&mut tx, &parent.id, &name).await?.filter(Node::is_folder) } else { None };
        let (id, free) = match there {
            Some(folder) => (folder.id, name.clone()),
            None => {
                let free =
                    if target.is_folder() { crate::fsops::free_name(&mut tx, &parent, &name, true).await? } else { tree::unique_name(&mut tx, &parent.id, &name, true).await? };
                (crate::content::create_folder(&mut tx, owner_of(cx, target), &parent.id, &free).await?, free)
            }
        };
        sqlx::query("INSERT INTO backup_restored (job_id, source_id, node_id) VALUES (?, ?, ?)").bind(&cx.job.id).bind(source).bind(&id).execute(&mut *tx).await?;
        AppResult::Ok((id, free != name))
    }
    .await;
    crate::db::settle(tx, res).await
}

/// Brings one file back into the folder `parent`; Ok(Err(stop)) when asked to stop
#[allow(clippy::too_many_arguments)]
async fn restore_file(
    cx: &Ctx<'_>,
    set: &str,
    dst: &dyn Storage,
    target: &tree::Drive,
    parent: &str,
    name: &str,
    source: &str,
    hash: &str,
    size: i64,
    modified: i64,
    conflict: &str,
) -> AppResult<Result<Outcome, Stop>> {
    let st = cx.st;
    let name = crate::util::validate_name(name)?;
    // A manifest found on a location may have been changed: content is only ever named by a SHA-256
    crate::storage::valid_hash(hash).map_err(|_| AppError::bad_request("The backup names content that isn't valid"))?;
    // Skipped when its place is taken: nothing to read
    if conflict == "skip" {
        let mut c = st.db.acquire().await?;
        if let Some(existing) = tree::find_child(&mut c, parent, &name).await? {
            drop(c);
            let _w = st.write_lock.lock().await;
            sqlx::query("INSERT INTO backup_restored (job_id, source_id, node_id) VALUES (?, ?, ?)").bind(&cx.job.id).bind(source).bind(&existing.id).execute(&st.db).await?;
            return Ok(Ok(Outcome::Skipped));
        }
    }
    let tmp = st.tmp_dir().join(format!("backup-{}", new_id()));
    // A content store that holds this content already needs no copy of it
    let held: Option<(String,)> = if target.is_folder() {
        None
    } else {
        sqlx::query_as("SELECT location_id FROM blobs WHERE hash = ?").bind(hash).fetch_optional(&st.db).await?
    };
    if held.is_none() {
        match fetch(cx, set, dst, hash, size, &tmp).await? {
            Ok(()) => {}
            Err(stop) => return Ok(Err(stop)),
        }
    }
    let result = restore_into(cx, set, dst, target, parent, &name, source, hash, size, modified, &tmp, conflict).await;
    let _ = tokio::fs::remove_file(&tmp).await;
    result.map(Ok)
}

/// Reads a content of the set into a temp file, checked against its SHA-256; Err(stop) inside when asked to stop
async fn fetch(cx: &Ctx<'_>, set: &str, dst: &dyn Storage, hash: &str, size: i64, tmp: &std::path::Path) -> AppResult<Result<(), Stop>> {
    let key = layout::object_key(set, hash);
    let fetched = cx
        .tries(
            |e: &std::io::Error| e.kind() != std::io::ErrorKind::NotFound && !(crate::hashing::unusable_kind(e) == Some(crate::hashing::Unusable::Damaged)),
            || async {
                let _ = tokio::fs::remove_file(tmp).await;
                let mut reader = dst.open_at(&key, 0, size as u64).await?;
                super::capture::copy_checked(&mut reader, hash, size, tmp).await
            },
        )
        .await;
    match fetched {
        Ok(()) => Ok(Ok(())),
        Err(Ok(stop)) => Ok(Err(stop)),
        Err(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Err(AppError::conflict("The copy doesn't hold this file's content any more")),
        Err(Err(e)) if crate::hashing::unusable_kind(&e) == Some(crate::hashing::Unusable::Damaged) => Err(AppError::conflict("The copy of this file's content is damaged")),
        Err(Err(e)) => Err(AppError::new(axum::http::StatusCode::BAD_GATEWAY, format!("Couldn't read from the copy's location: {}", crate::locations::describe(&e)))),
    }
}

/// Into the space: through content.rs, like an upload. A content store that holds the content already needs no copy of
/// it; the file is recorded, or (told to replace) the file there gets it
#[allow(clippy::too_many_arguments)]
async fn restore_into(
    cx: &Ctx<'_>,
    set: &str,
    dst: &dyn Storage,
    target: &tree::Drive,
    parent: &str,
    name: &str,
    source: &str,
    hash: &str,
    size: i64,
    modified: i64,
    tmp: &std::path::Path,
    conflict: &str,
) -> AppResult<Outcome> {
    let st = cx.st;
    let folder = folder_node(st, parent).await?;
    let received = || content::Received { path: tmp.to_path_buf(), size: size as u64, hash: Some(hash.to_string()) };
    let staged = match content::stage(st, &folder, received()).await {
        Ok(s) => s,
        // Deleted meanwhile, and not fetched: fetch it and try again
        Err(_) if !target.is_folder() && tokio::fs::metadata(tmp).await.is_err() => {
            match fetch(cx, set, dst, hash, size, tmp).await? {
                Ok(()) => {}
                Err(_) => return Err(AppError::internal("stopped while fetching")),
            }
            content::stage(st, &folder, received()).await?
        }
        Err(e) => return Err(e),
    };
    let by = cx.job.created_by.unwrap_or_default();
    let mut turn = staged.turn(st).await;
    let _w = st.write_lock.lock().await;
    let result = async {
        turn.ready()?;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            let folder = tree::get_node(&mut tx, parent).await?.filter(|n| n.is_folder() && n.trashed_at.is_none()).ok_or_else(|| AppError::conflict("The folder being restored into was deleted"))?;
            staged.check(&folder)?;
            let existing = tree::find_child(&mut tx, &folder.id, name).await?.filter(|n| !n.is_folder());
            if let (Some(existing), "replace") = (&existing, conflict) {
                tree::check_quota(&mut tx, &target.id, size - existing.size).await?;
                let written = content::replace(&mut tx, st, &staged, existing, by).await?;
                sqlx::query("INSERT INTO backup_restored (job_id, source_id, node_id) VALUES (?, ?, ?)").bind(&cx.job.id).bind(source).bind(&existing.id).execute(&mut *tx).await?;
                return AppResult::Ok((written, Outcome::Replaced));
            }
            tree::check_quota(&mut tx, &target.id, size).await?;
            let free = content::free_name(&mut tx, &folder, name).await?;
            // A file of the content store keeps the date it had
            let (id, written) = content::create(&mut tx, &staged, owner_of(cx, target), &folder, &free, Some(modified)).await?;
            sqlx::query("INSERT INTO backup_restored (job_id, source_id, node_id) VALUES (?, ?, ?)").bind(&cx.job.id).bind(source).bind(&id).execute(&mut *tx).await?;
            AppResult::Ok((written, Outcome::Restored { renamed: free != name }))
        }
        .await;
        crate::db::settle(tx, res).await
    }
    .await;
    match result {
        Ok((written, outcome)) => {
            staged.finish(st, written).await;
            Ok(outcome)
        }
        Err(e) => {
            staged.abandon(st).await;
            Err(e)
        }
    }
}
