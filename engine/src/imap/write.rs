//! Changes on the server for the op queue (ML-051, ML-052; ADR-015 and its S1 amendment): flags,
//! moves, deleting for good and a folder marked read, by UID, safe to run again after an unknown
//! outcome, and without the store: phase B's op runner calls them on the account's command
//! connection and records what they report. Every call selects its folder read-write (Dovecot
//! ignores a STORE in an EXAMINEd mailbox and still says OK, S1), checks that the folder's
//! UIDVALIDITY is the op's, and runs every command through the raw layer, which checks each
//! completion. CLOSE and a plain EXPUNGE are never sent: both remove every `\Deleted` message of
//! the folder, not only the op's.
//!
//! - [`store_flags`]: `UID STORE … +FLAGS.SILENT` and `-FLAGS.SILENT`, keywords only where the
//!   folder's PERMANENTFLAGS keeps them.
//! - [`move_messages`]: `UID MOVE` with COPYUID; without MOVE, `UID COPY`, `\Deleted` and
//!   `UID EXPUNGE` (UIDPLUS), or the messages stay flagged `\Deleted`. A retry first looks for
//!   candidates by Message-ID among the target's UIDs from the [`Mark`] up, then verifies their
//!   complete contents before deleting a source that is still present. Unverified sources are
//!   transferred again (S1: a blind replay of a COPY-based move made two copies, and Stalwart
//!   never matches `SEARCH HEADER Message-ID`). COPYUID's pairs are checked against
//!   the target's Message-IDs before they are reported: Stalwart 0.16 lists both of its sets in
//!   ascending order whatever order it copied in.
//! - [`copy_messages`] (ADR-020): `UID COPY` for a rule's `fileinto :copy`; a retry looks for
//!   the copies by Message-ID from the [`Mark`] up first, as a move's does.
//! - [`expunge`]: `\Deleted`, then `UID EXPUNGE` with UIDPLUS; otherwise left flagged.
//! - [`mark_folder_read`]: `UID SEARCH UNSEEN UNDELETED`, then `\Seen` on exactly those.
//! - [`append`], [`replace_draft`] and [`find`] (M6): the Sent copy and drafts, appended at most
//!   once by the same mark.

mod append;
mod retry;

pub use append::{
    Append, Appended, FIND_SCAN, Found, OldCopies, Replaced, append, find, replace_draft,
};

use super::connection::Connection;
use super::error::ImapError;
use super::raw::{self, Code, CopyUid, quote};
use super::sync::view::{self, View};
use async_imap::imap_proto::{
    AttributeValue, MailboxDatum, MessageSection, Response, ResponseCode, SectionPath, Status,
    StatusAttribute,
};
use sealwax_domain::message;
use sealwax_domain::model::Flags;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt;
use std::time::Instant;

/// UIDs per command at most, which keeps each reply well inside the read timeout.
const BATCH_UIDS: usize = 1_000;
/// Bytes of a command's UID set at most: RFC 7162 asks clients to keep command lines near 8,192
/// octets.
const BATCH_SET: usize = 4_000;
/// STORE and UID EXPUNGE commands sent before their replies are read.
const PIPELINE: usize = 8;

/// The IMAP names of the flags Sealwax changes.
const NAMES: [(Flags, &str); 8] = [
    (Flags::SEEN, "\\Seen"),
    (Flags::ANSWERED, "\\Answered"),
    (Flags::FLAGGED, "\\Flagged"),
    (Flags::DELETED, "\\Deleted"),
    (Flags::DRAFT, "\\Draft"),
    (Flags::FORWARDED, "$Forwarded"),
    (Flags::JUNK, "$Junk"),
    (Flags::NOT_JUNK, "$NotJunk"),
];

fn names(flags: Flags) -> Vec<&'static str> {
    NAMES
        .iter()
        .filter(|(bit, _)| flags.contains(*bit))
        .map(|(_, name)| *name)
        .collect()
}

/// A folder as an op names it: its wire name and the UIDVALIDITY the op's UIDs belong to.
#[derive(Clone, Copy)]
pub struct OpFolder<'a> {
    pub name_raw: &'a str,
    pub uidvalidity: u32,
}

impl fmt::Debug for OpFolder<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpFolder")
            .field("uidvalidity", &self.uidvalidity)
            .finish_non_exhaustive()
    }
}

/// A message a move takes: its UID in the source and its Message-ID as the store keeps it (with
/// or without angle brackets), by which a retry finds it in the target.
#[derive(Clone, Copy)]
pub struct Moving<'a> {
    pub uid: u32,
    pub message_id: Option<&'a str>,
}

