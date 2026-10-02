use std::{path::PathBuf, sync::Arc};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use serde_json::json;

use super::{REPLICAS, api, policy};
use crate::{
    auth::{Admin, User},
    backups::runner,
    replicas::Memory,
    storage::{self, LocalStorage, Storage},
    testutil::{self, TestEnv},
};

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

async fn add_nas(env: &TestEnv, id: &str) -> PathBuf {
    let dir = env.dir.join(id);
    add_location(env, id, "local", json!({ "path": dir.to_string_lossy() }), Arc::new(LocalStorage::create(dir.clone(), id).unwrap())).await;
    dir
}

/// Where a Local folder location keeps a content
fn stored(dir: &std::path::Path, content: &[u8]) -> PathBuf {
    let h = crate::util::sha256_hex(content);
    dir.join(&h[0..2]).join(&h[2..4]).join(&h)
}

/// A policy; not checked every day unless asked (the checks would run with the syncs)
async fn make(env: &TestEnv, mut body: serde_json::Value) -> String {
    if body.get("verify_days").is_none() {
        body["verify_days"] = json!(0);
    }
    let Json(v) = api::create(State(env.st.clone()), Admin(env.admin().await), Json(serde_json::from_value(body).unwrap())).await.unwrap();
    v["id"].as_str().unwrap().to_string()
}

/// Runs the queued jobs of a policy, oldest first; returns their end states
async fn run_queued(env: &TestEnv, policy: &str) -> Vec<String> {
    let ids: Vec<(String,)> = sqlx::query_as("SELECT id FROM replica_jobs WHERE policy_id = ? AND state = 'queued' ORDER BY created_at, rowid")
        .bind(policy)
        .fetch_all(&env.st.db)
        .await
        .unwrap();
    let mut out = Vec::new();
    for (id,) in ids {
        let job = runner::job_in(&mut env.st.db.acquire().await.unwrap(), &env.st.part::<Memory>().queue, &id).await.unwrap().unwrap();
        let ctl = runner::take_in(&env.st, &REPLICAS, &job).await.unwrap().expect("queued");
        runner::run_in(&env.st, &REPLICAS, &job, &ctl).await;
        let (state, error): (String, Option<String>) =
            sqlx::query_as("SELECT state, error FROM replica_jobs WHERE id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap();
        out.push(if let Some(e) = error { format!("{state}: {e}") } else { state });
    }
    out
}

/// A look by the scheduler, then another once changes had time to settle, then the jobs queued are run
async fn settle(env: &TestEnv, policy: &str) -> Vec<String> {
    let t = crate::util::now();
    policy::tick(&env.st, t).await.unwrap();
    policy::tick(&env.st, t + crate::backups::scheduler::BATCH_SECONDS).await.unwrap();
    run_queued(env, policy).await
}

async fn copies(env: &TestEnv, location: &str) -> Vec<(String, String)> {
    sqlx::query_as("SELECT hash, state FROM replica_copies WHERE location_id = ? ORDER BY hash").bind(location).fetch_all(&env.st.db).await.unwrap()
}

async fn read(env: &TestEnv, user: &User, id: &str) -> crate::error::AppResult<Vec<u8>> {
    let q = Query(serde_json::from_value(json!({})).unwrap());
    let res = crate::files::content(State(env.st.clone()), user.clone(), Path(id.to_string()), q, HeaderMap::new()).await?;
    Ok(axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec())
}

async fn purge_file(env: &TestEnv, user: &User, id: &str) {
    let req = serde_json::from_value(json!({ "ids": [id] })).unwrap();
    let _ = crate::nodes::trash(State(env.st.clone()), user.clone(), Json(req)).await.unwrap();
    let req = serde_json::from_value(json!({ "ids": [id] })).unwrap();
    let Json(job) = crate::nodes::delete_forever(State(env.st.clone()), user.clone(), Json(req)).await.unwrap();
    if job.state == "running" {
        crate::jobs::wait_for(&env.st, &job.id).await;
    }
}

async fn delete_due(env: &TestEnv, location: &str) {
    sqlx::query("UPDATE pending_blob_deletes SET created_at = 0 WHERE location_id = ?").bind(location).execute(&env.st.db).await.unwrap();
    crate::tree::retry_pending_deletes(&env.st, location).await;
}

async fn health(env: &TestEnv, id: &str) -> policy::Health {
    let mut c = env.st.db.acquire().await.unwrap();
    let p = super::load(&mut c, id).await.unwrap().unwrap();
    let targets = super::targets(&mut c, id).await.unwrap();
    drop(c);
    policy::health(&env.st, &p, &targets, crate::util::now()).await.unwrap()
}

/// The built-in location stops answering: its folder is gone (a disk that failed)
fn fail_builtin(env: &TestEnv) {
    std::fs::rename(env.dir.join("blobs"), env.dir.join("failed-disk")).unwrap();
    env.st.recheck.notify_one();
}

fn repair_builtin(env: &TestEnv) {
    std::fs::rename(env.dir.join("failed-disk"), env.dir.join("blobs")).unwrap();
}

#[tokio::test]
async fn replicas_are_copied_checked_kept_from_cleanup_and_read_when_the_primary_fails() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let bob = env.user("bob", true).await;
    let a = env.upload(&amy, amy.root(), "a.txt", b"amy's report").await;
    env.upload(&bob, bob.root(), "same.txt", b"amy's report").await;
    let nas = add_nas(&env, "nas").await;
    let id = make(&env, json!({ "source": "local", "targets": [{ "location": "nas" }] })).await;
    assert_eq!(health(&env, &id).await.targets[0].state, "initializing");
    // A new target is synced at once: identical content once
    assert_eq!(settle(&env, &id).await, ["done"]);
    assert_eq!(copies(&env, "nas").await.len(), 1);
    assert!(stored(&nas, b"amy's report").is_file());
    let h = health(&env, &id).await;
    assert_eq!((h.state, h.current, h.targets[0].held, h.targets[0].wanted), ("ok", 1, Some(1), Some(1)));
    // The copy isn't taken for unused content, nor deleted by a deletion of that content there
    let s = env.st.storage("nas").unwrap();
    let (unused, _) = crate::location_tools::unused_scan(&env.st, "nas", s.as_ref(), crate::util::now() + 86400 * 10, &|_| {}).await.unwrap();
    assert!(unused.is_empty(), "{unused:?}");
    let hash = crate::util::sha256_hex(b"amy's report");
    crate::tree::defer_blob_removal(&env.st, &[(hash.clone(), "nas".to_string())], 0).await;
    delete_due(&env, "nas").await;
    assert!(stored(&nas, b"amy's report").is_file());
    // The location holding it can't be deleted
    assert!(crate::locations::delete(State(env.st.clone()), Admin(env.admin().await), Path("nas".to_string())).await.is_err());
    // The primary fails: the file is read from the replica, the same content
    fail_builtin(&env);
    assert_eq!(read(&env, &amy, &a).await.unwrap(), b"amy's report");
    let (logged,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM activity WHERE action = 'replica_read'").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(logged, 1);
    // Without a checked copy, it isn't read from anywhere else
    sqlx::query("UPDATE replica_copies SET state = 'corrupt'").execute(&env.st.db).await.unwrap();
    assert!(read(&env, &amy, &a).await.is_err());
    // Not with read fallback off either
    sqlx::query("UPDATE replica_copies SET state = 'verified'").execute(&env.st.db).await.unwrap();
    let Json(_) =
        api::update(State(env.st.clone()), Admin(env.admin().await), Path(id.clone()), Json(serde_json::from_value(json!({ "read_fallback": false })).unwrap()))
            .await
            .unwrap();
    assert!(read(&env, &amy, &a).await.is_err());
    repair_builtin(&env);
}

#[tokio::test]
async fn a_space_deleted_before_its_changes_were_synced_leaves_no_target_behind() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let amy = env.user("amy", true).await;
    env.upload(&amy, amy.root(), "a.txt", b"amy's").await;
    let req = serde_json::from_value(json!({ "name": "Sales" })).unwrap();
    let Json(info) = crate::drives::create(State(env.st.clone()), admin.clone(), Json(req)).await.unwrap();
    let info = serde_json::to_value(&info).unwrap();
    let (sales, root) = (info["id"].as_str().unwrap().to_string(), info["root_id"].as_str().unwrap().to_string());
    env.upload(&admin, &root, "plan.txt", b"the plan").await;
    add_nas(&env, "nas").await;
    let id = make(&env, json!({ "source": "local", "targets": [{ "location": "nas" }] })).await;
    assert_eq!(settle(&env, &id).await, ["done"]);
    // The team space changes, and is deleted before the change is synced
    env.upload(&admin, &root, "later.txt", b"later").await;
    let t = crate::util::now();
    policy::tick(&env.st, t).await.unwrap();
    let _ = crate::drives::delete(State(env.st.clone()), admin.clone(), Path(sales.clone())).await.unwrap();
    // Long after: not behind, so not degraded either (administrators would be told)
    let mut c = env.st.db.acquire().await.unwrap();
    let p = super::load(&mut c, &id).await.unwrap().unwrap();
    let targets = super::targets(&mut c, &id).await.unwrap();
    drop(c);
    let h = policy::health(&env.st, &p, &targets, t + 48 * 3600).await.unwrap();
    assert_eq!((h.state, h.targets[0].state, h.targets[0].behind_since), ("ok", "current", None));
    // What was recorded of it goes with the next sync
    policy::trigger(&env.st, &id, "nas", "manual", None).await.unwrap();
    assert_eq!(run_queued(&env, &id).await, ["done"]);
    for table in ["replica_dirty", "replica_captured"] {
        let (n,): (i64,) =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table} WHERE drive_id = ?"))).bind(&sales).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(n, 0, "{table}");
    }
}

