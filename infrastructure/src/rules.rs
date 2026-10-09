//! The accounts' rules on disk (ADR-020): one Sieve script per account in
//! `$XDG_CONFIG_HOME/sealwax/rules/<account key>.sieve`. A missing file is no rules. Writes
//! replace the file atomically, 0600 in a 0700 folder, and keep a symbolic link (as
//! `accounts.toml`'s do); reads take at most 1 MiB of UTF-8 text, the most a script may be.

use crate::accounts;
use crate::settings_files::read_optional;
use crate::xdg::SealwaxDirs;
use sealwax_domain::account::AccountKey;
use sealwax_domain::accounts_file::is_valid_key;
use std::fs;
use std::io;
use std::path::PathBuf;

pub const RULES_DIR: &str = "rules";
pub const EXTENSION: &str = "sieve";

/// The account's script, or `None` for a key that can't name a file (never one Sealwax made).
pub fn path(dirs: &SealwaxDirs, key: &AccountKey) -> Option<PathBuf> {
    is_valid_key(&key.0).then(|| {
        dirs.config
            .join(RULES_DIR)
            .join(format!("{}.{EXTENSION}", key.0))
    })
}

/// The account's script; `None` when it has no file.
pub fn read(dirs: &SealwaxDirs, key: &AccountKey) -> io::Result<Option<String>> {
    match path(dirs, key) {
        Some(path) => read_optional(&path),
        None => Ok(None),
    }
}

/// Replaces the account's script with `text`.
pub fn write(dirs: &SealwaxDirs, key: &AccountKey, text: &str) -> io::Result<()> {
    let path = path(dirs, key).ok_or_else(invalid_key)?;
    accounts::replace(&path, text)
}

/// Removes the account's script, as when the account goes; no file is not an error.
pub fn remove(dirs: &SealwaxDirs, key: &AccountKey) -> io::Result<()> {
    let Some(path) = path(dirs, key) else {
        return Ok(());
    };
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

fn invalid_key() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "not an account key")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn key(text: &str) -> AccountKey {
        AccountKey(text.into())
    }

    fn mode(path: &std::path::Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_script_is_written_privately_read_back_and_removed() {
        let root = tempfile::tempdir().unwrap();
        let dirs = SealwaxDirs::under(root.path());
        let work = key("work");
        assert_eq!(read(&dirs, &work).unwrap(), None, "no file, no rules");
        let script = "require \"fileinto\";\nif header :contains \"list-id\" \"rust\" { fileinto \"Rust\"; }\n";
        write(&dirs, &work, script).unwrap();
        let path = path(&dirs, &work).unwrap();
        assert!(path.ends_with("config/sealwax/rules/work.sieve"));
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        assert_eq!(read(&dirs, &work).unwrap().as_deref(), Some(script));
        write(&dirs, &work, "keep;\n").unwrap();
        assert_eq!(read(&dirs, &work).unwrap().as_deref(), Some("keep;\n"));
        remove(&dirs, &work).unwrap();
        assert_eq!(read(&dirs, &work).unwrap(), None);
        remove(&dirs, &work).unwrap();
    }

    #[test]
    fn keys_that_cant_name_a_file_are_refused() {
        let root = tempfile::tempdir().unwrap();
        let dirs = SealwaxDirs::under(root.path());
        for bad in ["../accounts", "a/b", "", "Work"] {
            assert_eq!(path(&dirs, &key(bad)), None, "{bad:?}");
            assert_eq!(read(&dirs, &key(bad)).unwrap(), None);
            assert!(write(&dirs, &key(bad), "keep;").is_err());
            remove(&dirs, &key(bad)).unwrap();
        }
        assert!(!dirs.config.exists(), "nothing written");
    }

    #[test]
    fn a_script_over_a_mebibyte_or_not_utf8_is_an_error() {
        let root = tempfile::tempdir().unwrap();
        let dirs = SealwaxDirs::under(root.path());
        let big = key("big");
        let path = path(&dirs, &big).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, vec![b'#'; 1024 * 1024 + 1]).unwrap();
        assert!(read(&dirs, &big).is_err());
        fs::write(&path, [0xff, 0xfe]).unwrap();
        assert!(read(&dirs, &big).is_err());
    }
}
