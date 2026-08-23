//! Exact-head Stage 3 least-privilege authority certification.
//!
//! This campaign runs the closed role registry, credential narrowing,
//! Computer Use separation, public MCP filtering, worker-binding, and hosted
//! service authorization gates against one clean candidate SHA. It retains
//! only bounded structural digests and seals nothing unless every ordered gate
//! passes with its exact test cardinality.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::artifact::{verify_completed_campaign, SafeOutputRoot};

pub const AUTHORITY_STAGE3_REPORT_SCHEMA: &str = "grokptah.authority-stage3-campaign.v1";
pub const AUTHORITY_STAGE3_OUTPUT_RELATIVE_PATH: &str = "evals/runs/authority-stage3-cert";
pub const AUTHORITY_STAGE3_REPORT_PATH: &str = "report.json";
const MAX_REPORT_BYTES: usize = 256 * 1024;
const MAX_GATE_OUTPUT_BYTES: u64 = 8 * 1024 * 1024;
const GATE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

const REQUIRED_ROLES: &[&str] = &[
    "local_operator",
    "remote_operator",
    "remote_coordinator",
    "observer",
];
const REMOTE_COORDINATOR_DENIALS: &[&str] = &[
    "computer_use",
    "managed.authorize",
    "managed.configure",
    "runs.approve",
    "runs.promote",
    "work.admin",
    "work.approve",
];
const OBSERVER_DENIALS: &[&str] = &[
    "agents.resume",
    "manager.control",
    "queue.control",
    "routines.control",
    "runs.cancel",
    "runs.submit",
    "work.create",
    "workers.control",
];