#[tokio::test]
async fn a_sync_reads_the_content_a_page_at_a_time_and_an_idle_look_takes_no_write_lock() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    env.upload(&amy, amy.root(), "a.txt", b"amy's").await;
    add_nas(&env, "nas").await;
    let id = make(&env, json!({ "source": "local", "targets": [{ "location": "nas" }] })).await;
    assert_eq!(settle(&env, &id).await, ["done"]);
    // A page of what to copy goes on in hash order from where the last one ended, each content looked up in the
    // spaces: the content of the spaces isn't gathered again for every page
    let plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("EXPLAIN QUERY PLAN {}", super::sync::missing_page())))
        .bind("[]")
        .bind("nas")
        .bind("")
        .fetch_all(&env.st.db)
        .await
        .unwrap();
    let plan = plan.into_iter().map(|r| r.3).collect::<Vec<_>>().join(" | ");
    assert!(plan.contains("SEARCH b USING INDEX sqlite_autoindex_blobs_1 (hash>?)") && !plan.contains("UNION"), "{plan}");
    // Nothing to do: a look at the policy takes no write lock (it is held here, so one taken would wait)
    let held = env.st.write_lock.lock().await;
    let look = tokio::time::timeout(std::time::Duration::from_secs(10), policy::tick(&env.st, crate::util::now())).await;
    drop(held);
    look.expect("an idle look waited for the write lock").unwrap();
}

#[tokio::test]
async fn the_replicas_page_shows_the_counts_kept_with_each_target_instead_of_counting_every_copy() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let a = env.upload(&amy, amy.root(), "a.txt", b"first").await;
    env.upload(&amy, amy.root(), "b.txt", b"second").await;
    add_nas(&env, "nas").await;
    let id = make(&env, json!({ "source": "local", "targets": [{ "location": "nas" }] })).await;
    let page = async || {
        let Json(o) = api::list(State(env.st.clone()), Admin(env.admin().await)).await.unwrap();
        serde_json::to_value(&o).unwrap()
    };
    let shown = |o: &serde_json::Value, i: usize| {
        let t = &o["policies"][0]["health"]["targets"][i];
        (t["held"].clone(), t["wanted"].clone())
    };
    let kept = async |location: &str| {
        sqlx::query_as::<_, (Option<i64>, Option<i64>)>("SELECT held, wanted FROM replica_targets WHERE policy_id = ? AND location_id = ?")
            .bind(&id)
            .bind(location)
            .fetch_one(&env.st.db)
            .await
            .unwrap()
    };
    // Not worked out yet
    assert_eq!(shown(&page().await, 0), (json!(null), json!(null)));
    // The sync works them out and keeps them with the target
    assert_eq!(settle(&env, &id).await, ["done"]);
    assert_eq!(kept("nas").await, (Some(2), Some(2)));
    // The page shows what is kept: it doesn't count the copies again
    sqlx::query("UPDATE replica_targets SET held = 7, wanted = 9").execute(&env.st.db).await.unwrap();
    sqlx::query("INSERT OR REPLACE INTO replica_unneeded (location_id, copies, bytes) VALUES ('nas', 3, 300)").execute(&env.st.db).await.unwrap();
    let o = page().await;
    assert_eq!(shown(&o, 0), (json!(7), json!(9)));
    assert_eq!(o["policies"][0]["health"]["targets"][0]["state"], "behind");
    assert_eq!(o["unneeded"], json!([["nas", "NAS", 3, 300]]));
    // Copies made, and let go of: worked out again by the sync that made or let go of them
    env.upload(&amy, amy.root(), "c.txt", b"third").await;
    assert_eq!(settle(&env, &id).await, ["done"]);
    assert_eq!(kept("nas").await, (Some(3), Some(3)));
    assert_eq!(page().await["unneeded"], json!([]));
    purge_file(&env, &amy, &a).await;
    assert_eq!(settle(&env, &id).await, ["done"]);
    assert_eq!(kept("nas").await, (Some(2), Some(2)));
    // A changed policy (paused: no sync runs): worked out again by the scheduler's next look
    add_nas(&env, "usb").await;
    let req = json!({ "enabled": false, "copies": 2, "targets": [{ "location": "nas" }, { "location": "usb" }] });
    let Json(_) = api::update(State(env.st.clone()), Admin(env.admin().await), Path(id.clone()), Json(serde_json::from_value(req).unwrap())).await.unwrap();
    assert_eq!(shown(&page().await, 1), (json!(null), json!(null)));
    policy::tick(&env.st, crate::util::now()).await.unwrap();
    let o = page().await;
    assert_eq!((shown(&o, 0), shown(&o, 1)), ((json!(2), json!(2)), (json!(0), json!(2))), "{o}");
    assert_eq!(o["unneeded"], json!([]));
}

