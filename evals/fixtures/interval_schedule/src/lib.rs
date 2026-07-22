/// Half-open interval [start, end).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    pub start: i64,
    pub end: i64,
}

/// Return true if all intervals are pairwise non-overlapping (touching ends OK).
pub fn is_compatible(intervals: &[Interval]) -> bool {
    // BUG: only checks adjacent after sort by start, but uses wrong comparison
    // (treats touching as overlap) and forgets to sort.
    for w in intervals.windows(2) {
        if w[0].end >= w[1].start {
            return false;
        }
    }
    true
}

/// Maximum number of non-overlapping intervals (touching ends allowed).
/// Classic activity selection — implement correctly.
pub fn max_non_overlapping(intervals: &[Interval]) -> usize {
    // Wrong stub: returns length always.
    intervals.len()
}

/// Merge overlapping intervals; touching ends should merge into one.
pub fn merge_intervals(intervals: &[Interval]) -> Vec<Interval> {
    // Wrong: returns input unchanged.
    intervals.to_vec()
}
