//! The op queue's changes on the server (ML-051, ML-052) against the fake IMAP server, in CI: a
//! read-write SELECT next to the sync's EXAMINE, flags and keywords by PERMANENTFLAGS, UID MOVE
//! with COPYUID untagged, tagged or absent, the COPY path with and without UIDPLUS, deleting for
//! good, a folder marked read, refusals by response code, a changed UIDVALIDITY, and S1's lost
//! replies of MOVE, COPY, the `\Deleted` STORE and UID EXPUNGE, retried. Synthetic data only.

mod fake;

use fake::{CopyUid, Fake, Flavor, NOW, World};
use sealwax_domain::model::{AccountId, Flags};
use sealwax_engine::imap::write::{
    self, Done, Mark, Move, Moving, OpFolder, WriteError, WriteFailure,
};
use sealwax_engine::imap::{ConnectOptions, Connection, FolderLocks, SyncOptions, sync_account};
use sealwax_infrastructure::store::{NewAccount, Store, StoreOptions};
use std::collections::BTreeSet;
use std::path::Path;

fn runtime() -> tokio::runtime::Runtime {
    sealwax_engine::runtime().unwrap()
}

/// A fake with INBOX, Arkiv and Søppelpost (on the wire `S&APg-ppelpost`), MOVE and UIDPLUS as
/// given.
async fn fake(flavor: Flavor, moves: bool, uidplus: bool) -> Fake {
    let fake = Fake::start(flavor).await;
    {
        let mut world = fake.world();
        world.moves = moves;
        world.uidplus = uidplus;
        world.create("INBOX", &[]);
        world.create("Arkiv", &["\\Archive"]);
        world.create("S&APg-ppelpost", &["\\Junk"]);
    }
    fake
}

fn folder<'a>(fake: &Fake, name: &'a str) -> OpFolder<'a> {
    OpFolder {
        name_raw: name,
        uidvalidity: fake.world().mailboxes[name].uidvalidity,
    }
}

/// UIDs and Message-IDs of a mailbox's messages.
fn listed(fake: &Fake, name: &str) -> Vec<(u32, String)> {
    fake.world().mailboxes[name]
        .messages
        .iter()
        .map(|(uid, message)| (*uid, message.message_id.clone()))
        .collect()
}

fn moving(messages: &[(u32, String)]) -> Vec<Moving<'_>> {
    messages
        .iter()
        .map(|(uid, id)| Moving {
            uid: *uid,
            message_id: Some(id.as_str()),
        })
        .collect()
}

fn request<'a>(
    from: OpFolder<'a>,
    to: &'a str,
    messages: &'a [Moving<'a>],
    mark: Option<Mark>,
    retry: bool,
) -> Move<'a> {
    Move {
        from,
        to,
        messages,
        add: Flags::NONE,
        remove: Flags::NONE,
        mark,
        retry,
    }
}

fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

/// Commands since `from`, without tags.
fn commands(fake: &Fake, from: usize) -> Vec<String> {
    fake.world().commands[from..].to_vec()
}

fn sent(fake: &Fake) -> usize {
    fake.world().commands.len()
}

#[test]
fn select_reads_the_folder_and_refuses_read_only() {
    runtime().block_on(async {
        let fake = fake(Flavor::Qresync, true, true).await;
        fake.world().append("INBOX", 1, &[]);
        let mut conn = fake.connect().await;
        let selected = write::select(&mut conn, "INBOX").await.unwrap();
        assert_eq!(
            selected.uidvalidity,
            fake.world().mailboxes["INBOX"].uidvalidity
        );
        assert_eq!((selected.uidnext, selected.exists), (Some(2), 1));
        assert!(selected.keeps("$Junk") && selected.keeps("\\Seen"));

        fake.world().permanent_flags = "\\Seen \\Deleted".into();
        let selected = write::select(&mut conn, "INBOX").await.unwrap();
        assert!(!selected.keeps("$Junk") && !selected.keeps("\\Flagged"));
        assert_eq!(selected.kept(Flags::SEEN | Flags::JUNK), Flags::SEEN);

        fake.world().read_only.insert("INBOX".into());
        let error = write::select(&mut conn, "INBOX").await.unwrap_err();
        assert!(matches!(error, WriteError::ReadOnly));
        assert_eq!(error.failure(), WriteFailure::Refuse);

        let error = write::select(&mut conn, "Gone").await.unwrap_err();
        assert_eq!(error.failure(), WriteFailure::Refuse);
        assert!(!error.is_connection());
    });
}

