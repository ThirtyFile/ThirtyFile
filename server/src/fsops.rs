//! Changing folder spaces from the web.
//!
//! The folder on disk is the source of truth, so every change is made there first and the index follows only once
//! that worked: a failure leaves both as they were, and anything the index misses is picked up by the next scan.
//! A web change to a folder space and a scan of it take turns (the space's scan lock, always taken before the write
//! lock), so a scan never sees half of a change. New content is written under a name scans ignore (`.thirtyfile-…`)
//! and renamed into place once it is complete, and renames never replace an item that is already there.
//!
//! A change of several items can fail after some of them were renamed on disk: the transaction then rolls back, and
//! the renames are put back too (`SpaceLocks` keeps them until the change is committed), so the folder and the index
//! stay as they were.

use std::{
    collections::{HashMap, HashSet},
    io,
    path::{Path, PathBuf},
};

use axum::http::StatusCode;
use sqlx::SqliteConnection;
use tokio::sync::OwnedMutexGuard;

use crate::{
    auth::User,
    error::{AppError, AppResult},
    files::Source,
    logs,
    state::AppState,
    tree::{self, BlobRef, Need, Node, StagedBlob},
    util::{guess_mime, new_id, now, numbered_name, split_name},
    versions,
};

/// A folder space's trash, in its folder: `.thirtyfile-trash/<trash id>/<name>` (never indexed)
pub const TRASH_DIR: &str = ".thirtyfile-trash";

/// What the index keeps of an item on disk
#[derive(Debug, Clone, Copy)]
pub struct Stat {
    pub is_dir: bool,
    pub dev: i64,
    pub ino: i64,
    pub size: i64,
    pub mtime_ns: i64,
}

pub fn stat(path: &Path) -> io::Result<Stat> {
    let meta = std::fs::symlink_metadata(path)?;
    let (dev, ino) = crate::folders::identity(&meta);
    Ok(Stat {
        is_dir: meta.is_dir(),
        dev,
        ino,
        size: if meta.is_dir() { 0 } else { meta.len() as i64 },
        mtime_ns: crate::folders::mtime_ns(&meta),
    })
}

pub fn child_rel(parent: &str, name: &str) -> String {
    if parent.is_empty() { name.to_string() } else { format!("{parent}/{name}") }
}

fn rel_of(n: &Node) -> &str {
    n.fs_path.as_deref().unwrap_or_default()
}

/// The item on disk
fn abs(n: &Node) -> AppResult<PathBuf> {
    n.fs_file().ok_or_else(|| AppError::not_found("Item not found"))
}

fn under(top: &Path, rel: &str) -> PathBuf {
    if rel.is_empty() { top.to_path_buf() } else { top.join(rel) }
}

/// A disk error as people see it
pub fn disk_error(e: io::Error) -> AppError {
    match e.kind() {
        io::ErrorKind::AlreadyExists => AppError::conflict("An item with the same name already exists"),
        io::ErrorKind::NotFound => AppError::not_found("The item is no longer in the folder on the server"),
        io::ErrorKind::PermissionDenied => AppError::forbidden("ThirtyFile isn't allowed to change this in the folder on the server"),
        io::ErrorKind::StorageFull => AppError::new(StatusCode::INSUFFICIENT_STORAGE, "The disk of the folder on the server is full"),
        _ => AppError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("Couldn't change the folder on the server: {e}")),
    }
}

fn incomplete() -> AppError {
    AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "A file wasn't copied completely")
}

// ───────────── Taking turns with scans ─────────────

/// A rename made on disk by a change that isn't committed yet
struct Renamed {
    /// Where the item is now, and where it was
    now: PathBuf,
    was: PathBuf,
    /// A folder made for the rename (a trash folder), removed again when undoing
    made: Option<PathBuf>,
}

/// Scan locks of the folder spaces a change touches, held until it is done. They also keep the renames the change
/// made on disk: unless `committed` is called, dropping them puts those back (still holding the locks), newest first.
pub struct SpaceLocks {
    held: Vec<(String, OwnedMutexGuard<()>)>,
    renamed: std::sync::Mutex<Vec<Renamed>>,
}

impl SpaceLocks {
    /// An item of a folder space must be in a locked space (it could have moved to another one meanwhile)
    pub fn check(&self, n: &Node) -> AppResult<()> {
        if n.in_folder_space() && !self.held.iter().any(|(d, _)| d == n.drive()) {
            return Err(AppError::conflict("Something changed at the same time. Try again."));
        }
        Ok(())
    }

    fn note(&self, now: PathBuf, was: PathBuf, made: Option<PathBuf>) {
        self.renamed.lock().unwrap_or_else(|e| e.into_inner()).push(Renamed { now, was, made });
    }

    /// The change is in the index: its renames stay
    pub fn committed(&self) {
        self.renamed.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

impl Drop for SpaceLocks {
    fn drop(&mut self) {
        let renamed = std::mem::take(self.renamed.get_mut().unwrap_or_else(|e| e.into_inner()));
        for r in renamed.into_iter().rev() {
            if let Some(dir) = r.was.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            match rename_new(&r.now, &r.was) {
                Ok(()) => {
                    if let Some(made) = r.made {
                        let _ = std::fs::remove_dir(made);
                    }
                }
                Err(e) => tracing::error!("Couldn't put {} back to {} after a failed change: {e}", r.now.display(), r.was.display()),
            }
        }
    }
}

/// Locks the folder spaces holding these items (ids, or aliases such as `root`), always in the same order
pub async fn lock(st: &AppState, user: &User, ids: &[&str]) -> AppResult<SpaceLocks> {
    let ids: Vec<&str> = ids.iter().filter_map(|id| tree::resolve_alias(user, id).ok()).collect();
    let drives: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT n.drive_id FROM nodes n JOIN drives d ON d.id = n.drive_id
         WHERE d.mode = 'folder' AND n.id IN (SELECT value FROM json_each(?)) ORDER BY n.drive_id",
    )
    .bind(serde_json::to_string(&ids).unwrap())
    .fetch_all(&st.db)
    .await?;
    let mut held = Vec::with_capacity(drives.len());
    for (d,) in drives {
        let guard = lock_space(&d).await;
        held.push((d, guard));
    }
    Ok(SpaceLocks { held, renamed: Default::default() })
}

pub async fn lock_space(drive_id: &str) -> OwnedMutexGuard<()> {
    crate::folders::drive_lock(drive_id).lock_owned().await
}

// ───────────── Disk ─────────────

/// Renames `from` to `to` without replacing anything already at `to`
pub fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let a = CString::new(from.as_os_str().as_bytes()).map_err(io::Error::other)?;
        let b = CString::new(to.as_os_str().as_bytes()).map_err(io::Error::other)?;
        // SAFETY: two valid, NUL-terminated paths
        let r = unsafe { libc::syscall(libc::SYS_renameat2, libc::AT_FDCWD, a.as_ptr(), libc::AT_FDCWD, b.as_ptr(), libc::RENAME_NOREPLACE) };
        if r == 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        match e.raw_os_error() {
            // File systems without it (some network shares): look first, then rename
            Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP) => {}
            // Only the letter case changes, on a file system that ignores it: the same item
            Some(libc::EEXIST) if same_item(from, to) => return std::fs::rename(from, to),
            _ => return Err(e),
        }
    }
    if std::fs::symlink_metadata(to).is_ok() && !same_item(from, to) {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "an item with this name already exists"));
    }
    std::fs::rename(from, to)
}

fn same_item(a: &Path, b: &Path) -> bool {
    match (stat(a), stat(b)) {
        (Ok(x), Ok(y)) if x.ino != 0 => (x.dev, x.ino) == (y.dev, y.ino),
        _ => a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()),
    }
}

/// An original that was copied: removed afterwards only if it is still what was copied
struct Copied {
    path: PathBuf,
    is_dir: bool,
    /// A file's size and modification time when it was copied
    seen: Option<(u64, i64)>,
}

