//! Plugins in the window (ADR-022): their actions in the message menu and the palette, run on
//! the selected messages, and their hooks, run when new mail reaches an Inbox. The plugins
//! are read from `plugins/` at start and again whenever that folder changes. A plugin runs off
//! the GTK thread; its notice, or why it failed, is a toast. Logs name the plugin and counts,
//! never the mail or what the plugin wrote (ADR-018).

use crate::application::AppState;
use crate::engine_link::Subscription;
use crate::monitor::DirWatch;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use sealwax_domain::model::{FolderId, FolderRole, MessageId};
use sealwax_domain::plugins::{AccountInfo, HookEvent, MessageInfo, Part, Trigger, payload};
use sealwax_engine::api::Event;
use sealwax_infrastructure::plugins::{
    ACTION_TIMEOUT, HOOK_TIMEOUT, Plugin, Problem, discover, run,
};
use sealwax_infrastructure::store::Store;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;

/// The window action that runs a plugin's command; its target is `plugin/action`.
pub const ACTION: &str = "plugin-action";
/// The most messages one action is sent.
const MAX_MESSAGES: usize = 50;
/// The palette's category for the plugins' commands.
const CATEGORY: &str = "Plugins";

#[derive(Default)]
struct Plugins {
    loaded: Vec<Plugin>,
    problems: Vec<Problem>,
    watch: Option<Rc<DirWatch>>,
    /// Each plugin folder's monitor: a manifest or script changed inside one reloads too.
    folders: Vec<gio::FileMonitor>,
    /// A reload waiting out the burst of changes an install makes.
    pending: Option<glib::SourceId>,
    events: Option<Subscription>,
    /// Hooks waiting their turn: they run one at a time.
    hooks: Option<async_channel::Sender<(Plugin, String)>>,
}

thread_local! {
    static PLUGINS: RefCell<Plugins> = RefCell::default();
}

/// A plugin's command, as the message menu and the palette offer it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub title: String,
    /// `plugin/action`.
    pub target: String,
    /// The detailed action name that runs it.
    pub action: String,
}

/// Every enabled plugin's actions, in plugin order.
pub fn commands(_state: &AppState) -> Vec<Command> {
    PLUGINS.with(|plugins| {
        plugins
            .borrow()
            .loaded
            .iter()
            .flat_map(|plugin| {
                plugin.manifest.actions.iter().map(move |action| {
                    let target = format!("{}/{}", plugin.manifest.id, action.id);
                    Command {
                        title: action.title.clone(),
                        action: format!("win.{ACTION}::{target}"),
                        target,
                    }
                })
            })
            .collect()
    })
}

/// The plugins' commands for the palette, while messages are selected.
pub fn palette_extras(state: &AppState) -> Vec<crate::palette::Extra> {
    let selected = state
        .list()
        .and_then(|list| list.selected_targets().ok())
        .is_some_and(|targets| !targets.is_empty());
    if !selected {
        return Vec::new();
    }
    commands(state)
        .into_iter()
        .map(|command| crate::palette::Extra {
            title: command.title,
            category: CATEGORY,
            action: command.action,
        })
        .collect()
}

/// Why plugin folders can't be used, for Preferences and a toast at start.
pub fn problems() -> Vec<String> {
    PLUGINS.with(|plugins| {
        plugins
            .borrow()
            .problems
            .iter()
            .map(|problem| format!("Plugin {}: {}", problem.id, problem.message))
            .collect()
    })
}

