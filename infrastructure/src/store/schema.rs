//! The schema and its `user_version` migrations (the Hertz pattern): each migration runs in its
//! own transaction, and a database from a newer build is refused and left untouched.

use super::StoreError;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::time::Duration;

pub const VERSION: i64 = 11;

pub(super) const MIGRATIONS: &[&str] = &[V1, V2, V3, V4, V5, V6, V7, V8, V9, V10, V11];

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

/// M3's bodies (ML-044): every raw message the engine downloaded, with its size on disk and when
/// it was last fetched or opened, for the disk cap, which evicts the oldest first. A body leaves
/// the table with the last message that refers to it, however that message goes. Attachments of
/// a message stored as its text parts keep their IMAP section, to be fetched on demand.
const V5: &str = "
CREATE TABLE bodies(
    blob_key BLOB PRIMARY KEY,
    size INTEGER NOT NULL,
    used INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX bodies_used ON bodies(used, blob_key);
CREATE TRIGGER bodies_unreferenced AFTER DELETE ON messages WHEN old.blob_key IS NOT NULL
BEGIN
    DELETE FROM bodies WHERE blob_key = old.blob_key
        AND NOT EXISTS (SELECT 1 FROM messages WHERE blob_key = old.blob_key);
END;
ALTER TABLE message_parts ADD COLUMN section TEXT;
";

/// The server's counts for a windowed folder (the M3 gate, 2026-10-06): `below_unread` counts
/// the server's unread messages under the floor, as `below_floor` counts all of them, from the
/// account pass's STATUS. The counts the sidebar shows add both to the store's own; a hint,
/// never a reason to store or drop a row.
const V6: &str = "
ALTER TABLE folders ADD COLUMN below_unread INTEGER NOT NULL DEFAULT 0;
";

/// M4's op queue (ML-051, ADR-015). `messages.flags` becomes what the user sees: the server's
/// flags with the pending ops applied, plus bit 256 while a pending move or expunge takes the
/// message out of its folder. `server_flags` keeps the server's flags where they differ from
/// what is shown, NULL otherwise. Rows hidden by that bit or flagged `\Deleted` (8) are in no
/// view and no count, so every list index leaves them out (and holds `flags`, so its walks stay
/// covering: SQLite checks the condition again on every row) and the counts are recounted. An op
/// is one change to one message (or, for a folder marked read, to the server's unread below its
/// floor), in FIFO order per message; done ops stay with their batch, for Undo. Refusals the
/// sync paths decide wait in `op_refusals` for the account's op runner.
const V7: &str = "
ALTER TABLE messages ADD COLUMN server_flags INTEGER;
DROP INDEX messages_folder_date;
DROP INDEX messages_unread;
DROP INDEX messages_flagged;
DROP INDEX messages_attachments;
DROP INDEX messages_date;
CREATE INDEX messages_folder_date ON messages(folder_id, date, id, flags) WHERE flags & 264 = 0;
CREATE INDEX messages_unread ON messages(folder_id, date, id, flags) WHERE flags & 265 = 0;
CREATE INDEX messages_flagged ON messages(folder_id, date, id, flags) WHERE flags & 268 = 4;
CREATE INDEX messages_attachments ON messages(folder_id, date, id, has_attachments, flags)
    WHERE has_attachments <> 0 AND flags & 264 = 0;
CREATE INDEX messages_date ON messages(date, id, folder_id, flags) WHERE flags & 264 = 0;
CREATE INDEX messages_special ON messages(folder_id)
    WHERE server_flags IS NOT NULL OR flags & 8 <> 0;
UPDATE folders SET
    total_count = (SELECT count(*) FROM messages WHERE folder_id = folders.id AND flags & 264 = 0),
    unread_count = (SELECT count(*) FROM messages WHERE folder_id = folders.id AND flags & 265 = 0)
WHERE id IN (SELECT folder_id FROM messages WHERE flags & 8 <> 0);

CREATE TABLE op_batches(
    id INTEGER PRIMARY KEY,
    kind INTEGER NOT NULL,
    undone INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE ops(
    id INTEGER PRIMARY KEY,
    batch INTEGER NOT NULL REFERENCES op_batches(id) ON DELETE CASCADE,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    kind INTEGER NOT NULL,
    done INTEGER NOT NULL DEFAULT 0,
    message INTEGER REFERENCES messages(id) ON DELETE SET NULL,
    folder_id INTEGER NOT NULL,
    uidvalidity INTEGER NOT NULL DEFAULT 0,
    uid INTEGER NOT NULL DEFAULT 0,
    message_id TEXT,
    add_flags INTEGER NOT NULL DEFAULT 0,
    remove_flags INTEGER NOT NULL DEFAULT 0,
    target INTEGER,
    prior_folder INTEGER NOT NULL,
    prior_flags INTEGER NOT NULL DEFAULT 0,
    below INTEGER NOT NULL DEFAULT 0,
    not_before INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    target_uidnext INTEGER,
    new_uid INTEGER
);
CREATE INDEX ops_message ON ops(message, id);
CREATE INDEX ops_batch ON ops(batch);
CREATE INDEX ops_pending ON ops(account_id, not_before) WHERE done = 0;
CREATE INDEX ops_folder ON ops(folder_id) WHERE done = 0;
CREATE INDEX ops_waiting ON ops(folder_id, kind, message_id) WHERE done = 0 AND message IS NULL;
CREATE INDEX ops_target ON ops(target) WHERE done = 0 AND target IS NOT NULL;

CREATE TABLE op_refusals(
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    change INTEGER NOT NULL,
    messages INTEGER NOT NULL
);
CREATE INDEX op_refusals_account ON op_refusals(account_id);
";

/// M5's senders whose remote images load without asking (ML-058): one row per address, lower
/// case, with when the user added it (Unix seconds). The list belongs to the user, not to an
/// account: it stays when an account is removed.
const V8: &str = "
CREATE TABLE remote_senders(
    address TEXT PRIMARY KEY,
    added INTEGER NOT NULL
) WITHOUT ROWID;
";

/// M6's drafts, outbox and contacts (ML-074, ML-076, ML-077, ML-071; ADR-015's at-most-once
/// send). A draft is the composer's `domain::compose::Draft` with a revision bumped by every
/// save, whether a composer window has it open (reopened after a crash), its copy in the
/// server's Drafts folder and the upload under way (with the folder's UIDNEXT before the first
/// attempt). `draft_blobs` holds every blob a draft ever referenced, so none is evicted or
/// reported orphaned before the draft is discarded or sent. An `outbox` row makes a draft a
/// send: queued (until `not_before`), sending (from just before SMTP DATA, with the built bytes
/// and envelope), uncertain (sending when Sealwax stopped: never sent again without the user) or
/// sent, with its Sent copy. `draft_deletions` are server copies to delete that the store holds
/// no row for. `contacts` are the addresses seen in From, To and Cc per account, with
/// `contact_terms` (folded address, domain and name words) for prefix suggestions; the Rust step
/// after this SQL fills them from the stored messages.
const V9: &str = "
CREATE TABLE drafts(
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    message_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    created INTEGER NOT NULL,
    saved INTEGER NOT NULL,
    open INTEGER NOT NULL DEFAULT 0,
    from_name TEXT NOT NULL,
    from_addr TEXT NOT NULL,
    to_addrs TEXT NOT NULL,
    cc_addrs TEXT NOT NULL,
    bcc_addrs TEXT NOT NULL,
    subject TEXT NOT NULL,
    html TEXT NOT NULL,
    in_reply_to TEXT,
    refs TEXT NOT NULL,
    source_message INTEGER REFERENCES messages(id) ON DELETE SET NULL,
    source_message_id TEXT,
    source_kind INTEGER,
    failure INTEGER,
    copy_folder INTEGER,
    copy_uidvalidity INTEGER,
    copy_uid INTEGER,
    copy_mark INTEGER,
    copy_revision INTEGER NOT NULL DEFAULT 0,
    upload_revision INTEGER,
    upload_folder INTEGER,
    upload_uidvalidity INTEGER,
    upload_mark INTEGER,
    upload_attempts INTEGER NOT NULL DEFAULT 0,
    upload_not_before INTEGER NOT NULL DEFAULT 0,
    upload_refused INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX drafts_account ON drafts(account_id);
CREATE INDEX drafts_message_id ON drafts(message_id);
CREATE INDEX drafts_source ON drafts(source_message) WHERE source_message IS NOT NULL;
CREATE INDEX drafts_open ON drafts(open) WHERE open = 1;

CREATE TABLE draft_parts(
    draft INTEGER NOT NULL REFERENCES drafts(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    name TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    size INTEGER NOT NULL,
    blob_key BLOB NOT NULL,
    content_id TEXT,
    PRIMARY KEY(draft, position)
) WITHOUT ROWID;

CREATE TABLE draft_blobs(
    draft INTEGER NOT NULL REFERENCES drafts(id) ON DELETE CASCADE,
    blob_key BLOB NOT NULL,
    PRIMARY KEY(draft, blob_key)
) WITHOUT ROWID;
CREATE INDEX draft_blobs_key ON draft_blobs(blob_key);

CREATE TABLE outbox(
    draft INTEGER PRIMARY KEY REFERENCES drafts(id) ON DELETE CASCADE,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    state INTEGER NOT NULL,
    revision INTEGER NOT NULL,
    queued INTEGER NOT NULL,
    not_before INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    message_id TEXT,
    message_blob BLOB,
    sent_copy_blob BLOB,
    size INTEGER,
    envelope_from TEXT,
    envelope_to TEXT,
    started INTEGER,
    sent INTEGER,
    copy_state INTEGER NOT NULL DEFAULT 0,
    copy_folder INTEGER,
    copy_uidvalidity INTEGER,
    copy_mark INTEGER,
    copy_uid INTEGER,
    copy_attempts INTEGER NOT NULL DEFAULT 0,
    copy_not_before INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX outbox_account ON outbox(account_id, state);
CREATE INDEX outbox_message_blob ON outbox(message_blob) WHERE message_blob IS NOT NULL;
CREATE INDEX outbox_sent_copy_blob ON outbox(sent_copy_blob) WHERE sent_copy_blob IS NOT NULL;

CREATE TABLE draft_deletions(
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    uidvalidity INTEGER NOT NULL,
    uid INTEGER,
    message_id TEXT NOT NULL,
    from_uid INTEGER NOT NULL DEFAULT 1,
    below_uid INTEGER,
    attempts INTEGER NOT NULL DEFAULT 0,
    not_before INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX draft_deletions_account ON draft_deletions(account_id);
CREATE INDEX draft_deletions_folder ON draft_deletions(folder_id, uidvalidity);

CREATE TABLE contacts(
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    address TEXT NOT NULL,
    name TEXT NOT NULL,
    seen INTEGER NOT NULL DEFAULT 0,
    written INTEGER NOT NULL DEFAULT 0,
    last INTEGER NOT NULL DEFAULT 0,
    UNIQUE(account_id, address)
);

CREATE TABLE contact_terms(
    term TEXT NOT NULL,
    contact INTEGER NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
    PRIMARY KEY(term, contact)
) WITHOUT ROWID;
CREATE INDEX contact_terms_contact ON contact_terms(contact);
";

/// M7's search (ML-081, ML-082). Full-text entries hold æ and ø as ae and o, which unicode61
/// doesn't fold, and the body column ends with the names of the attachments: the table stays,
/// and a store with mail indexes every message again once, in place and in the background
/// (`writer/index.rs`); meanwhile a query that spells æ or ø also asks for the old entries'
/// spelling. The date index, which a search of every folder walks, also holds
/// `has_attachments`, so `has:attachment` there reads no rows; built anew it is also packed
/// (3.6 MiB a first sync left at 100,000 messages, 1.9 MiB). Opening a v9 store of 100,000
/// messages took about 0.1 s with this migration.
const V10: &str = "
INSERT OR REPLACE INTO meta(key, value)
    SELECT 'fts_rebuild_through', 0 WHERE EXISTS (SELECT 1 FROM messages);
DROP INDEX messages_date;
CREATE INDEX messages_date ON messages(date, id, folder_id, flags, has_attachments)
    WHERE flags & 264 = 0;
";

/// M8's POP3 accounts (ML-087, ML-088). An account with a `pop3_accounts` row keeps its mail
/// in local folders: its ops are done at once in the store, as a local account's, and no
/// server folder or Drafts-folder copy belongs to it. `first_pass` is when its first complete
/// pass ended (Unix seconds): mail downloaded before it isn't new mail. `pop3_uidls` holds every
/// message the account downloaded by the server's UIDL, written in the transaction that stores
/// the message, so none is downloaded twice: the row it became while the store holds it, when
/// it was downloaded (Unix seconds, for deleting it from the server after N days), whether it
/// was downloaded whole (a message above the client's size cap is stored as its header and
/// stays on the server, never deleted there), and when Sealwax deleted it from the server
/// (DELE, then a QUIT sent: its reply may be lost, so the next listing tells).
/// `blob_deletions` holds the raw messages that ops done at once (local and POP3 accounts) left
/// unreferenced: their replies go to the app, which deletes no blobs, so an actor takes them.
const V11: &str = "
CREATE TABLE pop3_accounts(
    account_id INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    first_pass INTEGER
);

CREATE TABLE pop3_uidls(
    account_id INTEGER NOT NULL REFERENCES pop3_accounts(account_id) ON DELETE CASCADE,
    uidl TEXT NOT NULL,
    message INTEGER REFERENCES messages(id) ON DELETE SET NULL,
    downloaded INTEGER NOT NULL,
    whole INTEGER NOT NULL DEFAULT 1,
    deleted INTEGER,
    PRIMARY KEY(account_id, uidl)
) WITHOUT ROWID;
CREATE INDEX pop3_uidls_message ON pop3_uidls(message);

CREATE TABLE blob_deletions(blob_key BLOB PRIMARY KEY) WITHOUT ROWID;
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

/// Applies the missing migrations; refuses a newer schema and leaves it untouched. v9's
/// contacts are filled from the stored messages in its transaction.
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
        if version == 9 {
            super::contacts::backfill(&tx)?;
        }
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
