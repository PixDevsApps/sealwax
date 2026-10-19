//! The reader (ML-027, ML-057; ADR-011 approach A; DESIGN.md "Reader" and "HTML mail inside the
//! theme"): the conversation of the selected message, its subject as the heading and its
//! messages oldest first, the selected one, the unread ones and the latest expanded as cards
//! (header, banner and attachment cards drawn by GTK, a plain-text body in a text view, an HTML
//! body in a pooled, hardened WebView, see [`view`], styled from the live theme), the others as
//! one-line rows ([`conversation`]). Everything is read from the store off the GTK thread; the
//! conversation appears once the bodies the first screen needs have their heights, scrolled to
//! the selected message. At most [`MAX_LIVE_VIEWS`] bodies hold a view ([`live`]).
//!
//! The lead wires it: `Reader::new` with the store and the theme, `widget()` into
//! `Window::set_reader`, `toolbar()` into `Window::set_reader_toolbar`, the list's selection
//! into [`Reader::show`], the store's change events into [`Reader::store_changed`], and the
//! list's position into [`Reader::set_position`] with [`Reader::connect_previous`] and
//! [`Reader::connect_next`].

mod attachments;
mod cards;
pub mod commands;
mod conversation;
mod engine;
mod export;
mod files;
mod format;
mod links;
mod live;
mod page;
mod plain;
mod prepare;
mod protection;
mod remote;
mod reply;
pub mod samples;
mod show;
mod source;
mod toolbar;
mod view;
mod widgets;

pub use attachments::CardState;
pub use conversation::{COLLAPSE_OVER, GAP_STEP, KEEP_LATEST};
pub use engine::{
    CONTENT_SECURITY_POLICY, CidOutcome, CidRequest, InlinePart, MAX_LIVE_VIEWS, NETWORK_FEATURES,
    PartLoader, RETIRED_FEATURES,
};
pub use export::{ORIGINAL_SHOWN, OriginalDialog, original_dialog};
pub use files::FileChoice;
pub use links::LinkState;
pub use plain::{links_in, shown_text};
pub use protection::{ENCRYPTED, SIGNED, Unreadable};
pub use reply::{
    Local, PLACEHOLDER, QuickReport, QuickState, expand_hint, reply_label, reply_target,
};
pub use sealwax_domain::html_scan::{Evidence, Scan, scan as scan_html};
pub use source::{MessageSource, Reads};
pub use toolbar::{Position, tooltip};
pub use view::{Hover, Link, MAX_VIEW_HEIGHT, Refused, ViewEvent, WORLD, disabled_features};

use crate::actions::{self, ScopedShortcuts};
use crate::engine_link::{EngineLink, Subscription};
use crate::theme::{ListenerId, ThemeController};
use conversation::{Entry, Page};
use engine::Engine;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use sealwax_domain::actions::{ActionId, Scope};
use sealwax_domain::config::{ConversationOrder, DefaultReply};
use sealwax_domain::keymap::Keymap;
use sealwax_domain::model::{MessageId, MessageSummary};
use sealwax_domain::theme::{BodyStyle, Rgb, Theme};
use sealwax_engine::api::{BodyError, Command, Event};
use sealwax_infrastructure::store::ChangeEvent;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};
use toolbar::Toolbar;
use widgets::Stage;

/// How long a new conversation may take to lay out before it is shown anyway.
const READY_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a conversation stays on screen while the next one is laid out, when the selection
/// moves on (`j`/`k`, a click, Previous/Next) or its message left the list (archived, deleted,
/// moved, gone in a sync): PRD's bar for opening cached HTML. Then the next one shows as far as
/// it is laid out, its bodies filling in; one not read yet hides the left one until it is.
pub const LEFT_SHOWN: Duration = Duration::from_millis(150);
/// A reader hidden this long gives its views back (ADR-011: views of hidden bodies are released
/// and re-rendered on return).
const RELEASE_HIDDEN_AFTER: Duration = Duration::from_secs(3);
/// Keys a focused message body keeps from the Reader scope: WebKitGTK 2.52 and the plain-text
/// view take Delete (seen headless).
const KEPT_KEYS: [gdk::Key; 1] = [gdk::Key::Delete];

