//! M6's exit tests for sending through the engine (ML-076, ML-077, ML-074; ADR-015, ADR-007's
//! M6 interop amendment), on the Docker testbed:
//!
//! - **At most once:** a store and engine sending as `outbox` to Stalwart's submissions port
//!   through the fault proxy, with the connection cut, or the engine stopped hard and a new one
//!   started on the same store and blobs, at each step (before MAIL, after RCPT, after the
//!   store's start before DATA, during the data, after the end of data before the reply), over
//!   several seeds. The proxy records what the engine sent, so every transmission of a message's
//!   data is counted by its Message-ID line (Stalwart itself drops a repeated delivery of a
//!   Message-ID); deliveries are read from bob's INBOX. Queued sends go exactly once; uncertain
//!   ones are never sent again until the user resolves them, then exactly once more for "send
//!   again" and never for "it was sent". Undo inside the window sends nothing on the wire.
//! - **Interop:** drafts built from S7's cases sent through the engine to Stalwart (bob, carol,
//!   dave) and to Mailpit; each received copy round-trips through mail-parser to the draft's
//!   fields; Sealwax's transmitted bytes (its Sent copy as appended, without its one Bcc field,
//!   checked to be exactly what each server received after its own lines) hold no Bcc field and
//!   no host name; each message arrives once and its Sent copy is in Sent once.
//! - **The Sent copy and drafts:** on Dovecot, IMAP4rev1 Dovecot and Stalwart, a reply's draft
//!   uploaded to Utkast with the upload's reply lost, then sent with the Sent copy's APPEND
//!   reply lost or cut part of the way into the literal: one Sent copy, the source `\Answered`,
//!   the draft's server copies gone, nothing duplicated.
//!
//! Test users and dummy addresses only; everything on 127.0.0.1.
//!
//! SEALWAX_TESTBED_CA=…/tools/testbed/certs/ca.pem \
//!   cargo test -p sealwax-engine --release --test outbox_testbed -- --ignored --test-threads=1 --nocapture

mod common;
mod outgoing;
mod proxy;
mod snapshot;
mod testbed_client;
mod testbed_engine;

use common::{DOVECOT, DOVECOT_REV1, PASSWORD, STALWART, Server};
use outgoing::{Mailpit, contains, has_bcc_header, hostname, prepended};
use proxy::Proxy;
use sealwax_domain::account::{Endpoint, Security};
use sealwax_domain::compose::{Draft, DraftPart, DraftSource, ReplyKind, ids};
use sealwax_domain::model::{AccountId, Address, DraftId, MessageId};
use sealwax_engine::api::{Command, Event, Password, SendError, SyncStatus};
use sealwax_engine::service::SyncConfig;
use sealwax_engine::smtp::submit::Timeouts;
use sealwax_infrastructure::store::{
    DraftSaved, Resolution, SaveDraft, SendQueued, SendState, SendUndone,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use testbed_client::Client;
use testbed_engine::{TestEngine, until};

const WAIT: Duration = Duration::from_secs(45);

fn runtime() -> tokio::runtime::Runtime {
    sealwax_engine::runtime().unwrap()
}

fn config() -> SyncConfig {
    SyncConfig {
        account_pass: Duration::from_secs(5),
        backoff_min: Duration::from_millis(300),
        backoff_max: Duration::from_secs(2),
        logout_wait: Duration::ZERO,
        submission: Timeouts {
            connect: Duration::from_secs(10),
            command: Duration::from_secs(10),
            data: Duration::from_secs(5),
        },
        ..SyncConfig::default()
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Sixteen bytes no other call in this process gets.
fn random() -> [u8; 16] {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let count = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&nanos.to_le_bytes());
    out[8..].copy_from_slice(&count.wrapping_mul(0x9e37_79b9_7f4a_7c15).to_le_bytes());
    out
}

fn message_id(sender: &str) -> String {
    ids::message_id(sender, random()).unwrap()
}

fn tls(port: u16) -> Endpoint {
    common::endpoint(port, Security::Tls)
}

/// The Sent and Drafts folders the account's sync finds by role: Stalwart's own special-use
/// folders, the seed's Norwegian ones on both Dovecots.
fn roles(server: Server) -> [&'static str; 2] {
    if server == STALWART {
        ["Sent Items", "Drafts"]
    } else {
        ["Sendt", "Utkast"]
    }
}

async fn mailbox(server: Server, user: &str) -> Client {
    let mut client = Client::connect(server.tls, &common::ca(), user)
        .await
        .unwrap_or_else(|error| panic!("{user} on {}: {error:?}", server.name));
    for folder in roles(server) {
        client.create(folder).await.unwrap();
    }
    client
}

async fn clear(server: Server, user: &str, folders: &[&str]) {
    let mut client = mailbox(server, user).await;
    for folder in folders {
        client.empty(folder).await.unwrap();
    }
    client.logout().await;
}

/// Copies of `id` in `folder` of `user`, after waiting up to `wait` for one.
async fn copies(
    server: Server,
    user: &str,
    folder: &str,
    id: &str,
    wait: Duration,
) -> Vec<outgoing::Held> {
    let mut client = mailbox(server, user).await;
    let found = outgoing::arrived(&mut client, folder, id, wait).await;
    client.logout().await;
    found
}

fn live(held: &[outgoing::Held]) -> Vec<&outgoing::Held> {
    held.iter()
        .filter(|held| !held.flags.iter().any(|flag| flag == "\\Deleted"))
        .collect()
}

/// Waits until the account's first sync settled.
async fn settled(engine: &mut TestEngine, account: AccountId) {
    let status = engine.settled(account, WAIT).await;
    assert_eq!(status, Some(SyncStatus::Synced), "the account synced");
}

/// The account's password again, after a restart (secrets were in memory).
async fn update_password(engine: &mut TestEngine, request: u64) {
    let key = engine.accounts()[0].key.clone();
    engine.send(Command::UpdatePassword {
        request,
        key,
        password: Password::new(PASSWORD.into()),
    });
    let updated = engine
        .event(WAIT, |event| match event {
            Event::PasswordUpdated {
                request: id,
                result,
            } if *id == request => Some(result.is_ok()),
            _ => None,
        })
        .await;
    assert_eq!(updated, Some(true), "the password was taken again");
}

/// Saves `draft` (its window open, so no Drafts-folder copy) and queues it from `not_before`.
async fn queue(engine: &TestEngine, draft: Draft, not_before: i64) -> DraftId {
    let saved = engine
        .store
        .save_draft(SaveDraft {
            id: None,
            draft,
            open: true,
            now: now_ms(),
        })
        .recv()
        .await
        .unwrap();
    let DraftSaved::Saved { id, .. } = saved else {
        panic!("not saved: {saved:?}");
    };
    match engine
        .store
        .queue_send(id, not_before)
        .recv()
        .await
        .unwrap()
    {
        SendQueued::Queued { account, .. } => engine.send(Command::OutboxQueued { account }),
        other => panic!("not queued: {other:?}"),
    }
    id
}

/// How a send ended, as the engine said.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Outcome {
    Sent,
    Failed(SendError),
    Uncertain,
}

async fn outcome(engine: &mut TestEngine, draft: DraftId, within: Duration) -> Option<Outcome> {
    engine
        .event(within, |event| match event {
            Event::Sent { draft: id, .. } if *id == draft => Some(Outcome::Sent),
            Event::SendUncertain { draft: id, .. } if *id == draft => Some(Outcome::Uncertain),
            Event::SendFailed {
                draft: id, error, ..
            } if *id == draft => Some(Outcome::Failed(error.clone())),
            _ => None,
        })
        .await
}

/// The draft's place in the outbox, `None` once it left.
async fn state(engine: &TestEngine, draft: DraftId) -> Option<SendState> {
    engine
        .store
        .outbox(None)
        .recv()
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.draft == draft)
        .map(|entry| entry.state)
}

