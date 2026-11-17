//! Group by Sender and Group by Domain (issue #24, ADR-023): a view's messages as one row per
//! sender address or per registrable domain, newest first, with the view's filter applied. In
//! the Inboxes, pinned groups come first, as the Pinned section does in the list (issue #26): a
//! domain's group is pinned by the domain or by a sender writing from it.
//!
//! Group by Conversation (issue #4): one row per conversation, from its newest message in the
//! view. The view's filter chooses the conversations (those with an unread, starred or attached
//! message here), but a row counts, and a command takes, all of the conversation's messages
//! here, so a command never splits a conversation.

use super::reader::ReadJob;
use super::{Reply, Store, StoreError, ViewFilter, ViewSpec, pins, views};
use rusqlite::{Connection, params_from_iter, types::Value};
use sealwax_domain::model::{Flags, FolderId, MessageId};
use sealwax_domain::senders::GroupBy;

/// The most groups one answer holds.
pub const MAX_GROUPS: u32 = 5_000;

/// One sender, one domain, or one conversation of a view. Its `Debug` prints counts only.
#[derive(Clone, PartialEq, Eq)]
pub struct SenderGroup {
    /// What `sender:` or `domain:` takes to find the group's mail: the address in lower case,
    /// or the registrable domain; a conversation's thread.
    pub key: String,
    /// The newest message's sender name, or its address when it has none.
    pub name: String,
    /// The group's messages in the view; a conversation's size, each Message-ID once, as the
    /// list's rows show it.
    pub count: u32,
    pub unread: u32,
    /// The newest message's date and subject.
    pub latest: i64,
    pub subject: String,
    /// The newest message, its sender's address and its preview.
    pub newest: MessageId,
    pub address: String,
    pub snippet: String,
    /// Some message of the group is starred; has attachments.
    pub starred: bool,
    pub attachments: bool,
    /// A pin names the group: its address or its domain; in Group by Domain its domain, or a
    /// pinned sender has mail in it.
    pub pinned: bool,
    /// The store put the group first, among the pinned ones (issue #26): in an Inbox or All
    /// Inboxes, an *always* pin's group, or an *until read* pin's while it has unread mail.
    pub leads: bool,
}

impl std::fmt::Debug for SenderGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SenderGroup")
            .field("count", &self.count)
            .field("unread", &self.unread)
            .field("latest", &self.latest)
            .field("newest", &self.newest)
            .field("starred", &self.starred)
            .field("attachments", &self.attachments)
            .field("pinned", &self.pinned)
            .field("leads", &self.leads)
            .finish_non_exhaustive()
    }
}

/// A group's messages in a view, for an action on all of them (issue #25). Its `Debug` prints
/// counts only.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct GroupMail {
    pub messages: Vec<GroupMessage>,
    /// The newest of them with a List-Unsubscribe header.
    pub subscription: Option<MessageId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupMessage {
    pub id: MessageId,
    pub folder: FolderId,
    pub flags: Flags,
}

impl std::fmt::Debug for GroupMail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupMail")
            .field("messages", &self.messages.len())
            .field("subscription", &self.subscription)
            .finish()
    }
}

impl Store {
    /// The view's messages grouped `by` sender or domain, newest group first, at most
    /// [`MAX_GROUPS`]. In an Inbox and in All Inboxes the pinned groups come first
    /// ([`SenderGroup::leads`]), newest first: an *always* pin's group, and an *until read* pin's while it has unread mail (a
    /// pinned sender's own unread mail, for its domain's group). `GroupBy::None` gives none.
    pub fn sender_groups(&self, spec: ViewSpec, by: GroupBy) -> Reply<Vec<SenderGroup>> {
        self.read(|reply| ReadJob::Groups { spec, by, reply })
    }

    /// The messages of the group `key` (a [`SenderGroup::key`]) in the view, newest first: what
    /// an action on the whole group acts on. `GroupBy::None` gives none.
    pub fn group_mail(&self, spec: ViewSpec, by: GroupBy, key: String) -> Reply<GroupMail> {
        self.read(|reply| ReadJob::GroupMail {
            spec,
            by,
            key,
            reply,
        })
    }
}

/// The mode of the pins that name a group (the higher wins: *always*), or NULL; on a row of the
/// group, whose key and domain are the group's.
fn pin_sql(by: GroupBy) -> &'static str {
    match by {
        GroupBy::Domain => {
            "(SELECT p.mode FROM pinned_senders p WHERE p.kind = 1 AND p.key = from_domain)"
        }
        GroupBy::None | GroupBy::Sender | GroupBy::Conversation => {
            "(SELECT max(p.mode) FROM pinned_senders p \
               WHERE (p.kind = 0 AND p.key = lower(trim(from_addr))) \
                  OR (p.kind = 1 AND p.key = from_domain AND from_domain <> ''))"
        }
    }
}

