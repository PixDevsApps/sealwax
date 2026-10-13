//! The connection layer against scripted servers: STARTTLS that never downgrades, the login
//! fallbacks, XOAUTH2, ID, QRESYNC and how refusals and certificates are classified.

use super::*;
use crate::net::Trust;
use crate::net::testing::{Peer, bind, self_signed, test_ca};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use sealwax_domain::account::{CertificatePin, LoginFailure};

const PASSWORD: Credentials<'static> = Credentials::Password {
    username: "user",
    password: "secret-password",
};

fn endpoint(port: u16, security: Security) -> Endpoint {
    Endpoint {
        host: "127.0.0.1".into(),
        port,
        security,
        certificate: None,
    }
}

fn assert_no_credentials(received: &str) {
    for secret in ["AUTHENTICATE", "LOGIN", "secret-password", "user"] {
        assert!(!received.contains(secret), "{secret} sent: {received}");
    }
}

#[tokio::test]
async fn a_refused_starttls_never_falls_back_to_plaintext() {
    let ca = test_ca();
    let (listener, port) = bind().await;
    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.send("* OK [CAPABILITY IMAP4rev1 AUTH=PLAIN] plaintext only\r\n")
            .await;
        let tag = peer.expect("STARTTLS").await;
        peer.send(&format!("{tag} NO not here\r\n")).await;
        peer.drain().await
    });
    let result = connect(
        &endpoint(port, Security::StartTls),
        PASSWORD,
        ca.trust.client_config().unwrap(),
    )
    .await;
    let error = result.unwrap_err();
    assert!(matches!(error, ImapError::StartTls), "{error}");
    assert_eq!(error.failure(), LoginFailure::NoTls);
    assert_no_credentials(&server.await.unwrap().replace("STARTTLS", ""));
}

#[tokio::test]
async fn a_preauth_greeting_on_a_starttls_port_is_refused() {
    let ca = test_ca();
    let (listener, port) = bind().await;
    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.send("* PREAUTH [CAPABILITY IMAP4rev1] logged in already\r\n")
            .await;
        peer.drain().await
    });
    let error = connect(
        &endpoint(port, Security::StartTls),
        PASSWORD,
        ca.trust.client_config().unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.failure(), LoginFailure::NoTls);
    assert_eq!(server.await.unwrap(), "", "nothing at all was sent");
}

#[tokio::test]
async fn starttls_asks_capabilities_again_and_falls_back_to_login() {
    let ca = test_ca();
    let config = ca.server.clone();
    let (listener, port) = bind().await;
    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.send("* OK [CAPABILITY IMAP4rev1 STARTTLS AUTH=PLAIN] ready\r\n")
            .await;
        let tag = peer.expect("STARTTLS").await;
        peer.send(&format!("{tag} OK begin TLS\r\n")).await;
        let mut peer = peer.tls(config).await;
        let tag = peer.expect("CAPABILITY").await;
        peer.send("* CAPABILITY IMAP4rev1 ID\r\n").await;
        peer.send(&format!("{tag} OK done\r\n")).await;
        let tag = peer.expect("LOGIN \"user\" \"secret-password\"").await;
        peer.send(&format!("{tag} OK logged in\r\n")).await;
        let tag = peer.expect("CAPABILITY").await;
        peer.send("* CAPABILITY IMAP4rev1 ID UNSELECT\r\n").await;
        peer.send(&format!("{tag} OK done\r\n")).await;
        let id = peer.line().await.unwrap();
        let tag = id.split(' ').next().unwrap().to_owned();
        peer.send("* ID (\"name\" \"Test\")\r\n").await;
        peer.send(&format!("{tag} OK id\r\n")).await;
        let tag = peer.expect("LOGOUT").await;
        peer.send("* BYE\r\n").await;
        peer.send(&format!("{tag} OK bye\r\n")).await;
        (id, peer.drain().await)
    });
    let conn = connect(
        &endpoint(port, Security::StartTls),
        PASSWORD,
        ca.trust.client_config().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(conn.mechanism(), Mechanism::Login);
    assert!(conn.caps().has("UNSELECT") && !conn.qresync());
    conn.logout().await;
    let (id, _) = server.await.unwrap();
    assert_eq!(
        id.split_once(' ').unwrap().1,
        format!(
            "ID (\"name\" \"Sealwax\" \"version\" \"{}\")",
            env!("CARGO_PKG_VERSION")
        ),
        "generic ID values only"
    );
}

