//! The command line. It is parsed twice: locally in `main`, so a typo never reaches a running
//! Sealwax, and again in the primary instance for forwarded command lines, where errors go back
//! to the calling terminal. Neither path calls `process::exit`.
//!
//! `--gapplication-service` is GApplication's own option for D-Bus activation (the service file
//! runs `sealwax --gapplication-service`); it is accepted here, hidden, so that the local parse
//! lets it through to GApplication.
//!
//! `--background` (ML-084) starts Sealwax without a window, as an autostart entry does; given to a
//! running Sealwax it shows nothing. `--compose` opens a new message; with `mailto:` links, those
//! messages are opened instead. Both are forwarded to a running Sealwax like the links.
//!
//! `--folder KEY` (1.6.3) shows a folder by its sidebar key, as `unread.json` names it
//! (`folder-42`, or `all-inboxes`), so the bar widget's list opens the folder picked there; it is
//! forwarded to a running Sealwax like the links, and a key Sealwax doesn't know shows the window
//! as it was.
//!
//! `--unread` (1.5.0, issue #17) prints the unread number of the Sealwax running now, from the
//! file it keeps (`sealwax_infrastructure::unread`), and exits 1 when none runs; it never starts
//! or asks a running Sealwax, so a bar or a script can call it as often as it likes.
//!
//! `--self-test` exists only in a build with the `test-hooks` feature (`sealwax-selftest`); the
//! released binary has no such option.

use clap::{Arg, ArgAction, Command};
use sealwax_domain::mailto;
use std::ffi::OsString;
#[cfg(feature = "test-hooks")]
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Cli {
    /// `mailto:` URIs (RFC 6068), each a message to write.
    pub uris: Vec<String>,
    #[cfg(feature = "test-hooks")]
    pub self_test: Option<PathBuf>,
    /// Run as the D-Bus-activated service.
    pub service: bool,
    /// Start without a window (autostart, ML-084).
    pub background: bool,
    /// Open a new message.
    pub compose: bool,
    /// Print the running Sealwax's unread number and exit.
    pub unread: bool,
    /// Show this folder, by its sidebar key.
    pub folder: Option<String>,
}

pub(crate) fn command() -> Command {
    let command = Command::new("sealwax")
        .about("A fast, minimal email client for Omarchy")
        .version(env!("CARGO_PKG_VERSION"))
        .arg(
            Arg::new("uris")
                .value_name("MAILTO")
                .num_args(0..)
                .action(ArgAction::Append)
                .value_parser(mailto_uri)
                .help("mailto: links to write a message from; attachments in them are ignored"),
        )
        .arg(
            Arg::new("compose")
                .long("compose")
                .action(ArgAction::SetTrue)
                .help("Write a new message"),
        )
        .arg(
            Arg::new("background")
                .long("background")
                .action(ArgAction::SetTrue)
                .conflicts_with_all(["uris", "compose"])
                .help("Start without a window, as at login; mail keeps syncing in the background"),
        )
        .arg(
            Arg::new("folder")
                .long("folder")
                .value_name("KEY")
                .value_parser(folder_key)
                .conflicts_with_all(["uris", "compose", "background"])
                .help(
                    "Show a folder: its key from unread.json, such as folder-42, or all-inboxes",
                ),
        )
        .arg(
            Arg::new("unread")
                .long("unread")
                .action(ArgAction::SetTrue)
                .conflicts_with_all(["uris", "compose", "background", "folder"])
                .help(
                    "Print the number of unread messages in the Inboxes, their folders and your own \
                     folders of the Sealwax running now, or exit 1 when it isn't running",
                ),
        )
        .arg(
            Arg::new("gapplication-service")
                .long("gapplication-service")
                .action(ArgAction::SetTrue)
                .hide(true),
        );
    #[cfg(feature = "test-hooks")]
    let command = command.arg(
        Arg::new("self-test")
            .long("self-test")
            .value_name("SCRIPT")
            .value_parser(clap::value_parser!(PathBuf))
            .conflicts_with("background")
            .help("Run a self-test script in a window of its own and exit (see docs/TESTING.md)"),
    );
    command
}

fn mailto_uri(value: &str) -> Result<String, String> {
    if mailto::is_mailto(value) {
        Ok(value.to_owned())
    } else {
        Err("not a mailto: link".to_owned())
    }
}

/// A sidebar key: `folder-` and a number, or `all-inboxes`.
fn folder_key(value: &str) -> Result<String, String> {
    let number = value.strip_prefix("folder-").is_some_and(|id| {
        !id.is_empty() && id.len() <= 19 && id.bytes().all(|b| b.is_ascii_digit())
    });
    if number || value == "all-inboxes" {
        Ok(value.to_owned())
    } else {
        Err("not a folder key (folder-42, or all-inboxes)".to_owned())
    }
}

