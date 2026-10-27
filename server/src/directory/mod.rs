//! Directory sign-in (Control panel › Directory sign-in): signing in with the password of an LDAP directory (Active
//! Directory, OpenLDAP, FreeIPA), accounts and groups that follow it.
//!
//! A username that belongs to a directory account is checked by finding the person with the service account and
//! binding as that entry with the password given; the password is never stored or logged. Accounts made by hand keep
//! their own passwords and never go to the directory. An account is linked by the directory's stable id
//! (`user_identities`, provider `ldap`), so a rename there doesn't make a second account.

mod admin;
mod settings;
mod sync;

#[cfg(test)]
mod real;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};

pub use admin::{get, save, sync_now, test};
#[cfg(test)]
pub use settings::Mapping;
pub use settings::{GroupMode, SEAL, Settings, load};
pub use sync::spawn;

use serde_json::json;

use crate::{
    auth::{NO_PASSWORD, UserRole},
    error::AppError,
    ldap::{self, Connection, Entry, Scope, Search, filter},
    logs,
    state::AppState,
    users::{NewUser, create_user},
    util::now,
};

/// The sign-in provider's name in `user_identities` and `users.source`
pub const PROVIDER: &str = "ldap";

/// Why a directory sign-in didn't sign anyone in
pub enum Failure {
    /// The person typed something that isn't right (`event`: what the sign-in log says); the account, when there is one
    Wrong {
        event: &'static str,
        user: Option<i64>,
    },
    /// The directory can't be used now: said as such, and not counted as a wrong password
    Unavailable(String),
    Error(AppError),
}

pub struct Signed {
    pub user_id: i64,
    pub username: String,
}

/// Whether a directory is told of a sign-in for this user name: it is turned on, and the name belongs to no account that
/// has a password of its own here. (`source`: the account's, when there is one)
pub async fn applies(st: &AppState, source: Option<&str>) -> Option<Settings> {
    if source.is_some_and(|s| s != PROVIDER) {
        return None;
    }
    let settings = load(&st.db).await;
    (settings.enabled && settings.check().is_ok()).then_some(settings)
}

/// What directory sign-in keeps in memory (a part of `AppState`)
#[derive(Default)]
pub struct Memory {
    /// Whether the directory couldn't be used the last time it was tried, so that the alert is told once when it begins
    /// and once when it is over
    down: AtomicBool,
    /// A sync is under way
    running: AtomicBool,
}

async fn mark(st: &AppState, down: bool, why: &str) {
    if st.part::<Memory>().down.swap(down, Ordering::Relaxed) != down {
        crate::alerts::notice(st, "directory", down, &json!({ "name": "directory", "error": why })).await;
    }
}

/// The connection with the service account bound
pub(super) async fn open(settings: &Settings) -> Result<Connection, ldap::Error> {
    let config = settings.connection().map_err(ldap::Error::Invalid)?;
    let password = settings.password().map_err(ldap::Error::Invalid)?;
    let mut conn = Connection::connect(&config).await?;
    conn.bind(&settings.bind_dn, &password).await?;
    Ok(conn)
}

/// What a search for a name found
pub(super) enum Lookup {
    Nobody,
    One(Box<Entry>),
    /// More than one entry: refused, as a name that points at two people points at neither
    Many,
}

pub(super) fn attributes(settings: &Settings) -> Vec<String> {
    let mut attrs: Vec<String> =
        [&settings.attr_id, &settings.attr_username, &settings.attr_name, &settings.attr_email].into_iter().filter(|a| !a.trim().is_empty()).cloned().collect();
    attrs.extend(["userAccountControl", "nsAccountLock", "pwdAccountLockedTime"].map(String::from));
    if settings.group_mode == GroupMode::MemberOf {
        attrs.push("memberOf".into());
    }
    attrs
}

