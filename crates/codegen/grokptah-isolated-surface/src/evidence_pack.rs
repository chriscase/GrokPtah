//! Sealed Sep 18 evidence pack + independent verifier.
//!
//! The verifier reads only the serialized pack — no live backend, no runner
//! aggregates. Admission stays false until a separate physical gate enables it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::captured_frame::{
    validate_public_evidence, CapturedFrameMediaKind, CapturedFrameSource,
};
use crate::contained_browser_dry_run::ContainedBrowserDryRunEvidence;
use crate::lifecycle::{GuestLifecycleDisposition, ProofEvidenceClass};
use crate::native_sentinel_runner::{
    NativeSentinelEvidence, NativeSentinelRunnerOutcome, NativeSentinelRunnerPlatform,
};
use crate::proof_sequencer::{ChecklistStep, FaultMatrixCase, SealedProofEvidence};
use crate::sentinel::HostSentinelProbeKind;
use crate::vf_dry_run::{VfDryRunEvidence, VfDryRunOutcome, VfDryRunPlatform};
use crate::{
    isolated_surface_admission_available, CONTAINED_BROWSER_DRY_RUN_NONCLAIM,
    NATIVE_HOST_SENTINEL_NONCLAIM, SYNTHETIC_HARNESS_NONCLAIM, VF_DRY_RUN_NONCLAIM,
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
    /// Explicit native Mac host-sentinel runner (collector exclusive).
    NativeHostSentinel,
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
    pub live_host_sentinel_collection_at_stop: bool,
    pub host_sentinels_unchanged_at_stop: bool,
    pub channels_destroyed: usize,
    pub channels_open_after_stop: usize,
    /// Kind of the last Stop probe. Missing deserializes fail-closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_probe_kind: Option<HostSentinelProbeKind>,
}

impl HostSentinelProbeSummary {
    pub fn empty() -> Self {
        Self {
            probes_performed: 0,
            live_host_sentinel_collection_at_stop: false,
            host_sentinels_unchanged_at_stop: false,
            channels_destroyed: 0,
            channels_open_after_stop: 0,
            last_probe_kind: None,
        }
    }

    pub fn from_stop_evidence(stop: &crate::harness::StopEvidence, channels_open: usize) -> Self {
        Self {
            probes_performed: stop.host_sentinel_probes_performed,
            live_host_sentinel_collection_at_stop: stop.live_host_sentinel_collection,
            host_sentinels_unchanged_at_stop: stop.host_sentinels_unchanged,
            channels_destroyed: stop.channels_destroyed,
            channels_open_after_stop: channels_open,
            last_probe_kind: stop.last_host_sentinel_probe_kind,
        }
    }

    pub fn from_vf_dry_run(evidence: &VfDryRunEvidence) -> Self {
        Self {
            probes_performed: evidence.host_sentinel_probes_performed,
            live_host_sentinel_collection_at_stop: evidence.live_host_sentinel_collection_at_stop,
            host_sentinels_unchanged_at_stop: evidence.host_sentinels_unchanged_at_stop,
            channels_destroyed: evidence.channels_destroyed,
            channels_open_after_stop: evidence.channels_open_after_stop,
            last_probe_kind: evidence.last_host_sentinel_probe_kind,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_host_sentinel: Option<NativeSentinelEvidence>,
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
    HostSentinelProbeSummaryMismatch,
    PackSealedEvidenceMismatch,
    SubstrateNestedEvidenceMissing,
    CapturedFrameEvidenceInvalid,
    NativeSentinelSyntheticFallback,
    NativeSentinelLiveClaimOnUnsupportedPlatform,
    NativeSentinelCannotQualifyPhysicalPass,
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

    if let Some(decision) = verify_substrate_pass_claims(pack) {
        return decision;
    }

    if let Some(decision) = verify_substrate_evidence_class(pack) {
        return decision;
    }

    if let Some(decision) = verify_substrate_nested_evidence(pack) {
        return decision;
    }

    if let Some(decision) = verify_native_host_sentinel(pack) {
        return decision;
    }

    if let Some(decision) = verify_vf_dry_run_stop_summary(pack) {
        return decision;
    }

    if let Some(decision) = verify_sealed_evidence_presence(pack) {
        return decision;
    }

    if pack.physical_pass_claimed && !pack.physical_proof_markers.qualifies_vf_physical_pass() {
        return EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::PhysicalPassWithoutMacMarkers,
            "physical_pass_claimed requires Mac worker attestation, live host sentinel collection, and physical_mac_proof_id",
        );
    }

    if pack.physical_proof_markers.live_host_sentinel_collection {
        if let Some(sealed) = &pack.sealed_evidence {
            if !sealed.stop_evidence.live_host_sentinel_collection {
                return EvidenceVerifierDecision::reject(
                    EvidenceVerifierCode::PhysicalPassWithoutMacMarkers,
                    "live_host_sentinel_collection marker requires successful native Mac host probes in stop_evidence",
                );
            }
        } else if !pack
            .host_sentinel_probes
            .live_host_sentinel_collection_at_stop
        {
            return EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::PhysicalPassWithoutMacMarkers,
                "live_host_sentinel_collection marker requires native Mac host probes at Stop",
            );
        }
    }

