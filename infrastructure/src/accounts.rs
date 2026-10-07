//! `accounts.toml` on disk (ADR-009, ML-034), the account key, and the journal of account
//! operations that a crash may interrupt. Writes replace the file atomically (a temporary file
//! in the same folder, fsync, rename, fsync of the folder), the file 0600 in a 0700 folder.
//! Each write starts from the text on disk and changes only the one entry, and it is retried
//! when the file changed underneath, so the user's hand edits survive. Nothing here ever
//! writes a secret.

use crate::settings_files::read_optional;
use crate::xdg::{SealwaxDirs, ensure_private_dir};
use sealwax_domain::account::{AccountKey, AccountSettings};
use sealwax_domain::accounts_file::{self, EditError};
use sealwax_domain::config::FileError;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Characters of a generated key: 60 random bits as lowercase base32.
pub const KEY_LENGTH: usize = 12;
const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
/// How often a write starts over when the file changed while it was being written.
const ATTEMPTS: usize = 3;

/// Serializes this process's writes of `accounts.toml` and the journal.
static WRITES: Mutex<()> = Mutex::new(());
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A new random account key, such as `k3j5x8m2q9wa`.
pub fn new_key() -> io::Result<AccountKey> {
    let mut bytes = [0; 8];
    getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
    let mut bits = u64::from_le_bytes(bytes);
    let key = (0..KEY_LENGTH)
        .map(|_| {
            let character = BASE32[(bits & 31) as usize];
            bits >>= 5;
            char::from(character)
        })
        .collect();
    Ok(AccountKey(key))
}

/// Where the accounts live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountPaths {
    /// `$XDG_CONFIG_HOME/sealwax/accounts.toml`.
    pub settings: PathBuf,
    /// `$XDG_STATE_HOME/sealwax/accounts-pending`: additions and removals in progress.
    pub pending: PathBuf,
}

impl AccountPaths {
    pub fn new(dirs: &SealwaxDirs) -> Self {
        Self {
            settings: dirs.accounts_file(),
            pending: dirs.state.join("accounts-pending"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AccountsFileError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("accounts.toml has {} mistake(s); the first: {}", .0.len(), .0.first().map(ToString::to_string).unwrap_or_default())]
    Invalid(Vec<FileError>),
    #[error("the account can't be written: {0}")]
    Account(String),
    #[error("accounts.toml already has an account with this key")]
    KeyExists,
    #[error("accounts.toml already has an account with this address")]
    AddressExists,
    #[error("accounts.toml kept changing while Sealwax wrote it")]
    Busy,
}

impl From<EditError> for AccountsFileError {
    fn from(error: EditError) -> Self {
        match error {
            EditError::Invalid(errors) => Self::Invalid(errors),
            EditError::Account(reason) => Self::Account(reason),
            EditError::KeyExists => Self::KeyExists,
            EditError::AddressExists => Self::AddressExists,
        }
    }
}

/// The accounts in `path`; a missing file has none.
pub fn read(path: &Path) -> Result<Vec<AccountSettings>, AccountsFileError> {
    match read_optional(path)? {
        Some(text) => accounts_file::parse(&text).map_err(AccountsFileError::Invalid),
        None => Ok(Vec::new()),
    }
}

/// Adds `account` to `path`, creating the file with its explanatory header when missing.
pub fn add(path: &Path, account: &AccountSettings) -> Result<(), AccountsFileError> {
    edit(path, |text| {
        let text = text.map_or_else(accounts_file::default_accounts_file, str::to_owned);
        accounts_file::add(&text, account).map(Some)
    })
    .map(|_| ())
}

/// Removes the account with `key` from `path`; false when the file doesn't list it.
pub fn remove(path: &Path, key: &AccountKey) -> Result<bool, AccountsFileError> {
    edit(path, |text| match text {
        Some(text) => accounts_file::remove(text, key),
        None => Ok(None),
    })
}

/// Rewrites `path` with what `change` makes of its text (`None` inside: no file); `change`
/// returning `None` writes nothing. Returns whether it wrote.
fn edit(
    path: &Path,
    change: impl Fn(Option<&str>) -> Result<Option<String>, EditError>,
) -> Result<bool, AccountsFileError> {
    let _guard = WRITES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for _ in 0..ATTEMPTS {
        let before = read_optional(path)?;
        let Some(text) = change(before.as_deref())? else {
            return Ok(false);
        };
        if replace_unless_changed(path, &text, Some(before.as_deref()))? {
            return Ok(true);
        }
        tracing::debug!("accounts.toml changed while it was written; starting over");
    }
    Err(AccountsFileError::Busy)
}

/// The file a write replaces: the target when `path` is a symbolic link (dotfile managers link
/// their files), so the link stays.
fn write_target(path: &Path) -> io::Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => fs::canonicalize(path),
        _ => Ok(path.to_owned()),
    }
}

