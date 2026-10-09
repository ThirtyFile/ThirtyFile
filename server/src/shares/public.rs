//! Opening a share link: its details, what is in it, and downloading from it

use super::*;

#[derive(Clone, sqlx::FromRow)]
pub(super) struct Share {
    pub(super) id: String,
    pub(super) node_id: String,
    pub(super) owner_id: i64,
    pub(super) password_hash: Option<String>,
    pub(super) expires_at: Option<i64>,
    pub(super) max_downloads: Option<i64>,
    pub(super) downloads: i64,
    pub(super) owner_name: String,
    pub(super) allow_upload: bool,
    pub(super) drop_only: bool,
    pub(super) allow_download: bool,
}

pub(super) fn cookie_name(token: &str) -> String {
    format!("tf_share_{token}")
}

/// Cookie handed out with a counted download: later Range requests for the same file carrying it are continuations
/// of that download (resuming, video seeking) and aren't counted again; without it a Range request counts as a new
/// download
pub(super) fn download_cookie_name(token: &str) -> String {
    format!("tf_dl_{token}")
}

/// `<expiry>.<HMAC of the share, the file and the expiry>`: the server checks the expiry itself, not only the
/// browser's Max-Age, and the cookie continues only the file that was counted, not the other files of a folder link
pub(super) fn download_value(st: &AppState, share: &Share, node_id: &str, expires: i64) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(&st.secret).expect("hmac key");
    mac.update(b"download:");
    mac.update(share.id.as_bytes());
    mac.update(b":");
    mac.update(node_id.as_bytes());
    mac.update(b":");
    mac.update(expires.to_string().as_bytes());
    format!("{expires}.{}", hex::encode(mac.finalize().into_bytes()))
}

/// Whether the request carries the cookie of a counted download of this file that hasn't expired
pub(super) fn continues_download(st: &AppState, headers: &HeaderMap, token: &str, share: &Share, node_id: &str) -> bool {
    let name = download_cookie_name(token);
    let Some(expires) = auth::get_cookie(headers, &name).and_then(|v| v.split_once('.')).and_then(|(e, _)| e.parse::<i64>().ok()) else {
        return false;
    };
    expires > now() && auth::cookie_matches(headers, &name, &download_value(st, share, node_id, expires))
}

/// How long a share link stays unlocked after the password was entered
pub(super) const UNLOCK_SECS: i64 = 86400;

/// The unlock cookie's value: when it stops working, signed together with the share and its password (changing the
/// password locks everyone out again). The browser keeps the cookie only as long, but a copied value ends then too.
pub(super) fn unlock_value(st: &AppState, share: &Share, expires: i64) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(&st.secret).expect("hmac key");
    mac.update(b"unlock:");
    mac.update(share.id.as_bytes());
    mac.update(b":");
    mac.update(share.password_hash.as_deref().unwrap_or_default().as_bytes());
    mac.update(b":");
    mac.update(expires.to_string().as_bytes());
    format!("{expires}.{}", hex::encode(mac.finalize().into_bytes()))
}

/// Whether the share has no password, or the request carries its unlock cookie and that hasn't expired
pub(super) fn unlocked(st: &AppState, headers: &HeaderMap, token: &str, share: &Share) -> bool {
    if share.password_hash.is_none() {
        return true;
    }
    let name = cookie_name(token);
    let Some(expires) = auth::get_cookie(headers, &name).and_then(|v| v.split_once('.')).and_then(|(e, _)| e.parse::<i64>().ok()) else {
        return false;
    };
    expires > now() && auth::cookie_matches(headers, &name, &unlock_value(st, share, expires))
}

pub(super) fn gone() -> AppError {
    AppError::not_found("This share link doesn't exist or has expired")
}

