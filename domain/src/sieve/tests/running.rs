//! Running scripts: RFC 5228 section 2.10's keep and filing, RFC 5232's flags, every test, match
//! type and comparator, and RFC 6785's `imap.mailbox`.

use super::super::check::{Comparator, MatchType, Matcher};
use super::super::evaluate::matches;
use super::super::{FlagChange, flag_names};
use super::language::RFC_EXAMPLE;
use super::{Message, filed, run, strings};
use crate::model::Flags;

const FILEINTO: &str = "require [\"fileinto\", \"copy\", \"imap4flags\"];\n";

fn with_fileinto(body: &str) -> String {
    format!("{FILEINTO}{body}")
}

fn change(add: &[&str], remove: &[&str]) -> FlagChange {
    FlagChange {
        add: strings(add),
        remove: strings(remove),
    }
}

#[test]
fn an_empty_script_keeps() {
    let outcome = run("", &Message::new());
    assert!(outcome.keep && outcome.filed.is_empty() && !outcome.discard && !outcome.stopped);
    assert!(outcome.flags.is_empty() && !outcome.trashed());
}

#[test]
fn fileinto_cancels_the_implicit_keep_but_copy_does_not() {
    let moved = run(&with_fileinto("fileinto \"Kunder\";"), &Message::new());
    assert!(!moved.keep);
    assert_eq!(filed(&moved), ["Kunder"]);
    assert!(!moved.filed[0].copy);
    let copied = run(
        &with_fileinto("fileinto :copy \"Kunder\";"),
        &Message::new(),
    );
    assert!(copied.keep);
    assert_eq!(filed(&copied), ["Kunder"]);
    assert!(copied.filed[0].copy);
}

#[test]
fn discard_cancels_the_implicit_keep_only() {
    let discarded = run("discard;", &Message::new());
    assert!(!discarded.keep && discarded.discard && discarded.trashed());
    let kept = run("keep; discard;", &Message::new());
    assert!(kept.keep && kept.discard && !kept.trashed());
    let filed_too = run(&with_fileinto("fileinto \"A\"; discard;"), &Message::new());
    assert!(!filed_too.keep && !filed_too.trashed());
}

#[test]
fn several_fileintos_file_into_each_once_in_order() {
    let outcome = run(
        &with_fileinto(
            "fileinto :copy \"B\"; fileinto \"A\"; fileinto \"B\"; fileinto :copy \"C\"; fileinto \
             \"A\";",
        ),
        &Message::new(),
    );
    assert!(!outcome.keep);
    assert_eq!(filed(&outcome), ["B", "A", "C"]);
    let copies: Vec<bool> = outcome.filed.iter().map(|filed| filed.copy).collect();
    assert_eq!(copies, [false, false, true]);
}

#[test]
fn filing_into_the_folder_it_arrived_in_is_keep() {
    for folder in ["INBOX", "inbox", "Inbox"] {
        let outcome = run(
            &with_fileinto(&format!("fileinto \"{folder}\";")),
            &Message::new(),
        );
        assert!(outcome.keep && outcome.filed.is_empty(), "{folder}");
    }
    let outcome = run(
        &with_fileinto("fileinto \"Lists\"; fileinto \"lists\";"),
        &Message::new().mailbox("Lists"),
    );
    assert!(outcome.keep);
    assert_eq!(
        filed(&outcome),
        ["lists"],
        "names other than the Inbox's are exact"
    );
}

#[test]
fn stop_ends_the_script_from_any_depth() {
    let outcome = run(
        &with_fileinto(
            "if true { if true { fileinto \"A\"; stop; } fileinto \"B\"; } fileinto \"C\";",
        ),
        &Message::new(),
    );
    assert!(outcome.stopped);
    assert_eq!(filed(&outcome), ["A"]);
    let kept = run("stop; discard;", &Message::new());
    assert!(kept.keep && kept.stopped && !kept.discard);
}

