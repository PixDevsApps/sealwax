//! `sealwax_infrastructure::pgp` against the real `gpg`, with a throwaway keyring and a key
//! without a passphrase (ADR-030). Skipped where GnuPG isn't installed.

use sealwax_domain::compose::protect::{self, Problem, Protection};
use sealwax_domain::compose::{Draft, mime};
use sealwax_domain::model::Address;
use sealwax_domain::pgp::{self, Decryption, Verdict, Work};
use sealwax_infrastructure::pgp::{Gpg, SealError};
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
        Keyring::with(&[
            "Ada Lovelace <ada@example.com>",
            "Mallory <mallory@example.com>",
        ])
    }

    /// A keyring with a key of its own, without a passphrase, for each of `users`.
    fn with(users: &[&str]) -> Option<Keyring> {
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
        for user in users {
            let made = keyring
                .gpg(&[
                    "--passphrase",
                    "",
                    "--quick-gen-key",
                    *user,
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
    signed_by(keyring, "ada@example.com", from, body)
}

/// A PGP/MIME message From `from`, signed with `signer`'s key over [`ENTITY`], with `body` as
/// its first part.
fn signed_by(keyring: &Keyring, signer: &str, from: &str, body: &[u8]) -> Vec<u8> {
    let signature = keyring.pipe(
        &["--local-user", signer, "--armor", "--detach-sign"],
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

/// #32: the user certified Mallory's own user ID; she adds `The Boss <boss@example.com>` to
/// that key, the user refreshes it, and she signs mail From the boss. GnuPG's TRUST line says
/// the key is fully valid, but the added user ID isn't, so it isn't "checked".
#[test]
fn a_user_id_added_to_a_certified_key_is_not_checked() {
    let (Some(user), Some(mallory)) = (
        Keyring::with(&["Ada Lovelace <ada@example.com>"]),
        Keyring::with(&["Mallory <mallory@example.com>"]),
    ) else {
        eprintln!("GnuPG isn't installed: skipped");
        return;
    };
    let public = mallory.pipe(&["--armor", "--export", "mallory@example.com"], b"");
    user.pipe(&["--import"], &public);
    let listed = String::from_utf8_lossy(&user.pipe(
        &[
            "--with-colons",
            "--fixed-list-mode",
            "--list-keys",
            "mallory@example.com",
        ],
        b"",
    ))
    .into_owned();
    let fingerprint = pgp::keys(&listed)
        .first()
        .map(|key| key.fingerprint.clone())
        .unwrap_or_else(|| panic!("Mallory's key wasn't imported"));
    let certified = user
        .gpg(&[
            "--quick-lsign-key",
            &fingerprint,
            "Mallory <mallory@example.com>",
        ])
        .status()
        .is_ok_and(|status| status.success());
    assert!(certified, "the user's certification failed");
    let added = mallory
        .gpg(&[
            "--quick-add-uid",
            "Mallory <mallory@example.com>",
            "The Boss <boss@example.com>",
        ])
        .status()
        .is_ok_and(|status| status.success());
    assert!(added, "the user ID couldn't be added");
    let refreshed = mallory.pipe(&["--armor", "--export", "mallory@example.com"], b"");
    user.pipe(&["--import"], &refreshed);

    let raw = signed_by(&mallory, "mallory@example.com", "boss@example.com", ENTITY);
    assert_eq!(
        check(&user, &raw, "boss@example.com"),
        Verdict::KeyNotVerified {
            signer: "The Boss <boss@example.com>".to_owned()
        }
    );
    assert!(matches!(
        check(&user, &raw, "mallory@example.com"),
        Verdict::Checked { .. }
    ));
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
    let answer = keyring
        .reader()
        .verify_inline(text.as_bytes())
        .unwrap_or_else(|error| panic!("verify: {error}"));
    assert_eq!(
        answer.status.signature.map(|signature| signature.outcome),
        Some(pgp::Outcome::Good)
    );
    assert_eq!(
        answer.output, b"Hello Bob.\n",
        "only the signed text, never what stands around it"
    );
    assert!(block.starts_with("-----BEGIN PGP SIGNED MESSAGE-----"));
}

fn draft_to(to: &str, bcc: Option<&str>, sign: bool, encrypt: bool) -> Draft {
    Draft {
        from: Address::new("Ada Lovelace", "ada@example.com"),
        to: vec![Address::new("", to)],
        bcc: bcc
            .map(|bcc| vec![Address::new("", bcc)])
            .unwrap_or_default(),
        subject: "Plans".into(),
        html: "<div>The plans are ready.</div>".into(),
        message_id: "plans-1@example.com".into(),
        protection: Protection {
            sign,
            encrypt,
            ..Protection::default()
        },
        ..Draft::default()
    }
}

fn transmitted(draft: &Draft) -> Vec<u8> {
    mime::build(draft, 1_795_000_000, &[])
        .unwrap_or_else(|error| panic!("{error}"))
        .transmitted
}

#[test]
fn a_signed_message_checks_in_the_reader() {
    let Some(keyring) = Keyring::new() else {
        eprintln!("GnuPG isn't installed: skipped");
        return;
    };
    let gpg = keyring.reader();
    let draft = draft_to("mallory@example.com", None, true, false);
    let sealed = gpg
        .seal(&draft, &transmitted(&draft), None)
        .unwrap_or_else(|error| panic!("{error:?}"));
    let text = String::from_utf8_lossy(&sealed);
    assert!(
        text.contains("Content-Type: multipart/signed; micalg=pgp-sha"),
        "{text}"
    );
    assert!(text.contains("The plans are ready."));
    assert_eq!(
        check(&keyring, &sealed, "ada@example.com"),
        Verdict::Checked {
            signer: "Ada Lovelace <ada@example.com>".to_owned()
        }
    );
}

#[test]
fn an_encrypted_message_opens_for_each_recipient_and_hides_the_bcc() {
    let Some(keyring) = Keyring::with(&[
        "Ada Lovelace <ada@example.com>",
        "Mallory <mallory@example.com>",
        "Bob <bob@example.com>",
    ]) else {
        eprintln!("GnuPG isn't installed: skipped");
        return;
    };
    let gpg = keyring.reader();
    let draft = draft_to("mallory@example.com", Some("bob@example.com"), true, true);
    let message = transmitted(&draft);
    let sealed = gpg
        .seal(&draft, &message, None)
        .unwrap_or_else(|error| panic!("{error:?}"));
    assert!(
        sealed.len() as u64 <= mime::estimate(&draft),
        "the composer's size check counts the armour"
    );
    let text = String::from_utf8_lossy(&sealed);
    assert!(text.contains("Subject: Plans"), "the Subject stays visible");
    assert!(!text.contains("The plans are ready."));
    let Some(Work::Decrypt(ciphertext)) = pgp::work(&sealed) else {
        panic!("no decrypt work");
    };
    let answer = gpg
        .decrypt(&ciphertext)
        .unwrap_or_else(|error| panic!("decrypt: {error}"));
    assert_eq!(answer.status.decryption, Some(Decryption::Okay));
    let split = protect::split(&message).unwrap_or_else(|| panic!("no split"));
    assert_eq!(answer.output, split.entity);
    assert_eq!(
        answer.status.signature.map(|signature| signature.outcome),
        Some(pgp::Outcome::Good)
    );
    let packets =
        String::from_utf8_lossy(&keyring.pipe(&["--list-packets"], &ciphertext)).into_owned();
    let keyids: Vec<&str> = packets
        .lines()
        .filter(|line| line.starts_with(":pubkey enc packet:"))
        .collect();
    assert_eq!(keyids.len(), 3, "Mallory, Bob and Ada herself: {packets}");
    assert_eq!(
        keyids
            .iter()
            .filter(|line| line.ends_with("keyid 0000000000000000"))
            .count(),
        1,
        "only Bob's is hidden: {packets}"
    );
}

#[test]
fn a_recipient_without_a_key_refuses_the_send() {
    let Some(keyring) = Keyring::new() else {
        eprintln!("GnuPG isn't installed: skipped");
        return;
    };
    let draft = draft_to("carl@example.com", None, false, true);
    match keyring.reader().seal(&draft, &transmitted(&draft), None) {
        Err(SealError::Problem(Problem::Missing(missing))) => {
            assert_eq!(missing, vec!["carl@example.com".to_owned()]);
        }
        other => panic!("{other:?}"),
    }
    let exported = keyring
        .reader()
        .secret_keys()
        .unwrap_or_default()
        .first()
        .map(|key| keyring.reader().export(&key.fingerprint))
        .unwrap_or_else(|| panic!("no secret key"))
        .unwrap_or_else(|error| panic!("export: {error}"));
    assert!(exported.starts_with(b"-----BEGIN PGP PUBLIC KEY BLOCK-----"));
}