pub(super) async fn find_share(st: &AppState, token: &str) -> AppResult<(Share, Node)> {
    // Turned off by an administrator: every link stops working until links are allowed again
    if !policy(st).public_links {
        return Err(gone());
    }
    let share: Share = sqlx::query_as(
        "SELECT s.id, s.node_id, s.owner_id, s.password_hash, s.expires_at, s.max_downloads, s.downloads, u.username AS owner_name,
                s.allow_upload, s.drop_only, s.allow_download
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

/// What a link's page asks for again and again, kept for a moment: the link, its item, that its creator may still
/// share it, and the items found within it. A folder of 200 pictures asks for 200 thumbnails at once, and each took
/// about seven queries to check. Kept for two seconds at most, and forgotten as soon as anything changes (a write is
/// saved: `db::writes`).
pub struct Seen {
    pub(super) at: std::time::Instant,
    pub(super) writes: u64,
    pub(super) share: Share,
    pub(super) root: Node,
    pub(super) within: std::collections::HashMap<String, Node>,
}

/// `Memory::links`, by link token
pub type SeenLinks = crate::sync::Mutex<std::collections::HashMap<String, Seen>>;
pub(super) const SEEN_FOR: std::time::Duration = std::time::Duration::from_secs(2);
/// Links, and items per link, kept at most
pub(super) const SEEN_LINKS: usize = 1000;
pub(super) const SEEN_ITEMS: usize = 5000;

/// The link `token` as it was seen a moment ago, when nothing changed since and it still works
pub(super) fn seen(st: &AppState, token: &str) -> Option<(Share, Node)> {
    let links = st.part::<crate::shares::Memory>().links.lock();
    let s = links.get(token).filter(|s| s.at.elapsed() < SEEN_FOR && s.writes == crate::db::writes())?;
    if !policy(st).public_links || s.share.expires_at.is_some_and(|t| t <= now()) {
        return None;
    }
    Some((s.share.clone(), s.root.clone()))
}

/// Keeps what was found of the link `token`, read after `writes` writes had been saved
pub(super) fn keep_seen(st: &AppState, token: &str, share: &Share, root: &Node, writes: u64) {
    let mut links = st.part::<crate::shares::Memory>().links.lock();
    if links.len() >= SEEN_LINKS {
        links.retain(|_, s| s.at.elapsed() < SEEN_FOR);
        if links.len() >= SEEN_LINKS {
            links.clear();
        }
    }
    links.insert(token.to_string(), Seen { at: std::time::Instant::now(), writes, share: share.clone(), root: root.clone(), within: Default::default() });
}

/// Finds the share and checks it has been unlocked (when it has a password)
pub(super) async fn open_share(st: &AppState, token: &str, headers: &HeaderMap) -> AppResult<(Share, Node)> {
    let (share, node) = match seen(st, token) {
        Some(s) => s,
        None => {
            let writes = crate::db::writes();
            let (share, node) = find_share(st, token).await?;
            keep_seen(st, token, &share, &node, writes);
            (share, node)
        }
    };
    if !unlocked(st, headers, token, &share) {
        return Err(AppError::new(StatusCode::UNAUTHORIZED, "This share requires a password").with_code("password"));
    }
    Ok((share, node))
}

/// A node within the share's scope; a link that only accepts files shows nothing but the shared folder itself
pub(super) async fn shared_node(st: &AppState, share: &Share, root: &Node, id: &str) -> AppResult<Node> {
    if id == "root" || id == root.id {
        return Ok(root.clone());
    }
    if share.drop_only {
        return Err(AppError::not_found("Item not found"));
    }
    let writes = crate::db::writes();
    if let Some(n) =
        st.part::<crate::shares::Memory>().links.lock().get(&share.id).filter(|s| s.writes == writes && s.at.elapsed() < SEEN_FOR).and_then(|s| s.within.get(id))
    {
        return Ok(n.clone());
    }
    let mut c = st.db.acquire().await?;
    match tree::get_node(&mut c, id).await? {
        Some(n) if n.trashed_at.is_none() && tree::is_within(&mut c, &n.id, &share.node_id).await? => {
            if let Some(s) = st.part::<crate::shares::Memory>().links.lock().get_mut(&share.id).filter(|s| s.writes == writes && s.within.len() < SEEN_ITEMS) {
                s.within.insert(n.id.clone(), n.clone());
            }
            Ok(n)
        }
        _ => Err(AppError::not_found("Item not found")),
    }
}

/// Visitors of a link that only accepts files can't see what is in the folder
pub(super) fn ensure_visible(share: &Share) -> AppResult<()> {
    if share.drop_only {
        return Err(AppError::forbidden("This link only accepts files").with_code("drop_only"));
    }
    Ok(())
}

/// Downloads (and ZIPs) of a link limited to previews
pub(super) fn ensure_download(share: &Share) -> AppResult<()> {
    ensure_visible(share)?;
    if !share.allow_download {
        return Err(AppError::forbidden("Downloads are turned off for this link").with_code("no_download"));
    }
    Ok(())
}

pub(super) fn downloads_left(share: &Share) -> Option<i64> {
    share.max_downloads.map(|m| (m - share.downloads).max(0))
}

/// How long a counted download can be resumed (or a video seeked) without counting again, in seconds
pub(super) const DOWNLOAD_CONTINUES: i64 = 3600;

/// Wrong passwords per share and visitor address in the sign-in failure window
pub(super) const UNLOCK_ATTEMPTS: usize = 10;

pub(super) async fn count_download(st: &AppState, share: &Share) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    // Once saved, the link's details kept for a moment (`seen`), with the downloads counted before this one, are read
    // again (`db::writes`). The limit itself holds whatever a visitor read before: only this UPDATE checks it
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = sqlx::query("UPDATE shares SET downloads = downloads + 1 WHERE id = ? AND (max_downloads IS NULL OR downloads < max_downloads)")
        .bind(&share.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    if res.rows_affected() == 0 {
        return Err(AppError::new(StatusCode::GONE, "The download limit has been reached"));
    }
    Ok(())
}

pub(super) fn ensure_quota_left(share: &Share) -> AppResult<()> {
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
        note_access(&st, &share.id, true);
    }
    let unlocked = unlocked(&st, &headers, &token, &share);
    let mut info = json!({
        "token": share.id,
        // Only once unlocked: the link alone shouldn't tell who in the organisation has which account
        "owner": unlocked.then_some(share.owner_name.as_str()),
        "expires_at": share.expires_at,
        "downloads_left": downloads_left(&share),
        "needs_password": !unlocked,
        "allow_upload": share.allow_upload,
        "drop_only": share.drop_only,
        "allow_download": share.allow_download,
        // Upload size limit per file in bytes (0 = none), so the page can refuse larger files before sending them
        "max_upload": st.max_upload,
    });
    if unlocked {
        info["node"] = public_node_json(&node);
    }
    Ok(Json(info))
}

#[derive(Deserialize)]
pub struct UnlockReq {
    pub(super) password: String,
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
    // And for the link from any address: after a few wrong passwords each try waits longer (as for accounts), which
    // slows down guessing from many addresses without locking the link's real visitors out
    let link_key = format!("share:{token}");
    if let Err(wait) = auth::begin_account_attempt(&st, &link_key) {
        auth::attempt_succeeded(&st, &key);
        return Err(AppError::new(StatusCode::TOO_MANY_REQUESTS, format!("Too many wrong passwords for this link. Try again in {wait} seconds.")));
    }
    if !verify_password(req.password, hash).await? {
        record_share_access(&st, &share.id, share.owner_id, None, "password_fail", &visitor);
        return Err(AppError::bad_request("Incorrect password"));
    }
    auth::attempt_succeeded(&st, &key);
    auth::attempt_succeeded(&st, &link_key);
    record_share_access(&st, &share.id, share.owner_id, None, "unlock", &visitor);
    let value = unlock_value(&st, &share, now() + UNLOCK_SECS);
    let cookie = cookie_header(&st, &cookie_name(&token), &value, &format!("/api/public/shares/{token}"), UNLOCK_SECS);
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response())
}

