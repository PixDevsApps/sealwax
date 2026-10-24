//! Submission against scripted servers: every reply the client can meet before and after its
//! mark, and what it sends.

use super::*;
use crate::net::testing::{Peer, bind, test_ca};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use sealwax_domain::account::Security;
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;

const PASSWORD: Credentials<'static> = Credentials::Password {
    username: "user",
    password: "secret-password",
};

const MESSAGE: &[u8] = b"From: <a@example.test>\r\nTo: <b@example.test>\r\nSubject: Test\r\n\
Message-ID: <id.1@example.test>\r\n\r\nHello\r\n.leading dot\r\n.\r\nend\r\n";

fn quick() -> Timeouts {
    Timeouts {
        connect: Duration::from_secs(5),
        command: Duration::from_secs(5),
        data: Duration::from_millis(500),
    }
}

/// What the scripted server answers.
#[derive(Clone)]
struct Script {
    /// EHLO's extension lines, after the server's name.
    ehlo: Vec<&'static str>,
    /// Replies to AUTH and to each answer to a challenge, in order.
    auth: Vec<&'static str>,
    mail: &'static str,
    /// One reply per RCPT; the last repeats.
    rcpt: Vec<&'static str>,
    data: &'static str,
    /// The reply to the end of data; `None` closes the connection without one.
    end: Option<&'static str>,
    /// The end of data is never answered.
    hang: bool,
    /// The end of data is answered with a line that never ends.
    endless: bool,
    /// The connection is closed as DATA arrives, unanswered.
    cut_at_data: bool,
    /// The connection is closed as RSET arrives, unanswered.
    cut_at_rset: bool,
}

impl Default for Script {
    fn default() -> Self {
        Script {
            ehlo: vec!["AUTH PLAIN LOGIN XOAUTH2", "SIZE 1000000", "8BITMIME"],
            auth: vec!["235 2.7.0 accepted"],
            mail: "250 2.1.0 ok",
            rcpt: vec!["250 2.1.5 ok"],
            data: "354 go ahead",
            end: Some("250 2.0.0 queued as X1"),
            hang: false,
            endless: false,
            cut_at_data: false,
            cut_at_rset: false,
        }
    }
}

/// What the client sent: its command lines (the AUTH lines as sent) and each message's data,
/// with the dot-stuffing undone.
#[derive(Clone, Debug, Default)]
struct Seen {
    lines: Vec<String>,
    messages: Vec<Vec<u8>>,
}

impl Seen {
    fn verbs(&self) -> Vec<String> {
        self.lines
            .iter()
            .map(|line| line.split([' ', ':']).next().unwrap_or("").to_owned())
            .collect()
    }
}

async fn serve(mut peer: Peer, script: Script, seen: Arc<Mutex<Seen>>) {
    peer.send("220 test ESMTP\r\n").await;
    let mut auth = script.auth.iter();
    let mut rcpts = 0;
    while let Some(line) = peer.line().await {
        seen.lock().unwrap().lines.push(line.clone());
        let verb = line
            .split([' ', ':'])
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        let reply = match verb.as_str() {
            "EHLO" => {
                let mut reply = String::from("250-test\r\n");
                for (i, line) in script.ehlo.iter().enumerate() {
                    let dash = if i + 1 == script.ehlo.len() { ' ' } else { '-' };
                    reply.push_str(&format!("250{dash}{line}\r\n"));
                }
                if script.ehlo.is_empty() {
                    reply = "250 test\r\n".into();
                }
                reply
            }
            "AUTH" | "" if !line.starts_with("RSET") => {
                format!("{}\r\n", auth.next().copied().unwrap_or("535 5.7.8 no"))
            }
            "MAIL" => format!("{}\r\n", script.mail),
            "RCPT" => {
                let reply = script
                    .rcpt
                    .get(rcpts)
                    .or(script.rcpt.last())
                    .copied()
                    .unwrap();
                rcpts += 1;
                format!("{reply}\r\n")
            }
            "DATA" => {
                if script.cut_at_data {
                    return;
                }
                peer.send(&format!("{}\r\n", script.data)).await;
                if !script.data.starts_with("354") {
                    continue;
                }
                let mut data = Vec::new();
                loop {
                    let Some(line) = peer.line().await else {
                        return;
                    };
                    if line == "." {
                        break;
                    }
                    let line = line.strip_prefix('.').unwrap_or(&line);
                    data.extend_from_slice(line.as_bytes());
                    data.extend_from_slice(b"\r\n");
                }
                seen.lock().unwrap().messages.push(data);
                if script.hang {
                    while peer.line().await.is_some() {}
                    return;
                }
                if script.endless {
                    peer.flood("250 2.0.0 ", b'x').await;
                    return;
                }
                match script.end {
                    Some(reply) => format!("{reply}\r\n"),
                    None => return,
                }
            }
            "RSET" if script.cut_at_rset => return,
            "RSET" => "250 2.0.0 reset\r\n".into(),
            "QUIT" => {
                peer.send("221 bye\r\n").await;
                return;
            }
            _ => "500 unknown\r\n".into(),
        };
        peer.send(&reply).await;
    }
}

