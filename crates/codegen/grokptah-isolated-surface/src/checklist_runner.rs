//! Sep 18 One-Mac / CI checklist runner — seals independent evidence packs.
//!
//! Default substrate is Contained Browser dry-run. Optional VF dry-run when
//! features allow. Never enables admission or claims physical PASS.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::backend::VfLaunchReceipt;
use crate::contained_browser_dry_run::ContainedBrowserDryRunEvidence;
use crate::error::{HarnessError, HarnessResult};
use crate::evidence_pack::{
    seal_contained_browser_dry_run_pack, seal_synthetic_harness_pack, seal_vf_dry_run_pack,
    serialize_evidence_pack, Sep18ChecklistSubstrate, Sep18EvidencePack,
};
use crate::proof_sequencer::{FaultMatrixCase, Sep18NoModelProofSequencer};
use crate::sentinel::HostSentinelSnapshot;
use crate::vf_dry_run::{run_vf_dry_run_with_native_host_sentinels, VfDryRunEvidence};

/// Runner configuration for the Sep 18 checklist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sep18ChecklistRunnerConfig {
    pub substrate: Sep18ChecklistSubstrate,
    pub fault_matrix_case: Option<FaultMatrixCase>,
    pub vf_physical_mac_proof_id: Option<String>,
    pub snapshot_root: Option<std::path::PathBuf>,
    #[serde(default)]
    pub native_host_sentinels: bool,
}

impl Default for Sep18ChecklistRunnerConfig {
    fn default() -> Self {
        Self {
            substrate: Sep18ChecklistSubstrate::ContainedBrowserDryRun,
            fault_matrix_case: None,
            vf_physical_mac_proof_id: None,
            snapshot_root: None,
            native_host_sentinels: false,
        }
    }
}

impl Sep18ChecklistRunnerConfig {
    pub fn contained_browser_default() -> Self {
        Self::default()
    }

    pub fn vf_dry_run(physical_mac_proof_id: impl Into<String>) -> Self {
        Self {
            substrate: Sep18ChecklistSubstrate::VfDryRun,
            vf_physical_mac_proof_id: Some(physical_mac_proof_id.into()),
            ..Self::default()
        }
    }

    pub fn vf_dry_run_with_native_host_sentinels(physical_mac_proof_id: impl Into<String>) -> Self {
        Self {
            native_host_sentinels: true,
            ..Self::vf_dry_run(physical_mac_proof_id)
        }
    }

    pub fn with_fault_matrix(mut self, case: FaultMatrixCase) -> Self {
        self.fault_matrix_case = Some(case);
        self
    }

    pub fn with_snapshot_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.snapshot_root = Some(root.into());
        self
    }
}

/// Parsed `run` CLI request. Validation completes before any checklist side effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sep18ChecklistRunRequest {
    pub output: PathBuf,
    pub config: Sep18ChecklistRunnerConfig,
}

