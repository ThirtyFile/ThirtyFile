//! Checking a set on its destination, and deleting it from there.

use super::{
    layout,
    runner::{Ctx, Stop},
};
use crate::{
    error::{AppError, AppResult},
    storage::{EntryKind, Storage},
    util::now,
};

/// Objects looked at per page
const PAGE: i64 = 200;

/// Reads everything a set holds back from its destination: each snapshot's manifest and completion marker, and each
/// content, checked against its SHA-256. What is missing or damaged is listed, and the job fails.
pub async fn verify(cx: &Ctx<'_>) -> AppResult<Stop> {
    let (st, job) = (cx.st, cx.job);
    let set = super::load_set(&st.db, &job.set_id).await?;
    if set.removing {
        return Err(AppError::conflict("This copy is being deleted"));
    }
    crate::locations::probe(st, &set.dest_location)
        .await
        .map_err(|e| AppError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, format!("The copy's location can't be reached: {e}")))?;
    let dst = st.storage(&set.dest_location)?;
    // A set found on a location: its records are made from its manifests first
    let params: serde_json::Value = serde_json::from_str(&job.params).unwrap_or_default();
    if params["rebuild"].as_bool() == Some(true) {
        rebuild(cx, &set, dst.as_ref()).await?;
    }
    let (files, bytes): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(size), 0) FROM backup_objects WHERE set_id = ?").bind(&set.id).fetch_one(&st.db).await?;
    cx.set_counts(0, 0, files, bytes);
    cx.flush().await?;
    // The snapshots first: a manifest that isn't what was written makes the snapshot useless
    let snapshots: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT id, manifest_sha256, manifest_size FROM backup_snapshots WHERE set_id = ? AND state = 'complete'")
            .bind(&set.id)
            .fetch_all(&st.db)
            .await?;
    for (id, sha, size) in &snapshots {
        let complete = layout::read_small(dst.as_ref(), &layout::complete_key(&set.id, id)).await;
        let marker_ok = matches!(&complete, Ok(Some(b)) if serde_json::from_slice::<layout::Complete>(b).is_ok_and(|c| &c.manifest_sha256 == sha));
        if !marker_ok {
            cx.failed("", None, "A snapshot's completion marker is missing or changed".to_string());
            continue;
        }
        match read_hash(dst.as_ref(), &layout::manifest_key(&set.id, id), *size as u64).await {
            Ok((h, n)) if &h == sha && n == *size as u64 => {}
            Ok(_) => cx.failed("", None, "A snapshot's list of files was changed or damaged".to_string()),
            Err(e) => return Err(read_error(&e)),
        }
    }
    let mut last = String::new();
    loop {
        let rows: Vec<(String, i64)> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT hash, size FROM backup_objects WHERE set_id = ? AND hash > ? ORDER BY hash LIMIT {PAGE}")))
                .bind(&set.id)
                .bind(&last)
                .fetch_all(&st.db)
                .await?;
        let Some((hash, _)) = rows.last() else { break };
        last = hash.clone();
        for (hash, size) in rows {
            if let Some(stop) = cx.stop() {
                return Ok(stop);
            }
            let key = layout::object_key(&set.id, &hash);
            let read = cx.tries(|e: &std::io::Error| e.kind() != std::io::ErrorKind::NotFound, || read_hash(dst.as_ref(), &key, size as u64)).await;
            match read {
                Ok((h, n)) if h == hash && n == size as u64 => {
                    let _w = st.write_lock.lock().await;
                    sqlx::query("UPDATE backup_objects SET verified_at = ? WHERE set_id = ? AND hash = ?")
                        .bind(now())
                        .bind(&set.id)
                        .bind(&hash)
                        .execute(&st.db)
                        .await?;
                }
                Ok(_) => cx.failed("", None, format!("The content {} is damaged", &hash[..12])),
                Err(Ok(stop)) => return Ok(stop),
                Err(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => cx.failed("", None, format!("The content {} is missing", &hash[..12])),
                Err(Err(e)) => return Err(read_error(&e)),
            }
            cx.done(1, size).await?;
        }
    }
    if cx.failed_count() > 0 {
        let n = cx.failed_count();
        return Err(AppError::new(
            axum::http::StatusCode::BAD_GATEWAY,
            if n == 1 { "1 item of the copy is missing or damaged".to_string() } else { format!("{n} items of the copy are missing or damaged") },
        ));
    }
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        super::runner::finish(&mut tx, cx, None).await?;
        super::log(&mut tx, job, "backup_verified", &job.label).await?;
        AppResult::Ok(())
    }
    .await;
    crate::db::settle(tx, res).await?;
    Ok(Stop::Done)
}

