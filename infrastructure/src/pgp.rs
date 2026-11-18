//! Running GnuPG for the reader (#32, ADR-030) and for sending (ADR-032): the user's own `gpg`, keyring and gpg-agent, so
//! private keys stay where they are (hardware keys included) and passphrases go through the
//! agent's pinentry. Sealwax holds no key and no passphrase.
//!
//! Every run is `--batch` with its status lines on a pipe of their own, file descriptor 3
//! ([`sealwax_domain::pgp::status`] reads them), so nothing GnuPG logs can pass for one. It
//! never fetches a key from the network (`--no-auto-key-retrieve`), and is killed after
//! [`TIMEOUT`]. Decryptions run one at a time, so two messages never ask for a passphrase at
//! once. Plaintext only ever goes through pipes; the one file written is a detached signature,
//! which is public, 0600 in a private folder, removed after the check.

use sealwax_domain::compose::Draft;
use sealwax_domain::compose::protect::{self, Plan, Problem};
use sealwax_domain::pgp::{self, Key, MAX_BYTES, Status};
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

/// How long one run may take, a passphrase typed into pinentry included.
pub const TIMEOUT: Duration = Duration::from_secs(300);

/// The descriptor gpg writes its status lines to.
const STATUS_FD: i32 = 3;

/// Decryptions wait for each other.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// The user's `gpg`.
#[derive(Clone, Debug)]
pub struct Gpg {
    program: PathBuf,
    /// `GNUPGHOME`, for tests; the user's own otherwise.
    home: Option<PathBuf>,
    /// Where detached signatures are written for the check.
    scratch: PathBuf,
}

/// What a run gave back.
#[derive(Debug)]
pub struct Answer {
    /// stdout: the plaintext of a decryption.
    pub output: Vec<u8>,
    pub status: Status,
}

impl Gpg {
    /// `gpg` (or `gpg2`) from `PATH`; `None` when GnuPG isn't installed. `scratch` is a private
    /// folder of Sealwax's (under the runtime folder).
    pub fn find(scratch: &Path) -> Option<Gpg> {
        let path = std::env::var_os("PATH")?;
        ["gpg", "gpg2"].iter().find_map(|name| {
            std::env::split_paths(&path)
                .map(|folder| folder.join(name))
                .find(|candidate| candidate.is_file())
                .map(|program| Gpg::new(program, None, scratch))
        })
    }

    pub fn new(program: PathBuf, home: Option<PathBuf>, scratch: &Path) -> Gpg {
        Gpg {
            program,
            home,
            scratch: scratch.to_owned(),
        }
    }

