//! A folder space to a folder on another location (the built-in disk to a NAS added as a Local folder location, say),
//! or a folder an administrator chose onto a location. The space is read-only meanwhile.
//!
//! When both folders are on the same file system, the folder is simply renamed to its new place (holding the space,
//! so no scan or change runs meanwhile). Otherwise:
//!
//! 1. Copy: the whole folder is copied, the trash, versions and the space's marker included, each file with its date,
//!    and checked by its size. Each copy records the original's identity, size and date, and each folder made in the
//!    new folder is recorded too.
//! 2. Before the switch the folder is scanned and copied again where anything changed; copies of what was removed or
//!    renamed go, folders included.
//! 3. Switch, in one transaction: the space points at its new folder, and its items at the copies (their paths below
//!    it stay the same). Then the old folder is removed, each file only when it is still what was copied.

use std::{
    collections::HashSet,
    io::Write,
    path::{Path, PathBuf},
};

use super::{Ctx, Stop};
use crate::{
    beneath::Pinned,
    error::{AppError, AppResult},
    folders::MARKER,
    fsops::disk_error,
    util::new_id,
};

/// Scan-and-copy rounds before the switch
const ROUNDS: usize = 10;

#[cfg(test)]
thread_local! {
    /// Tests: treat the two folders as being on different disks, so the folder is copied
    pub static OTHER_DISK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn other_disk() -> bool {
    #[cfg(test)]
    return OTHER_DISK.with(|d| d.get());
    #[cfg(not(test))]
    false
}

#[cfg(test)]
thread_local! {
    /// Tests: ThirtyFile stops right after the folder was renamed, before the switch is recorded
    pub static STOP_AFTER_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Tests: the new folder doesn't tell letter case apart (as on CIFS or exFAT)
    pub static IGNORES_CASE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn not_mounted() -> AppError {
    AppError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, crate::storage::NOT_MOUNTED)
}

pub async fn run(cx: &Ctx<'_>) -> AppResult<Stop> {
    let (st, job) = (cx.st, cx.job);
    let from = PathBuf::from(job.from_path.clone().unwrap_or_default());
    if resume_rename(cx, &from).await? {
        return Ok(Stop::Done);
    }
    let source = super::to_store::source(job)?;
    let target = super::to_folder::target(cx).await?;
    let (copied,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM space_move_items WHERE move_id = ?").bind(&job.id).fetch_one(&st.db).await?;
    if copied == 0 && renamed(cx, &from, &target).await? {
        return Ok(Stop::Done);
    }
    // Copied: the new folder's disk must have room for what is left
    super::check_room(st, &job.to_location, super::left_to_copy(st, job).await?).await?;
    let dest = Pinned::root(&target).map_err(|_| not_mounted())?;
    let ignores_case = ignores_case(&dest).await?;
    let (files_done, bytes_done): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(size), 0) FROM space_move_items WHERE move_id = ? AND kind = 'path'").bind(&job.id).fetch_one(&st.db).await?;
    // What the index has: files of the space (the trash included) and their versions
    let (files, bytes): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM nodes WHERE drive_id = ?1 AND kind = 'file') + (SELECT COUNT(*) FROM node_versions WHERE drive_id = ?1),
                (SELECT COALESCE(SUM(size), 0) FROM nodes WHERE drive_id = ?1 AND kind = 'file')
                  + (SELECT COALESCE(SUM(size), 0) FROM node_versions WHERE drive_id = ?1)",
    )
    .bind(&job.drive_id)
    .fetch_one(&st.db)
    .await?;
    cx.set_counts(files_done, bytes_done, files.max(files_done), bytes.max(bytes_done));
    cx.flush().await?;
    if let Some(stop) = copy_all(cx, &source, &dest, ignores_case, false).await? {
        return Ok(stop);
    }
    for _ in 0..ROUNDS {
        let report = crate::folders::scan(st, &job.drive_id).await?;
        if let Some(e) = report.error {
            return Err(AppError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, e));
        }
        if let Some(stop) = copy_all(cx, &source, &dest, ignores_case, true).await? {
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
        if switch(cx, &target).await? {
            super::clean_up(st, job).await;
            return Ok(Stop::Done);
        }
        tokio::time::sleep(std::time::Duration::from_secs(if cfg!(test) { 0 } else { 5 })).await;
    }
    Err(AppError::conflict("The space kept changing while it was being moved. Try again later."))
}

