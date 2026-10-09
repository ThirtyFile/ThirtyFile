//! WebDAV (RFC 4918, classes 1 and 2) at `/dav/`: ThirtyFile mapped as a drive on Windows and macOS, and used from
//! the Files apps of phones and from tools such as rclone.
//!
//! - `/dav/` shows the user's spaces as folders ("My files", the company space, team spaces) and "Shared with me"
//!   (items shared from spaces the user isn't a member of); below each is the folder tree. Names that clash get a
//!   number: "Projects (2)"
//! - Only app passwords sign in (HTTP Basic with the username and an app password, or Bearer), with the same limit on
//!   wrong attempts as the sign-in page. Cookies are ignored and never set. A read-only app password may only use GET,
//!   HEAD, OPTIONS and PROPFIND
//! - Every change goes through the same code as the web: permissions, read-only spaces, folder spaces, quotas, the
//!   upload size limit, name rules, the trash (DELETE) and the activity log
//! - Locks are only acknowledged: LOCK hands out a token and UNLOCK accepts any. That is what Windows Explorer, macOS
//!   Finder and Office need before they save; locks aren't enforced, so the last save wins
//! - PROPFIND answers Depth 0 and 1 (1 when the header is missing); Depth infinity is refused, as RFC 4918 allows
//! - PROPPATCH is answered as if it worked, but nothing is stored (clients use it to set times and Windows attributes)

mod methods;
mod paths;
mod props;
mod put;

use methods::*;
use paths::*;
use props::*;
use put::*;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

use axum::{
    Json,
    body::Body,
    extract::{self, Request, State},
    http::{HeaderMap, Method, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use quick_xml::{NsReader, escape::escape, events::Event, name::ResolveResult};
use serde_json::{Value, json};
use sqlx::SqliteConnection;
use tokio::io::AsyncWriteExt;

use crate::{
    auth::{self, User},
    content,
    error::{AppError, AppResult},
    files,
    jobs::{self, Limit, Outcome},
    logs, nodes,
    paths::{Found, SHARED, Target, child_named, resolve, tops},
    state::AppState,
    tree::{self, Need, Node},
    util::{new_id, now, numbered_name, validate_name},
};

/// Where WebDAV is served
pub const PREFIX: &str = "/dav";
/// Largest XML request body (PROPFIND, PROPPATCH, LOCK)
const MAX_XML: usize = 1024 * 1024;
/// Largest file a PUT may send when no upload size limit is set (sizes are summed as i64 for quotas)
const MAX_PUT: u64 = 1 << 50;
const ALLOW: &str = "OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, PROPPATCH, MKCOL, COPY, MOVE, LOCK, UNLOCK";
/// Characters left as they are in a path segment of an href; everything else is percent-encoded
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');
const DAV_NS: &str = "DAV:";

/// Every request to `/dav` and below
pub async fn handle(State(st): State<AppState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    // Clients ask what the server can do before signing in (Windows does)
    if parts.method == Method::OPTIONS {
        return options();
    }
    let user = match authenticate(&st, &parts).await {
        Ok(user) => user,
        Err(e) if e.status == StatusCode::UNAUTHORIZED => return challenge(),
        Err(e) => return fail(e),
    };
    let Some(segs) = segments(parts.uri.path()) else { return fail(AppError::not_found("Not found")) };
    let h = &parts.headers;
    let result = match parts.method.as_str() {
        "GET" | "HEAD" => get(&st, &user, &segs, h).await,
        "PROPFIND" => propfind(&st, &user, &segs, h, body).await,
        "PROPPATCH" => proppatch(&st, &user, &segs, body).await,
        "PUT" => put(&st, &user, &segs, h, body).await,
        "DELETE" => delete(&st, &user, &segs).await,
        "MKCOL" => mkcol(&st, &user, &segs, body).await,
        "MOVE" => transfer(&st, &user, &segs, h, true).await,
        "COPY" => transfer(&st, &user, &segs, h, false).await,
        "LOCK" => lock(&st, &user, &segs, h, body).await,
        // Locks aren't kept, so any token is accepted
        "UNLOCK" => Ok(StatusCode::NO_CONTENT.into_response()),
        _ => Ok((StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, ALLOW)]).into_response()),
    };
    let mut res = result.unwrap_or_else(fail);
    res.headers_mut().remove(header::SET_COOKIE);
    res
}

/// The site's root: Windows asks it whether the server speaks WebDAV (OPTIONS and PROPFIND /) before it opens `/dav/`
pub async fn server_root(st: State<AppState>, req: Request) -> Response {
    match *req.method() {
        Method::GET | Method::HEAD => crate::web::serve(st, req.uri().clone(), req.headers().clone()).await,
        Method::OPTIONS => options(),
        _ => (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "OPTIONS, GET, HEAD")]).into_response(),
    }
}

