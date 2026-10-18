use super::*;
use crate::{
    drives,
    testutil::{self, TestEnv},
};
use axum::{
    extract::Query,
    http::{HeaderMap, StatusCode},
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn req<T: DeserializeOwned>(value: Value) -> Json<T> {
    Json(serde_json::from_value(value).unwrap())
}

async fn guest(env: &TestEnv, name: &str) -> User {
    let hash = auth::hash_password(testutil::password().into()).await.unwrap();
    let _w = env.st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&env.st.db).await.unwrap();
    let id = crate::users::create_user(
        &mut tx,
        crate::users::NewUser {
            username: name,
            password_hash: &hash,
            role: auth::UserRole::User,
            guest: true,
            can_write: true,
            can_delete: true,
            can_share: true,
            quota_bytes: 0,
            source: "password",
            provisioned_by: None,
            personal_space: Some("local"),
            space_folders: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    auth::user_by_id(&env.st, &mut env.st.db.acquire().await.unwrap(), id).await.unwrap().unwrap()
}

async fn grant(env: &TestEnv, actor: &User, node: &str, kind: &str, id: i64, role: &str, expiry: Option<i64>) -> AppResult<Json<Value>> {
    drives::grant(State(env.st.clone()), actor.clone(), Path(node.into()), req(json!({"principal_type":kind,"principal_id":id,"role":role,"expires_at":expiry})))
        .await
}

async fn group(env: &TestEnv, admin: &User, name: &str, members: &[i64]) -> i64 {
    drives::create_group(State(env.st.clone()), Admin(admin.clone()), req(json!({"name":name,"members":members}))).await.unwrap().0["id"].as_i64().unwrap()
}

async fn convert_to_guest(env: &TestEnv, admin: &User, target: &User) -> AppResult<Json<crate::admin::UserRow>> {
    let p = preview_in(&mut *env.st.db.acquire().await?, target.id).await?;
    crate::admin::update(
        State(env.st.clone()),
        Admin(admin.clone()),
        Path(target.id),
        req(json!({"guest":true,"role":"user","confirm_guest":true,"guest_revision":p.revision,"my_password":testutil::password()})),
    )
    .await
}

#[tokio::test]
async fn guests_have_no_personal_or_everyone_access_and_the_directory_stays_in_their_shared_tree() {
    let env = testutil::env().await;
    let alice = env.user("alice", true).await;
    let unrelated = env.user("unrelated", true).await;
    let outside = guest(&env, "outside").await;
    assert!(outside.guest && outside.root_id.is_none() && !outside.can_share);
    let (personal,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM drives WHERE kind='personal' AND owner_id=?").bind(outside.id).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(personal, 0, "even the low-level creator must not give guests a personal space");
    let company = env.st.shared_root().unwrap();
    let mut conn = env.st.db.acquire().await.unwrap();
    assert!(crate::tree::node_for(&mut conn, &outside, &company, crate::tree::Need::Read).await.is_err());
    assert!(crate::tree::user_drives(&mut conn, &outside).await.unwrap().is_empty());
    drop(conn);
    let folder = env.folder(&alice, alice.root(), "Client files").await;
    let _ = grant(&env, &alice, &folder, "user", outside.id, "editor", Some(now() + 7 * 86400)).await.unwrap();
    let Json(found) = drives::directory(State(env.st.clone()), outside.clone(), Query(serde_json::from_value(json!({"q":""})).unwrap())).await.unwrap();
    let found = serde_json::to_value(found).unwrap();
    let names: Vec<_> = found.as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"alice") && names.contains(&"outside"));
    assert!(!names.contains(&unrelated.username.as_str()));
    assert!(crate::tree::node_for(&mut env.st.db.acquire().await.unwrap(), &outside, &folder, crate::tree::Need::Add).await.is_ok());
    let Json(me) = crate::signin::me(State(env.st.clone()), outside.clone(), HeaderMap::new()).await.unwrap();
    assert!(!me.can_create_drive && !me.personal_pending && me.used_bytes == 0);
    assert!(drives::create(State(env.st.clone()), outside.clone(), req(json!({"name":"Forbidden"}))).await.is_err());
    assert!(crate::shares::create(State(env.st.clone()), outside.clone(), req(json!({"node_id":folder,"expires_at":null}))).await.is_err());
    assert!(crate::s3::credentials::publisher(&mut env.st.db.acquire().await.unwrap(), &outside, &folder, true).await.is_err());
}

#[tokio::test]
async fn guest_end_dates_and_roles_apply_to_admins_and_group_membership_cannot_bypass_them() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let outside = guest(&env, "outside").await;
    let member = env.user("member", true).await;
    let folder = env.folder(&admin, admin.root(), "Shared").await;
    for (role, expiry) in [("manager", Some(now() + 86400)), ("owner", Some(now() + 86400)), ("editor", None), ("viewer", Some(now() + 91 * 86400))] {
        assert!(grant(&env, &admin, &folder, "user", outside.id, role, expiry).await.is_err());
    }
    // The longest access picked on a computer whose clock is a little ahead: taken, and stored as the longest access
    let _ = grant(&env, &admin, &folder, "user", outside.id, "editor", Some(now() + 90 * 86400 + 600)).await.unwrap();
    let (end,): (Option<i64>,) = sqlx::query_as("SELECT expires_at FROM grants WHERE node_id=? AND principal_type='user' AND principal_id=?")
        .bind(&folder)
        .bind(outside.id)
        .fetch_one(&env.st.db)
        .await
        .unwrap();
    assert!(end.is_some_and(|t| t <= now() + 90 * 86400 && t > now() + 90 * 86400 - 60), "{end:?}");
    // More than that is still refused
    assert!(grant(&env, &admin, &folder, "user", outside.id, "editor", Some(now() + 90 * 86400 + 2 * 3600)).await.is_err());
    let _ = grant(&env, &admin, &folder, "user", outside.id, "editor", Some(now() + 86400)).await.unwrap();
    let g = group(&env, &admin, "Guests", &[member.id, outside.id]).await;
    assert!(grant(&env, &admin, &folder, "group", g, "manager", Some(now() + 86400)).await.is_err());
    assert!(grant(&env, &admin, &folder, "group", g, "editor", None).await.is_err());
    let _ = grant(&env, &admin, &folder, "group", g, "viewer", Some(now() + 86400)).await.unwrap();
    let restricted = group(&env, &admin, "Managers", &[member.id]).await;
    let _ = grant(&env, &admin, &folder, "group", restricted, "manager", None).await.unwrap();
    let error = drives::update_group(State(env.st.clone()), Admin(admin.clone()), Path(restricted), req(json!({"members":[member.id,outside.id]})))
        .await
        .map(|_| ())
        .unwrap_err();
    assert!(error.message.contains("Managers") && error.message.contains("Shared"));
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM group_members WHERE group_id=? AND user_id=?").bind(restricted).bind(outside.id).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(count, 0);
    sqlx::query("UPDATE grants SET expires_at=? WHERE principal_type='user' AND principal_id=?").bind(now() - 1).bind(outside.id).execute(&env.st.db).await.unwrap();
    sqlx::query("UPDATE grants SET expires_at=? WHERE principal_type='group' AND principal_id=?").bind(now() - 1).bind(g).execute(&env.st.db).await.unwrap();
    assert!(crate::tree::node_for(&mut env.st.db.acquire().await.unwrap(), &outside, &folder, crate::tree::Need::Read).await.is_err());
}

