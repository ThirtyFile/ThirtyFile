//! Shared integration test helpers: each test uses its own temporary data directory and SQLite database.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use crate::{
    auth::{self, User},
    db::{self, NewUser},
    state::{AppState, Inner},
    storage::{LocalStorage, Storage},
    util::{new_id, now},
};

/// The password of every test user. Generated once per test run, so the code holds no hard-coded credentials.
pub fn password() -> &'static str {
    static PASSWORD: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PASSWORD.get_or_init(|| format!("pw-{}", new_id()))
}

/// A password that is certainly wrong
pub fn wrong_password() -> String {
    format!("wrong-{}", new_id())
}

pub struct TestEnv {
    pub st: AppState,
    pub dir: PathBuf,
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub async fn env() -> TestEnv {
    let dir = std::env::temp_dir().join(format!("thirtyfile-test-{}", new_id()));
    for d in ["tmp", "thumbs", "blobs"] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    let db = db::connect(&dir.join("drive.db")).await.unwrap();
    db::bootstrap_admin(&db, Some("admin-test-password")).await.unwrap();
    let system = db::load_system_settings(&db).await.unwrap();
    let mut storages: HashMap<String, Arc<dyn Storage>> = HashMap::new();
    storages.insert("local".into(), Arc::new(LocalStorage::new(dir.join("blobs")).unwrap()));
    let (log_tx, log_rx) = crate::logs::channel();
    let st = AppState(Arc::new(Inner {
        db,
        storages: std::sync::RwLock::new(storages),
        default_location: std::sync::RwLock::new("local".into()),
        migrations: Default::default(),
        data_dir: dir.clone(),
        storage_dir: dir.join("blobs"),
        secret: vec![7; 32],
        secure_cookie: false,
        trash_days: 30,
        trust_proxy: Default::default(),
        max_upload: 0,
        write_lock: tokio::sync::Mutex::new(()),
        active_uploads: Default::default(),
        login_failures: Default::default(),
        thumb_permits: tokio::sync::Semaphore::new(2),
        system: std::sync::RwLock::new(system),
        blob_guard: Default::default(),
        logs: std::sync::RwLock::new(Default::default()),
        branding: std::sync::RwLock::new(Default::default()),
        location_health: Default::default(),
        sso: Default::default(),
        sso_pending: Default::default(),
        archive_lock: Default::default(),
        share_views: Default::default(),
        log_tx,
    }));
    let _writer = crate::logs::spawn_writer(st.clone(), log_rx);
    TestEnv { st, dir }
}

impl TestEnv {
    /// Creates a standard user (with a personal space)
    pub async fn user(&self, name: &str, can_share: bool) -> User {
        let mut conn = self.st.db.acquire().await.unwrap();
        let password_hash = auth::hash_password(password().into()).await.unwrap();
        let id = db::create_user(
            &mut conn,
            NewUser { username: name, password_hash: &password_hash, role: "user", can_write: true, can_delete: true, can_share, quota_bytes: 0, source: "password", provisioned_by: None },
        )
        .await
        .unwrap();
        auth::user_by_id(&self.st, &mut conn, id).await.unwrap().unwrap()
    }

    pub async fn admin(&self) -> User {
        let mut conn = self.st.db.acquire().await.unwrap();
        let (id,): (i64,) = sqlx::query_as("SELECT id FROM users WHERE role = 'admin'").fetch_one(&mut *conn).await.unwrap();
        auth::user_by_id(&self.st, &mut conn, id).await.unwrap().unwrap()
    }

    pub async fn folder(&self, owner: &User, parent: &str, name: &str) -> String {
        let mut conn = self.st.db.acquire().await.unwrap();
        crate::tree::create_folder(&mut conn, owner.id, parent, name).await.unwrap()
    }

    /// Creates a file node (without physical content) in a folder
    pub async fn file(&self, owner: &User, parent: &str, name: &str) -> String {
        let id = new_id();
        sqlx::query(
            "INSERT INTO nodes (id, owner_id, parent_id, kind, name, size, mime, drive_id, created_at, updated_at)
             SELECT ?1, ?2, ?3, 'file', ?4, 0, 'text/plain', drive_id, ?5, ?5 FROM nodes WHERE id = ?3",
        )
        .bind(&id)
        .bind(owner.id)
        .bind(parent)
        .bind(name)
        .bind(now())
        .execute(&self.st.db)
        .await
        .unwrap();
        id
    }

    pub async fn grant(&self, node: &str, to: &User, role: &str) {
        let mut conn = self.st.db.acquire().await.unwrap();
        db::add_grant(&mut conn, node, "user", to.id, role, None, None).await.unwrap();
    }

    pub async fn revoke(&self, node: &str, from: &User) {
        sqlx::query("DELETE FROM grants WHERE node_id = ? AND principal_type = 'user' AND principal_id = ?")
            .bind(node)
            .bind(from.id)
            .execute(&self.st.db)
            .await
            .unwrap();
    }

    /// A folder space over a new temporary folder, created by the administrator (its owner) and scanned once
    pub async fn folder_space(&self, name: &str) -> FolderSpace {
        let dir = std::env::temp_dir().join(format!("thirtyfile-folder-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let req = serde_json::from_value(serde_json::json!({ "name": name, "source_path": dir.to_string_lossy() })).unwrap();
        let axum::Json(info) = crate::drives::create(axum::extract::State(self.st.clone()), self.admin().await, axum::Json(req)).await.unwrap();
        let v = serde_json::to_value(&info).unwrap();
        let drive = v["id"].as_str().unwrap().to_string();
        // Creating the space starts indexing in the background: let it finish on the empty folder first
        crate::folders::scan(&self.st, &drive).await.unwrap();
        FolderSpace { dir, drive, root: v["root_id"].as_str().unwrap().to_string() }
    }

    /// The node indexed at `rel` in a folder space: (id, size)
    pub async fn node_at(&self, drive: &str, rel: &str) -> Option<(String, i64)> {
        sqlx::query_as("SELECT id, size FROM nodes WHERE drive_id = ? AND fs_path = ? AND trashed_at IS NULL")
            .bind(drive)
            .bind(rel)
            .fetch_optional(&self.st.db)
            .await
            .unwrap()
    }

    pub async fn drive_of(&self, node: &str) -> String {
        let (d,): (String,) = sqlx::query_as("SELECT drive_id FROM nodes WHERE id = ?").bind(node).fetch_one(&self.st.db).await.unwrap();
        d
    }
}

/// A folder space's temporary folder, removed when dropped
pub struct FolderSpace {
    pub dir: PathBuf,
    pub drive: String,
    pub root: String,
}

impl Drop for FolderSpace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Writes a file dated a minute ago, so scans don't treat it as still being written
pub fn write_old(path: &std::path::Path, content: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(60)).unwrap();
}
