//! The Sent copy and drafts on the server (ML-077, ML-074) against the fake IMAP server, in CI:
//! APPEND with APPENDUID and without (found by Message-ID from the mark), S1's mid-APPEND faults
//! (the connection cut before the command, part of the way into the literal, or after the
//! server stored the message with its reply lost) retried with no duplicate and nothing lost,
//! the blind replay that duplicates, a draft's revision replacing its older copies, refusals
//! before the literal, and finding a message by Message-ID where SEARCH can't. Synthetic data
//! only.

mod fake;

use fake::{Fake, Flavor};
use sealwax_domain::model::Flags;
use sealwax_engine::imap::write::{
    self, Append, Appended, Mark, OldCopies, WriteError, WriteFailure,
};

const SENT: &str = "Sendt";
const DRAFTS: &str = "Utkast";

fn runtime() -> tokio::runtime::Runtime {
    sealwax_engine::runtime().unwrap()
}

/// A fake with INBOX, Sendt and Utkast, UIDPLUS as given.
async fn fake(flavor: Flavor, uidplus: bool) -> Fake {
    let fake = Fake::start(flavor).await;
    {
        let mut world = fake.world();
        world.uidplus = uidplus;
        world.create("INBOX", &[]);
        world.create(SENT, &["\\Sent"]);
        world.create(DRAFTS, &["\\Drafts"]);
    }
    fake
}

fn message(id: &str, body: &str) -> Vec<u8> {
    format!(
        "From: <me@example.test>\r\nTo: <you@example.test>\r\nSubject: Synthetic\r\n\
         Date: Tue, 6 Oct 2026 12:00:00 +0000\r\nMessage-ID: <{id}>\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

/// Copies of `id` in `folder`, with their flags.
fn copies(fake: &Fake, folder: &str, id: &str) -> Vec<(u32, Vec<String>)> {
    fake.world().mailboxes[folder]
        .messages
        .iter()
        .filter(|(_, message)| message.message_id == id)
        .map(|(uid, message)| (*uid, message.flags.clone()))
        .collect()
}

fn request<'a>(
    folder: &'a str,
    bytes: &'a [u8],
    id: &'a str,
    mark: Mark,
    retry: bool,
) -> Append<'a> {
    Append {
        folder,
        message: bytes,
        message_id: id,
        flags: Flags::SEEN,
        mark,
        retry,
    }
}

#[test]
fn the_sent_copy_is_appended_seen_with_its_uid() {
    runtime().block_on(async {
        for (flavor, uidplus) in [
            (Flavor::Qresync, true),
            (Flavor::Condstore, true),
            (Flavor::Rev1, false),
        ] {
            let fake = fake(flavor, uidplus).await;
            fake.world().append(SENT, 3, &["\\Seen"]);
            let mut conn = fake.connect().await;
            let mark = write::mark_target(&mut conn, SENT).await.unwrap();
            let bytes = message("sent.1@example.test", "hello");
            let appended = write::append(
                &mut conn,
                request(SENT, &bytes, "<sent.1@example.test>", mark, false),
            )
            .await
            .unwrap();
            let held = copies(&fake, SENT, "sent.1@example.test");
            assert_eq!(held.len(), 1, "{flavor:?}");
            assert_eq!(held[0].1, ["\\Seen"]);
            assert_eq!(
                appended,
                Appended {
                    uidvalidity: Some(fake.world().mailboxes[SENT].uidvalidity),
                    uid: Some(held[0].0),
                    earlier: false,
                },
                "{flavor:?}"
            );
            let commands = fake.world().commands.clone();
            assert!(
                commands
                    .iter()
                    .any(|c| c.starts_with(&format!("APPEND \"{SENT}\" (\\Seen) {{"))),
                "{commands:?}"
            );
            // Without APPENDUID the UID comes from the Message-IDs from the mark up.
            assert_eq!(
                commands.iter().any(|c| c.starts_with("UID FETCH")),
                !uidplus,
                "{flavor:?}"
            );
        }
    });
}

/// One mid-APPEND fault, then the retry with the mark: one copy, its UID known.
async fn fault_then_retry(flavor: Flavor, uidplus: bool, arm: impl FnOnce(&Fake, usize)) {
    let fake = fake(flavor, uidplus).await;
    fake.world().append(SENT, 1, &["\\Seen"]);
    let id = "fault.1@example.test";
    let bytes = message(id, &"a long line of synthetic text\r\n".repeat(200));
    let mut conn = fake.connect().await;
    let mark = write::mark_target(&mut conn, SENT).await.unwrap();
    arm(&fake, bytes.len());
    let error = write::append(&mut conn, request(SENT, &bytes, id, mark, false))
        .await
        .unwrap_err();
    assert_eq!(error.failure(), WriteFailure::Retry, "{error}");
    assert!(error.is_connection());
    let after_cut = copies(&fake, SENT, id).len();
    let mut conn = fake.connect().await;
    let appended = write::append(&mut conn, request(SENT, &bytes, id, mark, true))
        .await
        .unwrap();
    let held = copies(&fake, SENT, id);
    assert_eq!(held.len(), 1, "{flavor:?}: no duplicate, nothing lost");
    assert_eq!(appended.uid, Some(held[0].0));
    assert_eq!(appended.earlier, after_cut == 1);
    let appends = fake
        .world()
        .commands
        .iter()
        .filter(|c| c.starts_with("APPEND"))
        .count();
    assert_eq!(appends, 1 + usize::from(after_cut == 0), "{flavor:?}");
}

