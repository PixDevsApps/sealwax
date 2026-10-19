//! WebKit for the reader (ADR-011 with its S2 and S7 amendments; SECURITY.md "Message
//! rendering"): one web context with an ephemeral network session for the whole process, the
//! compiled content filters, the `cid:` scheme answered from each view's own message, and a pool
//! of hardened views that all share one web process (`related-view`). At most
//! [`MAX_LIVE_VIEWS`] views exist at a time (ADR-011's memory decision).
//!
//! `cid:` requests are answered asynchronously (ML-060): from the parts `prepare` decoded, or,
//! for a part the store doesn't hold, from the [`PartLoader`]. An answer is given only while
//! its view still shows the load that asked; a view released, blanked or loaded again in the
//! meantime gets its request ended unanswered.

use super::view::MessageView;
use crate::part_cache;
use gtk4::{gio, glib};
use sealwax_domain::model::MessageId;
use sealwax_domain::theme::WebViewFonts;
use sealwax_infrastructure::xdg::ensure_private_dir;
use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
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
/// `<link rel=prefetch|preload>` and speculation rules. Each is turned off where this WebKit
/// still has it.
pub const NETWORK_FEATURES: [&str; 5] = [
    "LinkDNSPrefetch",
    "LinkPreconnect",
    "LinkPrefetch",
    "LinkPreload",
    "SpeculationRulesPrefetch",
];

/// The [`NETWORK_FEATURES`] WebKit dropped, with the first WebKitGTK (major, minor) without
/// each. 2.54 has no switch for `<link rel=dns-prefetch>` any more (310198@main): it always
/// runs there, and only the pings filter stops it.
pub const RETIRED_FEATURES: [(&str, (u32, u32)); 1] = [("LinkDNSPrefetch", (2, 54))];

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

/// Blocks what WebKit checks as a ping in every view, remote images allowed or not: `<a ping>`,
/// `<link rel=dns-prefetch>` and `<link rel=preconnect>`. No CSP governs the last two, and
/// WebKitGTK 2.54 can't turn DNS prefetch off.
const PINGS_JSON: &str = r#"[
  {"trigger": {"url-filter": ".*", "resource-type": ["ping"]}, "action": {"type": "block"}}
]"#;
const PINGS_ID: &str = "sealwax-block-pings";

/// One inline part a view's message may load through `cid:`.
#[derive(Debug, Clone)]
pub struct InlinePart {
    pub mime_type: String,
    pub data: glib::Bytes,
}

/// Where an inline part comes from.
#[derive(Debug, Clone)]
enum Source {
    /// Decoded from the stored message.
    Ready(InlinePart),
    /// Not stored (a large message kept as its text parts, ML-044): the [`PartLoader`] fetches
    /// part `part` (a mail-parser index) of the message when the body asks for it; it is served
    /// as the type the stored message declares.
    Fetch { part: u32, mime_type: String },
}

/// A message's inline parts by Content-ID (without angle brackets).
#[derive(Debug, Clone, Default)]
pub struct InlineParts {
    message: Option<MessageId>,
    parts: HashMap<String, Source>,
}

impl InlineParts {
    pub fn new(message: MessageId) -> Self {
        Self {
            message: Some(message),
            parts: HashMap::new(),
        }
    }

    pub fn insert(&mut self, content_id: String, part: InlinePart) {
        self.parts.insert(content_id, Source::Ready(part));
    }

    /// A part the store doesn't hold, fetched by the [`PartLoader`] when the body asks for it.
    pub fn insert_missing(&mut self, content_id: String, part: u32, mime_type: String) {
        self.parts
            .insert(content_id, Source::Fetch { part, mime_type });
    }

    pub fn len(&self) -> usize {
        self.parts.len()
    }

    /// Some part must come from the server: not stored, and not kept in [`part_cache`] since
    /// it was last downloaded.
    pub fn waits_for_server(&self) -> bool {
        let Some(message) = self.message else {
            return false;
        };
        self.parts.values().any(|source| {
            matches!(source, Source::Fetch { part, .. } if !part_cache::contains(message, *part))
        })
    }

