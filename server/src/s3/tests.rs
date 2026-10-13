//! Contract tests use the official AWS SDK over a real HTTP listener, not synthetic calls to operation handlers.

use crate::{testutil, util::now};
use aws_sdk_s3::{
    Client,
    config::{BehaviorVersion, Credentials, Region, RequestChecksumCalculation},
    presigning::PresigningConfig,
    primitives::ByteStream,
    types::{CompletedMultipartUpload, CompletedPart, Delete, ObjectIdentifier},
};
use axum::{
    Json,
    extract::{Path, State},
};

#[path = "regressions_limits.rs"]
mod regressions_limits;

struct Server {
    env: testutil::TestEnv,
    client: Client,
    admin: crate::auth::User,
    bucket: String,
    key: super::manage::CreatedKey,
    endpoint: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn server(folders: bool) -> Server {
    let env = if folders { testutil::folders_env().await } else { testutil::env().await };
    let admin = env.admin().await;
    let _ = super::config::save(
        State(env.st.clone()),
        crate::auth::Admin(admin.clone()),
        Json(super::config::Config { enabled: true, hostname: "127.0.0.1".into(), region: "us-east-1".into(), managers_publish: false }),
    )
    .await
    .unwrap();
    let root = env.st.shared_root().unwrap();
    let folder = env.folder(&admin, &root, "Published").await;
    // Test helpers don't make disk folders: folder-space fixtures use the actual shared handler.
    let folder = if folders {
        sqlx::query("DELETE FROM nodes WHERE id=?").bind(&folder).execute(&env.st.db).await.unwrap();
        let parent = crate::tree::get_node(&mut env.st.db.acquire().await.unwrap(), &root).await.unwrap().unwrap();
        crate::access::make_folder(&env.st, &admin, &parent, "Published").await.unwrap();
        crate::paths::child_named(&mut env.st.db.acquire().await.unwrap(), &root, "Published").await.unwrap().unwrap().id
    } else {
        folder
    };
    let Json(bucket) =
        super::manage::create(State(env.st.clone()), admin.clone(), Json(super::manage::Publish { node_id: folder, name: "backups".into() })).await.unwrap();
    let Json(key) = super::manage::new_key(
        State(env.st.clone()),
        admin.clone(),
        Path(bucket.id.clone()),
        Json(super::manage::NewKey { description: "Contract client".into(), writable: true, expires_at: None }),
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = crate::app::routes::router(env.st.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap();
    });
    let client = client(&endpoint, &key.key.access_key, &key.secret_key);
    Server { env, client, admin, bucket: bucket.id, key, endpoint, task }
}

fn client(endpoint: &str, access: &str, secret: &str) -> Client {
    Client::from_conf(
        aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new("us-east-1"))
            .credentials_provider(Credentials::new(access, secret, None, None, "contract"))
            .endpoint_url(endpoint)
            .force_path_style(true)
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
            .build(),
    )
}

async fn put(s: &Server, key: &str, value: &'static [u8]) {
    tokio::time::timeout(std::time::Duration::from_secs(20), s.client.put_object().bucket("backups").key(key).body(ByteStream::from_static(value)).send())
        .await
        .unwrap()
        .unwrap();
}

async fn read(s: &Server, key: &str) -> bytes::Bytes {
    s.client.get_object().bucket("backups").key(key).send().await.unwrap().body.collect().await.unwrap().into_bytes()
}

/// A signed raw response supplements SDK deserialization assertions (the SDK doesn't presign ListObjects).
async fn signed_get(s: &Server, parameters: &[(&str, &str)]) -> String {
    use hmac::{KeyInit, Mac};
    fn mac(key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(key).unwrap();
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }
    let encode = |value: &str| {
        value.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>()
    };
    let mut pairs = parameters.iter().map(|(key, value)| format!("{}={}", encode(key), encode(value))).collect::<Vec<_>>();
    pairs.sort();
    let query = pairs.join("&");
    let date = jiff::Timestamp::now().strftime("%Y%m%dT%H%M%SZ").to_string();
    let day = &date[..8];
    let host = s.endpoint.trim_start_matches("http://");
    let hash = crate::util::sha256_hex(b"");
    let canonical = format!("GET\n/backups\n{query}\nhost:{host}\nx-amz-content-sha256:{hash}\nx-amz-date:{date}\n\nhost;x-amz-content-sha256;x-amz-date\n{hash}");
    let scope = format!("{day}/us-east-1/s3/aws4_request");
    let to_sign = format!("AWS4-HMAC-SHA256\n{date}\n{scope}\n{}", crate::util::sha256_hex(canonical.as_bytes()));
    let key = mac(format!("AWS4{}", s.key.secret_key).as_bytes(), day.as_bytes());
    let key = mac(&key, b"us-east-1");
    let key = mac(&key, b"s3");
    let key = mac(&key, b"aws4_request");
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature={}",
        s.key.key.access_key,
        hex::encode(mac(&key, to_sign.as_bytes()))
    );
    reqwest::Client::new()
        .get(format!("{}/backups?{query}", s.endpoint))
        .header("x-amz-date", date)
        .header("x-amz-content-sha256", hash)
        .header("Authorization", authorization)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap()
}

