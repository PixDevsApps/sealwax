//! Sealwax's own XDG directories (ADR-010): `$XDG_CONFIG_HOME/sealwax`, `$XDG_DATA_HOME/sealwax`,
//! `$XDG_STATE_HOME/sealwax` and `$XDG_CACHE_HOME/sealwax`. Directories Sealwax creates for
//! private data are mode 0700.

use std::ffi::OsStr;
use std::fs::{self, DirBuilder, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const APP_DIR: &str = "sealwax";

/// An XDG base directory: the variable when it holds an absolute path, else `home/fallback`.
pub fn base_dir(value: Option<&OsStr>, home: &Path, fallback: &str) -> PathBuf {
    match value.map(Path::new) {
        Some(dir) if dir.is_absolute() => dir.to_path_buf(),
        _ => home.join(fallback),
    }
}

fn from_env(variable: &str, fallback: &str) -> PathBuf {
    let home = std::env::home_dir().unwrap_or_default();
    base_dir(std::env::var_os(variable).as_deref(), &home, fallback)
}

pub fn config_home() -> PathBuf {
    from_env("XDG_CONFIG_HOME", ".config")
}

pub fn data_home() -> PathBuf {
    from_env("XDG_DATA_HOME", ".local/share")
}

pub fn state_home() -> PathBuf {
    from_env("XDG_STATE_HOME", ".local/state")
}

pub fn cache_home() -> PathBuf {
    from_env("XDG_CACHE_HOME", ".cache")
}

/// Where Sealwax keeps its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealwaxDirs {
    /// `config.toml`, `keys.toml` and `accounts.toml` (ADR-009).
    pub config: PathBuf,
    /// The message store and the blob store (ADR-005).
    pub data: PathBuf,
    /// Window and session state.
    pub state: PathBuf,
    /// Regenerable files.
    pub cache: PathBuf,
}

impl SealwaxDirs {
    pub fn from_env() -> Self {
        Self {
            config: config_home().join(APP_DIR),
            data: data_home().join(APP_DIR),
            state: state_home().join(APP_DIR),
            cache: cache_home().join(APP_DIR),
        }
    }

    /// Everything under one root, for tests and the self-test harness.
    pub fn under(root: &Path) -> Self {
        Self {
            config: root.join("config").join(APP_DIR),
            data: root.join("data").join(APP_DIR),
            state: root.join("state").join(APP_DIR),
            cache: root.join("cache").join(APP_DIR),
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }

    pub fn keys_file(&self) -> PathBuf {
        self.config.join("keys.toml")
    }

    pub fn accounts_file(&self) -> PathBuf {
        self.config.join("accounts.toml")
    }

    /// The accounts' rules, one Sieve script each (ADR-020).
    pub fn rules_dir(&self) -> PathBuf {
        self.config.join(crate::rules::RULES_DIR)
    }

    /// The window's layout between runs (`domain::layout`).
    pub fn layout_file(&self) -> PathBuf {
        self.state.join(sealwax_domain::layout::LAYOUT_FILE)
    }

    /// The SQLite store.
    pub fn store_file(&self) -> PathBuf {
        self.data.join("store.sqlite")
    }

    /// Raw messages, content-addressed.
    pub fn blobs_dir(&self) -> PathBuf {
        self.data.join("blobs")
    }
}

/// Creates `dir` (and missing parents) and makes `dir` itself mode 0700, also when it existed
/// with wider permissions.
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let mode = fs::metadata(dir)?.permissions().mode() & 0o777;
    if mode != 0o700 {
        fs::set_permissions(dir, Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_dirs_need_an_absolute_value() {
        let home = Path::new("/home/user");
        assert_eq!(
            base_dir(Some(OsStr::new("/srv/data")), home, ".local/share"),
            Path::new("/srv/data")
        );
        for value in [None, Some(OsStr::new("")), Some(OsStr::new("rel"))] {
            assert_eq!(
                base_dir(value, home, ".cache"),
                Path::new("/home/user/.cache")
            );
        }
    }

    #[test]
    fn dirs_under_a_root() {
        let dirs = SealwaxDirs::under(Path::new("/tmp/x"));
        assert_eq!(
            dirs.config_file(),
            Path::new("/tmp/x/config/sealwax/config.toml")
        );
        assert_eq!(
            dirs.keys_file(),
            Path::new("/tmp/x/config/sealwax/keys.toml")
        );
        assert_eq!(
            dirs.accounts_file(),
            Path::new("/tmp/x/config/sealwax/accounts.toml")
        );
        assert_eq!(
            dirs.store_file(),
            Path::new("/tmp/x/data/sealwax/store.sqlite")
        );
        assert_eq!(dirs.blobs_dir(), Path::new("/tmp/x/data/sealwax/blobs"));
        assert!(SealwaxDirs::from_env().data.ends_with("sealwax"));
    }

    #[test]
    fn private_dirs_are_0700() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a/b");
        ensure_private_dir(&dir).unwrap();
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );

        fs::set_permissions(&dir, Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&dir).unwrap();
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
