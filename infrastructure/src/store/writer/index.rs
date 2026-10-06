//! Rebuild Search Index (ML-081): every stored message indexed again, in place, a few at a time
//! on the writer's full-text lane, which runs only when no interactive write, header batch or
//! full-text batch waits. Each step replaces some entries in one transaction, so a search finds
//! every message by its old entry or by its new one, never by neither.
//!
//! A message whose body the store holds gets an entry from it. One without keeps its entry,
//! which holds words of the preview the store doesn't keep, or gets one from its headers and
//! snippet when it has none. Entries whose message is gone are deleted. `meta` holds the last
//! message done under `fts_rebuild_through`, so a rebuild cut short when the store closes goes
//! on when it opens again; schema v10 sets it to 0, which indexes every message once with M7's
//! folding and attachment names.
//!
//! FTS5's automatic merging is off while the key is there: replacing entries all through the
//! index made it merge large segments inside single commits, which held the writer for up to
//! 0.7 s at 100,000 messages. Each step merges a few pages itself instead, and once every
//! message is done the index is merged into one segment, a few pages a commit, which takes the
//! replaced entries out (a search ran about twice as slow before that).

use super::{FtsDoc, Sender, Writer, people};
use crate::store::{IndexProgress, IndexRebuilt, StoreError, codec, fts};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sealwax_domain::message;
use sealwax_domain::model::{Address, BlobKey, MessageId};
use std::io;
use std::time::{Duration, Instant};

/// Messages a step reads at most.
const STEP_MESSAGES: i64 = 64;
/// Reading and parsing bodies stops after this long, and the step commits what it has, so a
/// write that arrives meanwhile waits about this long, plus the commit.
const STEP_BUDGET: Duration = Duration::from_millis(8);
/// Pages of FTS5's merge work a step does, among segments of one level.
const STEP_MERGE: i64 = 32;
/// Pages of the final merge into one segment, a commit each.
const FINAL_MERGE: i64 = 32;
/// FTS5's default `automerge`, restored when a rebuild ends.
const AUTOMERGE: i64 = 4;
/// The `meta` key that holds the last message done while a rebuild runs.
const THROUGH: &str = "fts_rebuild_through";

pub(crate) struct Rebuild {
    replies: Vec<Sender<IndexRebuilt>>,
    started: Instant,
    /// The last message done.
    through: i64,
    /// The newest message when the rebuild started; later ones are indexed as they come.
    last: i64,
    /// Every message is done; the index is being merged.
    merging: bool,
    result: IndexRebuilt,
    progress: IndexProgress,
}

/// A message as a step reads it.
struct Stored {
    id: MessageId,
    blob: Option<BlobKey>,
    subject: String,
    sender: Address,
    recipients: Vec<Address>,
    snippet: String,
    indexed: bool,
}

impl Writer {
    /// Starts a rebuild from the first message, or adds `reply` to the one under way.
    pub(super) fn start_rebuild(&mut self, reply: Sender<IndexRebuilt>) {
        if let Some(rebuild) = &mut self.rebuild {
            rebuild.replies.push(reply);
            return;
        }
        match self.begin_rebuild(0) {
            Ok(mut rebuild) => {
                rebuild.replies.push(reply);
                self.rebuild = Some(rebuild);
            }
            Err(error) => {
                let _ = reply.try_send(Err(error));
            }
        }
    }

