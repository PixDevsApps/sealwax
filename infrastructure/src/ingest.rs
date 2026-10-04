//! The one path raw messages take into the store, for sync (M3) and the fixture importer alike:
//! the raw bytes go into the blob store, `domain::message::parse` reads them, and the writer
//! stores the rows, assigns conversations and indexes the text. Blobs are durable before the
//! commit that refers to them.

use crate::blobs::BlobStore;
use crate::store::{BodyState, Ingested, NewMessage, Reply, Store, StoreError};
use sealwax_domain::message;
use sealwax_domain::model::{Flags, FolderId};
use std::io;
use std::time::{Duration, Instant};

/// A raw message and what the server says about it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawMessage {
    pub folder: FolderId,
    /// The IMAP UID; `None` for local folders (see [`NewMessage::uid`]).
    pub uid: Option<u32>,
    pub flags: Flags,
    pub keywords: Vec<String>,
    /// IMAP INTERNALDATE; without it the Date header sorts the message.
    pub internal_date: Option<i64>,
    pub raw: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("blob store: {0}")]
    Blob(#[from] io::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Messages whose raw bytes are stored and parsed, ready for the writer.
#[derive(Debug, Default)]
pub struct Prepared {
    pub messages: Vec<NewMessage>,
    /// Time spent storing blobs, summed over the workers.
    pub blob_time: Duration,
    /// Time spent parsing, summed over the workers.
    pub parse_time: Duration,
    /// Wall time of the whole preparation.
    pub elapsed: Duration,
}

/// The workers `prepare` uses by default: the available cores, at most 8.
pub fn default_workers() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get().min(8))
}

/// Stores and parses `messages` on up to `workers` threads, keeping their order.
pub fn prepare(
    blobs: &BlobStore,
    messages: Vec<RawMessage>,
    workers: usize,
) -> io::Result<Prepared> {
    let started = Instant::now();
    let workers = workers.clamp(1, messages.len().max(1));
    let chunk = messages.len().div_ceil(workers).max(1);
    let mut prepared = Prepared::default();
    if workers == 1 {
        let (out, blob_time, parse_time) = prepare_chunk(blobs, messages)?;
        prepared.messages = out;
        prepared.blob_time = blob_time;
        prepared.parse_time = parse_time;
    } else {
        let mut chunks = Vec::with_capacity(workers);
        let mut rest = messages;
        while !rest.is_empty() {
            let tail = rest.split_off(chunk.min(rest.len()));
            chunks.push(std::mem::replace(&mut rest, tail));
        }
        let results: Vec<io::Result<(Vec<NewMessage>, Duration, Duration)>> =
            std::thread::scope(|scope| {
                let handles: Vec<_> = chunks
                    .into_iter()
                    .map(|chunk| scope.spawn(move || prepare_chunk(blobs, chunk)))
                    .collect();
                handles
                    .into_iter()
                    .map(|handle| {
                        handle
                            .join()
                            .unwrap_or_else(|_| Err(io::Error::other("ingest worker panicked")))
                    })
                    .collect()
            });
        for result in results {
            let (out, blob_time, parse_time) = result?;
            prepared.messages.extend(out);
            prepared.blob_time += blob_time;
            prepared.parse_time += parse_time;
        }
    }
    prepared.elapsed = started.elapsed();
    Ok(prepared)
}

fn prepare_chunk(
    blobs: &BlobStore,
    messages: Vec<RawMessage>,
) -> io::Result<(Vec<NewMessage>, Duration, Duration)> {
    let mut out = Vec::with_capacity(messages.len());
    let mut blob_time = Duration::ZERO;
    let mut parse_time = Duration::ZERO;
    for raw in messages {
        let started = Instant::now();
        let key = blobs.put(&raw.raw)?;
        let stored = Instant::now();
        let parsed = message::parse(&raw.raw);
        parse_time += stored.elapsed();
        blob_time += stored - started;
        let has_attachments = parsed.has_attachments();
        out.push(NewMessage {
            folder: raw.folder,
            uid: raw.uid,
            flags: raw.flags,
            keywords: raw.keywords,
            date: raw.internal_date.or(parsed.headers.date).unwrap_or(0),
            headers: parsed.headers,
            snippet: parsed.snippet,
            body_kind: parsed.body_kind,
            text_parts: parsed.text_parts,
            html_parts: parsed.html_parts,
            attachments: parsed.attachments,
            has_attachments,
            size: parsed.size,
            body_state: BodyState::Full,
            blob: Some(key),
            search_text: parsed.body_text,
        });
    }
    Ok((out, blob_time, parse_time))
}

