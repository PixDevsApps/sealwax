//! Folder management (ADR-020): what a change to an account's folders means for the folders the
//! store holds. [`plan`] checks it (a name the dialog would refuse, a role folder, a parent that
//! takes no subfolders) and works out the names on the wire and the folders under a renamed or
//! deleted one; a POP3 account's local folders then change at once ([`apply_local`]), and an
//! IMAP account's on its command connection (`imap::manage`).

use crate::api::{FolderChange, FolderError};
use sealwax_domain::folders::{self, Placement};
use sealwax_domain::model::{AccountId, BlobKey, FolderId, FolderRole};
use sealwax_infrastructure::store::{FolderRename, FolderSpec, Store, StoreError};
use std::collections::HashMap;

impl From<StoreError> for FolderError {
    fn from(error: StoreError) -> Self {
        tracing::warn!(%error, "a folder change couldn't read or write the store");
        FolderError::Store
    }
}

/// One of the account's folders as a change needs it.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Known {
    pub id: FolderId,
    /// The display name, the whole path.
    pub name: String,
    /// The name on the wire.
    pub name_raw: String,
    pub delimiter: Option<char>,
    pub role: FolderRole,
    /// It may have subfolders (not `\Noinferiors`).
    pub inferiors: bool,
}

/// The account's folders as the store holds them.
pub(crate) async fn known(store: &Store, account: AccountId) -> Result<Vec<Known>, FolderError> {
    let raw: HashMap<FolderId, String> = store
        .account_folders(account)
        .recv()
        .await?
        .into_iter()
        .map(|folder| (folder.folder, folder.name_raw))
        .collect();
    Ok(store
        .folders()
        .recv()
        .await?
        .value
        .into_iter()
        .filter(|folder| folder.account == account)
        .filter_map(|folder| {
            Some(Known {
                name_raw: raw.get(&folder.id)?.clone(),
                id: folder.id,
                name: folder.name,
                delimiter: folder.delimiter,
                role: folder.role,
                inferiors: folder.inferiors,
            })
        })
        .collect())
}

/// How names go on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wire {
    /// Modified UTF-7 (RFC 3501), with new folders at the top under the personal prefix the
    /// account's folders share.
    Imap,
    /// A POP3 account's local folders: as typed, levels apart by `/`.
    Local,
}

impl Wire {
    fn encode(self, name: &str) -> String {
        match self {
            Wire::Imap => crate::imap::mutf7::encode(name),
            Wire::Local => name.to_owned(),
        }
    }

    fn display(self, raw: &str) -> String {
        match self {
            Wire::Imap => crate::imap::mutf7::display_name(raw),
            Wire::Local => raw.to_owned(),
        }
    }
}

/// What a checked change does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Plan {
    Create {
        /// The display name, the whole path.
        name: String,
        name_raw: String,
    },
    /// Nothing to do when `from_raw` is `to_raw`.
    Rename {
        from_raw: String,
        to_raw: String,
        delimiter: Option<char>,
        /// The folder and the folders under it, with their new names.
        renames: Vec<FolderRename>,
        /// Their names on the wire before, in `renames`' order.
        before: Vec<String>,
    },
    Delete {
        top_raw: String,
        delimiter: Option<char>,
        /// The folder and the folders under it, the deepest first.
        folders: Vec<(FolderId, String)>,
    },
}

/// Role folders keep their names and places: Inbox, Drafts, Sent, Archive, Junk, Trash, All.
fn has_role(folder: &Known) -> bool {
    folder.role != FolderRole::Other || folder.name_raw.eq_ignore_ascii_case("INBOX")
}

