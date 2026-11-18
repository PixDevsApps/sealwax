//! PGP mail, read side (#32, ADR-030): what GnuPG must be given to decrypt a message or check
//! its signature, and what its answers mean. Pure: `sealwax_infrastructure::pgp` runs `gpg`,
//! the reader shows the outcome. A signature is called checked only when GnuPG says it is good,
//! the user's keyring says the key is valid, and one of the key's user IDs is the sender's
//! address; anything less says what is missing.
//!
//! PGP/MIME (RFC 3156): `multipart/encrypted` anywhere in the message is decrypted; a
//! `multipart/signed` is checked only at the root of the message (or of the decrypted content),
//! since one further down covers only part of what is shown. Inline PGP: the first armoured
//! block of the text body.

use mail_parser::{MessageParser, MimeHeaders, PartType};

/// The most bytes handed to GnuPG or taken back from it.
pub const MAX_BYTES: usize = 64 * 1024 * 1024;

const ENCRYPTED_START: &str = "-----BEGIN PGP MESSAGE-----";
const ENCRYPTED_END: &str = "-----END PGP MESSAGE-----";
const SIGNED_START: &str = "-----BEGIN PGP SIGNED MESSAGE-----";
const SIGNATURE_END: &str = "-----END PGP SIGNATURE-----";

/// What GnuPG has to do for a PGP/MIME message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Work {
    /// Decrypt the `multipart/encrypted` part's ciphertext; the plaintext is a MIME entity.
    Decrypt(Vec<u8>),
    /// Check `signature` over `signed`, the root's first part with CRLF line ends (RFC 3156 §5).
    Verify { signed: Vec<u8>, signature: Vec<u8> },
}

/// The PGP/MIME work `raw` needs, if any.
pub fn work(raw: &[u8]) -> Option<Work> {
    let message = MessageParser::default().parse(raw)?;
    let kind = |part: &mail_parser::MessagePart<'_>| {
        part.content_type()
            .map(|content_type| {
                format!(
                    "{}/{}",
                    content_type.ctype(),
                    content_type.subtype().unwrap_or_default()
                )
                .to_ascii_lowercase()
            })
            .unwrap_or_default()
    };
    for part in &message.parts {
        if let PartType::Multipart(children) = &part.body
            && kind(part) == "multipart/encrypted"
        {
            let ciphertext = children
                .get(1)
                .and_then(|index| message.parts.get(*index as usize))?
                .contents();
            return (!ciphertext.is_empty() && ciphertext.len() <= MAX_BYTES)
                .then(|| Work::Decrypt(ciphertext.to_vec()));
        }
    }
    let root = message.parts.first()?;
    let PartType::Multipart(children) = &root.body else {
        return None;
    };
    if kind(root) != "multipart/signed" {
        return None;
    }
    let content_type = root.content_type()?;
    let attribute = |name: &str| {
        content_type.attributes().and_then(|attributes| {
            attributes
                .iter()
                .find(|attribute| attribute.name.eq_ignore_ascii_case(name))
                .map(|attribute| attribute.value.to_string())
        })
    };
    if !attribute("protocol")?.eq_ignore_ascii_case("application/pgp-signature") {
        return None;
    }
    let boundary = attribute("boundary")?;
    let start = usize::try_from(root.offset_body).ok()?;
    let end = usize::try_from(root.offset_end).ok()?.min(raw.len());
    let signed = first_part(raw.get(start..end)?, boundary.as_bytes())?;
    let signature = children
        .get(1)
        .and_then(|index| message.parts.get(*index as usize))?
        .contents()
        .to_vec();
    (!signature.is_empty() && signed.len() <= MAX_BYTES).then(|| Work::Verify {
        signed: crlf(signed),
        signature,
    })
}

