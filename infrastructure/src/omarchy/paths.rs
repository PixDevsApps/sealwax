use crate::xdg;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Points Sealwax at another Omarchy state directory instead of `~/.local/state/omarchy`:
/// `<dir>/current/theme/colors.toml` and `<dir>/current/theme.name`, so tests and headless
/// checks can switch themes in a scratch directory. Must be absolute.
pub const STATE_DIR_ENV: &str = "SEALWAX_OMARCHY_STATE";

const THEME_COLOR_SCRIPT: &str = "usr/share/omarchy/bin/omarchy-theme-color";

/// Where the inputs of the theme live (INTEGRATIONS.md "Omarchy theme" and "Fonts").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmarchyPaths {
    /// Omarchy's state directory, which holds `current/`.
    pub state: PathBuf,
    /// `$XDG_CONFIG_HOME/omarchy`, where `omarchy display text size` writes `shell.toml`.
    pub config: PathBuf,
    /// `$XDG_CONFIG_HOME/fontconfig`, where `omarchy font set` writes `fonts.conf`.
    pub fontconfig: PathBuf,
}

impl OmarchyPaths {
    /// The live paths, or [`STATE_DIR_ENV`]'s directory for the state.
    pub fn from_env() -> Self {
        let home = std::env::home_dir().unwrap_or_default();
        let state = state_dir(std::env::var_os(STATE_DIR_ENV).as_deref(), &home);
        Self::new(state, &xdg::config_home())
    }

    pub fn new(state: PathBuf, config_home: &Path) -> Self {
        Self {
            state,
            config: config_home.join("omarchy"),
            fontconfig: config_home.join("fontconfig"),
        }
    }

    /// `current/`: `theme/`, `theme.name` and `background`.
    pub fn current(&self) -> PathBuf {
        self.state.join("current")
    }

    pub fn colors_file(&self) -> PathBuf {
        self.current().join("theme").join("colors.toml")
    }

    pub fn theme_name_file(&self) -> PathBuf {
        self.current().join("theme.name")
    }

    /// The theme's generated `shell.toml`.
    pub fn theme_shell_file(&self) -> PathBuf {
        self.current().join("theme").join("shell.toml")
    }

    /// The user's machine-level `shell.toml`, merged over the theme's.
    pub fn user_shell_file(&self) -> PathBuf {
        self.config.join("shell.toml")
    }
}

/// `~/.local/state/omarchy`, or `override_dir` when it is an absolute path. Omarchy's
/// scripts write `$HOME/.local/state` whatever `$XDG_STATE_HOME` says, so Sealwax reads there.
pub fn state_dir(override_dir: Option<&OsStr>, home: &Path) -> PathBuf {
    match override_dir.map(Path::new) {
        Some(dir) if dir.is_absolute() => dir.to_path_buf(),
        _ => home.join(".local/state/omarchy"),
    }
}

/// Omarchy's resolver script, when installed.
pub fn theme_color_script() -> Option<PathBuf> {
    theme_color_script_in(Path::new("/"))
}

/// The resolver script under `root` (`/` outside tests), when it is a file.
pub fn theme_color_script_in(root: &Path) -> Option<PathBuf> {
    let script = root.join(THEME_COLOR_SCRIPT);
    script.is_file().then_some(script)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn the_state_dir_is_under_home_unless_overridden() {
        let home = Path::new("/home/user");
        assert_eq!(
            state_dir(None, home),
            Path::new("/home/user/.local/state/omarchy")
        );
        assert_eq!(
            state_dir(Some(OsStr::new("/tmp/scratch/omarchy")), home),
            Path::new("/tmp/scratch/omarchy")
        );
        for relative in ["", "scratch"] {
            assert_eq!(
                state_dir(Some(OsStr::new(relative)), home),
                Path::new("/home/user/.local/state/omarchy")
            );
        }
    }

    #[test]
    fn files_are_where_omarchy_writes_them() {
        let paths = OmarchyPaths::new("/s/omarchy".into(), Path::new("/c"));
        assert_eq!(paths.current(), Path::new("/s/omarchy/current"));
        assert_eq!(
            paths.colors_file(),
            Path::new("/s/omarchy/current/theme/colors.toml")
        );
        assert_eq!(
            paths.theme_name_file(),
            Path::new("/s/omarchy/current/theme.name")
        );
        assert_eq!(
            paths.theme_shell_file(),
            Path::new("/s/omarchy/current/theme/shell.toml")
        );
        assert_eq!(paths.user_shell_file(), Path::new("/c/omarchy/shell.toml"));
        assert_eq!(paths.fontconfig, Path::new("/c/fontconfig"));
        assert!(OmarchyPaths::from_env().current().ends_with("current"));
    }

    #[test]
    fn the_script_must_be_a_file() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(theme_color_script_in(root.path()), None);
        let script = root.path().join(THEME_COLOR_SCRIPT);
        fs::create_dir_all(&script).unwrap();
        assert_eq!(theme_color_script_in(root.path()), None);
        fs::remove_dir(&script).unwrap();
        fs::write(&script, "#!/bin/sh\n").unwrap();
        assert_eq!(theme_color_script_in(root.path()), Some(script));
    }
}
