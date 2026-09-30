//! Public share links: optional password, expiration time and download limit; folder links can accept files from
//! their visitors, and links can be limited to previews.

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
use sqlx::SqliteConnection;

use crate::{
    auth::{self, User, cookie_header, hash_password, verify_password},
    error::{AppError, AppResult},
    downloads::{self, DownloadQuery},
    files::{self, node_blob, serve_blob},
    logs::{self, Visitor, record_share_access},
    nodes::{ListQuery as ChildrenQuery, Listing, list_children},
    state::AppState,
    thumbnails,
    tree::{self, Crumb, Node, Role},
    upload,
    util::{now, random_token},
};

#[derive(Debug, Serialize, sqlx::FromRow)]
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
    /// Who created the link (links others made are listed for the people who may manage them)
    owner_id: i64,
    owner_name: String,
    /// The space the item is in
    drive_id: Option<String>,
    drive_name: String,
    drive_kind: String,
    /// Owner of a personal space, to tell "My files" of different people apart
    drive_owner: String,
    /// Folder links: visitors may upload files into the folder
    allow_upload: bool,
    /// Folder links: visitors can only upload, not see what is in the folder
    drop_only: bool,
    /// false: files are served for previews only (no download button, no ZIP)
    allow_download: bool,
}

/// Share link token length: 10 alphanumeric characters (62^10, about 60 bits), infeasible to guess online while keeping the URL short
const SHARE_TOKEN_LEN: usize = 10;

/// Longest expiry a link policy can ask for, in days (about 10 years)
pub const MAX_EXPIRY_DAYS: i64 = 3650;

/// A link's expiry may be this much past the policy's limit: the browser picks the time, and its clock may be a little ahead
const EXPIRY_SLACK: i64 = 3600;

const SHARE_COLS: &str = "s.id, s.node_id, s.password_hash IS NOT NULL AS has_password, s.expires_at, s.max_downloads,
     s.downloads, s.created_at, n.name AS node_name, n.kind AS node_kind,
     s.views, s.last_access, s.owner_id, u.username AS owner_name, n.drive_id,
     COALESCE(d.name, '') AS drive_name, COALESCE(d.kind, '') AS drive_kind,
     COALESCE((SELECT username FROM users WHERE id = d.owner_id AND d.kind = 'personal'), '') AS drive_owner,
     s.allow_upload, s.drop_only, s.allow_download";

const SHARE_FROM: &str = "shares s JOIN nodes n ON n.id = s.node_id JOIN users u ON u.id = s.owner_id LEFT JOIN drives d ON d.id = n.drive_id";

/// The administrators' rules for public links (Control panel › General)
#[derive(Serialize, Clone, Debug)]
pub struct SharePolicy {
    pub password_required: bool,
    /// 0 = links may be kept without an expiry
    pub max_days: i64,
    /// Off: no new links, and existing ones stop working
    pub public_links: bool,
}

pub fn policy(st: &AppState) -> SharePolicy {
    let s = st.system.read().unwrap();
    SharePolicy { password_required: s.share_password_required, max_days: s.share_max_days, public_links: s.public_links }
}

fn links_off() -> AppError {
    AppError::forbidden("Public share links are turned off")
}

/// Checks an expiry against the policy; `None` means the link never expires
fn check_expiry(policy: &SharePolicy, expires_at: Option<i64>) -> AppResult<()> {
    if matches!(expires_at, Some(t) if t <= now()) {
        return Err(AppError::bad_request("The expiration time must be in the future"));
    }
    if policy.max_days > 0 && expires_at.is_none_or(|t| t > now() + policy.max_days * 86400 + EXPIRY_SLACK) {
        return Err(AppError::bad_request(format!(
            "Share links must expire within {} {}",
            policy.max_days,
            if policy.max_days == 1 { "day" } else { "days" }
        )));
    }
    Ok(())
}

fn check_max_downloads(n: Option<i64>) -> AppResult<()> {
    if matches!(n, Some(n) if n <= 0) {
        return Err(AppError::bad_request("The download limit must be greater than 0"));
    }
    Ok(())
}

/// Whether the user may change or delete a link: the person who created it, a manager of the item's space or folder,
/// the item's owner while they still have access to it, or an administrator
async fn can_manage(conn: &mut SqliteConnection, user: &User, creator: i64, node: &Node) -> AppResult<bool> {
    if creator == user.id || user.is_admin() {
        return Ok(true);
    }
    let role = tree::role_on(conn, user, node).await?;
    Ok(role.is_some_and(|r| r >= Role::Manager) || (role.is_some() && node.owner_id == user.id))
}

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

#[derive(Deserialize, Default)]
pub struct ListQuery {
    /// The links on one item: every one the caller may manage (the share dialog and the details pane)
    node_id: Option<String>,
    /// "mine" (default): links the caller created; "managed": every link the caller may manage (administrators: all)
    scope: Option<String>,
    drive_id: Option<String>,
    /// Only links created by this account
    owner_id: Option<i64>,
    /// true: only links that stopped working because they expired or used up their downloads; false: only the others
    expired: Option<bool>,
}

pub async fn list(State(st): State<AppState>, user: User, Query(q): Query<ListQuery>) -> AppResult<Json<Vec<ShareInfo>>> {
    let mut c = st.db.acquire().await?;
    // Which links besides the caller's own are listed: all of them, or those in spaces the caller manages and on
    // items the caller owns in spaces they can open
    let (all, managed, accessible) = match (&q.node_id, q.scope.as_deref()) {
        (Some(id), _) => {
            let node = tree::get_node(&mut c, id).await?.ok_or_else(|| AppError::not_found("Item not found"))?;
            // Other people's links on the item, when the caller could manage them (-1: nobody is the creator)
            (can_manage(&mut c, &user, -1, &node).await?, Vec::new(), Vec::new())
        }
        (None, Some("managed")) if user.is_admin() => (true, Vec::new(), Vec::new()),
        (None, Some("managed")) => {
            let drives = tree::user_drives(&mut c, &user).await?;
            let managed: Vec<String> = drives.iter().filter(|(_, r)| *r >= Role::Manager).map(|(d, _)| d.id.clone()).collect();
            (false, managed, drives.into_iter().map(|(d, _)| d.id).collect())
        }
        (None, None | Some("mine")) => (false, Vec::new(), Vec::new()),
        _ => return Err(AppError::bad_request("Invalid scope")),
    };
    let sql = format!(
        "SELECT {SHARE_COLS} FROM {SHARE_FROM}
         WHERE n.trashed_at IS NULL
           AND (?1 = 1 OR s.owner_id = ?2 OR n.drive_id IN (SELECT value FROM json_each(?3))
                OR (n.owner_id = ?2 AND n.drive_id IN (SELECT value FROM json_each(?4))))
           AND (?5 IS NULL OR s.node_id = ?5) AND (?6 IS NULL OR n.drive_id = ?6) AND (?7 IS NULL OR s.owner_id = ?7)
           AND (?8 IS NULL OR ?8 = ((s.expires_at IS NOT NULL AND s.expires_at <= ?9)
                                    OR (s.max_downloads IS NOT NULL AND s.downloads >= s.max_downloads)))
         ORDER BY s.created_at DESC"
    );
    let rows = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
        .bind(all)
        .bind(user.id)
        .bind(serde_json::to_string(&managed).unwrap())
        .bind(serde_json::to_string(&accessible).unwrap())
        .bind(&q.node_id)
        .bind(&q.drive_id)
        .bind(q.owner_id)
        .bind(q.expired)
        .bind(now())
        .fetch_all(&mut *c)
        .await?;
    Ok(Json(rows))
}

async fn share_info(conn: &mut SqliteConnection, id: &str) -> AppResult<ShareInfo> {
    let sql = format!("SELECT {SHARE_COLS} FROM {SHARE_FROM} WHERE s.id = ?");
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(id).fetch_one(conn).await?)
}

#[derive(Deserialize)]
pub struct CreateReq {
    node_id: String,
    password: Option<String>,
    expires_at: Option<i64>,
    max_downloads: Option<i64>,
    /// Folder links: visitors may upload files (the files belong to the link's creator)
    #[serde(default)]
    allow_upload: bool,
    /// With uploads: visitors can't see or download what is in the folder
    #[serde(default)]
    drop_only: bool,
    /// false: previews only
    #[serde(default = "yes")]
    allow_download: bool,
}

