//! Legal holds and what a space's retention period does to an item, as the pages see them (the rules themselves are
//! in tree/kept.rs). An administrator places a hold on a folder or file, a whole space, or the files a person owns, with
//! a reason, and releases it again; both go in the activity log with the reason. Managers of an item can see what keeps it.

use std::collections::HashMap;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::header,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    auth::{Admin, User},
    error::{AppError, AppResult},
    logs,
    state::AppState,
    tree::{self, Need},
    util::{new_id, now},
};

/// Longest reason a hold is given
const MAX_REASON: usize = 500;
/// Held items listed or exported at most, so one request can't read a whole large space
const MAX_ITEMS: i64 = 100_000;

#[derive(Serialize, sqlx::FromRow)]
pub struct Hold {
    id: String,
    scope: String,
    /// What it was placed on, as it was named then
    label: String,
    reason: String,
    created_by_name: String,
    created_at: i64,
    released_at: Option<i64>,
    released_by_name: Option<String>,
    node_id: Option<String>,
    drive_id: Option<String>,
    user_id: Option<i64>,
    /// What the hold names still exists
    present: bool,
}

const HOLD_COLS: &str = "h.id, h.scope, h.label, h.reason, h.created_by_name, h.created_at, h.released_at, h.released_by_name, h.node_id, h.drive_id, h.user_id,
     CASE h.scope WHEN 'node' THEN EXISTS (SELECT 1 FROM nodes WHERE id = h.node_id)
                  WHEN 'space' THEN EXISTS (SELECT 1 FROM drives WHERE id = h.drive_id)
                  ELSE EXISTS (SELECT 1 FROM users WHERE id = h.user_id) END AS present";

#[derive(Deserialize)]
pub struct ListQuery {
    /// active (default), released, or all
    state: Option<String>,
    q: Option<String>,
}

