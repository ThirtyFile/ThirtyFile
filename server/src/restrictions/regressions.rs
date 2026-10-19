//! API regressions exercise visibility and mutations through real authorization extractors and routes.

use crate::{
    auth::User,
    testutil::{self, TestEnv},
};
use axum::{
    Router,
    body::{Body, Bytes},
    http::{Method, Request, StatusCode, header},
};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Fixture {
    env: TestEnv,
    app: Router,
    owner: User,
    member: User,
    own_cookie: String,
    member_cookie: String,
    root: String,
    department: String,
    restricted: String,
    secret: String,
    visible: String,
}

async fn fixture() -> Fixture {
    let env = testutil::env().await;
    let owner = env.user("owner", true).await;
    let member = env.user("member", true).await;
    let root = {
        let mut c = env.st.db.acquire().await.unwrap();
        let (_, root) = crate::db::create_drive(&mut c, "Department space", "team", owner.id, 0, "local").await.unwrap();
        crate::db::add_grant(&mut c, &root, "user", owner.id, "owner", Some(owner.id), None).await.unwrap();
        root
    };
    env.grant(&root, &member, "manager").await;
    let department = env.folder(&owner, &root, "Department").await;
    let restricted = env.folder(&owner, &department, "Restricted Payroll").await;
    let secret = env.upload(&owner, &restricted, "compensation-secret.txt", b"confidential-payroll").await;
    let visible = env.upload(&owner, &department, "minutes.txt", b"public-minutes").await;
    let (_, own_cookie) = env.sign_in(&owner, "Restriction regression").await;
    let (_, member_cookie) = env.sign_in(&member, "Restriction regression").await;
    let app = crate::app::routes::router(env.st.clone());
    Fixture { env, app, owner, member, own_cookie, member_cookie, root, department, restricted, secret, visible }
}

async fn request(f: &Fixture, cookie: &str, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Bytes) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::COOKIE, cookie)
        .header(header::HOST, "files.example.com")
        .header(header::ORIGIN, "http://files.example.com")
        .header(header::USER_AGENT, "Restriction regression")
        .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from(([10, 0, 0, 1], 5000))));
    let body = match body {
        Some(value) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let response = f.app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 16 << 20).await.unwrap();
    (status, bytes)
}

async fn json_call(f: &Fixture, cookie: &str, method: Method, path: &str, body: Option<Value>) -> Value {
    let (status, bytes) = request(f, cookie, method, path, body).await;
    assert_eq!(status, StatusCode::OK, "{path}: {}", String::from_utf8_lossy(&bytes));
    serde_json::from_slice(&bytes).unwrap()
}

async fn restrict(f: &Fixture) {
    json_call(
        f,
        &f.own_cookie,
        Method::POST,
        &format!("/api/nodes/{}/restriction", f.restricted),
        Some(json!({"restricted":true,"expected":false,"confirmed":true})),
    )
    .await;
}

async fn restrict_node(f: &Fixture, id: &str) {
    let path = format!("/api/nodes/{id}/restriction");
    json_call(f, &f.own_cookie, Method::POST, &path, Some(json!({"restricted":true,"expected":false,"confirmed":true}))).await;
}

fn assert_hidden(f: &Fixture, value: &Value) {
    let text = value.to_string();
    for hidden in [&f.restricted, &f.secret, "Restricted Payroll", "compensation-secret.txt", "confidential-payroll"] {
        assert!(!text.contains(hidden), "hidden value {hidden} appeared in {text}");
    }
}

