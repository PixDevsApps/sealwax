//! The reader (ML-027; ADR-011 approach A; DESIGN.md "Reader" and "HTML mail inside the
//! theme"): one message at a time, its header, banner and attachment cards drawn by GTK, a
//! plain-text body in a text view, an HTML body in a pooled, hardened WebView (see [`view`])
//! styled from the live theme. Everything is read from the store off the GTK thread; the
//! message appears once its body has its height. Conversations are M5.
//!
//! The lead wires it: `Reader::new` with the store and the theme, `widget()` into
//! `Window::set_reader`, `toolbar()` into `Window::set_reader_toolbar`, the list's selection
//! into [`Reader::show`], and the list's position into [`Reader::set_position`] with
//! [`Reader::connect_previous`] and [`Reader::connect_next`].

mod engine;
mod format;
mod heuristic;
mod page;
mod prepare;
pub mod samples;
mod toolbar;
mod view;
mod widgets;

pub use engine::{CONTENT_SECURITY_POLICY, CidRequest, MAX_LIVE_VIEWS, NETWORK_FEATURES};
pub use toolbar::Position;
pub use view::{Link, MAX_VIEW_HEIGHT, Refused, ViewEvent, WORLD, disabled_features};

use crate::actions::{self, ScopedShortcuts};
use crate::theme::{ListenerId, ThemeController};
use engine::{Engine, InlineParts};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use page::PageWidgets;
use prepare::{Body, Prepared};
use sealwax_domain::actions::Scope;
use sealwax_domain::keymap::Keymap;
use sealwax_domain::model::{Account, MessageDetail, MessageId};
use sealwax_domain::theme::{BodyStyle, Rgb, Theme};
use sealwax_infrastructure::store::{Reply, Store, Versioned};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};
use toolbar::Toolbar;
use view::MessageView;
use widgets::{BodyBox, Stage};

/// How long a new message may take to lay out before it is shown anyway.
const READY_TIMEOUT: Duration = Duration::from_secs(3);
/// A reader hidden this long gives its view back (ADR-011: views of hidden bodies are released
/// and re-rendered on return).
const RELEASE_HIDDEN_AFTER: Duration = Duration::from_secs(3);

/// What [`Reader::show`] showed, for listeners such as the self-test and measurements.
#[derive(Debug, Clone, PartialEq)]
pub struct Shown {
    pub id: MessageId,
    pub body: ShownBody,
    /// From `show` to the stored message and its raw bytes.
    pub fetched: Duration,
    /// To the body prepared off the GTK thread.
    pub prepared: Duration,
    /// To the page on screen (an HTML body laid out at its width, its images in).
    pub ready: Duration,
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
    /// HTML shown as text because no WebView could show it safely (no content filter, no
    /// free view, or the web process ended).
    TextFallback,
    Empty,
    NotDownloaded,
    /// The store no longer has the message, or couldn't read it.
    Missing,
}

/// The reader's state, for checks.
#[derive(Debug, Clone, PartialEq)]
pub struct ReaderState {
    pub message: Option<MessageId>,
    pub body: Option<ShownBody>,
    pub banner: bool,
    pub attachments: usize,
    /// The HTML view's height (px) and its allocated width, when an HTML body is shown.
    pub view_size: Option<(i32, i32)>,
    /// The shown message's view went back to the pool while the reader was hidden.
    pub released: bool,
    pub live_views: usize,
}

type ShownListener = Rc<dyn Fn(&Shown)>;
type EventListener = Rc<dyn Fn(MessageId, &ViewEvent)>;

/// Where the reader sends its requests: the app's mailbox, whose store opens at startup and
/// closes at shutdown, or a store of its own (examples and self-tests).
pub trait MessageSource {
    /// The reads one message needs, sent together; `None` while no store is open.
    fn read(&self, id: MessageId) -> Option<Reads>;
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
}