/// Control panel › Retention: the holds, newest first
pub async fn list(State(st): State<AppState>, Admin(_): Admin, Query(q): Query<ListQuery>) -> AppResult<Json<Value>> {
    let filter = match q.state.as_deref() {
        Some("released") => "AND h.released_at IS NOT NULL",
        Some("all") => "",
        _ => "AND h.released_at IS NULL",
    };
    let search = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| format!("%{}%", s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")));
    let sql = format!(
        "SELECT {HOLD_COLS} FROM holds h WHERE (?1 IS NULL OR h.label LIKE ?1 ESCAPE '\\' OR h.reason LIKE ?1 ESCAPE '\\') {filter}
         ORDER BY h.created_at DESC, h.rowid DESC LIMIT 1000"
    );
    let holds: Vec<Hold> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(search).fetch_all(&st.db).await?;
    let spaces: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT id, name, retention_days FROM drives WHERE retention_days > 0 ORDER BY name").fetch_all(&st.db).await?;
    Ok(Json(json!({
        "holds": holds,
        "retention": spaces.into_iter().map(|(id, name, days)| json!({ "id": id, "name": name, "days": days })).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
pub struct PlaceReq {
    /// node, space or user
    scope: String,
    node_id: Option<String>,
    drive_id: Option<String>,
    user_id: Option<i64>,
    reason: String,
}

fn clean_reason(reason: &str) -> AppResult<String> {
    let reason = reason.trim();
    if reason.is_empty() || reason.chars().count() > MAX_REASON || reason.chars().any(|c| c.is_control() && c != '\n') {
        return Err(AppError::bad_request(format!("Give the reason for the hold, in up to {MAX_REASON} characters")));
    }
    Ok(reason.to_string())
}

/// Places a hold
pub async fn place(State(st): State<AppState>, Admin(me): Admin, Json(req): Json<PlaceReq>) -> AppResult<Json<Hold>> {
    let reason = clean_reason(&req.reason)?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let me = crate::auth::require_admin(&mut tx, &me).await?;
    let (label, node) = match req.scope.as_str() {
        "node" => {
            let id = req.node_id.as_deref().ok_or_else(|| AppError::bad_request("Choose the folder or file to hold"))?;
            let node = tree::get_node(&mut tx, id).await?.ok_or_else(|| AppError::not_found("Item not found"))?;
            // The top folder of a space is held by holding the space
            if node.parent_id.is_none() {
                return Err(AppError::bad_request("Hold the whole space instead of its top folder"));
            }
            let space: (String,) = sqlx::query_as("SELECT name FROM drives WHERE id = ?").bind(node.drive()).fetch_one(&mut *tx).await?;
            let path = tree::path_of(&mut tx, &node.id).await?.into_iter().map(|c| c.name).collect::<Vec<_>>().join("/");
            (format!("{}/{path}", space.0), Some(node))
        }
        "space" => {
            let id = req.drive_id.as_deref().ok_or_else(|| AppError::bad_request("Choose the space to hold"))?;
            let drive = tree::get_drive(&mut tx, id).await?.ok_or_else(|| AppError::not_found("Space not found"))?;
            (drive.name, None)
        }
        "user" => {
            let id = req.user_id.ok_or_else(|| AppError::bad_request("Choose the person whose files to hold"))?;
            let name: Option<(String,)> = sqlx::query_as("SELECT username FROM users WHERE id = ?").bind(id).fetch_optional(&mut *tx).await?;
            (name.ok_or_else(|| AppError::not_found("User not found"))?.0, None)
        }
        _ => return Err(AppError::bad_request("Choose what to hold: a folder or file, a space, or a person's files")),
    };
    let id = new_id();
    let (node_id, drive_id, user_id) = match req.scope.as_str() {
        "node" => (req.node_id.clone(), None, None),
        "space" => (None, req.drive_id.clone(), None),
        _ => (None, None, req.user_id),
    };
    sqlx::query(
        "INSERT INTO holds (id, scope, node_id, drive_id, user_id, label, reason, created_by, created_by_name, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&req.scope)
    .bind(node_id)
    .bind(drive_id)
    .bind(user_id)
    .bind(&label)
    .bind(&reason)
    .bind(me.id)
    .bind(&me.username)
    .bind(now())
    .execute(&mut *tx)
    .await?;
    logs::record_activity(&mut tx, &me, node.as_ref().map(|n| n as &dyn logs::Item), "hold_place", &format!("{label}: {reason}")).await?;
    let sql = format!("SELECT {HOLD_COLS} FROM holds h WHERE h.id = ?");
    let hold: Hold = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(&id).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(hold))
}

/// Releases a hold (it stays in the list, as released, with who released it and when)
pub async fn release(State(st): State<AppState>, Admin(me): Admin, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let me = crate::auth::require_admin(&mut tx, &me).await?;
    let row: Option<(String, String, Option<String>)> =
        sqlx::query_as("SELECT label, reason, node_id FROM holds WHERE id = ? AND released_at IS NULL").bind(&id).fetch_optional(&mut *tx).await?;
    let Some((label, reason, node_id)) = row else { return Err(AppError::not_found("Hold not found")) };
    sqlx::query("UPDATE holds SET released_at = ?, released_by_name = ? WHERE id = ?").bind(now()).bind(&me.username).bind(&id).execute(&mut *tx).await?;
    let node = match &node_id {
        Some(n) => tree::get_node(&mut tx, n).await?,
        None => None,
    };
    logs::record_activity(&mut tx, &me, node.as_ref().map(|n| n as &dyn logs::Item), "hold_release", &format!("{label}: {reason}")).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

/// A file a hold or a retention period keeps, as the list and the export show it
#[derive(Serialize, sqlx::FromRow)]
pub struct HeldItem {
    id: String,
    name: String,
    space: String,
    size: i64,
    updated_at: i64,
    owner: String,
    #[sqlx(skip)]
    path: String,
}

#[derive(Deserialize)]
pub struct ItemsQuery {
    limit: Option<i64>,
}

/// The files a hold covers (first `limit`), with their places
async fn items_of(st: &AppState, id: &str, limit: i64) -> AppResult<(Vec<HeldItem>, bool)> {
    let mut conn = st.db.acquire().await?;
    /// What a hold names: its scope, and the item, space or person
    type Names = (String, Option<String>, Option<String>, Option<i64>);
    let hold: Option<Names> = sqlx::query_as("SELECT scope, node_id, drive_id, user_id FROM holds WHERE id = ?").bind(id).fetch_optional(&mut *conn).await?;
    let Some((scope, node, drive, user)) = hold else { return Err(AppError::not_found("Hold not found")) };
    let cols = "n.id, n.name, d.name AS space, n.size, n.updated_at, COALESCE((SELECT username FROM users WHERE id = n.owner_id), '') AS owner";
    let rows: Vec<HeldItem> = match scope.as_str() {
        "node" => {
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "WITH RECURSIVE down(id) AS (SELECT ?1 UNION ALL SELECT n.id FROM down JOIN nodes n ON n.parent_id = down.id AND n.trash_root = 0)
                 SELECT {cols} FROM down JOIN nodes n ON n.id = down.id JOIN drives d ON d.id = n.drive_id WHERE n.kind = 'file' ORDER BY n.id LIMIT ?2"
            )))
            .bind(node)
            .bind(limit + 1)
            .fetch_all(&mut *conn)
            .await?
        }
        "space" => {
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "SELECT {cols} FROM nodes n JOIN drives d ON d.id = n.drive_id WHERE n.drive_id = ?1 AND n.kind = 'file' ORDER BY n.id LIMIT ?2"
            )))
            .bind(drive)
            .bind(limit + 1)
            .fetch_all(&mut *conn)
            .await?
        }
        _ => {
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "SELECT {cols} FROM nodes n JOIN drives d ON d.id = n.drive_id WHERE n.owner_id = ?1 AND n.kind = 'file' ORDER BY n.id LIMIT ?2"
            )))
            .bind(user)
            .bind(limit + 1)
            .fetch_all(&mut *conn)
            .await?
        }
    };
    let more = rows.len() as i64 > limit;
    let mut rows: Vec<HeldItem> = rows.into_iter().take(limit as usize).collect();
    let ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
    let mut paths: HashMap<String, Vec<tree::Crumb>> = tree::paths_of(&mut conn, &ids).await?;
    for r in &mut rows {
        r.path = paths.remove(&r.id).unwrap_or_default().into_iter().map(|c| c.name).collect::<Vec<_>>().join("/");
    }
    Ok((rows, more))
}

