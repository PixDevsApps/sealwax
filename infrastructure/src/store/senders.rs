//! Group by Sender and Group by Domain (issue #24, ADR-023): a view's messages as one row per
//! sender address or per registrable domain, newest first, with the view's filter applied. In
//! the Inboxes, pinned groups come first, as the Pinned section does in the list (issue #26): a
//! domain's group is pinned by the domain or by a sender writing from it.

use super::reader::ReadJob;
use super::{Reply, Store, StoreError, ViewSpec, pins, views};
use rusqlite::{Connection, params_from_iter, types::Value};
use sealwax_domain::model::{Flags, FolderId, MessageId};
use sealwax_domain::senders::GroupBy;

/// The most groups one answer holds.
pub const MAX_GROUPS: u32 = 5_000;

/// One sender, or one domain, of a view. Its `Debug` prints counts only.
#[derive(Clone, PartialEq, Eq)]
pub struct SenderGroup {
    /// What `sender:` or `domain:` takes to find the group's mail: the address in lower case,
    /// or the registrable domain.
    pub key: String,
    /// The newest message's sender name, or its address when it has none.
    pub name: String,
    pub count: u32,
    pub unread: u32,
    /// The newest message's date and subject.
    pub latest: i64,
    pub subject: String,
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
        GroupBy::None | GroupBy::Sender => {
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
    let (index, condition) = views::filter_sql(spec.filter);
    let sql = format!(
        "SELECT id, folder_id, flags, list_unsubscribe IS NOT NULL \
         FROM messages INDEXED BY {index} \
         WHERE folder_id IN ({ids}){condition} AND {column} = ?1 \
         ORDER BY date DESC, id DESC"
    );
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
    let (index, condition) = views::filter_sql(spec.filter);
    let pin = pin_sql(by);
    let pinnable = pins::pinnable(conn, spec.scope)?;
    let senders = match by {
        GroupBy::Domain => pinned_senders_domains(conn, &ids, condition)?,
        GroupBy::None | GroupBy::Sender => SendersDomains::default(),
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
           ?1 AND (pin = 1 OR (pin = 0 AND unread > 0) OR k IN ({leading})) AS leads \
         FROM (SELECT {key} AS k, count(*) AS n, sum(flags & 1 = 0) AS unread, \
                 max(date) AS latest, from_name, from_addr, subject, \
                 coalesce({pin}, -1) AS pin \
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
            let name: String = row.get(4)?;
            let address: String = row.get(5)?;
            Ok(SenderGroup {
                pinned: row.get::<_, i64>(7)? >= 0 || senders.pinned.contains(&key),
                leads: row.get(8)?,
                key,
                count: row.get(1)?,
                unread: row.get(2)?,
                latest: row.get(3)?,
                name: if name.trim().is_empty() {
                    address
                } else {
                    name
                },
                subject: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(groups)
}
