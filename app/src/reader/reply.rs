//! Replying from the reader (M6: ML-068, ML-069; DESIGN.md "Reader" and "Composer", the approved
//! mock). Each expanded card's header ends with Reply, Reply all and Forward (a draft's card with
//! Edit draft instead), acting on that card's message. Under the conversation sits the quick
//! reply: a bordered box saying whom it answers ("Reply to Tomas Berg"), a plain-text area
//! ("Write a reply…"), the hint "Expand to the full composer with Ctrl+Shift+E" and Send
//! (Ctrl+Enter). It answers the latest message of the conversation that isn't the user's own
//! ([`reply_target`]); once it holds text it keeps answering that message whatever arrives.
//!
//! What is typed is kept per message it answers while Sealwax runs, so selecting another message
//! and coming back finds it again (phase B saves it as a draft). The box is the Composer scope:
//! Ctrl+Enter runs `win.send`, the reader's own keys stop at its edge (a text field keeps its
//! letters, and Ctrl+Z, Ctrl+J or Ctrl+R never reach the mail commands from it), except Open
//! Reply in Composer (Ctrl+Shift+E); Escape gives the focus back to the conversation. Every
//! window opens through [`ComposeRequest`]s the app hands to `composer::open`; the quick reply's
//! Send goes through a seam the app sets ([`Reader::set_quick_send`]).

use super::conversation::{Entry, Item, Page};
use super::{Inner, Reader, tooltip};
use crate::composer::ComposeRequest;
use crate::window::panes::icon;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use sealwax_domain::actions::ActionId;
use sealwax_domain::compose::ReplyKind;
use sealwax_domain::keymap::Keymap;
use sealwax_domain::model::{FolderRole, MessageId, MessageSummary};
use sealwax_infrastructure::store::ConversationMessage;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

/// The text area's placeholder (the mock's).
pub const PLACEHOLDER: &str = "Write a reply…";

/// The cards' buttons: the command each stands for, its icons and its accessible label (the
/// mock's "Reply", "Reply all", "Forward").
const REPLIES: [(ActionId, &[&str], &str); 3] = [
    (ActionId::Reply, &["mail-reply-sender-symbolic"], "Reply"),
    (
        ActionId::ReplyAll,
        &["mail-reply-all-symbolic", "mail-reply-sender-symbolic"],
        "Reply all",
    ),
    (ActionId::Forward, &["mail-forward-symbolic"], "Forward"),
];

/// A draft's card offers Edit draft instead: replying to one's own unsent draft makes no sense.
const EDIT: (ActionId, &[&str], &str) = (
    ActionId::EditDraft,
    &["document-edit-symbolic", "mail-message-new-symbolic"],
    "Edit draft",
);

/// A card button's accessible label.
fn button_label(action: ActionId) -> &'static str {
    REPLIES
        .iter()
        .chain([&EDIT])
        .find(|(id, _, _)| *id == action)
        .map_or(action.title(), |(_, _, label)| label)
}

/// The message a quick reply answers, of `messages` oldest first: the latest that isn't the
/// user's own (`mine`, lower case) and isn't in a Drafts folder; when every message is the
/// user's own, the latest that isn't a draft; `None` when there is only drafts.
pub fn reply_target(messages: &[ConversationMessage], mine: &[String]) -> Option<MessageId> {
    let candidates: Vec<&ConversationMessage> = messages
        .iter()
        .filter(|message| message.role != FolderRole::Drafts)
        .collect();
    candidates
        .iter()
        .rev()
        .find(|message| !is_mine(&message.summary, mine))
        .or(candidates.last())
        .map(|message| message.summary.id)
}

fn is_mine(summary: &MessageSummary, mine: &[String]) -> bool {
    mine.iter()
        .any(|address| address.eq_ignore_ascii_case(summary.sender_email.trim()))
}

/// "Reply to Tomas Berg" (the sender's name, else the address); just "Reply" for the user's own
/// message, whose reply goes to its recipients.
pub fn reply_label(summary: &MessageSummary, mine: &[String]) -> String {
    if is_mine(summary, mine) {
        return "Reply".to_owned();
    }
    let name = summary.sender_name.trim();
    let name = if name.is_empty() {
        summary.sender_email.trim()
    } else {
        name
    };
    if name.is_empty() {
        "Reply".to_owned()
    } else {
        format!("Reply to {name}")
    }
}

