//! The mail data model shared by the store, the engine and the app: plain values with newtype
//! ids, no behaviour beyond small conversions.

use crate::account::AccountKey;
use std::fmt;

macro_rules! id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub i64);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

id!(
    /// An account in the store.
    AccountId
);
id!(
    /// A folder (IMAP mailbox) of one account.
    FolderId
);
id!(
    /// One stored copy of a message in one folder; not the RFC 5322 Message-ID.
    MessageId
);
id!(
    /// A conversation: messages joined by Message-ID, In-Reply-To and References.
    ThreadId
);
id!(
    /// A message being written or waiting in the outbox (M6): a draft keeps its id from the
    /// composer through the undo window and the send.
    DraftId
);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub id: AccountId,
    /// What Sealwax calls the account: `accounts.toml`'s label, else the address. The sender
    /// name is the settings' own.
    pub name: String,
    pub address: String,
    /// Its key in `accounts.toml` and the keyring; `None` for a local account with no server
    /// (fixtures, self-tests), which no settings file lists.
    pub key: Option<AccountKey>,
}

/// What a folder is for, from SPECIAL-USE or the name table (ADR-006).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FolderRole {
    Inbox,
    Drafts,
    Sent,
    Archive,
    Junk,
    Trash,
    All,
    Other,
}

impl FolderRole {
    pub const ALL: [FolderRole; 8] = [
        FolderRole::Inbox,
        FolderRole::Drafts,
        FolderRole::Sent,
        FolderRole::Archive,
        FolderRole::Junk,
        FolderRole::Trash,
        FolderRole::All,
        FolderRole::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            FolderRole::Inbox => "inbox",
            FolderRole::Drafts => "drafts",
            FolderRole::Sent => "sent",
            FolderRole::Archive => "archive",
            FolderRole::Junk => "junk",
            FolderRole::Trash => "trash",
            FolderRole::All => "all",
            FolderRole::Other => "other",
        }
    }

    /// The role stored as `text`; anything unknown is `Other`.
    pub fn parse(text: &str) -> FolderRole {
        Self::ALL
            .into_iter()
            .find(|role| role.as_str() == text)
            .unwrap_or(FolderRole::Other)
    }
}

/// A folder with the counts the sidebar shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    pub id: FolderId,
    pub account: AccountId,
    /// The display name, decoded from modified UTF-7.
    pub name: String,
    pub role: FolderRole,
    /// `\Noselect` folders hold no messages and take none: the hierarchy's containers.
    pub selectable: bool,
    pub total: u32,
    pub unread: u32,
}

/// IMAP system flags plus the keywords Sealwax filters on, as a bit set. Other keywords are kept
/// as text beside it, so the cache can equal the server (S1).
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Flags(u32);

impl Flags {
    pub const SEEN: Flags = Flags(1);
    pub const ANSWERED: Flags = Flags(2);
    pub const FLAGGED: Flags = Flags(4);
    pub const DELETED: Flags = Flags(8);
    pub const DRAFT: Flags = Flags(16);
    pub const FORWARDED: Flags = Flags(32);
    pub const JUNK: Flags = Flags(64);
    pub const NOT_JUNK: Flags = Flags(128);
    pub const NONE: Flags = Flags(0);
    const KNOWN: u32 = 255;

    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Unknown bits are dropped.
    pub const fn from_bits(bits: u32) -> Flags {
        Flags(bits & Self::KNOWN)
    }

    pub const fn contains(self, other: Flags) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Flags) -> Flags {
        Flags(self.0 | other.0)
    }

    pub const fn difference(self, other: Flags) -> Flags {
        Flags(self.0 & !other.0)
    }

    pub fn is_seen(self) -> bool {
        self.contains(Flags::SEEN)
    }

    pub fn is_flagged(self) -> bool {
        self.contains(Flags::FLAGGED)
    }

    pub fn is_answered(self) -> bool {
        self.contains(Flags::ANSWERED)
    }

    pub fn is_draft(self) -> bool {
        self.contains(Flags::DRAFT)
    }

    /// Splits IMAP flags into the bit set and the sorted, deduplicated other keywords.
    /// `\Recent` is session state and is dropped.
    pub fn from_imap<'a>(flags: impl IntoIterator<Item = &'a str>) -> (Flags, Vec<String>) {
        let mut bits = Flags::NONE;
        let mut keywords = Vec::new();
        for flag in flags {
            let known = match flag.to_ascii_lowercase().as_str() {
                "\\seen" => Flags::SEEN,
                "\\answered" => Flags::ANSWERED,
                "\\flagged" => Flags::FLAGGED,
                "\\deleted" => Flags::DELETED,
                "\\draft" => Flags::DRAFT,
                "$forwarded" => Flags::FORWARDED,
                "$junk" => Flags::JUNK,
                "$notjunk" => Flags::NOT_JUNK,
                "\\recent" => continue,
                _ => {
                    keywords.push(flag.to_owned());
                    continue;
                }
            };
            bits = bits.union(known);
        }
        keywords.sort();
        keywords.dedup();
        (bits, keywords)
    }
}

