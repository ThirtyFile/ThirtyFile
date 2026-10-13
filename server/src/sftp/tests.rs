use crate::{
    auth::User,
    testutil::{self, TestEnv},
    util::{now, random_token, sha256_hex},
};
use russh::{client, keys::PublicKeyOrCertificate};
use russh_sftp::{client::SftpSession, protocol::OpenFlags};
use std::sync::Arc;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

struct Client;

#[tokio::test]
async fn restricted_folders_are_hidden_from_native_listings_and_open_handles_recheck_access() {
    let env = testutil::env().await;
    let owner = env.user("owner", true).await;
    let member = env.user("member", true).await;
    let folder = env.folder(&owner, owner.root(), "Department").await;
    let hidden = env.folder(&owner, &folder, "HR").await;
    let secret = env.upload(&owner, &hidden, "payroll.txt", b"confidential").await;
    env.upload(&owner, &folder, "minutes.txt", b"public").await;
    env.grant(&folder, &member, "editor").await;
    let _listener = on(&env).await;
    let credential = token(&env, &member, "write").await;
    let (_ssh, sftp) = client(&env, &member, &credential).await;
    let mut opened = sftp.open("/Shared with me/Department/HR/payroll.txt").await.unwrap();
    let req = serde_json::from_value(serde_json::json!({"restricted":true,"expected":false,"confirmed":true})).unwrap();
    let _ = crate::restrictions::change(axum::extract::State(env.st.clone()), owner.clone(), axum::extract::Path(hidden.clone()), axum::Json(req)).await.unwrap();
    let names = sftp.read_dir("/Shared with me/Department").await.unwrap().map(|e| e.file_name()).collect::<Vec<_>>();
    assert_eq!(names, vec!["minutes.txt"]);
    assert!(sftp.metadata("/Shared with me/Department/HR").await.is_err());
    assert!(sftp.open("/Shared with me/Department/HR/payroll.txt").await.is_err());
    use tokio::io::AsyncReadExt;
    assert!(opened.read(&mut [0; 32]).await.is_err());
    assert!(sftp.rename("/Shared with me/Department/HR", "/Shared with me/Department/renamed").await.is_err());
    assert!(sftp.remove_file("/Shared with me/Department/HR/payroll.txt").await.is_err());
    env.grant(&hidden, &member, "viewer").await;
    let bytes = sftp.read("/Shared with me/Department/HR/payroll.txt").await.unwrap();
    assert_eq!(&bytes[..], b"confidential");
    env.revoke(&hidden, &member).await;
    assert!(sftp.read("/Shared with me/Department/HR/payroll.txt").await.is_err());
    assert!(crate::tree::get_node(&mut env.st.db.acquire().await.unwrap(), &secret).await.unwrap().is_some());
    sftp.close().await.unwrap();
}

