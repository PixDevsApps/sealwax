//! An account's rules (ADR-020, docs/m9/folders-rules.md): its Sieve script, read from
//! `rules/<key>.sieve` and compiled once per change of the file, run by Sealwax on the mail that
//! arrives in the account's Inbox and Junk, before `Event::NewMail` (mail a rule sends to Junk or
//! Trash doesn't notify), and on request for a folder (`Command::ApplyRules`). What the script
//! decides becomes ops in the op queue, as the user's own triage does: one move, the copies
//! before it, and flags; a POP3 account's copies are stored again from the message's bytes, as
//! it has no server to copy on. A header the store doesn't keep costs one header fetch for the messages
//! that need it (the stored original when the body is downloaded). A script that can't run runs
//! nothing and is reported once per change of its file. Nothing of a message or a script
//! reaches the log: counts and ids only.

use super::{Shared, actor, bodies, wake};
use crate::api::{Event, RequestId, RulesError};
use sealwax_domain::account::AccountKey;
use sealwax_domain::message::{HeaderField, header_fields};
use sealwax_domain::model::{
    AccountId, Flags, Folder, FolderId, FolderRole, MessageDetail, MessageId,
};
use sealwax_domain::sieve::{self, MessageView, Outcome, Script};
use sealwax_infrastructure::ingest::{self, RawMessage};
use sealwax_infrastructure::rules as files;
use sealwax_infrastructure::store::{RuleChange, RuleChanges};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

/// The headers the store keeps, which answer a test without a fetch.
const STORED: [&str; 9] = [
    "from",
    "to",
    "cc",
    "reply-to",
    "subject",
    "list-id",
    "message-id",
    "in-reply-to",
    "references",
];

/// Messages run per store round trip on `ApplyRules`.
const CHUNK: usize = 200;

/// Each account's script as last read, by key.
#[derive(Default)]
pub(super) struct Cache(Mutex<HashMap<AccountKey, Read>>);

/// A script file as read at its modification time.
#[derive(Clone)]
struct Read {
    modified: Option<SystemTime>,
    script: Result<Option<Arc<Script>>, RulesError>,
}

impl Cache {
    fn lock(&self) -> MutexGuard<'_, HashMap<AccountKey, Read>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The account's script: `None` without a file. A file that changed is read again, and an
/// error is reported once per change.
async fn script(
    shared: &Shared,
    account: AccountId,
    key: &AccountKey,
) -> Result<Option<Arc<Script>>, RulesError> {
    let dir = shared.paths.rules.clone();
    let owned = key.clone();
    let modified = tokio::task::spawn_blocking(move || files::modified(&dir, &owned))
        .await
        .map_err(|_| RulesError::File)?
        .map_err(|_| RulesError::File)?;
    if let Some(read) = shared.rules.lock().get(key)
        && read.modified == modified
    {
        return read.script.clone();
    }
    let script = match modified {
        None => Ok(None),
        Some(_) => {
            let dir = shared.paths.rules.clone();
            let owned = key.clone();
            match tokio::task::spawn_blocking(move || files::read(&dir, &owned)).await {
                Ok(Ok(Some(text))) => sieve::compile(&text)
                    .map(|script| Some(Arc::new(script)))
                    .map_err(RulesError::Script),
                Ok(Ok(None)) => Ok(None),
                Ok(Err(_)) | Err(_) => Err(RulesError::File),
            }
        }
    };
    if let Err(error) = &script {
        tracing::info!(account = account.0, kind = ?kind(error), "the account's rules can't run");
        shared.send(Event::RulesFailed {
            account,
            error: error.clone(),
        });
    }
    shared.rules.lock().insert(
        key.clone(),
        Read {
            modified,
            script: script.clone(),
        },
    );
    script
}

fn kind(error: &RulesError) -> &'static str {
    match error {
        RulesError::Script(_) => "script",
        RulesError::File => "file",
        RulesError::Store => "store",
    }
}

/// What a message shows a script: the stored headers, or all of them when fetched.
struct View {
    headers: HashMap<String, Vec<String>>,
    addresses: HashMap<String, Vec<String>>,
    size: u64,
    flags: Vec<String>,
    mailbox: String,
}

