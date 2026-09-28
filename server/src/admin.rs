//! Administration: user accounts, roles, permissions and quotas.

use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    auth::{Admin, hash_password, min_password, validate_password},
    db::{NewUser, add_grant, create_user, set_setting},
    error::{AppError, AppResult},
    state::AppState,
    tree,
};

#[derive(Serialize, sqlx::FromRow)]
pub struct UserRow {
    id: i64,
    username: String,
    display_name: String,
    role: String,
    can_write: bool,
    can_delete: bool,
    can_share: bool,
    quota_bytes: i64,
    disabled: bool,
    created_at: i64,
    last_login_at: Option<i64>,
    /// Linked third-party sign-ins (comma-separated)
    sso: String,
    /// Two-factor sign-in is set up
    two_factor: bool,
    /// 'password' (created by an administrator) or the provider that created the account automatically
    source: String,
    /// Email of the most recently used linked sign-in (to spot accounts whose username no longer matches)
    sso_email: String,
    used_bytes: i64,
}

const USER_ROW_SQL: &str = "SELECT u.id, u.username, u.display_name, u.role, u.can_write, u.can_delete, u.can_share, u.quota_bytes, u.disabled, u.created_at, u.last_login_at, u.source,
       (SELECT COALESCE(GROUP_CONCAT(provider), '') FROM user_identities WHERE user_id = u.id) AS sso,
       u.totp_secret IS NOT NULL AS two_factor,
       (SELECT COALESCE(email, '') FROM user_identities WHERE user_id = u.id ORDER BY last_login_at DESC LIMIT 1) AS sso_email,
       (SELECT COALESCE(SUM(used_bytes), 0) FROM drives WHERE kind = 'personal' AND owner_id = u.id) AS used_bytes
     FROM users u";

#[derive(Deserialize)]
pub struct ListQuery {
    /// Paging: only accounts with an id above this one
    after: Option<i64>,
    /// Page size; without it every account is returned (the group editor picks members from all of them)
    limit: Option<i64>,
}

pub async fn list(State(st): State<AppState>, _: Admin, Query(q): Query<ListQuery>) -> AppResult<Json<Vec<UserRow>>> {
    let sql = format!("{USER_ROW_SQL} WHERE u.id > ? ORDER BY u.id LIMIT ?");
    let limit = q.limit.map_or(-1, |l| l.clamp(1, 1000));
    Ok(Json(sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(q.after.unwrap_or(0)).bind(limit).fetch_all(&st.db).await?))
}

/// Trimmed, at most 80 characters, no control characters
pub fn validate_display_name(name: &str) -> AppResult<&str> {
    let name = name.trim();
    if name.chars().count() > 80 {
        return Err(AppError::bad_request("The display name can have at most 80 characters"));
    }
    if name.chars().any(char::is_control) {
        return Err(AppError::bad_request("The display name can't contain control characters"));
    }
    Ok(name)
}

fn validate_role(role: &str) -> AppResult<()> {
    if role == "admin" || role == "user" { Ok(()) } else { Err(AppError::bad_request("Invalid role")) }
}

pub fn validate_username(name: &str) -> AppResult<()> {
    let ok = (2..=32).contains(&name.chars().count())
        && name.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '@'));
    if ok { Ok(()) } else { Err(AppError::bad_request("Username must be 2–32 characters and can only contain letters, numbers, and _ - . @")) }
}

#[derive(Deserialize)]
pub struct CreateReq {
    username: String,
    password: String,
    #[serde(default)]
    display_name: String,
    #[serde(default = "default_role")]
    role: String,
    #[serde(default = "yes")]
    can_write: bool,
    #[serde(default = "yes")]
    can_delete: bool,
    #[serde(default = "yes")]
    can_share: bool,
    /// When omitted, the system setting "default space size for new users" is used
    #[serde(default)]
    quota_bytes: Option<i64>,
}

fn default_role() -> String {
    "user".into()
}
fn yes() -> bool {
    true
}

async fn get_row(st: &AppState, id: i64) -> AppResult<UserRow> {
    let sql = format!("{USER_ROW_SQL} WHERE u.id = ?");
    sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(id).fetch_optional(&st.db).await?.ok_or_else(|| AppError::not_found("User not found"))
}

pub async fn create(State(st): State<AppState>, Admin(me): Admin, Json(req): Json<CreateReq>) -> AppResult<Json<UserRow>> {
    let username = req.username.trim();
    validate_username(username)?;
    validate_password(&req.password, min_password(&st))?;
    validate_role(&req.role)?;
    let display_name = validate_display_name(&req.display_name)?.to_string();
    let password_hash = hash_password(req.password.clone()).await?;
    let id = {
        let _w = st.write_lock.lock().await;
        let mut tx = st.db.begin().await?;
        let id = create_user(
            &mut tx,
            NewUser {
                username,
                password_hash: &password_hash,
                role: &req.role,
                can_write: req.can_write,
                can_delete: req.can_delete,
                can_share: req.can_share,
                quota_bytes: req.quota_bytes.unwrap_or_else(|| st.system.read().unwrap().default_user_quota).max(0),
                source: "password",
                provisioned_by: None,
            },
        )
        .await
        .map_err(|e| if e.status == axum::http::StatusCode::CONFLICT { AppError::conflict("Username already exists") } else { e })?;
        if !display_name.is_empty() {
            sqlx::query("UPDATE users SET display_name = ? WHERE id = ?").bind(&display_name).bind(id).execute(&mut *tx).await?;
        }
        tree::log(&mut tx, &me, None, "user_create", &format!("{username} ({})", if req.role == "admin" { "administrator" } else { "standard user" })).await?;
        tx.commit().await?;
        id
    };
    Ok(Json(get_row(&st, id).await?))
}

