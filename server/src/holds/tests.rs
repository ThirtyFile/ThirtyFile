// Holds and retention periods where they are enforced: every way of deleting for good, the versions a change keeps,
// deleting a space or an account, and moving out of a space
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
};
use bytes::Bytes;

use super::*;
use crate::{
    auth::User,
    jobs::Job,
    nodes::{BatchReq, TrashQuery},
    testutil,
};

const DAY: i64 = 86400;

fn ids(list: &[&str]) -> Json<BatchReq> {
    Json(serde_json::from_value(json!({ "ids": list })).unwrap())
}

async fn finished(env: &testutil::TestEnv, Json(job): Json<Job>) -> Job {
    if job.state == "running" {
        crate::jobs::wait_for(&env.st, &job.id).await;
    }
    job
}

async fn place_on(env: &testutil::TestEnv, scope: &str, target: Value, reason: &str) -> Hold {
    let admin = env.admin().await;
    let mut req = json!({ "scope": scope, "reason": reason });
    req.as_object_mut().unwrap().extend(target.as_object().unwrap().clone());
    place(State(env.st.clone()), Admin(admin), Json(serde_json::from_value(req).unwrap())).await.unwrap().0
}

async fn hold_node(env: &testutil::TestEnv, node: &str, reason: &str) -> Hold {
    place_on(env, "node", json!({ "node_id": node }), reason).await
}

async fn release_hold(env: &testutil::TestEnv, id: &str) {
    let Json(_) = release(State(env.st.clone()), Admin(env.admin().await), Path(id.to_string())).await.unwrap();
}

async fn trash_items(env: &testutil::TestEnv, user: &User, list: &[&str]) {
    let _ = crate::nodes::trash(State(env.st.clone()), user.clone(), ids(list)).await.unwrap();
}

async fn exists(env: &testutil::TestEnv, id: &str) -> bool {
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE id = ?").bind(id).fetch_one(&env.st.db).await.unwrap();
    n == 1
}

async fn delete_for_good(env: &testutil::TestEnv, user: &User, list: &[&str]) -> Job {
    let res = crate::nodes::delete_forever(State(env.st.clone()), user.clone(), ids(list)).await.unwrap();
    finished(env, res).await
}

fn kept_names(job: &Job) -> Vec<String> {
    let mut names: Vec<String> =
        job.result.as_ref().and_then(|r| r["kept"].as_array()).map(|a| a.iter().map(|k| k["name"].as_str().unwrap().to_string()).collect()).unwrap_or_default();
    names.sort();
    names
}

struct World {
    env: testutil::TestEnv,
    amy: User,
}

async fn world() -> World {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    World { env, amy }
}

async fn retention(env: &testutil::TestEnv, node: &str, days: i64) {
    let drive = env.drive_of(node).await;
    sqlx::query("UPDATE drives SET retention_days = ? WHERE id = ?").bind(days).bind(drive).execute(&env.st.db).await.unwrap();
}

async fn changed(env: &testutil::TestEnv, id: &str, days_ago: i64) {
    sqlx::query("UPDATE nodes SET updated_at = ? WHERE id = ?").bind(now() - days_ago * DAY).bind(id).execute(&env.st.db).await.unwrap();
}

// ───────────── Placing and releasing ─────────────