/// Parse `run` flags. Invalid, duplicate, or unknown inputs fail before side effects.
/// `--native-host-sentinels` is valid only with `--vf-dry-run`.
pub fn parse_sep18_checklist_run_args(args: &[String]) -> HarnessResult<Sep18ChecklistRunRequest> {
    let mut output: Option<PathBuf> = None;
    let mut vf_dry_run = false;
    let mut native_host_sentinels = false;
    let mut synthetic = false;
    let mut fault_matrix: Option<FaultMatrixCase> = None;

    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--output" | "-o" => {
                if output.is_some() {
                    return Err(HarnessError::invalid_state("duplicate flag: --output"));
                }
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    HarnessError::invalid_state("--output requires a path argument")
                })?;
                output = Some(PathBuf::from(value));
            }
            "--vf-dry-run" => {
                if vf_dry_run {
                    return Err(HarnessError::invalid_state("duplicate flag: --vf-dry-run"));
                }
                vf_dry_run = true;
            }
            "--native-host-sentinels" => {
                if native_host_sentinels {
                    return Err(HarnessError::invalid_state(
                        "duplicate flag: --native-host-sentinels",
                    ));
                }
                native_host_sentinels = true;
            }
            "--synthetic" => {
                if synthetic {
                    return Err(HarnessError::invalid_state("duplicate flag: --synthetic"));
                }
                synthetic = true;
            }
            "--fault-matrix" => {
                if fault_matrix.is_some() {
                    return Err(HarnessError::invalid_state(
                        "duplicate flag: --fault-matrix",
                    ));
                }
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    HarnessError::invalid_state("--fault-matrix requires a case argument")
                })?;
                fault_matrix = Some(parse_fault_matrix_case(value)?);
            }
            other => {
                return Err(HarnessError::invalid_state(format!(
                    "unknown run flag: {other}"
                )));
            }
        }
        idx += 1;
    }

    if native_host_sentinels && !vf_dry_run {
        return Err(HarnessError::invalid_state(
            "--native-host-sentinels is valid only with --vf-dry-run",
        ));
    }
    if native_host_sentinels && synthetic {
        return Err(HarnessError::invalid_state(
            "--native-host-sentinels is valid only with --vf-dry-run",
        ));
    }
    if native_host_sentinels && fault_matrix.is_some() {
        return Err(HarnessError::invalid_state(
            "--native-host-sentinels is valid only with --vf-dry-run",
        ));
    }
    if vf_dry_run && synthetic {
        return Err(HarnessError::invalid_state(
            "--vf-dry-run and --synthetic are mutually exclusive",
        ));
    }

    let mut config = if vf_dry_run {
        if native_host_sentinels {
            Sep18ChecklistRunnerConfig::vf_dry_run_with_native_host_sentinels(
                "sep18-cli-vf-dry-run",
            )
        } else {
            Sep18ChecklistRunnerConfig::vf_dry_run("sep18-cli-vf-dry-run")
        }
    } else if synthetic {
        Sep18ChecklistRunnerConfig {
            substrate: Sep18ChecklistSubstrate::SyntheticHarness,
            ..Sep18ChecklistRunnerConfig::default()
        }
    } else {
        Sep18ChecklistRunnerConfig::contained_browser_default()
    };
    config.fault_matrix_case = fault_matrix;

    Ok(Sep18ChecklistRunRequest {
        output: output.unwrap_or_else(|| PathBuf::from("sep18-evidence-pack.json")),
        config,
    })
}

fn parse_fault_matrix_case(value: &str) -> HarnessResult<FaultMatrixCase> {
    match value {
        "boot_stop" | "BootStop" => Ok(FaultMatrixCase::BootStop),
        "pre_dispatch_stop" | "PreDispatchStop" => Ok(FaultMatrixCase::PreDispatchStop),
        "lost_ack_uncertain" | "LostAckUncertain" => Ok(FaultMatrixCase::LostAckUncertain),
        "restart_no_replay" | "RestartNoReplay" => Ok(FaultMatrixCase::RestartNoReplay),
        other => Err(HarnessError::invalid_state(format!(
            "unknown fault matrix case: {other}"
        ))),
    }
}

/// Outcome of a checklist run before independent verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sep18ChecklistRunOutcome {
    pub pack: Sep18EvidencePack,
    pub runner_error: Option<HarnessError>,
}

impl Sep18ChecklistRunOutcome {
    pub fn pack_json(&self) -> HarnessResult<String> {
        serialize_evidence_pack(&self.pack).map_err(|err| {
            HarnessError::invalid_state(format!("evidence pack serialization failed: {err}"))
        })
    }

    pub fn write_pack(&self, path: &Path) -> HarnessResult<()> {
        let json = self.pack_json()?;
        std::fs::write(path, json).map_err(|err| {
            HarnessError::invalid_state(format!("failed to write evidence pack: {err}"))
        })
    }
}

/// Run the Sep 18 checklist and seal an evidence pack.
pub fn run_sep18_checklist(
    baseline: HostSentinelSnapshot,
    config: Sep18ChecklistRunnerConfig,
) -> Sep18ChecklistRunOutcome {
    if config.native_host_sentinels && config.substrate != Sep18ChecklistSubstrate::VfDryRun {
        return Sep18ChecklistRunOutcome {
            pack: empty_rejected_pack(config.substrate),
            runner_error: Some(HarnessError::invalid_state(
                "native host sentinels are valid only with VF dry-run",
            )),
        };
    }

    let mut sequencer = Sep18NoModelProofSequencer::new(baseline.clone());
    if let Some(root) = &config.snapshot_root {
        sequencer = sequencer.with_snapshot_root(root);
    }

    match config.substrate {
        Sep18ChecklistSubstrate::ContainedBrowserDryRun => run_contained_browser(config, sequencer),
        Sep18ChecklistSubstrate::VfDryRun => run_vf_dry_run(config, baseline),
        Sep18ChecklistSubstrate::SyntheticHarness => run_synthetic(config, sequencer),
    }
}

