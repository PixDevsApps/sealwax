//! Headless measurements of M6's store (ML-074, ML-071) on a fixture store: the first open of an
//! older store (the v9 migration fills the contacts), a typical draft saved (a 200 KB HTML body
//! and three parts) on an idle writer and while a bulk ingest runs, a draft read back, and the
//! composer's recipient suggestions for prefixes of every kind, with the fixture's contacts and
//! again with `--contacts` synthetic ones added. The fixture account is made a server account
//! and gets a folder of its own for the ingest, so run it on a copy of the store.

use crate::import::account_address;
use crate::stats::{ms, summary};
use anyhow::{Context, Result};
use sealwax_domain::compose::{Draft, DraftPart};
use sealwax_domain::model::{AccountId, Address, BlobKey, FolderRole, Headers};
use sealwax_infrastructure::store::{
    DraftSaved, FolderSpec, NewMessage, SaveDraft, Store, StoreOptions,
};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct Options {
    pub store: PathBuf,
    pub samples: usize,
    /// Synthetic contacts added for the second round of suggestions.
    pub contacts: usize,
    pub now: i64,
}

const PREFIXES: [&str; 12] = [
    "k",
    "a",
    "e",
    "s",
    "ka",
    "no",
    "kar",
    "ola",
    "berg",
    "example",
    "example.te",
    "zzq",
];

fn draft(account: AccountId, n: usize) -> Draft {
    let paragraph = "<p>Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod \
                     tempor incididunt ut labore et dolore magna aliqua.</p>";
    let mut html = String::with_capacity(200_100);
    while html.len() < 200_000 {
        html.push_str(paragraph);
    }
    Draft {
        account,
        from: Address::new("Fixture", "fixtures@example.test"),
        to: vec![Address::new("Kari Nordmann", "kari.nordmann@example.test")],
        cc: vec![Address::new("Ola Berg", "ola.berg@example.test")],
        subject: format!("Bench draft {n}"),
        html,
        message_id: format!("bench-draft-{n}@example.test"),
        parts: (0..3u8)
            .map(|part| DraftPart {
                name: format!("part-{part}.pdf"),
                mime_type: "application/pdf".into(),
                size: 100_000,
                blob: BlobKey([part + 1; 32]),
                content_id: None,
            })
            .collect(),
        ..Draft::default()
    }
}

fn save(
    store: &Store,
    id: Option<sealwax_domain::model::DraftId>,
    draft: Draft,
    now: i64,
) -> Result<(sealwax_domain::model::DraftId, Duration)> {
    let started = Instant::now();
    let saved = store
        .save_draft(SaveDraft {
            id,
            draft,
            open: true,
            now,
        })
        .recv_blocking()?;
    let elapsed = started.elapsed();
    match saved {
        DraftSaved::Saved { id, .. } => Ok((id, elapsed)),
        other => anyhow::bail!("not saved: {other:?}"),
    }
}

fn bulk(folder: sealwax_domain::model::FolderId, count: usize, now: i64) -> Vec<NewMessage> {
    (0..count)
        .map(|n| NewMessage {
            folder,
            uid: Some(n as u32 + 1),
            date: now - n as i64 * 60,
            headers: Headers {
                from: vec![Address::new(
                    format!("Bulk Sender {}", n % 500),
                    format!("bulk{}@example.test", n % 500),
                )],
                to: vec![Address::new("Fixture", "fixtures@example.test")],
                cc: vec![Address::new(
                    format!("Copy {}", n % 50),
                    format!("copy{}@example.test", n % 50),
                )],
                subject: format!("Bulk message {n}"),
                message_id: Some(format!("bulk-{n}@example.test")),
                ..Headers::default()
            },
            snippet: "Bulk".into(),
            search_text: "bulk message body text ".repeat(40),
            size: 2_000,
            ..NewMessage::default()
        })
        .collect()
}

fn suggestions(store: &Store, account: AccountId, samples: usize, label: &str) -> Result<()> {
    println!("\n### Recipient suggestions, {label} (limit 8, warm)");
    let mut all = Vec::new();
    for prefix in PREFIXES {
        let mut times = Vec::with_capacity(samples);
        let mut found = 0;
        let mut first = String::new();
        for _ in 0..samples {
            let started = Instant::now();
            let contacts = store.recipients(account, prefix, 8).recv_blocking()?;
            times.push(started.elapsed());
            found = contacts.len();
            first = contacts
                .first()
                .map(|contact| format!("{} seen, {} written", contact.seen, contact.written))
                .unwrap_or_default();
        }
        all.extend(times.iter().copied());
        println!(
            "- `{prefix}`: {} — {found} found{}",
            summary(&times),
            if first.is_empty() {
                String::new()
            } else {
                format!(", first {first}")
            }
        );
    }
    println!("- every prefix together: {}", summary(&all));
    Ok(())
}

