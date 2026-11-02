//! Search (ML-081, ML-082): the query language against the index, the plans for few and many
//! matches checked against a scan, æ and ø typed either way, attachment names, what a search of
//! every folder leaves out, and Rebuild Search Index. Every name and address is fictional.

use super::ops_tests::{self, NOW, WINDOW};
use super::tests::scratch;
use super::views::local_midnight;
use super::*;
use crate::blobs::BlobStore;
use crate::ingest::{RawMessage, ingest_raw};
use rusqlite::Connection;
use sealwax_domain::model::{Address, Attachment, FolderRole, Headers};
use sealwax_domain::search::{self, Date};
use sealwax_domain::senders::GroupBy;
use sealwax_domain::triage::Triage;
use sealwax_domain::unsubscribe::Method;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// 2026-03-02, a Monday.
const T0: i64 = 1_772_409_600;
/// Where a rebuild under way keeps its place.
const THROUGH: &str = "fts_rebuild_through";

struct Fx {
    dir: tempfile::TempDir,
    store: Store,
    blobs: BlobStore,
    inbox: FolderId,
    archive: FolderId,
    trash: FolderId,
    junk: FolderId,
}

impl Fx {
    fn path(&self) -> PathBuf {
        self.dir.path().join("store.sqlite")
    }

    fn find(&self, text: &str, folder: Option<FolderId>) -> Vec<MessageId> {
        find(&self.store, text, folder, 1_000)
    }

    /// A connection beside the store's own, as a damaged or older index needs.
    fn conn(&self) -> Connection {
        let conn = Connection::open(self.path()).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        conn
    }
}

fn reopen(dir: &tempfile::TempDir, blobs: &BlobStore) -> Store {
    Store::open(
        &dir.path().join("store.sqlite"),
        StoreOptions {
            blobs: Some(blobs.clone()),
            ..StoreOptions::default()
        },
    )
    .unwrap()
}

fn fixture() -> Fx {
    let dir = scratch();
    let blobs = BlobStore::open(&dir.path().join("blobs")).unwrap();
    let store = Store::open(
        &dir.path().join("store.sqlite"),
        StoreOptions {
            blobs: Some(blobs.clone()),
            ..StoreOptions::default()
        },
    )
    .unwrap();
    let account = store
        .add_account(NewAccount {
            name: "Søk".into(),
            address: "sok@example.test".into(),
        })
        .recv_blocking()
        .unwrap()
        .value;
    let folder = |name: &str, role: FolderRole| {
        store
            .upsert_folder(FolderSpec::local(account, name, role))
            .recv_blocking()
            .unwrap()
            .value
    };
    let inbox = folder("INBOX", FolderRole::Inbox);
    let archive = folder("Archive", FolderRole::Archive);
    let trash = folder("Trash", FolderRole::Trash);
    let junk = folder("Junk", FolderRole::Junk);
    Fx {
        dir,
        store,
        blobs,
        inbox,
        archive,
        trash,
        junk,
    }
}

fn find(store: &Store, text: &str, folder: Option<FolderId>, limit: u32) -> Vec<MessageId> {
    store
        .search(text, folder, limit)
        .recv_blocking()
        .unwrap_or_else(|error| panic!("{text}: {error}"))
        .value
        .iter()
        .map(|row| row.id)
        .collect()
}

fn person(name: &str) -> Address {
    let local: String = name
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(".");
    Address::new(name, format!("{local}@example.test"))
}

/// A message without a body in the blob store, as the first sync stores it.
fn mail(folder: FolderId, n: u32, date: i64) -> NewMessage {
    NewMessage {
        folder,
        uid: Some(n + 1),
        date,
        headers: Headers {
            from: vec![person("Avsender")],
            subject: format!("Melding {n}"),
            message_id: Some(format!("melding-{n}@example.test")),
            date: Some(date),
            ..Headers::default()
        },
        snippet: format!("Utdrag {n}"),
        search_text: format!("tekst nummer{n}"),
        flags: Flags::SEEN,
        size: 100,
        ..NewMessage::default()
    }
}

fn ingest(store: &Store, messages: Vec<NewMessage>) -> Vec<MessageId> {
    store
        .ingest(messages)
        .recv_blocking()
        .unwrap()
        .ids
        .into_iter()
        .map(Option::unwrap)
        .collect()
}

/// A whole message for the blob store: `body` as its text, and a PDF named `file` when given.
fn raw(folder: FolderId, n: u32, subject: &str, body: &str, file: Option<&str>) -> RawMessage {
    let head = format!(
        "From: Avsender {n} <avsender{n}@example.test>\r\nTo: Mottaker <mottaker@example.test>\r\n\
         Subject: {subject}\r\nMessage-ID: <raw-{n}@example.test>\r\nMIME-Version: 1.0\r\n"
    );
    let content = match file {
        None => format!("Content-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"),
        Some(file) => format!(
            "Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n--b\r\n\
             Content-Type: application/pdf; name=\"{file}\"\r\n\
             Content-Disposition: attachment; filename=\"{file}\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n--b--\r\n"
        ),
    };
    RawMessage {
        folder,
        flags: Flags::SEEN,
        internal_date: Some(T0 + i64::from(n) * 60),
        raw: format!("{head}{content}").into_bytes(),
        ..RawMessage::default()
    }
}

fn ingest_raws(fx: &Fx, messages: Vec<RawMessage>) -> Vec<MessageId> {
    ingest_raw(&fx.store, &fx.blobs, messages)
        .unwrap()
        .ids
        .into_iter()
        .map(Option::unwrap)
        .collect()
}

