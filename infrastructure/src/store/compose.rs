//! What the writer and the readers share about drafts and the outbox: the column codes, a
//! draft read back whole, and which blobs drafts and the outbox hold.

use super::codec;
use super::compose_api::{SendFailure, SendState, StoredDraft};
use rusqlite::{Connection, OptionalExtension, params};
use sealwax_domain::compose::{Draft, DraftPart, DraftSource, ReplyKind};
use sealwax_domain::model::{AccountId, Address, BlobKey, DraftId, MessageId};
use std::collections::BTreeSet;

pub(crate) const QUEUED: i64 = 0;
pub(crate) const SENDING: i64 = 1;
pub(crate) const UNCERTAIN: i64 = 2;
pub(crate) const SENT: i64 = 3;

/// A Sent copy not started, and one whose APPEND started (its mark recorded).
pub(crate) const COPY_PENDING: i64 = 0;
pub(crate) const COPY_STARTED: i64 = 1;

pub(crate) fn state_from(code: i64) -> SendState {
    match code {
        QUEUED => SendState::Queued,
        SENDING => SendState::Sending,
        UNCERTAIN => SendState::Uncertain,
        _ => SendState::Sent,
    }
}

pub(crate) fn failure_code(failure: SendFailure) -> i64 {
    match failure {
        SendFailure::RecipientsRefused => 1,
        SendFailure::TooLarge => 2,
        SendFailure::Refused => 3,
        SendFailure::NeedsSignIn => 4,
        SendFailure::Other => 5,
    }
}

pub(crate) fn failure_from(code: i64) -> SendFailure {
    match code {
        1 => SendFailure::RecipientsRefused,
        2 => SendFailure::TooLarge,
        3 => SendFailure::Refused,
        4 => SendFailure::NeedsSignIn,
        _ => SendFailure::Other,
    }
}

pub(crate) fn kind_code(kind: ReplyKind) -> i64 {
    match kind {
        ReplyKind::Reply => 0,
        ReplyKind::ReplyAll => 1,
        ReplyKind::Forward => 2,
    }
}

pub(crate) fn kind_from(code: i64) -> ReplyKind {
    match code {
        0 => ReplyKind::Reply,
        1 => ReplyKind::ReplyAll,
        _ => ReplyKind::Forward,
    }
}

/// The source message's row: the one recorded, else (it left the store, a move fetched anew)
/// the account's copy with its Message-ID.
const SOURCE_SQL: &str = "coalesce(d.source_message, (SELECT m.id FROM messages m \
    JOIN folders f ON f.id = m.folder_id \
    WHERE m.message_id = d.source_message_id AND f.account_id = d.account_id \
    ORDER BY m.id LIMIT 1))";

/// A draft read back whole, with its place in the outbox.
pub(crate) fn read(conn: &Connection, id: DraftId) -> rusqlite::Result<Option<StoredDraft>> {
    let found = conn
        .prepare_cached(&format!(
            "SELECT d.account_id, d.message_id, d.revision, d.created, d.saved, d.open, \
               d.from_name, d.from_addr, d.to_addrs, d.cc_addrs, d.bcc_addrs, d.subject, d.html, \
               d.in_reply_to, d.refs, {SOURCE_SQL}, d.source_kind, d.failure, o.state \
             FROM drafts d LEFT JOIN outbox o ON o.draft = d.id WHERE d.id = ?1"
        ))?
        .query_row([id.0], |row| {
            let source = match (
                row.get::<_, Option<i64>>(15)?,
                row.get::<_, Option<i64>>(16)?,
            ) {
                (Some(message), Some(kind)) => Some(DraftSource {
                    message: MessageId(message),
                    kind: kind_from(kind),
                }),
                _ => None,
            };
            Ok(StoredDraft {
                id,
                draft: Draft {
                    account: AccountId(row.get(0)?),
                    message_id: row.get(1)?,
                    from: Address::new(row.get::<_, String>(6)?, row.get::<_, String>(7)?),
                    to: codec::decode_addresses(&row.get::<_, String>(8)?),
                    cc: codec::decode_addresses(&row.get::<_, String>(9)?),
                    bcc: codec::decode_addresses(&row.get::<_, String>(10)?),
                    subject: row.get(11)?,
                    html: row.get(12)?,
                    in_reply_to: row.get(13)?,
                    references: codec::decode_words(&row.get::<_, String>(14)?),
                    source,
                    parts: Vec::new(),
                },
                revision: row.get::<_, i64>(2)? as u64,
                created: row.get(3)?,
                saved: row.get(4)?,
                open: row.get(5)?,
                failure: row.get::<_, Option<i64>>(17)?.map(failure_from),
                send: row.get::<_, Option<i64>>(18)?.map(state_from),
            })
        })
        .optional()?;
    let Some(mut stored) = found else {
        return Ok(None);
    };
    stored.draft.parts = conn
        .prepare_cached(
            "SELECT name, mime_type, size, blob_key, content_id FROM draft_parts \
             WHERE draft = ?1 ORDER BY position",
        )?
        .query_map([id.0], |row| {
            let key: Vec<u8> = row.get(3)?;
            Ok(DraftPart {
                name: row.get(0)?,
                mime_type: row.get(1)?,
                size: row.get::<_, i64>(2)? as u64,
                blob: BlobKey::from_slice(&key).unwrap_or(BlobKey([0; 32])),
                content_id: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(stored))
}

/// Whether a draft or an outbox entry references `key`: such a blob is never evicted or
/// reported orphaned.
pub(crate) fn held(conn: &Connection, key: &BlobKey) -> rusqlite::Result<bool> {
    conn.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM draft_blobs INDEXED BY draft_blobs_key WHERE blob_key = ?1) \
           OR EXISTS(SELECT 1 FROM outbox WHERE message_blob = ?1) \
           OR EXISTS(SELECT 1 FROM outbox WHERE sent_copy_blob = ?1)",
    )?
    .query_row([key.0.as_slice()], |row| row.get(0))
}

/// Of `keys`, those neither a message, a draft nor the outbox references.
pub(crate) fn unreferenced(
    conn: &Connection,
    keys: BTreeSet<BlobKey>,
) -> rusqlite::Result<Vec<BlobKey>> {
    let mut used = conn.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM messages INDEXED BY messages_blob WHERE blob_key = ?1)",
    )?;
    let mut out = Vec::new();
    for key in keys {
        if !used.query_row([key.0.as_slice()], |row| row.get::<_, bool>(0))? && !held(conn, &key)? {
            out.push(key);
        }
    }
    Ok(out)
}

/// Every blob a draft holds: what it ever referenced, and its outbox entry's.
pub(crate) fn blobs_of(conn: &Connection, draft: DraftId) -> rusqlite::Result<BTreeSet<BlobKey>> {
    let mut keys = BTreeSet::new();
    let mut select = conn.prepare_cached(
        "SELECT blob_key FROM draft_blobs WHERE draft = ?1 \
         UNION SELECT message_blob FROM outbox WHERE draft = ?1 AND message_blob IS NOT NULL \
         UNION SELECT sent_copy_blob FROM outbox WHERE draft = ?1 AND sent_copy_blob IS NOT NULL",
    )?;
    for key in select.query_map(params![draft.0], |row| row.get::<_, Vec<u8>>(0))? {
        keys.extend(BlobKey::from_slice(&key?));
    }
    Ok(keys)
}
