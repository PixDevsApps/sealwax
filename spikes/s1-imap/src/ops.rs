//! Writes: APPEND and MOVE, then finding the new UID by Message-ID, because async-imap's
//! `append` and `uid_mv` return `()` and drop APPENDUID/COPYUID. The raw variants keep those
//! codes, for comparison.

use async_imap::imap_proto::Response;

use crate::net::Connection;
use crate::raw::{self, CopyUid, ImapError, quote};
use crate::sync::{self, View};

pub async fn select(conn: &mut Connection, name_raw: &str) -> Result<View, ImapError> {
    conn.selected = None;
    let mut view = View::default();
    raw::run(
        &mut conn.session,
        &format!("SELECT {}", quote(name_raw)),
        |r| {
            if let Response::MailboxData(async_imap::imap_proto::MailboxDatum::Exists(n)) = r {
                view.exists = *n;
            }
        },
    )
    .await?;
    conn.selected = Some(name_raw.to_owned());
    Ok(view)
}

/// UIDs in `name_raw` whose Message-ID header contains `<message_id>`.
pub async fn find_by_message_id(
    conn: &mut Connection,
    name_raw: &str,
    message_id: &str,
) -> Result<Vec<u32>, ImapError> {
    let mut view = if conn.selected.as_deref() == Some(name_raw) {
        View::default()
    } else {
        sync::examine(conn, name_raw).await?
    };
    sync::uid_search(
        conn,
        &format!("HEADER Message-ID {}", quote(&format!("<{message_id}>"))),
        &mut view,
    )
    .await
}

/// APPEND through async-imap (synchronizing literal, result `()`), then the UID by search.
pub async fn append_library(
    conn: &mut Connection,
    name_raw: &str,
    flags: &str,
    message: &[u8],
    message_id: &str,
) -> Result<Option<u32>, ImapError> {
    conn.session
        .append(name_raw, Some(&format!("({flags})")), None, message)
        .await?;
    Ok(find_by_message_id(conn, name_raw, message_id)
        .await?
        .last()
        .copied())
}

/// APPEND by the raw layer: returns APPENDUID's UID when the server sends it.
pub async fn append_raw(
    conn: &mut Connection,
    name_raw: &str,
    flags: &str,
    message: &[u8],
) -> Result<Option<u32>, ImapError> {
    let literal_plus = conn.caps.has("LITERAL+");
    let completion = raw::append(
        &mut conn.session,
        name_raw,
        flags,
        message,
        literal_plus,
        |_| {},
    )
    .await?;
    Ok(completion
        .append_uid
        .and_then(|(_, uids)| raw::expand(&uids).last().copied()))
}

/// The op-queue form of APPEND. On a retry (`attempt > 0`, i.e. the outcome of an earlier
/// attempt is unknown) it searches first and appends only if the message is not there.
/// Returns the UID and whether the APPEND was actually sent.
pub async fn append_once(
    conn: &mut Connection,
    name_raw: &str,
    flags: &str,
    message: &[u8],
    message_id: &str,
    attempt: u32,
) -> Result<(Option<u32>, bool), ImapError> {
    if attempt > 0
        && let Some(uid) = find_by_message_id(conn, name_raw, message_id)
            .await?
            .last()
            .copied()
    {
        return Ok((Some(uid), false));
    }
    Ok((
        append_library(conn, name_raw, flags, message, message_id).await?,
        true,
    ))
}

