//! Secret-free release-drill evidence for the operations roadmap gate.
//!
//! Unit tests can prove individual invariants, but Stage 11 requires a dated
//! runbook execution across a packaged desktop and a hosted service.  This
//! report shape makes that distinction explicit and prevents a partial or
//! single-environment report from being treated as operational certification.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const OPERATIONS_DRILL_SCHEMA: &str = "grokptah.operations-drill-report.v1";
pub const MAX_OPERATIONS_DRILL_CHECKS: usize = 14;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum OperationsDrillKind {
    BackupRestore,
    ReadyHealth,
    RestartRecovery,
    CursorExpiry,
    CredentialRotation,
    UpgradeRollback,
    DiskFull,
    CorruptState,
    TornState,
    SoleWriter,
    MonitoringAlerts,
    BackupConfidentiality,
    ComputerUseTakeover,
    BuildTargetCleanup,
}

impl OperationsDrillKind {
    pub const ALL: [Self; MAX_OPERATIONS_DRILL_CHECKS] = [
        Self::BackupRestore,
        Self::ReadyHealth,
        Self::RestartRecovery,
        Self::CursorExpiry,
        Self::CredentialRotation,
        Self::UpgradeRollback,
        Self::DiskFull,
        Self::CorruptState,
        Self::TornState,
        Self::SoleWriter,
        Self::MonitoringAlerts,
        Self::BackupConfidentiality,
        Self::ComputerUseTakeover,
        Self::BuildTargetCleanup,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::BackupRestore => "backup_restore",
            Self::ReadyHealth => "ready_health",
            Self::RestartRecovery => "restart_recovery",
            Self::CursorExpiry => "cursor_expiry",
            Self::CredentialRotation => "credential_rotation",
            Self::UpgradeRollback => "upgrade_rollback",
            Self::DiskFull => "disk_full",
            Self::CorruptState => "corrupt_state",
            Self::TornState => "torn_state",
            Self::SoleWriter => "sole_writer",
            Self::MonitoringAlerts => "monitoring_alerts",
            Self::BackupConfidentiality => "backup_confidentiality",
            Self::ComputerUseTakeover => "computer_use_takeover",
            Self::BuildTargetCleanup => "build_target_cleanup",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationsDrillEnvironment {
    PackagedDesktop,
    HostedService,
    Combined,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OperationsDrillCheck {
    pub kind: OperationsDrillKind,
    pub passed: bool,
    pub duration_ms: u64,
    pub evidence_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BuildTargetCleanupEvidence {
    pub target_fingerprint: String,
    pub owner_id: String,
    pub cargo_checked: bool,
    pub rustc_checked: bool,
    pub open_handles_checked: bool,
    pub active_deletion_refused: bool,
    pub removed_when_inactive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OperationsDrillReport {
    pub schema: String,
    pub report_id: String,
    pub release_id: String,
    pub executed_at: DateTime<Utc>,
    pub environment: OperationsDrillEnvironment,
    pub checks: Vec<OperationsDrillCheck>,
    pub rto_ms: u64,
    pub rpo_ms: u64,
    pub build_target_cleanup: BuildTargetCleanupEvidence,
    pub secret_free: bool,
    pub claim_eligible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationsDrillError {
    InvalidField(&'static str),
    UnsupportedSchema,
    MissingCheck(&'static str),
    DuplicateCheck,
    NotEligible(&'static str),
}

impl std::fmt::Display for OperationsDrillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidField(name) => write!(f, "invalid operations drill field: {name}"),
            Self::UnsupportedSchema => write!(f, "unsupported operations drill schema"),
            Self::MissingCheck(name) => write!(f, "operations drill is missing check: {name}"),
            Self::DuplicateCheck => write!(f, "operations drill contains a duplicate check"),
            Self::NotEligible(name) => write!(f, "operations drill is not eligible: {name}"),
        }
    }
}

impl std::error::Error for OperationsDrillError {}

impl OperationsDrillReport {
    pub fn validate(&self) -> Result<(), OperationsDrillError> {
        if self.schema != OPERATIONS_DRILL_SCHEMA {
            return Err(OperationsDrillError::UnsupportedSchema);
        }
        if !valid_opaque_id(&self.report_id) || !valid_opaque_id(&self.release_id) {
            return Err(OperationsDrillError::InvalidField("identity"));
        }
        if self.executed_at.timestamp() < 0 || !self.secret_free {
            return Err(OperationsDrillError::InvalidField("secret_free"));
        }
        if self.rto_ms == 0 || self.rpo_ms == 0 {
            return Err(OperationsDrillError::InvalidField("rto_rpo"));
        }
        if self.checks.len() != MAX_OPERATIONS_DRILL_CHECKS {
            return Err(OperationsDrillError::NotEligible("check cardinality"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for check in &self.checks {
            if !seen.insert(check.kind) {
                return Err(OperationsDrillError::DuplicateCheck);
            }
            if check.duration_ms == 0 || !valid_fingerprint(&check.evidence_digest) {
                return Err(OperationsDrillError::InvalidField("check"));
            }
            if !check.passed {
                return Err(OperationsDrillError::NotEligible(check.kind.id()));
            }
        }
        for kind in OperationsDrillKind::ALL {
            if !seen.contains(&kind) {
                return Err(OperationsDrillError::MissingCheck(kind.id()));
            }
        }
        let cleanup = &self.build_target_cleanup;
        if !valid_fingerprint(&cleanup.target_fingerprint)
            || !valid_opaque_id(&cleanup.owner_id)
            || !cleanup.cargo_checked
            || !cleanup.rustc_checked
            || !cleanup.open_handles_checked
            || !cleanup.active_deletion_refused
            || !cleanup.removed_when_inactive
        {
            return Err(OperationsDrillError::NotEligible("build-target cleanup"));
        }
        if self.claim_eligible && self.environment != OperationsDrillEnvironment::Combined {
            return Err(OperationsDrillError::NotEligible("combined environment"));
        }
        Ok(())
    }

    pub fn certification_ready(&self) -> bool {
        self.claim_eligible && self.validate().is_ok()
    }
}

fn valid_opaque_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

fn valid_fingerprint(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(
        environment: OperationsDrillEnvironment,
        claim_eligible: bool,
    ) -> OperationsDrillReport {
        OperationsDrillReport {
            schema: OPERATIONS_DRILL_SCHEMA.into(),
            report_id: "ops-report-1".into(),
            release_id: "release-1".into(),
            executed_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            environment,
            checks: OperationsDrillKind::ALL
                .into_iter()
                .map(|kind| OperationsDrillCheck {
                    kind,
                    passed: true,
                    duration_ms: 100,
                    evidence_digest: "a".repeat(64),
                })
                .collect(),
            rto_ms: 30_000,
            rpo_ms: 5_000,
            build_target_cleanup: BuildTargetCleanupEvidence {
                target_fingerprint: "b".repeat(64),
                owner_id: "ops-owner".into(),
                cargo_checked: true,
                rustc_checked: true,
                open_handles_checked: true,
                active_deletion_refused: true,
                removed_when_inactive: true,
            },
            secret_free: true,
            claim_eligible,
        }
    }

    #[test]
    fn combined_complete_report_is_ready() {
        let report = report(OperationsDrillEnvironment::Combined, true);
        report.validate().unwrap();
        assert!(report.certification_ready());
        let encoded = serde_json::to_string(&report).unwrap();
        assert!(!encoded.contains("bearer"));
        assert!(!encoded.contains("api_key"));
    }

    #[test]
    fn single_environment_or_failed_check_cannot_claim() {
        let desktop = report(OperationsDrillEnvironment::PackagedDesktop, true);
        assert_eq!(
            desktop.validate(),
            Err(OperationsDrillError::NotEligible("combined environment"))
        );
        let mut failed = report(OperationsDrillEnvironment::Combined, true);
        failed.checks[0].passed = false;
        assert_eq!(
            failed.validate(),
            Err(OperationsDrillError::NotEligible("backup_restore"))
        );
    }

    #[test]
    fn cleanup_and_cardinality_gates_fail_closed() {
        let mut duplicate = report(OperationsDrillEnvironment::Combined, true);
        duplicate.checks[1].kind = duplicate.checks[0].kind;
        assert_eq!(
            duplicate.validate(),
            Err(OperationsDrillError::DuplicateCheck)
        );

        let mut cleanup = report(OperationsDrillEnvironment::Combined, true);
        cleanup.build_target_cleanup.active_deletion_refused = false;
        assert_eq!(
            cleanup.validate(),
            Err(OperationsDrillError::NotEligible("build-target cleanup"))
        );
    }

    #[test]
    fn unknown_fields_and_bad_evidence_are_rejected() {
        let mut value =
            serde_json::to_value(report(OperationsDrillEnvironment::Combined, true)).unwrap();
        value["extra"] = serde_json::json!(true);
        assert!(serde_json::from_value::<OperationsDrillReport>(value).is_err());

        let mut bad = report(OperationsDrillEnvironment::Combined, true);
        bad.checks[0].evidence_digest = "not-a-digest".into();
        assert_eq!(
            bad.validate(),
            Err(OperationsDrillError::InvalidField("check"))
        );
    }
}
