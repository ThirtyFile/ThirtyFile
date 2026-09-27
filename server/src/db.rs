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

pub async fn connect(path: &Path) -> Result<SqlitePool, sqlx::Error> {
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(15))
        .foreign_keys(true)
        // Page cache per connection (64 MB): the file tree, grants and blobs stay in memory on a small server
        .pragma("cache_size", "-65536")
        // Temporary tables (sorting, recursive CTEs) in memory instead of on disk
        .pragma("temp_store", "MEMORY")
        // Memory-mapped reads (up to 256 MB): read queries skip the extra copy through the page cache
        .pragma("mmap_size", "268435456");
    let pool = SqlitePoolOptions::new().max_connections(8).connect_with(opts).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
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
    Ok(SystemSettings { shared_enabled: !disabled, shared_root_id, allow_user_drives, default_user_quota, public_url, default_lang })
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

/// Creates a user and their personal space, returning the user id. The caller must hold the write lock.
pub async fn create_user(conn: &mut SqliteConnection, u: NewUser<'_>) -> AppResult<i64> {
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO users (username, password_hash, role, can_write, can_delete, can_share, quota_bytes, created_at, source, provisioned_by)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
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
    .fetch_one(&mut *conn)
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
    let (password, generated) = match password {
        Some(p) => {
            if p.chars().count() < 8 {
                return Err(crate::error::AppError::bad_request("THIRTYFILE_ADMIN_PASSWORD must be at least 8 characters"));
            }
            (p.to_string(), false)
        }
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
