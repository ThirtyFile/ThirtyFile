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

mod regressions_limits;
mod restricted_regressions;

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
    let Json(bucket) = super::manage::create(
        State(env.st.clone()),
        admin.clone(),
        Json(super::manage::Publish { password: Some(crate::testutil::password().into()), code: None, node_id: folder, name: "backups".into() }),
    )
    .await
    .unwrap();
    let Json(key) = super::manage::new_key(
        State(env.st.clone()),
        admin.clone(),
        Path(bucket.id.clone()),
        Json(super::manage::NewKey {
            password: Some(crate::testutil::password().into()),
            code: None,
            description: "Contract client".into(),
            writable: true,
            expires_at: None,
        }),
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
        Json(super::manage::NewKey {
            password: Some(crate::testutil::password().into()),
            code: None,
            description: "Read client".into(),
            writable: false,
            expires_at: None,
        }),
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
        Json(super::manage::NewKey {
            password: Some(crate::testutil::password().into()),
            code: None,
            description: "Other client".into(),
            writable: true,
            expires_at: None,
        }),
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
    let body = futures_util::stream::once(std::future::ready(Ok::<_, thirtys3_core::S3Error>(bytes::Bytes::from_static(b"partial")))).chain(
        futures_util::stream::poll_fn(move |_| {
            signal.notify_one();
            std::task::Poll::Pending
        }),
    );
    use futures_util::StreamExt;
    let task =
        tokio::spawn(async move { super::objects::receive(&st, &identity, &identity.root, body, super::objects::Incoming { declared: None, replaced: 0 }).await });
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
        Json(super::manage::Publish {
            password: Some(crate::testutil::password().into()),
            code: None,
            node_id: identity.root.id.clone(),
            name: "managed-team".into(),
        }),
    )
    .await
    .unwrap();
    let Json(key) = super::manage::new_key(
        State(s.env.st.clone()),
        manager.clone(),
        Path(bucket.id),
        Json(super::manage::NewKey {
            password: Some(crate::testutil::password().into()),
            code: None,
            description: "Manager key".into(),
            writable: true,
            expires_at: None,
        }),
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
        super::manage::create(
            State(env.st.clone()),
            admin.clone(),
            Json(super::manage::Publish { password: Some(crate::testutil::password().into()), code: None, node_id: admin.root().into(), name: "personal".into() })
        )
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

#[tokio::test]
async fn key_expiry_uses_the_existing_access_notice_once_and_skips_disabled_or_distant_keys() {
    let s = server(false).await;
    for (description, expiry, enabled) in [("Distant key", now() + 4 * 86400, true), ("Disabled key", now() + 86400, false)] {
        let Json(key) = super::manage::new_key(
            State(s.env.st.clone()),
            s.admin.clone(),
            Path(s.bucket.clone()),
            Json(super::manage::NewKey {
                password: Some(crate::testutil::password().into()),
                code: None,
                description: description.into(),
                writable: false,
                expires_at: Some(expiry),
            }),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE s3_keys SET enabled=? WHERE id=?").bind(enabled).bind(key.key.id).execute(&s.env.st.db).await.unwrap();
    }
    super::expiry_notices(&s.env.st).await.unwrap();
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM notifications WHERE kind='access_expiring'").fetch_one(&s.env.st.db).await.unwrap();
    assert_eq!(count, 0);
    let expiry = now() + 86400;
    sqlx::query("UPDATE s3_keys SET expires_at=? WHERE id=?").bind(expiry).bind(&s.key.key.id).execute(&s.env.st.db).await.unwrap();
    super::expiry_notices(&s.env.st).await.unwrap();
    super::expiry_notices(&s.env.st).await.unwrap();
    let notices: Vec<(i64, String, String)> =
        sqlx::query_as("SELECT user_id,node_id,data FROM notifications WHERE kind='access_expiring'").fetch_all(&s.env.st.db).await.unwrap();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].0, s.admin.id);
    let identity = super::credentials::identity(&s.env.st, &s.key.key.access_key, false).await.unwrap();
    assert_eq!(notices[0].1, identity.root.id);
    let data: serde_json::Value = serde_json::from_str(&notices[0].2).unwrap();
    assert_eq!(data["name"], "S3: backups (Contract client)");
    assert_eq!(data["expires_at"], expiry);
    assert!(!notices[0].2.contains(&s.key.secret_key));
    assert!(!notices[0].2.contains(&s.key.key.access_key));
}