    if pack.vf_pass_claimed && !pack.physical_proof_markers.qualifies_vf_physical_pass() {
        return EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::VfPassClaimOnDryRun,
            "vf_pass_claimed requires Mac physical proof markers",
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
        if let Some(decision) = verify_pack_sealed_consistency(pack, sealed) {
            return decision;
        }
        if let Some(decision) = verify_sealed_evidence(pack, sealed) {
            return decision;
        }
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
        if let Some(decision) = verify_fault_matrix_encoding(sealed) {
            return decision;
        }
        if let Some(decision) = verify_fault_matrix_disposition(sealed) {
            return decision;
        }
    }

    if let Some(cb) = &pack.contained_browser {
        if let Some(cb_sealed) = &cb.sealed_evidence {
            if let Some(decision) = verify_uncertain_invariants(cb_sealed) {
                return decision;
            }
            if let Some(decision) = verify_fault_matrix_encoding(cb_sealed) {
                return decision;
            }
            if let Some(decision) = verify_fault_matrix_disposition(cb_sealed) {
                return decision;
            }
        }
    }

    if let Some(decision) = verify_pack_level_uncertain_triggers(pack) {
        return decision;
    }

    if let Some(decision) = verify_captured_frame_pack(pack) {
        return decision;
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
            EvidenceVerifierCode::HostSentinelProbeSummaryMismatch,
            "host_sentinel_probes_performed mismatch between pack and sealed stop_evidence",
        ));
    }

    if pack
        .host_sentinel_probes
        .live_host_sentinel_collection_at_stop
        != stop.live_host_sentinel_collection
    {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::HostSentinelProbeSummaryMismatch,
            "live_host_sentinel_collection mismatch between pack and sealed stop_evidence",
        ));
    }

    if pack.host_sentinel_probes.channels_destroyed != stop.channels_destroyed {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::HostSentinelProbeSummaryMismatch,
            "channels_destroyed mismatch between pack summary and sealed stop_evidence",
        ));
    }

    if pack.host_sentinel_probes.last_probe_kind != stop.last_host_sentinel_probe_kind {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::HostSentinelProbeSummaryMismatch,
            "last probe kind mismatch between pack summary and sealed stop_evidence",
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

    if !sealed
        .checklist_steps
        .contains(&ChecklistStep::StopDestroyed)
    {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::ChecklistIncomplete,
            "sealed checklist must include StopDestroyed teardown",
        ));
    }

    if !sealed.checklist_steps.contains(&ChecklistStep::Booted) {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::ChecklistIncomplete,
            "sealed checklist must include Booted",
        ));
    }

    if let Some(decision) = verify_probe_encoding_binding(
        sealed.fault_matrix_case,
        sealed.stop_evidence.host_sentinel_probes_performed,
        sealed.stop_evidence.disposition,
        &sealed.checklist_steps,
    ) {
        return Some(decision);
    }

    if let Some(decision) = verify_checklist_step_order(&sealed.checklist_steps) {
        return Some(decision);
    }

    if let Some(decision) = verify_exact_fault_matrix_steps(sealed) {
        return Some(decision);
    }

    if let Some(decision) = verify_checklist_shape(sealed) {
        return Some(decision);
    }

    if sealed
        .checklist_steps
        .contains(&ChecklistStep::StopDestroyed)
        && stop.channels_destroyed == 0
    {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::StopChannelsStillOpen,
            "StopDestroyed requires channels to be torn down",
        ));
    }

    if sealed
        .checklist_steps
        .contains(&ChecklistStep::StopDestroyed)
        && stop.backend_destroy_error.is_some()
    {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::ChecklistIncomplete,
            "StopDestroyed requires confirmed backend destroy",
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

fn verify_checklist_shape(sealed: &SealedProofEvidence) -> Option<EvidenceVerifierDecision> {
    let steps = &sealed.checklist_steps;

    match sealed.fault_matrix_case {
        Some(FaultMatrixCase::BootStop) | Some(FaultMatrixCase::PreDispatchStop) => return None,
        _ => {}
    }

    if steps.contains(&ChecklistStep::FrameChallenge)
        && !steps.contains(&ChecklistStep::GuestLocalActionMarkedPossible)
    {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::ChecklistIncomplete,
            "FrameChallenge checklist must record guest-local action possibility before Stop",
        ));
    }

    if !steps.contains(&ChecklistStep::FrameChallenge)
        && !steps.contains(&ChecklistStep::GuestLocalActionMarkedPossible)
    {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::ChecklistIncomplete,
            "Booted checklist must record guest-local action possibility before Stop",
        ));
    }

    None
}

