//! Move to… (ML-052): a keyboard-first picker over the folders the selected messages can move
//! to, their account's folders that take messages except the one they are in, role folders
//! first by their role's name. A subfolder shows by its last level first, the folders above it
//! beside it ("Acme", "Kunder / 2026"; M9). Typing filters with Stet's matcher, Up and Down
//! move, Enter moves the messages, Escape closes; a click picks a folder. It is an `AdwDialog`
//! like the command palette, so its entry keeps every letter: the list's one-key shortcuts don't
//! reach it.

use crate::triage::folder_title;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::folders;
use sealwax_domain::model::{Folder, FolderId, FolderRole};
use sealwax_domain::palette;
use std::cell::RefCell;
use std::rc::Rc;

/// One folder the picker offers.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Choice {
    id: FolderId,
    title: String,
    /// The server's own name of a role folder, when it differs from the role's ("Sendt"); the
    /// folders above a subfolder ("Kunder / 2026").
    detail: String,
}

impl Choice {
    /// What a screen reader says for the row.
    fn spoken(&self) -> String {
        if self.detail.is_empty() {
            format!("Move to {}", self.title)
        } else {
            format!("Move to {}, {}", self.title, self.detail)
        }
    }

    /// `folder`'s choice; `known` are its account's folders, which name the folders above it.
    fn new(folder: &Folder, known: &[Folder]) -> Self {
        if matches!(folder.role, FolderRole::Other | FolderRole::All) {
            let mut above = Vec::new();
            let mut name = folder.name.as_str();
            while let Some(parent) = folders::parent(name, folder.delimiter) {
                above.push(
                    known
                        .iter()
                        .find(|candidate| {
                            candidate.account == folder.account && candidate.name == parent
                        })
                        .filter(|parent| !matches!(parent.role, FolderRole::Other))
                        .map_or_else(
                            || folders::leaf(parent, folder.delimiter).to_owned(),
                            folder_title,
                        ),
                );
                name = parent;
            }
            above.reverse();
            return Self {
                id: folder.id,
                title: folders::leaf(&folder.name, folder.delimiter).to_owned(),
                detail: above.join(" / "),
            };
        }
        let title = folder_title(folder);
        let detail = if folder.name == title {
            String::new()
        } else {
            folder.name.clone()
        };
        Self {
            id: folder.id,
            title,
            detail,
        }
    }

    fn search_text(&self) -> String {
        format!("{} {}", self.title, self.detail)
    }
}

type Pick = Rc<dyn Fn(FolderId)>;

struct Inner {
    dialog: glib::WeakRef<adw::Dialog>,
    entry: glib::WeakRef<gtk::SearchEntry>,
    list: glib::WeakRef<gtk::ListBox>,
    scrolled: glib::WeakRef<gtk::ScrolledWindow>,
    choices: Vec<Choice>,
    shown: RefCell<Vec<Choice>>,
    pick: Pick,
}

thread_local! {
    /// The picker open now, for the self-test.
    static OPEN: RefCell<Option<MoveTo>> = const { RefCell::new(None) };
}

/// An open picker; [`present`] makes one.
#[derive(Clone)]
pub struct MoveTo {
    dialog: adw::Dialog,
    inner: Rc<Inner>,
}

/// Opens the picker over `window` for `count` messages, offering `folders` in their order;
/// `known` are every folder, which name the folders above a subfolder; `pick` gets the folder
/// chosen.
pub fn present(
    window: &impl IsA<gtk::Widget>,
    folders: &[Folder],
    known: &[Folder],
    count: u32,
    pick: impl Fn(FolderId) + 'static,
) -> MoveTo {
    let picker = MoveTo::new(folders, known, count, Rc::new(pick));
    picker.dialog.present(Some(window));
    if let Some(entry) = picker.inner.entry.upgrade() {
        entry.grab_focus();
    }
    OPEN.with(|open| open.replace(Some(picker.clone())));
    picker
}

/// The picker open now, if any.
pub fn current() -> Option<MoveTo> {
    OPEN.with(|open| open.borrow().clone())
        .filter(|picker| picker.dialog.is_mapped())
}

