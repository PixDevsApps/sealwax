//! One hardened WebView for one HTML body (ADR-011, SECURITY.md "Message rendering"): no
//! message script ever runs (`enable-javascript-markup` off); the CSP is set at construction;
//! the content filter blocks every remote load until the user allows images; WebKit's
//! prefetch and preconnect features are off; links and form submissions are intercepted in
//! Sealwax's isolated world and every navigation is blocked as the backstop; new windows,
//! dialogs, permissions, downloads, the file and colour choosers, fullscreen, printing and
//! authentication are refused; the context menu keeps only copying. Rendering is on the CPU
//! (`hardware-acceleration-policy` NEVER), and the height comes from a ResizeObserver in the
//! isolated world.

use super::engine::{CONTENT_SECURITY_POLICY, Engine, NETWORK_FEATURES};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use sealwax_domain::theme::WebViewFonts;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use webkit6::prelude::*;
use webkit6::{ContextMenuAction, PolicyDecisionType, javascriptcore};

/// The isolated script world Sealwax's helper scripts run in; page content can't see it.
pub const WORLD: &str = "sealwax";

/// No view grows taller than this; a taller body scrolls inside its view. S2: 16,000 px
/// rendered and scrolled on the CPU path; a 30,000 px CPU-rendered view stalled GTK.
pub const MAX_VIEW_HEIGHT: i32 = 16_000;

/// What a reader view loads between messages.
const BLANK: &str = "<!DOCTYPE html><html><head></head><body></body></html>";

/// Reports the body's height and the width it was measured at, the first frame and the end of
/// the load (images included), each tagged with the load's number (S2). The
/// root's height follows its content (the reader stylesheet's measuring rule), so shrinking
/// content shrinks it too. Content sized in viewport units grows each time the view is resized
/// to fit it: growth that directly follows a height-only resize (the `resize` event runs before
/// ResizeObserver callbacks) may continue only while it converges, each step under three
/// quarters of the previous one and at most ten steps; then the height stops growing and the
/// rest scrolls inside. A width change starts the count again.
const HEIGHT_JS: &str = r#"(() => {
  const load = SEALWAX_LOAD;
  const post = (name, value) => window.webkit.messageHandlers[name].postMessage(value);
  let last = -1;
  let lastWidth = -1;
  let steps = 0;
  let previousStep = Infinity;
  let frozen = false;
  const measure = () => {
    const root = document.documentElement;
    if (!root) return -1;
    const box = Math.ceil(root.getBoundingClientRect().height);
    const scroll = root.scrollHeight;
    return scroll > window.innerHeight ? Math.max(box, scroll) : box;
  };
  const report = () => {
    const height = measure();
    const width = window.innerWidth;
    if (height < 0 || (frozen && height > last)) return;
    if (height === last && width === lastWidth) return;
    last = height;
    lastWidth = width;
    post('height', {load, height, width});
  };
  let width = window.innerWidth;
  window.addEventListener('resize', () => {
    if (window.innerWidth !== width) {
      width = window.innerWidth;
      steps = 0;
      previousStep = Infinity;
      frozen = false;
      return;
    }
    const step = measure() - last;
    if (last < 0 || step <= 0) return;
    steps += 1;
    if (steps > 10 || step >= previousStep * 0.75) frozen = true;
    previousStep = step;
  });
  new ResizeObserver(report).observe(document.documentElement);
  report();
  requestAnimationFrame(() => requestAnimationFrame(() => post('frame', {load})));
  const loaded = () => post('loaded', {load});
  if (document.readyState === 'complete') loaded();
  else window.addEventListener('load', loaded, {once: true});
})();"#;

