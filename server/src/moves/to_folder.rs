//! A content-store space into a folder of this server (the built-in storage, or a Local folder location): the files
//! are written into a new folder for the space, placed as a new space's would be (`company`, `teams/<name>`,
//! `users/<user>`), with the trash in `.thirtyfile-trash` and earlier versions in `.thirtyfile-versions`, as in every
//! folder space. The space is read-only meanwhile.
//!
//! 1. Plan: every item gets its path in the folder. Names a folder can't hold as they are (characters a file name
//!    can't have, names scans skip, names too long for the disk) get another, as do names only letter case would
//!    tell apart (so the folder also works on disks that ignore it). The plan is kept (`space_move_items`), so a move
//!    that stops continues with the same names.
//! 2. Copy: each content is written under a temporary name, checked against its SHA-256, given the file's date, and
//!    renamed into place.
//! 3. Switch, in one transaction: items point at their paths, versions at their files, the content store lets go of
//!    the content (deleted a minute later when nothing else uses it), and the space becomes a folder space.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
};

use sqlx::SqliteConnection;
use tokio::io::AsyncWriteExt;

use super::{
    Ctx, Job, Stop,
    between_folders::{Found, find_folder},
};
use crate::{
    beneath::Pinned,
    error::{AppError, AppResult},
    folders::ignored,
    fsops::{TRASH_DIR, child_rel, disk_error},
    space_folders::disk_name,
    state::AppState,
    tree::REMOVAL_GRACE,
    util::{new_id, now, numbered_name},
    versions::VERSIONS_DIR,
};

/// Items looked at per page
const PAGE: i64 = 100;
/// Plan-and-copy rounds before the switch
const ROUNDS: usize = 5;

fn not_mounted() -> AppError {
    AppError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, crate::storage::NOT_MOUNTED)
}

/// The space's new folder: chosen and made (with the space's marker) when the move first starts, then kept. It must be
/// there with the marker afterwards: a disk that isn't mounted is never written to.
pub async fn target(cx: &Ctx<'_>) -> AppResult<PathBuf> {
    let (st, job) = (cx.st, cx.job);
    let (chosen,): (Option<String>,) = sqlx::query_as("SELECT to_path FROM space_moves WHERE id = ?").bind(&job.id).fetch_one(&st.db).await?;
    if let Some(path) = chosen {
        let root = Pinned::root(Path::new(&path)).map_err(|_| not_mounted())?;
        return match crate::folders::space_marker(&root) {
            Ok(Some(id)) if id == job.drive_id => Ok(PathBuf::from(path)),
            _ => Err(not_mounted()),
        };
    }
    let builtin = st.space_folders.clone().ok_or_else(|| AppError::bad_request("Spaces can't be folders on this server"))?;
    let mut c = st.db.acquire().await?;
    let root = crate::space_folders::location_folder(&mut c, &builtin, &job.to_location)
        .await?
        .ok_or_else(|| AppError::bad_request("This location keeps files in a content store, not in folders"))?;
    let root = std::path::absolute(&root).unwrap_or(root);
    if crate::storage::marker_of(&root).await.ok().flatten().as_deref() != Some(job.to_location.as_str()) {
        return Err(not_mounted());
    }
    let (name, kind, owner): (String, crate::tree::SpaceKind, String) =
        sqlx::query_as("SELECT d.name, d.kind, COALESCE((SELECT username FROM users WHERE id = d.owner_id), '') FROM drives d WHERE d.id = ?")
            .bind(&job.drive_id)
            .fetch_optional(&mut *c)
            .await?
            .ok_or_else(|| AppError::not_found("Space not found"))?;
    let (parent, wanted) = crate::space_folders::place(&root, kind, &name, &owner, &job.drive_id);
    std::fs::create_dir_all(&parent).map_err(disk_error)?;
    let mut folder = None;
    for n in 1..10_000u32 {
        let candidate = parent.join(if n == 1 { wanted.clone() } else { format!("{wanted} ({n})") });
        let text = candidate.to_string_lossy().into_owned();
        let (taken,): (i64,) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT (SELECT COUNT(*) FROM drives WHERE source_path = ?1) + (SELECT COUNT(*) FROM space_moves WHERE to_path = ?1 AND state IN {})",
            super::ACTIVE
        )))
        .bind(&text)
        .fetch_one(&mut *c)
        .await?;
        if taken > 0 {
            continue;
        }
        // Made here, so two moves never take the same folder; one already on the disk is left alone
        match std::fs::create_dir(&candidate) {
            Ok(()) => {
                folder = Some(candidate);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(disk_error(e)),
        }
    }
    let folder = folder.ok_or_else(|| AppError::conflict(format!("There is no free folder name for the space in {}", parent.display())))?;
    crate::folders::mark_space(&folder, &job.drive_id).map_err(disk_error)?;
    let _w = st.write_lock.lock().await;
    sqlx::query("UPDATE space_moves SET to_path = ? WHERE id = ?").bind(folder.to_string_lossy()).bind(&job.id).execute(&st.db).await?;
    Ok(folder)
}

