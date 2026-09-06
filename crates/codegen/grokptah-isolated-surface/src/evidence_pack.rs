//! Sealed Sep 18 evidence pack + independent verifier.
//!
//! The verifier reads only the serialized pack — no live backend, no runner
//! aggregates. Admission stays false until a separate physical gate enables it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::contained_browser_dry_run::ContainedBrowserDryRunEvidence;
use crate::lifecycle::{GuestLifecycleDisposition, ProofEvidenceClass};
use crate::proof_sequencer::{ChecklistStep, FaultMatrixCase, SealedProofEvidence};
use crate::vf_dry_run::{VfDryRunEvidence, VfDryRunOutcome, VfDryRunPlatform};
use crate::{
    isolated_surface_admission_available, CONTAINED_BROWSER_DRY_RUN_NONCLAIM,
    SYNTHETIC_HARNESS_NONCLAIM, VF_DRY_RUN_NONCLAIM,
};

pub const EVIDENCE_PACK_SCHEMA_VERSION: u32 = 1;

/// Which checklist substrate produced the pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sep18ChecklistSubstrate {
    /// Default Linux CI / one-Mac rehearsal pivot path.
    ContainedBrowserDryRun,
    /// Optional VF gate rehearsal when features allow.
    VfDryRun,
    /// Synthetic harness happy-path or fault-matrix subset.
    SyntheticHarness,
}

/// Mac physical proof markers required before any VF PASS claim is accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhysicalProofMarkers {
    pub mac_worker_attested: bool,
    pub live_host_sentinel_collection: bool,
    pub physical_mac_proof_id: Option<String>,
}

impl PhysicalProofMarkers {
    pub fn dry_run_none() -> Self {
        Self {
            mac_worker_attested: false,
            live_host_sentinel_collection: false,
            physical_mac_proof_id: None,
        }
    }

    pub fn qualifies_vf_physical_pass(&self) -> bool {
        self.mac_worker_attested
            && self.live_host_sentinel_collection
            && self
                .physical_mac_proof_id
                .as_ref()
                .is_some_and(|id| !id.trim().is_empty())
    }
}

/// Host-sentinel probe summary sealed with Stop evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSentinelProbeSummary {
    pub probes_performed: u32,
    pub host_sentinels_unchanged_at_stop: bool,
    pub channels_destroyed: usize,
    pub channels_open_after_stop: usize,
}

impl HostSentinelProbeSummary {
    pub fn from_stop_evidence(stop: &crate::harness::StopEvidence, channels_open: usize) -> Self {
        Self {
            probes_performed: stop.host_sentinel_probes_performed,
            host_sentinels_unchanged_at_stop: stop.host_sentinels_unchanged,
            channels_destroyed: stop.channels_destroyed,
            channels_open_after_stop: channels_open,
        }
    }
}

/// Sealed Sep 18 checklist evidence pack for CI / one-Mac independent review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sep18EvidencePack {
    pub schema_version: u32,
    pub sealed_at: DateTime<Utc>,
    pub substrate: Sep18ChecklistSubstrate,
    pub evidence_class: ProofEvidenceClass,
    pub physical_pass_claimed: bool,
    pub isolation_pass_claimed: bool,
    pub vf_pass_claimed: bool,
    pub admission_available: bool,
    pub nonclaim: String,
    pub host_sentinel_probes: HostSentinelProbeSummary,
    pub physical_proof_markers: PhysicalProofMarkers,
    pub checklist_completed: bool,
    pub fault_matrix_case: Option<FaultMatrixCase>,
    pub sealed_evidence: Option<SealedProofEvidence>,
    pub contained_browser: Option<ContainedBrowserDryRunEvidence>,
    pub vf_dry_run: Option<VfDryRunEvidence>,
}

