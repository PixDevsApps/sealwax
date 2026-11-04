//! Approach A's reader views (ADR-011): one hardened WebView per HTML body, all sharing one
//! web process through `related-view`, an ephemeral network session, a compiled content
//! filter and a `cid:` scheme answered from memory per view.

use email_spike_s2::corpus::Part;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;
use webkit6::prelude::*;
use webkit6::{PolicyDecisionType, javascriptcore};

/// The isolated script world Mail's helper scripts run in.
pub const WORLD: &str = "mail";

/// Blocks every load except `cid:`, `data:` and `about:` until remote content is allowed.
/// WebKit's content-filter regexes have no disjunction, so each exception is its own rule.
const FILTER_JSON: &str = r#"[
  {"trigger": {"url-filter": ".*"}, "action": {"type": "block"}},
  {"trigger": {"url-filter": "^cid:"}, "action": {"type": "ignore-previous-rules"}},
  {"trigger": {"url-filter": "^data:"}, "action": {"type": "ignore-previous-rules"}},
  {"trigger": {"url-filter": "^about:"}, "action": {"type": "ignore-previous-rules"}}
]"#;

/// Reports the body's height (and the first rendered frame) to the UI process. The root's
/// height follows its content (see `theme::reader_css`), so shrinking content shrinks it too.
/// Content sized in viewport units grows each time the view is resized to fit it. Growth that
/// directly follows a height-only resize (the `resize` event runs before ResizeObserver
/// callbacks in a rendering update) may continue only while it converges: each step must be
/// under three quarters of the previous one, for at most ten steps. Otherwise the height stops
/// growing and the rest scrolls inside the view. A width change starts the count again.
const HELPER_JS: &str = r#"(() => {
  const post = (name, value) => window.webkit.messageHandlers[name].postMessage(value);
  let last = -1;
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
    if (height < 0 || height === last || (frozen && height > last)) return;
    last = height;
    post('height', height);
  };
  let width = window.innerWidth;
  window.addEventListener('resize', () => {
    if (window.innerWidth !== width) {
      width = window.innerWidth;
      steps = 0;
      previousStep = Infinity;
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
  requestAnimationFrame(() => requestAnimationFrame(() => post('frame', 0)));
})();"#;

/// No view grows taller than this; a taller body scrolls inside its view. 16 000 px rendered
/// and scrolled on both the GPU and the CPU path here (scale 1); a 30 000 px CPU-rendered
/// view stalled GTK for 56–204 ms per frame.
pub const MAX_VIEW_HEIGHT: i32 = 16_000;

/// Hands every link activation to Mail instead of letting WebKit navigate: WebKit would
/// otherwise connect to a clicked link's host (and send `<a ping>`) before `decide-policy`
/// is asked. Form submissions are cancelled the same way.
const LINKS_JS: &str = r#"(() => {
  const follow = (event) => {
    if (!(event.target instanceof Element)) return;
    const link = event.target.closest('a[href], area[href], a[*|href]');
    if (!link) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    const raw = link.getAttribute('href') ?? link.getAttributeNS('http://www.w3.org/1999/xlink', 'href') ?? '';
    let target = raw;
    try { target = new URL(raw, document.baseURI).href; } catch (e) {}
    window.webkit.messageHandlers.link.postMessage(target);
  };
  document.addEventListener('click', follow, true);
  document.addEventListener('auxclick', follow, true);
  document.addEventListener('submit', (event) => event.preventDefault(), true);
})();"#;

/// Runtime features that make requests without any script and that the old settings no
/// longer control (`enable-dns-prefetching` and `enable-hyperlink-auditing` do nothing in
/// WebKitGTK 2.52). `MAIL_S2_FEATURES_OFF` (comma-separated) replaces the list for experiments.
pub const NETWORK_FEATURES: [&str; 5] = [
    "LinkDNSPrefetch",
    "LinkPreconnect",
    "LinkPrefetch",
    "LinkPreload",
    "SpeculationRulesPrefetch",
];