    fn command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args([
                "--batch",
                "--no-tty",
                "--status-fd",
                "3",
                "--no-auto-key-retrieve",
                "--display-charset",
                "utf-8",
            ])
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(home) = &self.home {
            command.env("GNUPGHOME", home);
        }
        command
    }

    /// Decrypts `ciphertext`; a signature inside it is checked on the way.
    pub fn decrypt(&self, ciphertext: &[u8]) -> io::Result<Answer> {
        let _turn = ONE_AT_A_TIME
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        run(self.command(&["--decrypt"]), ciphertext)
    }

    /// Checks a clear-signed block; the answer's output is the text GnuPG checked, its
    /// dash-escaping undone, and nothing from outside the block.
    pub fn verify_inline(&self, block: &[u8]) -> io::Result<Answer> {
        run(self.command(&["--output", "-", "--verify"]), block)
    }

    /// Checks `signature` over `signed`; the signed bytes go through a pipe.
    pub fn verify_detached(&self, signed: &[u8], signature: &[u8]) -> io::Result<Status> {
        let file = self.write_signature(signature)?;
        let mut command = self.command(&["--verify"]);
        command.arg(file.path()).arg("-");
        let answer = run(command, signed);
        drop(file);
        answer.map(|answer| answer.status)
    }

    /// The valid user IDs of the key `fingerprint`.
    pub fn user_ids(&self, fingerprint: &str) -> io::Result<Vec<pgp::UserId>> {
        if fingerprint.is_empty() || !fingerprint.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(Vec::new());
        }
        let answer = run(
            self.command(&[
                "--with-colons",
                "--fixed-list-mode",
                "--list-keys",
                fingerprint,
            ]),
            b"",
        )?;
        Ok(pgp::user_ids(&String::from_utf8_lossy(&answer.output)))
    }

    /// The public keys with a user ID of one of `addresses` (exact address matches).
    pub fn public_keys(&self, addresses: &[String]) -> io::Result<Vec<Key>> {
        let patterns: Vec<String> = addresses
            .iter()
            .filter(|address| address.contains('@') && !address.contains(['<', '>', '\n']))
            .map(|address| format!("<{address}>"))
            .collect();
        if patterns.is_empty() {
            return Ok(Vec::new());
        }
        let mut command =
            self.command(&["--with-colons", "--fixed-list-mode", "--list-keys", "--"]);
        command.args(&patterns);
        let answer = run(command, b"")?;
        Ok(pgp::keys(&String::from_utf8_lossy(&answer.output)))
    }

    /// The user's secret keys.
    pub fn secret_keys(&self) -> io::Result<Vec<Key>> {
        let answer = run(
            self.command(&["--with-colons", "--fixed-list-mode", "--list-secret-keys"]),
            b"",
        )?;
        Ok(pgp::keys(&String::from_utf8_lossy(&answer.output)))
    }

    /// What signing and encrypting `draft` takes, from the keyring as it is now: the keys, or
    /// what stands in the way. `configured` is the account's `pgp_key`.
    pub fn plan(&self, draft: &Draft, configured: Option<&str>) -> Result<Plan, SealError> {
        if !draft.protection.any() {
            return Ok(Plan::default());
        }
        let addresses: Vec<String> = protect::recipients(draft)
            .into_iter()
            .map(|(address, _)| address)
            .collect();
        let public = if draft.protection.encrypt {
            self.public_keys(&addresses).map_err(SealError::GnuPG)?
        } else {
            Vec::new()
        };
        let secret = self.secret_keys().map_err(SealError::GnuPG)?;
        protect::plan(draft, &public, &secret, configured).map_err(SealError::Problem)
    }

    /// `message` (the MIME builder's transmitted copy of `draft`) signed and/or encrypted as
    /// the draft says: a PGP/MIME message. Signing may ask for the passphrase through the
    /// agent's pinentry, so it waits its turn with decryptions.
    pub fn seal(
        &self,
        draft: &Draft,
        message: &[u8],
        configured: Option<&str>,
    ) -> Result<Vec<u8>, SealError> {
        let plan = self.plan(draft, configured)?;
        let split = protect::split(message).ok_or(SealError::Failed)?;
        let boundary = || {
            let mut random = [0u8; 16];
            getrandom::fill(&mut random).map_err(|_| SealError::Failed)?;
            Ok::<_, SealError>(protect::boundary(random))
        };
        let _turn = ONE_AT_A_TIME
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if draft.protection.encrypt {
            let mut arguments: Vec<String> = ["--trust-model", "always", "--armor", "--encrypt"]
                .map(str::to_owned)
                .to_vec();
            if let Some(signer) = &plan.signer {
                arguments.extend([
                    "--sign".to_owned(),
                    "--local-user".to_owned(),
                    signer.clone(),
                ]);
            }
            for key in &plan.recipients {
                arguments.extend(["--recipient".to_owned(), key.clone()]);
            }
            for key in &plan.hidden {
                arguments.extend(["--hidden-recipient".to_owned(), key.clone()]);
            }
            let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
            let answer = run(self.command(&arguments), &split.entity).map_err(SealError::GnuPG)?;
            if !answer.status.encrypted
                || answer.status.refused
                || answer.output.is_empty()
                || plan.signer.is_some() && answer.status.created.is_none()
            {
                return Err(SealError::Failed);
            }
            return protect::encrypted(&split, &answer.output, &boundary()?)
                .ok_or(SealError::Failed);
        }
        let signer = plan.signer.as_deref().ok_or(SealError::Failed)?;
        let answer = run(
            self.command(&["--armor", "--detach-sign", "--local-user", signer]),
            protect::signed_bytes(&split),
        )
        .map_err(SealError::GnuPG)?;
        let micalg = answer
            .status
            .created
            .and_then(pgp::micalg)
            .filter(|_| !answer.status.refused && !answer.output.is_empty())
            .ok_or(SealError::Failed)?;
        protect::signed(&split, &answer.output, micalg, &boundary()?).ok_or(SealError::Failed)
    }

    /// The user's public key `fingerprint`, armoured, to attach (`export-minimal`: no
    /// third-party signatures).
    pub fn export(&self, fingerprint: &str) -> io::Result<Vec<u8>> {
        if !protect::is_fingerprint(fingerprint) {
            return Err(io::Error::other("not a fingerprint"));
        }
        let answer = run(
            self.command(&[
                "--armor",
                "--export-options",
                "export-minimal",
                "--export",
                fingerprint,
            ]),
            b"",
        )?;
        if answer.output.is_empty() {
            return Err(io::Error::other("GnuPG exported nothing"));
        }
        Ok(answer.output)
    }

    fn write_signature(&self, signature: &[u8]) -> io::Result<Removed> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.scratch)?;
        let mut name = [0u8; 8];
        getrandom::fill(&mut name).map_err(io::Error::other)?;
        let name: String = name.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = self.scratch.join(format!("signature-{name}.asc"));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        let removed = Removed(path);
        file.write_all(signature)?;
        Ok(removed)
    }
}

