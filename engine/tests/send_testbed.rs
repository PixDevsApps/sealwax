//! M6's testbed suites for SMTP submission (ML-075): the size limit and extensions of Stalwart's
//! submission ports and Mailpit; S7's fixture messages submitted as alice through Stalwart's
//! ports to bob (To), carol (Cc) and dave (Bcc) and read back over IMAP byte for byte, and
//! through Mailpit, read back through its API, round-tripped through mail-parser and scanned
//! for a Bcc field and this machine's host name; refusals; the connection cut before DATA (sent
//! again, once) and after the end of the data (uncertain, never sent again) through the fault
//! proxy, with what Stalwart actually delivered; and submission times. Sending goes only to the
//! testbed on 127.0.0.1; every address is a testbed user or a dummy at example.com, .org or
//! .net, which Mailpit catches and never relays.
//!
//! SEALWAX_TESTBED_CA=…/tools/testbed/certs/ca.pem \
//!   cargo test -p sealwax-engine --release --test send_testbed -- --ignored --test-threads=1 --nocapture

mod common;
mod outgoing;
mod proxy;
mod snapshot;
mod testbed_client;

use common::{PASSWORD, STALWART};
use outgoing::{
    Fixture, Mailpit, contains, fixtures, has_bcc_header, hostname, ms, percentiles, prepended,
    run_tag, seen, synthetic, with_id,
};
use proxy::Proxy;
use sealwax_domain::account::{Endpoint, LoginFailure, Security};
use sealwax_engine::imap::Credentials;
use sealwax_engine::smtp::submit::{
    self, Envelope, SubmitError, SubmitFailure, Submitter, Timeouts,
};
use std::time::{Duration, Instant};
use testbed_client::Client;

const ALICE: Credentials<'static> = Credentials::Password {
    username: "alice",
    password: PASSWORD,
};
/// Mailpit takes any login.
const ANYONE: Credentials<'static> = Credentials::Password {
    username: "sealwax-test",
    password: "any-password",
};
const SENDER: &str = "alice@example.test";

fn runtime() -> tokio::runtime::Runtime {
    sealwax_engine::runtime().unwrap()
}

fn tls_endpoint(port: u16) -> Endpoint {
    common::endpoint(port, Security::Tls)
}

/// Stalwart closes a sixth submission connection from one address within a second, unanswered
/// (found here, 2026-10-06), so the suites leave a gap before each.
async fn pace() {
    tokio::time::sleep(Duration::from_millis(250)).await;
}

/// Connects, submits `bytes` once and quits.
async fn submit_once(
    endpoint: &Endpoint,
    credentials: Credentials<'_>,
    timeouts: Timeouts,
    from: &str,
    to: &[String],
    bytes: &[u8],
) -> Result<(), SubmitError> {
    pace().await;
    let mut submitter =
        Submitter::connect_with(endpoint, credentials, &common::trust(), timeouts).await?;
    let result = match submitter.prepare(Envelope { from, to }, bytes).await {
        Ok(prepared) => prepared.send().await,
        Err(error) => Err(error),
    };
    submitter.quit().await;
    result
}

async fn mailbox(user: &str) -> Client {
    Client::connect(STALWART.tls, &common::ca(), user)
        .await
        .unwrap_or_else(|error| panic!("{user} on stalwart: {error:?}"))
}

/// Empties the INBOX of Stalwart's recipients.
async fn clear(users: &[&str]) {
    for user in users {
        let mut client = mailbox(user).await;
        client.empty("INBOX").await.unwrap();
        client.logout().await;
    }
}

/// The copies of `id` in `user`'s INBOX on Stalwart after waiting up to `wait` for one.
async fn delivered(user: &str, id: &str, wait: Duration) -> Vec<outgoing::Held> {
    let mut client = mailbox(user).await;
    let found = outgoing::arrived(&mut client, "INBOX", id, wait).await;
    client.logout().await;
    found
}

