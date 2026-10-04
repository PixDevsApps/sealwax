//! Hyprland's corner rounding, which `corner_radius = "auto"` follows (ADR-019), read as
//! omarchy-shell reads it: `hyprctl -j getoption decoration:rounding`.

use super::command;
use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

/// How long `hyprctl` may take (about 4 ms here).
pub const TIMEOUT: Duration = Duration::from_millis(300);

/// Set by Hyprland in its session; without it there is no Hyprland to ask.
pub const INSTANCE_ENV: &str = "HYPRLAND_INSTANCE_SIGNATURE";

/// `decoration:rounding` in px, or `None` when Hyprland isn't running or `hyprctl` is
/// missing, fails, answers something else or takes longer than [`TIMEOUT`].
pub fn rounding() -> Option<u32> {
    std::env::var_os(INSTANCE_ENV)?;
    rounding_with(Path::new("hyprctl"), TIMEOUT)
}

pub fn rounding_with(hyprctl: &Path, timeout: Duration) -> Option<u32> {
    let args = ["-j", "getoption", "decoration:rounding"].map(OsStr::new);
    match command::stdout(hyprctl, &args, timeout) {
        Ok(json) => {
            let rounding = parse_rounding(&json);
            if rounding.is_none() {
                tracing::warn!("hyprctl's rounding answer was not understood; corners are square");
            }
            rounding
        }
        Err(error) => {
            tracing::warn!(%error, "could not read Hyprland's rounding; corners are square");
            None
        }
    }
}

/// The `int` field of `{"option": "decoration:rounding", "int": 8, "set": true }`.
pub fn parse_rounding(json: &str) -> Option<u32> {
    let after_key = &json[json.find("\"int\"")? + "\"int\"".len()..];
    let value = after_key.trim_start().strip_prefix(':')?.trim_start();
    let digits = value.bytes().take_while(u8::is_ascii_digit).count();
    let rest = value[digits..].trim_start();
    if digits == 0 || !(rest.starts_with(',') || rest.starts_with('}')) {
        return None;
    }
    value[..digits].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::omarchy::test_support::{script, spawn_lock};
    use std::time::Instant;

    #[test]
    fn parses_hyprctls_answer() {
        assert_eq!(
            parse_rounding("{\"option\": \"decoration:rounding\", \"int\": 8, \"set\": true }"),
            Some(8)
        );
        assert_eq!(parse_rounding("{\"int\":0}"), Some(0));
        assert_eq!(parse_rounding("{\"int\": 12 }"), Some(12));
        for answer in [
            "",
            "no such option",
            "{\"int\": -3, \"set\": true}",
            "{\"int\": 8.5, \"set\": true}",
            "{\"int\": \"8\"}",
            "{\"float\": 8.0}",
        ] {
            assert_eq!(parse_rounding(answer), None, "{answer}");
        }
    }

    #[test]
    fn reads_the_rounding_from_hyprctl() {
        let _spawning = spawn_lock();
        let dir = tempfile::tempdir().unwrap();
        let hyprctl = script(
            dir.path(),
            "hyprctl",
            "[ \"$*\" = '-j getoption decoration:rounding' ] || exit 2\n\
             printf '{\"option\": \"decoration:rounding\", \"int\": 10, \"set\": true }'",
        );
        assert_eq!(rounding_with(&hyprctl, TIMEOUT), Some(10));
    }

    #[test]
    fn failures_and_slow_answers_mean_square() {
        let _spawning = spawn_lock();
        let dir = tempfile::tempdir().unwrap();
        let failing = script(
            dir.path(),
            "failing",
            "echo 'HYPRLAND_INSTANCE_SIGNATURE not set' >&2; exit 1",
        );
        assert_eq!(rounding_with(&failing, TIMEOUT), None);
        assert_eq!(rounding_with(&dir.path().join("missing"), TIMEOUT), None);
        let slow = script(dir.path(), "slow", "exec sleep 5");
        let started = Instant::now();
        assert_eq!(rounding_with(&slow, TIMEOUT), None);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }
}
