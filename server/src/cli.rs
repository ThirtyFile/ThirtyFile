//! The command line: the settings (flags and THIRTYFILE_* variables) and the subcommands that work on the data folder
//! instead of starting the server.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use crate::{VERSION, auth, check, db, locations, secrets};

#[derive(Parser)]
#[command(name = "thirtyfile", version = VERSION, about = "ThirtyFile — lightweight cloud file management system")]
pub struct Config {
    /// Listen address
    #[arg(long, env = "THIRTYFILE_ADDR", default_value = "0.0.0.0:8080")]
    pub addr: String,
    /// Data directory (database, files, thumbnails)
    #[arg(long, env = "THIRTYFILE_DATA", default_value = "./data")]
    pub data: PathBuf,
    /// Folder for the file contents of the built-in storage location (default: blobs in the data directory)
    #[arg(long, env = "THIRTYFILE_STORAGE")]
    pub storage: Option<PathBuf>,
    /// Administrator password on first startup; if not set, a random one is generated and printed to the log
    #[arg(long, env = "THIRTYFILE_ADMIN_PASSWORD", hide_env_values = true)]
    pub admin_password: Option<String>,
    /// A file holding the administrator password for the first startup (for Docker secrets)
    #[arg(long, env = "THIRTYFILE_ADMIN_PASSWORD_FILE")]
    pub admin_password_file: Option<PathBuf>,
    /// Key that encrypts the passwords and keys saved in the database (64 hex characters); default: secret.key in the
    /// data folder, created on first start
    #[arg(long, env = "THIRTYFILE_SECRET_KEY", hide_env_values = true)]
    pub secret_key: Option<String>,
    /// A file holding that key instead (for example a Docker secret)
    #[arg(long, env = "THIRTYFILE_SECRET_KEY_FILE")]
    pub secret_key_file: Option<PathBuf>,
    /// Enable when serving over HTTPS; cookies get the Secure attribute (true/false, also 1/0, yes/no, on/off)
    #[arg(long, env = "THIRTYFILE_SECURE_COOKIE", default_value = "false", value_parser = clap::builder::BoolishValueParser::new(), action = clap::ArgAction::Set)]
    pub secure_cookie: bool,
    /// Enable when behind a reverse proxy (nginx, Caddy…): take the user's real IP from X-Forwarded-For (for sign-in rate
    /// limiting). `true` trusts proxies on private and loopback addresses; or list the proxies' addresses or networks
    #[arg(long, env = "THIRTYFILE_TRUST_PROXY", default_value = "false", value_parser = auth::TrustProxy::parse)]
    pub trust_proxy: auth::TrustProxy,
    /// Days to keep items in the trash, 0 = until emptied
    #[arg(long, env = "THIRTYFILE_TRASH_DAYS", default_value_t = 30, value_parser = clap::value_parser!(i64).range(0..=36500))]
    pub trash_days: i64,
    /// Upload size limit per file (MB), 0 = unlimited
    #[arg(long, env = "THIRTYFILE_MAX_UPLOAD_MB", default_value_t = 0)]
    pub max_upload_mb: u64,
    /// SQLite page cache per database connection (MB); up to 8 connections are open
    #[arg(long, env = "THIRTYFILE_DB_CACHE_MB", default_value_t = 16, value_parser = clap::value_parser!(u32).range(1..=1024))]
    pub db_cache_mb: u32,
    /// Thumbnails made at the same time (default: 1 with less than 2 GB of memory, else 2)
    #[arg(long, env = "THIRTYFILE_THUMBNAIL_JOBS", value_parser = clap::value_parser!(u32).range(1..=16))]
    pub thumbnail_jobs: Option<u32>,
    /// When started as root: give the data directory to this user (`uid` or `uid:gid`) and run as that user (set in the Docker image)
    #[arg(long, env = "THIRTYFILE_RUN_AS")]
    #[cfg_attr(not(unix), allow(dead_code))]
    pub run_as: Option<String>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Reset a user's password (for when the administrator password is forgotten). Without the password, it is read
    /// from the input, so it doesn't end up in the shell history or the process list
    ResetPassword { username: String, password: Option<String> },
    /// Turn a user's two-factor sign-in off (for when the phone and the recovery codes are lost)
    ResetTwoFactor { username: String },
    /// Check whether the running service is healthy (for Docker HEALTHCHECK): exit code 0 when healthy
    Health,
    /// Write a consistent copy of the database to a new file, also while ThirtyFile is running
    /// (e.g. `docker exec thirtyfile thirtyfile backup /data/backups/drive.db`)
    Backup { file: PathBuf },
    /// Encrypt the saved passwords and keys with a new key. Stop ThirtyFile first. With a key file, the file is
    /// replaced; with THIRTYFILE_SECRET_KEY, give the new key in THIRTYFILE_NEW_SECRET_KEY and set it afterwards
    RotateSecretKey,
    /// Compare the stored file contents with the database: missing, changed or unknown files, for every storage
    /// location. Unknown files are only listed, never deleted
    Check {
        /// Also read every file back and compare its hash (slow: reads everything)
        #[arg(long)]
        verify: bool,
    },
}

