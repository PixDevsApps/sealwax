//! Finding and running plugins (ADR-022). Each folder in `plugins/` with a `plugin.toml` is a
//! plugin. A plugin is run with the JSON on stdin, in its own folder and process group, with
//! a deadline; its output is read on threads and capped, and a plugin past its deadline is
//! killed with everything it started. Nothing a plugin writes is logged: only its id, its exit
//! status and lengths.

use crate::settings_files::read_optional;
use sealwax_domain::config::FileError;
use sealwax_domain::plugins::{self, MANIFEST_FILE, MAX_OUTPUT, Manifest, Reply};
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// How long an action may take: it should hand off to its own window and return.
pub const ACTION_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a hook may take; hooks run one at a time, after the mail is stored.
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(10);

/// An installed plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plugin {
    pub dir: PathBuf,
    pub manifest: Manifest,
}

/// A plugin folder that can't be used, and why, for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub id: String,
    pub message: String,
}

/// The installed plugins in `dir`, in folder order, and the folders that can't be used. A
/// missing `dir` is no plugins. Disabled plugins (`enabled = false`) are left out.
pub fn discover(dir: &Path) -> (Vec<Plugin>, Vec<Problem>) {
    let mut found = Vec::new();
    let mut problems = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (found, problems);
    };
    let mut folders: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    folders.sort();
    for folder in folders {
        let id = folder
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or_default()
            .to_owned();
        let manifest_path = folder.join(MANIFEST_FILE);
        if let Err(message) = private(&folder).and_then(|()| private(&manifest_path)) {
            problems.push(Problem { id, message });
            continue;
        }
        let text = match read_optional(&manifest_path) {
            Ok(Some(text)) => text,
            Ok(None) => continue,
            Err(error) => {
                problems.push(Problem {
                    id,
                    message: format!("{MANIFEST_FILE} can't be read: {error}"),
                });
                continue;
            }
        };
        match plugins::parse_manifest(&id, &text) {
            Ok(manifest) if manifest.enabled => found.push(Plugin {
                dir: folder,
                manifest,
            }),
            Ok(_) => {}
            Err(errors) => problems.push(Problem {
                id,
                message: describe(&errors),
            }),
        }
    }
    (found, problems)
}

fn describe(errors: &[FileError]) -> String {
    let first = errors.first().map(ToString::to_string).unwrap_or_default();
    match errors.len() {
        0 | 1 => format!("{MANIFEST_FILE} not used: {first}"),
        count => format!(
            "{MANIFEST_FILE} not used: {first} (and {} more mistakes)",
            count - 1
        ),
    }
}

/// A plugin's folder and manifest must be the user's and writable by the user alone, as ssh
/// asks of its keys: otherwise another account could make Sealwax run its program.
fn private(path: &Path) -> Result<(), String> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("{} can't be read: {error}", path.display())),
    };
    // SAFETY: getuid has no preconditions and can't fail.
    let me = unsafe { libc::getuid() };
    if metadata.uid() != me {
        return Err("it isn't yours, so it isn't run".to_owned());
    }
    if metadata.permissions().mode() & 0o022 != 0 {
        let name = path
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("its folder");
        return Err(format!(
            "{name} can be changed by other users, so it isn't run (chmod go-w)"
        ));
    }
    Ok(())
}

/// Why a run failed, for the user.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("couldn't start {program}: {error}")]
    Start { program: String, error: io::Error },
    #[error("took longer than {} s and was stopped", .0.as_secs())]
    Timeout(Duration),
    #[error("failed{}", .0.as_deref().map(|line| format!(": {line}")).unwrap_or_default())]
    Failed(Option<String>),
    #[error("{0}")]
    Answer(String),
}

/// The program to run: a path in the plugin's folder when the manifest's program has a `/`,
/// otherwise a name looked up on `PATH`.
fn program(plugin: &Plugin) -> PathBuf {
    let first = Path::new(&plugin.manifest.command[0]);
    if first.is_absolute() || !plugin.manifest.command[0].contains('/') {
        first.to_path_buf()
    } else {
        plugin.dir.join(first)
    }
}