fn address(name: &str, email: &str) -> Address {
    Address::new(name, email)
}

/// About 30 KB of short HTML lines with `middle` on a line of its own.
fn body(middle: &str) -> String {
    let mut html = String::new();
    for n in 0..300 {
        if n == 150 {
            html.push_str(&format!("<p>{middle}</p>\n"));
        }
        html.push_str("<p>alpha bravo charlie delta echo foxtrot golf hotel india</p>\n");
    }
    html
}

// --- At most once ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    BeforeMail,
    AfterRcpt,
    AfterStart,
    DuringData,
    AfterDot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    /// The connection cut: the same engine goes on.
    Cut,
    /// The connection held at the step and the engine stopped hard (no LOGOUT waited for); a
    /// new engine starts on the same store and blobs.
    Stop,
}

const STEPS: [Step; 5] = [
    Step::BeforeMail,
    Step::AfterRcpt,
    Step::AfterStart,
    Step::DuringData,
    Step::AfterDot,
];

fn arm(proxy: &Proxy, step: Step, fault: Fault, middle: &str) {
    match (step, fault) {
        (Step::BeforeMail, Fault::Cut) => proxy.cut_on_send("MAIL FROM"),
        (Step::BeforeMail, Fault::Stop) => proxy.hold_reply("AUTH PLAIN"),
        (Step::AfterRcpt, Fault::Cut) => proxy.lose_reply("RCPT TO"),
        (Step::AfterRcpt, Fault::Stop) => proxy.hold_reply("RCPT TO"),
        (Step::AfterStart, Fault::Cut) => proxy.lose_reply("DATA\r\n"),
        (Step::AfterStart, Fault::Stop) => proxy.hold_reply("DATA\r\n"),
        (Step::DuringData, Fault::Cut) => proxy.cut_on_send(middle),
        (Step::DuringData, Fault::Stop) => proxy.freeze_on_send(middle),
        (Step::AfterDot, Fault::Cut) => proxy.lose_reply("\r\n.\r\n"),
        (Step::AfterDot, Fault::Stop) => proxy.hold_reply("\r\n.\r\n"),
    }
}