/// ADR-011's construction-time policy; `image_origin` widens `img-src` for the
/// remote-allowed control (the canary is plain http, which `https:` does not match).
pub fn content_security_policy(image_origin: Option<&str>) -> String {
    let images = match image_origin {
        Some(origin) => format!("img-src cid: data: https: {origin}"),
        None => "img-src cid: data: https:".to_owned(),
    };
    format!(
        "default-src 'none'; {images}; style-src 'unsafe-inline'; font-src cid: data:; \
         form-action 'none'; base-uri 'none'; frame-src 'none'"
    )
}

/// Which layers a view gets. Production has all of them; the other combinations exist to
/// show what each layer stops on its own, and the controls prove the canary and the script
/// traces detect a leak.
#[derive(Debug, Clone)]
pub struct Defences {
    pub name: &'static str,
    pub page_scripts: bool,
    pub csp: Option<String>,
    pub content_filter: bool,
    pub navigation: Navigation,
    pub hardened_settings: bool,
    /// Turn off [`NETWORK_FEATURES`].
    pub network_features_off: bool,
    /// Run [`LINKS_JS`] in Mail's world.
    pub intercept_links: bool,
    /// Size the view to its content (approach A). Off for the approach-B proxy, whose one
    /// view keeps the pane's height and scrolls inside.
    pub fit_height: bool,
    /// Render this view without hardware acceleration (`hardware-acceleration-policy`
    /// NEVER) even when the engine is accelerated.
    pub cpu_rendering: bool,
}

/// What `decide-policy` does with navigations the message starts. Every navigation is
/// recorded either way.
#[derive(Debug, Clone)]
pub enum Navigation {
    Block,
    /// Controls only: let navigations that start with one of these prefixes through.
    AllowOnly(Vec<String>),
}

