//! Group by Sender and Group by Domain (issue #24, ADR-023): a view's messages as one row per
//! sender address or per registrable domain, newest first, with the view's filter applied.

use super::reader::ReadJob;
use super::views;
use super::{Reply, Store, StoreError, ViewSpec};
use rusqlite::Connection;
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

impl Store {
    /// The view's messages grouped `by` sender or domain, newest group first, at most
    /// [`MAX_GROUPS`]. `GroupBy::None` gives none.
    pub fn sender_groups(&self, spec: ViewSpec, by: GroupBy) -> Reply<Vec<SenderGroup>> {
        self.read(|reply| ReadJob::Groups { spec, by, reply })
    }
}

pub(super) fn groups(
    conn: &Connection,
    spec: ViewSpec,
    by: GroupBy,
) -> Result<Vec<SenderGroup>, StoreError> {
    let key = match by {
        GroupBy::None => return Ok(Vec::new()),
        GroupBy::Sender => "lower(trim(from_addr))",
        GroupBy::Domain => "from_domain",
    };
    let folders = views::folders(conn, spec.scope)?;
    if folders.is_empty() {
        return Ok(Vec::new());
    }
    let (index, condition) = views::filter_sql(spec.filter);
    let ids = folders
        .iter()
        .map(|folder| folder.0.to_string())
        .collect::<Vec<_>>()
        .join(", ");
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