#[tokio::test]
async fn only_the_publisher_or_an_administrator_hands_out_keys_and_every_change_is_logged() {
    use axum::http::StatusCode as Status;
    let env = crate::testutil::env().await;
    let admin = env.admin().await;
    let manager = env.user("folder-manager", true).await;
    let st = || State(env.st.clone());
    crate::db::set_setting(
        &mut env.st.db.acquire().await.unwrap(),
        "s3",
        &serde_json::json!({"enabled":true,"hostname":"s3.example.com","region":"us-east-1","managers_publish":true}).to_string(),
    )
    .await
    .unwrap();
    let folder = env.folder(&admin, &env.st.shared_root().unwrap(), "Exports").await;
    env.grant(&folder, &manager, "manager").await;
    let password = || Some(crate::testutil::password().to_string());
    let key = |description: &str, password: Option<String>| {
        Json(super::manage::NewKey { password, code: None, description: description.into(), writable: true, expires_at: None })
    };

    // Making credentials asks who is acting
    let publish = |who: &crate::auth::User, name: &str, password: Option<String>| {
        super::manage::create(st(), who.clone(), Json(super::manage::Publish { password, code: None, node_id: folder.clone(), name: name.into() }))
    };
    assert!(publish(&admin, "exports", None).await.is_err());
    let Json(bucket) = publish(&admin, "exports", password()).await.unwrap();
    assert!(super::manage::new_key(st(), admin.clone(), Path(bucket.id.clone()), key("Backup", None)).await.is_err());
    let Json(made) = super::manage::new_key(st(), admin.clone(), Path(bucket.id.clone()), key("Backup", password())).await.unwrap();

    // A manager of the folder can't give out credentials that act as the administrator, nor open things up again
    let refused = |r: crate::error::AppResult<_>| matches!(r, Err(e) if e.status == Status::FORBIDDEN);
    assert!(refused(super::manage::new_key(st(), manager.clone(), Path(bucket.id.clone()), key("Mine", password())).await.map(|_| ())));
    let set_key = |who: &crate::auth::User, on: bool| {
        super::manage::revoke_key(st(), who.clone(), Path((bucket.id.clone(), made.key.id.clone())), Json(super::manage::Enabled { enabled: on }))
    };
    let set_bucket =
        |who: &crate::auth::User, on: bool| super::manage::update(st(), who.clone(), Path(bucket.id.clone()), Json(super::manage::Enabled { enabled: on }));
    let _ = set_key(&manager, false).await.unwrap();
    assert!(refused(set_key(&manager, true).await.map(|_| ())));
    let _ = set_key(&admin, true).await.unwrap();
    let _ = set_bucket(&manager, false).await.unwrap();
    assert!(refused(set_bucket(&manager, true).await.map(|_| ())));
    let _ = set_bucket(&admin, true).await.unwrap();

    // Their own bucket is theirs to hand out
    let Json(own) = publish(&manager, "manager-exports", password()).await.unwrap();
    let _ = super::manage::new_key(st(), manager.clone(), Path(own.id.clone()), key("Mine", password())).await.unwrap();
    // A writable key takes files as its creator: an account that may not change files gives read-only keys only, as
    // an FTP receive folder needs a publisher who may add files
    sqlx::query("UPDATE users SET can_write = 0 WHERE id = ?").bind(manager.id).execute(&env.st.db).await.unwrap();
    assert!(refused(super::manage::new_key(st(), manager.clone(), Path(own.id.clone()), key("Writer", password())).await.map(|_| ())));
    let reader = Json(super::manage::NewKey { password: password(), code: None, description: "Reader".into(), writable: false, expires_at: None });
    let _ = super::manage::new_key(st(), manager.clone(), Path(own.id.clone()), reader).await.unwrap();

    let details: Vec<(String,)> = sqlx::query_as("SELECT detail FROM activity WHERE action = 'settings' ORDER BY id").fetch_all(&env.st.db).await.unwrap();
    let details: Vec<&str> = details.iter().map(|(d,)| d.as_str()).collect();
    for expected in [
        "Published S3 bucket exports",
        "Made S3 key Backup to read and change bucket exports",
        "Stopped S3 key Backup",
        "Turned S3 key Backup on",
        "Stopped S3 bucket exports",
        "Turned S3 bucket exports on",
        "Published S3 bucket manager-exports",
    ] {
        assert!(details.contains(&expected), "{expected} not in {details:?}");
    }
}

