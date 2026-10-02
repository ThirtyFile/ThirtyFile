//! Finding items: search, favorites, recent files and what is shared with me

use super::*;

#[derive(Deserialize)]
pub struct SearchQuery {
    pub(super) q: String,
    /// Only this folder and what's below it (a folder id); without it, everything the user can open
    #[serde(rename = "in")]
    pub(super) within: Option<String>,
    /// "folder" or "file"
    pub(super) kind: Option<String>,
    /// Extensions without the dot, comma separated (files only)
    pub(super) ext: Option<String>,
    /// Modified at or after / before (Unix seconds)
    pub(super) from: Option<i64>,
    pub(super) to: Option<i64>,
    /// Size in bytes (files only)
    pub(super) min_size: Option<i64>,
    pub(super) max_size: Option<i64>,
    /// Uploaded by (username, exact)
    pub(super) owner: Option<String>,
}

/// Results returned at most
pub(super) const SEARCH_LIMIT: i64 = 300;

#[derive(Serialize)]
pub struct SearchResult {
    pub(super) items: Vec<Located>,
    /// More matches than returned
    pub(super) truncated: bool,
}

/// Searches names in all spaces and shared folders I can access, or in one folder and below
pub async fn search(State(st): State<AppState>, user: User, Query(q): Query<SearchQuery>) -> AppResult<Json<SearchResult>> {
    let term = q.q.trim();
    if term.is_empty() {
        return Ok(Json(SearchResult { items: Vec::new(), truncated: false }));
    }
    let mut c = st.db.acquire().await?;
    let (drives, folders) = tree::scope(&mut c, &user).await?;
    let mut qb = sqlx::QueryBuilder::<sqlx::Sqlite>::new(format!("SELECT {NODE_COLS} FROM nodes n WHERE "));
    qb.push("(n.drive_id IN (SELECT value FROM json_each(").push_bind(drives).push("))");
    qb.push(" OR n.id IN (WITH RECURSIVE s(id) AS (SELECT value FROM json_each(").push_bind(folders);
    qb.push(") UNION ALL SELECT c.id FROM nodes c JOIN s ON c.parent_id = s.id) SELECT id FROM s))");
    qb.push(" AND n.trashed_at IS NULL AND n.parent_id IS NOT NULL");
    if let Some(within) = q.within.as_deref().filter(|w| !w.is_empty()) {
        let folder = tree::folder_for(&mut c, &user, within, Need::Read).await?;
        qb.push(" AND n.id IN (WITH RECURSIVE d(id) AS (SELECT id FROM nodes WHERE parent_id = ").push_bind(folder.id);
        qb.push(" AND trashed_at IS NULL UNION ALL SELECT c.id FROM nodes c JOIN d ON c.parent_id = d.id WHERE c.trashed_at IS NULL) SELECT id FROM d)");
    }
    if term.chars().count() >= 3 {
        // The trigram index: the term as one phrase (quotes inside it doubled)
        qb.push(" AND n.rowid IN (SELECT rowid FROM nodes_fts WHERE nodes_fts MATCH ").push_bind(format!("\"{}\"", term.replace('"', "\"\""))).push(")");
    } else {
        let escaped = crate::util::like_escape(&term.to_lowercase());
        qb.push(" AND unicode_lower(n.name) LIKE ").push_bind(format!("%{escaped}%")).push(" ESCAPE '\\'");
    }
    match q.kind.as_deref() {
        Some("folder") => {
            qb.push(" AND n.kind = 'folder'");
        }
        Some("file") => {
            qb.push(" AND n.kind = 'file'");
        }
        _ => {}
    }
    let exts: Vec<String> =
        q.ext.as_deref().unwrap_or_default().split(',').map(|e| e.trim().trim_start_matches('.').to_lowercase()).filter(|e| !e.is_empty()).collect();
    if !exts.is_empty() {
        qb.push(" AND n.kind = 'file' AND (");
        let mut sep = qb.separated(" OR ");
        for e in exts {
            sep.push("unicode_lower(n.name) LIKE ")
                .push_bind_unseparated(format!("%.{}", e.replace(['%', '\\'], "").replace('_', "\\_")))
                .push_unseparated(" ESCAPE '\\'");
        }
        qb.push(")");
    }
    if let Some(f) = q.from {
        qb.push(" AND n.updated_at >= ").push_bind(f);
    }
    if let Some(t) = q.to {
        qb.push(" AND n.updated_at < ").push_bind(t);
    }
    if let Some(m) = q.min_size {
        qb.push(" AND n.kind = 'file' AND n.size >= ").push_bind(m);
    }
    if let Some(m) = q.max_size {
        qb.push(" AND n.kind = 'file' AND n.size <= ").push_bind(m);
    }
    if let Some(o) = q.owner.as_deref().filter(|o| !o.trim().is_empty()) {
        qb.push(" AND n.found = 0 AND n.owner_id = (SELECT id FROM users WHERE username = ").push_bind(o.trim().to_string()).push(")");
    }
    qb.push(" ORDER BY (n.kind = 'folder') DESC, n.updated_at DESC LIMIT ").push_bind(SEARCH_LIMIT + 1);
    let mut nodes: Vec<Node> = qb.build_query_as().fetch_all(&mut *c).await?;
    drop(c);
    let truncated = nodes.len() as i64 > SEARCH_LIMIT;
    nodes.truncate(SEARCH_LIMIT as usize);
    Ok(Json(SearchResult { items: locate(&st, &user, nodes).await?, truncated }))
}

