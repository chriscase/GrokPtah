//! The artifact manifest.
//!
//! Everything a certification lab needs to run this benchmark against a
//! candidate -- the hazard taxonomy, the invariant set, the profiles, the
//! thresholds, the scenarios, the workflow matrix -- is emitted as canonical
//! JSON and digested here. The manifest digest covers all of it, so a lab can
//! state exactly which benchmark a result came from and a reader can check
//! that two results are comparable before comparing them.
//!
//! The artifacts are generated from the Rust definitions rather than being
//! the source of truth, and the checked-in copies are verified against
//! freshly generated ones in CI. That means the JSON cannot silently drift
//! from the code that produced it, which is the failure mode that makes
//! published fixture sets untrustworthy.

use serde::{Deserialize, Serialize};

use crate::digest::{canonical_json_pretty, fold_digests, sha256_hex};
use crate::hazard::HazardFamily;
use crate::modelclass::QualificationThresholds;
use crate::profile::{ExecutionProfile, ProfileId};
use crate::schema::SCHEMA_VERSION;

/// Where an artifact is written, relative to the crate root.
pub const ARTIFACT_DIR: &str = "artifacts";

/// What an artifact describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// The hazard taxonomy the catalog must cover.
    Taxonomy,
    /// The authority invariants a candidate is judged against.
    Invariants,
    /// The execution profiles under comparison.
    Profiles,
    /// The bounded efficiency envelope each model class declares.
    EfficiencyEnvelopes,
    /// The qualification thresholds, per model class and profile.
    Thresholds,
    /// The scenario fixtures.
    Scenarios,
    /// The representative workflow matrix and comparison status.
    WorkflowMatrix,
}

impl ArtifactKind {
    pub const ALL: &'static [ArtifactKind] = &[
        Self::Taxonomy,
        Self::Invariants,
        Self::Profiles,
        Self::EfficiencyEnvelopes,
        Self::Thresholds,
        Self::Scenarios,
        Self::WorkflowMatrix,
    ];

    /// Path relative to [`ARTIFACT_DIR`].
    #[must_use]
    pub fn path(self) -> &'static str {
        match self {
            Self::Taxonomy => "schema/hazard-families.json",
            Self::Invariants => "schema/invariants.json",
            Self::Profiles => "schema/profiles.json",
            Self::EfficiencyEnvelopes => "schema/efficiency-envelopes.json",
            Self::Thresholds => "schema/thresholds.json",
            Self::Scenarios => "schema/scenarios.json",
            Self::WorkflowMatrix => "schema/workflow-matrix.json",
        }
    }

    /// Generate this artifact's canonical content.
    #[must_use]
    pub fn render(self) -> String {
        match self {
            Self::Taxonomy => canonical_json_pretty(&taxonomy_doc()),
            Self::Invariants => canonical_json_pretty(&invariants_doc()),
            Self::Profiles => canonical_json_pretty(&profiles_doc()),
            Self::EfficiencyEnvelopes => canonical_json_pretty(&envelopes_doc()),
            Self::Thresholds => canonical_json_pretty(&QualificationThresholds::matrix()),
            Self::Scenarios => canonical_json_pretty(&crate::catalog::all()),
            Self::WorkflowMatrix => canonical_json_pretty(&crate::matrix::workflow_matrix()),
        }
    }
}

/// One row of the taxonomy artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyRow {
    pub family: HazardFamily,
    pub slug: String,
    pub role: &'static str,
}

#[must_use]
fn taxonomy_doc() -> Vec<TaxonomyRow> {
    HazardFamily::ALL
        .iter()
        .map(|family| TaxonomyRow {
            family: *family,
            slug: family.slug().to_owned(),
            role: if family.is_baseline() {
                "baseline"
            } else if family.is_robustness_family() {
                "robustness"
            } else if family.is_recovery_family() {
                "recovery"
            } else {
                "safety"
            },
        })
        .collect()
}

/// One row of the invariants artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvariantRow {
    pub invariant: crate::authority::Invariant,
    pub slug: String,
    pub refusal: crate::schema::RefusalCode,
    pub authority_bearing: bool,
}

#[must_use]
fn invariants_doc() -> Vec<InvariantRow> {
    crate::authority::Invariant::ALL
        .iter()
        .map(|invariant| InvariantRow {
            invariant: *invariant,
            slug: invariant.slug().to_owned(),
            refusal: invariant.refusal(),
            authority_bearing: invariant.is_authority_bearing(),
        })
        .collect()
}

