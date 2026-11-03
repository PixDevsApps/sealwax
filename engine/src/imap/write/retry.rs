//! A move retry's Message-ID match is only a candidate: a destination message can share a
//! source's Message-ID and still be another message. Before a source that is still there is
//! deleted, its candidate must match in two stages, each batched per folder:
//!
//! 1. RFC822.SIZE and INTERNALDATE, one `UID FETCH` for the sources and one for the targets.
//!    COPY and MOVE keep the internal date (RFC 3501, RFC 6851); a server that doesn't only
//!    costs a duplicate. A pair that differs here is moved again without downloading anything.
//! 2. A SHA-256 of the whole message, read through `BODY.PEEK[]<offset.count>` 64 KiB at a time
//!    for all of a folder's pairs per command, up to 16 MiB a message.
//!
//! A pair that differs, can't be read or is over the limit is moved again: a duplicate rather
//! than a lost message.

use super::{
    AttributeValue, Connection, ImapError, OpFolder, Response, Selected, WriteError, batches, open,
    raw, view,
};
use crate::imap::fetch::internal_date;
use sha2::{Digest, Sha256};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

#[cfg(test)]
mod tests;

const CHUNK_BYTES: u32 = 64 * 1024;
/// Limit the extra work of an uncertain move. Larger or unreadable messages are moved again
/// rather than deduplicated by their Message-ID alone.
const MAX_COMPARE_BYTES: u32 = 16 * 1024 * 1024;

/// What [`verify`] found, with the source selected again.
pub(super) struct Verified {
    pub same: HashSet<(u32, u32)>,
    pub selected: Selected,
}

