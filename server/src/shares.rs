//! Public share links: optional password, expiration time and download limit.

use axum::{
    Json,
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;

use crate::{
    auth::{self, User, cookie_header, hash_password, verify_password},
    error::{AppError, AppResult},
    files::{self, node_blob, serve_blob},
    logs::{self, Visitor, record_share_access},
    nodes::order_clause,
    state::AppState,
    tree::{self, Crumb, NODE_COLS, Node},
    util::{now, random_token},
};

#[derive(Serialize, sqlx::FromRow)]
pub struct ShareInfo {
    id: String,
    node_id: String,
    has_password: bool,
    expires_at: Option<i64>,
    max_downloads: Option<i64>,
    downloads: i64,
    created_at: i64,
    node_name: String,
    node_kind: String,
    /// Number of times the share page was opened
    views: i64,
    /// Time of the most recent access
    last_access: Option<i64>,
}

/// Share link token length: 10 alphanumeric characters (62^10, about 60 bits), infeasible to guess online while keeping the URL short
const SHARE_TOKEN_LEN: usize = 10;

const SHARE_COLS: &str = "s.id, s.node_id, s.password_hash IS NOT NULL AS has_password, s.expires_at, s.max_downloads,
     s.downloads, s.created_at, n.name AS node_name, n.kind AS node_kind,
     s.views, s.last_access";

/// Counts a visit on the share itself: the access log is archived and trimmed, the counters stay
async fn note_access(st: &AppState, share_id: &str, view: bool) {
    let _w = st.write_lock.lock().await;
    let res = sqlx::query("UPDATE shares SET views = views + ?, last_access = ? WHERE id = ?")
        .bind(view as i64)
        .bind(now())
        .bind(share_id)
        .execute(&st.db)
        .await;
    if let Err(e) = res {
        tracing::warn!("Couldn't count a visit of share {share_id}: {e}");
    }
}

#[derive(Deserialize)]
pub struct ListQuery {
    node_id: Option<String>,
}

pub async fn list(State(st): State<AppState>, user: User, Query(q): Query<ListQuery>) -> AppResult<Json<Vec<ShareInfo>>> {
    let sql = format!(
        "SELECT {SHARE_COLS} FROM shares s JOIN nodes n ON n.id = s.node_id
         WHERE s.owner_id = ? AND n.trashed_at IS NULL AND (?2 IS NULL OR s.node_id = ?2)
         ORDER BY s.created_at DESC"
    );
    Ok(Json(sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(user.id).bind(q.node_id).fetch_all(&st.db).await?))
}

#[derive(Deserialize)]
pub struct CreateReq {
    node_id: String,
    password: Option<String>,
    expires_at: Option<i64>,
    max_downloads: Option<i64>,
}

pub async fn create(State(st): State<AppState>, user: User, Json(req): Json<CreateReq>) -> AppResult<Json<ShareInfo>> {
    let password_hash = match req.password.as_deref().map(str::trim) {
        Some(p) if !p.is_empty() => Some(hash_password(p.to_string()).await?),
        _ => None,
    };
    if matches!(req.expires_at, Some(t) if t <= now()) {
        return Err(AppError::bad_request("The expiration time must be in the future"));
    }
    if matches!(req.max_downloads, Some(n) if n <= 0) {
        return Err(AppError::bad_request("The download limit must be greater than 0"));
    }
    let token = random_token(SHARE_TOKEN_LEN);
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    let node = tree::node_for(&mut tx, &user, &req.node_id, tree::Need::Share).await?;
    if node.parent_id.is_none() {
        return Err(AppError::bad_request("The root folder can't be shared"));
    }
    sqlx::query(
        "INSERT INTO shares (id, node_id, owner_id, password_hash, expires_at, max_downloads, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&token)
    .bind(&node.id)
    .bind(user.id)
    .bind(password_hash)
    .bind(req.expires_at)
    .bind(req.max_downloads)
    .bind(now())
    .execute(&mut *tx)
    .await?;
    let sql = format!("SELECT {SHARE_COLS} FROM shares s JOIN nodes n ON n.id = s.node_id WHERE s.id = ?");
    let info = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(&token).fetch_one(&mut *tx).await?;
    let mut options = Vec::new();
    if req.password.as_deref().is_some_and(|p| !p.trim().is_empty()) {
        options.push("password protected".to_string());
    }
    if let Some(t) = req.expires_at {
        options.push(format!("expires {}", &crate::logs::format_time(t, 0)[..10]));
    }
    if let Some(n) = req.max_downloads {
        options.push(format!("limited to {n} {}", if n == 1 { "download" } else { "downloads" }));
    }
    // Capitalize the first option so the detail reads as a sentence
    let mut detail = options.join(", ");
    if let Some(first) = detail.get(..1).map(str::to_uppercase) {
        detail.replace_range(..1, &first);
    }
    tree::log(&mut tx, &user, Some(&node), "share_create", &detail).await?;
    tx.commit().await?;
    Ok(Json(info))
}

pub async fn delete(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    let node_id: Option<(String,)> = sqlx::query_as("SELECT node_id FROM shares WHERE id = ? AND owner_id = ?").bind(&id).bind(user.id).fetch_optional(&mut *tx).await?;
    let Some((node_id,)) = node_id else { return Err(AppError::not_found("Share link not found")) };
    sqlx::query("DELETE FROM shares WHERE id = ?").bind(&id).execute(&mut *tx).await?;
    let node = tree::get_node(&mut tx, &node_id).await?;
    tree::log(&mut tx, &user, node.as_ref(), "share_delete", &format!("/share/{id}")).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

// ───────────── Public access ─────────────

#[derive(sqlx::FromRow)]
struct Share {
    id: String,
    node_id: String,
    owner_id: i64,
    password_hash: Option<String>,
    expires_at: Option<i64>,
    max_downloads: Option<i64>,
    downloads: i64,
    owner_name: String,
}

fn cookie_name(token: &str) -> String {
    format!("tf_share_{token}")
}

/// Cookie handed out with a counted download: later Range requests carrying it are continuations of that download
/// (resuming, video seeking) and aren't counted again; without it a Range request counts as a new download
fn download_cookie_name(token: &str) -> String {
    format!("tf_dl_{token}")
}

/// `<expiry>.<HMAC of the share and the expiry>`: the server checks the expiry itself, not only the browser's Max-Age
fn download_value(st: &AppState, share: &Share, expires: i64) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(&st.secret).expect("hmac key");
    mac.update(b"download:");
    mac.update(share.id.as_bytes());
    mac.update(b":");
    mac.update(expires.to_string().as_bytes());
    format!("{expires}.{}", hex::encode(mac.finalize().into_bytes()))
}

/// Whether the request carries the cookie of a counted download that hasn't expired
fn continues_download(st: &AppState, headers: &HeaderMap, token: &str, share: &Share) -> bool {
    let name = download_cookie_name(token);
    let Some(expires) = auth::get_cookie(headers, &name).and_then(|v| v.split_once('.')).and_then(|(e, _)| e.parse::<i64>().ok()) else {
        return false;
    };
    expires > now() && auth::cookie_matches(headers, &name, &download_value(st, share, expires))
}

fn unlock_value(st: &AppState, share: &Share) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(&st.secret).expect("hmac key");
    mac.update(share.id.as_bytes());
    mac.update(b":");
    mac.update(share.password_hash.as_deref().unwrap_or_default().as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn gone() -> AppError {
    AppError::not_found("This share link doesn't exist or has expired")
}

async fn find_share(st: &AppState, token: &str) -> AppResult<(Share, Node)> {
    let share: Share = sqlx::query_as(
        "SELECT s.id, s.node_id, s.owner_id, s.password_hash, s.expires_at, s.max_downloads, s.downloads, u.username AS owner_name
         FROM shares s JOIN users u ON u.id = s.owner_id WHERE s.id = ? AND u.disabled = 0",
    )
    .bind(token)
    .fetch_optional(&st.db)
    .await?
    .ok_or_else(gone)?;
    if matches!(share.expires_at, Some(t) if t <= now()) {
        return Err(gone());
    }
    let mut conn = st.db.acquire().await?;
    let node = tree::get_node(&mut conn, &share.node_id).await?.ok_or_else(gone)?;
    if node.trashed_at.is_some() {
        return Err(gone());
    }
    // The sharer must *currently* still have access and permission to share this item: when they're removed from the space, the folder share is revoked,
    // their share permission is taken away, or the space is disabled, links they created earlier stop working too (role_on already handles disabled spaces)
    let owner = auth::user_by_id(st, &mut conn, share.owner_id).await?.ok_or_else(gone)?;
    let role = tree::role_on(&mut conn, &owner, &node).await?.ok_or_else(gone)?;
    tree::allows(&owner, role, tree::Need::Share).map_err(|_| gone())?;
    Ok((share, node))
}

/// Finds the share and checks it has been unlocked (when it has a password)
async fn open_share(st: &AppState, token: &str, headers: &HeaderMap) -> AppResult<(Share, Node)> {
    let (share, node) = find_share(st, token).await?;
    if share.password_hash.is_some() && !auth::cookie_matches(headers, &cookie_name(token), &unlock_value(st, &share)) {
        return Err(AppError::new(StatusCode::UNAUTHORIZED, "This share requires a password").with_code("password"));
    }
    Ok((share, node))
}

/// A node within the share's scope
async fn shared_node(st: &AppState, share: &Share, root: &Node, id: &str) -> AppResult<Node> {
    if id == "root" || id == root.id {
        return Ok(root.clone());
    }
    let mut c = st.db.acquire().await?;
    match tree::get_node(&mut c, id).await? {
        Some(n) if n.trashed_at.is_none() && tree::is_within(&mut c, &n.id, &share.node_id).await? => Ok(n),
        _ => Err(AppError::not_found("Item not found")),
    }
}

fn downloads_left(share: &Share) -> Option<i64> {
    share.max_downloads.map(|m| (m - share.downloads).max(0))
}

/// How long a counted download can be resumed (or a video seeked) without counting again, in seconds
const DOWNLOAD_CONTINUES: i64 = 3600;

/// Wrong passwords per share and visitor address in the sign-in failure window
const UNLOCK_ATTEMPTS: usize = 10;

async fn count_download(st: &AppState, share: &Share) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    let res = sqlx::query(
        "UPDATE shares SET downloads = downloads + 1 WHERE id = ? AND (max_downloads IS NULL OR downloads < max_downloads)",
    )
    .bind(&share.id)
    .execute(&st.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::new(StatusCode::GONE, "The download limit has been reached"));
    }
    Ok(())
}

fn ensure_quota_left(share: &Share) -> AppResult<()> {
    if downloads_left(share) == Some(0) {
        return Err(AppError::new(StatusCode::GONE, "The download limit has been reached"));
    }
    Ok(())
}

pub async fn public_info(
    State(st): State<AppState>,
    Path(token): Path<String>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    visitor: Visitor,
) -> AppResult<Json<Value>> {
    let (share, node) = find_share(&st, &token).await?;
    // The page polls this while open, and a bot can reload it endlessly: log one view per visitor per minute.
    // The address is only kept in memory for this; whether it is written to the log depends on the log settings
    if logs::first_view_in_a_while(&st, &share.id, &auth::client_ip(&st, addr, &headers)) {
        record_share_access(&st, &share.id, share.owner_id, Some(&node), "view", &visitor);
        note_access(&st, &share.id, true).await;
    }
    let unlocked =
        share.password_hash.is_none() || auth::cookie_matches(&headers, &cookie_name(&token), &unlock_value(&st, &share));
    let mut info = json!({
        "token": share.id,
        // Only once unlocked: the link alone shouldn't tell who in the organisation has which account
        "owner": unlocked.then_some(share.owner_name.as_str()),
        "expires_at": share.expires_at,
        "downloads_left": downloads_left(&share),
        "needs_password": !unlocked,
    });
    if unlocked {
        info["node"] = public_node_json(&node);
    }
    Ok(Json(info))
}

#[derive(Deserialize)]
pub struct UnlockReq {
    password: String,
}

pub async fn unlock(
    State(st): State<AppState>,
    Path(token): Path<String>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    visitor: Visitor,
    Json(req): Json<UnlockReq>,
) -> AppResult<Response> {
    // Look the share up first so guessed tokens don't leave entries in the rate-limit table;
    // the limit is per share and visitor IP, so one visitor can't lock everyone else out of the share
    let (share, _) = find_share(&st, &token).await?;
    let Some(hash) = share.password_hash.clone() else {
        return Ok(Json(json!({ "ok": true })).into_response());
    };
    let key = format!("share:{token}|{}", auth::limit_key_ip(&auth::client_ip(&st, addr, &headers)));
    // Counted before the hash runs, so parallel guesses can't exceed the limit
    if !auth::begin_attempt(&st, &key, UNLOCK_ATTEMPTS) {
        return Err(AppError::new(StatusCode::TOO_MANY_REQUESTS, "Too many attempts. Try again later."));
    }
    if !verify_password(req.password, hash).await? {
        record_share_access(&st, &share.id, share.owner_id, None, "password_fail", &visitor);
        return Err(AppError::bad_request("Incorrect password"));
    }
    auth::attempt_succeeded(&st, &key);
    record_share_access(&st, &share.id, share.owner_id, None, "unlock", &visitor);
    let cookie = cookie_header(&st, &cookie_name(&token), &unlock_value(&st, &share), &format!("/api/public/shares/{token}"), 86400);
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response())
}

