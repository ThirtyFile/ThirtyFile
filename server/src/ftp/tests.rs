use super::*;
mod tls;

use crate::{
    auth::User,
    testutil::{self, TestEnv},
};
use axum::{Json, extract::State};
use suppaftp::tokio::AsyncRustlsFtpStream;
use tokio::io::AsyncReadExt;

// Test the production wrapper itself; the vendored crate has no upstream test-only dependencies.
#[path = "../../vendor/libunftp/src/server/tls_upload_reader.rs"]
mod tls_upload_reader;

async fn download(ftp: &mut AsyncRustlsFtpStream, name: &str) -> Result<Vec<u8>, suppaftp::FtpError> {
    let mut stream = ftp.retr_as_stream(name).await?;
    let mut bytes = vec![];
    stream.read_to_end(&mut bytes).await.map_err(suppaftp::FtpError::ConnectionError)?;
    stream.finish().await?;
    Ok(bytes)
}

async fn fixture(folders: bool) -> (TestEnv, User, manage::Created, Listener) {
    let env = if folders { testutil::folders_env().await } else { testutil::env().await };
    let admin = env.admin().await;
    let cfg = config::Config { enabled: true, managers_publish: false, allow_private_plaintext: true };
    let _ = config::save(State(env.st.clone()), crate::auth::Admin(admin.clone()), Json(cfg.clone())).await.unwrap();
    let root = env.st.shared_root().unwrap();
    let parent = crate::tree::get_node(&mut env.st.db.acquire().await.unwrap(), &root).await.unwrap().unwrap();
    crate::access::make_folder(&env.st, &admin, &parent, "Incoming").await.unwrap();
    let node = crate::paths::child_named(&mut env.st.db.acquire().await.unwrap(), &root, "Incoming").await.unwrap().unwrap();
    let Json(created) = manage::create(
        State(env.st.clone()),
        admin.clone(),
        Json(manage::Create {
            node_id: node.id,
            change: manage::Change { name: "Scanner".into(), enabled: true, allow_list: false, allow_read: false, replace_existing: true, expires_at: None },
            password: Some(testutil::password().into()),
            code: None,
        }),
    )
    .await
    .unwrap();
    let server =
        start(env.st.clone(), Settings { address: "127.0.0.1:0".into(), passive: "0-0".into(), host: String::new(), cert: None, key: None }, cfg).await.unwrap();
    (env, admin, created, server)
}
async fn client(env: &TestEnv, key: &manage::Created) -> AsyncRustlsFtpStream {
    let address = env.st.part::<Memory>().status.read().address.clone();
    let mut ftp = AsyncRustlsFtpStream::connect(address).await.unwrap();
    ftp.login(&key.folder.username, &key.password).await.unwrap();
    ftp.transfer_type(suppaftp::types::FileType::Binary).await.unwrap();
    ftp
}
async fn bytes(env: &TestEnv, parent: &str, name: &str) -> Vec<u8> {
    let node = crate::paths::child_named(&mut env.st.db.acquire().await.unwrap(), parent, name).await.unwrap().unwrap();
    let admin = env.admin().await;
    crate::access::read_at(&env.st, &admin, &node.id, 0, 256 * 1024).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upload_only_native_protocol_versions_and_no_destructive_powers() {
    let (env, _, created, server) = fixture(false).await;
    let mut ftp = client(&env, &created).await;
    ftp.put_file("scan.txt", &mut std::io::Cursor::new(b"first scan")).await.unwrap();
    assert_eq!(bytes(&env, &created.folder.node_id, "scan.txt").await, b"first scan");
    assert!(ftp.list(None).await.is_err());
    assert!(ftp.size("scan.txt").await.is_err());
    assert!(ftp.mdtm("scan.txt").await.is_err());
    assert!(download(&mut ftp, "scan.txt").await.is_err());
    assert!(ftp.rm("scan.txt").await.is_err());
    assert!(ftp.rename("scan.txt", "moved.txt").await.is_err());
    assert!(ftp.rmdir("/").await.is_err());
    assert!(ftp.cwd("/../").await.is_err());
    ftp.put_file("scan.txt", &mut std::io::Cursor::new(b"updated scan")).await.unwrap();
    ftp.put_file("empty.txt", &mut std::io::Cursor::new(b"")).await.unwrap();
    ftp.mkdir("batch").await.unwrap();
    ftp.cwd("batch").await.unwrap();
    ftp.put_file("next.txt", &mut std::io::Cursor::new(b"next")).await.unwrap();
    assert_eq!(bytes(&env, &created.folder.node_id, "scan.txt").await, b"updated scan");
    let (versions,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM node_versions").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(versions, 1);
    let (detail,): (String,) = sqlx::query_as("SELECT detail FROM activity WHERE action='upload' ORDER BY id DESC LIMIT 1").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(detail, "through FTP receive folder Scanner");
    ftp.quit().await.unwrap();
    drop(ftp);
    server.finish().await;
    env.st.db.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_and_read_are_separate_and_rotation_revokes_existing_session() {
    let (env, _, created, server) = fixture(false).await;
    sqlx::query("UPDATE ftp_receive_folders SET allow_list=1 WHERE id=?").bind(&created.folder.id).execute(&env.st.db).await.unwrap();
    let mut ftp = client(&env, &created).await;
    ftp.put_file("scan.txt", &mut std::io::Cursor::new(b"scan")).await.unwrap();
    assert_eq!(ftp.nlst(None).await.unwrap(), ["scan.txt"]);
    assert!(download(&mut ftp, "scan.txt").await.is_err());
    sqlx::query("UPDATE ftp_receive_folders SET allow_read=1 WHERE id=?").bind(&created.folder.id).execute(&env.st.db).await.unwrap();
    assert_eq!(download(&mut ftp, "scan.txt").await.unwrap(), b"scan");
    sqlx::query("UPDATE ftp_receive_folders SET generation=generation+1 WHERE id=?").bind(&created.folder.id).execute(&env.st.db).await.unwrap();
    assert!(ftp.put_file("scan.txt", &mut std::io::Cursor::new(b"must not replace")).await.is_err());
    assert_eq!(bytes(&env, &created.folder.node_id, "scan.txt").await, b"scan");
    let _ = ftp.quit().await;
    drop(ftp);
    server.finish().await;
    env.st.db.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revocation_after_receive_while_waiting_for_final_transaction_keeps_original() {
    let (env, _, created, server) = fixture(false).await;
    let mut ftp = client(&env, &created).await;
    ftp.put_file("scan.txt", &mut std::io::Cursor::new(b"original")).await.unwrap();
    let guard = env.st.write_lock.lock().await;
    let transfer = tokio::spawn(async move {
        let result = ftp.put_file("scan.txt", &mut std::io::Cursor::new(b"replacement")).await;
        let _ = ftp.quit().await;
        result
    });
    // Received whole, it waits for the write lock to be put in place
    let staged = async {
        loop {
            let sizes: Vec<u64> = std::fs::read_dir(env.st.tmp_dir()).unwrap().flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).collect();
            if sizes.contains(&(b"replacement".len() as u64)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), staged).await.unwrap();
    sqlx::query("UPDATE ftp_receive_folders SET enabled=0 WHERE id=?").bind(&created.folder.id).execute(&env.st.db).await.unwrap();
    drop(guard);
    assert!(tokio::time::timeout(Duration::from_secs(5), transfer).await.unwrap().unwrap().is_err());
    assert_eq!(bytes(&env, &created.folder.node_id, "scan.txt").await, b"original");
    assert_eq!(std::fs::read_dir(env.st.tmp_dir()).unwrap().count(), 0);
    server.finish().await;
    env.st.db.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_supervisor_with_ftp_off_reads_its_settings_only_when_they_change() {
    let env = testutil::env().await;
    let settings = Settings { address: "127.0.0.1:0".into(), passive: "0-0".into(), host: String::new(), cert: None, key: None };
    let server = supervise(env.st.clone(), settings);
    let looks = || env.st.part::<Memory>().looks.load(std::sync::atomic::Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(5), async {
        while looks() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // It used to read them every half second
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(looks(), 1);
    let _ = config::save(State(env.st.clone()), crate::auth::Admin(env.admin().await), Json(config::Config::default())).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while looks() < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    server.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_supervisor_does_not_bind_or_read_bad_certificate() {
    let env = testutil::env().await;
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap().to_string();
    let settings = Settings {
        address: address.clone(),
        passive: "bad range".into(),
        host: String::new(),
        cert: Some(env.dir.join("missing")),
        key: Some(env.dir.join("missing-key")),
    };
    let server = supervise(env.st.clone(), settings);
    // Once it has looked at the settings (FTP is off): nothing is bound or read
    tokio::time::timeout(Duration::from_secs(5), async {
        while env.st.part::<Memory>().looks.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(env.st.part::<Memory>().status.read().error.is_none());
    assert!(!env.st.part::<Memory>().status.read().listening);
    let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert!(http.local_addr().is_ok());
    let _ = config::save(State(env.st.clone()), crate::auth::Admin(env.admin().await), Json(config::Config { enabled: true, ..Default::default() })).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while env.st.part::<Memory>().status.read().error.is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(http.local_addr().is_ok());
    drop(http);
    drop(socket);
    server.finish().await;
    env.st.db.close().await;
}

#[test]
fn direct_private_peer_policy() {
    for ip in ["127.0.0.1", "10.1.2.3", "172.16.2.3", "192.168.2.3", "::1", "fc00::1", "::ffff:192.168.1.2"] {
        assert!(login::private_peer(ip.parse().unwrap()), "{ip}");
    }
    for ip in ["203.0.113.1", "172.15.2.3", "172.32.2.3", "169.254.1.2", "0.0.0.0", "2001:db8::1", "fe80::1"] {
        assert!(!login::private_peer(ip.parse().unwrap()), "{ip}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abort_during_stor_preserves_original_and_removes_partial() {
    use tokio::io::AsyncWriteExt;
    let (env, _, created, server) = fixture(false).await;
    let mut ftp = client(&env, &created).await;
    ftp.put_file("scan.txt", &mut std::io::Cursor::new(b"original")).await.unwrap();
    let mut transfer = ftp.put_with_stream("scan.txt").await.unwrap();
    transfer.write_all(b"partial replacement").await.unwrap();
    transfer.flush().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while std::fs::read_dir(env.st.tmp_dir()).unwrap().count() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    ftp.abort(transfer).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while std::fs::read_dir(env.st.tmp_dir()).unwrap().count() > 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(bytes(&env, &created.folder.node_id, "scan.txt").await, b"original");
    let _ = ftp.quit().await;
    drop(ftp);
    server.finish().await;
    env.st.db.close().await;
}

async fn tls_listener(env: &TestEnv) -> (Listener, Arc<rustls::ClientConfig>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_path = env.dir.join("ftp-cert.pem");
    let key_path = env.dir.join("ftp-key.pem");
    use base64::Engine;
    let pem = |label: &str, bytes: &[u8]| format!("-----BEGIN {label}-----\n{}\n-----END {label}-----\n", base64::engine::general_purpose::STANDARD.encode(bytes));
    tokio::fs::write(&cert_path, pem("CERTIFICATE", cert.cert.der())).await.unwrap();
    tokio::fs::write(&key_path, pem("PRIVATE KEY", &cert.signing_key.serialize_der())).await.unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.cert.der().clone()).unwrap();
    let client = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let cfg = config::Config { enabled: true, managers_publish: false, allow_private_plaintext: false };
    crate::db::set_setting(&mut env.st.db.acquire().await.unwrap(), "ftp", &serde_json::to_string(&cfg).unwrap()).await.unwrap();
    let listener = start(
        env.st.clone(),
        Settings { address: "127.0.0.1:0".into(), passive: "0-0".into(), host: String::new(), cert: Some(cert_path), key: Some(key_path) },
        cfg,
    )
    .await
    .unwrap();
    (listener, Arc::new(client))
}
async fn secure_client(env: &TestEnv, key: &manage::Created, tls: Arc<rustls::ClientConfig>) -> AsyncRustlsFtpStream {
    let address = env.st.part::<Memory>().status.read().address.clone();
    let ftp = AsyncRustlsFtpStream::connect(address).await.unwrap();
    let mut ftp = ftp.into_secure(suppaftp::tokio::AsyncRustlsConnector::from(tokio_rustls::TlsConnector::from(tls)), "localhost").await.unwrap();
    ftp.login(&key.folder.username, &key.password).await.unwrap();
    ftp.transfer_type(suppaftp::types::FileType::Binary).await.unwrap();
    ftp
}

async fn secure_upload_stream(ftp: &mut AsyncRustlsFtpStream, tls: Arc<rustls::ClientConfig>, name: &str) -> tokio_rustls::client::TlsStream<tokio::net::TcpStream> {
    let reply = ftp.custom_command("EPSV", &[suppaftp::Status::ExtendedPassiveMode]).await.unwrap().as_string().unwrap();
    let port: u16 = reply.split("|||").nth(1).unwrap().split('|').next().unwrap().parse().unwrap();
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    ftp.custom_command(format!("STOR {name}"), &[suppaftp::Status::AboutToSend]).await.unwrap();
    tokio_rustls::TlsConnector::from(tls).connect("localhost".try_into().unwrap(), stream).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ftps_upload_eof_returns_close_notify_before_the_success_reply() {
    use tokio::io::AsyncWriteExt;
    let (env, _, created, plain) = fixture(false).await;
    plain.finish().await;
    let (server, tls) = tls_listener(&env).await;
    let mut ftp = secure_client(&env, &created, tls.clone()).await;
    for (name, content) in [("empty.txt", vec![]), ("encrypted.txt", vec![42; 131_072])] {
        let mut data = secure_upload_stream(&mut ftp, tls.clone(), name).await;
        assert_eq!(data.get_ref().1.handshake_kind(), Some(rustls::HandshakeKind::Resumed), "upload TLS must retain the control connection's session cache");
        data.write_all(&content).await.unwrap();
        // Like FTP_TLS.unwrap(), send close_notify, then require the peer's TLS close_notify instead of accepting
        // a truncated TCP EOF. The server must finish this shutdown before storage can report success.
        data.shutdown().await.unwrap();
        let mut reply = vec![];
        let eof = tokio::time::timeout(Duration::from_secs(5), data.read_to_end(&mut reply)).await.unwrap();
        let response = tokio::time::timeout(Duration::from_secs(5), ftp.read_response(suppaftp::Status::ClosingDataConnection)).await.unwrap().unwrap();
        assert_eq!(response.status, suppaftp::Status::ClosingDataConnection);
        assert_eq!(bytes(&env, &created.folder.node_id, name).await, content);
        assert!(reply.is_empty());
        assert!(eof.is_ok(), "the upload peer must send close_notify: {eof:?}");
    }
    ftp.quit().await.unwrap();
    drop(ftp);
    server.finish().await;
    env.st.db.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ftps_sender_can_send_close_notify_then_close_without_waiting_for_our_reply() {
    let (env, _, created, plain) = fixture(false).await;
    plain.finish().await;
    let (server, tls) = tls_listener(&env).await;
    let mut ftp = secure_client(&env, &created, tls).await;
    // suppaftp sends its close_notify, immediately drops the data socket, then reads the control-channel reply.
    for (name, content) in [("empty.txt", vec![]), ("encrypted.txt", vec![42; 131_072])] {
        assert_eq!(ftp.put_file(name, &mut std::io::Cursor::new(&content)).await.unwrap_or_else(|error| panic!("{name}: {error:?}")), content.len() as u64);
        assert_eq!(bytes(&env, &created.folder.node_id, name).await, content);
    }
    ftp.quit().await.unwrap();
    drop(ftp);
    server.finish().await;
    env.st.db.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ftps_truncated_uploads_refuse_success_and_preserve_previous_bytes() {
    use tokio::io::AsyncWriteExt;
    let (env, _, created, plain) = fixture(false).await;
    plain.finish().await;
    let (server, tls) = tls_listener(&env).await;
    let mut ftp = secure_client(&env, &created, tls.clone()).await;
    ftp.put_file("encrypted.txt", &mut std::io::Cursor::new(b"original")).await.unwrap();
    let mut data = secure_upload_stream(&mut ftp, tls, "encrypted.txt").await;
    data.write_all(b"incomplete replacement").await.unwrap();
    data.flush().await.unwrap();
    // A TCP half-close without a TLS close_notify is truncation, not a successful end of an upload.
    data.get_mut().0.shutdown().await.unwrap();
    drop(data);
    assert!(tokio::time::timeout(Duration::from_secs(5), ftp.read_response(suppaftp::Status::ClosingDataConnection)).await.unwrap().is_err());
    assert_eq!(bytes(&env, &created.folder.node_id, "encrypted.txt").await, b"original");
    assert_eq!(std::fs::read_dir(env.st.tmp_dir()).unwrap().count(), 0);
    let _ = ftp.quit().await;
    drop(ftp);
    server.finish().await;
    env.st.db.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_ftps_validates_cert_encrypts_data_and_forbids_epsv_downgrade() {
    let (env, _, created, plain) = fixture(false).await;
    plain.finish().await;
    let (server, tls) = tls_listener(&env).await;
    let address = env.st.part::<Memory>().status.read().address.clone();
    let mut plain = AsyncRustlsFtpStream::connect(&address).await.unwrap();
    assert!(plain.login(&created.folder.username, &created.password).await.is_err());
    drop(plain);
    let mut ftp = secure_client(&env, &created, tls).await;
    ftp.set_mode(suppaftp::Mode::ExtendedPassive);
    ftp.put_file("encrypted.txt", &mut std::io::Cursor::new(b"encrypted FTPS data")).await.unwrap();
    assert_eq!(bytes(&env, &created.folder.node_id, "encrypted.txt").await, b"encrypted FTPS data");
    ftp.custom_command("PROT C", &[suppaftp::Status::CommandOk]).await.unwrap();
    assert!(ftp.custom_command("PASV", &[suppaftp::Status::PassiveMode]).await.is_err());
    assert!(ftp.custom_command("EPSV", &[suppaftp::Status::ExtendedPassiveMode]).await.is_err(), "TLS data policy must also cover EPSV");
    let _ = ftp.quit().await;
    drop(ftp);
    server.finish().await;
    env.st.db.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ftps_preallocated_socket_cannot_downgrade_then_store_plaintext() {
    use tokio::io::AsyncWriteExt;
    let (env, _, created, plain) = fixture(false).await;
    plain.finish().await;
    let (server, tls) = tls_listener(&env).await;
    let mut ftp = secure_client(&env, &created, tls).await;
    let reply = ftp.custom_command("EPSV", &[suppaftp::Status::ExtendedPassiveMode]).await.unwrap().as_string().unwrap();
    let port: u16 = reply.split("|||").nth(1).unwrap().split('|').next().unwrap().parse().unwrap();
    ftp.custom_command("PROT C", &[suppaftp::Status::CommandOk]).await.unwrap();
    let mut data = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let accepted = ftp.custom_command("STOR must-not-exist.txt", &[suppaftp::Status::AboutToSend]).await.is_ok();
    if accepted {
        data.write_all(b"plaintext on TLS-only listener").await.unwrap();
        data.shutdown().await.unwrap();
        // Only when the server took it: the file it stores fails the test as soon as it is there
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            while crate::paths::child_named(&mut env.st.db.acquire().await.unwrap(), &created.folder.node_id, "must-not-exist.txt").await.unwrap().is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
    }
    let node = crate::paths::child_named(&mut env.st.db.acquire().await.unwrap(), &created.folder.node_id, "must-not-exist.txt").await.unwrap();
    assert!(!accepted && node.is_none(), "a previously allocated passive socket must not authorize a plaintext STOR");
    drop(data);
    let _ = ftp.quit().await;
    drop(ftp);
    server.finish().await;
    env.st.db.close().await;
}

#[tokio::test]
async fn only_the_publisher_or_an_administrator_rotates_or_opens_up_a_receive_folder() {
    use axum::{
        Json,
        extract::{Path, State},
        http::StatusCode,
    };
    let env = crate::testutil::env().await;
    let admin = env.admin().await;
    let manager = env.user("folder-manager", true).await;
    let st = || State(env.st.clone());
    crate::db::set_setting(
        &mut env.st.db.acquire().await.unwrap(),
        "ftp",
        &serde_json::json!({"enabled":true,"managers_publish":true,"allow_private_plaintext":false}).to_string(),
    )
    .await
    .unwrap();
    let folder = env.folder(&admin, &env.st.shared_root().unwrap(), "Scans").await;
    env.grant(&folder, &manager, "manager").await;
    let password = || Some(crate::testutil::password().to_string());
    let change = |enabled: bool, read: bool| manage::Change {
        name: "Office printer".into(),
        enabled,
        allow_list: false,
        allow_read: read,
        replace_existing: true,
        expires_at: None,
    };
    let Json(created) =
        manage::create(st(), admin.clone(), Json(manage::Create { node_id: folder.clone(), change: change(true, false), password: password(), code: None }))
            .await
            .unwrap();
    let id = created.folder.id.clone();
    let edit = |who: &crate::auth::User, enabled: bool, read: bool| {
        manage::update(st(), who.clone(), Path(id.clone()), Json(manage::Edit { change: change(enabled, read), password: password(), code: None }))
    };
    let refused = |r: crate::error::AppResult<_>| matches!(r, Err(e) if e.status == StatusCode::FORBIDDEN);

    // A manager can narrow it or turn it off, but not let the device read, turn it on again, or take its password
    assert!(refused(edit(&manager, true, true).await.map(|_| ())));
    let _ = edit(&manager, false, false).await.unwrap();
    assert!(refused(edit(&manager, true, false).await.map(|_| ())));
    let rotate = |who: &crate::auth::User| manage::rotate(st(), who.clone(), Path(id.clone()), Json(manage::Confirm { password: password(), code: None }));
    assert!(refused(rotate(&manager).await.map(|_| ())));
    let _ = edit(&admin, true, true).await.unwrap();
    let _ = rotate(&admin).await.unwrap();

    // A receive folder only takes files: not published by an account that may not change files, nor in a read-only
    // space, even by an administrator
    let other = env.folder(&admin, &env.st.shared_root().unwrap(), "More scans").await;
    env.grant(&other, &manager, "manager").await;
    let create = |who: &crate::auth::User| {
        manage::create(st(), who.clone(), Json(manage::Create { node_id: other.clone(), change: change(true, false), password: password(), code: None }))
    };
    sqlx::query("UPDATE users SET can_write = 0 WHERE id = ?").bind(manager.id).execute(&env.st.db).await.unwrap();
    assert!(refused(create(&manager).await.map(|_| ())));
    sqlx::query("UPDATE drives SET read_only = 1 WHERE id = (SELECT drive_id FROM nodes WHERE id = ?)").bind(&other).execute(&env.st.db).await.unwrap();
    assert!(create(&admin).await.is_err());

    // The entries name the receive folder
    let (named,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM activity WHERE detail LIKE '%Office printer%'").fetch_one(&env.st.db).await.unwrap();
    assert!(named >= 3, "{named}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_receive_folder_that_keeps_both_numbers_a_new_file_with_a_taken_name() {
    let (env, admin, created, server) = fixture(false).await;
    let change = manage::Change { name: "Scanner".into(), enabled: true, allow_list: true, allow_read: false, replace_existing: false, expires_at: None };
    let edit = Json(manage::Edit { change, password: Some(testutil::password().into()), code: None });
    let _ = manage::update(State(env.st.clone()), admin, axum::extract::Path(created.folder.id.clone()), edit).await.unwrap();
    let mut ftp = client(&env, &created).await;
    ftp.put_file("scan.pdf", &mut std::io::Cursor::new(b"first")).await.unwrap();
    ftp.put_file("scan.pdf", &mut std::io::Cursor::new(b"second")).await.unwrap();
    let mut names = ftp.nlst(None).await.unwrap();
    names.sort();
    assert_eq!(names, ["scan (1).pdf", "scan.pdf"]);
    assert_eq!(bytes(&env, &created.folder.node_id, "scan.pdf").await, b"first");
    assert_eq!(bytes(&env, &created.folder.node_id, "scan (1).pdf").await, b"second");
    let _ = ftp.quit().await;
    drop(ftp);
    server.finish().await;
    env.st.db.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upload_checks_its_receive_folder_every_few_seconds_not_for_every_chunk() {
    let (env, _, created, server) = fixture(false).await;
    let mut ftp = client(&env, &created).await;
    let checks = || super::credentials::CHECKS.lock().get(&created.folder.id).copied().unwrap_or(0);
    let before = checks();
    ftp.put_file("big.bin", &mut std::io::Cursor::new(vec![7u8; 4 << 20])).await.unwrap();
    let made = checks() - before;
    assert!(made < 8, "the receive folder was checked {made} times for one upload of 64 chunks");
    assert_eq!(bytes(&env, &created.folder.node_id, "big.bin").await.len(), 256 * 1024, "read back in part");
    let _ = ftp.quit().await;
    drop(ftp);
    server.finish().await;
    env.st.db.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusal_after_the_right_password_isnt_a_failed_sign_in() {
    let (env, _, created, server) = fixture(false).await;
    let address = env.st.part::<Memory>().status.read().address.clone();
    // Five sessions already: a sixth with the right password is refused, many times, without using up the address
    let mut held = Vec::new();
    for _ in 0..5 {
        held.push(client(&env, &created).await);
    }
    for _ in 0..40 {
        let mut ftp = AsyncRustlsFtpStream::connect(&address).await.unwrap();
        assert!(ftp.login(&created.folder.username, &created.password).await.is_err());
    }
    drop(held.pop());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let mut ftp = AsyncRustlsFtpStream::connect(&address).await.unwrap();
            if ftp.login(&created.folder.username, &created.password).await.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("signs in once a session is free");
    drop(held);
    server.finish().await;
    env.st.db.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ended_receive_folder_can_be_renamed_with_its_end_date_kept_and_found_by_searching() {
    let (env, admin, created, server) = fixture(false).await;
    let ended = crate::util::now() - 3600;
    sqlx::query("UPDATE ftp_receive_folders SET expires_at = ? WHERE id = ?").bind(ended).bind(&created.folder.id).execute(&env.st.db).await.unwrap();
    let edit = |name: &str, expires_at: Option<i64>| {
        let change = manage::Change { name: name.into(), enabled: false, allow_list: false, allow_read: false, replace_existing: true, expires_at };
        Json(manage::Edit { change, password: Some(testutil::password().into()), code: None })
    };
    let update = async |name: &str, expires_at: Option<i64>| {
        manage::update(State(env.st.clone()), admin.clone(), axum::extract::Path(created.folder.id.clone()), edit(name, expires_at)).await
    };
    // Renamed and stopped, its end left as it is although it has passed
    let _ = update("Old scanner", Some(ended)).await.unwrap();
    // A new end date must still be in the future
    assert_eq!(update("Old scanner", Some(ended - 60)).await.unwrap_err().status, axum::http::StatusCode::BAD_REQUEST);
    // Searched for on the server: by its name, its creator, its folder; nothing else
    let search = async |text: &str| {
        let q = axum::extract::Query(manage::ListQuery { search: Some(text.into()), ..Default::default() });
        let Json(list) = manage::list(State(env.st.clone()), admin.clone(), q).await.unwrap();
        list.folders.len()
    };
    assert_eq!((search("OLD").await, search(&admin.username).await, search("incoming").await, search("nothing like it").await), (1, 1, 1, 0));
    server.finish().await;
    env.st.db.close().await;
}
