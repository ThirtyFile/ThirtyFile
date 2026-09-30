use std::{
    path::{Path as FsPath, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::SeqCst},
    },
};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use futures_util::future::BoxFuture;
use serde_json::json;

use super::{
    api,
    runner::{self, Control},
};
use crate::{
    auth::{Admin, User},
    storage::{self, LocalStorage, Storage},
    testutil::{self, TestEnv},
};

// ───────────── Helpers ─────────────

/// A Local folder location (a NAS, say) with its folder in the test's folder
async fn add_nas(env: &TestEnv, id: &str) -> PathBuf {
    let dir = env.dir.join(id);
    add_location(env, id, "local", json!({ "path": dir.to_string_lossy() }), Arc::new(LocalStorage::create(dir.clone(), id).unwrap())).await;
    dir
}

/// A location of any kind, connected to `backend`
async fn add_location(env: &TestEnv, id: &str, kind: &str, config: serde_json::Value, backend: Arc<dyn Storage>) {
    sqlx::query("INSERT INTO storage_locations (id, name, kind, config, is_default, created_at) VALUES (?, ?, ?, ?, 0, 0)")
        .bind(id)
        .bind(id.to_uppercase())
        .bind(kind)
        .bind(config.to_string())
        .execute(&env.st.db)
        .await
        .unwrap();
    env.st.storages.write().unwrap().insert(id.into(), backend);
}

/// Copies everything on `source` to `dest`: (set, job)
async fn copy_all(env: &TestEnv, source: &str, dest: &str) -> (String, String) {
    let req = serde_json::from_value(json!({ "source": source, "dest": dest })).unwrap();
    let Json(v) = api::copy(State(env.st.clone()), Admin(env.admin().await), Json(req)).await.unwrap();
    (v["set_id"].as_str().unwrap().to_string(), v["job_id"].as_str().unwrap().to_string())
}

/// Takes a queued job as the runner does
async fn take_job(env: &TestEnv, id: &str) -> (runner::Job, Arc<Control>) {
    let job = runner::job(&mut env.st.db.acquire().await.unwrap(), id).await.unwrap().unwrap();
    let ctl = runner::take(&env.st, &job).await.unwrap().expect("queued");
    (job, ctl)
}

/// Runs a queued job to its end, as the runner does; returns its state
async fn run_job(env: &TestEnv, id: &str) -> String {
    let (job, ctl) = take_job(env, id).await;
    runner::run(&env.st, &job, &ctl).await;
    state(env, id).await.0
}

async fn state(env: &TestEnv, id: &str) -> (String, Option<String>) {
    sqlx::query_as("SELECT state, error FROM backup_jobs WHERE id = ?").bind(id).fetch_one(&env.st.db).await.unwrap()
}

/// The newest job of a kind for a set
async fn job_of(env: &TestEnv, set: &str, kind: &str) -> Option<String> {
    sqlx::query_as::<_, (String,)>("SELECT id FROM backup_jobs WHERE set_id = ? AND kind = ? ORDER BY created_at DESC, rowid DESC LIMIT 1")
        .bind(set)
        .bind(kind)
        .fetch_optional(&env.st.db)
        .await
        .unwrap()
        .map(|r| r.0)
}

async fn snapshot_of(env: &TestEnv, set: &str) -> String {
    sqlx::query_as::<_, (String,)>("SELECT id FROM backup_snapshots WHERE set_id = ? AND state = 'complete'").bind(set).fetch_one(&env.st.db).await.unwrap().0
}

/// Queues a restore of a snapshot's space (into `target`, else where it goes by default)
async fn restore(env: &TestEnv, snapshot: &str, space: &str, target: Option<&str>, trash: bool) -> crate::error::AppResult<String> {
    let req = serde_json::from_value(json!({ "space": space, "target_drive": target, "trash": trash })).unwrap();
    let Json(v) = api::restore(State(env.st.clone()), Admin(env.admin().await), Path(snapshot.to_string()), Json(req)).await?;
    Ok(v["job_id"].as_str().unwrap().to_string())
}

/// What a folder holds (not in the trash): name → (id, kind)
async fn children(env: &TestEnv, parent: &str) -> std::collections::BTreeMap<String, (String, String)> {
    sqlx::query_as::<_, (String, String, String)>("SELECT name, id, kind FROM nodes WHERE parent_id = ? AND trashed_at IS NULL")
        .bind(parent)
        .fetch_all(&env.st.db)
        .await
        .unwrap()
        .into_iter()
        .map(|(n, id, k)| (n, (id, k)))
        .collect()
}

/// The folder a restore made
async fn restored_folder(env: &TestEnv, parent: &str) -> String {
    let found: Vec<(String, (String, String))> = children(env, parent).await.into_iter().filter(|(n, _)| n.starts_with("Restored ")).collect();
    assert_eq!(found.len(), 1, "one restored folder in {parent}");
    found[0].1.0.clone()
}