struct Server {
    port: u16,
    seen: Arc<Mutex<Seen>>,
    ca: crate::net::testing::TestCa,
}

impl Server {
    /// An implicit-TLS server that runs `scripts`, one per connection.
    async fn start(scripts: Vec<Script>) -> Server {
        let ca = test_ca();
        let config = ca.server.clone();
        let (listener, port) = bind().await;
        let seen = Arc::new(Mutex::new(Seen::default()));
        let shared = seen.clone();
        tokio::spawn(async move {
            for script in scripts {
                let peer = Peer::accept_tls(&listener, config.clone()).await;
                serve(peer, script, shared.clone()).await;
            }
        });
        Server { port, seen, ca }
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint {
            host: "127.0.0.1".into(),
            port: self.port,
            security: Security::Tls,
            certificate: None,
        }
    }

    async fn connect(&self) -> Result<Submitter, SubmitError> {
        Submitter::connect_with(&self.endpoint(), PASSWORD, &self.ca.trust, quick()).await
    }

    fn seen(&self) -> Seen {
        self.seen.lock().unwrap().clone()
    }
}

fn to(addresses: &[&str]) -> Vec<String> {
    addresses.iter().map(|a| a.to_string()).collect()
}

async fn send(submitter: &mut Submitter, to: &[String], message: &[u8]) -> Result<(), SubmitError> {
    let envelope = Envelope {
        from: "a@example.test",
        to,
    };
    submitter.prepare(envelope, message).await?.send().await
}

#[tokio::test]
async fn a_message_goes_out_byte_for_byte_after_the_envelope() {
    let server = Server::start(vec![Script::default()]).await;
    let mut submitter = server.connect().await.unwrap();
    assert_eq!(submitter.extensions().size, Some(1_000_000));
    let recipients = to(&["b@example.test", "c@example.test"]);
    let envelope = Envelope {
        from: "a@example.test",
        to: &recipients,
    };
    let prepared = submitter.prepare(envelope, MESSAGE).await.unwrap();
    assert!(
        !server.seen().verbs().contains(&"DATA".to_owned()),
        "nothing of the message before the caller's mark"
    );
    prepared.send().await.unwrap();
    submitter.quit().await;
    let seen = server.seen();
    assert_eq!(
        seen.verbs(),
        [
            "EHLO", "EHLO", "AUTH", "MAIL", "RCPT", "RCPT", "DATA", "QUIT"
        ]
    );
    assert_eq!(seen.lines[0], "EHLO [127.0.0.1]");
    assert_eq!(
        seen.lines[3],
        format!("MAIL FROM:<a@example.test> SIZE={}", MESSAGE.len())
    );
    assert_eq!(seen.lines[4], "RCPT TO:<b@example.test>");
    assert_eq!(seen.messages, [MESSAGE.to_vec()], "exactly the bytes given");
}