#[test]
fn flags_change_in_place_and_run_again_harmlessly() {
    runtime().block_on(async {
        let fake = fake(Flavor::Qresync, false, true).await;
        for _ in 0..5 {
            fake.world().append("INBOX", 1, &["\\Answered"]);
        }
        let mut conn = fake.connect().await;
        let inbox = folder(&fake, "INBOX");
        let from = sent(&fake);
        let add = Flags::SEEN | Flags::FLAGGED;
        let done = write::store_flags(&mut conn, inbox, &[1, 2, 3], add, Flags::NONE)
            .await
            .unwrap();
        assert_eq!(done, Done::default());
        assert_eq!(
            commands(&fake, from),
            [
                "SELECT \"INBOX\"",
                "UID STORE 1:3 +FLAGS.SILENT (\\Seen \\Flagged)"
            ]
        );
        for uid in 1..=3 {
            assert_eq!(
                fake.world().flags("INBOX", uid),
                ["\\Answered", "\\Seen", "\\Flagged"]
            );
        }
        assert_eq!(fake.world().flags("INBOX", 4), ["\\Answered"]);
        let modseq = fake.world().mailboxes["INBOX"].modseq;
        write::store_flags(&mut conn, inbox, &[3, 2, 1], add, Flags::NONE)
            .await
            .unwrap();
        assert_eq!(fake.world().mailboxes["INBOX"].modseq, modseq);

        write::store_flags(&mut conn, inbox, &[1, 5], Flags::FLAGGED, Flags::SEEN)
            .await
            .unwrap();
        assert_eq!(fake.world().flags("INBOX", 1), ["\\Answered", "\\Flagged"]);
        assert_eq!(fake.world().flags("INBOX", 5), ["\\Answered", "\\Flagged"]);

        let from = sent(&fake);
        let none = write::store_flags(&mut conn, inbox, &[], Flags::SEEN, Flags::NONE).await;
        assert_eq!(none.unwrap(), Done::default());
        let zero = write::store_flags(&mut conn, inbox, &[0], Flags::SEEN, Flags::NONE).await;
        assert_eq!(zero.unwrap(), Done::default());
        assert!(commands(&fake, from).is_empty());
    });
}

fn store() -> (tempfile::TempDir, Store, AccountId) {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
    std::fs::create_dir_all(&base).unwrap();
    let dir = tempfile::Builder::new()
        .prefix("write-")
        .tempdir_in(base)
        .unwrap();
    let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
    let account = store
        .add_account(NewAccount {
            name: "Fake".into(),
            address: "test@example.test".into(),
        })
        .recv_blocking()
        .unwrap()
        .value;
    (dir, store, account)
}

#[test]
fn a_write_after_a_sync_pass_selects_read_write_and_is_kept() {
    runtime().block_on(async {
        let fake = fake(Flavor::Condstore, false, true).await;
        fake.world().append_many("INBOX", 4, |_| 1);
        let (_dir, store, account) = store();
        let mut conn = fake.connect().await;
        let options = SyncOptions {
            now: Some(NOW),
            ..SyncOptions::default()
        };
        sync_account(
            &mut conn,
            &store,
            account,
            &options,
            &FolderLocks::default(),
            |_, _| {},
        )
        .await
        .unwrap();
        assert!(fake.world().count("EXAMINE \"INBOX\"") > 0);
        assert_eq!(fake.world().count("SELECT"), 0);
        write::store_flags(
            &mut conn,
            folder(&fake, "INBOX"),
            &[2],
            Flags::SEEN,
            Flags::NONE,
        )
        .await
        .unwrap();
        assert_eq!(fake.world().flags("INBOX", 2), ["\\Seen"]);
        // The sync's next pass EXAMINEs again; the write left nothing selected that matters.
        sync_account(
            &mut conn,
            &store,
            account,
            &options,
            &FolderLocks::default(),
            |_, _| {},
        )
        .await
        .unwrap();
        store.close().unwrap();
    });
}

