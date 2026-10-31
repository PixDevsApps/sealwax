//! Pinned senders (issue #26, ADR-024): an address or a registrable domain whose mail is listed
//! first in the Inboxes and in All Inboxes, while unread or for as long as it is there. A pin
//! belongs to no account. The newest [`PINNED_MAX`] such messages of a view are its Pinned
//! section, ahead of the dates; the rest stay in their dates.

use super::reader::ReadJob;
use super::views;
use super::writer::WriteJob;
use super::{Reply, Store, StoreError, ViewFilter, ViewScope};
use rusqlite::{Connection, OptionalExtension, params};
use sealwax_domain::model::{FolderId, MessageId};

/// The most messages a view's Pinned section holds; a pinned sender's older mail stays in its
/// date. Fewer than one screen of rows (14 at 1440 px), so the dates still show.
pub const PINNED_MAX: usize = 10;

/// What a pin names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PinKind {
    /// One address, as `sender:` takes it.
    Sender,
    /// A registrable domain, as `domain:` takes it.
    Domain,
}

impl PinKind {
    fn code(self) -> i64 {
        match self {
            PinKind::Sender => 0,
            PinKind::Domain => 1,
        }
    }

    fn from_code(code: i64) -> Self {
        if code == 1 {
            PinKind::Domain
        } else {
            PinKind::Sender
        }
    }

    /// The SQL of a message's key for this kind, on `messages m`.
    fn column(self) -> &'static str {
        match self {
            PinKind::Sender => "lower(trim(m.from_addr))",
            PinKind::Domain => "m.from_domain",
        }
    }
}

/// How long a pinned sender's message stays in the Pinned section.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PinMode {
    /// Until it is read, then it goes back to its date.
    #[default]
    UntilRead,
    /// For as long as it is in the Inbox, read or not.
    Keep,
}

impl PinMode {
    fn code(self) -> i64 {
        match self {
            PinMode::UntilRead => 0,
            PinMode::Keep => 1,
        }
    }

    fn from_code(code: i64) -> Self {
        if code == 1 {
            PinMode::Keep
        } else {
            PinMode::UntilRead
        }
    }
}

/// A pinned sender or domain. Its `Debug` leaves the key out.
#[derive(Clone, PartialEq, Eq)]
pub struct Pin {
    pub kind: PinKind,
    /// The address in lower case, or the registrable domain.
    pub key: String,
    pub mode: PinMode,
    /// When it was pinned, Unix seconds.
    pub added: i64,
}

impl std::fmt::Debug for Pin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pin")
            .field("kind", &self.kind)
            .field("mode", &self.mode)
            .field("added", &self.added)
            .finish_non_exhaustive()
    }
}

/// What to pin: a message's sender or domain, or a key as Group by gives it.
#[derive(Clone, PartialEq, Eq)]
pub enum PinTarget {
    Message(MessageId),
    Key(String),
}

impl std::fmt::Debug for PinTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PinTarget::Message(id) => f.debug_tuple("Message").field(id).finish(),
            PinTarget::Key(_) => f.write_str("Key(..)"),
        }
    }
}

impl Store {
    /// Every pin, senders first, each kind in key order.
    pub fn pins(&self) -> Reply<Vec<Pin>> {
        self.read(|reply| ReadJob::Pins { reply })
    }

    /// Pins the sender or the domain of `target` *until read*, at `at` (Unix seconds); one
    /// already pinned keeps its mode. Gives the pin, or `None` when the message is gone or has
    /// no such key. Every open view reloads.
    pub fn pin(&self, target: PinTarget, kind: PinKind, at: i64) -> Reply<Option<Pin>> {
        self.write(|reply| WriteJob::Pin {
            target,
            kind,
            at,
            reply,
        })
    }

    /// Unpins whatever pins `message`: its sender and its domain. Gives the pins taken off, so
    /// they can be put back ([`Store::restore_pins`]).
    pub fn unpin(&self, message: MessageId) -> Reply<Vec<Pin>> {
        self.write(|reply| WriteJob::Unpin { message, reply })
    }

    /// Puts `pins` back as they were (Undo of [`Store::unpin`]), or changes their mode.
    pub fn restore_pins(&self, pins: Vec<Pin>) -> Reply<()> {
        self.write(|reply| WriteJob::RestorePins { pins, reply })
    }

    /// Takes the pin `(kind, key)` off.
    pub fn remove_pin(&self, kind: PinKind, key: String) -> Reply<()> {
        self.write(|reply| WriteJob::RemovePin { kind, key, reply })
    }
}

/// Whether any pin exists.
fn any(conn: &Connection) -> rusqlite::Result<bool> {
    conn.prepare_cached("SELECT EXISTS(SELECT 1 FROM pinned_senders)")?
        .query_row([], |row| row.get(0))
}

/// Whether a view lists pinned mail first: All Inboxes and every Inbox.
fn pinnable(conn: &Connection, scope: ViewScope) -> rusqlite::Result<bool> {
    match scope {
        ViewScope::UnifiedInbox => Ok(true),
        ViewScope::Folder(folder) => Ok(conn
            .prepare_cached("SELECT role FROM folders WHERE id = ?1")?
            .query_row([folder.0], |row| row.get::<_, Option<String>>(0))
            .optional()?
            .flatten()
            .is_some_and(|role| role == "inbox")),
    }
}

