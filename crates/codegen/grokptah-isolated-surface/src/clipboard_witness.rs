//! Host clipboard witness for the Contained Browser clipboard kill-gate.
//!
//! Seals a before/after digest of the host pasteboard **change count and type
//! names only**. Never reads pasteboard data, never writes the general
//! pasteboard, and never uses CGEvent, Accessibility, or AppleScript.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{HarnessError, HarnessResult};

const DIGEST_PREFIX: &str = "sha256:";

/// Whether [`ClipboardWitness`] can collect a native host digest here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardWitnessPlatform {
    MacOs,
    NonMacOs,
}

/// Sealed host-clipboard fingerprint. Public fields never contain pasteboard
/// payloads, file paths, or private strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostClipboardDigest {
    /// Canonical `sha256:` + 64 lowercase hex over change-count + type names.
    pub digest: String,
    pub type_count: u32,
    /// True only when pasteboard *data* was read. Must stay false.
    pub contents_read: bool,
    /// True only when the general pasteboard was written. Must stay false.
    pub pasteboard_mutated: bool,
}

impl HostClipboardDigest {
    pub fn canonical(digest_bytes: &[u8], type_count: u32) -> Self {
        Self {
            digest: format!("{DIGEST_PREFIX}{:x}", Sha256::digest(digest_bytes)),
            type_count,
            contents_read: false,
            pasteboard_mutated: false,
        }
    }

    pub fn is_honest_sentinel(&self) -> bool {
        !self.contents_read && !self.pasteboard_mutated && is_canonical_digest(&self.digest)
    }
}

/// Host-side witness. Collects sealed digests without mutating NSPasteboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardWitness;

impl ClipboardWitness {
    pub fn platform() -> ClipboardWitnessPlatform {
        #[cfg(target_os = "macos")]
        {
            ClipboardWitnessPlatform::MacOs
        }
        #[cfg(not(target_os = "macos"))]
        {
            ClipboardWitnessPlatform::NonMacOs
        }
    }

    /// Seal the current host clipboard fingerprint.
    ///
    /// Non-macOS is deterministic [`HarnessErrorCode::BackendUnavailable`]
    /// with an unsupported-platform message. macOS failures are also
    /// fail-closed and never fabricate a digest.
    pub fn seal_digest() -> HarnessResult<HostClipboardDigest> {
        #[cfg(target_os = "macos")]
        {
            macos::seal_digest()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(HarnessError::backend_unavailable(
                "ClipboardWitness is unsupported on non-macOS; host clipboard digest is unavailable",
            ))
        }
    }
}

pub fn is_canonical_digest(digest: &str) -> bool {
    let rest = match digest.strip_prefix(DIGEST_PREFIX) {
        Some(rest) => rest,
        None => return false,
    };
    rest.len() == 64 && rest.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Compare two honest sealed digests. Any contents-read / mutation flag is
/// treated as an unexpected host clipboard change (fail closed).
pub fn host_clipboard_unchanged(before: &HostClipboardDigest, after: &HostClipboardDigest) -> bool {
    before.is_honest_sentinel()
        && after.is_honest_sentinel()
        && before.digest == after.digest
        && before.type_count == after.type_count
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use sha2::{Digest, Sha256};
    use std::sync::OnceLock;

    use super::HostClipboardDigest;
    use crate::error::{HarnessError, HarnessResult};

    pub fn seal_digest() -> HarnessResult<HostClipboardDigest> {
        let pasteboard = general_pasteboard().ok_or_else(|| {
            HarnessError::backend_unavailable(
                "ClipboardWitness NSPasteboard.generalPasteboard unavailable",
            )
        })?;
        let change_count: isize = unsafe { objc2::msg_send![&*pasteboard, changeCount] };
        let types: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![&*pasteboard, types] };

        let mut hasher = Sha256::new();
        hasher.update(b"grokptah-clipboard-witness-v1\0");
        hasher.update(change_count.to_le_bytes());
        let mut type_count = 0u32;
        if let Some(types) = types {
            let count: usize = unsafe { objc2::msg_send![&*types, count] };
            type_count = u32::try_from(count).unwrap_or(u32::MAX);
            hasher.update((count as u64).to_le_bytes());
            for index in 0..count {
                let item: Retained<AnyObject> =
                    unsafe { objc2::msg_send![&*types, objectAtIndex: index] };
                if let Some(type_name) = nsstring_to_string(Some(&*item)) {
                    hasher.update(type_name.as_bytes());
                    hasher.update(b"\0");
                }
            }
        }
        Ok(HostClipboardDigest {
            digest: format!("sha256:{:x}", hasher.finalize()),
            type_count,
            contents_read: false,
            pasteboard_mutated: false,
        })
    }

    fn general_pasteboard() -> Option<Retained<AnyObject>> {
        if !appkit_loaded() {
            return None;
        }
        let cls = AnyClass::get(c"NSPasteboard")?;
        unsafe { objc2::msg_send![cls, generalPasteboard] }
    }

    fn appkit_loaded() -> bool {
        static LOADED: OnceLock<bool> = OnceLock::new();
        *LOADED.get_or_init(|| {
            let path = c"/System/Library/Frameworks/AppKit.framework/AppKit";
            let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_LAZY) };
            !handle.is_null()
        })
    }

    fn nsstring_to_string(object: Option<&AnyObject>) -> Option<String> {
        let object = object?;
        let utf8: *const i8 = unsafe { objc2::msg_send![object, UTF8String] };
        if utf8.is_null() {
            return None;
        }
        unsafe {
            std::ffi::CStr::from_ptr(utf8)
                .to_str()
                .ok()
                .map(str::to_owned)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_macos_seal_is_unsupported() {
        let result = ClipboardWitness::seal_digest();
        #[cfg(not(target_os = "macos"))]
        {
            let err = result.expect_err("linux seal");
            assert_eq!(err.code, crate::error::HarnessErrorCode::BackendUnavailable);
            assert!(err.message.contains("unsupported on non-macOS"));
            assert_eq!(
                ClipboardWitness::platform(),
                ClipboardWitnessPlatform::NonMacOs
            );
        }
        #[cfg(target_os = "macos")]
        {
            let _ = result;
            assert_eq!(
                ClipboardWitness::platform(),
                ClipboardWitnessPlatform::MacOs
            );
        }
    }

    #[test]
    fn contents_read_or_mutation_is_not_honest() {
        let mut digest = HostClipboardDigest::canonical(b"fixture", 1);
        assert!(digest.is_honest_sentinel());
        digest.contents_read = true;
        assert!(!digest.is_honest_sentinel());
        digest.contents_read = false;
        digest.pasteboard_mutated = true;
        assert!(!digest.is_honest_sentinel());
    }

    #[test]
    fn drifted_digests_are_not_unchanged() {
        let before = HostClipboardDigest::canonical(b"before", 1);
        let after = HostClipboardDigest::canonical(b"after", 1);
        assert!(!host_clipboard_unchanged(&before, &after));
        assert!(host_clipboard_unchanged(&before, &before));
    }
}