#[test]
fn keywords_are_sent_only_where_permanentflags_keeps_them() {
    runtime().block_on(async {
        let fake = fake(Flavor::Qresync, false, true).await;
        fake.world().append("INBOX", 1, &[]);
        fake.world().permanent_flags = "\\Seen \\Flagged \\Deleted".into();
        let mut conn = fake.connect().await;
        let inbox = folder(&fake, "INBOX");
        let from = sent(&fake);
        let add = Flags::SEEN | Flags::JUNK;
        let done = write::store_flags(&mut conn, inbox, &[1], add, Flags::NOT_JUNK)
            .await
            .unwrap();
        assert_eq!(done.dropped, Flags::JUNK | Flags::NOT_JUNK);
        assert_eq!(
            commands(&fake, from)[1],
            "UID STORE 1 +FLAGS.SILENT (\\Seen)"
        );
        assert_eq!(fake.world().flags("INBOX", 1), ["\\Seen"]);

        fake.world().permanent_flags = "\\Seen $Junk".into();
        let done = write::store_flags(&mut conn, inbox, &[1], Flags::JUNK, Flags::NOT_JUNK)
            .await
            .unwrap();
        assert_eq!(done.dropped, Flags::NOT_JUNK);
        assert_eq!(fake.world().flags("INBOX", 1), ["\\Seen", "$Junk"]);
    });
}

#[test]
fn large_changes_are_split_under_the_command_length() {
    runtime().block_on(async {
        let fake = fake(Flavor::Qresync, true, true).await;
        fake.world().append_many("INBOX", 3_000, |_| 1);
        let mut conn = fake.connect().await;
        let inbox = folder(&fake, "INBOX");
        let odd: Vec<u32> = (1..=3_000).filter(|uid| uid % 2 == 1).collect();
        let from = sent(&fake);
        write::store_flags(&mut conn, inbox, &odd, Flags::FLAGGED, Flags::NONE)
            .await
            .unwrap();
        let stores = commands(&fake, from);
        assert!(stores.len() >= 3, "{} commands", stores.len());
        assert!(stores.iter().all(|command| command.len() < 4_100));
        for uid in 1..=3_000 {
            let flagged = fake.world().flags("INBOX", uid) == ["\\Flagged"];
            assert_eq!(flagged, uid % 2 == 1, "UID {uid}");
        }
        let messages = listed(&fake, "INBOX");
        let all = moving(&messages);
        let done = write::move_messages(&mut conn, request(inbox, "Arkiv", &all, None, false))
            .await
            .unwrap();
        assert_eq!(done.moved.len(), 3_000);
        assert_eq!(fake.world().count("UID MOVE"), 3);
        assert!(fake.world().mailboxes["INBOX"].messages.is_empty());
    });
}

/// Moves INBOX's messages to Arkiv on a server with MOVE and UIDPLUS, COPYUID as given.
async fn moved_with(copyuid: CopyUid) -> (Fake, Vec<(u32, String)>, Done) {
    let fake = fake(Flavor::Qresync, true, true).await;
    fake.world().copyuid = copyuid;
    fake.world().append("Arkiv", 1, &[]);
    for _ in 0..3 {
        fake.world().append("INBOX", 1, &["\\Seen"]);
    }
    let messages = listed(&fake, "INBOX");
    let mut conn = fake.connect().await;
    let done = write::move_messages(
        &mut conn,
        request(
            folder(&fake, "INBOX"),
            "Arkiv",
            &moving(&messages),
            None,
            false,
        ),
    )
    .await
    .unwrap();
    (fake, messages, done)
}

#[test]
fn uid_move_keeps_copyuid_from_an_untagged_or_the_tagged_ok() {
    runtime().block_on(async {
        for copyuid in [CopyUid::Untagged, CopyUid::Tagged] {
            let (fake, messages, done) = moved_with(copyuid).await;
            assert_eq!(done.moved, [(1, 2), (2, 3), (3, 4)], "{copyuid:?}");
            assert_eq!(
                done.target_uidvalidity,
                Some(fake.world().mailboxes["Arkiv"].uidvalidity)
            );
            assert!(done.gone.is_empty() && done.left_deleted.is_empty());
            assert!(fake.world().mailboxes["INBOX"].messages.is_empty());
            for (source, target) in &done.moved {
                let id = &messages.iter().find(|(uid, _)| uid == source).unwrap().1;
                assert_eq!(
                    &fake.world().mailboxes["Arkiv"].messages[target].message_id,
                    id
                );
            }
            assert_eq!(fake.world().count("UID COPY"), 0);
            assert_eq!(fake.world().count("EXPUNGE"), 0);
        }
        let (fake, _, done) = moved_with(CopyUid::Absent).await;
        assert!(done.moved.is_empty() && done.gone.is_empty());
        assert_eq!(fake.world().mailboxes["Arkiv"].messages.len(), 4);
    });
}

