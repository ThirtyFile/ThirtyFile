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
    path::Path,
};

use axum::http::StatusCode;
use sqlx::SqliteConnection;
use tokio::sync::OwnedMutexGuard;

use crate::{
    auth::User,
    beneath::Pinned,
    error::{AppError, AppResult},
    files::Source,
    jobs::Tracker,
    logs,
    state::AppState,
    tree::{self, BlobRef, Need, Node, StagedBlob, changes},
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
    /// When it was created, where the file system tells (`folders::birth_ns`)
    pub birth_ns: Option<i64>,
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
        birth_ns: crate::folders::birth_ns(&meta),
    })
}

pub fn child_rel(parent: &str, name: &str) -> String {
    if parent.is_empty() { name.to_string() } else { format!("{parent}/{name}") }
}

fn rel_of(n: &Node) -> &str {
    n.fs_path.as_deref().unwrap_or_default()
}

/// The item on disk, reached without following a symbolic link on the way (`beneath`)
fn abs(n: &Node) -> AppResult<Pinned> {
    let rel = n.fs_path.as_deref().ok_or_else(|| gone_or_disk_error(io::ErrorKind::NotFound.into()))?;
    space_root(n)?.join(rel).map_err(gone_or_disk_error)
}

/// A path that no longer leads to the item: a folder on the way is missing, or was replaced by a link
fn gone_or_disk_error(e: io::Error) -> AppError {
    match e.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory | io::ErrorKind::InvalidInput => {
            AppError::not_found("The item is no longer in the folder on the server")
        }
        #[cfg(unix)]
        _ if e.raw_os_error() == Some(libc::ELOOP) => AppError::not_found("The item is no longer in the folder on the server"),
        _ => disk_error(e),
    }
}

/// A space's folder, to change something in it. When it isn't there (its disk or share isn't mounted, say), nothing is
/// changed: it is never made again, which would put the space's files on the disk below the mount point. It must also
/// hold this space's marker (`folders::MARKER`): another disk mounted at the same place, with a folder of the same
/// name, isn't written to.
fn space_root(n: &Node) -> AppResult<Pinned> {
    let root = n.fs_root.as_deref().ok_or_else(|| gone_or_disk_error(io::ErrorKind::NotFound.into()))?;
    let not_mounted = || AppError::new(StatusCode::SERVICE_UNAVAILABLE, crate::storage::NOT_MOUNTED);
    let pinned = Pinned::root(Path::new(root)).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => not_mounted(),
        _ => gone_or_disk_error(e),
    })?;
    match crate::folders::space_marker(&pinned).map_err(disk_error)? {
        Some(id) if id == n.drive() => Ok(pinned),
        _ => Err(not_mounted()),
    }
}

/// Refuses, for an item of a folder space, a name scans skip (`folders::ignored`): ThirtyFile's own files there
/// (`.thirtyfile-…`) and temporary files of other programs. An item with such a name would never show, wouldn't count
/// against the space's size limit, and could be taken for something ThirtyFile left behind and removed.
pub fn check_name(name: &str) -> AppResult<()> {
    if crate::folders::ignored(name) {
        return Err(AppError::bad_request(format!(
            "\"{name}\" can't be used in a folder on the server: names like this are kept for ThirtyFile's own files and for temporary files of other programs"
        )));
    }
    Ok(())
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
    now: Pinned,
    was: Pinned,
    /// A folder made for the rename (a trash folder), removed again when undoing
    made: Option<Pinned>,
}

/// Scan locks of the folder spaces a change touches, held until it is done. They also keep the renames the change
/// made on disk: unless `committed` is called, dropping them puts those back (still holding the locks), newest first.
/// And they keep what the change left to finish after its transaction (tree/changes.rs): once it is committed, that is
/// finished in the background, the locks held until it is done.
pub struct SpaceLocks {
    st: AppState,
    held: Vec<(String, OwnedMutexGuard<()>)>,
    renamed: std::sync::Mutex<Vec<Renamed>>,
    unfinished: std::sync::Mutex<Vec<changes::Unfinished>>,
    committed: std::sync::atomic::AtomicBool,
}

impl SpaceLocks {
    /// An item of a folder space must be in a locked space (it could have moved to another one meanwhile)
    pub fn check(&self, n: &Node) -> AppResult<()> {
        if n.in_folder_space() && !self.held.iter().any(|(d, _)| d == n.drive()) {
            return Err(AppError::conflict("Something changed at the same time. Try again."));
        }
        Ok(())
    }

    fn note(&self, now: Pinned, was: Pinned, made: Option<Pinned>) {
        self.renamed.lock().unwrap_or_else(|e| e.into_inner()).push(Renamed { now, was, made });
    }

    /// What the change leaves to finish after its transaction, if anything
    pub fn later(&self, unfinished: Option<changes::Unfinished>) {
        self.unfinished.lock().unwrap_or_else(|e| e.into_inner()).extend(unfinished);
    }

    /// The change is in the index: its renames stay, and what it left to finish is finished once the locks go
    pub fn committed(&self) {
        self.renamed.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.committed.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Drop for SpaceLocks {
    fn drop(&mut self) {
        let unfinished = std::mem::take(self.unfinished.get_mut().unwrap_or_else(|e| e.into_inner()));
        if *self.committed.get_mut() && !unfinished.is_empty() {
            let held = std::mem::take(&mut self.held).into_iter().map(|(_, g)| g).collect();
            changes::finish_later(&self.st, unfinished, held);
        }
        let renamed = std::mem::take(self.renamed.get_mut().unwrap_or_else(|e| e.into_inner()));
        for r in renamed.into_iter().rev() {
            match rename_new(r.now.as_path(), r.was.as_path()) {
                Ok(()) => {
                    if let Some(made) = r.made {
                        let _ = std::fs::remove_dir(made.as_path());
                    }
                }
                Err(e) => tracing::error!("Couldn't put {:?} back after a failed change: {e}", r.was.name().unwrap_or_default()),
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
    Ok(SpaceLocks { st: st.clone(), held, renamed: Default::default(), unfinished: Default::default(), committed: Default::default() })
}

pub async fn lock_space(drive_id: &str) -> OwnedMutexGuard<()> {
    let guard = crate::folders::drive_lock(drive_id).lock_owned().await;
    // A scan reading the folder meanwhile reads it again
    crate::folders::changing(drive_id);
    guard
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

/// `rel` below `p` ("" for `p` itself), without following a link on the way
fn below(p: &Pinned, rel: &str) -> io::Result<Pinned> {
    if rel.is_empty() { Ok(p.clone()) } else { p.join(rel) }
}

/// An original that was copied: removed afterwards only if it is still what was copied
struct Copied {
    /// Below the item that was copied ("" for the item itself)
    rel: String,
    is_dir: bool,
    /// A file's size and modification time when it was copied
    seen: Option<(u64, i64)>,
}

/// The originals of a move to another disk: the item, and what was copied of it
struct CopiedTree {
    top: Pinned,
    items: Vec<Copied>,
}

/// Copies a file or folder on disk, including what the index doesn't have yet; symbolic links stay links. `copied`
/// lists the originals (by their path below the item), folders before what is in them.
fn copy_tree(from: &Pinned, to: &Pinned, rel: &str, copied: &mut Vec<Copied>, progress: &Tracker) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(from.as_path())?;
    if meta.is_dir() {
        std::fs::create_dir(to.as_path())?;
        progress.add(ITEM_WORK);
        copied.push(Copied { rel: rel.to_string(), is_dir: true, seen: None });
        let (from_dir, to_dir) = (from.dir()?, to.dir()?);
        for item in std::fs::read_dir(from_dir.as_path())? {
            let name = item?.file_name().into_string().map_err(|_| io::Error::other("a name that isn't valid text"))?;
            copy_tree(&from_dir.join(&name)?, &to_dir.join(&name)?, &child_rel(rel, &name), copied, progress)?;
        }
    } else if meta.file_type().is_symlink() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(std::fs::read_link(from.as_path())?, to.as_path())?;
        copied.push(Copied { rel: rel.to_string(), is_dir: false, seen: None });
    } else if meta.is_file() {
        if crate::beneath::copy_file(from, to.as_path())? != meta.len() {
            return Err(io::Error::other("a file wasn't copied completely"));
        }
        progress.add(meta.len() + ITEM_WORK);
        copied.push(Copied { rel: rel.to_string(), is_dir: false, seen: Some((meta.len(), crate::folders::mtime_ns(&meta))) });
    }
    Ok(())
}

/// Removes the originals of a copy: files unchanged since they were copied, then folders left empty. Anything added or
/// changed meanwhile (over SMB, say) stays, and the next scan shows it.
fn remove_copied(copied: CopiedTree) {
    let at = |rel: &str| below(&copied.top, rel);
    for c in copied.items.iter().filter(|c| !c.is_dir) {
        let Ok(p) = at(&c.rel) else { continue };
        let unchanged = match c.seen {
            Some((size, mtime)) => std::fs::symlink_metadata(p.as_path()).is_ok_and(|m| m.len() == size && crate::folders::mtime_ns(&m) == mtime),
            None => true,
        };
        if !unchanged {
            tracing::info!("Kept {:?}: it changed while it was being moved", c.rel);
            continue;
        }
        if let Err(e) = std::fs::remove_file(p.as_path())
            && e.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!("Couldn't remove {:?} from disk: {e}", c.rel);
        }
    }
    for c in copied.items.iter().rev().filter(|c| c.is_dir) {
        if let Ok(p) = at(&c.rel) {
            let _ = std::fs::remove_dir(p.as_path());
        }
    }
}

fn remove_all(p: &Pinned) -> io::Result<()> {
    // Neither follows a symbolic link: a link is removed itself
    if std::fs::symlink_metadata(p.as_path())?.is_dir() { std::fs::remove_dir_all(p.as_path()) } else { std::fs::remove_file(p.as_path()) }
}

fn older_than(p: &Pinned, age: std::time::Duration) -> bool {
    std::fs::symlink_metadata(p.as_path()).and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|a| a >= age))
}

/// Makes a folder, or uses the one already there (not a link)
pub fn ensure_dir(p: &Pinned) -> io::Result<()> {
    match std::fs::create_dir(p.as_path()) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && std::fs::symlink_metadata(p.as_path()).is_ok_and(|m| m.is_dir()) => Ok(()),
        r => r,
    }
}

