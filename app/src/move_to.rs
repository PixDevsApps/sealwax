//! Move to… (ML-052): a keyboard-first picker over the folders the selected messages can move
//! to, their account's folders that take messages except the one they are in, role folders
//! first by their role's name. A subfolder shows by its last level first, the folders above it
//! beside it ("Acme", "Kunder / 2026"; M9). Typing filters with Stet's matcher, Up and Down
//! move, Enter moves the messages, Escape closes; a click picks a folder. It is an `AdwDialog`
//! like the command palette, so its entry keeps every letter: the list's one-key shortcuts don't
//! reach it.
//!
//! Go to Folder… (1.5.0, issue #12) is the same picker over every account's folders, All
//! Inboxes first, each folder with its account beside it when there is more than one; choosing
//! one shows it.

use crate::triage::folder_title;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::folders;
use sealwax_domain::model::{Account, Folder, FolderId, FolderRole};
use sealwax_ui_model::fuzzy;
use std::cell::RefCell;
use std::rc::Rc;

/// Where a choice leads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Place {
    Folder(FolderId),
    AllInboxes,
}

/// One folder the picker offers.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Choice {
    place: Place,
    title: String,
    /// The server's own name of a role folder, when it differs from the role's ("Sendt"); the
    /// folders above a subfolder ("Kunder / 2026").
    detail: String,
}

impl Choice {
    /// What a screen reader says for the row, after `verb` ("Move to").
    fn spoken(&self, verb: &str) -> String {
        if self.detail.is_empty() {
            format!("{verb} {}", self.title)
        } else {
            format!("{verb} {}, {}", self.title, self.detail)
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
                place: Place::Folder(folder.id),
                title: folders::leaf(&folder.name, folder.delimiter).to_owned(),
                detail: above.join(" / "),
            };
        }
        let title = folder_title(folder);
        let detail = if folder.name.eq_ignore_ascii_case(&title) {
            String::new()
        } else {
            folder.name.clone()
        };
        Self {
            place: Place::Folder(folder.id),
            title,
            detail,
        }
    }

    /// `folder`'s choice for Go to Folder…, its account named beside it when `account` is some.
    fn in_account(folder: &Folder, known: &[Folder], account: Option<&str>) -> Self {
        let mut choice = Self::new(folder, known);
        if let Some(account) = account {
            choice.detail = if choice.detail.is_empty() {
                account.to_owned()
            } else {
                format!("{} — {account}", choice.detail)
            };
        }
        choice
    }

    fn search_text(&self) -> String {
        format!("{} {}", self.title, self.detail)
    }
}

type Pick = Rc<dyn Fn(Place)>;

/// What the picker says: its title and placeholder, what a screen reader says before a
/// folder's name, and its widget name.
struct Texts {
    title: String,
    verb: &'static str,
    name: &'static str,
}

struct Inner {
    dialog: glib::WeakRef<adw::Dialog>,
    entry: glib::WeakRef<gtk::SearchEntry>,
    list: glib::WeakRef<gtk::ListBox>,
    scrolled: glib::WeakRef<gtk::ScrolledWindow>,
    choices: Vec<Choice>,
    shown: RefCell<Vec<Choice>>,
    verb: &'static str,
    pick: Pick,
}

thread_local! {
    /// The picker open now, for the self-test.
    static OPEN: RefCell<Option<MoveTo>> = const { RefCell::new(None) };
}

/// An open picker; [`present`] makes one.
#[derive(Clone)]
pub(crate) struct MoveTo {
    dialog: adw::Dialog,
    inner: Rc<Inner>,
}

/// Opens the picker over `window` for `count` messages, offering `folders` in their order;
/// `known` are every folder, which name the folders above a subfolder; `pick` gets the folder
/// chosen.
pub(crate) fn present(
    window: &impl IsA<gtk::Widget>,
    folders: &[Folder],
    known: &[Folder],
    count: u32,
    pick: impl Fn(FolderId) + 'static,
) -> MoveTo {
    let title = match count {
        1 => "Move 1 message to…".to_owned(),
        count => format!("Move {count} messages to…"),
    };
    let choices = folders
        .iter()
        .map(|folder| Choice::new(folder, known))
        .collect();
    let texts = Texts {
        title,
        verb: "Move to",
        name: "move-to",
    };
    show(
        window,
        MoveTo::new(
            choices,
            texts,
            Rc::new(move |place| {
                if let Place::Folder(folder) = place {
                    pick(folder);
                }
            }),
        ),
    )
}

/// Opens Go to Folder… over `window`: All Inboxes first when there is an account, then each of
/// `accounts`' folders that take messages, in the order Move to… lists them; `known` are every
/// folder. `pick` gets the place chosen.
pub(crate) fn present_go_to(
    window: &impl IsA<gtk::Widget>,
    accounts: &[Account],
    known: &[Folder],
    pick: impl Fn(Place) + 'static,
) -> MoveTo {
    let texts = Texts {
        title: "Go to folder…".to_owned(),
        verb: "Go to",
        name: "go-to-folder",
    };
    show(
        window,
        MoveTo::new(go_to_choices(accounts, known), texts, Rc::new(pick)),
    )
}