fn key_sql(by: GroupBy) -> Option<&'static str> {
    match by {
        GroupBy::None => None,
        GroupBy::Sender => Some("lower(trim(from_addr))"),
        GroupBy::Domain => Some("from_domain"),
        GroupBy::Conversation => Some("thread_id"),
    }
}

/// The view's folders as SQL's `IN` list, or `None` when it has none.
fn folder_list(conn: &Connection, spec: ViewSpec) -> Result<Option<String>, StoreError> {
    let folders = views::folders(conn, spec.scope)?;
    Ok((!folders.is_empty()).then(|| {
        folders
            .iter()
            .map(|folder| folder.0.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }))
}

/// The domains that pinned senders write from in a view: those with mail there, and those of
/// them whose pin leads (*always*, or *until read* while the sender has unread mail there).
#[derive(Default)]
struct SendersDomains {
    leading: Vec<String>,
    pinned: Vec<String>,
}

fn pinned_senders_domains(
    conn: &Connection,
    ids: &str,
    condition: &str,
) -> Result<SendersDomains, StoreError> {
    let pins = conn
        .prepare_cached("SELECT key, mode FROM pinned_senders WHERE kind = 0")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut domains = SendersDomains::default();
    if pins.is_empty() {
        return Ok(domains);
    }
    let mut statement = conn.prepare(&format!(
        "SELECT from_domain, max(flags & 1 = 0) FROM messages INDEXED BY messages_sender \
         WHERE lower(trim(from_addr)) = ?1 AND folder_id IN ({ids}) AND flags & 264 = 0\
           {condition} AND from_domain <> '' \
         GROUP BY from_domain"
    ))?;
    for (key, mode) in pins {
        for row in statement.query_map([&key], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
        })? {
            let (domain, unread) = row?;
            if (mode == pins::mode_code(pins::PinMode::Keep) || unread)
                && !domains.leading.contains(&domain)
            {
                domains.leading.push(domain.clone());
            }
            if !domains.pinned.contains(&domain) {
                domains.pinned.push(domain);
            }
        }
    }
    Ok(domains)
}

pub(super) fn group_mail(
    conn: &Connection,
    spec: ViewSpec,
    by: GroupBy,
    key: &str,
) -> Result<GroupMail, StoreError> {
    let (Some(column), Some(ids)) = (key_sql(by), folder_list(conn, spec)?) else {
        return Ok(GroupMail::default());
    };
    if key.is_empty() {
        return Ok(GroupMail::default());
    }
    let sql = if by == GroupBy::Conversation {
        // The whole conversation here, whatever the filter: a command never splits it.
        format!(
            "SELECT id, folder_id, flags, list_unsubscribe IS NOT NULL \
             FROM messages INDEXED BY messages_thread \
             WHERE thread_id = CAST(?1 AS INTEGER) AND folder_id IN ({ids}) \
               AND flags & 264 = 0 \
             ORDER BY date DESC, id DESC"
        )
    } else {
        let (index, condition) = views::filter_sql(spec.filter);
        format!(
            "SELECT id, folder_id, flags, list_unsubscribe IS NOT NULL \
             FROM messages INDEXED BY {index} \
             WHERE folder_id IN ({ids}){condition} AND {column} = ?1 \
             ORDER BY date DESC, id DESC"
        )
    };
    let mut mail = GroupMail::default();
    let mut statement = conn.prepare_cached(&sql)?;
    let mut rows = statement.query([key])?;
    while let Some(row) = rows.next()? {
        let id = MessageId(row.get(0)?);
        if mail.subscription.is_none() && row.get::<_, bool>(3)? {
            mail.subscription = Some(id);
        }
        mail.messages.push(GroupMessage {
            id,
            folder: FolderId(row.get(1)?),
            flags: Flags::from_bits(row.get::<_, u32>(2)?),
        });
    }
    Ok(mail)
}