/// Atomically replaces `path` with `text`, mode 0600. Its folder is created 0700 when missing.
pub fn replace(path: &Path, text: &str) -> io::Result<()> {
    replace_unless_changed(path, text, None).map(|_| ())
}

/// `replace`, but only when the file still holds `expected` just before the rename (when
/// `expected` is given). Returns whether it replaced the file.
fn replace_unless_changed(
    path: &Path,
    text: &str,
    expected: Option<Option<&str>>,
) -> io::Result<bool> {
    let target = write_target(path)?;
    let dir = target
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_owned();
    if target == path {
        ensure_private_dir(&dir)?;
    }
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let written = write_synced(&temp, text.as_bytes()).and_then(|()| {
        if let Some(expected) = expected
            && read_optional(path)?.as_deref() != expected
        {
            return Ok(false);
        }
        fs::rename(&temp, &target)?;
        Ok(true)
    });
    match written {
        Ok(true) => {
            File::open(&dir)?.sync_all()?;
            Ok(true)
        }
        Ok(false) => {
            let _ = fs::remove_file(&temp);
            Ok(false)
        }
        Err(error) => {
            let _ = fs::remove_file(&temp);
            Err(error)
        }
    }
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// An account operation that must finish, or be undone, after a crash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingOp {
    /// The secret may be in the keyring before the entry is in `accounts.toml`: if the entry
    /// isn't there, the secret is deleted.
    Add,
    /// Removal was confirmed: it is finished.
    Remove,
}

impl PendingOp {
    fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Remove => "remove",
        }
    }
}

/// The journal of account operations in progress, one `add <key>` or `remove <key>` per line.
/// It holds keys only, never an address or a secret.
#[derive(Clone, Debug)]
pub struct Pending {
    path: PathBuf,
}

impl Pending {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The operations recorded and not finished, oldest first. Unreadable lines are skipped.
    pub fn list(&self) -> io::Result<Vec<(PendingOp, AccountKey)>> {
        let text = read_optional(&self.path)?.unwrap_or_default();
        Ok(text
            .lines()
            .filter_map(|line| {
                let (op, key) = line.split_once(' ')?;
                let op = match op {
                    "add" => PendingOp::Add,
                    "remove" => PendingOp::Remove,
                    _ => return None,
                };
                accounts_file::is_valid_key(key).then(|| (op, AccountKey(key.to_owned())))
            })
            .collect())
    }

    /// Records `op` on `key` durably, replacing what was recorded for that key.
    pub fn begin(&self, op: PendingOp, key: &AccountKey) -> io::Result<()> {
        self.update(key, Some(op))
    }

    /// Forgets what was recorded for `key`.
    pub fn finish(&self, key: &AccountKey) -> io::Result<()> {
        self.update(key, None)
    }

