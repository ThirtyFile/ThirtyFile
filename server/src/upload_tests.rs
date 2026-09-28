//! More upload tests: resuming, wrong offsets and sizes, folders that move or fill up while an upload runs, and uploaded
//! folder trees. They go through the tus handlers (create, head, patch) only.

use tokio::io::AsyncReadExt;

use super::*;
use crate::testutil;

fn b64(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(s)
}

/// Starts an upload of `len` bytes into `parent` (under `rel`, as a dropped folder would) and returns its id
async fn start(env: &testutil::TestEnv, user: &User, parent: &str, rel: &str, name: &str, len: usize) -> AppResult<String> {
    let mut h = HeaderMap::new();
    h.insert("upload-length", len.to_string().parse().unwrap());
    let meta = format!("filename {},parentId {},relativePath {},batchId {}", b64(name), b64(parent), b64(rel), b64("batch-1"));
    h.insert("upload-metadata", meta.parse().unwrap());
    let res = create(State(env.st.clone()), user.clone(), h).await?;
    Ok(res.headers()[header::LOCATION].to_str().unwrap().rsplit('/').next().unwrap().to_string())
}

async fn send(env: &testutil::TestEnv, user: &User, id: &str, offset: usize, data: &'static [u8]) -> AppResult<Response> {
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, "application/offset+octet-stream".parse().unwrap());
    h.insert("upload-offset", offset.to_string().parse().unwrap());
    patch(State(env.st.clone()), user.clone(), Path(id.to_string()), h, Body::from(data)).await
}

async fn offset_of(env: &testutil::TestEnv, user: &User, id: &str) -> String {
    let res = head(State(env.st.clone()), user.clone(), Path(id.to_string())).await.unwrap();
    res.headers()["upload-offset"].to_str().unwrap().to_string()
}

/// The file an upload became: its content, name and folder
async fn file(env: &testutil::TestEnv, res: &Response) -> (Vec<u8>, String, String) {
    let id = res.headers()["x-node-id"].to_str().unwrap();
    let node = tree::get_node(&mut env.st.db.acquire().await.unwrap(), id).await.unwrap().unwrap();
    let mut data = Vec::new();
    let mut r = crate::files::Source::of(&node).unwrap().open(&env.st, 0, node.size as u64).await.unwrap();
    r.read_to_end(&mut data).await.unwrap();
    (data, node.name, node.parent_id.unwrap())
}

/// Moves an item as the person would, into `dest`
async fn move_to(env: &testutil::TestEnv, user: &User, id: &str, dest: &str) {
    let req = serde_json::from_value(serde_json::json!({ "ids": [id], "dest_id": dest })).unwrap();
    let _ = crate::nodes::move_nodes(State(env.st.clone()), user.clone(), axum::Json(req)).await.unwrap();
}

async fn used(env: &testutil::TestEnv, root: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as("SELECT used_bytes FROM drives WHERE root_id = ?").bind(root).fetch_one(&env.st.db).await.unwrap();
    n
}

#[tokio::test]
async fn an_interrupted_upload_resumes_where_it_stopped() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let id = start(&env, &amy, "root", "", "notes.txt", 11).await.unwrap();
    assert_eq!(offset_of(&env, &amy, &id).await, "0");

    // The first part arrives; the file doesn't exist until everything has
    let res = send(&env, &amy, &id, 0, b"hello").await.unwrap();
    assert_eq!(res.headers()["upload-offset"], "5");
    assert!(!res.headers().contains_key("x-node-id"));
    assert_eq!(offset_of(&env, &amy, &id).await, "5");

    // A client that sends from another offset is told so, and nothing is written
    for wrong in [0, 3, 6] {
        let err = send(&env, &amy, &id, wrong, b" world").await.unwrap_err();
        assert_eq!(err.status, StatusCode::CONFLICT, "offset {wrong}");
    }
    assert_eq!(offset_of(&env, &amy, &id).await, "5");

    let res = send(&env, &amy, &id, 5, b" world").await.unwrap();
    assert_eq!(res.headers()["upload-offset"], "11");
    assert_eq!(file(&env, &res).await.0, b"hello world");
    assert_eq!(used(&env, &amy.root_id).await, 11);
}