/// Adds `count` synthetic contacts to `account`, with the terms the store would give them.
fn add_contacts(store: &PathBuf, account: AccountId, count: usize) -> Result<()> {
    let conn = rusqlite::Connection::open(store)?;
    let tx = conn.unchecked_transaction()?;
    {
        let mut contact = tx.prepare(
            "INSERT OR IGNORE INTO contacts(account_id, address, name, seen, written, last) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) RETURNING id",
        )?;
        let mut term =
            tx.prepare("INSERT OR IGNORE INTO contact_terms(term, contact) VALUES (?1, ?2)")?;
        let first = ["kari", "ola", "anne", "per", "eva", "nils", "sara", "erik"];
        let last = [
            "berg", "dahl", "lund", "strand", "haug", "moen", "vik", "nord",
        ];
        let domains = ["example.test", "mail.example.test", "corp.example.test"];
        for n in 0..count {
            let given = first[n % first.len()];
            let family = last[(n / first.len()) % last.len()];
            let domain = domains[n % domains.len()];
            let address = format!("{given}.{family}{n}@{domain}");
            let name = format!("{given} {family}");
            let id: Option<i64> = contact
                .query_row(
                    rusqlite::params![
                        account.0,
                        address,
                        name,
                        (n % 40) as i64,
                        (n % 7 == 0) as i64,
                        1_700_000_000 + n as i64
                    ],
                    |row| row.get(0),
                )
                .ok();
            let Some(id) = id else {
                continue;
            };
            for word in [address.as_str(), domain, given, family] {
                term.execute(rusqlite::params![word, id])?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn run(options: &Options) -> Result<()> {
    println!("## M6 store (bench-compose)");
    println!("- SQLite {}", rusqlite::version());
    let address = account_address(crate::import::ACCOUNT_NAME);
    let started = Instant::now();
    let store = Store::open(&options.store, StoreOptions::default())?;
    println!(
        "- first open (migration to v9 included when the store is older): {:.1} ms",
        ms(started.elapsed())
    );
    store.close()?;
    let conn = rusqlite::Connection::open(&options.store)?;
    conn.execute(
        "UPDATE accounts SET key = 'bench-compose' WHERE address = ?1 AND key IS NULL",
        [&address],
    )?;
    let (account, contacts, terms, messages): (i64, i64, i64, i64) = conn.query_row(
        "SELECT a.id, (SELECT count(*) FROM contacts WHERE account_id = a.id), \
           (SELECT count(*) FROM contact_terms), (SELECT count(*) FROM messages) \
         FROM accounts a WHERE a.address = ?1",
        [&address],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    drop(conn);
    let account = AccountId(account);
    println!("- {messages} messages, {contacts} contacts, {terms} terms");
    let store = Store::open(&options.store, StoreOptions::default())?;

    println!("\n### Draft saves (200 KB HTML, 3 parts), idle writer");
    let (id, _) = save(&store, None, draft(account, 0), options.now)?;
    let mut idle = Vec::new();
    for n in 0..options.samples {
        idle.push(save(&store, Some(id), draft(account, n), options.now)?.1);
    }
    println!("- save → reply: {}", summary(&idle));
    let mut reads = Vec::new();
    for _ in 0..options.samples {
        let started = Instant::now();
        store.draft(id).recv_blocking()?.context("draft gone")?;
        reads.push(started.elapsed());
    }
    println!("- read back: {}", summary(&reads));

    println!("\n### Draft saves during a bulk ingest");
    let folder = store
        .upsert_folder(FolderSpec::local(
            account,
            "Bench ingest",
            FolderRole::Other,
        ))
        .recv_blocking()?
        .value;
    let ingest_started = Instant::now();
    let ingest = store.ingest(bulk(folder, 20_000, options.now));
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || done.send(ingest.recv_blocking()));
    let mut busy = Vec::new();
    let mut n = 0;
    loop {
        if let Ok(result) = finished.try_recv() {
            let result = result?;
            println!(
                "- the ingest: {} messages in {:.0} ms ({} header batches, header time {:.0} ms, \
                 full text {:.0} ms)",
                result.ids.len(),
                ms(ingest_started.elapsed()),
                result.batches,
                ms(result.header_time),
                ms(result.index_time)
            );
            break;
        }
        busy.push(save(&store, Some(id), draft(account, n), options.now)?.1);
        n += 1;
        std::thread::sleep(Duration::from_millis(20));
    }
    println!("- save → reply while it ran: {}", summary(&busy));
    store.close()?;
    let store = Store::open(&options.store, StoreOptions::default())?;
    suggestions(&store, account, options.samples, "the fixture's contacts")?;
    store.close()?;
    if options.contacts > 0 {
        add_contacts(&options.store, account, options.contacts)?;
        let store = Store::open(&options.store, StoreOptions::default())?;
        suggestions(
            &store,
            account,
            options.samples,
            &format!("{} synthetic contacts added", options.contacts),
        )?;
        let problems = store.check().recv_blocking()?;
        println!(
            "\n- check(): {}",
            if problems.is_empty() {
                "ok".into()
            } else {
                problems.join("; ")
            }
        );
        store.close()?;
    }
    Ok(())
}
