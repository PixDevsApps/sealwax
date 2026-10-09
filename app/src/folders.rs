//! Folder management in the app (M9, ADR-020): New Folder (New Subfolder on a folder), Rename
//! Folder and Delete Folder, from the palette (on the folder shown), a folder row's menu (on the
//! folder right-clicked) and an account heading's menu (New Folder at the account's top). Each
//! asks in a dialog, checks a typed name before anything is sent and says why one is refused,
//! then sends `Command::ChangeFolder`; a toast reports the engine's answer, a server's refusal
//! in the server's words. An IMAP account's folders change online only: offline, the dialog
//! says so and sends nothing. Delete asks first, with the messages it takes, Cancel the default.

use crate::application::AppState;
use crate::engine_link::{EngineLink, Subscription};
use crate::triage::folder_title;
use crate::window::{AccountStatus, Window};
use gtk4 as gtk;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::account::Receive;
use sealwax_domain::actions::ActionId;
use sealwax_domain::folders::{self, NameProblem, Placement};
use sealwax_domain::model::{AccountId, Folder, FolderId, FolderRole};
use sealwax_engine::api::{Command, Event, FolderChange, FolderError};
use sealwax_engine::imap::hidden;
use sealwax_infrastructure::store::ViewScope;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The commands this module runs.
pub const COMMANDS: [ActionId; 3] = [
    ActionId::NewFolder,
    ActionId::RenameFolder,
    ActionId::DeleteFolder,
];

/// The longest server text a toast shows, in characters.
const SERVER_TEXT: usize = 200;

/// What a folder command works on: one account's folders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    pub account: AccountId,
    /// What Sealwax calls the account.
    pub account_name: String,
    /// The account's folders as the store holds them.
    pub folders: Vec<Folder>,
    /// A POP3 account: its folders are on this computer and change at once.
    pub local: bool,
    /// On Microsoft's servers, whose Notes and Outbox Sealwax hides.
    pub microsoft: bool,
    /// An IMAP account that isn't connected: its folders can't change now.
    pub offline: bool,
}

/// Role folders keep their names and places, Inbox, Drafts, Sent, Archive, Junk, Trash, All.
fn has_role(folder: &Folder) -> bool {
    folder.role != FolderRole::Other || folder.name.eq_ignore_ascii_case("INBOX")
}

impl Context {
    pub fn folder(&self, id: FolderId) -> Option<&Folder> {
        self.folders.iter().find(|folder| folder.id == id)
    }

    /// The folder and every folder under it.
    fn subtree(&self, top: &Folder) -> Vec<&Folder> {
        self.folders
            .iter()
            .filter(|folder| folders::within(&folder.name, &top.name, top.delimiter))
            .collect()
    }

