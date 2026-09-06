//! Patch artifact with bounded size and digest for fail-closed Accept.

use sha2::{Digest, Sha256};

use crate::error::{SessionError, SessionResult};

/// Maximum staged patch payload (10 MiB).
pub const MAX_PATCH_BYTES: usize = 10 * 1024 * 1024;

/// Maximum distinct paths referenced in a staged patch.
pub const MAX_PATCH_PATHS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchArtifact {
    pub digest: String,
    pub bytes: Vec<u8>,
    pub paths: Vec<String>,
}

impl PatchArtifact {
    pub fn from_diff(bytes: Vec<u8>, paths: Vec<String>) -> SessionResult<Self> {
        if bytes.len() > MAX_PATCH_BYTES {
            return Err(SessionError::patch_too_large(format!(
                "patch is {} bytes (max {})",
                bytes.len(),
                MAX_PATCH_BYTES
            )));
        }
        if paths.len() > MAX_PATCH_PATHS {
            return Err(SessionError::patch_path_limit(format!(
                "patch touches {} paths (max {})",
                paths.len(),
                MAX_PATCH_PATHS
            )));
        }
        Ok(Self {
            digest: digest_bytes(&bytes),
            bytes,
            paths,
        })
    }

    pub fn verify_digest(&self, expected: &str) -> SessionResult<()> {
        if self.digest != expected {
            return Err(SessionError::patch_digest_mismatch(format!(
                "expected digest {expected}, found {digest}",
                digest = self.digest
            )));
        }
        Ok(())
    }
}

pub fn digest_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

pub fn digest_path(path: &std::path::Path) -> SessionResult<String> {
    let canonical = dunce::canonicalize(path).map_err(|error| {
        SessionError::new(
            crate::error::SessionErrorCode::Internal,
            format!("canonicalize path: {error}"),
        )
    })?;
    Ok(digest_bytes(canonical.as_os_str().as_encoded_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_stable() {
        let a = digest_bytes(b"hello");
        let b = digest_bytes(b"hello");
        assert_eq!(a, b);
        assert!(a.starts_with("sha256:"));
    }
}