impl fmt::Debug for Moving<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Moving")
            .field("uid", &self.uid)
            .finish_non_exhaustive()
    }
}

/// A target folder before a move's first attempt, which the op keeps: a retry looks for the
/// messages among the UIDs from `uidnext` up. When the folder's UIDVALIDITY is no longer
/// `uidvalidity` (it was made again), the retry looks through the whole folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    pub uidvalidity: Option<u32>,
    pub uidnext: u32,
}

/// A move as the op queue keeps it.
#[derive(Clone, Copy, Debug)]
pub struct Move<'a> {
    pub from: OpFolder<'a>,
    /// The target's wire name.
    pub to: &'a str,
    pub messages: &'a [Moving<'a>],
    /// Flags set and cleared in the source first, where it keeps them: Junk's `$Junk` and
    /// `$NotJunk` travel with the messages.
    pub add: Flags,
    pub remove: Flags,
    /// The target before the first attempt ([`mark_target`]). A COPY-based retry without it
    /// looks through the whole target.
    pub mark: Option<Mark>,
    /// An earlier attempt may have done part of it.
    pub retry: bool,
}

/// A copy as the op queue keeps it: the messages stay in `from`.
#[derive(Clone, Copy, Debug)]
pub struct Copy<'a> {
    pub from: OpFolder<'a>,
    /// The target's wire name.
    pub to: &'a str,
    pub messages: &'a [Moving<'a>],
    /// The target before the first attempt; a retry without it looks through the whole target.
    pub mark: Option<Mark>,
    /// An earlier attempt may have done part of it.
    pub retry: bool,
}

/// What a change did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Done {
    /// Source and target UID of each moved message whose target UID is known: from COPYUID
    /// checked against the target's Message-IDs, or found by Message-ID among the target's new
    /// UIDs. A message left out arrives in the target as new mail.
    pub moved: Vec<(u32, u32)>,
    /// The target's UIDVALIDITY that `moved` belongs to.
    pub target_uidvalidity: Option<u32>,
    /// UIDs the server showed were no longer in the folder. A UID in `moved` too was moved by an
    /// earlier attempt.
    pub gone: Vec<u32>,
    /// UIDs flagged `\Deleted` and left in the folder: the server has no UIDPLUS, and a plain
    /// EXPUNGE would remove other `\Deleted` messages too. The store keeps them hidden.
    pub left_deleted: Vec<u32>,
    /// Flags the folder can't keep (PERMANENTFLAGS), which weren't sent.
    pub dropped: Flags,
    /// The messages a folder marked read changed: its Undo clears `\Seen` on these.
    pub marked: Vec<u32>,
}

/// Why a change didn't happen, or may have happened only in part.
#[derive(thiserror::Error)]
pub enum WriteError {
    #[error(transparent)]
    Imap(#[from] ImapError),
    /// The folder opened `[READ-ONLY]`.
    #[error("the folder is read-only")]
    ReadOnly,
    /// The folder's UIDVALIDITY isn't the op's: its UIDs name other messages now.
    #[error("the folder's UIDVALIDITY changed")]
    UidValidity { current: u32 },
}

impl fmt::Debug for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WriteError({self})")
    }
}

/// What the op runner does after a failed change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteFailure {
    /// Try again later, on a new connection when [`WriteError::is_connection`]: the connection
    /// was lost or timed out, the server said BYE, `[UNAVAILABLE]`, `[INUSE]` or a NO without a
    /// code it names. The attempt may have done part of the change, so the next one is a retry.
    Retry,
    /// The server won't do it (`[TRYCREATE]`, `[NONEXISTENT]`, `[READ-ONLY]`, `[CANNOT]`,
    /// `[NOPERM]`, BAD): the op is dropped and the messages put back.
    Refuse,
    /// The mailbox is full (`[OVERQUOTA]`): the account shows quota full; try again later.
    QuotaFull,
    /// The folder's UIDVALIDITY is now this: the op is re-targeted by Message-ID.
    Retarget(u32),
}