/// Hands every link activation to Sealwax instead of letting WebKit navigate: WebKit would
/// otherwise connect to a clicked link's host (and send `<a ping>`) before `decide-policy` is
/// asked (S2). Form submissions are cancelled the same way. The link's text goes along for
/// the link-mismatch check (M5).
const LINKS_JS: &str = r#"(() => {
  const follow = (event) => {
    if (!(event.target instanceof Element)) return;
    const link = event.target.closest('a[href], area[href], a[*|href]');
    if (!link) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    if (event.type === 'auxclick' && event.button !== 1) return;
    const raw = link.getAttribute('href') ?? link.getAttributeNS('http://www.w3.org/1999/xlink', 'href') ?? '';
    let href = raw;
    try { href = new URL(raw, document.baseURI).href; } catch (e) {}
    const text = (link.textContent || '').trim().slice(0, 500);
    window.webkit.messageHandlers.link.postMessage({href, text});
  };
  document.addEventListener('click', follow, true);
  document.addEventListener('auxclick', follow, true);
  document.addEventListener('submit', (event) => event.preventDefault(), true);
})();"#;

/// A link the user activated in a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The target, resolved against the document (`about:blank`): relative links stay unusable.
    pub uri: String,
    /// The link's text, at most 500 characters. Message content: never log it.
    pub text: String,
}

/// Something a message tried and the view refused; counted, never logged with its URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Refused {
    Navigation,
    NewWindow,
    Dialog,
    Permission,
    Download,
    FileChooser,
    ColorChooser,
    Fullscreen,
    Print,
    Notification,
    Authentication,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ViewEvent {
    /// The body's height in px (capped) and the viewport width it was measured at.
    Height {
        height: i32,
        width: i32,
    },
    /// WebKit rendered the first frame of the document.
    Frame,
    /// The document's `load` event: its images, `cid:` parts included, are in.
    Loaded,
    Finished,
    Failed,
    Link(Link),
    Refused(Refused),
    /// The web process ended; the view shows nothing until it loads again.
    Crashed,
    /// A resource load started (its URI); only the hostile suite listens.
    Resource(String),
}

type Handler = Rc<dyn Fn(&ViewEvent)>;

#[derive(Default)]
struct State {
    /// `load_html` calls whose navigation decision hasn't come yet: those `about:blank`
    /// navigations are Sealwax's own; every other navigation is refused.
    expected: Cell<u32>,
    /// The current load; height and frame reports from an earlier document carry another.
    load: Cell<u64>,
    /// The current load committed: resource events belong to it.
    committed: Cell<bool>,
    handler: RefCell<Option<Handler>>,
    height: Cell<i32>,
}

impl State {
    fn emit(&self, event: ViewEvent) {
        let handler = self.handler.borrow().clone();
        if let Some(handler) = handler {
            handler(&event);
        }
    }
}

#[derive(Clone)]
pub struct MessageView {
    view: webkit6::WebView,
    ucm: webkit6::UserContentManager,
    state: Rc<State>,
}

impl MessageView {
    pub(super) fn new(
        engine: &Engine,
        fonts: &WebViewFonts,
        related: Option<&webkit6::WebView>,
    ) -> Self {
        let ucm = webkit6::UserContentManager::new();
        for name in ["height", "frame", "loaded", "link"] {
            ucm.register_script_message_handler(name, Some(WORLD));
        }
        let settings = hardened_settings(fonts);
        let builder = webkit6::WebView::builder()
            .user_content_manager(&ucm)
            .settings(&settings)
            .default_content_security_policy(CONTENT_SECURITY_POLICY)
            .vexpand(false)
            .height_request(1);
        let builder = match related {
            Some(root) => builder.related_view(root),
            None => builder
                .web_context(engine.context())
                .network_session(engine.session()),
        };
        let view = builder.build();
        view.set_valign(gtk::Align::Start);
        view.add_css_class("sw-reader-web-view");
        let this = Self {
            view,
            ucm,
            state: Rc::default(),
        };
        this.connect();
        this
    }

    pub fn web_view(&self) -> &webkit6::WebView {
        &self.view
    }

    pub fn page_id(&self) -> u64 {
        self.view.page_id()
    }

    /// The last height applied, in px.
    pub fn height(&self) -> i32 {
        self.state.height.get()
    }

