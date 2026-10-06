//! The cache against a server, for the sync suites: every synced folder must hold exactly what
//! the server holds from the folder's floor up (UID, flags, keywords and Message-ID) under the
//! same UIDVALIDITY, and nothing below the floor. Folders the policy leaves unsynced are only
//! checked for presence. The counts a synced folder shows must be the server's, below the floor
//! included. Names are the testbed's and the fake server's synthetic ones.

#![allow(dead_code)]

use sealwax_domain::model::{AccountId, Flags};
use sealwax_infrastructure::store::{Store, SyncState};
use std::collections::{BTreeMap, HashMap};

/// Flags, keywords and Message-ID.
pub type Entry = (Flags, Vec<String>, Option<String>);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Folder {
    pub uidvalidity: Option<u32>,
    /// The lowest UID the cache is expected to hold (1 on the server's side).
    pub floor: u32,
    /// The cache syncs this folder (it has a UIDVALIDITY and its first sync is done).
    pub synced: bool,
    pub messages: BTreeMap<u32, Entry>,
}

/// The cache's side, keyed by wire name.
pub async fn local(store: &Store, account: AccountId) -> BTreeMap<String, Folder> {
    let mut out = BTreeMap::new();
    for state in store.account_folders(account).recv().await.unwrap() {
        if !state.selectable {
            continue;
        }
        let mut messages = BTreeMap::new();
        for stored in store.folder_messages(state.folder).recv().await.unwrap() {
            let message_id = store
                .message(stored.id)
                .recv()
                .await
                .unwrap()
                .value
                .and_then(|detail| detail.headers.message_id);
            messages.insert(stored.uid, (stored.flags, stored.keywords, message_id));
        }
        out.insert(
            state.name_raw.clone(),
            Folder {
                uidvalidity: state.uidvalidity,
                floor: state.floor.unwrap_or(1).max(1),
                synced: state.uidvalidity.is_some() && state.state == SyncState::Ready,
                messages,
            },
        );
    }
    out
}

/// Differences, as short descriptions of synthetic names and UIDs.
pub fn diff(server: &BTreeMap<String, Folder>, local: &BTreeMap<String, Folder>) -> Vec<String> {
    let mut out = Vec::new();
    for (name, theirs) in server {
        let Some(ours) = local.get(name) else {
            out.push(format!("{name}: missing locally"));
            continue;
        };
        if !ours.synced {
            if !ours.messages.is_empty() {
                out.push(format!("{name}: holds messages but isn't synced"));
            }
            continue;
        }
        if ours.uidvalidity != theirs.uidvalidity {
            out.push(format!(
                "{name}: uidvalidity {:?}, local {:?}",
                theirs.uidvalidity, ours.uidvalidity
            ));
            continue;
        }
        for (uid, entry) in theirs.messages.range(ours.floor..) {
            match ours.messages.get(uid) {
                None => out.push(format!("{name}: uid {uid} missing locally")),
                Some(held) if held != entry => {
                    out.push(format!("{name}: uid {uid} server {entry:?} local {held:?}"));
                }
                Some(_) => {}
            }
        }
        for uid in ours.messages.keys() {
            if *uid < ours.floor {
                out.push(format!("{name}: uid {uid} below the floor {}", ours.floor));
            } else if !theirs.messages.contains_key(uid) {
                out.push(format!("{name}: uid {uid} only local"));
            }
        }
    }
    for name in local.keys().filter(|name| !server.contains_key(*name)) {
        out.push(format!("{name}: only local"));
    }
    out
}

/// The counts the sidebar shows for the account's folders, keyed by wire name: total and unread.
pub async fn shown(store: &Store, account: AccountId) -> BTreeMap<String, (u32, u32)> {
    let names: HashMap<_, _> = store
        .account_folders(account)
        .recv()
        .await
        .unwrap()
        .into_iter()
        .map(|state| (state.folder, state.name_raw))
        .collect();
    store
        .folders()
        .recv()
        .await
        .unwrap()
        .value
        .into_iter()
        .filter_map(|folder| {
            let name = names.get(&folder.id)?;
            Some((name.clone(), (folder.total, folder.unread)))
        })
        .collect()
}

/// Synced folders whose shown counts aren't the server's: all its messages, and those without
/// `\Seen`.
pub fn count_diff(
    server: &BTreeMap<String, Folder>,
    local: &BTreeMap<String, Folder>,
    shown: &BTreeMap<String, (u32, u32)>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (name, theirs) in server {
        if !local.get(name).is_some_and(|ours| ours.synced) {
            continue;
        }
        let unread = theirs
            .messages
            .values()
            .filter(|(flags, ..)| !flags.is_seen())
            .count();
        let expected = (theirs.messages.len() as u32, unread as u32);
        let got = shown.get(name).copied();
        if got != Some(expected) {
            out.push(format!("{name}: server counts {expected:?}, shown {got:?}"));
        }
    }
    out
}

/// Flags and keywords as IMAP flag atoms, for STORE commands.
pub fn atoms(flags: Flags, keywords: &[String]) -> Vec<String> {
    let mut out: Vec<String> = [
        (Flags::SEEN, "\\Seen"),
        (Flags::ANSWERED, "\\Answered"),
        (Flags::FLAGGED, "\\Flagged"),
        (Flags::DELETED, "\\Deleted"),
        (Flags::DRAFT, "\\Draft"),
    ]
    .into_iter()
    .filter(|(bit, _)| flags.contains(*bit))
    .map(|(_, name)| name.to_owned())
    .collect();
    out.extend(keywords.iter().cloned());
    out
}

/// SplitMix64: seeds replay identically whatever the crate versions.
#[derive(Clone, Debug)]
pub struct Rng(pub u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }

    pub fn chance(&mut self, percent: u64) -> bool {
        self.next_u64() % 100 < percent
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}