/// Why a draft couldn't be signed or encrypted.
#[derive(Debug)]
pub enum SealError {
    /// A key is missing, to be picked, or the user's own can't be used.
    Problem(Problem),
    /// GnuPG couldn't be run.
    GnuPG(io::Error),
    /// GnuPG refused: the passphrase was cancelled or wrong, a key turned out unusable, or it
    /// failed otherwise.
    Failed,
}

/// A file removed when dropped.
struct Removed(PathBuf);

impl Removed {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Removed {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Runs `command` with `input` on stdin, reading stdout (at most [`MAX_BYTES`]) and the status
/// lines while it runs, and kills it after [`TIMEOUT`].
fn run(mut command: Command, input: &[u8]) -> io::Result<Answer> {
    if input.len() > MAX_BYTES {
        return Err(io::Error::other("too large for GnuPG"));
    }
    let (status_reader, status_writer) = io::pipe()?;
    let writer_fd = status_writer.as_raw_fd();
    // SAFETY: the closure runs in the child between fork and exec and calls only `dup2` and
    // `fcntl`, which are async-signal-safe, on descriptor numbers copied in before the fork.
    unsafe {
        command.pre_exec(move || {
            // `dup2` onto itself would keep close-on-exec, so that case clears it instead.
            let done = if writer_fd == STATUS_FD {
                libc::fcntl(writer_fd, libc::F_SETFD, 0)
            } else {
                libc::dup2(writer_fd, STATUS_FD)
            };
            if done < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let spawned = command.spawn();
    drop(status_writer);
    let mut child = spawned?;
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let input = input.to_vec();
    let writer = thread::spawn(move || {
        if let Some(stdin) = stdin.as_mut() {
            // gpg may stop reading early (a damaged message); the outcome says why.
            let _ = stdin.write_all(&input);
        }
    });
    let out = thread::spawn(move || capped(stdout));
    let status = thread::spawn(move || capped(Some(status_reader)));
    let finished = wait(&mut child);
    let _ = writer.join();
    let output = out.join().unwrap_or_default();
    let status = status.join().unwrap_or_default();
    finished?;
    Ok(Answer {
        output,
        status: pgp::status(&status),
    })
}

fn capped(stream: Option<impl Read>) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(stream) = stream {
        let _ = stream.take(MAX_BYTES as u64 + 1).read_to_end(&mut out);
    }
    if out.len() > MAX_BYTES {
        out.clear();
    }
    out
}

fn wait(child: &mut Child) -> io::Result<()> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "GnuPG took too long",
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}
