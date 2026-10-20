//! Check for Updates (ADR-021 amendment, 2026-10-20): the version GitHub names as Sealwax's latest
//! release against the running one, and the command that installs it. The tag comes from the
//! network and ends up in a command the user pastes into a terminal, so only `vX.Y.Z` with
//! plain numbers is read, and the command is built from the numbers, never from the tag's text.

use std::fmt;

/// Sealwax's public repository, whose releases carry the package.
pub const REPOSITORY: &str = "PixDevsApps/sealwax";

/// The name every release gives its package (`tools/release.py`'s `STABLE_NAME`).
pub const PACKAGE: &str = "sealwax-x86_64.pkg.tar.zst";

/// A release's version: three plain numbers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// `1.2.3` or a tag, `v1.2.3`; nothing else (no pre-release or build suffix, no sign, no
    /// spaces).
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut parts = text.split('.');
        let mut number = || {
            let part = parts.next()?;
            if part.is_empty() || part.len() > 9 || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        };
        let version = Self {
            major: number()?,
            minor: number()?,
            patch: number()?,
        };
        parts.next().is_none().then_some(version)
    }

    /// The release's tag, `v1.2.3`.
    pub fn tag(&self) -> String {
        format!("v{self}")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The running Sealwax against the latest release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The running version is the latest release.
    Latest,
    /// A newer release is out.
    Newer(Version),
    /// The running version is newer than the latest release (a build from `main`).
    Ahead(Version),
}

pub fn compare(running: Version, latest: Version) -> Outcome {
    match running.cmp(&latest) {
        std::cmp::Ordering::Less => Outcome::Newer(latest),
        std::cmp::Ordering::Equal => Outcome::Latest,
        std::cmp::Ordering::Greater => Outcome::Ahead(latest),
    }
}

/// INSTALL.md's install command for `version`'s own package: it downloads that release's
/// package to `/tmp` and installs it with pacman, which replaces the version installed.
pub fn update_command(version: Version) -> String {
    format!(
        "curl -L -o /tmp/sealwax.pkg.tar.zst \
         https://github.com/{REPOSITORY}/releases/download/{}/{PACKAGE} \
         && sudo pacman -U /tmp/sealwax.pkg.tar.zst",
        version.tag()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u32, minor: u32, patch: u32) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    #[test]
    fn reads_plain_versions_and_tags_only() {
        assert_eq!(Version::parse("0.5.0"), Some(v(0, 5, 0)));
        assert_eq!(Version::parse("v0.5.0"), Some(v(0, 5, 0)));
        assert_eq!(Version::parse("v1.10.200"), Some(v(1, 10, 200)));
        for text in [
            "",
            "v",
            "1.2",
            "1.2.3.4",
            "1..3",
            "V1.2.3",
            "vv1.2.3",
            " 1.2.3",
            "1.2.3 ",
            "1.2.3-rc1",
            "1.2.3+build",
            "+1.2.3",
            "1.-2.3",
            "1.2.3;rm -rf ~",
            "1.2.3$(id)",
            "١.٢.٣",
            "1.2.9999999999",
        ] {
            assert_eq!(Version::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn the_running_version_against_the_latest() {
        assert_eq!(compare(v(0, 5, 0), v(0, 5, 0)), Outcome::Latest);
        assert_eq!(compare(v(0, 5, 0), v(0, 6, 0)), Outcome::Newer(v(0, 6, 0)));
        assert_eq!(compare(v(0, 5, 0), v(0, 5, 1)), Outcome::Newer(v(0, 5, 1)));
        assert_eq!(compare(v(0, 9, 9), v(1, 0, 0)), Outcome::Newer(v(1, 0, 0)));
        assert_eq!(compare(v(0, 10, 0), v(0, 9, 0)), Outcome::Ahead(v(0, 9, 0)));
        assert_eq!(compare(v(0, 5, 1), v(0, 5, 0)), Outcome::Ahead(v(0, 5, 0)));
    }

    #[test]
    fn the_command_installs_that_release() {
        assert_eq!(
            update_command(v(0, 6, 0)),
            "curl -L -o /tmp/sealwax.pkg.tar.zst \
             https://github.com/PixDevsApps/sealwax/releases/download/v0.6.0/\
             sealwax-x86_64.pkg.tar.zst && sudo pacman -U /tmp/sealwax.pkg.tar.zst"
        );
    }

    #[test]
    fn the_workspace_version_reads() {
        assert!(Version::parse(env!("CARGO_PKG_VERSION")).is_some());
    }
}
