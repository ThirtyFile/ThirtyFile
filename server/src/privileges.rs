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
    // SAFETY: plain system calls; still single-threaded at this point, so every thread gets the new ids
    unsafe {
        if libc::setgroups(1, &gid) != 0 || libc::setgid(gid) != 0 || libc::setuid(uid) != 0 {
            return Err(format!("Can't switch to user {uid}:{gid}: {}", io::Error::last_os_error()).into());
        }
    }
    Ok(())
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
}
