//! The folder list (ML-036): LIST with SPECIAL-USE and STATUS return options where offered,
//! names decoded from modified UTF-7, other mail programs' housekeeping left out (`hidden`),
//! roles by SPECIAL-USE and then the name table, the server's MESSAGES and UNSEEN for folders
//! the store hasn't synced yet and for what a windowed folder leaves below its floor (ML-043),
//! and UIDNEXT, UIDVALIDITY and HIGHESTMODSEQ for the account pass's change check (ML-041).

use super::connection::Connection;
use super::error::{ImapError, SyncError};
use super::raw::{self, quote};
use super::{hidden, mutf7, roles};
use async_imap::imap_proto::{MailboxDatum, NameAttribute, Response, StatusAttribute};
use sealwax_domain::model::{AccountId, FolderId, FolderRole};
use sealwax_infrastructure::store::{FolderSpec, ServerCounts, ServerStatus, Store};
use std::collections::HashMap;

/// One folder as LIST and STATUS reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedFolder {
    /// The name on the wire (modified UTF-7), which every command uses.
    pub name_raw: String,
    /// The display name: decoded, or the wire name when it isn't valid modified UTF-7.
    pub name: String,
    pub delimiter: Option<char>,
    pub attributes: Vec<String>,
    pub role: FolderRole,
    pub selectable: bool,
    pub counts: Option<ServerCounts>,
    /// STATUS as listed; `None` for the connection's selected mailbox when it couldn't be
    /// closed first, whose STATUS may be stale.
    pub status: Option<FolderStatus>,
}

/// One folder's STATUS. HIGHESTMODSEQ only where the server has CONDSTORE.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FolderStatus {
    pub messages: Option<u32>,
    pub unseen: Option<u32>,
    pub uidnext: Option<u32>,
    pub uidvalidity: Option<u32>,
    pub highestmodseq: Option<u64>,
}

impl FolderStatus {
    fn counts(&self) -> Option<ServerCounts> {
        Some(ServerCounts {
            total: self.messages?,
            unread: self.unseen?,
        })
    }

    /// What the store needs to count a windowed folder's messages below its floor.
    pub fn server(&self) -> Option<ServerStatus> {
        Some(ServerStatus {
            uidvalidity: self.uidvalidity?,
            uidnext: self.uidnext?,
            highestmodseq: self.highestmodseq,
            counts: self.counts()?,
        })
    }
}

/// The stored folder list of an account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncedFolders {
    /// Store ids in LIST order.
    pub folders: Vec<(FolderId, ListedFolder)>,
    pub inbox: Option<FolderId>,
}

fn attribute(attribute: &NameAttribute<'_>) -> String {
    match attribute {
        NameAttribute::NoInferiors => "\\Noinferiors".into(),
        NameAttribute::NoSelect => "\\Noselect".into(),
        NameAttribute::Marked => "\\Marked".into(),
        NameAttribute::Unmarked => "\\Unmarked".into(),
        NameAttribute::All => "\\All".into(),
        NameAttribute::Archive => "\\Archive".into(),
        NameAttribute::Drafts => "\\Drafts".into(),
        NameAttribute::Flagged => "\\Flagged".into(),
        NameAttribute::Junk => "\\Junk".into(),
        NameAttribute::Sent => "\\Sent".into(),
        NameAttribute::Trash => "\\Trash".into(),
        NameAttribute::Extension(other) => other.to_string(),
        _ => String::new(),
    }
}

/// A LIST line before decoding.
struct Entry {
    name_raw: String,
    delimiter: Option<char>,
    attributes: Vec<String>,
}

/// LIST and STATUS responses as they arrive.
#[derive(Default)]
struct Found {
    entries: Vec<Entry>,
    statuses: HashMap<String, FolderStatus>,
}

