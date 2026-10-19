//! Account snapshots are confined to a trash transaction; node permissions and deadlines remain live.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::http::StatusCode;

use crate::{auth, db, fsops, testutil, tree, util::now};

use super::{trash_ids, trash_one};

async fn unchanged(env: &testutil::TestEnv, ids: &[String]) {
    let (trashed,): (i64,) = sqlx::query_as("SELECT count(*) FROM nodes WHERE id IN (SELECT value FROM json_each(?)) AND trashed_at IS NOT NULL")
        .bind(serde_json::to_string(ids).unwrap())
        .fetch_one(&env.st.db)
        .await
        .unwrap();
    assert_eq!(trashed, 0);
    let (logged,): (i64,) = sqlx::query_as("SELECT count(*) FROM activity WHERE action = 'trash'").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(logged, 0, "a refused batch must not retain activity for its rolled-back items");
}

#[tokio::test]
async fn a_queued_trash_refreshes_account_and_grants_after_obtaining_the_write_lock() {
    for changed in ["can_delete", "disabled", "guest", "grant"] {
        let env = testutil::env().await;
        let owner = env.user("owner", true).await;
        let user = env.user("editor", true).await;
        let shared = env.folder(&owner, owner.root(), "Shared").await;
        env.grant(&shared, &user, "editor").await;
        let ids = vec![env.file(&owner, &shared, "First.txt").await, env.file(&owner, &shared, "Second.txt").await];
        let (actor, _) = env.sign_in(&user, "Queued trash test").await;
        assert!(actor.shared_root.is_some(), "the ordinary browser's company alias must be retained");
        let writer = env.st.write_lock.lock().await;
        let (started, queued) = tokio::sync::oneshot::channel();
        let request = {
            let (st, ids) = (env.st.clone(), ids.clone());
            tokio::spawn(async move {
                started.send(()).unwrap();
                trash_ids(&st, &actor, &ids, false).await
            })
        };
        queued.await.unwrap();
        match changed {
            "can_delete" => {
                sqlx::query("UPDATE users SET can_delete = 0 WHERE id = ?").bind(user.id).execute(&env.st.db).await.unwrap();
            }
            "disabled" => {
                sqlx::query("UPDATE users SET disabled = 1 WHERE id = ?").bind(user.id).execute(&env.st.db).await.unwrap();
            }
            "guest" => {
                sqlx::query("UPDATE users SET guest = 1, can_share = 0 WHERE id = ?").bind(user.id).execute(&env.st.db).await.unwrap();
            }
            "grant" => env.revoke(&shared, &user).await,
            _ => unreachable!(),
        }
        drop(writer);
        let result = request.await.unwrap();
        assert!(result.is_err(), "the pre-lock actor must not retain {changed}");
        unchanged(&env, &ids).await;
    }
}

#[tokio::test]
async fn a_trash_snapshot_never_gains_an_accounts_new_permissions() {
    let env = testutil::env().await;
    let mut actor = env.user("reader", true).await;
    let ids = vec![env.file(&actor, actor.root(), "First.txt").await];
    actor.can_delete = false;
    let error = trash_ids(&env.st, &actor, &ids, false).await.unwrap_err();
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    unchanged(&env, &ids).await;
}

