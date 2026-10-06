//! Open views and the SQL that reads them. A view is one folder or several (the unified inbox)
//! with a filter, ordered by `(date, id)` descending; readers page through it and the writer
//! computes positions in it for change events.

use super::api::{ViewFilter, ViewId, ViewScope, ViewSpec};
use super::fts::Expression;
use super::overlay;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, Statement, ToSql, params};
use sealwax_domain::model::{FolderId, FolderRole, MessageId, MessageSummary, ThreadId};
use sealwax_domain::search::{Date, Query};
use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Write as _;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Keeps a view registered for change events; dropping it closes the view without blocking.
pub struct ViewHandle {
    id: ViewId,
    spec: ViewSpec,
    closed: async_channel::Sender<ViewId>,
}

impl ViewHandle {
    pub fn id(&self) -> ViewId {
        self.id
    }

    pub fn spec(&self) -> ViewSpec {
        self.spec
    }
}

impl Drop for ViewHandle {
    fn drop(&mut self) {
        let _ = self.closed.try_send(self.id);
    }
}

impl fmt::Debug for ViewHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ViewHandle")
            .field("id", &self.id)
            .field("spec", &self.spec)
            .finish()
    }
}

#[derive(Default)]
pub(crate) struct Views {
    next: u64,
    pub(crate) open: BTreeMap<ViewId, ViewSpec>,
}

/// The open views. A reader registers a view and pins its snapshot under this lock, and the
/// writer holds it from computing a commit's diffs until the commit, so every view either
/// started before a commit (and gets its diff) or after it (and needs none).
pub(crate) struct Registry {
    views: Mutex<Views>,
    closed_tx: async_channel::Sender<ViewId>,
    closed_rx: async_channel::Receiver<ViewId>,
}

