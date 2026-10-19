//! The composer's body (ADR-014 with its S7 amendment; SECURITY.md "Composer"): S7's editable
//! WebView with every layer the reader has. No script markup runs; the CSP is set at
//! construction and admits only `data:` and `cid:` images; the content filter blocks every load
//! but `cid:`, `data:`, `about:` and `blob:`; the network session is ephemeral; every
//! navigation and new window is refused; permissions, dialogs, file and colour choosers,
//! printing and downloads are refused; the context menu keeps editing items only. Rendering is
//! on the CPU (`hardware-acceleration-policy` NEVER, S7's latency result). The isolated-world
//! helper (`helper.js`) cleans pastes and drops with S7's allowlist, turns pasted or dropped
//! images into parts of the draft, and serialises the body with `cid:` references.
//!
//! Composer views share one web context and web process of their own, apart from the reader's.
//! Inline images are served to the view that holds them through its `cid:` scheme, read from
//! the blob store off the GTK thread; nothing is ever fetched from the network.

use super::js;
use crate::reader::NETWORK_FEATURES;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use sealwax_domain::model::BlobKey;
use sealwax_domain::theme::{Mode, Theme, css_family};
use sealwax_infrastructure::blobs::BlobStore;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use webkit6::javascriptcore::Value;
use webkit6::prelude::*;
use webkit6::{ContextMenuAction, EditorTypingAttributes, PolicyDecisionType};

/// The isolated world the helper runs in; the body's content can't see it.
pub const WORLD: &str = "sealwax-composer";
const HANDLER: &str = "composer";

/// S7's composer policy: images only from `data:` (pasted, not yet adopted) and `cid:` (the
/// draft's parts); never `https:`, unlike the reader's.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; img-src data: cid:; \
     style-src 'unsafe-inline'; font-src data:; form-action 'none'; base-uri 'none'; \
     frame-src 'none'";

/// Blocks every load but the draft's own parts, `data:`, `about:` and the `blob:` images
/// WebKit's native image paste makes (which the helper reads into parts).
const FILTER_JSON: &str = r#"[
  {"trigger": {"url-filter": ".*"}, "action": {"type": "block"}},
  {"trigger": {"url-filter": "^cid:"}, "action": {"type": "ignore-previous-rules"}},
  {"trigger": {"url-filter": "^data:"}, "action": {"type": "ignore-previous-rules"}},
  {"trigger": {"url-filter": "^about:"}, "action": {"type": "ignore-previous-rules"}},
  {"trigger": {"url-filter": "^blob:"}, "action": {"type": "ignore-previous-rules"}}
]"#;
const FILTER_ID: &str = "sealwax-composer-block-remote";

const HELPER: &str = include_str!("helper.js");
const EDITOR_HTML: &str = include_str!("editor.html");

/// The body's line height (the approved mock's composer: 15 px at 1.6).
pub const LINE_HEIGHT: f64 = 1.6;

/// An inline image of the draft: its Content-ID (without angle brackets) maps to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineImage {
    pub mime_type: String,
    pub blob: BlobKey,
    pub size: u64,
    pub name: String,
}

/// The cursor's block and inline state, from the helper (lists, quotes, links, the paragraph
/// style) and WebKit's typing attributes (bold, italic, underline, strikethrough).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockState {
    pub bulleted: bool,
    pub numbered: bool,
    pub quote: bool,
    pub indent: bool,
    pub link: String,
    /// `p`, `h1`, `h2`, `h3` or `pre`.
    pub block: String,
    pub selection: bool,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

/// What the cleaner removed from a paste or a drop.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CleanStats {
    pub dropped: Vec<(String, u32)>,
    pub handlers: u32,
    pub styles: u32,
    pub remote: u32,
    pub links: u32,
}

