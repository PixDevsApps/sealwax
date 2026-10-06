//! Attachment cards (ML-061; DESIGN.md "Reader", the approved mock; SECURITY.md "Attachments"):
//! "N attachments" with Download all, then a card per attachment with its type, its name (the
//! name it is saved under: no path, no invisible or direction characters) and its size, with
//! Open and Save. Open copies it into a private folder and hands it to the desktop's default
//! application; Save and Download all go through the file chooser. An attachment whose name,
//! type or content says it is a program, a launcher, a script or an installer, or whose name
//! hides its real type, is never opened: its card shows a warning icon and why, and offers
//! Save only. A message stored as its text parts (ML-044) downloads each attachment by its
//! section first: its sizes are estimates ("about 4.2 MB"), and its card shows "Downloading…"
//! with Cancel, or why it failed with Retry. Nothing here runs on the GTK thread but the
//! widgets: bytes are read, decoded and written on worker threads.

use super::files::{self, Failure};
use super::{Inner, Reader, format};
use crate::window::panes::icon;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use sealwax_domain::attachment::{self as rules, Danger};
use sealwax_domain::model::{Attachment, MessageId};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// A card's wrapped lines (the warning, a failure) ask for no more than this many characters,
/// so the cards keep three to a row on a wide reader.
const MAX_LINE_CHARS: i32 = 30;

/// What a card offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Open,
    Save,
}

/// One attachment's card and what it is doing.
struct Card {
    reader: Weak<Inner>,
    message: MessageId,
    attachment: Attachment,
    name: String,
    root: gtk::Box,
    badge: gtk::Box,
    open: gtk::Button,
    save: gtk::Button,
    warning: gtk::Label,
    status: gtk::Box,
    status_text: gtk::Label,
    cancel: gtk::Button,
    retry: gtk::Button,
    danger: Cell<Option<Danger>>,
    /// Each action takes the next number; Cancel takes one too, so a late answer is dropped.
    generation: Cell<u64>,
    last: Cell<Option<Action>>,
}

/// What the self-test sees of a card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardState {
    pub name: String,
    pub size: String,
    pub danger: bool,
    pub open: bool,
    pub busy: bool,
    pub failure: Option<String>,
    /// Why it isn't opened, for a dangerous attachment.
    pub warning: Option<String>,
}

/// The section of `message`'s attachment cards, under "N attachments" with Download all.
pub fn attachment_cards(
    reader: &Reader,
    message: MessageId,
    attachments: &[Attachment],
) -> gtk::Box {
    let section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .css_classes(["sw-reader-attachments"])
        .build();
    section.set_widget_name("reader-attachments");
    let heading = gtk::Box::builder().spacing(10).build();
    let count = gtk::Label::builder()
        .label(format::attachment_count(attachments.len()))
        .xalign(0.0)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .css_classes(["sw-secondary"])
        .build();
    heading.append(&count);
    let download_all = gtk::Button::builder()
        .child(&chip_content())
        .valign(gtk::Align::Center)
        .tooltip_text("Save every attachment into a folder")
        .css_classes(["sw-chip", "sw-download-all"])
        .build();
    download_all.set_widget_name("reader-download-all");
    download_all.update_property(&[gtk::accessible::Property::Label("Download all")]);
    heading.append(&download_all);
    section.append(&heading);
    let flow = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .min_children_per_line(1)
        .max_children_per_line(3)
        .column_spacing(10)
        .row_spacing(10)
        .build();
    let cards: Vec<Rc<Card>> = attachments
        .iter()
        .map(|attachment| Card::new(reader, message, attachment))
        .collect();
    for card in &cards {
        let child = gtk::FlowBoxChild::builder()
            .child(&card.root)
            .focusable(false)
            .build();
        flow.append(&child);
    }
    section.append(&flow);
    let weak: Vec<Weak<Card>> = cards.iter().map(Rc::downgrade).collect();
    download_all.connect_clicked(move |button| {
        let cards: Vec<Rc<Card>> = weak.iter().filter_map(Weak::upgrade).collect();
        glib::spawn_future_local(download_all_cards(button.clone(), cards));
    });
    // The cards live as long as their widgets.
    let kept = RefCell::new(cards);
    let reader = Rc::downgrade(&reader.inner);
    section.connect_destroy(move |_| {
        kept.borrow_mut().clear();
    });
    glib::spawn_future_local(estimate_sizes(reader, message, section.downgrade()));
    section
}

