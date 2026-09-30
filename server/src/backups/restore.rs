//! Restores: a space of a complete snapshot (or a folder of it) is brought back into a new folder of a space, which
//! nothing else is in, so nothing there is replaced. The folder takes the target space's permissions; the permissions
//! the snapshot recorded aren't given again, so a restore never widens who can see the files. Files that were in the
//! trash are left out unless asked for; earlier versions stay in the snapshot.
//!
//! Each file's content comes from the set (checked against its SHA-256), or, in a content store that holds that
//! content already, isn't copied at all. Everything restored is recorded (`backup_restored`), so a restore that stops
//! continues without making anything twice.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{
    layout::{self, Line},
    runner::{Ctx, Stop},
};
use crate::{
    error::{AppError, AppResult},
    state::AppState,
    storage::Storage,
    tree::{self, Node},
    util::{new_id, now},
};

/// What a restore brings back, and where
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Params {
    /// The space in the snapshot, and its kind there
    pub space: String,
    pub space_kind: String,
    /// A folder of it (its id in the snapshot); None: the whole space
    #[serde(default)]
    pub folder: Option<String>,
    /// The space restored into, and the folder there the new folder goes in (None: its top folder)
    pub target_drive: String,
    #[serde(default)]
    pub target_parent: Option<String>,
    /// The new folder's name
    pub folder_name: String,
    /// Items that were in the trash too
    #[serde(default)]
    pub trash: bool,
}

