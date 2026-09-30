//! Administration: the system settings

use super::*;

#[derive(Serialize)]
pub struct SystemInfo {
    pub(super) shared_enabled: bool,
    pub(super) shared_root_id: String,
    pub(super) allow_user_drives: bool,
    /// Default capacity of new users' personal spaces (bytes, 0 = unlimited)
    pub(super) default_user_quota: i64,
    /// New users get a personal space ("My files")
    pub(super) personal_spaces: bool,
    /// The storage location of new personal spaces; blank = the default location
    pub(super) personal_location: String,
    pub(super) public_url: String,
    pub(super) default_lang: String,
    pub(super) scan_minutes: i64,
    pub(super) require_two_factor: bool,
    pub(super) min_password_length: usize,
    /// Public share links: must have a password
    pub(super) share_password_required: bool,
    /// Public share links: must expire within this many days (0 = no limit)
    pub(super) share_max_days: i64,
    /// Public share links can be created and opened
    pub(super) public_links: bool,
    /// Earlier versions kept per file (0 = none), and for how many days (0 = no limit)
    pub(super) version_keep: i64,
    pub(super) version_days: i64,
    pub(super) stats: SystemStats,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct SystemStats {
    pub(super) users: i64,
    pub(super) groups: i64,
    pub(super) team_drives: i64,
    pub(super) personal_bytes: i64,
    pub(super) personal_files: i64,
    pub(super) shared_bytes: i64,
    pub(super) shared_files: i64,
    pub(super) team_bytes: i64,
    pub(super) team_files: i64,
    pub(super) trash_bytes: i64,
    /// Earlier versions of files (not counted toward the spaces' quotas)
    pub(super) version_bytes: i64,
    /// Storage actually used: the content store (duplicate files are stored only once), plus the files of folder
    /// spaces and their earlier versions, which are ordinary files on the disk
    pub(super) stored_bytes: i64,
    pub(super) share_links: i64,
}

pub(super) async fn system_info(st: &AppState) -> AppResult<SystemInfo> {
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
           (SELECT COALESCE(SUM(size), 0) FROM blobs)
             + (SELECT COALESCE(SUM(used_bytes), 0) FROM drives WHERE mode = 'folder')
             + (SELECT COALESCE(SUM(size), 0) FROM node_versions WHERE blob_hash IS NULL) AS stored_bytes,
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
        personal_spaces: s.personal_spaces,
        personal_location: s.personal_location,
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
    pub(super) shared_enabled: Option<bool>,
    pub(super) allow_user_drives: Option<bool>,
    pub(super) default_user_quota: Option<i64>,
    pub(super) personal_spaces: Option<bool>,
    /// A storage location's id, or blank for the default location
    pub(super) personal_location: Option<String>,
    pub(super) public_url: Option<String>,
    pub(super) default_lang: Option<String>,
    pub(super) scan_minutes: Option<i64>,
    pub(super) require_two_factor: Option<bool>,
    pub(super) min_password_length: Option<usize>,
    pub(super) share_password_required: Option<bool>,
    pub(super) share_max_days: Option<i64>,
    pub(super) public_links: Option<bool>,
    pub(super) version_keep: Option<i64>,
    pub(super) version_days: Option<i64>,
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
        let mut tx = crate::db::begin_write(&st.db).await?;
        if let Some(enabled) = req.shared_enabled {
            sqlx::query("UPDATE drives SET disabled = ? WHERE kind = 'company'").bind(!enabled).execute(&mut *tx).await?;
            logs::record_activity(&mut tx, &user, None, "settings", if enabled { "Enabled All files" } else { "Disabled All files" }).await?;
        }
        if let Some(allow) = req.allow_user_drives {
            set_setting(&mut tx, "allow_user_drives", if allow { "1" } else { "0" }).await?;
            logs::record_activity(&mut tx, &user, None, "settings", if allow { "Allowed users to create spaces" } else { "Only administrators can create spaces" }).await?;
        }
        if let Some(q) = req.default_user_quota {
            if q < 0 {
                return Err(AppError::bad_request("Space size can't be negative"));
            }
            set_setting(&mut tx, "default_user_quota", &q.to_string()).await?;
            let label = if q == 0 { "Unlimited".to_string() } else { crate::util::format_bytes(q) };
            logs::record_activity(&mut tx, &user, None, "settings", &format!("Default space size for new users: {label}")).await?;
        }
        if let Some(on) = req.personal_spaces {
            set_setting(&mut tx, "personal_spaces", if on { "1" } else { "0" }).await?;
            let detail = if on { "New users get My files" } else { "New users don't get My files" };
            logs::record_activity(&mut tx, &user, None, "settings", detail).await?;
        }
        let personal_location = req.personal_location.as_deref().map(str::trim);
        if let Some(location) = personal_location {
            let detail = if location.is_empty() {
                "Location of new users' My files: the default location".to_string()
            } else {
                let name: Option<(String,)> = sqlx::query_as("SELECT name FROM storage_locations WHERE id = ?").bind(location).fetch_optional(&mut *tx).await?;
                format!("Location of new users' My files: {}", name.ok_or_else(|| AppError::not_found("Storage location not found"))?.0)
            };
            set_setting(&mut tx, "personal_location", location).await?;
            logs::record_activity(&mut tx, &user, None, "settings", &detail).await?;
        }
        let public_url = req.public_url.as_deref().map(normalize_public_url).transpose()?;
        if let Some(url) = &public_url {
            set_setting(&mut tx, "public_url", url).await?;
            logs::record_activity(&mut tx, &user, None, "settings", &format!("Site URL: {}", if url.is_empty() { "Use the browser's current URL" } else { url })).await?;
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
            logs::record_activity(&mut tx, &user, None, "settings", &format!("Default language: {label}")).await?;
        }
        if let Some(m) = req.scan_minutes {
            if !(0..=1440).contains(&m) {
                return Err(AppError::bad_request("Enter a number of minutes from 0 to 1440"));
            }
            set_setting(&mut tx, "scan_minutes", &m.to_string()).await?;
            logs::record_activity(&mut tx, &user, None, "settings", &format!("Folder spaces are checked for changes every {m} minutes")).await?;
        }
        if let Some(require) = req.require_two_factor {
            set_setting(&mut tx, "require_two_factor", if require { "1" } else { "0" }).await?;
            // Turned on: password sign-ins without a second factor end (they set it up when signing in again), except
            // the administrator's own, who is asked the next time
            if require && !st.system.read().unwrap().require_two_factor {
                sqlx::query(
                    "DELETE FROM sessions WHERE method = 'password' AND id IS NOT ?
                       AND user_id IN (SELECT id FROM users WHERE totp_secret IS NULL AND password_hash != ?)",
                )
                .bind(&user.session_id)
                .bind(crate::sso::NO_PASSWORD)
                .execute(&mut *tx)
                .await?;
            }
            let detail = if require { "Two-factor sign-in required for password accounts" } else { "Two-factor sign-in optional" };
            logs::record_activity(&mut tx, &user, None, "settings", detail).await?;
        }
        if let Some(n) = req.min_password_length {
            if !(crate::auth::MIN_PASSWORD..=crate::auth::MAX_MIN_PASSWORD).contains(&n) {
                return Err(AppError::bad_request("The minimum password length must be from 6 to 64 characters"));
            }
            set_setting(&mut tx, "min_password_length", &n.to_string()).await?;
            logs::record_activity(&mut tx, &user, None, "settings", &format!("Minimum password length: {n} characters")).await?;
        }
        if let Some(required) = req.share_password_required {
            set_setting(&mut tx, "share_password_required", if required { "1" } else { "0" }).await?;
            let detail = if required { "Share links must have a password" } else { "Share links don't need a password" };
            logs::record_activity(&mut tx, &user, None, "settings", detail).await?;
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
            logs::record_activity(&mut tx, &user, None, "settings", &detail).await?;
        }
        if let Some(on) = req.public_links {
            set_setting(&mut tx, "public_links", if on { "1" } else { "0" }).await?;
            logs::record_activity(&mut tx, &user, None, "settings", if on { "Allowed public share links" } else { "Turned off public share links" }).await?;
        }
        if let Some(n) = req.version_keep {
            if !(0..=crate::versions::MAX_KEEP).contains(&n) {
                return Err(AppError::bad_request("Enter a number of versions from 0 to 1000"));
            }
            set_setting(&mut tx, "version_keep", &n.to_string()).await?;
            let detail = if n == 0 { "Earlier versions of files aren't kept".to_string() } else { format!("Earlier versions kept per file: {n}") };
            logs::record_activity(&mut tx, &user, None, "settings", &detail).await?;
        }
        if let Some(d) = req.version_days {
            if !(0..=crate::versions::MAX_DAYS).contains(&d) {
                return Err(AppError::bad_request("Enter a number of days from 0 to 3650"));
            }
            set_setting(&mut tx, "version_days", &d.to_string()).await?;
            let detail = if d == 0 { "Earlier versions of files are kept without a time limit".to_string() } else { format!("Earlier versions of files are kept for {d} days") };
            logs::record_activity(&mut tx, &user, None, "settings", &detail).await?;
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
        if let Some(on) = req.personal_spaces {
            s.personal_spaces = on;
        }
        if let Some(location) = personal_location {
            s.personal_location = location.to_string();
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

pub struct NewUser<'a> {
    pub username: &'a str,
    /// Argon2 hash (see `auth::hash_password`); hashed by the caller before taking the write lock, as it takes ~100 ms
    pub password_hash: &'a str,
    pub role: &'a str,
    pub can_write: bool,
    pub can_delete: bool,
    pub can_share: bool,
    pub quota_bytes: i64,
    /// 'password' for accounts created by an administrator, otherwise the sign-in provider that created the account
    pub source: &'a str,
    /// Automatically created accounts: the provider's identifier of the person
    pub provisioned_by: Option<&'a str>,
    /// The storage location of their personal space ("My files"), or None for no personal space (see
    /// `personal::choose`). When the space can't be created there now, it is created later (personal.rs).
    pub personal_space: Option<&'a str>,
    /// `AppState::space_folders`: their "My files" is a folder space in `users/<user name>` when its location is a
    /// folder of this server
    pub space_folders: Option<&'a std::path::Path>,
}

/// Creates a user, with their personal space when `u.personal_space` names its location, returning the user id. The
/// caller must hold the write lock, and calls `folders::spaces_changed` after committing (the personal space may be a
/// folder space).
pub async fn create_user(conn: &mut sqlx::SqliteConnection, u: NewUser<'_>) -> AppResult<i64> {
    let id = crate::db::next_id(conn, crate::db::Counted::Users).await?;
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, can_write, can_delete, can_share, quota_bytes, created_at, source, provisioned_by)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(u.username)
    .bind(u.password_hash)
    .bind(u.role)
    .bind(u.can_write)
    .bind(u.can_delete)
    .bind(u.can_share)
    .bind(u.quota_bytes)
    .bind(crate::util::now())
    .bind(u.source)
    .bind(u.provisioned_by)
    .execute(&mut *conn)
    .await?;
    if let Some(location) = u.personal_space {
        crate::personal::create_or_wait(conn, u.space_folders, id, u.username, location).await?;
    }
    Ok(id)
}
