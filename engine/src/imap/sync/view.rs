//! What the server says about the examined mailbox, kept current from every untagged EXISTS,
//! EXPUNGE and VANISHED a pass reads, and the commands a pass reads it with: EXAMINE, UID
//! SEARCH and the two kinds of UID FETCH. Server notices only ever feed this view; what a pass
//! concludes comes from the state it fetches (ADR-006).

use super::{Careful, Unreadable};
use crate::imap::connection::Connection;
use crate::imap::error::{ImapError, SyncError};
use crate::imap::fetch::{self, Fetched};
use crate::imap::raw::{self, quote};
use async_imap::imap_proto::{AttributeValue, MailboxDatum, Response, ResponseCode, Status};
use sealwax_domain::model::{Flags, FolderId};
use sealwax_infrastructure::store::{FlagUpdate, NewMessage};
use std::collections::{BTreeMap, HashSet};
use std::ops::RangeInclusive;

#[derive(Clone, Debug, Default)]
pub(crate) struct View {
    pub uidvalidity: u32,
    pub uidnext: Option<u32>,
    pub exists: u32,
    pub highestmodseq: Option<u64>,
    /// UIDs reported gone (VANISHED, with or without EARLIER) and not yet applied, as ranges:
    /// a server may name UIDs that never existed, so they are only ever matched against stored
    /// ones.
    pub vanished: Vec<RangeInclusive<u32>>,
    /// The account's unreadable FETCHes ([`Unreadable`]), set by the pass from its options.
    pub unreadable: Unreadable,
}

impl View {
    pub(crate) fn track(&mut self, response: &Response<'_>) {
        match response {
            Response::MailboxData(MailboxDatum::Exists(count)) => self.exists = *count,
            Response::Expunge(_) => self.exists = self.exists.saturating_sub(1),
            Response::Vanished { earlier, uids } => {
                if !earlier {
                    let gone: u64 = uids
                        .iter()
                        .map(|range| u64::from(range.end().saturating_sub(*range.start())) + 1)
                        .sum();
                    self.exists = self
                        .exists
                        .saturating_sub(u32::try_from(gone).unwrap_or(u32::MAX));
                }
                self.vanished.extend(uids.iter().cloned());
            }
            _ => {}
        }
    }

    /// Whether `uid` was reported gone.
    pub(crate) fn has_vanished(&self, uid: u32) -> bool {
        self.vanished.iter().any(|range| range.contains(&uid))
    }
}

/// EXAMINE, with CONDSTORE's parameter where the server offers it and QRESYNC isn't enabled
/// (which turns CONDSTORE on by itself).
pub(crate) async fn examine(conn: &mut Connection, name_raw: &str) -> Result<View, ImapError> {
    let mut command = format!("EXAMINE {}", quote(name_raw)?);
    if conn.caps().has("CONDSTORE") && !conn.qresync() {
        command.push_str(" (CONDSTORE)");
    }
    conn.selected = None;
    let mut view = View::default();
    let mut uidvalidity = None;
    raw::run(&mut conn.session, &command, |response| match response {
        Response::Data {
            status: Status::Ok,
            outcome,
        } => match outcome.code {
            Some(ResponseCode::UidValidity(value)) => uidvalidity = Some(value),
            Some(ResponseCode::UidNext(value)) => view.uidnext = Some(value),
            Some(ResponseCode::HighestModSeq(value)) => view.highestmodseq = Some(value),
            _ => {}
        },
        other => view.track(other),
    })
    .await?;
    conn.selected = Some(name_raw.to_owned());
    view.uidvalidity = uidvalidity.ok_or(ImapError::Protocol("EXAMINE without UIDVALIDITY"))?;
    view.vanished.clear();
    Ok(view)
}

/// `UID SEARCH <criteria>`: the matching UIDs, ascending.
pub(crate) async fn uid_search(
    conn: &mut Connection,
    criteria: &str,
    view: &mut View,
) -> Result<Vec<u32>, ImapError> {
    let mut uids = Vec::new();
    raw::run(
        &mut conn.session,
        &format!("UID SEARCH {criteria}"),
        |response| match response {
            Response::MailboxData(MailboxDatum::Search(found)) => uids.extend_from_slice(found),
            other => view.track(other),
        },
    )
    .await?;
    uids.sort_unstable();
    uids.dedup();
    Ok(uids)
}