#[test]
fn flags_start_as_the_messages_own() {
    let message = Message::new().flags(&["\\Seen", "$Forwarded", "\\Recent"]);
    let outcome = run(
        &with_fileinto(
            "addflag [\"\\\\Flagged\", \"$label1 \\\\seen\"]; removeflag \"$forwarded\";",
        ),
        &message,
    );
    assert!(outcome.keep);
    assert_eq!(
        outcome.flags,
        change(&["\\Flagged", "$label1"], &["$Forwarded"])
    );
    let set = run(&with_fileinto("setflag \"\\\\Answered\";"), &message);
    assert_eq!(
        set.flags,
        change(&["\\Answered"], &["\\Seen", "$Forwarded"])
    );
    let cleared = run(&with_fileinto("setflag \"\";"), &message);
    assert_eq!(cleared.flags, change(&[], &["\\Seen", "$Forwarded"]));
}

#[test]
fn each_delivery_takes_the_flags_of_its_moment() {
    let outcome = run(
        &with_fileinto(
            "addflag \"\\\\Seen\"; fileinto :copy \"A\"; addflag \"\\\\Flagged\"; fileinto \"B\"; \
             removeflag \"\\\\Seen\"; keep; addflag \"$late\";",
        ),
        &Message::new(),
    );
    assert_eq!(filed(&outcome), ["A", "B"]);
    assert_eq!(outcome.filed[0].flags, change(&["\\Seen"], &[]));
    assert_eq!(
        outcome.filed[1].flags,
        change(&["\\Seen", "\\Flagged"], &[])
    );
    assert!(outcome.keep);
    assert_eq!(
        outcome.flags,
        change(&["\\Flagged"], &[]),
        "as at the explicit keep"
    );
    let implicit = run(
        &with_fileinto("fileinto :copy \"A\"; addflag \"$late\";"),
        &Message::new(),
    );
    assert_eq!(
        implicit.flags,
        change(&["$late"], &[]),
        "the implicit keep comes last"
    );
    assert!(implicit.filed[0].flags.is_empty());
    let again = run(
        &with_fileinto("fileinto \"A\"; addflag \"\\\\Seen\"; fileinto \"A\";"),
        &Message::new(),
    );
    assert_eq!(
        again.filed[0].flags,
        change(&["\\Seen"], &[]),
        "the last filing's flags"
    );
}

#[test]
fn hasflag_reads_the_flags_so_far() {
    let script = with_fileinto(
        "addflag \"$Tagged\"; if hasflag :contains \"tagg\" { fileinto \"A\"; } if hasflag \
         [\"\\\\Seen\", \"\\\\Answered \\\\Draft\"] { fileinto \"B\"; } if hasflag :is \
         :comparator \"i;octet\" \"$tagged\" { fileinto \"C\"; }",
    );
    assert_eq!(filed(&run(&script, &Message::new())), ["A"]);
    assert_eq!(
        filed(&run(&script, &Message::new().flags(&["\\Draft"]))),
        ["A", "B"]
    );
}

#[test]
fn address_parts() {
    let message = Message::new()
        .from("Kari Nordmann <Kari.Nordmann@Example.test>")
        .header(
            "To",
            "ola@example.test, \"Per, P.\" <per@lists.example.test>",
        )
        .header("Cc", "undisclosed-recipients:;");
    let hit = |test: &str| {
        !run(
            &with_fileinto(&format!("if {test} {{ fileinto \"hit\"; }}")),
            &message,
        )
        .keep
    };
    assert!(hit("address :is \"from\" \"kari.nordmann@example.test\""));
    assert!(hit(
        "address :all :is \"from\" \"kari.nordmann@example.test\""
    ));
    assert!(hit("address :localpart :is \"from\" \"kari.nordmann\""));
    assert!(hit("address :domain :is \"from\" \"example.test\""));
    assert!(
        !hit("address :is \"from\" \"Kari Nordmann\""),
        "never the display name"
    );
    assert!(hit(
        "address :domain :is [\"to\", \"cc\"] \"lists.example.test\""
    ));
    assert!(hit("address :localpart :is \"to\" [\"nobody\", \"per\"]"));
    assert!(!hit("address :domain :is \"to\" \"example\""));
    assert!(
        !hit("address :contains \"cc\" \"undisclosed\""),
        "a group holds no address"
    );
    assert!(
        !hit("address :contains \"bcc\" \"\""),
        "no Bcc header, no address"
    );
    assert!(hit("address :matches \"from\" \"*@*.test\""));
}