fn verify_uncertain_invariants(sealed: &SealedProofEvidence) -> Option<EvidenceVerifierDecision> {
    let steps = &sealed.checklist_steps;
    let guest_action_marked = steps.contains(&ChecklistStep::GuestLocalActionMarkedPossible);
    let frame_challenge = steps.contains(&ChecklistStep::FrameChallenge);
    let postcondition_verified = steps.contains(&ChecklistStep::PostconditionVerified);
    let disposition = sealed.stop_evidence.disposition;

    if postcondition_verified && !frame_challenge {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::UncertainDowngradedAfterPossibleInject,
            "PostconditionVerified is only valid on the FrameChallenge happy path",
        ));
    }

    if guest_action_marked && !frame_challenge {
        if postcondition_verified {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::UncertainDowngradedAfterPossibleInject,
                "inject-uncertain checklist forbids PostconditionVerified",
            ));
        }
        if disposition != Some(GuestLifecycleDisposition::Uncertain) {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::UncertainDowngradedAfterPossibleInject,
                "guest inject without postcondition requires Uncertain disposition at Stop",
            ));
        }
    }

    if guest_action_marked
        && frame_challenge
        && !postcondition_verified
        && disposition != Some(GuestLifecycleDisposition::Uncertain)
    {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::UncertainDowngradedAfterPossibleInject,
            "guest inject without postcondition requires Uncertain disposition at Stop",
        ));
    }

    if sealed.fault_matrix_case == Some(FaultMatrixCase::LostAckUncertain)
        && disposition != Some(GuestLifecycleDisposition::Uncertain)
    {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::UncertainDowngradedAfterPossibleInject,
            "LostAckUncertain fault matrix requires Uncertain disposition at Stop",
        ));
    }

    None
}

fn verify_fault_matrix_encoding(sealed: &SealedProofEvidence) -> Option<EvidenceVerifierDecision> {
    let steps = &sealed.checklist_steps;
    let has_frame = steps.contains(&ChecklistStep::FrameChallenge);
    let has_guest = steps.contains(&ChecklistStep::GuestLocalActionMarkedPossible);
    let has_post = steps.contains(&ChecklistStep::PostconditionVerified);
    let has_stale = steps.contains(&ChecklistStep::StaleTokensRejected);
    let probes = sealed.stop_evidence.host_sentinel_probes_performed;

    match sealed.fault_matrix_case {
        Some(FaultMatrixCase::BootStop) => {
            if has_frame || has_guest || has_post {
                return Some(EvidenceVerifierDecision::reject(
                    EvidenceVerifierCode::FaultMatrixDispositionMismatch,
                    "BootStop checklist must not record inject or postcondition steps",
                ));
            }
            if probes != 3 {
                return Some(EvidenceVerifierDecision::reject(
                    EvidenceVerifierCode::FaultMatrixDispositionMismatch,
                    "BootStop fault matrix is inconsistent with sealed stop probe count",
                ));
            }
        }
        Some(FaultMatrixCase::PreDispatchStop) => {
            if !has_frame || has_guest || has_post {
                return Some(EvidenceVerifierDecision::reject(
                    EvidenceVerifierCode::FaultMatrixDispositionMismatch,
                    "PreDispatchStop checklist must record FrameChallenge without guest inject",
                ));
            }
            if probes != 3 {
                return Some(EvidenceVerifierDecision::reject(
                    EvidenceVerifierCode::FaultMatrixDispositionMismatch,
                    "PreDispatchStop fault matrix is inconsistent with sealed stop probe count",
                ));
            }
        }
        Some(FaultMatrixCase::LostAckUncertain) => {
            if has_frame || has_post || !has_guest {
                return Some(EvidenceVerifierDecision::reject(
                    EvidenceVerifierCode::FaultMatrixDispositionMismatch,
                    "LostAckUncertain checklist must record guest inject without postcondition",
                ));
            }
            if probes != 4 {
                return Some(EvidenceVerifierDecision::reject(
                    EvidenceVerifierCode::FaultMatrixDispositionMismatch,
                    "LostAckUncertain fault matrix is inconsistent with sealed stop probe count",
                ));
            }
        }
        Some(FaultMatrixCase::RestartNoReplay) => {
            if has_frame || has_post || !has_guest || !has_stale {
                return Some(EvidenceVerifierDecision::reject(
                    EvidenceVerifierCode::FaultMatrixDispositionMismatch,
                    "RestartNoReplay checklist must record guest inject and stale-token rejection",
                ));
            }
        }
        None => {
            if !has_frame
                || !has_guest
                || !has_post
                || !has_stale
                || probes != 5
                || sealed.stop_evidence.disposition != Some(GuestLifecycleDisposition::Stopped)
            {
                return Some(EvidenceVerifierDecision::reject(
                    EvidenceVerifierCode::ChecklistIncomplete,
                    "fault_matrix_case None requires exact happy-path checklist encoding",
                ));
            }
        }
    }

    None
}

fn checklist_step_index(step: &ChecklistStep) -> usize {
    match step {
        ChecklistStep::Armed => 0,
        ChecklistStep::Booted => 1,
        ChecklistStep::FrameChallenge => 2,
        ChecklistStep::GuestLocalActionMarkedPossible => 3,
        ChecklistStep::PostconditionVerified => 4,
        ChecklistStep::StopDestroyed => 5,
        ChecklistStep::StaleTokensRejected => 6,
        ChecklistStep::EvidenceSealed => 7,
    }
}

fn verify_checklist_step_order(steps: &[ChecklistStep]) -> Option<EvidenceVerifierDecision> {
    let mut last_index = None;
    for step in steps {
        let index = checklist_step_index(step);
        if let Some(prev) = last_index {
            if index <= prev {
                return Some(EvidenceVerifierDecision::reject(
                    EvidenceVerifierCode::ChecklistIncomplete,
                    "sealed checklist steps must follow canonical rehearsal order",
                ));
            }
        }
        last_index = Some(index);
    }
    None
}

