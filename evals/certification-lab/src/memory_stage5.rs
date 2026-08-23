//! Exact-head Stage 5 durable-memory certification.
//!
//! This campaign is deliberately separate from elapsed soak evidence. It runs
//! the production logical-years, crash/restart, scope, and Manager occurrence
//! gates, retains only bounded structural results, and seals a claim only when
//! every exact command passes on one clean candidate SHA.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use grokptah_agent_bridge::{
    expected_memory_evidence_digest, MemoryLongHorizonEvidence, MEMORY_LONG_HORIZON_EVIDENCE_SCHEMA,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::artifact::{verify_completed_campaign, SafeOutputRoot};

pub const MEMORY_STAGE5_REPORT_SCHEMA: &str = "grokptah.memory-stage5-campaign.v1";
pub const MEMORY_STAGE5_OUTPUT_RELATIVE_PATH: &str = "evals/runs/memory-stage5-cert";
pub const MEMORY_STAGE5_REPORT_PATH: &str = "report.json";
const MAX_REPORT_BYTES: usize = 256 * 1024;
const MAX_GATE_OUTPUT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_LOGICAL_EVIDENCE_BYTES: u64 = 64 * 1024;
const GATE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone)]
pub struct MemoryStage5Options {
    pub repository_root: PathBuf,
    pub output_root: PathBuf,
    pub artifact_budget_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryStage5GateEvidence {
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
pub struct MemoryStage5Evidence {
    pub schema: String,
    pub campaign_id: String,
    pub candidate_sha: String,
    pub repository_clean: bool,
    pub logical_years: MemoryLongHorizonEvidence,
    pub gates: Vec<MemoryStage5GateEvidence>,
    pub all_required_gates_passed: bool,
    pub secret_free: bool,
    pub claim_eligible: bool,
    pub evidence_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryStage5Completion {
    pub campaign_id: String,
    pub candidate_sha: String,
    pub certification_ready: bool,
    pub report_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryStage5InspectSummary {
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
    captures_logical_years: bool,
}

const GATES: &[GateSpec] = &[
    GateSpec {
        id: "logical-years-quality-v1",
        args: &[
            "test",
            "--locked",
            "--manifest-path",
            "crates/codegen/grokptah-agent-bridge/Cargo.toml",
            "--lib",
            "memory::tests::logical_years_certification_is_independent_of_fixture_echo",
            "--",
            "--exact",
            "--test-threads=1",
        ],
        expected_passed_tests: 1,
        captures_logical_years: true,
    },
    GateSpec {
        id: "memory-commit-cutpoints-v1",
        args: &[
            "test",
            "--locked",
            "--manifest-path",
            "crates/codegen/grokptah-agent-bridge/Cargo.toml",
            "--lib",
            "memory::tests::commit_cutpoints_never_false_succeed_and_uncertain_does_not_rollback",
            "--",
            "--exact",
            "--test-threads=1",
        ],
        expected_passed_tests: 1,
        captures_logical_years: false,
    },
    GateSpec {
        id: "memory-compaction-reopen-v1",
        args: &[
            "test",
            "--locked",
            "--manifest-path",
            "crates/codegen/grokptah-agent-bridge/Cargo.toml",
            "--lib",
            "memory::tests::receipts_survive_count_byte_expired_and_superseded_compaction_and_restart",
            "--",
            "--exact",
            "--test-threads=1",
        ],
        expected_passed_tests: 1,
        captures_logical_years: false,
    },
    GateSpec {
        id: "memory-cross-process-restart-v1",
        args: &[
            "test",
            "--locked",
            "--manifest-path",
            "crates/codegen/grokptah-agent-bridge/Cargo.toml",
            "--lib",
            "memory::tests::two_process_writers_remain_replayable_after_two_restarts",
            "--",
            "--exact",
            "--test-threads=1",
        ],
        expected_passed_tests: 1,
        captures_logical_years: false,
    },
    GateSpec {
        id: "memory-scope-isolation-v1",
        args: &[
            "test",
            "--locked",
            "--manifest-path",
            "crates/codegen/grokptah-agent-bridge/Cargo.toml",
            "--test",
            "memory_scopes",
            "--",
            "--test-threads=1",
        ],
        expected_passed_tests: 6,
        captures_logical_years: false,
    },
    GateSpec {
        id: "manager-memory-attribution-v1",
        args: &[
            "test",
            "--locked",
            "--manifest-path",
            "crates/codegen/grokptah-agent-bridge/Cargo.toml",
            "--lib",
            "orchestration::manager::tests::manager_memory_attribution_is_canonical_bounded_and_tamper_evident",
            "--",
            "--exact",
            "--test-threads=1",
        ],
        expected_passed_tests: 1,
        captures_logical_years: false,
    },
    GateSpec {
        id: "manager-objective-pre-run-v1",
        args: &[
            "test",
            "--locked",
            "--manifest-path",
            "crates/codegen/grokptah-agent-bridge/Cargo.toml",
            "--lib",
            "orchestration::service::provider_route_constraint_tests::manager_decision_work_objective_is_frozen_before_admission",
            "--",
            "--exact",
            "--test-threads=1",
        ],
        expected_passed_tests: 1,
        captures_logical_years: false,
    },
    GateSpec {
        id: "manager-store-restart-v1",
        args: &[
            "test",
            "--locked",
            "--manifest-path",
            "crates/codegen/grokptah-agent-bridge/Cargo.toml",
            "--test",
            "manager_store",
            "--",
            "--test-threads=1",
        ],
        expected_passed_tests: 5,
        captures_logical_years: false,
    },
    GateSpec {
        id: "manager-supervisor-loopback-v1",
        args: &[
            "test",
            "--locked",
            "--manifest-path",
            "crates/codegen/grokptah-agent-bridge/Cargo.toml",
            "--test",
            "manager_supervisor",
            "--",
            "--test-threads=1",
        ],
        expected_passed_tests: 4,
        captures_logical_years: false,
    },
    GateSpec {
        id: "manager-native-proposal-loopback-v1",
        args: &[
            "test",
            "--locked",
            "--manifest-path",
            "crates/codegen/grokptah-agent-bridge/Cargo.toml",
            "--test",
            "native_executor_mcp",
            "manager_decision_native_admission_has_durable_proposal_purpose",
            "--",
            "--exact",
            "--test-threads=1",
            "--nocapture",
        ],
        expected_passed_tests: 1,
        captures_logical_years: false,
    },
];

impl MemoryStage5Evidence {
    pub fn validate(&self) -> Result<()> {
        if self.schema != MEMORY_STAGE5_REPORT_SCHEMA {
            bail!("unsupported Stage 5 memory evidence schema");
        }
        if !valid_id(&self.campaign_id) || !lower_sha(&self.candidate_sha, 40) {
            bail!("invalid Stage 5 campaign identity");
        }
        if !self.repository_clean || !self.secret_free {
            bail!("Stage 5 evidence is not clean and secret-free");
        }
        self.logical_years.validate()?;
        if self.logical_years.schema != MEMORY_LONG_HORIZON_EVIDENCE_SCHEMA
            || self.logical_years.candidate_sha != self.candidate_sha
            || self.logical_years.evidence_digest
                != expected_memory_evidence_digest(&self.logical_years)
        {
            bail!("logical-years evidence is not bound to the Stage 5 candidate");
        }
        if self.gates.len() != GATES.len() {
            bail!("Stage 5 report omits or adds a required gate");
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
                bail!("Stage 5 gate evidence is missing, reordered, or invalid");
            }
        }
        if !self.all_required_gates_passed
            || !self.claim_eligible
            || self.evidence_digest != expected_evidence_digest(self)
        {
            bail!("Stage 5 evidence is incomplete or digest-invalid");
        }
        Ok(())
    }

    pub fn certification_ready(&self) -> bool {
        self.claim_eligible && self.validate().is_ok()
    }
}

pub fn expected_evidence_digest(evidence: &MemoryStage5Evidence) -> String {
    let mut unsigned = evidence.clone();
    unsigned.evidence_digest.clear();
    digest(&serde_json::to_vec(&unsigned).expect("Stage 5 evidence serialization"))
}

pub fn run(options: &MemoryStage5Options) -> Result<MemoryStage5Completion> {
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
        "memory-stage5-{}-{}",
        &candidate_sha[..16],
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let artifacts = output.create_campaign(&campaign_id)?;
    let scratch = tempfile::tempdir().context("creating Stage 5 scratch directory")?;
    let logical_path = scratch.path().join("logical-years.json");
    let mut gates = Vec::with_capacity(GATES.len());

    for spec in GATES {
        let gate = run_gate(
            spec,
            &repository,
            &candidate_sha,
            &logical_path,
            scratch.path(),
        )?;
        if !gate.passed {
            bail!("Stage 5 gate failed");
        }
        let checkpoint = serde_json::to_vec_pretty(&gate)?;
        artifacts.write_partial(format!("gates/{}.json", spec.id), &checkpoint)?;
        gates.push(gate);
    }

    let logical_years = read_logical_evidence(&logical_path, &candidate_sha)?;
    let final_repository = canonical_clean_repository(&repository)?;
    if final_repository != repository
        || git_output(&repository, &["rev-parse", "HEAD"])? != candidate_sha
    {
        bail!("candidate repository changed during Stage 5 certification");
    }
    let mut evidence = MemoryStage5Evidence {
        schema: MEMORY_STAGE5_REPORT_SCHEMA.to_owned(),
        campaign_id: campaign_id.clone(),
        candidate_sha: candidate_sha.clone(),
        repository_clean: true,
        logical_years,
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
        bail!("Stage 5 report exceeds its bound");
    }
    let report = artifacts.write_final(MEMORY_STAGE5_REPORT_PATH, &bytes)?;
    artifacts.mark_complete(&report)?;
    Ok(MemoryStage5Completion {
        campaign_id,
        candidate_sha,
        certification_ready: evidence.certification_ready(),
        report_sha256: report.sha256,
    })
}

pub fn inspect(campaign: &Path) -> Result<MemoryStage5InspectSummary> {
    let sealed = verify_completed_campaign(campaign)?;
    if sealed.relative_path != MEMORY_STAGE5_REPORT_PATH || sealed.bytes > MAX_REPORT_BYTES as u64 {
        bail!("completion seal does not reference a bounded Stage 5 report");
    }
    let bytes = bounded_regular_read(
        &campaign.join(MEMORY_STAGE5_REPORT_PATH),
        MAX_REPORT_BYTES as u64,
    )?;
    if bytes.len() as u64 != sealed.bytes || digest(&bytes) != sealed.sha256 {
        bail!("sealed Stage 5 report does not match its completion record");
    }
    let evidence: MemoryStage5Evidence =
        serde_json::from_slice(&bytes).context("invalid Stage 5 report")?;
    evidence.validate()?;
    Ok(MemoryStage5InspectSummary {
        valid: true,
        completion_seal_verified: true,
        certification_ready: evidence.certification_ready(),
        campaign_id: evidence.campaign_id,
        candidate_sha: evidence.candidate_sha,
        gates_passed: evidence.gates.iter().filter(|gate| gate.passed).count() as u32,
        gates_required: GATES.len() as u32,
    })
}

fn run_gate(
    spec: &GateSpec,
    repository: &Path,
    candidate_sha: &str,
    logical_path: &Path,
    scratch: &Path,
) -> Result<MemoryStage5GateEvidence> {
    let stdout_path = scratch.join(format!("{}.stdout", spec.id));
    let stderr_path = scratch.join(format!("{}.stderr", spec.id));
    let stdout = File::create(&stdout_path)?;
    let stderr = File::create(&stderr_path)?;
    let mut command = Command::new("cargo");
    command
        .args(spec.args)
        .current_dir(repository)
        .env("RUST_TEST_THREADS", "1")
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    if spec.captures_logical_years {
        command
            .env("GROKPTAH_CANDIDATE_SHA", candidate_sha)
            .env("GROKPTAH_MEMORY_EVIDENCE_OUTPUT", logical_path);
    }
    let mut child = command.spawn().context("launching Stage 5 gate")?;
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
            bail!("Stage 5 gate output exceeded its bound");
        }
        if started.elapsed() >= GATE_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Stage 5 gate exceeded its duration bound");
        }
        thread::sleep(Duration::from_millis(100));
    };
    let stdout = bounded_regular_read(&stdout_path, MAX_GATE_OUTPUT_BYTES)?;
    let stderr = bounded_regular_read(&stderr_path, MAX_GATE_OUTPUT_BYTES)?;
    let observed = observed_passed_tests(&stdout, &stderr, spec.expected_passed_tests);
    let exit_code = status_code(status)?;
    Ok(MemoryStage5GateEvidence {
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

fn read_logical_evidence(path: &Path, candidate_sha: &str) -> Result<MemoryLongHorizonEvidence> {
    let bytes = bounded_regular_read(path, MAX_LOGICAL_EVIDENCE_BYTES)?;
    let evidence: MemoryLongHorizonEvidence =
        serde_json::from_slice(&bytes).context("invalid logical-years evidence")?;
    evidence.validate()?;
    if evidence.candidate_sha != candidate_sha {
        bail!("logical-years evidence belongs to another candidate");
    }
    Ok(evidence)
}

fn canonical_clean_repository(path: &Path) -> Result<PathBuf> {
    let repository = dunce::canonicalize(path).context("canonicalizing repository")?;
    if !repository.is_dir() || !repository.join(".git").exists() {
        bail!("Stage 5 repository is not a Git checkout");
    }
    if !git_output(
        &repository,
        &["status", "--porcelain", "--untracked-files=normal"],
    )?
    .is_empty()
    {
        bail!("Stage 5 certification requires a clean repository");
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
            "capturesLogicalYears": spec.captures_logical_years,
        }))
        .expect("gate command serialization"),
    )
}