pub(super) async fn find_user(conn: &mut Connection, settings: &Settings, username: &str) -> Result<Lookup, ldap::Error> {
    let filter = filter::for_user(&settings.user_filter, username).map_err(|e| ldap::Error::Invalid(e.0.into()))?;
    let found = conn
        .search(&Search {
            base: settings.user_base.clone(),
            scope: Scope::Subtree,
            filter,
            attributes: attributes(settings),
            size_limit: 2,
            page_size: 0,
            max_entries: 2,
        })
        .await?;
    Ok(match found.entries.len() {
        0 => Lookup::Nobody,
        1 if !found.truncated => Lookup::One(Box::new(found.entries.into_iter().next().expect("one entry"))),
        _ => Lookup::Many,
    })
}

/// Whether the directory has the person turned on (Active Directory's account control, FreeIPA's lock, a policy lock)
pub(super) fn entry_enabled(entry: &Entry) -> bool {
    let disabled_in_ad = entry.text("userAccountControl").and_then(|v| v.trim().parse::<u32>().ok()).is_some_and(|flags| flags & 0x2 != 0);
    let locked = entry.text("nsAccountLock").is_some_and(|v| v.eq_ignore_ascii_case("true")) || entry.text("pwdAccountLockedTime").is_some();
    !disabled_in_ad && !locked
}

/// The person's stable id as text: what is text stays so, and binary (an `objectGUID`) is written in hex
pub(super) fn stable_id(entry: &Entry, attr: &str) -> Option<String> {
    let bytes = entry.bytes(attr).filter(|b| !b.is_empty())?;
    Some(match std::str::from_utf8(bytes) {
        Ok(text) if text.chars().all(|c| !c.is_control()) => text.trim().to_ascii_lowercase(),
        _ => bytes.iter().map(|b| format!("{b:02x}")).collect(),
    })
}

/// The directory groups the person is in (their DNs, normalized): from their own `memberOf` values, or by searching
pub(super) async fn memberships(conn: &mut Connection, settings: &Settings, entry: &Entry) -> Result<HashSet<String>, ldap::Error> {
    match settings.group_mode {
        GroupMode::None => Ok(HashSet::new()),
        GroupMode::MemberOf => Ok(entry.texts("memberOf").iter().map(|d| Settings::normalize_dn(d)).collect()),
        GroupMode::Search => {
            let filter = filter::parse(&settings.group_filter.replace("{dn}", &filter::escape(&entry.dn))).map_err(|e| ldap::Error::Invalid(e.0.into()))?;
            let found = conn
                .search(&Search {
                    base: settings.group_base.clone(),
                    scope: Scope::Subtree,
                    filter,
                    attributes: vec!["1.1".into()],
                    size_limit: 0,
                    page_size: 500,
                    max_entries: 20_000,
                })
                .await?;
            Ok(found.entries.iter().map(|e| Settings::normalize_dn(&e.dn)).collect())
        }
    }
}

fn unavailable(e: &ldap::Error) -> Failure {
    Failure::Unavailable(e.to_string())
}

