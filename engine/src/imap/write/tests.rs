use super::*;
use crate::imap::error::ServerText;

/// The UIDs a UID set names.
fn expand(set: &str) -> Vec<u32> {
    set.split(',')
        .flat_map(|part| match part.split_once(':') {
            Some((a, b)) => (a.parse::<u32>().unwrap()..=b.parse().unwrap()).collect::<Vec<_>>(),
            None => vec![part.parse().unwrap()],
        })
        .collect()
}

fn check(uids: &[u32]) -> Vec<Batch> {
    let out = batches(uids);
    let mut all = Vec::new();
    for batch in &out {
        assert!(!batch.uids.is_empty());
        assert!(batch.uids.len() <= BATCH_UIDS, "{} UIDs", batch.uids.len());
        assert!(batch.set.len() <= BATCH_SET, "{} bytes", batch.set.len());
        assert_eq!(expand(&batch.set), batch.uids);
        all.extend_from_slice(&batch.uids);
    }
    assert_eq!(all, unique(uids.iter().copied()));
    out
}

#[test]
fn uid_sets_are_compact_and_split_by_count_and_length() {
    assert_eq!(check(&[9, 1, 2, 3, 7, 10, 3, 0])[0].set, "1:3,7,9:10");
    assert!(check(&[]).is_empty());
    assert_eq!(
        check(&[u32::MAX - 1, u32::MAX])[0].set,
        "4294967294:4294967295"
    );
    let contiguous: Vec<u32> = (1..=2_500).collect();
    let sets: Vec<String> = check(&contiguous).into_iter().map(|b| b.set).collect();
    assert_eq!(sets, ["1:1000", "1001:2000", "2001:2500"]);
    let sparse: Vec<u32> = (0..3_000).map(|n| 1_000_000 + n * 2).collect();
    let out = check(&sparse);
    assert!(out.len() >= 3 && out.iter().all(|b| b.set.len() > 3_000));
    let mixed: Vec<u32> = (1..20_000).filter(|n| n % 7 != 0 && n % 11 != 3).collect();
    check(&mixed);
}

fn selected(permanent: Option<&[&str]>) -> Selected {
    Selected {
        uidvalidity: 1,
        uidnext: Some(10),
        exists: 9,
        permanent: permanent.map(|flags| flags.iter().map(|f| f.to_ascii_lowercase()).collect()),
    }
}

#[test]
fn keywords_are_kept_where_permanentflags_allows() {
    let all = Flags::SEEN | Flags::FLAGGED | Flags::JUNK | Flags::NOT_JUNK;
    let anything = selected(Some(&["\\Seen", "\\Flagged", "\\Deleted", "\\*"]));
    assert_eq!(anything.kept(all), all);
    let system = selected(Some(&["\\Seen", "\\Flagged", "\\Deleted"]));
    assert_eq!(system.kept(all), Flags::SEEN | Flags::FLAGGED);
    let listed = selected(Some(&["\\Seen", "$junk"]));
    assert_eq!(listed.kept(all), Flags::SEEN | Flags::JUNK);
    assert!(!listed.keeps("\\Flagged"));
    assert_eq!(selected(None).kept(all), all);
    assert_eq!(selected(Some(&[])).kept(all), Flags::NONE);
    assert!(!selected(Some(&["\\*"])).keeps("\\Seen"));
}

fn no(text: &str) -> WriteError {
    WriteError::Imap(ImapError::No(ServerText(text.to_owned())))
}

#[test]
fn failures_are_classified_without_server_text() {
    for (error, expected) in [
        (
            no("[TRYCREATE] Mailbox doesn't exist"),
            WriteFailure::Refuse,
        ),
        (no("[NONEXISTENT] Unknown"), WriteFailure::Refuse),
        (no("[READ-ONLY] Read-only mailbox"), WriteFailure::Refuse),
        (no("[CANNOT] Same mailbox"), WriteFailure::Refuse),
        (no("[NOPERM] Denied"), WriteFailure::Refuse),
        (no("[OVERQUOTA] Quota exceeded"), WriteFailure::QuotaFull),
        (no("[overquota] Quota"), WriteFailure::QuotaFull),
        (no("[UNAVAILABLE] Try later"), WriteFailure::Retry),
        (no("[INUSE] Locked"), WriteFailure::Retry),
        (no("[SERVERBUG] Oops"), WriteFailure::Retry),
        (no("Internal error"), WriteFailure::Retry),
        (
            WriteError::Imap(ImapError::Bad(ServerText("Error in IMAP command".into()))),
            WriteFailure::Refuse,
        ),
        (
            WriteError::Imap(ImapError::Bye(ServerText("[UNAVAILABLE] bye".into()))),
            WriteFailure::Retry,
        ),
        (WriteError::Imap(ImapError::Timeout), WriteFailure::Retry),
        (
            WriteError::Imap(ImapError::ConnectionLost),
            WriteFailure::Retry,
        ),
        (WriteError::ReadOnly, WriteFailure::Refuse),
        (
            WriteError::UidValidity { current: 7 },
            WriteFailure::Retarget(7),
        ),
    ] {
        assert_eq!(error.failure(), expected, "{error:?}");
    }
    let error = no("[TRYCREATE] Mailbox S&APg-ppelpost: test@example.test");
    assert!(!format!("{error} {error:?}").contains("example.test"));
    assert!(!no("[OVERQUOTA] full").is_connection());
    assert!(WriteError::Imap(ImapError::ConnectionLost).is_connection());
    assert!(!WriteError::ReadOnly.is_connection());
}

#[test]
fn a_window_pairs_gone_messages_first_and_each_target_once() {
    let messages = [
        Moving {
            uid: 5,
            message_id: Some("<twin@example.test>"),
        },
        Moving {
            uid: 6,
            message_id: Some("twin@example.test"),
        },
        Moving {
            uid: 7,
            message_id: Some("<single@example.test>"),
        },
        Moving {
            uid: 8,
            message_id: None,
        },
        Moving {
            uid: 9,
            message_id: Some("<elsewhere@example.test>"),
        },
    ];
    let window = [
        (40, "twin@example.test".to_owned()),
        (41, "single@example.test".to_owned()),
        (42, "unrelated@example.test".to_owned()),
    ];
    // 6 is gone from the source: it is the twin an earlier attempt moved; 5 is still there.
    let present = BTreeSet::from([5, 7, 8, 9]);
    let mut claimed = HashSet::new();
    assert_eq!(
        assign(&messages, &present, &window, &mut claimed),
        vec![(6, 40), (7, 41)]
    );
    assert_eq!(claimed, HashSet::from([40, 41]));
    assert_eq!(
        assign(&messages, &BTreeSet::new(), &window, &mut claimed),
        vec![]
    );
}

#[test]
fn flag_names() {
    assert_eq!(
        names(Flags::SEEN | Flags::JUNK | Flags::NOT_JUNK),
        ["\\Seen", "$Junk", "$NotJunk"]
    );
    assert!(names(Flags::NONE).is_empty());
}
