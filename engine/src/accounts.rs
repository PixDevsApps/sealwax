//! Adding and removing accounts (ML-034, ML-038), for the engine service's
//! `Command::AddAccount` and `Command::RemoveAccount`.
//!
//! `accounts.toml` is what makes an account exist; the store is a cache of it and the keyring
//! holds its secret. So:
//!
//! - **Add:** journal `add <key>`, then the secret to the keyring, then the entry to
//!   `accounts.toml` (the commit point), then the store row, then clear the journal. A failure
//!   undoes the earlier steps. After a crash, `recover` deletes the secret of a journalled key
//!   that `accounts.toml` doesn't list, and `reconcile` creates a missing store row, so no
//!   half-added account is left.
//! - **Remove:** journal `remove <key>`, then the entry (the commit point: when it fails,
//!   nothing changed and the journal is cleared), the store rows (one transaction), the blobs no
//!   other account refers to, the secrets, then clear the journal. Every step can run again, so
//!   a later failure or a crash leaves the removal journalled and `recover` finishes it.
//!
//! The journal holds keys only. Nothing here logs an address or a secret.

use crate::api::{AccountError, Password};
use crate::secrets::{SecretKind, SecretStore};
use sealwax_domain::account::{AccountColor, AccountKey, AccountSettings, AuthMethod};
use sealwax_domain::accounts_file;
use sealwax_domain::model::AccountId;
use sealwax_infrastructure::accounts::{
    self as files, AccountPaths, AccountsFileError, Pending, PendingOp,
};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{NewAccount, Store};

/// Runs `work` on tokio's blocking pool: file writes fsync.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, AccountError> {
    tokio::task::spawn_blocking(work).await.map_err(|error| {
        tracing::error!(%error, "an account file task failed");
        AccountError::SettingsFile
    })
}

fn file_error(error: AccountsFileError) -> AccountError {
    match error {
        AccountsFileError::AddressExists | AccountsFileError::KeyExists => AccountError::Exists,
        other => {
            tracing::warn!(error = %other, "accounts.toml was not changed");
            AccountError::SettingsFile
        }
    }
}

/// The secret an account keeps in the keyring.
fn secret_kind(auth: AuthMethod) -> SecretKind {
    match auth {
        AuthMethod::Password => SecretKind::Password,
        AuthMethod::OAuth(_) => SecretKind::RefreshToken,
    }
}

/// What keyring browsers show for an account's secret.
pub(crate) fn secret_label(address: &str) -> String {
    format!("Sealwax: {address}")
}

/// What account operations work on. The engine service owns these and lends them per command.
pub struct Accounts<'a, S> {
    pub paths: &'a AccountPaths,
    pub store: &'a Store,
    /// The blob store orphaned raw messages are deleted from; `None` keeps them.
    pub blobs: Option<&'a BlobStore>,
    pub secrets: &'a S,
}

