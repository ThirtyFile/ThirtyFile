//! Who may sign in with a user name and password: everyone, administrators only, or nobody (everyone through a provider)
//!
//! Turning passwords off must never leave a site nobody can sign in to, so as long as the setting is not "everyone"
//! there has to be an enabled administrator linked to a provider that is ready. Every change that could take that away
//! (disabling, deleting or demoting that administrator, unlinking their account, turning a provider off, an
//! administrator's password reset, which unlinks the account) checks it again inside its own transaction.

use super::*;
use crate::auth::UserRole;

/// Who may sign in with a password (Control panel › Single sign-on)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PasswordSignIn {
    /// Everyone
    #[default]
    Everyone,
    /// Administrators only: the way back in when the provider is down or set up wrongly
    Administrators,
    /// Nobody: every account signs in through a provider
    Nobody,
}

impl PasswordSignIn {
    /// Whether an account with this role may sign in with its password
    pub fn allows(self, role: UserRole) -> bool {
        match self {
            PasswordSignIn::Everyone => true,
            PasswordSignIn::Administrators => role == UserRole::Admin,
            PasswordSignIn::Nobody => false,
        }
    }

    /// The name used on the command line (`thirtyfile password-sign-in <mode>`)
    pub fn parse(s: &str) -> Option<PasswordSignIn> {
        match s {
            "everyone" | "on" => Some(PasswordSignIn::Everyone),
            "administrators" => Some(PasswordSignIn::Administrators),
            "nobody" | "off" => Some(PasswordSignIn::Nobody),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            PasswordSignIn::Everyone => "everyone",
            PasswordSignIn::Administrators => "administrators",
            PasswordSignIn::Nobody => "nobody",
        }
    }
}

/// The setting in force, read from the database each time: `thirtyfile password-sign-in` changes it from outside the
/// running server, and has to take effect at once
pub async fn mode(db: &sqlx::SqlitePool) -> AppResult<PasswordSignIn> {
    let stored: Option<Option<String>> =
        sqlx::query_scalar("SELECT json_extract(value, '$.password_sign_in') FROM settings WHERE key = 'sso'").fetch_optional(db).await?;
    Ok(stored.flatten().as_deref().and_then(PasswordSignIn::parse).unwrap_or_default())
}

/// Whether the account may sign in with its password now. A missing account is told "yes": there is nothing to refuse.
pub async fn may_use_password(st: &AppState, user_id: i64) -> AppResult<bool> {
    let mode = mode(&st.db).await?;
    if mode == PasswordSignIn::Everyone {
        return Ok(true);
    }
    let role: Option<(UserRole,)> = sqlx::query_as("SELECT role FROM users WHERE id = ?").bind(user_id).fetch_optional(&st.db).await?;
    Ok(role.is_none_or(|(role,)| mode.allows(role)))
}

/// An enabled administrator who can sign in through a ready provider
#[derive(Debug, Serialize)]
pub struct SsoAdmin {
    pub id: i64,
    pub username: String,
    /// Has signed in through the provider at least once, so it is known to work
    pub signed_in: bool,
}

/// The enabled administrators linked to a provider that is ready under `settings`
pub async fn sso_admins(conn: &mut sqlx::SqliteConnection, settings: &SsoSettings) -> AppResult<Vec<SsoAdmin>> {
    let rows: Vec<(i64, String, String, Option<i64>)> = sqlx::query_as(
        "SELECT u.id, u.username, i.provider, i.last_login_at
         FROM users u JOIN user_identities i ON i.user_id = u.id
         WHERE u.role = 'admin' AND u.disabled = 0
         ORDER BY u.username COLLATE NOCASE, u.id",
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut admins: Vec<SsoAdmin> = Vec::new();
    for (id, username, provider, last_login) in rows {
        if !settings.provider(&provider).is_some_and(ProviderConfig::ready) {
            continue;
        }
        match admins.iter_mut().find(|a| a.id == id) {
            Some(a) => a.signed_in |= last_login.is_some(),
            None => admins.push(SsoAdmin { id, username, signed_in: last_login.is_some() }),
        }
    }
    Ok(admins)
}

const NO_ADMIN: &str = "Single sign-on would be the only way to sign in, and no enabled administrator is linked to a sign-in provider that is on";

/// Refuses when passwords are not open to everyone and no enabled administrator could sign in through a provider.
/// Run inside the transaction of a change, after the change was made, so refusing rolls it back.
pub async fn ensure_sign_in_possible(conn: &mut sqlx::SqliteConnection, settings: &SsoSettings) -> AppResult<()> {
    if settings.password_sign_in == PasswordSignIn::Everyone {
        return Ok(());
    }
    if sso_admins(conn, settings).await?.is_empty() {
        return Err(AppError::conflict(NO_ADMIN));
    }
    Ok(())
}

/// The checks for saving a setting other than "everyone", by the administrator `saver`: the saver is one of the
/// administrators who can sign in through a provider, and (for "nobody") has signed in that way, so a wrong setup is
/// found now, not when the last password session ends
pub async fn check_saving(conn: &mut sqlx::SqliteConnection, settings: &SsoSettings, saver: i64) -> AppResult<()> {
    if settings.password_sign_in == PasswordSignIn::Everyone {
        return Ok(());
    }
    if !PROVIDERS.iter().any(|p| settings.provider(p).is_some_and(ProviderConfig::ready)) {
        return Err(AppError::bad_request("Turn on and set up a sign-in provider first"));
    }
    let admins = sso_admins(conn, settings).await?;
    let Some(me) = admins.iter().find(|a| a.id == saver) else {
        return Err(AppError::bad_request(
            "Link your own account to one of the sign-in providers first (My account › Sign-in methods): you have to be able to sign in that way",
        ));
    };
    if settings.password_sign_in == PasswordSignIn::Nobody && !me.signed_in {
        return Err(AppError::bad_request("Sign in through a provider once with your own account first, to be sure it works"));
    }
    Ok(())
}

/// The command line (`thirtyfile password-sign-in <mode>`): sets the mode without signing in, for when the provider no
/// longer works. Only that one value of the stored settings changes, so the sealed client secrets are left as they are.
/// Returns the previous mode.
pub async fn set_from_command_line(db: &sqlx::SqlitePool, mode: PasswordSignIn) -> AppResult<PasswordSignIn> {
    let mut tx = crate::db::begin_write(db).await?;
    let row: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key = 'sso'").fetch_optional(&mut *tx).await?;
    let mut stored: Value = match row {
        Some((text,)) => serde_json::from_str(&text).map_err(AppError::internal)?,
        None => json!({}),
    };
    let before = stored.get("password_sign_in").and_then(Value::as_str).and_then(PasswordSignIn::parse).unwrap_or_default();
    stored["password_sign_in"] = json!(mode.name());
    set_setting(&mut tx, "sso", &stored.to_string()).await?;
    tx.commit().await?;
    Ok(before)
}

#[cfg(test)]
mod tests;
