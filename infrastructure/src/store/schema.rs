//! The schema and its `user_version` migrations (the Hertz pattern): each migration runs in its
//! own transaction, and a database from a newer build is refused and left untouched.

use super::StoreError;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::time::Duration;

pub const VERSION: i64 = 4;

const MIGRATIONS: &[&str] = &[V1, V2, V3, V4];

/// S3's schema v1 with S1's additions (ADR-005 amendments): folder names and sync state, keywords,
/// threading headers, recipient lists, the List-Id and the body structure.
pub(super) const V1: &str = "
CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, applied INTEGER NOT NULL);

CREATE TABLE store_version(version INTEGER NOT NULL);
INSERT INTO store_version VALUES (0);

CREATE TABLE meta(key TEXT PRIMARY KEY, value INTEGER NOT NULL) WITHOUT ROWID;

CREATE TABLE accounts(
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    address TEXT NOT NULL
);

CREATE TABLE folders(
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    name_raw TEXT NOT NULL,
    delimiter TEXT,
    attributes TEXT NOT NULL DEFAULT '',
    role TEXT NOT NULL DEFAULT 'other',
    selectable INTEGER NOT NULL DEFAULT 1,
    uidvalidity INTEGER,
    uidnext INTEGER NOT NULL DEFAULT 1,
    highestmodseq INTEGER NOT NULL DEFAULT 0,
    sync_state TEXT NOT NULL DEFAULT 'new' CHECK (sync_state IN ('new', 'initial', 'ready')),
    initial_top INTEGER,
    initial_modseq INTEGER,
    sync_floor INTEGER,
    total_count INTEGER NOT NULL DEFAULT 0,
    unread_count INTEGER NOT NULL DEFAULT 0,
    UNIQUE(account_id, name_raw)
);
CREATE INDEX folders_role ON folders(role);

