//! M4's testbed suites for the op queue's changes on the server (ML-051, ML-052): what each
//! server offers (MOVE, UIDPLUS, COPYUID, PERMANENTFLAGS) and how it refuses; flags and keywords,
//! moves (UID MOVE, and the COPY path, forced where MOVE is offered), deleting for good and a
//! folder marked read, each checked against the server afterwards; S1's lost replies of MOVE,
//! COPY, the `\Deleted` STORE and UID EXPUNGE through the fault proxy, retried, over several
//! seeds; a rule's copy (ADR-020), and its retry making no second copy; and the timings of flags and moves on 1, 50 and 500 messages. They need the Docker
//! testbed running and seeded, and use only the users `ops` (direct) and `opsfault` (through
//! the proxy), whose folders they empty and fill with synthetic messages:
//!
//! SEALWAX_TESTBED_CA=…/tools/testbed/certs/ca.pem \
//!   cargo test -p sealwax-engine --release --test write_testbed -- --ignored --test-threads=1 --nocapture

mod common;
mod proxy;
mod snapshot;
mod testbed_client;

use async_imap::imap_proto::{
    AttributeValue, MessageSection, Response, ResponseCode, SectionPath, Status,
};
use common::{DOVECOT, DOVECOT_REV1, PASSWORD, SERVERS, STALWART, Server};
use proxy::Proxy;
use sealwax_domain::account::{Endpoint, Security};
use sealwax_domain::message;
use sealwax_domain::model::Flags;
use sealwax_engine::imap::write::{
    self, Copy, Done, Mark, Move, Moving, OpFolder, WriteError, WriteFailure,
};
use sealwax_engine::imap::{self, ConnectOptions, Connection, Credentials, ImapError};
use snapshot::Rng;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};
use testbed_client::Client;

const INBOX: &str = "INBOX";
const ARCHIVE: &str = "Arkiv";
const TRASH: &str = "Papirkurv";
const JUNK: &str = "S&APg-ppelpost";
const FOLDERS: [&str; 4] = [INBOX, ARCHIVE, TRASH, JUNK];

fn runtime() -> tokio::runtime::Runtime {
    sealwax_engine::runtime().unwrap()
}

async fn engine(port: u16, user: &str, moves: bool) -> Connection {
    imap::connect_with(
        &Endpoint {
            host: "127.0.0.1".into(),
            port,
            security: Security::Tls,
            certificate: None,
        },
        Credentials::Password {
            username: user,
            password: PASSWORD,
        },
        common::trust().client_config().unwrap(),
        ConnectOptions {
            moves,
            ..ConnectOptions::default()
        },
    )
    .await
    .unwrap_or_else(|error| panic!("engine on {port}: {error}"))
}

async fn client(server: Server, user: &str) -> Client {
    Client::connect(server.tls, &common::ca(), user)
        .await
        .unwrap_or_else(|error| panic!("test client on {}: {error:?}", server.name))
}

/// Empties the user's test folders (creating them where missing).
async fn reset(client: &mut Client) {
    for folder in FOLDERS {
        client.create(folder).await.unwrap();
        client.empty(folder).await.unwrap();
    }
}

/// Appends `count` synthetic messages to `folder` with `flags`; returns their Message-IDs.
async fn fill(
    client: &mut Client,
    folder: &str,
    tag: &str,
    count: usize,
    flags: &str,
    rng: &mut Rng,
) -> Vec<String> {
    let mut ids = Vec::new();
    for n in 0..count {
        let id = format!("ops.{tag}.{n}@example.test");
        let raw = testbed_client::message(&id, rng);
        client.append(folder, flags, &raw).await.unwrap();
        ids.push(id);
    }
    ids
}

/// A message as the server holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    uid: u32,
    flags: Flags,
    keywords: Vec<String>,
    id: String,
}

impl Row {
    fn deleted(&self) -> bool {
        self.flags.contains(Flags::DELETED)
    }
}