#[derive(Deserialize)]
pub struct UpdateReq {
    password: Option<String>,
    display_name: Option<String>,
    role: Option<String>,
    can_write: Option<bool>,
    can_delete: Option<bool>,
    can_share: Option<bool>,
    quota_bytes: Option<i64>,
    disabled: Option<bool>,
}

pub async fn update(
    State(st): State<AppState>,
    Admin(me): Admin,
    Path(id): Path<i64>,
    Json(req): Json<UpdateReq>,
) -> AppResult<Json<UserRow>> {
    if id == me.id && (req.role.as_deref().is_some_and(|r| r != "admin") || req.disabled == Some(true)) {
        return Err(AppError::bad_request("You can't disable your own account or remove your own administrator rights"));
    }
    if let Some(r) = &req.role {
        validate_role(r)?;
    }
    let password_hash = match &req.password {
        Some(p) => {
            validate_password(p, min_password(&st))?;
            Some(hash_password(p.clone()).await?)
        }
        None => None,
    };
    let target = get_row(&st, id).await?;
    let display_name = req.display_name.as_deref().map(validate_display_name).transpose()?.map(str::to_string);
    let mut changes = Vec::new();
    if password_hash.is_some() {
        changes.push("reset password".to_string());
    }
    if let Some(n) = display_name.as_deref().filter(|n| *n != target.display_name) {
        changes.push(if n.is_empty() { "cleared display name".to_string() } else { format!("display name {n}") });
    }
    if let Some(r) = req.role.as_deref().filter(|r| *r != target.role) {
        changes.push(if r == "admin" { "made administrator" } else { "changed to standard user" }.to_string());
    }
    for (label, v, old) in [("edit", req.can_write, target.can_write), ("delete", req.can_delete, target.can_delete), ("share", req.can_share, target.can_share)] {
        if let Some(v) = v.filter(|v| *v != old) {
            changes.push(format!("{} {label} permission", if v { "granted" } else { "removed" }));
        }
    }
    if let Some(q) = req.quota_bytes.filter(|q| *q != target.quota_bytes) {
        changes.push(format!("space size {}", if q <= 0 { "unlimited".to_string() } else { crate::util::format_bytes(q) }));
    }
    if let Some(d) = req.disabled.filter(|d| *d != target.disabled) {
        changes.push(if d { "disabled account" } else { "enabled account" }.to_string());
    }
    {
        let _w = st.write_lock.lock().await;
        let mut tx = st.db.begin().await?;
        if !changes.is_empty() {
            tree::log(&mut tx, &me, None, "user_update", &format!("{}: {}", target.username, changes.join(", "))).await?;
        }
        sqlx::query(
            "UPDATE users SET
               password_hash = COALESCE(?, password_hash),
               display_name = COALESCE(?, display_name),
               role = COALESCE(?, role),
               can_write = COALESCE(?, can_write),
               can_delete = COALESCE(?, can_delete),
               can_share = COALESCE(?, can_share),
               quota_bytes = COALESCE(?, quota_bytes),
               disabled = COALESCE(?, disabled)
             WHERE id = ?",
        )
        .bind(&password_hash)
        .bind(&display_name)
        .bind(&req.role)
        .bind(req.can_write)
        .bind(req.can_delete)
        .bind(req.can_share)
        .bind(req.quota_bytes.map(|q| q.max(0)))
        .bind(req.disabled)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        // When the password is changed or the account disabled, sign the user out on all devices
        if password_hash.is_some() || req.disabled == Some(true) {
            sqlx::query("DELETE FROM sessions WHERE user_id = ?").bind(id).execute(&mut *tx).await?;
        }
        tx.commit().await?;
    }
    Ok(Json(get_row(&st, id).await?))
}

#[derive(Deserialize, Default)]
pub struct DeleteQuery {
    /// The space to move the user's files to: they go into a new folder "Files of <username>" at its top
    move_to: Option<String>,
    /// Delete the files instead; one of the two must be chosen when the personal space holds anything
    #[serde(default)]
    delete_files: bool,
}