#[tokio::test]
async fn requests_that_dont_follow_the_protocol_change_nothing() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let ben = env.user("ben", true).await;
    let id = start(&env, &amy, "root", "", "a.txt", 5).await.unwrap();

    // More data than the declared length
    let err = send(&env, &amy, &id, 0, b"hello!").await.unwrap_err();
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert_eq!(offset_of(&env, &amy, &id).await, "0");
    // Another content type, or no offset
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, "text/plain".parse().unwrap());
    h.insert("upload-offset", "0".parse().unwrap());
    let err = patch(State(env.st.clone()), amy.clone(), Path(id.clone()), h, Body::from("hello")).await.unwrap_err();
    assert_eq!(err.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, "application/offset+octet-stream".parse().unwrap());
    let err = patch(State(env.st.clone()), amy.clone(), Path(id.clone()), h, Body::from("hello")).await.unwrap_err();
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    // Someone else's upload doesn't exist for them
    assert_eq!(send(&env, &ben, &id, 0, b"hello").await.unwrap_err().status, StatusCode::NOT_FOUND);
    assert_eq!(head(State(env.st.clone()), ben.clone(), Path(id.clone())).await.unwrap_err().status, StatusCode::NOT_FOUND);

    // Still waiting for the right data
    let res = send(&env, &amy, &id, 0, b"hello").await.unwrap();
    assert_eq!(file(&env, &res).await.0, b"hello");
    // No length, or an absurd one, is refused before anything is recorded
    let mut h = HeaderMap::new();
    h.insert("upload-metadata", format!("filename {}", b64("b.txt")).parse().unwrap());
    assert_eq!(create(State(env.st.clone()), amy.clone(), h.clone()).await.unwrap_err().status, StatusCode::BAD_REQUEST);
    h.insert("upload-length", (MAX_UPLOAD_LENGTH + 1).to_string().parse().unwrap());
    assert_eq!(create(State(env.st.clone()), amy.clone(), h).await.unwrap_err().status, StatusCode::PAYLOAD_TOO_LARGE);
    let (rows,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM uploads WHERE node_id IS NULL").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(rows, 0);
}

#[tokio::test]
async fn received_data_of_the_wrong_size_doesnt_become_a_file() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    // Everything seems to have arrived, but the data kept is longer than the upload (a damaged temporary file)
    let id = start(&env, &amy, "root", "", "odd.txt", 5).await.unwrap();
    tokio::fs::write(upload_path(&env.st, &id), b"hello, world").await.unwrap();
    sqlx::query("UPDATE uploads SET offset = 5 WHERE id = ?").bind(&id).execute(&env.st.db).await.unwrap();
    let err = head(State(env.st.clone()), amy.clone(), Path(id.clone())).await.unwrap_err();
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("size mismatch"), "{}", err.message);
    let (files,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE name = 'odd.txt'").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(files, 0);
    assert_eq!(used(&env, &amy.root_id).await, 0);
}

#[tokio::test]
async fn an_upload_whose_folder_moved_lands_in_the_folder_where_it_is_now() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let a = env.folder(&amy, &amy.root_id, "A").await;
    let b = env.folder(&amy, &amy.root_id, "B").await;
    let id = start(&env, &amy, &a, "", "plan.txt", 5).await.unwrap();
    move_to(&env, &amy, &a, &b).await;
    let res = send(&env, &amy, &id, 0, b"hello").await.unwrap();
    assert_eq!(file(&env, &res).await.2, a);

    // Moved to another space: it follows the folder there, and counts there
    let company = env.st.shared_root().unwrap();
    let id = start(&env, &amy, &a, "", "budget.txt", 6).await.unwrap();
    let before = used(&env, &amy.root_id).await;
    move_to(&env, &amy, &a, &company).await;
    let res = send(&env, &amy, &id, 0, b"budget").await.unwrap();
    assert_eq!(file(&env, &res).await, (b"budget".to_vec(), "budget.txt".into(), a.clone()));
    assert_eq!(env.drive_of(res.headers()["x-node-id"].to_str().unwrap()).await, env.drive_of(&company).await);
    assert_eq!(used(&env, &company).await, 5 + 6);
    assert_eq!(used(&env, &amy.root_id).await, before - 5);
}