/// Records whether the space's folder may have been renamed to its new place without the switch being recorded yet
/// (`space_moves.renamed`)
async fn set_renamed(cx: &Ctx<'_>, renamed: bool) -> AppResult<()> {
    let _w = cx.st.write_lock.lock().await;
    sqlx::query("UPDATE space_moves SET renamed = ? WHERE id = ?").bind(renamed).bind(&cx.job.id).execute(&cx.st.db).await?;
    Ok(())
}

/// Same file system: the folder is renamed to its new place, holding the space, and the space follows in one
/// transaction (the rename is undone when that fails). The step is recorded first, so a move stopped in between
/// finishes it when it runs again (`resume_rename`), or puts the folder back when it is cancelled
/// (to_folder::remove_copies). False when the folder is copied instead: the folders are on different disks, or the
/// new folder can't make way.
async fn renamed(cx: &Ctx<'_>, from: &Path, to: &Path) -> AppResult<bool> {
    let job = cx.job;
    if other_disk() {
        return Ok(false);
    }
    let held = crate::folders::hold(cx.st, &job.drive_id).await;
    set_renamed(cx, true).await?;
    let (f, t, drive) = (from.to_path_buf(), to.to_path_buf(), job.drive_id.clone());
    let moved = tokio::task::spawn_blocking(move || -> std::io::Result<bool> {
        // The new folder was made (with the marker) to keep its name: it makes way
        let _ = std::fs::remove_file(t.join(MARKER));
        if let Err(e) = std::fs::remove_dir(&t) {
            // Something else is in it: it keeps its marker, and the files are copied into it
            tracing::info!("{} can't make way for the space's folder ({e}): its files are copied instead", t.display());
            crate::folders::mark_space(&t, &drive)?;
            return Ok(false);
        }
        if let Err(e) = std::fs::rename(&f, &t) {
            // Another disk (or a rename refused): the files are copied instead, into the folder made again
            if e.kind() != std::io::ErrorKind::CrossesDevices {
                tracing::info!("Renaming {} failed ({e}): its files are copied instead", f.display());
            }
            std::fs::create_dir(&t).and_then(|()| crate::folders::mark_space(&t, &drive))?;
            return Ok(false);
        }
        Ok(true)
    })
    .await
    .map_err(AppError::internal)?
    .map_err(disk_error)?;
    if !moved {
        set_renamed(cx, false).await?;
        return Ok(false);
    }
    #[cfg(test)]
    if STOP_AFTER_RENAME.with(|s| s.get()) {
        return Err(AppError::internal("ThirtyFile stopped (test)"));
    }
    if let Err(e) = commit_rename(cx, to).await {
        let (f, t) = (from.to_path_buf(), to.to_path_buf());
        let back = tokio::task::spawn_blocking(move || std::fs::rename(&t, &f)).await.map_err(AppError::internal)?;
        match back {
            Ok(()) => set_renamed(cx, false).await?,
            // Still recorded as renamed: the next run records the switch, a cancel tries again to put it back
            Err(b) => tracing::error!("Couldn't put {} back after a failed move: {b}", from.display()),
        }
        return Err(e);
    }
    drop(held);
    Ok(true)
}

/// Records the switch to the renamed folder, in one transaction
async fn commit_rename(cx: &Ctx<'_>, to: &Path) -> AppResult<()> {
    let (st, job) = (cx.st, cx.job);
    let (files, bytes): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(size), 0) FROM nodes WHERE drive_id = ? AND kind = 'file'").bind(&job.drive_id).fetch_one(&st.db).await?;
    cx.set_counts(files, bytes, files, bytes);
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            sqlx::query("UPDATE drives SET location_id = ?2, source_path = ?3, last_scan_at = NULL, scan_report = NULL WHERE id = ?1")
                .bind(&job.drive_id)
                .bind(&job.to_location)
                .bind(to.to_string_lossy())
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE space_moves SET renamed = 0 WHERE id = ?").bind(&job.id).execute(&mut *tx).await?;
            super::finish(&mut tx, cx, false, None).await
        }
        .await;
        crate::db::settle(tx, res).await?;
    }
    crate::folders::spaces_changed(st);
    crate::folders::scan_later(st, &job.drive_id);
    Ok(())
}

/// Where the space's folder is: in its old place, in the new one, or in neither (a disk that isn't mounted)
#[derive(Debug, PartialEq)]
pub(super) enum Found {
    Old,
    New,
    Neither,
}