fn header_names(prefix: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(prefix)
        .split("\r\n")
        .filter(|line| !line.is_empty() && !line.starts_with([' ', '\t']))
        .filter_map(|line| line.split_once(':').map(|(name, _)| name.to_owned()))
        .collect()
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn limits_and_extensions_of_stalwart_and_mailpit() {
    runtime().block_on(async {
        for (name, endpoint, credentials) in [
            (
                "stalwart submission 587 STARTTLS",
                common::submission(Security::StartTls),
                ALICE,
            ),
            (
                "stalwart submissions 465 TLS",
                common::submission(Security::Tls),
                ALICE,
            ),
            ("mailpit 1025 STARTTLS", common::mailpit(), ANYONE),
        ] {
            pace().await;
            let started = Instant::now();
            let limit = submit::submission_limit(&endpoint, &common::trust(), Timeouts::default())
                .await
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            let elapsed = started.elapsed();
            pace().await;
            let submitter = Submitter::connect(&endpoint, credentials, &common::trust())
                .await
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            let extensions = submitter.extensions();
            submitter.quit().await;
            println!(
                "{name}: limit without login {limit:?} in {:.1} ms; after login {extensions:?}",
                ms(elapsed)
            );
            assert!(limit.is_some(), "{name} names SIZE before login");
            assert_eq!(limit, extensions.size, "{name}");
            assert!(extensions.smtputf8 && extensions.eight_bit, "{name}");
        }
    });
}

/// The testbed users a fixture's envelope maps to on Stalwart: To → bob, Cc → carol, Bcc →
/// dave.
fn testbed_recipients(fixture: &Fixture) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for (user, list) in [
        ("bob", &fixture.to),
        ("carol", &fixture.cc),
        ("dave", &fixture.bcc),
    ] {
        if !list.is_empty() {
            out.push((user, format!("{user}@example.test")));
        }
    }
    out
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn fixtures_through_stalwart_arrive_byte_for_byte() {
    runtime().block_on(async {
        let users = ["bob", "carol", "dave"];
        clear(&users).await;
        let tag = run_tag();
        let host = hostname();
        for (n, fixture) in fixtures().iter().enumerate() {
            let id = format!("m6.{tag}.{n}@example.com");
            let bytes = with_id(&fixture.transmitted, &id);
            assert!(!has_bcc_header(&bytes), "{}: no Bcc field", fixture.name);
            assert!(!contains(&bytes, host.as_bytes()), "{}", fixture.name);
            let recipients = testbed_recipients(fixture);
            let to: Vec<String> = recipients.iter().map(|(_, a)| a.clone()).collect();
            let security = if n % 2 == 0 {
                Security::StartTls
            } else {
                Security::Tls
            };
            submit_once(
                &common::submission(security),
                ALICE,
                Timeouts::default(),
                SENDER,
                &to,
                &bytes,
            )
            .await
            .unwrap_or_else(|error| panic!("{}: {error}", fixture.name));
            for (user, _) in &recipients {
                let held = delivered(user, &id, Duration::from_secs(10)).await;
                assert_eq!(held.len(), 1, "{} to {user}", fixture.name);
                let raw = &held[0].raw;
                let prefix = prepended(raw, &bytes)
                    .unwrap_or_else(|| panic!("{} to {user}: not byte for byte", fixture.name));
                assert_eq!(seen(raw), seen(&bytes), "{} to {user}", fixture.name);
                assert!(!has_bcc_header(raw), "{} to {user}", fixture.name);
                assert!(
                    !contains(raw, host.as_bytes()),
                    "{} to {user}",
                    fixture.name
                );
                println!(
                    "{} ({} bytes, {security:?}) to {user}: byte for byte after {:?}",
                    fixture.name,
                    bytes.len(),
                    header_names(prefix)
                );
            }
        }
        clear(&users).await;
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn fixtures_through_mailpit_round_trip() {
    runtime().block_on(async {
        Mailpit::clear().await;
        let tag = run_tag();
        let host = hostname();
        for (n, fixture) in fixtures().iter().enumerate() {
            let id = format!("m6.{tag}.mailpit.{n}@example.com");
            let bytes = with_id(&fixture.transmitted, &id);
            let to: Vec<String> = fixture
                .to
                .iter()
                .chain(&fixture.cc)
                .chain(&fixture.bcc)
                .cloned()
                .collect();
            submit_once(
                &common::mailpit(),
                ANYONE,
                Timeouts::default(),
                &fixture.from,
                &to,
                &bytes,
            )
            .await
            .unwrap_or_else(|error| panic!("{}: {error}", fixture.name));
            let caught = Mailpit::caught(&id, Duration::from_secs(10)).await;
            assert_eq!(caught.len(), 1, "{}", fixture.name);
            let raw = Mailpit::raw(&caught[0]).await;
            let prefix = prepended(&raw, &bytes)
                .unwrap_or_else(|| panic!("{}: not byte for byte", fixture.name));
            assert_eq!(seen(&raw), seen(&bytes), "{}", fixture.name);
            assert!(!has_bcc_header(&bytes), "{}: Sealwax's bytes", fixture.name);
            assert!(!contains(&bytes, host.as_bytes()), "{}", fixture.name);
            println!(
                "{} ({} bytes, {} recipients, {} Bcc): byte for byte after Mailpit's {:?}; \
                 Mailpit's own lines name the Bcc recipients: {}, the host name: {}",
                fixture.name,
                bytes.len(),
                to.len(),
                fixture.bcc.len(),
                header_names(prefix),
                has_bcc_header(prefix),
                contains(prefix, host.as_bytes()),
            );
        }
        Mailpit::clear().await;
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn refusals_on_stalwart() {
    runtime().block_on(async {
        clear(&["bob"]).await;
        let tag = run_tag();
        let endpoint = common::submission(Security::Tls);
        let id = format!("m6.{tag}.refused@example.com");
        let bytes = synthetic(&id, 2_000, "middle", "end");
        let to = vec![
            "bob@example.test".to_owned(),
            "nobody-here@example.test".to_owned(),
        ];
        let error = submit_once(&endpoint, ALICE, Timeouts::default(), SENDER, &to, &bytes)
            .await
            .unwrap_err();
        println!("an unknown local recipient: {error:?}");
        assert_eq!(
            error,
            SubmitError::RecipientsRefused {
                recipients: vec![1]
            }
        );
        assert!(
            delivered("bob", &id, Duration::from_secs(2))
                .await
                .is_empty(),
            "the whole send was refused: bob got nothing"
        );

        let id = format!("m6.{tag}.sender@example.com");
        let bytes = synthetic(&id, 2_000, "middle", "end");
        let other_sender = submit_once(
            &endpoint,
            ALICE,
            Timeouts::default(),
            "carol@example.test",
            &to[..1],
            &bytes,
        )
        .await;
        let arrived = delivered("bob", &id, Duration::from_secs(2)).await.len();
        println!("alice sending as carol: {other_sender:?}, copies delivered: {arrived}");
        assert!(
            matches!(other_sender, Ok(()) | Err(SubmitError::Refused { .. })),
            "{other_sender:?}"
        );

        let wrong = Credentials::Password {
            username: "alice",
            password: "wrong-password",
        };
        pace().await;
        let error = Submitter::connect(&endpoint, wrong, &common::trust())
            .await
            .unwrap_err();
        println!("a wrong password: {error:?}");
        assert_eq!(error, SubmitError::Auth(LoginFailure::WrongPassword));
        assert_eq!(error.failure(), SubmitFailure::Account);
        clear(&["bob"]).await;
    });
}

/// Seeds per fault scenario: `SEALWAX_SEND_SEEDS`, else 5.
fn seeds() -> u64 {
    std::env::var("SEALWAX_SEND_SEEDS")
        .ok()
        .and_then(|seeds| seeds.parse().ok())
        .unwrap_or(5)
}

#[derive(Default)]
struct Tally {
    runs: u64,
    first: Vec<String>,
    delivered_after_cut: u64,
    delivered_at_end: u64,
    duplicates: u64,
    lost: u64,
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn faults_on_stalwart() {
    runtime().block_on(async {
        clear(&["bob"]).await;
        let proxy = Proxy::start(16465, &common::ca()).await;
        let endpoint = tls_endpoint(proxy.port);
        let tag = run_tag();
        let to = vec!["bob@example.test".to_owned()];
        let quick = Timeouts {
            data: Duration::from_secs(3),
            ..Timeouts::default()
        };
        let scenarios = [
            "cut before DATA",
            "cut while the data goes out",
            "end of data sent, reply lost",
            "end of data sent, no reply (timeout)",
        ];
        for scenario in scenarios {
            let mut tally = Tally::default();
            for seed in 0..seeds() {
                let id = format!("m6.{tag}.fault.{}.{seed}@example.com", tally_key(scenario));
                let middle = format!("MIDDLE-{seed}-{}", tally_key(scenario));
                let bytes = synthetic(&id, 64 * 1024 + seed as usize * 997, &middle, "END");
                match scenario {
                    "cut before DATA" => proxy.cut_on_send("DATA\r\n"),
                    "cut while the data goes out" => proxy.cut_on_send(&middle),
                    "end of data sent, reply lost" => proxy.lose_reply("\r\n.\r\n"),
                    _ => proxy.hold_reply("\r\n.\r\n"),
                }
                let first = submit_once(&endpoint, ALICE, quick, SENDER, &to, &bytes).await;
                assert!(!proxy.sent_armed(), "{scenario}: the fault fired");
                let error = first.expect_err(scenario);
                tally.runs += 1;
                tally.first.push(format!("{error:?}"));
                let after_cut = delivered("bob", &id, Duration::from_secs(2)).await.len() as u64;
                tally.delivered_after_cut += after_cut;
                match error.failure() {
                    SubmitFailure::Retry => {
                        submit_once(&endpoint, ALICE, quick, SENDER, &to, &bytes)
                            .await
                            .unwrap_or_else(|error| panic!("{scenario}: the retry: {error}"));
                    }
                    SubmitFailure::Uncertain(_) => {}
                    other => panic!("{scenario}: {other:?}"),
                }
                let at_end = delivered("bob", &id, Duration::from_secs(3)).await.len() as u64;
                tally.delivered_at_end += at_end;
                tally.duplicates += at_end.saturating_sub(1);
                if error.failure() == SubmitFailure::Retry {
                    tally.lost += u64::from(at_end == 0);
                }
            }
            tally.first.sort();
            tally.first.dedup();
            println!(
                "{scenario}: {} runs; first attempt {:?}; delivered right after the fault {}; \
                 at the end {}; duplicates {}; lost {}",
                tally.runs,
                tally.first,
                tally.delivered_after_cut,
                tally.delivered_at_end,
                tally.duplicates,
                tally.lost
            );
            assert_eq!(tally.duplicates, 0, "{scenario}");
            assert_eq!(tally.lost, 0, "{scenario}");
            match scenario {
                "cut before DATA" => {
                    assert_eq!(tally.first, ["Transient"]);
                    assert_eq!(tally.delivered_after_cut, 0);
                    assert_eq!(tally.delivered_at_end, tally.runs);
                }
                "cut while the data goes out" => {
                    assert_eq!(tally.first, ["Uncertain"]);
                    assert_eq!(
                        tally.delivered_at_end, 0,
                        "a partial message isn't delivered"
                    );
                }
                _ => assert_eq!(tally.first, ["Uncertain"]),
            }
        }

        // The control: a blind resend of a message whose reply was lost.
        let id = format!("m6.{tag}.blind@example.com");
        let bytes = synthetic(&id, 8 * 1024, "MIDDLE-blind", "END");
        proxy.lose_reply("\r\n.\r\n");
        let first = submit_once(&endpoint, ALICE, quick, SENDER, &to, &bytes).await;
        submit_once(&endpoint, ALICE, quick, SENDER, &to, &bytes)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
        let copies = delivered("bob", &id, Duration::from_secs(3)).await.len();
        println!(
            "control, a blind resend after a lost reply: first {first:?}; copies in bob's INBOX: \
             {copies}"
        );
        clear(&["bob"]).await;
    });
}

fn tally_key(scenario: &str) -> &'static str {
    match scenario {
        "cut before DATA" => "data",
        "cut while the data goes out" => "content",
        "end of data sent, reply lost" => "lost",
        _ => "hold",
    }
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn submission_speed() {
    runtime().block_on(async {
        clear(&["bob"]).await;
        Mailpit::clear().await;
        let tag = run_tag();
        for (name, endpoint, credentials, recipient) in [
            (
                "stalwart 587 STARTTLS",
                common::submission(Security::StartTls),
                ALICE,
                "bob@example.test",
            ),
            (
                "stalwart 465 TLS",
                common::submission(Security::Tls),
                ALICE,
                "bob@example.test",
            ),
            (
                "mailpit 1025 STARTTLS",
                common::mailpit(),
                ANYONE,
                "someone@example.org",
            ),
        ] {
            let to = vec![recipient.to_owned()];
            for (size, runs) in [(1_024, 20), (10 * 1024 * 1024, 5)] {
                let mut whole = Vec::new();
                let mut data = Vec::new();
                for run in 0..runs {
                    let id = format!("m6.{tag}.speed.{size}.{run}.{}@example.com", name.len());
                    let bytes = synthetic(&id, size, "MIDDLE", "END");
                    pace().await;
                    let started = Instant::now();
                    let mut submitter =
                        Submitter::connect(&endpoint, credentials, &common::trust())
                            .await
                            .unwrap();
                    let envelope_started = Instant::now();
                    submitter
                        .prepare(
                            Envelope {
                                from: SENDER,
                                to: &to,
                            },
                            &bytes,
                        )
                        .await
                        .unwrap()
                        .send()
                        .await
                        .unwrap();
                    data.push(ms(envelope_started.elapsed()));
                    submitter.quit().await;
                    whole.push(ms(started.elapsed()));
                }
                let (whole50, whole95) = percentiles(&mut whole);
                let (data50, data95) = percentiles(&mut data);
                println!(
                    "{name}, {size} bytes, {runs} runs: connect to QUIT p50 {whole50:.1} ms, \
                     p95 {whole95:.1} ms; envelope and data p50 {data50:.1} ms, p95 {data95:.1} ms"
                );
            }
            if name.starts_with("stalwart") {
                clear(&["bob"]).await;
            }
        }
        Mailpit::clear().await;
    });
}
