//! Remote content (ML-058; SECURITY.md "Message rendering", rules 3 and 9): whose remote images
//! load, the banner that says a message's images are hidden, with Show images and Always for
//! this sender, and the quiet line a message from an allowed sender shows instead, with Hide.
//! Nothing remote loads until the user allows it for that message (this session, nothing
//! stored) or for its sender (the store's list, schema v8). The list is read once as the reader
//! starts, off the GTK thread, and kept in memory; allowing or forgetting a sender changes it at
//! once and writes it through the store. Every card of the shown conversation follows the policy
//! through `Inner::refresh_remote`: a sender allowed or forgotten builds that sender's expanded
//! cards again; collapsed ones take the policy when expanded.
//!
//! A forged From address can make a listed sender's images load in someone else's message; the
//! images still come only over https, from a session that keeps no cookies (rules 2 and 5).

use super::conversation::Entry;
use super::{Inner, Reader};
use crate::window::panes::icon;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::model::MessageId;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

/// How long the reader waits for a store to read the list from, when it starts before the
/// store is open.
const LOAD_TRIES: u32 = 100;
const LOAD_RETRY: Duration = Duration::from_millis(100);

type Listener = Rc<dyn Fn()>;

/// Whose remote images load: the messages the user allowed this session (nothing is stored),
/// and the senders on the store's list (lower case).
#[derive(Default)]
pub struct Policy {
    messages: RefCell<HashSet<MessageId>>,
    senders: RefCell<HashSet<String>>,
    /// The store's list has been read.
    loaded: Cell<bool>,
    listeners: RefCell<Vec<Listener>>,
}

/// What a message shows about its remote images.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// No remote images, or the user allowed them for this message this session.
    None,
    /// They are hidden: the banner, from this sender (lower case; empty without one).
    Hidden(String),
    /// They load because this sender is on the list: the quiet line, with Hide.
    SenderAllowed(String),
}

impl Policy {
    /// Whether `message`, from `sender` (its From address, lower case), may load remote images.
    pub fn allows(&self, message: MessageId, sender: &str) -> bool {
        self.messages.borrow().contains(&message) || self.sender_allowed(sender)
    }

    /// The user allowed `message`'s remote images until Sealwax quits.
    pub fn allow_message(&self, message: MessageId) {
        self.messages.borrow_mut().insert(message);
    }

    /// Whether `sender` is on the list of senders whose images load.
    pub fn sender_allowed(&self, sender: &str) -> bool {
        !sender.is_empty() && self.senders.borrow().contains(sender)
    }

    /// What `message` from `sender` shows, when it has remote images (`remote_images`).
    pub fn notice(&self, message: MessageId, sender: &str, remote_images: bool) -> Notice {
        if !remote_images {
            Notice::None
        } else if self.sender_allowed(sender) {
            Notice::SenderAllowed(sender.to_owned())
        } else if self.messages.borrow().contains(&message) {
            Notice::None
        } else {
            Notice::Hidden(sender.to_owned())
        }
    }

    /// The images `message` was allowed this session are hidden again.
    pub fn forget_message(&self, message: MessageId) {
        self.messages.borrow_mut().remove(&message);
    }

    /// Whether the store's list has been read.
    pub fn is_loaded(&self) -> bool {
        self.loaded.get()
    }

    fn set_sender(&self, sender: &str, allowed: bool) {
        if allowed {
            self.senders.borrow_mut().insert(sender.to_owned());
        } else {
            self.senders.borrow_mut().remove(sender);
        }
    }

    /// Calls `listener` whenever what the shown message's images do may have changed.
    pub fn connect_changed(&self, listener: impl Fn() + 'static) {
        self.listeners.borrow_mut().push(Rc::new(listener));
    }

    pub(super) fn changed(&self) {
        let listeners = self.listeners.borrow().clone();
        for listener in listeners {
            listener();
        }
    }
}

/// Reads the store's list off the GTK thread as the reader starts; a message shown before it
/// arrived is shown again if its sender is on it. Without an open store yet, it asks again for
/// a while.
pub(super) fn load(inner: &Rc<Inner>) {
    let weak = Rc::downgrade(inner);
    glib::spawn_future_local(async move {
        for _ in 0..LOAD_TRIES {
            let Some(reply) = weak.upgrade().map(|inner| inner.store.remote_senders()) else {
                return;
            };
            let Some(reply) = reply else {
                glib::timeout_future(LOAD_RETRY).await;
                continue;
            };
            let senders = match reply.recv().await {
                Ok(senders) => senders,
                Err(error) => {
                    tracing::warn!(%error, "reader: the senders whose images load couldn't be read");
                    return;
                }
            };
            let Some(inner) = weak.upgrade() else {
                return;
            };
            tracing::debug!(
                senders = senders.len(),
                "reader: the senders whose images load"
            );
            for sender in &senders {
                inner.remote.set_sender(sender, true);
            }
            inner.remote.loaded.set(true);
            inner.refresh_remote();
            inner.remote.changed();
            return;
        }
        tracing::warn!("reader: no store to read the senders whose images load from");
    });
}