/// Reads up to [`MAX_OUTPUT`] bytes of a pipe on a thread, and drains the rest so the plugin
/// never blocks on a full pipe.
fn collect(pipe: Option<impl Read + Send + 'static>, name: &str) -> thread::JoinHandle<Vec<u8>> {
    thread::Builder::new()
        .name(format!("sealwax-plugin-{name}"))
        .spawn(move || {
            let mut kept = Vec::new();
            if let Some(mut pipe) = pipe {
                let mut buffer = [0u8; 8192];
                while let Ok(read) = pipe.read(&mut buffer) {
                    if read == 0 {
                        break;
                    }
                    let room = MAX_OUTPUT.saturating_sub(kept.len());
                    kept.extend_from_slice(&buffer[..read.min(room)]);
                }
            }
            kept
        })
        .expect("a thread for a plugin's output")
}

/// Runs `plugin` with `input` (the JSON) on stdin and reads its answer. Blocking: call it off
/// the GTK thread.
pub fn run(plugin: &Plugin, input: &str, timeout: Duration) -> Result<Reply, RunError> {
    let program = program(plugin);
    let mut command = Command::new(&program);
    command
        .args(&plugin.manifest.command[1..])
        .current_dir(&plugin.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Its own group, so the deadline ends whatever it started too.
        .process_group(0)
        .env("SEALWAX_PLUGIN", &plugin.manifest.id)
        .env(
            "SEALWAX_PLUGIN_PROTOCOL",
            plugins::PROTOCOL_VERSION.to_string(),
        )
        // main removes it from Sealwax's own environment (SECURITY.md); never hand it on.
        .env_remove("WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS");
    let mut child = command.spawn().map_err(|error| RunError::Start {
        program: plugin.manifest.command[0].clone(),
        error,
    })?;
    let stdout = collect(child.stdout.take(), "out");
    let stderr = collect(child.stderr.take(), "err");
    let mut stdin = child.stdin.take();
    let input = input.as_bytes().to_vec();
    let writer = thread::Builder::new()
        .name("sealwax-plugin-in".to_owned())
        .spawn(move || {
            if let Some(stdin) = stdin.as_mut() {
                // A plugin that doesn't read its input is fine: ignore a closed pipe.
                let _ = stdin.write_all(&input);
            }
        })
        .expect("a thread for a plugin's input");

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            _ => break None,
        }
    };
    let Some(status) = status else {
        // SAFETY: kill with a negative pid signals the process group the child leads.
        unsafe {
            libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
        }
        let _ = child.wait();
        let _ = writer.join();
        tracing::warn!(plugin = %plugin.manifest.id, "a plugin ran past its deadline and was stopped");
        return Err(RunError::Timeout(timeout));
    };
    let _ = writer.join();
    let stdout = String::from_utf8_lossy(&stdout.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr.join().unwrap_or_default()).into_owned();
    tracing::debug!(
        plugin = %plugin.manifest.id,
        code = status.code(),
        stdout = stdout.len(),
        stderr = stderr.len(),
        "a plugin ran"
    );
    if !status.success() {
        return Err(RunError::Failed(plugins::first_line(&stderr)));
    }
    plugins::parse_reply(&stdout).map_err(RunError::Answer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::omarchy::test_support::{script, spawn_lock};

    fn plugin(dir: &Path, command: &[&str]) -> Plugin {
        let words: Vec<String> = command.iter().map(|word| format!("\"{word}\"")).collect();
        let manifest = format!(
            "command = [{}]\n[[action]]\nid = \"go\"\ntitle = \"Go\"\n",
            words.join(", ")
        );
        Plugin {
            dir: dir.to_path_buf(),
            manifest: plugins::parse_manifest("demo", &manifest).unwrap(),
        }
    }

    #[test]
    fn a_plugin_gets_the_json_and_answers() {
        let _lock = spawn_lock();
        let dir = tempfile::tempdir().unwrap();
        script(
            dir.path(),
            "echo.sh",
            "input=$(cat)\nprintf '{\"notice\": \"got %s bytes from %s\"}' \"${#input}\" \"$SEALWAX_PLUGIN\"",
        );
        let reply = run(
            &plugin(dir.path(), &["./echo.sh"]),
            "{\"version\": 1}",
            ACTION_TIMEOUT,
        )
        .unwrap();
        assert_eq!(reply.notice.as_deref(), Some("got 14 bytes from demo"));
    }

    #[test]
    fn failures_and_deadlines() {
        let _lock = spawn_lock();
        let dir = tempfile::tempdir().unwrap();
        script(
            dir.path(),
            "fail.sh",
            "echo 'no calendar is set up' >&2\nexit 3",
        );
        let error = run(&plugin(dir.path(), &["./fail.sh"]), "{}", ACTION_TIMEOUT).unwrap_err();
        assert_eq!(error.to_string(), "failed: no calendar is set up");

        script(dir.path(), "slow.sh", "sleep 30 &\nsleep 30");
        let started = Instant::now();
        let error = run(
            &plugin(dir.path(), &["./slow.sh"]),
            "{}",
            Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(matches!(error, RunError::Timeout(_)));
        assert!(started.elapsed() < Duration::from_secs(5));

        script(dir.path(), "chatty.sh", "echo 'not json'");
        let error = run(&plugin(dir.path(), &["./chatty.sh"]), "{}", ACTION_TIMEOUT).unwrap_err();
        assert!(matches!(error, RunError::Answer(_)));

        let missing =
            run(&plugin(dir.path(), &["./missing.sh"]), "{}", ACTION_TIMEOUT).unwrap_err();
        assert!(
            missing
                .to_string()
                .starts_with("couldn't start ./missing.sh")
        );
    }

    #[test]
    fn a_plugin_that_ignores_a_big_input_or_floods_output_still_finishes() {
        let _lock = spawn_lock();
        let dir = tempfile::tempdir().unwrap();
        script(
            dir.path(),
            "flood.sh",
            "head -c 1000000 /dev/zero | tr '\\0' 'x'\necho '{}'",
        );
        let input = "y".repeat(4 * 1024 * 1024);
        let result = run(&plugin(dir.path(), &["./flood.sh"]), &input, ACTION_TIMEOUT);
        assert!(
            matches!(result, Err(RunError::Answer(_))),
            "capped output isn't an answer"
        );
    }

    #[test]
    fn discovers_plugins_and_refuses_shared_ones() {
        let root = tempfile::tempdir().unwrap();
        let make = |id: &str, manifest: &str| {
            let dir = root.path().join(id);
            std::fs::create_dir(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::write(dir.join(MANIFEST_FILE), manifest).unwrap();
            std::fs::set_permissions(
                dir.join(MANIFEST_FILE),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            dir
        };
        let good = "command = [\"x\"]\n[[action]]\nid = \"go\"\ntitle = \"Go\"\n";
        make("signet", good);
        make("broken", "command = 3\n");
        make("off", &format!("enabled = false\n{good}"));
        let shared = make("shared", good);
        std::fs::set_permissions(
            shared.join(MANIFEST_FILE),
            std::fs::Permissions::from_mode(0o666),
        )
        .unwrap();
        std::fs::create_dir(root.path().join("empty")).unwrap();

        let (found, problems) = discover(root.path());
        let ids: Vec<&str> = found.iter().map(|p| p.manifest.id.as_str()).collect();
        assert_eq!(ids, ["signet"]);
        let mut problem_ids: Vec<&str> = problems.iter().map(|p| p.id.as_str()).collect();
        problem_ids.sort();
        assert_eq!(problem_ids, ["broken", "shared"]);
        assert!(problems.iter().any(|p| p.message.contains("chmod go-w")));
        assert!(problems.iter().any(|p| {
            p.message
                .starts_with("plugin.toml not used: line 1, column 11")
        }));
        assert!(discover(&root.path().join("nowhere")).0.is_empty());
    }
}
