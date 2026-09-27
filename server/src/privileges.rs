//! Running as an unprivileged user in the Docker image.
//!
//! The image starts as root so that it can take over data folders that Docker created as root
//! (host folders in `compose.yaml` that didn't exist yet). It gives the folders to the user in
//! `THIRTYFILE_RUN_AS`, then switches to that user before anything else runs.

use std::{io, os::unix::fs::MetadataExt, path::Path};

/// Parses `uid` or `uid:gid`.
pub fn parse_user(value: &str) -> Result<(u32, u32), String> {
    let invalid = || format!("THIRTYFILE_RUN_AS must be a user id or user id:group id, not {value:?}");
    let (uid, gid) = match value.split_once(':') {
        Some((u, g)) => (u, g),
        None => (value, value),
    };
    Ok((uid.trim().parse().map_err(|_| invalid())?, gid.trim().parse().map_err(|_| invalid())?))
}

/// Gives the data folders to `uid:gid` and switches to that user. Does nothing when not running as root
/// (for example with `docker run --user`), so the process then keeps the user it was started with.
///
/// Must be called before any other threads are started.
pub fn drop_to(value: &str, folders: &[&Path]) -> Result<(), Box<dyn std::error::Error>> {
    let (uid, gid) = parse_user(value)?;
    // SAFETY: geteuid has no preconditions
    if unsafe { libc::geteuid() } != 0 || uid == 0 {
        return Ok(());
    }
    for &folder in folders {
        std::fs::create_dir_all(folder)?;
        let meta = std::fs::metadata(folder)?;
        // The whole folder is checked only when the folder itself belongs to someone else, so that
        // a normal start doesn't walk through every stored file
        if (meta.uid(), meta.gid()) != (uid, gid) {
            tracing::info!("Giving the folder {} to user {uid}:{gid}", folder.display());
            // Not fatal: a network share may refuse it and still be writable
            if let Err(e) = chown_tree(folder, meta.dev(), uid, gid) {
                tracing::warn!("Can't give the folder {} to user {uid}:{gid}: {e}", folder.display());
            }
        }
    }
    let groups = kept_groups(gid);
    // SAFETY: plain system calls; still single-threaded at this point, so every thread gets the new ids
    unsafe {
        if libc::setgroups(groups.len(), groups.as_ptr()) != 0 || libc::setgid(gid) != 0 || libc::setuid(uid) != 0 {
            return Err(format!("Can't switch to user {uid}:{gid}: {}", io::Error::last_os_error()).into());
        }
    }
    Ok(())
}

/// The groups the server keeps after switching user: `gid`, plus the extra groups the container was started with
/// (`docker run --group-add`, `group_add:` in compose), which people use to reach NAS shares by group. The groups root
/// has in the image itself (root, and the ones /etc/group lists it in, such as disk) are left behind.
fn kept_groups(gid: u32) -> Vec<libc::gid_t> {
    // SAFETY: the first call only counts; the second fills a buffer of that size
    let current: Vec<libc::gid_t> = unsafe {
        let n = libc::getgroups(0, std::ptr::null_mut());
        let mut list = vec![0; n.max(0) as usize];
        let n = libc::getgroups(n, list.as_mut_ptr());
        list.truncate(n.max(0) as usize);
        list
    };
    let roots = std::fs::read_to_string("/etc/group").map(|g| root_groups(&g)).unwrap_or_default();
    extra_groups(gid, &current, &roots)
}

/// Groups /etc/group lists root in
fn root_groups(etc_group: &str) -> Vec<u32> {
    etc_group
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.len() >= 4 && f[3].split(',').any(|m| m.trim() == "root")).then(|| f[2].parse().ok()).flatten()
        })
        .collect()
}

fn extra_groups(gid: u32, current: &[u32], roots: &[u32]) -> Vec<u32> {
    let mut out = vec![gid];
    for &g in current {
        if g != 0 && !roots.contains(&g) && !out.contains(&g) {
            out.push(g);
        }
    }
    out
}

/// Changes the owner of `path` and everything below it, without following symbolic links and without entering
/// other file systems mounted inside it.
///
/// The walk works on open directories (`openat` with `O_NOFOLLOW`, `fchownat` without following links), not on
/// paths: a directory swapped for a symbolic link while the walk runs can't send it outside the folder.
fn chown_tree(path: &Path, dev: u64, uid: u32, gid: u32) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
    // SAFETY: a valid NUL-terminated path; the descriptor is owned by `walk`, which closes it
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    walk(fd, dev, uid, gid)
}

