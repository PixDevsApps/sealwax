//! M6's testbed suites for the Sent copy and drafts (ML-077, ML-074) and S1's mid-APPEND
//! scenario, carried into M6: APPEND of S7's Sent copies with `\Seen`, read back byte for byte;
//! a message found by Message-ID with and without a mark; a draft's revision replacing its
//! older copy; the connection cut as APPEND goes out, part of the way into the literal, and after
//! the server stored the message with its reply lost (for the Sent copy and a draft's
//! revision), each retried with the folder's mark, over several seeds; a blind replay as the
//! control; and APPEND times for 1 KB and about 10 MB. On Dovecot, IMAP4rev1-only Dovecot and
//! Stalwart, as the users `outbox` (direct) and `outboxfault` (through the fault proxy), whose
//! Sendt and Utkast they empty. Synthetic messages and S7's fixtures only.
//!
//! SEALWAX_TESTBED_CA=…/tools/testbed/certs/ca.pem \
//!   cargo test -p sealwax-engine --release --test append_testbed -- --ignored --test-threads=1 --nocapture

mod common;
mod outgoing;
mod proxy;
mod snapshot;
mod testbed_client;

use common::{DOVECOT, DOVECOT_REV1, PASSWORD, SERVERS, STALWART, Server};
use outgoing::{Held, fixtures, messages, ms, percentiles, run_tag, synthetic, with_id};
use proxy::Proxy;
use sealwax_domain::account::{Endpoint, Security};
use sealwax_domain::model::Flags;
use sealwax_engine::imap::write::{self, Append, OldCopies, WriteFailure};
use sealwax_engine::imap::{self, Connection, Credentials};
use std::time::Instant;
use testbed_client::Client;

const SENT: &str = "Sendt";
const DRAFTS: &str = "Utkast";

fn runtime() -> tokio::runtime::Runtime {
    sealwax_engine::runtime().unwrap()
}

async fn engine(port: u16, user: &str) -> Connection {
    imap::connect(
        &Endpoint {
            host: "127.0.0.1".into(),
            port,
            security: Security::Tls,
        },
        Credentials::Password {
            username: user,
            password: PASSWORD,
        },
        common::trust().client_config().unwrap(),
    )
    .await
    .unwrap_or_else(|error| panic!("engine on {port}: {error}"))
}

async fn client(server: Server, user: &str) -> Client {
    let mut client = Client::connect(server.tls, &common::ca(), user)
        .await
        .unwrap_or_else(|error| panic!("test client on {}: {error:?}", server.name));
    for folder in [SENT, DRAFTS] {
        client.create(folder).await.unwrap();
    }
    client
}

fn id_of(held: &Held) -> Option<String> {
    outgoing::seen(&held.raw).message_id
}

/// The copies of `id` in `folder`.
async fn copies(client: &mut Client, folder: &str, id: &str) -> Vec<Held> {
    messages(client, folder)
        .await
        .into_iter()
        .filter(|held| id_of(held).as_deref() == Some(id))
        .collect()
}