pub async fn run(cx: &Ctx<'_>) -> AppResult<Stop> {
    let (st, job) = (cx.st, cx.job);
    let folder = target(cx).await?;
    for _ in 0..ROUNDS {
        drop_gone(st, job).await?;
        plan(st, job).await?;
        let counts: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT COALESCE(SUM(done), 0), COALESCE(SUM(CASE WHEN done = 1 THEN size END), 0), COUNT(*), COALESCE(SUM(size), 0)
             FROM space_move_items WHERE move_id = ? AND kind != 'folder'",
        )
        .bind(&job.id)
        .fetch_one(&st.db)
        .await?;
        cx.set_counts(counts.0, counts.1, counts.2, counts.3);
        cx.flush().await?;
        let root = Pinned::root(&folder).map_err(|_| not_mounted())?;
        make_folders(st, job, &root).await?;
        if let Some(stop) = copy_left(cx, &root).await? {
            return Ok(stop);
        }
        let failed = cx.failed_count();
        if failed > 0 {
            return Err(AppError::new(
                axum::http::StatusCode::BAD_GATEWAY,
                if failed == 1 { "1 file couldn't be copied".to_string() } else { format!("{failed} files couldn't be copied") },
            ));
        }
        if let Some(stop) = cx.stop() {
            return Ok(stop);
        }
        if switch(cx, &folder).await? {
            return Ok(Stop::Done);
        }
    }
    Err(AppError::conflict("The space kept changing while it was being moved. Try again later."))
}

#[derive(sqlx::FromRow)]
struct Item {
    id: String,
    kind: String,
    name: String,
    blob_hash: Option<String>,
    size: i64,
}

/// A planned path, with the name on disk when it differs, and the content
struct Want {
    id: String,
    kind: &'static str,
    path: String,
    name: Option<String>,
    hash: Option<String>,
    size: i64,
}

/// Files and versions not copied yet that are gone meanwhile (the trash emptied, earlier versions no longer kept):
/// taken off the plan. `only`: just this one.
async fn drop_gone(st: &AppState, job: &Job) -> AppResult<()> {
    drop_gone_items(st, job, None).await
}

async fn drop_gone_items(st: &AppState, job: &Job, only: Option<&str>) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    sqlx::query(
        "DELETE FROM space_move_items WHERE move_id = ?1 AND done = 0 AND kind IN ('file', 'version') AND (?2 IS NULL OR item_id = ?2)
           AND NOT EXISTS (SELECT 1 FROM nodes n WHERE n.id = item_id) AND NOT EXISTS (SELECT 1 FROM node_versions v WHERE v.id = item_id)",
    )
    .bind(&job.id)
    .bind(only)
    .execute(&st.db)
    .await?;
    Ok(())
}

