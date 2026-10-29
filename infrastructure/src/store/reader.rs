//! Reader threads: read-only connections, each request in its own snapshot that reports the
//! `store_version` it saw. A reader takes every queued request and serves the newest first, so a
//! fast scroll isn't stuck behind stale pages; a request whose reply was dropped is skipped.

use super::api::{
    BodyCandidate, BodyTarget, BodyUsage, FolderSync, OpenView, Page, PartSource, StoreVersion,
    SyncState, Versioned, ViewOpened, ViewSpec,
};
use super::{Shared, StoreError, check, codec, views};
use rusqlite::{Connection, OptionalExtension};
use sealwax_domain::account::AccountKey;
use sealwax_domain::model::{
    Account, AccountId, Attachment, BlobKey, BodyKind, Folder, FolderId, FolderRole, Headers,
    MessageDetail, MessageId, MessageSummary, ThreadIndex,
};
use std::sync::Arc;

mod sync;

pub(crate) use sync::SyncRead;
mod bodies;
pub(crate) mod compose;
mod conversation;
mod pop3;
mod remote;

type Sender<T> = async_channel::Sender<Result<T, StoreError>>;

pub(crate) enum ReadJob {
    OpenView {
        request: OpenView,
        reply: Sender<ViewOpened>,
    },
    Page {
        spec: ViewSpec,
        offset: u32,
        limit: u32,
        reply: Sender<Page>,
    },
    AccountByKey {
        key: AccountKey,
        reply: Sender<Versioned<Option<Account>>>,
    },
    Accounts {
        reply: Sender<Versioned<Vec<Account>>>,
    },
    Folders {
        reply: Sender<Versioned<Vec<Folder>>>,
    },
    Detail {
        id: MessageId,
        reply: Sender<Versioned<Option<MessageDetail>>>,
    },
    Raw {
        id: MessageId,
        reply: Sender<Option<Vec<u8>>>,
    },
    Unsubscribe {
        ids: Vec<MessageId>,
        reply: Sender<Vec<super::UnsubscribeTarget>>,
    },
    Search {
        text: String,
        folder: Option<FolderId>,
        limit: u32,
        reply: Sender<Versioned<Vec<MessageSummary>>>,
    },
    Check {
        reply: Sender<Vec<String>>,
    },
    FolderSync {
        folder: FolderId,
        reply: Sender<Option<FolderSync>>,
    },
    Sync(SyncRead),
    Compose(compose::ComposeRead),
    BodyTargets {
        ids: Vec<MessageId>,
        reply: Sender<Vec<BodyTarget>>,
    },
    PartSource {
        message: MessageId,
        part: u32,
        reply: Sender<Option<PartSource>>,
    },
    BodyCandidates {
        since: i64,
        limit: u32,
        reply: Sender<Vec<BodyCandidate>>,
    },
    BodyUsage {
        reply: Sender<BodyUsage>,
    },
    PendingOps {
        account: AccountId,
        reply: Sender<u32>,
    },
    RemoteSenders {
        reply: Sender<Vec<String>>,
    },
    Conversation {
        id: MessageId,
        reply: Sender<Versioned<Option<crate::store::Conversation>>>,
    },
    Pop3Uidls {
        account: AccountId,
        reply: Sender<Vec<crate::store::Pop3Uidl>>,
    },
    IsPop3 {
        account: AccountId,
        reply: Sender<bool>,
    },
}

impl ReadJob {
    fn cancelled(&self) -> bool {
        match self {
            ReadJob::OpenView { reply, .. } => reply.is_closed(),
            ReadJob::Page { reply, .. } => reply.is_closed(),
            ReadJob::AccountByKey { reply, .. } => reply.is_closed(),
            ReadJob::Accounts { reply } => reply.is_closed(),
            ReadJob::Folders { reply } => reply.is_closed(),
            ReadJob::Detail { reply, .. } => reply.is_closed(),
            ReadJob::Raw { reply, .. } => reply.is_closed(),
            ReadJob::Unsubscribe { reply, .. } => reply.is_closed(),
            ReadJob::Search { reply, .. } => reply.is_closed(),
            ReadJob::Check { reply } => reply.is_closed(),
            ReadJob::FolderSync { reply, .. } => reply.is_closed(),
            ReadJob::Sync(read) => read.cancelled(),
            ReadJob::Compose(read) => read.cancelled(),
            ReadJob::BodyTargets { reply, .. } => reply.is_closed(),
            ReadJob::PartSource { reply, .. } => reply.is_closed(),
            ReadJob::BodyCandidates { reply, .. } => reply.is_closed(),
            ReadJob::BodyUsage { reply } => reply.is_closed(),
            ReadJob::PendingOps { reply, .. } => reply.is_closed(),
            ReadJob::RemoteSenders { reply } => reply.is_closed(),
            ReadJob::Conversation { reply, .. } => reply.is_closed(),
            ReadJob::Pop3Uidls { reply, .. } => reply.is_closed(),
            ReadJob::IsPop3 { reply, .. } => reply.is_closed(),
        }
    }
}

