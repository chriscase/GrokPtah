//! Strict data types for a Stage 6 post-soak qualification evidence report.
//!
//! Every type here rejects unknown fields and declares no serde defaults: a
//! report that omits a measurement, or carries one this schema does not define,
//! fails to parse instead of being silently completed. Nothing in this module
//! grants qualification. These types only describe what a completed soak must
//! have written down; [`crate::verify`] decides whether it is sufficient.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Schema identifier every accepted report must declare verbatim.
pub const EVIDENCE_SCHEMA_ID: &str = "grokptah.post_soak_qualification";

/// Schema version every accepted report must declare verbatim.
pub const EVIDENCE_SCHEMA_VERSION: u32 = 1;

/// Terminal marker a Stage 6 soak writes only after a clean, complete exit.
pub const SOAK_EXIT_MARKER: &str = "GROKPTAH_SOAK_EXIT_V1";

/// Ordered identifiers of the seven post-soak qualification checks.
///
/// An accepted report carries exactly these identifiers, exactly once each, in
/// exactly this order. Missing, extra, and reordered checks are all rejected.
pub const REQUIRED_CHECK_ORDER: [&str; 7] = [
    "soak_exit_marker",
    "worker_isolation",
    "credential_lifecycle",
    "restart_recovery",
    "duplicate_suppression",
    "audit_retention",
    "evidence_integrity",
];

/// How a reported soak duration was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurationSource {
    /// The writer observed both soak boundaries and subtracted them.
    Measured,
    /// The writer copied a configured or intended duration.
    Declared,
    /// The writer approximated the duration from partial signals.
    Estimated,
}

/// Claim posture a report is allowed to assert about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimState {
    /// The only state an evidence writer may emit.
    PendingVerification,
    /// A self-asserted claim. Verification rejects reports in this state:
    /// qualification is produced by the verifier, never by the report.
    Qualified,
}

/// Schema identity block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SchemaIdentity {
    /// Schema identifier; must equal [`EVIDENCE_SCHEMA_ID`].
    pub id: String,
    /// Schema version; must equal [`EVIDENCE_SCHEMA_VERSION`].
    pub version: u32,
}

/// Exact source identity the soak ran against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandidateIdentity {
    /// Full 40-character lowercase hex commit the soak ran against.
    pub candidate_head: String,
    /// Full 40-character lowercase hex first parent of the candidate head.
    pub parent_head: String,
}

/// How the soak process ended and what it still owned at exit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SoakOutcome {
    /// Terminal marker written on clean exit; must equal [`SOAK_EXIT_MARKER`].
    pub exit_marker: String,
    /// Child processes still owned by the soak at exit.
    pub owned_processes: u32,
    /// File and socket handles still owned by the soak at exit.
    pub owned_open_handles: u32,
    /// Duration the soak was configured to run for, in seconds.
    pub configured_seconds: u64,
    /// Duration the soak actually ran for, in seconds.
    pub measured_seconds: u64,
    /// Provenance of `measured_seconds`.
    pub duration_source: DurationSource,
}

/// One certified worker and the credential binding it ran under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerCertification {
    /// Stable identifier of the worker, unique within the report.
    pub worker_id: String,
    /// Credential binding the worker ran under, unique within the report.
    pub credential_binding_id: String,
    /// Executions this worker completed during the soak.
    pub executions: u64,
    /// Executions this worker performed more than once.
    pub duplicate_executions: u64,
}

/// Credential issuance, scoping, and rotation observed during the soak.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialLifecycle {
    /// Credentials issued, one per certified worker.
    pub issued: u32,
    /// Scope names granted to issued credentials.
    pub least_privilege_scopes: Vec<String>,
    /// Scope grants outside the least-privilege allowlist.
    pub privileged_scopes_requested: u32,
    /// Completed rotations.
    pub rotations: u32,
    /// Attempts with a rotated-out credential that were rejected.
    pub old_credential_rejections: u32,
    /// Attempts with a freshly rotated credential that were accepted.
    pub new_credential_acceptances: u32,
}