/// Plans a path in the folder for every item and version that has none yet (all of them the first time). It goes
/// through the space a folder at a time, so what it holds doesn't grow with the space: the folders still to go
/// through, and the items of one.
async fn plan(st: &AppState, job: &Job) -> AppResult<()> {
    let root_id: String = sqlx::query_as::<_, (String,)>("SELECT root_id FROM drives WHERE id = ?").bind(&job.drive_id).fetch_one(&st.db).await?.0;
    let mut out: Vec<Want> = Vec::new();
    if planned_paths(st, job, std::slice::from_ref(&root_id)).await?.is_empty() {
        out.push(Want { id: root_id.clone(), kind: "folder", path: String::new(), name: None, hash: None, size: 0 });
    }
    // The space's root folder, then the trash by its trash id, then the folders found in them
    let mut queue: VecDeque<(Option<String>, String)> = VecDeque::from([(Some(root_id.clone()), String::new())]);
    let trash_ids: Vec<(String,)> = sqlx::query_as("SELECT DISTINCT COALESCE(trash_id, id) FROM nodes WHERE drive_id = ? AND trash_root = 1 ORDER BY 1")
        .bind(&job.drive_id)
        .fetch_all(&st.db)
        .await?;
    queue.extend(trash_ids.into_iter().map(|(t,)| (None, format!("{TRASH_DIR}/{t}"))));
    while let Some((folder, dir)) = queue.pop_front() {
        let kids: Vec<Item> = match &folder {
            Some(id) => {
                sqlx::query_as("SELECT id, kind, name, blob_hash, size FROM nodes WHERE parent_id = ? AND drive_id = ? AND trash_root = 0 AND id != ?")
                    .bind(id)
                    .bind(&job.drive_id)
                    .bind(&root_id)
                    .fetch_all(&st.db)
                    .await?
            }
            // A group of the trash: the items deleted together
            None => {
                sqlx::query_as(
                    "SELECT id, kind, name, blob_hash, size FROM nodes
                 WHERE drive_id = ? AND trash_root = 1 AND COALESCE(trash_id, id) = ?",
                )
                .bind(&job.drive_id)
                .bind(dir.trim_start_matches(&format!("{TRASH_DIR}/")))
                .fetch_all(&st.db)
                .await?
            }
        };
        let ids: Vec<String> = kids.iter().map(|k| k.id.clone()).collect();
        let planned = planned_paths(st, job, &ids).await?;
        let folders = place_all(&dir, kids.iter().collect(), &planned, &mut out);
        queue.extend(folders.into_iter().map(|(id, path)| (Some(id), path)));
        if out.len() >= 500 {
            save_wants(st, job, std::mem::take(&mut out)).await?;
        }
    }
    save_wants(st, job, out).await?;
    // Earlier versions of the files planned, by their file
    loop {
        let _w = st.write_lock.lock().await;
        let added = sqlx::query(
            "INSERT OR IGNORE INTO space_move_items (move_id, item_id, kind, done, hash, size, path, name)
             SELECT ?1, v.id, 'version', 0, v.blob_hash, v.size, ?3 || '/' || v.node_id || '/' || v.id, NULL
             FROM node_versions v JOIN nodes n ON n.id = v.node_id
             WHERE n.drive_id = ?2 AND v.blob_hash IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM space_move_items i WHERE i.move_id = ?1 AND i.item_id = v.id)
               AND EXISTS (SELECT 1 FROM space_move_items i WHERE i.move_id = ?1 AND i.item_id = v.node_id)
             LIMIT 500",
        )
        .bind(&job.id)
        .bind(&job.drive_id)
        .bind(VERSIONS_DIR)
        .execute(&st.db)
        .await?
        .rows_affected();
        if added == 0 {
            return Ok(());
        }
    }
}

/// The paths planned already for these items (by id)
async fn planned_paths(st: &AppState, job: &Job, ids: &[String]) -> AppResult<HashMap<String, String>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(sqlx::query_as::<_, (String, String)>("SELECT item_id, path FROM space_move_items WHERE move_id = ? AND item_id IN (SELECT value FROM json_each(?))")
        .bind(&job.id)
        .bind(serde_json::to_string(ids).unwrap())
        .fetch_all(&st.db)
        .await?
        .into_iter()
        .collect())
}

