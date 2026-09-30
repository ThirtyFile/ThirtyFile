//! The marker file that says which location a folder is, and the built-in location's folder at startup

use super::*;

/// The file in a Local folder location's folder that says which location the folder is: it holds the location's id
/// (on its first line; S3, SFTP and FTP locations have one too, with the installation's id on a second line,
/// `locations::claim_place`). It is written only when an administrator adds the location or changes its folder (the
/// built-in location's at the first start of a new server, `prepare_builtin`). A location whose folder doesn't hold its marker is unavailable, and nothing is
/// created or written there: a disk or network share that isn't mounted leaves an empty folder (or none) at its
/// mount point, and files written there would land on the server's own disk and be hidden once it is mounted again.
pub const LOCATION_MARKER: &str = ".thirtyfile-location";
/// Shown when a Local folder location's folder isn't there, or doesn't hold the location's marker
pub const NOT_MOUNTED: &str = "The folder isn't there, or a different disk is mounted there";
/// Shown when a Local folder location's folder is there and has items, but no marker: it may be the right folder whose
/// marker was lost (a copy that left out hidden files, say). Followed by the location's id, which the file must hold.
pub const MARKER_MISSING: &str = "The folder has items but no .thirtyfile-location file. If it is this location's folder, create that file in it holding this line:";
/// Shown when an administrator chooses a folder whose marker names another location
pub const FOLDER_TAKEN: &str = "Another storage location uses this folder (it holds that location's .thirtyfile-location file)";
/// Shown when an S3, SFTP or FTP location's marker names another location, or another installation of ThirtyFile
pub const PLACE_TAKEN: &str =
    "Another storage location uses this place: its .thirtyfile-location file names another location or another ThirtyFile installation";
/// Items a disk or a NAS puts in a folder by itself: they don't make a storage folder "used"
pub const SYSTEM_ENTRIES: [&str; 6] = ["lost+found", "#recycle", "@eaDir", ".DS_Store", "System Volume Information", "$RECYCLE.BIN"];

