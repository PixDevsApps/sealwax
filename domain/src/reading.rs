//! What the reader needs from a raw message beyond what the store keeps (ML-064), from one
//! parse: which text parts are `format=flowed` (RFC 3676), whether the message is encrypted
//! (PGP/MIME, RFC 3156; S/MIME enveloped data, RFC 8551) or signed, and which parts are
//! signatures that the attachment cards leave out. Sealwax 1.0 neither decrypts nor verifies:
//! encrypted mail says so in place of its body, and signed mail shows its content.

use crate::flowed::Flowed;
use mail_parser::{MessageParser, MimeHeaders, PartType};

/// How a message is encrypted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encryption {
    /// `multipart/encrypted` with `application/pgp-encrypted` (RFC 3156).
    PgpMime,
    /// `application/pkcs7-mime` with enveloped data (RFC 8551).
    Smime,
    /// A `-----BEGIN PGP MESSAGE-----` block in the text.
    InlinePgp,
}

/// How a message is signed. Signatures aren't checked in 1.0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signature {
    /// `multipart/signed` with `application/pgp-signature` (RFC 3156).
    PgpMime,
    /// `multipart/signed` with `application/pkcs7-signature` (RFC 8551).
    Smime,
    /// `application/pkcs7-mime` with signed data: the content is inside the signature, so it
    /// can't be shown without an S/MIME implementation.
    SmimeOpaque,
    /// A `-----BEGIN PGP SIGNED MESSAGE-----` block in the text (RFC 4880 cleartext).
    InlinePgp,
}

/// The facts [`inspect`] finds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inspection {
    /// The `format=flowed` text parts, by mail-parser part index.
    pub flowed: Vec<(u32, Flowed)>,
    pub encrypted: Option<Encryption>,
    pub signed: Option<Signature>,
    /// Parts the attachment cards leave out: signatures, and PGP/MIME's version part.
    pub hidden: Vec<u32>,
}

const PGP_SIGNATURE: &str = "application/pgp-signature";
const SMIME_SIGNATURES: [&str; 2] = [
    "application/pkcs7-signature",
    "application/x-pkcs7-signature",
];
const SMIME_CONTENT: [&str; 2] = ["application/pkcs7-mime", "application/x-pkcs7-mime"];

pub fn inspect(raw: &[u8]) -> Inspection {
    let mut found = Inspection::default();
    let Some(message) = MessageParser::default().parse(raw) else {
        return found;
    };
    for (index, part) in message.parts.iter().enumerate() {
        let Ok(index) = u32::try_from(index) else {
            break;
        };
        let content_type = part.content_type();
        let kind = content_type
            .map(|content_type| {
                format!(
                    "{}/{}",
                    content_type.ctype(),
                    content_type.subtype().unwrap_or_default()
                )
                .to_ascii_lowercase()
            })
            .unwrap_or_default();
        let attribute =
            |name: &str| content_type.and_then(|content_type| attribute(content_type, name));
        let name = part
            .attachment_name()
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        match &part.body {
            PartType::Multipart(children) if kind == "multipart/signed" => {
                let protocol = attribute("protocol")
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                found.signed.get_or_insert(if protocol == PGP_SIGNATURE {
                    Signature::PgpMime
                } else {
                    Signature::Smime
                });
                if let Some(signature) = children.get(1) {
                    found.hidden.push(*signature);
                }
            }
            PartType::Multipart(children) if kind == "multipart/encrypted" => {
                found.encrypted.get_or_insert(Encryption::PgpMime);
                if let Some(version) = children.first() {
                    found.hidden.push(*version);
                }
            }
            PartType::Text(_) if kind.is_empty() || kind == "text/plain" => {
                let flowed = Flowed::from_params(attribute("format"), attribute("delsp"));
                if let Some(flowed) = flowed {
                    found.flowed.push((index, flowed));
                }
            }
            PartType::Multipart(_) | PartType::Message(_) => {}
            _ if SMIME_CONTENT.contains(&kind.as_str()) || name.ends_with(".p7m") => {
                let smime_type = attribute("smime-type")
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if smime_type == "signed-data" {
                    found.signed.get_or_insert(Signature::SmimeOpaque);
                } else if smime_type != "certs-only" {
                    found.encrypted.get_or_insert(Encryption::Smime);
                }
            }
            _ if kind == PGP_SIGNATURE
                || SMIME_SIGNATURES.contains(&kind.as_str())
                || name.ends_with(".p7s") =>
            {
                found.hidden.push(index);
            }
            _ => {}
        }
    }
    found.hidden.sort_unstable();
    found.hidden.dedup();
    found
}