#[tokio::test]
async fn a_hold_is_placed_with_a_reason_listed_and_released() {
    let w = world().await;
    let file = w.env.file(&w.amy, w.amy.root(), "contract.txt").await;
    let hold = hold_node(&w.env, &file, "Case 12").await;
    assert_eq!((hold.scope.as_str(), hold.reason.as_str(), hold.created_by_name.as_str()), ("node", "Case 12", "admin"));
    assert!(hold.label.ends_with("/contract.txt"), "{}", hold.label);
    assert!(hold.present && hold.released_at.is_none());

    let listed = |state: &'static str, q: Option<&'static str>| {
        let (st, admin) = (w.env.st.clone(), w.env.admin());
        async move { list(State(st), Admin(admin.await), Query(ListQuery { state: Some(state.into()), q: q.map(str::to_string) })).await.unwrap().0 }
    };
    assert_eq!(listed("active", None).await["holds"].as_array().unwrap().len(), 1);
    assert_eq!(listed("active", Some("case")).await["holds"].as_array().unwrap().len(), 1);
    assert_eq!(listed("active", Some("nothing like it")).await["holds"].as_array().unwrap().len(), 0);
    assert_eq!(listed("released", None).await["holds"].as_array().unwrap().len(), 0);

    release_hold(&w.env, &hold.id).await;
    assert_eq!(listed("active", None).await["holds"].as_array().unwrap().len(), 0);
    let released = listed("released", None).await;
    assert_eq!(released["holds"][0]["released_by_name"], "admin");
    // It can't be released twice
    let err = release(State(w.env.st.clone()), Admin(w.env.admin().await), Path(hold.id)).await.unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_hold_needs_a_reason_and_something_that_exists() {
    let w = world().await;
    let file = w.env.file(&w.amy, w.amy.root(), "a.txt").await;
    let place_err = |req: Value| {
        let (st, admin) = (w.env.st.clone(), w.env.admin());
        async move { place(State(st), Admin(admin.await), Json(serde_json::from_value(req).unwrap())).await.err().map(|e| e.status) }
    };
    for reason in ["", "   ", &"x".repeat(501)] {
        assert_eq!(place_err(json!({ "scope": "node", "node_id": file, "reason": reason })).await, Some(axum::http::StatusCode::BAD_REQUEST), "{reason:?}");
    }
    assert_eq!(place_err(json!({ "scope": "node", "node_id": "no-such-node", "reason": "x" })).await, Some(axum::http::StatusCode::NOT_FOUND));
    assert_eq!(place_err(json!({ "scope": "space", "drive_id": "no-such-space", "reason": "x" })).await, Some(axum::http::StatusCode::NOT_FOUND));
    assert_eq!(place_err(json!({ "scope": "user", "user_id": 9999, "reason": "x" })).await, Some(axum::http::StatusCode::NOT_FOUND));
    assert_eq!(place_err(json!({ "scope": "node", "reason": "x" })).await, Some(axum::http::StatusCode::BAD_REQUEST));
    assert_eq!(place_err(json!({ "scope": "everything", "reason": "x" })).await, Some(axum::http::StatusCode::BAD_REQUEST));
    // The top folder of a space is held by holding the space
    assert_eq!(place_err(json!({ "scope": "node", "node_id": w.amy.root(), "reason": "x" })).await, Some(axum::http::StatusCode::BAD_REQUEST));
}

// ───────────── Deleting for good ─────────────

#[tokio::test]
async fn deleting_for_good_leaves_what_is_held_in_the_trash_and_says_so() {
    let w = world().await;
    let held = w.env.file(&w.amy, w.amy.root(), "held.txt").await;
    let free = w.env.file(&w.amy, w.amy.root(), "free.txt").await;
    let hold = hold_node(&w.env, &held, "Case 12").await;
    trash_items(&w.env, &w.amy, &[&held, &free]).await;

    let job = delete_for_good(&w.env, &w.amy, &[&held, &free]).await;
    assert!(!exists(&w.env, &free).await);
    assert!(exists(&w.env, &held).await, "the held file is still there");
    assert_eq!(kept_names(&job), vec!["held.txt"]);
    assert_eq!(job.result.as_ref().unwrap()["kept"][0]["hold"], "Case 12");
    // It is still in the trash, and says what keeps it
    let listed = crate::nodes::list_trash(State(w.env.st.clone()), w.amy.clone(), Query(TrashQuery::default())).await.unwrap().0.into_items();
    assert_eq!(listed.len(), 1);
    let shown = serde_json::to_value(&listed[0]).unwrap();
    assert_eq!(shown["kept"]["hold"], "Case 12");

    // Once released it goes
    release_hold(&w.env, &hold.id).await;
    let job = delete_for_good(&w.env, &w.amy, &[&held]).await;
    assert!(!exists(&w.env, &held).await);
    assert!(kept_names(&job).is_empty());
}

#[tokio::test]
async fn a_folder_with_a_held_file_inside_is_kept_whole_and_nothing_is_half_deleted() {
    let w = world().await;
    let folder = w.env.folder(&w.amy, w.amy.root(), "Cases").await;
    let held = w.env.file(&w.amy, &folder, "held.txt").await;
    let free = w.env.file(&w.amy, &folder, "free.txt").await;
    hold_node(&w.env, &held, "Case 12").await;
    trash_items(&w.env, &w.amy, &[&folder]).await;
    let job = delete_for_good(&w.env, &w.amy, &[&folder]).await;
    assert_eq!(kept_names(&job), vec!["Cases"]);
    for id in [&folder, &held, &free] {
        assert!(exists(&w.env, id).await, "{id}");
    }
}

#[tokio::test]
async fn an_item_under_a_held_folder_stays_even_when_it_is_trashed_on_its_own() {
    let w = world().await;
    let folder = w.env.folder(&w.amy, w.amy.root(), "Audit").await;
    let file = w.env.file(&w.amy, &folder, "ledger.txt").await;
    hold_node(&w.env, &folder, "Audit 2026").await;
    trash_items(&w.env, &w.amy, &[&file]).await;
    let job = delete_for_good(&w.env, &w.amy, &[&file]).await;
    assert_eq!(kept_names(&job), vec!["ledger.txt"]);
    assert!(exists(&w.env, &file).await);
}

#[tokio::test]
async fn a_hold_on_a_space_or_on_a_person_keeps_what_they_own() {
    let w = world().await;
    let bob = w.env.user("bob", true).await;
    let mine = w.env.file(&w.amy, w.amy.root(), "mine.txt").await;
    let his = w.env.file(&bob, bob.root(), "his.txt").await;
    let on_space = place_on(&w.env, "space", json!({ "drive_id": w.env.drive_of(w.amy.root()).await }), "Everything of Amy's").await;
    trash_items(&w.env, &w.amy, &[&mine]).await;
    trash_items(&w.env, &bob, &[&his]).await;
    assert_eq!(kept_names(&delete_for_good(&w.env, &w.amy, &[&mine]).await), vec!["mine.txt"]);
    // Bob's space is another
    assert!(kept_names(&delete_for_good(&w.env, &bob, &[&his]).await).is_empty());
    assert!(!exists(&w.env, &his).await);
    release_hold(&w.env, &on_space.id).await;

    let on_person = place_on(&w.env, "user", json!({ "user_id": w.amy.id }), "Amy's files").await;
    assert_eq!(kept_names(&delete_for_good(&w.env, &w.amy, &[&mine]).await), vec!["mine.txt"]);
    release_hold(&w.env, &on_person.id).await;
    assert!(kept_names(&delete_for_good(&w.env, &w.amy, &[&mine]).await).is_empty());
    assert!(!exists(&w.env, &mine).await);
}

#[tokio::test]
async fn emptying_the_trash_leaves_what_is_kept_and_deletes_the_rest() {
    let w = world().await;
    let held = w.env.file(&w.amy, w.amy.root(), "held.txt").await;
    let free = w.env.file(&w.amy, w.amy.root(), "free.txt").await;
    hold_node(&w.env, &held, "Case 12").await;
    trash_items(&w.env, &w.amy, &[&held, &free]).await;
    let res = crate::nodes::empty_trash(State(w.env.st.clone()), w.amy.clone()).await.unwrap();
    let job = finished(&w.env, res).await;
    assert_eq!(job.result.as_ref().unwrap()["deleted"], 1);
    assert_eq!(kept_names(&job), vec!["held.txt"]);
    assert!(exists(&w.env, &held).await && !exists(&w.env, &free).await);
}

#[tokio::test]
async fn the_automatic_emptying_leaves_what_is_kept_until_nothing_keeps_it() {
    let w = world().await;
    let held = w.env.file(&w.amy, w.amy.root(), "held.txt").await;
    let free = w.env.file(&w.amy, w.amy.root(), "free.txt").await;
    let hold = hold_node(&w.env, &held, "Case 12").await;
    trash_items(&w.env, &w.amy, &[&held, &free]).await;
    sqlx::query("UPDATE nodes SET trashed_at = ? WHERE trash_root = 1").bind(now() - 40 * DAY).execute(&w.env.st.db).await.unwrap();
    assert_eq!(crate::nodes::purge_expired_trash(&w.env.st, 30).await.unwrap(), 1);
    assert!(exists(&w.env, &held).await && !exists(&w.env, &free).await);
    // The next emptying finds it free
    release_hold(&w.env, &hold.id).await;
    assert_eq!(crate::nodes::purge_expired_trash(&w.env.st, 30).await.unwrap(), 1);
    assert!(!exists(&w.env, &held).await);
}

#[tokio::test]
async fn a_retention_period_keeps_a_file_in_the_trash_until_it_has_passed() {
    let w = world().await;
    let young = w.env.file(&w.amy, w.amy.root(), "young.txt").await;
    let old = w.env.file(&w.amy, w.amy.root(), "old.txt").await;
    retention(&w.env, &young, 30).await;
    changed(&w.env, &young, 10).await;
    changed(&w.env, &old, 40).await;
    trash_items(&w.env, &w.amy, &[&young, &old]).await;
    let job = delete_for_good(&w.env, &w.amy, &[&young, &old]).await;
    assert_eq!(kept_names(&job), vec!["young.txt"]);
    let until = job.result.as_ref().unwrap()["kept"][0]["until"].as_i64().unwrap();
    assert!((until - (now() + 20 * DAY)).abs() < 10);
    assert!(exists(&w.env, &young).await && !exists(&w.env, &old).await);
    // Later its period has passed
    changed(&w.env, &young, 31).await;
    assert!(kept_names(&delete_for_good(&w.env, &w.amy, &[&young]).await).is_empty());
    assert!(!exists(&w.env, &young).await);
}

// ───────────── Versions ─────────────

async fn new_file(env: &testutil::TestEnv, user: &User, name: &str) -> String {
    let b64 = |s: &str| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, s);
    let mut h = HeaderMap::new();
    h.insert("upload-length", "0".parse().unwrap());
    h.insert("upload-metadata", format!("filename {},parentId {}", b64(name), b64(user.root())).parse().unwrap());
    let res = crate::upload::create(State(env.st.clone()), user.clone(), h).await.unwrap();
    res.headers()["x-node-id"].to_str().unwrap().to_string()
}

