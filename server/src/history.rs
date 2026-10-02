//! What people read of the logs about what they may see: a space's activity log (and its export), an item's history,
//! a share link's visits. The logs themselves (writing them, their settings and archives, the sign-in and error logs)
//! are in logs/; this asks spaces, items and share links who may read what.

use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde_json::Value;
use sqlx::{QueryBuilder, Sqlite};

use crate::{
    auth::User,
    error::{AppError, AppResult},
    logs::{AccessQuery, ActivityQuery, ActivityRow, CONTENT_ACTIONS, EXPORT_LIMIT, HISTORY_LIMIT, HistoryRow, page, page_size, query_access, query_activity},
    state::AppState,
    tree::{self, Role},
};

// ───────────── A space's activity log ─────────────

async fn authorize_activity(st: &AppState, user: &User, q: &ActivityQuery) -> AppResult<()> {
    if let Some(drive_id) = &q.drive_id {
        crate::drives::manageable_drive(&mut *st.db.acquire().await?, user, drive_id).await?;
        Ok(())
    } else if user.is_admin() {
        Ok(())
    } else {
        Err(AppError::forbidden("Administrator permission required"))
    }
}

/// With a space: visible to the space's managers; without: administrators see everything. The returned next loads the following page
pub async fn activity(State(st): State<AppState>, user: User, Query(q): Query<ActivityQuery>) -> AppResult<Json<Value>> {
    authorize_activity(&st, &user, &q).await?;
    let limit = page_size(q.limit);
    Ok(page(query_activity(&st, &q, user.id, limit).await?, limit, |r| r.id))
}

/// The activity log entries to export, as the person is shown them (`query_activity`: nothing named in someone else's
/// personal space)
pub async fn export_activity(State(st): State<AppState>, user: User, Query(q): Query<ActivityQuery>) -> AppResult<Json<Vec<ActivityRow>>> {
    authorize_activity(&st, &user, &q).await?;
    Ok(Json(query_activity(&st, &q, user.id, EXPORT_LIMIT).await?))
}

// ───────────── An item's history (Details pane) ─────────────

/// The most recent entries about an item, for anyone who can open it (the activity log itself is for space managers and
/// administrators). A folder's history also has the entries of everything now inside it, including items in its trash;
/// a space's root folder, those of the whole space. No IP addresses: the activity log doesn't record them.
pub async fn node_history(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<Json<Vec<HistoryRow>>> {
    let mut c = st.db.acquire().await?;
    let (node, role) = tree::node_with_role(&mut c, &user, &id).await?;
    let mut qb = QueryBuilder::<Sqlite>::new("SELECT a.id, a.at, a.username, a.node_id, a.node_name, a.action, a.detail FROM activity a WHERE ");
    if node.parent_id.is_none() {
        qb.push("a.drive_id = ").push_bind(node.drive().to_string()).push(" AND a.node_id IS NOT NULL");
    } else if node.is_folder() {
        qb.push(
            "a.node_id IN (WITH RECURSIVE sub(id) AS (
               SELECT ",
        )
        .push_bind(node.id.clone())
        .push(" UNION ALL SELECT n.id FROM nodes n JOIN sub ON n.parent_id = sub.id) SELECT id FROM sub)");
    } else {
        qb.push("a.node_id = ").push_bind(node.id.clone());
    }
    if role < Role::Manager {
        qb.push(" AND a.action IN (");
        let mut sep = qb.separated(", ");
        for a in CONTENT_ACTIONS {
            sep.push_bind(a);
        }
        qb.push(")");
    }
    qb.push(" ORDER BY a.id DESC LIMIT ").push_bind(HISTORY_LIMIT);
    Ok(Json(qb.build_query_as().fetch_all(&mut *c).await?))
}

// ───────────── Share link visit log ─────────────

/// Share link access records: standard users only see links they created; administrators can query everything
pub async fn share_access(State(st): State<AppState>, user: User, Query(mut q): Query<AccessQuery>) -> AppResult<Json<Value>> {
    // A link an administrator may only revoke is asked for by the handle they were given for it
    if let Some(id) = &q.share_id {
        let token = crate::shares::resolve(&st, &mut *st.db.acquire().await?, id).await?;
        q.share_id = Some(token.unwrap_or_default());
    }
    // Standard users can only query links they created (including records left by deleted links), and a link someone
    // else created that they may manage (a manager of its space, the owner of its item)
    let others = match &q.share_id {
        Some(id) if !user.is_admin() => crate::shares::may_manage(&st, &user, id).await?,
        _ => false,
    };
    let owner = if user.is_admin() || others { None } else { Some(user.id) };
    let limit = page_size(q.limit);
    Ok(page(query_access(&st, &q, owner, user.is_admin().then_some(user.id), limit).await?, limit, |r| r.id))
}
