//! Keeping a folder space's own files outside its folder (`THIRTYFILE_FOLDER_STATE`).
//!
//! A folder space keeps four things of its own in its folder: the marker (`.thirtyfile-space`), the journal of changes
//! under way, the earlier versions of files and the trash. When a folder is shared with other programs (a Git checkout
//! the editor and the build scripts also use) they get in the way. With the setting on, a space that shows a folder an
//! administrator chose keeps them in `<setting>/<space id>/` instead (`space`, `journal/`, `versions/`, `trash/`), and
//! the folder holds only the person's files. Nothing else changes: the index records the same paths as before, and
//! `beneath::Overlay` turns them into the place they are kept in, wherever a space's folder is opened.
//!
//! Trash and versions rely on a rename within one file system (instant, and safe if the process stops halfway), so the
//! folder for them has to be on the same file system *and the same mount* as the space's folder: a rename can't cross
//! two mounts even when both are the same disk. That is checked by renaming a small file between the two. When it fails
//! the space is not used (nothing is looked up, so nothing is written to either place) and the reason is shown.
//! Only local file systems qualify: object storage, SFTP, WebDAV and mounts that only look like folders can't rename.
//! A read-only space needs no rename: when it can't, nothing is moved and what its folder already holds of its own is
//! looked up there. Single-user mode shows a folder that can't be changed as such a space (`why_read_only`).
//!
//! The marker no longer vouches for the folder by itself (it is in another place): it only tells that the folder of
//! files is the one the space showed, together with the folder's identity (device and inode) recorded when it was chosen
//! (`space_marker`). Where the system has no identity (some Windows and exFAT disks), that check is weaker.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use sqlx::SqlitePool;

use crate::{beneath::Overlay, sync::Mutex};

/// The folder the setting names
static BASE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Turns the setting on (a full path of a folder that exists) or off. For the server's start
pub fn configure(base: Option<&Path>) -> Result<(), String> {
    let Some(base) = base else {
        *BASE.lock() = None;
        return Ok(());
    };
    if !base.is_absolute() {
        return Err("THIRTYFILE_FOLDER_STATE needs the full path of a folder".into());
    }
    if !base.is_dir() {
        return Err(format!("THIRTYFILE_FOLDER_STATE: {} isn't a folder that exists. Create it, or mount the disk that has it.", base.display()));
    }
    *BASE.lock() = Some(base.to_path_buf());
    Ok(())
}

/// The folder the setting names, when it is on
pub fn base() -> Option<PathBuf> {
    BASE.lock().clone()
}

/// Whether the setting is on
pub fn enabled() -> bool {
    BASE.lock().is_some()
}

fn real(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// The place for the space's own files, made if it isn't there
fn place(base: &Path, drive: &str) -> std::io::Result<PathBuf> {
    let state = base.join(drive);
    match std::fs::create_dir(&state) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && state.is_dir() => {}
        Err(e) => return Err(e),
    }
    Ok(state)
}

/// A file renamed from the space's folder to the other place and back, to see that both are on the same file system and
/// mount
fn probe(source: &Path, state: &Path) -> std::io::Result<()> {
    let name = format!(".thirtyfile-probe-{}", crate::util::new_id());
    let (a, b) = (source.join(&name), state.join(&name));
    std::fs::write(&a, b"")?;
    let moved = std::fs::rename(&a, &b).and_then(|()| std::fs::rename(&b, &a));
    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
    moved
}

/// Why changes can't be made in the folder `source`, when they can't: it can't be written to, or (with the setting
/// on) trash and versions couldn't be renamed out of it
pub fn why_read_only(source: &Path) -> Option<String> {
    why_read_only_in(base().as_deref(), source)
}

/// `why_read_only` with the folder of the setting given
pub(crate) fn why_read_only_in(base: Option<&Path>, source: &Path) -> Option<String> {
    let test = source.join(format!(".thirtyfile-probe-{}", crate::util::new_id()));
    match std::fs::File::create_new(&test) {
        Ok(file) => drop(file),
        Err(e) if matches!(e.kind(), std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem) => {
            return Some(format!("it can't be written to ({e})"));
        }
        // Anything else (a full disk, say) doesn't make it read-only: a change that fails says why
        Err(_) => return None,
    }
    let _ = std::fs::remove_file(&test);
    let base = base?;
    // One inside the other is a mistake in the settings, which `attach` reports
    if overlap(base, source) {
        return None;
    }
    probe(source, base).err().map(|e| {
        format!("trash and versions can't be renamed into THIRTYFILE_FOLDER_STATE ({}), which has to be on the same file system and mount ({e})", base.display())
    })
}

