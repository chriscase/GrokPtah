//! Focused coverage for post-soak qualification and release binding.
//!
//! Each test drives one rule: the fixture is a report that qualifies, and every
//! rejection test perturbs exactly one fact and reseals the evidence digest, so
//! a failure names the rule that broke rather than the fixture.

use std::collections::BTreeSet;
use std::fs;

use grokptah_release_evidence::{
    AuditRetention, CandidateIdentity, ClaimState, ContinuityMeasurements, CredentialLifecycle,
    DurationSource, EVIDENCE_SCHEMA_ID, EVIDENCE_SCHEMA_VERSION, MINIMUM_CERTIFIED_WORKERS,
    MINIMUM_RESTARTS, QualificationCheckRecord, QualificationEvidenceReport, QualificationPolicy,
    QualifiedCandidate, REQUIRED_CHECK_ORDER, Rejection, ReleaseArtifact, SOAK_EXIT_MARKER,
    SchemaIdentity, SoakOutcome, WorkerCertification, locate_sole_report, qualify_bytes,
    qualify_from_directory, recompute_evidence_digest,
};
use tempfile::TempDir;

const CANDIDATE_HEAD: &str = "8ad3be07eb27087acb67704fdf463ecb95b64505";
const PARENT_HEAD: &str = "0ff034c1a2b3c4d5e6f708192a3b4c5d6e7f8091";
const NOW: u64 = 1_756_000_000;

fn policy() -> QualificationPolicy {
    QualificationPolicy {
        expected_candidate_head: CANDIDATE_HEAD.into(),
        expected_parent_head: PARENT_HEAD.into(),
        minimum_soak_seconds: 86_400,
        minimum_workers: 3,
        minimum_restarts: 2,
        minimum_audit_records: 1_000,
        maximum_report_age_seconds: 3_600,
        allowed_scopes: BTreeSet::from([
            "audit:append".to_string(),
            "run:execute".to_string(),
            "run:read".to_string(),
        ]),
    }
}

fn check(id: &str, detail: &str) -> QualificationCheckRecord {
    QualificationCheckRecord {
        id: id.into(),
        passed: true,
        observed_detail: detail.into(),
    }
}

fn base_report() -> QualificationEvidenceReport {
    QualificationEvidenceReport {
        schema: SchemaIdentity {
            id: EVIDENCE_SCHEMA_ID.into(),
            version: EVIDENCE_SCHEMA_VERSION,
        },
        identity: CandidateIdentity {
            candidate_head: CANDIDATE_HEAD.into(),
            parent_head: PARENT_HEAD.into(),
        },
        soak: SoakOutcome {
            exit_marker: SOAK_EXIT_MARKER.into(),
            owned_processes: 0,
            owned_open_handles: 0,
            configured_seconds: 86_400,
            measured_seconds: 86_412,
            duration_source: DurationSource::Measured,
        },
        workers: vec![
            WorkerCertification {
                worker_id: "worker-alpha".into(),
                credential_binding_id: "binding-alpha".into(),
                executions: 1_200,
                duplicate_executions: 0,
            },
            WorkerCertification {
                worker_id: "worker-bravo".into(),
                credential_binding_id: "binding-bravo".into(),
                executions: 1_100,
                duplicate_executions: 0,
            },
            WorkerCertification {
                worker_id: "worker-charlie".into(),
                credential_binding_id: "binding-charlie".into(),
                executions: 1_300,
                duplicate_executions: 0,
            },
        ],
        credentials: CredentialLifecycle {
            issued: 3,
            least_privilege_scopes: vec![
                "audit:append".into(),
                "run:execute".into(),
                "run:read".into(),
            ],
            privileged_scopes_requested: 0,
            rotations: 2,
            old_credential_rejections: 2,
            new_credential_acceptances: 2,
        },
        continuity: ContinuityMeasurements {
            restarts: 3,
            uncertain_resumes: 0,
            leaked_workers: 0,
        },
        audit: AuditRetention {
            records_retained: 4_096,
            records_dropped: 0,
            retained_across_restarts: true,
        },
        checks: vec![
            check(
                "soak_exit_marker",
                "clean exit marker with no owned processes or handles",
            ),
            check(
                "worker_isolation",
                "three distinct workers, three distinct bindings",
            ),
            check(
                "credential_lifecycle",
                "one issuance per worker, least privilege scopes only, two rotations",
            ),
            check(
                "restart_recovery",
                "three restarts recovered with no leaked workers",
            ),
            check(
                "duplicate_suppression",
                "no duplicate executions across the soak",
            ),
            check(
                "audit_retention",
                "4096 audit records retained across every restart",
            ),
            check(
                "evidence_integrity",
                "body digest recomputed over the canonical encoding",
            ),
        ],
        claim_state: ClaimState::PendingVerification,
        generated_at_unix_seconds: NOW - 60,
        evidence_digest_sha256: String::new(),
    }
}

