//! ThirtyFile's server. The program (main.rs) only calls `cli::main`; tests in `tests/` start a server with
//! `app::startup::start`. The other modules stay private, so the compiler still reports code nothing uses.

mod admin;
pub mod app;
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
pub mod cli;
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

/// The version: the release number the image was built for (THIRTYFILE_VERSION at build time), else "dev" (Cargo.toml
/// doesn't carry the release number)
pub const VERSION: &str = match option_env!("THIRTYFILE_VERSION") {
    Some(v) => v,
    None => "dev",
};