/// Checks a user name and password against the directory, and finds (or makes) the account they sign in to
pub async fn authenticate(st: &AppState, settings: &Settings, typed: &str, password: &str) -> Result<Signed, Failure> {
    if password.is_empty() || typed.chars().count() > 256 {
        return Err(Failure::Wrong { event: "bad_password", user: None });
    }
    let mut conn = match open(settings).await {
        Ok(c) => c,
        Err(e) => {
            let down = e.is_unavailable() || e.is_bad_credentials() || matches!(e, ldap::Error::Server { .. } | ldap::Error::Invalid(_));
            // The service account itself that doesn't work is the administrator's to fix, not the person's
            let why = if e.is_bad_credentials() { "The service account's name or password is wrong".to_string() } else { e.to_string() };
            if down {
                mark(st, true, &why).await;
            }
            return Err(Failure::Unavailable(why));
        }
    };
    let entry = match find_user(&mut conn, settings, typed).await {
        Ok(Lookup::One(e)) => *e,
        Ok(Lookup::Nobody) => return Err(Failure::Wrong { event: "unknown_user", user: None }),
        Ok(Lookup::Many) => {
            tracing::warn!("Directory sign-in: more than one entry answers to \"{}\": refused", typed.chars().take(64).collect::<String>());
            return Err(Failure::Wrong { event: "unknown_user", user: None });
        }
        Err(e) => return Err(unavailable(&e)),
    };
    let denied = |event| Failure::Wrong { event, user: None };
    if !entry_enabled(&entry) {
        return Err(denied("disabled"));
    }
    let groups = match memberships(&mut conn, settings, &entry).await {
        Ok(g) => g,
        Err(e) => return Err(unavailable(&e)),
    };
    if !settings.require_group.trim().is_empty() && !groups.contains(&Settings::normalize_dn(&settings.require_group)) {
        return Err(denied("sso_denied"));
    }
    // The person's own password: the same connection, bound as them
    match conn.bind(&entry.dn, password).await {
        Ok(()) => {}
        Err(e) if e.is_unavailable() => return Err(unavailable(&e)),
        Err(_) => return Err(Failure::Wrong { event: "bad_password", user: None }),
    }
    conn.unbind().await;
    mark(st, false, "").await;
    let Some(id) = stable_id(&entry, &settings.attr_id) else {
        return Err(Failure::Error(AppError::forbidden("The directory gave no stable id for this account. Ask your administrator to check the attribute for it.")));
    };
    account_for(st, settings, &entry, &id, typed, &groups).await
}

/// A user name made of what the directory calls the person, in the characters a name here can have
fn username_of(entry: &Entry, settings: &Settings, typed: &str) -> String {
    let given = entry.text(&settings.attr_username).unwrap_or_default();
    let clean: String = given.trim().chars().map(|c| if c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '@') { c } else { '_' }).take(32).collect();
    if clean.chars().count() >= 2 { clean } else { typed.chars().filter(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '@')).take(32).collect() }
}

/// The account this directory entry signs in to, made when there is none and the settings allow it
async fn account_for(st: &AppState, settings: &Settings, entry: &Entry, id: &str, typed: &str, groups: &HashSet<String>) -> Result<Signed, Failure> {
    let result = account_inner(st, settings, entry, id, typed, groups).await;
    result.map_err(|e| match e {
        Refused::Wrong(event, user) => Failure::Wrong { event, user },
        Refused::Error(e) => Failure::Error(e),
    })
}

enum Refused {
    Wrong(&'static str, Option<i64>),
    Error(AppError),
}

impl From<AppError> for Refused {
    fn from(e: AppError) -> Refused {
        Refused::Error(e)
    }
}

impl From<sqlx::Error> for Refused {
    fn from(e: sqlx::Error) -> Refused {
        Refused::Error(e.into())
    }
}

/// Who is signing in: the directory's entry and what it says, and what was typed
#[derive(Clone, Copy)]
struct Who<'a> {
    entry: &'a Entry,
    id: &'a str,
    typed: &'a str,
    name: &'a str,
    email: &'a str,
}

