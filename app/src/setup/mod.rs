//! The setup assistant (ML-033; DESIGN.md "Setup assistant"; the approved mock's Setup
//! artboard): one column in a dialog over the window. The address and the sender name, then
//! "Looking up your provider…" with the sources as the engine reports them, then the password
//! (an app password where the provider wants one) or Microsoft sign-in, the login check, and the
//! account saved, after which the dialog closes and the store shows the account. "Set up
//! manually" opens host, port, security and username for IMAP and SMTP.
//!
//! A POP3 account (M8) is added only there: "Incoming mail" chooses IMAP (the default) or POP3,
//! which names the incoming server POP3's, moves its standard ports along (995 and 110) and
//! says what POP3 does with mail. Autodetect stays IMAP. The account is saved with the safe
//! policy, mail left on the server and never deleted there; `accounts.toml` changes it.
//!
//! It talks to the engine only through `engine::api` values on the [`EngineLink`]. Every reply
//! names its request, and a reply to a request the user walked away from is ignored. The
//! password is wrapped in `Password` as soon as it is read, and nothing typed is logged.
//! Enter advances, Escape goes back a step (and closes on the first), and each step puts the
//! focus in its first field that needs typing.
//!
//! Microsoft sign-in (ML-032) runs in the browser: the engine sends the page to open, the
//! assistant opens it (and again on request) and waits with a Cancel, after a minute with a note
//! on the admin approval a work account's organization may need, and the username the sign-in
//! belongs to is the one saved. The window's banner opens the assistant straight at that
//! step for an account that must sign in again. Neither the page's address nor the username
//! is logged.
//!
//! A server on this computer whose certificate the system doesn't trust (Proton Mail Bridge,
//! issue #9) gets a trust step after the check fails on it: the engine reads the certificate,
//! the assistant shows its fingerprint, and trusting it checks again with that certificate
//! pinned. The same certificate on another port of the same host (Bridge's SMTP) is trusted
//! without asking again.
//!
//! A password account the server refused (ML-048) gets the password step on its own: the
//! banner opens the assistant with the account's address read only, the engine checks the
//! password on IMAP and SMTP before it keeps it, and a refusal says why as the login check's
//! failures do.

pub mod browser;
mod colors;
pub mod edit;
pub mod form;
pub mod remove;
pub mod sources;
pub mod text;
mod view;

use crate::engine_link::{EngineLink, Subscription};
use crate::window::Window;
use form::{Field, Problem};
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::account::{
    AccountColor, AccountKey, AccountSettings, AuthMethod, CertificatePin, DetectedSettings,
    Detection, Endpoint, HelpLink, LoginError, LoginFailure, OAuthProvider, Pop3Policy, Protocol,
    Receive, Security, is_loopback,
};
use sealwax_domain::autoconfig::{Finding, Hints, address_domain, presets};
use sealwax_domain::model::AccountId;
use sealwax_engine::api::{AccountError, Command, Event, Password, RequestId};
use sources::Sources;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;
use text::Notice;
use view::Widgets;

/// The mock's card: a third of its 1440 × 820 artboard, less its margins.
const CONTENT_WIDTH: i32 = 428;
const CONTENT_HEIGHT: i32 = 740;

/// How long a Microsoft sign-in waits for the browser before the note on admin approval shows.
const STILL_WAITING_AFTER: Duration = Duration::from_secs(60);

thread_local! {
    /// [`STILL_WAITING_AFTER`], or a shorter wait the self-tests set.
    static STILL_WAITING_WAIT: Cell<Duration> = const { Cell::new(STILL_WAITING_AFTER) };
}

/// Changes how long a sign-in waits before the note on admin approval shows, for tests.
pub fn set_still_waiting_after(wait: Duration) {
    STILL_WAITING_WAIT.with(|current| current.set(wait));
}

/// Where the assistant is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Step {
    /// The address and the sender name.
    #[default]
    Address,
    /// Waiting for `Detected`, the sources listed as they start.
    Detecting,
    /// Settings found for a password: the settings, the username where it must be typed or
    /// confirmed, and the password.
    Password,
    /// Settings found for Microsoft sign-in, or an account to sign in again: the button that
    /// starts it, or how to make it available without a client ID.
    Microsoft,
    /// Waiting for `SignedIn` while the browser signs in.
    SigningIn,
    /// Signed in to Microsoft as another address than the one typed (issue #2): send from
    /// the one signed in with, or keep the one typed.
    Identity,
    /// A password account the server refused (ML-048): its password, typed again.
    PasswordAgain,
    /// Nothing to sign in to: a provider without IMAP, or a domain without mail.
    Blocked,
    /// Host, port, security and username for IMAP and SMTP, and the password.
    Manual,
    /// Waiting for `Verified`, or for `CertificateRead` after a server on this computer showed
    /// a certificate the system doesn't trust.
    Checking,
    /// That certificate's fingerprint: trust it and check again, or don't (issue #9).
    Certificate,
    /// The login failed: why, the password and the settings.
    Failed,
    /// Waiting for `AccountAdded`.
    Adding,
}

impl Step {
    pub fn name(self) -> &'static str {
        match self {
            Self::Address => "address",
            Self::Detecting => "detecting",
            Self::Password => "password",
            Self::Microsoft => "microsoft",
            Self::SigningIn => "signing-in",
            Self::Identity => "identity",
            Self::PasswordAgain => "password-again",
            Self::Blocked => "blocked",
            Self::Manual => "manual",
            Self::Checking => "checking",
            Self::Certificate => "certificate",
            Self::Failed => "failed",
            Self::Adding => "adding",
        }
    }

    /// A screen of its own, which Back returns to.
    fn is_screen(self) -> bool {
        matches!(
            self,
            Self::Address
                | Self::Password
                | Self::Microsoft
                | Self::Identity
                | Self::Blocked
                | Self::Manual
                | Self::PasswordAgain
                | Self::Certificate
        )
    }

    fn is_busy(self) -> bool {
        matches!(
            self,
            Self::Detecting | Self::Checking | Self::SigningIn | Self::Adding
        )
    }
}

/// The request the assistant waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Detect(RequestId),
    Verify(RequestId),
    /// `ReadCertificate` after a check failed on a server on this computer.
    Read(RequestId),
    SignIn(RequestId),
    Add(RequestId),
    Update(RequestId),
}

/// An account that must sign in again (the window's banner): what `SignIn` and
/// `UpdatePassword` name, and what the assistant shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Again {
    pub key: AccountKey,
    pub name: String,
    pub address: String,
    /// The account's servers, which a refused password's message names: the incoming one is
    /// IMAP's or POP3's, as `receive` says.
    pub incoming: Endpoint,
    pub receive: Receive,
    pub smtp: Endpoint,
}

/// A server on this computer that showed a certificate the system doesn't trust, while the trust
/// step asks.
#[derive(Debug, Clone)]
struct Untrusted {
    protocol: Protocol,
    endpoint: Endpoint,
    pin: CertificatePin,
    /// The check's failure, shown when the certificate isn't trusted.
    error: LoginError,
    /// The screen the check started from.
    from: Step,
}

/// Whether Microsoft sign-in can start: a client ID is in use
/// ([`crate::oauth::microsoft_client_id`]). Asked whenever the assistant draws, so a client ID
/// added to `config.toml` meanwhile counts.
pub type MicrosoftAvailable = Rc<dyn Fn() -> bool>;