/// Reads the plugins, watches their folder and runs hooks on new mail. Called once the engine
/// link exists, with or without a window.
pub fn install(state: &Rc<AppState>) {
    let dir = state.config().dirs.plugins_dir();
    // Sealwax's own folder, made private so it can be watched and shows where plugins go.
    if let Err(error) = sealwax_infrastructure::xdg::ensure_private_dir(&dir) {
        tracing::warn!(%error, "the plugins folder can't be made");
    }
    let weak = Rc::downgrade(state);
    let reload = move || {
        if let Some(state) = weak.upgrade() {
            reload_soon(&state);
        }
    };
    let watch = DirWatch::start(dir, Box::new(|_, _, _| true), reload);

    let weak = Rc::downgrade(state);
    let events = state.engine().subscribe(move |event| {
        if let Event::NewMail {
            folder, messages, ..
        } = event
            && let Some(state) = weak.upgrade()
        {
            arrived(&state, *folder, messages);
        }
    });

    let (sender, receiver) = async_channel::unbounded::<(Plugin, String)>();
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        while let Ok((plugin, input)) = receiver.recv().await {
            let id = plugin.manifest.id.clone();
            let name = plugin.manifest.name.clone();
            let result = gio::spawn_blocking(move || run(&plugin, &input, HOOK_TIMEOUT)).await;
            if let Ok(Err(error)) = result {
                tracing::warn!(plugin = %id, "a plugin's new-mail hook failed");
                if let Some(state) = weak.upgrade() {
                    toast(&state, &format!("{name} (new mail): {error}"));
                }
            }
        }
    });

    PLUGINS.with(|plugins| {
        let mut plugins = plugins.borrow_mut();
        plugins.watch = Some(watch);
        plugins.events = Some(events);
        plugins.hooks = Some(sender);
    });
    load(state);
}

/// Reloads once changes have settled for 150 ms, as the settings files do.
fn reload_soon(state: &Rc<AppState>) {
    let weak = Rc::downgrade(state);
    let source = glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
        PLUGINS.with(|plugins| plugins.borrow_mut().pending = None);
        if let Some(state) = weak.upgrade() {
            load(&state);
        }
    });
    if let Some(earlier) = PLUGINS.with(|plugins| plugins.borrow_mut().pending.replace(source)) {
        earlier.remove();
    }
}

/// Monitors for each folder in `dir`.
fn watch_folders(state: &Rc<AppState>, dir: &std::path::Path) -> Vec<gio::FileMonitor> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            let monitor = gio::File::for_path(entry.path())
                .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
                .ok()?;
            let weak = Rc::downgrade(state);
            monitor.connect_changed(move |_, _, _, _| {
                if let Some(state) = weak.upgrade() {
                    reload_soon(&state);
                }
            });
            Some(monitor)
        })
        .collect()
}

fn load(state: &Rc<AppState>) {
    let dir = state.config().dirs.plugins_dir();
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let folders = dir.clone();
        let Ok((loaded, problems)) = gio::spawn_blocking(move || discover(&dir)).await else {
            return;
        };
        if let Some(state) = weak.upgrade() {
            let monitors = watch_folders(&state, &folders);
            PLUGINS.with(|plugins| plugins.borrow_mut().folders = monitors);
        }
        tracing::info!(
            plugins = loaded.len(),
            problems = problems.len(),
            "plugins read"
        );
        let shown: Vec<String> = problems
            .iter()
            .map(|p| format!("Plugin {}: {}", p.id, p.message))
            .collect();
        let new_problems = PLUGINS.with(|plugins| {
            let mut plugins = plugins.borrow_mut();
            let changed = plugins.problems != problems;
            plugins.loaded = loaded;
            plugins.problems = problems;
            changed
        });
        if new_problems && let (Some(state), Some(first)) = (weak.upgrade(), shown.first()) {
            toast(&state, first);
        }
    });
}

/// Runs a detailed action name such as `win.plugin-action::signet/create-event` on `widget`:
/// GTK's `activate_action` takes the target apart from the name.
pub fn activate(widget: &impl IsA<gtk4::Widget>, detailed: &str) -> Result<(), glib::BoolError> {
    let (name, target) = gio::Action::parse_detailed_name(detailed)
        .map_err(|_| glib::bool_error!("not a detailed action name"))?;
    WidgetExt::activate_action(widget, &name, target.as_ref())
}