/// The selected message of the shown conversation, once its card is built: its id, its sender
/// (lower case) and whether its banner says its images are hidden.
pub(super) fn shown(reader: &Reader) -> Option<(MessageId, String, bool)> {
    let shown = reader.inner.shown.borrow();
    let page = shown.as_ref()?;
    let card = page.selected_card()?;
    let banner = card
        .widgets
        .banner
        .as_ref()
        .is_some_and(|banner| banner.root.parent().is_some());
    Some((page.selected, card.sender.clone(), banner))
}

/// Images from the selected message's sender load from now on (`on`), or are hidden again.
pub(super) fn set_always(reader: &Reader, on: bool) {
    if let Some((_, sender, _)) = shown(reader)
        && !sender.is_empty()
    {
        set_sender(&reader.inner, &sender, on);
    }
}

/// Puts `sender` on the list or takes it off, in memory at once and in the store, and builds
/// the sender's expanded cards again. Taken off, the images of the sender's messages in the
/// shown conversation that were allowed this session are hidden again too.
fn set_sender(inner: &Rc<Inner>, sender: &str, allowed: bool) {
    if inner.remote.sender_allowed(sender) == allowed {
        inner.remote.changed();
        return;
    }
    inner.remote.set_sender(sender, allowed);
    if allowed {
        inner.store.allow_remote_sender(sender);
    } else {
        inner.store.forget_remote_sender(sender);
        for id in messages_from(inner, sender) {
            inner.remote.forget_message(id);
        }
    }
    tracing::debug!(allowed, "reader: a sender's images load without asking");
    inner.refresh_remote();
    inner.remote.changed();
}

/// The messages from `sender` (lower case) in the shown and the pending conversation, folded
/// ones included.
fn messages_from(inner: &Inner, sender: &str) -> Vec<MessageId> {
    let mut out = Vec::new();
    for pages in [&inner.shown, &inner.pending] {
        let pages = pages.borrow();
        let Some(page) = pages.as_ref() else {
            continue;
        };
        for entry in &page.entries {
            match entry {
                Entry::Message(item) => {
                    let from = item.card.as_ref().map_or_else(
                        || item.message.summary.sender_email.to_lowercase(),
                        |card| card.sender.clone(),
                    );
                    if from == sender {
                        out.push(item.id());
                    }
                }
                Entry::Fold(fold) => out.extend(
                    fold.messages
                        .iter()
                        .filter(|message| message.summary.sender_email.to_lowercase() == sender)
                        .map(|message| message.summary.id),
                ),
            }
        }
    }
    out
}

/// The banner of one message whose images are hidden: "Images are hidden so the sender can't
/// tell you opened this.", Show images (the reader connects it) and Always for this sender.
pub struct Banner {
    pub root: gtk::Box,
    pub show_images: gtk::Button,
}

pub fn banner(reader: &Reader, sender: &str) -> Banner {
    let root = gtk::Box::builder()
        .spacing(12)
        .css_classes(["sw-banner", "sw-reader-banner"])
        .build();
    root.set_widget_name("reader-banner");
    root.update_property(&[gtk::accessible::Property::Label("Remote images")]);
    let image = icon(&["image-missing-symbolic", "image-x-generic-symbolic"]);
    image.add_css_class("sw-accent");
    root.append(&image);
    let text = gtk::Label::builder()
        .label("Images are hidden so the sender can't tell you opened this.")
        .xalign(0.0)
        .wrap(true)
        .hexpand(true)
        .build();
    root.append(&text);
    let show_images = gtk::Button::builder()
        .label("Show images")
        .valign(gtk::Align::Center)
        .css_classes(["sw-control"])
        .build();
    show_images.set_widget_name("reader-show-images");
    let weak = Rc::downgrade(&reader.inner);
    show_images.connect_clicked(move |_| {
        let weak = weak.clone();
        glib::idle_add_local_once(move || {
            if let Some(inner) = weak.upgrade() {
                inner.remote.changed();
            }
        });
    });
    root.append(&show_images);
    let always = gtk::Button::builder()
        .label("Always for this sender")
        .valign(gtk::Align::Center)
        .css_classes(["sw-control"])
        .build();
    always.set_widget_name("reader-always-show-images");
    if sender.is_empty() {
        always.set_sensitive(false);
        always.set_tooltip_text(Some("This message has no sender address"));
    } else {
        always.set_tooltip_text(Some(
            "Load images from this sender without asking, in every message",
        ));
        let weak = Rc::downgrade(&reader.inner);
        let sender = sender.to_owned();
        always.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                set_sender(&inner, &sender, true);
            }
        });
    }
    root.append(&always);
    Banner { root, show_images }
}