#[derive(Default)]
struct State {
    step: Step,
    /// The screens Back returns to.
    history: Vec<Step>,
    /// The screen a login check started from, shown while it runs.
    from: Option<Step>,
    pending: Option<Pending>,
    /// The address detection ran for.
    address: Option<form::Address>,
    detection: Option<Detection>,
    sources: Sources,
    notice: Option<Notice>,
    /// The settings sent to `Verify`, then to `AddAccount`.
    attempt: Option<AccountSettings>,
    /// The other username `Verify` was given, for a check made again.
    second: Option<String>,
    /// Certificates the user trusted in this assistant: host, port and fingerprint. `Verify`
    /// pins them on the servers with that host and port.
    trusted: Vec<(String, u16, CertificatePin)>,
    /// The check's failure while its certificate is read.
    reading: Option<LoginError>,
    /// The certificate the trust step shows.
    untrusted: Option<Untrusted>,
    /// The password of the check, for `AddAccount`; wiped when dropped.
    password: Option<Password>,
    /// Enter was pressed on an address that isn't one: say why from now on.
    address_tried: bool,
    /// The sign-in page the engine asked to open, to open it again; never logged.
    sign_in_url: Option<String>,
    /// The browser sign-in has waited [`STILL_WAITING_AFTER`]: the note on admin approval shows.
    still_waiting: bool,
    /// The account this assistant signs in again, when the banner opened it.
    again: Option<Again>,
    /// What the presets know of that account's provider: its name and app-password page.
    known: Option<Hints>,
    /// A finished Microsoft sign-in, and the address it belongs to, while the identity step
    /// asks which address to send from.
    signed_in_as: Option<(RequestId, String)>,
}

impl State {
    /// The address the Microsoft sign-in belongs to, while the identity step asks.
    pub(super) fn signed_in_as(&self) -> Option<&str> {
        self.signed_in_as
            .as_ref()
            .map(|(_, address)| address.as_str())
    }

    /// The address detection ran for, as typed.
    pub(super) fn typed_address(&self) -> Option<&str> {
        self.address.as_ref().map(|address| address.text.as_str())
    }

    /// The screen on view: while a check runs, the one it started from.
    fn shown(&self) -> Step {
        match self.step {
            Step::Checking | Step::Adding => self.from.unwrap_or(Step::Manual),
            Step::SigningIn => Step::Microsoft,
            step => step,
        }
    }

    fn found(&self) -> Option<&DetectedSettings> {
        match &self.detection {
            Some(Detection::Found(settings)) => Some(settings),
            _ => None,
        }
    }

    /// The provider's name, from detection or, for an account typing its password again, the
    /// presets.
    fn provider(&self) -> Option<&str> {
        match self.found() {
            Some(settings) => settings.provider.as_deref(),
            None => self.known.as_ref()?.provider.as_deref(),
        }
    }

    /// The account is reached through its provider's Bridge on this computer (Proton Mail): its
    /// password is the Bridge's.
    fn bridge(&self) -> bool {
        match &self.again {
            Some(again) => is_loopback(&again.incoming.host),
            None => self
                .found()
                .is_some_and(|settings| is_loopback(&settings.imap.host)),
        }
    }

    /// The certificate the user trusted for `endpoint`'s host and port, if it is on this
    /// computer.
    fn pin(&self, endpoint: &mut Endpoint) {
        if !is_loopback(&endpoint.host) {
            return;
        }
        if let Some((_, _, pin)) = self
            .trusted
            .iter()
            .find(|(host, port, _)| *host == endpoint.host && *port == endpoint.port)
        {
            endpoint.certificate = Some(*pin);
        }
    }

    /// The provider's app-password page, when it wants one.
    fn app_password(&self) -> Option<&HelpLink> {
        match self.found() {
            Some(settings) => settings.app_password.as_ref(),
            None => self.known.as_ref()?.app_password.as_ref(),
        }
    }

    /// Where IMAP is turned on, when the user must do it first.
    fn enable_imap(&self) -> Option<&HelpLink> {
        match self.found() {
            Some(settings) => settings.enable_imap.as_ref(),
            None => self.known.as_ref()?.enable_imap.as_ref(),
        }
    }

    /// The username must be typed (the provider assigns it) or confirmed (S4: settings only
    /// the MX operator gave).
    fn asks_username(&self) -> bool {
        self.found().is_some_and(|settings| {
            settings.confirm || settings.username == sealwax_domain::account::UsernameForm::Other
        })
    }

    /// Moves on to a new screen; the one left is remembered for Back.
    fn go(&mut self, next: Step) {
        if self.step.is_screen() && self.step != next {
            self.history.push(self.step);
        }
        self.step = next;
    }

    fn is_pending(&self, pending: Pending) -> bool {
        self.pending == Some(pending)
    }

    /// The browser signs in for `request`: no note yet.
    fn start_sign_in(&mut self, request: RequestId) {
        self.pending = Some(Pending::SignIn(request));
        self.notice = None;
        self.sign_in_url = None;
        self.still_waiting = false;
        self.step = Step::SigningIn;
    }

    /// The wait for `request` is up: the note shows if the browser still signs in for it, and
    /// says whether it does. A sign-in that ended or started again meanwhile keeps none.
    fn note_still_waiting(&mut self, request: RequestId) -> bool {
        let waiting = self.step == Step::SigningIn && self.is_pending(Pending::SignIn(request));
        if waiting {
            self.still_waiting = true;
        }
        waiting
    }
}

struct Inner {
    dialog: adw::Dialog,
    engine: EngineLink,
    toasts: glib::WeakRef<adw::ToastOverlay>,
    microsoft: MicrosoftAvailable,
    widgets: Widgets,
    state: RefCell<State>,
    /// The rows of the sources list as last drawn.
    drawn_sources: RefCell<Option<Sources>>,
    subscription: RefCell<Option<Subscription>>,
    /// The wait before the note on admin approval, while the browser signs in.
    still_waiting_timer: RefCell<Option<glib::SourceId>>,
}

impl Inner {
    /// Stops the wait before the note on admin approval, if it runs.
    fn stop_still_waiting_timer(&self) {
        if let Some(timer) = self.still_waiting_timer.take() {
            timer.remove();
        }
    }

    /// Tells the engine to stop waiting for the browser, if it still does; the late answer is
    /// ignored.
    fn abandon_sign_in(&self) {
        self.stop_still_waiting_timer();
        let request = {
            let mut state = self.state.borrow_mut();
            match state.pending {
                Some(Pending::SignIn(request)) => {
                    state.pending = None;
                    state.sign_in_url = None;
                    Some(request)
                }
                _ => state.signed_in_as.take().map(|(request, _)| request),
            }
        };
        if let Some(request) = request {
            tracing::info!(request, "setup: sign-in cancelled");
            let _ = self.engine.send(Command::CancelSignIn { request });
        }
    }
}

impl Drop for Inner {
    /// An assistant that goes away while the browser signs in cancels the sign-in, so the
    /// engine frees its redirect port.
    fn drop(&mut self) {
        self.abandon_sign_in();
    }
}

/// An open assistant. At most one is open; [`open`] brings it back to the front.
#[derive(Clone)]
pub struct Assistant {
    inner: Rc<Inner>,
}

thread_local! {
    /// The open assistant. Its widgets' handlers hold it weakly, so closing it frees it.
    static CURRENT: RefCell<Option<Assistant>> = const { RefCell::new(None) };
}

/// The open assistant, if any.
pub fn current() -> Option<Assistant> {
    CURRENT.with(|current| current.borrow().clone())
}