/// Explicit verifier decision codes — independent of runner success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceVerifierCode {
    Accepted,
    SchemaVersionUnsupported,
    AdmissionMustStayFalse,
    PhysicalPassWithoutMacMarkers,
    VfPassClaimOnDryRun,
    IsolationPassClaimOnDryRun,
    EvidenceClassTampered,
    StopChannelsStillOpen,
    StopSentinelProbeMissing,
    UncertainDowngradedAfterPossibleInject,
    ChecklistIncomplete,
    NonclaimMismatch,
    VfDryRunCannotQualifyPhysicalPass,
    FaultMatrixDispositionMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceVerifierDecision {
    pub accepted: bool,
    pub code: EvidenceVerifierCode,
    pub message: String,
}

impl EvidenceVerifierDecision {
    fn reject(code: EvidenceVerifierCode, message: impl Into<String>) -> Self {
        Self {
            accepted: false,
            code,
            message: message.into(),
        }
    }

    fn accept() -> Self {
        Self {
            accepted: true,
            code: EvidenceVerifierCode::Accepted,
            message: "evidence pack accepted".into(),
        }
    }
}

/// Independent verifier — reads only the sealed pack JSON.
pub fn verify_evidence_pack(pack: &Sep18EvidencePack) -> EvidenceVerifierDecision {
    if pack.schema_version != EVIDENCE_PACK_SCHEMA_VERSION {
        return EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::SchemaVersionUnsupported,
            format!(
                "unsupported schema version {} (expected {})",
                pack.schema_version, EVIDENCE_PACK_SCHEMA_VERSION
            ),
        );
    }

    if pack.admission_available {
        return EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::AdmissionMustStayFalse,
            "admission_available must remain false until a separate physical gate enables it",
        );
    }

    if pack.physical_pass_claimed && !pack.physical_proof_markers.qualifies_vf_physical_pass() {
        return EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::PhysicalPassWithoutMacMarkers,
            "physical_pass_claimed requires Mac worker attestation, live host sentinel collection, and physical_mac_proof_id",
        );
    }

    if pack.vf_pass_claimed && !pack.physical_proof_markers.qualifies_vf_physical_pass() {
        return EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::VfPassClaimOnDryRun,
            "vf_pass_claimed requires Mac physical proof markers",
        );
    }

    if pack.isolation_pass_claimed
        && matches!(
            pack.substrate,
            Sep18ChecklistSubstrate::ContainedBrowserDryRun
        )
    {
        return EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::IsolationPassClaimOnDryRun,
            "Contained Browser dry-run cannot claim isolation PASS",
        );
    }

    if let Some(vf) = &pack.vf_dry_run {
        if vf.physical_pass_claimed {
            return EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::VfDryRunCannotQualifyPhysicalPass,
                "VF dry-run artifacts never qualify as physical PASS",
            );
        }
        if matches!(
            vf.platform,
            VfDryRunPlatform::NonMacOs | VfDryRunPlatform::MacOsFeatureDisabled
        ) && (pack.physical_pass_claimed || pack.vf_pass_claimed)
        {
            return EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::VfDryRunCannotQualifyPhysicalPass,
                "VF dry-run on non-physical platforms cannot claim PASS",
            );
        }
        if matches!(
            vf.outcome,
            VfDryRunOutcome::UnsupportedPlatform
                | VfDryRunOutcome::FeatureDisabled
                | VfDryRunOutcome::BackendUnavailable
        ) && (pack.physical_pass_claimed || pack.vf_pass_claimed)
        {
            return EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::VfDryRunCannotQualifyPhysicalPass,
                "VF dry-run outcome is not a physical Sep 18 PASS",
            );
        }
    }

    if let Some(sealed) = &pack.sealed_evidence {
        if sealed.evidence_class != pack.evidence_class {
            return EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::EvidenceClassTampered,
                "pack evidence_class does not match sealed_evidence label",
            );
        }
        if let Some(decision) = verify_sealed_evidence(pack, sealed) {
            return decision;
        }
    } else if pack.checklist_completed {
        return EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::ChecklistIncomplete,
            "checklist_completed requires sealed_evidence",
        );
    }

    if !expected_nonclaim(pack).is_empty() && pack.nonclaim != expected_nonclaim(pack) {
        return EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::NonclaimMismatch,
            "nonclaim string does not match substrate contract",
        );
    }

    if pack.host_sentinel_probes.channels_open_after_stop > 0 {
        return EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::StopChannelsStillOpen,
            "Stop must destroy all guest channels",
        );
    }

    if pack.host_sentinel_probes.host_sentinels_unchanged_at_stop
        && pack.host_sentinel_probes.probes_performed == 0
    {
        return EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::StopSentinelProbeMissing,
            "host_sentinels_unchanged requires at least one host sentinel probe",
        );
    }

    if let Some(sealed) = &pack.sealed_evidence {
        if let Some(decision) = verify_uncertain_invariants(sealed) {
            return decision;
        }
        if let Some(decision) = verify_fault_matrix_disposition(sealed) {
            return decision;
        }
    }

    EvidenceVerifierDecision::accept()
}