#[tokio::test]
async fn changes_are_copied_soon_after_and_content_nothing_uses_lets_go_of_its_copy() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let bob = env.user("bob", true).await;
    let a = env.upload(&amy, amy.root(), "a.txt", b"shared").await;
    env.upload(&bob, bob.root(), "b.txt", b"shared").await;
    let nas = add_nas(&env, "nas").await;
    let id = make(&env, json!({ "source": "local", "targets": [{ "location": "nas" }] })).await;
    assert_eq!(settle(&env, &id).await, ["done"]);
    // A change: copied soon after, the target is current again
    let c = env.upload(&amy, amy.root(), "c.txt", b"new").await;
    policy::tick(&env.st, crate::util::now()).await.unwrap();
    assert_eq!(health(&env, &id).await.targets[0].state, "behind");
    assert_eq!(settle(&env, &id).await, ["done"]);
    assert!(stored(&nas, b"new").is_file());
    assert_eq!(health(&env, &id).await.targets[0].state, "current");
    // Content still used elsewhere keeps its copy; content nothing uses lets go of it
    purge_file(&env, &amy, &a).await;
    purge_file(&env, &amy, &c).await;
    assert_eq!(settle(&env, &id).await, ["done"]);
    delete_due(&env, "nas").await;
    assert!(stored(&nas, b"shared").is_file(), "Bob still has it");
    assert!(!stored(&nas, b"new").exists(), "nothing uses it any more");
    assert_eq!(copies(&env, "nas").await.len(), 1);
    // Many changes, one row for the space: nothing to run out of
    let (rows,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM space_changes").fetch_one(&env.st.db).await.unwrap();
    assert!(rows <= 4, "{rows}");
}

#[tokio::test]
async fn a_damaged_or_missing_copy_is_found_and_replaced_from_a_checked_one() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    env.upload(&amy, amy.root(), "a.txt", b"first").await;
    env.upload(&amy, amy.root(), "b.txt", b"second").await;
    let nas = add_nas(&env, "nas").await;
    let id = make(&env, json!({ "source": "local", "targets": [{ "location": "nas" }] })).await;
    assert_eq!(settle(&env, &id).await, ["done"]);
    // Damaged (same length) and missing
    std::fs::write(stored(&nas, b"first"), b"FIRST").unwrap();
    std::fs::remove_file(stored(&nas, b"second")).unwrap();
    let Json(_) = api::verify(State(env.st.clone()), Admin(env.admin().await), Path(id.clone()), Json(serde_json::from_value(json!({})).unwrap())).await.unwrap();
    let ran = run_queued(&env, &id).await;
    assert_eq!(ran[0], "done");
    assert_eq!(copies(&env, "nas").await.iter().filter(|(_, s)| s == "corrupt").count(), 2);
    assert_eq!(health(&env, &id).await.targets[0].state, "corrupt");
    // The check asked for a repair: from the primary, checked again
    assert_eq!(run_queued(&env, &id).await, ["done"]);
    assert_eq!(std::fs::read(stored(&nas, b"first")).unwrap(), b"first");
    assert_eq!(std::fs::read(stored(&nas, b"second")).unwrap(), b"second");
    assert!(copies(&env, "nas").await.iter().all(|(_, s)| s == "verified"));
    // With no whole copy anywhere, nothing else is put in its place
    std::fs::write(testutil::blob_file(&env, b"first"), b"FIRST").unwrap();
    std::fs::write(stored(&nas, b"first"), b"FIRST").unwrap();
    sqlx::query("UPDATE replica_copies SET state = 'corrupt'").execute(&env.st.db).await.unwrap();
    let ran = settle(&env, &id).await;
    let ran = if ran.is_empty() {
        let Json(_) = api::sync(State(env.st.clone()), Admin(env.admin().await), Path(id.clone()), Json(serde_json::from_value(json!({})).unwrap())).await.unwrap();
        run_queued(&env, &id).await
    } else {
        ran
    };
    assert!(ran[0].starts_with("failed"), "{ran:?}");
    delete_due(&env, "nas").await;
    assert_eq!(std::fs::read(stored(&nas, b"first")).unwrap_or_default(), b"", "the damaged copy went, nothing replaced it");
}

