//! Signing and encrypting a message on send (#32, ADR-032): which keys of the user's keyring
//! it goes to, and the PGP/MIME message (RFC 3156) made from the MIME builder's message and
//! what GnuPG returned. Pure: `sealwax_infrastructure::pgp` lists the keys and runs `gpg`.
//!
//! - **Recipients' keys:** a key whose user ID has the recipient's address, that can encrypt,
//!   and that isn't expired, revoked, disabled or distrusted. A verified one (full or ultimate
//!   validity for that address) is taken over unverified ones; when there is no verified key
//!   and several unverified ones, the user picks. A recipient without a key refuses the send.
//! - **Bcc recipients** are hidden recipients (`--hidden-recipient`), so the message names no
//!   key of theirs.
//! - **The user's own key** signs, and the message is encrypted to it too, so the Sent copy
//!   can be read: the account's `pgp_key` when it names one, otherwise the secret key whose
//!   user ID has the From address (several of them: the user names one in `accounts.toml`).
//! - **The message:** the builder's top-level content becomes the signed or encrypted entity;
//!   the other header fields (Subject among them) stay outside. Signed and encrypted is one
//!   OpenPGP message (RFC 3156 6.2), as the reader expects.

use super::Draft;
use crate::pgp::Key;
use std::fmt;

/// Whether a draft is signed and encrypted when it is sent.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Protection {
    pub sign: bool,
    pub encrypt: bool,
    /// The keys the user picked for recipients with several unverified ones: the address,
    /// lower case, and the key's fingerprint.
    pub picked: Vec<(String, String)>,
}

impl Protection {
    pub fn any(&self) -> bool {
        self.sign || self.encrypt
    }

    /// The fingerprint picked for `address`.
    pub fn picked_for(&self, address: &str) -> Option<&str> {
        let address = address.to_lowercase();
        self.picked
            .iter()
            .find(|(picked, _)| *picked == address)
            .map(|(_, fingerprint)| fingerprint.as_str())
    }

    /// Records `fingerprint` as the key picked for `address`.
    pub fn pick(&mut self, address: &str, fingerprint: &str) {
        let address = address.to_lowercase();
        self.picked.retain(|(picked, _)| *picked != address);
        self.picked
            .push((address, fingerprint.to_ascii_uppercase()));
    }

    /// The store's form of `picked`: one `address fingerprint` per line.
    pub fn encode_picked(&self) -> String {
        self.picked
            .iter()
            .map(|(address, fingerprint)| format!("{address} {fingerprint}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn decode_picked(text: &str) -> Vec<(String, String)> {
        text.lines()
            .filter_map(|line| {
                let (address, fingerprint) = line.split_once(' ')?;
                (is_fingerprint(fingerprint) && address.contains('@'))
                    .then(|| (address.to_lowercase(), fingerprint.to_ascii_uppercase()))
            })
            .collect()
    }
}

impl fmt::Debug for Protection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Protection")
            .field("sign", &self.sign)
            .field("encrypt", &self.encrypt)
            .field("picked", &self.picked.len())
            .finish()
    }
}

/// A fingerprint as `accounts.toml` and the store hold it: 40 or 64 hex digits (v4, v5/v6).
pub fn is_fingerprint(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.chars().all(|c| c.is_ascii_hexdigit())
}

/// A key one could encrypt to for an address.
#[derive(Clone, PartialEq, Eq)]
pub struct Candidate {
    pub fingerprint: String,
    /// The user ID with the address, as the keyring has it.
    pub user_id: String,
    pub verified: bool,
}

impl fmt::Debug for Candidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Candidate")
            .field("fingerprint", &self.fingerprint)
            .field("verified", &self.verified)
            .finish()
    }
}

/// The key a recipient's copy is encrypted to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecipientKey {
    Chosen(Candidate),
    /// No key in the keyring can be used.
    Missing,
    /// Several unverified keys (or several verified ones), none picked: the user picks.
    Choose(Vec<Candidate>),
}

