//! Paths inside a folder space's folder that can't lead outside it.
//!
//! People can change a folder space's folder outside ThirtyFile (over SMB, say), including replacing a folder with a
//! symbolic link to somewhere else after it was indexed. A path built from the space's folder and an indexed path
//! would then follow that link. `Pinned` opens each folder on the way itself, refusing symbolic links, and keeps the
//! last one open: what it names is looked up in that open folder (through `/proc/self/fd`), so swapping any folder on
//! the way afterwards changes nothing. The last part itself may still be a link: reading uses `open_file`, which
//! refuses one, and renaming or removing acts on the link, never on what it points to.
//!
//! Elsewhere than Linux (development only) it is an ordinary path.

use std::{
    io,
    path::{Path, PathBuf},
};

#[cfg(target_os = "linux")]
use std::{os::fd::OwnedFd, sync::Arc};

/// A file or folder inside a folder space's folder
#[derive(Clone, Debug)]
pub struct Pinned {
    /// The folder it is in, open (None for the path of an open folder itself)
    #[cfg(target_os = "linux")]
    dir: Arc<OwnedFd>,
    /// Its name in that folder; None: the open folder itself
    name: Option<String>,
    /// How the system reaches it: `/proc/self/fd/<dir>/<name>` (elsewhere: an ordinary path)
    path: PathBuf,
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, format!("not a path inside the folder: {what}"))
}

/// The parts of a path below a folder ('/' between them), refusing anything that could step out of it
fn parts(rel: &str) -> io::Result<Vec<&str>> {
    if rel.is_empty() {
        return Ok(Vec::new());
    }
    let parts: Vec<&str> = rel.split('/').collect();
    if rel.starts_with('/') || parts.iter().any(|p| p.is_empty() || *p == "." || *p == ".." || p.contains('\0')) {
        return Err(invalid(rel));
    }
    Ok(parts)
}

#[cfg(target_os = "linux")]
mod sys {
    use std::{
        ffi::CString,
        io,
        os::fd::{AsRawFd, FromRawFd, OwnedFd},
        path::Path,
    };

