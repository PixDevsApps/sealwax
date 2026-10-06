//! Putting a message into a folder (ML-077, ML-074; ADR-015 and its S1 amendment): the Sent copy
//! and a draft's server copy, appended at most once. The folder's mark (UIDVALIDITY and
//! UIDNEXT, [`super::mark_target`]) is taken before the first attempt and kept by the caller; a
//! retry first reads the Message-IDs of the folder's UIDs from the mark up and appends only when
//! the message isn't there (S1: a lost APPEND reply retried blindly made two copies, and
//! Stalwart's `SEARCH HEADER Message-ID` never matches, so it can't be the check). The new UID
//! comes from APPENDUID, else from the same Message-IDs.
//!
//! - [`append`]: APPEND with a synchronizing literal and the given flags.
//! - [`replace_draft`]: a draft's new revision, then its older copies removed (`\Deleted` and
//!   UID EXPUNGE with UIDPLUS; without it they stay flagged `\Deleted`).
//! - [`find`]: a message's UIDs in a folder by Message-ID, for the Sent copy a server may have
//!   saved itself.

use super::{
    Code, Connection, Done, ImapError, Mark, OpFolder, View, WriteError, batches, delete,
    fetched_id, names, open, quote, raw, view, window,
};
use async_imap::imap_proto::Response;
use sealwax_domain::message;
use sealwax_domain::model::Flags;
use std::fmt;
use std::time::Instant;

/// How many of a folder's newest messages [`find`] reads when the server's SEARCH finds nothing.
pub const FIND_SCAN: u32 = 200;

/// A message to put into a folder.
#[derive(Clone, Copy)]
pub struct Append<'a> {
    /// The folder's wire name.
    pub folder: &'a str,
    /// The message as it is stored, CRLF line ends.
    pub message: &'a [u8],
    /// Its Message-ID, with or without angle brackets, by which a retry finds an earlier
    /// attempt's copy.
    pub message_id: &'a str,
    /// `\Seen` for the Sent copy; `\Draft` and `\Seen` for a draft. Only system flags are sent.
    pub flags: Flags,
    /// The folder before the first attempt, which the caller keeps with the attempt: a retry
    /// looks among the UIDs from it up (from UID 1 when the UIDVALIDITY changed since), so a
    /// draft's older revisions below it are never taken for this one.
    pub mark: Mark,
    /// An earlier attempt may have stored the message.
    pub retry: bool,
}

impl fmt::Debug for Append<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Append")
            .field("bytes", &self.message.len())
            .field("flags", &self.flags)
            .field("mark", &self.mark)
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

/// Where an appended message is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Appended {
    /// The folder's UIDVALIDITY that `uid` belongs to; `None` when neither APPENDUID nor a
    /// look at the folder told it.
    pub uidvalidity: Option<u32>,
    /// The message's UID, from APPENDUID or found by Message-ID from the mark up; `None` when
    /// it isn't known (the folder's sync finds it as new mail).
    pub uid: Option<u32>,
    /// An earlier attempt had stored it, and nothing was appended now.
    pub earlier: bool,
}

/// The flags of an APPEND command: the system flags of `flags`.
fn flag_list(flags: Flags) -> String {
    names(flags)
        .into_iter()
        .filter(|name| name.starts_with('\\') && *name != "\\Deleted")
        .collect::<Vec<_>>()
        .join(" ")
}

/// The UIDs from the mark up whose Message-ID is `id`, and the folder's UIDVALIDITY.
async fn look(
    conn: &mut Connection,
    folder: &str,
    id: &str,
    mark: Mark,
) -> Result<(u32, Vec<u32>), WriteError> {
    let found = window(conn, folder, Some(mark)).await?;
    let uids = found
        .entries
        .iter()
        .filter(|(_, entry)| entry == id)
        .map(|(uid, _)| *uid)
        .collect();
    Ok((found.uidvalidity, uids))
}

/// Appends `request.message` to its folder, at most once across retries. A retry first looks
/// for the message by Message-ID from the mark up and appends nothing when it is there
/// ([`Appended::earlier`]). The new UID comes from APPENDUID, else from the same look after the
/// APPEND.
pub async fn append(conn: &mut Connection, request: Append<'_>) -> Result<Appended, WriteError> {
    let started = Instant::now();
    let id = message::normalize_id(request.message_id).ok_or(WriteError::Imap(
        ImapError::Protocol("an append without a Message-ID"),
    ))?;
    if request.retry {
        let (uidvalidity, uids) = look(conn, request.folder, &id, request.mark).await?;
        if let Some(uid) = uids.first() {
            let appended = Appended {
                uidvalidity: Some(uidvalidity),
                uid: Some(*uid),
                earlier: true,
            };
            logged(&request, &appended, uids.len(), started);
            return Ok(appended);
        }
    }
    let command = format!(
        "APPEND {} ({}) {{{}}}",
        quote(request.folder)?,
        flag_list(request.flags),
        request.message.len()
    );
    let code = raw::run_literal(&mut conn.session, &command, request.message, |_| {}).await?;
    let appended = match code {
        Some(Code::AppendUid { uidvalidity, uid }) => Appended {
            uidvalidity: Some(uidvalidity),
            uid: Some(uid),
            earlier: false,
        },
        _ => {
            let (uidvalidity, uids) = look(conn, request.folder, &id, request.mark).await?;
            Appended {
                uidvalidity: Some(uidvalidity),
                uid: uids.last().copied(),
                earlier: false,
            }
        }
    };
    logged(&request, &appended, 1, started);
    Ok(appended)
}

