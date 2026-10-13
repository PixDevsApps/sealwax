//! The SMTP login check against scripted servers.

use super::*;
use crate::net::testing::{Peer, bind, self_signed, test_ca};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use sealwax_domain::account::CertificatePin;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

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

#[test]
fn replies_are_classified() {
    for (code, text, failure) in [
        (
            535,
            "5.7.8 username and password not accepted.",
            LoginFailure::WrongPassword,
        ),
        (
            534,
            "5.7.9 application-specific password required. learn more at",
            LoginFailure::AppPasswordRequired,
        ),
        (
            534,
            "5.7.9 authentication mechanism is too weak",
            LoginFailure::AppPasswordRequired,
        ),
        (
            534,
            "5.7.14 please log in via your web browser and then try again.",
            LoginFailure::Other,
        ),
        (
            535,
            "5.7.139 authentication unsuccessful, basic authentication is disabled.",
            LoginFailure::OAuthRequired,
        ),
        (
            535,
            "5.7.139 authentication unsuccessful, smtpclientauthentication is disabled for the tenant.",
            LoginFailure::Other,
        ),
        (
            530,
            "5.7.0 must issue a starttls command first",
            LoginFailure::NoTls,
        ),
        (538, "5.7.11 encryption required", LoginFailure::NoTls),
        (
            454,
            "4.7.0 temporary authentication failure",
            LoginFailure::Other,
        ),
    ] {
        assert_eq!(reply(code, text), failure, "{code} {text}");
    }
}

/// An implicit-TLS submission server that offers `auth` and answers AUTH with `answer`.
async fn implicit(auth: &'static str, answer: &'static str) -> (Result<(), LoginFailure>, String) {
    let ca = test_ca();
    let config = ca.server.clone();
    let (listener, port) = bind().await;
    let server = tokio::spawn(async move {
        let mut peer = Peer::accept_tls(&listener, config).await;
        peer.send("220 test ESMTP\r\n").await;
        let ehlo = peer.line().await.unwrap();
        peer.send(&format!("250-test\r\n250-{auth}\r\n250 8BITMIME\r\n"))
            .await;
        if let Some(line) = peer.line().await
            && line.starts_with("AUTH")
        {
            peer.send(answer).await;
            if let Some(quit) = peer.line().await
                && quit == "QUIT"
            {
                peer.send("221 bye\r\n").await;
            }
        }
        format!("{ehlo}\n{}", peer.drain().await)
    });
    let result = check_login(&endpoint(port, Security::Tls), PASSWORD, &ca.trust).await;
    (result, server.await.unwrap())
}

#[tokio::test]
async fn implicit_tls_logs_in_with_plain_and_a_neutral_hello() {
    let (result, received) = implicit("AUTH PLAIN LOGIN", "235 2.7.0 accepted\r\n").await;
    assert_eq!(result, Ok(()));
    assert!(received.starts_with("EHLO [127.0.0.1]\n"), "{received}");
    let auth = received
        .lines()
        .find(|line| line.starts_with("AUTH PLAIN "))
        .unwrap();
    let response = STANDARD.decode(&auth["AUTH PLAIN ".len()..]).unwrap();
    assert_eq!(response, b"\0user\0secret-password");
}

#[tokio::test]
async fn refusals_and_oauth_only_servers_are_classified() {
    let (result, _) = implicit(
        "AUTH PLAIN LOGIN",
        "534-5.7.9 Application-specific password required. Learn more at\r\n\
         534 5.7.9  https://support.google.com/mail/?p=InvalidSecondFactor\r\n",
    )
    .await;
    assert_eq!(result, Err(LoginFailure::AppPasswordRequired));
    let (result, _) = implicit(
        "AUTH PLAIN LOGIN",
        "535 5.7.8 Authentication credentials invalid\r\n",
    )
    .await;
    assert_eq!(result, Err(LoginFailure::WrongPassword));
    let (result, received) = implicit("AUTH XOAUTH2 OAUTHBEARER", "").await;
    assert_eq!(result, Err(LoginFailure::OAuthRequired));
    assert!(!received.contains("AUTH ") && !received.contains("secret"));
}

#[tokio::test]
async fn starttls_is_required_and_then_used() {
    let ca = test_ca();
    let (listener, port) = bind().await;
    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.send("220 test ESMTP\r\n").await;
        peer.line().await;
        peer.send("250-test\r\n250 AUTH PLAIN LOGIN\r\n").await;
        peer.drain().await
    });
    let result = check_login(&endpoint(port, Security::StartTls), PASSWORD, &ca.trust).await;
    assert_eq!(result, Err(LoginFailure::NoTls));
    let received = server.await.unwrap();
    assert!(
        !received.contains("AUTH") && !received.contains("secret"),
        "{received}"
    );

    let config = ca.server.clone();
    let (listener, port) = bind().await;
    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.send("220 test ESMTP\r\n").await;
        peer.line().await;
        peer.send("250-test\r\n250-STARTTLS\r\n250 AUTH PLAIN LOGIN\r\n")
            .await;
        assert_eq!(peer.line().await.unwrap(), "STARTTLS");
        peer.send("220 2.0.0 go ahead\r\n").await;
        let mut peer = peer.tls(config).await;
        peer.line().await;
        peer.send("250-test\r\n250 AUTH LOGIN\r\n").await;
        assert_eq!(peer.line().await.unwrap(), "AUTH LOGIN");
        peer.send("334 VXNlcm5hbWU6\r\n").await;
        let username = STANDARD.decode(peer.line().await.unwrap()).unwrap();
        peer.send("334 UGFzc3dvcmQ6\r\n").await;
        let password = STANDARD.decode(peer.line().await.unwrap()).unwrap();
        peer.send("235 2.7.0 accepted\r\n").await;
        peer.line().await;
        peer.send("221 bye\r\n").await;
        (username, password)
    });
    let result = check_login(&endpoint(port, Security::StartTls), PASSWORD, &ca.trust).await;
    assert_eq!(result, Ok(()));
    assert_eq!(
        server.await.unwrap(),
        (b"user".to_vec(), b"secret-password".to_vec())
    );
}

