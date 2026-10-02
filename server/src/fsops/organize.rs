//! Changes within a space: folders, renaming, moving, the trash

use super::*;

/// A folder `make_dir` made, or found there already
pub struct MadeDir {
    /// Its name on disk: on a disk that ignores letter case, a folder already there may have its name in other case
    pub name: String,
    /// Its path below the space's folder
    pub rel: String,
    pub stat: Stat,
    /// It was there already (made on the server meanwhile, or the same folder by a name in other letter case)
    pub existed: bool,
}

/// Makes a folder on disk; one that is there already is used as it is
pub async fn make_dir(parent: &Node, name: &str) -> AppResult<MadeDir> {
    check_name(name)?;
    let (dir, name) = (parent.clone(), name.to_string());
    on_disk(parent.drive(), disk_wait(), move || {
        let inside = abs(&dir)?.dir().map_err(gone_or_disk_error)?;
        let path = inside.join(&name).map_err(gone_or_disk_error)?;
        let existed = match std::fs::create_dir(path.as_path()) {
            Ok(()) => false,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && std::fs::symlink_metadata(path.as_path()).is_ok_and(|m| m.is_dir()) => true,
            Err(e) => return Err(disk_error(e)),
        };
        let name = if existed { name_on_disk(&inside, &name, &path) } else { name };
        Ok(MadeDir { rel: child_rel(rel_of(&dir), &name), stat: stat(path.as_path()).map_err(disk_error)?, name, existed })
    })
    .await
}

/// The item the index has in `parent` for a folder `make_dir` found there already, if any: by its path on disk, or by
/// its identity where the system tells it
pub async fn indexed_dir(conn: &mut SqliteConnection, parent: &Node, made: &MadeDir) -> AppResult<Option<String>> {
    if !made.existed {
        return Ok(None);
    }
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM nodes WHERE parent_id = ?1 AND kind = 'folder' AND trashed_at IS NULL
           AND (fs_path = ?2 OR (?4 <> 0 AND fs_dev = ?3 AND fs_ino = ?4))
         ORDER BY fs_path = ?2 DESC LIMIT 1",
    )
    .bind(&parent.id)
    .bind(&made.rel)
    .bind(made.stat.dev)
    .bind(made.stat.ino)
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|(id,)| id))
}

/// The name the item `path` (found in the folder `dir` by `name`) has there: `name` itself, or, on a disk that ignores
/// letter case, the name in other case it was made with
fn name_on_disk(dir: &Pinned, name: &str, path: &Pinned) -> String {
    let Ok(read) = std::fs::read_dir(dir.as_path()) else { return name.to_string() };
    let lower = name.to_lowercase();
    let wanted = stat(path.as_path()).ok();
    // Where the system tells items apart by identity, the one with the same; elsewhere the disk ignoring letter case
    // is what found it
    let same = |p: &Pinned| match (wanted, stat(p.as_path())) {
        (Some(w), Ok(s)) if w.ino != 0 => (w.dev, w.ino) == (s.dev, s.ino),
        (_, s) => s.is_ok(),
    };
    let mut alike = None;
    for entry in read.flatten() {
        let Ok(found) = entry.file_name().into_string() else { continue };
        if found == name {
            return found;
        }
        if alike.is_none() && found.to_lowercase() == lower && dir.join(&found).is_ok_and(|p| same(&p)) {
            alike = Some(found);
        }
    }
    alike.unwrap_or_else(|| name.to_string())
}

/// Renames `node` to `name` in the folder `dest` on disk, the move written into the space's journal first
/// (journal.rs): should the change not be committed, the next scan puts the item back, or records the move, before it
/// reads the folders. Returns where it is now, where it was, and the entry in the journal.
pub(super) async fn rename_on_disk(node: &Node, dest: &Node, name: &str) -> AppResult<(Pinned, Pinned, Entry)> {
    let (drive, node, dest, name) = (node.drive().to_string(), node.clone(), dest.clone(), name.to_string());
    on_disk(&drive, disk_wait(), move || {
        let root = space_root(&node)?;
        let (from, to) = (abs(&node)?, abs(&dest)?.join(&name).map_err(gone_or_disk_error)?);
        let intent = Intent::Move { node: node.id.clone(), from: rel_of(&node).to_string(), to: child_rel(rel_of(&dest), &name) };
        let entry = write(&root, &intent).map_err(disk_error)?;
        match rename_new(from.as_path(), to.as_path()) {
            #[cfg(test)]
            Ok(()) if testing::stops(node.drive(), testing::Stop::Moved) => Err(testing::stopped()),
            Ok(()) => Ok((to, from, entry)),
            Err(e) => {
                entry.remove();
                Err(disk_error(e))
            }
        }
    })
    .await
}

