use std::{
    any::{Any, TypeId},
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, RwLock},
};

use sqlx::SqlitePool;

use crate::storage::Storage;

#[derive(Clone)]
pub struct AppState(pub Arc<Inner>);

/// Result of the most recent connection check of a storage location
#[derive(Debug, Clone, serde::Serialize)]
pub struct LocationHealth {
    pub ok: bool,
    pub error: Option<String>,
    pub checked_at: i64,
}

/// What the server holds that every feature uses. What a feature keeps for itself (caches, work in progress, its
/// settings) is its part, in `parts`.
pub struct Inner {
    pub db: SqlitePool,
    /// Connected storage locations: location id → backend
    pub storages: RwLock<HashMap<String, Arc<dyn Storage>>>,
    pub data_dir: PathBuf,
    /// Folder of the built-in `local` storage location
    pub storage_dir: PathBuf,
    /// Where new spaces of the built-in location get their folders (space_folders.rs): the storage folder, as an
    /// absolute path. None keeps new spaces in the content store (tests about it)
    pub space_folders: Option<PathBuf>,
    pub secret: Vec<u8>,
    pub secure_cookie: bool,
    /// Days before trashed items are deleted for good (0 = kept until the trash is emptied)
    pub trash_days: i64,
    pub max_upload: u64,
    /// SQLite allows only one writer; every write transaction takes this lock first, to avoid SQLITE_BUSY when a deferred transaction is upgraded.
    pub write_lock: tokio::sync::Mutex<()>,
    /// System settings (cached in memory; changes are also written to the settings table)
    pub system: RwLock<SystemSettings>,
    /// Connection status of storage locations (checked periodically in the background)
    pub location_health: Mutex<HashMap<String, LocationHealth>>,
    /// Asks the connection check of the storage locations to run now: a storage call failed with an error of the
    /// storage service (`usage::Metered`, `locations::spawn_health_monitor`)
    pub recheck: Arc<tokio::sync::Notify>,
    /// The features' parts
    pub parts: Parts,
}

/// What the features keep in memory for themselves (caches, work in progress, settings loaded at startup), one value
/// of each type: `shares::Memory`, `moves::Memory`… They are made by `app::startup::parts`, and a feature reaches its
/// own with `st.part::<Memory>()`. `AppState` holds them without naming them, as every feature depends on it.
#[derive(Default)]
pub struct Parts(HashMap<TypeId, Box<dyn Any + Send + Sync>>);

impl Parts {
    /// Adds a part; there is one of each type
    pub fn with<T: Any + Send + Sync>(mut self, part: T) -> Parts {
        let earlier = self.0.insert(TypeId::of::<T>(), Box::new(part));
        assert!(earlier.is_none(), "two parts of type {}", std::any::type_name::<T>());
        self
    }
}

/// What a server starts with: its settings, what startup loaded from the database, and the features' parts.
/// Everything else in `Inner` starts empty.
pub struct Setup {
    pub db: SqlitePool,
    pub storages: HashMap<String, Arc<dyn Storage>>,
    pub data_dir: PathBuf,
    pub storage_dir: PathBuf,
    pub space_folders: Option<PathBuf>,
    pub secret: Vec<u8>,
    pub secure_cookie: bool,
    pub trash_days: i64,
    pub max_upload: u64,
    pub system: SystemSettings,
    pub parts: Parts,
}

impl AppState {
    pub fn new(s: Setup) -> AppState {
        AppState(Arc::new(Inner {
            db: s.db,
            storages: RwLock::new(s.storages),
            data_dir: s.data_dir,
            storage_dir: s.storage_dir,
            space_folders: s.space_folders,
            secret: s.secret,
            secure_cookie: s.secure_cookie,
            trash_days: s.trash_days,
            max_upload: s.max_upload,
            write_lock: tokio::sync::Mutex::new(()),
            system: RwLock::new(s.system),
            location_health: Default::default(),
            recheck: Default::default(),
            parts: s.parts,
        }))
    }
}