#[tokio::test]
async fn a_message_without_a_final_line_break_still_ends_cleanly() {
    let server = Server::start(vec![Script::default()]).await;
    let mut submitter = server.connect().await.unwrap();
    let message = b"Subject: x\r\n\r\nno final break";
    send(&mut submitter, &to(&["b@example.test"]), message)
        .await
        .unwrap();
    submitter.quit().await;
    assert_eq!(
        server.seen().messages,
        [b"Subject: x\r\n\r\nno final break\r\n"]
    );
}

#[tokio::test]
async fn a_refused_recipient_refuses_the_whole_send() {
    let server = Server::start(vec![Script {
        rcpt: vec![
            "250 ok",
            "550 5.1.1 no such user",
            "250 ok",
            "553 5.1.3 bad",
        ],
        ..Script::default()
    }])
    .await;
    let mut submitter = server.connect().await.unwrap();
    let recipients = to(&["b@x.test", "c@x.test", "d@x.test", "e@x.test"]);
    let error = send(&mut submitter, &recipients, MESSAGE)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        SubmitError::RecipientsRefused {
            recipients: vec![1, 3]
        }
    );
    assert_eq!(error.failure(), SubmitFailure::Refuse);
    assert_eq!(
        error.send_error(&recipients),
        SendError::RecipientsRefused(to(&["c@x.test", "e@x.test"]))
    );
    assert_eq!(format!("{error:?} {error}").matches('@').count(), 0);
    submitter.quit().await;
    let verbs = server.seen().verbs();
    assert!(verbs.contains(&"RSET".to_owned()) && !verbs.contains(&"DATA".to_owned()));
}

#[tokio::test]
async fn a_refusal_keeps_its_class_when_the_reset_fails() {
    for (script, expected) in [
        (
            Script {
                rcpt: vec!["550 5.1.1 no such user"],
                cut_at_rset: true,
                ..Script::default()
            },
            SubmitError::RecipientsRefused {
                recipients: vec![0],
            },
        ),
        (
            Script {
                mail: "553 5.7.1 sender not allowed",
                cut_at_rset: true,
                ..Script::default()
            },
            SubmitError::Refused { code: 553 },
        ),
        (
            Script {
                data: "552 5.3.4 too big",
                cut_at_rset: true,
                ..Script::default()
            },
            SubmitError::TooLarge {
                limit: Some(1_000_000),
            },
        ),
    ] {
        let server = Server::start(vec![script]).await;
        let mut submitter = server.connect().await.unwrap();
        let error = send(&mut submitter, &to(&["b@x.test"]), MESSAGE)
            .await
            .unwrap_err();
        assert_eq!(error, expected);
        assert!(submitter.conn.is_none(), "the failed reset dropped it");
        submitter.quit().await;
    }
}

#[tokio::test]
async fn deferred_recipients_and_refusals_before_data_are_transient_or_refused() {
    for (script, expected) in [
        (
            Script {
                rcpt: vec!["250 ok", "451 4.3.0 try later"],
                ..Script::default()
            },
            SubmitError::Transient,
        ),
        (
            Script {
                rcpt: vec!["552 5.5.3 too many recipients"],
                ..Script::default()
            },
            SubmitError::Transient,
        ),
        (
            Script {
                mail: "553 5.7.1 sender not allowed",
                ..Script::default()
            },
            SubmitError::Refused { code: 553 },
        ),
        (
            Script {
                mail: "421 4.7.0 too busy",
                ..Script::default()
            },
            SubmitError::Transient,
        ),
        (
            Script {
                mail: "552 5.3.4 too big",
                ..Script::default()
            },
            SubmitError::TooLarge {
                limit: Some(1_000_000),
            },
        ),
        (
            Script {
                data: "554 5.5.1 no valid recipients",
                ..Script::default()
            },
            SubmitError::Refused { code: 554 },
        ),
        (
            Script {
                cut_at_data: true,
                ..Script::default()
            },
            SubmitError::Transient,
        ),
    ] {
        let server = Server::start(vec![script]).await;
        let mut submitter = server.connect().await.unwrap();
        let error = send(&mut submitter, &to(&["b@x.test", "c@x.test"]), MESSAGE)
            .await
            .unwrap_err();
        assert_eq!(error, expected);
        assert!(
            server.seen().messages.is_empty(),
            "no message data for {expected:?}"
        );
        submitter.quit().await;
    }
}

