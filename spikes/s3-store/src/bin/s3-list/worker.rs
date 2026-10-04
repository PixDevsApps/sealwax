//! Store threads. The GTK thread never runs SQL: it sends plain requests over async-channel and
//! awaits replies with `glib::spawn_future_local`. Two read-only readers serve pages; one
//! writer owns the read-write connection; a checkpoint thread does all WAL maintenance, so no
//! UI-driven commit runs a checkpoint or restarts the WAL.

use anyhow::Result;
use email_spike_s3::sections::Thresholds;
use email_spike_s3::stats::ms;
use email_spike_s3::store::{
    Checkpointer, FtsDoc, InsertDiff, MailboxInfo, Page, Reader, Snapshot, View, Writer,
};
use email_spike_s3::synth;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub enum ReadRequest {
    Mailboxes,
    Open {
        view: View,
        thresholds: Thresholds,
        limit: u32,
    },
    Page {
        view: View,
        offset: u32,
        limit: u32,
    },
}

pub enum ReadReply {
    Mailboxes(Vec<MailboxInfo>),
    Opened {
        view: View,
        snapshot: Snapshot,
        query_ms: f64,
    },
    Page {
        view: View,
        page: Page,
        query_ms: f64,
    },
    Failed(String),
}

pub enum WriteRequest {
    Incoming {
        view: View,
        mailbox: i64,
        seed: u64,
        date: i64,
        uid: i64,
    },
    Expunge(Vec<i64>),
}

pub enum StoreEvent {
    Inserted {
        diff: Box<InsertDiff>,
        writer_ms: f64,
        sent: Instant,
    },
    Expunged(usize),
    Failed(String),
}

pub struct Channels {
    pub reads: async_channel::Sender<ReadRequest>,
    pub replies: async_channel::Receiver<ReadReply>,
    pub writes: async_channel::Sender<(WriteRequest, Instant)>,
    pub events: async_channel::Receiver<StoreEvent>,
    pub stop: Arc<AtomicBool>,
}

pub fn start(path: &Path, readers: usize) -> Result<Channels> {
    let (reads, read_queue) = async_channel::unbounded::<ReadRequest>();
    let (reply_sender, replies) = async_channel::unbounded();
    for index in 0..readers {
        let reader = Reader::open(path)?;
        let queue = read_queue.clone();
        let reply = reply_sender.clone();
        std::thread::Builder::new()
            .name(format!("s3-reader-{index}"))
            .spawn(move || serve_reads(reader, queue, reply))?;
    }
    let mut writer = Writer::open(path)?;
    writer.hand_off_checkpoints();
    let (writes, write_queue) = async_channel::unbounded();
    let (event_sender, events) = async_channel::unbounded();
    std::thread::Builder::new()
        .name("s3-writer".into())
        .spawn(move || serve_writes(writer, write_queue, event_sender))?;
    let stop = Arc::new(AtomicBool::new(false));
    let checkpoint_stop = stop.clone();
    let checkpoint_path = path.to_owned();
    std::thread::Builder::new()
        .name("s3-checkpoint".into())
        .spawn(move || checkpoints(checkpoint_path, checkpoint_stop))?;
    Ok(Channels {
        reads,
        replies,
        writes,
        events,
        stop,
    })
}

fn serve_reads(
    reader: Reader,
    queue: async_channel::Receiver<ReadRequest>,
    reply: async_channel::Sender<ReadReply>,
) {
    while let Ok(first) = queue.recv_blocking() {
        let mut batch = vec![first];
        while let Ok(more) = queue.try_recv() {
            batch.push(more);
        }
        for request in batch.into_iter().rev() {
            let started = Instant::now();
            let answer = match request {
                ReadRequest::Mailboxes => reader.mailboxes().map(ReadReply::Mailboxes),
                ReadRequest::Open {
                    view,
                    thresholds,
                    limit,
                } => reader
                    .open_view(view, &thresholds, limit)
                    .map(|snapshot| ReadReply::Opened {
                        view,
                        snapshot,
                        query_ms: ms(started.elapsed()),
                    }),
                ReadRequest::Page {
                    view,
                    offset,
                    limit,
                } => reader
                    .page(view, offset, limit)
                    .map(|page| ReadReply::Page {
                        view,
                        page,
                        query_ms: ms(started.elapsed()),
                    }),
            };
            let answer = answer.unwrap_or_else(|error| ReadReply::Failed(format!("{error:#}")));
            if reply.send_blocking(answer).is_err() {
                return;
            }
        }
    }
}

fn serve_writes(
    mut writer: Writer,
    queue: async_channel::Receiver<(WriteRequest, Instant)>,
    events: async_channel::Sender<StoreEvent>,
) {
    while let Ok((request, sent)) = queue.recv_blocking() {
        let (view, mailbox, seed, date, uid) = match request {
            WriteRequest::Incoming {
                view,
                mailbox,
                seed,
                date,
                uid,
            } => (view, mailbox, seed, date, uid),
            WriteRequest::Expunge(ids) => {
                let event = ids
                    .iter()
                    .try_for_each(|id| writer.expunge(*id).map(|_| ()))
                    .map(|()| StoreEvent::Expunged(ids.len()))
                    .unwrap_or_else(|error| StoreEvent::Failed(format!("{error:#}")));
                if events.send_blocking(event).is_err() {
                    return;
                }
                continue;
            }
        };
        let (mut header, body) = synth::incoming(seed, date, None);
        header.mailbox_id = mailbox;
        header.uid = uid;
        let doc = FtsDoc {
            id: 0,
            subject: header.subject.clone(),
            sender: format!("{} {}", header.from_name, header.from_addr),
            recipients: header.to_summary.clone(),
            body: synth::body(&body, usize::MAX),
        };
        if std::env::var_os("S3_DEBUG").is_some() {
            let started = Instant::now();
            let empty = writer
                .connection()
                .execute_batch("BEGIN IMMEDIATE; COMMIT;");
            eprintln!(
                "s3-list: writer: empty BEGIN IMMEDIATE/COMMIT {:.2} ms ({empty:?})",
                ms(started.elapsed())
            );
        }
        let started = Instant::now();
        let event = match writer.insert_one(&header, None, &[view]) {
            Ok(diff) => {
                let id = diff.row.id;
                let event = StoreEvent::Inserted {
                    diff: Box::new(diff),
                    writer_ms: ms(started.elapsed()),
                    sent,
                };
                let sent_ok = events.send_blocking(event).is_ok();
                if let Err(error) = writer.index(&[FtsDoc { id, ..doc }]) {
                    let _ = events.send_blocking(StoreEvent::Failed(format!("{error:#}")));
                }
                if !sent_ok {
                    return;
                }
                continue;
            }
            Err(error) => StoreEvent::Failed(format!("{error:#}")),
        };
        if events.send_blocking(event).is_err() {
            return;
        }
    }
}

fn checkpoints(path: PathBuf, stop: Arc<AtomicBool>) {
    let checkpointer = match Checkpointer::open(&path, 1000) {
        Ok(checkpointer) => checkpointer,
        Err(error) => {
            eprintln!("s3-list: checkpoint thread: {error:#}");
            return;
        }
    };
    let mut runs = 0;
    while !stop.load(Ordering::Relaxed) {
        match checkpointer.tick() {
            Ok(Some(_)) => runs += 1,
            Ok(None) => {}
            Err(error) => eprintln!("s3-list: checkpoint failed: {error:#}"),
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if std::env::var_os("S3_DEBUG").is_some() {
        eprintln!("s3-list: {runs} checkpoints during the run");
    }
}