pub(crate) struct Reader {
    conn: Connection,
    shared: Arc<Shared>,
}

fn account_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Account> {
    Ok(Account {
        id: AccountId(row.get(0)?),
        name: row.get(1)?,
        address: row.get(2)?,
        key: row.get::<_, Option<String>>(3)?.map(AccountKey),
    })
}

fn version(conn: &Connection) -> rusqlite::Result<StoreVersion> {
    conn.prepare_cached("SELECT version FROM store_version")?
        .query_row([], |row| {
            row.get::<_, i64>(0)
                .map(|version| StoreVersion(version as u64))
        })
}

impl Reader {
    pub(crate) fn new(conn: Connection, shared: Arc<Shared>) -> Self {
        Self { conn, shared }
    }

    pub(crate) fn run(self, queue: async_channel::Receiver<ReadJob>) {
        while let Ok(first) = queue.recv_blocking() {
            let mut batch = vec![first];
            while let Ok(more) = queue.try_recv() {
                batch.push(more);
            }
            for job in batch.into_iter().rev() {
                if !job.cancelled() {
                    self.serve(job);
                }
            }
        }
    }

    fn serve(&self, job: ReadJob) {
        match job {
            ReadJob::OpenView { request, reply } => {
                let _ = reply.try_send(self.open_view(request));
            }
            ReadJob::Page {
                spec,
                offset,
                limit,
                reply,
            } => {
                let _ = reply.try_send(self.page(spec, offset, limit));
            }
            ReadJob::AccountByKey { key, reply } => {
                let _ = reply.try_send(self.account_by_key(&key));
            }
            ReadJob::Accounts { reply } => {
                let _ = reply.try_send(self.accounts());
            }
            ReadJob::Folders { reply } => {
                let _ = reply.try_send(self.folders());
            }
            ReadJob::Detail { id, reply } => {
                let _ = reply.try_send(self.detail(id));
            }
            ReadJob::Raw { id, reply } => {
                let _ = reply.try_send(self.raw(id));
            }
            ReadJob::Unsubscribe { ids, reply } => {
                let _ = reply.try_send(super::unsubscribe::targets(&self.conn, &ids));
            }
            ReadJob::Search {
                text,
                folder,
                limit,
                reply,
            } => {
                let _ = reply.try_send(self.search(&text, folder, limit));
            }
            ReadJob::Check { reply } => {
                let _ = reply.try_send(check::run(&self.conn).map_err(StoreError::from));
            }
            ReadJob::FolderSync { folder, reply } => {
                let _ = reply.try_send(self.folder_sync(folder));
            }
            ReadJob::Sync(read) => self.sync_read(read),
            ReadJob::Compose(read) => self.compose_read(read),
            ReadJob::BodyTargets { ids, reply } => {
                let _ = reply.try_send(self.body_targets(&ids));
            }
            ReadJob::PartSource {
                message,
                part,
                reply,
            } => {
                let _ = reply.try_send(self.part_source(message, part));
            }
            ReadJob::BodyCandidates {
                since,
                limit,
                reply,
            } => {
                let _ = reply.try_send(self.body_candidates(since, limit));
            }
            ReadJob::BodyUsage { reply } => {
                let _ = reply.try_send(self.body_usage());
            }
            ReadJob::PendingOps { account, reply } => {
                let _ = reply.try_send(self.pending_ops(account));
            }
            ReadJob::RemoteSenders { reply } => {
                let _ = reply.try_send(self.remote_senders());
            }
            ReadJob::Conversation { id, reply } => {
                let _ = reply.try_send(self.conversation(id));
            }
            ReadJob::Pop3Uidls { account, reply } => {
                let _ = reply.try_send(self.pop3_uidls(account));
            }
            ReadJob::IsPop3 { account, reply } => {
                let _ = reply.try_send(self.is_pop3(account));
            }
        }
    }

    fn pending_ops(&self, account: AccountId) -> Result<u32, StoreError> {
        Ok(self
            .conn
            .prepare_cached("SELECT count(*) FROM ops WHERE account_id = ?1 AND done = 0")?
            .query_row([account.0], |row| row.get(0))?)
    }

