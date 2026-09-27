//! Administration: user accounts, roles, permissions and quotas.

use axum::{
    Json,
    extract::{Path, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    auth::{Admin, hash_password, validate_password},
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
    /// 'password' (created by an administrator) or the provider that created the account automatically
    source: String,
    /// Email of the most recently used linked sign-in (to spot accounts whose username no longer matches)
    sso_email: String,
    used_bytes: i64,
}

const USER_ROW_SQL: &str = "SELECT u.id, u.username, u.display_name, u.role, u.can_write, u.can_delete, u.can_share, u.quota_bytes, u.disabled, u.created_at, u.last_login_at, u.source,
       (SELECT COALESCE(GROUP_CONCAT(provider), '') FROM user_identities WHERE user_id = u.id) AS sso,
       (SELECT COALESCE(email, '') FROM user_identities WHERE user_id = u.id ORDER BY last_login_at DESC LIMIT 1) AS sso_email,
       (SELECT COALESCE(SUM(used_bytes), 0) FROM drives WHERE kind = 'personal' AND owner_id = u.id) AS used_bytes
     FROM users u";

pub async fn list(State(st): State<AppState>, _: Admin) -> AppResult<Json<Vec<UserRow>>> {
    let sql = format!("{USER_ROW_SQL} ORDER BY u.id");
    Ok(Json(sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).fetch_all(&st.db).await?))
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
    validate_password(&req.password)?;
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
            validate_password(p)?;
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

/// Deletes a user: their personal space is permanently deleted too; files they uploaded to other spaces and team spaces they own are transferred to the administrator performing the deletion
pub async fn delete(State(st): State<AppState>, Admin(me): Admin, Path(id): Path<i64>) -> AppResult<Json<Value>> {
    if id == me.id {
        return Err(AppError::bad_request("You can't delete your own account"));
    }
    let row = get_row(&st, id).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    let personal: Option<(String, String)> = sqlx::query_as("SELECT id, root_id FROM drives WHERE kind = 'personal' AND owner_id = ?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let uploads: Vec<(String,)> = sqlx::query_as("SELECT id FROM uploads WHERE owner_id = ?").bind(id).fetch_all(&mut *tx).await?;
    sqlx::query("DELETE FROM uploads WHERE owner_id = ?").bind(id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM shares WHERE owner_id = ?").bind(id).execute(&mut *tx).await?;
    let mut orphans = Vec::new();
    if let Some((drive_id, root_id)) = personal {
        sqlx::query("DELETE FROM drives WHERE id = ?").bind(&drive_id).execute(&mut *tx).await?;
        orphans = tree::purge_subtree(&mut tx, &root_id).await?;
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
    sqlx::query("DELETE FROM users WHERE id = ?").bind(id).execute(&mut *tx).await?;
    tree::log(&mut tx, &me, None, "user_delete", &row.username).await?;
    tx.commit().await?;
    tree::schedule_blob_removal(&st, orphans);
    for (u,) in uploads {
        let _ = tokio::fs::remove_file(st.tmp_dir().join(format!("upload-{u}"))).await;
    }
    Ok(Json(json!({ "ok": true })))
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
        tx.commit().await?;
        let mut s = st.system.write().unwrap();
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

    fn req(name: &str, quota: Option<i64>) -> Json<CreateReq> {
        Json(CreateReq {
            username: name.into(),
            password: "password-1234".into(),
            display_name: String::new(),
            role: "user".into(),
            can_write: true,
            can_delete: true,
            can_share: true,
            quota_bytes: quota,
        })
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
}