/// Whether the folder `dir` has nothing in it but `SYSTEM_ENTRIES`
pub fn nothing_but_system_entries(dir: &Path) -> io::Result<bool> {
    for e in std::fs::read_dir(dir)? {
        let name = e?.file_name();
        let name = name.to_string_lossy();
        if !SYSTEM_ENTRIES.iter().any(|s| s.eq_ignore_ascii_case(&name)) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The location a marker names: its first line. A second line, when there is one, names the ThirtyFile installation
/// (`locations::install_id`), which the markers of S3, SFTP and FTP locations hold.
pub fn marker_location(body: &[u8]) -> String {
    String::from_utf8_lossy(body).lines().next().unwrap_or_default().trim().to_string()
}

/// The location id in the marker of the folder `root`; None when the folder or the marker isn't there
pub async fn marker_of(root: &Path) -> io::Result<Option<String>> {
    match tokio::fs::read(root.join(LOCATION_MARKER)).await {
        Ok(b) => Ok(Some(marker_location(&b))),
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => Ok(None),
        Err(e) => Err(e),
    }
}

/// An administrator added the location `id` with the folder `root`, or changed its folder to it: the folder is
/// created when it isn't there, and gets the location's marker. A folder whose marker names another location is
/// refused (`FOLDER_TAKEN`); one that already holds this location's (added again) is taken as it is. Returns
/// whether the marker was written now.
pub fn claim_folder(root: &Path, id: &str) -> io::Result<bool> {
    std::fs::create_dir_all(root)?;
    let marker = root.join(LOCATION_MARKER);
    match std::fs::read(&marker) {
        Ok(b) if marker_location(&b) == id => Ok(false),
        Ok(_) => Err(io::Error::other(StorageError { message: FOLDER_TAKEN, detail: root.display().to_string() })),
        Err(e) if e.kind() == io::ErrorKind::NotFound => std::fs::write(&marker, id).map(|()| true),
        Err(e) => Err(e),
    }
}

/// Removes the marker of the location `id` from its folder, when it holds that one (the location is deleted, or
/// adding it failed): the folder can be used for a location again
pub async fn release_folder(root: &Path, id: &str) {
    if marker_of(root).await.ok().flatten().is_some_and(|m| m == id) {
        let _ = tokio::fs::remove_file(root.join(LOCATION_MARKER)).await;
    }
}

/// The built-in location's folder (the storage folder) at startup, once the database is open. `recorded`: the database
/// records content or spaces on the built-in location.
///
/// A new install creates the folder, or finds it empty (items a disk or NAS makes by itself, `SYSTEM_ENTRIES`, don't
/// count): it gets the marker. When the database records something there, a missing or empty folder is never taken:
/// a disk or Docker volume that isn't mounted leaves exactly that, and uploads would land on the wrong disk. A folder
/// with items in it must already hold the built-in location's marker. Otherwise ThirtyFile doesn't start.
pub fn prepare_builtin(root: &Path, recorded: bool) -> Result<(), String> {
    let builtin = BUILTIN;
    let failed = |e: io::Error| format!("Can't use the storage folder {}: {e}", root.display());
    let unmounted = || {
        format!(
            "The storage folder {} is missing or empty, but the database records files and spaces stored there. The disk or volume that holds them may not be mounted. Check THIRTYFILE_STORAGE and the disks and volumes mounted there, then start ThirtyFile again.",
            root.display()
        )
    };
    let marker = root.join(LOCATION_MARKER);
    match std::fs::read(&marker) {
        Ok(b) if marker_location(&b) == builtin => Ok(()),
        Ok(_) => Err(format!(
            "The storage folder {} belongs to another storage location (its {LOCATION_MARKER} file names another one). Check THIRTYFILE_STORAGE and the disks mounted there.",
            root.display()
        )),
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => {
            let empty = match nothing_but_system_entries(root) {
                Ok(empty) => empty,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    if recorded {
                        return Err(unmounted());
                    }
                    std::fs::create_dir_all(root).map_err(failed)?;
                    true
                }
                Err(e) => return Err(failed(e)),
            };
            if !empty {
                return Err(format!(
                    "The storage folder {} has items in it but no {LOCATION_MARKER} file: it may be a different disk than the one ThirtyFile used. Check THIRTYFILE_STORAGE and the disks mounted there. If it is the right folder, create the file {} holding the word {builtin}.",
                    root.display(),
                    marker.display()
                ));
            }
            if recorded {
                return Err(unmounted());
            }
            std::fs::write(&marker, builtin).map_err(failed)
        }
        Err(e) => Err(failed(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_folder_gets_its_marker_only_when_new_or_empty() {
        let base = std::env::temp_dir().join(format!("thirtyfile-builtin-{}", crate::util::new_id()));
        let marker = |dir: &Path| std::fs::read_to_string(dir.join(LOCATION_MARKER)).ok();
        // A fresh install: the folder is created with the marker, and used from then on
        let fresh = base.join("storage");
        prepare_builtin(&fresh, false).unwrap();
        assert_eq!(marker(&fresh).as_deref(), Some("local"));
        std::fs::create_dir_all(fresh.join("users/admin")).unwrap();
        prepare_builtin(&fresh, true).unwrap();
        // An empty folder, or one with only what a disk or NAS makes by itself: nothing was there
        let empty = base.join("empty");
        for system in SYSTEM_ENTRIES {
            std::fs::create_dir_all(empty.join(system)).unwrap();
        }
        prepare_builtin(&empty, false).unwrap();
        assert_eq!(marker(&empty).as_deref(), Some("local"));
        // Items but no marker, or another location's: it may be another disk, so it isn't used or changed
        let full = base.join("full");
        std::fs::create_dir_all(full.join("users/amy")).unwrap();
        assert!(prepare_builtin(&full, false).unwrap_err().contains(LOCATION_MARKER));
        assert_eq!(marker(&full), None);
        let other = base.join("other");
        claim_folder(&other, "nas").unwrap();
        assert!(prepare_builtin(&other, false).is_err());
        assert_eq!(marker(&other).as_deref(), Some("nas"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_missing_or_empty_built_in_folder_is_never_taken_when_the_database_records_files_there() {
        let base = std::env::temp_dir().join(format!("thirtyfile-unmounted-{}", crate::util::new_id()));
        // A volume that isn't mounted: the folder is missing, or an empty mount point (with a disk's own entries)
        let missing = base.join("missing");
        let e = prepare_builtin(&missing, true).unwrap_err();
        assert!(e.contains("may not be mounted"), "{e}");
        assert!(!missing.exists(), "nothing is made");
        let empty = base.join("empty");
        std::fs::create_dir_all(empty.join("lost+found")).unwrap();
        assert!(prepare_builtin(&empty, true).unwrap_err().contains("may not be mounted"));
        assert!(!empty.join(LOCATION_MARKER).exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
