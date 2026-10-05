//! All Inboxes over several accounts (ML-047): opening the unified inbox, its first and deep
//! pages under each filter chip, and one new message's diff with every unified view open,
//! against S3's bars (folder open ≤ 100 ms; one-insert diff ≤ 20 ms). Cold runs evict the
//! store from the page cache as `bench` does.

use crate::bench::{evict, listen, open, section_starts, wait_for};
use crate::import::store_files;
use crate::stats::{ms, summary};
use anyhow::{Result, bail};
use sealwax_domain::model::{Flags, FolderId, FolderRole, MessageId};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::ingest::{self, RawMessage};
use sealwax_infrastructure::store::{OpenView, Store, ViewDiff, ViewFilter, ViewSpec};
use std::time::{Duration, Instant};

pub use crate::bench::Options;

const FILTERS: [ViewFilter; 4] = [
    ViewFilter::All,
    ViewFilter::Unread,
    ViewFilter::Starred,
    ViewFilter::Attachments,
];

fn total(store: &Store, spec: ViewSpec) -> Result<u32> {
    Ok(store
        .open_view(OpenView {
            spec,
            first_page: 0,
            section_starts: Vec::new(),
        })
        .recv_blocking()?
        .total)
}

/// Offsets of the pages measured: the first, 10,000 rows down, the middle (the deepest a page
/// walks) and the last.
fn offsets(total: u32) -> Vec<(&'static str, u32)> {
    let mut out = vec![("first", 0)];
    if total > 10_100 {
        out.push(("10,000", 10_000));
    }
    if total > 200 {
        out.push(("middle", total / 2 - 50));
        out.push(("last", total - 100));
    }
    out
}

/// A new message for the insert test, as a server would deliver it: `kind` 0 is new at the top
/// and unread, 1 a read reply into an existing conversation, 2 an old unread message with an
/// attachment (a deep position), 3 new, starred and with an attachment.
fn raw(run: u64, n: usize, kind: usize, date: i64, parent: Option<&str>) -> Vec<u8> {
    let threading = match parent {
        Some(parent) => format!("In-Reply-To: <{parent}>\r\nReferences: <{parent}>\r\n"),
        None => String::new(),
    };
    let text = format!("Hei,\r\n\r\nmåling nummer {n}.\r\n");
    let body = if kind >= 2 {
        format!(
            "Content-Type: multipart/mixed; boundary=\"b{n}\"\r\n\r\n--b{n}\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\r\n{text}--b{n}\r\n\
             Content-Type: text/csv; name=\"tall-{n}.csv\"\r\n\
             Content-Disposition: attachment; filename=\"tall-{n}.csv\"\r\n\r\n\
             a;b\r\n1;2\r\n--b{n}--\r\n"
        )
    } else {
        format!("Content-Type: text/plain; charset=utf-8\r\n\r\n{text}")
    };
    format!(
        "From: Måler {n} <maaler{n}@example.test>\r\nTo: alex.berg@example.test\r\n\
         Subject: Ny melding {n}\r\nDate: {}\r\n\
         Message-ID: <unified-{run}-{n}-{date}@example.test>\r\n{threading}MIME-Version: 1.0\r\n\
         {body}",
        crate::mime::date(date, 0)
    )
    .into_bytes()
}

