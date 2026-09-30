//! The command line: the settings (flags and THIRTYFILE_* variables) and the subcommands that work on the data folder
//! instead of starting the server.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[cfg(unix)]
use crate::privileges;
use crate::{
    VERSION,
    app::{
        health::health_probe,
        startup::{self, Settings},
    },
    auth, check, db, locations, secrets, twofactor,
};

#[derive(Parser)]
#[command(name = "thirtyfile", version = VERSION, about = "ThirtyFile — lightweight cloud file management system")]
pub struct Config {
    #[command(flatten)]
    pub server: Settings,
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

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::IsTerminal;
    // Colours only on a terminal (not in `docker logs` or a log collector); THIRTYFILE_LOG_FORMAT=json for JSON lines
    let logs = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "thirtyfile=info,tower_http=warn".into()))
        .with_ansi(std::io::stdout().is_terminal());
    if std::env::var("THIRTYFILE_LOG_FORMAT").is_ok_and(|f| f.eq_ignore_ascii_case("json")) {
        logs.json().init();
    } else {
        logs.init();
    }
    let runtime = || tokio::runtime::Builder::new_multi_thread().enable_all().build();
    let cfg = match Config::try_parse() {
        Ok(cfg) => cfg,
        // The health check only needs the address: a mistyped setting (which stops the server with a clear message)
        // mustn't also make Docker report the check itself as broken
        Err(e) if std::env::args().nth(1).as_deref() == Some("health") && e.kind() != clap::error::ErrorKind::DisplayHelp => {
            let addr = std::env::var("THIRTYFILE_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
            std::process::exit(if runtime()?.block_on(health_probe(&addr)) { 0 } else { 1 });
        }
        Err(e) => e.exit(),
    };

    // The health check only connects to the running service and doesn't touch the data directory
    if let Some(Command::Health) = &cfg.command {
        std::process::exit(if runtime()?.block_on(health_probe(&cfg.server.addr)) { 0 } else { 1 });
    }

    // The built-in storage location's folder
    let storage = cfg.server.storage.clone().unwrap_or_else(|| cfg.server.data.join("blobs"));

    // Data this version can't use (from 0.3 or older, say) stops the start before anything is written into the data
    // or storage folder
    let checked = tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(db::check_existing(&cfg.server.data.join("drive.db")));
    if let Err(message) = checked {
        return Err(message.into());
    }

    // Before the runtime starts its threads, so that all of them run as the new user. A storage folder that isn't
    // there isn't made here: whether it may be made is decided once the database is open (`storage::prepare_builtin`).
    #[cfg(unix)]
    if let Some(user) = &cfg.run_as {
        let folders: Vec<&std::path::Path> = [cfg.server.data.as_path(), storage.as_path()].into_iter().filter(|f| *f == cfg.server.data.as_path() || f.exists()).collect();
        privileges::drop_to(user, &folders)?;
    }

    runtime()?.block_on(start(cfg, storage))
}

/// Runs the subcommand given, or starts the server and serves until a stop signal
async fn start(cfg: Config, storage: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let (db, key_source) = startup::open(&cfg.server).await?;
    if run(&cfg, &db, &storage, &key_source).await? {
        return Ok(());
    }
    startup::start(cfg.server, storage, db).await?.serve().await
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
        let id = user_id(db, username).await?;
        let mut tx = db::begin_write(db).await?;
        twofactor::turn_off_in(&mut tx, id).await.map_err(|e| e.message)?;
        tx.commit().await?;
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
        let id = user_id(db, username).await?;
        let mut tx = db::begin_write(db).await?;
        sqlx::query("UPDATE users SET password_hash = ?, disabled = 0, must_change_password = 1 WHERE id = ?").bind(hash).bind(id).execute(&mut *tx).await?;
        // Signed out everywhere, as when an administrator sets the password
        auth::sign_out_everywhere(&mut tx, id, None).await.map_err(|e| e.message)?;
        tx.commit().await?;
        println!("Password reset");
        return Ok(true);
    }
    Ok(false)
}

