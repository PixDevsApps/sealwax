//! Getting help (ADR-028): the support address, the online user guide and
//! the details a message to support carries. The details say which Sealwax runs, how it was
//! installed and on what; never anything about the user's mail, accounts or files (no address,
//! server, folder name or path), and the user sees them before they go anywhere.

use crate::mailto::Mailto;

/// Where users without a GitHub account write for help.
pub const SUPPORT_ADDRESS: &str = "support@pixdevs.com";
/// The online user guide.
pub const GUIDE_URL: &str = "https://pixdevs.com/guides/sealwax/";
/// A new issue on GitHub, for those who use it: Report an Issue opens it.
pub const NEW_ISSUE_URL: &str = "https://github.com/PixDevsApps/sealwax/issues/new";
/// The subject of a message Write to Support starts.
pub const SUBJECT: &str = "Sealwax support";
/// The line above the details in a message to support.
pub const DETAILS_HEADING: &str = "About this Sealwax:";

/// The longest value the details keep from the system (os-release, the environment).
const MAX_VALUE_CHARS: usize = 80;

/// How this Sealwax was installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Install {
    Flatpak,
    /// A package: the binary is under `/usr`.
    Package,
    /// A debug build (`cargo run`), with its own folders.
    Development,
    /// A release build run from anywhere else.
    Source,
}

impl Install {
    pub const fn new(flatpak: bool, debug_build: bool, under_usr: bool) -> Self {
        if flatpak {
            Self::Flatpak
        } else if debug_build {
            Self::Development
        } else if under_usr {
            Self::Package
        } else {
            Self::Source
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Flatpak => "Flatpak",
            Self::Package => "package",
            Self::Development => "development build",
            Self::Source => "built from source",
        }
    }
}

/// What a message to support says about this Sealwax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Details {
    pub version: String,
    pub install: Install,
    /// os-release's name of the system ([`os_name`]).
    pub system: Option<String>,
    pub architecture: String,
    /// `XDG_CURRENT_DESKTOP`.
    pub desktop: Option<String>,
    /// `XDG_SESSION_TYPE`: wayland or x11.
    pub session: Option<String>,
    /// The libraries' versions at run time, `major.minor.micro`.
    pub gtk: String,
    pub libadwaita: String,
    pub webkit: String,
    /// The accounts Sealwax has, counted only.
    pub accounts: usize,
}

impl Details {
    /// The details as plain lines, the same in the dialog, on the clipboard and in the message.
    pub fn text(&self) -> String {
        let known = |value: &Option<String>| {
            value
                .as_deref()
                .map(clean)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "unknown".to_owned())
        };
        let desktop = match self.session.as_deref().map(clean) {
            Some(session) if !session.is_empty() => format!("{}, {session}", known(&self.desktop)),
            _ => known(&self.desktop),
        };
        format!(
            "Sealwax {} ({})\nSystem: {}, {}\nDesktop: {desktop}\nGTK {}, libadwaita {}, WebKitGTK {}\nAccounts: {}",
            clean(&self.version),
            self.install.label(),
            known(&self.system),
            clean(&self.architecture),
            clean(&self.gtk),
            clean(&self.libadwaita),
            clean(&self.webkit),
            self.accounts,
        )
    }
}

/// The message Write to Support opens: to the support address, with room to write above the
/// details.
pub fn message(details: &Details) -> Mailto {
    Mailto {
        to: vec![SUPPORT_ADDRESS.to_owned()],
        subject: Some(SUBJECT.to_owned()),
        body: Some(format!("\n\n\n{DETAILS_HEADING}\n{}", details.text())),
        ..Mailto::default()
    }
}

/// The system's name from an os-release file (os-release(5)): `PRETTY_NAME`, else `NAME`.
pub fn os_name(os_release: &str) -> Option<String> {
    let value = |key: &str| {
        os_release.lines().find_map(|line| {
            let rest = line.trim().strip_prefix(key)?.strip_prefix('=')?;
            let unquoted = rest
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .or_else(|| {
                    rest.strip_prefix('\'')
                        .and_then(|rest| rest.strip_suffix('\''))
                })
                .unwrap_or(rest);
            let name = clean(&unquoted.replace("\\\"", "\""));
            (!name.is_empty()).then_some(name)
        })
    };
    value("PRETTY_NAME").or_else(|| value("NAME"))
}

