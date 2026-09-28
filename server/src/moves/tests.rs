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

#[tokio::test]
async fn folder_spaces_cant_be_moved_yet() {
    let env = testutil::env().await;
    add_bucket(&env, "bucket").await;
    let space = env.folder_space("Scans").await;
    let err = move_to(&env, &[&space.drive], "bucket").await.unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
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