impl MoveTo {
    fn new(folders: &[Folder], known: &[Folder], count: u32, pick: Pick) -> Self {
        let title = match count {
            1 => "Move 1 message to…".to_owned(),
            count => format!("Move {count} messages to…"),
        };
        let entry = gtk::SearchEntry::builder()
            .placeholder_text(title.as_str())
            .search_delay(0)
            .hexpand(true)
            .css_classes(["sw-palette-search", "sw-move-to-search"])
            .build();
        entry.set_widget_name("move-to-search");
        entry.update_property(&[gtk::accessible::Property::Label(&title)]);
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Browse)
            .css_classes(["sw-palette-list", "sw-move-to-list"])
            .build();
        list.set_widget_name("move-to-list");
        list.update_property(&[gtk::accessible::Property::Label("Folders")]);
        let empty = gtk::Label::builder()
            .label("No matching folder")
            .css_classes(["sw-palette-empty", "sw-secondary"])
            .build();
        list.set_placeholder(Some(&empty));
        // The focus stays in the entry; the list is no Tab stop of its own.
        let scrolled = gtk::ScrolledWindow::builder()
            .child(&list)
            .focusable(false)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let header = adw::HeaderBar::builder().title_widget(&entry).build();
        let content = adw::ToolbarView::new();
        content.add_top_bar(&header);
        content.set_content(Some(&scrolled));
        let dialog = adw::Dialog::builder()
            .title(title.as_str())
            .content_width(420)
            .content_height(420)
            .child(&content)
            .focus_widget(&entry)
            .css_classes(["sw-palette", "sw-move-to"])
            .build();
        dialog.set_widget_name("move-to");
        let inner = Rc::new(Inner {
            dialog: dialog.downgrade(),
            entry: entry.downgrade(),
            list: list.downgrade(),
            scrolled: scrolled.downgrade(),
            choices: folders
                .iter()
                .map(|folder| Choice::new(folder, known))
                .collect(),
            shown: RefCell::new(Vec::new()),
            pick,
        });
        connect(&inner, &entry, &list);
        inner.refresh();
        Self { dialog, inner }
    }

    pub fn dialog(&self) -> &adw::Dialog {
        &self.dialog
    }

    pub fn search_entry(&self) -> Option<gtk::SearchEntry> {
        self.inner.entry.upgrade()
    }

    /// The folders shown, best match first, by their titles.
    pub fn shown(&self) -> Vec<String> {
        self.inner
            .shown
            .borrow()
            .iter()
            .map(|choice| choice.title.clone())
            .collect()
    }

    /// The folder Enter would pick, by its title.
    pub fn selected(&self) -> Option<String> {
        self.inner.selected().map(|choice| choice.title)
    }
}

fn connect(inner: &Rc<Inner>, entry: &gtk::SearchEntry, list: &gtk::ListBox) {
    entry.connect_search_changed(glib::clone!(
        #[strong]
        inner,
        move |_| inner.refresh()
    ));
    entry.connect_activate(glib::clone!(
        #[strong]
        inner,
        move |_| {
            if let Some(choice) = inner.selected() {
                inner.run(choice.id);
            }
        }
    ));
    entry.connect_stop_search(glib::clone!(
        #[strong]
        inner,
        move |_| {
            if let Some(dialog) = inner.dialog.upgrade() {
                dialog.close();
            }
        }
    ));
    list.connect_row_activated(glib::clone!(
        #[strong]
        inner,
        move |_, row| {
            let id = usize::try_from(row.index())
                .ok()
                .and_then(|index| inner.shown.borrow().get(index).map(|choice| choice.id));
            if let Some(id) = id {
                inner.run(id);
            }
        }
    ));
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[strong]
        inner,
        move |_, key, _, _| {
            let step = match key {
                gdk::Key::Down => 1,
                gdk::Key::Up => -1,
                gdk::Key::Page_Down => 8,
                gdk::Key::Page_Up => -8,
                _ => return glib::Propagation::Proceed,
            };
            inner.move_selection(step);
            glib::Propagation::Stop
        }
    ));
    entry.add_controller(keys);
}

impl Inner {
    fn refresh(&self) {
        let (Some(entry), Some(list)) = (self.entry.upgrade(), self.list.upgrade()) else {
            return;
        };
        let query = entry.text();
        let texts: Vec<String> = self.choices.iter().map(Choice::search_text).collect();
        let shown: Vec<Choice> = palette::rank(&query, texts.iter().map(String::as_str))
            .into_iter()
            .map(|index| self.choices[index].clone())
            .collect();
        list.remove_all();
        for choice in &shown {
            list.append(&row(choice));
        }
        if let Some(first) = list.row_at_index(0) {
            list.select_row(Some(&first));
        }
        self.shown.replace(shown);
    }