async fn save(env: &testutil::TestEnv, user: &User, id: &str, body: &'static [u8]) {
    let _ = crate::files::save_content(State(env.st.clone()), user.clone(), Path(id.to_string()), HeaderMap::new(), Bytes::from_static(body)).await.unwrap();
}

async fn versions_of(env: &testutil::TestEnv, id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM node_versions WHERE node_id = ?").bind(id).fetch_one(&env.st.db).await.unwrap()
}

#[tokio::test]
async fn what_a_change_replaces_is_kept_under_a_hold_even_when_versions_are_off() {
    let w = world().await;
    w.env.st.system.write().version_keep = 0;
    let file = new_file(&w.env, &w.amy, "a.txt").await;
    save(&w.env, &w.amy, &file, b"one").await;
    save(&w.env, &w.amy, &file, b"two").await;
    assert_eq!(versions_of(&w.env, &file).await, 0, "versions are off");
    let hold = hold_node(&w.env, &file, "Case 12").await;
    save(&w.env, &w.amy, &file, b"three").await;
    save(&w.env, &w.amy, &file, b"four").await;
    assert_eq!(versions_of(&w.env, &file).await, 2, "both replaced contents stay");
    // Maintenance doesn't prune them, however few are kept
    w.env.st.system.write().version_days = 1;
    sqlx::query("UPDATE node_versions SET created_at = ? WHERE node_id = ?").bind(now() - 400 * DAY).bind(&file).execute(&w.env.st.db).await.unwrap();
    assert_eq!(crate::versions::prune(&w.env.st).await.unwrap(), 0);
    assert_eq!(versions_of(&w.env, &file).await, 2);
    // Released: they go at the next pruning
    release_hold(&w.env, &hold.id).await;
    assert_eq!(crate::versions::prune(&w.env.st).await.unwrap(), 2);
    assert_eq!(versions_of(&w.env, &file).await, 0);
}

