//! Fail-closed verification of a post-soak qualification evidence report.
//!
//! The verifier never infers a missing fact and never accepts a report's own
//! assessment of itself. For each of the seven ordered checks it independently
//! evaluates the measurements the report carries, and then requires the
//! writer's declared outcome to agree with what those measurements show. A
//! report that declares a passing check the measurements do not support is
//! rejected, as is one that omits, duplicates, reorders, back-dates, or leaks
//! secrets into its evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::release::{ReleaseArtifact, ReleaseRecord};
use crate::report::{
    ClaimState, DurationSource, EVIDENCE_SCHEMA_ID, EVIDENCE_SCHEMA_VERSION,
    QualificationEvidenceReport, REQUIRED_CHECK_ORDER, SOAK_EXIT_MARKER, recompute_evidence_digest,
};

/// Largest evidence report the verifier will read, in bytes.
pub const MAX_REPORT_BYTES: u64 = 1_048_576;

/// Length of a full git object identifier in lowercase hex.
const COMMIT_HEX_LEN: usize = 40;

/// Length of a SHA-256 digest in lowercase hex.
pub(crate) const SHA256_HEX_LEN: usize = 64;

/// Fewest certified workers that can demonstrate distinct workers and
/// distinct credential bindings at all. A policy cannot lower this.
pub const MINIMUM_CERTIFIED_WORKERS: usize = 2;

/// Fewest restarts that can demonstrate restart recovery at all. A policy
/// cannot lower this.
pub const MINIMUM_RESTARTS: u32 = 1;

/// Lowercase substrings that must never appear in an evidence report.
///
/// The scan runs over the raw report bytes before parsing, so a secret hidden
/// in an unparsed or malformed region is still caught.
const SECRET_MARKERS: &[&str] = &[
    "-----begin",
    "api-key",
    "api_key",
    "apikey",
    "authorization",
    "bearer ",
    "passwd",
    "password",
    "private_key",
    "privatekey",
    "secret",
    "token",
];

/// One reason a report was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// What the finding is about, for example `identity` or `check:audit_retention`.
    pub subject: String,
    /// What was wrong.
    pub detail: String,
}

impl fmt::Display for Finding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.subject, self.detail)
    }
}

/// A refusal to qualify, carrying every reason found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    findings: Vec<Finding>,
}

impl Rejection {
    pub(crate) fn new(findings: Vec<Finding>) -> Self {
        debug_assert!(!findings.is_empty(), "a rejection must carry a reason");
        Self { findings }
    }

    pub(crate) fn single(subject: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(vec![Finding {
            subject: subject.into(),
            detail: detail.into(),
        }])
    }

    /// Every reason this evidence was refused.
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.findings.len();
        let noun = if count == 1 { "finding" } else { "findings" };
        write!(formatter, "evidence rejected ({count} {noun})")?;
        for finding in &self.findings {
            write!(formatter, "\n  - {finding}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Rejection {}

/// The exact identity and thresholds a candidate must satisfy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QualificationPolicy {
    /// Full 40-character lowercase hex commit under qualification.
    pub expected_candidate_head: String,
    /// Full 40-character lowercase hex first parent of the candidate head.
    pub expected_parent_head: String,
    /// Shortest soak the candidate may be qualified on, in seconds.
    pub minimum_soak_seconds: u64,
    /// Fewest distinct certified workers the soak must have exercised.
    pub minimum_workers: usize,
    /// Fewest full restarts the soak must have recovered from.
    pub minimum_restarts: u32,
    /// Fewest audit records that must remain readable at soak exit.
    pub minimum_audit_records: u64,
    /// Oldest a report may be, relative to verification time, in seconds.
    pub maximum_report_age_seconds: u64,
    /// The complete set of scopes a worker credential may hold.
    pub allowed_scopes: BTreeSet<String>,
}

/// A candidate that satisfied every qualification rule.
///
/// This type has no `Deserialize` implementation and no public constructor: the
/// only way to obtain one is to pass verification, so it cannot be forged from
/// a serialized value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QualifiedCandidate {
    candidate_head: String,
    parent_head: String,
    evidence_digest_sha256: String,
    measured_soak_seconds: u64,
    certified_workers: usize,
    restarts: u32,
    audit_records_retained: u64,
    qualified_at_unix_seconds: u64,
}

impl QualifiedCandidate {
    /// The exact commit that was qualified.
    pub fn candidate_head(&self) -> &str {
        &self.candidate_head
    }

    /// The exact first parent of the qualified commit.
    pub fn parent_head(&self) -> &str {
        &self.parent_head
    }

