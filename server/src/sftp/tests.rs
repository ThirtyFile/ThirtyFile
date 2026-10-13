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
        crate::db::set_setting(&mut *env.st.db.acquire().await.unwrap(), key, "1").await.unwrap();
    }
    super::start(env.st.clone(), "127.0.0.1:0").await.unwrap()
}
async fn connection(env: &TestEnv) -> client::Handle<Client> {
    client::connect(Arc::new(client::Config::default()), env.st.part::<super::Memory>().address.read().clone(), Client).await.unwrap()
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

#[tokio::test]
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
}

#[tokio::test]
async fn read_only_admin_credentials_never_write_and_revoked_open_files_are_discarded() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    let writable = token(&env, &admin, "write").await;
    let read = token(&env, &admin, "read").await;
    let _server = on(&env).await;
    let (_ssh, sftp) = client(&env, &admin, &writable).await;
    sftp.write("/My files/a.txt", b"original").await.unwrap();
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

#[tokio::test]
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
    let public = private.public_key();
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
    sftp.write("/My files/key.txt", b"key access").await.unwrap();
    sqlx::query("DELETE FROM sftp_keys WHERE id = 'key-test'").execute(&env.st.db).await.unwrap();
    assert!(sftp.read("/My files/key.txt").await.is_err());
    assert!(sftp.create_dir("/My files/No").await.is_err());
}

#[tokio::test]
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
    crate::db::set_setting(&mut *env.st.db.acquire().await.unwrap(), "sftp_enabled", "0").await.unwrap();
    assert!(held[0].1.read_dir("/").await.is_err());
}