#[tokio::test]
async fn replies_to_the_end_of_data_are_definite_and_silence_is_uncertain() {
    // Microsoft 365's reply lines can be longer than lettre's 1,000 bytes (issue #2): the patched
    // lettre reads lines up to 16 KiB, so they are definite. A line longer than that is answered,
    // but neither a refusal nor an acceptance can be read.
    let long = |code: &str, length: usize| -> &'static str {
        Box::leak(format!("{code} {}", "x".repeat(length)).into_boxed_str())
    };
    for (script, expected) in [
        (
            Script {
                end: Some("451 4.3.0 local error"),
                ..Script::default()
            },
            SubmitError::Transient,
        ),
        (
            Script {
                end: Some("554 5.7.1 rejected as spam"),
                ..Script::default()
            },
            SubmitError::Refused { code: 554 },
        ),
        (
            Script {
                end: Some("552 5.3.4 too big"),
                ..Script::default()
            },
            SubmitError::TooLarge {
                limit: Some(1_000_000),
            },
        ),
        (
            Script {
                end: None,
                ..Script::default()
            },
            SubmitError::Uncertain(UncertainReason::NoReply),
        ),
        (
            Script {
                hang: true,
                ..Script::default()
            },
            SubmitError::Uncertain(UncertainReason::NoReply),
        ),
        (
            Script {
                end: Some(long("554 5.2.252 SendAsDenied;", 1_200)),
                ..Script::default()
            },
            SubmitError::Refused { code: 554 },
        ),
        (
            Script {
                end: Some(long("451 4.4.0 try again;", 16_000)),
                ..Script::default()
            },
            SubmitError::Transient,
        ),
        (
            Script {
                end: Some(long("554 5.2.252 SendAsDenied;", 17_000)),
                ..Script::default()
            },
            SubmitError::Uncertain(UncertainReason::UnreadableReply),
        ),
        (
            Script {
                end: Some(long("250 2.0.0 OK", 17_000)),
                ..Script::default()
            },
            SubmitError::Uncertain(UncertainReason::UnreadableReply),
        ),
        (
            Script {
                end: Some("not an SMTP reply"),
                ..Script::default()
            },
            SubmitError::Uncertain(UncertainReason::UnreadableReply),
        ),
        (
            Script {
                end: Some("554-5.7.1 rejected\r\n554 5.7.1 as spam"),
                ..Script::default()
            },
            SubmitError::Refused { code: 554 },
        ),
        (
            Script {
                end: Some("250-2.0.0 queued\r\n554 5.7.1 rejected"),
                ..Script::default()
            },
            SubmitError::Uncertain(UncertainReason::UnreadableReply),
        ),
    ] {
        let server = Server::start(vec![script]).await;
        let mut submitter = server.connect().await.unwrap();
        let error = send(&mut submitter, &to(&["b@x.test"]), MESSAGE)
            .await
            .unwrap_err();
        assert_eq!(error, expected);
        assert_eq!(server.seen().messages.len(), 1, "the data went out");
        if let SubmitError::Uncertain(reason) = expected {
            assert_eq!(error.failure(), SubmitFailure::Uncertain(reason));
            assert!(
                submitter.conn.is_none(),
                "an uncertain connection is dropped"
            );
        }
        submitter.quit().await;
    }
}