impl WriteError {
    pub fn failure(&self) -> WriteFailure {
        match self {
            WriteError::ReadOnly => WriteFailure::Refuse,
            WriteError::UidValidity { current } => WriteFailure::Retarget(*current),
            WriteError::Imap(ImapError::No(text)) => {
                match text.code().map(str::to_ascii_uppercase).as_deref() {
                    Some("OVERQUOTA") => WriteFailure::QuotaFull,
                    Some(
                        "TRYCREATE" | "NONEXISTENT" | "READ-ONLY" | "CANNOT" | "NOPERM"
                        | "ALREADYEXISTS" | "CLIENTBUG" | "CONTACTADMIN" | "TOOBIG",
                    ) => WriteFailure::Refuse,
                    _ => WriteFailure::Retry,
                }
            }
            WriteError::Imap(ImapError::Bad(_)) => WriteFailure::Refuse,
            WriteError::Imap(_) => WriteFailure::Retry,
        }
    }

    /// The connection can't be used any more and must be replaced.
    pub fn is_connection(&self) -> bool {
        matches!(self, WriteError::Imap(error) if error.is_connection())
    }
}

/// A folder selected read-write.
#[derive(Clone, Debug)]
pub struct Selected {
    pub uidvalidity: u32,
    pub uidnext: Option<u32>,
    pub exists: u32,
    /// PERMANENTFLAGS, lower case; `None` when the server sent none, which means every flag is
    /// kept (RFC 3501).
    permanent: Option<Vec<String>>,
}

impl Selected {
    /// Whether the folder keeps `flag` across sessions: PERMANENTFLAGS lists it, or lists `\*`
    /// and it is a keyword.
    pub fn keeps(&self, flag: &str) -> bool {
        let Some(listed) = &self.permanent else {
            return true;
        };
        let flag = flag.to_ascii_lowercase();
        listed.contains(&flag) || !flag.starts_with('\\') && listed.iter().any(|f| f == "\\*")
    }

    /// The part of `flags` the folder keeps.
    pub fn kept(&self, flags: Flags) -> Flags {
        NAMES
            .iter()
            .filter(|(bit, name)| flags.contains(*bit) && self.keeps(name))
            .fold(Flags::NONE, |kept, (bit, _)| kept.union(*bit))
    }
}

/// SELECT `name_raw` read-write: its UIDVALIDITY, UIDNEXT, EXISTS and PERMANENTFLAGS. A
/// `[READ-ONLY]` answer is refused, and the folder stays selected read-only.
pub async fn select(conn: &mut Connection, name_raw: &str) -> Result<Selected, WriteError> {
    let command = format!("SELECT {}", quote(name_raw)?);
    conn.selected = None;
    let mut uidvalidity = None;
    let mut uidnext = None;
    let mut exists = 0;
    let mut permanent = None;
    let code = raw::run_coded(&mut conn.session, &command, |response| match response {
        Response::Data {
            status: Status::Ok,
            outcome,
        } => match &outcome.code {
            Some(ResponseCode::UidValidity(value)) => uidvalidity = Some(*value),
            Some(ResponseCode::UidNext(value)) => uidnext = Some(*value),
            Some(ResponseCode::PermanentFlags(flags)) => {
                permanent = Some(flags.iter().map(|flag| flag.to_ascii_lowercase()).collect());
            }
            _ => {}
        },
        Response::MailboxData(MailboxDatum::Exists(count)) => exists = *count,
        _ => {}
    })
    .await?;
    conn.selected = Some(name_raw.to_owned());
    if code == Some(Code::ReadOnly) {
        return Err(WriteError::ReadOnly);
    }
    Ok(Selected {
        uidvalidity: uidvalidity.ok_or(ImapError::Protocol("SELECT without UIDVALIDITY"))?,
        uidnext,
        exists,
        permanent,
    })
}

/// Selects the op's folder and checks its UIDVALIDITY.
async fn open(conn: &mut Connection, folder: OpFolder<'_>) -> Result<Selected, WriteError> {
    let selected = select(conn, folder.name_raw).await?;
    if selected.uidvalidity != folder.uidvalidity {
        return Err(WriteError::UidValidity {
            current: selected.uidvalidity,
        });
    }
    Ok(selected)
}

