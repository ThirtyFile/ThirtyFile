//! PEM loading must retain the chain, supported key formats and fail-closed configuration errors.

use std::{path::PathBuf, sync::Arc, time::Duration};

use axum::http::StatusCode;
use base64::{Engine, engine::general_purpose::STANDARD};
use rustls::pki_types::{PrivateKeyDer, pem::PemObject};

use crate::ftp::{Settings, tls};

struct Files {
    dir: PathBuf,
    settings: Settings,
}

impl Files {
    fn new(cert: &str, key: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("thirtyfile-ftp-tls-{}", crate::util::new_id()));
        std::fs::create_dir(&dir).unwrap();
        let cert_path = dir.join("cert.pem");
        let key_path = dir.join("key.pem");
        std::fs::write(&cert_path, cert).unwrap();
        std::fs::write(&key_path, key).unwrap();
        Self { dir, settings: Settings { address: String::new(), passive: String::new(), host: String::new(), cert: Some(cert_path), key: Some(key_path) } }
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn pem(label: &str, der: &[u8]) -> String {
    format!("-----BEGIN {label}-----\n{}\n-----END {label}-----\n", STANDARD.encode(der))
}

fn certificate(key: &rcgen::KeyPair) -> String {
    pem("CERTIFICATE", rcgen::CertificateParams::new(vec!["example.com".into()]).unwrap().self_signed(key).unwrap().der())
}

#[test]
fn pkcs8_keys_load_with_the_matching_certificate_and_skip_other_pem_sections() {
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = certificate(&key);
    let key_pem = pem("PRIVATE KEY", &key.serialize_der());
    let files = Files::new(&format!("{key_pem}{cert}"), &format!("{cert}{key_pem}"));
    assert!(tls(&files.settings, false).unwrap().is_some());
}

#[test]
fn pkcs1_and_sec1_keys_load_with_the_matching_certificate() {
    for (label, der, algorithm) in [("RSA PRIVATE KEY", RSA_KEY, &rcgen::PKCS_RSA_SHA256), ("EC PRIVATE KEY", EC_KEY, &rcgen::PKCS_ECDSA_P256_SHA256)] {
        let key_pem = pem(label, &STANDARD.decode(der).unwrap());
        let private = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).unwrap();
        match label {
            "RSA PRIVATE KEY" => assert!(matches!(private, PrivateKeyDer::Pkcs1(_))),
            "EC PRIVATE KEY" => assert!(matches!(private, PrivateKeyDer::Sec1(_))),
            _ => unreachable!(),
        }
        let key = rcgen::KeyPair::from_der_and_sign_algo(&private, algorithm).unwrap();
        let files = Files::new(&certificate(&key), &key_pem);
        assert!(tls(&files.settings, false).unwrap().is_some(), "{label}");
    }
}

#[tokio::test]
async fn the_complete_certificate_chain_is_sent_in_order_and_validates_in_a_tls_handshake() {
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca_params = rcgen::CertificateParams::new(vec!["example.com".into()]).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign];
    ca_params.distinguished_name.push(rcgen::DnType::CommonName, "Example CA");
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["example.com".into()]).unwrap().signed_by(&key, &rcgen::Issuer::from_params(&ca_params, &ca_key)).unwrap();
    let expected = vec![cert.der().clone(), ca.der().clone()];
    let files = Files::new(&format!("{}{}", pem("CERTIFICATE", cert.der()), pem("CERTIFICATE", ca.der())), &pem("PRIVATE KEY", &key.serialize_der()));
    let server = tokio_rustls::TlsAcceptor::from(tls(&files.settings, false).unwrap().unwrap());
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca.der().clone()).unwrap();
    let client = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let client = tokio_rustls::TlsConnector::from(Arc::new(client));
    let (server_io, client_io) = tokio::io::duplex(16 * 1024);
    let (server, client) =
        tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(server.accept(server_io), client.connect("example.com".try_into().unwrap(), client_io)) })
            .await
            .unwrap();
    let _server = server.unwrap();
    let client = client.unwrap();
    assert_eq!(client.get_ref().1.peer_certificates().unwrap(), expected);
}

#[test]
fn empty_and_certificate_only_key_files_keep_the_missing_key_error() {
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = certificate(&key);
    for contents in ["", "No PEM blocks here", &cert] {
        let files = Files::new(&cert, contents);
        let error = tls(&files.settings, false).unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.message, "The FTP TLS key is missing");
    }
}

#[test]
fn empty_or_malformed_certificates_fail_even_when_private_plaintext_is_allowed() {
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = certificate(&key);
    let malformed = "-----BEGIN CERTIFICATE-----\n!bad base64!\n-----END CERTIFICATE-----\n";
    for contents in [String::new(), malformed.into(), format!("{cert}{malformed}"), "-----BEGIN CERTIFICATE-----\nAAAA\n".into(), pem("CERTIFICATE", b"not DER")] {
        let files = Files::new(&contents, &pem("PRIVATE KEY", &key.serialize_der()));
        assert_eq!(tls(&files.settings, true).unwrap_err().status, StatusCode::INTERNAL_SERVER_ERROR);
    }
}

#[test]
fn malformed_keys_fail_without_using_a_later_key_or_plaintext() {
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = certificate(&key);
    let valid = pem("PRIVATE KEY", &key.serialize_der());
    for contents in [
        format!("-----BEGIN PRIVATE KEY-----\n!bad base64!\n-----END PRIVATE KEY-----\n{valid}"),
        "-----BEGIN PRIVATE KEY-----\nAAAA\n".into(),
        pem("PRIVATE KEY", b"not DER"),
    ] {
        let files = Files::new(&cert, &contents);
        assert_eq!(tls(&files.settings, true).unwrap_err().status, StatusCode::INTERNAL_SERVER_ERROR);
    }
}

