//! Unsubscribe's store requests (issue #14, ADR-023): what the selected messages can be
//! unsubscribed from, one target per list, and a record of what the user unsubscribed from. A
//! list is its List-Id, or its sender's address when it has none.

use super::reader::ReadJob;
use super::writer::WriteJob;
use super::{Reply, Store, StoreError};
use rusqlite::{Connection, OptionalExtension, params};
use sealwax_domain::model::{AccountId, MessageId};
use sealwax_domain::unsubscribe::Method;
use std::collections::HashSet;

/// One list the selected messages can be unsubscribed from, from its newest selected message.
/// Its `Debug` prints no address or URI.
#[derive(Clone, PartialEq, Eq)]
pub struct UnsubscribeTarget {
    pub account: AccountId,
    /// The List-Id, or the sender's address in lower case.
    pub key: String,
    pub message: MessageId,
    pub sender_name: String,
    pub sender_email: String,
    /// [`sealwax_domain::unsubscribe::normalize`]d.
    pub list_unsubscribe: String,
    pub one_click: bool,
    /// When and how the user unsubscribed from it before.
    pub unsubscribed: Option<(Method, i64)>,
}

impl std::fmt::Debug for UnsubscribeTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnsubscribeTarget")
            .field("account", &self.account)
            .field("message", &self.message)
            .field("one_click", &self.one_click)
            .field("unsubscribed", &self.unsubscribed)
            .finish_non_exhaustive()
    }
}

impl Store {
    /// The lists `ids` can be unsubscribed from, newest first, each once per account; messages
    /// without a usable List-Unsubscribe are left out.
    pub fn unsubscribe_targets(&self, ids: Vec<MessageId>) -> Reply<Vec<UnsubscribeTarget>> {
        self.read(|reply| ReadJob::Unsubscribe { ids, reply })
    }

    /// Notes that the user unsubscribed from `key` of `account` with `method` at `at` (Unix
    /// seconds). Nothing the list shows changes, so there is no change event.
    pub fn record_unsubscribed(
        &self,
        account: AccountId,
        key: String,
        method: Method,
        at: i64,
    ) -> Reply<()> {
        self.write(|reply| WriteJob::RecordUnsubscribed {
            account,
            key,
            method,
            at,
            reply,
        })
    }
}

pub(super) fn targets(
    conn: &Connection,
    ids: &[MessageId],
) -> Result<Vec<UnsubscribeTarget>, StoreError> {
    let mut select = conn.prepare_cached(
        "SELECT f.account_id, coalesce(m.list_id, lower(trim(m.from_addr))), m.from_name, \
           m.from_addr, m.list_unsubscribe, m.one_click, u.method, u.at, m.date \
         FROM messages m JOIN folders f ON f.id = m.folder_id \
         LEFT JOIN unsubscribed u ON u.account_id = f.account_id \
           AND u.key = coalesce(m.list_id, lower(trim(m.from_addr))) \
         WHERE m.id = ?1 AND m.list_unsubscribe IS NOT NULL",
    )?;
    let mut found: Vec<(i64, UnsubscribeTarget)> = Vec::new();
    for id in ids {
        let row = select
            .query_row([id.0], |row| {
                let method: Option<u8> = row.get(6)?;
                let at: Option<i64> = row.get(7)?;
                Ok((
                    row.get::<_, i64>(8)?,
                    UnsubscribeTarget {
                        account: AccountId(row.get(0)?),
                        key: row.get(1)?,
                        message: *id,
                        sender_name: row.get(2)?,
                        sender_email: row.get(3)?,
                        list_unsubscribe: row.get(4)?,
                        one_click: row.get(5)?,
                        unsubscribed: method.and_then(Method::from_code).zip(at),
                    },
                ))
            })
            .optional()?;
        found.extend(row);
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.message.0.cmp(&a.1.message.0)));
    let mut seen = HashSet::new();
    Ok(found
        .into_iter()
        .map(|(_, target)| target)
        .filter(|target| seen.insert((target.account, target.key.clone())))
        .collect())
}

pub(super) fn record(
    conn: &Connection,
    account: AccountId,
    key: &str,
    method: Method,
    at: i64,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT OR REPLACE INTO unsubscribed(account_id, key, method, at) VALUES (?1, ?2, ?3, ?4)",
        params![account.0, key, method.code(), at],
    )?;
    Ok(())
}
