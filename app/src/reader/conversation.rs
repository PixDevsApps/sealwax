//! The conversation the reader shows (ML-057; ADR-016's M5 amendment; DESIGN.md "Reader"): one
//! subject heading, then every message oldest first. The selected message, every unread one and
//! the latest are expanded cards; the others are one-line rows (avatar, sender, snippet, time)
//! that expand when clicked. In a conversation over [`COLLAPSE_OVER`] messages the middle folds
//! into "N earlier messages" rows that reveal [`GAP_STEP`] messages a click, keeping the first
//! message, the latest [`KEEP_LATEST`] and every expanded one. This module holds the page's model
//! and its rows; the reader (`mod.rs`, `cards.rs`) loads and shows the cards.

use super::ShownBody;
use super::engine::InlineParts;
use super::format;
use super::page::CardWidgets;
use super::view::MessageView;
use super::widgets::BodyBox;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::model::{MessageId, MessageSummary, ThreadId};
use sealwax_domain::theme::{BodyStyle, avatar_class, initials};
use sealwax_infrastructure::store::ConversationMessage;
use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;

/// Conversations longer than this fold their middle into "N earlier messages" (ADR-016).
pub const COLLAPSE_OVER: usize = 100;
/// The latest messages a folded conversation keeps in sight (a proposal).
pub const KEEP_LATEST: usize = 10;
/// Messages one click on "N earlier messages" reveals, the newest of them first (a proposal); a
/// fold with at most half a step more is revealed whole.
pub const GAP_STEP: usize = 50;

/// Which messages expand when the conversation is shown: the selected one (the user picked
/// it), every unread one and the latest.
pub fn initially_expanded(messages: &[ConversationMessage], selected: MessageId) -> Vec<bool> {
    let last = messages.len().saturating_sub(1);
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            message.summary.id == selected || !message.summary.flags.is_seen() || index == last
        })
        .collect()
}

/// Which messages show as rows or cards rather than inside a fold: all of them up to
/// [`COLLAPSE_OVER`]; beyond it the first, the latest [`KEEP_LATEST`] and every expanded one.
pub fn initially_shown(expanded: &[bool]) -> Vec<bool> {
    let count = expanded.len();
    if count <= COLLAPSE_OVER {
        return vec![true; count];
    }
    expanded
        .iter()
        .enumerate()
        .map(|(index, expanded)| *expanded || index == 0 || index + KEEP_LATEST >= count)
        .collect()
}

