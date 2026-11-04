//! Records that only the self-tests and the examples read back. Nothing is kept until one of them
//! calls [`record`], so a running Sealwax never holds a quick reply's text, a compose request or
//! a growing list of events after it is done with them.

use std::cell::{Cell, RefCell};

thread_local! {
    static RECORDING: Cell<bool> = const { Cell::new(false) };
}

/// Keeps every [`Log`]'s entries from now on, for a self-test or an example to read back.
pub fn record() {
    RECORDING.with(|recording| recording.set(true));
}

/// Whether [`record`] was called on this thread.
pub fn recording() -> bool {
    RECORDING.with(Cell::get)
}

/// A list of entries kept only while [`recording`].
#[derive(Debug)]
pub struct Log<T> {
    entries: RefCell<Vec<T>>,
}

impl<T> Default for Log<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Log<T> {
    pub const fn new() -> Self {
        Self {
            entries: RefCell::new(Vec::new()),
        }
    }

    /// Keeps `entry` while recording; its index, for [`Log::update`].
    pub fn push(&self, entry: T) -> Option<usize> {
        if !recording() {
            return None;
        }
        let mut entries = self.entries.borrow_mut();
        entries.push(entry);
        Some(entries.len() - 1)
    }

    /// Changes the entry [`Log::push`] gave `index` for.
    pub fn update(&self, index: usize, change: impl FnOnce(&mut T)) {
        if let Some(entry) = self.entries.borrow_mut().get_mut(index) {
            change(entry);
        }
    }

    /// The entries kept so far, oldest first, leaving none.
    pub fn take(&self) -> Vec<T> {
        self.entries.take()
    }

    pub fn len(&self) -> usize {
        self.entries.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.borrow().is_empty()
    }
}

impl<T: Clone> Log<T> {
    /// The entries kept so far, oldest first.
    pub fn entries(&self) -> Vec<T> {
        self.entries.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Each test runs on a thread of its own, so `record` in one doesn't reach another.

    #[test]
    fn nothing_is_kept_until_a_test_records() {
        let log = Log::new();
        assert_eq!(log.push("a reply's text"), None);
        assert!(log.is_empty());
        assert!(!recording());
    }

    #[test]
    fn entries_are_kept_and_updated_while_recording() {
        record();
        let log = Log::new();
        let first = log.push(1).unwrap();
        log.push(2);
        log.update(first, |entry| *entry = 10);
        assert_eq!(log.entries(), [10, 2]);
        assert_eq!(log.take(), [10, 2]);
        assert!(log.is_empty());
    }
}
