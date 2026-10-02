//! Listing a folder: pages, sorting, the position of an item and selecting a range

use super::*;

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub(super) sort: Option<String>,
    pub(super) order: Option<String>,
    pub(super) folders_only: Option<bool>,
    /// Items per page (at most MAX_PAGE); without it, the whole list comes at once
    pub(super) limit: Option<i64>,
    /// The `next` of the previous page
    pub(super) after: Option<String>,
    /// Where the page starts, counted from the first item (instead of `after`), so a list can show any part of a large
    /// folder without loading what comes before it. The page then says how many items there are (`total`).
    pub(super) offset: Option<i64>,
}

/// Largest page of a folder or trash listing
pub const MAX_PAGE: i64 = 5000;

impl ListQuery {
    /// For listings anyone with a link can ask for: one page of at most MAX_PAGE items when no page size was given,
    /// so a single request can't make the server send a whole large folder
    pub fn paged(mut self) -> Self {
        self.limit = Some(self.limit.unwrap_or(MAX_PAGE));
        self
    }
}

/// A whole listing, or one page of it when the request gave a `limit`
#[derive(Serialize, Debug)]
#[serde(untagged)]
pub enum Listing<T> {
    All(Vec<T>),
    Page {
        items: Vec<T>,
        /// `after` for the next page; None on the last page
        next: Option<String>,
        /// How many items the whole list has (pages asked for by `offset`)
        #[serde(skip_serializing_if = "Option::is_none")]
        total: Option<i64>,
    },
}

impl<T> Listing<T> {
    pub(super) fn new(items: Vec<T>, limit: Option<i64>, next: Option<String>) -> Self {
        if limit.is_some() { Listing::Page { items, next, total: None } } else { Listing::All(items) }
    }

    /// Changes the items, keeping the page
    pub fn map<U>(self, f: impl FnOnce(Vec<T>) -> Vec<U>) -> Listing<U> {
        match self {
            Listing::All(items) => Listing::All(f(items)),
            Listing::Page { items, next, total } => Listing::Page { items: f(items), next, total },
        }
    }

    pub fn items_mut(&mut self) -> &mut Vec<T> {
        match self {
            Listing::All(items) | Listing::Page { items, .. } => items,
        }
    }

    #[cfg(test)]
    pub fn into_items(self) -> Vec<T> {
        match self {
            Listing::All(items) | Listing::Page { items, .. } => items,
        }
    }
}

/// Where a folder page ends: the sort values of its last item, so the next page starts right after it even when items
/// were added or removed in between (keyset paging). The browser gets it as opaque text.
#[derive(Serialize, Deserialize)]
pub(super) struct Cursor {
    pub(super) folder: bool,
    pub(super) key: SortValue,
    pub(super) name: String,
    pub(super) id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub(super) enum SortValue {
    Int(i64),
    Text(String),
}

pub(super) fn encode_cursor<T: Serialize>(c: &T) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(c).unwrap())
}

pub(super) fn decode_cursor<T: serde::de::DeserializeOwned>(s: &str) -> AppResult<T> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(|| AppError::bad_request("This list has changed. Reload it."))
}

/// The page size asked for, and where the page starts (only with a size)
pub(super) fn page_of<T: serde::de::DeserializeOwned>(limit: Option<i64>, after: Option<&str>) -> AppResult<(Option<i64>, Option<T>)> {
    let limit = limit.map(|l| l.clamp(1, MAX_PAGE));
    let after = match (after, limit) {
        (Some(a), Some(_)) => Some(decode_cursor(a)?),
        _ => None,
    };
    Ok((limit, after))
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum SortCol {
    Name,
    Size,
    Updated,
    Created,
    Type,
}

impl SortCol {
    pub(super) fn parse(sort: Option<&str>) -> Self {
        match sort {
            Some("size") => Self::Size,
            Some("updated") => Self::Updated,
            Some("created") => Self::Created,
            Some("type") => Self::Type,
            _ => Self::Name,
        }
    }

    pub(super) fn expr(self) -> &'static str {
        const EXT: &str = "CASE WHEN n.kind = 'file' AND length(rtrim(n.name, replace(n.name, '.', ''))) > 1
            THEN lower(substr(n.name, length(rtrim(n.name, replace(n.name, '.', ''))) + 1)) ELSE '' END";
        match self {
            Self::Size => "n.size",
            Self::Updated => "n.updated_at",
            Self::Created => "n.created_at",
            Self::Type => EXT,
            Self::Name => "n.name COLLATE natural_name",
        }
    }
}