/// What visitors of a share link see of an item: no usernames, spaces or ids outside the shared folder
pub(super) fn public_node_json(node: &Node) -> serde_json::Value {
    let mut v = serde_json::to_value(node).unwrap();
    if let Some(o) = v.as_object_mut() {
        for key in ["parent_id", "drive_id", "owner_name", "trashed_at", "is_favorite", "tags"] {
            o.remove(key);
        }
    }
    v
}

#[derive(Serialize)]
pub struct SharedNodeInfo {
    pub(super) node: serde_json::Value,
    pub(super) path: Vec<Crumb>,
}

pub async fn public_node(State(st): State<AppState>, Path((token, id)): Path<(String, String)>, headers: HeaderMap) -> AppResult<Json<SharedNodeInfo>> {
    let (share, root) = open_share(&st, &token, &headers).await?;
    let node = shared_node(&st, &share, &root, &id).await?;
    let full = tree::path_of(&mut *st.db.acquire().await?, &node.id).await?;
    let start = full.iter().position(|c| c.id == share.node_id).unwrap_or(full.len());
    Ok(Json(SharedNodeInfo { node: public_node_json(&node), path: full.into_iter().skip(start).collect() }))
}

pub async fn public_children(
    State(st): State<AppState>,
    Path((token, id)): Path<(String, String)>,
    Query(q): Query<ChildrenQuery>,
    headers: HeaderMap,
) -> AppResult<Json<Listing<serde_json::Value>>> {
    let (share, root) = open_share(&st, &token, &headers).await?;
    ensure_visible(&share)?;
    let node = shared_node(&st, &share, &root, &id).await?;
    if !node.is_folder() {
        return Err(AppError::bad_request("This isn't a folder"));
    }
    let children = list_children(&mut *st.db.acquire().await?, &node.id, &q.paged()).await?;
    Ok(Json(children.map(|nodes| nodes.iter().map(public_node_json).collect())))
}

