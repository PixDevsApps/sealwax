//! Recounts what the writer maintains and lists disagreements: folder counts (of shown rows),
//! conversation sizes (each Message-ID once) and dates, aliases, empty conversations, foreign
//! keys, full-text rows without a message, drafts and the outbox (parts held, bytes recorded
//! from the start of a send), contacts, and the op queue: every overlay recomputed from the
//! server's flags and the pending ops, and every pending op at its message. A new folder
//! without messages may show the server's STATUS counts instead (`FolderSpec::server_counts`),
//! so its counts aren't recounted. The server's counts below a windowed folder's floor are
//! hints; only their bounds are checked.

use super::overlay;
use super::writer::THREAD_INDEX_ALIAS;
use super::writer::overlay::{chain, read_below, row, server_column};
use rusqlite::Connection;
use sealwax_domain::model::MessageId;

/// Messages whose threading keys (Message-ID, In-Reply-To and References entries, the
/// `Thread-Index` conversation) don't all alias the message's own conversation.
const ALIASES: &str = "
WITH RECURSIVE words(account, thread, rest, word) AS (
    SELECT f.account_id, m.thread_id, m.in_reply_to || ' ' || m.refs || ' ', m.message_id
    FROM messages m JOIN folders f ON f.id = m.folder_id
    UNION ALL
    SELECT account, thread, substr(rest, instr(rest, ' ') + 1),
           substr(rest, 1, instr(rest, ' ') - 1)
    FROM words WHERE rest <> ''
),
keys(account, thread, key) AS (
    SELECT account, thread, word FROM words WHERE word <> ''
    UNION ALL
    SELECT f.account_id, m.thread_id, ?1 || lower(hex(m.thread_index))
    FROM messages m JOIN folders f ON f.id = m.folder_id WHERE m.thread_index IS NOT NULL
)
SELECT count(*) FROM keys k
LEFT JOIN thread_aliases a ON a.account_id = k.account AND a.message_id = k.key
WHERE a.thread_id IS NOT k.thread";

const CHECKS: [(&str, &str); 15] = [
    (
        "folder counts",
        "SELECT count(*) FROM folders f LEFT JOIN \
           (SELECT folder_id, count(*) AS a, sum(flags & 264 = 0) AS n, sum(flags & 265 = 0) AS u \
            FROM messages GROUP BY folder_id) c ON c.folder_id = f.id \
         WHERE (f.total_count <> coalesce(c.n, 0) OR f.unread_count <> coalesce(c.u, 0)) \
           AND NOT (f.sync_state = 'new' AND c.a IS NULL)",
    ),
    (
        "counts below the floor",
        "SELECT count(*) FROM folders \
         WHERE below_floor < 0 OR below_unread < 0 OR below_unread > below_floor",
    ),
    (
        "conversation sizes and dates",
        "SELECT count(*) FROM threads t LEFT JOIN \
           (SELECT thread_id, count(DISTINCT message_id) + count(*) - count(message_id) AS n, \
                   max(date) AS l, min(date) AS e \
            FROM messages GROUP BY thread_id) c ON c.thread_id = t.id \
         WHERE t.message_count <> coalesce(c.n, 0) OR t.last_date IS NOT c.l \
            OR t.first_date IS NOT c.e",
    ),
    (
        "empty conversations",
        "SELECT count(*) FROM threads t \
         WHERE NOT EXISTS (SELECT 1 FROM messages WHERE thread_id = t.id)",
    ),
    (
        "messages without a conversation",
        "SELECT count(*) FROM messages m \
         WHERE NOT EXISTS (SELECT 1 FROM threads WHERE id = m.thread_id)",
    ),
    (
        "conversations across accounts",
        "SELECT count(*) FROM messages m JOIN folders f ON f.id = m.folder_id \
         JOIN threads t ON t.id = m.thread_id WHERE t.account_id <> f.account_id",
    ),
    (
        "full-text rows without a message",
        "SELECT count(*) FROM message_fts x \
         WHERE NOT EXISTS (SELECT 1 FROM messages WHERE id = x.rowid)",
    ),
    (
        "server flags",
        "SELECT count(*) FROM messages WHERE server_flags = flags OR server_flags & 256 <> 0 \
           OR (server_flags IS NULL AND flags & 256 <> 0)",
    ),
    (
        "pending ops away from their messages",
        "SELECT count(*) FROM ops o JOIN messages m ON m.id = o.message \
           JOIN folders f ON f.id = m.folder_id \
         WHERE o.done = 0 AND (o.folder_id <> m.folder_id OR o.uid <> m.uid \
           OR o.uidvalidity <> coalesce(f.uidvalidity, 0) OR o.account_id <> f.account_id)",
    ),
    (
        "pending ops of missing folders",
        "SELECT count(*) FROM ops o WHERE o.done = 0 \
           AND (NOT EXISTS (SELECT 1 FROM folders WHERE id = o.folder_id) \
             OR (o.target IS NOT NULL AND NOT EXISTS (SELECT 1 FROM folders WHERE id = o.target)))",
    ),
    (
        "ops without a batch",
        "SELECT count(*) FROM ops o \
         WHERE NOT EXISTS (SELECT 1 FROM op_batches WHERE id = o.batch)",
    ),
    (
        "draft parts not held",
        "SELECT count(*) FROM draft_parts p WHERE NOT EXISTS \
           (SELECT 1 FROM draft_blobs b WHERE b.draft = p.draft AND b.blob_key = p.blob_key)",
    ),
    (
        "outbox entries of another account",
        "SELECT count(*) FROM outbox o JOIN drafts d ON d.id = o.draft \
         WHERE o.account_id <> d.account_id",
    ),
    (
        "sends started without their bytes",
        "SELECT count(*) FROM outbox WHERE state NOT IN (0, 1, 2, 3) \
           OR (state IN (1, 2) AND message_blob IS NULL) \
           OR (state = 0 AND attempts = 0 AND message_blob IS NOT NULL)",
    ),
    (
        "contacts",
        "SELECT count(*) FROM contacts c WHERE seen < 0 OR written < 0 \
           OR NOT EXISTS (SELECT 1 FROM contact_terms t WHERE t.contact = c.id)",
    ),
];