/// Whether one folder is the other or inside it
fn overlap(a: &Path, b: &Path) -> bool {
    let (a, b) = (real(a), real(b));
    a.starts_with(&b) || b.starts_with(&a)
}

/// Whether the space's folder holds files of the space's own: its trash, versions or journal, or its marker
fn holds_own(drive: &str, source: &Path) -> bool {
    crate::beneath::MOVED_NAMES.iter().any(|(name, _)| {
        let here = source.join(name);
        std::fs::symlink_metadata(&here)
            .is_ok_and(|m| m.is_dir() || (*name == super::MARKER && m.is_file() && std::fs::read_to_string(&here).is_ok_and(|id| id.trim() == drive)))
    })
}

/// Moves what the space's folder holds of its own into the other place, once (for a space made before the setting was
/// on). A journal with an entry in it is an unfinished change that only the folder's own way of recovering understands.
fn migrate(drive: &str, source: &Path, state: &Path) -> Result<(), String> {
    for (from, name) in crate::beneath::MOVED_NAMES {
        let here = source.join(from);
        let Ok(meta) = std::fs::symlink_metadata(&here) else { continue };
        let there = state.join(name);
        if from == ".thirtyfile-space" {
            // Only this space's own marker moves; another's stays where it is
            if meta.is_file() && std::fs::read_to_string(&here).is_ok_and(|id| id.trim() == drive) {
                if !there.exists() {
                    std::fs::write(&there, drive).map_err(|e| format!("Couldn't write the space's marker in {}: {e}", state.display()))?;
                }
                std::fs::remove_file(&here).map_err(|e| format!("Couldn't remove the marker from {}: {e}", source.display()))?;
            }
            continue;
        }
        if !meta.is_dir() {
            continue;
        }
        if from == ".thirtyfile-journal" && std::fs::read_dir(&here).is_ok_and(|mut r| r.next().is_some()) {
            return Err(format!(
                "{} holds an unfinished change. Start ThirtyFile once without THIRTYFILE_FOLDER_STATE so that it recovers it, then set it again.",
                source.display()
            ));
        }
        if there.exists() {
            if std::fs::read_dir(&there).is_ok_and(|mut r| r.next().is_none()) {
                let _ = std::fs::remove_dir(&there);
            } else {
                return Err(format!("Both {} and {} hold the space's {name}: keep one of them and remove the other.", here.display(), there.display()));
            }
        }
        std::fs::rename(&here, &there).map_err(|e| format!("Couldn't move {} to {}: {e}", here.display(), there.display()))?;
    }
    Ok(())
}

/// Puts the space `drive`'s own files, from now on, in the folder the setting names (when it is on). `source` is the
/// space's folder. Its folder not being there (a disk that isn't mounted) isn't a failure: nothing is moved or checked
/// until it is, and what is looked up goes to the other place meanwhile. Returns why the space can't be used.
pub fn attach(drive: &str, source: &Path, read_only: bool, identity: Option<(i64, i64)>) -> Result<(), String> {
    match base() {
        Some(base) => attach_in(&base, drive, source, read_only, identity),
        None => Ok(()),
    }
}

/// `attach` with the folder given (the setting's)
pub(crate) fn attach_in(base: &Path, drive: &str, source: &Path, read_only: bool, identity: Option<(i64, i64)>) -> Result<(), String> {
    let state = place(base, drive).map_err(|e| format!("Couldn't use {} for the space's own files: {e}", base.display()))?;
    let overlay = Arc::new(Overlay::new(state.clone(), drive.to_string()));
    crate::beneath::overlay_folder(source, overlay.clone());
    if let Some(known) = identity {
        super::remember_root(drive, known);
    }
    let fail = |why: String| {
        overlay.set_broken(Some(why.clone()));
        Err(why)
    };
    if !source.is_dir() {
        return Ok(());
    }
    if overlap(source, &state) {
        return fail(format!("THIRTYFILE_FOLDER_STATE ({}) must be outside the folder of the space {} and not hold it", base.display(), source.display()));
    }
    if let Err(e) = probe(source, &state) {
        if read_only {
            // Nothing is changed in such a space, so nothing needs to move: what the folder already holds stays there
            if holds_own(drive, source) {
                crate::beneath::forget_overlay(source);
            }
            return Ok(());
        }
        return fail(format!(
            "{} isn't on the same file system and mount as the space's folder {}: trash and versions need to rename files between them ({e}). Use a folder next to the files on the same disk, mounted as one (in Docker, mount their common parent folder).",
            state.display(),
            source.display()
        ));
    }
    if let Err(why) = migrate(drive, source, &state) {
        return fail(why);
    }
    Ok(())
}

