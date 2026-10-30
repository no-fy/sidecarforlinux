//! TLS 1.3 with mutual certificate authentication.
//!
//! Neither side uses a CA. The handshake proves each peer holds the private
//! key of the certificate it presents (signatures are verified normally);
//! *which* certificates are acceptable is decided right after the handshake
//! by comparing the SHA-256 fingerprint with the pinned ones (or by running
//! the pairing exchange). See docs/protocol.md § Security.

use crate::identity::Identity;
use anyhow::{anyhow, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{
    verify_tls12_signature, verify_tls13_signature, CryptoProvider, WebPkiSupportedAlgorithms,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use sidecar_protocol::pairing::{self, Fingerprint};
use std::sync::Arc;

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

#[derive(Debug)]
struct AnyCert(WebPkiSupportedAlgorithms);

impl ServerCertVerifier for AnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
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
        verify_tls12_signature(message, cert, dss, &self.0)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}

impl ClientCertVerifier for AnyCert {
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
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.0)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}

fn key(id: &Identity) -> PrivateKeyDer<'static> {
    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(id.key_der.clone()))
}

/// Daemon side: TLS client presenting the daemon certificate.
pub fn client_config(id: &Identity) -> Result<Arc<rustls::ClientConfig>> {
    let p = provider();
    let verifier = Arc::new(AnyCert(p.signature_verification_algorithms));
    let mut cfg = rustls::ClientConfig::builder_with_provider(p)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![CertificateDer::from(id.cert_der.clone())], key(id))?;
    cfg.alpn_protocols = vec![sidecar_protocol::ALPN.to_vec()];
    Ok(Arc::new(cfg))
}

/// iPad side (used by the fake iPad): TLS server requiring a client cert.
pub fn server_config(id: &Identity) -> Result<Arc<rustls::ServerConfig>> {
    let p = provider();
    let verifier = Arc::new(AnyCert(p.signature_verification_algorithms));
    let mut cfg = rustls::ServerConfig::builder_with_provider(p)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![CertificateDer::from(id.cert_der.clone())], key(id))?;
    cfg.alpn_protocols = vec![sidecar_protocol::ALPN.to_vec()];
    Ok(Arc::new(cfg))
}

pub fn server_name() -> ServerName<'static> {
    ServerName::try_from("sidecar.invalid").expect("static name")
}

/// Fingerprint of the peer's end-entity certificate after the handshake.
pub fn peer_fingerprint(certs: Option<&[CertificateDer<'_>]>) -> Result<Fingerprint> {
    let c = certs
        .and_then(|c| c.first())
        .ok_or_else(|| anyhow!("peer presented no certificate"))?;
    Ok(pairing::fingerprint(c.as_ref()))
}
