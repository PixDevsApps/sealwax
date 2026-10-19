//! The composer's backend on the real store, engine and compose rules (M6 phase B): drafts in
//! the store's drafts table (schema v9), saved with `open` so they reopen after a crash;
//! replies, forwards and quick replies planned by `compose::reply` from the original's raw
//! bytes (downloaded through the engine when the store doesn't hold them), their inline images
//! and a forward's attachments copied into the blob store; Edit Draft through the store or
//! `compose::stored`; sending through the outbox with the undo window; suggestions from the
//! store's contacts; and the account's submission limit from the engine.
//!
//! Every store call is a request answered on the GTK thread; parsing originals, copying parts
//! and deleting blobs run on worker threads (`gio::spawn_blocking`). Nothing here logs mail.

use super::ComposeRequest;
use super::backend::{Backend, Local, Prepared, Refusal, Sender, Suggestion};
use crate::application::AppState;
use crate::engine_link::EngineLink;
use crate::mailbox;
use gtk4::{gio, glib};
use sealwax_domain::account::{AccountSettings, Identity};
use sealwax_domain::compose::reply::{self, Original};
use sealwax_domain::compose::{self, Draft, DraftPart, ReplyKind, identity, ids, mime, stored};
use sealwax_domain::model::{Account, AccountId, Address, DraftId, MessageId};
use sealwax_engine::api::{Command, Event};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{
    BodyState, DraftSaved, SaveDraft, SendQueued, Store, StoredDraft,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

/// The undo window before a queued message goes (triage's 7 s; accepted at the M6 gate).
pub const UNDO_SEND_MS: i64 = 7_000;
/// How long a reply waits for the engine to download the message it answers.
pub const DOWNLOAD_WAIT: Duration = Duration::from_secs(30);
/// How long the size check waits for the engine's answer about the submission limit.
pub const LIMIT_WAIT: Duration = Duration::from_secs(5);
/// Suggestions asked of the store.
const SUGGESTIONS: u32 = 8;

/// Unix milliseconds now.
pub fn now_ms() -> i64 {
    glib::real_time() / 1_000
}

/// 16 bytes from the operating system, for Message-IDs and Content-IDs.
pub fn random() -> [u8; 16] {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // The kernel always answers on Linux; the clock and the process keep ids distinct.
        let mixed = (glib::real_time() as u128) << 32 | u128::from(std::process::id());
        bytes = mixed.to_le_bytes();
    }
    bytes
}

/// A fresh Message-ID on `sender`'s domain.
pub fn message_id(sender: &str) -> String {
    ids::message_id(sender, random())
        .unwrap_or_else(|| format!("{}@sealwax.invalid", ids::token(random())))
}

/// The account's senders: its own address first, then its identities (`accounts.toml`); a
/// local account that no file lists sends as its own address.
pub fn senders(account: &Account, settings: &[AccountSettings]) -> Vec<Identity> {
    match account
        .key
        .as_ref()
        .and_then(|key| settings.iter().find(|settings| &settings.key == key))
    {
        Some(settings) => identity::senders(settings),
        None => vec![Identity {
            name: account.name.clone(),
            address: account.address.clone(),
            signature: None,
        }],
    }
}

fn chips(account: AccountId, senders: &[Identity]) -> Vec<Sender> {
    senders
        .iter()
        .map(|identity| Sender {
            account,
            label: identity.name.clone(),
            address: identity::from_address(identity),
            signature: identity.signature.clone(),
        })
        .collect()
}

/// The composer's backend on the store and the engine.
pub struct StoreBackend {
    state: Weak<AppState>,
    /// Submission limits the engine reported, per account.
    limits: Rc<RefCell<HashMap<AccountId, Option<u64>>>>,
    /// Self-tests: the limit every account reports instead of asking the engine.
    limit_override: Cell<Option<Option<u64>>>,
    /// The account of each draft this process opened, for `DraftsChanged`.
    accounts: Rc<RefCell<HashMap<DraftId, AccountId>>>,
}

