//! A pinned certificate (issue #9): a server on this computer, such as Proton Mail Bridge, whose
//! self-signed certificate the system doesn't trust, is trusted with exactly one certificate,
//! named by its SHA-256 in `accounts.toml`. The handshake's signature is still checked against
//! that certificate's key; its names, dates and CA flag are not, since the user chose it. Only a
//! loopback host can have one, and the connection must reach a loopback address.

use super::{ConnectError, provider, tcp};
use crate::autoconfig::probe;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, SignatureScheme};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use sealwax_domain::account::{
    CertificatePin, Endpoint, LoginFailure, Protocol, Security, is_loopback,
};
use sha2::{Digest, Sha256};
use std::io;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// The SHA-256 of a certificate's DER bytes.
pub fn fingerprint(der: &[u8]) -> CertificatePin {
    CertificatePin(Sha256::digest(der).into())
}

/// Refuses a connection that didn't reach this computer, whatever the host name said.
pub(crate) fn loopback_peer(tcp: &TcpStream) -> Result<(), ConnectError> {
    match tcp.peer_addr() {
        Ok(address) if address.ip().to_canonical().is_loopback() => Ok(()),
        Ok(_) => Err(ConnectError::NotLoopback),
        Err(error) => Err(ConnectError::Connect(error)),
    }
}

/// The rustls configuration that accepts exactly the certificate `pin` names.
pub(crate) fn pinned_config(pin: CertificatePin) -> Result<Arc<ClientConfig>, ConnectError> {
    config(Arc::new(Pinned {
        pin,
        provider: provider(),
    }))
    .map_err(|error| ConnectError::Tls(io::Error::other(error)))
}

fn config(verifier: Arc<dyn ServerCertVerifier>) -> Result<Arc<ClientConfig>, rustls::Error> {
    Ok(Arc::new(
        ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth(),
    ))
}

#[derive(Debug)]
struct Pinned {
    pin: CertificatePin,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if fingerprint(end_entity) == self.pin {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
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
        verify_tls13_signature(
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

/// Notes the certificate a server shows and ends the handshake there, so nothing is ever sent
/// over it.
#[derive(Debug)]
struct Reader {
    seen: Arc<Mutex<Option<CertificatePin>>>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Reader {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        *self.seen.lock().unwrap_or_else(PoisonError::into_inner) = Some(fingerprint(end_entity));
        Err(rustls::Error::InvalidCertificate(
            CertificateError::ApplicationVerificationFailure,
        ))
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("certificate read only".into()))
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("certificate read only".into()))
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// The fingerprint of the certificate a server on this computer shows, for the user to trust:
/// TCP, the plaintext half of STARTTLS when the endpoint uses it (no login, nothing but
/// CAPABILITY, STLS or `EHLO [127.0.0.1]`), and the handshake up to the certificate, where it
/// stops.
pub async fn read_certificate(
    endpoint: &Endpoint,
    protocol: Protocol,
    timeout: Duration,
) -> Result<CertificatePin, LoginFailure> {
    if !is_loopback(&endpoint.host) {
        return Err(LoginFailure::Other);
    }
    tokio::time::timeout(timeout, read(endpoint, protocol, timeout))
        .await
        .unwrap_or(Err(LoginFailure::Timeout))
}

async fn read(
    endpoint: &Endpoint,
    protocol: Protocol,
    timeout: Duration,
) -> Result<CertificatePin, LoginFailure> {
    let stream = tcp(&endpoint.host, endpoint.port, timeout)
        .await
        .map_err(|error| error.failure())?;
    loopback_peer(&stream).map_err(|error| error.failure())?;
    let stream = match endpoint.security {
        Security::Tls => stream,
        Security::StartTls => probe::starttls(protocol, stream).await.map_err(|error| {
            tracing::debug!(stage = ?error.stage, "reading a certificate: STARTTLS failed");
            LoginFailure::NoTls
        })?,
    };
    let seen = Arc::new(Mutex::new(None));
    let reader = Arc::new(Reader {
        seen: seen.clone(),
        provider: provider(),
    });
    let name = ServerName::try_from(endpoint.host.clone()).map_err(|_| LoginFailure::Other)?;
    let config = config(reader).map_err(|_| LoginFailure::Other)?;
    let handshake = TlsConnector::from(config).connect(name, stream).await;
    let seen = *seen.lock().unwrap_or_else(PoisonError::into_inner);
    match (seen, handshake) {
        (Some(pin), _) => Ok(pin),
        (None, Ok(_)) => Err(LoginFailure::Other),
        // No certificate came: the server doesn't speak TLS there.
        (None, Err(error)) if error.kind() == io::ErrorKind::TimedOut => Err(LoginFailure::Timeout),
        (None, Err(_)) => Err(LoginFailure::NoTls),
    }
}

#[cfg(test)]
mod tests;
