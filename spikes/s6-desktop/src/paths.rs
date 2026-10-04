//! Where the spike keeps its files: everything lives under the git-ignored `target/s6/`.

use std::path::PathBuf;

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("target/s6"))
}

pub fn data_dir() -> PathBuf {
    repo_root().join("target/s6/data")
}

pub fn default_db() -> PathBuf {
    data_dir().join("s6-inbox.sqlite")
}

pub fn results_dir() -> PathBuf {
    repo_root().join("target/s6/results")
}

/// Scratch XDG folders for live runs, so nothing a run writes lands in the user's home.
pub fn live_xdg() -> PathBuf {
    repo_root().join("target/s6/xdg-live")
}

pub fn app_binary() -> PathBuf {
    let release = repo_root().join("target/s6/release/email-spike-s6");
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("email-spike-s6")))
        .filter(|path| path.exists())
        .unwrap_or(release)
}

pub fn evidence_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("evidence")
}
