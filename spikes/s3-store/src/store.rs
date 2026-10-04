//! The writer (one connection, owned by one thread) and readers (read-only connections on
//! worker threads). Every write transaction bumps `store_version`; readers return the version
//! their snapshot saw, so the UI can tell whether a page matches the changes it has applied.

use crate::cjk;
use crate::schema;
use crate::sections::{Bounds, Thresholds};
use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::c_int;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

pub mod flags {
    pub const SEEN: i64 = 1;
    pub const ANSWERED: i64 = 2;
    pub const FLAGGED: i64 = 4;
    pub const DELETED: i64 = 8;
    pub const DRAFT: i64 = 16;
    pub const FORWARDED: i64 = 32;
    pub const JUNK: i64 = 64;
    pub const NOT_JUNK: i64 = 128;
}

/// A message header as sync delivers it.
#[derive(Clone, Debug, Default)]
pub struct NewMessage {
    pub mailbox_id: i64,
    pub uid: i64,
    pub modseq: i64,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub date: i64,
    pub sent_date: i64,
    pub from_name: String,
    pub from_addr: String,
    pub to_summary: String,
    pub cc_summary: String,
    pub subject: String,
    pub snippet: String,
    pub flags: i64,
    pub has_attachments: bool,
    pub size: i64,
    pub body_state: i64,
    pub blob_key: Option<[u8; 32]>,
}

#[derive(Clone, Debug, Default)]
pub struct FtsDoc {
    pub id: i64,
    pub subject: String,
    pub sender: String,
    pub recipients: String,
    pub body: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum View {
    All,
    Mailbox(i64),
}

/// What a message-list row shows.
#[derive(Clone, Debug, PartialEq)]
pub struct ListRow {
    pub id: i64,
    pub thread_id: i64,
    pub mailbox_id: i64,
    pub date: i64,
    pub from_name: String,
    pub from_addr: String,
    pub subject: String,
    pub snippet: String,
    pub flags: i64,
    pub has_attachments: bool,
    pub thread_count: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConversationRow {
    pub thread_id: i64,
    pub last_date: i64,
    pub subject: String,
    pub participants: String,
    pub message_count: i64,
    pub unread_count: i64,
    pub from_name: String,
    pub snippet: String,
}

/// The first screen of a view, read in one snapshot.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub version: i64,
    pub total: u32,
    pub bounds: Bounds,
    pub rows: Vec<ListRow>,
}

#[derive(Clone, Debug)]
pub struct Page {
    pub version: i64,
    pub offset: u32,
    pub rows: Vec<ListRow>,
}

/// The list diff for one inserted message: where it lands in each open message view, and how
/// its conversation moves in the conversation list of its mailbox.
#[derive(Clone, Debug)]
pub struct InsertDiff {
    pub version: i64,
    pub row: ListRow,
    pub positions: Vec<(View, u32)>,
    pub thread_move: Option<ThreadMove>,
    pub merged_threads: Vec<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadMove {
    pub mailbox_id: i64,
    pub from: Option<u32>,
    pub to: u32,
}

#[derive(Clone, Debug)]
pub struct MailboxInfo {
    pub id: i64,
    pub name: String,
    pub role: Option<String>,
    pub total: i64,
    pub unread: i64,
    pub next_uid: i64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SearchFilter {
    pub mailbox: Option<i64>,
    pub unread_only: bool,
}

/// How a search finds its newest matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchPlan {
    /// Walk every FTS match, look up its date and keep the newest.
    FtsFirst,
    /// Walk the date index newest first and stop at the first matches.
    DateFirst,
}

const ROW_COLUMNS: &str = "m.id, m.thread_id, m.mailbox_id, m.date, m.from_name, m.from_addr, \
     m.subject, m.snippet, m.flags, m.has_attachments, t.message_count";

fn list_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ListRow> {
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
}

const PARTICIPANT_SEPARATOR: char = '\u{1f}';
const MAX_PARTICIPANTS: usize = 8;

fn add_participant<'a>(list: &'a str, name: &str) -> Cow<'a, str> {
    if name.is_empty() || list.split(PARTICIPANT_SEPARATOR).any(|known| known == name) {
        return Cow::Borrowed(list);
    }
    if list.is_empty() {
        return Cow::Owned(name.to_owned());
    }
    if list.split(PARTICIPANT_SEPARATOR).count() >= MAX_PARTICIPANTS {
        return Cow::Borrowed(list);
    }
    Cow::Owned(format!("{list}{PARTICIPANT_SEPARATOR}{name}"))
}

/// The subject without reply and forward prefixes (English, Nordic, German, Chinese).
pub fn base_subject(subject: &str) -> &str {
    const PREFIXES: [&str; 14] = [
        "re", "sv", "fw", "fwd", "vs", "aw", "wg", "antw", "ref", "tr", "回复", "答复", "转发",
        "回覆",
    ];
    let mut rest = subject.trim_start();
    loop {
        let Some((colon, width)) = rest
            .char_indices()
            .take(12)
            .find(|(_, c)| *c == ':' || *c == '：')
            .map(|(index, c)| (index, c.len_utf8()))
        else {
            return rest;
        };
        let prefix = rest[..colon].trim_end();
        let prefix = match prefix.find('[') {
            Some(bracket) if prefix.ends_with(']') => &prefix[..bracket],
            _ => prefix,
        };
        if !PREFIXES
            .iter()
            .any(|known| prefix.eq_ignore_ascii_case(known))
        {
            return rest;
        }
        rest = rest[colon + width..].trim_start();
    }
}

