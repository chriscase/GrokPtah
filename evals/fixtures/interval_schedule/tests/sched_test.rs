use schedule_lib::{is_compatible, max_non_overlapping, merge_intervals, Interval};

fn iv(s: i64, e: i64) -> Interval {
    Interval { start: s, end: e }
}

#[test]
fn compatible_touching_ok() {
    let v = vec![iv(0, 2), iv(2, 5), iv(5, 6)];
    assert!(is_compatible(&v));
}

#[test]
fn compatible_unsorted() {
    let v = vec![iv(5, 6), iv(0, 2), iv(2, 5)];
    assert!(is_compatible(&v));
}

#[test]
fn compatible_overlap_false() {
    assert!(!is_compatible(&[iv(0, 3), iv(2, 4)]));
}

#[test]
fn max_selection() {
    let v = vec![iv(1, 4), iv(3, 5), iv(0, 6), iv(5, 7), iv(3, 9), iv(5, 9), iv(6, 10), iv(8, 11), iv(8, 12), iv(2, 14), iv(12, 16)];
    // Optimal classic example = 4
    assert_eq!(max_non_overlapping(&v), 4);
}

#[test]
fn max_empty() {
    assert_eq!(max_non_overlapping(&[]), 0);
}

#[test]
fn merge_basic() {
    let v = vec![iv(1, 3), iv(2, 6), iv(8, 10), iv(15, 18)];
    assert_eq!(merge_intervals(&v), vec![iv(1, 6), iv(8, 10), iv(15, 18)]);
}

#[test]
fn merge_touching() {
    let v = vec![iv(1, 4), iv(4, 5)];
    assert_eq!(merge_intervals(&v), vec![iv(1, 5)]);
}

#[test]
fn merge_unsorted() {
    let v = vec![iv(8, 10), iv(1, 3), iv(2, 6)];
    assert_eq!(merge_intervals(&v), vec![iv(1, 6), iv(8, 10)]);
}