fn status_code(status: ExitStatus) -> Result<i32> {
    status
        .code()
        .ok_or_else(|| anyhow::anyhow!("Stage 5 gate ended without an exit code"))
}

fn file_len(path: &Path) -> Result<u64> {
    Ok(fs::symlink_metadata(path)?.len())
}

fn bounded_regular_read(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > maximum {
        bail!("Stage 5 artifact is not a bounded regular file");
    }
    let bytes = fs::read(path)?;
    if bytes.len() as u64 != metadata.len() {
        bail!("Stage 5 artifact changed during read");
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

    fn logical(candidate_sha: &str) -> MemoryLongHorizonEvidence {
        let mut evidence = MemoryLongHorizonEvidence {
            schema: MEMORY_LONG_HORIZON_EVIDENCE_SCHEMA.into(),
            certification_id: "memory-certification".into(),
            candidate_sha: candidate_sha.into(),
            fixture_id: "memory-long-horizon-v1".into(),
            fixture_digest: "a".repeat(64),
            core_source_digest: "b".repeat(64),
            logical_years: 10,
            scopes: vec!["project".into(), "agent_private".into(), "team".into()],
            critical_recall_pct: 100,
            stale_as_current_pct: 0,
            conflict_recall_pct: 100,
            conflict_false_positive_pct: 0,
            duplicate_rate_pct: 0,
            hot_store_within_byte_bound: true,
            repeated_read_reopen_deterministic: true,
            secret_free: true,
            claim_eligible: false,
            evidence_digest: String::new(),
        };
        evidence.evidence_digest = expected_memory_evidence_digest(&evidence);
        evidence
    }

    fn ready() -> MemoryStage5Evidence {
        let candidate_sha = "c".repeat(40);
        let gates = GATES
            .iter()
            .map(|spec| MemoryStage5GateEvidence {
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
        let mut evidence = MemoryStage5Evidence {
            schema: MEMORY_STAGE5_REPORT_SCHEMA.into(),
            campaign_id: "memory-stage5-campaign".into(),
            candidate_sha: candidate_sha.clone(),
            repository_clean: true,
            logical_years: logical(&candidate_sha),
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
        assert!(!encoded.contains("bearer"));
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
    fn candidate_drift_unknown_fields_and_false_claims_fail_closed() {
        let mut drifted = ready();
        drifted.logical_years.candidate_sha = "f".repeat(40);
        drifted.logical_years.evidence_digest =
            expected_memory_evidence_digest(&drifted.logical_years);
        drifted.evidence_digest = expected_evidence_digest(&drifted);
        assert!(drifted.validate().is_err());

        let mut false_claim = ready();
        false_claim.all_required_gates_passed = false;
        false_claim.evidence_digest = expected_evidence_digest(&false_claim);
        assert!(false_claim.validate().is_err());

        let mut value = serde_json::to_value(ready()).unwrap();
        value["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<MemoryStage5Evidence>(value).is_err());
    }

    #[test]
    fn gate_output_parser_requires_the_exact_green_cardinality() {
        assert_eq!(
            observed_passed_tests(b"test result: ok. 4 passed; 0 failed;", b"", 4),
            4
        );
        assert_eq!(
            observed_passed_tests(b"test result: ok. 3 passed; 0 failed;", b"", 4),
            0
        );
        assert_eq!(
            observed_passed_tests(b"test result: FAILED. 4 passed; 1 failed;", b"", 4),
            0
        );
    }

    #[test]
    fn sealed_report_round_trips_through_the_independent_inspector() {
        let repository = tempfile::tempdir().unwrap();
        let repository = dunce::canonicalize(repository.path()).unwrap();
        let output = repository.join("runs");
        let root = SafeOutputRoot::open(&output, &repository, None, 1024 * 1024).unwrap();
        let artifacts = root.create_campaign("memory-stage5-inspect").unwrap();
        let mut evidence = ready();
        evidence.campaign_id = "memory-stage5-inspect".into();
        evidence.evidence_digest = expected_evidence_digest(&evidence);
        let bytes = serde_json::to_vec_pretty(&evidence).unwrap();
        let report = artifacts
            .write_final(MEMORY_STAGE5_REPORT_PATH, &bytes)
            .unwrap();
        artifacts.mark_complete(&report).unwrap();

        let inspected = inspect(artifacts.path()).unwrap();
        assert!(inspected.certification_ready);
        assert_eq!(inspected.gates_passed, GATES.len() as u32);
        assert_eq!(inspected.candidate_sha, evidence.candidate_sha);
    }
}
