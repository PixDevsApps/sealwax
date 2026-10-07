//! New-mail notifications (ML-083, PRD D5, ADR-013 and its M7 amendment): the engine's
//! `Event::NewMail` becomes a `gio::Notification` showing the sender and the subject (or only a
//! count, `[notifications] show = "count"`), whose default action opens that message with or
//! without a window; Archive and Mark Read where the server shows buttons.
//!
//! - **What notifies:** unread messages of Inbox folders. Junk, Trash, Sent, Drafts, Archive and
//!   other folders stay quiet, and so does a folder the focused window shows (its list shows the
//!   mail). `[notifications] enabled = false` turns them off; both settings apply as
//!   `config.toml` changes, and turning them off or to the count withdraws what is on screen.
//! - **Grouping:** up to [`SEPARATE`] notifications of their own within [`LIVE`]; past that, one
//!   notification counts them all ("5 new messages", with the senders), and the others are
//!   withdrawn. In the count mode there is only that one.
//! - **A click** (`app.open-message`, the message's id as the target) shows the main window
//!   (`AppState::show_main_window`, which builds it when background mode closed it), shows the message's folder (All Inboxes stays),
//!   selects the message and opens it in the reader. **Archive** (`app.archive-message`) and
//!   **Mark Read** (`app.mark-message-read`) run the triage controller's actions, so the window's
//!   Undo works. These actions take a message, so they are not the registry's commands.
//! - **Withdrawn** when the message is opened from it, read, deleted, or moved out of its Inbox
//!   anywhere (the store's change events), and when it is archived or marked read from it.
//! - **Text:** the sender and subject go to the notification server only. Control and direction
//!   characters are dropped and lengths capped; the body is markup-escaped when the server reads
//!   markup there (omarchy-shell does: a subject could otherwise carry a link). Nothing here logs
//!   a sender, a subject or an address.

use crate::application::{AppState, ICON_NAME, SealwaxApplication};
use crate::engine_link::Subscription;
use crate::list::model::passes;
use crate::mailbox::folder_key;
use crate::triage::{Controller, Directory, OpsBackend, Origin, Target};
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use sealwax_domain::config::{NotificationContent, Notifications};
use sealwax_domain::model::{FolderId, FolderRole, MessageId, MessageSummary};
use sealwax_domain::triage::Triage;
use sealwax_engine::api::Event;
use sealwax_infrastructure::store::{ChangeEvent, ViewFilter, ViewScope};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The actions a notification invokes, on the application, with a message id (`x`).
pub const OPEN_ACTION: &str = "open-message";
pub const ARCHIVE_ACTION: &str = "archive-message";
pub const READ_ACTION: &str = "mark-message-read";
/// The buttons' labels (shown where the server shows buttons; omarchy-shell shows none).
pub const ARCHIVE_LABEL: &str = "Archive";
pub const READ_LABEL: &str = "Mark Read";
/// The count's title.
pub const COUNT_TITLE: &str = "New mail";
/// What a click says when its message is gone.
pub const GONE_TEXT: &str = "The message was moved or deleted";
/// Notifications of their own at most within [`LIVE`]; more become one that counts them.
pub const SEPARATE: usize = 3;
/// How long a notification counts as on screen for grouping: omarchy-shell shows none longer.
pub const LIVE: Duration = Duration::from_secs(30);
/// Characters of the sender (the title) and the subject (the body) sent at most.
pub const TITLE_CHARS: usize = 80;
pub const BODY_CHARS: usize = 200;
/// How long a click waits for its message's row in the list, and for it on screen.
const OPEN_WAIT: Duration = Duration::from_secs(5);
const PAINT_WAIT: Duration = Duration::from_secs(10);
/// Messages remembered for withdrawal at most; the oldest are forgotten first.
const REMEMBERED: usize = 500;
/// The count's notification id.
const COUNT_ID: &str = "new-mail";
/// What [`sent`] and [`clicks`] keep at most, newest last.
const KEPT_SENT: usize = 1_000;
const KEPT_CLICKS: usize = 100;

/// What a notification stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shown {
    /// A notification of its own, sent at that time.
    Alone(Instant),
    /// Counted in the count's notification.
    Counted,
}