impl StoreBackend {
    pub fn new(state: &Rc<AppState>) -> Rc<Self> {
        Rc::new(Self {
            state: Rc::downgrade(state),
            limits: Rc::default(),
            limit_override: Cell::new(None),
            accounts: Rc::default(),
        })
    }

    /// Self-tests: every account's limit is `limit` from now on.
    pub fn set_limit(&self, limit: Option<u64>) {
        self.limit_override.set(Some(limit));
    }

    fn engine(&self) -> Option<EngineLink> {
        self.state.upgrade().map(|state| state.engine())
    }

    /// Records that draft `id` is `account`'s now; the account it left, if it moved (issue #7),
    /// is told to delete its copy at once.
    fn moved_to(&self, id: DraftId, account: AccountId) -> Option<AccountId> {
        self.accounts
            .borrow_mut()
            .insert(id, account)
            .filter(|left| *left != account)
    }
}

/// The account a draft left gets `DraftsChanged`, so its Drafts-folder copy goes now.
fn tell_left(engine: Option<EngineLink>, left: Option<AccountId>) {
    if let (Some(engine), Some(account)) = (engine, left) {
        let _ = engine.send(Command::DraftsChanged { account });
    }
}

/// What prepare needs from the app, read on the GTK thread before the first await.
struct Context {
    state: Weak<AppState>,
    settings: Vec<AccountSettings>,
}

impl Context {
    fn state(&self) -> Result<Rc<AppState>, String> {
        self.state
            .upgrade()
            .ok_or_else(|| "Sealwax is quitting".to_owned())
    }

    fn store(&self) -> Result<Arc<Store>, String> {
        self.state()?
            .mailbox()
            .shared_store()
            .ok_or_else(|| "The mail store isn't open.".to_owned())
    }

    fn blobs(&self) -> Result<BlobStore, String> {
        self.state()?
            .mailbox()
            .blobs()
            .ok_or_else(|| "The mail store isn't open.".to_owned())
    }

    fn account(&self, id: Option<AccountId>) -> Result<Account, String> {
        let accounts = self.state()?.mailbox().accounts();
        id.and_then(|id| accounts.iter().find(|account| account.id == id))
            .or_else(|| accounts.first())
            .cloned()
            .ok_or_else(|| "There's no account to send from. Add one first.".to_owned())
    }

    fn senders(&self, account: &Account) -> Vec<Identity> {
        senders(account, &self.settings)
    }

    /// Who a draft of `account` can be sent as: every account that sends, in the sidebar's
    /// order, each its own address then its identities (issue #7). An account no settings file
    /// lists has no server, so it sends nothing, unless it is the draft's own.
    fn all_senders(&self, account: &Account) -> Result<Vec<Sender>, String> {
        Ok(self
            .state()?
            .mailbox()
            .accounts()
            .iter()
            .filter(|known| known.key.is_some() || known.id == account.id)
            .flat_map(|known| chips(known.id, &self.senders(known)))
            .collect())
    }

    /// The account a stored message belongs to.
    async fn account_of(&self, message: MessageId) -> Result<Account, String> {
        let detail = self
            .store()?
            .message(message)
            .recv()
            .await
            .map_err(|_| "The message couldn't be read.".to_owned())?
            .value
            .ok_or_else(|| "The message is gone.".to_owned())?;
        let folder = self
            .state()?
            .mailbox()
            .folder(detail.summary.folder)
            .ok_or_else(|| "The message's folder is gone.".to_owned())?;
        self.account(Some(folder.account))
    }

    /// A new draft in the store, open in a window.
    async fn create(&self, draft: Draft) -> Result<DraftId, String> {
        let saved = self
            .store()?
            .save_draft(SaveDraft {
                id: None,
                draft,
                open: true,
                now: now_ms(),
            })
            .recv()
            .await
            .map_err(|_| "The draft couldn't be saved.".to_owned())?;
        match saved {
            DraftSaved::Saved { id, .. } => Ok(id),
            _ => Err("The draft couldn't be saved.".to_owned()),
        }
    }