/// What [`Reader::show`] showed, for listeners such as the self-test and measurements.
#[derive(Debug, Clone, PartialEq)]
pub struct Shown {
    pub id: MessageId,
    /// The selected message's body.
    pub body: ShownBody,
    /// From `show` to the conversation read.
    pub fetched: Duration,
    /// To the expanded messages read and their bodies prepared off the GTK thread.
    pub prepared: Duration,
    /// To the conversation on screen (the bodies its first screen shows laid out at their
    /// width, their images in).
    pub ready: Duration,
    /// Messages in the conversation, folded ones included.
    pub messages: usize,
    /// Messages shown expanded.
    pub expanded: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ShownBody {
    Html {
        style: BodyStyle,
        remote_images: bool,
        remote_allowed: bool,
        inline_parts: usize,
    },
    Plain,
    /// HTML shown as text because no WebView could show it safely (no content filter, or the
    /// web process ended twice).
    TextFallback,
    Empty,
    /// The body isn't stored and nothing can download it (no engine).
    NotDownloaded,
    /// The engine is downloading the body (ML-044); the snippet shows meanwhile.
    Downloading,
    /// The download failed; the reader says why and offers Retry.
    DownloadFailed(BodyError),
    /// The store no longer has the message, or couldn't read it.
    Missing,
    /// Encrypted, or sealed in an S/MIME signature: a notice instead (ML-064).
    Unreadable(Unreadable),
}

impl ShownBody {
    /// The message's own text is on screen (not a notice in its place); an encrypted message's
    /// notice is all there is of it.
    pub fn is_shown(&self) -> bool {
        matches!(
            self,
            ShownBody::Html { .. }
                | ShownBody::Plain
                | ShownBody::TextFallback
                | ShownBody::Empty
                | ShownBody::Unreadable(_)
        )
    }
}

/// Bodies the reader put on screen (ML-057), for marking them read: at `show`, every expanded
/// message whose body is shown; later, a message the user expanded, or whose body arrived.
#[derive(Debug, Clone, PartialEq)]
pub struct Opened {
    /// The message the reader was asked to show.
    pub selected: MessageId,
    pub messages: Vec<MessageSummary>,
}

/// The reader's state, for checks. The first fields describe the selected message.
#[derive(Debug, Clone, PartialEq)]
pub struct ReaderState {
    pub message: Option<MessageId>,
    pub body: Option<ShownBody>,
    pub banner: bool,
    pub attachments: usize,
    /// The selected message's HTML view: its height (px) and allocated width.
    pub view_size: Option<(i32, i32)>,
    /// The selected message's view went back to the pool while the reader was hidden, and
    /// hasn't been rendered again.
    pub released: bool,
    /// Views alive in the process, idle ones included.
    pub live_views: usize,
    /// Messages in the conversation, folded ones included.
    pub messages: usize,
    /// Expanded messages, in order.
    pub expanded: Vec<MessageId>,
    /// Messages shown as one-line rows, in order.
    pub collapsed: Vec<MessageId>,
    /// "N earlier messages" rows, and the messages they hold.
    pub folds: usize,
    pub folded: usize,
    /// Expanded HTML bodies, and those that hold a view now.
    pub html_bodies: usize,
    pub live_bodies: Vec<MessageId>,
}

/// An expanded HTML body of the shown conversation, for checks: where the column shows it (px
/// from the top of its visible part), whether it holds a view, and its heights.
#[derive(Debug, Clone, PartialEq)]
pub struct BodyReport {
    pub id: MessageId,
    pub top: f64,
    pub bottom: f64,
    pub live: bool,
    /// The view's current load fired its `load` event and painted a frame.
    pub loaded: bool,
    pub painted: bool,
    /// The height last measured, and the height its box has now.
    pub measured: Option<i32>,
    pub height: i32,
}

type ShownListener = Rc<dyn Fn(&Shown)>;
type OpenedListener = Rc<dyn Fn(&Opened)>;
type SizeListener = Rc<dyn Fn(usize)>;
type EventListener = Rc<dyn Fn(MessageId, &ViewEvent)>;

/// Asks for the bodies of messages; `false` when nothing can download them.
type Fetcher = Rc<dyn Fn(&[MessageId]) -> bool>;

#[derive(Clone)]
pub struct Reader {
    inner: Rc<Inner>,
}

struct Inner {
    store: Rc<dyn MessageSource>,
    theme: Rc<ThemeController>,
    engine: Rc<Engine>,
    stage: Stage,
    toolbar: Rc<Toolbar>,
    keys: ScopedShortcuts,
    /// The conversation on screen.
    shown: RefCell<Option<Page>>,
    /// The conversation being read and laid out unseen; it replaces the shown one when ready.
    pending: RefCell<Option<Page>>,
    /// Bumped by every `show` and `clear`; a page's key is the `show` it came from.
    generation: Cell<u64>,
    /// Tags card loads and view loads, so late answers are dropped.
    tokens: Cell<u64>,
    /// The compiled content filter, once the engine has it.
    filter: RefCell<Option<webkit6::UserContentFilter>>,
    /// Whose remote images load: messages allowed this session, and senders (ML-058).
    remote: remote::Policy,
    /// Messages whose view's web process ended; a second time, they are shown as text.
    crashed: RefCell<HashMap<MessageId, u32>>,
    listener: Cell<Option<ListenerId>>,
    /// The status strip, the warning and the vetted opener (ML-062); its overlay is the root.
    links: Rc<links::Links>,
    on_shown: RefCell<Vec<ShownListener>>,
    on_opened: RefCell<Vec<OpenedListener>>,
    on_size: RefCell<Vec<SizeListener>>,
    on_event: RefCell<Vec<EventListener>>,
    hidden_timer: RefCell<Option<glib::SourceId>>,
    /// The window whose focus the reader follows, and the handler.
    focus_window: RefCell<Option<(glib::WeakRef<gtk::Window>, glib::SignalHandlerId)>>,
    /// Downloads bodies the store doesn't hold (ML-044); without one they are "not downloaded".
    fetcher: RefCell<Option<Fetcher>>,
    /// Bodies asked for and not answered yet.
    fetching: RefCell<HashSet<MessageId>>,
    /// The engine's answer for a message whose card loads again, read by that load.
    answers: RefCell<HashMap<MessageId, Result<(), BodyError>>>,
    /// The message the newest `show` is for.
    target: Cell<Option<MessageId>>,
    engine_events: RefCell<Option<Subscription>>,
    /// The live views' scheduling and the column's scroll ([`live`]).
    scheduling: live::Scheduling,
    /// A change to the shown conversation waits for its re-read.
    refresh_queued: Cell<bool>,
    /// Woken by every body event, for the layout wait of a new conversation.
    progress: (async_channel::Sender<()>, async_channel::Receiver<()>),
    /// Attachments, Print, Show Original and Save as .eml (ML-061, ML-063).
    files: files::Files,
    /// The Composer scope's keys, on each conversation's quick reply (M6, ML-068).
    composer_keys: ScopedShortcuts,
    /// The keymap in use, for the cards' Reply buttons and the quick reply.
    keymap: RefCell<Keymap>,
    /// The user's own addresses, as the last card read them: the quick reply answers the
    /// latest message that isn't theirs.
    mine: RefCell<Vec<String>>,
    /// The quick replies' texts, and where compose requests and Send go (M6).
    quick: reply::Quick,
    /// `config.toml`'s `conversation_order` is `newest-first` (issue #3).
    newest_first: Cell<bool>,
    /// `config.toml`'s `[reader] reply` is `all`: Reply answers everyone (issue #8).
    reply_all: Cell<bool>,
    /// The shown page the selection moved on from, and when it must be off screen.
    left: Cell<Option<(u64, Instant)>>,
    /// The shown page is hidden: the selection moved on from it and the next page isn't read yet.
    hidden: Cell<bool>,
    /// Pages being laid out never finish, for the self-tests ([`Reader::hold_layout`]).
    layout_held: Cell<bool>,
}

impl Reader {
    /// A reader over `store`, styled by `theme`, with the Reader scope's keys from `keymap`.
    /// `cache` is Sealwax's cache folder (`SealwaxDirs::cache`); the compiled content filter
    /// lives there.
    pub fn new(
        store: Rc<dyn MessageSource>,
        theme: Rc<ThemeController>,
        keymap: &Keymap,
        cache: &Path,
    ) -> Reader {
        let stage = Stage::default();
        stage.add_css_class("sw-reader");
        stage.add_css_class("sw-reader-stage");
        stage.set_widget_name("reader-message");
        stage.set_hexpand(true);
        let toolbar = Toolbar::new(keymap);
        let keys = actions::install_scoped(&stage, Scope::Reader, keymap);
        keys.attach(toolbar.widget(), gtk::PropagationPhase::Bubble);
        let links = links::Links::new(&stage, Rc::new(launch));
        let inner = Rc::new(Inner {
            store,
            theme,
            engine: Engine::shared(cache),
            stage,
            toolbar,
            keys,
            shown: RefCell::new(None),
            pending: RefCell::new(None),
            generation: Cell::new(0),
            tokens: Cell::new(0),
            filter: RefCell::new(None),
            remote: remote::Policy::default(),
            crashed: RefCell::new(HashMap::new()),
            listener: Cell::new(None),
            links,
            on_shown: RefCell::new(Vec::new()),
            on_opened: RefCell::new(Vec::new()),
            on_size: RefCell::new(Vec::new()),
            on_event: RefCell::new(Vec::new()),
            hidden_timer: RefCell::new(None),
            focus_window: RefCell::new(None),
            fetcher: RefCell::new(None),
            fetching: RefCell::new(HashSet::new()),
            answers: RefCell::new(HashMap::new()),
            target: Cell::new(None),
            engine_events: RefCell::new(None),
            scheduling: live::Scheduling::default(),
            refresh_queued: Cell::new(false),
            progress: async_channel::unbounded(),
            files: files::Files::new(cache),
            composer_keys: actions::ScopedShortcuts::new(Scope::Composer, keymap),
            keymap: RefCell::new(keymap.clone()),
            mine: RefCell::new(Vec::new()),
            quick: reply::Quick::default(),
            newest_first: Cell::new(false),
            reply_all: Cell::new(false),
            left: Cell::new(None),
            hidden: Cell::new(false),
            layout_held: Cell::new(false),
        });
        remote::load(&inner);
        let weak = Rc::downgrade(&inner);
        let id = inner.theme.connect_changed(move |theme| {
            if let Some(inner) = weak.upgrade() {
                inner.restyle(theme);
            }
        });
        inner.listener.set(Some(id));
        Inner::follow_visibility(&inner);
        Inner::take_kept_keys(&inner);
        Reader { inner }
    }

