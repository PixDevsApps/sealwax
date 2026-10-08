//! The addresses an account sends from and their signatures (ML-070, PRD A6). Signatures are
//! plain text in 1.0 (accepted at the M6 gate): the composer shows them below the body after a `-- ` line,
//! inside a `div` of class [`SIGNATURE_CLASS`] so it can swap them when the sender changes.

use super::address;
use super::clean::SIGNATURE_CLASS;
use super::text;
use crate::account::{AccountSettings, Identity};
use crate::model::{Address, Headers};
use std::fmt;

/// The addresses `account` sends from: its own first, with the account's name and signature,
/// then its identities in the file's order, each address once.
pub fn senders(account: &AccountSettings) -> Vec<Identity> {
    let own = Identity {
        name: account.name.clone(),
        address: account.address.clone(),
        signature: account.signature.clone(),
    };
    let mut out = vec![own];
    for identity in &account.identities {
        if !out
            .iter()
            .any(|known| address::same(&known.address, &identity.address))
        {
            out.push(identity.clone());
        }
    }
    out
}

/// Whether `email` is one of the user's own addresses.
pub fn is_own(senders: &[Identity], email: &str) -> bool {
    senders
        .iter()
        .any(|sender| address::same(&sender.address, email))
}

/// The identity a reply to a message with `headers` goes out from: the one that sent it when
/// the user did, else the first of the user's addresses in To, then in Cc, else the first
/// sender (the account's own).
pub fn reply_sender<'s>(senders: &'s [Identity], headers: &Headers) -> Option<&'s Identity> {
    let find = |list: &[Address]| {
        list.iter().find_map(|recipient| {
            senders
                .iter()
                .find(|sender| address::same(&sender.address, &recipient.email))
        })
    };
    find(&headers.from)
        .or_else(|| find(&headers.to))
        .or_else(|| find(&headers.cc))
        .or_else(|| senders.first())
}

/// The From address of mail sent as `identity`.
pub fn from_address(identity: &Identity) -> Address {
    Address::new(identity.name.clone(), identity.address.clone())
}

/// A signature as the composer inserts it below the body. Its `Debug` prints lengths only.
#[derive(Clone, PartialEq, Eq)]
pub struct Signature {
    /// `<div class="sw-signature"><div>-- </div>…</div>`, one `div` per line.
    pub html: String,
    /// `-- ` on a line of its own, then the text.
    pub text: String,
}

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Signature")
            .field("html_len", &self.html.len())
            .field("text_len", &self.text.len())
            .finish()
    }
}

/// The separator line in a signature's HTML: `--` and a no-break space, which survives HTML's
/// whitespace rules; the plain-text part turns the line back into `-- ` (RFC 3676 4.3).
pub const SEPARATOR_HTML: &str = "<div>--&nbsp;</div>";

/// `text` as a signature; `None` when it is empty. Trailing white space and blank lines at the
/// ends are dropped.
pub fn signature(text: &str) -> Option<Signature> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let first = lines.iter().position(|line| !line.is_empty())?;
    let last = lines.iter().rposition(|line| !line.is_empty())?;
    let body = lines[first..=last].join("\n");
    Some(Signature {
        html: format!(
            "<div class=\"{SIGNATURE_CLASS}\">{SEPARATOR_HTML}{}</div>",
            text::to_html(&body)
        ),
        text: format!("-- \n{body}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{AccountKey, AuthMethod, Endpoint, Security};

    fn account() -> AccountSettings {
        let endpoint = Endpoint {
            host: "mail.example.test".into(),
            port: 993,
            security: Security::Tls,
        };
        AccountSettings {
            key: AccountKey("work".into()),
            name: "Ada Lovelace".into(),
            label: None,
            color: None,
            address: "ada@example.test".into(),
            incoming: endpoint.clone(),
            receive: crate::account::Receive::Imap,
            smtp: endpoint,
            username: "ada@example.test".into(),
            auth: AuthMethod::Password,
            signature: Some("Ada".into()),
            identities: vec![
                Identity {
                    name: "Ada at the board".into(),
                    address: "board@example.test".into(),
                    signature: None,
                },
                Identity {
                    name: "Duplicate".into(),
                    address: "ADA@example.test".into(),
                    signature: None,
                },
            ],
        }
    }

    #[test]
    fn senders_start_with_the_account() {
        let senders = senders(&account());
        let addresses: Vec<&str> = senders.iter().map(|s| s.address.as_str()).collect();
        assert_eq!(addresses, ["ada@example.test", "board@example.test"]);
        assert_eq!(senders[0].signature.as_deref(), Some("Ada"));
        assert!(is_own(&senders, "Board@Example.TEST"));
        assert!(!is_own(&senders, "bob@example.test"));
    }

    #[test]
    fn a_reply_goes_out_from_the_address_it_was_sent_to() {
        let senders = senders(&account());
        let mut headers = Headers {
            from: vec![Address::new("Bob", "bob@example.test")],
            to: vec![Address::new("", "list@example.test")],
            cc: vec![Address::new("", "BOARD@example.test")],
            ..Headers::default()
        };
        assert_eq!(
            reply_sender(&senders, &headers).unwrap().address,
            "board@example.test"
        );
        headers.cc.clear();
        assert_eq!(
            reply_sender(&senders, &headers).unwrap().address,
            "ada@example.test"
        );
        headers.from = vec![Address::new("Me", "board@example.test")];
        headers.to = vec![Address::new("", "ada@example.test")];
        assert_eq!(
            reply_sender(&senders, &headers).unwrap().address,
            "board@example.test"
        );
        assert_eq!(reply_sender(&[], &headers), None);
    }

    #[test]
    fn signatures_are_plain_text_after_the_separator() {
        let signature = signature("\n\nAda Lovelace  \r\n<Analytical> & Co.\n\n").unwrap();
        assert_eq!(signature.text, "-- \nAda Lovelace\n<Analytical> & Co.");
        assert_eq!(
            signature.html,
            "<div class=\"sw-signature\"><div>--&nbsp;</div><div>Ada Lovelace</div>\
             <div>&lt;Analytical&gt; &amp; Co.</div></div>"
        );
        assert_eq!(super::signature(" \n\t\n"), None);
        assert!(!format!("{signature:?}").contains("Ada"));
    }
}
