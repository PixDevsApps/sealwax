//! What the reader reads and writes through: the app's mailbox, whose store opens at startup
//! and closes at shutdown, or a store of its own (examples and self-tests). M5's owners add
//! their reads here: the conversation (ML-057), the senders whose images load (ML-058) and the
//! stored bytes behind attachments, Show Original and Save as .eml (ML-061, ML-063).

use gtk4::glib;
use sealwax_domain::model::{Account, MessageDetail, MessageId};
use sealwax_infrastructure::store::{BodyTarget, Reply, Store, Versioned};

/// Where the reader sends its requests.
pub trait MessageSource {
    /// The reads one message needs, sent together; `None` while no store is open.
    fn read(&self, id: MessageId) -> Option<Reads>;

    /// The reader showed the body of `id` at `at` (Unix seconds): the disk cap keeps the bodies
    /// opened last (ML-044).
    fn opened(&self, _id: MessageId, _at: i64) {}

    /// The senders whose remote images load without asking (ML-058), lower case; `None` while
    /// no store is open.
    fn remote_senders(&self) -> Option<Reply<Vec<String>>> {
        None
    }

    /// Images from `address` load from now on, in every message (ML-058).
    fn allow_remote_sender(&self, _address: &str) {}

    /// Images from `address` are hidden again until allowed (ML-058).
    fn forget_remote_sender(&self, _address: &str) {}

    /// Where `id` is on the server and how much of it the store holds (ML-061, ML-063): a
    /// message stored as its text parts fetches its attachments and its original from the
    /// server. `None` while no store is open.
    fn body_target(&self, _id: MessageId) -> Option<Reply<Vec<BodyTarget>>> {
        None
    }

    /// The raw message as stored; `None` while no store is open.
    fn raw_message(&self, _id: MessageId) -> Option<Reply<Option<Vec<u8>>>> {
        None
    }
}

/// The replies to [`MessageSource::read`].
pub struct Reads {
    pub detail: Reply<Versioned<Option<MessageDetail>>>,
    pub accounts: Reply<Versioned<Vec<Account>>>,
    pub raw: Reply<Option<Vec<u8>>>,
}

/// Waits for a write off the caller's path; a failure is logged without what was written.
fn logged(reply: Reply<()>, what: &'static str) {
    glib::spawn_future_local(async move {
        if let Err(error) = reply.recv().await {
            tracing::warn!(%error, what, "reader: the store didn't take a change");
        }
    });
}

fn now() -> i64 {
    glib::real_time() / 1_000_000
}

impl MessageSource for Store {
    fn read(&self, id: MessageId) -> Option<Reads> {
        Some(Reads {
            detail: self.message(id),
            accounts: self.accounts(),
            raw: Store::raw_message(self, id),
        })
    }

    fn opened(&self, id: MessageId, at: i64) {
        let _ = self.touch_body(id, at);
    }

    fn remote_senders(&self) -> Option<Reply<Vec<String>>> {
        Some(Store::remote_senders(self))
    }

    fn allow_remote_sender(&self, address: &str) {
        logged(
            Store::allow_remote_sender(self, address.to_owned(), now()),
            "a sender whose images load",
        );
    }

    fn forget_remote_sender(&self, address: &str) {
        logged(
            Store::forget_remote_sender(self, address.to_owned()),
            "a sender forgotten",
        );
    }

    fn body_target(&self, id: MessageId) -> Option<Reply<Vec<BodyTarget>>> {
        Some(self.body_targets(vec![id]))
    }

    fn raw_message(&self, id: MessageId) -> Option<Reply<Option<Vec<u8>>>> {
        Some(Store::raw_message(self, id))
    }
}

impl MessageSource for crate::mailbox::Mailbox {
    fn read(&self, id: MessageId) -> Option<Reads> {
        self.store()
            .and_then(|store| MessageSource::read(&*store, id))
    }

    fn opened(&self, id: MessageId, at: i64) {
        if let Some(store) = self.store() {
            MessageSource::opened(&*store, id, at);
        }
    }

    fn remote_senders(&self) -> Option<Reply<Vec<String>>> {
        self.store()
            .and_then(|store| MessageSource::remote_senders(&*store))
    }

    fn allow_remote_sender(&self, address: &str) {
        if let Some(store) = self.store() {
            MessageSource::allow_remote_sender(&*store, address);
        }
    }

    fn forget_remote_sender(&self, address: &str) {
        if let Some(store) = self.store() {
            MessageSource::forget_remote_sender(&*store, address);
        }
    }

    fn body_target(&self, id: MessageId) -> Option<Reply<Vec<BodyTarget>>> {
        self.store()
            .and_then(|store| MessageSource::body_target(&*store, id))
    }

    fn raw_message(&self, id: MessageId) -> Option<Reply<Option<Vec<u8>>>> {
        self.store()
            .and_then(|store| MessageSource::raw_message(&*store, id))
    }
}