/// Finds the space's folder by its marker, after a rename that may or may not have happened
pub(super) async fn find_folder(drive_id: &str, from: &Path, to: &Path) -> AppResult<Found> {
    let (drive, from, to) = (drive_id.to_string(), from.to_path_buf(), to.to_path_buf());
    tokio::task::spawn_blocking(move || {
        let ours = |p: &Path| Pinned::root(p).ok().and_then(|r| crate::folders::space_marker(&r).ok().flatten()).is_some_and(|id| id == drive);
        if ours(&from) {
            Found::Old
        } else if ours(&to) {
            Found::New
        } else {
            Found::Neither
        }
    })
    .await
    .map_err(AppError::internal)
}

/// A move that stopped after the step before the rename (`renamed`) was recorded: when the folder is in its new place,
/// the switch is recorded now (true); when it is still in its old place, the new folder is made again where it made way,
/// and the move goes on as usual (false)
async fn resume_rename(cx: &Ctx<'_>, from: &Path) -> AppResult<bool> {
    let (st, job) = (cx.st, cx.job);
    let (to, renamed): (Option<String>, bool) = sqlx::query_as("SELECT to_path, renamed FROM space_moves WHERE id = ?").bind(&job.id).fetch_one(&st.db).await?;
    let (Some(to), true) = (to, renamed) else { return Ok(false) };
    let to = PathBuf::from(to);
    let held = crate::folders::hold(st, &job.drive_id).await;
    match find_folder(&job.drive_id, from, &to).await? {
        Found::New => {
            commit_rename(cx, &to).await?;
            drop(held);
            Ok(true)
        }
        Found::Old => {
            let (t, drive) = (to.clone(), job.drive_id.clone());
            tokio::task::spawn_blocking(move || {
                match std::fs::create_dir(&t) {
                    Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e),
                    _ => {}
                }
                crate::folders::mark_space(&t, &drive)
            })
            .await
            .map_err(AppError::internal)?
            .map_err(disk_error)?;
            set_renamed(cx, false).await?;
            Ok(false)
        }
        Found::Neither => Err(not_mounted()),
    }
}

/// Whether the new folder's file system takes names that differ only in letter case for the same name (CIFS, exFAT,
/// and Windows and macOS disks by default)
async fn ignores_case(dest: &Pinned) -> AppResult<bool> {
    #[cfg(test)]
    if IGNORES_CASE.with(|c| c.get()) {
        return Ok(true);
    }
    let dest = dest.clone();
    tokio::task::spawn_blocking(move || -> std::io::Result<bool> {
        let name = format!("{}case-{}", crate::fsops::COPY_PREFIX, new_id());
        let probe = dest.join(&name)?;
        std::fs::File::create_new(probe.as_path())?;
        let same = dest.join(&name.to_uppercase()).and_then(|p| std::fs::symlink_metadata(p.as_path())).is_ok();
        let _ = std::fs::remove_file(probe.as_path());
        Ok(same)
    })
    .await
    .map_err(AppError::internal)?
    .map_err(disk_error)
}