    fn selected(&self) -> Option<Choice> {
        let row = self.list.upgrade()?.selected_row()?;
        let index = usize::try_from(row.index()).ok()?;
        self.shown.borrow().get(index).cloned()
    }

    fn move_selection(&self, step: i32) {
        let Some(list) = self.list.upgrade() else {
            return;
        };
        let count = i32::try_from(self.shown.borrow().len()).unwrap_or(i32::MAX);
        if count == 0 {
            return;
        }
        let current = list.selected_row().map_or(-1, |row| row.index());
        let next = (current + step).clamp(0, count - 1);
        if let Some(row) = list.row_at_index(next) {
            list.select_row(Some(&row));
            // The focus stays in the entry, so a screen reader hears the row this way.
            if let (Some(entry), Some(choice)) = (
                self.entry.upgrade(),
                usize::try_from(next)
                    .ok()
                    .and_then(|index| self.shown.borrow().get(index).cloned()),
            ) {
                crate::accessibility::announce(&entry, &choice.spoken());
            }
            if let (Some(scrolled), Some(bounds)) =
                (self.scrolled.upgrade(), row.compute_bounds(&list))
            {
                let adjustment = scrolled.vadjustment();
                let (top, bottom) = (
                    f64::from(bounds.y()),
                    f64::from(bounds.y() + bounds.height()),
                );
                if top < adjustment.value() {
                    adjustment.set_value(top);
                } else if bottom > adjustment.value() + adjustment.page_size() {
                    adjustment.set_value(bottom - adjustment.page_size());
                }
            }
        }
    }

    /// Closes the picker, then moves the messages.
    fn run(&self, id: FolderId) {
        if let Some(dialog) = self.dialog.upgrade() {
            dialog.close();
        }
        (self.pick)(id);
    }
}

fn row(choice: &Choice) -> gtk::ListBoxRow {
    let title = gtk::Label::builder()
        .label(&choice.title)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["sw-palette-title"])
        .build();
    let detail = gtk::Label::builder()
        .label(&choice.detail)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["sw-palette-category", "sw-secondary"])
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_start(10)
        .margin_end(10)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    content.append(&title);
    content.append(&detail);
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .css_classes(["sw-palette-row", "sw-move-to-row"])
        .build();
    // The focus stays in the search entry, where Up and Down move the chosen row and a screen
    // reader hears it: Tab doesn't walk every row.
    row.set_focusable(false);
    row.update_property(&[gtk::accessible::Property::Label(&choice.spoken())]);
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::model::AccountId;

    fn folder(id: i64, name: &str, role: FolderRole) -> Folder {
        Folder {
            id: FolderId(id),
            account: AccountId(1),
            name: name.to_owned(),
            delimiter: Some('/'),
            role,
            selectable: true,
            inferiors: true,
            total: 0,
            unread: 0,
        }
    }

    #[test]
    fn subfolders_show_by_their_last_level_with_the_folders_above() {
        let known = vec![
            folder(1, "INBOX", FolderRole::Inbox),
            folder(2, "INBOX/Kvitteringer", FolderRole::Other),
            folder(3, "Kunder", FolderRole::Other),
            folder(4, "Kunder/2026/Acme", FolderRole::Other),
            folder(5, "Sendt", FolderRole::Sent),
        ];
        let choice = |index: usize| Choice::new(&known[index], &known);
        let receipts = choice(1);
        assert_eq!(
            (receipts.title.as_str(), receipts.detail.as_str()),
            ("Kvitteringer", "Inbox")
        );
        assert_eq!(receipts.spoken(), "Move to Kvitteringer, Inbox");
        let acme = choice(3);
        assert_eq!(
            (acme.title.as_str(), acme.detail.as_str()),
            ("Acme", "Kunder / 2026"),
            "an unlisted level by its name"
        );
        assert!(acme.search_text().contains("Kunder"));
        let top = choice(2);
        assert_eq!((top.title.as_str(), top.detail.as_str()), ("Kunder", ""));
        let sent = choice(4);
        assert_eq!(
            (sent.title.as_str(), sent.detail.as_str()),
            ("Sent", "Sendt")
        );
    }
}
