use std::{path::Path, str::FromStr, time::Duration};

use sqlx::{
    SqliteConnection, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};

use crate::{
    auth::hash_password,
    error::AppResult,
    state::SystemSettings,
    util::{new_id, now, random_token},
};

/// Opens the database; `cache_mb` is the page cache of each connection (`THIRTYFILE_DB_CACHE_MB`)
pub async fn connect(path: &Path, cache_mb: u32) -> Result<SqlitePool, Box<dyn std::error::Error>> {
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(15))
        .foreign_keys(true)
        // Page cache per connection (16 MB by default, 8 connections): the file tree, grants and blobs of a small
        // server stay in memory, and the operating system caches the rest of the file
        .pragma("cache_size", format!("-{}", u64::from(cache_mb) * 1024))
        // Temporary tables (sorting, recursive CTEs) in memory instead of on disk
        .pragma("temp_store", "MEMORY")
        // Memory-mapped reads (up to 256 MB): read queries skip the extra copy through the page cache. The mapped
        // pages are the operating system's file cache, which it can drop when memory is short
        .pragma("mmap_size", "268435456")
        // Sorting names the way File Explorer does ("File 2" before "File 10", letter case ignored in every language)
        .collation("natural_name", crate::util::natural_cmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .after_connect(|conn, _| Box::pin(async move { register_functions(conn).await }))
        .connect_with(opts)
        .await?;
    let migrator = sqlx::migrate!("./migrations");
    // Said plainly, before the database is copied or changed (`check_existing` usually said it already)
    if let Ok(applied) = sqlx::query_as::<_, (i64, Vec<u8>)>("SELECT version, checksum FROM _sqlx_migrations WHERE success = 1").fetch_all(&pool).await
        && let Some(message) = unusable(&applied, &migrator)
    {
        return Err(message.into());
    }
    backup_before_migrations(&pool, &migrator, path).await?;
    migrator.run(&pool).await?;
    optimize(&pool).await;
    Ok(pool)
}

/// The upgrade guide, which says what to do with data from a version that can't be upgraded
pub const UPGRADE_GUIDE: &str = "https://thirtyfile.github.io/ThirtyFile/docs/backup.html#upgrade";

/// Plain messages for a database this version can't use: its migrations (`_sqlx_migrations`: version and checksum)
/// compared with this version's. None when it can be used (new, current, or with migrations still to apply).
fn unusable(applied: &[(i64, Vec<u8>)], migrator: &sqlx::migrate::Migrator) -> Option<String> {
    let known = |v: i64| migrator.iter().find(|m| m.version == v && !m.migration_type.is_down_migration());
    let first = applied.iter().find(|(v, _)| *v == 1);
    let same_start = first.is_some_and(|(_, sum)| known(1).is_some_and(|m| m.checksum.as_ref() == sum.as_slice()));
    let newest = applied.iter().map(|(v, _)| *v).max()?;
    if newest > 1 && !same_start {
        // 0.3 and older kept one migration per change; the database was started over after 0.3
        return Some(format!(
            "This database is from ThirtyFile 0.3 or older, which can't be upgraded to this version. Nothing was changed. See how to move your files over in the upgrade guide: {UPGRADE_GUIDE}"
        ));
    }
    if let Some((v, _)) = applied.iter().find(|(v, _)| known(*v).is_none()) {
        return Some(format!(
            "This database was changed by a newer version of ThirtyFile (it has change {v}, which this version doesn't know). Start the newer version again, or restore the copy saved before the upgrade: {UPGRADE_GUIDE}"
        ));
    }
    if let Some((v, _)) = applied.iter().find(|(v, sum)| known(*v).is_some_and(|m| m.checksum.as_ref() != sum.as_slice())) {
        return Some(format!(
            "This database was made by a different build of ThirtyFile (its change {v} isn't the one this version has): an older release or a development build. It can't be used by this version. Nothing was changed. See the upgrade guide: {UPGRADE_GUIDE}"
        ));
    }
    None
}