#[derive(Debug, Clone)]
pub struct AuthorityStage3Options {
    pub repository_root: PathBuf,
    pub output_root: PathBuf,
    pub artifact_budget_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorityStage3Contract {
    pub roles: Vec<String>,
    pub bearer_cannot_mint_local_operator: bool,
    pub remote_coordinator_denials: Vec<String>,
    pub observer_denials: Vec<String>,
    pub authority_bound_idempotency: bool,
    pub bound_worker_identity: bool,
    pub computer_read_session_workspace_bound: bool,
    pub public_capability_document_matches_enforcement: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorityStage3GateEvidence {
    pub gate_id: String,
    pub command_sha256: String,
    pub stdout_sha256: String,
    pub stderr_sha256: String,
    pub exit_code: i32,
    pub expected_passed_tests: u32,
    pub observed_passed_tests: u32,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorityStage3Evidence {
    pub schema: String,
    pub campaign_id: String,
    pub candidate_sha: String,
    pub repository_clean: bool,
    pub contract: AuthorityStage3Contract,
    pub gates: Vec<AuthorityStage3GateEvidence>,
    pub all_required_gates_passed: bool,
    pub secret_free: bool,
    pub claim_eligible: bool,
    pub evidence_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorityStage3Completion {
    pub campaign_id: String,
    pub candidate_sha: String,
    pub certification_ready: bool,
    pub report_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorityStage3InspectSummary {
    pub valid: bool,
    pub completion_seal_verified: bool,
    pub certification_ready: bool,
    pub campaign_id: String,
    pub candidate_sha: String,
    pub gates_passed: u32,
    pub gates_required: u32,
}

#[derive(Debug, Clone, Copy)]
struct GateSpec {
    id: &'static str,
    args: &'static [&'static str],
    expected_passed_tests: u32,
}

const GATES: &[GateSpec] = &[
    GateSpec {
        id: "authority-role-registry-v1",
        args: &[
            "test",
            "--locked",
            "--manifest-path",
            "crates/codegen/grokptah-agent-bridge/Cargo.toml",
            "--lib",
            "orchestration::authority::tests::",
            "--",
            "--test-threads=1",
        ],
        expected_passed_tests: 7,
    },
    GateSpec {
        id: "authority-credential-scope-v1",
        args: &[
            "test",
            "--locked",
            "--manifest-path",
            "crates/codegen/grokptah-agent-bridge/Cargo.toml",
            "--lib",
            "orchestration::authz::tests::",
            "--",
            "--test-threads=1",
        ],
        expected_passed_tests: 10,
    },
    GateSpec {
        id: "authority-computer-read-surface-v1",
        args: &[
            "test",
            "--locked",
            "--manifest-path",
            "crates/codegen/grokptah-agent-bridge/Cargo.toml",
            "--test",
            "computer_use_release_gate",
            "mcp_surface_exposes_only_the_scoped_computer_read_tools",
            "--",
            "--exact",
            "--test-threads=1",
        ],
        expected_passed_tests: 1,
    },
    GateSpec {
        id: "authority-computer-read-scope-v1",
        args: &[
            "test",
            "--locked",
            "--manifest-path",
            "crates/codegen/grokptah-agent-bridge/Cargo.toml",
            "--lib",
            "mcp_control::tests::computer_read_tools_are_scoped_and_fail_indistinguishably",
            "--",
            "--exact",
            "--test-threads=1",
        ],
        expected_passed_tests: 1,
    },
    GateSpec {
        id: "authority-public-mcp-role-filter-v1",
        args: &[
            "test",
            "--locked",
            "--manifest-path",
            "crates/codegen/grokptah-agent-bridge/Cargo.toml",
            "--lib",
            "mcp_control::tests::bearer_capabilities_are_role_filtered_and_session_bound",
            "--",
            "--exact",
            "--test-threads=1",
        ],
        expected_passed_tests: 1,
    },
    GateSpec {
        id: "authority-bound-worker-mcp-v1",
        args: &[
            "test",
            "--locked",
            "--manifest-path",
            "crates/codegen/grokptah-agent-bridge/Cargo.toml",
            "--test",
            "coordinator_mcp",
            "independent_worker_recovers_assignment_and_messages",
            "--",
            "--exact",
            "--test-threads=1",
        ],
        expected_passed_tests: 1,
    },
    GateSpec {
        id: "authority-service-scope-loopback-v1",
        args: &[
            "test",
            "--locked",
            "--manifest-path",
            "crates/codegen/grokptah-service/Cargo.toml",
            "--test",
            "service_conformance",
            "authorization_is_fail_closed_across_token_session_and_workspace",
            "--",
            "--exact",
            "--test-threads=1",
        ],
        expected_passed_tests: 1,
    },
];

impl AuthorityStage3Evidence {
    pub fn validate(&self) -> Result<()> {
        if self.schema != AUTHORITY_STAGE3_REPORT_SCHEMA {
            bail!("unsupported Stage 3 authority evidence schema");
        }
        if !valid_id(&self.campaign_id) || !lower_sha(&self.candidate_sha, 40) {
            bail!("invalid Stage 3 campaign identity");
        }
        if !self.repository_clean || !self.secret_free || self.contract != expected_contract() {
            bail!("Stage 3 evidence does not bind the exact authority contract");
        }
        if self.gates.len() != GATES.len() {
            bail!("Stage 3 report omits or adds a required gate");
        }
        for (gate, spec) in self.gates.iter().zip(GATES) {
            if gate.gate_id != spec.id
                || gate.command_sha256 != command_digest(spec)
                || !lower_sha(&gate.stdout_sha256, 64)
                || !lower_sha(&gate.stderr_sha256, 64)
                || gate.exit_code != 0
                || gate.expected_passed_tests != spec.expected_passed_tests
                || gate.observed_passed_tests != spec.expected_passed_tests
                || !gate.passed
            {
                bail!("Stage 3 gate evidence is missing, reordered, or invalid");
            }
        }
        if !self.all_required_gates_passed
            || !self.claim_eligible
            || self.evidence_digest != expected_evidence_digest(self)
        {
            bail!("Stage 3 evidence is incomplete or digest-invalid");
        }
        Ok(())
    }

    pub fn certification_ready(&self) -> bool {
        self.claim_eligible && self.validate().is_ok()
    }
}

pub fn expected_evidence_digest(evidence: &AuthorityStage3Evidence) -> String {
    let mut unsigned = evidence.clone();
    unsigned.evidence_digest.clear();
    digest(&serde_json::to_vec(&unsigned).expect("Stage 3 evidence serialization"))
}

pub fn run(options: &AuthorityStage3Options) -> Result<AuthorityStage3Completion> {
    let repository = canonical_clean_repository(&options.repository_root)?;
    let candidate_sha = git_output(&repository, &["rev-parse", "HEAD"])?;
    if !lower_sha(&candidate_sha, 40) {
        bail!("candidate HEAD is not a lowercase full SHA");
    }
    let output = SafeOutputRoot::open(
        &options.output_root,
        &repository,
        None,
        options.artifact_budget_bytes,
    )?;
    let campaign_id = format!(
        "authority-stage3-{}-{}",
        &candidate_sha[..16],
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let artifacts = output.create_campaign(&campaign_id)?;
    let scratch = tempfile::tempdir().context("creating Stage 3 scratch directory")?;
    let mut gates = Vec::with_capacity(GATES.len());

    for spec in GATES {
        let gate = run_gate(spec, &repository, scratch.path())?;
        let checkpoint = serde_json::to_vec_pretty(&gate)?;
        artifacts.write_partial(format!("gates/{}.json", spec.id), &checkpoint)?;
        if !gate.passed {
            bail!("Stage 3 gate failed");
        }
        gates.push(gate);
    }

    let final_repository = canonical_clean_repository(&repository)?;
    if final_repository != repository
        || git_output(&repository, &["rev-parse", "HEAD"])? != candidate_sha
    {
        bail!("candidate repository changed during Stage 3 certification");
    }
    let mut evidence = AuthorityStage3Evidence {
        schema: AUTHORITY_STAGE3_REPORT_SCHEMA.to_owned(),
        campaign_id: campaign_id.clone(),
        candidate_sha: candidate_sha.clone(),
        repository_clean: true,
        contract: expected_contract(),
        gates,
        all_required_gates_passed: true,
        secret_free: true,
        claim_eligible: true,
        evidence_digest: String::new(),
    };
    evidence.evidence_digest = expected_evidence_digest(&evidence);
    evidence.validate()?;
    let bytes = serde_json::to_vec_pretty(&evidence)?;
    if bytes.len() > MAX_REPORT_BYTES {
        bail!("Stage 3 report exceeds its bound");
    }
    let report = artifacts.write_final(AUTHORITY_STAGE3_REPORT_PATH, &bytes)?;
    artifacts.mark_complete(&report)?;
    Ok(AuthorityStage3Completion {
        campaign_id,
        candidate_sha,
        certification_ready: evidence.certification_ready(),
        report_sha256: report.sha256,
    })
}

pub fn inspect(campaign: &Path) -> Result<AuthorityStage3InspectSummary> {
    let sealed = verify_completed_campaign(campaign)?;
    if sealed.relative_path != AUTHORITY_STAGE3_REPORT_PATH
        || sealed.bytes > MAX_REPORT_BYTES as u64
    {
        bail!("completion seal does not reference a bounded Stage 3 report");
    }
    let bytes = bounded_regular_read(
        &campaign.join(AUTHORITY_STAGE3_REPORT_PATH),
        MAX_REPORT_BYTES as u64,
    )?;
    if bytes.len() as u64 != sealed.bytes || digest(&bytes) != sealed.sha256 {
        bail!("sealed Stage 3 report does not match its completion record");
    }
    let evidence: AuthorityStage3Evidence =
        serde_json::from_slice(&bytes).context("invalid Stage 3 report")?;
    evidence.validate()?;
    Ok(AuthorityStage3InspectSummary {
        valid: true,
        completion_seal_verified: true,
        certification_ready: evidence.certification_ready(),
        campaign_id: evidence.campaign_id,
        candidate_sha: evidence.candidate_sha,
        gates_passed: evidence.gates.iter().filter(|gate| gate.passed).count() as u32,
        gates_required: GATES.len() as u32,
    })
}

fn expected_contract() -> AuthorityStage3Contract {
    AuthorityStage3Contract {
        roles: strings(REQUIRED_ROLES),
        bearer_cannot_mint_local_operator: true,
        remote_coordinator_denials: strings(REMOTE_COORDINATOR_DENIALS),
        observer_denials: strings(OBSERVER_DENIALS),
        authority_bound_idempotency: true,
        bound_worker_identity: true,
        computer_read_session_workspace_bound: true,
        public_capability_document_matches_enforcement: true,
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn run_gate(
    spec: &GateSpec,
    repository: &Path,
    scratch: &Path,
) -> Result<AuthorityStage3GateEvidence> {
    let stdout_path = scratch.join(format!("{}.stdout", spec.id));
    let stderr_path = scratch.join(format!("{}.stderr", spec.id));
    let stdout = File::create(&stdout_path)?;
    let stderr = File::create(&stderr_path)?;
    let mut child = Command::new("cargo")
        .args(spec.args)
        .current_dir(repository)
        .env("RUST_TEST_THREADS", "1")
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .context("launching Stage 3 gate")?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if file_len(&stdout_path)? > MAX_GATE_OUTPUT_BYTES
            || file_len(&stderr_path)? > MAX_GATE_OUTPUT_BYTES
        {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Stage 3 gate output exceeded its bound");
        }
        if started.elapsed() >= GATE_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Stage 3 gate exceeded its duration bound");
        }
        thread::sleep(Duration::from_millis(100));
    };
    let stdout = bounded_regular_read(&stdout_path, MAX_GATE_OUTPUT_BYTES)?;
    let stderr = bounded_regular_read(&stderr_path, MAX_GATE_OUTPUT_BYTES)?;
    let observed = observed_passed_tests(&stdout, &stderr, spec.expected_passed_tests);
    let exit_code = status_code(status)?;
    Ok(AuthorityStage3GateEvidence {
        gate_id: spec.id.to_owned(),
        command_sha256: command_digest(spec),
        stdout_sha256: digest(&stdout),
        stderr_sha256: digest(&stderr),
        exit_code,
        expected_passed_tests: spec.expected_passed_tests,
        observed_passed_tests: observed,
        passed: exit_code == 0 && observed == spec.expected_passed_tests,
    })
}

fn observed_passed_tests(stdout: &[u8], stderr: &[u8], expected: u32) -> u32 {
    let marker = format!("test result: ok. {expected} passed; 0 failed;");
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    if stdout.contains(&marker) || stderr.contains(&marker) {
        expected
    } else {
        0
    }
}

fn canonical_clean_repository(path: &Path) -> Result<PathBuf> {
    let repository = dunce::canonicalize(path).context("canonicalizing repository")?;
    if !repository.is_dir() || !repository.join(".git").exists() {
        bail!("Stage 3 repository is not a Git checkout");
    }
    if !git_output(
        &repository,
        &["status", "--porcelain", "--untracked-files=normal"],
    )?
    .is_empty()
    {
        bail!("Stage 3 certification requires a clean repository");
    }
    Ok(repository)
}

fn git_output(repository: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repository)
        .output()
        .context("running bounded Git inspection")?;
    if !output.status.success() || output.stdout.len() > 64 * 1024 || !output.stderr.is_empty() {
        bail!("Git inspection failed closed");
    }
    Ok(String::from_utf8(output.stdout)
        .context("Git inspection was not UTF-8")?
        .trim()
        .to_owned())
}

fn command_digest(spec: &GateSpec) -> String {
    digest(
        &serde_json::to_vec(&serde_json::json!({
            "program": "cargo",
            "args": spec.args,
            "expectedPassedTests": spec.expected_passed_tests,
        }))
        .expect("gate command serialization"),
    )
}

fn status_code(status: ExitStatus) -> Result<i32> {
    status
        .code()
        .ok_or_else(|| anyhow::anyhow!("Stage 3 gate ended without an exit code"))
}

fn file_len(path: &Path) -> Result<u64> {
    Ok(fs::symlink_metadata(path)?.len())
}

fn bounded_regular_read(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > maximum {
        bail!("Stage 3 artifact is not a bounded regular file");
    }
    let bytes = fs::read(path)?;
    if bytes.len() as u64 != metadata.len() {
        bail!("Stage 3 artifact changed during read");
    }
    Ok(bytes)
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn lower_sha(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> AuthorityStage3Evidence {
        let gates = GATES
            .iter()
            .map(|spec| AuthorityStage3GateEvidence {
                gate_id: spec.id.into(),
                command_sha256: command_digest(spec),
                stdout_sha256: "d".repeat(64),
                stderr_sha256: "e".repeat(64),
                exit_code: 0,
                expected_passed_tests: spec.expected_passed_tests,
                observed_passed_tests: spec.expected_passed_tests,
                passed: true,
            })
            .collect();
        let mut evidence = AuthorityStage3Evidence {
            schema: AUTHORITY_STAGE3_REPORT_SCHEMA.into(),
            campaign_id: "authority-stage3-campaign".into(),
            candidate_sha: "c".repeat(40),
            repository_clean: true,
            contract: expected_contract(),
            gates,
            all_required_gates_passed: true,
            secret_free: true,
            claim_eligible: true,
            evidence_digest: String::new(),
        };
        evidence.evidence_digest = expected_evidence_digest(&evidence);
        evidence
    }

    #[test]
    fn complete_exact_gate_set_is_certification_ready() {
        let evidence = ready();
        evidence.validate().unwrap();
        assert!(evidence.certification_ready());
        let encoded = serde_json::to_string(&evidence).unwrap();
        assert!(!encoded.contains("api_key"));
        assert!(!encoded.contains("bearer_token"));
        assert!(!encoded.contains("/Users/"));
    }

    #[test]
    fn missing_reordered_failed_or_tampered_gate_fails_closed() {
        let mut missing = ready();
        missing.gates.pop();
        missing.evidence_digest = expected_evidence_digest(&missing);
        assert!(missing.validate().is_err());
        let mut reordered = ready();
        reordered.gates.swap(0, 1);
        reordered.evidence_digest = expected_evidence_digest(&reordered);
        assert!(reordered.validate().is_err());
        let mut failed = ready();
        failed.gates[0].passed = false;
        failed.gates[0].exit_code = 1;
        failed.evidence_digest = expected_evidence_digest(&failed);
        assert!(failed.validate().is_err());
        let mut tampered = ready();
        tampered.gates[0].stdout_sha256 = "f".repeat(64);
        assert!(tampered.validate().is_err());
    }

    #[test]
    fn contract_drift_unknown_fields_and_false_claims_fail_closed() {
        let mut drifted = ready();
        drifted.contract.roles.swap(0, 1);
        drifted.evidence_digest = expected_evidence_digest(&drifted);
        assert!(drifted.validate().is_err());
        let mut false_claim = ready();
        false_claim.all_required_gates_passed = false;
        false_claim.evidence_digest = expected_evidence_digest(&false_claim);
        assert!(false_claim.validate().is_err());
        let mut value = serde_json::to_value(ready()).unwrap();
        value["contract"]["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<AuthorityStage3Evidence>(value).is_err());
    }

    #[test]
    fn gate_output_parser_requires_the_exact_green_cardinality() {
        assert_eq!(
            observed_passed_tests(b"test result: ok. 5 passed; 0 failed;", b"", 5),
            5
        );
        assert_eq!(
            observed_passed_tests(b"test result: ok. 4 passed; 0 failed;", b"", 5),
            0
        );
        assert_eq!(
            observed_passed_tests(b"test result: FAILED. 5 passed; 1 failed;", b"", 5),
            0
        );
    }

    #[test]
    fn sealed_report_round_trips_through_the_independent_inspector() {
        let repository = tempfile::tempdir().unwrap();
        let repository = dunce::canonicalize(repository.path()).unwrap();
        let output = repository.join("runs");
        let root = SafeOutputRoot::open(&output, &repository, None, 1024 * 1024).unwrap();
        let artifacts = root.create_campaign("authority-stage3-inspect").unwrap();
        let mut evidence = ready();
        evidence.campaign_id = "authority-stage3-inspect".into();
        evidence.evidence_digest = expected_evidence_digest(&evidence);
        let bytes = serde_json::to_vec_pretty(&evidence).unwrap();
        let report = artifacts
            .write_final(AUTHORITY_STAGE3_REPORT_PATH, &bytes)
            .unwrap();
        artifacts.mark_complete(&report).unwrap();
        let inspected = inspect(artifacts.path()).unwrap();
        assert!(inspected.certification_ready);
        assert_eq!(inspected.gates_passed, GATES.len() as u32);
        assert_eq!(inspected.candidate_sha, evidence.candidate_sha);
    }
}
