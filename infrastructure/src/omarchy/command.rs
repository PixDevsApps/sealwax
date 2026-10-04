//! Runs a helper program (`omarchy-theme-color`, `fc-match`, `hyprctl`) with a deadline, so
//! a hung helper never stalls a theme reload.

use std::ffi::OsStr;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(1);

/// The program's standard output when it exits successfully within `timeout`; it is killed
/// when it doesn't. Standard input and error are closed.
pub fn stdout(program: &Path, args: &[&OsStr], timeout: Duration) -> io::Result<String> {
    let deadline = Instant::now() + timeout;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut pipe = child.stdout.take().expect("stdout is piped");
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("sealwax-helper-out".into())
        .spawn(move || {
            let mut out = Vec::new();
            let _ = sender.send(pipe.read_to_end(&mut out).map(|_| out));
        })?;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(timed_out(timeout));
        }
        std::thread::sleep(POLL);
    };
    let out = receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| timed_out(timeout))??;
    if !status.success() {
        return Err(io::Error::other(format!("exited with {status}")));
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn timed_out(timeout: Duration) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!("no answer within {} ms", timeout.as_millis()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::omarchy::test_support::{script, spawn_lock};

    #[test]
    fn returns_the_output() {
        let _spawning = spawn_lock();
        let dir = tempfile::tempdir().unwrap();
        let program = script(dir.path(), "echo", "printf '%s|%s' \"$1\" \"$2\"");
        let out = stdout(
            &program,
            &[OsStr::new("a b"), OsStr::new("c")],
            Duration::from_secs(5),
        );
        assert_eq!(out.unwrap(), "a b|c");
    }

    #[test]
    fn failures_are_errors() {
        let _spawning = spawn_lock();
        let dir = tempfile::tempdir().unwrap();
        let program = script(dir.path(), "fail", "echo partial; exit 3");
        let error = stdout(&program, &[], Duration::from_secs(5)).unwrap_err();
        assert!(error.to_string().contains("exit status: 3"), "{error}");
        let missing = stdout(&dir.path().join("missing"), &[], Duration::from_secs(5));
        assert_eq!(missing.unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn a_slow_program_is_killed_at_the_deadline() {
        let _spawning = spawn_lock();
        let dir = tempfile::tempdir().unwrap();
        let program = script(dir.path(), "slow", "exec sleep 5");
        let started = Instant::now();
        let error = stdout(&program, &[], Duration::from_millis(100)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }
}
