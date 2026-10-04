//! Prototype store for S1: the sync state and header columns the sync needs, so the lead can
//! reconcile them with S3's schema. Not the M1 schema.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Duration;

use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

pub const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE mailboxes(
    id INTEGER PRIMARY KEY,
    name_raw TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    delimiter TEXT,
    attributes TEXT NOT NULL DEFAULT '',
    role TEXT,
    selectable INTEGER NOT NULL DEFAULT 1,
    uidvalidity INTEGER,
    state TEXT NOT NULL DEFAULT 'new' CHECK (state IN ('new', 'initial', 'ready')),
    uidnext INTEGER NOT NULL DEFAULT 1,
    highestmodseq INTEGER NOT NULL DEFAULT 0,
    initial_top INTEGER,
    initial_modseq INTEGER,
    sync_floor INTEGER
);
CREATE TABLE messages(
    id INTEGER PRIMARY KEY,
    mailbox_id INTEGER NOT NULL REFERENCES mailboxes(id) ON DELETE CASCADE,
    uid INTEGER NOT NULL,
    modseq INTEGER,
    flags INTEGER NOT NULL,
    keywords TEXT NOT NULL DEFAULT '',
    internal_date INTEGER,
    size INTEGER,
    date INTEGER,
    subject TEXT,
    from_name TEXT,
    from_addr TEXT,
    to_addrs TEXT NOT NULL DEFAULT '',
    cc_addrs TEXT NOT NULL DEFAULT '',
    message_id TEXT,
    in_reply_to TEXT,
    refs TEXT,
    list_unsubscribe TEXT,
    has_attachments INTEGER NOT NULL DEFAULT 0,
    UNIQUE(mailbox_id, uid)
);
CREATE INDEX messages_message_id ON messages(message_id);
CREATE INDEX messages_mailbox_date ON messages(mailbox_id, internal_date);
";

pub const SEEN: u32 = 1;
pub const ANSWERED: u32 = 2;
pub const FLAGGED: u32 = 4;
pub const DELETED: u32 = 8;
pub const DRAFT: u32 = 16;