/// "Expand to the full composer with Ctrl+Shift+E", with the keys in use.
pub fn expand_hint(keymap: &Keymap) -> String {
    let keys = keymap.display(ActionId::ReplyInComposer);
    if keys.is_empty() {
        "Expand to the full composer".to_owned()
    } else {
        format!("Expand to the full composer with {keys}")
    }
}

/// What the window's commands need to know of the quick reply on screen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuickState {
    /// A conversation with a quick reply is on screen.
    pub shown: bool,
    /// It holds text to send.
    pub has_text: bool,
}

/// The quick reply on screen, for checks. `Debug` leaves the text out: it is the user's mail.
#[derive(Clone, PartialEq, Eq)]
pub struct QuickReport {
    pub target: MessageId,
    pub label: String,
    pub text: String,
    pub focused: bool,
    pub send_sensitive: bool,
}

impl fmt::Debug for QuickReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QuickReport")
            .field("target", &self.target)
            .field("text_len", &self.text.len())
            .field("focused", &self.focused)
            .field("send_sensitive", &self.send_sensitive)
            .finish()
    }
}

type ComposeListener = Rc<dyn Fn(ComposeRequest)>;
type StateListener = Rc<dyn Fn(QuickState)>;
/// Sends a quick reply's text as a reply to the message; `true` when it was taken (queued), so
/// the box empties.
pub type QuickSend = Rc<dyn Fn(MessageId, &str) -> bool>;

/// The reader's side of replying: the quick replies' texts, and where requests go.
#[derive(Default)]
pub struct Quick {
    /// What each message's quick reply holds, while it holds anything.
    texts: RefCell<HashMap<MessageId, String>>,
    on_compose: RefCell<Vec<ComposeListener>>,
    send: RefCell<Option<QuickSend>>,
    on_state: RefCell<Vec<StateListener>>,
    state: Cell<QuickState>,
}

/// One conversation's quick reply.
pub struct QuickReply {
    pub root: gtk::Box,
    label: gtk::Label,
    view: gtk::TextView,
    placeholder: gtk::Label,
    hint: gtk::Label,
    send: gtk::Button,
    send_keys: gtk::Label,
    target: Cell<MessageId>,
}