/// A message notified about, for the count's text. Holds the sender's name, so its `Debug`
/// prints none.
struct Notified {
    id: MessageId,
    sender: String,
    date: i64,
    shown: Shown,
}

impl std::fmt::Debug for Notified {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Notified")
            .field("id", &self.id)
            .field("sender_chars", &self.sender.chars().count())
            .field("shown", &self.shown)
            .finish()
    }
}

/// What a click on a notification did, for the self-tests and `notify_live`: ids and times.
#[derive(Clone, Copy, Debug)]
pub struct Click {
    pub message: MessageId,
    /// When the action ran (GLib runs it 0.05 ms after the server's `ActionInvoked`, S6).
    pub action: Instant,
    /// Whether the main window was on screen then.
    pub had_window: bool,
    /// The message was found in the store and its row selected in the list.
    pub selected: Option<Instant>,
    /// The first frame painted with the message in the reader.
    pub painted: Option<Instant>,
}

/// What one notification sent or withdrew, for the self-tests: no text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sent {
    Message(MessageId),
    Count(usize),
    Withdrawn(String),
}

#[derive(Default)]
struct Notifier {
    events: RefCell<Option<Subscription>>,
    settings: Cell<Notifications>,
    /// The messages notified about and not withdrawn, oldest first.
    notified: RefCell<Vec<Notified>>,
    /// When the count's notification was last sent, while it is shown.
    count_sent: Cell<Option<Instant>>,
    /// Whether the server reads markup in the body; asked once, before the first notification.
    markup: Cell<Option<bool>>,
    /// Archive and Mark Read before any window made the triage controller.
    triage: RefCell<Option<Controller>>,
    sent: RefCell<Vec<Sent>>,
    clicks: RefCell<Vec<Click>>,
}

thread_local! {
    static NOTIFIER: Rc<Notifier> = Rc::default();
}

fn shared() -> Rc<Notifier> {
    NOTIFIER.with(Rc::clone)
}

fn app() -> Option<SealwaxApplication> {
    gio::Application::default().and_downcast::<SealwaxApplication>()
}

/// Follows the engine's new mail and `[notifications]`, once at startup.
pub fn install(state: &Rc<AppState>) {
    let notifier = shared();
    if let Some(prefs) = state.preferences() {
        notifier.settings.set(prefs.config().notifications);
        let weak = Rc::downgrade(&prefs);
        prefs.connect_changed(move |update| {
            if update.config_before.is_some()
                && let Some(prefs) = weak.upgrade()
            {
                apply(prefs.config().notifications);
            }
        });
    }
    let weak = Rc::downgrade(state);
    let subscription = state.engine().subscribe(move |event| {
        if let Event::NewMail {
            folder, messages, ..
        } = event
            && let Some(state) = weak.upgrade()
        {
            arrived(&state, *folder, messages);
        }
    });
    notifier.events.replace(Some(subscription));
    let weak = Rc::downgrade(state);
    state.mailbox().connect_event(move |event| {
        if let Some(state) = weak.upgrade() {
            store_changed(&state, event);
        }
    });
    if let Some(app) = app() {
        install_actions(&app, state);
    }
}

fn install_actions(app: &SealwaxApplication, state: &Rc<AppState>) {
    let id_type = Some(glib::VariantTy::INT64);
    for name in [OPEN_ACTION, ARCHIVE_ACTION, READ_ACTION] {
        let action = gio::SimpleAction::new(name, id_type);
        let weak = Rc::downgrade(state);
        action.connect_activate(move |action, parameter| {
            let (Some(state), Some(id)) = (
                weak.upgrade(),
                parameter.and_then(glib::Variant::get::<i64>),
            ) else {
                return;
            };
            let id = MessageId(id);
            match action.name().as_str() {
                OPEN_ACTION => open(&state, id),
                ARCHIVE_ACTION => act(&state, id, Triage::Archive),
                _ => act(&state, id, Triage::Read(true)),
            }
        });
        app.add_action(&action);
    }
}

/// The settings in use.
pub fn settings() -> Notifications {
    shared().settings.get()
}