/// Deletes a user. Their personal space is moved to another space (`move_to`) or permanently deleted
/// (`delete_files`); files they uploaded to other spaces and team spaces they own are transferred to the administrator
/// performing the deletion; their share links are deleted
pub async fn delete(State(st): State<AppState>, Admin(me): Admin, Path(id): Path<i64>, Query(q): Query<DeleteQuery>) -> AppResult<Json<Value>> {
    if id == me.id {
        return Err(AppError::bad_request("You can't delete your own account"));
    }
    if q.move_to.is_some() && q.delete_files {
        return Err(AppError::bad_request("Choose either to move the user's files or to delete them"));
    }
    let row = get_row(&st, id).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    let personal: Option<(String, String)> = sqlx::query_as("SELECT id, root_id FROM drives WHERE kind = 'personal' AND owner_id = ?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let mut detail = row.username.clone();
    if let Some((drive_id, root_id)) = &personal {
        let (has_files,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM nodes WHERE parent_id = ?)").bind(root_id).fetch_one(&mut *tx).await?;
        if has_files {
            match &q.move_to {
                Some(target) => {
                    let place = move_personal_files(&mut tx, &me, &row.username, drive_id, root_id, target).await?;
                    detail = format!("{}: files moved to {place}", row.username);
                }
                None if q.delete_files => detail = format!("{}: files deleted", row.username),
                None => return Err(AppError::bad_request("Choose where to move the user's files, or choose to delete them")),
            }
        }
    }
    let uploads: Vec<(String,)> = sqlx::query_as("SELECT id FROM uploads WHERE owner_id = ? OR drive_id = ?")
        .bind(id)
        .bind(personal.as_ref().map(|p| p.0.as_str()))
        .fetch_all(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM uploads WHERE owner_id = ? OR drive_id = ?")
        .bind(id)
        .bind(personal.as_ref().map(|p| p.0.as_str()))
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM shares WHERE owner_id = ?").bind(id).execute(&mut *tx).await?;
    // The personal space disappears now; what is still in it (everything when the files are deleted, the root folder
    // and the trash after a move) is deleted in the background, a batch at a time
    if let Some((drive_id, _)) = &personal {
        sqlx::query("DELETE FROM drives WHERE id = ?").bind(drive_id).execute(&mut *tx).await?;
    }
    // Transfer team space ownership to the administrator
    let owned: Vec<(String,)> = sqlx::query_as(
        "SELECT node_id FROM grants WHERE principal_type = 'user' AND principal_id = ? AND role = 'owner'",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    for (node_id,) in owned {
        add_grant(&mut tx, &node_id, "user", me.id, "owner", Some(me.id), None).await?;
    }
    sqlx::query("DELETE FROM grants WHERE principal_type = 'user' AND principal_id = ?").bind(id).execute(&mut *tx).await?;
    sqlx::query("UPDATE drives SET owner_id = ? WHERE owner_id = ?").bind(me.id).bind(id).execute(&mut *tx).await?;
    sqlx::query("UPDATE nodes SET owner_id = ? WHERE owner_id = ?").bind(me.id).bind(id).execute(&mut *tx).await?;
    // The trash shows "—" for items deleted by a removed account (a later account could get the same id)
    sqlx::query("UPDATE nodes SET trashed_by = NULL WHERE trashed_by = ?").bind(id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM users WHERE id = ?").bind(id).execute(&mut *tx).await?;
    tree::log(&mut tx, &me, None, "user_delete", &detail).await?;
    tx.commit().await?;
    if personal.is_some() {
        tree::purge_detached_later(&st);
    }
    for (u,) in uploads {
        let _ = tokio::fs::remove_file(st.tmp_dir().join(format!("upload-{u}"))).await;
    }
    Ok(Json(json!({ "ok": true })))
}

/// Moves everything in a personal space into a new folder "Files of <username>" at the top of another space; returns
/// where they went (for the log). Nothing is copied: the top items get the new folder as their parent, and one
/// recursive update moves the whole tree to the other space, so even a large space takes one short transaction. The
/// trash isn't moved: it stays behind in the personal space and is deleted with it
async fn move_personal_files(
    conn: &mut sqlx::SqliteConnection,
    me: &crate::auth::User,
    username: &str,
    drive_id: &str,
    root_id: &str,
    target_id: &str,
) -> AppResult<String> {
    let target = tree::get_drive(conn, target_id).await?.ok_or_else(|| AppError::not_found("Space not found"))?;
    if target.id == drive_id {
        return Err(AppError::bad_request("Choose a space other than the user's own"));
    }
    if target.is_folder() {
        return Err(AppError::bad_request("Files can't be moved into a space that shows a folder on the server"));
    }
    if target.disabled {
        return Err(AppError::bad_request("That space is turned off"));
    }
    // Everything not in the trash, below the root folder
    const LIVE: &str = "WITH RECURSIVE sub(id) AS (
           SELECT id FROM nodes WHERE parent_id = ?1 AND trashed_at IS NULL
           UNION ALL SELECT c.id FROM nodes c JOIN sub ON c.parent_id = sub.id WHERE c.trashed_at IS NULL
         )";
    let sql = format!("{LIVE} SELECT COALESCE(SUM(size), 0) FROM nodes WHERE kind = 'file' AND id IN (SELECT id FROM sub)");
    let (bytes,): (i64,) = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(root_id).fetch_one(&mut *conn).await?;
    if let Err(e) = tree::check_quota(conn, &target.id, bytes).await {
        return Err(if e.code == Some("quota") {
            AppError::new(
                axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                format!("\"{}\" doesn't have room for {} more. Choose another space, or give it more space first.", target.name, crate::util::format_bytes(bytes)),
            )
            .with_code("quota")
        } else {
            e
        });
    }
    let name = tree::unique_name(conn, &target.root_id, &format!("Files of {username}"), true).await?;
    let folder = tree::create_folder(conn, me.id, &target.root_id, &name).await?;
    let sql = format!("{LIVE} UPDATE nodes SET drive_id = ?2 WHERE id IN (SELECT id FROM sub)");
    sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).bind(root_id).bind(&target.id).execute(&mut *conn).await?;
    sqlx::query("UPDATE nodes SET parent_id = ? WHERE parent_id = ? AND trashed_at IS NULL").bind(&folder).bind(root_id).execute(&mut *conn).await?;
    tree::adjust_usage(conn, drive_id, -bytes).await?;
    tree::adjust_usage(conn, &target.id, bytes).await?;
    let space = match target.kind.as_str() {
        "personal" => {
            let (owner,): (String,) = sqlx::query_as("SELECT COALESCE((SELECT username FROM users WHERE id = ?), '')").bind(target.owner_id).fetch_one(&mut *conn).await?;
            format!("My files of {owner}")
        }
        _ => target.name.clone(),
    };
    Ok(format!("{space} › {name}"))
}

// ───────────── System settings ─────────────

#[derive(Serialize)]
pub struct SystemInfo {
    shared_enabled: bool,
    shared_root_id: String,
    allow_user_drives: bool,
    /// Default capacity of new users' personal spaces (bytes, 0 = unlimited)
    default_user_quota: i64,
    public_url: String,
    default_lang: String,
    scan_minutes: i64,
    require_two_factor: bool,
    min_password_length: usize,
    /// Public share links: must have a password
    share_password_required: bool,
    /// Public share links: must expire within this many days (0 = no limit)
    share_max_days: i64,
    /// Public share links can be created and opened
    public_links: bool,
    /// Earlier versions kept per file (0 = none), and for how many days (0 = no limit)
    version_keep: i64,
    version_days: i64,
    stats: SystemStats,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct SystemStats {
    users: i64,
    groups: i64,
    team_drives: i64,
    personal_bytes: i64,
    personal_files: i64,
    shared_bytes: i64,
    shared_files: i64,
    team_bytes: i64,
    team_files: i64,
    trash_bytes: i64,
    /// Earlier versions of files (not counted toward the spaces' quotas)
    version_bytes: i64,
    /// Storage actually used (duplicate files are stored only once)
    stored_bytes: i64,
    share_links: i64,
}

async fn system_info(st: &AppState) -> AppResult<SystemInfo> {
    let stats: SystemStats = sqlx::query_as(
        "WITH f AS (SELECT n.size, d.kind FROM nodes n JOIN drives d ON d.id = n.drive_id WHERE n.kind = 'file' AND n.trashed_at IS NULL)
         SELECT
           (SELECT COUNT(*) FROM users) AS users,
           (SELECT COUNT(*) FROM groups) AS groups,
           (SELECT COUNT(*) FROM drives WHERE kind = 'team') AS team_drives,
           (SELECT COALESCE(SUM(size), 0) FROM f WHERE kind = 'personal') AS personal_bytes,
           (SELECT COUNT(*) FROM f WHERE kind = 'personal') AS personal_files,
           (SELECT COALESCE(SUM(size), 0) FROM f WHERE kind = 'company') AS shared_bytes,
           (SELECT COUNT(*) FROM f WHERE kind = 'company') AS shared_files,
           (SELECT COALESCE(SUM(size), 0) FROM f WHERE kind = 'team') AS team_bytes,
           (SELECT COUNT(*) FROM f WHERE kind = 'team') AS team_files,
           (SELECT COALESCE(SUM(size), 0) FROM nodes WHERE kind = 'file' AND trashed_at IS NOT NULL) AS trash_bytes,
           (SELECT COALESCE(SUM(size), 0) FROM node_versions) AS version_bytes,
           (SELECT COALESCE(SUM(size), 0) FROM blobs) AS stored_bytes,
           (SELECT COUNT(*) FROM shares) AS share_links",
    )
    .fetch_one(&st.db)
    .await?;
    let s = st.system.read().unwrap().clone();
    Ok(SystemInfo {
        shared_enabled: s.shared_enabled,
        shared_root_id: s.shared_root_id,
        allow_user_drives: s.allow_user_drives,
        default_user_quota: s.default_user_quota,
        public_url: s.public_url,
        default_lang: s.default_lang,
        scan_minutes: s.scan_minutes,
        require_two_factor: s.require_two_factor,
        min_password_length: s.min_password_length,
        share_password_required: s.share_password_required,
        share_max_days: s.share_max_days,
        public_links: s.public_links,
        version_keep: s.version_keep,
        version_days: s.version_days,
        stats,
    })
}

pub async fn get_settings(State(st): State<AppState>, _: Admin) -> AppResult<Json<SystemInfo>> {
    Ok(Json(system_info(&st).await?))
}

#[derive(Deserialize, Default)]
pub struct SettingsReq {
    shared_enabled: Option<bool>,
    allow_user_drives: Option<bool>,
    default_user_quota: Option<i64>,
    public_url: Option<String>,
    default_lang: Option<String>,
    scan_minutes: Option<i64>,
    require_two_factor: Option<bool>,
    min_password_length: Option<usize>,
    share_password_required: Option<bool>,
    share_max_days: Option<i64>,
    public_links: Option<bool>,
    version_keep: Option<i64>,
    version_days: Option<i64>,
}

/// Values of the default interface language: follow the browser, English, Traditional Chinese
pub const LANGS: [&str; 3] = ["auto", "en", "zh-TW"];

/// Site URL: only accepts http(s)://host[:port], with the trailing / removed; blank means not set
pub fn normalize_public_url(raw: &str) -> AppResult<String> {
    let url = raw.trim().trim_end_matches('/');
    if url.is_empty() {
        return Ok(String::new());
    }
    let invalid = || AppError::bad_request("Invalid site URL. Example: https://drive.example.com or http://192.168.1.10:8080");
    let (scheme, host) = url.split_once("://").ok_or_else(invalid)?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(invalid());
    }
    if host.contains('/') {
        return Err(AppError::bad_request("Enter only the domain or IP address (and port) for the site URL, without a path"));
    }
    let ok = !host.is_empty()
        && !host.starts_with(':')
        && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'));
    if !ok {
        return Err(invalid());
    }
    Ok(format!("{}://{}", scheme.to_ascii_lowercase(), host.to_ascii_lowercase()))
}

pub async fn update_settings(State(st): State<AppState>, Admin(user): Admin, Json(req): Json<SettingsReq>) -> AppResult<Json<SystemInfo>> {
    {
        let _w = st.write_lock.lock().await;
        let mut tx = st.db.begin().await?;
        if let Some(enabled) = req.shared_enabled {
            sqlx::query("UPDATE drives SET disabled = ? WHERE kind = 'company'").bind(!enabled).execute(&mut *tx).await?;
            tree::log(&mut tx, &user, None, "settings", if enabled { "Enabled All files" } else { "Disabled All files" }).await?;
        }
        if let Some(allow) = req.allow_user_drives {
            set_setting(&mut tx, "allow_user_drives", if allow { "1" } else { "0" }).await?;
            tree::log(&mut tx, &user, None, "settings", if allow { "Allowed users to create spaces" } else { "Only administrators can create spaces" }).await?;
        }
        if let Some(q) = req.default_user_quota {
            if q < 0 {
                return Err(AppError::bad_request("Space size can't be negative"));
            }
            set_setting(&mut tx, "default_user_quota", &q.to_string()).await?;
            let label = if q == 0 { "Unlimited".to_string() } else { crate::util::format_bytes(q) };
            tree::log(&mut tx, &user, None, "settings", &format!("Default space size for new users: {label}")).await?;
        }
        let public_url = req.public_url.as_deref().map(normalize_public_url).transpose()?;
        if let Some(url) = &public_url {
            set_setting(&mut tx, "public_url", url).await?;
            tree::log(&mut tx, &user, None, "settings", &format!("Site URL: {}", if url.is_empty() { "Use the browser's current URL" } else { url })).await?;
        }
        if let Some(lang) = &req.default_lang {
            if !LANGS.contains(&lang.as_str()) {
                return Err(AppError::bad_request("Invalid default language"));
            }
            set_setting(&mut tx, "default_lang", lang).await?;
            let label = match lang.as_str() {
                "en" => "English",
                "zh-TW" => "Traditional Chinese",
                _ => "Follow the browser language",
            };
            tree::log(&mut tx, &user, None, "settings", &format!("Default language: {label}")).await?;
        }
        if let Some(m) = req.scan_minutes {
            if !(0..=1440).contains(&m) {
                return Err(AppError::bad_request("Enter a number of minutes from 0 to 1440"));
            }
            set_setting(&mut tx, "scan_minutes", &m.to_string()).await?;
            tree::log(&mut tx, &user, None, "settings", &format!("Folder spaces are checked for changes every {m} minutes")).await?;
        }
        if let Some(require) = req.require_two_factor {
            set_setting(&mut tx, "require_two_factor", if require { "1" } else { "0" }).await?;
            let detail = if require { "Two-factor sign-in required for password accounts" } else { "Two-factor sign-in optional" };
            tree::log(&mut tx, &user, None, "settings", detail).await?;
        }
        if let Some(n) = req.min_password_length {
            if !(crate::auth::MIN_PASSWORD..=crate::auth::MAX_MIN_PASSWORD).contains(&n) {
                return Err(AppError::bad_request("The minimum password length must be from 6 to 64 characters"));
            }
            set_setting(&mut tx, "min_password_length", &n.to_string()).await?;
            tree::log(&mut tx, &user, None, "settings", &format!("Minimum password length: {n} characters")).await?;
        }
        if let Some(required) = req.share_password_required {
            set_setting(&mut tx, "share_password_required", if required { "1" } else { "0" }).await?;
            let detail = if required { "Share links must have a password" } else { "Share links don't need a password" };
            tree::log(&mut tx, &user, None, "settings", detail).await?;
        }
        if let Some(days) = req.share_max_days {
            if !(0..=crate::shares::MAX_EXPIRY_DAYS).contains(&days) {
                return Err(AppError::bad_request(format!("Enter a number of days from 0 to {}", crate::shares::MAX_EXPIRY_DAYS)));
            }
            set_setting(&mut tx, "share_max_days", &days.to_string()).await?;
            let detail = match days {
                0 => "Share links may be kept without an expiry".to_string(),
                1 => "Share links must expire within 1 day".to_string(),
                n => format!("Share links must expire within {n} days"),
            };
            tree::log(&mut tx, &user, None, "settings", &detail).await?;
        }
        if let Some(on) = req.public_links {
            set_setting(&mut tx, "public_links", if on { "1" } else { "0" }).await?;
            tree::log(&mut tx, &user, None, "settings", if on { "Allowed public share links" } else { "Turned off public share links" }).await?;
        }
        if let Some(n) = req.version_keep {
            if !(0..=crate::versions::MAX_KEEP).contains(&n) {
                return Err(AppError::bad_request("Enter a number of versions from 0 to 1000"));
            }
            set_setting(&mut tx, "version_keep", &n.to_string()).await?;
            let detail = if n == 0 { "Earlier versions of files aren't kept".to_string() } else { format!("Earlier versions kept per file: {n}") };
            tree::log(&mut tx, &user, None, "settings", &detail).await?;
        }
        if let Some(d) = req.version_days {
            if !(0..=crate::versions::MAX_DAYS).contains(&d) {
                return Err(AppError::bad_request("Enter a number of days from 0 to 3650"));
            }
            set_setting(&mut tx, "version_days", &d.to_string()).await?;
            let detail = if d == 0 { "Earlier versions of files are kept without a time limit".to_string() } else { format!("Earlier versions of files are kept for {d} days") };
            tree::log(&mut tx, &user, None, "settings", &detail).await?;
        }
        tx.commit().await?;
        let mut s = st.system.write().unwrap();
        if let Some(n) = req.version_keep {
            s.version_keep = n;
        }
        if let Some(d) = req.version_days {
            s.version_days = d;
        }
        if let Some(require) = req.require_two_factor {
            s.require_two_factor = require;
        }
        if let Some(n) = req.min_password_length {
            s.min_password_length = n;
        }
        if let Some(m) = req.scan_minutes {
            s.scan_minutes = m;
        }
        if let Some(required) = req.share_password_required {
            s.share_password_required = required;
        }
        if let Some(days) = req.share_max_days {
            s.share_max_days = days;
        }
        if let Some(on) = req.public_links {
            s.public_links = on;
        }
        if let Some(lang) = req.default_lang {
            s.default_lang = lang;
        }
        if let Some(url) = public_url {
            s.public_url = url;
        }
        if let Some(q) = req.default_user_quota {
            s.default_user_quota = q;
        }
        if let Some(enabled) = req.shared_enabled {
            s.shared_enabled = enabled;
        }
        if let Some(allow) = req.allow_user_drives {
            s.allow_user_drives = allow;
        }
    }
    Ok(Json(system_info(&st).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{auth::Admin, testutil};

    #[tokio::test]
    async fn the_user_list_comes_in_pages() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        for name in ["amy", "ben", "cat"] {
            env.user(name, false).await;
        }
        let page = |after, limit| {
            let (st, admin) = (env.st.clone(), admin.clone());
            async move {
                let Json(rows) = list(State(st), Admin(admin), Query(ListQuery { after, limit })).await.unwrap();
                rows.into_iter().map(|r| r.id).collect::<Vec<_>>()
            }
        };
        let all = page(None, None).await;
        assert_eq!(all.len(), 4);
        let first = page(None, Some(2)).await;
        assert_eq!(first, all[..2]);
        assert_eq!(page(Some(first[1]), Some(2)).await, all[2..]);
    }

    fn req(name: &str, quota: Option<i64>) -> Json<CreateReq> {
        Json(CreateReq {
            username: name.into(),
            password: testutil::password().into(),
            display_name: String::new(),
            role: "user".into(),
            can_write: true,
            can_delete: true,
            can_share: true,
            quota_bytes: quota,
        })
    }

    #[tokio::test]
    async fn a_new_account_never_gets_the_id_of_a_deleted_one() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let Json(first) = create(State(env.st.clone()), Admin(admin.clone()), req("first", None)).await.unwrap();
        // The newest account is deleted: its id would be handed out again without the counter
        let _ = delete(State(env.st.clone()), Admin(admin.clone()), Path(first.id), Query(DeleteQuery::default())).await.unwrap();
        let Json(next) = create(State(env.st.clone()), Admin(admin.clone()), req("next", None)).await.unwrap();
        assert!(next.id > first.id, "the deleted account's id was given out again");
    }

    #[tokio::test]
    async fn new_users_get_the_default_quota() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let gb = 1024 * 1024 * 1024;
        let settings = SettingsReq { default_user_quota: Some(10 * gb), ..Default::default() };
        let _ = update_settings(State(env.st.clone()), Admin(admin.clone()), Json(settings)).await.unwrap();

        // Not specified: apply the default; specified (including 0 = unlimited): use the given value
        let Json(a) = create(State(env.st.clone()), Admin(admin.clone()), req("carol", None)).await.unwrap();
        assert_eq!(a.quota_bytes, 10 * gb);
        let Json(b) = create(State(env.st.clone()), Admin(admin.clone()), req("dave", Some(2 * gb))).await.unwrap();
        assert_eq!(b.quota_bytes, 2 * gb);
        let Json(c) = create(State(env.st.clone()), Admin(admin.clone()), req("erin", Some(0))).await.unwrap();
        assert_eq!(c.quota_bytes, 0);

        // The setting is saved and survives restarts
        assert_eq!(crate::db::load_system_settings(&env.st.db).await.unwrap().default_user_quota, 10 * gb);
        let bad = SettingsReq { default_user_quota: Some(-1), ..Default::default() };
        assert!(update_settings(State(env.st.clone()), Admin(admin), Json(bad)).await.is_err());
    }

    #[tokio::test]
    async fn default_language_is_validated_and_saved() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        assert_eq!(env.st.system.read().unwrap().default_lang, "auto");
        let settings = SettingsReq { default_lang: Some("zh-TW".into()), ..Default::default() };
        let Json(info) = update_settings(State(env.st.clone()), Admin(admin.clone()), Json(settings)).await.unwrap();
        assert_eq!(info.default_lang, "zh-TW");
        assert_eq!(crate::db::load_system_settings(&env.st.db).await.unwrap().default_lang, "zh-TW");
        let bad = SettingsReq { default_lang: Some("fr".into()), ..Default::default() };
        assert!(update_settings(State(env.st.clone()), Admin(admin), Json(bad)).await.is_err());
        assert_eq!(env.st.system.read().unwrap().default_lang, "zh-TW");
    }

    #[tokio::test]
    async fn minimum_password_length_applies_to_new_passwords() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        for bad in [5, 65] {
            let settings = SettingsReq { min_password_length: Some(bad), ..Default::default() };
            assert!(update_settings(State(env.st.clone()), Admin(admin.clone()), Json(settings)).await.is_err());
        }
        let settings = SettingsReq { min_password_length: Some(12), require_two_factor: Some(true), ..Default::default() };
        let Json(info) = update_settings(State(env.st.clone()), Admin(admin.clone()), Json(settings)).await.unwrap();
        assert!(info.min_password_length == 12 && info.require_two_factor);
        let saved = crate::db::load_system_settings(&env.st.db).await.unwrap();
        assert!(saved.min_password_length == 12 && saved.require_two_factor);

        let mut short = req("carol", None);
        short.password = "elevenchars".into();
        let err = create(State(env.st.clone()), Admin(admin.clone()), short).await.map(|_| ()).unwrap_err();
        assert_eq!(err.message, "Password must be at least 12 characters");
        let amy = env.user("amy", true).await;
        let update = UpdateReq { password: Some("elevenchars".into()), display_name: None, role: None, can_write: None, can_delete: None, can_share: None, quota_bytes: None, disabled: None };
        assert!(super::update(State(env.st.clone()), Admin(admin), Path(amy.id), Json(update)).await.is_err());
        let Json(me) = crate::auth::me(State(env.st.clone()), amy).await.unwrap();
        assert_eq!(me.min_password_length, 12);
    }

    #[test]
    fn public_url_is_validated_and_normalized() {
        assert_eq!(normalize_public_url(" https://Drive.Example.com/ ").unwrap(), "https://drive.example.com");
        assert_eq!(normalize_public_url("http://192.168.1.10:8080").unwrap(), "http://192.168.1.10:8080");
        assert_eq!(normalize_public_url("").unwrap(), "");
        for bad in ["drive.example.com", "ftp://x.com", "https://x.com/drive", "https://", "https://x.com?a=1", "javascript://x", "https://a b.com"] {
            assert!(normalize_public_url(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn public_url_is_saved_and_given_to_users() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let settings = SettingsReq { public_url: Some("https://drive.example.com/".into()), ..Default::default() };
        let Json(info) = update_settings(State(env.st.clone()), Admin(admin), Json(settings)).await.unwrap();
        assert_eq!(info.public_url, "https://drive.example.com");
        assert_eq!(crate::db::load_system_settings(&env.st.db).await.unwrap().public_url, "https://drive.example.com");
        let amy = env.user("amy", true).await;
        let Json(me) = crate::auth::me(State(env.st.clone()), amy).await.unwrap();
        assert_eq!(me.public_url, "https://drive.example.com");
    }

    /// A file of `size` bytes (no content) in a folder; space usage is recomputed
    async fn sized_file(env: &testutil::TestEnv, owner: &crate::auth::User, parent: &str, name: &str, size: i64) -> String {
        let id = env.file(owner, parent, name).await;
        sqlx::query("UPDATE nodes SET size = ? WHERE id = ?").bind(size).bind(&id).execute(&env.st.db).await.unwrap();
        tree::recompute_usage(&env.st).await.unwrap();
        id
    }

    async fn delete_user(env: &testutil::TestEnv, id: i64, q: serde_json::Value) -> AppResult<Json<Value>> {
        let admin = env.admin().await;
        delete(State(env.st.clone()), Admin(admin), Path(id), Query(serde_json::from_value(q).unwrap())).await
    }

    async fn drive_row(env: &testutil::TestEnv, node: &str) -> (String, String) {
        sqlx::query_as("SELECT n.drive_id, n.parent_id FROM nodes n WHERE n.id = ?").bind(node).fetch_one(&env.st.db).await.unwrap()
    }

    async fn used(env: &testutil::TestEnv, drive: &str) -> i64 {
        let (n,): (i64,) = sqlx::query_as("SELECT used_bytes FROM drives WHERE id = ?").bind(drive).fetch_one(&env.st.db).await.unwrap();
        n
    }

    #[tokio::test]
    async fn deleting_a_user_moves_their_files_into_a_folder_named_after_them() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let docs = env.folder(&amy, &amy.root_id, "Docs").await;
        let deep = env.folder(&amy, &docs, "2026").await;
        let report = sized_file(&env, &amy, &deep, "report.pdf", 300).await;
        let top = sized_file(&env, &amy, &amy.root_id, "notes.txt", 100).await;
        let binned = sized_file(&env, &amy, &amy.root_id, "old.txt", 50).await;
        sqlx::query("UPDATE nodes SET trashed_at = 1, trash_id = 't1', trash_root = 1 WHERE id = ?").bind(&binned).execute(&env.st.db).await.unwrap();
        let company = env.st.shared_root().unwrap();
        let company_drive = env.drive_of(&company).await;
        // The name is taken already: the folder gets a number
        env.folder(&amy, &company, "Files of amy").await;

        // Without a choice nothing happens
        let err = delete_user(&env, amy.id, json!({})).await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(get_row(&env.st, amy.id).await.is_ok());

        let _ = delete_user(&env, amy.id, json!({ "move_to": company_drive })).await.unwrap();
        assert!(get_row(&env.st, amy.id).await.is_err());
        let (folder,): (String,) = sqlx::query_as("SELECT id FROM nodes WHERE parent_id = ? AND name = 'Files of amy (1)'").bind(&company).fetch_one(&env.st.db).await.unwrap();
        // The whole tree is in the company space now, as it was
        assert_eq!(drive_row(&env, &docs).await, (company_drive.clone(), folder.clone()));
        assert_eq!(drive_row(&env, &top).await, (company_drive.clone(), folder.clone()));
        assert_eq!(drive_row(&env, &report).await, (company_drive.clone(), deep.clone()));
        // The trash wasn't moved: it goes with the personal space, and with the root folder is deleted in the background
        assert_eq!(used(&env, &company_drive).await, 400);
        let gone = || async {
            let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE id IN (?, ?)").bind(&binned).bind(&amy.root_id).fetch_one(&env.st.db).await.unwrap();
            n == 0
        };
        for _ in 0..200 {
            if gone().await {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(gone().await, "the trash and the root folder were left behind");
        let (moved,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE drive_id = ?").bind(&company_drive).fetch_one(&env.st.db).await.unwrap();
        assert!(moved >= 6, "the moved files were purged with the space: {moved}");
        let (personal,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM drives WHERE kind = 'personal' AND name = 'My files' AND root_id = ?").bind(&amy.root_id).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(personal, 0);
        let (detail,): (String,) = sqlx::query_as("SELECT detail FROM activity WHERE action = 'user_delete'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(detail, "amy: files moved to All files › Files of amy (1)");
        tree::recompute_usage(&env.st).await.unwrap();
        assert_eq!(used(&env, &company_drive).await, 400, "the counter matches the files");
    }

    #[tokio::test]
    async fn moving_a_deleted_users_files_respects_the_target_space() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        sized_file(&env, &amy, &amy.root_id, "big.bin", 1000).await;
        let bens = env.drive_of(&ben.root_id).await;
        let amys = env.drive_of(&amy.root_id).await;

        // Another person's personal space counts against their quota: refused with a clear message, nothing changes
        sqlx::query("UPDATE users SET quota_bytes = 500 WHERE id = ?").bind(ben.id).execute(&env.st.db).await.unwrap();
        let err = delete_user(&env, amy.id, json!({ "move_to": bens })).await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::PAYLOAD_TOO_LARGE);
        assert!(err.message.contains("doesn't have room for"), "{}", err.message);
        assert!(get_row(&env.st, amy.id).await.is_ok());
        // Not into the user's own space, both choices at once, or a space that doesn't exist
        assert!(delete_user(&env, amy.id, json!({ "move_to": amys })).await.is_err());
        assert!(delete_user(&env, amy.id, json!({ "move_to": bens, "delete_files": true })).await.is_err());
        assert!(delete_user(&env, amy.id, json!({ "move_to": "nope" })).await.is_err());

        sqlx::query("UPDATE users SET quota_bytes = 0 WHERE id = ?").bind(ben.id).execute(&env.st.db).await.unwrap();
        let _ = delete_user(&env, amy.id, json!({ "move_to": bens })).await.unwrap();
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE drive_id = ? AND name = 'big.bin'").bind(&bens).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(n, 1);
        assert_eq!(used(&env, &bens).await, 1000);
    }

    #[tokio::test]
    async fn a_folder_space_cant_take_a_deleted_users_files_and_deleting_them_stays_possible() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let file = sized_file(&env, &amy, &amy.root_id, "a.txt", 10).await;
        let space = env.folder_space("Scans").await;
        assert!(delete_user(&env, amy.id, json!({ "move_to": space.drive })).await.is_err());
        let _ = delete_user(&env, amy.id, json!({ "delete_files": true })).await.unwrap();
        // Deleted in the background
        let mut n = 1;
        for _ in 0..200 {
            (n,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE id = ?").bind(&file).fetch_one(&env.st.db).await.unwrap();
            if n == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(n, 0);
        // An empty personal space needs no choice
        let ben = env.user("ben", true).await;
        let _ = delete_user(&env, ben.id, json!({})).await.unwrap();
    }
}