#[test]
fn mid_append_cuts_are_retried_once_without_a_duplicate() {
    runtime().block_on(async {
        for (flavor, uidplus) in [
            (Flavor::Qresync, true),
            (Flavor::Condstore, true),
            (Flavor::Rev1, false),
        ] {
            fault_then_retry(flavor, uidplus, |fake, _| {
                fake.world().cut_before("APPEND");
            })
            .await;
            fault_then_retry(flavor, uidplus, |fake, size| {
                fake.world().cut_literal(size / 2);
            })
            .await;
            fault_then_retry(flavor, uidplus, |fake, _| {
                fake.world().lose_reply("APPEND");
            })
            .await;
        }
    });
}

#[test]
fn a_blind_replay_of_a_lost_append_reply_duplicates_and_the_retry_does_not() {
    runtime().block_on(async {
        let fake = fake(Flavor::Qresync, true).await;
        let id = "blind.1@example.test";
        let bytes = message(id, "blind");
        let mut conn = fake.connect().await;
        let mark = write::mark_target(&mut conn, SENT).await.unwrap();
        fake.world().lose_reply("APPEND");
        write::append(&mut conn, request(SENT, &bytes, id, mark, false))
            .await
            .unwrap_err();
        let mut conn = fake.connect().await;
        write::append(&mut conn, request(SENT, &bytes, id, mark, false))
            .await
            .unwrap();
        assert_eq!(copies(&fake, SENT, id).len(), 2, "the control duplicates");
    });
}

#[test]
fn a_retry_never_takes_an_older_revision_below_the_mark_for_this_one() {
    runtime().block_on(async {
        let fake = fake(Flavor::Qresync, true).await;
        let id = "draft.1@example.test";
        let mut conn = fake.connect().await;
        let first = message(id, "first revision");
        let mark = write::mark_target(&mut conn, DRAFTS).await.unwrap();
        let mut draft = request(DRAFTS, &first, id, mark, false);
        draft.flags = Flags::DRAFT.union(Flags::SEEN);
        let old = write::append(&mut conn, draft).await.unwrap();
        let second = message(id, "second revision");
        let mark = write::mark_target(&mut conn, DRAFTS).await.unwrap();
        fake.world().cut_before("APPEND");
        let mut draft = request(DRAFTS, &second, id, mark, false);
        draft.flags = Flags::DRAFT.union(Flags::SEEN);
        write::append(&mut conn, draft).await.unwrap_err();
        let mut conn = fake.connect().await;
        draft.retry = true;
        let new = write::append(&mut conn, draft).await.unwrap();
        assert!(!new.earlier, "the first revision is below the mark");
        assert_ne!(new.uid, old.uid);
        assert_eq!(copies(&fake, DRAFTS, id).len(), 2);
    });
}

