//! The command line. It is parsed twice: locally in `main`, so a typo never reaches a running
//! Sealwax, and again in the primary instance for forwarded command lines, where errors go back
//! to the calling terminal. Neither path calls `process::exit`.
//!
//! `--gapplication-service` is GApplication's own option for D-Bus activation (the service file
//! runs `sealwax --gapplication-service`); it is accepted here, hidden, so that the local parse
//! lets it through to GApplication.

use clap::{Arg, ArgAction, Command, value_parser};
use sealwax_domain::mailto;
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cli {
    /// `mailto:` URIs (RFC 6068), each a message to write.
    pub uris: Vec<String>,
    pub self_test: Option<PathBuf>,
    /// Run as the D-Bus-activated service.
    pub service: bool,
}

pub fn command() -> Command {
    Command::new("sealwax")
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
            Arg::new("gapplication-service")
                .long("gapplication-service")
                .action(ArgAction::SetTrue)
                .hide(true),
        )
        .arg(
            Arg::new("self-test")
                .long("self-test")
                .value_name("SCRIPT")
                .value_parser(value_parser!(PathBuf))
                .help(
                    "Run a self-test script in a window of its own and exit (see docs/TESTING.md)",
                ),
        )
}

fn mailto_uri(value: &str) -> Result<String, String> {
    if mailto::is_mailto(value) {
        Ok(value.to_owned())
    } else {
        Err("not a mailto: link".to_owned())
    }
}

/// Parses `args`, including the program name. Help and version requests come back as errors
/// whose `exit_code()` is 0, as clap reports them.
pub fn parse<I, T>(args: I) -> Result<Cli, clap::Error>
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
        self_test: matches.get_one::<PathBuf>("self-test").cloned(),
        service: matches.get_flag("gapplication-service"),
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
        assert_eq!(cli.self_test, None);
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
        assert!(
            text.contains("MAILTO") && text.contains("--self-test"),
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
    fn service_and_self_test() {
        assert!(
            parse(["sealwax", "--gapplication-service"])
                .unwrap()
                .service
        );
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