/// Proton Mail Bridge (issue #9): its self-signed certificate is refused until it is pinned,
/// then the login goes through; another certificate on the same pin is refused before LOGIN.
#[tokio::test]
async fn a_pinned_bridge_certificate_logs_in_and_no_other_does() {
    let bridge = self_signed();
    let pin = crate::net::fingerprint(&bridge.der);
    for (certificate, outcome) in [
        (None, Err(LoginFailure::Certificate)),
        (
            Some(CertificatePin([0x44; 32])),
            Err(LoginFailure::Certificate),
        ),
        (Some(pin), Ok(())),
    ] {
        let config = bridge.server.clone();
        let (listener, port) = bind().await;
        let server = tokio::spawn(async move {
            let mut peer = Peer::accept(&listener).await;
            peer.send("* OK [CAPABILITY IMAP4rev1 STARTTLS AUTH=PLAIN] Bridge ready\r\n")
                .await;
            let tag = peer.expect("STARTTLS").await;
            peer.send(&format!("{tag} OK begin TLS\r\n")).await;
            let plain = peer.into_inner();
            let Ok(tls) = tokio_rustls::TlsAcceptor::from(config).accept(plain).await else {
                return String::new();
            };
            let mut peer = Peer::from_stream(Box::new(tls));
            let tag = peer.expect("CAPABILITY").await;
            peer.send("* CAPABILITY IMAP4rev1\r\n").await;
            peer.send(&format!("{tag} OK done\r\n")).await;
            let tag = peer.expect("LOGIN").await;
            peer.send(&format!("{tag} OK logged in\r\n")).await;
            let tag = peer.expect("CAPABILITY").await;
            peer.send("* CAPABILITY IMAP4rev1\r\n").await;
            peer.send(&format!("{tag} OK done\r\n")).await;
            let tag = peer.expect("LOGOUT").await;
            peer.send("* BYE\r\n").await;
            peer.send(&format!("{tag} OK bye\r\n")).await;
            peer.drain().await
        });
        let endpoint = Endpoint {
            certificate,
            ..endpoint(port, Security::StartTls)
        };
        let result = connect(
            &endpoint,
            PASSWORD,
            Trust::Platform.client_config().unwrap(),
        )
        .await;
        match (result, outcome) {
            (Ok(conn), Ok(())) => conn.logout().await,
            (Err(error), Err(failure)) => assert_eq!(error.failure(), failure),
            (result, outcome) => panic!("{:?} instead of {outcome:?}", result.map(drop)),
        }
        let received = server.await.unwrap();
        if outcome.is_err() {
            assert_no_credentials(&received);
        }
    }
}

/// STARTTLS response injection (ML-093): an answer a man in the middle puts right after the
/// STARTTLS OK is dropped with the plaintext connection's buffer, never read as if it came over
/// TLS. Read over TLS, this one would leave only XOAUTH2.
#[tokio::test]
async fn bytes_after_the_starttls_ok_are_never_read_over_tls() {
    let ca = test_ca();
    let config = ca.server.clone();
    let (listener, port) = bind().await;
    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.send("* OK [CAPABILITY IMAP4rev1 STARTTLS AUTH=PLAIN] ready\r\n")
            .await;
        let tag = peer.expect("STARTTLS").await;
        peer.send(&format!(
            "{tag} OK begin TLS\r\n* CAPABILITY IMAP4rev1 LOGINDISABLED AUTH=XOAUTH2\r\n"
        ))
        .await;
        let mut peer = peer.tls(config).await;
        let tag = peer.expect("CAPABILITY").await;
        peer.send("* CAPABILITY IMAP4rev1 AUTH=PLAIN\r\n").await;
        peer.send(&format!("{tag} OK done\r\n")).await;
        let tag = peer.expect("AUTHENTICATE PLAIN").await;
        peer.send("+ \r\n").await;
        peer.line().await;
        peer.send(&format!("{tag} OK authenticated\r\n")).await;
        let tag = peer.expect("CAPABILITY").await;
        peer.send("* CAPABILITY IMAP4rev1\r\n").await;
        peer.send(&format!("{tag} OK done\r\n")).await;
        peer.drain().await
    });
    let conn = connect(
        &endpoint(port, Security::StartTls),
        PASSWORD,
        ca.trust.client_config().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(conn.mechanism(), Mechanism::Plain);
    drop(conn);
    server.await.unwrap();
}

