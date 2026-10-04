//! The font families fontconfig resolves. fontconfig is Omarchy's source of truth: `omarchy
//! font set` writes `~/.config/fontconfig/fonts.conf` with a `monospace` rule, and Omarchy's
//! `50-omarchy.conf` maps `sans-serif` (INTEGRATIONS.md "Fonts").

use super::command;
use sealwax_domain::theme::FontFamilies;
use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

pub const MONOSPACE: &str = "monospace";
pub const SANS_SERIF: &str = "sans-serif";

/// How long `fc-match` may take (about 8 ms here; a cache rebuild can take longer).
pub const TIMEOUT: Duration = Duration::from_secs(2);

/// The longest family name kept.
const FAMILY_LIMIT: usize = 200;

/// `fc-match monospace` and `fc-match sans-serif`.
pub fn families() -> FontFamilies {
    families_with(Path::new("fc-match"))
}

pub fn families_with(fc_match: &Path) -> FontFamilies {
    FontFamilies {
        monospace: family_with(fc_match, MONOSPACE),
        sans: family_with(fc_match, SANS_SERIF),
    }
}

/// `fc-match <generic> -f '%{family[0]}'`, or `generic` itself when fc-match is missing,
/// fails or prints nothing; Pango and WebKit resolve a generic name on their own, but only
/// from the fontconfig setup they read at start, so a concrete name follows `omarchy font
/// set` live.
pub fn family_with(fc_match: &Path, generic: &str) -> String {
    let args = [
        OsStr::new(generic),
        OsStr::new("-f"),
        OsStr::new("%{family[0]}"),
    ];
    match command::stdout(fc_match, &args, TIMEOUT) {
        Ok(output) => {
            let family: String = output
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .chars()
                .filter(|character| !character.is_control())
                .take(FAMILY_LIMIT)
                .collect();
            if family.is_empty() {
                generic.to_owned()
            } else {
                family
            }
        }
        Err(error) => {
            tracing::warn!(%error, generic, "fc-match failed; using the generic family");
            generic.to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::omarchy::test_support::{script, spawn_lock};

    #[test]
    fn reads_the_first_family_per_generic() {
        let _spawning = spawn_lock();
        let dir = tempfile::tempdir().unwrap();
        let fc_match = script(
            dir.path(),
            "fc-match",
            "[ \"$2\" = -f ] && [ \"$3\" = '%{family[0]}' ] || exit 9\n\
             case $1 in monospace) printf 'Test Mono Nerd Font' ;; sans-serif) printf 'Test Sans\\n' ;; esac",
        );
        assert_eq!(
            families_with(&fc_match),
            FontFamilies {
                monospace: "Test Mono Nerd Font".to_owned(),
                sans: "Test Sans".to_owned(),
            }
        );
    }

    #[test]
    fn falls_back_to_the_generic_name() {
        let _spawning = spawn_lock();
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in [("fails", "exit 1"), ("blank", "printf '  \\n'")] {
            let fc_match = script(dir.path(), name, body);
            assert_eq!(family_with(&fc_match, MONOSPACE), MONOSPACE, "{body}");
        }
        assert_eq!(
            families_with(&dir.path().join("missing")),
            FontFamilies::default()
        );
    }

    #[test]
    fn the_real_fc_match_gives_names() {
        let _spawning = spawn_lock();
        let families = families();
        assert!(!families.monospace.is_empty());
        assert!(!families.sans.is_empty());
    }
}
