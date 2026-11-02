//! Self-test commands for folder management and the sidebar's folder tree (M9, ADR-020). A
//! folder is named `ACCOUNT:NAME` by its account's name and its whole name (`Work:Kunder/Acme`),
//! or `ACCOUNT:ROLE` (`Work:inbox`), as `folder` and `sidebar-menu` take it. Every name is
//! fictional.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `folder-add ACCOUNT NAME [noselect] [noinferiors]` | A folder of the account named ACCOUNT in the app's store, `/` between its levels |
//! | `folder-arrow SPEC` | Clicks the fold arrow of the folder SPEC through Broadway |
//! | `focus-folder SPEC` | Puts the keyboard focus on the folder's row |
//! | `assert-sidebar-tree ACCOUNT ROWS` | The account's folder rows on screen, in order, by the names they show, a `>` before a name per level (`Inbox,>Kvitteringer,Drafts`) |
//! | `assert-folder-level SPEC N` | The folder row's accessible level (1 at the top) |
//! | `assert-folder-expanded SPEC true\|false\|undefined` | The folder row's expanded state: its subfolders shown, folded away, or none |
//! | `assert-folder-count SPEC N [NAME]` | The count the folder row shows (0 for none), and its accessible name |
//! | `assert-folder-stored SPEC BOOL` | Whether the app's store holds the folder |
//! | `assert-response DIALOG RESPONSE enabled\|disabled\|default` | An alert dialog's response: whether it can be chosen, or that it is the default |
//!
//! The fake engine's answer is `setup.rs`'s `engine-reply folder-changed ok|offline|taken|not-allowed|missing|lost|store`
//! or `engine-reply folder-changed refused TEXT`; `ok` first makes the change in the app's store
//! as the engine would ([`apply`]).

use super::{Harness, StepResult, equal, number};
use crate::mailbox::folder_key;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::folders;
use sealwax_domain::model::{AccountId, FolderRole};
use sealwax_engine::api::FolderChange;
use sealwax_infrastructure::store::{FolderRename, FolderSpec};

/// The commands this module adds to the script format, with their argument counts.
pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("folder-add", 2, 4),
    ("folder-arrow", 1, 1),
    ("focus-folder", 1, 1),
    ("assert-sidebar-tree", 2, 2),
    ("assert-folder-level", 2, 2),
    ("assert-folder-expanded", 2, 2),
    ("assert-folder-count", 2, 3),
    ("assert-folder-stored", 2, 2),
    ("assert-response", 3, 3),
];

pub fn handles(command: &str) -> bool {
    COMMANDS.iter().any(|(name, _, _)| *name == command)
}

fn account_named(harness: &Harness, name: &str) -> Result<AccountId, String> {
    harness
        .state
        .mailbox()
        .accounts()
        .into_iter()
        .find(|account| account.name == name)
        .map(|account| account.id)
        .ok_or_else(|| format!("no account named {name}"))
}

/// The widget of the folder SPEC's row.
fn row(harness: &Harness, spec: &str) -> Result<gtk4::Widget, String> {
    let key = super::mail::folder_key_for(harness, spec)?;
    harness.widget(&format!("folder:{key}"))
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    let arg = |index: usize| args[index].as_str();
    match command {
        "folder-add" => {
            let account = account_named(harness, arg(0))?;
            let mut attributes = Vec::new();
            let mut selectable = true;
            for flag in &args[2..] {
                match flag.as_str() {
                    "noselect" => {
                        selectable = false;
                        attributes.push("\\Noselect".to_owned());
                    }
                    "noinferiors" => attributes.push("\\Noinferiors".to_owned()),
                    other => return Err(format!("use noselect or noinferiors, not {other}")),
                }
            }
            let spec = FolderSpec {
                attributes,
                selectable,
                ..FolderSpec::local(account, arg(1), FolderRole::Other)
            };
            let reply = harness
                .state
                .mailbox()
                .store()
                .ok_or("the store is not open")?
                .upsert_folder(spec);
            let id = reply.recv().await.map_err(|error| error.to_string())?.value;
            let key = folder_key(id);
            super::mail::settled(harness).await?;
            let started = std::time::Instant::now();
            while !harness.window()?.sidebar().contains(&key) {
                if started.elapsed() > super::WAIT_TIMEOUT {
                    return Err("the sidebar never showed the folder".to_owned());
                }
                gtk4::glib::timeout_future(std::time::Duration::from_millis(10)).await;
            }
            println!("  # {key}");
            Ok(())
        }
        "folder-arrow" => {
            let key = super::mail::folder_key_for(harness, arg(0))?;
            let arrow = harness.widget(&format!("folder-arrow:{key}"))?;
            super::triage::click_at(harness, &arrow, None).await
        }
        "focus-folder" => {
            row(harness, arg(0))?.grab_focus();
            harness.wait_until_settled().await;
            Ok(())
        }
        _ => check(harness, command, args).await,
    }
}