impl client::Handler for Client {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        Ok(true)
    }
}
async fn token(env: &TestEnv, user: &User, scope: &str) -> String {
    let id = random_token(crate::auth::app_passwords::ID_LEN);
    let token = format!("tfa_{id}_{}", random_token(40));
    sqlx::query("INSERT INTO app_passwords(id,user_id,name,token_hash,scope,created_at) VALUES(?,?,?,?,?,?)")
        .bind(id)
        .bind(user.id)
        .bind("SFTP test")
        .bind(sha256_hex(token.as_bytes()))
        .bind(scope)
        .bind(now())
        .execute(&env.st.db)
        .await
        .unwrap();
    token
}
async fn on(env: &TestEnv) -> super::Listener {
    for key in ["sftp_enabled", "sftp_keys_enabled"] {
        crate::db::set_setting(&mut env.st.db.acquire().await.unwrap(), key, "1").await.unwrap();
    }
    super::start(env.st.clone(), "127.0.0.1:0").await.unwrap()
}
async fn connection(env: &TestEnv) -> client::Handle<Client> {
    let address = env.st.part::<super::Memory>().address.read().clone();
    client::connect(Arc::new(client::Config::default()), address, Client).await.unwrap()
}
async fn client(env: &TestEnv, user: &User, token: &str) -> (client::Handle<Client>, SftpSession) {
    let mut ssh = connection(env).await;
    assert!(ssh.authenticate_password(&user.username, token).await.unwrap().success());
    let channel = ssh.channel_open_session().await.unwrap();
    channel.request_subsystem(true, "sftp").await.unwrap();
    let sftp = SftpSession::new(channel.into_stream()).await.unwrap();
    sftp.set_timeout(5);
    (ssh, sftp)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_commit_on_close_offset_writes_keep_versions_and_delete_goes_to_trash() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let token = token(&env, &amy, "write").await;
    let _server = on(&env).await;
    let (_ssh, sftp) = client(&env, &amy, &token).await;
    assert_eq!(sftp.canonicalize(".").await.unwrap(), "/");
    sftp.create_dir("/My files/Reports").await.unwrap();
    let path = "/My files/Reports/a.txt";
    let mut file = sftp.create(path).await.unwrap();
    file.write_all(b"hello world").await.unwrap();
    assert!(!sftp.try_exists(path).await.unwrap(), "not visible until close");
    file.shutdown().await.unwrap();
    assert_eq!(sftp.read(path).await.unwrap(), b"hello world");
    let mut file = sftp.open_with_flags(path, OpenFlags::WRITE).await.unwrap();
    file.seek(std::io::SeekFrom::Start(6)).await.unwrap();
    file.write_all(b"SFTP!").await.unwrap();
    file.shutdown().await.unwrap();
    assert_eq!(sftp.read(path).await.unwrap(), b"hello SFTP!");
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM node_versions").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(count, 1);
    let (detail,): (String,) = sqlx::query_as("SELECT detail FROM activity WHERE action = 'edit' ORDER BY id DESC LIMIT 1").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(detail, "through SFTP");
    sftp.rename(path, "/My files/Reports/b.txt").await.unwrap();
    sftp.remove_file("/My files/Reports/b.txt").await.unwrap();
    let (trash,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE trashed_at IS NOT NULL").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(trash, 1);
    let mut empty = sftp.create("/My files/empty.txt").await.unwrap();
    empty.shutdown().await.unwrap();
    assert_eq!(sftp.read("/My files/empty.txt").await.unwrap(), b"");
    assert!(sftp.symlink("/My files/link", "outside").await.is_err());
    assert!(sftp.canonicalize("../../etc").await.is_err());
    sftp.close().await.unwrap();
    _ssh.disconnect(russh::Disconnect::ByApplication, "Test complete", "en").await.unwrap();
    _server.stop();
    _server.finish().await;
    drop(empty);
    drop(file);
    drop(sftp);
    drop(_ssh);
    drop(amy);
    tokio::time::timeout(std::time::Duration::from_secs(5), env.st.db.close()).await.expect("SFTP pool closes after all sessions end");
    drop(env);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_admin_credentials_never_write_and_revoked_open_files_are_discarded() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let writable = token(&env, &admin, "write").await;
    let read = token(&env, &admin, "read").await;
    let _server = on(&env).await;
    let (_ssh, sftp) = client(&env, &admin, &writable).await;
    let mut initial = sftp.create("/My files/a.txt").await.unwrap();
    initial.write_all(b"original").await.unwrap();
    initial.shutdown().await.unwrap();
    let (_rssh, reader) = client(&env, &admin, &read).await;
    assert_eq!(reader.read("/My files/a.txt").await.unwrap(), b"original");
    assert!(reader.create("/My files/new.txt").await.is_err());
    assert!(reader.open_with_flags("/My files/a.txt", OpenFlags::CREATE | OpenFlags::TRUNCATE).await.is_err());
    assert!(reader.rename("/My files/a.txt", "/My files/b.txt").await.is_err());
    assert!(reader.remove_file("/My files/a.txt").await.is_err());
    assert!(reader.create_dir("/My files/New").await.is_err());
    let mut file = sftp.create("/My files/a.txt").await.unwrap();
    file.write_all(b"replacement").await.unwrap();
    file.flush().await.unwrap();
    sqlx::query("DELETE FROM app_passwords WHERE token_hash = ?").bind(sha256_hex(writable.as_bytes())).execute(&env.st.db).await.unwrap();
    assert!(file.shutdown().await.is_err());
    assert_eq!(reader.read("/My files/a.txt").await.unwrap(), b"original");
    assert_eq!(std::fs::read_dir(env.st.tmp_dir()).unwrap().count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_key_is_persisted_and_ssh_keys_are_revoked_during_a_session() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let server = on(&env).await;
    let first = env.st.part::<super::Memory>().fingerprint.read().clone();
    server.stop();
    drop(server);
    let _server = on(&env).await;
    assert_eq!(first, *env.st.part::<super::Memory>().fingerprint.read());
    let private = russh::keys::PrivateKey::from(russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&rand::random()));
    let public = private.public_key().clone();
    let fingerprint = public.fingerprint(russh::keys::ssh_key::HashAlg::Sha256).to_string();
    sqlx::query("INSERT INTO sftp_keys(id,user_id,name,public_key,fingerprint,created_at) VALUES(?,?,?,?,?,?)")
        .bind("key-test")
        .bind(amy.id)
        .bind("Test key")
        .bind(public.to_openssh().unwrap())
        .bind(fingerprint)
        .bind(now())
        .execute(&env.st.db)
        .await
        .unwrap();
    let mut ssh = connection(&env).await;
    let key = russh::keys::PrivateKeyWithHashAlg::new(Arc::new(private), None);
    assert!(ssh.authenticate_publickey(&amy.username, key).await.unwrap().success());
    let channel = ssh.channel_open_session().await.unwrap();
    channel.request_subsystem(true, "sftp").await.unwrap();
    let sftp = SftpSession::new(channel.into_stream()).await.unwrap();
    sftp.set_timeout(5);
    let mut initial = sftp.create("/My files/key.txt").await.unwrap();
    initial.write_all(b"key access").await.unwrap();
    initial.shutdown().await.unwrap();
    sqlx::query("DELETE FROM sftp_keys WHERE id = 'key-test'").execute(&env.st.db).await.unwrap();
    sqlx::query("INSERT INTO sftp_keys(id,user_id,name,public_key,fingerprint,created_at) VALUES(?,?,?,?,?,?)")
        .bind("different-key-id")
        .bind(amy.id)
        .bind("Re-added key")
        .bind(public.to_openssh().unwrap())
        .bind(public.fingerprint(russh::keys::ssh_key::HashAlg::Sha256).to_string())
        .bind(now())
        .execute(&env.st.db)
        .await
        .unwrap();
    assert!(sftp.read("/My files/key.txt").await.is_err());
    assert!(sftp.create_dir("/My files/No").await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_and_wrong_credentials_fail_and_session_limit_is_enforced() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let token = token(&env, &amy, "write").await;
    let _server = on(&env).await;
    let mut wrong = connection(&env).await;
    assert!(!wrong.authenticate_password("amy", "wrong").await.unwrap().success());
    let mut held = Vec::new();
    for _ in 0..5 {
        held.push(client(&env, &amy, &token).await);
    }
    let mut extra = connection(&env).await;
    assert!(!extra.authenticate_password("amy", &token).await.unwrap().success());
    crate::db::set_setting(&mut env.st.db.acquire().await.unwrap(), "sftp_enabled", "0").await.unwrap();
    assert!(held[0].1.read_dir("/").await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_rechecks_credentials_after_waiting_for_a_folder_space_turn() {
    let env = testutil::folders_env().await;
    let amy = env.user("amy", true).await;
    let token = token(&env, &amy, "write").await;
    let server = on(&env).await;
    let (ssh, sftp) = client(&env, &amy, &token).await;
    let mut initial = sftp.create("/My files/a.txt").await.unwrap();
    initial.write_all(b"original").await.unwrap();
    initial.shutdown().await.unwrap();
    let parent = crate::tree::get_node(&mut env.st.db.acquire().await.unwrap(), amy.root()).await.unwrap().unwrap();
    let held = crate::fsops::lock_space(&env.st, parent.drive()).await;
    let mut file = sftp.create("/My files/a.txt").await.unwrap();
    file.write_all(b"replacement").await.unwrap();
    file.flush().await.unwrap();
    let closing = tokio::spawn(async move { file.shutdown().await });
    // Seeing the staged file proves CLOSE already verified the credential and is waiting for this space's turn.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if std::fs::read_dir(parent.fs_root.as_ref().unwrap()).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".thirtyfile-upload-")) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("CLOSE staged the replacement");
    assert!(!closing.is_finished());
    sqlx::query("DELETE FROM app_passwords WHERE token_hash = ?").bind(sha256_hex(token.as_bytes())).execute(&env.st.db).await.unwrap();
    drop(held);
    assert!(closing.await.unwrap().is_err(), "revocation while CLOSE waits must refuse the commit");
    assert_eq!(std::fs::read(parent.fs_pinned().unwrap().join("a.txt").unwrap().as_path()).unwrap(), b"original");
    sftp.close().await.unwrap();
    ssh.disconnect(russh::Disconnect::ByApplication, "Test complete", "en").await.unwrap();
    server.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn folder_space_metadata_reads_current_size_and_rmdir_preserves_unindexed_files() {
    let env = testutil::folders_env().await;
    let amy = env.user("amy", true).await;
    let token = token(&env, &amy, "write").await;
    let server = on(&env).await;
    let (ssh, sftp) = client(&env, &amy, &token).await;
    let mut initial = sftp.create("/My files/a.txt").await.unwrap();
    initial.write_all(b"old").await.unwrap();
    initial.shutdown().await.unwrap();
    let parent = crate::tree::get_node(&mut env.st.db.acquire().await.unwrap(), amy.root()).await.unwrap().unwrap();
    let root = parent.fs_pinned().unwrap();
    std::fs::write(root.join("a.txt").unwrap().as_path(), b"changed outside the application").unwrap();
    assert_eq!(sftp.metadata("/My files/a.txt").await.unwrap().size, Some(31));
    let read = sftp.open("/My files/a.txt").await.unwrap();
    assert_eq!(read.metadata().await.unwrap().size, Some(31));
    assert_eq!(sftp.read("/My files/a.txt").await.unwrap(), b"changed outside the application");
    sftp.create_dir("/My files/Unindexed").await.unwrap();
    let directory = root.join("Unindexed").unwrap();
    for name in ["outside.txt", "Thumbs.db"] {
        let file = directory.join(name).unwrap();
        std::fs::write(file.as_path(), b"keep").unwrap();
        assert!(sftp.remove_dir("/My files/Unindexed").await.is_err());
        assert_eq!(std::fs::read(file.as_path()).unwrap(), b"keep");
        std::fs::remove_file(file.as_path()).unwrap();
    }
    sftp.remove_dir("/My files/Unindexed").await.unwrap();
    assert!(!directory.as_path().exists());
    sftp.close().await.unwrap();
    ssh.disconnect(russh::Disconnect::ByApplication, "Test complete", "en").await.unwrap();
    server.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rmdir_rechecks_disk_contents_after_waiting_for_a_space_turn() {
    let env = testutil::folders_env().await;
    let amy = env.user("amy", true).await;
    let token = token(&env, &amy, "write").await;
    let server = on(&env).await;
    let (ssh, sftp) = client(&env, &amy, &token).await;
    sftp.create_dir("/My files/Unindexed").await.unwrap();
    let parent = crate::tree::get_node(&mut env.st.db.acquire().await.unwrap(), amy.root()).await.unwrap().unwrap();
    let held = crate::fsops::lock_space(&env.st, parent.drive()).await;
    let removing = tokio::spawn(async move {
        let result = sftp.remove_dir("/My files/Unindexed").await;
        (sftp, result)
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!removing.is_finished());
    let file = parent.fs_pinned().unwrap().join("Unindexed").unwrap().join("Thumbs.db").unwrap();
    std::fs::write(file.as_path(), b"keep").unwrap();
    drop(held);
    let (sftp, result) = removing.await.unwrap();
    assert!(result.is_err());
    assert_eq!(std::fs::read(file.as_path()).unwrap(), b"keep");
    sftp.close().await.unwrap();
    ssh.disconnect(russh::Disconnect::ByApplication, "Test complete", "en").await.unwrap();
    server.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exclusive_create_and_staged_size_changes_enforce_conflicts_quota_and_modes() {
    use russh_sftp::protocol::FileAttributes;
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    sqlx::query("UPDATE users SET quota_bytes = 8 WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
    let token = token(&env, &amy, "write").await;
    let server = on(&env).await;
    let (ssh, sftp) = client(&env, &amy, &token).await;
    let flags = OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::EXCLUDE;
    let mut first = sftp.open_with_flags("/My files/a.txt", flags).await.unwrap();
    let mut second = sftp.open_with_flags("/My files/a.txt", flags).await.unwrap();
    first.write_all(b"one").await.unwrap();
    first.shutdown().await.unwrap();
    second.write_all(b"two").await.unwrap();
    assert!(second.shutdown().await.is_err());
    assert_eq!(sftp.read("/My files/a.txt").await.unwrap(), b"one");
    let mut file = sftp.open_with_flags("/My files/a.txt", OpenFlags::WRITE).await.unwrap();
    file.set_metadata(FileAttributes { size: Some(8), ..Default::default() }).await.unwrap();
    file.set_metadata(FileAttributes { size: Some(2), ..Default::default() }).await.unwrap();
    assert!(file.set_metadata(FileAttributes { permissions: Some(0o600), ..Default::default() }).await.is_err());
    file.shutdown().await.unwrap();
    assert_eq!(sftp.read("/My files/a.txt").await.unwrap(), b"on");
    let mut file = sftp.open_with_flags("/My files/a.txt", OpenFlags::WRITE).await.unwrap();
    assert!(file.set_metadata(FileAttributes { size: Some(9), ..Default::default() }).await.is_err());
    assert!(file.shutdown().await.is_err());
    assert_eq!(sftp.read("/My files/a.txt").await.unwrap(), b"on");
    sftp.close().await.unwrap();
    ssh.disconnect(russh::Disconnect::ByApplication, "Test complete", "en").await.unwrap();
    server.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webdav_and_sftp_share_the_same_sign_in_failure_budget() {
    use crate::auth::app_passwords::{self, Credential};
    use axum::{extract::ConnectInfo, http::Request};
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let token = token(&env, &amy, "write").await;
    let server = on(&env).await;
    let mut request = Request::builder().method("PROPFIND").body(()).unwrap();
    request.extensions_mut().insert(ConnectInfo("127.0.0.1:9000".parse::<std::net::SocketAddr>().unwrap()));
    let (parts, _) = request.into_parts();
    for _ in 0..29 {
        assert!(app_passwords::authenticate(&parts, &env.st, Credential::Basic { username: amy.username.clone(), token: "wrong".into() }).await.is_err());
    }
    let mut ssh = connection(&env).await;
    assert!(!ssh.authenticate_password(&amy.username, "wrong").await.unwrap().success());
    assert!(!ssh.authenticate_password(&amy.username, &token).await.unwrap().success());
    let error = app_passwords::authenticate(&parts, &env.st, Credential::Basic { username: amy.username.clone(), token }).await.unwrap_err();
    assert_eq!(error.status, axum::http::StatusCode::TOO_MANY_REQUESTS);
    ssh.disconnect(russh::Disconnect::ByApplication, "Test complete", "en").await.unwrap();
    server.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn optional_listener_does_not_bind_until_enabled_and_retries_a_conflicting_port() {
    let env = testutil::env().await;
    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = occupied.local_addr().unwrap().to_string();
    let server = super::supervise(env.st.clone(), &address);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while env.st.part::<super::Memory>().fingerprint.read().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(env.st.part::<super::Memory>().error.read().is_none(), "a disabled feature does not try to bind an occupied port");
    crate::db::set_setting(&mut env.st.db.acquire().await.unwrap(), "sftp_enabled", "1").await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while env.st.part::<super::Memory>().error.read().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(occupied);
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        while env.st.part::<super::Memory>().error.read().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(tokio::net::TcpStream::connect(&address).await.is_ok());
    crate::db::set_setting(&mut env.st.db.acquire().await.unwrap(), "sftp_enabled", "0").await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Ok(listener) = tokio::net::TcpListener::bind(&address).await {
                drop(listener);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    server.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_open_sftp_session_loses_personal_access_on_guest_conversion_but_can_use_explicit_shares() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let amy = env.user("amy", true).await;
    let shared = env.folder(&admin, admin.root(), "Client").await;
    env.grant(&shared, &amy, "editor").await;
    sqlx::query("UPDATE grants SET expires_at=? WHERE node_id=? AND principal_type='user' AND principal_id=?")
        .bind(now() + 86400)
        .bind(&shared)
        .bind(amy.id)
        .execute(&env.st.db)
        .await
        .unwrap();
    let token = token(&env, &amy, "write").await;
    let server = on(&env).await;
    let (ssh, sftp) = client(&env, &amy, &token).await;
    let mut original = sftp.create("/My files/kept.txt").await.unwrap();
    original.write_all(b"original").await.unwrap();
    original.shutdown().await.unwrap();
    let mut replacement = sftp.create("/My files/kept.txt").await.unwrap();
    replacement.write_all(b"replacement").await.unwrap();
    replacement.flush().await.unwrap();
    let preview = crate::guests::preview_in(&mut env.st.db.acquire().await.unwrap(), amy.id).await.unwrap();
    let _ = crate::admin::update(
        axum::extract::State(env.st.clone()),
        crate::auth::Admin(admin),
        axum::extract::Path(amy.id),
        axum::Json(
            serde_json::from_value(serde_json::json!({"guest":true,"confirm_guest":true,"guest_revision":preview.revision,"my_password":testutil::password()}))
                .unwrap(),
        ),
    )
    .await
    .unwrap();
    assert!(replacement.shutdown().await.is_err());
    assert!(sftp.open("/My files/kept.txt").await.is_err());
    let entries = sftp.read_dir("/").await.unwrap();
    assert!(!entries.into_iter().any(|entry| entry.file_name() == "My files" || entry.file_name() == "All files"));
    let mut upload = sftp.create("/Shared with me/Client/guest.txt").await.unwrap();
    upload.write_all(b"guest content").await.unwrap();
    upload.shutdown().await.unwrap();
    assert!(sftp.open("/Shared with me/Client/guest.txt").await.is_ok());
    sqlx::query("UPDATE grants SET expires_at=? WHERE node_id=? AND principal_type='user' AND principal_id=?")
        .bind(now() - 1)
        .bind(&shared)
        .bind(amy.id)
        .execute(&env.st.db)
        .await
        .unwrap();
    assert!(sftp.open("/Shared with me/Client/guest.txt").await.is_err());
    sqlx::query("UPDATE users SET guest=0 WHERE id=?").bind(amy.id).execute(&env.st.db).await.unwrap();
    let kept = crate::access::item(&env.st, &amy, &["My files".into(), "kept.txt".into()]).await.unwrap();
    assert_eq!(crate::access::read_at(&env.st, &amy, &kept.id, 0, 64).await.unwrap(), b"original");
    sftp.close().await.unwrap();
    ssh.disconnect(russh::Disconnect::ByApplication, "Test complete", "en").await.unwrap();
    server.finish().await;
}