/// The target's UIDVALIDITY and UIDNEXT, which the op keeps before a move's first attempt
/// ([`Move::mark`]): STATUS, or EXAMINE when the target is the connection's selected folder
/// (Dovecot answers STATUS of that from a stale view, S1). A server that sends no UIDNEXT gets
/// a mark from UID 1.
pub async fn mark_target(conn: &mut Connection, name_raw: &str) -> Result<Mark, WriteError> {
    if conn.selected.as_deref() == Some(name_raw) {
        let view = view::examine(conn, name_raw).await?;
        return Ok(Mark {
            uidvalidity: Some(view.uidvalidity),
            uidnext: view.uidnext.unwrap_or(1),
        });
    }
    let mut uidvalidity = None;
    let mut uidnext = None;
    let command = format!("STATUS {} (UIDNEXT UIDVALIDITY)", quote(name_raw)?);
    raw::run(&mut conn.session, &command, |response| {
        if let Response::MailboxData(MailboxDatum::Status { status, .. }) = response {
            for attribute in status {
                match attribute {
                    StatusAttribute::UidNext(value) => uidnext = Some(*value),
                    StatusAttribute::UidValidity(value) => uidvalidity = Some(*value),
                    _ => {}
                }
            }
        }
    })
    .await?;
    Ok(Mark {
        uidvalidity,
        uidnext: uidnext.unwrap_or(1),
    })
}

/// A command's share of UIDs: the set as sent and the UIDs in it.
#[derive(Debug, Default)]
struct Batch {
    set: String,
    uids: Vec<u32>,
}

/// `uids` as compact UID sets of at most [`BATCH_UIDS`] UIDs and [`BATCH_SET`] bytes each. UID 0
/// names no message and is left out.
fn batches(uids: &[u32]) -> Vec<Batch> {
    let sorted = unique(uids.iter().copied());
    let mut out = Vec::new();
    let mut batch = Batch::default();
    let mut i = 0;
    while i < sorted.len() {
        if batch.uids.len() == BATCH_UIDS {
            out.push(std::mem::take(&mut batch));
        }
        let start = i;
        while i + 1 < sorted.len()
            && sorted[i + 1] - sorted[i] == 1
            && batch.uids.len() + (i + 2 - start) <= BATCH_UIDS
        {
            i += 1;
        }
        let part = if start == i {
            sorted[i].to_string()
        } else {
            format!("{}:{}", sorted[start], sorted[i])
        };
        if !batch.set.is_empty() && batch.set.len() + 1 + part.len() > BATCH_SET {
            out.push(std::mem::take(&mut batch));
        }
        if !batch.set.is_empty() {
            batch.set.push(',');
        }
        batch.set.push_str(&part);
        batch.uids.extend_from_slice(&sorted[start..=i]);
        i += 1;
    }
    if !batch.uids.is_empty() {
        out.push(batch);
    }
    out
}