/// The keys of `keys` that `address` could be encrypted to, the verified ones only when there
/// are any.
pub fn candidates(address: &str, keys: &[Key]) -> Vec<Candidate> {
    let usable: Vec<Candidate> = keys
        .iter()
        .filter(|key| key.can_encrypt && !key.disabled && key.validity.usable())
        .filter_map(|key| {
            let validity = key.validity_for(address)?;
            let user_id = key.user_ids_for(address).next()?.text.clone();
            Some(Candidate {
                fingerprint: key.fingerprint.clone(),
                user_id,
                verified: validity.verified(),
            })
        })
        .collect();
    if usable.iter().any(|candidate| candidate.verified) {
        usable.into_iter().filter(|c| c.verified).collect()
    } else {
        usable
    }
}

/// The key for `address` among `keys`, `picked` being the fingerprint the user picked for it.
pub fn recipient_key(address: &str, keys: &[Key], picked: Option<&str>) -> RecipientKey {
    let mut candidates = candidates(address, keys);
    if let Some(picked) = picked
        && let Some(at) = candidates
            .iter()
            .position(|candidate| candidate.fingerprint.eq_ignore_ascii_case(picked))
    {
        return RecipientKey::Chosen(candidates.swap_remove(at));
    }
    match candidates.len() {
        0 => RecipientKey::Missing,
        1 => RecipientKey::Chosen(candidates.remove(0)),
        _ => RecipientKey::Choose(candidates),
    }
}

/// Why the user's own key can't be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnKeyProblem {
    /// No secret key for the From address (or the configured one isn't in the keyring).
    Missing,
    /// The configured key, or the From address's, can't sign (or can't encrypt).
    Unusable,
    /// Several secret keys for the From address and none configured.
    Several,
}

/// The user's key for `from`: `configured` (the account's `pgp_key`) when given, else the one
/// secret key with a user ID of that address. `secret` is `--list-secret-keys` output.
pub fn own_key(
    from: &str,
    secret: &[Key],
    configured: Option<&str>,
    protection: &Protection,
) -> Result<String, OwnKeyProblem> {
    let fits = |key: &Key| {
        key.secret
            && !key.disabled
            && key.validity.usable()
            && (!protection.sign || key.can_sign)
            && (!protection.encrypt || key.can_encrypt)
    };
    if let Some(configured) = configured {
        let key = secret
            .iter()
            .find(|key| key.fingerprint.eq_ignore_ascii_case(configured))
            .ok_or(OwnKeyProblem::Missing)?;
        return fits(key)
            .then(|| key.fingerprint.clone())
            .ok_or(OwnKeyProblem::Unusable);
    }
    let mine: Vec<&Key> = secret
        .iter()
        .filter(|key| key.secret && key.validity_for(from).is_some())
        .collect();
    let usable: Vec<&&Key> = mine.iter().filter(|key| fits(key)).collect();
    match (mine.len(), usable.as_slice()) {
        (0, _) => Err(OwnKeyProblem::Missing),
        (_, []) => Err(OwnKeyProblem::Unusable),
        (_, [key]) => Ok(key.fingerprint.clone()),
        _ => Err(OwnKeyProblem::Several),
    }
}

/// Why a protected draft can't be sent as it is. Names addresses only for the user's eyes; its
/// `Debug` prints counts.
#[derive(Clone, PartialEq, Eq)]
pub enum Problem {
    /// GnuPG isn't installed, or Sealwax runs where it can't reach it (the Flatpak).
    NoGnuPG,
    /// The user's own key.
    OwnKey(OwnKeyProblem),
    /// Recipients with no key (encrypting), lower case, in the draft's order.
    Missing(Vec<String>),
    /// Recipients whose key the user must pick, with the keys.
    Choose(Vec<(String, Vec<Candidate>)>),
}

impl fmt::Debug for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Problem::NoGnuPG => f.write_str("NoGnuPG"),
            Problem::OwnKey(problem) => write!(f, "OwnKey({problem:?})"),
            Problem::Missing(list) => write!(f, "Missing({})", list.len()),
            Problem::Choose(list) => write!(f, "Choose({})", list.len()),
        }
    }
}

/// What to hand GnuPG for a draft.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// The key that signs, when signing.
    pub signer: Option<String>,
    /// To, Cc and the user's own key, when encrypting.
    pub recipients: Vec<String>,
    /// Bcc.
    pub hidden: Vec<String>,
    /// Recipients whose key isn't verified, lower case: the composer names them.
    pub unverified: Vec<String>,
}

