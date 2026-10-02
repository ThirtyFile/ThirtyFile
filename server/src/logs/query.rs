//! Querying the logs: the activity log, an item's history, the share link visit log and the sign-in log. The three
//! logs share their filters and paging (`Filters`, `page`).

use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

use crate::{
    auth::User,
    error::{AppError, AppResult},
    state::AppState,
    tree::{self, Role},
};

// ───────────── Filters and paging ─────────────

/// A log query's conditions; a filter whose value is missing or blank is left out
pub(super) struct Filters(QueryBuilder<Sqlite>);

impl Filters {
    /// `sql` selects from the log, up to (not including) its WHERE clause
    pub(super) fn new(sql: impl Into<String>) -> Self {
        let mut qb = QueryBuilder::new(sql);
        qb.push(" WHERE 1 = 1");
        Filters(qb)
    }

    /// The column equals the value
    pub(super) fn eq<'t, T: sqlx::Encode<'t, Sqlite> + sqlx::Type<Sqlite>>(&mut self, col: &str, value: Option<T>) -> &mut Self {
        if let Some(v) = value {
            self.0.push(format_args!(" AND {col} = ")).push_bind(v);
        }
        self
    }

    /// One of the columns contains the text (partial match; `%` and `_` in it are literal characters, not wildcards)
    pub(super) fn contains(&mut self, cols: &[&str], text: Option<&str>) -> &mut Self {
        if let Some(t) = text.filter(|t| !t.trim().is_empty()) {
            let pat = format!("%{}%", crate::util::like_escape(t.trim()));
            self.0.push(" AND (");
            for (i, col) in cols.iter().enumerate() {
                if i > 0 {
                    self.0.push(" OR ");
                }
                self.0.push(format_args!("{col} LIKE ")).push_bind(pat.clone()).push(" ESCAPE '\\'");
            }
            self.0.push(")");
        }
        self
    }

    /// The column is one of the comma-separated values
    pub(super) fn one_of(&mut self, col: &str, list: Option<&str>) -> &mut Self {
        let values: Vec<&str> = list.unwrap_or_default().split(',').map(str::trim).filter(|v| !v.is_empty()).collect();
        if !values.is_empty() {
            self.0.push(format_args!(" AND {col} IN ("));
            let mut sep = self.0.separated(", ");
            for v in values {
                sep.push_bind(v);
            }
            self.0.push(")");
        }
        self
    }

    /// Time range (Unix seconds, start inclusive, end exclusive) on the `at` column
    pub(super) fn between(&mut self, at: &str, from: Option<i64>, to: Option<i64>) -> &mut Self {
        if let Some(f) = from {
            self.0.push(format_args!(" AND {at} >= ")).push_bind(f);
        }
        if let Some(t) = to {
            self.0.push(format_args!(" AND {at} < ")).push_bind(t);
        }
        self
    }

    /// One page, newest first: at most `limit` records, only those with an id below `before`
    pub(super) async fn fetch<T>(mut self, db: &SqlitePool, id: &str, before: Option<i64>, limit: i64) -> AppResult<Vec<T>>
    where
        T: Send + Unpin + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>,
    {
        if let Some(b) = before {
            self.0.push(format_args!(" AND {id} < ")).push_bind(b);
        }
        self.0.push(format_args!(" ORDER BY {id} DESC LIMIT ")).push_bind(limit);
        Ok(self.0.build_query_as().fetch_all(db).await?)
    }
}

/// Records per page in the log viewers
pub(super) fn page_size(limit: Option<i64>) -> i64 {
    limit.unwrap_or(100).clamp(1, 1000)
}

/// A page of records, and in `next` the `before` value that loads the following page (none after the last page)
pub(super) fn page<T: Serialize>(items: Vec<T>, limit: i64, id: fn(&T) -> i64) -> Json<Value> {
    let next = (items.len() as i64 == limit).then(|| items.last().map(id)).flatten();
    Json(json!({ "items": items, "next": next }))
}

// ───────────── Activity log queries ─────────────