fn expected_steps_for_fault(
    fault_matrix_case: Option<FaultMatrixCase>,
) -> &'static [ChecklistStep] {
    match fault_matrix_case {
        None => &[
            ChecklistStep::Armed,
            ChecklistStep::Booted,
            ChecklistStep::FrameChallenge,
            ChecklistStep::GuestLocalActionMarkedPossible,
            ChecklistStep::PostconditionVerified,
            ChecklistStep::StopDestroyed,
            ChecklistStep::StaleTokensRejected,
            ChecklistStep::EvidenceSealed,
        ],
        Some(FaultMatrixCase::BootStop) => &[
            ChecklistStep::Armed,
            ChecklistStep::Booted,
            ChecklistStep::StopDestroyed,
            ChecklistStep::EvidenceSealed,
        ],
        Some(FaultMatrixCase::PreDispatchStop) => &[
            ChecklistStep::Armed,
            ChecklistStep::Booted,
            ChecklistStep::FrameChallenge,
            ChecklistStep::StopDestroyed,
            ChecklistStep::EvidenceSealed,
        ],
        Some(FaultMatrixCase::LostAckUncertain) => &[
            ChecklistStep::Armed,
            ChecklistStep::Booted,
            ChecklistStep::GuestLocalActionMarkedPossible,
            ChecklistStep::StopDestroyed,
            ChecklistStep::EvidenceSealed,
        ],
        Some(FaultMatrixCase::RestartNoReplay) => &[
            ChecklistStep::Armed,
            ChecklistStep::Booted,
            ChecklistStep::GuestLocalActionMarkedPossible,
            ChecklistStep::StopDestroyed,
            ChecklistStep::StaleTokensRejected,
            ChecklistStep::EvidenceSealed,
        ],
    }
}

fn verify_exact_fault_matrix_steps(
    sealed: &SealedProofEvidence,
) -> Option<EvidenceVerifierDecision> {
    let expected = expected_steps_for_fault(sealed.fault_matrix_case);
    if sealed.checklist_steps.as_slice() != expected {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::ChecklistIncomplete,
            "checklist steps do not exactly match fault_matrix_case encoding",
        ));
    }
    None
}

fn verify_probe_encoding_binding(
    fault_matrix_case: Option<FaultMatrixCase>,
    probes: u32,
    disposition: Option<GuestLifecycleDisposition>,
    steps: &[ChecklistStep],
) -> Option<EvidenceVerifierDecision> {
    let has_frame = steps.contains(&ChecklistStep::FrameChallenge);
    let has_guest = steps.contains(&ChecklistStep::GuestLocalActionMarkedPossible);
    let has_post = steps.contains(&ChecklistStep::PostconditionVerified);
    let has_stale = steps.contains(&ChecklistStep::StaleTokensRejected);

    if probes == 4
        && (fault_matrix_case != Some(FaultMatrixCase::LostAckUncertain)
            || disposition != Some(GuestLifecycleDisposition::Uncertain)
            || !has_guest
            || has_post
            || has_frame)
    {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::FaultMatrixDispositionMismatch,
            "probe count 4 requires LostAckUncertain encoding with Uncertain disposition",
        ));
    }

    if probes == 3 {
        match fault_matrix_case {
            Some(FaultMatrixCase::BootStop) => {
                if has_frame
                    || has_guest
                    || has_post
                    || disposition != Some(GuestLifecycleDisposition::Stopped)
                {
                    return Some(EvidenceVerifierDecision::reject(
                        EvidenceVerifierCode::FaultMatrixDispositionMismatch,
                        "probe count 3 BootStop encoding is inconsistent with sealed checklist",
                    ));
                }
            }
            Some(FaultMatrixCase::PreDispatchStop) => {
                if !has_frame
                    || has_guest
                    || has_post
                    || disposition != Some(GuestLifecycleDisposition::Stopped)
                {
                    return Some(EvidenceVerifierDecision::reject(
                        EvidenceVerifierCode::FaultMatrixDispositionMismatch,
                        "probe count 3 PreDispatchStop encoding is inconsistent with sealed checklist",
                    ));
                }
            }
            _ => {
                return Some(EvidenceVerifierDecision::reject(
                    EvidenceVerifierCode::FaultMatrixDispositionMismatch,
                    "probe count 3 requires BootStop or PreDispatchStop encoding",
                ));
            }
        }
    }

    if probes == 5
        && (fault_matrix_case.is_some()
            || !has_frame
            || !has_guest
            || !has_post
            || !has_stale
            || disposition != Some(GuestLifecycleDisposition::Stopped))
    {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::ChecklistIncomplete,
            "probe count 5 requires exact happy-path encoding",
        ));
    }

    None
}

fn verify_pack_level_uncertain_triggers(
    pack: &Sep18EvidencePack,
) -> Option<EvidenceVerifierDecision> {
    if let Some(sealed) = &pack.sealed_evidence {
        if let Some(decision) = verify_probe_encoding_binding(
            pack.fault_matrix_case,
            pack.host_sentinel_probes.probes_performed,
            sealed.stop_evidence.disposition,
            &sealed.checklist_steps,
        ) {
            return Some(decision);
        }
    }

    match pack.fault_matrix_case {
        Some(FaultMatrixCase::LostAckUncertain) | Some(FaultMatrixCase::RestartNoReplay) => {
            let disposition = pack
                .sealed_evidence
                .as_ref()
                .and_then(|sealed| sealed.stop_evidence.disposition);
            if disposition != Some(GuestLifecycleDisposition::Uncertain) {
                return Some(EvidenceVerifierDecision::reject(
                    EvidenceVerifierCode::UncertainDowngradedAfterPossibleInject,
                    "pack fault_matrix_case requires Uncertain disposition at Stop",
                ));
            }
        }
        _ => {}
    }
    None
}