/// Copies a file or folder on disk, including what the index doesn't have yet; symbolic links stay links. `copied`
/// lists the originals, folders before what is in them.
fn copy_tree(from: &Path, to: &Path, copied: &mut Vec<Copied>) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.is_dir() {
        std::fs::create_dir(to)?;
        copied.push(Copied { path: from.to_path_buf(), is_dir: true, seen: None });
        for item in std::fs::read_dir(from)? {
            let item = item?;
            copy_tree(&item.path(), &to.join(item.file_name()), copied)?;
        }
    } else if meta.file_type().is_symlink() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(std::fs::read_link(from)?, to)?;
        copied.push(Copied { path: from.to_path_buf(), is_dir: false, seen: None });
    } else if meta.is_file() {
        // On btrfs and XFS the copy can share the original's blocks
        if std::fs::copy(from, to)? != meta.len() {
            return Err(io::Error::other("a file wasn't copied completely"));
        }
        copied.push(Copied { path: from.to_path_buf(), is_dir: false, seen: Some((meta.len(), crate::folders::mtime_ns(&meta))) });
    }
    Ok(())
}

/// Removes the originals of a copy: files unchanged since they were copied, then folders left empty. Anything added or
/// changed meanwhile (over SMB, say) stays, and the next scan shows it.
fn remove_copied(copied: Vec<Copied>) {
    for c in copied.iter().filter(|c| !c.is_dir) {
        let unchanged = match c.seen {
            Some((size, mtime)) => std::fs::symlink_metadata(&c.path).is_ok_and(|m| m.len() == size && crate::folders::mtime_ns(&m) == mtime),
            None => true,
        };
        if !unchanged {
            tracing::info!("Kept {}: it changed while it was being moved", c.path.display());
            continue;
        }
        if let Err(e) = std::fs::remove_file(&c.path)
            && e.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!("Couldn't remove {} from disk: {e}", c.path.display());
        }
    }
    for c in copied.iter().rev().filter(|c| c.is_dir) {
        let _ = std::fs::remove_dir(&c.path);
    }
}

fn remove_all(p: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(p)?.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) }
}

fn older_than(p: &Path, age: std::time::Duration) -> bool {
    std::fs::symlink_metadata(p).and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|a| a >= age))
}

/// Removes items from disk in the background (the index no longer has them)
pub fn remove_later(paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    tokio::task::spawn_blocking(move || {
        for p in paths {
            if let Err(e) = remove_all(&p)
                && e.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!("Couldn't remove {} from disk: {e}", p.display());
            }
        }
    });
}

// ───────────── Index ─────────────

/// A name that neither the index nor the disk has in `parent` yet: `name`, else "name (1)", "name (2)"…
pub async fn free_name(conn: &mut SqliteConnection, parent: &Node, name: &str, is_folder: bool) -> AppResult<String> {
    let dir = abs(parent)?;
    for n in 0..10_000u32 {
        let candidate = if n == 0 { name.to_string() } else { numbered_name(name, n, is_folder) };
        if !tree::name_taken(conn, &parent.id, &candidate).await? && std::fs::symlink_metadata(dir.join(&candidate)).is_err() {
            return Ok(candidate);
        }
    }
    Err(AppError::conflict("Too many items with the same name"))
}

/// Adds an item that is now on disk at `rel` to the index
#[allow(clippy::too_many_arguments)]
async fn insert_at(conn: &mut SqliteConnection, id: &str, owner: i64, parent_id: &str, drive_id: &str, name: &str, rel: &str, s: &Stat) -> AppResult<()> {
    let ts = now();
    sqlx::query(
        "INSERT INTO nodes (id, owner_id, parent_id, kind, name, size, mime, drive_id, created_at, updated_at,
                            fs_path, fs_dev, fs_ino, fs_size, fs_mtime_ns)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(owner)
    .bind(parent_id)
    .bind(if s.is_dir { "folder" } else { "file" })
    .bind(name)
    .bind(s.size)
    .bind(if s.is_dir { String::new() } else { guess_mime(name) })
    .bind(drive_id)
    .bind(ts)
    .bind(ts)
    .bind(rel)
    .bind(s.dev)
    .bind(s.ino)
    .bind(s.size)
    .bind(s.mtime_ns)
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn insert(conn: &mut SqliteConnection, id: &str, owner: i64, parent: &Node, name: &str, rel: &str, s: &Stat) -> AppResult<()> {
    insert_at(conn, id, owner, &parent.id, parent.drive(), name, rel, s).await
}

/// Records where an item is on disk now
async fn record(conn: &mut SqliteConnection, id: &str, drive_id: &str, rel: &str, s: &Stat) -> AppResult<()> {
    sqlx::query(
        "UPDATE nodes SET drive_id = ?, fs_path = ?, fs_dev = ?, fs_ino = ?, fs_size = ?, fs_mtime_ns = ?,
                          size = CASE WHEN kind = 'file' THEN ? ELSE size END
         WHERE id = ?",
    )
    .bind(drive_id)
    .bind(rel)
    .bind(s.dev)
    .bind(s.ino)
    .bind(s.size)
    .bind(s.mtime_ns)
    .bind(s.size)
    .bind(id)
    .execute(conn)
    .await?;
    Ok(())
}

/// An item and everything in it moved on disk from `old` to `new` (paths below the space's folder)
async fn repath(conn: &mut SqliteConnection, drive_id: &str, old: &str, new: &str) -> AppResult<()> {
    sqlx::query(
        "UPDATE nodes SET fs_path = ?3 || substr(fs_path, length(?2) + 1)
         WHERE drive_id = ?1 AND (fs_path = ?2 OR substr(fs_path, 1, length(?2) + 1) = ?2 || '/')",
    )
    .bind(drive_id)
    .bind(old)
    .bind(new)
    .execute(conn)
    .await?;
    Ok(())
}

// ───────────── Changes within a space ─────────────

/// Makes a folder on disk; one that was made on the server meanwhile is used as it is
pub fn make_dir(parent: &Node, name: &str) -> AppResult<(String, Stat)> {
    let path = abs(parent)?.join(name);
    match std::fs::create_dir(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) => {}
        Err(e) => return Err(disk_error(e)),
    }
    Ok((child_rel(rel_of(parent), name), stat(&path).map_err(disk_error)?))
}

/// Renames an item, or moves it to another folder of its space: on disk, then in the index (the caller records its
/// new name and folder)
pub async fn rename(conn: &mut SqliteConnection, locks: &SpaceLocks, node: &Node, dest: &Node, name: &str) -> AppResult<()> {
    let (from, to) = (abs(node)?, abs(dest)?.join(name));
    rename_new(&from, &to).map_err(disk_error)?;
    locks.note(to, from, None);
    repath(conn, node.drive(), rel_of(node), &child_rel(rel_of(dest), name)).await
}

/// Moves an item to the space's trash folder, where it can be restored from
pub async fn trash(conn: &mut SqliteConnection, locks: &SpaceLocks, node: &Node, trash_id: &str) -> AppResult<()> {
    let root = PathBuf::from(node.fs_root.as_deref().unwrap_or_default());
    let dir = root.join(TRASH_DIR).join(trash_id);
    let from = abs(node)?;
    std::fs::create_dir_all(&dir).map_err(disk_error)?;
    let to = dir.join(&node.name);
    match rename_new(&from, &to) {
        Ok(()) => locks.note(to, from, Some(dir)),
        // Already gone from the server: only the index still had it
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            let _ = std::fs::remove_dir(&dir);
            return Err(disk_error(e));
        }
    }
    repath(conn, node.drive(), rel_of(node), &format!("{TRASH_DIR}/{trash_id}/{}", node.name)).await
}

/// Moves a trashed item back into `dest` as `name`
pub async fn restore(conn: &mut SqliteConnection, locks: &SpaceLocks, node: &Node, dest: &Node, name: &str) -> AppResult<()> {
    let (from, to) = (abs(node)?, abs(dest)?.join(name));
    rename_new(&from, &to).map_err(disk_error)?;
    locks.note(to, from.clone(), None);
    if let Some(dir) = from.parent() {
        let _ = std::fs::remove_dir(dir);
    }
    repath(conn, node.drive(), rel_of(node), &child_rel(rel_of(dest), name)).await
}