impl QuickReply {
    fn new(target: MessageId, label: &str, text: &str, keymap: &Keymap) -> Rc<Self> {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["sw-quick-reply"])
            .build();
        root.set_widget_name("quick-reply");
        let heading = gtk::Label::builder()
            .label(label)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["sw-small", "sw-secondary", "sw-quick-reply-label"])
            .build();
        heading.set_widget_name("quick-reply-label");
        root.append(&heading);
        let view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .accepts_tab(false)
            .hexpand(true)
            .css_classes(["sw-reading", "sw-quick-reply-text"])
            .build();
        view.set_widget_name("quick-reply-text");
        view.buffer().set_text(text);
        view.update_property(&[
            gtk::accessible::Property::Label(label),
            gtk::accessible::Property::Placeholder(PLACEHOLDER),
            gtk::accessible::Property::MultiLine(true),
        ]);
        let placeholder = gtk::Label::builder()
            .label(PLACEHOLDER)
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .can_target(false)
            .visible(text.is_empty())
            .css_classes(["sw-reading", "sw-secondary", "sw-quick-reply-placeholder"])
            .build();
        placeholder.set_widget_name("quick-reply-placeholder");
        placeholder.set_accessible_role(gtk::AccessibleRole::Presentation);
        let area = gtk::Overlay::builder().child(&view).build();
        area.add_overlay(&placeholder);
        root.append(&area);
        let hint = gtk::Label::builder()
            .label(expand_hint(keymap))
            .xalign(0.0)
            .css_classes(["sw-small", "sw-secondary"])
            .build();
        let expand = gtk::Button::builder()
            .child(&hint)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Center)
            .action_name(ActionId::ReplyInComposer.detailed_name())
            .css_classes(["flat", "sw-quick-reply-expand"])
            .build();
        expand.set_widget_name("quick-reply-expand");
        let spacer = gtk::Box::builder().hexpand(true).build();
        let send_title = gtk::Label::builder()
            .label("Send")
            .css_classes(["sw-strong"])
            .build();
        let send_keys = gtk::Label::builder()
            .css_classes(["sw-quick-reply-send-keys"])
            .build();
        let send_box = gtk::Box::builder().spacing(8).build();
        send_box.append(&send_title);
        send_box.append(&send_keys);
        let send = gtk::Button::builder()
            .child(&send_box)
            .valign(gtk::Align::Center)
            .action_name(ActionId::Send.detailed_name())
            .css_classes(["sw-quick-reply-send"])
            .build();
        send.set_widget_name("quick-reply-send");
        send.update_property(&[gtk::accessible::Property::Label("Send")]);
        let row = gtk::Box::builder()
            .spacing(8)
            .css_classes(["sw-quick-reply-actions"])
            .build();
        row.append(&expand);
        row.append(&spacer);
        row.append(&send);
        root.append(&row);
        root.update_property(&[gtk::accessible::Property::Label("Quick reply")]);
        let quick = Rc::new(Self {
            root,
            label: heading,
            view,
            placeholder,
            hint,
            send,
            send_keys,
            target: Cell::new(target),
        });
        quick.update_keys(keymap);
        quick
    }

    pub fn target(&self) -> MessageId {
        self.target.get()
    }

    pub fn text(&self) -> String {
        let buffer = self.view.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string()
    }

    fn has_text(&self) -> bool {
        !self.text().trim().is_empty()
    }

    fn set_text(&self, text: &str) {
        self.view.buffer().set_text(text);
    }

    fn retarget(&self, target: MessageId, label: &str, text: &str) {
        self.target.set(target);
        self.label.set_label(label);
        self.view
            .update_property(&[gtk::accessible::Property::Label(label)]);
        self.set_text(text);
    }

    fn update_keys(&self, keymap: &Keymap) {
        self.hint.set_label(&expand_hint(keymap));
        let keys = keymap.display(ActionId::Send);
        self.send_keys.set_label(&keys);
        self.send_keys.set_visible(!keys.is_empty());
        self.send
            .set_tooltip_text(Some(&tooltip(keymap, ActionId::Send, "Send", None)));
    }

    fn is_focused(&self) -> bool {
        self.view.has_focus()
    }
}

/// The messages of `page`, oldest first, folded ones included.
fn page_messages(page: &Page) -> Vec<ConversationMessage> {
    let mut messages = Vec::new();
    for entry in &page.entries {
        match entry {
            Entry::Message(item) => messages.push(item.message.clone()),
            Entry::Fold(fold) => messages.extend(fold.messages.iter().cloned()),
        }
    }
    messages
}

/// Whether `event` is one of `triggers`' keys.
fn matches(event: &gdk::Event, triggers: &[gtk::ShortcutTrigger]) -> bool {
    triggers
        .iter()
        .any(|trigger| trigger.trigger(event, false) == gdk::KeyMatch::Exact)
}

/// The scoped keys of `id` in `keymap`, as GTK triggers.
fn triggers(keymap: &Keymap, id: ActionId) -> Vec<gtk::ShortcutTrigger> {
    keymap
        .scoped(id)
        .iter()
        .filter_map(|binding| binding.single())
        .filter_map(|key| gtk::ShortcutTrigger::parse_string(&key.canonical()))
        .collect()
}

impl Reader {
    /// Runs `listener` with every composer window the reader asks for: a card's Reply, Reply
    /// all, Forward or Edit draft, and the quick reply's expansion. The app opens them
    /// (`composer::open`).
    pub fn connect_compose(&self, listener: impl Fn(ComposeRequest) + 'static) {
        self.inner
            .quick
            .on_compose
            .borrow_mut()
            .push(Rc::new(listener));
    }

    /// What the quick reply's Send does with its text (phase B: planned as a reply, saved and
    /// queued with Undo send). Without one, Send keeps the text.
    pub fn set_quick_send(&self, send: impl Fn(MessageId, &str) -> bool + 'static) {
        self.inner.quick.send.replace(Some(Rc::new(send)));
    }