/// The bytes of a multipart body's first part: after the first delimiter line, up to the line
/// break before the next delimiter (RFC 2046 §5.1.1: that line break belongs to the delimiter).
fn first_part<'a>(body: &'a [u8], boundary: &[u8]) -> Option<&'a [u8]> {
    let mut delimiter = b"--".to_vec();
    delimiter.extend_from_slice(boundary);
    let mut starts = line_starts(body).filter(|&at| body[at..].starts_with(&delimiter));
    let first = starts.next()?;
    let content = first + body[first..].iter().position(|&byte| byte == b'\n')? + 1;
    let next = starts.find(|&at| at > content)?;
    let mut end = next.checked_sub(1)?;
    if end > content && body[end - 1] == b'\r' {
        end -= 1;
    }
    body.get(content..end)
}

fn line_starts(body: &[u8]) -> impl Iterator<Item = usize> + '_ {
    std::iter::once(0).chain(
        body.iter()
            .enumerate()
            .filter(|(_, byte)| **byte == b'\n')
            .map(|(at, _)| at + 1)
            .filter(move |at| *at < body.len()),
    )
}

/// `bytes` with every line break as CRLF.
fn crlf(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / 32);
    let mut previous = 0u8;
    for &byte in bytes {
        if byte == b'\n' && previous != b'\r' {
            out.push(b'\r');
        }
        out.push(byte);
        previous = byte;
    }
    out
}

/// An inline PGP block in a text body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inline {
    /// The armoured `PGP MESSAGE` block, to decrypt.
    Encrypted(String),
    /// The whole clear-signed block, signature included, to check.
    Signed(String),
}

/// The first inline block of `text` that GnuPG can work on: its lines from the BEGIN line to
/// the END line.
pub fn inline(text: &str) -> Option<Inline> {
    let lines: Vec<&str> = text.lines().collect();
    let block = |start: &str, end: &str| {
        let first = lines.iter().position(|line| line.trim_end() == start)?;
        let last = first
            + lines[first..]
                .iter()
                .position(|line| line.trim_end() == end)?;
        Some(lines[first..=last].join("\n") + "\n")
    };
    if let Some(block) = block(ENCRYPTED_START, ENCRYPTED_END) {
        return Some(Inline::Encrypted(block));
    }
    block(SIGNED_START, SIGNATURE_END).map(Inline::Signed)
}

/// What GnuPG said, read from its `--status-fd` lines.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub decryption: Option<Decryption>,
    /// The first signature's outcome; GnuPG reports a signed and encrypted message's signature
    /// while decrypting it.
    pub signature: Option<SignatureStatus>,
    /// More than one signature was reported; only the first is read.
    pub signatures: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decryption {
    Okay,
    /// None of the user's secret keys is one the message was encrypted to.
    NoSecretKey,
    /// Cancelled, a wrong passphrase, damaged data or anything else.
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureStatus {
    pub outcome: Outcome,
    /// The long key ID or fingerprint GnuPG names.
    pub key: String,
    /// The user ID GnuPG names (the key's primary one), unescaped.
    pub user_id: Option<String>,
    /// The primary key's fingerprint, from VALIDSIG.
    pub fingerprint: Option<String>,
    pub trust: Trust,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Good,
    Bad,
    /// The signature itself has expired.
    ExpiredSignature,
    ExpiredKey,
    RevokedKey,
    /// The key isn't in the keyring.
    NoKey,
    Error,
}

/// How far the user's keyring vouches for the key (GnuPG's TRUST_* lines).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Trust {
    #[default]
    Undefined,
    Never,
    Marginal,
    Full,
    Ultimate,
}