impl std::ops::BitOr for Flags {
    type Output = Flags;

    fn bitor(self, other: Flags) -> Flags {
        self.union(other)
    }
}

impl fmt::Debug for Flags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const NAMES: [&str; 8] = [
            "SEEN",
            "ANSWERED",
            "FLAGGED",
            "DELETED",
            "DRAFT",
            "FORWARDED",
            "JUNK",
            "NOT_JUNK",
        ];
        let set: Vec<&str> = NAMES
            .iter()
            .enumerate()
            .filter(|(bit, _)| self.0 & (1 << bit) != 0)
            .map(|(_, name)| *name)
            .collect();
        write!(f, "Flags({})", set.join(" | "))
    }
}

/// A mailbox address: the display name (decoded, possibly empty) and the address itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Address {
    pub name: String,
    pub email: String,
}

impl Address {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            email: email.into(),
        }
    }

    /// The name, or the address when there is no name.
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.email
        } else {
            &self.name
        }
    }
}

/// What a message list row shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageSummary {
    pub id: MessageId,
    pub folder: FolderId,
    pub thread: ThreadId,
    pub sender_name: String,
    pub sender_email: String,
    pub subject: String,
    pub snippet: String,
    /// When the message arrived (IMAP INTERNALDATE, else its Date header), Unix seconds.
    pub date: i64,
    pub flags: Flags,
    pub has_attachments: bool,
    /// Messages in the conversation, each Message-ID counted once.
    pub thread_count: u32,
}

/// Which body alternatives a message has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BodyKind {
    #[default]
    None,
    Plain,
    Html,
    Both,
}

impl BodyKind {
    pub fn from_parts(plain: bool, html: bool) -> BodyKind {
        match (plain, html) {
            (false, false) => BodyKind::None,
            (true, false) => BodyKind::Plain,
            (false, true) => BodyKind::Html,
            (true, true) => BodyKind::Both,
        }
    }

    pub fn code(self) -> u8 {
        match self {
            BodyKind::None => 0,
            BodyKind::Plain => 1,
            BodyKind::Html => 2,
            BodyKind::Both => 3,
        }
    }

    pub fn from_code(code: u8) -> BodyKind {
        match code {
            1 => BodyKind::Plain,
            2 => BodyKind::Html,
            3 => BodyKind::Both,
            _ => BodyKind::None,
        }
    }
}

/// The header fields Sealwax keeps, decoded (RFC 2047 names and subjects included).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Headers {
    pub from: Vec<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub subject: String,
    /// The Date header, Unix seconds.
    pub date: Option<i64>,
    /// Message-ID without angle brackets.
    pub message_id: Option<String>,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
    /// The List-Id identifier, without angle brackets.
    pub list_id: Option<String>,
    /// The conversation Outlook's `Thread-Index` names.
    pub thread_index: Option<ThreadIndex>,
}

/// A non-body MIME part. `part` indexes mail-parser's part list of the raw message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub part: u32,
    pub name: Option<String>,
    /// Decoded size in bytes.
    pub size: u64,
    /// Lower case, `type/subtype`.
    pub mime_type: String,
    /// Content-ID without angle brackets.
    pub content_id: Option<String>,
    /// Content-Disposition `inline` (or none, with a Content-ID).
    pub inline: bool,
}

impl Attachment {
    /// An inline image that the HTML body shows through `cid:`, rather than a file the user
    /// would save; it doesn't earn the list's attachment clip.
    pub fn is_embedded(&self) -> bool {
        self.inline && self.content_id.is_some() && self.mime_type.starts_with("image/")
    }
}