/// A Content-Type parameter by name, ignoring the name's case.
fn attribute<'a>(content_type: &'a mail_parser::ContentType<'_>, name: &str) -> Option<&'a str> {
    content_type
        .attributes()?
        .iter()
        .find(|attribute| attribute.name.eq_ignore_ascii_case(name))
        .map(|attribute| attribute.value.as_ref())
}

/// What an inline PGP block in a text body means for showing it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InlinePgp {
    /// The text holds an encrypted block: none of it is shown.
    Encrypted,
    /// The text was clear-signed: here it is without the armour, its dash-escaping undone.
    Signed(String),
}

const ENCRYPTED: &str = "-----BEGIN PGP MESSAGE-----";
const SIGNED: &str = "-----BEGIN PGP SIGNED MESSAGE-----";
const SIGNATURE_START: &str = "-----BEGIN PGP SIGNATURE-----";
const SIGNATURE_END: &str = "-----END PGP SIGNATURE-----";

/// An inline PGP block that starts a line of `text`, if there is one.
pub fn inline_pgp(text: &str) -> Option<InlinePgp> {
    let starts = |marker: &str| text.lines().any(|line| line.trim_end() == marker);
    if starts(ENCRYPTED) {
        return Some(InlinePgp::Encrypted);
    }
    if !starts(SIGNED) {
        return None;
    }
    let mut out = Vec::new();
    let mut state = Clear::Before;
    for line in text.lines() {
        let trimmed = line.trim_end();
        state = match state {
            Clear::Before if trimmed == SIGNED => Clear::Headers,
            Clear::Before => {
                out.push(line);
                Clear::Before
            }
            Clear::Headers if trimmed.is_empty() => Clear::Text,
            Clear::Headers => Clear::Headers,
            Clear::Text if trimmed == SIGNATURE_START => Clear::Signature,
            Clear::Text => {
                out.push(line.strip_prefix("- ").unwrap_or(line));
                Clear::Text
            }
            Clear::Signature if trimmed == SIGNATURE_END => Clear::After,
            Clear::Signature => Clear::Signature,
            Clear::After => {
                out.push(line);
                Clear::After
            }
        };
    }
    Some(InlinePgp::Signed(out.join("\n")))
}

#[derive(Clone, Copy)]
enum Clear {
    Before,
    Headers,
    Text,
    Signature,
    After,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message;

    fn raw(text: &str) -> Vec<u8> {
        text.replace('\n', "\r\n").into_bytes()
    }

    const PGP_MIME_ENCRYPTED: &str = "From: Ada <ada@example.com>\n\
        To: Tomas <tomas@example.com>\n\
        Subject: ...\n\
        MIME-Version: 1.0\n\
        Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"e\"\n\
        \n\
        This is an OpenPGP/MIME encrypted message (RFC 4880 and 3156)\n\
        --e\n\
        Content-Type: application/pgp-encrypted\n\
        Content-Description: PGP/MIME version identification\n\
        \n\
        Version: 1\n\
        \n\
        --e\n\
        Content-Type: application/octet-stream; name=\"encrypted.asc\"\n\
        Content-Disposition: inline; filename=\"encrypted.asc\"\n\
        \n\
        -----BEGIN PGP MESSAGE-----\n\
        \n\
        hQEMA0lv1QpY2sdpAQf/c2VjcmV0IG5vdCByZWFsbHk=\n\
        -----END PGP MESSAGE-----\n\
        \n\
        --e--\n";

    #[test]
    fn pgp_mime_encrypted_mail_is_found_and_its_version_part_hidden() {
        let raw = raw(PGP_MIME_ENCRYPTED);
        let found = inspect(&raw);
        assert_eq!(found.encrypted, Some(Encryption::PgpMime));
        assert_eq!(found.signed, None);
        let parsed = message::parse(&raw);
        let version = parsed
            .attachments
            .iter()
            .find(|part| part.mime_type == "application/pgp-encrypted")
            .expect("the version part is an attachment");
        assert_eq!(found.hidden, [version.part]);
        assert!(
            parsed.text_parts.is_empty(),
            "the ciphertext isn't a body part: {:?}",
            parsed.text_parts
        );
    }