/// The folder at the top of the account new folders go under: the Inbox, where every other
/// folder is under it (`INBOX.` servers), else none. NAMESPACE (RFC 2342) would say; its reply
/// is one imap-proto 0.17 can't read (see `imap::manage`).
fn top_parent(folders: &[Known]) -> Option<&Known> {
    let inbox = folders
        .iter()
        .find(|folder| folder.name_raw.eq_ignore_ascii_case("INBOX"))?;
    let delimiter = inbox.delimiter?;
    let mut others = folders
        .iter()
        .filter(|folder| folder.id != inbox.id)
        .peekable();
    others.peek()?;
    let length = inbox.name_raw.len();
    others
        .all(|folder| {
            let name = folder.name_raw.as_str();
            name.is_char_boundary(length)
                && name[..length].eq_ignore_ascii_case(&inbox.name_raw)
                && name[length..].starts_with(delimiter)
        })
        .then_some(inbox)
}

/// The account's hierarchy delimiter for a folder at its top.
fn top_delimiter(folders: &[Known], wire: Wire) -> Option<char> {
    match wire {
        Wire::Local => Some('/'),
        Wire::Imap => folders
            .iter()
            .find(|folder| folder.name_raw.eq_ignore_ascii_case("INBOX"))
            .and_then(|inbox| inbox.delimiter)
            .or_else(|| folders.iter().find_map(|folder| folder.delimiter)),
    }
}

/// The last levels of the folders whose parent is `parent` (`None`: the top), but `except`.
fn siblings<'a>(
    folders: &'a [Known],
    parent: Option<&str>,
    delimiter: Option<char>,
    except: Option<FolderId>,
) -> Vec<&'a str> {
    folders
        .iter()
        .filter(|folder| Some(folder.id) != except)
        .filter(|folder| folders::parent(&folder.name, delimiter) == parent)
        .map(|folder| folders::leaf(&folder.name, delimiter))
        .collect()
}

/// The folder `id` and every folder under it, by their names on the wire.
fn subtree<'a>(folders: &'a [Known], top: &Known) -> Vec<&'a Known> {
    folders
        .iter()
        .filter(|folder| folders::within(&folder.name_raw, &top.name_raw, top.delimiter))
        .collect()
}

fn find(folders: &[Known], id: FolderId) -> Result<&Known, FolderError> {
    folders
        .iter()
        .find(|folder| folder.id == id)
        .ok_or(FolderError::Missing)
}

/// A renamed or deleted folder: no role folder, and none under it.
fn changeable(folders: &[Known], id: FolderId) -> Result<&Known, FolderError> {
    let folder = find(folders, id)?;
    if subtree(folders, folder).into_iter().any(has_role) {
        return Err(FolderError::NotAllowed);
    }
    Ok(folder)
}

