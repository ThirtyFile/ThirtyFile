use crate::{
    auth::User,
    testutil::{self, TestEnv},
    util::{now, random_token, sha256_hex},
};
use std::{io, sync::Arc};
use thirtysftp::{
    client::{Dir, File, OpenOptions, Session as SftpSession, fs},
    wire::{Attributes, Times},
};
use thirtyssh::{
    client,
    keys::{Algorithm, Credential, HashAlg, PrivateKey, ssh_key},
    session as ssh_session,
};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

struct Client;

/// A new key pair
fn new_key() -> PrivateKey {
    PrivateKey::from(ssh_key::private::Ed25519Keypair::from_seed(&rand::random()))
}

/// The names in a folder
async fn names(sftp: &SftpSession, path: &str) -> Result<Vec<String>, thirtysftp::Error> {
    let mut dir = Dir::open(sftp, path).await?;
    let mut out = Vec::new();
    while let Some(entry) = dir.next_entry().await? {
        out.push(String::from_utf8_lossy(&entry.name).into_owned());
    }
    Ok(out)
}

/// Opens a file for writing the way a client does that asks for these things
fn writing(create: bool, truncate: bool, exclusive: bool) -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    if exclusive {
        options.create_new(true);
    } else {
        options.create(create);
    }
    options.truncate(truncate);
    options
}

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
    let mut opened = File::open(&sftp, "/Shared with me/Department/HR/payroll.txt").await.unwrap();
    let req = serde_json::from_value(serde_json::json!({"restricted":true,"expected":false,"confirmed":true})).unwrap();
    let _ = crate::restrictions::change(axum::extract::State(env.st.clone()), owner.clone(), axum::extract::Path(hidden.clone()), axum::Json(req)).await.unwrap();
    assert_eq!(names(&sftp, "/Shared with me/Department").await.unwrap(), vec!["minutes.txt"]);
    assert!(fs::metadata(&sftp, "/Shared with me/Department/HR").await.is_err());
    assert!(File::open(&sftp, "/Shared with me/Department/HR/payroll.txt").await.is_err());
    use tokio::io::AsyncReadExt;
    assert!(opened.read(&mut [0; 32]).await.is_err());
    assert!(fs::rename(&sftp, "/Shared with me/Department/HR", "/Shared with me/Department/renamed").await.is_err());
    assert!(fs::remove_file(&sftp, "/Shared with me/Department/HR/payroll.txt").await.is_err());
    env.grant(&hidden, &member, "viewer").await;
    let bytes = fs::read(&sftp, "/Shared with me/Department/HR/payroll.txt").await.unwrap();
    assert_eq!(&bytes[..], b"confidential");
    env.revoke(&hidden, &member).await;
    assert!(fs::read(&sftp, "/Shared with me/Department/HR/payroll.txt").await.is_err());
    assert!(crate::tree::get_node(&mut env.st.db.acquire().await.unwrap(), &secret).await.unwrap().is_some());
    sftp.close();
}

impl ssh_session::Handler for Client {}

impl client::Handler for Client {
    async fn verify_host(&mut self, _: &Credential, _: &Algorithm) -> io::Result<bool> {
        Ok(true)
    }
}

