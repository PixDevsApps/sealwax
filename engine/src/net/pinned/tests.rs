//! Pinned certificates against scripted servers with a Bridge-like certificate.

use super::*;
use crate::net::testing::{Peer, bind, self_signed};
use crate::net::{Trust, tls};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const WAIT: Duration = Duration::from_secs(10);

/// The plaintext half a scripted server plays: what it expects to read (nothing for its
/// greeting), and its answer.
type Script = &'static [(&'static str, &'static str)];

fn endpoint(port: u16, security: Security, certificate: Option<CertificatePin>) -> Endpoint {
    Endpoint {
        host: "127.0.0.1".into(),
        port,
        security,
        certificate,
    }
}

#[tokio::test]
async fn bridge_s_certificate_fails_even_as_the_only_trusted_root() {
    let bridge = self_signed();
    let server = bridge.server.clone();
    let (listener, port) = bind().await;
    tokio::spawn(async move {
        let peer = Peer::accept(&listener).await;
        let _ = tokio_rustls::TlsAcceptor::from(server)
            .accept(peer.into_inner())
            .await;
    });
    let stream = tcp("127.0.0.1", port, WAIT).await.unwrap();
    let error = tls(
        stream,
        bridge.trust.client_config().unwrap(),
        &endpoint(port, Security::Tls, None),
        WAIT,
    )
    .await
    .unwrap_err();
    assert_eq!(error.failure(), LoginFailure::Certificate);
}

#[tokio::test]
async fn the_pinned_certificate_is_trusted_and_no_other() {
    let bridge = self_signed();
    let pin = fingerprint(&bridge.der);
    let wrong = CertificatePin([0x11; 32]);
    for (certificate, works) in [(Some(pin), true), (Some(wrong), false)] {
        let server = bridge.server.clone();
        let (listener, port) = bind().await;
        let accepted = tokio::spawn(async move {
            let peer = Peer::accept(&listener).await;
            let Ok(stream) = tokio_rustls::TlsAcceptor::from(server)
                .accept(peer.into_inner())
                .await
            else {
                return None;
            };
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).await.ok()?;
            Some(line)
        });
        let stream = tcp("127.0.0.1", port, WAIT).await.unwrap();
        let result = tls(
            stream,
            Trust::Platform.client_config().unwrap(),
            &endpoint(port, Security::Tls, certificate),
            WAIT,
        )
        .await;
        match result {
            Ok(mut stream) => {
                assert!(works);
                stream.write_all(b"hello\r\n").await.unwrap();
                stream.flush().await.unwrap();
                assert_eq!(accepted.await.unwrap().as_deref(), Some("hello\r\n"));
            }
            Err(error) => {
                assert!(!works);
                assert_eq!(error.failure(), LoginFailure::Certificate);
                assert_eq!(accepted.await.unwrap(), None);
            }
        }
    }
}

#[tokio::test]
async fn a_pin_is_ignored_off_this_computer() {
    let endpoint = Endpoint {
        host: "localhost.example.test".into(),
        port: 993,
        security: Security::Tls,
        certificate: Some(CertificatePin([0x22; 32])),
    };
    assert_eq!(endpoint.pinned(), None);
    assert_eq!(
        read_certificate(&endpoint, Protocol::Imap, WAIT).await,
        Err(LoginFailure::Other)
    );
}

#[tokio::test]
async fn a_certificate_is_read_over_tls_and_after_each_starttls() {
    let bridge = self_signed();
    let pin = fingerprint(&bridge.der);
    let cases: [(Protocol, Security, Script); 4] = [
        (Protocol::Imap, Security::Tls, &[]),
        (
            Protocol::Imap,
            Security::StartTls,
            &[
                ("", "* OK [CAPABILITY IMAP4rev1 STARTTLS] Bridge ready\r\n"),
                (
                    "a1 CAPABILITY",
                    "* CAPABILITY IMAP4rev1 STARTTLS\r\na1 OK done\r\n",
                ),
                ("a2 STARTTLS", "a2 OK begin TLS\r\n"),
            ],
        ),
        (
            Protocol::Pop3,
            Security::StartTls,
            &[("", "+OK ready\r\n"), ("STLS", "+OK begin TLS\r\n")],
        ),
        (
            Protocol::Smtp,
            Security::StartTls,
            &[
                ("", "220 bridge ESMTP\r\n"),
                ("EHLO [127.0.0.1]", "250-bridge\r\n250 STARTTLS\r\n"),
                ("STARTTLS", "220 go ahead\r\n"),
            ],
        ),
    ];
    for (protocol, security, script) in cases {
        let server = bridge.server.clone();
        let script: Vec<(String, String)> = script
            .iter()
            .map(|(expect, reply)| ((*expect).to_owned(), (*reply).to_owned()))
            .collect();
        let (listener, port) = bind().await;
        let peer = tokio::spawn(async move {
            let mut peer = Peer::accept(&listener).await;
            for (expect, reply) in script {
                if !expect.is_empty() {
                    assert_eq!(peer.line().await.as_deref(), Some(expect.as_str()));
                }
                peer.send(&reply).await;
            }
            let received = peer.received.clone();
            let handshake = tokio_rustls::TlsAcceptor::from(server)
                .accept(peer.into_inner())
                .await;
            (received, handshake.is_ok())
        });
        let read = read_certificate(&endpoint(port, security, None), protocol, WAIT).await;
        assert_eq!(read, Ok(pin), "{protocol:?} {security:?}");
        let (received, handshake) = peer.await.unwrap();
        assert!(!handshake, "the handshake stops at the certificate");
        for word in ["LOGIN", "AUTH", "USER", "PASS"] {
            assert!(!received.contains(word), "{received}");
        }
    }
}

#[tokio::test]
async fn reading_a_plaintext_server_finds_no_certificate() {
    let (listener, port) = bind().await;
    tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.send("* OK [CAPABILITY IMAP4rev1] no TLS here\r\n")
            .await;
        peer.drain().await
    });
    assert_eq!(
        read_certificate(&endpoint(port, Security::Tls, None), Protocol::Imap, WAIT).await,
        Err(LoginFailure::NoTls)
    );
}