#[tokio::test]
async fn implicit_tls_authenticates_with_plain_and_enables_qresync() {
    let ca = test_ca();
    let config = ca.server.clone();
    let (listener, port) = bind().await;
    let server = tokio::spawn(async move {
        let mut peer = Peer::accept_tls(&listener, config).await;
        peer.send("* OK [CAPABILITY IMAP4rev1 AUTH=PLAIN AUTH=LOGIN] ready\r\n")
            .await;
        let tag = peer.expect("AUTHENTICATE PLAIN").await;
        peer.send("+ \r\n").await;
        let response = STANDARD.decode(peer.line().await.unwrap()).unwrap();
        peer.send(&format!("{tag} OK authenticated\r\n")).await;
        let tag = peer.expect("CAPABILITY").await;
        peer.send("* CAPABILITY IMAP4rev1 CONDSTORE QRESYNC ENABLE\r\n")
            .await;
        peer.send(&format!("{tag} OK done\r\n")).await;
        let tag = peer.expect("ENABLE QRESYNC").await;
        peer.send("* ENABLED QRESYNC\r\n").await;
        peer.send(&format!("{tag} OK enabled\r\n")).await;
        (response, peer.drain().await)
    });
    let conn = connect(
        &endpoint(port, Security::Tls),
        PASSWORD,
        ca.trust.client_config().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(conn.mechanism(), Mechanism::Plain);
    assert!(conn.qresync() && conn.condstore());
    drop(conn);
    let (response, received) = server.await.unwrap();
    assert_eq!(response, b"\0user\0secret-password");
    assert!(!received.contains("ID ("), "ID only where offered");
}

async fn refused_with(reply: &str) -> ImapError {
    let ca = test_ca();
    let config = ca.server.clone();
    let (listener, port) = bind().await;
    let reply = reply.to_owned();
    tokio::spawn(async move {
        let mut peer = Peer::accept_tls(&listener, config).await;
        peer.send("* OK [CAPABILITY IMAP4rev1 AUTH=PLAIN] ready\r\n")
            .await;
        let tag = peer.expect("AUTHENTICATE PLAIN").await;
        peer.send("+ \r\n").await;
        peer.line().await;
        peer.send(&format!("{tag} {reply}\r\n")).await;
        peer.drain().await
    });
    connect(
        &endpoint(port, Security::Tls),
        PASSWORD,
        ca.trust.client_config().unwrap(),
    )
    .await
    .unwrap_err()
}

#[tokio::test]
async fn refused_logins_are_classified_from_the_reply() {
    for (reply, failure) in [
        (
            "NO [AUTHENTICATIONFAILED] Authentication failed.",
            LoginFailure::WrongPassword,
        ),
        (
            "NO [ALERT] Application-specific password required: \
             https://support.google.com/accounts/answer/185833 (Failure)",
            LoginFailure::AppPasswordRequired,
        ),
        (
            "NO [ALERT] Your account is not enabled for IMAP use. (Failure)",
            LoginFailure::ImapDisabled,
        ),
        (
            "BAD User is authenticated but not connected.",
            LoginFailure::ImapDisabled,
        ),
    ] {
        let error = refused_with(reply).await;
        assert!(matches!(error, ImapError::Rejected(_)), "{reply}: {error}");
        assert_eq!(error.failure(), failure, "{reply}");
        assert!(!format!("{error:?}").contains("support.google.com"));
    }
}

#[tokio::test]
async fn logindisabled_with_only_xoauth2_means_oauth_is_required() {
    let ca = test_ca();
    let config = ca.server.clone();
    let (listener, port) = bind().await;
    let server = tokio::spawn(async move {
        let mut peer = Peer::accept_tls(&listener, config).await;
        peer.send("* OK [CAPABILITY IMAP4rev1 LOGINDISABLED AUTH=XOAUTH2] ready\r\n")
            .await;
        peer.drain().await
    });
    let error = connect(
        &endpoint(port, Security::Tls),
        PASSWORD,
        ca.trust.client_config().unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.failure(), LoginFailure::OAuthRequired);
    assert_no_credentials(&server.await.unwrap());
}

#[test]
fn sasl_responses() {
    assert_eq!(&*plain_response("user", "pass"), b"\0user\0pass");
    // Google's documented example (developers.google.com/gmail/imap/xoauth2-protocol).
    let response = xoauth2_response(
        "someuser@example.com",
        "ya29.vF9dft4qmTc2Nvb3RlckBhdHRhdmlzdGEuY29tCg",
    );
    assert_eq!(
        STANDARD.encode(&*response),
        "dXNlcj1zb21ldXNlckBleGFtcGxlLmNvbQFhdXRoPUJlYXJlciB5YTI5LnZGOWRmdDRxbVRjMk52\
         YjNSbGNrQmhkSFJoZG1semRHRXVZMjl0Q2cBAQ=="
    );
    let mut one_shot = OneShot(Some(response));
    use async_imap::Authenticator;
    assert!(!one_shot.process(b"").is_empty());
    assert!(one_shot.process(b"{\"status\":\"401\"}").is_empty());
}

#[tokio::test]
async fn xoauth2_sends_the_token_and_answers_the_error_challenge() {
    let ca = test_ca();
    let config = ca.server.clone();
    let (listener, port) = bind().await;
    let server = tokio::spawn(async move {
        let mut peer = Peer::accept_tls(&listener, config).await;
        peer.send("* OK [CAPABILITY IMAP4rev1 AUTH=XOAUTH2 AUTH=PLAIN] ready\r\n")
            .await;
        let tag = peer.expect("AUTHENTICATE XOAUTH2").await;
        peer.send("+ \r\n").await;
        let response = STANDARD.decode(peer.line().await.unwrap()).unwrap();
        peer.send(&format!(
            "+ {}\r\n",
            STANDARD.encode("{\"status\":\"401\",\"schemes\":\"bearer\"}")
        ))
        .await;
        let answer = peer.line().await.unwrap();
        peer.send(&format!(
            "{tag} NO [AUTHENTICATIONFAILED] Invalid credentials\r\n"
        ))
        .await;
        (response, answer, peer.drain().await)
    });
    let error = connect(
        &endpoint(port, Security::Tls),
        Credentials::OAuth2 {
            username: "user",
            token: "token-123",
        },
        ca.trust.client_config().unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.failure(), LoginFailure::WrongPassword);
    let (response, answer, _) = server.await.unwrap();
    assert_eq!(response, b"user=user\x01auth=Bearer token-123\x01\x01");
    assert_eq!(answer, "", "the error challenge gets an empty response");
}

#[tokio::test]
async fn certificates_are_verified_and_plaintext_is_no_tls() {
    let ca = test_ca();
    let config = ca.server.clone();
    let (listener, port) = bind().await;
    tokio::spawn(async move {
        let _ = tokio_rustls::TlsAcceptor::from(config)
            .accept(listener.accept().await.unwrap().0)
            .await;
    });
    let error = connect(
        &endpoint(port, Security::Tls),
        PASSWORD,
        Trust::Platform.client_config().unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.failure(), LoginFailure::Certificate, "{error}");

    let (listener, port) = bind().await;
    tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.send("* OK [CAPABILITY IMAP4rev1 AUTH=PLAIN] plaintext on the TLS port\r\n")
            .await;
        peer.drain().await
    });
    let error = connect(
        &endpoint(port, Security::Tls),
        PASSWORD,
        ca.trust.client_config().unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.failure(), LoginFailure::NoTls, "{error}");
}