fn toast(state: &AppState, text: &str) {
    let toast = adw::Toast::builder()
        .title(text)
        .use_markup(false)
        .timeout(6)
        .build();
    state.show_toast(&toast, false);
}

/// Installs `win.plugin-action` on the window.
pub fn install_window_action(state: &Rc<AppState>, window: &impl IsA<gio::ActionMap>) {
    let action = gio::SimpleAction::new(ACTION, Some(glib::VariantTy::STRING));
    let weak = Rc::downgrade(state);
    action.connect_activate(move |_, target| {
        let (Some(state), Some(target)) = (weak.upgrade(), target.and_then(|t| t.get::<String>()))
        else {
            return;
        };
        run_action(&state, &target);
    });
    window.add_action(&action);
}

/// The account a folder belongs to, as a plugin is told it.
fn account_of(state: &AppState, folder: FolderId) -> Option<AccountInfo> {
    let mailbox = state.mailbox();
    let folder = mailbox.folder(folder)?;
    let account = mailbox
        .accounts()
        .into_iter()
        .find(|account| account.id == folder.account)?;
    Some(AccountInfo {
        key: account.key.map(|key| key.0).unwrap_or_default(),
        address: account.address,
        label: account.name,
    })
}

/// Runs a plugin's action on the selected messages.
fn run_action(state: &Rc<AppState>, target: &str) {
    let Some((plugin_id, action_id)) = target.split_once('/') else {
        return;
    };
    let found = PLUGINS.with(|plugins| {
        plugins
            .borrow()
            .loaded
            .iter()
            .find(|plugin| plugin.manifest.id == plugin_id)
            .and_then(|plugin| Some((plugin.clone(), plugin.manifest.action(action_id)?.clone())))
    });
    let Some((plugin, action)) = found else {
        toast(state, "That plugin is no longer installed");
        return;
    };
    let targets = match state.list().map(|list| list.selected_targets()) {
        Some(Ok(targets)) if !targets.is_empty() => targets,
        Some(Err(_)) => return toast(state, "Some selected messages haven't loaded"),
        _ => return toast(state, "Select a message"),
    };
    if targets.len() > MAX_MESSAGES {
        return toast(
            state,
            &format!(
                "{} takes at most {MAX_MESSAGES} messages at a time",
                action.title
            ),
        );
    }
    let accounts: BTreeSet<_> = targets
        .iter()
        .filter_map(|target| {
            state
                .mailbox()
                .folder(target.folder)
                .map(|folder| folder.account)
        })
        .collect();
    if accounts.len() != 1 {
        return toast(state, "The messages are in more than one account");
    }
    let Some(account) = account_of(state, targets[0].folder) else {
        return toast(state, "The message's account is gone");
    };
    let Some(store) = state.mailbox().shared_store() else {
        return;
    };
    let engine = state.engine();
    let folders: Vec<String> = targets
        .iter()
        .map(|target| {
            state
                .mailbox()
                .folder(target.folder)
                .map(|folder| folder.name)
                .unwrap_or_default()
        })
        .collect();
    let messages: Vec<MessageId> = targets.iter().map(|target| target.id).collect();
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let mut infos = Vec::with_capacity(messages.len());
        for (message, folder) in messages.into_iter().zip(folders) {
            // The parts asked for need the message as the server has it, downloaded if need be;
            // headers alone come from the store.
            let info = if action.receives.is_empty() {
                headers_only(&store, message, &folder).await
            } else {
                match crate::composer::store_backend::original(&store, &engine, message).await {
                    Ok(raw) => {
                        let receives = action.receives.clone();
                        gio::spawn_blocking(move || MessageInfo::from_raw(&folder, &raw, &receives))
                            .await
                            .ok()
                    }
                    Err(error) => {
                        if let Some(state) = weak.upgrade() {
                            toast(&state, &error);
                        }
                        return;
                    }
                }
            };
            match info {
                Some(info) => infos.push(info),
                None => {
                    if let Some(state) = weak.upgrade() {
                        toast(&state, "The message couldn't be read.");
                    }
                    return;
                }
            }
        }
        let input = payload(
            &plugin.manifest.id,
            &Trigger::Action(action.id.clone()),
            &account,
            &infos,
        );
        let name = plugin.manifest.name.clone();
        let id = plugin.manifest.id.clone();
        let result = gio::spawn_blocking(move || run(&plugin, &input, ACTION_TIMEOUT)).await;
        let Some(state) = weak.upgrade() else { return };
        match result {
            Ok(Ok(reply)) => {
                tracing::info!(plugin = %id, messages = infos.len(), "a plugin's action ran");
                if let Some(notice) = reply.notice {
                    toast(&state, &notice);
                }
            }
            Ok(Err(error)) => {
                tracing::warn!(plugin = %id, "a plugin's action failed");
                toast(&state, &format!("{name}: {error}"));
            }
            Err(_) => toast(&state, &format!("{name} couldn't be run")),
        }
    });
}