#[tokio::test]
async fn a_target_is_promoted_after_its_primary_fails_and_the_old_primary_rejoins_checked() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let amy = env.user("amy", true).await;
    let a = env.upload(&amy, amy.root(), "a.txt", b"kept on both").await;
    let nas = add_nas(&env, "nas").await;
    let id = make(&env, json!({ "source": "local", "targets": [{ "location": "nas", "mode": "scheduled", "schedule": { "daily": "03:00" }, "tz": "UTC" }] })).await;
    assert_eq!(settle(&env, &id).await, ["done"], "a new scheduled target is synced once at once");
    // A file the scheduled target doesn't have yet
    let late = env.upload(&amy, amy.root(), "late.txt", b"not replicated yet").await;
    let preview = |target: &str| {
        let (st, id, target) = (env.st.clone(), id.clone(), target.to_string());
        let admin = admin.clone();
        async move {
            let q = serde_json::from_value(json!({ "target": target })).unwrap();
            let Json(p) = api::promote_preview(State(st), Admin(admin), Path(id), Query(q)).await.unwrap();
            serde_json::to_value(&p).unwrap()
        }
    };
    // While the primary works, a target that is behind isn't promoted
    let p = preview("nas").await;
    assert_eq!((p["moved"].as_i64(), p["missing"].as_i64()), (Some(1), Some(1)), "{p}");
    assert!(p["problem"].as_str().unwrap().contains("Sync it first"), "{p}");
    // The primary fails: promoting loses the late file until it is back, and has to be accepted
    fail_builtin(&env);
    let p = preview("nas").await;
    assert_eq!((p["needs_accept"].as_bool(), p["problem"].is_null()), (Some(true), true), "{p}");
    assert!(p["spaces"].as_array().unwrap().iter().any(|s| s == "My files · amy"), "{p}");
    let req = |accept: bool| serde_json::from_value(json!({ "target": "nas", "accept_missing": accept })).unwrap();
    assert!(api::promote(State(env.st.clone()), Admin(admin.clone()), Path(id.clone()), Json(req(false))).await.is_err());
    let Json(done) = api::promote(State(env.st.clone()), Admin(admin.clone()), Path(id.clone()), Json(req(true))).await.unwrap();
    assert_eq!((done["moved"].as_i64(), done["missing"].as_i64()), (Some(1), Some(1)));
    // The files are read from their new location; the late one can't be read (no copy of it was made)
    assert_eq!(read(&env, &amy, &a).await.unwrap(), b"kept on both");
    assert!(read(&env, &amy, &late).await.is_err());
    let (location,): (String,) =
        sqlx::query_as("SELECT location_id FROM drives WHERE id = ?").bind(env.drive_of(amy.root()).await).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(location, "nas");
    // New files go there
    let b = env.upload(&amy, amy.root(), "b.txt", b"after the promotion").await;
    assert!(stored(&nas, b"after the promotion").is_file());
    // The old primary is fenced: nothing new is stored there, and its copies are kept (not checked yet)
    assert!(super::fenced(&mut env.st.db.acquire().await.unwrap(), "local").await.unwrap());
    assert_eq!(copies(&env, "local").await, [(crate::util::sha256_hex(b"kept on both"), "stale".to_string())]);
    crate::tree::defer_blob_removal(&env.st, &[(crate::util::sha256_hex(b"kept on both"), "local".to_string())], 0).await;
    // The old disk is back: checked, then it counts again as a replica; nothing failed back by itself
    repair_builtin(&env);
    delete_due(&env, "local").await;
    assert!(testutil::blob_file(&env, b"kept on both").is_file(), "an old primary's copy isn't deleted");
    let ran = settle(&env, &id).await;
    assert_eq!(ran, ["done"]);
    assert_eq!(read(&env, &amy, &late).await.unwrap(), b"not replicated yet", "readable again where it is");
    let (state,): (String,) =
        sqlx::query_as("SELECT state FROM replica_targets WHERE policy_id = ? AND location_id = 'local'").bind(&id).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(state, "active");
    assert!(
        copies(&env, "local").await.iter().any(|(h, s)| *h == crate::util::sha256_hex(b"after the promotion") && s == "verified"),
        "new files are replicated back"
    );
    let (location,): (String,) =
        sqlx::query_as("SELECT location_id FROM drives WHERE id = ?").bind(env.drive_of(amy.root()).await).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(location, "nas", "no automatic failback");
    let _ = b;
    // Activity
    let (promoted,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM activity WHERE action = 'replica_promote'").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(promoted, 1);
}

#[tokio::test]
async fn a_removed_target_keeps_its_copies_until_they_are_removed_as_not_needed() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    env.upload(&amy, amy.root(), "a.txt", b"two copies").await;
    let nas = add_nas(&env, "nas").await;
    let bucket_dir = env.dir.join("bucket");
    add_location(&env, "bucket", "s3", json!({ "endpoint": "https://s3.example.com", "bucket": "b" }), Arc::new(storage::S3Storage::in_memory("tf"))).await;
    let id = make(&env, json!({ "source": "local", "copies": 2, "targets": [{ "location": "nas" }, { "location": "bucket" }] })).await;
    assert_eq!(settle(&env, &id).await, ["done", "done"]);
    assert_eq!(health(&env, &id).await.current, 2);
    assert_eq!(copies(&env, "bucket").await.len(), 1);
    // One copy is enough now, and the bucket goes from the list: its copy stays until removed
    let Json(_) = api::update(
        State(env.st.clone()),
        Admin(env.admin().await),
        Path(id.clone()),
        Json(serde_json::from_value(json!({ "copies": 1, "targets": [{ "location": "nas" }] })).unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(settle(&env, &id).await.len(), 0);
    assert_eq!(copies(&env, "bucket").await.len(), 1);
    let Json(o) = api::list(State(env.st.clone()), Admin(env.admin().await)).await.unwrap();
    let o = serde_json::to_value(&o).unwrap();
    assert_eq!(o["unneeded"][0][0], "bucket", "{o}");
    let e = crate::locations::delete(State(env.st.clone()), Admin(env.admin().await), Path("bucket".to_string())).await.unwrap_err();
    assert!(e.message.contains("holds replicas"), "{}", e.message);
    // Removed on request: then the location can go
    let Json(v) = api::purge(State(env.st.clone()), Admin(env.admin().await), Json(serde_json::from_value(json!({ "location": "bucket" })).unwrap())).await.unwrap();
    assert_eq!(v["removed"], 1);
    delete_due(&env, "bucket").await;
    let s = env.st.storage("bucket").unwrap();
    assert!(s.stat(&storage::join_key(&["blobs", &crate::util::sha256_hex(b"two copies")[0..2]])).await.unwrap().is_none());
    let _ = crate::locations::delete(State(env.st.clone()), Admin(env.admin().await), Path("bucket".to_string())).await.unwrap();
    // The one still wanted stays
    assert!(stored(&nas, b"two copies").is_file());
    let _ = bucket_dir;
}

/// Replicas on SFTP and FTP locations (the test servers of storage/sftp.rs and storage/ftp.rs), read back when copied and checked
#[tokio::test]
async fn replicas_on_sftp_and_ftp_are_checked_and_read_when_the_primary_fails() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let big: &'static [u8] = vec![3u8; 200_000].leak();
    let a = env.upload(&amy, amy.root(), "big.bin", big).await;
    let sftp = crate::storage::sftp::tests::server(testutil::password()).await;
    let ftp = crate::storage::ftp::tests::server(testutil::password()).await;
    add_location(
        &env,
        "sftp",
        "sftp",
        json!({ "host": "127.0.0.1", "path": "/files" }),
        Arc::new(crate::storage::sftp::tests::storage(&sftp, testutil::password(), &sftp.fingerprint)),
    )
    .await;
    add_location(&env, "ftp", "ftp", json!({ "host": "127.0.0.2", "path": "/files" }), Arc::new(crate::storage::ftp::tests::storage(&ftp, testutil::password())))
        .await;
    let id = make(&env, json!({ "source": "local", "copies": 2, "targets": [{ "location": "sftp" }, { "location": "ftp" }] })).await;
    assert_eq!(settle(&env, &id).await, ["done", "done"]);
    let Json(_) = api::verify(State(env.st.clone()), Admin(env.admin().await), Path(id.clone()), Json(serde_json::from_value(json!({})).unwrap())).await.unwrap();
    assert_eq!(run_queued(&env, &id).await, ["done", "done"]);
    assert_eq!(health(&env, &id).await.current, 2);
    fail_builtin(&env);
    assert_eq!(read(&env, &amy, &a).await.unwrap(), big);
    repair_builtin(&env);
}

