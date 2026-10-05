//! The folder list (ML-036): LIST with SPECIAL-USE and STATUS return options where offered,
//! names decoded from modified UTF-7, roles by SPECIAL-USE and then the name table, and the
//! server's MESSAGES and UNSEEN for folders the store hasn't synced yet.

use super::connection::Connection;
use super::error::{ImapError, SyncError};
use super::raw::{self, quote};
use super::{mutf7, roles};
use async_imap::imap_proto::{MailboxDatum, NameAttribute, Response, StatusAttribute};
use sealwax_domain::model::{AccountId, FolderId, FolderRole};
use sealwax_infrastructure::store::{FolderSpec, ServerCounts, Store};
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
    statuses: HashMap<String, ServerCounts>,
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
                if let Some(counts) = counts(status) {
                    self.statuses.insert(mailbox.to_string(), counts);
                }
            }
            _ => {}
        }
    }
}

fn counts(status: &[StatusAttribute]) -> Option<ServerCounts> {
    let mut total = None;
    let mut unread = None;
    for item in status {
        match item {
            StatusAttribute::Messages(n) => total = Some(*n),
            StatusAttribute::Unseen(n) => unread = Some(*n),
            _ => {}
        }
    }
    Some(ServerCounts {
        total: total?,
        unread: unread?,
    })
}

/// Lists every folder with its STATUS counts: in one round trip with LIST-STATUS, else LIST
/// followed by pipelined STATUS commands. SPECIAL-USE attributes are asked for explicitly
/// (`RETURN (SPECIAL-USE)`, RFC 6154) where the server offers it with LIST-EXTENDED.
pub async fn list(conn: &mut Connection) -> Result<Vec<ListedFolder>, ImapError> {
    let special_use = conn.caps().has("SPECIAL-USE") && conn.caps().has("LIST-EXTENDED");
    let list_status = conn.caps().has("LIST-STATUS");
    let mut options = Vec::new();
    if special_use {
        options.push("SPECIAL-USE");
    }
    if list_status {
        options.push("STATUS (MESSAGES UNSEEN)");
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
            .map(|entry| {
                Ok(format!(
                    "STATUS {} (MESSAGES UNSEEN)",
                    quote(&entry.name_raw)?
                ))
            })
            .collect::<Result<Vec<_>, ImapError>>()?;
        if !commands.is_empty() {
            raw::run_pipelined(&mut conn.session, &commands, |r| found.take(r)).await?;
        }
    }
    if let Some(name) = stale {
        found.statuses.remove(&name);
    }
    Ok(listed(found.entries, &found.statuses))
}

/// Decodes names and assigns roles.
fn listed(entries: Vec<Entry>, statuses: &HashMap<String, ServerCounts>) -> Vec<ListedFolder> {
    let names: Vec<String> = entries
        .iter()
        .map(|entry| mutf7::display_name(&entry.name_raw))
        .collect();
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
            counts: statuses.get(&entry.name_raw).copied(),
            name_raw: entry.name_raw,
            name,
            delimiter: entry.delimiter,
            attributes: entry.attributes,
            role,
        })
        .collect()
}

/// Lists the account's folders and writes them to the store in one commit. Folders that
/// disappeared from the server stay in the store until M3's re-sync removes them.
pub async fn sync_folders(
    conn: &mut Connection,
    store: &Store,
    account: AccountId,
) -> Result<SyncedFolders, SyncError> {
    let listed = list(conn).await?;
    let specs = listed
        .iter()
        .map(|folder| FolderSpec {
            account,
            name: folder.name.clone(),
            name_raw: folder.name_raw.clone(),
            delimiter: folder.delimiter,
            role: folder.role,
            attributes: folder.attributes.clone(),
            selectable: folder.selectable,
            server_counts: folder.counts,
        })
        .collect();
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
                ServerCounts {
                    total: 9,
                    unread: 2,
                },
            ),
            (
                "S&APg-ppelpost".to_owned(),
                ServerCounts {
                    total: 1,
                    unread: 1,
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
        );
        assert_eq!(folders[0].role, FolderRole::Inbox);
        assert_eq!(
            folders[0].counts,
            Some(ServerCounts {
                total: 9,
                unread: 2
            })
        );
        assert_eq!(folders[1].name, "Søppelpost");
        assert_eq!(folders[1].role, FolderRole::Junk);
        assert_eq!(folders[2].role, FolderRole::Sent);
        assert!(!folders[3].selectable);
        assert_eq!(folders[4].name, "Prosjekter/Høst 📬 2026");
        assert_eq!(folders[5].name, "Bad &Jjo", "invalid names show raw");
        assert_eq!(folders[5].counts, None);
    }
}