/// The id recorded for the new folder the restore made
const TOP: &str = "";

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
    let (Some(sha), Some(size)) = (sha, size).clone() else { return Err(AppError::conflict("This snapshot isn't complete")) };
    if state != "complete" {
        return Err(AppError::conflict("This snapshot isn't complete"));
    }
    crate::locations::probe(st, &set.dest_location)
        .await
        .map_err(|e| AppError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, format!("The copy's location can't be reached: {e}")))?;
    let dst = st.storage(&set.dest_location)?;
    let manifest = layout::manifest(st, dst.as_ref(), &set.id, snapshot, &sha, size as u64).await?;
    let target = target_space(st, &p).await?;
    // What there is to restore, and what was restored before a stop
    let (files, bytes) = count(manifest.clone(), p.clone()).await?;
    let restored: HashMap<String, String> = sqlx::query_as::<_, (String, String)>("SELECT source_id, node_id FROM backup_restored WHERE job_id = ?")
        .bind(&job.id)
        .fetch_all(&st.db)
        .await?
        .into_iter()
        .collect();
    cx.set_counts(0, 0, files, bytes);
    cx.flush().await?;
    let top = match restored.get(TOP) {
        Some(id) => id.clone(),
        None => make_top(cx, &p, &target).await?,
    };
    let mut places: HashMap<String, String> = restored;
    let mut renamed = 0i64;
    let mut lines = read_lines(manifest);
    // The source folder whose content goes into the new folder: the space's top folder, or the folder chosen
    let mut start: Option<String> = p.folder.clone();
    while let Some(line) = lines.recv().await {
        let line = line?;
        if let Some(stop) = cx.stop() {
            return Ok(stop);
        }
        match line {
            Line::Folder { space, id, parent, path, trashed, .. } if space == p.space => {
                if start.is_none() && parent.is_none() {
                    start = Some(id.clone());
                }
                if Some(&id) == start.as_ref() {
                    places.insert(id, top.clone());
                    continue;
                }
                if trashed.is_some() && !p.trash || places.contains_key(&id) {
                    continue;
                }
                let Some(into) = parent.and_then(|pid| places.get(&pid).cloned()) else { continue };
                let name = path.rsplit('/').next().unwrap_or_default().to_string();
                match make_folder(cx, &target, &into, &name, &id).await {
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
                let Some(into) = places.get(&parent).cloned() else { continue };
                let name = path.rsplit('/').next().unwrap_or_default().to_string();
                match restore_file(cx, &set.id, dst.as_ref(), &target, &into, &name, &id, &hash, size, modified).await {
                    Ok(Ok(again)) => {
                        renamed += again as i64;
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
fn read_lines(path: std::path::PathBuf) -> tokio::sync::mpsc::Receiver<AppResult<Line>> {
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

/// Files the restore brings back, and their bytes (a first pass over the manifest)
async fn count(manifest: std::path::PathBuf, p: Params) -> AppResult<(i64, i64)> {
    tokio::task::spawn_blocking(move || -> AppResult<(i64, i64)> {
        let mut inside: std::collections::HashSet<String> = std::collections::HashSet::new();
        let (mut files, mut bytes) = (0i64, 0i64);
        let mut start = p.folder.clone();
        for line in layout::lines(&manifest)? {
            match line? {
                Line::Folder { space, id, parent, trashed, .. } if space == p.space => {
                    if start.is_none() && parent.is_none() {
                        start = Some(id.clone());
                    }
                    let within = Some(&id) == start.as_ref() || parent.is_some_and(|pid| inside.contains(&pid));
                    if within && (trashed.is_none() || p.trash) {
                        inside.insert(id);
                    }
                }
                Line::File { space, parent, size, trashed, .. } if space == p.space && inside.contains(&parent) && (trashed.is_none() || p.trash) => {
                    files += 1;
                    bytes += size;
                }
                _ => {}
            }
        }
        Ok((files, bytes))
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
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
    if target.kind == "personal" { target.owner_id.unwrap_or_default() } else { cx.job.created_by.unwrap_or_default() }
}

/// Makes the new folder everything goes into, and records it
async fn make_top(cx: &Ctx<'_>, p: &Params, target: &tree::Drive) -> AppResult<String> {
    let parent = p.target_parent.clone().unwrap_or_else(|| target.root_id.clone());
    let (id, _) = make_folder(cx, target, &parent, &p.folder_name, TOP).await?;
    Ok(id)
}

/// Makes a folder `name` (with a number when the name is taken) in the target's folder `parent`, and records it as
/// the restore of `source`; returns its id and whether it got another name
async fn make_folder(cx: &Ctx<'_>, target: &tree::Drive, parent: &str, name: &str, source: &str) -> AppResult<(String, bool)> {
    let st = cx.st;
    let name = crate::util::validate_name(name)?;
    let _space = if target.is_folder() { Some(crate::fsops::lock_space(&target.id).await) } else { None };
    let parent = folder_node(st, parent).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        let free = if target.is_folder() { crate::fsops::free_name(&mut tx, &parent, &name, true).await? } else { tree::unique_name(&mut tx, &parent.id, &name, true).await? };
        let id = tree::create_folder(&mut tx, owner_of(cx, target), &parent.id, &free).await?;
        sqlx::query("INSERT INTO backup_restored (job_id, source_id, node_id) VALUES (?, ?, ?)").bind(&cx.job.id).bind(source).bind(&id).execute(&mut *tx).await?;
        AppResult::Ok((id, free != name))
    }
    .await;
    crate::db::settle(tx, res).await
}

/// Brings one file back into the folder `parent`; Ok(Ok(renamed)) once restored, Ok(Err(stop)) when asked to stop
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
) -> AppResult<Result<bool, Stop>> {
    let st = cx.st;
    let name = crate::util::validate_name(name)?;
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
    let result = if target.is_folder() { into_folder(cx, target, parent, &name, source, &tmp, size).await } else { into_store(cx, set, dst, target, parent, &name, source, hash, size, modified, &tmp).await };
    let _ = tokio::fs::remove_file(&tmp).await;
    result.map(Ok)
}

/// Reads a content of the set into a temp file, checked against its SHA-256; Err(stop) inside when asked to stop
async fn fetch(cx: &Ctx<'_>, set: &str, dst: &dyn Storage, hash: &str, size: i64, tmp: &std::path::Path) -> AppResult<Result<(), Stop>> {
    let key = layout::object_key(set, hash);
    let fetched = cx
        .tries(
            |e: &std::io::Error| e.kind() != std::io::ErrorKind::NotFound && e.to_string() != super::capture::DAMAGED,
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
        Err(Err(e)) if e.to_string() == super::capture::DAMAGED => Err(AppError::conflict("The copy of this file's content is damaged")),
        Err(Err(e)) => Err(AppError::new(axum::http::StatusCode::BAD_GATEWAY, format!("Couldn't read from the copy's location: {}", crate::locations::describe(&e)))),
    }
}

/// Into a content-store space: the content is stored on the space's location unless it is kept already, then the file
/// is recorded
#[allow(clippy::too_many_arguments)]
async fn into_store(
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
) -> AppResult<bool> {
    let st = cx.st;
    let staged = match tree::stage_blob(st, &target.id, hash.to_string(), size, tmp.to_path_buf()).await {
        Ok(s) => s,
        // Deleted meanwhile, and not fetched: fetch it and try again
        Err(_) if tokio::fs::metadata(tmp).await.is_err() => {
            match fetch(cx, set, dst, hash, size, tmp).await? {
                Ok(()) => {}
                Err(_) => return Err(AppError::internal("stopped while fetching")),
            }
            tree::stage_blob(st, &target.id, hash.to_string(), size, tmp.to_path_buf()).await?
        }
        Err(e) => return Err(e),
    };
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        let parent = tree::get_node(&mut tx, parent).await?.filter(|n| n.is_folder() && n.trashed_at.is_none()).ok_or_else(|| AppError::conflict("The folder being restored into was deleted"))?;
        tree::check_quota(&mut tx, &target.id, size).await?;
        let free = tree::unique_name(&mut tx, &parent.id, name, false).await?;
        let extra = tree::commit_blob(&mut tx, &staged).await?;
        let id = new_id();
        sqlx::query(
            "INSERT INTO nodes (id, owner_id, parent_id, kind, name, blob_hash, size, mime, drive_id, created_at, updated_at)
             VALUES (?, ?, ?, 'file', ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(owner_of(cx, target))
        .bind(&parent.id)
        .bind(&free)
        .bind(hash)
        .bind(size)
        .bind(crate::util::guess_mime(&free))
        .bind(&target.id)
        .bind(now())
        .bind(modified)
        .execute(&mut *tx)
        .await?;
        tree::adjust_usage(&mut tx, &target.id, size).await?;
        sqlx::query("INSERT INTO backup_restored (job_id, source_id, node_id) VALUES (?, ?, ?)").bind(&cx.job.id).bind(source).bind(&id).execute(&mut *tx).await?;
        AppResult::Ok((extra, free != name))
    }
    .await;
    match crate::db::settle(tx, res).await {
        Ok((extra, renamed)) => {
            tree::finish_staged(st, staged, extra).await;
            Ok(renamed)
        }
        Err(e) => {
            tree::abandon_staged(st, staged).await;
            Err(e)
        }
    }
}

/// Into a folder space: the file is written into the folder under a name scans ignore, then renamed into place and
/// indexed
async fn into_folder(cx: &Ctx<'_>, target: &tree::Drive, parent: &str, name: &str, source: &str, tmp: &std::path::Path, size: i64) -> AppResult<bool> {
    let st = cx.st;
    let folder = folder_node(st, parent).await?;
    let staged = crate::fsops::stage_upload(st, &folder, tmp, size as u64).await?;
    let _space = crate::fsops::lock_space(&target.id).await;
    let ready = crate::fsops::ready(st, &target.id).await;
    let _w = st.write_lock.lock().await;
    let result = async {
        ready?;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            let folder = tree::get_node(&mut tx, &folder.id).await?.filter(|n| n.trashed_at.is_none()).ok_or_else(|| AppError::conflict("The folder being restored into was deleted"))?;
            tree::check_quota(&mut tx, &target.id, size).await?;
            let free = crate::fsops::free_name(&mut tx, &folder, name, false).await?;
            let id = crate::fsops::place_file(&mut tx, &staged, owner_of(cx, target), &folder, &free).await?;
            tree::adjust_usage(&mut tx, &target.id, size).await?;
            sqlx::query("INSERT INTO backup_restored (job_id, source_id, node_id) VALUES (?, ?, ?)").bind(&cx.job.id).bind(source).bind(&id).execute(&mut *tx).await?;
            AppResult::Ok(free != name)
        }
        .await;
        crate::db::settle(tx, res).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(staged.as_path()).await;
    }
    result
}
