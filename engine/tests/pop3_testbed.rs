//! The POP3 client (ML-087) on the Docker testbed: Dovecot's and Stalwart's POP3 with implicit
//! TLS (14995, 16995) and STLS (14110, 16110). alice's INBOX is only listed and read, and only a
//! message IMAP already shows `\Seen` is retrieved, so no flag changes; the `popclient` user's
//! INBOX takes this suite's appends, DELE and QUIT; perf's 10,000 messages are listed only. The
//! IMAP side is read with the plain testbed client. Needs the testbed running and seeded, one
//! test at a time (the log test sets a subscriber for its thread):
//!
//! SEALWAX_TESTBED_CA=tools/testbed/certs/ca.pem cargo test -p sealwax-engine --release \
//!   --test pop3_testbed -- --ignored --test-threads=1 --nocapture

mod common;
mod proxy;
mod snapshot;
mod testbed_client;

use async_imap::imap_proto::{AttributeValue, Response};
use common::{DOVECOT, PASSWORD, STALWART, Server as Imap, ca, submission, trust};
use proxy::{Proxy, Rule};
use sealwax_domain::account::{
    AccountSettings, Endpoint, LoginFailure, Pop3Policy, Receive, Security,
};
use sealwax_engine::api::Password;
use sealwax_engine::pop3::{self, Credentials, Listing, Mailbox, Pop3Error, Session};
use sealwax_engine::verify::verify;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use testbed_client::Client;

const USER: &str = "popclient";

#[derive(Clone, Copy, Debug)]
struct Pop3 {
    name: &'static str,
    tls: u16,
    stls: u16,
    imap: Imap,
}

const DOVECOT_POP3: Pop3 = Pop3 {
    name: "dovecot",
    tls: 14995,
    stls: 14110,
    imap: DOVECOT,
};
const STALWART_POP3: Pop3 = Pop3 {
    name: "stalwart",
    tls: 16995,
    stls: 16110,
    imap: STALWART,
};
const SERVERS: [Pop3; 2] = [DOVECOT_POP3, STALWART_POP3];
const SECURITIES: [Security; 2] = [Security::Tls, Security::StartTls];

fn endpoint(server: Pop3, security: Security) -> Endpoint {
    common::endpoint(
        match security {
            Security::Tls => server.tls,
            Security::StartTls => server.stls,
        },
        security,
    )
}

fn credentials(user: &str) -> Credentials<'_> {
    Credentials::Password {
        username: user,
        password: PASSWORD,
    }
}

async fn session(server: Pop3, security: Security, user: &str) -> Session {
    pop3::connect(
        &endpoint(server, security),
        credentials(user),
        trust().client_config().unwrap(),
    )
    .await
    .unwrap_or_else(|error| panic!("{} {security:?}: {error}", server.name))
}

/// A message in IMAP's INBOX, in sequence order.
struct Stored {
    seen: bool,
    message_id: Option<String>,
    bytes: Vec<u8>,
}

/// The INBOX as IMAP shows it: EXAMINE (nothing changes), then every message's flags and bytes.
async fn imap_inbox(server: Imap, user: &str) -> Vec<Stored> {
    let mut client = Client::connect(server.tls, &ca(), user).await.unwrap();
    client.uidvalidity("INBOX").await.unwrap();
    let mut found: Vec<(u32, Stored)> = Vec::new();
    let fetched = client
        .run("FETCH 1:* (FLAGS BODY.PEEK[])", |response| {
            if let Response::Fetch(seq, attributes) = response {
                let mut seen = false;
                let mut bytes = Vec::new();
                for attribute in attributes {
                    match attribute {
                        AttributeValue::Flags(flags) => {
                            seen = flags.iter().any(|flag| flag.eq_ignore_ascii_case("\\Seen"));
                        }
                        AttributeValue::BodySection {
                            section: None,
                            data,
                            ..
                        } => bytes = data.as_deref().unwrap_or_default().to_vec(),
                        _ => {}
                    }
                }
                found.push((
                    *seq,
                    Stored {
                        seen,
                        message_id: message_id(&bytes),
                        bytes,
                    },
                ));
            }
        })
        .await;
    client.logout().await;
    if fetched.is_err() {
        return Vec::new();
    }
    found.sort_by_key(|(seq, _)| *seq);
    found.into_iter().map(|(_, stored)| stored).collect()
}