#[test]
fn a_nonmatching_first_private_key_is_rejected_even_if_a_matching_key_follows() {
    let key = rcgen::KeyPair::generate().unwrap();
    let wrong = rcgen::KeyPair::generate().unwrap();
    let files = Files::new(&certificate(&key), &format!("{}{}", pem("PRIVATE KEY", &wrong.serialize_der()), pem("PRIVATE KEY", &key.serialize_der())));
    assert_eq!(tls(&files.settings, false).unwrap_err().status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn missing_files_or_a_partial_tls_configuration_never_fall_back_to_plaintext() {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut files = Files::new(&certificate(&key), &pem("PRIVATE KEY", &key.serialize_der()));
    let key_path = files.settings.key.take().unwrap();
    assert_eq!(tls(&files.settings, true).unwrap_err().status, StatusCode::BAD_REQUEST);
    files.settings.key = Some(key_path);
    std::fs::remove_file(files.settings.cert.as_ref().unwrap()).unwrap();
    assert_eq!(tls(&files.settings, true).unwrap_err().status, StatusCode::INTERNAL_SERVER_ERROR);
    files.settings.cert = None;
    files.settings.key = None;
    assert!(tls(&files.settings, true).unwrap().is_none());
    assert_eq!(tls(&files.settings, false).unwrap_err().status, StatusCode::BAD_REQUEST);
}

// Public, test-only keys generated for these encoding fixtures. They are never used by the application.
const RSA_KEY: &str = "MIIEowIBAAKCAQEAnv8dFPByfTkuTHCfKSV3J5g5tleiLAySWVDBe2wLOWT6sjqaXyPPmwzY5MFFcjofta2ZWfCFCAFVUMd4iybkjlg/LU4SvJX3zZhkbHc89IJdxH9DoiRj5/H2pw3HJ/u2Fx20Io4pzS6Pjeo6Dwkrwtm4JFnEMzCupZI8qywooOldmzvuzjkT+/ZsVEC+9oVvpKam/RWBkh/1b6IdC98ld6QqAhJ5XRieU+eJulZBqKMlpKqZ1uKq0D+3NSTavV6a47nbOMFz3+T9n57YTz1uld2aD7I7Sc+Eh9siv4stE9Ll+YrXZqkGZrMrsSYnALUsq/i5wvbQHfi5xWYQwbH+bQIDAQABAoIBAAFw6oCQFVmRNRw4QRkMX+IZ/Rvr+2pOytzIYJiPyCJDCgqngVKvbmhHW4fnQIvifvsUkblA9tO1v2I2DJ+mQJgRsLDmbkELUu2oHfDlu8WzkMoBJUnI7qbhiIzHETEJlQLsn/9YLoTPkTmGD/Sm7qCbfJHHxoF4RXz2sFoU0mFy+F0k9h05DoeYNMAzOu6xtdH+R+7XVXXqy8XaoI0n+EL6HttdHdRGaTflQ2uTLOz0Z2UiLqc9clAU7dz+vani2pCgP+hzLso111RL/dRwS+gsCzJOYsBwBRpYqsTbe7h6fq0Nrjze+yygZWxbLa0qo8mxkdapKSrJUWhj2lZKWHkCgYEA2hKh8Nmqx6DHnYA0Q7gDY5UOzkFmf+7omeLrDWp9xfHPbGgErGI4E4+Cw9rqeCsDnd/LTFzkHeBdO4FXFPsI2bjH9W/dHV520VVpuzlYc/fM7FWdFnBxr42QyJrpvDzqoJj6ONDUXST4Iu+NT3xFahfZYNVJqUrfK/hPthG+Nr8CgYEAuqYy4JHJ4TF99XDur9sbQMSPQkUM1Ebgbxgt1fkTL7US33CxDsApKhioTNaLdq/nyJ1nlUSCnz3JgqFxQnjYk7VeRgWe2OwKYoiPkGbdEozY7H+mW6jDvYwPQjafd8BnV6xDWWQsj5E3IkpjYwFIj5RmOqkmwtxfSdUp3aRz4dMCgYBrHIfKQz+s+f3SEKybLoELoqU5DcqIwBIjs998IXhRh6o4NwdEnKGqiezSwm40Q/xsyQWUJnQtcIyas7bQKVV5i1c76IwU5tc5jth2C360x1wyIQMdiHFupvwcbGqt+MXNyBXvXjNQ5N20WzFvfzyhFcHB3EFoE8x2TmropOJnBQKBgF6y4k9kSJw0AydbZA7fSSWEVPcXl5nAWDClgOP2WpCy+19YiszkHD6aghFyLRJLz3rOufZW6zsmGy5nANLsA151/93hDiYbJG23wbdPKU03HvxnC52ns6BhBoDeu+wDN6uY4Bbjp2Q0FlgUi/Z4eTs33OJYXi0xqDsKpxIoVQ83AoGBALJTeAiLXnqRATQmHFiWFk8zZs8AHMrOqcCc8RP+rMRC2I07VOaAj4cZzDIfnQkW7fAkU2tr0Hwt5wCQI2Gj19LMSj60nqJe3GB5os8uWRa+TfqNaO110H87VkWxa9cUHzE7hd7yz+bjBisbrdtYAnyAaBe0foPXA4LTKAXM1fuT";
const EC_KEY: &str = "MHcCAQEEINl14KZskPt/rA/hwmbbNKQ3mMj6Ac9g62UP+h7VindcoAoGCCqGSM49AwEHoUQDQgAEnTRnP1lVDuG9b5MT6X8mw4igQoKedoBywZM/FaesckUNAjjGCYfsJaWudIJvSosmN98aFqXabH1RTL/eiNVxbQ==";
