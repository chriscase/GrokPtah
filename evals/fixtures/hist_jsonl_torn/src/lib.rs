use serde_json::Value;

/// Load JSONL records from a transcript file.
/// BUG (historical): `lines()` + parse all fails the whole load when the last
/// line is a torn (incomplete) JSON write.
pub fn load_jsonl(text: &str) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line)
            .map_err(|e| format!("line {}: {e}", i + 1))?;
        out.push(v);
    }
    Ok(out)
}