    /// The raw message `message` as the server has it: from the blob store, after the engine
    /// downloads its body if the store has none, or whole from the server when the store
    /// keeps only its text parts, unless `text_will_do` (a reply, which needs no attachments,
    /// gets its inline images one by one: [`Self::fetch_parts`]). `None` with why it couldn't
    /// be had.
    async fn original(&self, message: MessageId, text_will_do: bool) -> Result<Vec<u8>, String> {
        let store = self.store()?;
        let target = store
            .body_targets(vec![message])
            .recv()
            .await
            .map_err(|_| "The message couldn't be read.".to_owned())?
            .pop()
            .ok_or_else(|| "The message is gone.".to_owned())?;
        let engine = self.state()?.engine();
        match target.body_state {
            BodyState::Full => {}
            BodyState::Text if text_will_do => {}
            BodyState::Text => {
                let fetched = ask(
                    &engine,
                    Command::FetchOriginal { message },
                    move |event| match event {
                        Event::OriginalFetched {
                            message: answered,
                            result,
                        } if *answered == message => Some(result.clone()),
                        _ => None,
                    },
                    DOWNLOAD_WAIT,
                )
                .await;
                if let Some(Ok(original)) = fetched {
                    return Ok(original.0);
                }
                // Without the server, the stored text parts still make the quote; the parts
                // left empty are dropped from the draft.
            }
            BodyState::Headers | BodyState::Partial => {
                let fetched = ask(
                    &engine,
                    Command::FetchBodies {
                        messages: vec![message],
                    },
                    move |event| match event {
                        Event::BodyFetched {
                            message: answered,
                            result,
                        } if *answered == message => Some(result.is_ok()),
                        _ => None,
                    },
                    DOWNLOAD_WAIT,
                )
                .await;
                if fetched != Some(true) {
                    return Err(
                        "The message couldn't be downloaded. Try again when Sealwax is online."
                            .to_owned(),
                    );
                }
            }
        }
        store
            .raw_message(message)
            .recv()
            .await
            .ok()
            .flatten()
            .ok_or_else(|| "The message couldn't be read.".to_owned())
    }

    /// The bytes of `parts` of `message`, from [`crate::part_cache`] (the reader keeps the
    /// images it showed) or each downloaded by itself, all at once: `None` for a part the
    /// engine couldn't get.
    async fn fetch_parts(&self, message: MessageId, parts: &[u32]) -> Vec<Option<Vec<u8>>> {
        let Ok(state) = self.state() else {
            return vec![None; parts.len()];
        };
        let engine = state.engine();
        let fetches: Vec<_> = parts
            .iter()
            .map(|&part| {
                let engine = engine.clone();
                glib::spawn_future_local(async move {
                    if let Some(data) = crate::part_cache::get(message, part) {
                        return Some(data);
                    }
                    let data = ask(
                        &engine,
                        Command::FetchPart { message, part },
                        move |event| match event {
                            Event::PartFetched {
                                message: answered,
                                part: index,
                                result,
                            } if *answered == message && *index == part => {
                                Some(result.as_ref().ok().map(|fetched| fetched.data.clone()))
                            }
                            _ => None,
                        },
                        DOWNLOAD_WAIT,
                    )
                    .await
                    .flatten()?;
                    crate::part_cache::put(message, part, &data);
                    Some(data)
                })
            })
            .collect();
        let mut fetched = Vec::with_capacity(fetches.len());
        for fetch in fetches {
            fetched.push(fetch.await.ok().flatten());
        }
        fetched
    }
}