/// Before anything is written into the data or storage folder: stops with a plain message when the database in
/// `path` is one this version can't use (from 0.3 or older, from a newer version, or from another build). A database
/// that isn't there, or can't be read here, is left to `connect`.
pub async fn check_existing(path: &Path) -> Result<(), String> {
    if !path.is_file() {
        return Ok(());
    }
    let Ok(opts) = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display())) else { return Ok(()) };
    let opts = opts.read_only(true).create_if_missing(false);
    let Ok(mut conn) = sqlx::ConnectOptions::connect(&opts).await else { return Ok(()) };
    let applied: Result<Vec<(i64, Vec<u8>)>, _> =
        sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations WHERE success = 1").fetch_all(&mut conn).await;
    let _ = sqlx::Connection::close(conn).await;
    match applied {
        Ok(applied) => unusable(&applied, &sqlx::migrate!("./migrations")).map_or(Ok(()), Err),
        // No migrations table: a new database
        Err(_) => Ok(()),
    }
}

/// Brings the query planner's statistics up to date where they are missing or old (after connecting, and daily): without
/// them SQLite can pick an index that reads a whole space instead of the one that finds a single path
pub async fn optimize(pool: &SqlitePool) {
    if let Err(e) = sqlx::query("PRAGMA optimize=0x10002").execute(pool).await {
        tracing::warn!("Couldn't update the database statistics: {e}");
    }
}

/// Automatic backups kept before upgrades (the oldest are removed)
const UPGRADE_BACKUPS: usize = 3;