/// The folder holding a trashed item of a folder space, removed from disk when the item is deleted for good
pub fn trash_folder(n: &Node) -> Option<PathBuf> {
    let mut parts = n.fs_path.as_deref()?.split('/');
    if parts.next()? != TRASH_DIR {
        return None;
    }
    let id = parts.next().filter(|id| !id.is_empty() && *id != "." && *id != "..")?;
    Some(Path::new(n.fs_root.as_deref()?).join(TRASH_DIR).join(id))
}

/// Trash folders younger than this are never removed by `clean_trash`, known or not
pub const TRASH_GRACE: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// Removes trash folders the index doesn't know (deleted for good while removing them from disk failed, say). Recent
/// ones stay: something that went wrong halfway may still need them.
pub async fn clean_trash(st: &AppState, drive_id: &str, root: &Path) -> AppResult<()> {
    let dir = root.join(TRASH_DIR);
    let Ok(read) = std::fs::read_dir(&dir) else { return Ok(()) };
    let old = |e: &std::fs::DirEntry| older_than(e.path().as_path(), TRASH_GRACE);
    let names: Vec<String> = read.flatten().filter(old).filter_map(|e| e.file_name().into_string().ok()).collect();
    if names.is_empty() {
        return Ok(());
    }
    let known: HashSet<String> = sqlx::query_as::<_, (String,)>("SELECT DISTINCT trash_id FROM nodes WHERE drive_id = ? AND trash_id IS NOT NULL")
        .bind(drive_id)
        .fetch_all(&st.db)
        .await?
        .into_iter()
        .map(|(t,)| t)
        .collect();
    remove_later(names.into_iter().filter(|n| !known.contains(n)).map(|n| dir.join(n)).collect());
    Ok(())
}

/// Whether a name scans ignore is something a change left behind when it stopped halfway (a restart, say)
pub fn is_leftover(name: &str) -> bool {
    [MOVE_PREFIX, COPY_PREFIX, UPLOAD_PREFIX, SAVE_PREFIX].iter().any(|p| name.starts_with(p))
}

/// Deals with what changes left behind (`is_leftover`, paths found by a scan) once they are old enough that no change
/// can still be using them: an item that was being moved in is put back under its own name, where the next scan
/// shows it; half-made copies, uploads and saves are removed. `age`: how old they must be.
pub fn clean_leftovers(paths: Vec<PathBuf>, age: std::time::Duration) {
    for p in paths.into_iter().filter(|p| older_than(p, age)) {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if name.starts_with(MOVE_PREFIX) && p.is_dir() {
            let dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
            for item in std::fs::read_dir(&p).into_iter().flatten().flatten() {
                let item_name = item.file_name().to_string_lossy().into_owned();
                let is_dir = item.file_type().is_ok_and(|t| t.is_dir());
                let back = (0..10_000u32).map(|n| if n == 0 { item_name.clone() } else { numbered_name(&item_name, n, is_dir) }).find_map(|candidate| {
                    match rename_new(&item.path(), &dir.join(&candidate)) {
                        Ok(()) => Some(Ok(candidate)),
                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => None,
                        Err(e) => Some(Err(e)),
                    }
                });
                match back {
                    Some(Ok(n)) => tracing::warn!("Put back {} in {}: it was being moved when ThirtyFile stopped", n, dir.display()),
                    Some(Err(e)) => tracing::warn!("Couldn't put back {}: {e}", item.path().display()),
                    None => {}
                }
            }
            let _ = std::fs::remove_dir(&p);
        } else if let Err(e) = remove_all(&p)
            && e.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!("Couldn't remove {} from disk: {e}", p.display());
        }
    }
}

// ───────────── Uploads and saving ─────────────

const UPLOAD_PREFIX: &str = ".thirtyfile-upload-";
const SAVE_PREFIX: &str = ".thirtyfile-save-";

/// Puts a finished upload into the space's folder under a name scans ignore, ready to be renamed into place: a
/// rename when the folder is on the same disk as ThirtyFile's data, else a copy
pub async fn stage_upload(folder: &Node, tmp: &Path, size: u64) -> AppResult<PathBuf> {
    let root = PathBuf::from(folder.fs_root.as_deref().unwrap_or_default());
    let staged = root.join(format!("{UPLOAD_PREFIX}{}", new_id()));
    if tokio::fs::rename(tmp, &staged).await.is_err() {
        match tokio::fs::copy(tmp, &staged).await {
            Ok(n) if n == size => {
                let _ = tokio::fs::remove_file(tmp).await;
            }
            copied => {
                let _ = tokio::fs::remove_file(&staged).await;
                return Err(copied.err().map(disk_error).unwrap_or_else(incomplete));
            }
        }
    }
    Ok(staged)
}

/// Renames content staged in the space's folder into `folder` as `name` and indexes it; returns the new item's id
pub async fn place_file(conn: &mut SqliteConnection, staged: &Path, owner: i64, folder: &Node, name: &str) -> AppResult<String> {
    let to = abs(folder)?.join(name);
    rename_new(staged, &to).map_err(disk_error)?;
    let s = stat(&to).map_err(disk_error)?;
    let id = new_id();
    insert(conn, &id, owner, folder, name, &child_rel(rel_of(folder), name), &s).await?;
    Ok(id)
}

/// Renames content staged in the space's folder over an existing file, which keeps its id, and indexes it (the caller
/// counts the change in size). The file it had is kept as an earlier version first; returns what versions no longer
/// kept leave to remove after the commit.
pub async fn replace_file(conn: &mut SqliteConnection, policy: versions::Policy, staged: &Path, existing: &Node, by: i64) -> AppResult<versions::Removed> {
    let to = abs(existing)?;
    // The new content keeps the file's permissions
    if let Ok(m) = std::fs::metadata(&to) {
        let _ = std::fs::set_permissions(staged, m.permissions());
    }
    let removed = versions::keep_file(conn, policy, existing, &to).await?;
    std::fs::rename(staged, &to).map_err(disk_error)?;
    let s = stat(&to).map_err(disk_error)?;
    record(conn, &existing.id, existing.drive(), rel_of(existing), &s).await?;
    sqlx::query("UPDATE nodes SET updated_at = ?, content_by = ? WHERE id = ?")
        .bind(now().max(existing.updated_at + 1))
        .bind(by)
        .bind(&existing.id)
        .execute(conn)
        .await?;
    Ok(removed)
}