/// UIDs sorted, once each, without 0.
fn unique(uids: impl IntoIterator<Item = u32>) -> Vec<u32> {
    let mut out: Vec<u32> = uids.into_iter().filter(|uid| *uid > 0).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Runs `commands` a few at a time, each group's replies read together.
async fn pipelined(conn: &mut Connection, commands: &[String]) -> Result<(), ImapError> {
    for group in commands.chunks(PIPELINE) {
        raw::run_pipelined(&mut conn.session, group, |_| {}).await?;
    }
    Ok(())
}

/// `UID STORE` adding `add` and removing `remove` on `uids`.
async fn store(
    conn: &mut Connection,
    uids: &[u32],
    add: &[&str],
    remove: &[&str],
) -> Result<(), ImapError> {
    let mut commands = Vec::new();
    for batch in batches(uids) {
        for (sign, list) in [('+', add), ('-', remove)] {
            if !list.is_empty() {
                commands.push(format!(
                    "UID STORE {} {sign}FLAGS.SILENT ({})",
                    batch.set,
                    list.join(" ")
                ));
            }
        }
    }
    pipelined(conn, &commands).await
}

/// `\Deleted` on `uids`, then UID EXPUNGE of exactly those where the server has UIDPLUS; without
/// it they stay flagged. The EXPUNGEs wait for the STOREs: Stalwart runs pipelined commands
/// concurrently.
async fn delete(conn: &mut Connection, uids: &[u32], done: &mut Done) -> Result<(), ImapError> {
    if uids.is_empty() {
        return Ok(());
    }
    store(conn, uids, &["\\Deleted"], &[]).await?;
    if conn.caps().has("UIDPLUS") {
        let commands: Vec<String> = batches(uids)
            .into_iter()
            .map(|batch| format!("UID EXPUNGE {}", batch.set))
            .collect();
        pipelined(conn, &commands).await?;
    } else {
        done.left_deleted.extend_from_slice(uids);
    }
    Ok(())
}

/// What COPYUIDs say about a batch: its pairs (into `copied`, still to be checked), and its UIDs
/// they don't name, which the server didn't find.
fn record(done: &mut Done, copied: &mut Vec<(u32, u32)>, batch: &[u32], copies: Vec<CopyUid>) {
    let asked: HashSet<u32> = batch.iter().copied().collect();
    let mut named = HashSet::new();
    for copy in copies {
        done.target_uidvalidity = Some(copy.uidvalidity);
        for (source, target) in copy.pairs {
            if asked.contains(&source) && named.insert(source) {
                copied.push((source, target));
            }
        }
    }
    done.gone
        .extend(batch.iter().filter(|uid| !named.contains(uid)).copied());
}

/// `UID MOVE` (`moves`) or `UID COPY` of `uids` to `to`, batch by batch, keeping COPYUID from an
/// untagged OK or the tagged one (both occur) in `copied`. A MOVE that expunged nothing found
/// nothing.
async fn transfer(
    conn: &mut Connection,
    moves: bool,
    uids: &[u32],
    to: &str,
    done: &mut Done,
    copied: &mut Vec<(u32, u32)>,
) -> Result<(), ImapError> {
    let verb = if moves { "MOVE" } else { "COPY" };
    let to = quote(to)?;
    for batch in batches(uids) {
        let mut copies = Vec::new();
        let mut expunged = 0u64;
        let command = format!("UID {verb} {} {to}", batch.set);
        let tagged = raw::run_coded(&mut conn.session, &command, |response| match response {
            Response::Data {
                status: Status::Ok,
                outcome,
            } => copies.extend(raw::copy_uid(outcome.code.as_ref())),
            Response::Expunge(_) => expunged += 1,
            Response::Vanished {
                earlier: false,
                uids,
            } => {
                expunged += uids
                    .iter()
                    .map(|range| u64::from(range.end() - range.start()) + 1)
                    .sum::<u64>();
            }
            _ => {}
        })
        .await?;
        if let Some(Code::CopyUid(copy)) = tagged {
            copies.push(copy);
        }
        if !copies.is_empty() {
            record(done, copied, &batch.uids, copies);
        } else if moves && expunged == 0 {
            done.gone.extend_from_slice(&batch.uids);
        }
    }
    Ok(())
}

/// The UIDs of `uids` still in the selected folder.
async fn present(conn: &mut Connection, uids: &[u32]) -> Result<BTreeSet<u32>, ImapError> {
    let mut found = BTreeSet::new();
    let mut view = View::default();
    for batch in batches(uids) {
        let criteria = format!("UID {}", batch.set);
        found.extend(view::uid_search(conn, &criteria, &mut view).await?);
    }
    Ok(found)
}

/// A target's messages from a mark up, by UID with their Message-IDs.
struct Window {
    uidvalidity: u32,
    entries: Vec<(u32, String)>,
}

/// The Message-ID of a FETCH response's header section, as the store keeps it.
fn fetched_id(attributes: &[AttributeValue<'_>]) -> Option<(u32, String)> {
    let mut uid = None;
    let mut id = None;
    for attribute in attributes {
        match attribute {
            AttributeValue::Uid(found) => uid = Some(*found),
            AttributeValue::BodySection {
                section: Some(SectionPath::Full(MessageSection::Header)),
                data,
                ..
            } => {
                id = message::parse(data.as_deref().unwrap_or_default())
                    .headers
                    .message_id
            }
            _ => {}
        }
    }
    Some((uid?, id?))
}

/// EXAMINEs the target and fetches the Message-IDs of its UIDs from `mark` up; from UID 1
/// without a mark or when its UIDVALIDITY changed since. Only UIDs at or above the start count:
/// `n:*` names the highest UID even when it is below `n`.
async fn window(conn: &mut Connection, to: &str, mark: Option<Mark>) -> Result<Window, ImapError> {
    let view = view::examine(conn, to).await?;
    let since = match mark {
        Some(mark)
            if mark
                .uidvalidity
                .is_none_or(|known| known == view.uidvalidity) =>
        {
            mark.uidnext.max(1)
        }
        _ => 1,
    };
    let mut entries = Vec::new();
    if view.exists > 0 && view.uidnext.is_none_or(|next| next > since) {
        let command = format!("UID FETCH {since}:* (UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])");
        raw::run(&mut conn.session, &command, |response| {
            if let Response::Fetch(_, attributes) = response
                && let Some((uid, id)) = fetched_id(attributes)
                && uid >= since
            {
                entries.push((uid, id));
            }
        })
        .await?;
    }
    entries.sort_unstable();
    entries.dedup_by_key(|(uid, _)| *uid);
    Ok(Window {
        uidvalidity: view.uidvalidity,
        entries,
    })
}

/// Pairs messages with target UIDs (`entries`, with their Message-IDs) that carry their
/// Message-ID, each target UID once: messages no longer in the source first (an earlier attempt
/// moved them), then the others, by UID.
fn assign(
    messages: &[Moving<'_>],
    present: &BTreeSet<u32>,
    entries: &[(u32, String)],
    claimed: &mut HashSet<u32>,
) -> Vec<(u32, u32)> {
    let mut by_id: HashMap<&str, VecDeque<u32>> = HashMap::new();
    for (uid, id) in entries {
        if !claimed.contains(uid) {
            by_id.entry(id.as_str()).or_default().push_back(*uid);
        }
    }
    let mut order: Vec<(bool, u32, String)> = messages
        .iter()
        .filter_map(|message| {
            let id = message::normalize_id(message.message_id?)?;
            Some((present.contains(&message.uid), message.uid, id))
        })
        .collect();
    order.sort_unstable();
    order.dedup_by_key(|(_, uid, _)| *uid);
    let mut pairs = Vec::new();
    for (_, uid, id) in order {
        if let Some(target) = by_id.get_mut(id.as_str()).and_then(VecDeque::pop_front) {
            claimed.insert(target);
            pairs.push((uid, target));
        }
    }
    pairs
}

/// COPYUID's pairs, checked against the Message-IDs of the target UIDs it names. Stalwart 0.16
/// writes both of COPYUID's sets in ascending order whatever order it copied in, so its pairs can
/// swap messages: where a pair disagrees, the messages are paired again by Message-ID among
/// COPYUID's target UIDs, and a pair with no Message-ID to check is kept only when every checked
/// pair agreed. A single pair needs no check.
async fn checked(
    conn: &mut Connection,
    to: &str,
    messages: &[Moving<'_>],
    copied: Vec<(u32, u32)>,
    claimed: &mut HashSet<u32>,
) -> Result<Vec<(u32, u32)>, ImapError> {
    let targets: Vec<u32> = copied.iter().map(|(_, target)| *target).collect();
    if copied.len() < 2 {
        claimed.extend(targets);
        return Ok(copied);
    }
    view::examine(conn, to).await?;
    let mut entries = Vec::new();
    for batch in batches(&targets) {
        let command = format!(
            "UID FETCH {} (UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])",
            batch.set
        );
        raw::run(&mut conn.session, &command, |response| {
            if let Response::Fetch(_, attributes) = response
                && let Some(entry) = fetched_id(attributes)
            {
                entries.push(entry);
            }
        })
        .await?;
    }
    entries.sort_unstable();
    entries.dedup_by_key(|(uid, _)| *uid);
    let theirs: HashMap<u32, &str> = entries
        .iter()
        .map(|(uid, id)| (*uid, id.as_str()))
        .collect();
    let ours: HashMap<u32, String> = messages
        .iter()
        .filter_map(|message| Some((message.uid, message::normalize_id(message.message_id?)?)))
        .collect();
    let checks: Vec<bool> = copied
        .iter()
        .filter_map(|(source, target)| Some(ours.get(source)? == theirs.get(target)?))
        .collect();
    if !checks.is_empty() && checks.iter().all(|agrees| *agrees) {
        claimed.extend(targets);
        return Ok(copied);
    }
    tracing::debug!(
        pairs = copied.len(),
        checked = checks.len(),
        "COPYUID unconfirmed by the target; paired by Message-ID"
    );
    let sources: HashSet<u32> = copied.iter().map(|(source, _)| *source).collect();
    let moved: Vec<Moving<'_>> = messages
        .iter()
        .filter(|message| sources.contains(&message.uid))
        .copied()
        .collect();
    Ok(assign(&moved, &BTreeSet::new(), &entries, claimed))
}

fn logged(op: &'static str, messages: usize, done: &Done, started: Instant) {
    tracing::debug!(
        op,
        messages,
        moved = done.moved.len(),
        gone = done.gone.len(),
        left_deleted = done.left_deleted.len(),
        marked = done.marked.len(),
        ms = started.elapsed().as_secs_f64() * 1000.0,
        "imap write done"
    );
}

/// Adds and removes flags on `uids` of `folder`: `+FLAGS.SILENT` and `-FLAGS.SILENT`, never a
/// replace, so running it again changes nothing more. Flags the folder can't keep aren't sent
/// ([`Done::dropped`]); a flag in both `add` and `remove` is removed. The server leaves out UIDs
/// no longer in the folder.
pub async fn store_flags(
    conn: &mut Connection,
    folder: OpFolder<'_>,
    uids: &[u32],
    add: Flags,
    remove: Flags,
) -> Result<Done, WriteError> {
    let started = Instant::now();
    let add = add.difference(remove);
    let uids = unique(uids.iter().copied());
    let mut done = Done::default();
    if uids.is_empty() || add.union(remove) == Flags::NONE {
        return Ok(done);
    }
    let selected = open(conn, folder).await?;
    let (add_kept, remove_kept) = (selected.kept(add), selected.kept(remove));
    done.dropped = add.union(remove).difference(add_kept.union(remove_kept));
    store(conn, &uids, &names(add_kept), &names(remove_kept)).await?;
    logged("flags", uids.len(), &done, started);
    Ok(done)
}

/// Moves messages to another folder of the account: UID MOVE where the server has MOVE, else
/// UID COPY, `\Deleted` and UID EXPUNGE (UIDPLUS), else the source copies stay flagged
/// `\Deleted` ([`Done::left_deleted`]). The flags in `add` and `remove` are changed in the source
/// first. UIDs no longer in the source count as done.
///
/// A retry looks for Message-ID candidates among the target's UIDs from the mark up
/// (a MOVE retry only with a mark: a replayed UID MOVE of a gone UID is harmless, S1). Before
/// deleting a source that is still present, its complete contents must match the candidate
/// within the retry comparison budget. Otherwise it is moved or copied again: a possible
/// duplicate is preferable to deleting distinct mail that shares a Message-ID.
///
/// The target UIDs in [`Done::moved`] are checked: COPYUID's pairs against the target's
/// Message-IDs (Stalwart's can swap messages), and where COPYUID is missing the target's new
/// UIDs from the mark up are matched by Message-ID.
pub async fn move_messages(conn: &mut Connection, request: Move<'_>) -> Result<Done, WriteError> {
    let started = Instant::now();
    let mut done = Done::default();
    let requested = unique(request.messages.iter().map(|message| message.uid));
    if requested.is_empty() {
        return Ok(done);
    }
    let moves = conn.caps().has("MOVE");
    let look = if request.retry && (!moves || request.mark.is_some()) {
        Some(window(conn, request.to, request.mark).await?)
    } else {
        None
    };
    let mut selected = open(conn, request.from).await?;
    let mut claimed = HashSet::new();
    let mut remaining = requested.clone();
    let mut stale = Vec::new();
    if let Some(look) = &look {
        let present = present(conn, &requested).await?;
        done.gone = requested
            .iter()
            .filter(|uid| !present.contains(uid))
            .copied()
            .collect();
        done.target_uidvalidity = Some(look.uidvalidity);
        done.moved = assign(request.messages, &present, &look.entries, &mut claimed);
        let mut verified = Vec::new();
        for (source, target) in std::mem::take(&mut done.moved) {
            if !present.contains(&source)
                || retry::same_message(
                    conn,
                    request.from,
                    source,
                    request.to,
                    look.uidvalidity,
                    target,
                )
                .await?
            {
                verified.push((source, target));
            } else {
                claimed.remove(&target);
            }
        }
        done.moved = verified;
        // Content comparisons EXAMINE both folders. Restore a writable source and check its
        // UIDVALIDITY again before any flags, copies or deletions.
        selected = open(conn, request.from).await?;
        let found: HashSet<u32> = done.moved.iter().map(|(source, _)| *source).collect();
        (stale, remaining) = present.iter().copied().partition(|uid| found.contains(uid));
    }
    let add = selected.kept(request.add.difference(request.remove));
    let remove = selected.kept(request.remove);
    done.dropped = request
        .add
        .union(request.remove)
        .difference(add.union(remove));
    store(conn, &remaining, &names(add), &names(remove)).await?;
    let mut copied = Vec::new();
    transfer(conn, moves, &remaining, request.to, &mut done, &mut copied).await?;
    if moves {
        delete(conn, &stale, &mut done).await?;
    } else {
        let sources = unique(remaining.iter().chain(&stale).copied());
        delete(conn, &sources, &mut done).await?;
    }
    let pairs = checked(conn, request.to, request.messages, copied, &mut claimed).await?;
    done.moved.extend(pairs);
    if let Some(mark) = request.mark {
        let sent: HashSet<u32> = remaining.iter().copied().collect();
        let known: HashSet<u32> = done
            .moved
            .iter()
            .map(|(source, _)| *source)
            .chain(done.gone.iter().copied())
            .collect();
        let unmapped: Vec<Moving<'_>> = request
            .messages
            .iter()
            .filter(|message| {
                sent.contains(&message.uid)
                    && !known.contains(&message.uid)
                    && message.message_id.is_some()
            })
            .copied()
            .collect();
        if !unmapped.is_empty() {
            let look = window(conn, request.to, Some(mark)).await?;
            done.target_uidvalidity = Some(look.uidvalidity);
            let found = assign(&unmapped, &BTreeSet::new(), &look.entries, &mut claimed);
            done.moved.extend(found);
        }
    }
    done.moved.sort_unstable();
    done.gone = unique(done.gone.iter().copied());
    done.left_deleted = unique(done.left_deleted.iter().copied());
    logged("move", requested.len(), &done, started);
    Ok(done)
}

/// Copies messages to another folder of the account with UID COPY. A retry first looks for them
/// by Message-ID among the target's UIDs from the mark up and copies only what isn't there, so a
/// replay makes no second copy. UIDs no longer in the source count as done ([`Done::gone`]). The
/// copies' target UIDs aren't reported: the target's pass stores them as new rows.
pub async fn copy_messages(conn: &mut Connection, request: Copy<'_>) -> Result<Done, WriteError> {
    let started = Instant::now();
    let mut done = Done::default();
    let requested = unique(request.messages.iter().map(|message| message.uid));
    if requested.is_empty() {
        return Ok(done);
    }
    let look = if request.retry {
        Some(window(conn, request.to, request.mark).await?)
    } else {
        None
    };
    open(conn, request.from).await?;
    let mut remaining = requested.clone();
    if let Some(look) = &look {
        let present = present(conn, &requested).await?;
        done.gone = requested
            .iter()
            .filter(|uid| !present.contains(uid))
            .copied()
            .collect();
        let mut claimed = HashSet::new();
        let found: HashSet<u32> = assign(request.messages, &present, &look.entries, &mut claimed)
            .into_iter()
            .map(|(source, _)| source)
            .collect();
        remaining = present
            .iter()
            .copied()
            .filter(|uid| !found.contains(uid))
            .collect();
    }
    let mut copied = Vec::new();
    transfer(conn, false, &remaining, request.to, &mut done, &mut copied).await?;
    done.gone = unique(done.gone.iter().copied());
    logged("copy", requested.len(), &done, started);
    Ok(done)
}

/// Deletes `uids` of `folder` for good: `\Deleted`, then UID EXPUNGE of exactly those where the
/// server has UIDPLUS. Without it they stay flagged `\Deleted` ([`Done::left_deleted`]) and the
/// store keeps them hidden: a plain EXPUNGE would remove the folder's other `\Deleted` messages
/// too.
pub async fn expunge(
    conn: &mut Connection,
    folder: OpFolder<'_>,
    uids: &[u32],
) -> Result<Done, WriteError> {
    let started = Instant::now();
    let uids = unique(uids.iter().copied());
    let mut done = Done::default();
    if uids.is_empty() {
        return Ok(done);
    }
    open(conn, folder).await?;
    delete(conn, &uids, &mut done).await?;
    logged("expunge", uids.len(), &done, started);
    Ok(done)
}

/// Marks every message of `folder` read on the server, whatever the store holds of it (a
/// windowed folder's older messages too): `UID SEARCH UNSEEN UNDELETED` (messages flagged
/// `\Deleted` are hidden), then `+FLAGS.SILENT (\Seen)` on exactly those, listed in
/// [`Done::marked`] for an Undo after the window. With `up_to`, only UIDs up to it: the newest
/// message the user had when they asked, so mail arriving meanwhile stays unread.
pub async fn mark_folder_read(
    conn: &mut Connection,
    folder: OpFolder<'_>,
    up_to: Option<u32>,
) -> Result<Done, WriteError> {
    let started = Instant::now();
    let mut done = Done::default();
    if up_to == Some(0) {
        return Ok(done);
    }
    let selected = open(conn, folder).await?;
    if !selected.keeps("\\Seen") {
        done.dropped = Flags::SEEN;
        return Ok(done);
    }
    if selected.exists > 0 {
        let criteria = match up_to {
            Some(last) => format!("UNSEEN UNDELETED UID 1:{last}"),
            None => "UNSEEN UNDELETED".to_owned(),
        };
        let unseen = view::uid_search(conn, &criteria, &mut View::default()).await?;
        store(conn, &unseen, &["\\Seen"], &[]).await?;
        done.marked = unseen;
    }
    logged("folder read", done.marked.len(), &done, started);
    Ok(done)
}

#[cfg(test)]
mod tests;