/// The chip's icon and words.
fn chip_content() -> gtk::Box {
    let content = gtk::Box::builder().spacing(6).build();
    content.append(&icon(&[
        "folder-download-symbolic",
        "document-save-symbolic",
    ]));
    content.append(&gtk::Label::new(Some("Download all")));
    content
}

/// A message stored as its text parts: every size becomes an estimate.
async fn estimate_sizes(reader: Weak<Inner>, message: MessageId, section: glib::WeakRef<gtk::Box>) {
    let Some(inner) = reader.upgrade() else {
        return;
    };
    if !files::stored_as_text(&inner, message).await {
        return;
    }
    drop(inner);
    let Some(section) = section.upgrade() else {
        return;
    };
    for card in cards_in(&section) {
        if let Some(size) = find(&card, "reader-attachment-size").and_downcast::<gtk::Label>() {
            size.set_text(&format!("about {}", size.text()));
        }
    }
}

impl Card {
    fn new(reader: &Reader, message: MessageId, attachment: &Attachment) -> Rc<Card> {
        let name = rules::safe_file_name(attachment.name.as_deref(), &attachment.mime_type);
        let danger = rules::danger(attachment.name.as_deref(), &attachment.mime_type);
        let root = gtk::Box::builder()
            .spacing(10)
            .css_classes(["sw-attachment-card"])
            .build();
        root.set_widget_name("reader-attachment");
        let badge = gtk::Box::builder()
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Start)
            .hexpand(false)
            .css_classes(["sw-attachment-type"])
            .build();
        root.append(&badge);
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .spacing(2)
            .build();
        let title = gtk::Label::builder()
            .label(&name)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .tooltip_text(&name)
            .css_classes(["sw-attachment-name"])
            .build();
        title.set_widget_name("reader-attachment-name");
        text.append(&title);
        let meta = gtk::Box::builder()
            .spacing(4)
            .css_classes(["sw-attachment-meta"])
            .build();
        let size = gtk::Label::builder()
            .label(format::size(attachment.size))
            .xalign(0.0)
            .css_classes(["numeric"])
            .build();
        size.set_widget_name("reader-attachment-size");
        meta.append(&size);
        let open = action_button("Open", "reader-attachment-open");
        let save = action_button("Save", "reader-attachment-save");
        let dot = || gtk::Label::builder().label("·").build();
        meta.append(&dot());
        meta.append(&open);
        meta.append(&dot());
        meta.append(&save);
        text.append(&meta);
        let warning = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(MAX_LINE_CHARS)
            .visible(false)
            .css_classes(["sw-attachment-warning", "sw-small"])
            .build();
        warning.set_widget_name("reader-attachment-warning");
        text.append(&warning);
        let status = gtk::Box::builder().spacing(6).visible(false).build();
        let status_text = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(MAX_LINE_CHARS)
            .hexpand(true)
            .css_classes(["sw-secondary", "sw-small"])
            .build();
        status_text.set_widget_name("reader-attachment-status");
        let cancel = action_button("Cancel", "reader-attachment-cancel");
        let retry = action_button("Retry", "reader-attachment-retry");
        status.append(&status_text);
        status.append(&cancel);
        status.append(&retry);
        text.append(&status);
        root.append(&text);
        let card = Rc::new(Card {
            reader: Rc::downgrade(&reader.inner),
            message,
            attachment: attachment.clone(),
            name,
            root,
            badge,
            open,
            save,
            warning,
            status,
            status_text,
            cancel,
            retry,
            danger: Cell::new(None),
            generation: Cell::new(0),
            last: Cell::new(None),
        });
        card.set_danger(danger);
        card.connect();
        card
    }

    fn connect(self: &Rc<Self>) {
        for (button, action) in [(&self.open, Action::Open), (&self.save, Action::Save)] {
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(card) = weak.upgrade() {
                    glib::spawn_future_local(card.run(action));
                }
            });
        }
        let weak = Rc::downgrade(self);
        self.cancel.connect_clicked(move |_| {
            if let Some(card) = weak.upgrade() {
                card.next_generation();
                card.idle();
            }
        });
        let weak = Rc::downgrade(self);
        self.retry.connect_clicked(move |_| {
            if let Some(card) = weak.upgrade()
                && let Some(action) = card.last.get()
            {
                glib::spawn_future_local(card.run(action));
            }
        });
    }

    /// The badge, Open and the warning follow whether the attachment is dangerous.
    fn set_danger(&self, danger: Option<Danger>) {
        self.danger.set(danger);
        while let Some(child) = self.badge.first_child() {
            self.badge.remove(&child);
        }
        let kind = format::type_label(self.attachment.name.as_deref(), &self.attachment.mime_type);
        let size = format::size(self.attachment.size);
        match danger {
            Some(danger) => {
                let image = icon(&["dialog-warning-symbolic"]);
                image.set_valign(gtk::Align::Center);
                image.set_hexpand(true);
                self.badge.append(&image);
                self.badge.add_css_class("sw-attachment-danger");
                self.root.add_css_class("sw-attachment-dangerous");
                self.warning.set_text(warning(danger));
                self.warning.set_visible(true);
                self.open.set_visible(false);
                if let Some(dot) = self.open.prev_sibling() {
                    dot.set_visible(false);
                }
                self.root
                    .update_property(&[gtk::accessible::Property::Label(&format!(
                        "{}, {kind}, {size}, can't be opened: {}",
                        self.name,
                        warning(danger)
                    ))]);
            }
            None => {
                let label = gtk::Label::builder()
                    .label(&kind)
                    .valign(gtk::Align::Center)
                    .hexpand(true)
                    .build();
                self.badge.append(&label);
                self.root
                    .update_property(&[gtk::accessible::Property::Label(&format!(
                        "{}, {kind}, {size}",
                        self.name
                    ))]);
            }
        }
    }

    fn next_generation(&self) -> u64 {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        generation
    }

    fn idle(&self) {
        self.status.set_visible(false);
        self.open.set_sensitive(true);
        self.save.set_sensitive(true);
    }

    fn busy(&self) {
        self.status_text.set_text("Downloading…");
        self.cancel.set_visible(true);
        self.retry.set_visible(false);
        self.status.set_visible(true);
        self.open.set_sensitive(false);
        self.save.set_sensitive(false);
    }

    fn failed(&self, text: &str) {
        self.status_text.set_text(text);
        self.cancel.set_visible(false);
        self.retry.set_visible(true);
        self.status.set_visible(true);
        self.open.set_sensitive(true);
        self.save.set_sensitive(true);
    }

    /// Open or Save, from the click to the file.
    async fn run(self: Rc<Self>, action: Action) {
        let Some(inner) = self.reader.upgrade() else {
            return;
        };
        if action == Action::Open && self.danger.get().is_some() {
            return;
        }
        self.last.set(Some(action));
        let generation = self.next_generation();
        self.idle();
        let target = match action {
            Action::Save => match files::choose_save(&inner, "Save Attachment", &self.name).await {
                Some(file) => Some(file),
                None => return,
            },
            Action::Open => None,
        };
        if self.generation.get() != generation {
            return;
        }
        let bytes = files::attachment(&inner, self.message, &self.attachment, || self.busy()).await;
        if self.generation.get() != generation {
            return;
        }
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err(failure) => {
                tracing::info!(
                    id = self.message.0,
                    ?failure,
                    "reader: an attachment wasn't there"
                );
                self.failed(&download_failure(failure));
                return;
            }
        };
        self.idle();
        match target {
            Some(file) => self.save_to(&inner, &file, bytes).await,
            // An attachment that arrived after the user moved to another message isn't opened.
            None if self.root.root().is_none() => {}
            None => self.open_from(&inner, bytes).await,
        }
    }

    async fn save_to(&self, inner: &Inner, file: &gio::File, bytes: Vec<u8>) {
        match files::write(file, bytes).await {
            Ok(()) => {
                let name = file
                    .basename()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| self.name.clone());
                files::toast(inner, &format!("Saved {name}"));
            }
            Err(error) => {
                tracing::warn!("reader: an attachment couldn't be saved");
                self.failed(&format!("Couldn't save it: {}", error.message()));
            }
        }
    }

    /// Copies the attachment into a private folder and opens it, unless its content says it
    /// is a program after all.
    async fn open_from(&self, inner: &Inner, bytes: Vec<u8>) {
        let (sniffed, bytes) = gio::spawn_blocking(move || {
            let head = &bytes[..bytes.len().min(4096)];
            let (kind, _) = gio::content_type_guess(None::<&str>, head);
            (gio::content_type_get_mime_type(&kind), bytes)
        })
        .await
        .unwrap_or((None, Vec::new()));
        if sniffed.as_deref().is_some_and(rules::dangerous_type) {
            tracing::info!(
                id = self.message.0,
                "reader: an attachment's content is a program"
            );
            self.set_danger(Some(Danger::Program));
            return;
        }
        match files::write_private(inner, self.name.clone(), bytes).await {
            Ok(path) => files::launch(inner, &path),
            Err(error) => {
                tracing::warn!(kind = ?error.kind(), "reader: an attachment couldn't be copied to open it");
                self.failed("Couldn't open it: it couldn't be copied on this computer.");
            }
        }
    }
}