#[tokio::test]
async fn sdk_round_trips_nested_unicode_content_md5_ranges_conditions_copy_and_trash() {
    let s = server(false).await;
    let buckets = s.client.list_buckets().send().await.unwrap();
    assert_eq!(buckets.buckets().len(), 1);
    assert_eq!(buckets.buckets()[0].name(), Some("backups"));
    s.client.head_bucket().bucket("backups").send().await.unwrap();
    s.client.get_bucket_location().bucket("backups").send().await.unwrap();
    let key = "a/b/price + # %.txt";
    let out = s.client.put_object().bucket("backups").key(key).content_md5("XUFAKrxLKna5cZ2REBfFkg==").body(ByteStream::from_static(b"hello")).send().await.unwrap();
    assert_eq!(out.e_tag(), Some("\"5d41402abc4b2a76b9719d911017c592\""));
    assert_eq!(read(&s, key).await.as_ref(), b"hello");
    let partial = s.client.get_object().bucket("backups").key(key).range("bytes=1-3").send().await.unwrap();
    assert_eq!(partial.content_range(), Some("bytes 1-3/5"));
    assert_eq!(partial.body.collect().await.unwrap().into_bytes().as_ref(), b"ell");
    assert!(s.client.get_object().bucket("backups").key(key).range("bytes=99-").send().await.is_err());
    let head = s.client.head_object().bucket("backups").key(key).send().await.unwrap();
    assert_eq!(head.content_length(), Some(5));
    assert_eq!(head.e_tag(), out.e_tag());
    let signed =
        s.client.get_object().bucket("backups").key(key).presigned(PresigningConfig::expires_in(std::time::Duration::from_secs(300)).unwrap()).await.unwrap();
    let ranged = reqwest::Client::new().get(signed.uri()).header("range", "bytes=1-3").send().await.unwrap();
    assert_eq!(ranged.status(), axum::http::StatusCode::PARTIAL_CONTENT);
    assert_eq!(ranged.bytes().await.unwrap().as_ref(), b"ell");
    let not_modified = reqwest::Client::new().get(signed.uri()).header("if-none-match", out.e_tag().unwrap()).send().await.unwrap();
    assert_eq!(not_modified.status(), axum::http::StatusCode::NOT_MODIFIED);
    assert!(not_modified.bytes().await.unwrap().is_empty());
    assert!(s.client.get_object().bucket("backups").key(key).if_match("\"wrong\"").send().await.is_err());
    assert!(s.client.put_object().bucket("backups").key(key).content_md5("AAAAAAAAAAAAAAAAAAAAAA==").body(ByteStream::from_static(b"wrong")).send().await.is_err());
    assert_eq!(read(&s, key).await.as_ref(), b"hello");
    s.client
        .copy_object()
        .bucket("backups")
        .key("copied.txt")
        .copy_source(format!("backups/{}", percent_encoding::utf8_percent_encode(key, percent_encoding::NON_ALPHANUMERIC)))
        .send()
        .await
        .unwrap();
    assert_eq!(read(&s, "copied.txt").await.as_ref(), b"hello");
    put(&s, key, b"changed").await;
    let (versions,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM node_versions").fetch_one(&s.env.st.db).await.unwrap();
    assert!(versions > 0);
    s.client.delete_object().bucket("backups").key("copied.txt").send().await.unwrap();
    let (trashed,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE name='copied.txt' AND trashed_at IS NOT NULL").fetch_one(&s.env.st.db).await.unwrap();
    assert_eq!(trashed, 1);
    let batch = Delete::builder()
        .objects(ObjectIdentifier::builder().key(key).build().unwrap())
        .objects(ObjectIdentifier::builder().key("missing.txt").build().unwrap())
        .build()
        .unwrap();
    assert_eq!(s.client.delete_objects().bucket("backups").delete(batch).send().await.unwrap().deleted().len(), 2);
    let (activity,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM activity WHERE detail LIKE '%through S3 bucket backups, key Contract client%'").fetch_one(&s.env.st.db).await.unwrap();
    assert!(activity >= 6);
}

#[tokio::test]
async fn byte_order_prefix_delimiter_v1_v2_tokens_and_empty_folder_objects() {
    let s = server(false).await;
    for key in ["Z.txt", "a.txt", "a/x.txt", "a/y.txt", "zz.txt", "é.txt", "中.txt"] {
        put(&s, key, b"x").await;
    }
    assert!(s.client.put_object().bucket("backups").key("z.txt").body(ByteStream::from_static(b"overwrite")).send().await.is_err());
    assert_eq!(read(&s, "Z.txt").await.as_ref(), b"x");
    put(&s, "empty/", b"").await;
    let mut keys = Vec::new();
    let mut token = None;
    loop {
        let page = s.client.list_objects_v2().bucket("backups").max_keys(2).set_continuation_token(token).send().await.unwrap();
        keys.extend(page.contents().iter().map(|o| o.key().unwrap().to_string()));
        token = page.next_continuation_token().map(str::to_string);
        if token.is_none() {
            break;
        }
    }
    let mut sorted = keys.clone();
    sorted.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    assert_eq!(keys, sorted);
    assert_eq!(keys.len(), 9, "{keys:?}");
    let first = s.client.list_objects_v2().bucket("backups").delimiter("/").max_keys(2).send().await.unwrap();
    assert!(first.is_truncated().unwrap());
    let token = first.next_continuation_token().unwrap();
    assert!(s.client.list_objects_v2().bucket("backups").prefix("other").delimiter("/").continuation_token(token).send().await.is_err());
    assert!(s.client.list_objects_v2().bucket("backups").continuation_token("tampered").send().await.is_err());
    let filtered = s.client.list_objects_v2().bucket("backups").prefix("a/").send().await.unwrap();
    assert_eq!(filtered.contents().len(), 3);
    let v1 = s.client.list_objects().bucket("backups").prefix("a/").delimiter("/").send().await.unwrap();
    assert_eq!(v1.contents().len(), 3);
    let groups = s.client.list_objects_v2().bucket("backups").delimiter("/").send().await.unwrap();
    assert_eq!(groups.common_prefixes().len(), 2);
    for bad in ["../escape.txt", "a//bad.txt", "/absolute.txt", "a/ padded.txt", "a/../bad.txt", "a\\bad.txt"] {
        assert!(s.client.put_object().bucket("backups").key(bad).body(ByteStream::from_static(b"bad")).send().await.is_err(), "{bad}");
    }
    assert!(s.client.get_object().bucket("other").key("a.txt").send().await.is_err());
    assert!(s.client.put_bucket_acl().bucket("backups").send().await.is_err());
}

#[tokio::test]
async fn presigned_get_put_readonly_revoked_and_expired_keys_are_enforced_for_admin_creators() {
    let s = server(false).await;
    let duration = PresigningConfig::expires_in(std::time::Duration::from_secs(300)).unwrap();
    let signed = s.client.put_object().bucket("backups").key("presigned.txt").presigned(duration.clone()).await.unwrap();
    assert!(reqwest::Client::new().put(signed.uri()).body("signed").send().await.unwrap().status().is_success());
    let signed = s.client.get_object().bucket("backups").key("presigned.txt").presigned(duration.clone()).await.unwrap();
    assert_eq!(reqwest::get(signed.uri()).await.unwrap().text().await.unwrap(), "signed");
    let Json(readonly) = super::manage::new_key(
        State(s.env.st.clone()),
        s.admin.clone(),
        Path(s.bucket.clone()),
        Json(super::manage::NewKey { description: "Read client".into(), writable: false, expires_at: None }),
    )
    .await
    .unwrap();
    let client = client(&s.endpoint, &readonly.key.access_key, &readonly.secret_key);
    assert!(client.get_object().bucket("backups").key("presigned.txt").send().await.is_ok());
    assert!(client.put_object().bucket("backups").key("denied.txt").body(ByteStream::from_static(b"x")).send().await.is_err());
    assert!(client.delete_object().bucket("backups").key("presigned.txt").send().await.is_err());
    assert!(client.copy_object().bucket("backups").copy_source("backups/presigned.txt").key("denied.txt").send().await.is_err());
    assert!(client.create_multipart_upload().bucket("backups").key("denied.txt").send().await.is_err());
    let signed = client.put_object().bucket("backups").key("denied.txt").presigned(duration).await.unwrap();
    assert_eq!(reqwest::Client::new().put(signed.uri()).body("x").send().await.unwrap().status(), axum::http::StatusCode::FORBIDDEN);
    sqlx::query("UPDATE s3_keys SET expires_at=? WHERE id=?").bind(now() + 60).bind(&s.key.key.id).execute(&s.env.st.db).await.unwrap();
    let too_long = s
        .client
        .get_object()
        .bucket("backups")
        .key("presigned.txt")
        .presigned(PresigningConfig::expires_in(std::time::Duration::from_secs(300)).unwrap())
        .await
        .unwrap();
    assert_eq!(reqwest::get(too_long.uri()).await.unwrap().status(), axum::http::StatusCode::FORBIDDEN);
    sqlx::query("UPDATE s3_keys SET enabled=0 WHERE id=?").bind(&s.key.key.id).execute(&s.env.st.db).await.unwrap();
    assert!(s.client.list_buckets().send().await.is_err());
    sqlx::query("UPDATE s3_keys SET enabled=1,expires_at=? WHERE id=?").bind(now() - 1).bind(&s.key.key.id).execute(&s.env.st.db).await.unwrap();
    assert!(s.client.list_buckets().send().await.is_err());
}

#[tokio::test]
async fn multipart_parts_are_streamed_replaced_bound_to_the_key_completed_and_cleaned() {
    let s = server(false).await;
    let upload = s.client.create_multipart_upload().bucket("backups").key("large.bin").metadata("mtime", "1700000000.123456789").send().await.unwrap();
    let id = upload.upload_id().unwrap();
    let data = vec![b'a'; 5 * 1024 * 1024];
    let _ = s.client.upload_part().bucket("backups").key("large.bin").upload_id(id).part_number(1).body(ByteStream::from(data)).send().await.unwrap();
    let (old_file,): (String,) = sqlx::query_as("SELECT file_id FROM s3_parts WHERE upload_id=? AND number=1").bind(id).fetch_one(&s.env.st.db).await.unwrap();
    let first = s
        .client
        .upload_part()
        .bucket("backups")
        .key("large.bin")
        .upload_id(id)
        .part_number(1)
        .body(ByteStream::from(vec![b'b'; 5 * 1024 * 1024]))
        .send()
        .await
        .unwrap();
    assert!(!s.env.st.tmp_dir().join(format!("s3-part-{old_file}")).exists());
    let last = s.client.upload_part().bucket("backups").key("large.bin").upload_id(id).part_number(2).body(ByteStream::from_static(b"last")).send().await.unwrap();
    let listed = s.client.list_parts().bucket("backups").key("large.bin").upload_id(id).max_parts(1).send().await.unwrap();
    assert!(listed.is_truncated().unwrap());
    assert_eq!(listed.parts().len(), 1);
    assert_eq!(s.client.list_multipart_uploads().bucket("backups").send().await.unwrap().uploads().len(), 1);
    let Json(other) = super::manage::new_key(
        State(s.env.st.clone()),
        s.admin.clone(),
        Path(s.bucket.clone()),
        Json(super::manage::NewKey { description: "Other client".into(), writable: true, expires_at: None }),
    )
    .await
    .unwrap();
    assert!(client(&s.endpoint, &other.key.access_key, &other.secret_key).list_parts().bucket("backups").key("large.bin").upload_id(id).send().await.is_err());
    let selection = CompletedMultipartUpload::builder()
        .parts(CompletedPart::builder().part_number(1).e_tag(first.e_tag().unwrap()).build())
        .parts(CompletedPart::builder().part_number(2).e_tag(last.e_tag().unwrap()).build())
        .build();
    let completed = s.client.complete_multipart_upload().bucket("backups").key("large.bin").upload_id(id).multipart_upload(selection).send().await.unwrap();
    assert!(completed.e_tag().is_some());
    assert_eq!(s.client.head_object().bucket("backups").key("large.bin").send().await.unwrap().metadata().unwrap().get("mtime").unwrap(), "1700000000.123456789");
    let data = read(&s, "large.bin").await;
    assert_eq!(data.len(), 5 * 1024 * 1024 + 4);
    assert!(data.ends_with(b"last"));
    assert!(data[..5 * 1024 * 1024].iter().all(|b| *b == b'b'));
    assert!(s.client.list_parts().bucket("backups").key("large.bin").upload_id(id).send().await.is_err());
    let upload = s.client.create_multipart_upload().bucket("backups").key("cancel.bin").send().await.unwrap();
    s.client
        .upload_part()
        .bucket("backups")
        .key("cancel.bin")
        .upload_id(upload.upload_id().unwrap())
        .part_number(1)
        .body(ByteStream::from_static(b"part"))
        .send()
        .await
        .unwrap();
    s.client.abort_multipart_upload().bucket("backups").key("cancel.bin").upload_id(upload.upload_id().unwrap()).send().await.unwrap();
    let (parts,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM s3_parts").fetch_one(&s.env.st.db).await.unwrap();
    assert_eq!(parts, 0);
    assert!(!std::fs::read_dir(s.env.st.tmp_dir()).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with("s3-part-")));
    let upload = s.client.create_multipart_upload().bucket("backups").key("expired.bin").send().await.unwrap();
    s.client
        .upload_part()
        .bucket("backups")
        .key("expired.bin")
        .upload_id(upload.upload_id().unwrap())
        .part_number(1)
        .body(ByteStream::from_static(b"expired"))
        .send()
        .await
        .unwrap();
    sqlx::query("UPDATE s3_uploads SET created_at=?").bind(now() - 86401).execute(&s.env.st.db).await.unwrap();
    super::multipart::cleanup(&s.env.st).await.unwrap();
    assert!(s.client.list_parts().bucket("backups").key("expired.bin").upload_id(upload.upload_id().unwrap()).send().await.is_err());
    assert!(!std::fs::read_dir(s.env.st.tmp_dir()).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with("s3-part-")));
}

#[tokio::test]
async fn rclone_mtime_survives_identical_put_and_copy_but_never_external_or_other_protocol_changes() {
    use aws_sdk_s3::types::MetadataDirective;
    for folders in [false, true] {
        let s = server(folders).await;
        s.client
            .put_object()
            .bucket("backups")
            .key("mtime.txt")
            .metadata("mtime", "1700000000.123456789")
            .body(ByteStream::from_static(b"old"))
            .send()
            .await
            .unwrap();
        let head = s.client.head_object().bucket("backups").key("mtime.txt").send().await.unwrap();
        assert_eq!(head.metadata().unwrap().get("mtime").unwrap(), "1700000000.123456789");
        let get = s.client.get_object().bucket("backups").key("mtime.txt").send().await.unwrap();
        assert_eq!(get.metadata().unwrap().get("mtime"), head.metadata().unwrap().get("mtime"));
        s.client.put_object().bucket("backups").key("mtime.txt").metadata("mtime", "1800000000").body(ByteStream::from_static(b"old")).send().await.unwrap();
        s.client.copy_object().bucket("backups").key("copy.txt").copy_source("backups/mtime.txt").send().await.unwrap();
        assert_eq!(s.client.head_object().bucket("backups").key("copy.txt").send().await.unwrap().metadata().unwrap().get("mtime").unwrap(), "1800000000");
        s.client
            .copy_object()
            .bucket("backups")
            .key("mtime.txt")
            .copy_source("backups/mtime.txt")
            .metadata_directive(MetadataDirective::Replace)
            .metadata("mtime", "1900000000")
            .send()
            .await
            .unwrap();
        assert_eq!(s.client.head_object().bucket("backups").key("mtime.txt").send().await.unwrap().metadata().unwrap().get("mtime").unwrap(), "1900000000");
        for invalid in ["NaN", "inf", "-9999999999999999", "1e999", " padded "] {
            assert!(s.client.put_object().bucket("backups").key("mtime.txt").metadata("mtime", invalid).body(ByteStream::from_static(b"bad")).send().await.is_err());
        }
        assert!(
            s.client
                .put_object()
                .bucket("backups")
                .key("mtime.txt")
                .metadata("arbitrary", "not-supported")
                .body(ByteStream::from_static(b"bad"))
                .send()
                .await
                .is_err()
        );
        assert_eq!(read(&s, "mtime.txt").await.as_ref(), b"old");
        let identity = super::credentials::identity(&s.env.st, &s.key.key.access_key, true).await.unwrap();
        if folders {
            let node = crate::access::rooted_item(&s.env.st, &identity.user, &identity.root.id, "mtime.txt", crate::tree::Need::Read).await.unwrap();
            tokio::fs::write(node.fs_file().unwrap(), b"external longer content").await.unwrap();
        } else {
            let up = crate::access::upload_to(&s.env.st, &s.admin, &identity.root, "mtime.txt".into()).await.unwrap();
            let body = futures_util::stream::iter([Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"other protocol"))]);
            crate::access::write(&s.env.st, &s.admin, up, body, Some(14)).await.unwrap();
        }
        assert!(s.client.head_object().bucket("backups").key("mtime.txt").send().await.unwrap().metadata().is_none_or(|m| !m.contains_key("mtime")));
        put(&s, "copy.txt", b"old").await;
        assert!(s.client.head_object().bucket("backups").key("copy.txt").send().await.unwrap().metadata().is_none_or(|m| !m.contains_key("mtime")));
    }
}

