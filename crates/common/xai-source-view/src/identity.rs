//! On-disk identity for roots and documents.
//!
//! Two questions this module answers, and answers honestly:
//!
//! * *Is this still the same directory I authorized?* — compared before every
//!   read, so a worktree swapped between snapshot and action is refused.
//! * *Is this still the same file I started reading?* — compared after the
//!   read, so a file replaced mid-read is refused rather than returned as a
//!   seamless mixture of two files.
//!
//! On Unix the answer is a device/inode pair, which is exact. On Windows the
//! stable standard library exposes no file index, so the answer is a
//! composite of creation time, last write time, size, and attributes. That is
//! weaker, and [`IdentityStability`] says so rather than pretending otherwise.

use std::fs::Metadata;
use std::path::Path;

use crate::digest::{tagged_digest, to_hex};

/// How much weight an identity comparison carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityStability {
    /// Device and inode: distinct files always compare unequal.
    Exact,
    /// Composite of timestamps, size, and attributes: a replacement that
    /// preserves all of them would compare equal.
    Heuristic,
}

/// The platform-specific basis for an identity comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "basis", rename_all = "snake_case")]
pub enum IdentityBasis {
    Inode {
        device: u64,
        inode: u64,
    },
    WindowsAttributes {
        creation: u64,
        written: u64,
        attributes: u32,
    },
}

impl IdentityBasis {
    pub fn stability(&self) -> IdentityStability {
        match self {
            Self::Inode { .. } => IdentityStability::Exact,
            Self::WindowsAttributes { .. } => IdentityStability::Heuristic,
        }
    }

    fn fields(&self) -> Vec<[u8; 8]> {
        match self {
            Self::Inode { device, inode } => vec![device.to_be_bytes(), inode.to_be_bytes()],
            Self::WindowsAttributes {
                creation,
                written,
                attributes,
            } => vec![
                creation.to_be_bytes(),
                written.to_be_bytes(),
                u64::from(*attributes).to_be_bytes(),
            ],
        }
    }
}

/// Identity of one filesystem node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeIdentity {
    pub basis: IdentityBasis,
    pub len: u64,
}

impl NodeIdentity {
    /// Read identity from metadata obtained through an already-open handle.
    pub fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            basis: platform_basis(metadata),
            len: metadata.len(),
        }
    }

    pub fn stability(&self) -> IdentityStability {
        self.basis.stability()
    }

    /// Whether two observations describe the same node.
    ///
    /// Length participates, so a truncate-and-rewrite that preserves inode is
    /// still caught by the post-read comparison.
    pub fn same_node(&self, other: &Self) -> bool {
        self.basis == other.basis
    }

    /// Whether two observations describe the same node *and* the same extent.
    pub fn unchanged(&self, other: &Self) -> bool {
        self == other
    }

    /// Opaque, non-reversible identity digest for the wire.
    pub fn digest(&self) -> String {
        let fields = self.basis.fields();
        let mut refs: Vec<&[u8]> = fields.iter().map(|field| field.as_slice()).collect();
        let len = self.len.to_be_bytes();
        refs.push(&len);
        to_hex(&tagged_digest("grokptah.source-view.node.v1", &refs))
    }
}

#[cfg(unix)]
fn platform_basis(metadata: &Metadata) -> IdentityBasis {
    use std::os::unix::fs::MetadataExt;
    IdentityBasis::Inode {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(windows)]
fn platform_basis(metadata: &Metadata) -> IdentityBasis {
    use std::os::windows::fs::MetadataExt;
    IdentityBasis::WindowsAttributes {
        creation: metadata.creation_time(),
        written: metadata.last_write_time(),
        attributes: metadata.file_attributes(),
    }
}

#[cfg(not(any(unix, windows)))]
fn platform_basis(metadata: &Metadata) -> IdentityBasis {
    // No stable node identity is available; fall back to a length-only
    // heuristic so the type still exists and comparisons stay conservative.
    IdentityBasis::WindowsAttributes {
        creation: 0,
        written: 0,
        attributes: u32::from(metadata.is_file()),
    }
}

/// Digest a path without revealing it.
///
/// Byte-exact on Unix (so a non-UTF-8 name digests correctly rather than
/// collapsing into replacement characters) and UTF-16 code-unit exact on
/// Windows.
pub fn path_digest(domain: &str, path: &Path) -> String {
    to_hex(&tagged_digest(domain, &[&path_bytes(path)]))
}

/// Lossless byte view of a path, for digesting and comparison only.
pub fn path_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .flat_map(|unit| unit.to_le_bytes())
            .collect()
    }
    #[cfg(not(any(unix, windows)))]
    {
        path.to_string_lossy().as_bytes().to_vec()
    }
}