#[test]
fn a_move_of_uids_already_gone_counts_as_done() {
    runtime().block_on(async {
        let fake = fake(Flavor::Qresync, true, true).await;
        fake.world().append("INBOX", 1, &[]);
        let mut conn = fake.connect().await;
        let inbox = folder(&fake, "INBOX");
        let some = [
            Moving {
                uid: 1,
                message_id: None,
            },
            Moving {
                uid: 99,
                message_id: None,
            },
        ];
        let done = write::move_messages(&mut conn, request(inbox, "Arkiv", &some, None, false))
            .await
            .unwrap();
        assert_eq!((done.moved, done.gone), (vec![(1, 1)], vec![99]));
        let done = write::move_messages(&mut conn, request(inbox, "Arkiv", &some, None, false))
            .await
            .unwrap();
        assert_eq!((done.moved, done.gone), (vec![], vec![1, 99]));
        assert_eq!(fake.world().mailboxes["Arkiv"].messages.len(), 1);
    });
}

#[test]
fn junk_keywords_are_set_in_the_source_before_the_move() {
    runtime().block_on(async {
        let fake = fake(Flavor::Qresync, true, true).await;
        fake.world().append("INBOX", 1, &["$NotJunk"]);
        let messages = listed(&fake, "INBOX");
        let all = moving(&messages);
        let mut conn = fake.connect().await;
        let from = sent(&fake);
        let junk = Move {
            add: Flags::JUNK,
            remove: Flags::NOT_JUNK,
            ..request(folder(&fake, "INBOX"), "S&APg-ppelpost", &all, None, false)
        };
        let done = write::move_messages(&mut conn, junk).await.unwrap();
        assert_eq!(done.moved, [(1, 1)]);
        assert_eq!(fake.world().flags("S&APg-ppelpost", 1), ["$Junk"]);
        assert_eq!(
            commands(&fake, from),
            [
                "SELECT \"INBOX\"",
                "UID STORE 1 +FLAGS.SILENT ($Junk)",
                "UID STORE 1 -FLAGS.SILENT ($NotJunk)",
                "UID MOVE 1 \"S&APg-ppelpost\"",
            ]
        );
    });
}

#[test]
fn without_move_it_copies_flags_deleted_and_expunges_by_uid() {
    runtime().block_on(async {
        let fake = fake(Flavor::Condstore, false, true).await;
        fake.world().append("INBOX", 1, &[]);
        let other = fake.world().append("INBOX", 1, &["\\Deleted"]);
        fake.world().append("INBOX", 1, &[]);
        let messages: Vec<(u32, String)> = listed(&fake, "INBOX")
            .into_iter()
            .filter(|(uid, _)| *uid != other)
            .collect();
        let mut conn = fake.connect().await;
        let from = sent(&fake);
        let done = write::move_messages(
            &mut conn,
            request(
                folder(&fake, "INBOX"),
                "Arkiv",
                &moving(&messages),
                None,
                false,
            ),
        )
        .await
        .unwrap();
        assert_eq!(done.moved, [(1, 1), (3, 2)]);
        assert_eq!(
            commands(&fake, from),
            [
                "SELECT \"INBOX\"",
                "UID COPY 1,3 \"Arkiv\"",
                "UID STORE 1,3 +FLAGS.SILENT (\\Deleted)",
                "UID EXPUNGE 1,3",
            ]
        );
        // Another client's \Deleted message stays: only the op's UIDs are expunged.
        assert_eq!(fake.world().uids("INBOX"), [other]);
    });
}

#[test]
fn the_quirk_switch_takes_the_copy_path_on_a_server_with_move() {
    runtime().block_on(async {
        let fake = fake(Flavor::Qresync, true, true).await;
        fake.world().append("INBOX", 1, &[]);
        let messages = listed(&fake, "INBOX");
        let options = ConnectOptions {
            moves: false,
            ..ConnectOptions::default()
        };
        let mut conn = fake.connect_with(options).await;
        assert!(!conn.caps().has("MOVE"));
        write::move_messages(
            &mut conn,
            request(
                folder(&fake, "INBOX"),
                "Arkiv",
                &moving(&messages),
                None,
                false,
            ),
        )
        .await
        .unwrap();
        assert_eq!(fake.world().count("UID MOVE"), 0);
        assert_eq!(fake.world().count("UID COPY"), 1);
        assert_eq!(fake.world().mailboxes["Arkiv"].messages.len(), 1);
    });
}

