//! Where spaces keep their files on this server's disks.
//!
//! Spaces on the built-in storage and on *Local folder* locations are folder spaces (folders.rs): their files are
//! ordinary files in a folder of the location's folder (the storage folder, `/storage` in Docker, for the built-in one):
//!
//! - `company` for the company space "All files"
//! - `teams/<space name>` for team spaces
//! - `users/<user name>` for each user's "My files"
//!
//! The folder is named when the space is created (`thirtyfile convert` names the folders of spaces from 0.1 and 0.2
//! the same way):
//! - characters a file name can't have become `_`, and a name too long for the disk is shortened; a name with
//!   nothing left uses the space's id
//! - a folder that is taken, by another space or already on the disk (made by hand, or kept from a deleted space),
//!   gets a number: `teams/Sales (2)`
//! - renaming the space or the user later doesn't rename the folder: other programs, shares over SMB and backups may
//!   use the path
//! - deleting the space or the user doesn't delete the folder: the files stay on the disk, for an administrator to
//!   remove or keep (as with any folder space)
//!
//! Spaces on S3, SFTP and FTP keep their files in the content store (storage.rs): renaming a folder there would copy
//! every file in it. Spaces already in the content store stay there until `thirtyfile convert` turns them into
//! folders.

use std::path::{Path, PathBuf};

use sqlx::SqliteConnection;

use crate::{
    error::{AppError, AppResult},
    folders::ignored,
    storage::LocalConfig,
    util::split_name,
};

/// The longest name most file systems hold, in bytes
pub const MAX_NAME_BYTES: usize = 255;

/// The name an item gets on disk, and why it differs: characters a file name can't have become `_`, names longer
/// than disks allow are shortened, and names scans skip (`Thumbs.db`, `~$…` and other temporary files) get an
/// underscore, as the item would otherwise disappear from the space
pub fn disk_name(name: &str, is_dir: bool) -> (String, Option<&'static str>) {
    let mut why = None;
    let mut out: String = name.chars().map(|c| if c.is_control() || c == '/' || c == '\\' { '_' } else { c }).collect();
    if out != name || out.is_empty() || out == "." || out == ".." {
        if out.is_empty() || out == "." || out == ".." {
            out = format!("_{out}");
        }
        why = Some("it has characters a file name can't have");
    }
    if out.len() > MAX_NAME_BYTES {
        let (stem, ext) = split_name(&out, is_dir);
        let ext = if ext.len() <= 32 { ext } else { "" };
        // Room for a number, should the shorter name be taken
        let mut cut = (MAX_NAME_BYTES - ext.len() - 12).min(stem.len());
        while !stem.is_char_boundary(cut) {
            cut -= 1;
        }
        out = format!("{}{ext}", &stem[..cut]);
        why = Some("the name is too long for the disk");
    }
    if ignored(&out) {
        out = format!("_{out}");
        if ignored(&out) {
            out.push('_');
        }
        why = Some("scans skip names like this one");
    }
    (out, why)
}

/// A space's (or user's) name as a folder name: also without the dots and spaces at the end that Windows drops
fn folder_name(name: &str, id: &str) -> String {
    let (name, _) = disk_name(name.trim(), true);
    let name = name.trim_end_matches(['.', ' ']);
    if name.is_empty() { id.to_string() } else { name.to_string() }
}

/// Where a space's folder goes in the folder of its location, before numbering: (the folder it goes in, its name).
/// `name`: the space's name, `owner`: its owner's user name (personal spaces are named after it).
pub fn place(root: &Path, kind: &str, name: &str, owner: &str, id: &str) -> (PathBuf, String) {
    match kind {
        "company" => (root.to_path_buf(), "company".to_string()),
        "team" => (root.join("teams"), folder_name(name, id)),
        _ => (root.join("users"), folder_name(if owner.is_empty() { name } else { owner }, id)),
    }
}