#[tokio::test]
async fn lists_search_tags_recent_history_notifications_and_counts_hide_restricted_items() {
    let f = fixture().await;
    json_call(&f, &f.member_cookie, Method::POST, "/api/nodes/favorite", Some(json!({"ids":[f.secret],"favorite":true}))).await;
    let tag = json_call(&f, &f.member_cookie, Method::POST, "/api/tags", Some(json!({"name":"Review","color":"blue"}))).await;
    json_call(&f, &f.member_cookie, Method::POST, "/api/nodes/tags", Some(json!({"ids":[f.secret],"add":[tag["id"]],"remove":[]}))).await;
    let smart = json_call(&f, &f.member_cookie, Method::POST, "/api/smart-folders", Some(json!({"name":"Text files","query":{"ext":"txt"}}))).await;
    let (status, _) = request(&f, &f.member_cookie, Method::GET, &format!("/api/files/{}/content", f.secret), None).await;
    assert_eq!(status, StatusCode::OK);
    let _ = crate::notify::add(
        &mut f.env.st.db.acquire().await.unwrap(),
        &[f.member.id],
        &crate::notify::Notice { kind: "shared", node_id: Some(f.secret.clone()), data: json!({"name":"compensation-secret.txt","item":"file"}) },
    )
    .await
    .unwrap();
    restrict(&f).await;
    let children = json_call(&f, &f.member_cookie, Method::GET, &format!("/api/nodes/{}/children?limit=1&offset=0", f.department), None).await;
    assert_hidden(&f, &children);
    assert_eq!(children["total"], 1);
    assert_eq!(children["items"][0]["id"], f.visible);
    for path in [
        "/api/search?q=secret".into(),
        "/api/favorites".into(),
        "/api/recent".into(),
        format!("/api/tags/{}/items", tag["id"]),
        format!("/api/smart-folders/{}/items", smart["id"]),
        format!("/api/nodes/{}/activity", f.department),
        format!("/api/activity?drive_id={}", f.env.drive_of(&f.root).await),
        "/api/notifications".into(),
        "/api/shared-with-me".into(),
    ] {
        let value = json_call(&f, &f.member_cookie, Method::GET, &path, None).await;
        assert_hidden(&f, &value);
    }
    let selection = json_call(&f, &f.member_cookie, Method::POST, &format!("/api/nodes/{}/select", f.department), Some(json!({}))).await;
    assert_eq!(selection["ids"], json!([f.visible]));
    let position = json_call(&f, &f.member_cookie, Method::GET, &format!("/api/nodes/{}/position?item={}", f.department, f.restricted), None).await;
    assert_eq!(position["total"], 1);
    assert_eq!(position["position"], Value::Null);
    let contents = json_call(&f, &f.member_cookie, Method::POST, "/api/nodes/contents", Some(json!({"ids":[f.department]}))).await;
    assert_eq!(contents, json!({"size":14,"files":1,"folders":0}));
    for path in [
        format!("/api/nodes/{}", f.restricted),
        format!("/api/nodes/{}/access", f.restricted),
        format!("/api/files/{}/content", f.secret),
        format!("/api/files/{}/versions", f.secret),
        format!("/api/files/{}/thumbnail", f.secret),
    ] {
        assert_eq!(request(&f, &f.member_cookie, Method::GET, &path, None).await.0, StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn directly_shared_descendants_do_not_reveal_hidden_parents_and_stop_after_revocation() {
    let f = fixture().await;
    let child = f.env.folder(&f.owner, &f.restricted, "Visible contract").await;
    let file = f.env.upload(&f.owner, &child, "contract.txt", b"permitted contract").await;
    restrict(&f).await;
    f.env.grant(&child, &f.member, "viewer").await;
    let info = json_call(&f, &f.member_cookie, Method::GET, &format!("/api/nodes/{child}"), None).await;
    assert_eq!(info["path"].as_array().unwrap().len(), 1);
    assert_eq!(info["path"][0]["name"], "Visible contract");
    assert_hidden(&f, &info);
    let shared = json_call(&f, &f.member_cookie, Method::GET, "/api/shared-with-me", None).await;
    assert!(shared.as_array().unwrap().iter().any(|n| n["id"] == child));
    assert_hidden(&f, &shared);
    let search = json_call(&f, &f.member_cookie, Method::GET, "/api/search?q=contract", None).await;
    assert!(search["items"].as_array().unwrap().iter().any(|n| n["id"] == file));
    assert_hidden(&f, &search);
    f.env.revoke(&child, &f.member).await;
    assert_eq!(request(&f, &f.member_cookie, Method::GET, &format!("/api/files/{file}/content"), None).await.0, StatusCode::NOT_FOUND);
    assert_hidden(&f, &json_call(&f, &f.member_cookie, Method::GET, "/api/shared-with-me", None).await);
}

#[tokio::test]
async fn parent_download_filters_hidden_content_but_recursive_mutations_are_refused() {
    let f = fixture().await;
    restrict(&f).await;
    let (status, zip) = request(&f, &f.member_cookie, Method::GET, &format!("/api/download?ids={}", f.department), None).await;
    assert_eq!(status, StatusCode::OK);
    let mut cursor = std::io::Cursor::new(&zip);
    let names: Vec<String> = crate::zip::read_entries(&mut cursor, 100, 1 << 20).unwrap().into_iter().map(|entry| entry.name).collect();
    assert_eq!(names, vec!["Department/", "Department/minutes.txt"]);
    let destination = f.env.folder(&f.owner, &f.root, "Destination").await;
    for path in ["/api/nodes/copy", "/api/nodes/move", "/api/nodes/trash"] {
        let (status, bytes) = request(&f, &f.member_cookie, Method::POST, path, Some(json!({"ids":[f.department],"dest_id":destination}))).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {}", String::from_utf8_lossy(&bytes));
        assert_hidden(&f, &serde_json::from_slice(&bytes).unwrap());
    }
    assert_eq!(
        request(&f, &f.member_cookie, Method::POST, "/api/archive/compress", Some(json!({"ids":[f.department],"parent_id":f.root}))).await.0,
        StatusCode::FORBIDDEN
    );
    let (read, body) = request(&f, &f.own_cookie, Method::GET, &format!("/api/files/{}/content", f.secret), None).await;
    assert_eq!(read, StatusCode::OK);
    assert_eq!(&body[..], b"confidential-payroll");
}

#[tokio::test]
async fn saved_names_of_children_purged_before_restriction_remain_private_afterwards() {
    let f = fixture().await;
    let _ = crate::notify::add(
        &mut f.env.st.db.acquire().await.unwrap(),
        &[f.member.id, f.owner.id],
        &crate::notify::Notice { kind: "shared", node_id: Some(f.secret.clone()), data: json!({"name":"compensation-secret.txt","item":"file"}) },
    )
    .await
    .unwrap();
    json_call(&f, &f.own_cookie, Method::POST, "/api/nodes/trash", Some(json!({"ids":[f.secret]}))).await;
    let job = json_call(&f, &f.own_cookie, Method::POST, "/api/trash/delete", Some(json!({"ids":[f.secret]}))).await;
    if let Some(id) = job["id"].as_str().filter(|id| !id.is_empty()) {
        assert_eq!(crate::jobs::wait_for(&f.env.st, id).await.state, "done");
    }
    assert!(crate::tree::get_node(&mut f.env.st.db.acquire().await.unwrap(), &f.secret).await.unwrap().is_none());
    restrict(&f).await;
    for path in [format!("/api/activity?drive_id={}", f.env.drive_of(&f.root).await), format!("/api/nodes/{}/activity", f.root), "/api/notifications".into()] {
        let hidden = json_call(&f, &f.member_cookie, Method::GET, &path, None).await;
        assert_hidden(&f, &hidden);
        let owner = json_call(&f, &f.own_cookie, Method::GET, &path, None).await;
        assert!(owner.to_string().contains("compensation-secret.txt"), "{path}: {owner}");
    }
    let notifications = json_call(&f, &f.member_cookie, Method::GET, "/api/notifications", None).await;
    assert_eq!(notifications["unread"], 0);
}

#[tokio::test]
async fn owners_control_nested_boundaries_and_managers_cannot_regrant_themselves_access() {
    let f = fixture().await;
    restrict(&f).await;
    let grant = json!({"principal_type":"user","principal_id":f.member.id,"role":"manager"});
    assert_eq!(request(&f, &f.member_cookie, Method::POST, &format!("/api/nodes/{}/access", f.restricted), Some(grant)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        request(&f, &f.own_cookie, Method::POST, &format!("/api/nodes/{}/restriction", f.restricted), Some(json!({"restricted":false,"expected":true}))).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(&f, &f.own_cookie, Method::POST, &format!("/api/nodes/{}/restriction", f.root), Some(json!({"restricted":true,"expected":false,"confirmed":true})))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    f.env.grant(&f.restricted, &f.member, "editor").await;
    assert_eq!(request(&f, &f.member_cookie, Method::GET, &format!("/api/files/{}/content", f.secret), None).await.0, StatusCode::OK);
    let inner = f.env.folder(&f.member, &f.restricted, "Personal additions").await;
    let inner_file = f.env.upload(&f.member, &inner, "own.txt", b"added by member").await;
    json_call(&f, &f.own_cookie, Method::POST, &format!("/api/nodes/{inner}/restriction"), Some(json!({"restricted":true,"expected":false,"confirmed":true}))).await;
    assert_eq!(request(&f, &f.member_cookie, Method::GET, &format!("/api/files/{inner_file}/content"), None).await.0, StatusCode::NOT_FOUND);
    let admin = f.env.admin().await;
    let (_, cookie) = f.env.sign_in(&admin, "Restriction regression").await;
    assert_eq!(request(&f, &cookie, Method::GET, &format!("/api/files/{inner_file}/content"), None).await.0, StatusCode::OK);
    let personal = f.env.folder(&f.owner, f.owner.root(), "Private").await;
    let private = f.env.upload(&f.owner, &personal, "private.txt", b"private").await;
    json_call(&f, &f.own_cookie, Method::POST, &format!("/api/nodes/{personal}/restriction"), Some(json!({"restricted":true,"expected":false,"confirmed":true})))
        .await;
    assert_eq!(request(&f, &cookie, Method::GET, &format!("/api/files/{private}/content"), None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        request(&f, &cookie, Method::POST, &format!("/api/nodes/{personal}/restriction"), Some(json!({"restricted":false,"expected":true,"confirmed":true})))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn trash_restore_and_copy_preserve_boundaries_and_exclude_hidden_items() {
    let f = fixture().await;
    restrict(&f).await;
    json_call(&f, &f.own_cookie, Method::POST, "/api/nodes/trash", Some(json!({"ids":[f.restricted]}))).await;
    assert_hidden(&f, &json_call(&f, &f.member_cookie, Method::GET, "/api/trash?limit=1", None).await);
    for path in ["/api/trash/restore", "/api/trash/delete"] {
        assert_eq!(request(&f, &f.member_cookie, Method::POST, path, Some(json!({"ids":[f.restricted]}))).await.0, StatusCode::NOT_FOUND);
    }
    json_call(&f, &f.own_cookie, Method::POST, "/api/trash/restore", Some(json!({"ids":[f.restricted]}))).await;
    let destination = f.env.folder(&f.owner, &f.root, "Destination").await;
    json_call(&f, &f.own_cookie, Method::POST, "/api/nodes/copy", Some(json!({"ids":[f.restricted],"dest_id":destination}))).await;
    let copied = crate::paths::child_named(&mut f.env.st.db.acquire().await.unwrap(), &destination, "Restricted Payroll").await.unwrap().unwrap();
    assert!(copied.restricted);
    assert_eq!(request(&f, &f.member_cookie, Method::GET, &format!("/api/nodes/{}", copied.id), None).await.0, StatusCode::NOT_FOUND);
    json_call(
        &f,
        &f.own_cookie,
        Method::POST,
        "/api/nodes/move",
        Some(json!({"ids":[f.restricted],"dest_id":destination,"resolutions":{(f.restricted.clone()):"keep"}})),
    )
    .await;
    assert!(crate::tree::get_node(&mut f.env.st.db.acquire().await.unwrap(), &f.restricted).await.unwrap().unwrap().restricted);
}

#[tokio::test]
async fn public_parent_links_never_include_restricted_children_even_for_an_owner() {
    let f = fixture().await;
    let link = json_call(&f, &f.own_cookie, Method::POST, "/api/shares", Some(json!({"node_id":f.department}))).await;
    let token = link["id"].as_str().unwrap();
    restrict(&f).await;
    let listed = json_call(&f, "", Method::GET, &format!("/api/public/shares/{token}/nodes/root/children"), None).await;
    assert_hidden(&f, &listed);
    assert_eq!(listed["items"].as_array().unwrap().len(), 1);
    assert_eq!(request(&f, "", Method::GET, &format!("/api/public/shares/{token}/nodes/{}", f.restricted), None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(request(&f, "", Method::GET, &format!("/api/public/shares/{token}/nodes/{}/content", f.secret), None).await.0, StatusCode::NOT_FOUND);
    let (status, body) = request(&f, "", Method::GET, &format!("/api/public/shares/{token}/download?ids=root"), None).await;
    assert_eq!(status, StatusCode::OK);
    let names = crate::zip::read_entries(&mut std::io::Cursor::new(&body), 100, 1 << 20).unwrap().into_iter().map(|e| e.name).collect::<Vec<_>>();
    assert_eq!(names, vec!["Department/", "Department/minutes.txt"]);
    let child = json_call(&f, &f.own_cookie, Method::POST, "/api/shares", Some(json!({"node_id":f.restricted}))).await;
    assert_eq!(
        request(&f, "", Method::GET, &format!("/api/public/shares/{}/nodes/{}/content", child["id"].as_str().unwrap(), f.secret), None).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn collision_previews_do_not_reveal_hidden_existing_nodes() {
    let f = fixture().await;
    restrict(&f).await;
    let found = json_call(
        &f,
        &f.member_cookie,
        Method::POST,
        "/api/nodes/conflicts",
        Some(json!({"dest_id":f.department,"names":["Restricted Payroll","minutes.txt"],"ids":[]})),
    )
    .await;
    assert_hidden(&f, &found);
    assert_eq!(found.as_array().unwrap().len(), 1);
    assert_eq!(found[0]["existing"]["name"], "minutes.txt");
}

#[tokio::test]
async fn planned_archives_stop_after_restriction_or_share_revocation() {
    let f = fixture().await;
    let node = crate::tree::get_node(&mut f.env.st.db.acquire().await.unwrap(), &f.restricted).await.unwrap().unwrap();
    let plan = crate::downloads::zip_plan_checked(
        &f.env.st,
        vec![node.clone()],
        0,
        Some(crate::downloads::ZipAccess { user: f.member.clone(), publication: None, share: None }),
    )
    .await
    .unwrap();
    let mut walk = plan.walk();
    assert!(walk.next(&f.env.st).await.unwrap().is_some());
    restrict(&f).await;
    assert!(walk.next(&f.env.st).await.is_err());
    let link = json_call(&f, &f.own_cookie, Method::POST, "/api/shares", Some(json!({"node_id":f.restricted}))).await;
    let token = link["id"].as_str().unwrap().to_string();
    let plan = crate::downloads::zip_plan_checked(
        &f.env.st,
        vec![node],
        0,
        Some(crate::downloads::ZipAccess { user: f.owner.clone(), publication: Some(f.restricted.clone()), share: Some(token.clone()) }),
    )
    .await
    .unwrap();
    let mut walk = plan.walk();
    assert!(walk.next(&f.env.st).await.unwrap().is_some());
    sqlx::query("DELETE FROM shares WHERE id = ?").bind(token).execute(&f.env.st.db).await.unwrap();
    assert!(walk.next(&f.env.st).await.is_err());
}

#[tokio::test]
async fn local_group_access_ends_on_membership_removal_or_grant_expiry() {
    let f = fixture().await;
    restrict(&f).await;
    let group = crate::util::new_id();
    sqlx::query("INSERT INTO groups (name,created_at) VALUES (?,?)").bind(group).bind(crate::util::now()).execute(&f.env.st.db).await.unwrap();
    let (group,): (i64,) = sqlx::query_as("SELECT id FROM groups ORDER BY id DESC LIMIT 1").fetch_one(&f.env.st.db).await.unwrap();
    sqlx::query("INSERT INTO group_members (group_id,user_id) VALUES (?,?)").bind(group).bind(f.member.id).execute(&f.env.st.db).await.unwrap();
    crate::db::add_grant(&mut f.env.st.db.acquire().await.unwrap(), &f.restricted, "group", group, "viewer", Some(f.owner.id), Some(crate::util::now() + 300))
        .await
        .unwrap();
    let content = format!("/api/files/{}/content", f.secret);
    assert_eq!(request(&f, &f.member_cookie, Method::GET, &content, None).await.0, StatusCode::OK);
    sqlx::query("DELETE FROM group_members WHERE group_id = ? AND user_id = ?").bind(group).bind(f.member.id).execute(&f.env.st.db).await.unwrap();
    assert_eq!(request(&f, &f.member_cookie, Method::GET, &content, None).await.0, StatusCode::NOT_FOUND);
    sqlx::query("INSERT INTO group_members (group_id,user_id) VALUES (?,?)").bind(group).bind(f.member.id).execute(&f.env.st.db).await.unwrap();
    sqlx::query("UPDATE grants SET expires_at = ? WHERE node_id = ? AND principal_type = 'group'")
        .bind(crate::util::now() - 1)
        .bind(&f.restricted)
        .execute(&f.env.st.db)
        .await
        .unwrap();
    assert_eq!(request(&f, &f.member_cookie, Method::GET, &content, None).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn links_on_restricted_items_stay_out_of_the_links_a_manager_can_manage() {
    let f = fixture().await;
    let hidden = json_call(&f, &f.own_cookie, Method::POST, "/api/shares", Some(json!({"node_id":f.secret}))).await;
    let shown = json_call(&f, &f.own_cookie, Method::POST, "/api/shares", Some(json!({"node_id":f.visible}))).await;
    restrict(&f).await;
    let managed = json_call(&f, &f.member_cookie, Method::GET, "/api/shares?scope=managed", None).await;
    assert!(!managed.to_string().contains(hidden["id"].as_str().unwrap()), "the hidden link's token was listed: {managed}");
    assert_hidden(&f, &managed);
    assert!(managed.to_string().contains(shown["id"].as_str().unwrap()), "links on items the manager sees stay listed");
}

#[tokio::test]
async fn the_access_dialog_keeps_a_restricted_folder_above_and_its_grants_from_someone_granted_below() {
    let f = fixture().await;
    let hr = f.env.user("hr", true).await;
    let inner = f.env.folder(&f.owner, &f.restricted, "Inner").await;
    f.env.grant(&f.restricted, &hr, "viewer").await;
    // A member of the space, with access inside the restricted folder only
    let viewer = f.env.user("viewer", true).await;
    f.env.grant(&f.root, &viewer, "viewer").await;
    f.env.grant(&inner, &viewer, "viewer").await;
    restrict(&f).await;
    let (_, cookie) = f.env.sign_in(&viewer, "Restriction regression").await;
    let access = json_call(&f, &cookie, Method::GET, &format!("/api/nodes/{inner}/access"), None).await;
    let text = access["inherited"].to_string();
    assert!(!text.contains("Restricted Payroll") && !text.contains(&f.restricted) && !text.contains("\"hr\""), "{text}");
    // The owner still sees who has access above
    let access = json_call(&f, &f.own_cookie, Method::GET, &format!("/api/nodes/{inner}/access"), None).await;
    assert!(access["inherited"].to_string().contains("Restricted Payroll"));
}

#[tokio::test]
async fn copies_and_moves_into_a_restricted_folder_keep_their_entries_from_whoever_cant_see_it() {
    let f = fixture().await;
    restrict(&f).await;
    let log = format!("/api/activity?drive_id={}", f.env.drive_of(&f.department).await);
    let plan = f.env.upload(&f.owner, &f.department, "plan.txt", b"plan").await;
    json_call(&f, &f.own_cookie, Method::POST, "/api/nodes/copy", Some(json!({"ids":[f.visible],"dest_id":f.restricted}))).await;
    json_call(&f, &f.own_cookie, Method::PATCH, &format!("/api/nodes/{plan}"), Some(json!({"name":"plan-renamed.txt"}))).await;
    let before = json_call(&f, &f.member_cookie, Method::GET, &log, None).await;
    assert!(before.to_string().contains("plan-renamed.txt"), "seen while it is outside: {before}");
    json_call(&f, &f.own_cookie, Method::POST, "/api/nodes/move", Some(json!({"ids":[plan],"dest_id":f.restricted}))).await;
    let after = json_call(&f, &f.member_cookie, Method::GET, &log, None).await;
    assert_hidden(&f, &after);
    assert!(!after.to_string().contains("plan-renamed.txt"), "an item moved into a restricted folder: {after}");
    // Nor in the history of the item copied, which shows details to managers
    let history = json_call(&f, &f.member_cookie, Method::GET, &format!("/api/nodes/{}/activity", f.visible), None).await;
    assert_hidden(&f, &history);
}

#[tokio::test]
async fn a_restore_never_puts_items_into_a_restricted_folder_the_person_cant_add_to() {
    let f = fixture().await;
    // Merging into a folder of the name: its restricted subfolder isn't gone into
    let projects = f.env.folder(&f.member, &f.department, "Projects").await;
    let payroll = f.env.folder(&f.member, &projects, "Payroll").await;
    f.env.upload(&f.member, &payroll, "x.txt", b"x").await;
    json_call(&f, &f.member_cookie, Method::POST, "/api/nodes/trash", Some(json!({"ids":[projects]}))).await;
    let again = f.env.folder(&f.owner, &f.department, "Projects").await;
    let hr_payroll = f.env.folder(&f.owner, &again, "Payroll").await;
    restrict_node(&f, &hr_payroll).await;
    assert_ne!(request(&f, &f.member_cookie, Method::POST, "/api/trash/restore", Some(json!({"ids":[projects]}))).await.0, StatusCode::OK);
    let mut c = f.env.st.db.acquire().await.unwrap();
    assert!(crate::paths::child_named(&mut c, &hr_payroll, "x.txt").await.unwrap().is_none());
    drop(c);

    // Making the way back: a restricted folder of the name isn't gone into either
    let boxed = f.env.folder(&f.member, &f.department, "Box").await;
    let y = f.env.upload(&f.member, &boxed, "y.txt", b"y").await;
    json_call(&f, &f.member_cookie, Method::POST, "/api/nodes/trash", Some(json!({"ids":[y]}))).await;
    json_call(&f, &f.member_cookie, Method::POST, "/api/nodes/trash", Some(json!({"ids":[boxed]}))).await;
    let other_box = f.env.folder(&f.owner, &f.department, "Box").await;
    restrict_node(&f, &other_box).await;
    assert_ne!(request(&f, &f.member_cookie, Method::POST, "/api/trash/restore", Some(json!({"ids":[y]}))).await.0, StatusCode::OK);
    let mut c = f.env.st.db.acquire().await.unwrap();
    assert!(crate::paths::child_named(&mut c, &other_box, "y.txt").await.unwrap().is_none());
}

#[tokio::test]
async fn a_restricted_folder_restored_onto_one_of_its_name_keeps_its_restriction() {
    let f = fixture().await;
    let hr = f.env.user("hr", true).await;
    f.env.grant(&f.restricted, &hr, "viewer").await;
    restrict(&f).await;
    json_call(&f, &f.own_cookie, Method::POST, "/api/nodes/trash", Some(json!({"ids":[f.restricted]}))).await;
    let open = f.env.folder(&f.owner, &f.department, "Restricted Payroll").await;
    json_call(&f, &f.own_cookie, Method::POST, "/api/trash/restore", Some(json!({"ids":[f.restricted]}))).await;
    let mut c = f.env.st.db.acquire().await.unwrap();
    let back = crate::tree::get_node(&mut c, &f.restricted).await.unwrap().expect("not merged away");
    assert!(back.restricted && back.trashed_at.is_none() && back.name != "Restricted Payroll", "{back:?}");
    let (grants,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM grants WHERE node_id = ?").bind(&f.restricted).fetch_one(&mut *c).await.unwrap();
    assert_eq!(grants, 1);
    assert!(crate::paths::child_named(&mut c, &open, "compensation-secret.txt").await.unwrap().is_none());
    drop(c);
    assert_eq!(request(&f, &f.member_cookie, Method::GET, &format!("/api/nodes/{}", f.secret), None).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_restricted_folder_moved_on_disk_where_the_scan_cant_follow_keeps_its_protection_or_is_reported() {
    let env = testutil::env().await;
    let space = env.folder_space("Archive").await;
    let ben = env.user("ben", true).await;
    // As on Windows and exFAT: no identities, so a move is seen as removed and added
    let _hidden = crate::fsops::testing::no_identities(&space.dir);
    testutil::write_old(&space.dir.join("Docs/Secret/s.txt"), b"s");
    std::fs::create_dir_all(space.dir.join("Other")).unwrap();
    crate::folders::scan(&env.st, &space.drive).await.unwrap();
    let (secret, _) = env.node_at(&space.drive, "Docs/Secret").await.unwrap();
    sqlx::query("UPDATE nodes SET restricted = 1 WHERE id = ?").bind(&secret).execute(&env.st.db).await.unwrap();
    env.grant(&secret, &ben, "viewer").await;
    let birth_known = std::fs::metadata(space.dir.join("Docs/Secret")).unwrap().created().is_ok();

    // Moved: the folder found with its name and creation time gets the restriction and the grants
    std::fs::rename(space.dir.join("Docs/Secret"), space.dir.join("Other/Secret")).unwrap();
    let report = crate::folders::scan(&env.st, &space.drive).await.unwrap();
    let (moved, _) = env.node_at(&space.drive, "Other/Secret").await.unwrap();
    let node = crate::tree::get_node(&mut env.st.db.acquire().await.unwrap(), &moved).await.unwrap().unwrap();
    if birth_known {
        assert!(node.restricted, "{report:?}");
        let (grants,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM grants WHERE node_id = ?").bind(&moved).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(grants, 1);
    } else {
        assert!(report.skipped.iter().any(|s| s.contains("restricted folder is gone")), "{report:?}");
    }

    // Copied then deleted (a new creation time): the scan report says so
    sqlx::query("UPDATE nodes SET restricted = 1 WHERE id = ?").bind(&moved).execute(&env.st.db).await.unwrap();
    testutil::write_old(&space.dir.join("Copy/Secret/s.txt"), b"s");
    std::fs::remove_dir_all(space.dir.join("Other/Secret")).unwrap();
    let report = crate::folders::scan(&env.st, &space.drive).await.unwrap();
    assert!(report.skipped.iter().any(|s| s.contains("Other/Secret") && s.contains("restricted folder is gone")), "{report:?}");
}

#[tokio::test]
async fn a_members_space_size_leaves_out_only_what_restricted_folders_hide_from_them() {
    let f = fixture().await;
    restrict(&f).await;
    // Inside the restricted folder: a part shared with the member, and in it a restricted folder again
    let open = f.env.folder(&f.owner, &f.restricted, "Open part").await;
    f.env.upload(&f.owner, &open, "open.txt", b"12345").await;
    f.env.grant(&open, &f.member, "viewer").await;
    let closed = f.env.folder(&f.owner, &open, "Closed").await;
    f.env.upload(&f.owner, &closed, "closed.txt", b"abc").await;
    restrict_node(&f, &closed).await;
    let used = |v: &Value| v.as_array().unwrap().iter().find(|d| d["root_id"] == f.root.as_str()).unwrap()["used_bytes"].as_i64().unwrap();
    let all = used(&json_call(&f, &f.own_cookie, Method::GET, "/api/drives", None).await);
    let seen = used(&json_call(&f, &f.member_cookie, Method::GET, "/api/drives", None).await);
    // Every file the member can see, one by one
    let sql = format!(
        "SELECT COALESCE(SUM(n.size), 0) FROM nodes n WHERE n.drive_id = (SELECT drive_id FROM nodes WHERE id = ?1) AND n.kind = 'file' AND {}",
        crate::tree::visible_sql(&f.member, "n.id")
    );
    let (one_by_one,): (i64,) = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(&f.root).fetch_one(&f.env.st.db).await.unwrap();
    assert_eq!(seen, one_by_one);
    assert_eq!(all - seen, ("confidential-payroll".len() + "abc".len()) as i64);
}

#[tokio::test]
async fn restricting_a_folder_leaves_records_about_other_spaces_alone() {
    let f = fixture().await;
    // The member's own file, in their own space, which they were told about and then deleted for good
    let mine = f.env.upload(&f.member, f.member.root(), "holiday-plans.txt", b"beach").await;
    let notice = crate::notify::Notice { kind: "shared", node_id: Some(mine.clone()), data: json!({"name":"holiday-plans.txt","item":"file"}) };
    let _ = crate::notify::add(&mut f.env.st.db.acquire().await.unwrap(), &[f.member.id], &notice).await.unwrap();
    json_call(&f, &f.member_cookie, Method::POST, "/api/nodes/trash", Some(json!({"ids":[mine]}))).await;
    let job = json_call(&f, &f.member_cookie, Method::POST, "/api/trash/delete", Some(json!({"ids":[mine]}))).await;
    if let Some(id) = job["id"].as_str().filter(|id| !id.is_empty()) {
        assert_eq!(crate::jobs::wait_for(&f.env.st, id).await.state, "done");
    }
    restrict(&f).await;
    let notifications = json_call(&f, &f.member_cookie, Method::GET, "/api/notifications", None).await;
    assert!(notifications.to_string().contains("holiday-plans.txt"), "{notifications}");
}
