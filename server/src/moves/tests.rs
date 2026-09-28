use std::{
    path::Path as FsPath,
    sync::atomic::{AtomicUsize, Ordering::SeqCst},
};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use futures_util::future::BoxFuture;
use serde_json::json;

use super::*;
use crate::{
    storage::{self, LocalStorage, Storage},
    testutil::{self, TestEnv},
};

/// A location that works like a bucket (kind s3), its content in a folder of the test
async fn add_bucket(env: &TestEnv, id: &str) {
    sqlx::query("INSERT INTO storage_locations (id, name, kind, config, is_default, created_at) VALUES (?, ?, 's3', '{\"bucket\":\"b\"}', 0, 0)")
        .bind(id)
        .bind(id.to_uppercase())
        .execute(&env.st.db)
        .await
        .unwrap();
    env.st.storages.write().unwrap().insert(id.into(), Arc::new(LocalStorage::create(env.dir.join(id), id).unwrap()));
}

/// Where a location of `add_bucket` (or the built-in one, "blobs") keeps a content
fn stored(env: &TestEnv, dir: &str, content: &[u8]) -> std::path::PathBuf {
    let hash = crate::util::sha256_hex(content);
    env.dir.join(dir).join(&hash[0..2]).join(&hash[2..4]).join(&hash)
}

async fn move_to(env: &TestEnv, drives: &[&str], to: &str) -> AppResult<String> {
    let req = CreateReq { drive_ids: drives.iter().map(|d| d.to_string()).collect(), location_id: to.into() };
    let Json(v) = create(State(env.st.clone()), Admin(env.admin().await), Json(req)).await?;
    Ok(v["ids"][0].as_str().unwrap().to_string())
}

async fn load(env: &TestEnv, id: &str) -> Job {
    job(&mut env.st.db.acquire().await.unwrap(), id).await.unwrap().unwrap()
}

/// Takes a queued move as the runner does: its job and controls
async fn take_job(env: &TestEnv, id: &str) -> (Job, Arc<Control>) {
    let job = load(env, id).await;
    let ctl = take(&env.st, &job).await.unwrap().expect("queued");
    (job, ctl)
}

/// Runs a queued move to its end, as the runner does; returns its state
async fn run_move(env: &TestEnv, id: &str) -> String {
    let (job, ctl) = take_job(env, id).await;
    run(&env.st, &job, &ctl).await;
    state(env, id).await
}

async fn state(env: &TestEnv, id: &str) -> String {
    sqlx::query_as::<_, (String,)>("SELECT state FROM space_moves WHERE id = ?").bind(id).fetch_one(&env.st.db).await.unwrap().0
}

async fn location_of_space(env: &TestEnv, drive: &str) -> Option<String> {
    sqlx::query_as::<_, (Option<String>,)>("SELECT location_id FROM drives WHERE id = ?").bind(drive).fetch_one(&env.st.db).await.unwrap().0
}

/// Where the content of this text is recorded
async fn blob_location(env: &TestEnv, content: &[u8]) -> Option<String> {
    sqlx::query_as::<_, (String,)>("SELECT location_id FROM blobs WHERE hash = ?")
        .bind(crate::util::sha256_hex(content))
        .fetch_optional(&env.st.db)
        .await
        .unwrap()
        .map(|r| r.0)
}

async fn read(env: &TestEnv, user: &crate::auth::User, id: &str) -> Vec<u8> {
    let q = Query(serde_json::from_value(json!({})).unwrap());
    let res = crate::files::content(State(env.st.clone()), user.clone(), Path(id.to_string()), q, HeaderMap::new()).await.unwrap();
    axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec()
}

/// Deletions that are waiting are made due, and done
async fn delete_due(env: &TestEnv, location: &str) {
    sqlx::query("UPDATE pending_blob_deletes SET created_at = 0 WHERE location_id = ?").bind(location).execute(&env.st.db).await.unwrap();
    crate::tree::retry_pending_deletes(&env.st, location).await;
}

/// A local content store that calls a hook before each read, to do things while a move copies
struct Hooked {
    inner: LocalStorage,
    on_open: Box<dyn Fn(String) -> BoxFuture<'static, ()> + Send + Sync>,
}

impl Storage for Hooked {
    fn put_file<'a>(&'a self, hash: &'a str, src: &'a FsPath) -> BoxFuture<'a, std::io::Result<()>> {
        self.inner.put_file(hash, src)
    }
    fn open<'a>(&'a self, hash: &'a str, start: u64, len: u64) -> BoxFuture<'a, std::io::Result<storage::BoxReader>> {
        Box::pin(async move {
            (self.on_open)(hash.to_string()).await;
            self.inner.open(hash, start, len).await
        })
    }
    fn delete<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, std::io::Result<()>> {
        self.inner.delete(hash)
    }
    fn check(&self) -> BoxFuture<'_, std::io::Result<()>> {
        self.inner.check()
    }
}

/// The built-in location, with a hook before each read
fn hook_builtin(env: &TestEnv, on_open: impl Fn(String) -> BoxFuture<'static, ()> + Send + Sync + 'static) {
    let hooked = Hooked { inner: LocalStorage::new(env.dir.join("blobs"), "local"), on_open: Box::new(on_open) };
    env.st.storages.write().unwrap().insert("local".into(), Arc::new(hooked));
}

/// Stops a move (pauses or cancels it) at the `at`-th content it reads; counts the reads
fn stop_at(env: &TestEnv, ctl: &Arc<Control>, at: usize, cancel: bool) -> Arc<AtomicUsize> {
    let (ctl, reads) = (ctl.clone(), Arc::new(AtomicUsize::new(0)));
    let counted = reads.clone();
    hook_builtin(env, move |_| {
        if counted.fetch_add(1, SeqCst) + 1 == at {
            (if cancel { &ctl.cancel } else { &ctl.pause }).store(true, SeqCst);
        }
        Box::pin(async {})
    });
    reads
}

/// `stop_at` for reads from a location of `add_bucket`
fn stop_reading(env: &TestEnv, location: &str, ctl: &Arc<Control>, at: usize, cancel: bool) {
    let (ctl, reads) = (ctl.clone(), Arc::new(AtomicUsize::new(0)));
    let hooked = Hooked {
        inner: LocalStorage::new(env.dir.join(location), location),
        on_open: Box::new(move |_| {
            if reads.fetch_add(1, SeqCst) + 1 == at {
                (if cancel { &ctl.cancel } else { &ctl.pause }).store(true, SeqCst);
            }
            Box::pin(async {})
        }),
    };
    env.st.storages.write().unwrap().insert(location.into(), Arc::new(hooked));
}

/// Files with different content in Amy's space, more than a page of them; returns (id, content)
async fn many_files(env: &TestEnv, amy: &crate::auth::User, n: usize) -> Vec<(String, &'static [u8])> {
    let mut out = Vec::new();
    for i in 0..n {
        let content: &'static [u8] = format!("file number {i}").into_bytes().leak();
        out.push((env.upload(amy, amy.root(), &format!("f{i}.txt"), content).await, content));
    }
    out
}