#[test]
fn without_uidplus_the_source_copies_stay_flagged_deleted() {
    runtime().block_on(async {
        let fake = fake(Flavor::Rev1, false, false).await;
        fake.world().append("Arkiv", 1, &[]);
        fake.world().append("INBOX", 1, &[]);
        let other = fake.world().append("INBOX", 1, &["\\Deleted"]);
        fake.world().append("INBOX", 1, &["\\Seen"]);
        let messages: Vec<(u32, String)> = listed(&fake, "INBOX")
            .into_iter()
            .filter(|(uid, _)| *uid != other)
            .collect();
        let mut conn = fake.connect().await;
        assert!(!conn.caps().has("UIDPLUS"));
        let mark = write::mark_target(&mut conn, "Arkiv").await.unwrap();
        assert_eq!(mark.uidnext, 2);
        let done = write::move_messages(
            &mut conn,
            request(
                folder(&fake, "INBOX"),
                "Arkiv",
                &moving(&messages),
                Some(mark),
                false,
            ),
        )
        .await
        .unwrap();
        assert_eq!(done.left_deleted, [1, 3]);
        assert_eq!(
            done.moved,
            [(1, 2), (3, 3)],
            "found by Message-ID without COPYUID"
        );
        assert_eq!(fake.world().count("EXPUNGE"), 0);
        assert_eq!(fake.world().count("CLOSE"), 0);
        assert_eq!(fake.world().uids("INBOX"), [1, 2, 3]);
        for uid in [1, 3] {
            assert!(
                fake.world()
                    .flags("INBOX", uid)
                    .contains(&"\\Deleted".to_owned())
            );
        }
        let expunged = write::expunge(&mut conn, folder(&fake, "INBOX"), &[3])
            .await
            .unwrap();
        assert_eq!(expunged.left_deleted, [3]);
        assert_eq!(fake.world().count("EXPUNGE"), 0);
    });
}

#[test]
fn deleting_for_good_expunges_only_the_ops_messages() {
    runtime().block_on(async {
        let fake = fake(Flavor::Qresync, true, true).await;
        fake.world().append_many("INBOX", 4, |_| 1);
        fake.world().set_flags("INBOX", 4, &["\\Deleted"]);
        let mut conn = fake.connect().await;
        let from = sent(&fake);
        let done = write::expunge(&mut conn, folder(&fake, "INBOX"), &[1, 2, 7])
            .await
            .unwrap();
        assert!(done.left_deleted.is_empty());
        assert_eq!(fake.world().uids("INBOX"), [3, 4]);
        assert_eq!(
            commands(&fake, from)[1..],
            [
                "UID STORE 1:2,7 +FLAGS.SILENT (\\Deleted)",
                "UID EXPUNGE 1:2,7"
            ]
        );
        write::expunge(&mut conn, folder(&fake, "INBOX"), &[1, 2])
            .await
            .unwrap();
        assert_eq!(fake.world().uids("INBOX"), [3, 4]);
    });
}

#[test]
fn a_folder_marked_read_marks_every_unseen_message_and_lists_them() {
    runtime().block_on(async {
        let fake = fake(Flavor::Qresync, true, true).await;
        for n in 0..300 {
            let flags: &[&str] = if n % 3 == 0 { &["\\Seen"] } else { &[] };
            fake.world().append("INBOX", 400 - n, flags);
        }
        let mut conn = fake.connect().await;
        let inbox = folder(&fake, "INBOX");
        let done = write::mark_folder_read(&mut conn, inbox, Some(250))
            .await
            .unwrap();
        let expected: Vec<u32> = (1..=250).filter(|uid| (uid - 1) % 3 != 0).collect();
        assert_eq!(done.marked, expected);
        assert!(
            fake.world()
                .commands
                .contains(&"UID SEARCH UNSEEN UID 1:250".to_owned())
        );
        let unseen = |fake: &Fake| {
            fake.world().mailboxes["INBOX"]
                .messages
                .iter()
                .filter(|(_, m)| !m.flags.contains(&"\\Seen".to_owned()))
                .map(|(uid, _)| *uid)
                .collect::<Vec<_>>()
        };
        assert!(unseen(&fake).iter().all(|uid| *uid > 250));
        let done = write::mark_folder_read(&mut conn, inbox, None)
            .await
            .unwrap();
        let later: Vec<u32> = (251..=300).filter(|uid| (uid - 1) % 3 != 0).collect();
        assert_eq!(done.marked, later);
        assert!(unseen(&fake).is_empty());
        let from = sent(&fake);
        let again = write::mark_folder_read(&mut conn, inbox, None)
            .await
            .unwrap();
        assert!(again.marked.is_empty());
        assert_eq!(
            commands(&fake, from),
            ["SELECT \"INBOX\"", "UID SEARCH UNSEEN"]
        );
        let from = sent(&fake);
        let empty = folder(&fake, "Arkiv");
        assert!(
            write::mark_folder_read(&mut conn, empty, None)
                .await
                .unwrap()
                .marked
                .is_empty()
        );
        assert_eq!(commands(&fake, from), ["SELECT \"Arkiv\""]);
    });
}