/// When this version brings migrations the database doesn't have yet, copies the database to
/// `backups/drive-before-<version>.db` next to it first: going back to the older version means restoring it, since
/// an older version refuses to start on a database changed by a newer one.
async fn backup_before_migrations(pool: &SqlitePool, migrator: &sqlx::migrate::Migrator, path: &Path) -> Result<(), sqlx::Error> {
    // A new database has no migrations table yet: nothing to keep
    let Ok(applied) = sqlx::query_as::<_, (i64,)>("SELECT version FROM _sqlx_migrations WHERE success = 1").fetch_all(pool).await else {
        return Ok(());
    };
    let applied: std::collections::HashSet<i64> = applied.into_iter().map(|(v,)| v).collect();
    if applied.is_empty() || migrator.iter().all(|m| applied.contains(&m.version)) {
        return Ok(());
    }
    let dir = path.parent().unwrap_or(Path::new(".")).join("backups");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("drive-before-{}.db", crate::VERSION));
    if file.exists() {
        // An earlier start of this version failed after the copy: keep that one, it has the old schema
        return Ok(());
    }
    tracing::info!("Upgrading the database: saving a copy of it first in {}", file.display());
    backup_to(pool, &file).await?;
    // Keep the newest few
    let mut old: Vec<(std::time::SystemTime, std::path::PathBuf)> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("drive-before-"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    old.sort();
    for (_, p) in old.iter().rev().skip(UPGRADE_BACKUPS) {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

/// A consistent copy of the running database in one file (`VACUUM INTO`, safe while the server writes, unlike copying
/// drive.db with its WAL file). Refuses to overwrite an existing file.
pub async fn backup_to(pool: &SqlitePool, file: &Path) -> Result<(), sqlx::Error> {
    if file.exists() {
        return Err(sqlx::Error::Protocol(format!("{} already exists", file.display())));
    }
    sqlx::query("VACUUM INTO ?").bind(file.to_string_lossy().into_owned()).execute(pool).await?;
    Ok(())
}

/// Registers `unicode_lower(text)` on a connection: lower case in every language, where SQLite's `lower()` and
/// `NOCASE` only fold A–Z. Names in the content store are unique by this key (the `name_key` column), so it must be
/// the same rule as Rust's `to_lowercase()`, which the server uses to compare names in memory.
///
/// The column's index calls the function, so the database can only be changed by ThirtyFile itself (reading it with
/// the `sqlite3` tool works, as long as `name_key` isn't selected).
async fn register_functions(conn: &mut SqliteConnection) -> Result<(), sqlx::Error> {
    use libsqlite3_sys as ffi;
    use std::ffi::{c_int, c_void};

    unsafe extern "C" fn unicode_lower(ctx: *mut ffi::sqlite3_context, argc: c_int, argv: *mut *mut ffi::sqlite3_value) {
        // SAFETY: SQLite passes `argc` valid values, and the text pointer is valid for `sqlite3_value_bytes` bytes
        // until the next call on this value. The result is copied by SQLite (SQLITE_TRANSIENT).
        unsafe {
            if argc != 1 {
                ffi::sqlite3_result_null(ctx);
                return;
            }
            let value = *argv;
            if ffi::sqlite3_value_type(value) == ffi::SQLITE_NULL {
                ffi::sqlite3_result_null(ctx);
                return;
            }
            let text = ffi::sqlite3_value_text(value);
            let len = ffi::sqlite3_value_bytes(value);
            let bytes = if text.is_null() { &[][..] } else { std::slice::from_raw_parts(text, len.max(0) as usize) };
            let lower = String::from_utf8_lossy(bytes).to_lowercase();
            ffi::sqlite3_result_text(ctx, lower.as_ptr().cast(), lower.len() as c_int, ffi::SQLITE_TRANSIENT());
        }
    }

    let mut handle = conn.lock_handle().await?;
    let db = handle.as_raw_handle().as_ptr();
    let flags = ffi::SQLITE_UTF8 | ffi::SQLITE_DETERMINISTIC | ffi::SQLITE_INNOCUOUS;
    // SAFETY: `db` is the open connection, held locked for the call; the function has no user data or destructor.
    let rc = unsafe {
        ffi::sqlite3_create_function_v2(db, c"unicode_lower".as_ptr(), 1, flags, std::ptr::null_mut::<c_void>(), Some(unicode_lower), None, None, None)
    };
    if rc != ffi::SQLITE_OK {
        return Err(sqlx::Error::Protocol(format!("Couldn't register unicode_lower() (SQLite error {rc})")));
    }
    Ok(())
}

/// Gets (or generates on first startup) the server secret used for signing, stored encrypted (secrets.rs)
pub async fn load_secret(db: &SqlitePool) -> Result<Vec<u8>, sqlx::Error> {
    if let Some((v,)) = sqlx::query_as::<_, (String,)>("SELECT value FROM settings WHERE key = 'secret'")
        .fetch_optional(db)
        .await?
    {
        match crate::secrets::open("settings:secret", &v) {
            Ok(secret) => return Ok(secret.into_bytes()),
            // A database restored without its key: a new signing secret only signs everyone out and ends share links'
            // unlocked sessions, which is better than not starting
            Err(e) => tracing::error!("The signing secret can't be read ({e}); a new one is made, so everyone has to sign in again"),
        }
    }
    let secret = random_token(64);
    sqlx::query("INSERT INTO settings (key, value) VALUES ('secret', ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
        .bind(crate::secrets::seal("settings:secret", &secret))
        .execute(db)
        .await?;
    Ok(secret.into_bytes())
}

/// Encrypts every stored secret again with `new_key`, when the key is rotated. Returns how many values were written.
pub async fn reseal_secrets(db: &SqlitePool, new_key: &[u8; 32]) -> Result<usize, sqlx::Error> {
    let fix = |context: &str, v: &str| -> Result<Option<String>, sqlx::Error> {
        if v.is_empty() {
            return Ok(None);
        }
        crate::secrets::reseal(context, v, new_key).map(Some).map_err(sqlx::Error::Protocol)
    };
    let mut tx = begin_write(db).await?;
    let mut n = 0;
    if let Some((v,)) = sqlx::query_as::<_, (String,)>("SELECT value FROM settings WHERE key = 'secret'").fetch_optional(&mut *tx).await?
        && let Some(sealed) = fix("settings:secret", &v)?
    {
        set_setting(&mut tx, "secret", &sealed).await?;
        n += 1;
    }
    if let Some((v,)) = sqlx::query_as::<_, (String,)>("SELECT value FROM settings WHERE key = 'sso'").fetch_optional(&mut *tx).await?
        && let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&v)
    {
        let mut changed = false;
        for p in crate::sso::PROVIDERS {
            if let Some(secret) = json[p]["client_secret"].as_str().map(str::to_string)
                && let Some(sealed) = fix(&format!("sso:{p}"), &secret)?
            {
                json[p]["client_secret"] = sealed.into();
                changed = true;
                n += 1;
            }
        }
        if changed {
            set_setting(&mut tx, "sso", &json.to_string()).await?;
        }
    }
    if let Some((v,)) = sqlx::query_as::<_, (String,)>("SELECT value FROM settings WHERE key = 'smtp'").fetch_optional(&mut *tx).await?
        && let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&v)
        && let Some(secret) = json["password"].as_str().map(str::to_string)
        && let Some(sealed) = fix("smtp", &secret)?
    {
        json["password"] = sealed.into();
        set_setting(&mut tx, "smtp", &json.to_string()).await?;
        n += 1;
    }
    let totp: Vec<(i64, String)> = sqlx::query_as("SELECT id, totp_secret FROM users WHERE totp_secret IS NOT NULL").fetch_all(&mut *tx).await?;
    for (id, secret) in totp {
        if let Some(sealed) = fix(&format!("user:{id}:totp"), &secret)? {
            sqlx::query("UPDATE users SET totp_secret = ? WHERE id = ?").bind(sealed).bind(id).execute(&mut *tx).await?;
            n += 1;
        }
    }
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT id, config FROM storage_locations").fetch_all(&mut *tx).await?;
    for (id, config) in rows {
        let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&config) else { continue };
        let mut changed = false;
        for field in crate::locations::SECRET_FIELDS {
            if let Some(secret) = json[field].as_str().map(str::to_string)
                && let Some(sealed) = fix(&format!("location:{id}:{field}"), &secret)?
            {
                json[field] = sealed.into();
                changed = true;
                n += 1;
            }
        }
        if changed {
            sqlx::query("UPDATE storage_locations SET config = ? WHERE id = ?").bind(json.to_string()).bind(&id).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    Ok(n)
}

pub async fn get_setting(db: &SqlitePool, key: &str) -> Result<Option<String>, sqlx::Error> {
    Ok(sqlx::query_as::<_, (String,)>("SELECT value FROM settings WHERE key = ?").bind(key).fetch_optional(db).await?.map(|(v,)| v))
}

pub async fn set_setting(conn: &mut SqliteConnection, key: &str, value: &str) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
        .bind(key)
        .bind(value)
        .execute(conn)
        .await?;
    Ok(())
}

/// Starts a transaction that writes (`BEGIN IMMEDIATE`): every transaction that may write starts here, never with
/// `begin()` (clippy.toml refuses it). It takes SQLite's write lock at once, waiting for it up to the busy timeout
/// (`connect`), and keeps it until it ends: start it after taking `st.write_lock`, once slow work (hashing, other
/// storage) is done.
///
/// A deferred transaction (`BEGIN`) only asks for the lock at its first write, usually after reading. If another
/// connection holds the lock then, SQLite answers "database is locked" at once instead of waiting, as waiting could
/// deadlock. That happens when a transaction is dropped after writing (a handler returning an error with `?`): sqlx
/// rolls it back later, in the background, so its connection may still hold the lock when the next writer, which has
/// the server's write lock by then, starts writing.
///
/// Read-only transactions (a consistent view over several queries) may still use a deferred `begin()`, with
/// `#[allow(clippy::disallowed_methods)]` and a reason.
pub async fn begin_write(pool: &SqlitePool) -> Result<sqlx::Transaction<'static, sqlx::Sqlite>, sqlx::Error> {
    pool.begin_with("BEGIN IMMEDIATE").await
}