#[tokio::test]
async fn folder_space_writes_and_external_changes_get_fresh_md5_and_readonly_is_respected() {
    let s = server(true).await;
    put(&s, "external.txt", b"old").await;
    let first = s.client.head_object().bucket("backups").key("external.txt").send().await.unwrap().e_tag().unwrap().to_string();
    let identity = super::credentials::identity(&s.env.st, &s.key.key.access_key, false).await.unwrap();
    let node = crate::access::rooted_item(&s.env.st, &identity.user, &identity.root.id, "external.txt", crate::tree::Need::Read).await.unwrap();
    tokio::fs::write(node.fs_file().unwrap(), b"new longer content").await.unwrap();
    let after = s.client.head_object().bucket("backups").key("external.txt").send().await.unwrap();
    assert_ne!(after.e_tag(), Some(first.as_str()));
    assert_eq!(after.content_length(), Some(18));
    let listed = s.client.list_objects_v2().bucket("backups").prefix("external.txt").send().await.unwrap();
    assert_eq!(listed.contents()[0].size(), Some(18));
    assert_eq!(listed.contents()[0].e_tag(), after.e_tag());
    assert_eq!(read(&s, "external.txt").await.as_ref(), b"new longer content");
    sqlx::query("UPDATE drives SET read_only=1 WHERE id=?").bind(identity.root.drive()).execute(&s.env.st.db).await.unwrap();
    assert!(s.client.put_object().bucket("backups").key("bad.txt").body(ByteStream::from_static(b"bad")).send().await.is_err());
    assert_eq!(read(&s, "external.txt").await.as_ref(), b"new longer content");
}

