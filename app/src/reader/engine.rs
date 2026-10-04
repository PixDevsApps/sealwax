//! WebKit for the reader (ADR-011 with its S2 and S7 amendments; SECURITY.md "Message
//! rendering"): one web context with an ephemeral network session for the whole process, the
//! compiled content filter, the `cid:` scheme answered from each view's own message, and a pool
//! of hardened views that all share one web process (`related-view`). At most
//! [`MAX_LIVE_VIEWS`] views exist at a time (ADR-011's memory decision).

use super::view::MessageView;
use gtk4::{gio, glib};
use sealwax_domain::theme::WebViewFonts;
use sealwax_infrastructure::xdg::ensure_private_dir;
use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use webkit6::prelude::*;

/// The construction-time policy (ADR-011, S2 amendment 4): nothing but `cid:`, `data:` and,
/// once the user allows them, `https:` images; inline styles; `cid:` and `data:` fonts. Plain
/// http image URLs are upgraded, so allowed images never travel in clear text.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; img-src cid: data: https:; \
     style-src 'unsafe-inline'; font-src cid: data:; form-action 'none'; base-uri 'none'; \
     frame-src 'none'; upgrade-insecure-requests";

/// Runtime features that make requests without any script and that the old settings no
/// longer control in WebKitGTK 2.52 (S2): DNS prefetch, a clicked link's preconnect,
/// `<link rel=prefetch|preload>` and speculation rules.
pub const NETWORK_FEATURES: [&str; 5] = [
    "LinkDNSPrefetch",
    "LinkPreconnect",
    "LinkPrefetch",
    "LinkPreload",
    "SpeculationRulesPrefetch",
];

/// The most views that exist at once (ADR-011 amendment, memory bar decided; M5 sets the final
/// number from its measurement).
pub const MAX_LIVE_VIEWS: usize = 4;

/// Idle views kept ready, blank, so the next message doesn't wait for a new page.
const IDLE_LIMIT: usize = 2;

/// Blocks every load except `cid:`, `data:` and `about:` until the user allows remote images
/// for a message. WebKit's filter regexes have no alternation, so each exception is a rule.
const FILTER_JSON: &str = r#"[
  {"trigger": {"url-filter": ".*"}, "action": {"type": "block"}},
  {"trigger": {"url-filter": "^cid:"}, "action": {"type": "ignore-previous-rules"}},
  {"trigger": {"url-filter": "^data:"}, "action": {"type": "ignore-previous-rules"}},
  {"trigger": {"url-filter": "^about:"}, "action": {"type": "ignore-previous-rules"}}
]"#;
const FILTER_ID: &str = "sealwax-block-remote";

/// One inline part a view's message may load through `cid:`.
#[derive(Debug, Clone)]
pub struct InlinePart {
    pub mime_type: String,
    pub data: glib::Bytes,
}

/// A message's inline parts by Content-ID (without angle brackets).
#[derive(Debug, Clone, Default)]
pub struct InlineParts {
    parts: HashMap<String, InlinePart>,
}

impl InlineParts {
    pub fn insert(&mut self, content_id: String, part: InlinePart) {
        self.parts.insert(content_id, part);
    }

    pub fn len(&self) -> usize {
        self.parts.len()
    }

    /// The part a `cid:` URL names: exactly, else ignoring ASCII case, as some mailers change
    /// it between the header and the HTML.
    pub fn find(&self, content_id: &str) -> Option<&InlinePart> {
        self.parts.get(content_id).or_else(|| {
            self.parts
                .iter()
                .find(|(id, _)| id.eq_ignore_ascii_case(content_id))
                .map(|(_, part)| part)
        })
    }
}

/// What a `cid:` request found, for the hostile suite's checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CidRequest {
    pub page: u64,
    pub found: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Filter {
    Compiling,
    Ready,
    Failed(String),
}

struct Pool {
    idle: Vec<MessageView>,
    live: usize,
}

pub struct Engine {
    context: webkit6::WebContext,
    session: webkit6::NetworkSession,
    filter: RefCell<Option<webkit6::UserContentFilter>>,
    filter_state: Rc<RefCell<Filter>>,
    /// Closed once the filter compiled or failed; waiters see the channel close.
    filter_done: async_channel::Receiver<()>,
    parts: Rc<RefCell<HashMap<u64, Rc<InlineParts>>>>,
    cid_log: Rc<RefCell<Vec<CidRequest>>>,
    downloads: Rc<RefCell<u32>>,
    /// The first view: every later view is built with it as `related-view`, so all of them
    /// share its web process. Kept for the life of the process.
    root: RefCell<Option<webkit6::WebView>>,
    pool: RefCell<Pool>,
}

thread_local! {
    static ENGINE: OnceCell<Rc<Engine>> = const { OnceCell::new() };
}