fn bump_version(conn: &Connection) -> Result<i64> {
    Ok(conn
        .prepare_cached("UPDATE store_version SET version = version + 1 RETURNING version")?
        .query_row([], |row| row.get(0))?)
}

fn store_version(conn: &Connection) -> Result<i64> {
    Ok(conn
        .prepare_cached("SELECT version FROM store_version")?
        .query_row([], |row| row.get(0))?)
}

fn count(conn: &Connection, sql: &str, params: impl rusqlite::Params) -> Result<u32> {
    let value: i64 = conn
        .prepare_cached(sql)?
        .query_row(params, |row| row.get(0))?;
    Ok(u32::try_from(value)?)
}

/// Position of `(date, id)` in a view ordered by `date DESC, id DESC`.
pub fn position(conn: &Connection, view: View, date: i64, id: i64) -> Result<u32> {
    Ok(match view {
        View::All => {
            count(
                conn,
                "SELECT count(*) FROM messages WHERE date > ?1",
                [date],
            )? + count(
                conn,
                "SELECT count(*) FROM messages WHERE date = ?1 AND id > ?2",
                [date, id],
            )?
        }
        View::Mailbox(mailbox) => {
            count(
                conn,
                "SELECT count(*) FROM messages WHERE mailbox_id = ?1 AND date > ?2",
                [mailbox, date],
            )? + count(
                conn,
                "SELECT count(*) FROM messages WHERE mailbox_id = ?1 AND date = ?2 AND id > ?3",
                [mailbox, date, id],
            )?
        }
    })
}

/// Position of a conversation in its mailbox's conversation list (`last_date DESC, thread DESC`).
pub fn thread_position(
    conn: &Connection,
    mailbox: i64,
    last_date: i64,
    thread: i64,
) -> Result<u32> {
    Ok(count(
        conn,
        "SELECT count(*) FROM thread_mailboxes WHERE mailbox_id = ?1 AND last_date > ?2",
        [mailbox, last_date],
    )? + count(
        conn,
        "SELECT count(*) FROM thread_mailboxes \
         WHERE mailbox_id = ?1 AND last_date = ?2 AND thread_id > ?3",
        [mailbox, last_date, thread],
    )?)
}

fn row_by_id(conn: &Connection, id: i64) -> Result<ListRow> {
    Ok(conn
        .prepare_cached(&format!(
            "SELECT {ROW_COLUMNS} FROM messages m JOIN threads t ON t.id = m.thread_id WHERE m.id = ?1"
        ))?
        .query_row([id], list_row)?)
}

struct Resolved {
    thread: i64,
    merged: Vec<i64>,
}

pub struct Writer {
    conn: Connection,
    account_of: HashMap<i64, i64>,
}

impl Writer {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = schema::open_writer(path)?;
        let account_of = conn
            .prepare("SELECT id, account_id FROM mailboxes")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<HashMap<i64, i64>>>()?;
        Ok(Self { conn, account_of })
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn add_account(&mut self, name: &str, address: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO accounts(name, address, kind) VALUES (?1, ?2, 'imap')",
            params![name, address],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn add_mailbox(
        &mut self,
        account: i64,
        name: &str,
        role: Option<&str>,
        uidvalidity: i64,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO mailboxes(account_id, name, role, uidvalidity, uidnext, highestmodseq) \
             VALUES (?1, ?2, ?3, ?4, 1, 1)",
            params![account, name, role, uidvalidity],
        )?;
        let id = self.conn.last_insert_rowid();
        self.account_of.insert(id, account);
        Ok(id)
    }

