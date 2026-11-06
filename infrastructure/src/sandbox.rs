//! Whether Sealwax runs in a Flatpak, and the host's folders it then reads (INTEGRATIONS.md
//! "Flatpak"). Flatpak puts `/.flatpak-info` at the root of every sandbox it starts, and sets
//! `XDG_CONFIG_HOME` to the app's own `~/.var/app/<app-id>/config`; the host's configuration
//! folder, where Omarchy and Hyprland keep theirs, is `HOST_XDG_CONFIG_HOME` (flatpak-run(1)).
//! Outside a Flatpak nothing here changes what Sealwax reads.

use crate::xdg;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The file Flatpak writes into every sandbox.
pub const FLATPAK_INFO: &str = "/.flatpak-info";

/// The host's `XDG_CONFIG_HOME`, set by Flatpak in the sandbox when the host had one.
pub const HOST_CONFIG_HOME_ENV: &str = "HOST_XDG_CONFIG_HOME";

/// Whether this process runs in a Flatpak sandbox; looked at once.
pub fn is_flatpak() -> bool {
    static FLATPAK: OnceLock<bool> = OnceLock::new();
    *FLATPAK.get_or_init(|| Path::new(FLATPAK_INFO).is_file())
}

/// The configuration folder of the user's desktop: `~/.config` or the host's
/// `XDG_CONFIG_HOME` in a Flatpak, [`xdg::config_home`] outside one.
pub fn desktop_config_home() -> PathBuf {
    if !is_flatpak() {
        return xdg::config_home();
    }
    let home = std::env::home_dir().unwrap_or_default();
    host_config_home(std::env::var_os(HOST_CONFIG_HOME_ENV).as_deref(), &home)
}

/// In a Flatpak: the host's `XDG_CONFIG_HOME` when it is absolute, else `home/.config`.
pub fn host_config_home(host_value: Option<&OsStr>, home: &Path) -> PathBuf {
    xdg::base_dir(host_value, home, ".config")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_config_home_is_never_the_apps_own() {
        let home = Path::new("/home/kari");
        assert_eq!(
            host_config_home(None, home),
            Path::new("/home/kari/.config")
        );
        assert_eq!(
            host_config_home(Some(OsStr::new("/home/kari/dotfiles")), home),
            Path::new("/home/kari/dotfiles")
        );
        for unusable in ["", "relative/config"] {
            assert_eq!(
                host_config_home(Some(OsStr::new(unusable)), home),
                Path::new("/home/kari/.config")
            );
        }
    }

    #[test]
    fn outside_a_flatpak_the_desktop_config_is_xdgs() {
        if !Path::new(FLATPAK_INFO).exists() {
            assert!(!is_flatpak());
            assert_eq!(desktop_config_home(), xdg::config_home());
        }
    }
}
