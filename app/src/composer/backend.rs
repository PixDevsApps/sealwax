//! Everything a composer window needs from outside it (ML-067's seam): the draft a request
//! starts from, saving it (at most 5 s behind the typing, PRD C5), closing, discarding and
//! sending it, the recipients the user has written to, the submission server's size limit and
//! the blob store. Phase A runs on [`StandIn`], in memory; phase B swaps in the store, the
//! engine and the compose rules behind the same trait ([`super::set_backend`]).
//!
//! Every future resolves on the GTK thread and must not block it: the store's and the
//! engine's work happens on their own threads.

use super::ComposeRequest;
use super::recipients;
use crate::application::AppState;
use sealwax_domain::compose::{Draft, DraftSource, ReplyKind};
use sealwax_domain::model::{AccountId, Address, DraftId};
use sealwax_infrastructure::blobs::BlobStore;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::time::Instant;

/// A future that resolves on the GTK thread.
pub type Local<T> = Pin<Box<dyn Future<Output = T>>>;

/// An address a draft can be sent from: an account's own or one of its identities (PRD A6),
/// of any account that sends (issue #7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sender {
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
pub struct Prepared {
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
pub struct Suggestion {
    pub address: Address,
    /// Messages exchanged with it, for the suggestion's second line.
    pub messages: u32,
}

/// Why a draft wasn't taken for sending; the composer shows it and keeps the window open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal(pub String);

pub trait Backend {
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

/// The stand-in limit while the engine answers none (phase A): 25 MiB.
pub const STAND_IN_LIMIT: u64 = 25 * 1024 * 1024;

/// Phase A's backend: drafts in memory, every save, close, discard and send recorded for the
/// self-tests, a few fictional contacts, and the stand-in size limit. New and `mailto:` drafts
/// are filled from the request; replies, forwards and stored drafts are simple stand-ins until
/// the compose rules arrive.
pub struct StandIn {
    state: Weak<AppState>,
    next: Cell<i64>,
    drafts: RefCell<HashMap<DraftId, Draft>>,
    saves: RefCell<Vec<(DraftId, Instant)>>,
    closed: RefCell<Vec<DraftId>>,
    discarded: RefCell<Vec<DraftId>>,
    sent: RefCell<Vec<(DraftId, Draft)>>,
    limit: Cell<Option<u64>>,
    identities: RefCell<Vec<Sender>>,
    contacts: RefCell<Vec<Suggestion>>,
}

impl StandIn {
    pub fn new(state: &Rc<AppState>) -> Rc<Self> {
        let contacts = [
            ("Ada Lovelace", "ada@example.test", 14),
            ("Adrian Berg", "adrian.berg@example.com", 3),
            ("Bjørn Ærø", "bjorn@example.net", 9),
            ("Grace Hopper", "grace@example.test", 21),
            ("Kari Nordmann", "kari@example.com", 6),
            ("Ola Nordmann", "ola@example.org", 2),
            ("Øystein Åsen", "oystein@example.org", 5),
        ]
        .into_iter()
        .map(|(name, email, messages)| Suggestion {
            address: Address::new(name, email),
            messages,
        })
        .collect();
        Rc::new(Self {
            state: Rc::downgrade(state),
            next: Cell::new(1),
            drafts: RefCell::new(HashMap::new()),
            saves: RefCell::new(Vec::new()),
            closed: RefCell::new(Vec::new()),
            discarded: RefCell::new(Vec::new()),
            sent: RefCell::new(Vec::new()),
            limit: Cell::new(Some(STAND_IN_LIMIT)),
            identities: RefCell::new(Vec::new()),
            contacts: RefCell::new(contacts),
        })
    }

    /// Every save so far: the draft and when.
    pub fn saves(&self) -> Vec<(DraftId, Instant)> {
        self.saves.borrow().clone()
    }

    pub fn closed(&self) -> Vec<DraftId> {
        self.closed.borrow().clone()
    }

    pub fn discarded(&self) -> Vec<DraftId> {
        self.discarded.borrow().clone()
    }

    pub fn sent(&self) -> Vec<(DraftId, Draft)> {
        self.sent.borrow().clone()
    }

    /// The last saved state of `id`.
    pub fn draft(&self, id: DraftId) -> Option<Draft> {
        self.drafts.borrow().get(&id).cloned()
    }

    pub fn set_limit(&self, limit: Option<u64>) {
        self.limit.set(limit);
    }

    /// An identity every account can send from (self-tests; `accounts.toml`'s come too).
    pub fn add_identity(&self, label: &str, name: &str, email: &str) {
        self.identities.borrow_mut().push(Sender {
            account: AccountId::default(),
            label: label.to_owned(),
            address: Address::new(name, email),
            signature: None,
        });
    }