/// Sorting of folder listings, folders first. Names sort naturally ("File 2" before "File 10", the `natural_name`
/// collation), and Type sorts by extension, as the column shows it (`extOf` in the browser: after the last dot, unless
/// the name starts with it). The id comes last so every item has a fixed place, which paging relies on.
///
/// The default order (by name, ascending) is the order of the index `nodes_listed`, so a page of a large folder is read
/// from it instead of sorting every item of the folder first: by name, the name isn't repeated as the tiebreak, which
/// would keep SQLite from seeing that.
pub fn order_clause(sort: Option<&str>, order: Option<&str>) -> String {
    let sort = SortCol::parse(sort);
    let col = sort.expr();
    let dir = if order == Some("desc") { "DESC" } else { "ASC" };
    let by_name = if sort == SortCol::Name { "" } else { "n.name COLLATE natural_name, " };
    format!("ORDER BY (n.kind = 'folder') DESC, {col} {dir}, {by_name}n.id")
}

/// The children of a folder (not in the trash) in the order of `order_clause`; with a limit, one page of them.
/// SQLite runs NODE_COLS' subqueries after sorting, only for the rows it returns: measured on a folder of 50,000
/// items, that is faster than joining the same tables for every row, with or without a limit.
pub async fn list_children(conn: &mut SqliteConnection, parent_id: &str, q: &ListQuery) -> AppResult<Listing<Node>> {
    let sort = SortCol::parse(q.sort.as_deref());
    let desc = q.order.as_deref() == Some("desc");
    let (limit, after) = page_of::<Cursor>(q.limit, q.after.as_deref())?;
    // A position only goes with a page size, and not with a cursor
    let offset = q.offset.filter(|_| limit.is_some() && after.is_none()).map(|o| o.max(0));
    let folders_only = q.folders_only == Some(true);
    let kind_filter = if folders_only { "AND n.kind = 'folder'" } else { "" };
    // The navigation pane shows an arrow only on folders with folders in them (the index nodes_subfolders answers it)
    let has_folders =
        if folders_only { ", EXISTS (SELECT 1 FROM nodes c WHERE c.parent_id = n.id AND c.kind = 'folder' AND c.trashed_at IS NULL) AS has_folders" } else { "" };
    let mut args = vec![Arg::Text(parent_id.to_string())];
    let keyset = match &after {
        Some(c) => format!("AND {}", after_cursor(sort, desc, c, &mut args)),
        None => String::new(),
    };
    let ext = if sort == SortCol::Type { sort.expr() } else { "NULL" };
    let sql = format!(
        "SELECT {NODE_COLS}{has_folders}, {ext} AS ext FROM nodes n
         WHERE n.parent_id = ?1 AND n.trashed_at IS NULL {kind_filter} {keyset} {} LIMIT ?{} OFFSET ?{}",
        order_clause(q.sort.as_deref(), q.order.as_deref()),
        args.len() + 1,
        args.len() + 2,
    );
    // SQLite reads a negative limit as no limit
    args.push(Arg::Int(limit.unwrap_or(-1)));
    args.push(Arg::Int(offset.unwrap_or(0)));
    let rows: Vec<SortRow> = bind_all(sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())), args).fetch_all(&mut *conn).await?;
    let next = match (limit, rows.last()) {
        (Some(l), Some(last)) if rows.len() as i64 == l => Some(encode_cursor(&last.cursor(sort))),
        _ => None,
    };
    let total = match offset {
        Some(_) => {
            let sql = format!("SELECT COUNT(*) FROM nodes n WHERE n.parent_id = ?1 AND n.trashed_at IS NULL {kind_filter}");
            Some(sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql.as_str())).bind(parent_id).fetch_one(&mut *conn).await?)
        }
        None => None,
    };
    let items = rows.into_iter().map(|r| r.node).collect();
    Ok(match total {
        Some(total) => Listing::Page { items, next, total: Some(total) },
        None => Listing::new(items, limit, next),
    })
}

/// A value bound to a query built at run time
pub(super) enum Arg {
    Int(i64),
    Text(String),
    Bool(bool),
}

pub(super) fn bind_all<'q, O>(
    mut query: sqlx::query::QueryAs<'q, sqlx::Sqlite, O, sqlx::sqlite::SqliteArguments>,
    args: Vec<Arg>,
) -> sqlx::query::QueryAs<'q, sqlx::Sqlite, O, sqlx::sqlite::SqliteArguments> {
    for a in args {
        query = match a {
            Arg::Int(v) => query.bind(v),
            Arg::Text(v) => query.bind(v),
            Arg::Bool(v) => query.bind(v),
        };
    }
    query
}