    pub fn set_mailbox_state(
        &mut self,
        mailbox: i64,
        uidnext: i64,
        highestmodseq: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE mailboxes SET uidnext = ?2, highestmodseq = ?3 WHERE id = ?1",
            params![mailbox, uidnext, highestmodseq],
        )?;
        Ok(())
    }

    fn account(&self, mailbox: i64) -> Result<i64> {
        self.account_of
            .get(&mailbox)
            .copied()
            .ok_or_else(|| anyhow!("unknown mailbox {mailbox}"))
    }

    /// Headers in one transaction; returns the new message ids in input order.
    pub fn ingest(&mut self, batch: &[NewMessage]) -> Result<Vec<i64>> {
        let accounts = batch
            .iter()
            .map(|message| self.account(message.mailbox_id))
            .collect::<Result<Vec<_>>>()?;
        let tx = self.conn.transaction()?;
        let mut ids = Vec::with_capacity(batch.len());
        for (message, account) in batch.iter().zip(accounts) {
            let resolved = resolve_thread(&tx, account, message)?;
            ids.push(store_message(&tx, resolved.thread, message)?);
        }
        bump_version(&tx)?;
        tx.commit()?;
        Ok(ids)
    }

    /// Full-text documents for messages already stored, in one transaction. Lists don't
    /// change, so `store_version` doesn't either.
    pub fn index(&mut self, docs: &[FtsDoc]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for doc in docs {
            index_doc(&tx, doc.id, doc)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Turns automatic checkpoints off on this connection, without a replacement.
    pub fn disable_autocheckpoint(&mut self) -> Result<()> {
        self.conn.execute_batch("PRAGMA wal_autocheckpoint=0;")?;
        Ok(())
    }

    /// Leaves checkpoints to a `Checkpointer` on another thread: this connection's WAL hook
    /// (which replaces autocheckpoint) only records how many frames the WAL holds.
    pub fn hand_off_checkpoints(&mut self) {
        self.conn.wal_hook(Some(record_wal_frames));
    }

    /// Headers and their full text in one transaction.
    pub fn ingest_with_text(&mut self, batch: &[(NewMessage, FtsDoc)]) -> Result<()> {
        let accounts = batch
            .iter()
            .map(|(message, _)| self.account(message.mailbox_id))
            .collect::<Result<Vec<_>>>()?;
        let tx = self.conn.transaction()?;
        for ((message, doc), account) in batch.iter().zip(accounts) {
            let resolved = resolve_thread(&tx, account, message)?;
            let id = store_message(&tx, resolved.thread, message)?;
            index_doc(&tx, id, doc)?;
        }
        bump_version(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// One new message plus the diff the open lists need, in one transaction.
    pub fn insert_one(
        &mut self,
        message: &NewMessage,
        doc: Option<&FtsDoc>,
        views: &[View],
    ) -> Result<InsertDiff> {
        let account = self.account(message.mailbox_id)?;
        let tx = self.conn.transaction()?;
        let resolved = resolve_thread(&tx, account, message)?;
        let mailbox = message.mailbox_id;
        let before: Option<i64> = tx
            .prepare_cached(
                "SELECT last_date FROM thread_mailboxes WHERE thread_id = ?1 AND mailbox_id = ?2",
            )?
            .query_row([resolved.thread, mailbox], |row| row.get(0))
            .optional()?;
        let from = match before {
            Some(last_date) if resolved.merged.is_empty() => {
                Some(thread_position(&tx, mailbox, last_date, resolved.thread)?)
            }
            _ => None,
        };
        let id = store_message(&tx, resolved.thread, message)?;
        if let Some(doc) = doc {
            index_doc(&tx, id, doc)?;
        }
        let last_date: i64 = tx
            .prepare_cached(
                "SELECT last_date FROM thread_mailboxes WHERE thread_id = ?1 AND mailbox_id = ?2",
            )?
            .query_row([resolved.thread, mailbox], |row| row.get(0))?;
        let thread_move = if resolved.merged.is_empty() {
            Some(ThreadMove {
                mailbox_id: mailbox,
                from,
                to: thread_position(&tx, mailbox, last_date, resolved.thread)?,
            })
        } else {
            None
        };
        let positions = views
            .iter()
            .filter(|view| matches!(view, View::All) || **view == View::Mailbox(mailbox))
            .map(|view| Ok((*view, position(&tx, *view, message.date, id)?)))
            .collect::<Result<Vec<_>>>()?;
        let row = row_by_id(&tx, id)?;
        let version = bump_version(&tx)?;
        tx.commit()?;
        Ok(InsertDiff {
            version,
            row,
            positions,
            thread_move,
            merged_threads: resolved.merged,
        })
    }

    /// Replaces a message's flags and keeps every unread and flagged count in step.
    pub fn set_flags(&mut self, id: i64, new_flags: i64) -> Result<i64> {
        let tx = self.conn.transaction()?;
        let (mailbox, thread, old): (i64, i64, i64) = tx
            .prepare_cached("SELECT mailbox_id, thread_id, flags FROM messages WHERE id = ?1")?
            .query_row([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        let unread = |value: i64| i64::from(value & flags::SEEN == 0);
        let flagged = |value: i64| i64::from(value & flags::FLAGGED != 0);
        let unread_delta = unread(new_flags) - unread(old);
        let flagged_delta = flagged(new_flags) - flagged(old);
        tx.prepare_cached("UPDATE messages SET flags = ?2 WHERE id = ?1")?
            .execute([id, new_flags])?;
        tx.prepare_cached("UPDATE mailboxes SET unread_count = unread_count + ?2 WHERE id = ?1")?
            .execute([mailbox, unread_delta])?;
        tx.prepare_cached(
            "UPDATE threads SET unread_count = unread_count + ?2, \
             flagged_count = flagged_count + ?3 WHERE id = ?1",
        )?
        .execute([thread, unread_delta, flagged_delta])?;
        tx.prepare_cached(
            "UPDATE thread_mailboxes SET unread_count = unread_count + ?3 \
             WHERE thread_id = ?1 AND mailbox_id = ?2",
        )?
        .execute([thread, mailbox, unread_delta])?;
        let version = bump_version(&tx)?;
        tx.commit()?;
        Ok(version)
    }

    /// Removes a message (an EXPUNGE or VANISHED) and recomputes the aggregates it touched.
    pub fn expunge(&mut self, id: i64) -> Result<i64> {
        let tx = self.conn.transaction()?;
        let (mailbox, thread, message_flags): (i64, i64, i64) = tx
            .prepare_cached("SELECT mailbox_id, thread_id, flags FROM messages WHERE id = ?1")?
            .query_row([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        tx.prepare_cached("DELETE FROM messages WHERE id = ?1")?
            .execute([id])?;
        tx.prepare_cached("DELETE FROM message_fts WHERE rowid = ?1")?
            .execute([id])?;
        tx.prepare_cached(
            "UPDATE mailboxes SET total_count = total_count - 1, unread_count = unread_count - ?2 \
             WHERE id = ?1",
        )?
        .execute([mailbox, i64::from(message_flags & flags::SEEN == 0)])?;
        recompute_thread_mailbox(&tx, thread, mailbox)?;
        recompute_thread(&tx, thread)?;
        let version = bump_version(&tx)?;
        tx.commit()?;
        Ok(version)
    }

    /// Recomputes every maintained aggregate from the messages and lists disagreements.
    pub fn check_aggregates(&self) -> Result<Vec<String>> {
        check_aggregates(&self.conn)
    }
}

fn resolve_thread(conn: &Connection, account: i64, message: &NewMessage) -> Result<Resolved> {
    let mut keys: Vec<&str> = message
        .message_id
        .iter()
        .chain(message.in_reply_to.iter())
        .chain(message.references.iter())
        .map(String::as_str)
        .filter(|key| !key.is_empty())
        .collect();
    keys.sort_unstable();
    keys.dedup();
    let mut found: Vec<i64> = Vec::new();
    {
        let mut lookup = conn.prepare_cached(
            "SELECT thread_id FROM thread_ids WHERE account_id = ?1 AND message_id = ?2",
        )?;
        for key in &keys {
            if let Some(thread) = lookup
                .query_row(params![account, key], |row| row.get::<_, i64>(0))
                .optional()?
                && !found.contains(&thread)
            {
                found.push(thread);
            }
        }
    }
    let resolved = match found.as_slice() {
        [] => {
            conn.prepare_cached(
                "INSERT INTO threads(account_id, subject, first_date, last_date) VALUES (?1, ?2, ?3, ?3)",
            )?
            .execute(params![account, base_subject(&message.subject), message.date])?;
            Resolved {
                thread: conn.last_insert_rowid(),
                merged: Vec::new(),
            }
        }
        [single] => Resolved {
            thread: *single,
            merged: Vec::new(),
        },
        _ => merge_threads(conn, &found)?,
    };
    let mut alias = conn.prepare_cached(
        "INSERT OR IGNORE INTO thread_ids(account_id, message_id, thread_id) VALUES (?1, ?2, ?3)",
    )?;
    for key in &keys {
        alias.execute(params![account, key, resolved.thread])?;
    }
    Ok(resolved)
}

/// Union of several conversations into the one with the most messages.
fn merge_threads(conn: &Connection, threads: &[i64]) -> Result<Resolved> {
    let mut sized = Vec::with_capacity(threads.len());
    for thread in threads {
        let size: i64 = conn
            .prepare_cached("SELECT message_count FROM threads WHERE id = ?1")?
            .query_row([thread], |row| row.get(0))?;
        sized.push((size, -thread));
    }
    sized.sort_unstable();
    let target = -sized.last().context("no threads to merge")?.1;
    let merged: Vec<i64> = threads.iter().copied().filter(|t| *t != target).collect();
    for other in &merged {
        conn.prepare_cached("UPDATE messages SET thread_id = ?1 WHERE thread_id = ?2")?
            .execute([target, *other])?;
        conn.prepare_cached("UPDATE thread_ids SET thread_id = ?1 WHERE thread_id = ?2")?
            .execute([target, *other])?;
        conn.prepare_cached(
            "INSERT INTO thread_mailboxes(thread_id, mailbox_id, last_date, latest_id, message_count, unread_count) \
             SELECT ?1, mailbox_id, last_date, latest_id, message_count, unread_count \
             FROM thread_mailboxes WHERE thread_id = ?2 \
             ON CONFLICT(thread_id, mailbox_id) DO UPDATE SET \
               latest_id = CASE WHEN excluded.last_date > last_date \
                 OR (excluded.last_date = last_date AND excluded.latest_id > latest_id) \
                 THEN excluded.latest_id ELSE latest_id END, \
               last_date = max(last_date, excluded.last_date), \
               message_count = message_count + excluded.message_count, \
               unread_count = unread_count + excluded.unread_count",
        )?
        .execute([target, *other])?;
        let (target_people, other_people): (String, String) = conn
            .prepare_cached(
                "SELECT a.participants, b.participants FROM threads a, threads b \
                 WHERE a.id = ?1 AND b.id = ?2",
            )?
            .query_row([target, *other], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut people = Cow::Borrowed(target_people.as_str());
        for name in other_people.split(PARTICIPANT_SEPARATOR) {
            people = Cow::Owned(add_participant(&people, name).into_owned());
        }
        conn.prepare_cached(
            "UPDATE threads SET \
               message_count = threads.message_count + o.message_count, \
               unread_count = threads.unread_count + o.unread_count, \
               flagged_count = threads.flagged_count + o.flagged_count, \
               attachment_count = threads.attachment_count + o.attachment_count, \
               latest_id = CASE WHEN o.last_date > threads.last_date \
                 OR (o.last_date = threads.last_date AND o.latest_id > threads.latest_id) \
                 THEN o.latest_id ELSE threads.latest_id END, \
               last_date = max(threads.last_date, o.last_date), \
               subject = CASE WHEN o.first_date < threads.first_date THEN o.subject ELSE threads.subject END, \
               first_date = min(threads.first_date, o.first_date), \
               participants = ?3 \
             FROM (SELECT * FROM threads WHERE id = ?2) AS o WHERE threads.id = ?1",
        )?
        .execute(params![target, *other, people.as_ref()])?;
        conn.prepare_cached("DELETE FROM threads WHERE id = ?1")?
            .execute([*other])?;
    }
    Ok(Resolved {
        thread: target,
        merged,
    })
}

fn store_message(conn: &Connection, thread: i64, message: &NewMessage) -> Result<i64> {
    conn.prepare_cached(
        "INSERT INTO messages(mailbox_id, uid, modseq, message_id, thread_id, date, sent_date, \
         from_name, from_addr, to_summary, cc_summary, subject, snippet, flags, has_attachments, \
         size, body_state, blob_key) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
    )?
    .execute(params![
        message.mailbox_id,
        message.uid,
        message.modseq,
        message.message_id,
        thread,
        message.date,
        message.sent_date,
        message.from_name,
        message.from_addr,
        message.to_summary,
        message.cc_summary,
        message.subject,
        message.snippet,
        message.flags,
        message.has_attachments,
        message.size,
        message.body_state,
        message.blob_key.as_ref().map(|key| key.as_slice()),
    ])?;
    let id = conn.last_insert_rowid();
    let unread = i64::from(message.flags & flags::SEEN == 0);
    let flagged = i64::from(message.flags & flags::FLAGGED != 0);
    let people: String = conn
        .prepare_cached("SELECT participants FROM threads WHERE id = ?1")?
        .query_row([thread], |row| row.get(0))?;
    let people = add_participant(&people, &message.from_name);
    conn.prepare_cached(
        "UPDATE threads SET \
           message_count = message_count + 1, \
           unread_count = unread_count + ?2, \
           flagged_count = flagged_count + ?3, \
           attachment_count = attachment_count + ?4, \
           latest_id = CASE WHEN ?5 >= last_date THEN ?6 ELSE latest_id END, \
           last_date = max(last_date, ?5), \
           subject = CASE WHEN ?5 < first_date THEN ?7 ELSE subject END, \
           first_date = min(first_date, ?5), \
           participants = ?8 \
         WHERE id = ?1",
    )?
    .execute(params![
        thread,
        unread,
        flagged,
        i64::from(message.has_attachments),
        message.date,
        id,
        base_subject(&message.subject),
        people.as_ref(),
    ])?;
    conn.prepare_cached(
        "INSERT INTO thread_mailboxes(thread_id, mailbox_id, last_date, latest_id, message_count, unread_count) \
         VALUES (?1, ?2, ?3, ?4, 1, ?5) \
         ON CONFLICT(thread_id, mailbox_id) DO UPDATE SET \
           latest_id = CASE WHEN excluded.last_date >= last_date THEN excluded.latest_id ELSE latest_id END, \
           last_date = max(last_date, excluded.last_date), \
           message_count = message_count + 1, \
           unread_count = unread_count + excluded.unread_count",
    )?
    .execute(params![thread, message.mailbox_id, message.date, id, unread])?;
    conn.prepare_cached(
        "UPDATE mailboxes SET total_count = total_count + 1, unread_count = unread_count + ?2 \
         WHERE id = ?1",
    )?
    .execute([message.mailbox_id, unread])?;
    Ok(id)
}

fn index_doc(conn: &Connection, id: i64, doc: &FtsDoc) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO message_fts(rowid, subject, sender, recipients, body) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?
    .execute(params![
        id,
        cjk::index_text(&doc.subject).as_ref(),
        cjk::index_text(&doc.sender).as_ref(),
        cjk::index_text(&doc.recipients).as_ref(),
        cjk::index_text(&doc.body).as_ref(),
    ])?;
    Ok(())
}

fn recompute_thread_mailbox(conn: &Connection, thread: i64, mailbox: i64) -> Result<()> {
    let latest: Option<(i64, i64)> = conn
        .prepare_cached(
            "SELECT id, date FROM messages WHERE thread_id = ?1 AND mailbox_id = ?2 \
             ORDER BY date DESC, id DESC LIMIT 1",
        )?
        .query_row([thread, mailbox], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    match latest {
        None => {
            conn.prepare_cached(
                "DELETE FROM thread_mailboxes WHERE thread_id = ?1 AND mailbox_id = ?2",
            )?
            .execute([thread, mailbox])?;
        }
        Some((latest_id, last_date)) => {
            conn.prepare_cached(
                "UPDATE thread_mailboxes SET latest_id = ?3, last_date = ?4, \
                   message_count = (SELECT count(*) FROM messages WHERE thread_id = ?1 AND mailbox_id = ?2), \
                   unread_count = (SELECT count(*) FROM messages \
                     WHERE thread_id = ?1 AND mailbox_id = ?2 AND flags & 1 = 0) \
                 WHERE thread_id = ?1 AND mailbox_id = ?2",
            )?
            .execute([thread, mailbox, latest_id, last_date])?;
        }
    }
    Ok(())
}

fn recompute_thread(conn: &Connection, thread: i64) -> Result<()> {
    let latest: Option<(i64, i64)> = conn
        .prepare_cached(
            "SELECT id, date FROM messages WHERE thread_id = ?1 ORDER BY date DESC, id DESC LIMIT 1",
        )?
        .query_row([thread], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    let Some((latest_id, last_date)) = latest else {
        conn.prepare_cached("DELETE FROM thread_ids WHERE thread_id = ?1")?
            .execute([thread])?;
        conn.prepare_cached("DELETE FROM threads WHERE id = ?1")?
            .execute([thread])?;
        return Ok(());
    };
    let mut people = String::new();
    {
        let mut senders = conn.prepare_cached(
            "SELECT from_name FROM messages WHERE thread_id = ?1 ORDER BY date, id",
        )?;
        let mut rows = senders.query([thread])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            people = add_participant(&people, &name).into_owned();
        }
    }
    let (first_date, subject): (i64, String) = conn
        .prepare_cached(
            "SELECT date, subject FROM messages WHERE thread_id = ?1 ORDER BY date, id LIMIT 1",
        )?
        .query_row([thread], |row| Ok((row.get(0)?, row.get(1)?)))?;
    conn.prepare_cached(
        "UPDATE threads SET latest_id = ?2, last_date = ?3, first_date = ?4, subject = ?5, participants = ?6, \
           message_count = (SELECT count(*) FROM messages WHERE thread_id = ?1), \
           unread_count = (SELECT count(*) FROM messages WHERE thread_id = ?1 AND flags & 1 = 0), \
           flagged_count = (SELECT count(*) FROM messages WHERE thread_id = ?1 AND flags & 4 <> 0), \
           attachment_count = (SELECT count(*) FROM messages WHERE thread_id = ?1 AND has_attachments) \
         WHERE id = ?1",
    )?
    .execute(params![
        thread,
        latest_id,
        last_date,
        first_date,
        base_subject(&subject),
        people
    ])?;
    Ok(())
}

/// A PASSIVE checkpoint: copies what it can without waiting for readers or the writer.
pub fn checkpoint(conn: &Connection) -> Result<(i64, i64, i64)> {
    Ok(conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?)
}

/// Frames in the WAL after the writer's last commit, kept by the writer's WAL hook.
pub static WAL_FRAMES: AtomicI64 = AtomicI64::new(0);

fn record_wal_frames(_wal: &rusqlite::hooks::Wal, frames: c_int) -> rusqlite::Result<()> {
    WAL_FRAMES.store(i64::from(frames), Ordering::Relaxed);
    Ok(())
}

#[derive(Clone, Copy, Debug)]
pub struct CheckpointRun {
    pub frames: i64,
    pub checkpoint_ms: f64,
    pub restart_ms: Option<f64>,
}

/// Background WAL maintenance on its own connection. The first commit after a checkpoint has
/// emptied the WAL restarts it and fsyncs the new WAL header (about 10 ms on the development
/// machine); a one-row write right after the checkpoint pays that here instead of in the next
/// UI-driven commit.
pub struct Checkpointer {
    conn: Connection,
    threshold: i64,
}

impl Checkpointer {
    pub fn open(path: &Path, threshold: i64) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
        let checkpointer = Self { conn, threshold };
        checkpointer.restart()?;
        Ok(checkpointer)
    }

    /// Checkpoints once the WAL holds at least `threshold` frames.
    pub fn tick(&self) -> Result<Option<CheckpointRun>> {
        let frames = WAL_FRAMES.load(Ordering::Relaxed);
        if frames < self.threshold {
            return Ok(None);
        }
        let started = Instant::now();
        let (busy, log, done) = checkpoint(&self.conn)?;
        let checkpoint_ms = started.elapsed().as_secs_f64() * 1000.0;
        let restart_ms = if busy == 0 && log > 0 && done == log {
            let started = Instant::now();
            self.restart()?;
            Some(started.elapsed().as_secs_f64() * 1000.0)
        } else {
            None
        };
        WAL_FRAMES.store(0, Ordering::Relaxed);
        Ok(Some(CheckpointRun {
            frames,
            checkpoint_ms,
            restart_ms,
        }))
    }

    /// A one-row write: restarts an emptied WAL (and fsyncs its header) on this connection.
    pub fn restart(&self) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES ('wal_restarts', 1) \
             ON CONFLICT(key) DO UPDATE SET value = value + 1",
            [],
        )?;
        Ok(())
    }

    pub fn truncate(&self) -> Result<()> {
        self.conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    }
}

pub fn check_aggregates(conn: &Connection) -> Result<Vec<String>> {
    let checks = [
        (
            "mailbox counts",
            "SELECT count(*) FROM mailboxes b LEFT JOIN \
               (SELECT mailbox_id, count(*) AS n, sum(flags & 1 = 0) AS u FROM messages GROUP BY mailbox_id) c \
               ON c.mailbox_id = b.id \
             WHERE b.total_count <> coalesce(c.n, 0) OR b.unread_count <> coalesce(c.u, 0)",
        ),
        (
            "thread counts",
            "SELECT count(*) FROM threads t LEFT JOIN \
               (SELECT thread_id, count(*) AS n, sum(flags & 1 = 0) AS u, sum(flags & 4 <> 0) AS f, \
                       sum(has_attachments) AS a, max(date) AS l, min(date) AS e \
                FROM messages GROUP BY thread_id) c ON c.thread_id = t.id \
             WHERE t.message_count <> coalesce(c.n, 0) OR t.unread_count <> coalesce(c.u, 0) \
                OR t.flagged_count <> coalesce(c.f, 0) OR t.attachment_count <> coalesce(c.a, 0) \
                OR t.last_date <> c.l OR t.first_date <> c.e",
        ),
        (
            "thread latest message",
            "SELECT count(*) FROM threads t WHERE t.latest_id IS NOT \
               (SELECT id FROM messages WHERE thread_id = t.id ORDER BY date DESC, id DESC LIMIT 1)",
        ),
        (
            "empty threads",
            "SELECT count(*) FROM threads t WHERE NOT EXISTS (SELECT 1 FROM messages WHERE thread_id = t.id)",
        ),
        (
            "conversation rows",
            "SELECT count(*) FROM thread_mailboxes tm LEFT JOIN \
               (SELECT thread_id, mailbox_id, count(*) AS n, sum(flags & 1 = 0) AS u, max(date) AS l \
                FROM messages GROUP BY thread_id, mailbox_id) c \
               ON c.thread_id = tm.thread_id AND c.mailbox_id = tm.mailbox_id \
             WHERE tm.message_count <> coalesce(c.n, 0) OR tm.unread_count <> coalesce(c.u, 0) \
                OR tm.last_date <> c.l OR tm.latest_id IS NOT \
                  (SELECT id FROM messages WHERE thread_id = tm.thread_id AND mailbox_id = tm.mailbox_id \
                   ORDER BY date DESC, id DESC LIMIT 1)",
        ),
        (
            "missing conversation rows",
            "SELECT count(*) FROM (SELECT DISTINCT thread_id, mailbox_id FROM messages) c \
             WHERE NOT EXISTS (SELECT 1 FROM thread_mailboxes tm \
               WHERE tm.thread_id = c.thread_id AND tm.mailbox_id = c.mailbox_id)",
        ),
        (
            "message-id aliases",
            "SELECT count(*) FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id \
             JOIN thread_ids a ON a.account_id = b.account_id AND a.message_id = m.message_id \
             WHERE a.thread_id <> m.thread_id",
        ),
    ];
    let mut problems = Vec::new();
    for (name, sql) in checks {
        let bad: i64 = conn.query_row(sql, [], |row| row.get(0))?;
        if bad != 0 {
            problems.push(format!("{name}: {bad} rows disagree"));
        }
    }
    Ok(problems)
}

pub struct Reader {
    conn: Connection,
}

impl Reader {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            conn: schema::open_reader(path)?,
        })
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Count, section bounds and the first page, from one snapshot.
    pub fn open_view(&self, view: View, thresholds: &Thresholds, limit: u32) -> Result<Snapshot> {
        let tx = self.conn.unchecked_transaction()?;
        let version = store_version(&tx)?;
        let total = view_total(&tx, view)?;
        let mut newer = [0u32; 4];
        for (slot, start) in newer.iter_mut().zip(thresholds.starts) {
            *slot = match view {
                View::All => count(
                    &tx,
                    "SELECT count(*) FROM messages WHERE date >= ?1",
                    [start],
                )?,
                View::Mailbox(mailbox) => count(
                    &tx,
                    "SELECT count(*) FROM messages WHERE mailbox_id = ?1 AND date >= ?2",
                    [mailbox, start],
                )?,
            };
        }
        let rows = page_rows(&tx, view, total, 0, limit)?;
        Ok(Snapshot {
            version,
            total,
            bounds: Bounds::from_counts(newer, total),
            rows,
        })
    }

    pub fn page(&self, view: View, offset: u32, limit: u32) -> Result<Page> {
        let tx = self.conn.unchecked_transaction()?;
        let version = store_version(&tx)?;
        let total = view_total(&tx, view)?;
        let rows = page_rows(&tx, view, total, offset, limit)?;
        Ok(Page {
            version,
            offset,
            rows,
        })
    }

    /// Like `page`, but always walking from the newest row (no reverse trick), for comparison.
    pub fn page_forward(&self, view: View, offset: u32, limit: u32) -> Result<Vec<ListRow>> {
        query_page(&self.conn, view, false, offset, limit)
    }

    pub fn search(
        &self,
        query: &str,
        filter: SearchFilter,
        limit: u32,
        plan: SearchPlan,
    ) -> Result<Vec<ListRow>> {
        let mut statement = self.conn.prepare_cached(&search_sql(filter, plan))?;
        let rows = match filter.mailbox {
            Some(mailbox) => statement
                .query_map(params![query, limit, mailbox], list_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            None => statement
                .query_map(params![query, limit], list_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        };
        Ok(rows)
    }

    pub fn search_count(&self, query: &str) -> Result<u32> {
        count(
            &self.conn,
            "SELECT count(*) FROM message_fts WHERE message_fts MATCH ?1",
            [query],
        )
    }

    pub fn conversations(
        &self,
        mailbox: i64,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<ConversationRow>> {
        Ok(self
            .conn
            .prepare_cached(CONVERSATIONS_SQL)?
            .query_map(params![mailbox, limit, offset], |row| {
                Ok(ConversationRow {
                    thread_id: row.get(0)?,
                    last_date: row.get(1)?,
                    subject: row.get(2)?,
                    participants: row.get(3)?,
                    message_count: row.get(4)?,
                    unread_count: row.get(5)?,
                    from_name: row.get(6)?,
                    snippet: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn mailboxes(&self) -> Result<Vec<MailboxInfo>> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT b.id, b.name, b.role, b.total_count, b.unread_count, \
                   coalesce((SELECT max(uid) FROM messages WHERE mailbox_id = b.id), 0) + 1 \
                 FROM mailboxes b ORDER BY b.id",
            )?
            .query_map([], |row| {
                Ok(MailboxInfo {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    role: row.get(2)?,
                    total: row.get(3)?,
                    unread: row.get(4)?,
                    next_uid: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn thread_messages(&self, thread: i64) -> Result<Vec<ListRow>> {
        Ok(self
            .conn
            .prepare_cached(&format!(
                "SELECT {ROW_COLUMNS} FROM messages m JOIN threads t ON t.id = m.thread_id \
                 WHERE m.thread_id = ?1 ORDER BY m.date, m.id"
            ))?
            .query_map([thread], list_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

fn view_total(conn: &Connection, view: View) -> Result<u32> {
    match view {
        View::All => count(
            conn,
            "SELECT coalesce(sum(total_count), 0) FROM mailboxes",
            [],
        ),
        View::Mailbox(mailbox) => count(
            conn,
            "SELECT total_count FROM mailboxes WHERE id = ?1",
            [mailbox],
        ),
    }
}

/// Rows `offset..offset + limit` of a view. Deep pages walk the index from the oldest end, so
/// no page costs more than half the view.
fn page_rows(
    conn: &Connection,
    view: View,
    total: u32,
    offset: u32,
    limit: u32,
) -> Result<Vec<ListRow>> {
    if offset >= total {
        return Ok(Vec::new());
    }
    let limit = limit.min(total - offset);
    if offset > total / 2 {
        let mut rows = query_page(conn, view, true, total - offset - limit, limit)?;
        rows.reverse();
        Ok(rows)
    } else {
        query_page(conn, view, false, offset, limit)
    }
}

/// Parameters: `?1` limit, `?2` offset, `?3` mailbox (mailbox views only).
pub fn page_sql(view: View, oldest_first: bool) -> String {
    let order = if oldest_first { "ASC" } else { "DESC" };
    let filter = match view {
        View::All => "",
        View::Mailbox(_) => "WHERE mailbox_id = ?3",
    };
    format!(
        "SELECT {ROW_COLUMNS} FROM \
           (SELECT id FROM messages {filter} ORDER BY date {order}, id {order} LIMIT ?1 OFFSET ?2) AS p \
         JOIN messages m ON m.id = p.id JOIN threads t ON t.id = m.thread_id \
         ORDER BY m.date {order}, m.id {order}"
    )
}

/// Parameters: `?1` FTS5 query, `?2` limit, `?3` mailbox (when filtered).
pub fn search_sql(filter: SearchFilter, plan: SearchPlan) -> String {
    let mut conditions = String::new();
    if filter.mailbox.is_some() {
        conditions.push_str(" AND m.mailbox_id = ?3");
    }
    if filter.unread_only {
        conditions.push_str(" AND m.flags & 1 = 0");
    }
    let inner = match plan {
        SearchPlan::FtsFirst => format!(
            "SELECT m.id AS id FROM message_fts f CROSS JOIN messages m ON m.id = f.rowid \
             WHERE message_fts MATCH ?1{conditions} ORDER BY m.date DESC, m.id DESC LIMIT ?2"
        ),
        SearchPlan::DateFirst => format!(
            "SELECT m.id AS id FROM messages m INDEXED BY messages_date \
             WHERE m.id IN (SELECT rowid FROM message_fts WHERE message_fts MATCH ?1){conditions} \
             ORDER BY m.date DESC, m.id DESC LIMIT ?2"
        ),
    };
    format!(
        "SELECT {ROW_COLUMNS} FROM ({inner}) AS p JOIN messages m ON m.id = p.id \
         JOIN threads t ON t.id = m.thread_id ORDER BY m.date DESC, m.id DESC"
    )
}

/// Parameters: `?1` mailbox, `?2` limit, `?3` offset.
pub const CONVERSATIONS_SQL: &str = "SELECT t.id, p.last_date, t.subject, t.participants, \
       t.message_count, t.unread_count, m.from_name, m.snippet \
     FROM (SELECT thread_id, last_date FROM thread_mailboxes WHERE mailbox_id = ?1 \
           ORDER BY last_date DESC, thread_id DESC LIMIT ?2 OFFSET ?3) AS p \
     JOIN thread_mailboxes tm ON tm.thread_id = p.thread_id AND tm.mailbox_id = ?1 \
     JOIN threads t ON t.id = p.thread_id \
     JOIN messages m ON m.id = tm.latest_id \
     ORDER BY p.last_date DESC, p.thread_id DESC";

fn query_page(
    conn: &Connection,
    view: View,
    oldest_first: bool,
    skip: u32,
    limit: u32,
) -> Result<Vec<ListRow>> {
    let mut statement = conn.prepare_cached(&page_sql(view, oldest_first))?;
    let rows = match view {
        View::All => statement.query_map(params![limit, skip], list_row)?,
        View::Mailbox(mailbox) => statement.query_map(params![limit, skip, mailbox], list_row)?,
    };
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subjects_lose_reply_prefixes() {
        assert_eq!(base_subject("Re: SV: Fwd: Møte"), "Møte");
        assert_eq!(base_subject("RE[2]: budget"), "budget");
        assert_eq!(base_subject("回复：会议安排"), "会议安排");
        assert_eq!(base_subject("Agenda: Tuesday"), "Agenda: Tuesday");
        assert_eq!(base_subject("Re:"), "");
    }

    #[test]
    fn participants_are_unique_and_bounded() {
        let mut list = String::new();
        for name in [
            "Ola", "Kari", "Ola", "Per", "Liv", "Siv", "Tor", "Åse", "Pål", "Eli",
        ] {
            list = add_participant(&list, name).into_owned();
        }
        let names: Vec<&str> = list.split(PARTICIPANT_SEPARATOR).collect();
        assert_eq!(names.len(), MAX_PARTICIPANTS);
        assert_eq!(names[..3], ["Ola", "Kari", "Per"]);
    }
}