/// Removes items below a space's folder from disk in the background (the index no longer has them)
pub fn remove_below_later(paths: Vec<crate::beneath::Below>) {
    if paths.is_empty() {
        return;
    }
    tokio::task::spawn_blocking(move || {
        for b in paths {
            if let Err(e) = b.pin().and_then(|p| remove_all(&p))
                && e.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!("Couldn't remove {:?} from disk: {e}", b.rel);
            }
        }
    });
}

/// Removes items from disk in the background (the index no longer has them)
pub fn remove_later(paths: Vec<Pinned>) {
    if paths.is_empty() {
        return;
    }
    tokio::task::spawn_blocking(move || {
        for p in paths {
            if let Err(e) = remove_all(&p)
                && e.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!("Couldn't remove {:?} from disk: {e}", p.name().unwrap_or_default());
            }
        }
    });
}

// ───────────── Index ─────────────

/// A name that neither the index nor the disk has in `parent` yet: `name`, else "name (1)", "name (2)"…
pub async fn free_name(conn: &mut SqliteConnection, parent: &Node, name: &str, is_folder: bool) -> AppResult<String> {
    let dir = abs(parent)?.dir().map_err(gone_or_disk_error)?;
    for n in 0..10_000u32 {
        let candidate = if n == 0 { name.to_string() } else { numbered_name(name, n, is_folder) };
        let on_disk = dir.join(&candidate).is_ok_and(|p| std::fs::symlink_metadata(p.as_path()).is_ok());
        if !tree::name_taken(conn, &parent.id, &candidate).await? && !on_disk {
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
                            fs_path, fs_dev, fs_ino, fs_size, fs_mtime_ns, fs_birth_ns)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
    .bind(s.birth_ns)
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
        "UPDATE nodes SET drive_id = ?, fs_path = ?, fs_dev = ?, fs_ino = ?, fs_size = ?, fs_mtime_ns = ?, fs_birth_ns = ?,
                          size = CASE WHEN kind = 'file' THEN ? ELSE size END
         WHERE id = ?",
    )
    .bind(drive_id)
    .bind(rel)
    .bind(s.dev)
    .bind(s.ino)
    .bind(s.size)
    .bind(s.mtime_ns)
    .bind(s.birth_ns)
    .bind(s.size)
    .bind(id)
    .execute(conn)
    .await?;
    Ok(())
}

// ───────────── Changes within a space ─────────────

/// Makes a folder on disk; one that was made on the server meanwhile is used as it is
pub fn make_dir(parent: &Node, name: &str) -> AppResult<(String, Stat)> {
    check_name(name)?;
    let path = abs(parent)?.join(name).map_err(gone_or_disk_error)?;
    ensure_dir(&path).map_err(disk_error)?;
    Ok((child_rel(rel_of(parent), name), stat(path.as_path()).map_err(disk_error)?))
}

/// Renames an item, or moves it to another folder of its space: on disk, then in the index (the caller records its
/// new name and folder)
pub async fn rename(conn: &mut SqliteConnection, locks: &SpaceLocks, node: &Node, dest: &Node, name: &str) -> AppResult<()> {
    check_name(name)?;
    let (from, to) = (abs(node)?, abs(dest)?.join(name).map_err(gone_or_disk_error)?);
    rename_new(from.as_path(), to.as_path()).map_err(disk_error)?;
    locks.note(to, from, None);
    locks.later(changes::repath(conn, &node.id, node.drive(), rel_of(node), &child_rel(rel_of(dest), name)).await?);
    Ok(())
}

/// Moves an item to the space's trash folder, where it can be restored from
pub async fn trash(conn: &mut SqliteConnection, locks: &SpaceLocks, node: &Node, trash_id: &str) -> AppResult<()> {
    let root = space_root(node)?;
    let from = abs(node)?;
    ensure_dir(&root.join(TRASH_DIR).map_err(disk_error)?).map_err(disk_error)?;
    let dir = root.join(&format!("{TRASH_DIR}/{trash_id}")).map_err(disk_error)?;
    std::fs::create_dir(dir.as_path()).map_err(disk_error)?;
    let to = dir.join(&node.name).map_err(disk_error)?;
    match rename_new(from.as_path(), to.as_path()) {
        Ok(()) => locks.note(to, from, Some(dir)),
        // Already gone from the server: only the index still had it
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            let _ = std::fs::remove_dir(dir.as_path());
            return Err(disk_error(e));
        }
    }
    locks.later(changes::repath(conn, &node.id, node.drive(), rel_of(node), &format!("{TRASH_DIR}/{trash_id}/{}", node.name)).await?);
    Ok(())
}

/// Moves a trashed item back into `dest` as `name`
pub async fn restore(conn: &mut SqliteConnection, locks: &SpaceLocks, node: &Node, dest: &Node, name: &str) -> AppResult<()> {
    let (from, to) = (abs(node)?, abs(dest)?.join(name).map_err(gone_or_disk_error)?);
    rename_new(from.as_path(), to.as_path()).map_err(disk_error)?;
    // Its emptied trash folder stays until `clean_trash` (should the change fail, the item goes back into it)
    locks.note(to, from, None);
    locks.later(changes::repath(conn, &node.id, node.drive(), rel_of(node), &child_rel(rel_of(dest), name)).await?);
    Ok(())
}

/// The folder holding a trashed item of a folder space, removed from disk when the item is deleted for good
pub fn trash_folder(n: &Node) -> Option<crate::beneath::Below> {
    let mut parts = n.fs_path.as_deref()?.split('/');
    if parts.next()? != TRASH_DIR {
        return None;
    }
    let id = parts.next()?;
    Some(crate::beneath::Below::new(n.fs_root.as_deref()?, n.drive(), format!("{TRASH_DIR}/{id}")))
}

/// Trash folders younger than this are never removed by `clean_trash`, known or not
pub const TRASH_GRACE: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// Removes trash folders the index doesn't know (deleted for good while removing them from disk failed, say). Recent
/// ones stay: something that went wrong halfway may still need them. Only folders named by a trash id are ThirtyFile's.
pub async fn clean_trash(st: &AppState, drive_id: &str, root: &Path) -> AppResult<()> {
    let top = root.to_path_buf();
    let names = tokio::task::spawn_blocking(move || -> Vec<String> {
        let Ok(dir) = Pinned::root(&top).and_then(|r| r.join(TRASH_DIR)).and_then(|t| t.dir()) else { return Vec::new() };
        let Ok(read) = std::fs::read_dir(dir.as_path()) else { return Vec::new() };
        read.flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| crate::util::is_new_id(n) && dir.join(n).is_ok_and(|p| older_than(&p, TRASH_GRACE)))
            .collect()
    })
    .await?;
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
    remove_below_later(names.into_iter().filter(|n| !known.contains(n)).map(|n| crate::beneath::Below::new(root, drive_id, format!("{TRASH_DIR}/{n}"))).collect());
    Ok(())
}

/// Whether a name scans ignore is something a change left behind when it stopped halfway (a restart, say): one of
/// ThirtyFile's prefixes and a new id, exactly as it names them. An item people named alike is never taken for one.
pub fn is_leftover(name: &str) -> bool {
    [MOVE_PREFIX, COPY_PREFIX, UPLOAD_PREFIX, SAVE_PREFIX].iter().any(|p| name.strip_prefix(p).is_some_and(crate::util::is_new_id))
}