#[tokio::test]
async fn versions_are_kept_for_the_retention_period_from_the_day_they_were_made() {
    let w = world().await;
    w.env.st.system.write().version_keep = 1;
    w.env.st.system.write().version_days = 1;
    let file = new_file(&w.env, &w.amy, "a.txt").await;
    retention(&w.env, &file, 30).await;
    for body in [&b"one"[..], b"two", b"three", b"four"] {
        let body: &'static [u8] = Box::leak(body.to_vec().into_boxed_slice());
        save(&w.env, &w.amy, &file, body).await;
    }
    // Only one is kept by the settings, but all are inside the period (the empty file the upload made is the first)
    assert_eq!(versions_of(&w.env, &file).await, 4);
    assert_eq!(crate::versions::prune(&w.env.st).await.unwrap(), 0);
    // Made long ago, they are past it and the settings apply again
    sqlx::query("UPDATE node_versions SET created_at = ? WHERE node_id = ?").bind(now() - 40 * DAY).bind(&file).execute(&w.env.st.db).await.unwrap();
    assert_eq!(crate::versions::prune(&w.env.st).await.unwrap(), 4);
    assert_eq!(versions_of(&w.env, &file).await, 0);
}

// ───────────── Deleting a space or an account, and moving out ─────────────

async fn team_space(env: &testutil::TestEnv, name: &str) -> (String, String) {
    let Json(info) = crate::drives::create(State(env.st.clone()), env.admin().await, Json(serde_json::from_value(json!({ "name": name })).unwrap())).await.unwrap();
    let v = serde_json::to_value(&info).unwrap();
    (v["id"].as_str().unwrap().to_string(), v["root_id"].as_str().unwrap().to_string())
}

