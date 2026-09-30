//! Backup policies: schedules, changes, retention and how a policy is doing

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
};
use serde_json::json;

use super::{
    api,
    policy::{self, Schedule},
    runner,
};
use crate::{
    auth::{Admin, User},
    storage::LocalStorage,
    testutil::{self, TestEnv},
};

fn at(s: &str, tz: &str) -> i64 {
    let tz = jiff::tz::TimeZone::get(tz).unwrap();
    s.parse::<jiff::civil::DateTime>().unwrap().to_zoned(tz).unwrap().timestamp().as_second()
}

fn local(t: i64, tz: &str) -> String {
    jiff::Timestamp::from_second(t).unwrap().to_zoned(jiff::tz::TimeZone::get(tz).unwrap()).strftime("%Y-%m-%d %H:%M %Z").to_string()
}

#[test]
fn schedules_follow_their_time_zone_and_skip_or_merge_the_hours_clocks_change() {
    let ny = "America/New_York";
    let tz = jiff::tz::TimeZone::get(ny).unwrap();
    let daily = |h, m| Schedule::Daily(h, m);
    // An ordinary day: later today, else tomorrow
    assert_eq!(local(policy::next_after(&daily(3, 0), &tz, at("2026-06-10T01:00", ny)).unwrap(), ny), "2026-06-10 03:00 EDT");
    assert_eq!(local(policy::next_after(&daily(3, 0), &tz, at("2026-06-10T03:00", ny)).unwrap(), ny), "2026-06-11 03:00 EDT");
    // 02:30 doesn't exist on 8 March 2026 (clocks go from 02:00 to 03:00): skipped
    assert_eq!(local(policy::next_after(&daily(2, 30), &tz, at("2026-03-08T00:00", ny)).unwrap(), ny), "2026-03-09 02:30 EDT");
    // 01:30 happens twice on 1 November 2026: once, the first time
    let first = policy::next_after(&daily(1, 30), &tz, at("2026-11-01T00:00", ny)).unwrap();
    assert_eq!(local(first, ny), "2026-11-01 01:30 EDT");
    assert_eq!(local(policy::next_after(&daily(1, 30), &tz, first).unwrap(), ny), "2026-11-02 01:30 EST");
    // Some days of the week (Monday is 1): 2026-06-10 is a Wednesday
    let weekly = Schedule::Weekly(22, 15, vec![1, 5]);
    assert_eq!(local(policy::next_after(&weekly, &tz, at("2026-06-10T12:00", ny)).unwrap(), ny), "2026-06-12 22:15 EDT");
    assert_eq!(local(policy::next_after(&weekly, &tz, at("2026-06-12T22:15", ny)).unwrap(), ny), "2026-06-15 22:15 EDT");
    // Another zone, the same instant: its own wall clock
    let taipei = jiff::tz::TimeZone::get("Asia/Taipei").unwrap();
    assert_eq!(local(policy::next_after(&daily(3, 0), &taipei, at("2026-06-10T01:00", ny)).unwrap(), "Asia/Taipei"), "2026-06-11 03:00 CST");
    // Every so many minutes: from the time given
    assert_eq!(policy::next_after(&Schedule::Every(90), &tz, 1_000_000), Some(1_000_000 + 5400));
    // What is accepted
    assert!(Schedule::parse(&json!({ "daily": "24:00" })).is_err());
    assert!(Schedule::parse(&json!({ "every": 1 })).is_err());
    assert!(Schedule::parse(&json!({ "weekly": "03:00", "days": [] })).is_err());
    assert_eq!(Schedule::parse(&json!({ "weekly": "3:05", "days": [5, 1, 5] })).unwrap(), Schedule::Weekly(3, 5, vec![1, 5]));
    assert!(policy::time_zone("Mars/Olympus_Mons").is_err());
}

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