    /// Where this view's events go until [`MessageView::blank`].
    pub fn set_handler(&self, handler: impl Fn(&ViewEvent) + 'static) {
        self.state.handler.replace(Some(Rc::new(handler)));
    }

    pub fn set_fonts(&self, fonts: &WebViewFonts) {
        if let Some(settings) = WebViewExt::settings(&self.view) {
            apply_fonts(&settings, fonts);
        }
    }

    /// Loads one body. `filter` is the compiled content filter: with it, nothing remote loads;
    /// without it (the user allowed images for this message) the CSP still admits only https
    /// images. The view keeps its height until the new body reports one.
    pub fn load(
        &self,
        html: &str,
        css: &str,
        background: &gdk::RGBA,
        filter: Option<&webkit6::UserContentFilter>,
    ) {
        self.view.stop_loading();
        self.ucm.remove_all_filters();
        if let Some(filter) = filter {
            self.ucm.add_filter(filter);
        }
        self.set_style(css, background);
        self.start(html);
    }

    /// A new load: fresh helper scripts that tag their reports with its number, and one
    /// navigation decision expected for it.
    fn start(&self, html: &str) {
        let load = self.state.load.get() + 1;
        self.state.load.set(load);
        self.state.committed.set(false);
        self.ucm.remove_all_scripts();
        let height = HEIGHT_JS.replace("SEALWAX_LOAD", &load.to_string());
        for source in [height.as_str(), LINKS_JS] {
            self.ucm.add_script(&webkit6::UserScript::for_world(
                source,
                webkit6::UserContentInjectedFrames::TopFrame,
                webkit6::UserScriptInjectionTime::End,
                WORLD,
                &[],
                &[],
            ));
        }
        self.state.expected.set(self.state.expected.get() + 1);
        self.view.load_html(html, None);
    }

    /// Replaces the user stylesheet and the background; WebKit restyles the loaded document
    /// in place.
    pub fn set_style(&self, css: &str, background: &gdk::RGBA) {
        self.ucm.remove_all_style_sheets();
        self.ucm.add_style_sheet(&webkit6::UserStyleSheet::new(
            css,
            webkit6::UserContentInjectedFrames::AllFrames,
            webkit6::UserStyleLevel::User,
            &[],
            &[],
        ));
        self.view.set_background_color(background);
    }

    /// Forgets the message: no handler, a blank page, the content filter back on, no
    /// stylesheet, unparented.
    pub(super) fn blank(&self, filter: Option<&webkit6::UserContentFilter>) {
        self.state.handler.replace(None);
        if self.view.parent().is_some() {
            self.view.unparent();
        }
        self.view.stop_loading();
        self.ucm.remove_all_style_sheets();
        self.ucm.remove_all_filters();
        if let Some(filter) = filter {
            self.ucm.add_filter(filter);
        }
        self.state.height.set(1);
        self.view.set_size_request(-1, 1);
        self.start(BLANK);
    }

    pub(super) fn destroy(self) {
        self.state.handler.replace(None);
        if self.view.parent().is_some() {
            self.view.unparent();
        }
        self.view.stop_loading();
    }

    /// Runs `body` (a function body) in Sealwax's isolated world.
    pub async fn eval(&self, body: &str) -> Result<javascriptcore::Value, glib::Error> {
        self.view
            .call_async_javascript_function_future(body, None, Some(WORLD), None)
            .await
    }