/// Saves every card's attachment into a folder the user picks, each under a name the folder
/// doesn't hold yet; the chip waits meanwhile.
async fn download_all_cards(button: gtk::Button, cards: Vec<Rc<Card>>) {
    let Some(inner) = cards.first().and_then(|card| card.reader.upgrade()) else {
        return;
    };
    let Some(folder) = files::choose_folder(&inner, "Download All Attachments").await else {
        return;
    };
    button.set_sensitive(false);
    let mut ready = Vec::new();
    for card in &cards {
        let generation = card.next_generation();
        card.last.set(Some(Action::Save));
        card.idle();
        match files::attachment(&inner, card.message, &card.attachment, || card.busy()).await {
            Ok(bytes) if card.generation.get() == generation => {
                card.idle();
                ready.push((card.name.clone(), bytes));
            }
            Ok(_) => {}
            Err(failure) => card.failed(&download_failure(failure)),
        }
    }
    let total = cards.len();
    let (saved, error) = files::save_all(folder.clone(), ready).await;
    button.set_sensitive(true);
    if error.is_some() {
        tracing::warn!(saved, total, "reader: some attachments couldn't be saved");
    }
    let place = folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| folder.to_string_lossy().into_owned());
    files::toast(&inner, &saved_text(saved, total, &place));
}