    /// The recomputed evidence digest this qualification rests on.
    pub fn evidence_digest_sha256(&self) -> &str {
        &self.evidence_digest_sha256
    }

    /// Soak duration actually measured, in seconds.
    pub fn measured_soak_seconds(&self) -> u64 {
        self.measured_soak_seconds
    }

    /// Number of distinct certified workers.
    pub fn certified_workers(&self) -> usize {
        self.certified_workers
    }

    /// Restarts the soak recovered from.
    pub fn restarts(&self) -> u32 {
        self.restarts
    }

    /// Audit records still readable at soak exit.
    pub fn audit_records_retained(&self) -> u64 {
        self.audit_records_retained
    }

    /// Unix seconds at which verification granted this qualification.
    pub fn qualified_at_unix_seconds(&self) -> u64 {
        self.qualified_at_unix_seconds
    }

    /// Binds an immutable release record to this exact qualified head.
    ///
    /// Artifact metadata is validated and canonicalized here; the resulting
    /// record cannot be modified or deserialized back into existence.
    pub fn bind_release(
        &self,
        artifacts: Vec<ReleaseArtifact>,
    ) -> Result<ReleaseRecord, Rejection> {
        ReleaseRecord::bind(self, artifacts)
    }
}

/// Locates the single evidence report inside `directory`.
///
/// The directory must contain exactly one entry and that entry must be a
/// regular file. A missing report, a second file, a subdirectory, and a symlink
/// are all rejected; symlinks are classified without being followed.
pub fn locate_sole_report(directory: &Path) -> Result<PathBuf, Rejection> {
    let entries = fs::read_dir(directory).map_err(|error| {
        Rejection::single(
            "evidence_directory",
            format!("cannot read {}: {error}", directory.display()),
        )
    })?;

    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            Rejection::single(
                "evidence_directory",
                format!("cannot enumerate {}: {error}", directory.display()),
            )
        })?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort();

    match names.len() {
        1 => {}
        0 => {
            return Err(Rejection::single(
                "evidence_directory",
                format!("no evidence report in {}", directory.display()),
            ));
        }
        count => {
            return Err(Rejection::single(
                "evidence_directory",
                format!(
                    "expected exactly one evidence report in {}, found {count}: {}",
                    directory.display(),
                    names.join(", ")
                ),
            ));
        }
    }

    let path = directory.join(&names[0]);
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        Rejection::single(
            "evidence_report",
            format!("cannot stat {}: {error}", path.display()),
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(Rejection::single(
            "evidence_report",
            format!(
                "{} is a symlink; a regular file is required",
                path.display()
            ),
        ));
    }
    if !metadata.file_type().is_file() {
        return Err(Rejection::single(
            "evidence_report",
            format!("{} is not a regular file", path.display()),
        ));
    }
    if metadata.len() > MAX_REPORT_BYTES {
        return Err(Rejection::single(
            "evidence_report",
            format!(
                "{} is {} bytes, over the {MAX_REPORT_BYTES}-byte ceiling",
                path.display(),
                metadata.len()
            ),
        ));
    }
    Ok(path)
}

/// Verifies the sole evidence report in `directory` against `policy`.
pub fn qualify_from_directory(
    directory: &Path,
    policy: &QualificationPolicy,
    now_unix_seconds: u64,
) -> Result<QualifiedCandidate, Rejection> {
    let path = locate_sole_report(directory)?;
    let bytes = fs::read(&path).map_err(|error| {
        Rejection::single(
            "evidence_report",
            format!("cannot read {}: {error}", path.display()),
        )
    })?;
    qualify_bytes(&bytes, policy, now_unix_seconds)
}

/// Verifies already-read evidence bytes against `policy`.
pub fn qualify_bytes(
    bytes: &[u8],
    policy: &QualificationPolicy,
    now_unix_seconds: u64,
) -> Result<QualifiedCandidate, Rejection> {
    let secret_findings = scan_for_secrets(bytes);

    let report: QualificationEvidenceReport = match serde_json::from_slice(bytes) {
        Ok(report) => report,
        Err(error) => {
            let mut findings = vec![Finding {
                subject: "evidence_report".into(),
                detail: format!("report does not match schema exactly: {error}"),
            }];
            findings.extend(secret_findings.into_iter().map(|detail| Finding {
                subject: "check:evidence_integrity".into(),
                detail,
            }));
            return Err(Rejection::new(findings));
        }
    };

    let mut findings = policy_findings(policy);
    findings.extend(report_level_findings(&report, policy, now_unix_seconds));

    let measured = measured_failures(&report, policy, secret_findings);
    for id in REQUIRED_CHECK_ORDER {
        for detail in measured.get(id).into_iter().flatten() {
            findings.push(Finding {
                subject: format!("measurement:{id}"),
                detail: detail.clone(),
            });
        }
    }
    findings.extend(declaration_findings(&report, &measured));

    if findings.is_empty() {
        Ok(QualifiedCandidate {
            candidate_head: report.identity.candidate_head.clone(),
            parent_head: report.identity.parent_head.clone(),
            evidence_digest_sha256: report.evidence_digest_sha256.clone(),
            measured_soak_seconds: report.soak.measured_seconds,
            certified_workers: report.workers.len(),
            restarts: report.continuity.restarts,
            audit_records_retained: report.audit.records_retained,
            qualified_at_unix_seconds: now_unix_seconds,
        })
    } else {
        Err(Rejection::new(findings))
    }
}

