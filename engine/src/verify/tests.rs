//! The login check's attempts against scripted servers.

use super::*;
use crate::net::testing::{Peer, bind, test_ca};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rustls::ServerConfig;
use sealwax_domain::account::{AccountKey, Endpoint, OAuthProvider, Security};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

type Seen = Arc<Mutex<Vec<String>>>;

/// The user name inside a base64 SASL PLAIN response.
fn plain_user(line: &str) -> String {
    let decoded = STANDARD.decode(line).unwrap();
    String::from_utf8_lossy(decoded.split(|&b| b == 0).nth(1).unwrap()).into_owned()
}

/// An IMAP server that accepts only `accept` and answers others with `refusal`.
fn imap_server(
    listener: TcpListener,
    config: Arc<ServerConfig>,
    accept: &'static str,
    refusal: &'static str,
) -> Seen {
    let seen = Seen::default();
    let tried = seen.clone();
    tokio::spawn(async move {
        loop {
            let mut peer = Peer::accept_tls(&listener, config.clone()).await;
            peer.send("* OK [CAPABILITY IMAP4rev1 AUTH=PLAIN] ready\r\n")
                .await;
            let tag = peer.expect("AUTHENTICATE PLAIN").await;
            peer.send("+ \r\n").await;
            let user = plain_user(&peer.line().await.unwrap());
            tried.lock().unwrap().push(user.clone());
            if user != accept {
                peer.send(&format!("{tag} NO {refusal}\r\n")).await;
                continue;
            }
            peer.send(&format!("{tag} OK in\r\n")).await;
            let tag = peer.expect("CAPABILITY").await;
            peer.send(&format!("* CAPABILITY IMAP4rev1\r\n{tag} OK done\r\n"))
                .await;
            let tag = peer.expect("LOGOUT").await;
            peer.send(&format!("* BYE\r\n{tag} OK bye\r\n")).await;
        }
    });
    seen
}

/// A POP3 server that accepts only `accept` and answers others with `-ERR refusal`. It has
/// UIDL, so the check sends nothing after the login but CAPA and QUIT.
fn pop3_server(
    listener: TcpListener,
    config: Arc<ServerConfig>,
    accept: &'static str,
    refusal: &'static str,
) -> Seen {
    let seen = Seen::default();
    let tried = seen.clone();
    tokio::spawn(async move {
        loop {
            let mut peer = Peer::accept_tls(&listener, config.clone()).await;
            peer.send("+OK ready\r\n").await;
            assert_eq!(peer.line().await.unwrap(), "CAPA");
            peer.send("+OK\r\nSASL PLAIN\r\nUIDL\r\n.\r\n").await;
            let auth = peer.line().await.unwrap();
            let user = plain_user(auth.strip_prefix("AUTH PLAIN ").unwrap());
            tried.lock().unwrap().push(user.clone());
            if user != accept {
                peer.send(&format!("-ERR {refusal}\r\n")).await;
                continue;
            }
            peer.send("+OK in\r\n").await;
            assert_eq!(peer.line().await.unwrap(), "CAPA");
            peer.send("+OK\r\nUIDL\r\n.\r\n").await;
            assert_eq!(peer.line().await.unwrap(), "QUIT");
            peer.send("+OK bye\r\n").await;
        }
    });
    seen
}

/// An implicit-TLS submission server that accepts every login and records the user.
fn smtp_server(listener: TcpListener, config: Arc<ServerConfig>) -> Seen {
    let seen = Seen::default();
    let tried = seen.clone();
    tokio::spawn(async move {
        loop {
            let mut peer = Peer::accept_tls(&listener, config.clone()).await;
            peer.send("220 test\r\n").await;
            peer.line().await;
            peer.send("250-test\r\n250 AUTH PLAIN\r\n").await;
            let auth = peer.line().await.unwrap();
            tried
                .lock()
                .unwrap()
                .push(plain_user(&auth["AUTH PLAIN ".len()..]));
            peer.send("235 ok\r\n").await;
            peer.line().await;
            peer.send("221 bye\r\n").await;
        }
    });
    seen
}

fn settings(imap: u16, smtp: u16) -> AccountSettings {
    let endpoint = |port| Endpoint {
        host: "127.0.0.1".into(),
        port,
        security: Security::Tls,
        certificate: None,
    };
    AccountSettings {
        key: AccountKey("test".into()),
        name: "Test".into(),
        label: None,
        color: None,
        address: "first@example.test".into(),
        incoming: endpoint(imap),
        receive: sealwax_domain::account::Receive::Imap,
        smtp: endpoint(smtp),
        username: "first@example.test".into(),
        auth: AuthMethod::Password,
        signature: None,
        identities: Vec::new(),
    }
}