fn sealed(mut report: QualificationEvidenceReport) -> QualificationEvidenceReport {
    report.evidence_digest_sha256 =
        recompute_evidence_digest(&report).expect("canonical body must encode");
    report
}

fn qualify(report: &QualificationEvidenceReport) -> Result<QualifiedCandidate, Rejection> {
    qualify_with(report, &policy())
}

fn qualify_with(
    report: &QualificationEvidenceReport,
    policy: &QualificationPolicy,
) -> Result<QualifiedCandidate, Rejection> {
    let bytes = serde_json::to_vec(report).expect("report must encode");
    qualify_bytes(&bytes, policy, NOW)
}

fn qualify_mutated(
    mutate: impl FnOnce(&mut QualificationEvidenceReport),
) -> Result<QualifiedCandidate, Rejection> {
    let mut report = base_report();
    mutate(&mut report);
    qualify(&sealed(report))
}

#[track_caller]
fn assert_rejected<T: std::fmt::Debug>(result: Result<T, Rejection>, needle: &str) {
    let rejection = result.expect_err("evidence must be rejected");
    let rendered = rejection.to_string();
    assert!(
        rendered.contains(needle),
        "expected a finding containing {needle:?}, got:\n{rendered}"
    );
}

fn write_report(directory: &TempDir, name: &str, report: &QualificationEvidenceReport) {
    let bytes = serde_json::to_vec_pretty(report).expect("report must encode");
    fs::write(directory.path().join(name), bytes).expect("report must be writable");
}

// --- accepted evidence -----------------------------------------------------

#[test]
fn valid_evidence_qualifies_the_exact_candidate() {
    let candidate = qualify(&sealed(base_report())).expect("valid evidence must qualify");
    assert_eq!(candidate.candidate_head(), CANDIDATE_HEAD);
    assert_eq!(candidate.parent_head(), PARENT_HEAD);
    assert_eq!(candidate.measured_soak_seconds(), 86_412);
    assert_eq!(candidate.certified_workers(), 3);
    assert_eq!(candidate.restarts(), 3);
    assert_eq!(candidate.audit_records_retained(), 4_096);
    assert_eq!(candidate.qualified_at_unix_seconds(), NOW);
}

#[test]
fn a_sole_regular_report_qualifies_through_the_directory_entry_point() {
    let directory = TempDir::new().expect("temp dir");
    let report = sealed(base_report());
    write_report(&directory, "post-soak-qualification.json", &report);

    let located = locate_sole_report(directory.path()).expect("report must be located");
    assert_eq!(
        located.file_name().and_then(|name| name.to_str()),
        Some("post-soak-qualification.json")
    );

    let candidate = qualify_from_directory(directory.path(), &policy(), NOW)
        .expect("valid evidence must qualify");
    assert_eq!(
        candidate.evidence_digest_sha256(),
        report.evidence_digest_sha256
    );
}

// --- exactly one regular, non-symlink report --------------------------------

#[test]
fn rejects_an_empty_evidence_directory() {
    let directory = TempDir::new().expect("temp dir");
    assert_rejected(
        qualify_from_directory(directory.path(), &policy(), NOW),
        "no evidence report",
    );
}