fn yes() -> bool {
    true
}

/// What visitors may do with a link: uploads need a folder and the permission to change it, and "drop only" needs uploads
async fn check_access(conn: &mut SqliteConnection, user: &User, node: &Node, allow_upload: bool, drop_only: bool) -> AppResult<()> {
    if drop_only && !allow_upload {
        return Err(AppError::bad_request("A link that only accepts files must allow uploads"));
    }
    if allow_upload {
        if !node.is_folder() {
            return Err(AppError::bad_request("Only folder links can accept files"));
        }
        tree::node_for(conn, user, &node.id, tree::Need::Write).await?;
    }
    Ok(())
}

fn access_texts(allow_upload: bool, drop_only: bool, allow_download: bool) -> Vec<String> {
    let mut out = Vec::new();
    if drop_only {
        out.push("only accepts files".to_string());
    } else if allow_upload {
        out.push("accepts files".to_string());
    }
    if !allow_download && !drop_only {
        out.push("preview only".to_string());
    }
    out
}

/// A link's options for the activity log, e.g. "Password protected, expires 2026-05-01"
fn describe(options: Vec<String>) -> String {
    // Capitalize the first option so the detail reads as a sentence
    let mut detail = options.join(", ");
    if let Some(first) = detail.get(..1).map(str::to_uppercase) {
        detail.replace_range(..1, &first);
    }
    detail
}

fn expiry_text(t: Option<i64>) -> String {
    match t {
        Some(t) => format!("expires {}", &crate::logs::format_time(t, 0)[..10]),
        None => "never expires".to_string(),
    }
}

fn limit_text(n: Option<i64>) -> String {
    match n {
        Some(n) => format!("limited to {n} {}", if n == 1 { "download" } else { "downloads" }),
        None => "no download limit".to_string(),
    }
}

pub async fn create(State(st): State<AppState>, user: User, Json(req): Json<CreateReq>) -> AppResult<Json<ShareInfo>> {
    let policy = policy(&st);
    if !policy.public_links {
        return Err(links_off());
    }
    let password = req.password.as_deref().map(str::trim).filter(|p| !p.is_empty());
    if password.is_none() && policy.password_required {
        return Err(AppError::bad_request("Share links must have a password"));
    }
    if let Some(p) = password {
        auth::validate_password(p, auth::min_password(&st))?;
    }
    check_expiry(&policy, req.expires_at)?;
    check_max_downloads(req.max_downloads)?;
    let password_hash = match password {
        Some(p) => Some(hash_password(p.to_string()).await?),
        None => None,
    };
    let token = random_token(SHARE_TOKEN_LEN);
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let node = tree::node_for(&mut tx, &user, &req.node_id, tree::Need::Share).await?;
    if node.parent_id.is_none() {
        return Err(AppError::bad_request("The root folder can't be shared"));
    }
    check_access(&mut tx, &user, &node, req.allow_upload, req.drop_only).await?;
    sqlx::query(
        "INSERT INTO shares (id, node_id, owner_id, password_hash, expires_at, max_downloads, created_at, allow_upload, drop_only, allow_download)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&token)
    .bind(&node.id)
    .bind(user.id)
    .bind(password_hash)
    .bind(req.expires_at)
    .bind(req.max_downloads)
    .bind(now())
    .bind(req.allow_upload)
    .bind(req.drop_only)
    .bind(req.allow_download || req.drop_only)
    .execute(&mut *tx)
    .await?;
    let info = share_info(&mut tx, &token).await?;
    let mut options = Vec::new();
    if password.is_some() {
        options.push("password protected".to_string());
    }
    if req.expires_at.is_some() {
        options.push(expiry_text(req.expires_at));
    }
    if req.max_downloads.is_some() {
        options.push(limit_text(req.max_downloads));
    }
    options.extend(access_texts(req.allow_upload, req.drop_only, req.allow_download));
    logs::record_activity(&mut tx, &user, Some(&node), "share_create", &describe(options)).await?;
    tx.commit().await?;
    Ok(Json(info))
}

/// A field that can be left out (unchanged), null (cleared) or given
fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Deserialize, Default)]
pub struct UpdateReq {
    /// A new password; "" removes the password
    password: Option<String>,
    #[serde(default, deserialize_with = "present")]
    expires_at: Option<Option<i64>>,
    #[serde(default, deserialize_with = "present")]
    max_downloads: Option<Option<i64>>,
    allow_upload: Option<bool>,
    drop_only: Option<bool>,
    allow_download: Option<bool>,
}

/// A share link (its password hash) and its item, for changing or deleting it: only people who may manage it find it
async fn manageable_share(conn: &mut SqliteConnection, user: &User, id: &str) -> AppResult<(Option<String>, Node)> {
    let row: Option<(i64, Option<String>, String)> =
        sqlx::query_as("SELECT owner_id, password_hash, node_id FROM shares WHERE id = ?").bind(id).fetch_optional(&mut *conn).await?;
    let not_found = || AppError::not_found("Share link not found");
    let (creator, hash, node_id) = row.ok_or_else(not_found)?;
    let node = tree::get_node(conn, &node_id).await?.ok_or_else(not_found)?;
    if !can_manage(conn, user, creator, &node).await? {
        return Err(not_found());
    }
    Ok((hash, node))
}

/// Whether the user may manage the link (see `can_manage`); false when it doesn't exist
pub async fn may_manage(st: &AppState, user: &User, id: &str) -> AppResult<bool> {
    match manageable_share(&mut *st.db.acquire().await?, user, id).await {
        Ok(_) => Ok(true),
        Err(e) if e.status == StatusCode::NOT_FOUND => Ok(false),
        Err(e) => Err(e),
    }
}

/// Changes a link's password, expiry or download limit; the link keeps its address and its creator
pub async fn update(State(st): State<AppState>, user: User, Path(id): Path<String>, Json(req): Json<UpdateReq>) -> AppResult<Json<ShareInfo>> {
    let policy = policy(&st);
    let password = req.password.as_deref().map(str::trim);
    if password == Some("") && policy.password_required {
        return Err(AppError::bad_request("Share links must have a password"));
    }
    if let Some(p) = password.filter(|p| !p.is_empty()) {
        auth::validate_password(p, auth::min_password(&st))?;
    }
    if let Some(t) = req.expires_at {
        check_expiry(&policy, t)?;
    }
    if let Some(n) = req.max_downloads {
        check_max_downloads(n)?;
    }
    let new_hash = match password {
        Some(p) if !p.is_empty() => Some(hash_password(p.to_string()).await?),
        _ => None,
    };
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let (old_hash, node) = manageable_share(&mut tx, &user, &id).await?;
    let (was_upload, was_drop, was_download): (bool, bool, bool) =
        sqlx::query_as("SELECT allow_upload, drop_only, allow_download FROM shares WHERE id = ?").bind(&id).fetch_one(&mut *tx).await?;
    let allow_upload = req.allow_upload.unwrap_or(was_upload);
    let drop_only = req.drop_only.unwrap_or(was_drop) && allow_upload;
    let allow_download = req.allow_download.unwrap_or(was_download) || drop_only;
    // Turning uploads on needs the permission to change the folder; turning them off doesn't
    if allow_upload && !was_upload || drop_only && !was_drop {
        check_access(&mut tx, &user, &node, allow_upload, drop_only).await?;
    }
    let mut changes = Vec::new();
    // A new password also locks out visitors who unlocked the link with the old one (the unlock cookie is tied to it)
    let hash = match (password, new_hash) {
        (None, _) => old_hash,
        (Some(_), Some(new)) => {
            changes.push(if old_hash.is_some() { "changed the password" } else { "added a password" }.to_string());
            Some(new)
        }
        (Some(_), None) => {
            if old_hash.is_some() {
                changes.push("removed the password".to_string());
            }
            None
        }
    };
    if let Some(t) = req.expires_at {
        changes.push(expiry_text(t));
    }
    if let Some(n) = req.max_downloads {
        changes.push(limit_text(n));
    }
    if (allow_upload, drop_only, allow_download) != (was_upload, was_drop, was_download) {
        let access = access_texts(allow_upload, drop_only, allow_download);
        changes.push(if access.is_empty() { "view and download".to_string() } else { access.join(", ") });
    }
    sqlx::query(
        "UPDATE shares SET password_hash = ?, expires_at = CASE WHEN ? THEN ? ELSE expires_at END,
           max_downloads = CASE WHEN ? THEN ? ELSE max_downloads END, allow_upload = ?, drop_only = ?, allow_download = ? WHERE id = ?",
    )
    .bind(&hash)
    .bind(req.expires_at.is_some())
    .bind(req.expires_at.flatten())
    .bind(req.max_downloads.is_some())
    .bind(req.max_downloads.flatten())
    .bind(allow_upload)
    .bind(drop_only)
    .bind(allow_download)
    .bind(&id)
    .execute(&mut *tx)
    .await?;
    if !changes.is_empty() {
        logs::record_activity(&mut tx, &user, Some(&node), "share_update", &format!("/share/{id}: {}", changes.join(", "))).await?;
    }
    let info = share_info(&mut tx, &id).await?;
    tx.commit().await?;
    Ok(Json(info))
}