/// Copies the folder: what isn't copied yet, or changed since. After the scan before the switch (`last`), copies of
/// what is no longer there go too. When the new folder doesn't tell letter case apart (`ignores_case`), items whose
/// names differ only in letter case would become one there: they aren't copied, and are listed as failed after the
/// scan, so the move stops until they are renamed. It goes a folder at a time, reading what was copied of that folder
/// only, so what it holds doesn't grow with the space.
async fn copy_all(cx: &Ctx<'_>, source: &Pinned, dest: &Pinned, ignores_case: bool, last: bool) -> AppResult<Option<Stop>> {
    let (st, job) = (cx.st, cx.job);
    let mut queue = vec![String::new()];
    while let Some(dir) = queue.pop() {
        let entries = {
            let (source, dir) = (source.clone(), dir.clone());
            tokio::task::spawn_blocking(move || -> std::io::Result<Vec<(String, std::fs::Metadata)>> {
                let at = if dir.is_empty() { source } else { source.join(&dir)?.dir()? };
                let mut out = Vec::new();
                for e in std::fs::read_dir(at.as_path())? {
                    let e = e?;
                    let Ok(name) = e.file_name().into_string() else { continue };
                    out.push((name, e.metadata()?));
                }
                Ok(out)
            })
            .await
            .map_err(AppError::internal)?
            .map_err(disk_error)?
        };
        // What was copied of this folder: its files, and the folders made in it (`FOLDER`)
        let copied: std::collections::HashMap<String, (Option<i64>, i64, Option<i64>)> =
            in_folder(st, job, &dir, "path").await?.into_iter().map(|(p, ino, size, mtime)| (p, (ino, size, mtime))).collect();
        let made: HashSet<String> = in_folder(st, job, &dir, FOLDER).await?.into_iter().map(|(p, ..)| p).collect();
        let mut found: HashSet<String> = HashSet::new();
        let clashing = if ignores_case { case_clashes(entries.iter().map(|(name, _)| name.as_str())) } else { HashSet::new() };
        for (name, meta) in entries {
            let rel = crate::fsops::child_rel(&dir, &name);
            // Its own marker is in the new folder already; links are left where they are
            if (dir.is_empty() && name == MARKER) || meta.file_type().is_symlink() {
                continue;
            }
            if clashing.contains(&name) {
                if last {
                    cx.failed(&cx.job.drive_id, Some(rel), CASE_CLASH.to_string());
                }
                continue;
            }
            if meta.is_dir() {
                let (dest, rel2) = (dest.clone(), rel.clone());
                tokio::task::spawn_blocking(move || dest.join(&rel2).and_then(|d| crate::fsops::ensure_dir(&d)))
                    .await
                    .map_err(AppError::internal)?
                    .map_err(disk_error)?;
                if !made.contains(&rel) {
                    let _w = st.write_lock.lock().await;
                    sqlx::query("INSERT OR REPLACE INTO space_move_items (move_id, item_id, kind, path) VALUES (?, ?, ?, NULL)")
                        .bind(&job.id)
                        .bind(&rel)
                        .bind(FOLDER)
                        .execute(&st.db)
                        .await?;
                }
                found.insert(rel.clone());
                queue.push(rel);
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            found.insert(rel.clone());
            let now = crate::folders::seen(&meta);
            if copied.get(&rel).is_some_and(|(ino, size, mtime)| (*ino == Some(now.ino) || now.ino == 0) && *size == now.size && *mtime == Some(now.mtime_ns)) {
                continue;
            }
            if let Some(stop) = cx.stop() {
                return Ok(Some(stop));
            }
            let res = {
                let (source, dest, rel2) = (source.clone(), dest.clone(), rel.clone());
                cx.tries(
                    |e: &std::io::Error| crate::hashing::unusable_kind(e) == Some(crate::hashing::Unusable::Changed),
                    || {
                        let (source, dest, rel2) = (source.clone(), dest.clone(), rel2.clone());
                        async move { tokio::task::spawn_blocking(move || copy_file(&source, &dest, &rel2)).await.map_err(std::io::Error::other)? }
                    },
                )
                .await
            };
            match res {
                Ok((src, dst)) => record(cx, &rel, src, dst).await?,
                Err(Ok(stop)) => return Ok(Some(stop)),
                Err(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(Err(e)) => cx.failed(&cx.job.drive_id, Some(rel.clone()), disk_error(e).message),
            }
        }
        if last {
            // Copies of what is no longer in this folder (removed, or renamed: copied under its new name)
            for rel in copied.keys().filter(|p| !found.contains(*p)) {
                forget_copy(st, job, dest, rel).await?;
            }
            // Folders made for what went, with the copies in them, deepest first
            for rel in made.iter().filter(|p| !found.contains(*p)) {
                forget_folder(st, job, dest, rel).await?;
            }
        }
    }
    Ok(None)
}

/// What was recorded of the folder `dir` (a path below the space's folder; "" for itself) of this kind: its items
/// directly in it, (path, inode, size, modification time)
async fn in_folder(st: &crate::state::AppState, job: &super::Job, dir: &str, kind: &str) -> AppResult<Vec<(String, Option<i64>, i64, Option<i64>)>> {
    // A range of paths (by the index on the move's items), then only those without a '/' after it
    let (from, to) = if dir.is_empty() { (String::new(), String::from("\u{10FFFF}")) } else { (format!("{dir}/"), format!("{dir}0")) };
    Ok(sqlx::query_as(
        "SELECT item_id, src_ino, COALESCE(size, 0), src_mtime_ns FROM space_move_items
         WHERE move_id = ?1 AND kind = ?2 AND item_id >= ?3 AND item_id < ?4 AND instr(substr(item_id, length(?3) + 1), '/') = 0",
    )
    .bind(&job.id)
    .bind(kind)
    .bind(&from)
    .bind(&to)
    .fetch_all(&st.db)
    .await?)
}

/// Removes the copy of a file that is no longer in the folder, and forgets it
async fn forget_copy(st: &crate::state::AppState, job: &super::Job, dest: &Pinned, rel: &str) -> AppResult<()> {
    if let Ok(p) = dest.join(rel) {
        let _ = std::fs::remove_file(p.as_path());
    }
    let _w = st.write_lock.lock().await;
    sqlx::query("DELETE FROM space_move_items WHERE move_id = ? AND item_id = ?").bind(&job.id).bind(rel).execute(&st.db).await?;
    Ok(())
}

/// Removes a folder made in the new folder for one that is no longer there: the copies in it, the folders made in it
/// (deepest first), then itself, when nothing else is in it
async fn forget_folder(st: &crate::state::AppState, job: &super::Job, dest: &Pinned, rel: &str) -> AppResult<()> {
    let below: Vec<(String, String)> =
        sqlx::query_as("SELECT item_id, kind FROM space_move_items WHERE move_id = ?1 AND item_id >= ?2 AND item_id < ?3 AND kind IN ('path', ?4)")
            .bind(&job.id)
            .bind(format!("{rel}/"))
            .bind(format!("{rel}0"))
            .bind(FOLDER)
            .fetch_all(&st.db)
            .await?;
    for (p, _) in below.iter().filter(|(_, k)| k == "path") {
        forget_copy(st, job, dest, p).await?;
    }
    let mut dirs: Vec<&String> = below.iter().filter(|(_, k)| k == FOLDER).map(|(p, _)| p).collect();
    dirs.sort_by_key(|p| std::cmp::Reverse(p.len()));
    let rel = rel.to_string();
    for p in dirs.into_iter().chain(std::iter::once(&rel)) {
        let removed = match dest.join(p) {
            Ok(d) => std::fs::remove_dir(d.as_path()),
            Err(e) => Err(e),
        };
        if removed.is_ok() || matches!(removed, Err(ref e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory)) {
            let _w = st.write_lock.lock().await;
            sqlx::query("DELETE FROM space_move_items WHERE move_id = ? AND item_id = ? AND kind = ?").bind(&job.id).bind(p).bind(FOLDER).execute(&st.db).await?;
        }
    }
    Ok(())
}

/// `space_move_items.kind` of a folder made in the new folder, by its path below it (files are 'path')
const FOLDER: &str = "folder";

const CASE_CLASH: &str =
    "Another item in its folder has the same name in other letter case, which the new folder can't tell apart. Rename one of them, then resume the move.";

/// The names of a folder that another of its names equals but for letter case
fn case_clashes<'a>(names: impl Iterator<Item = &'a str>) -> HashSet<String> {
    let mut by_key: std::collections::HashMap<String, Vec<&str>> = std::collections::HashMap::new();
    for name in names {
        by_key.entry(name.to_lowercase()).or_default().push(name);
    }
    by_key.into_values().filter(|names| names.len() > 1).flatten().map(str::to_string).collect()
}

/// Copies a file to the same path in the new folder (under a temporary name, then renamed over an older copy), with its
/// date; returns what the original and the copy were
fn copy_file(source: &Pinned, dest: &Pinned, rel: &str) -> std::io::Result<(crate::folders::Seen, crate::folders::Seen)> {
    let from = source.join(rel)?;
    let mut src = from.open_file()?;
    let before = crate::folders::seen(&src.metadata()?);
    let to = dest.join(rel)?;
    let dir = to.parent().ok_or_else(|| std::io::Error::other("no folder"))?;
    let tmp = dir.join(&format!("{}{}", crate::fsops::COPY_PREFIX, new_id()))?;
    let copied = (|| {
        let mut out = std::fs::File::create_new(tmp.as_path())?;
        let n = std::io::copy(&mut src, &mut out)?;
        out.flush()?;
        out.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_nanos(before.mtime_ns.max(0) as u64))?;
        out.sync_all()?;
        drop(out);
        let after = crate::folders::seen(&std::fs::symlink_metadata(from.as_path())?);
        if after != before || n as i64 != before.size {
            return Err(crate::hashing::unusable(crate::hashing::Unusable::Changed, crate::folders::CHANGED));
        }
        std::fs::rename(tmp.as_path(), to.as_path())?;
        Ok(crate::folders::seen(&std::fs::symlink_metadata(to.as_path())?))
    })();
    if copied.is_err() {
        let _ = std::fs::remove_file(tmp.as_path());
    }
    copied.map(|dst| (before, dst))
}