    /// Whether `id` applies to the folder `folder`, or why not.
    pub fn applies(&self, id: ActionId, folder: FolderId) -> Result<(), &'static str> {
        let folder = self.folder(folder).ok_or("The folder is gone")?;
        match id {
            ActionId::NewFolder if !folder.inferiors || folder.delimiter.is_none() => {
                Err("This folder can't have subfolders")
            }
            ActionId::RenameFolder | ActionId::DeleteFolder if has_role(folder) => {
                Err("Special folders keep their names and places")
            }
            ActionId::RenameFolder | ActionId::DeleteFolder
                if self.subtree(folder).into_iter().any(has_role) =>
            {
                Err("It holds a special folder")
            }
            _ => Ok(()),
        }
    }

    /// The folder new folders at the account's top go under (the Inbox on servers that keep
    /// every folder under it), as the engine works it out.
    fn top_parent(&self) -> Option<&Folder> {
        if self.local {
            return None;
        }
        let names: Vec<(&str, Option<char>)> = self
            .folders
            .iter()
            .map(|folder| (folder.name.as_str(), folder.delimiter))
            .collect();
        folders::top_parent(&names).map(|index| &self.folders[index])
    }

    /// The account's delimiter at its top.
    fn top_delimiter(&self) -> Option<char> {
        if self.local {
            return Some('/');
        }
        self.folders
            .iter()
            .find(|folder| folder.name.eq_ignore_ascii_case("INBOX"))
            .and_then(|inbox| inbox.delimiter)
            .or_else(|| self.folders.iter().find_map(|folder| folder.delimiter))
    }

    /// Where a folder under `parent` (the top when `None`) goes, beside every folder there but
    /// `except`.
    fn placement<'a>(
        &'a self,
        parent: Option<&'a Folder>,
        except: Option<FolderId>,
    ) -> Placement<'a> {
        let parent = parent.or_else(|| self.top_parent());
        let delimiter = match parent {
            Some(parent) => parent.delimiter,
            None => self.top_delimiter(),
        };
        let parent = parent.map(|parent| parent.name.as_str());
        Placement {
            delimiter,
            parent,
            siblings: self
                .folders
                .iter()
                .filter(|folder| Some(folder.id) != except)
                .filter(|folder| folders::parent(&folder.name, delimiter) == parent)
                .map(|folder| folders::leaf(&folder.name, delimiter))
                .collect(),
        }
    }

    /// Where `folder` is, beside every folder there but itself.
    fn placement_of<'a>(&'a self, folder: &'a Folder) -> Placement<'a> {
        let delimiter = folder.delimiter;
        let parent = folders::parent(&folder.name, delimiter);
        Placement {
            delimiter,
            parent,
            siblings: self
                .folders
                .iter()
                .filter(|known| known.id != folder.id)
                .filter(|known| folders::parent(&known.name, delimiter) == parent)
                .map(|known| folders::leaf(&known.name, delimiter))
                .collect(),
        }
    }

    /// Checks `typed` as a folder's last level at `placement`.
    fn check(&self, typed: &str, placement: &Placement<'_>) -> Result<String, NameProblem> {
        let delimiter = placement.delimiter;
        folders::check_name(typed, placement, |name| {
            !self.local
                && hidden::hidden(
                    &hidden::Entry {
                        name,
                        delimiter,
                        selectable: true,
                        messages: Some(0),
                    },
                    self.microsoft,
                )
        })
    }
}

/// The context of `account` from the app's store and `accounts.toml`.
pub fn context(state: &AppState, account: AccountId) -> Option<Context> {
    let mailbox = state.mailbox();
    let known = mailbox
        .accounts()
        .into_iter()
        .find(|known| known.id == account)?;
    let listed = state
        .preferences()
        .map(|prefs| prefs.accounts())
        .unwrap_or_default();
    let settings = known
        .key
        .as_ref()
        .and_then(|key| listed.into_iter().find(|settings| &settings.key == key));
    let local = settings
        .as_ref()
        .is_some_and(|settings| matches!(settings.receive, Receive::Pop3(_)));
    let microsoft = !local
        && settings
            .as_ref()
            .is_some_and(|settings| hidden::microsoft_host(&settings.incoming.host));
    let offline = !local
        && matches!(
            mailbox.status(account),
            AccountStatus::Offline | AccountStatus::NeedsSignIn | AccountStatus::CertificateChanged
        );
    Some(Context {
        account,
        account_name: known.name,
        folders: mailbox
            .folders()
            .into_iter()
            .filter(|folder| folder.account == account)
            .collect(),
        local,
        microsoft,
        offline,
    })
}

/// The folder a sidebar key names.
fn folder_of_key(state: &AppState, key: &str) -> Option<Folder> {
    match state.mailbox().scope_for_key(key)? {
        ViewScope::Folder(folder) => state.mailbox().folder(folder),
        ViewScope::UnifiedInbox => None,
    }
}

/// Whether `id` applies to the folder `key` names, or why not.
pub fn applies(state: &AppState, id: ActionId, key: &str) -> Result<(), &'static str> {
    let folder = folder_of_key(state, key).ok_or("Not a folder")?;
    context(state, folder.account)
        .ok_or("The account is gone")?
        .applies(id, folder.id)
}

/// Runs `id` on the folder `key` names.
pub fn run(state: &Rc<AppState>, id: ActionId, key: &str) {
    let (Some(window), Some(folder)) = (state.window(), folder_of_key(state, key)) else {
        return;
    };
    let Some(context) = context(state, folder.account) else {
        return;
    };
    if context.applies(id, folder.id).is_err() {
        return;
    }
    let engine = state.engine();
    match id {
        ActionId::NewFolder => ask_name(&window, &engine, context, Ask::New(Some(folder.id))),
        ActionId::RenameFolder => ask_name(&window, &engine, context, Ask::Rename(folder.id)),
        ActionId::DeleteFolder => confirm_delete(&window, &engine, context, folder.id),
        _ => {}
    }
}