/// The item at a path below a folder
async fn at(env: &TestEnv, top: &str, path: &str) -> Option<String> {
    let mut id = top.to_string();
    for part in path.split('/') {
        id = children(env, &id).await.get(part)?.0.clone();
    }
    Some(id)
}

async fn read(env: &TestEnv, user: &User, id: &str) -> Vec<u8> {
    let q = Query(serde_json::from_value(json!({})).unwrap());
    let res = crate::files::content(State(env.st.clone()), user.clone(), Path(id.to_string()), q, HeaderMap::new()).await.unwrap();
    axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec()
}

async fn save(env: &TestEnv, user: &User, id: &str, body: &'static [u8]) {
    let _ = crate::files::save_content(State(env.st.clone()), user.clone(), Path(id.to_string()), HeaderMap::new(), axum::body::Bytes::from_static(body))
        .await
        .unwrap();
}

async fn trash(env: &TestEnv, user: &User, id: &str) {
    let req = serde_json::from_value(json!({ "ids": [id] })).unwrap();
    let _ = crate::nodes::trash(State(env.st.clone()), user.clone(), Json(req)).await.unwrap();
}

/// Deletes a file for good, and its content once nothing uses it
async fn purge(env: &TestEnv, user: &User, id: &str) {
    trash(env, user, id).await;
    let req = serde_json::from_value(json!({ "ids": [id] })).unwrap();
    let Json(job) = crate::nodes::delete_forever(State(env.st.clone()), user.clone(), Json(req)).await.unwrap();
    if job.state == "running" {
        crate::jobs::wait_for(&env.st, &job.id).await;
    }
    delete_due(env, "local").await;
}

/// Deletions that are waiting are made due, and done
async fn delete_due(env: &TestEnv, location: &str) {
    sqlx::query("UPDATE pending_blob_deletes SET created_at = 0 WHERE location_id = ?").bind(location).execute(&env.st.db).await.unwrap();
    crate::tree::retry_pending_deletes(&env.st, location).await;
}

/// Files below a folder, recursively
fn files_below(dir: &FsPath) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(read) = std::fs::read_dir(dir) else { return out };
    for e in read.flatten() {
        if e.path().is_dir() {
            out.extend(files_below(&e.path()));
        } else {
            out.push(e.path());
        }
    }
    out
}

/// A storage that calls a hook each time something is stored with `put_at`, to do things while a job runs
struct Hooked {
    inner: Arc<dyn Storage>,
    on_put: Box<dyn Fn(String) -> BoxFuture<'static, ()> + Send + Sync>,
}

impl Storage for Hooked {
    fn put_file<'a>(&'a self, hash: &'a str, src: &'a FsPath) -> BoxFuture<'a, std::io::Result<()>> {
        self.inner.put_file(hash, src)
    }
    fn open<'a>(&'a self, hash: &'a str, start: u64, len: u64) -> BoxFuture<'a, std::io::Result<storage::BoxReader>> {
        self.inner.open(hash, start, len)
    }
    fn delete<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, std::io::Result<()>> {
        self.inner.delete(hash)
    }
    fn check(&self) -> BoxFuture<'_, std::io::Result<()>> {
        self.inner.check()
    }
    fn ping(&self) -> BoxFuture<'_, std::io::Result<()>> {
        self.inner.ping()
    }
    fn content_dir(&self) -> &'static str {
        self.inner.content_dir()
    }
    fn list_dir<'a>(&'a self, dir: &'a str) -> BoxFuture<'a, std::io::Result<Vec<storage::Entry>>> {
        self.inner.list_dir(dir)
    }
    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, std::io::Result<Option<storage::Entry>>> {
        self.inner.stat(key)
    }
    fn put_at<'a>(&'a self, key: &'a str, src: &'a FsPath) -> BoxFuture<'a, std::io::Result<()>> {
        Box::pin(async move {
            (self.on_put)(key.to_string()).await;
            self.inner.put_at(key, src).await
        })
    }
    fn open_at<'a>(&'a self, key: &'a str, start: u64, len: u64) -> BoxFuture<'a, std::io::Result<storage::BoxReader>> {
        self.inner.open_at(key, start, len)
    }
    fn delete_at<'a>(&'a self, key: &'a str) -> BoxFuture<'a, std::io::Result<()>> {
        self.inner.delete_at(key)
    }
}

/// Puts a hook on a location's `put_at` of content (objects); the hook hears how many were stored so far
fn on_object_put(env: &TestEnv, location: &str, hook: impl Fn(usize) -> BoxFuture<'static, ()> + Send + Sync + 'static) {
    let inner = env.st.storages.read().unwrap().get(location).cloned().unwrap();
    let puts = Arc::new(AtomicUsize::new(0));
    let hooked = Hooked {
        inner,
        on_put: Box::new(move |key| {
            if key.contains("/objects/") {
                let n = puts.fetch_add(1, SeqCst) + 1;
                hook(n)
            } else {
                Box::pin(async {})
            }
        }),
    };
    env.st.storages.write().unwrap().insert(location.into(), Arc::new(hooked));
}

