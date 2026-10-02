//! Finding, creating or linking the account an identity signs in to

use super::*;

/// After a sign-in: remember the provider's current email and name, and keep the user's display name in step with the
/// provider's name. A display name set by an administrator (different from the name the provider reported last time) is kept.
pub(super) async fn sync_profile(st: &AppState, user_id: i64, provider: &str, ident: &Identity) -> AppResult<()> {
    let previous: Option<(String,)> =
        sqlx::query_as("SELECT name FROM user_identities WHERE provider = ? AND subject = ?").bind(provider).bind(&ident.subject).fetch_optional(&st.db).await?;
    let (current,): (String,) = sqlx::query_as("SELECT display_name FROM users WHERE id = ?").bind(user_id).fetch_one(&st.db).await?;
    let name = crate::users::validate_display_name(&ident.name).unwrap_or("");
    // Compare with the previous name as it would have been stored (trimmed), so surrounding spaces don't break the follow-up
    let previous = previous.map(|(p,)| crate::users::validate_display_name(&p).unwrap_or("").to_string());
    let follow = !name.is_empty() && name != current && (current.is_empty() || previous.as_deref() == Some(current.as_str()));
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    // A later sign-in without a verified email (e.g. a multi-tenant Microsoft app) keeps the email recorded earlier:
    // only a verified one is kept, as it can match an account by its email (`MATCHING_USER`)
    sqlx::query("UPDATE user_identities SET last_login_at = ?, email = COALESCE(NULLIF(?, ''), email), name = ? WHERE provider = ? AND subject = ?")
        .bind(now())
        .bind(ident.verified_email())
        .bind(&ident.name)
        .bind(provider)
        .bind(&ident.subject)
        .execute(&mut *tx)
        .await?;
    if follow {
        sqlx::query("UPDATE users SET display_name = ? WHERE id = ?").bind(name).bind(user_id).execute(&mut *tx).await?;
    }
    // Where notification emails go, until the person enters an address themselves (notify.rs)
    if ident.email_verified && crate::mail::valid_address(&ident.email) {
        sqlx::query("UPDATE users SET email = ? WHERE id = ? AND email = ''").bind(&ident.email).bind(user_id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// The provider's own domain list wins; otherwise the global list; an empty list allows every domain
pub(super) fn domain_allowed(settings: &SsoSettings, cfg: &ProviderConfig, email: &str) -> bool {
    let list = if cfg.allowed_domains.is_empty() { &settings.allowed_domains } else { &cfg.allowed_domains };
    if list.is_empty() {
        return true;
    }
    let domain = email.rsplit_once('@').map(|(_, d)| d.to_ascii_lowercase()).unwrap_or_default();
    list.contains(&domain)
}

/// The account a verified email signs in to when no account is linked to the external identity yet: the one whose
/// username is that email, if an administrator created it, or if single sign-on created it for that very email.
/// Accounts created by single sign-on get a username changed from the email (`amy+files@…` becomes `amy_files@…`),
/// so a username alone doesn't say whose account it is.
pub(super) const MATCHING_USER: &str = "u.username = ?1
     AND (u.source = 'password' OR EXISTS (SELECT 1 FROM user_identities i WHERE i.user_id = u.id AND lower(i.email) = lower(?1)))";

/// Whether an existing account may be linked by its email alone (`MATCHING_USER`): not when it has a password or
/// two-factor sign-in, which a linked account signs in without. Its owner links it from the account menu instead,
/// which asks for both (`start_link`).
const LINKABLE_BY_EMAIL: &str = "SELECT password_hash = ? AND totp_secret IS NULL FROM users WHERE id = ?";

/// The notice that tells a person an external account was linked to theirs, in the app and by email
fn linked_notice(settings: &SsoSettings, provider: &str, ident: &Identity, ip: &str) -> crate::notify::Notice {
    let shown = if provider == "oidc" { settings.provider(provider).map(shown_name).unwrap_or_default() } else { label(provider).to_string() };
    let account = if ident.email.is_empty() { ident.name.as_str() } else { ident.email.as_str() };
    crate::notify::Notice { kind: "sign_in_method", node_id: None, data: json!({ "provider": provider, "label": shown, "account": account, "ip": ip }) }
}

/// Finds (or creates) the user to sign in, per the provider's policy: already linked → existing user whose username is the email → create automatically.
/// Returns (user id, username, whether the account was just created)
pub(super) async fn resolve_user(st: &AppState, provider: &str, ident: &Identity, ip: &str) -> AppResult<(i64, String, bool)> {
    let settings = st.part::<Memory>().settings.read().unwrap().clone();
    let cfg = settings.provider(provider).cloned().unwrap_or_default();
    let linked: Option<(i64, String, bool)> =
        sqlx::query_as("SELECT u.id, u.username, u.disabled FROM user_identities i JOIN users u ON u.id = i.user_id WHERE i.provider = ? AND i.subject = ?")
            .bind(provider)
            .bind(&ident.subject)
            .fetch_optional(&st.db)
            .await?;
    if let Some((id, username, disabled)) = linked {
        if disabled {
            return Err(AppError::forbidden("This account is disabled. Contact your administrator."));
        }
        return Ok((id, username, false));
    }
    if cfg.provisioning == Provisioning::Off {
        return Err(AppError::forbidden(
            "This external account isn't linked yet. Sign in with your username and password, then link it in \"My account › Sign-in methods\".",
        ));
    }
    if !ident.email_verified {
        return Err(AppError::forbidden(if provider == "microsoft" && !ms_tenant_is_specific(&settings.microsoft.tenant) {
            "This external account isn't linked yet. Sign in with your username and password, then link it in \"My account › Sign-in methods\", or ask your administrator to enter the Microsoft tenant ID in the single sign-on settings."
        } else {
            "This external account has no verified email, so it can't be matched to an account automatically. Sign in with your username and password, then link it in \"My account › Sign-in methods\"."
        }));
    }
    if !domain_allowed(&settings, &cfg, &ident.email) {
        return Err(AppError::forbidden(format!("The domain of {} isn't allowed to sign in to this site", ident.email)));
    }
    // An existing user whose username is this email: link automatically (see `MATCHING_USER`)
    let existing: Option<(i64, String, bool)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT id, username, disabled FROM users u WHERE {MATCHING_USER}")))
        .bind(&ident.email)
        .fetch_optional(&st.db)
        .await?;
    let (id, username, created) = match existing {
        Some((_, _, true)) => return Err(AppError::forbidden("This account is disabled. Contact your administrator.")),
        Some((id, username, false)) => (id, username, false),
        None if cfg.provisioning == Provisioning::Create => create_sso_user(st, provider, &cfg, ident).await?,
        None => {
            return Err(AppError::forbidden(format!(
                "{} doesn't have an account on this site. Ask your administrator to create one, then link this external account in \"My account › Sign-in methods\".",
                ident.email
            )));
        }
    };
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    // Checked under the lock: a sign-in of the same person at the same time may have linked it already
    let already: Option<(i64,)> = sqlx::query_as("SELECT user_id FROM user_identities WHERE provider = ? AND subject = ?")
        .bind(provider)
        .bind(&ident.subject)
        .fetch_optional(&mut *tx)
        .await?;
    match already {
        Some((linked,)) if linked == id => return Ok((id, username, created)),
        Some(_) => return Err(AppError::conflict(format!("This {} account is already linked to another user", label(provider)))),
        None => {}
    }
    if !created {
        let (linkable,): (bool,) = sqlx::query_as(LINKABLE_BY_EMAIL).bind(NO_PASSWORD).bind(id).fetch_one(&mut *tx).await?;
        if !linkable {
            return Err(AppError::forbidden(
                "This account has a password or two-factor sign-in, so it isn't linked automatically. Sign in with your username and password, then link this external account in \"My account › Sign-in methods\".",
            ));
        }
    }
    sqlx::query("INSERT INTO user_identities (provider, subject, user_id, email, name, created_at) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(provider)
        .bind(&ident.subject)
        .bind(id)
        .bind(&ident.email)
        .bind(&ident.name)
        .bind(now())
        .execute(&mut *tx)
        .await?;
    // An existing account was linked: its owner is told, as when they link one themselves
    let emails = if created { Vec::new() } else { crate::notify::add(&mut tx, &[id], &linked_notice(&settings, provider, ident, ip)).await? };
    tx.commit().await?;
    crate::notify::send_later(st, emails);
    Ok((id, username, created))
}

/// Creates an account automatically: the username is the email (or the part before @ when too long) and no password (third-party sign-in only, until an administrator sets one)
/// Username for automatically created accounts: based on the email, replacing characters usernames don't allow (e.g. `+`), with the same rules as accounts created by administrators
pub(super) fn sso_username(email: &str) -> String {
    let clean = |s: &str| -> String { s.chars().map(|c| if c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '@') { c } else { '_' }).collect() };
    let full = clean(email);
    let base: String = if full.chars().count() <= 32 { full } else { clean(email.split('@').next().unwrap_or("")).chars().take(28).collect() };
    if base.chars().count() < 2 { format!("user{base}") } else { base }
}

/// Returns (user id, username, whether it was created): a sign-in of the same person at the same time, or an account
/// created for the email meanwhile, is found instead
pub(super) async fn create_sso_user(st: &AppState, provider: &str, cfg: &ProviderConfig, ident: &Identity) -> AppResult<(i64, String, bool)> {
    let base = sso_username(&ident.email);
    // Settings for the new account: the rule for the email's domain, otherwise the provider's defaults
    let rule = st.part::<Memory>().settings.read().unwrap().domain_rule(&ident.email).cloned();
    let (perms, quota_setting, groups) = match &rule {
        Some(r) => ((r.can_write, r.can_delete, r.can_share), r.quota_bytes, r.groups.clone()),
        None => ((cfg.defaults.can_write, cfg.defaults.can_delete, cfg.defaults.can_share), cfg.defaults.quota_bytes, cfg.groups.clone()),
    };
    let quota = quota_setting.unwrap_or_else(|| st.system.read().unwrap().default_user_quota).max(0);
    // Sign-in is only possible through the provider: no password can match this value until an administrator sets one
    let password_hash = NO_PASSWORD.to_string();
    let (create, location) = rule.as_ref().map_or((None, None), |r| (r.personal_space, r.personal_location.as_deref()));
    crate::personal::check_ahead(st, create, location).await;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    // Two sign-ins of the same person at the same time: the second one finds what the first created (checked under the lock)
    let linked: Option<(i64, String)> =
        sqlx::query_as("SELECT u.id, u.username FROM user_identities i JOIN users u ON u.id = i.user_id WHERE i.provider = ? AND i.subject = ?")
            .bind(provider)
            .bind(&ident.subject)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some((id, username)) = linked {
        return Ok((id, username, false));
    }
    let existing: Option<(i64, String)> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT id, username FROM users u WHERE {MATCHING_USER}"))).bind(&ident.email).fetch_optional(&mut *tx).await?;
    if let Some((id, username)) = existing {
        return Ok((id, username, false));
    }
    // A misconfigured tenant or domain list must not fill the user list: at most MAX_CREATED_PER_HOUR new accounts per provider
    let (recent,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM users WHERE source = ? AND created_at > ?").bind(provider).bind(now() - 3600).fetch_one(&mut *tx).await?;
    if recent >= MAX_CREATED_PER_HOUR {
        tracing::warn!("{} sign-in: account creation paused, {recent} accounts were created in the last hour", label(provider));
        return Err(AppError::forbidden("Too many accounts were created in the last hour. Try again later or ask your administrator to create your account."));
    }
    let mut username = base.clone();
    for i in 2..100 {
        let (taken,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users WHERE username = ?").bind(&username).fetch_one(&mut *tx).await?;
        if taken == 0 {
            break;
        }
        username = format!("{}-{i}", base.chars().take(28).collect::<String>());
    }
    crate::users::validate_username(&username)?;
    // "My files" as the domain rule says, else by the system setting; a location that was deleted since gives way to
    // the setting's
    let personal = crate::personal::choose(st, &mut tx, create, location, false).await?;
    let id = create_user(
        &mut tx,
        NewUser {
            username: &username,
            password_hash: &password_hash,
            role: crate::auth::UserRole::User,
            can_write: perms.0,
            can_delete: perms.1,
            can_share: perms.2,
            quota_bytes: quota,
            source: provider,
            provisioned_by: Some(&ident.subject),
            personal_space: personal.as_deref(),
            space_folders: st.space_folders.as_deref(),
        },
    )
    .await?;
    if let Ok(name) = crate::users::validate_display_name(&ident.name)
        && !name.is_empty()
    {
        sqlx::query("UPDATE users SET display_name = ? WHERE id = ?").bind(name).bind(id).execute(&mut *tx).await?;
    }
    // Groups configured for the domain rule or the provider (ones deleted since are skipped)
    let mut joined = Vec::new();
    for g in &groups {
        let name: Option<(String,)> = sqlx::query_as("SELECT name FROM groups WHERE id = ?").bind(g).fetch_optional(&mut *tx).await?;
        if let Some((name,)) = name {
            sqlx::query("INSERT OR IGNORE INTO group_members (group_id, user_id) VALUES (?, ?)").bind(g).bind(id).execute(&mut *tx).await?;
            joined.push(name);
        }
    }
    if let Some(user) = crate::auth::user_by_id(st, &mut tx, id).await? {
        let detail = format!(
            "{username} (created automatically by {} sign-in{}{})",
            label(provider),
            rule.as_ref().map(|r| format!(", domain rule {}", r.domain)).unwrap_or_default(),
            if joined.is_empty() { String::new() } else { format!(", groups: {}", joined.join(", ")) }
        );
        logs::record_activity(&mut tx, &user, None, "user_create", &detail).await?;
    }
    tx.commit().await?;
    crate::folders::spaces_changed(st);
    tracing::info!("Automatically created account {username} via {} sign-in", label(provider));
    Ok((id, username, true))
}

pub(super) async fn link(st: &AppState, provider: &str, ident: &Identity, user_id: i64, ip: &str) -> AppResult<String> {
    // The allowed domains apply to linked accounts too: with a list, only a verified email in it
    let settings = st.part::<Memory>().settings.read().unwrap().clone();
    let cfg = settings.provider(provider).cloned().unwrap_or_default();
    let restricted = !settings.allowed_domains.is_empty() || !cfg.allowed_domains.is_empty();
    if restricted && !(ident.email_verified && domain_allowed(&settings, &cfg, &ident.email)) {
        return Err(AppError::forbidden("Only an account with a verified email address in a domain allowed on this site can be linked"));
    }
    let other: Option<(i64,)> =
        sqlx::query_as("SELECT user_id FROM user_identities WHERE provider = ? AND subject = ?").bind(provider).bind(&ident.subject).fetch_optional(&st.db).await?;
    if other.is_some_and(|(id,)| id != user_id) {
        return Err(AppError::conflict(format!("This {} account is already linked to another user", label(provider))));
    }
    let (username,): (String,) = sqlx::query_as("SELECT username FROM users WHERE id = ?").bind(user_id).fetch_one(&st.db).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    // One linked account per sign-in method: relinking replaces the previous one
    sqlx::query("DELETE FROM user_identities WHERE user_id = ? AND provider = ?").bind(user_id).bind(provider).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO user_identities (provider, subject, user_id, email, name, created_at) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(provider)
        .bind(&ident.subject)
        .bind(user_id)
        .bind(ident.verified_email())
        .bind(&ident.name)
        .bind(now())
        .execute(&mut *tx)
        .await
        .map_err(|e| match &e {
            // Linked by someone else between the check above and here
            sqlx::Error::Database(d) if d.is_unique_violation() => AppError::conflict(format!("This {} account is already linked to another user", label(provider))),
            _ => AppError::from(e),
        })?;
    // Told in the app and by email, so an account linked by someone else doesn't go unnoticed
    let emails = crate::notify::add(&mut tx, &[user_id], &linked_notice(&settings, provider, ident, ip)).await?;
    tx.commit().await?;
    crate::notify::send_later(st, emails);
    Ok(username)
}
