//! The list's sections (DESIGN.md "Message list"): Pinned (issue #26: an Inbox's newest mail from
//! pinned senders, ahead of the dates), then the date sections Today, Yesterday, This week (from
//! Monday), This month and Older, starting at local midnights. Ported from S3. A list is ordered
//! newest first after its Pinned rows, so each section is one run of positions; [`Bounds`] holds
//! where the runs end and shifts them as rows come and go.

use gtk4::glib;
use std::time::Duration;

pub const LABELS: [&str; SECTIONS] = [
    "Pinned",
    "Today",
    "Yesterday",
    "This week",
    "This month",
    "Older",
];
pub const SECTIONS: usize = 6;
pub const PINNED: usize = 0;
pub const OLDER: usize = SECTIONS - 1;

/// Start times of the first four sections (Unix seconds, local midnights), non-increasing, so a
/// section that is empty on a given day (This week on a Monday) starts where the one before it
/// does.
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

    /// The sections on the day of `now`, in `now`'s time zone; weeks start on Monday.
    pub fn at(now: &glib::DateTime) -> Result<Self, glib::BoolError> {
        let zone = now.timezone();
        let (year, month, day) = now.ymd();
        let today = glib::DateTime::new(&zone, year, month, day, 0, 0, 0.0)?;
        let yesterday = today.add_days(-1)?;
        let week = today.add_days(-(now.day_of_week() - 1))?;
        let first = glib::DateTime::new(&zone, year, month, 1, 0, 0, 0.0)?;
        Ok(Self::new(
            today.to_unix(),
            yesterday.to_unix(),
            week.to_unix(),
            first.to_unix(),
        ))
    }

    /// The date section of a row dated `date` (never [`PINNED`]).
    pub fn section_of(&self, date: i64) -> usize {
        self.starts
            .iter()
            .position(|start| date >= *start)
            .map_or(OLDER, |index| index + 1)
    }

    /// The starts as the store's `OpenView::section_starts` takes them.
    pub fn starts(&self) -> Vec<i64> {
        self.starts.to_vec()
    }
}

/// How long until the sections change after `now`: the next local midnight.
pub fn until_next_day(now: &glib::DateTime) -> Duration {
    let (year, month, day) = now.ymd();
    let next = glib::DateTime::new(&now.timezone(), year, month, day, 0, 0, 0.0)
        .and_then(|today| today.add_days(1));
    match next {
        Ok(next) => Duration::from_secs((next.to_unix() - now.to_unix()).max(1) as u64),
        Err(_) => Duration::from_secs(3600),
    }
}

/// Where each section ends in list positions: `ends[i]` is one past the last row of section `i`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bounds {
    ends: [u32; SECTIONS],
}

impl Bounds {
    /// From the Pinned rows, the rows before the first one dated before each section start (the
    /// Pinned ones included), as the store counts them, and the row count.
    pub fn from_counts(pinned: u32, counts: &[u32], total: u32) -> Self {
        let mut ends = [0; SECTIONS];
        ends[PINNED] = pinned.min(total);
        ends[OLDER] = total;
        for (end, count) in ends[PINNED + 1..OLDER].iter_mut().zip(counts) {
            *end = (*count).min(total);
        }
        for index in 1..ends.len() {
            ends[index] = ends[index].max(ends[index - 1]);
        }
        Self { ends }
    }

    /// From the dates of rows ordered newest first.
    pub fn from_dates(dates: impl IntoIterator<Item = i64>, thresholds: &Thresholds) -> Self {
        let mut counts = [0u32; SECTIONS];
        for date in dates {
            counts[thresholds.section_of(date)] += 1;
        }
        let mut ends = [0; SECTIONS];
        let mut sum = 0;
        for (end, count) in ends.iter_mut().zip(counts) {
            sum += count;
            *end = sum;
        }
        Self { ends }
    }

    pub fn total(&self) -> u32 {
        self.ends[OLDER]
    }

    /// Rows in each section.
    pub fn counts(&self) -> [u32; SECTIONS] {
        let mut counts = [0; SECTIONS];
        let mut start = 0;
        for (count, end) in counts.iter_mut().zip(self.ends) {
            *count = end - start;
            start = end;
        }
        counts
    }

    /// The section at `position` and its `[start, end)`; past the end, GTK's convention
    /// `(total, u32::MAX)`.
    pub fn section_at(&self, position: u32) -> (usize, u32, u32) {
        if position >= self.total() {
            return (OLDER, self.total(), u32::MAX);
        }
        let index = self
            .ends
            .iter()
            .position(|end| position < *end)
            .unwrap_or(OLDER);
        let start = if index == 0 { 0 } else { self.ends[index - 1] };
        (index, start, self.ends[index])
    }

    /// A row was added to `section`.
    pub fn insert(&mut self, section: usize) {
        for end in &mut self.ends[section..] {
            *end += 1;
        }
    }