/// New Folder at the top of the account `account`.
pub fn new_at_top(state: &Rc<AppState>, account: AccountId) {
    let (Some(window), Some(context)) = (state.window(), context(state, account)) else {
        return;
    };
    ask_name(&window, &state.engine(), context, Ask::New(None));
}

/// What the name dialog asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ask {
    /// A new folder under a folder, or at the top.
    New(Option<FolderId>),
    Rename(FolderId),
}

/// A folder's name as dialogs and toasts show it: its role's name, else its last level.
fn shown_name(folder: &Folder) -> String {
    match folder.role {
        FolderRole::Other | FolderRole::All => {
            folders::leaf(&folder.name, folder.delimiter).to_owned()
        }
        _ => folder_title(folder),
    }
}

fn offline_text(context: &Context) -> String {
    format!(
        "{} is offline. Its folders can change once it is connected again.",
        context.account_name
    )
}

/// The name dialog: New Folder, New Subfolder or Rename Folder.
fn ask_name(window: &Window, engine: &EngineLink, context: Context, ask: Ask) {
    let (heading, response, label, current, name) = match ask {
        Ask::New(None) => (
            format!("New Folder in {}", context.account_name),
            "create",
            "Create",
            None,
            "new-folder",
        ),
        Ask::New(Some(parent)) => {
            let Some(parent) = context.folder(parent) else {
                return;
            };
            (
                format!("New Subfolder in “{}”", shown_name(parent)),
                "create",
                "Create",
                None,
                "new-folder",
            )
        }
        Ask::Rename(folder) => {
            let Some(folder) = context.folder(folder) else {
                return;
            };
            (
                format!("Rename “{}”", shown_name(folder)),
                "rename",
                "Rename",
                Some(shown_name(folder)),
                "rename-folder",
            )
        }
    };
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .close_response("cancel")
        .default_response(response)
        .build();
    dialog.set_widget_name(name);
    if context.offline {
        dialog.set_body(&offline_text(&context));
    }
    dialog.add_response("cancel", "Cancel");
    dialog.add_response(response, label);
    dialog.set_response_appearance(response, adw::ResponseAppearance::Suggested);
    dialog.set_response_enabled(response, false);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.add_css_class("sw-folder-dialog");
    let caption = gtk::Label::builder()
        .label("Folder name")
        .xalign(0.0)
        .css_classes(["sw-setup-label"])
        .build();
    let entry = gtk::Entry::builder()
        .text(current.as_deref().unwrap_or_default())
        .hexpand(true)
        .activates_default(true)
        .css_classes(["sw-setup-entry"])
        .build();
    entry.set_widget_name("folder-name");
    entry.update_property(&[gtk::accessible::Property::Label("Folder name")]);
    caption.set_mnemonic_widget(Some(&entry));
    let problem = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["sw-setup-validation"])
        .build();
    problem.set_widget_name("folder-name-problem");
    content.append(&caption);
    content.append(&entry);
    content.append(&problem);
    dialog.set_extra_child(Some(&content));
    dialog.set_property("focus-widget", &entry);

    let context = Rc::new(context);
    let touched = Rc::new(Cell::new(false));
    // What the typed name would be sent as, while it can be.
    let checked: Rc<RefCell<Option<String>>> = Rc::default();
    let validate = {
        let (context, dialog, problem, touched, checked) = (
            context.clone(),
            dialog.downgrade(),
            problem.clone(),
            touched.clone(),
            checked.clone(),
        );
        move |entry: &gtk::Entry| {
            let Some(dialog) = dialog.upgrade() else {
                return;
            };
            let typed = entry.text();
            // `Err(None)`: the folder's own name again, nothing to do and nothing to say.
            let result: Result<String, Option<NameProblem>> = match ask {
                Ask::New(parent) => {
                    let parent = parent.and_then(|id| context.folder(id));
                    context
                        .check(&typed, &context.placement(parent, None))
                        .map_err(Some)
                }
                Ask::Rename(folder) => {
                    let Some(folder) = context.folder(folder) else {
                        return;
                    };
                    if typed.trim() == shown_name(folder) {
                        Err(None)
                    } else {
                        context
                            .check(&typed, &context.placement_of(folder))
                            .map_err(Some)
                    }
                }
            };
            let message = match &result {
                Err(Some(problem)) if touched.get() => Some(problem.message()),
                _ => None,
            };
            problem.set_visible(message.is_some());
            problem.set_label(message.as_deref().unwrap_or_default());
            let invalid = message.is_some();
            if invalid {
                entry.add_css_class("sw-setup-invalid");
            } else {
                entry.remove_css_class("sw-setup-invalid");
            }
            entry.update_state(&[gtk::accessible::State::Invalid(if invalid {
                gtk::AccessibleInvalidState::True
            } else {
                gtk::AccessibleInvalidState::False
            })]);
            entry.update_property(&[gtk::accessible::Property::Description(
                message.as_deref().unwrap_or_default(),
            )]);
            let ready = result.ok().filter(|_| !context.offline);
            dialog.set_response_enabled(response, ready.is_some());
            checked.replace(ready);
        }
    };
    validate(&entry);
    let on_change = validate.clone();
    entry.connect_changed(move |entry| {
        touched.set(true);
        on_change(entry);
    });

    let engine = engine.clone();
    let toasts = window.toasts();
    let shown_context = context.clone();
    dialog.connect_response(None, move |_, chosen| {
        if chosen != response {
            return;
        }
        let Some(name) = checked.borrow().clone() else {
            return;
        };
        let context = &shown_context;
        let (change, done, failed) = match ask {
            Ask::New(parent) => (
                FolderChange::Create {
                    parent,
                    name: name.clone(),
                },
                format!("Created “{name}”"),
                format!("Couldn't create “{name}”"),
            ),
            Ask::Rename(folder) => {
                let old = context.folder(folder).map(shown_name).unwrap_or_default();
                (
                    FolderChange::Rename {
                        folder,
                        name: name.clone(),
                    },
                    format!("Renamed “{old}” to “{name}”"),
                    format!("Couldn't rename “{old}”"),
                )
            }
        };
        send(&engine, &toasts, context.account, change, done, failed);
    });
    dialog.present(Some(window.window()));
}