#[tokio::test]
async fn url_encoding_covers_objects_groups_and_all_echoed_listing_fields() {
    use aws_sdk_s3::types::EncodingType;
    let s = server(false).await;
    let prefix = "中文 # %/";
    put(&s, "中文 # %/a.txt", b"a").await;
    put(&s, "中文 # %/b.txt", b"b").await;
    let encoded = |v: &str| percent_encoding::utf8_percent_encode(v, percent_encoding::NON_ALPHANUMERIC).to_string();
    let page = s.client.list_objects_v2().bucket("backups").prefix(prefix).delimiter("/").start_after(prefix).encoding_type(EncodingType::Url).send().await.unwrap();
    assert_eq!(page.prefix(), Some(encoded(prefix).as_str()));
    assert_eq!(page.delimiter(), Some("%2F"));
    assert_eq!(page.start_after(), Some(encoded(prefix).as_str()));
    assert_eq!(page.contents()[0].key(), Some(encoded("中文 # %/a.txt").as_str()));
    let xml = signed_get(&s, &[("list-type", "2"), ("prefix", prefix), ("delimiter", "/"), ("start-after", prefix), ("encoding-type", "url")]).await;
    for field in ["Prefix", "StartAfter"] {
        assert!(xml.contains(&format!("<{field}>{}</{field}>", encoded(prefix))), "{xml}");
    }
    assert!(xml.contains("<Delimiter>%2F</Delimiter>"), "{xml}");
    let old = s.client.list_objects().bucket("backups").prefix(prefix).marker(prefix).encoding_type(EncodingType::Url).max_keys(1).send().await.unwrap();
    assert_eq!(old.marker(), Some(encoded(prefix).as_str()));
    assert_eq!(old.next_marker(), Some(encoded("中文 # %/a.txt").as_str()));
    let groups = s.client.list_objects_v2().bucket("backups").delimiter("/").encoding_type(EncodingType::Url).send().await.unwrap();
    assert_eq!(groups.common_prefixes()[0].prefix(), Some(encoded(prefix).as_str()));
}