#[tokio::test]
async fn a_stopped_key_and_a_skewed_clock_use_none_of_the_sign_in_limit_but_unknown_keys_do() {
    let s = server(false).await;
    let rate = format!("ip:{}", crate::auth::limit_key_ip("127.0.0.1"));
    // A backup job retrying with a key that was stopped: refused, but never counted, however often
    sqlx::query("UPDATE s3_keys SET enabled = 0 WHERE id = ?").bind(&s.key.key.id).execute(&s.env.st.db).await.unwrap();
    for _ in 0..(crate::auth::IP_FAIL_LIMIT + 5) {
        assert!(s.client.head_bucket().bucket("backups").send().await.is_err());
    }
    assert!(!crate::auth::attempts_exhausted(&s.env.st, &rate, 1), "a known key refused for another reason isn't a guess");
    // Nor does a request whose clock is far off
    let response = reqwest::Client::new()
        .get(format!("{}/backups", s.endpoint))
        .header("host", "127.0.0.1")
        .header("x-amz-date", "20000101T000000Z")
        .header(
            "authorization",
            format!("AWS4-HMAC-SHA256 Credential=TFX/20000101/us-east-1/s3/aws4_request, SignedHeaders=host;x-amz-date, Signature={}", "0".repeat(64)),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    assert!(!crate::auth::attempts_exhausted(&s.env.st, &rate, 1));
    // An unknown key is a guess, and counts
    let unknown = client(&s.endpoint, "TF00000000000000000000", "not-the-secret");
    assert!(unknown.head_bucket().bucket("backups").send().await.is_err());
    assert!(crate::auth::attempts_exhausted(&s.env.st, &rate, 1));
}

#[tokio::test]
async fn custom_metadata_such_as_rclones_md5_is_accepted_and_not_kept() {
    let s = server(false).await;
    s.client
        .put_object()
        .bucket("backups")
        .key("big.bin")
        .metadata("md5chksum", "XUFAKrxLKna5cZ2REBfFkg==")
        .metadata("mtime", "1700000000")
        .body(ByteStream::from_static(b"hello"))
        .send()
        .await
        .unwrap();
    let head = s.client.head_object().bucket("backups").key("big.bin").send().await.unwrap();
    assert_eq!(head.metadata().unwrap().keys().collect::<Vec<_>>(), ["mtime"]);
    // rclone sends it with large, multipart uploads
    s.client.create_multipart_upload().bucket("backups").key("large.bin").metadata("md5chksum", "XUFAKrxLKna5cZ2REBfFkg==").send().await.unwrap();
}

#[tokio::test]
async fn conditional_writes_create_only_once_and_replace_only_what_was_read() {
    let s = server(false).await;
    let put = |key: &'static str, body: &'static [u8]| s.client.put_object().bucket("backups").key(key).body(ByteStream::from_static(body));
    // A Terraform lock: made once; two at the same time, one of them
    let (a, b) = tokio::join!(put("terraform.tflock", b"a").if_none_match("*").send(), put("terraform.tflock", b"b").if_none_match("*").send());
    assert_eq!([a.is_ok(), b.is_ok()].iter().filter(|ok| **ok).count(), 1, "{a:?} {b:?}");
    let refused = put("terraform.tflock", b"c").if_none_match("*").send().await.unwrap_err();
    assert_eq!(refused.raw_response().unwrap().status().as_u16(), 412);
    // If-Match: only over the content of that ETag
    put("state.json", b"one").send().await.unwrap();
    let etag = s.client.head_object().bucket("backups").key("state.json").send().await.unwrap().e_tag().unwrap().to_string();
    let stale = put("state.json", b"two").if_match("\"0123456789abcdef0123456789abcdef\"").send().await.unwrap_err();
    assert_eq!(stale.raw_response().unwrap().status().as_u16(), 412);
    put("state.json", b"two").if_match(&etag).send().await.unwrap();
    assert_eq!(read(&s, "state.json").await.as_ref(), b"two");
    assert_eq!(put("state.json", b"three").if_match(&etag).send().await.unwrap_err().raw_response().unwrap().status().as_u16(), 412);
    assert_eq!(put("missing.json", b"x").if_match(&etag).send().await.unwrap_err().raw_response().unwrap().status().as_u16(), 404);
    // Completing a multipart upload too
    let upload = s.client.create_multipart_upload().bucket("backups").key("terraform.tflock").send().await.unwrap();
    let id = upload.upload_id().unwrap();
    let part =
        s.client.upload_part().bucket("backups").key("terraform.tflock").upload_id(id).part_number(1).body(ByteStream::from_static(b"d")).send().await.unwrap();
    let parts = CompletedMultipartUpload::builder().parts(CompletedPart::builder().part_number(1).e_tag(part.e_tag().unwrap()).build()).build();
    let complete = s.client.complete_multipart_upload().bucket("backups").key("terraform.tflock").upload_id(id).multipart_upload(parts).if_none_match("*");
    assert_eq!(complete.send().await.unwrap_err().raw_response().unwrap().status().as_u16(), 412);
}

#[tokio::test]
async fn deleting_a_folder_marker_removes_only_an_empty_folder() {
    let s = server(false).await;
    put(&s, "dir/a.txt", b"keep").await;
    s.client.delete_object().bucket("backups").key("dir/").send().await.unwrap();
    assert_eq!(read(&s, "dir/a.txt").await.as_ref(), b"keep", "a folder with something in it stays");
    s.client.delete_object().bucket("backups").key("dir/a.txt").send().await.unwrap();
    s.client.delete_object().bucket("backups").key("dir/").send().await.unwrap();
    assert!(s.client.head_object().bucket("backups").key("dir/").send().await.is_err(), "an empty one goes");
}

#[tokio::test]
async fn pages_of_more_than_1000_are_1000_as_in_aws() {
    let s = server(false).await;
    put(&s, "a.txt", b"a").await;
    let listed = s.client.list_objects_v2().bucket("backups").max_keys(5000).send().await.unwrap();
    assert_eq!((listed.max_keys(), listed.key_count()), (Some(1000), Some(1)));
}

#[tokio::test]
async fn a_listing_page_checks_access_per_request_not_per_object() {
    let s = server(false).await;
    let root = s.env.st.shared_root().unwrap();
    let published = crate::paths::child_named(&mut s.env.st.db.acquire().await.unwrap(), &root, "Published").await.unwrap().unwrap().id;
    for i in 0..300 {
        s.env.stored_file(&s.admin, &published, &format!("f{i:03}.txt"), b"x").await;
    }
    crate::tree::NODE_FOR_CALLS.with(|n| n.set(0));
    let listed = s.client.list_objects_v2().bucket("backups").send().await.unwrap();
    assert_eq!(listed.key_count(), Some(300));
    let calls = crate::tree::NODE_FOR_CALLS.with(|n| n.get());
    assert!(calls < 30, "node_for ran {calls} times for one page");
}

#[tokio::test]
async fn an_unusual_delimiter_lists_a_page_at_a_time_and_each_group_once() {
    let s = server(false).await;
    for i in 0..30 {
        put(&s, Box::leak(format!("a-{i:02}").into_boxed_str()), b"x").await;
    }
    put(&s, "b.txt", b"x").await;
    let _small = super::listing::small_budget(10);
    let (mut prefixes, mut keys, mut pages, mut token) = (Vec::new(), Vec::new(), 0, None::<String>);
    loop {
        let page = s.client.list_objects_v2().bucket("backups").delimiter("-").set_continuation_token(token.clone()).send().await.unwrap();
        pages += 1;
        prefixes.extend(page.common_prefixes().iter().filter_map(|p| p.prefix().map(str::to_string)));
        keys.extend(page.contents().iter().filter_map(|o| o.key().map(str::to_string)));
        token = page.next_continuation_token().map(str::to_string);
        if token.is_none() {
            break;
        }
    }
    assert_eq!((prefixes, keys), (vec!["a-".to_string()], vec!["b.txt".to_string()]));
    assert!(pages >= 3, "one request went through all of them: {pages} pages");
}

#[tokio::test]
async fn parts_are_received_while_another_part_of_the_upload_is_being_recorded() {
    let s = server(false).await;
    let upload = s.client.create_multipart_upload().bucket("backups").key("parallel.bin").send().await.unwrap();
    let id = upload.upload_id().unwrap().to_string();
    // Another part holds the upload's turn: this one still arrives, and waits only to be recorded
    let turn = super::multipart::lock(&id);
    let held = turn.lock().await;
    let (client, key, upload_id) = (s.client.clone(), "parallel.bin", id.clone());
    let sent = tokio::spawn(async move {
        client.upload_part().bucket("backups").key(key).upload_id(upload_id).part_number(1).body(ByteStream::from_static(b"part one")).send().await
    });
    let tmp = s.env.st.tmp_dir();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let received = std::fs::read_dir(&tmp)
                .unwrap()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with("s3-receive-") && e.metadata().is_ok_and(|m| m.len() == b"part one".len() as u64));
            if received {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the part is received while the upload's turn is held elsewhere");
    assert!(!sent.is_finished());
    drop(held);
    sent.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_completed_upload_is_stored_with_the_digests_worked_out_while_its_parts_are_joined() {
    use md5::Digest;
    let s = server(false).await;
    let first: &'static [u8] = Box::leak(vec![7u8; 5 * 1024 * 1024].into_boxed_slice());
    let upload = s.client.create_multipart_upload().bucket("backups").key("joined.bin").send().await.unwrap();
    let id = upload.upload_id().unwrap();
    let mut done = Vec::new();
    for (number, body) in [(1, first), (2, &b"tail"[..])] {
        let part =
            s.client.upload_part().bucket("backups").key("joined.bin").upload_id(id).part_number(number).body(ByteStream::from_static(body)).send().await.unwrap();
        done.push(CompletedPart::builder().part_number(number).e_tag(part.e_tag().unwrap()).build());
    }
    let parts = CompletedMultipartUpload::builder().set_parts(Some(done)).build();
    let out = s.client.complete_multipart_upload().bucket("backups").key("joined.bin").upload_id(id).multipart_upload(parts).send().await.unwrap();
    let whole = [first, b"tail"].concat();
    assert_eq!(out.e_tag().unwrap(), format!("\"{}\"", hex::encode(md5::Md5::digest(&whole))));
    assert_eq!(read(&s, "joined.bin").await.as_ref(), whole.as_slice());
    let root = s.env.st.shared_root().unwrap();
    let published = crate::paths::child_named(&mut s.env.st.db.acquire().await.unwrap(), &root, "Published").await.unwrap().unwrap().id;
    let node = crate::paths::child_named(&mut s.env.st.db.acquire().await.unwrap(), &published, "joined.bin").await.unwrap().unwrap();
    assert_eq!(node.blob_hash.unwrap(), hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&whole)));
}