/// A view's Pinned section: its newest pinned messages, `(id, date)` newest first, at most
/// [`PINNED_MAX`]. `None` when the view has no section to show (not an Inbox, or nothing is
/// pinned); the section can be empty otherwise. Each pin's newest rows in each folder are read
/// from its index alone, which stops at [`PINNED_MAX`]; the pins are few.
pub(crate) fn section(
    conn: &Connection,
    scope: ViewScope,
    folders: &[FolderId],
    filter: ViewFilter,
) -> rusqlite::Result<Option<Vec<(MessageId, i64)>>> {
    if folders.is_empty() || !any(conn)? || !pinnable(conn, scope)? {
        return Ok(None);
    }
    let (_, condition) = views::filter_sql(filter);
    let pins = conn
        .prepare_cached("SELECT kind, key, mode FROM pinned_senders")?
        .query_map([], |row| {
            Ok((
                PinKind::from_code(row.get(0)?),
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut rows: Vec<(MessageId, i64)> = Vec::new();
    for (kind, key, mode) in pins {
        let (index, column) = match kind {
            PinKind::Sender => ("messages_sender", "lower(trim(from_addr))"),
            PinKind::Domain => ("messages_domain", "from_domain"),
        };
        let mut statement = conn.prepare_cached(&format!(
            "SELECT id, date FROM messages INDEXED BY {index} \
             WHERE {column} = ?1 AND folder_id = ?2 AND flags & 264 = 0{condition} \
               AND (?3 = 1 OR flags & 1 = 0) \
             ORDER BY date DESC, id DESC LIMIT {PINNED_MAX}"
        ))?;
        for folder in folders {
            for row in statement.query_map(params![key, folder.0, mode], |row| {
                Ok((MessageId(row.get(0)?), row.get::<_, i64>(1)?))
            })? {
                rows.push(row?);
            }
        }
    }
    rows.sort_unstable_by_key(|row| std::cmp::Reverse((row.1, row.0.0)));
    rows.dedup_by_key(|row| row.0);
    rows.truncate(PINNED_MAX);
    Ok(Some(rows))
}

/// Whether a pin names the sender or the domain of `message` (false when it is gone).
pub(crate) fn matches(conn: &Connection, message: MessageId) -> rusqlite::Result<bool> {
    Ok(conn
        .prepare_cached(&format!(
            "SELECT {PINNED_SQL} FROM messages m WHERE m.id = ?1"
        ))?
        .query_row([message.0], |row| row.get(0))
        .optional()?
        .unwrap_or(false))
}

const PINNED_SQL: &str = views::pinned_sql!();

pub(super) fn list(conn: &Connection) -> Result<Vec<Pin>, StoreError> {
    let pins = conn
        .prepare_cached("SELECT kind, key, mode, added FROM pinned_senders ORDER BY kind, key")?
        .query_map([], |row| {
            Ok(Pin {
                kind: PinKind::from_code(row.get(0)?),
                key: row.get(1)?,
                mode: PinMode::from_code(row.get(2)?),
                added: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(pins)
}

/// The key `target` gives for `kind`; `None` when the message is gone or the key is empty.
pub(crate) fn key(
    conn: &Connection,
    target: &PinTarget,
    kind: PinKind,
) -> rusqlite::Result<Option<String>> {
    let key = match target {
        PinTarget::Key(key) => Some(match kind {
            PinKind::Sender => key.trim().to_lowercase(),
            PinKind::Domain => key.trim().to_owned(),
        }),
        PinTarget::Message(id) => conn
            .prepare_cached(&format!(
                "SELECT {} FROM messages m WHERE m.id = ?1",
                kind.column()
            ))?
            .query_row([id.0], |row| row.get::<_, String>(0))
            .optional()?,
    };
    Ok(key.filter(|key| !key.is_empty()))
}

/// The pins that name the sender or the domain of `message`.
pub(crate) fn of_message(conn: &Connection, message: MessageId) -> rusqlite::Result<Vec<Pin>> {
    conn.prepare_cached(
        "SELECT p.kind, p.key, p.mode, p.added FROM messages m JOIN pinned_senders p \
           ON (p.kind = 0 AND p.key = lower(trim(m.from_addr))) \
           OR (p.kind = 1 AND p.key = m.from_domain AND m.from_domain <> '') \
         WHERE m.id = ?1 ORDER BY p.kind",
    )?
    .query_map([message.0], |row| {
        Ok(Pin {
            kind: PinKind::from_code(row.get(0)?),
            key: row.get(1)?,
            mode: PinMode::from_code(row.get(2)?),
            added: row.get(3)?,
        })
    })?
    .collect()
}

pub(crate) fn kind_code(kind: PinKind) -> i64 {
    kind.code()
}

pub(crate) fn mode_code(mode: PinMode) -> i64 {
    mode.code()
}