/// Reads GnuPG's status lines (`[GNUPG:] KEYWORD …`) out of `output`; other lines are skipped.
pub fn status(output: &[u8]) -> Status {
    let mut status = Status::default();
    let mut decrypt_failed = false;
    let mut no_secret_key = false;
    let mut decrypted = false;
    for line in String::from_utf8_lossy(output).lines() {
        let Some(rest) = line.strip_prefix("[GNUPG:] ") else {
            continue;
        };
        let mut fields = rest.split(' ');
        let keyword = fields.next().unwrap_or_default();
        let arguments: Vec<&str> = fields.collect();
        let named = |outcome: Outcome| SignatureStatus {
            outcome,
            key: arguments.first().copied().unwrap_or_default().to_owned(),
            user_id: (arguments.len() > 1).then(|| unescape(&arguments[1..].join(" "))),
            fingerprint: None,
            trust: Trust::Undefined,
        };
        let outcome = match keyword {
            "DECRYPTION_OKAY" => {
                decrypted = true;
                None
            }
            "DECRYPTION_FAILED" => {
                decrypt_failed = true;
                None
            }
            "NO_SECKEY" => {
                no_secret_key = true;
                None
            }
            "NEWSIG" => {
                status.signatures += 1;
                None
            }
            "GOODSIG" => Some(named(Outcome::Good)),
            "BADSIG" => Some(named(Outcome::Bad)),
            "EXPSIG" => Some(named(Outcome::ExpiredSignature)),
            "EXPKEYSIG" => Some(named(Outcome::ExpiredKey)),
            "REVKEYSIG" => Some(named(Outcome::RevokedKey)),
            "ERRSIG" => {
                let missing = arguments.get(5) == Some(&"9");
                Some(SignatureStatus {
                    outcome: if missing {
                        Outcome::NoKey
                    } else {
                        Outcome::Error
                    },
                    key: arguments.first().copied().unwrap_or_default().to_owned(),
                    user_id: None,
                    fingerprint: arguments
                        .get(6)
                        .map(|fingerprint| (*fingerprint).to_owned()),
                    trust: Trust::Undefined,
                })
            }
            "VALIDSIG" => {
                if let Some(signature) = status.signature.as_mut()
                    && status.signatures <= 1
                {
                    signature.fingerprint = arguments
                        .get(9)
                        .or(arguments.first())
                        .map(|fingerprint| (*fingerprint).to_owned());
                }
                None
            }
            "TRUST_UNDEFINED" | "TRUST_NEVER" | "TRUST_MARGINAL" | "TRUST_FULLY"
            | "TRUST_ULTIMATE" => {
                if let Some(signature) = status.signature.as_mut()
                    && status.signatures <= 1
                {
                    signature.trust = match keyword {
                        "TRUST_NEVER" => Trust::Never,
                        "TRUST_MARGINAL" => Trust::Marginal,
                        "TRUST_FULLY" => Trust::Full,
                        "TRUST_ULTIMATE" => Trust::Ultimate,
                        _ => Trust::Undefined,
                    };
                }
                None
            }
            _ => None,
        };
        if let Some(outcome) = outcome
            && status.signature.is_none()
        {
            status.signature = Some(outcome);
        }
    }
    status.signatures = status
        .signatures
        .max(usize::from(status.signature.is_some()));
    status.decryption = if decrypted && !decrypt_failed {
        Some(Decryption::Okay)
    } else if no_secret_key && !decrypted {
        Some(Decryption::NoSecretKey)
    } else if decrypt_failed || no_secret_key {
        Some(Decryption::Failed)
    } else {
        None
    };
    status
}

/// GnuPG escapes a status line's user ID as `%XX` (RFC 4880 user IDs are UTF-8).
fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%'
            && let Some(byte) = bytes
                .get(at + 1..at + 3)
                .and_then(|hex| std::str::from_utf8(hex).ok())
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            at += 3;
            continue;
        }
        out.push(bytes[at]);
        at += 1;
    }
    clean(&String::from_utf8_lossy(&out))
}

/// No control or direction characters in text a user ID shows.
fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() && !crate::attachment::is_invisible(*c))
        .collect()
}

/// How far the user's keyring vouches for a key or user ID: the validity field of
/// `gpg --with-colons` (field 2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Validity {
    /// `o`, `-`, `q` and anything GnuPG adds later: nothing is known.
    #[default]
    Unknown,
    /// `i` or `d`.
    Invalid,
    Revoked,
    Expired,
    /// `n`: the keyring says never to rely on it.
    Never,
    Marginal,
    Full,
    Ultimate,
}