/// Hands prepared messages to the writer.
pub fn submit(store: &Store, prepared: Prepared) -> Reply<Ingested> {
    store.ingest(prepared.messages)
}

/// Stores, parses and ingests `messages`, waiting for the writer. Blocks: run it off the GTK
/// thread.
pub fn ingest_raw(
    store: &Store,
    blobs: &BlobStore,
    messages: Vec<RawMessage>,
) -> Result<Ingested, IngestError> {
    let workers = if messages.len() < 16 {
        1
    } else {
        default_workers()
    };
    let prepared = prepare(blobs, messages, workers)?;
    Ok(submit(store, prepared).recv_blocking()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{FolderSpec, NewAccount, OpenView, StoreOptions, ViewSpec};
    use sealwax_domain::model::FolderRole;

    fn raw(n: u32, parent: Option<u32>) -> Vec<u8> {
        let mut text = format!(
            "From: Avsender {n} <avsender{n}@example.test>\r\nTo: mottaker@example.test\r\n\
             Subject: Melding {n}\r\nDate: Mon, 05 Oct 2026 09:{:02}:00 +0000\r\n\
             Message-ID: <m{n}@example.test>\r\n",
            n % 60
        );
        if let Some(parent) = parent {
            text.push_str(&format!(
                "In-Reply-To: <m{parent}@example.test>\r\nReferences: <m{parent}@example.test>\r\n"
            ));
        }
        text.push_str(&format!(
            "Content-Type: text/plain; charset=utf-8\r\n\r\nHei nummer {n}, blåbær.\r\n"
        ));
        text.into_bytes()
    }

    #[test]
    fn stores_parses_and_threads() {
        let dir = crate::store::tests::scratch();
        let blobs = BlobStore::open(&dir.path().join("blobs")).unwrap();
        let store = Store::open(
            &dir.path().join("store.sqlite"),
            StoreOptions {
                blobs: Some(blobs.clone()),
                ..StoreOptions::default()
            },
        )
        .unwrap();
        let account = store
            .add_account(NewAccount {
                name: "Test".into(),
                address: "me@example.test".into(),
            })
            .recv_blocking()
            .unwrap()
            .value;
        let inbox = store
            .upsert_folder(FolderSpec::local(account, "INBOX", FolderRole::Inbox))
            .recv_blocking()
            .unwrap()
            .value;
        let messages: Vec<RawMessage> = (1..=40)
            .map(|n| RawMessage {
                folder: inbox,
                raw: raw(n, (n % 4 == 0).then_some(n - 1)),
                ..RawMessage::default()
            })
            .collect();
        let ingested = ingest_raw(&store, &blobs, messages.clone()).unwrap();
        assert_eq!(ingested.ids.len(), 40);
        assert!(ingested.ids.iter().all(Option::is_some));
        assert_eq!(blobs.disk_usage().unwrap().files, 40);

        let again = ingest_raw(&store, &blobs, messages).unwrap();
        assert!(
            again.ids.iter().all(Option::is_none),
            "local re-import is skipped"
        );
        assert_eq!(again.version, ingested.version);

        let opened = store
            .open_view(OpenView {
                spec: ViewSpec::folder(inbox),
                first_page: 100,
                section_starts: Vec::new(),
            })
            .recv_blocking()
            .unwrap();
        assert_eq!(opened.total, 40);
        let replies = opened
            .rows
            .iter()
            .filter(|row| row.thread_count == 2)
            .count();
        assert_eq!(replies, 20);
        let first = ingested.ids[0].unwrap();
        let detail = store.message(first).recv_blocking().unwrap().value.unwrap();
        assert_eq!(detail.summary.sender_email, "avsender1@example.test");
        assert_eq!(detail.summary.snippet, "Hei nummer 1, blåbær.");
        assert_eq!(
            store.raw_message(first).recv_blocking().unwrap().unwrap(),
            raw(1, None)
        );
        let found = store.search("blåbær", None, 50).recv_blocking().unwrap();
        assert_eq!(found.value.len(), 40);
        assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
        store.close().unwrap();
    }
}