fn run_contained_browser(
    config: Sep18ChecklistRunnerConfig,
    sequencer: Sep18NoModelProofSequencer,
) -> Sep18ChecklistRunOutcome {
    let result = if let Some(case) = config.fault_matrix_case {
        sequencer
            .run_contained_browser_fault_matrix(case)
            .map(|sealed| {
                let mut evidence = ContainedBrowserDryRunEvidence {
                    platform: crate::contained_browser_dry_run::ContainedBrowserDryRunPlatform::SimulatorSubstrate,
                    outcome: crate::contained_browser_dry_run::ContainedBrowserDryRunOutcome::SubstrateRehearsal,
                    evidence_class: sealed.evidence_class,
                    checklist_completed: false,
                    isolation_pass_claimed: false,
                    vf_pass_claimed: false,
                    nonclaim: sealed.nonclaim.clone(),
                    sealed_evidence: Some(sealed),
                    captured_frames: None,
                    recorded_at: chrono::Utc::now(),
                };
                evidence.checklist_completed = evidence
                    .sealed_evidence
                    .as_ref()
                    .is_some_and(|s| s.checklist_steps.contains(&crate::proof_sequencer::ChecklistStep::EvidenceSealed));
                evidence
            })
    } else {
        sequencer.run_contained_browser_dry_run()
    };

    match result {
        Ok(evidence) => Sep18ChecklistRunOutcome {
            pack: seal_contained_browser_dry_run_pack(evidence),
            runner_error: None,
        },
        Err(err) => Sep18ChecklistRunOutcome {
            pack: empty_rejected_pack(Sep18ChecklistSubstrate::ContainedBrowserDryRun),
            runner_error: Some(err),
        },
    }
}

fn run_vf_dry_run(
    config: Sep18ChecklistRunnerConfig,
    baseline: HostSentinelSnapshot,
) -> Sep18ChecklistRunOutcome {
    let proof_id = config
        .vf_physical_mac_proof_id
        .unwrap_or_else(|| "sep18-vf-dry-run".into());
    let receipt = VfLaunchReceipt {
        physical_mac_proof_id: proof_id,
    };

    match run_vf_dry_run_with_native_host_sentinels(receipt, baseline, config.native_host_sentinels)
    {
        Ok(evidence) => Sep18ChecklistRunOutcome {
            pack: seal_vf_dry_run_pack(evidence),
            runner_error: None,
        },
        Err(err) => Sep18ChecklistRunOutcome {
            pack: empty_rejected_pack(Sep18ChecklistSubstrate::VfDryRun),
            runner_error: Some(err),
        },
    }
}

fn run_synthetic(
    config: Sep18ChecklistRunnerConfig,
    sequencer: Sep18NoModelProofSequencer,
) -> Sep18ChecklistRunOutcome {
    let result = if let Some(case) = config.fault_matrix_case {
        sequencer.run_fault_matrix(case)
    } else {
        sequencer.run_happy_path()
    };

    match result {
        Ok(sealed) => {
            let completed = sealed
                .checklist_steps
                .contains(&crate::proof_sequencer::ChecklistStep::EvidenceSealed);
            Sep18ChecklistRunOutcome {
                pack: seal_synthetic_harness_pack(sealed, completed),
                runner_error: None,
            }
        }
        Err(err) => Sep18ChecklistRunOutcome {
            pack: empty_rejected_pack(Sep18ChecklistSubstrate::SyntheticHarness),
            runner_error: Some(err),
        },
    }
}