#[tokio::test]
async fn group_only_sharing_uses_current_membership_without_revoking_earlier_unrelated_grants() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let alice = env.user("alice", true).await;
    let bob = env.user("bob", true).await;
    let other = env.user("other", true).await;
    let folder = env.folder(&admin, admin.root(), "Managed").await;
    let _ = grant(&env, &admin, &folder, "user", alice.id, "manager", None).await.unwrap();
    let _ = grant(&env, &alice, &folder, "user", other.id, "viewer", None).await.unwrap();
    let mine = group(&env, &admin, "Mine", &[alice.id, bob.id]).await;
    let theirs = group(&env, &admin, "Theirs", &[other.id]).await;
    crate::db::set_setting(&mut env.st.db.acquire().await.unwrap(), "share_own_groups", "1").await.unwrap();
    let _ = grant(&env, &alice, &folder, "user", bob.id, "viewer", None).await.unwrap();
    for (kind, id) in [("user", other.id), ("group", theirs), ("everyone", 0)] {
        assert_eq!(grant(&env, &alice, &folder, kind, id, "viewer", None).await.map(|_| ()).unwrap_err().status, StatusCode::FORBIDDEN);
    }
    let Json(found) = drives::directory(State(env.st.clone()), alice.clone(), Query(serde_json::from_value(json!({"q":""})).unwrap())).await.unwrap();
    let names = serde_json::to_value(found).unwrap();
    assert!(names.as_array().unwrap().iter().any(|p| p["name"] == "Mine"));
    assert!(!names.as_array().unwrap().iter().any(|p| p["name"] == "Theirs" || p["name"] == "other"));
    let _ = drives::update_group(State(env.st.clone()), Admin(admin.clone()), Path(mine), req(json!({"members":[alice.id]}))).await.unwrap();
    assert!(grant(&env, &alice, &folder, "user", bob.id, "editor", None).await.is_err());
    for user in [&bob, &other] {
        assert!(crate::tree::node_for(&mut env.st.db.acquire().await.unwrap(), user, &folder, crate::tree::Need::Read).await.is_ok());
    }
    let _ = grant(&env, &admin, &folder, "user", other.id, "editor", None).await.unwrap();
}