#[test]
fn header_values() {
    let message = Message::new()
        .header("Subject", "  Møte i morgen: budsjett  ")
        .header("Received", "from one")
        .header("Received", "from two")
        .header("X-Empty", "");
    let hit = |test: &str| {
        !run(
            &with_fileinto(&format!("if {test} {{ fileinto \"hit\"; }}")),
            &message,
        )
        .keep
    };
    assert!(
        hit("header :is \"subject\" \"Møte i morgen: budsjett\""),
        "trimmed"
    );
    assert!(
        !hit("header :contains \"Subject\" \"MØTE\""),
        "only ASCII letters fold"
    );
    assert!(hit("header :contains \"Subject\" \"BUDSJETT\""));
    assert!(hit("header :is \"received\" \"from two\""), "each value");
    assert!(hit("header :contains \"x-empty\" \"\""));
    assert!(hit("header :is \"x-empty\" \"\""));
    assert!(!hit("header :contains \"x-missing\" \"\""));
    assert!(hit(
        "header :contains [\"x-missing\", \"subject\"] [\"nope\", \"i morgen\"]"
    ));
}

#[test]
fn exists_needs_every_header() {
    let message = Message::new()
        .header("Date", "today")
        .header("X-Spam", "yes");
    let hit = |test: &str| {
        !run(
            &with_fileinto(&format!("if {test} {{ fileinto \"hit\"; }}")),
            &message,
        )
        .keep
    };
    assert!(hit("exists \"x-spam\""));
    assert!(hit("exists [\"Date\", \"X-Spam\"]"));
    assert!(!hit("exists [\"Date\", \"List-Id\"]"));
}

#[test]
fn size_is_strict() {
    let message = Message::new().size(100 << 10);
    let hit = |test: &str| {
        !run(
            &with_fileinto(&format!("if {test} {{ fileinto \"hit\"; }}")),
            &message,
        )
        .keep
    };
    assert!(!hit("size :over 100K") && !hit("size :under 100K"));
    assert!(hit("size :over 102399") && hit("size :under 102401"));
    assert!(hit("size :under 1M") && !hit("size :over 1G"));
}

#[test]
fn logic() {
    let message = Message::new().header("Subject", "x");
    let hit = |test: &str| {
        !run(
            &with_fileinto(&format!("if {test} {{ fileinto \"hit\"; }}")),
            &message,
        )
        .keep
    };
    assert!(hit("true") && !hit("false"));
    assert!(hit("not false") && !hit("not not false"));
    assert!(hit("allof (true, exists \"subject\")") && !hit("allof (true, false)"));
    assert!(hit("anyof (false, exists \"subject\")") && !hit("anyof (false, not true)"));
    assert!(hit("allof (anyof (false, true), not anyof (false, false))"));
}

#[test]
fn match_types_and_comparators() {
    let casemap = |kind| Matcher {
        kind,
        comparator: Comparator::AsciiCasemap,
    };
    let octet = |kind| Matcher {
        kind,
        comparator: Comparator::Octet,
    };
    let cases = [
        (casemap(MatchType::Is), "Faktura", "fAKTURA", true),
        (octet(MatchType::Is), "Faktura", "fAKTURA", false),
        (octet(MatchType::Is), "Faktura", "Faktura", true),
        (casemap(MatchType::Is), "Ærlig", "ærlig", false),
        (
            casemap(MatchType::Contains),
            "Re: Faktura 2026",
            "FAKTURA",
            true,
        ),
        (
            octet(MatchType::Contains),
            "Re: Faktura 2026",
            "FAKTURA",
            false,
        ),
        (casemap(MatchType::Contains), "anything", "", true),
        (casemap(MatchType::Contains), "æøå", "ø", true),
        (
            casemap(MatchType::Matches),
            "Faktura 2026",
            "faktura*",
            true,
        ),
        (casemap(MatchType::Matches), "Faktura 2026", "*2026", true),
        (
            casemap(MatchType::Matches),
            "Faktura 2026",
            "f?ktura ????",
            true,
        ),
        (
            casemap(MatchType::Matches),
            "Faktura 2026",
            "f?ktura ???",
            false,
        ),
        (casemap(MatchType::Matches), "Faktura 2026", "*a*a*", true),
        (casemap(MatchType::Matches), "Faktura 2026", "*b*", false),
        (casemap(MatchType::Matches), "", "*", true),
        (casemap(MatchType::Matches), "", "?", false),
        (casemap(MatchType::Matches), "æøå", "?ø?", true),
        (casemap(MatchType::Matches), "50% *off*", "*\\*off\\*", true),
        (casemap(MatchType::Matches), "50% off", "*\\*off\\*", false),
        (casemap(MatchType::Matches), "what?", "what\\?", true),
        (casemap(MatchType::Matches), "whatx", "what\\?", false),
        (
            casemap(MatchType::Matches),
            "back\\slash",
            "back\\\\slash",
            true,
        ),
        (
            casemap(MatchType::Matches),
            "trailing\\",
            "trailing\\",
            true,
        ),
        (octet(MatchType::Matches), "Faktura", "faktura", false),
        (casemap(MatchType::Matches), "faktura", "Faktura", true),
    ];
    for (matcher, value, key, expected) in cases {
        assert_eq!(
            matches(&matcher, value, key),
            expected,
            "{matcher:?} {value:?} {key:?}"
        );
    }
}