impl Validity {
    fn from_field(field: &str) -> Validity {
        match field {
            "i" | "d" => Validity::Invalid,
            "r" => Validity::Revoked,
            "e" => Validity::Expired,
            "n" => Validity::Never,
            "m" => Validity::Marginal,
            "f" => Validity::Full,
            "u" => Validity::Ultimate,
            _ => Validity::Unknown,
        }
    }

    /// The keyring vouches for it: fully valid or the user's own.
    pub fn verified(self) -> bool {
        matches!(self, Validity::Full | Validity::Ultimate)
    }

    /// It may still be used: not revoked, expired, invalid or distrusted.
    pub fn usable(self) -> bool {
        !matches!(
            self,
            Validity::Invalid | Validity::Revoked | Validity::Expired | Validity::Never
        )
    }
}

/// A user ID of a key and its own validity, which may differ from the key's: a user ID added
/// to a key the user certified isn't certified itself (#32).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserId {
    pub text: String,
    pub validity: Validity,
}

impl UserId {
    /// Its address, lower case.
    pub fn address(&self) -> Option<String> {
        user_id_address(&self.text)
    }
}

/// A key in `gpg --with-colons --fixed-list-mode --list-keys` (or `--list-secret-keys`)
/// output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Key {
    /// The primary key's fingerprint, upper-case hex.
    pub fingerprint: String,
    pub validity: Validity,
    /// The key as a whole can encrypt (an `E` among its capabilities: a usable subkey).
    pub can_encrypt: bool,
    /// The key as a whole can sign.
    pub can_sign: bool,
    /// Disabled by the user (`D`).
    pub disabled: bool,
    /// From `--list-secret-keys`: the secret key is there (`sec` and not a stub).
    pub secret: bool,
    pub user_ids: Vec<UserId>,
}

impl Key {
    /// Its user IDs with `address` that may be used, the verified ones first.
    pub fn user_ids_for(&self, address: &str) -> impl Iterator<Item = &UserId> {
        let address = address.to_lowercase();
        self.user_ids.iter().filter(move |user_id| {
            user_id.validity.usable() && user_id.address().as_deref() == Some(&address)
        })
    }

    /// The best validity among its user IDs with `address`: `None` when it has none that may
    /// be used.
    pub fn validity_for(&self, address: &str) -> Option<Validity> {
        let mut best = None;
        for user_id in self.user_ids_for(address) {
            if user_id.validity.verified() {
                return Some(user_id.validity);
            }
            best.get_or_insert(user_id.validity);
        }
        best
    }
}

/// The keys in `gpg --with-colons --fixed-list-mode` output, unescaped.
pub fn keys(colons: &str) -> Vec<Key> {
    let mut keys: Vec<Key> = Vec::new();
    let mut in_primary = false;
    for line in colons.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        let field = |at: usize| fields.get(at).copied().unwrap_or_default();
        match field(0) {
            kind @ ("pub" | "sec") => {
                let capabilities = field(11);
                keys.push(Key {
                    validity: Validity::from_field(field(1)),
                    can_encrypt: capabilities.contains('E'),
                    can_sign: capabilities.contains('S'),
                    disabled: capabilities.contains('D'),
                    // A `sec` record's field 15 is `#` for a stub and `>` for a card; a stub
                    // has no secret key here.
                    secret: kind == "sec" && !field(14).starts_with('#'),
                    ..Key::default()
                });
                in_primary = true;
            }
            "fpr" if in_primary => {
                if let Some(key) = keys.last_mut() {
                    key.fingerprint = field(9).to_ascii_uppercase();
                }
                in_primary = false;
            }
            "uid" => {
                if let Some(key) = keys.last_mut() {
                    key.user_ids.push(UserId {
                        text: clean(&colon_unescape(field(9))),
                        validity: Validity::from_field(field(1)),
                    });
                }
                in_primary = false;
            }
            "sub" | "ssb" | "uat" => in_primary = false,
            _ => {}
        }
    }
    keys.retain(|key| !key.fingerprint.is_empty());
    keys
}