async fn check(harness: &Harness, assertion: &str, args: &[String]) -> StepResult {
    let arg = |index: usize| args[index].as_str();
    match assertion {
        "assert-sidebar-tree" => {
            let account = account_named(harness, arg(0))?;
            let rows = harness
                .window()?
                .sidebar()
                .shown_tree(&format!("account-{account}"));
            let text: Vec<String> = rows
                .into_iter()
                .map(|(title, level)| format!("{}{title}", ">".repeat(level)))
                .collect();
            equal("the account's rows", text.join(",").as_str(), arg(1))
        }
        "assert-folder-level" => {
            let widget = row(harness, arg(0))?;
            let level = super::a11y::level(&widget).unwrap_or_else(|| "none".to_owned());
            equal(
                &format!("{}'s level", arg(0)),
                level.as_str(),
                number(arg(1))?.to_string().as_str(),
            )
        }
        "assert-folder-expanded" => {
            let widget = row(harness, arg(0))?;
            let state = super::a11y::state_value(&widget, gtk4::AccessibleState::Expanded);
            equal(&format!("{} expanded", arg(0)), state.as_str(), arg(1))
        }
        "assert-folder-count" => {
            let key = super::mail::folder_key_for(harness, arg(0))?;
            let shown = harness
                .widget(&format!("count:{key}"))
                .ok()
                .and_downcast::<gtk4::Label>()
                .map_or_else(|| "0".to_owned(), |label| label.label().to_string());
            equal(
                &format!("{}'s count", arg(0)),
                shown.as_str(),
                number(arg(1))?.to_string().as_str(),
            )?;
            match args.get(2) {
                Some(wanted) => {
                    let name = super::a11y::name(&row(harness, arg(0))?).unwrap_or_default();
                    equal(
                        &format!("{}'s accessible name", arg(0)),
                        name.as_str(),
                        wanted.as_str(),
                    )
                }
                None => Ok(()),
            }
        }
        "assert-folder-stored" => {
            let stored = super::mail::find_folder(harness, arg(0)).is_ok();
            equal(
                &format!("{} stored", arg(0)),
                stored,
                super::script::boolean(arg(1))?,
            )
        }
        "assert-response" => {
            let found = harness.widget(arg(0))?;
            let dialog = found
                .downcast_ref::<adw::AlertDialog>()
                .ok_or_else(|| format!("{} is not an alert dialog", arg(0)))?;
            if !dialog.has_response(arg(1)) {
                return Err(format!("{} has no response {}", arg(0), arg(1)));
            }
            match arg(2) {
                "default" => equal(
                    "default response",
                    dialog.default_response().as_deref(),
                    Some(arg(1)),
                ),
                state => equal(
                    &format!("{}'s {}", arg(0), arg(1)),
                    if dialog.is_response_enabled(arg(1)) {
                        "enabled"
                    } else {
                        "disabled"
                    },
                    state,
                ),
            }
        }
        other => Err(format!("unknown command {other}")),
    }
}

/// Makes `change` to the account's folders in the app's store, as the engine does once the
/// server has: a new folder, a rename in place of the folder and those under it, or a delete
/// of them all.
pub(super) async fn apply(
    harness: &Harness,
    account: AccountId,
    change: &FolderChange,
) -> StepResult {
    let mailbox = harness.state.mailbox();
    let known: Vec<_> = mailbox
        .folders()
        .into_iter()
        .filter(|folder| folder.account == account)
        .collect();
    let under = |id| -> Result<Vec<_>, String> {
        let top = known
            .iter()
            .find(|folder| folder.id == id)
            .ok_or("no such folder")?;
        Ok(known
            .iter()
            .filter(|folder| folders::within(&folder.name, &top.name, top.delimiter))
            .cloned()
            .collect())
    };
    match change {
        FolderChange::Create { parent, name } => {
            let parent = parent.and_then(|id| known.iter().find(|folder| folder.id == id));
            let whole = folders::child(parent.map(|parent| parent.name.as_str()), Some('/'), name);
            let reply = mailbox
                .store()
                .ok_or("the store is not open")?
                .upsert_folder(FolderSpec::local(account, &whole, FolderRole::Other));
            reply.recv().await.map_err(|error| error.to_string())?;
        }
        FolderChange::Rename { folder, name } => {
            let subtree = under(*folder)?;
            let top = subtree
                .iter()
                .find(|known| known.id == *folder)
                .ok_or("no such folder")?;
            let renamed = folders::child(
                folders::parent(&top.name, top.delimiter),
                top.delimiter,
                name,
            );
            let renames = subtree
                .iter()
                .filter_map(|known| {
                    folders::renamed(&known.name, &top.name, &renamed, top.delimiter).map(|new| {
                        FolderRename {
                            folder: known.id,
                            name_raw: new.clone(),
                            name: new,
                        }
                    })
                })
                .collect();
            let reply = mailbox
                .store()
                .ok_or("the store is not open")?
                .rename_folders(account, renames);
            reply.recv().await.map_err(|error| error.to_string())?;
        }
        FolderChange::Delete { folder } => {
            let ids = under(*folder)?.into_iter().map(|known| known.id).collect();
            let reply = mailbox
                .store()
                .ok_or("the store is not open")?
                .delete_folders(account, ids);
            reply.recv().await.map_err(|error| error.to_string())?;
        }
    }
    super::mail::settled(harness).await
}
