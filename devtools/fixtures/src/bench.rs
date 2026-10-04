//! Headless measurements of a fixture store against S3's bars (PLAN.md, S3): folder open,
//! paging, the first 50 search results and the one-insert diff. Cold runs close the store,
//! evict its files from the page cache (`posix_fadvise(DONTNEED)`, no root needed) and open new
//! connections.

use crate::import::{ACCOUNT_NAME, account_address, store_files};
use crate::stats::{ms, summary};
use anyhow::{Context, Result, bail};
use sealwax_domain::model::{Flags, FolderId, FolderRole, MessageId};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::ingest::{self, RawMessage};
use sealwax_infrastructure::store::{
    ChangeEvent, OpenView, Store, StoreOptions, StoreVersion, ViewDiff, ViewFilter, ViewSpec,
};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub struct Options {
    pub store: PathBuf,
    pub blobs: PathBuf,
    pub cold: usize,
    pub warm: usize,
    pub inserts: usize,
    pub now: i64,
}

const SEARCHES: [(&str, &str, bool); 11] = [
    ("common Norwegian word", "møte", false),
    ("very common English word", "the", false),
    ("two words", "budsjett kontrakt", false),
    ("phrase", "\"project update\"", false),
    ("prefix", "prosjekt*", false),
    ("sender name", "Hansen", false),
    ("rare word with æøå", "blåbærsyltetøy", false),
    ("Chinese word (bigrams)", "会议", false),
    ("Japanese word (bigrams)", "進捗", false),
    ("no match", "zzqxv", false),
    ("word, INBOX only", "faktura", true),
];

fn evict(store: &std::path::Path) -> Result<()> {
    for file in store_files(store) {
        let Ok(handle) = File::open(&file) else {
            continue;
        };
        handle.sync_all()?;
        let status =
            unsafe { libc::posix_fadvise(handle.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
        if status != 0 {
            bail!("posix_fadvise failed on {}: {status}", file.display());
        }
    }
    Ok(())
}

fn open(options: &Options, blobs: &BlobStore) -> Result<Store> {
    Ok(Store::open(
        &options.store,
        StoreOptions {
            blobs: Some(blobs.clone()),
            ..StoreOptions::default()
        },
    )?)
}

/// Today, yesterday, a week and a month back, at UTC midnight.
fn section_starts(now: i64) -> Vec<i64> {
    let today = now - now.rem_euclid(86_400);
    vec![
        today,
        today - 86_400,
        today - 6 * 86_400,
        today - 30 * 86_400,
    ]
}

fn open_view(store: &Store, spec: ViewSpec, now: i64) -> Result<(Duration, u32)> {
    let started = Instant::now();
    let opened = store
        .open_view(OpenView {
            spec,
            first_page: 100,
            section_starts: section_starts(now),
        })
        .recv_blocking()?;
    Ok((started.elapsed(), opened.total))
}

/// Stamps each change event with its arrival time.
fn listen(store: &Store) -> mpsc::Receiver<(Arc<ChangeEvent>, Instant)> {
    let events = store.subscribe();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        while let Ok(event) = events.recv_blocking() {
            if sender.send((event, Instant::now())).is_err() {
                break;
            }
        }
    });
    receiver
}

fn wait_for(
    events: &mpsc::Receiver<(Arc<ChangeEvent>, Instant)>,
    version: StoreVersion,
) -> Result<(Arc<ChangeEvent>, Instant)> {
    loop {
        let (event, at) = events
            .recv_timeout(Duration::from_secs(10))
            .context("no change event")?;
        if event.version == version {
            return Ok((event, at));
        }
    }
}

fn new_message(run: u64, n: usize, kind: usize, date: i64, parent: Option<&str>) -> Vec<u8> {
    let threading = match parent {
        Some(parent) => format!("In-Reply-To: <{parent}>\r\nReferences: <{parent}>\r\n"),
        None => String::new(),
    };
    let body = match kind {
        3 => "Content-Type: text/html; charset=utf-8\r\n\r\n<html><body><table><tr><td>\
              <h1>Nyheter</h1><p>Ukens <b>beste</b> turer i fjellet.</p></td></tr></table>\
              </body></html>\r\n"
            .to_owned(),
        _ => format!(
            "Content-Type: text/plain; charset=utf-8\r\n\r\nHei,\r\n\r\nmåling nummer {n}, \
             blåbærsyltetøy på fredag.\r\n"
        ),
    };
    format!(
        "From: Måler {n} <maaler{n}@example.test>\r\nTo: alex.berg@example.test\r\n\
         Subject: Ny melding {n}\r\nDate: {}\r\nMessage-ID: <bench-{run}-{n}-{date}@example.test>\r\n\
         {threading}MIME-Version: 1.0\r\n{body}",
        crate::mime::date(date, 0)
    )
    .into_bytes()
}

