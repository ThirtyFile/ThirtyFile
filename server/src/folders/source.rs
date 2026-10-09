//! Checks of the folders a folder space or a Local folder location is to use: an existing folder, outside ThirtyFile's
//! own folders and the system's, and not one another space or location already uses (the same files would be in two
//! places, and deleting in one would change the other); and making a space a folder space.

use std::path::{Path, PathBuf};

use sqlx::SqliteConnection;

use super::mark_space;
use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

/// Checks the folder a new folder space is to show: an existing folder, given as an absolute path, that isn't
/// ThirtyFile's own data or storage (or inside them)
pub fn check_source(st: &AppState, path: &str) -> AppResult<String> {
    let path = path.trim();
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(AppError::bad_request("Enter the folder's full path, for example /mnt/nas/shared"));
    }
    let real = std::fs::canonicalize(p).map_err(|_| AppError::bad_request("The folder doesn't exist on the server"))?;
    if !real.is_dir() {
        return Err(AppError::bad_request("The folder doesn't exist on the server"));
    }
    for own in [&st.data_dir, &st.storage_dir] {
        if let Ok(own) = std::fs::canonicalize(own)
            && (real.starts_with(&own) || own.starts_with(&real))
        {
            return Err(AppError::bad_request("Choose a folder outside ThirtyFile's own data and storage folders"));
        }
    }
    if crate::util::system_folder(&real) {
        return Err(AppError::bad_request("Choose a folder outside the system's own folders"));
    }
    let real = real.to_string_lossy().into_owned();
    // Windows: show C:\folder rather than the \\?\C:\folder form canonicalize returns
    Ok(match real.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => rest.to_owned(),
        _ => real,
    })
}

/// The path with links resolved, as far as it exists (a folder that isn't made yet keeps the rest as given); on Windows
/// without the `\\?\` form, so paths compare
fn real_path(p: &str) -> PathBuf {
    let given = PathBuf::from(p);
    let mut rest = Vec::new();
    let mut at = given.as_path();
    let real = loop {
        if let Ok(r) = std::fs::canonicalize(at) {
            break r;
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                at = parent;
            }
            _ => return given,
        }
    };
    let real = match real.to_string_lossy().strip_prefix(r"\\?\") {
        Some(plain) if !plain.starts_with("UNC\\") => PathBuf::from(plain),
        _ => real,
    };
    rest.into_iter().rev().fold(real, |p, name| p.join(name))
}

fn overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Folders on the server ThirtyFile already uses, besides its own data folder: the storage folder, each Local folder
/// location's folder (with the spaces made in it), and each folder shown as a space from elsewhere. A location with the
/// id `except` (the one being edited) is left out.
async fn claimed(st: &AppState, except: Option<&str>) -> AppResult<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mut locations = vec![real_path(&st.storage_dir.to_string_lossy())];
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT id, config FROM storage_locations WHERE kind = 'local'").fetch_all(&st.db).await?;
    for (id, config) in rows {
        let path = serde_json::from_str::<serde_json::Value>(&config).ok().and_then(|c| c["path"].as_str().map(str::trim).map(str::to_string));
        if let Some(path) = path.filter(|p| !p.is_empty() && except != Some(id.as_str())) {
            locations.push(real_path(&path));
        }
    }
    let spaces: Vec<(String,)> = sqlx::query_as("SELECT source_path FROM drives WHERE mode = 'folder' AND source_path IS NOT NULL").fetch_all(&st.db).await?;
    let shown: Vec<PathBuf> = spaces.into_iter().map(|(p,)| real_path(&p)).filter(|p| !locations.iter().any(|l| p.starts_with(l))).collect();
    Ok((locations, shown))
}

/// A folder to show as a space must not contain, or be inside, one ThirtyFile already uses: the same files would be in
/// two spaces, and deleting in one would change the other
pub async fn check_new_source(st: &AppState, path: &str) -> AppResult<()> {
    let p = real_path(path);
    let (locations, shown) = claimed(st, None).await?;
    if locations.iter().chain(&shown).any(|c| overlap(&p, c)) {
        return Err(AppError::conflict("This folder contains, or is inside, a folder that another space or storage location uses"));
    }
    Ok(())
}

/// The folder of a Local folder location (`id` when editing one) must not be inside ThirtyFile's data folder, nor
/// contain or be inside a folder shown as a space or another location's folder
pub async fn check_location_folder(st: &AppState, id: Option<&str>, kind: &str, config: &serde_json::Value) -> AppResult<()> {
    let path = config["path"].as_str().map(str::trim).unwrap_or_default();
    if kind != "local" || path.is_empty() {
        return Ok(());
    }
    let p = real_path(path);
    if overlap(&p, &real_path(&st.data_dir.to_string_lossy())) {
        return Err(AppError::bad_request("Choose a folder outside ThirtyFile's data folder"));
    }
    if crate::util::system_folder(&p) {
        return Err(AppError::bad_request("Choose a folder outside the system's own folders"));
    }
    let (locations, shown) = claimed(st, id).await?;
    if locations.iter().chain(&shown).any(|c| overlap(&p, c)) {
        return Err(AppError::conflict("This folder contains, or is inside, a folder that another space or storage location uses"));
    }
    Ok(())
}

/// Makes a new space a folder space showing `source` and creates its index root (called when the space is created).
/// `location`: the storage location whose folder holds `source` (space_folders.rs), None for a folder an administrator
/// chose, which is on no location.
pub async fn set_up(conn: &mut SqliteConnection, drive_id: &str, root_id: &str, source: &str, location: Option<&str>) -> AppResult<()> {
    sqlx::query("UPDATE drives SET mode = 'folder', source_path = ?, location_id = ? WHERE id = ?")
        .bind(source)
        .bind(location)
        .bind(drive_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query("UPDATE nodes SET fs_path = '' WHERE id = ?").bind(root_id).execute(&mut *conn).await?;
    // Before anything is written there (a folder that can't take it, read-only say, gets it from a scan if ever)
    if let Err(e) = mark_space(Path::new(source), drive_id) {
        tracing::debug!("Couldn't write the marker file in {source}: {e}");
    }
    Ok(())
}

#[cfg(test)]
mod overlap_tests {
    use super::*;
    use crate::{testutil, util::new_id};

    #[tokio::test]
    async fn folders_already_in_use_cant_be_shown_again_or_used_for_a_location() {
        let env = testutil::env().await;
        let space = env.folder_space("NAS").await;
        std::fs::create_dir_all(space.dir.join("Inside")).unwrap();
        let inside = space.dir.join("Inside").to_string_lossy().into_owned();
        let parent = space.dir.parent().unwrap().to_string_lossy().into_owned();
        // Inside a space's folder, or holding it: no new space
        assert!(check_new_source(&env.st, &inside).await.is_err());
        assert!(check_new_source(&env.st, &parent).await.is_err());
        // Nor a Local folder location there, or in ThirtyFile's data folder
        let local = |p: &str| serde_json::json!({ "path": p });
        assert!(check_location_folder(&env.st, None, "local", &local(&inside)).await.is_err());
        assert!(check_location_folder(&env.st, None, "local", &local(&env.dir.join("data-inside").to_string_lossy())).await.is_err());
        // A folder of its own is fine
        let free = std::env::temp_dir().join(format!("thirtyfile-free-{}", new_id()));
        std::fs::create_dir_all(&free).unwrap();
        assert!(check_new_source(&env.st, &free.to_string_lossy()).await.is_ok());
        assert!(check_location_folder(&env.st, None, "local", &local(&free.to_string_lossy())).await.is_ok());
        let _ = std::fs::remove_dir_all(&free);
    }
}
