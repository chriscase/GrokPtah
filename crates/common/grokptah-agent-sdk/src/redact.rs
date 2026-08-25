//! Shared fail-closed redaction checks for public event projections.

pub(crate) fn contains_control(value: &str) -> bool {
    value
        .chars()
        .any(|character| character.is_control() || character == '\u{7f}')
}

pub(crate) fn contains_privileged_text(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "/users/",
        "/private/",
        "/var/",
        "/tmp/",
        "/home/",
        "/volumes/",
        "\\users\\",
        "http://",
        "https://",
        "authorization",
        "bearer ",
        "api_key",
        "xai_api_key",
        "grokptah_home",
        "clipboard",
        "private_key",
        "password",
        "cookie",
        "session_token",
        "secret",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

pub(crate) fn reject_bounded_text(
    value: &str,
    max_bytes: usize,
    empty_or_bound_err: &'static str,
    privileged_err: &'static str,
) -> Result<(), &'static str> {
    if value.trim().is_empty() || value.len() > max_bytes || contains_control(value) {
        return Err(empty_or_bound_err);
    }
    if contains_privileged_text(value) {
        return Err(privileged_err);
    }
    Ok(())
}

pub(crate) fn is_share_safe_event_type(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }
    value.len() <= 64
        && value.as_bytes()[1..].iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.' | b'-')
        })
        && !contains_privileged_text(value)
}