/// INBOX's message count, as IMAP sees it now.
async fn imap_count(server: Imap, user: &str) -> usize {
    imap_inbox(server, user).await.len()
}

/// The Message-ID header's value.
fn message_id(bytes: &[u8]) -> Option<String> {
    let header = &bytes[..header_end(bytes)];
    String::from_utf8_lossy(header)
        .split("\r\n")
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("message-id")
                .then(|| value.trim().to_owned())
        })
}

/// Where the header ends: after its blank line.
fn header_end(bytes: &[u8]) -> usize {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map_or(bytes.len(), |at| at + 4)
}

/// A synthetic message for popclient's INBOX: dot-stuffing cases, 8-bit text and filler up to
/// about `size` octets, with a marker line in the middle.
fn fixture(id: &str, size: usize) -> Vec<u8> {
    let mut message = format!(
        "From: Testbed <testbed@example.test>\r\nTo: {USER}@example.test\r\n\
         Subject: POP3 fixture {id}\r\nDate: Wed, 07 Oct 2026 03:00:00 +0000\r\n\
         Message-ID: <{id}@example.test>\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n\
         .\r\n..two dots\r\n.leading dot\r\nbl\u{e5}b\u{e6}r og s\u{f8}t\u{f8}l\r\n"
    )
    .into_bytes();
    let mut state = 0x9E37_79B9_7F4A_7C15u64 ^ size as u64;
    let alphabet = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+/";
    let mut marked = false;
    while message.len() < size {
        if !marked && message.len() >= size / 2 {
            message.extend_from_slice(b"MIDPOINT-MARKER\r\n");
            marked = true;
        }
        for _ in 0..76 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            message.push(alphabet[(state >> 58) as usize]);
        }
        message.extend_from_slice(b"\r\n");
    }
    message.extend_from_slice(b"last line\r\n");
    message
}

/// Empties popclient's INBOX and appends `messages`, unseen, in order.
async fn reset_inbox(server: Imap, messages: &[Vec<u8>]) {
    let mut client = Client::connect(server.tls, &ca(), USER).await.unwrap();
    client.empty("INBOX").await.unwrap();
    for message in messages {
        client.append("INBOX", "", message).await.unwrap();
    }
    client.logout().await;
}

fn percentile(samples: &mut [f64], p: f64) -> f64 {
    samples.sort_by(f64::total_cmp);
    let rank = ((p / 100.0) * samples.len() as f64).ceil() as usize;
    samples[rank.clamp(1, samples.len()) - 1]
}

fn ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