    /// Starts the web process ahead of the first message, before the reader is first on
    /// screen (it maps with the first selection), so the first message after a start doesn't
    /// wait for it. For a reader in a window.
    pub fn prewarm(&self) {
        let engine = self.inner.engine.clone();
        let fonts = self.inner.theme.theme().webview_fonts();
        glib::idle_add_local_once(move || {
            glib::spawn_future_local(async move { engine.prewarm(&fonts).await });
        });
    }

    /// For `Window::set_reader`: the page with the link status strip over it.
    pub fn widget(&self) -> gtk::Widget {
        self.inner.links.root().clone().upcast()
    }

    /// For `Window::set_reader_toolbar`.
    pub fn toolbar(&self) -> gtk::Widget {
        self.inner.toolbar.widget().clone().upcast()
    }

    /// The Reader scope's keys on the reader and its toolbar.
    pub fn keys(&self) -> &ScopedShortcuts {
        &self.inner.keys
    }

    /// The order of a conversation's messages (`config.toml`'s `conversation_order`); the shown
    /// conversation is shown again in it.
    pub fn set_conversation_order(&self, order: ConversationOrder) {
        let newest_first = order == ConversationOrder::NewestFirst;
        if self.inner.newest_first.replace(newest_first) != newest_first {
            self.show_again();
        }
    }