#[tokio::test]
async fn cancelling_a_partial_body_removes_its_owned_temporary_file() {
    let s = server(false).await;
    let identity = super::credentials::identity(&s.env.st, &s.key.key.access_key, true).await.unwrap();
    let st = s.env.st.clone();
    let received = std::sync::Arc::new(tokio::sync::Notify::new());
    let signal = received.clone();
    let body = futures_util::stream::once(std::future::ready(Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"partial")))).chain(futures_util::stream::poll_fn(
        move |_| {
            signal.notify_one();
            std::task::Poll::Pending
        },
    ));
    use futures_util::StreamExt;
    let task = tokio::spawn(async move {
        super::objects::receive(
            &st,
            &identity,
            &identity.root,
            body,
            super::objects::Incoming { size: None, md5: None, headers: &axum::http::HeaderMap::new(), trailers: None, replaced: 0 },
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), received.notified()).await.unwrap();
    assert!(std::fs::read_dir(s.env.st.tmp_dir()).unwrap().flatten().any(|f| f.file_name().to_string_lossy().starts_with("s3-receive-")));
    task.abort();
    assert!(matches!(task.await, Err(e) if e.is_cancelled()));
    assert!(!std::fs::read_dir(s.env.st.tmp_dir()).unwrap().flatten().any(|f| f.file_name().to_string_lossy().starts_with("s3-receive-")));
}

