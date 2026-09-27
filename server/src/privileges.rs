//! Running as an unprivileged user in the Docker image.
//!
//! The image starts as root so that it can take over data folders that Docker created as root
//! (host folders in `compose.yaml` that didn't exist yet). It gives the folders to the user in
//! `THIRTYFILE_RUN_AS`, then switches to that user before anything else runs.

use std::{
    io,
    os::unix::fs::{MetadataExt, lchown},
    path::Path,
};

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

/// Changes the owner of `path` and everything below it, without following symbolic links
/// and without entering other file systems mounted inside it.
fn chown_tree(path: &Path, dev: u64, uid: u32, gid: u32) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.dev() != dev {
        return Ok(());
    }
    if (meta.uid(), meta.gid()) != (uid, gid) {
        lchown(path, Some(uid), Some(gid))?;
    }
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            chown_tree(&entry?.path(), dev, uid, gid)?;
        }
    }
    Ok(())
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
    fn extra_groups_are_kept_and_roots_groups_left_behind() {
        let etc = "root:x:0:root\nbin:x:1:root,bin,daemon\ndisk:x:6:root,adm\nusers:x:100:games\nnas:x:2001:\n";
        let roots = root_groups(etc);
        assert_eq!(roots, [0, 1, 6]);
        // Started as root with its image groups plus --group-add 2001 and 100
        assert_eq!(extra_groups(1000, &[0, 1, 6, 2001, 100, 1000], &roots), [1000, 2001, 100]);
    }
}