/// Ends a write transaction by what the work in it returned: committed when it succeeded, rolled back when it failed,
/// before the write lock is released. A transaction that is only dropped rolls back later, in the background, and keeps
/// SQLite's write lock meanwhile: harmless, as the next writer (`begin_write`) waits for it, but settling it releases
/// the lock straight away.
pub async fn settle<T>(tx: sqlx::Transaction<'_, sqlx::Sqlite>, res: AppResult<T>) -> AppResult<T> {
    match res {
        Ok(v) => {
            tx.commit().await?;
            Ok(v)
        }
        Err(e) => {
            tx.rollback().await?;
            Err(e)
        }
    }
}

pub async fn location_exists(conn: &mut SqliteConnection, id: &str) -> Result<bool, sqlx::Error> {
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM storage_locations WHERE id = ?").bind(id).fetch_one(&mut *conn).await?;
    Ok(n > 0)
}

/// Checks a storage location an administrator chose for a new space
pub async fn check_location(conn: &mut SqliteConnection, id: &str) -> AppResult<()> {
    if location_exists(conn, id).await? { Ok(()) } else { Err(crate::error::AppError::not_found("Storage location not found")) }
}

/// Creates a space and its root folder on the storage location `location_id`, returning (space id, root folder id).
/// The space records the location for good (only a move changes it, locations.rs): its files go there, or its folder
/// once `space_folders::make_folder_space` makes it a folder space. The caller creates the access grants separately.
pub async fn create_drive(
    conn: &mut SqliteConnection,
    name: &str,
    kind: &str,
    owner_id: i64,
    quota_bytes: i64,
    location_id: &str,
) -> Result<(String, String), sqlx::Error> {
    let drive_id = new_id();
    let root_id = new_id();
    let ts = now();
    sqlx::query(
        "INSERT INTO nodes (id, owner_id, parent_id, kind, name, drive_id, created_at, updated_at)
         VALUES (?, ?, NULL, 'folder', '', ?, ?, ?)",
    )
    .bind(&root_id)
    .bind(owner_id)
    .bind(&drive_id)
    .bind(ts)
    .bind(ts)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO drives (id, name, kind, root_id, owner_id, quota_bytes, created_by, created_at, location_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&drive_id)
    .bind(name)
    .bind(kind)
    .bind(&root_id)
    .bind(owner_id)
    .bind(quota_bytes)
    .bind(owner_id)
    .bind(ts)
    .bind(location_id)
    .execute(&mut *conn)
    .await?;
    Ok((drive_id, root_id))
}