    /// Whom Reply answers (`config.toml`'s `[reader] reply`, issue #8): the cards' first reply
    /// button and the quick reply follow it; the shown conversation is shown again.
    pub fn set_default_reply(&self, reply: DefaultReply) {
        let all = reply == DefaultReply::All;
        if self.inner.reply_all.replace(all) != all {
            self.show_again();
        }
    }

    /// The shown conversation, shown again from the store.
    fn show_again(&self) {
        let shown = self
            .inner
            .shown
            .borrow()
            .as_ref()
            .filter(|page| !page.missing && !self.inner.hidden.get())
            .map(|page| page.selected);
        if let Some(id) = shown {
            self.show(id);
        }
    }

    /// `keys.toml` changed: the Reader scope's keys and the toolbar's tooltips follow.
    pub fn update_keys(&self, keymap: &Keymap) {
        self.inner.keys.update(keymap);
        self.inner.toolbar.update_keys(keymap);
        self.inner.update_reply_keys(keymap);
    }

    /// Shows the conversation of the message `id`, scrolled to it. Returns at once; the
    /// conversation replaces the shown one when it is ready. A newer `show` or
    /// [`Reader::clear`] cancels it.
    pub fn show(&self, id: MessageId) {
        self.inner.target.set(Some(id));
        self.inner.answers.borrow_mut().remove(&id);
        let generation = self.inner.next_generation();
        let inner = self.inner.clone();
        glib::spawn_future_local(async move { inner.load(id, generation).await });
    }

    /// [`Reader::show`] in place of the shown message, the selection having moved on from it
    /// or the message having left the list: the left conversation stays on screen at most
    /// [`LEFT_SHOWN`], then the new one shows as far as it is laid out, or the left one is
    /// hidden until the new one is read.
    pub fn show_next(&self, id: MessageId) {
        let left = self
            .inner
            .shown
            .borrow()
            .as_ref()
            .filter(|page| page.selected != id)
            .map(|page| page.key);
        if let Some(key) = left
            && self
                .inner
                .left
                .get()
                .is_none_or(|(earlier, _)| earlier != key)
        {
            self.inner
                .left
                .set(Some((key, Instant::now() + LEFT_SHOWN)));
            let weak = Rc::downgrade(&self.inner);
            glib::timeout_add_local_once(LEFT_SHOWN, move || {
                if let Some(inner) = weak.upgrade() {
                    inner.left_due(key);
                }
            });
        }
        self.show(id);
    }

    /// Downloads the bodies the store doesn't hold through `fetch` (ML-044): the reader asks
    /// for the expanded messages' bodies, shows "Downloading…" meanwhile, and each message once
    /// [`Reader::store_changed`] or [`Reader::body_fetched`] says it is there. `fetch` returns
    /// `false` when nothing can download them.
    pub fn set_body_fetcher(&self, fetch: impl Fn(&[MessageId]) -> bool + 'static) {
        self.inner.fetcher.replace(Some(Rc::new(fetch)));
    }

    /// Asks `engine` for bodies and takes its answers.
    pub fn connect_engine(&self, engine: &EngineLink) {
        let link = engine.clone();
        self.set_body_fetcher(move |messages| {
            link.send(Command::FetchBodies {
                messages: messages.to_vec(),
            })
            .is_ok()
        });
        let weak = Rc::downgrade(&self.inner);
        let subscription = engine.subscribe(move |event| {
            if let Event::BodyFetched { message, result } = event
                && let Some(inner) = weak.upgrade()
            {
                Reader { inner }.body_fetched(*message, *result);
            }
        });
        self.inner.engine_events.replace(Some(subscription));
        files::connect_engine(self, engine);
    }

    /// The engine's answer to a download: a failure is shown with Retry, a success shows the
    /// message unless the store's change event already did.
    pub fn body_fetched(&self, id: MessageId, result: Result<(), BodyError>) {
        self.inner.fetching.borrow_mut().remove(&id);
        self.inner.body_answered(id, result);
    }

    /// A store commit (ADR-003's change events): the shown conversation is read again when the
    /// commit touched it (a reply arrived, flags changed, a message moved away or was expunged,
    /// two conversations merged), keeping its bodies and its scroll; a message waiting for its
    /// body, or whose download failed, loads again, a failure staying rather than asking again.
    pub fn store_changed(&self, event: &ChangeEvent) {
        self.inner.store_changed(event);
    }

    /// Messages a store commit changed (M3's hook): [`Reader::store_changed`] for those alone.
    pub fn messages_changed(&self, ids: &[MessageId]) {
        self.inner.store_changed(&ChangeEvent {
            changed: ids.to_vec(),
            ..ChangeEvent::default()
        });
    }

    /// Shows nothing and gives every view back.
    pub fn clear(&self) {
        self.inner.target.set(None);
        self.inner.next_generation();
        self.inner.discard_pending();
        self.inner.left.set(None);
        self.inner.hidden.set(false);
        self.inner.stage.show(None::<&gtk::Widget>, None);
        if let Some(page) = self.inner.shown.take() {
            self.inner.release_page(page);
        }
        self.inner.size_changed();
        self.inner.quick_changed();
    }

    /// The selected message: the one the reader was asked to show, once its conversation is on
    /// screen. Print, Show Original and Save as .eml act on it.
    pub fn message(&self) -> Option<MessageId> {
        if self.inner.hidden.get() {
            return None;
        }
        self.inner.shown.borrow().as_ref().map(|page| page.selected)
    }