/// Records planned paths, a batch per transaction
async fn save_wants(st: &AppState, job: &Job, wants: Vec<Want>) -> AppResult<()> {
    for chunk in wants.chunks(500) {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            for w in chunk {
                sqlx::query("INSERT OR IGNORE INTO space_move_items (move_id, item_id, kind, done, hash, size, path, name) VALUES (?, ?, ?, 0, ?, ?, ?, ?)")
                    .bind(&job.id)
                    .bind(&w.id)
                    .bind(w.kind)
                    .bind(&w.hash)
                    .bind(w.size)
                    .bind(&w.path)
                    .bind(&w.name)
                    .execute(&mut *tx)
                    .await?;
            }
            AppResult::Ok(())
        }
        .await;
        crate::db::settle(tx, res).await?;
    }
    Ok(())
}

/// Names the items of one folder on disk, next to what is planned there already: unique regardless of letter case,
/// and something a scan shows
fn place_all(dir: &str, mut kids: Vec<&Item>, planned: &HashMap<String, String>, out: &mut Vec<Want>) -> Vec<(String, String)> {
    let mut folders = Vec::new();
    kids.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    let name_of = |path: &str| path.rsplit('/').next().unwrap_or(path).to_lowercase();
    let mut taken: HashSet<String> = kids.iter().filter_map(|k| planned.get(&k.id)).map(|p| name_of(p)).collect();
    for it in kids {
        let is_dir = it.kind == "folder";
        let path = match planned.get(&it.id) {
            Some(p) => p.clone(),
            None => {
                let (base, _) = disk_name(&it.name, is_dir);
                let mut name = base.clone();
                let mut n = 0;
                while taken.contains(&name.to_lowercase()) || ignored(&name) {
                    n += 1;
                    name = numbered_name(&base, n, is_dir);
                }
                taken.insert(name.to_lowercase());
                let path = child_rel(dir, &name);
                out.push(Want {
                    id: it.id.clone(),
                    kind: if is_dir { "folder" } else { "file" },
                    path: path.clone(),
                    name: (name != it.name).then_some(name),
                    hash: it.blob_hash.clone(),
                    size: it.size,
                });
                path
            }
        };
        if is_dir {
            folders.push((it.id.clone(), path));
        }
    }
    folders
}

/// Makes a folder of the plan (and those on the way), reached without following a link
fn make_dirs(root: &Pinned, rel: &str) -> std::io::Result<Pinned> {
    let mut at = root.clone();
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        at = at.join(part)?;
        crate::fsops::ensure_dir(&at)?;
        at = at.dir()?;
    }
    Ok(at)
}

/// A copy as the file system has it: (device, inode, modification time)
type Stat = (i64, i64, i64);

/// What the file system says a copy is: identity and modification time
fn identity(p: &Path) -> std::io::Result<Stat> {
    let meta = std::fs::symlink_metadata(p)?;
    let (dev, ino) = crate::folders::identity(&meta);
    Ok((dev, ino, crate::folders::mtime_ns(&meta)))
}

/// Makes the planned folders
async fn make_folders(st: &AppState, job: &Job, root: &Pinned) -> AppResult<()> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT item_id, path FROM space_move_items WHERE move_id = ? AND kind = 'folder' AND done = 0 ORDER BY length(path)")
            .bind(&job.id)
            .fetch_all(&st.db)
            .await?;
    if rows.is_empty() {
        return Ok(());
    }
    let root = root.clone();
    let made = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<(String, Stat)>> {
        let mut out = Vec::new();
        for (id, rel) in rows {
            let dir = make_dirs(&root, &rel)?;
            out.push((id, identity(dir.as_path())?));
        }
        Ok(out)
    })
    .await
    .map_err(AppError::internal)?
    .map_err(disk_error)?;
    for chunk in made.chunks(500) {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            for (id, (dev, ino, mtime)) in chunk {
                sqlx::query("UPDATE space_move_items SET done = 1, dst_dev = ?, dst_ino = ?, dst_mtime_ns = ? WHERE move_id = ? AND item_id = ?")
                    .bind(dev)
                    .bind(ino)
                    .bind(mtime)
                    .bind(&job.id)
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            }
            AppResult::Ok(())
        }
        .await;
        crate::db::settle(tx, res).await?;
    }
    Ok(())
}

