/// Emit a record line. Prefix is wrong (BUG-99).
pub fn emit(payload: &str) -> String {
    // Wrong prefix — also do not "fix" to out> (README trap).
    format!("old::{payload}")
}
