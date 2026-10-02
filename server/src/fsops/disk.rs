//! The disk: renaming without replacing, copying, reading an item's details

use super::*;

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

pub(super) fn same_item(a: &Path, b: &Path) -> bool {
    match (stat(a), stat(b)) {
        (Ok(x), Ok(y)) if x.ino != 0 => (x.dev, x.ino) == (y.dev, y.ino),
        _ => a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()),
    }
}

/// `rel` below `p` ("" for `p` itself), without following a link on the way
pub(super) fn below(p: &Pinned, rel: &str) -> io::Result<Pinned> {
    if rel.is_empty() { Ok(p.clone()) } else { p.join(rel) }
}

/// An original that was copied: removed afterwards only if it is still what was copied
pub(super) struct Copied {
    /// Below the item that was copied ("" for the item itself)
    pub(super) rel: String,
    pub(super) is_dir: bool,
    /// A file's size and modification time when it was copied
    pub(super) seen: Option<(u64, i64)>,
}

/// The originals of a move to another disk: the item, and what was copied of it
pub(super) struct CopiedTree {
    pub(super) top: Pinned,
    pub(super) items: Vec<Copied>,
}

/// Copies a file or folder on disk, including what the index doesn't have yet; symbolic links stay links. `copied`
/// lists the originals (by their path below the item), folders before what is in them.
pub(super) fn copy_tree(from: &Pinned, to: &Pinned, rel: &str, copied: &mut Vec<Copied>, progress: &Tracker) -> io::Result<()> {
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
pub(super) fn remove_copied(copied: CopiedTree) {
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

pub(super) fn remove_all(p: &Pinned) -> io::Result<()> {
    // Neither follows a symbolic link: a link is removed itself
    if std::fs::symlink_metadata(p.as_path())?.is_dir() { std::fs::remove_dir_all(p.as_path()) } else { std::fs::remove_file(p.as_path()) }
}

pub(super) fn older_than(p: &Pinned, age: std::time::Duration) -> bool {
    std::fs::symlink_metadata(p.as_path()).and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|a| a >= age))
}

/// Puts the content of the file just written at `path` on disk, before a name leads to it: after a power loss, the
/// name never leads to content cut short
pub fn sync_file(file: &std::fs::File, path: &Path) -> io::Result<()> {
    file.sync_all()?;
    #[cfg(test)]
    testing::synced(path);
    let _ = path;
    Ok(())
}

/// Makes a folder, or uses the one already there (not a link)
pub fn ensure_dir(p: &Pinned) -> io::Result<()> {
    match std::fs::create_dir(p.as_path()) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && std::fs::symlink_metadata(p.as_path()).is_ok_and(|m| m.is_dir()) => Ok(()),
        r => r,
    }
}

/// Removes items below a space's folder from disk in the background (the index no longer has them)
pub fn remove_below_later(paths: Vec<crate::folders::Below>) {
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