impl Engine {
    /// The process's engine, created on first use. The content filter compiles into
    /// `cache/content-filters` (Sealwax's cache folder) in the background.
    pub fn shared(cache: &Path) -> Rc<Engine> {
        ENGINE.with(|engine| engine.get_or_init(|| Engine::new(cache)).clone())
    }

    fn new(cache: &Path) -> Rc<Engine> {
        let context = webkit6::WebContext::new();
        context.set_cache_model(webkit6::CacheModel::DocumentViewer);
        let session = webkit6::NetworkSession::new_ephemeral();
        session.set_tls_errors_policy(webkit6::TLSErrorsPolicy::Fail);
        if let Some(cookies) = session.cookie_manager() {
            cookies.set_accept_policy(webkit6::CookieAcceptPolicy::Never);
        }
        let downloads: Rc<RefCell<u32>> = Rc::default();
        session.connect_download_started(glib::clone!(
            #[strong]
            downloads,
            move |_, download| {
                download.cancel();
                *downloads.borrow_mut() += 1;
                tracing::debug!("reader: a download was refused");
            }
        ));
        let parts: Rc<RefCell<HashMap<u64, Rc<InlineParts>>>> = Rc::default();
        let cid_log: Rc<RefCell<Vec<CidRequest>>> = Rc::default();
        context.register_uri_scheme(
            "cid",
            glib::clone!(
                #[strong]
                parts,
                #[strong]
                cid_log,
                move |request| serve_cid(request, &parts, &cid_log)
            ),
        );
        let (done, filter_done) = async_channel::bounded(1);
        let engine = Rc::new(Engine {
            context,
            session,
            filter: RefCell::new(None),
            filter_state: Rc::new(RefCell::new(Filter::Compiling)),
            filter_done,
            parts,
            cid_log,
            downloads,
            root: RefCell::new(None),
            pool: RefCell::new(Pool {
                idle: Vec::new(),
                live: 0,
            }),
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
                            "reader: the content filter did not compile, so HTML is shown as \
                             text: {error}"
                        );
                        engine.filter_state.replace(Filter::Failed(error));
                    }
                }
            }
            drop(done);
        });
        engine
    }

    /// The compiled content filter, once ready; `None` when it couldn't be compiled, in which
    /// case no HTML may be shown in a WebView.
    pub async fn filter(&self) -> Option<webkit6::UserContentFilter> {
        if *self.filter_state.borrow() == Filter::Compiling {
            let _ = self.filter_done.recv().await;
        }
        self.filter.borrow().clone()
    }

    pub fn context(&self) -> &webkit6::WebContext {
        &self.context
    }

    pub fn session(&self) -> &webkit6::NetworkSession {
        &self.session
    }

    /// The inline parts `page` (a view's `page_id`) may load; any other `cid:` it asks for fails.
    pub fn set_parts(&self, page: u64, parts: Rc<InlineParts>) {
        self.parts.borrow_mut().insert(page, parts);
    }

    pub fn clear_parts(&self, page: u64) {
        self.parts.borrow_mut().remove(&page);
    }

    /// Every `cid:` request so far (page and whether it was served), for the hostile suite.
    pub fn cid_requests(&self) -> Vec<CidRequest> {
        self.cid_log.borrow().clone()
    }

    /// Downloads refused so far.
    pub fn refused_downloads(&self) -> u32 {
        *self.downloads.borrow()
    }

    /// Views alive now, idle ones included.
    pub fn live_views(&self) -> usize {
        self.pool.borrow().live
    }

    /// A blank view for a message: an idle one, or a new one while fewer than
    /// [`MAX_LIVE_VIEWS`] exist. `None` at the cap.
    pub fn acquire(self: &Rc<Self>, fonts: &WebViewFonts) -> Option<MessageView> {
        let idle = self.pool.borrow_mut().idle.pop();
        if let Some(view) = idle {
            view.set_fonts(fonts);
            return Some(view);
        }
        if self.pool.borrow().live >= MAX_LIVE_VIEWS {
            tracing::warn!(cap = MAX_LIVE_VIEWS, "reader: every live view is in use");
            return None;
        }
        let root = self.root.borrow().clone();
        let view = MessageView::new(self, fonts, root.as_ref());
        if root.is_none() {
            self.root.replace(Some(view.web_view().clone()));
        }
        self.pool.borrow_mut().live += 1;
        tracing::debug!(live = self.pool.borrow().live, "reader: new view");
        Some(view)
    }

    /// Takes a view back: its page is blanked (the message's DOM and surfaces go), its parts
    /// forgotten, and it waits idle, or is destroyed when enough views already wait. The root
    /// view is never destroyed: later views share its web process.
    pub fn release(&self, view: MessageView) {
        self.clear_parts(view.page_id());
        let filter = self.filter.borrow().clone();
        view.blank(filter.as_ref());
        let is_root = self
            .root
            .borrow()
            .as_ref()
            .is_some_and(|root| root == view.web_view());
        let mut pool = self.pool.borrow_mut();
        if is_root || pool.idle.len() < IDLE_LIMIT {
            pool.idle.push(view);
        } else {
            pool.live -= 1;
            drop(pool);
            view.destroy();
            tracing::debug!(live = self.live_views(), "reader: view destroyed");
        }
    }

    /// Starts the web process ahead of the first message: one idle view with a blank page.
    pub async fn prewarm(self: &Rc<Self>, fonts: &WebViewFonts) {
        let filter = self.filter().await;
        if !self.pool.borrow().idle.is_empty() {
            return;
        }
        if let Some(view) = self.acquire(fonts) {
            view.blank(filter.as_ref());
            self.pool.borrow_mut().idle.push(view);
        }
    }
}