#[tokio::test]
async fn targets_in_the_same_place_are_refused_and_too_few_targets_are_reported() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let inside = env.dir.join("blobs").join("inside");
    add_location(&env, "inside", "local", json!({ "path": inside.to_string_lossy() }), Arc::new(LocalStorage::create(inside, "inside").unwrap())).await;
    add_nas(&env, "nas").await;
    let refused = |body: serde_json::Value| {
        let (st, admin) = (env.st.clone(), admin.clone());
        async move { api::create(State(st), Admin(admin), Json(serde_json::from_value(body).unwrap())).await.unwrap_err().message }
    };
    assert!(refused(json!({ "source": "local", "targets": [{ "location": "inside" }] })).await.contains("inside the other"));
    assert!(refused(json!({ "source": "local", "targets": [{ "location": "local" }] })).await.contains("other locations"));
    assert!(refused(json!({ "source": "local", "targets": [{ "location": "nas" }, { "location": "nas" }] })).await.contains("twice"));
    assert!(refused(json!({ "source": "local", "targets": [] })).await.contains("Choose where"));
    // Two copies wanted, one target: said
    let id = make(&env, json!({ "source": "local", "copies": 2, "targets": [{ "location": "nas" }] })).await;
    let h = health(&env, &id).await;
    assert!(h.shortfall);
    assert_eq!(h.state, "degraded");
}

#[tokio::test]
async fn a_sync_waits_for_its_target_continues_after_a_restart_and_is_refused_after_a_promotion() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    env.upload(&amy, amy.root(), "a.txt", b"a").await;
    let nas = add_nas(&env, "nas").await;
    add_nas(&env, "other").await;
    let id = make(&env, json!({ "source": "local", "targets": [{ "location": "nas" }, { "location": "other" }] })).await;
    // The target isn't there: its sync waits, and is tried again once it is back
    std::fs::rename(&nas, env.dir.join("unplugged")).unwrap();
    let ran = settle(&env, &id).await;
    assert!(ran.iter().any(|r| r.starts_with("waiting")), "{ran:?}");
    assert_eq!(health(&env, &id).await.state, "degraded");
    std::fs::rename(env.dir.join("unplugged"), &nas).unwrap();
    // Not before its time, unless the location was seen working again since
    assert!(settle(&env, &id).await.is_empty());
    let back = crate::state::LocationHealth { ok: true, error: None, checked_at: crate::util::now() + 1 };
    env.st.location_health.lock().unwrap().insert("nas".into(), back);
    assert_eq!(settle(&env, &id).await, ["done"]);
    // ThirtyFile stopped during a sync: it waits for its turn again
    env.upload(&amy, amy.root(), "b.txt", b"b").await;
    let t = crate::util::now();
    policy::tick(&env.st, t).await.unwrap();
    policy::tick(&env.st, t + crate::backups::scheduler::BATCH_SECONDS).await.unwrap();
    sqlx::query("UPDATE replica_jobs SET state = 'running' WHERE state = 'queued'").execute(&env.st.db).await.unwrap();
    runner::recover_in(&env.st, &env.st.part::<Memory>().queue).await.unwrap();
    let (queued,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM replica_jobs WHERE state = 'queued'").fetch_one(&env.st.db).await.unwrap();
    assert!(queued >= 1);
    // A promotion meanwhile: the syncs asked for before are refused, not run with an old view
    sqlx::query("UPDATE replica_policies SET epoch = epoch + 1").execute(&env.st.db).await.unwrap();
    let ran = run_queued(&env, &id).await;
    assert!(!ran.is_empty() && ran.iter().all(|r| r == "cancelled"), "{ran:?}");
    // Asked for again, it runs with the policy as it is
    assert_eq!(settle(&env, &id).await.iter().filter(|r| *r == "done").count(), 2);
}

// ───────────── Folder spaces ─────────────

async fn save(env: &TestEnv, user: &User, id: &str, body: &'static [u8]) {
    let _ =
        crate::files::save_content(State(env.st.clone()), user.clone(), Path(id.to_string()), HeaderMap::new(), axum::body::Bytes::from_static(body)).await.unwrap();
}

async fn trash(env: &TestEnv, user: &User, id: &str) {
    let req = serde_json::from_value(json!({ "ids": [id] })).unwrap();
    let _ = crate::nodes::trash(State(env.st.clone()), user.clone(), Json(req)).await.unwrap();
}

fn has(list: &[(String, String)], content: &[u8]) -> bool {
    list.iter().any(|(h, s)| *h == crate::util::sha256_hex(content) && s == "verified")
}