/// Renames an item, or moves it to another folder of its space: on disk, then in the index (the caller records its
/// new name and folder)
pub async fn rename(conn: &mut SqliteConnection, locks: &SpaceLocks, node: &Node, dest: &Node, name: &str) -> AppResult<()> {
    check_name(name)?;
    let (to, from, entry) = rename_on_disk(node, dest, name).await?;
    locks.note_journaled(to, from, None, Some(entry));
    locks.later(changes::repath(conn, &node.id, node.drive(), rel_of(node), &child_rel(rel_of(dest), name)).await?);
    Ok(())
}

/// Moves an item to the space's trash folder, where it can be restored from. The move is written into the space's
/// journal first (journal.rs), so should the change not be committed, the item is put back.
pub async fn trash(conn: &mut SqliteConnection, locks: &SpaceLocks, node: &Node, trash_id: &str) -> AppResult<()> {
    let (n, id) = (node.clone(), trash_id.to_string());
    let moved = on_disk(node.drive(), disk_wait(), move || {
        let root = space_root(&n)?;
        let from = abs(&n)?;
        ensure_dir(&root.join(TRASH_DIR).map_err(disk_error)?).map_err(disk_error)?;
        let dir = root.join(&format!("{TRASH_DIR}/{id}")).map_err(disk_error)?;
        std::fs::create_dir(dir.as_path()).map_err(disk_error)?;
        let to = dir.join(&n.name).map_err(disk_error)?;
        let intent = Intent::Trash { node: n.id.clone(), from: rel_of(&n).to_string(), to: format!("{TRASH_DIR}/{id}/{}", n.name) };
        let entry = match write(&root, &intent) {
            Ok(entry) => entry,
            Err(e) => {
                let _ = std::fs::remove_dir(dir.as_path());
                return Err(disk_error(e));
            }
        };
        match rename_new(from.as_path(), to.as_path()) {
            #[cfg(test)]
            Ok(()) if testing::stops(n.drive(), testing::Stop::Trashed) => Err(testing::stopped()),
            Ok(()) => Ok(Some((to, from, dir, entry))),
            // Already gone from the server: only the index still had it
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                entry.remove();
                Ok(None)
            }
            Err(e) => {
                entry.remove();
                let _ = std::fs::remove_dir(dir.as_path());
                Err(disk_error(e))
            }
        }
    })
    .await?;
    if let Some((to, from, dir, entry)) = moved {
        locks.note_journaled(to, from, Some(dir), Some(entry));
    }
    locks.later(changes::repath(conn, &node.id, node.drive(), rel_of(node), &format!("{TRASH_DIR}/{trash_id}/{}", node.name)).await?);
    Ok(())
}

/// Moves a trashed item back into `dest` as `name`
pub async fn restore(conn: &mut SqliteConnection, locks: &SpaceLocks, node: &Node, dest: &Node, name: &str) -> AppResult<()> {
    let (to, from, entry) = rename_on_disk(node, dest, name).await?;
    // Its emptied trash folder stays until `clean_trash` (should the change fail, the item goes back into it)
    locks.note_journaled(to, from, None, Some(entry));
    locks.later(changes::repath(conn, &node.id, node.drive(), rel_of(node), &child_rel(rel_of(dest), name)).await?);
    Ok(())
}

/// The folder holding a trashed item of a folder space, removed from disk when the item is deleted for good
pub fn trash_folder(n: &Node) -> Option<crate::folders::Below> {
    let mut parts = n.fs_path.as_deref()?.split('/');
    if parts.next()? != TRASH_DIR {
        return None;
    }
    let id = parts.next()?;
    Some(crate::folders::Below::new(n.fs_root.as_deref()?, n.drive(), format!("{TRASH_DIR}/{id}")))
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
    remove_below_later(names.into_iter().filter(|n| !known.contains(n)).map(|n| crate::folders::Below::new(root, drive_id, format!("{TRASH_DIR}/{n}"))).collect());
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
            match put_back_into(&from, &dir, &item_name, is_dir) {
                Some(Ok(n)) => tracing::warn!("Put back {n:?}: it was being moved when ThirtyFile stopped"),
                Some(Err(e)) => tracing::warn!("Couldn't put back {item_name:?}: {e}"),
                None => {}
            }
        }
        let _ = std::fs::remove_dir(p.as_path());
    }
}