    fn next_id(&self) -> DraftId {
        let id = self.next.get();
        self.next.set(id + 1);
        DraftId(id)
    }

    fn fill(
        &self,
        request: ComposeRequest,
        account: AccountId,
        from: &Address,
    ) -> (DraftId, Draft, String) {
        let mut draft = Draft {
            account,
            from: from.clone(),
            message_id: message_id(&from.email),
            ..Draft::default()
        };
        let mut title = "New message".to_owned();
        let id = match request {
            ComposeRequest::New { .. } => self.next_id(),
            ComposeRequest::Mailto(mailto) => {
                let addresses = |list: &[String]| -> Vec<Address> {
                    list.iter()
                        .flat_map(|item| recipients::parse_list(item))
                        .filter_map(|(address, valid)| valid.then_some(address))
                        .collect()
                };
                draft.to = addresses(&mailto.to);
                draft.cc = addresses(&mailto.cc);
                draft.bcc = addresses(&mailto.bcc);
                draft.subject = mailto.subject.unwrap_or_default();
                draft.html = mailto.body.as_deref().map(text_html).unwrap_or_default();
                draft.in_reply_to = mailto.in_reply_to;
                self.next_id()
            }
            ComposeRequest::Reply { message, kind } => {
                title = match kind {
                    ReplyKind::Reply => "Reply",
                    ReplyKind::ReplyAll => "Reply all",
                    ReplyKind::Forward => "Forward",
                }
                .to_owned();
                stand_in_reply(&mut draft, message, kind, "");
                self.next_id()
            }
            ComposeRequest::QuickReply { message, text } => {
                title = "Reply".to_owned();
                stand_in_reply(&mut draft, message, ReplyKind::Reply, &text);
                self.next_id()
            }
            ComposeRequest::Draft(id) => {
                title = "Draft".to_owned();
                if let Some(stored) = self.draft(id) {
                    draft = stored;
                }
                id
            }
            ComposeRequest::ServerDraft(_) => {
                title = "Draft".to_owned();
                draft.subject = "Notes for Thursday".to_owned();
                draft.html =
                    "<div>A draft written elsewhere (a stand-in until phase B).</div>".to_owned();
                self.next_id()
            }
        };
        (id, draft, title)
    }
}

impl Backend for StandIn {
    fn prepare(&self, request: ComposeRequest) -> Local<Result<Prepared, String>> {
        let Some(state) = self.state.upgrade() else {
            return Box::pin(async { Err("Sealwax is quitting".to_owned()) });
        };
        let mailbox = state.mailbox();
        let ready = mailbox.wait_ready();
        let identities = self.identities.borrow().clone();
        let settings = state
            .preferences()
            .map(|prefs| prefs.accounts())
            .unwrap_or_default();
        let wanted = match &request {
            ComposeRequest::New { account } => *account,
            _ => None,
        };
        let me = Weak::clone(&self.state);
        let request = RefCell::new(Some(request));
        let stand_in = super::stand_in();
        Box::pin(async move {
            ready.await;
            let Some(state) = me.upgrade() else {
                return Err("Sealwax is quitting".to_owned());
            };
            let accounts = state.mailbox().accounts();
            let account = wanted
                .and_then(|id| accounts.iter().find(|account| account.id == id))
                .or_else(|| accounts.first())
                .ok_or_else(|| "There's no account to send from. Add one first.".to_owned())?;
            let mut senders = vec![Sender {
                account: account.id,
                label: account.name.clone(),
                address: Address::new(account.name.clone(), account.address.clone()),
                signature: None,
            }];
            if let Some(key) = &account.key
                && let Some(settings) = settings.iter().find(|settings| &settings.key == key)
            {
                senders.extend(settings.identities.iter().map(|identity| Sender {
                    account: account.id,
                    label: identity.name.clone(),
                    address: Address::new(identity.name.clone(), identity.address.clone()),
                    signature: identity.signature.clone(),
                }));
            }
            senders.extend(identities.into_iter().map(|sender| Sender {
                account: account.id,
                ..sender
            }));
            let stand_in = stand_in.ok_or_else(|| "the composer has no backend".to_owned())?;
            let request = request.borrow_mut().take().ok_or("prepared twice")?;
            let (id, mut draft, title) = stand_in.fill(request, account.id, &senders[0].address);
            if !senders
                .iter()
                .any(|sender| sender.address.email == draft.from.email)
            {
                draft.from = senders[0].address.clone();
            }
            Ok(Prepared {
                id,
                draft,
                senders,
                title,
            })
        })
    }

    fn save(&self, id: DraftId, draft: Draft) -> Local<Result<(), String>> {
        self.drafts.borrow_mut().insert(id, draft);
        self.saves.borrow_mut().push((id, Instant::now()));
        Box::pin(async { Ok(()) })
    }