    /// Runs `listener` when the quick reply on screen comes or goes, or gets or loses its text.
    pub fn connect_quick_state(&self, listener: impl Fn(QuickState) + 'static) {
        self.inner
            .quick
            .on_state
            .borrow_mut()
            .push(Rc::new(listener));
    }

    pub fn quick_state(&self) -> QuickState {
        self.inner.quick.state.get()
    }

    /// The quick reply on screen, for checks.
    pub fn quick_reply(&self) -> Option<QuickReport> {
        let quick = self.inner.shown_quick()?;
        Some(QuickReport {
            target: quick.target(),
            label: quick.label.label().to_string(),
            text: quick.text(),
            focused: quick.is_focused(),
            send_sensitive: quick.send.is_sensitive(),
        })
    }

    /// Quick Reply: the focus goes into the quick reply, scrolled into view.
    pub fn focus_quick_reply(&self) {
        self.inner.focus_quick_reply();
    }

    /// Open Reply in Composer (Ctrl+Shift+E): what the quick reply holds goes to a composer
    /// window as a reply to its message, and the box empties.
    pub fn expand_quick_reply(&self) {
        self.inner.expand_quick_reply();
    }

    /// Send (Ctrl+Enter) from the quick reply: its text goes to the send seam; the box empties
    /// once the seam took it.
    pub fn send_quick_reply(&self) {
        self.inner.send_quick_reply();
    }

    /// The message of the shown conversation the user is on, for Reply, Reply All and Forward:
    /// the one whose row or card holds the focus, else the one Next or Previous Message in
    /// Conversation last reached, else the selected one.
    pub fn current_message(&self) -> Option<MessageId> {
        self.inner.current_message()
    }
}

impl Inner {
    fn compose(&self, request: ComposeRequest) {
        let listeners = self.quick.on_compose.borrow().clone();
        for listener in listeners {
            listener(request.clone());
        }
    }

    /// The buttons at the end of message `id`'s card header: Reply, Reply all and Forward, or
    /// Edit draft for a draft. `header` is the card's sender block.
    pub(super) fn reply_buttons(
        self: &Rc<Self>,
        header: &gtk::Button,
        id: MessageId,
        draft: bool,
    ) -> Vec<(ActionId, gtk::Button)> {
        let Some(row) = header.parent().and_downcast::<gtk::Box>() else {
            return Vec::new();
        };
        let specs: Vec<(ActionId, &[&str], &str)> =
            if draft { vec![EDIT] } else { REPLIES.to_vec() };
        let keymap = self.keymap.borrow().clone();
        specs
            .into_iter()
            .map(|(action, icons, label)| {
                let button = gtk::Button::builder()
                    .child(&icon(icons))
                    .valign(gtk::Align::Center)
                    .tooltip_text(tooltip(&keymap, action, label, None))
                    .css_classes(["flat", "sw-icon-button", "sw-card-reply"])
                    .build();
                button.set_widget_name(&format!("reader-card-{}", action.name()));
                button.update_property(&[gtk::accessible::Property::Label(label)]);
                let request = move || match action {
                    ActionId::ReplyAll => ComposeRequest::Reply {
                        message: id,
                        kind: ReplyKind::ReplyAll,
                    },
                    ActionId::Forward => ComposeRequest::Reply {
                        message: id,
                        kind: ReplyKind::Forward,
                    },
                    ActionId::EditDraft => ComposeRequest::ServerDraft(id),
                    _ => ComposeRequest::Reply {
                        message: id,
                        kind: ReplyKind::Reply,
                    },
                };
                let weak = Rc::downgrade(self);
                button.connect_clicked(move |_| {
                    if let Some(inner) = weak.upgrade() {
                        inner.compose(request());
                    }
                });
                row.append(&button);
                (action, button)
            })
            .collect()
    }

    /// `keys.toml` changed: the cards' buttons and the quick reply name the new keys.
    pub(super) fn update_reply_keys(&self, keymap: &Keymap) {
        self.keymap.replace(keymap.clone());
        self.composer_keys.update(keymap);
        for pages in [&self.shown, &self.pending] {
            if let Some(page) = pages.borrow().as_ref() {
                for item in page.items() {
                    for (action, button) in item.card.iter().flat_map(|card| &card.replies) {
                        let label = button_label(*action);
                        button.set_tooltip_text(Some(&tooltip(keymap, *action, label, None)));
                    }
                }
                if let Some(quick) = &page.quick {
                    quick.update_keys(keymap);
                }
            }
        }
    }