fn pack_implies_sealed_stop_story(pack: &Sep18EvidencePack) -> bool {
    pack.fault_matrix_case.is_some()
        || pack.checklist_completed
        || pack.host_sentinel_probes.channels_destroyed > 0
        || pack.host_sentinel_probes.probes_performed > 0
}

fn verify_sealed_evidence_presence(pack: &Sep18EvidencePack) -> Option<EvidenceVerifierDecision> {
    if pack.sealed_evidence.is_none() && pack_implies_sealed_stop_story(pack) {
        if pack.substrate == Sep18ChecklistSubstrate::VfDryRun && pack.vf_dry_run.is_some() {
            // VF dry-run carries Stop summary on nested evidence without a full checklist seal.
        } else {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::ChecklistIncomplete,
                "stop or checklist progress requires sealed_evidence",
            ));
        }
    }

    if matches!(
        pack.substrate,
        Sep18ChecklistSubstrate::ContainedBrowserDryRun | Sep18ChecklistSubstrate::SyntheticHarness
    ) && pack.sealed_evidence.is_none()
    {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::ChecklistIncomplete,
            "substrate checklist requires sealed_evidence",
        ));
    }

    if let Some(cb) = &pack.contained_browser {
        if cb.checklist_completed && pack.sealed_evidence.is_none() {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::ChecklistIncomplete,
                "contained_browser checklist_completed requires pack sealed_evidence",
            ));
        }
    }

    if matches!(
        pack.substrate,
        Sep18ChecklistSubstrate::ContainedBrowserDryRun
    ) && pack.sealed_evidence.is_some()
    {
        let cb = pack.contained_browser.as_ref()?;
        if cb.sealed_evidence.is_none() {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::SubstrateNestedEvidenceMissing,
                "ContainedBrowserDryRun requires contained_browser.sealed_evidence when pack sealed_evidence is present",
            ));
        }
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

fn verify_substrate_pass_claims(pack: &Sep18EvidencePack) -> Option<EvidenceVerifierDecision> {
    // No physical VF substrate exists today — every current substrate is dry-run /
    // synthetic rehearsal. Unsigned JSON markers must never qualify PASS.
    if pack.physical_pass_claimed {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::VfDryRunCannotQualifyPhysicalPass,
            "no current substrate can claim physical PASS",
        ));
    }

    if pack.vf_pass_claimed {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::VfPassClaimOnDryRun,
            "no current substrate can claim VF PASS",
        ));
    }

    if pack.isolation_pass_claimed {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::IsolationPassClaimOnDryRun,
            "no current substrate can claim isolation PASS",
        ));
    }

    if let Some(cb) = &pack.contained_browser {
        if cb.vf_pass_claimed {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::VfPassClaimOnDryRun,
                "contained_browser evidence cannot claim VF PASS on dry-run",
            ));
        }
        if cb.isolation_pass_claimed {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::IsolationPassClaimOnDryRun,
                "contained_browser evidence cannot claim isolation PASS on dry-run",
            ));
        }
    }

    if let Some(vf) = &pack.vf_dry_run {
        if vf.physical_pass_claimed {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::VfDryRunCannotQualifyPhysicalPass,
                "VF dry-run artifacts never qualify as physical PASS",
            ));
        }
    }

    if let Some(native) = &pack.native_host_sentinel {
        if native.physical_pass_claimed || native.vf_pass_claimed {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::NativeSentinelCannotQualifyPhysicalPass,
                "native host-sentinel runner cannot claim VF or physical PASS",
            ));
        }
        if native.isolation_pass_claimed {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::IsolationPassClaimOnDryRun,
                "native host-sentinel runner cannot claim isolation PASS",
            ));
        }
    }

    None
}

fn verify_substrate_nested_evidence(pack: &Sep18EvidencePack) -> Option<EvidenceVerifierDecision> {
    match pack.substrate {
        Sep18ChecklistSubstrate::ContainedBrowserDryRun => {
            if pack.contained_browser.is_none() {
                return Some(EvidenceVerifierDecision::reject(
                    EvidenceVerifierCode::SubstrateNestedEvidenceMissing,
                    "ContainedBrowserDryRun requires contained_browser nested evidence",
                ));
            }
        }
        Sep18ChecklistSubstrate::VfDryRun => {
            if pack.vf_dry_run.is_none() {
                return Some(EvidenceVerifierDecision::reject(
                    EvidenceVerifierCode::SubstrateNestedEvidenceMissing,
                    "VfDryRun requires vf_dry_run nested evidence",
                ));
            }
        }
        Sep18ChecklistSubstrate::SyntheticHarness => {}
        Sep18ChecklistSubstrate::NativeHostSentinel => {
            if pack.native_host_sentinel.is_none() {
                return Some(EvidenceVerifierDecision::reject(
                    EvidenceVerifierCode::SubstrateNestedEvidenceMissing,
                    "NativeHostSentinel requires native_host_sentinel nested evidence",
                ));
            }
        }
    }
    None
}