async fn compile_filter(directory: &Path) -> Result<webkit6::UserContentFilter, String> {
    ensure_private_dir(directory).map_err(|error| error.to_string())?;
    let store = webkit6::UserContentFilterStore::new(&directory.to_string_lossy());
    store
        .save_future(FILTER_ID, &glib::Bytes::from_static(FILTER_JSON.as_bytes()))
        .await
        .map_err(|error| error.to_string())
}

/// Serves `cid:<content-id>` from the parts of the message loaded in the requesting view, and
/// nothing else: a part of another message, or of an earlier message in the same view, fails.
fn serve_cid(
    request: &webkit6::URISchemeRequest,
    parts: &Rc<RefCell<HashMap<u64, Rc<InlineParts>>>>,
    log: &Rc<RefCell<Vec<CidRequest>>>,
) {
    let uri = request.uri().map(String::from).unwrap_or_default();
    let content_id = percent_decode(uri.get(4..).unwrap_or_default());
    let page = request.web_view().map_or(0, |view| view.page_id());
    let part = parts
        .borrow()
        .get(&page)
        .and_then(|parts| parts.find(&content_id).cloned());
    log.borrow_mut().push(CidRequest {
        page,
        found: part.is_some(),
    });
    let request = request.clone();
    glib::idle_add_local_once(move || match part {
        Some(part) => {
            let length = part.data.len() as i64;
            let stream = gio::MemoryInputStream::from_bytes(&part.data);
            let response = webkit6::URISchemeResponse::new(&stream, length);
            response.set_content_type(&part.mime_type);
            request.finish_with_response(&response);
        }
        None => request.finish_error(&mut glib::Error::new(
            gio::IOErrorEnum::NotFound,
            "no such part in this message",
        )),
    });
}

/// `%xx` escapes decoded (RFC 2392 `cid:` URLs escape the Content-ID); invalid ones stay.
pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = |byte: u8| char::from(byte).to_digit(16);
        match (bytes[at], bytes.get(at + 1), bytes.get(at + 2)) {
            (b'%', Some(&high), Some(&low)) if hex(high).is_some() && hex(low).is_some() => {
                out.push((hex(high).unwrap_or(0) * 16 + hex(low).unwrap_or(0)) as u8);
                at += 3;
            }
            (byte, _, _) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cid_urls_are_percent_decoded() {
        assert_eq!(percent_decode("logo@example.test"), "logo@example.test");
        assert_eq!(percent_decode("image001.png%4001D9"), "image001.png@01D9");
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("%zz%41"), "%zzA");
        assert_eq!(percent_decode("%C3%A6"), "æ");
    }

    #[test]
    fn content_ids_match_exactly_then_ignoring_case() {
        let mut parts = InlineParts::default();
        let part = |text: &str| InlinePart {
            mime_type: "image/png".into(),
            data: glib::Bytes::from(text.as_bytes()),
        };
        parts.insert("Logo@Example.test".into(), part("a"));
        parts.insert("logo@example.test".into(), part("b"));
        assert_eq!(parts.find("logo@example.test").unwrap().data.as_ref(), b"b");
        assert_eq!(parts.find("Logo@Example.test").unwrap().data.as_ref(), b"a");
        assert!(parts.find("LOGO@EXAMPLE.TEST").is_some());
        assert!(parts.find("other@example.test").is_none());
        assert_eq!(parts.len(), 2);
    }

    #[test]
    fn the_policy_and_filter_are_adr_011s() {
        for directive in [
            "default-src 'none'",
            "img-src cid: data: https:",
            "style-src 'unsafe-inline'",
            "font-src cid: data:",
            "form-action 'none'",
            "base-uri 'none'",
            "frame-src 'none'",
            "upgrade-insecure-requests",
        ] {
            assert!(CONTENT_SECURITY_POLICY.contains(directive), "{directive}");
        }
        assert!(!CONTENT_SECURITY_POLICY.contains("script-src"));
        assert!(FILTER_JSON.contains(r#""url-filter": ".*"}, "action": {"type": "block"}"#));
        for scheme in ["cid", "data", "about"] {
            assert!(FILTER_JSON.contains(&format!(r#""url-filter": "^{scheme}:""#)));
        }
        assert!(
            !FILTER_JSON.contains('|'),
            "WebKit's filter regexes reject alternation"
        );
    }
}
