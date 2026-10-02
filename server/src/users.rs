//! Accounts: the rules for user names and display names, and creating an account (by an administrator, by single
//! sign-on, and the first administrator when a server starts for the first time)

use crate::error::{AppError, AppResult};

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

pub fn validate_username(name: &str) -> AppResult<()> {
    let ok = (2..=32).contains(&name.chars().count()) && name.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '@'));
    if ok { Ok(()) } else { Err(AppError::bad_request("Username must be 2–32 characters and can only contain letters, numbers, and _ - . @")) }
}

pub struct NewUser<'a> {
    pub username: &'a str,
    /// Argon2 hash (see `auth::hash_password`); hashed by the caller before taking the write lock, as it takes ~100 ms
    pub password_hash: &'a str,
    pub role: crate::auth::UserRole,
    pub can_write: bool,
    pub can_delete: bool,
    pub can_share: bool,
    pub quota_bytes: i64,
    /// 'password' for accounts created by an administrator, otherwise the sign-in provider that created the account
    pub source: &'a str,
    /// Automatically created accounts: the provider's identifier of the person
    pub provisioned_by: Option<&'a str>,
    /// The storage location of their personal space ("My files"), or None for no personal space (see
    /// `personal::choose`). When the space can't be created there now, it is created later (personal/).
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