#[derive(Serialize, sqlx::FromRow)]
pub struct ActivityRow {
    pub(super) id: i64,
    pub(super) at: i64,
    pub(super) username: String,
    pub(super) drive_id: Option<String>,
    pub(super) drive_name: Option<String>,
    pub(super) node_id: Option<String>,
    pub(super) node_name: String,
    pub(super) action: String,
    pub(super) detail: String,
}

#[derive(Deserialize, Default)]
pub struct ActivityQuery {
    pub(super) drive_id: Option<String>,
    /// Username (partial match)
    pub(super) user: Option<String>,
    /// Actions, comma-separated
    pub(super) action: Option<String>,
    /// Time range (Unix seconds, start inclusive, end exclusive)
    pub(super) from: Option<i64>,
    pub(super) to: Option<i64>,
    /// Keyword: item name or details
    pub(super) q: Option<String>,
    /// Paging: only records with an id below this value
    pub(super) before: Option<i64>,
    pub(super) limit: Option<i64>,
    /// Time zone for exports (minutes, same as JavaScript's getTimezoneOffset)
    pub(super) tz: Option<i64>,
}

pub(super) async fn authorize_activity(st: &AppState, user: &User, q: &ActivityQuery) -> AppResult<()> {
    if let Some(drive_id) = &q.drive_id {
        crate::drives::manageable_drive(&mut *st.db.acquire().await?, user, drive_id).await?;
        Ok(())
    } else if user.is_admin() {
        Ok(())
    } else {
        Err(AppError::forbidden("Administrator permission required"))
    }
}

pub(super) async fn query_activity(st: &AppState, q: &ActivityQuery, limit: i64) -> AppResult<Vec<ActivityRow>> {
    let mut f = Filters::new(
        "SELECT a.id, a.at, a.username, a.drive_id, d.name AS drive_name, a.node_id, a.node_name, a.action, a.detail
         FROM activity a LEFT JOIN drives d ON d.id = a.drive_id",
    );
    f.eq("a.drive_id", q.drive_id.as_deref())
        .contains(&["a.username"], q.user.as_deref())
        .one_of("a.action", q.action.as_deref())
        .between("a.at", q.from, q.to)
        .contains(&["a.node_name", "a.detail"], q.q.as_deref());
    f.fetch(&st.db, "a.id", q.before, limit).await
}

/// With a space: visible to the space's managers; without: administrators see everything. The returned next loads the following page
pub async fn activity(State(st): State<AppState>, user: User, Query(q): Query<ActivityQuery>) -> AppResult<Json<Value>> {
    authorize_activity(&st, &user, &q).await?;
    let limit = page_size(q.limit);
    Ok(page(query_activity(&st, &q, limit).await?, limit, |r| r.id))
}

// ───────────── An item's history (Details pane) ─────────────

/// Entries shown in an item's history
pub(super) const HISTORY_LIMIT: i64 = 50;

/// What happened to the item and its contents. People who can only view or edit it don't see sharing and permission
/// changes (who was given access, share links); those stay with managers, as in the space's activity log.
const CONTENT_ACTIONS: [&str; 9] = ["upload", "create_folder", "edit", "rename", "move", "copy", "trash", "restore", "delete"];

#[derive(Serialize, sqlx::FromRow, Debug)]
pub struct HistoryRow {
    pub(super) id: i64,
    pub(super) at: i64,
    pub(super) username: String,
    pub(super) node_id: Option<String>,
    pub(super) node_name: String,
    pub(super) action: String,
    pub(super) detail: String,
}

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

#[derive(Serialize, sqlx::FromRow)]
pub struct AccessRow {
    pub(super) id: i64,
    pub(super) at: i64,
    pub(super) share_id: String,
    pub(super) owner_name: Option<String>,
    pub(super) node_id: Option<String>,
    pub(super) node_name: String,
    pub(super) event: String,
    pub(super) ip: String,
    pub(super) user_agent: String,
    /// A visit to a link in someone else's personal space, shown to an administrator: the link's address and the item
    /// are left out
    pub(super) private: bool,
}