#[tokio::test]
async fn etags_missing_in_published_buckets_are_worked_out_ahead() {
    let s = server(false).await;
    put(&s, "a.txt", b"hello").await;
    put(&s, "b.txt", b"world").await;
    sqlx::query("UPDATE nodes SET content_md5 = NULL, md5_version = NULL WHERE kind = 'file'").execute(&s.env.st.db).await.unwrap();
    assert_eq!(super::fill_digests(&s.env.st, 1 << 30).await.unwrap(), 2);
    let (missing,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE kind = 'file' AND content_md5 IS NULL").fetch_one(&s.env.st.db).await.unwrap();
    assert_eq!(missing, 0);
}

#[tokio::test]
async fn a_clock_out_of_step_is_told_from_x_amz_date_or_from_date_and_counts_for_nothing() {
    let s = server(false).await;
    let rate = format!("ip:{}", crate::auth::limit_key_ip("127.0.0.1"));
    let http = crate::util::http_date;
    let signed = |date_header: &str, value: String, credential_date: &str| {
        reqwest::Client::new()
            .get(format!("{}/backups", s.endpoint))
            .header("host", "127.0.0.1")
            .header(date_header, value)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=TFX/{credential_date}/us-east-1/s3/aws4_request, SignedHeaders=host;{date_header}, Signature={}",
                    "0".repeat(64)
                ),
            )
            .send()
    };
    // A clock an hour off, in either header: refused as a clock, and no guess at a key
    let day_of = |t: i64| jiff::Timestamp::from_second(t).unwrap().strftime("%Y%m%d").to_string();
    for (header, value, day) in [
        ("x-amz-date", "20000101T000000Z".to_string(), "20000101".to_string()),
        ("date", http(now() - 3600), day_of(now() - 3600)),
        ("date", http(now() + 3600), day_of(now() + 3600)),
    ] {
        let response = signed(header, value, &day).await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN, "{header}");
        assert!(response.text().await.unwrap().contains("RequestTimeTooSkewed"), "{header}");
        assert!(!crate::auth::attempts_exhausted(&s.env.st, &rate, 1), "{header}");
    }
    // The same request on time is a guess at an unknown key, and counts
    let day = jiff::Timestamp::from_second(now()).unwrap().strftime("%Y%m%d").to_string();
    let response = signed("date", http(now()), &day).await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    assert!(!response.text().await.unwrap().contains("RequestTimeTooSkewed"));
    assert!(crate::auth::attempts_exhausted(&s.env.st, &rate, 1));
}