#[tokio::test]
async fn a_long_acceptance_is_a_sent_message() {
    let reply = format!("250 2.0.0 OK {}", "x".repeat(4_000));
    let server = Server::start(vec![Script {
        end: Some(Box::leak(reply.into_boxed_str())),
        ..Script::default()
    }])
    .await;
    let mut submitter = server.connect().await.unwrap();
    send(&mut submitter, &to(&["b@x.test"]), MESSAGE)
        .await
        .unwrap();
    submitter.quit().await;
    assert_eq!(server.seen().messages.len(), 1);
}

#[tokio::test]
async fn a_token_goes_to_no_smtp_server_but_microsofts() {
    // An account whose SMTP server isn't Microsoft's, as detection could find from a DNS answer
    // alone: no token is fetched for it and nothing connects.
    let (listener, port) = bind().await;
    let endpoint = Endpoint {
        host: "127.0.0.1".into(),
        port,
        security: Security::Tls,
        certificate: None,
    };
    let asked = Arc::new(Mutex::new(0));
    let counted = asked.clone();
    let result = connect_oauth(
        &endpoint,
        "signin@example.test",
        &Trust::Platform,
        quick(),
        move |_| {
            *counted.lock().unwrap() += 1;
            async { Ok(fetched("token", false)) }
        },
    )
    .await;
    assert!(matches!(
        result,
        Err(SubmitError::Connect(LoginFailure::Other))
    ));
    assert_eq!(*asked.lock().unwrap(), 0, "no token was fetched");
    assert!(
        tokio::time::timeout(Duration::from_millis(200), listener.accept())
            .await
            .is_err(),
        "nothing connected"
    );
}

#[tokio::test]
async fn a_reply_line_that_never_ends_is_given_up_at_its_limit() {
    // lettre read a whole line before checking its length, so a server that never ended one
    // filled the client's memory until the timeout: 300 s at the message's end.
    let server = Server::start(vec![Script {
        endless: true,
        ..Script::default()
    }])
    .await;
    let mut submitter = server.connect().await.unwrap();
    let error = send(&mut submitter, &to(&["b@x.test"]), MESSAGE)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        SubmitError::Uncertain(UncertainReason::UnreadableReply)
    );
    assert!(submitter.conn.is_none(), "the connection is dropped");

    let ca = test_ca();
    let config = ca.server.clone();
    let (listener, port) = bind().await;
    tokio::spawn(async move {
        let mut peer = Peer::accept_tls(&listener, config).await;
        peer.flood("220 ", b'x').await;
    });
    let endpoint = Endpoint {
        host: "127.0.0.1".into(),
        port,
        security: Security::Tls,
        certificate: None,
    };
    let Err(error) = Submitter::connect_with(&endpoint, PASSWORD, &ca.trust, quick()).await else {
        panic!("a greeting that never ends is no connection");
    };
    assert_eq!(error, SubmitError::Connect(LoginFailure::Other));
}