#[tokio::test]
async fn a_space_with_something_kept_in_it_cannot_be_deleted() {
    let w = world().await;
    let admin = w.env.admin().await;
    let (space, root) = team_space(&w.env, "Legal").await;
    let file = w.env.file(&admin, &root, "case.txt").await;
    let hold = hold_node(&w.env, &file, "Case 12").await;
    let err = crate::drives::delete(State(w.env.st.clone()), admin.clone(), Path(space.clone())).await.unwrap_err();
    assert_eq!((err.status, err.code), (axum::http::StatusCode::CONFLICT, Some("kept")));
    release_hold(&w.env, &hold.id).await;
    // A retention period keeps it too, until it has passed
    retention(&w.env, &file, 30).await;
    assert!(crate::drives::delete(State(w.env.st.clone()), admin.clone(), Path(space.clone())).await.is_err());
    changed(&w.env, &file, 31).await;
    let _ = crate::drives::delete(State(w.env.st.clone()), admin, Path(space)).await.unwrap();
}

#[tokio::test]
async fn an_account_with_files_kept_cannot_be_deleted() {
    let w = world().await;
    let admin = w.env.admin().await;
    let file = w.env.file(&w.amy, w.amy.root(), "a.txt").await;
    let query = || Query(serde_json::from_value(json!({ "delete_files": true })).unwrap());
    let on_person = place_on(&w.env, "user", json!({ "user_id": w.amy.id }), "Amy's files").await;
    let res = crate::admin::delete(State(w.env.st.clone()), Admin(admin.clone()), Path(w.amy.id), query()).await;
    let failed = match res {
        Ok(job) => finished(&w.env, job).await.error.is_some(),
        Err(_) => true,
    };
    assert!(failed, "the account stays");
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users WHERE id = ?").bind(w.amy.id).fetch_one(&w.env.st.db).await.unwrap();
    assert_eq!(n, 1);
    assert!(exists(&w.env, &file).await);
    release_hold(&w.env, &on_person.id).await;

    // So does a hold on something in their personal space
    let hold = hold_node(&w.env, &file, "Case 12").await;
    let res = crate::admin::delete(State(w.env.st.clone()), Admin(admin.clone()), Path(w.amy.id), query()).await;
    let failed = match res {
        Ok(job) => finished(&w.env, job).await.error.is_some(),
        Err(_) => true,
    };
    assert!(failed);
    release_hold(&w.env, &hold.id).await;
    let job = finished(&w.env, crate::admin::delete(State(w.env.st.clone()), Admin(admin), Path(w.amy.id), query()).await.unwrap()).await;
    assert!(job.error.is_none(), "{:?}", job.error);
}

