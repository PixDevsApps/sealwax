//! The store reader thread. The GTK thread never runs SQL: it sends plain requests over
//! async-channel and awaits the replies. The thread starts at `startup`, before GTK initializes,
//! so opening the database and reading the first page overlap with GTK's own start.

use anyhow::{Context, Result};
use email_spike_s3::sections::Thresholds;
use email_spike_s3::store::{ListRow, Page, Reader, Snapshot, View};
use email_spike_s6::clock::mono_ns;
use std::path::PathBuf;

pub enum ReadRequest {
    Page { view: View, offset: u32, limit: u32 },
    Message { id: i64 },
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OpenTimes {
    pub thread_start: i64,
    pub connection_open: i64,
    pub snapshot_done: i64,
}

pub struct Opened {
    pub view: View,
    pub thresholds: Thresholds,
    pub snapshot: Snapshot,
    pub times: OpenTimes,
}

pub enum ReadReply {
    Opened(Box<Opened>),
    Page { view: View, page: Page },
    Message { id: i64, row: Option<ListRow> },
    Failed(String),
}

pub struct Channels {
    pub reads: async_channel::Sender<ReadRequest>,
    pub replies: async_channel::Receiver<ReadReply>,
}

/// Starts the reader thread, which opens INBOX first and then serves requests, newest first.
pub fn start(db: PathBuf, snapshot_rows: u32) -> Channels {
    let (reads, queue) = async_channel::unbounded::<ReadRequest>();
    let (reply, replies) = async_channel::unbounded();
    let spawned = std::thread::Builder::new()
        .name("s6-reader".into())
        .spawn(move || {
            let thread_start = mono_ns();
            let reader = match open(&db, snapshot_rows, thread_start) {
                Ok((reader, opened)) => {
                    if reply
                        .send_blocking(ReadReply::Opened(Box::new(opened)))
                        .is_err()
                    {
                        return;
                    }
                    reader
                }
                Err(error) => {
                    let _ = reply.send_blocking(ReadReply::Failed(format!("{error:#}")));
                    return;
                }
            };
            serve(&reader, &queue, &reply);
        });
    if let Err(error) = spawned {
        eprintln!("s6: reader thread: {error}");
    }
    Channels { reads, replies }
}

fn open(db: &std::path::Path, rows: u32, thread_start: i64) -> Result<(Reader, Opened)> {
    let reader = Reader::open(db).with_context(|| format!("open {}", db.display()))?;
    let connection_open = mono_ns();
    let inbox = reader
        .mailboxes()?
        .into_iter()
        .find(|mailbox| mailbox.role.as_deref() == Some("inbox"))
        .context("no INBOX in the cache")?;
    let view = View::Mailbox(inbox.id);
    let thresholds = Thresholds::local_now()?;
    let snapshot = reader.open_view(view, &thresholds, rows)?;
    Ok((
        reader,
        Opened {
            view,
            thresholds,
            snapshot,
            times: OpenTimes {
                thread_start,
                connection_open,
                snapshot_done: mono_ns(),
            },
        },
    ))
}

const MESSAGE_SQL: &str = "SELECT m.id, m.thread_id, m.mailbox_id, m.date, m.from_name, \
     m.from_addr, m.subject, m.snippet, m.flags, m.has_attachments, t.message_count \
     FROM messages m JOIN threads t ON t.id = m.thread_id WHERE m.id = ?1";

fn message(reader: &Reader, id: i64) -> Result<Option<ListRow>> {
    use rusqlite::OptionalExtension;
    Ok(reader
        .connection()
        .prepare_cached(MESSAGE_SQL)?
        .query_row([id], |row| {
            Ok(ListRow {
                id: row.get(0)?,
                thread_id: row.get(1)?,
                mailbox_id: row.get(2)?,
                date: row.get(3)?,
                from_name: row.get(4)?,
                from_addr: row.get(5)?,
                subject: row.get(6)?,
                snippet: row.get(7)?,
                flags: row.get(8)?,
                has_attachments: row.get(9)?,
                thread_count: row.get(10)?,
            })
        })
        .optional()?)
}

fn serve(
    reader: &Reader,
    queue: &async_channel::Receiver<ReadRequest>,
    reply: &async_channel::Sender<ReadReply>,
) {
    while let Ok(first) = queue.recv_blocking() {
        let mut batch = vec![first];
        while let Ok(more) = queue.try_recv() {
            batch.push(more);
        }
        for request in batch.into_iter().rev() {
            let answer = match request {
                ReadRequest::Page {
                    view,
                    offset,
                    limit,
                } => reader
                    .page(view, offset, limit)
                    .map(|page| ReadReply::Page { view, page }),
                ReadRequest::Message { id } => {
                    message(reader, id).map(|row| ReadReply::Message { id, row })
                }
            };
            let answer = answer.unwrap_or_else(|error| ReadReply::Failed(format!("{error:#}")));
            if reply.send_blocking(answer).is_err() {
                return;
            }
        }
    }
}
