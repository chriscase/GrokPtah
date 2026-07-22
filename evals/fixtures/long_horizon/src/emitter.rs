pub fn emit(parts: &[&str]) -> String {
    // BUG-42: wrong prefix (should be OUT: with colon)
    format!("out> {}", parts.join(" "))
}
