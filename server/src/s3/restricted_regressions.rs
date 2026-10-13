//! A bucket key never inherits its privileged publisher's access across a restriction.

use super::{client, put, read, server};
use aws_sdk_s3::{error::ProvideErrorMetadata, primitives::ByteStream};
use axum::{
    Json,
    extract::{Path, State},
};

#[tokio::test]
async fn parent_bucket_hides_restrictions_and_only_explicit_child_publication_can_open_them() {
    let s = server(false).await;
    put(&s, "HR/payroll.txt", b"confidential").await;
    put(&s, "minutes.txt", b"public").await;
    let root: (String,) = sqlx::query_as("SELECT node_id FROM s3_buckets WHERE id=?").bind(&s.bucket).fetch_one(&s.env.st.db).await.unwrap();
    let folder = crate::paths::child_named(&mut *s.env.st.db.acquire().await.unwrap(), &root.0, "HR").await.unwrap().unwrap();
    let req = serde_json::from_value(serde_json::json!({"restricted":true,"expected":false,"confirmed":true})).unwrap();
    let _ = crate::restrictions::change(State(s.env.st.clone()), s.admin.clone(), Path(folder.id.clone()), Json(req)).await.unwrap();
    for delimiter in ["", "/"] {
        let page = s.client.list_objects_v2().bucket("backups").delimiter(delimiter).send().await.unwrap();
        assert_eq!(page.contents().iter().map(|o| o.key().unwrap()).collect::<Vec<_>>(), vec!["minutes.txt"]);
        assert!(page.common_prefixes().is_empty());
        assert_eq!(page.key_count(), Some(1));
        let page = s.client.list_objects().bucket("backups").delimiter(delimiter).send().await.unwrap();
        assert_eq!(page.contents().iter().map(|o| o.key().unwrap()).collect::<Vec<_>>(), vec!["minutes.txt"]);
        assert!(page.common_prefixes().is_empty());
    }
    let error = s.client.get_object().bucket("backups").key("HR/payroll.txt").send().await.unwrap_err();
    assert_eq!(error.as_service_error().and_then(|e| e.code()), Some("NoSuchKey"));
    assert!(s.client.head_object().bucket("backups").key("HR/payroll.txt").send().await.is_err());
    assert!(s.client.put_object().bucket("backups").key("HR/new.txt").body(ByteStream::from_static(b"must not appear")).send().await.is_err());
    assert!(s.client.copy_object().bucket("backups").key("minutes.txt").copy_source("backups/HR/payroll.txt").send().await.is_err());
    assert_eq!(&read(&s, "minutes.txt").await[..], b"public");
    let Json(bucket) =
        crate::s3::manage::create(State(s.env.st.clone()), s.admin.clone(), Json(crate::s3::manage::Publish { node_id: folder.id, name: "private-hr".into() }))
            .await
            .unwrap();
    let Json(key) = crate::s3::manage::new_key(
        State(s.env.st.clone()),
        s.admin.clone(),
        Path(bucket.id),
        Json(crate::s3::manage::NewKey { description: "Explicit HR scope".into(), writable: true, expires_at: None }),
    )
    .await
    .unwrap();
    let child = client(&s.endpoint, &key.key.access_key, &key.secret_key);
    let result = child.get_object().bucket("private-hr").key("payroll.txt").send().await.unwrap();
    assert_eq!(&result.body.collect().await.unwrap().into_bytes()[..], b"confidential");
}