/// The quiet line of a message whose images load because its sender is on the list, with Hide,
/// which takes the sender off it.
pub fn sender_note(reader: &Reader, sender: &str) -> gtk::Box {
    let root = gtk::Box::builder()
        .spacing(10)
        .css_classes(["sw-reader-sender-note"])
        .build();
    root.set_widget_name("reader-sender-note");
    root.update_property(&[gtk::accessible::Property::Label("Remote images")]);
    let image = icon(&["image-x-generic-symbolic"]);
    image.add_css_class("sw-secondary");
    root.append(&image);
    let text = gtk::Label::builder()
        .label("Images from this sender are always shown.")
        .xalign(0.0)
        .wrap(true)
        .hexpand(true)
        .css_classes(["sw-secondary", "sw-small"])
        .build();
    root.append(&text);
    let hide = gtk::Button::builder()
        .label("Hide")
        .valign(gtk::Align::Center)
        .tooltip_text("Hide this sender's images again and ask first")
        .css_classes(["flat", "sw-reader-sender-hide"])
        .build();
    hide.set_widget_name("reader-hide-images");
    let weak = Rc::downgrade(&reader.inner);
    let sender = sender.to_owned();
    hide.connect_clicked(move |_| {
        if let Some(inner) = weak.upgrade() {
            set_sender(&inner, &sender, false);
        }
    });
    root.append(&hide);
    root
}

/// Calls `listener` whenever what the selected message's images do may have changed: the list
/// was read, a sender was allowed or forgotten, Show images was clicked, the selected message's
/// card was built.
pub fn connect_changed(reader: &Reader, listener: impl Fn() + 'static) {
    reader.inner.remote.connect_changed(listener);
}

impl Reader {
    /// Whether the list of senders whose images load has been read from the store.
    pub fn remote_senders_loaded(&self) -> bool {
        self.inner.remote.is_loaded()
    }

    /// Whether `sender` (lower case) is on the list of senders whose images load.
    pub fn remote_sender_allowed(&self, sender: &str) -> bool {
        self.inner.remote.sender_allowed(sender)
    }

    /// Whether message `id`'s HTML body, expanded in the shown conversation, loads its remote
    /// images, for checks.
    pub fn remote_allowed(&self, id: MessageId) -> bool {
        self.inner
            .shown
            .borrow()
            .as_ref()
            .and_then(|page| page.item(id))
            .and_then(|item| item.card.as_ref())
            .and_then(|card| card.html.as_ref())
            .is_some_and(|html| html.remote_allowed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_or_its_sender_lets_images_load() {
        let policy = Policy::default();
        let one = MessageId(1);
        let two = MessageId(2);
        assert!(!policy.allows(one, "news@northwind.example.test"));
        assert_eq!(
            policy.notice(one, "news@northwind.example.test", true),
            Notice::Hidden("news@northwind.example.test".into())
        );
        assert_eq!(
            policy.notice(one, "news@northwind.example.test", false),
            Notice::None
        );
        policy.allow_message(one);
        assert!(policy.allows(one, "news@northwind.example.test"));
        assert!(!policy.allows(two, "news@northwind.example.test"));
        assert_eq!(
            policy.notice(one, "news@northwind.example.test", true),
            Notice::None
        );

        policy.set_sender("news@northwind.example.test", true);
        assert!(policy.allows(two, "news@northwind.example.test"));
        assert!(!policy.allows(two, "other@example.test"));
        assert_eq!(
            policy.notice(two, "news@northwind.example.test", true),
            Notice::SenderAllowed("news@northwind.example.test".into())
        );
        assert_eq!(
            policy.notice(one, "news@northwind.example.test", true),
            Notice::SenderAllowed("news@northwind.example.test".into()),
            "Hide is offered even where the message was allowed itself"
        );

        policy.set_sender("news@northwind.example.test", false);
        policy.forget_message(one);
        assert!(!policy.allows(one, "news@northwind.example.test"));
        assert!(!policy.allows(two, "news@northwind.example.test"));
    }

    #[test]
    fn a_message_without_a_sender_is_never_allowed_by_sender() {
        let policy = Policy::default();
        policy.set_sender("", true);
        assert!(!policy.sender_allowed(""));
        assert!(!policy.allows(MessageId(3), ""));
        assert_eq!(
            policy.notice(MessageId(3), "", true),
            Notice::Hidden(String::new())
        );
    }

    #[test]
    fn listeners_hear_every_change() {
        let policy = Policy::default();
        let heard = Rc::new(Cell::new(0));
        let counter = heard.clone();
        policy.connect_changed(move || counter.set(counter.get() + 1));
        policy.changed();
        policy.changed();
        assert_eq!(heard.get(), 2);
    }
}