fn verify_vf_dry_run_stop_summary(pack: &Sep18EvidencePack) -> Option<EvidenceVerifierDecision> {
    if pack.substrate != Sep18ChecklistSubstrate::VfDryRun {
        return None;
    }
    let vf = pack.vf_dry_run.as_ref()?;
    let expected = HostSentinelProbeSummary::from_vf_dry_run(vf);
    if pack.host_sentinel_probes != expected {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::HostSentinelProbeSummaryMismatch,
            "VF dry-run pack.host_sentinel_probes must match nested VfDryRunEvidence Stop summary",
        ));
    }
    None
}

fn verify_native_host_sentinel(pack: &Sep18EvidencePack) -> Option<EvidenceVerifierDecision> {
    if pack.substrate != Sep18ChecklistSubstrate::NativeHostSentinel {
        return None;
    }
    let native = pack.native_host_sentinel.as_ref()?;

    if native.synthetic_fallback_used {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::NativeSentinelSyntheticFallback,
            "native host-sentinel runner must never fall back to synthetic probes",
        ));
    }

    if pack.physical_proof_markers != PhysicalProofMarkers::dry_run_none() {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::NativeSentinelCannotQualifyPhysicalPass,
            "native host-sentinel runner must keep PhysicalProofMarkers::dry_run_none",
        ));
    }

    if native.platform == NativeSentinelRunnerPlatform::NonMacOs
        && (native.live_host_sentinel_collection
            || pack
                .host_sentinel_probes
                .live_host_sentinel_collection_at_stop
            || native.outcome != NativeSentinelRunnerOutcome::UnsupportedPlatform)
    {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::NativeSentinelLiveClaimOnUnsupportedPlatform,
            "non-macOS native host-sentinel runner cannot claim live collection or completed probes",
        ));
    }

    if native.outcome != NativeSentinelRunnerOutcome::NativeProbesCompleted
        && (native.live_host_sentinel_collection
            || native.independent_collection_verified
            || native.checklist_completed
            || pack
                .host_sentinel_probes
                .live_host_sentinel_collection_at_stop)
    {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::NativeSentinelLiveClaimOnUnsupportedPlatform,
            "live native host-sentinel collection requires NativeProbesCompleted",
        ));
    }

    if native.outcome == NativeSentinelRunnerOutcome::NativeProbesCompleted {
        if pack.sealed_evidence.is_none() || native.sealed_evidence.is_none() {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::ChecklistIncomplete,
                "NativeProbesCompleted requires sealed_evidence",
            ));
        }
        if !native.live_host_sentinel_collection
            || !native.independent_collection_verified
            || !native.post_stop_inject_fenced
            || native.channels_destroyed == 0
        {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::ChecklistIncomplete,
                "NativeProbesCompleted requires live native probes, independent collection, Stop fence, and destroyed channels",
            ));
        }
    }

    if let (Some(pack_sealed), Some(native_sealed)) =
        (&pack.sealed_evidence, &native.sealed_evidence)
    {
        if pack_sealed != native_sealed {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::PackSealedEvidenceMismatch,
                "native_host_sentinel.sealed_evidence does not match pack.sealed_evidence",
            ));
        }
    }

    if let (Some(pack_vf), Some(native_vf)) = (&pack.vf_dry_run, &native.vf_dry_run) {
        if pack_vf != native_vf {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::PackSealedEvidenceMismatch,
                "native_host_sentinel.vf_dry_run does not match pack.vf_dry_run",
            ));
        }
    }

    if let Some(decision) = verify_native_live_kind_and_counts(pack, native) {
        return Some(decision);
    }

    None
}