impl Registry {
    pub(crate) fn new() -> Self {
        let (closed_tx, closed_rx) = async_channel::unbounded();
        Self {
            views: Mutex::new(Views::default()),
            closed_tx,
            closed_rx,
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Views> {
        let mut views = self.views.lock().unwrap_or_else(PoisonError::into_inner);
        while let Ok(id) = self.closed_rx.try_recv() {
            views.open.remove(&id);
        }
        views
    }

    pub(crate) fn register(&self, views: &mut Views, spec: ViewSpec) -> ViewHandle {
        views.next += 1;
        let id = ViewId(views.next);
        views.open.insert(id, spec);
        ViewHandle {
            id,
            spec,
            closed: self.closed_tx.clone(),
        }
    }
}

/// The partial index each filter reads, and the condition that selects it. Every index leaves
/// out rows hidden by a pending move or expunge (256) and rows flagged `\Deleted` (8).
fn filter_sql(filter: ViewFilter) -> (&'static str, &'static str) {
    match filter {
        ViewFilter::All => ("messages_folder_date", " AND flags & 264 = 0"),
        ViewFilter::Unread => ("messages_unread", " AND flags & 265 = 0"),
        ViewFilter::Starred => ("messages_flagged", " AND flags & 268 = 4"),
        ViewFilter::Attachments => (
            "messages_attachments",
            " AND has_attachments <> 0 AND flags & 264 = 0",
        ),
    }
}

/// Whether a row with these values passes `filter`.
pub(crate) fn passes(filter: ViewFilter, flags: u32, has_attachments: bool) -> bool {
    overlay::shown(flags)
        && match filter {
            ViewFilter::All => true,
            ViewFilter::Unread => flags & 1 == 0,
            ViewFilter::Starred => flags & 4 != 0,
            ViewFilter::Attachments => has_attachments,
        }
}

/// The folders a view lists; the unified inbox leaves out detached accounts.
pub(crate) fn folders(conn: &Connection, scope: ViewScope) -> rusqlite::Result<Vec<FolderId>> {
    match scope {
        ViewScope::Folder(folder) => Ok(vec![folder]),
        ViewScope::UnifiedInbox => conn
            .prepare_cached(
                "SELECT f.id FROM folders f JOIN accounts a ON a.id = f.account_id \
                 WHERE f.role = 'inbox' AND a.detached = 0 ORDER BY f.id",
            )?
            .query_map([], |row| row.get(0).map(FolderId))?
            .collect(),
    }
}

/// Rows in the view.
pub(crate) fn count(
    conn: &Connection,
    folders: &[FolderId],
    filter: ViewFilter,
) -> rusqlite::Result<u32> {
    let sql = match filter {
        ViewFilter::All => "SELECT coalesce(sum(total_count), 0) FROM folders WHERE id = ?1".into(),
        ViewFilter::Unread => {
            "SELECT coalesce(sum(unread_count), 0) FROM folders WHERE id = ?1".into()
        }
        ViewFilter::Starred | ViewFilter::Attachments => {
            let (index, condition) = filter_sql(filter);
            format!(
                "SELECT count(*) FROM messages INDEXED BY {index} WHERE folder_id = ?1{condition}"
            )
        }
    };
    let mut statement = conn.prepare_cached(&sql)?;
    let mut total = 0i64;
    for folder in folders {
        total += statement.query_row([folder.0], |row| row.get::<_, i64>(0))?;
    }
    Ok(total as u32)
}

/// Rows dated at or after `start`.
pub(crate) fn count_since(
    conn: &Connection,
    folders: &[FolderId],
    filter: ViewFilter,
    start: i64,
) -> rusqlite::Result<u32> {
    let (index, condition) = filter_sql(filter);
    let sql = format!(
        "SELECT count(*) FROM messages INDEXED BY {index} \
         WHERE folder_id = ?1{condition} AND date >= ?2"
    );
    let mut statement = conn.prepare_cached(&sql)?;
    let mut total = 0i64;
    for folder in folders {
        total += statement.query_row(params![folder.0, start], |row| row.get::<_, i64>(0))?;
    }
    Ok(total as u32)
}

/// The position of the row `(date, id)`, which is in folder `home` and in the view: the number of
/// rows before it. Each folder is counted from the end the row is nearer to, judged from the
/// folder's date range (undated messages, at 0, left out of it), so a message deep in a large
/// folder costs little more than a new one.
pub(crate) fn position(
    conn: &Connection,
    folders: &[FolderId],
    filter: ViewFilter,
    date: i64,
    id: MessageId,
    home: FolderId,
) -> rusqlite::Result<u32> {
    let (index, condition) = filter_sql(filter);
    let count = |comparison: &str, tie: &str, folder: FolderId| -> rusqlite::Result<i64> {
        let newer = format!(
            "SELECT count(*) FROM messages INDEXED BY {index} \
             WHERE folder_id = ?1{condition} AND date {comparison} ?2"
        );
        let same = format!(
            "SELECT count(*) FROM messages INDEXED BY {index} \
             WHERE folder_id = ?1{condition} AND date = ?2 AND id {tie} ?3"
        );
        Ok(conn
            .prepare_cached(&newer)?
            .query_row(params![folder.0, date], |row| row.get::<_, i64>(0))?
            + conn
                .prepare_cached(&same)?
                .query_row(params![folder.0, date, id.0], |row| row.get::<_, i64>(0))?)
    };
    let edge = |order: &str, folder: FolderId| -> rusqlite::Result<Option<i64>> {
        conn.prepare_cached(&format!(
            "SELECT date FROM messages INDEXED BY {index} \
             WHERE folder_id = ?1{condition} AND date > 0 ORDER BY date {order} LIMIT 1"
        ))?
        .query_row([folder.0], |row| row.get(0))
        .optional()
    };
    let mut position = 0i64;
    for folder in folders {
        let (Some(oldest), Some(newest)) = (edge("ASC", *folder)?, edge("DESC", *folder)?) else {
            continue;
        };
        let deep = newest > oldest && (newest - date) * 2 > newest - oldest;
        position += if deep {
            let total = i64::from(count_in(conn, *folder, filter)?);
            total - count("<", "<", *folder)? - i64::from(*folder == home)
        } else {
            count(">", ">", *folder)?
        };
    }
    Ok(position as u32)
}

fn count_in(conn: &Connection, folder: FolderId, filter: ViewFilter) -> rusqlite::Result<u32> {
    count(conn, &[folder], filter)
}

pub(crate) const SUMMARY_COLUMNS: &str = "m.id, m.folder_id, m.thread_id, m.from_name, \
     m.from_addr, m.subject, m.snippet, m.date, m.flags, m.has_attachments, t.message_count";

pub(crate) fn summary_row(row: &Row<'_>) -> rusqlite::Result<MessageSummary> {
    Ok(MessageSummary {
        id: MessageId(row.get(0)?),
        folder: FolderId(row.get(1)?),
        thread: ThreadId(row.get(2)?),
        sender_name: row.get(3)?,
        sender_email: row.get(4)?,
        subject: row.get(5)?,
        snippet: row.get(6)?,
        date: row.get(7)?,
        flags: sealwax_domain::model::Flags::from_bits(row.get::<_, u32>(8)?),
        has_attachments: row.get(9)?,
        thread_count: row.get(10)?,
    })
}

pub(crate) fn summary(conn: &Connection, id: MessageId) -> rusqlite::Result<MessageSummary> {
    let sql = format!(
        "SELECT {SUMMARY_COLUMNS} FROM messages m JOIN threads t ON t.id = m.thread_id \
         WHERE m.id = ?1"
    );
    conn.prepare_cached(&sql)?.query_row([id.0], summary_row)
}

/// Rows `offset..offset + limit` of a view with `total` rows. Pages past the middle walk the
/// index from the oldest end, so no page reads more than half the view. Each folder's index
/// (its date index, or the filter's partial index) holds its rows in list order, so several
/// folders are one compound SELECT whose ORDER BY every arm's index satisfies: SQLite merges
/// the folders' index walks without sorting and reads only the rows up to the page. Without a
/// filter, folders that hold at least half of all mail walk the global date index instead,
/// which costs less per row than the merge and skips little.
pub(crate) fn page(
    conn: &Connection,
    folders: &[FolderId],
    filter: ViewFilter,
    total: u32,
    offset: u32,
    limit: u32,
) -> rusqlite::Result<Vec<MessageSummary>> {
    if offset >= total || limit == 0 || folders.is_empty() {
        return Ok(Vec::new());
    }
    let limit = limit.min(total - offset);
    let oldest_first = offset > total / 2;
    let (order, skip) = if oldest_first {
        ("ASC", total - offset - limit)
    } else {
        ("DESC", offset)
    };
    let (index, condition) = filter_sql(filter);
    let inner = if let [folder] = folders {
        format!(
            "SELECT id FROM messages INDEXED BY {index} WHERE folder_id = {}{condition} \
             ORDER BY date {order}, id {order} LIMIT ?1 OFFSET ?2",
            folder.0
        )
    } else if filter == ViewFilter::All && u64::from(total) * 2 >= stored(conn)? {
        let list: Vec<String> = folders.iter().map(|folder| folder.0.to_string()).collect();
        format!(
            "SELECT id FROM messages INDEXED BY messages_date \
             WHERE folder_id IN ({}) AND flags & 264 = 0 \
             ORDER BY date {order}, id {order} LIMIT ?1 OFFSET ?2",
            list.join(", ")
        )
    } else {
        let mut walks = String::new();
        for (n, folder) in folders.iter().enumerate() {
            if n > 0 {
                walks.push_str(" UNION ALL ");
            }
            let _ = write!(
                walks,
                "SELECT id, date FROM messages INDEXED BY {index} WHERE folder_id = {}{condition}",
                folder.0
            );
        }
        format!("SELECT id FROM ({walks} ORDER BY date {order}, id {order} LIMIT ?1 OFFSET ?2)")
    };
    let sql = format!(
        "SELECT {SUMMARY_COLUMNS} FROM ({inner}) AS p JOIN messages m ON m.id = p.id \
         JOIN threads t ON t.id = m.thread_id ORDER BY m.date {order}, m.id {order}"
    );
    let mut rows = conn
        .prepare_cached(&sql)?
        .query_map(params![limit, skip], summary_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if oldest_first {
        rows.reverse();
    }
    Ok(rows)
}

/// Messages in the store, from the folders' maintained counts.
fn stored(conn: &Connection) -> rusqlite::Result<u64> {
    conn.prepare_cached("SELECT coalesce(sum(total_count), 0) FROM folders")?
        .query_row([], |row| row.get::<_, i64>(0))
        .map(|count| count.max(0) as u64)
}

/// Below this many matches a search reads the matching rows themselves ("FTS first"). From it
/// on, it walks a date index against the match set ("date first", S3): the walk reads only the
/// rows newer than its last result, but FTS5 gathers every match before it starts.
const FEW_MATCHES: usize = 200;

/// Folders a search of every folder leaves out, as most clients do: what the user threw away and
/// what the server judged junk. A search in one of them finds its mail.
pub(crate) const UNSEARCHED: [FolderRole; 2] = [FolderRole::Trash, FolderRole::Junk];

/// The folders a search looks in: `folder`, or every folder of an attached account but the
/// [`UNSEARCHED`] ones.
fn searched(conn: &Connection, folder: Option<FolderId>) -> rusqlite::Result<Vec<FolderId>> {
    if let Some(folder) = folder {
        return Ok(vec![folder]);
    }
    conn.prepare_cached(
        "SELECT f.id FROM folders f JOIN accounts a ON a.id = f.account_id \
         WHERE a.detached = 0 AND f.role NOT IN (?1, ?2) ORDER BY f.id",
    )?
    .query_map([UNSEARCHED[0].as_str(), UNSEARCHED[1].as_str()], |row| {
        row.get(0).map(FolderId)
    })?
    .collect()
}

/// The newest `limit` messages a query finds, in `folder` or in every searched folder. Hidden
/// rows are left out and the flags are the ones shown, pending ops applied, so search agrees
/// with the list. `after:` and `before:` are local midnights.
pub(crate) fn search(
    conn: &Connection,
    query: &Query,
    folder: Option<FolderId>,
    limit: u32,
) -> rusqlite::Result<Vec<MessageSummary>> {
    let after = query.after.map(local_midnight);
    let before = query.before.map(local_midnight);
    if limit == 0 || matches!((after, before), (Some(after), Some(before)) if after >= before) {
        return Ok(Vec::new());
    }
    let folders = searched(conn, folder)?;
    let mut conditions = match folders.as_slice() {
        [] => return Ok(Vec::new()),
        [folder] => format!(" AND m.folder_id = {}", folder.0),
        many => format!(
            " AND m.folder_id IN ({})",
            list(many.iter().map(|folder| folder.0))
        ),
    };
    for (wanted, yes, no) in [
        (query.unread, "m.flags & 1 = 0", "m.flags & 1 <> 0"),
        (query.starred, "m.flags & 4 <> 0", "m.flags & 4 = 0"),
        (
            query.has_attachment,
            "m.has_attachments <> 0",
            "m.has_attachments = 0",
        ),
    ] {
        if let Some(wanted) = wanted {
            conditions.push_str(" AND ");
            conditions.push_str(if wanted { yes } else { no });
        }
    }
    let mut values = vec![(":limit", Value::Integer(i64::from(limit)))];
    if let Some(after) = after {
        conditions.push_str(" AND m.date >= :after");
        values.push((":after", Value::Integer(after)));
    }
    if let Some(before) = before {
        conditions.push_str(" AND m.date < :before");
        values.push((":before", Value::Integer(before)));
    }
    let expression = Expression::of(query);
    if let Some(excluded) = expression.excluded {
        conditions.push_str(
            " AND m.id NOT IN (SELECT rowid FROM message_fts WHERE message_fts MATCH :excluded)",
        );
        values.push((":excluded", Value::Text(excluded)));
    }
    if let Some(matching) = expression.matching {
        if !expression.prefix
            && let Some(ids) = few_matches(conn, &matching)?
        {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let sql = format!(
                "SELECT {SUMMARY_COLUMNS} FROM messages m JOIN threads t ON t.id = m.thread_id \
                 WHERE m.id IN ({}) AND m.flags & 264 = 0{conditions} \
                 ORDER BY m.date DESC, m.id DESC LIMIT :limit",
                list(ids.into_iter())
            );
            return rows(&mut conn.prepare(&sql)?, &values);
        }
        conditions.push_str(
            " AND m.id IN (SELECT rowid FROM message_fts WHERE message_fts MATCH :matching)",
        );
        values.push((":matching", Value::Text(matching)));
    }
    let (index, shown) = walk(query, folders.len() == 1);
    let sql = format!(
        "SELECT {SUMMARY_COLUMNS} FROM (SELECT m.id FROM messages m INDEXED BY {index} \
         WHERE {shown}{conditions} ORDER BY m.date DESC, m.id DESC LIMIT :limit) AS p \
         JOIN messages m ON m.id = p.id JOIN threads t ON t.id = m.thread_id \
         ORDER BY m.date DESC, m.id DESC"
    );
    let mut statement = conn.prepare_cached(&sql)?;
    rows(&mut statement, &values)
}

/// The matches of a full-text expression, when there are at most [`FEW_MATCHES`]. FTS5 stops
/// early for words and phrases, not for prefixes.
fn few_matches(conn: &Connection, expression: &str) -> rusqlite::Result<Option<Vec<i64>>> {
    let ids = conn
        .prepare_cached("SELECT rowid FROM message_fts WHERE message_fts MATCH ?1 LIMIT ?2")?
        .query_map(params![expression, FEW_MATCHES as i64 + 1], |row| {
            row.get(0)
        })?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    Ok((ids.len() <= FEW_MATCHES).then_some(ids))
}

/// The index a search walks newest first, and the condition that selects it: in one folder the
/// partial index of the most selective condition asked for, across folders the date index.
fn walk(query: &Query, one_folder: bool) -> (&'static str, &'static str) {
    if !one_folder {
        ("messages_date", "m.flags & 264 = 0")
    } else if query.starred == Some(true) {
        ("messages_flagged", "m.flags & 268 = 4")
    } else if query.has_attachment == Some(true) {
        (
            "messages_attachments",
            "m.has_attachments <> 0 AND m.flags & 264 = 0",
        )
    } else if query.unread == Some(true) {
        ("messages_unread", "m.flags & 265 = 0")
    } else {
        ("messages_folder_date", "m.flags & 264 = 0")
    }
}

fn list(ids: impl Iterator<Item = i64>) -> String {
    let mut out = String::new();
    for (n, id) in ids.enumerate() {
        if n > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "{id}");
    }
    out
}

fn rows(
    statement: &mut Statement<'_>,
    values: &[(&str, Value)],
) -> rusqlite::Result<Vec<MessageSummary>> {
    let values: Vec<(&str, &dyn ToSql)> = values
        .iter()
        .map(|(name, value)| (*name, value as &dyn ToSql))
        .collect();
    statement
        .query_map(values.as_slice(), summary_row)?
        .collect()
}

/// Unix seconds at the start of `date` in the local time zone (UTC when the C library can't
/// tell).
pub(crate) fn local_midnight(date: Date) -> i64 {
    // SAFETY: `tm` is a plain C struct for which all zeros (a null zone name) is valid, and
    // `mktime` only reads and normalizes it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = date.year - 1900;
    tm.tm_mon = i32::from(date.month) - 1;
    tm.tm_mday = i32::from(date.day);
    tm.tm_isdst = -1;
    // SAFETY: as above; `mktime` is thread-safe in glibc.
    let seconds = unsafe { libc::mktime(&mut tm) };
    if seconds == -1 {
        utc_midnight(date)
    } else {
        seconds
    }
}

/// Unix seconds at the start of `date` in UTC (days from the civil date, Howard Hinnant's
/// algorithm).
fn utc_midnight(date: Date) -> i64 {
    let month = i64::from(date.month);
    let year = i64::from(date.year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let of_era = year.rem_euclid(400);
    let of_year = (153 * ((month + 9) % 12) + 2) / 5 + i64::from(date.day) - 1;
    let of_era_days = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    (era * 146_097 + of_era_days - 719_468) * 86_400
}
