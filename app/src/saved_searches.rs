//! Saved searches (1.5.0): Save Search… keeps the search the list runs under a name, its query
//! and where it runs (the folder shown, or All Inboxes for every folder); the sidebar lists them
//! under All Inboxes, and choosing one shows its folder and runs it again. A saved search's
//! context menu renames or removes it. They live in `layout.toml` with the window's layout
//! (`ui_model::layout`), written off the GTK thread, and follow a renamed folder; a search whose
//! folder is gone says so when chosen.

use crate::actions::ActionSet;
use crate::application::AppState;
use crate::mailbox::{self, folder_key};
use crate::window::Window;
use crate::window::sidebar::SearchCommands;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_infrastructure::store::ViewScope;
use sealwax_ui_model::actions::ActionId;
use sealwax_ui_model::layout::{SEARCH_TEXT_MAX, SEARCHES_MAX, SavedSearch, View};
use std::rc::Rc;

/// Connects Save Search… on the window's actions; it applies while the list runs a search.
pub(crate) fn install(state: &Rc<AppState>, actions: &ActionSet) {
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::SaveSearch, move || {
        if let Some(state) = weak.upgrade() {
            save(&state);
        }
    });
    actions.set_applies(ActionId::SaveSearch, false);
    if let Some(list) = state.list() {
        let weak = Rc::downgrade(state);
        list.connect_search_changed(move |_| {
            if let Some(state) = weak.upgrade() {
                refresh(&state);
            }
        });
    }
}

/// Save Search… applies while a search runs.
fn refresh(state: &AppState) {
    let searching = state
        .list()
        .is_some_and(|list| !list.query().trim().is_empty());
    state.set_window_applies(ActionId::SaveSearch, searching);
}

/// The sidebar's rows run, rename and remove the saved searches; it shows them now.
pub(crate) fn connect(state: &Rc<AppState>, window: &Window) {
    let (run_state, rename_state, remove_state) = (
        Rc::downgrade(state),
        Rc::downgrade(state),
        Rc::downgrade(state),
    );
    window.sidebar().connect_searches(SearchCommands {
        run: Rc::new(move |index| {
            if let Some(state) = run_state.upgrade() {
                run(&state, index);
            }
        }),
        rename: Rc::new(move |index| {
            if let Some(state) = rename_state.upgrade() {
                rename(&state, index);
            }
        }),
        remove: Rc::new(move |index| {
            if let Some(state) = remove_state.upgrade() {
                remove(&state, index);
            }
        }),
    });
    show(state);
}

/// The sidebar shows the saved searches as the layout has them.
pub(crate) fn show(state: &AppState) {
    if let Some(window) = state.window() {
        let names = state
            .saved_searches()
            .into_iter()
            .map(|search| search.name)
            .collect();
        window.sidebar().set_searches(names);
    }
}

/// Where the list's search runs: All Inboxes for every folder, else the folder shown. A folder
/// of an account `accounts.toml` doesn't list (a local one, as in the self-tests) can't be named
/// in the file, so its search runs over every folder.
fn searched_view(state: &AppState) -> Option<View> {
    let list = state.list()?;
    match list.scope()? {
        ViewScope::UnifiedInbox => Some(View::AllInboxes),
        ViewScope::Folder(_) if list.is_everywhere() => Some(View::AllInboxes),
        ViewScope::Folder(folder) => Some(
            state
                .mailbox()
                .view_of_key(&folder_key(folder))
                .unwrap_or(View::AllInboxes),
        ),
    }
}

fn save(state: &Rc<AppState>) {
    let (Some(window), Some(list)) = (state.window(), state.list()) else {
        return;
    };
    let query = list.query().trim().to_owned();
    let Some(view) = searched_view(state).filter(|_| !query.is_empty()) else {
        return;
    };
    let weak = Rc::downgrade(state);
    let current = query.clone();
    ask_name(
        &window,
        "Save Search",
        "Save",
        &current,
        "save-search",
        move |name| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            let search = SavedSearch {
                name,
                query: query.chars().take(SEARCH_TEXT_MAX).collect(),
                view: view.clone(),
            };
            let mut saved = true;
            state.change_searches(|layout| saved = layout.save_search(search));
            if !saved {
                state.show_toast(
                    &adw::Toast::new(&format!(
                        "Sealwax keeps up to {SEARCHES_MAX} searches; remove one first"
                    )),
                    false,
                );
            }
        },
    );
}

fn run(state: &Rc<AppState>, index: usize) {
    let Some(search) = state.saved_searches().into_iter().nth(index) else {
        return;
    };
    let mailbox = state.mailbox();
    let key = mailbox::view_key(&search.view, &mailbox.accounts(), &mailbox.folders());
    let (Some(key), Some(list)) = (key, state.list()) else {
        state.show_toast(
            &adw::Toast::new("This search's folder is gone; remove the search or save it again"),
            false,
        );
        return;
    };
    crate::go_to::show(state, &key);
    list.set_everywhere(false);
    list.set_query(search.query);
}

fn rename(state: &Rc<AppState>, index: usize) {
    let (Some(window), Some(search)) = (
        state.window(),
        state.saved_searches().into_iter().nth(index),
    ) else {
        return;
    };
    let weak = Rc::downgrade(state);
    ask_name(
        &window,
        &format!("Rename “{}”", search.name),
        "Rename",
        &search.name,
        "rename-search",
        move |name| {
            if let Some(state) = weak.upgrade() {
                state.change_searches(|layout| {
                    if let Some(search) = layout.searches.get_mut(index) {
                        search.name = name;
                    }
                });
            }
        },
    );
}

fn remove(state: &Rc<AppState>, index: usize) {
    state.change_searches(|layout| {
        if index < layout.searches.len() {
            layout.searches.remove(index);
        }
    });
}

/// Asks for a saved search's name, `current` filled in; `done` gets the name typed, trimmed
/// and cut to [`SEARCH_TEXT_MAX`] characters. Save stays off while the name is empty.
fn ask_name(
    window: &Window,
    heading: &str,
    label: &str,
    current: &str,
    name: &str,
    done: impl Fn(String) + 'static,
) {
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .close_response("cancel")
        .default_response("save")
        .build();
    dialog.set_widget_name(name);
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("save", label);
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.add_css_class("sw-folder-dialog");
    let caption = gtk::Label::builder()
        .label("Name")
        .xalign(0.0)
        .css_classes(["sw-setup-label"])
        .build();
    let entry = gtk::Entry::builder()
        .text(current)
        .hexpand(true)
        .activates_default(true)
        .max_length(i32::try_from(SEARCH_TEXT_MAX).unwrap_or(i32::MAX))
        .css_classes(["sw-setup-entry"])
        .build();
    entry.set_widget_name("search-name");
    entry.update_property(&[gtk::accessible::Property::Label("Name")]);
    caption.set_mnemonic_widget(Some(&entry));
    content.append(&caption);
    content.append(&entry);
    dialog.set_extra_child(Some(&content));
    dialog.set_property("focus-widget", &entry);
    dialog.set_response_enabled("save", !current.trim().is_empty());
    entry.connect_changed(glib::clone!(
        #[weak]
        dialog,
        move |entry| dialog.set_response_enabled("save", !entry.text().trim().is_empty())
    ));
    dialog.connect_response(None, move |_, response| {
        let typed: String = entry.text().trim().chars().take(SEARCH_TEXT_MAX).collect();
        if response == "save" && !typed.is_empty() {
            done(typed);
        }
    });
    dialog.present(Some(window.window()));
}