impl fmt::Debug for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Plan")
            .field("signs", &self.signer.is_some())
            .field("recipients", &self.recipients.len())
            .field("hidden", &self.hidden.len())
            .field("unverified", &self.unverified.len())
            .finish()
    }
}

/// Every address the draft goes to, lower case, each once: To, Cc, then Bcc, with whether it
/// is a Bcc.
pub fn recipients(draft: &Draft) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    for (list, hidden) in [(&draft.to, false), (&draft.cc, false), (&draft.bcc, true)] {
        for address in list {
            let address = address.email.trim().to_lowercase();
            if !address.is_empty() && !out.iter().any(|(known, _)| *known == address) {
                out.push((address, hidden));
            }
        }
    }
    out
}

/// The plan for `draft` from the keyring's listings: `public` from `--list-keys`, `secret`
/// from `--list-secret-keys`, `configured` the account's `pgp_key`. Every problem is found at
/// once: missing keys before keys to pick.
pub fn plan(
    draft: &Draft,
    public: &[Key],
    secret: &[Key],
    configured: Option<&str>,
) -> Result<Plan, Problem> {
    let protection = &draft.protection;
    let mut plan = Plan::default();
    if !protection.any() {
        return Ok(plan);
    }
    let own =
        own_key(&draft.from.email, secret, configured, protection).map_err(Problem::OwnKey)?;
    if protection.sign {
        plan.signer = Some(own.clone());
    }
    if !protection.encrypt {
        return Ok(plan);
    }
    let mut missing = Vec::new();
    let mut choose = Vec::new();
    for (address, hidden) in recipients(draft) {
        match recipient_key(&address, public, protection.picked_for(&address)) {
            RecipientKey::Chosen(candidate) => {
                if !candidate.verified {
                    plan.unverified.push(address.clone());
                }
                let list = if hidden {
                    &mut plan.hidden
                } else {
                    &mut plan.recipients
                };
                if !list.contains(&candidate.fingerprint) {
                    list.push(candidate.fingerprint);
                }
            }
            RecipientKey::Missing => missing.push(address),
            RecipientKey::Choose(candidates) => choose.push((address, candidates)),
        }
    }
    if !missing.is_empty() {
        return Err(Problem::Missing(missing));
    }
    if !choose.is_empty() {
        return Err(Problem::Choose(choose));
    }
    if !plan.recipients.contains(&own) {
        plan.recipients.push(own);
    }
    plan.hidden.retain(|key| !plan.recipients.contains(key));
    Ok(plan)
}

/// A message cut in two: its header fields other than the content's, and the content as a
/// MIME entity (its Content-* fields, an empty line, the body).
#[derive(Clone, PartialEq, Eq)]
pub struct Split {
    pub outer: Vec<u8>,
    pub entity: Vec<u8>,
}

impl fmt::Debug for Split {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Split")
            .field("outer", &self.outer.len())
            .field("entity", &self.entity.len())
            .finish()
    }
}

/// Cuts the MIME builder's `message` (CRLF line ends) into its outer header fields and its
/// content. `MIME-Version` is dropped (the wrapped message writes its own); `None` when the
/// message has no header end.
pub fn split(message: &[u8]) -> Option<Split> {
    let end = message
        .windows(4)
        .position(|window| window == b"\r\n\r\n")?;
    let header = &message[..end + 2];
    let body = &message[end + 4..];
    let mut outer = Vec::new();
    let mut entity = Vec::new();
    let mut field: Vec<u8> = Vec::new();
    let mut flush = |field: &mut Vec<u8>| {
        if field.is_empty() {
            return;
        }
        let name = field
            .split(|byte| *byte == b':')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if name.starts_with(b"content-") {
            entity.extend_from_slice(field);
        } else if name != b"mime-version" {
            outer.extend_from_slice(field);
        }
        field.clear();
    };
    for line in header.split_inclusive(|byte| *byte == b'\n') {
        if line
            .first()
            .is_some_and(|byte| *byte == b' ' || *byte == b'\t')
        {
            field.extend_from_slice(line);
        } else {
            flush(&mut field);
            field.extend_from_slice(line);
        }
    }
    flush(&mut field);
    if entity.is_empty() {
        entity.extend_from_slice(b"Content-Type: text/plain; charset=us-ascii\r\n");
    }
    entity.extend_from_slice(b"\r\n");
    entity.extend_from_slice(body);
    Some(Split { outer, entity })
}