fn empty_rejected_pack(substrate: Sep18ChecklistSubstrate) -> Sep18EvidencePack {
    seal_vf_dry_run_pack(VfDryRunEvidence {
        platform: crate::vf_dry_run::VfDryRunPlatform::NonMacOs,
        outcome: crate::vf_dry_run::VfDryRunOutcome::UnsupportedPlatform,
        evidence_class: crate::lifecycle::ProofEvidenceClass::Synthetic,
        boot_attempted: false,
        physical_pass_claimed: false,
        nonclaim: crate::SYNTHETIC_HARNESS_NONCLAIM.into(),
        recorded_at: chrono::Utc::now(),
        native_host_sentinels_requested: false,
        host_sentinel_probe_kind: None,
        host_sentinel_probes_performed: 0,
        host_sentinels_unchanged_at_stop: false,
        live_host_sentinel_collection_at_stop: false,
        channels_destroyed: 0,
    })
    .into_placeholder(substrate)
}

trait PlaceholderPack {
    fn into_placeholder(self, substrate: Sep18ChecklistSubstrate) -> Sep18EvidencePack;
}

impl PlaceholderPack for Sep18EvidencePack {
    fn into_placeholder(mut self, substrate: Sep18ChecklistSubstrate) -> Sep18EvidencePack {
        self.substrate = substrate;
        self.checklist_completed = false;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::HarnessErrorCode;
    use crate::isolated_surface_admission_available;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn native_host_sentinels_requires_vf_dry_run() {
        let err = parse_sep18_checklist_run_args(&args(&["--native-host-sentinels"]))
            .expect_err("native without vf-dry-run");
        assert_eq!(err.code, HarnessErrorCode::InvalidState);
        assert!(err.message.contains("--vf-dry-run"));
    }

    #[test]
    fn native_host_sentinels_rejects_duplicates_before_side_effects() {
        let err = parse_sep18_checklist_run_args(&args(&[
            "--vf-dry-run",
            "--native-host-sentinels",
            "--native-host-sentinels",
        ]))
        .expect_err("duplicate native flag");
        assert_eq!(err.code, HarnessErrorCode::InvalidState);
        assert!(err.message.contains("duplicate"));
    }

    #[test]
    fn native_host_sentinels_rejects_unknown_flags_before_side_effects() {
        let err = parse_sep18_checklist_run_args(&args(&[
            "--vf-dry-run",
            "--native-host-sentinels",
            "--promote",
        ]))
        .expect_err("unknown flag");
        assert!(err.message.contains("unknown run flag"));
    }

    #[test]
    fn native_host_sentinels_with_vf_dry_run_is_accepted() {
        let request = parse_sep18_checklist_run_args(&args(&[
            "--vf-dry-run",
            "--native-host-sentinels",
            "-o",
            "vf-native.json",
        ]))
        .expect("valid native vf dry-run");
        assert_eq!(request.config.substrate, Sep18ChecklistSubstrate::VfDryRun);
        assert!(request.config.native_host_sentinels);
        assert_eq!(request.output, PathBuf::from("vf-native.json"));
    }

    #[cfg(not(all(target_os = "macos", feature = "vf-backend")))]
    #[test]
    fn native_vf_dry_run_runner_fails_closed_without_pack_success() {
        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::vf_dry_run_with_native_host_sentinels("cli-native"),
        );
        let err = outcome.runner_error.expect("native must fail closed");
        assert_eq!(err.code, HarnessErrorCode::BackendUnavailable);
        assert!(!outcome.pack.physical_pass_claimed);
        assert!(!outcome.pack.vf_pass_claimed);
        assert!(!outcome.pack.isolation_pass_claimed);
        assert!(!outcome.pack.admission_available);
        assert!(!isolated_surface_admission_available());
    }

    #[test]
    fn default_vf_dry_run_still_uses_synthetic_when_native_not_requested() {
        let outcome = run_sep18_checklist(
            HostSentinelSnapshot::synthetic_baseline(),
            Sep18ChecklistRunnerConfig::vf_dry_run("cli-synthetic-default"),
        );
        assert!(outcome.runner_error.is_none(), "{:?}", outcome.runner_error);
        let vf = outcome.pack.vf_dry_run.expect("vf nested evidence");
        assert!(!vf.native_host_sentinels_requested);
        assert!(!outcome.pack.physical_pass_claimed);
        assert!(!outcome.pack.vf_pass_claimed);
        assert!(!outcome.pack.isolation_pass_claimed);
        assert!(!outcome.pack.admission_available);
    }
}
