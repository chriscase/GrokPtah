//! Secret-free evidence contract for independent long-running workers.
//!
//! Runtime lease fencing and restart recovery are necessary but not
//! sufficient for the Stage 6 release exit. This record binds a measured
//! multi-worker campaign to an exact assembled revision and requires proof of
//! restart recovery, no duplicate execution, least-privilege credential
//! issuance/rotation, retained audit evidence, and the full operational soak.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const WORKER_CERTIFICATION_EVIDENCE_SCHEMA: &str = "grokptah.worker-certification-evidence.v2";
pub const REQUIRED_WORKER_CHECKS: [&str; 7] = [
    "multi_worker_leases",
    "crash_restart_recovery",
    "no_duplicate_execution",
    "credential_issuance",
    "credential_rotation",
    "retained_audit",
    "operational_soak",
];
pub const REQUIRED_SOAK_SECONDS: u64 = 72 * 60 * 60;
pub const REQUIRED_WORKERS: usize = 2;
pub const REQUIRED_RESTARTS: u32 = 3;
pub const MAX_WORKERS: usize = 128;
pub const MAX_CHECKS: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkerCheckEvidence {
    pub check_id: String,
    pub passed: bool,
    pub duration_ms: u64,
    pub evidence_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkerCredentialLifecycleEvidence {
    pub bound_agent_id: String,
    pub credential_fingerprint: String,
    pub issued: bool,
    pub least_privilege: bool,
    pub rotation_observed: bool,
    pub old_credential_rejected: bool,
    pub new_credential_accepted: bool,
    pub evidence_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LongRunningWorkerEvidence {
    pub schema: String,
    pub certification_id: String,
    pub candidate_sha: String,
    pub campaign_id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub workers: Vec<String>,
    pub checks: Vec<WorkerCheckEvidence>,
    pub credential_lifecycle: Vec<WorkerCredentialLifecycleEvidence>,
    pub restart_count: u32,
    pub duplicate_execution_count: u32,
    pub retained_audit_entries: u64,
    pub soak_seconds: u64,
    pub secret_free: bool,
    pub claim_eligible: bool,
    pub evidence_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerCertificationEvidenceError {
    InvalidField(&'static str),
    UnsupportedSchema,
    MissingCheck(&'static str),
    UnknownCheck,
    DuplicateCheck,
    DuplicateWorker,
    DuplicateCredentialBinding,
    DuplicateCredentialFingerprint,
    NotEligible(&'static str),
}

impl std::fmt::Display for WorkerCertificationEvidenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidField(name) => write!(f, "invalid worker evidence field: {name}"),
            Self::UnsupportedSchema => write!(f, "unsupported worker evidence schema"),
            Self::MissingCheck(check) => write!(f, "worker evidence is missing check: {check}"),
            Self::UnknownCheck => write!(f, "worker evidence contains an unknown check"),
            Self::DuplicateCheck => write!(f, "worker evidence contains a duplicate check"),
            Self::DuplicateWorker => write!(f, "worker evidence contains a duplicate worker"),
            Self::DuplicateCredentialBinding => {
                write!(f, "worker evidence contains a duplicate credential binding")
            }
            Self::DuplicateCredentialFingerprint => {
                write!(f, "worker evidence reuses a credential fingerprint")
            }
            Self::NotEligible(name) => write!(f, "worker evidence is not eligible: {name}"),
        }
    }
}

impl std::error::Error for WorkerCertificationEvidenceError {}

impl LongRunningWorkerEvidence {
    pub fn validate(&self) -> Result<(), WorkerCertificationEvidenceError> {
        if self.schema != WORKER_CERTIFICATION_EVIDENCE_SCHEMA {
            return Err(WorkerCertificationEvidenceError::UnsupportedSchema);
        }
        if !valid_opaque_id(&self.certification_id)
            || !valid_opaque_id(&self.campaign_id)
            || !valid_sha(&self.candidate_sha)
            || !valid_fingerprint(&self.evidence_digest)
        {
            return Err(WorkerCertificationEvidenceError::InvalidField("identity"));
        }
        if self.started_at.timestamp() < 0
            || self.finished_at < self.started_at
            || !self.secret_free
        {
            return Err(WorkerCertificationEvidenceError::InvalidField(
                "time_or_secrets",
            ));
        }
        let elapsed_seconds = self
            .finished_at
            .signed_duration_since(self.started_at)
            .num_seconds();
        if elapsed_seconds < REQUIRED_SOAK_SECONDS as i64
            || self.soak_seconds > elapsed_seconds as u64
        {
            return Err(WorkerCertificationEvidenceError::NotEligible(
                "measured soak duration",
            ));
        }
        if self.workers.len() < REQUIRED_WORKERS || self.workers.len() > MAX_WORKERS {
            return Err(WorkerCertificationEvidenceError::NotEligible(
                "worker cardinality",
            ));
        }
        let mut workers = std::collections::BTreeSet::new();
        for worker in &self.workers {
            if !valid_opaque_id(worker) || !workers.insert(worker.as_str()) {
                return Err(WorkerCertificationEvidenceError::DuplicateWorker);
            }
        }
        if self.checks.is_empty() || self.checks.len() > MAX_CHECKS {
            return Err(WorkerCertificationEvidenceError::NotEligible(
                "check cardinality",
            ));
        }
        let mut checks = std::collections::BTreeSet::new();
        for check in &self.checks {
            if !checks.insert(check.check_id.as_str()) {
                return Err(WorkerCertificationEvidenceError::DuplicateCheck);
            }
            if !REQUIRED_WORKER_CHECKS.contains(&check.check_id.as_str()) {
                return Err(WorkerCertificationEvidenceError::UnknownCheck);
            }
            if !valid_opaque_id(&check.check_id)
                || check.duration_ms == 0
                || !valid_fingerprint(&check.evidence_digest)
            {
                return Err(WorkerCertificationEvidenceError::InvalidField("check"));
            }
            if !check.passed {
                return Err(WorkerCertificationEvidenceError::NotEligible(
                    "check failed",
                ));
            }
            if check.check_id == "operational_soak"
                && (check.duration_ms < REQUIRED_SOAK_SECONDS.saturating_mul(1000)
                    || check.duration_ms > (elapsed_seconds as u64).saturating_mul(1000))
            {
                return Err(WorkerCertificationEvidenceError::NotEligible(
                    "operational soak check duration",
                ));
            }
        }
        for required in REQUIRED_WORKER_CHECKS {
            if !checks.contains(required) {
                return Err(WorkerCertificationEvidenceError::MissingCheck(required));
            }
        }
        let mut credential_bindings = std::collections::BTreeSet::new();
        let mut credential_fingerprints = std::collections::BTreeSet::new();
        if self.credential_lifecycle.len() != self.workers.len() {
            return Err(WorkerCertificationEvidenceError::NotEligible(
                "credential coverage",
            ));
        }
        for credential in &self.credential_lifecycle {
            if !workers.contains(credential.bound_agent_id.as_str())
                || !credential_bindings.insert(credential.bound_agent_id.as_str())
            {
                return Err(WorkerCertificationEvidenceError::DuplicateCredentialBinding);
            }
            if !credential_fingerprints.insert(credential.credential_fingerprint.as_str()) {
                return Err(WorkerCertificationEvidenceError::DuplicateCredentialFingerprint);
            }
            if !valid_opaque_id(&credential.bound_agent_id)
                || !valid_fingerprint(&credential.credential_fingerprint)
                || !valid_fingerprint(&credential.evidence_digest)
                || !(credential.issued
                    && credential.least_privilege
                    && credential.rotation_observed
                    && credential.old_credential_rejected
                    && credential.new_credential_accepted)
            {
                return Err(WorkerCertificationEvidenceError::NotEligible(
                    "credential lifecycle",
                ));
            }
        }
        if self.restart_count < REQUIRED_RESTARTS {
            return Err(WorkerCertificationEvidenceError::NotEligible(
                "restart recovery",
            ));
        }
        if self.duplicate_execution_count != 0 {
            return Err(WorkerCertificationEvidenceError::NotEligible(
                "duplicate execution",
            ));
        }
        if self.retained_audit_entries == 0 {
            return Err(WorkerCertificationEvidenceError::NotEligible(
                "retained audit",
            ));
        }
        if self.soak_seconds < REQUIRED_SOAK_SECONDS {
            return Err(WorkerCertificationEvidenceError::NotEligible(
                "72-hour soak",
            ));
        }
        if self.evidence_digest != expected_worker_evidence_digest(self) {
            return Err(WorkerCertificationEvidenceError::InvalidField(
                "evidence_digest",
            ));
        }
        Ok(())
    }

    pub fn certification_ready(&self) -> bool {
        self.claim_eligible && self.validate().is_ok()
    }
}

pub fn expected_worker_evidence_digest(evidence: &LongRunningWorkerEvidence) -> String {
    let mut unsigned = evidence.clone();
    unsigned.evidence_digest.clear();
    let encoded =
        serde_json::to_vec(&unsigned).expect("worker evidence serialization is infallible");
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

    fn evidence(claim_eligible: bool) -> LongRunningWorkerEvidence {
        let mut evidence = LongRunningWorkerEvidence {
            schema: WORKER_CERTIFICATION_EVIDENCE_SCHEMA.into(),
            certification_id: "worker-cert-1".into(),
            candidate_sha: "a".repeat(40),
            campaign_id: "overnight-campaign-1".into(),
            started_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            finished_at: DateTime::from_timestamp(1_700_000_000 + REQUIRED_SOAK_SECONDS as i64, 0)
                .unwrap(),
            workers: vec!["worker-a".into(), "worker-b".into()],
            checks: REQUIRED_WORKER_CHECKS
                .into_iter()
                .map(|check_id| WorkerCheckEvidence {
                    check_id: check_id.into(),
                    passed: true,
                    duration_ms: if check_id == "operational_soak" {
                        REQUIRED_SOAK_SECONDS * 1000
                    } else {
                        100
                    },
                    evidence_digest: "b".repeat(64),
                })
                .collect(),
            credential_lifecycle: ["worker-a", "worker-b"]
                .into_iter()
                .enumerate()
                .map(
                    |(index, bound_agent_id)| WorkerCredentialLifecycleEvidence {
                        bound_agent_id: bound_agent_id.into(),
                        credential_fingerprint: if index == 0 {
                            "c".repeat(64)
                        } else {
                            "e".repeat(64)
                        },
                        issued: true,
                        least_privilege: true,
                        rotation_observed: true,
                        old_credential_rejected: true,
                        new_credential_accepted: true,
                        evidence_digest: "d".repeat(64),
                    },
                )
                .collect(),
            restart_count: REQUIRED_RESTARTS,
            duplicate_execution_count: 0,
            retained_audit_entries: 42,
            soak_seconds: REQUIRED_SOAK_SECONDS,
            secret_free: true,
            claim_eligible,
            evidence_digest: String::new(),
        };
        evidence.evidence_digest = expected_worker_evidence_digest(&evidence);
        evidence
    }

    #[test]
    fn complete_multi_worker_campaign_is_ready_and_secret_free() {
        let evidence = evidence(true);
        evidence.validate().unwrap();
        assert!(evidence.certification_ready());
        let encoded = serde_json::to_string(&evidence).unwrap();
        assert!(!encoded.contains("bearer"));
        assert!(!encoded.contains("token"));
    }

    #[test]
    fn short_soak_or_duplicate_execution_fails_closed() {
        let mut short_soak = evidence(true);
        short_soak.soak_seconds = REQUIRED_SOAK_SECONDS - 1;
        short_soak.evidence_digest = expected_worker_evidence_digest(&short_soak);
        assert!(matches!(
            short_soak.validate(),
            Err(WorkerCertificationEvidenceError::NotEligible(
                "72-hour soak"
            ))
        ));
        let mut duplicate = evidence(true);
        duplicate.duplicate_execution_count = 1;
        duplicate.evidence_digest = expected_worker_evidence_digest(&duplicate);
        assert!(matches!(
            duplicate.validate(),
            Err(WorkerCertificationEvidenceError::NotEligible(
                "duplicate execution"
            ))
        ));
    }

    #[test]
    fn missing_check_or_rotation_denies_claim() {
        let mut missing_check = evidence(true);
        missing_check.checks.pop();
        missing_check.evidence_digest = expected_worker_evidence_digest(&missing_check);
        assert!(matches!(
            missing_check.validate(),
            Err(WorkerCertificationEvidenceError::MissingCheck(
                "operational_soak"
            ))
        ));
        let mut bad_rotation = evidence(true);
        bad_rotation.credential_lifecycle[0].old_credential_rejected = false;
        bad_rotation.evidence_digest = expected_worker_evidence_digest(&bad_rotation);
        assert!(matches!(
            bad_rotation.validate(),
            Err(WorkerCertificationEvidenceError::NotEligible(
                "credential lifecycle"
            ))
        ));
    }

    #[test]
    fn non_claiming_or_unknown_evidence_stays_ineligible() {
        let non_claiming = evidence(false);
        assert!(non_claiming.validate().is_ok());
        assert!(!non_claiming.certification_ready());

        let mut legacy = evidence(true);
        legacy.schema = "grokptah.worker-certification-evidence.v1".into();
        legacy.evidence_digest = expected_worker_evidence_digest(&legacy);
        assert!(matches!(
            legacy.validate(),
            Err(WorkerCertificationEvidenceError::UnsupportedSchema)
        ));

        let mut value = serde_json::to_value(evidence(true)).unwrap();
        value["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<LongRunningWorkerEvidence>(value).is_err());
    }

    #[test]
    fn cardinality_elapsed_restarts_and_credentials_fail_closed() {
        let mut one_worker = evidence(true);
        one_worker.workers.pop();
        one_worker.credential_lifecycle.pop();
        one_worker.evidence_digest = expected_worker_evidence_digest(&one_worker);
        assert!(matches!(
            one_worker.validate(),
            Err(WorkerCertificationEvidenceError::NotEligible(
                "worker cardinality"
            ))
        ));

        let mut short_elapsed = evidence(true);
        short_elapsed.finished_at =
            short_elapsed.started_at + chrono::Duration::seconds(REQUIRED_SOAK_SECONDS as i64 - 1);
        short_elapsed.soak_seconds = REQUIRED_SOAK_SECONDS - 1;
        short_elapsed.evidence_digest = expected_worker_evidence_digest(&short_elapsed);
        assert!(matches!(
            short_elapsed.validate(),
            Err(WorkerCertificationEvidenceError::NotEligible(
                "measured soak duration"
            ))
        ));

        let mut overclaimed_check = evidence(true);
        let operational_soak = overclaimed_check
            .checks
            .iter_mut()
            .find(|check| check.check_id == "operational_soak")
            .unwrap();
        operational_soak.duration_ms += 1;
        overclaimed_check.evidence_digest = expected_worker_evidence_digest(&overclaimed_check);
        assert!(matches!(
            overclaimed_check.validate(),
            Err(WorkerCertificationEvidenceError::NotEligible(
                "operational soak check duration"
            ))
        ));

        let mut two_restarts = evidence(true);
        two_restarts.restart_count = REQUIRED_RESTARTS - 1;
        two_restarts.evidence_digest = expected_worker_evidence_digest(&two_restarts);
        assert!(matches!(
            two_restarts.validate(),
            Err(WorkerCertificationEvidenceError::NotEligible(
                "restart recovery"
            ))
        ));

        let mut shared_credential = evidence(true);
        shared_credential.credential_lifecycle[1].credential_fingerprint = shared_credential
            .credential_lifecycle[0]
            .credential_fingerprint
            .clone();
        shared_credential.evidence_digest = expected_worker_evidence_digest(&shared_credential);
        assert!(matches!(
            shared_credential.validate(),
            Err(WorkerCertificationEvidenceError::DuplicateCredentialFingerprint)
        ));
    }

    #[test]
    fn unknown_check_or_transport_tamper_fails_closed() {
        let mut unknown = evidence(true);
        unknown.checks.push(WorkerCheckEvidence {
            check_id: "invented_check".into(),
            passed: true,
            duration_ms: 1,
            evidence_digest: "f".repeat(64),
        });
        unknown.evidence_digest = expected_worker_evidence_digest(&unknown);
        assert!(matches!(
            unknown.validate(),
            Err(WorkerCertificationEvidenceError::UnknownCheck)
        ));

        let mut tampered = evidence(true);
        tampered.retained_audit_entries += 1;
        assert!(matches!(
            tampered.validate(),
            Err(WorkerCertificationEvidenceError::InvalidField(
                "evidence_digest"
            ))
        ));
    }
}