#[test]
fn the_last_word_typed_finds_the_words_it_starts() {
    let fx = fixture();
    let mut m0 = mail(fx.inbox, 0, T0);
    m0.headers.from = vec![person("Annabelle Lie")];
    m0.search_text = "budsjett og kontrakt".into();
    let mut m1 = mail(fx.inbox, 1, T0 + 60);
    m1.headers.from = vec![person("Kon Tiki")];
    m1.search_text = "budsjett kon".into();
    let ids = ingest(&fx.store, vec![m0, m1]);
    let none = Vec::<MessageId>::new();
    for (text, expected) in [
        ("budsjett kon", vec![ids[1], ids[0]]),
        ("budsjett kon ", vec![ids[1]]),
        ("budsjett kontr", vec![ids[0]]),
        ("from:anna", vec![ids[0]]),
        ("from:anna ", none.clone()),
        ("\"budsjett ko\"", none.clone()),
        ("budsjett -kon", vec![ids[0]]),
        ("k", none.clone()),
        ("ko", vec![ids[1], ids[0]]),
        ("from:annabelle.lie@example.t", vec![ids[0]]),
        ("from:annabelle.lie@example.te", vec![ids[0]]),
        ("from:kon.tiki@e", vec![ids[1]]),
    ] {
        assert_eq!(fx.find(text, None), expected, "{text:?}");
    }
}

