pub fn parse(s: &str) -> Vec<&str> {
    s.split_whitespace().collect()
}