#[tokio::test]
async fn a_personal_space_with_something_kept_cannot_be_removed() {
    let w = world().await;
    let admin = w.env.admin().await;
    let file = w.env.file(&w.amy, w.amy.root(), "a.txt").await;
    let hold = hold_node(&w.env, &file, "Case 12").await;
    let query = || axum::extract::Query(serde_json::from_value(json!({ "delete_files": true })).unwrap());
    let res = crate::admin::personal::remove(State(w.env.st.clone()), Admin(admin.clone()), Path(w.amy.id), query()).await;
    let failed = match res {
        Ok(job) => finished(&w.env, job).await.error.is_some(),
        Err(_) => true,
    };
    assert!(failed);
    assert!(exists(&w.env, &file).await);
    release_hold(&w.env, &hold.id).await;
    let job = finished(&w.env, crate::admin::personal::remove(State(w.env.st.clone()), Admin(admin), Path(w.amy.id), query()).await.unwrap()).await;
    assert!(job.error.is_none(), "{:?}", job.error);
}

#[tokio::test]
async fn what_is_kept_cannot_be_moved_to_another_space() {
    // (the environment moves of files between spaces are tested in)
    let env = testutil::folders_env().await;
    let amy = env.user("amy", true).await;
    let w = World { env, amy };
    let admin = w.env.admin().await;
    let (_, company) = team_space(&w.env, "Legal").await;
    let held = w.env.upload(&admin, admin.root(), "held.txt", b"held").await;
    let free = w.env.upload(&admin, admin.root(), "free.txt", b"free").await;
    let hold = hold_node(&w.env, &held, "Case 12").await;
    let dest = crate::tree::get_node(&mut w.env.st.db.acquire().await.unwrap(), &company).await.unwrap().unwrap();
    let node = |id: String| {
        let st = w.env.st.clone();
        async move { crate::tree::get_node(&mut st.db.acquire().await.unwrap(), &id).await.unwrap().unwrap() }
    };
    let err = crate::fsops::move_across(&w.env.st, &admin, &dest, vec![vec![node(held.clone()).await]], &Default::default()).await.unwrap_err();
    assert_eq!((err.status, err.code), (axum::http::StatusCode::CONFLICT, Some("kept")));
    assert!(exists(&w.env, &held).await);
    assert_eq!(w.env.drive_of(&held).await, w.env.drive_of(admin.root()).await);
    // What nothing keeps moves, and so does the file once released
    crate::fsops::move_across(&w.env.st, &admin, &dest, vec![vec![node(free.clone()).await]], &Default::default()).await.unwrap();
    release_hold(&w.env, &hold.id).await;
    crate::fsops::move_across(&w.env.st, &admin, &dest, vec![vec![node(held.clone()).await]], &Default::default()).await.unwrap();
}

// ───────────── Retention of a space, and what Properties shows ─────────────