#[test]
fn rejects_a_second_evidence_file() {
    let directory = TempDir::new().expect("temp dir");
    let report = sealed(base_report());
    write_report(&directory, "post-soak-qualification.json", &report);
    write_report(&directory, "post-soak-qualification.backup.json", &report);
    assert_rejected(
        qualify_from_directory(directory.path(), &policy(), NOW),
        "expected exactly one evidence report",
    );
}

#[test]
fn rejects_a_report_that_is_a_directory() {
    let directory = TempDir::new().expect("temp dir");
    fs::create_dir(directory.path().join("post-soak-qualification.json")).expect("subdirectory");
    assert_rejected(
        qualify_from_directory(directory.path(), &policy(), NOW),
        "is not a regular file",
    );
}

#[cfg(unix)]
#[test]
fn rejects_a_symlinked_report() {
    let outside = TempDir::new().expect("temp dir");
    let directory = TempDir::new().expect("temp dir");
    write_report(&outside, "real.json", &sealed(base_report()));
    std::os::unix::fs::symlink(
        outside.path().join("real.json"),
        directory.path().join("post-soak-qualification.json"),
    )
    .expect("symlink");
    assert_rejected(
        qualify_from_directory(directory.path(), &policy(), NOW),
        "is a symlink",
    );
}

// --- schema exactness -------------------------------------------------------

#[test]
fn rejects_evidence_carrying_an_undefined_field() {
    let report = sealed(base_report());
    let mut encoded = serde_json::to_value(&report).expect("report must encode");
    encoded.as_object_mut().expect("object").insert(
        "operatorAttestation".into(),
        serde_json::json!("looks fine"),
    );
    let bytes = serde_json::to_vec(&encoded).expect("encode");
    assert_rejected(
        qualify_bytes(&bytes, &policy(), NOW),
        "does not match schema exactly",
    );
}

#[test]
fn rejects_evidence_missing_a_measurement() {
    let report = sealed(base_report());
    let mut encoded = serde_json::to_value(&report).expect("report must encode");
    encoded
        .get_mut("continuity")
        .and_then(serde_json::Value::as_object_mut)
        .expect("continuity object")
        .remove("restarts");
    let bytes = serde_json::to_vec(&encoded).expect("encode");
    assert_rejected(
        qualify_bytes(&bytes, &policy(), NOW),
        "does not match schema exactly",
    );
}

#[test]
fn rejects_an_unknown_schema_version() {
    assert_rejected(
        qualify_mutated(|report| report.schema.version = EVIDENCE_SCHEMA_VERSION + 1),
        "schema version",
    );
}

#[test]
fn rejects_an_unknown_schema_id() {
    assert_rejected(
        qualify_mutated(|report| report.schema.id = "grokptah.some_other_report".into()),
        "schema id",
    );
}

// --- exact candidate and parent identity ------------------------------------

#[test]
fn rejects_a_report_for_a_different_candidate_head() {
    assert_rejected(
        qualify_mutated(|report| {
            report.identity.candidate_head = "1111111111111111111111111111111111111111".into();
        }),
        "candidate head is",
    );
}

#[test]
fn rejects_a_report_for_a_different_parent_head() {
    assert_rejected(
        qualify_mutated(|report| {
            report.identity.parent_head = "2222222222222222222222222222222222222222".into();
        }),
        "parent head is",
    );
}

#[test]
fn rejects_an_abbreviated_commit_identity() {
    assert_rejected(
        qualify_mutated(|report| report.identity.candidate_head = "8ad3be0".into()),
        "not a full lowercase hex commit",
    );
}

#[test]
fn rejects_a_candidate_that_is_its_own_parent() {
    assert_rejected(
        qualify_mutated(|report| report.identity.parent_head = CANDIDATE_HEAD.into()),
        "same commit",
    );
}

// --- freshness --------------------------------------------------------------

#[test]
fn rejects_stale_evidence() {
    assert_rejected(
        qualify_mutated(|report| report.generated_at_unix_seconds = NOW - 7_200),
        "over the 3600s ceiling",
    );
}