/// `boundary` isn't anywhere in `parts`, so it can delimit them.
fn fits(boundary: &str, parts: &[&[u8]]) -> bool {
    !boundary.is_empty()
        && parts.iter().all(|part| {
            part.windows(boundary.len())
                .all(|window| window != boundary.as_bytes())
        })
}

fn with_line_end(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    if !out.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// The signed message (RFC 3156 5): `split`'s entity as the first part, exactly as it was
/// signed, and `signature` (armoured) as the second. `None` when `boundary` occurs in either.
pub fn signed(split: &Split, signature: &[u8], micalg: &str, boundary: &str) -> Option<Vec<u8>> {
    if !fits(boundary, &[&split.entity, signature]) {
        return None;
    }
    let mut out = split.outer.clone();
    out.extend_from_slice(
        format!(
            "MIME-Version: 1.0\r\nContent-Type: multipart/signed; micalg={micalg};\r\n \
             protocol=\"application/pgp-signature\"; boundary=\"{boundary}\"\r\n\r\n\
             This is an OpenPGP/MIME signed message (RFC 4880 and 3156)\r\n--{boundary}\r\n"
        )
        .as_bytes(),
    );
    out.extend_from_slice(&split.entity);
    if !split.entity.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Type: application/pgp-signature; name=\"signature.asc\"\r\n\
             Content-Description: OpenPGP digital signature\r\n\
             Content-Disposition: attachment; filename=\"signature.asc\"\r\n\r\n"
        )
        .as_bytes(),
    );
    out.extend_from_slice(&with_line_end(signature));
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Some(out)
}

/// What is signed for [`signed`]: the entity with its line ends as they will be sent, without
/// the line end that belongs to the boundary after it.
pub fn signed_bytes(split: &Split) -> &[u8] {
    split.entity.strip_suffix(b"\r\n").unwrap_or(&split.entity)
}

/// The encrypted message (RFC 3156 4): the version part, then `ciphertext` (armoured).
/// `None` when `boundary` occurs in it.
pub fn encrypted(split: &Split, ciphertext: &[u8], boundary: &str) -> Option<Vec<u8>> {
    if !fits(boundary, &[ciphertext]) {
        return None;
    }
    let mut out = split.outer.clone();
    out.extend_from_slice(
        format!(
            "MIME-Version: 1.0\r\nContent-Type: multipart/encrypted;\r\n \
             protocol=\"application/pgp-encrypted\"; boundary=\"{boundary}\"\r\n\r\n\
             This is an OpenPGP/MIME encrypted message (RFC 4880 and 3156)\r\n\
             --{boundary}\r\nContent-Type: application/pgp-encrypted\r\n\
             Content-Description: PGP/MIME version identification\r\n\r\nVersion: 1\r\n\r\n\
             --{boundary}\r\nContent-Type: application/octet-stream; name=\"encrypted.asc\"\r\n\
             Content-Description: OpenPGP encrypted message\r\n\
             Content-Disposition: inline; filename=\"encrypted.asc\"\r\n\r\n"
        )
        .as_bytes(),
    );
    out.extend_from_slice(&with_line_end(ciphertext));
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Some(out)
}

/// A boundary from 16 random bytes.
pub fn boundary(random: [u8; 16]) -> String {
    let hex: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("=-pgp-{hex}")
}

/// The name an exported public key is attached under: `OpenPGP_0x` and the last 16 digits of
/// its fingerprint, as other clients name it.
pub fn public_key_name(fingerprint: &str) -> String {
    let start = fingerprint.len().saturating_sub(16);
    format!("OpenPGP_0x{}.asc", &fingerprint[start..])
}

/// A reply to `original` starts protected like it: Encrypt on when it was encrypted, Sign on
/// when it was signed (#32: mlamoure's answer), on top of the account's defaults.
pub fn for_reply(defaults: &Protection, encrypted: bool, signed: bool) -> Protection {
    Protection {
        sign: defaults.sign || (encrypted && signed),
        encrypt: defaults.encrypt || encrypted,
        picked: Vec::new(),
    }
}

#[cfg(test)]
mod tests;
