//! Headless measurement of the reader's read of a conversation (ML-057, `Store::conversation`)
//! at fixture scale: a two-message conversation of the corpus, and a 150-message one this
//! command files into the INBOX the first time (so run it on a copy of a store), each read cold
//! (store closed, its files evicted from the page cache, opened again) and warm.

use crate::bench::{Options, evict, open};
use crate::import::{ACCOUNT_NAME, account_address};
use crate::stats::summary;
use anyhow::{Context, Result, bail};
use sealwax_domain::model::{FolderRole, MessageId};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::ingest::{self, RawMessage};
use sealwax_infrastructure::store::{Store, ViewSpec};
use std::time::{Duration, Instant};

/// The long conversation's size.
const LONG: usize = 150;

/// One message of the long conversation, answering the one before it.
fn chained(n: usize, now: i64) -> Vec<u8> {
    let date = now - (LONG - n) as i64 * 1_800;
    let threading = if n == 0 {
        String::new()
    } else {
        format!(
            "In-Reply-To: <bench-conversation-{}@example.test>\r\n\
             References: <bench-conversation-0@example.test> <bench-conversation-{}@example.test>\r\n",
            n - 1,
            n - 1
        )
    };
    format!(
        "From: Deltaker {n} <deltaker{n}@example.test>\r\nTo: alex.berg@example.test\r\n\
         Subject: Re: Lang tråd\r\nDate: {}\r\nMessage-ID: <bench-conversation-{n}@example.test>\r\n\
         {threading}MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
         Innlegg {n} i den lange tråden.\r\n",
        crate::mime::date(date, 0)
    )
    .into_bytes()
}

/// The first INBOX message whose conversation has `size` messages, looking through the newest
/// 20,000.
fn with_size(store: &Store, spec: ViewSpec, size: u32) -> Result<Option<MessageId>> {
    for offset in (0..20_000).step_by(1_000) {
        let page = store.page(spec, offset, 1_000).recv_blocking()?;
        if page.rows.is_empty() {
            break;
        }
        if let Some(row) = page.rows.iter().find(|row| row.thread_count == size) {
            return Ok(Some(row.id));
        }
    }
    Ok(None)
}

fn read(store: &Store, id: MessageId) -> Result<(Duration, usize)> {
    let started = Instant::now();
    let conversation = store
        .conversation(id)
        .recv_blocking()?
        .value
        .context("the conversation is gone")?;
    Ok((started.elapsed(), conversation.messages.len()))
}

pub fn run(options: &Options) -> Result<()> {
    let blobs = BlobStore::open(&options.blobs)?;
    let mut store = open(options, &blobs)?;
    let accounts = store.accounts().recv_blocking()?.value;
    let account = accounts
        .iter()
        .find(|account| account.address == account_address(ACCOUNT_NAME))
        .context("no fixture account; run `import` first")?;
    let folders = store.folders().recv_blocking()?.value;
    let inbox = folders
        .iter()
        .find(|folder| folder.account == account.id && folder.role == FolderRole::Inbox)
        .context("no fixture INBOX")?;
    let spec = ViewSpec::folder(inbox.id);
    let messages: u32 = folders.iter().map(|folder| folder.total).sum();
    println!("## Store");
    println!(
        "- SQLite {}; {messages} messages, INBOX {}",
        rusqlite::version(),
        inbox.total
    );
    let short = with_size(&store, spec, 2)?.context("no two-message conversation in the INBOX")?;
    let long = match with_size(&store, spec, LONG as u32)? {
        Some(id) => id,
        None => {
            let raw = (0..LONG)
                .map(|n| RawMessage {
                    folder: inbox.id,
                    raw: chained(n, options.now),
                    ..RawMessage::default()
                })
                .collect();
            let filed = ingest::ingest_raw(&store, &blobs, raw)?;
            println!(
                "- filed a {LONG}-message conversation into the INBOX ({} stored)",
                filed.ids.iter().flatten().count()
            );
            filed
                .ids
                .last()
                .copied()
                .flatten()
                .context("the long conversation wasn't stored")?
        }
    };
    println!("\n## Store::conversation (cold: store reopened, page cache evicted)");
    for (name, id) in [("2-message", short), ("150-message", long)] {
        let mut cold = Vec::new();
        let mut shown = 0;
        for _ in 0..options.cold {
            store.close()?;
            evict(&options.store)?;
            store = open(options, &blobs)?;
            let (elapsed, count) = read(&store, id)?;
            cold.push(elapsed);
            shown = count;
        }
        let mut warm = Vec::new();
        for _ in 0..options.warm {
            let (elapsed, count) = read(&store, id)?;
            warm.push(elapsed);
            shown = count;
        }
        if name == "150-message" && shown != LONG {
            bail!("the long conversation shows {shown} messages, not {LONG}");
        }
        println!(
            "- {name} conversation ({shown} shown): cold {} | warm {}",
            summary(&cold),
            summary(&warm)
        );
    }
    store.close()?;
    Ok(())
}