#[test]
fn rejects_future_dated_evidence() {
    assert_rejected(
        qualify_mutated(|report| report.generated_at_unix_seconds = NOW + 60),
        "after verification time",
    );
}

// --- evidence integrity -----------------------------------------------------

#[test]
fn rejects_a_tampered_evidence_digest() {
    let mut report = sealed(base_report());
    report.audit.records_retained = 9_999;
    assert_rejected(qualify(&report), "does not match the recomputed");
}

#[test]
fn rejects_a_malformed_evidence_digest() {
    let mut report = sealed(base_report());
    report.evidence_digest_sha256 = "NOT-A-DIGEST".into();
    assert_rejected(qualify(&report), "not a lowercase hex SHA-256");
}

#[test]
fn rejects_a_report_that_asserts_its_own_qualification() {
    assert_rejected(
        qualify_mutated(|report| report.claim_state = ClaimState::Qualified),
        "only verification may qualify a candidate",
    );
}

#[test]
fn rejects_secret_bearing_evidence() {
    assert_rejected(
        qualify_mutated(|report| {
            report.checks[2].observed_detail = "rotated the worker api_key in place".into();
        }),
        "forbidden secret marker",
    );
}

#[test]
fn rejects_secret_bearing_evidence_even_when_it_cannot_be_parsed() {
    let bytes = br#"{"truncated": true, "authorization": "Bearer abc"#;
    let rejection = qualify_bytes(bytes, &policy(), NOW).expect_err("must be rejected");
    let rendered = rejection.to_string();
    assert!(
        rendered.contains("does not match schema exactly"),
        "{rendered}"
    );
    assert!(rendered.contains("forbidden secret marker"), "{rendered}");
}

// --- soak exit and measured duration ----------------------------------------

#[test]
fn rejects_a_soak_without_its_terminal_exit_marker() {
    assert_rejected(
        qualify_mutated(|report| report.soak.exit_marker = "GROKPTAH_SOAK_INTERRUPTED".into()),
        "exit marker is",
    );
}

#[test]
fn rejects_a_soak_that_still_owns_processes() {
    assert_rejected(
        qualify_mutated(|report| report.soak.owned_processes = 1),
        "processes still owned at exit",
    );
}

#[test]
fn rejects_a_soak_that_still_owns_open_handles() {
    assert_rejected(
        qualify_mutated(|report| report.soak.owned_open_handles = 4),
        "open handles still owned at exit",
    );
}

#[test]
fn rejects_a_declared_rather_than_measured_duration() {
    assert_rejected(
        qualify_mutated(|report| report.soak.duration_source = DurationSource::Declared),
        "not measured",
    );
}

#[test]
fn rejects_a_soak_configured_below_the_required_duration() {
    assert_rejected(
        qualify_mutated(|report| {
            report.soak.configured_seconds = 3_600;
            report.soak.measured_seconds = 3_601;
        }),
        "under the required 86400s",
    );
}

#[test]
fn rejects_a_soak_that_stopped_short_of_its_configured_duration() {
    assert_rejected(
        qualify_mutated(|report| report.soak.measured_seconds = 40_000),
        "short of the configured",
    );
}

// --- distinct workers and credential bindings -------------------------------

#[test]
fn rejects_workers_sharing_one_credential_binding() {
    assert_rejected(
        qualify_mutated(|report| {
            report.workers[1].credential_binding_id = "binding-alpha".into();
        }),
        "shared by more than one worker",
    );
}

#[test]
fn rejects_a_repeated_worker_identity() {
    assert_rejected(
        qualify_mutated(|report| report.workers[2].worker_id = "worker-alpha".into()),
        "appears more than once",
    );
}

#[test]
fn rejects_fewer_workers_than_the_policy_requires() {
    assert_rejected(
        qualify_mutated(|report| {
            report.workers.truncate(2);
            report.credentials.issued = 2;
        }),
        "under the required 3",
    );
}

#[test]
fn rejects_a_worker_that_executed_nothing() {
    assert_rejected(
        qualify_mutated(|report| report.workers[0].executions = 0),
        "executed nothing",
    );
}