#[tokio::test]
async fn folder_spaces_are_read_from_their_folder_kept_from_cleanup_and_read_when_the_folder_fails() {
    let env = testutil::folders_env().await;
    let admin = env.admin().await;
    let amy = env.user("amy", true).await;
    let company = env.st.shared_root().unwrap();
    let (all, mine) = (env.drive_of(&company).await, env.drive_of(amy.root()).await);
    let folder = env.dir.join("blobs").join("company");
    let docs = env.folder(&admin, &company, "Docs").await;
    let plan = env.upload(&admin, &docs, "plan.txt", b"plan, first").await;
    save(&env, &admin, &plan, b"plan, second").await;
    let old = env.upload(&admin, &docs, "old.txt", b"in the trash").await;
    trash(&env, &admin, &old).await;
    // The same content twice, in two spaces
    env.upload(&admin, &company, "same.txt", b"shared content").await;
    let diary = env.upload(&amy, amy.root(), "diary.txt", b"shared content").await;
    let nas = add_nas(&env, "nas").await;
    let id = make(&env, json!({ "source": "local", "targets": [{ "location": "nas" }] })).await;
    assert_eq!(settle(&env, &id).await, ["done"]);
    // Files, earlier versions and the trash, each content once
    let held = copies(&env, "nas").await;
    for content in [&b"plan, second"[..], b"plan, first", b"in the trash", b"shared content"] {
        assert!(has(&held, content), "{content:?}");
        assert!(stored(&nas, content).is_file());
    }
    assert_eq!(held.len(), 4);
    let h = health(&env, &id).await;
    assert_eq!((h.state, h.current, h.targets[0].held, h.targets[0].wanted), ("ok", 1, Some(4), Some(4)), "{h:?}");
    // A file put there by another program is copied once the check for changes has seen it
    testutil::write_old(&folder.join("scan.pdf"), b"scanned by a copier");
    crate::folders::scan(&env.st, &all).await.unwrap();
    assert_eq!(settle(&env, &id).await, ["done"]);
    assert!(has(&copies(&env, "nas").await, b"scanned by a copier"));
    // The copies aren't taken for unused content, nor deleted by a deletion of that content there
    let s = env.st.storage("nas").unwrap();
    let (unused, _) = crate::location_tools::unused_scan(&env.st, "nas", s.as_ref(), crate::util::now() + 86400 * 10, &|_| {}).await.unwrap();
    assert!(unused.is_empty(), "{unused:?}");
    crate::tree::defer_blob_removal(&env.st, &[(crate::util::sha256_hex(b"plan, second"), "nas".to_string())], 0).await;
    delete_due(&env, "nas").await;
    assert!(stored(&nas, b"plan, second").is_file());
    // Failures in personal spaces aren't named
    let job = runner::Job {
        id: String::new(),
        kind: "sync".into(),
        set_id: id.clone(),
        snapshot_id: Some("nas".into()),
        params: "{}".into(),
        label: String::new(),
        created_by: None,
        created_by_name: String::new(),
    };
    assert!(runner::Engine::private(&REPLICAS, &env.st, &job).await.unwrap().contains(&mine));
    // The disk fails: the files are read from their copies, as they were read, and open as usual
    fail_builtin(&env);
    let offline = crate::state::LocationHealth { ok: false, error: Some("The folder isn't there".into()), checked_at: 0 };
    env.st.location_health.lock().unwrap().insert("local".into(), offline);
    assert_eq!(read(&env, &admin, &plan).await.unwrap(), b"plan, second");
    let Json(info) = crate::nodes::get(State(env.st.clone()), admin.clone(), Path(plan.clone())).await.unwrap();
    assert!(serde_json::to_value(&info).unwrap()["offline"].is_null());
    let h = health(&env, &id).await;
    assert_eq!((h.state, h.source_offline.as_deref()), ("degraded", Some("Local disk can't be reached now")), "{h:?}");
    assert_eq!(read(&env, &amy, &diary).await.unwrap(), b"shared content");
    let (logged,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM activity WHERE action = 'replica_read'").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(logged, 1);
    // A sync meanwhile says so, and lets go of nothing
    sqlx::query("UPDATE space_changes SET seq = seq + 1").execute(&env.st.db).await.unwrap();
    let ran = settle(&env, &id).await;
    assert!(!ran.is_empty() && ran.iter().all(|r| r.starts_with("failed")), "{ran:?}");
    assert_eq!(copies(&env, "nas").await.len(), 5);
    // Without reading from copies, not
    let Json(_) =
        api::update(State(env.st.clone()), Admin(admin.clone()), Path(id.clone()), Json(serde_json::from_value(json!({ "read_fallback": false })).unwrap()))
            .await
            .unwrap();
    assert!(read(&env, &admin, &plan).await.is_err());
    let Json(info) = crate::nodes::get(State(env.st.clone()), admin.clone(), Path(plan.clone())).await.unwrap();
    assert!(!serde_json::to_value(&info).unwrap()["offline"].is_null());
    repair_builtin(&env);
    env.st.location_health.lock().unwrap().remove("local");
    assert_eq!(read(&env, &admin, &plan).await.unwrap(), b"plan, second");
}

/// The failures and notes of a policy's last job
async fn last_job(env: &TestEnv, policy: &str) -> (i64, String, Option<String>) {
    sqlx::query_as("SELECT failed_items, failures, note FROM replica_jobs WHERE policy_id = ? ORDER BY created_at DESC, rowid DESC LIMIT 1")
        .bind(policy)
        .fetch_one(&env.st.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_folder_file_changed_after_its_check_is_copied_as_read_and_content_nothing_uses_lets_go_of_its_copy() {
    let env = testutil::folders_env().await;
    let company = env.st.shared_root().unwrap();
    let all = env.drive_of(&company).await;
    let folder = env.dir.join("blobs").join("company");
    testutil::write_old(&folder.join("report.txt"), b"report, as indexed");
    crate::folders::scan(&env.st, &all).await.unwrap();
    // Changed by another program just now: the check for changes leaves it for later, the sync copies it as it read
    // it whole, and says so
    std::fs::write(folder.join("report.txt"), b"report, being written").unwrap();
    add_nas(&env, "nas").await;
    let id = make(&env, json!({ "source": "local", "targets": [{ "location": "nas" }] })).await;
    assert_eq!(settle(&env, &id).await, ["done"]);
    let held = copies(&env, "nas").await;
    assert!(!has(&held, b"report, as indexed") && has(&held, b"report, being written"), "{held:?}");
    let (failed, failures, note) = last_job(&env, &id).await;
    assert_eq!(failed, 0, "{failures}");
    assert!(failures.contains("report.txt") && failures.contains(super::folders::KEPT_AS_READ), "{failures}");
    assert_eq!(note.as_deref(), Some("1 content was copied\n1 file kept changing while it was copied: it is kept as it was last read"));
    let h = health(&env, &id).await;
    assert_eq!((h.targets[0].held, h.targets[0].wanted, h.state), (Some(1), Some(1), "ok"), "{h:?}");
    // Once it settled and was seen, it is copied as it is
    testutil::write_old(&folder.join("report.txt"), b"report, final");
    crate::folders::scan(&env.st, &all).await.unwrap();
    assert_eq!(settle(&env, &id).await, ["done"]);
    let held = copies(&env, "nas").await;
    assert!(has(&held, b"report, final") && !has(&held, b"report, being written"), "{held:?}");
    assert_eq!(health(&env, &id).await.state, "ok");
    // Changed again: the new content is copied, the old one's copy goes
    testutil::write_old(&folder.join("report.txt"), b"report, corrected");
    crate::folders::scan(&env.st, &all).await.unwrap();
    assert_eq!(settle(&env, &id).await, ["done"]);
    let held = copies(&env, "nas").await;
    assert!(has(&held, b"report, corrected") && !has(&held, b"report, final"), "{held:?}");
    // Deleted: its copy goes too
    std::fs::remove_file(folder.join("report.txt")).unwrap();
    crate::folders::scan(&env.st, &all).await.unwrap();
    assert_eq!(settle(&env, &id).await, ["done"]);
    assert!(copies(&env, "nas").await.is_empty());
}

#[tokio::test]
async fn a_folder_file_that_keeps_changing_keeps_its_copy_as_last_read_and_is_listed_without_naming_personal_files() {
    let env = testutil::folders_env().await;
    let amy = env.user("amy", true).await;
    let company = env.st.shared_root().unwrap();
    let (all, mine) = (env.drive_of(&company).await, env.drive_of(amy.root()).await);
    let (company_dir, amy_dir) = (env.dir.join("blobs").join("company"), env.dir.join("blobs").join("users").join("amy"));
    testutil::write_old(&company_dir.join("steady.txt"), b"steady");
    testutil::write_old(&company_dir.join("app.log"), b"line 1;");
    testutil::write_old(&amy_dir.join("diary.txt"), b"day 1;");
    crate::folders::scan(&env.st, &all).await.unwrap();
    crate::folders::scan(&env.st, &mine).await.unwrap();
    add_nas(&env, "nas").await;
    let id = make(&env, json!({ "source": "local", "targets": [{ "location": "nas" }] })).await;
    assert_eq!(settle(&env, &id).await, ["done"]);
    // Other programs write these all the time from now on: every read finds them changing. A new one was never read.
    testutil::write_old(&company_dir.join("app.log"), b"line 1; line 2;");
    testutil::write_old(&amy_dir.join("diary.txt"), b"day 1; day 2;");
    testutil::write_old(&company_dir.join("new.log"), b"started");
    crate::folders::scan(&env.st, &all).await.unwrap();
    crate::folders::scan(&env.st, &mine).await.unwrap();
    let changing = [(all.clone(), "app.log"), (mine.clone(), "diary.txt"), (all.clone(), "new.log")];
    let keep_changing = |on: bool| {
        let mut list = super::folders::KEEPS_CHANGING.lock().unwrap();
        list.retain(|(s, _)| *s != all && *s != mine);
        if on {
            list.extend(changing.iter().map(|(s, p)| (s.clone(), p.to_string())));
        }
    };
    keep_changing(true);
    let ran = settle(&env, &id).await;
    keep_changing(false);
    // Not failed: the copies as last read stay, and they are listed, never by name in a personal space
    assert_eq!(ran, ["done"]);
    let (failed, failures, note) = last_job(&env, &id).await;
    assert_eq!(failed, 0, "{failures}");
    assert!(failures.contains("app.log") && failures.contains(super::folders::KEPT_AS_READ), "{failures}");
    assert!(failures.contains("new.log") && failures.contains(super::folders::NOT_COPIED), "{failures}");
    assert!(!failures.contains("diary") && failures.contains("\"item\":null"), "{failures}");
    assert_eq!(
        note.as_deref(),
        Some(
            "2 files kept changing while they were copied: each is kept as it was last read\n\
             1 file kept changing while it was copied and has no copy yet: it is tried again at the next sync"
        )
    );
    let held = copies(&env, "nas").await;
    for content in [&b"steady"[..], b"line 1;", b"day 1;"] {
        assert!(has(&held, content), "{content:?}");
    }
    // A new target gets the copies as last read too, from the one that has them
    add_nas(&env, "usb").await;
    keep_changing(true);
    let Json(_) = api::update(
        State(env.st.clone()),
        Admin(env.admin().await),
        Path(id.clone()),
        Json(serde_json::from_value(json!({ "copies": 2, "targets": [{ "location": "nas" }, { "location": "usb" }] })).unwrap()),
    )
    .await
    .unwrap();
    let ran = settle(&env, &id).await;
    keep_changing(false);
    assert_eq!(ran, ["done"]);
    let usb = copies(&env, "usb").await;
    assert!(has(&usb, b"line 1;") && has(&usb, b"day 1;") && !has(&usb, b"started"), "{usb:?}");
    // Tried again by the next sync: once they settle, they are copied as they are, and the old copies go
    let Json(_) = api::sync(State(env.st.clone()), Admin(env.admin().await), Path(id.clone()), Json(serde_json::from_value(json!({})).unwrap())).await.unwrap();
    assert_eq!(run_queued(&env, &id).await, ["done", "done"]);
    let held = copies(&env, "nas").await;
    for content in [&b"line 1; line 2;"[..], b"day 1; day 2;", b"started"] {
        assert!(has(&held, content), "{content:?}");
    }
    assert!(!has(&held, b"line 1;") && !has(&held, b"day 1;"), "{held:?}");
    let (failed, failures, _) = last_job(&env, &id).await;
    assert_eq!((failed, failures.as_str()), (0, "[]"));
}

#[tokio::test]
async fn a_folder_space_is_promoted_into_a_content_store_on_the_target_and_its_folder_stays() {
    let env = testutil::folders_env().await;
    let admin = env.admin().await;
    let amy = env.user("amy", true).await;
    let company = env.st.shared_root().unwrap();
    let (all, mine) = (env.drive_of(&company).await, env.drive_of(amy.root()).await);
    let docs = env.folder(&admin, &company, "Docs").await;
    let plan = env.upload(&admin, &docs, "plan.txt", b"plan, first").await;
    save(&env, &admin, &plan, b"plan, second").await;
    let old = env.upload(&admin, &docs, "old.txt", b"in the trash").await;
    trash(&env, &admin, &old).await;
    let diary = env.upload(&amy, amy.root(), "diary.txt", b"dear diary").await;
    let nas = add_nas(&env, "nas").await;
    let id = make(&env, json!({ "source": "local", "targets": [{ "location": "nas" }] })).await;
    assert_eq!(settle(&env, &id).await, ["done"]);
    // Amy adds a file that isn't replicated yet
    let late = env.upload(&amy, amy.root(), "late.txt", b"not replicated yet").await;
    let preview = |target: &str| {
        let (st, id, target, admin) = (env.st.clone(), id.clone(), target.to_string(), admin.clone());
        async move {
            let q = serde_json::from_value(json!({ "target": target })).unwrap();
            let Json(p) = api::promote_preview(State(st), Admin(admin), Path(id), Query(q)).await.unwrap();
            serde_json::to_value(&p).unwrap()
        }
    };
    let p = preview("nas").await;
    assert!(p["problem"].as_str().unwrap().contains("Sync it first"), "{p}");
    // The disk fails: the company space is wholly on the NAS, Amy's isn't and stays
    fail_builtin(&env);
    let p = preview("nas").await;
    assert_eq!((p["needs_accept"].as_bool(), p["problem"].is_null(), p["missing"].as_i64()), (Some(true), true, Some(1)), "{p}");
    assert_eq!(p["folder_spaces"], json!(["My files · amy"]), "{p}");
    assert!(p["spaces"].as_array().unwrap().iter().any(|s| s == "All files"), "{p}");
    let req = serde_json::from_value(json!({ "target": "nas", "accept_missing": true })).unwrap();
    let Json(done) = api::promote(State(env.st.clone()), Admin(admin.clone()), Path(id.clone()), Json(req)).await.unwrap();
    assert_eq!(done["missing"].as_i64(), Some(1), "{done}");
    let state = |drive: String| {
        let st = env.st.clone();
        async move {
            let row: (String, Option<String>, Option<String>) =
                sqlx::query_as("SELECT mode, location_id, source_path FROM drives WHERE id = ?").bind(drive).fetch_one(&st.db).await.unwrap();
            row
        }
    };
    assert_eq!(state(all.clone()).await, ("store".into(), Some("nas".into()), None));
    assert_eq!(state(mine.clone()).await.0, "folder");
    // The company's files, earlier versions and trash are read from the NAS now
    assert_eq!(read(&env, &admin, &plan).await.unwrap(), b"plan, second");
    let (v,): (String,) = sqlx::query_as("SELECT id FROM node_versions WHERE node_id = ?").bind(&plan).fetch_one(&env.st.db).await.unwrap();
    let q = Query(serde_json::from_value(json!({})).unwrap());
    let res = crate::versions::content(State(env.st.clone()), admin.clone(), Path((plan.clone(), v)), q, HeaderMap::new()).await.unwrap();
    assert_eq!(axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec(), b"plan, first");
    let req = serde_json::from_value(json!({ "ids": [old] })).unwrap();
    let _ = crate::nodes::restore(State(env.st.clone()), admin.clone(), Json(req)).await.unwrap();
    assert_eq!(read(&env, &admin, &old).await.unwrap(), b"in the trash");
    // New files go there
    env.upload(&admin, &company, "new.txt", b"after the promotion").await;
    assert!(stored(&nas, b"after the promotion").is_file());
    // Amy's files wait for the old disk, which is back unchanged: nothing was deleted from it
    assert!(read(&env, &amy, &late).await.is_err());
    repair_builtin(&env);
    assert_eq!(read(&env, &amy, &late).await.unwrap(), b"not replicated yet");
    assert_eq!(read(&env, &amy, &diary).await.unwrap(), b"dear diary");
    assert_eq!(std::fs::read(env.dir.join("blobs/company/Docs/plan.txt")).unwrap(), b"plan, second");
    // Amy's copies stay, though no policy takes her space now
    super::sync::release(&env.st, "nas").await.unwrap();
    assert!(has(&copies(&env, "nas").await, b"dear diary"));
}

/// An upload during the network preflight must not bypass missing-content acceptance.
#[tokio::test]
async fn a_promotion_refuses_content_missing_since_its_preflight() {
    use futures_util::future::BoxFuture;
    struct PausedPing {
        inner: Arc<dyn Storage>,
        entered: Arc<tokio::sync::Notify>,
        proceed: Arc<tokio::sync::Notify>,
    }
    impl Storage for PausedPing {
        fn put_file<'a>(&'a self, h: &'a str, p: &'a std::path::Path) -> BoxFuture<'a, std::io::Result<()>> {
            self.inner.put_file(h, p)
        }
        fn open<'a>(&'a self, h: &'a str, s: u64, n: u64) -> BoxFuture<'a, std::io::Result<storage::BoxReader>> {
            self.inner.open(h, s, n)
        }
        fn delete<'a>(&'a self, h: &'a str) -> BoxFuture<'a, std::io::Result<()>> {
            self.inner.delete(h)
        }
        fn check(&self) -> BoxFuture<'_, std::io::Result<()>> {
            self.inner.check()
        }
        fn ping(&self) -> BoxFuture<'_, std::io::Result<()>> {
            Box::pin(async move {
                self.entered.notify_one();
                self.proceed.notified().await;
                self.inner.ping().await
            })
        }
    }
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    env.upload(&amy, amy.root(), "replicated.txt", b"on both locations").await;
    add_nas(&env, "nas").await;
    let id = make(&env, json!({"source":"local", "targets":[{"location":"nas"}]})).await;
    assert_eq!(settle(&env, &id).await, ["done"]);
    let entered = Arc::new(tokio::sync::Notify::new());
    let proceed = Arc::new(tokio::sync::Notify::new());
    let inner = env.st.storage("nas").unwrap();
    env.st.storages.write().unwrap().insert("nas".into(), Arc::new(PausedPing { inner, entered: entered.clone(), proceed: proceed.clone() }));
    let st = env.st.clone();
    let admin = env.admin().await;
    let task = tokio::spawn(async move {
        let req = serde_json::from_value(json!({"target":"nas","accept_missing":false})).unwrap();
        api::promote(State(st), Admin(admin), Path(id), Json(req)).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(30), entered.notified()).await.unwrap();
    let late = env.upload(&amy, amy.root(), "late.txt", b"concurrent upload, not on target").await;
    proceed.notify_one();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(30), task).await.unwrap().unwrap();
    assert!(outcome.is_err(), "a concurrent upload must invalidate the preflight");
    let (source, epoch): (String, i64) = sqlx::query_as("SELECT source_location,epoch FROM replica_policies").fetch_one(&env.st.db).await.unwrap();
    assert_eq!((source.as_str(), epoch), ("local", 1));
    assert_eq!(read(&env, &amy, &late).await.unwrap(), b"concurrent upload, not on target");
}

