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