// --- credential issuance, least privilege, and rotation ---------------------

#[test]
fn rejects_credential_counts_that_do_not_match_the_certified_workers() {
    assert_rejected(
        qualify_mutated(|report| report.credentials.issued = 2),
        "credentials issued for 3 certified workers",
    );
}

#[test]
fn rejects_a_soak_with_no_credential_rotation() {
    assert_rejected(
        qualify_mutated(|report| {
            report.credentials.rotations = 0;
            report.credentials.old_credential_rejections = 0;
            report.credentials.new_credential_acceptances = 0;
        }),
        "no credential rotation occurred",
    );
}

#[test]
fn rejects_a_rotation_whose_old_credential_was_not_rejected() {
    assert_rejected(
        qualify_mutated(|report| report.credentials.old_credential_rejections = 1),
        "rotated-out credentials were rejected",
    );
}

#[test]
fn rejects_a_rotation_whose_new_credential_was_not_accepted() {
    assert_rejected(
        qualify_mutated(|report| report.credentials.new_credential_acceptances = 0),
        "rotated-in credentials were accepted",
    );
}

#[test]
fn rejects_a_scope_outside_the_least_privilege_allowlist() {
    assert_rejected(
        qualify_mutated(|report| {
            report
                .credentials
                .least_privilege_scopes
                .push("host:admin".into());
        }),
        "outside the least-privilege allowlist",
    );
}

#[test]
fn rejects_a_privileged_scope_request() {
    assert_rejected(
        qualify_mutated(|report| report.credentials.privileged_scopes_requested = 1),
        "privileged scope grants were requested",
    );
}

// --- restarts, duplicates, and audit ----------------------------------------

#[test]
fn rejects_fewer_restarts_than_the_policy_requires() {
    assert_rejected(
        qualify_mutated(|report| report.continuity.restarts = 1),
        "restarts, under the required 2",
    );
}

#[test]
fn rejects_an_uncertain_resume() {
    assert_rejected(
        qualify_mutated(|report| report.continuity.uncertain_resumes = 1),
        "uncertain state",
    );
}

#[test]
fn rejects_a_leaked_worker() {
    assert_rejected(
        qualify_mutated(|report| report.continuity.leaked_workers = 1),
        "leaked across restarts",
    );
}

#[test]
fn rejects_any_duplicate_execution() {
    assert_rejected(
        qualify_mutated(|report| report.workers[1].duplicate_executions = 1),
        "duplicate executions",
    );
}

#[test]
fn rejects_dropped_audit_records() {
    assert_rejected(
        qualify_mutated(|report| report.audit.records_dropped = 3),
        "audit records were dropped",
    );
}

#[test]
fn rejects_audit_that_did_not_survive_a_restart() {
    assert_rejected(
        qualify_mutated(|report| report.audit.retained_across_restarts = false),
        "did not survive every restart",
    );
}

#[test]
fn rejects_fewer_retained_audit_records_than_the_policy_requires() {
    assert_rejected(
        qualify_mutated(|report| report.audit.records_retained = 10),
        "under the required 1000",
    );
}

// --- the policy cannot weaken the gate --------------------------------------

#[test]
fn a_relaxed_policy_cannot_lower_the_hard_worker_and_restart_floors() {
    let mut relaxed = policy();
    relaxed.minimum_workers = 0;
    relaxed.minimum_restarts = 0;
    let mut report = base_report();
    report.workers.truncate(1);
    report.credentials.issued = 1;
    report.continuity.restarts = 0;

    let rejection = qualify_with(&sealed(report), &relaxed)
        .expect_err("hard floors must survive a relaxed policy");
    let rendered = rejection.to_string();
    assert!(
        rendered.contains(&format!("under the required {MINIMUM_CERTIFIED_WORKERS}")),
        "worker floor must hold: {rendered}"
    );
    assert!(
        rendered.contains(&format!("restarts, under the required {MINIMUM_RESTARTS}")),
        "restart floor must hold: {rendered}"
    );
}