/// Before a read-only space is opened up for changes: it is refused when its own files can't be kept where the server
/// is set to keep them, and stays read-only
pub async fn allow_changes(conn: &mut sqlx::SqliteConnection, drive: &str) -> crate::error::AppResult<()> {
    if !enabled() {
        return Ok(());
    }
    let row: Option<(String, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT source_path, source_dev, source_ino FROM drives WHERE id = ? AND mode = 'folder' AND source_path IS NOT NULL AND location_id IS NULL",
    )
    .bind(drive)
    .fetch_optional(conn)
    .await?;
    let Some((source, dev, ino)) = row else { return Ok(()) };
    let (source, identity) = (Path::new(&source), dev.zip(ino));
    if let Err(why) = attach(drive, source, false, identity) {
        // Read-only as before
        let _ = attach(drive, source, true, identity);
        return Err(crate::error::AppError::bad_request(why));
    }
    Ok(())
}

/// `attach` for every space that shows a folder an administrator chose, when the server starts. A space that can't be
/// used is left out (and logged); the others carry on.
pub async fn attach_all(db: &SqlitePool) -> Result<(), sqlx::Error> {
    if !enabled() {
        return Ok(());
    }
    type Row = (String, String, bool, Option<i64>, Option<i64>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, source_path, read_only, source_dev, source_ino FROM drives WHERE mode = 'folder' AND source_path IS NOT NULL AND location_id IS NULL",
    )
    .fetch_all(db)
    .await?;
    for (id, source, read_only, dev, ino) in rows {
        if let Err(why) = attach(&id, Path::new(&source), read_only, dev.zip(ino)) {
            tracing::error!("The space at {source} isn't used: {why}");
        }
    }
    Ok(())
}

