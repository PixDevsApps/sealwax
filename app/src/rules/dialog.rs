//! The Rules dialog (ADR-020): one account's rules in order, each with its name, a one-line
//! summary, an on/off switch, Move Up, Move Down, Edit and Delete; New Rule; the always-junk
//! and never-junk senders, each removable, with a field to add one; Apply to Folder…. Rules
//! the model doesn't recognise stay where they are as "Written by hand", and the page says
//! "Edited by hand" with Open File; a file that can't be read shows why, with Open File, and
//! nothing else. Every change is saved at once (the file off the GTK thread, `file::run`);
//! Delete has Undo in the dialog's own toast.
//!
//! Named for the self-test: `rules-dialog`, `rules-page`, `rules-new`, `rules-notice`,
//! `rules-open-file`, `rule-row-N`, `rule-switch-N`, `rule-up-N`, `rule-down-N`,
//! `rule-edit-N`, `rule-delete-N` (N the rule's place among the file's items),
//! `blocked-row-N`, `blocked-remove-N`, `blocked-add`, the same with `trusted-`,
//! `rules-apply`, `rules-apply-folder-N`.

use super::draft::Draft;
use super::editor::{self, Editor};
use super::file::{self, Loaded, Place, SaveError, Unreadable};
use super::text;
use super::{Account, open_file};
use crate::application::AppState;
use crate::window::panes::{classed, icon_button, named};
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::model::FolderId;
use sealwax_domain::sieve::{Item, Rule, RuleSet};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::SystemTime;

/// What the dialog shows first.
pub enum Start {
    /// The rules.
    List,
    /// The editor with a new rule, over the rules (Create Rule from Message).
    New(Draft),
}

/// The open dialog, for the self-test and for changes made elsewhere.
#[derive(Clone)]
pub struct Shown {
    inner: Rc<Inner>,
}

struct Inner {
    state: Weak<AppState>,
    account: Account,
    place: Place,
    dialog: adw::Dialog,
    nav: adw::NavigationView,
    toasts: adw::ToastOverlay,
    notice: adw::PreferencesGroup,
    rules: adw::PreferencesGroup,
    blocked: adw::PreferencesGroup,
    trusted: adw::PreferencesGroup,
    apply: adw::PreferencesGroup,
    new_rule: gtk::Button,
    /// The rows each group shows now, removed when it is filled again.
    rows: RefCell<Vec<(adw::PreferencesGroup, gtk::Widget)>>,
    set: RefCell<Option<RuleSet>>,
    unreadable: RefCell<Option<Unreadable>>,
    /// The file's modification time as last read or written here.
    modified: Cell<Option<SystemTime>>,
    saving: Cell<bool>,
    dirty: Cell<bool>,
    editor: RefCell<Option<Editor>>,
    /// Where the keyboard focus goes after the next fill, the first that can take it.
    focus_next: RefCell<Vec<String>>,
    weak: RefCell<Weak<Inner>>,
}

thread_local! {
    static OPEN: RefCell<Weak<Inner>> = const { RefCell::new(Weak::new()) };
}

/// The dialog open now, if any.
pub fn current() -> Option<Shown> {
    OPEN.with(|open| open.borrow().upgrade())
        .filter(|inner| inner.dialog.is_mapped())
        .map(|inner| Shown { inner })
}

/// The account's rules file changed elsewhere (Always Junk, Never Junk, a renamed folder):
/// the dialog showing it reads it again.
pub fn reload_if_open(account: &Account) {
    if let Some(shown) = current()
        && shown.inner.account.key == account.key
    {
        shown.inner.load(None);
    }
}