pub async fn add_grant(
    conn: &mut SqliteConnection,
    node_id: &str,
    principal_type: &str,
    principal_id: i64,
    role: &str,
    granted_by: Option<i64>,
    expires_at: Option<i64>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO grants (node_id, principal_type, principal_id, role, granted_by, created_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (node_id, principal_type, principal_id)
         DO UPDATE SET role = excluded.role, granted_by = excluded.granted_by, expires_at = excluded.expires_at",
    )
    .bind(node_id)
    .bind(principal_type)
    .bind(principal_id)
    .bind(role)
    .bind(granted_by)
    .bind(now())
    .bind(expires_at)
    .execute(conn)
    .await?;
    Ok(())
}

/// On the first start, creates the company space "All files" (editable by everyone): a folder space in `folders`
/// (see `AppState::space_folders`) when its location is a folder of this server
pub async fn create_company_space(db: &SqlitePool, folders: Option<&Path>) -> AppResult<()> {
    let (exists,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM drives WHERE kind = 'company')").fetch_one(db).await?;
    if exists {
        return Ok(());
    }
    let (admin_id,): (i64,) = sqlx::query_as("SELECT MIN(id) FROM users WHERE role = 'admin'").fetch_one(db).await?;
    let mut tx = begin_write(db).await?;
    let location = crate::locations::default_location(&mut tx).await?;
    let (drive_id, root_id) = create_drive(&mut tx, "All files", "company", admin_id, 0, &location).await?;
    crate::space_folders::make_folder_space(&mut tx, folders, &drive_id).await?;
    add_grant(&mut tx, &root_id, "everyone", 0, "editor", None, None).await?;
    set_setting(&mut tx, "shared_root_id", &root_id).await?;
    tx.commit().await?;
    Ok(())
}

