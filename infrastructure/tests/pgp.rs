//! `sealwax_infrastructure::pgp` against the real `gpg`, with a throwaway keyring and a key
//! without a passphrase (ADR-030). Skipped where GnuPG isn't installed.

use sealwax_domain::pgp::{self, Decryption, Verdict, Work};
use sealwax_infrastructure::pgp::Gpg;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Keyring {
    home: tempfile::TempDir,
    scratch: tempfile::TempDir,
    program: PathBuf,
}

impl Drop for Keyring {
    fn drop(&mut self) {
        let _ = Command::new("gpgconf")
            .arg("--homedir")
            .arg(self.home.path())
            .args(["--kill", "all"])
            .status();
    }
}

impl Keyring {
    fn new() -> Option<Keyring> {
        let program = which("gpg")?;
        let scratch = tempfile::tempdir().ok()?;
        let home = tempfile::Builder::new()
            .prefix("g")
            .tempdir_in("/tmp")
            .ok()?;
        let keyring = Keyring {
            home,
            scratch,
            program,
        };
        for user in [
            "Ada Lovelace <ada@example.com>",
            "Mallory <mallory@example.com>",
        ] {
            let made = keyring
                .gpg(&[
                    "--passphrase",
                    "",
                    "--quick-gen-key",
                    user,
                    "future-default",
                    "default",
                    "never",
                ])
                .status()
                .ok()?
                .success();
            assert!(made, "the test key couldn't be made");
        }
        Some(keyring)
    }

    fn gpg(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(&self.program);
        command
            .env("GNUPGHOME", self.home.path())
            .args(["--batch", "--yes", "--no-tty", "--quiet"])
            .args(arguments)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command
    }

    /// `gpg arguments` with `input` on stdin; its stdout.
    fn pipe(&self, arguments: &[&str], input: &[u8]) -> Vec<u8> {
        let mut child = self
            .gpg(arguments)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("gpg: {error}"));
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(input)
                .unwrap_or_else(|error| panic!("gpg stdin: {error}"));
        }
        child
            .wait_with_output()
            .unwrap_or_else(|error| panic!("gpg: {error}"))
            .stdout
    }

    fn reader(&self) -> Gpg {
        Gpg::new(
            self.program.clone(),
            Some(self.home.path().to_owned()),
            &self.scratch.path().join("pgp"),
        )
    }
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|folder| folder.join(name))
        .find(|path| path.is_file())
}

const ENTITY: &[u8] = b"Content-Type: text/plain; charset=utf-8\r\n\r\nHello Bob.\r\n";

fn signed_message(keyring: &Keyring, from: &str, body: &[u8]) -> Vec<u8> {
    let signature = keyring.pipe(
        &[
            "--local-user",
            "ada@example.com",
            "--armor",
            "--detach-sign",
        ],
        ENTITY,
    );
    let mut raw = format!(
        "From: {from}\r\nTo: bob@example.com\r\nSubject: Signed\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/signed; micalg=pgp-sha512; \
         protocol=\"application/pgp-signature\"; boundary=\"b\"\r\n\r\n--b\r\n"
    )
    .into_bytes();
    raw.extend_from_slice(body);
    raw.extend_from_slice(b"\r\n--b\r\nContent-Type: application/pgp-signature\r\n\r\n");
    raw.extend_from_slice(&signature);
    raw.extend_from_slice(b"\r\n--b--\r\n");
    raw
}

fn check(keyring: &Keyring, raw: &[u8], from: &str) -> Verdict {
    let Some(Work::Verify { signed, signature }) = pgp::work(raw) else {
        panic!("no verify work");
    };
    let gpg = keyring.reader();
    let status = gpg
        .verify_detached(&signed, &signature)
        .unwrap_or_else(|error| panic!("verify: {error}"));
    let signature = status
        .signature
        .unwrap_or_else(|| panic!("no signature status"));
    let ids = gpg
        .user_ids(signature.fingerprint.as_deref().unwrap_or_default())
        .unwrap_or_default();
    pgp::verdict(&signature, &ids, Some(from))
}

