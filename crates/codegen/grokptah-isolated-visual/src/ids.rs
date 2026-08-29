use sha2::{Digest, Sha256};

use crate::error::{IsolatedError, IsolatedResult};

pub const MAX_ID_BYTES: usize = 256;
pub const MAX_RELATIVE_PATH_BYTES: usize = 256;
pub const SCHEMA_VERSION: u32 = 1;
pub const ISOLATED_VISUAL_BACKEND_ID: &str = "macos_isolated_visual_candidate_v1";
pub const LIVE_DESKTOP_CONFLICT_DOMAIN: &str = "conflict-foreground-live-desktop-v1";
pub const GUEST_PROTOCOL_VERSION: u32 = 1;

pub fn validate_id(name: &str, value: &str) -> IsolatedResult<()> {
    if value.trim().is_empty()
        || value.len() > MAX_ID_BYTES
        || value.contains('\0')
        || value.contains('/')
        || value.contains('\\')
        || value.contains("..")
    {
        return Err(IsolatedError::invalid(format!("invalid {name}")));
    }
    Ok(())
}

pub fn validate_digest(name: &str, value: &str) -> IsolatedResult<()> {
    if value.len() != 64
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
        || value.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return Err(IsolatedError::invalid(format!(
            "{name} must be a lowercase SHA-256 digest"
        )));
    }
    Ok(())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex_encode(&Sha256::digest(bytes))
}

pub fn hex_encode(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

pub fn safe_file_id(id: &str) -> IsolatedResult<String> {
    validate_id("record_id", id)?;
    if !id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'_' || byte == b'-')
    {
        return Err(IsolatedError::invalid(
            "durable record id is not filename-safe",
        ));
    }
    Ok(id.to_string())
}

/// Host-issued conflict-domain identity. Callers cannot self-attest this value.
pub fn isolated_conflict_domain_id(guest_id: &str) -> String {
    let digest = Sha256::digest(
        [
            b"grokptah-computer-conflict-v1\0isolated-guest\0".as_slice(),
            guest_id.as_bytes(),
        ]
        .concat(),
    );
    format!("conflict-isolated-{}", hex_encode(&digest[..16]))
}

pub fn isolated_input_domain_id(guest_id: &str) -> String {
    let digest = Sha256::digest(
        [
            b"grokptah-computer-input-v1\0isolated-guest\0".as_slice(),
            guest_id.as_bytes(),
        ]
        .concat(),
    );
    format!("input-isolated-{}", hex_encode(&digest[..16]))
}

/// Relative source paths are ASCII, NFC-equivalent by construction, and cannot
/// traverse, collide case-insensitively, or carry a Windows separator.
pub fn validate_relative_path(path: &str) -> IsolatedResult<()> {
    if path.is_empty()
        || path.len() > MAX_RELATIVE_PATH_BYTES
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains('\0')
        || path.contains('\\')
        || path.contains("//")
        || !path.is_ascii()
    {
        return Err(IsolatedError::forbidden(
            "source path is not a hermetic relative allowlist path",
        ));
    }
    let mut depth = 0u8;
    for component in path.split('/') {
        depth = depth.saturating_add(1);
        if depth > 8
            || component.is_empty()
            || component == "."
            || component == ".."
            || component.starts_with('.')
            || !component.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || byte == b'.'
                    || byte == b'_'
                    || byte == b'-'
            })
        {
            return Err(IsolatedError::forbidden(
                "source path component is not allowlisted (traversal, hidden, case, or unicode)",
            ));
        }
    }
    Ok(())
}

pub fn casefold_key(path: &str) -> String {
    path.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_reject_traversal_case_and_unicode() {
        validate_relative_path("guest-init.c").unwrap();
        assert!(validate_relative_path("../etc/passwd").is_err());
        assert!(validate_relative_path("Guest-Init.c").is_err());
        assert!(validate_relative_path("guest\u{2215}init.c").is_err());
        assert!(validate_relative_path(".gitmodules").is_err());
        assert!(validate_relative_path("foo/../../bar").is_err());
        assert!(validate_relative_path("/abs").is_err());
    }
}