    /// Goes on with a rebuild an earlier run left unfinished, or the one schema v10 asked for.
    pub(super) fn resume_rebuild(&mut self) {
        let through = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [THROUGH], |row| {
                row.get::<_, i64>(0)
            })
            .optional();
        match through {
            Ok(Some(through)) => match self.begin_rebuild(through) {
                Ok(rebuild) => self.rebuild = Some(rebuild),
                Err(error) => tracing::warn!(%error, "search index rebuild failed to start"),
            },
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "search index rebuild failed to start"),
        }
    }

    fn begin_rebuild(&mut self, through: i64) -> Result<Rebuild, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (last, total, done): (i64, i64, i64) = tx.query_row(
            "SELECT coalesce(max(id), 0), count(*), count(*) FILTER (WHERE id <= ?1) \
             FROM messages",
            [through],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        tx.prepare_cached(
            "INSERT INTO meta(key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )?
        .execute(params![THROUGH, through])?;
        tx.execute(
            "INSERT INTO message_fts(message_fts, rank) VALUES ('automerge', 0)",
            [],
        )?;
        tx.commit()?;
        self.state.after_commit();
        let progress = IndexProgress {
            done: done as u64,
            total: total as u64,
        };
        self.publish_progress(Some(progress));
        tracing::info!(
            messages = progress.total - progress.done,
            "search index rebuild started"
        );
        Ok(Rebuild {
            replies: Vec::new(),
            started: Instant::now(),
            through,
            last,
            merging: false,
            result: IndexRebuilt::default(),
            progress,
        })
    }

    /// Leaves the rebuild for the next start: the store is closing.
    pub(super) fn stop_rebuild(&mut self) {
        if self.rebuild.take().is_some() {
            self.publish_progress(None);
            tracing::info!("search index rebuild paused until the store opens again");
        }
    }

    /// Indexes the next few messages, merges a few pages, or finishes.
    pub(super) fn rebuild_step(&mut self) {
        let Some(rebuild) = &self.rebuild else {
            return;
        };
        let step = if rebuild.merging {
            self.merge_step()
        } else {
            self.index_step()
        };
        if let Err(error) = step {
            tracing::warn!(%error, "search index rebuild failed");
            if let Some(rebuild) = self.rebuild.take() {
                self.publish_progress(None);
                let text = error.to_string();
                let mut error = Some(error);
                for reply in rebuild.replies {
                    let error = error
                        .take()
                        .unwrap_or_else(|| StoreError::Io(io::Error::other(text.clone())));
                    let _ = reply.try_send(Err(error));
                }
            }
        }
    }

    fn index_step(&mut self) -> Result<(), StoreError> {
        let Some(rebuild) = &self.rebuild else {
            return Ok(());
        };
        let (from, last) = (rebuild.through, rebuild.last);
        let started = Instant::now();
        let stored = self
            .conn
            .prepare_cached(
                "SELECT id, blob_key, subject, from_name, from_addr, to_addrs, cc_addrs, snippet, \
                   EXISTS (SELECT 1 FROM message_fts WHERE rowid = m.id) \
                 FROM messages m WHERE id > ?1 AND id <= ?2 ORDER BY id LIMIT ?3",
            )?
            .query_map(params![from, last, STEP_MESSAGES], |row| {
                Ok(Stored {
                    id: MessageId(row.get(0)?),
                    blob: row
                        .get::<_, Option<Vec<u8>>>(1)?
                        .and_then(|key| BlobKey::from_slice(&key)),
                    subject: row.get(2)?,
                    sender: Address::new(row.get::<_, String>(3)?, row.get::<_, String>(4)?),
                    recipients: [
                        codec::decode_addresses(&row.get::<_, String>(5)?),
                        codec::decode_addresses(&row.get::<_, String>(6)?),
                    ]
                    .concat(),
                    snippet: row.get(7)?,
                    indexed: row.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if stored.is_empty() {
            return self.end_walk();
        }
        let blobs = self.state.shared.blobs.clone();
        let mut docs = Vec::new();
        let mut kept = 0;
        let mut walked = 0;
        let mut through = from;
        for message in stored {
            through = message.id.0;
            walked += 1;
            let body = blobs
                .as_ref()
                .zip(message.blob)
                .and_then(|(blobs, key)| blobs.get(&key).ok())
                .map(|raw| {
                    let parsed = message::parse(&raw);
                    fts::body_text(&parsed.body_text, &parsed.attachments).into_owned()
                });
            let replace = body.is_some();
            match body.or_else(|| (!message.indexed).then(|| message.snippet.clone())) {
                Some(body) => docs.push((
                    FtsDoc {
                        id: message.id,
                        subject: message.subject,
                        sender: people(std::slice::from_ref(&message.sender)),
                        recipients: people(&message.recipients),
                        body,
                    },
                    replace,
                )),
                None => kept += 1,
            }
            if started.elapsed() >= STEP_BUDGET {
                break;
            }
        }
        let prepared = started.elapsed();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        {
            let mut unindex = tx.prepare_cached("DELETE FROM message_fts WHERE rowid = ?1")?;
            let mut insert = tx.prepare_cached(
                "INSERT INTO message_fts(rowid, subject, sender, recipients, body) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for (doc, replace) in &docs {
                if *replace {
                    unindex.execute([doc.id.0])?;
                }
                insert.execute(params![
                    doc.id.0,
                    fts::index_text(&doc.subject),
                    fts::index_text(&doc.sender),
                    fts::index_text(&doc.recipients),
                    fts::index_text(&doc.body),
                ])?;
            }
        }
        let orphans = orphans(&tx, from, Some(through))?;
        merge(&tx, STEP_MERGE)?;
        tx.prepare_cached("UPDATE meta SET value = ?2 WHERE key = ?1")?
            .execute(params![THROUGH, through])?;
        let written = started.elapsed();
        tx.commit()?;
        self.state.after_commit();
        tracing::debug!(
            messages = walked,
            indexed = docs.len(),
            read_ms = prepared.as_secs_f64() * 1000.0,
            write_ms = (started.elapsed() - prepared).as_secs_f64() * 1000.0,
            commit_ms = (started.elapsed() - written).as_secs_f64() * 1000.0,
            "search index rebuild step"
        );
        let Some(rebuild) = &mut self.rebuild else {
            return Ok(());
        };
        rebuild.through = through;
        rebuild.result.messages += docs.len() as u64;
        rebuild.result.kept += kept;
        rebuild.result.removed += orphans;
        rebuild.progress.done = (rebuild.progress.done + walked).min(rebuild.progress.total);
        let progress = rebuild.progress;
        self.publish_progress(Some(progress));
        Ok(())
    }

    /// Every message is done: entries above the last one whose message is gone are deleted,
    /// and the merge begins.
    fn end_walk(&mut self) -> Result<(), StoreError> {
        let Some(rebuild) = &self.rebuild else {
            return Ok(());
        };
        let last = rebuild.last;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let orphans = orphans(&tx, last, None)?;
        tx.commit()?;
        self.state.after_commit();
        if let Some(rebuild) = &mut self.rebuild {
            rebuild.result.removed += orphans;
            rebuild.merging = true;
            rebuild.progress.done = rebuild.progress.total;
            let progress = rebuild.progress;
            self.publish_progress(Some(progress));
        }
        Ok(())
    }

    /// A few pages of the merge into one segment, in a commit of their own; finishes once FTS5
    /// has nothing left to merge.
    fn merge_step(&mut self) -> Result<(), StoreError> {
        let before = self.conn.total_changes();
        let started = Instant::now();
        merge(&self.conn, -FINAL_MERGE)?;
        self.state.after_commit();
        tracing::debug!(
            ms = started.elapsed().as_secs_f64() * 1000.0,
            "search index merge step"
        );
        // FTS5's documented test: a merge that did no work changes fewer than two rows.
        if self.conn.total_changes() - before < 2 {
            self.finish_rebuild()?;
        }
        Ok(())
    }

    fn finish_rebuild(&mut self) -> Result<(), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.prepare_cached("DELETE FROM meta WHERE key = ?1")?
            .execute([THROUGH])?;
        tx.execute(
            "INSERT INTO message_fts(message_fts, rank) VALUES ('automerge', ?1)",
            [AUTOMERGE],
        )?;
        tx.commit()?;
        self.state.after_commit();
        let Some(mut rebuild) = self.rebuild.take() else {
            return Ok(());
        };
        self.publish_progress(None);
        rebuild.result.elapsed = rebuild.started.elapsed();
        tracing::info!(
            messages = rebuild.result.messages,
            kept = rebuild.result.kept,
            removed = rebuild.result.removed,
            ms = rebuild.result.elapsed.as_secs_f64() * 1000.0,
            "search index rebuilt"
        );
        for reply in rebuild.replies {
            let _ = reply.try_send(Ok(rebuild.result.clone()));
        }
        Ok(())
    }

    fn publish_progress(&self, progress: Option<IndexProgress>) {
        *self
            .state
            .shared
            .index_progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = progress;
    }
}

/// FTS5's incremental merge: up to `pages` pages among segments of one level, or of all
/// segments into one when negative.
fn merge(conn: &Connection, pages: i64) -> rusqlite::Result<()> {
    conn.prepare_cached("INSERT INTO message_fts(message_fts, rank) VALUES ('merge', ?1)")?
        .execute([pages])?;
    Ok(())
}

/// Deletes the entries after `from` (up to `through`) whose message is gone; returns how many.
fn orphans(tx: &Connection, from: i64, through: Option<i64>) -> Result<u64, StoreError> {
    let through = through.unwrap_or(i64::MAX);
    let gone = tx
        .prepare_cached(
            "SELECT rowid FROM message_fts WHERE rowid > ?1 AND rowid <= ?2 \
             EXCEPT SELECT id FROM messages WHERE id > ?1 AND id <= ?2",
        )?
        .query_map([from, through], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut unindex = tx.prepare_cached("DELETE FROM message_fts WHERE rowid = ?1")?;
    for id in &gone {
        unindex.execute([id])?;
    }
    Ok(gone.len() as u64)
}