/// A FETCH response's UID and flags, when it carries both.
fn flag_update(attributes: &[AttributeValue<'_>]) -> Option<FlagUpdate> {
    let mut uid = None;
    let mut flags = None;
    for attribute in attributes {
        match attribute {
            AttributeValue::Uid(found) => uid = Some(*found),
            AttributeValue::Flags(found) => {
                flags = Some(Flags::from_imap(found.iter().map(|flag| flag.as_ref())));
            }
            _ => {}
        }
    }
    let (flags, keywords) = flags?;
    Some(FlagUpdate {
        uid: uid?,
        flags,
        keywords,
    })
}

/// A `UID FETCH … (UID FLAGS)` with its modifiers: the flags of every UID in `range`, the last
/// response for a UID winning. Responses outside `range` are hints and are dropped.
pub(crate) async fn fetch_flags(
    conn: &mut Connection,
    command: &str,
    range: RangeInclusive<u32>,
    view: &mut View,
) -> Result<Vec<FlagUpdate>, ImapError> {
    let mut found = BTreeMap::new();
    raw::run(&mut conn.session, command, |response| match response {
        Response::Fetch(_, attributes) => {
            if let Some(update) = flag_update(attributes).filter(|u| range.contains(&u.uid)) {
                found.insert(update.uid, update);
            }
        }
        other => view.track(other),
    })
    .await?;
    Ok(found.into_values().collect())
}

/// One header FETCH of `set` with `items`, added to `out`. Responses for UIDs `wanted` refuses,
/// or without the header section (unsolicited flag updates), are hints and are dropped.
async fn fetch_into(
    conn: &mut Connection,
    set: &str,
    items: &str,
    wanted: &impl Fn(u32) -> bool,
    view: &mut View,
    out: &mut Vec<Fetched>,
) -> Result<(), ImapError> {
    let mut seen: HashSet<u32> = out.iter().map(|found| found.uid).collect();
    let command = format!("UID FETCH {set} {items}");
    raw::run(&mut conn.session, &command, |response| match response {
        Response::Fetch(_, attributes) => {
            if let Some(found) = fetch::fetched(attributes)
                && wanted(found.uid)
                && seen.insert(found.uid)
            {
                out.push(found);
            }
        }
        other => view.track(other),
    })
    .await
}

/// The header FETCH of `set` in `folder`, asking as little as the folder's earlier unreadable
/// replies call for ([`Unreadable`]). Once the folder is careful or has messages left out, the
/// set's UIDs are searched first, so the left-out ones are never asked for again.
pub(crate) async fn fetch_headers(
    conn: &mut Connection,
    folder: FolderId,
    set: &str,
    wanted: impl Fn(u32) -> bool,
    view: &mut View,
) -> Result<Vec<Fetched>, ImapError> {
    let unreadable = view.unreadable.clone();
    let careful = unreadable.careful(folder);
    let skipped = unreadable.skipped(folder);
    let mut out = Vec::new();
    if careful == Careful::Full && skipped.is_empty() {
        let fetched = fetch_into(conn, set, &fetch::items(), &wanted, view, &mut out).await;
        return fetched
            .map(|()| out)
            .map_err(|error| failed(&unreadable, folder, error));
    }
    let mut uids = uid_search(conn, &format!("UID {set}"), view).await?;
    uids.retain(|uid| wanted(*uid) && skipped.binary_search(uid).is_err());
    if uids.is_empty() {
        return Ok(out);
    }
    match careful {
        Careful::Full | Careful::Lean => {
            let items = match careful {
                Careful::Full => fetch::items(),
                _ => fetch::lean_items(),
            };
            let set = raw::uid_set(&uids);
            fetch_into(conn, &set, &items, &wanted, view, &mut out)
                .await
                .map_err(|error| failed(&unreadable, folder, error))?;
        }
        Careful::Single => {
            for uid in uids {
                let fetched = fetch_into(
                    conn,
                    &uid.to_string(),
                    &fetch::lean_items(),
                    &wanted,
                    view,
                    &mut out,
                )
                .await;
                if let Err(error) = fetched {
                    if matches!(error, ImapError::Unreadable(_)) {
                        tracing::warn!(
                            %folder,
                            uid,
                            "a message's headers couldn't be read; it is left out until Sealwax starts again"
                        );
                        unreadable.skip(folder, uid);
                    }
                    return Err(error);
                }
            }
        }
    }
    unreadable.passed(folder);
    Ok(out)
}

/// Notes an unreadable reply in `folder`, so the next pass asks for less.
fn failed(unreadable: &Unreadable, folder: FolderId, error: ImapError) -> ImapError {
    if matches!(error, ImapError::Unreadable(_)) {
        let next = unreadable.failed(folder);
        tracing::warn!(%folder, ?next, "a header FETCH couldn't be read; the next pass asks for less");
    }
    error
}

/// The header FETCH of `set` ([`fetch_headers`]) as store rows.
pub(crate) async fn fetch_rows(
    conn: &mut Connection,
    folder: FolderId,
    set: &str,
    wanted: impl Fn(u32) -> bool,
    view: &mut View,
) -> Result<Vec<NewMessage>, SyncError> {
    let out = fetch_headers(conn, folder, set, wanted, view).await?;
    let rows = tokio::task::spawn_blocking(move || {
        out.into_iter()
            .map(|found| fetch::message(folder, found))
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|_| SyncError::Parse)?;
    Ok(rows)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A SEARCH date (`7-Jul-2026`) for the day `unix` (seconds, UTC) falls on; the epoch for
/// anything earlier.
pub(crate) fn imap_date(unix: i64) -> String {
    let days = unix.max(0).div_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{day}-{}-{year}", MONTHS[(month - 1) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn the_view_follows_exists_expunge_and_vanished() {
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
            uids: vec![1..=1, 20..=4_000_000_000],
        });
        assert_eq!(view.exists, 9);
        assert!(view.has_vanished(5) && view.has_vanished(1) && view.has_vanished(3_000_000));
        assert!(!view.has_vanished(7));
        view.track(&Response::Vanished {
            earlier: false,
            uids: vec![1..=u32::MAX],
        });
        assert_eq!(view.exists, 0);
    }

    #[test]
    fn search_dates() {
        assert_eq!(imap_date(0), "1-Jan-1970");
        assert_eq!(imap_date(1_767_254_820), "1-Jan-2026");
        assert_eq!(imap_date(1_709_251_199), "29-Feb-2024");
        assert_eq!(imap_date(1_791_158_400), "5-Oct-2026");
        assert_eq!(imap_date(-5), "1-Jan-1970");
    }
}
