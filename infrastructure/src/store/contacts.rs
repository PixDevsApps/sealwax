//! Addresses already seen, for the composer's suggestions (ML-071, PRD C3): per account, every
//! address in From, To and Cc of the stored messages with a display name, how many messages it
//! was seen in, how many the user wrote to it (To and Cc of messages in Sent folders) and when
//! it was last seen. Each message counts once per Message-ID while the store holds a copy of it
//! (Gmail's labels file one message in several folders), and once more as written to for its
//! first copy in a Sent folder. The account's own address is left out. Counts only grow: mail
//! deleted later still counts.
//!
//! Suggestions match a prefix of the address, its domain or any word of a name the contact was
//! seen with, case-insensitive and with common Latin diacritics folded (`å` → `a`, `æ` → `ae`).

use super::codec;
use rusqlite::{Connection, OptionalExtension, params};
use sealwax_domain::model::{AccountId, Address};
use std::collections::{HashMap, HashSet};

/// Lower case, common Latin diacritics folded to their base letters.
pub(crate) fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars().flat_map(char::to_lowercase) {
        match c {
            'à'..='å' | 'ā' | 'ă' | 'ą' => out.push('a'),
            'æ' => out.push_str("ae"),
            'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => out.push('c'),
            'ð' | 'ď' | 'đ' => out.push('d'),
            'è'..='ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => out.push('e'),
            'ĝ' | 'ğ' | 'ġ' | 'ģ' => out.push('g'),
            'ĥ' | 'ħ' => out.push('h'),
            'ì'..='ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => out.push('i'),
            'ĵ' => out.push('j'),
            'ķ' => out.push('k'),
            'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => out.push('l'),
            'ñ' | 'ń' | 'ņ' | 'ň' => out.push('n'),
            'ò'..='ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => out.push('o'),
            'œ' => out.push_str("oe"),
            'ŕ' | 'ŗ' | 'ř' => out.push('r'),
            'ś' | 'ŝ' | 'ş' | 'š' => out.push('s'),
            'ß' => out.push_str("ss"),
            'ţ' | 'ť' | 'ŧ' => out.push('t'),
            'þ' => out.push_str("th"),
            'ù'..='ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => out.push('u'),
            'ŵ' => out.push('w'),
            'ý' | 'ÿ' | 'ŷ' => out.push('y'),
            'ź' | 'ż' | 'ž' => out.push('z'),
            c => out.push(c),
        }
    }
    out
}

/// How a contact's address is kept: trimmed and in lower case; `None` for one that isn't an
/// address (no `@`, or whitespace inside).
pub(crate) fn address_key(address: &str) -> Option<String> {
    let address = address.trim();
    (address.contains('@') && !address.chars().any(char::is_whitespace))
        .then(|| address.to_lowercase())
}

/// The terms a contact is found by: the folded address, its domain and the name's words.
pub(crate) fn terms(address: &str, name: &str) -> Vec<String> {
    let folded = fold(address);
    let mut terms = vec![folded.clone()];
    if let Some((_, domain)) = folded.rsplit_once('@')
        && !domain.is_empty()
    {
        terms.push(domain.to_owned());
    }
    terms.extend(name_terms(name));
    terms.sort_unstable();
    terms.dedup();
    terms
}

fn name_terms(name: &str) -> Vec<String> {
    fold(name)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

/// What one message adds to its account's contacts.
pub(crate) struct Seen<'a> {
    pub(crate) account: AccountId,
    /// The account's own address, which is left out.
    pub(crate) own: &'a str,
    pub(crate) from: Option<&'a Address>,
    pub(crate) to: &'a [Address],
    pub(crate) cc: &'a [Address],
    pub(crate) date: i64,
    /// The store holds no other copy of the message: it counts as seen.
    pub(crate) first_copy: bool,
    /// The first copy of the message in a Sent folder: its To and Cc count as written to.
    pub(crate) written: bool,
}

/// A message's addresses, each once, without the account's own: the key, a name it came with
/// and whether it was a recipient (To or Cc).
fn distinct<'a>(
    own: &str,
    from: Option<&'a Address>,
    to: &'a [Address],
    cc: &'a [Address],
) -> Vec<(String, &'a str, bool)> {
    let own = own.trim().to_lowercase();
    let mut out: Vec<(String, &str, bool)> = Vec::new();
    let senders = from.into_iter().map(|address| (address, false));
    let recipients = to.iter().chain(cc).map(|address| (address, true));
    for (address, recipient) in senders.chain(recipients) {
        let Some(key) = address_key(&address.email) else {
            continue;
        };
        if key == own {
            continue;
        }
        let name = address.name.trim();
        match out.iter_mut().find(|(other, _, _)| *other == key) {
            Some(entry) => {
                entry.2 |= recipient;
                if entry.1.is_empty() {
                    entry.1 = name;
                }
            }
            None => out.push((key, name, recipient)),
        }
    }
    out
}

/// Adds one stored message to the contacts, in the writer's transaction.
pub(crate) fn add(tx: &Connection, seen: &Seen<'_>) -> rusqlite::Result<()> {
    if !seen.first_copy && !seen.written {
        return Ok(());
    }
    for (key, name, recipient) in distinct(seen.own, seen.from, seen.to, seen.cc) {
        upsert(
            tx,
            seen.account,
            &key,
            name,
            (
                u32::from(seen.first_copy),
                u32::from(seen.written && recipient),
            ),
            seen.date,
        )?;
    }
    Ok(())
}