/// What was sent and withdrawn lately, oldest first (the self-tests).
pub fn sent() -> Vec<Sent> {
    shared().sent.borrow().clone()
}

/// The latest clicks, oldest first.
pub fn clicks() -> Vec<Click> {
    shared().clicks.borrow().clone()
}

/// The messages with a notification on screen or counted in one.
pub fn notified() -> Vec<MessageId> {
    shared().notified.borrow().iter().map(|n| n.id).collect()
}

/// `[notifications]` changed: off withdraws everything, the count withdraws what shows a sender
/// or a subject.
fn apply(settings: Notifications) {
    let notifier = shared();
    let before = notifier.settings.replace(settings);
    if before == settings {
        return;
    }
    tracing::info!(
        enabled = settings.enabled,
        count = settings.content == NotificationContent::Count,
        "notification settings changed"
    );
    let private = settings.content == NotificationContent::Count
        && before.content == NotificationContent::SenderAndSubject;
    if !settings.enabled || private {
        withdraw_all();
    }
}

fn withdraw_all() {
    let notifier = shared();
    let all: Vec<MessageId> = notifier.notified.borrow().iter().map(|n| n.id).collect();
    for id in all {
        forget(id);
    }
    if notifier.count_sent.take().is_some() {
        withdraw(COUNT_ID);
    }
}

fn message_key(id: MessageId) -> String {
    format!("message-{}", id.0)
}

fn withdraw(key: &str) {
    if let Some(app) = app() {
        app.withdraw_notification(key);
    }
    record(Sent::Withdrawn(key.to_owned()));
}

fn record(sent: Sent) {
    let notifier = shared();
    let mut log = notifier.sent.borrow_mut();
    log.push(sent);
    if log.len() > KEPT_SENT {
        log.remove(0);
    }
}

/// Stops tracking `id` and withdraws what shows it: its own notification, or the count once
/// it counts nothing more.
fn forget(id: MessageId) {
    let notifier = shared();
    let removed = {
        let mut notified = notifier.notified.borrow_mut();
        let at = notified.iter().position(|n| n.id == id);
        at.map(|at| notified.remove(at))
    };
    let Some(removed) = removed else {
        return;
    };
    match removed.shown {
        Shown::Alone(_) => withdraw(&message_key(id)),
        Shown::Counted => {
            let counting = notifier
                .notified
                .borrow()
                .iter()
                .any(|n| n.shown == Shown::Counted);
            if !counting && notifier.count_sent.take().is_some() {
                withdraw(COUNT_ID);
            }
        }
    }
}

/// New mail from the engine: the unread messages of an Inbox, read off the GTK thread.
fn arrived(state: &Rc<AppState>, folder: FolderId, messages: &[MessageId]) {
    if !settings().enabled {
        return;
    }
    let inbox = state
        .mailbox()
        .folder(folder)
        .is_some_and(|found| found.role == FolderRole::Inbox);
    if !inbox {
        tracing::debug!(%folder, messages = messages.len(), "new mail outside an Inbox: quiet");
        return;
    }
    let Some(store) = state.mailbox().shared_store() else {
        return;
    };
    let replies: Vec<_> = messages.iter().map(|id| store.message(*id)).collect();
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let mut unread = Vec::new();
        for reply in replies {
            if let Ok(found) = reply.recv().await
                && let Some(detail) = found.value
                && !detail.summary.flags.is_seen()
                && detail.summary.folder == folder
            {
                unread.push(detail.summary);
            }
        }
        let Some(state) = weak.upgrade() else {
            return;
        };
        if unread.is_empty() || !settings().enabled || shows_folder(&state, folder) {
            return;
        }
        show(unread, markup().await);
    });
}

/// Whether the focused main window's list shows `folder`'s mail: its own list, or All Inboxes.
fn shows_folder(state: &AppState, folder: FolderId) -> bool {
    let (Some(window), Some(list)) = (state.window(), state.list()) else {
        return false;
    };
    window.window().is_active()
        && list.query().is_empty()
        && match list.scope() {
            Some(ViewScope::Folder(shown)) => shown == folder,
            Some(ViewScope::UnifiedInbox) => true,
            None => false,
        }
}