/// Add Account: opens the assistant over `window`, or shows the one already open there. A new
/// one offers the first colour none of the accounts has (`used`).
pub fn open(
    window: &Window,
    engine: &EngineLink,
    microsoft: MicrosoftAvailable,
    used: &[AccountColor],
) -> Assistant {
    let parent = window.window();
    if let Some(open) = current() {
        if open.is_over(window) {
            open.inner.dialog.present(Some(parent));
            return open;
        }
        open.close();
    }
    let assistant = Assistant::new(engine, window.toasts(), microsoft);
    assistant
        .inner
        .widgets
        .colors
        .choose(Some(AccountColor::first_unused(used)), false);
    CURRENT.with(|current| current.replace(Some(assistant.clone())));
    assistant.inner.dialog.present(Some(parent));
    assistant.render();
    assistant.focus_first();
    assistant
}

/// The banner's Sign in (ML-032): opens the assistant over `window` at Microsoft sign-in for
/// `again` and starts it, or shows the one already open for it (or saving an account).
pub fn sign_in_again(
    window: &Window,
    engine: &EngineLink,
    microsoft: MicrosoftAvailable,
    again: Again,
) -> Assistant {
    let parent = window.window();
    if let Some(open) = current() {
        let state = open.inner.state.borrow();
        let keep = state.step == Step::Adding
            || state
                .again
                .as_ref()
                .is_some_and(|open| open.key == again.key);
        drop(state);
        if keep && open.is_over(window) {
            open.inner.dialog.present(Some(parent));
            return open;
        }
        open.close();
    }
    let assistant = Assistant::new(engine, window.toasts(), microsoft);
    assistant.inner.widgets.address.set_text(&again.address);
    {
        let mut state = assistant.inner.state.borrow_mut();
        state.again = Some(again);
        state.step = Step::Microsoft;
    }
    CURRENT.with(|current| current.replace(Some(assistant.clone())));
    assistant.inner.dialog.present(Some(parent));
    assistant.render();
    assistant.focus_first();
    assistant.sign_in();
    assistant
}

/// The banner's "Type the password again" (ML-048): opens the assistant over `window` at the
/// password for `again`, a password account the server refused, or shows the one already open
/// for it (or saving an account). The password goes to the engine with `UpdatePassword`.
pub fn type_password_again(
    window: &Window,
    engine: &EngineLink,
    microsoft: MicrosoftAvailable,
    again: Again,
) -> Assistant {
    let parent = window.window();
    if let Some(open) = current() {
        let state = open.inner.state.borrow();
        let keep = state.step == Step::Adding
            || state
                .again
                .as_ref()
                .is_some_and(|open| open.key == again.key);
        drop(state);
        if keep && open.is_over(window) {
            open.inner.dialog.present(Some(parent));
            return open;
        }
        open.close();
    }
    let assistant = Assistant::new(engine, window.toasts(), microsoft);
    assistant.inner.widgets.address.set_text(&again.address);
    {
        let mut state = assistant.inner.state.borrow_mut();
        state.known = known_provider(&again.address);
        state.again = Some(again);
        state.step = Step::PasswordAgain;
    }
    CURRENT.with(|current| current.replace(Some(assistant.clone())));
    assistant.inner.dialog.present(Some(parent));
    assistant.render();
    assistant.focus_first();
    assistant
}

/// What the presets say about the provider of `address`, without any lookup.
fn known_provider(address: &str) -> Option<Hints> {
    let preset = presets::by_domain(&address_domain(address)?)?;
    match preset.finding() {
        Finding::Servers { hints, .. } => Some(hints),
        _ => None,
    }
}

/// Draws the open assistant again, as after a change to `config.toml`'s client ID.
pub fn refresh() {
    if let Some(open) = current() {
        open.render();
    }
}

