//! Everything a composer window needs from outside it (ML-067's seam): the draft a request
//! starts from, saving it (at most 5 s behind the typing, PRD C5), closing, discarding and
//! sending it, the recipients the user has written to, the submission server's size limit and
//! the blob store. The store's backend ([`super::store_backend`]) runs it; self-test builds also
//! have an in-memory stand-in (`super::stand_in`, `test-hooks` only).
//!
//! Every future resolves on the GTK thread and must not block it: the store's and the
//! engine's work happens on their own threads.

use super::ComposeRequest;
use sealwax_domain::compose::Draft;
use sealwax_domain::model::{AccountId, Address, DraftId};
use sealwax_infrastructure::blobs::BlobStore;
use std::future::Future;
use std::pin::Pin;

/// A future that resolves on the GTK thread.
pub(crate) type Local<T> = Pin<Box<dyn Future<Output = T>>>;

/// An address a draft can be sent from: an account's own or one of its identities (PRD A6),
/// of any account that sends (issue #7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sender {
    /// The account it is sent through: picking it moves the draft there.
    pub account: AccountId,
    /// What the From chip calls it: the sender name the address goes out with.
    pub label: String,
    /// The sender name and address the message goes out with.
    pub address: Address,
    /// Its signature, plain text (`accounts.toml`); the composer swaps it in when the sender
    /// changes.
    pub signature: Option<String>,
}

/// What a composer window starts from.
#[derive(Clone, Debug)]
pub(crate) struct Prepared {
    pub id: DraftId,
    pub draft: Draft,
    /// Who it can be sent as: every account that sends, each its own address then its
    /// identities.
    pub senders: Vec<Sender>,
    /// The header's title: "New message", "Reply", "Forward" or "Draft".
    pub title: String,
}

/// An address the user has written to or heard from, for the suggestions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Suggestion {
    pub address: Address,
    /// Messages exchanged with it, for the suggestion's second line.
    pub messages: u32,
}

/// Why a draft wasn't taken for sending; the composer shows it and keeps the window open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Refusal(pub String);

pub(crate) trait Backend {
    /// The draft for `request`, with who it can be sent as.
    fn prepare(&self, request: ComposeRequest) -> Local<Result<Prepared, String>>;
    /// Keeps the draft as it is now.
    fn save(&self, id: DraftId, draft: Draft) -> Local<Result<(), String>>;
    /// The window closed; the draft is kept (it was saved just before).
    fn close(&self, id: DraftId);
    /// The user discarded the draft.
    fn discard(&self, id: DraftId) -> Local<Result<(), String>>;
    /// Queues the draft for sending (phase B: the outbox, with the undo window).
    fn send(&self, id: DraftId, draft: Draft) -> Local<Result<(), Refusal>>;
    /// Addresses matching `query`, best first.
    fn recipients(&self, account: AccountId, query: String) -> Local<Vec<Suggestion>>;
    /// The largest message the account's submission server takes, `None` when unknown.
    fn submission_limit(&self, account: AccountId) -> Local<Option<u64>>;
    /// Where attachments and inline images go; `None` until the store is open.
    fn blobs(&self) -> Option<BlobStore>;
}

/// The right-hand side of the composer's Message-IDs and Content-IDs: the sender's domain in
/// ASCII, lower case.
pub(crate) fn id_domain(sender: &str) -> String {
    let domain = sender
        .rsplit_once('@')
        .map(|(_, domain)| domain.trim().trim_end_matches('.'))
        .unwrap_or_default();
    let ascii = gtk4::glib::hostname_to_ascii(domain)
        .map(|ascii| ascii.to_ascii_lowercase())
        .unwrap_or_default();
    let valid = !ascii.is_empty()
        && ascii.split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        });
    if valid {
        ascii
    } else {
        "sealwax.invalid".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_use_the_sender_domain() {
        assert_eq!(id_domain("kari@Example.COM"), "example.com");
        assert!(id_domain("ola@blåbær.example").starts_with("xn--"));
        assert_eq!(id_domain("nobody"), "sealwax.invalid");
    }
}