#[test]
fn operators_search_their_columns() {
    let fx = fixture();
    let mut m0 = mail(fx.inbox, 0, T0);
    m0.headers.from = vec![person("Anna Berg")];
    m0.headers.to = vec![person("Kari Nordmann")];
    m0.headers.subject = "Q3 plan".into();
    m0.search_text = "budsjett for høsten".into();
    let mut m1 = mail(fx.inbox, 1, T0 + 60);
    m1.headers.from = vec![person("Kari Nordmann")];
    m1.headers.to = vec![person("Anna Berg")];
    m1.headers.cc = vec![person("Ola Hansen")];
    m1.headers.subject = "Re: lunsj".into();
    m1.search_text = "anna sa ja til lunsj".into();
    let mut m2 = mail(fx.archive, 2, T0 + 120);
    m2.headers.from = vec![person("Ola Hansen")];
    m2.headers.to = vec![Address::new("Team", "team@example.org")];
    m2.headers.cc = vec![person("Anna Berg")];
    m2.headers.subject = "Annas plan".into();
    m2.search_text = "plan for neste kvartal".into();
    let ids = ingest(&fx.store, vec![m0, m1, m2]);
    let (m0, m1, m2) = (ids[0], ids[1], ids[2]);
    let none = Vec::<MessageId>::new();
    for (text, expected) in [
        ("from:anna", vec![m0]),
        ("FROM:\"Anna Berg\"", vec![m0]),
        ("from:\"anna nordmann\"", none.clone()),
        ("from:ann*", vec![m0]),
        ("from:anna.berg@example.test", vec![m0]),
        ("to:anna", vec![m2, m1]),
        ("to:team@example.org", vec![m2]),
        ("to:hansen", vec![m1]),
        ("subject:plan", vec![m2, m0]),
        ("subject:\"q3 plan\"", vec![m0]),
        ("subject:lunsj", vec![m1]),
        ("anna", vec![m2, m1, m0]),
        ("from:anna subject:plan", vec![m0]),
        ("from:kari to:anna lunsj", vec![m1]),
        ("plan -lunsj", vec![m2, m0]),
        ("plan -\"q3 plan\"", vec![m2]),
        ("-anna", none.clone()),
        ("-kvartal -budsjett", vec![m1]),
        ("subject:plan -from:ola", vec![m2, m0]),
    ] {
        assert_eq!(fx.find(text, None), expected, "{text}");
    }
    assert_eq!(fx.find("plan", Some(fx.inbox)), [m0]);
    assert_eq!(
        fx.store.check().recv_blocking().unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn flags_attachments_and_days() {
    let fx = fixture();
    let day = Date::new(2026, 3, 10).unwrap();
    let midnight = local_midnight(day);
    let at = |n: u32, date: i64, flags: Flags, attachments: bool| {
        let mut m = mail(fx.inbox, n, date);
        m.flags = flags;
        m.has_attachments = attachments;
        m.search_text = "rapport".into();
        m
    };
    let ids = ingest(
        &fx.store,
        vec![
            at(0, midnight - 1, Flags::SEEN, false),
            at(1, midnight, Flags::NONE, true),
            at(2, midnight + 86_399, Flags::SEEN | Flags::FLAGGED, true),
            at(3, midnight + 86_400, Flags::FLAGGED, false),
        ],
    );
    let none = Vec::<MessageId>::new();
    for (text, expected) in [
        ("before:2026-03-10", vec![ids[0]]),
        ("rapport before:10.3.2026", vec![ids[0]]),
        ("after:2026-03-10", vec![ids[3], ids[2], ids[1]]),
        ("after:2026/03/10 before:2026-03-11", vec![ids[2], ids[1]]),
        ("-before:2026-03-11", vec![ids[3]]),
        ("after:2026-03-11 before:2026-03-10", none.clone()),
        ("before:2026-03", none.clone()),
        ("after:2026", vec![ids[3], ids[2], ids[1], ids[0]]),
        ("is:unread", vec![ids[3], ids[1]]),
        ("is:read", vec![ids[2], ids[0]]),
        ("is:starred", vec![ids[3], ids[2]]),
        ("-is:starred rapport", vec![ids[1], ids[0]]),
        ("has:attachment", vec![ids[2], ids[1]]),
        ("-has:attachment", vec![ids[3], ids[0]]),
        ("has:attachment is:unread", vec![ids[1]]),
        ("has:attachment is:starred rapport", vec![ids[2]]),
        ("is:unread after:2026-03-11", vec![ids[3]]),
        ("is:starred -has:attachment before:2026-03-11", none.clone()),
    ] {
        assert_eq!(fx.find(text, None), expected, "{text}");
        assert_eq!(
            fx.find(text, Some(fx.inbox)),
            expected,
            "{text} in the inbox"
        );
    }
    assert_eq!(fx.find("is:unread", Some(fx.archive)), none);
}

#[test]
fn flags_are_the_ones_the_list_shows() {
    let fx = ops_tests::server();
    let ids = ops_tests::arrive(
        &fx.store,
        fx.inbox,
        (1..=6)
            .map(|uid| ops_tests::message(fx.inbox, uid, Flags::NONE))
            .collect(),
    );
    let queue = |action: Triage, messages: &[MessageId]| {
        fx.store
            .queue_triage(QueueTriage {
                action,
                messages: messages.to_vec(),
                not_before: NOW + WINDOW,
            })
            .recv_blocking()
            .unwrap();
    };
    queue(Triage::Read(true), &ids[0..2]);
    queue(Triage::Star(true), &ids[2..3]);
    queue(Triage::Archive, &ids[3..4]);
    let listed = |filter: ViewFilter| -> Vec<MessageId> {
        let mut rows: Vec<MessageId> =
            super::tests::whole(&fx.store, ViewSpec::folder(fx.inbox).with_filter(filter))
                .rows
                .iter()
                .map(|row| row.id)
                .collect();
        rows.sort_unstable_by(|a, b| b.cmp(a));
        rows
    };
    for (text, filter) in [
        ("is:unread", ViewFilter::Unread),
        ("is:starred", ViewFilter::Starred),
        ("has:attachment", ViewFilter::Attachments),
        ("is:unread message*", ViewFilter::Unread),
    ] {
        let mut found = find(&fx.store, text, Some(fx.inbox), 100);
        found.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(found, listed(filter), "{text}");
    }
    assert!(
        find(&fx.store, "message4", None, 10).is_empty(),
        "a pending archive hides it"
    );
    assert_eq!(find(&fx.store, "is:unread", None, 10).len(), 3);
}

#[test]
fn every_folder_leaves_out_trash_and_junk() {
    let fx = fixture();
    let ids = ingest(
        &fx.store,
        [fx.inbox, fx.archive, fx.trash, fx.junk]
            .into_iter()
            .enumerate()
            .map(|(n, folder)| {
                let mut m = mail(folder, n as u32, T0 + n as i64);
                m.search_text = "kvittering".into();
                m.flags = Flags::NONE;
                m
            })
            .collect(),
    );
    assert_eq!(fx.find("kvittering", None), [ids[1], ids[0]]);
    assert_eq!(fx.find("is:unread", None), [ids[1], ids[0]]);
    assert_eq!(fx.find("kvittering", Some(fx.trash)), [ids[2]]);
    assert_eq!(fx.find("is:unread", Some(fx.junk)), [ids[3]]);
}

#[test]
fn ae_and_o_with_stroke_are_found_typed_either_way() {
    let fx = fixture();
    let mut m = mail(fx.inbox, 0, T0);
    m.headers.from = vec![person("Bjørn Ærøe")];
    m.headers.subject = "Blåbærsyltetøy på hytta".into();
    m.search_text = "Vi tar med blåbærsyltetøy og øl.".into();
    let id = ingest(&fx.store, vec![m])[0];
    for text in [
        "blåbærsyltetøy",
        "blabaersyltetoy",
        "BLÅBÆRSYLTETØY",
        "blåbaersyltetoy",
        "from:bjorn",
        "from:bjørn",
        "from:aeroe",
        "bjø*",
        "bjo*",
        "subject:\"blabaersyltetoy pa hytta\"",
        "ol",
    ] {
        assert_eq!(fx.find(text, None), [id], "{text}");
    }
    assert!(fx.find("blabarsyltetoy", None).is_empty());

    // An entry written before M7 holds æ and ø as they were: typed with them it is found.
    let conn = fx.conn();
    conn.execute("DELETE FROM message_fts WHERE rowid = ?1", [id.0])
        .unwrap();
    conn.execute(
        "INSERT INTO message_fts(rowid, subject, sender, recipients, body) \
         VALUES (?1, 'Blåbærsyltetøy på hytta', 'Bjørn Ærøe', '', 'blåbærsyltetøy og øl')",
        [id.0],
    )
    .unwrap();
    drop(conn);
    assert_eq!(fx.find("blåbærsyltetøy", None), [id]);
    assert_eq!(fx.find("from:bjø*", None), [id]);
    assert!(fx.find("blabaersyltetoy", None).is_empty());
}

#[test]
fn attachment_names_are_found_with_the_body() {
    let fx = fixture();
    let file = |name: &str, embedded: bool| Attachment {
        part: 2,
        name: Some(name.into()),
        size: 100,
        mime_type: if embedded {
            "image/png"
        } else {
            "application/pdf"
        }
        .into(),
        content_id: embedded.then(|| "logo@example.test".into()),
        inline: embedded,
    };
    let mut m = mail(fx.inbox, 0, T0);
    m.attachments = vec![
        file("kontrakt-2026.pdf", false),
        file("firmalogo.png", true),
    ];
    m.has_attachments = true;
    let header_only = mail(fx.inbox, 1, T0 + 60);
    let ids = ingest(&fx.store, vec![m, header_only]);
    assert_eq!(fx.find("kontrakt", None), [ids[0]]);
    assert_eq!(fx.find("has:attachment pdf", None), [ids[0]]);
    assert!(fx.find("firmalogo", None).is_empty(), "an inline image");

    let raw = raw(
        fx.inbox,
        1,
        "Melding 1",
        "Signert avtale.",
        Some("avtale_signert.pdf"),
    );
    let key = fx.blobs.put(&raw.raw).unwrap();
    let parsed = sealwax_domain::message::parse(&raw.raw);
    let upgraded = fx
        .store
        .upgrade_body(BodyUpgrade {
            message: ids[1],
            uid: 2,
            blob: key,
            blob_size: raw.raw.len() as u64,
            body_state: BodyState::Full,
            body_kind: parsed.body_kind,
            has_attachments: parsed.has_attachments(),
            snippet: parsed.snippet.clone(),
            text_parts: parsed.text_parts.clone(),
            html_parts: parsed.html_parts.clone(),
            attachments: parsed.attachments.clone(),
            sections: Vec::new(),
            search_text: parsed.body_text.clone(),
            used: T0,
        })
        .recv_blocking()
        .unwrap();
    assert!(upgraded.found);
    assert_eq!(fx.find("avtale_signert", None), [ids[1]]);
    assert_eq!(fx.find("signert", None), [ids[1]]);
}

#[test]
fn any_text_is_a_search() {
    let fx = fixture();
    ingest(
        &fx.store,
        (0..5)
            .map(|n| mail(fx.inbox, n, T0 + i64::from(n)))
            .collect(),
    );
    let mut rng = super::tests::Rng(7);
    let pieces = [
        "\"",
        "*",
        ":",
        "-",
        "(",
        ")",
        "^",
        "+",
        "NEAR",
        "OR",
        "AND",
        "NOT",
        "from:",
        "to:",
        "is:",
        "has:",
        "before:",
        "after:",
        "subject:",
        "{",
        "}",
        "æ",
        "ø",
        "会议",
        " ",
        "  ",
        "tekst",
        "nummer1",
        "x",
        "0",
        "2026-01-01",
        "'",
        "\\",
        "\u{301}",
        "ा",
    ];
    for _ in 0..2_000 {
        let text: String = (0..rng.below(8))
            .map(|_| pieces[rng.below(pieces.len() as u64) as usize])
            .collect();
        for folder in [None, Some(fx.inbox)] {
            fx.store
                .search(&text, folder, 50)
                .recv_blocking()
                .unwrap_or_else(|error| panic!("{text:?}: {error}"));
        }
    }
}

/// A message of the scan model: what the store was given.
struct Modelled {
    id: MessageId,
    folder: FolderId,
    date: i64,
    flags: Flags,
    attachments: bool,
    subject: Vec<&'static str>,
    sender: &'static str,
    recipient: &'static str,
    body: Vec<&'static str>,
}

const WORDS: [&str; 24] = [
    "alfa", "bravo", "charlie", "delta", "ekko", "foxtrot", "golf", "hotell", "india", "juliett",
    "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango",
    "uniform", "viktor", "whiskey", "xray",
];
const PEOPLE: [&str; 5] = ["ada", "berit", "cato", "dina", "eirik"];

impl Modelled {
    fn has(words: &[&str], term: &search::Term) -> bool {
        match term {
            search::Term::Word(word) | search::Term::Phrase(word) => words.contains(&word.as_str()),
            search::Term::Prefix(stem) => words.iter().any(|word| word.starts_with(stem.as_str())),
        }
    }

    /// The words the index holds of an address: its local part, `example` and `test`.
    fn address(local: &'static str) -> [&'static str; 3] {
        [local, "example", "test"]
    }

    /// Whether the full-text part of a query matches, wherever the message is.
    fn matches_words(&self, query: &search::Query) -> bool {
        let sender = Self::address(self.sender);
        let recipient = Self::address(self.recipient);
        let everywhere = [self.subject.as_slice(), &sender, &recipient, &self.body].concat();
        query.text.iter().all(|term| Self::has(&everywhere, term))
            && !query
                .exclude
                .iter()
                .any(|term| Self::has(&everywhere, term))
            && query.from.iter().all(|term| Self::has(&sender, term))
            && query.to.iter().all(|term| Self::has(&recipient, term))
            && query
                .subject
                .iter()
                .all(|term| Self::has(&self.subject, term))
    }

    fn matches(
        &self,
        query: &search::Query,
        folder: Option<FolderId>,
        left_out: &[FolderId],
    ) -> bool {
        let searched = match folder {
            Some(folder) => self.folder == folder,
            None => !left_out.contains(&self.folder),
        };
        searched
            && self.matches_words(query)
            && query
                .unread
                .is_none_or(|unread| unread != self.flags.is_seen())
            && query
                .starred
                .is_none_or(|starred| starred == self.flags.contains(Flags::FLAGGED))
            && query
                .has_attachment
                .is_none_or(|has| has == self.attachments)
            && query
                .after
                .is_none_or(|after| self.date >= local_midnight(after))
            && query
                .before
                .is_none_or(|before| self.date < local_midnight(before))
    }
}

fn pick(rng: &mut super::tests::Rng) -> Vec<&'static str> {
    WORDS
        .iter()
        .enumerate()
        .filter(|(n, _)| rng.below(*n as u64 * *n as u64 + 2) == 0)
        .map(|(_, word)| *word)
        .collect()
}

/// Every plan (few matches, many, prefixes, no text, one folder or all) against a scan of what
/// the store was given.
#[test]
fn plans_agree_with_a_scan() {
    let fx = fixture();
    let mut rng = super::tests::Rng(42);
    let folders = [fx.inbox, fx.inbox, fx.archive, fx.trash, fx.junk];
    // Word n is in about one subject or body of n² + 2, so the first words are common and the
    // last ones rare, on both sides of the few-matches threshold.
    let mut messages = Vec::new();
    let mut model = Vec::new();
    for n in 0..1_500u32 {
        let folder = folders[rng.below(folders.len() as u64) as usize];
        let date = T0 - 86_400 * 400 + rng.below(86_400 * 400) as i64 / 3_600 * 3_600;
        let subject = pick(&mut rng);
        let body = pick(&mut rng);
        let sender = PEOPLE[rng.below(PEOPLE.len() as u64) as usize];
        let recipient = PEOPLE[rng.below(PEOPLE.len() as u64) as usize];
        let mut flags = Flags::NONE;
        if rng.below(3) > 0 {
            flags = flags | Flags::SEEN;
        }
        if rng.below(10) == 0 {
            flags = flags | Flags::FLAGGED;
        }
        let attachments = rng.below(4) == 0;
        let mut m = mail(folder, n, date);
        m.flags = flags;
        m.has_attachments = attachments;
        m.headers.subject = subject.join(" ");
        m.headers.from = vec![Address::new("", format!("{sender}@example.test"))];
        m.headers.to = vec![Address::new("", format!("{recipient}@example.test"))];
        m.search_text = body.join(" ");
        messages.push(m);
        model.push(Modelled {
            id: MessageId(0),
            folder,
            date,
            flags,
            attachments,
            subject,
            sender,
            recipient,
            body,
        });
    }
    let ids = ingest(&fx.store, messages);
    for (modelled, id) in model.iter_mut().zip(ids) {
        modelled.id = id;
    }
    let left_out = [fx.trash, fx.junk];
    let day = |offset: u64| {
        let days = offset as i64;
        let seconds = T0 - 86_400 * 400 + days * 86_400;
        let (year, month, day) = civil(seconds);
        format!("{year:04}-{month:02}-{day:02}")
    };
    let mut few = 0;
    let mut many = 0;
    for round in 0..400 {
        let mut parts: Vec<String> = Vec::new();
        for _ in 0..rng.below(3) {
            let word = WORDS[rng.below(WORDS.len() as u64) as usize];
            parts.push(match rng.below(6) {
                0 => format!("{}*", &word[..2 + rng.below(3) as usize]),
                1 => format!("-{word}"),
                2 => format!("subject:{word}"),
                _ => word.to_owned(),
            });
        }
        let person = PEOPLE[rng.below(PEOPLE.len() as u64) as usize];
        match rng.below(6) {
            0 => parts.push(format!("from:{person}")),
            1 => parts.push(format!("to:{person}")),
            _ => {}
        }
        for (chance, operator) in [
            (5, "is:unread"),
            (8, "is:read"),
            (8, "is:starred"),
            (10, "-is:starred"),
            (5, "has:attachment"),
            (10, "-has:attachment"),
        ] {
            if rng.below(chance) == 0 {
                parts.push(operator.to_owned());
            }
        }
        if rng.below(3) == 0 {
            parts.push(format!("after:{}", day(rng.below(400))));
        }
        if rng.below(3) == 0 {
            parts.push(format!("before:{}", day(rng.below(400))));
        }
        let text = parts.join(" ");
        let query = search::parse_typed(&text);
        let folder = (rng.below(3) == 0).then(|| folders[rng.below(4) as usize]);
        let limit = [10, 50, 1_000][round % 3];
        let mut expected: Vec<&Modelled> = model
            .iter()
            .filter(|modelled| !query.is_empty() && modelled.matches(&query, folder, &left_out))
            .collect();
        expected.sort_by_key(|modelled| std::cmp::Reverse((modelled.date, modelled.id)));
        expected.truncate(limit as usize);
        let expected: Vec<MessageId> = expected.iter().map(|modelled| modelled.id).collect();
        assert_eq!(
            find(&fx.store, &text, folder, limit),
            expected,
            "{text:?} in {folder:?}, limit {limit}"
        );
        let expression = super::fts::Expression::of(&query);
        if expression.matching.is_some() && !expression.prefix {
            let words = model
                .iter()
                .filter(|modelled| {
                    modelled.matches_words(&search::Query {
                        exclude: Vec::new(),
                        ..query.clone()
                    })
                })
                .count();
            if words <= 200 {
                few += 1;
            } else {
                many += 1;
            }
        }
    }
    assert!(few > 30 && many > 30, "few {few}, many {many}");
}

/// The civil date of Unix seconds, in local time (the inverse `local_midnight` needs).
fn civil(seconds: i64) -> (i32, u8, u8) {
    // SAFETY: as in `local_midnight`: plain C structs, and `localtime_r` writes only `tm`.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let time: libc::time_t = seconds;
    unsafe { libc::localtime_r(&time, &mut tm) };
    (tm.tm_year + 1900, (tm.tm_mon + 1) as u8, tm.tm_mday as u8)
}

#[test]
fn local_midnights() {
    for (year, month, day) in [(2026, 3, 29), (2026, 10, 25), (2024, 2, 29), (1999, 12, 31)] {
        let date = Date::new(year, month, day).unwrap();
        let midnight = local_midnight(date);
        assert_eq!(civil(midnight), (year, month, day));
        assert_ne!(civil(midnight - 1), (year, month, day), "{date:?}");
    }
}

/// Waits until the store's rebuild is done. A write's round trip first: the writer starts a
/// rebuild left unfinished before it serves any.
fn rebuilt(store: &Store) {
    store.touch_body(MessageId(0), 0).recv_blocking().unwrap();
    let started = Instant::now();
    while store.index_progress().is_some() {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "no end to the rebuild"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn meta(conn: &Connection) -> Option<i64> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [THROUGH], |row| {
        row.get(0)
    })
    .ok()
}

/// The index passes FTS5's check, and its automatic merging is on again (FTS5's default, 4).
fn integrity(conn: &Connection) {
    conn.execute_batch("INSERT INTO message_fts(message_fts) VALUES ('integrity-check')")
        .unwrap();
    let automerge: i64 = conn
        .query_row(
            "SELECT v FROM message_fts_config WHERE k = 'automerge'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(automerge, 4);
}

#[test]
fn a_rebuild_reindexes_in_place() {
    let fx = fixture();
    let whole = ingest_raws(
        &fx,
        (0..120)
            .map(|n| {
                let file = (n % 4 == 0).then_some("vedlegg.pdf");
                raw(
                    fx.inbox,
                    n,
                    &format!("Sak {n}"),
                    &format!("felles ord{n} møte"),
                    file,
                )
            })
            .collect(),
    );
    let previews = ingest(
        &fx.store,
        (200..230)
            .map(|n| {
                let mut m = mail(fx.archive, n, T0 + i64::from(n) * 60);
                m.search_text = format!("felles forhandsvisning{n}");
                m
            })
            .collect(),
    );
    let conn = fx.conn();
    // Entries lost, entries that went wrong, and entries of messages that are gone.
    for id in whole[..10].iter().chain(&previews[..5]) {
        conn.execute("DELETE FROM message_fts WHERE rowid = ?1", [id.0])
            .unwrap();
    }
    for id in &whole[10..20] {
        conn.execute("DELETE FROM message_fts WHERE rowid = ?1", [id.0])
            .unwrap();
        conn.execute(
            "INSERT INTO message_fts(rowid, subject, sender, recipients, body) \
             VALUES (?1, '', '', '', 'feil')",
            [id.0],
        )
        .unwrap();
    }
    for rowid in [5_000_001, 5_000_002, 5_000_003] {
        conn.execute(
            "INSERT INTO message_fts(rowid, subject, sender, recipients, body) \
             VALUES (?1, '', '', '', 'felles')",
            [rowid],
        )
        .unwrap();
    }
    drop(conn);
    assert_eq!(fx.find("feil", None).len(), 10);
    assert_eq!(fx.find("felles", None).len(), 120 - 20 + 30 - 5);

    let first = fx.store.rebuild_index();
    let second = fx.store.rebuild_index();
    let result = first.recv_blocking().unwrap();
    assert_eq!(
        second.recv_blocking().unwrap(),
        result,
        "one rebuild for both"
    );
    assert_eq!(
        (result.messages, result.kept, result.removed),
        (120 + 5, 25, 3)
    );
    assert!(fx.store.index_progress().is_none());
    assert!(fx.find("feil", None).is_empty());
    assert_eq!(
        fx.find("felles", None).len(),
        120 + 25,
        "a lost preview is lost"
    );
    assert_eq!(fx.find("mote", None).len(), 120, "folded");
    assert_eq!(fx.find("vedlegg", None).len(), 30, "attachment names");
    assert_eq!(fx.find("forhandsvisning229", None), [previews[29]], "kept");
    assert_eq!(
        fx.find("utdrag 200", None),
        [previews[0]],
        "from the snippet"
    );
    assert_eq!(fx.find("ord7 ", None), [whole[7]]);
    assert_eq!(
        fx.store.check().recv_blocking().unwrap(),
        Vec::<String>::new()
    );
    let conn = fx.conn();
    integrity(&conn);
    assert_eq!(meta(&conn), None);
}

#[test]
fn search_and_writes_go_on_during_a_rebuild() {
    let fx = fixture();
    let ids = ingest_raws(
        &fx,
        (0..600)
            .map(|n| {
                raw(
                    fx.inbox,
                    n,
                    &format!("Sak {n}"),
                    &format!("felles ord{n}"),
                    None,
                )
            })
            .collect(),
    );
    let reply = fx.store.rebuild_index();
    let done = Arc::new(AtomicBool::new(false));
    let waiter = {
        let done = done.clone();
        std::thread::spawn(move || {
            let result = reply.recv_blocking();
            done.store(true, Ordering::SeqCst);
            result
        })
    };
    let mut during = 0;
    let mut flagged = 0;
    while !done.load(Ordering::SeqCst) {
        let running = fx.store.index_progress().is_some();
        assert_eq!(find(&fx.store, "felles", None, 1_000).len(), 600);
        during += usize::from(running);
        if during % 4 == 1 && flagged < ids.len() {
            fx.store
                .set_flags(vec![ids[flagged]], Flags::FLAGGED, Flags::NONE)
                .recv_blocking()
                .unwrap();
            flagged += 1;
        }
    }
    let result = waiter.join().unwrap().unwrap();
    assert_eq!(result.messages, 600);
    assert!(during > 0);
    assert_eq!(find(&fx.store, "is:starred", None, 1_000).len(), flagged);
    assert_eq!(
        fx.store.check().recv_blocking().unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_rebuild_goes_on_when_the_store_opens_again() {
    let fx = fixture();
    let ids = ingest_raws(
        &fx,
        (0..300)
            .map(|n| {
                raw(
                    fx.inbox,
                    n,
                    &format!("Sak {n}"),
                    &format!("felles ord{n}"),
                    None,
                )
            })
            .collect(),
    );
    // Closed during a rebuild: its reply says so, and it goes on at the next start.
    let reply = fx.store.rebuild_index();
    let Fx {
        dir, store, blobs, ..
    } = fx;
    store.close().unwrap();
    let _ = reply.recv_blocking();
    let path = dir.path().join("store.sqlite");
    let conn = Connection::open(&path).unwrap();
    // Wherever it stopped, the rest is left to do, with the entries after 100 lost.
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, 100) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [THROUGH],
    )
    .unwrap();
    for id in &ids[150..] {
        conn.execute("DELETE FROM message_fts WHERE rowid = ?1", [id.0])
            .unwrap();
    }
    drop(conn);
    let store = reopen(&dir, &blobs);
    rebuilt(&store);
    assert_eq!(find(&store, "felles", None, 1_000).len(), 300);
    let conn = Connection::open(&path).unwrap();
    assert_eq!(meta(&conn), None);
    integrity(&conn);
    store.close().unwrap();
}

#[test]
fn subscriptions_keep_their_unsubscribe_header_and_are_found() {
    let fx = fixture();
    let mut letter = mail(fx.inbox, 1, NOW - 60);
    letter.headers.from = vec![Address::new(
        "Butikken",
        "nyhetsbrev@post.butikk.example.co.uk",
    )];
    letter.headers.list_unsubscribe = Some("<https://butikk.example.co.uk/u/1>".into());
    letter.headers.one_click = true;
    let ids = ingest(&fx.store, vec![letter, mail(fx.inbox, 2, NOW - 30)]);
    assert_eq!(find(&fx.store, "is:subscription", None, 10), [ids[0]]);
    assert_eq!(find(&fx.store, "-is:subscription", None, 10), [ids[1]]);
    let detail = fx
        .store
        .message(ids[0])
        .recv_blocking()
        .unwrap()
        .value
        .unwrap();
    assert_eq!(
        detail.headers.list_unsubscribe.as_deref(),
        Some("<https://butikk.example.co.uk/u/1>")
    );
    assert!(detail.headers.one_click);
    assert_eq!(
        find(
            &fx.store,
            "sender:Nyhetsbrev@post.butikk.example.co.uk",
            None,
            10
        ),
        [ids[0]]
    );
    assert_eq!(
        find(&fx.store, "domain:butikk.example.co.uk", None, 10),
        [ids[0]]
    );
    assert_eq!(find(&fx.store, "domain:example.test", None, 10), [ids[1]]);
    assert!(
        find(
            &fx.store,
            "sender:avsender@example.test is:subscription",
            None,
            10
        )
        .is_empty()
    );
    let (path, store) = (fx.path(), fx.store);
    store.close().unwrap();
    let conn = Connection::open(&path).unwrap();
    let domains: Vec<String> = conn
        .prepare("SELECT from_domain FROM messages ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(domains, ["example.co.uk", "example.test"]);
}

#[test]
fn unsubscribe_targets_one_per_list_newest_first_and_remember_it() {
    let fx = fixture();
    let letter = |n: u32, date: i64, list_id: Option<&str>, from: &str| {
        let mut message = mail(fx.inbox, n, date);
        message.headers.from = vec![Address::new("Butikken", from)];
        message.headers.list_id = list_id.map(str::to_owned);
        message.headers.list_unsubscribe = Some(format!("<https://butikk.example/u/{n}>"));
        message.headers.one_click = n.is_multiple_of(2);
        message
    };
    let ids = ingest(
        &fx.store,
        vec![
            letter(
                1,
                NOW - 300,
                Some("tilbud.butikk.example"),
                "a@butikk.example",
            ),
            letter(
                2,
                NOW - 200,
                Some("tilbud.butikk.example"),
                "b@butikk.example",
            ),
            letter(3, NOW - 100, None, "Nytt@Butikk.example"),
            mail(fx.inbox, 4, NOW),
        ],
    );
    let targets = |ids: &[MessageId]| {
        fx.store
            .unsubscribe_targets(ids.to_vec())
            .recv_blocking()
            .unwrap()
    };
    let found = targets(&ids);
    assert_eq!(
        found
            .iter()
            .map(|target| (target.message, target.key.as_str(), target.one_click))
            .collect::<Vec<_>>(),
        [
            (ids[2], "nytt@butikk.example", false),
            (ids[1], "tilbud.butikk.example", true),
        ]
    );
    assert_eq!(found[1].list_unsubscribe, "<https://butikk.example/u/2>");
    assert!(found.iter().all(|target| target.unsubscribed.is_none()));
    assert!(
        !format!("{found:?}").contains("butikk"),
        "no address in Debug"
    );
    let account = found[0].account;
    fx.store
        .record_unsubscribed(
            account,
            "tilbud.butikk.example".into(),
            Method::OneClick,
            NOW,
        )
        .recv_blocking()
        .unwrap();
    assert_eq!(
        targets(&ids[..1])[0].unsubscribed,
        Some((Method::OneClick, NOW))
    );
    assert!(targets(&ids[3..]).is_empty());
}

#[test]
fn a_view_grouped_by_sender_and_by_domain() {
    let fx = fixture();
    let from = |n: u32, date: i64, name: &str, address: &str, seen: bool| {
        let mut message = mail(fx.inbox, n, date);
        message.headers.from = vec![Address::new(name, address)];
        message.flags = if seen { Flags::SEEN } else { Flags::NONE };
        message
    };
    ingest(
        &fx.store,
        vec![
            from(1, NOW - 500, "Butikken", "nytt@post.butikk.example", true),
            from(
                2,
                NOW - 400,
                "Butikken Kundeservice",
                "Hjelp@Butikk.example",
                false,
            ),
            from(3, NOW - 300, "Butikken", "NYTT@post.butikk.example", false),
            from(4, NOW - 200, "", "anna@example.test", true),
            mail(fx.archive, 5, NOW),
        ],
    );
    let groups = |filter: ViewFilter, by: GroupBy| {
        fx.store
            .sender_groups(ViewSpec::folder(fx.inbox).with_filter(filter), by)
            .recv_blocking()
            .unwrap()
            .into_iter()
            .map(|group| {
                (
                    group.key,
                    group.name,
                    group.count,
                    group.unread,
                    group.latest,
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        groups(ViewFilter::All, GroupBy::Sender),
        [
            (
                "anna@example.test".into(),
                "anna@example.test".into(),
                1,
                0,
                NOW - 200
            ),
            (
                "nytt@post.butikk.example".into(),
                "Butikken".into(),
                2,
                1,
                NOW - 300
            ),
            (
                "hjelp@butikk.example".into(),
                "Butikken Kundeservice".into(),
                1,
                1,
                NOW - 400
            ),
        ]
    );
    assert_eq!(
        groups(ViewFilter::All, GroupBy::Domain),
        [
            (
                "example.test".into(),
                "anna@example.test".into(),
                1,
                0,
                NOW - 200
            ),
            ("butikk.example".into(), "Butikken".into(), 3, 2, NOW - 300),
        ]
    );
    assert_eq!(
        groups(ViewFilter::Unread, GroupBy::Domain),
        [("butikk.example".into(), "Butikken".into(), 2, 2, NOW - 300)]
    );
    assert!(groups(ViewFilter::All, GroupBy::None).is_empty());
    let subject = fx
        .store
        .sender_groups(ViewSpec::folder(fx.inbox), GroupBy::Sender)
        .recv_blocking()
        .unwrap()[1]
        .subject
        .clone();
    assert_eq!(subject, "Melding 3");
}

#[test]
fn a_groups_mail_in_the_view_newest_first_with_its_newest_subscription() {
    let fx = fixture();
    let from = |folder: FolderId, n: u32, date: i64, address: &str, seen: bool, list: bool| {
        let mut message = mail(folder, n, date);
        message.headers.from = vec![Address::new("Butikken", address)];
        message.flags = if seen { Flags::SEEN } else { Flags::NONE };
        if list {
            message.headers.list_unsubscribe = Some(format!("<https://butikk.example/u/{n}>"));
        }
        message
    };
    let ids = ingest(
        &fx.store,
        vec![
            from(
                fx.inbox,
                1,
                NOW - 500,
                "nytt@post.butikk.example",
                true,
                true,
            ),
            from(fx.inbox, 2, NOW - 400, "Hjelp@Butikk.example", false, false),
            from(
                fx.inbox,
                3,
                NOW - 300,
                "NYTT@post.butikk.example",
                false,
                true,
            ),
            from(
                fx.inbox,
                4,
                NOW - 200,
                "nytt@post.butikk.example",
                true,
                false,
            ),
            from(fx.inbox, 5, NOW - 100, "anna@example.test", false, true),
            from(fx.archive, 6, NOW, "nytt@post.butikk.example", false, true),
        ],
    );
    let mail = |filter: ViewFilter, by: GroupBy, key: &str| {
        fx.store
            .group_mail(
                ViewSpec::folder(fx.inbox).with_filter(filter),
                by,
                key.into(),
            )
            .recv_blocking()
            .unwrap()
    };
    let sender = mail(ViewFilter::All, GroupBy::Sender, "nytt@post.butikk.example");
    assert_eq!(
        sender
            .messages
            .iter()
            .map(|message| message.id)
            .collect::<Vec<_>>(),
        [ids[3], ids[2], ids[0]]
    );
    assert!(
        sender
            .messages
            .iter()
            .all(|message| message.folder == fx.inbox)
    );
    assert_eq!(sender.messages[1].flags, Flags::NONE);
    assert_eq!(sender.subscription, Some(ids[2]));
    assert!(
        !format!("{sender:?}").contains("butikk"),
        "no address in Debug"
    );
    let domain = mail(ViewFilter::All, GroupBy::Domain, "butikk.example");
    assert_eq!(domain.messages.len(), 4);
    let unread = mail(ViewFilter::Unread, GroupBy::Domain, "butikk.example");
    assert_eq!(
        unread
            .messages
            .iter()
            .map(|message| message.id)
            .collect::<Vec<_>>(),
        [ids[2], ids[1]]
    );
    assert_eq!(
        mail(ViewFilter::All, GroupBy::Sender, "hjelp@butikk.example").subscription,
        None
    );
    assert_eq!(
        mail(ViewFilter::All, GroupBy::None, "butikk.example"),
        GroupMail::default()
    );
    assert_eq!(
        mail(ViewFilter::All, GroupBy::Sender, ""),
        GroupMail::default()
    );
    assert_eq!(
        mail(ViewFilter::All, GroupBy::Sender, "nobody@example.test"),
        GroupMail::default()
    );
}

#[test]
fn v13_fills_the_sender_domains_and_marks_folders_for_the_backfill() {
    let fx = fixture();
    ingest(&fx.store, vec![mail(fx.inbox, 1, NOW)]);
    let (path, store) = (fx.path(), fx.store);
    store.close().unwrap();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(super::schema::UNDO_V13).unwrap();
    drop(conn);
    reopen(&fx.dir, &fx.blobs).close().unwrap();
    let conn = Connection::open(&path).unwrap();
    let domain: String = conn
        .query_row("SELECT from_domain FROM messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(domain, "example.test");
    let marked: Vec<(i64, bool)> = conn
        .prepare("SELECT id, unsubscribe_backfill FROM folders ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        marked,
        [
            (fx.inbox.0, true),
            (fx.archive.0, false),
            (fx.trash.0, false),
            (fx.junk.0, false)
        ]
    );
}

#[test]
fn v10_indexes_every_message_once() {
    let fx = fixture();
    let ids = ingest_raws(
        &fx,
        (0..40)
            .map(|n| {
                raw(
                    fx.inbox,
                    n,
                    "Blåbærsyltetøy",
                    &format!("ærlig talt {n}"),
                    None,
                )
            })
            .collect(),
    );
    let Fx {
        dir, store, blobs, ..
    } = fx;
    store.close().unwrap();
    // Back to v9: entries as M6 wrote them.
    let path = dir.path().join("store.sqlite");
    let conn = Connection::open(&path).unwrap();
    for id in &ids {
        conn.execute("DELETE FROM message_fts WHERE rowid = ?1", [id.0])
            .unwrap();
        conn.execute(
            "INSERT INTO message_fts(rowid, subject, sender, recipients, body) \
             VALUES (?1, 'Blåbærsyltetøy', '', '', 'ærlig talt')",
            [id.0],
        )
        .unwrap();
    }
    conn.execute_batch(super::schema::UNDO_V13).unwrap();
    conn.execute_batch(
        "ALTER TABLE outbox DROP COLUMN building; ALTER TABLE drafts DROP COLUMN upload_building; \
         DROP TABLE blob_deletions; DROP TABLE pop3_uidls; DROP TABLE pop3_accounts; \
         DELETE FROM schema_migrations WHERE version >= 10; PRAGMA user_version = 9;",
    )
    .unwrap();
    drop(conn);
    let store = reopen(&dir, &blobs);
    assert_eq!(find(&store, "blåbærsyltetøy", None, 100).len(), 40);
    rebuilt(&store);
    assert_eq!(find(&store, "blabaersyltetoy aerlig", None, 100).len(), 40);
    assert_eq!(find(&store, "talt 7", None, 100), [ids[7]]);
    let conn = Connection::open(&path).unwrap();
    assert_eq!(meta(&conn), None);
    integrity(&conn);
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    store.close().unwrap();

    let fresh = fixture();
    assert_eq!(
        meta(&fresh.conn()),
        None,
        "a new store has nothing to index again"
    );
    assert!(fresh.store.index_progress().is_none());
}
