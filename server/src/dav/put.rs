//! PUT: receiving a file and storing it

use super::*;

pub(super) fn check_size(st: &AppState, size: u64) -> AppResult<()> {
    if size > MAX_PUT || (st.max_upload > 0 && size > st.max_upload) {
        return Err(AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "The file exceeds the upload size limit"));
    }
    Ok(())
}

/// Creates or replaces a file. The body is received into a temporary file first, then stored like an upload
pub(super) async fn put(st: &AppState, user: &User, segs: &[String], headers: &HeaderMap, body: Body) -> AppResult<Response> {
    let (parent, name) = {
        let mut c = st.db.acquire().await?;
        let (parent, name) = parent_of(&mut c, user, segs).await?;
        // Checked before anything is received: nobody without write access makes the server take a file
        let parent = tree::folder_for(&mut c, user, &parent.id, Need::Write).await?;
        (parent, name)
    };
    let mut c = st.db.acquire().await?;
    let mut existing = child_named(&mut c, &parent.id, &name).await?;
    // Only when the index and the folder on the server disagree about this name is the folder looked at again:
    // copying thousands of files into one folder mustn't re-read it for each
    if parent.in_folder_space() {
        // (on a blocking thread, within a few seconds: when the disk doesn't answer, the index is taken as it is)
        let (p, n) = (parent.clone(), name.clone());
        let look = move || p.fs_pinned().and_then(|p| p.join(&n)).is_ok_and(|p| std::fs::symlink_metadata(p.as_path()).is_ok());
        let on_disk = crate::util::blocking_within(format!("check of {name} in {}", parent.id), std::time::Duration::from_secs(5), look).await;
        if on_disk.is_some_and(|on_disk| on_disk != existing.is_some()) {
            drop(c);
            crate::folders::sync_folder(st, &parent).await;
            c = st.db.acquire().await?;
            existing = child_named(&mut c, &parent.id, &name).await?;
        }
    }
    if let Some(n) = &existing {
        if n.is_folder() {
            return Err(AppError::new(StatusCode::METHOD_NOT_ALLOWED, "A folder has this name"));
        }
        tree::node_for(&mut c, user, &n.id, Need::Write).await?;
        if headers.get(header::IF_NONE_MATCH).is_some_and(|v| v.as_bytes() == b"*") {
            return Err(precondition("The file already exists"));
        }
    }
    let replaced = existing.as_ref().map_or(0, |n| n.size);
    // Without a stated length (a body sent in chunks), receiving stops as soon as more arrived than the space can take
    let mut room = None;
    if let Some(len) = headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok()) {
        check_size(st, len)?;
        tree::check_quota(&mut c, parent.drive(), len as i64 - replaced).await?;
    } else if let Some((left, drive)) = tree::room_left(&mut c, parent.drive(), None).await? {
        room = Some((u64::try_from(left + replaced).unwrap_or(0), drive));
    }
    drop(c);
    let tmp = st.tmp_dir().join(format!("dav-{}", new_id()));
    // Content for the content store is hashed as it arrives, so storing it doesn't read the file again
    let (size, hash) = match receive(st, body, &tmp, !parent.in_folder_space(), room).await {
        Ok(received) => received,
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e);
        }
    };
    // Stored in a task of its own, so a dropped request can't stop it halfway
    let created = tokio::spawn(store(st.clone(), user.clone(), parent, name, tmp, size, hash)).await.map_err(AppError::internal)??;
    Ok(if created { StatusCode::CREATED } else { StatusCode::NO_CONTENT }.into_response())
}

/// Writes the body to a file, within the upload size limit and the `room` the space has left; returns its size, and
/// with `hash` its SHA-256
pub(super) async fn receive(st: &AppState, body: Body, path: &Path, hash: bool, room: Option<(u64, tree::Drive)>) -> AppResult<(u64, Option<String>)> {
    use sha2::Digest;
    let mut file = tokio::fs::File::create(path).await?;
    let mut stream = body.into_data_stream();
    let mut size = 0u64;
    let mut hasher = hash.then(sha2::Sha256::new);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| AppError::bad_request("Connection interrupted"))?;
        size += chunk.len() as u64;
        check_size(st, size)?;
        if let Some((left, drive)) = &room
            && size > *left
        {
            return Err(tree::quota_error(drive));
        }
        file.write_all(&chunk).await?;
        if let Some(h) = &mut hasher {
            h.update(&chunk);
        }
    }
    file.flush().await?;
    file.sync_data().await?;
    Ok((size, hasher.map(|h| hex::encode(h.finalize()))))
}

/// Stores a received file as `name` in `parent`, replacing a file of that name; true when it was created. `hash`: the
/// content's SHA-256 when it was computed while receiving it
pub(super) async fn store(st: AppState, user: User, parent: Node, name: String, tmp: PathBuf, size: u64, hash: Option<String>) -> AppResult<bool> {
    let result = if parent.in_folder_space() {
        store_in_folder(&st, &user, &parent, &name, &tmp, size).await
    } else {
        store_content(&st, &user, &parent, &name, &tmp, size, hash).await
    };
    let _ = tokio::fs::remove_file(&tmp).await;
    result
}

