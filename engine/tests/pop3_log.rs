//! ADR-018 for the POP3 client (ML-087): a whole session against the scripted server and a
//! refused login at trace level leave counts, sizes, timings and the mechanism in the log, and
//! no username, password, UIDL, message text, server text or protocol line. Its own test
//! binary: a subscriber set for one thread misses events whose callsites other tests' threads
//! registered at the same moment.

mod pop3_script;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use pop3_script::*;
use sealwax_domain::account::{LoginFailure, Security};
use sealwax_engine::pop3::{self, Mailbox};
use std::io::Write;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn nothing_but_counts_reaches_the_log() {
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let message: &[u8] = b"Subject: Hemmelig emne\r\n\r\nSynthetic body text\r\n";
    let ca = ca();
    let (port, server) = serve(Some(ca.server.clone()), move |mut peer| async move {
        peer.logged_in().await;
        peer.expect("UIDL").await;
        peer.send("+OK\r\n1 uidl-secret-1\r\n2 uidl-secret-2\r\n.\r\n")
            .await;
        peer.expect("LIST").await;
        peer.send("+OK\r\n1 52\r\n2 52\r\n.\r\n").await;
        peer.expect("RETR 1").await;
        peer.send_bytes(&wire_form(message)).await;
        peer.expect("DELE 1").await;
        peer.send("+OK\r\n").await;
        peer.expect("QUIT").await;
        peer.send("+OK bye kari.nordmann\r\n").await;
        peer
    })
    .await;
    let mut session = connect(&ca, port, PASSWORD).await.unwrap();
    session.listing().await.unwrap();
    session.retrieve(1).await.unwrap();
    session.delete(1).await.unwrap();
    session.quit().await.unwrap();
    server.await.unwrap();

    let ca = self::ca();
    let (port, server) = serve(Some(ca.server.clone()), |mut peer| async move {
        peer.hello(CAPS).await;
        peer.expect("AUTH PLAIN ").await;
        peer.send("-ERR [AUTH] No such user kari.nordmann\r\n")
            .await;
        peer
    })
    .await;
    let failure = pop3::check_login(
        &endpoint(port, Security::Tls),
        PASSWORD,
        ca.trust.client_config().unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(failure, LoginFailure::WrongPassword);
    server.await.unwrap();

    let log = String::from_utf8_lossy(&capture.0.lock().unwrap()).into_owned();
    for event in [
        "pop3 connected",
        "pop3 listed",
        "pop3 retrieved",
        "pop3 quit",
        "pop3 login check failed",
    ] {
        assert!(log.contains(event), "{event} missing:\n{log}");
    }
    let plain = STANDARD.encode(format!("\0{USERNAME}\0{SECRET}"));
    for forbidden in [
        "kari",
        "nordmann",
        SECRET,
        plain.as_str(),
        "uidl-secret",
        "Hemmelig",
        "Synthetic",
        "Subject",
        "USER",
        "PASS",
        "AUTH",
        "RETR",
        "DELE",
        "+OK",
        "-ERR",
        "No such user",
    ] {
        assert!(!log.contains(forbidden), "{forbidden} in the log:\n{log}");
    }
}