pub fn run(options: &Options) -> Result<()> {
    let blobs = BlobStore::open(&options.blobs)?;
    let mut store = open(options, &blobs)?;
    let accounts = store.accounts().recv_blocking()?.value;
    let folders = store.folders().recv_blocking()?.value;
    let inboxes: Vec<FolderId> = folders
        .iter()
        .filter(|folder| folder.role == FolderRole::Inbox)
        .map(|folder| folder.id)
        .collect();
    if inboxes.len() < 2 {
        bail!("needs several accounts with an INBOX; import with --account");
    }
    let messages: u32 = folders.iter().map(|folder| folder.total).sum();
    let bytes: u64 = store_files(&options.store)
        .iter()
        .filter_map(|file| std::fs::metadata(file).ok())
        .map(|meta| meta.len())
        .sum();
    println!("## Store");
    println!("- SQLite {}", rusqlite::version());
    println!(
        "- {} accounts, {messages} messages in {} folders; store files {:.1} MiB",
        accounts.len(),
        folders.len(),
        bytes as f64 / 1_048_576.0
    );
    println!(
        "- inboxes: {}",
        folders
            .iter()
            .filter(|folder| folder.role == FolderRole::Inbox)
            .map(|folder| format!("{} ({} unread)", folder.total, folder.unread))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let specs: Vec<ViewSpec> = FILTERS
        .iter()
        .map(|filter| ViewSpec::unified_inbox().with_filter(*filter))
        .collect();
    let mut totals = Vec::new();
    for spec in &specs {
        totals.push(total(&store, *spec)?);
    }
    println!(
        "- All Inboxes: {}",
        FILTERS
            .iter()
            .zip(&totals)
            .map(|(filter, total)| format!("{filter:?} {total}"))
            .collect::<Vec<_>>()
            .join(", ")
    );

    println!("\n## Opening All Inboxes (count + 4 section counts + first 100 rows, one snapshot)");
    for (spec, rows) in specs.iter().zip(&totals) {
        let mut cold = Vec::new();
        for _ in 0..options.cold {
            store.close()?;
            evict(&options.store)?;
            store = open(options, &blobs)?;
            cold.push(open_view(&store, *spec, options.now)?);
        }
        let mut warm = Vec::new();
        for _ in 0..options.warm {
            warm.push(open_view(&store, *spec, options.now)?);
        }
        println!(
            "- {:?} ({rows} rows): cold {} | warm {}",
            spec.filter,
            summary(&cold),
            summary(&warm)
        );
    }

    println!("\n## Pages of 100 rows");
    println!(
        "Cold: the store reopened with its files evicted, then the page alone. First: the first \
         read at that offset in this process, warm cache otherwise. Warm: {} more reads.",
        options.warm
    );
    for (spec, rows) in specs.iter().zip(&totals) {
        for (name, offset) in offsets(*rows) {
            let mut cold = Vec::new();
            for _ in 0..options.cold {
                store.close()?;
                evict(&options.store)?;
                store = open(options, &blobs)?;
                cold.push(page(&store, *spec, offset)?);
            }
            let first = page(&store, *spec, offset)?;
            let mut warm = Vec::new();
            for _ in 0..options.warm {
                warm.push(page(&store, *spec, offset)?);
            }
            println!(
                "- {:?} {name} (offset {offset}): cold {} | first {:.2} ms | warm {}",
                spec.filter,
                summary(&cold),
                ms(first),
                summary(&warm)
            );
        }
    }

    println!("\n## One new message with its diff");
    let events = listen(&store);
    let mut open_views = specs.clone();
    open_views.extend(inboxes.iter().map(|inbox| ViewSpec::folder(*inbox)));
    let mut handles = Vec::new();
    for spec in &open_views {
        let opened = store
            .open_view(OpenView {
                spec: *spec,
                first_page: 100,
                section_starts: Vec::new(),
            })
            .recv_blocking()?;
        handles.push(opened.view);
    }
    println!(
        "- open views: the four unified ones and each INBOX ({} views)",
        handles.len()
    );
    let mut parents: Vec<Vec<String>> = Vec::new();
    for inbox in &inboxes {
        let count = total(&store, ViewSpec::folder(*inbox))?;
        let sample = store
            .page(ViewSpec::folder(*inbox), count / 3, 20)
            .recv_blocking()?
            .rows;
        let mut ids = Vec::new();
        for row in sample {
            if let Some(detail) = store.message(row.id).recv_blocking()?.value
                && let Some(id) = detail.headers.message_id
            {
                ids.push(id);
            }
        }
        parents.push(ids);
    }
    let run = store.accounts().recv_blocking()?.version.0;
    let mut writer = Vec::new();
    let mut to_event = Vec::new();
    let mut whole = Vec::new();
    let mut inserted: Vec<MessageId> = Vec::new();
    let mut splices = 0;
    let mut reloads = 0;
    let mut state = 0x5eed_u64;
    for n in 0..options.inserts {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        let kind = n % 4;
        let account = (n / 4) % inboxes.len();
        let date = match kind {
            2 => options.now - 86_400 * (30 + (state >> 33) as i64 % 1_500),
            _ => options.now + n as i64,
        };
        let parent = (kind == 1)
            .then(|| parents[account].get(n % parents[account].len().max(1)))
            .flatten()
            .map(String::as_str);
        let flags = match kind {
            1 => Flags::SEEN,
            3 => Flags::SEEN | Flags::FLAGGED,
            _ => Flags::NONE,
        };
        let started = Instant::now();
        let prepared = ingest::prepare(
            &blobs,
            vec![RawMessage {
                folder: inboxes[account],
                flags,
                raw: raw(run, n, kind, date, parent),
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
        for change in &event.views {
            match change.diff {
                ViewDiff::Splice { .. } => splices += 1,
                ViewDiff::Reload { .. } => reloads += 1,
            }
        }
    }
    println!(
        "- writer transaction (insert, thread, counts, positions in the open views, commit): {}",
        summary(&writer)
    );
    let kinds = [
        "new, top, unread",
        "reply, read",
        "old, deep, unread, attachment",
        "new, top, starred, attachment",
    ];
    for (kind, name) in kinds.iter().enumerate() {
        let of_kind: Vec<Duration> = writer.iter().copied().skip(kind).step_by(4).collect();
        println!("  {name}: {}", summary(&of_kind));
    }
    println!("- submit → change event received: {}", summary(&to_event));
    println!(
        "- raw bytes → change event (blob fsyncs and parsing included): {}",
        summary(&whole)
    );
    println!(
        "- {splices} view splices and {reloads} reloads in {} events",
        options.inserts
    );

    let removed = store.remove(inserted).recv_blocking()?;
    for key in &removed.orphaned_blobs {
        blobs.remove(key)?;
    }
    drop(handles);
    println!(
        "- cleanup: {} inserted messages removed, {} blobs deleted",
        removed.removed.len(),
        removed.orphaned_blobs.len()
    );
    let started = Instant::now();
    let problems = store.check().recv_blocking()?;
    println!(
        "- consistency check ({:.0} ms): {}",
        ms(started.elapsed()),
        if problems.is_empty() {
            "ok".to_owned()
        } else {
            problems.join("; ")
        }
    );
    store.close()?;
    Ok(())
}

fn open_view(store: &Store, spec: ViewSpec, now: i64) -> Result<Duration> {
    let started = Instant::now();
    store
        .open_view(OpenView {
            spec,
            first_page: 100,
            section_starts: section_starts(now),
        })
        .recv_blocking()?;
    Ok(started.elapsed())
}

fn page(store: &Store, spec: ViewSpec, offset: u32) -> Result<Duration> {
    let started = Instant::now();
    let page = store.page(spec, offset, 100).recv_blocking()?;
    let elapsed = started.elapsed();
    if page.rows.len() != 100 {
        bail!(
            "{:?} at {offset}: {} rows, not 100",
            spec.filter,
            page.rows.len()
        );
    }
    Ok(elapsed)
}
