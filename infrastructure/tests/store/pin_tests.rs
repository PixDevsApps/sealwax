//! Pinned senders (issue #26): the Pinned section ahead of the dates, its paging, its change
//! events, pinned groups first in Group by, and the pins themselves.

use crate::common::{Model, Rng, scratch, whole};
use crate::prelude::*;
use rusqlite::Connection;
use sealwax_domain::model::{Address, FolderRole, Headers, MessageSummary};
use sealwax_domain::senders::{GroupBy, domain_key, sender_key};

const NOW: i64 = 1_800_000_000;

struct Fx {
    dir: tempfile::TempDir,
    store: Store,
    inbox: FolderId,
    other_inbox: FolderId,
    archive: FolderId,
}

fn fixture() -> Fx {
    let dir = scratch();
    let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
    let account = |name: &str| {
        store
            .add_account(NewAccount {
                name: name.into(),
                address: format!("{name}@example.test"),
            })
            .recv_blocking()
            .unwrap()
            .value
    };
    let (first, second) = (account("en"), account("to"));
    let folder = |account, name: &str, role| {
        store
            .upsert_folder(FolderSpec::local(account, name, role))
            .recv_blocking()
            .unwrap()
            .value
    };
    let inbox = folder(first, "INBOX", FolderRole::Inbox);
    let archive = folder(first, "Archive", FolderRole::Archive);
    let other_inbox = folder(second, "INBOX", FolderRole::Inbox);
    Fx {
        dir,
        store,
        inbox,
        other_inbox,
        archive,
    }
}