/// Deals with what changes left behind (`is_leftover`, paths found by a scan) once they are old enough that no change
/// can still be using them: an item that was being moved in is put back under its own name, where the next scan
/// shows it; half-made copies, uploads and saves are removed. `age`: how old they must be.
pub fn clean_leftovers(paths: Vec<Pinned>, age: std::time::Duration) {
    for p in paths.into_iter().filter(|p| older_than(p, age)) {
        let name = p.name().unwrap_or_default();
        let (Some(dir), true) = (p.parent(), name.starts_with(MOVE_PREFIX)) else {
            if let Err(e) = remove_all(&p)
                && e.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!("Couldn't remove {name:?} from disk: {e}");
            }
            continue;
        };
        let Ok(inside) = p.dir() else { continue };
        for item in std::fs::read_dir(inside.as_path()).into_iter().flatten().flatten() {
            let Ok(item_name) = item.file_name().into_string() else { continue };
            let Ok(from) = inside.join(&item_name) else { continue };
            let is_dir = item.file_type().is_ok_and(|t| t.is_dir());
            let back = (0..10_000u32).map(|n| if n == 0 { item_name.clone() } else { numbered_name(&item_name, n, is_dir) }).find_map(|candidate| {
                match dir.join(&candidate).and_then(|to| rename_new(from.as_path(), to.as_path())) {
                    Ok(()) => Some(Ok(candidate)),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => None,
                    Err(e) => Some(Err(e)),
                }
            });
            match back {
                Some(Ok(n)) => tracing::warn!("Put back {n:?}: it was being moved when ThirtyFile stopped"),
                Some(Err(e)) => tracing::warn!("Couldn't put back {item_name:?}: {e}"),
                None => {}
            }
        }
        let _ = std::fs::remove_dir(p.as_path());
    }
}

// ───────────── Uploads and saving ─────────────

/// Removes a file staged for a change when the change stops before using it (once renamed into place, the name is gone)
struct Discard(Option<Pinned>);

impl Drop for Discard {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p.as_path());
        }
    }
}

const UPLOAD_PREFIX: &str = ".thirtyfile-upload-";
const SAVE_PREFIX: &str = ".thirtyfile-save-";

/// Puts a finished upload into the space's folder under a name scans ignore, ready to be renamed into place: a
/// rename when the folder is on the same disk as ThirtyFile's data, else a copy. Counted as storing content on the
/// space's location (Storage usage).
pub async fn stage_upload(st: &AppState, folder: &Node, tmp: &Path, size: u64) -> AppResult<Pinned> {
    let location = crate::usage::sample::folder_location(st, Path::new(folder.fs_root.as_deref().unwrap_or_default())).await;
    let started = std::time::Instant::now();
    let staged = stage(folder, tmp, size).await;
    crate::usage::sample::record_folder(st, &location, crate::usage::Op::Write, started, staged.is_ok(), size);
    staged
}

async fn stage(folder: &Node, tmp: &Path, size: u64) -> AppResult<Pinned> {
    let staged = space_root(folder)?.join(&format!("{UPLOAD_PREFIX}{}", new_id())).map_err(disk_error)?;
    if tokio::fs::rename(tmp, staged.as_path()).await.is_err() {
        let (from, to) = (tmp.to_path_buf(), staged.clone());
        let copied = tokio::task::spawn_blocking(move || {
            let mut src = std::fs::File::open(&from)?;
            let mut dst = std::fs::File::create_new(to.as_path())?;
            let n = io::copy(&mut src, &mut dst)?;
            dst.sync_all().map(|()| n)
        })
        .await?;
        match copied {
            Ok(n) if n == size => {
                let _ = tokio::fs::remove_file(tmp).await;
            }
            copied => {
                let _ = tokio::fs::remove_file(staged.as_path()).await;
                return Err(copied.err().map(disk_error).unwrap_or_else(incomplete));
            }
        }
    }
    Ok(staged)
}

/// Renames content staged in the space's folder into `folder` as `name` and indexes it; returns the new item's id
pub async fn place_file(conn: &mut SqliteConnection, staged: &Pinned, owner: i64, folder: &Node, name: &str) -> AppResult<String> {
    check_name(name)?;
    let to = abs(folder)?.join(name).map_err(gone_or_disk_error)?;
    rename_new(staged.as_path(), to.as_path()).map_err(disk_error)?;
    let s = stat(to.as_path()).map_err(disk_error)?;
    let id = new_id();
    insert(conn, &id, owner, folder, name, &child_rel(rel_of(folder), name), &s).await?;
    Ok(id)
}

/// Renames content staged in the space's folder over an existing file, which keeps its id, and indexes it (the caller
/// counts the change in size). The file it had is kept as an earlier version first; returns what versions no longer
/// kept leave to remove after the commit.
pub async fn replace_file(conn: &mut SqliteConnection, policy: versions::Policy, staged: &Pinned, existing: &Node, by: i64) -> AppResult<versions::Removed> {
    let to = abs(existing)?;
    // The new content keeps the file's permissions
    let _ = crate::beneath::copy_permissions(&to, staged);
    let removed = versions::keep_file(conn, policy, existing, &to).await?;
    std::fs::rename(staged.as_path(), to.as_path()).map_err(disk_error)?;
    let s = stat(to.as_path()).map_err(disk_error)?;
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
    let before = tree::node_for(&mut *st.db.acquire().await?, user, id, Need::Write).await?;
    let drive = before.drive().to_string();
    // The new content is written next to the file before taking the locks, so a large save doesn't hold up every
    // other change meanwhile; it is removed again unless it is put in place
    let tmp = abs(&before)?.parent().ok_or_else(|| AppError::not_found("Item not found"))?.join(&format!("{SAVE_PREFIX}{}", new_id())).map_err(disk_error)?;
    {
        let (tmp, body) = (tmp.clone(), body.to_vec());
        tokio::task::spawn_blocking(move || crate::beneath::write_new(&tmp, &body)).await?.map_err(disk_error)?;
    }
    let _discard = Discard(Some(tmp.clone()));
    let _space = lock_space(&drive).await;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let node = tree::node_for(&mut tx, user, id, Need::Write).await?;
    if node.drive() != drive {
        return Err(AppError::conflict("Something changed at the same time. Try again."));
    }
    if base.is_some_and(|b| b != node.updated_at) {
        return Err(conflict());
    }
    let path = abs(&node)?;
    let (fs_size, fs_mtime, fs_ino): (Option<i64>, Option<i64>, Option<i64>) =
        sqlx::query_as("SELECT fs_size, fs_mtime_ns, fs_ino FROM nodes WHERE id = ?").bind(&node.id).fetch_one(&mut *tx).await?;
    let unchanged = stat(path.as_path()).is_ok_and(|s| Some(s.size) == fs_size && Some(s.mtime_ns) == fs_mtime && Some(s.ino) == fs_ino);
    // The new content counts against the space's size limit: by how much it grows the file, or all of it as a copy
    tree::check_quota(&mut tx, node.drive(), body.len() as i64 - if unchanged { node.size } else { 0 }).await?;
    // The new content keeps the file's permissions
    match crate::beneath::copy_permissions(&path, &tmp) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(disk_error(e)),
        _ => {}
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
                let _ = std::fs::remove_file(tmp.as_path());
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
            let _ = std::fs::remove_file(tmp.as_path());
            return Err(e);
        }
    };
    if let Err(e) = std::fs::rename(tmp.as_path(), path.as_path()) {
        let _ = std::fs::remove_file(tmp.as_path());
        return Err(disk_error(e));
    }
    let s = stat(path.as_path()).map_err(disk_error)?;
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
    Disk { wrap: Pinned, tmp: Pinned, renamed_from: Option<Pinned>, copied: Option<CopiedTree> },
    /// In the destination's content store: one staged content per file, and the file's size and modification time as
    /// it was stored (by node id)
    Store(HashMap<String, StagedBlob>, HashMap<String, (u64, i64)>),
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
async fn write_tree(st: &AppState, nodes: &[Node], top: &Pinned, progress: &Tracker) -> AppResult<()> {
    for (n, rel) in nodes.iter().zip(layout(nodes)) {
        let Some(rel) = rel else { continue };
        let to = below(top, &rel).map_err(disk_error)?;
        if n.is_folder() {
            tokio::fs::create_dir(to.as_path()).await.map_err(disk_error)?;
            progress.add(ITEM_WORK);
            continue;
        }
        match Source::of(n)? {
            Source::File(from) => {
                let copied = tokio::task::spawn_blocking(move || {
                    let want = std::fs::symlink_metadata(from.as_path())?.len();
                    crate::beneath::copy_file(&from, to.as_path()).map(|got| got == want)
                })
                .await?;
                if !copied.map_err(disk_error)? {
                    return Err(incomplete());
                }
            }
            source => {
                let mut reader = source
                    .open(st, 0, n.size as u64)
                    .await
                    .map_err(|e| AppError::new(StatusCode::BAD_GATEWAY, format!("Couldn't read \"{}\": {e}", n.name)))?;
                let mut file = tokio::fs::OpenOptions::new().write(true).create_new(true).open(to.as_path()).await.map_err(disk_error)?;
                let got = tokio::io::copy(&mut reader, &mut file).await.map_err(disk_error)?;
                file.sync_all().await.map_err(disk_error)?;
                if got != n.size as u64 {
                    return Err(incomplete());
                }
            }
        }
        progress.add(work_of(n));
    }
    Ok(())
}

const CHANGED_WHILE_COPIED: &str = "the file changed while it was being copied";

/// Copies a file of a folder space to `to`; returns its size and modification time as it was copied (it mustn't change
/// meanwhile)
fn copy_checked(from: &Pinned, to: &Path) -> io::Result<(u64, i64)> {
    let mut src = from.open_file()?;
    let before = src.metadata()?;
    let mut dst = std::fs::File::create_new(to)?;
    let n = io::copy(&mut src, &mut dst)?;
    dst.sync_all()?;
    let after = std::fs::symlink_metadata(from.as_path())?;
    let mtime = crate::folders::mtime_ns(&before);
    if n != before.len() || after.len() != before.len() || crate::folders::mtime_ns(&after) != mtime {
        return Err(io::Error::other(CHANGED_WHILE_COPIED));
    }
    Ok((n, mtime))
}

