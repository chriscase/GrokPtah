//! Immutable release identity bound to an exact qualified head.
//!
//! A [`ReleaseRecord`] can only be produced by binding artifacts to a
//! [`QualifiedCandidate`], so a release record always names the exact commit
//! whose evidence passed verification. The record has no `Deserialize`
//! implementation, no public constructor, and no mutators: once bound, its
//! head, evidence digest, and artifact metadata cannot be edited or forged.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::verify::{Finding, QualifiedCandidate, Rejection, SHA256_HEX_LEN, is_lowercase_hex};

/// Metadata for one immutable release artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseArtifact {
    /// Plain file name of the artifact, without any directory component.
    pub name: String,
    /// Exact byte length of the artifact.
    pub bytes: u64,
    /// Lowercase hex SHA-256 of the artifact contents.
    pub sha256: String,
}

/// A release bound to one qualified candidate and its artifact metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseRecord {
    candidate_head: String,
    parent_head: String,
    evidence_digest_sha256: String,
    qualified_at_unix_seconds: u64,
    artifacts: Vec<ReleaseArtifact>,
    release_digest_sha256: String,
}

/// The digest domain for a release record: everything except its own digest.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseBody<'a> {
    candidate_head: &'a str,
    parent_head: &'a str,
    evidence_digest_sha256: &'a str,
    qualified_at_unix_seconds: u64,
    artifacts: &'a [ReleaseArtifact],
}

impl ReleaseRecord {
    pub(crate) fn bind(
        candidate: &QualifiedCandidate,
        mut artifacts: Vec<ReleaseArtifact>,
    ) -> Result<Self, Rejection> {
        let mut findings = Vec::new();
        let mut fail = |detail: String| {
            findings.push(Finding {
                subject: "release_artifacts".into(),
                detail,
            });
        };

        if artifacts.is_empty() {
            fail("a release record must bind at least one artifact".into());
        }

        let mut names = BTreeSet::new();
        for artifact in &artifacts {
            if !is_plain_file_name(&artifact.name) {
                fail(format!(
                    "artifact name {:?} is not a plain file name",
                    artifact.name
                ));
            } else if !names.insert(artifact.name.as_str()) {
                fail(format!(
                    "artifact name {:?} is bound more than once",
                    artifact.name
                ));
            }
            if artifact.bytes == 0 {
                fail(format!("artifact {:?} declares zero bytes", artifact.name));
            }
            if !is_lowercase_hex(&artifact.sha256, SHA256_HEX_LEN) {
                fail(format!(
                    "artifact {:?} digest {:?} is not a lowercase hex SHA-256",
                    artifact.name, artifact.sha256
                ));
            }
        }

        if !findings.is_empty() {
            return Err(Rejection::new(findings));
        }

        artifacts.sort_by(|left, right| left.name.cmp(&right.name));

        let body = ReleaseBody {
            candidate_head: candidate.candidate_head(),
            parent_head: candidate.parent_head(),
            evidence_digest_sha256: candidate.evidence_digest_sha256(),
            qualified_at_unix_seconds: candidate.qualified_at_unix_seconds(),
            artifacts: &artifacts,
        };
        let encoded = serde_json::to_vec(&body).map_err(|error| {
            Rejection::single(
                "release_record",
                format!("release record could not be canonicalized: {error}"),
            )
        })?;

        Ok(Self {
            candidate_head: candidate.candidate_head().to_owned(),
            parent_head: candidate.parent_head().to_owned(),
            evidence_digest_sha256: candidate.evidence_digest_sha256().to_owned(),
            qualified_at_unix_seconds: candidate.qualified_at_unix_seconds(),
            artifacts,
            release_digest_sha256: format!("{:x}", Sha256::digest(&encoded)),
        })
    }

    /// The exact commit this release is bound to.
    pub fn candidate_head(&self) -> &str {
        &self.candidate_head
    }

    /// The exact first parent of the bound commit.
    pub fn parent_head(&self) -> &str {
        &self.parent_head
    }

    /// The evidence digest the bound qualification rests on.
    pub fn evidence_digest_sha256(&self) -> &str {
        &self.evidence_digest_sha256
    }

    /// Unix seconds at which the bound candidate was qualified.
    pub fn qualified_at_unix_seconds(&self) -> u64 {
        self.qualified_at_unix_seconds
    }

    /// Bound artifact metadata, canonically ordered by name.
    pub fn artifacts(&self) -> &[ReleaseArtifact] {
        &self.artifacts
    }

    /// Lowercase hex SHA-256 over the canonical release body.
    pub fn release_digest_sha256(&self) -> &str {
        &self.release_digest_sha256
    }
}

fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.chars().any(char::is_control)
        && name.trim() == name
}