impl CleanStats {
    fn from_value(value: &Value) -> Self {
        let dropped = js::get(value, "dropped")
            .map(|map| {
                js::keys(&map)
                    .into_iter()
                    .map(|key| {
                        let count = js::number(&map, &key) as u32;
                        (key, count)
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            dropped,
            handlers: js::number(value, "handlers") as u32,
            styles: js::number(value, "styles") as u32,
            remote: js::number(value, "remote") as u32,
            links: js::number(value, "links") as u32,
        }
    }
}

#[derive(Clone, Debug)]
pub enum EditorEvent {
    /// The editor page is loaded and the helper runs.
    Ready,
    /// The user changed the body.
    Input { composing: bool },
    /// The cursor's state changed.
    State(BlockState),
    /// Undo and Redo availability.
    History { undo: bool, redo: bool },
    /// A pasted or dropped image to store as a part (base64 data); answered with
    /// [`Editor::adopt`].
    Image {
        token: String,
        mime_type: String,
        data: String,
        name: String,
    },
    /// A file dropped on the body that isn't an image: an attachment (base64 data).
    File {
        name: String,
        mime_type: String,
        data: String,
    },
    /// A paste or a drop: how it was taken (`html`, `text`, `files`, `native`) and what the
    /// cleaner removed.
    Transfer {
        kind: String,
        mode: String,
        stats: Option<CleanStats>,
    },
    /// The helper refused something; for the log, never with content.
    Problem(String),
    /// The web process ended.
    Crashed,
}

/// The body serialised for the draft: cleaned HTML whose images are `cid:` references, and
/// the Content-IDs it refers to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Serialized {
    pub html: String,
    pub cids: Vec<String>,
}

type Parts = Rc<RefCell<HashMap<String, InlineImage>>>;

/// What each composer view may load through `cid:`, by its page id.
#[derive(Default)]
struct Pages {
    parts: HashMap<u64, Parts>,
    served: u64,
    refused: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Filter {
    Compiling,
    Ready,
    Failed,
}

/// The composer views' web context, network session, content filter and `cid:` scheme.
pub struct EditorEngine {
    context: webkit6::WebContext,
    session: webkit6::NetworkSession,
    filter: RefCell<Option<webkit6::UserContentFilter>>,
    filter_state: Rc<RefCell<Filter>>,
    filter_done: async_channel::Receiver<()>,
    pages: Rc<RefCell<Pages>>,
    blobs: Rc<RefCell<Option<BlobStore>>>,
    spell: RefCell<Vec<String>>,
}

thread_local! {
    static ENGINE: OnceCell<Rc<EditorEngine>> = const { OnceCell::new() };
}

impl EditorEngine {
    /// The process's composer engine, created on first use; the filter compiles into
    /// `cache/content-filters` in the background.
    pub fn shared(cache: &Path) -> Rc<EditorEngine> {
        ENGINE.with(|engine| engine.get_or_init(|| EditorEngine::new(cache)).clone())
    }

    fn new(cache: &Path) -> Rc<EditorEngine> {
        let context = webkit6::WebContext::new();
        context.set_cache_model(webkit6::CacheModel::DocumentViewer);
        let session = webkit6::NetworkSession::new_ephemeral();
        session.set_tls_errors_policy(webkit6::TLSErrorsPolicy::Fail);
        if let Some(cookies) = session.cookie_manager() {
            cookies.set_accept_policy(webkit6::CookieAcceptPolicy::Never);
        }
        session.connect_download_started(|_, download| {
            download.cancel();
            tracing::debug!("composer: a download was refused");
        });
        let pages: Rc<RefCell<Pages>> = Rc::default();
        let blobs: Rc<RefCell<Option<BlobStore>>> = Rc::default();
        context.register_uri_scheme(
            "cid",
            glib::clone!(
                #[strong]
                pages,
                #[strong]
                blobs,
                move |request| serve_cid(request, &pages, &blobs)
            ),
        );
        let spell = spell_check(&context);
        let (done, filter_done) = async_channel::bounded(1);
        let engine = Rc::new(EditorEngine {
            context,
            session,
            filter: RefCell::new(None),
            filter_state: Rc::new(RefCell::new(Filter::Compiling)),
            filter_done,
            pages,
            blobs,
            spell: RefCell::new(spell),
        });
        let directory = cache.join("content-filters");
        let weak = Rc::downgrade(&engine);
        glib::spawn_future_local(async move {
            let compiled = compile_filter(&directory).await;
            if let Some(engine) = weak.upgrade() {
                match compiled {
                    Ok(filter) => {
                        engine.filter.replace(Some(filter));
                        engine.filter_state.replace(Filter::Ready);
                    }
                    Err(error) => {
                        tracing::error!(
                            "composer: the content filter did not compile, so the composer \
                             stays closed: {error}"
                        );
                        engine.filter_state.replace(Filter::Failed);
                    }
                }
            }
            drop(done);
        });
        engine
    }

    /// The compiled filter, once ready; `None` when it couldn't be compiled, in which case no
    /// body is loaded.
    pub async fn filter(&self) -> Option<webkit6::UserContentFilter> {
        if *self.filter_state.borrow() == Filter::Compiling {
            let _ = self.filter_done.recv().await;
        }
        self.filter.borrow().clone()
    }

    /// Where inline images are read from.
    pub fn set_blobs(&self, blobs: Option<BlobStore>) {
        if blobs.is_some() || self.blobs.borrow().is_none() {
            self.blobs.replace(blobs);
        }
    }

    /// The spell-checking languages WebKit accepted (none without dictionaries).
    pub fn spell_languages(&self) -> Vec<String> {
        self.spell.borrow().clone()
    }

    /// `cid:` requests served, and refused (no such part in the asking view's draft).
    pub fn cid_counts(&self) -> (u64, u64) {
        let pages = self.pages.borrow();
        (pages.served, pages.refused)
    }
}

/// Spell checking only with a dictionary (ADR-014's S7 amendment): the locale's languages are
/// offered and WebKit keeps those enchant has dictionaries for; with none it stays off.
fn spell_check(context: &webkit6::WebContext) -> Vec<String> {
    let mut wanted: Vec<String> = Vec::new();
    for name in glib::language_names() {
        let name = name.as_str();
        if name == "C" || name == "POSIX" || name.contains('.') || name.contains('@') {
            continue;
        }
        if !wanted.iter().any(|known| known == name) {
            wanted.push(name.to_owned());
        }
    }
    let wanted: Vec<&str> = wanted.iter().map(String::as_str).collect();
    context.set_spell_checking_languages(&wanted);
    let accepted: Vec<String> = context
        .spell_checking_languages()
        .iter()
        .map(|language| language.to_string())
        .collect();
    context.set_spell_checking_enabled(!accepted.is_empty());
    tracing::info!(
        languages = accepted.len(),
        "composer: spell checking {}",
        if accepted.is_empty() {
            "off (no dictionary)"
        } else {
            "on"
        }
    );
    accepted
}

async fn compile_filter(directory: &Path) -> Result<webkit6::UserContentFilter, String> {
    let dir = directory.to_owned();
    let made = gio::spawn_blocking(move || sealwax_infrastructure::xdg::ensure_private_dir(&dir))
        .await
        .map_err(|_| "the filter folder could not be made".to_owned())?;
    made.map_err(|error| error.to_string())?;
    let store = webkit6::UserContentFilterStore::new(&directory.to_string_lossy());
    store
        .save_future(FILTER_ID, &glib::Bytes::from_static(FILTER_JSON.as_bytes()))
        .await
        .map_err(|error| error.to_string())
}

/// Answers a `cid:` request from the asking view's own parts, read off the GTK thread.
fn serve_cid(
    request: &webkit6::URISchemeRequest,
    pages: &Rc<RefCell<Pages>>,
    blobs: &Rc<RefCell<Option<BlobStore>>>,
) {
    let page = request.web_view().map(|view| view.page_id());
    let content_id = request
        .uri()
        .map(|uri| uri.as_str().get(4..).unwrap_or_default().to_owned())
        .unwrap_or_default();
    let content_id = glib::Uri::unescape_string(&content_id, None::<&str>)
        .map(|id| id.to_string())
        .unwrap_or(content_id);
    let part = page.and_then(|page| {
        pages
            .borrow()
            .parts
            .get(&page)
            .and_then(|parts| parts.borrow().get(&content_id).cloned())
    });
    let store = blobs.borrow().clone();
    let (Some(part), Some(store)) = (part, store) else {
        pages.borrow_mut().refused += 1;
        let mut error = glib::Error::new(gio::IOErrorEnum::NotFound, "no such part");
        request.finish_error(&mut error);
        return;
    };
    let request = request.clone();
    let pages = pages.clone();
    glib::spawn_future_local(async move {
        let key = part.blob;
        let read = gio::spawn_blocking(move || store.get(&key)).await;
        match read {
            Ok(Ok(bytes)) => {
                pages.borrow_mut().served += 1;
                let length = bytes.len() as i64;
                let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(bytes));
                request.finish(&stream, length, Some(&part.mime_type));
            }
            _ => {
                pages.borrow_mut().refused += 1;
                let mut error = glib::Error::new(gio::IOErrorEnum::NotFound, "the part is gone");
                request.finish_error(&mut error);
            }
        }
    });
}

#[allow(deprecated)]
fn hardened_settings(theme: &Theme) -> webkit6::Settings {
    let settings = webkit6::Settings::new();
    settings.set_enable_javascript(true);
    settings.set_enable_javascript_markup(false);
    settings.set_auto_load_images(true);
    settings.set_hardware_acceleration_policy(webkit6::HardwareAccelerationPolicy::Never);
    settings.set_enable_page_cache(false);
    settings.set_enable_html5_local_storage(false);
    settings.set_enable_html5_database(false);
    settings.set_enable_webgl(false);
    settings.set_enable_webaudio(false);
    settings.set_enable_media(false);
    settings.set_enable_media_stream(false);
    settings.set_enable_mediasource(false);
    settings.set_enable_encrypted_media(false);
    settings.set_enable_media_capabilities(false);
    settings.set_enable_webrtc(false);
    settings.set_enable_developer_extras(cfg!(debug_assertions));
    settings.set_enable_write_console_messages_to_stdout(false);
    settings.set_enable_fullscreen(false);
    settings.set_enable_back_forward_navigation_gestures(false);
    settings.set_enable_site_specific_quirks(false);
    settings.set_javascript_can_open_windows_automatically(false);
    settings.set_javascript_can_access_clipboard(false);
    settings.set_allow_file_access_from_file_urls(false);
    settings.set_allow_universal_access_from_file_urls(false);
    settings.set_allow_top_navigation_to_data_urls(false);
    settings.set_allow_modal_dialogs(false);
    settings.set_enable_dns_prefetching(false);
    if let Some(features) = webkit6::Settings::all_features() {
        for index in 0..features.length() {
            if let Some(feature) = features.get(index)
                && feature
                    .identifier()
                    .is_some_and(|id| NETWORK_FEATURES.contains(&id.as_str()))
            {
                settings.set_feature_enabled(&feature, false);
            }
        }
    }
    apply_fonts(&settings, theme);
    settings
}

fn apply_fonts(settings: &webkit6::Settings, theme: &Theme) {
    let fonts = theme.webview_fonts();
    settings.set_default_font_family(fonts.default_family);
    settings.set_sans_serif_font_family(&fonts.sans_serif);
    settings.set_monospace_font_family(&fonts.monospace);
    settings.set_default_font_size(fonts.default_size);
    settings.set_default_monospace_font_size(fonts.monospace_size);
}

/// The body in the live theme: its background, text and caret, accent links, the reading font
/// at the mock's line height, quotes with a left border in the secondary colour and the line
/// before a quote (its attribution) small and muted.
pub fn editor_css(theme: &Theme) -> String {
    let p = &theme.palette;
    let fonts = theme.webview_fonts();
    let scheme = match p.mode {
        Mode::Dark => "dark",
        Mode::Light => "light",
    };
    let selection = theme
        .controls
        .selection_color
        .with_alpha(theme.controls.selection_fill);
    let space = |px: u32| theme.spacing.space(px);
    format!(
        ":root{{color-scheme:{scheme}}}\n\
         html{{background-color:{background};color:{fg};font:{size}px/{line} {family}}}\n\
         body{{margin:0;padding:{pad_y}px {pad_x}px {pad_bottom}px;min-height:100vh;\
         box-sizing:border-box;overflow-wrap:anywhere;caret-color:{accent};outline:none}}\n\
         a{{color:{accent}}}\n\
         blockquote:not([style]),blockquote[type=cite]{{margin:0 0 0 2px;padding-left:{quote_pad}px;\
         border-left:3px solid {border};color:{secondary}}}\n\
         :has(+ blockquote:not([style])),:has(+ blockquote[type=cite]){{font-size:{small}px;\
         color:{secondary}}}\n\
         pre,code,kbd,samp,tt{{font-family:{mono};font-size:{mono_size}px}}\n\
         pre{{white-space:pre-wrap}}\n\
         hr{{border:0;border-top:1px solid {border}}}\n\
         img{{max-width:100%;height:auto}}\n\
         ul,ol{{margin:0.3em 0;padding-left:1.8em}}\n\
         ::selection{{background-color:{selection}}}\n",
        background = p.background,
        fg = p.foreground,
        size = fonts.default_size,
        line = LINE_HEIGHT,
        family = css_family(&theme.fonts.reading, fonts.default_family),
        accent = p.accent,
        border = theme.ui.border,
        secondary = theme.mail.secondary_text,
        small = theme.scale.small,
        mono = css_family(&fonts.monospace, "monospace"),
        mono_size = fonts.monospace_size,
        pad_y = space(14),
        pad_x = space(18),
        pad_bottom = space(18) * 2,
        quote_pad = space(12),
    )
}

type Handler = Rc<dyn Fn(EditorEvent)>;

struct State {
    handler: RefCell<Option<Handler>>,
    ready: Cell<bool>,
    block: RefCell<BlockState>,
    typing: Cell<u32>,
    history: Cell<(bool, bool)>,
    /// Navigations, new windows, dialogs and other things the body tried and the view refused.
    refused: Cell<u32>,
    /// Loads that started for anything but the draft's own parts and the editor page.
    remote: Cell<u32>,
}

impl State {
    fn emit(&self, event: EditorEvent) {
        let handler = self.handler.borrow().clone();
        if let Some(handler) = handler {
            handler(event);
        }
    }
}

/// One composer body.
#[derive(Clone)]
pub struct Editor {
    view: webkit6::WebView,
    ucm: webkit6::UserContentManager,
    engine: Rc<EditorEngine>,
    parts: Parts,
    state: Rc<State>,
}

impl Editor {
    pub fn new(engine: &Rc<EditorEngine>, theme: &Theme) -> Self {
        let ucm = webkit6::UserContentManager::new();
        ucm.register_script_message_handler(HANDLER, Some(WORLD));
        ucm.add_script(&webkit6::UserScript::for_world(
            HELPER,
            webkit6::UserContentInjectedFrames::TopFrame,
            webkit6::UserScriptInjectionTime::End,
            WORLD,
            &[],
            &[],
        ));
        let view = webkit6::WebView::builder()
            .web_context(&engine.context)
            .network_session(&engine.session)
            .user_content_manager(&ucm)
            .settings(&hardened_settings(theme))
            .default_content_security_policy(CONTENT_SECURITY_POLICY)
            .editable(true)
            .hexpand(true)
            .vexpand(true)
            .build();
        view.add_css_class("sw-composer-body");
        view.set_widget_name("composer-body");
        view.update_property(&[gtk::accessible::Property::Label("Message body")]);
        crate::accessibility::keep_focus_off_key_bindings(&view);
        crate::accessibility::tab_leaves(&view);
        let parts: Parts = Rc::default();
        engine
            .pages
            .borrow_mut()
            .parts
            .insert(view.page_id(), parts.clone());
        let editor = Self {
            view,
            ucm,
            engine: engine.clone(),
            parts,
            state: Rc::new(State {
                handler: RefCell::new(None),
                ready: Cell::new(false),
                block: RefCell::new(BlockState::default()),
                typing: Cell::new(0),
                history: Cell::new((false, false)),
                refused: Cell::new(0),
                remote: Cell::new(0),
            }),
        };
        editor.set_theme(theme);
        editor.connect();
        editor
    }

    pub fn view(&self) -> &webkit6::WebView {
        &self.view
    }

    pub fn set_handler(&self, handler: impl Fn(EditorEvent) + 'static) {
        self.state.handler.replace(Some(Rc::new(handler)));
    }

    /// Loads the editor page with the content filter; the helper reports `Ready`.
    pub fn load(&self, filter: &webkit6::UserContentFilter) {
        self.ucm.remove_all_filters();
        self.ucm.add_filter(filter);
        self.view.load_html(EDITOR_HTML, None);
    }

    pub fn is_ready(&self) -> bool {
        self.state.ready.get()
    }

    /// Restyles the body for `theme` in place.
    pub fn set_theme(&self, theme: &Theme) {
        self.ucm.remove_all_style_sheets();
        self.ucm.add_style_sheet(&webkit6::UserStyleSheet::new(
            &editor_css(theme),
            webkit6::UserContentInjectedFrames::AllFrames,
            webkit6::UserStyleLevel::User,
            &[],
            &[],
        ));
        let background = theme.palette.background;
        self.view.set_background_color(&gdk::RGBA::new(
            f32::from(background.r) / 255.0,
            f32::from(background.g) / 255.0,
            f32::from(background.b) / 255.0,
            1.0,
        ));
        if let Some(settings) = WebViewExt::settings(&self.view) {
            apply_fonts(&settings, theme);
        }
    }

    /// The draft's inline images, by Content-ID.
    pub fn parts(&self) -> HashMap<String, InlineImage> {
        self.parts.borrow().clone()
    }

    pub fn add_part(&self, content_id: String, image: InlineImage) {
        self.parts.borrow_mut().insert(content_id, image);
    }

    pub fn block(&self) -> BlockState {
        self.state.block.borrow().clone()
    }

    pub fn history(&self) -> (bool, bool) {
        self.state.history.get()
    }

    /// Things the body tried that the view refused, and loads of anything but its own parts.
    pub fn refusals(&self) -> (u32, u32) {
        (self.state.refused.get(), self.state.remote.get())
    }

    pub fn engine(&self) -> &Rc<EditorEngine> {
        &self.engine
    }

    pub fn command(&self, command: &str) {
        self.view.execute_editing_command(command);
    }

    pub fn command_with(&self, command: &str, argument: &str) {
        self.view
            .execute_editing_command_with_argument(command, argument);
    }

    /// Runs `script` in the helper's world.
    pub async fn eval(&self, script: &str) -> Result<Value, String> {
        self.view
            .evaluate_javascript_future(script, Some(WORLD), None)
            .await
            .map_err(|error| error.to_string())
    }

    pub fn eval_detached(&self, script: &str) {
        self.view
            .evaluate_javascript(script, Some(WORLD), None, None::<&gio::Cancellable>, |_| {});
    }

    pub async fn set_content(&self, html: &str) {
        let _ = self
            .eval(&format!(
                "sealwaxComposer.setContent({})",
                js::literal(html)
            ))
            .await;
    }

    pub async fn insert_text(&self, text: &str) {
        let _ = self
            .eval(&format!(
                "sealwaxComposer.insertText({})",
                js::literal(text)
            ))
            .await;
    }

    /// The body as the draft keeps it; `None` until the page is ready.
    pub async fn serialize(&self, domain: &str) -> Option<Serialized> {
        if !self.is_ready() {
            return None;
        }
        let value = self
            .eval(&format!(
                "sealwaxComposer.serialize({})",
                js::literal(domain)
            ))
            .await
            .ok()?;
        Some(Serialized {
            html: js::string(&value, "html"),
            cids: js::array(&value, "cids")
                .iter()
                .map(|cid| cid.to_str().to_string())
                .collect(),
        })
    }

    pub async fn is_empty(&self) -> bool {
        self.eval("sealwaxComposer.isEmpty()")
            .await
            .is_ok_and(|value| value.to_boolean())
    }

    /// Shows the stored image for the helper's `token` through `cid:`.
    pub fn adopt(&self, token: &str, content_id: &str) {
        self.eval_detached(&format!(
            "sealwaxComposer.adopt({}, {})",
            js::literal(token),
            js::literal(content_id)
        ));
    }

    /// Selects the whole link under the cursor, so Unlink takes all of it.
    pub async fn select_link(&self) -> bool {
        self.eval("sealwaxComposer.selectLink()")
            .await
            .is_ok_and(|value| value.to_boolean())
    }

    fn connect(&self) {
        let state = &self.state;
        self.ucm.connect_script_message_received(
            Some(HANDLER),
            glib::clone!(
                #[strong]
                state,
                move |_, value| {
                    if let Some(event) = parse_event(value, &state) {
                        state.emit(event);
                    }
                }
            ),
        );
        if let Some(editor) = self.view.editor_state() {
            editor.connect_typing_attributes_notify(glib::clone!(
                #[strong]
                state,
                move |editor| {
                    state.typing.set(editor.typing_attributes());
                    let flags =
                        EditorTypingAttributes::from_bits_truncate(editor.typing_attributes());
                    let mut block = state.block.borrow().clone();
                    block.bold = flags.contains(EditorTypingAttributes::BOLD);
                    block.italic = flags.contains(EditorTypingAttributes::ITALIC);
                    block.underline = flags.contains(EditorTypingAttributes::UNDERLINE);
                    block.strikethrough = flags.contains(EditorTypingAttributes::STRIKETHROUGH);
                    state.block.replace(block.clone());
                    state.emit(EditorEvent::State(block));
                }
            ));
            editor.connect_changed(glib::clone!(
                #[strong]
                state,
                move |editor| {
                    let history = (editor.is_undo_available(), editor.is_redo_available());
                    if state.history.replace(history) != history {
                        state.emit(EditorEvent::History {
                            undo: history.0,
                            redo: history.1,
                        });
                    }
                }
            ));
        }
        self.view.connect_decide_policy(glib::clone!(
            #[strong]
            state,
            move |_, decision, kind| {
                let ours = kind == PolicyDecisionType::NavigationAction
                    && !state.ready.get()
                    && decision
                        .downcast_ref::<webkit6::NavigationPolicyDecision>()
                        .and_then(|decision| decision.navigation_action())
                        .and_then(|action| action.request())
                        .and_then(|request| request.uri())
                        .is_some_and(|uri| uri == "about:blank");
                match kind {
                    PolicyDecisionType::Response => decision.use_(),
                    _ if ours => decision.use_(),
                    _ => {
                        state.refused.set(state.refused.get() + 1);
                        decision.ignore();
                    }
                }
                true
            }
        ));
        self.view.connect_create(glib::clone!(
            #[strong]
            state,
            move |_, _| {
                state.refused.set(state.refused.get() + 1);
                None
            }
        ));
        self.view.connect_script_dialog(glib::clone!(
            #[strong]
            state,
            move |_, _| {
                state.refused.set(state.refused.get() + 1);
                true
            }
        ));
        self.view.connect_permission_request(glib::clone!(
            #[strong]
            state,
            move |_, request| {
                request.deny();
                state.refused.set(state.refused.get() + 1);
                true
            }
        ));
        self.view.connect_query_permission_state(|_, query| {
            query.finish(webkit6::PermissionState::Denied);
            true
        });
        self.view.connect_run_file_chooser(glib::clone!(
            #[strong]
            state,
            move |_, request| {
                request.cancel();
                state.refused.set(state.refused.get() + 1);
                true
            }
        ));
        self.view.connect_run_color_chooser(|_, request| {
            request.cancel();
            true
        });
        self.view.connect_enter_fullscreen(|_| true);
        self.view.connect_print(|_, _| true);
        self.view.connect_show_notification(|_, _| true);
        self.view.connect_authenticate(|_, request| {
            request.cancel();
            true
        });
        self.view
            .connect_load_failed_with_tls_errors(|_, _, _, _| true);
        self.view.connect_context_menu(|_, menu, _| {
            for item in menu.items() {
                if !item.is_separator() && !keep_in_context_menu(item.stock_action()) {
                    menu.remove(&item);
                }
            }
            menu.n_items() == 0
        });
        self.view.connect_resource_load_started(glib::clone!(
            #[strong]
            state,
            move |_, _, request| {
                let uri = request.uri().map(String::from).unwrap_or_default();
                let own = ["cid:", "data:", "about:", "blob:"]
                    .iter()
                    .any(|scheme| uri.starts_with(scheme));
                if !own {
                    state.remote.set(state.remote.get() + 1);
                }
            }
        ));
        self.view.connect_web_process_terminated(glib::clone!(
            #[strong]
            state,
            move |_, reason| {
                tracing::warn!(?reason, "composer: the web process ended");
                state.ready.set(false);
                state.emit(EditorEvent::Crashed);
            }
        ));
    }

    /// Forgets the view's parts; call when its window goes.
    pub fn release(&self) {
        self.state.handler.replace(None);
        self.engine
            .pages
            .borrow_mut()
            .parts
            .remove(&self.view.page_id());
        self.view.stop_loading();
    }
}

fn keep_in_context_menu(action: ContextMenuAction) -> bool {
    matches!(
        action,
        ContextMenuAction::Cut
            | ContextMenuAction::Copy
            | ContextMenuAction::Paste
            | ContextMenuAction::Delete
            | ContextMenuAction::SelectAll
            | ContextMenuAction::SpellingGuess
            | ContextMenuAction::NoGuessesFound
            | ContextMenuAction::IgnoreSpelling
            | ContextMenuAction::LearnSpelling
            | ContextMenuAction::InputMethods
            | ContextMenuAction::Unicode
            | ContextMenuAction::InsertEmoji
    ) || (action == ContextMenuAction::InspectElement && cfg!(debug_assertions))
}

fn parse_event(value: &Value, state: &State) -> Option<EditorEvent> {
    Some(match js::string(value, "type").as_str() {
        "ready" => {
            state.ready.set(true);
            EditorEvent::Ready
        }
        "input" => EditorEvent::Input {
            composing: js::boolean(value, "composing"),
        },
        "state" => {
            let typing = EditorTypingAttributes::from_bits_truncate(state.typing.get());
            let block = BlockState {
                bulleted: js::boolean(value, "bulleted"),
                numbered: js::boolean(value, "numbered"),
                quote: js::boolean(value, "quote"),
                indent: js::boolean(value, "indent"),
                link: js::string(value, "link"),
                block: js::string(value, "block"),
                selection: js::boolean(value, "selection"),
                bold: js::boolean(value, "bold") || typing.contains(EditorTypingAttributes::BOLD),
                italic: js::boolean(value, "italic")
                    || typing.contains(EditorTypingAttributes::ITALIC),
                underline: js::boolean(value, "underline")
                    || typing.contains(EditorTypingAttributes::UNDERLINE),
                strikethrough: js::boolean(value, "strikethrough")
                    || typing.contains(EditorTypingAttributes::STRIKETHROUGH),
            };
            state.block.replace(block.clone());
            EditorEvent::State(block)
        }
        "image" => EditorEvent::Image {
            token: js::string(value, "token"),
            mime_type: js::string(value, "mime"),
            data: js::string(value, "data"),
            name: js::string(value, "name"),
        },
        "file" => EditorEvent::File {
            name: js::string(value, "name"),
            mime_type: js::string(value, "mime"),
            data: js::string(value, "data"),
        },
        kind @ ("paste" | "drop") => EditorEvent::Transfer {
            kind: kind.to_owned(),
            mode: js::string(value, "mode"),
            stats: js::get(value, "stats").map(|stats| CleanStats::from_value(&stats)),
        },
        "error" => EditorEvent::Problem(js::string(value, "message")),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::theme::{ThemeInputs, ThemeOptions};

    #[test]
    fn the_body_takes_the_theme() {
        for mode in [Mode::Dark, Mode::Light] {
            let theme = Theme::new(&ThemeInputs::builtin(mode), &ThemeOptions::default());
            let css = editor_css(&theme);
            let p = &theme.palette;
            for expected in [
                format!("background-color:{}", p.background),
                format!("color:{}", p.foreground),
                format!("caret-color:{}", p.accent),
                format!("a{{color:{}}}", p.accent),
                format!("color:{}}}", theme.mail.secondary_text),
                "/1.6 ".to_owned(),
            ] {
                assert!(css.contains(&expected), "{expected}\n{css}");
            }
            assert!(css.contains(match mode {
                Mode::Dark => "color-scheme:dark",
                Mode::Light => "color-scheme:light",
            }));
        }
    }

    #[test]
    fn the_policy_admits_no_remote_image() {
        assert!(!CONTENT_SECURITY_POLICY.contains("https:"));
        assert!(CONTENT_SECURITY_POLICY.contains("img-src data: cid:;"));
        assert!(FILTER_JSON.contains(r#""url-filter": ".*"}, "action": {"type": "block"}"#));
    }
}
