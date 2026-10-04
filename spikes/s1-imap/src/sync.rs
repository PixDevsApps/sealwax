//! State-driven sync. Every pass starts from the server's state (EXAMINE) and the stored sync
//! state; server notices only decide *when* a pass runs, never what it concludes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::Result;
use async_imap::imap_proto::{
    AttributeValue, MailboxDatum, NameAttribute, Response, ResponseCode, Status,
};

use crate::fetch;
use crate::mutf7;
use crate::net::Connection;
use crate::raw::{self, ImapError, quote};
use crate::roles;
use crate::store::{
    Batch, FlagUpdate, FolderSnapshot, Header, ListedMailbox, MailboxRow, MailboxState, Store,
};

/// The store plus one async lock per mailbox, so the IDLE connection and the command connection
/// never run passes over the same mailbox at once. The std mutex is never held across an await.
pub struct Shared {
    store: Mutex<Store>,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl Shared {
    pub fn new(store: Store) -> Arc<Shared> {
        Arc::new(Shared {
            store: Mutex::new(store),
            locks: Mutex::new(HashMap::new()),
        })
    }

    /// Locks the store. Bind or use the guard within one statement and never take it twice in
    /// one statement: temporaries live to the end of the statement, and the lock isn't
    /// reentrant (a struct literal with two `store()` calls deadlocked an early perf run).
    pub fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn folder_lock(&self, name_raw: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        locks.entry(name_raw.to_owned()).or_default().clone()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SyncOptions {
    /// UIDs per FETCH batch.
    pub batch: usize,
    /// Deliberate defects, to show that the randomized test detects divergence.
    pub sabotage: Sabotage,
    /// A mailbox another connection keeps current (INBOX on the IDLE connection, ADR-006).
    pub elsewhere: Option<Elsewhere>,
    /// Use LIST-STATUS when offered; off forces the pipelined STATUS fallback.
    pub list_status: bool,
}

impl Default for SyncOptions {
    fn default() -> Self {
        SyncOptions {
            batch: 500,
            sabotage: Sabotage::None,
            elsewhere: None,
            list_status: true,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Elsewhere {
    /// Wire name.
    pub mailbox: &'static str,
    /// Still check its STATUS in the account pass and sync it when that shows a change: the
    /// backstop for notices a server never sends (Stalwart skips changes made just before IDLE).
    pub backstop: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sabotage {
    None,
    /// Drop VANISHED results (the count check should still repair the cache).
    IgnoreVanished,
    /// Drop VANISHED results and skip the count check (the cache must diverge).
    IgnoreVanishedNoCountCheck,
}

/// Counters for reports. No content, names or addresses.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub passes: u64,
    pub fast_path: u64,
    pub resets: u64,
    pub initial_batches: u64,
    pub headers_fetched: u64,
    pub new_message_fetches: u64,
    pub changedsince_fetches: u64,
    pub vanished_uids: u64,
    pub full_flag_scans: u64,
    pub count_mismatches: u64,
    pub uid_diffs: u64,
    pub flag_updates: u64,
    pub deletes: u64,
    pub mailboxes_removed: u64,
    pub skipped_missing: u64,
    /// Mailboxes skipped because their STATUS matched the stored state.
    pub status_skips: u64,
    /// NOOPs before IDLE that reported a change (another pass ran instead of IDLE).
    pub noop_changes: u64,
}

impl Stats {
    pub fn add(&mut self, other: &Stats) {
        self.passes += other.passes;
        self.fast_path += other.fast_path;
        self.resets += other.resets;
        self.initial_batches += other.initial_batches;
        self.headers_fetched += other.headers_fetched;
        self.new_message_fetches += other.new_message_fetches;
        self.changedsince_fetches += other.changedsince_fetches;
        self.vanished_uids += other.vanished_uids;
        self.full_flag_scans += other.full_flag_scans;
        self.count_mismatches += other.count_mismatches;
        self.uid_diffs += other.uid_diffs;
        self.flag_updates += other.flag_updates;
        self.deletes += other.deletes;
        self.mailboxes_removed += other.mailboxes_removed;
        self.skipped_missing += other.skipped_missing;
        self.status_skips += other.status_skips;
        self.noop_changes += other.noop_changes;
    }
}

/// What the server told us about the examined mailbox, kept current from untagged responses.
#[derive(Clone, Debug, Default)]
pub struct View {
    pub uidvalidity: u32,
    pub uidnext: Option<u32>,
    pub exists: u32,
    pub highestmodseq: Option<u64>,
    /// UIDs reported expunged (VANISHED, with or without EARLIER) during this pass.
    pub vanished: Vec<u32>,
}

impl View {
    fn track(&mut self, response: &Response<'_>) {
        match response {
            Response::MailboxData(MailboxDatum::Exists(n)) => self.exists = *n,
            Response::Expunge(_) => self.exists = self.exists.saturating_sub(1),
            Response::Vanished { earlier, uids } => {
                let uids = raw::expand(uids);
                if !earlier {
                    self.exists = self.exists.saturating_sub(uids.len() as u32);
                }
                self.vanished.extend(uids);
            }
            _ => {}
        }
    }
}

fn attribute_name(attribute: &NameAttribute<'_>) -> String {
    match attribute {
        NameAttribute::NoInferiors => "\\Noinferiors".into(),
        NameAttribute::NoSelect => "\\Noselect".into(),
        NameAttribute::Marked => "\\Marked".into(),
        NameAttribute::Unmarked => "\\Unmarked".into(),
        NameAttribute::All => "\\All".into(),
        NameAttribute::Archive => "\\Archive".into(),
        NameAttribute::Drafts => "\\Drafts".into(),
        NameAttribute::Flagged => "\\Flagged".into(),
        NameAttribute::Junk => "\\Junk".into(),
        NameAttribute::Sent => "\\Sent".into(),
        NameAttribute::Trash => "\\Trash".into(),
        NameAttribute::Extension(other) => other.to_string(),
        _ => String::new(),
    }
}

/// One LIST line: wire name, delimiter, attributes.
#[derive(Clone, Debug)]
pub struct ListEntry {
    pub name_raw: String,
    pub delimiter: Option<String>,
    pub attributes: Vec<String>,
}

impl ListEntry {
    pub fn selectable(&self) -> bool {
        !self.attributes.iter().any(|a| {
            a.eq_ignore_ascii_case("\\Noselect") || a.eq_ignore_ascii_case("\\NonExistent")
        })
    }
}

pub async fn list(conn: &mut Connection) -> Result<Vec<ListEntry>, ImapError> {
    let mut entries = Vec::new();
    raw::run(&mut conn.session, "LIST \"\" \"*\"", |r| {
        if let Response::MailboxData(MailboxDatum::List(l)) = r {
            entries.push(ListEntry {
                name_raw: l.name.to_string(),
                delimiter: l.delimiter.as_ref().map(|d| d.to_string()),
                attributes: l.name_attributes.iter().map(attribute_name).collect(),
            });
        }
    })
    .await?;
    Ok(entries)
}

/// Decodes names, detects roles and turns a LIST result into store rows.
pub fn listed_mailboxes(entries: &[ListEntry]) -> Vec<ListedMailbox> {
    let names: Vec<String> = entries
        .iter()
        .map(|e| mutf7::display_name(&e.name_raw))
        .collect();
    let role_entries: Vec<roles::Entry<'_>> = entries
        .iter()
        .zip(&names)
        .map(|(e, name)| roles::Entry {
            name,
            delimiter: e.delimiter.as_deref().and_then(|d| d.chars().next()),
            attributes: &e.attributes,
        })
        .collect();
    let assigned = roles::assign(&role_entries);
    entries
        .iter()
        .zip(names)
        .zip(assigned)
        .map(|((e, name), role)| ListedMailbox {
            name_raw: e.name_raw.clone(),
            name,
            delimiter: e.delimiter.clone(),
            attributes: e.attributes.clone(),
            role: role.map(|r| r.as_str().to_owned()),
            selectable: e.selectable(),
        })
        .collect()
}

pub async fn sync_list(conn: &mut Connection, shared: &Shared, stats: &mut Stats) -> Result<()> {
    let entries = list(conn).await?;
    let listed = listed_mailboxes(&entries);
    stats.mailboxes_removed += shared.store().apply_list(&listed)? as u64;
    Ok(())
}

pub async fn examine(conn: &mut Connection, name_raw: &str) -> Result<View, ImapError> {
    let command = if conn.qresync || !conn.caps.has("CONDSTORE") {
        format!("EXAMINE {}", quote(name_raw))
    } else {
        format!("EXAMINE {} (CONDSTORE)", quote(name_raw))
    };
    conn.selected = None;
    let mut view = View::default();
    let mut uidvalidity = None;
    raw::run(&mut conn.session, &command, |r| match r {
        Response::Data {
            status: Status::Ok,
            outcome,
        } => match &outcome.code {
            Some(ResponseCode::UidValidity(v)) => uidvalidity = Some(*v),
            Some(ResponseCode::UidNext(n)) => view.uidnext = Some(*n),
            Some(ResponseCode::HighestModSeq(m)) => view.highestmodseq = Some(*m),
            _ => {}
        },
        other => view.track(other),
    })
    .await?;
    view.uidvalidity =
        uidvalidity.ok_or_else(|| ImapError::Protocol("EXAMINE without UIDVALIDITY".into()))?;
    view.vanished.clear();
    conn.selected = Some(name_raw.to_owned());
    Ok(view)
}

/// `UID SEARCH <criteria>`, all matching UIDs.
pub async fn uid_search(
    conn: &mut Connection,
    criteria: &str,
    view: &mut View,
) -> Result<Vec<u32>, ImapError> {
    let mut uids = Vec::new();
    raw::run(
        &mut conn.session,
        &format!("UID SEARCH {criteria}"),
        |r| match r {
            Response::MailboxData(MailboxDatum::Search(found)) => uids.extend_from_slice(found),
            other => view.track(other),
        },
    )
    .await?;
    uids.sort_unstable();
    uids.dedup();
    Ok(uids)
}

fn has_envelope(attrs: &[AttributeValue<'_>]) -> bool {
    attrs
        .iter()
        .any(|a| matches!(a, AttributeValue::Envelope(_)))
}

/// Full header rows for `set`. FETCH responses without an ENVELOPE (unsolicited flag updates)
/// or for UIDs the caller did not ask for are hints and are dropped.
pub async fn fetch_headers(
    conn: &mut Connection,
    set: &str,
    wanted: impl Fn(u32) -> bool,
    view: &mut View,
) -> Result<Vec<Header>, ImapError> {
    let items = if view.highestmodseq.is_some() {
        fetch::HEADER_ITEMS_MODSEQ
    } else {
        fetch::HEADER_ITEMS
    };
    let mut headers = Vec::new();
    raw::run(
        &mut conn.session,
        &format!("UID FETCH {set} {items}"),
        |r| match r {
            Response::Fetch(_, attrs) if has_envelope(attrs) => {
                if let Some(header) = fetch::header(attrs).filter(|h| wanted(h.uid)) {
                    headers.push(header);
                }
            }
            other => view.track(other),
        },
    )
    .await?;
    Ok(headers)
}

/// UID FETCH of FLAGS with optional modifiers; returns every FLAGS update seen.
async fn fetch_flags(
    conn: &mut Connection,
    command: &str,
    view: &mut View,
) -> Result<Vec<FlagUpdate>, ImapError> {
    let mut updates = Vec::new();
    raw::run(&mut conn.session, command, |r| match r {
        Response::Fetch(_, attrs) => {
            if let Some(update) = fetch::flags(attrs) {
                updates.push(update);
            }
        }
        other => view.track(other),
    })
    .await?;
    Ok(updates)
}

fn is_missing_mailbox(error: &anyhow::Error) -> bool {
    matches!(error.downcast_ref::<ImapError>(), Some(ImapError::No(_)))
}

/// STATUS of one mailbox as the change check needs it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MailboxStatus {
    pub messages: Option<u32>,
    pub uidnext: Option<u32>,
    pub uidvalidity: Option<u32>,
    pub highestmodseq: Option<u64>,
}

fn apply_status(status: &mut MailboxStatus, attributes: &[async_imap::types::StatusAttribute]) {
    use async_imap::types::StatusAttribute;
    for attribute in attributes {
        match attribute {
            StatusAttribute::Messages(n) => status.messages = Some(*n),
            StatusAttribute::UidNext(n) => status.uidnext = Some(*n),
            StatusAttribute::UidValidity(n) => status.uidvalidity = Some(*n),
            StatusAttribute::HighestModSeq(n) => status.highestmodseq = Some(*n),
            _ => {}
        }
    }
}

/// LIST plus, on CONDSTORE servers, the STATUS of every mailbox in one round trip: LIST-STATUS
/// (RFC 5819) when offered, else LIST followed by pipelined STATUS commands.
pub async fn list_with_status(
    conn: &mut Connection,
    use_list_status: bool,
) -> Result<(Vec<ListEntry>, HashMap<String, MailboxStatus>), ImapError> {
    let mut statuses: HashMap<String, MailboxStatus> = HashMap::new();
    if !conn.caps.has("CONDSTORE") {
        return Ok((list(conn).await?, statuses));
    }
    let items = "MESSAGES UIDNEXT UIDVALIDITY HIGHESTMODSEQ";
    let mut entries = Vec::new();
    let mut on = |r: &Response<'_>| match r {
        Response::MailboxData(MailboxDatum::List(l)) => entries.push(ListEntry {
            name_raw: l.name.to_string(),
            delimiter: l.delimiter.as_ref().map(|d| d.to_string()),
            attributes: l.name_attributes.iter().map(attribute_name).collect(),
        }),
        Response::MailboxData(MailboxDatum::Status { mailbox, status }) => {
            apply_status(statuses.entry(mailbox.to_string()).or_default(), status);
        }
        _ => {}
    };
    // STATUS of the selected mailbox can be the session's stale view (Dovecot 2.4 answers
    // that way), so unselect first, pipelined with the LIST; without UNSELECT the selected
    // mailbox's STATUS is ignored below.
    let unselect = conn.selected.is_some() && conn.caps.has("UNSELECT");
    let stale = if unselect {
        None
    } else {
        conn.selected.clone()
    };
    if use_list_status && conn.caps.has("LIST-STATUS") {
        let list = format!("LIST \"\" \"*\" RETURN (STATUS ({items}))");
        if unselect {
            raw::run_pipelined(&mut conn.session, &["UNSELECT".to_owned(), list], &mut on).await?;
            conn.selected = None;
        } else {
            raw::run(&mut conn.session, &list, &mut on).await?;
        }
    } else {
        raw::run(&mut conn.session, "LIST \"\" \"*\"", &mut on).await?;
        let mut commands: Vec<String> = Vec::new();
        if unselect {
            commands.push("UNSELECT".to_owned());
        }
        commands.extend(
            entries
                .iter()
                .filter(|e| e.selectable())
                .map(|e| format!("STATUS {} ({items})", quote(&e.name_raw))),
        );
        let mut statuses_only = |r: &Response<'_>| {
            if let Response::MailboxData(MailboxDatum::Status { mailbox, status }) = r {
                apply_status(statuses.entry(mailbox.to_string()).or_default(), status);
            }
        };
        raw::run_pipelined(&mut conn.session, &commands, &mut statuses_only).await?;
        if unselect {
            conn.selected = None;
        }
    }
    if let Some(name) = stale {
        statuses.remove(&name);
    }
    Ok((entries, statuses))
}

fn unchanged(row: &MailboxRow, status: &MailboxStatus, local_count: u32) -> bool {
    row.state == MailboxState::Ready
        && status.uidvalidity.is_some()
        && status.uidvalidity == row.uidvalidity
        && status.uidnext == Some(row.uidnext)
        && status.highestmodseq == Some(row.highestmodseq)
        && status.messages == Some(local_count)
}

/// Lists, then runs a pass over every selectable mailbox whose STATUS differs from the stored
/// state (all of them on servers without CONDSTORE, where STATUS can't show flag changes). A
/// mailbox that disappeared between LIST and EXAMINE (NO) is skipped; the next LIST removes it.
pub async fn sync_account(
    conn: &mut Connection,
    shared: &Shared,
    options: &SyncOptions,
    stats: &mut Stats,
) -> Result<()> {
    let (entries, statuses) = list_with_status(conn, options.list_status).await?;
    stats.mailboxes_removed += shared.store().apply_list(&listed_mailboxes(&entries))? as u64;
    let rows: Vec<MailboxRow> = shared
        .store()
        .mailboxes()?
        .into_iter()
        .filter(|m| m.selectable)
        .collect();
    for row in rows {
        if let Some(elsewhere) = options.elsewhere
            && elsewhere.mailbox == row.name_raw
            && (!elsewhere.backstop || !statuses.contains_key(&row.name_raw))
        {
            continue;
        }
        if let Some(status) = statuses.get(&row.name_raw) {
            let local_count = shared.store().count(row.id)?;
            if unchanged(&row, status, local_count) {
                stats.status_skips += 1;
                continue;
            }
        }
        match sync_folder(conn, shared, row.id, options, stats).await {
            Ok(()) => {}
            Err(error) if is_missing_mailbox(&error) => stats.skipped_missing += 1,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// One pass over one mailbox: UIDVALIDITY check, initial sync if needed, then incremental.
pub async fn sync_folder(
    conn: &mut Connection,
    shared: &Shared,
    id: i64,
    options: &SyncOptions,
    stats: &mut Stats,
) -> Result<()> {
    let Some(row) = shared.store().mailbox(id)? else {
        return Ok(());
    };
    let lock = shared.folder_lock(&row.name_raw);
    let _guard = lock.lock().await;
    let Some(mut row) = shared.store().mailbox(id)? else {
        return Ok(());
    };
    if !row.selectable {
        return Ok(());
    }
    let mut view = examine(conn, &row.name_raw).await?;
    stats.passes += 1;
    if row.uidvalidity != Some(view.uidvalidity) {
        if row.uidvalidity.is_some() {
            stats.resets += 1;
        }
        shared.store().reset_mailbox(id, view.uidvalidity)?;
        row = reload(shared, id)?;
    }
    if row.state != MailboxState::Ready {
        initial(conn, shared, &row, &mut view, options, stats).await?;
        row = reload(shared, id)?;
    }
    incremental(conn, shared, &row, &mut view, options, stats).await
}

fn reload(shared: &Shared, id: i64) -> Result<MailboxRow> {
    shared
        .store()
        .mailbox(id)?
        .ok_or_else(|| anyhow::anyhow!("mailbox row vanished"))
}

/// Newest-first header sync in UID batches. Progress (`sync_floor`) is committed with every
/// batch, so an interrupted first sync resumes below the last committed batch. The state
/// recorded at the end is the UIDNEXT and HIGHESTMODSEQ from the *start*, so the incremental
/// pass that follows catches everything that changed meanwhile.
async fn initial(
    conn: &mut Connection,
    shared: &Shared,
    row: &MailboxRow,
    view: &mut View,
    options: &SyncOptions,
    stats: &mut Stats,
) -> Result<()> {
    let floor = match row.state {
        MailboxState::New => {
            let top = match view.uidnext {
                Some(n) => n,
                None => uid_search(conn, "ALL", view)
                    .await?
                    .last()
                    .map_or(1, |u| u + 1),
            };
            shared
                .store()
                .begin_initial(row.id, top, view.highestmodseq.unwrap_or(0))?;
            top
        }
        MailboxState::Initial { floor, .. } => floor,
        MailboxState::Ready => return Ok(()),
    };
    if floor > 1 {
        let mut uids = uid_search(conn, &format!("UID 1:{}", floor - 1), view).await?;
        uids.retain(|&u| u < floor);
        uids.reverse();
        for chunk in uids.chunks(options.batch.max(1)) {
            let wanted: HashSet<u32> = chunk.iter().copied().collect();
            let headers =
                fetch_headers(conn, &raw::uid_set(chunk), |u| wanted.contains(&u), view).await?;
            stats.headers_fetched += headers.len() as u64;
            stats.initial_batches += 1;
            let floor = chunk.iter().copied().min().unwrap_or(1);
            shared.store().commit(
                row.id,
                &Batch {
                    upserts: headers,
                    floor: Some(floor),
                    ..Batch::default()
                },
            )?;
        }
    }
    shared.store().commit(
        row.id,
        &Batch {
            finish_initial: true,
            ..Batch::default()
        },
    )?;
    Ok(())
}

/// New messages above the stored UIDNEXT, ascending, committing UIDNEXT with each batch.
async fn fetch_new(
    conn: &mut Connection,
    shared: &Shared,
    id: i64,
    from: u32,
    view: &mut View,
    options: &SyncOptions,
    stats: &mut Stats,
) -> Result<u32> {
    let gap = view.uidnext.map(|n| n.saturating_sub(from) as usize);
    let mut next = from;
    if gap.is_some_and(|g| g <= options.batch) {
        let headers = fetch_headers(conn, &format!("{from}:*"), |u| u >= from, view).await?;
        stats.new_message_fetches += 1;
        stats.headers_fetched += headers.len() as u64;
        next = headers
            .iter()
            .map(|h| h.uid + 1)
            .max()
            .unwrap_or(from)
            .max(view.uidnext.unwrap_or(from));
        shared.store().commit(
            id,
            &Batch {
                upserts: headers,
                uidnext: Some(next),
                ..Batch::default()
            },
        )?;
    } else {
        let uids: Vec<u32> = uid_search(conn, &format!("UID {from}:*"), view)
            .await?
            .into_iter()
            .filter(|&u| u >= from)
            .collect();
        for chunk in uids.chunks(options.batch.max(1)) {
            let wanted: HashSet<u32> = chunk.iter().copied().collect();
            let headers =
                fetch_headers(conn, &raw::uid_set(chunk), |u| wanted.contains(&u), view).await?;
            stats.new_message_fetches += 1;
            stats.headers_fetched += headers.len() as u64;
            next = chunk.iter().copied().max().map_or(next, |m| m + 1);
            shared.store().commit(
                id,
                &Batch {
                    upserts: headers,
                    uidnext: Some(next),
                    ..Batch::default()
                },
            )?;
        }
        if let Some(n) = view.uidnext.filter(|&n| n > next) {
            next = n;
            shared.store().commit(
                id,
                &Batch {
                    uidnext: Some(n),
                    ..Batch::default()
                },
            )?;
        }
    }
    Ok(next)
}

async fn incremental(
    conn: &mut Connection,
    shared: &Shared,
    row: &MailboxRow,
    view: &mut View,
    options: &SyncOptions,
    stats: &mut Stats,
) -> Result<()> {
    let id = row.id;
    let known_next = row.uidnext;
    let modseq = view.highestmodseq.filter(|_| conn.caps.has("CONDSTORE"));
    let local_count = shared.store().count(id)?;
    if view.uidnext == Some(known_next)
        && view.exists == local_count
        && modseq.is_some_and(|m| m == row.highestmodseq)
    {
        stats.fast_path += 1;
        return Ok(());
    }

    if view.uidnext.is_none_or(|n| n > known_next) {
        fetch_new(conn, shared, id, known_next, view, options, stats).await?;
    }

    let mut batch = Batch::default();
    if known_next > 1 {
        let range = format!("1:{}", known_next - 1);
        match modseq {
            Some(server) if server == row.highestmodseq => {}
            Some(_) => {
                let modifier = if conn.qresync {
                    format!("(CHANGEDSINCE {} VANISHED)", row.highestmodseq)
                } else {
                    format!("(CHANGEDSINCE {})", row.highestmodseq)
                };
                batch.flags = fetch_flags(
                    conn,
                    &format!("UID FETCH {range} (UID FLAGS) {modifier}"),
                    view,
                )
                .await?;
                stats.changedsince_fetches += 1;
            }
            None => {
                let server =
                    fetch_flags(conn, &format!("UID FETCH {range} (UID FLAGS)"), view).await?;
                stats.full_flag_scans += 1;
                let present: HashSet<u32> = server.iter().map(|f| f.uid).collect();
                let local = shared.store().flag_map(id)?;
                batch.deletes = local
                    .keys()
                    .copied()
                    .filter(|u| *u < known_next && !present.contains(u))
                    .collect();
                batch.flags = server
                    .into_iter()
                    .filter(|f| {
                        local
                            .get(&f.uid)
                            .is_some_and(|(bits, kw)| *bits != f.flags || *kw != f.keywords)
                    })
                    .collect();
            }
        }
    }
    stats.vanished_uids += view.vanished.len() as u64;
    if options.sabotage == Sabotage::None {
        batch.deletes.append(&mut view.vanished);
    }
    view.vanished.clear();
    stats.flag_updates += batch.flags.len() as u64;
    stats.deletes += batch.deletes.len() as u64;
    batch.highestmodseq = modseq;
    shared.store().commit(id, &batch)?;

    if options.sabotage == Sabotage::IgnoreVanishedNoCountCheck {
        return Ok(());
    }
    for _ in 0..3 {
        let local = shared.store().count(id)?;
        if local == view.exists {
            return Ok(());
        }
        stats.count_mismatches += 1;
        if local < view.exists {
            let next = reload(shared, id)?.uidnext;
            fetch_new(conn, shared, id, next, view, options, stats).await?;
            if shared.store().count(id)? == view.exists {
                return Ok(());
            }
        }
        uid_diff(conn, shared, id, view, stats).await?;
    }
    Ok(())
}

/// The fallback that needs nothing but IMAP4rev1: the server's UID list against ours.
async fn uid_diff(
    conn: &mut Connection,
    shared: &Shared,
    id: i64,
    view: &mut View,
    stats: &mut Stats,
) -> Result<()> {
    stats.uid_diffs += 1;
    view.vanished.clear();
    let server = uid_search(conn, "ALL", view).await?;
    let server_set: HashSet<u32> = server.iter().copied().collect();
    let local = shared.store().uids(id)?;
    let local_set: HashSet<u32> = local.iter().copied().collect();
    let deletes: Vec<u32> = local
        .into_iter()
        .filter(|u| !server_set.contains(u))
        .collect();
    let missing: Vec<u32> = server
        .iter()
        .copied()
        .filter(|u| !local_set.contains(u))
        .collect();
    let mut upserts = Vec::new();
    if !missing.is_empty() {
        let wanted: HashSet<u32> = missing.iter().copied().collect();
        upserts =
            fetch_headers(conn, &raw::uid_set(&missing), |u| wanted.contains(&u), view).await?;
        stats.headers_fetched += upserts.len() as u64;
    }
    stats.deletes += deletes.len() as u64;
    let uidnext = server.last().map(|u| u + 1);
    shared.store().commit(
        id,
        &Batch {
            upserts,
            deletes,
            uidnext,
            ..Batch::default()
        },
    )?;
    view.exists = server.len() as u32;
    view.vanished.clear();
    Ok(())
}

/// The server's state, read independently of the sync code: every selectable mailbox with its
/// UIDVALIDITY and each message's UID, flags and Message-ID.
pub async fn server_snapshot(conn: &mut Connection) -> Result<BTreeMap<String, FolderSnapshot>> {
    let mut out = BTreeMap::new();
    for entry in list(conn).await?.into_iter().filter(ListEntry::selectable) {
        let mut view = examine(conn, &entry.name_raw).await?;
        let mut messages = BTreeMap::new();
        if view.exists > 0 {
            raw::run(
                &mut conn.session,
                "UID FETCH 1:* (UID FLAGS BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])",
                |r| {
                    if let Response::Fetch(_, attrs) = r
                        && let Some(update) = fetch::flags(attrs)
                    {
                        messages.insert(
                            update.uid,
                            (update.flags, update.keywords, fetch::message_id(attrs)),
                        );
                    } else {
                        view.track(r);
                    }
                },
            )
            .await?;
        }
        out.insert(
            mutf7::display_name(&entry.name_raw),
            FolderSnapshot {
                uidvalidity: Some(view.uidvalidity),
                messages,
            },
        );
    }
    Ok(out)
}

/// Differences between the server and the local cache, as short descriptions (synthetic folder
/// names and UIDs only).
pub fn diff(
    server: &BTreeMap<String, FolderSnapshot>,
    local: &BTreeMap<String, FolderSnapshot>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (name, s) in server {
        let Some(l) = local.get(name) else {
            out.push(format!(
                "{name}: missing locally ({} messages)",
                s.messages.len()
            ));
            continue;
        };
        if s.uidvalidity != l.uidvalidity {
            out.push(format!(
                "{name}: uidvalidity {:?} != local {:?}",
                s.uidvalidity, l.uidvalidity
            ));
        }
        for (uid, value) in &s.messages {
            match l.messages.get(uid) {
                None => out.push(format!("{name}: uid {uid} missing locally")),
                Some(local_value) if local_value != value => out.push(format!(
                    "{name}: uid {uid} server {value:?} local {local_value:?}"
                )),
                Some(_) => {}
            }
        }
        for uid in l.messages.keys().filter(|u| !s.messages.contains_key(u)) {
            out.push(format!("{name}: uid {uid} only local"));
        }
    }
    for name in local.keys().filter(|n| !server.contains_key(*n)) {
        out.push(format!("{name}: only local"));
    }
    out
}

/// Keywords and flags as IMAP flag atoms, for STORE commands in tests.
pub fn flag_atoms(bits: u32, keywords: &str) -> Vec<String> {
    let mut out: Vec<String> = [
        (crate::store::SEEN, "\\Seen"),
        (crate::store::ANSWERED, "\\Answered"),
        (crate::store::FLAGGED, "\\Flagged"),
        (crate::store::DELETED, "\\Deleted"),
        (crate::store::DRAFT, "\\Draft"),
    ]
    .into_iter()
    .filter(|(bit, _)| bits & bit != 0)
    .map(|(_, name)| name.to_owned())
    .collect();
    out.extend(keywords.split_whitespace().map(str::to_owned));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn view_tracks_exists_expunge_and_vanished() {
        let mut view = View {
            exists: 10,
            ..View::default()
        };
        view.track(&Response::MailboxData(MailboxDatum::Exists(12)));
        view.track(&Response::Expunge(3));
        view.track(&Response::Vanished {
            earlier: false,
            uids: vec![4..=5],
        });
        view.track(&Response::Vanished {
            earlier: true,
            uids: vec![1..=1],
        });
        assert_eq!(view.exists, 9);
        assert_eq!(view.vanished, vec![4, 5, 1]);
    }

    #[test]
    fn listed_mailboxes_decode_and_assign_roles() {
        let entry = |name: &str, attrs: &[&str]| ListEntry {
            name_raw: name.into(),
            delimiter: Some("/".into()),
            attributes: attrs.iter().map(|a| a.to_string()).collect(),
        };
        let listed = listed_mailboxes(&[
            entry("INBOX", &[]),
            entry("S&APg-ppelpost", &["\\HasNoChildren"]),
            entry("Sendt", &["\\Sent"]),
            entry("Prosjekter", &["\\Noselect"]),
        ]);
        assert_eq!(listed[1].name, "Søppelpost");
        assert_eq!(listed[1].role.as_deref(), Some("junk"));
        assert_eq!(listed[2].role.as_deref(), Some("sent"));
        assert!(!listed[3].selectable);
    }

    #[test]
    fn flag_atoms_round_trip() {
        let (bits, keywords) = crate::store::split_flags(["\\Seen", "\\Draft", "$Label1"]);
        assert_eq!(
            flag_atoms(bits, &keywords),
            vec!["\\Seen", "\\Draft", "$Label1"]
        );
    }
}