#[test]
fn a_signed_message_is_checked_against_the_keyring() {
    let Some(keyring) = Keyring::new() else {
        eprintln!("GnuPG isn't installed: skipped");
        return;
    };
    let raw = signed_message(&keyring, "ada@example.com", ENTITY);
    assert_eq!(
        check(&keyring, &raw, "ada@example.com"),
        Verdict::Checked {
            signer: "Ada Lovelace <ada@example.com>".to_owned()
        }
    );
    assert!(matches!(
        check(&keyring, &raw, "mallory@example.com"),
        Verdict::OtherSigner { .. }
    ));
    let changed = signed_message(
        &keyring,
        "ada@example.com",
        b"Content-Type: text/plain; charset=utf-8\r\n\r\nHello Eve.\r\n",
    );
    assert_eq!(check(&keyring, &changed, "ada@example.com"), Verdict::Bad);
    let lf: Vec<u8> = String::from_utf8_lossy(&raw)
        .replace("\r\n", "\n")
        .into_bytes();
    assert!(matches!(
        check(&keyring, &lf, "ada@example.com"),
        Verdict::Checked { .. }
    ));
    assert_eq!(
        std::fs::read_dir(keyring.scratch.path().join("pgp"))
            .map(Iterator::count)
            .unwrap_or(0),
        0,
        "the signature file is removed"
    );
}

#[test]
fn an_encrypted_message_is_decrypted_and_its_signature_read() {
    let Some(keyring) = Keyring::new() else {
        eprintln!("GnuPG isn't installed: skipped");
        return;
    };
    let ciphertext = keyring.pipe(
        &[
            "--local-user",
            "ada@example.com",
            "--recipient",
            "ada@example.com",
            "--armor",
            "--sign",
            "--encrypt",
        ],
        ENTITY,
    );
    let mut raw = b"From: ada@example.com\r\nMIME-Version: 1.0\r\n\
        Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"e\"\r\n\r\n\
        --e\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n\r\n\
        --e\r\nContent-Type: application/octet-stream\r\n\r\n"
        .to_vec();
    raw.extend_from_slice(&ciphertext);
    raw.extend_from_slice(b"\r\n--e--\r\n");
    let Some(Work::Decrypt(ciphertext)) = pgp::work(&raw) else {
        panic!("no decrypt work");
    };
    let answer = keyring
        .reader()
        .decrypt(&ciphertext)
        .unwrap_or_else(|error| panic!("decrypt: {error}"));
    assert_eq!(answer.status.decryption, Some(Decryption::Okay));
    assert_eq!(answer.output, ENTITY);
    assert_eq!(
        answer.status.signature.map(|signature| signature.outcome),
        Some(pgp::Outcome::Good)
    );

    let to_mallory_only = keyring.pipe(
        &["--recipient", "mallory@example.com", "--armor", "--encrypt"],
        ENTITY,
    );
    let other = Keyring::new().unwrap_or_else(|| panic!("second keyring"));
    let answer = other
        .reader()
        .decrypt(&to_mallory_only)
        .unwrap_or_else(|error| panic!("decrypt: {error}"));
    assert_eq!(answer.status.decryption, Some(Decryption::NoSecretKey));
    assert!(answer.output.is_empty());
}

#[test]
fn a_clear_signed_text_is_checked() {
    let Some(keyring) = Keyring::new() else {
        eprintln!("GnuPG isn't installed: skipped");
        return;
    };
    let block = keyring.pipe(
        &["--local-user", "ada@example.com", "--clearsign"],
        b"Hello Bob.\n",
    );
    let text = format!("Before\n{}After\n", String::from_utf8_lossy(&block));
    let Some(pgp::Inline::Signed(block)) = pgp::inline(&text) else {
        panic!("no inline block");
    };
    let status = keyring
        .reader()
        .verify_inline(block.as_bytes())
        .unwrap_or_else(|error| panic!("verify: {error}"));
    assert_eq!(
        status.signature.map(|signature| signature.outcome),
        Some(pgp::Outcome::Good)
    );
}
