//! Finding content in a location that nothing in ThirtyFile uses, and removing it once an administrator confirms.
//!
//! Content is unused when no file or version records it at this location (`blobs`), it isn't waiting to be deleted
//! anyway (`pending_blob_deletes`), no upload is storing it right now, and it was written more than a day ago:
//! uploads store their content before recording it. Removing re-checks each item right before deleting it, under the
//! write lock (`tree::remove_unreferenced`), so content used again meanwhile stays.

use std::{
    collections::{HashMap, HashSet},
    sync::{LazyLock, Mutex},
};

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};

use super::location;
use crate::{
    auth::{Admin, User},
    error::{AppError, AppResult},
    locations::{self, describe},
    logs,
    state::AppState,
    storage::{self, Storage},
    tree,
    util::{new_id, now},
};

/// Content written more recently than this is left alone: an upload may have stored it and not recorded it yet
pub const MARGIN: i64 = 24 * 3600;
/// Items the status lists (all of them are removed)
const SHOWN: usize = 500;

#[derive(Debug, Clone, Serialize)]
pub struct UnusedItem {
    pub hash: String,
    /// Its path in the location
    pub path: String,
    pub size: u64,
    pub modified: Option<i64>,
}

/// A search for unused content, and its removal once confirmed
#[derive(Debug, Clone, Serialize)]
pub struct Job {
    /// Identifies this search: removal is confirmed for it, not for a later one
    pub scan_id: String,
    /// scanning, found, removing, removed, failed
    pub phase: &'static str,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    /// Content seen in the location so far
    pub scanned: u64,
    /// Unused content found: count and bytes
    pub count: u64,
    pub bytes: u64,
    /// The first of them
    pub items: Vec<UnusedItem>,
    /// Unused content written within the last day, left alone
    pub recent: u64,
    pub error: Option<String>,
    /// Removal: removed (count and bytes), kept (used again, or written again meanwhile), failed
    pub removed: u64,
    pub removed_bytes: u64,
    pub kept: u64,
    pub failed: u64,
    #[serde(skip)]
    all: Vec<UnusedItem>,
}

impl Job {
    /// The job as the page sees it: without the whole list of what was found (the page polls it, and the list can hold
    /// hundreds of thousands of items)
    fn view(&self) -> Job {
        Job {
            scan_id: self.scan_id.clone(),
            phase: self.phase,
            started_at: self.started_at,
            finished_at: self.finished_at,
            scanned: self.scanned,
            count: self.count,
            bytes: self.bytes,
            items: self.items.clone(),
            recent: self.recent,
            error: self.error.clone(),
            removed: self.removed,
            removed_bytes: self.removed_bytes,
            kept: self.kept,
            failed: self.failed,
            all: Vec::new(),
        }
    }
}

/// Searches by location id (one per location, the latest)
static JOBS: LazyLock<Mutex<HashMap<String, Job>>> = LazyLock::new(Default::default);

fn update(id: &str, scan_id: &str, f: impl FnOnce(&mut Job)) {
    if let Some(job) = JOBS.lock().unwrap().get_mut(id).filter(|j| j.scan_id == scan_id) {
        f(job);
    }
}

pub async fn unused_status(_: Admin, Path(id): Path<String>) -> Json<Option<Job>> {
    Json(JOBS.lock().unwrap().get(&id).map(Job::view))
}

/// Starts a search in the background; its progress is read with `unused_status`
pub async fn find_unused(State(st): State<AppState>, _: Admin, Path(id): Path<String>) -> AppResult<Json<Job>> {
    let loc = location(&st, &id).await?;
    let backend = st.storage(&id)?;
    // Content in a place another location or installation uses isn't unused: it is theirs
    locations::require_own_place(&st, &id, &loc.kind, backend.as_ref()).await?;
    let job = Job {
        scan_id: new_id(),
        phase: "scanning",
        started_at: now(),
        finished_at: None,
        scanned: 0,
        count: 0,
        bytes: 0,
        items: Vec::new(),
        recent: 0,
        error: None,
        removed: 0,
        removed_bytes: 0,
        kept: 0,
        failed: 0,
        all: Vec::new(),
    };
    {
        let mut jobs = JOBS.lock().unwrap();
        if jobs.get(&id).is_some_and(|j| matches!(j.phase, "scanning" | "removing")) {
            return Err(AppError::conflict("This location is already being checked or cleaned up"));
        }
        jobs.insert(id.clone(), job.clone());
    }
    let scan_id = job.scan_id.clone();
    tokio::spawn(async move {
        let seen = |n: u64| update(&id, &scan_id, |j| j.scanned = n);
        let found = crate::usage::background(scan(&st, &id, backend.as_ref(), now() - MARGIN, &seen)).await;
        update(&id, &scan_id, |j| {
            j.finished_at = Some(now());
            match found {
                Ok((items, recent)) => {
                    j.phase = "found";
                    j.count = items.len() as u64;
                    j.bytes = items.iter().map(|i| i.size).sum();
                    j.items = items.iter().take(SHOWN).cloned().collect();
                    j.recent = recent;
                    j.all = items;
                }
                Err(e) => {
                    tracing::warn!("Finding unused content in storage location {id} failed: {}", e.message);
                    j.phase = "failed";
                    j.error = Some(e.message);
                }
            }
        });
    });
    Ok(Json(job))
}