#[derive(Deserialize)]
pub struct FavoriteReq {
    pub(super) ids: Vec<String>,
    pub(super) favorite: bool,
}

pub async fn set_favorite(State(st): State<AppState>, user: User, Json(req): Json<FavoriteReq>) -> AppResult<Json<Value>> {
    if req.ids.is_empty() || req.ids.len() > MAX_BATCH {
        return Err(AppError::bad_request("Select 1 to 1000 items"));
    }
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    for id in &req.ids {
        let node = tree::owned_node(&mut tx, &user, id).await?;
        not_root(&node)?;
        if req.favorite {
            sqlx::query("INSERT OR IGNORE INTO favorites (user_id, node_id, created_at) VALUES (?, ?, ?)")
                .bind(user.id)
                .bind(&node.id)
                .bind(now())
                .execute(&mut *tx)
                .await?;
        } else {
            sqlx::query("DELETE FROM favorites WHERE user_id = ? AND node_id = ?").bind(user.id).bind(&node.id).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn favorites(State(st): State<AppState>, user: User, Query(q): Query<ListQuery>) -> AppResult<Json<Vec<Located>>> {
    let (drives, folders) = tree::scope(&mut *st.db.acquire().await?, &user).await?;
    let sql = format!(
        "SELECT {NODE_COLS} FROM favorites f JOIN nodes n ON n.id = f.node_id
         WHERE f.user_id = ?3 AND {} AND n.trashed_at IS NULL {}",
        tree::scope_sql(1, 2),
        order_clause(q.sort.as_deref(), q.order.as_deref())
    );
    let nodes: Vec<Node> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(drives).bind(folders).bind(user.id).fetch_all(&st.db).await?;
    Ok(Json(locate(&st, &user, nodes).await?))
}

/// Files listed in Recent
pub(super) const RECENT_LIMIT: i64 = 60;
/// Opened files remembered per person
pub(super) const RECENT_OPENS_KEPT: i64 = 300;
/// An open is recorded again only after this many seconds, so a video streamed in many range requests writes once
pub(super) const RECENT_OPEN_INTERVAL: i64 = 60;
/// Candidates taken from each source (own files, newest edits) before the access check, so a person with many files
/// or a long history doesn't make Recent slow
pub(super) const RECENT_CANDIDATES: i64 = 500;

/// Remembers that the user opened a file (for Recent): at most once a minute per file, keeping the latest few hundred
pub async fn record_open(st: &AppState, user_id: i64, node_id: &str) -> AppResult<()> {
    let at = now();
    // Most requests for the same file come close together (range requests, reloads): a read settles them without writing
    let last: Option<(i64,)> =
        sqlx::query_as("SELECT at FROM recent_files WHERE user_id = ? AND node_id = ?").bind(user_id).bind(node_id).fetch_optional(&st.db).await?;
    if last.is_some_and(|(t,)| t > at - RECENT_OPEN_INTERVAL) {
        return Ok(());
    }
    // Like every other write: a transaction that reads before writing would otherwise fail when this commits in between
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    sqlx::query("INSERT INTO recent_files (user_id, node_id, at) VALUES (?1, ?2, ?3) ON CONFLICT (user_id, node_id) DO UPDATE SET at = ?3")
        .bind(user_id)
        .bind(node_id)
        .bind(at)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "DELETE FROM recent_files WHERE user_id = ?1 AND node_id NOT IN (
           SELECT node_id FROM recent_files WHERE user_id = ?1 ORDER BY at DESC LIMIT ?2
         )",
    )
    .bind(user_id)
    .bind(RECENT_OPENS_KEPT)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Recent: files I uploaded, edited or opened, wherever they are (also files of others in shared spaces and folders),
/// newest first by the latest of those times. Only files I can still open and that aren't in the trash.
pub async fn recent(State(st): State<AppState>, user: User) -> AppResult<Json<Vec<Located>>> {
    let (drives, folders) = tree::scope(&mut *st.db.acquire().await?, &user).await?;
    let sql = format!(
        "WITH cand(id, at) AS (
           SELECT * FROM (SELECT id, updated_at FROM nodes
                          WHERE owner_id = ?3 AND kind = 'file' AND found = 0 AND trashed_at IS NULL ORDER BY updated_at DESC LIMIT ?4)
           UNION ALL
           SELECT node_id, at FROM recent_files WHERE user_id = ?3
           UNION ALL
           SELECT * FROM (SELECT node_id, at FROM activity
                          WHERE user_id = ?3 AND action IN ('edit', 'upload') AND node_id IS NOT NULL ORDER BY id DESC LIMIT ?4)
         ),
         latest(id, at) AS (SELECT id, MAX(at) FROM cand GROUP BY id)
         SELECT {NODE_COLS} FROM latest JOIN nodes n ON n.id = latest.id
         WHERE {} AND n.kind = 'file' AND n.trashed_at IS NULL
         ORDER BY latest.at DESC LIMIT ?5",
        tree::scope_sql(1, 2)
    );
    let nodes: Vec<Node> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
        .bind(drives)
        .bind(folders)
        .bind(user.id)
        .bind(RECENT_CANDIDATES)
        .bind(RECENT_LIMIT)
        .fetch_all(&st.db)
        .await?;
    Ok(Json(locate(&st, &user, nodes).await?))
}

#[derive(Serialize)]
pub struct SharedItem {
    #[serde(flatten)]
    pub(super) located: Located,
    pub(super) role: Role,
    pub(super) sharer: String,
}

/// Shared with me: folders and files others shared with me
pub async fn shared_with_me(State(st): State<AppState>, user: User) -> AppResult<Json<Vec<SharedItem>>> {
    let items = tree::shared_with_me(&mut *st.db.acquire().await?, &user).await?;
    let mut roles: HashMap<String, (Role, String)> = HashMap::new();
    let mut nodes = Vec::with_capacity(items.len());
    for (n, role, sharer) in items {
        roles.insert(n.id.clone(), (role, sharer));
        nodes.push(n);
    }
    let located = locate(&st, &user, nodes).await?;
    Ok(Json(
        located
            .into_iter()
            .map(|l| {
                let (role, sharer) = roles.remove(&l.node.id).unwrap_or((Role::Viewer, String::new()));
                SharedItem { located: l, role, sharer }
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;
    use crate::testutil;

    fn favorite(ids: &[&str], on: bool) -> Json<FavoriteReq> {
        Json(FavoriteReq { ids: ids.iter().map(|s| s.to_string()).collect(), favorite: on })
    }

    async fn favorite_names(env: &testutil::TestEnv, user: &User, sort: Option<&str>, order: Option<&str>) -> Vec<String> {
        let q = ListQuery { sort: sort.map(Into::into), order: order.map(Into::into), ..Default::default() };
        let Json(items) = favorites(State(env.st.clone()), user.clone(), Query(q)).await.unwrap();
        assert!(items.iter().all(|l| l.node.is_favorite));
        items.into_iter().map(|l| l.node.name).collect()
    }

    async fn trash(env: &testutil::TestEnv, id: &str) {
        sqlx::query("UPDATE nodes SET trashed_at = ? WHERE id = ?").bind(now()).bind(id).execute(&env.st.db).await.unwrap();
    }

    #[tokio::test]
    async fn favorites_list_what_the_person_marked_and_can_still_open() {
        let env = testutil::env().await;
        let (amy, ben) = (env.user("amy", true).await, env.user("ben", true).await);
        let docs = env.folder(&amy, amy.root(), "Docs").await;
        let (a, b) = (env.stored_file(&amy, &docs, "a.txt", b"a").await, env.stored_file(&amy, amy.root(), "b.txt", b"bb").await);
        let shared = env.stored_file(&ben, ben.root(), "shared.txt", b"s").await;
        env.grant(&shared, &amy, "viewer").await;

        let _ = set_favorite(State(env.st.clone()), amy.clone(), favorite(&[&docs, &a, &b, &shared], true)).await.unwrap();
        // Marking again changes nothing
        let _ = set_favorite(State(env.st.clone()), amy.clone(), favorite(&[&a], true)).await.unwrap();
        // Folders first, then in the order asked for
        assert_eq!(favorite_names(&env, &amy, None, None).await, ["Docs", "a.txt", "b.txt", "shared.txt"]);
        assert_eq!(favorite_names(&env, &amy, Some("name"), Some("desc")).await, ["Docs", "shared.txt", "b.txt", "a.txt"]);
        assert_eq!(favorite_names(&env, &amy, Some("size"), Some("desc")).await, ["Docs", "b.txt", "a.txt", "shared.txt"]);
        // They are the person's own: ben's list is empty
        assert!(favorite_names(&env, &ben, None, None).await.is_empty());
        // Where each is, as the list shows it
        let Json(items) = favorites(State(env.st.clone()), amy.clone(), Query(ListQuery::default())).await.unwrap();
        let a_item = items.iter().find(|l| l.node.id == a).unwrap();
        assert_eq!(a_item.location_path, ["Docs"]);
        assert!(items.iter().find(|l| l.node.id == shared).unwrap().location_space.is_none(), "only shared with amy");

        // Unmarked, in the trash, or no longer shared: not listed
        let _ = set_favorite(State(env.st.clone()), amy.clone(), favorite(&[&b], false)).await.unwrap();
        trash(&env, &a).await;
        env.revoke(&shared, &amy).await;
        assert_eq!(favorite_names(&env, &amy, None, None).await, ["Docs"]);

        // What can't be marked: nothing, too much at once, a space's top folder, or someone else's file
        let status = |r: AppResult<Json<Value>>| r.unwrap_err().status;
        assert_eq!(status(set_favorite(State(env.st.clone()), amy.clone(), favorite(&[], true)).await), StatusCode::BAD_REQUEST);
        let many: Vec<String> = (0..=MAX_BATCH).map(|i| format!("n{i}")).collect();
        let too_many = Json(FavoriteReq { ids: many, favorite: true });
        assert_eq!(status(set_favorite(State(env.st.clone()), amy.clone(), too_many).await), StatusCode::BAD_REQUEST);
        assert!(set_favorite(State(env.st.clone()), amy.clone(), favorite(&[amy.root()], true)).await.is_err());
        let bens = env.stored_file(&ben, ben.root(), "private.txt", b"p").await;
        assert!(set_favorite(State(env.st.clone()), amy.clone(), favorite(&[&bens], true)).await.is_err());
        // A batch with one item that can't be marked marks none
        assert!(set_favorite(State(env.st.clone()), amy.clone(), favorite(&[&b, &bens], true)).await.is_err());
        assert_eq!(favorite_names(&env, &amy, None, None).await, ["Docs"]);
    }

    #[tokio::test]
    async fn shared_with_me_lists_what_others_shared_with_their_role_and_name() {
        let env = testutil::env().await;
        let (amy, ben) = (env.user("amy", true).await, env.user("ben", true).await);
        let plans = env.folder(&ben, ben.root(), "Plans").await;
        let inside = env.stored_file(&ben, &plans, "inside.txt", b"i").await;
        let notes = env.stored_file(&ben, ben.root(), "notes.txt", b"n").await;
        let gone = env.stored_file(&ben, ben.root(), "gone.txt", b"g").await;
        let mut c = env.st.db.acquire().await.unwrap();
        for (node, role) in [(&plans, "editor"), (&notes, "viewer"), (&gone, "viewer")] {
            crate::db::add_grant(&mut c, node, "user", amy.id, role, Some(ben.id), None).await.unwrap();
        }
        // A file in that folder, also shared on its own, is listed on its own too
        crate::db::add_grant(&mut c, &inside, "user", amy.id, "viewer", Some(ben.id), None).await.unwrap();
        // Something in a space amy is a member of isn't "shared with" her
        let company = crate::tree::user_drives(&mut c, &amy).await.unwrap().into_iter().find(|(d, _)| d.kind == crate::tree::SpaceKind::Company).unwrap().0;
        let report = env.stored_file(&env.admin().await, &company.root_id, "report.txt", b"r").await;
        crate::db::add_grant(&mut c, &report, "user", amy.id, "editor", Some(ben.id), None).await.unwrap();
        drop(c);
        trash(&env, &gone).await;

        let Json(items) = shared_with_me(State(env.st.clone()), amy.clone()).await.unwrap();
        let mut got: Vec<(String, Role, String)> = items.into_iter().map(|i| (i.located.node.name, i.role, i.sharer)).collect();
        got.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            got,
            [
                ("Plans".to_string(), Role::Editor, "ben".to_string()),
                ("inside.txt".to_string(), Role::Viewer, "ben".to_string()),
                ("notes.txt".to_string(), Role::Viewer, "ben".to_string()),
            ]
        );
        // Nothing is shared with ben
        assert!(shared_with_me(State(env.st.clone()), ben.clone()).await.unwrap().0.is_empty());
        // Revoked: gone from the list
        env.revoke(&notes, &amy).await;
        let Json(items) = shared_with_me(State(env.st.clone()), amy.clone()).await.unwrap();
        assert!(items.iter().all(|i| i.located.node.id != notes));
    }
}