/// The SHA-256 of a raw message, which names it in the blob store.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlobKey(pub [u8; 32]);

impl BlobKey {
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }

    pub fn from_hex(text: &str) -> Option<BlobKey> {
        let bytes = text.as_bytes();
        if bytes.len() != 64 {
            return None;
        }
        let nibble = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        let mut key = [0u8; 32];
        for (slot, [high, low]) in key.iter_mut().zip(bytes.as_chunks::<2>().0) {
            *slot = nibble(*high)? << 4 | nibble(*low)?;
        }
        Some(BlobKey(key))
    }

    pub fn from_slice(bytes: &[u8]) -> Option<BlobKey> {
        <[u8; 32]>::try_from(bytes).ok().map(BlobKey)
    }
}

impl fmt::Debug for BlobKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlobKey({})", self.to_hex())
    }
}

/// The conversation part of Outlook's `Thread-Index` header: the 22-byte header block (a
/// reserved byte, a time and a GUID) that every reply in the conversation repeats before its
/// own 5-byte blocks ([MS-OXOMSG] 2.2.1.3).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ThreadIndex(pub [u8; 22]);

impl ThreadIndex {
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }

    pub fn from_slice(bytes: &[u8]) -> Option<ThreadIndex> {
        <[u8; 22]>::try_from(bytes).ok().map(ThreadIndex)
    }
}

impl fmt::Debug for ThreadIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ThreadIndex({})", self.to_hex())
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    text
}

/// Everything the reader shows about one message except its body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageDetail {
    pub summary: MessageSummary,
    pub headers: Headers,
    pub body_kind: BodyKind,
    /// mail-parser part indexes of the text/plain bodies, in order.
    pub text_parts: Vec<u32>,
    /// mail-parser part indexes of the text/html bodies, in order.
    pub html_parts: Vec<u32>,
    pub attachments: Vec<Attachment>,
    pub keywords: Vec<String>,
    /// RFC822.SIZE, or the raw length.
    pub size: u64,
    /// Where the raw message is, once it has been fetched.
    pub blob: Option<BlobKey>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_round_trip() {
        for role in FolderRole::ALL {
            assert_eq!(FolderRole::parse(role.as_str()), role);
        }
        assert_eq!(FolderRole::parse("flagged"), FolderRole::Other);
    }

    #[test]
    fn flags_from_imap_keep_keywords() {
        let (flags, keywords) = Flags::from_imap([
            "\\Seen",
            "\\Flagged",
            "\\Recent",
            "$Label1",
            "Prioritert",
            "$Label1",
        ]);
        assert_eq!(flags, Flags::SEEN | Flags::FLAGGED);
        assert!(flags.is_seen() && flags.is_flagged() && !flags.is_answered());
        assert_eq!(keywords, ["$Label1", "Prioritert"]);
        assert_eq!(Flags::from_bits(0xffff_ffff).bits(), 255);
        assert_eq!(
            (Flags::SEEN | Flags::DRAFT).difference(Flags::SEEN),
            Flags::DRAFT
        );
        assert_eq!(
            format!("{:?}", Flags::SEEN | Flags::FLAGGED),
            "Flags(SEEN | FLAGGED)"
        );
    }

    #[test]
    fn blob_keys_round_trip_through_hex() {
        let mut bytes = [0u8; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = (index * 37) as u8;
        }
        let key = BlobKey(bytes);
        let hex = key.to_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(BlobKey::from_hex(&hex), Some(key));
        assert_eq!(BlobKey::from_hex("zz"), None);
        assert_eq!(BlobKey::from_hex(&hex.to_uppercase()), None);
        assert_eq!(BlobKey::from_slice(&bytes[..31]), None);
    }

    #[test]
    fn body_kind_codes_round_trip() {
        for kind in [
            BodyKind::None,
            BodyKind::Plain,
            BodyKind::Html,
            BodyKind::Both,
        ] {
            assert_eq!(BodyKind::from_code(kind.code()), kind);
        }
        assert_eq!(BodyKind::from_parts(true, true), BodyKind::Both);
    }

    #[test]
    fn display_name_falls_back_to_the_address() {
        assert_eq!(
            Address::new("", "a@example.test").display_name(),
            "a@example.test"
        );
        assert_eq!(Address::new("Ada", "a@example.test").display_name(), "Ada");
    }
}