    /// "n of N" in the toolbar, or nothing; previous and next follow it.
    pub fn set_position(&self, position: Option<Position>) {
        self.inner.toolbar.set_position(position);
    }

    /// A toolbar command's title for the selected messages and why it doesn't apply (M4).
    pub fn set_command(&self, id: ActionId, title: &str, reason: Option<&str>) {
        self.inner.toolbar.set_command(id, title, reason);
    }

    pub fn connect_previous(&self, callback: impl Fn() + 'static) {
        self.inner.toolbar.connect_previous(callback);
    }

    pub fn connect_next(&self, callback: impl Fn() + 'static) {
        self.inner.toolbar.connect_next(callback);
    }

    /// Shows the selected message's remote images (https only), in this reader until Sealwax
    /// quits; the same as its banner's "Show images".
    pub fn show_remote_images(&self) {
        if let Some(id) = self.message() {
            self.inner.allow_remote(id);
        }
    }

    /// Holds the layout of the conversations to come unfinished while `held`, as a slow machine
    /// would: they show only when the shown one may stay no longer. For the self-tests.
    pub fn hold_layout(&self, held: bool) {
        self.inner.layout_held.set(held);
        if !held {
            let _ = self.inner.progress.0.try_send(());
        }
    }

    /// Runs after every conversation shown.
    pub fn connect_shown(&self, listener: impl Fn(&Shown) + 'static) {
        self.inner.on_shown.borrow_mut().push(Rc::new(listener));
    }

    /// Runs when message bodies come on screen ([`Opened`]), for marking them read.
    pub fn connect_opened(&self, listener: impl Fn(&Opened) + 'static) {
        self.inner.on_opened.borrow_mut().push(Rc::new(listener));
    }

    /// Runs when the shown conversation's size changes (0 when nothing is shown), for Expand
    /// All and Collapse All.
    pub fn connect_size(&self, listener: impl Fn(usize) + 'static) {
        self.inner.on_size.borrow_mut().push(Rc::new(listener));
    }

    /// Every event of the views that show messages (links, refusals, resource loads): for the
    /// hostile suite and the self-test.
    pub fn connect_view_event(&self, listener: impl Fn(MessageId, &ViewEvent) + 'static) {
        self.inner.on_event.borrow_mut().push(Rc::new(listener));
    }

    /// Replaces how vetted links open (`gtk::UriLauncher` by default), for tests.
    pub fn set_link_opener(&self, opener: impl Fn(&gtk::Widget, &str) + 'static) {
        self.inner.links.set_opener(Rc::new(opener));
    }

    /// Replaces the link-mismatch check (ML-062): called before a vetted link opens; `false`
    /// keeps it closed. By default a link whose text names another domain than its target
    /// asks first, in a dialog with Cancel as the default.
    pub fn set_link_check(&self, check: impl Fn(&Link) -> bool + 'static) {
        self.inner.links.set_check(Some(Rc::new(check)));
    }

    /// The link status strip, the warning and the dialogs shown, for checks.
    pub fn link_state(&self) -> LinkState {
        self.inner.links.state()
    }

    /// The link warning dialog on screen, for checks that answer it.
    pub fn link_dialog(&self) -> Option<libadwaita::AlertDialog> {
        self.inner.links.dialog()
    }

    /// Where `cid:` parts the store doesn't hold come from (ML-060; phase B: `FetchPart`).
    pub fn set_part_loader(&self, loader: Option<PartLoader>) {
        self.inner.engine.set_part_loader(loader);
    }

    /// The WebView of the selected message's HTML body, while it holds one.
    pub fn web_view(&self) -> Option<webkit6::WebView> {
        self.view_of(self.message()?)
    }

    /// The WebView of message `id`'s HTML body in the shown conversation, while it holds one.
    pub fn view_of(&self, id: MessageId) -> Option<webkit6::WebView> {
        self.inner.shown.borrow().as_ref().and_then(|page| {
            page.item(id)
                .and_then(|item| item.card.as_ref())
                .and_then(|card| card.html.as_ref())
                .and_then(|html| html.view.as_ref())
                .map(|view| view.web_view().clone())
        })
    }

    /// Runs `body` (a function body) in Sealwax's isolated world of the selected message's
    /// HTML body.
    pub async fn eval(&self, body: &str) -> Result<webkit6::javascriptcore::Value, String> {
        let id = self.message().ok_or("no message is shown")?;
        self.eval_in(id, body).await
    }

    /// [`Reader::eval`] in message `id`'s HTML body.
    pub async fn eval_in(
        &self,
        id: MessageId,
        body: &str,
    ) -> Result<webkit6::javascriptcore::Value, String> {
        let view = self
            .inner
            .shown
            .borrow()
            .as_ref()
            .and_then(|page| page.item(id))
            .and_then(|item| item.card.as_ref())
            .and_then(|card| card.html.as_ref())
            .and_then(|html| html.view.clone())
            .ok_or("no HTML body with a view is shown for that message")?;
        view.eval(body).await.map_err(|error| error.to_string())
    }

