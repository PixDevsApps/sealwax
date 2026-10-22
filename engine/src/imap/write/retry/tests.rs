use super::*;
use crate::imap::{Credentials, connect, write};
use crate::net::testing::{Peer, bind, test_ca};
use sealwax_domain::account::{Endpoint, Security};
use sealwax_domain::model::Flags;

#[derive(Clone, Copy)]
enum BodyReply {
    Complete,
    WrongOffset,
    Missing,
    Refused,
    SourceReset,
}

/// A source and an unrelated destination deliberately share their Message-ID. The fake server
/// accepts deletion even if no copy happened, so the test can expose loss rather than merely
/// rejecting the old client's command sequence.
async fn scenario(native_move: bool, same: bool, reply: BodyReply, padding: usize) {
    let ca = test_ca();
    let (listener, port) = bind().await;
    let config = ca.server.clone();
    let source = format!(
        "Message-ID: <shared@example.test>\r\n\r\n{}original",
        "x".repeat(padding)
    );
    let within_budget = source.len() <= MAX_COMPARE_BYTES as usize;
    // Equal lengths and equal headers: metadata comparisons would not catch the difference.
    let target = if same {
        source.clone()
    } else {
        source.replace("original", "distinct")
    };
    let server = tokio::spawn(async move {
        let mut peer = Peer::accept_tls(&listener, config).await;
        peer.send("* OK [CAPABILITY IMAP4rev1 AUTH=PLAIN] ready\r\n")
            .await;
        let tag = peer.expect("AUTHENTICATE PLAIN").await;
        peer.send("+ \r\n").await;
        peer.line().await.unwrap();
        peer.send(&format!("{tag} OK authenticated\r\n")).await;
        let tag = peer.expect("CAPABILITY").await;
        peer.send(&format!(
            "* CAPABILITY IMAP4rev1 UIDPLUS{}\r\n{tag} OK capabilities\r\n",
            if native_move { " MOVE" } else { "" }
        ))
        .await;
        let mut folder = String::new();
        let mut transferred = false;
        let mut deleted = false;
        let mut partial_reads = 0;
        let mut source_selects = 0;
        while let Some(line) = peer.line().await {
            let (tag, command) = line.split_once(' ').unwrap();
            if command.starts_with("SELECT ") || command.starts_with("EXAMINE ") {
                folder = if command.contains("INBOX") {
                    "INBOX"
                } else {
                    "Archive"
                }
                .to_owned();
                let validity = if folder == "INBOX" {
                    if command.starts_with("SELECT ") {
                        source_selects += 1;
                    }
                    if matches!(reply, BodyReply::SourceReset) && source_selects > 1 {
                        8
                    } else {
                        7
                    }
                } else {
                    9
                };
                peer.send(&format!("* 1 EXISTS\r\n* OK [UIDVALIDITY {validity}] validity\r\n* OK [UIDNEXT 41] next\r\n{tag} OK selected\r\n")).await;
            } else if command == "UID SEARCH UID 5" {
                peer.send(&format!("* SEARCH 5\r\n{tag} OK search\r\n"))
                    .await;
            } else if command.contains("HEADER.FIELDS (MESSAGE-ID)") {
                let header = "Message-ID: <shared@example.test>\r\n\r\n";
                peer.send(&format!("* 1 FETCH (UID 40 BODY[HEADER.FIELDS (MESSAGE-ID)] {{{}}}\r\n{header})\r\n{tag} OK fetched\r\n", header.len())).await;
            } else if command.contains("BODY.PEEK[]<") {
                partial_reads += 1;
                if matches!(reply, BodyReply::Refused) {
                    peer.send(&format!("{tag} NO cannot read\r\n")).await;
                    continue;
                }
                if matches!(reply, BodyReply::Missing) {
                    peer.send(&format!("{tag} OK vanished\r\n")).await;
                    continue;
                }
                let range = command
                    .split_once('<')
                    .unwrap()
                    .1
                    .split_once('>')
                    .unwrap()
                    .0;
                let (offset, count) = range.split_once('.').unwrap();
                let offset: usize = offset.parse().unwrap();
                let count: usize = count.parse().unwrap();
                let (uid, bytes) = if folder == "INBOX" {
                    (5, &source)
                } else {
                    (40, &target)
                };
                let bytes = &bytes[offset.min(bytes.len())..(offset + count).min(bytes.len())];
                let returned_offset = if matches!(reply, BodyReply::WrongOffset) {
                    offset + 1
                } else {
                    offset
                };
                peer.send(&format!("* 1 FETCH (UID {uid} BODY[]<{returned_offset}> {{{}}}\r\n{bytes})\r\n{tag} OK body\r\n", bytes.len())).await;
            } else if command.starts_with("UID MOVE 5 ") || command.starts_with("UID COPY 5 ") {
                assert_eq!(
                    folder, "INBOX",
                    "must restore the source before transferring"
                );
                transferred = true;
                deleted |= native_move;
                peer.send(&format!("{tag} OK [COPYUID 9 5 41] transferred\r\n"))
                    .await;
            } else if command == "UID STORE 5 +FLAGS.SILENT (\\Deleted)" {
                assert_eq!(folder, "INBOX", "must restore the source before deleting");
                deleted = true;
                peer.send(&format!("{tag} OK deleted\r\n")).await;
            } else if command == "UID EXPUNGE 5" {
                peer.send(&format!("{tag} OK expunged\r\n")).await;
            } else {
                panic!("unexpected command: {command}");
            }
        }
        (transferred, deleted, partial_reads)
    });
    let mut conn = connect(
        &Endpoint {
            host: "127.0.0.1".into(),
            port,
            security: Security::Tls,
            certificate: None,
        },
        Credentials::Password {
            username: "user",
            password: "test-password",
        },
        ca.trust.client_config().unwrap(),
    )
    .await
    .unwrap();
    let messages = [write::Moving {
        uid: 5,
        message_id: Some("shared@example.test"),
    }];
    let result = write::move_messages(
        &mut conn,
        write::Move {
            from: OpFolder {
                name_raw: "INBOX",
                uidvalidity: 7,
            },
            to: "Archive",
            messages: &messages,
            add: Flags::NONE,
            remove: Flags::NONE,
            mark: Some(write::Mark {
                uidvalidity: Some(9),
                uidnext: 40,
            }),
            retry: true,
        },
    )
    .await;
    drop(conn);
    let (transferred, deleted, reads) = server.await.unwrap();
    if matches!(reply, BodyReply::Refused | BodyReply::SourceReset) {
        assert!(result.is_err());
        assert!(
            !transferred && !deleted,
            "a failed comparison must not delete the source"
        );
    } else {
        let done = result.unwrap();
        let verified = same && within_budget && matches!(reply, BodyReply::Complete);
        assert_eq!(
            transferred, !verified,
            "distinct or unverified mail must actually be transferred"
        );
        assert!(deleted);
        assert_eq!(done.moved, vec![(5, if verified { 40 } else { 41 })]);
    }
    assert!(
        reads > 0,
        "Message-ID alone must never authorize source deletion"
    );
    if padding >= CHUNK_BYTES as usize && matches!(reply, BodyReply::Complete) {
        assert!(
            reads >= 4,
            "the comparison must include bytes after the first chunk"
        );
    }
}

