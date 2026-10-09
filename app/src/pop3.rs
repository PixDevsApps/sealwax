//! Where the app treats a POP3 account (M8, ML-088) differently from an IMAP one: its folders
//! are local. Sealwax downloads its mail into them and no server holds them, so the list never
//! asks for older mail ("Loading older messages…") and the engine isn't told such a folder was
//! opened. Everything else is shared: triage, Move to… and Mark Folder as Read go through the
//! store's op queue as for IMAP, and the account's actor applies them locally; the sidebar and
//! the banners use the same words.
//!
//! Which accounts receive by POP3 comes from `accounts.toml`, matched to the store's accounts
//! by key. Before the file is read, or for an account it doesn't list, a folder counts as a
//! server's, so the engine answers for it.

use crate::application::AppState;
use sealwax_domain::account::{AccountSettings, Receive};
use sealwax_domain::model::{Account, AccountId, Folder, FolderId};

/// Whether the store's `account` receives its mail by POP3, as `listed` (`accounts.toml`) says.
pub fn receives_by_pop3(
    account: AccountId,
    accounts: &[Account],
    listed: &[AccountSettings],
) -> bool {
    accounts
        .iter()
        .find(|known| known.id == account)
        .and_then(|known| known.key.as_ref())
        .and_then(|key| listed.iter().find(|settings| &settings.key == key))
        .is_some_and(|settings| matches!(settings.receive, Receive::Pop3(_)))
}

/// Whether `folder` is one of a POP3 account's local folders.
pub fn is_local(
    folder: FolderId,
    folders: &[Folder],
    accounts: &[Account],
    listed: &[AccountSettings],
) -> bool {
    folders
        .iter()
        .find(|known| known.id == folder)
        .is_some_and(|known| receives_by_pop3(known.account, accounts, listed))
}

/// [`is_local`] for the app's store and its `accounts.toml`.
pub fn is_local_folder(state: &AppState, folder: FolderId) -> bool {
    let Some(prefs) = state.preferences() else {
        return false;
    };
    let mailbox = state.mailbox();
    let Some(known) = mailbox.folder(folder) else {
        return false;
    };
    receives_by_pop3(known.account, &mailbox.accounts(), &prefs.accounts())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::account::{
        AccountKey, AuthMethod, Endpoint, Pop3Policy, Receive, Security,
    };
    use sealwax_domain::model::FolderRole;

    fn settings(key: &str, receive: Receive) -> AccountSettings {
        let endpoint = |host: &str| Endpoint {
            host: host.to_owned(),
            port: 995,
            security: Security::Tls,
        };
        AccountSettings {
            key: AccountKey(key.to_owned()),
            name: key.to_owned(),
            label: None,
            color: None,
            address: format!("{key}@example.test"),
            incoming: endpoint("pop.example.test"),
            receive,
            smtp: endpoint("smtp.example.test"),
            username: key.to_owned(),
            auth: AuthMethod::Password,
            signature: None,
            identities: Vec::new(),
        }
    }

    fn account(id: i64, key: Option<&str>) -> Account {
        Account {
            id: AccountId(id),
            name: format!("Account {id}"),
            address: format!("a{id}@example.test"),
            key: key.map(|key| AccountKey(key.to_owned())),
        }
    }

    fn folder(id: i64, account: i64) -> Folder {
        Folder {
            id: FolderId(id),
            account: AccountId(account),
            name: "Inbox".to_owned(),
            delimiter: Some('/'),
            role: FolderRole::Inbox,
            selectable: true,
            inferiors: true,
            total: 0,
            unread: 0,
        }
    }

    #[test]
    fn only_a_pop3_accounts_folders_are_local() {
        let listed = [
            settings("work", Receive::Imap),
            settings("home", Receive::Pop3(Pop3Policy::default())),
        ];
        let accounts = [
            account(1, Some("work")),
            account(2, Some("home")),
            account(3, None),
            account(4, Some("gone")),
        ];
        let folders = [folder(10, 1), folder(20, 2), folder(30, 3), folder(40, 4)];
        let local: Vec<i64> = folders
            .iter()
            .filter(|known| is_local(known.id, &folders, &accounts, &listed))
            .map(|known| known.id.0)
            .collect();
        // IMAP's, a keyless fixture account's and one the file doesn't list keep the server's
        // behaviour; an unknown folder too.
        assert_eq!(local, [20]);
        assert!(!is_local(FolderId(99), &folders, &accounts, &listed));
        assert!(receives_by_pop3(AccountId(2), &accounts, &listed));
        assert!(!receives_by_pop3(AccountId(2), &accounts, &[]));
    }
}