/// The delete dialog: what goes, and that it goes for good.
fn confirm_delete(window: &Window, engine: &EngineLink, context: Context, folder: FolderId) {
    let Some(top) = context.folder(folder) else {
        return;
    };
    let name = shown_name(top);
    let subtree = context.subtree(top);
    let subfolders = subtree.len().saturating_sub(1);
    let messages: u32 = subtree.iter().map(|folder| folder.total).sum();
    let dialog = adw::AlertDialog::builder()
        .heading(format!("Delete “{name}” and everything in it?"))
        .body(if context.offline {
            offline_text(&context)
        } else {
            delete_text(&name, subfolders, messages, context.local)
        })
        .close_response("cancel")
        .default_response("cancel")
        .build();
    dialog.set_widget_name("delete-folder");
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("delete", "Delete");
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_response_enabled("delete", !context.offline);
    let engine = engine.clone();
    let toasts = window.toasts();
    let account = context.account;
    let offline = context.offline;
    dialog.connect_response(None, move |_, chosen| {
        if chosen != "delete" || offline {
            return;
        }
        send(
            &engine,
            &toasts,
            account,
            FolderChange::Delete { folder },
            format!("Deleted “{name}”"),
            format!("Couldn't delete “{name}”"),
        );
    });
    dialog.present(Some(window.window()));
}