#[tokio::test]
async fn administrators_find_buckets_by_their_name_creator_or_folder() {
    let s = server(false).await;
    let search = async |text: &str| {
        let q = axum::extract::Query(super::manage::ListQuery { search: Some(text.into()), ..Default::default() });
        let Json(list) = super::manage::list(State(s.env.st.clone()), s.admin.clone(), q).await.unwrap();
        list.buckets.len()
    };
    assert_eq!((search("backups").await, search(&s.admin.username.to_uppercase()).await, search("published").await, search("nothing").await), (1, 1, 1, 0));
}

/// The administrator's list comes a page at a time, newest first: every bucket once, also of several made in the
/// same second, each with its own keys, and its creator and folder looked up once for the page, not once a bucket
#[tokio::test]
async fn the_administrators_bucket_list_pages_without_skipping_any_and_with_each_buckets_keys() {
    let s = server(false).await;
    let (node, created): (String, i64) = sqlx::query_as("SELECT node_id, created_at FROM s3_buckets").fetch_one(&s.env.st.db).await.unwrap();
    for (id, name) in [("b-1", "same-second-one"), ("b-2", "same-second-two"), ("b-3", "same-second-three")] {
        sqlx::query("INSERT INTO s3_buckets(id, name, node_id, creator_id, created_at) VALUES(?, ?, ?, ?, ?)")
            .bind(id)
            .bind(name)
            .bind(&node)
            .bind(s.admin.id)
            .bind(created + 1)
            .execute(&s.env.st.db)
            .await
            .unwrap();
    }
    let page = async |after: Option<&super::manage::BucketInfo>| {
        let q = super::manage::ListQuery {
            after_created: after.map(|b| b.bucket.created_at),
            after_id: after.map(|b| b.bucket.id.clone()),
            limit: Some(2),
            ..Default::default()
        };
        super::manage::list(State(s.env.st.clone()), s.admin.clone(), axum::extract::Query(q)).await.unwrap().0.buckets
    };
    let first = page(None).await;
    let second = page(first.last()).await;
    let names: Vec<&str> = first.iter().chain(&second).map(|b| b.bucket.name.as_str()).collect();
    assert_eq!(names, ["same-second-three", "same-second-two", "same-second-one", "backups"]);
    assert!(page(second.last()).await.is_empty());
    let backups = second.iter().find(|b| b.bucket.name == "backups").unwrap();
    assert_eq!(backups.keys.len(), 1);
    assert_eq!(backups.folder, "Published");
    assert!(first.iter().all(|b| b.keys.is_empty() && b.folder == "Published" && b.creator == s.admin.username));
}