#[tokio::test]
async fn verified_slow_uploads_release_only_their_own_sign_in_budget_reservation() {
    use tokio::io::AsyncWriteExt;
    let s = server(false).await;
    let rate = format!("ip:{}", crate::auth::limit_key_ip("127.0.0.1"));
    for _ in 0..29 {
        assert!(crate::auth::begin_attempt(&s.env.st, &rate, crate::auth::IP_FAIL_LIMIT));
    }
    let signed =
        s.client.put_object().bucket("backups").key("slow.bin").presigned(PresigningConfig::expires_in(std::time::Duration::from_secs(300)).unwrap()).await.unwrap();
    let uri: axum::http::Uri = signed.uri().parse().unwrap();
    let mut stream = tokio::net::TcpStream::connect(uri.authority().unwrap().as_str()).await.unwrap();
    stream
        .write_all(format!("PUT {} HTTP/1.1\r\nHost: {}\r\nContent-Length: 1024\r\n\r\nx", uri.path_and_query().unwrap(), uri.authority().unwrap()).as_bytes())
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if std::fs::read_dir(s.env.st.tmp_dir()).unwrap().flatten().any(|f| f.file_name().to_string_lossy().starts_with("s3-receive-")) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    for _ in 0..30 {
        s.client.head_bucket().bucket("backups").send().await.unwrap();
    }
    assert!(!crate::auth::attempts_exhausted(&s.env.st, &rate, 30));
    assert!(crate::auth::attempts_exhausted(&s.env.st, &rate, 29), "valid requests must preserve earlier failures");
    let bad = client(&s.endpoint, &s.key.key.access_key, "incorrect-secret");
    assert!(bad.head_bucket().bucket("backups").send().await.is_err());
    assert!(crate::auth::attempts_exhausted(&s.env.st, &rate, 30));
    drop(stream);
}