    /// Parts left to the [`PartLoader`].
    #[cfg(test)]
    pub fn missing(&self) -> usize {
        self.parts
            .values()
            .filter(|source| matches!(source, Source::Fetch { .. }))
            .count()
    }

    /// The part a `cid:` URL names: exactly, else ignoring ASCII case, as some mailers change
    /// it between the header and the HTML.
    fn find(&self, content_id: &str) -> Option<&Source> {
        self.parts.get(content_id).or_else(|| {
            self.parts
                .iter()
                .find(|(id, _)| id.eq_ignore_ascii_case(content_id))
                .map(|(_, part)| part)
        })
    }

    /// A decoded part by Content-ID, for checks.
    #[cfg(test)]
    pub fn ready(&self, content_id: &str) -> Option<&InlinePart> {
        match self.find(content_id)? {
            Source::Ready(part) => Some(part),
            Source::Fetch { .. } => None,
        }
    }
}

/// Fetches the decoded bytes of part `part` (a mail-parser index) of a message the store keeps
/// as its text parts: in the app, `Command::FetchPart`. `None` when it can't be had.
pub type PartLoader =
    Rc<dyn Fn(MessageId, u32) -> Pin<Box<dyn Future<Output = Option<glib::Bytes>>>>>;

/// What became of a `cid:` request, for the hostile suite and the self-tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CidOutcome {
    /// Not answered yet: a part being fetched.
    Pending,
    Served,
    /// No such part in the requesting view's message, or it couldn't be fetched.
    NotFound,
    /// The view moved on (released, blanked or loaded again) before the answer was ready, so
    /// the request ended unanswered.
    Dropped,
}

/// A `cid:` request, for checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CidRequest {
    pub page: u64,
    /// The requesting view's message has a part by that Content-ID.
    pub found: bool,
    pub outcome: CidOutcome,
}

/// The parts each view may load, with the load they belong to: a new set of parts (another
/// message, or the same one loaded again) starts a new epoch, and an answer is given only in
/// the epoch of its request.
#[derive(Default)]
struct Pages {
    parts: HashMap<u64, (u64, Rc<InlineParts>)>,
    epochs: u64,
}

/// What [`Pages::lookup`] finds: the epoch, the part's source and the message.
type Found = (u64, Option<Source>, Option<MessageId>);

impl Pages {
    fn set(&mut self, page: u64, parts: Rc<InlineParts>) -> u64 {
        self.epochs += 1;
        self.parts.insert(page, (self.epochs, parts));
        self.epochs
    }

    fn clear(&mut self, page: u64) {
        self.parts.remove(&page);
    }

    fn lookup(&self, page: u64, content_id: &str) -> Option<Found> {
        let (epoch, parts) = self.parts.get(&page)?;
        Some((*epoch, parts.find(content_id).cloned(), parts.message))
    }