/// Why the space whose folder is `source` can't be used now, when it can't
#[cfg(test)]
fn broken(source: &Path) -> Option<String> {
    crate::beneath::overlay_of(source).and_then(|o| o.broken())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::new_id;

    fn dir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("thirtyfile-state-{name}-{}", new_id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn what_a_space_keeps_of_its_own_moves_out_of_its_folder() {
        let (files, base) = (dir("files"), dir("base"));
        let drive = new_id();
        // A space made before the setting: its own files are in its folder
        std::fs::create_dir_all(files.join(".thirtyfile-trash/t1")).unwrap();
        std::fs::write(files.join(".thirtyfile-trash/t1/a.txt"), b"deleted").unwrap();
        std::fs::create_dir_all(files.join(".thirtyfile-versions/n1")).unwrap();
        std::fs::write(files.join(".thirtyfile-versions/n1/v1"), b"old").unwrap();
        std::fs::create_dir_all(files.join(".thirtyfile-journal")).unwrap();
        std::fs::write(files.join(".thirtyfile-space"), &drive).unwrap();
        std::fs::write(files.join("mine.txt"), b"mine").unwrap();

        assert!(attach_in(&base, &drive, &files, false, None).is_ok());
        let mut left: Vec<String> = std::fs::read_dir(&files).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["mine.txt"], "only the person's files are left");
        let state = base.join(&drive);
        assert_eq!(std::fs::read_to_string(state.join("trash/t1/a.txt")).unwrap(), "deleted");
        assert_eq!(std::fs::read_to_string(state.join("versions/n1/v1")).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(state.join("space")).unwrap(), drive);

        // Looked up through the space's folder, the same names reach the other place
        let root = crate::beneath::Pinned::root(&files).unwrap();
        assert_eq!(std::fs::read_to_string(root.join(".thirtyfile-trash/t1/a.txt").unwrap().as_path()).unwrap(), "deleted");
        assert_eq!(crate::folders::space_marker(&root).unwrap().as_deref(), Some(drive.as_str()));
        // A rename between the folder and the other place works, as trash and versions need
        std::fs::create_dir_all(root.join(".thirtyfile-trash").unwrap().as_path()).unwrap();
        std::fs::rename(root.join("mine.txt").unwrap().as_path(), root.join(".thirtyfile-trash/mine.txt").unwrap().as_path()).unwrap();
        assert!(state.join("trash/mine.txt").is_file() && !files.join("mine.txt").exists());
        // Nothing of ThirtyFile's was made in the folder meanwhile
        assert_eq!(std::fs::read_dir(&files).unwrap().count(), 0);

        // Starting again changes nothing
        assert!(attach_in(&base, &drive, &files, false, None).is_ok());
        let _ = std::fs::remove_dir_all(&files);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_unfinished_change_or_two_copies_stop_the_move() {
        let (files, base) = (dir("files"), dir("base"));
        let drive = new_id();
        std::fs::create_dir_all(files.join(".thirtyfile-journal")).unwrap();
        std::fs::write(files.join(".thirtyfile-journal/entry"), b"{}").unwrap();
        let err = attach_in(&base, &drive, &files, false, None).unwrap_err();
        assert!(err.contains("unfinished change"), "{err}");
        // Not used meanwhile: nothing is looked up in either place
        let root = crate::beneath::Pinned::root(&files).unwrap();
        assert!(root.join(".thirtyfile-trash").is_err());
        assert!(broken(&files).is_some());
        let _ = std::fs::remove_dir_all(&files);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_other_place_cant_be_inside_the_folder() {
        let files = dir("files");
        let base = files.join("state");
        std::fs::create_dir_all(&base).unwrap();
        let err = attach_in(&base, &new_id(), &files, false, None).unwrap_err();
        assert!(err.contains("must be outside"), "{err}");
        let _ = std::fs::remove_dir_all(&files);
    }

    #[test]
    fn a_folder_that_takes_changes_isnt_read_only() {
        let (files, base) = (dir("files"), dir("base"));
        assert_eq!(why_read_only_in(None, &files), None);
        assert_eq!(why_read_only_in(Some(&base), &files), None);
        // Nothing is left behind in either
        assert_eq!(std::fs::read_dir(&files).unwrap().count() + std::fs::read_dir(&base).unwrap().count(), 0);
        // The other place can't be renamed into (here: it isn't there), as with another mount
        let why = why_read_only_in(Some(&base.join("nothing")), &files).unwrap();
        assert!(why.contains("same file system and mount"), "{why}");
        assert_eq!(std::fs::read_dir(&files).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&files);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_read_only_space_that_cant_move_its_own_files_keeps_them_where_they_are() {
        let (files, base) = (dir("files"), dir("base"));
        let drive = new_id();
        // The rename between the two fails: what stands where the space's other place would be takes no files
        let state = base.join(&drive);
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(files.join(".thirtyfile-trash/t1")).unwrap();
        deny(&state, true);
        if std::fs::write(state.join("probe"), b"").is_ok() {
            deny(&state, false);
            return;
        }
        assert!(attach_in(&base, &drive, &files, false, None).is_err());
        assert!(attach_in(&base, &drive, &files, true, None).is_ok());
        assert!(crate::beneath::overlay_of(&files).is_none() && files.join(".thirtyfile-trash/t1").is_dir());
        // One holding nothing of its own keeps it elsewhere all the same, so the folder is left alone
        std::fs::remove_dir_all(files.join(".thirtyfile-trash")).unwrap();
        assert!(attach_in(&base, &drive, &files, true, None).is_ok());
        assert!(crate::beneath::overlay_of(&files).is_some() && broken(&files).is_none());
        deny(&state, false);
        let _ = std::fs::remove_dir_all(&files);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Takes away, and gives back, the right to put anything in a folder
    fn deny(folder: &Path, yes: bool) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if yes { 0o555 } else { 0o755 };
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        #[cfg(windows)]
        {
            let me = format!("{}\\{}", std::env::var("USERDOMAIN").unwrap(), std::env::var("USERNAME").unwrap());
            let args = if yes { ["/deny".to_string(), format!("{me}:(WD,AD)")] } else { ["/remove:d".to_string(), me] };
            assert!(std::process::Command::new("icacls").arg(folder).args(args).output().unwrap().status.success());
        }
    }

    #[test]
    fn the_setting_needs_a_folder_that_exists() {
        assert!(configure(Some(Path::new("relative/path"))).is_err());
        assert!(configure(Some(&std::env::temp_dir().join(format!("nothing-{}", new_id())))).is_err());
    }
}