#[tokio::test]
async fn different_contents_with_same_id_are_transferred_before_source_deletion() {
    for native_move in [false, true] {
        scenario(native_move, false, BodyReply::Complete, 0).await;
    }
}

#[tokio::test]
async fn identical_contents_keep_retry_deduplication() {
    for native_move in [false, true] {
        scenario(native_move, true, BodyReply::Complete, 0).await;
    }
}

#[tokio::test]
async fn differences_after_the_first_chunk_are_not_deduplicated() {
    scenario(false, false, BodyReply::Complete, CHUNK_BYTES as usize).await;
}

#[tokio::test]
async fn missing_or_misindexed_contents_do_not_authorize_deletion() {
    for reply in [BodyReply::WrongOffset, BodyReply::Missing] {
        scenario(false, true, reply, 0).await;
    }
}

#[tokio::test]
async fn refused_comparison_preserves_the_source() {
    scenario(false, true, BodyReply::Refused, 0).await;
}

#[tokio::test]
async fn messages_over_the_comparison_budget_are_transferred_again() {
    scenario(false, true, BodyReply::Complete, MAX_COMPARE_BYTES as usize).await;
}

#[tokio::test]
async fn identical_messages_exactly_at_the_budget_are_deduplicated() {
    let overhead = "Message-ID: <shared@example.test>\r\n\r\noriginal".len();
    scenario(
        false,
        true,
        BodyReply::Complete,
        MAX_COMPARE_BYTES as usize - overhead,
    )
    .await;
}

#[tokio::test]
async fn source_uidvalidity_is_checked_again_after_comparison() {
    scenario(false, true, BodyReply::SourceReset, 0).await;
}