/// What visitors of a share link see of an item: no usernames, spaces or ids outside the shared folder
fn public_node_json(node: &Node) -> serde_json::Value {
    let mut v = serde_json::to_value(node).unwrap();
    if let Some(o) = v.as_object_mut() {
        for key in ["parent_id", "drive_id", "owner_name", "trashed_at", "is_favorite"] {
            o.remove(key);
        }
    }
    v
}

#[derive(Serialize)]
pub struct SharedNodeInfo {
    node: serde_json::Value,
    path: Vec<Crumb>,
}

pub async fn public_node(
    State(st): State<AppState>,
    Path((token, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> AppResult<Json<SharedNodeInfo>> {
    let (share, root) = open_share(&st, &token, &headers).await?;
    let node = shared_node(&st, &share, &root, &id).await?;
    let full = tree::path_of(&mut *st.db.acquire().await?, &node.id).await?;
    let start = full.iter().position(|c| c.id == share.node_id).unwrap_or(full.len());
    Ok(Json(SharedNodeInfo { node: public_node_json(&node), path: full.into_iter().skip(start).collect() }))
}

#[derive(Deserialize)]
pub struct ChildrenQuery {
    sort: Option<String>,
    order: Option<String>,
}

pub async fn public_children(
    State(st): State<AppState>,
    Path((token, id)): Path<(String, String)>,
    Query(q): Query<ChildrenQuery>,
    headers: HeaderMap,
) -> AppResult<Json<Vec<serde_json::Value>>> {
    let (share, root) = open_share(&st, &token, &headers).await?;
    let node = shared_node(&st, &share, &root, &id).await?;
    if !node.is_folder() {
        return Err(AppError::bad_request("This isn't a folder"));
    }
    let sql = format!(
        "SELECT {NODE_COLS} FROM nodes n WHERE n.parent_id = ? AND n.trashed_at IS NULL {}",
        order_clause(q.sort.as_deref(), q.order.as_deref())
    );
    let children: Vec<Node> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(&node.id).fetch_all(&st.db).await?;
    Ok(Json(children.iter().map(public_node_json).collect()))
}

#[derive(Deserialize)]
pub struct ContentQuery {
    download: Option<u8>,
}

pub async fn public_content(
    State(st): State<AppState>,
    Path((token, id)): Path<(String, String)>,
    Query(q): Query<ContentQuery>,
    headers: HeaderMap,
    visitor: Visitor,
) -> AppResult<Response> {
    let (share, root) = open_share(&st, &token, &headers).await?;
    let node = shared_node(&st, &share, &root, &id).await?;
    let download = q.download == Some(1);
    // A request without Range, or starting at byte 0, starts a download and is counted (with a download limit, previews
    // fetch the whole file and count too, otherwise the preview URL would bypass the limit). A Range starting later is a
    // continuation (resuming, video seeking) only when it carries the cookie of a counted download; otherwise it's a new
    // download and counts too, so skipping byte 0 can't be used to fetch the file without being counted
    let limited = share.max_downloads.is_some();
    let continuation =
        files::range_start(&headers, node.size as u64) > 0 && (!limited || continues_download(&st, &headers, &token, &share));
    let counted = !continuation && (download || limited);
    if counted {
        ensure_quota_left(&share)?;
    }
    // Opened first: a download that fails because the storage can't be reached doesn't use up the link
    let mut res = serve_blob(&st, &headers, node_blob(&node)?, download).await?;
    if counted {
        count_download(&st, &share).await?;
    }
    if !continuation {
        record_share_access(&st, &share.id, share.owner_id, Some(&node), if download { "download" } else { "preview" }, &visitor);
        note_access(&st, &share.id, false).await;
    }
    if counted && limited {
        res.headers_mut().append(header::SET_COOKIE, download_cookie(&st, &token, &share)?);
    }
    Ok(res)
}

/// Continuation cookie for a counted download: an hour is enough to resume a large download or seek through a video
fn download_cookie(st: &AppState, token: &str, share: &Share) -> AppResult<HeaderValue> {
    let value = download_value(st, share, now() + DOWNLOAD_CONTINUES);
    let cookie = cookie_header(st, &download_cookie_name(token), &value, &format!("/api/public/shares/{token}"), DOWNLOAD_CONTINUES);
    HeaderValue::from_str(&cookie).map_err(AppError::internal)
}

pub async fn public_thumbnail(
    State(st): State<AppState>,
    Path((token, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let (share, root) = open_share(&st, &token, &headers).await?;
    let node = shared_node(&st, &share, &root, &id).await?;
    files::thumbnail_response(&st, &headers, &node).await
}

#[derive(Deserialize)]
pub struct DownloadQuery {
    ids: String,
    /// The browser's time zone (JavaScript's getTimezoneOffset), for the times inside a ZIP
    tz: Option<i64>,
}

pub async fn public_download(
    State(st): State<AppState>,
    Path(token): Path<String>,
    Query(q): Query<DownloadQuery>,
    headers: HeaderMap,
    visitor: Visitor,
) -> AppResult<Response> {
    let (share, root) = open_share(&st, &token, &headers).await?;
    let mut roots = Vec::new();
    for id in q.ids.split(',').filter(|s| !s.is_empty()).take(1000) {
        roots.push(shared_node(&st, &share, &root, id).await?);
    }
    if roots.is_empty() {
        return Err(AppError::bad_request("Select items to download"));
    }
    // A single file: a Range request with the continuation cookie resumes a counted download, like /content
    let single = matches!(roots.as_slice(), [one] if !one.is_folder());
    let continuation = single
        && files::range_start(&headers, roots[0].size as u64) > 0
        && (share.max_downloads.is_none() || continues_download(&st, &headers, &token, &share));
    if !continuation {
        ensure_quota_left(&share)?;
    }
    // Opened first (a ZIP opens its first file before answering): a download that fails because the storage can't be
    // reached doesn't use up the link
    let mut res = match roots.as_slice() {
        [one] if !one.is_folder() => serve_blob(&st, &headers, node_blob(one)?, true).await?,
        _ => files::zip_response(&st, roots.clone(), q.tz.unwrap_or(0)).await?,
    };
    if !continuation {
        count_download(&st, &share).await?;
        record_share_access(&st, &share.id, share.owner_id, roots.first(), if single { "download" } else { "zip" }, &visitor);
        note_access(&st, &share.id, false).await;
        if single && share.max_downloads.is_some() {
            res.headers_mut().append(header::SET_COOKIE, download_cookie(&st, &token, &share)?);
        }
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    /// A file with real content in the local storage location
    async fn stored_file(env: &testutil::TestEnv, owner: &User, parent: &str, name: &str, content: &[u8]) -> String {
        let id = env.file(owner, parent, name).await;
        let hash = crate::util::sha256_hex(content);
        let tmp = env.dir.join("tmp").join("share-src");
        std::fs::write(&tmp, content).unwrap();
        crate::storage::Storage::put_file(env.st.storage("local").unwrap().as_ref(), &hash, &tmp).await.unwrap();
        let mut c = env.st.db.acquire().await.unwrap();
        tree::add_blob_ref(&mut c, &hash, content.len() as i64, "local").await.unwrap();
        sqlx::query("UPDATE nodes SET blob_hash = ?, size = ? WHERE id = ?").bind(&hash).bind(content.len() as i64).bind(&id).execute(&mut *c).await.unwrap();
        id
    }

    #[tokio::test]
    async fn share_links_show_visitors_only_what_they_need() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let folder = env.folder(&amy, &amy.root_id, "Docs").await;
        let script = stored_file(&env, &amy, &folder, "app.js", b"alert(1)").await;
        let visitor = || Visitor { ip: String::new(), user_agent: String::new() };
        let addr: std::net::SocketAddr = "203.0.113.5:4000".parse().unwrap();

        // Behind a password: nothing about the owner before unlocking
        let req = CreateReq { node_id: folder.clone(), password: Some(testutil::wrong_password()), expires_at: None, max_downloads: None };
        let Json(locked) = create(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
        let Json(info) = public_info(State(env.st.clone()), Path(locked.id), ConnectInfo(addr), HeaderMap::new(), visitor()).await.unwrap();
        assert!(info["owner"].is_null() && info["node"].is_null(), "{info}");

        // Open: items without usernames, spaces or ids outside the share
        let req = CreateReq { node_id: folder.clone(), password: None, expires_at: None, max_downloads: None };
        let Json(open) = create(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
        let Json(info) = public_info(State(env.st.clone()), Path(open.id.clone()), ConnectInfo(addr), HeaderMap::new(), visitor()).await.unwrap();
        assert_eq!(info["owner"], "amy");
        let q = Query(ChildrenQuery { sort: None, order: None });
        let Json(items) = public_children(State(env.st.clone()), Path((open.id.clone(), folder.clone())), q, HeaderMap::new()).await.unwrap();
        for key in ["owner_name", "drive_id", "parent_id"] {
            assert!(items[0].get(key).is_none() && info["node"].get(key).is_none(), "{key} in {items:?}");
        }

        // An uploaded script is served as plain text, and can't be loaded as a script at all
        let content = |dest: Option<&'static str>| {
            let mut h = HeaderMap::new();
            if let Some(d) = dest {
                h.insert("sec-fetch-dest", d.parse().unwrap());
            }
            public_content(State(env.st.clone()), Path((open.id.clone(), script.clone())), Query(ContentQuery { download: None }), h, visitor())
        };
        let res = content(Some("document")).await.unwrap();
        assert!(res.headers()[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/plain"));
        assert_eq!(content(Some("script")).await.unwrap_err().status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_download_that_fails_to_open_doesnt_use_up_the_link() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let doc = stored_file(&env, &amy, &amy.root_id, "report.pdf", b"content").await;
        let req = CreateReq { node_id: doc.clone(), password: None, expires_at: None, max_downloads: Some(1) };
        let Json(info) = create(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
        let visitor = || Visitor { ip: String::new(), user_agent: String::new() };
        let get = || public_content(State(env.st.clone()), Path((info.id.clone(), doc.clone())), Query(ContentQuery { download: Some(1) }), HeaderMap::new(), visitor());
        // The stored content can't be read (a storage service that is down behaves the same)
        let hash = crate::util::sha256_hex(b"content");
        let blob = env.dir.join("blobs").join(&hash[0..2]).join(&hash[2..4]).join(&hash);
        let moved = blob.with_extension("away");
        std::fs::rename(&blob, &moved).unwrap();
        assert!(get().await.is_err());
        // Back again: the one allowed download still works
        std::fs::rename(&moved, &blob).unwrap();
        assert_eq!(get().await.unwrap().status(), StatusCode::OK);
        assert_eq!(get().await.unwrap_err().status, StatusCode::GONE);
    }

    #[tokio::test]
    async fn visits_are_counted_on_the_share_and_survive_trimming_the_log() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let doc = stored_file(&env, &amy, &amy.root_id, "a.txt", b"hello").await;
        let req = CreateReq { node_id: doc.clone(), password: None, expires_at: None, max_downloads: None };
        let Json(info) = create(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
        let visitor = || Visitor { ip: String::new(), user_agent: String::new() };
        for ip in ["203.0.113.1:1", "203.0.113.2:1"] {
            let addr: std::net::SocketAddr = ip.parse().unwrap();
            let _ = public_info(State(env.st.clone()), Path(info.id.clone()), ConnectInfo(addr), HeaderMap::new(), visitor()).await.unwrap();
        }
        // The access log is trimmed after its retention period; the count stays
        sqlx::query("DELETE FROM share_access").execute(&env.st.db).await.unwrap();
        let Json(list) = super::list(State(env.st.clone()), amy.clone(), Query(ListQuery { node_id: None })).await.unwrap();
        assert_eq!(list[0].views, 2);
        assert!(list[0].last_access.is_some());
    }

    #[tokio::test]
    async fn zips_give_items_with_the_same_name_a_number_and_include_each_item_once() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let one = env.folder(&amy, &amy.root_id, "One").await;
        let two = env.folder(&amy, &amy.root_id, "Two").await;
        let a1 = stored_file(&env, &amy, &one, "a.txt", b"first").await;
        let a2 = stored_file(&env, &amy, &two, "a.txt", b"second").await;
        let zip = |ids: Vec<&String>| {
            let st = env.st.clone();
            let ids: Vec<String> = ids.into_iter().cloned().collect();
            async move {
                let mut roots = Vec::new();
                let mut c = st.db.acquire().await.unwrap();
                for id in &ids {
                    roots.push(tree::get_node(&mut c, id).await.unwrap().unwrap());
                }
                drop(c);
                let res = files::zip_response(&st, roots, -480).await.unwrap();
                let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
                String::from_utf8_lossy(&body).into_owned()
            }
        };
        // Two files named a.txt from different folders: the second is numbered (each name appears in the local header
        // and in the central directory)
        let text = zip(vec![&a1, &a2]).await;
        assert_eq!((text.matches("a (1).txt").count(), text.matches("a.txt").count()), (2, 2));
        // A file selected on its own and inside a selected folder, and selected twice: packed once, inside the folder
        let text = zip(vec![&a1, &one, &a1]).await;
        assert_eq!((text.matches("One/a.txt").count(), text.matches("a.txt").count()), (2, 2));
    }

    #[tokio::test]
    async fn download_limit_cannot_be_skipped_with_a_range_request() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let doc = stored_file(&env, &amy, &amy.root_id, "movie.bin", &[7u8; 4096]).await;
        let req = CreateReq { node_id: doc.clone(), password: None, expires_at: None, max_downloads: Some(1) };
        let Json(info) = create(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
        let visitor = || Visitor { ip: String::new(), user_agent: String::new() };
        let content = |range: Option<&'static str>, cookie: Option<String>| {
            let mut h = HeaderMap::new();
            if let Some(r) = range {
                h.insert(header::RANGE, r.parse().unwrap());
            }
            if let Some(c) = cookie {
                h.insert(header::COOKIE, c.parse().unwrap());
            }
            public_content(State(env.st.clone()), Path((info.id.clone(), doc.clone())), Query(ContentQuery { download: Some(1) }), h, visitor())
        };

        // The first download is counted and hands out a continuation cookie
        let first = content(None, None).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let cookie = first.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string();
        assert!(cookie.starts_with("tf_dl_"));
        // Resuming (or seeking in a video) with the cookie isn't counted again…
        assert_eq!(content(Some("bytes=1024-"), Some(cookie.clone())).await.unwrap().status(), StatusCode::PARTIAL_CONTENT);
        // …but a Range request from someone else is a new download, and the limit is reached
        assert_eq!(content(Some("bytes=1-"), None).await.unwrap_err().status, StatusCode::GONE);
        assert_eq!(content(None, None).await.unwrap_err().status, StatusCode::GONE);
        assert_eq!(content(Some("bytes=0-100"), Some(cookie)).await.unwrap_err().status, StatusCode::GONE);
    }

    #[tokio::test]
    async fn download_endpoint_hands_out_the_same_continuation_cookie() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let doc = stored_file(&env, &amy, &amy.root_id, "movie.bin", &[7u8; 4096]).await;
        let req = CreateReq { node_id: doc.clone(), password: None, expires_at: None, max_downloads: Some(1) };
        let Json(info) = create(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
        let visitor = || Visitor { ip: String::new(), user_agent: String::new() };
        let download = |range: Option<&'static str>, cookie: Option<String>| {
            let mut h = HeaderMap::new();
            if let Some(r) = range {
                h.insert(header::RANGE, r.parse().unwrap());
            }
            if let Some(c) = cookie {
                h.insert(header::COOKIE, c.parse().unwrap());
            }
            public_download(State(env.st.clone()), Path(info.id.clone()), Query(DownloadQuery { ids: "root".into(), tz: None }), h, visitor())
        };
        let first = download(None, None).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let set = first.headers()[header::SET_COOKIE].to_str().unwrap().to_string();
        assert!(set.starts_with("tf_dl_") && set.contains("Max-Age=3600"), "{set}");
        let cookie = set.split(';').next().unwrap().to_string();
        // A browser resuming the same download isn't counted again; anyone else has reached the limit
        assert_eq!(download(Some("bytes=2048-"), Some(cookie)).await.unwrap().status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(download(Some("bytes=1-"), None).await.unwrap_err().status, StatusCode::GONE);
        assert_eq!(download(None, None).await.unwrap_err().status, StatusCode::GONE);
    }

    #[tokio::test]
    async fn share_link_dies_when_owner_loses_access() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let folder = env.folder(&amy, &amy.root_id, "Shared").await;
        let doc = env.file(&amy, &folder, "report.txt").await;
        env.grant(&folder, &ben, "editor").await;

        let req = CreateReq { node_id: doc.clone(), password: None, expires_at: None, max_downloads: None };
        let Json(info) = create(State(env.st.clone()), ben.clone(), Json(req)).await.unwrap();
        assert!(find_share(&env.st, &info.id).await.is_ok());

        // Ben's folder share is revoked: public links he created earlier stop working too
        env.revoke(&folder, &ben).await;
        assert_eq!(find_share(&env.st, &info.id).await.err().map(|e| e.status), Some(StatusCode::NOT_FOUND));

        // Sharing again restores them; but with only view permission (can't share) they stop working
        env.grant(&folder, &ben, "editor").await;
        assert!(find_share(&env.st, &info.id).await.is_ok());
        env.grant(&folder, &ben, "viewer").await;
        assert!(find_share(&env.st, &info.id).await.is_err());

        // Losing the share permission, or the account being disabled, also makes them stop working
        env.grant(&folder, &ben, "editor").await;
        sqlx::query("UPDATE users SET can_share = 0 WHERE id = ?").bind(ben.id).execute(&env.st.db).await.unwrap();
        assert!(find_share(&env.st, &info.id).await.is_err());
        sqlx::query("UPDATE users SET can_share = 1, disabled = 1 WHERE id = ?").bind(ben.id).execute(&env.st.db).await.unwrap();
        assert!(find_share(&env.st, &info.id).await.is_err());

        // The file owner's own links are unaffected
        let req = CreateReq { node_id: doc.clone(), password: None, expires_at: None, max_downloads: None };
        let Json(own) = create(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
        assert!(find_share(&env.st, &own.id).await.is_ok());
    }
}
