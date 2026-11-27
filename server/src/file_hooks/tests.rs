//! File webhooks: what is queued for which changes, what is left out, sending and trying again, and the settings

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedReceiver;

use super::{delivery::deliver, *};
use crate::{
    nodes,
    testutil::{self, TestEnv},
};

/// The shared secret the tests use: made when it is first needed, not written in the code
static SHARED: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| "s".repeat(24));

fn secret_text() -> &'static str {
    &SHARED
}

fn req<T: serde::de::DeserializeOwned>(v: Value) -> Json<T> {
    Json(serde_json::from_value(v).unwrap())
}

fn change(url: &str, events: &[&str], drives: &[&str]) -> Value {
    json!({ "url": url, "secret": secret_text(), "events": events, "drives": drives, "allow_private": true, "enabled": true })
}

/// Adds a webhook for `events` (all of them when empty) and returns its id
async fn hook(env: &TestEnv, url: &str, events: &[&str], drives: &[&str]) -> String {
    let events = if events.is_empty() { EVENTS.as_slice() } else { events };
    let Json(info) = create(State(env.st.clone()), crate::auth::Admin(env.admin().await), req(change(url, events, drives))).await.unwrap();
    info.webhooks.last().unwrap().id.clone()
}

/// The messages queued, oldest first
async fn queued(env: &TestEnv) -> Vec<Value> {
    sqlx::query_as::<_, (String,)>("SELECT body FROM file_webhook_deliveries ORDER BY id")
        .fetch_all(&env.st.db)
        .await
        .unwrap()
        .into_iter()
        .map(|(b,)| serde_json::from_str(&b).unwrap())
        .collect()
}

/// Removes what is queued, and returns each message's event and item name
async fn drain(env: &TestEnv) -> Vec<(String, String)> {
    let found = queued(env).await.iter().map(|m| (m["event"].as_str().unwrap().to_string(), m["item"]["name"].as_str().unwrap().to_string())).collect();
    sqlx::query("DELETE FROM file_webhook_deliveries").execute(&env.st.db).await.unwrap();
    found
}

fn told(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(e, n)| (e.to_string(), n.to_string())).collect()
}

async fn rename(env: &TestEnv, user: &User, id: &str, name: &str) -> AppResult<()> {
    nodes::rename(State(env.st.clone()), user.clone(), Path(id.into()), req(json!({ "name": name }))).await.map(|_| ())
}

async fn transfer(env: &TestEnv, user: &User, copy: bool, id: &str, dest: &str) {
    let Json(req) = req(json!({ "ids": [id], "dest_id": dest }));
    let across = if copy { nodes::copy_items(&env.st, user, &req).await } else { nodes::move_items(&env.st, user, &req).await }.unwrap();
    if let Some(across) = across {
        nodes::run_content(&env.st, user, across, &Default::default()).await.unwrap();
    }
}

async fn trash(env: &TestEnv, user: &User, id: &str) {
    let _ = nodes::trash(State(env.st.clone()), user.clone(), req(json!({ "ids": [id] }))).await.unwrap();
}

