//! PGP samples for the self-tests (#32, ADR-030), made with the real `gpg` in the keyring
//! `tools/headless.sh` gives every headless run (`$GNUPGHOME`, empty at first): Ada's key with
//! its secret and no passphrase, and Bob's public key alone. Every name and address is
//! fictional.
//!
//! | Sample | What it is |
//! | --- | --- |
//! | `pgp-encrypted` | From Ada, signed and encrypted to Ada: a text and `report.pdf` |
//! | `pgp-signed` | From Ada, PGP/MIME signed by Ada |
//! | `pgp-tampered` | As `pgp-signed`, its text changed after signing |
//! | `pgp-not-for-me` | Encrypted to Bob only |
//! | `pgp-inline` | An inline PGP message to Ada in a plain text body |

use std::io::Write;
use std::process::{Command, Stdio};

pub(super) const NAMES: [&str; 5] = [
    "pgp-encrypted",
    "pgp-signed",
    "pgp-tampered",
    "pgp-not-for-me",
    "pgp-inline",
];

const ADA: &str = "Ada Lovelace <ada@example.test>";
const BOB: &str = "Bob Berg <bob@example.test>";

/// The PDF the encrypted sample carries.
pub(super) const PDF: &[u8] = b"%PDF-1.4\n% a fictional report\n";

fn gpg(arguments: &[&str], input: &[u8]) -> Result<Vec<u8>, String> {
    if std::env::var_os("GNUPGHOME").is_none() {
        return Err(
            "no GNUPGHOME: run under tools/headless.sh, never on the user's keyring".into(),
        );
    }
    let mut child = Command::new("gpg")
        .args([
            "--batch",
            "--yes",
            "--quiet",
            "--no-tty",
            "--trust-model",
            "always",
        ])
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("gpg: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input)
            .map_err(|error| format!("gpg: {error}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("gpg: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "gpg {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

/// Ada's and Bob's keys, made once; Bob's secret key is deleted.
fn keys() -> Result<(), String> {
    let listed = gpg(&["--list-secret-keys", "--with-colons"], b"").unwrap_or_default();
    if String::from_utf8_lossy(&listed).contains("ada@example.test") {
        return Ok(());
    }
    for user in [ADA, BOB] {
        gpg(
            &[
                "--passphrase",
                "",
                "--quick-gen-key",
                user,
                "future-default",
                "default",
                "never",
            ],
            b"",
        )?;
    }
    let colons = gpg(
        &["--with-colons", "--list-secret-keys", "bob@example.test"],
        b"",
    )?;
    let fingerprint = String::from_utf8_lossy(&colons)
        .lines()
        .find(|line| line.starts_with("fpr:"))
        .and_then(|line| line.split(':').nth(9))
        .map(str::to_owned)
        .ok_or("no fingerprint for Bob")?;
    gpg(&["--delete-secret-keys", &fingerprint], b"")?;
    Ok(())
}

fn header(subject: &str, content_type: &str) -> String {
    format!(
        "From: {ADA}\r\nTo: Me <me@example.test>\r\nSubject: {subject}\r\n\
         Date: Wed, 18 Nov 2026 09:00:00 +0000\r\nMessage-ID: <{}@example.test>\r\n\
         MIME-Version: 1.0\r\nContent-Type: {content_type}\r\n\r\n",
        subject.to_lowercase().replace(' ', "-")
    )
}

fn encrypted(subject: &str, content: &[u8], to: &str, sign: bool) -> Result<Vec<u8>, String> {
    let mut arguments = vec!["--recipient", to, "--armor", "--encrypt"];
    if sign {
        arguments.extend(["--local-user", "ada@example.test", "--sign"]);
    }
    let ciphertext = gpg(&arguments, content)?;
    let mut raw = header(
        subject,
        "multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"e\"",
    )
    .into_bytes();
    raw.extend_from_slice(
        b"--e\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n\r\n\
          --e\r\nContent-Type: application/octet-stream; name=\"encrypted.asc\"\r\n\r\n",
    );
    raw.extend_from_slice(&ciphertext);
    raw.extend_from_slice(b"\r\n--e--\r\n");
    Ok(raw)
}

fn signed(subject: &str, text: &str, shown: &str) -> Result<Vec<u8>, String> {
    let entity = |text: &str| format!("Content-Type: text/plain; charset=utf-8\r\n\r\n{text}\r\n");
    let signature = gpg(
        &[
            "--local-user",
            "ada@example.test",
            "--armor",
            "--detach-sign",
        ],
        entity(text).as_bytes(),
    )?;
    let mut raw = header(
        subject,
        "multipart/signed; micalg=pgp-sha512; protocol=\"application/pgp-signature\"; \
         boundary=\"s\"",
    )
    .into_bytes();
    raw.extend_from_slice(format!("--s\r\n{}\r\n--s\r\n", entity(shown)).as_bytes());
    raw.extend_from_slice(
        b"Content-Type: application/pgp-signature; name=\"signature.asc\"\r\n\r\n",
    );
    raw.extend_from_slice(&signature);
    raw.extend_from_slice(b"\r\n--s--\r\n");
    Ok(raw)
}

/// The sample `name`.
pub(super) fn sample(name: &str) -> Result<Vec<u8>, String> {
    keys()?;
    match name {
        "pgp-encrypted" => {
            let pdf: String = glib_base64(PDF);
            let content = format!(
                "Content-Type: multipart/mixed; boundary=\"m\"\r\n\r\n\
                 --m\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nThe minutes are inside.\r\n\
                 --m\r\nContent-Type: application/pdf; name=\"report.pdf\"\r\n\
                 Content-Disposition: attachment; filename=\"report.pdf\"\r\n\
                 Content-Transfer-Encoding: base64\r\n\r\n{pdf}\r\n--m--\r\n"
            );
            encrypted(
                "Board minutes",
                content.as_bytes(),
                "ada@example.test",
                true,
            )
        }
        "pgp-signed" => signed(
            "Meeting moved",
            "The meeting moved to Friday.",
            "The meeting moved to Friday.",
        ),
        "pgp-tampered" => signed(
            "Meeting moved again",
            "The meeting moved to Friday.",
            "The meeting moved to Monday.",
        ),
        "pgp-not-for-me" => encrypted(
            "For Bob",
            b"Content-Type: text/plain\r\n\r\nOnly Bob can read this.\r\n",
            "bob@example.test",
            false,
        ),
        "pgp-inline" => {
            let block = gpg(
                &["--recipient", "ada@example.test", "--armor", "--encrypt"],
                b"The inline secret.\n",
            )?;
            let mut raw = header("Inline", "text/plain; charset=utf-8").into_bytes();
            raw.extend_from_slice(b"Hello,\r\n\r\n");
            raw.extend_from_slice(&block);
            Ok(raw)
        }
        other => Err(format!("no PGP sample {other}")),
    }
}

fn glib_base64(bytes: &[u8]) -> String {
    gtk4::glib::base64_encode(bytes).to_string()
}