    pub fn state(&self) -> ReaderState {
        let shown = self.inner.shown.borrow();
        let page = shown.as_ref().filter(|_| !self.inner.hidden.get());
        let card = page.and_then(Page::selected_card);
        let html = card.and_then(|card| card.html.as_ref());
        let mut state = ReaderState {
            message: page.map(|page| page.selected),
            body: page.map(|page| {
                if page.missing {
                    ShownBody::Missing
                } else {
                    card.map_or(ShownBody::Empty, |card| card.body.clone())
                }
            }),
            banner: card
                .and_then(|card| card.widgets.banner.as_ref())
                .is_some_and(|banner| banner.root.parent().is_some()),
            attachments: card.map_or(0, |card| card.attachments),
            view_size: html
                .and_then(|html| html.view.as_ref())
                .map(|view| (view.height(), view.web_view().width())),
            released: html.is_some_and(|html| html.released),
            live_views: self.inner.engine.live_views(),
            messages: page.map_or(0, Page::message_count),
            expanded: Vec::new(),
            collapsed: Vec::new(),
            folds: 0,
            folded: 0,
            html_bodies: 0,
            live_bodies: page.map(Page::live_bodies).unwrap_or_default(),
        };
        for entry in page.map_or(&[][..], |page| &page.entries[..]) {
            match entry {
                Entry::Message(item) if item.expanded => {
                    state.expanded.push(item.id());
                    if item.card.as_ref().is_some_and(|card| card.html.is_some()) {
                        state.html_bodies += 1;
                    }
                }
                Entry::Message(item) => state.collapsed.push(item.id()),
                Entry::Fold(fold) => {
                    state.folds += 1;
                    state.folded += fold.messages.len();
                }
            }
        }
        state
    }

    /// The shown conversation's expanded HTML bodies, where the column shows them.
    pub fn bodies(&self) -> Vec<BodyReport> {
        self.inner.body_reports()
    }

    /// Where message `id`'s row or card starts in the column, px from the top of what the
    /// column shows; `None` when it isn't in the shown conversation (or folded).
    pub fn message_top(&self, id: MessageId) -> Option<f64> {
        self.inner.message_top(id)
    }

    /// Every `cid:` request the process's views made so far.
    pub fn cid_requests(&self) -> Vec<CidRequest> {
        self.inner.engine.cid_requests()
    }

    /// Downloads refused so far.
    pub fn refused_downloads(&self) -> u32 {
        self.inner.engine.refused_downloads()
    }

    /// What the reader reads through, for the commands on the shown message (M5).
    pub fn source(&self) -> Rc<dyn MessageSource> {
        self.inner.store.clone()
    }

    /// Prints the message the reader shows (ML-063).
    pub fn print(&self) {
        export::print(self);
    }

    /// Shows the raw source of the message the reader shows (ML-063).
    pub fn show_original(&self) {
        export::show_original(self);
    }

    /// Saves the message the reader shows as a `.eml` file (ML-063).
    pub fn save_message(&self) {
        export::save_message(self);
    }

    /// Next Message in Conversation (`n`): the message after the one last moved to expands and
    /// the column scrolls to it.
    pub fn next_in_conversation(&self) {
        self.inner.step(true);
    }

    /// Previous Message in Conversation (`p`).
    pub fn previous_in_conversation(&self) {
        self.inner.step(false);
    }

    /// Expands every message of the shown conversation, folded ones excepted (ML-057).
    pub fn expand_all(&self) {
        self.inner.expand_all();
    }

    /// Collapses every message of the shown conversation but the selected one (ML-057).
    pub fn collapse_all(&self) {
        self.inner.collapse_all();
    }

    /// Expands message `id` of the shown conversation, as a click on its row does.
    pub fn expand(&self, id: MessageId) {
        self.inner.expand(id);
    }

    /// Collapses message `id` of the shown conversation, as a click on its header does; the
    /// only expanded message stays.
    pub fn collapse(&self, id: MessageId) {
        self.inner.collapse(id);
    }

    /// Reveals one step of the shown conversation's first "N earlier messages" row.
    pub fn unfold(&self) {
        self.inner.unfold_first();
    }

    /// Images from the shown message's sender load from now on (`true`) or are hidden again
    /// (`false`) (ML-058).
    pub fn set_always_show_images(&self, on: bool) {
        remote::set_always(self, on);
    }

    /// The shown conversation's bodies whose images the remote-content policy now allows load
    /// them (ML-058: after a sender was allowed).
    pub fn refresh_remote_images(&self) {
        self.inner.refresh_remote();
    }
}

/// Whether a link may open, and as what: absolute http(s) with a host, or mailto.
pub fn openable(uri: &str) -> Option<String> {
    let parsed = glib::Uri::parse(uri.trim(), glib::UriFlags::NONE).ok()?;
    match parsed.scheme().to_ascii_lowercase().as_str() {
        "http" | "https" if parsed.host().is_some_and(|host| !host.is_empty()) => {
            Some(parsed.to_str().to_string())
        }
        "mailto" => Some(parsed.to_str().to_string()),
        _ => None,
    }
}

/// Opens a vetted link in the default browser or mail handler. Never under Broadway: a
/// headless run must not reach the live desktop's browser (S2 once opened a real page).
fn launch(widget: &gtk::Widget, uri: &str) {
    let broadway = gdk::Display::default()
        .is_some_and(|display| display.type_().name() == "GdkBroadwayDisplay");
    if broadway {
        tracing::warn!("reader: links don't open from a headless (Broadway) run");
        return;
    }
    let window = widget.root().and_downcast::<gtk::Window>();
    gtk::UriLauncher::new(uri).launch(window.as_ref(), gio::Cancellable::NONE, |result| {
        if let Err(error) = result {
            tracing::warn!(%error, "reader: the link didn't open");
        }
    });
}

fn rgba(color: Rgb) -> gdk::RGBA {
    gdk::RGBA::new(
        f32::from(color.r) / 255.0,
        f32::from(color.g) / 255.0,
        f32::from(color.b) / 255.0,
        1.0,
    )
}

/// The body's user stylesheet (since M5 `Theme::webview_css` paints the themed root in the
/// theme's background itself).
fn stylesheet(theme: &Theme, style: BodyStyle) -> String {
    theme.webview_css(style)
}

fn now() -> i64 {
    glib::real_time() / 1_000_000
}

/// Why a body couldn't be downloaded, in plain words.
fn download_failure(error: BodyError) -> &'static str {
    match error {
        BodyError::Offline => {
            "This message couldn't be downloaded: the mail server can't be reached."
        }
        BodyError::NeedsSignIn => {
            "This message couldn't be downloaded: the account needs you to sign in again."
        }
        BodyError::Gone => "This message is no longer on the server.",
        BodyError::Store => "This message couldn't be saved on this computer.",
        BodyError::Other => "This message couldn't be downloaded.",
    }
}

