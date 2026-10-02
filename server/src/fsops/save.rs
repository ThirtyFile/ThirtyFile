//! Uploads and saving: new content written next to its place, then renamed into it

use super::*;

pub(super) const UPLOAD_PREFIX: &str = ".thirtyfile-upload-";
/// What saves from the editor were written as before they went through content.rs: still recognised as leftovers
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
        #[cfg(test)]
        if testing::stops(n.drive(), testing::Stop::VersionKept) {
            return Err(testing::stopped());
        }
        if let Err(e) = std::fs::rename(staged.as_path(), to.as_path()) {
            if let Some(k) = kept {
                k.undo();
            }
            return Err(disk_error(e));
        }
        #[cfg(test)]
        if testing::stops(n.drive(), testing::Stop::Replaced) {
            return Err(testing::stopped());
        }
        Ok((stat(to.as_path()).map_err(disk_error)?, kept))
    })
    .await
}

/// Whether `file` is on disk as it was indexed (its size, time of change and inode): it may have been changed or
/// replaced there since
pub async fn unchanged_on_disk(conn: &mut SqliteConnection, file: &Node) -> AppResult<bool> {
    let (fs_size, fs_mtime, fs_ino): (Option<i64>, Option<i64>, Option<i64>) =
        sqlx::query_as("SELECT fs_size, fs_mtime_ns, fs_ino FROM nodes WHERE id = ?").bind(&file.id).fetch_one(&mut *conn).await?;
    let n = file.clone();
    on_disk(file.drive(), disk_wait(), move || {
        let path = abs(&n)?;
        Ok(stat(path.as_path()).is_ok_and(|s| Some(s.size) == fs_size && Some(s.mtime_ns) == fs_mtime && Some(s.ino) == fs_ino))
    })
    .await
}