fn options() -> Response {
    (StatusCode::OK, [("dav", "1, 2"), ("allow", ALLOW), ("ms-author-via", "DAV"), ("content-length", "0")]).into_response()
}

/// Signs the request in with its app password (a missing or wrong one is answered with a sign-in challenge)
async fn authenticate(st: &AppState, parts: &Parts) -> AppResult<User> {
    let credential = auth::app_passwords::credential(&parts.headers).ok_or_else(AppError::unauthorized)?;
    auth::app_passwords::authenticate(parts, st, credential).await
}

fn challenge() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"ThirtyFile\", charset=\"UTF-8\""), (header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "Sign in with your username and an app password\n",
    )
        .into_response()
}

/// Errors as plain text: WebDAV clients show the status, not a JSON body
fn fail(e: AppError) -> Response {
    (e.status, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], format!("{}\n", e.message)).into_response()
}

fn not_found() -> AppError {
    AppError::not_found("Not found")
}

fn precondition(m: &str) -> AppError {
    AppError::new(StatusCode::PRECONDITION_FAILED, m)
}

// ───────────── Finding items ─────────────

/// The folder a new item at this path goes into, and the item's name
async fn parent_of(conn: &mut SqliteConnection, user: &User, segs: &[String]) -> AppResult<(Node, String)> {
    let Some((last, parent)) = segs.split_last() else { return Err(AppError::forbidden("Spaces can't be changed here")) };
    let found = resolve(conn, user, parent).await?.ok_or_else(|| AppError::conflict("The folder doesn't exist"))?;
    let folder = match found.target {
        Target::Node(n) if n.is_folder() => *n,
        Target::Node(_) => return Err(AppError::conflict("The folder doesn't exist")),
        Target::Root | Target::Shared => return Err(AppError::forbidden("Items can only be added inside a space or a shared folder")),
    };
    let name = validate_name(last)?;
    if name != *last {
        return Err(AppError::bad_request("Name can't start or end with a space"));
    }
    // Refused as a client expects for its temporary and system files (.DS_Store, Thumbs.db…) where it can't write them
    if folder.in_folder_space() {
        crate::fsops::check_name(&name).map_err(|e| AppError::forbidden(e.message))?;
    }
    Ok((folder, name))
}

async fn found_node(st: &AppState, user: &User, segs: &[String]) -> AppResult<Node> {
    match resolve(&mut *st.db.acquire().await?, user, segs).await?.ok_or_else(not_found)?.target {
        Target::Node(n) => Ok(*n),
        Target::Root | Target::Shared => Err(AppError::forbidden("This folder can't be changed")),
    }
}

/// Calls a web handler with a JSON request, so WebDAV changes go through the same checks and logging as the web
fn json_req<T: serde::de::DeserializeOwned>(v: Value) -> AppResult<Json<T>> {
    serde_json::from_value(v).map(Json).map_err(AppError::internal)
}

async fn rename(st: &AppState, user: &User, id: &str, name: &str) -> AppResult<()> {
    nodes::rename(State(st.clone()), user.clone(), extract::Path(id.to_string()), json_req(json!({ "name": name }))?).await.map(|_| ())
}

/// Moves an item into another folder, content included (a WebDAV request waits for all of it: see `transfer`)
async fn move_into(st: &AppState, user: &User, id: &str, dest: &str) -> AppResult<()> {
    let Json(req) = json_req(json!({ "ids": [id], "dest_id": dest }))?;
    if let Some(across) = nodes::move_items(st, user, &req).await? {
        nodes::run_content(st, user, across, &Default::default()).await?;
    }
    Ok(())
}