/// Splits IMAP flags into the system-flag bitset and the sorted keyword list. `\Recent` is
/// session state and is never stored.
pub fn split_flags<'a>(flags: impl IntoIterator<Item = &'a str>) -> (u32, String) {
    let mut bits = 0;
    let mut keywords = Vec::new();
    for flag in flags {
        match flag.to_ascii_lowercase().as_str() {
            "\\seen" => bits |= SEEN,
            "\\answered" => bits |= ANSWERED,
            "\\flagged" => bits |= FLAGGED,
            "\\deleted" => bits |= DELETED,
            "\\draft" => bits |= DRAFT,
            "\\recent" => {}
            _ => keywords.push(flag.to_owned()),
        }
    }
    keywords.sort();
    keywords.dedup();
    (bits, keywords.join(" "))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MailboxState {
    New,
    Initial { top: u32, modseq: u64, floor: u32 },
    Ready,
}

#[derive(Clone, Debug)]
pub struct MailboxRow {
    pub id: i64,
    pub name_raw: String,
    pub name: String,
    pub delimiter: Option<String>,
    pub role: Option<String>,
    pub selectable: bool,
    pub uidvalidity: Option<u32>,
    pub state: MailboxState,
    pub uidnext: u32,
    pub highestmodseq: u64,
}

#[derive(Clone, Debug)]
pub struct ListedMailbox {
    pub name_raw: String,
    pub name: String,
    pub delimiter: Option<String>,
    pub attributes: Vec<String>,
    pub role: Option<String>,
    pub selectable: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Header {
    pub uid: u32,
    pub modseq: Option<u64>,
    pub flags: u32,
    pub keywords: String,
    pub internal_date: Option<i64>,
    pub size: Option<u32>,
    pub date: Option<i64>,
    pub subject: Option<String>,
    pub from_name: Option<String>,
    pub from_addr: Option<String>,
    pub to_addrs: String,
    pub cc_addrs: String,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub list_unsubscribe: Option<String>,
    pub has_attachments: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlagUpdate {
    pub uid: u32,
    pub flags: u32,
    pub keywords: String,
    pub modseq: Option<u64>,
}

/// One transaction's worth of changes for a mailbox, with the state it leaves behind.
#[derive(Debug, Default)]
pub struct Batch {
    pub upserts: Vec<Header>,
    pub flags: Vec<FlagUpdate>,
    pub deletes: Vec<u32>,
    pub uidnext: Option<u32>,
    pub highestmodseq: Option<u64>,
    pub floor: Option<u32>,
    pub finish_initial: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderSnapshot {
    pub uidvalidity: Option<u32>,
    /// uid -> (flags, keywords, message-id)
    pub messages: BTreeMap<u32, (u32, String, Option<String>)>,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Store> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Store> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;",
        )?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            bail!("the store was created by a newer schema ({version})");
        }
        if version == 0 {
            let tx = conn.transaction()?;
            tx.execute_batch(SCHEMA)?;
            tx.execute_batch(&format!("PRAGMA user_version={SCHEMA_VERSION}"))?;
            tx.commit()?;
        }
        Ok(Store { conn })
    }

    fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<MailboxRow> {
        let state: String = r.get(7)?;
        let state = match state.as_str() {
            "initial" => MailboxState::Initial {
                top: r.get::<_, Option<u32>>(10)?.unwrap_or(1),
                modseq: r.get::<_, Option<i64>>(11)?.unwrap_or(0) as u64,
                floor: r.get::<_, Option<u32>>(12)?.unwrap_or(1),
            },
            "ready" => MailboxState::Ready,
            _ => MailboxState::New,
        };
        Ok(MailboxRow {
            id: r.get(0)?,
            name_raw: r.get(1)?,
            name: r.get(2)?,
            delimiter: r.get(3)?,
            role: r.get(4)?,
            selectable: r.get(5)?,
            uidvalidity: r.get(6)?,
            state,
            uidnext: r.get(8)?,
            highestmodseq: r.get::<_, i64>(9)? as u64,
        })
    }

    const MAILBOX_COLUMNS: &str = "id, name_raw, name, delimiter, role, selectable, uidvalidity, state, \
         uidnext, highestmodseq, initial_top, initial_modseq, sync_floor";

    pub fn mailboxes(&self) -> Result<Vec<MailboxRow>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {} FROM mailboxes ORDER BY id",
            Self::MAILBOX_COLUMNS
        ))?;
        Ok(stmt
            .query_map([], Self::row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn mailbox(&self, id: i64) -> Result<Option<MailboxRow>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {} FROM mailboxes WHERE id = ?1",
            Self::MAILBOX_COLUMNS
        ))?;
        Ok(stmt.query_row([id], Self::row).optional()?)
    }

    pub fn mailbox_by_raw(&self, name_raw: &str) -> Result<Option<MailboxRow>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {} FROM mailboxes WHERE name_raw = ?1",
            Self::MAILBOX_COLUMNS
        ))?;
        Ok(stmt.query_row([name_raw], Self::row).optional()?)
    }

    /// Makes the mailbox table match a LIST result. Returns how many mailboxes were removed.
    pub fn apply_list(&mut self, listed: &[ListedMailbox]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut removed = 0;
        {
            let existing: Vec<String> = tx
                .prepare("SELECT name_raw FROM mailboxes")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for name_raw in existing {
                if !listed.iter().any(|l| l.name_raw == name_raw) {
                    removed +=
                        tx.execute("DELETE FROM mailboxes WHERE name_raw = ?1", [&name_raw])?;
                }
            }
            let mut upsert = tx.prepare(
                "INSERT INTO mailboxes(name_raw, name, delimiter, attributes, role, selectable)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(name_raw) DO UPDATE SET name = excluded.name, delimiter = excluded.delimiter,
                     attributes = excluded.attributes, role = excluded.role, selectable = excluded.selectable",
            )?;
            for l in listed {
                upsert.execute(params![
                    l.name_raw,
                    l.name,
                    l.delimiter,
                    l.attributes.join(" "),
                    l.role,
                    l.selectable
                ])?;
            }
        }
        tx.commit()?;
        Ok(removed)
    }

    /// A UIDVALIDITY change (or the first EXAMINE): drop every cached message of the mailbox.
    pub fn reset_mailbox(&mut self, id: i64, uidvalidity: u32) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM messages WHERE mailbox_id = ?1", [id])?;
        tx.execute(
            "UPDATE mailboxes SET uidvalidity = ?2, state = 'new', uidnext = 1, highestmodseq = 0,
                 initial_top = NULL, initial_modseq = NULL, sync_floor = NULL WHERE id = ?1",
            params![id, uidvalidity],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn begin_initial(&mut self, id: i64, top: u32, modseq: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE mailboxes SET state = 'initial', initial_top = ?2, initial_modseq = ?3, sync_floor = ?2
             WHERE id = ?1",
            params![id, top, modseq as i64],
        )?;
        Ok(())
    }

    pub fn count(&self, id: i64) -> Result<u32> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT COUNT(*) FROM messages WHERE mailbox_id = ?1")?;
        Ok(stmt.query_row([id], |r| r.get(0))?)
    }

    pub fn uids(&self, id: i64) -> Result<Vec<u32>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT uid FROM messages WHERE mailbox_id = ?1 ORDER BY uid")?;
        Ok(stmt
            .query_map([id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn flag_map(&self, id: i64) -> Result<HashMap<u32, (u32, String)>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT uid, flags, keywords FROM messages WHERE mailbox_id = ?1")?;
        let rows = stmt.query_map([id], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn has_message_id(&self, message_id: &str) -> Result<bool> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT 1 FROM messages WHERE message_id = ?1 LIMIT 1")?;
        Ok(stmt.exists([message_id])?)
    }

    /// Applies one batch in a single transaction.
    pub fn commit(&mut self, id: i64, batch: &Batch) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut upsert = tx.prepare_cached(
                "INSERT INTO messages(mailbox_id, uid, modseq, flags, keywords, internal_date, size, date, subject,
                     from_name, from_addr, to_addrs, cc_addrs, message_id, in_reply_to, refs, list_unsubscribe,
                     has_attachments)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
                 ON CONFLICT(mailbox_id, uid) DO UPDATE SET modseq = excluded.modseq, flags = excluded.flags,
                     keywords = excluded.keywords",
            )?;
            for h in &batch.upserts {
                upsert.execute(params![
                    id,
                    h.uid,
                    h.modseq.map(|m| m as i64),
                    h.flags,
                    h.keywords,
                    h.internal_date,
                    h.size,
                    h.date,
                    h.subject,
                    h.from_name,
                    h.from_addr,
                    h.to_addrs,
                    h.cc_addrs,
                    h.message_id,
                    h.in_reply_to,
                    h.references,
                    h.list_unsubscribe,
                    h.has_attachments,
                ])?;
            }
            let mut flags = tx.prepare_cached(
                "UPDATE messages SET flags = ?3, keywords = ?4, modseq = COALESCE(?5, modseq)
                 WHERE mailbox_id = ?1 AND uid = ?2",
            )?;
            for f in &batch.flags {
                flags.execute(params![
                    id,
                    f.uid,
                    f.flags,
                    f.keywords,
                    f.modseq.map(|m| m as i64)
                ])?;
            }
            let mut delete =
                tx.prepare_cached("DELETE FROM messages WHERE mailbox_id = ?1 AND uid = ?2")?;
            for uid in &batch.deletes {
                delete.execute(params![id, uid])?;
            }
            if let Some(floor) = batch.floor {
                tx.execute(
                    "UPDATE mailboxes SET sync_floor = ?2 WHERE id = ?1",
                    params![id, floor],
                )?;
            }
            if batch.finish_initial {
                tx.execute(
                    "UPDATE mailboxes SET state = 'ready', uidnext = initial_top, highestmodseq = initial_modseq,
                         initial_top = NULL, initial_modseq = NULL, sync_floor = NULL WHERE id = ?1",
                    [id],
                )?;
            }
            if let Some(uidnext) = batch.uidnext {
                tx.execute(
                    "UPDATE mailboxes SET uidnext = MAX(uidnext, ?2) WHERE id = ?1",
                    params![id, uidnext],
                )?;
            }
            if let Some(modseq) = batch.highestmodseq {
                tx.execute(
                    "UPDATE mailboxes SET highestmodseq = ?2 WHERE id = ?1",
                    params![id, modseq as i64],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Every selectable mailbox keyed by decoded name, for comparisons with the server.
    pub fn snapshot(&self) -> Result<BTreeMap<String, FolderSnapshot>> {
        let mut out = BTreeMap::new();
        for mailbox in self.mailboxes()?.into_iter().filter(|m| m.selectable) {
            let mut stmt = self.conn.prepare_cached(
                "SELECT uid, flags, keywords, message_id FROM messages WHERE mailbox_id = ?1",
            )?;
            let messages = stmt
                .query_map([mailbox.id], |r| {
                    Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
                })?
                .collect::<rusqlite::Result<_>>()?;
            out.insert(
                mailbox.name,
                FolderSnapshot {
                    uidvalidity: mailbox.uidvalidity,
                    messages,
                },
            );
        }
        Ok(out)
    }

    pub fn total_messages(&self) -> Result<u64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_, i64>(0))?
            as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(name: &str) -> ListedMailbox {
        ListedMailbox {
            name_raw: name.into(),
            name: name.into(),
            delimiter: Some("/".into()),
            attributes: vec![],
            role: None,
            selectable: true,
        }
    }

    fn header(uid: u32) -> Header {
        Header {
            uid,
            flags: SEEN,
            message_id: Some(format!("m{uid}@example.test")),
            ..Header::default()
        }
    }

    #[test]
    fn flags_split_into_bits_and_keywords() {
        assert_eq!(
            split_flags(["\\Seen", "\\Recent", "$Label1", "\\Flagged", "Junk"]),
            (SEEN | FLAGGED, "$Label1 Junk".into())
        );
        assert_eq!(
            split_flags(["\\SEEN", "\\draft"]),
            (SEEN | DRAFT, String::new())
        );
    }

    #[test]
    fn initial_sync_state_machine_and_reset() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .apply_list(&[listed("INBOX"), listed("Sendt")])
            .unwrap();
        let inbox = store.mailbox_by_raw("INBOX").unwrap().unwrap();
        assert_eq!(inbox.state, MailboxState::New);
        store.reset_mailbox(inbox.id, 7).unwrap();
        store.begin_initial(inbox.id, 11, 40).unwrap();
        store
            .commit(
                inbox.id,
                &Batch {
                    upserts: (6..=10).map(header).collect(),
                    floor: Some(6),
                    ..Batch::default()
                },
            )
            .unwrap();
        let row = store.mailbox(inbox.id).unwrap().unwrap();
        assert_eq!(
            row.state,
            MailboxState::Initial {
                top: 11,
                modseq: 40,
                floor: 6
            }
        );
        store
            .commit(
                inbox.id,
                &Batch {
                    upserts: (1..=5).map(header).collect(),
                    finish_initial: true,
                    ..Batch::default()
                },
            )
            .unwrap();
        let row = store.mailbox(inbox.id).unwrap().unwrap();
        assert_eq!(
            (row.state, row.uidnext, row.highestmodseq),
            (MailboxState::Ready, 11, 40)
        );
        assert_eq!(store.count(inbox.id).unwrap(), 10);

        store.reset_mailbox(inbox.id, 8).unwrap();
        let row = store.mailbox(inbox.id).unwrap().unwrap();
        assert_eq!(
            (row.state, row.uidnext, row.uidvalidity),
            (MailboxState::New, 1, Some(8))
        );
        assert_eq!(store.count(inbox.id).unwrap(), 0);
    }

    #[test]
    fn list_removal_cascades_to_messages() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .apply_list(&[listed("INBOX"), listed("Gammel")])
            .unwrap();
        let old = store.mailbox_by_raw("Gammel").unwrap().unwrap();
        store
            .commit(
                old.id,
                &Batch {
                    upserts: vec![header(1)],
                    ..Batch::default()
                },
            )
            .unwrap();
        assert_eq!(store.apply_list(&[listed("INBOX")]).unwrap(), 1);
        assert_eq!(store.total_messages().unwrap(), 0);
    }

    #[test]
    fn upsert_keeps_headers_and_updates_flags() {
        let mut store = Store::open_in_memory().unwrap();
        store.apply_list(&[listed("INBOX")]).unwrap();
        let id = store.mailbox_by_raw("INBOX").unwrap().unwrap().id;
        store
            .commit(
                id,
                &Batch {
                    upserts: vec![header(3)],
                    ..Batch::default()
                },
            )
            .unwrap();
        let update = FlagUpdate {
            uid: 3,
            flags: FLAGGED,
            keywords: "$Label1".into(),
            modseq: Some(9),
        };
        store
            .commit(
                id,
                &Batch {
                    flags: vec![update],
                    deletes: vec![99],
                    ..Batch::default()
                },
            )
            .unwrap();
        let snap = store.snapshot().unwrap();
        assert_eq!(
            snap["INBOX"].messages[&3],
            (FLAGGED, "$Label1".into(), Some("m3@example.test".into()))
        );
    }
}