/// Records a copy; the index follows what was read, when it differs (a scan leaves files changed in the last seconds
/// for later)
async fn record(cx: &Ctx<'_>, rel: &str, src: crate::folders::Seen, dst: crate::folders::Seen) -> AppResult<()> {
    let (st, job) = (cx.st, cx.job);
    {
        let _w = st.write_lock.lock().await;
        sqlx::query(
            "INSERT OR REPLACE INTO space_move_items (move_id, item_id, kind, size, path, src_dev, src_ino, src_mtime_ns, dst_dev, dst_ino, dst_mtime_ns)
             VALUES (?, ?, 'path', ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&job.id)
        .bind(rel)
        .bind(src.size)
        .bind(rel)
        .bind(src.dev)
        .bind(src.ino)
        .bind(src.mtime_ns)
        .bind(dst.dev)
        .bind(dst.ino)
        .bind(dst.mtime_ns)
        .execute(&st.db)
        .await?;
        sqlx::query(
            "UPDATE nodes SET size = ?1, fs_size = ?1, fs_mtime_ns = ?2, fs_dev = ?3, fs_ino = ?4
             WHERE drive_id = ?5 AND fs_path = ?6 AND kind = 'file' AND (fs_size IS NOT ?1 OR fs_mtime_ns IS NOT ?2)",
        )
        .bind(src.size)
        .bind(src.mtime_ns)
        .bind(src.dev)
        .bind(src.ino)
        .bind(&job.drive_id)
        .bind(rel)
        .execute(&st.db)
        .await?;
    }
    cx.done(1, src.size).await
}

/// Switches the space to its new folder in one transaction, holding it. False when something isn't copied as the index
/// has it now.
async fn switch(cx: &Ctx<'_>, target: &Path) -> AppResult<bool> {
    let (st, job) = (cx.st, cx.job);
    let held = crate::folders::hold(st, &job.drive_id).await;
    let w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        let pending: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM nodes n LEFT JOIN space_move_items i ON i.move_id = ?1 AND i.item_id = n.fs_path AND i.kind = 'path'
             WHERE n.drive_id = ?2 AND n.kind = 'file' AND (i.item_id IS NULL OR i.size IS NOT n.fs_size OR i.src_mtime_ns IS NOT n.fs_mtime_ns)
             UNION ALL
             SELECT 1 FROM nodes n LEFT JOIN space_move_items i ON i.move_id = ?1 AND i.item_id = n.fs_path AND i.kind = 'folder'
             WHERE n.drive_id = ?2 AND n.kind = 'folder' AND n.fs_path != '' AND i.item_id IS NULL
             UNION ALL
             SELECT 1 FROM node_versions v LEFT JOIN space_move_items i ON i.move_id = ?1 AND i.item_id = v.fs_path AND i.kind = 'path'
             WHERE v.drive_id = ?2 AND v.fs_path IS NOT NULL AND i.item_id IS NULL
             LIMIT 1",
        )
        .bind(&job.id)
        .bind(&job.drive_id)
        .fetch_optional(&mut *tx)
        .await?;
        if pending.is_some() {
            return Ok(false);
        }
        sqlx::query(
            "UPDATE nodes SET fs_dev = i.dst_dev, fs_ino = i.dst_ino, fs_mtime_ns = i.dst_mtime_ns, fs_birth_ns = NULL
             FROM space_move_items i WHERE i.move_id = ?1 AND i.kind = 'path' AND nodes.drive_id = ?2 AND nodes.fs_path = i.item_id",
        )
        .bind(&job.id)
        .bind(&job.drive_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE drives SET location_id = ?2, source_path = ?3, last_scan_at = NULL, scan_report = NULL WHERE id = ?1")
            .bind(&job.drive_id)
            .bind(&job.to_location)
            .bind(target.to_string_lossy())
            .execute(&mut *tx)
            .await?;
        super::finish(&mut tx, cx, true, None).await?;
        Ok(true)
    }
    .await;
    let switched = crate::db::settle(tx, res).await?;
    drop((w, held));
    if switched {
        crate::folders::spaces_changed(st);
        // Folders get the new disk's identities from a scan
        crate::folders::scan_later(st, &job.drive_id);
    }
    Ok(switched)
}