/// What a delete takes: "The server deletes “Kunder”, its 2 subfolders and the 14 messages in
/// them for good."
fn delete_text(name: &str, subfolders: usize, messages: u32, local: bool) -> String {
    let folders = match subfolders {
        0 => format!("“{name}”"),
        1 => format!("“{name}”, its subfolder"),
        count => format!("“{name}”, its {count} subfolders"),
    };
    let held = match (messages, subfolders) {
        (0, 0) => " It holds no messages.".to_owned(),
        (0, _) => " They hold no messages.".to_owned(),
        (1, 0) => " with the message in it".to_owned(),
        (1, _) => " with the message in them".to_owned(),
        (count, 0) => format!(" with the {count} messages in it"),
        (count, _) => format!(" with the {count} messages in them"),
    };
    let (who, place) = if local {
        ("Sealwax deletes", " from this computer")
    } else {
        ("The server deletes", "")
    };
    if messages == 0 {
        format!("{who} {folders}{place} for good.{held}")
    } else {
        format!("{who} {folders}{held}{place} for good.")
    }
}

/// Why a change didn't happen, for its toast.
fn why(error: &FolderError) -> String {
    match error {
        FolderError::Offline => "the account is offline".to_owned(),
        FolderError::Name(problem) => {
            let message = problem.message();
            let message = message.trim_end_matches('.');
            let mut chars = message.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_lowercase().chain(chars).collect()
            })
        }
        FolderError::NotAllowed => "this folder can't change that way".to_owned(),
        FolderError::Missing => "the folder is gone".to_owned(),
        FolderError::Refused(text) => {
            let text: String = text
                .as_str()
                .chars()
                .filter(|ch| !ch.is_control())
                .take(SERVER_TEXT)
                .collect();
            format!("the server said “{}”", text.trim())
        }
        FolderError::Lost => {
            "the connection was lost; Sealwax reads the folders again when it reconnects".to_owned()
        }
        FolderError::Store => "the local store failed".to_owned(),
    }
}

fn toast(toasts: &adw::ToastOverlay, text: &str) {
    toasts.add_toast(adw::Toast::builder().title(text).use_markup(false).build());
}

/// Sends `change` and toasts the answer: `done`, or `failed` with why.
fn send(
    engine: &EngineLink,
    toasts: &adw::ToastOverlay,
    account: AccountId,
    change: FolderChange,
    done: String,
    failed: String,
) {
    let request = engine.next_request();
    let command = Command::ChangeFolder {
        request,
        account,
        change,
    };
    if engine.send(command).is_err() {
        toast(toasts, "Not connected to the mail engine");
        return;
    }
    // The subscription lives until the engine answers this request.
    let held: Rc<RefCell<Option<Subscription>>> = Rc::default();
    let toasts = toasts.clone();
    let release = held.clone();
    let subscription = engine.subscribe(move |event| {
        let Event::FolderChanged {
            request: answered,
            result,
            ..
        } = event
        else {
            return;
        };
        if *answered != request {
            return;
        }
        let text = match result {
            Ok(()) => done.clone(),
            Err(error) => format!("{failed}: {}", why(error)),
        };
        toast(&toasts, &text);
        release.take();
    });
    held.replace(Some(subscription));
}

/// The folder the list shows, as the palette's commands take it.
fn shown_folder(state: &AppState) -> Option<String> {
    let window = state.window()?;
    let key = window.selected_folder();
    folder_of_key(state, &key).map(|_| key)
}

/// Connects the three commands on the main window's action set (on the folder shown) and in
/// the sidebar's menus.
pub fn install(state: &Rc<AppState>, window: &Window, actions: &crate::actions::ActionSet) {
    for id in COMMANDS {
        let weak = Rc::downgrade(state);
        actions.connect(id, move || {
            if let Some(state) = weak.upgrade()
                && let Some(key) = shown_folder(&state)
            {
                run(&state, id, &key);
            }
        });
    }
    let (applies_to, activate) = (Rc::downgrade(state), Rc::downgrade(state));
    window.sidebar().connect_folder_commands(
        move |id, key| match applies_to.upgrade() {
            Some(state) => applies(&state, id, key),
            None => Err("Sealwax is closing"),
        },
        move |id, key| {
            if let Some(state) = activate.upgrade() {
                run(&state, id, key);
            }
        },
    );
    let weak = Rc::downgrade(state);
    window.sidebar().connect_new_folder(move |key| {
        if let (Some(state), Some(account)) = (weak.upgrade(), crate::mailbox::account_for_key(key))
        {
            new_at_top(&state, account);
        }
    });
}

