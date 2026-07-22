use test_fix_fixture::max_i32;

#[test]
fn max_picks_larger() {
    assert_eq!(max_i32(1, 2), 2);
    assert_eq!(max_i32(5, 3), 5);
}