/// Whether the notification server takes markup in the body, asked once: the freedesktop
/// server's `body-markup`, unless GLib sends through `org.gtk.Notifications` (GNOME), which
/// takes plain text. Unknown counts as markup, so a subject is escaped rather than read.
async fn markup() -> bool {
    let notifier = shared();
    if let Some(known) = notifier.markup.get() {
        return known;
    }
    let Some(connection) = app().and_then(|app| app.dbus_connection()) else {
        return true;
    };
    let gtk = connection
        .call_future(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&("org.gtk.Notifications",).to_variant()),
            None,
            gio::DBusCallFlags::NO_AUTO_START,
            2_000,
        )
        .await
        .ok()
        .and_then(|reply| reply.get::<(bool,)>())
        .is_some_and(|(owned,)| owned);
    let known = if gtk {
        Some(false)
    } else {
        connection
            .call_future(
                Some("org.freedesktop.Notifications"),
                "/org/freedesktop/Notifications",
                "org.freedesktop.Notifications",
                "GetCapabilities",
                None,
                None,
                gio::DBusCallFlags::NO_AUTO_START,
                2_000,
            )
            .await
            .ok()
            .and_then(|reply| reply.get::<(Vec<String>,)>())
            .map(|(capabilities,)| capabilities.iter().any(|c| c == "body-markup"))
    };
    if let Some(known) = known {
        notifier.markup.set(Some(known));
    }
    known.unwrap_or(true)
}

/// Shows `messages`: each on its own while few are on screen, else one count for them and
/// those on screen.
fn show(mut messages: Vec<MessageSummary>, markup: bool) {
    let Some(app) = app() else {
        return;
    };
    let notifier = shared();
    let settings = notifier.settings.get();
    messages.retain(|message| {
        !notifier
            .notified
            .borrow()
            .iter()
            .any(|n| n.id == message.id)
    });
    if messages.is_empty() {
        return;
    }
    let live = |shown: Shown| matches!(shown, Shown::Alone(at) if at.elapsed() < LIVE);
    let alone_live = notifier
        .notified
        .borrow()
        .iter()
        .filter(|n| live(n.shown))
        .count();
    let count_live = notifier
        .count_sent
        .get()
        .is_some_and(|at| at.elapsed() < LIVE);
    let alone = settings.content == NotificationContent::SenderAndSubject
        && !count_live
        && alone_live + messages.len() <= SEPARATE;
    let now = Instant::now();
    if alone {
        for message in &messages {
            app.send_notification(
                Some(&message_key(message.id)),
                &message_notification(message, markup),
            );
            record(Sent::Message(message.id));
            remember(message, Shown::Alone(now));
        }
        tracing::debug!(messages = messages.len(), "new mail notified, one each");
        return;
    }
    // The count takes over the notifications on screen and counts every message it took that
    // is still new; it replaces itself.
    for message in &messages {
        remember(message, Shown::Counted);
    }
    {
        let mut notified = notifier.notified.borrow_mut();
        for n in notified.iter_mut().filter(|n| live(n.shown)) {
            app.withdraw_notification(&message_key(n.id));
            record(Sent::Withdrawn(message_key(n.id)));
            n.shown = Shown::Counted;
        }
    }
    let counted: Vec<(MessageId, String, i64)> = notifier
        .notified
        .borrow()
        .iter()
        .filter(|n| n.shown == Shown::Counted)
        .map(|n| (n.id, n.sender.clone(), n.date))
        .collect();
    let Some(newest) = counted.iter().max_by_key(|(id, _, date)| (*date, *id)) else {
        return;
    };
    let notification = match settings.content {
        NotificationContent::Count => count_notification(counted.len(), None, newest.0),
        NotificationContent::SenderAndSubject => {
            let mut by_date = counted.clone();
            by_date.sort_by_key(|(id, _, date)| std::cmp::Reverse((*date, *id)));
            let names: Vec<String> = by_date.into_iter().map(|(_, name, _)| name).collect();
            let senders = senders_text(&names);
            let body = if markup {
                escape_markup(&senders)
            } else {
                senders
            };
            count_notification(counted.len(), Some(&body), newest.0)
        }
    };
    app.send_notification(Some(COUNT_ID), &notification);
    notifier.count_sent.set(Some(now));
    record(Sent::Count(counted.len()));
    tracing::debug!(messages = counted.len(), "new mail notified as a count");
}

