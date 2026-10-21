use crate::{auth::User, testutil, tree};
use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};

async fn save(env: &testutil::TestEnv, user: &User, id: &str, base: i64, key: &str, body: &'static [u8]) -> crate::error::AppResult<tree::Node> {
    let mut headers = HeaderMap::new();
    headers.insert("x-base-version", base.into());
    headers.insert("idempotency-key", key.parse().unwrap());
    super::save_content(State(env.st.clone()), user.clone(), Path(id.into()), headers, Bytes::from_static(body)).await.map(|Json(node)| node)
}

#[tokio::test]
async fn concurrent_retries_commit_one_version_and_receipts_survive_a_new_connection() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let id = env.stored_file(&amy, amy.root(), "document.docx", b"original").await;
    let base = tree::get_node(&mut env.st.db.acquire().await.unwrap(), &id).await.unwrap().unwrap().updated_at;
    let (a, b) = tokio::join!(save(&env, &amy, &id, base, "same-request", b"saved"), save(&env, &amy, &id, base, "same-request", b"saved"));
    let committed = a.unwrap();
    assert_eq!(b.unwrap().updated_at, committed.updated_at);
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM node_versions WHERE node_id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(count.0, 1);
    assert_eq!(save(&env, &amy, &id, base, "same-request", b"saved").await.unwrap().updated_at, committed.updated_at);
    assert_eq!(save(&env, &amy, &id, base, "same-request", b"different").await.unwrap_err().status, StatusCode::BAD_REQUEST);
    assert_eq!(save(&env, &amy, &id, base, "another-request", b"stale").await.unwrap_err().status, StatusCode::CONFLICT);
    let newer = save(&env, &amy, &id, committed.updated_at, "new-request", b"latest").await.unwrap();
    assert_eq!(save(&env, &amy, &id, base, "same-request", b"saved").await.unwrap().updated_at, committed.updated_at);
    assert_eq!(tree::get_node(&mut env.st.db.acquire().await.unwrap(), &id).await.unwrap().unwrap().updated_at, newer.updated_at);
    let bob = env.user("bob", true).await;
    assert!(save(&env, &bob, &id, base, "same-request", b"saved").await.is_err());
}

#[tokio::test]
async fn strong_content_conditions_are_checked_under_the_write_lock() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let id = env.stored_file(&amy, amy.root(), "document.docx", b"original").await;
    let node = tree::get_node(&mut env.st.db.acquire().await.unwrap(), &id).await.unwrap().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("x-base-version", node.updated_at.into());
    headers.insert("if-match", "\"wrong\"".parse().unwrap());
    let error = super::save_content(State(env.st.clone()), amy, Path(id), headers, Bytes::from_static(b"new")).await.unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
}