fn deleted(held: &Held) -> bool {
    held.flags.iter().any(|flag| flag == "\\Deleted")
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn appends_on_every_server() {
    runtime().block_on(async {
        let tag = run_tag();
        for server in SERVERS {
            let mut test = client(server, "outbox").await;
            test.empty(SENT).await.unwrap();
            test.empty(DRAFTS).await.unwrap();
            let mut conn = engine(server.tls, "outbox").await;
            println!(
                "{}: UIDPLUS {}, LITERAL+ {}",
                server.name,
                conn.caps().has("UIDPLUS"),
                conn.caps().has("LITERAL+")
            );
            let mut appended_count = 0;
            for (n, fixture) in fixtures().iter().enumerate() {
                let id = format!("m6.{tag}.sent.{n}@example.com");
                let bytes = with_id(&fixture.sent_copy, &id);
                let mark = write::mark_target(&mut conn, SENT).await.unwrap();
                let appended = write::append(
                    &mut conn,
                    Append {
                        folder: SENT,
                        message: &bytes,
                        message_id: &id,
                        flags: Flags::SEEN,
                        mark,
                        retry: false,
                    },
                )
                .await
                .unwrap();
                let held = copies(&mut test, SENT, &id).await;
                assert_eq!(held.len(), 1, "{} {}", server.name, fixture.name);
                assert_eq!(appended.uid, Some(held[0].uid), "{}", server.name);
                assert_eq!(held[0].raw, bytes, "{} {}: byte for byte", server.name, fixture.name);
                assert_eq!(held[0].flags, ["\\Seen"], "{}", server.name);
                appended_count += 1;
                let found = write::find(&mut conn, SENT, &id, None).await.unwrap();
                assert_eq!(found.uids, [held[0].uid], "{} find", server.name);
                let marked = write::find(&mut conn, SENT, &id, Some(mark)).await.unwrap();
                assert_eq!(marked.uids, [held[0].uid], "{} find from the mark", server.name);
            }
            println!(
                "{}: {} Sent copies appended byte for byte, `\\Seen`, UIDs known",
                server.name, appended_count
            );

            let id = format!("m6.{tag}.draft@example.com");
            let flags = Flags::DRAFT.union(Flags::SEEN);
            let first = synthetic(&id, 3_000, "first revision", "END");
            let mark = write::mark_target(&mut conn, DRAFTS).await.unwrap();
            let request = Append {
                folder: DRAFTS,
                message: &first,
                message_id: &id,
                flags,
                mark,
                retry: false,
            };
            let old = write::replace_draft(&mut conn, request, None)
                .await
                .unwrap()
                .appended
                .unwrap();
            let second = synthetic(&id, 3_500, "second revision", "END");
            let mark = write::mark_target(&mut conn, DRAFTS).await.unwrap();
            let old_uids = [old.uid.unwrap()];
            let replaced = write::replace_draft(
                &mut conn,
                Append {
                    message: &second,
                    mark,
                    ..request
                },
                Some(OldCopies {
                    uidvalidity: old.uidvalidity.unwrap(),
                    uids: &old_uids,
                }),
            )
            .await
            .unwrap();
            let held = copies(&mut test, DRAFTS, &id).await;
            let live: Vec<&Held> = held.iter().filter(|held| !deleted(held)).collect();
            assert_eq!(live.len(), 1, "{}: one live revision", server.name);
            assert_eq!(live[0].raw, second, "{}", server.name);
            let mut flags = live[0].flags.clone();
            flags.sort();
            assert_eq!(flags, ["\\Draft", "\\Seen"], "{}", server.name);
            println!(
                "{}: a draft's revision replaced its older copy: removed {:?}, left \\Deleted {:?}, \
                 copies on the server {}",
                server.name,
                replaced.removed,
                replaced.done.left_deleted,
                held.len()
            );
            conn.logout().await;
            test.empty(SENT).await.unwrap();
            test.empty(DRAFTS).await.unwrap();
            test.logout().await;
        }
    });
}

/// Seeds per fault scenario: `SEALWAX_APPEND_SEEDS`, else 5.
fn seeds() -> u64 {
    std::env::var("SEALWAX_APPEND_SEEDS")
        .ok()
        .and_then(|seeds| seeds.parse().ok())
        .unwrap_or(5)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    CutOnSend,
    CutMidLiteral,
    ReplyLost,
    DraftReplyLost,
}

#[derive(Debug, Default)]
struct Tally {
    runs: u64,
    after_cut: [u64; 2],
    earlier: u64,
    duplicates: u64,
    lost: u64,
    older_left: u64,
}

async fn fault_suite(server: Server) {
    let proxy = Proxy::start(server.tls, &common::ca()).await;
    let tag = run_tag();
    let mut test = client(server, "outboxfault").await;
    for fault in [
        Fault::CutOnSend,
        Fault::CutMidLiteral,
        Fault::ReplyLost,
        Fault::DraftReplyLost,
    ] {
        test.empty(SENT).await.unwrap();
        test.empty(DRAFTS).await.unwrap();
        let mut tally = Tally::default();
        for seed in 0..seeds() {
            let id = format!("m6.{tag}.{fault:?}.{seed}@example.com");
            let middle = format!("MIDDLE-{fault:?}-{seed}");
            let end = format!("END-{fault:?}-{seed}");
            let size = 200 * 1024 + seed as usize * 7_919;
            let bytes = synthetic(&id, size, &middle, &end);
            let draft = fault == Fault::DraftReplyLost;
            let folder = if draft { DRAFTS } else { SENT };
            let flags = if draft {
                Flags::DRAFT.union(Flags::SEEN)
            } else {
                Flags::SEEN
            };
            let mut conn = engine(proxy.port, "outboxfault").await;
            let old = if draft {
                let older = synthetic(&id, 4_000, "older revision", "END");
                let mark = write::mark_target(&mut conn, folder).await.unwrap();
                let appended = write::append(
                    &mut conn,
                    Append {
                        folder,
                        message: &older,
                        message_id: &id,
                        flags,
                        mark,
                        retry: false,
                    },
                )
                .await
                .unwrap();
                Some((appended.uidvalidity.unwrap(), appended.uid.unwrap()))
            } else {
                None
            };
            let old_uids: Vec<u32> = old.iter().map(|(_, uid)| *uid).collect();
            let old_copies = old.map(|(uidvalidity, _)| OldCopies {
                uidvalidity,
                uids: &old_uids,
            });
            let mark = write::mark_target(&mut conn, folder).await.unwrap();
            let request = Append {
                folder,
                message: &bytes,
                message_id: &id,
                flags,
                mark,
                retry: false,
            };
            match fault {
                Fault::CutOnSend => proxy.cut_on_send("APPEND "),
                Fault::CutMidLiteral => proxy.cut_on_send(&middle),
                Fault::ReplyLost | Fault::DraftReplyLost => proxy.lose_reply(&end),
            }
            let first = write::replace_draft(&mut conn, request, old_copies).await;
            assert!(
                !proxy.sent_armed(),
                "{} {fault:?}: the fault fired",
                server.name
            );
            let error = first.expect_err("the first attempt fails");
            assert_eq!(
                error.failure(),
                WriteFailure::Retry,
                "{} {fault:?}",
                server.name
            );
            assert!(error.is_connection());
            let after_cut = copies(&mut test, folder, &id)
                .await
                .iter()
                .filter(|held| held.raw == bytes)
                .count();
            tally.after_cut[after_cut.min(1)] += 1;
            let mut conn = engine(proxy.port, "outboxfault").await;
            let retried = write::replace_draft(
                &mut conn,
                Append {
                    retry: true,
                    ..request
                },
                old_copies,
            )
            .await
            .unwrap_or_else(|error| panic!("{} {fault:?}: the retry: {error}", server.name));
            conn.logout().await;
            tally.runs += 1;
            tally.earlier += u64::from(retried.appended.unwrap().earlier);
            let held = copies(&mut test, folder, &id).await;
            let live: Vec<&Held> = held.iter().filter(|held| !deleted(held)).collect();
            let revisions = live.iter().filter(|held| held.raw == bytes).count() as u64;
            tally.duplicates += revisions.saturating_sub(1);
            tally.lost += u64::from(revisions == 0);
            tally.older_left += (live.len() as u64).saturating_sub(revisions);
            assert_eq!(
                retried.appended.unwrap().uid,
                live.iter()
                    .find(|held| held.raw == bytes)
                    .map(|held| held.uid),
                "{} {fault:?}: the UID is known",
                server.name
            );
        }
        println!(
            "{} {fault:?}: {} runs; after the cut, copies 0/1: {}/{}; the retry found an earlier \
             copy {} times; duplicates {}; lost {}; older revisions left live {}",
            server.name,
            tally.runs,
            tally.after_cut[0],
            tally.after_cut[1],
            tally.earlier,
            tally.duplicates,
            tally.lost,
            tally.older_left
        );
        assert_eq!(tally.duplicates, 0, "{} {fault:?}", server.name);
        assert_eq!(tally.lost, 0, "{} {fault:?}", server.name);
        assert_eq!(tally.older_left, 0, "{} {fault:?}", server.name);
        match fault {
            Fault::CutOnSend | Fault::CutMidLiteral => assert_eq!(tally.after_cut[1], 0),
            Fault::ReplyLost | Fault::DraftReplyLost => {
                assert_eq!(tally.after_cut[1], tally.runs);
                assert_eq!(tally.earlier, tally.runs);
            }
        }
    }

    // The control: a blind replay of an APPEND whose reply was lost.
    test.empty(SENT).await.unwrap();
    let id = format!("m6.{tag}.blind@example.com");
    let bytes = synthetic(&id, 50 * 1024, "MIDDLE-blind", "END-blind");
    let mut conn = engine(proxy.port, "outboxfault").await;
    let mark = write::mark_target(&mut conn, SENT).await.unwrap();
    let request = Append {
        folder: SENT,
        message: &bytes,
        message_id: &id,
        flags: Flags::SEEN,
        mark,
        retry: false,
    };
    proxy.lose_reply("END-blind");
    write::append(&mut conn, request).await.unwrap_err();
    let mut conn = engine(proxy.port, "outboxfault").await;
    write::append(&mut conn, request).await.unwrap();
    conn.logout().await;
    let blind = copies(&mut test, SENT, &id).await.len();
    println!(
        "{}: control, a blind replay of an APPEND whose reply was lost: {blind} copies",
        server.name
    );
    test.empty(SENT).await.unwrap();
    test.empty(DRAFTS).await.unwrap();
    test.logout().await;
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn append_faults_dovecot() {
    runtime().block_on(fault_suite(DOVECOT));
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn append_faults_dovecot_rev1() {
    runtime().block_on(fault_suite(DOVECOT_REV1));
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn append_faults_stalwart() {
    runtime().block_on(fault_suite(STALWART));
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn append_speed_on_every_server() {
    runtime().block_on(async {
        let tag = run_tag();
        for server in SERVERS {
            let mut test = client(server, "outbox").await;
            test.empty(SENT).await.unwrap();
            let mut conn = engine(server.tls, "outbox").await;
            for (size, runs) in [(1_024, 20), (10 * 1024 * 1024, 5)] {
                let mut samples = Vec::new();
                let mut uid_known = 0;
                for run in 0..runs {
                    let id = format!("m6.{tag}.speed.{size}.{run}@example.com");
                    let bytes = synthetic(&id, size, "MIDDLE", "END");
                    let mark = write::mark_target(&mut conn, SENT).await.unwrap();
                    let started = Instant::now();
                    let appended = write::append(
                        &mut conn,
                        Append {
                            folder: SENT,
                            message: &bytes,
                            message_id: &id,
                            flags: Flags::SEEN,
                            mark,
                            retry: false,
                        },
                    )
                    .await
                    .unwrap();
                    samples.push(ms(started.elapsed()));
                    uid_known += usize::from(appended.uid.is_some());
                }
                let (p50, p95) = percentiles(&mut samples);
                println!(
                    "{}: APPEND of {size} bytes, {runs} runs: p50 {p50:.1} ms, p95 {p95:.1} ms; \
                     UID known {uid_known} of {runs}",
                    server.name
                );
                test.empty(SENT).await.unwrap();
            }
            conn.logout().await;
            test.logout().await;
        }
    });
}