fn remember(message: &MessageSummary, shown: Shown) {
    let notifier = shared();
    let mut notified = notifier.notified.borrow_mut();
    notified.push(Notified {
        id: message.id,
        sender: sender(message),
        date: message.date,
        shown,
    });
    if notified.len() > REMEMBERED {
        let extra = notified.len() - REMEMBERED;
        notified.drain(..extra);
    }
}

/// A message's notification: the sender, the subject, its id on the actions.
fn message_notification(message: &MessageSummary, markup: bool) -> gio::Notification {
    let notification = gio::Notification::new(&sender(message));
    let subject = subject_text(&message.subject);
    let body = if markup {
        escape_markup(&subject)
    } else {
        subject
    };
    notification.set_body(Some(&body));
    let target = message.id.0.to_variant();
    decorate(&notification, &target);
    notification.add_button_with_target_value(
        ARCHIVE_LABEL,
        &format!("app.{ARCHIVE_ACTION}"),
        Some(&target),
    );
    notification.add_button_with_target_value(
        READ_LABEL,
        &format!("app.{READ_ACTION}"),
        Some(&target),
    );
    notification
}

/// The count: "New mail" and how many (the count mode), or how many and from whom; a click
/// opens the newest.
fn count_notification(count: usize, senders: Option<&str>, newest: MessageId) -> gio::Notification {
    let notification = match senders {
        None => {
            let notification = gio::Notification::new(COUNT_TITLE);
            notification.set_body(Some(&count_text(count)));
            notification
        }
        Some(senders) => {
            let notification = gio::Notification::new(&count_text(count));
            notification.set_body(Some(senders));
            notification
        }
    };
    decorate(&notification, &newest.0.to_variant());
    notification
}

/// What every new-mail notification carries: normal priority (omarchy-shell shows it 8 s; GNOME
/// shows low priority without a banner), the mail category, the app's icon and the open action.
fn decorate(notification: &gio::Notification, target: &glib::Variant) {
    notification.set_priority(gio::NotificationPriority::Normal);
    notification.set_category(Some("email.arrived"));
    notification.set_icon(&gio::ThemedIcon::new(ICON_NAME));
    notification.set_default_action_and_target_value(&format!("app.{OPEN_ACTION}"), Some(target));
}

/// The sender's display name, else the address, cleaned and capped.
fn sender(message: &MessageSummary) -> String {
    let name = clean(&message.sender_name, TITLE_CHARS);
    if !name.is_empty() {
        return name;
    }
    let address = clean(&message.sender_email, TITLE_CHARS);
    if address.is_empty() {
        "Unknown sender".to_owned()
    } else {
        address
    }
}

/// The subject, cleaned and capped, or "(no subject)" as the list says.
pub fn subject_text(subject: &str) -> String {
    let subject = clean(subject, BODY_CHARS);
    if subject.is_empty() {
        "(no subject)".to_owned()
    } else {
        subject
    }
}

/// "1 new message", "5 new messages".
pub fn count_text(count: usize) -> String {
    if count == 1 {
        "1 new message".to_owned()
    } else {
        format!("{count} new messages")
    }
}

/// The senders newest first, each once: "Kari", "Kari and Ola", "Kari, Ola and Leah",
/// "Kari, Ola, Leah and 2 others"; at most [`BODY_CHARS`] characters.
pub fn senders_text(names: &[String]) -> String {
    let mut distinct: Vec<&str> = Vec::new();
    for name in names {
        if !distinct.contains(&name.as_str()) {
            distinct.push(name);
        }
    }
    let text = match distinct.as_slice() {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [first, second] => format!("{first} and {second}"),
        [first, second, third] => format!("{first}, {second} and {third}"),
        [first, second, third, rest @ ..] => {
            let others = if rest.len() == 1 {
                "1 other".to_owned()
            } else {
                format!("{} others", rest.len())
            };
            format!("{first}, {second}, {third} and {others}")
        }
    };
    clean(&text, BODY_CHARS)
}

