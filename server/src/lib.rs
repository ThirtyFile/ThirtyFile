//! ThirtyFile's server. The program (main.rs) only calls `cli::main`; tests in `tests/` start a server with
//! `app::startup`. The other modules stay private, so the compiler still reports code nothing uses.

// Functions stay short enough to read (split along their steps, with a struct for what they share), and every
// `allow` says why. Tests may be longer.
#![cfg_attr(not(test), warn(clippy::too_many_lines))]
#![warn(clippy::allow_attributes_without_reason)]

mod admin;
pub mod app;
mod archive;
mod auth;
mod backups;
mod beneath;
mod branding;
mod check;
pub mod cli;
mod content;
mod dav;
mod db;
mod downloads;
mod drives;
mod error;
mod files;
mod folders;
mod fsops;
mod hashing;
mod history;
mod jobs;
mod location_tools;
mod locations;
mod logs;
mod mail;
mod moves;
mod nodes;
mod notify;
mod paths;
mod personal;
#[cfg(unix)]
mod privileges;
mod redact;
mod replicas;
mod reset;
mod secrets;
mod sessions;
mod settings;
mod shares;
mod signin;
mod space_folders;
mod sso;
mod state;
mod storage;
#[cfg(test)]
mod testutil;
mod thumbnails;
mod tls;
mod tokens;
mod tree;
mod twofactor;
mod upload;
mod usage;
mod users;
mod util;
mod versions;
#[cfg(target_os = "linux")]
mod watch;
mod web;
mod zip;

/// The version: the release number the image was built for (THIRTYFILE_VERSION at build time), else "dev" (Cargo.toml
/// doesn't carry the release number)
pub const VERSION: &str = match option_env!("THIRTYFILE_VERSION") {
    Some(v) => v,
    None => "dev",
};