/// A folder's messages, read by the test client (BODY.PEEK, nothing marked).
async fn read(client: &mut Client, folder: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    if client.select(folder).await.unwrap() == 0 {
        return rows;
    }
    client
        .run(
            "UID FETCH 1:* (UID FLAGS BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])",
            |response| {
                let Response::Fetch(_, attributes) = response else {
                    return;
                };
                let mut row = Row {
                    uid: 0,
                    flags: Flags::NONE,
                    keywords: Vec::new(),
                    id: String::new(),
                };
                for attribute in attributes {
                    match attribute {
                        AttributeValue::Uid(uid) => row.uid = *uid,
                        AttributeValue::Flags(flags) => {
                            (row.flags, row.keywords) =
                                Flags::from_imap(flags.iter().map(|f| f.as_ref()));
                        }
                        AttributeValue::BodySection {
                            section: Some(SectionPath::Full(MessageSection::Header)),
                            data,
                            ..
                        } => {
                            row.id = message::parse(data.as_deref().unwrap_or_default())
                                .headers
                                .message_id
                                .unwrap_or_default();
                        }
                        _ => {}
                    }
                }
                rows.push(row);
            },
        )
        .await
        .unwrap();
    rows.sort_by_key(|row| row.uid);
    rows
}

fn ids(rows: &[Row]) -> Vec<String> {
    let mut ids: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
    ids.sort();
    ids
}

fn moving(rows: &[Row]) -> Vec<Moving<'_>> {
    rows.iter()
        .map(|row| Moving {
            uid: row.uid,
            message_id: Some(row.id.as_str()),
        })
        .collect()
}

async fn opened<'a>(conn: &mut Connection, name: &'a str) -> OpFolder<'a> {
    OpFolder {
        name_raw: name,
        uidvalidity: write::select(conn, name).await.unwrap().uidvalidity,
    }
}

fn plain<'a>(from: OpFolder<'a>, to: &'a str, messages: &'a [Moving<'a>]) -> Move<'a> {
    Move {
        from,
        to,
        messages,
        add: Flags::NONE,
        remove: Flags::NONE,
        mark: None,
        retry: false,
    }
}

/// Each moved pair names the right message in the target.
fn pairs_match(done: &Done, source: &[Row], target: &[Row]) {
    for (from, to) in &done.moved {
        let id = &source.iter().find(|row| row.uid == *from).unwrap().id;
        let found = target.iter().find(|row| row.uid == *to);
        assert_eq!(found.map(|row| &row.id), Some(id), "pair {from} → {to}");
    }
}

/// The paths a server takes moves on: UID MOVE where offered, and the COPY path.
fn paths(server: Server) -> Vec<bool> {
    if server == DOVECOT_REV1 {
        vec![false]
    } else {
        vec![true, false]
    }
}

fn path_name(moves: bool) -> &'static str {
    if moves { "UID MOVE" } else { "COPY path" }
}

