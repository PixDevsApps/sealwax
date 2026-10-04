//! Readers for what the theme follows (ADR-004, ADR-019): the Omarchy palette, the
//! fontconfig families, `shell.toml` and Hyprland's rounding. Everything here only reads;
//! nothing is ever written under `~/.config` or `~/.local/state/omarchy`.

pub mod colors;
mod command;
pub mod font;
pub mod hyprland;
pub mod paths;
pub mod shell;

pub use colors::{OmarchyTheme, read_theme};
pub use paths::{OmarchyPaths, STATE_DIR_ENV};

use sealwax_domain::theme::{FontFamilies, ShellValues};

/// Which inputs a reload reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Request {
    pub palette: bool,
    pub shell: bool,
    pub rounding: bool,
    pub fonts: bool,
}

impl Request {
    /// Everything: the first read.
    pub const ALL: Self = Self {
        palette: true,
        shell: true,
        rounding: true,
        fonts: true,
    };
    /// A theme switch: the palette, the theme's `shell.toml` and Hyprland's rounding.
    pub const THEME: Self = Self {
        palette: true,
        shell: true,
        rounding: true,
        fonts: false,
    };
    /// `~/.config/fontconfig/` changed.
    pub const FONTS: Self = Self {
        palette: false,
        shell: false,
        rounding: false,
        fonts: true,
    };
    /// The user's `shell.toml` changed.
    pub const SHELL: Self = Self {
        palette: false,
        shell: true,
        rounding: false,
        fonts: false,
    };

    pub fn union(self, other: Self) -> Self {
        Self {
            palette: self.palette || other.palette,
            shell: self.shell || other.shell,
            rounding: self.rounding || other.rounding,
            fonts: self.fonts || other.fonts,
        }
    }
}

/// What a reload found; `None` for a part it didn't read.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Readout {
    /// `Some(None)`: no Omarchy theme is installed.
    pub palette: Option<Option<OmarchyTheme>>,
    pub shell: Option<ShellValues>,
    /// `Some(None)`: Hyprland isn't running or didn't answer.
    pub rounding: Option<Option<u32>>,
    pub fonts: Option<FontFamilies>,
}

/// Reads what `request` names, each part on its own thread, so the slowest helper sets the
/// time: `omarchy-theme-color` and `fc-match` take about 8 ms each on the development machine,
/// `hyprctl` about 4 ms.
pub fn read(paths: &OmarchyPaths, request: Request) -> Readout {
    std::thread::scope(|scope| {
        let palette = request
            .palette
            .then(|| scope.spawn(|| colors::read_theme(paths)));
        let shell = request
            .shell
            .then(|| scope.spawn(|| shell::read_shell(paths)));
        let rounding = request.rounding.then(|| scope.spawn(hyprland::rounding));
        let monospace = request
            .fonts
            .then(|| scope.spawn(|| font::family_with("fc-match".as_ref(), font::MONOSPACE)));
        let sans = request
            .fonts
            .then(|| scope.spawn(|| font::family_with("fc-match".as_ref(), font::SANS_SERIF)));
        let fonts = monospace.zip(sans).and_then(|(monospace, sans)| {
            Some(FontFamilies {
                monospace: monospace.join().ok()?,
                sans: sans.join().ok()?,
            })
        });
        Readout {
            palette: palette.and_then(|handle| handle.join().ok()),
            shell: shell.and_then(|handle| handle.join().ok()),
            rounding: rounding.and_then(|handle| handle.join().ok()),
            fonts,
        }
    })
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, MutexGuard};

    static SPAWN: Mutex<()> = Mutex::new(());

    /// Serializes tests that write scripts and run programs: a fork on another test thread
    /// while a script is still open for writing makes its exec fail with ETXTBSY.
    pub fn spawn_lock() -> MutexGuard<'static, ()> {
        SPAWN
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// An executable `#!/bin/sh` script named `name` in `dir`.
    pub fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn requests_combine() {
        assert_eq!(Request::FONTS.union(Request::THEME), Request::ALL);
        assert_eq!(Request::SHELL.union(Request::default()), Request::SHELL);
    }

    #[test]
    fn reads_only_what_was_asked() {
        let _spawning = test_support::spawn_lock();
        let root = tempfile::tempdir().unwrap();
        let paths = OmarchyPaths::new(root.path().join("omarchy"), &root.path().join("config"));
        let current = paths.current();
        fs::create_dir_all(current.join("theme")).unwrap();
        fs::write(
            paths.colors_file(),
            "mode = \"light\"\naccent = \"#1e66f5\"\n",
        )
        .unwrap();
        fs::write(paths.theme_name_file(), "catppuccin-latte\n").unwrap();
        fs::write(paths.theme_shell_file(), "[font]\nbase-size = 13\n").unwrap();

        let readout = read(&paths, Request::SHELL);
        assert_eq!(readout.palette, None);
        assert_eq!(readout.rounding, None);
        assert_eq!(readout.fonts, None);
        assert_eq!(
            readout
                .shell
                .unwrap()
                .get("font.base-size")
                .map(String::as_str),
            Some("13")
        );

        let readout = read(
            &paths,
            Request {
                palette: true,
                ..Request::default()
            },
        );
        let theme = readout.palette.unwrap().unwrap();
        assert_eq!(theme.name, "catppuccin-latte");
        assert_eq!(theme.resolved["mode"], "light");

        fs::remove_dir_all(&current).unwrap();
        let readout = read(
            &paths,
            Request {
                palette: true,
                ..Request::default()
            },
        );
        assert_eq!(readout.palette, Some(None));
    }
}
