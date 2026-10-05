//! Recounts what the writer maintains and lists disagreements: folder counts, conversation
//! sizes (each Message-ID once) and dates, aliases, empty conversations, foreign keys and
//! full-text rows without a message. A new folder without messages may show the server's
//! STATUS counts instead (`FolderSpec::server_counts`), so its counts aren't recounted.

use rusqlite::Connection;

const CHECKS: [(&str, &str); 7] = [
    (
        "folder counts",
        "SELECT count(*) FROM folders f LEFT JOIN \
           (SELECT folder_id, count(*) AS n, sum(flags & 1 = 0) AS u FROM messages GROUP BY folder_id) c \
           ON c.folder_id = f.id \
         WHERE (f.total_count <> coalesce(c.n, 0) OR f.unread_count <> coalesce(c.u, 0)) \
           AND NOT (f.sync_state = 'new' AND c.n IS NULL)",
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
        "message-id aliases",
        "SELECT count(*) FROM messages m JOIN folders f ON f.id = m.folder_id \
         JOIN thread_aliases a ON a.account_id = f.account_id AND a.message_id = m.message_id \
         WHERE a.thread_id <> m.thread_id",
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
];

pub(crate) fn run(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let tx = conn.unchecked_transaction()?;
    let mut problems = Vec::new();
    for (name, sql) in CHECKS {
        let bad: i64 = tx.query_row(sql, [], |row| row.get(0))?;
        if bad != 0 {
            problems.push(format!("{name}: {bad} disagree"));
        }
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