#[tokio::test]
async fn changes_to_files_are_queued_with_what_they_changed() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let all = env.st.shared_root().unwrap();
    hook(&env, "http://127.0.0.1:9/hook", &[], &[]).await;
    // Changing the settings is no change to files
    assert!(queued(&env).await.is_empty());

    let Json(docs) = nodes::create_folder(State(env.st.clone()), admin.clone(), req(json!({ "parent_id": all, "name": "Docs" }))).await.unwrap();
    let other = env.folder(&admin, &all, "Other").await;
    let id = env.upload(&admin, &docs.id, "a.txt", b"alpha").await;
    rename(&env, &admin, &id, "b.txt").await.unwrap();
    let messages = queued(&env).await;
    let renamed = messages.last().unwrap();
    assert_eq!(renamed["event"], "rename");
    assert_eq!(renamed["previous_name"], "a.txt");
    assert_eq!(renamed["item"], json!({ "id": id, "name": "b.txt", "kind": "file", "parent_id": docs.id }));
    assert_eq!(renamed["user"], json!({ "id": admin.id, "name": admin.username }));
    assert_eq!(renamed["space"]["id"].as_str().unwrap(), env.drive_of(&all).await);
    assert!(renamed["id"].as_str().is_some_and(|i| i.len() == 32) && renamed["at"].as_i64().unwrap() > 0);
    assert_eq!(drain(&env).await, told(&[("create_folder", "Docs"), ("upload", "a.txt"), ("rename", "b.txt")]));

    transfer(&env, &admin, false, &id, &other).await;
    let moved = queued(&env).await.remove(0);
    assert_eq!((moved["to"]["folder_id"].as_str(), moved["to"]["name"].as_str()), (Some(other.as_str()), Some("Other")));
    assert_eq!(moved["item"]["parent_id"].as_str(), Some(other.as_str()));
    transfer(&env, &admin, true, &id, &docs.id).await;
    trash(&env, &admin, &id).await;
    let _ = nodes::restore(State(env.st.clone()), admin.clone(), req(json!({ "ids": [id] }))).await.unwrap();
    assert_eq!(drain(&env).await, told(&[("move", "b.txt"), ("copy", "b.txt"), ("trash", "b.txt"), ("restore", "b.txt")]));

    // Share links: the item, never the link's address or password
    let Json(share) = crate::shares::create(State(env.st.clone()), admin.clone(), req(json!({ "node_id": id, "password": testutil::password() }))).await.unwrap();
    let share = serde_json::to_value(&share).unwrap()["id"].as_str().unwrap().to_string();
    let _ = crate::shares::update(State(env.st.clone()), admin.clone(), Path(share.clone()), req(json!({ "allow_download": false }))).await.unwrap();
    let _ = crate::shares::delete(State(env.st.clone()), admin.clone(), Path(share.clone())).await.unwrap();
    for message in queued(&env).await {
        let text = message.to_string();
        assert!(!text.contains(&share) && !text.contains(testutil::password()), "{text}");
    }
    assert_eq!(drain(&env).await, told(&[("share_create", "b.txt"), ("share_update", "b.txt"), ("share_delete", "b.txt")]));

    // Deleted for good: one by one, by Empty trash, and by the retention time
    trash(&env, &admin, &id).await;
    let _ = nodes::delete_forever(State(env.st.clone()), admin.clone(), req(json!({ "ids": [id] }))).await.unwrap();
    assert_eq!(drain(&env).await, told(&[("trash", "b.txt"), ("delete", "b.txt")]));
    trash(&env, &admin, &other).await;
    let _ = nodes::empty_trash(State(env.st.clone()), admin.clone()).await.unwrap();
    assert_eq!(drain(&env).await, told(&[("trash", "Other"), ("delete", "Other")]));
    trash(&env, &admin, &docs.id).await;
    drain(&env).await;
    sqlx::query("UPDATE nodes SET trashed_at = 1 WHERE trashed_at IS NOT NULL").execute(&env.st.db).await.unwrap();
    nodes::purge_expired_trash(&env.st, 30).await.unwrap();
    let purged = queued(&env).await;
    assert_eq!(purged[0]["user"], Value::Null);
    assert_eq!(drain(&env).await, told(&[("delete", "Docs")]));
}

#[tokio::test]
async fn a_change_that_fails_queues_nothing() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let all = env.st.shared_root().unwrap();
    let a = env.folder(&admin, &all, "a").await;
    env.folder(&admin, &all, "b").await;
    hook(&env, "http://127.0.0.1:9/hook", &[], &[]).await;
    assert!(rename(&env, &admin, &a, "b").await.is_err());
    // All or none: the first of a move's two items would have been queued
    let dest = env.folder(&admin, &all, "dest").await;
    env.folder(&admin, &dest, "b").await;
    let c = env.folder(&admin, &all, "c").await;
    let b: (String,) = sqlx::query_as("SELECT id FROM nodes WHERE parent_id = ? AND name = 'b'").bind(&all).fetch_one(&env.st.db).await.unwrap();
    let Json(both) = req(json!({ "ids": [c, b.0], "dest_id": dest }));
    assert!(nodes::move_items(&env.st, &admin, &both).await.is_err());
    assert!(queued(&env).await.is_empty());
}