#[tokio::test]
async fn a_grant_expiring_between_items_refuses_the_second_and_rolls_back_the_first() {
    let env = testutil::env().await;
    let owner = env.user("owner", true).await;
    let user = env.user("editor", true).await;
    let parent = env.folder(&owner, owner.root(), "Shared").await;
    env.grant(&parent, &user, "editor").await;
    let ids = vec![env.file(&owner, &parent, "First.txt").await, env.file(&owner, &parent, "Second.txt").await];
    let deadline = now() + 2;
    sqlx::query("UPDATE grants SET expires_at = ? WHERE node_id = ? AND principal_type = 'user' AND principal_id = ?")
        .bind(deadline)
        .bind(&parent)
        .bind(user.id)
        .execute(&env.st.db)
        .await
        .unwrap();
    let locks = fsops::lock_seen(&env.st, &user, &ids.iter().map(String::as_str).collect::<Vec<_>>()).await.unwrap();
    let writer = env.st.write_lock.lock().await;
    let mut tx = db::begin_write(&env.st.db).await.unwrap();
    let mut access = tree::TrashAccess::new(&mut tx, &user).await.unwrap();
    let first = access.node(&ids[0]).await.unwrap();
    let (conn, actor) = access.parts();
    trash_one(conn, actor, &locks, &first, "").await.unwrap();
    assert_eq!(access.node(&ids[0]).await.unwrap_err().status, StatusCode::NOT_FOUND, "a node trashed by this transaction must not be cached");
    // Wait for the actual wall-clock deadline, rather than changing a grant behind the snapshot or freezing now().
    tokio::time::timeout(Duration::from_secs(4), async {
        while now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(access.node(&ids[1]).await.unwrap_err().status, StatusCode::NOT_FOUND);
    drop(access);
    tx.rollback().await.unwrap();
    drop(writer);
    drop(locks);
    unchanged(&env, &ids).await;
}

#[tokio::test]
async fn mixed_trash_batches_preserve_personal_restricted_contributor_and_read_only_boundaries() {
    for boundary in ["personal", "restricted", "contributor", "read_only"] {
        let env = testutil::env().await;
        let owner = env.user("owner", true).await;
        let user = env.user("editor", true).await;
        let first = env.file(&user, user.root(), "First.txt").await;
        let parent = env.folder(&owner, owner.root(), "Shared").await;
        let forbidden = match boundary {
            "personal" => env.file(&owner, &parent, "Private.txt").await,
            "restricted" => {
                env.grant(&parent, &user, "editor").await;
                let restricted = env.folder(&owner, &parent, "Restricted").await;
                sqlx::query("UPDATE nodes SET restricted = 1 WHERE id = ?").bind(&restricted).execute(&env.st.db).await.unwrap();
                parent.clone()
            }
            "contributor" => {
                env.grant(&parent, &user, "contributor").await;
                let own = env.folder(&user, &parent, "Own folder").await;
                env.file(&owner, &own, "Someone else's.txt").await;
                own
            }
            "read_only" => {
                env.grant(&parent, &user, "editor").await;
                sqlx::query("UPDATE drives SET read_only = 1 WHERE root_id = ?").bind(owner.root()).execute(&env.st.db).await.unwrap();
                parent.clone()
            }
            _ => unreachable!(),
        };
        let ids = vec![first, forbidden];
        assert!(trash_ids(&env.st, &user, &ids, false).await.is_err(), "{boundary}");
        unchanged(&env, &ids).await;
    }
}

#[tokio::test]
async fn protocol_markers_keep_the_original_trash_credential_and_root_validators() {
    let env = testutil::env().await;
    let user = env.user("editor", true).await;
    let root = env.folder(&user, user.root(), "Different scope").await;
    let ids = vec![env.file(&user, user.root(), "First.txt").await];
    for protocol in ["root", "s3", "sftp", "ftp"] {
        let mut actor = user.clone();
        match protocol {
            "root" => actor.protocol_root = Some(root.clone()),
            "s3" => {
                actor.protocol_key = Some("test-revoked-key".into());
            }
            "sftp" => actor.sftp_credential = Some(auth::sftp::SftpCredential::Key("test-revoked-key".into())),
            "ftp" => {
                actor.ftp_credential = Some(crate::ftp::credentials::Credential {
                    id: "test-revoked-folder".into(),
                    generation: 1,
                    alive: Arc::new(AtomicBool::new(false)),
                    transfer: None,
                });
            }
            _ => unreachable!(),
        }
        assert!(trash_ids(&env.st, &actor, &ids, false).await.is_err(), "{protocol} must use its current validator");
        unchanged(&env, &ids).await;
    }
}

#[tokio::test]
async fn an_activity_scoped_actor_retains_per_item_account_validation() {
    let env = testutil::env().await;
    let mut user = env.user("editor", true).await;
    user.activity_source = Some("Test protocol".into());
    let id = env.file(&user, user.root(), "First.txt").await;
    let writer = env.st.write_lock.lock().await;
    let mut tx = db::begin_write(&env.st.db).await.unwrap();
    let mut access = tree::TrashAccess::new(&mut tx, &user).await.unwrap();
    // An internal scope may itself change account state. Such contexts must not use the ordinary HTTP snapshot.
    sqlx::query("UPDATE users SET disabled = 1 WHERE id = ?").bind(user.id).execute(access.parts().0).await.unwrap();
    assert_eq!(access.node(&id).await.unwrap_err().status, StatusCode::UNAUTHORIZED);
    drop(access);
    tx.rollback().await.unwrap();
    drop(writer);
    unchanged(&env, &[id]).await;
}

#[tokio::test]
async fn an_administrators_batch_does_not_inherit_access_to_another_personal_space() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let user = env.user("private_owner", true).await;
    let ids = vec![env.file(&admin, admin.root(), "Own.txt").await, env.file(&user, user.root(), "Private.txt").await];
    assert_eq!(trash_ids(&env.st, &admin, &ids, false).await.unwrap_err().status, StatusCode::NOT_FOUND);
    unchanged(&env, &ids).await;
}

#[tokio::test]
async fn a_refused_folder_space_batch_restores_the_first_files_disk_bytes_and_index() {
    let env = testutil::folders_env().await;
    let user = env.user("editor", true).await;
    let other = env.user("private_owner", true).await;
    let first = env.upload(&user, user.root(), "First.txt", b"preserved disk bytes").await;
    let second = env.upload(&other, other.root(), "Private.txt", b"private disk bytes").await;
    let mut conn = env.st.db.acquire().await.unwrap();
    let path = tree::get_node(&mut conn, &first).await.unwrap().unwrap().fs_file().unwrap();
    drop(conn);
    let ids = vec![first, second];
    assert_eq!(trash_ids(&env.st, &user, &ids, false).await.unwrap_err().status, StatusCode::NOT_FOUND);
    assert_eq!(std::fs::read(path).unwrap(), b"preserved disk bytes");
    unchanged(&env, &ids).await;
}

#[tokio::test]
async fn ftp_cancellation_flags_are_rechecked_after_the_trash_context_is_created() {
    let env = testutil::env().await;
    let mut admin = env.admin().await;
    let company = admin.shared_root.as_ref().unwrap();
    let folder = env.folder(&admin, company, "Receive").await;
    let id = env.file(&admin, &folder, "First.txt").await;
    let mut conn = env.st.db.acquire().await.unwrap();
    db::set_setting(&mut conn, "ftp", r#"{"enabled":true}"#).await.unwrap();
    sqlx::query("INSERT INTO ftp_receive_folders (id,node_id,creator_id,name,username,password_hash,created_at) VALUES ('test-folder',?,?,'Test folder','test-user','test-unused-hash',?)")
        .bind(&folder).bind(admin.id).bind(now()).execute(&mut *conn).await.unwrap();
    drop(conn);
    let alive = Arc::new(AtomicBool::new(true));
    let transfer = Arc::new(AtomicBool::new(true));
    admin.protocol_root = Some(folder);
    admin.ftp_credential =
        Some(crate::ftp::credentials::Credential { id: "test-folder".into(), generation: 1, alive: alive.clone(), transfer: Some(transfer.clone()) });
    let writer = env.st.write_lock.lock().await;
    let mut tx = db::begin_write(&env.st.db).await.unwrap();
    let mut access = tree::TrashAccess::new(&mut tx, &admin).await.unwrap();
    let error = access.node(&id).await.unwrap_err();
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    assert_eq!(error.message, "FTP receive folders only accept uploads");
    alive.store(false, Ordering::Release);
    assert_eq!(access.node(&id).await.unwrap_err().status, StatusCode::UNAUTHORIZED);
    alive.store(true, Ordering::Release);
    transfer.store(false, Ordering::Release);
    assert_eq!(access.node(&id).await.unwrap_err().status, StatusCode::UNAUTHORIZED);
    drop(access);
    tx.rollback().await.unwrap();
    drop(writer);
    unchanged(&env, &[id]).await;
}
