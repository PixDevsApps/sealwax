//! Run configuration from the environment. The command line carries only what a user would pass
//! (`mailto:` URIs), because a second instance forwards it to the first.

use email_spike_s6::env;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Open the cached INBOX, report the first frame with rows, quit.
    Startup,
    /// Stay up and handle forwarded `mailto:` command lines and D-Bus `Open` calls.
    Forward,
    /// Query the notification server and send one notification with actions.
    Notify,
    /// Background mode: IMAP IDLE, close the window, notify on new mail.
    Background,
    /// The user's notification-click check (manual-check.sh).
    ManualNotify,
    /// The user's suspend and resume check (manual-resume.sh).
    ManualResume,
    /// Whether `present()` gives an existing, unfocused window the focus back (no input).
    PresentCheck,
}

impl Mode {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "startup" => Self::Startup,
            "forward" => Self::Forward,
            "notify" => Self::Notify,
            "background" => Self::Background,
            "manual-notify" => Self::ManualNotify,
            "manual-resume" => Self::ManualResume,
            "present-check" => Self::PresentCheck,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Forward => "forward",
            Self::Notify => "notify",
            Self::Background => "background",
            Self::ManualNotify => "manual-notify",
            Self::ManualResume => "manual-resume",
            Self::PresentCheck => "present-check",
        }
    }

    pub fn uses_imap(self) -> bool {
        matches!(self, Self::Background | Self::ManualResume)
    }
}

#[derive(Clone, Debug)]
pub struct Imap {
    pub port: u16,
    pub server: String,
    pub user: String,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub mode: Mode,
    pub db: PathBuf,
    pub exit_after: Duration,
    pub linger: Duration,
    /// Rows the list model shows in the first frame (the full count follows right after).
    pub first_rows: Option<u32>,
    pub snapshot_rows: u32,
    pub windowed: bool,
    pub maps_out: Option<PathBuf>,
    pub imap: Option<Imap>,
    pub run_dir: PathBuf,
    pub close_when_idling: bool,
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn number<T: std::str::FromStr>(name: &str) -> Option<T> {
    var(name).and_then(|value| value.parse().ok())
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let mode = match var(env::MODE) {
            Some(name) => Mode::parse(&name).ok_or_else(|| format!("unknown mode {name}"))?,
            None => Mode::Startup,
        };
        let default_exit = match mode {
            Mode::ManualNotify => 180_000,
            Mode::ManualResume => 900_000,
            _ => 55_000,
        };
        let imap = mode.uses_imap().then(|| Imap {
            port: number(env::IMAP_PORT).unwrap_or(16993),
            server: var(env::IMAP_SERVER).unwrap_or_else(|| "stalwart".into()),
            user: var(env::IMAP_USER).unwrap_or_else(|| "spike".into()),
        });
        Ok(Self {
            mode,
            db: var(env::DB)
                .map(PathBuf::from)
                .unwrap_or_else(email_spike_s6::paths::default_db),
            exit_after: Duration::from_millis(number(env::EXIT_AFTER_MS).unwrap_or(default_exit)),
            linger: Duration::from_millis(number(env::LINGER_MS).unwrap_or(400)),
            first_rows: number(env::FIRST_ROWS),
            snapshot_rows: number(env::SNAPSHOT_ROWS).unwrap_or(300),
            windowed: var(env::WINDOWED).is_some(),
            maps_out: var(env::MAPS_OUT).map(PathBuf::from),
            imap,
            run_dir: var(env::RUN_DIR).map(PathBuf::from).unwrap_or_else(|| {
                email_spike_s6::paths::repo_root()
                    .join("target/s6/run")
                    .join(std::process::id().to_string())
            }),
            close_when_idling: var(env::BACKGROUND_CLOSE).is_none_or(|value| value != "0"),
        })
    }
}