/// Copies the files and versions not copied yet
async fn copy_left(cx: &Ctx<'_>, root: &Pinned) -> AppResult<Option<Stop>> {
    let (st, job) = (cx.st, cx.job);
    let mut last = String::new();
    loop {
        let rows: Vec<(String, String, Option<String>, i64, String)> = sqlx::query_as(
            "SELECT i.item_id, i.path, i.hash, i.size, COALESCE((SELECT name FROM nodes WHERE id = i.item_id), '')
             FROM space_move_items i WHERE i.move_id = ? AND i.kind IN ('file', 'version') AND i.done = 0 AND i.item_id > ?
             ORDER BY i.item_id LIMIT ?",
        )
        .bind(&job.id)
        .bind(&last)
        .bind(PAGE)
        .fetch_all(&st.db)
        .await?;
        let Some((id, ..)) = rows.last() else { return Ok(None) };
        last = id.clone();
        for (id, rel, hash, size, name) in rows {
            if let Some(stop) = cx.stop() {
                return Ok(Some(stop));
            }
            if let Some(stop) = copy_one(cx, root, &id, &rel, hash.as_deref(), size, &name).await? {
                return Ok(Some(stop));
            }
        }
    }
}

/// Writes one content to its path and records the copy
async fn copy_one(cx: &Ctx<'_>, root: &Pinned, id: &str, rel: &str, hash: Option<&str>, size: i64, name: &str) -> AppResult<Option<Stop>> {
    let (st, job) = (cx.st, cx.job);
    // The file's date: when it last got content (a version: when it got the content it keeps)
    let (modified,): (i64,) =
        sqlx::query_as("SELECT COALESCE((SELECT updated_at FROM nodes WHERE id = ?1), (SELECT modified_at FROM node_versions WHERE id = ?1), 0)")
            .bind(id)
            .fetch_one(&st.db)
            .await?;
    let (dir, file) = rel.rsplit_once('/').unwrap_or(("", rel));
    let dest = {
        let (root, dir) = (root.clone(), dir.to_string());
        tokio::task::spawn_blocking(move || make_dirs(&root, &dir)).await.map_err(AppError::internal)?.map_err(disk_error)?
    };
    let location = match hash {
        Some(h) => sqlx::query_as::<_, (String,)>("SELECT location_id FROM blobs WHERE hash = ?").bind(h).fetch_optional(&st.db).await?.map(|r| r.0),
        None => None,
    };
    let written =
        cx.tries(|e: &std::io::Error| e.kind() != std::io::ErrorKind::NotFound, || write_one(st, &dest, file, hash, location.as_deref(), size, modified)).await;
    let seen = match written {
        Ok(seen) => seen,
        Err(Ok(stop)) => return Ok(Some(stop)),
        Err(Err(e)) if e.kind() == std::io::ErrorKind::NotFound || crate::hashing::unusable_kind(&e) == Some(crate::hashing::Unusable::Damaged) => {
            // Gone meanwhile (deleted for good, say): nothing to copy for it any more
            drop_gone_items(st, job, Some(id)).await?;
            let (planned,): (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM space_move_items WHERE move_id = ? AND item_id = ?").bind(&job.id).bind(id).fetch_one(&st.db).await?;
            if planned == 1 {
                cx.failed(&cx.job.drive_id, Some(if name.is_empty() { rel.to_string() } else { name.to_string() }), e.to_string());
            }
            return Ok(None);
        }
        Err(Err(e)) => return Err(AppError::new(axum::http::StatusCode::BAD_GATEWAY, format!("Failed to move files: {}", crate::locations::describe(&e)))),
    };
    {
        let _w = st.write_lock.lock().await;
        sqlx::query("UPDATE space_move_items SET done = 1, dst_dev = ?, dst_ino = ?, dst_mtime_ns = ? WHERE move_id = ? AND item_id = ?")
            .bind(seen.0)
            .bind(seen.1)
            .bind(seen.2)
            .bind(&job.id)
            .bind(id)
            .execute(&st.db)
            .await?;
    }
    cx.done(1, size).await?;
    Ok(None)
}

const VERIFY_FAILED: &str = "Verification of the copied content failed";

/// Writes a content into the folder `dir` as `name`: under a temporary name first, checked, dated, then renamed into
/// place. Returns the copy's identity and modification time.
async fn write_one(st: &AppState, dir: &Pinned, name: &str, hash: Option<&str>, location: Option<&str>, size: i64, modified: i64) -> std::io::Result<Stat> {
    let tmp = dir.join(&format!("{}{}", crate::fsops::COPY_PREFIX, new_id()))?;
    let written = async {
        let mut out = tokio::fs::OpenOptions::new().write(true).create_new(true).open(tmp.as_path()).await?;
        if let Some(hash) = hash {
            let location = location.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "the content isn't recorded"))?;
            let storage = st.storage(location).map_err(|e| std::io::Error::other(e.message))?;
            let mut reader = storage.open(hash, 0, size as u64).await?;
            let (got, len) = crate::hashing::copy_async(&mut reader, &mut out).await?;
            if got != hash || len != size as u64 {
                return Err(crate::hashing::unusable(crate::hashing::Unusable::Damaged, VERIFY_FAILED));
            }
        }
        out.flush().await?;
        let out = out.into_std().await;
        let (tmp, to) = (tmp.clone(), dir.join(name)?);
        // Waiting for the disk blocks: not on the thread that serves requests
        tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            FINISHED_ON.lock().unwrap().push(std::thread::current().id());
            out.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(modified.max(0) as u64))?;
            out.sync_all()?;
            drop(out);
            // Left by an earlier try that stopped after renaming: replaced
            match std::fs::symlink_metadata(to.as_path()) {
                Ok(m) if m.is_file() => std::fs::remove_file(to.as_path())?,
                _ => {}
            }
            crate::fsops::rename_new(tmp.as_path(), to.as_path())?;
            identity(to.as_path())
        })
        .await
        .map_err(std::io::Error::other)?
    }
    .await;
    if written.is_err() {
        let _ = tokio::fs::remove_file(tmp.as_path()).await;
    }
    written
}

