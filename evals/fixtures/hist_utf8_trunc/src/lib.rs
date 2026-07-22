/// Truncate to at most `max_bytes` bytes.
/// BUG (historical): slices by raw byte index and panics or produces invalid UTF-8
/// when `max_bytes` lands mid-codepoint (CJK / emoji).
pub fn truncate_bytes(s: &str, max_bytes: usize) -> String {
    if max_bytes >= s.len() {
        return s.to_string();
    }
    // Intentionally wrong: byte slice without char boundary check.
    s[..max_bytes].to_string()
}