/// Rejects a policy that cannot pin an exact candidate, before any evidence is
/// weighed against it.
fn policy_findings(policy: &QualificationPolicy) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (label, value) in [
        ("expected candidate head", &policy.expected_candidate_head),
        ("expected parent head", &policy.expected_parent_head),
    ] {
        if !is_lowercase_hex(value, COMMIT_HEX_LEN) {
            findings.push(Finding {
                subject: "policy".into(),
                detail: format!("{label} {value:?} is not a full lowercase hex commit"),
            });
        }
    }
    if policy.expected_candidate_head == policy.expected_parent_head {
        findings.push(Finding {
            subject: "policy".into(),
            detail: "expected candidate head and parent head are the same commit".into(),
        });
    }
    findings
}

fn scan_for_secrets(bytes: &[u8]) -> Vec<String> {
    let lowered = String::from_utf8_lossy(bytes).to_lowercase();
    SECRET_MARKERS
        .iter()
        .filter(|marker| lowered.contains(**marker))
        .map(|marker| format!("report contains the forbidden secret marker {marker:?}"))
        .collect()
}

fn report_level_findings(
    report: &QualificationEvidenceReport,
    policy: &QualificationPolicy,
    now_unix_seconds: u64,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut push = |subject: &str, detail: String| {
        findings.push(Finding {
            subject: subject.into(),
            detail,
        });
    };

    if report.schema.id != EVIDENCE_SCHEMA_ID {
        push(
            "schema",
            format!(
                "schema id is {:?}, expected {EVIDENCE_SCHEMA_ID:?}",
                report.schema.id
            ),
        );
    }
    if report.schema.version != EVIDENCE_SCHEMA_VERSION {
        push(
            "schema",
            format!(
                "schema version is {}, expected {EVIDENCE_SCHEMA_VERSION}",
                report.schema.version
            ),
        );
    }

    for (label, expected, actual) in [
        (
            "candidate head",
            &policy.expected_candidate_head,
            &report.identity.candidate_head,
        ),
        (
            "parent head",
            &policy.expected_parent_head,
            &report.identity.parent_head,
        ),
    ] {
        if !is_lowercase_hex(actual, COMMIT_HEX_LEN) {
            push(
                "identity",
                format!("{label} {actual:?} is not a full lowercase hex commit"),
            );
        } else if actual != expected {
            push(
                "identity",
                format!("{label} is {actual}, expected {expected}"),
            );
        }
    }
    if report.identity.candidate_head == report.identity.parent_head {
        push(
            "identity",
            "candidate head and parent head are the same commit".into(),
        );
    }

    if report.generated_at_unix_seconds > now_unix_seconds {
        push(
            "freshness",
            format!(
                "report is dated {} which is after verification time {now_unix_seconds}",
                report.generated_at_unix_seconds
            ),
        );
    } else {
        let age = now_unix_seconds - report.generated_at_unix_seconds;
        if age > policy.maximum_report_age_seconds {
            push(
                "freshness",
                format!(
                    "report is {age}s old, over the {}s ceiling",
                    policy.maximum_report_age_seconds
                ),
            );
        }
    }

    findings
}

