//! Building a new folder (extracting a ZIP file)

use super::*;

/// A new folder built inside `dest`'s folder under a name scans ignore, removed again unless `place_folder` puts it in
/// place
pub struct Staging {
    pub(super) wrap: Pinned,
    /// The new folder, inside `wrap`
    pub top: Pinned,
}

impl Drop for Staging {
    fn drop(&mut self) {
        // On a blocking thread (a folder already gone is left as it is)
        remove_later(vec![self.wrap.clone()]);
    }
}

pub async fn staging(dest: &Node) -> AppResult<Staging> {
    let d = dest.clone();
    on_disk(dest.drive(), disk_wake(), move || {
        let wrap = abs(&d)?.join(&format!("{COPY_PREFIX}{}", new_id())).map_err(gone_or_disk_error)?;
        std::fs::create_dir(wrap.as_path()).map_err(disk_error)?;
        let top = wrap.join("folder").map_err(disk_error)?;
        let staged = Staging { wrap, top };
        std::fs::create_dir(staged.top.as_path()).map_err(disk_error)?;
        Ok(staged)
    })
    .await
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
pub(super) fn list_tree(top: &Pinned) -> io::Result<Vec<(String, Stat)>> {
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
    let w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let dest = tree::folder_for(&mut tx, user, dest_id, Need::Write).await?;
    locks.check(&dest)?;
    tree::check_quota(&mut tx, dest.drive(), bytes).await?;
    check_name(name)?;
    let name = free_name(&mut tx, &dest, name, true).await?;
    let (d, n, top) = (dest.clone(), name.clone(), staged.top.clone());
    let final_path = on_disk(dest.drive(), disk_wait(), move || {
        let final_path = abs(&d)?.join(&n).map_err(gone_or_disk_error)?;
        rename_new(top.as_path(), final_path.as_path()).map_err(disk_error)?;
        Ok(final_path)
    })
    .await?;
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
    drop(w);
    // The folder that held it is empty now: removed before the change is reported done, so nothing half-made is left
    // behind once it is (should the change fail instead, the renames are undone and `Staging` removes it all)
    let wrap = staged.wrap.clone();
    let _ = on_disk(dest.drive(), disk_wait(), move || {
        let _ = std::fs::remove_dir(wrap.as_path());
        Ok(())
    })
    .await;
    Ok((root, name))
}
