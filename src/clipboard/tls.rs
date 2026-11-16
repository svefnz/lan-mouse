//! TLS configuration for the clipboard channel.
//!
//! The channel reuses the certificate lan-mouse already generated for
//! the input protocol, so a peer has one identity and one fingerprint.
//! Trust is the existing `authorized_keys` fingerprint list: the server
//! requires a client certificate whose fingerprint is in that list, and
//! the client accepts the server it dialed.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use rustls::{
    DigitallySignedStruct, DistinguishedName, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::CryptoProvider,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
};
use webrtc_dtls::crypto::Certificate;

use crate::crypto;

fn cert_chain(cert: &Certificate) -> Vec<CertificateDer<'static>> {
    cert.certificate.clone()
}

fn private_key(cert: &Certificate) -> PrivateKeyDer<'static> {
    // rcgen serializes keys as PKCS#8
    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        cert.private_key.serialized_der.clone(),
    ))
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Rejects any client whose certificate fingerprint is not authorized.
#[derive(Debug)]
struct FingerprintVerifier {
    authorized: Arc<RwLock<HashMap<String, String>>>,
    provider: Arc<CryptoProvider>,
}

impl ClientCertVerifier for FingerprintVerifier {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        let fingerprint = crypto::generate_fingerprint(end_entity.as_ref());
        if self
            .authorized
            .read()
            .expect("lock")
            .contains_key(&fingerprint)
        {
            Ok(ClientCertVerified::assertion())
        } else {
            log::debug!("clipboard: rejecting unauthorized peer {fingerprint}");
            Err(rustls::Error::General(format!(
                "unauthorized fingerprint {fingerprint}"
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Accepts any server certificate.
///
/// The client only ever dials a peer it resolved from its own
/// configuration, and the peer proves possession of its key during the
/// handshake; there is no certificate authority to chain to because
/// every lan-mouse certificate is self-signed.
#[derive(Debug)]
struct AcceptAnyServerCert {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub fn server_config(
    cert: &Certificate,
    authorized: Arc<RwLock<HashMap<String, String>>>,
) -> Result<rustls::ServerConfig, rustls::Error> {
    let provider = provider();
    let verifier = Arc::new(FingerprintVerifier {
        authorized,
        provider: provider.clone(),
    });
    rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_client_cert_verifier(verifier)
        .with_single_cert(cert_chain(cert), private_key(cert))
}

pub fn client_config(cert: &Certificate) -> Result<rustls::ClientConfig, rustls::Error> {
    let provider = provider();
    rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert { provider }))
        .with_client_auth_cert(cert_chain(cert), private_key(cert))
}