/// Pauses or cancels a job once `at` objects were stored
fn stop_at_put(env: &TestEnv, location: &str, ctl: &Arc<Control>, at: usize, cancel: bool) {
    let ctl = ctl.clone();
    on_object_put(env, location, move |n| {
        if n == at {
            (if cancel { &ctl.cancel } else { &ctl.pause }).store(true, SeqCst);
        }
        Box::pin(async {})
    });
}

async fn blob_location(env: &TestEnv, content: &[u8]) -> Option<String> {
    sqlx::query_as::<_, (String,)>("SELECT location_id FROM blobs WHERE hash = ?")
        .bind(crate::util::sha256_hex(content))
        .fetch_optional(&env.st.db)
        .await
        .unwrap()
        .map(|r| r.0)
}

/// Objects a set keeps in a Local folder location's folder
fn objects_in(dir: &FsPath, set: &str) -> Vec<PathBuf> {
    files_below(&dir.join(super::layout::ROOT).join(set).join("objects"))
}

// ───────────── Copies ─────────────

#[tokio::test]
async fn everything_on_a_location_is_copied_and_a_space_restored_from_it_after_its_files_are_gone() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let amy = env.user("amy", true).await;
    let docs = env.folder(&amy, amy.root(), "Docs").await;
    let deep = env.folder(&amy, &docs, "Deep").await;
    env.folder(&amy, amy.root(), "Empty").await;
    let a = env.upload(&amy, &deep, "a.txt", b"alpha").await;
    save(&env, &amy, &a, b"alpha, edited").await;
    let old = env.upload(&amy, amy.root(), "old.txt", b"thrown away").await;
    trash(&env, &amy, &old).await;
    // The same content in the company space: stored once, copied once
    let company = env.st.system.read().unwrap().shared_root_id.clone();
    env.upload(&admin, &company, "plan.txt", b"alpha").await;
    let nas = add_nas(&env, "nas").await;
    let (default_before,): (String,) = sqlx::query_as("SELECT id FROM storage_locations WHERE is_default = 1").fetch_one(&env.st.db).await.unwrap();

    let (set, job) = copy_all(&env, "local", "nas").await;
    assert_eq!(run_job(&env, &job).await, "done", "{:?}", state(&env, &job).await);
    // The source is as it was: its content, its spaces' location, the default
    assert_eq!(blob_location(&env, b"alpha").await.as_deref(), Some("local"));
    let (on_local,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM drives WHERE location_id = 'local'").fetch_one(&env.st.db).await.unwrap();
    assert!(on_local >= 2);
    let (default_after,): (String,) = sqlx::query_as("SELECT id FROM storage_locations WHERE is_default = 1").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(default_before, default_after);
    assert!(testutil::blob_file(&env, b"alpha").is_file());
    // Each content once: alpha, the edited alpha, the trashed file
    assert_eq!(objects_in(&nas, &set).len(), 3);
    let snapshot = snapshot_of(&env, &set).await;
    let root = nas.join(super::layout::ROOT).join(&set);
    assert!(root.join("set.json").is_file());
    assert!(root.join("snapshots").join(&snapshot).join("manifest.jsonl").is_file());
    assert!(root.join("snapshots").join(&snapshot).join("complete.json").is_file());
    let (files, versions): (i64, i64) = sqlx::query_as("SELECT files, versions FROM backup_snapshots WHERE id = ?").bind(&snapshot).fetch_one(&env.st.db).await.unwrap();
    assert_eq!((files, versions), (3, 1), "a.txt, old.txt in the trash, plan.txt; one earlier version");

    // Amy's files go for good, content and all
    purge(&env, &amy, &a).await;
    assert!(!testutil::blob_file(&env, b"alpha, edited").exists());
    // Restored into her space, in a new folder, from the copy
    let mine = env.drive_of(amy.root()).await;
    let job = restore(&env, &snapshot, &mine, None, false).await.unwrap();
    assert_eq!(run_job(&env, &job).await, "done", "{:?}", state(&env, &job).await);
    let top = restored_folder(&env, amy.root()).await;
    let file = at(&env, &top, "Docs/Deep/a.txt").await.expect("restored");
    assert_eq!(read(&env, &amy, &file).await, b"alpha, edited");
    assert!(at(&env, &top, "Empty").await.is_some(), "empty folders too");
    assert!(at(&env, &top, "old.txt").await.is_none(), "not what was in the trash");
    let (owner,): (i64,) = sqlx::query_as("SELECT owner_id FROM nodes WHERE id = ?").bind(&file).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(owner, amy.id);
    // With the trash, into a folder of its own again
    let job = restore(&env, &snapshot, &mine, None, true).await.unwrap();
    assert_eq!(run_job(&env, &job).await, "done");
    let restored: Vec<String> = children(&env, amy.root()).await.into_keys().filter(|n| n.starts_with("Restored ")).collect();
    assert_eq!(restored.len(), 2, "{restored:?}");
    let second = children(&env, amy.root()).await.get(&restored[1]).unwrap().0.clone();
    let old = at(&env, &second, "old.txt").await.expect("the trash asked for");
    assert_eq!(read(&env, &amy, &old).await, b"thrown away");
}

