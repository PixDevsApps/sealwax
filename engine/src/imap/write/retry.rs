//! A Message-ID is a retry hint, not proof that a destination holds the source's contents.
//! Compare complete-message hashes through partial BODY.PEEK reads before deleting a source.

use super::{AttributeValue, Connection, ImapError, OpFolder, Response, raw, view};
use sha2::{Digest, Sha256};

#[cfg(test)]
mod tests;

const CHUNK_BYTES: u32 = 64 * 1024;
/// Limit the extra work of an uncertain move. Larger or unreadable messages are copied again
/// rather than deduplicated using only their Message-ID.
const MAX_COMPARE_BYTES: u32 = 16 * 1024 * 1024;

pub(super) async fn same_message(
    conn: &mut Connection,
    from: OpFolder<'_>,
    source: u32,
    to: &str,
    target_uidvalidity: u32,
    target: u32,
) -> Result<bool, ImapError> {
    let Some(source) = fingerprint(conn, from.name_raw, from.uidvalidity, source).await? else {
        return Ok(false);
    };
    let target = fingerprint(conn, to, target_uidvalidity, target).await?;
    Ok(target == Some(source))
}

async fn fingerprint(
    conn: &mut Connection,
    folder: &str,
    uidvalidity: u32,
    uid: u32,
) -> Result<Option<[u8; 32]>, ImapError> {
    if view::examine(conn, folder).await?.uidvalidity != uidvalidity {
        return Ok(None);
    }
    let mut hash = Sha256::new();
    let mut offset = 0;
    loop {
        // One byte beyond the budget distinguishes an exact-boundary message from a prefix.
        let count = CHUNK_BYTES.min(MAX_COMPARE_BYTES - offset + 1);
        let command = format!("UID FETCH {uid} (UID BODY.PEEK[]<{offset}.{count}>)");
        let mut part = None;
        let mut invalid = false;
        raw::run(&mut conn.session, &command, |response| {
            let Response::Fetch(_, attributes) = response else {
                return;
            };
            if !attributes
                .iter()
                .any(|attribute| matches!(attribute, AttributeValue::Uid(found) if *found == uid))
            {
                return;
            }
            for attribute in attributes {
                if let AttributeValue::BodySection {
                    section: None,
                    index,
                    data,
                } = attribute
                {
                    match data.as_deref() {
                        Some(bytes)
                            if *index == Some(offset)
                                && bytes.len() <= count as usize
                                && part.is_none() =>
                        {
                            part = Some(bytes.to_vec());
                        }
                        _ => invalid = true,
                    }
                }
            }
        })
        .await?;
        let Some(part) = part.filter(|_| !invalid) else {
            return Ok(None);
        };
        offset += part.len() as u32;
        if offset > MAX_COMPARE_BYTES {
            return Ok(None);
        }
        hash.update(&part);
        if part.len() < count as usize {
            return Ok(Some(hash.finalize().into()));
        }
    }
}