    /// A row was taken from `section`.
    pub fn remove(&mut self, section: usize) {
        for end in &mut self.ends[section..] {
            *end = end.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    #[test]
    fn empty_sections_start_where_the_one_before_does() {
        // A Monday: This week starts today, so it is empty and starts at Yesterday.
        let today = 100 * DAY;
        let thresholds = Thresholds::new(today, today - DAY, today, today - 20 * DAY);
        assert_eq!(thresholds.starts[2], thresholds.starts[1]);
        assert_eq!(thresholds.section_of(today + 5), 1);
        assert_eq!(thresholds.section_of(today - 5), 2);
        assert_eq!(thresholds.section_of(today - 3 * DAY), 4);
        assert_eq!(thresholds.section_of(today - 40 * DAY), OLDER);
        assert_eq!(thresholds.section_of(0), OLDER);
    }

    #[test]
    fn thresholds_follow_the_calendar() {
        // Wednesday 2026-10-07 12:00 UTC: Monday the 5th starts the week, the 1st the month.
        let utc = glib::TimeZone::utc();
        let now = glib::DateTime::new(&utc, 2026, 10, 7, 12, 0, 0.0).unwrap();
        let thresholds = Thresholds::at(&now).unwrap();
        let midnight = |day| {
            glib::DateTime::new(&utc, 2026, 10, day, 0, 0, 0.0)
                .unwrap()
                .to_unix()
        };
        assert_eq!(
            thresholds.starts,
            [midnight(7), midnight(6), midnight(5), midnight(1)]
        );
        assert_eq!(until_next_day(&now), Duration::from_secs(12 * 3600));
        // Thursday the 1st: the month starts today, so This week (from Monday the
        // previous month) comes before This month, which is empty.
        let first = glib::DateTime::new(&utc, 2026, 10, 1, 9, 0, 0.0).unwrap();
        let thresholds = Thresholds::at(&first).unwrap();
        assert_eq!(thresholds.starts[3], thresholds.starts[2]);
    }

    #[test]
    fn bounds_from_counts_and_dates_agree() {
        let today = 100 * DAY;
        let thresholds = Thresholds::new(today, today - DAY, today - 2 * DAY, today - 9 * DAY);
        let dates = [
            today + 10,
            today + 5,
            today - 100,
            today - DAY - 100,
            today - 5 * DAY,
            today - 30 * DAY,
            today - 31 * DAY,
        ];
        let from_dates = Bounds::from_dates(dates, &thresholds);
        let from_counts = Bounds::from_counts(0, &[2, 3, 4, 5], 7);
        assert_eq!(from_dates, from_counts);
        assert_eq!(from_dates.counts(), [0, 2, 1, 1, 1, 2]);
        assert_eq!(from_dates.section_at(0), (1, 0, 2));
        assert_eq!(from_dates.section_at(2), (2, 2, 3));
        assert_eq!(from_dates.section_at(6), (OLDER, 5, 7));
        assert_eq!(from_dates.section_at(7), (OLDER, 7, u32::MAX));
    }

    #[test]
    fn rows_shift_later_sections() {
        let mut bounds = Bounds::from_counts(0, &[0, 7, 20, 20], 100);
        assert_eq!(bounds.section_at(0), (2, 0, 7));
        bounds.insert(1);
        assert_eq!(bounds.section_at(0), (1, 0, 1));
        assert_eq!(bounds.section_at(1), (2, 1, 8));
        assert_eq!(bounds.total(), 101);
        bounds.remove(1);
        assert_eq!(bounds, Bounds::from_counts(0, &[0, 7, 20, 20], 100));
        bounds.remove(OLDER);
        assert_eq!(bounds.total(), 99);
        assert_eq!(bounds.counts(), [0, 0, 7, 13, 0, 79]);
    }

    #[test]
    fn counts_past_the_total_are_clamped() {
        let bounds = Bounds::from_counts(0, &[5, 3, 9, 12], 10);
        assert_eq!(bounds.counts(), [0, 5, 0, 4, 1, 0]);
        assert_eq!(Bounds::from_counts(0, &[], 4).counts(), [0, 0, 0, 0, 0, 4]);
    }

    #[test]
    fn the_pinned_rows_come_first() {
        // Two pinned rows, then one today, two yesterday, none this week or month, four older.
        let bounds = Bounds::from_counts(2, &[3, 5, 5, 5], 9);
        assert_eq!(bounds.counts(), [2, 1, 2, 0, 0, 4]);
        assert_eq!(bounds.section_at(0), (PINNED, 0, 2));
        assert_eq!(bounds.section_at(1), (PINNED, 0, 2));
        assert_eq!(bounds.section_at(2), (1, 2, 3));
        assert_eq!(
            Bounds::from_counts(12, &[3], 9).counts(),
            [9, 0, 0, 0, 0, 0]
        );
    }
}
