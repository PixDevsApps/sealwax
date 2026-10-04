//! Omarchy themes and corners for self-tests (`theme`, `corners`). The harness points the theme
//! follower at a scratch Omarchy state directory (`SEALWAX_OMARCHY_STATE`, set before the app
//! starts), so a run never depends on the desktop's theme, and swaps the fixtures of
//! `tests/theme/` into it the way `omarchy-theme-set` does (as `app/examples/theme_switch.rs`
//! does), then waits for the follower to apply them.

use super::{Harness, StepResult, WAIT_TIMEOUT};
use gtk4::glib;
use sealwax_domain::theme::{CornerRadius, ThemeOptions, ThemeSource};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The scratch state directory under the run's root; it starts without `current/`, so the
/// built-in palette shows until a script installs a theme.
pub fn state_dir(root: &Path) -> PathBuf {
    root.join("omarchy")
}

/// `tests/theme/`: a `colors.toml` per stock theme and the `shell.toml` Omarchy generates.
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/theme")
}

fn fixture(name: &str) -> Result<PathBuf, String> {
    let plain = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    let dir = fixtures().join(name);
    if plain && dir.join("colors.toml").is_file() {
        Ok(dir)
    } else {
        Err(format!("no theme {name} in tests/theme/"))
    }
}

/// Swaps `name` in as `omarchy-theme-set` does: stage `next-theme/`, remove `theme/`, rename
/// `next-theme/` to `theme/`, then write `theme.name`.
fn install(state: &Path, name: &str) -> io::Result<()> {
    let source = fixture(name).map_err(io::Error::other)?;
    let current = state.join("current");
    let next = current.join("next-theme");
    let _ = fs::remove_dir_all(&next);
    fs::create_dir_all(&next)?;
    fs::copy(source.join("colors.toml"), next.join("colors.toml"))?;
    fs::copy(fixtures().join("shell.toml"), next.join("shell.toml"))?;
    let theme = current.join("theme");
    if theme.exists() {
        fs::remove_dir_all(&theme)?;
    }
    fs::rename(&next, &theme)?;
    fs::write(current.join("theme.name"), format!("{name}\n"))
}

impl Harness {
    /// `theme NAME`: installs the fixture `NAME` (or, for `none`, removes `current/`) and waits
    /// until the follower has applied it without a CSS error.
    pub(super) async fn switch_theme(&self, name: &str) -> StepResult {
        let controller = self
            .state
            .theme()
            .ok_or("the theme follower isn't running")?;
        let state = state_dir(&self.root);
        let before = controller.reloads();
        let swapped = Instant::now();
        let builtin = name == "none";
        if builtin {
            match fs::remove_dir_all(state.join("current")) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                    return Err(error.to_string());
                }
                _ => {}
            }
        } else {
            install(&state, name).map_err(|error| error.to_string())?;
        }
        loop {
            let theme = controller.theme();
            let applied = if builtin {
                theme.source == ThemeSource::Builtin
            } else {
                theme.name() == name
            };
            if controller.reloads() > before && applied {
                println!(
                    "  # {} ({}) applied {:.0} ms after the swap",
                    theme.name(),
                    theme.mode().as_str(),
                    swapped.elapsed().as_secs_f64() * 1000.0
                );
                break;
            }
            if swapped.elapsed() > WAIT_TIMEOUT {
                return Err(format!(
                    "{name} was never applied; the theme is {}",
                    theme.name()
                ));
            }
            glib::timeout_future(Duration::from_millis(20)).await;
        }
        match controller.css_errors() {
            0 => Ok(()),
            errors => Err(format!("{errors} CSS errors (see the log)")),
        }
    }

    /// `corners auto|PX`: `config.toml`'s `corner_radius`, applied as the settings would.
    pub(super) fn set_corners(&self, value: &str) -> StepResult {
        let controller = self
            .state
            .theme()
            .ok_or("the theme follower isn't running")?;
        let corner_radius: CornerRadius = value.parse()?;
        controller.set_options(ThemeOptions {
            corner_radius,
            ..controller.options()
        });
        let theme = controller.theme();
        println!(
            "  # radius {} px, small {} px",
            theme.radius, theme.radius_small
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_are_plain_names_of_stock_themes() {
        assert!(fixture("matte-black").is_ok());
        assert!(fixture("catppuccin-latte").is_ok());
        for name in [
            "",
            "../theme",
            "matte-black/",
            "Matte-Black",
            "no-such-theme",
        ] {
            assert!(fixture(name).is_err(), "{name}");
        }
    }

    #[test]
    fn install_swaps_a_theme_in_as_omarchy_does() {
        let root = crate::selftest::TempRoot::create().unwrap();
        let state = state_dir(&root.path);
        install(&state, "matte-black").unwrap();
        install(&state, "catppuccin-latte").unwrap();
        let current = state.join("current");
        assert_eq!(
            fs::read_to_string(current.join("theme.name")).unwrap(),
            "catppuccin-latte\n"
        );
        assert_eq!(
            fs::read(current.join("theme/colors.toml")).unwrap(),
            fs::read(fixtures().join("catppuccin-latte/colors.toml")).unwrap()
        );
        assert!(current.join("theme/shell.toml").is_file());
        assert!(!current.join("next-theme").exists());
    }
}