/// The files a hold covers, for its page in the Control panel
pub async fn items(State(st): State<AppState>, Admin(_): Admin, Path(id): Path<String>, Query(q): Query<ItemsQuery>) -> AppResult<Json<Value>> {
    let (items, more) = items_of(&st, &id, q.limit.unwrap_or(200).clamp(1, 1000)).await?;
    Ok(Json(json!({ "items": items, "more": more })))
}

fn csv_cell(s: &str) -> String {
    // A cell that a spreadsheet could take for a formula is written as text
    let s = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{s}") } else { s.to_string() };
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// The files a hold covers, as a CSV file
pub async fn export(State(st): State<AppState>, Admin(me): Admin, Path(id): Path<String>) -> AppResult<Response> {
    let (items, more) = items_of(&st, &id, MAX_ITEMS).await?;
    let mut out = String::from("\u{feff}Space,Path,Name,Owner,Size,Last changed\r\n");
    for i in &items {
        let changed = chrono_like(i.updated_at);
        out.push_str(&format!("{},{},{},{},{},{}\r\n", csv_cell(&i.space), csv_cell(&i.path), csv_cell(&i.name), csv_cell(&i.owner), i.size, changed));
    }
    if more {
        out.push_str(&format!("\"More files are covered; this list stops at {MAX_ITEMS}\"\r\n"));
    }
    let (label,): (String,) = sqlx::query_as("SELECT label FROM holds WHERE id = ?").bind(&id).fetch_one(&st.db).await?;
    let mut conn = st.db.acquire().await?;
    logs::record_activity(&mut conn, &me, None, "hold_export", &format!("{label}: {} files", items.len())).await?;
    Ok(([(header::CONTENT_TYPE, "text/csv; charset=utf-8"), (header::CONTENT_DISPOSITION, "attachment; filename=\"held-files.csv\"")], out).into_response())
}

/// A time as `2026-10-27 14:05` (UTC), for the export
fn chrono_like(ts: i64) -> String {
    let days = ts.div_euclid(86400);
    let secs = ts.rem_euclid(86400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

/// What keeps one item, for its Properties: the retention period of its space, and the holds on it (placed on it, on a
/// folder above it, on its space, or on its owner). Managers of the item see it; only administrators place holds.
pub async fn of_node(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let mut conn = st.db.acquire().await?;
    let node = tree::node_for(&mut conn, &user, &id, Need::Read).await?;
    let role = tree::role_on(&mut conn, &user, &node).await?;
    if !user.is_admin() && !role.is_some_and(|r| r >= tree::Role::Manager) {
        return Err(AppError::forbidden("Only managers of this item can see what keeps it"));
    }
    let (days,): (i64,) = sqlx::query_as("SELECT retention_days FROM drives WHERE id = ?").bind(node.drive()).fetch_one(&mut *conn).await?;
    let holds: Vec<Hold> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "WITH RECURSIVE up(id) AS (SELECT ?1 UNION ALL SELECT n.parent_id FROM up JOIN nodes n ON n.id = up.id WHERE n.parent_id IS NOT NULL)
         SELECT {HOLD_COLS} FROM holds h
         WHERE h.released_at IS NULL AND (
               (h.scope = 'node' AND h.node_id IN (SELECT id FROM up))
            OR (h.scope = 'space' AND h.drive_id = ?2)
            OR (h.scope = 'user' AND h.user_id = ?3 AND ?4 = 'file'))
         ORDER BY h.created_at"
    )))
    .bind(&node.id)
    .bind(node.drive())
    .bind(node.owner_id)
    .bind(&node.kind)
    .fetch_all(&mut *conn)
    .await?;
    let kept = tree::kept::subtrees(&mut conn, std::slice::from_ref(&node.id)).await?.remove(&node.id);
    Ok(Json(json!({
        "retention_days": days,
        // The latest date a retention period keeps this item (or something in it) until
        "kept_until": kept.as_ref().and_then(|k| k.until),
        "holds": holds,
        "can_place": user.is_admin(),
    })))
}

#[cfg(test)]
mod tests;
