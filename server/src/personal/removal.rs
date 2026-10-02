//! Removing a personal space, when an administrator removes it or deletes its user: its files are moved into another
//! space or deleted

use serde::Deserialize;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
    tree,
};

#[derive(Deserialize, Default)]
pub struct DeleteQuery {
    /// The space to move the user's files to: they go into a new folder "Files of <username>" at its top
    move_to: Option<String>,
    /// Delete the files instead; one of the two must be chosen when the personal space holds anything
    #[serde(default)]
    delete_files: bool,
}

/// A personal space removed in a transaction (`remove_personal_in`)
pub struct Removed {
    /// What happened to its files, for the log ("files moved to …"); None when it was empty
    pub detail: Option<String>,
    /// Uploads into it that were cancelled: their temporary files go once the transaction is committed
    uploads: Vec<String>,
    folder: bool,
}

impl Removed {
    /// After committing: what is still in the space (everything when the files are deleted, the root folder and the
    /// trash after a move) is deleted in the background, a batch at a time
    pub async fn finish(self, st: &AppState) {
        tree::purge_detached_later(st);
        if self.folder {
            crate::folders::spaces_changed(st);
        }
        for u in self.uploads {
            let _ = tokio::fs::remove_file(st.tmp_dir().join(format!("upload-{u}"))).await;
        }
    }
}

/// The files of a personal space moved by `move_personal_first`, before the space is removed. The space stays
/// read-only (`drives.moving`) until the transaction that removes it; should that fail, `kept_on_error` makes it
/// writable again.
pub struct Moved {
    /// Where the files went, for the log
    place: String,
    drive_id: String,
}

impl Moved {
    /// Passes on the result of removing the space; when it failed, the space stays and is writable again
    pub async fn kept_on_error<T>(moved: Option<&Moved>, st: &AppState, res: AppResult<T>) -> AppResult<T> {
        if res.is_err()
            && let Some(m) = moved
        {
            release(st, &m.drive_id).await;
        }
        res
    }
}

/// Makes a personal space writable again when removing it failed
async fn release(st: &AppState, drive_id: &str) {
    let _w = st.write_lock.lock().await;
    let res = async {
        let mut tx = crate::db::begin_write(&st.db).await?;
        sqlx::query("UPDATE drives SET moving = 0 WHERE id = ?").bind(drive_id).execute(&mut *tx).await?;
        tx.commit().await
    }
    .await;
    if let Err(e) = res {
        tracing::warn!("Couldn't make the personal space {drive_id} writable again: {e}");
    }
}

/// At startup: personal spaces left read-only by a removal that ThirtyFile stopped in the middle of (and not by a move
/// to another storage location) are writable again; what wasn't moved yet is still in them
pub async fn release_interrupted(st: &AppState) {
    let rows: Result<Vec<(String,)>, _> = sqlx::query_as("SELECT id FROM drives WHERE kind = 'personal' AND moving = 1").fetch_all(&st.db).await;
    for (drive_id,) in rows.unwrap_or_default() {
        let busy = match st.db.acquire().await {
            Ok(mut c) => crate::moves::drive_busy(&mut c, &drive_id).await.unwrap_or(true),
            Err(_) => true,
        };
        if !busy {
            release(st, &drive_id).await;
        }
    }
}

