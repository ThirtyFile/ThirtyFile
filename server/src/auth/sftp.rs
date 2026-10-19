//! Rechecks an SSH actor inside the file operation's transaction, after staging or waiting for a space's turn.
//! The context contains only a database row ID; it never contains an app password or private key.

use super::{USER_COLS, User};
use crate::error::{AppError, AppResult};
use sqlx::SqliteConnection;

#[derive(Debug, Clone)]
pub enum SftpCredential {
    AppPassword(String),
    Key(String),
}

pub async fn current_user(conn: &mut SqliteConnection, user: &User, write: bool) -> AppResult<Option<User>> {
    let Some(credential) = &user.sftp_credential else { return Ok(None) };
    let cfg = crate::sftp::settings::from_connection(conn).await?;
    if !cfg.enabled {
        return Err(AppError::unauthorized());
    }
    let active = match credential {
        SftpCredential::AppPassword(id) => {
            let (active,): (bool,) = sqlx::query_as(
                "SELECT EXISTS(SELECT 1 FROM app_passwords WHERE id = ? AND user_id = ? AND (expires_at IS NULL OR expires_at > ?) AND (? = 0 OR scope = 'write'))",
            )
            .bind(id)
            .bind(user.id)
            .bind(crate::util::now())
            .bind(write)
            .fetch_one(&mut *conn)
            .await?;
            active
        }
        SftpCredential::Key(id) => {
            let (known,): (bool,) =
                sqlx::query_as("SELECT EXISTS(SELECT 1 FROM sftp_keys WHERE id = ? AND user_id = ?)").bind(id).bind(user.id).fetch_one(&mut *conn).await?;
            known && cfg.keys()
        }
    };
    if !active {
        return Err(AppError::unauthorized());
    }
    let sql = format!("SELECT {USER_COLS} FROM users u WHERE u.id = ? AND u.disabled = 0");
    let mut current: User = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(user.id).fetch_optional(&mut *conn).await?.ok_or_else(AppError::unauthorized)?;
    current.shared_root = user.shared_root.clone();
    current.activity_source = user.activity_source.clone();
    current.sftp_credential = user.sftp_credential.clone();
    Ok(Some(current))
}