/// The spaces a manifest lists, and the content it names with its size
type Held = (Vec<super::SpaceInfo>, Vec<(String, i64)>);

/// A set found on a location (imported): which spaces each snapshot holds, and which content the set should have, as
/// its manifests say (each checked against its completion marker)
async fn rebuild(cx: &Ctx<'_>, set: &super::Set, dst: &dyn Storage) -> AppResult<()> {
    let st = cx.st;
    let snapshots: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT id, manifest_sha256, manifest_size FROM backup_snapshots WHERE set_id = ? AND state = 'complete'")
            .bind(&set.id)
            .fetch_all(&st.db)
            .await?;
    for (id, sha, size) in snapshots {
        let path = layout::manifest(st, dst, &set.id, &id, &sha, size as u64).await?;
        let (spaces, objects) = tokio::task::spawn_blocking(move || -> std::io::Result<Held> {
            let mut spaces: Vec<super::SpaceInfo> = Vec::new();
            let mut objects = std::collections::HashMap::new();
            for line in layout::lines(&path)? {
                match line? {
                    layout::Line::Space { id, name, kind, owner, owner_id, mode, .. } => {
                        spaces.push(super::SpaceInfo { id, name, kind, owner, owner_id, mode, files: 0, bytes: 0 })
                    }
                    layout::Line::File { space, hash, size, .. } => {
                        if let Some(s) = spaces.iter_mut().find(|s| s.id == space) {
                            s.files += 1;
                            s.bytes += size;
                        }
                        if crate::storage::valid_hash(&hash).is_ok() {
                            objects.insert(hash, size);
                        }
                    }
                    layout::Line::Version { space, hash, size, .. } => {
                        if let Some(s) = spaces.iter_mut().find(|s| s.id == space) {
                            s.bytes += size;
                        }
                        if crate::storage::valid_hash(&hash).is_ok() {
                            objects.insert(hash, size);
                        }
                    }
                    _ => {}
                }
            }
            Ok((spaces, objects.into_iter().collect()))
        })
        .await
        .map_err(|e| AppError::internal(e.to_string()))??;
        let _w = st.write_lock.lock().await;
        sqlx::query("UPDATE backup_snapshots SET space_list = ? WHERE id = ?").bind(serde_json::to_string(&spaces).unwrap()).bind(&id).execute(&st.db).await?;
        for chunk in objects.chunks(500) {
            sqlx::query(
                "INSERT OR IGNORE INTO backup_objects (set_id, hash, size, created_at)
                 SELECT ?1, json_extract(value, '$[0]'), json_extract(value, '$[1]'), ?3 FROM json_each(?2)",
            )
            .bind(&set.id)
            .bind(serde_json::to_string(chunk).unwrap())
            .bind(now())
            .execute(&st.db)
            .await?;
        }
    }
    Ok(())
}

fn read_error(e: &std::io::Error) -> AppError {
    AppError::new(axum::http::StatusCode::BAD_GATEWAY, format!("Couldn't read from the copy's location: {}", crate::locations::describe(e)))
}

/// SHA-256 and length of what is at `key`, which should be `size` bytes (a length found otherwise is returned as it is)
async fn read_hash(dst: &dyn Storage, key: &str, size: u64) -> std::io::Result<(String, u64)> {
    match dst.stat(key).await? {
        None => return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "not there")),
        Some(e) if e.size != size => return Ok((String::new(), e.size)),
        Some(_) => {}
    }
    let mut reader = dst.open_at(key, 0, size).await?;
    crate::hashing::read_async(&mut reader).await
}

