//! Open views and the SQL that reads them. A view is one folder or several (the unified inbox)
//! with a filter, ordered by `(date, id)` descending; readers page through it and the writer
//! computes positions in it for change events.

use super::api::{ViewFilter, ViewId, ViewScope, ViewSpec};
use rusqlite::{Connection, OptionalExtension, Row, params};
use sealwax_domain::model::{FolderId, MessageId, MessageSummary, ThreadId};
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

/// The partial index each filter reads, and the condition that selects it.
fn filter_sql(filter: ViewFilter) -> (&'static str, &'static str) {
    match filter {
        ViewFilter::All => ("messages_folder_date", ""),
        ViewFilter::Unread => ("messages_unread", " AND flags & 1 = 0"),
        ViewFilter::Starred => ("messages_flagged", " AND flags & 4 <> 0"),
        ViewFilter::Attachments => ("messages_attachments", " AND has_attachments <> 0"),
    }
}

/// Whether a row with these values passes `filter`.
pub(crate) fn passes(filter: ViewFilter, flags: u32, has_attachments: bool) -> bool {
    match filter {
        ViewFilter::All => true,
        ViewFilter::Unread => flags & 1 == 0,
        ViewFilter::Starred => flags & 4 != 0,
        ViewFilter::Attachments => has_attachments,
    }
}

/// The folders a view lists.
pub(crate) fn folders(conn: &Connection, scope: ViewScope) -> rusqlite::Result<Vec<FolderId>> {
    match scope {
        ViewScope::Folder(folder) => Ok(vec![folder]),
        ViewScope::UnifiedInbox => conn
            .prepare_cached("SELECT id FROM folders WHERE role = 'inbox' ORDER BY id")?
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
/// index from the oldest end, so no page reads more than half the view. Several folders without
/// a filter walk the global date index (which covers `id` and `folder_id`); with a filter, each
/// folder's partial index is read up to the page and the results merged.
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
    } else if filter == ViewFilter::All {
        let list: Vec<String> = folders.iter().map(|folder| folder.0.to_string()).collect();
        format!(
            "SELECT id FROM messages INDEXED BY messages_date WHERE folder_id IN ({}) \
             ORDER BY date {order}, id {order} LIMIT ?1 OFFSET ?2",
            list.join(", ")
        )
    } else {
        let reach = skip + limit;
        let mut union = String::new();
        for (n, folder) in folders.iter().enumerate() {
            if n > 0 {
                union.push_str(" UNION ALL ");
            }
            let _ = write!(
                union,
                "SELECT id, date FROM (SELECT id, date FROM messages INDEXED BY {index} \
                 WHERE folder_id = {}{condition} ORDER BY date {order}, id {order} LIMIT {reach})",
                folder.0
            );
        }
        format!("SELECT id FROM ({union} ORDER BY date {order}, id {order} LIMIT ?1 OFFSET ?2)")
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

/// The newest `limit` messages matching an FTS5 expression, walking the date index against the
/// match set ("date first", S3).
pub(crate) fn search(
    conn: &Connection,
    expression: &str,
    folder: Option<FolderId>,
    limit: u32,
) -> rusqlite::Result<Vec<MessageSummary>> {
    let inner = match folder {
        None => "SELECT m.id FROM messages m INDEXED BY messages_date \
                 WHERE m.id IN (SELECT rowid FROM message_fts WHERE message_fts MATCH ?1) \
                 ORDER BY m.date DESC, m.id DESC LIMIT ?2"
            .to_owned(),
        Some(folder) => format!(
            "SELECT m.id FROM messages m INDEXED BY messages_folder_date \
             WHERE m.folder_id = {} \
               AND m.id IN (SELECT rowid FROM message_fts WHERE message_fts MATCH ?1) \
             ORDER BY m.date DESC, m.id DESC LIMIT ?2",
            folder.0
        ),
    };
    let sql = format!(
        "SELECT {SUMMARY_COLUMNS} FROM ({inner}) AS p JOIN messages m ON m.id = p.id \
         JOIN threads t ON t.id = m.thread_id ORDER BY m.date DESC, m.id DESC"
    );
    conn.prepare_cached(&sql)?
        .query_map(params![expression, limit], summary_row)?
        .collect()
}
