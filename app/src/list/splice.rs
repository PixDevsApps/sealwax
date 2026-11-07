//! How a list of rows becomes another while moving as few rows as it can (issue #29): the rows
//! both lists hold, in the same order, stay; the others are removed and inserted. A view that
//! changes its rows this way keeps its scroll place, its selection and its keyboard focus on the
//! rows that stay, where replacing every row loses all three.

use std::collections::HashMap;
use std::hash::Hash;

/// The changes that turn the old rows into the new ones: remove `removed` (old positions, last
/// first), then insert `inserted` (new positions, first first); `kept` pairs each row that
/// stays, as `(old position, new position)`, in order.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Splice {
    pub removed: Vec<u32>,
    pub inserted: Vec<u32>,
    pub kept: Vec<(u32, u32)>,
}

/// The changes from `old` to `new`, rows matched by key; `stays(old, new)` says whether a
/// matched row may stay where it is (a row that must move to another section can't). A key twice
/// in a list matches once.
pub(super) fn splice<K: Eq + Hash>(
    old: &[K],
    new: &[K],
    stays: impl Fn(usize, usize) -> bool,
) -> Splice {
    let mut wanted: HashMap<&K, usize> = HashMap::with_capacity(new.len());
    for (index, key) in new.iter().enumerate() {
        wanted.entry(key).or_insert(index);
    }
    // The matched rows in the old order, with their new positions.
    let matched: Vec<(usize, usize)> = old
        .iter()
        .enumerate()
        .filter_map(|(from, key)| {
            let to = wanted.remove(key)?;
            stays(from, to).then_some((from, to))
        })
        .collect();
    let kept = longest_rising(&matched);
    let mut stay_old = vec![false; old.len()];
    let mut stay_new = vec![false; new.len()];
    for &(from, to) in &kept {
        stay_old[from] = true;
        stay_new[to] = true;
    }
    Splice {
        removed: (0..old.len())
            .rev()
            .filter(|&index| !stay_old[index])
            .map(|index| index as u32)
            .collect(),
        inserted: (0..new.len())
            .filter(|&index| !stay_new[index])
            .map(|index| index as u32)
            .collect(),
        kept: kept
            .into_iter()
            .map(|(from, to)| (from as u32, to as u32))
            .collect(),
    }
}

/// The longest run of `pairs` (in rising first position) whose second positions rise too.
fn longest_rising(pairs: &[(usize, usize)]) -> Vec<(usize, usize)> {
    // tails[n]: the index into `pairs` of the smallest end of a rising run of n + 1 pairs.
    let mut tails: Vec<usize> = Vec::new();
    let mut before: Vec<Option<usize>> = vec![None; pairs.len()];
    for (index, &(_, to)) in pairs.iter().enumerate() {
        let length = tails.partition_point(|&tail| pairs[tail].1 < to);
        before[index] = length.checked_sub(1).map(|previous| tails[previous]);
        if length == tails.len() {
            tails.push(index);
        } else {
            tails[length] = index;
        }
    }
    let mut run = Vec::with_capacity(tails.len());
    let mut next = tails.last().copied();
    while let Some(index) = next {
        run.push(pairs[index]);
        next = before[index];
    }
    run.reverse();
    run
}

/// Runs of `positions` that follow each other, as `(first, count)`: for removals given last
/// first, each run's first is its lowest position.
pub(super) fn runs(positions: &[u32]) -> Vec<(u32, u32)> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &position in positions {
        match runs.last_mut() {
            Some((first, count)) if position + 1 == *first => {
                *first = position;
                *count += 1;
            }
            Some((first, count)) if *first + *count == position => *count += 1,
            _ => runs.push((position, 1)),
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(text: &str) -> Vec<char> {
        text.chars().collect()
    }

    /// Applies `splice` to `old` as a list model would, taking new rows from `new`.
    fn apply(old: &str, new: &str, splice: &Splice) -> String {
        let mut rows = keys(old);
        let new = keys(new);
        for &position in &splice.removed {
            rows.remove(position as usize);
        }
        for &position in &splice.inserted {
            rows.insert(position as usize, new[position as usize]);
        }
        rows.into_iter().collect()
    }

    fn check(old: &str, new: &str) -> Splice {
        let splice = splice(&keys(old), &keys(new), |_, _| true);
        assert_eq!(apply(old, new, &splice), new, "{old} → {new}: {splice:?}");
        splice
    }

    #[test]
    fn the_same_rows_change_nothing() {
        let splice = check("abcdef", "abcdef");
        assert!(splice.removed.is_empty() && splice.inserted.is_empty());
        assert_eq!(splice.kept.len(), 6);
    }

    #[test]
    fn a_new_row_on_top_or_at_the_bottom_is_one_insertion() {
        assert_eq!(check("abcdef", "xabcdef").inserted, [0]);
        assert_eq!(check("abcdef", "abcdefx").inserted, [6]);
        assert!(check("abcdef", "xabcdef").removed.is_empty());
    }

    #[test]
    fn a_row_taken_out_is_one_removal() {
        let splice = check("abcdef", "abdef");
        assert_eq!(splice.removed, [2]);
        assert!(splice.inserted.is_empty());
    }

    #[test]
    fn a_row_moved_to_the_top_moves_alone() {
        let splice = check("abcdef", "eabcdf");
        assert_eq!(splice.removed, [4]);
        assert_eq!(splice.inserted, [0]);
    }

    #[test]
    fn a_row_that_may_not_stay_is_moved_even_in_place() {
        let old = keys("abcdef");
        let splice = splice(&old, &old, |from, _| from != 3);
        assert_eq!(splice.removed, [3]);
        assert_eq!(splice.inserted, [3]);
        assert_eq!(apply("abcdef", "abcdef", &splice), "abcdef");
    }

    #[test]
    fn mixed_changes_and_empty_lists() {
        check("", "abc");
        check("abc", "");
        check("abcdefgh", "hgfedcba");
        check("abcdefgh", "xbdyfhz");
        check("aabbc", "cba");
        check("abc", "aabbcc");
    }

    #[test]
    fn rows_that_follow_each_other_are_one_run() {
        assert_eq!(runs(&[7, 6, 5, 2, 1]), [(5, 3), (1, 2)]);
        assert_eq!(runs(&[0, 1, 2, 5, 6]), [(0, 3), (5, 2)]);
        assert!(runs(&[]).is_empty());
    }
}