/// Deletes a set from its destination: its content, its snapshots, anything else in its folder, then set.json; then
/// its records. It can be paused, not cancelled (a set half deleted can't be restored from).
pub async fn remove(cx: &Ctx<'_>) -> AppResult<Stop> {
    let (st, job) = (cx.st, cx.job);
    let set = super::load_set(&st.db, &job.set_id).await?;
    crate::locations::probe(st, &set.dest_location)
        .await
        .map_err(|e| AppError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, format!("The copy's location can't be reached: {e}")))?;
    let dst = st.storage(&set.dest_location)?;
    let (files, bytes): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(size), 0) FROM backup_objects WHERE set_id = ?").bind(&set.id).fetch_one(&st.db).await?;
    cx.set_counts(0, 0, files, bytes);
    cx.flush().await?;
    loop {
        let rows: Vec<(String, i64)> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT hash, size FROM backup_objects WHERE set_id = ? ORDER BY hash LIMIT {PAGE}")))
                .bind(&set.id)
                .fetch_all(&st.db)
                .await?;
        if rows.is_empty() {
            break;
        }
        for (hash, size) in rows {
            if cx.stop().is_some() {
                return Ok(Stop::Paused);
            }
            let key = layout::object_key(&set.id, &hash);
            match cx.tries(|_: &std::io::Error| true, || dst.delete_at(&key)).await {
                Ok(()) => {}
                Err(Ok(_)) => return Ok(Stop::Paused),
                Err(Err(e)) => return Err(delete_error(&e)),
            }
            {
                let _w = st.write_lock.lock().await;
                sqlx::query("DELETE FROM backup_objects WHERE set_id = ? AND hash = ?").bind(&set.id).bind(&hash).execute(&st.db).await?;
            }
            cx.done(1, size).await?;
        }
    }
    let snapshots: Vec<(String,)> = sqlx::query_as("SELECT id FROM backup_snapshots WHERE set_id = ?").bind(&set.id).fetch_all(&st.db).await?;
    for (id,) in &snapshots {
        // The completion marker first: a manifest without it is never taken for a restore point
        for key in [layout::complete_key(&set.id, id), layout::manifest_key(&set.id, id)] {
            dst.delete_at(&key).await.map_err(|e| delete_error(&e))?;
        }
        let _ = tokio::fs::remove_file(layout::cached_manifest(st, id)).await;
    }
    // Anything else in the set's folder: content stored by a run that stopped before recording it
    if let Some(stop) = sweep(cx, dst.as_ref(), &layout::set_dir(&set.id)).await? {
        return Ok(stop);
    }
    dst.delete_at(&layout::set_file(&set.id)).await.map_err(|e| delete_error(&e))?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        sqlx::query("DELETE FROM backup_folder_files WHERE set_id = ?").bind(&set.id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM backup_objects WHERE set_id = ?").bind(&set.id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM backup_sets WHERE id = ?").bind(&set.id).execute(&mut *tx).await?;
        super::runner::finish(&mut tx, cx, None).await?;
        super::log(&mut tx, job, "backup_delete", &job.label).await?;
        AppResult::Ok(())
    }
    .await;
    crate::db::settle(tx, res).await?;
    Ok(Stop::Done)
}

fn delete_error(e: &std::io::Error) -> AppError {
    AppError::new(axum::http::StatusCode::BAD_GATEWAY, format!("Couldn't delete from the copy's location: {}", crate::locations::describe(e)))
}

/// Deletes the files below `dir` (set.json last, by the caller); links are deleted themselves, never followed
async fn sweep(cx: &Ctx<'_>, dst: &dyn Storage, dir: &str) -> AppResult<Option<Stop>> {
    let mut stack = vec![dir.to_string()];
    while let Some(at) = stack.pop() {
        let entries = match dst.list_dir(&at).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(delete_error(&e)),
        };
        for e in entries {
            if cx.stop().is_some() {
                return Ok(Some(Stop::Paused));
            }
            let key = format!("{at}/{}", e.name);
            match e.kind {
                EntryKind::Folder => stack.push(key),
                _ if at == dir && e.name == "set.json" => {}
                _ => dst.delete_at(&key).await.map_err(|e| delete_error(&e))?,
            }
        }
    }
    Ok(None)
}