/// The id of the account with this username
async fn user_id(db: &sqlx::SqlitePool, username: &str) -> Result<i64, Box<dyn std::error::Error>> {
    let id: Option<i64> = sqlx::query_scalar("SELECT id FROM users WHERE username = ?").bind(username).fetch_optional(db).await?;
    id.ok_or_else(|| format!("User not found: {username}").into())
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
    crate::settings::reseal_secrets(db, &new_key).await?;
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
            assert!(parse(&["--secure-cookie", yes]).unwrap().server.secure_cookie, "{yes}");
        }
        for no in ["false", "0", "no", "off"] {
            assert!(!parse(&["--secure-cookie", no]).unwrap().server.secure_cookie, "{no}");
        }
        assert!(parse(&["--trash-days=-5"]).is_err(), "a negative number of days would turn off emptying the trash");
        assert_eq!(parse(&["--trash-days", "0"]).unwrap().server.trash_days, 0);
    }

    #[test]
    fn help_doesnt_show_the_administrator_password() {
        use clap::CommandFactory;
        let cmd = Config::command();
        let arg = cmd.get_arguments().find(|a| a.get_id() == "admin_password").unwrap();
        assert!(arg.is_hide_env_values_set());
    }

    #[tokio::test]
    async fn reset_two_factor_and_reset_password_sign_the_account_out() {
        let env = crate::testutil::env().await;
        let amy = env.user("amy", true).await;
        env.sign_in(&amy, "Test").await;
        sqlx::query("UPDATE users SET totp_secret = 'sealed' WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
        sqlx::query("INSERT INTO recovery_codes (user_id, code_hash, created_at) VALUES (?, 'hash', 0)").bind(amy.id).execute(&env.st.db).await.unwrap();
        let key = secrets::KeySource::Env(String::new());
        let run = |args: Vec<&str>| {
            let cfg = Config::try_parse_from(std::iter::once("thirtyfile").chain(args)).unwrap();
            let (db, storage, key) = (env.st.db.clone(), env.st.storage_dir.clone(), &key);
            async move { run(&cfg, &db, &storage, key).await.map_err(|e| e.to_string()) }
        };
        let db = env.st.db.clone();
        let count = |sql: &'static str| {
            let db = db.clone();
            async move { sqlx::query_scalar::<_, i64>(sql).bind(amy.id).fetch_one(&db).await.unwrap() }
        };

        assert_eq!(run(vec!["reset-two-factor", "amy"]).await, Ok(true));
        assert_eq!(count("SELECT COUNT(*) FROM users WHERE id = ? AND totp_secret IS NOT NULL").await, 0);
        assert_eq!(count("SELECT COUNT(*) FROM recovery_codes WHERE user_id = ?").await, 0);

        let new_password = format!("new-{}", crate::util::new_id());
        assert_eq!(count("SELECT COUNT(*) FROM sessions WHERE user_id = ?").await, 1);
        assert_eq!(run(vec!["reset-password", "amy", &new_password]).await, Ok(true));
        assert_eq!(count("SELECT COUNT(*) FROM sessions WHERE user_id = ?").await, 0);
        assert_eq!(count("SELECT must_change_password FROM users WHERE id = ?").await, 1);
        let (hash,): (String,) = sqlx::query_as("SELECT password_hash FROM users WHERE id = ?").bind(amy.id).fetch_one(&env.st.db).await.unwrap();
        assert!(auth::verify_password(new_password.clone(), hash).await.unwrap());

        assert_eq!(run(vec!["reset-two-factor", "nobody"]).await, Err("User not found: nobody".into()));
        assert_eq!(run(vec!["reset-password", "nobody", &new_password]).await, Err("User not found: nobody".into()));
        assert_eq!(run(vec![]).await, Ok(false), "no subcommand: the server starts");
    }
}