pub(super) fn groups(
    conn: &Connection,
    spec: ViewSpec,
    by: GroupBy,
) -> Result<Vec<SenderGroup>, StoreError> {
    let (Some(key), Some(ids)) = (key_sql(by), folder_list(conn, spec)?) else {
        return Ok(Vec::new());
    };
    if by == GroupBy::Conversation {
        return conversations(conn, spec, &ids);
    }
    let (index, condition) = views::filter_sql(spec.filter);
    let pin = pin_sql(by);
    let pinnable = pins::pinnable(conn, spec.scope)?;
    let senders = match by {
        GroupBy::Domain => pinned_senders_domains(conn, &ids, condition)?,
        GroupBy::None | GroupBy::Sender | GroupBy::Conversation => SendersDomains::default(),
    };
    let list = |from: usize, count: usize| {
        (from..from + count)
            .map(|at| format!("?{at}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let leading = list(2, senders.leading.len());
    // SQLite takes the bare columns of an aggregate with max() from the row holding the max.
    let sql = format!(
        "SELECT k, n, unread, latest, from_name, from_addr, subject, pin, \
           ?1 AND (pin = 1 OR (pin = 0 AND unread > 0) OR k IN ({leading})) AS leads, \
           id, snippet, starred, attachments \
         FROM (SELECT {key} AS k, count(*) AS n, sum(flags & 1 = 0) AS unread, \
                 max(date) AS latest, from_name, from_addr, subject, \
                 coalesce({pin}, -1) AS pin, id, snippet, \
                 sum(flags & 4 <> 0) > 0 AS starred, sum(has_attachments <> 0) > 0 AS attachments \
               FROM messages INDEXED BY {index} \
               WHERE folder_id IN ({ids}){condition} AND {key} <> '' \
               GROUP BY k) \
         ORDER BY leads DESC, latest DESC, k \
         LIMIT {MAX_GROUPS}"
    );
    let values: Vec<Value> = std::iter::once(Value::from(pinnable))
        .chain(senders.leading.iter().cloned().map(Value::from))
        .collect();
    let groups = conn
        .prepare(&sql)?
        .query_map(params_from_iter(values), |row| {
            let key: String = row.get(0)?;
            let mut group = group_row(row, key)?;
            group.pinned = group.pinned || senders.pinned.contains(&group.key);
            Ok(group)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(groups)
}

/// A group from a row of `groups` or `conversations` (its key taken already): count, unread,
/// latest, name, address, subject, pin, leads, newest id, snippet, starred, attachments.
fn group_row(row: &rusqlite::Row<'_>, key: String) -> rusqlite::Result<SenderGroup> {
    let name: String = row.get(4)?;
    let address: String = row.get(5)?;
    Ok(SenderGroup {
        key,
        count: row.get(1)?,
        unread: row.get(2)?,
        latest: row.get(3)?,
        name: if name.trim().is_empty() {
            address.clone()
        } else {
            name
        },
        address,
        subject: row.get(6)?,
        pinned: row.get::<_, i64>(7)? >= 0,
        leads: row.get(8)?,
        newest: MessageId(row.get(9)?),
        snippet: row.get(10)?,
        starred: row.get(11)?,
        attachments: row.get(12)?,
    })
}

/// Group by Conversation (issue #4): one row per conversation with a message in the view that
/// passes its filter, from its newest message here; newest first, the pinned ones first in the
/// Inboxes (a conversation is pinned by its newest message's sender or domain). The count is
/// the conversation's size, as the list's rows show it; the unread, starred and attachment
/// marks are its messages' here.
fn conversations(
    conn: &Connection,
    spec: ViewSpec,
    ids: &str,
) -> Result<Vec<SenderGroup>, StoreError> {
    let pin = pin_sql(GroupBy::Conversation);
    let pinnable = pins::pinnable(conn, spec.scope)?;
    let having = match spec.filter {
        ViewFilter::All => "",
        ViewFilter::Unread => " HAVING unread > 0",
        ViewFilter::Starred => " HAVING starred",
        ViewFilter::Attachments => " HAVING attachments",
    };
    // SQLite takes the bare columns of an aggregate with max() from the row holding the max.
    let sql = format!(
        "SELECT k, max(n, coalesce((SELECT message_count FROM threads WHERE id = k), 0)), \
           unread, latest, from_name, from_addr, subject, pin, \
           ?1 AND (pin = 1 OR (pin = 0 AND unread > 0)) AS leads, \
           id, snippet, starred, attachments \
         FROM (SELECT thread_id AS k, count(*) AS n, sum(flags & 1 = 0) AS unread, \
                 max(date) AS latest, from_name, from_addr, subject, \
                 coalesce({pin}, -1) AS pin, id, snippet, \
                 sum(flags & 4 <> 0) > 0 AS starred, sum(has_attachments <> 0) > 0 AS attachments \
               FROM messages INDEXED BY messages_folder_date \
               WHERE folder_id IN ({ids}) AND flags & 264 = 0 \
               GROUP BY k{having}) \
         ORDER BY leads DESC, latest DESC, id DESC \
         LIMIT {MAX_GROUPS}"
    );
    let groups = conn
        .prepare(&sql)?
        .query_map([pinnable], |row| {
            let key: i64 = row.get(0)?;
            group_row(row, key.to_string())
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(groups)
}