/// Stores the files of a folder space in the content store of the space `drive`, for a move (`moving`) or a copy
async fn ingest(st: &AppState, nodes: &[Node], drive: &str, moving: bool, progress: &Tracker) -> AppResult<Placed> {
    let (mut staged, mut seen) = (HashMap::new(), HashMap::new());
    for n in nodes.iter().filter(|n| !n.is_folder()) {
        let tmp = st.tmp_dir().join(new_id());
        let stored = async {
            let (from, to) = (abs(n)?, tmp.clone());
            let copied = tokio::task::spawn_blocking(move || copy_checked(&from, &to)).await?.map_err(|e| {
                if e.to_string() != CHANGED_WHILE_COPIED {
                    disk_error(e)
                } else if moving {
                    AppError::conflict(format!("\"{}\" changed while it was being moved. Try again.", n.name))
                } else {
                    AppError::conflict(format!("\"{}\" changed while it was being copied. Try again.", n.name))
                }
            })?;
            let (hash, size) = crate::files::hash_file(tmp.clone()).await?;
            Ok::<_, AppError>((tree::stage_blob(st, drive, hash, size as i64, tmp.clone()).await?, copied))
        }
        .await;
        match stored {
            Ok((s, copied)) => {
                progress.add(work_of(n));
                staged.insert(n.id.clone(), s);
                seen.insert(n.id.clone(), copied);
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
    progress.add(ITEM_WORK * nodes.iter().filter(|n| n.is_folder()).count() as u64);
    Ok(Placed::Store(staged, seen))
}

/// Puts the content of `nodes` (an item and everything in it) into place for `dest`
async fn place(st: &AppState, nodes: &[Node], dest: &Node, moving: bool, progress: &Tracker) -> AppResult<Placed> {
    let top = &nodes[0];
    if !dest.in_folder_space() {
        return ingest(st, nodes, dest.drive(), moving, progress).await;
    }
    for n in nodes {
        check_name(&n.name)?;
    }
    let within = abs(dest)?;
    let wrap = within.join(&format!("{}{}", if moving { MOVE_PREFIX } else { COPY_PREFIX }, new_id())).map_err(gone_or_disk_error)?;
    tokio::fs::create_dir(wrap.as_path()).await.map_err(disk_error)?;
    let tmp = wrap.join(&top.name).map_err(disk_error)?;
    if moving && top.in_folder_space() {
        let from = abs(top)?;
        let renamed = if other_disk() { Err(io::ErrorKind::CrossesDevices.into()) } else { std::fs::rename(from.as_path(), tmp.as_path()) };
        match renamed {
            Ok(()) => {
                progress.add(nodes.iter().map(work_of).sum());
                return Ok(Placed::Disk { wrap, tmp, renamed_from: Some(from), copied: None });
            }
            Err(e) if e.kind() != io::ErrorKind::CrossesDevices => {
                let _ = std::fs::remove_dir(wrap.as_path());
                return Err(disk_error(e));
            }
            Err(_) => {}
        }
        // Another disk: copy everything, including what the index doesn't have yet, before the original goes. Until the
        // index follows, the original stays where it was and this is only a copy: it is made in a folder named as one,
        // which is removed rather than put back should ThirtyFile stop meanwhile (`clean_leftovers`)
        let copy_wrap = within.join(&format!("{COPY_PREFIX}{}", new_id())).map_err(disk_error)?;
        if let Err(e) = rename_new(wrap.as_path(), copy_wrap.as_path()) {
            let _ = std::fs::remove_dir(wrap.as_path());
            return Err(disk_error(e));
        }
        let (wrap, tmp) = (copy_wrap.clone(), copy_wrap.join(&top.name).map_err(disk_error)?);
        let (f, t, p) = (from.clone(), tmp.clone(), progress.clone());
        match tokio::task::spawn_blocking(move || {
            let mut items = Vec::new();
            copy_tree(&f, &t, "", &mut items, &p).map(|()| CopiedTree { top: f, items })
        })
        .await?
        {
            Ok(copied) => return Ok(Placed::Disk { wrap, tmp, renamed_from: None, copied: Some(copied) }),
            Err(e) => {
                remove_later(vec![wrap]);
                return Err(disk_error(e));
            }
        }
    }
    if let Err(e) = write_tree(st, nodes, &tmp, progress).await {
        remove_later(vec![wrap]);
        return Err(e);
    }
    Ok(Placed::Disk { wrap, tmp, renamed_from: None, copied: None })
}

/// A move or copy's progress counts each item as this many bytes besides its content: making a file or folder takes
/// about as long as copying that much, so a folder of many small files doesn't look done long before it is
const ITEM_WORK: u64 = 256 * 1024;

/// What moving or copying an item counts for in the progress
fn work_of(n: &Node) -> u64 {
    ITEM_WORK + if n.is_folder() { 0 } else { n.size.max(0) as u64 }
}

/// Folders holding an item on its way into a folder (`Placed::Disk`)
pub const MOVE_PREFIX: &str = ".thirtyfile-move-";
pub(crate) const COPY_PREFIX: &str = ".thirtyfile-copy-";

// ───────────── Building a new folder (extracting a ZIP file) ─────────────

/// A new folder built inside `dest`'s folder under a name scans ignore, removed again unless `place_folder` puts it in
/// place
pub struct Staging {
    wrap: Pinned,
    /// The new folder, inside `wrap`
    pub top: Pinned,
}

impl Drop for Staging {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(self.wrap.as_path()).is_ok() {
            remove_later(vec![self.wrap.clone()]);
        }
    }
}

pub fn staging(dest: &Node) -> AppResult<Staging> {
    let wrap = abs(dest)?.join(&format!("{COPY_PREFIX}{}", new_id())).map_err(gone_or_disk_error)?;
    std::fs::create_dir(wrap.as_path()).map_err(disk_error)?;
    let top = wrap.join("folder").map_err(disk_error)?;
    let staged = Staging { wrap, top };
    std::fs::create_dir(staged.top.as_path()).map_err(disk_error)?;
    Ok(staged)
}

/// Makes the folders `dirs` below `top` (those already there are used)
pub fn make_dirs(top: &Pinned, dirs: &[String]) -> io::Result<Pinned> {
    let mut at = top.clone();
    for d in dirs {
        at = at.join(d)?;
        ensure_dir(&at)?;
    }
    Ok(at)
}

/// Moves a finished file (`tmp`, in ThirtyFile's data folder) into the folder `dir` as `name`, or "name (1)"… when
/// the name is taken: renamed on the same disk, else copied. Never replaces anything.
pub fn move_in(tmp: &Path, dir: &Pinned, name: &str) -> io::Result<()> {
    for n in 0..10_000u32 {
        let candidate = if n == 0 { name.to_string() } else { numbered_name(name, n, false) };
        let to = dir.join(&candidate)?;
        match rename_new(tmp, to.as_path()) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
                let mut src = std::fs::File::open(tmp)?;
                let mut dst = match std::fs::File::create_new(to.as_path()) {
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                    r => r?,
                };
                io::copy(&mut src, &mut dst)?;
                dst.sync_all()?;
                return std::fs::remove_file(tmp);
            }
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "too many items with the same name"))
}

/// Everything in a folder (and the folder itself, as ""), parents before their contents: (path below it, what the
/// index keeps)
fn list_tree(top: &Pinned) -> io::Result<Vec<(String, Stat)>> {
    let mut out = vec![(String::new(), stat(top.as_path())?)];
    let mut queue = vec![String::new()];
    while let Some(rel) = queue.pop() {
        let dir = below(top, &rel)?.dir()?;
        let mut names: Vec<String> = std::fs::read_dir(dir.as_path())?.flatten().filter_map(|e| e.file_name().into_string().ok()).collect();
        names.sort();
        for name in names {
            let child = child_rel(&rel, &name);
            let s = stat(dir.join(&name)?.as_path())?;
            if s.is_dir {
                queue.push(child.clone());
            }
            out.push((child, s));
        }
    }
    Ok(out)
}