async fn make_policy(env: &TestEnv, body: serde_json::Value) -> String {
    let mut req = json!({ "source": "local", "dest": "nas", "mode": "both", "schedule": { "daily": "03:00" }, "tz": "Asia/Taipei" });
    for (k, v) in body.as_object().unwrap() {
        req[k] = v.clone();
    }
    let Json(v) = api::create_policy(State(env.st.clone()), Admin(env.admin().await), Json(serde_json::from_value(req).unwrap())).await.unwrap();
    v["set_id"].as_str().unwrap().to_string()
}

/// The snapshot jobs of a set that aren't over, oldest first
async fn open_jobs(env: &TestEnv, set: &str) -> Vec<(String, String)> {
    sqlx::query_as("SELECT id, state FROM backup_jobs WHERE set_id = ? AND kind = 'snapshot' AND state NOT IN ('done', 'cancelled') ORDER BY created_at, rowid")
        .bind(set)
        .fetch_all(&env.st.db)
        .await
        .unwrap()
}

async fn run_job(env: &TestEnv, id: &str) -> String {
    let job = runner::job(&mut env.st.db.acquire().await.unwrap(), id).await.unwrap().unwrap();
    let ctl = runner::take(&env.st, &job).await.unwrap().expect("queued");
    runner::run(&env.st, &job, &ctl).await;
    sqlx::query_as::<_, (String,)>("SELECT state FROM backup_jobs WHERE id = ?").bind(id).fetch_one(&env.st.db).await.unwrap().0
}

/// Runs the policy's snapshot that is queued
async fn run_queued(env: &TestEnv, set: &str) -> String {
    let jobs = open_jobs(env, set).await;
    let (id, state) = jobs.first().expect("a snapshot is queued").clone();
    assert_eq!(state, "queued");
    run_job(env, &id).await
}

async fn complete(env: &TestEnv, set: &str) -> Vec<(String, i64)> {
    sqlx::query_as("SELECT id, completed_at FROM backup_snapshots WHERE set_id = ? AND state = 'complete' ORDER BY completed_at DESC, rowid DESC")
        .bind(set)
        .fetch_all(&env.st.db)
        .await
        .unwrap()
}

async fn seq(env: &TestEnv, drive: &str) -> i64 {
    sqlx::query_as::<_, (i64,)>("SELECT COALESCE((SELECT seq FROM space_changes WHERE drive_id = ?), 0)").bind(drive).fetch_one(&env.st.db).await.unwrap().0
}

async fn health(env: &TestEnv, set: &str) -> policy::Health {
    let mut c = env.st.db.acquire().await.unwrap();
    let p = policy::load(&mut c, set).await.unwrap().unwrap();
    policy::health(&mut c, &p, crate::util::now()).await.unwrap()
}

