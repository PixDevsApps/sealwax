//! The IMAP fuzz target's entry point (`crate::fuzz`): what Sealwax itself parses of a server's
//! IMAP answers, past async-imap and imap-proto: modified UTF-7 folder names, the folder roles
//! from a LIST, COPYUID's UID sets, and the names and UID sets it writes back.

use super::raw::{copy_uid, quote, uid_set};
use super::{mutf7, roles};
use async_imap::imap_proto::{Response, ResponseCode};
use sealwax_domain::model::FolderRole;

/// The UIDs an IMAP sequence set names, sorted (only `n` and `n:m`, as [`uid_set`] writes).
fn expand(set: &str) -> Vec<u32> {
    let mut uids = Vec::new();
    for member in set.split(',').filter(|member| !member.is_empty()) {
        let number = |text: &str| text.parse::<u32>().expect("a UID");
        match member.split_once(':') {
            Some((start, end)) => uids.extend(number(start)..=number(end)),
            None => uids.push(number(member)),
        }
    }
    uids
}

pub fn imap(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    let encoded = mutf7::encode(&text);
    assert!(encoded.is_ascii());
    assert_eq!(
        mutf7::decode(&encoded).as_deref(),
        Ok(&*text),
        "a name changed on the wire"
    );
    if let Ok(name) = mutf7::decode(&text) {
        assert_eq!(mutf7::decode(&mutf7::encode(&name)), Ok(name));
    }
    let _ = mutf7::display_name(&text);
    if let Ok(quoted) = quote(&text) {
        assert!(quoted.starts_with('"') && quoted.ends_with('"') && quoted.len() >= 2);
        assert!(!quoted.contains(['\r', '\n', '\0']));
    }

    // A LIST reply as lines of "attributes<TAB>delimiter<TAB>name".
    let listed: Vec<(Vec<String>, Option<char>, String)> = text
        .lines()
        .take(64)
        .map(|line| {
            let mut fields = line.splitn(3, '\t');
            let attributes = fields
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .map(str::to_owned)
                .collect();
            let delimiter = fields.next().and_then(|field| field.chars().next());
            (
                attributes,
                delimiter,
                mutf7::display_name(fields.next().unwrap_or_default()),
            )
        })
        .collect();
    let entries: Vec<roles::Entry<'_>> = listed
        .iter()
        .map(|(attributes, delimiter, name)| roles::Entry {
            name,
            delimiter: *delimiter,
            attributes,
        })
        .collect();
    let assigned = roles::assign(&entries);
    assert_eq!(assigned.len(), entries.len());
    for (i, role) in assigned.iter().enumerate() {
        if *role != FolderRole::Other {
            assert!(
                !assigned[i + 1..].contains(role),
                "two folders with one role"
            );
        }
    }

    let uids: Vec<u32> = data
        .as_chunks::<4>()
        .0
        .iter()
        .take(256)
        .map(|bytes| u32::from_le_bytes(*bytes) % 100_000 + 1)
        .collect();
    let mut sorted = uids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        expand(&uid_set(&uids)),
        sorted,
        "a UID set names other UIDs"
    );

    // imap-proto 0.17.0 panics when a response code is followed at once by a non-ASCII
    // character (`Outcome::parse` slices off one byte for the space); docs/m9/security.md has
    // the finding. Such lines are left out so the target keeps looking for other faults.
    if data
        .windows(2)
        .any(|pair| pair[0] == b']' && !pair[1].is_ascii())
    {
        return;
    }
    if let Ok((_, Response::Done { outcome, .. } | Response::Data { outcome, .. })) =
        Response::parse(data)
    {
        if let Some(found) = copy_uid(outcome.code.as_ref()) {
            assert!(found.pairs.len() <= 100_000);
        }
        if let Some(ResponseCode::CopyUid(..)) = outcome.code {
            let _ = format!("{:?}", outcome.code);
        }
    }
}
