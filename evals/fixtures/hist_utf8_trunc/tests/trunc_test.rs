use clip_util::truncate_bytes;

#[test]
fn ascii_short() {
    assert_eq!(truncate_bytes("hello", 3), "hel");
    assert_eq!(truncate_bytes("hi", 100), "hi");
}

#[test]
fn cjk_mid_codepoint_does_not_panic_and_is_valid() {
    // Each CJK char is 3 bytes. Cap 4 lands mid-second character.
    let s = "你好世界";
    let t = truncate_bytes(s, 4);
    assert!(t.len() <= 4);
    assert!(s.starts_with(&t));
    // Must be valid UTF-8 (to_string already is) and not panic above.
    let _ = format!("{t}…");
}

#[test]
fn emoji_cap_three_is_empty_or_boundary() {
    // 🙂 is 4 bytes; cap 3 must not panic.
    let t = truncate_bytes("🙂", 3);
    assert!(t.len() <= 3);
    assert!(t.is_empty() || "🙂".starts_with(&t));
}