/// A first sign-in: the account is made (or an account of the directory whose link was lost is found) and linked
async fn new_account(
    st: &AppState,
    tx: &mut sqlx::SqliteConnection,
    settings: &Settings,
    who: &Who<'_>,
    mode: crate::sso::PasswordSignIn,
) -> Result<(i64, String), Refused> {
    let Who { entry, id, typed, name, email } = *who;
    if !mode.allows(UserRole::User) {
        return Err(Refused::Wrong("password_off", None));
    }
    if !settings.create_accounts {
        return Err(Refused::Error(AppError::forbidden("This directory account has no account here yet. Ask your administrator.")));
    }
    let base = username_of(entry, settings, typed);
    crate::users::validate_username(&base)?;
    // A name an account made here has: that one keeps its own password and is never the directory's. Another person of
    // the directory with the same name (one who left, and a new one who got it) is another account, never that one.
    let mut free = base.clone();
    for i in 2..100 {
        let taken: Option<(String,)> = sqlx::query_as("SELECT source FROM users WHERE username = ?").bind(&free).fetch_optional(&mut *tx).await?;
        match taken {
            None => break,
            Some((source,)) if source != PROVIDER && free == base => {
                return Err(Refused::Error(AppError::forbidden(
                    "An account with this name already exists here and doesn't belong to the directory. Ask your administrator.",
                )));
            }
            Some(_) => free = format!("{}-{i}", base.chars().take(28).collect::<String>()),
        }
    }
    let base = free;
    let (recent,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM users WHERE source = ? AND created_at > ?").bind(PROVIDER).bind(now() - 3600).fetch_one(&mut *tx).await?;
    if recent >= crate::sso::MAX_CREATED_PER_HOUR {
        tracing::warn!("Directory sign-in: account creation paused, {recent} accounts were created in the last hour");
        return Err(Refused::Error(AppError::forbidden(
            "Too many accounts were created in the last hour. Try again later or ask your administrator to create your account.",
        )));
    }
    let personal = crate::personal::choose(st, &mut *tx, None, None, false).await?;
    let quota = st.system.read().default_user_quota.max(0);
    let user_id = create_user(
        &mut *tx,
        NewUser {
            username: &base,
            password_hash: NO_PASSWORD,
            role: UserRole::User,
            guest: false,
            can_write: true,
            can_delete: true,
            can_share: true,
            quota_bytes: quota,
            source: PROVIDER,
            provisioned_by: Some(id),
            personal_space: personal.as_deref(),
            space_folders: st.space_folders.as_deref(),
        },
    )
    .await?;
    if let Some(user) = crate::auth::user_by_id(st, &mut *tx, user_id).await? {
        logs::record_activity(&mut *tx, &user, None, "user_create", &format!("{base} (created automatically by directory sign-in)")).await?;
    }
    sqlx::query("INSERT INTO user_identities (provider, subject, user_id, email, name, created_at) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(PROVIDER)
        .bind(id)
        .bind(user_id)
        .bind(email)
        .bind(name)
        .bind(now())
        .execute(&mut *tx)
        .await?;
    Ok((user_id, base))
}

async fn account_inner(st: &AppState, settings: &Settings, entry: &Entry, id: &str, typed: &str, groups: &HashSet<String>) -> Result<Signed, Refused> {
    let name = entry.text(&settings.attr_name).unwrap_or_default();
    let email = entry.text(&settings.attr_email).unwrap_or_default();
    // Where passwords are not allowed for this person, a directory password isn't either (Control panel › Single sign-on)
    let mode = crate::sso::mode(&st.db).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let linked: Option<(i64, String, bool, bool, UserRole)> = sqlx::query_as(
        "SELECT u.id, u.username, u.disabled, u.directory_disabled, u.role FROM user_identities i JOIN users u ON u.id = i.user_id WHERE i.provider = ? AND i.subject = ?",
    )
    .bind(PROVIDER)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let (user_id, username, created) = match linked {
        Some((user_id, username, disabled, by_directory, role)) => {
            if !mode.allows(role) {
                return Err(Refused::Wrong("password_off", Some(user_id)));
            }
            if disabled {
                // Turned off by the directory and the person is back: on again. One an administrator turned off stays off.
                if !by_directory {
                    return Err(Refused::Wrong("disabled", Some(user_id)));
                }
                sqlx::query("UPDATE users SET disabled = 0, directory_disabled = 0 WHERE id = ?").bind(user_id).execute(&mut *tx).await?;
            }
            (user_id, username, false)
        }
        None => {
            let (user_id, username) = new_account(st, &mut tx, settings, &Who { entry, id, typed, name: &name, email: &email }, mode).await?;
            (user_id, username, true)
        }
    };
    sync::profile(&mut tx, user_id, &name, &email).await?;
    sync::follow_groups(&mut tx, settings, user_id, groups).await?;
    sqlx::query("UPDATE user_identities SET last_login_at = ?, email = ?, name = ? WHERE provider = ? AND subject = ?")
        .bind(now())
        .bind(&email)
        .bind(&name)
        .bind(PROVIDER)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    if created {
        crate::folders::spaces_changed(st);
        tracing::info!("Automatically created account {username} via directory sign-in");
    }
    Ok(Signed { user_id, username })
}
