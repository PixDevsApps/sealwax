//! Open With (issue #39, ADR-031): hands the message the reader shows to a command from
//! `config.toml`'s `[[open_with]]`. The message's original, the bytes Save as .eml saves
//! (downloaded first when the store keeps only its text parts), is written as a 0600 file named
//! after its subject in a new 0700 folder under `$XDG_RUNTIME_DIR/<app-id>/open-with` (`files`).
//! Then the command starts with `{file}` replaced by the file's path: directly, without a shell,
//! in a process group of its own, with its input and output closed, off the GTK thread.
//!
//! Sealwax doesn't wait for the command before the file goes: many programs hand the file to a
//! window that is already open and exit at once (`xdg-open` does), so the file stays until the
//! next start empties the folder; the runtime folder is also emptied at logout. A command that
//! doesn't start says why in a toast. The log names an entry by its place in the list, never its
//! command, the file or the message.

use super::files;
use super::{Reader, export};
use gtk4::{gio, glib};
use sealwax_domain::attachment::eml_file_name;
use sealwax_domain::open_with::OpenWith;
use std::ffi::OsString;
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

impl Reader {
    /// The Open With commands the reader offers, in order; the More menu lists them.
    pub fn set_open_with(&self, entries: Vec<OpenWith>) {
        let titles: Vec<String> = entries.iter().map(OpenWith::title).collect();
        self.inner.toolbar.set_open_with(&titles);
        self.inner.files.open_with.replace(entries);
    }

    /// The More menu's Open With items, titles and actions, for checks.
    #[cfg(feature = "test-hooks")]
    pub fn open_with_items(&self) -> Vec<(String, String)> {
        self.inner.toolbar.open_with_items()
    }

    /// Hands the message the reader shows to Open With entry `number` (1 for the first).
    pub fn open_with(&self, number: usize) {
        let entry = number
            .checked_sub(1)
            .and_then(|index| self.inner.files.open_with.borrow().get(index).cloned());
        let (Some(entry), Some(id)) = (entry, self.message()) else {
            return;
        };
        let inner = self.inner.clone();
        glib::spawn_future_local(async move {
            let title = entry.title();
            let downloading = || files::toast(&inner, "Downloading the message from the server…");
            let bytes = match files::original(&inner, id, downloading).await {
                Ok(bytes) => bytes,
                Err(failure) => {
                    tracing::info!(entry = number, ?failure, "reader: no message for Open With");
                    files::toast(&inner, &not_started(&title, failure.reason()));
                    return;
                }
            };
            let name = eml_file_name(&export::subject(&inner, id).await);
            let root = inner.files.open_with_root.borrow().clone();
            let path = match files::write_private_in(root, name, bytes).await {
                Ok(path) => path,
                Err(error) => {
                    tracing::warn!(
                        entry = number,
                        kind = ?error.kind(),
                        "reader: the message for Open With couldn't be written"
                    );
                    files::toast(
                        &inner,
                        &not_started(
                            &title,
                            &format!("the message couldn't be written to a file ({error})"),
                        ),
                    );
                    return;
                }
            };
            let arguments = entry.arguments(&path);
            let started = gio::spawn_blocking(move || start(number, arguments))
                .await
                .unwrap_or_else(|_| Err(io::Error::other("the starter stopped")));
            match started {
                Ok(()) => tracing::info!(entry = number, "reader: an Open With command started"),
                Err(error) => {
                    tracing::warn!(
                        entry = number,
                        kind = ?error.kind(),
                        "reader: an Open With command didn't start"
                    );
                    files::toast(
                        &inner,
                        &not_started(&title, &start_failure(&error, entry.program())),
                    );
                }
            }
        });
    }
}

/// "Open With Archive didn't start: …."
fn not_started(title: &str, reason: &str) -> String {
    format!("{title} didn't start: {reason}.")
}

/// Why the program didn't start, in plain words.
fn start_failure(error: &io::Error, program: &str) -> String {
    match error.kind() {
        io::ErrorKind::NotFound => format!(
            "Sealwax can't find the program “{program}”; give its full path if it isn't in PATH"
        ),
        io::ErrorKind::PermissionDenied => {
            format!("“{program}” can't be run; check that it is a program and executable")
        }
        _ => format!("“{program}” couldn't be started ({error})"),
    }
}

/// Starts `arguments` (the program first) without a shell and without waiting for it: in a
/// process group of its own, so a signal to Sealwax's group doesn't reach it, with its input
/// and output closed, so nothing it prints reaches Sealwax's log. A thread reaps it when it
/// ends. Blocks while the program is started: call it off the GTK thread.
fn start(number: usize, arguments: Vec<OsString>) -> io::Result<()> {
    let mut arguments = arguments.into_iter();
    let program = arguments
        .next()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    let reaper = std::thread::Builder::new()
        .name("open-with".to_owned())
        .spawn(move || match child.wait() {
            Ok(status) => tracing::debug!(
                entry = number,
                code = ?status.code(),
                "reader: an Open With command ended"
            ),
            Err(error) => tracing::debug!(
                entry = number,
                kind = ?error.kind(),
                "reader: an Open With command couldn't be waited for"
            ),
        });
    if let Err(error) = reaper {
        tracing::warn!(entry = number, %error, "reader: an Open With command isn't reaped");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::files::Failure;
    use sealwax_engine::api::BodyError;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sealwax-open-with-test-{}-{}",
            std::process::id(),
            glib::random_int()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn wait_for(path: &Path) -> Vec<u8> {
        let started = Instant::now();
        loop {
            if let Ok(bytes) = std::fs::read(path)
                && !bytes.is_empty()
            {
                return bytes;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "nothing at {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn the_command_gets_the_path_as_one_argument_without_a_shell() {
        let dir = scratch();
        // A name a shell would split, expand, glob or end at.
        let file = dir.join("Re: \"Q3\" $HOME; echo hi * `id` & 'x'.eml");
        std::fs::write(&file, b"Subject: Q3\r\n\r\nHello\r\n").unwrap();
        let copy = dir.join("copy.eml");
        let entry = OpenWith {
            name: "Copy".to_owned(),
            command: vec![
                "cp".to_owned(),
                "--".to_owned(),
                "{file}".to_owned(),
                copy.to_string_lossy().into_owned(),
            ],
        };
        start(1, entry.arguments(&file)).unwrap();
        assert_eq!(wait_for(&copy), b"Subject: Q3\r\n\r\nHello\r\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_program_is_named_in_plain_words() {
        let error = start(
            1,
            vec![
                "sealwax-no-such-program".into(),
                "/run/x/message.eml".into(),
            ],
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(
            not_started(
                "Open With Archive",
                &start_failure(&error, "sealwax-no-such-program")
            ),
            "Open With Archive didn't start: Sealwax can't find the program \
             “sealwax-no-such-program”; give its full path if it isn't in PATH."
        );
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        assert_eq!(
            start_failure(&denied, "/home/kari/notes.txt"),
            "“/home/kari/notes.txt” can't be run; check that it is a program and executable"
        );
        assert_eq!(
            not_started(
                "Open With Archive",
                Failure::Download(BodyError::Offline).reason()
            ),
            "Open With Archive didn't start: the mail server can't be reached."
        );
    }

    #[test]
    fn a_file_that_isnt_a_program_doesnt_start() {
        let dir = scratch();
        let file = dir.join("notes.txt");
        std::fs::write(&file, b"not a program").unwrap();
        let error = start(1, vec![file.clone().into_os_string()]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