/// Sends `command` and waits at most `limit` for the first event `pick` takes; `None` when
/// no engine is connected or it didn't answer in time.
async fn ask<T: 'static>(
    engine: &EngineLink,
    command: Command,
    pick: impl Fn(&Event) -> Option<T> + 'static,
    limit: Duration,
) -> Option<T> {
    let (sender, receiver) = async_channel::bounded(1);
    let _subscription = engine.subscribe(move |event| {
        if let Some(value) = pick(event) {
            let _ = sender.try_send(value);
        }
    });
    engine.send(command).ok()?;
    mailbox::with_timeout(receiver.recv(), limit).await?.ok()
}

/// Copies `parts` of `raw` into the blob store, in order; `None` for a part whose bytes the
/// message doesn't hold (a copy stored as its text parts). Runs off the GTK thread.
fn copy_parts(
    blobs: &BlobStore,
    raw: &[u8],
    parts: &[sealwax_domain::model::Attachment],
) -> Vec<Option<DraftPart>> {
    parts
        .iter()
        .map(|part| {
            let data = sealwax_domain::message::part(raw, part.part)?.data;
            store_part(blobs, part, &data)
        })
        .collect()
}

/// `data`, the bytes of `part`, in the blob store as a draft's part; `None` when empty.
fn store_part(
    blobs: &BlobStore,
    part: &sealwax_domain::model::Attachment,
    data: &[u8],
) -> Option<DraftPart> {
    if data.is_empty() {
        return None;
    }
    let blob = blobs.put(data).ok()?;
    let mut draft_part = reply::draft_part(part, blob);
    draft_part.size = data.len() as u64;
    Some(draft_part)
}

fn title(kind: ReplyKind) -> &'static str {
    match kind {
        ReplyKind::Reply => "Reply",
        ReplyKind::ReplyAll => "Reply all",
        ReplyKind::Forward => "Forward",
    }
}

async fn prepare(context: Context, request: ComposeRequest) -> Result<Prepared, String> {
    let state = context.state()?;
    state.mailbox().wait_ready().await;
    let (account, draft, title, id) = match request {
        ComposeRequest::New { account } => {
            let account = context.account(account)?;
            let sender = context.senders(&account).remove(0);
            let draft = compose::new_draft(account.id, &sender, message_id(&sender.address));
            (account, draft, "New message".to_owned(), None)
        }
        ComposeRequest::Mailto(mailto) => {
            let account = context.account(None)?;
            let sender = context.senders(&account).remove(0);
            let mut draft = compose::new_draft(account.id, &sender, message_id(&sender.address));
            mailto.fill(&mut draft);
            (account, draft, "New message".to_owned(), None)
        }
        ComposeRequest::Reply { message, kind } => {
            let account = context.account_of(message).await?;
            let draft = answer(&context, &account, message, kind, "").await?;
            (account, draft, title(kind).to_owned(), None)
        }
        ComposeRequest::QuickReply {
            message,
            text,
            kind,
        } => {
            let account = context.account_of(message).await?;
            let typed = compose::text::to_html(&text);
            let draft = answer(&context, &account, message, kind, &typed).await?;
            (account, draft, title(kind).to_owned(), None)
        }
        ComposeRequest::Draft(id) => {
            let stored = stored_draft(&context, id).await?;
            let account = context.account(Some(stored.draft.account))?;
            (account, stored.draft, "Draft".to_owned(), Some(id))
        }
        ComposeRequest::ServerDraft(message) => {
            let store = context.store()?;
            let mine = store
                .draft_for_message(message)
                .recv()
                .await
                .map_err(|_| "The draft couldn't be read.".to_owned())?;
            match mine {
                Some(id) => {
                    let stored = stored_draft(&context, id).await?;
                    let account = context.account(Some(stored.draft.account))?;
                    (account, stored.draft, "Draft".to_owned(), Some(id))
                }
                None => {
                    let account = context.account_of(message).await?;
                    let draft = server_draft(&context, &account, message).await?;
                    (account, draft, "Draft".to_owned(), None)
                }
            }
        }
    };
    let id = match id {
        Some(id) => {
            let store = context.store()?;
            let _ = store.set_draft_open(id, true).recv().await;
            id
        }
        None => context.create(draft.clone()).await?,
    };
    let senders = context.all_senders(&account)?;
    Ok(Prepared {
        id,
        draft,
        senders,
        title,
    })
}