#[must_use]
fn envelopes_doc() -> Vec<crate::efficiency::EfficiencyEnvelope> {
    crate::modelclass::ModelClass::ALL
        .iter()
        .map(|class| crate::efficiency::EfficiencyEnvelope::for_class(*class))
        .collect()
}

#[must_use]
fn profiles_doc() -> Vec<ExecutionProfile> {
    ProfileId::ALL
        .iter()
        .map(|id| ExecutionProfile::for_id(*id))
        .collect()
}

/// One manifest entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEntry {
    pub kind: ArtifactKind,
    pub path: String,
    pub sha256: String,
    pub byte_len: u64,
}

/// The manifest for one build of the benchmark.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub schema_version: String,
    pub generator: String,
    pub entries: Vec<ManifestEntry>,
    /// Digest over every entry, in kind order. Cite this to say which
    /// benchmark a result came from.
    pub manifest_digest: String,
}

/// Build the manifest from freshly generated artifacts.
#[must_use]
pub fn manifest() -> Manifest {
    let entries: Vec<ManifestEntry> = ArtifactKind::ALL
        .iter()
        .map(|kind| {
            let content = kind.render();
            ManifestEntry {
                kind: *kind,
                path: kind.path().to_owned(),
                sha256: sha256_hex(content.as_bytes()),
                byte_len: content.len() as u64,
            }
        })
        .collect();

    let manifest_digest = fold_digests(
        "grokptah.cu-bench/manifest",
        &entries
            .iter()
            .map(|entry| entry.sha256.clone())
            .collect::<Vec<_>>(),
    );

    Manifest {
        schema_version: SCHEMA_VERSION.to_owned(),
        generator: concat!("grokptah-cu-bench ", env!("CARGO_PKG_VERSION")).to_owned(),
        entries,
        manifest_digest,
    }
}

/// The manifest's own canonical JSON, for writing to `artifacts/manifest.json`.
#[must_use]
pub fn manifest_json() -> String {
    canonical_json_pretty(&manifest())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_artifact_kind_renders_non_empty_canonical_json() {
        for kind in ArtifactKind::ALL {
            let rendered = kind.render();
            assert!(!rendered.trim().is_empty(), "{kind:?} rendered nothing");
            serde_json::from_str::<serde_json::Value>(&rendered)
                .unwrap_or_else(|error| panic!("{kind:?} is not valid JSON: {error}"));
        }
    }

    #[test]
    fn the_manifest_is_deterministic() {
        assert_eq!(manifest(), manifest());
        assert_eq!(manifest_json(), manifest_json());
    }

    #[test]
    fn the_manifest_covers_every_artifact_kind() {
        let manifest = manifest();
        assert_eq!(manifest.entries.len(), ArtifactKind::ALL.len());
        for kind in ArtifactKind::ALL {
            assert!(manifest.entries.iter().any(|entry| entry.kind == *kind));
        }
    }

    #[test]
    fn changing_any_artifact_changes_the_manifest_digest() {
        let baseline = manifest();
        let mut mutated = baseline.clone();
        mutated.entries[0].sha256 = sha256_hex(b"different");
        let recomputed = fold_digests(
            "grokptah.cu-bench/manifest",
            &mutated
                .entries
                .iter()
                .map(|entry| entry.sha256.clone())
                .collect::<Vec<_>>(),
        );
        assert_ne!(recomputed, baseline.manifest_digest);
    }

    #[test]
    fn artifact_paths_are_unique_and_relative() {
        let mut paths: Vec<&str> = ArtifactKind::ALL.iter().map(|kind| kind.path()).collect();
        paths.sort_unstable();
        let count = paths.len();
        paths.dedup();
        assert_eq!(paths.len(), count);
        assert!(
            ArtifactKind::ALL
                .iter()
                .all(|kind| !kind.path().starts_with('/'))
        );
    }

    #[test]
    fn every_digest_is_well_formed() {
        let manifest = manifest();
        assert!(crate::digest::is_digest(&manifest.manifest_digest));
        for entry in &manifest.entries {
            assert!(crate::digest::is_digest(&entry.sha256), "{}", entry.path);
        }
    }
}
