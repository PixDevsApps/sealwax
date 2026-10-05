//! `remove-account`: an account's removal at scale (ML-038), as the engine runs it. The store
//! deletes the account in one transaction while the views the app would have open (the
//! account's Inbox and All Inboxes) are registered and another thread keeps reading pages, then
//! the orphaned raw messages are deleted from the blob store. Ends with the recount check and
//! SQLite's and FTS5's integrity checks.

use crate::import::{account_address, store_files};
use crate::stats::{ms, summary};
use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use sealwax_domain::model::FolderRole;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{OpenView, Store, StoreOptions, ViewSpec};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub struct Options {
    pub store: PathBuf,
    pub blobs: PathBuf,
    pub account: String,
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

fn store_bytes(options: &Options) -> u64 {
    store_files(&options.store)
        .iter()
        .filter_map(|path| std::fs::metadata(path).ok())
        .map(|metadata| metadata.len())
        .sum()
}

pub fn run(options: &Options) -> Result<()> {
    let blobs = BlobStore::open(&options.blobs)?;
    let store = Store::open(
        &options.store,
        StoreOptions {
            blobs: Some(blobs.clone()),
            ..StoreOptions::default()
        },
    )?;
    let address = account_address(&options.account);
    let accounts = store.accounts().recv_blocking()?.value;
    let account = accounts
        .iter()
        .find(|account| account.address == address)
        .with_context(|| format!("no fixture account named {}", options.account))?
        .id;
    let others: Vec<_> = accounts.iter().filter(|a| a.id != account).collect();
    let folders = store.folders().recv_blocking()?.value;
    let inbox = folders
        .iter()
        .find(|folder| folder.account == account && folder.role == FolderRole::Inbox)
        .map(|folder| folder.id);
    let other_inbox = folders
        .iter()
        .find(|folder| folder.account != account && folder.role == FolderRole::Inbox)
        .map(|folder| folder.id);
    let open = |spec| {
        store.open_view(OpenView {
            spec,
            first_page: 100,
            section_starts: Vec::new(),
        })
    };
    let mut views = vec![open(ViewSpec::unified_inbox()).recv_blocking()?];
    if let Some(inbox) = inbox {
        views.push(open(ViewSpec::folder(inbox)).recv_blocking()?);
    }
    let blobs_before = blobs.disk_usage()?;
    let bytes_before = store_bytes(options);

    let done = AtomicBool::new(false);
    let (removed, store_time, reads) = std::thread::scope(|scope| -> Result<_> {
        let reader = scope.spawn(|| {
            let spec = ViewSpec::folder(other_inbox.or(inbox).unwrap_or_default());
            let mut samples = Vec::new();
            while !done.load(Ordering::Relaxed) {
                let started = Instant::now();
                if store.page(spec, 0, 100).recv_blocking().is_err() {
                    break;
                }
                samples.push(started.elapsed());
                std::thread::sleep(Duration::from_millis(5));
            }
            samples
        });
        std::thread::sleep(Duration::from_millis(50));
        let started = Instant::now();
        let removed = store.remove_account(account).recv_blocking();
        let store_time = started.elapsed();
        done.store(true, Ordering::Relaxed);
        let reads = reader.join().expect("the reader thread panicked");
        Ok((removed?, store_time, reads))
    })?;
    let started = Instant::now();
    let failed = removed
        .orphaned_blobs
        .iter()
        .filter(|key| blobs.remove(key).is_err())
        .count();
    let blob_time = started.elapsed();
    drop(views);

    let problems = store.check().recv_blocking()?;
    let left = store.accounts().recv_blocking()?.value;
    store.close()?;
    let conn = Connection::open(&options.store)?;
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    conn.execute_batch("INSERT INTO message_fts(message_fts) VALUES ('integrity-check')")?;
    let remaining: i64 = conn.query_row("SELECT count(*) FROM messages", [], |row| row.get(0))?;
    drop(conn);
    let blobs_after = blobs.disk_usage()?;

    println!("## Remove account {}", options.account);
    println!(
        "- removed {} folders, {} messages; {} raw messages orphaned ({} failed to delete)",
        removed.folders,
        removed.messages,
        removed.orphaned_blobs.len(),
        failed
    );
    println!(
        "- store transaction (rows, full text, view reloads, commit): {:.0} ms",
        ms(store_time)
    );
    println!("- blob deletion: {:.0} ms", ms(blob_time));
    if reads.is_empty() {
        println!("- page reads during the removal: none completed");
    } else {
        println!(
            "- page reads (100 rows) on another thread during the removal: {}",
            summary(&reads)
        );
    }
    println!(
        "- store files {:.1} → {:.1} MiB (after close); blob store {} → {} files, {:.1} → {:.1} MiB",
        mib(bytes_before),
        mib(store_bytes(options)),
        blobs_before.files,
        blobs_after.files,
        mib(blobs_before.bytes),
        mib(blobs_after.bytes)
    );
    println!(
        "- left: {} account(s) (expected {}), {} messages; integrity {integrity}; FTS5 integrity-check ok",
        left.len(),
        others.len(),
        remaining
    );
    if !problems.is_empty() || integrity != "ok" || left.len() != others.len() {
        bail!("inconsistent after removal: {}", problems.join("; "));
    }
    println!("- recount check: consistent");
    Ok(())
}
