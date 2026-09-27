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
pub async fn connect(path: &Path, cache_mb: u32) -> Result<SqlitePool, sqlx::Error> {
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
    backup_before_migrations(&pool, &migrator, path).await?;
    migrator.run(&pool).await?;
    Ok(pool)
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

/// Gets (or generates on first startup) the server secret used for signing.
pub async fn load_secret(db: &SqlitePool) -> Result<Vec<u8>, sqlx::Error> {
    if let Some((v,)) = sqlx::query_as::<_, (String,)>("SELECT value FROM settings WHERE key = 'secret'")
        .fetch_optional(db)
        .await?
    {
        return Ok(v.into_bytes());
    }
    let secret = random_token(64);
    sqlx::query("INSERT INTO settings (key, value) VALUES ('secret', ?)").bind(&secret).execute(db).await?;
    Ok(secret.into_bytes())
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

/// Creates a space and its root folder, returning (space id, root folder id). The caller creates the access grants separately.
pub async fn create_drive(
    conn: &mut SqliteConnection,
    name: &str,
    kind: &str,
    owner_id: i64,
    quota_bytes: i64,
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
        "INSERT INTO drives (id, name, kind, root_id, owner_id, quota_bytes, created_by, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&drive_id)
    .bind(name)
    .bind(kind)
    .bind(&root_id)
    .bind(owner_id)
    .bind(quota_bytes)
    .bind(owner_id)
    .bind(ts)
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

/// Loads system settings; on first startup creates the "All files" company space (editable by everyone).
pub async fn load_system_settings(db: &SqlitePool) -> Result<SystemSettings, sqlx::Error> {
    let company: Option<(String, bool)> =
        sqlx::query_as("SELECT root_id, disabled FROM drives WHERE kind = 'company' LIMIT 1").fetch_optional(db).await?;
    let (shared_root_id, disabled) = match company {
        Some(c) => c,
        None => {
            let (admin_id,): (i64,) = sqlx::query_as("SELECT MIN(id) FROM users WHERE role = 'admin'").fetch_one(db).await?;
            let mut tx = db.begin().await?;
            let (_, root_id) = create_drive(&mut tx, "All files", "company", admin_id, 0).await?;
            add_grant(&mut tx, &root_id, "everyone", 0, "editor", None, None).await?;
            set_setting(&mut tx, "shared_root_id", &root_id).await?;
            tx.commit().await?;
            (root_id, false)
        }
    };
    let allow_user_drives = get_setting(db, "allow_user_drives").await?.as_deref() == Some("1");
    let default_user_quota = get_setting(db, "default_user_quota").await?.and_then(|v| v.parse().ok()).unwrap_or(0).max(0);
    let public_url = get_setting(db, "public_url").await?.unwrap_or_default();
    let default_lang = get_setting(db, "default_lang")
        .await?
        .filter(|v| crate::admin::LANGS.contains(&v.as_str()))
        .unwrap_or_else(|| "auto".into());
    let scan_minutes = get_setting(db, "scan_minutes").await?.and_then(|v| v.parse().ok()).unwrap_or(15).clamp(0, 1440);
    Ok(SystemSettings { shared_enabled: !disabled, shared_root_id, allow_user_drives, default_user_quota, public_url, default_lang, scan_minutes })
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

/// Creates a user and their personal space, returning the user id. The caller must hold the write lock.
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
    let (_, root_id) = create_drive(conn, "My files", "personal", id, 0).await?;
    add_grant(conn, &root_id, "user", id, "owner", Some(id), None).await?;
    sqlx::query("UPDATE users SET root_id = ? WHERE id = ?").bind(&root_id).bind(id).execute(&mut *conn).await?;
    Ok(id)
}

/// Creates the default administrator when there are no users.
pub async fn bootstrap_admin(db: &SqlitePool, password: Option<&str>) -> AppResult<()> {
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
    let mut tx = db.begin().await?;
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

    async fn admin_hash(password: Option<&str>) -> String {
        let dir = std::env::temp_dir().join(format!("thirtyfile-test-{}", crate::util::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = connect(&dir.join("drive.db"), 16).await.unwrap();
        bootstrap_admin(&db, password).await.unwrap();
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
}