#[tokio::test]
async fn a_personal_space_is_only_restored_into_its_owners_and_its_items_are_never_named() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let amy = env.user("amy", true).await;
    let secret = env.upload(&amy, amy.root(), "secret-plans.txt", b"amy's").await;
    let company = env.st.system.read().unwrap().shared_root_id.clone();
    env.upload(&admin, &company, "agenda.txt", b"the company's").await;
    add_nas(&env, "nas").await;
    // Amy's content is damaged where it is kept: the copy fails, without naming her file
    std::fs::write(testutil::blob_file(&env, b"amy's"), b"AMY'S").unwrap();
    let (set, job) = copy_all(&env, "local", "nas").await;
    assert_eq!(run_job(&env, &job).await, "failed");
    let (failures,): (String,) = sqlx::query_as("SELECT failures FROM backup_jobs WHERE id = ?").bind(&job).fetch_one(&env.st.db).await.unwrap();
    assert!(!failures.contains("secret-plans"), "{failures}");
    assert!(failures.contains("damaged"), "{failures}");
    assert_eq!(sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM backup_snapshots WHERE state = 'complete'").fetch_one(&env.st.db).await.unwrap().0, 0);
    // Repaired: the copy is tried again and completes
    std::fs::write(testutil::blob_file(&env, b"amy's"), b"amy's").unwrap();
    let Json(_) = api::resume(State(env.st.clone()), Admin(admin.clone()), Path(job.clone())).await.unwrap();
    assert_eq!(run_job(&env, &job).await, "done");
    let snapshot = snapshot_of(&env, &set).await;
    let mine = env.drive_of(amy.root()).await;
    let company_drive = env.drive_of(&company).await;
    // Not into the company space, whatever is asked
    let e = restore(&env, &snapshot, &mine, Some(&company_drive), false).await.unwrap_err();
    assert!(e.message.contains("owner's personal space"), "{}", e.message);
    let req = serde_json::from_value(json!({ "space": mine })).unwrap();
    let Json(preview) = api::restore_preview(State(env.st.clone()), Admin(admin.clone()), Path(snapshot.clone()), Json(req)).await.unwrap();
    let preview = serde_json::to_value(&preview).unwrap();
    assert_eq!(preview["targets"].as_array().unwrap().len(), 1, "{preview}");
    assert_eq!(preview["target_drive"], json!(mine));
    // Restoring into her space works, and nothing names her file in the list of jobs
    let job = restore(&env, &snapshot, &mine, None, false).await.unwrap();
    assert_eq!(run_job(&env, &job).await, "done");
    let Json(list) = api::list(State(env.st.clone()), Admin(admin.clone())).await.unwrap();
    assert!(!serde_json::to_string(&list).unwrap().contains("secret-plans"));
    let _ = secret;
}

#[tokio::test]
async fn content_deleted_while_a_copy_is_being_made_is_kept_until_it_is_copied() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let mut ids = Vec::new();
    for i in 0..6 {
        let body: &'static [u8] = format!("content {i}").into_bytes().leak();
        ids.push((env.upload(&amy, amy.root(), &format!("f{i}.txt"), body).await, body));
    }
    add_nas(&env, "nas").await;
    let (set, job) = copy_all(&env, "local", "nas").await;
    // Once the first content is stored, every file goes for good, content and all
    let (st, amy2, ids2) = (env.st.clone(), amy.clone(), ids.clone());
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = done.clone();
    let env_dir = env.dir.clone();
    on_object_put(&env, "nas", move |n| {
        let (st, amy, ids, once, _dir) = (st.clone(), amy2.clone(), ids2.clone(), once.clone(), env_dir.clone());
        Box::pin(async move {
            if n != 1 || once.swap(true, SeqCst) {
                return;
            }
            for (id, _) in &ids {
                let req = serde_json::from_value(json!({ "ids": [id] })).unwrap();
                let _ = crate::nodes::trash(State(st.clone()), amy.clone(), Json(req)).await.unwrap();
                let req = serde_json::from_value(json!({ "ids": [id] })).unwrap();
                let Json(job) = crate::nodes::delete_forever(State(st.clone()), amy.clone(), Json(req)).await.unwrap();
                if job.state == "running" {
                    crate::jobs::wait_for(&st, &job.id).await;
                }
            }
            sqlx::query("UPDATE pending_blob_deletes SET created_at = 0").execute(&st.db).await.unwrap();
            crate::tree::retry_pending_deletes(&st, "local").await;
        })
    });
    assert_eq!(run_job(&env, &job).await, "done", "{:?}", state(&env, &job).await);
    assert!(done.load(SeqCst));
    // The snapshot holds all six, as they were when the spaces were read
    let snapshot = snapshot_of(&env, &set).await;
    let (files,): (i64,) = sqlx::query_as("SELECT files FROM backup_snapshots WHERE id = ?").bind(&snapshot).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(files, 6);
    assert_eq!(objects_in(&env.dir.join("nas"), &set).len(), 6);
    // Now nothing keeps the content any more
    for (_, body) in &ids {
        assert!(testutil::blob_file(&env, body).exists(), "kept until copied");
    }
    delete_due(&env, "local").await;
    for (_, body) in &ids {
        assert!(!testutil::blob_file(&env, body).exists(), "deleted once copied");
    }
    // And they come back from the copy
    let mine = env.drive_of(amy.root()).await;
    let job = restore(&env, &snapshot, &mine, None, false).await.unwrap();
    assert_eq!(run_job(&env, &job).await, "done");
    let top = restored_folder(&env, amy.root()).await;
    for (i, (_, body)) in ids.iter().enumerate() {
        let f = at(&env, &top, &format!("f{i}.txt")).await.unwrap();
        assert_eq!(read(&env, &amy, &f).await, *body);
    }
}