#[tokio::test]
async fn an_upload_whose_folder_moved_to_a_full_space_fails() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let folder = env.folder(&amy, &amy.root_id, "Docs").await;
    // A team space with room for 8 bytes, 4 of them taken
    let team_root = {
        let mut conn = env.st.db.acquire().await.unwrap();
        let (_, root) = crate::db::create_drive(&mut conn, "Team", "team", amy.id, 8, "local").await.unwrap();
        crate::db::add_grant(&mut conn, &root, "user", amy.id, "owner", Some(amy.id), None).await.unwrap();
        root
    };
    let small = start(&env, &amy, &team_root, "", "small.txt", 4).await.unwrap();
    send(&env, &amy, &small, 0, b"four").await.unwrap();
    assert_eq!(start(&env, &amy, &team_root, "", "big.txt", 6).await.unwrap_err().status, StatusCode::PAYLOAD_TOO_LARGE);

    // Admitted in the personal space (no limit), then its folder moves into the team space
    let id = start(&env, &amy, &folder, "", "big.txt", 6).await.unwrap();
    move_to(&env, &amy, &folder, &team_root).await;
    let err = send(&env, &amy, &id, 0, b"sixsix").await.unwrap_err();
    assert_eq!((err.status, err.code), (StatusCode::PAYLOAD_TOO_LARGE, Some("upload_discarded")));
    let (files,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE name = 'big.txt'").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(files, 0);
    assert_eq!(used(&env, &team_root).await, 4);
    // Nothing is left to resume, and nothing holds space in either space
    let (rows,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM uploads WHERE node_id IS NULL").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(rows, 0);
}

#[tokio::test]
async fn files_of_an_uploaded_folder_go_into_folders_already_there() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let trip = env.folder(&amy, &amy.root_id, "Trip").await;
    // A folder with the same name in the trash isn't used
    let old = env.folder(&amy, &amy.root_id, "Old").await;
    let _ = crate::nodes::trash(State(env.st.clone()), amy.clone(), axum::Json(serde_json::from_value(serde_json::json!({ "ids": [old] })).unwrap()))
        .await
        .unwrap();

    let mut folders = Vec::new();
    for (rel, name) in [("trip/Day 1", "a.jpg"), ("TRIP/day 1", "b.jpg"), ("Trip", "c.jpg"), ("Old/x", "d.jpg")] {
        let id = start(&env, &amy, "root", rel, name, 3).await.unwrap();
        let res = send(&env, &amy, &id, 0, b"jpg").await.unwrap();
        folders.push(file(&env, &res).await.2);
    }
    // The two files of "Day 1" share one new folder inside the existing "Trip"; "c.jpg" is in "Trip" itself
    assert_eq!(folders[0], folders[1]);
    assert_eq!(folders[2], trip);
    let day = tree::get_node(&mut env.st.db.acquire().await.unwrap(), &folders[0]).await.unwrap().unwrap();
    assert_eq!((day.name.as_str(), day.parent_id.as_deref()), ("Day 1", Some(trip.as_str())));
    // A new "Old" was made next to the one in the trash
    let x = tree::get_node(&mut env.st.db.acquire().await.unwrap(), &folders[3]).await.unwrap().unwrap();
    let parent = tree::get_node(&mut env.st.db.acquire().await.unwrap(), x.parent_id.as_deref().unwrap()).await.unwrap().unwrap();
    assert_eq!(parent.name, "Old");
    assert_ne!(parent.id, old);
    assert!(parent.trashed_at.is_none());
}
