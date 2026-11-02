//! Inline images of messages stored as their text parts (ML-061), kept in memory once
//! downloaded: the reader shows them again, and a reply copies them, from here rather than the
//! server. Attachments opened or saved still come from the server each time. At most [`CAP`]
//! bytes, the least recently used dropped first, and nothing over [`LARGEST`]; a message gone
//! from the store takes its parts with it. Nothing is written to disk. GTK thread only.

use sealwax_domain::model::MessageId;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

/// The most bytes kept.
pub const CAP: usize = 32 * 1024 * 1024;
/// A part larger than this isn't kept.
pub const LARGEST: usize = 8 * 1024 * 1024;

type Key = (MessageId, u32);

#[derive(Default)]
struct Cache {
    /// Least recently used first.
    entries: VecDeque<(Key, Rc<[u8]>)>,
    bytes: usize,
}

impl Cache {
    fn take(&mut self, key: Key) -> Option<(Key, Rc<[u8]>)> {
        let at = self.entries.iter().position(|(known, _)| *known == key)?;
        let entry = self.entries.remove(at)?;
        self.bytes -= entry.1.len();
        Some(entry)
    }
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::default();
}

/// The bytes of part `part` of `message`, when kept.
pub fn get(message: MessageId, part: u32) -> Option<Vec<u8>> {
    CACHE.with_borrow_mut(|cache| {
        let entry = cache.take((message, part))?;
        let data = entry.1.to_vec();
        cache.bytes += entry.1.len();
        cache.entries.push_back(entry);
        Some(data)
    })
}

/// Whether part `part` of `message` is kept, without counting as a use.
pub fn contains(message: MessageId, part: u32) -> bool {
    CACHE.with_borrow(|cache| cache.entries.iter().any(|(key, _)| *key == (message, part)))
}

/// Keeps `data` as part `part` of `message`, dropping the least recently used to stay in
/// [`CAP`].
pub fn put(message: MessageId, part: u32, data: &[u8]) {
    if data.is_empty() || data.len() > LARGEST {
        return;
    }
    CACHE.with_borrow_mut(|cache| {
        cache.take((message, part));
        while cache.bytes + data.len() > CAP {
            let Some((_, dropped)) = cache.entries.pop_front() else {
                break;
            };
            cache.bytes -= dropped.len();
        }
        cache.bytes += data.len();
        cache.entries.push_back(((message, part), Rc::from(data)));
    });
}

/// Drops the parts of `messages` (gone from the store: their ids may be used again).
pub fn forget(messages: &[MessageId]) {
    if messages.is_empty() {
        return;
    }
    CACHE.with_borrow_mut(|cache| {
        cache
            .entries
            .retain(|((message, _), _)| !messages.contains(message));
        cache.bytes = cache.entries.iter().map(|(_, data)| data.len()).sum();
    });
}

/// How many bytes are kept.
pub fn bytes() -> usize {
    CACHE.with_borrow(|cache| cache.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_part_kept_comes_back_until_its_message_is_gone() {
        put(MessageId(1), 2, b"png");
        put(MessageId(3), 2, b"gif");
        assert_eq!(get(MessageId(1), 2).as_deref(), Some(&b"png"[..]));
        assert_eq!(get(MessageId(1), 3), None);
        forget(&[MessageId(1)]);
        assert_eq!(get(MessageId(1), 2), None);
        assert_eq!(get(MessageId(3), 2).as_deref(), Some(&b"gif"[..]));
        assert_eq!(bytes(), 3);
        forget(&[MessageId(3)]);
        assert_eq!(bytes(), 0);
    }

    #[test]
    fn the_least_recently_used_go_first_and_large_parts_are_not_kept() {
        let part = vec![7u8; LARGEST];
        for message in 0..4 {
            put(MessageId(message), 0, &part);
        }
        assert_eq!(bytes(), CAP);
        assert!(get(MessageId(0), 0).is_some());
        put(MessageId(4), 0, &part);
        assert!(get(MessageId(1), 0).is_none());
        assert!(get(MessageId(0), 0).is_some());
        put(MessageId(5), 0, &vec![1u8; LARGEST + 1]);
        assert!(get(MessageId(5), 0).is_none());
        put(MessageId(0), 0, &part);
        assert_eq!(bytes(), CAP);
        forget(&(0..6).map(MessageId).collect::<Vec<_>>());
        assert_eq!(bytes(), 0);
    }
}
