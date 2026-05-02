// TLS configuration helpers.
//
// Server: set VEKTALITE_TLS_CERT and VEKTALITE_TLS_KEY (PEM paths) to enable TLS.
// Client: set VEKTALITE_TLS_CA (PEM path) for a custom CA, or VEKTALITE_TLS_INSECURE=1
//         to skip verification (dev mode only — vulnerable to MITM).

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use rustls_pemfile::{certs, private_key};

use crate::error::{Result, VektaError};

pub fn server_config(
    cert_path: impl AsRef<Path>,
    key_path: impl AsRef<Path>,
) -> Result<Arc<ServerConfig>> {
    let cert_chain = load_certs(cert_path)?;
    let key = load_key(key_path)?;
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)
        .map_err(|e| VektaError::Network(format!("TLS server config: {e}")))?;
    Ok(Arc::new(config))
}

pub fn client_config(ca_path: Option<&str>, insecure: bool) -> Result<Arc<ClientConfig>> {
    let config = if insecure {
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertVerifier))
            .with_no_client_auth()
    } else if let Some(ca) = ca_path {
        let ca_cert = load_certs(ca)?
            .into_iter()
            .next()
            .ok_or_else(|| VektaError::Network("CA cert file is empty".into()))?;
        let mut roots = RootCertStore::empty();
        roots
            .add(ca_cert)
            .map_err(|e| VektaError::Network(format!("CA cert: {e}")))?;
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth()
    } else {
        return Err(VektaError::Network(
            "TLS: set VEKTALITE_TLS_CA (CA cert path) or VEKTALITE_TLS_INSECURE=1".into(),
        ));
    };
    Ok(Arc::new(config))
}

fn load_certs(path: impl AsRef<Path>) -> Result<Vec<CertificateDer<'static>>> {
    let mut r = BufReader::new(
        File::open(path).map_err(|e| VektaError::Network(format!("cert file: {e}")))?,
    );
    certs(&mut r)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| VektaError::Network(format!("cert parse: {e}")))
}

fn load_key(path: impl AsRef<Path>) -> Result<PrivateKeyDer<'static>> {
    let mut r = BufReader::new(
        File::open(path).map_err(|e| VektaError::Network(format!("key file: {e}")))?,
    );
    private_key(&mut r)
        .map_err(|e| VektaError::Network(format!("key parse: {e}")))?
        .ok_or_else(|| VektaError::Network("no private key found in file".into()))
}

// --- NoCertVerifier (insecure / dev mode) ---

#[derive(Debug)]
struct NoCertVerifier;

impl rustls::client::danger::ServerCertVerifier for NoCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _msg: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _msg: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        use rustls::SignatureScheme;
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
        ]
    }
}