CREATE TABLE threads(
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    subject TEXT NOT NULL,
    first_date INTEGER NOT NULL,
    last_date INTEGER NOT NULL,
    message_count INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE messages(
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    uid INTEGER NOT NULL,
    modseq INTEGER,
    thread_id INTEGER NOT NULL REFERENCES threads(id),
    message_id TEXT,
    in_reply_to TEXT NOT NULL DEFAULT '',
    refs TEXT NOT NULL DEFAULT '',
    date INTEGER NOT NULL,
    sent_date INTEGER,
    from_name TEXT NOT NULL,
    from_addr TEXT NOT NULL,
    to_addrs TEXT NOT NULL DEFAULT '',
    cc_addrs TEXT NOT NULL DEFAULT '',
    reply_to TEXT NOT NULL DEFAULT '',
    subject TEXT NOT NULL,
    snippet TEXT NOT NULL,
    list_id TEXT,
    flags INTEGER NOT NULL,
    keywords TEXT NOT NULL DEFAULT '',
    has_attachments INTEGER NOT NULL,
    body_kind INTEGER NOT NULL,
    size INTEGER NOT NULL,
    body_state INTEGER NOT NULL,
    blob_key BLOB,
    UNIQUE(folder_id, uid)
);
CREATE INDEX messages_folder_date ON messages(folder_id, date);
CREATE INDEX messages_unread ON messages(folder_id, date, id, flags) WHERE flags & 1 = 0;
CREATE INDEX messages_flagged ON messages(folder_id, date, id, flags) WHERE flags & 4 <> 0;
CREATE INDEX messages_attachments ON messages(folder_id, date, id, has_attachments)
    WHERE has_attachments <> 0;
CREATE INDEX messages_date ON messages(date, id, folder_id);
CREATE INDEX messages_thread ON messages(thread_id, date);
CREATE INDEX messages_message_id ON messages(message_id);
CREATE INDEX messages_blob ON messages(blob_key) WHERE blob_key IS NOT NULL;

CREATE TABLE message_parts(
    message INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    part INTEGER NOT NULL,
    role INTEGER NOT NULL,
    mime_type TEXT NOT NULL,
    name TEXT,
    size INTEGER NOT NULL,
    content_id TEXT,
    inline INTEGER NOT NULL,
    PRIMARY KEY(message, role, part)
) WITHOUT ROWID;

CREATE TABLE thread_aliases(
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    message_id TEXT NOT NULL,
    thread_id INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    PRIMARY KEY(account_id, message_id)
) WITHOUT ROWID;
CREATE INDEX thread_aliases_thread ON thread_aliases(thread_id);

CREATE VIRTUAL TABLE message_fts USING fts5(
    subject, sender, recipients, body,
    content='', contentless_delete=1,
    tokenize='unicode61 remove_diacritics 2'
);
";

/// M2's account key (ML-034): the account's name in `accounts.toml` and the keyring, so a
/// deleted store finds its accounts again. Rows from before it, and local accounts that no
/// settings file lists (fixtures, self-tests), have none. `detached` marks an account that
/// `accounts.toml` no longer lists: its mail stays until it comes back or is removed.
const V2: &str = "
ALTER TABLE accounts ADD COLUMN key TEXT;
ALTER TABLE accounts ADD COLUMN detached INTEGER NOT NULL DEFAULT 0;
CREATE UNIQUE INDEX accounts_key ON accounts(key);
";

/// M3's Outlook conversations (ML-046): the 22-byte conversation block of a message's
/// `Thread-Index`, which the writer also files as a threading alias. Messages stored before it
/// have none.
const V3: &str = "
ALTER TABLE messages ADD COLUMN thread_index BLOB;
";

/// M3's first-sync window (ML-043): a folder may hold only its newest messages, every UID from
/// `sync_floor` up. `below_floor` counts the messages the server held under the floor when last
/// counted, a hint for the count checks and the STATUS skip (a stale value costs a UID diff,
/// never a wrong row). A floor of 1 means nothing older is left, which is true of every folder
/// M2 synced completely.
const V4: &str = "
ALTER TABLE folders ADD COLUMN below_floor INTEGER NOT NULL DEFAULT 0;
UPDATE folders SET sync_floor = 1 WHERE sync_state = 'ready';
";

/// SQLite 3.43 brought `contentless_delete`.
const MIN_SQLITE: i32 = 3_043_000;

/// Refuses an SQLite without FTS5 or `contentless_delete` (ADR-005: the system library, no
/// `bundled`).
pub fn check_sqlite() -> Result<(), StoreError> {
    let missing = || StoreError::MissingFts5(rusqlite::version().to_owned());
    if rusqlite::version_number() < MIN_SQLITE {
        return Err(missing());
    }
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(
        "CREATE VIRTUAL TABLE probe USING fts5(a, content='', contentless_delete=1);",
    )
    .map_err(|_| missing())
}

pub fn user_version(conn: &Connection) -> Result<i64, StoreError> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

/// Applies the missing migrations; refuses a newer schema and leaves it untouched.
pub fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    let current = user_version(conn)?;
    if current > VERSION {
        return Err(StoreError::NewerSchema {
            found: current,
            supported: VERSION,
        });
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let version = index as i64 + 1;
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_migrations(version, applied) VALUES (?1, unixepoch())",
            [version],
        )?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    Ok(())
}

/// The read-write connection: WAL, `synchronous=NORMAL`, migrated.
pub fn open_writer(path: &Path) -> Result<Connection, StoreError> {
    let mut conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    let version = user_version(&conn)?;
    if version > VERSION {
        return Err(StoreError::NewerSchema {
            found: version,
            supported: VERSION,
        });
    }
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA foreign_keys=ON;
         PRAGMA temp_store=MEMORY;
         PRAGMA cache_size=-65536;
         PRAGMA journal_size_limit=67108864;",
    )?;
    conn.set_prepared_statement_cache_capacity(128);
    migrate(&mut conn)?;
    Ok(conn)
}

/// A read-only connection for a reader thread.
pub fn open_reader(path: &Path) -> Result<Connection, StoreError> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch(
        "PRAGMA query_only=1;
         PRAGMA temp_store=MEMORY;
         PRAGMA cache_size=-16384;",
    )?;
    conn.set_prepared_statement_cache_capacity(64);
    let version = user_version(&conn)?;
    if version != VERSION {
        return Err(StoreError::NewerSchema {
            found: version,
            supported: VERSION,
        });
    }
    Ok(conn)
}

/// The maintenance thread's connection: checkpoints and the one-row write that restarts the WAL.
pub fn open_maintenance(path: &Path) -> Result<Connection, StoreError> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
    Ok(conn)
}