/// Saves from the online editor into a folder space. A file changed on the server since it was indexed (or removed
/// there) isn't overwritten: the new content is saved next to it as "name (conflict copy)" and the save reports a
/// conflict.
pub async fn save(st: &AppState, user: &User, id: &str, body: &[u8], base: Option<i64>, conflict: fn() -> AppError) -> AppResult<Node> {
    let drive = tree::get_node(&mut *st.db.acquire().await?, id).await?.map(|n| n.drive().to_string()).unwrap_or_default();
    let _space = lock_space(&drive).await;
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    let node = tree::node_for(&mut tx, user, id, Need::Write).await?;
    if node.drive() != drive {
        return Err(AppError::conflict("Something changed at the same time. Try again."));
    }
    if base.is_some_and(|b| b != node.updated_at) {
        return Err(conflict());
    }
    let path = abs(&node)?;
    let dir = path.parent().map(Path::to_path_buf).ok_or_else(|| AppError::not_found("Item not found"))?;
    let (fs_size, fs_mtime, fs_ino): (Option<i64>, Option<i64>, Option<i64>) =
        sqlx::query_as("SELECT fs_size, fs_mtime_ns, fs_ino FROM nodes WHERE id = ?").bind(&node.id).fetch_one(&mut *tx).await?;
    let unchanged = stat(&path).is_ok_and(|s| Some(s.size) == fs_size && Some(s.mtime_ns) == fs_mtime && Some(s.ino) == fs_ino);

    let tmp = dir.join(format!("{SAVE_PREFIX}{}", new_id()));
    let written = std::fs::write(&tmp, body).and_then(|()| match std::fs::metadata(&path) {
        // The new content keeps the file's permissions
        Ok(m) => std::fs::set_permissions(&tmp, m.permissions()),
        Err(_) => Ok(()),
    });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(disk_error(e));
    }

    if !unchanged {
        let parent = tree::get_node(&mut tx, node.parent_id.as_deref().unwrap_or_default()).await?.ok_or_else(|| AppError::not_found("Folder not found"))?;
        let (stem, ext) = split_name(&node.name, false);
        let placed = async {
            let name = free_name(&mut tx, &parent, &format!("{stem} (conflict copy){ext}"), false).await?;
            let copy_id = place_file(&mut tx, &tmp, user.id, &parent, &name).await?;
            Ok::<_, AppError>((name, copy_id))
        }
        .await;
        let (name, copy_id) = match placed {
            Ok(p) => p,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        };
        if let Some(copy) = tree::get_node(&mut tx, &copy_id).await? {
            tree::adjust_usage(&mut tx, copy.drive(), copy.size).await?;
            logs::record_activity(&mut tx, user, Some(&copy), "upload", "").await?;
        }
        tx.commit().await?;
        return Err(AppError::new(
            StatusCode::CONFLICT,
            format!("The file was changed on the server while you were editing it. Your version was saved as \"{name}\"."),
        )
        .with_code("conflict_copy"));
    }

    // The content it had is kept as an earlier version
    let removed = match versions::keep_file(&mut tx, versions::Policy::of(st), &node, &path).await {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(disk_error(e));
    }
    let s = stat(&path).map_err(disk_error)?;
    record(&mut tx, &node.id, node.drive(), rel_of(&node), &s).await?;
    sqlx::query("UPDATE nodes SET updated_at = ?, content_by = ? WHERE id = ?")
        .bind(now().max(node.updated_at + 1))
        .bind(user.id)
        .bind(&node.id)
        .execute(&mut *tx)
        .await?;
    tree::adjust_usage(&mut tx, node.drive(), s.size - node.size).await?;
    logs::record_activity(&mut tx, user, Some(&node), "edit", "").await?;
    let node = tree::get_node(&mut tx, &node.id).await?.ok_or_else(|| AppError::not_found("Item not found"))?;
    tx.commit().await?;
    removed.finish(st);
    Ok(node)
}

// ───────────── Moving and copying between spaces ─────────────

/// Content of items being moved or copied to another space, put in place before the index changes
enum Placed {
    /// In the destination folder on disk, as `tmp` inside a folder `wrap` that scans ignore (`.thirtyfile-move-…`, so
    /// that after a restart in between, the item is still found there under its name); `renamed_from`: the original
    /// was renamed there (same disk), so undoing renames it back; `copied`: the originals of a move that copied them
    /// (another disk), removed once the index follows
    Disk { wrap: PathBuf, tmp: PathBuf, renamed_from: Option<PathBuf>, copied: Vec<Copied> },
    /// In the destination's content store: one staged content per file (by node id)
    Store(HashMap<String, StagedBlob>),
}

/// Each item's path relative to the first one (the item being moved or copied); the list is ordered by depth
fn layout(nodes: &[Node]) -> Vec<Option<String>> {
    let mut rels: HashMap<&str, String> = HashMap::new();
    let mut out = Vec::with_capacity(nodes.len());
    for (i, n) in nodes.iter().enumerate() {
        let rel = if i == 0 { Some(String::new()) } else { n.parent_id.as_deref().and_then(|p| rels.get(p)).map(|p| child_rel(p, &n.name)) };
        if let Some(r) = &rel {
            rels.insert(n.id.as_str(), r.clone());
        }
        out.push(rel);
    }
    out
}

/// Writes the items to `top` on disk, from wherever their content is
async fn write_tree(st: &AppState, nodes: &[Node], top: &Path) -> AppResult<()> {
    for (n, rel) in nodes.iter().zip(layout(nodes)) {
        let Some(rel) = rel else { continue };
        let to = under(top, &rel);
        if n.is_folder() {
            tokio::fs::create_dir(&to).await.map_err(disk_error)?;
            continue;
        }
        match Source::of(n)? {
            Source::File(from) => {
                // On btrfs and XFS the copy can share the original's blocks
                let want = tokio::fs::metadata(&from).await.map_err(disk_error)?.len();
                if tokio::fs::copy(&from, &to).await.map_err(disk_error)? != want {
                    return Err(incomplete());
                }
            }
            source => {
                let mut reader = source
                    .open(st, 0, n.size as u64)
                    .await
                    .map_err(|e| AppError::new(StatusCode::BAD_GATEWAY, format!("Couldn't read \"{}\": {e}", n.name)))?;
                let mut file = tokio::fs::File::create(&to).await.map_err(disk_error)?;
                let got = tokio::io::copy(&mut reader, &mut file).await.map_err(disk_error)?;
                file.sync_all().await.map_err(disk_error)?;
                if got != n.size as u64 {
                    return Err(incomplete());
                }
            }
        }
    }
    Ok(())
}

/// Stores the files of a folder space in the content store of the space `drive`
async fn ingest(st: &AppState, nodes: &[Node], drive: &str) -> AppResult<HashMap<String, StagedBlob>> {
    let mut staged = HashMap::new();
    for n in nodes.iter().filter(|n| !n.is_folder()) {
        let tmp = st.tmp_dir().join(new_id());
        let stored = async {
            tokio::fs::copy(abs(n)?, &tmp).await.map_err(disk_error)?;
            let (hash, size) = crate::files::hash_file(tmp.clone()).await?;
            tree::stage_blob(st, drive, hash, size as i64, tmp.clone()).await
        }
        .await;
        match stored {
            Ok(s) => {
                staged.insert(n.id.clone(), s);
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                for (_, s) in staged {
                    tree::abandon_staged(st, s).await;
                }
                return Err(e);
            }
        }
    }
    Ok(staged)
}

/// Puts the content of `nodes` (an item and everything in it) into place for `dest`
async fn place(st: &AppState, nodes: &[Node], dest: &Node, moving: bool) -> AppResult<Placed> {
    let top = &nodes[0];
    if !dest.in_folder_space() {
        return Ok(Placed::Store(ingest(st, nodes, dest.drive()).await?));
    }
    let wrap = abs(dest)?.join(format!("{}{}", if moving { MOVE_PREFIX } else { COPY_PREFIX }, new_id()));
    tokio::fs::create_dir(&wrap).await.map_err(disk_error)?;
    let tmp = wrap.join(&top.name);
    if moving && top.in_folder_space() {
        let from = abs(top)?;
        let renamed = if other_disk() { Err(io::ErrorKind::CrossesDevices.into()) } else { std::fs::rename(&from, &tmp) };
        match renamed {
            Ok(()) => return Ok(Placed::Disk { wrap, tmp, renamed_from: Some(from), copied: Vec::new() }),
            Err(e) if e.kind() != io::ErrorKind::CrossesDevices => {
                let _ = std::fs::remove_dir(&wrap);
                return Err(disk_error(e));
            }
            Err(_) => {}
        }
        // Another disk: copy everything, including what the index doesn't have yet, before the original goes
        let (f, t) = (from.clone(), tmp.clone());
        match tokio::task::spawn_blocking(move || {
            let mut copied = Vec::new();
            copy_tree(&f, &t, &mut copied).map(|()| copied)
        })
        .await?
        {
            Ok(copied) => return Ok(Placed::Disk { wrap, tmp, renamed_from: None, copied }),
            Err(e) => {
                remove_later(vec![wrap]);
                return Err(disk_error(e));
            }
        }
    }
    if let Err(e) = write_tree(st, nodes, &tmp).await {
        remove_later(vec![wrap]);
        return Err(e);
    }
    Ok(Placed::Disk { wrap, tmp, renamed_from: None, copied: Vec::new() })
}