type Ssh = ssh_session::Session;
type Connection = client::Client<tokio::net::TcpStream, Client>;
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
/// Turns SFTP and its SSH keys on or off, as the control panel saves them
async fn switch(env: &TestEnv, on: bool) {
    let config = super::settings::Config { enabled: on, keys_enabled: on };
    crate::db::set_json_setting(&mut env.st.db.acquire().await.unwrap(), "sftp", &config).await.unwrap();
}
async fn on(env: &TestEnv) -> super::Listener {
    switch(env, true).await;
    super::start(env.st.clone(), "127.0.0.1:0").await.unwrap()
}
async fn connection(env: &TestEnv) -> Connection {
    let address = env.st.part::<super::Memory>().address.read().clone();
    client::connect(Arc::new(client::Config::default()), address, Client).await.unwrap()
}
async fn password(connection: &mut Connection, user: &str, password: &str) -> bool {
    connection.authenticate(user, client::Method::Password(password)).await.unwrap().is_accepted()
}
/// The SFTP session of a connection that signed in
async fn sftp_of(connection: Connection) -> (Ssh, SftpSession) {
    let ssh = connection.start().unwrap();
    let sftp = thirtysftp::ssh::connect(&ssh.handle(), thirtysftp::client::Config::default()).await.unwrap();
    (ssh, sftp)
}
async fn client(env: &TestEnv, user: &User, token: &str) -> (Ssh, SftpSession) {
    let mut ssh = connection(env).await;
    assert!(password(&mut ssh, &user.username, token).await);
    sftp_of(ssh).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_commit_on_close_offset_writes_keep_versions_and_delete_goes_to_trash() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let token = token(&env, &amy, "write").await;
    let _server = on(&env).await;
    let (_ssh, sftp) = client(&env, &amy, &token).await;
    assert_eq!(&fs::canonicalize(&sftp, ".").await.unwrap()[..], b"/");
    fs::create_dir(&sftp, "/My files/Reports").await.unwrap();
    let path = "/My files/Reports/a.txt";
    let mut file = File::create(&sftp, path).await.unwrap();
    file.write_all(b"hello world").await.unwrap();
    assert!(!fs::try_exists(&sftp, path).await.unwrap(), "not visible until close");
    file.shutdown().await.unwrap();
    assert_eq!(fs::read(&sftp, path).await.unwrap(), b"hello world");
    let mut file = writing(false, false, false).open(&sftp, path).await.unwrap();
    file.seek(std::io::SeekFrom::Start(6)).await.unwrap();
    file.write_all(b"SFTP!").await.unwrap();
    file.shutdown().await.unwrap();
    assert_eq!(fs::read(&sftp, path).await.unwrap(), b"hello SFTP!");
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM node_versions").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(count, 1);
    let (detail,): (String,) = sqlx::query_as("SELECT detail FROM activity WHERE action = 'edit' ORDER BY id DESC LIMIT 1").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(detail, "through SFTP");
    fs::rename(&sftp, path, "/My files/Reports/b.txt").await.unwrap();
    fs::remove_file(&sftp, "/My files/Reports/b.txt").await.unwrap();
    let (trash,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE trashed_at IS NOT NULL").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(trash, 1);
    let mut empty = File::create(&sftp, "/My files/empty.txt").await.unwrap();
    empty.shutdown().await.unwrap();
    assert_eq!(fs::read(&sftp, "/My files/empty.txt").await.unwrap(), b"");
    assert!(fs::symlink(&sftp, "outside", "/My files/link").await.is_err());
    assert!(fs::canonicalize(&sftp, "../../etc").await.is_err());
    sftp.close();
    _ssh.disconnect().await.unwrap();
    _server.stop();
    _server.finish().await;
    drop(empty);
    drop(file);
    drop(sftp);
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
    let mut initial = File::create(&sftp, "/My files/a.txt").await.unwrap();
    initial.write_all(b"original").await.unwrap();
    initial.shutdown().await.unwrap();
    let (_rssh, reader) = client(&env, &admin, &read).await;
    assert_eq!(fs::read(&reader, "/My files/a.txt").await.unwrap(), b"original");
    assert!(File::create(&reader, "/My files/new.txt").await.is_err());
    assert!(writing(true, true, false).open(&reader, "/My files/a.txt").await.is_err());
    assert!(fs::rename(&reader, "/My files/a.txt", "/My files/b.txt").await.is_err());
    assert!(fs::remove_file(&reader, "/My files/a.txt").await.is_err());
    assert!(fs::create_dir(&reader, "/My files/New").await.is_err());
    let mut file = File::create(&sftp, "/My files/a.txt").await.unwrap();
    file.write_all(b"replacement").await.unwrap();
    file.flush().await.unwrap();
    sqlx::query("DELETE FROM app_passwords WHERE token_hash = ?").bind(sha256_hex(writable.as_bytes())).execute(&env.st.db).await.unwrap();
    assert!(file.shutdown().await.is_err());
    assert_eq!(fs::read(&reader, "/My files/a.txt").await.unwrap(), b"original");
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
    let private = new_key();
    let public = private.public_key().clone();
    let fingerprint = public.fingerprint(HashAlg::Sha256).to_string();
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
    assert!(ssh.authenticate(&amy.username, &private).await.unwrap().is_accepted());
    let (_ssh, sftp) = sftp_of(ssh).await;
    let mut initial = File::create(&sftp, "/My files/key.txt").await.unwrap();
    initial.write_all(b"key access").await.unwrap();
    initial.shutdown().await.unwrap();
    sqlx::query("DELETE FROM sftp_keys WHERE id = 'key-test'").execute(&env.st.db).await.unwrap();
    sqlx::query("INSERT INTO sftp_keys(id,user_id,name,public_key,fingerprint,created_at) VALUES(?,?,?,?,?,?)")
        .bind("different-key-id")
        .bind(amy.id)
        .bind("Re-added key")
        .bind(public.to_openssh().unwrap())
        .bind(public.fingerprint(HashAlg::Sha256).to_string())
        .bind(now())
        .execute(&env.st.db)
        .await
        .unwrap();
    assert!(fs::read(&sftp, "/My files/key.txt").await.is_err());
    assert!(fs::create_dir(&sftp, "/My files/No").await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_and_wrong_credentials_fail_and_session_limit_is_enforced() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let token = token(&env, &amy, "write").await;
    let _server = on(&env).await;
    let mut wrong = connection(&env).await;
    assert!(!password(&mut wrong, "amy", "wrong").await);
    let mut held = Vec::new();
    for _ in 0..5 {
        held.push(client(&env, &amy, &token).await);
    }
    let mut extra = connection(&env).await;
    assert!(!password(&mut extra, "amy", &token).await);
    switch(&env, false).await;
    assert!(names(&held[0].1, "/").await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_rechecks_credentials_after_waiting_for_a_folder_space_turn() {
    let env = testutil::folders_env().await;
    let amy = env.user("amy", true).await;
    let token = token(&env, &amy, "write").await;
    let server = on(&env).await;
    let (ssh, sftp) = client(&env, &amy, &token).await;
    let mut initial = File::create(&sftp, "/My files/a.txt").await.unwrap();
    initial.write_all(b"original").await.unwrap();
    initial.shutdown().await.unwrap();
    let parent = crate::tree::get_node(&mut env.st.db.acquire().await.unwrap(), amy.root()).await.unwrap().unwrap();
    let held = crate::fsops::lock_space(&env.st, parent.drive()).await;
    let mut file = File::create(&sftp, "/My files/a.txt").await.unwrap();
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
    sftp.close();
    ssh.disconnect().await.unwrap();
    server.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn folder_space_metadata_reads_current_size_and_rmdir_preserves_unindexed_files() {
    let env = testutil::folders_env().await;
    let amy = env.user("amy", true).await;
    let token = token(&env, &amy, "write").await;
    let server = on(&env).await;
    let (ssh, sftp) = client(&env, &amy, &token).await;
    let mut initial = File::create(&sftp, "/My files/a.txt").await.unwrap();
    initial.write_all(b"old").await.unwrap();
    initial.shutdown().await.unwrap();
    let parent = crate::tree::get_node(&mut env.st.db.acquire().await.unwrap(), amy.root()).await.unwrap().unwrap();
    let root = parent.fs_pinned().unwrap();
    std::fs::write(root.join("a.txt").unwrap().as_path(), b"changed outside the application").unwrap();
    assert_eq!(fs::metadata(&sftp, "/My files/a.txt").await.unwrap().size, Some(31));
    let read = File::open(&sftp, "/My files/a.txt").await.unwrap();
    assert_eq!(read.metadata().await.unwrap().size, Some(31));
    assert_eq!(fs::read(&sftp, "/My files/a.txt").await.unwrap(), b"changed outside the application");
    fs::create_dir(&sftp, "/My files/Unindexed").await.unwrap();
    let directory = root.join("Unindexed").unwrap();
    for name in ["outside.txt", "Thumbs.db"] {
        let file = directory.join(name).unwrap();
        std::fs::write(file.as_path(), b"keep").unwrap();
        assert!(fs::remove_dir(&sftp, "/My files/Unindexed").await.is_err());
        assert_eq!(std::fs::read(file.as_path()).unwrap(), b"keep");
        std::fs::remove_file(file.as_path()).unwrap();
    }
    fs::remove_dir(&sftp, "/My files/Unindexed").await.unwrap();
    assert!(!directory.as_path().exists());
    sftp.close();
    ssh.disconnect().await.unwrap();
    server.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rmdir_rechecks_disk_contents_after_waiting_for_a_space_turn() {
    let env = testutil::folders_env().await;
    let amy = env.user("amy", true).await;
    let token = token(&env, &amy, "write").await;
    let server = on(&env).await;
    let (ssh, sftp) = client(&env, &amy, &token).await;
    fs::create_dir(&sftp, "/My files/Unindexed").await.unwrap();
    let parent = crate::tree::get_node(&mut env.st.db.acquire().await.unwrap(), amy.root()).await.unwrap().unwrap();
    let held = crate::fsops::lock_space(&env.st, parent.drive()).await;
    let removing = tokio::spawn(async move {
        let result = fs::remove_dir(&sftp, "/My files/Unindexed").await;
        (sftp, result)
    });
    // Waiting for its turn, behind the lock held here
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while crate::fsops::testing::waiting(parent.drive()) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!removing.is_finished());
    let file = parent.fs_pinned().unwrap().join("Unindexed").unwrap().join("Thumbs.db").unwrap();
    std::fs::write(file.as_path(), b"keep").unwrap();
    drop(held);
    let (sftp, result) = removing.await.unwrap();
    assert!(result.is_err());
    assert_eq!(std::fs::read(file.as_path()).unwrap(), b"keep");
    sftp.close();
    ssh.disconnect().await.unwrap();
    server.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exclusive_create_and_staged_size_changes_enforce_conflicts_quota_and_modes() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    sqlx::query("UPDATE users SET quota_bytes = 8 WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
    let token = token(&env, &amy, "write").await;
    let server = on(&env).await;
    let (ssh, sftp) = client(&env, &amy, &token).await;
    let mut first = writing(true, false, true).open(&sftp, "/My files/a.txt").await.unwrap();
    let mut second = writing(true, false, true).open(&sftp, "/My files/a.txt").await.unwrap();
    first.write_all(b"one").await.unwrap();
    first.shutdown().await.unwrap();
    second.write_all(b"two").await.unwrap();
    assert!(second.shutdown().await.is_err());
    assert_eq!(fs::read(&sftp, "/My files/a.txt").await.unwrap(), b"one");
    let mut file = writing(false, false, false).open(&sftp, "/My files/a.txt").await.unwrap();
    file.set_attributes(Attributes { size: Some(8), ..Default::default() }).await.unwrap();
    file.set_attributes(Attributes { size: Some(2), ..Default::default() }).await.unwrap();
    // Permissions are accepted and left as they are (scp -p sets them)
    file.set_attributes(Attributes { permissions: Some(0o600), ..Default::default() }).await.unwrap();
    // Owners can't be kept
    assert!(file.set_attributes(Attributes { owner: Some(thirtysftp::wire::Owner { uid: 0, gid: 0 }), ..Default::default() }).await.is_err());
    file.shutdown().await.unwrap();
    assert_eq!(fs::read(&sftp, "/My files/a.txt").await.unwrap(), b"on");
    let mut file = writing(false, false, false).open(&sftp, "/My files/a.txt").await.unwrap();
    assert!(file.set_attributes(Attributes { size: Some(9), ..Default::default() }).await.is_err());
    assert!(file.shutdown().await.is_err());
    assert_eq!(fs::read(&sftp, "/My files/a.txt").await.unwrap(), b"on");
    sftp.close();
    ssh.disconnect().await.unwrap();
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
    assert!(!password(&mut ssh, &amy.username, "wrong").await);
    assert!(!password(&mut ssh, &amy.username, &token).await);
    let error = app_passwords::authenticate(&parts, &env.st, Credential::Basic { username: amy.username.clone(), token }).await.unwrap_err();
    assert_eq!(error.status, axum::http::StatusCode::TOO_MANY_REQUESTS);
    drop(ssh);
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
    switch(&env, true).await;
    env.st.part::<super::Memory>().changed.notify_one();
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
    switch(&env, false).await;
    env.st.part::<super::Memory>().changed.notify_one();
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
    let mut original = File::create(&sftp, "/My files/kept.txt").await.unwrap();
    original.write_all(b"original").await.unwrap();
    original.shutdown().await.unwrap();
    let mut replacement = File::create(&sftp, "/My files/kept.txt").await.unwrap();
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
    assert!(File::open(&sftp, "/My files/kept.txt").await.is_err());
    let entries = names(&sftp, "/").await.unwrap();
    assert!(!entries.into_iter().any(|entry| entry == "My files" || entry == "All files"));
    let mut upload = File::create(&sftp, "/Shared with me/Client/guest.txt").await.unwrap();
    upload.write_all(b"guest content").await.unwrap();
    upload.shutdown().await.unwrap();
    assert!(File::open(&sftp, "/Shared with me/Client/guest.txt").await.is_ok());
    sqlx::query("UPDATE grants SET expires_at=? WHERE node_id=? AND principal_type='user' AND principal_id=?")
        .bind(now() - 1)
        .bind(&shared)
        .bind(amy.id)
        .execute(&env.st.db)
        .await
        .unwrap();
    assert!(File::open(&sftp, "/Shared with me/Client/guest.txt").await.is_err());
    sqlx::query("UPDATE users SET guest=0 WHERE id=?").bind(amy.id).execute(&env.st.db).await.unwrap();
    let kept = crate::access::item(&env.st, &amy, &["My files".into(), "kept.txt".into()]).await.unwrap();
    assert_eq!(crate::access::read_at(&env.st, &amy, &kept.id, 0, 64).await.unwrap(), b"original");
    sftp.close();
    ssh.disconnect().await.unwrap();
    server.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_ssh_keys_use_none_of_the_sign_in_limit_and_an_address_cant_hold_every_slot() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let _server = on(&env).await;
    let rate = format!("ip:{}", crate::auth::limit_key_ip("127.0.0.1"));
    let make = new_key;
    let known = make();
    let public = known.public_key().clone();
    sqlx::query("INSERT INTO sftp_keys(id,user_id,name,public_key,fingerprint,created_at) VALUES(?,?,?,?,?,?)")
        .bind("known-key")
        .bind(amy.id)
        .bind("Laptop")
        .bind(public.to_openssh().unwrap())
        .bind(public.fingerprint(HashAlg::Sha256).to_string())
        .bind(now())
        .execute(&env.st.db)
        .await
        .unwrap();
    // An SSH agent tries its other keys first
    let mut ssh = connection(&env).await;
    for _ in 0..3 {
        assert!(!ssh.authenticate(&amy.username, &make()).await.unwrap().is_accepted());
    }
    assert!(ssh.authenticate(&amy.username, &known).await.unwrap().is_accepted());
    assert!(!crate::auth::attempts_exhausted(&env.st, &rate, 1), "unknown keys aren't failed sign-ins");
    drop(ssh);

    // Connections that never sign in: an address holds a few, and the next is closed at once
    let address = env.st.part::<super::Memory>().address.read().clone();
    let mut idle = Vec::new();
    for _ in 0..crate::auth::pending::PER_ADDRESS {
        idle.push(tokio::net::TcpStream::connect(&address).await.unwrap());
    }
    // (give the listener a moment to take them all)
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let mut probe = tokio::net::TcpStream::connect(&address).await.unwrap();
            let mut byte = [0u8; 1];
            if tokio::time::timeout(std::time::Duration::from_millis(500), tokio::io::AsyncReadExt::read(&mut probe, &mut byte))
                .await
                .is_ok_and(|r| matches!(r, Ok(0) | Err(_)))
            {
                break;
            }
        }
    })
    .await
    .expect("a further connection from the same address is closed");
    idle.clear();
    // Once they're gone, the address connects again (the listener lets go of their places a moment after they close)
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let mut probe = tokio::net::TcpStream::connect(&address).await.unwrap();
            let mut byte = [0u8; 1];
            // A connection that is kept is greeted by the server; a refused one is closed at once
            if tokio::time::timeout(std::time::Duration::from_millis(500), tokio::io::AsyncReadExt::read(&mut probe, &mut byte))
                .await
                .is_ok_and(|r| matches!(r, Ok(n) if n > 0))
            {
                break;
            }
        }
    })
    .await
    .expect("the address connects again once its idle connections are gone");
    let (_ssh, _sftp) = client(&env, &amy, &token(&env, &amy, "write").await).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_times_a_client_sets_are_kept_and_permissions_are_accepted() {
    let env = testutil::env().await;
    let amy = env.user("amy", true).await;
    let token = token(&env, &amy, "write").await;
    let _server = on(&env).await;
    let (_ssh, sftp) = client(&env, &amy, &token).await;
    let path = "/My files/photo.jpg";
    let modified = |a: &Attributes| a.times.map(|t| t.modified);
    // WinSCP sets the time on the file it writes, before closing it
    let mut file = File::create(&sftp, path).await.unwrap();
    file.write_all(b"pixels").await.unwrap();
    file.set_attributes(Attributes { times: Some(Times { accessed: 1_600_000_000, modified: 1_600_000_000 }), ..Default::default() }).await.unwrap();
    file.shutdown().await.unwrap();
    assert_eq!(modified(&fs::metadata(&sftp, path).await.unwrap()), Some(1_600_000_000));
    // rclone sets it once the file is there; scp -p sets permissions as well, which are accepted and left as they are
    sftp.setstat(path, Attributes { times: Some(Times { accessed: 1_500_000_000, modified: 1_500_000_000 }), permissions: Some(0o100600), ..Default::default() })
        .await
        .unwrap();
    let after = fs::metadata(&sftp, path).await.unwrap();
    assert_eq!((modified(&after), after.permissions), (Some(1_500_000_000), Some(0o100644)));
    // New content without a time: when it changed in ThirtyFile
    let mut file = File::create(&sftp, path).await.unwrap();
    file.write_all(b"edited").await.unwrap();
    file.shutdown().await.unwrap();
    assert!(modified(&fs::metadata(&sftp, path).await.unwrap()).unwrap() > 1_700_000_000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transfer_checks_the_account_every_few_seconds_not_for_every_packet() {
    let env = testutil::env().await;
    let user = env.user("packet-counter", true).await;
    let token = token(&env, &user, "write").await;
    let _server = on(&env).await;
    let (_ssh, sftp) = client(&env, &user, &token).await;
    let checks = || super::CHECKS.lock().get("packet-counter").copied().unwrap_or(0);
    let before = checks();
    let mut file = File::create(&sftp, "/My files/big.bin").await.unwrap();
    file.write_all(&vec![7u8; 4 << 20]).await.unwrap();
    file.shutdown().await.unwrap();
    assert_eq!(fs::read(&sftp, "/My files/big.bin").await.unwrap().len(), 4 << 20);
    let made = checks() - before;
    assert!(made < 8, "the account was checked {made} times for one file");
}
