//! Schema v1 with `user_version` migrations. One writer connection; read-only readers.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::time::Duration;

pub const VERSION: i64 = 1;

const MIGRATIONS: &[&str] = &[V1];

pub const V1: &str = "
CREATE TABLE store_version(version INTEGER NOT NULL);
INSERT INTO store_version VALUES (0);

CREATE TABLE meta(key TEXT PRIMARY KEY, value INTEGER NOT NULL) WITHOUT ROWID;

CREATE TABLE accounts(
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    address TEXT NOT NULL,
    kind TEXT NOT NULL
);

CREATE TABLE mailboxes(
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    role TEXT,
    uidvalidity INTEGER,
    uidnext INTEGER,
    highestmodseq INTEGER,
    total_count INTEGER NOT NULL DEFAULT 0,
    unread_count INTEGER NOT NULL DEFAULT 0,
    UNIQUE(account_id, name)
);

CREATE TABLE threads(
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    subject TEXT NOT NULL,
    first_date INTEGER NOT NULL,
    last_date INTEGER NOT NULL,
    latest_id INTEGER,
    message_count INTEGER NOT NULL DEFAULT 0,
    unread_count INTEGER NOT NULL DEFAULT 0,
    flagged_count INTEGER NOT NULL DEFAULT 0,
    attachment_count INTEGER NOT NULL DEFAULT 0,
    participants TEXT NOT NULL DEFAULT ''
);

CREATE TABLE messages(
    id INTEGER PRIMARY KEY,
    mailbox_id INTEGER NOT NULL REFERENCES mailboxes(id) ON DELETE CASCADE,
    uid INTEGER NOT NULL,
    modseq INTEGER,
    message_id TEXT,
    thread_id INTEGER NOT NULL REFERENCES threads(id),
    date INTEGER NOT NULL,
    sent_date INTEGER,
    from_name TEXT NOT NULL,
    from_addr TEXT NOT NULL,
    to_summary TEXT NOT NULL,
    cc_summary TEXT NOT NULL,
    subject TEXT NOT NULL,
    snippet TEXT NOT NULL,
    flags INTEGER NOT NULL,
    has_attachments INTEGER NOT NULL,
    size INTEGER NOT NULL,
    body_state INTEGER NOT NULL,
    blob_key BLOB,
    UNIQUE(mailbox_id, uid)
);
CREATE INDEX messages_mailbox_date ON messages(mailbox_id, date);
CREATE INDEX messages_date ON messages(date);
CREATE INDEX messages_thread ON messages(thread_id, date);
CREATE INDEX messages_message_id ON messages(message_id);
CREATE INDEX messages_unread ON messages(mailbox_id, date) WHERE flags & 1 = 0;
CREATE INDEX messages_flagged ON messages(mailbox_id, date) WHERE flags & 4 <> 0;

CREATE TABLE thread_ids(
    account_id INTEGER NOT NULL,
    message_id TEXT NOT NULL,
    thread_id INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    PRIMARY KEY(account_id, message_id)
) WITHOUT ROWID;
CREATE INDEX thread_ids_thread ON thread_ids(thread_id);

CREATE TABLE thread_mailboxes(
    thread_id INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    mailbox_id INTEGER NOT NULL REFERENCES mailboxes(id) ON DELETE CASCADE,
    last_date INTEGER NOT NULL,
    latest_id INTEGER NOT NULL,
    message_count INTEGER NOT NULL,
    unread_count INTEGER NOT NULL,
    PRIMARY KEY(thread_id, mailbox_id)
) WITHOUT ROWID;
CREATE INDEX thread_mailboxes_list ON thread_mailboxes(mailbox_id, last_date, thread_id);

CREATE VIRTUAL TABLE message_fts USING fts5(
    subject, sender, recipients, body,
    content='', contentless_delete=1,
    tokenize='unicode61 remove_diacritics 2'
);
";