/// Before removing a personal space whose files go to or come from a folder space: moves them on the disk first, item
/// by item (the removal can't happen halfway: should a move fail, the space stays, with what wasn't moved yet). The
/// space is read-only from the moment its items are listed until it is removed, so nothing added meanwhile is lost
/// with it. Returns where they went; None when nothing had to move this way.
pub async fn move_personal_first(
    st: &AppState,
    me: &crate::auth::User,
    user_id: i64,
    username: &str,
    q: &DeleteQuery,
    progress: &crate::jobs::Tracker,
) -> AppResult<Option<Moved>> {
    if q.move_to.is_some() && q.delete_files {
        return Err(AppError::bad_request("Choose either to move the user's files or to delete them"));
    }
    let Some(target) = &q.move_to else { return Ok(None) };
    let mut c = st.db.acquire().await?;
    let own: Option<(String,)> = sqlx::query_as("SELECT id FROM drives WHERE kind = 'personal' AND owner_id = ?").bind(user_id).fetch_optional(&mut *c).await?;
    let Some(own) = (match own {
        Some((d,)) => tree::get_drive(&mut c, &d).await?,
        None => None,
    }) else {
        return Ok(None);
    };
    let target = tree::get_drive(&mut c, target).await?.ok_or_else(|| AppError::not_found("Space not found"))?;
    // Neither may be on its way to another storage location
    crate::moves::refuse_busy(&mut c, &own.id).await?;
    crate::moves::refuse_busy(&mut c, &target.id).await?;
    drop(c);
    if !(own.is_folder() || target.is_folder()) {
        return Ok(None);
    }
    let place = move_personal_across(st, me, username, &own, &target, progress).await?;
    Ok(Some(Moved { place, drive_id: own.id }))
}

/// Removes the personal space of user `id` in the transaction (under the write lock), when deleting the user or only
/// their space: its files are moved into a new folder "Files of <username>" in the space `q.move_to`, or deleted with
/// it (`q.delete_files`); one of the two must be chosen when it holds anything. `moved`: what `move_personal_first`
/// moved already; the space must hold nothing else by now. The user is left without a personal space, and without one waiting to be created. None when
/// they had none. The caller calls `Removed::finish` after committing.
pub async fn remove_personal_in(
    tx: &mut sqlx::SqliteConnection,
    me: &crate::auth::User,
    id: i64,
    username: &str,
    q: &DeleteQuery,
    moved: Option<&Moved>,
) -> AppResult<Option<Removed>> {
    let personal: Option<(String, String, Option<String>)> =
        sqlx::query_as("SELECT id, root_id, CASE WHEN mode = 'folder' THEN source_path END FROM drives WHERE kind = 'personal' AND owner_id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((drive_id, root_id, folder)) = personal else {
        sqlx::query("UPDATE users SET personal_pending = NULL WHERE id = ?").bind(id).execute(&mut *tx).await?;
        return Ok(None);
    };
    crate::moves::refuse_busy(tx, &drive_id).await?;
    if let Some(target) = &q.move_to {
        crate::moves::refuse_busy(tx, target).await?;
    }
    let (has_files,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM nodes WHERE parent_id = ?)").bind(&root_id).fetch_one(&mut *tx).await?;
    let detail = if let Some(m) = moved {
        // Checked again under the write lock: anything that got in after the files were listed (a scan of the folder,
        // say) would otherwise be deleted with the space
        let (left,): (bool,) =
            sqlx::query_as("SELECT EXISTS (SELECT 1 FROM nodes WHERE parent_id = ? AND trashed_at IS NULL)").bind(&root_id).fetch_one(&mut *tx).await?;
        if left || m.drive_id != drive_id {
            return Err(AppError::conflict("Something changed at the same time. Try again."));
        }
        Some(format!("files moved to {}", m.place))
    } else if has_files {
        match &q.move_to {
            Some(target) => Some(format!("files moved to {}", move_personal_files(tx, me, username, &drive_id, &root_id, target).await?)),
            None if q.delete_files => Some(match &folder {
                Some(path) => format!("files removed, their folder on the server is kept: {path}"),
                None => "files deleted".to_string(),
            }),
            None => return Err(AppError::bad_request("Choose where to move the user's files, or choose to delete them")),
        }
    } else {
        None
    };
    let uploads: Vec<(String,)> = sqlx::query_as("SELECT id FROM uploads WHERE drive_id = ?").bind(&drive_id).fetch_all(&mut *tx).await?;
    sqlx::query("DELETE FROM uploads WHERE drive_id = ?").bind(&drive_id).execute(&mut *tx).await?;
    // The space disappears now; what is still in it is deleted in the background (`Removed::finish`)
    sqlx::query("DELETE FROM drives WHERE id = ?").bind(&drive_id).execute(&mut *tx).await?;
    sqlx::query("UPDATE users SET root_id = NULL, personal_pending = NULL WHERE id = ?").bind(id).execute(&mut *tx).await?;
    Ok(Some(Removed { detail, uploads: uploads.into_iter().map(|(u,)| u).collect(), folder: folder.is_some() }))
}

