use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex, RwLock},
};

use sqlx::SqlitePool;
use tokio::sync::Semaphore;

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

/// "Staging / deleting" registry for physical files, so background deletion doesn't need the global write lock and never deletes by mistake:
/// - Hashes being staged (uploaded, references not yet recorded) are skipped by background deletion
/// - For hashes being deleted, new staging waits until the deletion finishes before uploading
#[derive(Default)]
pub struct BlobGuard {
    pub staging: HashMap<String, u32>,
    pub deleting: HashMap<String, u32>,
}

pub struct Inner {
    pub db: SqlitePool,
    /// Connected storage locations: location id → backend
    pub storages: RwLock<HashMap<String, Arc<dyn Storage>>>,
    /// Moves of spaces to other storage locations that are running now (moves/)
    pub moves: crate::moves::Moves,
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
    /// Trust X-Forwarded-For sent by a reverse proxy
    pub trust_proxy: crate::auth::TrustProxy,
    pub max_upload: u64,
    /// SQLite allows only one writer; every write transaction takes this lock first, to avoid SQLITE_BUSY when a deferred transaction is upgraded.
    pub write_lock: tokio::sync::Mutex<()>,
    /// Uploads currently receiving a PATCH, so the same upload isn't written concurrently.
    pub active_uploads: Mutex<HashSet<String>>,
    /// Failed sign-in records: username → failure timestamps
    pub login_failures: Mutex<HashMap<String, Vec<i64>>>,
    /// Held while log archiving runs: the manual "archive now" and the daily run must not process the same rows
    pub archive_lock: tokio::sync::Mutex<()>,
    /// Last logged page view per "share|address": repeated views within a minute aren't logged again
    pub share_views: Mutex<HashMap<String, i64>>,
    /// Selections waiting to be downloaded through a short-lived link: link token → selection (see `downloads::store_download_link`)
    pub download_links: Mutex<HashMap<String, crate::downloads::DownloadLink>>,
    /// Compress and extract tasks running or recently finished, by id
    pub jobs: Mutex<HashMap<String, crate::archive::Job>>,
    /// Purge of deleted spaces' content: (running, asked to run again)
    pub detached_purge: (std::sync::atomic::AtomicBool, std::sync::atomic::AtomicBool),
    pub thumb_permits: Semaphore,
    /// Most memory one thumbnail may use to decode its image
    pub thumb_decode_bytes: u64,
    /// System settings (cached in memory; changes are also written to the settings table)
    pub system: RwLock<SystemSettings>,
    pub blob_guard: Mutex<BlobGuard>,
    /// Log retention and archive settings
    pub logs: RwLock<crate::logs::LogSettings>,
    /// Branding (site name, logo, colors, sign-in page text)
    pub branding: RwLock<crate::branding::Branding>,
    /// Connection status of storage locations (checked periodically in the background)
    pub location_health: Mutex<HashMap<String, LocationHealth>>,
    /// Third-party sign-in settings and sign-ins in progress
    pub sso: RwLock<crate::sso::SsoSettings>,
    pub sso_pending: crate::sso::PendingMap,
    /// Two-factor sign-in being set up from the account menu
    pub twofactor_setups: crate::twofactor::SetupMap,
    /// Sign-in and share-access events, written to the database in batches by one background task (see `logs::spawn_writer`)
    pub log_tx: tokio::sync::mpsc::Sender<crate::logs::LogEvent>,
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
    /// The storage location new personal spaces go on; blank = the default location at the time (personal.rs)
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

    /// Gets the backend of a storage location
    pub fn storage(&self, location: &str) -> crate::error::AppResult<Arc<dyn Storage>> {
        self.storages
            .read()
            .unwrap()
            .get(location)
            .cloned()
            .ok_or_else(|| crate::error::AppError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, format!("Storage location \"{location}\" is currently unavailable")))
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