fn verify_sealed_evidence(
    pack: &Sep18EvidencePack,
    sealed: &SealedProofEvidence,
) -> Option<EvidenceVerifierDecision> {
    let stop = &sealed.stop_evidence;
    if stop.host_sentinels_unchanged && stop.host_sentinel_probes_performed == 0 {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::StopSentinelProbeMissing,
            "sealed stop_evidence claims unchanged sentinels without probes",
        ));
    }

    if pack.host_sentinel_probes.probes_performed != stop.host_sentinel_probes_performed {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::EvidenceClassTampered,
            "host_sentinel_probes_performed mismatch between pack and sealed stop_evidence",
        ));
    }

    if !sealed
        .checklist_steps
        .contains(&ChecklistStep::EvidenceSealed)
    {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::ChecklistIncomplete,
            "sealed checklist missing EvidenceSealed step",
        ));
    }

    if pack.checklist_completed
        && !sealed
            .checklist_steps
            .contains(&ChecklistStep::StopDestroyed)
    {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::ChecklistIncomplete,
            "completed checklist must include StopDestroyed",
        ));
    }

    if stop.host_sentinel_probe_error.is_some() && stop.host_sentinels_unchanged {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::StopSentinelProbeMissing,
            "stop_evidence cannot claim unchanged sentinels with probe error",
        ));
    }

    None
}

fn verify_uncertain_invariants(sealed: &SealedProofEvidence) -> Option<EvidenceVerifierDecision> {
    let possible_inject = sealed
        .checklist_steps
        .contains(&ChecklistStep::GuestLocalActionMarkedPossible);
    let disposition = sealed.stop_evidence.disposition;

    if sealed.fault_matrix_case == Some(FaultMatrixCase::LostAckUncertain)
        && disposition != Some(GuestLifecycleDisposition::Uncertain)
    {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::UncertainDowngradedAfterPossibleInject,
            "LostAckUncertain fault matrix requires Uncertain disposition at Stop",
        ));
    }

    if possible_inject
        && sealed.fault_matrix_case == Some(FaultMatrixCase::LostAckUncertain)
        && disposition == Some(GuestLifecycleDisposition::Stopped)
    {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::UncertainDowngradedAfterPossibleInject,
            "Uncertain cannot be downgraded to Stopped after possible guest inject",
        ));
    }

    None
}