#[test]
fn mark_target_asks_status_or_examines_the_selected_folder() {
    runtime().block_on(async {
        let fake = fake(Flavor::Qresync, true, true).await;
        fake.world().append_many("Arkiv", 3, |_| 1);
        let validity = fake.world().mailboxes["Arkiv"].uidvalidity;
        let mut conn = fake.connect().await;
        let expected = Mark {
            uidvalidity: Some(validity),
            uidnext: 4,
        };
        let from = sent(&fake);
        assert_eq!(
            write::mark_target(&mut conn, "Arkiv").await.unwrap(),
            expected
        );
        assert_eq!(
            commands(&fake, from),
            ["STATUS \"Arkiv\" (UIDNEXT UIDVALIDITY)"]
        );
        write::select(&mut conn, "Arkiv").await.unwrap();
        let from = sent(&fake);
        assert_eq!(
            write::mark_target(&mut conn, "Arkiv").await.unwrap(),
            expected
        );
        assert!(commands(&fake, from)[0].starts_with("EXAMINE"));
        let error = write::mark_target(&mut conn, "Gone").await.unwrap_err();
        assert_eq!(error.failure(), WriteFailure::Refuse);
    });
}

#[test]
fn refusals_are_classified_by_their_response_code() {
    runtime().block_on(async {
        for (reply, expected) in [
            ("NO [OVERQUOTA] Quota exceeded", WriteFailure::QuotaFull),
            ("NO [UNAVAILABLE] Try again", WriteFailure::Retry),
            ("NO [INUSE] Mailbox in use", WriteFailure::Retry),
            ("NO Something went wrong", WriteFailure::Retry),
            ("NO [CANNOT] Not like this", WriteFailure::Refuse),
            ("NO [NONEXISTENT] Gone", WriteFailure::Refuse),
            ("NO [TRYCREATE] Gone", WriteFailure::Refuse),
            ("NO [READ-ONLY] Read-only", WriteFailure::Refuse),
            ("NO [NOPERM] Denied", WriteFailure::Refuse),
            ("BAD Error in command", WriteFailure::Refuse),
        ] {
            let fake = fake(Flavor::Qresync, true, true).await;
            fake.world().append("INBOX", 1, &[]);
            let messages = listed(&fake, "INBOX");
            let all = moving(&messages);
            let mut conn = fake.connect().await;
            fake.world().refuse("UID MOVE", reply);
            let error = write::move_messages(
                &mut conn,
                request(folder(&fake, "INBOX"), "Arkiv", &all, None, false),
            )
            .await
            .unwrap_err();
            assert_eq!(error.failure(), expected, "{reply}");
            assert!(!error.is_connection(), "{reply}");
            assert!(!format!("{error} {error:?}").contains("Quota"), "{reply}");
            fake.world().refuse("SELECT", reply);
            let error = write::store_flags(
                &mut conn,
                folder(&fake, "INBOX"),
                &[1],
                Flags::SEEN,
                Flags::NONE,
            )
            .await
            .unwrap_err();
            assert_eq!(error.failure(), expected, "{reply} on SELECT");
        }
        let fake = fake(Flavor::Qresync, true, true).await;
        fake.world().append("INBOX", 1, &[]);
        let mut conn = fake.connect().await;
        fake.world()
            .refuse("UID STORE", "* BYE [UNAVAILABLE] Shutting down");
        let error = write::store_flags(
            &mut conn,
            folder(&fake, "INBOX"),
            &[1],
            Flags::SEEN,
            Flags::NONE,
        )
        .await
        .unwrap_err();
        assert_eq!(error.failure(), WriteFailure::Retry);
        assert!(error.is_connection());
    });
}

#[test]
fn a_move_into_a_missing_folder_is_refused_on_either_path() {
    runtime().block_on(async {
        for (moves, uidplus) in [(true, true), (false, true), (false, false)] {
            let fake = fake(Flavor::Qresync, moves, uidplus).await;
            fake.world().append("INBOX", 1, &[]);
            let messages = listed(&fake, "INBOX");
            let all = moving(&messages);
            let mut conn = fake.connect().await;
            let error = write::move_messages(
                &mut conn,
                request(folder(&fake, "INBOX"), "Gone", &all, None, false),
            )
            .await
            .unwrap_err();
            assert_eq!(error.failure(), WriteFailure::Refuse, "MOVE {moves}");
            assert_eq!(fake.world().uids("INBOX"), [1]);
            assert!(
                !fake
                    .world()
                    .flags("INBOX", 1)
                    .contains(&"\\Deleted".to_owned())
            );
        }
    });
}