/// "Saved 3 attachments to Downloads", or how many of them.
fn saved_text(saved: usize, total: usize, place: &str) -> String {
    match (saved, total) {
        (0, _) => "No attachment was saved".to_owned(),
        (1, 1) => format!("Saved 1 attachment to {place}"),
        (saved, total) if saved == total => format!("Saved {saved} attachments to {place}"),
        (saved, total) => format!("Saved {saved} of {total} attachments to {place}"),
    }
}

fn action_button(label: &str, name: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .label(label)
        .valign(gtk::Align::Center)
        .css_classes(["flat", "sw-attachment-action"])
        .build();
    button.set_widget_name(name);
    button
}

/// Why a dangerous attachment isn't opened (the card's warning).
fn warning(danger: Danger) -> &'static str {
    match danger {
        Danger::Program => {
            "Sealwax won't open this file: it can run programs on your computer. Save it only \
             if you trust the sender."
        }
        Danger::Disguised => {
            "Sealwax won't open this file: its name hides what kind of file it is. Save it only \
             if you trust the sender."
        }
    }
}

fn download_failure(failure: Failure) -> String {
    let reason = failure.reason();
    let mut chars = reason.chars();
    let reason = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
        None => String::new(),
    };
    match failure {
        Failure::Download(_) => format!("Couldn't download it. {reason}."),
        Failure::NotDownloaded | Failure::Unreadable => format!("Couldn't get it. {reason}."),
    }
}

/// The cards under `section`, in order.
fn cards_in(widget: &impl IsA<gtk::Widget>) -> Vec<gtk::Widget> {
    let mut out = Vec::new();
    let mut stack = vec![widget.as_ref().clone()];
    while let Some(widget) = stack.pop() {
        if widget.widget_name() == "reader-attachment" {
            out.push(widget);
            continue;
        }
        let mut children = Vec::new();
        let mut child = widget.first_child();
        while let Some(next) = child {
            child = next.next_sibling();
            children.push(next);
        }
        stack.extend(children.into_iter().rev());
    }
    out
}

/// The first widget named `name` under `widget`.
fn find(widget: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if widget.widget_name() == name {
        return Some(widget.clone());
    }
    let mut child = widget.first_child();
    while let Some(next) = child {
        if let Some(found) = find(&next, name) {
            return Some(found);
        }
        child = next.next_sibling();
    }
    None
}