#[derive(Deserialize)]
pub struct ContentQuery {
    pub(super) download: Option<u8>,
}

/// `public_content` at an address that ends in the file's name (see files::content_named)
pub async fn public_content_named(
    st: State<AppState>,
    Path((token, id, _name)): Path<(String, String, String)>,
    q: Query<ContentQuery>,
    headers: HeaderMap,
    visitor: Visitor,
) -> AppResult<Response> {
    public_content(st, Path((token, id)), q, headers, visitor).await
}

pub async fn public_content(
    State(st): State<AppState>,
    Path((token, id)): Path<(String, String)>,
    Query(q): Query<ContentQuery>,
    headers: HeaderMap,
    visitor: Visitor,
) -> AppResult<Response> {
    let (share, root) = open_share(&st, &token, &headers).await?;
    ensure_visible(&share)?;
    let download = q.download == Some(1);
    if download {
        ensure_download(&share)?;
    }
    let node = shared_node(&st, &share, &root, &id).await?;
    // A request without Range, or starting at byte 0, starts a download and is counted (with a download limit, previews
    // fetch the whole file and count too, otherwise the preview URL would bypass the limit). A Range starting later is a
    // continuation (resuming, video seeking) only when it carries the cookie of a counted download; otherwise it's a new
    // download and counts too, so skipping byte 0 can't be used to fetch the file without being counted
    let limited = share.max_downloads.is_some();
    let continuation = files::range_start(&headers, node.size as u64) > 0 && (!limited || continues_download(&st, &headers, &token, &share, &node.id));
    let counted = !continuation && (download || limited);
    if counted {
        ensure_quota_left(&share)?;
    }
    // Opened first: a download that fails because the storage can't be reached doesn't use up the link
    let mut res = serve_blob(&st, &headers, node_blob(&st, &node).await?, download).await?;
    if counted && limited {
        // Checked against the limit before it is served
        count_download(&st, &share).await?;
    } else if counted {
        // Only counted: in the background, so the file doesn't wait for another change holding the write lock
        let (st, share) = (st.clone(), share.clone());
        tokio::spawn(async move {
            if let Err(e) = count_download(&st, &share).await {
                tracing::warn!("Couldn't count a download of share {}: {}", share.id, e.message);
            }
        });
    }
    if !continuation {
        record_share_access(&st, &share.id, share.owner_id, Some(&node), if download { "download" } else { "preview" }, &visitor);
        note_access(&st, &share.id, false);
    }
    if counted && limited {
        res.headers_mut().append(header::SET_COOKIE, download_cookie(&st, &token, &share, &node.id)?);
    }
    Ok(res)
}

/// Continuation cookie for a counted download of a file: an hour is enough to resume a large download or seek through
/// a video
pub(super) fn download_cookie(st: &AppState, token: &str, share: &Share, node_id: &str) -> AppResult<HeaderValue> {
    let value = download_value(st, share, node_id, now() + DOWNLOAD_CONTINUES);
    let cookie = cookie_header(st, &download_cookie_name(token), &value, &format!("/api/public/shares/{token}"), DOWNLOAD_CONTINUES);
    HeaderValue::from_str(&cookie).map_err(AppError::internal)
}

pub async fn public_thumbnail(State(st): State<AppState>, Path((token, id)): Path<(String, String)>, headers: HeaderMap) -> AppResult<Response> {
    let (share, root) = open_share(&st, &token, &headers).await?;
    ensure_visible(&share)?;
    let node = shared_node(&st, &share, &root, &id).await?;
    thumbnails::thumbnail_response(&st, &headers, &node).await
}

/// The items to download, each of which must be within the share
pub(super) async fn shared_nodes(st: &AppState, share: &Share, root: &Node, ids: &[String]) -> AppResult<Vec<Node>> {
    let mut roots = Vec::with_capacity(ids.len());
    for id in ids {
        roots.push(shared_node(st, share, root, id).await?);
    }
    Ok(roots)
}

/// Download with the ids in the URL, for a few items (any number goes through `create_public_download_link`)
pub async fn public_download(
    State(st): State<AppState>,
    Path(token): Path<String>,
    Query(q): Query<DownloadQuery>,
    headers: HeaderMap,
    visitor: Visitor,
) -> AppResult<Response> {
    let (share, root) = open_share(&st, &token, &headers).await?;
    let ids = downloads::download_ids(q.ids.split(','))?;
    let roots = shared_nodes(&st, &share, &root, &ids).await?;
    serve_public_download(&st, &token, share, roots, q.tz.unwrap_or(0), &headers, &visitor).await
}