#[tokio::test]
async fn conversion_requires_a_current_preview_preserves_files_and_caps_direct_access() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let alice = env.user("alice", true).await;
    let private = env.stored_file(&alice, alice.root(), "kept.txt", b"kept content").await;
    let shared = env.folder(&admin, admin.root(), "Client").await;
    let _ = grant(&env, &admin, &shared, "user", alice.id, "manager", None).await.unwrap();
    let p = preview_in(&mut env.st.db.acquire().await.unwrap(), alice.id).await.unwrap();
    assert!(p.personal_space && p.personal_files == 1 && p.changed_grants.len() == 1);
    let no_confirm = crate::admin::update(
        State(env.st.clone()),
        Admin(admin.clone()),
        Path(alice.id),
        req(json!({"guest":true,"role":"user","my_password":testutil::password()})),
    )
    .await
    .map(|_| ())
    .unwrap_err();
    assert!(no_confirm.message.contains("confirm"));
    let _ = grant(&env, &admin, &shared, "user", alice.id, "editor", Some(now() + 86400)).await.unwrap();
    let stale = crate::admin::update(
        State(env.st.clone()),
        Admin(admin.clone()),
        Path(alice.id),
        req(json!({"guest":true,"role":"user","confirm_guest":true,"guest_revision":p.revision,"my_password":testutil::password()})),
    )
    .await
    .map(|_| ())
    .unwrap_err();
    assert_eq!(stale.status, StatusCode::CONFLICT);
    let _ = grant(&env, &admin, &shared, "user", alice.id, "manager", None).await.unwrap();
    let _ = convert_to_guest(&env, &admin, &alice).await.unwrap();
    let current = auth::user_by_id(&env.st, &mut env.st.db.acquire().await.unwrap(), alice.id).await.unwrap().unwrap();
    assert!(current.guest && current.root_id.is_none());
    assert!(crate::access::read_at(&env.st, &alice, &private, 0, 64).await.is_err(), "a stale standard actor cannot read the preserved personal tree");
    let (role, end): (Role, Option<i64>) = sqlx::query_as("SELECT role,expires_at FROM grants WHERE node_id=? AND principal_type='user' AND principal_id=?")
        .bind(&shared)
        .bind(alice.id)
        .fetch_one(&env.st.db)
        .await
        .unwrap();
    assert_eq!(role, Role::Editor);
    assert!(end.is_some_and(|t| t > now() && t <= now() + 90 * 86400));
    let _ = crate::admin::update(State(env.st.clone()), Admin(admin), Path(alice.id), req(json!({"guest":false,"my_password":testutil::password()}))).await.unwrap();
    let restored = auth::user_by_id(&env.st, &mut env.st.db.acquire().await.unwrap(), alice.id).await.unwrap().unwrap();
    assert_eq!(restored.root_id, alice.root_id);
    assert_eq!(crate::access::read_at(&env.st, &restored, &private, 0, 64).await.unwrap(), b"kept content");
}

