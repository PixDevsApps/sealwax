//! Date sections of a list ordered newest first: Today, Yesterday, This week, This month, Older.

use gtk4::glib;

pub const LABELS: [&str; 5] = ["Today", "Yesterday", "This week", "This month", "Older"];
pub const OLDER: u8 = 4;

/// Start times (Unix seconds, local midnight) of the first four sections, non-increasing, so an
/// empty section has the same start as the one before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thresholds {
    pub starts: [i64; 4],
}

impl Thresholds {
    pub fn new(today: i64, yesterday: i64, week: i64, month: i64) -> Self {
        let yesterday = yesterday.min(today);
        let week = week.min(yesterday);
        let month = month.min(week);
        Self {
            starts: [today, yesterday, week, month],
        }
    }

    /// Weeks start on Monday.
    pub fn local(now: &glib::DateTime) -> Result<Self, glib::BoolError> {
        let today =
            glib::DateTime::from_local(now.year(), now.month(), now.day_of_month(), 0, 0, 0.0)?;
        let yesterday = today.add_days(-1)?;
        let week = today.add_days(-(now.day_of_week() - 1))?;
        let month = glib::DateTime::from_local(now.year(), now.month(), 1, 0, 0, 0.0)?;
        Ok(Self::new(
            today.to_unix(),
            yesterday.to_unix(),
            week.to_unix(),
            month.to_unix(),
        ))
    }

    pub fn local_now() -> Result<Self, glib::BoolError> {
        Self::local(&glib::DateTime::now_local()?)
    }

    pub fn section_of(&self, date: i64) -> u8 {
        self.starts
            .iter()
            .position(|start| date >= *start)
            .map_or(OLDER, |index| index as u8)
    }
}

/// Section ranges in list positions: `ends[i]` is one past the last position of section `i`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub ends: [u32; 5],
}

impl Bounds {
    /// `newer_than[i]` is the number of rows dated at or after `Thresholds::starts[i]`.
    pub fn from_counts(newer_than: [u32; 4], total: u32) -> Self {
        Self {
            ends: [
                newer_than[0],
                newer_than[1],
                newer_than[2],
                newer_than[3],
                total,
            ],
        }
    }

    pub fn total(&self) -> u32 {
        self.ends[4]
    }

    /// The section at `position` and its `[start, end)`; past the end, GTK's convention.
    pub fn section_at(&self, position: u32) -> (u8, u32, u32) {
        if position >= self.total() {
            return (OLDER, self.total(), u32::MAX);
        }
        let index = self
            .ends
            .iter()
            .position(|end| position < *end)
            .unwrap_or(4);
        let start = if index == 0 { 0 } else { self.ends[index - 1] };
        (index as u8, start, self.ends[index])
    }

    pub fn insert(&mut self, section: u8) {
        for end in &mut self.ends[usize::from(section)..] {
            *end += 1;
        }
    }

    pub fn remove(&mut self, section: u8) {
        for end in &mut self.ends[usize::from(section)..] {
            *end -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_sections_are_skipped() {
        let day = 86_400;
        // A Sunday whose month started on the Thursday of the same week: "This month" is empty.
        let today = 100 * day;
        let thresholds = Thresholds::new(today, today - day, today - 6 * day, today - 3 * day);
        assert_eq!(thresholds.starts[3], thresholds.starts[2]);
        assert_eq!(thresholds.section_of(today + 5), 0);
        assert_eq!(thresholds.section_of(today - 5), 1);
        assert_eq!(thresholds.section_of(today - 2 * day), 2);
        assert_eq!(thresholds.section_of(today - 30 * day), OLDER);

        let bounds = Bounds::from_counts([3, 7, 20, 20], 100);
        assert_eq!(bounds.section_at(0), (0, 0, 3));
        assert_eq!(bounds.section_at(3), (1, 3, 7));
        assert_eq!(bounds.section_at(19), (2, 7, 20));
        assert_eq!(bounds.section_at(20), (4, 20, 100));
        assert_eq!(bounds.section_at(100), (4, 100, u32::MAX));
    }

    #[test]
    fn insert_shifts_later_sections() {
        let mut bounds = Bounds::from_counts([0, 7, 20, 20], 100);
        assert_eq!(bounds.section_at(0), (1, 0, 7));
        bounds.insert(0);
        assert_eq!(bounds.section_at(0), (0, 0, 1));
        assert_eq!(bounds.section_at(1), (1, 1, 8));
        assert_eq!(bounds.total(), 101);
        bounds.remove(0);
        assert_eq!(bounds, Bounds::from_counts([0, 7, 20, 20], 100));
    }
}
