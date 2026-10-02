//! Storage locations (System settings › Storage locations). Moving spaces between them is in moves/.

mod api;
mod health;
mod markers;

pub use api::*;
pub use health::*;
pub use markers::*;

use std::{collections::HashMap, path::Path as FsPath, sync::Arc, time::Duration};

use axum::{
    Json,
    extract::{Path, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{SqliteConnection, SqlitePool};

use crate::{
    auth::Admin,
    error::{AppError, AppResult},
    logs,
    state::{AppState, LocationHealth},
    storage::{self, Storage},
    tree,
    util::{self, new_id, now, validate_name},
};

/// The id of the built-in location
pub use crate::storage::BUILTIN;

#[derive(sqlx::FromRow)]
struct LocationRow {
    id: String,
    name: String,
    kind: String,
    config: String,
    is_default: bool,
}

/// A location's stored settings, with its passwords and keys decrypted (secrets.rs; each bound to the location and
/// field it is stored for)
pub(crate) fn config_json(id: &str, raw: &str) -> Value {
    let mut cfg: Value = serde_json::from_str(raw).unwrap_or_else(|_| json!({}));
    if let Some(obj) = cfg.as_object_mut() {
        for field in SECRET_FIELDS {
            if let Some(Value::String(v)) = obj.get_mut(field) {
                match crate::secrets::open(&format!("location:{id}:{field}"), v) {
                    Ok(plain) => *v = plain,
                    Err(e) => {
                        tracing::error!("A saved {field} of a storage location can't be read ({e}); enter it again");
                        v.clear();
                    }
                }
            }
        }
    }
    cfg
}

/// Settings as stored: passwords and keys encrypted
fn sealed_config(id: &str, cfg: &Value) -> String {
    let mut cfg = cfg.clone();
    if let Some(obj) = cfg.as_object_mut() {
        for field in SECRET_FIELDS {
            if let Some(Value::String(v)) = obj.get_mut(field) {
                *v = crate::secrets::seal(&format!("location:{id}:{field}"), v);
            }
        }
    }
    cfg.to_string()
}

/// Loads all storage locations at startup; locations that can't be built are logged as warnings and skipped (reading their files reports "unavailable").
/// Nothing is created: a Local folder location whose folder isn't there (or lacks its marker) is loaded, and the
/// health check reports it unavailable until the folder is back.
pub async fn load_all(db: &SqlitePool, storage_dir: &FsPath) -> Result<HashMap<String, Arc<dyn Storage>>, sqlx::Error> {
    let rows: Vec<LocationRow> = sqlx::query_as("SELECT id, name, kind, config, is_default FROM storage_locations").fetch_all(db).await?;
    let mut map = HashMap::new();
    for r in rows {
        match storage::build(&r.id, &r.kind, &config_json(&r.id, &r.config), storage_dir) {
            Ok(s) => {
                map.insert(r.id, s);
            }
            Err(e) => tracing::warn!("Storage location \"{}\" is unavailable: {e}", r.name),
        }
    }
    Ok(map)
}

/// The default storage location: where new spaces are created. A space records its location when it is created
/// (`db::create_drive`), so changing the default later doesn't move existing spaces.
pub async fn default_location(conn: &mut SqliteConnection) -> Result<String, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as("SELECT id FROM storage_locations WHERE is_default = 1 LIMIT 1").fetch_optional(conn).await?;
    Ok(row.map_or_else(|| BUILTIN.to_string(), |r| r.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    /// Adds a storage location (in the database and connected); `folder`: a Local folder location's folder, else one
    /// that works like a bucket (its content in a folder of the test)
    async fn add_location(env: &testutil::TestEnv, id: &str, folder: Option<&FsPath>) {
        let (kind, config) = match folder {
            Some(dir) => {
                std::fs::create_dir_all(dir).unwrap();
                ("local", json!({ "path": dir.to_string_lossy() }))
            }
            None => ("s3", json!({ "bucket": "files" })),
        };
        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, is_default, created_at) VALUES (?, ?, ?, ?, 0, 0)")
            .bind(id)
            .bind(id.to_uppercase())
            .bind(kind)
            .bind(config.to_string())
            .execute(&env.st.db)
            .await
            .unwrap();
        let backend = crate::storage::LocalStorage::create(folder.map_or_else(|| env.dir.join(id), FsPath::to_path_buf), id).unwrap();
        env.st.storages.write().unwrap().insert(id.into(), Arc::new(backend));
    }

    async fn make_default(env: &testutil::TestEnv, id: &str) {
        let _ = set_default(State(env.st.clone()), Admin(env.admin().await), Path(id.into())).await.unwrap();
    }

    /// A space's recorded location and mode
    async fn placed(env: &testutil::TestEnv, root_id: &str) -> (Option<String>, String) {
        sqlx::query_as("SELECT location_id, mode FROM drives WHERE root_id = ?").bind(root_id).fetch_one(&env.st.db).await.unwrap()
    }

    async fn new_team(env: &testutil::TestEnv, name: &str) -> String {
        let req = serde_json::from_value(json!({ "name": name })).unwrap();
        let Json(info) = crate::drives::create(State(env.st.clone()), env.admin().await, Json(req)).await.unwrap();
        serde_json::to_value(&info).unwrap()["root_id"].as_str().unwrap().to_string()
    }

    fn at(location: &str, mode: &str) -> (Option<String>, String) {
        (Some(location.into()), mode.into())
    }

    #[tokio::test]
    async fn spaces_keep_the_location_they_were_created_on() {
        let env = testutil::folders_env().await;
        let admin = env.admin().await;
        let amy = env.user("amy", true).await;
        let sales = new_team(&env, "Sales").await;
        // Every kind of space, on the built-in location
        for root in [admin.root(), amy.root(), &env.st.shared_root().unwrap(), &sales] {
            assert_eq!(placed(&env, root).await, at(BUILTIN, "folder"));
        }
        // A folder the administrator chose is on no location
        let shown = env.folder_space("Scans").await;
        assert_eq!(placed(&env, &shown.root).await, (None, "folder".into()));

        // A Local folder location as the default: new spaces get their folder there, the earlier ones stay
        let nas = env.dir.join("nas");
        add_location(&env, "nas", Some(&nas)).await;
        make_default(&env, "nas").await;
        let ben = env.user("ben", true).await;
        let plans = new_team(&env, "Plans").await;
        assert_eq!(placed(&env, ben.root()).await, at("nas", "folder"));
        assert_eq!(placed(&env, &plans).await, at("nas", "folder"));
        assert!(nas.join("users").join("ben").is_dir() && nas.join("teams").join("Plans").is_dir());
        assert_eq!(placed(&env, amy.root()).await, at(BUILTIN, "folder"));
        assert_eq!(placed(&env, &sales).await, at(BUILTIN, "folder"));

        // A bucket as the default: new spaces keep the content store there
        add_location(&env, "bucket", None).await;
        make_default(&env, "bucket").await;
        let carl = env.user("carl", true).await;
        assert_eq!(placed(&env, carl.root()).await, at("bucket", "store"));

        // Changing the default again moves nothing: Carl's new files still go to the bucket
        make_default(&env, BUILTIN).await;
        assert_eq!(placed(&env, carl.root()).await, at("bucket", "store"));
        assert_eq!(placed(&env, ben.root()).await, at("nas", "folder"));
        let carls = env.drive_of(carl.root()).await;
        assert_eq!(tree::drive_location(&mut env.st.db.acquire().await.unwrap(), &carls).await.unwrap(), "bucket");
        let id = env.upload(&carl, carl.root(), "a.txt", b"carl's").await;
        let (location,): (String,) =
            sqlx::query_as("SELECT b.location_id FROM nodes n JOIN blobs b ON b.hash = n.blob_hash WHERE n.id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(location, "bucket");
    }

    #[tokio::test]
    async fn the_list_counts_folder_spaces_and_the_content_store() {
        let env = testutil::folders_env().await;
        let amy = env.user("amy", true).await;
        env.upload(&amy, amy.root(), "notes.txt", b"twelve bytes").await;
        let company = env.st.shared_root().unwrap();
        env.upload(&env.admin().await, &company, "plan.txt", b"plan").await;
        // A space on a bucket, whose files are in the content store there
        add_location(&env, "bucket", None).await;
        make_default(&env, "bucket").await;
        let ben = env.user("ben", true).await;
        env.upload(&ben, ben.root(), "a.txt", b"ben's file").await;
        // A folder chosen by the administrator counts on no location
        let shown = env.folder_space("Scans").await;
        testutil::write_old(&shown.dir.join("scan.pdf"), b"%PDF-1.7 elsewhere");
        crate::folders::scan(&env.st, &shown.drive).await.unwrap();

        let Json(list) = list(State(env.st.clone()), Admin(env.admin().await)).await.unwrap();
        let list = serde_json::to_value(&list).unwrap();
        let find = |id: &str| list.as_array().unwrap().iter().find(|l| l["id"] == id).unwrap().clone();
        let local = find(BUILTIN);
        // "My files" of the administrator and Amy, and "All files"
        assert_eq!((local["drive_count"].as_i64(), local["used_bytes"].as_i64(), local["folder_bytes"].as_i64()), (Some(3), Some(16), Some(16)));
        // Its files are in folders, not in the content store: notes.txt and plan.txt
        assert_eq!((local["blob_count"].as_i64(), local["folder_files"].as_i64()), (Some(0), Some(2)));
        assert!(!cfg!(any(unix, windows)) || local["disk_total_bytes"].as_u64().unwrap() > 0);
        assert!(!cfg!(any(unix, windows)) || local["disk_free_bytes"].as_u64().is_some());
        let bucket = find("bucket");
        assert_eq!((bucket["drive_count"].as_i64(), bucket["used_bytes"].as_i64(), bucket["blob_count"].as_i64()), (Some(1), Some(10), Some(1)));
        assert_eq!((bucket["folder_bytes"].as_i64(), bucket["folder_files"].as_i64()), (Some(0), Some(0)));
        assert!(bucket["disk_total_bytes"].is_null(), "a bucket isn't a disk of this server");

        // The spaces on a location: what they are and their size
        let Json(on_local) = spaces(State(env.st.clone()), Admin(env.admin().await), Path(BUILTIN.into())).await.unwrap();
        let on_local = serde_json::to_value(&on_local).unwrap();
        let rows: Vec<(String, String, i64)> = on_local
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["kind"].as_str().unwrap().into(), s["owner_name"].as_str().unwrap().into(), s["used_bytes"].as_i64().unwrap()))
            .collect();
        assert_eq!(rows, [("company".into(), "".into(), 4), ("personal".into(), "admin".into(), 0), ("personal".into(), "amy".into(), 12)]);
        let Json(on_bucket) = spaces(State(env.st.clone()), Admin(env.admin().await), Path("bucket".into())).await.unwrap();
        assert_eq!(serde_json::to_value(&on_bucket).unwrap()[0]["mode"], "store");
    }

    #[tokio::test]
    async fn a_location_holding_a_folder_space_cant_be_deleted() {
        let env = testutil::folders_env().await;
        let nas = env.dir.join("nas");
        add_location(&env, "nas", Some(&nas)).await;
        make_default(&env, "nas").await;
        let amy = env.user("amy", true).await;
        make_default(&env, BUILTIN).await;
        // Nothing of Amy's is in the content store: her space alone keeps the location
        let (blobs,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM blobs").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(blobs, 0);
        let err = delete(State(env.st.clone()), Admin(env.admin().await), Path("nas".into())).await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err.message, "1 space still uses this location. Move it to another location first.");
        assert_eq!(placed(&env, amy.root()).await, at("nas", "folder"));
    }

    /// A folder outside the test's data folder (Local folder locations can't be inside it), removed when dropped
    struct Outside(std::path::PathBuf);

    impl Outside {
        fn new() -> Outside {
            let dir = std::env::temp_dir().join(format!("thirtyfile-disk-{}", new_id()));
            std::fs::create_dir_all(&dir).unwrap();
            Outside(dir)
        }
    }

    impl Drop for Outside {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn local(dir: &FsPath) -> Value {
        json!({ "kind": "local", "config": { "path": dir.to_string_lossy() } })
    }

    async fn add_local(env: &testutil::TestEnv, name: &str, dir: &FsPath) -> AppResult<String> {
        let mut req = local(dir);
        req["name"] = name.into();
        let Json(v) = create(State(env.st.clone()), Admin(env.admin().await), Json(serde_json::from_value(req).unwrap())).await?;
        Ok(v["id"].as_str().unwrap().to_string())
    }

    async fn move_to(env: &testutil::TestEnv, id: &str, dir: &FsPath) -> AppResult<()> {
        let req = serde_json::from_value(json!({ "config": { "path": dir.to_string_lossy() } })).unwrap();
        update(State(env.st.clone()), Admin(env.admin().await), Path(id.into()), Json(req)).await.map(|_| ())
    }

    fn marker(dir: &FsPath) -> Option<String> {
        std::fs::read_to_string(dir.join(storage::LOCATION_MARKER)).ok()
    }

    #[tokio::test]
    async fn a_local_folder_location_is_used_only_while_its_folder_holds_its_marker() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let disk = Outside::new();
        let dir = disk.0.join("nas").join("thirtyfile");
        let away = disk.0.join("away");

        // Trying the settings before adding creates nothing; adding creates the folder with the location's marker
        let _ = test(State(env.st.clone()), Admin(admin.clone()), Json(serde_json::from_value(local(&dir)).unwrap())).await.unwrap();
        assert!(!disk.0.join("nas").exists());
        let id = add_local(&env, "NAS", &dir).await.unwrap();
        assert_eq!(marker(&dir).as_deref(), Some(id.as_str()));
        probe(&env.st, &id).await.unwrap();
        make_default(&env, &id).await;
        let team = new_team(&env, "Sales").await;
        make_default(&env, BUILTIN).await;
        env.upload(&admin, &team, "before.txt", b"before").await;

        // Not mounted: the folder is gone. The location is unavailable, and nothing is made or written.
        std::fs::rename(&dir, &away).unwrap();
        let unmounted = |what: &str| (axum::http::StatusCode::SERVICE_UNAVAILABLE, storage::NOT_MOUNTED.to_string(), what.to_string());
        for what in ["missing", "an empty mount point", "another location's folder"] {
            match what {
                "an empty mount point" => std::fs::create_dir_all(&dir).unwrap(),
                "another location's folder" => std::fs::write(dir.join(storage::LOCATION_MARKER), "another").unwrap(),
                _ => {}
            }
            assert_eq!(probe(&env.st, &id).await.unwrap_err(), storage::NOT_MOUNTED, "{what}");
            assert_eq!(env.st.location_offline(&id).as_deref(), Some(storage::NOT_MOUNTED));
            let err = env.try_upload(&admin, &team, "after.txt", b"after").await.unwrap_err();
            assert_eq!((err.status, err.message, what.to_string()), unmounted(what));
            let err = test_existing(State(env.st.clone()), Admin(admin.clone()), Path(id.clone())).await.unwrap_err();
            assert_eq!(err.message, storage::NOT_MOUNTED, "{what}");
            // Its settings can't be saved either while the folder isn't there
            assert!(move_to(&env, &id, &dir).await.is_err(), "{what}");
            let steps = crate::location_tools::test_steps(State(env.st.clone()), Admin(admin.clone()), Path(id.clone())).await.unwrap();
            assert!(!steps.ok && steps.step("connect").unwrap().message.as_deref() == Some(storage::NOT_MOUNTED), "{what}");
        }
        assert_eq!(std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect::<Vec<_>>(), [storage::LOCATION_MARKER]);
        std::fs::remove_dir_all(&dir).unwrap();

        // Mounted again: available at the next check, without a restart
        std::fs::rename(&away, &dir).unwrap();
        probe(&env.st, &id).await.unwrap();
        assert_eq!(env.st.location_offline(&id), None);
        env.upload(&admin, &team, "after.txt", b"after").await;
    }

    #[tokio::test]
    async fn a_local_folder_location_takes_only_a_folder_of_its_own() {
        let env = testutil::env().await;
        let disk = Outside::new();
        let (first, second, other) = (disk.0.join("first"), disk.0.join("second"), disk.0.join("other"));
        let id = add_local(&env, "First", &first).await.unwrap();

        // A folder holding another location's marker is refused, when adding a location and when changing its folder
        storage::claim_folder(&other, "someone-else").unwrap();
        let err = add_local(&env, "Other", &other).await.unwrap_err();
        assert_eq!(err.message, storage::FOLDER_TAKEN);
        assert_eq!(move_to(&env, &id, &other).await.unwrap_err().message, storage::FOLDER_TAKEN);
        assert_eq!(marker(&other).as_deref(), Some("someone-else"));

        // Another folder is created with the marker; its earlier folder, which still holds its marker, is taken back
        move_to(&env, &id, &second).await.unwrap();
        assert_eq!(marker(&second).as_deref(), Some(id.as_str()));
        move_to(&env, &id, &first).await.unwrap();
        probe(&env.st, &id).await.unwrap();

        // A deleted location leaves its folder free for another
        let _ = delete(State(env.st.clone()), Admin(env.admin().await), Path(id.clone())).await.unwrap();
        assert_eq!(marker(&first), None);
        let again = add_local(&env, "Again", &first).await.unwrap();
        assert_eq!(marker(&first).as_deref(), Some(again.as_str()));
    }

    #[tokio::test]
    async fn a_folder_space_whose_folder_is_missing_is_never_made_again() {
        let env = testutil::folders_env().await;
        let admin = env.admin().await;
        let nas = env.dir.join("nas");
        add_location(&env, "nas", Some(&nas)).await;
        make_default(&env, "nas").await;
        let team = new_team(&env, "Plans").await;
        make_default(&env, BUILTIN).await;
        let drive = env.drive_of(&team).await;
        // The space's folder is marked as its own when it is created, before anything is written there
        assert_eq!(std::fs::read_to_string(nas.join("teams/Plans").join(crate::folders::MARKER)).unwrap(), drive);
        env.upload(&admin, &team, "a.txt", b"one").await;
        assert!(nas.join("teams/Plans/a.txt").is_file());

        let away = env.dir.join("nas-away");
        std::fs::rename(&nas, &away).unwrap();
        let _ = probe(&env.st, "nas").await;
        for mount_point in [false, true] {
            if mount_point {
                std::fs::create_dir(&nas).unwrap();
            }
            // Shown as offline, and changes are refused
            let Json(info) = crate::nodes::get(State(env.st.clone()), admin.clone(), Path(team.clone())).await.unwrap();
            assert_eq!(serde_json::to_value(&info).unwrap()["offline"], storage::NOT_MOUNTED);
            let err = env.try_upload(&admin, &team, "b.txt", b"two").await.unwrap_err();
            assert_eq!(err.status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
            assert!(err.message.starts_with(storage::NOT_MOUNTED), "{}", err.message);
            let req = serde_json::from_value(json!({ "parent_id": team, "name": "Docs" })).unwrap();
            let err = crate::nodes::create_folder(State(env.st.clone()), admin.clone(), Json(req)).await.unwrap_err();
            assert_eq!(err.message, storage::NOT_MOUNTED);
            // A scan keeps the index: the files aren't deleted, just not there right now
            let report = crate::folders::scan(&env.st, &drive).await.unwrap();
            assert!(report.error.is_some());
            assert!(env.node_at(&drive, "a.txt").await.is_some());
            assert_eq!(nas.exists(), mount_point, "nothing made");
            if mount_point {
                assert_eq!(std::fs::read_dir(&nas).unwrap().count(), 0, "nothing made in the empty mount point");
                std::fs::remove_dir(&nas).unwrap();
            }
        }

        // Another disk mounted there, with a folder at the same place: a scan doesn't take its items for the space's
        std::fs::create_dir_all(nas.join("teams/Plans")).unwrap();
        std::fs::write(nas.join(storage::LOCATION_MARKER), "another").unwrap();
        testutil::write_old(&nas.join("teams/Plans/other.txt"), b"other");
        let report = crate::folders::scan(&env.st, &drive).await.unwrap();
        assert_eq!(report.error.as_deref(), Some(storage::NOT_MOUNTED));
        assert!(env.node_at(&drive, "a.txt").await.is_some() && env.node_at(&drive, "other.txt").await.is_none());
        // ...and nothing is written into it, even with its own location's marker there: the folder isn't the space's
        std::fs::write(nas.join(storage::LOCATION_MARKER), "nas").unwrap();
        let err = env.try_upload(&admin, &team, "b.txt", b"two").await.unwrap_err();
        assert!(err.message.starts_with(storage::NOT_MOUNTED), "{}", err.message);
        let req = serde_json::from_value(json!({ "parent_id": team, "name": "Docs" })).unwrap();
        assert_eq!(crate::nodes::create_folder(State(env.st.clone()), admin.clone(), Json(req)).await.unwrap_err().message, storage::NOT_MOUNTED);
        let names = |dir: std::path::PathBuf| std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect::<Vec<_>>();
        assert_eq!(names(nas.join("teams/Plans")), ["other.txt"]);
        std::fs::remove_dir_all(&nas).unwrap();

        std::fs::rename(&away, &nas).unwrap();
        probe(&env.st, "nas").await.unwrap();
        env.upload(&admin, &team, "b.txt", b"two").await;
        assert!(nas.join("teams/Plans/b.txt").is_file());
    }

    #[tokio::test]
    async fn a_local_folder_location_in_use_moves_only_to_a_copy_of_its_folder() {
        let env = testutil::folders_env().await;
        let admin = env.admin().await;
        let disk = Outside::new();
        let (first, second, copy) = (disk.0.join("first"), disk.0.join("second"), disk.0.join("copy"));
        let id = add_local(&env, "NAS", &first).await.unwrap();
        make_default(&env, &id).await;
        let team = new_team(&env, "Sales").await;
        make_default(&env, BUILTIN).await;
        env.upload(&admin, &team, "plan.txt", b"the plan").await;
        let drive = env.drive_of(&team).await;

        // Another folder, which isn't a copy of it: the space's folder would stay behind
        let err = move_to(&env, &id, &second).await.unwrap_err();
        assert_eq!(err.message, IN_USE);
        assert!(!second.exists(), "nothing made");
        let saved: (String,) = sqlx::query_as("SELECT config FROM storage_locations WHERE id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap();
        assert!(saved.0.contains("first"));

        // A copy of the folder, marker included: the location and its spaces' folders follow
        copy_dir(&first, &copy);
        move_to(&env, &id, &copy).await.unwrap();
        let (source,): (String,) = sqlx::query_as("SELECT source_path FROM drives WHERE id = ?").bind(&drive).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(std::path::PathBuf::from(source), copy.join("teams").join("Sales"));
        std::fs::remove_dir_all(&first).unwrap();
        env.upload(&admin, &team, "after.txt", b"after").await;
        assert!(copy.join("teams/Sales/after.txt").is_file());
    }

    fn copy_dir(from: &FsPath, to: &FsPath) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                copy_dir(&e.path(), &to.join(e.file_name()));
            } else {
                std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn two_locations_never_share_a_place() {
        let env = testutil::env().await;
        let s3 = |endpoint: &str, bucket: &str, prefix: &str| json!({ "endpoint": endpoint, "bucket": bucket, "prefix": prefix });
        let place = |kind: &str, cfg: Value| place_of(kind, &cfg).unwrap();
        // The same bucket and prefix, written differently
        assert_eq!(place("s3", s3("https://S3.example.com/", "files", "/drive/")), place("s3", s3("https://s3.example.com/files", "", "drive")));
        assert_ne!(place("s3", s3("https://s3.example.com", "files", "drive")), place("s3", s3("https://s3.example.com", "files", "other")));
        assert_ne!(place("s3", s3("https://s3.example.com", "files", "")), place("s3", s3("https://s3.example.com", "photos", "")));
        // The same server and folder; a relative folder is in the account's home folder
        let host = |host: &str, user: &str, path: &str| json!({ "host": host, "username": user, "path": path });
        assert_eq!(place("sftp", host("NAS.example.com", "a", "/data/")), place("sftp", host("sftp://b@nas.example.com:22/data", "", "")));
        assert_ne!(place("sftp", host("nas.example.com", "a", "data")), place("sftp", host("nas.example.com", "b", "data")));
        assert_ne!(place("sftp", host("nas.example.com", "a", "/data")), place("ftp", host("nas.example.com", "a", "/data")));
        assert!(place_of("local", &json!({ "path": "/mnt/nas" })).is_none());

        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, is_default, created_at) VALUES ('b1', 'Bucket', 's3', ?, 0, 0)")
            .bind(s3("https://s3.example.com", "files", "drive").to_string())
            .execute(&env.st.db)
            .await
            .unwrap();
        let req = json!({ "name": "Again", "kind": "s3", "config": s3("https://s3.example.com/files", "", "/drive") });
        let err = create(State(env.st.clone()), Admin(env.admin().await), Json(serde_json::from_value(req).unwrap())).await.unwrap_err();
        assert_eq!(
            (err.status, err.message.as_str()),
            (
                axum::http::StatusCode::CONFLICT,
                "The storage location \"Bucket\" already uses this place (the same bucket and prefix, or the same server and folder)"
            )
        );
        // Editing a location to point there is refused too; itself is no duplicate
        let mut c = env.st.db.acquire().await.unwrap();
        assert!(check_place_free(&mut c, Some("b2"), "s3", &s3("https://s3.example.com", "files", "drive")).await.is_err());
        check_place_free(&mut c, Some("b1"), "s3", &s3("https://s3.example.com", "files", "drive")).await.unwrap();
    }

    #[tokio::test]
    async fn a_remote_place_holds_a_marker_naming_its_location_and_installation() {
        let env = testutil::env().await;
        // Ids of their own: health checks remember the locations they marked
        let (id, other) = (format!("b{}", new_id()), format!("o{}", new_id()));
        // Works like a bucket; a location added before markers were written has none with this installation's id
        add_location(&env, &id, None).await;
        let s = env.st.storage(&id).unwrap();
        let file = env.dir.join(&id).join(storage::LOCATION_MARKER);
        // Its first successful health check writes it
        probe(&env.st, &id).await.unwrap();
        let install = install_id(&env.st).await.unwrap();
        assert_eq!(install_id(&env.st).await.unwrap(), install, "made once");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), format!("{id}\n{install}\n"));
        assert_eq!(marker_state(&env.st, Some(&id), s.as_ref()).await.unwrap(), Marker::Ours);
        // Another location of this installation, or another installation: taken
        add_location(&env, &other, None).await;
        assert_eq!(marker_state(&env.st, Some(&other), s.as_ref()).await.unwrap(), Marker::Taken);
        std::fs::write(&file, format!("{id}\nsomeone-else\n")).unwrap();
        assert_eq!(marker_state(&env.st, Some(&id), s.as_ref()).await.unwrap(), Marker::Taken);
        // A location of this installation that is gone: free
        std::fs::write(&file, format!("{id}\n{install}\n")).unwrap();
        assert_eq!(marker_state(&env.st, Some("new"), s.as_ref()).await.unwrap(), Marker::Taken);
        sqlx::query("DELETE FROM storage_locations WHERE id = ?").bind(&id).execute(&env.st.db).await.unwrap();
        assert_eq!(marker_state(&env.st, Some("new"), s.as_ref()).await.unwrap(), Marker::Missing);
    }

    #[tokio::test]
    async fn saved_passwords_are_only_reused_for_the_same_server_and_account() {
        let env = testutil::env().await;
        let saved = json!({ "host": "files.example.com", "port": 22, "username": "backup", "password": testutil::password(), "host_key": "k" });
        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, is_default, created_at) VALUES ('nas', 'NAS', 'sftp', ?, 0, 0)")
            .bind(sealed_config("nas", &saved))
            .execute(&env.st.db)
            .await
            .unwrap();
        // Editing something else: the password is filled in
        let same = json!({ "host": "files.example.com", "port": 22, "username": "backup", "password": "", "host_key": "k" });
        let merged = merged_config(&env.st, Some("nas"), "sftp", same).await.unwrap();
        assert_eq!(merged["password"], testutil::password());
        // Another server, another account or another kind: it must be entered again
        for (kind, cfg) in [
            ("sftp", json!({ "host": "elsewhere.example.com", "port": 22, "username": "backup", "password": "" })),
            ("sftp", json!({ "host": "files.example.com", "port": 22, "username": "someone", "password": "" })),
            ("ftp", json!({ "host": "files.example.com", "port": 22, "username": "backup", "password": "" })),
        ] {
            let err = merged_config(&env.st, Some("nas"), kind, cfg).await.unwrap_err();
            assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        }
        // Trusting another host key (or any, to record it again), or no longer checking the certificate: the same
        let cleared = json!({ "host": "files.example.com", "port": 22, "username": "backup", "password": "", "host_key": "" });
        assert!(merged_config(&env.st, Some("nas"), "sftp", cleared).await.is_err());
        let other_key = json!({ "host": "files.example.com", "port": 22, "username": "backup", "password": "", "host_key": "k2" });
        assert!(merged_config(&env.st, Some("nas"), "sftp", other_key).await.is_err());
        let ftps = json!({ "host": "files.example.com", "port": 21, "username": "backup", "password": testutil::password(), "tls": true });
        sqlx::query("UPDATE storage_locations SET kind = 'ftp', config = ? WHERE id = 'nas'").bind(sealed_config("nas", &ftps)).execute(&env.st.db).await.unwrap();
        let unchecked = json!({ "host": "files.example.com", "port": 21, "username": "backup", "password": "", "tls": true, "tls_insecure": true });
        assert!(merged_config(&env.st, Some("nas"), "ftp", unchecked).await.is_err());
        let checked = json!({ "host": "files.example.com", "port": 21, "username": "backup", "password": "", "tls": true, "tls_insecure": false });
        assert_eq!(merged_config(&env.st, Some("nas"), "ftp", checked).await.unwrap()["password"], testutil::password());
        // A password entered anew is used as it is
        let fresh = json!({ "host": "elsewhere.example.com", "port": 22, "username": "backup", "password": testutil::wrong_password() });
        assert!(merged_config(&env.st, Some("nas"), "sftp", fresh).await.is_ok());
    }
}