impl Reader {
    /// The card of message `id` in the shown conversation, while it is expanded, for checks and
    /// real clicks.
    pub fn message_card(&self, id: MessageId) -> Option<gtk::Widget> {
        self.inner
            .shown
            .borrow()
            .as_ref()?
            .item(id)?
            .card
            .as_ref()
            .map(|card| card.widgets.root.clone().upcast())
    }

    /// The attachment cards of the selected message's card, in order, for checks.
    pub fn attachment_cards(&self) -> Vec<gtk::Widget> {
        self.message()
            .and_then(|id| self.message_card(id))
            .map(|card| cards_in(&card))
            .unwrap_or_default()
    }

    /// The button named `name` (`reader-attachment-open`, `-save`, `-cancel`, `-retry`) of the
    /// selected message's attachment card `index`, for checks and real clicks.
    pub fn attachment_button(&self, index: usize, name: &str) -> Option<gtk::Widget> {
        self.attachment_cards()
            .get(index)
            .and_then(|card| find(card, name))
    }

    /// What attachment card `index` of the selected message shows, for checks.
    pub fn attachment_card(&self, index: usize) -> Option<CardState> {
        let card = self.attachment_cards().into_iter().nth(index)?;
        let label = |name: &str| {
            find(&card, name)
                .and_downcast::<gtk::Label>()
                .map(|label| label.text().to_string())
                .unwrap_or_default()
        };
        let visible = |name: &str| find(&card, name).is_some_and(|widget| widget.is_visible());
        let status = find(&card, "reader-attachment-status")
            .and_then(|status| status.parent())
            .is_some_and(|status| status.is_visible());
        Some(CardState {
            name: label("reader-attachment-name"),
            size: label("reader-attachment-size"),
            danger: card.has_css_class("sw-attachment-dangerous"),
            open: visible("reader-attachment-open"),
            busy: status && visible("reader-attachment-cancel"),
            failure: (status && visible("reader-attachment-retry"))
                .then(|| label("reader-attachment-status")),
            warning: visible("reader-attachment-warning")
                .then(|| label("reader-attachment-warning")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_engine::api::BodyError;

    #[test]
    fn failures_read_as_sentences() {
        assert_eq!(
            download_failure(Failure::Download(BodyError::Offline)),
            "Couldn't download it. The mail server can't be reached."
        );
        assert_eq!(
            download_failure(Failure::NotDownloaded),
            "Couldn't get it. The message hasn't been downloaded yet."
        );
    }

    #[test]
    fn the_danger_sample_is_dangerous_as_parsed() {
        let raw = crate::reader::samples::files_draft("files-danger", 1_791_115_200)
            .unwrap()
            .render();
        let parsed = sealwax_domain::message::parse(&raw);
        let found: Vec<(String, Option<Danger>)> = parsed
            .attachments
            .iter()
            .map(|attachment| {
                (
                    attachment
                        .name
                        .clone()
                        .unwrap_or_default()
                        .escape_unicode()
                        .to_string(),
                    rules::danger(attachment.name.as_deref(), &attachment.mime_type),
                )
            })
            .collect();
        assert_eq!(found.len(), 4);
        assert_eq!(found[0].1, None);
        assert_eq!(found[1].1, Some(Danger::Program));
        // Parsing drops the direction override, so the name shows its real extension.
        assert_eq!(found[2].1, Some(Danger::Program), "{:?}", found[2].0);
        assert!(!found[2].0.contains("202e"), "{:?}", found[2].0);
        assert_eq!(found[3].1, Some(Danger::Disguised), "{:?}", found[3].0);
    }

    #[test]
    fn download_all_says_how_many_were_saved() {
        assert_eq!(
            saved_text(1, 1, "Downloads"),
            "Saved 1 attachment to Downloads"
        );
        assert_eq!(
            saved_text(3, 3, "Downloads"),
            "Saved 3 attachments to Downloads"
        );
        assert_eq!(
            saved_text(2, 3, "Downloads"),
            "Saved 2 of 3 attachments to Downloads"
        );
        assert_eq!(saved_text(0, 3, "Downloads"), "No attachment was saved");
    }

    #[test]
    fn warnings_say_why_and_what_to_do() {
        assert!(warning(Danger::Program).contains("can run programs"));
        assert!(warning(Danger::Disguised).contains("hides what kind of file"));
        for danger in [Danger::Program, Danger::Disguised] {
            assert!(warning(danger).contains("Save it only if you trust the sender"));
        }
    }
}