#[test]
fn rejects_a_policy_that_does_not_pin_a_full_candidate_commit() {
    let mut vague = policy();
    vague.expected_candidate_head = "8ad3be0".into();
    assert_rejected(
        qualify_with(&sealed(base_report()), &vague),
        "expected candidate head",
    );
}

#[test]
fn rejects_a_policy_whose_candidate_is_its_own_parent() {
    let mut degenerate = policy();
    degenerate.expected_parent_head = CANDIDATE_HEAD.into();
    assert_rejected(
        qualify_with(&sealed(base_report()), &degenerate),
        "expected candidate head and parent head are the same commit",
    );
}

// --- the seven ordered checks -----------------------------------------------

#[test]
fn rejects_reordered_checks() {
    assert_rejected(
        qualify_mutated(|report| report.checks.swap(1, 2)),
        "position 1 declares",
    );
}

#[test]
fn rejects_a_missing_check() {
    assert_rejected(
        qualify_mutated(|report| {
            report.checks.remove(4);
        }),
        "report declares 6 checks, expected exactly 7",
    );
}

#[test]
fn rejects_an_extra_check() {
    assert_rejected(
        qualify_mutated(|report| {
            report.checks.push(check("operator_sign_off", "looks good"));
        }),
        "unexpected extra check",
    );
}

#[test]
fn rejects_a_check_without_observed_detail() {
    assert_rejected(
        qualify_mutated(|report| report.checks[0].observed_detail = "   ".into()),
        "records no observed detail",
    );
}

#[test]
fn rejects_a_check_the_writer_recorded_as_failing() {
    assert_rejected(
        qualify_mutated(|report| report.checks[5].passed = false),
        "writer recorded a failure",
    );
}

#[test]
fn a_declared_pass_never_substitutes_for_the_measurement() {
    let rejection = qualify_mutated(|report| {
        // The writer still claims restart recovery passed.
        report.continuity.restarts = 0;
    })
    .expect_err("an unsupported declaration must not qualify");
    let rendered = rejection.to_string();
    assert!(
        rendered.contains("measurement:restart_recovery"),
        "the measurement must be reported: {rendered}"
    );
    assert!(
        rendered.contains("declared passing but the measurements do not support it"),
        "the unsupported declaration must be reported: {rendered}"
    );
}

#[test]
fn every_required_check_is_evaluated_against_a_measurement() {
    // Each check identifier must be reachable as an independently measured
    // outcome, so no check can pass on its declaration alone.
    for id in REQUIRED_CHECK_ORDER {
        let rejection = qualify_mutated(|report| match id {
            "soak_exit_marker" => report.soak.owned_processes = 1,
            "worker_isolation" => report.workers[0].worker_id = String::new(),
            "credential_lifecycle" => report.credentials.rotations = 0,
            "restart_recovery" => report.continuity.leaked_workers = 1,
            "duplicate_suppression" => report.workers[0].duplicate_executions = 1,
            "audit_retention" => report.audit.records_dropped = 1,
            "evidence_integrity" => report.claim_state = ClaimState::Qualified,
            other => panic!("unmapped check {other}"),
        })
        .err()
        .unwrap_or_else(|| panic!("{id} must be independently measured"));
        let rendered = rejection.to_string();
        assert!(
            rendered.contains(&format!("measurement:{id}")),
            "{id} must fail on its own measurement: {rendered}"
        );
        assert!(
            rendered.contains(&format!("check:{id}")),
            "{id} must report an unsupported declaration: {rendered}"
        );
    }
}

// --- release identity binding -----------------------------------------------

fn artifact(name: &str, bytes: u64, sha256: &str) -> ReleaseArtifact {
    ReleaseArtifact {
        name: name.into(),
        bytes,
        sha256: sha256.into(),
    }
}

const ARTIFACT_DIGEST_A: &str = "aaaaaaaabbbbbbbbccccccccddddddddeeeeeeeeffffffff0000000011111111";
const ARTIFACT_DIGEST_B: &str = "1111111100000000ffffffffeeeeeeeeddddddddccccccccbbbbbbbbaaaaaaaa";