fn tag() -> String {
    format!(
        "pop3-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    )
}

#[tokio::test]
#[ignore = "needs the testbed: tools/testbed/up.sh and seed.sh"]
async fn alice_is_listed_and_read_on_every_port_byte_for_byte_with_imap() {
    for server in SERVERS {
        let stored = imap_inbox(server.imap, "alice").await;
        assert_eq!(stored.len(), 30, "{}: alice's seeded INBOX", server.name);
        let (k, expected) = stored
            .iter()
            .enumerate()
            .find(|(_, message)| message.seen)
            .map(|(index, message)| (index as u32 + 1, message))
            .unwrap();
        let mut uidls: Option<Vec<Listing>> = None;
        for security in SECURITIES {
            let started = Instant::now();
            let mut session = session(server, security, "alice").await;
            let connected = ms(started);
            let caps = session.capabilities().clone();
            let listing = session.listing().await.unwrap();
            assert_eq!(listing.len(), 30);
            assert!(
                listing
                    .iter()
                    .enumerate()
                    .all(|(i, entry)| entry.number == i as u32 + 1)
            );
            let header = session.header(k).await.unwrap();
            if message_id(&header) != expected.message_id {
                // Not the message IMAP showed as read: leave without QUIT so the server changes
                // no flag, and fail.
                drop(session);
                panic!("{}: POP3's order differs from IMAP's", server.name);
            }
            assert_eq!(header, &expected.bytes[..header_end(&expected.bytes)]);
            let message = session.retrieve(k).await.unwrap();
            assert_eq!(message, expected.bytes, "{} {security:?}", server.name);
            let listed = listing[k as usize - 1].size;
            let mechanism = session.mechanism();
            session.quit().await.unwrap();
            println!(
                "{} {security:?}: connected in {connected:.1} ms, mechanism {:?}, top {}, uidl {}, \
                 expire {:?}, login-delay {:?}; 30 listed ({} octets); message {k}: LIST {listed}, \
                 RETR {} octets, equal to IMAP's",
                server.name,
                mechanism,
                caps.top,
                caps.uidl,
                caps.expire,
                caps.login_delay,
                listing.iter().map(|entry| entry.size).sum::<u64>(),
                message.len(),
            );
            match &uidls {
                None => uidls = Some(listing),
                Some(first) => assert_eq!(first, &listing, "the same UIDLs over STLS"),
            }
        }
        let after = imap_inbox(server.imap, "alice").await;
        let flags = |messages: &[Stored]| messages.iter().map(|m| m.seen).collect::<Vec<_>>();
        assert_eq!(
            flags(&after),
            flags(&stored),
            "{}: no flag changed",
            server.name
        );
    }
}

#[tokio::test]
#[ignore = "needs the testbed: tools/testbed/up.sh and seed.sh"]
async fn the_login_check_and_verify_on_every_port() {
    for server in SERVERS {
        for security in SECURITIES {
            let endpoint = endpoint(server, security);
            pop3::check_login(
                &endpoint,
                credentials("alice"),
                trust().client_config().unwrap(),
            )
            .await
            .unwrap_or_else(|failure| panic!("{} {security:?}: {failure:?}", server.name));
            let started = Instant::now();
            let failure = pop3::check_login(
                &endpoint,
                Credentials::Password {
                    username: "alice",
                    password: "wrong-password",
                },
                trust().client_config().unwrap(),
            )
            .await
            .unwrap_err();
            assert_eq!(failure, LoginFailure::WrongPassword, "{}", server.name);
            println!(
                "{} {security:?}: a wrong password refused as WrongPassword in {:.0} ms",
                server.name,
                ms(started)
            );
            let account = AccountSettings {
                receive: Receive::Pop3(Pop3Policy::default()),
                ..common::settings(endpoint.clone(), submission(Security::Tls), "alice")
            };
            tokio::time::sleep(Duration::from_millis(300)).await;
            let verified = verify(
                &account,
                None,
                &Password::new(PASSWORD.to_owned()),
                &trust(),
            )
            .await
            .unwrap_or_else(|error| panic!("{} {security:?}: {error:?}", server.name));
            assert_eq!(verified, "alice");
        }
    }
}

#[tokio::test]
#[ignore = "needs the testbed: tools/testbed/up.sh and seed.sh"]
async fn dele_takes_effect_only_at_a_successful_quit() {
    for server in SERVERS {
        let run = tag();
        let messages: Vec<Vec<u8>> = (1..=3)
            .map(|i| fixture(&format!("{run}-{i}"), 600))
            .collect();
        reset_inbox(server.imap, &messages).await;
        let ids: Vec<String> = (1..=3)
            .map(|i| format!("<{run}-{i}@example.test>"))
            .collect();

        // Marked and dropped without QUIT: nothing is deleted.
        let mut first = session(server, Security::Tls, USER).await;
        let listing = first.listing().await.unwrap();
        assert_eq!(listing.len(), 3);
        for entry in &listing {
            let header = first.header(entry.number).await.unwrap();
            assert_eq!(
                message_id(&header).as_ref(),
                Some(&ids[entry.number as usize - 1])
            );
        }
        first.delete(1).await.unwrap();
        drop(first);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            imap_count(server.imap, USER).await,
            3,
            "{}: dropped",
            server.name
        );

        // Marked, RSET, QUIT: nothing is deleted.
        let mut second = session(server, Security::Tls, USER).await;
        second.listing().await.unwrap();
        second.delete(1).await.unwrap();
        second.reset().await.unwrap();
        second.quit().await.unwrap();
        assert_eq!(
            imap_count(server.imap, USER).await,
            3,
            "{}: RSET",
            server.name
        );

        // Retrieved, marked, QUIT: the marked message is gone.
        let mut third = session(server, Security::StartTls, USER).await;
        let before = third.listing().await.unwrap();
        assert_eq!(
            before, listing,
            "{}: UIDLs stable across sessions",
            server.name
        );
        let retrieved = third.retrieve(2).await.unwrap();
        assert_eq!(retrieved, messages[1], "{}: as appended", server.name);
        third.delete(1).await.unwrap();
        let again = third.delete(1).await;
        assert!(matches!(again, Err(Pop3Error::Err)), "{again:?}");
        third.noop().await.unwrap();
        third.quit().await.unwrap();
        let stored = imap_inbox(server.imap, USER).await;
        let left: Vec<_> = stored.iter().filter_map(|m| m.message_id.clone()).collect();
        assert_eq!(left, ids[1..], "{}: QUIT deleted message 1", server.name);
        println!(
            "{}: after RETR and QUIT the retrieved message is {}",
            server.name,
            if stored[0].seen {
                "\\Seen"
            } else {
                "not \\Seen"
            }
        );

        let mut fourth = session(server, Security::Tls, USER).await;
        let after = fourth.listing().await.unwrap();
        let uidls = |listing: &[Listing]| -> Vec<String> {
            listing.iter().map(|entry| entry.uidl.clone()).collect()
        };
        assert_eq!(uidls(&after), uidls(&listing[1..]), "{}", server.name);
        assert_eq!(after[0].number, 1, "renumbered from 1");
        fourth.quit().await.unwrap();
        reset_inbox(server.imap, &[]).await;
    }
}