/// Independently evaluates each of the seven checks from the measurements.
///
/// The returned map holds one entry per check identifier; an empty vector means
/// the measurements support that check.
fn measured_failures(
    report: &QualificationEvidenceReport,
    policy: &QualificationPolicy,
    secret_findings: Vec<String>,
) -> BTreeMap<&'static str, Vec<String>> {
    let mut failures: BTreeMap<&'static str, Vec<String>> = REQUIRED_CHECK_ORDER
        .iter()
        .map(|id| (*id, Vec::new()))
        .collect();
    let mut fail = |id: &'static str, detail: String| {
        failures.entry(id).or_default().push(detail);
    };

    let soak = &report.soak;
    if soak.exit_marker != SOAK_EXIT_MARKER {
        fail(
            "soak_exit_marker",
            format!(
                "exit marker is {:?}, expected {SOAK_EXIT_MARKER:?}",
                soak.exit_marker
            ),
        );
    }
    if soak.owned_processes != 0 {
        fail(
            "soak_exit_marker",
            format!("{} processes still owned at exit", soak.owned_processes),
        );
    }
    if soak.owned_open_handles != 0 {
        fail(
            "soak_exit_marker",
            format!(
                "{} open handles still owned at exit",
                soak.owned_open_handles
            ),
        );
    }
    if soak.duration_source != DurationSource::Measured {
        fail(
            "soak_exit_marker",
            format!("soak duration is {:?}, not measured", soak.duration_source),
        );
    }
    if soak.configured_seconds < policy.minimum_soak_seconds {
        fail(
            "soak_exit_marker",
            format!(
                "configured soak is {}s, under the required {}s",
                soak.configured_seconds, policy.minimum_soak_seconds
            ),
        );
    }
    if soak.measured_seconds < soak.configured_seconds {
        fail(
            "soak_exit_marker",
            format!(
                "measured soak is {}s, short of the configured {}s",
                soak.measured_seconds, soak.configured_seconds
            ),
        );
    }

    let required_workers = policy.minimum_workers.max(MINIMUM_CERTIFIED_WORKERS);
    if report.workers.len() < required_workers {
        fail(
            "worker_isolation",
            format!(
                "{} certified workers, under the required {required_workers}",
                report.workers.len()
            ),
        );
    }
    let mut worker_ids = BTreeSet::new();
    let mut binding_ids = BTreeSet::new();
    for worker in &report.workers {
        if worker.worker_id.trim().is_empty() {
            fail("worker_isolation", "a worker has an empty id".into());
        } else if !worker_ids.insert(worker.worker_id.as_str()) {
            fail(
                "worker_isolation",
                format!("worker id {:?} appears more than once", worker.worker_id),
            );
        }
        if worker.credential_binding_id.trim().is_empty() {
            fail(
                "worker_isolation",
                format!(
                    "worker {:?} has an empty credential binding",
                    worker.worker_id
                ),
            );
        } else if !binding_ids.insert(worker.credential_binding_id.as_str()) {
            fail(
                "worker_isolation",
                format!(
                    "credential binding {:?} is shared by more than one worker",
                    worker.credential_binding_id
                ),
            );
        }
        if worker.executions == 0 {
            fail(
                "worker_isolation",
                format!("worker {:?} executed nothing", worker.worker_id),
            );
        }
        if worker.duplicate_executions != 0 {
            fail(
                "duplicate_suppression",
                format!(
                    "worker {:?} recorded {} duplicate executions",
                    worker.worker_id, worker.duplicate_executions
                ),
            );
        }
    }

    let credentials = &report.credentials;
    if credentials.issued as usize != report.workers.len() {
        fail(
            "credential_lifecycle",
            format!(
                "{} credentials issued for {} certified workers",
                credentials.issued,
                report.workers.len()
            ),
        );
    }
    if credentials.least_privilege_scopes.is_empty() {
        fail(
            "credential_lifecycle",
            "no least-privilege scopes recorded".into(),
        );
    }
    let mut seen_scopes = BTreeSet::new();
    for scope in &credentials.least_privilege_scopes {
        if !seen_scopes.insert(scope.as_str()) {
            fail(
                "credential_lifecycle",
                format!("scope {scope:?} is listed more than once"),
            );
        }
        if !policy.allowed_scopes.contains(scope) {
            fail(
                "credential_lifecycle",
                format!("scope {scope:?} is outside the least-privilege allowlist"),
            );
        }
    }
    if credentials.privileged_scopes_requested != 0 {
        fail(
            "credential_lifecycle",
            format!(
                "{} privileged scope grants were requested",
                credentials.privileged_scopes_requested
            ),
        );
    }
    if credentials.rotations == 0 {
        fail(
            "credential_lifecycle",
            "no credential rotation occurred".into(),
        );
    }
    if credentials.old_credential_rejections < credentials.rotations {
        fail(
            "credential_lifecycle",
            format!(
                "{} rotations but only {} rotated-out credentials were rejected",
                credentials.rotations, credentials.old_credential_rejections
            ),
        );
    }
    if credentials.new_credential_acceptances < credentials.rotations {
        fail(
            "credential_lifecycle",
            format!(
                "{} rotations but only {} rotated-in credentials were accepted",
                credentials.rotations, credentials.new_credential_acceptances
            ),
        );
    }

    let continuity = &report.continuity;
    let required_restarts = policy.minimum_restarts.max(MINIMUM_RESTARTS);
    if continuity.restarts < required_restarts {
        fail(
            "restart_recovery",
            format!(
                "{} restarts, under the required {required_restarts}",
                continuity.restarts
            ),
        );
    }
    if continuity.uncertain_resumes != 0 {
        fail(
            "restart_recovery",
            format!(
                "{} resumes ended in an uncertain state",
                continuity.uncertain_resumes
            ),
        );
    }
    if continuity.leaked_workers != 0 {
        fail(
            "restart_recovery",
            format!(
                "{} workers leaked across restarts",
                continuity.leaked_workers
            ),
        );
    }

    let audit = &report.audit;
    if audit.records_retained < policy.minimum_audit_records {
        fail(
            "audit_retention",
            format!(
                "{} audit records retained, under the required {}",
                audit.records_retained, policy.minimum_audit_records
            ),
        );
    }
    if audit.records_dropped != 0 {
        fail(
            "audit_retention",
            format!("{} audit records were dropped", audit.records_dropped),
        );
    }
    if !audit.retained_across_restarts {
        fail(
            "audit_retention",
            "audit records did not survive every restart".into(),
        );
    }

    for detail in secret_findings {
        fail("evidence_integrity", detail);
    }
    if report.claim_state != ClaimState::PendingVerification {
        fail(
            "evidence_integrity",
            format!(
                "report asserts claim state {:?}; only verification may qualify a candidate",
                report.claim_state
            ),
        );
    }
    if !is_lowercase_hex(&report.evidence_digest_sha256, SHA256_HEX_LEN) {
        fail(
            "evidence_integrity",
            format!(
                "declared evidence digest {:?} is not a lowercase hex SHA-256",
                report.evidence_digest_sha256
            ),
        );
    }
    match recompute_evidence_digest(report) {
        Ok(recomputed) if recomputed == report.evidence_digest_sha256 => {}
        Ok(recomputed) => fail(
            "evidence_integrity",
            format!(
                "declared evidence digest {} does not match the recomputed {recomputed}",
                report.evidence_digest_sha256
            ),
        ),
        Err(error) => fail(
            "evidence_integrity",
            format!("evidence digest could not be recomputed: {error}"),
        ),
    }

    failures
}

