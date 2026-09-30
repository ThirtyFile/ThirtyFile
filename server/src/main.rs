mod admin;
mod app;
mod archive;
mod auth;
mod backups;
mod beneath;
mod dav;
mod db;
mod downloads;
mod drives;
mod error;
mod files;
mod folders;
mod fsops;
mod location_tools;
mod locations;
mod logs;
mod mail;
mod moves;
mod notify;
mod branding;
mod check;
mod cli;
mod ftp;
mod jobs;
mod sftp;
mod sso;
mod nodes;
mod paths;
mod personal;
#[cfg(unix)]
mod privileges;
mod replicas;
mod reset;
mod secrets;
mod sessions;
mod shares;
mod space_folders;
mod state;
mod storage;
#[cfg(test)]
mod testutil;
mod thumbnails;
mod tokens;
mod tree;
mod twofactor;
mod upload;
mod usage;
mod util;
#[cfg(target_os = "linux")]
mod watch;
mod versions;
mod web;
mod zip;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::{
    app::{health::health_probe, startup::run},
    cli::{Command, Config},
};

/// The version: the release number the image was built for (THIRTYFILE_VERSION at build time), else "dev" (Cargo.toml
/// doesn't carry the release number)
pub const VERSION: &str = match option_env!("THIRTYFILE_VERSION") {
    Some(v) => v,
    None => "dev",
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
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
        std::process::exit(if runtime()?.block_on(health_probe(&cfg.addr)) { 0 } else { 1 });
    }

    // The built-in storage location's folder
    let storage = cfg.storage.clone().unwrap_or_else(|| cfg.data.join("blobs"));

    // Data this version can't use (from 0.3 or older, say) stops the start before anything is written into the data
    // or storage folder
    let checked = tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(db::check_existing(&cfg.data.join("drive.db")));
    if let Err(message) = checked {
        return Err(message.into());
    }

    // Before the runtime starts its threads, so that all of them run as the new user. A storage folder that isn't
    // there isn't made here: whether it may be made is decided once the database is open (`storage::prepare_builtin`).
    #[cfg(unix)]
    if let Some(user) = &cfg.run_as {
        let folders: Vec<&std::path::Path> = [cfg.data.as_path(), storage.as_path()].into_iter().filter(|f| *f == cfg.data.as_path() || f.exists()).collect();
        privileges::drop_to(user, &folders)?;
    }

    runtime()?.block_on(run(cfg, storage))
}
