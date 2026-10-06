//! Headless measurements of the op queue (ML-051, ML-053) on a fixture store: a triage action
//! to its change event and Undo to its change event, single messages at several depths and
//! batches; folder open, pages and counts with pending ops hidden and overlaid; the runner's
//! reads and records; marking the whole INBOX read. The fixture account is made a server
//! account first (it gets a key, so ops wait for a runner instead of applying at once), so run
//! it on a copy of the store. The first open of an older store migrates it, and is timed.

use crate::bench::{evict, listen, section_starts, wait_for};
use crate::import::{ACCOUNT_NAME, account_address};
use crate::stats::{ms, summary};
use anyhow::{Context, Result, bail};
use sealwax_domain::model::{FolderId, FolderRole, MessageId};
use sealwax_domain::triage::Triage;
use sealwax_infrastructure::store::{
    BatchId, FirstSyncStep, FolderSpec, OpAction, OpenView, OpsDone, QueueTriage, Store,
    StoreOptions, ViewFilter, ViewSpec,
};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct Options {
    pub store: PathBuf,
    pub cold: usize,
    pub warm: usize,
    /// Samples per single-message measurement.
    pub samples: usize,
    /// Messages hidden by pending moves, and as many with pending flag changes, for the reads
    /// with overlays present.
    pub overlays: usize,
    pub now: i64,
}

/// Unix ms; the undo window ends long after the run.
const LATER: i64 = 4_000_000_000_000;

fn open(options: &Options) -> Result<Store> {
    Ok(Store::open(&options.store, StoreOptions::default())?)
}

fn views(inbox: FolderId) -> [ViewSpec; 5] {
    [
        ViewSpec::folder(inbox),
        ViewSpec::folder(inbox).with_filter(ViewFilter::Unread),
        ViewSpec::folder(inbox).with_filter(ViewFilter::Starred),
        ViewSpec::folder(inbox).with_filter(ViewFilter::Attachments),
        ViewSpec::unified_inbox(),
    ]
}

/// Folder opens (count, section counts, first 100 rows), pages and the folder list. Cold opens
/// close the store, evict its files and open it again.
fn reads(mut store: Store, options: &Options, inbox: FolderId, label: &str) -> Result<Store> {
    println!("\n### Reads, {label}");
    for spec in views(inbox) {
        let mut cold = Vec::new();
        let mut rows = 0;
        for _ in 0..options.cold {
            store.close()?;
            evict(&options.store)?;
            store = open(options)?;
            let started = Instant::now();
            let opened = store
                .open_view(OpenView {
                    spec,
                    first_page: 100,
                    section_starts: section_starts(options.now),
                })
                .recv_blocking()?;
            cold.push(started.elapsed());
            rows = opened.total;
        }
        let mut warm = Vec::new();
        for _ in 0..options.warm {
            let started = Instant::now();
            let opened = store
                .open_view(OpenView {
                    spec,
                    first_page: 100,
                    section_starts: section_starts(options.now),
                })
                .recv_blocking()?;
            warm.push(started.elapsed());
            rows = opened.total;
        }
        println!(
            "- open {spec:?} ({rows} rows): cold {} | warm {}",
            summary(&cold),
            summary(&warm)
        );
    }
    let total = store
        .open_view(OpenView {
            spec: ViewSpec::folder(inbox),
            first_page: 0,
            section_starts: Vec::new(),
        })
        .recv_blocking()?
        .total;
    for fraction in [0u32, 25, 50, 75, 100] {
        let offset = (u64::from(total.saturating_sub(100)) * u64::from(fraction) / 100) as u32;
        let mut samples = Vec::new();
        for _ in 0..options.warm {
            let started = Instant::now();
            store
                .page(ViewSpec::folder(inbox), offset, 100)
                .recv_blocking()?;
            samples.push(started.elapsed());
        }
        println!("- INBOX page at {offset}: warm {}", summary(&samples));
    }
    let mut lists = Vec::new();
    for _ in 0..options.warm {
        let started = Instant::now();
        store.folders().recv_blocking()?;
        lists.push(started.elapsed());
    }
    println!("- the folder list with counts: warm {}", summary(&lists));
    Ok(store)
}

