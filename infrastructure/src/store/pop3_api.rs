//! POP3 accounts (M8: ML-087, ML-088). A POP3 account keeps its mail in local folders: Inbox,
//! Sent, Drafts, Junk, Trash and Archive, created the first time its actor starts. Its ops are
//! done at once in the store, as a local account's, with Undo; its sends go through the outbox,
//! their Sent copies are filed into its local Sent folder, and its drafts never go to a server.
//!
//! The account's actor downloads what the server lists that the store hasn't recorded, by the
//! server's UIDL: each message is stored with its UIDL in one transaction, so a crash never
//! loses a downloaded message nor downloads one twice. The UIDLs keep when each message was
//! downloaded, for "delete from the server after N days", and when Sealwax deleted it there.

use super::reader::ReadJob;
use super::writer::WriteJob;
use super::writer::pop3::Pop3Job;
use super::{NewMessage, Reply, Store, StoreVersion};
use sealwax_domain::model::{AccountId, FolderId, FolderRole, MessageId};
use std::fmt;

/// A POP3 account's local folders: their names and roles, in the order they are created.
pub const POP3_FOLDERS: [(&str, FolderRole); 6] = [
    ("INBOX", FolderRole::Inbox),
    ("Sent", FolderRole::Sent),
    ("Drafts", FolderRole::Drafts),
    ("Junk", FolderRole::Junk),
    ("Trash", FolderRole::Trash),
    ("Archive", FolderRole::Archive),
];

/// The reply to [`Store::pop3_account`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pop3Account {
    pub account: AccountId,
    /// Where downloaded mail goes.
    pub inbox: FolderId,
    /// When the account's first complete pass ended (Unix seconds); `None` before it, while the
    /// mail downloaded isn't new mail.
    pub first_pass: Option<i64>,
    /// This call made the account a POP3 account or created some of its folders.
    pub created: bool,
}

/// A message the account downloaded, by the server's UIDL.
#[derive(Clone, PartialEq, Eq)]
pub struct Pop3Uidl {
    pub uidl: String,
    /// The row it became, while the store holds it (the user may have deleted it since).
    pub message: Option<MessageId>,
    /// When it was downloaded (Unix seconds).
    pub downloaded: i64,
    /// When Sealwax deleted it from the server (Unix seconds).
    pub deleted: Option<i64>,
}

impl fmt::Debug for Pop3Uidl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pop3Uidl")
            .field("uidl", &self.uidl.len())
            .field("message", &self.message)
            .field("downloaded", &self.downloaded)
            .field("deleted", &self.deleted)
            .finish()
    }
}

/// A downloaded message to store with its UIDL.
#[derive(Clone, PartialEq, Eq)]
pub struct Pop3Download {
    pub uidl: String,
    /// Into one of the account's folders (its Inbox); `uid` `None`, as for local folders.
    pub message: NewMessage,
}

impl fmt::Debug for Pop3Download {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pop3Download")
            .field("uidl", &self.uidl.len())
            .field("folder", &self.message.folder)
            .field("size", &self.message.size)
            .finish()
    }
}

/// The writer's reply to [`Store::pop3_store`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pop3Stored {
    /// The commit's version, or the current one when nothing new was stored.
    pub version: StoreVersion,
    /// The new rows, in input order: `None` for a UIDL recorded already, and for a message the
    /// folder held already (its UIDL then names that row).
    pub ids: Vec<Option<MessageId>>,
}

/// What a pass leaves to record once it has ended.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Pop3PassDone {
    /// Every message the server listed is downloaded (or was too large to download): the
    /// account's first complete pass ends here, if it hadn't.
    pub complete: bool,
    /// UIDLs whose DELE a QUIT answered with `+OK` carried out.
    pub deleted: Vec<String>,
    /// UIDLs Sealwax deleted that the server no longer lists: forgotten.
    pub forget: Vec<String>,
    /// Unix seconds.
    pub at: i64,
}

impl fmt::Debug for Pop3PassDone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pop3PassDone")
            .field("complete", &self.complete)
            .field("deleted", &self.deleted.len())
            .field("forget", &self.forget.len())
            .field("at", &self.at)
            .finish()
    }
}

impl Store {
    /// Makes `account` a POP3 account, creating the local folders it lacks ([`POP3_FOLDERS`]),
    /// in one transaction; replies with its Inbox and its first complete pass. From then on
    /// its ops are done at once and nothing of it goes to an IMAP server.
    pub fn pop3_account(&self, account: AccountId) -> Reply<Pop3Account> {
        self.write(|reply| WriteJob::Pop3(Pop3Job::Account { account, reply }))
    }

    /// Every UIDL the account has downloaded, in UIDL order.
    pub fn pop3_uidls(&self, account: AccountId) -> Reply<Vec<Pop3Uidl>> {
        self.read(|reply| ReadJob::Pop3Uidls { account, reply })
    }

    /// Stores downloaded messages with their UIDLs in one transaction, then indexes their text;
    /// a UIDL recorded already is skipped, so a message is never stored twice. `at` is when
    /// they were downloaded (Unix seconds). Every message must go to one of the account's
    /// folders, and the account must be a POP3 account ([`Store::pop3_account`]).
    pub fn pop3_store(
        &self,
        account: AccountId,
        downloads: Vec<Pop3Download>,
        at: i64,
    ) -> Reply<Pop3Stored> {
        self.write(|reply| {
            WriteJob::Pop3(Pop3Job::Store {
                account,
                downloads,
                at,
                reply,
            })
        })
    }

    /// Records the end of a pass: its first complete pass, the server's deletions and the
    /// UIDLs to forget. Changes nothing the app shows.
    pub fn pop3_pass_done(&self, account: AccountId, done: Pop3PassDone) -> Reply<()> {
        self.write(|reply| {
            WriteJob::Pop3(Pop3Job::PassDone {
                account,
                done,
                reply,
            })
        })
    }
}