/// Puts a folder built with `staging` into `dest_id` as `name` (or "name (1)"… when taken) and indexes everything in
/// it, counted against the space's quota; `action` goes into the activity log with `detail`. Returns the new folder's
/// id and name.
pub async fn place_folder(st: &AppState, user: &User, staged: Staging, dest_id: &str, name: &str, action: &str, detail: &str) -> AppResult<(String, String)> {
    let top = staged.top.clone();
    let items = tokio::task::spawn_blocking(move || list_tree(&top)).await?.map_err(disk_error)?;
    let bytes: i64 = items.iter().filter(|(_, s)| !s.is_dir).map(|(_, s)| s.size).sum();
    let locks = lock(st, user, &[dest_id]).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let dest = tree::folder_for(&mut tx, user, dest_id, Need::Write).await?;
    locks.check(&dest)?;
    tree::check_quota(&mut tx, dest.drive(), bytes).await?;
    check_name(name)?;
    let name = free_name(&mut tx, &dest, name, true).await?;
    let final_path = abs(&dest)?.join(&name).map_err(gone_or_disk_error)?;
    rename_new(staged.top.as_path(), final_path.as_path()).map_err(disk_error)?;
    locks.note(final_path, staged.top.clone(), None);
    let dest_rel = child_rel(rel_of(&dest), &name);
    let mut ids: HashMap<String, String> = HashMap::new();
    for (rel, s) in &items {
        let id = new_id();
        let (parent, item_name) = match rel.rsplit_once('/') {
            _ if rel.is_empty() => (dest.id.clone(), name.as_str()),
            Some((up, last)) => (ids.get(up).cloned().unwrap_or_default(), last),
            None => (ids.get("").cloned().unwrap_or_default(), rel.as_str()),
        };
        let full = if rel.is_empty() { dest_rel.clone() } else { format!("{dest_rel}/{rel}") };
        insert_at(&mut tx, &id, user.id, &parent, dest.drive(), item_name, &full, s).await?;
        ids.insert(rel.clone(), id);
    }
    let root = ids.get("").cloned().unwrap_or_default();
    tree::adjust_usage(&mut tx, dest.drive(), bytes).await?;
    tree::touch(&mut tx, &dest.id).await?;
    let node = tree::get_node(&mut tx, &root).await?;
    logs::record_activity(&mut tx, user, node.as_ref(), action, detail).await?;
    tx.commit().await?;
    locks.committed();
    // The folder that held it is empty now: removed before the change is reported done, so nothing half-made is left
    // behind once it is (should the change fail instead, the renames are undone and `Staging` removes it all)
    let _ = std::fs::remove_dir(staged.wrap.as_path());
    Ok((root, name))
}

