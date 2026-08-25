//! Server-owned canonical source manifest.
//!
//! The previous source digest covered an id, a path, a heading, and a
//! visibility label — the *metadata about* a section, not the section. Two
//! different documents with the same heading digested identically, so
//! substituting the bytes behind a citation changed nothing any check could
//! see. `SourceDescriptor.digest` was parsed and then never compared to
//! anything at all.
//!
//! This module owns the manifest instead:
//!
//! 1. **Digest the bytes.** A source digest covers the exact normalized bytes
//!    of the section plus its metadata, so changing one character of the
//!    source changes the digest and every receipt that names it.
//! 2. **Reject duplicate keys before parsing.** `serde_json` silently keeps
//!    the last of two identical keys. A manifest carrying `"visibility":
//!    "private"` followed by `"visibility": "public"` would parse as public.
//!    Raw scanning rejects the document before ordinary parsing runs.
//! 3. **Enforce the descriptor digest.** A descriptor whose digest is not the
//!    one the manifest holds for that id is refused, so a caller cannot
//!    present a stale or fabricated source record.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::contract::Visibility;
use crate::{MAX_ID_BYTES, SourceDescriptor, domain_digest, id_within_bounds};

/// Wire schema id for a manifest.
pub const HELP_MANIFEST_SCHEMA: &str = "grokptah.help-manifest.v1";

/// Longest source body the manifest will accept, in bytes.
pub const MAX_SOURCE_BYTES: usize = 262_144;
/// Most entries a manifest may carry.
pub const MAX_MANIFEST_ENTRIES: usize = 4_096;

/// One source, as the server knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ManifestEntry {
    pub source_id: String,
    pub path: String,
    pub heading: String,
    pub visibility: Visibility,
    pub tenant_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_principal_id: Option<String>,
    /// Digest over the exact normalized section bytes plus this metadata.
    pub digest: String,
}

/// Why a manifest was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// A JSON object carried the same key twice.
    DuplicateKey(String),
    /// The payload was not well-formed JSON.
    Malformed(String),
    /// A field exceeded its bound.
    Bounds(&'static str),
    /// Two entries claimed the same source id.
    DuplicateSourceId(String),
    /// A stored digest did not match the bytes it claims to cover.
    DigestMismatch(String),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateKey(key) => write!(formatter, "duplicate JSON key: {key}"),
            Self::Malformed(detail) => write!(formatter, "malformed manifest: {detail}"),
            Self::Bounds(field) => write!(formatter, "manifest field out of bounds: {field}"),
            Self::DuplicateSourceId(id) => write!(formatter, "duplicate source id: {id}"),
            Self::DigestMismatch(id) => write!(formatter, "source digest mismatch: {id}"),
        }
    }
}

impl std::error::Error for ManifestError {}

/// Normalize section bytes before digesting.
///
/// Line endings are folded and trailing whitespace on each line is dropped, so
/// a checkout difference does not read as a content difference. Nothing else
/// is altered: the point is to digest what the section *says*, byte for byte,
/// not a summary of it.
#[must_use]
pub fn normalize_source_bytes(raw: &str) -> String {
    raw.replace("\r\n", "\n")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

/// Digest a source's exact normalized bytes together with its metadata.
///
/// Both halves matter. Bytes alone would let the same text be relabelled from
/// `private` to `public` without changing the digest; metadata alone was the
/// original defect.
#[must_use]
pub fn source_digest(
    source_id: &str,
    path: &str,
    heading: &str,
    visibility: Visibility,
    tenant_id: &str,
    project_id: Option<&str>,
    owner_principal_id: Option<&str>,
    raw_bytes: &str,
) -> String {
    let normalized = normalize_source_bytes(raw_bytes);
    let project = optional_fields(project_id);
    let owner = optional_fields(owner_principal_id);
    domain_digest(
        "grokptah.help.source-bytes.v1",
        &[
            source_id,
            path,
            heading,
            &format!("{visibility:?}"),
            tenant_id,
            &project[0],
            &project[1],
            &owner[0],
            &owner[1],
            &normalized,
        ],
    )
}

/// Encode an optional field injectively.
///
/// A sentinel is not enough. `Some("<none>")` and `None` both render as
/// `<none>`, so a source owned by a principal literally named that would
/// digest identically to an unowned one. A separate presence discriminant
/// keeps them apart whatever the value spells.
fn optional_fields(value: Option<&str>) -> [String; 2] {
    match value {
        Some(text) => ["present".to_string(), text.to_string()],
        None => ["absent".to_string(), String::new()],
    }
}

/// Scan raw JSON for a repeated key within any single object.
///
/// `serde_json` keeps the last duplicate silently, so a manifest whose
/// `visibility` appears twice parses as whichever came last. This runs over the
/// raw text before any parsing, so the document is refused rather than
/// interpreted.
///
/// # Errors
/// Returns the offending key, or a parse error if the text is not JSON.
pub fn reject_duplicate_keys(raw: &str) -> Result<(), ManifestError> {
    #[derive(Default)]
    struct Scanner {
        stack: Vec<std::collections::BTreeSet<String>>,
    }

    let bytes = raw.as_bytes();
    let mut scanner = Scanner::default();
    let mut index = 0usize;
    let mut expecting_key = false;

    while index < bytes.len() {
        match bytes[index] {
            b'{' => {
                scanner.stack.push(std::collections::BTreeSet::new());
                expecting_key = true;
                index += 1;
            }
            b'}' => {
                scanner.stack.pop();
                expecting_key = false;
                index += 1;
            }
            b'[' => {
                expecting_key = false;
                index += 1;
            }
            b']' => {
                index += 1;
            }
            b',' => {
                // A comma inside an object introduces the next key.
                expecting_key = !scanner.stack.is_empty();
                index += 1;
            }
            b':' => {
                expecting_key = false;
                index += 1;
            }
            b'"' => {
                let start = index + 1;
                let mut cursor = start;
                while cursor < bytes.len() {
                    if bytes[cursor] == b'\\' {
                        cursor += 2;
                        continue;
                    }
                    if bytes[cursor] == b'"' {
                        break;
                    }
                    cursor += 1;
                }
                if cursor >= bytes.len() {
                    return Err(ManifestError::Malformed("unterminated string".into()));
                }
                let text = &raw[start..cursor];
                if expecting_key
                    && let Some(seen) = scanner.stack.last_mut()
                    && !seen.insert(text.to_string())
                {
                    return Err(ManifestError::DuplicateKey(text.to_string()));
                }
                index = cursor + 1;
            }
            _ => index += 1,
        }
    }
    Ok(())
}

/// The parsed, validated manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceManifest {
    entries: BTreeMap<String, ManifestEntry>,
    manifest_digest: String,
}