#[tokio::test]
async fn a_space_moves_to_another_content_store_with_its_versions_and_trash() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let drive = env.drive_of(amy.root()).await;
    add_bucket(&env, "bucket").await;
    let files = many_files(&env, &amy, 60).await;
    let docs = env.folder(&amy, amy.root(), "Docs").await;
    let notes = env.upload(&amy, &docs, "notes.txt", b"first").await;
    // An earlier version, and an item in the trash
    let _ = crate::files::save_content(State(env.st.clone()), amy.clone(), Path(notes.clone()), HeaderMap::new(), axum::body::Bytes::from_static(b"second"))
        .await
        .unwrap();
    let gone = env.upload(&amy, amy.root(), "old.txt", b"in the trash").await;
    let req = serde_json::from_value(json!({ "ids": [gone] })).unwrap();
    let _ = crate::nodes::trash(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
    let root_before = amy.root().to_string();

    let id = move_to(&env, &[&drive], "bucket").await.unwrap();
    assert_eq!(state(&env, &id).await, "queued");
    assert_eq!(run_move(&env, &id).await, "done");

    assert_eq!(location_of_space(&env, &drive).await.as_deref(), Some("bucket"));
    let (left,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM blobs WHERE location_id != 'bucket'").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(left, 0, "every content moved: files, the earlier version and the trash");
    for (id, content) in &files {
        assert_eq!(read(&env, &amy, id).await, *content);
    }
    assert_eq!(read(&env, &amy, &notes).await, b"second");
    let (version,): (String,) = sqlx::query_as("SELECT id FROM node_versions WHERE node_id = ?").bind(&notes).fetch_one(&env.st.db).await.unwrap();
    let q = Query(serde_json::from_value(json!({})).unwrap());
    let res = crate::versions::content(State(env.st.clone()), amy.clone(), Path((notes.clone(), version)), q, HeaderMap::new()).await.unwrap();
    assert_eq!(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap()[..], b"first");
    assert!(stored(&env, "bucket", b"in the trash").is_file());
    // The personal space keeps its root: Amy's "My files" is the same folder
    let (root,): (String,) = sqlx::query_as("SELECT root_id FROM users WHERE id = ?").bind(amy.id).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(root, root_before);
    assert_eq!(env.drive_of(&root).await, drive);

    // New files go to the new location
    let new = env.upload(&amy, amy.root(), "new.txt", b"after the move").await;
    assert_eq!(blob_location(&env, b"after the move").await.as_deref(), Some("bucket"));
    assert_eq!(read(&env, &amy, &new).await, b"after the move");

    // The old copies are deleted a minute later, not at once
    assert!(stored(&env, "blobs", b"file number 1").is_file());
    delete_due(&env, "local").await;
    assert!(!stored(&env, "blobs", b"file number 1").exists());
    assert_eq!(read(&env, &amy, &files[1].0).await, b"file number 1");

    // Done, with its progress and history
    let Json(list) = list(State(env.st.clone()), Admin(env.admin().await)).await.unwrap();
    let m = serde_json::to_value(&list).unwrap()["moves"][0].clone();
    assert_eq!((m["state"].as_str(), m["files_done"].as_i64(), m["files_total"].as_i64()), (Some("done"), Some(63), Some(63)));
    assert_eq!((m["space_kind"].as_str(), m["owner_name"].as_str(), m["from_name"].as_str(), m["to_name"].as_str()), (Some("personal"), Some("amy"), Some("Local disk"), Some("BUCKET")));
    let (items,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM space_move_items").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(items, 0, "the record of copies goes once the move is done");
    let actions: Vec<(String, String)> = sqlx::query_as("SELECT action, detail FROM activity WHERE action LIKE 'move_%' ORDER BY id").fetch_all(&env.st.db).await.unwrap();
    assert_eq!(actions, [("move_start".into(), "Local disk → BUCKET".into()), ("move_done".into(), "Local disk → BUCKET".into())]);
}

#[tokio::test]
async fn a_paused_move_continues_where_it_stopped_also_after_a_restart() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let drive = env.drive_of(amy.root()).await;
    add_bucket(&env, "bucket").await;
    let files = many_files(&env, &amy, 30).await;
    let id = move_to(&env, &[&drive], "bucket").await.unwrap();

    let (job, ctl) = take_job(&env, &id).await;
    stop_at(&env, &ctl, 10, false);
    run(&env.st, &job, &ctl).await;
    assert_eq!(state(&env, &id).await, "paused");
    // What was copied is kept, the space is still where it was
    let (copied,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM space_move_items WHERE move_id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap();
    assert!((9..=10).contains(&copied), "{copied}");
    assert_eq!(location_of_space(&env, &drive).await.as_deref(), Some("local"));
    assert_eq!(blob_location(&env, files[0].1).await.as_deref(), Some("local"));
    let (done,): (i64,) = sqlx::query_as("SELECT files_done FROM space_moves WHERE id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(done, copied);
    // Copies of a move that isn't over are left alone by background deletion
    delete_due(&env, "bucket").await;
    let (hash,): (String,) = sqlx::query_as("SELECT hash FROM space_move_items WHERE move_id = ? LIMIT 1").bind(&id).fetch_one(&env.st.db).await.unwrap();
    assert!(env.dir.join("bucket").join(&hash[0..2]).join(&hash[2..4]).join(&hash).is_file());

    // Resumed, it runs again, and ThirtyFile stops in the middle (the move was running)
    let _ = resume(State(env.st.clone()), Admin(env.admin().await), Path(id.clone())).await.unwrap();
    assert_eq!(state(&env, &id).await, "queued");
    let (job, ctl) = take_job(&env, &id).await;
    stop_at(&env, &ctl, 5, false);
    run(&env.st, &job, &ctl).await;
    sqlx::query("UPDATE space_moves SET state = 'running' WHERE id = ?").bind(&id).execute(&env.st.db).await.unwrap();
    recover(&env.st).await.unwrap();
    assert_eq!(state(&env, &id).await, "queued", "after a restart, a move that was running waits for its turn again");

    // It copies only what is left
    let (copied,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM space_move_items WHERE move_id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap();
    assert!(copied >= 14, "{copied}");
    let (job, ctl) = take_job(&env, &id).await;
    let reads = stop_at(&env, &ctl, usize::MAX, false);
    run(&env.st, &job, &ctl).await;
    assert_eq!(state(&env, &id).await, "done");
    assert_eq!(reads.load(SeqCst) as i64, 30 - copied, "only what was left is read");
    for (id, content) in &files {
        assert_eq!(read(&env, &amy, id).await, *content);
    }
    assert_eq!(location_of_space(&env, &drive).await.as_deref(), Some("bucket"));
}

#[tokio::test]
async fn a_cancelled_move_leaves_the_space_where_it_was() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let drive = env.drive_of(amy.root()).await;
    add_bucket(&env, "bucket").await;
    let files = many_files(&env, &amy, 20).await;

    // Cancelled while it runs
    let id = move_to(&env, &[&drive], "bucket").await.unwrap();
    let (job, ctl) = take_job(&env, &id).await;
    stop_at(&env, &ctl, 8, true);
    run(&env.st, &job, &ctl).await;
    assert_eq!(state(&env, &id).await, "cancelled");
    assert_eq!(location_of_space(&env, &drive).await.as_deref(), Some("local"));
    assert!(stored(&env, "bucket", files[0].1).is_file() || stored(&env, "bucket", files[1].1).is_file(), "copies were made");
    // What it copied goes (a minute later, like any content no longer used); the originals stay
    delete_due(&env, "bucket").await;
    for (id, content) in &files {
        assert!(!stored(&env, "bucket", content).exists());
        assert_eq!(read(&env, &amy, id).await, *content);
        assert_eq!(blob_location(&env, content).await.as_deref(), Some("local"));
    }

    // Cancelled while paused
    let id = move_to(&env, &[&drive], "bucket").await.unwrap();
    let (job, ctl) = take_job(&env, &id).await;
    stop_at(&env, &ctl, 5, false);
    run(&env.st, &job, &ctl).await;
    assert_eq!(state(&env, &id).await, "paused");
    let _ = cancel(State(env.st.clone()), Admin(env.admin().await), Path(id.clone())).await.unwrap();
    assert_eq!(state(&env, &id).await, "cancelled");
    delete_due(&env, "bucket").await;
    assert!(files.iter().all(|(_, c)| !stored(&env, "bucket", c).exists()));
    let actions: Vec<(String,)> = sqlx::query_as("SELECT action FROM activity WHERE action = 'move_cancel'").fetch_all(&env.st.db).await.unwrap();
    assert_eq!(actions.len(), 2);
    // A queued one is simply taken off the queue
    let id = move_to(&env, &[&drive], "bucket").await.unwrap();
    let _ = cancel(State(env.st.clone()), Admin(env.admin().await), Path(id.clone())).await.unwrap();
    assert_eq!(state(&env, &id).await, "cancelled");
    assert!(cancel(State(env.st.clone()), Admin(env.admin().await), Path(id.clone())).await.is_err(), "over already");
}

#[tokio::test]
async fn a_move_copies_what_the_space_gets_meanwhile_and_leaves_what_it_loses() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let drive = env.drive_of(amy.root()).await;
    add_bucket(&env, "bucket").await;
    add_bucket(&env, "third").await;
    let files = many_files(&env, &amy, 5).await;
    let hashes: Vec<String> = files.iter().map(|(_, c)| crate::util::sha256_hex(c)).collect();
    let id = move_to(&env, &[&drive], "bucket").await.unwrap();

    // While the move copies: a new file is uploaded, one file is deleted for good, and the content of another is
    // moved elsewhere by something else
    let st = env.st.clone();
    let (amy2, deleted, elsewhere) = (amy.clone(), files[1].0.clone(), hashes[2].clone());
    let first = Arc::new(AtomicUsize::new(0));
    hook_builtin(&env, move |hash| {
        let (st, amy, deleted, elsewhere, first) = (st.clone(), amy2.clone(), deleted.clone(), elsewhere.clone(), first.clone());
        Box::pin(async move {
            if first.fetch_add(1, SeqCst) == 0 {
                let env = TestEnvRef(st.clone());
                env.upload(&amy, "new.txt", b"uploaded during the move").await;
                let req = serde_json::from_value(json!({ "ids": [deleted] })).unwrap();
                let _ = crate::nodes::trash(State(st.clone()), amy.clone(), Json(req)).await.unwrap();
                let req = serde_json::from_value(json!({ "ids": [deleted] })).unwrap();
                let _ = crate::nodes::delete_forever(State(st.clone()), amy.clone(), Json(req)).await.unwrap();
            }
            if hash == elsewhere && first.load(SeqCst) < 100 {
                first.store(100, SeqCst);
                let (from, to) = (st.data_dir.join("blobs"), st.data_dir.join("third"));
                let rel = std::path::PathBuf::from(&hash[0..2]).join(&hash[2..4]).join(&hash);
                std::fs::create_dir_all(to.join(rel.parent().unwrap())).unwrap();
                std::fs::copy(from.join(&rel), to.join(&rel)).unwrap();
                let _w = st.write_lock.lock().await;
                sqlx::query("UPDATE blobs SET location_id = 'third' WHERE hash = ?").bind(&hash).execute(&st.db).await.unwrap();
            }
        })
    });
    let ended = run_move(&env, &id).await;
    let (error,): (Option<String>,) = sqlx::query_as("SELECT error || failures FROM space_moves WHERE id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(ended, "done", "{error:?}");
    assert_eq!(blob_location(&env, b"uploaded during the move").await.as_deref(), Some("bucket"), "copied before the switch");
    assert_eq!(blob_location(&env, files[1].1).await, None);
    // Moved elsewhere meanwhile: copied again from there, as the space's content goes to the target wherever it is
    assert_eq!(blob_location(&env, files[2].1).await.as_deref(), Some("bucket"));
    for (i, (id, content)) in files.iter().enumerate().filter(|(i, _)| *i != 1) {
        assert_eq!(read(&env, &amy, id).await, *content, "{i}");
    }
    // The copies nothing uses go, after checking
    delete_due(&env, "bucket").await;
    assert!(!stored(&env, "bucket", files[1].1).exists());
    delete_due(&env, "third").await;
    assert!(!stored(&env, "third", files[2].1).exists(), "the copy it was moved from goes too");
    assert!(stored(&env, "bucket", files[2].1).is_file());
    assert!(stored(&env, "bucket", files[0].1).is_file());
}

/// Uploading from inside a storage hook, where a `TestEnv` isn't at hand
struct TestEnvRef(AppState);

impl TestEnvRef {
    async fn upload(&self, user: &crate::auth::User, name: &str, content: &'static [u8]) {
        use base64::Engine;
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        let mut h = HeaderMap::new();
        h.insert("upload-length", content.len().to_string().parse().unwrap());
        h.insert("upload-metadata", format!("filename {},parentId {}", b64(name), b64(user.root())).parse().unwrap());
        let res = crate::upload::create(State(self.0.clone()), user.clone(), h).await.unwrap();
        let upload = res.headers()[axum::http::header::LOCATION].to_str().unwrap().rsplit('/').next().unwrap().to_string();
        let mut h = HeaderMap::new();
        h.insert(axum::http::header::CONTENT_TYPE, "application/offset+octet-stream".parse().unwrap());
        h.insert("upload-offset", "0".parse().unwrap());
        crate::upload::patch(State(self.0.clone()), user.clone(), Path(upload), h, axum::body::Body::from(content)).await.unwrap();
    }
}

#[tokio::test]
async fn a_move_to_a_location_that_cant_be_reached_fails_and_can_be_resumed() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let drive = env.drive_of(amy.root()).await;
    add_bucket(&env, "bucket").await;
    let files = many_files(&env, &amy, 3).await;
    let id = move_to(&env, &[&drive], "bucket").await.unwrap();
    // Gone before it starts: it fails, and the space stays as it was
    let backend = env.st.storages.write().unwrap().remove("bucket").unwrap();
    assert_eq!(run_move(&env, &id).await, "failed");
    let (error,): (Option<String>,) = sqlx::query_as("SELECT error FROM space_moves WHERE id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap();
    assert!(error.unwrap().starts_with("The target storage location can't be reached"));
    assert_eq!(location_of_space(&env, &drive).await.as_deref(), Some("local"));
    assert_eq!(read(&env, &amy, &files[0].0).await, files[0].1);
    // A new move to it isn't even queued
    assert!(move_to(&env, &[&drive], "bucket").await.is_err());
    let bob = env.user("bob", true).await;
    let bobs = env.drive_of(bob.root()).await;
    assert!(move_to(&env, &[&bobs], "bucket").await.unwrap_err().message.starts_with("The target storage location can't be reached"));

    // Back again: resumed, it finishes
    env.st.storages.write().unwrap().insert("bucket".into(), backend);
    let _ = resume(State(env.st.clone()), Admin(env.admin().await), Path(id.clone())).await.unwrap();
    assert_eq!(run_move(&env, &id).await, "done");
    assert_eq!(read(&env, &amy, &files[0].0).await, files[0].1);
    let failed: Vec<(String,)> = sqlx::query_as("SELECT detail FROM activity WHERE action = 'move_failed'").fetch_all(&env.st.db).await.unwrap();
    assert_eq!(failed.len(), 1);
}

#[tokio::test]
async fn content_that_cant_be_read_is_listed_without_names_from_personal_spaces() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let amy = env.user("amy", true).await;
    add_bucket(&env, "bucket").await;
    let company = env.st.shared_root().unwrap();
    env.upload(&admin, &company, "plan.txt", b"the plan").await;
    env.upload(&admin, &company, "budget.txt", b"the budget").await;
    env.upload(&amy, amy.root(), "diary.txt", b"dear diary").await;
    // Missing from the storage
    let away = |c: &[u8]| {
        let p = stored(&env, "blobs", c);
        std::fs::rename(&p, p.with_extension("away")).unwrap();
        p
    };
    let (plan, diary) = (away(b"the plan"), away(b"dear diary"));

    let ids = [move_to(&env, &[&env.drive_of(&company).await], "bucket").await.unwrap(), move_to(&env, &[&env.drive_of(amy.root()).await], "bucket").await.unwrap()];
    for id in &ids {
        assert_eq!(run_move(&env, id).await, "failed");
    }
    let Json(list) = list(State(env.st.clone()), Admin(admin.clone())).await.unwrap();
    let moves = serde_json::to_value(&list).unwrap()["moves"].clone();
    let of = |id: &str| moves.as_array().unwrap().iter().find(|m| m["id"] == id).unwrap().clone();
    let company_move = of(&ids[0]);
    assert_eq!(company_move["failed_items"], 1);
    assert_eq!(company_move["failures"][0]["item"], "plan.txt");
    assert_eq!(company_move["error"], "1 file couldn't be copied");
    let personal = of(&ids[1]);
    assert_eq!(personal["failed_items"], 1);
    assert!(personal["failures"][0]["item"].is_null(), "no names from a personal space");
    assert_eq!(blob_location(&env, b"the budget").await.as_deref(), Some("local"), "nothing switched");

    // Found again: resumed, the moves finish
    std::fs::rename(plan.with_extension("away"), &plan).unwrap();
    std::fs::rename(diary.with_extension("away"), &diary).unwrap();
    for id in &ids {
        let _ = resume(State(env.st.clone()), Admin(admin.clone()), Path(id.clone())).await.unwrap();
        assert_eq!(run_move(&env, id).await, "done");
    }
    assert_eq!(blob_location(&env, b"the plan").await.as_deref(), Some("bucket"));
    assert_eq!(blob_location(&env, b"dear diary").await.as_deref(), Some("bucket"));
}

#[tokio::test]
async fn spaces_and_locations_being_moved_are_kept() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    add_bucket(&env, "bucket").await;
    let req = serde_json::from_value(json!({ "name": "Sales" })).unwrap();
    let Json(team) = crate::drives::create(State(env.st.clone()), admin.clone(), Json(req)).await.unwrap();
    let team = serde_json::to_value(&team).unwrap()["id"].as_str().unwrap().to_string();
    let id = move_to(&env, &[&team], "bucket").await.unwrap();
    let err = crate::drives::delete(State(env.st.clone()), admin.clone(), Path(team.clone())).await.unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::CONFLICT);
    let err = crate::locations::delete(State(env.st.clone()), Admin(admin.clone()), Path("bucket".into())).await.unwrap_err();
    assert!(err.message.starts_with("A space is being moved to or from this location"));
    // Once it is done: the space is on the location, which keeps it; the old one can go
    assert_eq!(run_move(&env, &id).await, "done");
    assert_eq!(move_to(&env, &[&team], "bucket").await.unwrap_err().message, "\"Sales\" is already on this location");
    let _ = crate::drives::delete(State(env.st.clone()), admin.clone(), Path(team.clone())).await.unwrap();
}