/// Rows whose overlay may differ from their server flags: those with pending ops, an overlay
/// stored, or under a pending read below their folder's floor.
const OVERLAID: &str = "
SELECT id FROM messages WHERE server_flags IS NOT NULL OR flags & 256 <> 0
UNION SELECT message FROM ops WHERE done = 0 AND message IS NOT NULL
UNION SELECT m.id FROM ops o JOIN messages m ON m.folder_id = o.folder_id AND m.uid < o.uid
WHERE o.done = 0 AND o.kind = 3";

/// Rows whose shown flags aren't their server flags with their pending ops applied, or whose
/// `server_flags` column doesn't follow.
fn overlays(conn: &Connection) -> rusqlite::Result<i64> {
    let ids: Vec<MessageId> = conn
        .prepare(OVERLAID)?
        .query_map([], |row| row.get(0).map(MessageId))?
        .collect::<rusqlite::Result<_>>()?;
    let mut bad = 0;
    for id in ids {
        let Some(found) = row(conn, id)? else {
            continue;
        };
        let stored: Option<u32> = conn
            .prepare_cached("SELECT server_flags FROM messages WHERE id = ?1")?
            .query_row([id.0], |row| row.get(0))?;
        let read = read_below(conn, found.folder, found.uidvalidity, found.uid)?;
        let expected = overlay::fold(found.folder, chain(conn, id)?.iter().map(|op| op.change))
            .shown(found.server, read);
        if expected != found.flags || stored != server_column(found.flags, found.server) {
            bad += 1;
        }
    }
    Ok(bad)
}

pub(crate) fn run(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let tx = conn.unchecked_transaction()?;
    let mut problems = Vec::new();
    for (name, sql) in CHECKS {
        let bad: i64 = tx.query_row(sql, [], |row| row.get(0))?;
        if bad != 0 {
            problems.push(format!("{name}: {bad} disagree"));
        }
    }
    let bad: i64 = tx.query_row(ALIASES, [THREAD_INDEX_ALIAS], |row| row.get(0))?;
    if bad != 0 {
        problems.push(format!("threading aliases: {bad} disagree"));
    }
    let bad = overlays(&tx)?;
    if bad != 0 {
        problems.push(format!("overlays: {bad} disagree"));
    }
    let violations = tx
        .prepare("PRAGMA foreign_key_check")?
        .query_map([], |_| Ok(()))?
        .count();
    if violations != 0 {
        problems.push(format!("foreign keys: {violations} violations"));
    }
    Ok(problems)
}