impl MessageView for View {
    fn header(&self, name: &str) -> Vec<&str> {
        self.headers
            .get(name)
            .map(|values| values.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    fn addresses(&self, name: &str) -> Vec<&str> {
        self.addresses
            .get(name)
            .map(|values| values.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    fn size(&self) -> u64 {
        self.size
    }

    fn flags(&self) -> Vec<&str> {
        self.flags.iter().map(String::as_str).collect()
    }

    fn mailbox(&self) -> &str {
        &self.mailbox
    }
}

impl View {
    fn new(detail: &MessageDetail, mailbox: &str, fields: Option<Vec<HeaderField>>) -> View {
        let mut headers: HashMap<String, Vec<String>> = HashMap::new();
        let mut addresses: HashMap<String, Vec<String>> = HashMap::new();
        match fields {
            Some(fields) => {
                for field in fields {
                    if !field.addresses.is_empty() {
                        addresses
                            .entry(field.name.clone())
                            .or_default()
                            .extend(field.addresses);
                    }
                    headers.entry(field.name).or_default().push(field.value);
                }
            }
            None => {
                let stored = &detail.headers;
                for (name, list) in [
                    ("from", &stored.from),
                    ("to", &stored.to),
                    ("cc", &stored.cc),
                    ("reply-to", &stored.reply_to),
                ] {
                    if list.is_empty() {
                        continue;
                    }
                    let shown = list
                        .iter()
                        .map(|address| match address.name.is_empty() {
                            true => address.email.clone(),
                            false => format!("{} <{}>", address.name, address.email),
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    headers.insert(name.into(), vec![shown]);
                    addresses.insert(
                        name.into(),
                        list.iter().map(|address| address.email.clone()).collect(),
                    );
                }
                headers.insert("subject".into(), vec![stored.subject.clone()]);
                for (name, id) in [
                    ("list-id", &stored.list_id),
                    ("message-id", &stored.message_id),
                ] {
                    if let Some(id) = id {
                        headers.insert(name.into(), vec![format!("<{id}>")]);
                    }
                }
                for (name, ids) in [
                    ("in-reply-to", &stored.in_reply_to),
                    ("references", &stored.references),
                ] {
                    if !ids.is_empty() {
                        let joined = ids
                            .iter()
                            .map(|id| format!("<{id}>"))
                            .collect::<Vec<_>>()
                            .join(" ");
                        headers.insert(name.into(), vec![joined]);
                    }
                }
            }
        }
        let mut flags: Vec<String> = sieve::flag_names(detail.summary.flags)
            .into_iter()
            .map(str::to_owned)
            .collect();
        flags.extend(detail.keywords.iter().cloned());
        View {
            headers,
            addresses,
            size: detail.size,
            flags,
            mailbox: mailbox.to_owned(),
        }
    }
}

/// Whether the script tests a header the store doesn't keep.
fn needs_headers(script: &Script) -> bool {
    script
        .headers()
        .iter()
        .any(|name| !STORED.contains(&name.as_str()))
}

/// The account's folders a script can name, by name.
struct Folders {
    by_name: HashMap<String, (FolderId, FolderRole)>,
    trash: Option<FolderId>,
}

impl Folders {
    fn new(folders: &[Folder], account: AccountId) -> Folders {
        let mut by_name = HashMap::new();
        let mut trash = None;
        for folder in folders.iter().filter(|folder| folder.account == account) {
            if !folder.selectable {
                continue;
            }
            if folder.role == FolderRole::Trash {
                trash = Some(folder.id);
            }
            let name = match folder.role {
                FolderRole::Inbox => "INBOX".to_owned(),
                _ => folder.name.clone(),
            };
            by_name.insert(name, (folder.id, folder.role));
        }
        Folders { by_name, trash }
    }

    /// The folder `name` names: exactly, or INBOX in any case (RFC 3501).
    fn find(&self, name: &str) -> Option<(FolderId, FolderRole)> {
        if name.eq_ignore_ascii_case("INBOX") {
            return self.by_name.get("INBOX").copied();
        }
        self.by_name.get(name).copied()
    }

    fn role(&self, folder: FolderId) -> Option<FolderRole> {
        self.by_name
            .values()
            .find(|(id, _)| *id == folder)
            .map(|(_, role)| *role)
    }
}

/// What a script's outcome does to a message in `at`: its changes, and where it ends up.
struct Planned {
    changes: Vec<RuleChange>,
    /// The folder a move takes it to.
    to: Option<FolderId>,
    /// Flags or keywords Sealwax can't set (only its own bits travel through the op queue).
    dropped: bool,
}

fn plan(outcome: &Outcome, at: FolderId, folders: &Folders) -> Planned {
    let mut dropped = false;
    let mut bits = |change: &sieve::FlagChange| {
        let (add, remove, add_keywords, remove_keywords) = change.to_model();
        dropped |= !add_keywords.is_empty() || !remove_keywords.is_empty();
        (add, remove)
    };
    // A fileinto that can't be carried out is a keep (RFC 5228 section 4.1).
    let mut keep = outcome.keep;
    let mut targets = Vec::new();
    for filed in &outcome.filed {
        match folders.find(&filed.folder) {
            Some((folder, _)) if folder != at => targets.push((folder, filed)),
            Some(_) => keep = true,
            None => keep = true,
        }
    }
    let mut changes = Vec::new();
    let mut to = None;
    let moving = if keep {
        None
    } else {
        targets
            .iter()
            .position(|(_, filed)| !filed.copy)
            .or((!targets.is_empty()).then_some(0))
    };
    for (index, (folder, _)) in targets.iter().enumerate() {
        if Some(index) != moving {
            changes.push(RuleChange::Copy { to: *folder });
        }
    }
    match moving {
        Some(index) => {
            let (folder, filed) = targets[index];
            let (add, remove) = bits(&filed.flags);
            changes.push(RuleChange::Move {
                to: folder,
                add,
                remove,
            });
            to = Some(folder);
        }
        None if !keep && outcome.trashed() => match folders.trash {
            Some(trash) if trash != at => {
                changes.push(RuleChange::Move {
                    to: trash,
                    add: Flags::NONE,
                    remove: Flags::NONE,
                });
                to = Some(trash);
            }
            _ => {}
        },
        None => {
            let (add, remove) = bits(&outcome.flags);
            if add != Flags::NONE || remove != Flags::NONE {
                changes.push(RuleChange::Flags { add, remove });
            }
        }
    }
    Planned {
        changes,
        to,
        dropped,
    }
}

/// The folder's account, its key and folders, and the folder as a script names it.
struct Account {
    id: AccountId,
    key: AccountKey,
    folders: Folders,
    mailbox: String,
    role: FolderRole,
}

async fn account_of(shared: &Shared, folder: FolderId) -> Result<Option<Account>, RulesError> {
    let folders = shared
        .store
        .folders()
        .recv()
        .await
        .map_err(|_| RulesError::Store)?
        .value;
    let Some(found) = folders.iter().find(|candidate| candidate.id == folder) else {
        return Ok(None);
    };
    let Some(key) = shared
        .store
        .accounts()
        .recv()
        .await
        .map_err(|_| RulesError::Store)?
        .value
        .into_iter()
        .find(|candidate| candidate.id == found.account)
        .and_then(|candidate| candidate.key)
    else {
        return Ok(None);
    };
    let mailbox = match found.role {
        FolderRole::Inbox => "INBOX".to_owned(),
        _ => found.name.clone(),
    };
    Ok(Some(Account {
        id: found.account,
        key,
        folders: Folders::new(&folders, found.account),
        mailbox,
        role: found.role,
    }))
}

/// What a run did.
#[derive(Default)]
struct Ran {
    /// Messages that got a change queued.
    changed: u32,
    /// Messages a rule moved to Junk or Trash.
    junked: HashSet<MessageId>,
}

/// Runs the account's script on `messages` of `folder` and queues what it decides. `Ok(None)`
/// when the account has no rules.
async fn run(
    shared: &Arc<Shared>,
    account: &Account,
    folder: FolderId,
    messages: &[MessageId],
) -> Result<Option<Ran>, RulesError> {
    let account_id = account.id;
    let Some(script) = script(shared, account_id, &account.key).await? else {
        return Ok(None);
    };
    let fetch = needs_headers(&script);
    let local = shared
        .store
        .is_pop3_account(account_id)
        .recv()
        .await
        .map_err(|_| RulesError::Store)?;
    let mut ran = Ran::default();
    let mut dropped = 0u32;
    for chunk in messages.chunks(CHUNK) {
        let mut details = Vec::with_capacity(chunk.len());
        for id in chunk {
            if let Ok(found) = shared.store.message(*id).recv().await
                && let Some(detail) = found.value
                && detail.summary.folder == folder
            {
                details.push(detail);
            }
        }
        let mut raw = if fetch {
            headers(shared, account_id, folder, &details).await
        } else {
            HashMap::new()
        };
        let mut queued = Vec::new();
        let mut copies = Vec::new();
        for detail in &details {
            let fields = raw
                .remove(&detail.summary.id)
                .map(|bytes| header_fields(&bytes));
            let view = View::new(detail, &account.mailbox, fields);
            let outcome = sieve::evaluate(&script, &view);
            let planned = plan(&outcome, folder, &account.folders);
            dropped += u32::from(planned.dropped);
            if planned.changes.is_empty() {
                continue;
            }
            if let Some(to) = planned.to
                && matches!(
                    account.folders.role(to),
                    Some(FolderRole::Junk | FolderRole::Trash)
                )
            {
                ran.junked.insert(detail.summary.id);
            }
            let mut changes = planned.changes;
            if local {
                for change in &changes {
                    if let (RuleChange::Copy { to }, Some(blob)) = (change, detail.blob) {
                        copies.push(RawCopy {
                            to: *to,
                            blob,
                            flags: detail.summary.flags,
                            keywords: detail.keywords.clone(),
                            date: detail.summary.date,
                        });
                    }
                }
                changes.retain(|change| !matches!(change, RuleChange::Copy { .. }));
            }
            if changes.is_empty() {
                ran.changed += 1;
                continue;
            }
            queued.push(RuleChanges {
                message: detail.summary.id,
                changes,
            });
        }
        if !copies.is_empty() {
            copy_locally(shared, account_id, copies).await;
        }
        if queued.is_empty() {
            continue;
        }
        let result = shared
            .store
            .queue_rules(queued)
            .recv()
            .await
            .map_err(|_| RulesError::Store)?;
        ran.changed += result.applied;
        for account in result.accounts {
            wake(shared, account, actor::Control::Ops);
        }
    }
    if dropped > 0 {
        tracing::info!(
            account = account_id.0,
            messages = dropped,
            "rules set keywords Sealwax doesn't keep"
        );
    }
    tracing::debug!(
        account = account_id.0,
        %folder,
        messages = messages.len(),
        changed = ran.changed,
        "rules ran"
    );
    Ok(Some(ran))
}

/// A copy for an account without a server: the message's bytes stored again in `to`.
struct RawCopy {
    to: FolderId,
    blob: sealwax_domain::model::BlobKey,
    flags: Flags,
    keywords: Vec<String>,
    date: i64,
}

/// Stores `copies` from their bytes in the blob store; a copy whose bytes are gone is skipped.
async fn copy_locally(shared: &Arc<Shared>, account: AccountId, copies: Vec<RawCopy>) {
    let blobs = shared.blobs.clone();
    let count = copies.len();
    let prepared = tokio::task::spawn_blocking(move || {
        let raws: Vec<RawMessage> = copies
            .into_iter()
            .filter_map(|copy| {
                Some(RawMessage {
                    folder: copy.to,
                    uid: None,
                    flags: copy.flags,
                    keywords: copy.keywords,
                    internal_date: Some(copy.date),
                    raw: blobs.get(&copy.blob).ok()?,
                })
            })
            .collect();
        ingest::prepare(&blobs, raws, 1)
    })
    .await;
    let messages = match prepared {
        Ok(Ok(prepared)) => prepared.messages,
        _ => {
            tracing::warn!(
                account = account.0,
                copies = count,
                "rules' copies couldn't be read"
            );
            return;
        }
    };
    if let Err(error) = shared.store.ingest(messages).recv().await {
        tracing::warn!(account = account.0, %error, "rules' copies couldn't be stored");
    }
}

/// The full headers of `details`: from the stored original when the body is downloaded, else
/// fetched on the account's body connection. A message whose headers can't be had is run on the
/// stored ones.
async fn headers(
    shared: &Arc<Shared>,
    account: AccountId,
    folder: FolderId,
    details: &[MessageDetail],
) -> HashMap<MessageId, Vec<u8>> {
    let mut found = HashMap::new();
    let mut missing = Vec::new();
    for detail in details {
        let id = detail.summary.id;
        let stored = match detail.blob {
            Some(_) => shared.store.raw_message(id).recv().await.ok().flatten(),
            None => None,
        };
        match stored {
            Some(bytes) => {
                found.insert(id, bytes);
            }
            None => missing.push(id),
        }
    }
    if missing.is_empty() {
        return found;
    }
    let uids: HashMap<MessageId, u32> = match shared.store.folder_messages(folder).recv().await {
        Ok(stored) => stored
            .into_iter()
            .map(|message| (message.id, message.uid))
            .collect(),
        Err(_) => return found,
    };
    let wanted: Vec<(MessageId, u32)> = missing
        .iter()
        .filter_map(|id| Some((*id, *uids.get(id)?)))
        .filter(|(_, uid)| *uid > 0)
        .collect();
    match bodies::headers(shared, account, folder, wanted).await {
        Ok(fetched) => found.extend(fetched),
        Err(error) => {
            tracing::info!(
                account = account.0,
                ?error,
                "headers for rules couldn't be fetched"
            );
        }
    }
    found
}

/// New mail of `folder`: the account's rules run on it when it arrived in the Inbox or Junk.
/// Replies with the messages that still count as new mail: not those a rule sent to Junk or
/// Trash.
pub(super) async fn on_arrival(
    shared: &Arc<Shared>,
    folder: FolderId,
    messages: Vec<MessageId>,
) -> Vec<MessageId> {
    let account = match account_of(shared, folder).await {
        Ok(Some(account)) if matches!(account.role, FolderRole::Inbox | FolderRole::Junk) => {
            account
        }
        Ok(_) => return messages,
        Err(error) => {
            tracing::info!(%folder, kind = kind(&error), "rules didn't run on new mail");
            return messages;
        }
    };
    match run(shared, &account, folder, &messages).await {
        Ok(Some(ran)) if !ran.junked.is_empty() => messages
            .into_iter()
            .filter(|message| !ran.junked.contains(message))
            .collect(),
        Ok(_) => messages,
        Err(error) => {
            tracing::info!(%folder, kind = kind(&error), "rules didn't run on new mail");
            messages
        }
    }
}

/// `ApplyRules`: the account's rules on every message the store holds in `folder`.
pub(super) async fn apply(shared: Arc<Shared>, request: RequestId, folder: FolderId) {
    let result = async {
        let messages: Vec<MessageId> = shared
            .store
            .folder_messages(folder)
            .recv()
            .await
            .map_err(|_| RulesError::Store)?
            .into_iter()
            .map(|message| message.id)
            .collect();
        let Some(account) = account_of(&shared, folder).await? else {
            return Ok(0);
        };
        Ok(run(&shared, &account, folder, &messages)
            .await?
            .map_or(0, |ran| ran.changed))
    }
    .await;
    shared.send(Event::RulesApplied {
        request,
        folder,
        result,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::model::{Address, Headers, MessageSummary, ThreadId};

    fn folders() -> Folders {
        let folder = |id, name: &str, role| Folder {
            id: FolderId(id),
            account: AccountId(1),
            name: name.into(),
            role,
            selectable: true,
            total: 0,
            unread: 0,
        };
        Folders::new(
            &[
                folder(1, "Inbox", FolderRole::Inbox),
                folder(2, "Junk", FolderRole::Junk),
                folder(3, "Trash", FolderRole::Trash),
                folder(4, "INBOX.Kunder", FolderRole::Other),
                folder(5, "Arkiv", FolderRole::Archive),
            ],
            AccountId(1),
        )
    }

    fn detail() -> MessageDetail {
        MessageDetail {
            summary: MessageSummary {
                id: MessageId(9),
                folder: FolderId(1),
                thread: ThreadId(1),
                sender_name: String::new(),
                sender_email: String::new(),
                subject: String::new(),
                snippet: String::new(),
                date: 0,
                flags: Flags::SEEN,
                has_attachments: false,
                thread_count: 1,
            },
            headers: Headers {
                from: vec![Address::new("Kari", "kari@example.test")],
                to: vec![Address::new("", "ola@example.test")],
                subject: "Faktura mars".into(),
                list_id: Some("rust.lists.example.test".into()),
                ..Headers::default()
            },
            body_kind: Default::default(),
            text_parts: Vec::new(),
            html_parts: Vec::new(),
            attachments: Vec::new(),
            keywords: vec!["$Label1".into()],
            size: 2048,
            blob: None,
        }
    }

    fn outcome(script: &str, view: &View) -> Outcome {
        sieve::evaluate(&sieve::compile(script).unwrap(), view)
    }

    #[test]
    fn the_stored_headers_answer_tests() {
        let view = View::new(&detail(), "INBOX", None);
        assert_eq!(view.header("from"), ["Kari <kari@example.test>"]);
        assert_eq!(view.addresses("from"), ["kari@example.test"]);
        assert_eq!(view.header("to"), ["ola@example.test"]);
        assert_eq!(view.header("list-id"), ["<rust.lists.example.test>"]);
        assert_eq!(view.flags(), ["\\Seen", "$Label1"]);
        assert!(view.header("x-spam-flag").is_empty());
        let filed = outcome(
            "require \"fileinto\"; if address :domain :is \"from\" \"example.test\" { fileinto \"INBOX.Kunder\"; }",
            &view,
        );
        let planned = plan(&filed, FolderId(1), &folders());
        assert_eq!(
            planned.changes,
            [RuleChange::Move {
                to: FolderId(4),
                add: Flags::NONE,
                remove: Flags::NONE
            }]
        );
        assert_eq!(planned.to, Some(FolderId(4)));
    }

    #[test]
    fn fetched_headers_answer_every_test() {
        let raw = b"From: Kari <kari@example.test>\r\nX-Spam-Flag: YES\r\n\r\n";
        let view = View::new(&detail(), "INBOX", Some(header_fields(raw)));
        assert_eq!(view.header("x-spam-flag"), ["YES"]);
        let junk = outcome(
            "require [\"fileinto\", \"imap4flags\"]; if header :is \"x-spam-flag\" \"YES\" { addflag \"$Junk\"; fileinto \"Junk\"; stop; }",
            &view,
        );
        let planned = plan(&junk, FolderId(1), &folders());
        assert_eq!(
            planned.changes,
            [RuleChange::Move {
                to: FolderId(2),
                add: Flags::JUNK,
                remove: Flags::NONE
            }]
        );
        assert!(needs_headers(
            &sieve::compile("if exists \"x-spam-flag\" { keep; }").unwrap()
        ));
        assert!(!needs_headers(
            &sieve::compile("if header :contains \"subject\" \"x\" { keep; }").unwrap()
        ));
    }

    #[test]
    fn copies_come_before_the_move_and_a_kept_message_only_copies() {
        let view = View::new(&detail(), "INBOX", None);
        let script = "require [\"fileinto\", \"copy\"]; fileinto :copy \"Arkiv\"; fileinto \"INBOX.Kunder\";";
        let planned = plan(&outcome(script, &view), FolderId(1), &folders());
        assert_eq!(
            planned.changes,
            [
                RuleChange::Copy { to: FolderId(5) },
                RuleChange::Move {
                    to: FolderId(4),
                    add: Flags::NONE,
                    remove: Flags::NONE
                }
            ]
        );
        let kept = "require [\"fileinto\", \"copy\"]; fileinto :copy \"Arkiv\";";
        let planned = plan(&outcome(kept, &view), FolderId(1), &folders());
        assert_eq!(planned.changes, [RuleChange::Copy { to: FolderId(5) }]);
        assert_eq!(planned.to, None);
    }

    #[test]
    fn a_missing_folder_keeps_discard_trashes_and_flags_stay_in_place() {
        let view = View::new(&detail(), "INBOX", None);
        let missing = "require \"fileinto\"; fileinto \"Nowhere\";";
        assert!(
            plan(&outcome(missing, &view), FolderId(1), &folders())
                .changes
                .is_empty()
        );
        let discard = plan(&outcome("discard;", &view), FolderId(1), &folders());
        assert_eq!(
            discard.changes,
            [RuleChange::Move {
                to: FolderId(3),
                add: Flags::NONE,
                remove: Flags::NONE
            }]
        );
        let flags = "require \"imap4flags\"; addflag \"\\\\Flagged\"; removeflag \"\\\\Seen\"; addflag \"custom\";";
        let planned = plan(&outcome(flags, &view), FolderId(1), &folders());
        assert_eq!(
            planned.changes,
            [RuleChange::Flags {
                add: Flags::FLAGGED,
                remove: Flags::SEEN
            }]
        );
        assert!(planned.dropped, "a keyword Sealwax doesn't keep");
        let inbox = "require \"fileinto\"; fileinto \"inbox\";";
        assert!(
            plan(&outcome(inbox, &view), FolderId(1), &folders())
                .changes
                .is_empty(),
            "filed where it is: kept"
        );
    }
}