#[tokio::test]
async fn ownership_and_unsafe_groups_block_conversion_without_changing_any_member_or_files() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let alice = env.user("alice", true).await;
    let next = env.user("next", true).await;
    let Json(space) = drives::create(State(env.st.clone()), admin.clone(), req(json!({"name":"Project"}))).await.unwrap();
    let space = serde_json::to_value(space).unwrap();
    let root = space["root_id"].as_str().unwrap();
    let _ = grant(&env, &admin, root, "user", alice.id, "owner", None).await.unwrap();
    assert!(convert_to_guest(&env, &admin, &alice).await.map(|_| ()).unwrap_err().message.contains("Project"));
    let _ = grant(&env, &admin, root, "user", next.id, "owner", None).await.unwrap();
    let _ = grant(&env, &admin, root, "user", alice.id, "editor", Some(now() + 86400)).await.unwrap();
    let g = group(&env, &admin, "Managers", &[alice.id, next.id]).await;
    let _ = grant(&env, &admin, root, "group", g, "manager", None).await.unwrap();
    assert!(convert_to_guest(&env, &admin, &alice).await.map(|_| ()).unwrap_err().message.contains("Managers"));
    let (still_standard,role,count): (bool,Role,i64)=sqlx::query_as("SELECT NOT u.guest, g.role, (SELECT COUNT(*) FROM group_members WHERE group_id=?1) FROM users u JOIN grants g ON g.principal_type='group' AND g.principal_id=?1 WHERE u.id=?2").bind(g).bind(alice.id).fetch_one(&env.st.db).await.unwrap();
    assert!(still_standard);
    assert_eq!(role, Role::Manager);
    assert_eq!(count, 2);
    let _ = grant(&env, &admin, root, "group", g, "editor", Some(now() + 86400)).await.unwrap();
    let _ = convert_to_guest(&env, &admin, &alice).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_admin_context_queued_before_conversion_cannot_commit_settings_after_it() {
    let env = testutil::env().await;
    let first = env.admin().await;
    let second = env.user("second", true).await;
    sqlx::query("UPDATE users SET role='admin' WHERE id=?").bind(second.id).execute(&env.st.db).await.unwrap();
    let second = auth::user_by_id(&env.st, &mut env.st.db.acquire().await.unwrap(), second.id).await.unwrap().unwrap();
    let lock = env.st.write_lock.lock().await;
    let state = env.st.clone();
    let stale = second.clone();
    let waiting = tokio::spawn(async move { crate::admin::update_settings(State(state), Admin(stale), req(json!({"share_own_groups":true}))).await });
    let mut tx = crate::db::begin_write(&env.st.db).await.unwrap();
    let p = preview_in(&mut tx, second.id).await.unwrap();
    convert(&mut tx, second.id, Some(&p.revision)).await.unwrap();
    sqlx::query("UPDATE users SET role='user',guest=1 WHERE id=?").bind(second.id).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    drop(lock);
    assert_eq!(waiting.await.unwrap().map(|_| ()).unwrap_err().status, StatusCode::FORBIDDEN);
    assert!(!own_groups(&mut env.st.db.acquire().await.unwrap()).await.unwrap());
    let self_change = crate::admin::update(
        State(env.st.clone()),
        Admin(first.clone()),
        Path(first.id),
        req(json!({"guest":true,"role":"user","confirm_guest":true,"my_password":testutil::password()})),
    )
    .await
    .map(|_| ())
    .unwrap_err();
    assert!(self_change.message.contains("your own"));
}