impl Assistant {
    fn new(engine: &EngineLink, toasts: adw::ToastOverlay, microsoft: MicrosoftAvailable) -> Self {
        let (widgets, dialog) = Widgets::build();
        let assistant = Self {
            inner: Rc::new(Inner {
                dialog,
                engine: engine.clone(),
                toasts: toasts.downgrade(),
                microsoft,
                widgets,
                state: RefCell::new(State::default()),
                drawn_sources: RefCell::new(None),
                subscription: RefCell::new(None),
                still_waiting_timer: RefCell::new(None),
            }),
        };
        assistant.connect();
        assistant
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.inner)
    }

    fn is_over(&self, window: &Window) -> bool {
        self.inner
            .dialog
            .root()
            .as_ref()
            .map(|root| root.upcast_ref::<gtk::Widget>())
            == Some(window.window().upcast_ref::<gtk::Widget>())
    }

    /// Closes the assistant from any step; a sign-in it waits for is cancelled.
    fn close(&self) {
        self.inner.abandon_sign_in();
        self.inner.dialog.force_close();
    }

    /// Runs `action` on the assistant from a signal handler, if it still exists.
    fn handler(&self, action: impl Fn(&Assistant) + 'static) -> impl Fn() + 'static {
        let weak = self.weak();
        move || {
            if let Some(inner) = weak.upgrade() {
                action(&Assistant { inner });
            }
        }
    }

    fn connect(&self) {
        let w = &self.inner.widgets;
        let advance = self.handler(Assistant::advance);
        let advance = Rc::new(advance);
        for entry in [
            &w.address,
            &w.name,
            &w.label,
            &w.username,
            &w.incoming.host,
            &w.incoming.port,
            &w.smtp.host,
            &w.smtp.port,
            &w.manual_username,
        ] {
            let advance = advance.clone();
            entry.connect_activate(move |_| advance());
        }
        let on_enter = advance.clone();
        w.password.connect_activate(move |_| on_enter());
        let on_swatch = advance.clone();
        w.colors.connect_enter(move || on_swatch());
        let on_click = advance.clone();
        w.primary.connect_clicked(move |_| on_click());

        let address_changed = self.handler(Assistant::address_changed);
        w.address.connect_changed(move |_| address_changed());
        let changed = Rc::new(self.handler(Assistant::render));
        for entry in [
            &w.name,
            &w.username,
            &w.incoming.host,
            &w.incoming.port,
            &w.smtp.host,
            &w.smtp.port,
            &w.manual_username,
        ] {
            let changed = changed.clone();
            entry.connect_changed(move |_| changed());
        }
        let on_password = changed.clone();
        w.password.connect_changed(move |_| on_password());
        for (incoming, fields) in [(true, &w.incoming), (false, &w.smtp)] {
            let weak = self.weak();
            fields.security.connect_selected_notify(move |_| {
                if let Some(inner) = weak.upgrade() {
                    Assistant { inner }.security_changed(incoming);
                }
            });
        }
        let receive_changed = self.handler(Assistant::receive_changed);
        w.receive
            .connect_selected_notify(move |_| receive_changed());

        let back = self.handler(Assistant::back);
        w.back.connect_clicked(move |_| back());
        let manual = self.handler(Assistant::open_manual);
        w.manual_link.connect_clicked(move |_| manual());
        let sign_in = self.handler(Assistant::sign_in);
        w.microsoft_button.connect_clicked(move |_| sign_in());
        let cancel_sign_in = self.handler(Assistant::cancel_sign_in);
        w.cancel.connect_clicked(move |_| cancel_sign_in());
        let open_again = self.handler(Assistant::open_again);
        w.open_again.connect_clicked(move |_| open_again());
        for (button, send_from_sign_in) in [(&w.send_from, true), (&w.keep_address, false)] {
            let weak = self.weak();
            button.connect_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    Assistant { inner }.answer_identity(send_from_sign_in);
                }
            });
        }
        for (button, trust) in [(&w.trust, true), (&w.dont_trust, false)] {
            let weak = self.weak();
            button.connect_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    Assistant { inner }.answer_certificate(trust);
                }
            });
        }
        let escape = self.handler(Assistant::back);
        self.inner.dialog.connect_close_attempt(move |_| escape());
        let cancel = self.handler(Assistant::close);
        w.close.connect_clicked(move |_| cancel());
        self.inner.dialog.connect_closed(|dialog| {
            // Dropping the assistant ends its engine subscription; a late reply finds no one.
            let closed = CURRENT.with(|current| {
                let mut current = current.borrow_mut();
                let ours = current
                    .as_ref()
                    .is_some_and(|assistant| &assistant.inner.dialog == dialog);
                if ours { current.take() } else { None }
            });
            if let Some(assistant) = closed {
                assistant.inner.abandon_sign_in();
                assistant.inner.subscription.take();
                assistant.inner.state.borrow_mut().password = None;
            }
        });

        let weak = self.weak();
        let subscription = self.inner.engine.subscribe(move |event| {
            if let Some(inner) = weak.upgrade() {
                Assistant { inner }.on_event(event);
            }
        });
        self.inner.subscription.replace(Some(subscription));
    }

    pub fn step(&self) -> Step {
        self.inner.state.borrow().step
    }

    pub fn dialog(&self) -> &adw::Dialog {
        &self.inner.dialog
    }

    /// The sources listed, as `preset:no match,autoconfig:found`.
    pub fn sources(&self) -> String {
        self.inner.state.borrow().sources.summary()
    }

    /// A widget of the assistant by its name (`setup-primary`, `setup-password`, …).
    pub fn widget(&self, name: &str) -> Option<gtk::Widget> {
        view::find_named(self.inner.dialog.upcast_ref(), name)
    }

    /// The name of the field or button that has the focus, such as `setup-address`.
    pub fn focused(&self) -> Option<String> {
        let mut widget = self.inner.dialog.focus()?;
        loop {
            if widget.widget_name().starts_with("setup-") {
                return Some(widget.widget_name().to_string());
            }
            widget = widget.parent()?;
        }
    }

    /// What Enter does in the focused field or colour.
    pub fn press_enter(&self) -> Result<(), String> {
        let focus = self
            .inner
            .dialog
            .focus()
            .ok_or("nothing in the assistant has the focus")?;
        if focus.is::<gtk::Text>() {
            focus.emit_by_name::<()>("activate", &[]);
            Ok(())
        } else if self.inner.widgets.colors.has_focus() {
            self.inner.widgets.colors.enter();
            Ok(())
        } else {
            Err(format!(
                "the focus is on a {}, not a text field",
                focus.type_().name()
            ))
        }
    }

    /// What Escape does: back a step, or close.
    pub fn press_escape(&self) {
        self.inner.dialog.close();
    }

    fn engine_send(&self, command: Command) -> bool {
        self.inner.engine.send(command).is_ok()
    }

    /// Enter and the primary button: the step's next move, when its fields allow it.
    fn advance(&self) {
        let step = self.step();
        if step.is_busy()
            || matches!(
                step,
                Step::Microsoft | Step::Blocked | Step::Identity | Step::Certificate
            )
        {
            return;
        }
        if step == Step::Address
            && form::parse_address(&self.inner.widgets.address.text()).is_none()
        {
            self.inner.state.borrow_mut().address_tried = true;
            self.render();
            return;
        }
        if self.problem().is_some() {
            self.render();
            return;
        }
        match step {
            Step::Address => self.detect(),
            Step::Password => self.verify_found(),
            Step::Manual | Step::Failed => self.verify_manual(),
            Step::PasswordAgain => self.update_password(),
            _ => {}
        }
    }

    /// `UpdatePassword` for the account the banner named: the engine checks the password on
    /// IMAP and SMTP and keeps it only if both take it.
    fn update_password(&self) {
        let Some(key) = self
            .inner
            .state
            .borrow()
            .again
            .as_ref()
            .map(|again| again.key.clone())
        else {
            return;
        };
        let password = Password::new(self.inner.widgets.password.text().to_string());
        let request = self.inner.engine.next_request();
        let sent = self.engine_send(Command::UpdatePassword {
            request,
            key,
            password,
        });
        {
            let mut state = self.inner.state.borrow_mut();
            if sent {
                tracing::info!(request, "setup: checking a new password");
                state.notice = None;
                state.pending = Some(Pending::Update(request));
                state.from = Some(Step::PasswordAgain);
                state.step = Step::Checking;
            } else {
                state.notice = Some(text::not_connected());
            }
        }
        self.render();
    }

    /// `PasswordUpdated`: a password both servers took closes the assistant with a toast; a
    /// refused one says why, and the password can be typed again.
    fn password_updated(&self, result: Result<(), LoginError>) {
        let again = {
            let mut state = self.inner.state.borrow_mut();
            state.pending = None;
            state.from = None;
            state.step = Step::PasswordAgain;
            state.again.clone()
        };
        let Some(again) = again else {
            return;
        };
        match result {
            Ok(()) => {
                tracing::info!("setup: the new password works");
                if let Some(toasts) = self.inner.toasts.upgrade() {
                    toasts.add_toast(adw::Toast::new(&text::password_accepted(&again.name)));
                }
                self.inner.dialog.force_close();
            }
            Err(error) => {
                tracing::info!(failure = ?error.failure, "setup: the new password was refused");
                {
                    let mut state = self.inner.state.borrow_mut();
                    let notice = text::password_again_failure(
                        &error,
                        &text::Context {
                            provider: state.provider(),
                            incoming: &again.incoming,
                            smtp: &again.smtp,
                            app_password: state.app_password(),
                            enable_imap: state.enable_imap(),
                        },
                    );
                    state.notice = Some(notice);
                }
                self.render();
                focus(self.inner.widgets.password.upcast_ref());
            }
        }
    }

    fn detect(&self) {
        let Some(address) = form::parse_address(&self.inner.widgets.address.text()) else {
            return;
        };
        let request = self.inner.engine.next_request();
        let sent = self.engine_send(Command::Detect {
            request,
            address: address.text.clone(),
        });
        {
            let mut state = self.inner.state.borrow_mut();
            if sent {
                tracing::info!(request, "setup: looking up an address");
                state.pending = Some(Pending::Detect(request));
                state.address = Some(address);
                state.detection = None;
                state.sources = Sources::default();
                state.notice = None;
                state.go(Step::Detecting);
            } else {
                state.notice = Some(text::not_connected());
            }
        }
        // A new lookup is a new attempt: no password typed for another one carries over.
        if sent {
            self.inner.widgets.password.set_text("");
        }
        self.render();
        self.focus_first();
    }

    fn detected(&self, detection: Detection) {
        let (step, proposed) = {
            let mut state = self.inner.state.borrow_mut();
            state.pending = None;
            state.sources.finished(&detection);
            let domain = state
                .address
                .as_ref()
                .map(|address| address.domain.clone())
                .unwrap_or_default();
            state.notice = text::detection(&detection, &domain);
            let next = match &detection {
                Detection::Found(settings) => match settings.auth {
                    AuthMethod::Password => Step::Password,
                    AuthMethod::OAuth(_) => Step::Microsoft,
                },
                Detection::Unsupported { .. } | Detection::NoMail => Step::Blocked,
                Detection::NotFound => Step::Manual,
            };
            state.detection = Some(detection);
            state.go(next);
            tracing::info!(step = next.name(), "setup: detection finished");
            // The username to confirm, or none for one the provider assigned.
            let proposed = state
                .found()
                .zip(state.address.as_ref())
                .and_then(|(settings, address)| settings.username.apply(&address.text));
            (next, proposed)
        };
        if step == Step::Password {
            self.inner
                .widgets
                .username
                .set_text(proposed.as_deref().unwrap_or_default());
        }
        if step == Step::Manual {
            self.fill_manual();
        }
        self.render();
        self.focus_first();
    }

    /// The manual fields from what detection found, else the standard ports and the address
    /// as the username; IMAP either way, which detection finds.
    fn fill_manual(&self) {
        let w = &self.inner.widgets;
        w.set_receive_protocol(Protocol::Imap);
        let state = self.inner.state.borrow();
        let address = form::parse_address(&w.address.text());
        match state.found() {
            Some(settings) => {
                w.incoming.fill(&settings.imap);
                w.smtp.fill(&settings.smtp);
                let typed = state.asks_username().then(|| w.username.text().to_string());
                let username = typed.or_else(|| {
                    address
                        .as_ref()
                        .and_then(|address| settings.username.apply(&address.text))
                });
                w.manual_username
                    .set_text(username.as_deref().unwrap_or_default());
            }
            None => {
                for (protocol, fields) in [(Protocol::Imap, &w.incoming), (Protocol::Smtp, &w.smtp)]
                {
                    fields.host.set_text("");
                    fields.security.set_selected(0);
                    fields
                        .port
                        .set_text(&form::default_port(protocol, Security::Tls).to_string());
                }
                w.manual_username.set_text(
                    address
                        .as_ref()
                        .map(|address| address.text.as_str())
                        .unwrap_or_default(),
                );
            }
        }
    }

    /// "Set up manually": the server fields, filled from detection when there was one.
    fn open_manual(&self) {
        {
            let mut state = self.inner.state.borrow_mut();
            if !matches!(state.step, Step::Address | Step::Detecting | Step::Password) {
                return;
            }
            state.pending = None;
            state.notice = None;
            if state.step != Step::Password {
                state.detection = None;
            }
            state.go(Step::Manual);
        }
        self.fill_manual();
        self.render();
        self.focus_first();
    }

    fn verify_found(&self) {
        let w = &self.inner.widgets;
        let plan = {
            let state = self.inner.state.borrow();
            let (Some(settings), Some(address)) = (state.found(), state.address.as_ref()) else {
                return;
            };
            let typed = state.asks_username().then(|| w.username.text().to_string());
            form::usernames(address, Some(settings), typed.as_deref()).map(|(first, second)| {
                (
                    form::settings(
                        address,
                        &w.naming(),
                        settings.imap.clone(),
                        Receive::Imap,
                        settings.smtp.clone(),
                        first,
                    ),
                    second,
                )
            })
        };
        if let Some((settings, second)) = plan {
            self.verify(settings, second);
        }
    }

    fn verify_manual(&self) {
        let w = &self.inner.widgets;
        let Ok(checked) = form::check_manual(&self.manual_fields().form()) else {
            return;
        };
        let settings = form::settings(
            &checked.address,
            &w.naming(),
            checked.incoming,
            checked.receive,
            checked.smtp,
            checked.username,
        );
        self.inner.state.borrow_mut().address = Some(checked.address);
        self.verify(settings, None);
    }

    /// `Verify`: one login with the username, and one more with `second` if that is refused.
    /// A server on this computer gets the certificate the user trusted for it.
    fn verify(&self, mut settings: AccountSettings, second: Option<String>) {
        {
            let state = self.inner.state.borrow();
            state.pin(&mut settings.incoming);
            state.pin(&mut settings.smtp);
        }
        let password = Password::new(self.inner.widgets.password.text().to_string());
        let request = self.inner.engine.next_request();
        let sent = self.engine_send(Command::Verify {
            request,
            settings: settings.clone(),
            second_username: second.clone(),
            password: password.clone(),
        });
        {
            let mut state = self.inner.state.borrow_mut();
            if sent {
                tracing::info!(request, "setup: checking a login");
                if state.step != Step::Failed {
                    state.notice = None;
                }
                state.pending = Some(Pending::Verify(request));
                state.from = Some(state.step);
                state.step = Step::Checking;
                state.attempt = Some(settings);
                state.second = second;
                state.password = Some(password);
            } else {
                state.notice = Some(text::not_connected());
            }
        }
        self.render();
    }

    fn verified(&self, result: Result<String, LoginError>) {
        match result {
            Ok(username) => {
                let (settings, password) = {
                    let mut state = self.inner.state.borrow_mut();
                    state.pending = None;
                    let (Some(mut settings), Some(password)) =
                        (state.attempt.clone(), state.password.clone())
                    else {
                        return;
                    };
                    settings.username = username;
                    state.attempt = Some(settings.clone());
                    (settings, password)
                };
                let request = self.inner.engine.next_request();
                let sent = self.engine_send(Command::AddAccount {
                    request,
                    settings,
                    password,
                    sign_in: None,
                });
                let mut state = self.inner.state.borrow_mut();
                if sent {
                    tracing::info!(request, "setup: the login works; saving the account");
                    state.pending = Some(Pending::Add(request));
                    state.step = Step::Adding;
                } else {
                    state.notice = Some(text::not_connected());
                    state.step = state.from.unwrap_or(Step::Manual);
                }
            }
            Err(error) => {
                tracing::info!(failure = ?error.failure, "setup: the login failed");
                if !self.read_certificate(&error) {
                    self.failed(error);
                }
                return;
            }
        }
        self.render();
    }

    /// The check failed: why, on the screen it started from (the manual settings, filled in,
    /// for found settings).
    fn failed(&self, error: LoginError) {
        let from = {
            let mut state = self.inner.state.borrow_mut();
            state.pending = None;
            state.password = None;
            let notice = state.attempt.as_ref().map(|attempt| {
                let found = state.found();
                text::login_failure(
                    &error,
                    &text::Context {
                        provider: found.and_then(|settings| settings.provider.as_deref()),
                        incoming: &attempt.incoming,
                        smtp: &attempt.smtp,
                        app_password: found.and_then(|settings| settings.app_password.as_ref()),
                        enable_imap: found.and_then(|settings| settings.enable_imap.as_ref()),
                    },
                )
            });
            state.notice = notice;
            let from = state.from.take().unwrap_or(Step::Manual);
            if from != Step::Failed {
                state.history.push(from);
            }
            state.step = Step::Failed;
            from
        };
        let attempt = self.inner.state.borrow().attempt.clone();
        if from == Step::Password
            && let Some(attempt) = attempt
        {
            let w = &self.inner.widgets;
            w.set_receive_protocol(attempt.receive.protocol());
            w.incoming.fill(&attempt.incoming);
            w.smtp.fill(&attempt.smtp);
            w.manual_username.set_text(&attempt.username);
        }
        self.render();
        self.focus_failure(error);
    }

    /// A certificate the system doesn't trust on a server on this computer: `ReadCertificate`,
    /// for the trust step. `false` when that isn't the failure, or the engine is gone.
    fn read_certificate(&self, error: &LoginError) -> bool {
        let endpoint = {
            let state = self.inner.state.borrow();
            let Some(attempt) = state.attempt.as_ref() else {
                return false;
            };
            let endpoint = match error.protocol {
                Protocol::Imap | Protocol::Pop3 => &attempt.incoming,
                Protocol::Smtp => &attempt.smtp,
            };
            if error.failure != LoginFailure::Certificate || !is_loopback(&endpoint.host) {
                return false;
            }
            endpoint.clone()
        };
        let request = self.inner.engine.next_request();
        if !self.engine_send(Command::ReadCertificate {
            request,
            endpoint,
            protocol: error.protocol,
        }) {
            return false;
        }
        tracing::info!(
            request,
            "setup: reading the certificate of a server on this computer"
        );
        let mut state = self.inner.state.borrow_mut();
        state.pending = Some(Pending::Read(request));
        state.reading = Some(error.clone());
        true
    }

    /// The certificate read: the trust step, or straight to a check again when the user
    /// trusted it on another port of the same host.
    fn certificate_read(&self, result: Result<CertificatePin, LoginFailure>) {
        let (error, endpoint) = {
            let mut state = self.inner.state.borrow_mut();
            state.pending = None;
            let Some(error) = state.reading.take() else {
                return;
            };
            let endpoint = state.attempt.as_ref().map(|attempt| match error.protocol {
                Protocol::Imap | Protocol::Pop3 => attempt.incoming.clone(),
                Protocol::Smtp => attempt.smtp.clone(),
            });
            (error, endpoint)
        };
        let (Ok(pin), Some(endpoint)) = (result, endpoint) else {
            self.failed(error);
            return;
        };
        if endpoint.certificate == Some(pin) {
            self.failed(error);
            return;
        }
        let known = self
            .inner
            .state
            .borrow()
            .trusted
            .iter()
            .any(|(host, _, trusted)| *host == endpoint.host && *trusted == pin);
        if known {
            tracing::info!("setup: the same certificate as trusted; checking again");
            self.trust(&endpoint, pin);
            self.check_again();
            return;
        }
        {
            let mut state = self.inner.state.borrow_mut();
            let from = state.from.take().unwrap_or(Step::Manual);
            if from != Step::Failed {
                state.history.push(from);
            }
            state.untrusted = Some(Untrusted {
                protocol: error.protocol,
                endpoint,
                pin,
                error,
                from,
            });
            state.notice = None;
            state.step = Step::Certificate;
        }
        self.render();
        self.focus_first();
    }

    fn trust(&self, endpoint: &Endpoint, pin: CertificatePin) {
        let mut state = self.inner.state.borrow_mut();
        state
            .trusted
            .retain(|(host, port, _)| !(*host == endpoint.host && *port == endpoint.port));
        state
            .trusted
            .push((endpoint.host.clone(), endpoint.port, pin));
    }

    /// The check again, with the settings and usernames of the last one.
    fn check_again(&self) {
        let again = {
            let state = self.inner.state.borrow();
            state
                .attempt
                .clone()
                .map(|attempt| (attempt, state.second.clone()))
        };
        match again {
            Some((attempt, second)) => self.verify(attempt, second),
            None => self.render(),
        }
    }

    /// The trust step's answer. Trusting checks again with the certificate pinned; not trusting
    /// shows the check's failure.
    fn answer_certificate(&self, trust: bool) {
        let Some(untrusted) = self.inner.state.borrow_mut().untrusted.take() else {
            return;
        };
        tracing::info!(trust, "setup: the certificate answered");
        {
            let mut state = self.inner.state.borrow_mut();
            if untrusted.from != Step::Failed {
                state.history.pop();
            }
            state.step = untrusted.from;
            state.from = Some(untrusted.from);
        }
        if trust {
            self.trust(&untrusted.endpoint, untrusted.pin);
            self.check_again();
        } else {
            self.failed(untrusted.error);
        }
    }

    /// "Sign in with Microsoft": `SignIn` for the address (or the account signed in again);
    /// the engine answers with the page to open, then `SignedIn`.
    fn sign_in(&self) {
        if !(self.inner.microsoft)() {
            return;
        }
        let target = {
            let state = self.inner.state.borrow();
            if state.step != Step::Microsoft {
                return;
            }
            let provider = match state.found().map(|settings| settings.auth) {
                Some(AuthMethod::OAuth(provider)) => provider,
                _ => OAuthProvider::Microsoft,
            };
            match &state.again {
                Some(again) => Some((provider, again.address.clone(), Some(again.key.clone()))),
                None => state
                    .address
                    .as_ref()
                    .map(|address| (provider, address.text.clone(), None)),
            }
        };
        let Some((provider, address, account)) = target else {
            return;
        };
        let request = self.inner.engine.next_request();
        let sent = self.engine_send(Command::SignIn {
            request,
            provider,
            address,
            account,
        });
        {
            let mut state = self.inner.state.borrow_mut();
            if sent {
                tracing::info!(request, "setup: signing in in the browser");
                state.start_sign_in(request);
            } else {
                state.notice = Some(text::not_connected());
            }
        }
        if sent {
            self.start_still_waiting_timer(request);
        }
        self.render();
    }

    /// Shows the note on admin approval once the browser has signed in for `request` for a
    /// while without an answer.
    fn start_still_waiting_timer(&self, request: RequestId) {
        self.inner.stop_still_waiting_timer();
        let weak = self.weak();
        let timer = glib::timeout_add_local_once(STILL_WAITING_WAIT.with(Cell::get), move || {
            if let Some(inner) = weak.upgrade() {
                inner.still_waiting_timer.take();
                Assistant { inner }.still_waiting(request);
            }
        });
        self.inner.still_waiting_timer.replace(Some(timer));
    }

    fn still_waiting(&self, request: RequestId) {
        if !self.inner.state.borrow_mut().note_still_waiting(request) {
            return;
        }
        tracing::info!(request, "setup: still waiting for the browser");
        self.render();
        crate::accessibility::announce(&self.inner.widgets.still_waiting, text::STILL_WAITING);
    }

    /// `SignInOpen`: the page goes to the browser, and stays at hand to open again.
    fn sign_in_open(&self, url: &str) {
        self.inner.state.borrow_mut().sign_in_url = Some(url.to_owned());
        tracing::info!("setup: opening the sign-in page");
        self.render();
        browser::open(self.inner.dialog.upcast_ref(), url);
    }

    /// "Open the sign-in page again", for a tab closed before the sign-in ended.
    fn open_again(&self) {
        let url = {
            let state = self.inner.state.borrow();
            state
                .sign_in_url
                .clone()
                .filter(|_| state.step == Step::SigningIn)
        };
        if let Some(url) = url {
            tracing::info!("setup: opening the sign-in page again");
            browser::open(self.inner.dialog.upcast_ref(), &url);
        }
    }

    /// Cancel, Back and Escape while the browser signs in: `CancelSignIn`, and the step's
    /// button again.
    fn cancel_sign_in(&self) {
        if self.step() != Step::SigningIn {
            return;
        }
        self.inner.abandon_sign_in();
        self.inner.state.borrow_mut().step = Step::Microsoft;
        self.render();
        self.focus_first();
    }

    /// `SignedIn`: a new account is saved with the username the sign-in belongs to and the
    /// sign-in's token; an account signed in again is done. A failure says why, and the step's
    /// button signs in again.
    fn signed_in(&self, request: RequestId, result: Result<String, LoginError>) {
        self.inner.stop_still_waiting_timer();
        let username = match result {
            Ok(username) => username,
            Err(error) => {
                tracing::info!(failure = ?error.failure, "setup: the sign-in failed");
                {
                    let mut state = self.inner.state.borrow_mut();
                    state.pending = None;
                    state.sign_in_url = None;
                    let provider = state.found().and_then(|settings| settings.provider.clone());
                    state.notice = Some(text::sign_in_failure(&error, provider.as_deref()));
                    state.step = Step::Microsoft;
                }
                self.render();
                self.focus_first();
                return;
            }
        };
        let again = {
            let mut state = self.inner.state.borrow_mut();
            state.pending = None;
            state.sign_in_url = None;
            state.again.clone()
        };
        if let Some(again) = again {
            tracing::info!(request, "setup: signed in again");
            if let Some(toasts) = self.inner.toasts.upgrade() {
                toasts.add_toast(adw::Toast::new(&text::signed_in_again(&again.name)));
            }
            self.inner.dialog.force_close();
            return;
        }
        let typed = self.inner.state.borrow().typed_address().map(str::to_owned);
        if let Some(typed) = typed
            && !same_address(&typed, &username)
        {
            tracing::info!(
                request,
                "setup: signed in as another address; asking which to send from"
            );
            {
                let mut state = self.inner.state.borrow_mut();
                state.signed_in_as = Some((request, username));
                state.history.push(Step::Microsoft);
                state.step = Step::Identity;
            }
            self.render();
            self.focus_first();
            return;
        }
        self.add_signed_in(request, username, None);
    }

    /// The identity step's answer: send from the address signed in with, or keep the one
    /// typed.
    fn answer_identity(&self, send_from_sign_in: bool) {
        let Some((request, username)) = self.inner.state.borrow_mut().signed_in_as.take() else {
            return;
        };
        tracing::info!(
            request,
            send_from_sign_in,
            "setup: the sending address chosen"
        );
        let send_from = send_from_sign_in
            .then(|| form::parse_address(&username))
            .flatten();
        self.add_signed_in(request, username, send_from);
    }

    /// `AddAccount` for the sign-in `request`, whose token belongs to `username`, sending from
    /// the address typed or from `send_from`.
    fn add_signed_in(
        &self,
        request: RequestId,
        username: String,
        send_from: Option<form::Address>,
    ) {
        let settings = {
            let state = self.inner.state.borrow();
            state
                .found()
                .zip(send_from.as_ref().or(state.address.as_ref()))
                .map(|(found, address)| {
                    let mut settings = form::settings(
                        address,
                        &self.inner.widgets.naming(),
                        found.imap.clone(),
                        Receive::Imap,
                        found.smtp.clone(),
                        username,
                    );
                    settings.auth = found.auth;
                    settings
                })
        };
        let Some(settings) = settings else {
            self.inner.state.borrow_mut().step = Step::Microsoft;
            self.render();
            return;
        };
        let add = self.inner.engine.next_request();
        let sent = self.engine_send(Command::AddAccount {
            request: add,
            settings: settings.clone(),
            password: Password::new(String::new()),
            sign_in: Some(request),
        });
        {
            let mut state = self.inner.state.borrow_mut();
            state.from = Some(Step::Microsoft);
            if sent {
                tracing::info!(request = add, "setup: signed in; saving the account");
                state.pending = Some(Pending::Add(add));
                state.attempt = Some(settings);
                state.step = Step::Adding;
            } else {
                state.notice = Some(text::not_connected());
                state.step = Step::Microsoft;
            }
        }
        self.render();
    }

    fn added(&self, result: Result<AccountId, AccountError>) {
        match result {
            Ok(_) => {
                let address = {
                    let mut state = self.inner.state.borrow_mut();
                    state.pending = None;
                    state.password = None;
                    state
                        .attempt
                        .as_ref()
                        .map(|settings| settings.address.clone())
                };
                tracing::info!("setup: account added");
                if let (Some(toasts), Some(address)) = (self.inner.toasts.upgrade(), address) {
                    toasts.add_toast(adw::Toast::new(&format!("Added {address}")));
                }
                self.inner.dialog.force_close();
            }
            Err(error) => {
                tracing::warn!(%error, "setup: the account could not be saved");
                {
                    let mut state = self.inner.state.borrow_mut();
                    state.pending = None;
                    state.password = None;
                    state.notice = Some(text::account_error(&error));
                    state.step = state.from.take().unwrap_or(Step::Manual);
                }
                self.render();
                self.focus_first();
            }
        }
    }

    fn on_event(&self, event: &Event) {
        let state = self.inner.state.borrow();
        match event {
            Event::DetectProgress { request, source }
                if state.is_pending(Pending::Detect(*request)) =>
            {
                drop(state);
                self.inner.state.borrow_mut().sources.started(*source);
                self.render();
            }
            Event::Detected { request, detection }
                if state.is_pending(Pending::Detect(*request)) =>
            {
                drop(state);
                self.detected(detection.clone());
            }
            Event::Verified { request, result } if state.is_pending(Pending::Verify(*request)) => {
                drop(state);
                self.verified(result.clone());
            }
            Event::CertificateRead { request, result }
                if state.is_pending(Pending::Read(*request)) =>
            {
                drop(state);
                self.certificate_read(*result);
            }
            Event::AccountAdded { request, result } if state.is_pending(Pending::Add(*request)) => {
                drop(state);
                self.added(result.clone());
            }
            Event::SignInOpen { request, url } if state.is_pending(Pending::SignIn(*request)) => {
                drop(state);
                self.sign_in_open(url);
            }
            Event::SignedIn { request, result } if state.is_pending(Pending::SignIn(*request)) => {
                drop(state);
                self.signed_in(*request, result.clone());
            }
            Event::PasswordUpdated { request, result }
                if state.is_pending(Pending::Update(*request)) =>
            {
                drop(state);
                self.password_updated(result.clone());
            }
            _ => {}
        }
    }

    /// Escape, Back and the dialog's close: a step back, or out on the first one. Saving an
    /// account can't be abandoned halfway.
    fn back(&self) {
        if self.step() == Step::SigningIn {
            self.cancel_sign_in();
            return;
        }
        if self.step() == Step::Identity {
            // The sign-in isn't used: let the engine drop its token.
            self.inner.abandon_sign_in();
        }
        if self.step() == Step::Certificate {
            self.answer_certificate(false);
            return;
        }
        let close = {
            let mut state = self.inner.state.borrow_mut();
            match state.step {
                Step::Adding => return,
                Step::Checking => {
                    state.pending = None;
                    state.password = None;
                    state.step = state.from.take().unwrap_or(Step::Manual);
                    false
                }
                _ => {
                    state.pending = None;
                    state.notice = None;
                    state.address_tried = false;
                    match state.history.pop() {
                        Some(step) => {
                            state.step = step;
                            false
                        }
                        None => true,
                    }
                }
            }
        };
        if close {
            self.inner.dialog.force_close();
            return;
        }
        self.render();
        self.focus_first();
    }

    /// A new address makes what was found for the old one stale: back to the first step.
    fn address_changed(&self) {
        {
            let mut state = self.inner.state.borrow_mut();
            if matches!(
                state.step,
                Step::Detecting | Step::Password | Step::Microsoft | Step::Blocked
            ) {
                state.pending = None;
                state.notice = None;
                state.detection = None;
                state.sources = Sources::default();
                state.history.clear();
                state.step = Step::Address;
            }
        }
        self.render();
    }

    /// Switching TLS ↔ STARTTLS moves a standard port along with it; `incoming`: the incoming
    /// server's (IMAP's or POP3's, as chosen), else SMTP's.
    fn security_changed(&self, incoming: bool) {
        let w = &self.inner.widgets;
        let (protocol, fields) = if incoming {
            (w.receive_protocol(), &w.incoming)
        } else {
            (Protocol::Smtp, &w.smtp)
        };
        let security = fields.security();
        let other = match security {
            Security::Tls => Security::StartTls,
            Security::StartTls => Security::Tls,
        };
        let port = fields.port.text();
        if port.trim().is_empty()
            || form::parse_port(&port) == Some(form::default_port(protocol, other))
        {
            fields
                .port
                .set_text(&form::default_port(protocol, security).to_string());
        }
        self.render();
    }

    /// "Incoming mail" switched between IMAP and POP3: the server's caption follows, and a
    /// standard port moves to the other protocol's (993 ↔ 995, 143 ↔ 110); a typed one stays.
    fn receive_changed(&self) {
        let w = &self.inner.widgets;
        let to = w.receive_protocol();
        let from = match to {
            Protocol::Pop3 => Protocol::Imap,
            Protocol::Imap | Protocol::Smtp => Protocol::Pop3,
        };
        let port = w.incoming.port.text();
        if let Some(moved) = form::moved_port(&port, from, to, w.incoming.security()) {
            w.incoming.port.set_text(&moved.to_string());
        }
        self.render();
    }

    fn manual_fields(&self) -> ManualTexts {
        let w = &self.inner.widgets;
        let receive = match w.receive_protocol() {
            Protocol::Pop3 => Receive::Pop3(Pop3Policy::default()),
            Protocol::Imap | Protocol::Smtp => Receive::Imap,
        };
        ManualTexts {
            address: w.address.text().to_string(),
            receive,
            incoming_host: w.incoming.host.text().to_string(),
            incoming_port: w.incoming.port.text().to_string(),
            incoming_security: w.incoming.security(),
            smtp_host: w.smtp.host.text().to_string(),
            smtp_port: w.smtp.port.text().to_string(),
            smtp_security: w.smtp.security(),
            username: w.manual_username.text().to_string(),
        }
    }

    /// What keeps the step's button insensitive, if anything.
    fn problem(&self) -> Option<Problem> {
        let w = &self.inner.widgets;
        let state = self.inner.state.borrow();
        let password_empty = w.password.text().is_empty();
        match state.step {
            Step::Address => {
                let text = w.address.text();
                if text.trim().is_empty() {
                    Some(Problem::Empty(Field::Address))
                } else if form::parse_address(&text).is_none() {
                    Some(Problem::Invalid(Field::Address))
                } else {
                    None
                }
            }
            Step::Password => {
                if state.asks_username() && w.username.text().trim().is_empty() {
                    Some(Problem::Empty(Field::Username))
                } else if password_empty {
                    Some(Problem::Empty(Field::Password))
                } else {
                    None
                }
            }
            Step::Manual | Step::Failed => form::check_manual(&self.manual_fields().form())
                .err()
                .or(password_empty.then_some(Problem::Empty(Field::Password))),
            Step::PasswordAgain => password_empty.then_some(Problem::Empty(Field::Password)),
            _ => None,
        }
    }

    fn render(&self) {
        let problem = self.problem();
        let microsoft = (self.inner.microsoft)();
        let state = self.inner.state.borrow();
        self.inner
            .widgets
            .draw(&state, problem, &self.inner.drawn_sources, microsoft);
    }

    /// The first field the step needs typed: the address, a username to type or confirm, the
    /// password, or the first manual field that is empty or wrong.
    fn focus_first(&self) {
        let w = &self.inner.widgets;
        let step = self.step();
        let target: Option<gtk::Widget> = match step {
            Step::Address => Some(w.address.clone().upcast()),
            Step::Password => {
                if w.username_field.is_visible() && w.username.text().trim().is_empty() {
                    Some(w.username.clone().upcast())
                } else {
                    Some(w.password.clone().upcast())
                }
            }
            Step::Manual | Step::Failed => Some(match self.problem() {
                Some(problem) => self.field_widget(problem.field()),
                None => w.incoming.host.clone().upcast(),
            }),
            Step::PasswordAgain => Some(w.password.clone().upcast()),
            Step::Microsoft if (self.inner.microsoft)() => {
                Some(w.microsoft_button.clone().upcast())
            }
            Step::Microsoft if !w.back.is_visible() => Some(w.close.clone().upcast()),
            Step::Microsoft | Step::Blocked => Some(w.back.clone().upcast()),
            Step::Identity => Some(w.send_from.clone().upcast()),
            Step::Certificate => Some(w.trust.clone().upcast()),
            Step::SigningIn => Some(w.cancel.clone().upcast()),
            Step::Detecting | Step::Checking | Step::Adding => None,
        };
        if let Some(target) = target {
            focus(&target);
        }
    }

    /// After a failed login: the password for a password problem, else the server's name.
    fn focus_failure(&self, error: LoginError) {
        let w = &self.inner.widgets;
        let target: gtk::Widget = match error.failure {
            LoginFailure::WrongPassword
            | LoginFailure::AppPasswordRequired
            | LoginFailure::ImapDisabled
            | LoginFailure::OAuthRequired => w.password.clone().upcast(),
            _ => match error.protocol {
                Protocol::Imap | Protocol::Pop3 => w.incoming.host.clone().upcast(),
                Protocol::Smtp => w.smtp.host.clone().upcast(),
            },
        };
        focus(&target);
    }

    fn field_widget(&self, field: Field) -> gtk::Widget {
        let w = &self.inner.widgets;
        match field {
            Field::Address => w.address.clone().upcast(),
            Field::IncomingHost => w.incoming.host.clone().upcast(),
            Field::IncomingPort => w.incoming.port.clone().upcast(),
            Field::SmtpHost => w.smtp.host.clone().upcast(),
            Field::SmtpPort => w.smtp.port.clone().upcast(),
            Field::Username => {
                if w.username_field.is_visible() {
                    w.username.clone().upcast()
                } else {
                    w.manual_username.clone().upcast()
                }
            }
            Field::Password => w.password.clone().upcast(),
        }
    }
}