/// Message `n` in `folder` from `address`, `n` seconds after `NOW`.
fn mail(folder: FolderId, n: u32, address: &str, seen: bool) -> NewMessage {
    let date = NOW + i64::from(n);
    NewMessage {
        folder,
        date,
        headers: Headers {
            from: vec![Address::new("Avsender", address)],
            subject: format!("Melding {n}"),
            date: Some(date),
            message_id: Some(format!("pin-{n}@example.test")),
            ..Headers::default()
        },
        snippet: format!("Utdrag {n}"),
        search_text: format!("tekst {n}"),
        flags: if seen { Flags::SEEN } else { Flags::NONE },
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

fn ids(rows: &[MessageSummary]) -> Vec<MessageId> {
    rows.iter().map(|row| row.id).collect()
}

fn pin(store: &Store, target: PinTarget, kind: PinKind) -> Option<Pin> {
    store.pin(target, kind, NOW).recv_blocking().unwrap()
}

fn keep(store: &Store, mut pin: Pin) {
    pin.mode = PinMode::Keep;
    store.restore_pins(vec![pin]).recv_blocking().unwrap();
}

/// Whether `row` belongs in a Pinned section under `pins`.
fn leads(row: &MessageSummary, pins: &[Pin]) -> bool {
    pins.iter().any(|pin| {
        let named = match pin.kind {
            PinKind::Sender => sender_key(&row.sender_email) == pin.key,
            PinKind::Domain => domain_key(&row.sender_email) == pin.key,
        };
        named && (pin.mode == PinMode::Keep || !row.flags.is_seen())
    })
}

fn newest_first(rows: &[MessageSummary]) -> bool {
    rows.windows(2)
        .all(|pair| (pair[0].date, pair[0].id.0) > (pair[1].date, pair[1].id.0))
}

/// Asserts that `rows`, a whole Inbox view, is its newest pinned rows (at most `PINNED_MAX`),
/// then the rest by date; and that every row knows whether its sender is pinned. Gives the
/// section's length.
fn assert_pinned_first(rows: &[MessageSummary], pins: &[Pin], context: &str) -> usize {
    let mut by_date = rows.to_vec();
    by_date.sort_by_key(|row| std::cmp::Reverse((row.date, row.id.0)));
    let section: Vec<MessageSummary> = by_date
        .iter()
        .filter(|row| leads(row, pins))
        .take(PINNED_MAX)
        .cloned()
        .collect();
    let rest: Vec<MessageSummary> = by_date
        .iter()
        .filter(|row| !section.iter().any(|pinned| pinned.id == row.id))
        .cloned()
        .collect();
    assert!(newest_first(&section) && newest_first(&rest), "{context}");
    let expected: Vec<MessageId> = ids(&section).into_iter().chain(ids(&rest)).collect();
    assert_eq!(ids(rows), expected, "{context}");
    for row in rows {
        let named = pins.iter().any(|pin| match pin.kind {
            PinKind::Sender => sender_key(&row.sender_email) == pin.key,
            PinKind::Domain => domain_key(&row.sender_email) == pin.key,
        });
        assert_eq!(row.pinned, named, "{context}: row {:?}", row.id);
    }
    section.len()
}

#[test]
fn pinned_mail_leads_the_inboxes_while_unread_or_kept() {
    let fx = fixture();
    let ids = ingest(
        &fx.store,
        vec![
            mail(fx.inbox, 1, "regning@kraft.example", false),
            mail(fx.inbox, 2, "anna@example.test", false),
            mail(fx.inbox, 3, "trener@klatring.example", true),
            mail(fx.inbox, 4, "Regning@Kraft.example", true),
            mail(fx.inbox, 5, "anna@example.test", true),
            mail(fx.other_inbox, 6, "leder@post.klatring.example", false),
            mail(fx.archive, 7, "regning@kraft.example", false),
            mail(fx.inbox, 8, "ola@example.test", false),
        ],
    );
    let inbox = ViewSpec::folder(fx.inbox);
    assert!(!whole(&fx.store, inbox).rows.iter().any(|row| row.pinned));

    let bills = pin(&fx.store, PinTarget::Message(ids[0]), PinKind::Sender).unwrap();
    assert_eq!(bills.key, "regning@kraft.example");
    assert_eq!(bills.mode, PinMode::UntilRead);
    assert!(
        !format!("{bills:?}").contains("kraft"),
        "no address in Debug"
    );
    let club = pin(
        &fx.store,
        PinTarget::Key("klatring.example".into()),
        PinKind::Domain,
    )
    .unwrap();
    keep(&fx.store, club);
    let pins = fx.store.pins().recv_blocking().unwrap();
    assert_eq!(
        pins.iter()
            .map(|pin| (pin.kind, pin.key.as_str(), pin.mode))
            .collect::<Vec<_>>(),
        [
            (PinKind::Sender, "regning@kraft.example", PinMode::UntilRead),
            (PinKind::Domain, "klatring.example", PinMode::Keep),
        ]
    );

    // The unread bill and the club's mail, read or not, lead the Inbox; the read bill doesn't.
    let rows = whole(&fx.store, inbox).rows;
    assert_eq!(
        self::ids(&rows),
        [ids[2], ids[0], ids[7], ids[4], ids[3], ids[1]]
    );
    assert_eq!(assert_pinned_first(&rows, &pins, "inbox"), 2);
    let opened = fx
        .store
        .open_view(OpenView {
            spec: inbox,
            first_page: 3,
            section_starts: vec![NOW + 6, NOW + 4, NOW],
        })
        .recv_blocking()
        .unwrap();
    assert_eq!((opened.total, opened.pinned), (6, 2));
    // Dated at or after NOW + 6: message 8; NOW + 4: also 4 and 5; NOW: everything.
    assert_eq!(opened.section_counts, [3, 5, 6]);
    assert_eq!(self::ids(&opened.rows), [ids[2], ids[0], ids[7]]);

    // All Inboxes has the other account's club mail too; the Archive keeps its dates.
    let unified = whole(&fx.store, ViewSpec::unified_inbox()).rows;
    assert_eq!(assert_pinned_first(&unified, &pins, "all inboxes"), 3);
    assert_eq!(self::ids(&unified[..3]), [ids[5], ids[2], ids[0]]);
    let archive = whole(&fx.store, ViewSpec::folder(fx.archive)).rows;
    assert_eq!(self::ids(&archive), [ids[6]]);
    assert!(archive[0].pinned);

    // The chips filter the section like the rest.
    let unread = whole(&fx.store, inbox.with_filter(ViewFilter::Unread)).rows;
    assert_eq!(self::ids(&unread), [ids[0], ids[7], ids[1]]);
    assert!(
        whole(&fx.store, inbox.with_filter(ViewFilter::Starred))
            .rows
            .is_empty()
    );

    // Every page is the matching run of the whole view, from either end.
    for offset in 0..7 {
        for limit in 0..7 {
            let page = fx.store.page(inbox, offset, limit).recv_blocking().unwrap();
            let end = (offset + limit).min(6) as usize;
            let start = (offset as usize).min(end);
            assert_eq!(page.rows, rows[start..end], "{offset}+{limit}");
        }
    }

    // A pinned sender's message elsewhere doesn't pin the Archive; pinning twice changes nothing.
    let again = pin(&fx.store, PinTarget::Message(ids[6]), PinKind::Sender).unwrap();
    assert_eq!(again.added, bills.added);
    assert_eq!(fx.store.pins().recv_blocking().unwrap(), pins);
}

/// The groups of `spec` as `(key, pinned)`, in their order.
fn groups(store: &Store, spec: ViewSpec, by: GroupBy) -> Vec<(String, bool)> {
    store
        .sender_groups(spec, by)
        .recv_blocking()
        .unwrap()
        .into_iter()
        .map(|group| (group.key, group.pinned))
        .collect()
}

/// Which groups of `spec` the store put first (its Pinned section), in their order.
fn leading(store: &Store, spec: ViewSpec, by: GroupBy) -> Vec<bool> {
    store
        .sender_groups(spec, by)
        .recv_blocking()
        .unwrap()
        .into_iter()
        .map(|group| group.leads)
        .collect()
}

fn keys(list: &[(&str, bool)]) -> Vec<(String, bool)> {
    list.iter()
        .map(|(key, pinned)| ((*key).to_owned(), *pinned))
        .collect()
}

/// The reporter's case on #26: a club's domain pinned *always*, its mail all read and older than
/// the rest, comes first in Group by Domain and its senders first in Group by Sender; a bill's
/// sender pinned *until read* comes first only while it has unread mail. Only in the Inboxes;
/// elsewhere the groups keep their dates and show the pin.
#[test]
fn pinned_groups_come_first_in_the_inboxes() {
    let fx = fixture();
    let ids = ingest(
        &fx.store,
        vec![
            mail(fx.inbox, 1, "trener@klatring.example", true),
            mail(fx.inbox, 2, "leder@post.klatring.example", true),
            mail(fx.inbox, 3, "regning@kraft.example", true),
            mail(fx.inbox, 4, "anna@example.test", false),
            mail(fx.inbox, 5, "nyheter@avisa.example", true),
            mail(fx.other_inbox, 6, "ola@example.test", false),
            mail(fx.archive, 7, "trener@klatring.example", true),
            mail(fx.archive, 8, "anna@example.test", true),
        ],
    );
    let inbox = ViewSpec::folder(fx.inbox);
    let by_date = keys(&[
        ("avisa.example", false),
        ("example.test", false),
        ("kraft.example", false),
        ("klatring.example", false),
    ]);
    assert_eq!(groups(&fx.store, inbox, GroupBy::Domain), by_date);

    // Pinned until read, the club's read mail stays where its date puts it, with a pin.
    let club = pin(&fx.store, PinTarget::Message(ids[0]), PinKind::Domain).unwrap();
    assert_eq!(club.key, "klatring.example");
    assert_eq!(
        groups(&fx.store, inbox, GroupBy::Domain),
        keys(&[
            ("avisa.example", false),
            ("example.test", false),
            ("kraft.example", false),
            ("klatring.example", true),
        ])
    );
    assert_eq!(
        leading(&fx.store, inbox, GroupBy::Domain),
        [false, false, false, false]
    );
    // Always: it comes first, read or not, in either grouping, its senders newest first.
    keep(&fx.store, club);
    assert_eq!(
        groups(&fx.store, inbox, GroupBy::Domain),
        keys(&[
            ("klatring.example", true),
            ("avisa.example", false),
            ("example.test", false),
            ("kraft.example", false),
        ])
    );
    assert_eq!(
        leading(&fx.store, inbox, GroupBy::Domain),
        [true, false, false, false]
    );
    assert_eq!(
        groups(&fx.store, inbox, GroupBy::Sender),
        keys(&[
            ("leder@post.klatring.example", true),
            ("trener@klatring.example", true),
            ("nyheter@avisa.example", false),
            ("anna@example.test", false),
            ("regning@kraft.example", false),
        ])
    );

    // The bill's sender until read: first only while one is unread.
    pin(&fx.store, PinTarget::Message(ids[2]), PinKind::Sender).unwrap();
    let senders = groups(&fx.store, inbox, GroupBy::Sender);
    assert_eq!(senders[2], ("nyheter@avisa.example".to_owned(), false));
    assert_eq!(senders[4], ("regning@kraft.example".to_owned(), true));
    fx.store
        .set_flags(vec![ids[2]], Flags::NONE, Flags::SEEN)
        .recv_blocking()
        .unwrap();
    assert_eq!(
        groups(&fx.store, inbox, GroupBy::Sender),
        keys(&[
            ("regning@kraft.example", true),
            ("leder@post.klatring.example", true),
            ("trener@klatring.example", true),
            ("nyheter@avisa.example", false),
            ("anna@example.test", false),
        ])
    );
    // A sender's pin moves its domain's group too, while it leads (#26, on 1.1.3: the bill's
    // sender kept its domain's group in its date).
    assert_eq!(
        groups(&fx.store, inbox, GroupBy::Domain),
        keys(&[
            ("kraft.example", true),
            ("klatring.example", true),
            ("avisa.example", false),
            ("example.test", false),
        ])
    );
    // The Unread chip: the club has no unread mail, so it has no group there.
    assert_eq!(
        groups(
            &fx.store,
            inbox.with_filter(ViewFilter::Unread),
            GroupBy::Sender
        ),
        keys(&[
            ("regning@kraft.example", true),
            ("anna@example.test", false),
        ])
    );

    // Read, the bill's domain goes back to its date, with its pin.
    fx.store
        .set_flags(vec![ids[2]], Flags::SEEN, Flags::NONE)
        .recv_blocking()
        .unwrap();
    assert_eq!(
        groups(&fx.store, inbox, GroupBy::Domain),
        keys(&[
            ("klatring.example", true),
            ("avisa.example", false),
            ("example.test", false),
            ("kraft.example", true),
        ])
    );
    // Kept pinned, it leads read or not, as in Group by Sender.
    let bills = fx.store.pins().recv_blocking().unwrap();
    let bills = bills
        .into_iter()
        .find(|pin| pin.kind == PinKind::Sender)
        .unwrap();
    keep(&fx.store, bills.clone());
    assert_eq!(
        groups(&fx.store, inbox, GroupBy::Domain)[..2],
        keys(&[("kraft.example", true), ("klatring.example", true)])
    );
    assert_eq!(
        groups(&fx.store, inbox, GroupBy::Sender)[..3],
        keys(&[
            ("regning@kraft.example", true),
            ("leder@post.klatring.example", true),
            ("trener@klatring.example", true),
        ])
    );
    fx.store.restore_pins(vec![bills]).recv_blocking().unwrap();

    // All Inboxes too; the Archive keeps its dates.
    assert_eq!(
        groups(&fx.store, ViewSpec::unified_inbox(), GroupBy::Domain),
        keys(&[
            ("klatring.example", true),
            ("example.test", false),
            ("avisa.example", false),
            ("kraft.example", true),
        ])
    );
    assert_eq!(
        groups(&fx.store, ViewSpec::folder(fx.archive), GroupBy::Domain),
        keys(&[("example.test", false), ("klatring.example", true)])
    );
    assert_eq!(
        leading(&fx.store, ViewSpec::folder(fx.archive), GroupBy::Domain),
        [false, false]
    );
    assert_eq!(
        groups(&fx.store, ViewSpec::folder(fx.archive), GroupBy::Sender),
        keys(&[
            ("anna@example.test", false),
            ("trener@klatring.example", true),
        ])
    );
}

#[test]
fn the_section_holds_the_newest_ten_and_the_rest_keep_their_dates() {
    let fx = fixture();
    let ids = ingest(
        &fx.store,
        (1..=30)
            .map(|n| {
                let from = if n % 2 == 0 {
                    "nytt@butikk.example"
                } else {
                    "anna@example.test"
                };
                mail(fx.inbox, n, from, false)
            })
            .collect(),
    );
    pin(&fx.store, PinTarget::Message(ids[1]), PinKind::Sender).unwrap();
    let pins = fx.store.pins().recv_blocking().unwrap();
    let rows = whole(&fx.store, ViewSpec::folder(fx.inbox)).rows;
    assert_eq!(assert_pinned_first(&rows, &pins, "capped"), PINNED_MAX);
    // Messages 30, 28 … 12 lead; 10 and older pinned ones stay in their dates.
    assert_eq!(rows[0].id, ids[29]);
    assert_eq!(rows[PINNED_MAX - 1].id, ids[11]);
    assert_eq!(rows[PINNED_MAX].id, ids[28]);
    for offset in [0, 5, 9, 10, 11, 14, 15, 16, 20, 29] {
        for limit in [1, 3, 7, 30] {
            let page = fx
                .store
                .page(ViewSpec::folder(fx.inbox), offset, limit)
                .recv_blocking()
                .unwrap();
            let end = (offset + limit).min(30) as usize;
            assert_eq!(page.rows, rows[offset as usize..end], "{offset}+{limit}");
        }
    }
}

#[test]
fn unpin_takes_the_senders_pins_off_and_undo_puts_them_back() {
    let fx = fixture();
    let ids = ingest(
        &fx.store,
        vec![
            mail(fx.inbox, 1, "trener@klatring.example", false),
            mail(fx.inbox, 2, "anna@example.test", false),
        ],
    );
    pin(&fx.store, PinTarget::Message(ids[0]), PinKind::Sender).unwrap();
    pin(&fx.store, PinTarget::Message(ids[0]), PinKind::Domain).unwrap();
    assert_eq!(
        self::ids(&whole(&fx.store, ViewSpec::folder(fx.inbox)).rows),
        [ids[0], ids[1]]
    );
    let removed = fx.store.unpin(ids[0]).recv_blocking().unwrap();
    assert_eq!(removed.len(), 2);
    assert!(fx.store.pins().recv_blocking().unwrap().is_empty());
    assert_eq!(
        self::ids(&whole(&fx.store, ViewSpec::folder(fx.inbox)).rows),
        [ids[1], ids[0]]
    );
    assert!(fx.store.unpin(ids[0]).recv_blocking().unwrap().is_empty());
    fx.store
        .restore_pins(removed.clone())
        .recv_blocking()
        .unwrap();
    assert_eq!(fx.store.pins().recv_blocking().unwrap(), removed);
    fx.store
        .remove_pin(PinKind::Domain, "klatring.example".into())
        .recv_blocking()
        .unwrap();
    fx.store
        .remove_pin(PinKind::Sender, "trener@klatring.example".into())
        .recv_blocking()
        .unwrap();
    assert!(fx.store.pins().recv_blocking().unwrap().is_empty());
    // Nothing to pin: a message that is gone, or a sender without a domain.
    assert_eq!(
        pin(
            &fx.store,
            PinTarget::Message(MessageId(999)),
            PinKind::Sender
        ),
        None
    );
    let odd = ingest(&fx.store, vec![mail(fx.inbox, 3, "undisclosed", false)]);
    assert_eq!(
        pin(&fx.store, PinTarget::Message(odd[0]), PinKind::Domain),
        None
    );
}

#[test]
fn change_events_keep_pinned_lists_exact() {
    let fx = fixture();
    const SENDERS: [&str; 6] = [
        "regning@kraft.example",
        "trener@klatring.example",
        "leder@post.klatring.example",
        "anna@example.test",
        "ola@example.test",
        "nytt@butikk.example",
    ];
    let folders = [fx.inbox, fx.inbox, fx.other_inbox, fx.archive];
    let mut rng = Rng(26);
    let mut serial = 0u32;
    let mut fresh = |rng: &mut Rng, count: usize| -> Vec<NewMessage> {
        (0..count)
            .map(|_| {
                serial += 1;
                let folder = folders[rng.below(folders.len() as u64) as usize];
                let from = SENDERS[rng.below(SENDERS.len() as u64) as usize];
                let mut message = mail(folder, serial * 7 % 1_000, from, rng.below(2) == 0);
                message.headers.message_id = Some(format!("event-{serial}@example.test"));
                if rng.below(5) == 0 {
                    message.flags = message.flags | Flags::FLAGGED;
                }
                message
            })
            .collect()
    };
    let mut live = ingest(&fx.store, fresh(&mut rng, 60));
    let bills = pin(&fx.store, PinTarget::Message(live[0]), PinKind::Sender).unwrap();
    let club = pin(
        &fx.store,
        PinTarget::Key("klatring.example".into()),
        PinKind::Domain,
    )
    .unwrap();
    keep(&fx.store, club.clone());

    let events = fx.store.subscribe();
    let specs = [
        ViewSpec::folder(fx.inbox),
        ViewSpec::folder(fx.inbox).with_filter(ViewFilter::Unread),
        ViewSpec::folder(fx.inbox).with_filter(ViewFilter::Starred),
        ViewSpec::unified_inbox(),
        ViewSpec::folder(fx.archive),
    ];
    let mut models: Vec<Model> = specs
        .iter()
        .map(|spec| {
            let opened = fx
                .store
                .open_view(OpenView {
                    spec: *spec,
                    first_page: u32::MAX,
                    section_starts: Vec::new(),
                })
                .recv_blocking()
                .unwrap();
            Model {
                view: opened.view,
                rows: opened.rows,
                version: opened.version,
            }
        })
        .collect();
    let mut counts = (0, 0);
    for step in 0..200 {
        let pick = |rng: &mut Rng, live: &[MessageId]| live[rng.below(live.len() as u64) as usize];
        let version = match rng.below(12) {
            0..=2 => {
                let count = 1 + rng.below(3) as usize;
                let reply = fx
                    .store
                    .ingest(fresh(&mut rng, count))
                    .recv_blocking()
                    .unwrap();
                live.extend(reply.ids.iter().flatten());
                reply.version
            }
            3..=7 => {
                let picks: Vec<MessageId> = (0..1 + rng.below(3))
                    .map(|_| pick(&mut rng, &live))
                    .collect();
                let (add, remove) = match rng.below(3) {
                    0 => (Flags::SEEN, Flags::NONE),
                    1 => (Flags::NONE, Flags::SEEN),
                    _ => (Flags::FLAGGED, Flags::NONE),
                };
                fx.store
                    .set_flags(picks, add, remove)
                    .recv_blocking()
                    .unwrap()
            }
            8 | 9 => {
                let picks: Vec<MessageId> = (0..1 + rng.below(2))
                    .map(|_| pick(&mut rng, &live))
                    .collect();
                live.retain(|id| !picks.contains(id));
                fx.store.remove(picks).recv_blocking().unwrap().version
            }
            10 => {
                fx.store
                    .unpin(pick(&mut rng, &live))
                    .recv_blocking()
                    .unwrap();
                drain(&fx.store, &events, &mut models, &mut counts);
                fx.store
                    .restore_pins(vec![bills.clone(), club.clone()])
                    .recv_blocking()
                    .unwrap();
                drain(&fx.store, &events, &mut models, &mut counts);
                models[0].version
            }
            _ => {
                let mut changed = club.clone();
                changed.mode = if rng.below(2) == 0 {
                    PinMode::Keep
                } else {
                    PinMode::UntilRead
                };
                fx.store
                    .restore_pins(vec![changed])
                    .recv_blocking()
                    .unwrap();
                drain(&fx.store, &events, &mut models, &mut counts);
                models[0].version
            }
        };
        while models[0].version < version {
            let event = events.recv_blocking().unwrap();
            apply(&fx.store, &event, &mut models, &mut counts);
        }
        let pins = fx.store.pins().recv_blocking().unwrap();
        for model in &models {
            let page = whole(&fx.store, model.view.spec());
            assert_eq!(page.version, model.version, "step {step}");
            assert_eq!(
                page.rows,
                model.rows,
                "step {step}, {:?}",
                model.view.spec()
            );
            let context = format!("step {step}, {:?}", model.view.spec());
            if model.view.spec().scope == ViewScope::Folder(fx.archive) {
                assert!(newest_first(&model.rows), "{context}");
            } else {
                assert_pinned_first(&model.rows, &pins, &context);
            }
        }
    }
    let (splices, reloads) = counts;
    assert!(
        splices > 50 && reloads > 50,
        "{splices} splices, {reloads} reloads"
    );
    assert_eq!(
        fx.store.check().recv_blocking().unwrap(),
        Vec::<String>::new()
    );
}

/// Applies `event` to the models, counting splices and reloads.
fn apply(store: &Store, event: &ChangeEvent, models: &mut [Model], counts: &mut (u32, u32)) {
    for change in &event.views {
        match change.diff {
            ViewDiff::Splice { .. } => counts.0 += 1,
            ViewDiff::Reload { .. } => counts.1 += 1,
        }
    }
    for model in models {
        model.apply(store, event);
    }
}

/// Applies the events already published.
fn drain(
    store: &Store,
    events: &async_channel::Receiver<std::sync::Arc<ChangeEvent>>,
    models: &mut [Model],
    counts: &mut (u32, u32),
) {
    while let Ok(event) = events.try_recv() {
        apply(store, &event, models, counts);
    }
}

#[test]
fn v14_adds_the_pins_to_a_v13_store() {
    let fx = fixture();
    let ids = ingest(
        &fx.store,
        vec![mail(fx.inbox, 1, "regning@kraft.example", false)],
    );
    let path = fx.dir.path().join("store.sqlite");
    fx.store.close().unwrap();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(testing::UNDO_V14).unwrap();
    drop(conn);
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    assert!(store.pins().recv_blocking().unwrap().is_empty());
    pin(&store, PinTarget::Message(ids[0]), PinKind::Domain).unwrap();
    assert!(whole(&store, ViewSpec::folder(fx.inbox)).rows[0].pinned);
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
}

/// Random mailboxes and pins: the groups' order and pins match a model, in both groupings, in an
/// Inbox and in All Inboxes. A group leads when a pin on it does (*always*, or *until read* with
/// unread mail: the group's for its own pin, the sender's for a sender's pin on a domain group).
#[test]
fn pinned_groups_match_a_model() {
    const ADDRESSES: [&str; 10] = [
        "Trener@Klatring.example",
        "leder@post.klatring.example",
        "kafe@klatring.example",
        "Regning@Kraft.example",
        "kunde@kraft.example",
        "anna@example.test",
        "Ola@Example.test",
        "nyheter@avisa.example",
        "tips@avisa.example",
        "x@solo.example",
    ];
    for seed in 0..60u64 {
        let fx = fixture();
        let mut rng = Rng(seed);
        let folders = [fx.inbox, fx.other_inbox, fx.archive];
        let mut sent = Vec::new();
        for n in 0..60u32 {
            let folder = folders[rng.below(3) as usize];
            let address = ADDRESSES[rng.below(ADDRESSES.len() as u64) as usize];
            sent.push((folder, address, rng.below(3) != 0, n));
        }
        let ids = ingest(
            &fx.store,
            sent.iter()
                .map(|(folder, address, seen, n)| mail(*folder, *n, address, *seen))
                .collect(),
        );
        for _ in 0..3 {
            let id = ids[rng.below(ids.len() as u64) as usize];
            let kind = if rng.below(2) == 0 {
                PinKind::Sender
            } else {
                PinKind::Domain
            };
            if let Some(made) = pin(&fx.store, PinTarget::Message(id), kind)
                && rng.below(2) == 0
            {
                keep(&fx.store, made);
            }
        }
        let pins = fx.store.pins().recv_blocking().unwrap();
        let names = |pin: &Pin, address: &str| match pin.kind {
            PinKind::Sender => pin.key == sender_key(address),
            PinKind::Domain => pin.key == domain_key(address),
        };
        for (spec, shown) in [
            (ViewSpec::folder(fx.inbox), vec![fx.inbox]),
            (ViewSpec::unified_inbox(), vec![fx.inbox, fx.other_inbox]),
        ] {
            for by in [GroupBy::Sender, GroupBy::Domain] {
                let key = |address: &str| match by {
                    GroupBy::Domain => domain_key(address),
                    _ => sender_key(address),
                };
                let mail: Vec<_> = sent
                    .iter()
                    .filter(|(folder, ..)| shown.contains(folder))
                    .collect();
                let mut model: Vec<(String, u32, bool, bool)> = Vec::new();
                for (_, address, _, _) in &mail {
                    let group = key(address);
                    if model.iter().any(|(known, ..)| *known == group) {
                        continue;
                    }
                    let members: Vec<_> = mail.iter().filter(|m| key(m.1) == group).collect();
                    let latest = members.iter().map(|m| m.3).max().unwrap();
                    let (mut pinned, mut leads) = (false, false);
                    for pin in &pins {
                        let named: Vec<_> = members.iter().filter(|m| names(pin, m.1)).collect();
                        let counts = match (by, pin.kind) {
                            (GroupBy::Domain, PinKind::Sender) => !named.is_empty(),
                            _ => named.len() == members.len(),
                        };
                        if !counts {
                            continue;
                        }
                        pinned = true;
                        let unread = match (by, pin.kind) {
                            (GroupBy::Domain, PinKind::Sender) => named.iter().any(|m| !m.2),
                            _ => members.iter().any(|m| !m.2),
                        };
                        leads |= pin.mode == PinMode::Keep || unread;
                    }
                    model.push((group, latest, pinned, leads));
                }
                model.sort_by(|a, b| b.3.cmp(&a.3).then(b.1.cmp(&a.1)).then(a.0.cmp(&b.0)));
                let wanted_leads: Vec<bool> = model.iter().map(|group| group.3).collect();
                let wanted: Vec<(String, bool)> = model
                    .into_iter()
                    .map(|(key, _, pinned, _)| (key, pinned))
                    .collect();
                assert_eq!(
                    groups(&fx.store, spec, by),
                    wanted,
                    "seed {seed}, {by:?}, {spec:?}, {pins:?}"
                );
                assert_eq!(
                    leading(&fx.store, spec, by),
                    wanted_leads,
                    "seed {seed}, {by:?}, {spec:?}, {pins:?}"
                );
            }
        }
    }
}

#[test]
fn a_group_whose_address_has_non_ascii_capitals_pins_its_mail() {
    let fx = fixture();
    let ids = ingest(
        &fx.store,
        vec![mail(fx.inbox, 1, "ÅSE@Fjell.example", false)],
    );
    let inbox = ViewSpec::folder(fx.inbox);
    // Group by Sender's key is SQLite's `lower(trim(…))`, which folds ASCII letters only.
    let (key, _) = groups(&fx.store, inbox, GroupBy::Sender).remove(0);
    assert_eq!(key, "Åse@fjell.example");
    let pinned = pin(&fx.store, PinTarget::Key(key.clone()), PinKind::Sender).unwrap();
    assert_eq!(pinned.key, key);
    let rows = whole(&fx.store, inbox).rows;
    assert_eq!(self::ids(&rows), [ids[0]]);
    assert!(rows[0].pinned, "the group's pin matches its mail");
}