/// Gives the open directory `fd` and its contents to `uid:gid`; takes ownership of `fd`
fn walk(fd: libc::c_int, dev: u64, uid: u32, gid: u32) -> io::Result<()> {
    // SAFETY: `fd` is an open directory; fdopendir takes it over and closedir closes both
    let dir = unsafe { libc::fdopendir(fd) };
    if dir.is_null() {
        let e = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    struct Dir(*mut libc::DIR);
    impl Drop for Dir {
        fn drop(&mut self) {
            // SAFETY: opened by fdopendir above, closed once
            unsafe { libc::closedir(self.0) };
        }
    }
    let dir = Dir(dir);
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` stays valid while `dir` is open
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if st.st_dev as u64 != dev {
        return Ok(());
    }
    if (st.st_uid, st.st_gid) != (uid, gid) && unsafe { libc::fchown(fd, uid, gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    loop {
        // SAFETY: readdir on an open DIR; the entry is valid until the next call, and its name is copied first
        let entry = unsafe { libc::readdir(dir.0) };
        if entry.is_null() {
            return Ok(());
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_owned();
        if matches!(name.as_bytes(), b"." | b"..") {
            continue;
        }
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is open and `name` a NUL-terminated entry name
        if unsafe { libc::fstatat(fd, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if st.st_dev as u64 != dev {
            continue;
        }
        if st.st_mode & libc::S_IFMT == libc::S_IFDIR {
            // SAFETY: as above; O_NOFOLLOW refuses a link that replaced the directory since fstatat
            let child = unsafe { libc::openat(fd, name.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
            if child < 0 {
                return Err(io::Error::last_os_error());
            }
            walk(child, dev, uid, gid)?;
        } else if (st.st_uid, st.st_gid) != (uid, gid)
            // SAFETY: as above; the link itself is changed, never what it points to
            && unsafe { libc::fchownat(fd, name.as_ptr(), uid, gid, libc::AT_SYMLINK_NOFOLLOW) } != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_user_and_group() {
        assert_eq!(parse_user("1000"), Ok((1000, 1000)));
        assert_eq!(parse_user("1000:100"), Ok((1000, 100)));
        assert!(parse_user("drive").is_err());
        assert!(parse_user("1000:").is_err());
    }

    #[test]
    fn the_ownership_walk_stays_inside_the_folder() {
        // Changing owners needs root (the Docker image starts as root); elsewhere there is nothing to check
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
        let base = std::env::temp_dir().join(format!("thirtyfile-test-{}", crate::util::new_id()));
        let (inside, outside) = (base.join("data"), base.join("outside"));
        std::fs::create_dir_all(inside.join("a/b")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(inside.join("a/b/file.txt"), "x").unwrap();
        std::fs::write(outside.join("secret.txt"), "x").unwrap();
        std::os::unix::fs::symlink(&outside, inside.join("link")).unwrap();
        let dev = std::fs::metadata(&inside).unwrap().dev();
        chown_tree(&inside, dev, 4321, 4321).unwrap();
        let owner = |p: &Path| {
            let m = std::fs::symlink_metadata(p).unwrap();
            (m.uid(), m.gid())
        };
        assert_eq!(owner(&inside.join("a/b/file.txt")), (4321, 4321));
        assert_eq!(owner(&inside.join("a")), (4321, 4321));
        assert_eq!(owner(&inside.join("link")), (4321, 4321), "the link itself changes");
        assert_eq!(owner(&outside.join("secret.txt")), (0, 0), "what it points to doesn't");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn extra_groups_are_kept_and_roots_groups_left_behind() {
        let etc = "root:x:0:root\nbin:x:1:root,bin,daemon\ndisk:x:6:root,adm\nusers:x:100:games\nnas:x:2001:\n";
        let roots = root_groups(etc);
        assert_eq!(roots, [0, 1, 6]);
        // Started as root with its image groups plus --group-add 2001 and 100
        assert_eq!(extra_groups(1000, &[0, 1, 6, 2001, 100, 1000], &roots), [1000, 2001, 100]);
    }
}