/// Folders holding an item on its way into a folder (`Placed::Disk`)
pub const MOVE_PREFIX: &str = ".thirtyfile-move-";
const COPY_PREFIX: &str = ".thirtyfile-copy-";

#[cfg(test)]
thread_local! {
    /// Tests: treat every folder as being on another disk, so moves copy
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

/// Takes back content put in place when the index couldn't follow
async fn undo(st: &AppState, placed: Placed) {
    match placed {
        Placed::Disk { wrap, tmp, renamed_from: Some(from), .. } => {
            if std::fs::rename(&tmp, &from).is_err() && tmp.exists() {
                tracing::warn!("Couldn't move {} back to {}", tmp.display(), from.display());
            } else {
                let _ = std::fs::remove_dir(&wrap);
            }
        }
        Placed::Disk { wrap, renamed_from: None, .. } => remove_later(vec![wrap]),
        Placed::Store(staged) => {
            for (_, s) in staged {
                tree::abandon_staged(st, s).await;
            }
        }
    }
}

/// After the index changed: staged content is kept, content no longer used goes
async fn finish(st: &AppState, placed: Placed, extras: Vec<BlobRef>) {
    if let Placed::Store(staged) = placed {
        for (_, s) in staged {
            tree::finish_staged(st, s, None).await;
        }
    }
    tree::schedule_blob_removal(st, extras);
}

/// Names for the items going into the content store, where letter case doesn't tell names apart: "A.txt" and "a.txt"
/// from a folder can't both keep theirs. The first item's name is `top_name`.
fn store_names(nodes: &[Node], top_name: &str) -> Vec<String> {
    let mut taken: HashMap<&str, HashSet<String>> = HashMap::new();
    nodes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            if i == 0 {
                return top_name.to_string();
            }
            let set = taken.entry(n.parent_id.as_deref().unwrap_or_default()).or_default();
            let mut name = n.name.clone();
            let mut k = 1;
            while !set.insert(name.to_lowercase()) {
                name = numbered_name(&n.name, k, n.is_folder());
                k += 1;
            }
            name
        })
        .collect()
}

fn id_list<'a>(ids: impl Iterator<Item = &'a str>) -> String {
    serde_json::to_string(&ids.collect::<Vec<_>>()).unwrap()
}

