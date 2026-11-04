//! The cached mailbox the app opens at startup: S3's schema v1 and synthetic corpus (fictional
//! people on reserved `.example` domains), sized so INBOX holds at least 10,000 messages.

use anyhow::{Context, Result};
use email_spike_s3::store::{FtsDoc, NewMessage, Writer};
use email_spike_s3::synth::{self, Config, MAILBOXES};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub struct Built {
    pub messages: usize,
    pub inbox: usize,
    pub per_mailbox: Vec<(String, usize)>,
    pub bytes: u64,
    pub build_ms: f64,
}

pub fn db_files(path: &Path) -> Vec<PathBuf> {
    ["", "-wal", "-shm"]
        .iter()
        .map(|suffix| PathBuf::from(format!("{}{suffix}", path.display())))
        .collect()
}

pub fn build(path: &Path, inbox_target: usize, seed: u64) -> Result<Built> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    for file in db_files(path) {
        if file.exists() {
            std::fs::remove_file(&file).with_context(|| format!("remove {}", file.display()))?;
        }
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
    let mut total = inbox_target * 100 / 44;
    let mut corpus = loop {
        let corpus = synth::generate(Config {
            messages: total,
            seed,
            now,
            years: 5.0,
        });
        let inbox = corpus.messages.iter().filter(|m| m.mailbox == 0).count();
        if inbox >= inbox_target {
            break corpus;
        }
        total = total * inbox_target / inbox.max(1) + 64;
    };
    let started = Instant::now();
    let mut writer = Writer::open(path)?;
    let ids = synth::create_mailboxes(&mut writer)?;
    corpus.assign_mailboxes(&ids);
    let mut message_ids = Vec::with_capacity(corpus.messages.len());
    for chunk in corpus.messages.chunks(500) {
        let headers: Vec<NewMessage> = chunk.iter().map(|m| m.header.clone()).collect();
        message_ids.extend(writer.ingest(&headers)?);
    }
    let mut per_mailbox = Vec::new();
    for (index, spec) in MAILBOXES.iter().enumerate() {
        let members: Vec<_> = corpus
            .messages
            .iter()
            .filter(|m| m.mailbox == index)
            .collect();
        let (uidnext, modseq) = members
            .iter()
            .map(|m| (m.header.uid + 1, m.header.modseq))
            .max()
            .unwrap_or((1, 1));
        writer.set_mailbox_state(ids[index], uidnext, modseq)?;
        per_mailbox.push((spec.name.to_owned(), members.len()));
    }
    for (chunk, chunk_ids) in corpus.messages.chunks(500).zip(message_ids.chunks(500)) {
        let docs: Vec<FtsDoc> = chunk
            .iter()
            .zip(chunk_ids)
            .map(|(message, id)| message.doc(*id))
            .collect();
        writer.index(&docs)?;
    }
    writer
        .connection()
        .execute_batch("PRAGMA optimize=0x10002;")?;
    writer
        .connection()
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    drop(writer);
    let bytes = db_files(path)
        .iter()
        .filter_map(|file| std::fs::metadata(file).ok())
        .map(|meta| meta.len())
        .sum();
    Ok(Built {
        messages: corpus.messages.len(),
        inbox: per_mailbox.first().map_or(0, |(_, count)| *count),
        per_mailbox,
        bytes,
        build_ms: started.elapsed().as_secs_f64() * 1000.0,
    })
}