/// Runs the subcommand that works on the database, if one was given: `true` when it ran and the program is done,
/// `false` when the server is to start
pub async fn run(cfg: &Config, db: &sqlx::SqlitePool, storage: &Path, key_source: &secrets::KeySource) -> Result<bool, Box<dyn std::error::Error>> {
    if let Some(Command::Check { verify }) = &cfg.command {
        let storages = locations::load_all(db, storage).await?;
        let reports = check::run(db, &storages, *verify, |id, n| eprintln!("Checking storage location {id} ({n} file(s))…")).await?;
        let problems = check::print(&reports);
        std::process::exit(if problems == 0 { 0 } else { 1 });
    }

    if let Some(Command::RotateSecretKey) = &cfg.command {
        return rotate_secret_key(db, key_source).await.map(|()| true);
    }

    if let Some(Command::Backup { file }) = &cfg.command {
        if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        db::backup_to(db, file).await?;
        println!("Database saved to {}", file.display());
        return Ok(true);
    }

    if let Some(Command::ResetTwoFactor { username }) = &cfg.command {
        let res = sqlx::query("UPDATE users SET totp_secret = NULL, totp_last_step = 0 WHERE username = ?").bind(username).execute(db).await?;
        if res.rows_affected() == 0 {
            return Err(format!("User not found: {username}").into());
        }
        sqlx::query("DELETE FROM recovery_codes WHERE user_id = (SELECT id FROM users WHERE username = ?)").bind(username).execute(db).await?;
        println!("Two-factor sign-in is turned off for this account");
        return Ok(true);
    }
    if let Some(Command::ResetPassword { username, password }) = &cfg.command {
        let password = match password {
            Some(p) => p.clone(),
            None => {
                eprintln!("Type the new password and press Enter:");
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                line.trim_end_matches(['\r', '\n']).to_string()
            }
        };
        let password = &password;
        auth::validate_password(password, auth::MIN_PASSWORD).map_err(|e| e.message)?;
        let hash = auth::hash_password(password.clone()).await.map_err(|e| e.message)?;
        let res = sqlx::query("UPDATE users SET password_hash = ?, disabled = 0 WHERE username = ?")
            .bind(hash)
            .bind(username)
            .execute(db)
            .await?;
        if res.rows_affected() == 0 {
            return Err(format!("User not found: {username}").into());
        }
        sqlx::query("UPDATE users SET must_change_password = 1 WHERE username = ?").bind(username).execute(db).await?;
        for table in ["sessions", "app_passwords"] {
            sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table} WHERE user_id = (SELECT id FROM users WHERE username = ?)")))
                .bind(username)
                .execute(db)
                .await?;
        }
        println!("Password reset");
        return Ok(true);
    }
    Ok(false)
}

/// `thirtyfile rotate-secret-key`: re-encrypts the saved secrets with a new key. The new key file is written first
/// (as `<file>.new`) and moved into place once the database is updated, so a stop halfway leaves both keys on disk.
async fn rotate_secret_key(db: &sqlx::SqlitePool, source: &secrets::KeySource) -> Result<(), Box<dyn std::error::Error>> {
    let given = std::env::var("THIRTYFILE_NEW_SECRET_KEY").ok().filter(|k| !k.trim().is_empty());
    let new_key = match (&given, source) {
        (Some(k), _) => secrets::KeySource::Env(k.clone()).load()?,
        (None, secrets::KeySource::Env(_)) => {
            return Err("The key is set with THIRTYFILE_SECRET_KEY: put the new key (64 hex characters, e.g. from `openssl rand -hex 32`) in THIRTYFILE_NEW_SECRET_KEY, run this again, then set THIRTYFILE_SECRET_KEY to it".into());
        }
        (None, secrets::KeySource::File(_)) => rand::random(),
    };
    let staged = match source {
        secrets::KeySource::File(path) => {
            let staged = path.with_extension("key.new");
            secrets::write_key_file(&staged, &new_key)?;
            Some((staged, path.clone()))
        }
        secrets::KeySource::Env(_) => None,
    };
    db::reseal_secrets(db, &new_key).await?;
    if let Some((staged, path)) = staged {
        std::fs::rename(&staged, &path)?;
        println!("The saved passwords and keys are encrypted with a new key, saved in {}. Back it up again.", path.display());
    } else {
        println!("The saved passwords and keys are encrypted with the new key. Now set THIRTYFILE_SECRET_KEY to it before starting ThirtyFile.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_accept_common_ways_of_writing_true_and_refuse_impossible_values() {
        let parse = |args: &[&str]| Config::try_parse_from(std::iter::once("thirtyfile").chain(args.iter().copied()));
        for yes in ["true", "TRUE", "1", "yes", "on"] {
            assert!(parse(&["--secure-cookie", yes]).unwrap().secure_cookie, "{yes}");
        }
        for no in ["false", "0", "no", "off"] {
            assert!(!parse(&["--secure-cookie", no]).unwrap().secure_cookie, "{no}");
        }
        assert!(parse(&["--trash-days=-5"]).is_err(), "a negative number of days would turn off emptying the trash");
        assert_eq!(parse(&["--trash-days", "0"]).unwrap().trash_days, 0);
    }

    #[test]
    fn help_doesnt_show_the_administrator_password() {
        use clap::CommandFactory;
        let cmd = Config::command();
        let arg = cmd.get_arguments().find(|a| a.get_id() == "admin_password").unwrap();
        assert!(arg.is_hide_env_values_set());
    }
}