/// Tests: the threads that finished writing a file into the folder (dated, synced and renamed it)
#[cfg(test)]
pub static FINISHED_ON: std::sync::Mutex<Vec<std::thread::ThreadId>> = std::sync::Mutex::new(Vec::new());

/// Switches the space to its folder in one transaction. False when an item isn't planned or copied yet.
async fn switch(cx: &Ctx<'_>, folder: &Path) -> AppResult<bool> {
    let (st, job) = (cx.st, cx.job);
    let (gone, switched) = {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            if !all_copied(&mut tx, job).await? {
                return Ok((Vec::new(), false));
            }
            // Copies whose item went meanwhile (a version no longer kept, say, or an item moved to another space):
            // removed from the folder afterwards. Everything below joins on the item and the space, so an item that
            // left the space keeps what it has where it is now.
            let gone: Vec<(String,)> = sqlx::query_as(
                "SELECT path FROM space_move_items i WHERE i.move_id = ?1 AND i.kind IN ('file', 'version', 'folder') AND i.path != ''
                   AND NOT EXISTS (SELECT 1 FROM nodes n WHERE n.id = i.item_id AND n.drive_id = ?2)
                   AND NOT EXISTS (SELECT 1 FROM node_versions v JOIN nodes n ON n.id = v.node_id WHERE v.id = i.item_id AND n.drive_id = ?2)
                 ORDER BY i.kind = 'folder', length(i.path) DESC",
            )
            .bind(&job.id)
            .bind(&job.drive_id)
            .fetch_all(&mut *tx)
            .await?;
            let renamed = use_folder(&mut tx, job, folder).await?;
            let note = match renamed {
                0 => None,
                1 => Some("1 item got another name in the folder: a folder can't hold its name as it was".to_string()),
                n => Some(format!("{n} items got another name in the folder: a folder can't hold their names as they were")),
            };
            super::finish(&mut tx, cx, false, note.as_deref()).await?;
            Ok((gone, true))
        }
        .await;
        crate::db::settle(tx, res).await?
    };
    if switched {
        crate::fsops::remove_below_later(gone.into_iter().map(|(p,)| crate::folders::Below::new(folder, &job.drive_id, p)).collect());
        crate::folders::spaces_changed(st);
        crate::folders::scan_later(st, &job.drive_id);
    }
    Ok(switched)
}

