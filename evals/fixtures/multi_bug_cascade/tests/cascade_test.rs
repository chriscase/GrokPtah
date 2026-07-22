use cascade_lib::{clamp_u8, parse_pair, title_case};

#[test]
fn clamp_inclusive() {
    assert_eq!(clamp_u8(0), 0);
    assert_eq!(clamp_u8(255), 255);
    assert_eq!(clamp_u8(300), 255);
    assert_eq!(clamp_u8(-5), 0);
}

#[test]
fn parse_comma_pair() {
    assert_eq!(parse_pair("3,4"), Some((3, 4)));
    assert_eq!(parse_pair(" 3, 4 "), Some((3, 4)));
    assert_eq!(parse_pair("nope"), None);
}

#[test]
fn title_case_words() {
    assert_eq!(title_case("hello world"), "Hello World");
    assert_eq!(title_case("a"), "A");
    assert_eq!(title_case(""), "");
}