pub fn run(options: &Options) -> Result<()> {
    let blobs = BlobStore::open(&options.blobs)?;
    let mut store = open(options, &blobs)?;
    let folders = store.folders().recv_blocking()?.value;
    let accounts = store.accounts().recv_blocking()?.value;
    let account = accounts
        .iter()
        .find(|account| account.address == account_address(ACCOUNT_NAME))
        .context("no fixture account; run `import` first")?;
    let inbox = folders
        .iter()
        .find(|folder| folder.account == account.id && folder.role == FolderRole::Inbox)
        .context("no fixture INBOX")?;
    let inbox_id: FolderId = inbox.id;
    let messages: u32 = folders.iter().map(|folder| folder.total).sum();
    let bytes: u64 = store_files(&options.store)
        .iter()
        .filter_map(|file| std::fs::metadata(file).ok())
        .map(|meta| meta.len())
        .sum();
    println!("## Store");
    println!("- SQLite {}", rusqlite::version());
    println!(
        "- {messages} messages in {} folders; INBOX {} ({} unread); store files {:.1} MiB",
        folders.len(),
        inbox.total,
        inbox.unread,
        bytes as f64 / 1_048_576.0
    );

    let views = [
        ("INBOX", ViewSpec::folder(inbox_id)),
        (
            "INBOX unread",
            ViewSpec::folder(inbox_id).with_filter(ViewFilter::Unread),
        ),
        (
            "INBOX starred",
            ViewSpec::folder(inbox_id).with_filter(ViewFilter::Starred),
        ),
        (
            "INBOX attachments",
            ViewSpec::folder(inbox_id).with_filter(ViewFilter::Attachments),
        ),
        ("Unified inbox", ViewSpec::unified_inbox()),
    ];

    println!("\n## Folder open (count + 4 section counts + first 100 rows, one snapshot)");
    let mut cold_open = Vec::new();
    for (name, spec) in views {
        let mut cold = Vec::new();
        let mut rows = 0;
        for _ in 0..options.cold {
            store.close()?;
            evict(&options.store)?;
            let started = Instant::now();
            store = open(options, &blobs)?;
            cold_open.push(started.elapsed());
            let (elapsed, total) = open_view(&store, spec, options.now)?;
            cold.push(elapsed);
            rows = total;
        }
        let mut warm = Vec::new();
        for _ in 0..options.warm {
            warm.push(open_view(&store, spec, options.now)?.0);
        }
        println!(
            "- {name} ({rows} rows): cold {} | warm {}",
            summary(&cold),
            summary(&warm)
        );
    }
    println!(
        "- store open (connections, migration check, threads; cache evicted): {}",
        summary(&cold_open)
    );

    println!("\n## Pages of 100 rows, INBOX, warm");
    let total = inbox.total;
    for fraction in [0u32, 25, 50, 75, 100] {
        let offset = (u64::from(total.saturating_sub(100)) * u64::from(fraction) / 100) as u32;
        let mut samples = Vec::new();
        for _ in 0..options.warm {
            let started = Instant::now();
            let page = store
                .page(ViewSpec::folder(inbox_id), offset, 100)
                .recv_blocking()?;
            samples.push(started.elapsed());
            if page.rows.len() != 100.min(total as usize) {
                bail!("page at {offset} had {} rows", page.rows.len());
            }
        }
        println!("- offset {offset}: {}", summary(&samples));
    }

    let unified = store
        .open_view(OpenView {
            spec: ViewSpec::unified_inbox(),
            first_page: 0,
            section_starts: Vec::new(),
        })
        .recv_blocking()?
        .total;
    let inboxes = folders
        .iter()
        .filter(|folder| folder.role == FolderRole::Inbox)
        .count();
    println!("\n## Pages of 100 rows, unified inbox ({inboxes} inboxes, {unified} rows), warm");
    for fraction in [0u32, 25, 50, 75, 100] {
        let offset = (u64::from(unified.saturating_sub(100)) * u64::from(fraction) / 100) as u32;
        let mut samples = Vec::new();
        for _ in 0..options.warm {
            let started = Instant::now();
            store
                .page(ViewSpec::unified_inbox(), offset, 100)
                .recv_blocking()?;
            samples.push(started.elapsed());
        }
        println!("- offset {offset}: {}", summary(&samples));
    }

    println!("\n## Search: newest 50 matches with their rows");
    for (name, query, inbox_only) in SEARCHES {
        let folder = inbox_only.then_some(inbox_id);
        if options.cold > 0 {
            store.close()?;
            evict(&options.store)?;
            store = open(options, &blobs)?;
        }
        let started = Instant::now();
        let found = store.search(query, folder, 50).recv_blocking()?.value.len();
        let cold = started.elapsed();
        let mut warm = Vec::new();
        for _ in 0..options.warm {
            let started = Instant::now();
            store.search(query, folder, 50).recv_blocking()?;
            warm.push(started.elapsed());
        }
        println!(
            "- {name} `{query}`: {found} rows; cold {:.2} ms | warm {}",
            ms(cold),
            summary(&warm)
        );
    }

    println!("\n## One new message with its list diff");
    let events = listen(&store);
    let mut handles = Vec::new();
    for spec in [
        ViewSpec::folder(inbox_id),
        ViewSpec::folder(inbox_id).with_filter(ViewFilter::Unread),
        ViewSpec::unified_inbox(),
    ] {
        let opened = store
            .open_view(OpenView {
                spec,
                first_page: 100,
                section_starts: Vec::new(),
            })
            .recv_blocking()?;
        handles.push(opened.view);
    }
    let sample = store
        .page(ViewSpec::folder(inbox_id), total / 3, 100)
        .recv_blocking()?
        .rows;
    let mut parents = Vec::new();
    for row in sample.iter().take(50) {
        if let Some(detail) = store.message(row.id).recv_blocking()?.value
            && let Some(id) = detail.headers.message_id
        {
            parents.push(id);
        }
    }
    let mut writer = Vec::new();
    let mut to_event = Vec::new();
    let mut whole = Vec::new();
    let mut inserted: Vec<MessageId> = Vec::new();
    let mut splices = 0;
    let run = store.accounts().recv_blocking()?.version.0;
    let mut state = 0x5eed_u64;
    for n in 0..options.inserts {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        let kind = n % 4;
        let date = match kind {
            2 => options.now - 86_400 * (30 + (state >> 33) as i64 % 1_500),
            _ => options.now + n as i64,
        };
        let parent =
            (kind == 1 && !parents.is_empty()).then(|| parents[n % parents.len()].as_str());
        let raw = new_message(run, n, kind, date, parent);
        let started = Instant::now();
        let prepared = ingest::prepare(
            &blobs,
            vec![RawMessage {
                folder: inbox_id,
                flags: Flags::NONE,
                raw,
                ..RawMessage::default()
            }],
            1,
        )?;
        let submitted = Instant::now();
        let ingested = ingest::submit(&store, prepared).recv_blocking()?;
        if ingested.ids.iter().all(Option::is_none) {
            bail!("message {n} was already in the store");
        }
        let (event, at) = wait_for(&events, ingested.version)?;
        writer.push(ingested.header_time);
        to_event.push(at - submitted);
        whole.push(at - started);
        inserted.extend(ingested.ids.iter().flatten());
        splices += event
            .views
            .iter()
            .filter(|change| matches!(change.diff, ViewDiff::Splice { .. }))
            .count();
    }
    println!(
        "- writer transaction (insert, thread, counts, positions in 3 open views, commit): {}",
        summary(&writer)
    );
    let mut slowest: Vec<(Duration, usize)> = writer.iter().copied().zip(0..).collect();
    slowest.sort_by(|a, b| b.cmp(a));
    let kinds = ["new, top", "reply", "old, deep", "HTML, top"];
    println!(
        "  slowest: {}",
        slowest
            .iter()
            .take(3)
            .map(|(time, n)| format!("#{n} ({}) {:.2} ms", kinds[n % 4], ms(*time)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for (kind, name) in kinds.iter().enumerate() {
        let of_kind: Vec<Duration> = writer.iter().copied().skip(kind).step_by(4).collect();
        println!("  {name}: {}", summary(&of_kind));
    }
    println!("- submit → change event received: {}", summary(&to_event));
    println!(
        "- raw bytes → change event (blob fsyncs and parsing included): {}",
        summary(&whole)
    );
    println!("- {splices} view splices in {} events", options.inserts);

    let mut flag_events = Vec::new();
    let mut last = store.accounts().recv_blocking()?.version;
    for n in 0..options.inserts.min(200) {
        let row = &sample[(n / 2) % sample.len()];
        let flip = n % 2 == 0;
        let mark_read = flip != row.flags.is_seen();
        let (add, remove) = if mark_read {
            (Flags::SEEN, Flags::NONE)
        } else {
            (Flags::NONE, Flags::SEEN)
        };
        let started = Instant::now();
        let version = store.set_flags(vec![row.id], add, remove).recv_blocking()?;
        if version == last {
            continue;
        }
        last = version;
        let (_, at) = wait_for(&events, version)?;
        flag_events.push(at - started);
    }
    println!(
        "- mark read or unread → change event received: {}",
        summary(&flag_events)
    );

    let removed = store.remove(inserted.clone()).recv_blocking()?;
    for key in &removed.orphaned_blobs {
        blobs.remove(key)?;
    }
    drop(handles);
    let after: u32 = store
        .folders()
        .recv_blocking()?
        .value
        .iter()
        .map(|folder| folder.total)
        .sum();
    println!(
        "- cleanup: {} inserted messages removed, {} blobs deleted; {after} messages remain",
        removed.removed.len(),
        removed.orphaned_blobs.len()
    );
    let problems = store.check().recv_blocking()?;
    println!(
        "- consistency check: {}",
        if problems.is_empty() {
            "ok".to_owned()
        } else {
            problems.join("; ")
        }
    );
    store.close()?;
    Ok(())
}