async fn make_default_bucket(env: &TestEnv) {
    sqlx::query("UPDATE storage_locations SET is_default = (id = 'bucket')").execute(&env.st.db).await.unwrap();
}

#[tokio::test]
async fn moves_run_one_at_a_time_unless_set_otherwise() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let bob = env.user("bob", true).await;
    add_bucket(&env, "bucket").await;
    env.upload(&amy, amy.root(), "a.txt", b"amy's").await;
    env.upload(&bob, bob.root(), "b.txt", b"bob's").await;
    // Reads wait until the test lets them go on
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let g = gate.clone();
    hook_builtin(&env, move |_| {
        let g = g.clone();
        Box::pin(async move { g.acquire().await.unwrap().forget() })
    });
    let first = move_to(&env, &[&env.drive_of(amy.root()).await], "bucket").await.unwrap();
    let second = move_to(&env, &[&env.drive_of(bob.root()).await], "bucket").await.unwrap();
    start_due(&env.st).await.unwrap();
    let running = || async {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM space_moves WHERE state = 'running'").fetch_one(&env.st.db).await.unwrap();
        n
    };
    assert_eq!(running().await, 1);
    assert_eq!(state(&env, &first).await, "running", "the older one first");
    assert_eq!(state(&env, &second).await, "queued");
    // Two at a time: the second starts too
    let _ = update_settings(State(env.st.clone()), Admin(env.admin().await), Json(SettingsReq { concurrency: 2 })).await.unwrap();
    start_due(&env.st).await.unwrap();
    assert_eq!(running().await, 2);
    assert!(update_settings(State(env.st.clone()), Admin(env.admin().await), Json(SettingsReq { concurrency: 0 })).await.is_err());
    gate.add_permits(100);
    for _ in 0..500 {
        if state(&env, &first).await == "done" && state(&env, &second).await == "done" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!((state(&env, &first).await, state(&env, &second).await), ("done".into(), "done".into()));
    assert!(env.st.moves.running.lock().unwrap().is_empty());
}