/// Opens the Rules dialog for `account` over the main window.
pub fn open(state: &Rc<AppState>, account: Account, start: Start) {
    let Some(window) = state.window() else {
        return;
    };
    if let Some(shown) = current() {
        shown.inner.dialog.close();
    }
    let place = account.place(state);
    let notice = named(adw::PreferencesGroup::new(), "rules-notice");
    let rules = adw::PreferencesGroup::builder().title("Rules").build();
    let new_rule = named(
        classed(
            gtk::Button::builder()
                .child(
                    &adw::ButtonContent::builder()
                        .icon_name("list-add-symbolic")
                        .label("New Rule")
                        .build(),
                )
                .valign(gtk::Align::Center)
                .build(),
            &["flat"],
        ),
        "rules-new",
    );
    rules.set_header_suffix(Some(&new_rule));
    let blocked = adw::PreferencesGroup::builder()
        .title("Always junk")
        .description("Mail from these senders goes to Junk.")
        .build();
    let trusted = adw::PreferencesGroup::builder()
        .title("Never junk")
        .description("Mail from these senders that arrives in Junk goes back to the Inbox.")
        .build();
    let apply = adw::PreferencesGroup::new();
    let preferences = named(adw::PreferencesPage::new(), "rules-page");
    for (group, label) in [
        (&notice, "Rules file"),
        (&rules, "Rules"),
        (&blocked, "Always junk"),
        (&trusted, "Never junk"),
        (&apply, "Apply rules"),
    ] {
        name_lists(group.upcast_ref(), label);
        preferences.add(group);
    }
    let header = adw::HeaderBar::new();
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&preferences));
    let page = adw::NavigationPage::builder()
        .title(text::dialog_title(&account.name))
        .tag("rules")
        .child(&view)
        .build();
    let nav = adw::NavigationView::new();
    nav.add(&page);
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&nav));
    let dialog = named(
        adw::Dialog::builder()
            .title(text::dialog_title(&account.name))
            .content_width(680)
            .content_height(640)
            .child(&toasts)
            .build(),
        "rules-dialog",
    );
    let inner = Rc::new(Inner {
        state: Rc::downgrade(state),
        account,
        place,
        dialog: dialog.clone(),
        nav,
        toasts,
        notice,
        rules,
        blocked,
        trusted,
        apply,
        new_rule,
        rows: RefCell::new(Vec::new()),
        set: RefCell::new(None),
        unreadable: RefCell::new(None),
        modified: Cell::new(None),
        saving: Cell::new(false),
        dirty: Cell::new(false),
        editor: RefCell::new(None),
        focus_next: RefCell::new(Vec::new()),
        weak: RefCell::new(Weak::new()),
    });
    inner.weak.replace(Rc::downgrade(&inner));
    let weak = Rc::downgrade(&inner);
    inner.new_rule.connect_clicked(move |_| {
        if let Some(inner) = weak.upgrade() {
            inner.edit(None, &Draft::default());
        }
    });
    // The dialog holds what its handlers share until it closes.
    let held = RefCell::new(Some(inner.clone()));
    dialog.connect_closed(move |_| {
        held.take();
    });
    OPEN.with(|open| open.replace(Rc::downgrade(&inner)));
    inner.fill();
    dialog.present(Some(window.window()));
    inner.load(Some(start));
}

impl Shown {
    pub fn dialog(&self) -> &adw::Dialog {
        &self.inner.dialog
    }

    /// The rules as the dialog holds them.
    pub fn rule_set(&self) -> Option<RuleSet> {
        self.inner.set.borrow().clone()
    }

    /// Whether a save is waiting or running.
    pub fn is_saving(&self) -> bool {
        self.inner.saving.get() || self.inner.dirty.get()
    }

    /// The tag of the page on top: `rules`, `edit` or `apply`.
    pub fn page(&self) -> String {
        self.inner
            .nav
            .visible_page()
            .and_then(|page| page.tag())
            .map(|tag| tag.to_string())
            .unwrap_or_default()
    }

    /// Why the open editor can't save, if it can't.
    pub fn editor_problem(&self) -> Option<&'static str> {
        self.inner
            .editor
            .borrow()
            .as_ref()
            .and_then(Editor::problem)
    }
}

impl Inner {
    /// Reads the file off the GTK thread, then shows it; `start` once it is read.
    fn load(&self, start: Option<Start>) {
        let weak = self.weak.borrow().clone();
        let place = self.place.clone();
        glib::spawn_future_local(async move {
            let loaded = file::run(move || file::load(&place)).await;
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let loaded = loaded.unwrap_or(Loaded {
                set: Err(Unreadable::File),
                modified: None,
            });
            inner.modified.set(loaded.modified);
            match loaded.set {
                Ok(set) => {
                    inner.set.replace(Some(set));
                    inner.unreadable.replace(None);
                }
                Err(problem) => {
                    inner.set.replace(None);
                    inner.unreadable.replace(Some(problem));
                }
            }
            inner.fill();
            if let Some(Start::New(draft)) = start
                && inner.set.borrow().is_some()
            {
                inner.edit(None, &draft);
            }
        });
    }

    fn add_row(&self, group: &adw::PreferencesGroup, row: &impl IsA<gtk::Widget>) {
        group.add(row);
        self.rows
            .borrow_mut()
            .push((group.clone(), row.clone().upcast()));
    }