#[tokio::test]
async fn every_change_of_a_space_is_counted_in_the_transaction_that_makes_it() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let bob = env.user("bob", true).await;
    let mine = env.drive_of(amy.root()).await;
    let bobs = env.drive_of(bob.root()).await;
    let bob_before = seq(&env, &bobs).await;
    let mut last = seq(&env, &mine).await;
    let mut changed = |now: i64, what: &str| {
        assert!(now > last, "{what} is counted");
        last = now;
    };
    let f = env.upload(&amy, amy.root(), "a.txt", b"one").await;
    changed(seq(&env, &mine).await, "an upload");
    let req = serde_json::from_value(json!({ "name": "b.txt" })).unwrap();
    let _ = crate::nodes::rename(State(env.st.clone()), amy.clone(), Path(f.clone()), Json(req)).await.unwrap();
    changed(seq(&env, &mine).await, "a rename");
    let _ = crate::files::save_content(State(env.st.clone()), amy.clone(), Path(f.clone()), Default::default(), axum::body::Bytes::from_static(b"two"))
        .await
        .unwrap();
    changed(seq(&env, &mine).await, "a save, with its earlier version");
    env.grant(&f, &bob, "viewer").await;
    changed(seq(&env, &mine).await, "access given");
    let req = serde_json::from_value(json!({ "ids": [f] })).unwrap();
    let _ = crate::nodes::trash(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
    changed(seq(&env, &mine).await, "moving to the trash");
    // One row per space, however many changes: nothing to run out of
    let (rows,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM space_changes WHERE drive_id = ?").bind(&mine).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(rows, 1);
    // Bob's space didn't change
    assert_eq!(seq(&env, &bobs).await, bob_before);
}
#[tokio::test]
async fn a_check_for_changes_that_finds_none_and_counting_usage_again_dont_count_as_changes() {
    let env = testutil::folders_env().await;
    let admin = env.admin().await;
    let company = env.st.shared_root().unwrap();
    let all = env.drive_of(&company).await;
    env.upload(&admin, &company, "a.txt", b"one").await;
    crate::folders::scan(&env.st, &all).await.unwrap();
    let before = seq(&env, &all).await;
    // Nothing changed in the folder: the check writes its report only
    crate::folders::scan(&env.st, &all).await.unwrap();
    crate::tree::recompute_usage(&env.st).await.unwrap();
    assert_eq!(seq(&env, &all).await, before, "no change");
    // A change of the space's settings counts
    sqlx::query("UPDATE drives SET quota_bytes = 1000 WHERE id = ?").bind(&all).execute(&env.st.db).await.unwrap();
    assert!(seq(&env, &all).await > before, "a setting changed");
    // A file another program added counts once the check sees it
    let before = seq(&env, &all).await;
    testutil::write_old(&env.dir.join("blobs").join("company").join("scan.pdf"), b"scanned");
    crate::folders::scan(&env.st, &all).await.unwrap();
    assert!(seq(&env, &all).await > before, "a file added in the folder");
}

#[tokio::test]
async fn a_policy_backs_up_changes_one_snapshot_at_a_time_and_keeps_the_last_one_whatever_its_age() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let a = env.upload(&amy, amy.root(), "a.txt", b"first").await;
    let gone = env.upload(&amy, amy.root(), "gone.txt", b"only in the first snapshot").await;
    let nas = add_nas(&env, "nas").await;
    let set = make_policy(&env, json!({ "keep_days": 1, "keep_min": 1 })).await;
    // The first snapshot is made right away
    assert_eq!(run_queued(&env, &set).await, "done");
    assert_eq!(complete(&env, &set).await.len(), 1);
    let t = crate::util::now();
    // Nothing changed: nothing to do, and the next scheduled time is set
    policy::tick(&env.st, t).await.unwrap();
    assert!(open_jobs(&env, &set).await.is_empty());
    let next: Option<i64> = sqlx::query_as::<_, (Option<i64>,)>("SELECT next_run_at FROM backup_policies WHERE set_id = ?").bind(&set).fetch_one(&env.st.db).await.unwrap().0;
    assert!(next.is_some_and(|n| n > t));
    assert_eq!(health(&env, &set).await.state, "protected");
    // Changes: noted, then backed up once they had a few seconds. A file is deleted for good.
    let _ = crate::files::save_content(State(env.st.clone()), amy.clone(), Path(a.clone()), Default::default(), axum::body::Bytes::from_static(b"second"))
        .await
        .unwrap();
    let req = serde_json::from_value(json!({ "ids": [gone] })).unwrap();
    let _ = crate::nodes::trash(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
    let req = serde_json::from_value(json!({ "ids": [gone] })).unwrap();
    let Json(j) = crate::nodes::delete_forever(State(env.st.clone()), amy.clone(), Json(req)).await.unwrap();
    if j.state == "running" {
        crate::jobs::wait_for(&env.st, &j.id).await;
    }
    policy::tick(&env.st, t).await.unwrap();
    assert!(open_jobs(&env, &set).await.is_empty(), "changes are gathered first");
    assert_eq!(health(&env, &set).await.state, "catching_up");
    policy::tick(&env.st, t + policy::BATCH_SECONDS).await.unwrap();
    assert_eq!(open_jobs(&env, &set).await.len(), 1);
    // More changes and the schedule meanwhile: still one snapshot queued
    env.upload(&amy, amy.root(), "b.txt", b"third").await;
    sqlx::query("UPDATE backup_policies SET next_run_at = ? WHERE set_id = ?").bind(t - 86400 * 3).bind(&set).execute(&env.st.db).await.unwrap();
    policy::tick(&env.st, t + 2 * policy::BATCH_SECONDS).await.unwrap();
    assert_eq!(open_jobs(&env, &set).await.len(), 1);
    // ThirtyFile was stopped for three days: one catch-up, and the next time is in the future again
    let next: i64 = sqlx::query_as::<_, (i64,)>("SELECT next_run_at FROM backup_policies WHERE set_id = ?").bind(&set).fetch_one(&env.st.db).await.unwrap().0;
    assert!(next > t);
    assert_eq!(run_queued(&env, &set).await, "done");
    // Old snapshots go (older than a day), the newest stays
    sqlx::query("UPDATE backup_snapshots SET completed_at = completed_at - 3 * 86400 WHERE set_id = ?").bind(&set).execute(&env.st.db).await.unwrap();
    sqlx::query("UPDATE backup_objects SET created_at = created_at - 3 * 86400 WHERE set_id = ?").bind(&set).execute(&env.st.db).await.unwrap();
    let before = complete(&env, &set).await;
    assert_eq!(before.len(), 2);
    let set_row = super::load_set(&env.st.db, &set).await.unwrap();
    policy::prune(&env.st, &set_row).await.unwrap();
    let after = complete(&env, &set).await;
    assert_eq!(after, before[..1].to_vec(), "only the newest, although it is older than a day too");
    // Content only the deleted snapshot held is gone; what the kept one holds stays ("first" is an earlier version)
    let object = |content: &[u8]| {
        let h = crate::util::sha256_hex(content);
        nas.join(super::layout::object_key(&set, &h))
    };
    assert!(!object(b"only in the first snapshot").exists());
    assert!(object(b"first").exists() && object(b"second").exists() && object(b"third").exists());
    assert!(!nas.join(super::layout::complete_key(&set, &before[1].0)).exists());
    // Everything is captured: nothing waits
    policy::tick(&env.st, t + 3 * policy::BATCH_SECONDS).await.unwrap();
    assert!(open_jobs(&env, &set).await.is_empty(), "{:?}", open_jobs(&env, &set).await);
}

#[tokio::test]
async fn a_failing_or_unreachable_backup_keeps_what_it_had_is_retried_and_administrators_are_told_once() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let amy = env.user("amy", true).await;
    env.upload(&amy, amy.root(), "a.txt", b"kept").await;
    let nas = add_nas(&env, "nas").await;
    let set = make_policy(&env, json!({ "mode": "realtime" })).await;
    assert_eq!(run_queued(&env, &set).await, "done");
    let first = complete(&env, &set).await;
    // New content, damaged where it is kept: the snapshot fails, the old one stays
    env.upload(&amy, amy.root(), "b.txt", b"broken").await;
    std::fs::write(testutil::blob_file(&env, b"broken"), b"BROKEN").unwrap();
    let t = crate::util::now();
    policy::tick(&env.st, t).await.unwrap();
    policy::tick(&env.st, t + policy::BATCH_SECONDS).await.unwrap();
    assert_eq!(run_queued(&env, &set).await, "failed");
    assert_eq!(complete(&env, &set).await, first);
    policy::tick(&env.st, t + 2 * policy::BATCH_SECONDS).await.unwrap();
    assert_eq!(health(&env, &set).await.state, "failing");
    let told = |kind: &'static str| {
        let db = env.st.db.clone();
        let admin = admin.id;
        async move {
            sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM notifications WHERE user_id = ? AND kind = 'backup' AND data LIKE ?")
                .bind(admin)
                .bind(format!("%\"state\":\"{kind}\"%"))
                .fetch_one(&db)
                .await
                .unwrap()
                .0
        }
    };
    assert_eq!(told("failing").await, 1);
    policy::tick(&env.st, t + 3 * policy::BATCH_SECONDS).await.unwrap();
    assert_eq!(told("failing").await, 1, "told once");
    // Repaired: tried again (not at once: an hour after the last try), and all is well again
    std::fs::write(testutil::blob_file(&env, b"broken"), b"broken").unwrap();
    sqlx::query("UPDATE backup_policies SET last_run_at = last_run_at - 7200 WHERE set_id = ?").bind(&set).execute(&env.st.db).await.unwrap();
    policy::tick(&env.st, t + 4 * policy::BATCH_SECONDS).await.unwrap();
    assert_eq!(run_queued(&env, &set).await, "done");
    policy::tick(&env.st, t + 5 * policy::BATCH_SECONDS).await.unwrap();
    assert_eq!(health(&env, &set).await.state, "protected");
    assert_eq!(told("recovered").await, 1);
    // The location goes away: the snapshot waits for it, and is tried again once it is back
    env.upload(&amy, amy.root(), "c.txt", b"while offline").await;
    std::fs::rename(&nas, env.dir.join("unplugged")).unwrap();
    policy::tick(&env.st, t + 6 * policy::BATCH_SECONDS).await.unwrap();
    policy::tick(&env.st, t + 7 * policy::BATCH_SECONDS).await.unwrap();
    assert_eq!(run_queued(&env, &set).await, "waiting");
    policy::tick(&env.st, t + 8 * policy::BATCH_SECONDS).await.unwrap();
    assert_eq!(health(&env, &set).await.state, "waiting");
    assert_eq!(told("waiting").await, 1);
    std::fs::rename(env.dir.join("unplugged"), &nas).unwrap();
    sqlx::query("UPDATE backup_policies SET last_run_at = last_run_at - 600 WHERE set_id = ?").bind(&set).execute(&env.st.db).await.unwrap();
    policy::tick(&env.st, t + 9 * policy::BATCH_SECONDS).await.unwrap();
    assert_eq!(run_queued(&env, &set).await, "done");
    assert_eq!(complete(&env, &set).await.len(), 3);
}