    fn open_view(&self, request: OpenView) -> Result<ViewOpened, StoreError> {
        let spec = request.spec;
        let (tx, version, view) = {
            let mut open = self.shared.registry.lock();
            let tx = self.conn.unchecked_transaction()?;
            let version = version(&tx)?;
            let view = self.shared.registry.register(&mut open, spec);
            (tx, version, view)
        };
        let folders = views::folders(&tx, spec.scope)?;
        let total = views::count(&tx, &folders, spec.filter)?;
        let section_counts = request
            .section_starts
            .iter()
            .map(|start| views::count_since(&tx, &folders, spec.filter, *start))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let rows = views::page(&tx, &folders, spec.filter, total, 0, request.first_page)?;
        Ok(ViewOpened {
            version,
            view,
            total,
            section_counts,
            rows,
        })
    }

    fn page(&self, spec: ViewSpec, offset: u32, limit: u32) -> Result<Page, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let version = version(&tx)?;
        let folders = views::folders(&tx, spec.scope)?;
        let total = views::count(&tx, &folders, spec.filter)?;
        let rows = views::page(&tx, &folders, spec.filter, total, offset, limit)?;
        Ok(Page {
            version,
            offset,
            rows,
        })
    }

    /// The account with `key`, detached or not.
    fn account_by_key(&self, key: &AccountKey) -> Result<Versioned<Option<Account>>, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let version = version(&tx)?;
        let value = tx
            .prepare_cached("SELECT id, name, address, key FROM accounts WHERE key = ?1")?
            .query_row([&key.0], account_row)
            .optional()?;
        Ok(Versioned { version, value })
    }

    /// The accounts the app shows: all but the detached ones.
    fn accounts(&self) -> Result<Versioned<Vec<Account>>, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let version = version(&tx)?;
        let value = tx
            .prepare_cached(
                "SELECT id, name, address, key FROM accounts WHERE detached = 0 ORDER BY id",
            )?
            .query_map([], account_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Versioned { version, value })
    }

    /// The counts are the server's where the store knows them, with the pending ops applied:
    /// the shown rows plus what a windowed folder leaves below its floor (ML-043).
    fn folders(&self) -> Result<Versioned<Vec<Folder>>, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let version = version(&tx)?;
        let value = tx
            .prepare_cached(&format!(
                "SELECT id, account_id, name, role, selectable, total_count + below_floor, {}, \
                   delimiter, attributes \
                 FROM folders ORDER BY account_id, id",
                super::overlay::UNREAD_SQL
            ))?
            .query_map([], |row| {
                Ok(Folder {
                    id: FolderId(row.get(0)?),
                    account: AccountId(row.get(1)?),
                    name: row.get(2)?,
                    delimiter: row
                        .get::<_, Option<String>>(7)?
                        .and_then(|delimiter| delimiter.chars().next()),
                    role: FolderRole::parse(&row.get::<_, String>(3)?),
                    selectable: row.get(4)?,
                    inferiors: !row
                        .get::<_, String>(8)?
                        .split(' ')
                        .any(|attribute| attribute.eq_ignore_ascii_case("\\Noinferiors")),
                    total: row.get(5)?,
                    unread: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Versioned { version, value })
    }

    fn detail(&self, id: MessageId) -> Result<Versioned<Option<MessageDetail>>, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let version = version(&tx)?;
        let Some(summary) = views::summary(&tx, id).optional()? else {
            return Ok(Versioned {
                version,
                value: None,
            });
        };
        let mut detail = tx
            .prepare_cached(
                "SELECT to_addrs, cc_addrs, reply_to, sent_date, message_id, in_reply_to, refs, \
                   list_id, keywords, body_kind, size, blob_key, thread_index, list_unsubscribe, \
                   one_click \
                 FROM messages WHERE id = ?1",
            )?
            .query_row([id.0], |row| {
                Ok(MessageDetail {
                    headers: Headers {
                        from: vec![sealwax_domain::model::Address::new(
                            summary.sender_name.clone(),
                            summary.sender_email.clone(),
                        )],
                        to: codec::decode_addresses(&row.get::<_, String>(0)?),
                        cc: codec::decode_addresses(&row.get::<_, String>(1)?),
                        reply_to: codec::decode_addresses(&row.get::<_, String>(2)?),
                        subject: summary.subject.clone(),
                        date: row.get(3)?,
                        message_id: row.get(4)?,
                        in_reply_to: codec::decode_words(&row.get::<_, String>(5)?),
                        references: codec::decode_words(&row.get::<_, String>(6)?),
                        list_id: row.get(7)?,
                        list_unsubscribe: row.get(13)?,
                        one_click: row.get(14)?,
                        thread_index: row
                            .get::<_, Option<Vec<u8>>>(12)?
                            .and_then(|index| ThreadIndex::from_slice(&index)),
                    },
                    keywords: codec::decode_words(&row.get::<_, String>(8)?),
                    body_kind: BodyKind::from_code(row.get(9)?),
                    size: row.get::<_, i64>(10)? as u64,
                    blob: row
                        .get::<_, Option<Vec<u8>>>(11)?
                        .and_then(|key| BlobKey::from_slice(&key)),
                    summary: summary.clone(),
                    text_parts: Vec::new(),
                    html_parts: Vec::new(),
                    attachments: Vec::new(),
                })
            })?;
        if detail.headers.from[0].email.is_empty() && detail.headers.from[0].name.is_empty() {
            detail.headers.from.clear();
        }
        let mut parts = tx.prepare_cached(
            "SELECT role, part, mime_type, name, size, content_id, inline FROM message_parts \
             WHERE message = ?1 ORDER BY role, part",
        )?;
        let mut rows = parts.query([id.0])?;
        while let Some(row) = rows.next()? {
            let part: u32 = row.get(1)?;
            match row.get::<_, i64>(0)? {
                0 => detail.text_parts.push(part),
                1 => detail.html_parts.push(part),
                _ => detail.attachments.push(Attachment {
                    part,
                    mime_type: row.get(2)?,
                    name: row.get(3)?,
                    size: row.get::<_, i64>(4)? as u64,
                    content_id: row.get(5)?,
                    inline: row.get(6)?,
                }),
            }
        }
        Ok(Versioned {
            version,
            value: Some(detail),
        })
    }

    fn raw(&self, id: MessageId) -> Result<Option<Vec<u8>>, StoreError> {
        let Some(blobs) = &self.shared.blobs else {
            return Ok(None);
        };
        let key: Option<Option<Vec<u8>>> = self
            .conn
            .prepare_cached("SELECT blob_key FROM messages WHERE id = ?1")?
            .query_row([id.0], |row| row.get(0))
            .optional()?;
        match key.flatten().and_then(|key| BlobKey::from_slice(&key)) {
            Some(key) => Ok(Some(blobs.get(&key)?)),
            None => Ok(None),
        }
    }

    fn search(
        &self,
        text: &str,
        folder: Option<FolderId>,
        limit: u32,
    ) -> Result<Versioned<Vec<MessageSummary>>, StoreError> {
        let query = sealwax_domain::search::parse_typed(text);
        let tx = self.conn.unchecked_transaction()?;
        let version = version(&tx)?;
        let value = if query.is_empty() {
            Vec::new()
        } else {
            views::search(&tx, &query, folder, limit)?
        };
        Ok(Versioned { version, value })
    }

    fn folder_sync(&self, folder: FolderId) -> Result<Option<FolderSync>, StoreError> {
        let row = self
            .conn
            .prepare_cached(&format!(
                "SELECT {FOLDER_SYNC} FROM folders f WHERE id = ?1"
            ))?
            .query_row([folder.0], folder_sync_row)
            .optional()?;
        Ok(row)
    }
}