pub fn open_writer(path: &Path) -> Result<Connection> {
    let mut conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA foreign_keys=ON;
         PRAGMA temp_store=MEMORY;
         PRAGMA cache_size=-65536;",
    )?;
    conn.set_prepared_statement_cache_capacity(64);
    migrate(&mut conn)?;
    Ok(conn)
}

pub fn open_reader(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("open {} read-only", path.display()))?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch(
        "PRAGMA query_only=1;
         PRAGMA temp_store=MEMORY;
         PRAGMA cache_size=-16384;",
    )?;
    conn.set_prepared_statement_cache_capacity(32);
    let version = user_version(&conn)?;
    if version != VERSION {
        bail!("schema version {version}, expected {VERSION}");
    }
    Ok(conn)
}

pub fn user_version(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

/// Applies the missing migrations, each in its own transaction. Refuses a newer schema and
/// leaves it untouched.
pub fn migrate(conn: &mut Connection) -> Result<()> {
    let current = user_version(conn)?;
    if current > VERSION {
        bail!("the database has schema version {current}; this build knows up to {VERSION}");
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", index as i64 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

/// What the linked SQLite offers.
#[derive(Debug)]
pub struct Capabilities {
    pub version: String,
    pub source: &'static str,
    pub fts5_compiled: bool,
    pub fts5_works: bool,
    pub contentless_delete: bool,
    pub trigram: bool,
    pub dbstat: bool,
    pub compile_options: Vec<String>,
}

pub fn capabilities() -> Result<Capabilities> {
    let conn = Connection::open_in_memory()?;
    let compile_options = conn
        .prepare("PRAGMA compile_options")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let fts5_compiled = conn.query_row(
        "SELECT sqlite_compileoption_used('ENABLE_FTS5')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    let works = |sql: &str| conn.execute_batch(sql).is_ok();
    let fts5_works = works(
        "CREATE VIRTUAL TABLE probe USING fts5(a);
         INSERT INTO probe VALUES ('blåbærsyltetøy på fredag');
         SELECT * FROM probe WHERE probe MATCH 'blåbærsyltetøy';",
    );
    let contentless_delete =
        works("CREATE VIRTUAL TABLE probe_cd USING fts5(a, content='', contentless_delete=1);");
    let trigram = works("CREATE VIRTUAL TABLE probe_tri USING fts5(a, tokenize='trigram');");
    let dbstat = works("SELECT count(*) FROM dbstat;");
    Ok(Capabilities {
        version: rusqlite::version().to_owned(),
        source: if cfg!(feature = "bundled") {
            "bundled (libsqlite3-sys)"
        } else {
            "system (libsqlite3.so)"
        },
        fts5_compiled,
        fts5_works,
        contentless_delete,
        trigram,
        dbstat,
        compile_options,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_once_and_refuses_newer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mail.sqlite");
        let conn = open_writer(&path).unwrap();
        assert_eq!(user_version(&conn).unwrap(), VERSION);
        drop(conn);
        let conn = open_writer(&path).unwrap();
        assert_eq!(user_version(&conn).unwrap(), VERSION);
        drop(conn);
        assert!(open_reader(&path).is_ok());

        let future = dir.path().join("future.sqlite");
        let conn = Connection::open(&future).unwrap();
        conn.execute_batch(
            "PRAGMA user_version=9; CREATE TABLE later(x); INSERT INTO later VALUES (1);",
        )
        .unwrap();
        drop(conn);
        assert!(open_writer(&future).is_err());
        let conn = Connection::open(&future).unwrap();
        assert_eq!(user_version(&conn).unwrap(), 9);
        let kept: i64 = conn
            .query_row("SELECT x FROM later", [], |row| row.get(0))
            .unwrap();
        assert_eq!(kept, 1);
    }

    #[test]
    fn linked_sqlite_has_fts5() {
        let caps = capabilities().unwrap();
        assert!(caps.fts5_works, "{caps:?}");
        assert!(caps.contentless_delete, "{caps:?}");
    }
}
