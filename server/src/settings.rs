//! The system settings and the server's own secrets kept in the database, and the first start: the first
//! administrator and the company space

use std::path::Path;

use sqlx::SqlitePool;

use crate::{
    admin::{NewUser, create_user},
    auth::hash_password,
    db::{add_grant, begin_write, create_drive, get_setting, set_setting},
    error::AppResult,
    state::SystemSettings,
    util::random_token,
};

/// Gets (or generates on first startup) the server secret used for signing, stored encrypted (secrets.rs)
pub async fn load_secret(db: &SqlitePool) -> Result<Vec<u8>, sqlx::Error> {
    if let Some((v,)) = sqlx::query_as::<_, (String,)>("SELECT value FROM settings WHERE key = 'secret'").fetch_optional(db).await? {
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

/// Loads system settings (the company space is created first, by `create_company_space`)
pub async fn load_system_settings(db: &SqlitePool) -> Result<SystemSettings, sqlx::Error> {
    let (shared_root_id, disabled): (String, bool) = sqlx::query_as("SELECT root_id, disabled FROM drives WHERE kind = 'company' LIMIT 1").fetch_one(db).await?;
    let allow_user_drives = get_setting(db, "allow_user_drives").await?.as_deref() == Some("1");
    let default_user_quota = get_setting(db, "default_user_quota").await?.and_then(|v| v.parse().ok()).unwrap_or(0).max(0);
    let personal_spaces = get_setting(db, "personal_spaces").await?.as_deref() != Some("0");
    let personal_location = get_setting(db, "personal_location").await?.unwrap_or_default();
    let public_url = get_setting(db, "public_url").await?.unwrap_or_default();
    let default_lang = get_setting(db, "default_lang").await?.filter(|v| crate::admin::LANGS.contains(&v.as_str())).unwrap_or_else(|| "auto".into());
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
    let version_keep =
        get_setting(db, "version_keep").await?.and_then(|v| v.parse().ok()).unwrap_or(crate::versions::DEFAULT_KEEP).clamp(0, crate::versions::MAX_KEEP);
    let version_days =
        get_setting(db, "version_days").await?.and_then(|v| v.parse().ok()).unwrap_or(crate::versions::DEFAULT_DAYS).clamp(0, crate::versions::MAX_DAYS);
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
    tx.commit().await?;
    Ok(())
}

/// The password the guides' examples show for the first administrator. Whoever read the same guide knows it, so it
/// counts as not set: the account gets a random password, written to the log.
pub const EXAMPLE_ADMIN_PASSWORD: &str = "choose-a-password";

/// Creates the default administrator when there are no users. `space_folders`: see `NewUser`.
pub async fn bootstrap_admin(db: &SqlitePool, password: Option<&str>, space_folders: Option<&Path>) -> AppResult<()> {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users").fetch_one(db).await?;
    if count > 0 {
        return Ok(());
    }
    // Any password is accepted here: the administrator is asked to change it after signing in.
    // An empty one counts as not set, so `THIRTYFILE_ADMIN_PASSWORD=` in a compose file gets a random password.
    if password == Some(EXAMPLE_ADMIN_PASSWORD) {
        tracing::warn!("THIRTYFILE_ADMIN_PASSWORD is the example from the guide, which anyone can read: a random password is used instead");
    }
    let (password, generated) = match password.filter(|p| !p.is_empty() && *p != EXAMPLE_ADMIN_PASSWORD) {
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

    async fn admin_hash(password: Option<&str>) -> String {
        let dir = std::env::temp_dir().join(format!("thirtyfile-test-{}", crate::util::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::connect(&dir.join("drive.db"), 16).await.unwrap();
        bootstrap_admin(&db, password, None).await.unwrap();
        let (hash,): (String,) = sqlx::query_as("SELECT password_hash FROM users WHERE username = 'admin'").fetch_one(&db).await.unwrap();
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
        hash
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
            s.into_iter().map(|(v,)| v).collect::<Vec<_>>().join(
                "
",
            )
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
    async fn the_example_password_of_the_guides_counts_as_not_set() {
        let hash = admin_hash(Some(EXAMPLE_ADMIN_PASSWORD)).await;
        assert!(!crate::auth::verify_password(EXAMPLE_ADMIN_PASSWORD.into(), hash).await.unwrap());
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