// ───────────── Folder spaces into a content store ─────────────

/// A content store that calls a hook each time something is stored in it
struct PutHook {
    inner: LocalStorage,
    on_put: Box<dyn Fn(String) -> BoxFuture<'static, ()> + Send + Sync>,
}

impl Storage for PutHook {
    fn put_file<'a>(&'a self, hash: &'a str, src: &'a FsPath) -> BoxFuture<'a, std::io::Result<()>> {
        Box::pin(async move {
            (self.on_put)(hash.to_string()).await;
            self.inner.put_file(hash, src).await
        })
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
}

/// The bucket of `add_bucket`, with a hook each time something is stored in it
fn hook_bucket(env: &TestEnv, id: &str, on_put: impl Fn(String) -> BoxFuture<'static, ()> + Send + Sync + 'static) {
    let hooked = PutHook { inner: LocalStorage::new(env.dir.join(id), id), on_put: Box::new(on_put) };
    env.st.storages.write().unwrap().insert(id.into(), Arc::new(hooked));
}

/// Pauses or cancels a move once `at` files are stored in the bucket
fn stop_on_put(env: &TestEnv, ctl: &Arc<Control>, at: usize, cancel: bool) {
    let (ctl, puts) = (ctl.clone(), Arc::new(AtomicUsize::new(0)));
    hook_bucket(env, "bucket", move |_| {
        if puts.fetch_add(1, SeqCst) + 1 == at {
            (if cancel { &ctl.cancel } else { &ctl.pause }).store(true, SeqCst);
        }
        Box::pin(async {})
    });
}