#[test]
fn a_changed_uidvalidity_is_reported_for_retargeting() {
    runtime().block_on(async {
        let fake = fake(Flavor::Qresync, true, true).await;
        fake.world().append("INBOX", 1, &[]);
        let stale = folder(&fake, "INBOX");
        fake.world().recreate("INBOX");
        fake.world().append("INBOX", 1, &[]);
        let current = fake.world().mailboxes["INBOX"].uidvalidity;
        let mut conn = fake.connect().await;
        let messages = listed(&fake, "INBOX");
        let all = moving(&messages);
        let from = sent(&fake);
        for error in [
            write::store_flags(&mut conn, stale, &[1], Flags::SEEN, Flags::NONE)
                .await
                .unwrap_err(),
            write::move_messages(&mut conn, request(stale, "Arkiv", &all, None, false))
                .await
                .unwrap_err(),
            write::expunge(&mut conn, stale, &[1]).await.unwrap_err(),
            write::mark_folder_read(&mut conn, stale, None)
                .await
                .unwrap_err(),
        ] {
            assert_eq!(error.failure(), WriteFailure::Retarget(current));
        }
        assert!(
            commands(&fake, from)
                .iter()
                .all(|c| c.starts_with("SELECT"))
        );
        assert!(fake.world().flags("INBOX", 1).is_empty());
    });
}

/// How a test server is set up for the lost-reply scenarios.
#[derive(Clone, Copy, Debug)]
struct Server {
    flavor: Flavor,
    moves: bool,
    uidplus: bool,
}

const SERVERS: [Server; 3] = [
    Server {
        flavor: Flavor::Qresync,
        moves: true,
        uidplus: true,
    },
    Server {
        flavor: Flavor::Condstore,
        moves: false,
        uidplus: true,
    },
    Server {
        flavor: Flavor::Rev1,
        moves: false,
        uidplus: false,
    },
];

/// Moves INBOX's five messages to Arkiv with `fault` armed, then again as a retry (or blindly)
/// on a new connection. Returns the first attempt's error, the second's result and the fake.
async fn interrupted(
    server: Server,
    fault: impl FnOnce(&mut World),
    retry: bool,
) -> (Option<WriteError>, Done, Fake, Vec<(u32, String)>) {
    let fake = fake(server.flavor, server.moves, server.uidplus).await;
    fake.world().append("Arkiv", 1, &[]);
    for _ in 0..5 {
        fake.world().append("INBOX", 1, &[]);
    }
    let messages = listed(&fake, "INBOX");
    let all = moving(&messages);
    let inbox = folder(&fake, "INBOX");
    let mut conn: Connection = fake.connect().await;
    let mark = write::mark_target(&mut conn, "Arkiv").await.unwrap();
    fault(&mut fake.world());
    let first = write::move_messages(&mut conn, request(inbox, "Arkiv", &all, Some(mark), false))
        .await
        .err();
    let mut conn = fake.connect().await;
    let second = write::move_messages(&mut conn, request(inbox, "Arkiv", &all, Some(mark), retry))
        .await
        .unwrap();
    (first, second, fake, messages)
}

/// Exactly one copy of each message in Arkiv besides the one already there, none left in INBOX
/// (or only flagged `\Deleted` without UIDPLUS), nothing lost.
fn exactly_once(server: Server, fake: &Fake, messages: &[(u32, String)], done: &Done) {
    let ids: Vec<String> = messages.iter().map(|(_, id)| id.clone()).collect();
    let arkiv = fake.world().ids("Arkiv");
    assert_eq!(arkiv.len(), ids.len() + 1, "{server:?}: {arkiv:?}");
    assert_eq!(
        sorted(arkiv[1..].to_vec()),
        sorted(ids.clone()),
        "{server:?}"
    );
    let inbox = fake.world().uids("INBOX");
    if server.uidplus {
        assert!(inbox.is_empty(), "{server:?}: {inbox:?}");
    } else {
        for uid in &inbox {
            assert!(
                fake.world()
                    .flags("INBOX", *uid)
                    .contains(&"\\Deleted".to_owned())
            );
        }
        assert_eq!(done.left_deleted, inbox, "{server:?}");
    }
    let targets: BTreeSet<u32> = done.moved.iter().map(|(_, target)| *target).collect();
    assert_eq!(
        targets.len(),
        done.moved.len(),
        "{server:?}: a target named twice"
    );
    for (source, target) in &done.moved {
        let id = &messages.iter().find(|(uid, _)| uid == source).unwrap().1;
        assert_eq!(
            &fake.world().mailboxes["Arkiv"].messages[target].message_id,
            id
        );
    }
}

