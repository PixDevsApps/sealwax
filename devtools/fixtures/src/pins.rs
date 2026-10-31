//! Headless measurement of the Inbox with pinned senders (issue #26, ADR-024) at fixture scale:
//! folder open, pages from the top, the middle and the end, and a flag change's diff, with no
//! pin, with one sender pinned until read, with the busiest sender kept pinned, and with the
//! whole fixture domain kept pinned (every message a candidate: the worst case). It pins and
//! unpins, so run it on a copy of a store; it leaves no pin behind.

use crate::bench::{Options, evict, listen, open, section_starts, wait_for};
use crate::stats::summary;
use anyhow::{Context, Result};
use sealwax_domain::model::{Flags, FolderRole, MessageId};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{
    OpenView, Pin, PinKind, PinMode, PinTarget, Store, ViewDiff, ViewSpec,
};
use std::time::{Duration, Instant};

const OFFSETS: [u32; 8] = [0, 100, 1_000, 10_000, 49_900, 50_100, 90_000, 99_900];

/// How long `read` takes.
fn timed<T>(read: impl FnOnce() -> Result<T>) -> Result<(Duration, T)> {
    let started = Instant::now();
    let value = read()?;
    Ok((started.elapsed(), value))
}

fn open_view(store: &Store, spec: ViewSpec, now: i64) -> Result<(Duration, u32)> {
    timed(|| {
        Ok(store
            .open_view(OpenView {
                spec,
                first_page: 100,
                section_starts: section_starts(now),
            })
            .recv_blocking()?
            .pinned)
    })
}

/// The newest message of the view whose sender is pinned (`true`) or not.
fn row_where(store: &Store, spec: ViewSpec, pinned: bool) -> Result<Option<MessageId>> {
    for offset in (0..20_000).step_by(1_000) {
        let page = store.page(spec, offset, 1_000).recv_blocking()?;
        if page.rows.is_empty() {
            break;
        }
        if let Some(row) = page.rows.iter().find(|row| row.pinned == pinned) {
            return Ok(Some(row.id));
        }
    }
    Ok(None)
}

/// A flag change's commit to its change event, and whether the view got a splice or a reload.
fn flag_change(
    store: &Store,
    events: &std::sync::mpsc::Receiver<(
        std::sync::Arc<sealwax_infrastructure::store::ChangeEvent>,
        Instant,
    )>,
    id: MessageId,
    runs: usize,
) -> Result<(Vec<Duration>, &'static str)> {
    let mut samples = Vec::new();
    let mut kind = "none";
    for run in 0..runs {
        let (add, remove) = if run % 2 == 0 {
            (Flags::FLAGGED, Flags::NONE)
        } else {
            (Flags::NONE, Flags::FLAGGED)
        };
        let started = Instant::now();
        let version = store.set_flags(vec![id], add, remove).recv_blocking()?;
        let (event, at) = wait_for(events, version)?;
        samples.push(at - started);
        if let Some(change) = event.views.first() {
            kind = match change.diff {
                ViewDiff::Splice { .. } => "splice",
                ViewDiff::Reload { .. } => "reload",
            };
        }
    }
    Ok((samples, kind))
}

fn measure(options: &Options, store: &Store, spec: ViewSpec, label: &str) -> Result<()> {
    let (_, pinned) = open_view(store, spec, options.now)?;
    println!("## {label}: {pinned} rows in the Pinned section");
    let mut warm = Vec::new();
    for _ in 0..options.warm {
        warm.push(open_view(store, spec, options.now)?.0);
    }
    println!("- open, warm: {}", summary(&warm));
    for offset in OFFSETS {
        let mut samples = Vec::new();
        for _ in 0..options.warm {
            let (took, page) = timed(|| Ok(store.page(spec, offset, 100).recv_blocking()?))?;
            anyhow::ensure!(!page.rows.is_empty(), "no rows at {offset}");
            samples.push(took);
        }
        println!("- page at {offset}, warm: {}", summary(&samples));
    }
    let events = listen(store);
    let _view = store
        .open_view(OpenView {
            spec,
            first_page: 0,
            section_starts: Vec::new(),
        })
        .recv_blocking()?
        .view;
    for pinned in [false, true] {
        let Some(id) = row_where(store, spec, pinned)? else {
            continue;
        };
        let (samples, kind) = flag_change(store, &events, id, options.warm)?;
        let whose = if pinned { "a pinned" } else { "another" };
        println!(
            "- star {whose} sender's message → event ({kind}): {}",
            summary(&samples)
        );
    }
    Ok(())
}

fn cold_open(options: &Options, blobs: &BlobStore, spec: ViewSpec) -> Result<Vec<Duration>> {
    let mut samples = Vec::new();
    for _ in 0..options.cold {
        evict(&options.store)?;
        let store = open(options, blobs)?;
        samples.push(open_view(&store, spec, options.now)?.0);
        store.close()?;
    }
    Ok(samples)
}

fn keep(store: &Store, pin: Pin) -> Result<()> {
    store
        .restore_pins(vec![Pin {
            mode: PinMode::Keep,
            ..pin
        }])
        .recv_blocking()?;
    Ok(())
}

fn clear(store: &Store) -> Result<()> {
    for pin in store.pins().recv_blocking()? {
        store.remove_pin(pin.kind, pin.key).recv_blocking()?;
    }
    Ok(())
}

pub fn run(options: &Options) -> Result<()> {
    let blobs = BlobStore::open(&options.blobs)?;
    let store = open(options, &blobs)?;
    let folders = store.folders().recv_blocking()?.value;
    let inbox = folders
        .iter()
        .filter(|folder| folder.role == FolderRole::Inbox)
        .max_by_key(|folder| folder.total)
        .context("no INBOX; run `import` first")?;
    let spec = ViewSpec::folder(inbox.id);
    println!("# Pinned senders, INBOX of {} messages", inbox.total);
    clear(&store)?;
    measure(options, &store, spec, "no pin")?;
    store.close()?;
    println!(
        "- open, cold, no pin: {}",
        summary(&cold_open(options, &blobs, spec)?)
    );

    let store = open(options, &blobs)?;
    let pin = |key: &str, kind| -> Result<Pin> {
        store
            .pin(PinTarget::Key(key.into()), kind, options.now)
            .recv_blocking()?
            .context("nothing to pin")
    };
    pin("techdigest@example.test", PinKind::Sender)?;
    measure(options, &store, spec, "a newsletter pinned until read")?;
    keep(&store, pin("alex.berg@example.test", PinKind::Sender)?)?;
    measure(
        options,
        &store,
        spec,
        "and the busiest sender (about 21,000 messages) kept",
    )?;
    store.close()?;
    println!(
        "- open, cold, those two pins: {}",
        summary(&cold_open(options, &blobs, spec)?)
    );

    let store = open(options, &blobs)?;
    clear(&store)?;
    keep(
        &store,
        store
            .pin(
                PinTarget::Key("example.test".into()),
                PinKind::Domain,
                options.now,
            )
            .recv_blocking()?
            .context("nothing to pin")?,
    )?;
    measure(
        options,
        &store,
        spec,
        "the fixture's whole domain kept (every message a candidate)",
    )?;
    clear(&store)?;
    store.close()?;
    Ok(())
}
