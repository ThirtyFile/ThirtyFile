//! Access review: what each view lists, what it leaves out, and the reminder

use super::*;
use crate::{
    auth::{self, User},
    testutil::{self, TestEnv},
};

async fn guest(env: &TestEnv, name: &str) -> User {
    let hash = auth::hash_password(testutil::password().into()).await.unwrap();
    let _w = env.st.write_lock.lock().await;
    let mut tx = db::begin_write(&env.st.db).await.unwrap();
    let id = crate::users::create_user(
        &mut tx,
        crate::users::NewUser {
            username: name,
            password_hash: &hash,
            role: auth::UserRole::User,
            guest: true,
            can_write: true,
            can_delete: true,
            can_share: false,
            quota_bytes: 0,
            source: "password",
            provisioned_by: None,
            personal_space: None,
            space_folders: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    auth::user_by_id(&env.st, &mut env.st.db.acquire().await.unwrap(), id).await.unwrap().unwrap()
}

async fn add_grant(env: &TestEnv, node: &str, kind: &str, id: i64, role: &str, expires_at: Option<i64>) {
    db::add_grant(&mut env.st.db.acquire().await.unwrap(), node, kind, id, role, None, expires_at).await.unwrap();
}

async fn run(env: &TestEnv, sql: &str, binds: &[&str]) {
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
    for b in binds {
        q = q.bind(*b);
    }
    q.execute(&env.st.db).await.unwrap();
}

struct Scene {
    env: TestEnv,
    amy: User,
    ben: User,
    outside: User,
    group: i64,
    team: String,
    client: String,
    hr: String,
}

/// A team space Amy owns, with a group as editors, a folder shared with a guest until a date, a restricted folder;
/// Ben's personal file shared; links, a bucket and a receive folder; and things that have ended
async fn scene() -> Scene {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let amy = env.user("amy", true).await;
    let ben = env.user("ben", true).await;
    let cat = env.user("cat", true).await;
    let outside = guest(&env, "outside").await;
    let (_, team) = db::create_drive(&mut env.st.db.acquire().await.unwrap(), "Projects", "team", amy.id, 0, "local").await.unwrap();
    add_grant(&env, &team, "user", amy.id, "owner", None).await;
    let Json(made) = crate::drives::create_group(
        State(env.st.clone()),
        Admin(admin.clone()),
        Json(serde_json::from_value(json!({"name": "Design", "members": [cat.id]})).unwrap()),
    )
    .await
    .unwrap();
    let group = made["id"].as_i64().unwrap();
    add_grant(&env, &team, "group", group, "editor", None).await;
    let client = env.folder(&amy, &team, "Client").await;
    let soon = now() + 3 * 86400;
    add_grant(&env, &client, "user", outside.id, "viewer", Some(soon)).await;
    // Ended: never listed
    add_grant(&env, &client, "user", ben.id, "editor", Some(now() - 60)).await;
    let hr = env.folder(&amy, &team, "HR").await;
    run(&env, "UPDATE nodes SET restricted = 1 WHERE id = ?", &[&hr]).await;
    add_grant(&env, &hr, "user", cat.id, "viewer", Some(now() + 30 * 86400)).await;
    // Everyone in the company space, and everyone in a folder until a date (which a guest still doesn't get)
    let company = env.st.shared_root().unwrap();
    add_grant(&env, &company, "everyone", 0, "viewer", None).await;
    let news = env.folder(&amy, &team, "News").await;
    add_grant(&env, &news, "everyone", 0, "viewer", Some(now() + 86400)).await;
    // Ben's personal file, shared with Amy and by a link
    let private = env.file(&ben, ben.root(), "salary-review-2026.txt").await;
    add_grant(&env, &private, "user", amy.id, "viewer", None).await;
    let created = now().to_string();
    run(&env, "INSERT INTO shares (id, node_id, owner_id, created_at) VALUES ('link-private', ?, ?, ?)", &[&private, &ben.id.to_string(), &created]).await;
    run(
        &env,
        "INSERT INTO shares (id, node_id, owner_id, password_hash, created_at) VALUES ('link-client', ?, ?, 'x', ?)",
        &[&client, &amy.id.to_string(), &created],
    )
    .await;
    run(&env, "INSERT INTO shares (id, node_id, owner_id, expires_at, created_at) VALUES ('link-ended', ?, ?, 1, ?)", &[&client, &amy.id.to_string(), &created])
        .await;
    run(
        &env,
        "INSERT INTO s3_buckets (id, name, node_id, creator_id, created_at) VALUES ('b1', 'client-files', ?, ?, ?)",
        &[&client, &amy.id.to_string(), &created],
    )
    .await;
    run(
        &env,
        "INSERT INTO s3_keys (id, bucket_id, access_key, secret, description, writable, created_at) VALUES ('k1', 'b1', 'AK1', 's', 'Reader', 0, ?)",
        &[&created],
    )
    .await;
    run(
        &env,
        "INSERT INTO s3_keys (id, bucket_id, access_key, secret, description, writable, expires_at, created_at) VALUES ('k2', 'b1', 'AK2', 's', 'Old', 1, 1, ?)",
        &[&created],
    )
    .await;
    run(
        &env,
        "INSERT INTO ftp_receive_folders (id, node_id, creator_id, name, username, password_hash, enabled, allow_list, allow_read, replace_existing, created_at) VALUES ('f1', ?, ?, 'Scanner', 'ftp-scan', 'x', 1, 0, 0, 1, ?)",
        &[&client, &amy.id.to_string(), &created],
    )
    .await;
    run(
        &env,
        "INSERT INTO app_passwords (id, user_id, name, token_hash, scope, created_at) VALUES ('ap1', ?, 'Nightly sync', 'h', 'read', ?)",
        &[&cat.id.to_string(), &created],
    )
    .await;
    run(
        &env,
        "INSERT INTO sftp_keys (id, user_id, name, public_key, fingerprint, created_at) VALUES ('sk1', ?, 'Laptop', 'ssh-ed25519 AAAA', 'SHA256:x', ?)",
        &[&cat.id.to_string(), &created],
    )
    .await;
    Scene { env, amy, ben, outside, group, team, client, hr }
}

#[tokio::test]
async fn spaces_list_their_members_and_the_folders_shared_or_restricted_in_them() {
    let s = scene().await;
    let o = overview(&mut s.env.st.db.acquire().await.unwrap()).await.unwrap();
    let team = o.spaces.iter().find(|sp| sp.id == o.spaces.iter().find(|sp| sp.name == "Projects").unwrap().id).unwrap();
    let members: Vec<(&str, &str, &str)> = team.members.iter().map(|g| (g.principal.kind.as_str(), g.principal.name.as_str(), g.role.as_str())).collect();
    assert_eq!(members, [("user", "amy", "owner"), ("group", "Design", "editor")]);
    let folders: Vec<(Vec<String>, bool, Vec<&str>)> =
        team.folders.iter().map(|f| (f.place.path.clone().unwrap(), f.restricted, f.grants.iter().map(|g| g.principal.name.as_str()).collect())).collect();
    assert_eq!(
        folders,
        [(vec!["Client".to_string()], false, vec!["outside"]), (vec!["HR".to_string()], true, vec!["cat"]), (vec!["News".to_string()], false, vec!["Everyone"])]
    );
    // The guest is marked
    assert!(team.folders[0].grants[0].principal.guest);
    // The company space: everyone
    let company = o.spaces.iter().find(|sp| sp.kind == "company").unwrap();
    assert_eq!(company.members.iter().map(|g| g.principal.kind.as_str()).collect::<Vec<_>>(), ["everyone"]);
    // Ended grants are nowhere
    assert!(o.expiring.iter().all(|e| e.grant.principal.name != "ben"));
    assert!(team.folders.iter().flat_map(|f| &f.grants).all(|g| g.principal.name != "ben"));
    let _ = (&s.team, &s.hr, s.group);
}

#[tokio::test]
async fn people_get_what_names_them_their_groups_and_everyone_with_their_credentials() {
    let s = scene().await;
    let o = overview(&mut s.env.st.db.acquire().await.unwrap()).await.unwrap();
    let person = |name: &str| o.people.iter().find(|p| p.username == name).unwrap();
    let cat = person("cat");
    assert_eq!(cat.groups, ["Design"]);
    let mut via: Vec<(String, &str, Option<&str>)> = cat.reach.iter().map(|r| (r.place.space.clone(), r.via.as_str(), r.group.as_deref())).collect();
    via.sort();
    assert!(via.contains(&("Projects".into(), "group", Some("Design"))), "{via:?}");
    assert!(via.contains(&("Projects".into(), "direct", None)), "the restricted folder: {via:?}");
    assert!(via.iter().any(|(_, v, _)| *v == "everyone"), "the company space: {via:?}");
    assert_eq!(cat.app_passwords.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Nightly sync"]);
    assert_eq!(cat.ssh_keys.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(), ["Laptop"]);
    // A guest gets only what names it with an end date, never "everyone"
    let outside = person("outside");
    assert!(outside.guest);
    assert_eq!(outside.reach.len(), 1, "{:?}", outside.reach.iter().map(|r| &r.via).collect::<Vec<_>>());
    assert_eq!((outside.reach[0].via.as_str(), outside.reach[0].place.node_id.as_deref()), ("direct", Some(s.client.as_str())));
    // Grants with an end date, soonest first
    let ends: Vec<i64> = o.expiring.iter().filter_map(|e| e.grant.expires_at).collect();
    assert!(ends.windows(2).all(|w| w[0] <= w[1]) && ends.len() >= 2, "{ends:?}");
    let _ = (&s.amy, &s.outside);
}

#[tokio::test]
async fn outside_access_lists_what_works_and_personal_spaces_are_never_named() {
    let s = scene().await;
    let o = overview(&mut s.env.st.db.acquire().await.unwrap()).await.unwrap();
    // Links that work: the ended one is left out
    assert_eq!(o.links.len(), 2);
    let client_link = o.links.iter().find(|l| l.place.node_id.as_deref() == Some(s.client.as_str())).unwrap();
    assert!(client_link.password);
    assert_eq!(o.buckets.len(), 1);
    assert_eq!(o.buckets[0].keys.iter().map(|k| k.description.as_str()).collect::<Vec<_>>(), ["Reader"], "the ended key is left out");
    assert_eq!(o.receive_folders.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["Scanner"]);
    // Ben's personal space: counted, and named only by its owner
    let ben = o.personal.iter().find(|p| p.owner == "ben").unwrap();
    assert_eq!((ben.items, ben.grants, ben.links), (1, 1, 1));
    let private = o.links.iter().find(|l| l.place.space_kind == "personal").unwrap();
    assert!(private.place.path.is_none() && private.place.node_id.is_none());
    assert_eq!(private.place.space, "ben");
    let all = serde_json::to_string(&o).unwrap();
    assert!(!all.contains("salary-review"), "a personal item's name is in the review");
    let _ = &s.ben;
}

#[tokio::test]
async fn a_due_review_is_told_once_and_marking_it_reviewed_counts_from_then() {
    let s = scene().await;
    let admin = s.env.admin().await;
    let st = || State(s.env.st.clone());
    assert!(set_reminder(st(), Admin(admin.clone()), Json(ReminderChange { every_days: MAX_DAYS + 1 })).await.is_err());
    let Json(state) = set_reminder(st(), Admin(admin.clone()), Json(ReminderChange { every_days: 7 })).await.unwrap();
    let due = state.due_at.unwrap();
    assert!(due > now() + 6 * 86400);
    let told = || async {
        let (n,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM notifications WHERE user_id = ? AND kind = 'access_review'").bind(admin.id).fetch_one(&s.env.st.db).await.unwrap();
        n
    };
    // Not yet, then once
    remind(&s.env.st, due - 60).await.unwrap();
    assert_eq!(told().await, 0);
    remind(&s.env.st, due + 60).await.unwrap();
    remind(&s.env.st, due + 120).await.unwrap();
    assert_eq!(told().await, 1);
    // Reviewed: logged, and the next one counted from now; nothing more is told until then
    let Json(state) = reviewed(st(), Admin(admin.clone())).await.unwrap();
    assert_eq!(state.last_reviewed_by.as_deref(), Some(admin.username.as_str()));
    assert!(state.due_at.unwrap() > now() + 6 * 86400);
    remind(&s.env.st, now()).await.unwrap();
    assert_eq!(told().await, 1);
    let (logged,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM activity WHERE action = 'access_review' AND detail = 'Reviewed access'").fetch_one(&s.env.st.db).await.unwrap();
    assert_eq!(logged, 1);
    remind(&s.env.st, state.due_at.unwrap() + 1).await.unwrap();
    assert_eq!(told().await, 2);
    // Off: never told
    let _ = set_reminder(st(), Admin(admin.clone()), Json(ReminderChange { every_days: 0 })).await.unwrap();
    remind(&s.env.st, now() + 400 * 86400).await.unwrap();
    assert_eq!(told().await, 2);
}