    /// The quick reply of the conversation on screen.
    fn shown_quick(&self) -> Option<Rc<QuickReply>> {
        self.shown
            .borrow()
            .as_ref()
            .and_then(|page| page.quick.clone())
    }

    /// Puts a quick reply under page `key`'s conversation, answering [`reply_target`], with
    /// what it held for that message.
    pub(super) fn attach_quick_reply(self: &Rc<Self>, key: u64) {
        let mine = self.mine.borrow().clone();
        let target = self
            .with_page(key, |page| {
                if page.quick.is_some() || page.missing {
                    return None;
                }
                let messages = page_messages(page);
                let target = reply_target(&messages, &mine)?;
                let summary = messages
                    .iter()
                    .find(|message| message.summary.id == target)?
                    .summary
                    .clone();
                Some((target, summary))
            })
            .flatten();
        let Some((target, summary)) = target else {
            return;
        };
        let text = self
            .quick
            .texts
            .borrow()
            .get(&target)
            .cloned()
            .unwrap_or_default();
        let keymap = self.keymap.borrow().clone();
        let quick = QuickReply::new(target, &reply_label(&summary, &mine), &text, &keymap);
        self.wire_quick_reply(key, &quick);
        self.with_page(key, |page| {
            page.root.append(&quick.root);
            page.quick = Some(quick.clone());
        });
        if self.is_shown(key) {
            self.quick_changed();
        }
    }