/// Whether the send can't have reached the server's delivery: it stays queued (a stop before
/// the store's start) or was found not delivered (a cut before any data went out).
fn retried(step: Step, fault: Fault) -> bool {
    match fault {
        Fault::Cut => matches!(step, Step::BeforeMail | Step::AfterRcpt | Step::AfterStart),
        Fault::Stop => matches!(step, Step::BeforeMail | Step::AfterRcpt),
    }
}

/// Transmissions of the message's data the engine sent through the proxy: its Message-ID line.
fn transmissions(proxy: &Proxy, id: &str) -> usize {
    let line = format!("Message-ID: <{id}>");
    proxy
        .sent_raw_lines()
        .iter()
        .filter(|sent| sent.eq_ignore_ascii_case(&line))
        .count()
}

/// The commands the engine sent through the proxy: lines starting with `verb`.
fn commands(proxy: &Proxy, verb: &str) -> usize {
    proxy
        .sent_raw_lines()
        .iter()
        .filter(|sent| sent.starts_with(verb))
        .count()
}

#[derive(Default)]
struct Tally {
    runs: usize,
    retried_once: usize,
    uncertain: usize,
    resent_before_resolved: usize,
    send_again: usize,
    send_again_once: usize,
    was_sent: usize,
    was_sent_none: usize,
    delivered: usize,
    failures: Vec<String>,
}

/// Seeds per step and fault: `SEALWAX_OUTBOX_SEEDS`, else 2.
fn seeds() -> u64 {
    std::env::var("SEALWAX_OUTBOX_SEEDS")
        .ok()
        .and_then(|seeds| seeds.parse().ok())
        .unwrap_or(2)
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn at_most_once_through_cuts_and_stops() {
    runtime().block_on(async {
        clear(STALWART, "bob", &["INBOX"]).await;
        clear(STALWART, "outbox", &roles(STALWART)).await;
        let proxy = Proxy::start(16465, &common::ca()).await;
        proxy.record();
        let mut engine = TestEngine::start(config());
        let account = engine
            .add_with(1, "outbox", tls(STALWART.tls), tls(proxy.port))
            .await;
        settled(&mut engine, account).await;
        let mut request = 10;
        let mut tallies = Vec::new();
        for step in STEPS {
            for fault in [Fault::Cut, Fault::Stop] {
                let mut tally = Tally::default();
                for seed in 0..seeds() {
                    let id = message_id("outbox@example.test");
                    let middle = format!("MIDDLEMARK{step:?}{fault:?}{seed}");
                    let draft = Draft {
                        account,
                        from: address("Outbox", "outbox@example.test"),
                        to: vec![address("Bob", "bob@example.test")],
                        subject: format!("At most once {step:?} {fault:?} {seed}"),
                        html: body(&middle),
                        message_id: id.clone(),
                        ..Draft::default()
                    };
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    arm(&proxy, step, fault, &middle);
                    let draft = queue(&engine, draft, now_ms()).await;
                    let fired = until(WAIT, || async { !proxy.sent_armed() }).await;
                    assert!(fired.is_some(), "{step:?} {fault:?}: the rule fired");
                    if fault == Fault::Stop {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        engine = engine.restart(config());
                        request += 1;
                        update_password(&mut engine, request).await;
                    }
                    tally.runs += 1;
                    let label = format!("{step:?} {fault:?} seed {seed}");
                    if retried(step, fault) {
                        let ended = outcome(&mut engine, draft, WAIT).await;
                        if ended != Some(Outcome::Sent) {
                            tally.failures.push(format!("{label}: {ended:?}"));
                        }
                        let sent = transmissions(&proxy, &id);
                        tally.retried_once += usize::from(sent == 1);
                        if sent != 1 {
                            tally
                                .failures
                                .push(format!("{label}: {sent} transmissions"));
                        }
                    } else {
                        let uncertain = until(WAIT, || async {
                            state(&engine, draft).await == Some(SendState::Uncertain)
                        })
                        .await;
                        if uncertain.is_none() {
                            tally.failures.push(format!(
                                "{label}: not uncertain but {:?}",
                                state(&engine, draft).await
                            ));
                            continue;
                        }
                        tally.uncertain += 1;
                        let before = transmissions(&proxy, &id);
                        // Nothing goes again by itself: wake the outbox and wait.
                        engine.send(Command::OutboxQueued { account });
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        let unresolved = transmissions(&proxy, &id);
                        if unresolved != before
                            || state(&engine, draft).await != Some(SendState::Uncertain)
                        {
                            tally.resent_before_resolved += 1;
                            tally
                                .failures
                                .push(format!("{label}: resent before resolved"));
                        }
                        // The uncertain send's own events are read; only what follows counts.
                        engine.drain();
                        if seed % 2 == 0 {
                            tally.send_again += 1;
                            let resolution = Resolution::SendAgain {
                                not_before: now_ms(),
                            };
                            engine
                                .store
                                .resolve_uncertain(draft, resolution, now_ms())
                                .recv()
                                .await
                                .unwrap();
                            engine.send(Command::OutboxQueued { account });
                            let ended = outcome(&mut engine, draft, WAIT).await;
                            let after = transmissions(&proxy, &id);
                            if ended == Some(Outcome::Sent) && after == before + 1 {
                                tally.send_again_once += 1;
                            } else {
                                tally.failures.push(format!(
                                    "{label}: send again {ended:?}, {before} → {after}"
                                ));
                            }
                        } else {
                            tally.was_sent += 1;
                            engine
                                .store
                                .resolve_uncertain(draft, Resolution::WasSent, now_ms())
                                .recv()
                                .await
                                .unwrap();
                            engine.send(Command::OutboxQueued { account });
                            tokio::time::sleep(Duration::from_secs(3)).await;
                            let after = transmissions(&proxy, &id);
                            if after == before {
                                tally.was_sent_none += 1;
                            } else {
                                tally
                                    .failures
                                    .push(format!("{label}: it was sent, yet {before} → {after}"));
                            }
                        }
                    }
                    let delivered = copies(STALWART, "bob", "INBOX", &id, Duration::from_secs(3))
                        .await
                        .len();
                    tally.delivered += delivered;
                    println!(
                        "{label}: {} transmissions, {delivered} in bob's INBOX",
                        transmissions(&proxy, &id)
                    );
                }
                println!(
                    "{step:?} {fault:?}: {} runs; queued and sent exactly once {}; uncertain {}; \
                     resent before resolved {}; send again {} (exactly once more {}); it was sent \
                     {} (nothing more {}); delivered {}",
                    tally.runs,
                    tally.retried_once,
                    tally.uncertain,
                    tally.resent_before_resolved,
                    tally.send_again,
                    tally.send_again_once,
                    tally.was_sent,
                    tally.was_sent_none,
                    tally.delivered
                );
                tallies.push((step, fault, tally));
            }
        }

        // Undo inside the window: nothing on the wire.
        let mails = commands(&proxy, "MAIL FROM");
        let id = message_id("outbox@example.test");
        let draft = Draft {
            account,
            from: address("Outbox", "outbox@example.test"),
            to: vec![address("Bob", "bob@example.test")],
            subject: "Undone".into(),
            html: body("UNDONE"),
            message_id: id.clone(),
            ..Draft::default()
        };
        let draft = queue(&engine, draft, now_ms() + 2_000).await;
        let undone = engine.store.undo_send(draft).recv().await.unwrap();
        assert!(matches!(undone, SendUndone::Undone { .. }), "{undone:?}");
        engine.send(Command::OutboxQueued { account });
        tokio::time::sleep(Duration::from_secs(4)).await;
        let after = commands(&proxy, "MAIL FROM");
        println!(
            "undo inside the window: MAIL FROM sent {} times, the message's data {} times",
            after - mails,
            transmissions(&proxy, &id)
        );
        assert_eq!(after, mails, "nothing sent after an undo");
        assert_eq!(transmissions(&proxy, &id), 0);

        drop(engine);
        clear(STALWART, "bob", &["INBOX"]).await;
        clear(STALWART, "outbox", &roles(STALWART)).await;
        for (step, fault, tally) in &tallies {
            assert!(
                tally.failures.is_empty(),
                "{step:?} {fault:?}: {:?}",
                tally.failures
            );
        }
    });
}

// --- Interop --------------------------------------------------------------------------------

/// A tiny PNG (1×1, one pixel).
const PNG: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I', b'H', b'D', b'R', 0, 0, 0,
    1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89, 0, 0, 0, 0x0d, b'I', b'D', b'A', b'T',
    0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0, 5, 0, 1, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0, 0,
    0, 0, b'I', b'E', b'N', b'D', 0xae, b'B', 0x60, 0x82,
];
const PDF: &[u8] = b"%PDF-1.4\n1 0 obj <<>> endobj\ntrailer <<>>\n%%EOF\n";
const TEXT: &str = "Blåbær, tyttebær og molte 🫐\nLinje to — æøå ÆØÅ.\n";