fn upsert(
    tx: &Connection,
    account: AccountId,
    address: &str,
    name: &str,
    (seen, written): (u32, u32),
    date: i64,
) -> rusqlite::Result<()> {
    let found: Option<(i64, String, i64)> = tx
        .prepare_cached(
            "SELECT id, name, last FROM contacts WHERE account_id = ?1 AND address = ?2",
        )?
        .query_row(params![account.0, address], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?;
    let mut insert_term =
        tx.prepare_cached("INSERT OR IGNORE INTO contact_terms(term, contact) VALUES (?1, ?2)")?;
    let Some((id, old_name, last)) = found else {
        let id: i64 = tx
            .prepare_cached(
                "INSERT INTO contacts(account_id, address, name, seen, written, last) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) RETURNING id",
            )?
            .query_row(
                params![account.0, address, name, seen, written, date],
                |row| row.get(0),
            )?;
        for term in terms(address, name) {
            insert_term.execute(params![term, id])?;
        }
        return Ok(());
    };
    let rename = !name.is_empty() && name != old_name && (old_name.is_empty() || date >= last);
    tx.prepare_cached(
        "UPDATE contacts SET seen = seen + ?2, written = written + ?3, last = max(last, ?4), \
           name = CASE WHEN ?5 THEN ?6 ELSE name END WHERE id = ?1",
    )?
    .execute(params![id, seen, written, date, rename, name])?;
    if rename {
        // Old names' words stay: a contact is still found by a name it was seen with.
        for term in name_terms(name) {
            insert_term.execute(params![term, id])?;
        }
    }
    Ok(())
}

/// A contact as the backfill adds it up.
struct Aggregate {
    name: String,
    seen: u32,
    written: u32,
    last: i64,
}

/// Fills the contacts from every stored message (migration v9), as [`add`] would have.
pub(crate) fn backfill(tx: &Connection) -> rusqlite::Result<()> {
    let own: HashMap<i64, String> = tx
        .prepare("SELECT id, address FROM accounts")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut contacts: HashMap<(i64, String), Aggregate> = HashMap::new();
    let mut seen_ids: HashSet<(i64, String)> = HashSet::new();
    let mut sent_ids: HashSet<(i64, String)> = HashSet::new();
    let mut select = tx.prepare(
        "SELECT f.account_id, f.role = 'sent', m.message_id, m.from_name, m.from_addr, \
           m.to_addrs, m.cc_addrs, m.date \
         FROM messages m JOIN folders f ON f.id = m.folder_id ORDER BY m.id",
    )?;
    let mut rows = select.query([])?;
    while let Some(row) = rows.next()? {
        let account: i64 = row.get(0)?;
        let in_sent: bool = row.get(1)?;
        let (first_copy, written) = match row.get::<_, Option<String>>(2)? {
            Some(id) => (
                seen_ids.insert((account, id.clone())),
                in_sent && sent_ids.insert((account, id)),
            ),
            None => (true, in_sent),
        };
        if !first_copy && !written {
            continue;
        }
        let from = Address::new(row.get::<_, String>(3)?, row.get::<_, String>(4)?);
        let to = codec::decode_addresses(&row.get::<_, String>(5)?);
        let cc = codec::decode_addresses(&row.get::<_, String>(6)?);
        let date: i64 = row.get(7)?;
        let own = own.get(&account).map_or("", String::as_str);
        for (key, name, recipient) in distinct(own, Some(&from), &to, &cc) {
            let entry = contacts.entry((account, key)).or_insert(Aggregate {
                name: String::new(),
                seen: 0,
                written: 0,
                last: date,
            });
            if !name.is_empty() && (entry.name.is_empty() || date >= entry.last) {
                entry.name = name.to_owned();
            }
            entry.last = entry.last.max(date);
            entry.seen += u32::from(first_copy);
            entry.written += u32::from(written && recipient);
        }
    }
    drop(rows);
    let mut insert = tx.prepare(
        "INSERT INTO contacts(account_id, address, name, seen, written, last) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) RETURNING id",
    )?;
    let mut term =
        tx.prepare("INSERT OR IGNORE INTO contact_terms(term, contact) VALUES (?1, ?2)")?;
    let mut sorted: Vec<_> = contacts.into_iter().collect();
    sorted.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    for ((account, address), aggregate) in sorted {
        let id: i64 = insert.query_row(
            params![
                account,
                address,
                aggregate.name,
                aggregate.seen,
                aggregate.written,
                aggregate.last
            ],
            |row| row.get(0),
        )?;
        for word in terms(&address, &aggregate.name) {
            term.execute(params![word, id])?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_case_and_diacritics() {
        assert_eq!(fold("Åse Bråten"), "ase braten");
        assert_eq!(fold("Ærlig Øvre"), "aerlig ovre");
        assert_eq!(fold("Jürgen Groß"), "jurgen gross");
        assert_eq!(fold("Łukasz Żółć"), "lukasz zolc");
        assert_eq!(fold("王伟"), "王伟");
    }

    #[test]
    fn terms_are_the_address_its_domain_and_the_name_words() {
        assert_eq!(
            terms("kari.nordmann@example.test", "Nordmann, Kari"),
            [
                "example.test",
                "kari",
                "kari.nordmann@example.test",
                "nordmann"
            ]
        );
        assert_eq!(
            address_key(" Kari@Example.Test "),
            Some("kari@example.test".into())
        );
        assert_eq!(address_key("undisclosed-recipients"), None);
    }

    #[test]
    fn a_message_counts_each_address_once_without_the_own() {
        let from = Address::new("Kari", "kari@example.test");
        let to = [
            Address::new("", "KARI@example.test"),
            Address::new("Me", "me@example.test"),
            Address::new("Ola", "ola@example.test"),
        ];
        let got = distinct("Me@Example.test", Some(&from), &to, &[]);
        assert_eq!(
            got,
            [
                ("kari@example.test".to_owned(), "Kari", true),
                ("ola@example.test".to_owned(), "Ola", true)
            ]
        );
    }
}