/// The user IDs in `gpg --with-colons --list-keys` output that aren't revoked or expired, with
/// their validity.
pub fn user_ids(colons: &str) -> Vec<UserId> {
    colons
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            let validity = Validity::from_field(fields.get(1).copied().unwrap_or_default());
            (fields.first() == Some(&"uid")
                && !matches!(validity, Validity::Revoked | Validity::Expired))
            .then(|| {
                fields.get(9).map(|text| UserId {
                    text: clean(&colon_unescape(text)),
                    validity,
                })
            })
            .flatten()
        })
        .collect()
}

/// `--with-colons` escapes as `\xNN`.
fn colon_unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'\\'
            && bytes.get(at + 1) == Some(&b'x')
            && let Some(byte) = bytes
                .get(at + 2..at + 4)
                .and_then(|hex| std::str::from_utf8(hex).ok())
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            at += 4;
            continue;
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The address in a user ID such as `Ada Lovelace <ada@example.com>`, lower case.
pub fn user_id_address(user_id: &str) -> Option<String> {
    let address = match (user_id.rfind('<'), user_id.rfind('>')) {
        (Some(open), Some(close)) if open < close => &user_id[open + 1..close],
        _ => user_id.trim(),
    };
    (address.contains('@') && !address.contains(char::is_whitespace))
        .then(|| address.to_lowercase())
}

/// What the reader may say about a signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Good, and the key's user ID with the sender's address valid in the user's keyring.
    Checked {
        signer: String,
    },
    /// Good and the key valid, but none of its user IDs is the sender's address.
    OtherSigner {
        signer: String,
    },
    /// Good, but the user's keyring doesn't vouch for the key.
    KeyNotVerified {
        signer: String,
    },
    /// Good, but the user's keyring says never to trust the key.
    KeyDistrusted {
        signer: String,
    },
    /// The content doesn't match the signature.
    Bad,
    Expired {
        signer: String,
    },
    Revoked {
        signer: String,
    },
    /// The key isn't in the user's keyring.
    MissingKey {
        key: String,
    },
    /// GnuPG couldn't check it.
    Error,
}