#[test]
fn a_draft_revision_replaces_its_older_copies() {
    runtime().block_on(async {
        for (flavor, uidplus) in [(Flavor::Qresync, true), (Flavor::Rev1, false)] {
            let fake = fake(flavor, uidplus).await;
            let id = "draft.2@example.test";
            let other = fake.world().append(DRAFTS, 1, &["\\Draft", "\\Deleted"]);
            let mut conn = fake.connect().await;
            let flags = Flags::DRAFT.union(Flags::SEEN);
            let first = message(id, "first");
            let mark = write::mark_target(&mut conn, DRAFTS).await.unwrap();
            let replaced = write::replace_draft(
                &mut conn,
                Append {
                    flags,
                    ..request(DRAFTS, &first, id, mark, false)
                },
                None,
            )
            .await
            .unwrap();
            let old = replaced.appended.unwrap();
            let second = message(id, "second, longer");
            let mark = write::mark_target(&mut conn, DRAFTS).await.unwrap();
            let revision = Append {
                flags,
                ..request(DRAFTS, &second, id, mark, false)
            };
            let old_uids = [old.uid.unwrap()];
            let old_copies = OldCopies {
                uidvalidity: old.uidvalidity.unwrap(),
                uids: &old_uids,
            };
            if uidplus {
                fake.world().lose_reply("UID EXPUNGE");
            }
            let first_try = write::replace_draft(&mut conn, revision, Some(old_copies)).await;
            if uidplus {
                assert_eq!(first_try.unwrap_err().failure(), WriteFailure::Retry);
                let mut conn = fake.connect().await;
                let retried = write::replace_draft(
                    &mut conn,
                    Append {
                        retry: true,
                        ..revision
                    },
                    Some(old_copies),
                )
                .await
                .unwrap();
                assert!(retried.appended.unwrap().earlier);
                assert_eq!(retried.removed, old_uids);
            } else {
                let done = first_try.unwrap();
                assert_eq!(done.done.left_deleted, old_uids);
            }
            let held = copies(&fake, DRAFTS, id);
            let live: Vec<&(u32, Vec<String>)> = held
                .iter()
                .filter(|(_, flags)| !flags.iter().any(|f| f == "\\Deleted"))
                .collect();
            assert_eq!(live.len(), 1, "{flavor:?}: one live revision");
            assert_eq!(live[0].1, ["\\Seen", "\\Draft"]);
            assert_eq!(held.len(), if uidplus { 1 } else { 2 }, "{flavor:?}");
            assert!(
                fake.world().mailboxes[DRAFTS].messages.contains_key(&other),
                "another client's \\Deleted message is never expunged"
            );
            let commands = fake.world().commands.clone();
            assert!(
                !commands.iter().any(|c| c == "EXPUNGE" || c == "CLOSE"),
                "{commands:?}"
            );
        }
    });
}

#[test]
fn refusals_before_the_literal_are_classified_and_send_nothing() {
    runtime().block_on(async {
        let fake = fake(Flavor::Qresync, true).await;
        let bytes = message("refused.1@example.test", "x");
        let mut conn = fake.connect().await;
        let mark = write::mark_target(&mut conn, SENT).await.unwrap();
        for (reply, failure) in [
            ("NO [OVERQUOTA] Quota exceeded", WriteFailure::QuotaFull),
            ("NO [TOOBIG] Too large", WriteFailure::Refuse),
            ("NO Temporary failure", WriteFailure::Retry),
            ("BAD Syntax", WriteFailure::Refuse),
        ] {
            fake.world().refuse("APPEND", reply);
            let error = write::append(
                &mut conn,
                request(SENT, &bytes, "refused.1@example.test", mark, false),
            )
            .await
            .unwrap_err();
            assert_eq!(error.failure(), failure, "{reply}");
            assert!(!error.is_connection());
            assert!(copies(&fake, SENT, "refused.1@example.test").is_empty());
        }
        let error = write::append(
            &mut conn,
            request("Missing", &bytes, "refused.1@example.test", mark, false),
        )
        .await
        .unwrap_err();
        assert_eq!(error.failure(), WriteFailure::Refuse);
        let error = write::append(&mut conn, request(SENT, &bytes, "", mark, false))
            .await
            .unwrap_err();
        assert!(matches!(error, WriteError::Imap(_)));
        assert_eq!(
            fake.world().count("APPEND"),
            5,
            "no APPEND without a Message-ID"
        );
    });
}

#[test]
fn a_message_is_found_by_message_id_where_search_cant() {
    runtime().block_on(async {
        for misses in [false, true] {
            let fake = fake(Flavor::Qresync, true).await;
            {
                let mut world = fake.world();
                world.header_search_misses = misses;
                for n in 0..300 {
                    world.append_id(SENT, &format!("old.{n}@example.test"), 2, &["\\Seen"]);
                }
                world.append_id(SENT, "find.1@example.test.longer", 1, &["\\Seen"]);
            }
            let mut conn = fake.connect().await;
            let mark = write::mark_target(&mut conn, SENT).await.unwrap();
            let target = fake
                .world()
                .append_id(SENT, "find.1@example.test", 0, &["\\Seen"]);
            let uidvalidity = fake.world().mailboxes[SENT].uidvalidity;
            let found = write::find(&mut conn, SENT, "find.1@example.test", None)
                .await
                .unwrap();
            assert_eq!(found.uids, [target], "misses {misses}");
            assert_eq!(found.uidvalidity, uidvalidity);
            let marked = write::find(&mut conn, SENT, "<find.1@example.test>", Some(mark))
                .await
                .unwrap();
            assert_eq!(marked.uids, [target]);
            let none = write::find(&mut conn, SENT, "absent@example.test", Some(mark))
                .await
                .unwrap();
            assert!(none.uids.is_empty());
            // An old message beyond the newest 200 isn't found once SEARCH misses.
            let old = write::find(&mut conn, SENT, "old.3@example.test", None)
                .await
                .unwrap();
            assert_eq!(old.uids.len(), usize::from(!misses), "misses {misses}");
        }
    });
}