/// The response code of a refusal, for the table (synthetic test data only).
fn code(error: &WriteError) -> String {
    match error {
        WriteError::Imap(ImapError::No(text) | ImapError::Bad(text)) => {
            format!("[{}]", text.code().unwrap_or("no code"))
        }
        other => format!("({other})"),
    }
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn capabilities_copyuid_and_refusals_on_every_server() {
    runtime().block_on(async {
        println!(
            "| server | MOVE | UIDPLUS | CONDSTORE | QRESYNC | PERMANENTFLAGS (INBOX) | $Junk/$NotJunk/$Forwarded kept | COPYUID on COPY | COPYUID on MOVE | move to a missing folder | SELECT of a missing folder |"
        );
        for server in SERVERS {
            let mut test = client(server, "ops").await;
            reset(&mut test).await;
            let mut rng = Rng(7);
            fill(&mut test, INBOX, "probe", 4, "", &mut rng).await;
            let mut permanent = String::new();
            test.run(&format!("SELECT {INBOX}"), |response| {
                if let Response::Data {
                    status: Status::Ok,
                    outcome,
                } = response
                    && let Some(ResponseCode::PermanentFlags(flags)) = &outcome.code
                {
                    permanent = flags.join(" ");
                }
            })
            .await
            .unwrap();
            let uids: Vec<u32> = read(&mut test, INBOX).await.iter().map(|r| r.uid).collect();
            test.select(INBOX).await.unwrap();
            let copy = test
                .copyuid_placement(&format!("UID COPY {} {ARCHIVE}", uids[0]))
                .await
                .unwrap();
            let mut conn = engine(server.tls, "ops", true).await;
            let moves = conn.caps().has("MOVE");
            let on_move = if moves {
                test.select(INBOX).await.unwrap();
                let placed = test
                    .copyuid_placement(&format!("UID MOVE {} {ARCHIVE}", uids[1]))
                    .await
                    .unwrap();
                format!("{placed:?}")
            } else {
                "n/a".to_owned()
            };
            let mut kept = Vec::new();
            for folder in FOLDERS {
                let selected = write::select(&mut conn, folder).await.unwrap();
                kept.push(format!(
                    "{}/{}/{}",
                    selected.keeps("$Junk"),
                    selected.keeps("$NotJunk"),
                    selected.keeps("$Forwarded")
                ));
            }
            kept.dedup();
            let inbox = opened(&mut conn, INBOX).await;
            let rows = read(&mut test, INBOX).await;
            let all = moving(&rows);
            let refused = write::move_messages(&mut conn, plain(inbox, "Nowhere", &all))
                .await
                .unwrap_err();
            assert_eq!(refused.failure(), WriteFailure::Refuse, "{}", server.name);
            let missing = write::select(&mut conn, "Nowhere").await.unwrap_err();
            assert_eq!(missing.failure(), WriteFailure::Refuse, "{}", server.name);
            assert_eq!(read(&mut test, INBOX).await.len(), rows.len());
            println!(
                "| {} | {} | {} | {} | {} | {} | {} | {:?} (untagged, tagged) | {} | {:?} {} | {:?} {} |",
                server.name,
                moves,
                conn.caps().has("UIDPLUS"),
                conn.caps().has("CONDSTORE"),
                conn.qresync(),
                permanent,
                kept.join(", "),
                copy,
                on_move,
                refused.failure(),
                code(&refused),
                missing.failure(),
                code(&missing),
            );
            conn.logout().await;
            reset(&mut test).await;
            test.logout().await;
        }
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn flags_and_keywords_on_every_server() {
    runtime().block_on(async {
        for server in SERVERS {
            let mut test = client(server, "ops").await;
            reset(&mut test).await;
            let mut rng = Rng(11);
            fill(&mut test, INBOX, "flags", 12, "", &mut rng).await;
            let rows = read(&mut test, INBOX).await;
            let uids: Vec<u32> = rows.iter().map(|row| row.uid).collect();
            let mut conn = engine(server.tls, "ops", true).await;
            let inbox = opened(&mut conn, INBOX).await;
            let star = Flags::SEEN | Flags::FLAGGED;
            let mut dropped = Flags::NONE;
            for _ in 0..2 {
                let steps = [
                    (&uids[0..6], star, Flags::NONE),
                    (&uids[0..2], Flags::NONE, Flags::SEEN),
                    (&uids[6..9], Flags::JUNK, Flags::NOT_JUNK),
                    (&uids[9..12], Flags::NOT_JUNK, Flags::JUNK),
                ];
                for (some, add, remove) in steps {
                    let done = write::store_flags(&mut conn, inbox, some, add, remove)
                        .await
                        .unwrap();
                    dropped = dropped.union(done.dropped);
                }
            }
            let after = read(&mut test, INBOX).await;
            let has = |uid: u32, flag: Flags| {
                after
                    .iter()
                    .find(|row| row.uid == uid)
                    .unwrap()
                    .flags
                    .contains(flag)
            };
            for (i, uid) in uids.iter().enumerate() {
                assert_eq!(has(*uid, Flags::SEEN), (2..6).contains(&i), "{} {i}", server.name);
                assert_eq!(has(*uid, Flags::FLAGGED), i < 6, "{} {i}", server.name);
                if !dropped.contains(Flags::JUNK) {
                    assert_eq!(has(*uid, Flags::JUNK), (6..9).contains(&i), "{}", server.name);
                    assert_eq!(has(*uid, Flags::NOT_JUNK), i >= 9, "{}", server.name);
                }
                assert!(!has(*uid, Flags::DELETED));
            }
            println!(
                "{}: flags and keywords as asked after the steps and their replay; dropped {dropped:?}",
                server.name
            );
            conn.logout().await;
            reset(&mut test).await;
            test.logout().await;
        }
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn moves_junk_and_not_junk_on_every_server() {
    runtime().block_on(async {
        for server in SERVERS {
            for moves in paths(server) {
                let name = format!("{} {}", server.name, path_name(moves));
                let mut test = client(server, "ops").await;
                reset(&mut test).await;
                let mut rng = Rng(13);
                fill(&mut test, INBOX, "move", 6, "", &mut rng).await;
                fill(&mut test, INBOX, "junk", 2, "$NotJunk", &mut rng).await;
                let rows = read(&mut test, INBOX).await;
                let mut conn = engine(server.tls, "ops", moves).await;
                let uidplus = conn.caps().has("UIDPLUS");

                let archive_mark = write::mark_target(&mut conn, ARCHIVE).await.unwrap();
                let inbox = opened(&mut conn, INBOX).await;
                let first = moving(&rows[0..5]);
                let archived = write::move_messages(
                    &mut conn,
                    Move {
                        mark: Some(archive_mark),
                        ..plain(inbox, ARCHIVE, &first)
                    },
                )
                .await
                .unwrap();
                let archive = read(&mut test, ARCHIVE).await;
                assert_eq!(ids(&archive), ids(&rows[0..5]), "{name}");
                assert!(archive.iter().all(|row| !row.deleted()), "{name}");
                assert_eq!(archived.moved.len(), 5, "{name}: {archived:?}");
                pairs_match(&archived, &rows, &archive);
                let left = read(&mut test, INBOX).await;
                let live: Vec<Row> = left.iter().filter(|r| !r.deleted()).cloned().collect();
                assert_eq!(ids(&live), ids(&rows[5..]), "{name}");
                if uidplus {
                    assert!(left.iter().all(|row| !row.deleted()), "{name}");
                    assert!(archived.left_deleted.is_empty());
                } else {
                    let deleted: Vec<u32> =
                        left.iter().filter(|r| r.deleted()).map(|r| r.uid).collect();
                    assert_eq!(archived.left_deleted, deleted, "{name}");
                    assert_eq!(deleted.len(), 5, "{name}");
                }

                let junk_mark = write::mark_target(&mut conn, JUNK).await.unwrap();
                let spam = moving(&rows[6..8]);
                let done = write::move_messages(
                    &mut conn,
                    Move {
                        add: Flags::JUNK,
                        remove: Flags::NOT_JUNK,
                        mark: Some(junk_mark),
                        ..plain(inbox, JUNK, &spam)
                    },
                )
                .await
                .unwrap();
                let junk = read(&mut test, JUNK).await;
                assert_eq!(ids(&junk), ids(&rows[6..8]), "{name}");
                pairs_match(&done, &rows, &junk);
                if done.dropped == Flags::NONE {
                    for row in &junk {
                        assert!(row.flags.contains(Flags::JUNK), "{name}");
                        assert!(!row.flags.contains(Flags::NOT_JUNK), "{name}");
                    }
                }

                let inbox_mark = write::mark_target(&mut conn, INBOX).await.unwrap();
                let junk_folder = opened(&mut conn, JUNK).await;
                let back = moving(&junk);
                write::move_messages(
                    &mut conn,
                    Move {
                        add: Flags::NOT_JUNK,
                        remove: Flags::JUNK,
                        mark: Some(inbox_mark),
                        ..plain(junk_folder, INBOX, &back)
                    },
                )
                .await
                .unwrap();
                let inbox_rows = read(&mut test, INBOX).await;
                let returned: Vec<&Row> = inbox_rows
                    .iter()
                    .filter(|row| row.id.starts_with("ops.junk.") && !row.deleted())
                    .collect();
                assert_eq!(returned.len(), 2, "{name}");
                assert!(
                    returned
                        .iter()
                        .all(|row| row.flags.contains(Flags::NOT_JUNK)
                            && !row.flags.contains(Flags::JUNK)),
                    "{name}"
                );
                println!(
                    "{name}: 5 to the archive ({} target UIDs known, target UIDVALIDITY {:?}), 2 to Junk with $Junk ({} pairs), back to the Inbox as $NotJunk; {} left flagged \\Deleted",
                    archived.moved.len(),
                    archived.target_uidvalidity.is_some(),
                    done.moved.len(),
                    if uidplus { 0 } else { 5 + 2 + 2 }
                );
                conn.logout().await;
                reset(&mut test).await;
                test.logout().await;
            }
        }
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn a_rules_copy_on_every_server_and_its_retry_makes_no_second() {
    runtime().block_on(async {
        for server in SERVERS {
            let mut test = client(server, "ops").await;
            reset(&mut test).await;
            let mut rng = Rng(23);
            fill(&mut test, INBOX, "copy", 3, "\\Seen", &mut rng).await;
            let rows = read(&mut test, INBOX).await;
            let messages = moving(&rows);
            let mut conn = engine(server.tls, "ops", true).await;
            let mark = write::mark_target(&mut conn, ARCHIVE).await.unwrap();
            let inbox = opened(&mut conn, INBOX).await;
            let copy = |retry| Copy {
                from: inbox,
                to: ARCHIVE,
                messages: &messages,
                mark: Some(mark),
                retry,
            };
            write::copy_messages(&mut conn, copy(false)).await.unwrap();
            let archive = read(&mut test, ARCHIVE).await;
            assert_eq!(ids(&archive), ids(&rows), "{}: each copied", server.name);
            assert!(
                archive.iter().all(|row| row.flags.contains(Flags::SEEN)),
                "{}: flags travel with a copy",
                server.name
            );
            assert_eq!(
                read(&mut test, INBOX).await.len(),
                3,
                "{}: the messages stay",
                server.name
            );
            write::copy_messages(&mut conn, copy(true)).await.unwrap();
            assert_eq!(
                read(&mut test, ARCHIVE).await.len(),
                3,
                "{}: a retry finds the copies and makes none",
                server.name
            );
            println!("{}: 3 copied once, the retry copied nothing", server.name);
            conn.logout().await;
            reset(&mut test).await;
            test.logout().await;
        }
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn deleting_for_good_on_every_server() {
    runtime().block_on(async {
        for server in SERVERS {
            let mut test = client(server, "ops").await;
            reset(&mut test).await;
            let mut rng = Rng(17);
            fill(&mut test, TRASH, "trash", 4, "", &mut rng).await;
            fill(&mut test, TRASH, "other", 1, "\\Deleted", &mut rng).await;
            let rows = read(&mut test, TRASH).await;
            let mut conn = engine(server.tls, "ops", true).await;
            let uidplus = conn.caps().has("UIDPLUS");
            let trash = opened(&mut conn, TRASH).await;
            let doomed = [rows[0].uid, rows[2].uid];
            let done = write::expunge(&mut conn, trash, &doomed).await.unwrap();
            let after = read(&mut test, TRASH).await;
            let other = after.iter().find(|row| row.id.starts_with("ops.other."));
            assert!(
                other.is_some_and(Row::deleted),
                "{}: another \\Deleted message stays",
                server.name
            );
            if uidplus {
                assert_eq!(after.len(), 3, "{}", server.name);
                assert!(after.iter().all(|row| !doomed.contains(&row.uid)));
            } else {
                assert_eq!(after.len(), 5, "{}", server.name);
                assert_eq!(done.left_deleted, doomed);
                for row in after.iter().filter(|row| doomed.contains(&row.uid)) {
                    assert!(row.deleted(), "{}", server.name);
                }
            }
            println!(
                "{}: 2 deleted for good ({}), another client's \\Deleted message kept",
                server.name,
                if uidplus {
                    "UID EXPUNGE"
                } else {
                    "left flagged \\Deleted, no UIDPLUS"
                }
            );
            conn.logout().await;
            reset(&mut test).await;
            test.logout().await;
        }
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn a_folder_marked_read_on_every_server() {
    runtime().block_on(async {
        for server in SERVERS {
            let mut test = client(server, "ops").await;
            reset(&mut test).await;
            let mut rng = Rng(19);
            fill(&mut test, ARCHIVE, "seen", 100, "\\Seen", &mut rng).await;
            fill(&mut test, ARCHIVE, "unseen", 200, "", &mut rng).await;
            let rows = read(&mut test, ARCHIVE).await;
            let up_to = rows[249].uid;
            let mut conn = engine(server.tls, "ops", true).await;
            let archive = opened(&mut conn, ARCHIVE).await;
            let started = Instant::now();
            let done = write::mark_folder_read(&mut conn, archive, Some(up_to))
                .await
                .unwrap();
            let first = started.elapsed();
            let expected: Vec<u32> = rows[100..250].iter().map(|row| row.uid).collect();
            assert_eq!(done.marked, expected, "{}", server.name);
            fill(&mut test, ARCHIVE, "late", 3, "", &mut rng).await;
            let after = read(&mut test, ARCHIVE).await;
            let unseen: Vec<u32> = after
                .iter()
                .filter(|row| !row.flags.contains(Flags::SEEN))
                .map(|row| row.uid)
                .collect();
            assert_eq!(unseen.len(), 50 + 3, "{}", server.name);
            assert!(unseen.iter().all(|uid| *uid > up_to));
            let done = write::mark_folder_read(&mut conn, archive, None)
                .await
                .unwrap();
            assert_eq!(done.marked, unseen, "{}", server.name);
            let after = read(&mut test, ARCHIVE).await;
            assert!(after.iter().all(|row| row.flags.contains(Flags::SEEN)));
            println!(
                "{}: 150 of 300 marked read up to the user's newest in {:.1} ms, then the other 53; all read on the server",
                server.name,
                first.as_secs_f64() * 1000.0
            );
            conn.logout().await;
            reset(&mut test).await;
            test.logout().await;
        }
    });
}

/// Fault runs' outcomes.
#[derive(Default)]
struct Tally {
    runs: usize,
    fired: usize,
    /// Messages moved, over all runs.
    messages: usize,
    /// Target UIDs the retry reported.
    pairs: usize,
    duplicates: usize,
    lost: usize,
    left_in_source: usize,
    /// Moved copies left flagged `\Deleted` in the target.
    deleted_in_target: usize,
    /// Right after the cut, before the retry: messages already only in the target, in both
    /// folders (copied, the source not yet removed or only flagged), only in the source, and in
    /// neither.
    cut_done: usize,
    cut_both: usize,
    cut_untouched: usize,
    cut_neither: usize,
}

/// Seeds per fault scenario: `SEALWAX_WRITE_SEEDS`, else 5.
fn seeds() -> u64 {
    std::env::var("SEALWAX_WRITE_SEEDS")
        .ok()
        .and_then(|seeds| seeds.parse().ok())
        .unwrap_or(5)
}

/// A lost or cut command, retried: `needle` with `lose` (the reply lost after the server acted)
/// or not (the connection cut as the command goes out).
#[allow(clippy::too_many_arguments)]
async fn fault_run(
    server: Server,
    proxy: &Proxy,
    moves: bool,
    needle: &str,
    lose: bool,
    retry: bool,
    seed: u64,
    tally: &mut Tally,
) {
    let mut rng = Rng(seed);
    let mut test = client(server, "opsfault").await;
    reset(&mut test).await;
    let count = 2 + rng.below(24);
    let tag = format!("fault.{seed}");
    fill(&mut test, INBOX, &tag, count, "", &mut rng).await;
    let target = *rng.pick(&[ARCHIVE, TRASH, JUNK]);
    fill(
        &mut test,
        target,
        &format!("{tag}.there"),
        rng.below(3),
        "",
        &mut rng,
    )
    .await;
    let rows = read(&mut test, INBOX).await;
    let chosen: Vec<Row> = rows.iter().filter(|_| rng.chance(70)).cloned().collect();
    let chosen = if chosen.is_empty() {
        vec![rows[0].clone()]
    } else {
        chosen
    };
    let before_target = ids(&read(&mut test, target).await);
    let messages = moving(&chosen);
    let (add, remove) = if target == JUNK {
        (Flags::JUNK, Flags::NOT_JUNK)
    } else {
        (Flags::NONE, Flags::NONE)
    };

    let mut conn = engine(proxy.port, "opsfault", moves).await;
    let mark: Mark = write::mark_target(&mut conn, target).await.unwrap();
    let inbox = opened(&mut conn, INBOX).await;
    if lose {
        proxy.lose_reply(needle);
    } else {
        proxy.cut_on_send(needle);
    }
    let request = Move {
        from: inbox,
        to: target,
        messages: &messages,
        add,
        remove,
        mark: Some(mark),
        retry: false,
    };
    let first = write::move_messages(&mut conn, request).await;
    assert!(
        !proxy.sent_armed(),
        "{} {needle}: the rule never fired",
        server.name
    );
    let error = first.expect_err("the first attempt was cut");
    assert_eq!(error.failure(), WriteFailure::Retry);
    assert!(error.is_connection());
    tally.fired += 1;
    let in_target: BTreeSet<String> = read(&mut test, target)
        .await
        .into_iter()
        .map(|row| row.id)
        .collect();
    let in_source: BTreeSet<String> = read(&mut test, INBOX)
        .await
        .into_iter()
        .filter(|row| !row.deleted())
        .map(|row| row.id)
        .collect();
    for row in &chosen {
        match (in_target.contains(&row.id), in_source.contains(&row.id)) {
            (true, false) => tally.cut_done += 1,
            (true, true) => tally.cut_both += 1,
            (false, true) => tally.cut_untouched += 1,
            (false, false) => tally.cut_neither += 1,
        }
    }

    let mut conn = engine(proxy.port, "opsfault", moves).await;
    let done = write::move_messages(&mut conn, Move { retry, ..request })
        .await
        .unwrap();
    conn.logout().await;

    let after_target = read(&mut test, target).await;
    let after_source = read(&mut test, INBOX).await;
    let mut expected = before_target.clone();
    expected.extend(chosen.iter().map(|row| row.id.clone()));
    expected.sort();
    let got = ids(&after_target);
    let moved_ids: BTreeSet<&str> = chosen.iter().map(|row| row.id.as_str()).collect();
    for id in &moved_ids {
        let copies = got.iter().filter(|g| g.as_str() == *id).count();
        tally.duplicates += copies.saturating_sub(1);
        tally.lost += usize::from(copies == 0);
    }
    tally.messages += moved_ids.len();
    tally.pairs += done.moved.len();
    tally.deleted_in_target += after_target
        .iter()
        .filter(|row| moved_ids.contains(row.id.as_str()) && row.deleted())
        .count();
    if target == JUNK && done.dropped == Flags::NONE {
        for row in after_target
            .iter()
            .filter(|row| moved_ids.contains(row.id.as_str()))
        {
            assert!(row.flags.contains(Flags::JUNK), "{} {needle}", server.name);
        }
    }
    let live_source: Vec<&Row> = after_source.iter().filter(|row| !row.deleted()).collect();
    tally.left_in_source += live_source
        .iter()
        .filter(|row| moved_ids.contains(row.id.as_str()))
        .count();
    let kept: BTreeSet<&str> = live_source.iter().map(|row| row.id.as_str()).collect();
    for row in &rows {
        if !moved_ids.contains(row.id.as_str()) {
            assert!(
                kept.contains(row.id.as_str()),
                "{} lost an unmoved message",
                server.name
            );
        }
    }
    if retry {
        assert_eq!(got, expected, "{} {needle} seed {seed}", server.name);
        pairs_match(&done, &chosen, &after_target);
    }
    tally.runs += 1;
    test.logout().await;
}

fn fault_suite(server: Server) {
    runtime().block_on(async {
        let proxy = Proxy::start(server.tls, &common::ca()).await;
        let mut summary = Vec::new();
        for moves in paths(server) {
            let mut needles = if moves {
                vec!["UID MOVE"]
            } else {
                vec!["UID COPY", "\\Deleted)"]
            };
            let uidplus = server != DOVECOT_REV1;
            if !moves && uidplus {
                needles.push("UID EXPUNGE");
            }
            let seeds = seeds();
            for needle in needles {
                let mut tally = Tally::default();
                for seed in 1..=seeds {
                    fault_run(server, &proxy, moves, needle, true, true, seed, &mut tally).await;
                }
                summary.push((path_name(moves), format!("{needle} reply lost"), tally));
            }
            let first = if moves { "UID MOVE" } else { "UID COPY" };
            let mut tally = Tally::default();
            for seed in 101..101 + seeds.div_ceil(2) {
                fault_run(server, &proxy, moves, first, false, true, seed, &mut tally).await;
            }
            summary.push((path_name(moves), format!("{first} cut on send"), tally));
            if !moves {
                let mut control = Tally::default();
                fault_run(server, &proxy, moves, "UID COPY", true, false, 201, &mut control).await;
                summary.push((
                    path_name(moves),
                    "UID COPY reply lost, blind replay (control)".into(),
                    control,
                ));
            }
        }
        let mut test = client(server, "opsfault").await;
        reset(&mut test).await;
        test.logout().await;
        println!(
            "| server | path | scenario | runs | cuts | messages | after the cut: moved / in both / untouched / in neither | target UIDs reported | duplicates | lost | left in source | \\Deleted in target |"
        );
        for (path, scenario, tally) in &summary {
            println!(
                "| {} | {path} | {scenario} | {} | {} | {} | {} / {} / {} / {} | {} | {} | {} | {} | {} |",
                server.name,
                tally.runs,
                tally.fired,
                tally.messages,
                tally.cut_done,
                tally.cut_both,
                tally.cut_untouched,
                tally.cut_neither,
                tally.pairs,
                tally.duplicates,
                tally.lost,
                tally.left_in_source,
                tally.deleted_in_target
            );
        }
        for (_, scenario, tally) in &summary {
            if !scenario.contains("control") {
                let bad = (
                    tally.duplicates,
                    tally.lost,
                    tally.left_in_source,
                    tally.deleted_in_target,
                );
                assert_eq!(bad, (0, 0, 0, 0), "{scenario}");
                assert_eq!(tally.pairs, tally.messages, "{scenario}: every target UID known");
            }
        }
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn faults_dovecot() {
    fault_suite(DOVECOT);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn faults_dovecot_rev1() {
    fault_suite(DOVECOT_REV1);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn faults_stalwart() {
    fault_suite(STALWART);
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn summary(samples: &mut [f64]) -> (f64, f64) {
    samples.sort_by(f64::total_cmp);
    (samples[samples.len() / 2], samples[samples.len() - 1])
}

/// Removes what the COPY path left flagged `\Deleted` without UIDPLUS (the test client's plain
/// EXPUNGE, on a test user's folder).
async fn purge(test: &mut Client, folder: &str) {
    test.select(folder).await.unwrap();
    test.run("EXPUNGE", |_| {}).await.unwrap();
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn write_speed_on_every_server() {
    const REPEATS: usize = 5;
    runtime().block_on(async {
        println!("| server | change | messages | p50 ms | max ms | runs |");
        for server in SERVERS {
            let mut test = client(server, "ops").await;
            reset(&mut test).await;
            let mut rng = Rng(23);
            fill(&mut test, INBOX, "speed", 500, "", &mut rng).await;
            let mut conn = engine(server.tls, "ops", true).await;
            for n in [1, 50, 500] {
                let rows = read(&mut test, INBOX).await;
                let uids: Vec<u32> = rows.iter().take(n).map(|row| row.uid).collect();
                let inbox = opened(&mut conn, INBOX).await;
                let mut samples = Vec::new();
                for repeat in 0..REPEATS {
                    let (add, remove) = if repeat % 2 == 0 {
                        (Flags::SEEN, Flags::NONE)
                    } else {
                        (Flags::NONE, Flags::SEEN)
                    };
                    let started = Instant::now();
                    write::store_flags(&mut conn, inbox, &uids, add, remove)
                        .await
                        .unwrap();
                    samples.push(ms(started.elapsed()));
                }
                let (p50, max) = summary(&mut samples);
                println!(
                    "| {} | flags | {n} | {p50:.1} | {max:.1} | {REPEATS} |",
                    server.name
                );
            }
            let mut marks = Vec::new();
            for moves in paths(server) {
                let mut conn = engine(server.tls, "ops", moves).await;
                for n in [1, 50, 500] {
                    let mut samples = Vec::new();
                    for repeat in 0..REPEATS {
                        let (from, to) = if repeat % 2 == 0 {
                            (INBOX, ARCHIVE)
                        } else {
                            (ARCHIVE, INBOX)
                        };
                        let rows: Vec<Row> = read(&mut test, from)
                            .await
                            .into_iter()
                            .filter(|row| !row.deleted())
                            .take(n)
                            .collect();
                        let messages = moving(&rows);
                        let started = Instant::now();
                        let mark = write::mark_target(&mut conn, to).await.unwrap();
                        marks.push(ms(started.elapsed()));
                        let source = opened(&mut conn, from).await;
                        let started = Instant::now();
                        let done = write::move_messages(
                            &mut conn,
                            Move {
                                mark: Some(mark),
                                ..plain(source, to, &messages)
                            },
                        )
                        .await
                        .unwrap();
                        samples.push(ms(started.elapsed()));
                        assert_eq!(done.moved.len(), rows.len(), "{} {n}", server.name);
                        if !done.left_deleted.is_empty() {
                            purge(&mut test, from).await;
                        }
                    }
                    if read(&mut test, ARCHIVE)
                        .await
                        .iter()
                        .any(|row| !row.deleted())
                    {
                        let rows = read(&mut test, ARCHIVE).await;
                        let messages = moving(&rows);
                        let source = opened(&mut conn, ARCHIVE).await;
                        write::move_messages(&mut conn, plain(source, INBOX, &messages))
                            .await
                            .unwrap();
                        purge(&mut test, ARCHIVE).await;
                    }
                    let (p50, max) = summary(&mut samples);
                    println!(
                        "| {} | move ({}) | {n} | {p50:.1} | {max:.1} | {REPEATS} |",
                        server.name,
                        path_name(moves)
                    );
                }
                conn.logout().await;
            }
            let (p50, max) = summary(&mut marks);
            println!(
                "| {} | mark_target (STATUS) | – | {p50:.1} | {max:.1} | {} |",
                server.name,
                marks.len()
            );
            assert_eq!(read(&mut test, INBOX).await.len(), 500);
            conn.logout().await;
            reset(&mut test).await;
            test.logout().await;
        }
    });
}
