//! Limits through actual HTTP clients: overwrite credit and concurrent browser key creation.

use super::{put, read, server};
use aws_sdk_s3::{error::ProvideErrorMetadata, primitives::ByteStream};
use reqwest::StatusCode;
use std::{sync::Arc, time::Duration};

#[tokio::test]
async fn full_quota_allows_equal_and_smaller_put_and_copy_overwrites_but_refuses_extra_content() {
    let s = server(false).await;
    put(&s, "source.bin", b"source!!").await;
    put(&s, "target.bin", b"original").await;
    put(&s, "small.bin", b"mini").await;
    let root = s.env.st.shared_root().unwrap();
    let drive = s.env.drive_of(&root).await;
    let (used,): (i64,) = sqlx::query_as("SELECT used_bytes FROM drives WHERE id = ?").bind(&drive).fetch_one(&s.env.st.db).await.unwrap();
    assert_eq!(used, 20);
    sqlx::query("UPDATE drives SET quota_bytes = ? WHERE id = ?").bind(used).bind(&drive).execute(&s.env.st.db).await.unwrap();

    // There is no free space, but replacing eight bytes with eight adds nothing to the quota.
    put(&s, "target.bin", b"equalPUT").await;
    assert_eq!(read(&s, "target.bin").await.as_ref(), b"equalPUT");
    s.client.copy_object().bucket("backups").key("target.bin").copy_source("backups/source.bin").send().await.unwrap();
    assert_eq!(read(&s, "target.bin").await.as_ref(), b"source!!");
    let (used,): (i64,) = sqlx::query_as("SELECT used_bytes FROM drives WHERE id = ?").bind(&drive).fetch_one(&s.env.st.db).await.unwrap();
    assert_eq!(used, 20);

    // A smaller PUT frees space. Fill the quota to that new total before a smaller COPY as well.
    put(&s, "target.bin", b"tiny").await;
    assert_eq!(read(&s, "target.bin").await.as_ref(), b"tiny");
    let (used,): (i64,) = sqlx::query_as("SELECT used_bytes FROM drives WHERE id = ?").bind(&drive).fetch_one(&s.env.st.db).await.unwrap();
    assert_eq!(used, 16);
    sqlx::query("UPDATE drives SET quota_bytes = ? WHERE id = ?").bind(used).bind(&drive).execute(&s.env.st.db).await.unwrap();
    s.client.copy_object().bucket("backups").key("source.bin").copy_source("backups/small.bin").send().await.unwrap();
    assert_eq!(read(&s, "source.bin").await.as_ref(), b"mini");

    // Four bytes remain; a five-byte new object must be refused without changing the existing objects.
    let error = s
        .client
        .put_object()
        .bucket("backups")
        .key("overflow.bin")
        .body(ByteStream::from_static(b"extra"))
        .send()
        .await
        .expect_err("a new object cannot exceed the remaining quota");
    assert_eq!(error.as_service_error().and_then(|e| e.code()), Some("EntityTooLarge"));
    assert!(s.client.head_object().bucket("backups").key("overflow.bin").send().await.is_err());
    assert_eq!(read(&s, "target.bin").await.as_ref(), b"tiny");
    assert_eq!(read(&s, "source.bin").await.as_ref(), b"mini");
    assert_eq!(read(&s, "small.bin").await.as_ref(), b"mini");
    let (used,): (i64,) = sqlx::query_as("SELECT used_bytes FROM drives WHERE id = ?").bind(&drive).fetch_one(&s.env.st.db).await.unwrap();
    assert_eq!(used, 12, "a refused upload consumes no space quota");

    // The exact remaining amount still succeeds, distinguishing the quota failure from broken credentials.
    put(&s, "fits.bin", b"fits").await;
    assert_eq!(read(&s, "fits.bin").await.as_ref(), b"fits");
    let (used,): (i64,) = sqlx::query_as("SELECT used_bytes FROM drives WHERE id = ?").bind(&drive).fetch_one(&s.env.st.db).await.unwrap();
    assert_eq!(used, 16);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_browser_requests_for_the_last_key_slot_create_exactly_one_key() {
    let s = server(false).await;
    let (_, cookie) = s.env.sign_in(&s.admin, "S3 key limit regression").await;
    let http = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().unwrap();
    let url = format!("{}/api/s3/buckets/{}/keys", s.endpoint, s.bucket);

    // The contract server already made one key. Create the other 98 through the same authenticated browser API.
    for index in 1..99 {
        let response = http
            .post(&url)
            .header("host", "app.example.com")
            .header("origin", "http://app.example.com")
            .header("user-agent", "S3 key limit regression")
            .header("cookie", &cookie)
            .json(&serde_json::json!({ "description": format!("Existing key {index}"), "writable": true, "expires_at": null }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "fixture key {index}");
        let _ = response.bytes().await.unwrap();
    }
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM s3_keys WHERE bucket_id = ?").bind(&s.bucket).fetch_one(&s.env.st.db).await.unwrap();
    assert_eq!(count, 99);

    let gate = Arc::new(tokio::sync::Barrier::new(3));
    let spawn = |description: &'static str| {
        let (http, url, cookie, gate) = (http.clone(), url.clone(), cookie.clone(), gate.clone());
        tokio::spawn(async move {
            gate.wait().await;
            let response = http
                .post(url)
                .header("host", "app.example.com")
                .header("origin", "http://app.example.com")
                .header("user-agent", "S3 key limit regression")
                .header("cookie", cookie)
                .json(&serde_json::json!({ "description": description, "writable": true, "expires_at": null }))
                .send()
                .await
                .unwrap();
            let status = response.status();
            let body: serde_json::Value = response.json().await.unwrap();
            (status, body)
        })
    };
    let first = spawn("First concurrent key");
    let second = spawn("Second concurrent key");
    gate.wait().await;
    let results = tokio::time::timeout(Duration::from_secs(25), async { [first.await.unwrap(), second.await.unwrap()] }).await.unwrap();
    assert_eq!(results.iter().filter(|(status, _)| *status == StatusCode::OK).count(), 1);
    let rejected: Vec<_> = results.iter().filter(|(status, _)| *status != StatusCode::OK).collect();
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].0, StatusCode::BAD_REQUEST);
    assert_eq!(rejected[0].1["error"].as_str(), Some("This bucket already has 100 keys"));
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM s3_keys WHERE bucket_id = ?").bind(&s.bucket).fetch_one(&s.env.st.db).await.unwrap();
    assert_eq!(count, 100, "concurrent requests share one remaining slot");
}