impl Inner {
    fn next_generation(&self) -> u64 {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        generation
    }

    fn stale(&self, generation: u64) -> bool {
        self.generation.get() != generation
    }

    fn next_token(&self) -> u64 {
        let token = self.tokens.get() + 1;
        self.tokens.set(token);
        token
    }

    /// Runs `apply` on the page `key` names, shown or being laid out.
    fn with_page<R>(&self, key: u64, apply: impl FnOnce(&mut Page) -> R) -> Option<R> {
        let mut shown = self.shown.borrow_mut();
        if let Some(page) = shown.as_mut().filter(|page| page.key == key) {
            return Some(apply(page));
        }
        drop(shown);
        let mut pending = self.pending.borrow_mut();
        pending.as_mut().filter(|page| page.key == key).map(apply)
    }

    /// Whether page `key` is the one on screen.
    fn is_shown(&self, key: u64) -> bool {
        self.shown
            .borrow()
            .as_ref()
            .is_some_and(|page| page.key == key)
    }

    /// Gives every view of `page` back to the pool.
    fn release_page(&self, mut page: Page) {
        for item in page.items_mut() {
            if let Some(view) = item
                .card
                .as_mut()
                .and_then(|card| card.html.as_mut())
                .and_then(|html| html.view.take())
            {
                self.engine.release(view);
            }
        }
    }

    /// Drops the conversation being laid out, if any.
    fn discard_pending(&self) {
        if let Some(page) = self.pending.take() {
            if self
                .stage
                .staged()
                .is_some_and(|staged| staged == *page.root.upcast_ref::<gtk::Widget>())
            {
                self.stage.unstage();
            }
            self.release_page(page);
        }
    }

    fn size_changed(&self) {
        let size = self.shown.borrow().as_ref().map_or(0, Page::message_count);
        let listeners = self.on_size.borrow().clone();
        for listener in listeners {
            listener(size);
        }
    }

    fn opened(&self, opened: Opened) {
        if opened.messages.is_empty() {
            return;
        }
        let listeners = self.on_opened.borrow().clone();
        for listener in listeners {
            listener(&opened);
        }
    }

    fn view_event(self: &Rc<Self>, id: MessageId, event: &ViewEvent) {
        let listeners = self.on_event.borrow().clone();
        for listener in listeners {
            listener(id, event);
        }
    }

    /// The theme changed: every live body takes its new stylesheet, background and fonts in
    /// place. GTK restyles the rest itself.
    fn restyle(&self, theme: &Theme) {
        let fonts = theme.webview_fonts();
        let mut views = Vec::new();
        for pages in [&self.shown, &self.pending] {
            if let Some(page) = pages.borrow().as_ref() {
                for item in page.items() {
                    if let Some(html) = item.card.as_ref().and_then(|card| card.html.as_ref())
                        && let Some(view) = &html.view
                    {
                        views.push((view.clone(), html.style));
                    }
                }
            }
        }
        for (view, style) in views {
            view.set_fonts(&fonts);
            view.set_style(
                &stylesheet(theme, style),
                &rgba(theme.webview_background(style)),
            );
        }
    }