/// Loads system settings (the company space is created first, by `create_company_space`)
pub async fn load_system_settings(db: &SqlitePool) -> Result<SystemSettings, sqlx::Error> {
    let (shared_root_id, disabled): (String, bool) =
        sqlx::query_as("SELECT root_id, disabled FROM drives WHERE kind = 'company' LIMIT 1").fetch_one(db).await?;
    let allow_user_drives = get_setting(db, "allow_user_drives").await?.as_deref() == Some("1");
    let default_user_quota = get_setting(db, "default_user_quota").await?.and_then(|v| v.parse().ok()).unwrap_or(0).max(0);
    let personal_spaces = get_setting(db, "personal_spaces").await?.as_deref() != Some("0");
    let personal_location = get_setting(db, "personal_location").await?.unwrap_or_default();
    let public_url = get_setting(db, "public_url").await?.unwrap_or_default();
    let default_lang = get_setting(db, "default_lang")
        .await?
        .filter(|v| crate::admin::LANGS.contains(&v.as_str()))
        .unwrap_or_else(|| "auto".into());
    let scan_minutes = get_setting(db, "scan_minutes").await?.and_then(|v| v.parse().ok()).unwrap_or(15).clamp(0, 1440);
    let require_two_factor = get_setting(db, "require_two_factor").await?.as_deref() == Some("1");
    let min_password_length = get_setting(db, "min_password_length")
        .await?
        .and_then(|v| v.parse().ok())
        .unwrap_or(crate::auth::MIN_PASSWORD)
        .clamp(crate::auth::MIN_PASSWORD, crate::auth::MAX_MIN_PASSWORD);
    let share_password_required = get_setting(db, "share_password_required").await?.as_deref() == Some("1");
    let share_max_days = get_setting(db, "share_max_days").await?.and_then(|v| v.parse().ok()).unwrap_or(0).clamp(0, crate::shares::MAX_EXPIRY_DAYS);
    let public_links = get_setting(db, "public_links").await?.as_deref() != Some("0");
    let version_keep = get_setting(db, "version_keep")
        .await?
        .and_then(|v| v.parse().ok())
        .unwrap_or(crate::versions::DEFAULT_KEEP)
        .clamp(0, crate::versions::MAX_KEEP);
    let version_days = get_setting(db, "version_days")
        .await?
        .and_then(|v| v.parse().ok())
        .unwrap_or(crate::versions::DEFAULT_DAYS)
        .clamp(0, crate::versions::MAX_DAYS);
    let move_jobs = get_setting(db, "move_jobs").await?.and_then(|v| v.parse().ok()).unwrap_or(1).clamp(1, crate::moves::MAX_JOBS);
    Ok(SystemSettings {
        shared_enabled: !disabled,
        shared_root_id,
        allow_user_drives,
        default_user_quota,
        personal_spaces,
        personal_location,
        public_url,
        default_lang,
        scan_minutes,
        require_two_factor,
        min_password_length,
        share_password_required,
        share_max_days,
        public_links,
        version_keep,
        version_days,
        move_jobs,
    })
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
    pub space_folders: Option<&'a Path>,
}

/// Tables whose ids are never given out twice
pub enum Counted {
    Users,
    Groups,
}

/// The next id for a user or group: one higher than any id given out before, even when that row was deleted since, so
/// a new account or group can't pick up log entries or sign-in settings still pointing at a deleted one
pub async fn next_id(conn: &mut SqliteConnection, table: Counted) -> Result<i64, sqlx::Error> {
    let (name, sql) = match table {
        Counted::Users => (
            "users",
            "INSERT INTO id_counters (name, last) VALUES (?1, (SELECT COALESCE(MAX(id), 0) FROM users) + 1)
             ON CONFLICT (name) DO UPDATE SET last = MAX(id_counters.last, (SELECT COALESCE(MAX(id), 0) FROM users)) + 1
             RETURNING last",
        ),
        Counted::Groups => (
            "groups",
            "INSERT INTO id_counters (name, last) VALUES (?1, (SELECT COALESCE(MAX(id), 0) FROM groups) + 1)
             ON CONFLICT (name) DO UPDATE SET last = MAX(id_counters.last, (SELECT COALESCE(MAX(id), 0) FROM groups)) + 1
             RETURNING last",
        ),
    };
    let (id,): (i64,) = sqlx::query_as(sql).bind(name).fetch_one(&mut *conn).await?;
    Ok(id)
}

/// Creates a user, with their personal space when `u.personal_space` names its location, returning the user id. The
/// caller must hold the write lock, and calls `folders::spaces_changed` after committing (the personal space may be a
/// folder space).
pub async fn create_user(conn: &mut SqliteConnection, u: NewUser<'_>) -> AppResult<i64> {
    let id = next_id(conn, Counted::Users).await?;
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
    .bind(now())
    .bind(u.source)
    .bind(u.provisioned_by)
    .execute(&mut *conn)
    .await?;
    if let Some(location) = u.personal_space {
        crate::personal::create_or_wait(conn, u.space_folders, id, u.username, location).await?;
    }
    Ok(id)
}