    /// Fills the page from the rules held. The keyboard focus stays on the widget of the same
    /// name, else nearby: a list's field, else New Rule.
    fn fill(&self) {
        let mut wanted = self.focus_next.take();
        if wanted.is_empty()
            && let Some(name) = self
                .dialog
                .focus()
                .filter(|widget| widget.is_ancestor(&self.dialog))
                .map(|widget| named_around(&widget))
                .filter(|name| !name.is_empty())
        {
            wanted.push(name);
        }
        for (group, row) in self.rows.take() {
            group.remove(&row);
        }
        self.fill_groups();
        if let Some(first) = wanted.first() {
            let fallback = if first.starts_with("blocked-") {
                "blocked-add"
            } else if first.starts_with("trusted-") {
                "trusted-add"
            } else {
                "rules-new"
            };
            // Once the new rows are on screen, where they can take the focus.
            let weak = self.weak.borrow().clone();
            glib::idle_add_local_once(move || {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let target = wanted
                    .iter()
                    .filter_map(|name| inner.find(name))
                    .find(|widget| widget.is_sensitive())
                    .or_else(|| inner.find(fallback))
                    .filter(|widget| widget.is_visible());
                if let Some(target) = target {
                    target.grab_focus();
                }
            });
        }
    }

    fn fill_groups(&self) {
        let set = self.set.borrow().clone();
        let unreadable = self.unreadable.borrow().clone();
        let readable = set.is_some();
        for group in [&self.rules, &self.blocked, &self.trusted, &self.apply] {
            group.set_visible(readable);
        }
        match (&set, &unreadable) {
            (_, Some(problem)) => {
                let why = match problem {
                    Unreadable::Script(error) => text::script_error(error),
                    Unreadable::File => text::problem_text(&sealwax_engine::api::RulesError::File),
                };
                self.notice.set_title("The rules file can't be read");
                self.notice.set_description(Some(&format!(
                    "{why}. Fix it in the file, or remove the file to start again."
                )));
                self.notice
                    .set_header_suffix(Some(&self.open_file_button()));
                self.notice.set_visible(true);
            }
            (Some(set), None) if set.is_hand_written() => {
                self.notice.set_title("Edited by hand");
                self.notice.set_description(Some(
                    "Parts of the rules file are Sieve written by hand. They run as written, in \
                     their place among the rules; change them in the file.",
                ));
                self.notice
                    .set_header_suffix(Some(&self.open_file_button()));
                self.notice.set_visible(true);
            }
            _ => {
                self.notice.set_header_suffix(None::<&gtk::Widget>);
                self.notice.set_visible(false);
            }
        }
        let Some(set) = set else {
            return;
        };
        self.fill_rules(&set);
        self.fill_senders(&set, true);
        self.fill_senders(&set, false);
        let apply = named(
            adw::ActionRow::builder()
                .title("Apply to Folder…")
                .subtitle("Run these rules now on the messages a folder holds")
                .activatable(true)
                .use_markup(false)
                .build(),
            "rules-apply",
        );
        apply.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let weak = self.weak.borrow().clone();
        apply.connect_activated(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.show_apply();
            }
        });
        self.add_row(&self.apply, &apply);
    }

    fn open_file_button(&self) -> gtk::Button {
        let button = named(
            gtk::Button::builder()
                .label("Open File")
                .valign(gtk::Align::Center)
                .build(),
            "rules-open-file",
        );
        let weak = self.weak.borrow().clone();
        button.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade()
                && let Some(state) = inner.state.upgrade()
            {
                open_file(&state, &inner.place);
            }
        });
        button
    }

    fn fill_rules(&self, set: &RuleSet) {
        let positions: Vec<usize> = set
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| matches!(item, Item::Rule(_)))
            .map(|(index, _)| index)
            .collect();
        if set.items.is_empty() {
            let empty = adw::ActionRow::builder()
                .title("No rules yet")
                .subtitle(
                    "New Rule adds one; Create Rule from Message in a message's menu fills one \
                     in from it.",
                )
                .use_markup(false)
                .build();
            self.add_row(&self.rules, &empty);
        }
        for (index, item) in set.items.iter().enumerate() {
            match item {
                Item::Rule(rule) => {
                    let first = positions.first() == Some(&index);
                    let last = positions.last() == Some(&index);
                    let row = self.rule_row(index, rule, first, last);
                    self.add_row(&self.rules, &row);
                }
                Item::HandWritten(written) => {
                    let lines = written
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .count();
                    let row = named(
                        adw::ActionRow::builder()
                            .title("Written by hand")
                            .subtitle(match lines {
                                1 => "1 line of Sieve this dialog doesn't show".to_owned(),
                                lines => format!("{lines} lines of Sieve this dialog doesn't show"),
                            })
                            .use_markup(false)
                            .build(),
                        &format!("rule-hand-{index}"),
                    );
                    self.add_row(&self.rules, &row);
                }
            }
        }
    }

    fn rule_row(&self, index: usize, rule: &Rule, first: bool, last: bool) -> adw::ActionRow {
        let title = text::rule_title(rule).to_owned();
        let row = named(
            adw::ActionRow::builder()
                .title(&title)
                .subtitle(text::summary(rule, &self.account.folders))
                .subtitle_lines(1)
                .use_markup(false)
                .activatable(true)
                .build(),
            &format!("rule-row-{index}"),
        );
        let switch = named(
            gtk::Switch::builder()
                .active(rule.enabled)
                .valign(gtk::Align::Center)
                .build(),
            &format!("rule-switch-{index}"),
        );
        switch.update_property(&[gtk::accessible::Property::Label(&title)]);
        switch.set_tooltip_text(Some("On or off"));
        let weak = self.weak.borrow().clone();
        switch.connect_active_notify(move |switch| {
            let on = switch.is_active();
            if let Some(inner) = weak.upgrade() {
                inner.change_rule(index, |rule| rule.enabled = on);
            }
        });
        row.add_suffix(&switch);
        let button = |icons: &[&str], label: &str, part: &str, sensitive: bool| {
            let button = icon_button(icons, label);
            button.add_css_class("flat");
            button.set_valign(gtk::Align::Center);
            button.set_sensitive(sensitive);
            button.set_widget_name(&format!("rule-{part}-{index}"));
            row.add_suffix(&button);
            button
        };
        let up = button(&["go-up-symbolic"], "Move Up", "up", !first);
        let down = button(&["go-down-symbolic"], "Move Down", "down", !last);
        let edit = button(&["document-edit-symbolic"], "Edit", "edit", true);
        let delete = button(&["user-trash-symbolic"], "Delete", "delete", true);
        let weak = self.weak.borrow().clone();
        up.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.move_rule(index, false);
            }
        });
        let weak = self.weak.borrow().clone();
        down.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.move_rule(index, true);
            }
        });
        let weak = self.weak.borrow().clone();
        let edit_rule = move || {
            if let Some(inner) = weak.upgrade() {
                let draft = match inner
                    .set
                    .borrow()
                    .as_ref()
                    .and_then(|set| set.items.get(index))
                {
                    Some(Item::Rule(rule)) => Draft::from_rule(rule),
                    _ => return,
                };
                inner.edit(Some(index), &draft);
            }
        };
        let activate = edit_rule.clone();
        edit.connect_clicked(move |_| edit_rule());
        row.connect_activated(move |_| activate());
        let weak = self.weak.borrow().clone();
        delete.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.delete_rule(index);
            }
        });
        row
    }

    fn fill_senders(&self, set: &RuleSet, always: bool) {
        let (group, senders, prefix) = if always {
            (&self.blocked, &set.blocked, "blocked")
        } else {
            (&self.trusted, &set.trusted, "trusted")
        };
        let entries: Vec<(String, bool)> = senders
            .addresses
            .iter()
            .map(|address| (address.clone(), false))
            .chain(senders.domains.iter().map(|domain| (domain.clone(), true)))
            .collect();
        for (index, (sender, domain)) in entries.into_iter().enumerate() {
            let row = named(
                adw::ActionRow::builder()
                    .title(&sender)
                    .use_markup(false)
                    .build(),
                &format!("{prefix}-row-{index}"),
            );
            if domain {
                row.set_subtitle("Every address at this domain");
            }
            let remove = icon_button(&["list-remove-symbolic"], "Remove");
            remove.add_css_class("flat");
            remove.set_valign(gtk::Align::Center);
            remove.set_widget_name(&format!("{prefix}-remove-{index}"));
            remove.update_property(&[gtk::accessible::Property::Label(&format!(
                "Remove {sender}"
            ))]);
            let weak = self.weak.borrow().clone();
            remove.connect_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.change(|set| {
                        let senders = if always {
                            &mut set.blocked
                        } else {
                            &mut set.trusted
                        };
                        if domain {
                            senders.remove_domain(&sender)
                        } else {
                            senders.remove_address(&sender)
                        }
                    });
                }
            });
            row.add_suffix(&remove);
            self.add_row(group, &row);
        }
        let add = named(
            adw::EntryRow::builder()
                .title("Add an address or a domain")
                .show_apply_button(true)
                .build(),
            &format!("{prefix}-add"),
        );
        let weak = self.weak.borrow().clone();
        add.connect_apply(move |entry| {
            let typed = entry.text().trim().to_owned();
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let mut added = false;
            inner.change(|set| {
                added = match (typed.contains('@') && !typed.starts_with('@'), always) {
                    (true, true) => set.always_junk(&typed),
                    (true, false) => set.never_junk(&typed),
                    (false, true) => set.always_junk_domain(&typed),
                    (false, false) => set.never_junk_domain(&typed),
                };
                added
            });
            if !added {
                inner.toast(&format!(
                    "{typed} isn't an address or a domain, or is listed"
                ));
            }
        });
        self.add_row(group, &add);
    }

    fn toast(&self, text: &str) {
        self.toasts
            .add_toast(adw::Toast::builder().title(text).use_markup(false).build());
    }

    /// Changes the rules held with `edit` and saves them when it says it changed them.
    fn change(&self, edit: impl FnOnce(&mut RuleSet) -> bool) {
        let changed = match self.set.borrow_mut().as_mut() {
            Some(set) => edit(set),
            None => false,
        };
        if changed {
            self.fill();
            self.save();
        }
    }

    fn change_rule(&self, index: usize, edit: impl FnOnce(&mut Rule)) {
        let changed = match self
            .set
            .borrow_mut()
            .as_mut()
            .and_then(|set| set.items.get_mut(index))
        {
            Some(Item::Rule(rule)) => {
                edit(rule);
                true
            }
            _ => false,
        };
        if changed {
            // The switch already shows the change; the rest of the row follows on the next
            // fill, so the focus stays on it.
            self.save();
        }
    }

    /// Moves the rule at `index` past the next item (`down`) or the one before it.
    fn move_rule(&self, index: usize, down: bool) {
        let other = if down {
            index + 1
        } else {
            match index.checked_sub(1) {
                Some(other) => other,
                None => return,
            }
        };
        // The focus follows the rule to its new place, on the same button where it can.
        let (same, opposite) = if down { ("down", "up") } else { ("up", "down") };
        self.focus_next.replace(vec![
            format!("rule-{same}-{other}"),
            format!("rule-{opposite}-{other}"),
        ]);
        self.change(|set| {
            if other >= set.items.len() {
                return false;
            }
            set.items.swap(index, other);
            true
        });
        self.focus_next.take();
    }

    fn delete_rule(&self, index: usize) {
        let mut removed = None;
        self.change(|set| {
            if !matches!(set.items.get(index), Some(Item::Rule(_))) {
                return false;
            }
            removed = Some(set.items.remove(index));
            true
        });
        let Some(Item::Rule(rule)) = removed else {
            return;
        };
        let toast = adw::Toast::builder()
            .title(format!("Deleted “{}”", text::rule_title(&rule)))
            .use_markup(false)
            .button_label("Undo")
            .build();
        let weak = self.weak.borrow().clone();
        toast.connect_button_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                let rule = rule.clone();
                inner.change(move |set| {
                    let at = index.min(set.items.len());
                    set.items.insert(at, Item::Rule(rule));
                    true
                });
            }
        });
        self.toasts.add_toast(toast);
    }

    /// The editor over the rules, for the rule at `index` or a new one.
    fn edit(&self, index: Option<usize>, draft: &Draft) {
        let title = if index.is_some() {
            "Edit Rule"
        } else {
            "New Rule"
        };
        let targets = self.account.targets();
        let weak = self.weak.borrow().clone();
        let editor = editor::new(title, draft, targets, move |rule| {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            inner.change(|set| {
                match index {
                    Some(index) if matches!(set.items.get(index), Some(Item::Rule(_))) => {
                        set.items[index] = Item::Rule(rule);
                    }
                    _ => set.items.push(Item::Rule(rule)),
                }
                true
            });
            inner.nav.pop_to_tag("rules");
        });
        back_tooltip(&editor.page);
        self.nav.push(&editor.page);
        self.editor.replace(Some(editor));
    }

    /// The folders the rules can run on: a page of them, the folder shown first.
    fn show_apply(&self) {
        let list = classed(
            gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .build(),
            &["boxed-list"],
        );
        list.update_property(&[gtk::accessible::Property::Label("Folders")]);
        for (index, folder) in self.account.targets().into_iter().enumerate() {
            let row = named(
                adw::ActionRow::builder()
                    .title(&folder.label)
                    .use_markup(false)
                    .activatable(true)
                    .build(),
                &format!("rules-apply-folder-{index}"),
            );
            let id = self.account.folder_id(&folder.name);
            let weak = self.weak.borrow().clone();
            row.connect_activated(move |_| {
                if let (Some(inner), Some(id)) = (weak.upgrade(), id) {
                    inner.apply_to(id);
                }
            });
            list.append(&row);
        }
        let caption = classed(
            gtk::Label::builder()
                .label(
                    "The rules run on every message the folder holds, as if it had just arrived \
                     there.",
                )
                .xalign(0.0)
                .wrap(true)
                .build(),
            &["dim-label"],
        );
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&caption);
        content.append(&list);
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&adw::Clamp::builder().child(&content).build())
            .build();
        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        view.set_content(Some(&scrolled));
        let page = adw::NavigationPage::builder()
            .title("Apply Rules to a Folder")
            .tag("apply")
            .child(&view)
            .build();
        back_tooltip(&page);
        self.nav.push(&page);
    }

    fn apply_to(&self, folder: FolderId) {
        if let Some(state) = self.state.upgrade() {
            self.dialog.close();
            super::apply(&state, folder);
        }
    }

    /// Saves the rules held, one write at a time: a change made while one runs is written
    /// after it, against the time it left.
    fn save(&self) {
        if self.saving.get() {
            self.dirty.set(true);
            return;
        }
        self.saving.set(true);
        let weak = self.weak.borrow().clone();
        glib::spawn_future_local(async move {
            loop {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                inner.dirty.set(false);
                let (Some(set), expected) = (inner.set.borrow().clone(), inner.modified.get())
                else {
                    inner.saving.set(false);
                    return;
                };
                let place = inner.place.clone();
                drop(inner);
                let saved = file::run(move || file::save(&place, &set, expected)).await;
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                match saved {
                    Some(Ok(modified)) => {
                        inner.modified.set(modified);
                        super::saved(&inner.account);
                    }
                    Some(Err(SaveError::Changed)) => {
                        inner.saving.set(false);
                        inner
                            .toast("The rules file changed outside Sealwax; it shows as it is now");
                        inner.load(None);
                        return;
                    }
                    Some(Err(_)) | None => {
                        tracing::warn!(account = inner.account.id.0, "the rules weren't saved");
                        inner.toast("The rules couldn't be saved");
                    }
                }
                if !inner.dirty.get() {
                    inner.saving.set(false);
                    return;
                }
            }
        });
    }

    /// A widget of the dialog by its name.
    fn find(&self, name: &str) -> Option<gtk::Widget> {
        find_named(self.dialog.upcast_ref(), name)
    }
}