    fn open_at(dir: Option<&OwnedFd>, name: &[u8], flags: libc::c_int) -> io::Result<OwnedFd> {
        let c = CString::new(name).map_err(|_| super::invalid("a name with a NUL"))?;
        let at = dir.map_or(libc::AT_FDCWD, |d| d.as_raw_fd());
        // SAFETY: a valid NUL-terminated name and an open folder (or the working directory)
        let fd = unsafe { libc::openat(at, c.as_ptr(), flags | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a file descriptor just opened, owned by nobody else
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Opens a folder for looking things up in it (following links: a space's folder is set by an administrator)
    pub fn open_root(path: &Path) -> io::Result<OwnedFd> {
        use std::os::unix::ffi::OsStrExt;
        open_at(None, path.as_os_str().as_bytes(), libc::O_PATH | libc::O_DIRECTORY)
    }

    /// Opens the folder `name` inside `dir`, refusing a symbolic link
    pub fn open_dir(dir: &OwnedFd, name: &str) -> io::Result<OwnedFd> {
        open_at(Some(dir), name.as_bytes(), libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW)
    }

    /// Opens the file `name` inside `dir` for reading, refusing a symbolic link
    pub fn open_file(dir: &OwnedFd, name: &str) -> io::Result<std::fs::File> {
        let fd = open_at(Some(dir), name.as_bytes(), libc::O_RDONLY | libc::O_NOFOLLOW)?;
        Ok(std::fs::File::from(fd))
    }

    pub fn proc_path(dir: &OwnedFd) -> std::path::PathBuf {
        std::path::PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()))
    }
}

impl Pinned {
    /// A space's folder
    pub fn root(root: &Path) -> io::Result<Pinned> {
        #[cfg(target_os = "linux")]
        {
            let dir = sys::open_root(root)?;
            let path = sys::proc_path(&dir);
            Ok(Pinned { dir: Arc::new(dir), name: None, path })
        }
        #[cfg(not(target_os = "linux"))]
        {
            if !std::fs::metadata(root)?.is_dir() {
                return Err(io::Error::new(io::ErrorKind::NotFound, "not a folder"));
            }
            Ok(Pinned { name: None, path: root.to_path_buf() })
        }
    }

    /// `rel` (parts with '/' between them) inside this folder. Every folder on the way must be a folder, not a link;
    /// the last part needn't exist.
    pub fn join(&self, rel: &str) -> io::Result<Pinned> {
        let parts = parts(rel)?;
        let Some((last, on_the_way)) = parts.split_last() else { return Ok(self.clone()) };
        #[cfg(target_os = "linux")]
        {
            let mut dir = self.dir()?.dir;
            for p in on_the_way {
                dir = Arc::new(sys::open_dir(&dir, p)?);
            }
            let path = sys::proc_path(&dir).join(last);
            Ok(Pinned { dir, name: Some(last.to_string()), path })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = on_the_way;
            let mut path = self.dir()?.path;
            for p in &parts {
                path.push(p);
            }
            Ok(Pinned { name: Some(last.to_string()), path })
        }
    }

    /// This folder, opened (it must be a folder, not a link)
    pub fn dir(&self) -> io::Result<Pinned> {
        let Some(name) = &self.name else { return Ok(self.clone()) };
        #[cfg(target_os = "linux")]
        {
            let dir = sys::open_dir(&self.dir, name)?;
            let path = sys::proc_path(&dir);
            Ok(Pinned { dir: Arc::new(dir), name: None, path })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = name;
            if std::fs::symlink_metadata(&self.path)?.file_type().is_symlink() {
                return Err(io::Error::new(io::ErrorKind::NotADirectory, "a symbolic link"));
            }
            Ok(Pinned { name: None, path: self.path.clone() })
        }
    }

    /// The folder it is in: None for a folder that was opened itself (`root`, `dir`)
    pub fn parent(&self) -> Option<Pinned> {
        self.name.as_ref()?;
        #[cfg(target_os = "linux")]
        {
            Some(Pinned { dir: self.dir.clone(), name: None, path: sys::proc_path(&self.dir) })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Some(Pinned { name: None, path: self.path.parent()?.to_path_buf() })
        }
    }

    /// Its name in its folder (None for a folder that was opened itself)
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Opens the file for reading, refusing a symbolic link
    pub fn open_file(&self) -> io::Result<std::fs::File> {
        #[cfg(target_os = "linux")]
        {
            match &self.name {
                Some(name) => sys::open_file(&self.dir, name),
                None => Err(io::Error::new(io::ErrorKind::IsADirectory, "a folder")),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            if std::fs::symlink_metadata(&self.path)?.file_type().is_symlink() {
                return Err(io::Error::new(io::ErrorKind::NotFound, "a symbolic link"));
            }
            std::fs::File::open(&self.path)
        }
    }

    /// Opens the file (refusing a symbolic link) and returns a path that opens that same file again as long as the
    /// result is kept, whatever happens to its name meanwhile: for code that opens a file by path several times
    pub fn open_stable(&self) -> io::Result<(std::fs::File, PathBuf)> {
        let f = self.open_file()?;
        #[cfg(target_os = "linux")]
        let path = PathBuf::from(format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(&f)));
        #[cfg(not(target_os = "linux"))]
        let path = self.path.clone();
        Ok((f, path))
    }

    /// The path the system reaches it by, valid while this is kept
    pub fn as_path(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for Pinned {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// A path below a space's folder, looked up (`pin`) only when it is used: long lists of them keep no folder open
#[derive(Clone, Debug)]
pub struct Below {
    pub root: PathBuf,
    pub rel: String,
}

impl Below {
    pub fn new(root: impl Into<PathBuf>, rel: impl Into<String>) -> Below {
        Below { root: root.into(), rel: rel.into() }
    }

    pub fn pin(&self) -> io::Result<Pinned> {
        Pinned::root(&self.root)?.join(&self.rel)
    }
}

/// Gives the file `to` the permissions `from` has, through the open file (never following a link)
pub fn copy_permissions(from: &Pinned, to: &Pinned) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(from.as_path())?;
    if !meta.is_file() {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    return to.open_file()?.set_permissions(meta.permissions());
    #[cfg(not(target_os = "linux"))]
    std::fs::set_permissions(to.as_path(), meta.permissions())
}

/// Writes a new file (never one already there, nor through a link)
pub fn write_new(to: &Pinned, body: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create_new(to.as_path())?;
    f.write_all(body)?;
    f.sync_all()
}

/// Copies a file, refusing a symbolic link as the source; returns the bytes copied
pub fn copy_file(from: &Pinned, to: &Path) -> io::Result<u64> {
    let mut src = from.open_file()?;
    let mut dst = std::fs::File::create_new(to)?;
    let n = io::copy(&mut src, &mut dst)?;
    dst.sync_all()?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_that_step_out_are_refused() {
        for bad in ["../x", "a/../../x", "/etc/passwd", "a//b", "./a", "a/."] {
            assert!(parts(bad).is_err(), "{bad}");
        }
        assert_eq!(parts("a/b c/d.txt").unwrap(), ["a", "b c", "d.txt"]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_folder_swapped_for_a_link_is_not_followed() {
        let top = std::env::temp_dir().join(format!("thirtyfile-beneath-{}", crate::util::new_id()));
        let (space, outside) = (top.join("space"), top.join("outside"));
        std::fs::create_dir_all(space.join("Sub")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(space.join("Sub/a.txt"), b"inside").unwrap();
        std::fs::write(outside.join("a.txt"), b"secret").unwrap();
        let root = Pinned::root(&space).unwrap();
        assert_eq!(std::io::read_to_string(root.join("Sub/a.txt").unwrap().open_file().unwrap()).unwrap(), "inside");

        // Opened before the swap: still the folder it was
        let pinned = root.join("Sub/a.txt").unwrap();
        std::fs::rename(space.join("Sub"), space.join("Moved")).unwrap();
        std::os::unix::fs::symlink(&outside, space.join("Sub")).unwrap();
        assert_eq!(std::io::read_to_string(pinned.open_file().unwrap()).unwrap(), "inside");
        // Looked up after it: refused
        assert!(root.join("Sub/a.txt").is_err());
        assert!(root.join("Sub").unwrap().dir().is_err());
        // A link as the last part isn't read
        std::os::unix::fs::symlink(outside.join("a.txt"), space.join("link.txt")).unwrap();
        assert!(root.join("link.txt").unwrap().open_file().is_err());
        let _ = std::fs::remove_dir_all(&top);
    }
}