/// Whether each command applies to the folder shown.
pub fn refresh(state: &AppState, actions: &crate::actions::ActionSet) {
    let key = shown_folder(state);
    for id in COMMANDS {
        let applies = key
            .as_deref()
            .is_some_and(|key| applies(state, id, key).is_ok());
        actions.set_applies(id, applies);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: i64, name: &str, role: FolderRole) -> Folder {
        Folder {
            id: FolderId(id),
            account: AccountId(1),
            name: name.to_owned(),
            delimiter: Some('/'),
            role,
            selectable: true,
            inferiors: true,
            total: 3,
            unread: 1,
        }
    }

    fn context(local: bool) -> Context {
        Context {
            account: AccountId(1),
            account_name: "Arbeid".into(),
            folders: vec![
                folder(1, "INBOX", FolderRole::Inbox),
                folder(2, "Sent", FolderRole::Sent),
                folder(3, "Kunder", FolderRole::Other),
                folder(4, "Kunder/Acme", FolderRole::Other),
                Folder {
                    inferiors: false,
                    ..folder(5, "Arkiv 2019", FolderRole::Other)
                },
                folder(6, "Prosjekter", FolderRole::Other),
                folder(7, "Prosjekter/Arkiv", FolderRole::Archive),
                folder(8, "Sync Issues", FolderRole::Other),
            ],
            local,
            microsoft: false,
            offline: false,
        }
    }

    #[test]
    fn what_each_command_applies_to() {
        let context = context(false);
        assert_eq!(context.applies(ActionId::NewFolder, FolderId(1)), Ok(()));
        assert_eq!(
            context.applies(ActionId::NewFolder, FolderId(5)),
            Err("This folder can't have subfolders")
        );
        for id in [ActionId::RenameFolder, ActionId::DeleteFolder] {
            assert_eq!(
                context.applies(id, FolderId(1)),
                Err("Special folders keep their names and places")
            );
            assert_eq!(
                context.applies(id, FolderId(6)),
                Err("It holds a special folder")
            );
            assert_eq!(context.applies(id, FolderId(3)), Ok(()));
            assert_eq!(context.applies(id, FolderId(4)), Ok(()));
        }
    }

    #[test]
    fn names_are_checked_where_they_go() {
        let context = context(false);
        let kunder = context.folder(FolderId(3));
        assert_eq!(
            context.check("acme", &context.placement(kunder, None)),
            Err(NameProblem::Taken)
        );
        assert_eq!(
            context.check("Fjord", &context.placement(kunder, None)),
            Ok("Fjord".into())
        );
        assert_eq!(
            context.check("kunder", &context.placement(None, None)),
            Err(NameProblem::Taken)
        );
        assert_eq!(
            context.check("Synkroniseringsfeil", &context.placement(None, None)),
            Err(NameProblem::Hidden),
            "a name Sealwax would hide at the top"
        );
        assert_eq!(
            context.check("Synkroniseringsfeil", &context.placement(kunder, None)),
            Ok("Synkroniseringsfeil".into())
        );
        let local = super::tests::context(true);
        assert_eq!(
            local.check("Synkroniseringsfeil", &local.placement(None, None)),
            Ok("Synkroniseringsfeil".into()),
            "nothing hides a local folder"
        );
    }

    #[test]
    fn deletes_say_what_goes() {
        assert_eq!(
            delete_text("Kunder", 2, 14, false),
            "The server deletes “Kunder”, its 2 subfolders with the 14 messages in them for good."
        );
        assert_eq!(
            delete_text("Acme", 0, 1, false),
            "The server deletes “Acme” with the message in it for good."
        );
        assert_eq!(
            delete_text("Acme", 0, 0, true),
            "Sealwax deletes “Acme” from this computer for good. It holds no messages."
        );
        assert_eq!(
            delete_text("Kunder", 1, 0, false),
            "The server deletes “Kunder”, its subfolder for good. They hold no messages."
        );
    }

    #[test]
    fn refusals_toast_the_servers_words_and_the_names_problem() {
        assert_eq!(
            why(&FolderError::Name(NameProblem::Taken)),
            "there is a folder with this name here already"
        );
        assert_eq!(why(&FolderError::Offline), "the account is offline");
    }
}
