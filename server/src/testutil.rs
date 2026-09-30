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

/// A server whose spaces keep their files in the content store, as most tests of files, shares and permissions want
/// (they work the same in folder spaces, which have tests of their own)
pub async fn env() -> TestEnv {
    make_env(false).await
}

/// A server as installed today: every space of the built-in storage is a folder space, in `blobs/company`,
/// `blobs/teams/<name>` and `blobs/users/<name>`
pub async fn folders_env() -> TestEnv {
    make_env(true).await
}

async fn make_env(space_folders: bool) -> TestEnv {
    let dir = std::env::temp_dir().join(format!("thirtyfile-test-{}", new_id()));
    for d in ["tmp", "thumbs", "blobs"] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    crate::storage::prepare_builtin(&dir.join("blobs"), false).unwrap();
    let space_folders = space_folders.then(|| dir.join("blobs"));
    let db = db::connect(&dir.join("drive.db"), 16).await.unwrap();
    db::bootstrap_admin(&db, Some(password()), space_folders.as_deref()).await.unwrap();
    db::create_company_space(&db, space_folders.as_deref()).await.unwrap();
    let system = db::load_system_settings(&db).await.unwrap();
    let mut storages: HashMap<String, Arc<dyn Storage>> = HashMap::new();
    storages.insert("local".into(), Arc::new(LocalStorage::new(dir.join("blobs"), "local")));
    let (log_tx, log_rx) = crate::logs::channel();
    let st = AppState(Arc::new(Inner {
        db,
        storages: std::sync::RwLock::new(storages),
        moves: Default::default(),
        backups: Default::default(),
        data_dir: dir.clone(),
        storage_dir: dir.join("blobs"),
        space_folders,
        secret: vec![7; 32],
        secure_cookie: false,
        trash_days: 30,
        trust_proxy: Default::default(),
        max_upload: 0,
        write_lock: tokio::sync::Mutex::new(()),
        active_uploads: Default::default(),
        login_failures: Default::default(),
        detached_purge: Default::default(),
        thumb_permits: tokio::sync::Semaphore::new(2),
        thumb_decode_bytes: crate::thumbnails::MAX_THUMB_DECODE_BYTES,
        system: std::sync::RwLock::new(system),
        blob_guard: Default::default(),
        logs: std::sync::RwLock::new(Default::default()),
        branding: std::sync::RwLock::new(Default::default()),
        location_health: Default::default(),
        sso: Default::default(),
        sso_pending: Default::default(),
        twofactor_setups: Default::default(),
        archive_lock: Default::default(),
        share_views: Default::default(),
        download_links: Default::default(),
        jobs: Default::default(),
        log_tx,
        usage: Default::default(),
        error_log: Default::default(),
    }));
    let _writer = crate::logs::spawn_writer(st.clone(), log_rx);
    TestEnv { st, dir }
}

impl TestEnv {
    /// Creates a standard user (with a personal space)
    pub async fn user(&self, name: &str, can_share: bool) -> User {
        let mut conn = self.st.db.acquire().await.unwrap();
        let password_hash = auth::hash_password(password().into()).await.unwrap();
        let location = crate::locations::default_location(&mut conn).await.unwrap();
        let id = db::create_user(
            &mut conn,
            NewUser {
                username: name,
                password_hash: &password_hash,
                role: "user",
                can_write: true,
                can_delete: true,
                can_share,
                quota_bytes: 0,
                source: "password",
                provisioned_by: None,
                personal_space: Some(&location),
                space_folders: self.st.space_folders.as_deref(),
            },
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

    /// Signs `user` in from a browser with this User-Agent (from 10.0.0.1): the user as the session sees them, and the
    /// `name=value` cookie to send
    pub async fn sign_in(&self, user: &User, agent: &str) -> (User, String) {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(axum::http::header::USER_AGENT, agent.parse().unwrap());
        let set = auth::open_session(&self.st, user.id, "password", "10.0.0.1", &headers).await.unwrap();
        let cookie = set.split(';').next().unwrap().to_string();
        let session = self.session_user(&cookie).await.expect("a new session works");
        (session, cookie)
    }

    /// The user a request with this cookie is signed in as, if any
    pub async fn session_user(&self, cookie: &str) -> Option<User> {
        // (the page a session always reaches, even one that must change its password first)
        self.request_user(axum::http::Request::builder().uri("/api/auth/me").header(axum::http::header::COOKIE, cookie)).await
    }

    /// Runs the `User` extractor on a request
    pub async fn request_user(&self, req: axum::http::request::Builder) -> Option<User> {
        use axum::extract::FromRequestParts;
        let (mut parts, _) = req.body(()).unwrap().into_parts();
        User::from_request_parts(&mut parts, &self.st).await.ok()
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

    /// Creates a file with real content in the local storage location
    pub async fn stored_file(&self, owner: &User, parent: &str, name: &str, content: &[u8]) -> String {
        let id = self.file(owner, parent, name).await;
        let hash = crate::util::sha256_hex(content);
        let tmp = self.dir.join("tmp").join(format!("src-{}", new_id()));
        std::fs::write(&tmp, content).unwrap();
        crate::storage::Storage::put_file(self.st.storage("local").unwrap().as_ref(), &hash, &tmp).await.unwrap();
        let mut c = self.st.db.acquire().await.unwrap();
        crate::tree::add_blob_ref(&mut c, &hash, content.len() as i64, "local").await.unwrap();
        sqlx::query("UPDATE nodes SET blob_hash = ?, size = ?, mime = ? WHERE id = ?")
            .bind(&hash)
            .bind(content.len() as i64)
            .bind(crate::util::guess_mime(name))
            .bind(&id)
            .execute(&mut *c)
            .await
            .unwrap();
        id
    }

    /// Uploads a file as the web does (tus): into the content store or a folder space, whichever `parent` is in.
    /// Returns its id.
    pub async fn upload(&self, user: &User, parent: &str, name: &str, content: &'static [u8]) -> String {
        self.try_upload(user, parent, name, content).await.unwrap()
    }

    /// `upload`, returning the error when it fails
    pub async fn try_upload(&self, user: &User, parent: &str, name: &str, content: &'static [u8]) -> crate::error::AppResult<String> {
        use axum::{
            extract::{Path, State},
            http::{HeaderMap, header},
        };
        use base64::Engine;
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        let mut h = HeaderMap::new();
        h.insert("upload-length", content.len().to_string().parse().unwrap());
        h.insert("upload-metadata", format!("filename {},parentId {}", b64(name), b64(parent)).parse().unwrap());
        let res = crate::upload::create(State(self.st.clone()), user.clone(), h).await?;
        let upload = res.headers()[header::LOCATION].to_str().unwrap().rsplit('/').next().unwrap().to_string();
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_TYPE, "application/offset+octet-stream".parse().unwrap());
        h.insert("upload-offset", "0".parse().unwrap());
        let res = crate::upload::patch(State(self.st.clone()), user.clone(), Path(upload), h, axum::body::Body::from(content)).await?;
        Ok(res.headers()["x-node-id"].to_str().unwrap().to_string())
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

/// Where the local storage keeps a content
pub fn blob_file(env: &TestEnv, content: &[u8]) -> std::path::PathBuf {
    let hash = crate::util::sha256_hex(content);
    env.dir.join("blobs").join(&hash[0..2]).join(&hash[2..4]).join(&hash)
}
