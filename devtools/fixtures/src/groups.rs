//! Headless measurement of Group by at fixture scale (issues #24 and #4): the INBOX read as
//! groups by sender, by domain and by conversation, warm and cold, with the number of groups
//! each gives. Read-only: it changes nothing in the store.

use crate::bench::{Options, evict, open};
use crate::stats::summary;
use anyhow::{Context, Result};
use sealwax_domain::model::FolderRole;
use sealwax_domain::senders::GroupBy;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{MAX_GROUPS, ViewFilter, ViewSpec};
use std::time::{Duration, Instant};

pub fn run(options: &Options) -> Result<()> {
    let blobs = BlobStore::open(&options.blobs)?;
    let store = open(options, &blobs)?;
    let folders = store.folders().recv_blocking()?.value;
    let inbox = folders
        .iter()
        .filter(|folder| folder.role == FolderRole::Inbox)
        .max_by_key(|folder| folder.total)
        .context("no INBOX; run `import` first")?;
    println!(
        "# Group by, INBOX of {} messages (at most {MAX_GROUPS} groups an answer)",
        inbox.total
    );
    store.close()?;
    for by in [GroupBy::Sender, GroupBy::Domain, GroupBy::Conversation] {
        for filter in [ViewFilter::All, ViewFilter::Unread] {
            let spec = ViewSpec::folder(inbox.id).with_filter(filter);
            let mut cold = Vec::new();
            let mut groups = 0;
            for _ in 0..options.cold {
                evict(&options.store)?;
                let store = open(options, &blobs)?;
                let started = Instant::now();
                groups = store.sender_groups(spec, by).recv_blocking()?.len();
                cold.push(started.elapsed());
                store.close()?;
            }
            let store = open(options, &blobs)?;
            let mut warm: Vec<Duration> = Vec::new();
            for _ in 0..options.warm {
                let started = Instant::now();
                groups = store.sender_groups(spec, by).recv_blocking()?.len();
                warm.push(started.elapsed());
            }
            store.close()?;
            println!(
                "- {} {filter:?} ({groups} groups): warm {}; cold {}",
                by.name(),
                summary(&warm),
                summary(&cold)
            );
        }
    }
    Ok(())
}
