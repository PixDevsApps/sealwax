//! Records that only the self-tests and the harnesses read back. Nothing is kept until one of
//! them calls `record` (a `test-hooks` build's), so a running Sealwax never holds a quick reply's
//! text, a compose request or a growing list of events after it is done with them; the released
//! binary can't record at all.

use std::cell::RefCell;

#[cfg(any(test, feature = "test-hooks"))]
thread_local! {
    static RECORDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Keeps every [`Log`]'s entries from now on, for a self-test or a harness to read back.
#[cfg(any(test, feature = "test-hooks"))]
pub fn record() {
    RECORDING.with(|recording| recording.set(true));
}

/// Whether `record` was called on this thread; never in the released binary.
#[cfg(any(test, feature = "test-hooks"))]
pub(crate) fn recording() -> bool {
    RECORDING.with(std::cell::Cell::get)
}

/// Whether `record` was called on this thread; never in the released binary.
#[cfg(not(any(test, feature = "test-hooks")))]
pub(crate) fn recording() -> bool {
    false
}

/// A list of entries kept only while [`recording`].
#[derive(Debug)]
pub(crate) struct Log<T> {
    entries: RefCell<Vec<T>>,
}

impl<T> Default for Log<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Log<T> {
    pub(crate) const fn new() -> Self {
        Self {
            entries: RefCell::new(Vec::new()),
        }
    }

    /// Keeps `entry` while recording; its index, for [`Log::update`].
    pub(crate) fn push(&self, entry: T) -> Option<usize> {
        if !recording() {
            return None;
        }
        let mut entries = self.entries.borrow_mut();
        entries.push(entry);
        Some(entries.len() - 1)
    }

    /// Changes the entry [`Log::push`] gave `index` for.
    pub(crate) fn update(&self, index: usize, change: impl FnOnce(&mut T)) {
        if let Some(entry) = self.entries.borrow_mut().get_mut(index) {
            change(entry);
        }
    }

    /// The entries kept so far, oldest first, leaving none.
    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn take(&self) -> Vec<T> {
        self.entries.take()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.borrow().is_empty()
    }
}

impl<T: Clone> Log<T> {
    /// The entries kept so far, oldest first.
    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn entries(&self) -> Vec<T> {
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