fn verify_fault_matrix_disposition(
    sealed: &SealedProofEvidence,
) -> Option<EvidenceVerifierDecision> {
    match sealed.fault_matrix_case {
        Some(FaultMatrixCase::PreDispatchStop) => {
            if sealed.stop_evidence.disposition != Some(GuestLifecycleDisposition::Stopped) {
                return Some(EvidenceVerifierDecision::reject(
                    EvidenceVerifierCode::FaultMatrixDispositionMismatch,
                    "PreDispatchStop requires Stopped disposition",
                ));
            }
        }
        Some(FaultMatrixCase::LostAckUncertain) | Some(FaultMatrixCase::RestartNoReplay) => {
            if sealed.stop_evidence.disposition != Some(GuestLifecycleDisposition::Uncertain) {
                return Some(EvidenceVerifierDecision::reject(
                    EvidenceVerifierCode::UncertainDowngradedAfterPossibleInject,
                    "uncertain fault matrix requires Uncertain disposition at Stop",
                ));
            }
        }
        _ => {}
    }
    None
}

fn expected_nonclaim(pack: &Sep18EvidencePack) -> String {
    match pack.substrate {
        Sep18ChecklistSubstrate::ContainedBrowserDryRun => {
            CONTAINED_BROWSER_DRY_RUN_NONCLAIM.into()
        }
        Sep18ChecklistSubstrate::VfDryRun => VF_DRY_RUN_NONCLAIM.into(),
        Sep18ChecklistSubstrate::SyntheticHarness => SYNTHETIC_HARNESS_NONCLAIM.into(),
    }
}

/// Serialize a pack to pretty JSON for independent verification.
pub fn serialize_evidence_pack(pack: &Sep18EvidencePack) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(pack)
}

/// Parse a sealed evidence pack from JSON.
pub fn parse_evidence_pack(json: &str) -> Result<Sep18EvidencePack, serde_json::Error> {
    serde_json::from_str(json)
}

/// Build an honest dry-run pack from runner outputs. Never sets physical PASS.
pub fn seal_contained_browser_dry_run_pack(
    evidence: ContainedBrowserDryRunEvidence,
) -> Sep18EvidencePack {
    let host_sentinel_probes = evidence
        .sealed_evidence
        .as_ref()
        .map(|sealed| HostSentinelProbeSummary::from_stop_evidence(&sealed.stop_evidence, 0))
        .unwrap_or(HostSentinelProbeSummary {
            probes_performed: 0,
            host_sentinels_unchanged_at_stop: false,
            channels_destroyed: 0,
            channels_open_after_stop: 0,
        });

    Sep18EvidencePack {
        schema_version: EVIDENCE_PACK_SCHEMA_VERSION,
        sealed_at: Utc::now(),
        substrate: Sep18ChecklistSubstrate::ContainedBrowserDryRun,
        evidence_class: evidence.evidence_class,
        physical_pass_claimed: false,
        isolation_pass_claimed: evidence.isolation_pass_claimed,
        vf_pass_claimed: evidence.vf_pass_claimed,
        admission_available: isolated_surface_admission_available(),
        nonclaim: evidence.nonclaim.clone(),
        host_sentinel_probes,
        physical_proof_markers: PhysicalProofMarkers::dry_run_none(),
        checklist_completed: evidence.checklist_completed,
        fault_matrix_case: evidence
            .sealed_evidence
            .as_ref()
            .and_then(|s| s.fault_matrix_case),
        sealed_evidence: evidence.sealed_evidence.clone(),
        contained_browser: Some(evidence),
        vf_dry_run: None,
    }
}

pub fn seal_vf_dry_run_pack(evidence: VfDryRunEvidence) -> Sep18EvidencePack {
    Sep18EvidencePack {
        schema_version: EVIDENCE_PACK_SCHEMA_VERSION,
        sealed_at: Utc::now(),
        substrate: Sep18ChecklistSubstrate::VfDryRun,
        evidence_class: evidence.evidence_class,
        physical_pass_claimed: evidence.physical_pass_claimed,
        isolation_pass_claimed: false,
        vf_pass_claimed: false,
        admission_available: isolated_surface_admission_available(),
        nonclaim: evidence.nonclaim.clone(),
        host_sentinel_probes: HostSentinelProbeSummary {
            probes_performed: 0,
            host_sentinels_unchanged_at_stop: false,
            channels_destroyed: 0,
            channels_open_after_stop: 0,
        },
        physical_proof_markers: PhysicalProofMarkers::dry_run_none(),
        checklist_completed: false,
        fault_matrix_case: None,
        sealed_evidence: None,
        contained_browser: None,
        vf_dry_run: Some(evidence),
    }
}