/// One of S7's cases as a draft from `from` to the given recipients, with its parts' bytes.
struct Case {
    name: &'static str,
    draft: Draft,
    parts: Vec<Vec<u8>>,
    /// A phrase the text and HTML bodies both carry.
    phrase: &'static str,
}

/// The recipients a case writes to: (To, Cc, Bcc), by role.
struct People {
    to: Vec<Address>,
    cc: Vec<Address>,
    bcc: Vec<Address>,
}

fn cases(engine: &TestEngine, account: AccountId, from: &Address, people: &People) -> Vec<Case> {
    let sender = from.email.as_str();
    let draft = |subject: &str, html: String| Draft {
        account,
        from: from.clone(),
        to: people.to.clone(),
        cc: people.cc.clone(),
        bcc: people.bcc.clone(),
        subject: subject.into(),
        html,
        message_id: message_id(sender),
        ..Draft::default()
    };
    let mut out = Vec::new();
    out.push(Case {
        name: "norwegian and emoji",
        draft: draft(
            "Blåbærsyltetøy 🫐 på brødskiva — møte i morgen?",
            "<p>Hei Ola,</p><p>Dette er <b>fet</b> tekst med æøå og 🫐. Vi ses på hytta.</p>"
                .into(),
        ),
        parts: Vec::new(),
        phrase: "Vi ses på hytta",
    });
    let url = format!(
        "https://example.com/{}?q={}",
        "very/long/path/segment".repeat(12),
        "x".repeat(120)
    );
    out.push(Case {
        name: "a long URL",
        draft: draft(
            "En lang lenke",
            format!("<p>Se her: <a href=\"{url}\">{url}</a></p><p>Lenken er lang.</p>"),
        ),
        parts: Vec::new(),
        phrase: "Lenken er lang",
    });
    let mut bcc_only = draft(
        "Kun blindkopi",
        "<p>Only Bcc recipients get this one.</p>".into(),
    );
    bcc_only.to.clear();
    bcc_only.cc.clear();
    bcc_only.bcc = people.bcc.clone();
    out.push(Case {
        name: "Bcc only",
        draft: bcc_only,
        parts: Vec::new(),
        phrase: "Only Bcc recipients",
    });
    let content_id = ids::content_id(sender, random()).unwrap();
    let mut parts = Vec::new();
    let mut draft_parts = Vec::new();
    for (name, mime_type, bytes, cid) in [
        (
            "blåbær.png",
            "image/png",
            PNG.to_vec(),
            Some(content_id.clone()),
        ),
        ("rapport.pdf", "application/pdf", PDF.to_vec(), None),
        (
            "notat æøå.txt",
            "text/plain",
            TEXT.as_bytes().to_vec(),
            None,
        ),
    ] {
        let blob = engine.blobs.put(&bytes).unwrap();
        draft_parts.push(DraftPart {
            name: name.into(),
            mime_type: mime_type.into(),
            size: bytes.len() as u64,
            blob,
            content_id: cid,
        });
        parts.push(bytes);
    }
    let mut attached = draft(
        "Vedlegg: bilde, PDF og tekst",
        format!("<p>Bildet under, og to vedlegg.</p><p><img src=\"cid:{content_id}\"></p>"),
    );
    attached.parts = draft_parts;
    out.push(Case {
        name: "an inline image, a PDF and a UTF-8 text",
        draft: attached,
        parts,
        phrase: "og to vedlegg",
    });
    let parent = format!("parent.{}@example.org", ids::token(random()));
    let mut reply = draft(
        "Re: Blåbær",
        "<p>Takk, det passer fint.</p>\
         <blockquote type=\"cite\"><p>Kan vi møtes i morgen?</p></blockquote>"
            .into(),
    );
    reply.in_reply_to = Some(parent.clone());
    reply.references = vec![parent];
    out.push(Case {
        name: "a reply with a quote",
        draft: reply,
        parts: Vec::new(),
        phrase: "det passer fint",
    });
    out
}