    #[test]
    fn smime_enveloped_data_is_encrypted() {
        let raw = raw("From: Ada <ada@example.com>\n\
            Subject: x\n\
            MIME-Version: 1.0\n\
            Content-Type: application/pkcs7-mime; smime-type=enveloped-data; name=smime.p7m\n\
            Content-Transfer-Encoding: base64\n\
            Content-Disposition: attachment; filename=smime.p7m\n\
            \n\
            MIAGCSqGSIb3DQEHA6CAMIACAQAxggE=\n");
        let found = inspect(&raw);
        assert_eq!(found.encrypted, Some(Encryption::Smime));
        assert!(
            found.hidden.is_empty(),
            "the encrypted part stays, to be saved"
        );
        let old = raw_with_type("application/x-pkcs7-mime; name=\"smime.p7m\"");
        assert_eq!(inspect(&old).encrypted, Some(Encryption::Smime));
        let opaque = raw_with_type("application/pkcs7-mime; smime-type=signed-data");
        let found = inspect(&opaque);
        assert_eq!(
            (found.encrypted, found.signed),
            (None, Some(Signature::SmimeOpaque))
        );
        let certs = raw_with_type("application/pkcs7-mime; smime-type=certs-only");
        assert_eq!(inspect(&certs), Inspection::default());
    }

    fn raw_with_type(content_type: &str) -> Vec<u8> {
        raw(&format!(
            "From: a@example.com\nSubject: x\nMIME-Version: 1.0\nContent-Type: {content_type}\n\
             Content-Transfer-Encoding: base64\n\nMIAGCSqGSIb3DQEHA6CAMIACAQAxggE=\n"
        ))
    }

    #[test]
    fn signed_mail_shows_its_content_and_hides_the_signature() {
        for (protocol, signature_type, name, kind) in [
            (
                "application/pgp-signature",
                "application/pgp-signature",
                "signature.asc",
                Signature::PgpMime,
            ),
            (
                "application/pkcs7-signature",
                "application/pkcs7-signature",
                "smime.p7s",
                Signature::Smime,
            ),
            (
                "application/x-pkcs7-signature",
                "application/x-pkcs7-signature",
                "smime.p7s",
                Signature::Smime,
            ),
        ] {
            let raw = raw(&format!(
                "From: Ada <ada@example.com>\nSubject: Signed\nMIME-Version: 1.0\n\
                 Content-Type: multipart/signed; micalg=sha256; protocol=\"{protocol}\"; boundary=\"s\"\n\
                 \n\
                 --s\n\
                 Content-Type: multipart/mixed; boundary=\"m\"\n\
                 \n\
                 --m\n\
                 Content-Type: text/plain; charset=utf-8\n\
                 \n\
                 The minutes are attached.\n\
                 --m\n\
                 Content-Type: application/pdf; name=\"minutes.pdf\"\n\
                 Content-Disposition: attachment; filename=\"minutes.pdf\"\n\
                 Content-Transfer-Encoding: base64\n\
                 \n\
                 JVBERi0xLjQK\n\
                 --m--\n\
                 \n\
                 --s\n\
                 Content-Type: {signature_type}; name=\"{name}\"\n\
                 Content-Disposition: attachment; filename=\"{name}\"\n\
                 \n\
                 c2lnbmF0dXJl\n\
                 --s--\n"
            ));
            let found = inspect(&raw);
            assert_eq!(found.signed, Some(kind), "{protocol}");
            assert_eq!(found.encrypted, None);
            let parsed = message::parse(&raw);
            assert_eq!(parsed.text_parts.len(), 1, "the content is the body");
            let signature = parsed
                .attachments
                .iter()
                .find(|part| part.name.as_deref() == Some(name))
                .expect("the signature is an attachment");
            assert_eq!(found.hidden, [signature.part], "{protocol}");
            assert!(
                parsed
                    .attachments
                    .iter()
                    .any(|part| part.name.as_deref() == Some("minutes.pdf")),
                "real attachments stay"
            );
        }
    }