#[tokio::test]
async fn a_paused_copy_continues_where_it_stopped_also_after_a_restart_and_a_cancelled_one_is_removed() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    for i in 0..8 {
        let body: &'static [u8] = format!("file {i}").into_bytes().leak();
        env.upload(&amy, amy.root(), &format!("f{i}.txt"), body).await;
    }
    let nas = add_nas(&env, "nas").await;
    let admin = env.admin().await;
    let (set, job) = copy_all(&env, "local", "nas").await;
    let (j, ctl) = take_job(&env, &job).await;
    stop_at_put(&env, "nas", &ctl, 3, false);
    runner::run(&env.st, &j, &ctl).await;
    assert_eq!(state(&env, &job).await.0, "paused");
    assert_eq!(objects_in(&nas, &set).len(), 3);
    // ThirtyFile restarts: a paused job stays paused, a running one waits for its turn again
    runner::recover(&env.st).await.unwrap();
    assert_eq!(state(&env, &job).await.0, "paused");
    let Json(_) = api::resume(State(env.st.clone()), Admin(admin.clone()), Path(job.clone())).await.unwrap();
    sqlx::query("UPDATE backup_jobs SET state = 'running' WHERE id = ?").bind(&job).execute(&env.st.db).await.unwrap();
    runner::recover(&env.st).await.unwrap();
    assert_eq!(state(&env, &job).await.0, "queued");
    let (j, ctl) = take_job(&env, &job).await;
    let copied = Arc::new(AtomicUsize::new(0));
    let c = copied.clone();
    on_object_put(&env, "nas", move |_| {
        c.fetch_add(1, SeqCst);
        Box::pin(async {})
    });
    runner::run(&env.st, &j, &ctl).await;
    assert_eq!(state(&env, &job).await.0, "done");
    assert_eq!(copied.load(SeqCst), 5, "only what wasn't copied before");
    assert_eq!(objects_in(&nas, &set).len(), 8);

    // Another copy, cancelled halfway: what it wrote goes, the source stays
    let (set2, job2) = copy_all(&env, "local", "nas").await;
    let (j, ctl) = take_job(&env, &job2).await;
    stop_at_put(&env, "nas", &ctl, 2, true);
    runner::run(&env.st, &j, &ctl).await;
    assert_eq!(state(&env, &job2).await.0, "cancelled");
    let remove = job_of(&env, &set2, "remove").await.expect("the copy is removed");
    assert_eq!(run_job(&env, &remove).await, "done");
    assert!(files_below(&nas.join(super::layout::ROOT).join(&set2)).is_empty());
    assert!(sqlx::query_as::<_, (i64,)>("SELECT 1 FROM backup_sets WHERE id = ?").bind(&set2).fetch_optional(&env.st.db).await.unwrap().is_none());
    assert_eq!(objects_in(&nas, &set).len(), 8, "the other copy stays");
    assert!(testutil::blob_file(&env, b"file 0").is_file(), "the source stays");
    // A removal can't be cancelled
    let Json(_) = api::delete(State(env.st.clone()), Admin(admin.clone()), Path(set.clone())).await.unwrap();
    let remove = job_of(&env, &set, "remove").await.unwrap();
    assert!(api::cancel(State(env.st.clone()), Admin(admin.clone()), Path(remove.clone())).await.is_err());
    assert_eq!(run_job(&env, &remove).await, "done");
    assert!(files_below(&nas.join(super::layout::ROOT)).is_empty());
}

#[tokio::test]
async fn a_copy_to_a_location_that_cant_be_reached_fails_and_can_be_resumed() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    env.upload(&amy, amy.root(), "a.txt", b"a").await;
    let nas = add_nas(&env, "nas").await;
    let (set, job) = copy_all(&env, "local", "nas").await;
    // The disk isn't mounted: its folder (and marker) is gone
    std::fs::rename(&nas, env.dir.join("unplugged")).unwrap();
    assert_eq!(run_job(&env, &job).await, "failed");
    assert!(state(&env, &job).await.1.unwrap().contains("can't be reached"));
    std::fs::rename(env.dir.join("unplugged"), &nas).unwrap();
    let Json(_) = api::resume(State(env.st.clone()), Admin(env.admin().await), Path(job.clone())).await.unwrap();
    assert_eq!(run_job(&env, &job).await, "done");
    assert_eq!(objects_in(&nas, &set).len(), 1);
}