/// Checks `change` against the account's `folders` and works out what it does. `hidden` says
/// whether Sealwax would leave a folder of a whole display name (with its delimiter) out of
/// the list.
pub(crate) fn plan(
    change: &FolderChange,
    folders: &[Known],
    wire: Wire,
    hidden: impl Fn(&str, Option<char>) -> bool,
) -> Result<Plan, FolderError> {
    match change {
        FolderChange::Create { parent, name } => {
            let parent = match parent {
                Some(id) => {
                    let parent = find(folders, *id)?;
                    if !parent.inferiors || parent.delimiter.is_none() {
                        return Err(FolderError::NotAllowed);
                    }
                    Some(parent)
                }
                None if wire == Wire::Imap => top_parent(folders),
                None => None,
            };
            let delimiter = match parent {
                Some(parent) => parent.delimiter,
                None => top_delimiter(folders, wire),
            };
            let parent_name = parent.map(|parent| parent.name.as_str());
            let placement = Placement {
                delimiter,
                parent: parent_name,
                siblings: siblings(folders, parent_name, delimiter, None),
            };
            let leaf = folders::check_name(name, &placement, |whole| hidden(whole, delimiter))
                .map_err(FolderError::Name)?;
            let encoded = wire.encode(&leaf);
            let name_raw = match (parent, delimiter) {
                (Some(parent), Some(delimiter)) => {
                    format!("{}{delimiter}{encoded}", parent.name_raw)
                }
                _ => encoded,
            };
            Ok(Plan::Create {
                name: folders::child(parent_name, delimiter, &leaf),
                name_raw,
            })
        }
        FolderChange::Rename { folder, name } => {
            let folder = changeable(folders, *folder)?;
            let delimiter = folder.delimiter;
            let parent_name = folders::parent(&folder.name, delimiter);
            let placement = Placement {
                delimiter,
                parent: parent_name,
                siblings: siblings(folders, parent_name, delimiter, Some(folder.id)),
            };
            let leaf = folders::check_name(name, &placement, |whole| hidden(whole, delimiter))
                .map_err(FolderError::Name)?;
            let encoded = wire.encode(&leaf);
            let to_raw = match (folders::parent(&folder.name_raw, delimiter), delimiter) {
                (Some(parent), Some(delimiter)) => format!("{parent}{delimiter}{encoded}"),
                _ => encoded,
            };
            let mut renames = Vec::new();
            let mut before = Vec::new();
            for under in subtree(folders, folder) {
                let Some(name_raw) =
                    folders::renamed(&under.name_raw, &folder.name_raw, &to_raw, delimiter)
                else {
                    continue;
                };
                before.push(under.name_raw.clone());
                renames.push(FolderRename {
                    folder: under.id,
                    name: wire.display(&name_raw),
                    name_raw,
                });
            }
            Ok(Plan::Rename {
                from_raw: folder.name_raw.clone(),
                to_raw,
                delimiter,
                renames,
                before,
            })
        }
        FolderChange::Delete { folder } => {
            let folder = changeable(folders, *folder)?;
            let mut under: Vec<(FolderId, String)> = subtree(folders, folder)
                .into_iter()
                .map(|known| (known.id, known.name_raw.clone()))
                .collect();
            deepest_first(&mut under, |(_, raw)| raw, folder.delimiter);
            Ok(Plan::Delete {
                top_raw: folder.name_raw.clone(),
                delimiter: folder.delimiter,
                folders: under,
            })
        }
    }
}

/// Sorts names so a folder comes before the folder above it.
pub(crate) fn deepest_first<T>(
    items: &mut [T],
    name: impl Fn(&T) -> &str,
    delimiter: Option<char>,
) {
    items.sort_by(|a, b| {
        let (a, b) = (name(a), name(b));
        folders::depth(b, delimiter)
            .cmp(&folders::depth(a, delimiter))
            .then_with(|| b.cmp(a))
    });
}