#[test]
fn lost_replies_of_move_copy_deleted_and_expunge_are_retried_exactly_once() {
    runtime().block_on(async {
        for server in SERVERS {
            let needles = match (server.moves, server.uidplus) {
                (true, _) => vec!["UID MOVE"],
                (false, true) => vec!["UID COPY", "\\Deleted)", "UID EXPUNGE"],
                (false, false) => vec!["UID COPY", "\\Deleted)"],
            };
            for needle in needles {
                let (first, done, fake, messages) =
                    interrupted(server, |world| world.lose_reply(needle), true).await;
                let first = first.unwrap();
                assert_eq!(first.failure(), WriteFailure::Retry, "{server:?} {needle}");
                assert!(first.is_connection());
                exactly_once(server, &fake, &messages, &done);
                assert_eq!(done.moved.len(), 5, "{server:?} {needle}: {done:?}");
            }
            let (first, done, fake, messages) =
                interrupted(server, |world| world.cut_before("UID "), true).await;
            assert!(first.unwrap().is_connection());
            exactly_once(server, &fake, &messages, &done);
        }
    });
}

#[test]
fn a_blind_replay_of_a_copy_based_move_duplicates_and_the_retry_does_not() {
    runtime().block_on(async {
        let server = SERVERS[1];
        let (_, _, fake, _) =
            interrupted(server, |world| world.lose_reply("UID COPY"), false).await;
        assert_eq!(
            fake.world().ids("Arkiv").len(),
            11,
            "the control: two copies of each"
        );
        let (_, done, fake, messages) =
            interrupted(server, |world| world.lose_reply("UID COPY"), true).await;
        exactly_once(server, &fake, &messages, &done);
    });
}

#[test]
fn a_move_retry_finds_a_half_done_move_and_only_deletes_the_source() {
    runtime().block_on(async {
        let fake = fake(Flavor::Qresync, true, true).await;
        for _ in 0..3 {
            fake.world().append("INBOX", 1, &[]);
        }
        let messages = listed(&fake, "INBOX");
        let all = moving(&messages);
        let mut conn = fake.connect().await;
        let mark = write::mark_target(&mut conn, "Arkiv").await.unwrap();
        // An earlier attempt copied message 2 and died before the source's expunge.
        fake.world().append_id("Arkiv", &messages[1].1, 1, &[]);
        let done = write::move_messages(
            &mut conn,
            request(folder(&fake, "INBOX"), "Arkiv", &all, Some(mark), true),
        )
        .await
        .unwrap();
        assert_eq!(done.moved, [(1, 2), (2, 1), (3, 3)]);
        assert_eq!(
            sorted(fake.world().ids("Arkiv")),
            sorted(messages.iter().map(|m| m.1.clone()).collect())
        );
        assert!(fake.world().uids("INBOX").is_empty());
        assert_eq!(fake.world().count("UID MOVE 1,3"), 1);
    });
}

#[test]
fn a_copy_retry_without_a_mark_looks_through_the_whole_target() {
    runtime().block_on(async {
        let server = SERVERS[1];
        let fake = fake(server.flavor, server.moves, server.uidplus).await;
        for _ in 0..2 {
            fake.world().append("INBOX", 1, &[]);
        }
        let messages = listed(&fake, "INBOX");
        let all = moving(&messages);
        let inbox = folder(&fake, "INBOX");
        let mut conn = fake.connect().await;
        fake.world().lose_reply("UID COPY");
        write::move_messages(&mut conn, request(inbox, "Arkiv", &all, None, false))
            .await
            .unwrap_err();
        let mut conn = fake.connect().await;
        let done = write::move_messages(&mut conn, request(inbox, "Arkiv", &all, None, true))
            .await
            .unwrap();
        assert_eq!(fake.world().ids("Arkiv").len(), 2);
        assert_eq!(done.moved.len(), 2);
        assert!(
            fake.world()
                .commands
                .contains(&"UID FETCH 1:* (UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])".to_owned())
        );
    });
}