async fn trash(st: &AppState, user: &User, id: &str) -> AppResult<()> {
    nodes::trash(State(st.clone()), user.clone(), json_req(json!({ "ids": [id] }))?).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        testutil::{self, TestEnv},
        tokens,
    };
    use axum::{Router, extract::ConnectInfo};
    use tower::ServiceExt;

    async fn app_password(env: &TestEnv, user: &User, scope: &str) -> String {
        let req = serde_json::from_value(json!({ "name": "Drive", "scope": scope, "password": crate::testutil::password() })).unwrap();
        let addr = ConnectInfo(std::net::SocketAddr::from(([10, 0, 0, 9], 5000)));
        let Json(v) = tokens::create(State(env.st.clone()), user.clone(), addr, HeaderMap::new(), Json(req)).await.unwrap();
        v["token"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn an_idle_put_removes_its_temporary_file_and_keeps_the_existing_content() {
        use futures_util::StreamExt;
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let path = vec!["My files".to_string(), "idle.txt".to_string()];
        assert_eq!(put(&env.st, &amy, &path, &HeaderMap::new(), Body::from("original")).await.unwrap().status(), StatusCode::CREATED);
        let before: (String, String) = sqlx::query_as("SELECT id, blob_hash FROM nodes WHERE name = 'idle.txt'").fetch_one(&env.st.db).await.unwrap();
        let first = futures_util::stream::once(async { Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"partial")) });
        let (waiting, waited) = tokio::sync::oneshot::channel();
        let rest = futures_util::stream::once(async move {
            waiting.send(()).unwrap();
            std::future::pending::<Result<axum::body::Bytes, std::io::Error>>().await
        });
        let st = env.st.clone();
        let user = amy.clone();
        let request_path = path.clone();
        let request = tokio::spawn(async move { put(&st, &user, &request_path, &HeaderMap::new(), Body::from_stream(first.chain(rest))).await });
        waited.await.unwrap();
        tokio::time::pause();
        tokio::time::advance(crate::http_body::UPLOAD_IDLE + std::time::Duration::from_secs(1)).await;
        tokio::time::resume();
        assert_eq!(request.await.unwrap().unwrap_err().status, StatusCode::REQUEST_TIMEOUT);
        assert_eq!(std::fs::read_dir(env.st.tmp_dir()).unwrap().count(), 0);
        let after: (String, String) = sqlx::query_as("SELECT id, blob_hash FROM nodes WHERE name = 'idle.txt'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(before, after);
        assert_eq!(put(&env.st, &amy, &path, &HeaderMap::new(), Body::from("retry")).await.unwrap().status(), StatusCode::NO_CONTENT);
        let (hash,): (String,) = sqlx::query_as("SELECT blob_hash FROM nodes WHERE id = ?").bind(&before.0).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(hash, crate::util::sha256_hex(b"retry"));
    }

    struct Client {
        app: Router,
        auth: String,
    }

    struct Reply {
        status: StatusCode,
        headers: HeaderMap,
        body: String,
    }

    impl Client {
        async fn new(env: &TestEnv, user: &User, scope: &str) -> Client {
            let token = app_password(env, user, scope).await;
            let auth = format!("Basic {}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, format!("{}:{token}", user.username)));
            Client { app: crate::app::routes::router(env.st.clone()), auth }
        }

        async fn send(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Reply {
            let mut req = axum::http::Request::builder()
                .method(method)
                .uri(path)
                .header(header::AUTHORIZATION, &self.auth)
                .extension(ConnectInfo(std::net::SocketAddr::from(([10, 0, 0, 9], 5000))));
            for (k, v) in headers {
                req = req.header(*k, *v);
            }
            let res = self.app.clone().oneshot(req.body(Body::from(body.to_string())).unwrap()).await.unwrap();
            let (parts, body) = res.into_parts();
            let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
            Reply { status: parts.status, headers: parts.headers, body: String::from_utf8_lossy(&bytes).into_owned() }
        }

        async fn propfind(&self, path: &str, depth: &str) -> Reply {
            self.send("PROPFIND", path, &[("depth", depth)], "").await
        }
    }

    async fn activity(env: &TestEnv, user: &User) -> Vec<String> {
        sqlx::query_as::<_, (String,)>("SELECT action FROM activity WHERE user_id = ? ORDER BY id")
            .bind(user.id)
            .fetch_all(&env.st.db)
            .await
            .unwrap()
            .into_iter()
            .map(|(a,)| a)
            .collect()
    }

    #[tokio::test]
    async fn propfind_lists_spaces_shared_items_and_folders() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let amy = env.user("amy", true).await;
        let bob = env.user("bob", true).await;
        env.folder(&amy, amy.root(), "Docs").await;
        env.file(&amy, amy.root(), "notes.txt").await;
        // A team space whose name clashes with "My files", and a folder Bob shares with Amy
        let req = serde_json::from_value(json!({ "name": "My files" })).unwrap();
        let Json(team) = crate::drives::create(State(env.st.clone()), admin, Json(req)).await.unwrap();
        let team = serde_json::to_value(&team).unwrap();
        env.grant(team["root_id"].as_str().unwrap(), &amy, "editor").await;
        let plans = env.folder(&bob, bob.root(), "Plans & ideas").await;
        env.grant(&plans, &amy, "viewer").await;

        let dav = Client::new(&env, &amy, "read").await;
        let res = dav.propfind("/dav/", "1").await;
        assert_eq!(res.status, StatusCode::MULTI_STATUS);
        assert!(res.headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("application/xml"));
        for href in [
            "<D:href>/dav/</D:href>",
            "<D:href>/dav/My%20files/</D:href>",
            "<D:href>/dav/My%20files%20%282%29/</D:href>",
            "<D:href>/dav/All%20files/</D:href>",
            "<D:href>/dav/Shared%20with%20me/</D:href>",
        ] {
            assert!(res.body.contains(href), "{href} in {}", res.body);
        }
        assert!(res.body.contains("<D:displayname>My files (2)</D:displayname>"));

        let res = dav.propfind("/dav/My%20files", "1").await;
        assert_eq!(res.status, StatusCode::MULTI_STATUS);
        assert!(res.body.contains("<D:href>/dav/My%20files/Docs/</D:href>"));
        assert!(res.body.contains("<D:href>/dav/My%20files/notes.txt</D:href>"));
        assert!(res.body.contains("<D:resourcetype><D:collection/></D:resourcetype>"));
        assert!(res.body.contains("<D:getcontentlength>0</D:getcontentlength>"));
        assert!(res.body.contains(" GMT</D:getlastmodified>"));
        // Depth 0: the folder alone
        let res = dav.propfind("/dav/my%20FILES/", "0").await;
        assert!(res.body.contains("/dav/My%20files/</D:href>") && !res.body.contains("Docs"));
        assert_eq!(dav.propfind("/dav/My%20files/", "infinity").await.status, StatusCode::FORBIDDEN);

        let res = dav.propfind("/dav/Shared%20with%20me/", "1").await;
        assert!(res.body.contains("<D:href>/dav/Shared%20with%20me/Plans%20%26%20ideas/</D:href>"), "{}", res.body);
        assert!(res.body.contains("<D:displayname>Plans &amp; ideas</D:displayname>"));
        assert_eq!(dav.propfind("/dav/Shared%20with%20me/Plans%20%26%20ideas/", "1").await.status, StatusCode::MULTI_STATUS);

        // Named properties: the ones there are, and the unknown ones as not found
        let body = r#"<?xml version="1.0"?><propfind xmlns="DAV:" xmlns:W="urn:schemas-microsoft-com:"><prop><getcontentlength/><W:Win32FileAttributes/></prop></propfind>"#;
        let res = dav.send("PROPFIND", "/dav/My%20files/notes.txt", &[("depth", "0")], body).await;
        assert!(res.body.contains("<D:getcontentlength>0</D:getcontentlength>") && !res.body.contains("displayname"), "{}", res.body);
        assert!(res.body.contains(r#"<R:Win32FileAttributes xmlns:R="urn:schemas-microsoft-com:"/></D:prop><D:status>HTTP/1.1 404 Not Found"#));
    }

    #[tokio::test]
    async fn only_what_the_user_can_reach_is_found() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let bob = env.user("bob", true).await;
        let secret = env.folder(&bob, bob.root(), "Secret").await;
        let bobs = Client::new(&env, &bob, "write").await;
        assert_eq!(bobs.send("PUT", "/dav/My%20files/Secret/plan.txt", &[], "plan").await.status, StatusCode::CREATED);
        let dav = Client::new(&env, &amy, "write").await;
        assert!(!dav.propfind("/dav/", "1").await.body.contains("Secret"));
        for path in ["/dav/Secret/", "/dav/My%20files/Secret/", "/dav/Shared%20with%20me/Secret/", "/dav/Shared%20with%20me/Secret/plan.txt", "/dav/My%20files/../x"]
        {
            assert_eq!(dav.propfind(path, "0").await.status, StatusCode::NOT_FOUND, "{path}");
        }
        assert_eq!(dav.send("GET", "/dav/Shared%20with%20me/Secret/plan.txt", &[], "").await.status, StatusCode::NOT_FOUND);
        assert_eq!(dav.send("PUT", "/dav/Shared%20with%20me/Secret/new.txt", &[], "x").await.status, StatusCode::CONFLICT);
        // Once shared, it's there
        env.grant(&secret, &amy, "viewer").await;
        assert_eq!(dav.send("GET", "/dav/Shared%20with%20me/Secret/plan.txt", &[], "").await.status, StatusCode::OK);
        // Viewers can't change it
        assert_eq!(dav.send("PUT", "/dav/Shared%20with%20me/Secret/new.txt", &[], "x").await.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn put_and_get_round_trip() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let dav = Client::new(&env, &amy, "write").await;
        let res = dav.send("PUT", "/dav/My%20files/hello%20world.txt", &[], "Hello, WebDAV!").await;
        assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
        let res = dav.send("GET", "/dav/My%20files/hello%20world.txt", &[], "").await;
        assert_eq!((res.status, res.body.as_str()), (StatusCode::OK, "Hello, WebDAV!"));
        let res = dav.send("GET", "/dav/My%20files/hello%20world.txt", &[("range", "bytes=7-12")], "").await;
        assert_eq!((res.status, res.body.as_str()), (StatusCode::PARTIAL_CONTENT, "WebDAV"));
        assert!(!res.headers.contains_key(header::SET_COOKIE));

        // Replacing keeps the file and its id
        let (id,): (String,) = sqlx::query_as("SELECT id FROM nodes WHERE name = 'hello world.txt'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(dav.send("PUT", "/dav/My%20files/hello%20world.txt", &[], "Changed").await.status, StatusCode::NO_CONTENT);
        assert_eq!(dav.send("GET", "/dav/My%20files/hello%20world.txt", &[], "").await.body, "Changed");
        let (again, size): (String, i64) = sqlx::query_as("SELECT id, size FROM nodes WHERE name = 'hello world.txt'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!((again, size), (id, 7));
        assert_eq!(tree::used_bytes(&env.st.db, amy.id).await.unwrap(), 7);
        // The same content again changes nothing
        assert_eq!(dav.send("PUT", "/dav/My%20files/hello%20world.txt", &[], "Changed").await.status, StatusCode::NO_CONTENT);
        assert_eq!(activity(&env, &amy).await, ["upload", "edit"]);

        // Name rules, quota and the upload size limit
        assert_eq!(dav.send("PUT", "/dav/My%20files/a%3Ab.txt", &[], "x").await.status, StatusCode::BAD_REQUEST);
        assert_eq!(dav.send("PUT", "/dav/My%20files/Missing/a.txt", &[], "x").await.status, StatusCode::CONFLICT);
        assert_eq!(dav.send("PUT", "/dav/My%20files", &[], "x").await.status, StatusCode::FORBIDDEN);
        sqlx::query("UPDATE users SET quota_bytes = 10 WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
        assert_eq!(dav.send("PUT", "/dav/My%20files/big.txt", &[], "0123456789").await.status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(dav.send("PUT", "/dav/My%20files/small.txt", &[], "012").await.status, StatusCode::CREATED);
    }

    #[tokio::test]
    async fn a_file_sent_without_its_length_stops_where_the_space_ends() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let dav = Client::new(&env, &amy, "write").await;
        sqlx::query("UPDATE users SET quota_bytes = 1000000 WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
        // A body that never ends, sent in chunks (no Content-Length)
        let endless = futures_util::stream::repeat_with(|| Ok::<_, std::io::Error>(axum::body::Bytes::from_static(&[7u8; 65536])));
        let req = axum::http::Request::builder()
            .method("PUT")
            .uri("/dav/My%20files/endless.bin")
            .header(header::AUTHORIZATION, &dav.auth)
            .extension(ConnectInfo(std::net::SocketAddr::from(([10, 0, 0, 9], 5000))))
            .body(Body::from_stream(endless))
            .unwrap();
        let res = tokio::time::timeout(std::time::Duration::from_secs(30), dav.app.clone().oneshot(req)).await.expect("the server kept receiving");
        assert_eq!(res.unwrap().status(), StatusCode::PAYLOAD_TOO_LARGE);
        let left: Vec<_> = std::fs::read_dir(env.st.tmp_dir()).unwrap().flatten().collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[tokio::test]
    async fn mkcol_creates_folders() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let dav = Client::new(&env, &amy, "write").await;
        assert_eq!(dav.send("MKCOL", "/dav/My%20files/Projects", &[], "").await.status, StatusCode::CREATED);
        assert_eq!(dav.send("MKCOL", "/dav/My%20files/projects/", &[], "").await.status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(dav.send("MKCOL", "/dav/My%20files/Projects/2026/", &[], "").await.status, StatusCode::CREATED);
        assert_eq!(dav.send("MKCOL", "/dav/My%20files/Nope/2026/", &[], "").await.status, StatusCode::CONFLICT);
        assert_eq!(dav.send("MKCOL", "/dav/New%20space/", &[], "").await.status, StatusCode::FORBIDDEN);
        assert!(dav.propfind("/dav/My%20files/Projects/", "1").await.body.contains("<D:href>/dav/My%20files/Projects/2026/</D:href>"));
        assert_eq!(activity(&env, &amy).await, ["create_folder", "create_folder"]);
    }

    #[tokio::test]
    async fn move_and_copy_with_overwrite() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let dav = Client::new(&env, &amy, "write").await;
        for (path, body) in [("/dav/My%20files/a.txt", "A"), ("/dav/My%20files/b.txt", "B")] {
            assert_eq!(dav.send("PUT", path, &[], body).await.status, StatusCode::CREATED);
        }
        dav.send("MKCOL", "/dav/My%20files/Sub", &[], "").await;
        let to = |dest: &str, overwrite: &str| [("destination", format!("http://localhost{dest}")), ("overwrite", overwrite.to_string())];
        let send = async |method: &str, path: &str, headers: [(&str, String); 2]| {
            let h: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
            dav.send(method, path, &h, "").await.status
        };
        assert_eq!(send("MOVE", "/dav/My%20files/a.txt", to("/dav/My%20files/b.txt", "F")).await, StatusCode::PRECONDITION_FAILED);
        assert_eq!(send("MOVE", "/dav/My%20files/a.txt", to("/dav/My%20files/b.txt", "T")).await, StatusCode::NO_CONTENT);
        assert_eq!(dav.send("GET", "/dav/My%20files/b.txt", &[], "").await.body, "A");
        assert_eq!(dav.send("GET", "/dav/My%20files/a.txt", &[], "").await.status, StatusCode::NOT_FOUND);
        // The file that was replaced is in the trash
        let (trashed,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE name = 'b.txt' AND trashed_at IS NOT NULL").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(trashed, 1);

        // Into another folder under a new name; a copy under a new name; a rename that only changes letter case
        assert_eq!(send("MOVE", "/dav/My%20files/b.txt", to("/dav/My%20files/Sub/c.txt", "T")).await, StatusCode::CREATED);
        assert_eq!(send("COPY", "/dav/My%20files/Sub/c.txt", to("/dav/My%20files/Sub/d.txt", "F")).await, StatusCode::CREATED);
        assert_eq!(send("COPY", "/dav/My%20files/Sub/c.txt", to("/dav/My%20files/Sub/C.txt", "F")).await, StatusCode::FORBIDDEN);
        assert_eq!(send("MOVE", "/dav/My%20files/Sub/c.txt", to("/dav/My%20files/Sub/C.txt", "F")).await, StatusCode::CREATED);
        assert_eq!(dav.send("GET", "/dav/My%20files/Sub/d.txt", &[], "").await.body, "A");
        let listing = dav.propfind("/dav/My%20files/Sub/", "1").await.body;
        assert!(listing.contains("/Sub/C.txt<") && listing.contains("/Sub/d.txt<"), "{listing}");
        assert_eq!(send("MOVE", "/dav/My%20files/Sub", to("/dav/My%20files/Sub/Inner", "T")).await, StatusCode::FORBIDDEN);
        assert_eq!(send("MOVE", "/dav/My%20files/Sub", to("https://elsewhere.example/x/Sub", "T")).await, StatusCode::BAD_GATEWAY);
        let actions = activity(&env, &amy).await;
        assert!(actions.contains(&"move".to_string()) && actions.contains(&"copy".to_string()) && actions.contains(&"rename".to_string()), "{actions:?}");
    }

    #[tokio::test]
    async fn delete_moves_to_the_trash() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let dav = Client::new(&env, &amy, "write").await;
        dav.send("MKCOL", "/dav/My%20files/Old", &[], "").await;
        dav.send("PUT", "/dav/My%20files/Old/x.txt", &[], "x").await;
        assert_eq!(dav.send("DELETE", "/dav/My%20files/Old/", &[], "").await.status, StatusCode::NO_CONTENT);
        assert_eq!(dav.propfind("/dav/My%20files/Old/", "0").await.status, StatusCode::NOT_FOUND);
        let (trashed,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE trashed_at IS NOT NULL AND name IN ('Old', 'x.txt')").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(trashed, 2);
        assert!(activity(&env, &amy).await.contains(&"trash".to_string()));
        assert_eq!(dav.send("DELETE", "/dav/My%20files/", &[], "").await.status, StatusCode::FORBIDDEN);
        assert_eq!(dav.send("DELETE", "/dav/", &[], "").await.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn read_only_app_passwords_only_read() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        env.file(&amy, amy.root(), "a.txt").await;
        let dav = Client::new(&env, &amy, "read").await;
        assert_eq!(dav.propfind("/dav/My%20files/", "1").await.status, StatusCode::MULTI_STATUS);
        assert_eq!(dav.send("OPTIONS", "/dav/", &[], "").await.status, StatusCode::OK);
        let dest = [("destination", "/dav/My%20files/b.txt")];
        type Case<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);
        let cases: [Case; 7] = [
            ("PUT", "/dav/My%20files/new.txt", &[]),
            ("MKCOL", "/dav/My%20files/New", &[]),
            ("DELETE", "/dav/My%20files/a.txt", &[]),
            ("MOVE", "/dav/My%20files/a.txt", &dest),
            ("COPY", "/dav/My%20files/a.txt", &dest),
            ("PROPPATCH", "/dav/My%20files/a.txt", &[]),
            ("LOCK", "/dav/My%20files/a.txt", &[]),
        ];
        for (method, path, headers) in cases {
            assert_eq!(dav.send(method, path, headers, "").await.status, StatusCode::FORBIDDEN, "{method}");
        }
        assert!(activity(&env, &amy).await.is_empty());
    }

    #[tokio::test]
    async fn only_app_passwords_sign_in() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let app = crate::app::routes::router(env.st.clone());
        let (_, cookie) = env.sign_in(&amy, "Test").await;
        let send = async |headers: &[(header::HeaderName, String)]| {
            let mut req = axum::http::Request::builder().method("PROPFIND").uri("/dav/").extension(ConnectInfo(std::net::SocketAddr::from(([10, 0, 0, 9], 5000))));
            for (k, v) in headers {
                req = req.header(k, v);
            }
            app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap()
        };
        // A browser session isn't enough, and neither is the account's own password
        for headers in [
            vec![],
            vec![(header::COOKIE, cookie)],
            vec![(
                header::AUTHORIZATION,
                format!("Basic {}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, format!("amy:{}", testutil::password()))),
            )],
        ] {
            let res = send(&headers).await;
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(res.headers()[header::WWW_AUTHENTICATE], "Basic realm=\"ThirtyFile\", charset=\"UTF-8\"");
        }
        // A page on another site can't use credentials the browser remembers
        let dav = Client::new(&env, &amy, "write").await;
        let res = dav.send("PUT", "/dav/My%20files/x.txt", &[("origin", "https://elsewhere.example")], "x").await;
        assert_eq!(res.status, StatusCode::FORBIDDEN);
        let res = dav.send("OPTIONS", "/dav/", &[], "").await;
        assert_eq!(res.headers["dav"], "1, 2");
        assert!(res.headers["allow"].to_str().unwrap().contains("PROPFIND"));
        // Windows asks the site's root first
        assert_eq!(dav.send("OPTIONS", "/", &[], "").await.headers["dav"], "1, 2");
        assert_eq!(dav.send("PROPFIND", "/", &[("depth", "0")], "").await.status, StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn locks_and_property_changes_are_acknowledged() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let dav = Client::new(&env, &amy, "write").await;
        let body = r#"<?xml version="1.0" encoding="utf-8"?><D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype><D:owner><D:href>Amy</D:href></D:owner></D:lockinfo>"#;
        // A new path becomes an empty file
        let res = dav.send("LOCK", "/dav/My%20files/Report.docx", &[("timeout", "Second-600")], body).await;
        assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
        let token = res.headers["lock-token"].to_str().unwrap().to_string();
        assert!(token.starts_with("<opaquelocktoken:") && res.body.contains(&token[1..token.len() - 1]));
        assert!(res.body.contains("<D:owner><D:href>Amy</D:href></D:owner>") && res.body.contains("Second-600"));
        assert_eq!(dav.send("GET", "/dav/My%20files/Report.docx", &[], "").await.body, "");
        // A refresh keeps the token
        let res = dav.send("LOCK", "/dav/My%20files/Report.docx", &[("if", &format!("({token})"))], "").await;
        assert_eq!(res.status, StatusCode::OK);
        assert!(res.body.contains(&token[1..token.len() - 1]));
        // Saving while locked, then unlocking
        assert_eq!(dav.send("PUT", "/dav/My%20files/Report.docx", &[("if", &format!("({token})"))], "content").await.status, StatusCode::NO_CONTENT);
        assert_eq!(dav.send("UNLOCK", "/dav/My%20files/Report.docx", &[("lock-token", &token)], "").await.status, StatusCode::NO_CONTENT);

        let body = r#"<?xml version="1.0"?><D:propertyupdate xmlns:D="DAV:" xmlns:Z="urn:schemas-microsoft-com:"><D:set><D:prop><Z:Win32LastModifiedTime>Wed, 01 Jan 2026 00:00:00 GMT</Z:Win32LastModifiedTime></D:prop></D:set></D:propertyupdate>"#;
        let res = dav.send("PROPPATCH", "/dav/My%20files/Report.docx", &[], body).await;
        assert_eq!(res.status, StatusCode::MULTI_STATUS);
        assert!(res.body.contains(r#"<R:Win32LastModifiedTime xmlns:R="urn:schemas-microsoft-com:"/></D:prop><D:status>HTTP/1.1 200 OK"#), "{}", res.body);
        assert_eq!(dav.send("PROPPATCH", "/dav/My%20files/Nothing.docx", &[], body).await.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_long_copy_or_move_is_accepted_and_finishes_in_the_background() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let space = env.folder_space("Server").await;
        testutil::write_old(&space.dir.join("Photos/a.txt"), b"a");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let dav = Client::new(&env, &admin, "write").await;
        // Copying takes longer than the client is kept waiting: accepted, and done meanwhile
        let short = crate::jobs::short_wait();
        let go = std::sync::Arc::new(tokio::sync::Notify::new());
        let hook = crate::fsops::hook_after_place(crate::fsops::wait_for(&go));
        let h = [("destination", "/dav/Server/Copied")];
        assert_eq!(dav.send("COPY", "/dav/Server/Photos", &h, "").await.status, StatusCode::ACCEPTED);
        go.notify_one();
        let mut done = false;
        for _ in 0..200 {
            if env.node_at(&space.drive, "Copied/a.txt").await.is_some() {
                done = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(done, "the copy got its name");
        assert_eq!(std::fs::read(space.dir.join("Copied/a.txt")).unwrap(), b"a");
        // Quick ones answer as always
        drop((hook, short));
        let h = [("destination", "/dav/My%20files/Photos")];
        assert_eq!(dav.send("MOVE", "/dav/Server/Photos", &h, "").await.status, StatusCode::CREATED);
        assert!(env.node_at(&space.drive, "Photos").await.is_none());
    }

    #[tokio::test]
    async fn folder_spaces_and_read_only_spaces_behave_as_on_the_web() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let space = env.folder_space("Server").await;
        let dav = Client::new(&env, &admin, "write").await;
        assert_eq!(dav.send("MKCOL", "/dav/Server/Photos", &[], "").await.status, StatusCode::CREATED);
        assert_eq!(dav.send("PUT", "/dav/Server/Photos/a.txt", &[], "on disk").await.status, StatusCode::CREATED);
        assert_eq!(std::fs::read_to_string(space.dir.join("Photos/a.txt")).unwrap(), "on disk");
        assert_eq!(dav.send("PUT", "/dav/Server/Photos/a.txt", &[], "replaced").await.status, StatusCode::NO_CONTENT);
        assert_eq!(std::fs::read_to_string(space.dir.join("Photos/a.txt")).unwrap(), "replaced");
        assert_eq!(dav.send("GET", "/dav/Server/Photos/a.txt", &[], "").await.body, "replaced");
        assert_eq!(env.node_at(&space.drive, "Photos/a.txt").await.unwrap().1, 8);
        // A file put there on the server shows up
        testutil::write_old(&space.dir.join("Photos/b.txt"), b"b");
        assert!(dav.propfind("/dav/Server/Photos/", "1").await.body.contains("/dav/Server/Photos/b.txt<"));
        let h = [("destination", "/dav/Server/Photos/c.txt")];
        assert_eq!(dav.send("MOVE", "/dav/Server/Photos/b.txt", &h, "").await.status, StatusCode::CREATED);
        assert!(space.dir.join("Photos/c.txt").exists() && !space.dir.join("Photos/b.txt").exists());
        assert_eq!(dav.send("DELETE", "/dav/Server/Photos/c.txt", &[], "").await.status, StatusCode::NO_CONTENT);
        assert!(!space.dir.join("Photos/c.txt").exists());
        // Names the folder can't show are refused as a client expects for its system files
        assert_eq!(dav.send("PUT", "/dav/Server/Photos/.DS_Store", &[], "finder").await.status, StatusCode::FORBIDDEN);
        assert_eq!(dav.send("MKCOL", "/dav/Server/.thirtyfile-trash", &[], "").await.status, StatusCode::FORBIDDEN);
        let h = [("destination", "/dav/Server/Photos/.thirtyfile-save-x")];
        assert_eq!(dav.send("MOVE", "/dav/Server/Photos/a.txt", &h, "").await.status, StatusCode::FORBIDDEN);
        assert!(!space.dir.join("Photos/.DS_Store").exists() && env.node_at(&space.drive, ".thirtyfile-trash").await.is_none());
        assert!(space.dir.join("Photos/a.txt").exists());

        sqlx::query("UPDATE drives SET read_only = 1 WHERE id = ?").bind(&space.drive).execute(&env.st.db).await.unwrap();
        assert_eq!(dav.send("PUT", "/dav/Server/Photos/a.txt", &[], "no").await.status, StatusCode::FORBIDDEN);
        assert_eq!(dav.send("MKCOL", "/dav/Server/New", &[], "").await.status, StatusCode::FORBIDDEN);
        assert_eq!(dav.send("DELETE", "/dav/Server/Photos/a.txt", &[], "").await.status, StatusCode::FORBIDDEN);
        assert_eq!(dav.send("GET", "/dav/Server/Photos/a.txt", &[], "").await.body, "replaced");
    }

    #[test]
    fn dates_and_paths() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(784_111_777), "1994-11-06T08:49:37Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(http_date(784_111_777), "Sun, 06 Nov 1994 08:49:37 GMT");
        // Times a disk or a backup gave that dates can't be written with are kept within 1970 to 9999
        assert_eq!(http_date(i64::MAX), "Fri, 31 Dec 9999 23:59:59 GMT");
        assert_eq!(rfc3339(i64::MAX), "9999-12-31T23:59:59Z");
        assert_eq!((http_date(-5), rfc3339(i64::MIN)), ("Thu, 01 Jan 1970 00:00:00 GMT".into(), "1970-01-01T00:00:00Z".into()));
        assert_eq!(segments("/dav/My%20files//a%2Bb/"), Some(vec!["My files".to_string(), "a+b".to_string()]));
        assert_eq!(segments("/dav"), Some(vec![]));
        assert_eq!(segments("/davx/a"), None);
        assert_eq!(segments("/dav/a/%2e%2e/b"), None);
        assert_eq!(segments("/dav/a%2Fb"), None);
        assert_eq!(href(&["My files".into(), "Ünïcode & co.txt".into()], false), "/dav/My%20files/%C3%9Cn%C3%AFcode%20%26%20co.txt");
        assert_eq!(parse(b"").unwrap().want(), Want::All);
        assert_eq!(parse(br#"<D:propfind xmlns:D="DAV:"><D:propname/></D:propfind>"#).unwrap().want(), Want::Names);
        assert!(parse(b"<not xml").is_err() || parse(b"<a><b></a>").is_err());
    }
}