/// Go to Folder…'s choices: All Inboxes, then every account's folders, each account's in Move
/// to…'s order, named with their account when there is more than one.
fn go_to_choices(accounts: &[Account], known: &[Folder]) -> Vec<Choice> {
    let mut choices = Vec::new();
    if !accounts.is_empty() {
        choices.push(Choice {
            place: Place::AllInboxes,
            title: "All Inboxes".to_owned(),
            detail: String::new(),
        });
    }
    let named = accounts.len() > 1;
    for account in accounts {
        let mut folders: Vec<Folder> = known
            .iter()
            .filter(|folder| folder.account == account.id && folder.selectable)
            .cloned()
            .collect();
        crate::triage::sort_for_picker(&mut folders);
        choices.extend(folders.iter().map(|folder| {
            Choice::in_account(folder, known, named.then_some(account.name.as_str()))
        }));
    }
    choices
}

fn show(window: &impl IsA<gtk::Widget>, picker: MoveTo) -> MoveTo {
    picker.dialog.present(Some(window));
    if let Some(entry) = picker.inner.entry.upgrade() {
        entry.grab_focus();
    }
    OPEN.with(|open| open.replace(Some(picker.clone())));
    picker
}

/// The picker open now, if any.
#[cfg(feature = "test-hooks")]
pub(crate) fn current() -> Option<MoveTo> {
    OPEN.with(|open| open.borrow().clone())
        .filter(|picker| picker.dialog.is_mapped())
}

impl MoveTo {
    fn new(choices: Vec<Choice>, texts: Texts, pick: Pick) -> Self {
        let Texts { title, verb, name } = texts;
        let entry = gtk::SearchEntry::builder()
            .placeholder_text(title.as_str())
            .search_delay(0)
            .hexpand(true)
            .css_classes(["sw-palette-search", "sw-move-to-search"])
            .build();
        entry.set_widget_name(&format!("{name}-search"));
        entry.update_property(&[gtk::accessible::Property::Label(&title)]);
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Browse)
            .css_classes(["sw-palette-list", "sw-move-to-list"])
            .build();
        list.set_widget_name(&format!("{name}-list"));
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
        dialog.set_widget_name(name);
        let inner = Rc::new(Inner {
            dialog: dialog.downgrade(),
            entry: entry.downgrade(),
            list: list.downgrade(),
            scrolled: scrolled.downgrade(),
            choices,
            shown: RefCell::new(Vec::new()),
            verb,
            pick,
        });
        connect(&inner, &entry, &list);
        inner.refresh();
        Self { dialog, inner }
    }

    #[cfg(feature = "test-hooks")]
    pub(crate) fn search_entry(&self) -> Option<gtk::SearchEntry> {
        self.inner.entry.upgrade()
    }

    /// The folders shown, best match first, by their titles.
    #[cfg(feature = "test-hooks")]
    pub(crate) fn shown(&self) -> Vec<String> {
        self.inner
            .shown
            .borrow()
            .iter()
            .map(|choice| choice.title.clone())
            .collect()
    }

    /// The folder Enter would pick, by its title.
    #[cfg(feature = "test-hooks")]
    pub(crate) fn selected(&self) -> Option<String> {
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
                inner.run(choice.place);
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
            let place = usize::try_from(row.index())
                .ok()
                .and_then(|index| inner.shown.borrow().get(index).map(|choice| choice.place));
            if let Some(place) = place {
                inner.run(place);
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
        let shown: Vec<Choice> = fuzzy::rank(&query, texts.iter().map(String::as_str))
            .into_iter()
            .map(|index| self.choices[index].clone())
            .collect();
        list.remove_all();
        for choice in &shown {
            list.append(&row(choice, self.verb));
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
                crate::accessibility::announce(&entry, &choice.spoken(self.verb));
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

    /// Closes the picker, then hands over the place chosen.
    fn run(&self, place: Place) {
        if let Some(dialog) = self.dialog.upgrade() {
            dialog.close();
        }
        (self.pick)(place);
    }
}

fn row(choice: &Choice, verb: &str) -> gtk::ListBoxRow {
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
    row.update_property(&[gtk::accessible::Property::Label(&choice.spoken(verb))]);
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
        assert_eq!(receipts.spoken("Move to"), "Move to Kvitteringer, Inbox");
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

    #[test]
    fn go_to_folder_offers_all_inboxes_then_every_accounts_folders() {
        let mut known = vec![
            folder(1, "INBOX", FolderRole::Inbox),
            folder(2, "Kunder", FolderRole::Other),
            folder(3, "Sendt", FolderRole::Sent),
        ];
        let mut other = folder(4, "INBOX", FolderRole::Inbox);
        other.account = AccountId(2);
        known.push(other);
        let mut hidden = folder(5, "[Gmail]", FolderRole::Other);
        hidden.selectable = false;
        known.push(hidden);
        let account = |id: i64, name: &str| Account {
            id: AccountId(id),
            name: name.to_owned(),
            address: format!("{name}@example.test"),
            key: None,
        };
        let accounts = [account(2, "Home"), account(1, "Work")];
        let choices = go_to_choices(&accounts, &known);
        let shown: Vec<(&str, &str)> = choices
            .iter()
            .map(|choice| (choice.title.as_str(), choice.detail.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                ("All Inboxes", ""),
                ("Inbox", "Home"),
                ("Inbox", "Work"),
                ("Sent", "Sendt — Work"),
                ("Kunder", "Work"),
            ],
            "the accounts in the sidebar's order, roles first, no folder that takes no messages"
        );
        assert_eq!(choices[0].place, Place::AllInboxes);
        assert_eq!(choices[2].place, Place::Folder(FolderId(1)));
        assert_eq!(choices[4].spoken("Go to"), "Go to Kunder, Work");
        let one = go_to_choices(&accounts[1..], &known[..3]);
        assert_eq!(one[3].detail, "", "one account: no account name");
        assert!(go_to_choices(&[], &known).is_empty());
    }
}