/// Parses `args`, including the program name. Help and version requests come back as errors
/// whose `exit_code()` is 0, as clap reports them.
pub(crate) fn parse<I, T>(args: I) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let matches = command().try_get_matches_from(args)?;
    Ok(Cli {
        uris: matches
            .get_many::<String>("uris")
            .map(|uris| uris.cloned().collect())
            .unwrap_or_default(),
        #[cfg(feature = "test-hooks")]
        self_test: matches.get_one::<PathBuf>("self-test").cloned(),
        service: matches.get_flag("gapplication-service"),
        background: matches.get_flag("background"),
        compose: matches.get_flag("compose"),
        unread: matches.get_flag("unread"),
        folder: matches.get_one::<String>("folder").cloned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn mailto_links() {
        let cli = parse([
            "sealwax",
            "mailto:a@example.com?subject=Hi",
            "MAILTO:b@example.com",
        ])
        .unwrap();
        assert_eq!(
            cli.uris,
            ["mailto:a@example.com?subject=Hi", "MAILTO:b@example.com"]
        );
        assert!(!cli.service);
        assert_eq!(parse(["sealwax"]).unwrap(), Cli::default());
    }

    #[test]
    fn anything_else_fails_here_and_never_reaches_a_running_sealwax() {
        for args in [
            &["sealwax", "https://example.com"][..],
            &["sealwax", "notes.txt"],
            &["sealwax", "--no-such-flag"],
            &["sealwax", "--self-test"],
        ] {
            let error = parse(args).unwrap_err();
            assert_eq!(error.exit_code(), 2, "{args:?}");
        }
        let error = parse(["sealwax", "notes.txt"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        assert!(error.to_string().contains("not a mailto: link"), "{error}");
    }

    #[test]
    fn help_and_version_exit_zero() {
        let help = parse(["sealwax", "--help"]).unwrap_err();
        assert_eq!(help.kind(), ErrorKind::DisplayHelp);
        assert_eq!(help.exit_code(), 0);
        let text = help.to_string();
        assert!(text.contains("MAILTO"), "{text}");
        assert_eq!(
            text.contains("--self-test"),
            cfg!(feature = "test-hooks"),
            "{text}"
        );
        assert!(!text.contains("gapplication-service"), "{text}");
        let version = parse(["sealwax", "--version"]).unwrap_err();
        assert_eq!(version.kind(), ErrorKind::DisplayVersion);
        assert_eq!(version.exit_code(), 0);
        assert_eq!(
            version.to_string().trim(),
            format!("sealwax {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn a_folder_by_its_key() {
        let cli = parse(["sealwax", "--folder", "folder-42"]).unwrap();
        assert_eq!(cli.folder.as_deref(), Some("folder-42"));
        assert_eq!(
            parse(["sealwax", "--folder=all-inboxes"])
                .unwrap()
                .folder
                .as_deref(),
            Some("all-inboxes")
        );
        for key in [
            "42",
            "folder-",
            "folder-4a",
            "folder--1",
            "folder-42; rm -rf ~",
            "INBOX",
            "folder-12345678901234567890",
        ] {
            let error = parse(["sealwax", "--folder", key]).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::ValueValidation, "{key}");
        }
        for args in [
            &["sealwax", "--folder", "folder-1", "--compose"][..],
            &["sealwax", "--folder", "folder-1", "--background"],
            &["sealwax", "--folder", "folder-1", "--unread"],
            &["sealwax", "--folder", "folder-1", "mailto:a@example.com"],
        ] {
            assert_eq!(
                parse(args).unwrap_err().kind(),
                ErrorKind::ArgumentConflict,
                "{args:?}"
            );
        }
    }

    #[test]
    fn service() {
        assert!(
            parse(["sealwax", "--gapplication-service"])
                .unwrap()
                .service
        );
    }

    #[cfg(feature = "test-hooks")]
    #[test]
    fn self_test() {
        let cli = parse([
            "sealwax",
            "--self-test",
            "tests/selftest/window.sealwax-test",
        ])
        .unwrap();
        assert_eq!(
            cli.self_test,
            Some(PathBuf::from("tests/selftest/window.sealwax-test"))
        );
    }

    #[test]
    fn unread_stands_alone() {
        assert!(parse(["sealwax", "--unread"]).unwrap().unread);
        assert!(!parse(["sealwax"]).unwrap().unread);
        for args in [
            &["sealwax", "--unread", "--compose"][..],
            &["sealwax", "--unread", "--background"],
            &["sealwax", "--unread", "mailto:a@example.com"],
        ] {
            let error = parse(args).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::ArgumentConflict, "{args:?}");
        }
    }

    #[test]
    fn background_and_compose() {
        let cli = parse(["sealwax", "--background"]).unwrap();
        assert!(cli.background && !cli.compose && cli.uris.is_empty());
        let cli = parse(["sealwax", "--compose"]).unwrap();
        assert!(cli.compose && !cli.background);
        let cli = parse(["sealwax", "--compose", "mailto:a@example.com"]).unwrap();
        assert!(cli.compose);
        assert_eq!(cli.uris, ["mailto:a@example.com"]);
        assert!(
            parse(["sealwax", "--gapplication-service", "--background"])
                .unwrap()
                .background
        );
        for args in [
            &["sealwax", "--background", "--compose"][..],
            &["sealwax", "--background", "mailto:a@example.com"],
            #[cfg(feature = "test-hooks")]
            &["sealwax", "--background", "--self-test", "a.sealwax-test"],
        ] {
            let error = parse(args).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::ArgumentConflict, "{args:?}");
            assert_eq!(error.exit_code(), 2, "{args:?}");
        }
        let help = parse(["sealwax", "--help"]).unwrap_err().to_string();
        assert!(
            help.contains("--background") && help.contains("--compose"),
            "{help}"
        );
        assert!(help.contains("Write a new message"), "{help}");
    }

    #[test]
    fn command_is_consistent() {
        command().debug_assert();
    }

    #[test]
    fn the_help_starts_with_the_description() {
        let help = command().render_help().to_string();
        assert_eq!(
            help.lines().next(),
            Some("A fast, minimal email client for Omarchy")
        );
    }
}