/// Checks the space a deleted user's files go to
fn check_target(target: &tree::Drive, drive_id: &str) -> AppResult<()> {
    if target.id == drive_id {
        return Err(AppError::bad_request("Choose a space other than the user's own"));
    }
    if target.disabled {
        return Err(AppError::bad_request("That space is turned off"));
    }
    Ok(())
}

/// Checks that `target` has room for the files of a personal space (not in the trash); returns their size
async fn check_room(conn: &mut sqlx::SqliteConnection, root_id: &str, target: &tree::Drive) -> AppResult<i64> {
    let sql = format!("{LIVE} SELECT COALESCE(SUM(size), 0) FROM nodes WHERE kind = 'file' AND id IN (SELECT id FROM sub)");
    let (bytes,): (i64,) = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(root_id).fetch_one(&mut *conn).await?;
    if let Err(e) = tree::check_quota(conn, &target.id, bytes).await {
        return Err(if e.code == Some("quota") {
            AppError::new(
                axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                format!("\"{}\" doesn't have room for {} more. Choose another space, or give it more space first.", target.name, crate::util::format_bytes(bytes)),
            )
            .with_code("quota")
        } else {
            e
        });
    }
    Ok(bytes)
}

/// How the log names the space the files went to
async fn space_label(conn: &mut sqlx::SqliteConnection, target: &tree::Drive) -> AppResult<String> {
    Ok(match target.kind {
        tree::SpaceKind::Personal => {
            let (owner,): (String,) =
                sqlx::query_as("SELECT COALESCE((SELECT username FROM users WHERE id = ?), '')").bind(target.owner_id).fetch_one(&mut *conn).await?;
            format!("My files of {owner}")
        }
        _ => target.name.clone(),
    })
}

/// Everything not in the trash, below the root folder `?1`
const LIVE: &str = "WITH RECURSIVE sub(id) AS (
       SELECT id FROM nodes WHERE parent_id = ?1 AND trashed_at IS NULL
       UNION ALL SELECT c.id FROM nodes c JOIN sub ON c.parent_id = sub.id WHERE c.trashed_at IS NULL
     )";