/// A listed item with what it is sorted by
#[derive(sqlx::FromRow)]
pub(super) struct SortRow {
    #[sqlx(flatten)]
    pub(super) node: Node,
    /// The extension when sorting by type (the other sort values are columns of the node)
    pub(super) ext: Option<String>,
}

impl SortRow {
    /// Where the item is in the listing's order
    pub(super) fn cursor(&self, sort: SortCol) -> Cursor {
        let n = &self.node;
        Cursor {
            folder: n.is_folder(),
            key: match sort {
                SortCol::Size => SortValue::Int(n.size),
                SortCol::Updated => SortValue::Int(n.updated_at),
                SortCol::Created => SortValue::Int(n.created_at),
                SortCol::Type => SortValue::Text(self.ext.clone().unwrap_or_default()),
                SortCol::Name => SortValue::Text(n.name.clone()),
            },
            name: n.name.clone(),
            id: n.id.clone(),
        }
    }
}

/// The condition "comes after `c` in the listing's order" (folders before files, then the sort column, the name and the
/// id), with its values added to `args`
pub(super) fn after_cursor(sort: SortCol, desc: bool, c: &Cursor, args: &mut Vec<Arg>) -> String {
    let col = sort.expr();
    let op = if desc { "<" } else { ">" };
    let at = args.len();
    let (f, k, nm, id) = (at + 1, at + 2, at + 3, at + 4);
    args.push(Arg::Bool(c.folder));
    args.push(match &c.key {
        SortValue::Int(v) => Arg::Int(*v),
        SortValue::Text(v) => Arg::Text(v.clone()),
    });
    args.push(Arg::Text(c.name.clone()));
    args.push(Arg::Text(c.id.clone()));
    format!(
        "((n.kind = 'folder') < ?{f} OR ((n.kind = 'folder') = ?{f} AND ({col} {op} ?{k} OR ({col} = ?{k}
          AND (n.name COLLATE natural_name > ?{nm} OR (n.name COLLATE natural_name = ?{nm} AND n.id > ?{id}))))))"
    )
}

/// An item of a folder (not in the trash) with what it is sorted by
pub(super) async fn sort_row(conn: &mut SqliteConnection, parent_id: &str, id: &str, sort: SortCol) -> AppResult<Option<SortRow>> {
    let ext = if sort == SortCol::Type { sort.expr() } else { "NULL" };
    let sql = format!("SELECT {NODE_COLS}, {ext} AS ext FROM nodes n WHERE n.id = ?1 AND n.parent_id = ?2 AND n.trashed_at IS NULL");
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(id).bind(parent_id).fetch_optional(&mut *conn).await?)
}

pub(super) async fn count_children(conn: &mut SqliteConnection, parent_id: &str) -> AppResult<i64> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM nodes WHERE parent_id = ? AND trashed_at IS NULL").bind(parent_id).fetch_one(&mut *conn).await?)
}

#[derive(Deserialize)]
pub struct PositionQuery {
    pub(super) item: String,
    pub(super) sort: Option<String>,
    pub(super) order: Option<String>,
}

#[derive(Serialize)]
pub struct Position {
    /// Where the item is in the folder's listing, counted from 0; None when it isn't in the folder (any more)
    pub(super) position: Option<i64>,
    pub(super) total: i64,
}

/// Where an item is in a folder's listing, so a list that shows only part of a large folder can go to it (the folder
/// the person came from, when going up)
pub async fn position(State(st): State<AppState>, user: User, Path(id): Path<String>, Query(q): Query<PositionQuery>) -> AppResult<Json<Position>> {
    let mut c = st.db.acquire().await?;
    let folder = tree::folder_for(&mut c, &user, &id, Need::Read).await?;
    let sort = SortCol::parse(q.sort.as_deref());
    let desc = q.order.as_deref() == Some("desc");
    let total = count_children(&mut c, &folder.id).await?;
    let Some(row) = sort_row(&mut c, &folder.id, &q.item, sort).await? else { return Ok(Json(Position { position: None, total })) };
    let mut args = vec![Arg::Text(folder.id.clone())];
    let after = after_cursor(sort, desc, &row.cursor(sort), &mut args);
    let sql = format!("SELECT COUNT(*) FROM nodes n WHERE n.parent_id = ?1 AND n.trashed_at IS NULL AND {after}");
    let (behind,): (i64,) = bind_all(sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())), args).fetch_one(&mut *c).await?;
    Ok(Json(Position { position: Some((total - 1 - behind).max(0)), total }))
}