    #[test]
    fn stray_signature_files_are_hidden_but_other_asc_and_sig_files_stay() {
        let raw = raw("From: a@example.com\nSubject: x\nMIME-Version: 1.0\n\
            Content-Type: multipart/mixed; boundary=\"m\"\n\
            \n\
            --m\n\
            Content-Type: text/plain\n\
            \n\
            Release attached.\n\
            --m\n\
            Content-Type: application/octet-stream; name=\"release.tar.gz.sig\"\n\
            Content-Disposition: attachment; filename=\"release.tar.gz.sig\"\n\
            \n\
            sig\n\
            --m\n\
            Content-Type: application/pgp-keys; name=\"key.asc\"\n\
            Content-Disposition: attachment; filename=\"key.asc\"\n\
            \n\
            key\n\
            --m\n\
            Content-Type: application/pgp-signature; name=\"signature.asc\"\n\
            Content-Disposition: attachment; filename=\"signature.asc\"\n\
            \n\
            sig\n\
            --m\n\
            Content-Type: application/octet-stream; name=\"smime.p7s\"\n\
            Content-Disposition: attachment; filename=\"smime.p7s\"\n\
            \n\
            sig\n\
            --m--\n");
        let found = inspect(&raw);
        let parsed = message::parse(&raw);
        let hidden: Vec<&str> = parsed
            .attachments
            .iter()
            .filter(|part| found.hidden.contains(&part.part))
            .filter_map(|part| part.name.as_deref())
            .collect();
        assert_eq!(hidden, ["signature.asc", "smime.p7s"]);
        assert_eq!(found.signed, None, "a stray signature signs nothing shown");
    }

    #[test]
    fn flowed_parts_are_found_with_delsp() {
        let raw = raw("From: a@example.com\nSubject: x\nMIME-Version: 1.0\n\
            Content-Type: multipart/alternative; boundary=\"a\"\n\
            \n\
            --a\n\
            Content-Type: text/plain; charset=utf-8; Format=Flowed; DelSp=Yes\n\
            \n\
            Hello \n\
            there.\n\
            --a\n\
            Content-Type: text/html\n\
            \n\
            <p>Hello there.</p>\n\
            --a--\n");
        let found = inspect(&raw);
        let parsed = message::parse(&raw);
        assert_eq!(
            found.flowed,
            [(parsed.text_parts[0], Flowed { delsp: true })]
        );
        let plain = self::raw(
            "From: a@example.com\nSubject: x\nContent-Type: text/plain; format=flowed\n\nHi \nthere\n",
        );
        assert_eq!(inspect(&plain).flowed, [(0, Flowed { delsp: false })]);
        let fixed = self::raw("From: a@example.com\nSubject: x\nContent-Type: text/plain\n\nHi\n");
        assert!(inspect(&fixed).flowed.is_empty());
    }

    #[test]
    fn garbage_finds_nothing() {
        assert_eq!(inspect(b""), Inspection::default());
        assert_eq!(inspect(b"\x00\xff not mail"), Inspection::default());
    }

    #[test]
    fn inline_encrypted_text_is_never_shown() {
        let text =
            "Hi,\n\n-----BEGIN PGP MESSAGE-----\n\nhQEMA0lv1QpY\n-----END PGP MESSAGE-----\n";
        assert_eq!(inline_pgp(text), Some(InlinePgp::Encrypted));
        assert_eq!(
            inline_pgp("Talking about -----BEGIN PGP MESSAGE----- inline"),
            None,
            "only a line of its own is armour"
        );
        assert_eq!(inline_pgp("Plain text.\n"), None);
    }

    #[test]
    fn clear_signed_text_loses_its_armour() {
        let text = "-----BEGIN PGP SIGNED MESSAGE-----\r\n\
                    Hash: SHA256\r\n\
                    \r\n\
                    The meeting moved to Thursday.\r\n\
                    - -- \r\n\
                    - From the board\r\n\
                    -----BEGIN PGP SIGNATURE-----\r\n\
                    \r\n\
                    iQEzBAEBCAAdFiEE\r\n\
                    -----END PGP SIGNATURE-----\r\n";
        assert_eq!(
            inline_pgp(text),
            Some(InlinePgp::Signed(
                "The meeting moved to Thursday.\n-- \nFrom the board".into()
            ))
        );
        let around = "Forwarded:\n-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA1\n\nBody\n\
                      -----BEGIN PGP SIGNATURE-----\nx\n-----END PGP SIGNATURE-----\nAfter\n";
        assert_eq!(
            inline_pgp(around),
            Some(InlinePgp::Signed("Forwarded:\nBody\nAfter".into()))
        );
    }
}