impl Defences {
    pub fn production() -> Self {
        Self {
            name: "production",
            page_scripts: false,
            csp: Some(content_security_policy(None)),
            content_filter: true,
            navigation: Navigation::Block,
            hardened_settings: true,
            network_features_off: true,
            intercept_links: true,
            fit_height: true,
            cpu_rendering: false,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    Committed,
    Finished,
    Failed(String),
    Height(i32),
    Frame,
    Dialog(String),
    Navigation {
        uri: String,
        kind: String,
        blocked: bool,
    },
    Link(String),
    NewWindow(String),
    Download(String),
    Resource(String),
    Crashed(String),
}

#[derive(Debug, Clone)]
pub struct Stamped {
    pub at: Instant,
    pub event: Event,
}

#[derive(Debug, Clone)]
pub struct CidRequest {
    pub page: u64,
    pub cid: String,
    pub found: bool,
}

type PartMap = HashMap<u64, HashMap<String, Part>>;

pub struct Engine {
    context: webkit6::WebContext,
    session: webkit6::NetworkSession,
    filter: webkit6::UserContentFilter,
    parts: Rc<RefCell<PartMap>>,
    pub cid_requests: Rc<RefCell<Vec<CidRequest>>>,
    pub downloads: Rc<RefCell<Vec<String>>>,
    root: RefCell<Option<webkit6::WebView>>,
    share_process: bool,
    accelerated: bool,
    sans: String,
}

impl Engine {
    pub async fn new(
        cache_dir: &Path,
        share_process: bool,
        accelerated: bool,
        sans: &str,
    ) -> Result<Rc<Self>, glib::Error> {
        let context = webkit6::WebContext::new();
        context.set_cache_model(webkit6::CacheModel::DocumentViewer);
        let session = webkit6::NetworkSession::new_ephemeral();
        let downloads = Rc::new(RefCell::new(Vec::new()));
        session.connect_download_started(glib::clone!(
            #[strong]
            downloads,
            move |_, download| {
                let uri = download
                    .request()
                    .and_then(|request| request.uri())
                    .map(String::from)
                    .unwrap_or_default();
                download.cancel();
                downloads.borrow_mut().push(uri);
            }
        ));
        let store = webkit6::UserContentFilterStore::new(
            &cache_dir.join("content-filters").to_string_lossy(),
        );
        let filter = store
            .save_future(
                "block-remote",
                &glib::Bytes::from_static(FILTER_JSON.as_bytes()),
            )
            .await?;
        let parts: Rc<RefCell<PartMap>> = Rc::default();
        let cid_requests: Rc<RefCell<Vec<CidRequest>>> = Rc::default();
        context.register_uri_scheme(
            "cid",
            glib::clone!(
                #[strong]
                parts,
                #[strong]
                cid_requests,
                move |request| serve_cid(request, &parts, &cid_requests)
            ),
        );
        Ok(Rc::new(Self {
            context,
            session,
            filter,
            parts,
            cid_requests,
            downloads,
            root: RefCell::new(None),
            share_process,
            accelerated,
            sans: sans.to_owned(),
        }))
    }

    /// The parts `view` may load; any other `cid:` it asks for fails.
    pub fn set_parts(&self, view: &webkit6::WebView, parts: &[Part]) {
        self.set_parts_for_page(view.page_id(), parts);
    }

    pub fn set_parts_for_page(&self, page: u64, parts: &[Part]) {
        let map = parts
            .iter()
            .map(|part| (part.cid.clone(), part.clone()))
            .collect();
        self.parts.borrow_mut().insert(page, map);
    }

    /// A view with WebKit's default settings in the same context and ephemeral session, for
    /// WebKit's own pages such as `webkit://gpu`.
    pub fn plain_view(&self) -> webkit6::WebView {
        webkit6::WebView::builder()
            .web_context(&self.context)
            .network_session(&self.session)
            .build()
    }

    pub fn new_view(self: &Rc<Self>, defences: &Defences) -> MessageView {
        let ucm = webkit6::UserContentManager::new();
        if defences.content_filter {
            ucm.add_filter(&self.filter);
        }
        ucm.register_script_message_handler("height", Some(WORLD));
        ucm.register_script_message_handler("frame", Some(WORLD));
        ucm.register_script_message_handler("link", Some(WORLD));
        let scripts = [
            Some(HELPER_JS),
            defences.intercept_links.then_some(LINKS_JS),
        ];
        for source in scripts.into_iter().flatten() {
            ucm.add_script(&webkit6::UserScript::for_world(
                source,
                webkit6::UserContentInjectedFrames::TopFrame,
                webkit6::UserScriptInjectionTime::End,
                WORLD,
                &[],
                &[],
            ));
        }
        let settings = settings(defences, &self.sans);
        if !self.accelerated || defences.cpu_rendering {
            settings.set_hardware_acceleration_policy(webkit6::HardwareAccelerationPolicy::Never);
        }
        let mut builder = webkit6::WebView::builder()
            .user_content_manager(&ucm)
            .settings(&settings)
            .vexpand(false)
            .height_request(1);
        if let Some(csp) = &defences.csp {
            builder = builder.default_content_security_policy(csp.as_str());
        }
        let root = self.root.borrow().clone();
        builder = match root {
            Some(root) if self.share_process => builder.related_view(&root),
            _ => builder
                .web_context(&self.context)
                .network_session(&self.session),
        };
        let view = builder.build();
        view.set_valign(gtk4::Align::Start);
        if self.share_process && self.root.borrow().is_none() {
            self.root.replace(Some(view.clone()));
        }
        MessageView::wire(view, ucm, defences.navigation.clone(), defences.fit_height)
    }
}

fn serve_cid(
    request: &webkit6::URISchemeRequest,
    parts: &Rc<RefCell<PartMap>>,
    log: &Rc<RefCell<Vec<CidRequest>>>,
) {
    let uri = request.uri().map(String::from).unwrap_or_default();
    let cid = uri.get(4..).unwrap_or_default().to_owned();
    let page = request.web_view().map_or(0, |view| view.page_id());
    let part = parts
        .borrow()
        .get(&page)
        .and_then(|map| map.get(&cid))
        .cloned();
    log.borrow_mut().push(CidRequest {
        page,
        cid,
        found: part.is_some(),
    });
    let request = request.clone();
    glib::idle_add_local_once(move || match part {
        Some(part) => {
            let bytes = glib::Bytes::from_owned(part.data);
            let stream = gio::MemoryInputStream::from_bytes(&bytes);
            let response = webkit6::URISchemeResponse::new(&stream, bytes.len() as i64);
            response.set_content_type(part.content_type);
            request.finish_with_response(&response);
        }
        None => request.finish_error(&mut glib::Error::new(
            gio::IOErrorEnum::NotFound,
            "no such part in this message",
        )),
    });
}

#[allow(deprecated)]
fn settings(defences: &Defences, sans: &str) -> webkit6::Settings {
    let settings = webkit6::Settings::new();
    settings.set_enable_javascript(true);
    settings.set_enable_javascript_markup(defences.page_scripts);
    settings.set_auto_load_images(true);
    settings.set_default_font_family("sans-serif");
    settings.set_sans_serif_font_family(sans);
    settings.set_default_font_size(15);
    settings.set_print_backgrounds(true);
    if defences.hardened_settings {
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
        settings.set_enable_developer_extras(false);
        settings.set_enable_fullscreen(false);
        settings.set_enable_back_forward_navigation_gestures(false);
        settings.set_enable_site_specific_quirks(false);
        settings.set_javascript_can_open_windows_automatically(false);
        settings.set_javascript_can_access_clipboard(false);
        settings.set_allow_file_access_from_file_urls(false);
        settings.set_allow_universal_access_from_file_urls(false);
        settings.set_allow_top_navigation_to_data_urls(false);
        settings.set_allow_modal_dialogs(false);
        // WebKitGTK 2.52 ignores both ("deprecated and does nothing"); the runtime features
        // and link interception below do the job. Kept so the suite shows the difference.
        settings.set_enable_dns_prefetching(false);
        settings.set_enable_hyperlink_auditing(false);
    } else {
        settings.set_javascript_can_open_windows_automatically(true);
    }
    let switched_off: Vec<String> = std::env::var("MAIL_S2_FEATURES_OFF").map_or_else(
        |_| NETWORK_FEATURES.map(String::from).to_vec(),
        |list| list.split(',').map(String::from).collect(),
    );
    if defences.network_features_off
        && let Some(features) = webkit6::Settings::all_features()
    {
        for index in 0..features.length() {
            if let Some(feature) = features.get(index)
                && feature
                    .identifier()
                    .is_some_and(|id| switched_off.iter().any(|off| off == id.as_str()))
            {
                settings.set_feature_enabled(&feature, false);
            }
        }
    }
    settings
}

pub struct MessageView {
    pub view: webkit6::WebView,
    ucm: webkit6::UserContentManager,
    pub events: async_channel::Receiver<Stamped>,
    expecting_load: Rc<Cell<bool>>,
}

impl MessageView {
    fn wire(
        view: webkit6::WebView,
        ucm: webkit6::UserContentManager,
        navigation: Navigation,
        fit_height: bool,
    ) -> Self {
        let (sender, events) = async_channel::unbounded::<Stamped>();
        let send = Rc::new(move |event: Event| {
            let _ = sender.try_send(Stamped {
                at: Instant::now(),
                event,
            });
        });
        let expecting_load = Rc::new(Cell::new(false));

        ucm.connect_script_message_received(
            Some("height"),
            glib::clone!(
                #[weak]
                view,
                #[strong]
                send,
                move |_, value| {
                    let height = value.to_int32().clamp(1, MAX_VIEW_HEIGHT);
                    if fit_height {
                        view.set_size_request(-1, height);
                    }
                    send(Event::Height(height));
                }
            ),
        );
        ucm.connect_script_message_received(
            Some("frame"),
            glib::clone!(
                #[strong]
                send,
                move |_, _| send(Event::Frame)
            ),
        );
        ucm.connect_script_message_received(
            Some("link"),
            glib::clone!(
                #[strong]
                send,
                move |_, value| send(Event::Link(value.to_str().into()))
            ),
        );
        view.connect_load_changed(glib::clone!(
            #[strong]
            send,
            move |_, event| match event {
                webkit6::LoadEvent::Committed => send(Event::Committed),
                webkit6::LoadEvent::Finished => send(Event::Finished),
                _ => {}
            }
        ));
        view.connect_load_failed(glib::clone!(
            #[strong]
            send,
            move |_, _, uri, error| {
                send(Event::Failed(format!("{uri}: {error}")));
                false
            }
        ));
        view.connect_decide_policy(glib::clone!(
            #[strong]
            send,
            #[strong]
            expecting_load,
            move |_, decision, kind| {
                decide(decision, kind, &navigation, &expecting_load, &send);
                true
            }
        ));
        view.connect_create(glib::clone!(
            #[strong]
            send,
            move |_, action| {
                let uri = action
                    .clone()
                    .request()
                    .and_then(|request| request.uri())
                    .map(String::from)
                    .unwrap_or_default();
                send(Event::NewWindow(uri));
                None
            }
        ));
        view.connect_script_dialog(glib::clone!(
            #[strong]
            send,
            move |_, dialog| {
                send(Event::Dialog(
                    dialog.message().map(String::from).unwrap_or_default(),
                ));
                true
            }
        ));
        view.connect_resource_load_started(glib::clone!(
            #[strong]
            send,
            move |_, _, request| {
                send(Event::Resource(
                    request.uri().map(String::from).unwrap_or_default(),
                ));
            }
        ));
        view.connect_web_process_terminated(glib::clone!(
            #[strong]
            send,
            move |_, reason| send(Event::Crashed(format!("{reason:?}")))
        ));
        view.connect_permission_request(|_, request| {
            request.deny();
            true
        });
        Self {
            view,
            ucm,
            events,
            expecting_load,
        }
    }

    /// Loads `html` with the reader stylesheet `css` and the view background `background`.
    pub fn load(&self, html: &str, css: &str, background: &gdk::RGBA) {
        self.ucm.remove_all_style_sheets();
        self.ucm.add_style_sheet(&webkit6::UserStyleSheet::new(
            css,
            webkit6::UserContentInjectedFrames::AllFrames,
            webkit6::UserStyleLevel::User,
            &[],
            &[],
        ));
        self.view.set_background_color(background);
        self.expecting_load.set(true);
        self.view.load_html(html, None);
    }

    pub fn reset_height(&self) {
        self.view.set_size_request(-1, 1);
    }

    pub fn drain(&self) -> Vec<Stamped> {
        std::iter::from_fn(|| self.events.try_recv().ok()).collect()
    }

    /// Runs `body` (a function body) in Mail's isolated world.
    pub async fn eval(&self, body: &str) -> Result<javascriptcore::Value, glib::Error> {
        self.view
            .call_async_javascript_function_future(body, None, Some(WORLD), None)
            .await
    }
}

fn decide(
    decision: &webkit6::PolicyDecision,
    kind: PolicyDecisionType,
    navigation: &Navigation,
    expecting_load: &Cell<bool>,
    send: &Rc<impl Fn(Event)>,
) {
    match kind {
        PolicyDecisionType::NavigationAction | PolicyDecisionType::NewWindowAction => {
            let action = decision
                .downcast_ref::<webkit6::NavigationPolicyDecision>()
                .and_then(|decision| decision.navigation_action());
            let (uri, action_type) = action.map_or_else(
                || (String::new(), String::new()),
                |action| {
                    let uri = action
                        .request()
                        .and_then(|request| request.uri())
                        .map(String::from)
                        .unwrap_or_default();
                    (uri, format!("{:?}", action.navigation_type()))
                },
            );
            if kind == PolicyDecisionType::NavigationAction
                && expecting_load.get()
                && uri == "about:blank"
            {
                expecting_load.set(false);
                decision.use_();
                return;
            }
            let allowed = match navigation {
                Navigation::Block => false,
                Navigation::AllowOnly(prefixes) => prefixes
                    .iter()
                    .any(|prefix| uri.starts_with(prefix.as_str())),
            };
            send(Event::Navigation {
                uri,
                kind: format!("{kind:?}/{action_type}"),
                blocked: !allowed,
            });
            if allowed {
                decision.use_();
            } else {
                decision.ignore();
            }
        }
        PolicyDecisionType::Response => {
            let response = decision.downcast_ref::<webkit6::ResponsePolicyDecision>();
            if response.is_some_and(|response| !response.is_mime_type_supported()) {
                let uri = response
                    .and_then(|response| response.request())
                    .and_then(|request| request.uri())
                    .map(String::from)
                    .unwrap_or_default();
                send(Event::Download(uri));
                decision.ignore();
            } else {
                decision.use_();
            }
        }
        _ => decision.ignore(),
    }
}