/// The name of `widget` or of the nearest named widget around it.
fn named_around(widget: &gtk::Widget) -> String {
    let mut current = Some(widget.clone());
    while let Some(widget) = current {
        let name = widget.widget_name();
        if name != widget.type_().name() {
            return name.to_string();
        }
        current = widget.parent();
    }
    String::new()
}

/// Names the lists inside `widget` for screen readers: libadwaita leaves a preferences group's
/// list without one.
fn name_lists(widget: &gtk::Widget, label: &str) {
    if widget.is::<gtk::ListBox>() {
        widget.update_property(&[gtk::accessible::Property::Label(label)]);
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        name_lists(&current, label);
        child = current.next_sibling();
    }
}

/// The back button of `page`'s header bar says what it does when pointed at, as the window's
/// icon buttons do; libadwaita gives it only a name.
fn back_tooltip(page: &adw::NavigationPage) {
    page.connect_shown(|page| {
        let mut stack = vec![page.clone().upcast::<gtk::Widget>()];
        while let Some(widget) = stack.pop() {
            if widget.is::<gtk::Button>() && widget.has_css_class("back") {
                widget.set_tooltip_text(Some("Back"));
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                child = current.next_sibling();
                stack.push(current);
            }
        }
    });
}

fn find_named(widget: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if widget.widget_name() == name {
        return Some(widget.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_named(&current, name) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}
