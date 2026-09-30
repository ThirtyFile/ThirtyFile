//! Uploads and saving: new content written next to its place, then renamed into it

use super::*;

/// Removes a file staged for a change when the change stops before using it (once renamed into place, the name is gone)
pub(super) struct Discard(Option<Pinned>);

impl Drop for Discard {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            // On a blocking thread: the disk may be the one that doesn't answer
            tokio::task::spawn_blocking(move || {
                let _ = std::fs::remove_file(p.as_path());
            });
        }
    }
}

pub(super) const UPLOAD_PREFIX: &str = ".thirtyfile-upload-";
pub(super) const SAVE_PREFIX: &str = ".thirtyfile-save-";

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

pub(super) async fn stage(folder: &Node, tmp: &Path, size: u64) -> AppResult<Pinned> {
    let f = folder.clone();
    let staged = on_disk(folder.drive(), disk_wake(), move || space_root(&f)?.join(&format!("{UPLOAD_PREFIX}{}", new_id())).map_err(disk_error)).await?;
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
    let (f, n, from) = (folder.clone(), name.to_string(), staged.clone());
    let s = on_disk(folder.drive(), disk_wait(), move || {
        let to = abs(&f)?.join(&n).map_err(gone_or_disk_error)?;
        rename_new(from.as_path(), to.as_path()).map_err(disk_error)?;
        stat(to.as_path()).map_err(disk_error)
    })
    .await?;
    let id = new_id();
    insert(conn, &id, owner, folder, name, &child_rel(rel_of(folder), name), &s).await?;
    Ok(id)
}

/// Renames content staged in the space's folder over an existing file, which keeps its id, and indexes it (the caller
/// counts the change in size). The file it had is kept as an earlier version first; returns what versions no longer
/// kept leave to remove after the commit.
pub async fn replace_file(conn: &mut SqliteConnection, policy: versions::Policy, staged: &Pinned, existing: &Node, by: i64) -> AppResult<versions::Removed> {
    let (s, kept) = replace_on_disk(policy, existing, staged).await?;
    let removed = versions::record_kept(conn, policy, existing, kept).await?;
    record(conn, &existing.id, existing.drive(), rel_of(existing), &s).await?;
    sqlx::query("UPDATE nodes SET updated_at = ?, content_by = ? WHERE id = ?")
        .bind(now().max(existing.updated_at + 1))
        .bind(by)
        .bind(&existing.id)
        .execute(conn)
        .await?;
    Ok(removed)
}

/// Puts `staged` (in the space's folder) in the place of the file `existing`, which keeps its permissions; what it had
/// is kept as an earlier version on disk, for the caller to record. Returns what is there now.
pub(super) async fn replace_on_disk(policy: versions::Policy, existing: &Node, staged: &Pinned) -> AppResult<(Stat, Option<versions::KeptFile>)> {
    let (n, staged) = (existing.clone(), staged.clone());
    on_disk(existing.drive(), disk_wait(), move || {
        let to = abs(&n)?;
        let _ = crate::beneath::copy_permissions(&to, &staged);
        let kept = versions::keep_on_disk(policy, &n, &to).map_err(disk_error)?;
        if let Err(e) = std::fs::rename(staged.as_path(), to.as_path()) {
            if let Some(k) = kept {
                k.undo();
            }
            return Err(disk_error(e));
        }
        Ok((stat(to.as_path()).map_err(disk_error)?, kept))
    })
    .await
}

/// Saves from the online editor into a folder space. A file changed on the server since it was indexed (or removed
/// there) isn't overwritten: the new content is saved next to it as "name (conflict copy)" and the save reports a
/// conflict.
pub async fn save(st: &AppState, user: &User, id: &str, body: &[u8], base: Option<i64>, conflict: fn() -> AppError) -> AppResult<Node> {
    let before = tree::node_for(&mut *st.db.acquire().await?, user, id, Need::Write).await?;
    let drive = before.drive().to_string();
    // The new content is written next to the file before taking the locks, so a large save doesn't hold up every
    // other change meanwhile; it is removed again unless it is put in place
    let tmp = {
        let (before, body) = (before.clone(), body.to_vec());
        on_disk(&drive, disk_wake(), move || {
            let tmp = abs(&before)?.parent().ok_or_else(|| AppError::not_found("Item not found"))?.join(&format!("{SAVE_PREFIX}{}", new_id())).map_err(disk_error)?;
            crate::beneath::write_new(&tmp, &body).map_err(disk_error)?;
            Ok(tmp)
        })
        .await?
    };
    let _discard = Discard(Some(tmp.clone()));
    let _space = lock_space(&drive).await;
    ready(st, &drive).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let node = tree::node_for(&mut tx, user, id, Need::Write).await?;
    if node.drive() != drive {
        return Err(AppError::conflict("Something changed at the same time. Try again."));
    }
    if base.is_some_and(|b| b != node.updated_at) {
        return Err(conflict());
    }
    let (fs_size, fs_mtime, fs_ino): (Option<i64>, Option<i64>, Option<i64>) =
        sqlx::query_as("SELECT fs_size, fs_mtime_ns, fs_ino FROM nodes WHERE id = ?").bind(&node.id).fetch_one(&mut *tx).await?;
    let unchanged = {
        let n = node.clone();
        on_disk(&drive, disk_wait(), move || {
            let path = abs(&n)?;
            Ok(stat(path.as_path()).is_ok_and(|s| Some(s.size) == fs_size && Some(s.mtime_ns) == fs_mtime && Some(s.ino) == fs_ino))
        })
        .await?
    };
    // The new content counts against the space's size limit: by how much it grows the file, or all of it as a copy
    tree::check_quota(&mut tx, node.drive(), body.len() as i64 - if unchanged { node.size } else { 0 }).await?;

    if !unchanged {
        let parent = tree::get_node(&mut tx, node.parent_id.as_deref().unwrap_or_default()).await?.ok_or_else(|| AppError::not_found("Folder not found"))?;
        let (stem, ext) = split_name(&node.name, false);
        let placed = async {
            let name = free_name(&mut tx, &parent, &format!("{stem} (conflict copy){ext}"), false).await?;
            let copy_id = place_file(&mut tx, &tmp, user.id, &parent, &name).await?;
            Ok::<_, AppError>((name, copy_id))
        }
        .await;
        // (should it fail, `Discard` removes the new content)
        let (name, copy_id) = placed?;
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

    // The new content takes the file's place (and its permissions); the content it had is kept as an earlier version
    let policy = versions::Policy::of(st);
    let (s, kept) = replace_on_disk(policy, &node, &tmp).await?;
    let removed = versions::record_kept(&mut tx, policy, &node, kept).await?;
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