/// In the switch's transaction: whether every item and version of the space is planned and copied
async fn all_copied(tx: &mut SqliteConnection, job: &Job) -> AppResult<bool> {
    let pending: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM nodes n LEFT JOIN space_move_items i ON i.move_id = ?1 AND i.item_id = n.id
         WHERE n.drive_id = ?2 AND (i.item_id IS NULL OR i.done = 0)
         UNION ALL
         SELECT 1 FROM node_versions v JOIN nodes n ON n.id = v.node_id LEFT JOIN space_move_items i ON i.move_id = ?1 AND i.item_id = v.id
         WHERE n.drive_id = ?2 AND v.blob_hash IS NOT NULL AND (i.item_id IS NULL OR i.done = 0)
         LIMIT 1",
    )
    .bind(&job.id)
    .bind(&job.drive_id)
    .fetch_optional(&mut *tx)
    .await?;
    Ok(pending.is_none())
}

/// In the switch's transaction: the space's files and versions are the copies in `folder`, under the names they got
/// there, and the content store lets go of what they had. Returns how many items got another name.
async fn use_folder(tx: &mut SqliteConnection, job: &Job, folder: &Path) -> AppResult<usize> {
    // The content store lets go of the content of the space's files and versions: counted in the database (a list of
    // it all would be as large as the space), before the files forget it below
    sqlx::query(
        "UPDATE blobs SET refcount = refcount - d.n FROM (
           SELECT h, COUNT(*) AS n FROM (
             SELECT blob_hash AS h FROM nodes WHERE drive_id = ?1 AND blob_hash IS NOT NULL
             UNION ALL
             SELECT v.blob_hash FROM node_versions v JOIN nodes x ON x.id = v.node_id WHERE x.drive_id = ?1 AND v.blob_hash IS NOT NULL
           ) GROUP BY h
         ) d WHERE blobs.hash = d.h",
    )
    .bind(&job.drive_id)
    .execute(&mut *tx)
    .await?;
    let renamed: Vec<(String, String)> = sqlx::query_as(
        "SELECT i.item_id, i.name FROM space_move_items i JOIN nodes n ON n.id = i.item_id AND n.drive_id = ?2 WHERE i.move_id = ?1 AND i.name IS NOT NULL",
    )
    .bind(&job.id)
    .bind(&job.drive_id)
    .fetch_all(&mut *tx)
    .await?;
    for (id, _) in &renamed {
        sqlx::query("UPDATE nodes SET name = char(1) || id WHERE id = ?").bind(id).execute(&mut *tx).await?;
    }
    sqlx::query(
        "UPDATE nodes SET fs_path = i.path, fs_dev = i.dst_dev, fs_ino = i.dst_ino, fs_mtime_ns = i.dst_mtime_ns, fs_birth_ns = NULL,
                          fs_size = CASE WHEN nodes.kind = 'file' THEN i.size ELSE 0 END, blob_hash = NULL
         FROM space_move_items i WHERE i.move_id = ?1 AND i.item_id = nodes.id AND nodes.drive_id = ?2 AND i.kind IN ('file', 'folder')",
    )
    .bind(&job.id)
    .bind(&job.drive_id)
    .execute(&mut *tx)
    .await?;
    for (id, name) in &renamed {
        sqlx::query("UPDATE nodes SET name = ? WHERE id = ?").bind(name).bind(id).execute(&mut *tx).await?;
    }
    sqlx::query(
        "UPDATE node_versions SET drive_id = ?2, fs_path = i.path, blob_hash = NULL
         FROM space_move_items i WHERE i.move_id = ?1 AND i.kind = 'version' AND node_versions.id = i.item_id
           AND node_versions.node_id IN (SELECT id FROM nodes WHERE drive_id = ?2)",
    )
    .bind(&job.id)
    .bind(&job.drive_id)
    .execute(&mut *tx)
    .await?;
    // The content store lets go of the content: what nothing else uses is deleted a minute later
    let released: Vec<(String, String)> = sqlx::query_as(
        "DELETE FROM blobs WHERE refcount <= 0 AND NOT EXISTS (SELECT 1 FROM nodes WHERE blob_hash = blobs.hash)
           AND NOT EXISTS (SELECT 1 FROM node_versions WHERE blob_hash = blobs.hash)
         RETURNING hash, location_id",
    )
    .fetch_all(&mut *tx)
    .await?;
    for (hash, location) in released {
        sqlx::query(
            "INSERT INTO pending_blob_deletes (hash, location_id, created_at, attempts, last_error) VALUES (?, ?, ?, 0, 'deferred')
             ON CONFLICT (hash, location_id) DO UPDATE SET created_at = MIN(created_at, excluded.created_at)",
        )
        .bind(&hash)
        .bind(&location)
        .bind(now() + REMOVAL_GRACE)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("UPDATE drives SET mode = 'folder', location_id = ?2, source_path = ?3, last_scan_at = NULL, scan_report = NULL WHERE id = ?1")
        .bind(&job.drive_id)
        .bind(&job.to_location)
        .bind(folder.to_string_lossy())
        .execute(&mut *tx)
        .await?;
    Ok(renamed.len())
}

