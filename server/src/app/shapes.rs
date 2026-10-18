//! The shapes of the main API responses, recorded in `web/tests/contract/shapes.json`: the web app's types
//! (`web/src/api/types.ts`) are checked against them when it is type-checked (`web/tests/unit/apiShapes.test.ts`).
//! A change to what one of these responses holds fails this test until the file is written again
//! (`THIRTYFILE_UPDATE_SHAPES=1 cargo test api_shapes`), and then the web app's check until its types follow.

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use serde_json::{Map, Value, json};
use tower::ServiceExt;

use crate::testutil;

const SHAPES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/tests/contract/shapes.json");

/// What a response holds, without its values: each string, number and boolean the same, and a list by its first item
fn shape(v: &Value) -> Value {
    match v {
        Value::Null => Value::Null,
        Value::Bool(_) => json!(false),
        Value::Number(_) => json!(0),
        Value::String(_) => json!(""),
        Value::Array(items) => Value::Array(items.first().map(shape).into_iter().collect()),
        Value::Object(fields) => Value::Object(fields.iter().map(|(k, v)| (k.clone(), shape(v))).collect()),
    }
}

async fn call(app: &Router, cookie: &str, method: Method, uri: &str, body: Option<Value>) -> Value {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, cookie)
        .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from(([10, 0, 0, 1], 5000))));
    let req = match body {
        Some(b) => req.header(header::CONTENT_TYPE, "application/json").body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    };
    let res = app.clone().oneshot(req.unwrap()).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(status, StatusCode::OK, "{uri}: {}", String::from_utf8_lossy(&bytes));
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn api_shapes_match_the_recorded_ones() {
    let env = testutil::env().await;
    let app = super::routes::router(env.st.clone());
    let admin = env.admin().await;
    let amy = env.user("amy", true).await;
    let (_, me) = env.sign_in(&admin, "Test").await;
    let (_, amy_cookie) = env.sign_in(&amy, "Test").await;

    // A folder with a file, shared with Amy and by a link; a tag, a group, and a file in the trash
    let folder = env.folder(&admin, admin.root(), "Reports").await;
    let file = env.upload(&admin, &folder, "report.txt", b"Q3").await;
    let old = env.upload(&admin, &folder, "old.txt", b"old").await;
    env.grant(&folder, &amy, "editor").await;
    let mut shapes = Map::new();
    let mut record = |name: &str, v: &Value| {
        assert!(!v.is_null(), "{name}: nothing to record");
        shapes.insert(name.to_string(), shape(v));
    };
    record("ShareInfo", &call(&app, &me, Method::POST, "/api/shares", Some(json!({ "node_id": folder }))).await);
    record("Tag", &call(&app, &me, Method::POST, "/api/tags", Some(json!({ "name": "Draft", "color": "blue" }))).await);
    let _ = call(&app, &me, Method::POST, "/api/admin/groups", Some(json!({ "name": "Designers", "members": [amy.id] }))).await;
    let _ = call(&app, &me, Method::POST, "/api/nodes/trash", Some(json!({ "ids": [old] }))).await;

    record("Me", &call(&app, &me, Method::GET, "/api/auth/me", None).await);
    record("Drive", &call(&app, &me, Method::GET, "/api/drives", None).await[0]);
    record("NodeInfo", &call(&app, &me, Method::GET, &format!("/api/nodes/{file}"), None).await);
    record("Node", &call(&app, &me, Method::GET, &format!("/api/nodes/{folder}/children"), None).await[0]);
    let access = call(&app, &me, Method::GET, &format!("/api/nodes/{folder}/access"), None).await;
    record("Grant", &access["direct"][0]);
    record("AccessInfo", &access);
    record("Located", &call(&app, &me, Method::GET, "/api/search?q=report", None).await["items"][0]);
    record("SharedItem", &call(&app, &amy_cookie, Method::GET, "/api/shared-with-me", None).await[0]);
    record("Activity", &call(&app, &me, Method::GET, "/api/activity", None).await["items"][0]);
    record("HistoryEntry", &call(&app, &me, Method::GET, &format!("/api/nodes/{file}/activity"), None).await[0]);
    record("Principal", &call(&app, &me, Method::GET, "/api/directory?q=amy", None).await[0]);
    record("TwoFactorStatus", &call(&app, &me, Method::GET, "/api/auth/2fa", None).await);
    record("Device", &call(&app, &me, Method::GET, "/api/auth/sessions", None).await[0]);
    record("NotificationSettings", &call(&app, &me, Method::GET, "/api/notifications/settings", None).await);
    record("UserRow", &call(&app, &me, Method::GET, "/api/admin/users", None).await[0]);
    record("Group", &call(&app, &me, Method::GET, "/api/admin/groups", None).await[0]);
    record("SystemInfo", &call(&app, &me, Method::GET, "/api/admin/settings", None).await);
    record("EmailSettings", &call(&app, &me, Method::GET, "/api/admin/email", None).await);
    record("SsoSettings", &call(&app, &me, Method::GET, "/api/admin/sso", None).await);
    record("LogStatus", &call(&app, &me, Method::GET, "/api/admin/logs", None).await);
    let job = call(&app, &me, Method::POST, "/api/trash/delete", Some(json!({ "ids": [old] }))).await;
    let job = match &job["id"] {
        Value::String(id) => id.clone(),
        id => id.to_string(),
    };
    record("Job", &call(&app, &me, Method::GET, &format!("/api/jobs/{job}"), None).await);

    // Restricting a folder, and turning a member into a guest: what each would change
    record("RestrictionPreview", &call(&app, &me, Method::GET, &format!("/api/nodes/{folder}/restriction"), None).await);
    record("GuestConversionPreview", &call(&app, &me, Method::GET, &format!("/api/admin/users/{}/guest-conversion", amy.id), None).await);

    // The protocols: S3 with a bucket and a key, FTP with a receive folder, SFTP with a key, and an app password
    let password = testutil::password();
    // Published from a team space's folder
    let exchange = env.folder(&admin, &env.st.shared_root().unwrap(), "Exchange").await;
    let s3 = json!({ "enabled": true, "hostname": "s3.example.com", "region": "us-east-1", "managers_publish": false });
    record("S3Config", &call(&app, &me, Method::PUT, "/api/admin/s3", Some(s3)).await);
    let bucket = call(&app, &me, Method::POST, "/api/s3/buckets", Some(json!({ "node_id": exchange, "name": "reports", "password": password }))).await;
    record("NewS3Bucket", &bucket);
    let key = json!({ "description": "Backup", "writable": false, "expires_at": null, "password": password });
    let bucket_id = bucket["id"].as_str().unwrap();
    record("NewS3Key", &call(&app, &me, Method::POST, &format!("/api/s3/buckets/{bucket_id}/keys"), Some(key)).await);
    let buckets = call(&app, &me, Method::GET, &format!("/api/s3/buckets?node_id={exchange}"), None).await;
    record("S3Bucket", &buckets["buckets"][0]);
    record("S3Key", &buckets["buckets"][0]["keys"][0]);
    let ftp = json!({ "enabled": true, "managers_publish": false, "allow_private_plaintext": false });
    record("FtpStatus", &call(&app, &me, Method::PUT, "/api/admin/ftp", Some(ftp)).await);
    let receive =
        json!({ "node_id": exchange, "name": "Scanner", "enabled": true, "allow_list": false, "allow_read": false, "expires_at": null, "password": password });
    record("FtpCreated", &call(&app, &me, Method::POST, "/api/ftp/folders", Some(receive)).await);
    record("FtpFolder", &call(&app, &me, Method::GET, &format!("/api/ftp/folders?node_id={exchange}"), None).await["folders"][0]);
    record("SftpSettings", &call(&app, &me, Method::GET, "/api/admin/sftp", None).await);
    let private = russh::keys::PrivateKey::from(russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[7; 32]));
    let public = private.public_key().to_openssh().unwrap();
    record("SftpKey", &call(&app, &me, Method::POST, "/api/auth/sftp-keys", Some(json!({ "name": "Laptop", "public_key": public, "password": password }))).await);
    let app_password = json!({ "name": "Backup", "scope": "read", "password": password });
    record("NewAppPassword", &call(&app, &me, Method::POST, "/api/auth/app-passwords", Some(app_password)).await);
    record("AppPassword", &call(&app, &me, Method::GET, "/api/auth/app-passwords", None).await[0]);

    let now = serde_json::to_string_pretty(&Value::Object(shapes)).unwrap() + "\n";
    if std::env::var_os("THIRTYFILE_UPDATE_SHAPES").is_some() {
        std::fs::write(SHAPES, &now).unwrap();
        return;
    }
    let recorded = std::fs::read_to_string(SHAPES).unwrap_or_default().replace("\r\n", "\n");
    assert!(
        recorded == now,
        "The API's responses changed shape: run `THIRTYFILE_UPDATE_SHAPES=1 cargo test api_shapes`, then make web/src/api/types.ts follow (pnpm typecheck says where)"
    );
}