#[derive(Deserialize, Default)]
pub struct AccessQuery {
    pub(super) share_id: Option<String>,
    /// Sharer's username (partial match, for administrators)
    pub(super) owner: Option<String>,
    pub(super) event: Option<String>,
    pub(super) ip: Option<String>,
    /// Keyword: item name or link token
    pub(super) q: Option<String>,
    pub(super) from: Option<i64>,
    pub(super) to: Option<i64>,
    pub(super) before: Option<i64>,
    pub(super) limit: Option<i64>,
}

/// Visit records: only those of links `owner_id` created, if given. For the administrator `admin`, visits to links
/// someone else made in someone else's personal space (or in a space that is gone) don't say which link or item they
/// were, and the keyword search doesn't look at what they leave out.
pub(super) async fn query_access(st: &AppState, q: &AccessQuery, owner_id: Option<i64>, admin: Option<i64>, limit: i64) -> AppResult<Vec<AccessRow>> {
    let private = match admin {
        Some(me) => format!("(a.owner_id IS NOT {me} AND a.private_to IS NOT NULL AND a.private_to <> {me})"),
        None => "0".to_string(),
    };
    let mut f = Filters::new(format!(
        "SELECT * FROM (
           SELECT a.id, a.at, a.owner_id, a.share_id AS token, a.event, a.ip, a.user_agent, u.username AS owner_name, {private} AS private,
                  CASE WHEN {private} THEN '' ELSE a.share_id END AS share_id,
                  CASE WHEN {private} THEN NULL ELSE a.node_id END AS node_id,
                  CASE WHEN {private} THEN '' ELSE a.node_name END AS node_name
           FROM share_access a LEFT JOIN users u ON u.id = a.owner_id
         ) a"
    ));
    f.eq("a.owner_id", owner_id)
        .eq("a.token", q.share_id.as_deref())
        .contains(&["a.owner_name"], q.owner.as_deref())
        .one_of("a.event", q.event.as_deref())
        .contains(&["a.ip"], q.ip.as_deref())
        .contains(&["a.node_name", "a.share_id"], q.q.as_deref())
        .between("a.at", q.from, q.to);
    f.fetch(&st.db, "a.id", q.before, limit).await
}

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

// ───────────── Sign-in log ─────────────

#[derive(Serialize, sqlx::FromRow)]
pub struct LoginRow {
    pub(super) id: i64,
    pub(super) at: i64,
    pub(super) user_id: Option<i64>,
    pub(super) username: String,
    pub(super) event: String,
    pub(super) ip: String,
    pub(super) user_agent: String,
    pub(super) method: String,
}

#[derive(Deserialize, Default)]
pub struct LoginQuery {
    pub(super) user_id: Option<i64>,
    /// Username (partial match)
    pub(super) user: Option<String>,
    pub(super) event: Option<String>,
    pub(super) ip: Option<String>,
    pub(super) from: Option<i64>,
    pub(super) to: Option<i64>,
    pub(super) before: Option<i64>,
    pub(super) limit: Option<i64>,
    pub(super) tz: Option<i64>,
}

pub(super) async fn query_logins(st: &AppState, q: &LoginQuery, limit: i64) -> AppResult<Vec<LoginRow>> {
    let mut f = Filters::new("SELECT id, at, user_id, username, event, ip, user_agent, method FROM login_log");
    f.eq("user_id", q.user_id)
        .contains(&["username"], q.user.as_deref())
        .one_of("event", q.event.as_deref())
        .contains(&["ip"], q.ip.as_deref())
        .between("at", q.from, q.to);
    f.fetch(&st.db, "id", q.before, limit).await
}

/// Standard users can only see their own sign-in records; administrators can query everything
pub(super) fn scope_logins(user: &User, q: &mut LoginQuery) {
    if !user.is_admin() {
        q.user_id = Some(user.id);
        q.user = None;
    }
}

pub async fn login_log(State(st): State<AppState>, user: User, Query(mut q): Query<LoginQuery>) -> AppResult<Json<Value>> {
    scope_logins(&user, &mut q);
    let limit = page_size(q.limit);
    Ok(page(query_logins(&st, &q, limit).await?, limit, |r| r.id))
}