#[tokio::test]
async fn only_an_administrator_sets_a_retention_period() {
    let w = world().await;
    let (space, root) = team_space(&w.env, "Legal").await;
    let update = |user: User, days: i64| {
        let st = w.env.st.clone();
        let space = space.clone();
        async move { crate::drives::update(State(st), user, Path(space), Json(serde_json::from_value(json!({ "retention_days": days })).unwrap())).await }
    };
    // The space's owner may manage it, but not shorten what it keeps
    let admin = w.env.admin().await;
    let owner = w.env.user("owner", true).await;
    w.env.grant(&root, &owner, "owner").await;
    assert!(update(owner, 30).await.is_err());
    let Json(info) = update(admin.clone(), 30).await.unwrap();
    assert_eq!(serde_json::to_value(&info).unwrap()["retention_days"], 30);
    assert!(update(admin.clone(), -1).await.is_err());
    assert!(update(admin.clone(), 40_000).await.is_err());
    let Json(info) = update(admin, 0).await.unwrap();
    assert_eq!(serde_json::to_value(&info).unwrap()["retention_days"], 0);
}

#[tokio::test]
async fn managers_see_what_keeps_an_item_and_others_do_not() {
    let w = world().await;
    let bob = w.env.user("bob", true).await;
    let admin = w.env.admin().await;
    // A team space of the administrator's, where Amy manages a folder
    let (_, root) = team_space(&w.env, "Legal").await;
    let folder = w.env.folder(&admin, &root, "Cases").await;
    w.env.grant(&folder, &w.amy, "manager").await;
    let file = w.env.file(&admin, &folder, "a.txt").await;
    retention(&w.env, &file, 30).await;
    changed(&w.env, &file, 10).await;
    hold_node(&w.env, &folder, "Audit").await;
    let Json(v) = of_node(State(w.env.st.clone()), w.amy.clone(), Path(file.clone())).await.unwrap();
    assert_eq!(v["retention_days"], 30);
    assert!((v["kept_until"].as_i64().unwrap() - (now() + 20 * DAY)).abs() < 10);
    assert_eq!(v["holds"][0]["reason"], "Audit");
    assert_eq!(v["can_place"], false);
    // Bob can't even see the file
    assert!(of_node(State(w.env.st.clone()), bob, Path(file)).await.is_err());
    let Json(v) = of_node(State(w.env.st.clone()), admin, Path(folder)).await.unwrap();
    assert_eq!(v["can_place"], true);
}