fn emails(list: &[Address]) -> Vec<String> {
    list.iter().map(|address| address.email.clone()).collect()
}

/// What mail-parser reads from `raw` matches the draft's fields: subject, sender, recipients,
/// Message-ID, threading, both bodies and every part's name and bytes; and no Bcc field unless
/// the server wrote its own (`server_bcc`: Mailpit lists the envelope's other recipients).
fn round_trips(case: &Case, raw: &[u8], server_bcc: bool) -> Result<(), String> {
    use mail_parser::{MessageParser, MimeHeaders};
    let message = MessageParser::default().parse(raw).ok_or("not parsed")?;
    let draft = &case.draft;
    let addresses = |list: Option<&mail_parser::Address<'_>>| -> Vec<String> {
        list.map(|list| {
            list.iter()
                .filter_map(|addr| addr.address().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
    };
    let mut wrong = Vec::new();
    if message.subject() != Some(draft.subject.as_str()) {
        wrong.push("subject");
    }
    let from = message.from().and_then(|from| from.first());
    if from.and_then(|from| from.address()) != Some(draft.from.email.as_str())
        || from.and_then(|from| from.name()) != Some(draft.from.name.as_str())
    {
        wrong.push("from");
    }
    if addresses(message.to()) != emails(&draft.to) {
        wrong.push("to");
    }
    if addresses(message.cc()) != emails(&draft.cc) {
        wrong.push("cc");
    }
    if message.bcc().is_some() && !server_bcc {
        wrong.push("a Bcc field");
    }
    if message.message_id() != Some(draft.message_id.as_str()) {
        wrong.push("message-id");
    }
    let in_reply_to = message.in_reply_to().as_text().map(str::to_owned);
    if in_reply_to != draft.in_reply_to {
        wrong.push("in-reply-to");
    }
    let words = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = words(&message.body_text(0).unwrap_or_default());
    let html = words(&message.body_html(0).unwrap_or_default());
    if !text.contains(case.phrase) || !html.contains(case.phrase) {
        wrong.push("bodies");
    }
    for (part, bytes) in draft.parts.iter().zip(&case.parts) {
        let found = message.attachments().any(|attachment| {
            attachment.attachment_name() == Some(part.name.as_str())
                && attachment.contents() == bytes.as_slice()
        });
        if !found {
            wrong.push("a part");
        }
    }
    if wrong.is_empty() {
        Ok(())
    } else {
        Err(wrong.join(", "))
    }
}

/// The Sent copy without its one Bcc field: the transmitted bytes (ADR-007's S7 amendment).
fn without_bcc(sent_copy: &[u8]) -> Vec<u8> {
    let header_end = outgoing::find(sent_copy, b"\r\n\r\n").unwrap_or(sent_copy.len());
    let start = if sent_copy.starts_with(b"Bcc: ") {
        Some(0)
    } else {
        outgoing::find(&sent_copy[..header_end], b"\r\nBcc: ").map(|at| at + 2)
    };
    let Some(start) = start else {
        return sent_copy.to_vec();
    };
    let mut end = start;
    loop {
        let line_end = end + outgoing::find(&sent_copy[end..], b"\r\n").unwrap() + 2;
        end = line_end;
        if !matches!(sent_copy.get(end), Some(b' ' | b'\t')) {
            break;
        }
    }
    let mut out = sent_copy[..start].to_vec();
    out.extend_from_slice(&sent_copy[end..]);
    out
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn interop_through_the_engine() {
    runtime().block_on(async {
        let host = hostname();
        for user in ["bob", "carol", "dave"] {
            clear(STALWART, user, &["INBOX"]).await;
        }
        clear(STALWART, "outbox", &roles(STALWART)).await;
        clear(DOVECOT, "dave", &roles(DOVECOT)).await;
        Mailpit::clear().await;
        let mut engine = TestEngine::start(config());
        // Stalwart for both sides; Dovecot with Mailpit, whose catch is counted through its API.
        let stalwart = engine
            .add_with(
                1,
                "outbox",
                tls(STALWART.tls),
                common::submission(Security::Tls),
            )
            .await;
        settled(&mut engine, stalwart).await;
        let mailpit = engine
            .add_with(2, "dave", tls(DOVECOT.tls), common::mailpit())
            .await;
        settled(&mut engine, mailpit).await;
        let to_stalwart = People {
            to: vec![address("Ola Nordmann", "bob@example.test")],
            cc: vec![address("Øystein Åsen", "carol@example.test")],
            bcc: vec![address("Blind Kopi", "dave@example.test")],
        };
        let to_mailpit = People {
            to: vec![
                address("Ola Nordmann", "ola@example.org"),
                address("Bjørn Ærø 🙂", "bjorn@example.net"),
            ],
            cc: vec![address("Øystein Åsen", "oystein@example.org")],
            bcc: vec![address("Blind Kopi", "blind@example.net")],
        };
        let sides = [
            (
                "stalwart",
                stalwart,
                address("Kåre Ærø 🫐", "outbox@example.test"),
                to_stalwart,
                STALWART,
                "outbox",
            ),
            (
                "mailpit",
                mailpit,
                address("Kari Nordmann", "dave@example.test"),
                to_mailpit,
                DOVECOT,
                "dave",
            ),
        ];
        let mut failures = Vec::new();
        for (side, account, from, people, imap, user) in &sides {
            for case in cases(&engine, *account, from, people) {
                let id = case.draft.message_id.clone();
                tokio::time::sleep(Duration::from_millis(250)).await;
                let draft = queue(&engine, case.draft.clone(), now_ms()).await;
                let ended = outcome(&mut engine, draft, WAIT).await;
                assert_eq!(ended, Some(Outcome::Sent), "{side} {}", case.name);
                let sent = copies(*imap, user, roles(*imap)[0], &id, WAIT).await;
                let transmitted = match sent.as_slice() {
                    [copy] => without_bcc(&copy.raw),
                    other => {
                        failures.push(format!("{side} {}: {} Sent copies", case.name, other.len()));
                        continue;
                    }
                };
                let mut scanned = vec![("Sealwax's bytes", transmitted.clone())];
                let received: Vec<(String, Vec<u8>)> = if *side == "stalwart" {
                    let mut out = Vec::new();
                    for (user, list) in [
                        ("bob", &case.draft.to),
                        ("carol", &case.draft.cc),
                        ("dave", &case.draft.bcc),
                    ] {
                        if list.is_empty() {
                            continue;
                        }
                        let held = copies(STALWART, user, "INBOX", &id, WAIT).await;
                        if held.len() != 1 {
                            failures.push(format!(
                                "{side} {} to {user}: {} copies",
                                case.name,
                                held.len()
                            ));
                        }
                        out.extend(held.into_iter().map(|held| (user.to_string(), held.raw)));
                    }
                    out
                } else {
                    let caught = Mailpit::caught(&id, WAIT).await;
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    let caught = if caught.is_empty() {
                        caught
                    } else {
                        Mailpit::ids(&id).await
                    };
                    if caught.len() != 1 {
                        failures.push(format!(
                            "{side} {}: Mailpit caught {}",
                            case.name,
                            caught.len()
                        ));
                    }
                    let mut out = Vec::new();
                    for id in caught {
                        out.push(("mailpit".to_owned(), Mailpit::raw(&id).await));
                    }
                    out
                };
                for (who, raw) in &received {
                    // Mailpit's own lines name the Bcc recipients and its reverse lookup of the
                    // client, so only Stalwart's whole copy is scanned besides Sealwax's bytes.
                    if prepended(raw, &transmitted).is_none() {
                        failures.push(format!(
                            "{side} {} to {who}: not Sealwax's bytes after the server's lines",
                            case.name
                        ));
                    } else if *side == "stalwart" {
                        scanned.push(("Stalwart's copy", raw.clone()));
                    }
                    if let Err(wrong) = round_trips(&case, raw, *side == "mailpit") {
                        failures.push(format!("{side} {} to {who}: {wrong}", case.name));
                    }
                }
                for (what, bytes) in &scanned {
                    if has_bcc_header(bytes) {
                        failures.push(format!("{side} {}: a Bcc field in {what}", case.name));
                    }
                    if contains(bytes, host.as_bytes()) {
                        failures.push(format!("{side} {}: the host name in {what}", case.name));
                    }
                }
                println!(
                    "{side}, {}: sent; {} received copies round-trip; {} Sent copy; scanned {:?} \
                     ({} bytes transmitted)",
                    case.name,
                    received.len(),
                    sent.len(),
                    scanned.iter().map(|(what, _)| *what).collect::<Vec<_>>(),
                    transmitted.len()
                );
            }
        }
        drop(engine);
        for user in ["bob", "carol", "dave"] {
            clear(STALWART, user, &["INBOX"]).await;
        }
        clear(STALWART, "outbox", &roles(STALWART)).await;
        clear(DOVECOT, "dave", &roles(DOVECOT)).await;
        Mailpit::clear().await;
        assert!(failures.is_empty(), "{failures:#?}");
    });
}

// --- The Sent copy and drafts ---------------------------------------------------------------

async fn sent_copy_and_drafts(server: Server) {
    let user = "outboxfault";
    clear(server, user, &["INBOX", roles(server)[0], roles(server)[1]]).await;
    clear(STALWART, "bob", &["INBOX"]).await;
    Mailpit::clear().await;
    let smtp = if server == STALWART {
        common::submission(Security::Tls)
    } else {
        common::mailpit()
    };
    let proxy = Proxy::start(server.tls, &common::ca()).await;
    let mut test = mailbox(server, user).await;
    let source_id = format!("source.{}@example.org", ids::token(random()));
    let source = format!(
        "From: \"Ola Nordmann\" <bob@example.test>\r\nTo: <{user}@example.test>\r\n\
         Subject: Kan vi møtes?\r\nDate: Tue, 6 Oct 2026 12:00:00 +0000\r\n\
         Message-ID: <{source_id}>\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\nKan vi møtes i morgen?\r\n"
    );
    test.append("INBOX", "", source.as_bytes()).await.unwrap();
    let mut engine = TestEngine::start(config());
    let account = engine.add_with(1, user, tls(proxy.port), smtp).await;
    settled(&mut engine, account).await;
    let inbox = engine.folder(account, "INBOX").await.unwrap();
    let stored: Vec<MessageId> = engine
        .store
        .folder_messages(inbox)
        .recv()
        .await
        .unwrap()
        .into_iter()
        .map(|message| message.id)
        .collect();
    assert_eq!(stored.len(), 1, "{}: the source is stored", server.name);
    let sender = format!("{user}@example.test");
    let id = message_id(&sender);
    let middle = format!("MIDDLEMARK{}", server.name.replace('-', ""));
    let draft = Draft {
        account,
        from: address("Outbox Fault", &sender),
        to: vec![address("Ola Nordmann", "bob@example.test")],
        subject: "Re: Kan vi møtes?".into(),
        html: body(&middle),
        message_id: id.clone(),
        in_reply_to: Some(source_id.clone()),
        references: vec![source_id.clone()],
        source: Some(DraftSource {
            message: stored[0],
            kind: ReplyKind::Reply,
        }),
        ..Draft::default()
    };
    // The draft's copy in Utkast, its APPEND's reply lost: the retry leaves one live copy.
    let saved = engine
        .store
        .save_draft(SaveDraft {
            id: None,
            draft,
            open: true,
            now: now_ms(),
        })
        .recv()
        .await
        .unwrap();
    let DraftSaved::Saved { id: draft_id, .. } = saved else {
        panic!("{saved:?}");
    };
    proxy.lose_reply(&middle);
    engine
        .store
        .set_draft_open(draft_id, false)
        .recv()
        .await
        .unwrap();
    engine.send(Command::DraftsChanged { account });
    let fired = until(WAIT, || async { !proxy.sent_armed() }).await;
    assert!(fired.is_some(), "{}: the upload's rule fired", server.name);
    // Recorded: nothing due for the draft however late it is asked.
    let uploaded = until(WAIT, || async {
        let due = engine
            .store
            .due_drafts(account, now_ms() + 600_000)
            .recv()
            .await
            .unwrap();
        due.uploads.iter().all(|upload| upload.draft != draft_id) && due.deletions.is_empty()
    })
    .await;
    assert!(
        uploaded.is_some(),
        "{}: the upload was recorded",
        server.name
    );
    let drafts = copies(server, user, roles(server)[1], &id, Duration::ZERO).await;
    println!(
        "{}: the draft's upload with its reply lost: {} copies in the Drafts folder, {} live",
        server.name,
        drafts.len(),
        live(&drafts).len()
    );
    assert_eq!(
        live(&drafts).len(),
        1,
        "{}: one live draft copy",
        server.name
    );

    // The send, with the Sent copy's APPEND reply lost.
    proxy.lose_reply(&middle);
    match engine
        .store
        .queue_send(draft_id, now_ms())
        .recv()
        .await
        .unwrap()
    {
        SendQueued::Queued { .. } => engine.send(Command::OutboxQueued { account }),
        other => panic!("{other:?}"),
    }
    let ended = outcome(&mut engine, draft_id, WAIT).await;
    assert_eq!(ended, Some(Outcome::Sent), "{}", server.name);
    let fired = until(WAIT, || async { !proxy.sent_armed() }).await;
    assert!(
        fired.is_some(),
        "{}: the Sent copy's rule fired",
        server.name
    );
    let done = until(WAIT, || async { state(&engine, draft_id).await.is_none() }).await;
    assert!(
        done.is_some(),
        "{}: the Sent copy was recorded",
        server.name
    );
    let sent = copies(server, user, roles(server)[0], &id, Duration::from_secs(5)).await;
    let answered = until(WAIT, || async {
        let held = copies(server, user, "INBOX", &source_id, Duration::ZERO).await;
        held.len() == 1 && held[0].flags.iter().any(|flag| flag == "\\Answered")
    })
    .await;
    let gone = until(WAIT, || async {
        live(&copies(server, user, roles(server)[1], &id, Duration::ZERO).await).is_empty()
    })
    .await;
    let drafts = copies(server, user, roles(server)[1], &id, Duration::ZERO).await;
    println!(
        "{}: sent; Sent copies {} (flags {:?}); the source \\Answered {}; the draft's copies \
         live {} of {}",
        server.name,
        sent.len(),
        sent.first().map(|copy| copy.flags.clone()),
        answered.is_some(),
        live(&drafts).len(),
        drafts.len()
    );
    assert_eq!(sent.len(), 1, "{}: one Sent copy", server.name);
    assert_eq!(sent[0].flags, ["\\Seen"], "{}", server.name);
    assert!(
        answered.is_some(),
        "{}: the source is \\Answered",
        server.name
    );
    assert!(
        gone.is_some(),
        "{}: the draft's copies are gone",
        server.name
    );

    // A second message, the Sent copy's APPEND cut part of the way into the literal.
    let id = message_id(&sender);
    let middle = format!("SECONDMARK{}", server.name.replace('-', ""));
    let draft = Draft {
        account,
        from: address("Outbox Fault", &sender),
        to: vec![address("Ola Nordmann", "bob@example.test")],
        subject: "Andre melding".into(),
        html: body(&middle),
        message_id: id.clone(),
        ..Draft::default()
    };
    proxy.cut_on_send(&middle);
    let draft = queue(&engine, draft, now_ms()).await;
    assert_eq!(outcome(&mut engine, draft, WAIT).await, Some(Outcome::Sent));
    let done = until(WAIT, || async { state(&engine, draft).await.is_none() }).await;
    assert!(
        done.is_some(),
        "{}: the second Sent copy was recorded",
        server.name
    );
    let sent = copies(server, user, roles(server)[0], &id, Duration::from_secs(5)).await;
    println!(
        "{}: the Sent copy's APPEND cut mid-literal: {} Sent copies; the rule fired {}",
        server.name,
        sent.len(),
        !proxy.sent_armed()
    );
    assert!(!proxy.sent_armed(), "{}: the cut fired", server.name);
    assert_eq!(sent.len(), 1, "{}: one Sent copy", server.name);
    drop(engine);
    test.logout().await;
    clear(server, user, &["INBOX", roles(server)[0], roles(server)[1]]).await;
    clear(STALWART, "bob", &["INBOX"]).await;
    Mailpit::clear().await;
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn sent_copy_and_drafts_dovecot() {
    runtime().block_on(sent_copy_and_drafts(DOVECOT));
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn sent_copy_and_drafts_dovecot_rev1() {
    runtime().block_on(sent_copy_and_drafts(DOVECOT_REV1));
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn sent_copy_and_drafts_stalwart() {
    runtime().block_on(sent_copy_and_drafts(STALWART));
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn submission_limit_answered_and_cached() {
    runtime().block_on(async {
        let mut engine = TestEngine::start(config());
        let account = engine
            .add_with(
                1,
                "outbox",
                tls(STALWART.tls),
                common::submission(Security::Tls),
            )
            .await;
        settled(&mut engine, account).await;
        for request in [7, 8] {
            let started = Instant::now();
            engine.send(Command::SubmissionLimit { request, account });
            let limit = engine
                .event(WAIT, |event| match event {
                    Event::SubmissionLimit {
                        request: id,
                        result,
                        ..
                    } if *id == request => Some(result.clone()),
                    _ => None,
                })
                .await;
            println!(
                "SubmissionLimit {request}: {limit:?} in {:.1} ms",
                started.elapsed().as_secs_f64() * 1000.0
            );
            assert_eq!(limit, Some(Ok(Some(104_857_600))));
        }
    });
}