#[tokio::test]
async fn a_reply_of_many_lines_is_parsed_once() {
    // lettre parsed the whole reply again after each line: 16,000 short lines took 268 s in a
    // debug build before the 100,000-byte cap, and no timeout stops it, since the reply is
    // already read and the parse never yields. A first line ending in a bare LF, which isn't one
    // of lettre's lines on its own, mustn't bring that back (605 s with a first, naive fix).
    for (first, lines, expected) in [
        ("", 14_000, Ok(())),
        ("250-a\n", 14_000, Ok(())),
        (
            "",
            16_000,
            Err(SubmitError::Uncertain(UncertainReason::UnreadableReply)),
        ),
    ] {
        let reply = format!("{first}{}250 ok", "250-x\r\n".repeat(lines));
        let server = Server::start(vec![Script {
            end: Some(Box::leak(reply.into_boxed_str())),
            ..Script::default()
        }])
        .await;
        let timeouts = Timeouts {
            data: Duration::from_secs(5),
            ..quick()
        };
        let mut submitter =
            Submitter::connect_with(&server.endpoint(), PASSWORD, &server.ca.trust, timeouts)
                .await
                .unwrap();
        let started = std::time::Instant::now();
        let result = send(&mut submitter, &to(&["b@x.test"]), MESSAGE).await;
        let took = started.elapsed();
        assert_eq!(result, expected, "{first:?} and {lines} lines");
        assert!(
            took < Duration::from_secs(20),
            "{first:?} and {lines} lines took {took:?}"
        );
        submitter.quit().await;
    }
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn an_unreadable_reply_is_logged_by_why_without_the_servers_text() {
    let capture = Capture::default();
    let sink = capture.clone();
    let _logs = tracing::subscriber::set_default(sealwax_infrastructure::logging::subscriber(
        tracing_subscriber::EnvFilter::new("trace"),
        move || sink.clone(),
        false,
    ));
    // While only one dispatcher was ever made, tracing-core caches a new callsite's interest from
    // the registering thread's subscriber alone, so a parallel test that reaches a callsite first
    // turns it off here too; with a second one it asks every dispatcher.
    let _second = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let reply = format!(
        "554 5.2.252 SendAsDenied; {}",
        "secret-words ".repeat(1_400)
    );
    let server = Server::start(vec![Script {
        end: Some(Box::leak(reply.into_boxed_str())),
        ..Script::default()
    }])
    .await;
    let mut submitter = server.connect().await.unwrap();
    let error = send(&mut submitter, &to(&["b@x.test"]), MESSAGE)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        SubmitError::Uncertain(UncertainReason::UnreadableReply)
    );
    let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        text.contains("a reply line longer than 16384 bytes")
            && text.contains("smtp reply to the message unknown"),
        "{text}"
    );
    assert!(
        !text.contains("SendAsDenied") && !text.contains("secret-words"),
        "{text}"
    );
}

#[tokio::test]
async fn checks_that_need_no_server_send_nothing() {
    let server = Server::start(vec![Script {
        ehlo: vec!["AUTH PLAIN", "SIZE 100"],
        ..Script::default()
    }])
    .await;
    let mut submitter = server.connect().await.unwrap();
    let large = [b'x'; 101];
    let cases: Vec<(Vec<String>, &[u8], SubmitError)> = vec![
        (
            to(&["b@x.test"]),
            b"bare\nfeed\r\n".as_slice(),
            SubmitError::Malformed,
        ),
        (
            to(&["b@x.test"]),
            b"bare\rreturn\r\n".as_slice(),
            SubmitError::Malformed,
        ),
        (
            to(&["b@x.test"]),
            b"nul\0\r\n".as_slice(),
            SubmitError::Malformed,
        ),
        (Vec::new(), b"ok\r\n".as_slice(), SubmitError::Malformed),
        (
            to(&["b@x.test", "no-at", "c d@x.test", "<e@x.test>"]),
            b"ok\r\n".as_slice(),
            SubmitError::InvalidAddress {
                sender: false,
                recipients: vec![1, 2, 3],
            },
        ),
        (
            to(&["b@x.test", "øyvind@x.test"]),
            b"ok\r\n".as_slice(),
            SubmitError::NeedsSmtpUtf8 {
                sender: false,
                recipients: vec![1],
            },
        ),
        (
            to(&["b@x.test"]),
            "8 bit æ\r\n".as_bytes(),
            SubmitError::NeedsEightBit,
        ),
        (
            to(&["b@x.test"]),
            large.as_slice(),
            SubmitError::TooLarge { limit: Some(100) },
        ),
    ];
    for (recipients, message, expected) in cases {
        let error = send(&mut submitter, &recipients, message)
            .await
            .unwrap_err();
        assert_eq!(error, expected);
        assert_eq!(error.failure(), SubmitFailure::Refuse);
    }
    let error = submitter
        .prepare(
            Envelope {
                from: "not an address",
                to: &to(&["b@x.test"]),
            },
            b"ok\r\n",
        )
        .await
        .unwrap_err();
    assert_eq!(
        error,
        SubmitError::InvalidAddress {
            sender: true,
            recipients: Vec::new()
        }
    );
    assert_eq!(error.send_error(&[]), SendError::Refused);
    submitter.quit().await;
    assert_eq!(server.seen().verbs(), ["EHLO", "EHLO", "AUTH", "QUIT"]);
}