async fn space_state(env: &TestEnv, drive: &str) -> (String, Option<String>, Option<String>, bool) {
    sqlx::query_as("SELECT mode, location_id, source_path, moving FROM drives WHERE id = ?").bind(drive).fetch_one(&env.st.db).await.unwrap()
}

async fn trash(env: &TestEnv, user: &crate::auth::User, id: &str) {
    let req = serde_json::from_value(json!({ "ids": [id] })).unwrap();
    let _ = crate::nodes::trash(State(env.st.clone()), user.clone(), Json(req)).await.unwrap();
}

async fn save(env: &TestEnv, user: &crate::auth::User, id: &str, body: &'static [u8]) {
    let _ = crate::files::save_content(State(env.st.clone()), user.clone(), Path(id.to_string()), HeaderMap::new(), axum::body::Bytes::from_static(body))
        .await
        .unwrap();
}

async fn version_contents(env: &TestEnv, user: &crate::auth::User, id: &str) -> Vec<Vec<u8>> {
    let versions: Vec<(String,)> = sqlx::query_as("SELECT id FROM node_versions WHERE node_id = ? ORDER BY created_at, rowid").bind(id).fetch_all(&env.st.db).await.unwrap();
    let mut out = Vec::new();
    for (v,) in versions {
        let q = Query(serde_json::from_value(json!({})).unwrap());
        let res = crate::versions::content(State(env.st.clone()), user.clone(), Path((id.to_string(), v)), q, HeaderMap::new()).await.unwrap();
        out.push(axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec());
    }
    out
}