#[cfg(test)]
thread_local! {
    /// Tests: treat every folder as being on another disk, so moves copy
    static OTHER_DISK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn other_disk() -> bool {
    OTHER_DISK.with(|d| d.get())
}

#[cfg(test)]
type Hook = Box<dyn Fn() -> futures_util::future::BoxFuture<'static, ()>>;

#[cfg(test)]
thread_local! {
    /// Tests: runs once the content of items moved to another space is in place, before the index follows
    static AFTER_PLACE: std::cell::RefCell<Option<Hook>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
async fn after_place() {
    let run = AFTER_PLACE.with(|h| h.borrow().as_ref().map(|f| f()));
    if let Some(run) = run {
        run.await;
    }
}

/// Tests: clears the hook of `hook_after_place` when dropped
#[cfg(test)]
pub(crate) struct HookGuard;

#[cfg(test)]
impl Drop for HookGuard {
    fn drop(&mut self) {
        AFTER_PLACE.with(|h| *h.borrow_mut() = None);
    }
}

/// Tests: runs `hook` each time the content of a move or copy to another space is in place, before the index follows
#[cfg(test)]
pub(crate) fn hook_after_place(hook: impl Fn() -> futures_util::future::BoxFuture<'static, ()> + 'static) -> HookGuard {
    AFTER_PLACE.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    HookGuard
}

/// Tests: a hook for `hook_after_place` that waits until `go` is notified
#[cfg(test)]
pub(crate) fn wait_for(go: &std::sync::Arc<tokio::sync::Notify>) -> impl Fn() -> futures_util::future::BoxFuture<'static, ()> + 'static {
    let go = go.clone();
    move || {
        let go = go.clone();
        Box::pin(async move { go.notified().await })
    }
}

#[cfg(not(test))]
fn other_disk() -> bool {
    false
}

/// Takes back content put in place when the index couldn't follow
async fn undo(st: &AppState, placed: Placed) {
    match placed {
        Placed::Disk { wrap, tmp, renamed_from: Some(from), .. } => {
            if std::fs::rename(tmp.as_path(), from.as_path()).is_err() && std::fs::symlink_metadata(tmp.as_path()).is_ok() {
                tracing::warn!("Couldn't move {:?} back where it was", from.name().unwrap_or_default());
            } else {
                let _ = std::fs::remove_dir(wrap.as_path());
            }
        }
        Placed::Disk { wrap, renamed_from: None, .. } => remove_later(vec![wrap]),
        Placed::Store(staged, _) => {
            for (_, s) in staged {
                tree::abandon_staged(st, s).await;
            }
        }
    }
}

/// After the index changed: staged content is kept, content no longer used goes
async fn finish(st: &AppState, placed: Placed, extras: Vec<BlobRef>) {
    if let Placed::Store(staged, _) = placed {
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

/// Items to move or copy to or from a folder space, checked in the transaction of the move or copy. Their content is
/// copied (or renamed) item by item afterwards, which can take long: the request runs it as a job (jobs.rs).
pub struct Across {
    dest: Node,
    /// Each item with everything in it (not in the trash), ordered by depth
    items: Vec<Vec<Node>>,
    moving: bool,
}

impl Across {
    /// None when there is nothing to do this way
    pub fn new(dest: Node, items: Vec<Vec<Node>>, moving: bool) -> Option<Across> {
        (!items.is_empty()).then_some(Across { dest, items, moving })
    }

    pub async fn run(self, st: &AppState, user: &User, progress: &Tracker) -> AppResult<()> {
        progress.add_total(self.items.iter().flatten().map(work_of).sum());
        if self.moving {
            move_across(st, user, &self.dest, self.items, progress).await
        } else {
            copy_across(st, user, &self.dest, self.items, progress).await
        }
    }
}

/// Moves items to a folder of another space, where one of the two (or both) is a folder space. The content is put in
/// place first (renamed when both folders are on the same disk, else copied); then the index moves the items, which
/// keep their ids and with them their shares, permissions and favourites; then the originals are removed.
/// `items`: each item with everything in it (not in the trash), ordered by depth.
pub async fn move_across(st: &AppState, user: &User, dest: &Node, items: Vec<Vec<Node>>, progress: &Tracker) -> AppResult<()> {
    for nodes in items {
        let mut placed = place(st, &nodes, dest, true, progress).await?;
        #[cfg(test)]
        after_place().await;
        let result = {
            let _w = st.write_lock.lock().await;
            commit_move(st, user, dest, &nodes, &placed).await
        };
        match result {
            Ok((extras, remove)) => {
                let copied = match &mut placed {
                    Placed::Disk { copied, .. } => copied.take(),
                    Placed::Store(..) => None,
                };
                finish(st, placed, extras).await;
                if let Some(copied) = copied {
                    tokio::task::spawn_blocking(move || remove_copied(copied));
                }
                // Stored in the content store: the originals go, each file only if it is still what was stored
                if !remove.is_empty()
                    && let Ok(top) = space_root(&nodes[0])
                {
                    tokio::task::spawn_blocking(move || remove_copied(CopiedTree { top, items: remove }));
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

/// Checks, in the transaction that records a move or copy, that the destination is still as the content was put in
/// place for: there, writable, and in the same space kept the same way (its folder, or the content store). The space
/// may have been moved to another storage location, or made read-only, while the content was copied.
async fn still_there(conn: &mut SqliteConnection, dest: &Node) -> AppResult<()> {
    let now = tree::get_node(conn, &dest.id)
        .await?
        .filter(|d| d.trashed_at.is_none())
        .ok_or_else(|| AppError::not_found("The destination folder no longer exists"))?;
    if now.space_read_only {
        return Err(tree::read_only_error(&now));
    }
    if now.drive_id != dest.drive_id || now.fs_root != dest.fs_root {
        return Err(AppError::conflict("The destination was moved to another storage location meanwhile. Try again."));
    }
    Ok(())
}

/// The index side of a move; returns content no longer used and (for items now in the content store) what to remove
/// from disk (paths below the space's folder, folders before what is in them)
async fn commit_move(st: &AppState, user: &User, dest: &Node, nodes: &[Node], placed: &Placed) -> AppResult<(Vec<BlobRef>, Vec<Copied>)> {
    let mut tx = crate::db::begin_write(&st.db).await?;
    let top = &nodes[0];
    // Everything must still be as it was when the content was copied: items added to a folder of the content store
    // meanwhile would be left behind
    let changed = || AppError::conflict(format!("\"{}\" changed while it was being moved. Try again.", top.name));
    let current = tree::get_node(&mut tx, &top.id).await?.ok_or_else(changed)?;
    if current.trashed_at.is_some() || current.parent_id != top.parent_id || current.drive_id != top.drive_id || current.fs_root != top.fs_root {
        return Err(changed());
    }
    // The space it leaves may have started moving to another storage location while the content was copied: that move
    // has its items as they are, and switches them over itself. (A personal space being removed is read-only too, while
    // its files are moved out this way.)
    if current.space_moving && crate::moves::drive_busy(&mut tx, current.drive()).await? {
        return Err(tree::read_only_error(&current));
    }
    still_there(&mut tx, dest).await?;
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
            let final_path = abs(dest)?.join(&top.name).map_err(gone_or_disk_error)?;
            rename_new(tmp.as_path(), final_path.as_path()).map_err(disk_error)?;
            let _ = std::fs::remove_dir(wrap.as_path());
            let dest_rel = child_rel(rel_of(dest), &top.name);
            for (n, rel) in nodes.iter().zip(layout(nodes)) {
                let Some(rel) = rel else { continue };
                let full = if rel.is_empty() { dest_rel.clone() } else { format!("{dest_rel}/{rel}") };
                match below(&final_path, &rel).and_then(|p| stat(p.as_path())) {
                    Ok(s) => record(&mut tx, &n.id, dest.drive(), &full, &s).await?,
                    // Gone from the server before it could be moved: gone from the index too, with the earlier versions
                    // it may still have in the content store
                    Err(_) => extras.extend(tree::purge_subtree(&mut tx, &n.id).await?),
                }
            }
            // Content that was in the content store is in the folder now
            let hashes: Vec<String> = nodes.iter().filter_map(|n| n.blob_hash.clone()).collect();
            sqlx::query("UPDATE nodes SET blob_hash = NULL WHERE id IN (SELECT value FROM json_each(?))").bind(&ids).execute(&mut *tx).await?;
            extras.extend(tree::release_blobs(&mut tx, &hashes).await?);
        }
        Placed::Store(staged, seen) => {
            let names = store_names(nodes, &top.name);
            // Leave the names the folder gave them first, so renamed items can't run into each other on the way
            sqlx::query("UPDATE nodes SET name = char(1) || id WHERE id IN (SELECT value FROM json_each(?))").bind(&ids).execute(&mut *tx).await?;
            sqlx::query(
                "UPDATE nodes SET drive_id = ?, fs_path = NULL, fs_dev = NULL, fs_ino = NULL, fs_size = NULL, fs_mtime_ns = NULL, fs_birth_ns = NULL
                 WHERE id IN (SELECT value FROM json_each(?))",
            )
            .bind(dest.drive())
            .bind(&ids)
            .execute(&mut *tx)
            .await?;
            for (n, name) in nodes.iter().zip(&names) {
                sqlx::query("UPDATE nodes SET name = ? WHERE id = ?").bind(name).bind(&n.id).execute(&mut *tx).await?;
                if let Some(s) = staged.get(&n.id) {
                    extras.extend(tree::commit_blob(&mut tx, s).await?);
                    sqlx::query("UPDATE nodes SET blob_hash = ?, size = ? WHERE id = ?").bind(&s.hash).bind(s.size).bind(&n.id).execute(&mut *tx).await?;
                }
                if let Some(rel) = n.fs_path.clone().filter(|r| !r.is_empty()) {
                    remove.push(Copied { rel, is_dir: n.is_folder(), seen: seen.get(&n.id).copied() });
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
pub async fn copy_across(st: &AppState, user: &User, dest: &Node, plans: Vec<Vec<Node>>, progress: &Tracker) -> AppResult<()> {
    for nodes in plans {
        let placed = place(st, &nodes, dest, false, progress).await?;
        #[cfg(test)]
        after_place().await;
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
    let mut tx = crate::db::begin_write(&st.db).await?;
    let top = &nodes[0];
    still_there(&mut tx, dest).await?;
    let mut ids: HashMap<&str, String> = HashMap::new();
    let mut extras = Vec::new();
    let mut bytes = 0i64;
    match placed {
        Placed::Disk { wrap, tmp, .. } => {
            let name = free_name(&mut tx, dest, &top.name, top.is_folder()).await?;
            let final_path = abs(dest)?.join(&name).map_err(gone_or_disk_error)?;
            rename_new(tmp.as_path(), final_path.as_path()).map_err(disk_error)?;
            let _ = std::fs::remove_dir(wrap.as_path());
            let dest_rel = child_rel(rel_of(dest), &name);
            for (i, (n, rel)) in nodes.iter().zip(layout(nodes)).enumerate() {
                let Some(rel) = rel else { continue };
                let parent = if i == 0 { dest.id.clone() } else { ids.get(n.parent_id.as_deref().unwrap_or_default()).cloned().unwrap_or_default() };
                let Ok(s) = below(&final_path, &rel).and_then(|p| stat(p.as_path())) else { continue };
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
        Placed::Store(staged, _) => {
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
                    extras.extend(tree::commit_blob(&mut tx, s).await?);
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
    async fn nothing_is_written_into_a_folder_that_isnt_the_spaces_own() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let space = env.folder_space("Scans").await;
        let (dir, drive) = (&space.dir, &space.drive);
        let marker = dir.join(crate::folders::MARKER);
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), *drive);
        let away = dir.with_extension("away");
        std::fs::rename(dir, &away).unwrap();
        let refused = |what: &'static str| {
            let (env, admin, root) = (&env, admin.clone(), space.root.clone());
            async move {
                let err = env.try_upload(&admin, &root, "a.txt", b"a").await.unwrap_err();
                assert!(err.message.starts_with(crate::storage::NOT_MOUNTED), "{what}: {}", err.message);
                let req = req(json!({ "parent_id": root, "name": "Docs" }));
                let err = crate::nodes::create_folder(State(env.st.clone()), admin, req).await.unwrap_err();
                assert_eq!((err.status, err.message.as_str()), (StatusCode::SERVICE_UNAVAILABLE, crate::storage::NOT_MOUNTED), "{what}");
            }
        };
        // Missing: not made again
        refused("missing").await;
        assert!(!dir.exists());
        // Another disk mounted there, with a folder of the same name: without the marker, or with another space's
        for other in [None, Some("another space")] {
            std::fs::create_dir(dir).unwrap();
            if let Some(id) = other {
                std::fs::write(&marker, id).unwrap();
            }
            refused("another disk").await;
            let report = crate::folders::scan(&env.st, drive).await.unwrap();
            let names: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name()).collect();
            if other.is_some() {
                assert!(report.error.is_some(), "a scan leaves another space's folder alone");
                assert_eq!(names, [crate::folders::MARKER]);
            }
            std::fs::remove_dir_all(dir).unwrap();
        }
        // Its own folder back: changes are made again
        std::fs::rename(&away, dir).unwrap();
        env.upload(&admin, &space.root, "a.txt", b"a").await;
        assert!(dir.join("a.txt").is_file());
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
        let folder = folder.root.join(&folder.rel);
        assert!(folder.is_dir());
        let _ = crate::nodes::delete_forever(st(), admin.clone(), req(json!({ "ids": [a] }))).await.unwrap();
        assert!(eventually_gone(&folder).await);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_folder_swapped_for_a_link_leads_nowhere() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        let (dir, drive) = (&space.dir, &space.drive);
        let st = || State(env.st.clone());
        write_old(&dir.join("Sub/drive.db"), b"indexed");
        write_old(&dir.join("a.txt"), b"a");
        crate::folders::scan(&env.st, drive).await.unwrap();
        let (file, _) = env.node_at(drive, "Sub/drive.db").await.unwrap();
        let (sub, _) = env.node_at(drive, "Sub").await.unwrap();
        let (a, _) = env.node_at(drive, "a.txt").await.unwrap();

        // Someone with access to the folder (over SMB, say) replaces the indexed folder with a link elsewhere
        let outside = dir.with_extension("outside");
        write_old(&outside.join("drive.db"), b"SECRET");
        std::fs::rename(dir.join("Sub"), dir.with_extension("was-sub")).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("Sub")).unwrap();

        let q = Query(serde_json::from_value(json!({})).unwrap());
        let read = crate::files::content(st(), admin.clone(), UrlPath(file.clone()), q, HeaderMap::new()).await;
        assert!(read.is_err(), "read through the link");
        assert!(crate::nodes::create_folder(st(), admin.clone(), req(json!({ "parent_id": sub, "name": "New" }))).await.is_err());
        assert!(crate::nodes::move_nodes(st(), admin.clone(), req(json!({ "ids": [a], "dest_id": sub }))).await.is_err());
        assert!(crate::nodes::trash(st(), admin.clone(), req(json!({ "ids": [file] }))).await.is_err());
        let mut left: Vec<String> = std::fs::read_dir(&outside).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["drive.db"], "nothing was written there");
        assert_eq!(std::fs::read(outside.join("drive.db")).unwrap(), b"SECRET");
        assert!(dir.join("a.txt").is_file());
        // A scan doesn't index what the link leads to
        let r = crate::folders::scan(&env.st, drive).await.unwrap();
        assert!(r.skipped.iter().any(|s| s.contains("symbolic link")), "{r:?}");
        let _ = std::fs::remove_dir_all(&outside);
        let _ = std::fs::remove_dir_all(dir.with_extension("was-sub"));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_space_folder_swapped_for_a_link_leads_nowhere() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        let (dir, drive) = (&space.dir, &space.drive);
        write_old(&dir.join("drive.db"), b"indexed");
        crate::folders::scan(&env.st, drive).await.unwrap();
        let (file, _) = env.node_at(drive, "drive.db").await.unwrap();

        // Someone who can write where the space's folder is (over SMB, say) replaces it with a link to another folder
        let (outside, was) = (dir.with_extension("outside"), dir.with_extension("was"));
        write_old(&outside.join("drive.db"), b"SECRET");
        write_old(&outside.join("secret.key"), b"KEY");
        std::fs::rename(dir, &was).unwrap();
        std::os::unix::fs::symlink(&outside, dir).unwrap();
        let q = Query(serde_json::from_value(json!({})).unwrap());
        let read = crate::files::content(State(env.st.clone()), admin.clone(), UrlPath(file.clone()), q, HeaderMap::new()).await;
        assert!(read.is_err(), "read through the link");
        // Neither a scan nor opening the folder indexes what the link leads to
        let r = crate::folders::scan(&env.st, drive).await.unwrap();
        assert!(r.error.is_some(), "{r:?}");
        crate::folders::sync_folder(&env.st, &node(&env, &space.root).await).await;
        assert!(env.node_at(drive, "secret.key").await.is_none());
        assert_eq!(env.node_at(drive, "drive.db").await.unwrap().0, file);
        assert!(!outside.join(crate::folders::MARKER).exists(), "nothing is written there");

        // The folder back, but its marker gone (deleted over SMB, say): the scan recognises the folder and marks it again
        std::fs::remove_file(dir).unwrap();
        std::fs::rename(&was, dir).unwrap();
        std::fs::remove_file(dir.join(crate::folders::MARKER)).unwrap();
        let r = crate::folders::scan(&env.st, drive).await.unwrap();
        assert!(r.error.is_none(), "{r:?}");
        assert_eq!(std::fs::read_to_string(dir.join(crate::folders::MARKER)).unwrap(), *drive);
        assert_eq!(content(&env, &admin, &file).await, b"indexed");
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[tokio::test]
    async fn saving_and_restoring_versions_keep_to_the_space_size_limit() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        write_old(&space.dir.join("notes.txt"), b"one");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (id, _) = env.node_at(&space.drive, "notes.txt").await.unwrap();
        let limit = |bytes: i64| sqlx::query("UPDATE drives SET quota_bytes = ? WHERE id = ?").bind(bytes).bind(&space.drive).execute(&env.st.db);
        let save = |body: &'static [u8]| crate::files::save_content(State(env.st.clone()), admin.clone(), UrlPath(id.clone()), HeaderMap::new(), Bytes::from_static(body));
        limit(10).await.unwrap();
        assert!(save(b"far more than ten bytes").await.is_err(), "over the limit");
        assert_eq!(std::fs::read(space.dir.join("notes.txt")).unwrap(), b"one");
        let _ = save(b"0123456789").await.unwrap();
        let _ = save(b"x").await.unwrap();
        // The earlier ten bytes don't fit once the limit is lower
        limit(5).await.unwrap();
        let axum::Json(list) = versions::list(State(env.st.clone()), admin.clone(), UrlPath(id.clone())).await.unwrap();
        let list = serde_json::to_value(&list).unwrap();
        let ten = list.as_array().unwrap().iter().find(|v| v["size"] == 10).unwrap()["id"].as_str().unwrap().to_string();
        assert!(versions::restore(State(env.st.clone()), admin.clone(), UrlPath((id.clone(), ten))).await.is_err());
        assert_eq!(std::fs::read(space.dir.join("notes.txt")).unwrap(), b"x");
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
        let _ = crate::nodes::move_nodes(st(), admin.clone(), req(json!({ "ids": [sub], "dest_id": admin.root() }))).await.unwrap();
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
        let _ = crate::nodes::copy_nodes(st(), admin.clone(), req(json!({ "ids": [x], "dest_id": admin.root() }))).await.unwrap();
        let (copy,): (String,) =
            sqlx::query_as("SELECT id FROM nodes WHERE parent_id = ? AND name = 'x.txt'").bind(admin.root()).fetch_one(&env.st.db).await.unwrap();
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
        let pin = |p: &Path| Pinned::root(p.parent().unwrap()).unwrap().join(p.file_name().unwrap().to_str().unwrap()).unwrap();
        let mut items = Vec::new();
        copy_tree(&pin(&from), &pin(&to), "", &mut items, &Tracker::default()).unwrap();
        assert_eq!(std::fs::read(to.join("Sub/edited.txt")).unwrap(), b"before");
        // Saved over SMB while the copy ran, and a file added
        std::fs::write(from.join("Sub/edited.txt"), b"after, and longer").unwrap();
        write_old(&from.join("new.txt"), b"new");
        remove_copied(CopiedTree { top: pin(&from), items });
        assert!(!from.join("same.txt").exists());
        assert_eq!(std::fs::read(from.join("Sub/edited.txt")).unwrap(), b"after, and longer");
        assert!(from.join("new.txt").is_file());
        let _ = std::fs::remove_dir_all(&top);
    }

    #[test]
    fn what_a_stopped_change_left_behind_is_put_back_or_removed() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-leftovers-{}", new_id()));
        let (moving, upload) = (format!("{MOVE_PREFIX}{}", new_id()), format!("{UPLOAD_PREFIX}{}", new_id()));
        write_old(&dir.join(&moving).join("Report/a.txt"), b"moving");
        write_old(&dir.join("Report"), b"already taken");
        write_old(&dir.join(&upload), b"half an upload");
        assert!(is_leftover(&moving) && is_leftover(&format!("{SAVE_PREFIX}{}", new_id())) && !is_leftover(TRASH_DIR));
        let found = [dir.join(&moving), dir.join(&upload)];
        let root = Pinned::root(&dir).unwrap();
        let pinned = || vec![root.join(&moving).unwrap(), root.join(&upload).unwrap()];
        clean_leftovers(pinned(), TRASH_GRACE);
        assert!(found.iter().all(|p| p.exists()), "recent ones may still be in use");
        clean_leftovers(pinned(), std::time::Duration::ZERO);
        assert_eq!(std::fs::read(dir.join("Report (1)/a.txt")).unwrap(), b"moving");
        assert!(found.iter().all(|p| !p.exists()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn names_scans_skip_are_refused_in_folder_spaces() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        let st = || State(env.st.clone());
        write_old(&space.dir.join("app.py"), b"print()");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (app, _) = env.node_at(&space.drive, "app.py").await.unwrap();
        for name in [
            ".thirtyfile-save-x",
            ".thirtyfile-upload-x",
            ".thirtyfile-trash",
            ".THIRTYFILE-versions",
            ".thirtyfile-space",
            "~$Budget.xlsx",
            ".~lock.Budget.xlsx#",
            "movie.mp4.part",
            "setup.crdownload",
            "Thumbs.db",
            "desktop.ini",
            ".DS_Store",
            ".smbdelete0001",
        ] {
            let err = crate::nodes::create_folder(st(), admin.clone(), req(json!({ "parent_id": space.root, "name": name }))).await.unwrap_err();
            assert_eq!(err.status, StatusCode::BAD_REQUEST, "{name}: {}", err.message);
            let err = crate::nodes::rename(st(), admin.clone(), UrlPath(app.clone()), req(json!({ "name": name }))).await.unwrap_err();
            assert_eq!(err.status, StatusCode::BAD_REQUEST, "{name}: {}", err.message);
            let name: &'static str = name.to_string().leak();
            assert!(env.try_upload(&admin, &space.root, name, b"x").await.is_err(), "{name}");
        }
        let names: Vec<String> = std::fs::read_dir(&space.dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        let mut names = names;
        names.sort();
        assert_eq!(names, [crate::folders::MARKER, "app.py"]);
        assert_eq!(node(&env, &app).await.name, "app.py");

        // Nor can they come in from the content store, where they are ordinary names
        let docs = env.folder(&admin, admin.root(), "Docs").await;
        env.upload(&admin, &docs, "desktop.ini", b"[.ShellClassInfo]").await;
        let err = crate::nodes::copy_nodes(st(), admin.clone(), req(json!({ "ids": [docs], "dest_id": space.root }))).await.unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST, "{}", err.message);
        let err = crate::nodes::move_nodes(st(), admin.clone(), req(json!({ "ids": [docs], "dest_id": space.root }))).await.unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST, "{}", err.message);
        assert_eq!(env.drive_of(&docs).await, env.drive_of(admin.root()).await);
        assert!(!space.dir.join("Docs").exists());
        // Names that only look alike are fine
        env.upload(&admin, &space.root, "part.txt", b"x").await;
        env.upload(&admin, &space.root, "thirtyfile-notes.txt", b"x").await;
    }

    #[tokio::test]
    async fn only_what_thirtyfile_left_behind_is_cleaned_up() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let dir = &space.dir;
        let old = |path: &Path| {
            let two_days = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 24 * 3600);
            std::fs::File::options().write(true).open(path).unwrap().set_modified(two_days).unwrap();
        };
        // Named by people (over SMB, say) like ThirtyFile's own files, and old
        let people = [
            ".thirtyfile-save-x".to_string(),
            format!("{UPLOAD_PREFIX}notes.txt"),
            format!("{MOVE_PREFIX}{}", "A".repeat(32)),
            format!("{TRASH_DIR}/Notes/a.txt"),
            format!("{}/Diary/2026.txt", versions::VERSIONS_DIR),
        ];
        for p in &people {
            write_old(&dir.join(p), b"kept");
            old(&dir.join(p));
        }
        for p in [format!("{TRASH_DIR}/Notes"), format!("{}/Diary", versions::VERSIONS_DIR)] {
            let two_days = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 24 * 3600);
            let _ = std::fs::File::open(dir.join(p)).and_then(|f| f.set_modified(two_days));
        }
        // Left by ThirtyFile
        let upload = format!("{UPLOAD_PREFIX}{}", new_id());
        write_old(&dir.join(&upload), b"half an upload");
        old(&dir.join(&upload));
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        assert!(eventually_gone(&dir.join(&upload)).await);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        for p in &people {
            assert_eq!(std::fs::read(dir.join(p)).unwrap(), b"kept", "{p}");
        }
    }

    /// Waits until the removal of this content from the built-in location is scheduled
    async fn removal_scheduled(env: &testutil::TestEnv, content: &[u8]) -> bool {
        let hash = crate::util::sha256_hex(content);
        for _ in 0..100 {
            let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pending_blob_deletes WHERE hash = ? AND location_id = 'local'")
                .bind(&hash)
                .fetch_one(&env.st.db)
                .await
                .unwrap();
            if n > 0 {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        false
    }

    #[tokio::test]
    async fn a_file_changed_while_it_was_being_stored_stays_in_the_folder() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        write_old(&space.dir.join("Sub/same.txt"), b"same");
        write_old(&space.dir.join("Sub/edited.txt"), b"before");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (sub, _) = env.node_at(&space.drive, "Sub").await.unwrap();
        let (edited, _) = env.node_at(&space.drive, "Sub/edited.txt").await.unwrap();
        // Saved over SMB after it was stored in the content store, before the originals are removed
        let path = space.dir.join("Sub/edited.txt");
        let _hook = hook_after_place(move || {
            let path = path.clone();
            Box::pin(async move { std::fs::write(&path, b"after, and longer").unwrap() })
        });
        let _ = crate::nodes::move_nodes(State(env.st.clone()), admin.clone(), req(json!({ "ids": [sub], "dest_id": admin.root() }))).await.unwrap();
        assert!(eventually_gone(&space.dir.join("Sub/same.txt")).await);
        assert_eq!(std::fs::read(space.dir.join("Sub/edited.txt")).unwrap(), b"after, and longer");
        assert_eq!(content(&env, &admin, &edited).await, b"before");
    }

    #[tokio::test]
    async fn items_stay_in_a_space_that_started_moving_to_another_location_meanwhile() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        write_old(&space.dir.join("a.txt"), b"a");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (a, _) = env.node_at(&space.drive, "a.txt").await.unwrap();
        // The space starts moving to another storage location while the item's content is being copied
        let (st, drive) = (env.st.clone(), space.drive.clone());
        let _hook = hook_after_place(move || {
            let (st, drive) = (st.clone(), drive.clone());
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO space_moves (id, drive_id, space_name, space_kind, from_mode, to_location, to_mode, state, created_at)
                     VALUES ('m', ?, 'Shared', 'team', 'folder', 'local', 'store', 'running', 0)",
                )
                .bind(&drive)
                .execute(&st.db)
                .await
                .unwrap();
                sqlx::query("UPDATE drives SET moving = 1 WHERE id = ?").bind(drive).execute(&st.db).await.unwrap();
            })
        });
        let res = crate::nodes::move_nodes(State(env.st.clone()), admin.clone(), req(json!({ "ids": [a], "dest_id": admin.root() }))).await;
        assert!(res.is_err());
        assert_eq!(env.drive_of(&a).await, space.drive);
        assert_eq!(node(&env, &a).await.fs_path.as_deref(), Some("a.txt"));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(space.dir.join("a.txt").is_file());
    }

    #[tokio::test]
    async fn a_move_to_another_space_goes_on_when_the_request_is_given_up() {
        let env = testutil::env().await;
        let one = env.folder_space("One").await;
        let two = env.folder_space("Two").await;
        let admin = env.admin().await;
        write_old(&one.dir.join("Sub/x.txt"), b"x");
        crate::folders::scan(&env.st, &one.drive).await.unwrap();
        let (sub, _) = env.node_at(&one.drive, "Sub").await.unwrap();
        // The content takes a while to copy: longer than the browser waits
        let go = std::sync::Arc::new(tokio::sync::Notify::new());
        let _hook = hook_after_place(wait_for(&go));
        OTHER_DISK.with(|d| d.set(true));
        let asked = crate::nodes::move_nodes(State(env.st.clone()), admin.clone(), req(json!({ "ids": [sub], "dest_id": two.root })));
        // The browser (or a proxy) gives up on the request: the move goes on regardless
        let _ = tokio::time::timeout(std::time::Duration::from_millis(300), asked).await;
        go.notify_one();
        let mut moved = false;
        for _ in 0..200 {
            if env.drive_of(&sub).await == two.drive {
                moved = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        OTHER_DISK.with(|d| d.set(false));
        assert!(moved, "the move finished");
        assert_eq!(std::fs::read(two.dir.join("Sub/x.txt")).unwrap(), b"x");
        assert!(eventually_gone(&one.dir.join("Sub")).await);
        let hidden: Vec<String> =
            std::fs::read_dir(&two.dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with(".thirtyfile-m") || n.starts_with(".thirtyfile-c")).collect();
        assert!(hidden.is_empty(), "nothing is left behind: {hidden:?}");

        // A copy inside a folder space, the same
        let asked = crate::nodes::copy_nodes(State(env.st.clone()), admin.clone(), req(json!({ "ids": [sub], "dest_id": two.root })));
        let _ = tokio::time::timeout(std::time::Duration::from_millis(300), asked).await;
        go.notify_one();
        let mut copied = false;
        for _ in 0..200 {
            if env.node_at(&two.drive, "Sub (1)/x.txt").await.is_some() {
                copied = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(copied, "the copy finished");
        assert_eq!(std::fs::read(two.dir.join("Sub (1)/x.txt")).unwrap(), b"x");
    }

    #[tokio::test]
    async fn a_move_to_another_disk_copies_under_a_name_that_is_removed_should_it_stop() {
        let env = testutil::env().await;
        let one = env.folder_space("One").await;
        let two = env.folder_space("Two").await;
        let admin = env.admin().await;
        write_old(&one.dir.join("Sub/x.txt"), b"x");
        crate::folders::scan(&env.st, &one.drive).await.unwrap();
        let (sub, _) = env.node_at(&one.drive, "Sub").await.unwrap();
        // While the copy waits for the index, the destination holds it under a copy's name: should ThirtyFile stop
        // now, the copy is removed later (`clean_leftovers`) rather than put in place next to the original, which is
        // still in "One"
        let (dir, seen) = (two.dir.clone(), std::sync::Arc::new(std::sync::Mutex::new(Vec::new())));
        let names = seen.clone();
        let _hook = hook_after_place(move || {
            let (dir, names) = (dir.clone(), names.clone());
            Box::pin(async move {
                let found = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned());
                names.lock().unwrap().extend(found.filter(|n| n.starts_with(".thirtyfile-") && n != crate::folders::MARKER));
            })
        });
        OTHER_DISK.with(|d| d.set(true));
        let moved = crate::nodes::move_nodes(State(env.st.clone()), admin.clone(), req(json!({ "ids": [sub], "dest_id": two.root }))).await;
        OTHER_DISK.with(|d| d.set(false));
        assert_eq!(moved.unwrap().0.state, "done");
        let seen = seen.lock().unwrap().clone();
        assert!(seen.len() == 1 && seen[0].starts_with(COPY_PREFIX), "{seen:?}");
        assert!(two.dir.join("Sub/x.txt").is_file());
    }

    #[tokio::test]
    async fn earlier_versions_in_the_content_store_go_with_a_file_deleted_on_the_server() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        // A file of the content store, with an earlier version there, moved into the folder space
        let notes = env.upload(&admin, admin.root(), "notes.txt", b"version one").await;
        let _ = crate::files::save_content(State(env.st.clone()), admin.clone(), UrlPath(notes.clone()), HeaderMap::new(), Bytes::from_static(b"two"))
            .await
            .unwrap();
        let _ = crate::nodes::move_nodes(State(env.st.clone()), admin.clone(), req(json!({ "ids": [notes], "dest_id": space.root }))).await.unwrap();
        let (kept,): (Option<String>,) = sqlx::query_as("SELECT blob_hash FROM node_versions WHERE node_id = ?").bind(&notes).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(kept, Some(crate::util::sha256_hex(b"version one")));
        // Deleted on the server
        std::fs::remove_file(space.dir.join("notes.txt")).unwrap();
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        assert!(removal_scheduled(&env, b"version one").await);

        // The same when the file is gone from the server while its folder is moved to another folder space
        let two = env.folder_space("Two").await;
        let docs = env.folder(&admin, admin.root(), "Docs").await;
        let other = env.upload(&admin, &docs, "other.txt", b"other, one").await;
        let _ = crate::files::save_content(State(env.st.clone()), admin.clone(), UrlPath(other.clone()), HeaderMap::new(), Bytes::from_static(b"two"))
            .await
            .unwrap();
        let _ = crate::nodes::move_nodes(State(env.st.clone()), admin.clone(), req(json!({ "ids": [docs], "dest_id": space.root }))).await.unwrap();
        let dir = two.dir.clone();
        let _hook = hook_after_place(move || {
            let dir = dir.clone();
            Box::pin(async move {
                for e in std::fs::read_dir(&dir).unwrap().flatten() {
                    if e.file_name().to_string_lossy().starts_with(MOVE_PREFIX) {
                        std::fs::remove_file(e.path().join("Docs/other.txt")).unwrap();
                    }
                }
            })
        });
        let _ = crate::nodes::move_nodes(State(env.st.clone()), admin.clone(), req(json!({ "ids": [docs], "dest_id": two.root }))).await.unwrap();
        assert!(two.dir.join("Docs").is_dir());
        assert!(tree::get_node(&mut env.st.db.acquire().await.unwrap(), &other).await.unwrap().is_none());
        assert!(removal_scheduled(&env, b"other, one").await);
    }
}