/// Requires the declared check list to be exactly the seven ordered checks and
/// to agree with what the measurements independently show.
fn declaration_findings(
    report: &QualificationEvidenceReport,
    measured: &BTreeMap<&'static str, Vec<String>>,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    if report.checks.len() != REQUIRED_CHECK_ORDER.len() {
        findings.push(Finding {
            subject: "checks".into(),
            detail: format!(
                "report declares {} checks, expected exactly {}",
                report.checks.len(),
                REQUIRED_CHECK_ORDER.len()
            ),
        });
    }

    for (position, expected_id) in REQUIRED_CHECK_ORDER.iter().enumerate() {
        let Some(record) = report.checks.get(position) else {
            findings.push(Finding {
                subject: "checks".into(),
                detail: format!("check {expected_id:?} is missing from position {position}"),
            });
            continue;
        };
        if record.id != *expected_id {
            findings.push(Finding {
                subject: "checks".into(),
                detail: format!(
                    "position {position} declares {:?}, expected {expected_id:?}",
                    record.id
                ),
            });
            continue;
        }
        if record.observed_detail.trim().is_empty() {
            findings.push(Finding {
                subject: format!("check:{expected_id}"),
                detail: "check records no observed detail".into(),
            });
        }
        let measured_pass = measured.get(expected_id).is_some_and(Vec::is_empty);
        match (record.passed, measured_pass) {
            (true, true) => {}
            (true, false) => findings.push(Finding {
                subject: format!("check:{expected_id}"),
                detail: "check is declared passing but the measurements do not support it".into(),
            }),
            (false, _) => findings.push(Finding {
                subject: format!("check:{expected_id}"),
                detail: format!("writer recorded a failure: {}", record.observed_detail),
            }),
        }
    }

    for extra in report.checks.iter().skip(REQUIRED_CHECK_ORDER.len()) {
        findings.push(Finding {
            subject: "checks".into(),
            detail: format!("unexpected extra check {:?}", extra.id),
        });
    }

    findings
}

pub(crate) fn is_lowercase_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