/// UID MOVE through async-imap (result `()`), falling back to COPY + \Deleted + UID EXPUNGE
/// without MOVE (plain EXPUNGE without UIDPLUS, which also removes other \Deleted messages);
/// then the destination UID by search. A UID that no longer exists is a no-op for the server,
/// so a replayed MOVE is harmless.
pub async fn move_library(
    conn: &mut Connection,
    from_raw: &str,
    uid: u32,
    to_raw: &str,
    message_id: &str,
) -> Result<Option<u32>, ImapError> {
    select(conn, from_raw).await?;
    if conn.caps.has("MOVE") {
        conn.session.uid_mv(uid.to_string(), to_raw).await?;
    } else {
        raw::run(
            &mut conn.session,
            &format!("UID COPY {uid} {}", quote(to_raw)),
            |_| {},
        )
        .await?;
        raw::run(
            &mut conn.session,
            &format!("UID STORE {uid} +FLAGS.SILENT (\\Deleted)"),
            |_| {},
        )
        .await?;
        let expunge = if conn.caps.has("UIDPLUS") {
            format!("UID EXPUNGE {uid}")
        } else {
            "EXPUNGE".to_owned()
        };
        raw::run(&mut conn.session, &expunge, |_| {}).await?;
    }
    Ok(find_by_message_id(conn, to_raw, message_id)
        .await?
        .last()
        .copied())
}

/// The op-queue form of MOVE. On a retry it first looks for the message in the target by
/// Message-ID; if it is there the move happened (or the COPY half of a COPY-based move did),
/// so only a leftover source copy is removed. Message-ID is not unique in general, so a
/// production op must prefer COPYUID or an OBJECTID when the server offers them.
pub async fn move_once(
    conn: &mut Connection,
    from_raw: &str,
    uid: u32,
    to_raw: &str,
    message_id: &str,
    attempt: u32,
) -> Result<Option<u32>, ImapError> {
    if attempt > 0 {
        let in_target = find_by_message_id(conn, to_raw, message_id).await?;
        if let Some(&target_uid) = in_target.last() {
            let mut view = select(conn, from_raw).await?;
            if !sync::uid_search(conn, &format!("UID {uid}"), &mut view)
                .await?
                .is_empty()
            {
                raw::run(
                    &mut conn.session,
                    &format!("UID STORE {uid} +FLAGS.SILENT (\\Deleted)"),
                    |_| {},
                )
                .await?;
                let expunge = if conn.caps.has("UIDPLUS") {
                    format!("UID EXPUNGE {uid}")
                } else {
                    "EXPUNGE".to_owned()
                };
                raw::run(&mut conn.session, &expunge, |_| {}).await?;
            }
            return Ok(Some(target_uid));
        }
    }
    move_library(conn, from_raw, uid, to_raw, message_id).await
}

/// UIDs at or above `since` whose Message-ID is `message_id`, found by fetching the Message-ID
/// header of the newer messages and comparing here. Needs no server-side search: Stalwart 0.16
/// never matches `SEARCH HEADER Message-ID`.
pub async fn find_since(
    conn: &mut Connection,
    name_raw: &str,
    since: u32,
    message_id: &str,
) -> Result<Vec<u32>, ImapError> {
    if conn.selected.as_deref() != Some(name_raw) {
        sync::examine(conn, name_raw).await?;
    }
    let mut found = Vec::new();
    raw::run(
        &mut conn.session,
        &format!("UID FETCH {since}:* (UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])"),
        |r| {
            if let Response::Fetch(_, attrs) = r {
                let uid = attrs.iter().find_map(|a| match a {
                    async_imap::imap_proto::AttributeValue::Uid(u) => Some(*u),
                    _ => None,
                });
                if let Some(uid) = uid.filter(|u| *u >= since)
                    && crate::fetch::message_id(attrs).as_deref() == Some(message_id)
                {
                    found.push(uid);
                }
            }
        },
    )
    .await?;
    found.sort_unstable();
    Ok(found)
}

/// UIDNEXT of a mailbox (it becomes the examined mailbox).
pub async fn uidnext(conn: &mut Connection, name_raw: &str) -> Result<u32, ImapError> {
    let view = sync::examine(conn, name_raw).await?;
    view.uidnext
        .ok_or_else(|| ImapError::Protocol("no UIDNEXT".into()))
}