/// The columns `folder_sync_row` reads.
pub(super) const FOLDER_SYNC: &str = "id, account_id, name_raw, delimiter, role, selectable, \
    uidvalidity, uidnext, highestmodseq, sync_state, initial_top, initial_modseq, sync_floor, \
    below_floor, (SELECT count(*) FROM messages WHERE folder_id = f.id), unsubscribe_backfill";

pub(super) fn folder_sync_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FolderSync> {
    let state = match row.get::<_, String>(9)?.as_str() {
        "initial" => SyncState::Initial {
            top: row.get::<_, Option<u32>>(10)?.unwrap_or(1),
            modseq: row.get::<_, Option<i64>>(11)?.unwrap_or(0) as u64,
            floor: row.get::<_, Option<u32>>(12)?.unwrap_or(1),
        },
        "ready" => SyncState::Ready,
        _ => SyncState::New,
    };
    Ok(FolderSync {
        folder: FolderId(row.get(0)?),
        account: AccountId(row.get(1)?),
        name_raw: row.get(2)?,
        delimiter: row
            .get::<_, Option<String>>(3)?
            .and_then(|delimiter| delimiter.chars().next()),
        role: FolderRole::parse(&row.get::<_, String>(4)?),
        selectable: row.get(5)?,
        uidvalidity: row.get(6)?,
        uidnext: row.get(7)?,
        highestmodseq: row.get::<_, i64>(8)? as u64,
        state,
        floor: row.get(12)?,
        below: row.get(13)?,
        messages: row.get(14)?,
        unsubscribe_backfill: row.get(15)?,
    })
}