fn verify_native_live_kind_and_counts(
    pack: &Sep18EvidencePack,
    native: &NativeSentinelEvidence,
) -> Option<EvidenceVerifierDecision> {
    let stop = native
        .sealed_evidence
        .as_ref()
        .or(pack.sealed_evidence.as_ref())
        .map(|sealed| &sealed.stop_evidence);
    let probes = &pack.host_sentinel_probes;

    if native.live_host_sentinel_collection != probes.live_host_sentinel_collection_at_stop {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::HostSentinelProbeSummaryMismatch,
            "native live_host_sentinel_collection does not match pack.host_sentinel_probes",
        ));
    }
    if native.channels_destroyed != probes.channels_destroyed {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::HostSentinelProbeSummaryMismatch,
            "native channels_destroyed does not match pack.host_sentinel_probes",
        ));
    }
    if native.host_sentinel_probes_performed != probes.probes_performed {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::HostSentinelProbeSummaryMismatch,
            "native host_sentinel_probes_performed does not match pack.host_sentinel_probes",
        ));
    }
    if native.last_host_sentinel_probe_kind != probes.last_probe_kind {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::HostSentinelProbeSummaryMismatch,
            "native last_host_sentinel_probe_kind does not match pack.host_sentinel_probes",
        ));
    }

    if let Some(stop) = stop {
        if native.live_host_sentinel_collection != stop.live_host_sentinel_collection
            || probes.live_host_sentinel_collection_at_stop != stop.live_host_sentinel_collection
        {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::HostSentinelProbeSummaryMismatch,
                "live_host_sentinel_collection mismatch versus sealed.stop_evidence",
            ));
        }
        if native.channels_destroyed != stop.channels_destroyed
            || probes.channels_destroyed != stop.channels_destroyed
        {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::HostSentinelProbeSummaryMismatch,
                "channels_destroyed mismatch versus sealed.stop_evidence",
            ));
        }
        if native.host_sentinel_probes_performed != stop.host_sentinel_probes_performed
            || probes.probes_performed != stop.host_sentinel_probes_performed
        {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::HostSentinelProbeSummaryMismatch,
                "host_sentinel_probes_performed mismatch versus sealed.stop_evidence",
            ));
        }
        if native.last_host_sentinel_probe_kind != stop.last_host_sentinel_probe_kind
            || probes.last_probe_kind != stop.last_host_sentinel_probe_kind
        {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::HostSentinelProbeSummaryMismatch,
                "last probe kind mismatch versus sealed.stop_evidence",
            ));
        }
        if native.independent_collection_verified != native.live_host_sentinel_collection {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::HostSentinelProbeSummaryMismatch,
                "independent_collection_verified must match live_host_sentinel_collection",
            ));
        }
    } else if native.independent_collection_verified {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::HostSentinelProbeSummaryMismatch,
            "independent_collection_verified requires sealed.stop_evidence",
        ));
    }

    let live_claimed = native.live_host_sentinel_collection
        || probes.live_host_sentinel_collection_at_stop
        || stop
            .map(|s| s.live_host_sentinel_collection)
            .unwrap_or(false);
    if live_claimed {
        let kind = native
            .last_host_sentinel_probe_kind
            .or(probes.last_probe_kind)
            .or(stop.and_then(|s| s.last_host_sentinel_probe_kind));
        match kind {
            Some(HostSentinelProbeKind::NativeMacHost) => {}
            Some(HostSentinelProbeKind::SyntheticRehearsal) => {
                return Some(EvidenceVerifierDecision::reject(
                    EvidenceVerifierCode::NativeSentinelSyntheticFallback,
                    "live native collection cannot be attested by a synthetic last probe kind",
                ));
            }
            None => {
                return Some(EvidenceVerifierDecision::reject(
                    EvidenceVerifierCode::HostSentinelProbeSummaryMismatch,
                    "live native collection requires last probe kind NativeMacHost",
                ));
            }
        }
        if native.last_host_sentinel_probe_kind != Some(HostSentinelProbeKind::NativeMacHost)
            || probes.last_probe_kind != Some(HostSentinelProbeKind::NativeMacHost)
            || stop
                .map(|s| s.last_host_sentinel_probe_kind)
                .unwrap_or(None)
                != Some(HostSentinelProbeKind::NativeMacHost)
        {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::HostSentinelProbeSummaryMismatch,
                "live native collection must bind NativeMacHost across native, pack, and stop_evidence",
            ));
        }
    }

    None
}

fn verify_pack_sealed_consistency(
    pack: &Sep18EvidencePack,
    sealed: &SealedProofEvidence,
) -> Option<EvidenceVerifierDecision> {
    if pack.fault_matrix_case != sealed.fault_matrix_case {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::PackSealedEvidenceMismatch,
            "pack fault_matrix_case does not match sealed_evidence",
        ));
    }

    if let Some(cb) = &pack.contained_browser {
        if pack.sealed_evidence.is_some() {
            match &cb.sealed_evidence {
                None => {
                    return Some(EvidenceVerifierDecision::reject(
                        EvidenceVerifierCode::SubstrateNestedEvidenceMissing,
                        "contained_browser.sealed_evidence required when pack sealed_evidence is present",
                    ));
                }
                Some(cb_sealed) if cb_sealed != sealed => {
                    return Some(EvidenceVerifierDecision::reject(
                        EvidenceVerifierCode::PackSealedEvidenceMismatch,
                        "contained_browser.sealed_evidence does not match pack.sealed_evidence",
                    ));
                }
                _ => {}
            }
        }
    }

    None
}

fn verify_substrate_evidence_class(pack: &Sep18EvidencePack) -> Option<EvidenceVerifierDecision> {
    let expected = match pack.substrate {
        Sep18ChecklistSubstrate::ContainedBrowserDryRun => ProofEvidenceClass::ContainedBrowser,
        Sep18ChecklistSubstrate::VfDryRun => ProofEvidenceClass::VirtualizationFramework,
        Sep18ChecklistSubstrate::SyntheticHarness | Sep18ChecklistSubstrate::NativeHostSentinel => {
            ProofEvidenceClass::Synthetic
        }
    };

    if pack.evidence_class != expected {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::EvidenceClassTampered,
            "pack evidence_class is inconsistent with substrate",
        ));
    }

    if let Some(cb) = &pack.contained_browser {
        if cb.evidence_class != expected {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::EvidenceClassTampered,
                "contained_browser evidence_class is inconsistent with substrate",
            ));
        }
    }

    if let Some(native) = &pack.native_host_sentinel {
        if native.evidence_class != expected {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::EvidenceClassTampered,
                "native_host_sentinel evidence_class is inconsistent with substrate",
            ));
        }
    }

    if let Some(sealed) = &pack.sealed_evidence {
        if sealed.evidence_class != expected {
            return Some(EvidenceVerifierDecision::reject(
                EvidenceVerifierCode::EvidenceClassTampered,
                "sealed_evidence evidence_class is inconsistent with substrate",
            ));
        }
    }

    None
}

