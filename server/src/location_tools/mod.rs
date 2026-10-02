//! What administrators can do with a storage location besides its settings (Control panel › Storage locations):
//!
//! - a test step by step: connect, write and read back a small and a larger file (a multipart upload on S3), and
//!   delete both, each step with its time and the larger file's speed (`steps`)
//! - browsing what the location holds, read-only, page by page, with the space and file each piece of content
//!   belongs to, and downloading one item (`browse`)
//! - finding content nothing in ThirtyFile uses, and removing it after confirmation (`unused`)
//!
//! Everything a test writes is below `.thirtyfile-check/`. Files in other people's personal spaces stay private here
//! too: their space is named, never their files.

mod browse;
pub(crate) mod steps;
mod unused;

pub use browse::{browse, download};
pub use steps::test_steps;
#[cfg(test)]
pub use unused::scan as unused_scan;
pub use unused::{Job as UnusedSearch, find_unused, remove_unused, unused_status};

/// What the storage location tools keep in memory (a part of `AppState`)
#[derive(Default)]
pub struct Memory {
    /// Searches for unused content, by storage location (location_tools/unused.rs)
    pub unused_searches: std::sync::Mutex<std::collections::HashMap<String, UnusedSearch>>,
}

use std::path::PathBuf;

use crate::{
    error::{AppError, AppResult},
    locations::{self, BUILTIN},
    state::AppState,
};

/// Folder of the test files, in every kind of location
pub const CHECK_DIR: &str = ".thirtyfile-check";

/// A storage location as these tools need it
struct Location {
    id: String,
    name: String,
    kind: String,
    config: serde_json::Value,
}

impl Location {
    /// The folder on this server, for the built-in location and Local folder locations
    fn folder(&self, st: &AppState) -> Option<PathBuf> {
        if self.kind != "local" {
            return None;
        }
        let path = self.config["path"].as_str().map(str::trim).unwrap_or_default();
        let folder = if self.id == BUILTIN || path.is_empty() { st.storage_dir.clone() } else { PathBuf::from(path) };
        Some(std::path::absolute(&folder).unwrap_or(folder))
    }
}

async fn location(st: &AppState, id: &str) -> AppResult<Location> {
    let row: Option<(String, String, String)> =
        sqlx::query_as("SELECT name, kind, config FROM storage_locations WHERE id = ?").bind(id).fetch_optional(&st.db).await?;
    let (name, kind, raw) = row.ok_or_else(|| AppError::not_found("Storage location not found"))?;
    Ok(Location { id: id.to_string(), name, kind, config: locations::config_json(id, &raw) })
}