/// Mail text made safe to show on one line: control characters become spaces, direction
/// overrides and isolates go, runs of white space collapse, and past `max` characters it ends
/// in an ellipsis.
pub fn clean(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut count = 0;
    let mut space = false;
    for c in text.chars() {
        if matches!(
            c,
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{061c}'
        ) {
            continue;
        }
        if c.is_control() || c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            count += 1;
            space = false;
        }
        out.push(c);
        count += 1;
        if count > max {
            break;
        }
    }
    if count <= max {
        return out;
    }
    let mut cut: String = out.chars().take(max.saturating_sub(1)).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

/// The freedesktop body's markup made literal: `&`, `<` and `>`.
pub fn escape_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

/// The store changed: a message notified about that was read, deleted or moved out of its Inbox
/// is withdrawn.
fn store_changed(state: &Rc<AppState>, event: &Arc<ChangeEvent>) {
    let tracked: HashSet<MessageId> = notified().into_iter().collect();
    if tracked.is_empty() {
        return;
    }
    for id in event.removed.iter().filter(|id| tracked.contains(id)) {
        forget(*id);
    }
    let changed: Vec<MessageId> = event
        .changed
        .iter()
        .copied()
        .filter(|id| tracked.contains(id))
        .collect();
    if changed.is_empty() {
        return;
    }
    let Some(store) = state.mailbox().shared_store() else {
        return;
    };
    let replies: Vec<_> = changed.iter().map(|id| (*id, store.message(*id))).collect();
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        for (id, reply) in replies {
            let summary = reply
                .recv()
                .await
                .ok()
                .and_then(|found| found.value)
                .map(|detail| detail.summary);
            let Some(state) = weak.upgrade() else {
                return;
            };
            let still_new = summary.is_some_and(|summary| {
                !summary.flags.is_seen()
                    && state
                        .mailbox()
                        .folder(summary.folder)
                        .is_some_and(|folder| folder.role == FolderRole::Inbox)
            });
            if !still_new {
                forget(id);
            }
        }
    });
}

/// The withdrawal a click on the notification of `id` makes: its own, or the count it is in.
fn clicked(id: MessageId) {
    let notifier = shared();
    let shown = notifier
        .notified
        .borrow()
        .iter()
        .find(|n| n.id == id)
        .map(|n| n.shown);
    match shown {
        Some(Shown::Counted) => {
            let counted: Vec<MessageId> = notifier
                .notified
                .borrow()
                .iter()
                .filter(|n| n.shown == Shown::Counted)
                .map(|n| n.id)
                .collect();
            for id in counted {
                forget(id);
            }
        }
        Some(Shown::Alone(_)) => forget(id),
        None => {}
    }
}

/// The default action: the main window (built when background mode closed it) shows the
/// message in its folder, selected and open in the reader.
fn open(state: &Rc<AppState>, id: MessageId) {
    let action = Instant::now();
    let had_window = state
        .window()
        .is_some_and(|window| window.window().is_visible());
    clicked(id);
    let Some(app) = app() else {
        return;
    };
    state.show_main_window(&app);
    {
        let notifier = shared();
        let mut clicks = notifier.clicks.borrow_mut();
        clicks.push(Click {
            message: id,
            action,
            had_window,
            selected: None,
            painted: None,
        });
        if clicks.len() > KEPT_CLICKS {
            clicks.remove(0);
        }
    }
    tracing::info!(had_window, "a notification was clicked");
    let Some(reply) = state.mailbox().store().map(|store| store.message(id)) else {
        return;
    };
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let summary = reply
            .recv()
            .await
            .ok()
            .and_then(|found| found.value)
            .map(|detail| detail.summary);
        let Some(state) = weak.upgrade() else {
            return;
        };
        let Some(summary) = summary else {
            tracing::info!("the clicked notification's message is gone");
            state.show_toast(&adw::Toast::new(GONE_TEXT), false);
            return;
        };
        if !select(&state, &summary).await {
            return;
        }
        update_click(action, |click| click.selected = Some(Instant::now()));
        let painted = painted(&state, id).await;
        update_click(action, |click| click.painted = painted);
    });
}