fn logged(request: &Append<'_>, appended: &Appended, copies: usize, started: Instant) {
    tracing::debug!(
        bytes = request.message.len(),
        retry = request.retry,
        earlier = appended.earlier,
        uid_known = appended.uid.is_some(),
        copies,
        ms = started.elapsed().as_secs_f64() * 1000.0,
        "imap append done"
    );
}

/// A draft's older copies in its folder: the UIDs the store knows, under their UIDVALIDITY.
#[derive(Clone, Copy, Debug)]
pub struct OldCopies<'a> {
    pub uidvalidity: u32,
    pub uids: &'a [u32],
}

/// What [`replace_draft`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Replaced {
    pub appended: Option<Appended>,
    /// Older copies flagged `\Deleted` and expunged by UID (UIDPLUS), or, without UIDPLUS,
    /// left flagged `\Deleted` (`done.left_deleted`); UIDs already gone count too.
    pub removed: Vec<u32>,
    pub done: Done,
}

/// Puts a draft's new revision into its folder ([`append`], `\Draft` and `\Seen` as the request
/// says), then removes the older copies the store knows. Safe to run again: the retry finds the
/// revision by its mark, and removing a copy twice changes nothing. Older copies under another
/// UIDVALIDITY are no longer named by their UIDs and are left alone.
pub async fn replace_draft(
    conn: &mut Connection,
    request: Append<'_>,
    old: Option<OldCopies<'_>>,
) -> Result<Replaced, WriteError> {
    let appended = append(conn, request).await?;
    let mut replaced = Replaced {
        appended: Some(appended),
        ..Replaced::default()
    };
    let Some(old) = old else {
        return Ok(replaced);
    };
    if appended
        .uidvalidity
        .is_some_and(|now| now != old.uidvalidity)
    {
        return Ok(replaced);
    }
    let uids: Vec<u32> = old
        .uids
        .iter()
        .copied()
        .filter(|uid| *uid > 0 && Some(*uid) != appended.uid)
        .collect();
    if uids.is_empty() {
        return Ok(replaced);
    }
    let folder = OpFolder {
        name_raw: request.folder,
        uidvalidity: old.uidvalidity,
    };
    match open(conn, folder).await {
        Ok(_) => {}
        Err(WriteError::UidValidity { .. }) => return Ok(replaced),
        Err(error) => return Err(error),
    }
    delete(conn, &uids, &mut replaced.done).await?;
    replaced.removed = uids;
    Ok(replaced)
}

/// Where [`find`] found a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub uidvalidity: u32,
    pub uids: Vec<u32>,
}

/// The UIDs in `folder` whose Message-ID is `message_id`. With `since`, the UIDs from that mark
/// up (one FETCH of their Message-IDs; the Sent copy's check after a send). Without it, `UID
/// SEARCH HEADER Message-ID`, every hit checked against its header (a substring match on
/// Dovecot), and when that finds nothing, the newest [`FIND_SCAN`] messages read, because
/// Stalwart's SEARCH never matches a Message-ID (S1).
pub async fn find(
    conn: &mut Connection,
    folder: &str,
    message_id: &str,
    since: Option<Mark>,
) -> Result<Found, WriteError> {
    let started = Instant::now();
    let Some(id) = message::normalize_id(message_id) else {
        return Ok(Found {
            uidvalidity: view::examine(conn, folder).await?.uidvalidity,
            uids: Vec::new(),
        });
    };
    let found = match since {
        Some(mark) => {
            let (uidvalidity, uids) = look(conn, folder, &id, mark).await?;
            Found { uidvalidity, uids }
        }
        None => search(conn, folder, &id).await?,
    };
    tracing::debug!(
        marked = since.is_some(),
        found = found.uids.len(),
        ms = started.elapsed().as_secs_f64() * 1000.0,
        "imap find done"
    );
    Ok(found)
}

/// Exact Message-ID matches among `fetch`'s answers.
async fn matching(conn: &mut Connection, fetch: &str, id: &str) -> Result<Vec<u32>, WriteError> {
    let mut uids = Vec::new();
    raw::run(&mut conn.session, fetch, |response| {
        if let Response::Fetch(_, attributes) = response
            && let Some((uid, found)) = fetched_id(attributes)
            && found == id
        {
            uids.push(uid);
        }
    })
    .await?;
    uids.sort_unstable();
    uids.dedup();
    Ok(uids)
}

async fn search(conn: &mut Connection, folder: &str, id: &str) -> Result<Found, WriteError> {
    let mut view: View = view::examine(conn, folder).await?;
    let uidvalidity = view.uidvalidity;
    if view.exists == 0 {
        return Ok(Found {
            uidvalidity,
            uids: Vec::new(),
        });
    }
    let criteria = format!("HEADER Message-ID {}", quote(&format!("<{id}>"))?);
    let hits = view::uid_search(conn, &criteria, &mut view).await?;
    let mut uids = Vec::new();
    for batch in batches(&hits) {
        let fetch = format!(
            "UID FETCH {} (UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])",
            batch.set
        );
        uids.extend(matching(conn, &fetch, id).await?);
    }
    if uids.is_empty() {
        let first = view.exists.saturating_sub(FIND_SCAN - 1).max(1);
        let fetch = format!("FETCH {first}:* (UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])");
        uids = matching(conn, &fetch, id).await?;
    }
    uids.sort_unstable();
    uids.dedup();
    Ok(Found { uidvalidity, uids })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_system_flags_go_into_an_append() {
        assert_eq!(flag_list(Flags::SEEN), "\\Seen");
        assert_eq!(flag_list(Flags::DRAFT.union(Flags::SEEN)), "\\Seen \\Draft");
        assert_eq!(
            flag_list(Flags::JUNK.union(Flags::DELETED).union(Flags::FORWARDED)),
            ""
        );
    }
}