pub async fn delete(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let (_, node) = manageable_share(&mut tx, &user, &id).await?;
    sqlx::query("DELETE FROM shares WHERE id = ?").bind(&id).execute(&mut *tx).await?;
    logs::record_activity(&mut tx, &user, Some(&node), "share_delete", &format!("/share/{id}")).await?;
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
    allow_upload: bool,
    drop_only: bool,
    allow_download: bool,
}

fn cookie_name(token: &str) -> String {
    format!("tf_share_{token}")
}

/// Cookie handed out with a counted download: later Range requests for the same file carrying it are continuations
/// of that download (resuming, video seeking) and aren't counted again; without it a Range request counts as a new
/// download
fn download_cookie_name(token: &str) -> String {
    format!("tf_dl_{token}")
}

/// `<expiry>.<HMAC of the share, the file and the expiry>`: the server checks the expiry itself, not only the
/// browser's Max-Age, and the cookie continues only the file that was counted, not the other files of a folder link
fn download_value(st: &AppState, share: &Share, node_id: &str, expires: i64) -> String {
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
fn continues_download(st: &AppState, headers: &HeaderMap, token: &str, share: &Share, node_id: &str) -> bool {
    let name = download_cookie_name(token);
    let Some(expires) = auth::get_cookie(headers, &name).and_then(|v| v.split_once('.')).and_then(|(e, _)| e.parse::<i64>().ok()) else {
        return false;
    };
    expires > now() && auth::cookie_matches(headers, &name, &download_value(st, share, node_id, expires))
}

/// How long a share link stays unlocked after the password was entered
const UNLOCK_SECS: i64 = 86400;

/// The unlock cookie's value: when it stops working, signed together with the share and its password (changing the
/// password locks everyone out again). The browser keeps the cookie only as long, but a copied value ends then too.
fn unlock_value(st: &AppState, share: &Share, expires: i64) -> String {
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
fn unlocked(st: &AppState, headers: &HeaderMap, token: &str, share: &Share) -> bool {
    if share.password_hash.is_none() {
        return true;
    }
    let name = cookie_name(token);
    let Some(expires) = auth::get_cookie(headers, &name).and_then(|v| v.split_once('.')).and_then(|(e, _)| e.parse::<i64>().ok()) else {
        return false;
    };
    expires > now() && auth::cookie_matches(headers, &name, &unlock_value(st, share, expires))
}

fn gone() -> AppError {
    AppError::not_found("This share link doesn't exist or has expired")
}

async fn find_share(st: &AppState, token: &str) -> AppResult<(Share, Node)> {
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

/// Finds the share and checks it has been unlocked (when it has a password)
async fn open_share(st: &AppState, token: &str, headers: &HeaderMap) -> AppResult<(Share, Node)> {
    let (share, node) = find_share(st, token).await?;
    if !unlocked(st, headers, token, &share) {
        return Err(AppError::new(StatusCode::UNAUTHORIZED, "This share requires a password").with_code("password"));
    }
    Ok((share, node))
}

/// A node within the share's scope; a link that only accepts files shows nothing but the shared folder itself
async fn shared_node(st: &AppState, share: &Share, root: &Node, id: &str) -> AppResult<Node> {
    if id == "root" || id == root.id {
        return Ok(root.clone());
    }
    if share.drop_only {
        return Err(AppError::not_found("Item not found"));
    }
    let mut c = st.db.acquire().await?;
    match tree::get_node(&mut c, id).await? {
        Some(n) if n.trashed_at.is_none() && tree::is_within(&mut c, &n.id, &share.node_id).await? => Ok(n),
        _ => Err(AppError::not_found("Item not found")),
    }
}

/// Visitors of a link that only accepts files can't see what is in the folder
fn ensure_visible(share: &Share) -> AppResult<()> {
    if share.drop_only {
        return Err(AppError::forbidden("This link only accepts files").with_code("drop_only"));
    }
    Ok(())
}

/// Downloads (and ZIPs) of a link limited to previews
fn ensure_download(share: &Share) -> AppResult<()> {
    ensure_visible(share)?;
    if !share.allow_download {
        return Err(AppError::forbidden("Downloads are turned off for this link").with_code("no_download"));
    }
    Ok(())
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
    let children = list_children(&mut *st.db.acquire().await?, &node.id, &q).await?;
    Ok(Json(children.map(|nodes| nodes.iter().map(public_node_json).collect())))
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
    let continuation =
        files::range_start(&headers, node.size as u64) > 0 && (!limited || continues_download(&st, &headers, &token, &share, &node.id));
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
        res.headers_mut().append(header::SET_COOKIE, download_cookie(&st, &token, &share, &node.id)?);
    }
    Ok(res)
}

/// Continuation cookie for a counted download of a file: an hour is enough to resume a large download or seek through
/// a video
fn download_cookie(st: &AppState, token: &str, share: &Share, node_id: &str) -> AppResult<HeaderValue> {
    let value = download_value(st, share, node_id, now() + DOWNLOAD_CONTINUES);
    let cookie = cookie_header(st, &download_cookie_name(token), &value, &format!("/api/public/shares/{token}"), DOWNLOAD_CONTINUES);
    HeaderValue::from_str(&cookie).map_err(AppError::internal)
}

pub async fn public_thumbnail(
    State(st): State<AppState>,
    Path((token, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let (share, root) = open_share(&st, &token, &headers).await?;
    ensure_visible(&share)?;
    let node = shared_node(&st, &share, &root, &id).await?;
    thumbnails::thumbnail_response(&st, &headers, &node).await
}

/// The items to download, each of which must be within the share
async fn shared_nodes(st: &AppState, share: &Share, root: &Node, ids: &[String]) -> AppResult<Vec<Node>> {
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
async fn serve_public_download(
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
        [one] if !one.is_folder() => serve_blob(st, headers, node_blob(one)?, true).await?,
        _ => downloads::zip_response(st, roots.clone(), tz).await?,
    };
    if !continuation {
        count_download(st, &share).await?;
        record_share_access(st, &share.id, share.owner_id, roots.first(), if single { "download" } else { "zip" }, visitor);
        note_access(st, &share.id, false).await;
        if single && share.max_downloads.is_some() {
            res.headers_mut().append(header::SET_COOKIE, download_cookie(st, token, &share, &roots[0].id)?);
        }
    }
    Ok(res)
}

// ───────────── Uploads through a link ─────────────

/// A link that accepts files, opened (password, expiry, the creator's access), with the account the files will belong to
async fn upload_share(st: &AppState, token: &str, headers: &HeaderMap, visitor: Visitor) -> AppResult<upload::Uploader> {
    let (share, root) = open_share(st, token, headers).await?;
    if !share.allow_upload || !root.is_folder() {
        return Err(AppError::forbidden("This link doesn't accept files"));
    }
    // The creator's current permissions decide (open_share checked they can still share it; uploading checks they
    // can still change the folder, and their space's quota)
    let user = auth::user_by_id(st, &mut *st.db.acquire().await?, share.owner_id).await?.ok_or_else(gone)?;
    Ok(upload::Uploader {
        user,
        share: Some(upload::ShareUpload { id: share.id, root: root.id, drop_only: share.drop_only, visitor }),
    })
}

pub async fn public_upload_create(
    State(st): State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
    visitor: Visitor,
) -> AppResult<Response> {
    let up = upload_share(&st, &token, &headers, visitor).await?;
    upload::create_as(&st, &up, &headers).await
}

pub async fn public_upload_head(
    State(st): State<AppState>,
    Path((token, id)): Path<(String, String)>,
    headers: HeaderMap,
    visitor: Visitor,
) -> AppResult<Response> {
    let up = upload_share(&st, &token, &headers, visitor).await?;
    upload::head_as(&st, &up, &id).await
}

pub async fn public_upload_patch(
    State(st): State<AppState>,
    Path((token, id)): Path<(String, String)>,
    headers: HeaderMap,
    visitor: Visitor,
    body: axum::body::Body,
) -> AppResult<Response> {
    let up = upload_share(&st, &token, &headers, visitor).await?;
    upload::patch_as(&st, &up, &id, &headers, body).await
}

pub async fn public_upload_delete(
    State(st): State<AppState>,
    Path((token, id)): Path<(String, String)>,
    headers: HeaderMap,
    visitor: Visitor,
) -> AppResult<Response> {
    let up = upload_share(&st, &token, &headers, visitor).await?;
    upload::delete_as(&st, &up, &id).await
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
        let folder = env.folder(&amy, amy.root(), "Docs").await;
        let script = stored_file(&env, &amy, &folder, "app.js", b"alert(1)").await;
        let visitor = || Visitor { ip: String::new(), user_agent: String::new() };
        let addr: std::net::SocketAddr = "203.0.113.5:4000".parse().unwrap();

        // Behind a password: nothing about the owner before unlocking
        let req = CreateReq { node_id: folder.clone(), password: Some(testutil::wrong_password()), expires_at: None, max_downloads: None, allow_upload: false, drop_only: false, allow_download: true };
        let Json(locked) = create(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
        let Json(info) = public_info(State(env.st.clone()), Path(locked.id), ConnectInfo(addr), HeaderMap::new(), visitor()).await.unwrap();
        assert!(info["owner"].is_null() && info["node"].is_null(), "{info}");

        // Open: items without usernames, spaces or ids outside the share
        let req = CreateReq { node_id: folder.clone(), password: None, expires_at: None, max_downloads: None, allow_upload: false, drop_only: false, allow_download: true };
        let Json(open) = create(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
        let Json(info) = public_info(State(env.st.clone()), Path(open.id.clone()), ConnectInfo(addr), HeaderMap::new(), visitor()).await.unwrap();
        assert_eq!(info["owner"], "amy");
        let q = Query(ChildrenQuery::default());
        let items = public_children(State(env.st.clone()), Path((open.id.clone(), folder.clone())), q, HeaderMap::new()).await.unwrap().0.into_items();
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
        let doc = stored_file(&env, &amy, amy.root(), "report.pdf", b"content").await;
        let req = CreateReq { node_id: doc.clone(), password: None, expires_at: None, max_downloads: Some(1), allow_upload: false, drop_only: false, allow_download: true };
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
    async fn a_zip_that_fails_to_open_doesnt_use_up_the_link() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let folder = env.folder(&amy, amy.root(), "Reports").await;
        let doc = stored_file(&env, &amy, &folder, "report.pdf", b"zipped").await;
        let req = CreateReq { node_id: folder.clone(), password: None, expires_at: None, max_downloads: Some(1), allow_upload: false, drop_only: false, allow_download: true };
        let Json(info) = create(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
        let visitor = || Visitor { ip: String::new(), user_agent: String::new() };
        // The whole shared folder, and the file inside it together with the folder (two items make a ZIP too)
        let get = |ids: String| public_download(State(env.st.clone()), Path(info.id.clone()), Query(DownloadQuery { ids, tz: None }), HeaderMap::new(), visitor());
        let downloads = || async {
            let (n,): (i64,) = sqlx::query_as("SELECT downloads FROM shares WHERE id = ?").bind(&info.id).fetch_one(&env.st.db).await.unwrap();
            n
        };
        let hash = crate::util::sha256_hex(b"zipped");
        let blob = env.dir.join("blobs").join(&hash[0..2]).join(&hash[2..4]).join(&hash);
        let moved = blob.with_extension("away");
        std::fs::rename(&blob, &moved).unwrap();
        assert!(get(folder.clone()).await.is_err());
        assert!(get(format!("{folder},{doc}")).await.is_err());
        assert_eq!(downloads().await, 0);
        std::fs::rename(&moved, &blob).unwrap();
        let res = get(folder.clone()).await.unwrap();
        assert_eq!(res.headers()[header::CONTENT_TYPE], "application/zip");
        assert_eq!(downloads().await, 1);
        assert_eq!(get(folder).await.unwrap_err().status, StatusCode::GONE);
    }

    #[tokio::test]
    async fn visits_are_counted_on_the_share_and_survive_trimming_the_log() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let doc = stored_file(&env, &amy, amy.root(), "a.txt", b"hello").await;
        let req = CreateReq { node_id: doc.clone(), password: None, expires_at: None, max_downloads: None, allow_upload: false, drop_only: false, allow_download: true };
        let Json(info) = create(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
        let visitor = || Visitor { ip: String::new(), user_agent: String::new() };
        for ip in ["203.0.113.1:1", "203.0.113.2:1"] {
            let addr: std::net::SocketAddr = ip.parse().unwrap();
            let _ = public_info(State(env.st.clone()), Path(info.id.clone()), ConnectInfo(addr), HeaderMap::new(), visitor()).await.unwrap();
        }
        // The access log is trimmed after its retention period; the count stays
        sqlx::query("DELETE FROM share_access").execute(&env.st.db).await.unwrap();
        let Json(list) = super::list(State(env.st.clone()), amy.clone(), Query(ListQuery::default())).await.unwrap();
        assert_eq!(list[0].views, 2);
        assert!(list[0].last_access.is_some());
    }

    #[tokio::test]
    async fn zips_give_items_with_the_same_name_a_number_and_include_each_item_once() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let one = env.folder(&amy, amy.root(), "One").await;
        let two = env.folder(&amy, amy.root(), "Two").await;
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
                let res = downloads::zip_response(&st, roots, -480).await.unwrap();
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
        let doc = stored_file(&env, &amy, amy.root(), "movie.bin", &[7u8; 4096]).await;
        let req = CreateReq { node_id: doc.clone(), password: None, expires_at: None, max_downloads: Some(1), allow_upload: false, drop_only: false, allow_download: true };
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
        let doc = stored_file(&env, &amy, amy.root(), "movie.bin", &[7u8; 4096]).await;
        let req = CreateReq { node_id: doc.clone(), password: None, expires_at: None, max_downloads: Some(1), allow_upload: false, drop_only: false, allow_download: true };
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
    async fn the_continuation_cookie_of_a_folder_link_continues_only_the_file_it_came_with() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let folder = env.folder(&amy, amy.root(), "Films").await;
        let first = stored_file(&env, &amy, &folder, "one.bin", &[1u8; 4096]).await;
        let second = stored_file(&env, &amy, &folder, "two.bin", &[2u8; 4096]).await;
        let req = CreateReq { max_downloads: Some(1), ..link(&folder) };
        let Json(info) = create(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
        let visitor = || Visitor { ip: String::new(), user_agent: String::new() };
        let get = |id: &String, range: Option<&'static str>, cookie: Option<&String>| {
            let mut h = HeaderMap::new();
            if let Some(r) = range {
                h.insert(header::RANGE, r.parse().unwrap());
            }
            if let Some(c) = cookie {
                h.insert(header::COOKIE, c.parse().unwrap());
            }
            public_content(State(env.st.clone()), Path((info.id.clone(), id.clone())), Query(ContentQuery { download: Some(1) }), h, visitor())
        };
        let res = get(&first, None, None).await.unwrap();
        let cookie = res.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string();
        assert_eq!(get(&first, Some("bytes=2048-"), Some(&cookie)).await.unwrap().status(), StatusCode::PARTIAL_CONTENT);
        // Another file of the folder is a new download, which the limit no longer allows
        assert_eq!(get(&second, Some("bytes=1-"), Some(&cookie)).await.unwrap_err().status, StatusCode::GONE);
    }

    #[tokio::test]
    async fn selections_download_through_a_link_for_the_same_share() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let folder = env.folder(&amy, amy.root(), "Docs").await;
        let a = stored_file(&env, &amy, &folder, "a.txt", b"first").await;
        let b = stored_file(&env, &amy, &folder, "b.txt", b"second").await;
        let outside = stored_file(&env, &amy, amy.root(), "c.txt", b"third").await;
        let share = |max: Option<i64>| {
            let req = CreateReq { node_id: folder.clone(), password: None, expires_at: None, max_downloads: max, ..link(&folder) };
            create(State(env.st.clone()), amy.clone(), Json(req))
        };
        let Json(info) = share(Some(1)).await.unwrap();
        let Json(other) = share(None).await.unwrap();
        let visitor = || Visitor { ip: String::new(), user_agent: String::new() };
        let make = |token: String, ids: Vec<&String>| {
            let req = downloads::DownloadReq { ids: ids.into_iter().cloned().collect(), tz: None };
            create_public_download_link(State(env.st.clone()), Path(token), HeaderMap::new(), Json(req))
        };
        let get = |token: String, url: &str| {
            let link = url.rsplit('/').next().unwrap().to_string();
            public_download_by_link(State(env.st.clone()), Path((token, link)), HeaderMap::new(), visitor())
        };

        // Items outside the share are refused when the link is made, and nothing is counted yet
        assert_eq!(make(info.id.clone(), vec![&a, &outside]).await.unwrap_err().status, StatusCode::NOT_FOUND);
        let Json(res) = make(info.id.clone(), vec![&a, &b]).await.unwrap();
        let url = res["url"].as_str().unwrap().to_string();
        assert!(url.starts_with(&format!("/api/public/shares/{}/download/", info.id)), "{url}");
        // The link works for its own share only
        assert_eq!(get(other.id.clone(), &url).await.unwrap_err().status, StatusCode::NOT_FOUND);
        // Downloading counts, and the limit is then reached
        let zip = get(info.id.clone(), &url).await.unwrap();
        assert_eq!(zip.headers()[header::CONTENT_TYPE], "application/zip");
        assert_eq!(get(info.id.clone(), &url).await.unwrap_err().status, StatusCode::GONE);
        assert_eq!(make(info.id.clone(), vec![&a]).await.unwrap_err().status, StatusCode::GONE);
    }

    #[tokio::test]
    async fn share_link_dies_when_owner_loses_access() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let folder = env.folder(&amy, amy.root(), "Shared").await;
        let doc = env.file(&amy, &folder, "report.txt").await;
        env.grant(&folder, &ben, "editor").await;

        let req = CreateReq { node_id: doc.clone(), password: None, expires_at: None, max_downloads: None, allow_upload: false, drop_only: false, allow_download: true };
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
        let req = CreateReq { node_id: doc.clone(), password: None, expires_at: None, max_downloads: None, allow_upload: false, drop_only: false, allow_download: true };
        let Json(own) = create(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
        assert!(find_share(&env.st, &own.id).await.is_ok());
    }

    fn link(node: &str) -> CreateReq {
        CreateReq { node_id: node.to_string(), password: None, expires_at: None, max_downloads: None, allow_upload: false, drop_only: false, allow_download: true }
    }

    async fn links(env: &testutil::TestEnv, user: &User, q: ListQuery) -> Vec<String> {
        let Json(list) = super::list(State(env.st.clone()), user.clone(), Query(q)).await.unwrap();
        list.into_iter().map(|s| s.id).collect()
    }

    fn managed() -> ListQuery {
        ListQuery { scope: Some("managed".into()), ..Default::default() }
    }

    async fn change(env: &testutil::TestEnv, user: &User, id: &str, req: serde_json::Value) -> AppResult<ShareInfo> {
        let req: UpdateReq = serde_json::from_value(req).unwrap();
        update(State(env.st.clone()), user.clone(), Path(id.to_string()), Json(req)).await.map(|Json(i)| i)
    }

    async fn remove(env: &testutil::TestEnv, user: &User, id: &str) -> AppResult<Json<Value>> {
        delete(State(env.st.clone()), user.clone(), Path(id.to_string())).await
    }

    async fn set_policy(env: &testutil::TestEnv, req: serde_json::Value) {
        let admin = env.admin().await;
        let req: crate::admin::SettingsReq = serde_json::from_value(req).unwrap();
        let _ = crate::admin::update_settings(State(env.st.clone()), crate::auth::Admin(admin), Json(req)).await.unwrap();
    }

    #[tokio::test]
    async fn managers_administrators_and_owners_see_and_manage_links_others_made() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let ben = env.user("ben", true).await;
        let carl = env.user("carl", true).await;
        let company = env.st.shared_root().unwrap();
        let doc = env.file(&ben, &company, "plan.txt").await;
        let Json(info) = create(State(env.st.clone()), ben.clone(), Json(link(&doc))).await.unwrap();
        assert_eq!((info.owner_name.as_str(), info.drive_kind.as_str()), ("ben", "company"));

        // Another member of the space: doesn't see the link and can't change or delete it
        assert!(links(&env, &carl, managed()).await.is_empty());
        assert!(links(&env, &carl, ListQuery { node_id: Some(doc.clone()), ..Default::default() }).await.is_empty());
        assert_eq!(change(&env, &carl, &info.id, json!({ "max_downloads": 1 })).await.unwrap_err().status, StatusCode::NOT_FOUND);
        assert_eq!(remove(&env, &carl, &info.id).await.unwrap_err().status, StatusCode::NOT_FOUND);
        assert!(!may_manage(&env.st, &carl, &info.id).await.unwrap(), "nor read its access log");

        // A manager of the space sees it among the links they manage and on the item, and can change it
        env.grant(&company, &carl, "manager").await;
        assert!(may_manage(&env.st, &carl, &info.id).await.unwrap());
        assert_eq!(links(&env, &carl, managed()).await, vec![info.id.clone()]);
        assert_eq!(links(&env, &carl, ListQuery { node_id: Some(doc.clone()), ..Default::default() }).await, vec![info.id.clone()]);
        assert!(links(&env, &carl, ListQuery::default()).await.is_empty(), "their own links stay separate");
        let changed = change(&env, &carl, &info.id, json!({ "max_downloads": 3 })).await.unwrap();
        assert_eq!((changed.max_downloads, changed.owner_id), (Some(3), ben.id), "the link keeps its creator");

        // An administrator sees every link, filtered by space, creator and state, and can delete it
        assert_eq!(links(&env, &admin, managed()).await, vec![info.id.clone()]);
        let drive = env.drive_of(&company).await;
        assert_eq!(links(&env, &admin, ListQuery { drive_id: Some(drive), owner_id: Some(ben.id), ..managed() }).await.len(), 1);
        assert!(links(&env, &admin, ListQuery { owner_id: Some(carl.id), ..managed() }).await.is_empty());
        assert!(links(&env, &admin, ListQuery { expired: Some(true), ..managed() }).await.is_empty());
        sqlx::query("UPDATE shares SET downloads = 3").execute(&env.st.db).await.unwrap();
        assert_eq!(links(&env, &admin, ListQuery { expired: Some(true), ..managed() }).await.len(), 1, "used up counts as expired");
        let _ = remove(&env, &admin, &info.id).await.unwrap();
        assert!(find_share(&env.st, &info.id).await.is_err());

        // The owner of a file sees and deletes a link a colleague made on it
        let amy = env.user("amy", true).await;
        let folder = env.folder(&amy, amy.root(), "Shared").await;
        let report = env.file(&amy, &folder, "report.txt").await;
        env.grant(&folder, &ben, "editor").await;
        let Json(theirs) = create(State(env.st.clone()), ben.clone(), Json(link(&report))).await.unwrap();
        assert_eq!(links(&env, &amy, ListQuery { node_id: Some(report.clone()), ..Default::default() }).await, vec![theirs.id.clone()]);
        assert_eq!(links(&env, &amy, managed()).await, vec![theirs.id.clone()]);
        let _ = remove(&env, &amy, &theirs.id).await.unwrap();
    }

    #[tokio::test]
    async fn changing_a_link_keeps_its_address_and_a_new_password_locks_out_earlier_visitors() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let doc = stored_file(&env, &amy, amy.root(), "a.txt", b"hello").await;
        let first = testutil::wrong_password();
        let req = CreateReq { password: Some(first.clone()), expires_at: Some(now() + 86400), ..link(&doc) };
        let Json(info) = create(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
        let addr: std::net::SocketAddr = "203.0.113.9:1".parse().unwrap();
        let visitor = Visitor { ip: String::new(), user_agent: String::new() };
        let res = unlock(State(env.st.clone()), Path(info.id.clone()), ConnectInfo(addr), HeaderMap::new(), visitor, Json(UnlockReq { password: first })).await.unwrap();
        let cookie = res.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string();
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, cookie.parse().unwrap());
        assert!(open_share(&env.st, &info.id, &headers).await.is_ok());

        // Leaving a field out keeps it; null clears it
        let changed = change(&env, &amy, &info.id, json!({ "expires_at": null, "max_downloads": 5 })).await.unwrap();
        assert_eq!((changed.id.as_str(), changed.expires_at, changed.max_downloads, changed.has_password), (info.id.as_str(), None, Some(5), true));
        assert!(open_share(&env.st, &info.id, &headers).await.is_ok());
        assert!(change(&env, &amy, &info.id, json!({ "expires_at": now() - 10 })).await.is_err());
        assert!(change(&env, &amy, &info.id, json!({ "max_downloads": 0 })).await.is_err());

        // A new password: visitors who unlocked the link before must enter it
        change(&env, &amy, &info.id, json!({ "password": testutil::wrong_password() })).await.unwrap();
        assert_eq!(open_share(&env.st, &info.id, &headers).await.err().map(|e| e.status), Some(StatusCode::UNAUTHORIZED));
        let open = change(&env, &amy, &info.id, json!({ "password": "" })).await.unwrap();
        assert!(!open.has_password);
        assert!(open_share(&env.st, &info.id, &HeaderMap::new()).await.is_ok());
        let (logged,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM activity WHERE action = 'share_update'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(logged, 3);
    }

    #[tokio::test]
    async fn link_passwords_are_long_enough_hard_to_guess_and_unlock_for_a_day() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let doc = stored_file(&env, &amy, amy.root(), "a.txt", b"hello").await;
        // As long as account passwords must be
        let short = CreateReq { password: Some("abc".into()), ..link(&doc) };
        assert_eq!(create(State(env.st.clone()), amy.clone(), Json(short)).await.unwrap_err().status, StatusCode::BAD_REQUEST);
        let password = testutil::wrong_password();
        let Json(info) = create(State(env.st.clone()), amy.clone(), Json(CreateReq { password: Some(password.clone()), ..link(&doc) })).await.unwrap();
        assert!(change(&env, &amy, &info.id, json!({ "password": "abc" })).await.is_err());

        let try_from = |ip: &str, password: String| {
            let addr: std::net::SocketAddr = format!("{ip}:1").parse().unwrap();
            let visitor = Visitor { ip: String::new(), user_agent: String::new() };
            unlock(State(env.st.clone()), Path(info.id.clone()), ConnectInfo(addr), HeaderMap::new(), visitor, Json(UnlockReq { password }))
        };
        // Guesses from many addresses (each within its own limit): after ten, the link makes everyone wait
        let mut waited = false;
        for i in 0..16 {
            if let Err(e) = try_from(&format!("198.51.100.{i}"), format!("guess number {i}")).await
                && e.status == StatusCode::TOO_MANY_REQUESTS
            {
                waited = true;
                break;
            }
        }
        assert!(waited, "guessing from many addresses is slowed down");
        env.st.login_failures.lock().unwrap().clear();

        // The unlock lasts a day, even when the cookie is kept longer
        let res = try_from("203.0.113.9", password).await.unwrap();
        let cookie = res.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string();
        let with = |cookie: &str| {
            let mut h = HeaderMap::new();
            h.insert(header::COOKIE, cookie.parse().unwrap());
            h
        };
        assert!(open_share(&env.st, &info.id, &with(&cookie)).await.is_ok());
        let (share, _) = find_share(&env.st, &info.id).await.unwrap();
        let old = format!("{}={}", cookie_name(&info.id), unlock_value(&env.st, &share, now() - 1));
        assert!(open_share(&env.st, &info.id, &with(&old)).await.is_err());
        let forged = cookie.replace(cookie.split('=').nth(1).unwrap().split('.').next().unwrap(), &(now() + 999_999).to_string());
        assert!(open_share(&env.st, &info.id, &with(&forged)).await.is_err(), "the time is signed");
    }

    #[tokio::test]
    async fn the_link_policy_applies_to_new_and_changed_links() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let doc = env.file(&amy, amy.root(), "a.txt").await;
        let Json(old) = create(State(env.st.clone()), amy.clone(), Json(link(&doc))).await.unwrap();
        set_policy(&env, json!({ "share_password_required": true, "share_max_days": 7 })).await;
        let make = |req: CreateReq| create(State(env.st.clone()), amy.clone(), Json(req));

        let err = make(CreateReq { expires_at: Some(now() + 86400), ..link(&doc) }).await.unwrap_err();
        assert_eq!(err.message, "Share links must have a password");
        let pw = || Some(testutil::wrong_password());
        let err = make(CreateReq { password: pw(), ..link(&doc) }).await.unwrap_err();
        assert_eq!(err.message, "Share links must expire within 7 days");
        assert!(make(CreateReq { password: pw(), expires_at: Some(now() + 30 * 86400), ..link(&doc) }).await.is_err());
        let Json(ok) = make(CreateReq { password: pw(), expires_at: Some(now() + 7 * 86400), ..link(&doc) }).await.unwrap();
        assert!(change(&env, &amy, &ok.id, json!({ "password": "" })).await.is_err());
        assert!(change(&env, &amy, &ok.id, json!({ "expires_at": null })).await.is_err());
        assert!(change(&env, &amy, &ok.id, json!({ "expires_at": now() + 3 * 86400 })).await.is_ok());
        // A link made before the rule keeps working as it is
        assert!(find_share(&env.st, &old.id).await.is_ok());
        assert!(crate::db::load_system_settings(&env.st.db).await.unwrap().share_password_required);

        // Public links turned off: none can be made, and existing ones stop working until they're allowed again,
        // while their creators and administrators still find them to delete them
        set_policy(&env, json!({ "public_links": false })).await;
        assert_eq!(make(CreateReq { password: pw(), expires_at: Some(now() + 86400), ..link(&doc) }).await.unwrap_err().status, StatusCode::FORBIDDEN);
        assert_eq!(find_share(&env.st, &ok.id).await.err().map(|e| e.status), Some(StatusCode::NOT_FOUND));
        assert_eq!(links(&env, &amy, ListQuery::default()).await.len(), 2);
        assert!(!crate::db::load_system_settings(&env.st.db).await.unwrap().public_links);
        set_policy(&env, json!({ "public_links": true })).await;
        assert!(find_share(&env.st, &ok.id).await.is_ok());
    }

    fn visitor() -> Visitor {
        Visitor { ip: String::new(), user_agent: String::new() }
    }

    fn folder_link(node: &str, drop_only: bool) -> CreateReq {
        CreateReq { allow_upload: true, drop_only, ..link(node) }
    }

    /// Starts an upload through a link; returns the upload's id (from its address)
    async fn start_upload(env: &testutil::TestEnv, token: &str, parent: Option<&str>, name: &str, len: usize, cookie: Option<&str>) -> AppResult<String> {
        use base64::Engine;
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        let mut h = HeaderMap::new();
        h.insert("upload-length", len.to_string().parse().unwrap());
        let mut meta = format!("filename {}", b64(name));
        if let Some(p) = parent {
            meta.push_str(&format!(",parentId {}", b64(p)));
        }
        h.insert("upload-metadata", meta.parse().unwrap());
        if let Some(c) = cookie {
            h.insert(header::COOKIE, c.parse().unwrap());
        }
        let res = public_upload_create(State(env.st.clone()), Path(token.to_string()), h, visitor()).await?;
        let location = res.headers()[header::LOCATION].to_str().unwrap().to_string();
        assert!(location.starts_with(&format!("/api/public/shares/{token}/uploads/")), "{location}");
        Ok(location.rsplit('/').next().unwrap().to_string())
    }

    async fn send_upload(env: &testutil::TestEnv, token: &str, id: &str, data: &'static [u8]) -> AppResult<Option<String>> {
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_TYPE, "application/offset+octet-stream".parse().unwrap());
        h.insert("upload-offset", "0".parse().unwrap());
        let res = public_upload_patch(State(env.st.clone()), Path((token.to_string(), id.to_string())), h, visitor(), axum::body::Body::from(data)).await?;
        Ok(res.headers().get("x-node-id").map(|v| v.to_str().unwrap().to_string()))
    }

    async fn upload_through(env: &testutil::TestEnv, token: &str, parent: Option<&str>, name: &str, data: &'static [u8]) -> AppResult<String> {
        let id = start_upload(env, token, parent, name, data.len(), None).await?;
        Ok(send_upload(env, token, &id, data).await?.expect("the upload finished"))
    }

    async fn node(env: &testutil::TestEnv, id: &str) -> Node {
        tree::get_node(&mut env.st.db.acquire().await.unwrap(), id).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn a_folder_link_accepts_files_that_belong_to_its_creator() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let inbox = env.folder(&amy, amy.root(), "Inbox").await;
        let sub = env.folder(&amy, &inbox, "2026").await;
        env.file(&amy, &inbox, "a.txt").await;
        let Json(info) = create(State(env.st.clone()), amy.clone(), Json(folder_link(&inbox, false))).await.unwrap();
        assert!(info.allow_upload && !info.drop_only && info.allow_download);

        // A name that is taken gets a number; the file is Amy's, in the shared folder ("root" is the shared folder)
        let id = upload_through(&env, &info.id, Some("root"), "a.txt", b"hello").await.unwrap();
        let file = node(&env, &id).await;
        assert_eq!((file.name.as_str(), file.parent_id.as_deref(), file.owner_id, file.size), ("a (1).txt", Some(inbox.as_str()), amy.id, 5));
        let id = upload_through(&env, &info.id, Some(&sub), "b.txt", b"hi").await.unwrap();
        assert_eq!(node(&env, &id).await.parent_id.as_deref(), Some(sub.as_str()));
        // Recorded in the activity log and the link's access log
        let (detail,): (String,) = sqlx::query_as("SELECT detail FROM activity WHERE action = 'upload' AND node_id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(detail, format!("Through share link /share/{}", info.id));
        let mut logged = 0;
        for _ in 0..100 {
            (logged,) = sqlx::query_as("SELECT COUNT(*) FROM share_access WHERE share_id = ? AND event = 'upload'").bind(&info.id).fetch_one(&env.st.db).await.unwrap();
            if logged == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(logged, 2);

        // Nothing outside the shared folder, not even Amy's own root folder
        assert_eq!(start_upload(&env, &info.id, Some(amy.root()), "x.txt", 1, None).await.unwrap_err().status, StatusCode::NOT_FOUND);
        // Bad names are refused as in any upload
        assert!(start_upload(&env, &info.id, None, "a/b.txt", 1, None).await.is_err());
        // A signed-in person can't continue a visitor's upload, nor a visitor someone else's
        let pending = start_upload(&env, &info.id, None, "c.txt", 3, None).await.unwrap();
        assert!(crate::upload::head(State(env.st.clone()), amy.clone(), Path(pending.clone())).await.is_err());
        let Json(other) = create(State(env.st.clone()), amy.clone(), Json(folder_link(&sub, false))).await.unwrap();
        assert!(send_upload(&env, &other.id, &pending, b"abc").await.is_err());

        // The space's quota counts
        sqlx::query("UPDATE users SET quota_bytes = 10 WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
        assert_eq!(start_upload(&env, &info.id, None, "big.bin", 100, None).await.unwrap_err().status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn uploads_through_a_link_follow_the_link_and_its_creator() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let shared = env.folder(&amy, amy.root(), "Shared").await;
        let sub = env.folder(&amy, &shared, "Sub").await;
        env.grant(&shared, &ben, "editor").await;

        // A link without uploads, a file link, "drop only" without uploads: refused
        let Json(plain) = create(State(env.st.clone()), ben.clone(), Json(link(&shared))).await.unwrap();
        assert_eq!(start_upload(&env, &plain.id, None, "a.txt", 1, None).await.unwrap_err().status, StatusCode::FORBIDDEN);
        let doc = env.file(&amy, &shared, "doc.txt").await;
        assert!(create(State(env.st.clone()), ben.clone(), Json(folder_link(&doc, false))).await.is_err());
        assert!(create(State(env.st.clone()), ben.clone(), Json(CreateReq { drop_only: true, ..link(&shared) })).await.is_err());

        // Uploads can be turned on later; the files belong to the link's creator
        let changed = change(&env, &ben, &plain.id, json!({ "allow_upload": true })).await.unwrap();
        assert!(changed.allow_upload);
        let id = upload_through(&env, &plain.id, None, "a.txt", b"hello").await.unwrap();
        assert_eq!(node(&env, &id).await.owner_id, ben.id);
        // Ben is told, once for files that keep coming through the same link
        upload_through(&env, &plain.id, None, "a2.txt", b"again").await.unwrap();
        let told: Vec<(String, String)> =
            sqlx::query_as("SELECT kind, data FROM notifications WHERE user_id = ? AND kind = 'link_upload'").bind(ben.id).fetch_all(&env.st.db).await.unwrap();
        assert_eq!(told.len(), 1);
        let data: serde_json::Value = serde_json::from_str(&told[0].1).unwrap();
        assert_eq!((data["count"].as_i64(), data["file"].as_str(), data["name"].as_str()), (Some(2), Some("a2.txt"), Some("Shared")));

        // A folder moved out of the shared folder during an upload: the file doesn't follow it
        let pending = start_upload(&env, &plain.id, Some(&sub), "late.txt", 4, None).await.unwrap();
        sqlx::query("UPDATE nodes SET parent_id = ? WHERE id = ?").bind(amy.root()).bind(&sub).execute(&env.st.db).await.unwrap();
        assert!(send_upload(&env, &plain.id, &pending, b"late").await.is_err());
        let (placed,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE name LIKE 'late%'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(placed, 0);

        // The creator may only view now: no more uploads (nor the link at all, since they can't share it)
        env.grant(&shared, &ben, "viewer").await;
        assert!(start_upload(&env, &plain.id, None, "b.txt", 1, None).await.is_err());
        env.grant(&shared, &ben, "editor").await;
        sqlx::query("UPDATE users SET can_write = 0 WHERE id = ?").bind(ben.id).execute(&env.st.db).await.unwrap();
        assert_eq!(start_upload(&env, &plain.id, None, "b.txt", 1, None).await.unwrap_err().status, StatusCode::FORBIDDEN);
        sqlx::query("UPDATE users SET can_write = 1 WHERE id = ?").bind(ben.id).execute(&env.st.db).await.unwrap();

        // A password protects uploads like everything else; the unlock cookie opens them
        let pw = testutil::wrong_password();
        change(&env, &ben, &plain.id, json!({ "password": pw })).await.unwrap();
        assert_eq!(start_upload(&env, &plain.id, None, "c.txt", 1, None).await.unwrap_err().status, StatusCode::UNAUTHORIZED);
        let addr: std::net::SocketAddr = "203.0.113.7:1".parse().unwrap();
        let res = unlock(State(env.st.clone()), Path(plain.id.clone()), ConnectInfo(addr), HeaderMap::new(), visitor(), Json(UnlockReq { password: pw })).await.unwrap();
        let cookie = res.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string();
        assert!(start_upload(&env, &plain.id, None, "c.txt", 1, Some(&cookie)).await.is_ok());
        // And an expired link takes none
        sqlx::query("UPDATE shares SET expires_at = ? WHERE id = ?").bind(now() - 1).bind(&plain.id).execute(&env.st.db).await.unwrap();
        assert_eq!(start_upload(&env, &plain.id, None, "d.txt", 1, Some(&cookie)).await.unwrap_err().status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_link_that_only_accepts_files_shows_nothing_of_the_folder() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let inbox = env.folder(&amy, amy.root(), "Inbox").await;
        let sub = env.folder(&amy, &inbox, "Private").await;
        let secret = stored_file(&env, &amy, &inbox, "secret.txt", b"secret").await;
        let Json(info) = create(State(env.st.clone()), amy.clone(), Json(folder_link(&inbox, true))).await.unwrap();
        let addr: std::net::SocketAddr = "203.0.113.8:1".parse().unwrap();
        let Json(public) = public_info(State(env.st.clone()), Path(info.id.clone()), ConnectInfo(addr), HeaderMap::new(), visitor()).await.unwrap();
        assert_eq!((public["drop_only"].as_bool(), public["allow_upload"].as_bool()), (Some(true), Some(true)));
        assert_eq!(public["node"]["name"], "Inbox");

        let q = || Query(ChildrenQuery::default());
        let err = public_children(State(env.st.clone()), Path((info.id.clone(), inbox.clone())), q(), HeaderMap::new()).await.unwrap_err();
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert!(public_node(State(env.st.clone()), Path((info.id.clone(), sub.clone())), HeaderMap::new()).await.is_err());
        for download in [None, Some(1)] {
            let res = public_content(State(env.st.clone()), Path((info.id.clone(), secret.clone())), Query(ContentQuery { download }), HeaderMap::new(), visitor()).await;
            assert_eq!(res.unwrap_err().status, StatusCode::FORBIDDEN);
        }
        assert!(public_thumbnail(State(env.st.clone()), Path((info.id.clone(), secret.clone())), HeaderMap::new()).await.is_err());
        let zip = public_download(State(env.st.clone()), Path(info.id.clone()), Query(DownloadQuery { ids: "root".into(), tz: None }), HeaderMap::new(), visitor()).await;
        assert_eq!(zip.unwrap_err().status, StatusCode::FORBIDDEN);

        // Files go into the shared folder itself, never into a folder below it
        let id = upload_through(&env, &info.id, None, "invoice.pdf", b"%PDF").await.unwrap();
        assert_eq!(node(&env, &id).await.parent_id.as_deref(), Some(inbox.as_str()));
        assert_eq!(start_upload(&env, &info.id, Some(&sub), "x.pdf", 1, None).await.unwrap_err().status, StatusCode::NOT_FOUND);

        // A visitor asking to replace a file of the same name gets a numbered copy: the file there stays as it is
        use base64::Engine;
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        let mut h = HeaderMap::new();
        h.insert("upload-length", "5".parse().unwrap());
        h.insert("upload-metadata", format!("filename {},onConflict {}", b64("secret.txt"), b64("replace")).parse().unwrap());
        let res = public_upload_create(State(env.st.clone()), Path(info.id.clone()), h, visitor()).await.unwrap();
        let up = res.headers()[header::LOCATION].to_str().unwrap().rsplit('/').next().unwrap().to_string();
        let copy = send_upload(&env, &info.id, &up, b"hacks").await.unwrap().expect("the upload finished");
        assert_ne!(copy, secret);
        assert_eq!(node(&env, &copy).await.name, "secret (1).txt");
        assert_eq!(node(&env, &secret).await.size, 6);

        // A visitor can't keep any number of uploads open at once
        for i in 0..upload::MAX_PENDING_PER_SHARE {
            sqlx::query("INSERT INTO uploads (id, owner_id, parent_id, name, size, created_at, expires_at, share_id) VALUES (?, ?, ?, 'x', 1, ?, ?, ?)")
                .bind(format!("pending-{i}"))
                .bind(amy.id)
                .bind(&inbox)
                .bind(now())
                .bind(now() + 3600)
                .bind(&info.id)
                .execute(&env.st.db)
                .await
                .unwrap();
        }
        assert_eq!(start_upload(&env, &info.id, None, "y.pdf", 1, None).await.unwrap_err().status, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn a_link_that_only_accepts_files_tells_nothing_of_the_names_or_folders_there() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let inbox = env.folder(&amy, amy.root(), "Inbox").await;
        let private = env.folder(&amy, &inbox, "Private").await;
        stored_file(&env, &amy, &inbox, "secret.txt", b"secret").await;
        let Json(info) = create(State(env.st.clone()), amy.clone(), Json(folder_link(&inbox, true))).await.unwrap();
        use base64::Engine;
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        let mut h = HeaderMap::new();
        h.insert("upload-length", "5".parse().unwrap());
        h.insert("upload-metadata", format!("filename {},relativePath {}", b64("secret.txt"), b64("Private")).parse().unwrap());
        let res = public_upload_create(State(env.st.clone()), Path(info.id.clone()), h, visitor()).await.unwrap();
        let up = res.headers()[header::LOCATION].to_str().unwrap().rsplit('/').next().unwrap().to_string();
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_TYPE, "application/offset+octet-stream".parse().unwrap());
        h.insert("upload-offset", "0".parse().unwrap());
        let res = public_upload_patch(State(env.st.clone()), Path((info.id.clone(), up.clone())), h, visitor(), axum::body::Body::from(&b"hello"[..])).await.unwrap();
        // The file is kept under another name, but the visitor isn't told which: that would say the name was taken
        assert!(res.headers().get("x-node-name").is_none());
        let id = res.headers()["x-node-id"].to_str().unwrap().to_string();
        // Nor can a folder path put it into a folder the visitor can't see
        assert_eq!(node(&env, &id).await.parent_id.as_deref(), Some(inbox.as_str()));
        let (inside,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE parent_id = ?").bind(&private).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(inside, 0);
        let res = public_upload_head(State(env.st.clone()), Path((info.id.clone(), up)), HeaderMap::new(), visitor()).await.unwrap();
        assert!(res.headers().get("x-node-name").is_none());
    }

    #[tokio::test]
    async fn uploads_through_a_link_are_limited_in_size_and_dropped_after_a_day_without_progress() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let inbox = env.folder(&amy, amy.root(), "Inbox").await;
        let Json(info) = create(State(env.st.clone()), amy.clone(), Json(folder_link(&inbox, true))).await.unwrap();
        // Unfinished uploads through the link already take all it may hold
        let pending = |id: &str, size: u64, idle: i64, share: Option<&str>| {
            sqlx::query("INSERT INTO uploads (id, owner_id, parent_id, name, size, created_at, expires_at, share_id) VALUES (?, ?, ?, 'x', ?, ?, ?, ?)")
                .bind(id.to_string())
                .bind(amy.id)
                .bind(inbox.clone())
                .bind(size as i64)
                .bind(now())
                .bind(now() + upload::UPLOAD_TTL - idle)
                .bind(share.map(str::to_string))
                .execute(&env.st.db)
        };
        pending("big", upload::MAX_PENDING_BYTES_PER_SHARE - 10, 0, Some(&info.id)).await.unwrap();
        assert_eq!(start_upload(&env, &info.id, None, "y.pdf", 100, None).await.unwrap_err().status, StatusCode::TOO_MANY_REQUESTS);
        assert!(start_upload(&env, &info.id, None, "y.pdf", 5, None).await.is_ok());
        // A day without progress ends an upload through a link; the person's own uploads stay for the week
        pending("idle-link", 10, 2 * 86400, Some(&info.id)).await.unwrap();
        pending("idle-own", 10, 2 * 86400, None).await.unwrap();
        upload::purge_expired(&env.st).await.unwrap();
        let left: Vec<(String,)> = sqlx::query_as("SELECT id FROM uploads WHERE id LIKE 'idle-%'").fetch_all(&env.st.db).await.unwrap();
        assert_eq!(left, vec![("idle-own".to_string(),)]);
    }

    #[tokio::test]
    async fn a_preview_only_link_serves_previews_but_no_downloads() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let folder = env.folder(&amy, amy.root(), "Photos").await;
        let photo = stored_file(&env, &amy, &folder, "a.jpg", b"jpeg").await;
        let Json(info) = create(State(env.st.clone()), amy.clone(), Json(CreateReq { allow_download: false, ..link(&folder) })).await.unwrap();
        assert!(!info.allow_download);
        let content = |download| public_content(State(env.st.clone()), Path((info.id.clone(), photo.clone())), Query(ContentQuery { download }), HeaderMap::new(), visitor());
        assert_eq!(content(None).await.unwrap().status(), StatusCode::OK);
        assert_eq!(content(Some(1)).await.unwrap_err().status, StatusCode::FORBIDDEN);
        for ids in ["root", photo.as_str()] {
            let res = public_download(State(env.st.clone()), Path(info.id.clone()), Query(DownloadQuery { ids: ids.into(), tz: None }), HeaderMap::new(), visitor()).await;
            assert_eq!(res.unwrap_err().status, StatusCode::FORBIDDEN);
        }
        // Allowed again by editing the link
        change(&env, &amy, &info.id, json!({ "allow_download": true })).await.unwrap();
        assert_eq!(content(Some(1)).await.unwrap().status(), StatusCode::OK);
    }
}