#[derive(Debug, Clone)]
pub struct SystemSettings {
    /// Whether the "All files" shared space is enabled
    pub shared_enabled: bool,
    pub shared_root_id: String,
    /// Whether standard users can create team spaces themselves
    pub allow_user_drives: bool,
    /// Default capacity of new users' personal spaces (bytes, 0 = unlimited)
    pub default_user_quota: i64,
    /// New users get a personal space ("My files") unless the administrator or a sign-in domain rule says otherwise
    pub personal_spaces: bool,
    /// The storage location new personal spaces go on; blank = the default location at the time (personal/)
    pub personal_location: String,
    /// The site's public URL (e.g. https://drive.example.com), used to build share links; blank = use the browser's current URL
    pub public_url: String,
    /// Interface language for people who haven't picked one: "auto" (follow the browser), "en" or "zh-TW"
    pub default_lang: String,
    /// Folder spaces are scanned for changes made outside ThirtyFile this often (minutes, 0 = only by hand)
    pub scan_minutes: i64,
    /// Password sign-in needs a second factor: accounts without one set it up right after signing in
    pub require_two_factor: bool,
    /// Shortest password people may choose (at least `auth::MIN_PASSWORD`)
    pub min_password_length: usize,
    /// Public share links: new and changed links must have a password
    pub share_password_required: bool,
    /// Public share links: new and changed links must expire within this many days (0 = no limit)
    pub share_max_days: i64,
    /// Public share links can be created and opened; while off, existing links stop working (they aren't deleted)
    pub public_links: bool,
    /// Earlier versions kept per file (0 = none)
    pub version_keep: i64,
    /// Days an earlier version is kept after it was replaced (0 = no limit)
    pub version_days: i64,
    /// Moves of spaces to another storage location that run at the same time (moves/); the others wait their turn
    pub move_jobs: i64,
}

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl Inner {
    /// A feature's part (see `Parts`)
    pub fn part<T: Any>(&self) -> &T {
        let part = self.parts.0.get(&TypeId::of::<T>()).and_then(|p| p.downcast_ref());
        part.unwrap_or_else(|| panic!("AppState has no {}: add it in app::startup::parts", std::any::type_name::<T>()))
    }

    /// Returns the root folder id of the shared space when it is enabled
    pub fn shared_root(&self) -> Option<String> {
        let s = self.system.read().unwrap();
        s.shared_enabled.then(|| s.shared_root_id.clone())
    }
    /// Returns the reason when a storage location is offline (its settings couldn't be loaded, or the most recent connection check failed)
    pub fn location_offline(&self, location: &str) -> Option<String> {
        if !self.storages.read().unwrap().contains_key(location) {
            return Some("Storage location unavailable".into());
        }
        let health = self.location_health.lock().unwrap();
        health.get(location).filter(|h| !h.ok).map(|h| h.error.clone().unwrap_or_else(|| "Can't connect".into()))
    }

    /// `location_offline` as a person is told: the reason names hosts, addresses and folders, which only
    /// administrators see; everyone else learns that it can't be reached
    pub fn location_offline_for(&self, location: &str, admin: bool) -> Option<String> {
        let reason = self.location_offline(location)?;
        Some(if admin || reason == "Storage location unavailable" { reason } else { "Can't connect".into() })
    }

    /// Whether the site is served over HTTPS: THIRTYFILE_SECURE_COOKIE, or a Site URL that starts with https. Cookies are
    /// then marked Secure and browsers are told to use HTTPS only (HSTS).
    pub fn https(&self) -> bool {
        self.secure_cookie || self.system.read().unwrap().public_url.starts_with("https://")
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.data_dir.join("tmp")
    }
    pub fn thumb_path(&self, hash: &str) -> PathBuf {
        self.data_dir.join("thumbs").join(&hash[0..2]).join(format!("{hash}.jpg"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn each_feature_reaches_its_own_part() {
        #[derive(Default)]
        struct Counter(std::sync::atomic::AtomicU32);
        #[derive(Default)]
        struct Names(Mutex<Vec<String>>);
        let env = crate::testutil::env().await;
        let st = AppState::new(Setup {
            db: env.st.db.clone(),
            storages: HashMap::new(),
            data_dir: env.dir.clone(),
            storage_dir: env.dir.clone(),
            space_folders: None,
            secret: vec![7; 32],
            secure_cookie: false,
            trash_days: 30,
            max_upload: 0,
            system: env.st.system.read().unwrap().clone(),
            parts: Parts::default().with(Counter::default()).with(Names::default()),
        });
        st.part::<Counter>().0.fetch_add(2, std::sync::atomic::Ordering::Relaxed);
        st.part::<Names>().0.lock().unwrap().push("amy".into());
        assert_eq!(st.part::<Counter>().0.load(std::sync::atomic::Ordering::Relaxed), 2);
        assert_eq!(*st.part::<Names>().0.lock().unwrap(), ["amy"]);
        // A part nobody added is a mistake in app::startup::parts, and says so
        let missing = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| st.part::<String>().len())).unwrap_err();
        let message = missing.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(message.contains("AppState has no alloc::string::String"), "{message}");
    }

    #[test]
    #[should_panic(expected = "two parts of type")]
    fn there_is_one_part_of_each_type() {
        let _ = Parts::default().with(1u32).with(2u32);
    }
}