    fn close(&self, id: DraftId) {
        self.closed.borrow_mut().push(id);
    }

    fn discard(&self, id: DraftId) -> Local<Result<(), String>> {
        self.drafts.borrow_mut().remove(&id);
        self.discarded.borrow_mut().push(id);
        Box::pin(async { Ok(()) })
    }

    fn send(&self, id: DraftId, draft: Draft) -> Local<Result<(), Refusal>> {
        self.drafts.borrow_mut().remove(&id);
        self.sent.borrow_mut().push((id, draft));
        Box::pin(async { Ok(()) })
    }

    fn recipients(&self, _account: AccountId, query: String) -> Local<Vec<Suggestion>> {
        let query = query.trim().to_lowercase();
        let mut found: Vec<(u8, Suggestion)> = self
            .contacts
            .borrow()
            .iter()
            .filter_map(|contact| {
                let name = contact.address.name.to_lowercase();
                let email = contact.address.email.to_lowercase();
                let rank = if name.starts_with(&query) || email.starts_with(&query) {
                    0
                } else if name.split_whitespace().any(|word| word.starts_with(&query)) {
                    1
                } else if name.contains(&query) || email.contains(&query) {
                    2
                } else {
                    return None;
                };
                Some((rank, contact.clone()))
            })
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.messages.cmp(&a.1.messages)));
        let found = if query.is_empty() {
            Vec::new()
        } else {
            found.into_iter().map(|(_, contact)| contact).collect()
        };
        Box::pin(async move { found })
    }

    fn submission_limit(&self, _account: AccountId) -> Local<Option<u64>> {
        let limit = self.limit.get();
        Box::pin(async move { limit })
    }

    fn blobs(&self) -> Option<BlobStore> {
        self.state
            .upgrade()
            .and_then(|state| state.mailbox().blobs())
    }
}

/// A Message-ID on the sender's domain (never the machine's name); the compose rules make the
/// real one in phase B.
fn message_id(sender: &str) -> String {
    let token: String = glib_random_hex();
    format!("{token}@{}", id_domain(sender))
}

fn glib_random_hex() -> String {
    gtk4::glib::uuid_string_random().replace('-', "")
}

/// The right-hand side of the composer's Message-IDs and Content-IDs: the sender's domain in
/// ASCII, lower case.
pub fn id_domain(sender: &str) -> String {
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

/// Plain text as the editor's HTML: one `div` a line, escaped.
pub fn text_html(text: &str) -> String {
    text.lines()
        .map(|line| {
            if line.is_empty() {
                "<div><br></div>".to_owned()
            } else {
                format!("<div>{}</div>", escape(line))
            }
        })
        .collect()
}

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            ch => out.push(ch),
        }
    }
    out
}

fn stand_in_reply(
    draft: &mut Draft,
    message: sealwax_domain::model::MessageId,
    kind: ReplyKind,
    text: &str,
) {
    let original = Address::new("Grace Hopper", "grace@example.test");
    draft.subject = match kind {
        ReplyKind::Forward => "Fwd: The quarterly figures",
        _ => "Re: The quarterly figures",
    }
    .to_owned();
    match kind {
        ReplyKind::Reply => draft.to = vec![original.clone()],
        ReplyKind::ReplyAll => {
            draft.to = vec![original.clone()];
            draft.cc = vec![Address::new("Ada Lovelace", "ada@example.test")];
        }
        ReplyKind::Forward => {}
    }
    draft.source = Some(DraftSource { message, kind });
    let typed = if text.is_empty() {
        "<div><br></div>".to_owned()
    } else {
        text_html(text)
    };
    draft.html = format!(
        "{typed}<div><br></div><div>On Monday 5 October 2026 at 14:02, {name} &lt;{email}&gt; \
         wrote:</div><blockquote type=\"cite\"><div>Here are the quarterly figures we talked \
         about. The totals are on the second page.</div><div><br></div><div>Grace</div>\
         </blockquote>",
        name = escape(&original.name),
        email = escape(&original.email),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_use_the_sender_domain() {
        assert_eq!(id_domain("kari@Example.COM"), "example.com");
        assert!(id_domain("ola@blåbær.example").starts_with("xn--"));
        assert_eq!(id_domain("nobody"), "sealwax.invalid");
        let id = message_id("kari@example.com");
        assert!(id.ends_with("@example.com"));
        let host = gtk4::glib::host_name().to_ascii_lowercase();
        assert!(!id.to_ascii_lowercase().contains(host.as_str()));
    }

    #[test]
    fn text_becomes_escaped_lines() {
        assert_eq!(
            text_html("a <b>\n\næøå"),
            "<div>a &lt;b&gt;</div><div><br></div><div>æøå</div>"
        );
    }
}