async fn stored_draft(context: &Context, id: DraftId) -> Result<StoredDraft, String> {
    let stored = context
        .store()?
        .draft(id)
        .recv()
        .await
        .map_err(|_| "The draft couldn't be read.".to_owned())?
        .ok_or_else(|| "This draft is gone: it was sent or discarded.".to_owned())?;
    if stored.send.is_some() {
        return Err(
            "This message is being sent; Undo in the main window takes it back.".to_owned(),
        );
    }
    Ok(stored)
}

/// A reply, reply all, forward or quick reply to `message`, `typed` above the quote.
async fn answer(
    context: &Context,
    account: &Account,
    message: MessageId,
    kind: ReplyKind,
    typed: &str,
) -> Result<Draft, String> {
    let raw = context
        .original(message, kind != ReplyKind::Forward)
        .await?;
    let blobs = context.blobs()?;
    let senders = context.senders(account);
    let worker = blobs.clone();
    let planned = gio::spawn_blocking(move || {
        let original = Original::parse(&raw);
        let planned = reply::plan(&original, kind, &senders);
        let parts = copy_parts(&worker, &raw, &planned.parts);
        (planned, parts)
    })
    .await
    .map_err(|_| "The message couldn't be read.".to_owned())?;
    let (planned, mut parts) = planned;
    let missing: Vec<usize> = (0..parts.len()).filter(|&at| parts[at].is_none()).collect();
    if !missing.is_empty() {
        let indexes: Vec<u32> = missing.iter().map(|&at| planned.parts[at].part).collect();
        let fetched = context.fetch_parts(message, &indexes).await;
        let wanted: Vec<_> = missing
            .iter()
            .zip(fetched)
            .filter_map(|(&at, data)| Some((at, planned.parts[at].clone(), data?)))
            .collect();
        let stored = gio::spawn_blocking(move || {
            wanted
                .into_iter()
                .map(|(at, part, data)| (at, store_part(&blobs, &part, &data)))
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        for (at, part) in stored {
            parts[at] = part;
        }
    }
    let id = message_id(&planned.sender.address);
    let parts = parts.into_iter().flatten().collect();
    Ok(planned.draft(account.id, message, id, typed, parts))
}

/// A Drafts-folder message written elsewhere, as a new local draft.
async fn server_draft(
    context: &Context,
    account: &Account,
    message: MessageId,
) -> Result<Draft, String> {
    let raw = context.original(message, false).await?;
    let blobs = context.blobs()?;
    let parsed = gio::spawn_blocking(move || {
        let stored = stored::parse(&raw)?;
        let parts: Vec<DraftPart> = stored
            .parts
            .iter()
            .filter(|part| !part.data.is_empty())
            .filter_map(|part| blobs.put(&part.data).ok().map(|blob| part.draft_part(blob)))
            .collect();
        Some((stored, parts))
    })
    .await
    .map_err(|_| "The draft couldn't be read.".to_owned())?;
    let (stored, parts) =
        parsed.ok_or_else(|| "This message isn't a draft Sealwax can open.".to_owned())?;
    let senders = context.senders(account);
    let from = stored
        .from
        .as_ref()
        .filter(|from| identity::is_own(&senders, &from.email))
        .cloned()
        .unwrap_or_else(|| identity::from_address(&senders[0]));
    let id = stored
        .message_id
        .clone()
        .filter(|id| same_domain(id, &from.email))
        .unwrap_or_else(|| message_id(&from.email));
    Ok(stored.draft(account.id, from, id, parts))
}

/// Whether Message-ID `id` is on `sender`'s domain (the builder refuses one that isn't).
pub fn same_domain(id: &str, sender: &str) -> bool {
    match (
        id.rsplit_once('@')
            .map(|(_, domain)| domain.to_ascii_lowercase()),
        ids::ascii_domain(sender),
    ) {
        (Some(id_domain), Some(sender_domain)) => id_domain == sender_domain,
        _ => false,
    }
}

impl Backend for StoreBackend {
    fn prepare(&self, request: ComposeRequest) -> Local<Result<Prepared, String>> {
        let settings = self
            .state
            .upgrade()
            .and_then(|state| state.preferences())
            .map(|prefs| prefs.accounts())
            .unwrap_or_default();
        let context = Context {
            state: self.state.clone(),
            settings,
        };
        let accounts = Rc::downgrade(&self.accounts);
        Box::pin(async move {
            let prepared = prepare(context, request).await?;
            if let Some(accounts) = accounts.upgrade() {
                accounts
                    .borrow_mut()
                    .insert(prepared.id, prepared.draft.account);
            }
            Ok(prepared)
        })
    }

    fn save(&self, id: DraftId, draft: Draft) -> Local<Result<(), String>> {
        let left = self.moved_to(id, draft.account);
        let engine = self.engine();
        let store = self
            .state
            .upgrade()
            .and_then(|state| state.mailbox().shared_store());
        Box::pin(async move {
            let store = store.ok_or("the mail store isn't open")?;
            let saved = store
                .save_draft(SaveDraft {
                    id: Some(id),
                    draft,
                    open: true,
                    now: now_ms(),
                })
                .recv()
                .await
                .map_err(|_| "the store failed".to_owned())?;
            match saved {
                DraftSaved::Saved { .. } => {
                    tell_left(engine, left);
                    Ok(())
                }
                DraftSaved::NotEditable { .. } => Err("the draft is being sent".to_owned()),
                DraftSaved::Gone => Err("the draft is gone".to_owned()),
            }
        })
    }

    fn close(&self, id: DraftId) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let Some(store) = state.mailbox().shared_store() else {
            return;
        };
        let account = self.accounts.borrow().get(&id).copied();
        let engine = state.engine();
        glib::spawn_future_local(async move {
            let _ = store.set_draft_open(id, false).recv().await;
            if let Some(account) = account {
                let _ = engine.send(Command::DraftsChanged { account });
            }
        });
    }

    fn discard(&self, id: DraftId) -> Local<Result<(), String>> {
        let state = self.state.upgrade();
        let store = state
            .as_ref()
            .and_then(|state| state.mailbox().shared_store());
        let blobs = state.as_ref().and_then(|state| state.mailbox().blobs());
        let engine = state.as_ref().map(|state| state.engine());
        Box::pin(async move {
            let store = store.ok_or("the mail store isn't open")?;
            let discarded = store
                .discard_draft(id)
                .recv()
                .await
                .map_err(|_| "the store failed".to_owned())?;
            if discarded.refused.is_some() {
                return Err("it is being sent".to_owned());
            }
            if let Some(blobs) = blobs
                && !discarded.orphaned_blobs.is_empty()
            {
                let orphaned = discarded.orphaned_blobs.clone();
                let _ = gio::spawn_blocking(move || {
                    for key in &orphaned {
                        let _ = blobs.remove(key);
                    }
                })
                .await;
            }
            if discarded.server_copy
                && let (Some(account), Some(engine)) = (discarded.account, engine)
            {
                let _ = engine.send(Command::DraftsChanged { account });
            }
            Ok(())
        })
    }

    fn send(&self, id: DraftId, draft: Draft) -> Local<Result<(), Refusal>> {
        let state = self.state.clone();
        let limit = self.submission_limit(draft.account);
        let left = self.moved_to(id, draft.account);
        let engine = self.engine();
        Box::pin(async move {
            let refuse = |text: &str| Refusal(text.to_owned());
            let size = mime::estimate(&draft);
            if let Some(limit) = limit.await.filter(|limit| size > *limit) {
                return Err(Refusal(format!(
                    "Not sent: {} is more than this account's server takes ({})",
                    glib::format_size(size),
                    glib::format_size(limit)
                )));
            }
            let state = state
                .upgrade()
                .ok_or_else(|| refuse("Sealwax is quitting"))?;
            let store = state
                .mailbox()
                .shared_store()
                .ok_or_else(|| refuse("The mail store isn't open."))?;
            let saved = store
                .save_draft(SaveDraft {
                    id: Some(id),
                    draft: draft.clone(),
                    open: false,
                    now: now_ms(),
                })
                .recv()
                .await
                .map_err(|_| refuse("The draft couldn't be saved, so it wasn't sent."))?;
            if !matches!(saved, DraftSaved::Saved { .. }) {
                return Err(refuse(
                    "This draft can't be sent: it is being sent, or gone.",
                ));
            }
            tell_left(engine, left);
            let queued = store
                .queue_send(id, now_ms() + UNDO_SEND_MS)
                .recv()
                .await
                .map_err(|_| refuse("The message couldn't be queued."))?;
            let refusal = match queued {
                SendQueued::Queued { account, .. } => {
                    let _ = state.engine().send(Command::OutboxQueued { account });
                    crate::outbox::queued(&state, id, account);
                    return Ok(());
                }
                SendQueued::LocalAccount => {
                    "Not sent: this account has no mail server to send from."
                }
                SendQueued::NotEditable(_) => "This message is already being sent.",
                SendQueued::Gone => "This draft is gone: it was sent or discarded.",
            };
            // Still a draft in a window: keep it reopening after a crash.
            let _ = store
                .save_draft(SaveDraft {
                    id: Some(id),
                    draft,
                    open: true,
                    now: now_ms(),
                })
                .recv()
                .await;
            Err(refuse(refusal))
        })
    }

    fn recipients(&self, account: AccountId, query: String) -> Local<Vec<Suggestion>> {
        let store = self
            .state
            .upgrade()
            .and_then(|state| state.mailbox().shared_store());
        Box::pin(async move {
            let Some(store) = store else {
                return Vec::new();
            };
            if query.trim().is_empty() {
                return Vec::new();
            }
            store
                .recipients(account, query.trim(), SUGGESTIONS)
                .recv()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|contact| Suggestion {
                    address: Address::new(contact.name, contact.address),
                    messages: contact.seen,
                })
                .collect()
        })
    }

    fn submission_limit(&self, account: AccountId) -> Local<Option<u64>> {
        if let Some(limit) = self.limit_override.get() {
            return Box::pin(async move { limit });
        }
        if let Some(limit) = self.limits.borrow().get(&account).copied() {
            return Box::pin(async move { limit });
        }
        let engine = self.engine();
        let limits = Rc::downgrade(&self.limits);
        Box::pin(async move {
            let engine = engine?;
            let request = engine.next_request();
            let answer = ask(
                &engine,
                Command::SubmissionLimit { request, account },
                move |event| match event {
                    Event::SubmissionLimit {
                        request: answered,
                        result,
                        ..
                    } if *answered == request => Some(result.clone()),
                    _ => None,
                },
                LIMIT_WAIT,
            )
            .await;
            match answer {
                Some(Ok(limit)) => {
                    if let Some(limits) = limits.upgrade() {
                        limits.borrow_mut().insert(account, limit);
                    }
                    limit
                }
                // Unknown until the engine answers (it refuses while that is unbuilt, or
                // offline): no limit is enforced, and it is asked again next time.
                _ => None,
            }
        })
    }

    fn blobs(&self) -> Option<BlobStore> {
        self.state
            .upgrade()
            .and_then(|state| state.mailbox().blobs())
    }
}