#[tokio::test]
async fn cleaning_up_a_location_never_touches_the_copies_it_holds() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let a = env.upload(&amy, amy.root(), "a.txt", b"kept in a copy").await;
    let nas = add_nas(&env, "nas").await;
    let (set, job) = copy_all(&env, "local", "nas").await;
    assert_eq!(run_job(&env, &job).await, "done");
    let objects = objects_in(&nas, &set);
    assert_eq!(objects.len(), 1);
    // Old enough to be taken for unused content, if anything looked
    let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(10 * 86400);
    for f in files_below(&nas.join(super::layout::ROOT)) {
        std::fs::File::options().write(true).open(&f).unwrap().set_modified(long_ago).unwrap();
    }
    let s = env.st.storage("nas").unwrap();
    let (unused, _) = crate::location_tools::unused_scan(&env.st, "nas", s.as_ref(), crate::util::now(), &|_| {}).await.unwrap();
    assert!(unused.is_empty(), "{unused:?}");
    // A deletion of the same content on that location (as after a move away from it) deletes the content store's copy
    // only
    let hash = crate::util::sha256_hex(b"kept in a copy");
    crate::tree::defer_blob_removal(&env.st, &[(hash.clone(), "nas".to_string())], 0).await;
    delete_due(&env, "nas").await;
    assert!(objects[0].is_file());
    // The source's own deletions leave it too
    purge(&env, &amy, &a).await;
    assert!(objects[0].is_file());
    // Browsing the location doesn't open the copies (their lists name personal spaces' files)
    let q = serde_json::from_value(json!({ "path": super::layout::ROOT })).unwrap();
    let e = crate::location_tools::browse(State(env.st.clone()), Admin(env.admin().await), Path("nas".to_string()), Query(q)).await.unwrap_err();
    assert_eq!(e.status, axum::http::StatusCode::FORBIDDEN);
    let manifest = format!("{}/{set}/snapshots/{}/manifest.jsonl", super::layout::ROOT.to_uppercase(), snapshot_of(&env, &set).await);
    let q = serde_json::from_value(json!({ "path": manifest })).unwrap();
    let e = crate::location_tools::download(State(env.st.clone()), Admin(env.admin().await), Path("nas".to_string()), Query(q)).await.unwrap_err();
    assert_eq!(e.status, axum::http::StatusCode::FORBIDDEN);
    // The location can't be deleted while it holds the copy
    let e = crate::locations::delete(State(env.st.clone()), Admin(env.admin().await), Path("nas".to_string())).await.unwrap_err();
    assert!(e.message.contains("holds a copy"), "{}", e.message);
}

#[tokio::test]
async fn locations_in_the_same_place_or_inside_each_other_are_refused() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    env.upload(&amy, amy.root(), "a.txt", b"a").await;
    // A folder inside the built-in storage's folder
    let inside = env.dir.join("blobs").join("nested");
    add_location(&env, "inside", "local", json!({ "path": inside.to_string_lossy() }), Arc::new(LocalStorage::create(inside.clone(), "inside").unwrap())).await;
    let req = serde_json::from_value(json!({ "source": "local", "dest": "inside" })).unwrap();
    let e = api::copy(State(env.st.clone()), Admin(env.admin().await), Json(req)).await.unwrap_err();
    assert!(e.message.contains("inside the other"), "{}", e.message);
    let req = serde_json::from_value(json!({ "source": "local", "dest": "local" })).unwrap();
    assert!(api::copy(State(env.st.clone()), Admin(env.admin().await), Json(req)).await.is_err());
    // Two buckets of one service: allowed, and said to fail together
    add_location(&env, "b1", "s3", json!({ "endpoint": "https://s3.example.com", "bucket": "one" }), Arc::new(storage::S3Storage::in_memory(""))).await;
    add_location(&env, "b2", "s3", json!({ "endpoint": "https://s3.example.com", "bucket": "two" }), Arc::new(storage::S3Storage::in_memory(""))).await;
    add_location(&env, "b3", "s3", json!({ "endpoint": "https://s3.example.com", "bucket": "one", "prefix": "sub" }), Arc::new(storage::S3Storage::in_memory(""))).await;
    assert_eq!(crate::locations::relation(&env.st, "b1", "b2").await.unwrap(), crate::locations::Relation::Shared);
    assert_eq!(crate::locations::relation(&env.st, "b1", "b3").await.unwrap(), crate::locations::Relation::Nested);
    add_nas(&env, "nas").await;
    assert_ne!(crate::locations::relation(&env.st, "local", "nas").await.unwrap(), crate::locations::Relation::Nested);
    assert_eq!(crate::locations::relation(&env.st, "nas", "b1").await.unwrap(), crate::locations::Relation::Apart);
}