/// Moves everything in a personal space into a new folder "Files of <username>" at the top of another space when one
/// of the two is a folder space: item by item, the way the web moves items between spaces (renamed on the same disk,
/// else copied; items keep their ids). The trash stays behind. Returns where they went (for the log).
async fn move_personal_across(
    st: &AppState,
    me: &crate::auth::User,
    username: &str,
    own: &tree::Drive,
    target: &tree::Drive,
    progress: &crate::jobs::Tracker,
) -> AppResult<String> {
    check_target(target, &own.id)?;
    // Scans of the two spaces wait meanwhile (always locked in the same order)
    let mut spaces: Vec<&str> = [own, target].iter().filter(|d| d.is_folder()).map(|d| d.id.as_str()).collect();
    spaces.sort_unstable();
    let mut _scans = Vec::new();
    for d in spaces {
        _scans.push(crate::fsops::lock_space(st, d).await);
        crate::fsops::ready(st, d).await?;
    }
    let (dest, items, label) = {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        check_room(&mut tx, &own.root_id, target).await?;
        let top = tree::get_node(&mut tx, &target.root_id).await?.ok_or_else(|| AppError::not_found("Space not found"))?;
        let wanted = format!("Files of {username}");
        let name = if target.is_folder() {
            crate::fsops::free_name(&mut tx, &top, &wanted, true).await?
        } else {
            tree::unique_name(&mut tx, &top.id, &wanted, true).await?
        };
        let folder = crate::content::create_folder(&mut tx, me.id, &top.id, &name).await?;
        let dest = tree::get_node(&mut tx, &folder).await?.unwrap();
        let (tops,): (String,) = sqlx::query_as("SELECT COALESCE(json_group_array(id), '[]') FROM nodes WHERE parent_id = ? AND trashed_at IS NULL")
            .bind(&own.root_id)
            .fetch_one(&mut *tx)
            .await?;
        // Each item with everything in it; in the content store, names that differ only in letter case (possible in a
        // folder) get a number
        let mut items = Vec::new();
        let mut taken = std::collections::HashSet::new();
        for id in serde_json::from_str::<Vec<String>>(&tops).unwrap_or_default() {
            let mut nodes: Vec<tree::Node> = tree::subtree(&mut tx, &id).await?.into_iter().map(|(n, _)| n).filter(|n| n.trashed_at.is_none()).collect();
            let top = &mut nodes[0];
            let base = top.name.clone();
            let mut k = 0;
            while !target.is_folder() && !taken.insert(top.name.to_lowercase()) {
                k += 1;
                top.name = crate::util::numbered_name(&base, k, top.is_folder());
            }
            items.push(nodes);
        }
        let label = format!("{} › {name}", space_label(&mut tx, target).await?);
        // Read-only from now until the space is removed: an item added later wouldn't be moved, and would be deleted
        // with the space. Another removal under way already is left to finish.
        let marked = sqlx::query("UPDATE drives SET moving = 1 WHERE id = ? AND moving = 0").bind(&own.id).execute(&mut *tx).await?;
        if marked.rows_affected() == 0 {
            return Err(AppError::conflict("Something changed at the same time. Try again."));
        }
        tx.commit().await?;
        (dest, items, label)
    };
    if let Some(across) = crate::fsops::Across::new(dest, items, true)
        && let Err(e) = across.run(st, me, progress).await
    {
        release(st, &own.id).await;
        return Err(e);
    }
    Ok(label)
}

/// Moves everything in a personal space into a new folder "Files of <username>" at the top of another space, when
/// both are in the content store; returns where they went (for the log). Nothing is copied: the top items get the new
/// folder as their parent, and one recursive update moves the whole tree to the other space, so even a large space
/// takes one short transaction. The trash isn't moved: it stays behind in the personal space and is deleted with it
async fn move_personal_files(
    conn: &mut sqlx::SqliteConnection,
    me: &crate::auth::User,
    username: &str,
    drive_id: &str,
    root_id: &str,
    target_id: &str,
) -> AppResult<String> {
    let target = tree::get_drive(conn, target_id).await?.ok_or_else(|| AppError::not_found("Space not found"))?;
    check_target(&target, drive_id)?;
    let bytes = check_room(conn, root_id, &target).await?;
    let name = tree::unique_name(conn, &target.root_id, &format!("Files of {username}"), true).await?;
    let folder = crate::content::create_folder(conn, me.id, &target.root_id, &name).await?;
    let sql = format!("{LIVE} UPDATE nodes SET drive_id = ?2 WHERE id IN (SELECT id FROM sub)");
    sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).bind(root_id).bind(&target.id).execute(&mut *conn).await?;
    sqlx::query("UPDATE nodes SET parent_id = ? WHERE parent_id = ? AND trashed_at IS NULL").bind(&folder).bind(root_id).execute(&mut *conn).await?;
    tree::adjust_usage(conn, drive_id, -bytes).await?;
    tree::adjust_usage(conn, &target.id, bytes).await?;
    Ok(format!("{} › {name}", space_label(conn, &target).await?))
}