#[test]
fn wildcards_take_at_most_value_times_pattern_steps() {
    let value = "a".repeat(20_000);
    let pattern = format!("{}b", "*a".repeat(200));
    let matcher = Matcher {
        kind: MatchType::Matches,
        comparator: Comparator::AsciiCasemap,
    };
    assert!(!matches(&matcher, &value, &pattern));
    assert!(matches(&matcher, &value, &"*a".repeat(200)));
}

#[test]
fn environment_names_the_folder_it_arrived_in() {
    let script = "require [\"environment\", \"imapsieve\", \"fileinto\"];\nif environment :is \
                  \"imap.mailbox\" \"Junk\" { fileinto \"INBOX\"; } if environment :contains \
                  \"imap.user\" \"\" { discard; }";
    let in_junk = run(script, &Message::new().mailbox("Junk"));
    assert_eq!(filed(&in_junk), ["INBOX"]);
    assert!(!in_junk.discard, "an item Sealwax doesn't know is false");
    assert!(run(script, &Message::new()).keep);
    let without_imapsieve = "require \"environment\";\nif environment \"imap.mailbox\" \"junk\" { \
                             discard; }";
    assert!(run(without_imapsieve, &Message::new().mailbox("Junk")).trashed());
}

#[test]
fn rfc_5228_example_runs() {
    let cases = [
        (
            Message::new()
                .header("Sender", "owner-ietf-mta-filters@lists.example.test")
                .from("someone@elsewhere.test"),
            Some("filter"),
        ),
        (Message::new().from("Ola <ola@example.test>"), None),
        (
            Message::new()
                .from("x@elsewhere.test")
                .header("To", "me@home.example.test")
                .header("Subject", "Make lots of MONEY fast"),
            Some("spam"),
        ),
        (
            Message::new()
                .from("x@elsewhere.test")
                .header("To", "other@elsewhere.test"),
            Some("spam"),
        ),
        (
            Message::new()
                .from("x@elsewhere.test")
                .header("Cc", "Me <me@home.example.test>")
                .header("Subject", "hello"),
            Some("personal"),
        ),
    ];
    for (message, expected) in cases {
        let outcome = run(RFC_EXAMPLE, &message);
        match expected {
            Some(folder) => assert_eq!(filed(&outcome), [folder]),
            None => assert!(outcome.keep && outcome.filed.is_empty()),
        }
    }
}

#[test]
fn outcomes_never_show_names_in_debug() {
    let outcome = run(
        &with_fileinto("addflag \"$private\"; fileinto \"Kari's private folder\";"),
        &Message::new(),
    );
    let debug = format!("{outcome:?}");
    assert!(
        !debug.contains("Kari") && !debug.contains("private"),
        "{debug}"
    );
}

#[test]
fn flags_map_to_the_model() {
    assert_eq!(
        flag_names(Flags::SEEN | Flags::JUNK | Flags::FORWARDED),
        ["\\Seen", "$Forwarded", "$Junk"]
    );
    let (add, remove, add_keywords, remove_keywords) =
        change(&["\\Seen", "$NotJunk", "$label1"], &["$Junk"]).to_model();
    assert_eq!(add, Flags::SEEN | Flags::NOT_JUNK);
    assert_eq!(remove, Flags::JUNK);
    assert_eq!(add_keywords, ["$label1"]);
    assert!(remove_keywords.is_empty());
}
