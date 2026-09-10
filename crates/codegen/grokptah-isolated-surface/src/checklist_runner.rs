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
    seal_contained_browser_dry_run_pack, seal_native_host_sentinel_pack,
    seal_synthetic_harness_pack, seal_vf_dry_run_pack, serialize_evidence_pack,
    Sep18ChecklistSubstrate, Sep18EvidencePack,
};
use crate::native_sentinel_runner::NativeSentinelEvidence;
use crate::proof_sequencer::{FaultMatrixCase, Sep18NoModelProofSequencer};
use crate::sentinel::HostSentinelSnapshot;
use crate::vf_dry_run::VfDryRunEvidence;

/// Runner configuration for the Sep 18 checklist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sep18ChecklistRunnerConfig {
    pub substrate: Sep18ChecklistSubstrate,
    pub fault_matrix_case: Option<FaultMatrixCase>,
    pub vf_physical_mac_proof_id: Option<String>,
    pub snapshot_root: Option<std::path::PathBuf>,
    pub checkout_path: Option<std::path::PathBuf>,
}

impl Default for Sep18ChecklistRunnerConfig {
    fn default() -> Self {
        Self {
            substrate: Sep18ChecklistSubstrate::ContainedBrowserDryRun,
            fault_matrix_case: None,
            vf_physical_mac_proof_id: None,
            snapshot_root: None,
            checkout_path: None,
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

    pub fn with_fault_matrix(mut self, case: FaultMatrixCase) -> Self {
        self.fault_matrix_case = Some(case);
        self
    }

    pub fn with_snapshot_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.snapshot_root = Some(root.into());
        self
    }

    pub fn native_host_sentinel(checkout_path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            substrate: Sep18ChecklistSubstrate::NativeHostSentinel,
            checkout_path: Some(checkout_path.into()),
            vf_physical_mac_proof_id: Some("sep18-native-vf-dry-run".into()),
            ..Self::default()
        }
    }
}

/// Parsed `run` CLI request. Validation completes before any checklist side effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sep18ChecklistRunRequest {
    pub output: PathBuf,
    pub config: Sep18ChecklistRunnerConfig,
}