#[test]
fn a_release_record_binds_the_exact_qualified_head_and_ordered_artifacts() {
    let candidate = qualify(&sealed(base_report())).expect("valid evidence must qualify");
    let release = candidate
        .bind_release(vec![
            artifact("grokptah-client-0.1.0.tgz", 41_216, ARTIFACT_DIGEST_B),
            artifact("GrokPtah_0.1.0_aarch64.dmg", 7_651_328, ARTIFACT_DIGEST_A),
        ])
        .expect("well-formed artifacts must bind");

    assert_eq!(release.candidate_head(), CANDIDATE_HEAD);
    assert_eq!(release.parent_head(), PARENT_HEAD);
    assert_eq!(
        release.evidence_digest_sha256(),
        candidate.evidence_digest_sha256()
    );
    assert_eq!(release.qualified_at_unix_seconds(), NOW);
    let names: Vec<&str> = release
        .artifacts()
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(
        names,
        ["GrokPtah_0.1.0_aarch64.dmg", "grokptah-client-0.1.0.tgz"],
        "artifact metadata must be canonically ordered"
    );
    assert_eq!(release.release_digest_sha256().len(), 64);
}

#[test]
fn an_identical_binding_reproduces_the_same_release_digest() {
    let candidate = qualify(&sealed(base_report())).expect("valid evidence must qualify");
    let first = candidate
        .bind_release(vec![artifact(
            "GrokPtah_0.1.0_aarch64.dmg",
            7_651_328,
            ARTIFACT_DIGEST_A,
        )])
        .expect("bind");
    let second = candidate
        .bind_release(vec![artifact(
            "GrokPtah_0.1.0_aarch64.dmg",
            7_651_328,
            ARTIFACT_DIGEST_A,
        )])
        .expect("bind");
    assert_eq!(
        first.release_digest_sha256(),
        second.release_digest_sha256()
    );
}

#[test]
fn a_changed_artifact_digest_changes_the_release_digest() {
    let candidate = qualify(&sealed(base_report())).expect("valid evidence must qualify");
    let first = candidate
        .bind_release(vec![artifact("app.dmg", 7_651_328, ARTIFACT_DIGEST_A)])
        .expect("bind");
    let second = candidate
        .bind_release(vec![artifact("app.dmg", 7_651_328, ARTIFACT_DIGEST_B)])
        .expect("bind");
    assert_ne!(
        first.release_digest_sha256(),
        second.release_digest_sha256()
    );
}

#[test]
fn rejects_a_release_with_no_artifacts() {
    let candidate = qualify(&sealed(base_report())).expect("valid evidence must qualify");
    assert_rejected(candidate.bind_release(Vec::new()), "at least one artifact");
}

#[test]
fn rejects_an_artifact_name_carrying_a_path() {
    let candidate = qualify(&sealed(base_report())).expect("valid evidence must qualify");
    assert_rejected(
        candidate.bind_release(vec![artifact("../outside/app.dmg", 10, ARTIFACT_DIGEST_A)]),
        "not a plain file name",
    );
}

#[test]
fn rejects_an_artifact_without_a_well_formed_digest() {
    let candidate = qualify(&sealed(base_report())).expect("valid evidence must qualify");
    assert_rejected(
        candidate.bind_release(vec![artifact("app.dmg", 10, "deadbeef")]),
        "not a lowercase hex SHA-256",
    );
}

#[test]
fn rejects_an_empty_artifact() {
    let candidate = qualify(&sealed(base_report())).expect("valid evidence must qualify");
    assert_rejected(
        candidate.bind_release(vec![artifact("app.dmg", 0, ARTIFACT_DIGEST_A)]),
        "declares zero bytes",
    );
}

#[test]
fn rejects_a_repeated_artifact_name() {
    let candidate = qualify(&sealed(base_report())).expect("valid evidence must qualify");
    assert_rejected(
        candidate.bind_release(vec![
            artifact("app.dmg", 10, ARTIFACT_DIGEST_A),
            artifact("app.dmg", 20, ARTIFACT_DIGEST_B),
        ]),
        "bound more than once",
    );
}