#[tokio::test]
async fn a_folder_space_moves_into_a_content_store_with_its_trash_and_versions() {
    let env = testutil::folders_env().await;
    let admin = env.admin().await;
    let amy = env.user("amy", true).await;
    add_bucket(&env, "bucket").await;
    let company = env.st.shared_root().unwrap();
    let (all, mine) = (env.drive_of(&company).await, env.drive_of(amy.root()).await);
    let folder = env.dir.join("blobs").join("company");
    let docs = env.folder(&admin, &company, "Docs").await;
    let plan = env.upload(&admin, &docs, "plan.txt", b"plan, first").await;
    save(&env, &admin, &plan, b"plan, second").await;
    save(&env, &admin, &plan, b"plan, third").await;
    let mut files = Vec::new();
    for i in 0..30 {
        let content: &'static [u8] = format!("company file {i}").into_bytes().leak();
        files.push((env.upload(&admin, &company, &format!("f{i}.txt"), content).await, content));
    }
    let old = env.upload(&admin, &docs, "old.txt", b"in the trash").await;
    trash(&env, &admin, &old).await;
    // Put there by other programs: two names only letter case tells apart (a disk that tells them apart), and a
    // file ThirtyFile never shows
    if cfg!(unix) {
        testutil::write_old(&folder.join("Case.txt"), b"upper");
        testutil::write_old(&folder.join("case.txt"), b"lower");
    }
    testutil::write_old(&folder.join("Thumbs.db"), b"thumbnails");
    crate::folders::scan(&env.st, &all).await.unwrap();
    let diary = env.upload(&amy, amy.root(), "diary.txt", b"dear diary").await;
    let used_before = sqlx::query_as::<_, (i64,)>("SELECT used_bytes FROM drives WHERE id = ?").bind(&all).fetch_one(&env.st.db).await.unwrap().0;

    let ids = [move_to(&env, &[&all], "bucket").await.unwrap(), move_to(&env, &[&mine], "bucket").await.unwrap()];
    for id in &ids {
        assert_eq!(run_move(&env, id).await, "done");
    }
    assert_eq!(space_state(&env, &all).await, ("store".into(), Some("bucket".into()), None, false));
    assert_eq!(space_state(&env, &mine).await, ("store".into(), Some("bucket".into()), None, false));
    // Everything is read from the content store now
    assert_eq!(read(&env, &admin, &plan).await, b"plan, third");
    assert_eq!(version_contents(&env, &admin, &plan).await, [b"plan, first".to_vec(), b"plan, second".to_vec()]);
    for (id, content) in &files {
        assert_eq!(read(&env, &admin, id).await, *content);
    }
    assert_eq!(read(&env, &amy, &diary).await, b"dear diary");
    assert_eq!(blob_location(&env, b"plan, first").await.as_deref(), Some("bucket"));
    let (with_path,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE drive_id IN (?, ?) AND fs_path IS NOT NULL").bind(&all).bind(&mine).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(with_path, 0);
    let used_after = sqlx::query_as::<_, (i64,)>("SELECT used_bytes FROM drives WHERE id = ?").bind(&all).fetch_one(&env.st.db).await.unwrap().0;
    assert_eq!(used_after, used_before);
    // The trash is the store's trash: restored, it opens
    let req = serde_json::from_value(json!({ "ids": [old] })).unwrap();
    let _ = crate::nodes::restore(State(env.st.clone()), admin.clone(), Json(req)).await.unwrap();
    assert_eq!(read(&env, &admin, &old).await, b"in the trash");
    if cfg!(unix) {
        let names: Vec<(String,)> =
            sqlx::query_as("SELECT name FROM nodes WHERE parent_id = ? AND lower(name) LIKE 'case%' ORDER BY name").bind(&company).fetch_all(&env.st.db).await.unwrap();
        assert_eq!(names, [("Case.txt".into(),), ("case (1).txt".into(),)]);
    }
    // The old folders are gone, apart from what ThirtyFile never showed, which the move names
    assert!(!env.dir.join("blobs/users/amy").exists());
    let left: Vec<String> = std::fs::read_dir(&folder).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(left, ["Thumbs.db"]);
    let (note,): (Option<String>,) = sqlx::query_as("SELECT note FROM space_moves WHERE id = ?").bind(&ids[0]).fetch_one(&env.st.db).await.unwrap();
    assert!(note.as_deref().unwrap_or_default().contains("Thumbs.db"), "{note:?}");
    if cfg!(unix) {
        assert!(note.unwrap().contains("1 item got another name"));
    }
    // A personal space keeps its root
    let (root,): (String,) = sqlx::query_as("SELECT root_id FROM users WHERE id = ?").bind(amy.id).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(root, amy.root());
    // New files go to the bucket
    env.upload(&admin, &company, "new.txt", b"after").await;
    assert_eq!(blob_location(&env, b"after").await.as_deref(), Some("bucket"));
}

#[tokio::test]
async fn a_folder_space_is_read_only_while_it_moves_and_changes_in_the_folder_are_moved_too() {
    let env = testutil::folders_env().await;
    let admin = env.admin().await;
    add_bucket(&env, "bucket").await;
    let company = env.st.shared_root().unwrap();
    let all = env.drive_of(&company).await;
    let folder = env.dir.join("blobs").join("company");
    let mut files = Vec::new();
    for i in 0..20 {
        let content: &'static [u8] = format!("file {i}").into_bytes().leak();
        files.push((env.upload(&admin, &company, &format!("f{i:02}.txt"), content).await, content));
    }
    let id = move_to(&env, &[&all], "bucket").await.unwrap();
    // Waiting its turn, the space can still be changed
    env.upload(&admin, &company, "queued.txt", b"while waiting").await;

    let (job, ctl) = take_job(&env, &id).await;
    stop_on_put(&env, &ctl, 8, false);
    run(&env.st, &job, &ctl).await;
    assert_eq!(state(&env, &id).await, "paused");
    assert_eq!(space_state(&env, &all).await, ("folder".into(), Some("local".into()), Some(folder.to_string_lossy().into_owned()), true));
    // Read-only meanwhile, and people are told why
    let err = env.try_upload(&admin, &company, "refused.txt", b"no").await.unwrap_err();
    assert_eq!((err.status, err.message.as_str()), (axum::http::StatusCode::FORBIDDEN, "This space is being moved to another storage location. It is read-only until the move finishes."));
    let Json(info) = crate::nodes::get(State(env.st.clone()), admin.clone(), Path(company.clone())).await.unwrap();
    let info = serde_json::to_value(&info).unwrap();
    assert_eq!((info["moving"].as_bool(), info["read_only"].as_bool()), (Some(true), Some(true)));
    assert_eq!(read(&env, &admin, &files[0].0).await, files[0].1, "files still open");

    // Changed on the server meanwhile (over SMB, say): a file copied already, a new one, one removed
    testutil::write_old(&folder.join("f00.txt"), b"file 0, changed on the server");
    testutil::write_old(&folder.join("Scans").join("scan.pdf"), b"%PDF new on the server");
    std::fs::remove_file(folder.join("f01.txt")).unwrap();
    // ThirtyFile stops while the move runs, and continues after the start
    let _ = resume(State(env.st.clone()), Admin(admin.clone()), Path(id.clone())).await.unwrap();
    let (job, ctl) = take_job(&env, &id).await;
    stop_on_put(&env, &ctl, 3, false);
    run(&env.st, &job, &ctl).await;
    sqlx::query("UPDATE space_moves SET state = 'running' WHERE id = ?").bind(&id).execute(&env.st.db).await.unwrap();
    recover(&env.st).await.unwrap();
    assert!(space_state(&env, &all).await.3, "still read-only after the restart");
    // And a file changed after the last look at the folder (just now, so the scan leaves it for later): the space
    // keeps what was copied, and the changed file stays in the old folder
    let (job, ctl) = take_job(&env, &id).await;
    let written = Arc::new(AtomicUsize::new(0));
    let (f, w) = (folder.clone(), written.clone());
    hook_bucket(&env, "bucket", move |_| {
        if w.fetch_add(1, SeqCst) == 0 {
            std::fs::write(f.join("f19.txt"), b"file 19, changed at the last moment").unwrap();
        }
        Box::pin(async {})
    });
    run(&env.st, &job, &ctl).await;
    let (error,): (Option<String>,) = sqlx::query_as("SELECT error || ' ' || failures FROM space_moves WHERE id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(state(&env, &id).await, "done", "{error:?}");
    assert_eq!(space_state(&env, &all).await, ("store".into(), Some("bucket".into()), None, false));
    assert_eq!(read(&env, &admin, &files[0].0).await, b"file 0, changed on the server");
    let (scan,) = sqlx::query_as::<_, (String,)>("SELECT id FROM nodes WHERE drive_id = ? AND name = 'scan.pdf'").bind(&all).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(read(&env, &admin, &scan).await, b"%PDF new on the server");
    let gone: Option<(String,)> = sqlx::query_as("SELECT id FROM nodes WHERE id = ?").bind(&files[1].0).fetch_optional(&env.st.db).await.unwrap();
    assert!(gone.is_none());
    let queued = sqlx::query_as::<_, (String,)>("SELECT id FROM nodes WHERE drive_id = ? AND name = 'queued.txt'").bind(&all).fetch_one(&env.st.db).await.unwrap().0;
    assert_eq!(read(&env, &admin, &queued).await, b"while waiting");
    // The space got the content that was copied; the file changed since is kept in the old folder, and named
    let f19 = sqlx::query_as::<_, (String,)>("SELECT id FROM nodes WHERE drive_id = ? AND name = 'f19.txt'").bind(&all).fetch_one(&env.st.db).await.unwrap().0;
    if read(&env, &admin, &f19).await == b"file 19" {
        assert_eq!(std::fs::read(folder.join("f19.txt")).unwrap(), b"file 19, changed at the last moment");
        let (note,): (Option<String>,) = sqlx::query_as("SELECT note FROM space_moves WHERE id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap();
        assert!(note.unwrap().contains("f19.txt"));
    }
    assert!(!folder.join("f00.txt").exists() && !folder.join("Scans").exists(), "what was copied is gone from the folder");
    // Writable again
    env.upload(&admin, &company, "after.txt", b"after the move").await;
}

#[tokio::test]
async fn a_cancelled_move_of_a_folder_space_leaves_its_folder_as_it_was() {
    let env = testutil::folders_env().await;
    let admin = env.admin().await;
    add_bucket(&env, "bucket").await;
    let company = env.st.shared_root().unwrap();
    let all = env.drive_of(&company).await;
    let folder = env.dir.join("blobs").join("company");
    let mut files = Vec::new();
    for i in 0..10 {
        let content: &'static [u8] = format!("file {i}").into_bytes().leak();
        files.push((env.upload(&admin, &company, &format!("f{i}.txt"), content).await, content));
    }
    let id = move_to(&env, &[&all], "bucket").await.unwrap();
    let (job, ctl) = take_job(&env, &id).await;
    stop_on_put(&env, &ctl, 4, true);
    run(&env.st, &job, &ctl).await;
    assert_eq!(state(&env, &id).await, "cancelled");
    assert_eq!(space_state(&env, &all).await, ("folder".into(), Some("local".into()), Some(folder.to_string_lossy().into_owned()), false));
    for (id, content) in &files {
        assert_eq!(read(&env, &admin, id).await, *content);
    }
    assert!(stored(&env, "bucket", files.iter().map(|f| f.1).find(|c| stored(&env, "bucket", c).exists()).unwrap_or(b"none")).exists());
    delete_due(&env, "bucket").await;
    assert!(files.iter().all(|(_, c)| !stored(&env, "bucket", c).exists()), "what was copied goes");
    let (blobs,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM blobs").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(blobs, 0);
    env.upload(&admin, &company, "after.txt", b"writable again").await;
    assert!(folder.join("after.txt").is_file());
}

#[tokio::test]
async fn a_folder_that_isnt_there_is_never_moved_as_an_empty_space() {
    let env = testutil::folders_env().await;
    let admin = env.admin().await;
    add_bucket(&env, "bucket").await;
    let company = env.st.shared_root().unwrap();
    let all = env.drive_of(&company).await;
    let folder = env.dir.join("blobs").join("company");
    let away = env.dir.join("away");
    let a = env.upload(&admin, &company, "a.txt", b"still here").await;
    // Not there when the move is asked for: refused
    std::fs::rename(&folder, &away).unwrap();
    let err = move_to(&env, &[&all], "bucket").await.unwrap_err();
    assert_eq!((err.status, err.message.as_str()), (axum::http::StatusCode::SERVICE_UNAVAILABLE, storage::NOT_MOUNTED));
    // Gone when it starts: it stops, and the space is as it was
    std::fs::rename(&away, &folder).unwrap();
    let id = move_to(&env, &[&all], "bucket").await.unwrap();
    std::fs::rename(&folder, &away).unwrap();
    std::fs::create_dir(&folder).unwrap();
    assert_eq!(run_move(&env, &id).await, "failed");
    let (error,): (Option<String>,) = sqlx::query_as("SELECT error FROM space_moves WHERE id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(error.as_deref(), Some(storage::NOT_MOUNTED), "an empty folder without the marker isn't the space's");
    assert_eq!(space_state(&env, &all).await.0, "folder");
    let (blobs,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM blobs").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(blobs, 0);
    // Back: resumed, it finishes
    std::fs::remove_dir(&folder).unwrap();
    std::fs::rename(&away, &folder).unwrap();
    let _ = resume(State(env.st.clone()), Admin(admin.clone()), Path(id.clone())).await.unwrap();
    assert_eq!(run_move(&env, &id).await, "done");
    assert_eq!(read(&env, &admin, &a).await, b"still here");
}

// ───────────── Into folders ─────────────

/// A Local folder location (a NAS, say) with its folder in the test's folder
async fn add_nas(env: &TestEnv, id: &str) -> std::path::PathBuf {
    let dir = env.dir.join(id);
    sqlx::query("INSERT INTO storage_locations (id, name, kind, config, is_default, created_at) VALUES (?, ?, 'local', ?, 0, 0)")
        .bind(id)
        .bind(id.to_uppercase())
        .bind(json!({ "path": dir.to_string_lossy() }).to_string())
        .execute(&env.st.db)
        .await
        .unwrap();
    env.st.storages.write().unwrap().insert(id.into(), Arc::new(LocalStorage::create(dir.clone(), id).unwrap()));
    dir
}

/// Reads a file of a folder space as the index has it: (id, content on disk)
async fn on_disk(env: &TestEnv, drive: &str, rel: &str) -> Option<Vec<u8>> {
    let (root,): (String,) = sqlx::query_as("SELECT source_path FROM drives WHERE id = ?").bind(drive).fetch_one(&env.st.db).await.unwrap();
    std::fs::read(std::path::Path::new(&root).join(rel)).ok()
}

#[tokio::test]
async fn a_content_store_space_moves_into_a_folder_with_its_trash_and_versions() {
    let env = testutil::folders_env().await;
    add_bucket(&env, "bucket").await;
    make_default_bucket(&env).await;
    let amy = env.user("amy", true).await;
    let mine = env.drive_of(amy.root()).await;
    assert_eq!(space_state(&env, &mine).await.0, "store");
    let docs = env.folder(&amy, amy.root(), "Docs").await;
    let notes = env.upload(&amy, &docs, "notes.txt", b"one").await;
    save(&env, &amy, &notes, b"two").await;
    // Names a folder would hide: renamed on the way
    let thumbs = env.upload(&amy, amy.root(), "Thumbs.db", b"not a real thumbnail cache").await;
    let old = env.upload(&amy, &docs, "old.txt", b"in the trash").await;
    trash(&env, &amy, &old).await;
    let files = many_files(&env, &amy, 12).await;
    sqlx::query("UPDATE nodes SET updated_at = 1700000000 WHERE id = ?").bind(&files[0].0).execute(&env.st.db).await.unwrap();

    let id = move_to(&env, &[&mine], "local").await.unwrap();
    // Read-only while it runs
    let (job, ctl) = take_job(&env, &id).await;
    stop_reading(&env, "bucket", &ctl, 4, false);
    run(&env.st, &job, &ctl).await;
    assert_eq!(state(&env, &id).await, "paused");
    assert!(space_state(&env, &mine).await.3);
    let err = env.try_upload(&amy, amy.root(), "no.txt", b"no").await.unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::FORBIDDEN);
    let _ = resume(State(env.st.clone()), Admin(env.admin().await), Path(id.clone())).await.unwrap();
    assert_eq!(run_move(&env, &id).await, "done");

    let folder = env.dir.join("blobs").join("users").join("amy");
    assert_eq!(space_state(&env, &mine).await, ("folder".into(), Some("local".into()), Some(folder.to_string_lossy().into_owned()), false));
    assert_eq!(std::fs::read(folder.join("Docs/notes.txt")).unwrap(), b"two");
    assert_eq!(read(&env, &amy, &notes).await, b"two");
    assert_eq!(version_contents(&env, &amy, &notes).await, [b"one".to_vec()]);
    assert_eq!(std::fs::read(folder.join("_Thumbs.db")).unwrap(), b"not a real thumbnail cache");
    let (name,): (String,) = sqlx::query_as("SELECT name FROM nodes WHERE id = ?").bind(&thumbs).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(name, "_Thumbs.db");
    for (id, content) in &files {
        assert_eq!(read(&env, &amy, id).await, *content);
    }
    let modified = std::fs::metadata(folder.join("f0.txt")).unwrap().modified().unwrap();
    assert_eq!(modified.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(), 1700000000, "files keep their dates");
    assert!(std::fs::read_dir(folder.join(crate::fsops::TRASH_DIR)).unwrap().next().is_some());
    let req = serde_json::from_value(json!({ "ids": [old] })).unwrap();
    let _ = crate::nodes::restore(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
    assert_eq!(on_disk(&env, &mine, "Docs/old.txt").await.as_deref(), Some(&b"in the trash"[..]));
    // A scan finds the folder as the index has it
    let report = crate::folders::scan(&env.st, &mine).await.unwrap();
    assert_eq!((report.added, report.removed, report.moved), (0, 0, 0), "{report:?}");
    // The content store lets go: nothing of Amy's is left there, a minute later
    let (blobs,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM blobs").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(blobs, 0);
    delete_due(&env, "bucket").await;
    assert!(!stored(&env, "bucket", b"one").exists() && !stored(&env, "bucket", files[0].1).exists());
    let (root,): (String,) = sqlx::query_as("SELECT root_id FROM users WHERE id = ?").bind(amy.id).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(root, amy.root());
    // Writable, as a folder space
    env.upload(&amy, amy.root(), "after.txt", b"after").await;
    assert_eq!(std::fs::read(folder.join("after.txt")).unwrap(), b"after");
}

#[tokio::test]
async fn a_cancelled_move_into_a_folder_removes_the_folder_it_made() {
    let env = testutil::folders_env().await;
    add_bucket(&env, "bucket").await;
    make_default_bucket(&env).await;
    let amy = env.user("amy", true).await;
    let mine = env.drive_of(amy.root()).await;
    let files = many_files(&env, &amy, 10).await;
    let id = move_to(&env, &[&mine], "local").await.unwrap();
    let (job, ctl) = take_job(&env, &id).await;
    stop_reading(&env, "bucket", &ctl, 5, true);
    run(&env.st, &job, &ctl).await;
    assert_eq!(state(&env, &id).await, "cancelled");
    assert!(!env.dir.join("blobs/users/amy").exists(), "the folder it made is gone");
    assert_eq!(space_state(&env, &mine).await, ("store".into(), Some("bucket".into()), None, false));
    for (id, content) in &files {
        assert_eq!(read(&env, &amy, id).await, *content);
    }
    env.upload(&amy, amy.root(), "after.txt", b"writable").await;
}

#[tokio::test]
async fn a_folder_space_moves_to_a_folder_on_another_location() {
    for other_disk in [false, true] {
        let env = testutil::folders_env().await;
        let admin = env.admin().await;
        let nas = add_nas(&env, "nas").await;
        let company = env.st.shared_root().unwrap();
        let all = env.drive_of(&company).await;
        let folder = env.dir.join("blobs").join("company");
        let docs = env.folder(&admin, &company, "Docs").await;
        let plan = env.upload(&admin, &docs, "plan.txt", b"first").await;
        save(&env, &admin, &plan, b"second").await;
        let old = env.upload(&admin, &company, "old.txt", b"trash").await;
        trash(&env, &admin, &old).await;
        let files = {
            let mut out = Vec::new();
            for i in 0..8 {
                let content: &'static [u8] = format!("file {i}").into_bytes().leak();
                out.push((env.upload(&admin, &company, &format!("f{i}.txt"), content).await, content));
            }
            out
        };
        testutil::write_old(&folder.join("Thumbs.db"), b"kept with the files");
        let id = move_to(&env, &[&all], "nas").await.unwrap();
        between_folders::OTHER_DISK.with(|d| d.set(other_disk));
        if other_disk {
            // Changed in the folder while the move is paused
            let (job, ctl) = take_job(&env, &id).await;
            ctl.pause.store(true, SeqCst);
            run(&env.st, &job, &ctl).await;
            assert_eq!(state(&env, &id).await, "paused");
            testutil::write_old(&folder.join("f0.txt"), b"file 0, changed on the server");
            testutil::write_old(&folder.join("new.txt"), b"new on the server");
            std::fs::remove_file(folder.join("f1.txt")).unwrap();
            let _ = resume(State(env.st.clone()), Admin(admin.clone()), Path(id.clone())).await.unwrap();
        }
        let ended = run_move(&env, &id).await;
        between_folders::OTHER_DISK.with(|d| d.set(false));
        let (error,): (Option<String>,) = sqlx::query_as("SELECT error || failures FROM space_moves WHERE id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(ended, "done", "{other_disk}: {error:?}");
        let new = nas.join("company");
        assert_eq!(space_state(&env, &all).await, ("folder".into(), Some("nas".into()), Some(new.to_string_lossy().into_owned()), false), "{other_disk}");
        assert_eq!(read(&env, &admin, &plan).await, b"second");
        assert_eq!(version_contents(&env, &admin, &plan).await, [b"first".to_vec()]);
        let req = serde_json::from_value(json!({ "ids": [old] })).unwrap();
        let _ = crate::nodes::restore(State(env.st.clone()), admin.clone(), Json(req)).await.unwrap();
        assert_eq!(std::fs::read(new.join("old.txt")).unwrap(), b"trash");
        assert_eq!(std::fs::read(new.join("Thumbs.db")).unwrap(), b"kept with the files", "{other_disk}: everything in the folder moves");
        assert_eq!(std::fs::read_to_string(new.join(crate::folders::MARKER)).unwrap(), all);
        if other_disk {
            assert_eq!(read(&env, &admin, &files[0].0).await, b"file 0, changed on the server");
            let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE drive_id = ? AND name IN ('new.txt', 'f1.txt')").bind(&all).fetch_one(&env.st.db).await.unwrap();
            assert_eq!(n, 1, "the new file is in, the removed one out");
        } else {
            assert_eq!(read(&env, &admin, &files[0].0).await, files[0].1);
        }
        assert!(!folder.exists(), "{other_disk}: the old folder is gone");
        let report = crate::folders::scan(&env.st, &all).await.unwrap();
        assert_eq!((report.added, report.removed), (0, 0), "{other_disk}: {report:?}");
        env.upload(&admin, &company, "after.txt", b"after").await;
        assert!(new.join("after.txt").is_file());
    }
}

#[tokio::test]
async fn a_folder_an_administrator_chose_moves_onto_a_location() {
    let env = testutil::folders_env().await;
    let admin = env.admin().await;
    add_bucket(&env, "bucket").await;
    let nas = add_nas(&env, "nas").await;
    for (name, to) in [("Scans", "nas"), ("Archive", "bucket")] {
        let space = env.folder_space(name).await;
        testutil::write_old(&space.dir.join("a.txt"), b"shown from elsewhere");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (a, _) = env.node_at(&space.drive, "a.txt").await.unwrap();
        assert_eq!(space_state(&env, &space.drive).await.1, None, "on no location");
        let id = move_to(&env, &[&space.drive], to).await.unwrap();
        assert_eq!(run_move(&env, &id).await, "done", "{name}");
        let (mode, location, path, _) = space_state(&env, &space.drive).await;
        assert_eq!(location.as_deref(), Some(to));
        match to {
            "nas" => assert_eq!((mode.as_str(), path), ("folder", Some(nas.join("teams").join(name).to_string_lossy().into_owned()))),
            _ => assert_eq!((mode.as_str(), path), ("store", None)),
        }
        assert_eq!(read(&env, &admin, &a).await, b"shown from elsewhere");
        assert!(!space.dir.exists(), "{name}: the folder was moved");
    }
}
