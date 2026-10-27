//! Storage analytics: what takes the space of a space. A task reads the space and keeps the result as a snapshot for the
//! person who asked (what each person may see differs where there are restricted folders), so the page opens at once
//! and says when it was made. Who may look: the owner and managers of a space, administrators for team and company
//! spaces, and each person for their own My files (never anybody else's).

use axum::{
    Json,
    extract::{Path, State},
};
use serde_json::{Value, json};

use crate::{
    auth::User,
    error::{AppError, AppResult},
    jobs::{self, Limit},
    state::AppState,
    tree::{self, Drive, Role, SpaceKind},
    util::now,
};

pub mod compute;

/// The space, when `user` may look at what takes its room
pub async fn allowed(conn: &mut sqlx::SqliteConnection, user: &User, id: &str) -> AppResult<Drive> {
    let current = crate::auth::current_user(conn, user).await?;
    let drive = tree::get_drive(conn, id).await?.ok_or_else(|| AppError::not_found("Space not found"))?;
    if drive.disabled {
        return Err(AppError::not_found("Space not found"));
    }
    let may = match drive.kind {
        // Nobody else's My files, administrators included
        SpaceKind::Personal => drive.owner_id == Some(current.id) && !current.guest,
        SpaceKind::Company | SpaceKind::Team => {
            if current.is_admin() && !current.guest {
                true
            } else {
                let root = tree::get_node(conn, &drive.root_id).await?.ok_or_else(|| AppError::not_found("Space not found"))?;
                tree::role_on(conn, &current, &root).await?.is_some_and(|r| r >= Role::Manager)
            }
        }
    };
    if !may {
        return Err(AppError::forbidden("You don't have permission to see what takes the room of this space"));
    }
    Ok(drive)
}

/// The last snapshot made for this person, and the space it is about
pub async fn get(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let mut conn = st.db.acquire().await?;
    let drive = allowed(&mut conn, &user, &id).await?;
    let row: Option<(i64, String)> = sqlx::query_as("SELECT taken_at, data FROM analytics_snapshots WHERE drive_id = ? AND user_id = ?")
        .bind(&id)
        .bind(user.id)
        .fetch_optional(&mut *conn)
        .await?;
    let snapshot = row.and_then(|(_, data)| serde_json::from_str::<Value>(&data).ok());
    Ok(Json(json!({ "drive": { "id": drive.id, "name": drive.name, "kind": drive.kind, "folder_space": drive.is_folder() }, "snapshot": snapshot })))
}

/// Starts making a new snapshot: a task, so a large space doesn't make the request wait
pub async fn refresh(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<Json<jobs::Job>> {
    {
        let mut conn = st.db.acquire().await?;
        allowed(&mut conn, &user, &id).await?;
    }
    let job = jobs::start(&st, user.id, "analytics", Limit::Changes)?;
    let (state, person) = (st.clone(), user.clone());
    jobs::spawn(&st, &job, async move {
        let mut conn = state.db.acquire().await?;
        let drive = allowed(&mut conn, &person, &id).await?;
        let snapshot = compute::compute(&state, &mut conn, &person, &drive).await?;
        let data = serde_json::to_string(&snapshot).map_err(AppError::internal)?;
        drop(conn);
        let _w = state.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&state.db).await?;
        sqlx::query("INSERT OR REPLACE INTO analytics_snapshots (drive_id, user_id, taken_at, data) VALUES (?, ?, ?, ?)")
            .bind(&id)
            .bind(person.id)
            .bind(now())
            .bind(data)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(jobs::Outcome::default())
    });
    Ok(Json(job))
}

#[cfg(test)]
mod tests;