pub fn run(options: &Options) -> Result<()> {
    // Make the fixture account a server account: ops then wait for a runner.
    let conn = rusqlite::Connection::open(&options.store)?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let keyed = conn.execute(
        "UPDATE accounts SET key = 'bench-ops' WHERE address = ?1 AND key IS NULL",
        [account_address(ACCOUNT_NAME)],
    )?;
    drop(conn);
    let started = Instant::now();
    let mut store = open(options)?;
    let opened = started.elapsed();
    println!("## Op queue");
    println!("- SQLite {}", rusqlite::version());
    println!(
        "- store open from schema v{version}: {:.1} ms{}",
        ms(opened),
        if version < 7 {
            " (migration to v7 included)"
        } else {
            ""
        }
    );
    if keyed == 0 {
        println!("- the fixture account already had a key");
    }
    let accounts = store.accounts().recv_blocking()?.value;
    let account = accounts
        .iter()
        .find(|account| account.address == account_address(ACCOUNT_NAME))
        .context("no fixture account; run `import` first")?
        .id;
    let folders = store.folders().recv_blocking()?.value;
    let inbox = folders
        .iter()
        .find(|folder| folder.account == account && folder.role == FolderRole::Inbox)
        .context("no fixture INBOX")?;
    let (inbox, total, unread) = (inbox.id, inbox.total, inbox.unread);
    println!("- INBOX {total} messages, {unread} unread");
    let archive = match folders
        .iter()
        .find(|folder| folder.account == account && folder.role == FolderRole::Archive)
    {
        Some(folder) => folder.id,
        None => {
            store
                .upsert_folder(FolderSpec::local(account, "Archive", FolderRole::Archive))
                .recv_blocking()?
                .value
        }
    };
    if store
        .folder_sync(archive)
        .recv_blocking()?
        .is_some_and(|state| state.uidvalidity.is_none())
    {
        store
            .first_sync_step(
                archive,
                FirstSyncStep::Start {
                    uidvalidity: 1,
                    top: 1_000_000,
                    modseq: 0,
                },
            )
            .recv_blocking()?;
        store
            .first_sync_step(archive, FirstSyncStep::Finish { below: 0 })
            .recv_blocking()?;
    }

    store = reads(store, options, inbox, "no ops pending")?;

    let events = listen(&store);
    let mut handles = Vec::new();
    for spec in [
        ViewSpec::folder(inbox),
        ViewSpec::folder(inbox).with_filter(ViewFilter::Unread),
        ViewSpec::unified_inbox(),
    ] {
        handles.push(
            store
                .open_view(OpenView {
                    spec,
                    first_page: 100,
                    section_starts: Vec::new(),
                })
                .recv_blocking()?
                .view,
        );
    }
    // Messages spread over the folder, newest to oldest.
    let mut sample = Vec::new();
    let step = (total / options.samples.max(1) as u32).max(1);
    let mut offset = 0;
    while sample.len() < options.samples && offset < total {
        let page = store
            .page(ViewSpec::folder(inbox), offset, 1)
            .recv_blocking()?;
        sample.extend(page.rows);
        offset += step;
    }

    println!("\n### Triage → change event (INBOX, INBOX unread and All Inboxes open)");
    let measure = |store: &Store,
                   action: Triage,
                   ids: Vec<MessageId>,
                   not_before: i64|
     -> Result<(Duration, Duration, Option<BatchId>)> {
        let started = Instant::now();
        let queued = store
            .queue_triage(QueueTriage {
                action,
                messages: ids,
                not_before,
            })
            .recv_blocking()?;
        let replied = started.elapsed();
        let batch = queued.batch;
        if batch.is_none() {
            return Ok((replied, replied, None));
        }
        let (_, at) = wait_for(&events, queued.version)?;
        Ok((replied, at - started, batch))
    };
    let undo = |store: &Store, batch: BatchId| -> Result<Duration> {
        let started = Instant::now();
        let undone = store.undo(batch).recv_blocking()?;
        let (_, at) = wait_for(&events, undone.version)?;
        Ok(at - started)
    };
    for (name, action, window) in [
        ("mark read or unread", None, 0),
        ("star", Some(Triage::Star(true)), 0),
        ("archive (in the window)", Some(Triage::Archive), LATER),
        (
            "delete to the Trash (in the window)",
            Some(Triage::Delete),
            LATER,
        ),
    ] {
        let mut queued = Vec::new();
        let mut undone = Vec::new();
        for row in &sample {
            let action = action.unwrap_or(Triage::Read(!row.flags.is_seen()));
            let (_, to_event, batch) = measure(&store, action, vec![row.id], window)?;
            let Some(batch) = batch else {
                continue;
            };
            queued.push(to_event);
            undone.push(undo(&store, batch)?);
        }
        println!("- {name}, one message → event: {}", summary(&queued));
        println!("  Undo → event: {}", summary(&undone));
    }
    let mut queued = Vec::new();
    let mut undone = Vec::new();
    for batch in 0..20u32 {
        let rows = store
            .page(ViewSpec::folder(inbox), batch * 50, 50)
            .recv_blocking()?
            .rows;
        let ids: Vec<MessageId> = rows.iter().map(|row| row.id).collect();
        let (_, to_event, batch) = measure(&store, Triage::Archive, ids, LATER)?;
        queued.push(to_event);
        undone.push(undo(&store, batch.context("archived nothing")?)?);
    }
    println!(
        "- archive 50 adjacent messages among the newest 1,000 → event: {}",
        summary(&queued)
    );
    println!("  Undo → event: {}", summary(&undone));
    let mut queued = Vec::new();
    let mut undone = Vec::new();
    for chunk in sample.chunks(50) {
        let ids: Vec<MessageId> = chunk.iter().map(|row| row.id).collect();
        let (_, to_event, batch) = measure(&store, Triage::Archive, ids, LATER)?;
        queued.push(to_event);
        undone.push(undo(&store, batch.context("archived nothing")?)?);
    }
    println!(
        "- archive 50 messages spread over the whole folder → event: {}",
        summary(&queued)
    );
    println!("  Undo → event: {}", summary(&undone));

    // Pending overlays: messages hidden by pending moves, and as many marked read.
    let mut hidden = Vec::new();
    let mut flagged = Vec::new();
    let every = (total / (2 * options.overlays.max(1) as u32)).max(1);
    let mut offset = 0;
    while hidden.len() < options.overlays && offset < total {
        let page = store
            .page(ViewSpec::folder(inbox), offset, 2)
            .recv_blocking()?;
        if let [a, b] = page.rows.as_slice() {
            hidden.push(a.id);
            flagged.push(b.id);
        }
        offset += every;
    }
    let started = Instant::now();
    let archived = store
        .queue_triage(QueueTriage {
            action: Triage::Archive,
            messages: hidden.clone(),
            not_before: LATER,
        })
        .recv_blocking()?;
    let archive_many = started.elapsed();
    let started = Instant::now();
    let starred = store
        .queue_triage(QueueTriage {
            action: Triage::Star(true),
            messages: flagged.clone(),
            not_before: LATER,
        })
        .recv_blocking()?;
    let star_many = started.elapsed();
    println!(
        "\n- {} messages archived in one action: {:.1} ms; {} starred: {:.1} ms",
        hidden.len(),
        ms(archive_many),
        flagged.len(),
        ms(star_many)
    );
    let pending = store.pending_ops(account).recv_blocking()?;
    println!("- pending ops now {pending}");
    drop(handles);
    drop(events);
    store = reads(
        store,
        options,
        inbox,
        &format!("{} hidden and {} overlaid", hidden.len(), flagged.len()),
    )?;

    println!("\n### The runner's side, with those ops pending");
    let mut due_times = Vec::new();
    for _ in 0..options.warm {
        let started = Instant::now();
        let due = store.due_ops(account, LATER).recv_blocking()?;
        due_times.push(started.elapsed());
        if due.groups.is_empty() {
            bail!("nothing due");
        }
    }
    println!(
        "- due ops ({} pending, all due): {}",
        pending,
        summary(&due_times)
    );
    let due = store.due_ops(account, LATER).recv_blocking()?;
    let stars = due
        .groups
        .iter()
        .find(|group| matches!(group.action, OpAction::Flags { .. }))
        .context("no star group")?;
    let mut records = Vec::new();
    for chunk in stars.messages.chunks(50).take(20) {
        let ops: Vec<_> = chunk
            .iter()
            .flat_map(|message| message.ops.clone())
            .collect();
        store.start_ops(ops.clone(), None).recv_blocking()?;
        let started = Instant::now();
        let recorded = store
            .ops_done(OpsDone {
                ops,
                ..OpsDone::default()
            })
            .recv_blocking()?;
        records.push(started.elapsed());
        let _ = recorded;
    }
    println!(
        "- 50 flag changes recorded done (the overlay becomes the server's state): {}",
        summary(&records)
    );
    let started = Instant::now();
    let undone = store
        .undo(archived.batch.context("archived nothing")?)
        .recv_blocking()?;
    println!(
        "- Undo of the {}-message archive: {:.1} ms ({} restored)",
        hidden.len(),
        ms(started.elapsed()),
        undone.restored
    );
    let started = Instant::now();
    let undone = store
        .undo(starred.batch.context("starred nothing")?)
        .recv_blocking()?;
    println!(
        "- Undo of the star: {:.1} ms ({} restored, {} reverted)",
        ms(started.elapsed()),
        undone.restored,
        undone.reverted
    );
    let mut drained = 0;
    loop {
        let due = store.due_ops(account, LATER).recv_blocking()?;
        if due.groups.is_empty() {
            break;
        }
        for group in due.groups {
            store.start_ops(group.ops.clone(), None).recv_blocking()?;
            store
                .ops_done(OpsDone {
                    ops: group.ops,
                    ..OpsDone::default()
                })
                .recv_blocking()?;
            drained += 1;
        }
    }
    println!("- {drained} groups drained");

    let unread = store
        .folders()
        .recv_blocking()?
        .value
        .iter()
        .find(|folder| folder.id == inbox)
        .map_or(0, |folder| folder.unread);
    let events = listen(&store);
    // The first time the unread rows may not be in the page cache, the second they are.
    for pass in ["first", "again"] {
        let started = Instant::now();
        let read = store.mark_folder_read(inbox, LATER).recv_blocking()?;
        let replied = started.elapsed();
        let (_, at) = wait_for(&events, read.version)?;
        println!(
            "- mark INBOX read ({unread} unread), {pass}: reply {:.1} ms, event {:.1} ms",
            ms(replied),
            ms(at - started)
        );
        let started = Instant::now();
        let undone = store
            .undo(read.batch.context("nothing marked read")?)
            .recv_blocking()?;
        println!(
            "  Undo: {:.1} ms ({} restored)",
            ms(started.elapsed()),
            undone.restored
        );
    }
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