/// A copy on a target is replaced in one step, never deleted first: the target may have become where the content is
/// kept meanwhile (a promotion), and deleting it there would lose it
#[tokio::test]
async fn a_copy_of_another_length_is_replaced_without_deleting_what_is_there_first() {
    use futures_util::future::BoxFuture;
    struct Watched {
        inner: Arc<dyn Storage>,
        deletes: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl Storage for Watched {
        fn put_file<'a>(&'a self, h: &'a str, p: &'a std::path::Path) -> BoxFuture<'a, std::io::Result<()>> {
            self.inner.put_file(h, p)
        }
        fn open<'a>(&'a self, h: &'a str, s: u64, n: u64) -> BoxFuture<'a, std::io::Result<storage::BoxReader>> {
            self.inner.open(h, s, n)
        }
        fn delete<'a>(&'a self, h: &'a str) -> BoxFuture<'a, std::io::Result<()>> {
            self.deletes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.delete(h)
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
        fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, std::io::Result<Option<storage::Entry>>> {
            self.inner.stat(key)
        }
        fn put_at<'a>(&'a self, key: &'a str, src: &'a std::path::Path) -> BoxFuture<'a, std::io::Result<()>> {
            self.inner.put_at(key, src)
        }
    }
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    env.upload(&amy, amy.root(), "a.txt", b"the content").await;
    let nas = add_nas(&env, "nas").await;
    // Left there by a copy that stopped halfway
    std::fs::create_dir_all(stored(&nas, b"the content").parent().unwrap()).unwrap();
    std::fs::write(stored(&nas, b"the content"), b"the cont").unwrap();
    let deletes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let inner = env.st.storage("nas").unwrap();
    env.st.storages.write().unwrap().insert("nas".into(), Arc::new(Watched { inner, deletes: deletes.clone() }));
    let id = make(&env, json!({ "source": "local", "targets": [{ "location": "nas" }] })).await;
    assert_eq!(settle(&env, &id).await, ["done"]);
    assert_eq!(std::fs::read(stored(&nas, b"the content")).unwrap(), b"the content");
    assert_eq!(deletes.load(std::sync::atomic::Ordering::SeqCst), 0);
}