/// The runs of messages that aren't shown, as ranges of positions.
pub fn folds(shown: &[bool]) -> Vec<Range<usize>> {
    let mut folds = Vec::new();
    let mut start = None;
    for (index, shown) in shown.iter().enumerate() {
        match (shown, start) {
            (false, None) => start = Some(index),
            (true, Some(from)) => {
                folds.push(from..index);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        folds.push(from..shown.len());
    }
    folds
}

/// What one click on a fold of `fold` reveals: its newest [`GAP_STEP`] messages, the ones next
/// to what follows it, or all of it when little more is left.
pub fn reveal(fold: Range<usize>) -> Range<usize> {
    if fold.len() <= GAP_STEP + GAP_STEP / 2 {
        fold
    } else {
        fold.end - GAP_STEP..fold.end
    }
}

/// "1 earlier message", "138 earlier messages".
pub fn fold_text(count: usize) -> String {
    if count == 1 {
        "1 earlier message".to_owned()
    } else {
        format!("{count} earlier messages")
    }
}

/// "Conversation, 12 messages", for screen readers.
pub fn size_text(count: usize) -> String {
    if count == 1 {
        "1 message".to_owned()
    } else {
        format!("{count} messages")
    }
}

/// An expanded HTML body: its frame on the page, the box that keeps its height, and the view it
/// holds while it is near the visible part of the column ([`super::live`]).
pub struct HtmlBody {
    pub frame: gtk::Box,
    pub holder: BodyBox,
    pub view: Option<MessageView>,
    pub style: BodyStyle,
    pub html: Rc<str>,
    pub parts: Rc<InlineParts>,
    pub remote_images: bool,
    pub remote_allowed: bool,
    /// Its quoted history collapses behind "…" (an earlier message is on the page).
    pub collapse_quotes: bool,
    /// The height last measured, and the width it was measured at.
    pub measured: Option<(i32, i32)>,
    /// The view's current load has fired its `load` event.
    pub loaded: bool,
    /// The view's current load has painted a frame.
    pub painted: bool,
    /// The view went back to the pool while the reader was hidden.
    pub released: bool,
    /// Tags the events of the view's current load.
    pub token: u64,
}

impl HtmlBody {
    /// Measured at `width` (within a pixel), as the reader shows it now.
    pub fn measured_at(&self, width: i32) -> bool {
        self.measured
            .is_some_and(|(_, measured)| width > 1 && (measured - width).abs() <= 1)
    }
}

/// An expanded message's card once its message is read.
pub struct Card {
    pub widgets: CardWidgets,
    pub body: ShownBody,
    pub html: Option<HtmlBody>,
    pub attachments: usize,
    /// The sender's address, lower case, for the senders whose images load (ML-058).
    pub sender: String,
    /// Its body has remote images (ML-058).
    pub remote_images: bool,
    /// The header's Reply, Reply all and Forward buttons, or Edit draft (M6).
    pub replies: Vec<(sealwax_domain::actions::ActionId, gtk::Button)>,
}

impl Card {
    /// What takes the focus when the reader moves it into this card: its body once it has one,
    /// else its header.
    pub fn focus_target(&self) -> Option<gtk::Widget> {
        match self.html.as_ref().and_then(|html| html.view.as_ref()) {
            Some(view) => Some(view.web_view().clone().upcast()),
            None => self
                .widgets
                .focus
                .clone()
                .or_else(|| Some(self.widgets.header.clone().upcast())),
        }
    }
}

/// One message of the page: its row, and its card while expanded.
pub struct Item {
    pub message: ConversationMessage,
    /// Holds the row or the card, in the page's list.
    pub slot: gtk::Box,
    pub row: gtk::Button,
    pub expanded: bool,
    pub card: Option<Card>,
    /// The newest load of its card; older answers are dropped.
    pub load: u64,
    /// Its body was reported shown ([`super::Opened`]).
    pub opened: bool,
}

impl Item {
    pub fn id(&self) -> MessageId {
        self.message.summary.id
    }
}

/// Messages folded into one "N earlier messages" row.
pub struct Fold {
    pub row: gtk::Button,
    pub messages: Vec<ConversationMessage>,
}

pub enum Entry {
    Message(Box<Item>),
    Fold(Fold),
}

/// The conversation on screen, or being laid out unseen.
pub struct Page {
    /// The `show` it came from: card loads and view events of another page are dropped.
    pub key: u64,
    pub selected: MessageId,
    pub thread: Option<ThreadId>,
    pub root: gtk::Box,
    pub list: gtk::Box,
    pub entries: Vec<Entry>,
    /// Every row of the conversation (copies and rows left out too): a change event that names
    /// one of them changes what the page shows.
    pub members: HashSet<MessageId>,
    /// The page shows that the message is gone.
    pub missing: bool,
    /// The message Next and Previous Message in Conversation last moved to; the selected one
    /// before they are used.
    pub current: Option<MessageId>,
    /// The quick reply under the conversation (M6, ML-068), once its messages are read.
    pub quick: Option<Rc<super::reply::QuickReply>>,
}

impl Page {
    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.entries.iter().filter_map(|entry| match entry {
            Entry::Message(item) => Some(&**item),
            Entry::Fold(_) => None,
        })
    }

    pub fn items_mut(&mut self) -> impl Iterator<Item = &mut Item> {
        self.entries.iter_mut().filter_map(|entry| match entry {
            Entry::Message(item) => Some(&mut **item),
            Entry::Fold(_) => None,
        })
    }

    pub fn item(&self, id: MessageId) -> Option<&Item> {
        self.items().find(|item| item.id() == id)
    }

    pub fn item_mut(&mut self, id: MessageId) -> Option<&mut Item> {
        self.items_mut().find(|item| item.id() == id)
    }

    /// The selected message's card, once loaded.
    pub fn selected_card(&self) -> Option<&Card> {
        self.item(self.selected).and_then(|item| item.card.as_ref())
    }

    /// The messages of the conversation, folded ones included.
    pub fn message_count(&self) -> usize {
        self.entries
            .iter()
            .map(|entry| match entry {
                Entry::Message(_) => 1,
                Entry::Fold(fold) => fold.messages.len(),
            })
            .sum()
    }

    /// Whether an earlier message of the conversation (a row, a card or a fold) comes before
    /// message `id`: its quoted history then repeats what the page shows, so it collapses.
    pub fn has_earlier(&self, id: MessageId) -> bool {
        self.entries
            .iter()
            .position(|entry| matches!(entry, Entry::Message(item) if item.id() == id))
            .is_some_and(|index| index > 0)
    }

    /// Expanded items whose card is in the page.
    pub fn expanded(&self) -> Vec<MessageId> {
        self.items()
            .filter(|item| item.expanded)
            .map(Item::id)
            .collect()
    }

    /// Every HTML body that holds a view.
    pub fn live_bodies(&self) -> Vec<MessageId> {
        self.items()
            .filter(|item| {
                item.card
                    .as_ref()
                    .and_then(|card| card.html.as_ref())
                    .is_some_and(|html| html.view.is_some())
            })
            .map(Item::id)
            .collect()
    }
}

/// The page's top: the subject as the conversation's heading (the selected message's subject).
pub fn heading(subject: &str) -> gtk::Label {
    let subject = subject.trim();
    let heading = gtk::Label::builder()
        .label(if subject.is_empty() {
            "(No subject)"
        } else {
            subject
        })
        .xalign(0.0)
        .selectable(true)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .css_classes(["sw-subject-heading"])
        .build();
    heading.set_widget_name("reader-subject");
    heading
}

fn sender_name(summary: &MessageSummary) -> &str {
    if summary.sender_name.trim().is_empty() {
        &summary.sender_email
    } else {
        &summary.sender_name
    }
}

/// A collapsed message: one line with the sender's initials, name, the snippet and the time, a
/// button that expands it (click, Enter or Space).
pub fn collapsed_row(summary: &MessageSummary, now: i64) -> gtk::Button {
    let row = gtk::Button::builder()
        .css_classes(["flat", "sw-collapsed-message"])
        .build();
    row.set_widget_name("reader-row");
    row.update_state(&[gtk::accessible::State::Expanded(Some(false))]);
    fill_row(&row, summary, now);
    row
}

/// Puts `summary`'s line in `row`, again when its flags change.
pub fn fill_row(row: &gtk::Button, summary: &MessageSummary, now: i64) {
    let zone = glib::TimeZone::local();
    let name = sender_name(summary);
    let unread = !summary.flags.is_seen();
    let avatar = gtk::Label::builder()
        .label(initials(&summary.sender_name, &summary.sender_email))
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .css_classes([
            "sw-avatar".to_owned(),
            "sw-avatar-small".to_owned(),
            avatar_class(&summary.sender_email),
        ])
        .build();
    let sender = gtk::Label::builder()
        .label(name)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(24)
        .css_classes(if unread {
            vec!["sw-strong"]
        } else {
            Vec::new()
        })
        .build();
    sender.set_widget_name("reader-row-sender");
    let snippet = gtk::Label::builder()
        .label(summary.snippet.lines().next().unwrap_or_default())
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .width_chars(1)
        .css_classes(["sw-secondary"])
        .build();
    let time = format::short_date(summary.date, now, &zone);
    let when = gtk::Label::builder()
        .label(&time)
        .css_classes(["sw-small", "sw-secondary", "numeric"])
        .tooltip_text(format::full_date(summary.date, &zone))
        .build();
    let line = gtk::Box::builder().spacing(10).build();
    if unread {
        let dot = gtk::Box::builder()
            .valign(gtk::Align::Center)
            .css_classes(["sw-unread-dot"])
            .build();
        line.append(&dot);
    }
    line.append(&avatar);
    line.append(&sender);
    line.append(&snippet);
    line.append(&when);
    row.set_child(Some(&line));
    row.update_property(&[
        gtk::accessible::Property::Label(&row_label(name, &time, unread)),
        gtk::accessible::Property::Description(summary.snippet.lines().next().unwrap_or_default()),
    ]);
}

/// What a screen reader says for a collapsed row: the sender, the time, unread, collapsed.
pub fn row_label(sender: &str, time: &str, unread: bool) -> String {
    let unread = if unread { ", unread" } else { "" };
    format!("{sender}, {time}{unread}, collapsed")
}

/// The row that stands for folded messages.
pub fn fold_row(count: usize) -> gtk::Button {
    let label = gtk::Label::builder()
        .label(fold_text(count))
        .css_classes(["sw-small", "sw-reader-fold-count"])
        .build();
    let rule = || {
        gtk::Separator::builder()
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build()
    };
    let line = gtk::Box::builder().spacing(10).build();
    line.append(&rule());
    line.append(&label);
    line.append(&rule());
    let row = gtk::Button::builder()
        .child(&line)
        .css_classes(["flat", "sw-reader-fold"])
        .build();
    row.set_widget_name("reader-fold");
    row.update_property(&[gtk::accessible::Property::Label(&format!(
        "{}, folded; show them",
        fold_text(count)
    ))]);
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::model::{Flags, FolderId, FolderRole};

    fn message(id: i64, read: bool) -> ConversationMessage {
        ConversationMessage {
            summary: MessageSummary {
                id: MessageId(id),
                folder: FolderId(1),
                thread: ThreadId(1),
                sender_name: "Kari Nordmann".into(),
                sender_email: "kari@example.test".into(),
                subject: "Hyttetur".into(),
                snippet: "Hei".into(),
                date: id,
                flags: if read { Flags::SEEN } else { Flags::NONE },
                has_attachments: false,
                thread_count: 1,
            },
            role: FolderRole::Inbox,
            copies: Vec::new(),
        }
    }

    #[test]
    fn the_selected_the_unread_and_the_latest_expand() {
        let messages: Vec<ConversationMessage> =
            (1..=6).map(|id| message(id, id != 3 && id != 4)).collect();
        assert_eq!(
            initially_expanded(&messages, MessageId(1)),
            [true, false, true, true, false, true]
        );
        let all_read: Vec<ConversationMessage> = (1..=3).map(|id| message(id, true)).collect();
        assert_eq!(
            initially_expanded(&all_read, MessageId(3)),
            [false, false, true]
        );
        assert_eq!(initially_expanded(&all_read[..1], MessageId(1)), [true]);
    }

    #[test]
    fn long_conversations_fold_their_middle_and_reveal_it_in_steps() {
        assert_eq!(initially_shown(&[false; 100]), vec![true; 100]);
        let mut expanded = vec![false; 150];
        expanded[149] = true;
        expanded[70] = true;
        let shown = initially_shown(&expanded);
        assert_eq!(
            shown.iter().filter(|shown| **shown).count(),
            1 + 1 + KEEP_LATEST
        );
        assert_eq!(folds(&shown), [1..70, 71..140]);
        assert_eq!(reveal(71..140), 71..140);
        assert_eq!(reveal(1..139), 89..139);
        assert_eq!(reveal(1..89), 39..89);
        assert_eq!(reveal(1..39), 1..39);
        assert_eq!(folds(&[false, true, false]), [0..1, 2..3]);
        assert_eq!(fold_text(138), "138 earlier messages");
        assert_eq!(fold_text(1), "1 earlier message");
    }

    #[test]
    fn rows_say_who_when_and_that_they_are_collapsed() {
        assert_eq!(
            row_label("Kari Nordmann", "09:30", true),
            "Kari Nordmann, 09:30, unread, collapsed"
        );
        assert_eq!(row_label("Ola", "Mon", false), "Ola, Mon, collapsed");
        assert_eq!(size_text(12), "12 messages");
    }
}