#[tokio::test]
async fn revoked_credentials_and_case_only_creation_are_rechecked_at_file_commit() {
    let s = server(true).await;
    let identity = super::credentials::identity(&s.env.st, &s.key.key.access_key, true).await.unwrap();
    let held = crate::fsops::lock_space(&s.env.st, identity.root.drive()).await;
    let request = s.client.put_object().bucket("backups").key("late.txt").body(ByteStream::from_static(b"late"));
    let pending = tokio::spawn(async move { request.send().await });
    wait_for_staged(&identity.root).await;
    let _ = super::manage::revoke_key(
        State(s.env.st.clone()),
        s.admin.clone(),
        Path((s.bucket.clone(), s.key.key.id.clone())),
        Json(super::manage::Enabled { enabled: false }),
    )
    .await
    .unwrap();
    drop(held);
    assert!(pending.await.unwrap().is_err());
    assert!(!identity.root.fs_file().unwrap().join("late.txt").exists());
    let _ = super::manage::revoke_key(
        State(s.env.st.clone()),
        s.admin.clone(),
        Path((s.bucket.clone(), s.key.key.id.clone())),
        Json(super::manage::Enabled { enabled: true }),
    )
    .await
    .unwrap();
    let held = crate::fsops::lock_space(&s.env.st, identity.root.drive()).await;
    let request = s.client.put_object().bucket("backups").key("lower.txt").body(ByteStream::from_static(b"overwrite"));
    let pending = tokio::spawn(async move { request.send().await });
    wait_for_staged(&identity.root).await;
    let path = identity.root.fs_file().unwrap().join("Lower.txt");
    std::fs::write(&path, b"original").unwrap();
    // Simulate another publisher committing the distinct spelling while this body was in flight.
    s.env.stored_file(&s.admin, &identity.root.id, "Lower.txt", b"original").await;
    drop(held);
    assert!(pending.await.unwrap().is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"original");
}