/// A value from the system with no control characters, at most [`MAX_VALUE_CHARS`] long.
fn clean(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_VALUE_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn details() -> Details {
        Details {
            version: "1.3.2".to_owned(),
            install: Install::Flatpak,
            system: Some("Arch Linux".to_owned()),
            architecture: "x86_64".to_owned(),
            desktop: Some("Hyprland".to_owned()),
            session: Some("wayland".to_owned()),
            gtk: "4.24.1".to_owned(),
            libadwaita: "1.9.0".to_owned(),
            webkit: "2.54.1".to_owned(),
            accounts: 2,
        }
    }

    #[test]
    fn the_details_read_as_lines() {
        assert_eq!(
            details().text(),
            "Sealwax 1.3.2 (Flatpak)\n\
             System: Arch Linux, x86_64\n\
             Desktop: Hyprland, wayland\n\
             GTK 4.24.1, libadwaita 1.9.0, WebKitGTK 2.54.1\n\
             Accounts: 2"
        );
    }

    #[test]
    fn what_is_missing_says_unknown() {
        let text = Details {
            system: None,
            desktop: None,
            session: None,
            ..details()
        }
        .text();
        assert!(text.contains("System: unknown, x86_64\n"), "{text}");
        assert!(text.contains("Desktop: unknown\n"), "{text}");
        let text = Details {
            desktop: None,
            ..details()
        }
        .text();
        assert!(text.contains("Desktop: unknown, wayland\n"), "{text}");
    }

    #[test]
    fn values_from_the_system_lose_control_characters_and_length() {
        let text = Details {
            system: Some(format!("Evil\nAccounts: 99\r{}", "x".repeat(200))),
            ..details()
        }
        .text();
        assert_eq!(text.lines().count(), 5, "{text}");
        assert!(text.ends_with("Accounts: 2"), "{text}");
        let system = text.lines().nth(1).unwrap_or_default();
        assert!(system.chars().count() <= "System: ".len() + 80 + ", x86_64".len());
    }

    #[test]
    fn each_install_has_a_label() {
        assert_eq!(Install::new(true, true, true), Install::Flatpak);
        assert_eq!(Install::new(false, true, true), Install::Development);
        assert_eq!(Install::new(false, false, true), Install::Package);
        assert_eq!(Install::new(false, false, false), Install::Source);
        assert_eq!(Install::Package.label(), "package");
        assert_eq!(Install::Source.label(), "built from source");
    }

    #[test]
    fn the_message_goes_to_support_with_the_details_below() {
        let message = message(&details());
        assert_eq!(message.to, [SUPPORT_ADDRESS]);
        assert_eq!(message.subject.as_deref(), Some(SUBJECT));
        let body = message.body.unwrap_or_default();
        assert!(body.starts_with("\n\n\n"), "{body:?}");
        assert!(
            body.ends_with(&format!("{DETAILS_HEADING}\n{}", details().text())),
            "{body:?}"
        );
        assert!(message.cc.is_empty() && message.bcc.is_empty());
    }

    #[test]
    fn os_release_names_the_system() {
        let arch = "NAME=\"Arch Linux\"\nPRETTY_NAME=\"Arch Linux\"\nID=arch\n";
        assert_eq!(os_name(arch).as_deref(), Some("Arch Linux"));
        let fedora = "NAME='Fedora Linux'\nVERSION_ID=43\nPRETTY_NAME='Fedora Linux 43 (Workstation Edition)'";
        assert_eq!(
            os_name(fedora).as_deref(),
            Some("Fedora Linux 43 (Workstation Edition)")
        );
        assert_eq!(os_name("NAME=NixOS\nID=nixos").as_deref(), Some("NixOS"));
        assert_eq!(
            os_name("PRETTY_NAME=\"\"\nNAME=Debian").as_deref(),
            Some("Debian")
        );
        assert_eq!(os_name("ID=arch\n"), None);
        assert_eq!(os_name(""), None);
    }

    #[test]
    fn the_addresses_are_the_owners() {
        assert_eq!(SUPPORT_ADDRESS, "support@pixdevs.com");
        assert_eq!(GUIDE_URL, "https://pixdevs.com/guides/sealwax/");
        assert!(NEW_ISSUE_URL.starts_with("https://github.com/PixDevsApps/sealwax/"));
        assert!(crate::compose::address::is_valid(SUPPORT_ADDRESS));
    }
}
