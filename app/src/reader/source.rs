//! What the reader reads and writes through: the app's mailbox, whose store opens at startup
//! and closes at shutdown, or a store of its own (examples and self-tests). M5's owners add
//! their reads here: the conversation (ML-057) and the senders whose images load (ML-058).

use sealwax_domain::model::{Account, MessageDetail, MessageId};
use sealwax_infrastructure::store::{Conversation, Reply, Store, Versioned};

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

    /// The conversation of `id` as the reader shows it (ML-057); `None` while no store is open.
    fn conversation(&self, _id: MessageId) -> Option<Reply<Versioned<Option<Conversation>>>> {
        None
    }
}

/// The replies to [`MessageSource::read`].
pub struct Reads {
    pub detail: Reply<Versioned<Option<MessageDetail>>>,
    pub accounts: Reply<Versioned<Vec<Account>>>,
    pub raw: Reply<Option<Vec<u8>>>,
}

impl MessageSource for Store {
    fn read(&self, id: MessageId) -> Option<Reads> {
        Some(Reads {
            detail: self.message(id),
            accounts: self.accounts(),
            raw: self.raw_message(id),
        })
    }

    fn opened(&self, id: MessageId, at: i64) {
        let _ = self.touch_body(id, at);
    }

    fn conversation(&self, id: MessageId) -> Option<Reply<Versioned<Option<Conversation>>>> {
        Some(Store::conversation(self, id))
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

    fn conversation(&self, id: MessageId) -> Option<Reply<Versioned<Option<Conversation>>>> {
        self.store()
            .and_then(|store| MessageSource::conversation(&*store, id))
    }
}