/// The `(source, target)` pairs whose source in `from`, the selected folder, has the same
/// contents as its target in `to`. Ends with `from` selected again and its UIDVALIDITY checked.
pub(super) async fn verify(
    conn: &mut Connection,
    from: OpFolder<'_>,
    to: &str,
    target_uidvalidity: u32,
    pairs: &[(u32, u32)],
) -> Result<Verified, WriteError> {
    let sources = metadata(conn, &pairs.iter().map(|pair| pair.0).collect::<Vec<_>>()).await?;
    let mut same = HashSet::new();
    if view::examine(conn, to).await?.uidvalidity == target_uidvalidity {
        let targets = metadata(conn, &pairs.iter().map(|pair| pair.1).collect::<Vec<_>>()).await?;
        let alike: Vec<(u32, u32)> = pairs
            .iter()
            .copied()
            .filter(
                |(source, target)| match (sources.get(source), targets.get(target)) {
                    (Some(source), Some(target)) => {
                        source == target && source.size <= MAX_COMPARE_BYTES
                    }
                    _ => false,
                },
            )
            .collect();
        if !alike.is_empty() {
            let targets =
                hashes(conn, &alike.iter().map(|pair| pair.1).collect::<Vec<_>>()).await?;
            let alike: Vec<(u32, u32)> = alike
                .into_iter()
                .filter(|(_, target)| targets.contains_key(target))
                .collect();
            let selected = open(conn, from).await?;
            let sources =
                hashes(conn, &alike.iter().map(|pair| pair.0).collect::<Vec<_>>()).await?;
            same = alike
                .into_iter()
                .filter(|(source, target)| {
                    sources
                        .get(source)
                        .is_some_and(|hash| targets.get(target) == Some(hash))
                })
                .collect();
            return Ok(Verified { same, selected });
        }
    }
    let selected = open(conn, from).await?;
    Ok(Verified { same, selected })
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Meta {
    size: u32,
    date: i64,
}

/// RFC822.SIZE and INTERNALDATE of `uids` in the selected folder. A UID the server doesn't
/// answer for, answers twice for differently, or gives an unreadable date for is left out.
async fn metadata(conn: &mut Connection, uids: &[u32]) -> Result<HashMap<u32, Meta>, ImapError> {
    let wanted: HashSet<u32> = uids.iter().copied().collect();
    let mut found: HashMap<u32, Option<Meta>> = HashMap::new();
    for batch in batches(uids) {
        let command = format!("UID FETCH {} (UID RFC822.SIZE INTERNALDATE)", batch.set);
        raw::run(&mut conn.session, &command, |response| {
            let Response::Fetch(_, attributes) = response else {
                return;
            };
            let (mut uid, mut size, mut date) = (None, None, None);
            for attribute in attributes {
                match attribute {
                    AttributeValue::Uid(value) => uid = Some(*value),
                    AttributeValue::Rfc822Size(value) => size = Some(*value),
                    AttributeValue::InternalDate(value) => date = internal_date(value),
                    _ => {}
                }
            }
            let Some(uid) = uid.filter(|uid| wanted.contains(uid)) else {
                return;
            };
            let meta = size.zip(date).map(|(size, date)| Meta { size, date });
            match found.entry(uid) {
                Entry::Vacant(entry) => {
                    entry.insert(meta);
                }
                Entry::Occupied(mut entry) if *entry.get() != meta => {
                    entry.insert(None);
                }
                Entry::Occupied(_) => {}
            }
        })
        .await?;
    }
    Ok(found
        .into_iter()
        .filter_map(|(uid, meta)| Some((uid, meta?)))
        .collect())
}

/// SHA-256 of each of `uids` whole in the selected folder, read [`CHUNK_BYTES`] at a time for
/// all of them per command. One byte beyond [`MAX_COMPARE_BYTES`] is asked for, so a message of
/// exactly that size is told from a longer one. A UID whose part is missing, at another offset,
/// longer than asked for or answered twice, or that is over the limit, is left out.
async fn hashes(conn: &mut Connection, uids: &[u32]) -> Result<HashMap<u32, [u8; 32]>, ImapError> {
    let mut reading: HashMap<u32, Sha256> = uids.iter().map(|uid| (*uid, Sha256::new())).collect();
    let mut done = HashMap::new();
    let mut offset = 0;
    while !reading.is_empty() {
        let count = CHUNK_BYTES.min(MAX_COMPARE_BYTES - offset + 1);
        let pending: Vec<u32> = reading.keys().copied().collect();
        let mut parts: HashMap<u32, Option<Vec<u8>>> = HashMap::new();
        for batch in batches(&pending) {
            let command = format!(
                "UID FETCH {} (UID BODY.PEEK[]<{offset}.{count}>)",
                batch.set
            );
            raw::run(&mut conn.session, &command, |response| {
                let Response::Fetch(_, attributes) = response else {
                    return;
                };
                let Some(uid) = attributes.iter().find_map(|attribute| match attribute {
                    AttributeValue::Uid(uid) if reading.contains_key(uid) => Some(*uid),
                    _ => None,
                }) else {
                    return;
                };
                for attribute in attributes {
                    if let AttributeValue::BodySection {
                        section: None,
                        index,
                        data,
                    } = attribute
                    {
                        let part = data
                            .as_deref()
                            .filter(|bytes| *index == Some(offset) && bytes.len() <= count as usize)
                            .map(<[u8]>::to_vec);
                        match parts.entry(uid) {
                            Entry::Vacant(entry) => {
                                entry.insert(part);
                            }
                            Entry::Occupied(mut entry) => {
                                entry.insert(None);
                            }
                        }
                    }
                }
            })
            .await?;
        }
        for uid in pending {
            let Some(mut hash) = reading.remove(&uid) else {
                continue;
            };
            let Some(Some(part)) = parts.remove(&uid) else {
                continue;
            };
            hash.update(&part);
            if part.len() < count as usize {
                done.insert(uid, hash.finalize().into());
            } else if offset + count <= MAX_COMPARE_BYTES {
                reading.insert(uid, hash);
            }
        }
        offset += count;
    }
    Ok(done)
}