impl MessageSource for crate::mailbox::Mailbox {
    fn read(&self, id: MessageId) -> Option<Reads> {
        self.store()
            .and_then(|store| MessageSource::read(&*store, id))
    }
}
type Opener = Rc<dyn Fn(&gtk::Widget, &str)>;
type LinkCheck = Rc<dyn Fn(&Link) -> bool>;

#[derive(Clone)]
pub struct Reader {
    inner: Rc<Inner>,
}

struct Html {
    view: MessageView,
    style: BodyStyle,
    html: Rc<str>,
    parts: Rc<InlineParts>,
    remote_images: bool,
    remote_allowed: bool,
}

struct Page {
    id: MessageId,
    widgets: PageWidgets,
    html: Option<Html>,
    body: ShownBody,
    attachments: usize,
    /// The view went back to the pool while the reader was hidden.
    released: bool,
}

impl Page {
    fn focus_target(&self) -> Option<gtk::Widget> {
        match &self.html {
            Some(html) => Some(html.view.web_view().clone().upcast()),
            None => self.widgets.focus.clone(),
        }
    }
}

struct Inner {
    store: Rc<dyn MessageSource>,
    theme: Rc<ThemeController>,
    engine: Rc<Engine>,
    stage: Stage,
    toolbar: Rc<Toolbar>,
    keys: ScopedShortcuts,
    shown: RefCell<Option<Page>>,
    /// The view of the page being laid out, for restyling; its load owns the page.
    staged: RefCell<Option<(MessageView, BodyStyle)>>,
    generation: Cell<u64>,
    /// Messages whose remote images the user allowed this session (nothing is stored).
    allowed: RefCell<HashSet<MessageId>>,
    /// Messages whose view's web process ended; a second time, they are shown as text.
    crashed: RefCell<HashMap<MessageId, u32>>,
    listener: Cell<Option<ListenerId>>,
    opener: RefCell<Opener>,
    link_check: RefCell<Option<LinkCheck>>,
    on_shown: RefCell<Vec<ShownListener>>,
    on_event: RefCell<Vec<EventListener>>,
    hidden_timer: RefCell<Option<glib::SourceId>>,
    /// The window whose focus the reader follows, and the handler.
    focus_window: RefCell<Option<(glib::WeakRef<gtk::Window>, glib::SignalHandlerId)>>,
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
        let inner = Rc::new(Inner {
            store,
            theme,
            engine: Engine::shared(cache),
            stage,
            toolbar,
            keys,
            shown: RefCell::new(None),
            staged: RefCell::new(None),
            generation: Cell::new(0),
            allowed: RefCell::new(HashSet::new()),
            crashed: RefCell::new(HashMap::new()),
            listener: Cell::new(None),
            opener: RefCell::new(Rc::new(launch)),
            link_check: RefCell::new(None),
            on_shown: RefCell::new(Vec::new()),
            on_event: RefCell::new(Vec::new()),
            hidden_timer: RefCell::new(None),
            focus_window: RefCell::new(None),
        });
        let weak = Rc::downgrade(&inner);
        let id = inner.theme.connect_changed(move |theme| {
            if let Some(inner) = weak.upgrade() {
                inner.restyle(theme);
            }
        });
        inner.listener.set(Some(id));
        Inner::follow_visibility(&inner);
        Reader { inner }
    }

    /// For `Window::set_reader`.
    pub fn widget(&self) -> gtk::Widget {
        self.inner.stage.clone().upcast()
    }

    /// For `Window::set_reader_toolbar`.
    pub fn toolbar(&self) -> gtk::Widget {
        self.inner.toolbar.widget().clone().upcast()
    }

    /// The Reader scope's keys on the reader and its toolbar.
    pub fn keys(&self) -> &ScopedShortcuts {
        &self.inner.keys
    }

    /// `keys.toml` changed: the Reader scope's keys and the toolbar's tooltips follow.
    pub fn update_keys(&self, keymap: &Keymap) {
        self.inner.keys.update(keymap);
        self.inner.toolbar.update_keys(keymap);
    }

    /// Shows the message `id`. Returns at once; the page replaces the shown one when it is
    /// ready. A newer `show` or [`Reader::clear`] cancels it.
    pub fn show(&self, id: MessageId) {
        let generation = self.inner.next_generation();
        let inner = self.inner.clone();
        glib::spawn_future_local(async move { inner.load(id, generation, Instant::now()).await });
    }

    /// Shows nothing and gives every view back.
    pub fn clear(&self) {
        self.inner.next_generation();
        self.inner.stage.unstage();
        self.inner.stage.show(None::<&gtk::Widget>, None);
        if let Some(page) = self.inner.shown.take() {
            self.inner.release_page(page);
        }
    }

    /// The message shown, if any.
    pub fn message(&self) -> Option<MessageId> {
        self.inner.shown.borrow().as_ref().map(|page| page.id)
    }

    /// "n of N" in the toolbar, or nothing; previous and next follow it.
    pub fn set_position(&self, position: Option<Position>) {
        self.inner.toolbar.set_position(position);
    }

    pub fn connect_previous(&self, callback: impl Fn() + 'static) {
        self.inner.toolbar.connect_previous(callback);
    }

    pub fn connect_next(&self, callback: impl Fn() + 'static) {
        self.inner.toolbar.connect_next(callback);
    }

    /// Shows the shown message's remote images (https only), in this reader until Sealwax
    /// quits; the same as its banner's "Show images".
    pub fn show_remote_images(&self) {
        if let Some(id) = self.message() {
            self.inner.allow_remote(id);
        }
    }

    /// Runs after every message shown.
    pub fn connect_shown(&self, listener: impl Fn(&Shown) + 'static) {
        self.inner.on_shown.borrow_mut().push(Rc::new(listener));
    }

    /// Every event of the views that show messages (links, refusals, resource loads): for the
    /// hostile suite and the self-test.
    pub fn connect_view_event(&self, listener: impl Fn(MessageId, &ViewEvent) + 'static) {
        self.inner.on_event.borrow_mut().push(Rc::new(listener));
    }

    /// Replaces how vetted links open (`gtk::UriLauncher` by default), for tests.
    pub fn set_link_opener(&self, opener: impl Fn(&gtk::Widget, &str) + 'static) {
        self.inner.opener.replace(Rc::new(opener));
    }

    /// The link-mismatch check's place (M5): called before a vetted link opens; `false` keeps
    /// it closed.
    pub fn set_link_check(&self, check: impl Fn(&Link) -> bool + 'static) {
        self.inner.link_check.replace(Some(Rc::new(check)));
    }

    /// The WebView of the shown HTML body, for checks.
    pub fn web_view(&self) -> Option<webkit6::WebView> {
        self.inner
            .shown
            .borrow()
            .as_ref()
            .and_then(|page| page.html.as_ref())
            .map(|html| html.view.web_view().clone())
    }

    /// Runs `body` (a function body) in Sealwax's isolated world of the shown HTML body.
    pub async fn eval(&self, body: &str) -> Result<webkit6::javascriptcore::Value, String> {
        let view = self
            .inner
            .shown
            .borrow()
            .as_ref()
            .and_then(|page| page.html.as_ref())
            .map(|html| html.view.clone())
            .ok_or("no HTML body is shown")?;
        view.eval(body).await.map_err(|error| error.to_string())
    }

    pub fn state(&self) -> ReaderState {
        let shown = self.inner.shown.borrow();
        ReaderState {
            message: shown.as_ref().map(|page| page.id),
            body: shown.as_ref().map(|page| page.body.clone()),
            banner: shown
                .as_ref()
                .and_then(|page| page.widgets.banner.as_ref())
                .is_some_and(|banner| banner.root.parent().is_some()),
            attachments: shown.as_ref().map_or(0, |page| page.attachments),
            view_size: shown
                .as_ref()
                .and_then(|page| page.html.as_ref())
                .map(|html| (html.view.height(), html.view.web_view().width())),
            released: shown.as_ref().is_some_and(|page| page.released),
            live_views: self.inner.engine.live_views(),
        }
    }

    /// Every `cid:` request the process's views made so far.
    pub fn cid_requests(&self) -> Vec<CidRequest> {
        self.inner.engine.cid_requests()
    }

    /// Downloads refused so far.
    pub fn refused_downloads(&self) -> u32 {
        self.inner.engine.refused_downloads()
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

/// The body's user stylesheet: the theme's, with the themed root painted in the theme's
/// background. With `color-scheme: dark` and a transparent root, WebKit paints its own dark
/// canvas (#1e1e1e) rather than the view's background colour.
fn stylesheet(theme: &Theme, style: BodyStyle) -> String {
    let mut css = theme.webview_css(style);
    if style == BodyStyle::Themed {
        css.push_str(&format!(
            "html{{background-color:{}}}\n",
            theme.webview_background(style)
        ));
    }
    css
}

fn now() -> i64 {
    glib::real_time() / 1_000_000
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

    async fn load(self: Rc<Self>, id: MessageId, generation: u64, started: Instant) {
        let Some(reads) = self.store.read(id) else {
            self.show_missing(id, started);
            return;
        };
        let detail = reads.detail.recv().await;
        let accounts = reads.accounts.recv().await;
        let raw = reads.raw.recv().await.ok().flatten();
        if self.stale(generation) {
            return;
        }
        let detail = match detail {
            Ok(versioned) => versioned.value,
            Err(error) => {
                tracing::warn!(message = id.0, %error, "reader: the message couldn't be read");
                None
            }
        };
        let Some(detail) = detail else {
            self.show_missing(id, started);
            return;
        };
        let mine: Vec<String> = accounts
            .map(|accounts| {
                accounts
                    .value
                    .into_iter()
                    .map(|account| account.address)
                    .collect()
            })
            .unwrap_or_default();
        let fetched = started.elapsed();
        let for_worker = detail.clone();
        let prepared = gio::spawn_blocking(move || prepare::prepare(&for_worker, raw.as_deref()))
            .await
            .unwrap_or_else(|_| {
                tracing::error!(message = id.0, "reader: preparing the message failed");
                Prepared {
                    body: Body::Empty,
                    attachments: Vec::new(),
                }
            });
        if self.stale(generation) {
            return;
        }
        let prepared_at = started.elapsed();
        let header = page::Header {
            detail: &detail,
            mine: &mine,
            now: now(),
        };
        let remote_images = matches!(
            &prepared.body,
            Body::Html {
                remote_images: true,
                ..
            }
        ) && !self.allowed.borrow().contains(&id);
        let widgets = page::build(&header, &prepared.attachments, remote_images);
        let attachments = prepared.attachments.len();
        let mut page = Page {
            id,
            widgets,
            html: None,
            body: ShownBody::Empty,
            attachments,
            released: false,
        };
        match prepared.body {
            Body::Html {
                html,
                style,
                remote_images,
                parts,
            } => {
                let html: Rc<str> = Rc::from(html);
                let parts = Rc::new(parts);
                let crashes = self.crashed.borrow().get(&id).copied().unwrap_or(0);
                if crashes >= 2
                    || !self
                        .clone()
                        .stage_html(
                            &mut page,
                            html.clone(),
                            style,
                            remote_images,
                            parts,
                            generation,
                        )
                        .await
                {
                    if self.stale(generation) {
                        return;
                    }
                    self.text_fallback(&mut page, html).await;
                    if self.stale(generation) {
                        return;
                    }
                }
            }
            Body::Plain(text) => {
                let text = page::plain_text(&text);
                page.widgets.body.append(&text);
                page.widgets.focus = Some(text.upcast());
                page.body = ShownBody::Plain;
            }
            Body::Empty => {
                page.widgets
                    .body
                    .append(&page::notice("This message has no text."));
                page.body = ShownBody::Empty;
            }
            Body::NotDownloaded => {
                let snippet = &detail.summary.snippet;
                page.widgets.body.append(&page::notice(
                    "The rest of this message hasn't been downloaded yet.",
                ));
                if !snippet.is_empty() {
                    let snippet = page::plain_text(snippet);
                    page.widgets.body.append(&snippet);
                    page.widgets.focus = Some(snippet.upcast());
                }
                page.body = ShownBody::NotDownloaded;
            }
        }
        self.present(page, started, fetched, prepared_at);
    }

    /// Lays an HTML body out unseen in a pooled view until it has its width, its height and
    /// its images. `false` when no view can show it safely; the page then gets the text.
    async fn stage_html(
        self: Rc<Self>,
        page: &mut Page,
        html: Rc<str>,
        style: BodyStyle,
        remote_images: bool,
        parts: Rc<InlineParts>,
        generation: u64,
    ) -> bool {
        let Some(filter) = self.engine.filter().await else {
            return false;
        };
        if self.stale(generation) {
            return false;
        }
        let theme = self.theme.theme();
        let Some(view) = self.engine.acquire(&theme.webview_fonts()) else {
            return false;
        };
        let frame = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .overflow(gtk::Overflow::Hidden)
            .css_classes([match style {
                BodyStyle::Paper => "sw-paper",
                BodyStyle::Themed => "sw-themed-body",
            }])
            .build();
        frame.set_widget_name("reader-frame");
        frame.append(&BodyBox::new(view.web_view()));
        page.widgets.body.append(&frame);
        let (events, received) = async_channel::unbounded::<ViewEvent>();
        let weak = Rc::downgrade(&self);
        let id = page.id;
        view.set_handler(move |event| {
            let _ = events.try_send(event.clone());
            if let Some(inner) = weak.upgrade() {
                inner.view_event(id, event);
            }
        });
        let remote_allowed = self.allowed.borrow().contains(&page.id);
        self.engine.set_parts(view.page_id(), parts.clone());
        self.stage.stage(&page.widgets.root);
        self.staged.replace(Some((view.clone(), style)));
        view.load(
            &html,
            &stylesheet(&theme, style),
            &rgba(theme.webview_background(style)),
            (!remote_allowed).then_some(&filter),
        );
        if let Some(banner) = &page.widgets.banner {
            let weak = Rc::downgrade(&self);
            banner.show_images.connect_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.allow_remote(id);
                }
            });
        }
        page.html = Some(Html {
            view: view.clone(),
            style,
            html,
            parts: parts.clone(),
            remote_images,
            remote_allowed,
        });
        page.body = ShownBody::Html {
            style,
            remote_images,
            remote_allowed,
            inline_parts: parts.len(),
        };
        let ready = self.wait_until_laid_out(&view, &received, generation).await;
        let ours = self
            .staged
            .borrow()
            .as_ref()
            .is_some_and(|(staged, _)| staged.page_id() == view.page_id());
        if ours {
            self.staged.replace(None);
        }
        let stale = self.stale(generation);
        if stale || !ready {
            if ours {
                self.stage.unstage();
            }
            if let Some(html) = page.html.take() {
                self.engine.release(html.view);
            }
            page.widgets.body.remove(&frame);
            return false;
        }
        true
    }

    /// Waits for the body's `load` event and a height measured at the view's allocated width.
    /// `false` when the load failed or the web process ended.
    async fn wait_until_laid_out(
        &self,
        view: &MessageView,
        events: &async_channel::Receiver<ViewEvent>,
        generation: u64,
    ) -> bool {
        let deadline = Instant::now() + READY_TIMEOUT;
        let (mut loaded, mut sized) = (false, false);
        while !(loaded && sized) {
            let left = deadline.saturating_duration_since(Instant::now());
            if !self.stage.is_mapped() && loaded {
                break;
            }
            match glib::future_with_timeout(left, events.recv()).await {
                Ok(Ok(ViewEvent::Height { width, .. })) => {
                    let allocated = view.web_view().width();
                    sized = width > 1 && (width - allocated).abs() <= 1;
                }
                Ok(Ok(ViewEvent::Loaded)) => loaded = true,
                Ok(Ok(ViewEvent::Failed | ViewEvent::Crashed)) => return false,
                Ok(Ok(_)) => {}
                Ok(Err(_)) => return false,
                Err(_) => {
                    tracing::warn!(
                        loaded,
                        sized,
                        "reader: the body took over {} s to lay out; shown as it is",
                        READY_TIMEOUT.as_secs()
                    );
                    break;
                }
            }
            if self.stale(generation) {
                return true;
            }
        }
        true
    }

    /// The HTML as text, when no WebView may show it.
    async fn text_fallback(&self, page: &mut Page, html: Rc<str>) {
        let html = html.to_string();
        let text = gio::spawn_blocking(move || sealwax_domain::message::html_to_text(&html))
            .await
            .unwrap_or_default();
        page.widgets.body.append(&page::notice(
            "This message is shown as text: its formatted view isn't available.",
        ));
        let text = page::plain_text(&text);
        page.widgets.body.append(&text);
        page.widgets.focus = Some(text.upcast());
        page.body = ShownBody::TextFallback;
    }

    fn show_missing(&self, id: MessageId, started: Instant) {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        root.append(&page::notice("This message is no longer here."));
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let page = Page {
            id,
            widgets: PageWidgets {
                root,
                body,
                banner: None,
                focus: None,
            },
            html: None,
            body: ShownBody::Missing,
            attachments: 0,
            released: false,
        };
        let elapsed = started.elapsed();
        self.present(page, started, elapsed, elapsed);
    }

    /// Puts `page` on screen in place of the shown one, which gives its view back. A focus in
    /// the shown page moves to the new one's body.
    fn present(&self, page: Page, started: Instant, fetched: Duration, prepared: Duration) {
        let focus = page.focus_target();
        if page.html.is_some() {
            self.stage.promote(focus.as_ref());
        } else {
            self.stage.unstage();
            self.stage.show(Some(&page.widgets.root), focus.as_ref());
        }
        let old = self.shown.replace(None);
        if old.as_ref().is_none_or(|old| old.id != page.id)
            && let Some(column) = self
                .stage
                .ancestor(gtk::ScrolledWindow::static_type())
                .and_downcast::<gtk::ScrolledWindow>()
        {
            column.vadjustment().set_value(0.0);
        }
        if let Some(old) = old {
            self.release_page(old);
        }
        let shown = Shown {
            id: page.id,
            body: page.body.clone(),
            fetched,
            prepared,
            ready: started.elapsed(),
        };
        tracing::debug!(
            message = page.id.0,
            body = ?shown.body,
            ms = shown.ready.as_secs_f64() * 1000.0,
            "reader: shown"
        );
        self.shown.replace(Some(page));
        let listeners = self.on_shown.borrow().clone();
        for listener in listeners {
            listener(&shown);
        }
    }

    fn release_page(&self, mut page: Page) {
        if let Some(html) = page.html.take() {
            self.engine.release(html.view);
        }
    }

    fn view_event(self: &Rc<Self>, id: MessageId, event: &ViewEvent) {
        match event {
            ViewEvent::Link(link) => self.open_link(link),
            ViewEvent::Refused(refused) => {
                tracing::debug!(?refused, "reader: refused what a message asked for");
            }
            ViewEvent::Crashed
                if self
                    .shown
                    .borrow()
                    .as_ref()
                    .is_some_and(|page| page.id == id && page.html.is_some()) =>
            {
                *self.crashed.borrow_mut().entry(id).or_default() += 1;
                self.reshow(id);
            }
            _ => {}
        }
        let listeners = self.on_event.borrow().clone();
        for listener in listeners {
            listener(id, event);
        }
    }

    /// Shows `id` again, as after the web process ended or the reader was hidden.
    fn reshow(self: &Rc<Self>, id: MessageId) {
        let generation = self.next_generation();
        let inner = self.clone();
        glib::spawn_future_local(async move {
            inner.load(id, generation, Instant::now()).await;
        });
    }

    fn open_link(&self, link: &Link) {
        let Some(uri) = openable(&link.uri) else {
            tracing::debug!("reader: a link that isn't http(s) or mailto stays closed");
            return;
        };
        let check = self.link_check.borrow().clone();
        if let Some(check) = check
            && !check(link)
        {
            return;
        }
        let opener = self.opener.borrow().clone();
        opener(self.stage.upcast_ref(), &uri);
    }

    fn allow_remote(self: &Rc<Self>, id: MessageId) {
        self.allowed.borrow_mut().insert(id);
        let theme = self.theme.theme();
        let mut shown = self.shown.borrow_mut();
        let Some(page) = shown.as_mut().filter(|page| page.id == id) else {
            return;
        };
        if let Some(banner) = &page.widgets.banner
            && banner.root.parent().is_some()
        {
            widgets::hand_over_focus(&banner.root, page.focus_target().as_ref());
            page.widgets.root.remove(&banner.root);
        }
        let Some(html) = page.html.as_mut() else {
            return;
        };
        if html.remote_allowed {
            return;
        }
        html.remote_allowed = true;
        self.engine
            .set_parts(html.view.page_id(), html.parts.clone());
        html.view.load(
            &html.html,
            &stylesheet(&theme, html.style),
            &rgba(theme.webview_background(html.style)),
            None,
        );
        page.body = ShownBody::Html {
            style: html.style,
            remote_images: html.remote_images,
            remote_allowed: true,
            inline_parts: html.parts.len(),
        };
        tracing::debug!(
            message = id.0,
            "reader: remote images allowed for a message"
        );
    }

    /// The theme changed: every live body takes its new stylesheet, background and fonts in
    /// place. GTK restyles the rest itself.
    fn restyle(&self, theme: &Theme) {
        let fonts = theme.webview_fonts();
        let shown = self
            .shown
            .borrow()
            .as_ref()
            .and_then(|page| page.html.as_ref())
            .map(|html| (html.view.clone(), html.style));
        let staged = self.staged.borrow().clone();
        for (view, style) in [shown, staged].into_iter().flatten() {
            view.set_fonts(&fonts);
            view.set_style(
                &stylesheet(theme, style),
                &rgba(theme.webview_background(style)),
            );
        }
    }

    /// Releases the view of a reader hidden for [`RELEASE_HIDDEN_AFTER`], and shows its
    /// message again when it comes back.
    fn follow_visibility(inner: &Rc<Inner>) {
        let weak = Rc::downgrade(inner);
        inner.stage.connect_unmap(glib::clone!(
            #[strong]
            weak,
            move |_| {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
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
            let released = inner
                .shown
                .borrow()
                .as_ref()
                .filter(|page| page.released)
                .map(|page| page.id);
            if let Some(id) = released {
                inner.reshow(id);
            }
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

    fn release_hidden(&self) {
        if self.stage.is_mapped() {
            return;
        }
        self.next_generation();
        self.stage.unstage();
        let mut shown = self.shown.borrow_mut();
        if let Some(page) = shown.as_mut()
            && let Some(html) = page.html.take()
        {
            widgets::hand_over_focus(&page.widgets.root, None);
            self.engine.release(html.view);
            page.released = true;
            tracing::debug!("reader: hidden, so its view went back to the pool");
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