pub(super) async fn store_content(st: &AppState, user: &User, parent: &Node, name: &str, tmp: &Path, size: u64, hash: Option<String>) -> AppResult<bool> {
    let hash = match hash {
        Some(h) => h,
        None => {
            let (hash, hashed) = files::hash_file(tmp.to_path_buf()).await?;
            if hashed != size {
                return Err(AppError::bad_request("File size mismatch"));
            }
            hash
        }
    };
    let size = size as i64;
    // First into the space's storage location, without holding the write lock (S3 may take a while)
    let staged = tree::stage_blob(st, parent.drive(), hash.clone(), size, tmp.to_path_buf()).await?;
    let _w = st.write_lock.lock().await;
    let result = async {
        let mut tx = crate::db::begin_write(&st.db).await?;
        // Looked at again under the write lock: the folder or the file may have changed meanwhile
        let folder = tree::folder_for(&mut tx, user, &parent.id, Need::Write).await?;
        if folder.in_folder_space() || folder.drive() != parent.drive() {
            return Err(AppError::conflict("Something changed at the same time. Try again."));
        }
        let (created, extra, removed) = match child_named(&mut tx, &folder.id, name).await? {
            Some(n) if n.is_folder() => return Err(AppError::new(StatusCode::METHOD_NOT_ALLOWED, "A folder has this name")),
            Some(n) => {
                let n = tree::node_for(&mut tx, user, &n.id, Need::Write).await?;
                // The same content again (clients often save a file twice): nothing changes
                if n.blob_hash.as_deref() == Some(hash.as_str()) {
                    return Ok((false, None, crate::versions::Removed::default()));
                }
                tree::check_quota(&mut tx, n.drive(), size - n.size).await?;
                let extra = tree::commit_blob(&mut tx, &staged).await?;
                // The content it had is kept as an earlier version
                let removed = tree::set_content(&mut tx, crate::versions::Policy::of(st), &n, &hash, size, user.id).await?;
                tree::touch(&mut tx, &folder.id).await?;
                logs::record_activity(&mut tx, user, Some(&n), "edit", "").await?;
                (false, extra, removed)
            }
            None => {
                tree::check_quota(&mut tx, folder.drive(), size).await?;
                let extra = tree::commit_blob(&mut tx, &staged).await?;
                let id = new_id();
                sqlx::query(
                    "INSERT INTO nodes (id, owner_id, parent_id, kind, name, blob_hash, size, mime, drive_id, created_at, updated_at)
                     SELECT ?1, ?2, ?3, 'file', ?4, ?5, ?6, ?7, drive_id, ?8, ?8 FROM nodes WHERE id = ?3",
                )
                .bind(&id)
                .bind(user.id)
                .bind(&folder.id)
                .bind(name)
                .bind(&hash)
                .bind(size)
                .bind(guess_mime(name))
                .bind(now())
                .execute(&mut *tx)
                .await?;
                tree::touch(&mut tx, &folder.id).await?;
                tree::adjust_usage(&mut tx, folder.drive(), size).await?;
                let node = tree::get_node(&mut tx, &id).await?;
                logs::record_activity(&mut tx, user, node.as_ref(), "upload", "").await?;
                (true, extra, crate::versions::Removed::default())
            }
        };
        tx.commit().await?;
        Ok((created, extra, removed))
    }
    .await;
    match result {
        Ok((created, extra, removed)) => {
            tree::finish_staged(st, staged, extra).await;
            removed.finish(st);
            Ok(created)
        }
        Err(e) => {
            tree::abandon_staged(st, staged).await;
            Err(e)
        }
    }
}

pub(super) async fn store_in_folder(st: &AppState, user: &User, parent: &Node, name: &str, tmp: &Path, size: u64) -> AppResult<bool> {
    let staged = fsops::stage_upload(st, parent, tmp, size).await?;
    let _space = fsops::lock_space(parent.drive()).await;
    // Its folder answers, before the write lock is taken
    let ready = fsops::ready(st, parent.drive()).await;
    let _w = st.write_lock.lock().await;
    let result = async {
        ready?;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let folder = tree::folder_for(&mut tx, user, &parent.id, Need::Write).await?;
        if folder.drive() != parent.drive() {
            return Err(AppError::conflict("Something changed at the same time. Try again."));
        }
        let mut removed = crate::versions::Removed::default();
        let created = match child_named(&mut tx, &folder.id, name).await? {
            Some(n) if n.is_folder() => return Err(AppError::new(StatusCode::METHOD_NOT_ALLOWED, "A folder has this name")),
            Some(n) => {
                let n = tree::node_for(&mut tx, user, &n.id, Need::Write).await?;
                tree::check_quota(&mut tx, n.drive(), size as i64 - n.size).await?;
                // The content it had is kept as an earlier version
                removed = fsops::replace_file(&mut tx, crate::versions::Policy::of(st), &staged, &n, user.id).await?;
                let new_size = tree::get_node(&mut tx, &n.id).await?.map_or(n.size, |x| x.size);
                tree::adjust_usage(&mut tx, n.drive(), new_size - n.size).await?;
                logs::record_activity(&mut tx, user, Some(&n), "edit", "").await?;
                false
            }
            None => {
                tree::check_quota(&mut tx, folder.drive(), size as i64).await?;
                let id = fsops::place_file(&mut tx, &staged, user.id, &folder, name).await?;
                tree::touch(&mut tx, &folder.id).await?;
                if let Some(n) = tree::get_node(&mut tx, &id).await? {
                    tree::adjust_usage(&mut tx, n.drive(), n.size).await?;
                    logs::record_activity(&mut tx, user, Some(&n), "upload", "").await?;
                }
                true
            }
        };
        tx.commit().await?;
        removed.finish(st);
        Ok(created)
    }
    .await;
    if result.is_err() {
        // Still under its temporary name unless it was put in place and only the index failed (the next scan shows it then)
        let _ = tokio::fs::remove_file(&staged).await;
    }
    result
}
