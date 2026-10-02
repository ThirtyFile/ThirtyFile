//! The TLS set-up of connections ThirtyFile makes to other servers (FTPS, email)

use std::{io, sync::Arc};

use rustls::{
    ClientConfig, DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::CryptoProvider,
    pki_types::{CertificateDer, ServerName, UnixTime},
};

/// The TLS set-up is built once for each kind (verifying or not) and shared by every connection (FTPS in
/// storage/ftp.rs, email in mail.rs): loading the platform's certificate verifier is slow
pub fn client_tls(insecure: bool) -> io::Result<Arc<ClientConfig>> {
    static CONFIGS: [std::sync::OnceLock<Arc<ClientConfig>>; 2] = [std::sync::OnceLock::new(), std::sync::OnceLock::new()];
    let slot = &CONFIGS[usize::from(insecure)];
    Ok(match slot.get() {
        Some(c) => c.clone(),
        None => {
            let c = tls_config(insecure)?;
            slot.get_or_init(|| c).clone()
        }
    })
}

fn tls_config(insecure: bool) -> io::Result<Arc<ClientConfig>> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let verifier: Arc<dyn ServerCertVerifier> = if insecure {
        Arc::new(AcceptAnyCert(provider.clone()))
    } else {
        Arc::new(rustls_platform_verifier::Verifier::new(provider.clone()).map_err(io::Error::other)?)
    };
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Skips certificate verification (self-signed certificates); signatures are still checked as usual
#[derive(Debug)]
struct AcceptAnyCert(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::TcpListener,
    };

    use super::*;
    use crate::storage::{Storage, StorageError, ftp};

    /// An FTPS (AUTH TLS) or SMTP (TLS from the start) server on a local port, with a self-signed certificate for
    /// 127.0.0.1: answers every command it gets once the connection is secure
    async fn self_signed_server(ftp: bool) -> u16 {
        let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into(), "localhost".into()]).unwrap();
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert.cert.der().clone()], key)
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    if ftp {
                        tcp.write_all(b"220 ready\r\n").await.unwrap();
                        let mut line = String::new();
                        BufReader::new(&mut tcp).read_line(&mut line).await.unwrap();
                        assert_eq!(line.trim_end(), "AUTH TLS");
                        tcp.write_all(b"234 go ahead\r\n").await.unwrap();
                    }
                    let Ok(tls) = acceptor.accept(tcp).await else { return };
                    let mut io = BufReader::new(tls);
                    if !ftp {
                        io.get_mut().write_all(b"220 ready\r\n").await.unwrap();
                    }
                    loop {
                        let mut line = String::new();
                        if io.read_line(&mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                        let answer: &[u8] = match line.split_whitespace().next().unwrap_or_default() {
                            "USER" => b"331 password please\r\n",
                            "PASS" => b"230 signed in\r\n",
                            "EHLO" => b"250 localhost\r\n",
                            "DATA" => {
                                io.get_mut().write_all(b"354 go\r\n").await.unwrap();
                                loop {
                                    let mut l = String::new();
                                    if io.read_line(&mut l).await.unwrap_or(0) == 0 || l == ".\r\n" {
                                        break;
                                    }
                                }
                                b"250 queued\r\n"
                            }
                            "QUIT" => {
                                let _ = io.get_mut().write_all(b"221 bye\r\n").await;
                                return;
                            }
                            // PBSZ, PROT, TYPE and NOOP; MAIL and RCPT
                            _ if ftp => b"200 ok\r\n",
                            _ => b"250 ok\r\n",
                        };
                        io.get_mut().write_all(answer).await.unwrap();
                    }
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn a_certificate_nobody_vouches_for_is_refused_unless_checking_is_turned_off() {
        let port = self_signed_server(false).await;
        let connect = |insecure: bool| async move {
            let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            let name = ServerName::try_from("127.0.0.1").unwrap();
            tokio_rustls::TlsConnector::from(client_tls(insecure).unwrap()).connect(name, tcp).await
        };
        let err = connect(false).await.unwrap_err();
        assert!(err.to_string().contains("certificate"), "{err}");
        let mut tls = connect(true).await.unwrap();
        let mut greeting = String::new();
        BufReader::new(&mut tls).read_line(&mut greeting).await.unwrap();
        assert_eq!(greeting, "220 ready\r\n", "still encrypted, with the server's signature checked");
        // The set-up is built once for each kind
        assert!(Arc::ptr_eq(&client_tls(false).unwrap(), &client_tls(false).unwrap()));
        assert!(!Arc::ptr_eq(&client_tls(false).unwrap(), &client_tls(true).unwrap()));
    }

    #[tokio::test]
    async fn email_over_tls_refuses_a_self_signed_certificate() {
        let port = self_signed_server(false).await;
        let msg = crate::mail::Message { to: "amy@example.com", subject: "Test", body: "Test" };
        let cfg = crate::mail::SmtpSettings { security: crate::mail::Security::Tls, username: String::new(), ..crate::mail::tests::settings(port) };
        let err = crate::mail::send(&cfg, "Drive", &msg).await.unwrap_err();
        assert!(err.contains("secure connection failed"), "{err}");
        // A server on the local network with a certificate of its own, when the administrator says so
        let unchecked = crate::mail::SmtpSettings { insecure: true, ..cfg };
        crate::mail::send(&unchecked, "Drive", &msg).await.unwrap();
    }

    #[tokio::test]
    async fn ftps_refuses_a_self_signed_certificate() {
        let port = self_signed_server(true).await;
        let cfg = ftp::FtpConfig {
            host: "127.0.0.1".into(),
            port,
            username: "files".into(),
            password: crate::testutil::password().into(),
            tls: true,
            ..Default::default()
        };
        let err = ftp::FtpStorage::new(&cfg).unwrap().ping().await.unwrap_err();
        let message = err.get_ref().and_then(|e| e.downcast_ref::<StorageError>()).map(|e| e.message).unwrap_or_default();
        assert!(message.starts_with("FTPS encrypted connection failed"), "{err}");
        let unchecked = ftp::FtpConfig { tls_insecure: true, ..cfg };
        ftp::FtpStorage::new(&unchecked).unwrap().ping().await.unwrap();
    }
}