/// Moves items to a folder of another space, where one of the two (or both) is a folder space. The content is put in
/// place first (renamed when both folders are on the same disk, else copied); then the index moves the items, which
/// keep their ids and with them their shares, permissions and favourites; then the originals are removed.
/// `items`: each item with everything in it (not in the trash), ordered by depth.
pub async fn move_across(st: &AppState, user: &User, dest: &Node, items: Vec<Vec<Node>>) -> AppResult<()> {
    for nodes in items {
        let mut placed = place(st, &nodes, dest, true).await?;
        let result = {
            let _w = st.write_lock.lock().await;
            commit_move(st, user, dest, &nodes, &placed).await
        };
        match result {
            Ok((extras, remove)) => {
                let copied = match &mut placed {
                    Placed::Disk { copied, .. } => std::mem::take(copied),
                    Placed::Store(_) => Vec::new(),
                };
                finish(st, placed, extras).await;
                if !copied.is_empty() {
                    tokio::task::spawn_blocking(move || remove_copied(copied));
                }
                if !remove.is_empty() {
                    tokio::task::spawn_blocking(move || remove_indexed(remove));
                }
            }
            Err(e) => {
                undo(st, placed).await;
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Removes the files that were stored elsewhere, then their folders if nothing else is left in them (an item the
/// index didn't have yet stays, and the next scan shows it)
fn remove_indexed(paths: Vec<(PathBuf, bool)>) {
    for (p, _) in paths.iter().filter(|(_, d)| !d) {
        if let Err(e) = std::fs::remove_file(p)
            && e.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!("Couldn't remove {} from disk: {e}", p.display());
        }
    }
    for (p, _) in paths.iter().rev().filter(|(_, d)| *d) {
        let _ = std::fs::remove_dir(p);
    }
}

/// The index side of a move; returns content no longer used and (for items now in the content store) what to remove
/// from disk
async fn commit_move(st: &AppState, user: &User, dest: &Node, nodes: &[Node], placed: &Placed) -> AppResult<(Vec<BlobRef>, Vec<(PathBuf, bool)>)> {
    let mut tx = st.db.begin().await?;
    let top = &nodes[0];
    // Everything must still be as it was when the content was copied: items added to a folder of the content store
    // meanwhile would be left behind
    let changed = || AppError::conflict(format!("\"{}\" changed while it was being moved. Try again.", top.name));
    let current = tree::get_node(&mut tx, &top.id).await?.ok_or_else(changed)?;
    if current.trashed_at.is_some() || current.parent_id != top.parent_id || current.drive_id != top.drive_id {
        return Err(changed());
    }
    if tree::get_node(&mut tx, &dest.id).await?.is_none_or(|d| d.trashed_at.is_some()) {
        return Err(AppError::not_found("The destination folder no longer exists"));
    }
    let planned: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let now_there: Vec<String> = tree::subtree(&mut tx, &top.id).await?.into_iter().filter(|(n, _)| n.trashed_at.is_none()).map(|(n, _)| n.id).collect();
    if now_there.len() != planned.len() || !now_there.iter().all(|id| planned.contains(id.as_str())) {
        return Err(changed());
    }
    let ids = id_list(nodes.iter().map(|n| n.id.as_str()));
    let src_drive = top.drive().to_string();
    let src_root = tree::get_drive(&mut tx, &src_drive).await?.map(|d| d.root_id).unwrap_or_default();
    // Items in the trash stay in the trash of their space
    sqlx::query("UPDATE nodes SET parent_id = ? WHERE trash_root = 1 AND parent_id IN (SELECT value FROM json_each(?))")
        .bind(&src_root)
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    let bytes: i64 = nodes.iter().filter(|n| !n.is_folder()).map(|n| n.size).sum();
    let mut extras = Vec::new();
    let mut remove = Vec::new();
    match placed {
        Placed::Disk { wrap, tmp, .. } => {
            let final_path = abs(dest)?.join(&top.name);
            rename_new(tmp, &final_path).map_err(disk_error)?;
            let _ = std::fs::remove_dir(wrap);
            let dest_rel = child_rel(rel_of(dest), &top.name);
            for (n, rel) in nodes.iter().zip(layout(nodes)) {
                let Some(rel) = rel else { continue };
                let full = if rel.is_empty() { dest_rel.clone() } else { format!("{dest_rel}/{rel}") };
                match stat(&under(&final_path, &rel)) {
                    Ok(s) => record(&mut tx, &n.id, dest.drive(), &full, &s).await?,
                    // Gone from the server before it could be moved: gone from the index too
                    Err(_) => {
                        tree::purge_subtree(&mut tx, &n.id).await?;
                    }
                }
            }
            // Content that was in the content store is in the folder now
            let hashes: Vec<String> = nodes.iter().filter_map(|n| n.blob_hash.clone()).collect();
            sqlx::query("UPDATE nodes SET blob_hash = NULL WHERE id IN (SELECT value FROM json_each(?))").bind(&ids).execute(&mut *tx).await?;
            extras.extend(tree::release_blobs(&mut tx, &hashes).await?);
        }
        Placed::Store(staged) => {
            let names = store_names(nodes, &top.name);
            // Leave the names the folder gave them first, so renamed items can't run into each other on the way
            sqlx::query("UPDATE nodes SET name = char(1) || id WHERE id IN (SELECT value FROM json_each(?))").bind(&ids).execute(&mut *tx).await?;
            sqlx::query(
                "UPDATE nodes SET drive_id = ?, fs_path = NULL, fs_dev = NULL, fs_ino = NULL, fs_size = NULL, fs_mtime_ns = NULL
                 WHERE id IN (SELECT value FROM json_each(?))",
            )
            .bind(dest.drive())
            .bind(&ids)
            .execute(&mut *tx)
            .await?;
            for (n, name) in nodes.iter().zip(&names) {
                sqlx::query("UPDATE nodes SET name = ? WHERE id = ?").bind(name).bind(&n.id).execute(&mut *tx).await?;
                if let Some(s) = staged.get(&n.id) {
                    extras.extend(tree::commit_blob(st, &mut tx, s).await?);
                    sqlx::query("UPDATE nodes SET blob_hash = ?, size = ? WHERE id = ?").bind(&s.hash).bind(s.size).bind(&n.id).execute(&mut *tx).await?;
                }
                if let Ok(p) = abs(n) {
                    remove.push((p, n.is_folder()));
                }
            }
        }
    }
    sqlx::query("UPDATE nodes SET parent_id = ? WHERE id = ?").bind(&dest.id).bind(&top.id).execute(&mut *tx).await?;
    tree::adjust_usage(&mut tx, &src_drive, -bytes).await?;
    tree::adjust_usage(&mut tx, dest.drive(), bytes).await?;
    if let Some(p) = &top.parent_id {
        tree::touch(&mut tx, p).await?;
    }
    tree::touch(&mut tx, &dest.id).await?;
    logs::record_activity(&mut tx, user, Some(top), "move", &format!("→ {}", if dest.parent_id.is_none() { "Root folder" } else { &dest.name })).await?;
    tx.commit().await?;
    Ok((extras, remove))
}

/// Copies items to a folder of another space (or of the same folder space), where one of the two (or both) is a folder
/// space: the content is written first, then indexed. `plans`: each item with everything in it (not in the trash),
/// ordered by depth.
pub async fn copy_across(st: &AppState, user: &User, dest: &Node, plans: Vec<Vec<Node>>) -> AppResult<()> {
    for nodes in plans {
        let placed = place(st, &nodes, dest, false).await?;
        let result = {
            let _w = st.write_lock.lock().await;
            commit_copy(st, user, dest, &nodes, &placed).await
        };
        match result {
            Ok(extras) => finish(st, placed, extras).await,
            Err(e) => {
                undo(st, placed).await;
                return Err(e);
            }
        }
    }
    Ok(())
}

async fn commit_copy(st: &AppState, user: &User, dest: &Node, nodes: &[Node], placed: &Placed) -> AppResult<Vec<BlobRef>> {
    let mut tx = st.db.begin().await?;
    let top = &nodes[0];
    if tree::get_node(&mut tx, &dest.id).await?.is_none_or(|d| d.trashed_at.is_some()) {
        return Err(AppError::not_found("The destination folder no longer exists"));
    }
    let mut ids: HashMap<&str, String> = HashMap::new();
    let mut extras = Vec::new();
    let mut bytes = 0i64;
    match placed {
        Placed::Disk { wrap, tmp, .. } => {
            let name = free_name(&mut tx, dest, &top.name, top.is_folder()).await?;
            let final_path = abs(dest)?.join(&name);
            rename_new(tmp, &final_path).map_err(disk_error)?;
            let _ = std::fs::remove_dir(wrap);
            let dest_rel = child_rel(rel_of(dest), &name);
            for (i, (n, rel)) in nodes.iter().zip(layout(nodes)).enumerate() {
                let Some(rel) = rel else { continue };
                let parent = if i == 0 { dest.id.clone() } else { ids.get(n.parent_id.as_deref().unwrap_or_default()).cloned().unwrap_or_default() };
                let Ok(s) = stat(&under(&final_path, &rel)) else { continue };
                if parent.is_empty() {
                    continue;
                }
                let id = new_id();
                let full = if rel.is_empty() { dest_rel.clone() } else { format!("{dest_rel}/{rel}") };
                insert_at(&mut tx, &id, user.id, &parent, dest.drive(), if i == 0 { &name } else { &n.name }, &full, &s).await?;
                bytes += s.size;
                ids.insert(n.id.as_str(), id);
            }
        }
        Placed::Store(staged) => {
            let name = tree::unique_name(&mut tx, &dest.id, &top.name, top.is_folder()).await?;
            let names = store_names(nodes, &name);
            let ts = now();
            for (i, (n, name)) in nodes.iter().zip(&names).enumerate() {
                let parent = if i == 0 { dest.id.clone() } else { ids.get(n.parent_id.as_deref().unwrap_or_default()).cloned().unwrap_or_default() };
                if parent.is_empty() {
                    continue;
                }
                let s = staged.get(&n.id);
                if let Some(s) = s {
                    extras.extend(tree::commit_blob(st, &mut tx, s).await?);
                    bytes += s.size;
                }
                let id = new_id();
                sqlx::query(
                    "INSERT INTO nodes (id, owner_id, parent_id, kind, name, blob_hash, size, mime, drive_id, created_at, updated_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&id)
                .bind(user.id)
                .bind(&parent)
                .bind(&n.kind)
                .bind(name)
                .bind(s.map(|s| s.hash.clone()))
                .bind(s.map(|s| s.size).unwrap_or(0))
                .bind(&n.mime)
                .bind(dest.drive())
                .bind(ts)
                .bind(ts)
                .execute(&mut *tx)
                .await?;
                ids.insert(n.id.as_str(), id);
            }
        }
    }
    tree::adjust_usage(&mut tx, dest.drive(), bytes).await?;
    tree::touch(&mut tx, &dest.id).await?;
    logs::record_activity(&mut tx, user, Some(top), "copy", &format!("→ {}", if dest.parent_id.is_none() { "Root folder" } else { &dest.name })).await?;
    tx.commit().await?;
    Ok(extras)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{self, write_old};
    use axum::{
        Json,
        body::Bytes,
        extract::{Path as UrlPath, Query, State},
        http::HeaderMap,
    };
    use serde_json::json;

    fn req<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> Json<T> {
        Json(serde_json::from_value(v).unwrap())
    }

    async fn node(env: &testutil::TestEnv, id: &str) -> Node {
        tree::get_node(&mut env.st.db.acquire().await.unwrap(), id).await.unwrap().unwrap()
    }

    /// Removing from disk happens in the background
    async fn eventually_gone(path: &Path) -> bool {
        for _ in 0..100 {
            if !path.exists() {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        false
    }

    async fn content(env: &testutil::TestEnv, user: &User, id: &str) -> Vec<u8> {
        let q = Query(serde_json::from_value(json!({})).unwrap());
        let res = crate::files::content(State(env.st.clone()), user.clone(), UrlPath(id.to_string()), q, HeaderMap::new()).await.unwrap();
        axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec()
    }

    #[tokio::test]
    async fn changes_from_the_web_are_made_in_the_folder() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        let (dir, drive) = (&space.dir, &space.drive);
        let st = || State(env.st.clone());

        let Json(docs) = crate::nodes::create_folder(st(), admin.clone(), req(json!({ "parent_id": space.root, "name": "Docs" }))).await.unwrap();
        assert!(dir.join("Docs").is_dir());
        write_old(&dir.join("a.txt"), b"one");
        crate::folders::scan(&env.st, drive).await.unwrap();
        let (a, _) = env.node_at(drive, "a.txt").await.unwrap();

        // Renamed on disk, and the index follows
        let _ = crate::nodes::rename(st(), admin.clone(), UrlPath(a.clone()), req(json!({ "name": "b.txt" }))).await.unwrap();
        assert!(dir.join("b.txt").is_file() && !dir.join("a.txt").exists());
        assert_eq!(env.node_at(drive, "b.txt").await.unwrap().0, a);

        // A rename onto an item the index doesn't have yet fails, and changes nothing
        write_old(&dir.join("taken.txt"), b"not indexed yet");
        let err = crate::nodes::rename(st(), admin.clone(), UrlPath(a.clone()), req(json!({ "name": "taken.txt" }))).await.unwrap_err();
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert_eq!(std::fs::read(dir.join("taken.txt")).unwrap(), b"not indexed yet");
        assert_eq!(node(&env, &a).await.name, "b.txt");
        assert!(dir.join("b.txt").is_file());

        // Moved into a folder
        let _ = crate::nodes::move_nodes(st(), admin.clone(), req(json!({ "ids": [a], "dest_id": docs.id }))).await.unwrap();
        assert!(dir.join("Docs/b.txt").is_file());
        assert_eq!(env.node_at(drive, "Docs/b.txt").await.unwrap().0, a);
        let r = crate::folders::scan(&env.st, drive).await.unwrap();
        assert_eq!((r.changed, r.moved, r.removed), (0, 0, 0), "the index already knows: {r:?}");

        // The trash is a folder in the space; restoring puts the item back
        let _ = crate::nodes::trash(st(), admin.clone(), req(json!({ "ids": [docs.id] }))).await.unwrap();
        assert!(!dir.join("Docs").exists());
        let in_trash = node(&env, &a).await.fs_file().unwrap();
        assert!(in_trash.starts_with(dir.join(TRASH_DIR)) && in_trash.is_file(), "{}", in_trash.display());
        let r = crate::folders::scan(&env.st, drive).await.unwrap();
        assert_eq!((r.added, r.removed), (0, 0), "the trash isn't indexed: {r:?}");
        let _ = crate::nodes::restore(st(), admin.clone(), req(json!({ "ids": [docs.id] }))).await.unwrap();
        assert!(dir.join("Docs/b.txt").is_file());
        assert_eq!(env.node_at(drive, "Docs/b.txt").await.unwrap().0, a);

        // Deleted for good: gone from disk too
        let _ = crate::nodes::trash(st(), admin.clone(), req(json!({ "ids": [a] }))).await.unwrap();
        let folder = trash_folder(&node(&env, &a).await).unwrap();
        assert!(folder.is_dir());
        let _ = crate::nodes::delete_forever(st(), admin.clone(), req(json!({ "ids": [a] }))).await.unwrap();
        assert!(eventually_gone(&folder).await);
    }

    #[tokio::test]
    async fn a_file_changed_on_the_server_while_editing_is_kept_and_the_edit_saved_as_a_copy() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        write_old(&space.dir.join("notes.txt"), b"one");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (id, _) = env.node_at(&space.drive, "notes.txt").await.unwrap();
        let save = |body: &'static [u8], base: i64| {
            let mut h = HeaderMap::new();
            h.insert("x-base-version", base.to_string().parse().unwrap());
            crate::files::save_content(State(env.st.clone()), admin.clone(), UrlPath(id.clone()), h, Bytes::from_static(body))
        };

        let Json(saved) = save(b"two", node(&env, &id).await.updated_at).await.unwrap();
        assert_eq!(std::fs::read(space.dir.join("notes.txt")).unwrap(), b"two");
        assert_eq!(saved.size, 3);

        // Changed on the server (over SMB, say) while someone edits it here
        write_old(&space.dir.join("notes.txt"), b"changed on the server");
        let err = save(b"three", saved.updated_at).await.unwrap_err();
        assert_eq!((err.status, err.code), (StatusCode::CONFLICT, Some("conflict_copy")), "{}", err.message);
        assert_eq!(std::fs::read(space.dir.join("notes.txt")).unwrap(), b"changed on the server");
        assert_eq!(std::fs::read(space.dir.join("notes (conflict copy).txt")).unwrap(), b"three");
        assert!(env.node_at(&space.drive, "notes (conflict copy).txt").await.is_some());
    }

    #[tokio::test]
    async fn an_earlier_version_stays_as_it_was_when_the_file_is_written_in_place() {
        use std::io::Write;
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        write_old(&space.dir.join("notes.txt"), b"one");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (id, _) = env.node_at(&space.drive, "notes.txt").await.unwrap();
        let _ = crate::files::save_content(State(env.st.clone()), admin.clone(), UrlPath(id.clone()), HeaderMap::new(), Bytes::from_static(b"two"))
            .await
            .unwrap();
        // The version keeps the file that was replaced (a hard link to it): the file's name now has new content, so
        // a program writing to the file in place (over SMB, say) changes only that
        std::fs::OpenOptions::new().append(true).open(space.dir.join("notes.txt")).unwrap().write_all(b" and more").unwrap();
        let kept: Vec<Vec<u8>> = std::fs::read_dir(space.dir.join(versions::VERSIONS_DIR).join(&id))
            .unwrap()
            .flatten()
            .map(|e| std::fs::read(e.path()).unwrap())
            .collect();
        assert_eq!(kept, [b"one".to_vec()]);
    }

    #[tokio::test]
    async fn items_move_and_copy_between_folder_spaces_and_the_content_store() {
        let env = testutil::env().await;
        let one = env.folder_space("One").await;
        let two = env.folder_space("Two").await;
        let admin = env.admin().await;
        let st = || State(env.st.clone());
        write_old(&one.dir.join("Sub/x.txt"), b"x");
        write_old(&one.dir.join("Sub/Inner/y.txt"), b"yy");
        crate::folders::scan(&env.st, &one.drive).await.unwrap();
        let (sub, _) = env.node_at(&one.drive, "Sub").await.unwrap();
        let (x, _) = env.node_at(&one.drive, "Sub/x.txt").await.unwrap();

        // Copied to another folder space
        let _ = crate::nodes::copy_nodes(st(), admin.clone(), req(json!({ "ids": [sub], "dest_id": two.root }))).await.unwrap();
        assert_eq!(std::fs::read(two.dir.join("Sub/Inner/y.txt")).unwrap(), b"yy");
        assert!(env.node_at(&two.drive, "Sub/Inner/y.txt").await.is_some());

        // Moved to a folder space on another disk: copied, checked, then the original goes; the items keep their ids
        let Json(dest) = crate::nodes::create_folder(st(), admin.clone(), req(json!({ "parent_id": two.root, "name": "Dest" }))).await.unwrap();
        OTHER_DISK.with(|d| d.set(true));
        let moved = crate::nodes::move_nodes(st(), admin.clone(), req(json!({ "ids": [sub], "dest_id": dest.id }))).await;
        OTHER_DISK.with(|d| d.set(false));
        let _ = moved.unwrap();
        assert_eq!(std::fs::read(two.dir.join("Dest/Sub/x.txt")).unwrap(), b"x");
        assert!(eventually_gone(&one.dir.join("Sub")).await);
        assert_eq!(env.node_at(&two.drive, "Dest/Sub/x.txt").await.unwrap().0, x);
        assert_eq!(env.drive_of(&x).await, two.drive);

        // Into the content store: stored, then removed from the folder
        let _ = crate::nodes::move_nodes(st(), admin.clone(), req(json!({ "ids": [sub], "dest_id": admin.root_id }))).await.unwrap();
        let n = node(&env, &x).await;
        assert!(n.blob_hash.is_some() && n.fs_path.is_none());
        assert_eq!(content(&env, &admin, &x).await, b"x");
        assert!(eventually_gone(&two.dir.join("Dest/Sub/x.txt")).await);

        // And back out of it into a folder space: written to the folder, the stored content released
        let _ = crate::nodes::move_nodes(st(), admin.clone(), req(json!({ "ids": [sub], "dest_id": one.root }))).await.unwrap();
        assert_eq!(std::fs::read(one.dir.join("Sub/x.txt")).unwrap(), b"x");
        assert_eq!(std::fs::read(one.dir.join("Sub/Inner/y.txt")).unwrap(), b"yy");
        let n = node(&env, &x).await;
        assert!(n.blob_hash.is_none() && n.fs_path.as_deref() == Some("Sub/x.txt"));
        let (blobs,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM blobs").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(blobs, 0);

        // A copy from a folder space into the content store
        let _ = crate::nodes::copy_nodes(st(), admin.clone(), req(json!({ "ids": [x], "dest_id": admin.root_id }))).await.unwrap();
        let (copy,): (String,) =
            sqlx::query_as("SELECT id FROM nodes WHERE parent_id = ? AND name = 'x.txt'").bind(&admin.root_id).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(content(&env, &admin, &copy).await, b"x");
        assert!(one.dir.join("Sub/x.txt").is_file(), "copying leaves the original");
    }

    #[tokio::test]
    async fn a_change_of_several_items_that_fails_halfway_puts_the_first_ones_back() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        let (dir, drive) = (&space.dir, &space.drive);
        let st = || State(env.st.clone());
        write_old(&dir.join("a.txt"), b"a");
        write_old(&dir.join("b.txt"), b"b");
        crate::folders::scan(&env.st, drive).await.unwrap();
        let (a, _) = env.node_at(drive, "a.txt").await.unwrap();
        let (b, _) = env.node_at(drive, "b.txt").await.unwrap();
        let Json(docs) = crate::nodes::create_folder(st(), admin.clone(), req(json!({ "parent_id": space.root, "name": "Docs" }))).await.unwrap();
        let unchanged = || async {
            assert!(dir.join("a.txt").is_file() && dir.join("b.txt").is_file());
            assert_eq!(env.node_at(drive, "a.txt").await.unwrap().0, a);
            assert!(node(&env, &a).await.trashed_at.is_none());
            let r = crate::folders::scan(&env.st, drive).await.unwrap();
            assert_eq!((r.added, r.moved, r.removed), (0, 0, 0), "{r:?}");
        };

        // The space's root folder can't be deleted or moved: the first item was already changed on disk by then
        assert!(crate::nodes::trash(st(), admin.clone(), req(json!({ "ids": [a, space.root] }))).await.is_err());
        unchanged().await;
        assert_eq!(std::fs::read_dir(dir.join(TRASH_DIR)).map(|r| r.count()).unwrap_or(0), 0, "no trash folder is left");
        assert!(crate::nodes::move_nodes(st(), admin.clone(), req(json!({ "ids": [a, space.root], "dest_id": docs.id }))).await.is_err());
        unchanged().await;

        // Restoring: the first item goes back to the trash when the second one fails
        let _ = crate::nodes::trash(st(), admin.clone(), req(json!({ "ids": [a] }))).await.unwrap();
        let in_trash = node(&env, &a).await.fs_file().unwrap();
        assert!(crate::nodes::restore(st(), admin.clone(), req(json!({ "ids": [a, "no-such-item"] }))).await.is_err());
        assert!(in_trash.is_file() && !dir.join("a.txt").exists());
        assert_eq!(node(&env, &a).await.fs_file().unwrap(), in_trash);
        let _ = crate::nodes::restore(st(), admin.clone(), req(json!({ "ids": [a] }))).await.unwrap();
        unchanged().await;

        // Nothing went wrong: the changes stay
        let _ = crate::nodes::move_nodes(st(), admin.clone(), req(json!({ "ids": [a, b], "dest_id": docs.id }))).await.unwrap();
        assert!(dir.join("Docs/a.txt").is_file() && dir.join("Docs/b.txt").is_file());
    }

    #[tokio::test]
    async fn a_folder_that_looks_empty_or_cant_be_read_keeps_its_items() {
        let env = testutil::env().await;
        let space = env.folder_space("NAS").await;
        let (dir, drive) = (&space.dir, &space.drive);
        write_old(&dir.join("a.txt"), b"a");
        write_old(&dir.join("Sub/b.txt"), b"b");
        crate::folders::scan(&env.st, drive).await.unwrap();
        let (a, _) = env.node_at(drive, "a.txt").await.unwrap();

        // A share that isn't mounted: its mount point is an empty folder, without the marker
        let mounted = dir.with_extension("mounted");
        std::fs::rename(dir, &mounted).unwrap();
        std::fs::create_dir(dir).unwrap();
        let r = crate::folders::scan(&env.st, drive).await.unwrap();
        assert!(r.error.is_some() && r.removed == 0, "{r:?}");
        assert_eq!(env.node_at(drive, "a.txt").await.unwrap().0, a);
        std::fs::remove_dir(dir).unwrap();
        std::fs::rename(&mounted, dir).unwrap();
        let r = crate::folders::scan(&env.st, drive).await.unwrap();
        assert_eq!((r.added, r.removed, r.error.is_none()), (0, 0, true), "{r:?}");

        // A folder that can't be read keeps what the index has in it
        #[cfg(unix)]
        if unsafe { libc::geteuid() } != 0 {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.join("Sub"), std::fs::Permissions::from_mode(0o000)).unwrap();
            let r = crate::folders::scan(&env.st, drive).await;
            std::fs::set_permissions(dir.join("Sub"), std::fs::Permissions::from_mode(0o755)).unwrap();
            let r = r.unwrap();
            assert_eq!(r.removed, 0, "{r:?}");
            assert!(env.node_at(drive, "Sub/b.txt").await.is_some());
        }

        // Emptied for real (the marker is there): the items go
        std::fs::remove_file(dir.join("a.txt")).unwrap();
        std::fs::remove_dir_all(dir.join("Sub")).unwrap();
        let r = crate::folders::scan(&env.st, drive).await.unwrap();
        assert_eq!(r.removed, 3, "{r:?}");
        assert!(env.node_at(drive, "a.txt").await.is_none());
    }

    #[tokio::test]
    async fn a_file_replaced_by_a_folder_of_the_same_name_shows_as_the_folder_when_opened() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        write_old(&space.dir.join("Report"), b"a file");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        std::fs::remove_file(space.dir.join("Report")).unwrap();
        write_old(&space.dir.join("Report/inside.txt"), b"x");
        let later = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        let _ = std::fs::File::open(space.dir.join("Report")).and_then(|f| f.set_modified(later));
        crate::folders::sync_folder(&env.st, &node(&env, &space.root).await).await;
        let (id, _) = env.node_at(&space.drive, "Report").await.unwrap();
        assert!(node(&env, &id).await.is_folder());
    }

    #[test]
    fn a_move_to_another_disk_keeps_what_changed_while_it_was_copied() {
        let top = std::env::temp_dir().join(format!("thirtyfile-copied-{}", new_id()));
        let (from, to) = (top.join("from"), top.join("to"));
        write_old(&from.join("same.txt"), b"same");
        write_old(&from.join("Sub/edited.txt"), b"before");
        let mut copied = Vec::new();
        copy_tree(&from, &to, &mut copied).unwrap();
        assert_eq!(std::fs::read(to.join("Sub/edited.txt")).unwrap(), b"before");
        // Saved over SMB while the copy ran, and a file added
        std::fs::write(from.join("Sub/edited.txt"), b"after, and longer").unwrap();
        write_old(&from.join("new.txt"), b"new");
        remove_copied(copied);
        assert!(!from.join("same.txt").exists());
        assert_eq!(std::fs::read(from.join("Sub/edited.txt")).unwrap(), b"after, and longer");
        assert!(from.join("new.txt").is_file());
        let _ = std::fs::remove_dir_all(&top);
    }

    #[test]
    fn what_a_stopped_change_left_behind_is_put_back_or_removed() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-leftovers-{}", new_id()));
        write_old(&dir.join(format!("{MOVE_PREFIX}1/Report/a.txt")), b"moving");
        write_old(&dir.join("Report"), b"already taken");
        write_old(&dir.join(format!("{UPLOAD_PREFIX}2")), b"half an upload");
        assert!(is_leftover(&format!("{MOVE_PREFIX}1")) && is_leftover(&format!("{SAVE_PREFIX}3")) && !is_leftover(TRASH_DIR));
        let found = vec![dir.join(format!("{MOVE_PREFIX}1")), dir.join(format!("{UPLOAD_PREFIX}2"))];
        clean_leftovers(found.clone(), TRASH_GRACE);
        assert!(found.iter().all(|p| p.exists()), "recent ones may still be in use");
        clean_leftovers(found.clone(), std::time::Duration::ZERO);
        assert_eq!(std::fs::read(dir.join("Report (1)/a.txt")).unwrap(), b"moving");
        assert!(found.iter().all(|p| !p.exists()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