/// The verdict on `signature`: `user_ids` are the signing key's user IDs (from [`user_ids`]),
/// `sender` the message's From address. "Checked" needs the user ID with the sender's address
/// to be valid in the keyring itself: GnuPG's TRUST line speaks for the key's primary user ID,
/// and anyone can add a user ID to a key someone certified (#32).
pub fn verdict(signature: &SignatureStatus, user_ids: &[UserId], sender: Option<&str>) -> Verdict {
    let sender = sender.map(str::to_lowercase);
    let matching: Vec<&UserId> = user_ids
        .iter()
        .filter(|user_id| sender.is_some() && user_id.address() == sender)
        .collect();
    let verified = matching
        .iter()
        .find(|user_id| user_id.validity.verified())
        .copied();
    let signer = verified
        .or_else(|| matching.first().copied())
        .map(|user_id| user_id.text.clone())
        .or_else(|| signature.user_id.clone())
        .or_else(|| user_ids.first().map(|user_id| user_id.text.clone()))
        .unwrap_or_else(|| signature.key.clone());
    match signature.outcome {
        Outcome::Good => match signature.trust {
            Trust::Never => Verdict::KeyDistrusted { signer },
            _ if verified.is_some() => Verdict::Checked { signer },
            _ if !matching.is_empty() => Verdict::KeyNotVerified { signer },
            Trust::Full | Trust::Ultimate => Verdict::OtherSigner { signer },
            Trust::Undefined | Trust::Marginal => Verdict::KeyNotVerified { signer },
        },
        Outcome::Bad => Verdict::Bad,
        Outcome::ExpiredSignature | Outcome::ExpiredKey => Verdict::Expired { signer },
        Outcome::RevokedKey => Verdict::Revoked { signer },
        Outcome::NoKey => Verdict::MissingKey {
            key: signature
                .fingerprint
                .clone()
                .unwrap_or_else(|| signature.key.clone()),
        },
        Outcome::Error => Verdict::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIGNED: &str = "From: Ada <ada@example.com>\r\n\
        To: Bob <bob@example.com>\r\n\
        Subject: Signed\r\n\
        MIME-Version: 1.0\r\n\
        Content-Type: multipart/signed; micalg=pgp-sha256;\r\n \
        protocol=\"application/pgp-signature\"; boundary=\"sig\"\r\n\
        \r\n\
        This is an OpenPGP/MIME signed message.\r\n\
        --sig\r\n\
        Content-Type: text/plain; charset=utf-8\r\n\
        \r\n\
        Hello Bob.\r\n\
        \r\n\
        --sig\r\n\
        Content-Type: application/pgp-signature; name=\"signature.asc\"\r\n\
        \r\n\
        -----BEGIN PGP SIGNATURE-----\r\n\
        \r\n\
        iQ==\r\n\
        -----END PGP SIGNATURE-----\r\n\
        --sig--\r\n";

    #[test]
    fn a_signed_message_hands_over_its_first_part_as_signed_and_the_signature() {
        let Some(Work::Verify { signed, signature }) = work(SIGNED.as_bytes()) else {
            panic!("no verify work");
        };
        assert_eq!(
            signed,
            b"Content-Type: text/plain; charset=utf-8\r\n\r\nHello Bob.\r\n".to_vec()
        );
        assert!(signature.starts_with(b"-----BEGIN PGP SIGNATURE-----"));
    }

    #[test]
    fn bare_line_feeds_become_crlf_in_what_is_checked() {
        let lf = SIGNED.replace("\r\n", "\n");
        let Some(Work::Verify { signed, .. }) = work(lf.as_bytes()) else {
            panic!("no verify work");
        };
        assert_eq!(
            signed,
            b"Content-Type: text/plain; charset=utf-8\r\n\r\nHello Bob.\r\n".to_vec()
        );
    }

    #[test]
    fn a_signed_part_further_down_is_not_checked() {
        let wrapped = format!(
            "From: list@example.com\r\nMIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"outer\"\r\n\r\n\
             --outer\r\n{}\r\n--outer\r\nContent-Type: text/plain\r\n\r\nfooter\r\n--outer--\r\n",
            SIGNED
                .split_once("MIME-Version: 1.0\r\n")
                .map_or("", |(_, rest)| rest)
        );
        assert_eq!(work(wrapped.as_bytes()), None);
    }

    #[test]
    fn an_encrypted_message_hands_over_its_ciphertext() {
        let raw = "From: a@example.com\r\nMIME-Version: 1.0\r\n\
            Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"e\"\r\n\r\n\
            --e\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n\
            --e\r\nContent-Type: application/octet-stream\r\n\r\n\
            -----BEGIN PGP MESSAGE-----\r\n\r\nhQ==\r\n-----END PGP MESSAGE-----\r\n--e--\r\n";
        let Some(Work::Decrypt(ciphertext)) = work(raw.as_bytes()) else {
            panic!("no decrypt work");
        };
        assert!(ciphertext.starts_with(b"-----BEGIN PGP MESSAGE-----"));
    }

    #[test]
    fn plain_mail_needs_nothing() {
        assert_eq!(
            work(b"From: a@example.com\r\nSubject: x\r\n\r\nHello\r\n"),
            None
        );
    }

    #[test]
    fn inline_blocks_are_cut_from_the_text() {
        let text = "Hi\n-----BEGIN PGP MESSAGE-----\n\nhQ==\n-----END PGP MESSAGE-----\nbye";
        assert_eq!(
            inline(text),
            Some(Inline::Encrypted(
                "-----BEGIN PGP MESSAGE-----\n\nhQ==\n-----END PGP MESSAGE-----\n".to_owned()
            ))
        );
        let signed = "-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA256\n\nHello\n\
            -----BEGIN PGP SIGNATURE-----\n\niQ==\n-----END PGP SIGNATURE-----\n";
        assert_eq!(inline(signed), Some(Inline::Signed(signed.to_owned())));
        assert_eq!(inline("-----BEGIN PGP MESSAGE-----\nno end"), None);
    }

    fn good(trust: &str) -> Status {
        status(
            format!(
                "gpg: Signature made …\n\
                 [GNUPG:] NEWSIG\n\
                 [GNUPG:] GOODSIG 0123456789ABCDEF Ada Lovelace <ada@example.com>\n\
                 [GNUPG:] VALIDSIG AAAA 2026-11-18 1 0 4 0 22 8 00 FFFF\n\
                 [GNUPG:] {trust} 0 pgp\n"
            )
            .as_bytes(),
        )
    }

    #[test]
    fn a_good_signature_is_checked_only_with_a_valid_key_for_the_sender() {
        let ada = vec![UserId {
            text: "Ada Lovelace <ada@example.com>".to_owned(),
            validity: Validity::Ultimate,
        }];
        let signature = good("TRUST_ULTIMATE").signature.unwrap_or_else(|| panic!());
        assert_eq!(signature.fingerprint.as_deref(), Some("FFFF"));
        assert_eq!(
            verdict(&signature, &ada, Some("ADA@example.com")),
            Verdict::Checked {
                signer: "Ada Lovelace <ada@example.com>".to_owned()
            }
        );
        assert!(matches!(
            verdict(&signature, &ada, Some("mallory@example.com")),
            Verdict::OtherSigner { .. }
        ));
        let unknown = good("TRUST_UNDEFINED")
            .signature
            .unwrap_or_else(|| panic!());
        let unverified = vec![UserId {
            validity: Validity::Unknown,
            ..ada[0].clone()
        }];
        assert!(matches!(
            verdict(&unknown, &unverified, Some("ada@example.com")),
            Verdict::KeyNotVerified { .. }
        ));
        let never = good("TRUST_NEVER").signature.unwrap_or_else(|| panic!());
        assert!(matches!(
            verdict(&never, &ada, Some("ada@example.com")),
            Verdict::KeyDistrusted { .. }
        ));
    }

    /// #32: Mallory adds `The Boss` to a key whose own user ID the user certified; GnuPG's
    /// TRUST line is the key's (fully trusted), the added user ID's validity is unknown.
    #[test]
    fn a_user_id_the_keyring_does_not_vouch_for_is_not_checked() {
        let fully = good("TRUST_FULLY").signature.unwrap_or_else(|| panic!());
        let ids = user_ids(
            "pub:f:255:22:01:1:::-:::scESC::::::23::0:\n\
             fpr:::::::::FFFF:\n\
             uid:f::::1::AA::Mallory <mallory@example.com>::::::::::0:\n\
             uid:-::::2::BB::The Boss <boss@example.com>::::::::::0:\n",
        );
        assert_eq!(ids[1].validity, Validity::Unknown);
        assert_eq!(
            verdict(&fully, &ids, Some("boss@example.com")),
            Verdict::KeyNotVerified {
                signer: "The Boss <boss@example.com>".to_owned()
            }
        );
        assert!(matches!(
            verdict(&fully, &ids, Some("mallory@example.com")),
            Verdict::Checked { .. }
        ));
        let marginal: Vec<UserId> = ids
            .iter()
            .map(|id| UserId {
                validity: Validity::Marginal,
                ..id.clone()
            })
            .collect();
        assert!(matches!(
            verdict(&fully, &marginal, Some("mallory@example.com")),
            Verdict::KeyNotVerified { .. }
        ));
    }

    #[test]
    fn keys_are_read_with_their_capabilities_and_user_ids() {
        let listed = keys(
            "tru::1:1700000000:0:3:1:5\n\
             pub:u:255:22:AAAA:1:::u:::scESC::::::23::0:\n\
             fpr:::::::::aaaa1111:\n\
             uid:u::::1::X::Ada <ada@example.com>::::::::::0:\n\
             uid:r::::1::Y::Old <ada@old.example>::::::::::0:\n\
             sub:u:255:18:BBBB:1::::::e::::::cv25519::\n\
             fpr:::::::::BBBB2222:\n\
             pub:-:255:22:CCCC:1:::-:::scSCD::::::23::0:\n\
             fpr:::::::::CCCC3333:\n\
             uid:-::::1::Z::Bob <bob@example.com>::::::::::0:\n",
        );
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].fingerprint, "AAAA1111");
        assert!(listed[0].can_encrypt && listed[0].can_sign && !listed[0].disabled);
        assert_eq!(
            listed[0].validity_for("ADA@example.com"),
            Some(Validity::Ultimate)
        );
        assert_eq!(listed[0].validity_for("ada@old.example"), None);
        assert!(!listed[1].can_encrypt && listed[1].disabled);
        assert_eq!(
            listed[1].validity_for("bob@example.com"),
            Some(Validity::Unknown)
        );
        let secret = keys(
            "sec:u:255:22:AAAA:1:::u:::scESC:::+:::23::0:\n\
             fpr:::::::::AAAA1111:\n\
             sec:u:255:22:DDDD:1:::u:::scESC:::#:::23::0:\n\
             fpr:::::::::DDDD4444:\n",
        );
        assert!(secret[0].secret && !secret[1].secret);
    }

    #[test]
    fn bad_missing_and_failed_answers_are_read() {
        let bad = status(b"[GNUPG:] NEWSIG\n[GNUPG:] BADSIG 0123 Ada <ada@example.com>\n");
        assert_eq!(
            verdict(&bad.signature.unwrap_or_else(|| panic!()), &[], None),
            Verdict::Bad
        );
        let missing = status(b"[GNUPG:] NEWSIG\n[GNUPG:] ERRSIG 0123 22 8 00 1700000000 9 ABCD\n[GNUPG:] NO_PUBKEY 0123\n");
        assert_eq!(
            verdict(&missing.signature.unwrap_or_else(|| panic!()), &[], None),
            Verdict::MissingKey {
                key: "ABCD".to_owned()
            }
        );
        let no_key = status(b"[GNUPG:] ENC_TO 0123 18 0\n[GNUPG:] NO_SECKEY 0123\n[GNUPG:] BEGIN_DECRYPTION\n[GNUPG:] DECRYPTION_FAILED\n[GNUPG:] END_DECRYPTION\n");
        assert_eq!(no_key.decryption, Some(Decryption::NoSecretKey));
        let cancelled = status(b"[GNUPG:] BEGIN_DECRYPTION\n[GNUPG:] DECRYPTION_FAILED\n");
        assert_eq!(cancelled.decryption, Some(Decryption::Failed));
        let okay = status(
            b"[GNUPG:] BEGIN_DECRYPTION\n[GNUPG:] DECRYPTION_OKAY\n[GNUPG:] END_DECRYPTION\n",
        );
        assert_eq!(okay.decryption, Some(Decryption::Okay));
        assert_eq!(okay.signature, None);
    }

    #[test]
    fn user_ids_are_unescaped_and_cleaned() {
        let status = status(b"[GNUPG:] GOODSIG 01 Ada%0A %25 <ada@example.com>\n");
        assert_eq!(
            status.signature.and_then(|signature| signature.user_id),
            Some("Ada % <ada@example.com>".to_owned())
        );
        let colons = "pub:u:255:22:01:1:::u:::scESC::::::23::0:\n\
            uid:u::::1::AA::Ada Lovelace <ada@example.com>::::::::::0:\n\
            uid:r::::1::BB::Old <old@example.com>::::::::::0:\n\
            uid:u::::1::CC::Colon\\x3a Name <c@example.com>::::::::::0:\n";
        assert_eq!(
            user_ids(colons)
                .into_iter()
                .map(|id| id.text)
                .collect::<Vec<_>>(),
            vec![
                "Ada Lovelace <ada@example.com>".to_owned(),
                "Colon: Name <c@example.com>".to_owned()
            ]
        );
        assert_eq!(
            user_id_address("Ada <Ada@Example.com>").as_deref(),
            Some("ada@example.com")
        );
        assert_eq!(user_id_address("No address"), None);
    }
}