#[tokio::test]
async fn a_copy_can_be_checked_and_what_is_missing_or_damaged_is_found() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    env.upload(&amy, amy.root(), "a.txt", b"first").await;
    env.upload(&amy, amy.root(), "b.txt", b"second").await;
    let nas = add_nas(&env, "nas").await;
    let (set, job) = copy_all(&env, "local", "nas").await;
    assert_eq!(run_job(&env, &job).await, "done");
    let admin = env.admin().await;
    let Json(v) = api::verify(State(env.st.clone()), Admin(admin.clone()), Path(set.clone())).await.unwrap();
    let check = v["job_id"].as_str().unwrap().to_string();
    assert_eq!(run_job(&env, &check).await, "done");
    let (verified,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM backup_objects WHERE set_id = ? AND verified_at IS NOT NULL").bind(&set).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(verified, 2);
    let objects = objects_in(&nas, &set);
    std::fs::write(&objects[0], b"xxxxx").unwrap();
    std::fs::remove_file(&objects[1]).unwrap();
    let Json(v) = api::verify(State(env.st.clone()), Admin(admin), Path(set.clone())).await.unwrap();
    let check = v["job_id"].as_str().unwrap().to_string();
    assert_eq!(run_job(&env, &check).await, "failed");
    let (failures,): (String,) = sqlx::query_as("SELECT failures FROM backup_jobs WHERE id = ?").bind(&check).fetch_one(&env.st.db).await.unwrap();
    assert!(failures.contains("damaged") && failures.contains("missing"), "{failures}");
}

#[tokio::test]
async fn folder_spaces_are_copied_with_their_versions_and_restored_into_their_folder() {
    let env = testutil::folders_env().await;
    let admin = env.admin().await;
    let space = env.folder_space("Scans").await;
    testutil::write_old(&space.dir.join("one.txt"), b"one");
    testutil::write_old(&space.dir.join("sub/two.txt"), b"two");
    std::fs::create_dir_all(space.dir.join("empty")).unwrap();
    crate::folders::scan(&env.st, &space.drive).await.unwrap();
    let (one, _) = env.node_at(&space.drive, "one.txt").await.unwrap();
    save(&env, &admin, &one, b"one, edited").await;
    // The folder space is on no location: it goes with a location only when its folder is on one. Put it on the
    // built-in one for the copy.
    sqlx::query("UPDATE drives SET location_id = 'local' WHERE id = ?").bind(&space.drive).execute(&env.st.db).await.unwrap();
    let nas = add_nas(&env, "nas").await;
    let (set, job) = copy_all(&env, "local", "nas").await;
    // Once the first file is stored, the other one changes on disk: it is read again, as it is then
    let (dir, fired) = (space.dir.clone(), Arc::new(std::sync::atomic::AtomicBool::new(false)));
    let f = fired.clone();
    on_object_put(&env, "nas", move |n| {
        if n == 1 && !f.swap(true, SeqCst) {
            testutil::write_old(&dir.join("sub/two.txt"), b"two, changed while copying");
        }
        Box::pin(async {})
    });
    assert_eq!(run_job(&env, &job).await, "done", "{:?}", state(&env, &job).await);
    assert!(!objects_in(&nas, &set).is_empty());
    // The folder is as it was, the space where it was
    assert_eq!(std::fs::read(space.dir.join("one.txt")).unwrap(), b"one, edited");
    let snapshot = snapshot_of(&env, &set).await;
    let (versions,): (i64,) = sqlx::query_as("SELECT versions FROM backup_snapshots WHERE id = ?").bind(&snapshot).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(versions, 1);
    let job = restore(&env, &snapshot, &space.drive, None, false).await.unwrap();
    assert_eq!(run_job(&env, &job).await, "done", "{:?}", state(&env, &job).await);
    let top = restored_folder(&env, &space.root).await;
    let name = children(&env, &space.root).await.into_iter().find(|(_, (id, _))| *id == top).unwrap().0;
    assert_eq!(std::fs::read(space.dir.join(&name).join("one.txt")).unwrap(), b"one, edited");
    let two = std::fs::read(space.dir.join(&name).join("sub/two.txt")).unwrap();
    assert!(two == b"two" || two == b"two, changed while copying", "one of the two, whole");
    assert_eq!(std::fs::read(space.dir.join("sub/two.txt")).unwrap(), b"two, changed while copying");
    assert!(space.dir.join(&name).join("empty").is_dir());
}