impl Found {
    fn take(&mut self, response: &Response<'_>) {
        match response {
            Response::MailboxData(MailboxDatum::List(list)) => self.entries.push(Entry {
                name_raw: list.name.to_string(),
                delimiter: list.delimiter.as_ref().and_then(|d| d.chars().next()),
                attributes: list
                    .name_attributes
                    .iter()
                    .map(attribute)
                    .filter(|a| !a.is_empty())
                    .collect(),
            }),
            Response::MailboxData(MailboxDatum::Status { mailbox, status }) => {
                let found = self.statuses.entry(mailbox.to_string()).or_default();
                for item in status {
                    match item {
                        StatusAttribute::Messages(n) => found.messages = Some(*n),
                        StatusAttribute::Unseen(n) => found.unseen = Some(*n),
                        StatusAttribute::UidNext(n) => found.uidnext = Some(*n),
                        StatusAttribute::UidValidity(n) => found.uidvalidity = Some(*n),
                        StatusAttribute::HighestModSeq(n) => found.highestmodseq = Some(*n),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// Lists every folder with its STATUS: in one round trip with LIST-STATUS, else LIST followed by
/// pipelined STATUS commands. SPECIAL-USE attributes are asked for explicitly
/// (`RETURN (SPECIAL-USE)`, RFC 6154) where the server offers it with LIST-EXTENDED.
pub async fn list(conn: &mut Connection) -> Result<Vec<ListedFolder>, ImapError> {
    let special_use = conn.caps().has("SPECIAL-USE") && conn.caps().has("LIST-EXTENDED");
    let list_status = conn.caps().has("LIST-STATUS");
    let items = if conn.condstore() {
        "MESSAGES UNSEEN UIDNEXT UIDVALIDITY HIGHESTMODSEQ"
    } else {
        "MESSAGES UNSEEN UIDNEXT UIDVALIDITY"
    };
    let status = format!("STATUS ({items})");
    let mut options = Vec::new();
    if special_use {
        options.push("SPECIAL-USE");
    }
    if list_status {
        options.push(&status);
    }
    let list = if options.is_empty() {
        "LIST \"\" \"*\"".to_owned()
    } else {
        format!("LIST \"\" \"*\" RETURN ({})", options.join(" "))
    };
    // STATUS of the selected mailbox can be the session's stale view (Dovecot, S1): close it
    // first, or ignore its STATUS where the server can't.
    let unselect = conn.selected.is_some() && conn.caps().has("UNSELECT");
    let stale = conn.selected.clone().filter(|_| !unselect);
    let mut commands = Vec::new();
    if unselect {
        commands.push("UNSELECT".to_owned());
    }
    commands.push(list);
    let mut found = Found::default();
    raw::run_pipelined(&mut conn.session, &commands, |r| found.take(r)).await?;
    if unselect {
        conn.selected = None;
    }
    if !list_status {
        let commands = found
            .entries
            .iter()
            .filter(|entry| roles::selectable(&entry.attributes))
            .map(|entry| Ok(format!("STATUS {} ({items})", quote(&entry.name_raw)?)))
            .collect::<Result<Vec<_>, ImapError>>()?;
        if !commands.is_empty() {
            raw::run_pipelined(&mut conn.session, &commands, |r| found.take(r)).await?;
        }
    }
    if let Some(name) = stale {
        found.statuses.remove(&name);
    }
    Ok(listed(found.entries, &found.statuses, conn.microsoft()))
}

/// Decodes names, leaves out the folders `hidden` names and assigns roles to the rest;
/// `microsoft` for an account on Microsoft's servers.
fn listed(
    entries: Vec<Entry>,
    statuses: &HashMap<String, FolderStatus>,
    microsoft: bool,
) -> Vec<ListedFolder> {
    let (entries, names): (Vec<Entry>, Vec<String>) = entries
        .into_iter()
        .map(|entry| {
            let name = mutf7::display_name(&entry.name_raw);
            (entry, name)
        })
        .filter(|(entry, name)| {
            let shown = hidden::Entry {
                name,
                delimiter: entry.delimiter,
                selectable: roles::selectable(&entry.attributes),
                messages: statuses.get(&entry.name_raw).and_then(|s| s.messages),
            };
            !hidden::hidden(&shown, microsoft)
        })
        .unzip();
    let role_entries: Vec<roles::Entry<'_>> = entries
        .iter()
        .zip(&names)
        .map(|(entry, name)| roles::Entry {
            name,
            delimiter: entry.delimiter,
            attributes: &entry.attributes,
        })
        .collect();
    let assigned = roles::assign(&role_entries);
    entries
        .into_iter()
        .zip(names)
        .zip(assigned)
        .map(|((entry, name), role)| ListedFolder {
            selectable: roles::selectable(&entry.attributes),
            counts: statuses.get(&entry.name_raw).and_then(FolderStatus::counts),
            status: statuses.get(&entry.name_raw).copied(),
            name_raw: entry.name_raw,
            name,
            delimiter: entry.delimiter,
            attributes: entry.attributes,
            role,
        })
        .collect()
}

/// The store's row for a listed folder.
pub(crate) fn spec(account: AccountId, folder: &ListedFolder) -> FolderSpec {
    FolderSpec {
        account,
        name: folder.name.clone(),
        name_raw: folder.name_raw.clone(),
        delimiter: folder.delimiter,
        role: folder.role,
        attributes: folder.attributes.clone(),
        selectable: folder.selectable,
        server_counts: folder.counts,
    }
}

/// Lists the account's folders and writes them to the store in one commit. Folders that
/// disappeared from the server stay in the store; the account pass (`sync_account`) removes
/// them.
pub async fn sync_folders(
    conn: &mut Connection,
    store: &Store,
    account: AccountId,
) -> Result<SyncedFolders, SyncError> {
    let listed = list(conn).await?;
    let specs = listed.iter().map(|folder| spec(account, folder)).collect();
    let ids = store.upsert_folders(specs).recv().await?.value;
    let inbox = ids
        .iter()
        .zip(&listed)
        .find(|(_, folder)| folder.role == FolderRole::Inbox)
        .map(|(id, _)| *id);
    tracing::debug!(%account, folders = ids.len(), "folder list stored");
    Ok(SyncedFolders {
        folders: ids.into_iter().zip(listed).collect(),
        inbox,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, attributes: &[&str]) -> Entry {
        Entry {
            name_raw: name.into(),
            delimiter: Some('/'),
            attributes: attributes.iter().map(|a| a.to_string()).collect(),
        }
    }

    #[test]
    fn listed_folders_decode_and_get_roles_and_counts() {
        let statuses = HashMap::from([
            (
                "INBOX".to_owned(),
                FolderStatus {
                    messages: Some(9),
                    unseen: Some(2),
                    uidnext: Some(40),
                    uidvalidity: Some(7),
                    highestmodseq: Some(77),
                },
            ),
            (
                "S&APg-ppelpost".to_owned(),
                FolderStatus {
                    messages: Some(1),
                    unseen: Some(1),
                    ..FolderStatus::default()
                },
            ),
            (
                "Sendt".to_owned(),
                FolderStatus {
                    messages: Some(4),
                    ..FolderStatus::default()
                },
            ),
        ]);
        let folders = listed(
            vec![
                entry("INBOX", &[]),
                entry("S&APg-ppelpost", &["\\HasNoChildren"]),
                entry("Sendt", &["\\Sent"]),
                entry("Prosjekter", &["\\Noselect", "\\HasChildren"]),
                entry("Prosjekter/H&APg-st &2D3c7A- 2026", &[]),
                entry("Bad &Jjo", &[]),
            ],
            &statuses,
            false,
        );
        assert_eq!(folders[0].role, FolderRole::Inbox);
        assert_eq!(
            folders[0].counts,
            Some(ServerCounts {
                total: 9,
                unread: 2
            })
        );
        assert_eq!(folders[0].status.and_then(|s| s.highestmodseq), Some(77));
        assert_eq!(folders[2].counts, None, "MESSAGES without UNSEEN");
        assert_eq!(folders[2].status.and_then(|s| s.messages), Some(4));
        assert_eq!(folders[1].name, "Søppelpost");
        assert_eq!(folders[1].role, FolderRole::Junk);
        assert_eq!(folders[2].role, FolderRole::Sent);
        assert!(!folders[3].selectable);
        assert_eq!(folders[4].name, "Prosjekter/Høst 📬 2026");
        assert_eq!(folders[5].name, "Bad &Jjo", "invalid names show raw");
        assert_eq!(folders[5].counts, None);
    }

    #[test]
    fn housekeeping_folders_are_left_out() {
        let empty = FolderStatus {
            messages: Some(0),
            unseen: Some(0),
            ..FolderStatus::default()
        };
        let statuses = HashMap::from([
            ("Outbox".to_owned(), empty),
            ("Mailspring/Snoozed".to_owned(), empty),
            (
                "INBOX/Mailspring".to_owned(),
                FolderStatus {
                    messages: Some(1),
                    ..empty
                },
            ),
        ]);
        let entries = || {
            vec![
                entry("INBOX", &[]),
                entry("Sent", &["\\Sent"]),
                entry("Notes", &[]),
                entry("Outbox", &[]),
                entry("Synkroniseringsfeil", &["\\HasChildren"]),
                entry("Synkroniseringsfeil/Konflikter", &[]),
                entry("Mailspring", &["\\Noselect"]),
                entry("Mailspring/Snoozed", &[]),
                entry("INBOX/Mailspring", &[]),
            ]
        };
        let names = |folders: Vec<ListedFolder>| -> Vec<String> {
            folders.into_iter().map(|folder| folder.name).collect()
        };
        assert_eq!(
            names(listed(entries(), &statuses, true)),
            ["INBOX", "Sent", "INBOX/Mailspring"]
        );
        let elsewhere = listed(entries(), &statuses, false);
        assert_eq!(elsewhere[1].role, FolderRole::Sent);
        assert_eq!(
            names(elsewhere),
            ["INBOX", "Sent", "Notes", "Outbox", "INBOX/Mailspring"]
        );
    }
}