/// The first of `name`, `name (2)`, `name (3)`… in `parent` that no space uses and that isn't on the disk yet (on a
/// disk that ignores letter case, `Sales` is also taken by `sales`); None when all are taken
pub async fn free_folder(conn: &mut SqliteConnection, parent: &Path, name: &str) -> Result<Option<PathBuf>, sqlx::Error> {
    for n in 1..10_000u32 {
        let candidate = parent.join(if n == 1 { name.to_string() } else { format!("{name} ({n})") });
        let (taken,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM drives WHERE source_path = ?").bind(candidate.to_string_lossy()).fetch_one(&mut *conn).await?;
        if taken == 0 && std::fs::symlink_metadata(&candidate).is_err() {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

/// The folder of the location a new space's files go to, when it is a folder of this server: the built-in location
/// (`builtin`, the storage folder) or a *Local folder* location. None for S3, SFTP and FTP.
async fn location_folder(conn: &mut SqliteConnection, builtin: &Path, location: &str) -> AppResult<Option<PathBuf>> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT kind, config FROM storage_locations WHERE id = ?").bind(location).fetch_optional(&mut *conn).await?;
    let (kind, config) = match row {
        Some(r) => r,
        // The built-in location is always there; an unknown one keeps the content store, which reports it
        None if location == crate::locations::BUILTIN => ("local".into(), String::new()),
        None => return Ok(None),
    };
    if kind != "local" {
        return Ok(None);
    }
    let path = serde_json::from_str::<LocalConfig>(&config).map(|c| c.path.trim().to_string()).unwrap_or_default();
    Ok(Some(if location == crate::locations::BUILTIN || path.is_empty() { builtin.to_path_buf() } else { PathBuf::from(path) }))
}

/// Makes a space that was just created (and is still empty) a folder space when its files go to a folder of this
/// server: creates its folder and points the space at it. `builtin`: the built-in location's folder
/// (`AppState::space_folders`); None keeps every new space in the content store. Returns the folder; the caller calls
/// `folders::spaces_changed` once its transaction is committed.
pub async fn make_folder_space(conn: &mut SqliteConnection, builtin: Option<&Path>, drive_id: &str) -> AppResult<Option<PathBuf>> {
    let Some(builtin) = builtin else { return Ok(None) };
    let (name, kind, root_id, location, owner): (String, String, String, String, String) = sqlx::query_as(
        "SELECT d.name, d.kind, d.root_id,
                COALESCE(d.location_id, (SELECT id FROM storage_locations WHERE is_default = 1), 'local'),
                COALESCE((SELECT username FROM users WHERE id = d.owner_id), '')
         FROM drives d WHERE d.id = ?",
    )
    .bind(drive_id)
    .fetch_one(&mut *conn)
    .await?;
    let Some(root) = location_folder(conn, builtin, &location).await? else { return Ok(None) };
    let root = std::path::absolute(&root).unwrap_or(root);
    // The location's folder itself must be there: a NAS that isn't mounted must not get the folder on the disk
    // below it
    if !root.is_dir() {
        return Err(AppError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            format!("The folder of the storage location ({}) isn't available", root.display()),
        ));
    }
    let (parent, wanted) = place(&root, &kind, &name, &owner, drive_id);
    let folder = free_folder(conn, &parent, &wanted)
        .await?
        .ok_or_else(|| AppError::conflict(format!("There is no free folder name for the space in {}", parent.display())))?;
    std::fs::create_dir_all(&folder).map_err(|e| {
        AppError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("Couldn't create the folder {} for the space: {e}", folder.display()),
        )
    })?;
    crate::folders::set_up(conn, drive_id, &root_id, &folder.to_string_lossy()).await?;
    Ok(Some(folder))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;
    use axum::{
        Json,
        extract::{Path as UrlPath, Query, State},
        http::HeaderMap,
    };
    use serde_json::{Value, json};

    async fn space_of(env: &testutil::TestEnv, root_id: &str) -> (String, Option<String>) {
        sqlx::query_as("SELECT mode, source_path FROM drives WHERE root_id = ?").bind(root_id).fetch_one(&env.st.db).await.unwrap()
    }

    async fn new_team(env: &testutil::TestEnv, name: &str) -> Value {
        let req = serde_json::from_value(json!({ "name": name })).unwrap();
        let Json(info) = crate::drives::create(State(env.st.clone()), env.admin().await, Json(req)).await.unwrap();
        serde_json::to_value(&info).unwrap()
    }

    async fn content(env: &testutil::TestEnv, user: &crate::auth::User, id: &str) -> Vec<u8> {
        let q = Query(serde_json::from_value(json!({})).unwrap());
        let res = crate::files::content(State(env.st.clone()), user.clone(), UrlPath(id.to_string()), q, HeaderMap::new()).await.unwrap();
        axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec()
    }

    fn req<T: serde::de::DeserializeOwned>(v: Value) -> Json<T> {
        Json(serde_json::from_value(v).unwrap())
    }

    fn folder(storage: &Path, rel: &str) -> Option<String> {
        Some(storage.join(rel).to_string_lossy().into_owned())
    }

    #[tokio::test]
    async fn new_spaces_are_folders_in_the_storage_folder() {
        let env = testutil::folders_env().await;
        let storage = env.dir.join("blobs");
        let admin = env.admin().await;
        let amy = env.user("amy", true).await;
        assert_eq!(space_of(&env, &admin.root_id).await, ("folder".into(), folder(&storage, "users/admin")));
        assert_eq!(space_of(&env, &amy.root_id).await, ("folder".into(), folder(&storage, "users/amy")));
        assert_eq!(space_of(&env, &env.st.shared_root().unwrap()).await, ("folder".into(), folder(&storage, "company")));
        assert!(storage.join("users/amy").is_dir() && storage.join("company").is_dir());

        // Team spaces: two with the same name, a name scans would skip, a folder made by hand
        let sales = new_team(&env, "Sales").await;
        let again = new_team(&env, "Sales").await;
        let odd = new_team(&env, "desktop.ini").await;
        std::fs::create_dir_all(storage.join("teams/Plans")).unwrap();
        let plans = new_team(&env, "Plans").await;
        let path = |v: &Value| v["id"].as_str().unwrap().to_string();
        for (space, rel) in [(&sales, "teams/Sales"), (&again, "teams/Sales (2)"), (&odd, "teams/_desktop.ini"), (&plans, "teams/Plans (2)")] {
            let (mode, source): (String, Option<String>) =
                sqlx::query_as("SELECT mode, source_path FROM drives WHERE id = ?").bind(path(space)).fetch_one(&env.st.db).await.unwrap();
            assert_eq!((mode, source), ("folder".into(), folder(&storage, rel)));
            assert!(storage.join(rel).is_dir());
        }

        // Renaming the space keeps its folder
        let req = serde_json::from_value(json!({ "name": "Sales and marketing" })).unwrap();
        let _ = crate::drives::update(State(env.st.clone()), admin.clone(), UrlPath(path(&sales)), Json(req)).await.unwrap();
        let (source,): (String,) = sqlx::query_as("SELECT source_path FROM drives WHERE id = ?").bind(path(&sales)).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(Some(source), folder(&storage, "teams/Sales"));

        // Deleting it keeps the folder and what is in it; a new space with the name gets another folder
        let root = sales["root_id"].as_str().unwrap();
        env.upload(&admin, root, "plan.txt", b"keep me").await;
        let _ = crate::drives::delete(State(env.st.clone()), admin.clone(), UrlPath(path(&sales))).await.unwrap();
        assert_eq!(std::fs::read(storage.join("teams/Sales/plan.txt")).unwrap(), b"keep me");
        let (detail,): (String,) = sqlx::query_as("SELECT detail FROM activity WHERE action = 'drive_delete'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(detail, format!("Sales and marketing (its folder on the server is kept: {})", storage.join("teams/Sales").display()));
        let third = new_team(&env, "Sales").await;
        let (source,): (String,) = sqlx::query_as("SELECT source_path FROM drives WHERE id = ?").bind(path(&third)).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(Some(source), folder(&storage, "teams/Sales (3)"));
    }

    #[tokio::test]
    async fn files_are_ordinary_files_in_a_new_space() {
        let env = testutil::folders_env().await;
        let dir = env.dir.join("blobs/users/amy");
        let amy = env.user("amy", true).await;
        let st = || State(env.st.clone());

        let Json(docs) = crate::nodes::create_folder(st(), amy.clone(), req(json!({ "parent_id": amy.root_id, "name": "Docs" }))).await.unwrap();
        let a = env.upload(&amy, &docs.id, "a.txt", b"hello").await;
        assert_eq!(std::fs::read(dir.join("Docs/a.txt")).unwrap(), b"hello");
        assert_eq!(content(&env, &amy, &a).await, b"hello");
        let (hashed,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM blobs").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(hashed, 0, "nothing in the content store");

        let _ = crate::nodes::rename(st(), amy.clone(), UrlPath(a.clone()), req(json!({ "name": "b.txt" }))).await.unwrap();
        assert!(dir.join("Docs/b.txt").is_file() && !dir.join("Docs/a.txt").exists());
        let _ = crate::nodes::move_nodes(st(), amy.clone(), req(json!({ "ids": [a], "dest_id": amy.root_id }))).await.unwrap();
        assert!(dir.join("b.txt").is_file());

        // Deleted: in the space's trash folder first, then gone
        let _ = crate::nodes::trash(st(), amy.clone(), req(json!({ "ids": [a] }))).await.unwrap();
        assert!(!dir.join("b.txt").exists() && dir.join(crate::fsops::TRASH_DIR).is_dir());
        let _ = crate::nodes::delete_forever(st(), amy.clone(), req(json!({ "ids": [a] }))).await.unwrap();
        let mut gone = false;
        for _ in 0..100 {
            gone = std::fs::read_dir(dir.join(crate::fsops::TRASH_DIR)).unwrap().next().is_none();
            if gone {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(gone);

        // A file put there by another program shows up
        testutil::write_old(&dir.join("scanned.pdf"), b"%PDF");
        let (drive,): (String,) = sqlx::query_as("SELECT id FROM drives WHERE root_id = ?").bind(&amy.root_id).fetch_one(&env.st.db).await.unwrap();
        crate::folders::scan(&env.st, &drive).await.unwrap();
        assert!(env.node_at(&drive, "scanned.pdf").await.is_some());
        let (used,): (i64,) = sqlx::query_as("SELECT used_bytes FROM drives WHERE id = ?").bind(&drive).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(used, 4);
    }

    #[tokio::test]
    async fn spaces_on_s3_sftp_or_ftp_keep_the_content_store_and_local_folders_get_theirs() {
        let env = testutil::folders_env().await;
        // New files go to a bucket: new spaces keep the content store
        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, is_default, created_at) VALUES ('s3', 'Bucket', 's3', '{}', 0, 0)")
            .execute(&env.st.db)
            .await
            .unwrap();
        sqlx::query("UPDATE storage_locations SET is_default = (id = 's3')").execute(&env.st.db).await.unwrap();
        let amy = env.user("amy", true).await;
        let team = new_team(&env, "Remote").await;
        assert_eq!(space_of(&env, &amy.root_id).await, ("store".into(), None));
        assert_eq!(space_of(&env, team["root_id"].as_str().unwrap()).await, ("store".into(), None));
        assert!(!env.dir.join("blobs/users/amy").exists());

        // A Local folder location (a NAS): the spaces' folders go in its folder
        let nas = env.dir.join("nas");
        std::fs::create_dir_all(&nas).unwrap();
        let config = json!({ "path": nas.to_string_lossy() }).to_string();
        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, is_default, created_at) VALUES ('nas', 'NAS', 'local', ?, 0, 0)")
            .bind(&config)
            .execute(&env.st.db)
            .await
            .unwrap();
        sqlx::query("UPDATE storage_locations SET is_default = (id = 'nas')").execute(&env.st.db).await.unwrap();
        let ben = env.user("ben", true).await;
        assert_eq!(space_of(&env, &ben.root_id).await, ("folder".into(), folder(&nas, "users/ben")));

        // Not mounted: the space isn't created on the disk below the mount point
        std::fs::remove_dir_all(&nas).unwrap();
        let req = serde_json::from_value(json!({ "name": "Offline" })).unwrap();
        let Err(err) = crate::drives::create(State(env.st.clone()), env.admin().await, Json(req)).await else { panic!("created below the mount point") };
        assert_eq!(err.status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert!(!nas.exists());
    }

    #[test]
    fn space_names_become_folder_names() {
        let root = Path::new("/storage");
        assert_eq!(place(root, "company", "All files", "admin", "id1"), (root.to_path_buf(), "company".into()));
        assert_eq!(place(root, "team", "Sales / EU", "amy", "id2"), (root.join("teams"), "Sales _ EU".into()));
        assert_eq!(place(root, "personal", "My files", "amy", "id3"), (root.join("users"), "amy".into()));
        // Nothing usable left, or names Windows can't keep: the id, or the name without the dots at the end
        assert_eq!(place(root, "team", "...", "amy", "id4").1, "id4");
        assert_eq!(place(root, "team", "Plans...", "amy", "id5").1, "Plans");
        assert_eq!(place(root, "team", "Thumbs.db", "amy", "id6").1, "_Thumbs.db");
        let long = "x".repeat(300);
        assert!(place(root, "team", &long, "amy", "id7").1.len() <= MAX_NAME_BYTES);
    }
}
