//! Secret-free evidence contract for long-horizon durable memory.
//!
//! The memory core already has a deterministic logical-years workload. This
//! record makes its eventual retained campaign evidence machine-verifiable and
//! keeps that claim separate from elapsed wall-clock soak evidence.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MEMORY_LONG_HORIZON_EVIDENCE_SCHEMA: &str = "grokptah.memory-long-horizon-evidence.v1";
pub const REQUIRED_LOGICAL_YEARS: u32 = 10;
pub const REQUIRED_MEMORY_SCOPES: [&str; 3] = ["project", "agent_private", "team"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryLongHorizonEvidence {
    pub schema: String,
    pub certification_id: String,
    pub candidate_sha: String,
    pub fixture_id: String,
    pub fixture_digest: String,
    pub core_source_digest: String,
    pub logical_years: u32,
    pub scopes: Vec<String>,
    pub critical_recall_pct: u32,
    pub stale_as_current_pct: u32,
    pub conflict_recall_pct: u32,
    pub conflict_false_positive_pct: u32,
    pub duplicate_rate_pct: u32,
    pub hot_store_within_byte_bound: bool,
    pub repeated_read_reopen_deterministic: bool,
    pub secret_free: bool,
    pub claim_eligible: bool,
    pub evidence_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryLongHorizonEvidenceError {
    InvalidField(&'static str),
    UnsupportedSchema,
    MissingScope(&'static str),
    DuplicateScope,
    NotEligible(&'static str),
}

impl std::fmt::Display for MemoryLongHorizonEvidenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidField(name) => write!(f, "invalid memory evidence field: {name}"),
            Self::UnsupportedSchema => write!(f, "unsupported memory evidence schema"),
            Self::MissingScope(scope) => write!(f, "memory evidence is missing scope: {scope}"),
            Self::DuplicateScope => write!(f, "memory evidence contains a duplicate scope"),
            Self::NotEligible(name) => write!(f, "memory evidence is not eligible: {name}"),
        }
    }
}

impl std::error::Error for MemoryLongHorizonEvidenceError {}

impl MemoryLongHorizonEvidence {
    pub fn validate(&self) -> Result<(), MemoryLongHorizonEvidenceError> {
        if self.schema != MEMORY_LONG_HORIZON_EVIDENCE_SCHEMA {
            return Err(MemoryLongHorizonEvidenceError::UnsupportedSchema);
        }
        if !valid_opaque_id(&self.certification_id)
            || !valid_opaque_id(&self.fixture_id)
            || !valid_sha(&self.candidate_sha)
            || !valid_fingerprint(&self.fixture_digest)
            || !valid_fingerprint(&self.core_source_digest)
            || !valid_fingerprint(&self.evidence_digest)
        {
            return Err(MemoryLongHorizonEvidenceError::InvalidField("identity"));
        }
        if self.logical_years < REQUIRED_LOGICAL_YEARS {
            return Err(MemoryLongHorizonEvidenceError::NotEligible(
                "logical-years span",
            ));
        }
        let mut scopes = std::collections::BTreeSet::new();
        for scope in &self.scopes {
            if !valid_opaque_id(scope) || !scopes.insert(scope.as_str()) {
                return Err(MemoryLongHorizonEvidenceError::DuplicateScope);
            }
        }
        for required in REQUIRED_MEMORY_SCOPES {
            if !scopes.contains(required) {
                return Err(MemoryLongHorizonEvidenceError::MissingScope(required));
            }
        }
        if self.critical_recall_pct != 100
            || self.stale_as_current_pct != 0
            || self.conflict_recall_pct != 100
            || self.conflict_false_positive_pct != 0
            || self.duplicate_rate_pct != 0
        {
            return Err(MemoryLongHorizonEvidenceError::NotEligible(
                "quality oracle",
            ));
        }
        if !self.hot_store_within_byte_bound || !self.repeated_read_reopen_deterministic {
            return Err(MemoryLongHorizonEvidenceError::NotEligible(
                "durability bounds",
            ));
        }
        if !self.secret_free {
            return Err(MemoryLongHorizonEvidenceError::InvalidField("secret_free"));
        }
        if self.evidence_digest != expected_evidence_digest(self) {
            return Err(MemoryLongHorizonEvidenceError::InvalidField(
                "evidence_digest",
            ));
        }
        Ok(())
    }

    pub fn certification_ready(&self) -> bool {
        self.claim_eligible && self.validate().is_ok()
    }
}

pub fn expected_evidence_digest(evidence: &MemoryLongHorizonEvidence) -> String {
    let mut unsigned = evidence.clone();
    unsigned.evidence_digest.clear();
    let encoded =
        serde_json::to_vec(&unsigned).expect("memory evidence serialization is infallible");
    format!("{:x}", Sha256::digest(encoded))
}

fn valid_opaque_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

fn valid_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_fingerprint(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(claim_eligible: bool) -> MemoryLongHorizonEvidence {
        let mut evidence = MemoryLongHorizonEvidence {
            schema: MEMORY_LONG_HORIZON_EVIDENCE_SCHEMA.into(),
            certification_id: "memory-cert-1".into(),
            candidate_sha: "a".repeat(40),
            fixture_id: "memory-long-horizon-v1".into(),
            fixture_digest: "b".repeat(64),
            core_source_digest: "c".repeat(64),
            logical_years: REQUIRED_LOGICAL_YEARS,
            scopes: REQUIRED_MEMORY_SCOPES
                .into_iter()
                .map(str::to_owned)
                .collect(),
            critical_recall_pct: 100,
            stale_as_current_pct: 0,
            conflict_recall_pct: 100,
            conflict_false_positive_pct: 0,
            duplicate_rate_pct: 0,
            hot_store_within_byte_bound: true,
            repeated_read_reopen_deterministic: true,
            secret_free: true,
            claim_eligible,
            evidence_digest: String::new(),
        };
        evidence.evidence_digest = expected_evidence_digest(&evidence);
        evidence
    }

    #[test]
    fn complete_memory_evidence_is_ready_and_secret_free() {
        let evidence = evidence(true);
        evidence.validate().unwrap();
        assert!(evidence.certification_ready());
        let encoded = serde_json::to_string(&evidence).unwrap();
        assert!(!encoded.contains("bearer"));
        assert!(!encoded.contains("api_key"));
    }

    #[test]
    fn missing_scope_or_short_span_fails_closed() {
        let mut missing = evidence(true);
        missing.scopes.pop();
        missing.evidence_digest = expected_evidence_digest(&missing);
        assert_eq!(
            missing.validate(),
            Err(MemoryLongHorizonEvidenceError::MissingScope("team"))
        );
        let mut short = evidence(true);
        short.logical_years = REQUIRED_LOGICAL_YEARS - 1;
        short.evidence_digest = expected_evidence_digest(&short);
        assert_eq!(
            short.validate(),
            Err(MemoryLongHorizonEvidenceError::NotEligible(
                "logical-years span"
            ))
        );
    }

    #[test]
    fn quality_oracle_and_digest_tampering_fail_closed() {
        let mut oracle = evidence(true);
        oracle.stale_as_current_pct = 1;
        oracle.evidence_digest = expected_evidence_digest(&oracle);
        assert_eq!(
            oracle.validate(),
            Err(MemoryLongHorizonEvidenceError::NotEligible(
                "quality oracle"
            ))
        );
        let mut tampered = evidence(true);
        tampered.core_source_digest = "d".repeat(64);
        assert_eq!(
            tampered.validate(),
            Err(MemoryLongHorizonEvidenceError::InvalidField(
                "evidence_digest"
            ))
        );
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let mut value = serde_json::to_value(evidence(false)).unwrap();
        value["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<MemoryLongHorizonEvidence>(value).is_err());
    }
}
