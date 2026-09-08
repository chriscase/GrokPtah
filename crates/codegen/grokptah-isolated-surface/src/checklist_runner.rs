//! Sep 18 One-Mac / CI checklist runner — seals independent evidence packs.
//!
//! Default substrate is Contained Browser dry-run. Optional VF dry-run when
//! features allow. Never enables admission or claims physical PASS.

use std::path::Path;

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
use crate::vf_dry_run::VfDryRunEvidence;

/// Runner configuration for the Sep 18 checklist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sep18ChecklistRunnerConfig {
    pub substrate: Sep18ChecklistSubstrate,
    pub fault_matrix_case: Option<FaultMatrixCase>,
    pub vf_physical_mac_proof_id: Option<String>,
    pub snapshot_root: Option<std::path::PathBuf>,
}

impl Default for Sep18ChecklistRunnerConfig {
    fn default() -> Self {
        Self {
            substrate: Sep18ChecklistSubstrate::ContainedBrowserDryRun,
            fault_matrix_case: None,
            vf_physical_mac_proof_id: None,
            snapshot_root: None,
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

fn empty_rejected_pack(substrate: Sep18ChecklistSubstrate) -> Sep18EvidencePack {
    seal_vf_dry_run_pack(VfDryRunEvidence {
        platform: crate::vf_dry_run::VfDryRunPlatform::NonMacOs,
        outcome: crate::vf_dry_run::VfDryRunOutcome::UnsupportedPlatform,
        evidence_class: crate::lifecycle::ProofEvidenceClass::Synthetic,
        boot_attempted: false,
        physical_pass_claimed: false,
        nonclaim: crate::SYNTHETIC_HARNESS_NONCLAIM.into(),
        recorded_at: chrono::Utc::now(),
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