#[tokio::test]
async fn downloads_are_attachments_unless_a_signed_link_asks_for_the_response_headers_it_wants() {
    let s = server(false).await;
    put(&s, "page.html", b"<script>alert(1)</script>").await;
    let ask = |disposition: Option<&'static str>, content_type: Option<&'static str>, cache: Option<&'static str>| {
        let s = &s;
        async move {
            let mut request = s.client.get_object().bucket("backups").key("page.html");
            if let Some(disposition) = disposition {
                request = request.response_content_disposition(disposition);
            }
            if let Some(content_type) = content_type {
                request = request.response_content_type(content_type);
            }
            if let Some(cache) = cache {
                request = request.response_cache_control(cache);
            }
            let signed = request.presigned(PresigningConfig::expires_in(std::time::Duration::from_secs(300)).unwrap()).await.unwrap();
            reqwest::get(signed.uri()).await.unwrap()
        }
    };
    // Plain: an attachment, and the browser is not left to guess a type
    let plain = ask(None, None, None).await;
    assert_eq!(plain.status(), reqwest::StatusCode::OK);
    assert_eq!(plain.headers()["content-disposition"], "attachment");
    assert_eq!(plain.headers()["x-content-type-options"], "nosniff");
    assert_eq!(plain.text().await.unwrap(), "<script>alert(1)</script>");
    // A file name and another type for what is downloaded
    let named = ask(Some("attachment; filename=\"report.html\""), Some("text/plain"), Some("no-store")).await;
    assert_eq!(named.status(), reqwest::StatusCode::OK);
    assert_eq!(named.headers()["content-disposition"], "attachment; filename=\"report.html\"");
    assert_eq!(named.headers()["content-type"], "text/plain");
    assert_eq!(named.headers()["cache-control"], "no-store");
    // As in S3, a link that was signed with `inline` gets it (only the holder of the key can sign that); the type is still not guessed
    let inline = ask(Some("inline"), Some("text/html"), None).await;
    assert_eq!(inline.status(), reqwest::StatusCode::OK);
    assert_eq!(inline.headers()["content-disposition"], "inline");
    assert_eq!(inline.headers()["content-type"], "text/html");
    assert_eq!(inline.headers()["x-content-type-options"], "nosniff");
    // The same parameters on a HEAD request, and a changed parameter is a bad signature
    let signed = s
        .client
        .head_object()
        .bucket("backups")
        .key("page.html")
        .response_content_disposition("inline")
        .presigned(PresigningConfig::expires_in(std::time::Duration::from_secs(300)).unwrap())
        .await
        .unwrap();
    let head = reqwest::Client::new().head(signed.uri()).send().await.unwrap();
    assert_eq!((head.status(), head.headers()["content-disposition"].to_str().unwrap()), (reqwest::StatusCode::OK, "inline"));
    let signed = s
        .client
        .get_object()
        .bucket("backups")
        .key("page.html")
        .response_content_disposition("attachment")
        .presigned(PresigningConfig::expires_in(std::time::Duration::from_secs(300)).unwrap())
        .await
        .unwrap();
    let changed = signed.uri().replace("response-content-disposition=attachment", "response-content-disposition=inline");
    assert_ne!(changed, signed.uri());
    assert_eq!(reqwest::get(changed).await.unwrap().status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_copy_never_reads_from_another_bucket_and_settings_that_are_not_kept_are_refused() {
    let s = server(false).await;
    put(&s, "own.txt", b"mine").await;
    // The source has to be in the key's own bucket, whatever the other bucket is called
    for source in ["other-bucket/own.txt", "backups-2/own.txt", "/other/own.txt"] {
        let error = s.client.copy_object().bucket("backups").key("stolen.txt").copy_source(source).send().await.unwrap_err();
        assert_eq!(error.raw_response().unwrap().status().as_u16(), 403, "{source}");
    }
    assert!(s.client.head_object().bucket("backups").key("stolen.txt").send().await.is_err());
    // Copying a folder is no copy
    s.client.put_object().bucket("backups").key("folder/").body(ByteStream::from_static(b"")).send().await.unwrap();
    assert!(s.client.copy_object().bucket("backups").key("folder-copy").copy_source("backups/folder/").send().await.is_err());
    // Settings this server does not keep are refused, not taken and forgotten
    let acl =
        s.client.put_object().bucket("backups").key("acl.txt").acl(aws_sdk_s3::types::ObjectCannedAcl::PublicRead).body(ByteStream::from_static(b"x")).send().await;
    assert_eq!(acl.unwrap_err().raw_response().unwrap().status().as_u16(), 501);
    let encrypted = s
        .client
        .put_object()
        .bucket("backups")
        .key("sse.txt")
        .server_side_encryption(aws_sdk_s3::types::ServerSideEncryption::Aes256)
        .body(ByteStream::from_static(b"x"))
        .send()
        .await;
    assert_eq!(encrypted.unwrap_err().raw_response().unwrap().status().as_u16(), 501);
    assert!(s.client.head_object().bucket("backups").key("acl.txt").send().await.is_err());
}

#[tokio::test]
async fn a_marker_or_start_after_that_is_a_group_does_not_list_the_group_again() {
    let s = server(false).await;
    for key in ["a/1.txt", "a/2.txt", "b.txt", "c/1.txt"] {
        put(&s, key, b"x").await;
    }
    // ListObjects: the marker is the last key or group of the page before, so a group it ends is done
    let v1 = s.client.list_objects().bucket("backups").delimiter("/").marker("a/").send().await.unwrap();
    assert_eq!(v1.common_prefixes().iter().map(|p| p.prefix().unwrap()).collect::<Vec<_>>(), ["c/"]);
    assert_eq!(v1.contents().iter().map(|o| o.key().unwrap()).collect::<Vec<_>>(), ["b.txt"]);
    // ListObjectsV2 with start-after: the same
    let v2 = s.client.list_objects_v2().bucket("backups").delimiter("/").start_after("a/").send().await.unwrap();
    assert_eq!(v2.common_prefixes().iter().map(|p| p.prefix().unwrap()).collect::<Vec<_>>(), ["c/"]);
    assert_eq!(v2.contents().iter().map(|o| o.key().unwrap()).collect::<Vec<_>>(), ["b.txt"]);
}

#[tokio::test]
async fn a_body_that_fails_its_checks_is_refused_with_the_code_of_the_check_and_nothing_is_kept() {
    use aws_sdk_s3::error::ProvideErrorMetadata;
    let s = server(false).await;
    // A Content-MD5 that is not that of the data
    let error = s
        .client
        .put_object()
        .bucket("backups")
        .key("a.txt")
        .content_md5("AAAAAAAAAAAAAAAAAAAAAA==")
        .body(ByteStream::from_static(b"wrong"))
        .send()
        .await
        .unwrap_err();
    assert_eq!(error.as_service_error().and_then(|e| e.code()), Some("BadDigest"));
    // The same for a part of an upload
    let upload = s.client.create_multipart_upload().bucket("backups").key("m.bin").send().await.unwrap();
    let error = s
        .client
        .upload_part()
        .bucket("backups")
        .key("m.bin")
        .upload_id(upload.upload_id().unwrap())
        .part_number(1)
        .content_md5("AAAAAAAAAAAAAAAAAAAAAA==")
        .body(ByteStream::from_static(b"wrong"))
        .send()
        .await
        .unwrap_err();
    assert_eq!(error.as_service_error().and_then(|e| e.code()), Some("BadDigest"));
    assert_eq!(s.client.list_parts().bucket("backups").key("m.bin").upload_id(upload.upload_id().unwrap()).send().await.unwrap().parts().len(), 0);
    // A checksum of the data that is not the one sent
    let error = s
        .client
        .put_object()
        .bucket("backups")
        .key("c.txt")
        .checksum_sha256("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
        .body(ByteStream::from_static(b"wrong"))
        .send()
        .await
        .unwrap_err();
    assert_eq!(error.as_service_error().and_then(|e| e.code()), Some("BadDigest"));
    for key in ["a.txt", "c.txt"] {
        assert!(s.client.head_object().bucket("backups").key(key).send().await.is_err(), "{key}");
    }
}

#[tokio::test]
async fn current_clients_send_a_checksum_in_a_trailer_and_it_is_checked_for_whole_objects_and_for_parts() {
    use aws_sdk_s3::{error::ProvideErrorMetadata, types::ChecksumAlgorithm};
    let s = server(false).await;
    // What the AWS SDKs and the CLI do by default now: the body goes in aws-chunked pieces, with its checksum after them
    let current = Client::from_conf(
        aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new("us-east-1"))
            .credentials_provider(Credentials::new(&s.key.key.access_key, &s.key.secret_key, None, None, "contract"))
            .endpoint_url(&s.endpoint)
            .force_path_style(true)
            .request_checksum_calculation(RequestChecksumCalculation::WhenSupported)
            .build(),
    );
    let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 253) as u8).collect();
    let out = current.put_object().bucket("backups").key("trailer.bin").body(ByteStream::from(data.clone())).send().await.unwrap();
    assert!(out.checksum_crc32().is_some(), "the checksum is given back");
    assert_eq!(read(&s, "trailer.bin").await.as_ref(), data.as_slice());
    let (hashed,): (String,) = sqlx::query_as("SELECT content_md5 FROM nodes WHERE name = 'trailer.bin'").fetch_one(&s.env.st.db).await.unwrap();
    assert_eq!(out.e_tag(), Some(format!("\"{hashed}\"").as_str()));
    // Parts with a checksum each, put together
    let upload = current.create_multipart_upload().bucket("backups").key("parts.bin").checksum_algorithm(ChecksumAlgorithm::Crc32).send().await.unwrap();
    let id = upload.upload_id().unwrap();
    let (first, second) = (vec![7u8; 5 * 1024 * 1024], vec![9u8; 300_000]);
    let mut done = Vec::new();
    for (number, body) in [(1, first.clone()), (2, second.clone())] {
        let part = current
            .upload_part()
            .bucket("backups")
            .key("parts.bin")
            .upload_id(id)
            .part_number(number)
            .checksum_algorithm(ChecksumAlgorithm::Crc32)
            .body(ByteStream::from(body))
            .send()
            .await
            .unwrap();
        done.push(CompletedPart::builder().part_number(number).e_tag(part.e_tag().unwrap()).set_checksum_crc32(part.checksum_crc32().map(str::to_string)).build());
    }
    current
        .complete_multipart_upload()
        .bucket("backups")
        .key("parts.bin")
        .upload_id(id)
        .multipart_upload(CompletedMultipartUpload::builder().set_parts(Some(done)).build())
        .send()
        .await
        .unwrap();
    assert_eq!(read(&s, "parts.bin").await.as_ref(), [first, second].concat().as_slice());
    // A checksum that is not that of the data is refused and nothing is kept
    let wrong =
        current.put_object().bucket("backups").key("wrong.bin").checksum_crc32("AAAAAA==").body(ByteStream::from_static(b"not that")).send().await.unwrap_err();
    assert_eq!(wrong.as_service_error().and_then(|e| e.code()), Some("BadDigest"));
    assert!(s.client.head_object().bucket("backups").key("wrong.bin").send().await.is_err());
}
