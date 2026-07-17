//! Safe text helpers shared across the bridge agent path.

/// Truncate `s` to at most `max_bytes` **on a char boundary**.
///
/// Never panics on multi-byte UTF-8 (CJK, emoji). If `max_bytes` falls mid-codepoint,
/// the cut moves back to the previous boundary. Empty when `max_bytes == 0`.
pub fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if max_bytes >= s.len() {
        return s;
    }
    if max_bytes == 0 {
        return "";
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Truncate and append an ellipsis marker when the original exceeded the cap.
pub fn truncate_with_marker(s: &str, max_bytes: usize, marker: &str) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // Leave room for the marker when possible.
    let budget = max_bytes.saturating_sub(marker.len()).max(1);
    let head = truncate_at_char_boundary(s, budget);
    format!("{head}{marker}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_straddling_cap_does_not_panic() {
        // Each CJK ideograph is 3 bytes in UTF-8.
        let s = "你好世界テスト🙂文字".repeat(4_000);
        assert!(s.len() > 32_000, "len={}", s.len());
        // Cap that lands mid-codepoint for many inputs.
        for cap in [1, 2, 3, 31_999, 32_000, 32_001, 10] {
            let t = truncate_at_char_boundary(&s, cap);
            assert!(t.len() <= cap);
            assert!(s.starts_with(t));
            // Round-trip: concatenating remainder (from char boundary) is valid UTF-8.
            let _ = format!("{t}…");
        }
        let marked = truncate_with_marker(&s, 100, "\n… (truncated)");
        assert!(marked.contains("… (truncated)"));
        assert!(marked.is_char_boundary(marked.len()));
    }

    #[test]
    fn emoji_and_short_strings() {
        assert_eq!(truncate_at_char_boundary("hi", 100), "hi");
        assert_eq!(truncate_at_char_boundary("", 10), "");
        assert_eq!(truncate_at_char_boundary("🙂🙂", 0), "");
        // Single emoji is 4 bytes; cap 3 → empty (no partial)
        let t = truncate_at_char_boundary("🙂", 3);
        assert_eq!(t, "");
        let t = truncate_at_char_boundary("🙂", 4);
        assert_eq!(t, "🙂");
    }
}