impl SourceManifest {
    /// Parse a manifest from raw JSON, refusing duplicate keys first.
    ///
    /// # Errors
    /// Returns a [`ManifestError`] describing the first problem found.
    pub fn from_json(raw: &str) -> Result<Self, ManifestError> {
        reject_duplicate_keys(raw)?;
        let entries: Vec<ManifestEntry> = serde_json::from_str(raw)
            .map_err(|error| ManifestError::Malformed(error.to_string()))?;
        Self::from_entries(entries)
    }

    /// Build from already-parsed entries.
    ///
    /// # Errors
    /// Returns a [`ManifestError`] when a bound is exceeded or an id repeats.
    pub fn from_entries(entries: Vec<ManifestEntry>) -> Result<Self, ManifestError> {
        if entries.len() > MAX_MANIFEST_ENTRIES {
            return Err(ManifestError::Bounds("entry count"));
        }
        let mut map = BTreeMap::new();
        for entry in entries {
            if !id_within_bounds(&entry.source_id)
                || !id_within_bounds(&entry.tenant_id)
                || entry.path.len() > MAX_ID_BYTES * 4
                || entry.heading.len() > MAX_ID_BYTES * 4
            {
                return Err(ManifestError::Bounds("identifier"));
            }
            if map.contains_key(&entry.source_id) {
                return Err(ManifestError::DuplicateSourceId(entry.source_id));
            }
            map.insert(entry.source_id.clone(), entry);
        }
        // The manifest digest covers every entry digest, in id order, so a
        // reordering cannot change it but a substitution must.
        let fields: Vec<String> = map
            .values()
            .flat_map(|entry| [entry.source_id.clone(), entry.digest.clone()])
            .collect();
        let refs: Vec<&str> = fields.iter().map(String::as_str).collect();
        Ok(Self {
            entries: map,
            manifest_digest: domain_digest("grokptah.help.manifest.v1", &refs),
        })
    }

    #[must_use]
    pub fn manifest_digest(&self) -> &str {
        &self.manifest_digest
    }

    #[must_use]
    pub fn get(&self, source_id: &str) -> Option<&ManifestEntry> {
        self.entries.get(source_id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Rebuild the descriptors for a set of source ids from the manifest.
    ///
    /// Descriptors are produced here rather than accepted from a caller, so a
    /// request cannot relabel a private source as public.
    #[must_use]
    pub fn describe(&self, source_ids: &[String]) -> Vec<SourceDescriptor> {
        source_ids
            .iter()
            .filter_map(|id| self.entries.get(id))
            .map(|entry| SourceDescriptor {
                source_id: entry.source_id.clone(),
                visibility: entry.visibility,
                tenant_id: entry.tenant_id.clone(),
                project_id: entry.project_id.clone(),
                owner_principal_id: entry.owner_principal_id.clone(),
                digest: entry.digest.clone(),
            })
            .collect()
    }

    /// Confirm a presented descriptor is exactly the one the manifest holds.
    ///
    /// This is the check `SourceDescriptor.digest` never had: previously the
    /// field was parsed and then ignored, so a stale or fabricated record was
    /// indistinguishable from a current one.
    ///
    /// # Errors
    /// Returns [`ManifestError::DigestMismatch`] when the descriptor is not the
    /// manifest's, including when the id is unknown.
    pub fn enforce_descriptor(&self, descriptor: &SourceDescriptor) -> Result<(), ManifestError> {
        let Some(entry) = self.entries.get(&descriptor.source_id) else {
            return Err(ManifestError::DigestMismatch(descriptor.source_id.clone()));
        };
        let matches = entry.digest == descriptor.digest
            && entry.visibility == descriptor.visibility
            && entry.tenant_id == descriptor.tenant_id
            && entry.project_id == descriptor.project_id
            && entry.owner_principal_id == descriptor.owner_principal_id;
        if matches {
            Ok(())
        } else {
            Err(ManifestError::DigestMismatch(descriptor.source_id.clone()))
        }
    }
}