/// Download of any number of items from a share: the ids come in the body, and the answer is a short-lived link for
/// this share that the browser downloads from. Nothing is counted until that download starts
pub async fn create_public_download_link(
    State(st): State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
    Json(req): Json<downloads::DownloadReq>,
) -> AppResult<Json<Value>> {
    let (share, root) = open_share(&st, &token, &headers).await?;
    ensure_download(&share)?;
    let ids = downloads::download_ids(&req.ids)?;
    ensure_quota_left(&share)?;
    shared_nodes(&st, &share, &root, &ids).await?;
    let link = downloads::store_download_link(&st, format!("share:{token}"), ids, req.tz.unwrap_or(0));
    Ok(Json(json!({ "url": format!("/api/public/shares/{token}/download/{link}") })))
}

/// Downloads the selection behind a link from `create_public_download_link`
pub async fn public_download_by_link(
    State(st): State<AppState>,
    Path((token, link)): Path<(String, String)>,
    headers: HeaderMap,
    visitor: Visitor,
) -> AppResult<Response> {
    let (ids, tz) = downloads::download_link(&st, &format!("share:{token}"), &link)?;
    let (share, root) = open_share(&st, &token, &headers).await?;
    let roots = shared_nodes(&st, &share, &root, &ids).await?;
    serve_public_download(&st, &token, share, roots, tz, &headers, &visitor).await
}

/// Serves a download from a share and counts it (a single file resumed with the continuation cookie isn't counted again)
pub(super) async fn serve_public_download(
    st: &AppState,
    token: &str,
    share: Share,
    roots: Vec<Node>,
    tz: i64,
    headers: &HeaderMap,
    visitor: &Visitor,
) -> AppResult<Response> {
    ensure_download(&share)?;
    // A single file: a Range request with the continuation cookie resumes a counted download, like /content
    let single = matches!(roots.as_slice(), [one] if !one.is_folder());
    let continuation = single
        && files::range_start(headers, roots[0].size as u64) > 0
        && (share.max_downloads.is_none() || continues_download(st, headers, token, &share, &roots[0].id));
    if !continuation {
        ensure_quota_left(&share)?;
    }
    // Opened first (a ZIP opens its first file before answering): a download that fails because the storage can't be
    // reached doesn't use up the link
    let mut res = match roots.as_slice() {
        [one] if !one.is_folder() => serve_blob(st, headers, node_blob(st, one).await?, true).await?,
        _ => downloads::zip_response(st, roots.clone(), tz).await?,
    };
    if !continuation {
        count_download(st, &share).await?;
        record_share_access(st, &share.id, share.owner_id, roots.first().map(|n| n as _), if single { "download" } else { "zip" }, visitor);
        note_access(st, &share.id, false);
        if single && share.max_downloads.is_some() {
            res.headers_mut().append(header::SET_COOKIE, download_cookie(st, token, &share, &roots[0].id)?);
        }
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[tokio::test]
    async fn a_folder_link_lists_a_page_at_a_time_even_when_no_page_size_is_asked_for() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let big = env.folder(&amy, amy.root(), "Big").await;
        let drive = env.drive_of(&big).await;
        let count = crate::nodes::MAX_PAGE + 1;
        sqlx::query(
            "WITH RECURSIVE c(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM c WHERE i < ?1)
             INSERT INTO nodes (id, owner_id, parent_id, kind, name, created_at, updated_at, drive_id)
             SELECT 'sub-' || i, ?2, ?3, 'folder', 'Folder ' || i, 0, 0, ?4 FROM c",
        )
        .bind(count)
        .bind(amy.id)
        .bind(&big)
        .bind(&drive)
        .execute(&env.st.db)
        .await
        .unwrap();
        let req =
            CreateReq { node_id: big.clone(), password: None, expires_at: None, max_downloads: None, allow_upload: false, drop_only: false, allow_download: true };
        let Json(link) = create(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
        let list = |q: ChildrenQuery| public_children(State(env.st.clone()), Path((link.id.clone(), big.clone())), Query(q), HeaderMap::new());
        let Json(listing) = list(ChildrenQuery::default()).await.unwrap();
        let Listing::Page { items, next: Some(next), .. } = listing else { panic!("the whole folder came at once") };
        assert_eq!(items.len() as i64, crate::nodes::MAX_PAGE);
        // The rest comes with the next page
        let q: ChildrenQuery = serde_json::from_value(json!({ "after": next, "limit": crate::nodes::MAX_PAGE })).unwrap();
        let Json(rest) = list(q).await.unwrap();
        assert_eq!(rest.into_items().len(), 1);
    }
}