impl<S: SecretStore> Accounts<'_, S> {
    fn pending(&self) -> Pending {
        Pending::new(self.paths.pending.clone())
    }

    async fn journal(&self, op: Option<PendingOp>, key: &AccountKey) -> Result<(), AccountError> {
        let pending = self.pending();
        let key = key.clone();
        blocking(move || match op {
            Some(op) => pending.begin(op, &key),
            None => pending.finish(&key),
        })
        .await?
        .map_err(|error| {
            tracing::warn!(%error, "the account journal could not be written");
            AccountError::SettingsFile
        })
    }

    async fn listed(&self) -> Result<Vec<AccountSettings>, AccountsFileError> {
        let path = self.paths.settings.clone();
        blocking(move || files::read(&path))
            .await
            .map_err(|_| AccountsFileError::Busy)?
    }

    /// Saves a verified account: secret, `accounts.toml` entry, store row. An empty or taken
    /// `settings.key` gets a new random key. Returns the account's key and store id.
    pub async fn add(
        &self,
        mut settings: AccountSettings,
        secret: &Password,
    ) -> Result<(AccountKey, AccountId), AccountError> {
        let listed = self.listed().await.map_err(file_error)?;
        if listed
            .iter()
            .any(|known| known.address.eq_ignore_ascii_case(&settings.address))
        {
            return Err(AccountError::Exists);
        }
        while settings.key.0.is_empty() || listed.iter().any(|known| known.key == settings.key) {
            settings.key = files::new_key().map_err(|error| {
                tracing::error!(%error, "no random key");
                AccountError::SettingsFile
            })?;
        }
        if let Err(reason) = accounts_file::check(&settings) {
            tracing::warn!(reason, "the account to add is incomplete");
            return Err(AccountError::SettingsFile);
        }
        let key = settings.key.clone();
        self.journal(Some(PendingOp::Add), &key).await?;

        let label = secret_label(&settings.address);
        if let Err(error) = self
            .secrets
            .store(
                &key.0,
                secret_kind(settings.auth),
                &label,
                secret.expose().as_bytes(),
            )
            .await
        {
            tracing::warn!(%error, "the keyring refused the new account's secret");
            self.journal(None, &key).await?;
            return Err(AccountError::Keyring);
        }

        let path = self.paths.settings.clone();
        let entry = settings.clone();
        let written = blocking(move || files::add(&path, &entry)).await?;
        if let Err(error) = written {
            self.forget_secret(&key).await;
            return Err(file_error(error));
        }

        let row = NewAccount {
            name: settings.title().to_owned(),
            address: settings.address.clone(),
        };
        let id = match self.store.ensure_account(key.clone(), row).recv().await {
            Ok(id) => id.value,
            Err(error) => {
                tracing::warn!(%error, "the new account's store row failed; undoing the add");
                let path = self.paths.settings.clone();
                let undo = key.clone();
                match blocking(move || files::remove(&path, &undo)).await? {
                    Ok(_) => self.forget_secret(&key).await,
                    // The entry stays with its secret: a complete account, whose store row
                    // `reconcile` creates later.
                    Err(error) => {
                        tracing::warn!(%error, "the new entry could not be taken out again");
                        let _ = self.journal(None, &key).await;
                    }
                }
                return Err(AccountError::Store);
            }
        };
        if self.journal(None, &key).await.is_err() {
            tracing::warn!("the finished add stays journalled; recovery will clear it");
        }
        tracing::info!(account = id.0, "account added");
        Ok((key, id))
    }

    /// Deletes a secret stored by an add that failed later; when the keyring refuses, the add
    /// stays journalled so `recover` deletes it.
    async fn forget_secret(&self, key: &AccountKey) {
        match self.secrets.delete_account(&key.0).await {
            Ok(()) => {
                let _ = self.journal(None, key).await;
            }
            Err(error) => {
                tracing::warn!(%error, "the failed add's secret stays until recovery");
            }
        }
    }

    /// Removes the account with `key`: its entry, store rows, orphaned blobs and secrets.
    /// Sets the account's `label` and `color` lines in `accounts.toml` (`None` takes one
    /// out), keeping the rest of the file. The store's name follows with `reconcile`, once the
    /// app has read the file.
    pub async fn edit(
        &self,
        key: &AccountKey,
        label: Option<String>,
        color: Option<AccountColor>,
    ) -> Result<(), AccountError> {
        let path = self.paths.settings.clone();
        let key = key.clone();
        match blocking(move || files::relabel(&path, &key, label.as_deref(), color)).await? {
            Ok(true) => Ok(()),
            Ok(false) => Err(AccountError::Missing),
            Err(error) => Err(file_error(error)),
        }
    }

    pub async fn remove(&self, key: &AccountKey) -> Result<(), AccountError> {
        let in_file = match self.listed().await {
            Ok(listed) => listed.iter().any(|known| &known.key == key),
            // A file with mistakes may still list it; the removal edits it if it does.
            Err(_) => true,
        };
        let in_store = self
            .store
            .account_by_key(key.clone())
            .recv()
            .await
            .map_err(|_| AccountError::Store)?
            .value
            .is_some();
        if !in_file && !in_store {
            return Err(AccountError::Missing);
        }
        self.journal(Some(PendingOp::Remove), key).await?;
        // Until the entry is out of the file nothing has changed: a failure there ends the
        // removal instead of leaving it journalled for a later start.
        if let Err(error) = self.remove_entry(key).await {
            let _ = self.journal(None, key).await;
            return Err(error);
        }
        self.finish_removal(key).await
    }

    async fn remove_entry(&self, key: &AccountKey) -> Result<(), AccountError> {
        let path = self.paths.settings.clone();
        let entry = key.clone();
        blocking(move || files::remove(&path, &entry))
            .await?
            .map(|_| ())
            .map_err(file_error)
    }

    /// The removal from the entry on; every step can run again.
    async fn finish_removal(&self, key: &AccountKey) -> Result<(), AccountError> {
        self.remove_entry(key).await?;
        let account = self
            .store
            .account_by_key(key.clone())
            .recv()
            .await
            .map_err(|_| AccountError::Store)?
            .value;
        if let Some(account) = account {
            let removed = self
                .store
                .remove_account(account.id)
                .recv()
                .await
                .map_err(|error| {
                    tracing::warn!(%error, "the account's rows could not be removed");
                    AccountError::Store
                })?;
            let orphaned = removed.orphaned_blobs.len();
            if let Some(blobs) = self.blobs.cloned() {
                let keys = removed.orphaned_blobs;
                let failed = blocking(move || {
                    keys.iter()
                        .filter(|blob| blobs.remove(blob).is_err())
                        .count()
                })
                .await?;
                if failed > 0 {
                    tracing::warn!(
                        failed,
                        "some raw messages of a removed account stay on disk"
                    );
                }
            }
            tracing::info!(
                account = account.id.0,
                folders = removed.folders,
                messages = removed.messages,
                blobs = orphaned,
                "account removed"
            );
        }

        self.secrets.delete_account(&key.0).await.map_err(|error| {
            tracing::warn!(%error, "the removed account's secrets stay until recovery");
            AccountError::Keyring
        })?;
        self.journal(None, key).await
    }

    /// Finishes what a crash interrupted: an add whose entry never reached `accounts.toml` loses
    /// its secret, a confirmed removal is completed. Run it when the engine starts, before
    /// `reconcile`. A step that fails stays journalled for the next start.
    pub async fn recover(&self) -> Result<(), AccountError> {
        let pending = self.pending();
        let entries = blocking(move || pending.list()).await?.map_err(|error| {
            tracing::warn!(%error, "the account journal could not be read");
            AccountError::SettingsFile
        })?;
        let mut result = Ok(());
        for (op, key) in entries {
            let outcome = match op {
                PendingOp::Add => match self.listed().await {
                    Ok(listed) if listed.iter().any(|known| known.key == key) => {
                        self.journal(None, &key).await
                    }
                    Ok(_) => match self.secrets.delete_account(&key.0).await {
                        Ok(()) => self.journal(None, &key).await,
                        Err(_) => Err(AccountError::Keyring),
                    },
                    Err(error) => Err(file_error(error)),
                },
                PendingOp::Remove => self.finish_removal(&key).await,
            };
            if let Err(error) = outcome {
                tracing::warn!(%error, "an interrupted account operation stays journalled");
                result = Err(error);
            }
        }
        result
    }

    /// Makes the store match `accounts.toml`: a row for every entry (also after the store was
    /// deleted), names (each account's title: its label, else its address) and addresses
    /// updated, and accounts the file no longer lists detached.
    /// Returns the store ids in the order of `accounts`.
    pub async fn reconcile(
        &self,
        accounts: &[AccountSettings],
    ) -> Result<Vec<AccountId>, AccountError> {
        let rows = accounts
            .iter()
            .map(|account| {
                (
                    account.key.clone(),
                    NewAccount {
                        name: account.title().to_owned(),
                        address: account.address.clone(),
                    },
                )
            })
            .collect();
        self.store
            .reconcile_accounts(rows)
            .recv()
            .await
            .map(|ids| ids.value)
            .map_err(|error| {
                tracing::warn!(%error, "the store could not follow accounts.toml");
                AccountError::Store
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::{MemoryStore, Secret, SecretError};
    use sealwax_domain::account::{Endpoint, Security};
    use sealwax_domain::model::FolderRole;
    use sealwax_infrastructure::ingest::{RawMessage, ingest_raw};
    use sealwax_infrastructure::store::{FolderSpec, StoreOptions};
    use sealwax_infrastructure::xdg::SealwaxDirs;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// The in-memory store, refusing every call while `fail` is set.
    #[derive(Default)]
    struct Flaky {
        inner: MemoryStore,
        fail: AtomicBool,
        stored: AtomicUsize,
    }

    impl Flaky {
        fn check(&self) -> Result<(), SecretError> {
            if self.fail.load(Ordering::SeqCst) {
                Err(SecretError::Locked)
            } else {
                Ok(())
            }
        }

        async fn has(&self, key: &AccountKey) -> bool {
            self.inner
                .read(&key.0, SecretKind::Password)
                .await
                .unwrap()
                .is_some()
        }
    }

    impl SecretStore for Flaky {
        async fn store(
            &self,
            account: &str,
            kind: SecretKind,
            label: &str,
            secret: &[u8],
        ) -> Result<(), SecretError> {
            self.check()?;
            self.stored.fetch_add(1, Ordering::SeqCst);
            self.inner.store(account, kind, label, secret).await
        }

        async fn read(
            &self,
            account: &str,
            kind: SecretKind,
        ) -> Result<Option<Secret>, SecretError> {
            self.check()?;
            self.inner.read(account, kind).await
        }

        async fn delete(&self, account: &str, kind: SecretKind) -> Result<(), SecretError> {
            self.check()?;
            self.inner.delete(account, kind).await
        }

        async fn delete_account(&self, account: &str) -> Result<(), SecretError> {
            self.check()?;
            self.inner.delete_account(account).await
        }
    }

    struct Bench {
        _root: tempfile::TempDir,
        paths: AccountPaths,
        store: Store,
        blobs: BlobStore,
        secrets: Flaky,
    }

    impl Bench {
        fn new() -> Self {
            let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
            fs::create_dir_all(&base).unwrap();
            let root = tempfile::Builder::new()
                .prefix("accounts-")
                .tempdir_in(base)
                .unwrap();
            let dirs = SealwaxDirs::under(root.path());
            let blobs = BlobStore::open(&dirs.blobs_dir()).unwrap();
            let store = Store::open(
                &dirs.store_file(),
                StoreOptions {
                    blobs: Some(blobs.clone()),
                    ..StoreOptions::default()
                },
            )
            .unwrap();
            Self {
                _root: root,
                paths: AccountPaths::new(&dirs),
                store,
                blobs,
                secrets: Flaky::default(),
            }
        }

        fn accounts(&self) -> Accounts<'_, Flaky> {
            Accounts {
                paths: &self.paths,
                store: &self.store,
                blobs: Some(&self.blobs),
                secrets: &self.secrets,
            }
        }

        fn file(&self) -> String {
            fs::read_to_string(&self.paths.settings).unwrap_or_default()
        }

        fn journal(&self) -> Vec<(PendingOp, AccountKey)> {
            Pending::new(self.paths.pending.clone()).list().unwrap()
        }

        fn in_store(&self, key: &AccountKey) -> bool {
            self.store
                .account_by_key(key.clone())
                .recv_blocking()
                .unwrap()
                .value
                .is_some()
        }
    }

    fn settings(address: &str) -> AccountSettings {
        let endpoint = |host: &str, port| Endpoint {
            host: host.to_owned(),
            port,
            security: Security::Tls,
        };
        AccountSettings {
            key: AccountKey(String::new()),
            name: "Test Person".to_owned(),
            label: None,
            color: None,
            address: address.to_owned(),
            incoming: endpoint("imap.example.test", 993),
            receive: sealwax_domain::account::Receive::Imap,
            smtp: endpoint("smtp.example.test", 465),
            username: address.to_owned(),
            auth: AuthMethod::Password,
            signature: None,
            identities: Vec::new(),
        }
    }

    fn password() -> Password {
        Password::new("dummy-password-1".to_owned())
    }

    fn run<T>(future: impl Future<Output = T>) -> T {
        crate::runtime().unwrap().block_on(future)
    }

    #[test]
    fn add_then_remove_leaves_nothing() {
        let bench = Bench::new();
        let accounts = bench.accounts();
        let (key, id) = run(accounts.add(settings("ada@example.test"), &password())).unwrap();
        assert_eq!(key.0.len(), files::KEY_LENGTH);
        assert!(run(bench.secrets.has(&key)));
        assert!(bench.in_store(&key));
        assert_eq!(files::read(&bench.paths.settings).unwrap()[0].key, key);
        assert!(bench.journal().is_empty());

        let inbox = bench
            .store
            .upsert_folder(FolderSpec::local(id, "INBOX", FolderRole::Inbox))
            .recv_blocking()
            .unwrap()
            .value;
        let raw = b"From: a@example.test\r\nSubject: s\r\n\r\nbody\r\n".to_vec();
        ingest_raw(
            &bench.store,
            &bench.blobs,
            vec![RawMessage {
                folder: inbox,
                raw,
                ..RawMessage::default()
            }],
        )
        .unwrap();
        assert_eq!(bench.blobs.disk_usage().unwrap().files, 1);

        run(accounts.remove(&key)).unwrap();
        assert!(!run(bench.secrets.has(&key)));
        assert!(!bench.in_store(&key));
        assert_eq!(files::read(&bench.paths.settings).unwrap(), Vec::new());
        assert_eq!(bench.blobs.disk_usage().unwrap().files, 0);
        assert!(bench.journal().is_empty());
        assert_eq!(run(accounts.remove(&key)), Err(AccountError::Missing));
    }

    #[test]
    fn a_removal_that_cant_edit_the_file_changes_nothing() {
        let bench = Bench::new();
        let accounts = bench.accounts();
        let (key, _) = run(accounts.add(settings("ada@example.test"), &password())).unwrap();
        let good = bench.file();
        fs::write(&bench.paths.settings, format!("{good}[[account]\n")).unwrap();
        assert_eq!(run(accounts.remove(&key)), Err(AccountError::SettingsFile));
        assert!(
            bench.journal().is_empty(),
            "nothing is left to finish later"
        );
        assert!(bench.in_store(&key));
        assert!(run(bench.secrets.has(&key)));
        fs::write(&bench.paths.settings, &good).unwrap();
        run(accounts.remove(&key)).unwrap();
        assert!(!bench.in_store(&key));
    }

    #[test]
    fn a_refused_secret_adds_nothing() {
        let bench = Bench::new();
        bench.secrets.fail.store(true, Ordering::SeqCst);
        let result = run(bench
            .accounts()
            .add(settings("ada@example.test"), &password()));
        assert_eq!(result, Err(AccountError::Keyring));
        assert_eq!(bench.file(), "");
        assert!(bench.journal().is_empty());
        assert!(
            bench
                .store
                .accounts()
                .recv_blocking()
                .unwrap()
                .value
                .is_empty()
        );
    }

    #[test]
    fn a_failed_write_undoes_the_secret() {
        let bench = Bench::new();
        let dir = bench.paths.settings.parent().unwrap();
        fs::create_dir_all(dir).unwrap();
        // A dangling link reads as no file but can't be written through.
        std::os::unix::fs::symlink(dir.join("missing/accounts.toml"), &bench.paths.settings)
            .unwrap();
        let mut wanted = settings("ada@example.test");
        wanted.key = AccountKey("chosen".to_owned());
        let result = run(bench.accounts().add(wanted.clone(), &password()));
        assert_eq!(result, Err(AccountError::SettingsFile));
        assert_eq!(bench.secrets.stored.load(Ordering::SeqCst), 1);
        assert!(!run(bench.secrets.has(&wanted.key)), "and deleted again");
        assert!(bench.journal().is_empty());
        assert!(!bench.in_store(&wanted.key));

        fs::remove_file(&bench.paths.settings).unwrap();
        fs::write(&bench.paths.settings, "[[account]]\nkey = 1\n").unwrap();
        let result = run(bench.accounts().add(wanted.clone(), &password()));
        assert_eq!(
            result,
            Err(AccountError::SettingsFile),
            "a faulty file isn't added to"
        );
        assert_eq!(bench.file(), "[[account]]\nkey = 1\n");
        assert!(!run(bench.secrets.has(&wanted.key)));
    }

    #[test]
    fn an_address_is_added_once() {
        let bench = Bench::new();
        let accounts = bench.accounts();
        run(accounts.add(settings("ada@example.test"), &password())).unwrap();
        let file = bench.file();
        let again = run(accounts.add(settings("ADA@example.test"), &password()));
        assert_eq!(again, Err(AccountError::Exists));
        assert_eq!(bench.file(), file);
        let mut taken = settings("other@example.test");
        taken.key = files::read(&bench.paths.settings).unwrap()[0].key.clone();
        let (key, _) = run(accounts.add(taken.clone(), &password())).unwrap();
        assert_ne!(key, taken.key, "a taken key is replaced");
    }

    #[test]
    fn recovery_finishes_what_a_crash_interrupted() {
        let bench = Bench::new();
        let accounts = bench.accounts();
        // A crash after the secret was stored, before the entry was written.
        let orphan = AccountKey("orphan".to_owned());
        Pending::new(bench.paths.pending.clone())
            .begin(PendingOp::Add, &orphan)
            .unwrap();
        run(bench
            .secrets
            .store(&orphan.0, SecretKind::Password, "", b"x"))
        .unwrap();
        // A crash after the entry was written, before the journal was cleared.
        let (kept, _) = run(accounts.add(settings("kept@example.test"), &password())).unwrap();
        Pending::new(bench.paths.pending.clone())
            .begin(PendingOp::Add, &kept)
            .unwrap();
        // A removal whose keyring step failed.
        let (gone, _) = run(accounts.add(settings("gone@example.test"), &password())).unwrap();
        bench.secrets.fail.store(true, Ordering::SeqCst);
        assert_eq!(run(accounts.remove(&gone)), Err(AccountError::Keyring));
        assert!(!bench.in_store(&gone));
        assert_eq!(bench.journal().len(), 3);
        assert!(
            run(accounts.recover()).is_err(),
            "the keyring is still locked"
        );
        assert_eq!(bench.journal().len(), 2, "only the finished add is cleared");

        bench.secrets.fail.store(false, Ordering::SeqCst);
        run(accounts.recover()).unwrap();
        assert!(bench.journal().is_empty());
        assert!(!run(bench.secrets.has(&orphan)));
        assert!(run(bench.secrets.has(&kept)));
        assert!(!run(bench.secrets.has(&gone)));
        let listed = files::read(&bench.paths.settings).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].key, kept);
    }

    #[test]
    fn reconcile_recreates_rows_after_a_deleted_store() {
        let bench = Bench::new();
        let (key, _) = run(bench
            .accounts()
            .add(settings("ada@example.test"), &password()))
        .unwrap();
        let listed = files::read(&bench.paths.settings).unwrap();
        let fresh = Bench::new();
        let accounts = Accounts {
            paths: &bench.paths,
            store: &fresh.store,
            blobs: None,
            secrets: &bench.secrets,
        };
        let ids = run(accounts.reconcile(&listed)).unwrap();
        assert_eq!(ids.len(), 1);
        let row = fresh
            .store
            .account_by_key(key.clone())
            .recv_blocking()
            .unwrap()
            .value
            .unwrap();
        assert_eq!((row.id, row.key), (ids[0], Some(key)));
        run(accounts.reconcile(&[])).unwrap();
        assert!(
            fresh
                .store
                .accounts()
                .recv_blocking()
                .unwrap()
                .value
                .is_empty(),
            "an account the file no longer lists is detached"
        );
    }

    #[test]
    fn the_store_names_an_account_by_its_label_else_its_address() {
        let bench = Bench::new();
        let accounts = bench.accounts();
        let (key, id) = run(accounts.add(settings("ada@example.test"), &password())).unwrap();
        let name = |store: &Store| {
            store
                .accounts()
                .recv_blocking()
                .unwrap()
                .value
                .into_iter()
                .find(|account| account.id == id)
                .map(|account| account.name)
        };
        assert_eq!(
            name(&bench.store).as_deref(),
            Some("ada@example.test"),
            "not the sender name"
        );
        let mut listed = files::read(&bench.paths.settings).unwrap();
        assert_eq!(listed[0].key, key);
        listed[0].label = Some("Work".to_owned());
        run(accounts.reconcile(&listed)).unwrap();
        assert_eq!(name(&bench.store).as_deref(), Some("Work"));
        listed[0].label = None;
        run(accounts.reconcile(&listed)).unwrap();
        assert_eq!(name(&bench.store).as_deref(), Some("ada@example.test"));
    }

    #[test]
    fn edit_writes_the_label_and_the_color() {
        let bench = Bench::new();
        let accounts = bench.accounts();
        let (key, _) = run(accounts.add(settings("ada@example.test"), &password())).unwrap();
        run(accounts.edit(&key, Some("Work".to_owned()), Some(AccountColor::Mauve))).unwrap();
        let listed = files::read(&bench.paths.settings).unwrap();
        assert_eq!(
            (listed[0].title(), listed[0].color),
            ("Work", Some(AccountColor::Mauve))
        );
        assert_eq!(listed[0].name, "Test Person", "the sender name stays");
        run(accounts.edit(&key, None, None)).unwrap();
        let listed = files::read(&bench.paths.settings).unwrap();
        assert_eq!((listed[0].label.as_deref(), listed[0].color), (None, None));
        assert_eq!(
            run(accounts.edit(&AccountKey("gone".into()), None, None)),
            Err(AccountError::Missing)
        );
    }
}