fn verify_captured_frame_pack(pack: &Sep18EvidencePack) -> Option<EvidenceVerifierDecision> {
    let cb = pack.contained_browser.as_ref()?;

    let postcondition_verified = cb.sealed_evidence.as_ref().is_some_and(|sealed| {
        sealed
            .checklist_steps
            .contains(&ChecklistStep::PostconditionVerified)
    });

    if postcondition_verified && cb.captured_frames.is_none() {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::CapturedFrameEvidenceInvalid,
            "PostconditionVerified requires sealed captured-frame metadata",
        ));
    }

    let frames = cb.captured_frames.as_ref()?;

    if let Err(err) = validate_public_evidence(&frames.before) {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::CapturedFrameEvidenceInvalid,
            format!("before captured-frame evidence is invalid: {}", err.message),
        ));
    }
    if let Err(err) = validate_public_evidence(&frames.after) {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::CapturedFrameEvidenceInvalid,
            format!("after captured-frame evidence is invalid: {}", err.message),
        ));
    }
    if frames.before.source != CapturedFrameSource::SyntheticSimulator
        || frames.after.source != CapturedFrameSource::SyntheticSimulator
        || frames.before.media_kind != CapturedFrameMediaKind::SyntheticPayload
        || frames.after.media_kind != CapturedFrameMediaKind::SyntheticPayload
    {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::CapturedFrameEvidenceInvalid,
            "sealed captured-frame source must remain synthetic simulator payload",
        ));
    }
    if frames.before.digest == frames.after.digest || frames.before.epoch >= frames.after.epoch {
        return Some(EvidenceVerifierDecision::reject(
            EvidenceVerifierCode::CapturedFrameEvidenceInvalid,
            "sealed captured-frame pair must record a truthful epoch and digest change",
        ));
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
        Sep18ChecklistSubstrate::NativeHostSentinel => NATIVE_HOST_SENTINEL_NONCLAIM.into(),
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
        .unwrap_or_else(HostSentinelProbeSummary::empty);

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
        native_host_sentinel: None,
        contained_browser: Some(evidence),
        vf_dry_run: None,
    }
}

pub fn seal_vf_dry_run_pack(evidence: VfDryRunEvidence) -> Sep18EvidencePack {
    let host_sentinel_probes = HostSentinelProbeSummary::from_vf_dry_run(&evidence);
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
        host_sentinel_probes,
        physical_proof_markers: PhysicalProofMarkers::dry_run_none(),
        checklist_completed: false,
        fault_matrix_case: None,
        sealed_evidence: None,
        native_host_sentinel: None,
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
        native_host_sentinel: None,
        contained_browser: None,
        vf_dry_run: None,
    }
}

pub fn seal_native_host_sentinel_pack(evidence: NativeSentinelEvidence) -> Sep18EvidencePack {
    let host_sentinel_probes = evidence
        .sealed_evidence
        .as_ref()
        .map(|sealed| HostSentinelProbeSummary::from_stop_evidence(&sealed.stop_evidence, 0))
        .unwrap_or(HostSentinelProbeSummary {
            probes_performed: evidence.host_sentinel_probes_performed,
            live_host_sentinel_collection_at_stop: evidence.live_host_sentinel_collection,
            host_sentinels_unchanged_at_stop: false,
            channels_destroyed: evidence.channels_destroyed,
            channels_open_after_stop: 0,
            last_probe_kind: evidence.last_host_sentinel_probe_kind,
        });
    let vf_dry_run = evidence.vf_dry_run.clone();

    Sep18EvidencePack {
        schema_version: EVIDENCE_PACK_SCHEMA_VERSION,
        sealed_at: Utc::now(),
        substrate: Sep18ChecklistSubstrate::NativeHostSentinel,
        evidence_class: evidence.evidence_class,
        physical_pass_claimed: evidence.physical_pass_claimed,
        isolation_pass_claimed: evidence.isolation_pass_claimed,
        vf_pass_claimed: evidence.vf_pass_claimed,
        admission_available: isolated_surface_admission_available(),
        nonclaim: evidence.nonclaim.clone(),
        host_sentinel_probes,
        physical_proof_markers: PhysicalProofMarkers::dry_run_none(),
        checklist_completed: evidence.checklist_completed,
        fault_matrix_case: None,
        sealed_evidence: evidence.sealed_evidence.clone(),
        native_host_sentinel: Some(evidence),
        contained_browser: None,
        vf_dry_run,
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
            EvidenceVerifierCode::HostSentinelProbeSummaryMismatch => 23,
            EvidenceVerifierCode::PackSealedEvidenceMismatch => 24,
            EvidenceVerifierCode::SubstrateNestedEvidenceMissing => 25,
            EvidenceVerifierCode::CapturedFrameEvidenceInvalid => 26,
            EvidenceVerifierCode::NativeSentinelSyntheticFallback => 27,
            EvidenceVerifierCode::NativeSentinelLiveClaimOnUnsupportedPlatform => 28,
            EvidenceVerifierCode::NativeSentinelCannotQualifyPhysicalPass => 29,
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
            EvidenceVerifierCode::VfDryRunCannotQualifyPhysicalPass
        );
    }
}