#[tokio::test]
#[ignore = "needs the testbed: tools/testbed/up.sh and seed.sh"]
async fn a_second_session_while_one_holds_the_maildrop() {
    for server in SERVERS {
        reset_inbox(server.imap, &[fixture(&format!("{}-busy", tag()), 300)]).await;
        let mut first = session(server, Security::Tls, USER).await;
        first.listing().await.unwrap();
        let second = pop3::connect(
            &endpoint(server, Security::Tls),
            credentials(USER),
            trust().client_config().unwrap(),
        )
        .await;
        match second {
            Ok(mut second) => {
                let listed = second.listing().await.map(|listing| listing.len());
                println!(
                    "{}: a second session logs in while the first is open (listing {listed:?})",
                    server.name
                );
                second.quit().await.unwrap();
            }
            Err(error) => println!(
                "{}: a second session is refused while the first is open: {error:?}",
                server.name
            ),
        }
        first.quit().await.unwrap();
        reset_inbox(server.imap, &[]).await;
    }
}

#[tokio::test]
#[ignore = "needs the testbed: tools/testbed/up.sh and seed.sh"]
async fn faults_through_the_proxy() {
    let runs: usize = std::env::var("SEALWAX_POP3_SEEDS")
        .ok()
        .and_then(|runs| runs.parse().ok())
        .unwrap_or(5);
    for server in SERVERS {
        let proxy = Proxy::start(server.tls, &ca()).await;
        let through = common::endpoint(proxy.port, Security::Tls);
        let connect = || {
            pop3::connect(
                &through,
                credentials(USER),
                trust().client_config().unwrap(),
            )
        };
        let mut cut_mid_retr = 0;
        let mut quit_never_sent_kept = 0;
        let mut quit_reply_lost_deleted = 0;
        for run in 0..runs {
            let id = format!("{}-fault-{run}", tag());
            let large = fixture(&format!("{id}-large"), 1024 * 1024);
            let small = fixture(&format!("{id}-small"), 400);
            reset_inbox(server.imap, &[large.clone(), small.clone()]).await;

            // Cut part of the way through RETR: nothing comes back, nothing is lost.
            let mut session = connect().await.unwrap();
            session.listing().await.unwrap();
            proxy.arm(Rule {
                needle: b"MIDPOINT-MARKER".to_vec(),
                nth: 1,
                extra: 1000,
            });
            let error = session.retrieve(1).await.unwrap_err();
            assert!(error.is_connection(), "{}: {error}", server.name);
            assert!(matches!(
                session.quit().await,
                Err(Pop3Error::ConnectionLost)
            ));
            cut_mid_retr += 1;
            let mut direct = self::session(server, Security::Tls, USER).await;
            assert_eq!(direct.listing().await.unwrap().len(), 2);
            assert_eq!(direct.retrieve(1).await.unwrap(), large);
            direct.quit().await.unwrap();

            // QUIT cut before the server sees it: the marked message stays.
            let mut session = connect().await.unwrap();
            session.listing().await.unwrap();
            session.delete(2).await.unwrap();
            proxy.cut_on_send("QUIT");
            let error = session.quit().await.unwrap_err();
            assert!(error.is_connection(), "{error}");
            tokio::time::sleep(Duration::from_millis(300)).await;
            if imap_count(server.imap, USER).await == 2 {
                quit_never_sent_kept += 1;
            }

            // QUIT's reply lost after the server acted: the client can't tell, the next listing
            // can.
            let mut session = connect().await.unwrap();
            session.listing().await.unwrap();
            session.delete(2).await.unwrap();
            proxy.arm(Rule {
                needle: b"+OK".to_vec(),
                nth: 1,
                extra: 0,
            });
            let error = session.quit().await.unwrap_err();
            assert!(error.is_connection(), "{error}");
            tokio::time::sleep(Duration::from_millis(300)).await;
            let mut direct = self::session(server, Security::Tls, USER).await;
            let left = direct.listing().await.unwrap().len();
            direct.quit().await.unwrap();
            if left == 1 {
                quit_reply_lost_deleted += 1;
            }
            proxy.disarm();
        }
        println!(
            "{}: {runs} runs: cut mid-RETR → ConnectionLost and the message intact {cut_mid_retr}/{runs}; \
             QUIT cut before the server → the marked message kept {quit_never_sent_kept}/{runs}; \
             QUIT's reply lost → deleted on the server {quit_reply_lost_deleted}/{runs}",
            server.name
        );
        assert_eq!(quit_never_sent_kept, runs, "{}", server.name);
        reset_inbox(server.imap, &[]).await;
    }
}