pub fn seal_synthetic_harness_pack(
    sealed: SealedProofEvidence,
    checklist_completed: bool,
) -> Sep18EvidencePack {
    let host_sentinel_probes =
        HostSentinelProbeSummary::from_stop_evidence(&sealed.stop_evidence, 0);

    Sep18EvidencePack {
        schema_version: EVIDENCE_PACK_SCHEMA_VERSION,
        sealed_at: Utc::now(),
        substrate: Sep18ChecklistSubstrate::SyntheticHarness,
        evidence_class: sealed.evidence_class,
        physical_pass_claimed: false,
        isolation_pass_claimed: false,
        vf_pass_claimed: false,
        admission_available: isolated_surface_admission_available(),
        nonclaim: sealed.nonclaim.clone(),
        host_sentinel_probes,
        physical_proof_markers: PhysicalProofMarkers::dry_run_none(),
        checklist_completed,
        fault_matrix_case: sealed.fault_matrix_case,
        sealed_evidence: Some(sealed),
        contained_browser: None,
        vf_dry_run: None,
    }
}

/// Map verifier code to process exit status for CLI.
pub fn verifier_exit_code(decision: &EvidenceVerifierDecision) -> i32 {
    if decision.accepted {
        0
    } else {
        match decision.code {
            EvidenceVerifierCode::Accepted => 0,
            EvidenceVerifierCode::SchemaVersionUnsupported => 10,
            EvidenceVerifierCode::AdmissionMustStayFalse => 11,
            EvidenceVerifierCode::PhysicalPassWithoutMacMarkers => 12,
            EvidenceVerifierCode::VfPassClaimOnDryRun => 13,
            EvidenceVerifierCode::IsolationPassClaimOnDryRun => 14,
            EvidenceVerifierCode::EvidenceClassTampered => 15,
            EvidenceVerifierCode::StopChannelsStillOpen => 16,
            EvidenceVerifierCode::StopSentinelProbeMissing => 17,
            EvidenceVerifierCode::UncertainDowngradedAfterPossibleInject => 18,
            EvidenceVerifierCode::ChecklistIncomplete => 19,
            EvidenceVerifierCode::NonclaimMismatch => 20,
            EvidenceVerifierCode::VfDryRunCannotQualifyPhysicalPass => 21,
            EvidenceVerifierCode::FaultMatrixDispositionMismatch => 22,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::VfLaunchReceipt;
    use crate::proof_sequencer::Sep18NoModelProofSequencer;
    use crate::sentinel::HostSentinelSnapshot;

    #[test]
    fn cb_dry_run_pack_verifies() {
        let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
        let evidence = sequencer
            .run_contained_browser_dry_run()
            .expect("cb dry-run");
        let pack = seal_contained_browser_dry_run_pack(evidence);
        let decision = verify_evidence_pack(&pack);
        assert!(decision.accepted, "{:?}", decision);
    }

    #[test]
    fn vf_dry_run_pack_never_qualifies_physical_pass() {
        let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
        let evidence = sequencer
            .run_vf_dry_run(VfLaunchReceipt {
                physical_mac_proof_id: "linux-ci-dry-run".into(),
            })
            .expect("vf dry-run");
        let mut pack = seal_vf_dry_run_pack(evidence);
        pack.physical_pass_claimed = true;
        pack.vf_pass_claimed = true;
        let decision = verify_evidence_pack(&pack);
        assert!(!decision.accepted);
        assert_eq!(
            decision.code,
            EvidenceVerifierCode::PhysicalPassWithoutMacMarkers
        );
    }
}
