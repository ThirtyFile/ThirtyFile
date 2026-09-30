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
    let result = store_file(&st, &user, &parent, &name, &tmp, size, hash).await;
    let _ = tokio::fs::remove_file(&tmp).await;
    result
}

/// Makes the received file `name` in `parent` (content.rs), or gives the file of that name its content; true when
/// it was created
pub(super) async fn store_file(st: &AppState, user: &User, parent: &Node, name: &str, tmp: &Path, size: u64, hash: Option<String>) -> AppResult<bool> {
    let staged = content::stage(st, parent, content::Received { path: tmp.to_path_buf(), size, hash }).await?;
    let size = size as i64;
    let mut turn = staged.turn(st).await;
    let _w = st.write_lock.lock().await;
    let result = async {
        turn.ready()?;
        let mut tx = crate::db::begin_write(&st.db).await?;
        // Looked at again under the write lock: the folder or the file may have changed meanwhile
        let folder = tree::folder_for(&mut tx, user, &parent.id, Need::Write).await?;
        staged.check(&folder)?;
        let (created, written) = match child_named(&mut tx, &folder.id, name).await? {
            Some(n) if n.is_folder() => return Err(AppError::new(StatusCode::METHOD_NOT_ALLOWED, "A folder has this name")),
            Some(n) => {
                let n = tree::node_for(&mut tx, user, &n.id, Need::Write).await?;
                // The same content again (clients often save a file twice): nothing changes
                if staged.hash().is_some() && n.blob_hash.as_deref() == staged.hash() {
                    return Ok((false, content::Written::default()));
                }
                tree::check_quota(&mut tx, n.drive(), size - n.size).await?;
                // The content it had is kept as an earlier version
                let written = content::replace(&mut tx, st, &staged, &n, user.id).await?;
                tree::touch(&mut tx, &folder.id).await?;
                logs::record_activity(&mut tx, user, Some(&n), "edit", "").await?;
                (false, written)
            }
            None => {
                tree::check_quota(&mut tx, folder.drive(), size).await?;
                let (id, written) = content::create(&mut tx, &staged, user.id, &folder, name).await?;
                tree::touch(&mut tx, &folder.id).await?;
                let node = tree::get_node(&mut tx, &id).await?;
                logs::record_activity(&mut tx, user, node.as_ref(), "upload", "").await?;
                (true, written)
            }
        };
        tx.commit().await?;
        Ok((created, written))
    }
    .await;
    match result {
        Ok((created, written)) => {
            staged.finish(st, written).await;
            Ok(created)
        }
        Err(e) => {
            staged.abandon(st).await;
            Err(e)
        }
    }
}