    fn connect(&self) {
        let state = &self.state;
        self.ucm.connect_script_message_received(
            Some("height"),
            glib::clone!(
                #[weak(rename_to = view)]
                self.view,
                #[strong]
                state,
                move |_, value| {
                    let number = |name: &str| {
                        value
                            .object_get_property(name)
                            .map_or(-1.0, |value| value.to_double())
                    };
                    if number("load") != state.load.get() as f64 {
                        return;
                    }
                    let number = |name: &str| number(name) as i32;
                    let height = number("height").clamp(1, MAX_VIEW_HEIGHT);
                    let width = number("width");
                    state.height.set(height);
                    view.set_size_request(-1, height);
                    state.emit(ViewEvent::Height { height, width });
                }
            ),
        );
        for (name, event) in [("frame", ViewEvent::Frame), ("loaded", ViewEvent::Loaded)] {
            self.ucm.connect_script_message_received(
                Some(name),
                glib::clone!(
                    #[strong]
                    state,
                    move |_, value| {
                        let load = value
                            .object_get_property("load")
                            .map_or(-1.0, |value| value.to_double());
                        if load == state.load.get() as f64 {
                            state.emit(event.clone());
                        }
                    }
                ),
            );
        }
        self.ucm.connect_script_message_received(
            Some("link"),
            glib::clone!(
                #[strong]
                state,
                move |_, value| {
                    let text = |name: &str| {
                        value
                            .object_get_property(name)
                            .filter(|value| value.is_string())
                            .map(|value| value.to_str().to_string())
                            .unwrap_or_default()
                    };
                    state.emit(ViewEvent::Link(Link {
                        uri: text("href"),
                        text: text("text"),
                    }));
                }
            ),
        );
        self.view.connect_load_changed(glib::clone!(
            #[strong]
            state,
            move |_, event| match event {
                webkit6::LoadEvent::Committed => state.committed.set(true),
                webkit6::LoadEvent::Finished if state.committed.get() => {
                    state.emit(ViewEvent::Finished);
                }
                _ => {}
            }
        ));
        self.view.connect_load_failed(glib::clone!(
            #[strong]
            state,
            move |_, _, _, error| {
                if !error.matches(webkit6::NetworkError::Cancelled) {
                    tracing::debug!("reader: a load failed");
                    state.emit(ViewEvent::Failed);
                }
                true
            }
        ));
        self.view
            .connect_load_failed_with_tls_errors(|_, _, _, _| true);
        self.view.connect_decide_policy(glib::clone!(
            #[strong]
            state,
            move |_, decision, kind| {
                decide(decision, kind, &state);
                true
            }
        ));
        self.view.connect_create(glib::clone!(
            #[strong]
            state,
            move |_, _| {
                state.emit(ViewEvent::Refused(Refused::NewWindow));
                None
            }
        ));
        self.view.connect_script_dialog(glib::clone!(
            #[strong]
            state,
            move |_, _| {
                state.emit(ViewEvent::Refused(Refused::Dialog));
                true
            }
        ));
        self.view.connect_permission_request(glib::clone!(
            #[strong]
            state,
            move |_, request| {
                request.deny();
                state.emit(ViewEvent::Refused(Refused::Permission));
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
                state.emit(ViewEvent::Refused(Refused::FileChooser));
                true
            }
        ));
        self.view.connect_run_color_chooser(glib::clone!(
            #[strong]
            state,
            move |_, request| {
                request.cancel();
                state.emit(ViewEvent::Refused(Refused::ColorChooser));
                true
            }
        ));
        self.view.connect_enter_fullscreen(glib::clone!(
            #[strong]
            state,
            move |_| {
                state.emit(ViewEvent::Refused(Refused::Fullscreen));
                true
            }
        ));
        self.view.connect_print(glib::clone!(
            #[strong]
            state,
            move |_, _| {
                state.emit(ViewEvent::Refused(Refused::Print));
                true
            }
        ));
        self.view.connect_show_notification(glib::clone!(
            #[strong]
            state,
            move |_, _| {
                state.emit(ViewEvent::Refused(Refused::Notification));
                true
            }
        ));
        self.view.connect_authenticate(glib::clone!(
            #[strong]
            state,
            move |_, request| {
                request.cancel();
                state.emit(ViewEvent::Refused(Refused::Authentication));
                true
            }
        ));
        self.view.connect_context_menu(|_, menu, _| {
            for item in menu.items() {
                if !keep_in_context_menu(item.stock_action()) {
                    menu.remove(&item);
                }
            }
            menu.n_items() == 0
        });
        self.view.connect_resource_load_started(glib::clone!(
            #[strong]
            state,
            move |_, _, request| {
                if state.committed.get() {
                    let uri = request.uri().map(String::from).unwrap_or_default();
                    state.emit(ViewEvent::Resource(uri));
                }
            }
        ));
        self.view.connect_web_process_terminated(glib::clone!(
            #[strong]
            state,
            move |_, reason| {
                tracing::warn!(?reason, "reader: the web process ended");
                state.emit(ViewEvent::Crashed);
            }
        ));
    }
}

