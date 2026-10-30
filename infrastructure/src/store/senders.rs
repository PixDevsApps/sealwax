//! Group by Sender and Group by Domain (issue #24, ADR-023): a view's messages as one row per
//! sender address or per registrable domain, newest first, with the view's filter applied.

use super::reader::ReadJob;
use super::views;
use super::{Reply, Store, StoreError, ViewSpec};
use rusqlite::Connection;
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
}

impl std::fmt::Debug for SenderGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SenderGroup")
            .field("count", &self.count)
            .field("unread", &self.unread)
            .field("latest", &self.latest)
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
    /// [`MAX_GROUPS`]. `GroupBy::None` gives none.
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
    // SQLite takes the bare columns of an aggregate with max() from the row holding the max.
    let sql = format!(
        "SELECT {key} AS k, count(*), sum(flags & 1 = 0), max(date), from_name, from_addr, \
           subject \
         FROM messages INDEXED BY {index} \
         WHERE folder_id IN ({ids}){condition} AND {key} <> '' \
         GROUP BY k ORDER BY max(date) DESC, k LIMIT {MAX_GROUPS}"
    );
    let groups = conn
        .prepare_cached(&sql)?
        .query_map([], |row| {
            let name: String = row.get(4)?;
            let address: String = row.get(5)?;
            Ok(SenderGroup {
                key: row.get(0)?,
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