#[derive(Deserialize, Clone)]
pub struct SelectReq {
    pub(super) sort: Option<String>,
    pub(super) order: Option<String>,
    /// The first and last selected item in that order; without them, from the first item or up to the last
    pub(super) from: Option<String>,
    pub(super) to: Option<String>,
    /// Items left out of the selection
    #[serde(default)]
    pub(super) except: Vec<String>,
    /// The `next` of the previous answer
    pub(super) after: Option<String>,
    pub(super) limit: Option<i64>,
}

#[derive(Serialize, Debug)]
pub struct Selected {
    pub(super) ids: Vec<String>,
    /// `after` for the next ids; None when these were the last
    pub(super) next: Option<String>,
}

/// Items selected in a large folder that isn't all loaded in the browser: every item (Select all), or the items from one
/// to another (Shift), less those left out. The browser gets their ids a batch at a time (at most MAX_BATCH, what a
/// change to several items takes at once) and changes each batch in turn, so it never holds them all. Each change still
/// checks what the person may do with each item.
pub async fn select(State(st): State<AppState>, user: User, Path(id): Path<String>, Json(req): Json<SelectReq>) -> AppResult<Json<Selected>> {
    let mut c = st.db.acquire().await?;
    let folder = tree::folder_for(&mut c, &user, &id, Need::Read).await?;
    let sort = SortCol::parse(req.sort.as_deref());
    let desc = req.order.as_deref() == Some("desc");
    let limit = req.limit.unwrap_or(MAX_BATCH as i64).clamp(1, MAX_BATCH as i64);
    let after: Option<Cursor> = req.after.as_deref().map(decode_cursor).transpose()?;
    let mut args = vec![Arg::Text(folder.id.clone()), Arg::Text(serde_json::to_string(&req.except).unwrap())];
    let mut conditions = Vec::new();
    let bound = async |c: &mut SqliteConnection, item: &str| -> AppResult<SortRow> {
        sort_row(c, &folder.id, item, sort).await?.ok_or_else(|| AppError::conflict("The selected items have changed. Select them again."))
    };
    match (&after, &req.from) {
        (Some(a), _) => conditions.push(after_cursor(sort, desc, a, &mut args)),
        (None, Some(from)) => {
            let first = bound(&mut c, from).await?;
            args.push(Arg::Text(first.node.id.clone()));
            let same = args.len();
            conditions.push(format!("(n.id = ?{same} OR {})", after_cursor(sort, desc, &first.cursor(sort), &mut args)));
        }
        (None, None) => {}
    }
    if let Some(to) = &req.to {
        let last = bound(&mut c, to).await?;
        args.push(Arg::Text(last.node.id.clone()));
        let same = args.len();
        conditions.push(format!("(n.id = ?{same} OR NOT {})", after_cursor(sort, desc, &last.cursor(sort), &mut args)));
    }
    let ext = if sort == SortCol::Type { sort.expr() } else { "NULL" };
    let sql = format!(
        "SELECT {NODE_COLS}, {ext} AS ext FROM nodes n
         WHERE n.parent_id = ?1 AND n.trashed_at IS NULL AND n.id NOT IN (SELECT value FROM json_each(?2)) {} {} LIMIT {}",
        conditions.iter().map(|w| format!("AND {w}")).collect::<String>(),
        order_clause(req.sort.as_deref(), req.order.as_deref()),
        limit + 1,
    );
    // One row more than asked for says whether there are more: a full last batch has no `next`, since asking again
    // after its items have changed would find the span's last item gone
    let mut rows: Vec<SortRow> = bind_all(sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())), args).fetch_all(&mut *c).await?;
    let more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let next = match rows.last() {
        Some(last) if more => Some(encode_cursor(&last.cursor(sort))),
        _ => None,
    };
    Ok(Json(Selected { ids: rows.into_iter().map(|r| r.node.id).collect(), next }))
}

pub async fn children(State(st): State<AppState>, user: User, Path(id): Path<String>, Query(q): Query<ListQuery>) -> AppResult<Json<Listing<Node>>> {
    let folder = tree::folder_for(&mut *st.db.acquire().await?, &user, &id, Need::Read).await?;
    if folder.in_folder_space() && q.after.is_none() && q.offset.unwrap_or(0) == 0 {
        // Changes made on the server's folder show up when the folder is opened (not again for each further page).
        // No connection is held meanwhile: syncing takes its own, and many folders opened at once would otherwise
        // use up the pool while each waits for a second one
        crate::folders::sync_opened(&st, &folder).await;
    }
    let mut c = st.db.acquire().await?;
    let mut list = list_children(&mut c, &folder.id, &q).await?;
    tree::mark_favorites(&mut c, user.id, list.items_mut()).await?;
    Ok(Json(list))
}
