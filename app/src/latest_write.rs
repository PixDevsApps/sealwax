//! One write at a time, of the latest value: a value handed in while a write runs waits,
//! replacing any value that waited before it, and is written when that write ends. Writes
//! finish in the order they started, so the file ends up holding the last value, which two
//! writes running side by side don't promise.

/// The writes' state; the caller runs each write it is given and reports its end.
#[derive(Debug)]
pub struct LatestWrite<T> {
    writing: bool,
    waiting: Option<T>,
}

impl<T> Default for LatestWrite<T> {
    fn default() -> Self {
        Self {
            writing: false,
            waiting: None,
        }
    }
}

impl<T> LatestWrite<T> {
    /// Hands in `value`: returned when the caller should write it now, kept for later while a
    /// write runs.
    pub fn offer(&mut self, value: T) -> Option<T> {
        if self.writing {
            self.waiting = Some(value);
            None
        } else {
            self.writing = true;
            Some(value)
        }
    }

    /// The running write ended: the value to write next, if one waited.
    pub fn finished(&mut self) -> Option<T> {
        let next = self.waiting.take();
        self.writing = next.is_some();
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_value_is_written_at_once() {
        let mut writes = LatestWrite::default();
        assert_eq!(writes.offer(1), Some(1));
        assert_eq!(writes.finished(), None);
        assert_eq!(writes.offer(2), Some(2));
    }

    #[test]
    fn values_handed_in_during_a_write_wait_and_only_the_latest_is_written() {
        let mut writes = LatestWrite::default();
        assert_eq!(writes.offer(1), Some(1));
        assert_eq!(writes.offer(2), None);
        assert_eq!(writes.offer(3), None);
        assert_eq!(writes.finished(), Some(3));
        assert_eq!(writes.offer(4), None);
        assert_eq!(writes.finished(), Some(4));
        assert_eq!(writes.finished(), None);
        assert_eq!(writes.offer(5), Some(5));
    }
}