/// Copies to and from S3, SFTP and FTP locations (the in-memory bucket and the test servers of sftp.rs and ftp.rs)
#[tokio::test]
async fn copies_go_to_s3_sftp_and_ftp_locations_and_come_back_from_them() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let docs = env.folder(&amy, amy.root(), "Docs").await;
    let a = env.upload(&amy, &docs, "a.txt", b"over the network").await;
    let big: &'static [u8] = vec![7u8; 300_000].leak();
    let b = env.upload(&amy, amy.root(), "big.bin", big).await;
    let sftp = crate::sftp::tests::server(testutil::password()).await;
    let ftp = crate::ftp::tests::server(testutil::password()).await;
    add_location(&env, "bucket", "s3", json!({ "endpoint": "https://s3.example.com", "bucket": "b" }), Arc::new(storage::S3Storage::in_memory("tf"))).await;
    add_location(
        &env,
        "sftp",
        "sftp",
        json!({ "host": "127.0.0.1", "path": "/files" }),
        Arc::new(crate::sftp::tests::storage(&sftp, testutil::password(), &sftp.fingerprint)),
    )
    .await;
    add_location(&env, "ftp", "ftp", json!({ "host": "127.0.0.2", "path": "/files" }), Arc::new(crate::ftp::tests::storage(&ftp, testutil::password()))).await;
    let mut snapshots = Vec::new();
    for dest in ["bucket", "sftp", "ftp"] {
        let (set, job) = copy_all(&env, "local", dest).await;
        assert_eq!(run_job(&env, &job).await, "done", "{dest}: {:?}", state(&env, &job).await);
        snapshots.push((dest, set.clone(), snapshot_of(&env, &set).await));
        // Checked by reading it back
        let Json(v) = api::verify(State(env.st.clone()), Admin(env.admin().await), Path(set.clone())).await.unwrap();
        let check = v["job_id"].as_str().unwrap().to_string();
        assert_eq!(run_job(&env, &check).await, "done", "{dest}: {:?}", state(&env, &check).await);
    }
    assert!(!files_below(&sftp.dir.join("files").join(super::layout::ROOT)).is_empty());
    assert!(!files_below(&ftp.dir.join("files").join(super::layout::ROOT)).is_empty());
    // The files go for good; each copy brings them back
    purge(&env, &amy, &a).await;
    purge(&env, &amy, &b).await;
    let mine = env.drive_of(amy.root()).await;
    for (dest, _, snapshot) in &snapshots {
        let job = restore(&env, snapshot, &mine, None, false).await.unwrap();
        assert_eq!(run_job(&env, &job).await, "done", "{dest}: {:?}", state(&env, &job).await);
        let restored: Vec<String> = sqlx::query_as::<_, (String,)>("SELECT id FROM nodes WHERE parent_id = ? AND name LIKE 'Restored %' ORDER BY created_at DESC, rowid DESC")
            .bind(amy.root())
            .fetch_all(&env.st.db)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.0)
            .collect();
        let top = &restored[0];
        let f = at(&env, top, "Docs/a.txt").await.unwrap();
        assert_eq!(read(&env, &amy, &f).await, b"over the network", "{dest}");
        let f = at(&env, top, "big.bin").await.unwrap();
        assert_eq!(read(&env, &amy, &f).await, big, "{dest}");
    }
    // Deleting the copies leaves nothing behind on the services
    for (dest, set, _) in &snapshots {
        let Json(_) = api::delete(State(env.st.clone()), Admin(env.admin().await), Path(set.clone())).await.unwrap();
        let remove = job_of(&env, set, "remove").await.unwrap();
        assert_eq!(run_job(&env, &remove).await, "done", "{dest}: {:?}", state(&env, &remove).await);
    }
    assert!(files_below(&sftp.dir.join("files").join(super::layout::ROOT)).is_empty());
    assert!(files_below(&ftp.dir.join("files").join(super::layout::ROOT)).is_empty());
}

#[tokio::test]
async fn spaces_on_an_s3_location_are_copied_to_a_local_folder() {
    let env = testutil::env().await;
    add_location(&env, "bucket", "s3", json!({ "endpoint": "https://s3.example.com", "bucket": "b" }), Arc::new(storage::S3Storage::in_memory("tf"))).await;
    sqlx::query("UPDATE storage_locations SET is_default = (id = 'bucket')").execute(&env.st.db).await.unwrap();
    let amy = env.user("amy", true).await;
    let a = env.upload(&amy, amy.root(), "a.txt", b"from the bucket").await;
    assert_eq!(blob_location(&env, b"from the bucket").await.as_deref(), Some("bucket"));
    let nas = add_nas(&env, "nas").await;
    let (set, job) = copy_all(&env, "bucket", "nas").await;
    assert_eq!(run_job(&env, &job).await, "done", "{:?}", state(&env, &job).await);
    assert_eq!(objects_in(&nas, &set).len(), 1);
    purge(&env, &amy, &a).await;
    delete_due(&env, "bucket").await;
    let snapshot = snapshot_of(&env, &set).await;
    let job = restore(&env, &snapshot, &env.drive_of(amy.root()).await, None, false).await.unwrap();
    assert_eq!(run_job(&env, &job).await, "done");
    let top = restored_folder(&env, amy.root()).await;
    let f = at(&env, &top, "a.txt").await.unwrap();
    assert_eq!(read(&env, &amy, &f).await, b"from the bucket");
    assert_eq!(blob_location(&env, b"from the bucket").await.as_deref(), Some("bucket"), "stored where the space keeps its files");
}