/// Moves the focus to `widget`. A text field gets the cursor at the end of its text, not GTK's
/// select-all, which stays highlighted once the focus moves on; the password keeps it, so
/// typing replaces a wrong one.
fn focus(widget: &gtk::Widget) {
    match widget.downcast_ref::<gtk::Entry>() {
        Some(entry) => {
            entry.grab_focus_without_selecting();
        }
        None => {
            widget.grab_focus();
        }
    }
}

/// The manual fields' text, owned, for [`form::check_manual`].
struct ManualTexts {
    address: String,
    receive: Receive,
    incoming_host: String,
    incoming_port: String,
    incoming_security: Security,
    smtp_host: String,
    smtp_port: String,
    smtp_security: Security,
    username: String,
}

impl ManualTexts {
    fn form(&self) -> form::Manual<'_> {
        form::Manual {
            address: &self.address,
            receive: self.receive,
            incoming_host: &self.incoming_host,
            incoming_port: &self.incoming_port,
            incoming_security: Some(self.incoming_security),
            smtp_host: &self.smtp_host,
            smtp_port: &self.smtp_port,
            smtp_security: Some(self.smtp_security),
            username: &self.username,
        }
    }
}

/// The same mailbox: addresses compare without case or surrounding space.
fn same_address(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_note_on_admin_approval_is_for_the_sign_in_still_waiting() {
        let mut state = State {
            step: Step::Microsoft,
            ..State::default()
        };
        state.start_sign_in(1);
        assert!(!state.still_waiting);
        assert!(state.note_still_waiting(1));
        assert!(state.still_waiting);
        // Signing in again starts without the note, and the first sign-in's wait shows none.
        state.start_sign_in(2);
        assert!(!state.still_waiting);
        assert!(!state.note_still_waiting(1));
        assert!(!state.still_waiting);
        assert!(state.note_still_waiting(2));
        // A sign-in that ended or was cancelled before its wait was up shows none.
        state.start_sign_in(3);
        state.pending = None;
        state.step = Step::Microsoft;
        assert!(!state.note_still_waiting(3));
        assert!(!state.still_waiting);
        state.start_sign_in(4);
        state.step = Step::Adding;
        assert!(!state.note_still_waiting(4));
        assert!(!state.still_waiting);
    }

    #[test]
    fn the_note_waits_a_minute() {
        assert_eq!(STILL_WAITING_AFTER, Duration::from_secs(60));
        assert_eq!(STILL_WAITING_WAIT.with(Cell::get), STILL_WAITING_AFTER);
    }
}
