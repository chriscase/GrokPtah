//! Fail-closed qualification of GrokPtah post-soak release evidence.
//!
//! A Stage 6 Always-On soak writes one evidence report when it exits. This
//! crate decides whether that report is sufficient to qualify an exact
//! candidate commit, and binds an immutable release record to the head that
//! qualified.
//!
//! Three properties hold by construction:
//!
//! * **Nothing is inferred.** Report types carry no serde defaults and reject
//!   unknown fields, so a missing measurement fails to parse rather than
//!   quietly reading as zero, and evidence this schema does not define is
//!   refused instead of ignored.
//! * **A report never qualifies itself.** For each of the seven ordered checks
//!   the verifier evaluates the underlying measurements independently, then
//!   requires the writer's declared outcome to agree. A declared pass the
//!   measurements do not support is a rejection, and a report that arrives
//!   already asserting [`report::ClaimState::Qualified`] is refused outright.
//! * **Qualification is unforgeable.** [`verify::QualifiedCandidate`] and
//!   [`release::ReleaseRecord`] have no `Deserialize` implementation and no
//!   public constructor, so the only way to hold one is to have passed
//!   verification.
//!
//! This crate produces no evidence. It reads what a soak actually wrote; it
//! cannot stand in for a soak that has not run, and a passing verification
//! says nothing about live provider, packaged, or hardware gates.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Immutable release identity bound to a qualified head.
pub mod release;
/// Strict evidence report schema.
pub mod report;
/// Fail-closed verification.
pub mod verify;

pub use release::{ReleaseArtifact, ReleaseRecord};
pub use report::{
    AuditRetention, CandidateIdentity, ClaimState, ContinuityMeasurements, CredentialLifecycle,
    DurationSource, EVIDENCE_SCHEMA_ID, EVIDENCE_SCHEMA_VERSION, QualificationCheckRecord,
    QualificationEvidenceReport, REQUIRED_CHECK_ORDER, SOAK_EXIT_MARKER, SchemaIdentity,
    SoakOutcome, WorkerCertification, canonical_evidence_bytes, recompute_evidence_digest,
};
pub use verify::{
    Finding, MAX_REPORT_BYTES, MINIMUM_CERTIFIED_WORKERS, MINIMUM_RESTARTS, QualificationPolicy,
    QualifiedCandidate, Rejection, locate_sole_report, qualify_bytes, qualify_from_directory,
};