    /// WebKitGTK and the plain-text view keep Delete although a read-only message has no use
    /// for it, so with a body focused the Reader scope's Delete would never run (M4's keyboard
    /// pass): the scope sees the keys of [`KEPT_KEYS`] before the body does. A text field
    /// inside an HTML message loses them.
    fn take_kept_keys(inner: &Rc<Inner>) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(inner);
        keys.connect_key_pressed(move |controller, key, _, state| {
            let Some(inner) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let in_view = inner
                .stage
                .root()
                .and_then(|root| root.focus())
                .is_some_and(|focus| {
                    focus.is::<webkit6::WebView>()
                        || focus.ancestor(webkit6::WebView::static_type()).is_some()
                        || focus
                            .downcast_ref::<gtk::TextView>()
                            .is_some_and(|view| !view.is_editable())
                });
            let (Some(event), true) = (controller.current_event(), in_view) else {
                return glib::Propagation::Proceed;
            };
            if !KEPT_KEYS.contains(&key) || !state.is_empty() {
                return glib::Propagation::Proceed;
            }
            let model = inner.keys.model();
            let shortcut = (0..model.n_items())
                .filter_map(|index| model.item(index).and_downcast::<gtk::Shortcut>())
                .find(|shortcut| {
                    shortcut.trigger().is_some_and(|trigger| {
                        trigger.trigger(&event, false) == gdk::KeyMatch::Exact
                    })
                });
            let ran = shortcut
                .and_then(|shortcut| shortcut.action())
                .is_some_and(|action| {
                    action.activate(
                        gtk::ShortcutActionFlags::empty(),
                        &inner.stage,
                        None::<&glib::Variant>,
                    )
                });
            if ran {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        inner.stage.add_controller(keys);
    }

    /// Releases the views of a reader hidden for [`RELEASE_HIDDEN_AFTER`]; they come back when
    /// it is shown again. While it is shown, the column's scroll and layout drive the views.
    fn follow_visibility(inner: &Rc<Inner>) {
        let weak = Rc::downgrade(inner);
        inner.stage.connect_unmap(glib::clone!(
            #[strong]
            weak,
            move |_| {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                inner.unwatch_column();
                let timer_owner = weak.clone();
                let timer = glib::timeout_add_local_once(RELEASE_HIDDEN_AFTER, move || {
                    if let Some(inner) = timer_owner.upgrade() {
                        inner.hidden_timer.replace(None);
                        inner.release_hidden();
                    }
                });
                if let Some(old) = inner.hidden_timer.replace(Some(timer)) {
                    old.remove();
                }
            }
        ));
        inner.stage.connect_map(move |_| {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            if let Some(timer) = inner.hidden_timer.take() {
                timer.remove();
            }
            inner.follow_focus();
            inner.watch_column();
            inner.schedule();
            let engine = inner.engine.clone();
            let fonts = inner.theme.theme().webview_fonts();
            glib::idle_add_local_once(move || {
                glib::spawn_future_local(async move { engine.prewarm(&fonts).await });
            });
        });
    }

    /// Scrolling to the focus is the reader's own job: the column's GtkViewport would scroll a
    /// tall body to its end as soon as the body takes the focus (a click into its text, Tab).
    /// The reader scrolls to every other widget that takes the focus.
    fn follow_focus(&self) {
        let Some(viewport) = self
            .stage
            .ancestor(gtk::Viewport::static_type())
            .and_downcast::<gtk::Viewport>()
        else {
            return;
        };
        viewport.set_scroll_to_focus(false);
        let Some(window) = self.stage.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let followed = self
            .focus_window
            .borrow()
            .as_ref()
            .and_then(|(window, _)| window.upgrade())
            .is_some_and(|followed| followed == window);
        if followed {
            return;
        }
        let stage = self.stage.downgrade();
        let viewport = viewport.downgrade();
        let handler = window.connect_focus_widget_notify(move |window| {
            let (Some(stage), Some(viewport), Some(focus)) = (
                stage.upgrade(),
                viewport.upgrade(),
                GtkWindowExt::focus(window),
            ) else {
                return;
            };
            let body = focus.is::<webkit6::WebView>() || focus.is::<gtk::TextView>();
            if focus.is_ancestor(&stage) && !body {
                viewport.scroll_to(&focus, None);
            }
        });
        if let Some((old, handler)) = self
            .focus_window
            .replace(Some((window.downgrade(), handler)))
            && let Some(old) = old.upgrade()
        {
            old.disconnect(handler);
        }
    }

    /// The reader has been hidden for a while: every view goes back to the pool, the bodies
    /// keeping their heights.
    fn release_hidden(&self) {
        if self.stage.is_mapped() {
            return;
        }
        for pages in [&self.shown, &self.pending] {
            let mut released = Vec::new();
            if let Some(page) = pages.borrow_mut().as_mut() {
                widgets::hand_over_focus(&page.root, None);
                for item in page.items_mut() {
                    if let Some(html) = item.card.as_mut().and_then(|card| card.html.as_mut())
                        && let Some(view) = html.view.take()
                    {
                        html.holder.set_placeholder(
                            html.measured.map_or(view.height(), |(height, _)| height),
                        );
                        html.released = true;
                        released.push(view);
                    }
                }
            }
            if !released.is_empty() {
                tracing::debug!(
                    views = released.len(),
                    "reader: hidden, so its views went back to the pool"
                );
            }
            for view in released {
                self.engine.release(view);
            }
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(id) = self.listener.take() {
            self.theme.disconnect(id);
        }
        if let Some(timer) = self.hidden_timer.take() {
            timer.remove();
        }
        if let Some((window, handler)) = self.focus_window.take()
            && let Some(window) = window.upgrade()
        {
            window.disconnect(handler);
        }
        self.unwatch_column();
        if let Some(page) = self.pending.take() {
            self.release_page(page);
        }
        if let Some(page) = self.shown.take() {
            self.release_page(page);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_absolute_web_and_mail_links_open() {
        assert_eq!(
            openable("https://example.test/a?b=c#d").as_deref(),
            Some("https://example.test/a?b=c#d")
        );
        assert!(openable("HTTP://example.test/").is_some());
        assert!(openable("mailto:leah@example.test?subject=Hi").is_some());
        for refused in [
            "",
            "about:blank",
            "about:blank#top",
            "cid:logo@example.test",
            "data:text/html,<p>x",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ftp://example.test/",
            "rel-link",
            "//example.test/x",
            "https://",
            "sealwax:compose",
        ] {
            assert_eq!(openable(refused), None, "{refused}");
        }
    }

    #[test]
    fn colours_become_rgba() {
        let color = rgba(Rgb::from_hex(0xff8000));
        assert_eq!(
            (color.red(), color.green(), color.blue(), color.alpha()),
            (1.0, 128.0 / 255.0, 0.0, 1.0)
        );
    }
}