    fn current(&self, page: u64, epoch: u64) -> bool {
        self.parts.get(&page).is_some_and(|(now, _)| *now == epoch)
    }
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
    /// In every view, whatever `filter` does.
    pings: RefCell<Option<webkit6::UserContentFilter>>,
    filter_state: Rc<RefCell<Filter>>,
    /// Closed once the filters compiled or failed; waiters see the channel close.
    filter_done: async_channel::Receiver<()>,
    pages: Rc<RefCell<Pages>>,
    loader: Rc<RefCell<Option<PartLoader>>>,
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
    /// The process's engine, created on first use. The content filters compile into
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
        let pages: Rc<RefCell<Pages>> = Rc::default();
        let loader: Rc<RefCell<Option<PartLoader>>> = Rc::default();
        let cid_log: Rc<RefCell<Vec<CidRequest>>> = Rc::default();
        context.register_uri_scheme(
            "cid",
            glib::clone!(
                #[strong]
                pages,
                #[strong]
                loader,
                #[strong]
                cid_log,
                move |request| serve_cid(request, &pages, &loader, &cid_log)
            ),
        );
        let (done, filter_done) = async_channel::bounded(1);
        let engine = Rc::new(Engine {
            context,
            session,
            filter: RefCell::new(None),
            pings: RefCell::new(None),
            filter_state: Rc::new(RefCell::new(Filter::Compiling)),
            filter_done,
            pages,
            loader,
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
            let compiled = async {
                let filter = compile_filter(&directory, FILTER_ID, FILTER_JSON).await?;
                let pings = compile_filter(&directory, PINGS_ID, PINGS_JSON).await?;
                Ok::<_, String>((filter, pings))
            }
            .await;
            if let Some(engine) = weak.upgrade() {
                match compiled {
                    Ok((filter, pings)) => {
                        engine.filter.replace(Some(filter));
                        engine.pings.replace(Some(pings));
                        engine.filter_state.replace(Filter::Ready);
                    }
                    Err(error) => {
                        tracing::error!(
                            "reader: a content filter did not compile, so HTML is shown as \
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

    /// The compiled content filter, once ready; `None` when it or the pings filter couldn't be
    /// compiled, in which case no HTML may be shown in a WebView.
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

    /// The inline parts `page` (a view's `page_id`) may load from now on; any other `cid:` it
    /// asks for fails, and requests of its earlier loads end unanswered.
    pub fn set_parts(&self, page: u64, parts: Rc<InlineParts>) {
        self.pages.borrow_mut().set(page, parts);
    }

    pub fn clear_parts(&self, page: u64) {
        self.pages.borrow_mut().clear(page);
    }

    /// Where parts the store doesn't hold come from (phase B: `Command::FetchPart`). Without
    /// one, they don't load.
    pub fn set_part_loader(&self, loader: Option<PartLoader>) {
        self.loader.replace(loader);
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
    /// [`MAX_LIVE_VIEWS`] exist. `None` at the cap, and before the content filters compiled.
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
        let Some(pings) = self.pings.borrow().clone() else {
            tracing::warn!("reader: no view without the content filters");
            return None;
        };
        let root = self.root.borrow().clone();
        let view = MessageView::new(self, fonts, root.as_ref(), pings);
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

async fn compile_filter(
    directory: &Path,
    id: &str,
    json: &'static str,
) -> Result<webkit6::UserContentFilter, String> {
    ensure_private_dir(directory).map_err(|error| error.to_string())?;
    let store = webkit6::UserContentFilterStore::new(&directory.to_string_lossy());
    store
        .save_future(id, &glib::Bytes::from_static(json.as_bytes()))
        .await
        .map_err(|error| error.to_string())
}

/// Serves `cid:<content-id>` from the parts of the message loaded in the requesting view, and
/// nothing else: a part of another message, or of an earlier message in the same view, fails.
/// The answer comes from the main loop, never inside WebKit's call: a decoded part at once,
/// a part the store doesn't hold once the loader has it. If the view has moved on by then,
/// the request ends unanswered.
fn serve_cid(
    request: &webkit6::URISchemeRequest,
    pages: &Rc<RefCell<Pages>>,
    loader: &Rc<RefCell<Option<PartLoader>>>,
    log: &Rc<RefCell<Vec<CidRequest>>>,
) {
    let uri = request.uri().map(String::from).unwrap_or_default();
    let content_id = percent_decode(uri.get(4..).unwrap_or_default());
    let page = request.web_view().map_or(0, |view| view.page_id());
    let found = pages.borrow().lookup(page, &content_id);
    let (epoch, source, message) = found.unwrap_or((0, None, None));
    let entry = {
        let mut log = log.borrow_mut();
        log.push(CidRequest {
            page,
            found: source.is_some(),
            outcome: CidOutcome::Pending,
        });
        log.len() - 1
    };
    let loader = loader.borrow().clone();
    let fetch = match (&source, message, loader) {
        (Some(Source::Fetch { part, mime_type }), Some(message), Some(loader)) => {
            Some((loader(message, *part), mime_type.clone()))
        }
        _ => None,
    };
    let request = request.clone();
    let pages = pages.clone();
    let log = log.clone();
    glib::spawn_future_local(async move {
        let part = match (source, fetch) {
            (Some(Source::Ready(part)), _) => Some(part),
            (_, Some((fetch, mime_type))) => fetch.await.map(|data| InlinePart { mime_type, data }),
            _ => None,
        };
        let outcome = if !pages.borrow().current(page, epoch) {
            request.finish_error(&mut glib::Error::new(
                gio::IOErrorEnum::Cancelled,
                "the view shows another load",
            ));
            tracing::debug!(
                page,
                "reader: a cid: answer for a view that moved on was dropped"
            );
            CidOutcome::Dropped
        } else if let Some(part) = part {
            let length = part.data.len() as i64;
            let stream = gio::MemoryInputStream::from_bytes(&part.data);
            let response = webkit6::URISchemeResponse::new(&stream, length);
            response.set_content_type(&part.mime_type);
            request.finish_with_response(&response);
            CidOutcome::Served
        } else {
            request.finish_error(&mut glib::Error::new(
                gio::IOErrorEnum::NotFound,
                "no such part in this message",
            ));
            CidOutcome::NotFound
        };
        if let Some(entry) = log.borrow_mut().get_mut(entry) {
            entry.outcome = outcome;
        }
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
        assert_eq!(
            parts.ready("logo@example.test").unwrap().data.as_ref(),
            b"b"
        );
        assert_eq!(
            parts.ready("Logo@Example.test").unwrap().data.as_ref(),
            b"a"
        );
        assert!(parts.find("LOGO@EXAMPLE.TEST").is_some());
        assert!(parts.find("other@example.test").is_none());
        assert_eq!(parts.len(), 2);
    }

    #[test]
    fn answers_belong_to_the_load_that_asked() {
        let mut first = InlineParts::new(MessageId(1));
        first.insert(
            "image001.png@01D9".into(),
            InlinePart {
                mime_type: "image/png".into(),
                data: glib::Bytes::from_static(b"png"),
            },
        );
        let mut second = InlineParts::new(MessageId(2));
        second.insert_missing("image001.png@01D9".into(), 3, "image/png".into());
        assert_eq!((second.len(), second.missing()), (1, 1));
        let mut pages = Pages::default();
        let epoch = pages.set(7, Rc::new(first));
        let (asked, source, message) = pages.lookup(7, "image001.png@01D9").unwrap();
        assert_eq!((asked, message), (epoch, Some(MessageId(1))));
        assert!(matches!(source, Some(Source::Ready(_))));
        assert!(pages.current(7, asked));
        let again = pages.set(7, Rc::new(second));
        assert!(
            !pages.current(7, asked),
            "the view shows another message now"
        );
        let (_, source, message) = pages.lookup(7, "IMAGE001.PNG@01d9").unwrap();
        assert!(
            matches!(source, Some(Source::Fetch { part: 3, mime_type }) if mime_type == "image/png")
        );
        assert_eq!(message, Some(MessageId(2)));
        assert!(
            pages.lookup(8, "image001.png@01D9").is_none(),
            "another view"
        );
        pages.clear(7);
        assert!(!pages.current(7, again), "released");
        assert!(pages.lookup(7, "image001.png@01D9").is_none());
    }

    #[test]
    fn only_parts_neither_stored_nor_kept_wait_for_the_server() {
        let mut stored = InlineParts::new(MessageId(41));
        stored.insert(
            "logo@example.test".into(),
            InlinePart {
                mime_type: "image/png".into(),
                data: glib::Bytes::from_static(b"png"),
            },
        );
        assert!(!stored.waits_for_server());
        let mut large = InlineParts::new(MessageId(42));
        large.insert_missing("chart@example.test".into(), 4, "image/png".into());
        assert!(large.waits_for_server());
        part_cache::put(MessageId(42), 4, b"png");
        assert!(!large.waits_for_server(), "kept since it was downloaded");
        part_cache::forget(&[MessageId(42)]);
        assert!(large.waits_for_server());
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
        assert!(PINGS_JSON.contains(
            r#"{"trigger": {"url-filter": ".*", "resource-type": ["ping"]}, "action": {"type": "block"}}"#
        ));
        assert!(
            RETIRED_FEATURES
                .iter()
                .all(|(name, _)| NETWORK_FEATURES.contains(name)),
            "a retired feature is still turned off where WebKit has it"
        );
    }
}