/// A server on this computer with a pinned certificate (issue #9): STARTTLS accepts exactly that
/// certificate, and a different one ends the check before AUTH.
#[tokio::test]
async fn a_pinned_certificate_is_checked_after_starttls_before_auth() {
    let bridge = self_signed();
    let pin = crate::net::fingerprint(&bridge.der);
    for (certificate, works) in [(pin, true), (CertificatePin([0x33; 32]), false)] {
        let config = bridge.server.clone();
        let (listener, port) = bind().await;
        let server = tokio::spawn(async move {
            let mut peer = Peer::accept(&listener).await;
            peer.send("220 bridge ESMTP\r\n").await;
            peer.line().await;
            peer.send("250-bridge\r\n250-STARTTLS\r\n250 AUTH PLAIN LOGIN\r\n")
                .await;
            assert_eq!(peer.line().await.unwrap(), "STARTTLS");
            peer.send("220 2.0.0 go ahead\r\n").await;
            let mut peer = peer.tls(config).await;
            peer.line().await;
            peer.send("250-bridge\r\n250 AUTH PLAIN\r\n").await;
            let Some(auth) = peer.line().await.filter(|line| line.starts_with("AUTH")) else {
                return peer.drain().await;
            };
            assert!(auth.starts_with("AUTH PLAIN"), "{auth}");
            peer.send("235 2.7.0 accepted\r\n").await;
            peer.line().await;
            peer.send("221 bye\r\n").await;
            peer.drain().await
        });
        let endpoint = Endpoint {
            certificate: Some(certificate),
            ..endpoint(port, Security::StartTls)
        };
        let result = check_login(&endpoint, PASSWORD, &Trust::Platform).await;
        let received = server.await.unwrap();
        if works {
            assert_eq!(result, Ok(()));
            assert!(received.contains("AUTH PLAIN"), "{received}");
        } else {
            assert_eq!(result, Err(LoginFailure::Certificate));
            assert!(!received.contains("AUTH"), "{received}");
        }
    }
}

/// STARTTLS response injection (ML-093): a man in the middle sends a fake answer right after
/// STARTTLS's 220. Read after the handshake, it would pass for the server's answer over TLS
/// and put every later reply one behind. Anything sent ahead of the handshake ends the check
/// before TLS starts, so nothing goes over TLS.
#[tokio::test]
async fn bytes_after_starttls_s_reply_end_the_check() {
    let ca = test_ca();
    let config = ca.server.clone();
    let (listener, port) = bind().await;
    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.send("220 test ESMTP\r\n").await;
        peer.line().await;
        peer.send("250-test\r\n250-STARTTLS\r\n250 AUTH PLAIN\r\n")
            .await;
        assert_eq!(peer.line().await.unwrap(), "STARTTLS");
        peer.send("220 2.0.0 go ahead\r\n250-injected\r\n250 AUTH PLAIN\r\n")
            .await;
        let Ok(tls) = tokio_rustls::TlsAcceptor::from(config)
            .accept(peer.into_inner())
            .await
        else {
            return Vec::new();
        };
        let mut tls = tokio::io::BufReader::new(tls);
        let mut lines = Vec::new();
        let mut line = String::new();
        while matches!(tls.read_line(&mut line).await, Ok(1..)) {
            let answer = match line.get(..4).unwrap_or_default() {
                "EHLO" => "250-test\r\n250 AUTH PLAIN\r\n",
                "AUTH" => "235 2.7.0 accepted\r\n",
                _ => "221 bye\r\n",
            };
            lines.push(std::mem::take(&mut line));
            tls.get_mut().write_all(answer.as_bytes()).await.unwrap();
        }
        lines
    });
    let result = check_login(&endpoint(port, Security::StartTls), PASSWORD, &ca.trust).await;
    let over_tls = server.await.unwrap();
    assert_eq!(result, Err(LoginFailure::NoTls));
    assert!(over_tls.is_empty(), "{over_tls:?}");
}

#[tokio::test]
async fn an_unknown_certificate_fails_the_check() {
    let ca = test_ca();
    let config = ca.server.clone();
    let (listener, port) = bind().await;
    tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.send("220 test ESMTP\r\n").await;
        peer.line().await;
        peer.send("250-test\r\n250-STARTTLS\r\n250 AUTH PLAIN\r\n")
            .await;
        peer.line().await;
        peer.send("220 go ahead\r\n").await;
        let _ = tokio_rustls::TlsAcceptor::from(config)
            .accept(peer.into_inner())
            .await;
    });
    let result = check_login(
        &endpoint(port, Security::StartTls),
        PASSWORD,
        &Trust::Platform,
    )
    .await;
    assert_eq!(result, Err(LoginFailure::Certificate));
}