/// A cancelled move into a folder: the folder it made goes, with what was copied into it. Only a folder holding the
/// space's marker goes, and never one a space uses (or one inside it, or holding it), so nothing else is ever removed.
/// A folder renamed to its new place by a move that stopped before the switch was recorded is renamed back instead.
pub async fn remove_copies(st: &AppState, job: &Job) -> AppResult<()> {
    let (chosen, renamed): (Option<String>, bool) = sqlx::query_as("SELECT to_path, renamed FROM space_moves WHERE id = ?").bind(&job.id).fetch_one(&st.db).await?;
    if let Some(path) = chosen {
        let folder = PathBuf::from(&path);
        let from = PathBuf::from(job.from_path.clone().unwrap_or_default());
        if renamed && job.from_path.is_some() && find_folder(&job.drive_id, &from, &folder).await? == Found::New {
            let (f, t) = (from.clone(), folder.clone());
            match tokio::task::spawn_blocking(move || std::fs::rename(&t, &f)).await.map_err(AppError::internal)? {
                Ok(()) => {
                    let _w = st.write_lock.lock().await;
                    sqlx::query("UPDATE space_moves SET renamed = 0 WHERE id = ?").bind(&job.id).execute(&st.db).await?;
                }
                Err(e) => tracing::error!("Couldn't put {} back after a cancelled move: {e}", from.display()),
            }
            return Ok(());
        }
        let used: Vec<(String,)> = sqlx::query_as("SELECT source_path FROM drives WHERE source_path IS NOT NULL").fetch_all(&st.db).await?;
        let in_use = used.iter().any(|(p,)| {
            let p = Path::new(p);
            p.starts_with(&folder) || folder.starts_with(p)
        });
        if in_use {
            tracing::error!("{path} is a space's folder: it is kept, although the move that made it was cancelled");
        } else {
            let drive = job.drive_id.clone();
            tokio::task::spawn_blocking(move || {
                let ours = Pinned::root(&folder).ok().and_then(|r| crate::folders::space_marker(&r).ok().flatten()).is_some_and(|id| id == drive);
                if ours && let Err(e) = std::fs::remove_dir_all(&folder) {
                    tracing::warn!("Couldn't remove {path} after a cancelled move: {e}");
                }
            })
            .await
            .map_err(AppError::internal)?;
        }
    }
    let _w = st.write_lock.lock().await;
    sqlx::query("DELETE FROM space_move_items WHERE move_id = ?").bind(&job.id).execute(&st.db).await?;
    Ok(())
}