    fn update(&self, key: &AccountKey, op: Option<PendingOp>) -> io::Result<()> {
        let _guard = WRITES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut entries: Vec<_> = self
            .list()?
            .into_iter()
            .filter(|(_, known)| known != key)
            .collect();
        if let Some(op) = op {
            entries.push((op, key.clone()));
        }
        if entries.is_empty() {
            return match fs::remove_file(&self.path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            };
        }
        let text: String = entries
            .iter()
            .map(|(op, key)| format!("{} {key}\n", op.as_str()))
            .collect();
        replace(&self.path, &text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::account::{AuthMethod, Endpoint, Security};
    use std::os::unix::fs::symlink;

    fn scratch() -> tempfile::TempDir {
        crate::store::tests::scratch()
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn account(key: &str, address: &str) -> AccountSettings {
        let endpoint = |host: &str, port| Endpoint {
            host: host.to_owned(),
            port,
            security: Security::Tls,
        };
        AccountSettings {
            key: AccountKey(key.to_owned()),
            name: "Test Person".to_owned(),
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

    #[test]
    fn keys_are_random_base32() {
        let keys: Vec<_> = (0..200).map(|_| new_key().unwrap().0).collect();
        for key in &keys {
            assert_eq!(key.len(), KEY_LENGTH);
            assert!(accounts_file::is_valid_key(key), "{key}");
            assert!(key.bytes().all(|byte| BASE32.contains(&byte)));
        }
        let mut unique = keys.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), keys.len());
    }

    #[test]
    fn adds_and_removes_with_private_modes() {
        let root = scratch();
        let dirs = SealwaxDirs::under(root.path());
        let path = dirs.accounts_file();
        assert_eq!(read(&path).unwrap(), Vec::new());
        add(&path, &account("work", "work@example.test")).unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&dirs.config), 0o700);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(&accounts_file::default_accounts_file()));
        add(&path, &account("home", "home@example.test")).unwrap();
        assert_eq!(read(&path).unwrap().len(), 2);
        assert!(matches!(
            add(&path, &account("other", "HOME@example.test")),
            Err(AccountsFileError::AddressExists)
        ));
        assert!(remove(&path, &AccountKey("work".into())).unwrap());
        assert!(!remove(&path, &AccountKey("work".into())).unwrap());
        assert_eq!(read(&path).unwrap()[0].key.0, "home");
        let leftovers: Vec<_> = fs::read_dir(&dirs.config)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, ["accounts.toml"], "no temporary file is left");
        let missing = dirs.config.join("none.toml");
        assert!(!remove(&missing, &AccountKey("x".into())).unwrap());
        assert!(!missing.exists());
    }

    #[test]
    fn hand_edits_survive_and_faulty_files_stop_adding() {
        let root = scratch();
        let path = SealwaxDirs::under(root.path()).accounts_file();
        add(&path, &account("work", "work@example.test")).unwrap();
        let mut text = fs::read_to_string(&path).unwrap();
        text = text.replace(
            "name = \"Test Person\"",
            "name = \"Test\"  # shorter\n# my note",
        );
        fs::write(&path, &text).unwrap();
        add(&path, &account("home", "home@example.test")).unwrap();
        assert!(fs::read_to_string(&path).unwrap().starts_with(&text));

        fs::write(&path, format!("{text}\nport = 1\n")).unwrap();
        assert!(matches!(
            add(&path, &account("x", "x@example.test")),
            Err(AccountsFileError::Invalid(_))
        ));
        assert!(matches!(read(&path), Err(AccountsFileError::Invalid(_))));
    }

    #[test]
    fn writes_through_a_symbolic_link() {
        let root = scratch();
        let dirs = SealwaxDirs::under(root.path());
        let dotfiles = root.path().join("dotfiles");
        fs::create_dir_all(&dotfiles).unwrap();
        fs::create_dir_all(&dirs.config).unwrap();
        let real = dotfiles.join("accounts.toml");
        fs::write(&real, "# mine\n").unwrap();
        symlink(&real, dirs.accounts_file()).unwrap();
        add(&dirs.accounts_file(), &account("work", "work@example.test")).unwrap();
        assert!(
            fs::symlink_metadata(dirs.accounts_file())
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(fs::read_to_string(&real).unwrap().starts_with("# mine\n"));
        assert_eq!(read(&dirs.accounts_file()).unwrap().len(), 1);
    }

    #[test]
    fn the_journal_keeps_keys_only() {
        let root = scratch();
        let paths = AccountPaths::new(&SealwaxDirs::under(root.path()));
        let pending = Pending::new(paths.pending.clone());
        assert_eq!(pending.list().unwrap(), Vec::new());
        let (a, b) = (AccountKey("aaaa".into()), AccountKey("bbbb".into()));
        pending.begin(PendingOp::Add, &a).unwrap();
        pending.begin(PendingOp::Remove, &b).unwrap();
        assert_eq!(mode(&paths.pending), 0o600);
        pending.begin(PendingOp::Remove, &a).unwrap();
        assert_eq!(
            pending.list().unwrap(),
            [
                (PendingOp::Remove, b.clone()),
                (PendingOp::Remove, a.clone())
            ]
        );
        assert_eq!(
            fs::read_to_string(&paths.pending).unwrap(),
            "remove bbbb\nremove aaaa\n"
        );
        pending.finish(&a).unwrap();
        pending.finish(&b).unwrap();
        assert!(!paths.pending.exists());
        pending.finish(&b).unwrap();
    }
}