#[tokio::test]
async fn personal_spaces_and_restricted_folders_are_left_out() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let amy = env.user("amy", true).await;
    let all = env.st.shared_root().unwrap();
    let open = env.folder(&admin, &all, "Open").await;
    let closed = env.folder(&admin, &all, "Closed").await;
    sqlx::query("UPDATE nodes SET restricted = 1 WHERE id = ?").bind(&closed).execute(&env.st.db).await.unwrap();
    hook(&env, "http://127.0.0.1:9/hook", &[], &[]).await;

    // In a personal space
    let own = env.upload(&amy, amy.root(), "diary.txt", b"private").await;
    rename(&env, &amy, &own, "journal.txt").await.unwrap();
    // In a restricted folder, and the folder itself
    let hidden = env.upload(&admin, &closed, "salary.txt", b"hidden").await;
    rename(&env, &admin, &hidden, "pay.txt").await.unwrap();
    rename(&env, &admin, &closed, "Locked").await.unwrap();
    assert_eq!(drain(&env).await, told(&[]));

    // Into or out of either, whichever end it is
    let shown = env.upload(&admin, &open, "memo.txt", b"memo").await;
    assert_eq!(drain(&env).await, told(&[("upload", "memo.txt")]));
    transfer(&env, &admin, false, &shown, &closed).await;
    transfer(&env, &admin, false, &shown, &open).await;
    transfer(&env, &admin, false, &shown, admin.root()).await;
    transfer(&env, &admin, false, &shown, &open).await;
    transfer(&env, &admin, true, &shown, &closed).await;
    transfer(&env, &admin, true, &shown, admin.root()).await;
    transfer(&env, &admin, true, &hidden, &open).await;
    assert_eq!(drain(&env).await, told(&[]));
}

#[tokio::test]
async fn copies_and_moves_between_spaces_into_restricted_folders_are_left_out_and_logged_as_such() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let all = env.st.shared_root().unwrap();
    let space = env.folder_space("Projects").await;
    let closed = env.folder(&admin, &all, "Closed").await;
    sqlx::query("UPDATE nodes SET restricted = 1 WHERE id = ?").bind(&closed).execute(&env.st.db).await.unwrap();
    testutil::write_old(&space.dir.join("plan.txt"), b"plan");
    crate::folders::scan(&env.st, &space.drive).await.unwrap();
    let (plan, _) = env.node_at(&space.drive, "plan.txt").await.unwrap();
    hook(&env, "http://127.0.0.1:9/hook", &[], &[]).await;
    transfer(&env, &admin, true, &plan, &closed).await;
    assert_eq!(drain(&env).await, told(&[]));
    // The activity log keeps the entry, which names the destination, from whoever can't see it
    let (scopes,): (String,) =
        sqlx::query_as("SELECT restricted_scopes FROM activity WHERE action = 'copy' ORDER BY id DESC LIMIT 1").fetch_one(&env.st.db).await.unwrap();
    assert!(scopes.contains(&closed), "{scopes}");

    // A folder space is told of like any space, and only to the webhooks that chose it or none
    sqlx::query("DELETE FROM file_webhooks").execute(&env.st.db).await.unwrap();
    hook(&env, "http://127.0.0.1:9/one", &["rename"], &[&space.drive]).await;
    let a = env.folder(&admin, &all, "a").await;
    rename(&env, &admin, &a, "b").await.unwrap();
    rename(&env, &admin, &plan, "plans.txt").await.unwrap();
    assert_eq!(drain(&env).await, told(&[("rename", "plans.txt")]));
    // Events not chosen, and a paused webhook
    trash(&env, &admin, &plan).await;
    sqlx::query("UPDATE file_webhooks SET enabled = 0").execute(&env.st.db).await.unwrap();
    let _ = nodes::restore(State(env.st.clone()), admin.clone(), req(json!({ "ids": [plan] }))).await.unwrap();
    rename(&env, &admin, &plan, "plan.txt").await.unwrap();
    assert_eq!(drain(&env).await, told(&[]));
}