#[tokio::test]
async fn a_policy_takes_the_spaces_on_its_location_when_each_snapshot_starts() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    env.upload(&amy, amy.root(), "a.txt", b"amy's").await;
    add_nas(&env, "nas").await;
    let set = make_policy(&env, json!({})).await;
    // A space added to the location meanwhile is taken; the policy doesn't need changing
    let bob = env.user("bob", true).await;
    env.upload(&bob, bob.root(), "b.txt", b"bob's").await;
    assert_eq!(run_queued(&env, &set).await, "done");
    let (list,): (String,) = sqlx::query_as("SELECT space_list FROM backup_snapshots WHERE set_id = ? AND state = 'complete'").bind(&set).fetch_one(&env.st.db).await.unwrap();
    assert!(list.contains("\"owner\":\"bob\""), "{list}");
    // Chosen spaces only: Bob's is left out
    let Json(_) = api::update_policy(
        State(env.st.clone()),
        Admin(env.admin().await),
        Path(set.clone()),
        Json(serde_json::from_value(json!({ "all_spaces": false, "spaces": [env.drive_of(amy.root()).await] })).unwrap()),
    )
    .await
    .unwrap();
    let mut c = env.st.db.acquire().await.unwrap();
    assert_eq!(policy::scope(&mut c, &set).await.unwrap(), vec![env.drive_of(amy.root()).await]);
    // Paused: nothing is made, whatever changes
    drop(c);
    let Json(_) = api::update_policy(State(env.st.clone()), Admin(env.admin().await), Path(set.clone()), Json(serde_json::from_value(json!({ "enabled": false })).unwrap()))
        .await
        .unwrap();
    env.upload(&amy, amy.root(), "c.txt", b"later").await;
    let t = crate::util::now();
    policy::tick(&env.st, t).await.unwrap();
    policy::tick(&env.st, t + 60).await.unwrap();
    assert!(open_jobs(&env, &set).await.is_empty());
    assert_eq!(health(&env, &set).await.state, "paused");
    let _: User = amy;
}