/// Restart and resume behaviour observed during the soak.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContinuityMeasurements {
    /// Full process restarts the soak drove and recovered from.
    pub restarts: u32,
    /// Resumes whose completion state could not be determined.
    pub uncertain_resumes: u64,
    /// Workers left running or unaccounted for after a restart.
    pub leaked_workers: u32,
}

/// Audit retention observed across the soak.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuditRetention {
    /// Audit records still readable at soak exit.
    pub records_retained: u64,
    /// Audit records lost, truncated, or evicted during the soak.
    pub records_dropped: u64,
    /// Whether retained records survived every restart.
    pub retained_across_restarts: bool,
}

/// One declared qualification check outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QualificationCheckRecord {
    /// Check identifier from [`REQUIRED_CHECK_ORDER`].
    pub id: String,
    /// Whether the writer observed the check to pass.
    pub passed: bool,
    /// What the writer observed. Must be non-empty.
    pub observed_detail: String,
}

/// A complete post-soak qualification evidence report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QualificationEvidenceReport {
    /// Schema identity.
    pub schema: SchemaIdentity,
    /// Exact candidate and parent commits.
    pub identity: CandidateIdentity,
    /// Soak exit state and duration.
    pub soak: SoakOutcome,
    /// Certified workers and their credential bindings.
    pub workers: Vec<WorkerCertification>,
    /// Credential issuance, scoping, and rotation.
    pub credentials: CredentialLifecycle,
    /// Restart and resume behaviour.
    pub continuity: ContinuityMeasurements,
    /// Audit retention.
    pub audit: AuditRetention,
    /// The seven declared check outcomes, in [`REQUIRED_CHECK_ORDER`].
    pub checks: Vec<QualificationCheckRecord>,
    /// Claim posture. Writers may only emit
    /// [`ClaimState::PendingVerification`].
    pub claim_state: ClaimState,
    /// Unix seconds at which the report body was sealed.
    pub generated_at_unix_seconds: u64,
    /// Lowercase hex SHA-256 over the canonical report body.
    pub evidence_digest_sha256: String,
}

/// The digest domain: every report field except the digest itself.
///
/// Field order here is the canonical encoding order and must not be reordered
/// without a schema version bump.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EvidenceBody<'a> {
    schema: &'a SchemaIdentity,
    identity: &'a CandidateIdentity,
    soak: &'a SoakOutcome,
    workers: &'a [WorkerCertification],
    credentials: &'a CredentialLifecycle,
    continuity: &'a ContinuityMeasurements,
    audit: &'a AuditRetention,
    checks: &'a [QualificationCheckRecord],
    claim_state: ClaimState,
    generated_at_unix_seconds: u64,
}

/// Canonical bytes a report's evidence digest is computed over.
///
/// Encoding is `serde_json` over a fixed-order struct rather than over a
/// dynamic map, so the result does not depend on `serde_json` map-ordering
/// features enabled elsewhere in the dependency graph.
pub fn canonical_evidence_bytes(
    report: &QualificationEvidenceReport,
) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&EvidenceBody {
        schema: &report.schema,
        identity: &report.identity,
        soak: &report.soak,
        workers: &report.workers,
        credentials: &report.credentials,
        continuity: &report.continuity,
        audit: &report.audit,
        checks: &report.checks,
        claim_state: report.claim_state,
        generated_at_unix_seconds: report.generated_at_unix_seconds,
    })
}

/// Recomputes a report's evidence digest from its own body.
///
/// The verifier compares this against the digest the report declares; it never
/// accepts a declared digest on its own.
pub fn recompute_evidence_digest(
    report: &QualificationEvidenceReport,
) -> Result<String, serde_json::Error> {
    let bytes = canonical_evidence_bytes(report)?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}