/// A webhook receiver on this computer: answers `status` and hands over each request (its headers and body)
async fn receiver(status: std::sync::Arc<std::sync::atomic::AtomicU16>) -> (String, UnboundedReceiver<(HeaderMap, String)>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let app = Router::new().route(
        "/hook",
        post(move |headers: HeaderMap, body: String| {
            let (tx, status) = (tx.clone(), status.clone());
            async move {
                let _ = tx.send((headers, body));
                StatusCode::from_u16(status.load(std::sync::atomic::Ordering::SeqCst)).unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, rx)
}

/// (attempts, next attempt, sent, given up on) of each message
async fn states(env: &TestEnv) -> Vec<(i64, i64, bool, bool)> {
    sqlx::query_as("SELECT attempts, next_attempt_at, done_at IS NOT NULL, failed FROM file_webhook_deliveries ORDER BY id").fetch_all(&env.st.db).await.unwrap()
}

#[tokio::test]
async fn messages_are_signed_sent_in_order_and_tried_again() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let all = env.st.shared_root().unwrap();
    let status = std::sync::Arc::new(std::sync::atomic::AtomicU16::new(500));
    let (url, mut received) = receiver(status.clone()).await;
    hook(&env, &url, &["create_folder"], &[]).await;
    for name in ["one", "two"] {
        let _ = nodes::create_folder(State(env.st.clone()), admin.clone(), req(json!({ "parent_id": all, "name": name }))).await.unwrap();
    }
    let t = crate::util::now();

    // The first fails: the second isn't tried, and both wait
    deliver(&env.st, t).await.unwrap();
    let (headers, body) = received.try_recv().unwrap();
    assert!(received.try_recv().is_err());
    let at: i64 = headers["x-thirtyfile-timestamp"].to_str().unwrap().parse().unwrap();
    assert_eq!(headers["x-thirtyfile-signature"].to_str().unwrap(), crate::alerts::webhook::signature(secret_text(), at, &body));
    let sent: Value = serde_json::from_str(&body).unwrap();
    assert_eq!((sent["item"]["name"].as_str(), sent["site"].is_string()), (Some("one"), true));
    assert_eq!(states(&env).await, [(1, t + 60, false, false), (0, t + 60, false, false)]);
    deliver(&env.st, t + 59).await.unwrap();
    assert!(received.try_recv().is_err());

    // Once it answers, both arrive in order, the first with the same id as before
    status.store(204, std::sync::atomic::Ordering::SeqCst);
    deliver(&env.st, t + 60).await.unwrap();
    let again: Value = serde_json::from_str(&received.try_recv().unwrap().1).unwrap();
    assert_eq!(again["id"], sent["id"]);
    assert_eq!(serde_json::from_str::<Value>(&received.try_recv().unwrap().1).unwrap()["item"]["name"], "two");
    assert!(states(&env).await.iter().all(|s| s.2));

    // Given up on after the last wait
    status.store(500, std::sync::atomic::Ordering::SeqCst);
    sqlx::query("DELETE FROM file_webhook_deliveries").execute(&env.st.db).await.unwrap();
    let _ = nodes::create_folder(State(env.st.clone()), admin.clone(), req(json!({ "parent_id": all, "name": "three" }))).await.unwrap();
    let mut t = crate::util::now();
    for wait in [60, 300, 1800, 7200, 21600, 0] {
        deliver(&env.st, t).await.unwrap();
        t += wait;
    }
    assert!(matches!(states(&env).await[..], [(6, _, false, true)]));
    let (_, error): (i64, String) = sqlx::query_as("SELECT id, error FROM file_webhook_deliveries").fetch_one(&env.st.db).await.unwrap();
    assert!(error.contains("500"), "{error}");

    // Ones that waited a day are given up on, and old ones removed
    let _ = nodes::create_folder(State(env.st.clone()), admin.clone(), req(json!({ "parent_id": all, "name": "four" }))).await.unwrap();
    let t = crate::util::now();
    sqlx::query("UPDATE file_webhook_deliveries SET next_attempt_at = ?").bind(t + 10 * 86400).execute(&env.st.db).await.unwrap();
    deliver(&env.st, t + 86401).await.unwrap();
    assert!(states(&env).await.iter().all(|s| s.3));
    deliver(&env.st, t + 31 * 86400).await.unwrap();
    assert!(states(&env).await.is_empty());
}

#[tokio::test]
async fn webhooks_are_checked_tested_and_removed_with_their_messages() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let amy = env.user("amy", true).await;
    let personal = env.drive_of(amy.root()).await;
    let add = async |v: Value| create(State(env.st.clone()), crate::auth::Admin(admin.clone()), req(v)).await.map(|Json(i)| i);
    let good = change("https://hooks.example.com/files?key=1", &["upload", "upload"], &[]);
    for (field, value) in [
        ("url", json!("ftp://example.com")),
        ("url", json!("")),
        ("secret", json!("short")),
        ("secret", Value::Null),
        ("events", json!([])),
        ("events", json!(["scan"])),
        ("drives", json!([personal])),
        ("drives", json!(["no-such-space"])),
    ] {
        let mut bad = good.clone();
        bad[field] = value.clone();
        assert_eq!(add(bad).await.err().map(|e| e.status), Some(StatusCode::BAD_REQUEST), "{field} {value}");
    }
    let info = add(good.clone()).await.unwrap();
    let hook = &info.webhooks[0];
    assert_eq!(hook.events, ["upload"]);
    // The secret is kept sealed, and never sent back; the log leaves out the address's query
    let shown = serde_json::to_string(&info).unwrap();
    let (stored,): (String,) = sqlx::query_as("SELECT secret FROM file_webhooks").fetch_one(&env.st.db).await.unwrap();
    assert!(!shown.contains(secret_text()) && !stored.contains(secret_text()));
    let (logged,): (String,) = sqlx::query_as("SELECT detail FROM activity WHERE action = 'settings' ORDER BY id DESC LIMIT 1").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(logged, "Added a file webhook: https://hooks.example.com/files");

    // Changed without a new secret: the one saved stays
    let mut changed = good.clone();
    changed["secret"] = Value::Null;
    changed["events"] = json!(["delete", "upload"]);
    let id = hook.id.clone();
    let Json(info) = update(State(env.st.clone()), crate::auth::Admin(admin.clone()), Path(id.clone()), req(changed)).await.unwrap();
    assert_eq!(info.webhooks[0].events, ["upload", "delete"]);
    assert_eq!(sqlx::query_as::<_, (String,)>("SELECT secret FROM file_webhooks").fetch_one(&env.st.db).await.unwrap().0, stored);
    // …and is sealed again when the key is rotated
    assert!(crate::settings::reseal_secrets(&env.st.db, &rand::random()).await.unwrap() >= 1);
    assert_ne!(sqlx::query_as::<_, (String,)>("SELECT secret FROM file_webhooks").fetch_one(&env.st.db).await.unwrap().0, stored);
    sqlx::query("UPDATE file_webhooks SET secret = ?").bind(&stored).execute(&env.st.db).await.unwrap();

    // The test message goes where the webhook does
    let (url, mut received) = receiver(std::sync::Arc::new(std::sync::atomic::AtomicU16::new(200))).await;
    let mut local = good.clone();
    local["url"] = url.into();
    let _ = update(State(env.st.clone()), crate::auth::Admin(admin.clone()), Path(id.clone()), req(local.clone())).await.unwrap();
    let _ = test(State(env.st.clone()), crate::auth::Admin(admin.clone()), Path(id.clone())).await.unwrap();
    assert_eq!(serde_json::from_str::<Value>(&received.try_recv().unwrap().1).unwrap()["event"], "test");
    // Not to the private network unless allowed
    local["allow_private"] = false.into();
    let _ = update(State(env.st.clone()), crate::auth::Admin(admin.clone()), Path(id.clone()), req(local)).await.unwrap();
    let refused = test(State(env.st.clone()), crate::auth::Admin(admin.clone()), Path(id.clone())).await.err().unwrap();
    assert!(refused.message.contains("private network"), "{}", refused.message);
    assert!(received.try_recv().is_err());

    env.upload(&admin, &env.st.shared_root().unwrap(), "a.txt", b"a").await;
    assert_eq!(queued(&env).await.len(), 1);
    let Json(info) = remove(State(env.st.clone()), crate::auth::Admin(admin.clone()), Path(id.clone())).await.unwrap();
    assert!(info.webhooks.is_empty() && queued(&env).await.is_empty());
    assert_eq!(test(State(env.st.clone()), crate::auth::Admin(admin.clone()), Path(id)).await.err().unwrap().status, StatusCode::NOT_FOUND);
}