#[tokio::test]
#[ignore = "needs the testbed: tools/testbed/up.sh and seed.sh"]
async fn timings() {
    const CONNECTS: usize = 20;
    const LISTINGS: usize = 5;
    const RETRIEVALS: usize = 10;
    for server in SERVERS {
        for security in SECURITIES {
            let mut connects = Vec::new();
            for _ in 0..CONNECTS {
                let started = Instant::now();
                let session = session(server, security, USER).await;
                connects.push(ms(started));
                session.quit().await.unwrap();
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            println!(
                "{} {security:?}: connect and log in p50 {:.1} ms, p95 {:.1} ms ({CONNECTS} runs)",
                server.name,
                percentile(&mut connects, 50.0),
                percentile(&mut connects, 95.0)
            );
        }

        let mut listings = Vec::new();
        for _ in 0..LISTINGS {
            let mut session = session(server, Security::Tls, "perf").await;
            let started = Instant::now();
            let listing = session.listing().await.unwrap();
            listings.push(ms(started));
            assert_eq!(listing.len(), 10_000, "{}: perf's INBOX", server.name);
            session.quit().await.unwrap();
        }
        println!(
            "{}: UIDL and LIST of 10,000 p50 {:.1} ms, p95 {:.1} ms ({LISTINGS} runs)",
            server.name,
            percentile(&mut listings, 50.0),
            percentile(&mut listings, 95.0)
        );

        let large = fixture(&format!("{}-timing", tag()), 1024 * 1024);
        reset_inbox(server.imap, std::slice::from_ref(&large)).await;
        let mut session = session(server, Security::Tls, USER).await;
        let listing = session.listing().await.unwrap();
        let mut retrievals = Vec::new();
        for _ in 0..RETRIEVALS {
            let started = Instant::now();
            let message = session.retrieve(1).await.unwrap();
            retrievals.push(ms(started));
            assert_eq!(message, large);
        }
        session.quit().await.unwrap();
        println!(
            "{}: RETR of {} octets (LIST {}) p50 {:.1} ms, p95 {:.1} ms ({RETRIEVALS} runs, one session)",
            server.name,
            large.len(),
            listing[0].size,
            percentile(&mut retrievals, 50.0),
            percentile(&mut retrievals, 95.0)
        );
        reset_inbox(server.imap, &[]).await;
    }
}

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

/// ADR-018 on the testbed: logins, listings, a retrieval, DELE, QUIT and a refused login at
/// trace level under the app's own filter leave no user, password, UIDL, Message-ID, subject,
/// body text or protocol line in the log.
#[tokio::test]
#[ignore = "needs the testbed: tools/testbed/up.sh and seed.sh"]
async fn the_log_holds_no_user_secret_uidl_or_content() {
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let run = tag();
    let mut uidls = Vec::new();
    for server in SERVERS {
        reset_inbox(server.imap, &[fixture(&format!("{run}-log"), 2000)]).await;
        for security in SECURITIES {
            let mut session = session(server, security, USER).await;
            let listing = session.listing().await.unwrap();
            uidls.extend(listing.iter().map(|entry| entry.uidl.clone()));
            session.retrieve(1).await.unwrap();
            session.header(1).await.unwrap();
            if security == Security::StartTls {
                session.delete(1).await.unwrap();
            }
            session.quit().await.unwrap();
        }
        let failure = pop3::check_login(
            &endpoint(server, Security::Tls),
            Credentials::Password {
                username: USER,
                password: "wrong-password",
            },
            trust().client_config().unwrap(),
        )
        .await
        .unwrap_err();
        assert_eq!(failure, LoginFailure::WrongPassword);
    }
    let log = String::from_utf8_lossy(&capture.0.lock().unwrap()).into_owned();
    let lines = log.lines().count();
    for event in [
        "pop3 connected",
        "pop3 listed",
        "pop3 retrieved",
        "pop3 quit",
        "pop3 login check failed",
    ] {
        assert!(log.contains(event), "{event} missing");
    }
    let mut forbidden: Vec<String> = [
        USER,
        PASSWORD,
        "wrong-password",
        "example.test",
        run.as_str(),
        "POP3 fixture",
        "MIDPOINT",
        "two dots",
        "USER ",
        "PASS ",
        "AUTH ",
        "RETR",
        "DELE",
        "+OK",
        "-ERR",
        "Dovecot",
        "Stalwart",
    ]
    .iter()
    .map(|text| text.to_string())
    .collect();
    forbidden.extend(uidls);
    for text in &forbidden {
        assert!(!log.contains(text.as_str()), "{text:?} in the log");
    }
    println!(
        "log grep: {lines} lines at trace, {} forbidden strings absent",
        forbidden.len()
    );
}