async fn wait_for_staged(root: &crate::tree::Node) {
    let directory = std::path::Path::new(root.fs_root.as_deref().unwrap());
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if std::fs::read_dir(directory).unwrap().flatten().any(|f| f.file_name().to_string_lossy().starts_with(".thirtyfile-upload-")) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn team_admin_scope_and_manager_revocation_do_not_grant_unrelated_access() {
    let s = server(false).await;
    let identity = super::credentials::identity(&s.env.st, &s.key.key.access_key, true).await.unwrap();
    sqlx::query("UPDATE drives SET kind='team' WHERE id=?").bind(identity.root.drive()).execute(&s.env.st.db).await.unwrap();
    sqlx::query("DELETE FROM grants WHERE node_id=?").bind(s.env.st.shared_root().unwrap()).execute(&s.env.st.db).await.unwrap();
    assert!(crate::tree::node_for(&mut s.env.st.db.acquire().await.unwrap(), &s.admin, &identity.root.id, crate::tree::Need::Read).await.is_err());
    put(&s, "admin-team.txt", b"scoped").await;
    let sibling = s.env.folder(&s.admin, &s.env.st.shared_root().unwrap(), "Private sibling").await;
    assert!(crate::tree::node_for(&mut s.env.st.db.acquire().await.unwrap(), &identity.user, &sibling, crate::tree::Need::Read).await.is_err());
    assert!(crate::tree::node_for(&mut s.env.st.db.acquire().await.unwrap(), &identity.user, s.admin.root(), crate::tree::Need::Read).await.is_err());
    let manager = s.env.user("bucket-manager", true).await;
    sqlx::query("INSERT INTO grants(node_id,principal_type,principal_id,role,created_at) VALUES(?,'user',?,'manager',?)")
        .bind(&identity.root.id)
        .bind(manager.id)
        .bind(now())
        .execute(&s.env.st.db)
        .await
        .unwrap();
    let mut config = super::config::load(&s.env.st).await.unwrap();
    config.managers_publish = true;
    let _ = super::config::save(State(s.env.st.clone()), crate::auth::Admin(s.admin.clone()), Json(config)).await.unwrap();
    let Json(bucket) = super::manage::create(
        State(s.env.st.clone()),
        manager.clone(),
        Json(super::manage::Publish { node_id: identity.root.id.clone(), name: "managed-team".into() }),
    )
    .await
    .unwrap();
    let Json(key) = super::manage::new_key(
        State(s.env.st.clone()),
        manager.clone(),
        Path(bucket.id),
        Json(super::manage::NewKey { description: "Manager key".into(), writable: true, expires_at: None }),
    )
    .await
    .unwrap();
    let client = client(&s.endpoint, &key.key.access_key, &key.secret_key);
    assert!(client.list_objects_v2().bucket("managed-team").send().await.is_ok());
    sqlx::query("DELETE FROM grants WHERE principal_id=? AND principal_type='user'").bind(manager.id).execute(&s.env.st.db).await.unwrap();
    assert!(client.list_objects_v2().bucket("managed-team").send().await.is_err());
    sqlx::query("UPDATE nodes SET parent_id=?,drive_id=? WHERE id=?")
        .bind(s.admin.root())
        .bind(s.env.drive_of(s.admin.root()).await)
        .bind(&identity.root.id)
        .execute(&s.env.st.db)
        .await
        .unwrap();
    assert!(s.client.list_objects_v2().bucket("backups").send().await.is_err());
}

#[tokio::test]
async fn defaults_publish_personal_manager_links_trash_and_credential_rotation_are_checked() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    assert!(!super::config::load(&env.st).await.unwrap().enabled);
    let _ = super::config::save(State(env.st.clone()), crate::auth::Admin(admin.clone()), Json(super::config::Config::default())).await.unwrap();
    assert!(!super::manage::bucket_name("Bad.Name"));
    assert!(!super::manage::bucket_name("-bad"));
    assert!(super::manage::bucket_name("backup-2026"));
    assert!(
        super::manage::create(State(env.st.clone()), admin.clone(), Json(super::manage::Publish { node_id: admin.root().into(), name: "personal".into() }))
            .await
            .is_err()
    );
    let s = server(false).await;
    let identity = super::credentials::identity(&s.env.st, &s.key.key.access_key, false).await.unwrap();
    assert!(!s.key.key.secret.contains(&s.key.secret_key));
    let rotated = rand::random();
    crate::settings::reseal_secrets(&s.env.st.db, &rotated).await.unwrap();
    let (secret,): (String,) = sqlx::query_as("SELECT secret FROM s3_keys WHERE id=?").bind(&s.key.key.id).fetch_one(&s.env.st.db).await.unwrap();
    assert_ne!(secret, s.key.key.secret);
    // Restore the original ciphertext: the test process has not rotated its process-wide cipher.
    sqlx::query("UPDATE s3_keys SET secret=? WHERE id=?").bind(&s.key.key.secret).bind(&s.key.key.id).execute(&s.env.st.db).await.unwrap();
    sqlx::query("UPDATE drives SET link_makers='nobody' WHERE id=?").bind(identity.root.drive()).execute(&s.env.st.db).await.unwrap();
    assert!(s.client.list_buckets().send().await.is_err());
    sqlx::query("UPDATE drives SET link_makers='editors' WHERE id=?").bind(identity.root.drive()).execute(&s.env.st.db).await.unwrap();
    crate::access::trash(&s.env.st, &s.admin, &identity.root.id).await.unwrap();
    assert!(s.client.list_buckets().send().await.is_err());
    crate::access::restore(&s.env.st, &s.admin, &identity.root.id).await.unwrap();
    assert!(s.client.list_buckets().send().await.is_ok());
}