#[tokio::test]
async fn smtputf8_and_8bitmime_are_asked_for_only_when_needed() {
    let server = Server::start(vec![Script {
        ehlo: vec!["AUTH PLAIN", "smtputf8", "8bitmime"],
        ..Script::default()
    }])
    .await;
    let mut submitter = server.connect().await.unwrap();
    let message = "Subject: blåbær\r\n\r\nhei\r\n".as_bytes();
    send(&mut submitter, &to(&["b@blåbær.test"]), message)
        .await
        .unwrap();
    send(&mut submitter, &to(&["øyvind@x.test"]), b"plain\r\n")
        .await
        .unwrap();
    submitter.quit().await;
    let lines = server.seen().lines;
    let mail: Vec<&String> = lines.iter().filter(|l| l.starts_with("MAIL")).collect();
    let rcpt: Vec<&String> = lines.iter().filter(|l| l.starts_with("RCPT")).collect();
    assert_eq!(mail[0], "MAIL FROM:<a@example.test> BODY=8BITMIME");
    assert_eq!(rcpt[0], "RCPT TO:<b@xn--blbr-roah.test>");
    assert_eq!(mail[1], "MAIL FROM:<a@example.test> SMTPUTF8");
    assert_eq!(rcpt[1], "RCPT TO:<øyvind@x.test>");
}

#[tokio::test]
async fn a_dropped_envelope_is_reset_before_the_next() {
    let server = Server::start(vec![Script::default()]).await;
    let mut submitter = server.connect().await.unwrap();
    let recipients = to(&["b@x.test"]);
    let envelope = Envelope {
        from: "a@example.test",
        to: &recipients,
    };
    let _abandoned = submitter.prepare(envelope, MESSAGE).await.unwrap();
    submitter
        .prepare(envelope, MESSAGE)
        .await
        .unwrap()
        .cancel()
        .await
        .unwrap();
    send(&mut submitter, &recipients, MESSAGE).await.unwrap();
    submitter.quit().await;
    assert_eq!(
        server.seen().verbs()[3..],
        [
            "MAIL", "RCPT", "RSET", "MAIL", "RCPT", "RSET", "MAIL", "RCPT", "DATA", "QUIT"
        ]
    );
}

#[tokio::test]
async fn refused_logins_are_auth_failures_and_nothing_else_is_sent() {
    let server = Server::start(vec![Script {
        auth: vec!["535 5.7.8 Authentication credentials invalid"],
        ..Script::default()
    }])
    .await;
    let error = server.connect().await.unwrap_err();
    assert_eq!(error, SubmitError::Auth(LoginFailure::WrongPassword));
    assert_eq!(error.failure(), SubmitFailure::Account);
    assert_eq!(error.send_error(&[]), SendError::NeedsSignIn);
    let (listener, port) = bind().await;
    drop(listener);
    let closed = Endpoint {
        host: "127.0.0.1".into(),
        port,
        security: Security::Tls,
        certificate: None,
    };
    let error = Submitter::connect_with(&closed, PASSWORD, &server.ca.trust, quick())
        .await
        .unwrap_err();
    assert_eq!(error, SubmitError::Connect(LoginFailure::Unreachable));
    assert_eq!(error.failure(), SubmitFailure::Retry);
    assert_eq!(error.send_error(&[]), SendError::Offline);
}

fn fetched(token: &str, refreshed: bool) -> Fetched {
    Fetched {
        token: Zeroizing::new(token.to_owned()),
        refreshed,
    }
}

fn xoauth2(line: &str) -> String {
    let encoded = line.strip_prefix("AUTH XOAUTH2 ").unwrap();
    String::from_utf8(STANDARD.decode(encoded).unwrap()).unwrap()
}