/// The unused content of location `id` in `s` last written before `cutoff`, and how many more were written since
pub async fn scan(st: &AppState, id: &str, s: &dyn Storage, cutoff: i64, seen: &(dyn Fn(u64) + Send + Sync)) -> AppResult<(Vec<UnusedItem>, u64)> {
    let entries = s.list_content(seen).await.map_err(|e| AppError::new(StatusCode::BAD_GATEWAY, describe(&e)))?;
    seen(entries.len() as u64);
    let base = s.content_dir();
    let (mut out, mut recent) = (Vec::new(), 0);
    for chunk in entries.chunks(500) {
        let list = serde_json::to_string(&chunk.iter().map(|e| e.name.as_str()).collect::<Vec<_>>()).unwrap();
        let known: HashSet<String> = sqlx::query_as::<_, (String,)>(
            "SELECT hash FROM blobs WHERE location_id = ?2 AND hash IN (SELECT value FROM json_each(?1))
             UNION SELECT hash FROM pending_blob_deletes WHERE location_id = ?2 AND hash IN (SELECT value FROM json_each(?1))",
        )
        .bind(&list)
        .bind(id)
        .fetch_all(&st.db)
        .await?
        .into_iter()
        .map(|r| r.0)
        .collect();
        let staging: HashSet<String> = st.blob_guard.lock().unwrap().staging.keys().cloned().collect();
        for e in chunk {
            if known.contains(&e.name) || staging.contains(&e.name) {
                continue;
            }
            if e.modified.is_none_or(|m| m > cutoff) {
                recent += 1;
                continue;
            }
            let path = storage::join_key(&[base, &e.name[0..2], &e.name[2..4], &e.name]);
            out.push(UnusedItem { hash: e.name.clone(), path, size: e.size, modified: e.modified });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((out, recent))
}

#[derive(Deserialize)]
pub struct RemoveReq {
    /// The search whose list was confirmed
    scan_id: String,
}

/// Removes exactly the content the search `scan_id` found, in the background
pub async fn remove_unused(State(st): State<AppState>, Admin(user): Admin, Path(id): Path<String>, Json(req): Json<RemoveReq>) -> AppResult<Json<Job>> {
    let loc = location(&st, &id).await?;
    locations::require_own_place(&st, &id, &loc.kind, st.storage(&id)?.as_ref()).await?;
    let (job, items) = {
        let mut jobs = JOBS.lock().unwrap();
        let job = jobs.get_mut(&id).filter(|j| j.scan_id == req.scan_id && j.phase == "found").ok_or_else(|| {
            AppError::conflict("This list is out of date. Find unused content again.")
        })?;
        if job.count == 0 {
            return Err(AppError::bad_request("There is no unused content to remove"));
        }
        job.phase = "removing";
        job.finished_at = None;
        (job.view(), std::mem::take(&mut job.all))
    };
    let scan_id = job.scan_id.clone();
    tokio::spawn(async move {
        let progress = |r: Removal| {
            update(&id, &scan_id, |j| {
                (j.removed, j.removed_bytes, j.kept, j.failed) = (r.removed, r.bytes, r.kept, r.failed);
            })
        };
        let done = crate::usage::background(remove(&st, &id, &items, now() - MARGIN, &progress)).await;
        let logged = log_removal(&st, &user, &loc.name, done.removed).await;
        update(&id, &scan_id, |j| {
            j.phase = "removed";
            j.finished_at = Some(now());
            if let Err(e) = logged {
                j.error = Some(e.message);
            }
        });
    });
    Ok(Json(job))
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Removal {
    pub removed: u64,
    pub bytes: u64,
    pub kept: u64,
    pub failed: u64,
}

/// Removes the items one by one, each re-checked right before: still there and last written before `cutoff`, and
/// (under the write lock) still unused here and not being uploaded
pub async fn remove(st: &AppState, id: &str, items: &[UnusedItem], cutoff: i64, progress: &(dyn Fn(Removal) + Send + Sync)) -> Removal {
    let mut r = Removal::default();
    let Ok(s) = st.storage(id) else {
        r.failed = items.len() as u64;
        progress(r);
        return r;
    };
    for item in items {
        match s.stat(&item.path).await {
            // Gone already
            Ok(None) => {}
            Ok(Some(e)) if e.modified.is_some_and(|m| m <= cutoff) => {
                if !tree::remove_unreferenced(st, vec![(item.hash.clone(), id.to_string())]).await.is_empty() {
                    r.failed += 1;
                } else if matches!(s.stat(&item.path).await, Ok(None)) {
                    r.removed += 1;
                    r.bytes += item.size;
                } else {
                    // Used again, or being uploaded: left
                    r.kept += 1;
                }
            }
            // Written again since the search
            Ok(Some(_)) => r.kept += 1,
            Err(_) => r.failed += 1,
        }
        progress(r);
    }
    r
}

async fn log_removal(st: &AppState, user: &User, location: &str, removed: u64) -> AppResult<()> {
    if removed == 0 {
        return Ok(());
    }
    let detail = if removed == 1 { format!("{location}: {removed} unused item removed") } else { format!("{location}: {removed} unused items removed") };
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    logs::record_activity(&mut tx, user, None, "storage_cleanup", &detail).await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    /// Content stored in the local location without anything recording it, written `age` seconds ago
    async fn orphan(env: &testutil::TestEnv, content: &[u8], age: u64) -> (String, std::path::PathBuf) {
        let hash = crate::util::sha256_hex(content);
        let tmp = env.dir.join("tmp").join(new_id());
        std::fs::write(&tmp, content).unwrap();
        env.st.storage("local").unwrap().put_file(&hash, &tmp).await.unwrap();
        let file = testutil::blob_file(env, content);
        let f = std::fs::File::options().write(true).open(&file).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(age)).unwrap();
        (hash, file)
    }

    async fn find(env: &testutil::TestEnv) -> (Vec<UnusedItem>, u64) {
        let s = env.st.storage("local").unwrap();
        scan(&env.st, "local", s.as_ref(), now() - MARGIN, &|_| {}).await.unwrap()
    }

    #[tokio::test]
    async fn only_old_content_nothing_uses_is_found() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        env.stored_file(&admin, admin.root(), "kept.txt", b"in use").await;
        let (old, _) = orphan(&env, b"old orphan", 2 * 86400).await;
        orphan(&env, b"fresh orphan", 60).await;
        // Waiting to be deleted anyway
        let (pending, _) = orphan(&env, b"pending", 2 * 86400).await;
        sqlx::query("INSERT INTO pending_blob_deletes (hash, location_id, created_at) VALUES (?, 'local', 0)").bind(&pending).execute(&env.st.db).await.unwrap();
        // Recorded in another location: this copy is unused
        let (moved, _) = orphan(&env, b"moved away", 2 * 86400).await;
        sqlx::query("INSERT INTO blobs (hash, size, refcount, created_at, location_id) VALUES (?, 10, 1, 0, 'second')").bind(&moved).execute(&env.st.db).await.unwrap();
        // Other files in the folder aren't content
        std::fs::write(env.dir.join("blobs").join("notes.txt"), b"not content").unwrap();

        let (items, recent) = find(&env).await;
        let mut found: Vec<&str> = items.iter().map(|i| i.hash.as_str()).collect();
        found.sort();
        let mut want = vec![old.as_str(), moved.as_str()];
        want.sort();
        assert_eq!(found, want);
        assert_eq!(recent, 1, "the fresh one is left for a day");
        let item = items.iter().find(|i| i.hash == old).unwrap();
        assert_eq!(item.path, format!("{}/{}/{old}", &old[0..2], &old[2..4]));
        assert_eq!(item.size, 10);
    }

    #[tokio::test]
    async fn removal_checks_each_item_again_first() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let (_, gone_file) = orphan(&env, b"removed", 2 * 86400).await;
        let (used_again, used_file) = orphan(&env, b"used again", 2 * 86400).await;
        let (_, rewritten_file) = orphan(&env, b"written again", 2 * 86400).await;
        let (uploading, uploading_file) = orphan(&env, b"being uploaded", 2 * 86400).await;
        let (items, _) = find(&env).await;
        assert_eq!(items.len(), 4);

        // Meanwhile: a file uses one, one is written again, one is being uploaded
        let id = env.file(&admin, admin.root(), "again.txt").await;
        let mut c = env.st.db.acquire().await.unwrap();
        tree::add_blob_ref(&mut c, &used_again, 10, "local").await.unwrap();
        sqlx::query("UPDATE nodes SET blob_hash = ? WHERE id = ?").bind(&used_again).bind(&id).execute(&mut *c).await.unwrap();
        drop(c);
        std::fs::File::options().write(true).open(&rewritten_file).unwrap().set_modified(std::time::SystemTime::now()).unwrap();
        let guard = tree::stage_guard(&env.st, &uploading).await;

        let r = remove(&env.st, "local", &items, now() - MARGIN, &|_| {}).await;
        drop(guard);
        assert_eq!(r, Removal { removed: 1, bytes: 7, kept: 3, failed: 0 });
        assert!(!gone_file.exists());
        assert!(used_file.exists() && rewritten_file.exists() && uploading_file.exists());
    }

    #[tokio::test]
    async fn a_place_another_location_or_installation_uses_is_never_cleaned_up() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        // Works like a bucket (kind s3), its content in a folder of the test
        let id = format!("b{}", new_id());
        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, is_default, created_at) VALUES (?, 'Bucket', 's3', '{\"bucket\":\"b\"}', 0, 0)")
            .bind(&id)
            .execute(&env.st.db)
            .await
            .unwrap();
        let dir = env.dir.join(&id);
        env.st.storages.write().unwrap().insert(id.clone(), std::sync::Arc::new(crate::storage::LocalStorage::create(dir.clone(), &id).unwrap()));
        let marker = dir.join(storage::LOCATION_MARKER);
        // Another installation's marker (the place was set up by another ThirtyFile with a location of the same id)
        std::fs::write(&marker, format!("{id}\nanother-installation\n")).unwrap();
        let err = find_unused(State(env.st.clone()), Admin(admin.clone()), Path(id.clone())).await.unwrap_err();
        assert_eq!((err.status, err.message.as_str()), (StatusCode::CONFLICT, storage::PLACE_TAKEN));
        let req = RemoveReq { scan_id: "x".into() };
        let err = remove_unused(State(env.st.clone()), Admin(admin.clone()), Path(id.clone()), Json(req)).await.unwrap_err();
        assert_eq!(err.message, storage::PLACE_TAKEN);
        // Its own marker, without the installation (written before it was added): completed, and the search runs
        std::fs::write(&marker, &id).unwrap();
        let _ = find_unused(State(env.st.clone()), Admin(admin.clone()), Path(id.clone())).await.unwrap();
        let install = locations::install_id(&env.st).await.unwrap();
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), format!("{id}\n{install}\n"));
    }

    #[tokio::test]
    async fn removing_is_confirmed_for_one_search_and_logged() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let (_, file) = orphan(&env, b"left behind", 2 * 86400).await;
        let Json(job) = find_unused(State(env.st.clone()), Admin(admin.clone()), Path("local".into())).await.unwrap();
        let wait = |phase: &'static str| {
            let admin = admin.clone();
            async move {
            for _ in 0..200 {
                if let Json(Some(j)) = unused_status(Admin(admin.clone()), Path("local".into())).await
                    && j.phase == phase
                {
                    return j;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            panic!("never reached {phase}");
            }
        };
        let found = wait("found").await;
        assert_eq!((found.count, found.bytes, found.items.len()), (1, 11, 1));

        // Another search's list can't be confirmed
        let stale = RemoveReq { scan_id: "someone else's".into() };
        let e = remove_unused(State(env.st.clone()), Admin(admin.clone()), Path("local".into()), Json(stale)).await.unwrap_err();
        assert_eq!(e.status, StatusCode::CONFLICT);
        assert!(file.exists());

        let req = RemoveReq { scan_id: job.scan_id.clone() };
        let _ = remove_unused(State(env.st.clone()), Admin(admin.clone()), Path("local".into()), Json(req)).await.unwrap();
        let done = wait("removed").await;
        assert_eq!((done.removed, done.removed_bytes), (1, 11));
        assert!(!file.exists());
        let (detail,): (String,) = sqlx::query_as("SELECT detail FROM activity WHERE action = 'storage_cleanup'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(detail, "Local disk: 1 unused item removed");
    }
}