/// Copying stays; everything that would open, download, reload or navigate goes. The
/// inspector stays in debug builds.
fn keep_in_context_menu(action: ContextMenuAction) -> bool {
    matches!(
        action,
        ContextMenuAction::Copy
            | ContextMenuAction::CopyLinkToClipboard
            | ContextMenuAction::CopyImageToClipboard
            | ContextMenuAction::SelectAll
    ) || (action == ContextMenuAction::InspectElement && cfg!(debug_assertions))
}

fn decide(decision: &webkit6::PolicyDecision, kind: PolicyDecisionType, state: &State) {
    match kind {
        PolicyDecisionType::NavigationAction => {
            let expected = state.expected.get();
            let ours = expected > 0
                && decision
                    .downcast_ref::<webkit6::NavigationPolicyDecision>()
                    .and_then(|decision| decision.navigation_action())
                    .and_then(|action| action.request())
                    .and_then(|request| request.uri())
                    .is_some_and(|uri| uri == "about:blank");
            if ours {
                state.expected.set(expected - 1);
                decision.use_();
            } else {
                state.emit(ViewEvent::Refused(Refused::Navigation));
                decision.ignore();
            }
        }
        PolicyDecisionType::Response => {
            let displayable = decision
                .downcast_ref::<webkit6::ResponsePolicyDecision>()
                .is_some_and(|response| response.is_mime_type_supported());
            if displayable {
                decision.use_();
            } else {
                state.emit(ViewEvent::Refused(Refused::Download));
                decision.ignore();
            }
        }
        _ => {
            state.emit(ViewEvent::Refused(Refused::NewWindow));
            decision.ignore();
        }
    }
}

#[allow(deprecated)]
fn hardened_settings(fonts: &WebViewFonts) -> webkit6::Settings {
    let settings = webkit6::Settings::new();
    // Script markup is off: no message script runs. JavaScript itself stays on only for
    // Sealwax's isolated-world helpers.
    settings.set_enable_javascript(true);
    settings.set_enable_javascript_markup(false);
    settings.set_auto_load_images(true);
    settings.set_load_icons_ignoring_image_load_setting(false);
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
    settings.set_print_backgrounds(true);
    // Both do nothing in WebKitGTK 2.52 (S2); the feature switches below do the work. They
    // stay off in case a future WebKit honours them again.
    settings.set_enable_dns_prefetching(false);
    settings.set_enable_hyperlink_auditing(false);
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
    apply_fonts(&settings, fonts);
    settings
}

/// WebKit's default fonts, set explicitly because it would otherwise pick its own.
fn apply_fonts(settings: &webkit6::Settings, fonts: &WebViewFonts) {
    settings.set_default_font_family(fonts.default_family);
    settings.set_sans_serif_font_family(&fonts.sans_serif);
    settings.set_monospace_font_family(&fonts.monospace);
    settings.set_default_font_size(fonts.default_size);
    settings.set_default_monospace_font_size(fonts.monospace_size);
}

/// The features [`hardened_settings`] switches off, as WebKit reports them, for checks.
pub fn disabled_features(view: &webkit6::WebView) -> Vec<String> {
    let (Some(settings), Some(features)) = (
        WebViewExt::settings(view),
        webkit6::Settings::all_features(),
    ) else {
        return Vec::new();
    };
    (0..features.length())
        .filter_map(|index| features.get(index))
        .filter(|feature| !settings.is_feature_enabled(feature))
        .filter_map(|feature| feature.identifier().map(String::from))
        .filter(|id| NETWORK_FEATURES.contains(&id.as_str()))
        .collect()
}
