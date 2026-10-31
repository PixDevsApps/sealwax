//! Pinned senders (issue #26): the Pinned section ahead of the dates, its paging, its change
//! events and the pins themselves.

use super::tests::{Model, Rng, scratch, whole};
use super::*;
use rusqlite::Connection;
use sealwax_domain::model::{Address, FolderRole, Headers, MessageSummary};
use sealwax_domain::senders::{domain_key, sender_key};

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
    conn.execute_batch(super::schema::UNDO_V14).unwrap();
    drop(conn);
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    assert!(store.pins().recv_blocking().unwrap().is_empty());
    pin(&store, PinTarget::Message(ids[0]), PinKind::Domain).unwrap();
    assert!(whole(&store, ViewSpec::folder(fx.inbox)).rows[0].pinned);
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
}