/// Changes one of a POP3 account's local folders at once; returns the raw messages a delete
/// left without a row, for the blob store.
pub(crate) async fn apply_local(
    store: &Store,
    account: AccountId,
    change: &FolderChange,
) -> Result<Vec<BlobKey>, FolderError> {
    let folders = known(store, account).await?;
    match plan(change, &folders, Wire::Local, |_, _| false)? {
        Plan::Create { name, .. } => {
            store
                .upsert_folder(FolderSpec::local(account, &name, FolderRole::Other))
                .recv()
                .await?;
            Ok(Vec::new())
        }
        Plan::Rename {
            from_raw,
            to_raw,
            renames,
            ..
        } => {
            if from_raw != to_raw {
                store.rename_folders(account, renames).recv().await?;
            }
            Ok(Vec::new())
        }
        Plan::Delete { folders, .. } => {
            let ids = folders.into_iter().map(|(id, _)| id).collect();
            Ok(store
                .delete_folders(account, ids)
                .recv()
                .await?
                .orphaned_blobs)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::folders::NameProblem;

    fn known(id: i64, name_raw: &str, role: FolderRole) -> Known {
        Known {
            id: FolderId(id),
            name: crate::imap::mutf7::display_name(name_raw),
            name_raw: name_raw.to_owned(),
            delimiter: Some('/'),
            role,
            inferiors: true,
        }
    }

    fn dotted(id: i64, name_raw: &str, role: FolderRole) -> Known {
        Known {
            delimiter: Some('.'),
            ..known(id, name_raw, role)
        }
    }

    fn account() -> Vec<Known> {
        vec![
            known(1, "INBOX", FolderRole::Inbox),
            known(2, "Sent", FolderRole::Sent),
            known(3, "Kunder", FolderRole::Other),
            known(4, "Kunder/Acme", FolderRole::Other),
            known(5, "Kunder/Acme/2026", FolderRole::Other),
            known(6, "Kunderservice", FolderRole::Other),
            known(7, "INBOX/Prosjekter", FolderRole::Other),
            Known {
                inferiors: false,
                ..known(8, "Arkiv 2019", FolderRole::Other)
            },
            known(9, "Prosjekter/Arkiv", FolderRole::Archive),
            known(10, "Prosjekter", FolderRole::Other),
        ]
    }

    fn never(_: &str, _: Option<char>) -> bool {
        false
    }

    fn create(parent: Option<i64>, name: &str) -> FolderChange {
        FolderChange::Create {
            parent: parent.map(FolderId),
            name: name.to_owned(),
        }
    }

    #[test]
    fn a_new_folder_goes_under_its_parent_encoded() {
        let folders = account();
        assert_eq!(
            plan(&create(Some(3), " Høst 📬 "), &folders, Wire::Imap, never).unwrap(),
            Plan::Create {
                name: "Kunder/Høst 📬".into(),
                name_raw: "Kunder/H&APg-st &2D3c7A-".into(),
            }
        );
        assert_eq!(
            plan(&create(None, "Reiser & ferie"), &folders, Wire::Imap, never).unwrap(),
            Plan::Create {
                name: "Reiser & ferie".into(),
                name_raw: "Reiser &- ferie".into(),
            },
            "at the top: the account's folders share no prefix"
        );
        assert_eq!(
            plan(
                &create(None, "Reiser & ferie"),
                &folders,
                Wire::Local,
                never
            )
            .unwrap(),
            Plan::Create {
                name: "Reiser & ferie".into(),
                name_raw: "Reiser & ferie".into(),
            }
        );
        assert_eq!(
            plan(
                &create(Some(1), "Kvitteringer"),
                &folders,
                Wire::Imap,
                never
            )
            .unwrap(),
            Plan::Create {
                name: "INBOX/Kvitteringer".into(),
                name_raw: "INBOX/Kvitteringer".into(),
            },
            "a role folder takes subfolders"
        );
    }

    #[test]
    fn at_the_top_of_an_inbox_server_new_folders_go_under_the_inbox() {
        let folders = vec![
            dotted(1, "INBOX", FolderRole::Inbox),
            dotted(2, "INBOX.Sent", FolderRole::Sent),
            dotted(3, "INBOX.Kunder", FolderRole::Other),
        ];
        assert_eq!(
            plan(&create(None, "Reiser"), &folders, Wire::Imap, never).unwrap(),
            Plan::Create {
                name: "INBOX.Reiser".into(),
                name_raw: "INBOX.Reiser".into(),
            }
        );
        assert!(matches!(
            plan(&create(None, "kunder"), &folders, Wire::Imap, never),
            Err(FolderError::Name(NameProblem::Taken))
        ));
        assert!(matches!(
            plan(&create(None, "Kunder.Acme"), &folders, Wire::Imap, never),
            Err(FolderError::Name(NameProblem::Delimiter('.')))
        ));
        // Only the Inbox: no telling, so the top.
        let alone = vec![dotted(1, "INBOX", FolderRole::Inbox)];
        assert_eq!(
            plan(&create(None, "Reiser"), &alone, Wire::Imap, never).unwrap(),
            Plan::Create {
                name: "Reiser".into(),
                name_raw: "Reiser".into(),
            }
        );
    }

    #[test]
    fn refusals_before_anything_is_sent() {
        let folders = account();
        let refused = |change: FolderChange| plan(&change, &folders, Wire::Imap, never);
        assert!(matches!(
            refused(create(Some(8), "Juni")),
            Err(FolderError::NotAllowed)
        ));
        assert!(matches!(
            refused(create(Some(99), "Juni")),
            Err(FolderError::Missing)
        ));
        assert!(matches!(
            refused(create(Some(3), "acme")),
            Err(FolderError::Name(NameProblem::Taken))
        ));
        assert!(matches!(
            refused(create(None, "kunder")),
            Err(FolderError::Name(NameProblem::Taken))
        ));
        assert!(matches!(
            refused(create(Some(3), "")),
            Err(FolderError::Name(NameProblem::Empty))
        ));
        for folder in [1, 2, 9, 10] {
            assert!(
                matches!(
                    refused(FolderChange::Rename {
                        folder: FolderId(folder),
                        name: "Nytt".into()
                    }),
                    Err(FolderError::NotAllowed)
                ),
                "{folder}: a role folder, or one holding one"
            );
            assert!(matches!(
                refused(FolderChange::Delete {
                    folder: FolderId(folder)
                }),
                Err(FolderError::NotAllowed)
            ));
        }
        let hidden = plan(
            &create(None, "Sync Issues"),
            &folders,
            Wire::Imap,
            |name, _| name == "Sync Issues",
        );
        assert!(matches!(
            hidden,
            Err(FolderError::Name(NameProblem::Hidden))
        ));
    }

    #[test]
    fn a_rename_takes_the_folders_under_it_along() {
        let folders = account();
        let planned = plan(
            &FolderChange::Rename {
                folder: FolderId(3),
                name: "Klienter æ".into(),
            },
            &folders,
            Wire::Imap,
            never,
        )
        .unwrap();
        let Plan::Rename {
            from_raw,
            to_raw,
            renames,
            before,
            ..
        } = planned
        else {
            panic!("a rename");
        };
        assert_eq!(
            (from_raw.as_str(), to_raw.as_str()),
            ("Kunder", "Klienter &AOY-")
        );
        let names: Vec<(i64, &str, &str)> = renames
            .iter()
            .map(|rename| {
                (
                    rename.folder.0,
                    rename.name.as_str(),
                    rename.name_raw.as_str(),
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                (3, "Klienter æ", "Klienter &AOY-"),
                (4, "Klienter æ/Acme", "Klienter &AOY-/Acme"),
                (5, "Klienter æ/Acme/2026", "Klienter &AOY-/Acme/2026"),
            ]
        );
        assert_eq!(before, ["Kunder", "Kunder/Acme", "Kunder/Acme/2026"]);
        // A sibling's name is taken; the folder's own isn't, in another case.
        let rename = |folder: i64, name: &str| {
            plan(
                &FolderChange::Rename {
                    folder: FolderId(folder),
                    name: name.into(),
                },
                &folders,
                Wire::Imap,
                never,
            )
        };
        assert!(matches!(
            rename(3, "kunderservice"),
            Err(FolderError::Name(NameProblem::Taken))
        ));
        assert!(matches!(rename(4, "ACME"), Ok(Plan::Rename { .. })));
    }

    #[test]
    fn a_delete_goes_deepest_first() {
        let folders = account();
        let planned = plan(
            &FolderChange::Delete {
                folder: FolderId(3),
            },
            &folders,
            Wire::Imap,
            never,
        )
        .unwrap();
        assert_eq!(
            planned,
            Plan::Delete {
                top_raw: "Kunder".into(),
                delimiter: Some('/'),
                folders: vec![
                    (FolderId(5), "Kunder/Acme/2026".into()),
                    (FolderId(4), "Kunder/Acme".into()),
                    (FolderId(3), "Kunder".into()),
                ],
            }
        );
    }
}