struct Http {
    base: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Http {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn http(env: &TestEnv) -> Http {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = crate::app::routes::router(env.st.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap();
    });
    Http { base, task }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guest_http_and_webdav_read_write_only_explicitly_shared_files_and_expiry_applies_immediately() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let outside = guest(&env, "outside").await;
    let shared = env.folder(&admin, admin.root(), "Client").await;
    let file = env.stored_file(&admin, &shared, "notes.txt", b"shared bytes").await;
    let _ = grant(&env, &admin, &shared, "user", outside.id, "editor", Some(now() + 86400)).await.unwrap();
    let (_, cookie) = env.sign_in(&outside, "Guest contracts").await;
    let server = http(&env).await;
    let client = reqwest::Client::new();
    let me: Value =
        serde_json::from_str(&client.get(format!("{}/api/auth/me", server.base)).header("cookie", &cookie).send().await.unwrap().text().await.unwrap()).unwrap();
    assert_eq!(me["guest"], true);
    assert!(me["root_id"].is_null());
    assert_eq!(me["can_create_drive"], false);
    assert_eq!(me["can_share"], false);
    assert_eq!(
        client.get(format!("{}/api/files/{file}/content", server.base)).header("cookie", &cookie).send().await.unwrap().bytes().await.unwrap(),
        b"shared bytes".as_slice()
    );
    let token = crate::util::random_token(40);
    let id = crate::util::random_token(crate::auth::app_passwords::ID_LEN);
    let token = format!("tfa_{id}_{token}");
    sqlx::query("INSERT INTO app_passwords(id,user_id,name,token_hash,scope,created_at) VALUES(?,?, 'Guest DAV',?,'write',?)")
        .bind(id)
        .bind(outside.id)
        .bind(crate::util::sha256_hex(token.as_bytes()))
        .bind(now())
        .execute(&env.st.db)
        .await
        .unwrap();
    let root = client
        .request(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), format!("{}/dav/", server.base))
        .basic_auth(&outside.username, Some(&token))
        .header("depth", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(root.status(), StatusCode::MULTI_STATUS);
    let xml = root.text().await.unwrap();
    assert!(!xml.contains("My files") && !xml.contains("All files"));
    let path = format!("{}/dav/Shared%20with%20me/Client/upload.txt", server.base);
    assert_eq!(client.put(&path).basic_auth(&outside.username, Some(&token)).body("guest upload").send().await.unwrap().status(), StatusCode::CREATED);
    assert_eq!(client.get(&path).basic_auth(&outside.username, Some(&token)).send().await.unwrap().text().await.unwrap(), "guest upload");
    sqlx::query("UPDATE grants SET expires_at=? WHERE principal_type='user' AND principal_id=?").bind(now() - 1).bind(outside.id).execute(&env.st.db).await.unwrap();
    assert_eq!(client.get(&path).basic_auth(&outside.username, Some(&token)).send().await.unwrap().status(), StatusCode::NOT_FOUND);
    assert_eq!(client.get(format!("{}/api/files/{file}/content", server.base)).header("cookie", &cookie).send().await.unwrap().status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn guest_directory_hides_principals_in_a_restricted_child_until_the_guest_is_explicitly_invited() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let outside = guest(&env, "outside").await;
    let unrelated = env.user("secret-person", true).await;
    let department = env.folder(&admin, admin.root(), "Department").await;
    let hr = env.folder(&admin, &department, "HR").await;
    sqlx::query("UPDATE nodes SET restricted=1 WHERE id=?").bind(&hr).execute(&env.st.db).await.unwrap();
    let secret = group(&env, &admin, "SecretGroup", &[unrelated.id]).await;
    let _ = grant(&env, &admin, &department, "user", outside.id, "viewer", Some(now() + 86400)).await.unwrap();
    let _ = grant(&env, &admin, &hr, "user", unrelated.id, "viewer", None).await.unwrap();
    let _ = grant(&env, &admin, &hr, "group", secret, "viewer", None).await.unwrap();
    for invited in [false, true, false] {
        if invited {
            let _ = grant(&env, &admin, &hr, "user", outside.id, "viewer", Some(now() + 86400)).await.unwrap();
        } else {
            sqlx::query("DELETE FROM grants WHERE node_id=? AND principal_type='user' AND principal_id=?")
                .bind(&hr)
                .bind(outside.id)
                .execute(&env.st.db)
                .await
                .unwrap();
        }
        let Json(rows) = drives::directory(State(env.st.clone()), outside.clone(), Query(serde_json::from_value(json!({"q":""})).unwrap())).await.unwrap();
        let rows = serde_json::to_value(rows).unwrap();
        for name in ["secret-person", "SecretGroup"] {
            assert_eq!(rows.as_array().unwrap().iter().any(|p| p["name"] == name), invited, "directory visibility for {name}");
        }
        assert_eq!(crate::tree::node_for(&mut env.st.db.acquire().await.unwrap(), &outside, &hr, crate::tree::Need::Read).await.is_ok(), invited);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unconfirmed_standard_update_queued_before_conversion_cannot_restore_guest_privileges() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let target = env.user("outside", true).await;
    let lock = env.st.write_lock.lock().await;
    let st = env.st.clone();
    let actor = admin.clone();
    let id = target.id;
    let waiting =
        tokio::spawn(
            async move { crate::admin::update(State(st), Admin(actor), Path(id), req(json!({"guest":false,"display_name":"Queued standard update"}))).await },
        );
    // The request is already live while conversion holds the writer. Both schedules must refuse privilege restoration.
    tokio::task::yield_now().await;
    let mut tx = crate::db::begin_write(&env.st.db).await.unwrap();
    let p = preview_in(&mut tx, target.id).await.unwrap();
    convert(&mut tx, target.id, Some(&p.revision)).await.unwrap();
    sqlx::query("UPDATE users SET guest=1 WHERE id=?").bind(target.id).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    drop(lock);
    assert!(waiting.await.unwrap().is_err());
    let current = auth::user_by_id(&env.st, &mut env.st.db.acquire().await.unwrap(), target.id).await.unwrap().unwrap();
    assert!(current.guest && current.root_id.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn existing_public_links_and_signed_s3_keys_stop_on_conversion_without_deleting_their_data() {
    use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region, RequestChecksumCalculation};
    let env = testutil::env().await;
    let admin = env.admin().await;
    let alice = env.user("alice", true).await;
    let shared = env.folder(&admin, &env.st.shared_root().unwrap(), "Published").await;
    let _ = grant(&env, &admin, &shared, "user", alice.id, "manager", None).await.unwrap();
    let file = env.stored_file(&alice, &shared, "shared.txt", b"keep published content").await;
    let Json(link) = crate::shares::create(State(env.st.clone()), alice.clone(), req(json!({"node_id":file}))).await.unwrap();
    let link = serde_json::to_value(link).unwrap()["id"].as_str().unwrap().to_owned();
    assert!(crate::shares::check_stream_access(&env.st, &link).await.is_ok());
    let _ = crate::s3::config::save(
        State(env.st.clone()),
        Admin(admin.clone()),
        req(json!({"enabled":true,"hostname":"127.0.0.1","region":"us-east-1","managers_publish":true})),
    )
    .await
    .unwrap();
    let Json(bucket) = crate::s3::manage::create(
        State(env.st.clone()),
        alice.clone(),
        req(json!({"node_id":shared,"name":"guest-conversion","password":crate::testutil::password()})),
    )
    .await
    .unwrap();
    let Json(key) = crate::s3::manage::new_key(
        State(env.st.clone()),
        alice.clone(),
        Path(bucket.id.clone()),
        req(json!({"description":"Conversion regression","writable":true,"expires_at":null,"password":crate::testutil::password()})),
    )
    .await
    .unwrap();
    let server = http(&env).await;
    let client = aws_sdk_s3::Client::from_conf(
        aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new("us-east-1"))
            .credentials_provider(Credentials::new(&key.key.access_key, &key.secret_key, None, None, "contract"))
            .endpoint_url(&server.base)
            .force_path_style(true)
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
            .build(),
    );
    assert!(client.head_object().bucket(&bucket.name).key("shared.txt").send().await.is_ok());
    let _ = convert_to_guest(&env, &admin, &alice).await.unwrap();
    assert!(crate::shares::check_stream_access(&env.st, &link).await.is_err());
    assert!(client.head_object().bucket(&bucket.name).key("shared.txt").send().await.is_err());
    assert!(
        client
            .put_object()
            .bucket(&bucket.name)
            .key("forbidden.txt")
            .body(aws_sdk_s3::primitives::ByteStream::from_static(b"must not commit"))
            .send()
            .await
            .is_err()
    );
    assert!(crate::s3::credentials::identity(&env.st, &key.key.access_key, true).await.is_err());
    let current = auth::user_by_id(&env.st, &mut env.st.db.acquire().await.unwrap(), alice.id).await.unwrap().unwrap();
    assert_eq!(crate::access::read_at(&env.st, &current, &file, 0, 64).await.unwrap(), b"keep published content");
    let (links, buckets, keys): (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM shares WHERE owner_id=?1),(SELECT COUNT(*) FROM s3_buckets WHERE creator_id=?1),(SELECT COUNT(*) FROM s3_keys)",
    )
    .bind(alice.id)
    .fetch_one(&env.st.db)
    .await
    .unwrap();
    assert_eq!((links, buckets, keys), (1, 1, 1));
}
