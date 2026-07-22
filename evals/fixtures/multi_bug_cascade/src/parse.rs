/// Parse "a,b" into (a,b). BUG: splits on whitespace not comma.
pub fn parse_pair(s: &str) -> Option<(i32, i32)> {
    let parts: Vec<_> = s.split_whitespace().collect();
    if parts.len() != 2 {
        return None;
    }
    Some((parts[0].parse().ok()?, parts[1].parse().ok()?))
}