/// Fills in the click made at `action`, while it is kept.
fn update_click(action: Instant, update: impl FnOnce(&mut Click)) {
    let notifier = shared();
    let mut clicks = notifier.clicks.borrow_mut();
    if let Some(click) = clicks.iter_mut().rev().find(|click| click.action == action) {
        update(click);
    }
}

/// Shows `message` in the list: its folder unless All Inboxes shows it, no search, a filter
/// that keeps it; then selects it and opens it, as Enter does. False when it never showed.
async fn select(state: &Rc<AppState>, message: &MessageSummary) -> bool {
    let (Some(window), Some(list)) = (state.window(), state.list()) else {
        return false;
    };
    let inbox = state
        .mailbox()
        .folder(message.folder)
        .is_some_and(|folder| folder.role == FolderRole::Inbox);
    let in_all_inboxes = inbox && list.scope() == Some(ViewScope::UnifiedInbox);
    if !in_all_inboxes {
        let key = folder_key(message.folder);
        if window.selected_folder() != key {
            window.select_folder(&key);
        }
    }
    if !list.query().is_empty() {
        list.clear_search();
    }
    if !passes(list.filter(), message) {
        list.set_filter(ViewFilter::All);
    }
    drop((window, list));
    let started = Instant::now();
    loop {
        // Looked up each time: a window closed meanwhile is freed with its list.
        let Some(list) = state.list() else {
            return false;
        };
        let model = list.model();
        if !model.is_pending() {
            if let Some(position) = model.position_of(message.id) {
                list.select_position(position);
                list.open_selected();
                return true;
            }
            if let Some(unloaded) = (0..model.total()).find(|p| model.row_at(*p).is_none()) {
                model.request_position(unloaded);
            }
        }
        if started.elapsed() > OPEN_WAIT {
            tracing::info!("the clicked notification's message didn't show in the list");
            return false;
        }
        glib::timeout_future(Duration::from_millis(5)).await;
    }
}

/// The first frame the window paints with `id` on screen in the reader.
async fn painted(state: &Rc<AppState>, id: MessageId) -> Option<Instant> {
    let (sender, receiver) = async_channel::bounded::<Instant>(1);
    let started = Instant::now();
    let mut hooked: Option<(gtk4::gdk::FrameClock, glib::SignalHandlerId)> = None;
    let mut found = None;
    while started.elapsed() < PAINT_WAIT {
        if hooked.is_none()
            && let Some(clock) = state
                .window()
                .and_then(|window| window.window().frame_clock())
        {
            let weak = Rc::downgrade(state);
            let sender = sender.clone();
            let handler = clock.connect_after_paint(move |_| {
                let shown = weak
                    .upgrade()
                    .and_then(|state| state.reader())
                    .and_then(|reader| reader.message());
                if shown == Some(id) {
                    let _ = sender.try_send(Instant::now());
                }
            });
            clock.begin_updating();
            hooked = Some((clock, handler));
        }
        if let Ok(Ok(at)) =
            glib::future_with_timeout(Duration::from_millis(50), receiver.recv()).await
        {
            found = Some(at);
            break;
        }
    }
    if let Some((clock, handler)) = hooked {
        clock.end_updating();
        clock.disconnect(handler);
    }
    found
}

/// Archive or Mark Read from a notification: withdrawn at once, then queued like the window's
/// own commands (the toast and Undo show in the window when there is one).
fn act(state: &Rc<AppState>, id: MessageId, action: Triage) {
    forget(id);
    let Some(reply) = state.mailbox().store().map(|store| store.message(id)) else {
        return;
    };
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let summary = reply
            .recv()
            .await
            .ok()
            .and_then(|found| found.value)
            .map(|detail| detail.summary);
        let (Some(state), Some(summary)) = (weak.upgrade(), summary) else {
            return;
        };
        let target = Target {
            id,
            folder: summary.folder,
            flags: summary.flags,
        };
        tracing::info!(
            archive = action == Triage::Archive,
            "a notification's button was clicked"
        );
        triage(&state).queue(action, vec![target], Origin::User);
    });
}