#[tokio::test]
async fn the_files_a_hold_covers_are_listed_and_exported() {
    let w = world().await;
    let folder = w.env.folder(&w.amy, w.amy.root(), "Cases").await;
    let a = w.env.file(&w.amy, &folder, "a.txt").await;
    w.env.file(&w.amy, &folder, "=cmd.txt").await;
    w.env.file(&w.amy, w.amy.root(), "elsewhere.txt").await;
    let hold = hold_node(&w.env, &folder, "Audit").await;
    let Json(v) = items(State(w.env.st.clone()), Admin(w.env.admin().await), Path(hold.id.clone()), Query(ItemsQuery { limit: None })).await.unwrap();
    let names: Vec<&str> = v["items"].as_array().unwrap().iter().map(|i| i["name"].as_str().unwrap()).collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.contains(&"a.txt") && !names.contains(&"elsewhere.txt"));
    assert_eq!(v["more"], false);
    let csv = export(State(w.env.st.clone()), Admin(w.env.admin().await), Path(hold.id)).await.unwrap();
    assert_eq!(csv.headers()["content-type"], "text/csv; charset=utf-8");
    let body = String::from_utf8(axum::body::to_bytes(csv.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap();
    assert!(body.contains("Cases") && body.contains("\"a.txt\""), "{body}");
    // A name that a spreadsheet would run as a formula is written as text
    assert!(body.contains("\"'=cmd.txt\""), "{body}");
    let _ = a;
}

#[test]
fn a_time_is_written_as_a_date_and_clock() {
    assert_eq!(chrono_like(0), "1970-01-01 00:00");
    assert_eq!(chrono_like(1_800_000_000), "2027-01-15 08:00");
    assert_eq!(chrono_like(951_782_400 + 3600 * 5 + 60 * 7), "2000-02-29 05:07");
}

async fn move_to(env: &testutil::TestEnv, user: &User, id: &str, dest: &str) -> AppResult<()> {
    let req = Json(serde_json::from_value(json!({ "ids": [id], "dest_id": dest })).unwrap());
    let job = crate::nodes::move_nodes(State(env.st.clone()), user.clone(), req).await?;
    finished(env, job).await;
    Ok(())
}

#[tokio::test]
async fn an_item_under_a_hold_does_not_leave_the_held_folder_nor_its_space() {
    let w = world().await;
    let admin = w.env.admin().await;
    let held = w.env.folder(&admin, admin.root(), "Held").await;
    let inside = w.env.folder(&admin, &held, "Inside").await;
    let elsewhere = w.env.folder(&admin, admin.root(), "Elsewhere").await;
    let file = w.env.file(&admin, &held, "a.txt").await;
    let hold = hold_node(&w.env, &held, "Audit").await;

    // Within the folder it may go; out of it, it may not
    move_to(&w.env, &admin, &file, &inside).await.unwrap();
    let err = move_to(&w.env, &admin, &file, &elsewhere).await.unwrap_err();
    assert_eq!((err.status, err.code), (axum::http::StatusCode::CONFLICT, Some("kept")));
    // Nor can the held folder's contents be taken out one folder at a time
    let err = move_to(&w.env, &admin, &inside, &elsewhere).await.unwrap_err();
    assert_eq!(err.code, Some("kept"));
    // What is not in it moves in and out freely; the held folder itself moves as it is (the hold goes with it)
    let free = w.env.file(&admin, &elsewhere, "free.txt").await;
    move_to(&w.env, &admin, &free, admin.root()).await.unwrap();
    move_to(&w.env, &admin, &held, &elsewhere).await.unwrap();
    // To another space it doesn't go, whatever the folder: the hold on a space and its retention period stay behind
    let (_, company) = team_space(&w.env, "Legal").await;
    let err = move_to(&w.env, &admin, &held, &company).await.unwrap_err();
    assert_eq!(err.code, Some("kept"));
    release_hold(&w.env, &hold.id).await;
    move_to(&w.env, &admin, &file, &elsewhere).await.unwrap();
    move_to(&w.env, &admin, &held, &company).await.unwrap();
}

#[tokio::test]
async fn merging_a_trashed_folder_into_a_new_one_of_its_name_keeps_the_hold() {
    let w = world().await;
    let folder = w.env.folder(&w.amy, w.amy.root(), "Cases").await;
    let file = w.env.file(&w.amy, &folder, "a.txt").await;
    hold_node(&w.env, &folder, "Audit").await;
    trash_items(&w.env, &w.amy, &[&folder]).await;
    // A new folder takes the name, and the trashed one is restored into it
    let again = w.env.folder(&w.amy, w.amy.root(), "Cases").await;
    let res = crate::nodes::restore(State(w.env.st.clone()), w.amy.clone(), ids(&[&folder])).await.unwrap();
    finished(&w.env, res).await;
    let parent: Option<String> = sqlx::query_scalar("SELECT parent_id FROM nodes WHERE id = ?").bind(&file).fetch_one(&w.env.st.db).await.unwrap();
    assert_eq!(parent.as_deref(), Some(again.as_str()), "the file is in the folder that took the name");
    assert!(!exists(&w.env, &folder).await);
    // The folder that held it is gone, and the hold is on the one that took its place: the file can't be deleted for good
    trash_items(&w.env, &w.amy, &[&file]).await;
    let job = delete_for_good(&w.env, &w.amy, &[&file]).await;
    assert_eq!(kept_names(&job), vec!["a.txt"]);
    assert!(exists(&w.env, &file).await);
}