/// A message's headers from the store, with no download.
async fn headers_only(store: &Store, message: MessageId, folder: &str) -> Option<MessageInfo> {
    let detail = store.message(message).recv().await.ok()?.value?;
    Some(MessageInfo {
        folder: folder.to_owned(),
        headers: detail.headers,
        text: None,
        raw: None,
    })
}

/// New mail in an Inbox: each plugin with a new-mail hook is sent the new messages, with only
/// what is already on this computer (a hook never makes Sealwax download).
fn arrived(state: &Rc<AppState>, folder: FolderId, messages: &[MessageId]) {
    let hooked: Vec<(Plugin, BTreeSet<Part>)> = PLUGINS.with(|plugins| {
        plugins
            .borrow()
            .loaded
            .iter()
            .flat_map(|plugin| {
                plugin
                    .manifest
                    .hooks
                    .iter()
                    .filter(|hook| hook.event == HookEvent::NewMail)
                    .map(move |hook| (plugin.clone(), hook.receives.clone()))
            })
            .collect()
    });
    if hooked.is_empty() || messages.is_empty() {
        return;
    }
    let Some(found) = state.mailbox().folder(folder) else {
        return;
    };
    if found.role != FolderRole::Inbox {
        return;
    }
    let (Some(account), Some(store)) = (account_of(state, folder), state.mailbox().shared_store())
    else {
        return;
    };
    let Some(queue) = PLUGINS.with(|plugins| plugins.borrow().hooks.clone()) else {
        return;
    };
    let messages = messages.to_vec();
    let folder_name = found.name;
    glib::spawn_future_local(async move {
        for (plugin, receives) in hooked {
            let infos = hook_messages(&store, &messages, &folder_name, &receives).await;
            if infos.is_empty() {
                continue;
            }
            let input = payload(
                &plugin.manifest.id,
                &Trigger::Hook(HookEvent::NewMail),
                &account,
                &infos,
            );
            let _ = queue.send((plugin, input)).await;
        }
    });
}

async fn hook_messages(
    store: &Arc<Store>,
    messages: &[MessageId],
    folder: &str,
    receives: &BTreeSet<Part>,
) -> Vec<MessageInfo> {
    let mut infos = Vec::with_capacity(messages.len());
    for message in messages {
        let Some(mut info) = headers_only(store, *message, folder).await else {
            continue;
        };
        if !receives.is_empty()
            && let Ok(Some(raw)) = store.raw_message(*message).recv().await
        {
            let (folder, receives) = (folder.to_owned(), receives.clone());
            if let Ok(full) =
                gio::spawn_blocking(move || MessageInfo::from_raw(&folder, &raw, &receives)).await
            {
                info = full;
            }
        }
        infos.push(info);
    }
    infos
}