#[tokio::test]
async fn a_refused_token_is_refreshed_once_and_the_error_challenge_answered() {
    let challenge = "334 eyJzdGF0dXMiOiI0MDEiLCJzY2hlbWVzIjoiQmVhcmVyIn0=";
    let server = Server::start(vec![
        Script {
            auth: vec![challenge, "535 5.7.3 Authentication unsuccessful"],
            ..Script::default()
        },
        Script::default(),
    ])
    .await;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let asked = calls.clone();
    let mut submitter = connect_oauth(
        &server.endpoint(),
        "user",
        &server.ca.trust,
        quick(),
        |renew| {
            asked.lock().unwrap().push(renew);
            let token = if renew { "fresh-token" } else { "stale-token" };
            std::future::ready(Ok(fetched(token, renew)))
        },
    )
    .await
    .unwrap();
    send(&mut submitter, &to(&["b@x.test"]), MESSAGE)
        .await
        .unwrap();
    submitter.quit().await;
    assert_eq!(*calls.lock().unwrap(), [false, true]);
    let lines = server.seen().lines;
    let auths: Vec<&String> = lines.iter().filter(|l| l.starts_with("AUTH")).collect();
    assert_eq!(
        xoauth2(auths[0]),
        "user=user\x01auth=Bearer stale-token\x01\x01"
    );
    assert_eq!(
        xoauth2(auths[1]),
        "user=user\x01auth=Bearer fresh-token\x01\x01"
    );
    let challenge_answer = lines.iter().position(|l| l.is_empty()).unwrap();
    assert!(lines[challenge_answer - 1].starts_with("AUTH XOAUTH2"));

    let server = Server::start(vec![Script {
        auth: vec!["535 5.7.3 Authentication unsuccessful"],
        ..Script::default()
    }])
    .await;
    let error = connect_oauth(
        &server.endpoint(),
        "user",
        &server.ca.trust,
        quick(),
        |_| std::future::ready(Ok(fetched("fresh-token", true))),
    )
    .await
    .unwrap_err();
    assert_eq!(error, SubmitError::Auth(LoginFailure::OAuthRequired));
}

#[tokio::test]
async fn the_limit_needs_no_login() {
    let server = Server::start(vec![
        Script {
            ehlo: vec!["AUTH PLAIN", "size 52428800"],
            ..Script::default()
        },
        Script {
            ehlo: vec!["AUTH PLAIN", "SIZE 0"],
            ..Script::default()
        },
        Script {
            ehlo: vec!["AUTH PLAIN"],
            ..Script::default()
        },
    ])
    .await;
    for expected in [Some(52_428_800), None, None] {
        let limit = submission_limit(&server.endpoint(), &server.ca.trust, quick())
            .await
            .unwrap();
        assert_eq!(limit, expected);
    }
    let verbs = server.seen().verbs();
    assert!(!verbs.contains(&"AUTH".to_owned()), "{verbs:?}");
}

#[test]
fn paths_and_bare_line_ends() {
    assert!(path("a@b.test").is_some_and(|p| !p.utf8));
    assert!(path(" a@b.test ").is_some_and(|p| p.text == "a@b.test"));
    assert!(path("ø@b.test").is_some_and(|p| p.utf8));
    assert_eq!(path("a@bücher.test").unwrap().text, "a@xn--bcher-kva.test");
    for bad in [
        "",
        "a",
        "@b",
        "a@",
        "a b@c",
        "a@b\r\nRCPT TO:<x@y>",
        "<a@b>",
    ] {
        assert!(path(bad).is_none(), "{bad:?}");
    }
    assert!(!malformed(b"a\r\nb\r\n"));
    assert!(malformed(b"\na"));
    assert!(malformed(b"a\r"));
    assert!(malformed(b"a\r\r\n"));
}

#[test]
fn debug_forms_print_no_addresses() {
    let recipients = to(&["secret@x.test"]);
    let envelope = Envelope {
        from: "me@x.test",
        to: &recipients,
    };
    let printed = format!("{envelope:?}");
    assert!(!printed.contains('@'), "{printed}");
}