/// The recommended op-queue APPEND: the op records the target's UIDNEXT before the first
/// attempt; a retry looks for the Message-ID among UIDs at or above it, and the new UID comes
/// from APPENDUID when the server sends it, else from the same window.
pub async fn append_op(
    conn: &mut Connection,
    name_raw: &str,
    flags: &str,
    message: &[u8],
    message_id: &str,
    uidnext_before: u32,
    attempt: u32,
) -> Result<(Option<u32>, bool), ImapError> {
    if attempt > 0
        && let Some(uid) = find_since(conn, name_raw, uidnext_before, message_id)
            .await?
            .last()
            .copied()
    {
        return Ok((Some(uid), false));
    }
    let uid = match append_raw(conn, name_raw, flags, message).await? {
        Some(uid) => Some(uid),
        None => find_since(conn, name_raw, uidnext_before, message_id)
            .await?
            .last()
            .copied(),
    };
    Ok((uid, true))
}

/// The recommended op-queue MOVE: COPYUID when offered; on a retry the target window
/// (UIDs at or above the target's UIDNEXT recorded before the first attempt) is checked first,
/// and a leftover source copy (COPY-based move) is removed.
pub async fn move_op(
    conn: &mut Connection,
    from_raw: &str,
    uid: u32,
    to_raw: &str,
    message_id: &str,
    target_uidnext_before: u32,
    attempt: u32,
) -> Result<Option<u32>, ImapError> {
    if attempt > 0
        && let Some(target_uid) = find_since(conn, to_raw, target_uidnext_before, message_id)
            .await?
            .last()
            .copied()
    {
        let mut view = select(conn, from_raw).await?;
        if !sync::uid_search(conn, &format!("UID {uid}"), &mut view)
            .await?
            .is_empty()
        {
            raw::run(
                &mut conn.session,
                &format!("UID STORE {uid} +FLAGS.SILENT (\\Deleted)"),
                |_| {},
            )
            .await?;
            let expunge = if conn.caps.has("UIDPLUS") {
                format!("UID EXPUNGE {uid}")
            } else {
                "EXPUNGE".to_owned()
            };
            raw::run(&mut conn.session, &expunge, |_| {}).await?;
        }
        return Ok(Some(target_uid));
    }
    if conn.caps.has("MOVE")
        && let Some(copy) = move_raw(conn, from_raw, uid, to_raw).await?
    {
        return Ok(raw::expand(&copy.destination).last().copied());
    }
    if !conn.caps.has("MOVE") {
        select(conn, from_raw).await?;
        raw::run(
            &mut conn.session,
            &format!("UID COPY {uid} {}", quote(to_raw)),
            |_| {},
        )
        .await?;
        raw::run(
            &mut conn.session,
            &format!("UID STORE {uid} +FLAGS.SILENT (\\Deleted)"),
            |_| {},
        )
        .await?;
        let expunge = if conn.caps.has("UIDPLUS") {
            format!("UID EXPUNGE {uid}")
        } else {
            "EXPUNGE".to_owned()
        };
        raw::run(&mut conn.session, &expunge, |_| {}).await?;
    }
    Ok(find_since(conn, to_raw, target_uidnext_before, message_id)
        .await?
        .last()
        .copied())
}

/// UID MOVE by the raw layer, keeping COPYUID (sent untagged before the expunges, RFC 6851,
/// or in the tagged OK).
pub async fn move_raw(
    conn: &mut Connection,
    from_raw: &str,
    uid: u32,
    to_raw: &str,
) -> Result<Option<CopyUid>, ImapError> {
    select(conn, from_raw).await?;
    let mut untagged = None;
    let completion = raw::run(
        &mut conn.session,
        &format!("UID MOVE {uid} {}", quote(to_raw)),
        |r| {
            if let Response::Data { outcome, .. } = r
                && let Some(code) = raw::copy_uid(outcome.code.as_ref())
            {
                untagged = Some(code);
            }
        },
    )
    .await?;
    Ok(untagged.or(completion.copy_uid))
}