    fn wire_quick_reply(self: &Rc<Self>, key: u64, quick: &Rc<QuickReply>) {
        let weak = Rc::downgrade(self);
        let shown = Rc::downgrade(quick);
        quick.view.buffer().connect_changed(move |_| {
            if let (Some(inner), Some(quick)) = (weak.upgrade(), shown.upgrade()) {
                inner.quick_text_changed(key, &quick);
            }
        });
        self.composer_keys
            .attach(&quick.root, gtk::PropagationPhase::Capture);
        // With nothing to send, Send's keys do nothing (GtkTextView would take Ctrl+Enter as a
        // new line).
        let empty = gtk::EventControllerKey::new();
        empty.set_propagation_phase(gtk::PropagationPhase::Capture);
        empty.set_name(Some("sealwax-quick-reply-send"));
        let weak = Rc::downgrade(self);
        let shown = Rc::downgrade(quick);
        empty.connect_key_pressed(move |controller, _, _, _| {
            let (Some(inner), Some(quick), Some(event)) =
                (weak.upgrade(), shown.upgrade(), controller.current_event())
            else {
                return glib::Propagation::Proceed;
            };
            let send = triggers(&inner.keymap.borrow(), ActionId::Send);
            if matches(&event, &send) && !quick.has_text() {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        quick.root.add_controller(empty);
        // The reader's keys stop at the box's edge: what the text area didn't take (Escape,
        // Ctrl+Z with nothing to undo, Ctrl+J, Ctrl+R…) never reaches a mail command. Open Reply
        // in Composer passes; Escape gives the focus back to the conversation.
        let guard = gtk::EventControllerKey::new();
        guard.set_name(Some("sealwax-quick-reply-guard"));
        let weak = Rc::downgrade(self);
        guard.connect_key_pressed(move |controller, key, _, state| {
            let (Some(inner), Some(event)) = (weak.upgrade(), controller.current_event()) else {
                return glib::Propagation::Proceed;
            };
            if key == gdk::Key::Escape && state.is_empty() {
                inner.leave_quick_reply();
                return glib::Propagation::Stop;
            }
            let expand = triggers(&inner.keymap.borrow(), ActionId::ReplyInComposer);
            if matches(&event, &expand) {
                return glib::Propagation::Proceed;
            }
            let model = inner.keys.model();
            let ours = (0..model.n_items())
                .filter_map(|index| model.item(index).and_downcast::<gtk::Shortcut>())
                .filter_map(|shortcut| shortcut.trigger())
                .any(|trigger| trigger.trigger(&event, false) == gdk::KeyMatch::Exact);
            if ours {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        quick.root.add_controller(guard);
    }

    fn quick_text_changed(&self, key: u64, quick: &QuickReply) {
        let text = quick.text();
        quick.placeholder.set_visible(text.is_empty());
        {
            let mut texts = self.quick.texts.borrow_mut();
            if text.is_empty() {
                texts.remove(&quick.target());
            } else {
                texts.insert(quick.target(), text);
            }
        }
        if self.is_shown(key) {
            self.quick_changed();
            if quick.is_focused()
                && let Some(viewport) = quick
                    .root
                    .ancestor(gtk::Viewport::static_type())
                    .and_downcast::<gtk::Viewport>()
            {
                viewport.scroll_to(&quick.root, None);
            }
        }
    }

    /// Tells the listeners what the quick reply on screen is now, when that changed.
    pub(super) fn quick_changed(&self) {
        let state = self
            .shown_quick()
            .map_or(QuickState::default(), |quick| QuickState {
                shown: true,
                has_text: quick.has_text(),
            });
        if self.quick.state.replace(state) == state {
            return;
        }
        let listeners = self.quick.on_state.borrow().clone();
        for listener in listeners {
            listener(state);
        }
    }

    /// The shown conversation changed in place: an empty quick reply follows the message it
    /// should answer now (a reply arrived); one with text keeps its message.
    pub(super) fn retarget_quick_reply(self: &Rc<Self>, key: u64) {
        let has_quick = self
            .with_page(key, |page| page.quick.is_some())
            .unwrap_or(false);
        if !has_quick {
            self.attach_quick_reply(key);
            return;
        }
        let mine = self.mine.borrow().clone();
        let change = self
            .with_page(key, |page| {
                let quick = page.quick.clone()?;
                if quick.has_text() {
                    return None;
                }
                let messages = page_messages(page);
                let target = reply_target(&messages, &mine)?;
                if target == quick.target() {
                    return None;
                }
                let summary = messages
                    .iter()
                    .find(|message| message.summary.id == target)?
                    .summary
                    .clone();
                Some((quick, target, summary))
            })
            .flatten();
        if let Some((quick, target, summary)) = change {
            let text = self
                .quick
                .texts
                .borrow()
                .get(&target)
                .cloned()
                .unwrap_or_default();
            quick.retarget(target, &reply_label(&summary, &mine), &text);
        }
        if self.is_shown(key) {
            self.quick_changed();
        }
    }

    fn focus_quick_reply(&self) {
        let Some(quick) = self.shown_quick() else {
            return;
        };
        quick.view.grab_focus();
        if let Some(viewport) = quick
            .root
            .ancestor(gtk::Viewport::static_type())
            .and_downcast::<gtk::Viewport>()
        {
            viewport.scroll_to(&quick.root, None);
        }
    }

    fn expand_quick_reply(&self) {
        let Some(quick) = self.shown_quick() else {
            return;
        };
        let request = ComposeRequest::QuickReply {
            message: quick.target(),
            text: quick.text(),
        };
        quick.set_text("");
        self.compose(request);
    }

    fn send_quick_reply(&self) {
        let Some(quick) = self.shown_quick() else {
            return;
        };
        if !quick.has_text() {
            return;
        }
        let Some(send) = self.quick.send.borrow().clone() else {
            tracing::warn!("reader: the quick reply has nowhere to send to; its text stays");
            return;
        };
        if send(quick.target(), &quick.text()) {
            quick.set_text("");
        }
    }

    /// Escape in the quick reply: the focus goes to the message it answers, else the selected
    /// one.
    fn leave_quick_reply(&self) {
        let shown = self.shown.borrow();
        let Some(page) = shown.as_ref() else {
            return;
        };
        let target = page.quick.as_ref().map(|quick| quick.target());
        let focus = [target, Some(page.selected)]
            .into_iter()
            .flatten()
            .filter_map(|id| page.item(id))
            .find_map(|item| match &item.card {
                Some(card) => card.focus_target(),
                None => Some(item.row.clone().upcast()),
            });
        drop(shown);
        match focus {
            Some(widget) => {
                widget.grab_focus();
            }
            None => {
                self.stage.grab_focus();
            }
        }
    }

    fn current_message(&self) -> Option<MessageId> {
        let shown = self.shown.borrow();
        let page = shown.as_ref().filter(|page| !page.missing)?;
        let focus = self.stage.root().and_then(|root| root.focus());
        if let Some(focus) = focus
            && let Some(item) = page.items().find(|item| focus.is_ancestor(&item.slot))
        {
            return Some(item.id());
        }
        let ids: Vec<MessageId> = page.items().map(Item::id).collect();
        Some(
            page.current
                .filter(|id| ids.contains(id))
                .unwrap_or(page.selected),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::model::{Flags, FolderId, ThreadId};

    fn message(id: i64, from: &str, role: FolderRole) -> ConversationMessage {
        ConversationMessage {
            summary: MessageSummary {
                id: MessageId(id),
                folder: FolderId(1),
                thread: ThreadId(1),
                sender_name: String::new(),
                sender_email: from.into(),
                subject: "Hyttetur".into(),
                snippet: String::new(),
                date: id,
                flags: Flags::SEEN,
                has_attachments: false,
                thread_count: 1,
            },
            role,
            copies: Vec::new(),
        }
    }

    #[test]
    fn the_quick_reply_answers_the_latest_message_not_the_users_own() {
        let mine = vec!["alex@example.test".to_owned()];
        let inbox = FolderRole::Inbox;
        let conversation = [
            message(1, "ingrid@example.test", inbox),
            message(2, "tomas@example.test", inbox),
            message(3, "Alex@Example.test", FolderRole::Sent),
        ];
        assert_eq!(reply_target(&conversation, &mine), Some(MessageId(2)));
        // A draft is never answered, the user's own or not.
        let with_draft = [
            message(1, "ingrid@example.test", inbox),
            message(2, "sana@example.test", FolderRole::Drafts),
        ];
        assert_eq!(reply_target(&with_draft, &mine), Some(MessageId(1)));
        // Only the user's own messages: the latest of them.
        let own = [
            message(1, "alex@example.test", FolderRole::Sent),
            message(2, "alex@example.test", FolderRole::Sent),
            message(3, "alex@example.test", FolderRole::Drafts),
        ];
        assert_eq!(reply_target(&own, &mine), Some(MessageId(2)));
        let drafts = [message(1, "alex@example.test", FolderRole::Drafts)];
        assert_eq!(reply_target(&drafts, &mine), None);
        assert_eq!(reply_target(&[], &mine), None);
        // Without the user's addresses, the latest.
        assert_eq!(reply_target(&conversation, &[]), Some(MessageId(3)));
    }

    #[test]
    fn labels_name_whom_the_reply_goes_to() {
        let mine = vec!["alex@example.test".to_owned()];
        let mut summary = message(1, "tomas@example.test", FolderRole::Inbox).summary;
        assert_eq!(reply_label(&summary, &mine), "Reply to tomas@example.test");
        summary.sender_name = "Tomas Berg".into();
        assert_eq!(reply_label(&summary, &mine), "Reply to Tomas Berg");
        summary.sender_email = "alex@example.test".into();
        assert_eq!(reply_label(&summary, &mine), "Reply");
        let nobody = message(2, "", FolderRole::Inbox).summary;
        assert_eq!(reply_label(&nobody, &mine), "Reply");
    }

    #[test]
    fn the_hint_names_the_keys_in_use() {
        let keymap = Keymap::default();
        assert_eq!(
            expand_hint(&keymap),
            "Expand to the full composer with Ctrl+Shift+E"
        );
        let unbound = Keymap::with_overrides([(ActionId::ReplyInComposer, Vec::new())]);
        assert_eq!(expand_hint(&unbound), "Expand to the full composer");
    }

    #[test]
    fn a_report_prints_no_text() {
        let report = QuickReport {
            target: MessageId(4),
            label: "Reply to Tomas Berg".into(),
            text: "See you Friday".into(),
            focused: true,
            send_sensitive: true,
        };
        let printed = format!("{report:?}");
        assert!(
            !printed.contains("Friday") && !printed.contains("Tomas"),
            "{printed}"
        );
    }
}