/// Parse `run` flags. Native mode requires `--native-host-sentinels`,
/// `--vf-dry-run`, and an explicitly supplied `--checkout PATH`.
pub fn parse_sep18_checklist_run_args(args: &[String]) -> HarnessResult<Sep18ChecklistRunRequest> {
    let mut output: Option<PathBuf> = None;
    let mut vf_dry_run = false;
    let mut native_host_sentinels = false;
    let mut synthetic = false;
    let mut checkout: Option<PathBuf> = None;
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
            "--checkout" => {
                if checkout.is_some() {
                    return Err(HarnessError::invalid_state("duplicate flag: --checkout"));
                }
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    HarnessError::invalid_state("--checkout requires a PATH argument")
                })?;
                if value.is_empty() {
                    return Err(HarnessError::invalid_state(
                        "native host-sentinel runner requires an explicitly supplied --checkout PATH",
                    ));
                }
                checkout = Some(PathBuf::from(value));
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

    if native_host_sentinels {
        if !vf_dry_run {
            return Err(HarnessError::invalid_state(
                "native mode requires --native-host-sentinels, --vf-dry-run, and an explicitly supplied --checkout PATH",
            ));
        }
        if checkout.is_none() {
            return Err(HarnessError::invalid_state(
                "native mode requires an explicitly supplied --checkout PATH (never defaulted to .)",
            ));
        }
        if synthetic || fault_matrix.is_some() {
            return Err(HarnessError::invalid_state(
                "--native-host-sentinels cannot be combined with --synthetic or --fault-matrix",
            ));
        }
    } else if checkout.is_some() {
        return Err(HarnessError::invalid_state(
            "--checkout is only valid with --native-host-sentinels",
        ));
    }

    if vf_dry_run && synthetic {
        return Err(HarnessError::invalid_state(
            "--vf-dry-run and --synthetic are mutually exclusive",
        ));
    }

    let mut config = if native_host_sentinels {
        let checkout_path = checkout.ok_or_else(|| {
            HarnessError::invalid_state(
                "native mode requires an explicitly supplied --checkout PATH (never defaulted to .)",
            )
        })?;
        Sep18ChecklistRunnerConfig::native_host_sentinel(checkout_path)
    } else if vf_dry_run {
        Sep18ChecklistRunnerConfig::vf_dry_run("sep18-cli-vf-dry-run")
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
    let mut sequencer = Sep18NoModelProofSequencer::new(baseline);
    if let Some(root) = &config.snapshot_root {
        sequencer = sequencer.with_snapshot_root(root);
    }

    match config.substrate {
        Sep18ChecklistSubstrate::ContainedBrowserDryRun => run_contained_browser(config, sequencer),
        Sep18ChecklistSubstrate::VfDryRun => run_vf_dry_run(config, sequencer),
        Sep18ChecklistSubstrate::SyntheticHarness => run_synthetic(config, sequencer),
        Sep18ChecklistSubstrate::NativeHostSentinel => run_native_host_sentinel(config, sequencer),
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
    sequencer: Sep18NoModelProofSequencer,
) -> Sep18ChecklistRunOutcome {
    let proof_id = config
        .vf_physical_mac_proof_id
        .unwrap_or_else(|| "sep18-vf-dry-run".into());
    let receipt = VfLaunchReceipt {
        physical_mac_proof_id: proof_id,
    };

    match sequencer.run_vf_dry_run(receipt) {
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

fn run_native_host_sentinel(
    config: Sep18ChecklistRunnerConfig,
    sequencer: Sep18NoModelProofSequencer,
) -> Sep18ChecklistRunOutcome {
    if config.fault_matrix_case.is_some() {
        return Sep18ChecklistRunOutcome {
            pack: empty_rejected_pack(Sep18ChecklistSubstrate::NativeHostSentinel),
            runner_error: Some(HarnessError::invalid_state(
                "native host-sentinel runner does not accept a fault-matrix subset",
            )),
        };
    }

    let checkout = match config.checkout_path {
        Some(path) if !path.as_os_str().is_empty() => path,
        _ => {
            let err = HarnessError::invalid_state(
                "native host-sentinel runner requires an explicitly supplied checkout PATH",
            );
            return Sep18ChecklistRunOutcome {
                pack: native_fail_closed_pack(&err),
                runner_error: Some(err),
            };
        }
    };

    match sequencer.run_native_host_sentinel(checkout) {
        Ok(evidence) => Sep18ChecklistRunOutcome {
            pack: seal_native_host_sentinel_pack(evidence),
            runner_error: None,
        },
        Err(err) => Sep18ChecklistRunOutcome {
            pack: native_fail_closed_pack(&err),
            runner_error: Some(err),
        },
    }
}

fn native_fail_closed_pack(err: &HarnessError) -> Sep18EvidencePack {
    seal_native_host_sentinel_pack(NativeSentinelEvidence::fail_closed_for_current_platform(
        err,
    ))
}

fn empty_rejected_pack(substrate: Sep18ChecklistSubstrate) -> Sep18EvidencePack {
    match substrate {
        Sep18ChecklistSubstrate::NativeHostSentinel => native_fail_closed_pack(
            &HarnessError::invalid_state("native host-sentinel runner failed closed"),
        ),
        other => {
            let mut evidence = VfDryRunEvidence::unsupported(
                crate::vf_dry_run::VfDryRunPlatform::NonMacOs,
                crate::vf_dry_run::VfDryRunOutcome::UnsupportedPlatform,
            );
            evidence.evidence_class = crate::lifecycle::ProofEvidenceClass::Synthetic;
            evidence.nonclaim = crate::SYNTHETIC_HARNESS_NONCLAIM.into();
            seal_vf_dry_run_pack(evidence).into_placeholder(other)
        }
    }
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