/// The window's triage controller, or one of this module's while no window has made one.
fn triage(state: &AppState) -> Controller {
    if let Some(controller) = state.triage() {
        return controller;
    }
    let notifier = shared();
    let mut own = notifier.triage.borrow_mut();
    own.get_or_insert_with(|| {
        let mailbox = state.mailbox();
        let directory: Rc<dyn Directory> = mailbox.clone();
        Controller::new(Rc::new(OpsBackend::new(mailbox)), directory, state.engine())
    })
    .clone()
}

/// The messages notified about, for checks: whether each has a notification of its own (else
/// the count counts it).
pub fn on_screen() -> HashMap<MessageId, bool> {
    shared()
        .notified
        .borrow()
        .iter()
        .map(|n| (n.id, matches!(n.shown, Shown::Alone(_))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mail_text_is_one_safe_line() {
        assert_eq!(clean("  Kari \n Nordmann\t", 80), "Kari Nordmann");
        assert_eq!(clean("Faktura\u{202e}fdp.exe", 80), "Fakturafdp.exe");
        assert_eq!(clean("a\u{0007}b\u{2066}c", 80), "a bc");
        // The Arabic letter mark reorders text too (found in ML-093's review).
        assert_eq!(clean("a\u{061c}b", 80), "ab");
        assert_eq!(clean("", 80), "");
        assert_eq!(clean("abcdef", 4), "abc…");
        assert_eq!(clean("abcd", 4), "abcd");
        assert_eq!(clean("ab cd ef", 5), "ab c…");
        assert_eq!(clean(&"æ".repeat(300), 200).chars().count(), 200);
        assert!(clean(&"æ".repeat(300), 200).ends_with('…'));
    }

    #[test]
    fn markup_in_the_body_is_literal() {
        assert_eq!(
            escape_markup("<a href=\"https://example.org\">Win</a> & more"),
            "&lt;a href=\"https://example.org\"&gt;Win&lt;/a&gt; &amp; more"
        );
        assert_eq!(escape_markup("Blåbær"), "Blåbær");
    }

    #[test]
    fn counts_and_senders_read_as_sentences() {
        assert_eq!(count_text(1), "1 new message");
        assert_eq!(count_text(12), "12 new messages");
        let names = |list: &[&str]| list.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
        assert_eq!(senders_text(&names(&[])), "");
        assert_eq!(senders_text(&names(&["Kari"])), "Kari");
        assert_eq!(senders_text(&names(&["Kari", "Kari"])), "Kari");
        assert_eq!(senders_text(&names(&["Kari", "Ola"])), "Kari and Ola");
        assert_eq!(
            senders_text(&names(&["Kari", "Ola", "Kari", "Leah"])),
            "Kari, Ola and Leah"
        );
        assert_eq!(
            senders_text(&names(&["Kari", "Ola", "Leah", "Ingrid"])),
            "Kari, Ola, Leah and 1 other"
        );
        assert_eq!(
            senders_text(&names(&["Kari", "Ola", "Leah", "Ingrid", "Marius"])),
            "Kari, Ola, Leah and 2 others"
        );
        // Three names of 80 characters and the rest stay within the body's cap (ML-093).
        let long: Vec<String> = ['a', 'b', 'c', 'd']
            .iter()
            .map(|c| c.to_string().repeat(TITLE_CHARS))
            .collect();
        let text = senders_text(&long);
        assert_eq!(text.chars().count(), BODY_CHARS);
        assert!(text.ends_with('…'));
    }

    #[test]
    fn a_message_without_a_sender_or_subject_still_reads() {
        let mut message = MessageSummary {
            id: MessageId(1),
            folder: FolderId(1),
            thread: sealwax_domain::model::ThreadId(1),
            sender_name: String::new(),
            sender_email: "kari@example.test".into(),
            subject: " \n ".into(),
            snippet: String::new(),
            date: 0,
            flags: sealwax_domain::model::Flags::NONE,
            has_attachments: false,
            thread_count: 1,
        };
        assert_eq!(sender(&message), "kari@example.test");
        assert_eq!(subject_text(&message.subject), "(no subject)");
        message.sender_email.clear();
        assert_eq!(sender(&message), "Unknown sender");
        message.sender_name = "Kari Nordmann".into();
        assert_eq!(sender(&message), "Kari Nordmann");
    }
}
