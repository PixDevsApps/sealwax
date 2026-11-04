//! rustls client configurations. Production uses the platform verifier only; the testbed CA is
//! trusted explicitly through a separate constructor, never by weakening verification.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use rustls_platform_verifier::BuilderVerifierExt;

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// The production path: the system trust store through rustls-platform-verifier.
pub fn platform() -> Result<Arc<ClientConfig>> {
    let config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_platform_verifier()?
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Trusts exactly the certificates in `ca_pem` (the testbed CA) and nothing else.
pub fn with_roots(ca_pem: &Path) -> Result<Arc<ClientConfig>> {
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_file_iter(ca_pem)
        .with_context(|| format!("read {}", ca_pem.display()))?
    {
        roots.add(cert?)?;
    }
    let config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Server side for the fault-injection proxy, which terminates TLS with the testbed certificate.
pub fn server(cert_pem: &Path, key_pem: &Path) -> Result<Arc<ServerConfig>> {
    let certs = CertificateDer::pem_file_iter(cert_pem)?.collect::<Result<Vec<_>, _>>()?;
    let key = PrivateKeyDer::from_pem_file(key_pem)?;
    let config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    #[test]
    fn platform_verifier_builds() {
        super::platform().unwrap();
    }
}