/// Creates the default administrator when there are no users. `space_folders`: see `NewUser`.
pub async fn bootstrap_admin(db: &SqlitePool, password: Option<&str>, space_folders: Option<&Path>) -> AppResult<()> {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users").fetch_one(db).await?;
    if count > 0 {
        return Ok(());
    }
    // Any password is accepted here: the administrator is asked to change it after signing in.
    // An empty one counts as not set, so `THIRTYFILE_ADMIN_PASSWORD=` in a compose file gets a random password.
    let (password, generated) = match password.filter(|p| !p.is_empty()) {
        Some(p) => (p.to_string(), false),
        None => (random_token(16), true),
    };
    let password_hash = hash_password(password.clone()).await?;
    let mut tx = begin_write(db).await?;
    // The first administrator gets "My files" on the built-in storage (there are no settings yet)
    let location = crate::locations::default_location(&mut tx).await?;
    create_user(
        &mut tx,
        NewUser {
            username: "admin",
            password_hash: &password_hash,
            role: "admin",
            can_write: true,
            can_delete: true,
            can_share: true,
            quota_bytes: 0,
            source: "password",
            provisioned_by: None,
            personal_space: Some(&location),
            space_folders,
        },
    )
    .await?;
    tx.commit().await?;
    if generated {
        tracing::warn!("Created default administrator account admin, password: {password} (change it right after signing in)");
    } else {
        tracing::info!("Created default administrator account admin (password from THIRTYFILE_ADMIN_PASSWORD)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database file whose migrations table holds `applied` (version, checksum), as an earlier version left it
    async fn database_with(dir: &Path, applied: &[(i64, Vec<u8>)]) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("drive.db");
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display())).unwrap().create_if_missing(true);
        let mut c = sqlx::ConnectOptions::connect(&opts).await.unwrap();
        sqlx::query(
            "CREATE TABLE _sqlx_migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL, installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                                            success BOOLEAN NOT NULL, checksum BLOB NOT NULL, execution_time BIGINT NOT NULL)",
        )
        .execute(&mut c)
        .await
        .unwrap();
        for (v, sum) in applied {
            sqlx::query("INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (?, 'old', 1, ?, 0)")
                .bind(v)
                .bind(sum)
                .execute(&mut c)
                .await
                .unwrap();
        }
        sqlx::Connection::close(c).await.unwrap();
        path
    }

    #[tokio::test]
    async fn databases_this_version_cant_use_are_refused_plainly_and_left_alone() {
        let base = std::env::temp_dir().join(format!("thirtyfile-old-{}", crate::util::new_id()));
        let current = sqlx::migrate!("./migrations").iter().find(|m| m.version == 1).unwrap().checksum.to_vec();
        let cases = [
            // 0.3 kept a migration per change
            ("0.3", (1..=24).map(|v| (v, vec![v as u8; 48])).collect::<Vec<_>>(), "ThirtyFile 0.3 or older"),
            ("newer", vec![(1, current.clone()), (2, vec![2; 48])], "a newer version"),
            ("other build", vec![(1, vec![9; 48])], "a different build"),
        ];
        for (what, applied, says) in cases {
            let dir = base.join(what.replace(' ', "-"));
            let path = database_with(&dir, &applied).await;
            let e = check_existing(&path).await.unwrap_err();
            assert!(e.contains(says) && e.contains(UPGRADE_GUIDE), "{what}: {e}");
            let e = connect(&path, 16).await.unwrap_err().to_string();
            assert!(e.contains(says), "{what}: {e}");
            assert!(!dir.join("backups").exists(), "{what}: nothing copied");
        }
        // A new database, and one from this version, are fine
        check_existing(&base.join("none").join("drive.db")).await.unwrap();
        let db = connect(&base.join("drive.db"), 16).await.unwrap();
        db.close().await;
        check_existing(&base.join("drive.db")).await.unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    async fn admin_hash(password: Option<&str>) -> String {
        let dir = std::env::temp_dir().join(format!("thirtyfile-test-{}", crate::util::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = connect(&dir.join("drive.db"), 16).await.unwrap();
        bootstrap_admin(&db, password, None).await.unwrap();
        let (hash,): (String,) = sqlx::query_as("SELECT password_hash FROM users WHERE username = 'admin'").fetch_one(&db).await.unwrap();
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
        hash
    }

    #[tokio::test]
    async fn the_database_is_copied_before_new_migrations_and_on_request() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-test-{}", crate::util::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("drive.db");
        let db = connect(&path, 16).await.unwrap();
        // Up to date: no copy
        let current = sqlx::migrate!("./migrations");
        backup_before_migrations(&db, &current, &path).await.unwrap();
        assert!(!dir.join("backups").exists());
        // A newer version with one more migration: copied first
        let newer_dir = dir.join("migrations");
        std::fs::create_dir_all(&newer_dir).unwrap();
        for e in std::fs::read_dir("migrations").unwrap() {
            let e = e.unwrap();
            std::fs::copy(e.path(), newer_dir.join(e.file_name())).unwrap();
        }
        std::fs::write(newer_dir.join("9999_next.sql"), "CREATE TABLE next (id INTEGER);").unwrap();
        let newer = sqlx::migrate::Migrator::new(newer_dir.as_path()).await.unwrap();
        backup_before_migrations(&db, &newer, &path).await.unwrap();
        let copy = dir.join("backups").join(format!("drive-before-{}.db", crate::VERSION));
        let copied = connect(&copy, 16).await.unwrap();
        let (users,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users").fetch_one(&copied).await.unwrap();
        assert_eq!(users, 0);
        copied.close().await;
        // On request, never over an existing file
        backup_to(&db, &dir.join("manual.db")).await.unwrap();
        assert!(backup_to(&db, &dir.join("manual.db")).await.is_err());
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn saved_secrets_are_encrypted_again_when_the_key_is_rotated() {
        let env = crate::testutil::env().await;
        let db = &env.st.db;
        let pw = crate::testutil::password();
        let secret = load_secret(db).await.unwrap();
        let mut c = db.acquire().await.unwrap();
        let mut sso = crate::sso::SsoSettings::default();
        sso.google = crate::sso::ProviderConfig { enabled: true, client_id: "id".into(), client_secret: pw.to_string(), ..Default::default() };
        crate::sso::store(&mut c, &sso).await.unwrap();
        drop(c);
        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, is_default, created_at) VALUES ('nas', 'NAS', 'sftp', ?, 0, 0)")
            .bind(serde_json::json!({ "host": "h", "password": crate::secrets::seal("location:nas:password", pw) }).to_string())
            .execute(db)
            .await
            .unwrap();
        let dump = || async {
            let s: Vec<(String,)> = sqlx::query_as("SELECT value FROM settings UNION ALL SELECT config FROM storage_locations").fetch_all(db).await.unwrap();
            s.into_iter().map(|(v,)| v).collect::<Vec<_>>().join("
")
        };
        let stored = dump().await;
        assert!(!stored.contains(pw), "{stored}");
        assert_eq!(crate::sso::load(db).await.google.client_secret, pw);
        // Rotating re-encrypts all of them
        assert_eq!(reseal_secrets(db, &rand::random()).await.unwrap(), 3);
        assert_ne!(dump().await, stored);
        // Now this process's key can't read them (as after restoring without the key): the server still starts, with
        // a new signing secret, and without the client secret
        let fresh = load_secret(db).await.unwrap();
        assert_ne!(fresh, secret);
        assert_eq!(load_secret(db).await.unwrap(), fresh);
        assert_eq!(crate::sso::load(db).await.google.client_secret, "");
    }

    #[tokio::test]
    async fn the_first_administrator_password_has_no_minimum_length() {
        let short: String = crate::util::new_id().chars().take(5).collect();
        let hash = admin_hash(Some(&short)).await;
        assert!(crate::auth::verify_password(short, hash).await.unwrap());
    }

    #[tokio::test]
    async fn an_empty_first_administrator_password_counts_as_not_set() {
        let hash = admin_hash(Some("")).await;
        assert!(!crate::auth::verify_password(String::new(), hash).await.unwrap());
    }

    /// A write that fails after writing drops its transaction, which sqlx rolls back later, in the background, while
    /// the next writer already has the write lock. The writers after it (reading first, like most handlers) wait for
    /// SQLite's lock instead of failing with "database is locked".
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_write_that_fails_after_writing_does_not_make_the_next_ones_fail() {
        let env = crate::testutil::env().await;
        let mut writers = tokio::task::JoinSet::new();
        for w in 0..6 {
            let st = env.st.clone();
            writers.spawn(async move {
                for i in 0..100 {
                    let _w = st.write_lock.lock().await;
                    let mut tx = begin_write(&st.db).await?;
                    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM settings").fetch_one(&mut *tx).await?;
                    set_setting(&mut tx, &format!("stress-{w}"), &format!("{i}-{n}")).await?;
                    if (w + i) % 3 == 0 {
                        // Fails after writing: the transaction is dropped (as `?` does), then the lock released
                        continue;
                    }
                    tx.commit().await?;
                }
                Ok::<_, sqlx::Error>(())
            });
        }
        while let Some(res) = writers.join_next().await {
            res.unwrap().unwrap();
        }
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM settings WHERE key LIKE 'stress-%'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(n, 6);
    }
}
