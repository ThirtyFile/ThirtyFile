//! Control panel › Users: adding a personal space to a user, and removing it (personal/)

use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::Deserialize;
use sqlx::SqliteConnection;

use crate::{
    admin::UserRow,
    auth::Admin,
    error::{AppError, AppResult},
    jobs::{self, Job, Limit},
    logs,
    personal::{DeleteQuery, check_ahead, create, policy_location},
    state::AppState,
};

#[derive(Deserialize, Default)]
pub struct AddReq {
    /// The storage location; None = the policy's (Control panel › General)
    #[serde(default)]
    pub(crate) location_id: Option<String>,
}

async fn location_name(conn: &mut SqliteConnection, id: &str) -> AppResult<String> {
    let (name,): (String,) = sqlx::query_as("SELECT COALESCE((SELECT name FROM storage_locations WHERE id = ?1), ?1)").bind(id).fetch_one(&mut *conn).await?;
    Ok(name)
}

/// Gives a user a personal space now (it replaces one waiting for its location). Fails, changing nothing, when the
/// location's folder isn't available.
pub async fn add(State(st): State<AppState>, Admin(me): Admin, Path(id): Path<i64>, Json(req): Json<AddReq>) -> AppResult<Json<UserRow>> {
    check_ahead(&st, Some(true), req.location_id.as_deref()).await;
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = add_in(&st, &mut tx, &me, id, &req).await;
        // Creating the space can fail after writing (a folder that isn't available): rolled back before the lock goes
        crate::db::settle(tx, res).await?;
    }
    crate::folders::spaces_changed(&st);
    Ok(Json(crate::admin::get_row(&st, id).await?))
}

async fn add_in(st: &AppState, tx: &mut SqliteConnection, me: &crate::auth::User, id: i64, req: &AddReq) -> AppResult<()> {
    let row: Option<(String, Option<String>)> = sqlx::query_as("SELECT username, root_id FROM users WHERE id = ?").bind(id).fetch_optional(&mut *tx).await?;
    let (username, root) = row.ok_or_else(|| AppError::not_found("User not found"))?;
    if root.is_some() {
        return Err(AppError::conflict("This user already has a personal space"));
    }
    let location = match req.location_id.as_deref().filter(|l| !l.is_empty()) {
        Some(l) => {
            crate::db::check_location(tx, l).await?;
            l.to_string()
        }
        None => policy_location(st, tx).await?,
    };
    create(tx, st.space_folders.as_deref(), id, &location).await?;
    let name = location_name(tx, &location).await?;
    logs::record_activity(tx, me, None, "user_update", &format!("{username}: added My files on {name}")).await?;
    Ok(())
}

/// Removes a user's personal space: its files are moved into a folder "Files of <username>" in another space
/// (`move_to`), or deleted (`delete_files`), as when deleting the user. Also cancels one waiting for its location.
/// Moving the files to or from a folder space copies them, which can take long: the removal runs as a job (jobs.rs),
/// which the page follows.
pub async fn remove(State(st): State<AppState>, Admin(me): Admin, Path(id): Path<i64>, Query(q): Query<DeleteQuery>) -> AppResult<Json<Job>> {
    let username = crate::admin::get_row(&st, id).await?.username;
    let pending = jobs::reserve(&st, me.id, "remove_personal", Limit::Changes)?;
    let job = pending.run(jobs::wait(), move |t| async move { remove_now(&st, &me, id, &username, &q, &t).await.map(|()| Default::default()) }).await?;
    Ok(Json(job))
}

async fn remove_now(st: &AppState, me: &crate::auth::User, id: i64, username: &str, q: &DeleteQuery, progress: &jobs::Tracker) -> AppResult<()> {
    let moved = crate::personal::move_personal_first(st, me, id, username, q, progress).await?;
    let res = {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = remove_in(&mut tx, me, id, username, q, moved.as_ref()).await;
        // Rolled back before the lock goes when it fails (`db::settle`)
        crate::db::settle(tx, res).await
    };
    let removed = crate::personal::Moved::kept_on_error(moved.as_ref(), st, res).await?;
    if let Some(removed) = removed {
        removed.finish(st).await;
    }
    Ok(())
}

async fn remove_in(
    tx: &mut SqliteConnection,
    me: &crate::auth::User,
    id: i64,
    username: &str,
    q: &DeleteQuery,
    moved: Option<&crate::personal::Moved>,
) -> AppResult<Option<crate::personal::Removed>> {
    let (pending,): (Option<String>,) = sqlx::query_as("SELECT personal_pending FROM users WHERE id = ?").bind(id).fetch_one(&mut *tx).await?;
    let removed = crate::personal::remove_personal_in(tx, me, id, username, q, moved).await?;
    let detail = match &removed {
        Some(r) => match &r.detail {
            Some(d) => format!("{username}: removed My files, {d}"),
            None => format!("{username}: removed My files"),
        },
        None if pending.is_some() => format!("{username}: stopped waiting to create My files"),
        None => return Err(AppError::bad_request("This user doesn't have a personal space")),
    };
    logs::record_activity(tx, me, None, "user_update", &detail).await?;
    Ok(removed)
}