async fn run(
    refusal: &'static str,
    second: Option<&str>,
) -> (Result<String, LoginError>, Vec<String>, Vec<String>) {
    let ca = test_ca();
    let (imap_listener, imap_port) = bind().await;
    let (smtp_listener, smtp_port) = bind().await;
    let imap = imap_server(imap_listener, ca.server.clone(), "first", refusal);
    let smtp = smtp_server(smtp_listener, ca.server.clone());
    let password = Password::new("secret".into());
    let result = verify(
        &settings(imap_port, smtp_port),
        second,
        &password,
        &ca.trust,
    )
    .await;
    let imap = imap.lock().unwrap().clone();
    let smtp = smtp.lock().unwrap().clone();
    (result, imap, smtp)
}

#[tokio::test]
async fn the_second_username_is_tried_once_and_smtp_uses_the_one_that_worked() {
    let (result, imap, smtp) = run("[AUTHENTICATIONFAILED] no", Some("first")).await;
    assert_eq!(result, Ok("first".to_owned()));
    assert_eq!(imap, ["first@example.test", "first"]);
    assert_eq!(smtp, ["first"]);

    let (result, imap, smtp) = run("[AUTHENTICATIONFAILED] no", Some("other")).await;
    assert_eq!(
        result,
        Err(LoginError {
            protocol: Protocol::Imap,
            failure: LoginFailure::WrongPassword,
        })
    );
    assert_eq!(imap.len(), 2, "never more than two attempts");
    assert!(smtp.is_empty(), "no SMTP after a failed IMAP login");
}

#[tokio::test]
async fn other_refusals_and_a_repeated_username_get_no_second_attempt() {
    let (result, imap, _) = run(
        "[ALERT] Application-specific password required",
        Some("first"),
    )
    .await;
    assert_eq!(
        result.unwrap_err().failure,
        LoginFailure::AppPasswordRequired
    );
    assert_eq!(imap.len(), 1);
    let (result, imap, _) = run("[AUTHENTICATIONFAILED] no", Some("first@example.test")).await;
    assert_eq!(result.unwrap_err().failure, LoginFailure::WrongPassword);
    assert_eq!(imap.len(), 1);
}

#[tokio::test]
async fn oauth_accounts_send_no_password() {
    let mut account = settings(1, 1);
    account.auth = AuthMethod::OAuth(OAuthProvider::Microsoft);
    let password = Password::new("secret".into());
    let result = verify(&account, None, &password, &Trust::Platform).await;
    assert_eq!(
        result,
        Err(LoginError {
            protocol: Protocol::Imap,
            failure: LoginFailure::OAuthRequired
        })
    );
}

async fn run_pop3(
    refusal: &'static str,
    second: Option<&str>,
) -> (Result<String, LoginError>, Vec<String>, Vec<String>) {
    let ca = test_ca();
    let (pop3_listener, pop3_port) = bind().await;
    let (smtp_listener, smtp_port) = bind().await;
    let pop3 = pop3_server(pop3_listener, ca.server.clone(), "first", refusal);
    let smtp = smtp_server(smtp_listener, ca.server.clone());
    let mut account = settings(pop3_port, smtp_port);
    account.receive = Receive::Pop3(Default::default());
    let password = Password::new("secret".into());
    let result = verify(&account, second, &password, &ca.trust).await;
    let pop3 = pop3.lock().unwrap().clone();
    let smtp = smtp.lock().unwrap().clone();
    (result, pop3, smtp)
}

#[tokio::test]
async fn a_pop3_account_logs_in_to_pop3_with_the_same_two_username_rule() {
    let (result, pop3, smtp) = run_pop3("[AUTH] no", Some("first")).await;
    assert_eq!(result, Ok("first".to_owned()));
    assert_eq!(pop3, ["first@example.test", "first"]);
    assert_eq!(smtp, ["first"]);

    let (result, pop3, smtp) = run_pop3("[AUTH] no", Some("other")).await;
    assert_eq!(
        result,
        Err(LoginError {
            protocol: Protocol::Pop3,
            failure: LoginFailure::WrongPassword,
        })
    );
    assert_eq!(pop3.len(), 2, "never more than two attempts");
    assert!(smtp.is_empty(), "no SMTP after a failed POP3 login");

    let (result, pop3, _) = run_pop3("[IN-USE] another session", Some("first")).await;
    assert_eq!(
        result,
        Err(LoginError {
            protocol: Protocol::Pop3,
            failure: LoginFailure::Busy,
        })
    );
    assert_eq!(pop3.len(), 1, "a busy maildrop isn't a wrong username");
}

#[tokio::test]
async fn a_pop3_oauth_account_is_named_as_pop3() {
    let mut account = settings(1, 1);
    account.receive = Receive::Pop3(Default::default());
    account.auth = AuthMethod::OAuth(OAuthProvider::Microsoft);
    let password = Password::new("secret".into());
    let result = verify(&account, None, &password, &Trust::Platform).await;
    assert_eq!(
        result,
        Err(LoginError {
            protocol: Protocol::Pop3,
            failure: LoginFailure::OAuthRequired
        })
    );
}
