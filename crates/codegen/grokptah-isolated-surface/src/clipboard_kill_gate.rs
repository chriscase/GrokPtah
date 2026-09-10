//! Contained Browser clipboard isolation kill-gate (Sep 8–17 Phase-1).
//!
//! Orchestrates [`ClipboardWitness`] and [`WKClipboardProbe`] into one
//! independently reviewable evidence artifact. Computer Mode, bridge
//! admission, VF PASS, and isolation PASS stay false. Synthetic fixtures can
//! prove the verifier; they cannot establish physical Mac Pass.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::clipboard_witness::{
    host_clipboard_unchanged, ClipboardWitness, ClipboardWitnessPlatform, HostClipboardDigest,
};
use crate::error::HarnessResult;
use crate::lifecycle::ProofEvidenceClass;
#[cfg(target_os = "macos")]
use crate::wk_clipboard_probe::{admit_probe_reply, ProbePull, ProbeReply, WKClipboardProbe};
use crate::wk_clipboard_probe::{
    ClipboardOperation, ClipboardProbeFailClosedReason, PageLocalClipboardReceipt,
    ReceiptInitiator, ReplyChannel, ScriptEvaluationPath,
};
use crate::CLIPBOARD_KILL_GATE_NONCLAIM;

/// Where the kill-gate executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardKillGatePlatform {
    MacOs,
    NonMacOs,
}

/// Runner outcome. Never a VF/isolation/physical PASS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardKillGateOutcome {
    UnsupportedPlatform,
    FailClosed,
    IsolationBroken,
    MediationProven,
}

/// Honest kill-gate verdict. Independent of VF/isolation PASS flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardKillGateVerdict {
    Pass,
    Fail,
    Inconclusive,
    Unsupported,
}

/// Evidence provenance. Synthetic fixtures cannot seal Pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardKillGateProvenance {
    NativeWebKit,
    SyntheticVerifierFixture,
}

/// Sealed clipboard kill-gate artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardKillGateEvidence {
    pub platform: ClipboardKillGatePlatform,
    pub outcome: ClipboardKillGateOutcome,
    pub verdict: ClipboardKillGateVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_closed_reason: Option<ClipboardProbeFailClosedReason>,
    pub provenance: ClipboardKillGateProvenance,
    pub script_evaluation_path: ScriptEvaluationPath,
    pub reply_channel: ReplyChannel,
    pub page_world_participated: bool,
    pub private_world_initiated_operations: bool,
    pub page_world_evaluate_javascript_used: bool,
    pub call_async_javascript_used: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_clipboard_before: Option<HostClipboardDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_clipboard_after: Option<HostClipboardDigest>,
    pub host_clipboard_unchanged: bool,
    pub receipts: Vec<PageLocalClipboardReceipt>,
    pub generation: u64,
    pub epoch: u64,
    pub evidence_class: ProofEvidenceClass,
    pub physical_pass_claimed: bool,
    pub isolation_pass_claimed: bool,
    pub vf_pass_claimed: bool,
    pub computer_mode_enabled: bool,
    pub admission_available: bool,
    pub nonclaim: String,
    pub recorded_at: DateTime<Utc>,
}

impl ClipboardKillGateEvidence {
    fn honest_shell(
        platform: ClipboardKillGatePlatform,
        outcome: ClipboardKillGateOutcome,
        verdict: ClipboardKillGateVerdict,
    ) -> Self {
        Self {
            platform,
            outcome,
            verdict,
            fail_closed_reason: None,
            provenance: ClipboardKillGateProvenance::NativeWebKit,
            script_evaluation_path: ScriptEvaluationPath::PrivateContentWorldPull,
            reply_channel: ReplyChannel::PrivateWorldScriptMessageHandler,
            page_world_participated: false,
            private_world_initiated_operations: false,
            page_world_evaluate_javascript_used: false,
            call_async_javascript_used: false,
            host_clipboard_before: None,
            host_clipboard_after: None,
            host_clipboard_unchanged: false,
            receipts: Vec::new(),
            generation: 0,
            epoch: 0,
            evidence_class: ProofEvidenceClass::ContainedBrowser,
            physical_pass_claimed: false,
            isolation_pass_claimed: false,
            vf_pass_claimed: false,
            computer_mode_enabled: false,
            admission_available: crate::isolated_surface_admission_available(),
            nonclaim: CLIPBOARD_KILL_GATE_NONCLAIM.into(),
            recorded_at: Utc::now(),
        }
    }

    pub fn unsupported_non_macos() -> Self {
        let mut evidence = Self::honest_shell(
            ClipboardKillGatePlatform::NonMacOs,
            ClipboardKillGateOutcome::UnsupportedPlatform,
            ClipboardKillGateVerdict::Unsupported,
        );
        evidence.fail_closed_reason = Some(ClipboardProbeFailClosedReason::UnsupportedPlatform);
        evidence
    }

    pub fn macos_fail_closed(reason: ClipboardProbeFailClosedReason, message: &str) -> Self {
        let _ = message;
        let mut evidence = Self::honest_shell(
            ClipboardKillGatePlatform::MacOs,
            ClipboardKillGateOutcome::FailClosed,
            ClipboardKillGateVerdict::Inconclusive,
        );
        evidence.fail_closed_reason = Some(reason);
        evidence
    }

    /// Synthetic fixture for verifier tests. Never a physical Pass.
    pub fn synthetic_verifier_fixture(
        verdict: ClipboardKillGateVerdict,
        reason: Option<ClipboardProbeFailClosedReason>,
    ) -> Self {
        let mut evidence = Self::honest_shell(
            ClipboardKillGatePlatform::NonMacOs,
            match verdict {
                ClipboardKillGateVerdict::Fail => ClipboardKillGateOutcome::IsolationBroken,
                ClipboardKillGateVerdict::Pass => ClipboardKillGateOutcome::MediationProven,
                ClipboardKillGateVerdict::Unsupported => {
                    ClipboardKillGateOutcome::UnsupportedPlatform
                }
                ClipboardKillGateVerdict::Inconclusive => ClipboardKillGateOutcome::FailClosed,
            },
            verdict,
        );
        evidence.provenance = ClipboardKillGateProvenance::SyntheticVerifierFixture;
        evidence.fail_closed_reason = reason;
        evidence
    }
}

/// Derived Pass contract. Any uncertainty or synthetic provenance fails closed.
pub fn clipboard_kill_gate_may_claim_pass(evidence: &ClipboardKillGateEvidence) -> bool {
    evidence.platform == ClipboardKillGatePlatform::MacOs
        && evidence.outcome == ClipboardKillGateOutcome::MediationProven
        && evidence.verdict == ClipboardKillGateVerdict::Pass
        && evidence.provenance == ClipboardKillGateProvenance::NativeWebKit
        && evidence.script_evaluation_path == ScriptEvaluationPath::PrivateContentWorldPull
        && evidence.reply_channel == ReplyChannel::PrivateWorldScriptMessageHandler
        && evidence.page_world_participated
        && !evidence.private_world_initiated_operations
        && evidence
            .receipts
            .iter()
            .all(|receipt| receipt.initiator == ReceiptInitiator::PageWorld)
        && !evidence.page_world_evaluate_javascript_used
        && !evidence.call_async_javascript_used
        && evidence.host_clipboard_unchanged
        && evidence.host_clipboard_before.is_some()
        && evidence.host_clipboard_after.is_some()
        && evidence
            .host_clipboard_before
            .as_ref()
            .zip(evidence.host_clipboard_after.as_ref())
            .is_some_and(|(before, after)| host_clipboard_unchanged(before, after))
        && evidence.fail_closed_reason.is_none()
        && evidence.generation > 0
        && evidence.epoch > 0
        && evidence.receipts.len() == ClipboardOperation::ALL.len()
        && !evidence.physical_pass_claimed
        && !evidence.isolation_pass_claimed
        && !evidence.vf_pass_claimed
        && !evidence.computer_mode_enabled
        && !evidence.admission_available
        && evidence.evidence_class == ProofEvidenceClass::ContainedBrowser
}

/// Independent machine verifier for a sealed kill-gate artifact.
pub fn verify_clipboard_kill_gate_evidence(
    evidence: &ClipboardKillGateEvidence,
) -> Result<(), ClipboardProbeFailClosedReason> {
    if evidence.admission_available || evidence.computer_mode_enabled {
        return Err(ClipboardProbeFailClosedReason::Uncertain);
    }
    if evidence.physical_pass_claimed || evidence.isolation_pass_claimed || evidence.vf_pass_claimed
    {
        return Err(ClipboardProbeFailClosedReason::Uncertain);
    }
    if evidence.page_world_evaluate_javascript_used || evidence.call_async_javascript_used {
        return Err(ClipboardProbeFailClosedReason::ForbiddenScriptEvaluation);
    }
    if evidence.script_evaluation_path != ScriptEvaluationPath::PrivateContentWorldPull {
        return Err(ClipboardProbeFailClosedReason::ForbiddenScriptEvaluation);
    }

    if evidence.provenance == ClipboardKillGateProvenance::SyntheticVerifierFixture
        && evidence.verdict == ClipboardKillGateVerdict::Pass
    {
        return Err(ClipboardProbeFailClosedReason::Uncertain);
    }

    match evidence.platform {
        ClipboardKillGatePlatform::NonMacOs => {
            if evidence.verdict == ClipboardKillGateVerdict::Pass
                || evidence.outcome == ClipboardKillGateOutcome::MediationProven
            {
                return Err(ClipboardProbeFailClosedReason::UnsupportedPlatform);
            }
            if evidence.provenance == ClipboardKillGateProvenance::NativeWebKit
                && evidence.outcome != ClipboardKillGateOutcome::UnsupportedPlatform
            {
                return Err(ClipboardProbeFailClosedReason::UnsupportedPlatform);
            }
        }
        ClipboardKillGatePlatform::MacOs => {
            if evidence.outcome == ClipboardKillGateOutcome::UnsupportedPlatform {
                return Err(ClipboardProbeFailClosedReason::Uncertain);
            }
        }
    }

    if evidence.verdict == ClipboardKillGateVerdict::Pass {
        if evidence.reply_channel != ReplyChannel::PrivateWorldScriptMessageHandler {
            return Err(ClipboardProbeFailClosedReason::UntrustedReplyChannel);
        }
        if evidence.private_world_initiated_operations
            || evidence
                .receipts
                .iter()
                .any(|receipt| receipt.initiator != ReceiptInitiator::PageWorld)
        {
            return Err(ClipboardProbeFailClosedReason::PrivateWorldGeneratedReceipts);
        }
        if !evidence.page_world_participated {
            return Err(ClipboardProbeFailClosedReason::PageWorldNonparticipation);
        }
    }

    if let Some(reason) = evidence.fail_closed_reason {
        if evidence.verdict == ClipboardKillGateVerdict::Pass {
            return Err(reason);
        }
    }

    if evidence.verdict == ClipboardKillGateVerdict::Pass
        && !clipboard_kill_gate_may_claim_pass(evidence)
    {
        return Err(evidence
            .fail_closed_reason
            .unwrap_or(ClipboardProbeFailClosedReason::Uncertain));
    }

    if evidence.host_clipboard_before.is_some() && evidence.host_clipboard_after.is_some() {
        let before = evidence.host_clipboard_before.as_ref().expect("before");
        let after = evidence.host_clipboard_after.as_ref().expect("after");
        if !host_clipboard_unchanged(before, after) {
            if evidence.verdict == ClipboardKillGateVerdict::Pass {
                return Err(ClipboardProbeFailClosedReason::UnexpectedHostClipboardChange);
            }
        } else if evidence.verdict == ClipboardKillGateVerdict::Fail
            && evidence.outcome != ClipboardKillGateOutcome::IsolationBroken
        {
            return Err(ClipboardProbeFailClosedReason::Uncertain);
        }
    }

    if evidence.verdict == ClipboardKillGateVerdict::Pass {
        #[cfg(not(target_os = "macos"))]
        {
            return Err(ClipboardProbeFailClosedReason::UnsupportedPlatform);
        }
        #[cfg(target_os = "macos")]
        {
            let pull = ProbePull::private(evidence.generation, evidence.epoch);
            let reply = ProbeReply {
                generation: evidence.generation,
                epoch: evidence.epoch,
                world: crate::wk_clipboard_probe::ContentWorld::PrivateProbe,
                path: evidence.script_evaluation_path,
                reply_channel: evidence.reply_channel,
                page_world_participated: evidence.page_world_participated,
                private_world_initiated_operations: evidence.private_world_initiated_operations,
                receipts: evidence.receipts.clone(),
            };
            admit_probe_reply(&pull, &reply, &[]).map(|_| ())?;
            if !clipboard_kill_gate_may_claim_pass(evidence) {
                return Err(ClipboardProbeFailClosedReason::Uncertain);
            }
        }
    }

    Ok(())
}

/// Run the kill-gate. Linux is deterministic unsupported. macOS attempts a
/// native private-world probe and seals an honest verdict.
pub fn run_clipboard_kill_gate() -> HarnessResult<ClipboardKillGateEvidence> {
    match ClipboardWitness::platform() {
        ClipboardWitnessPlatform::NonMacOs => {
            Ok(ClipboardKillGateEvidence::unsupported_non_macos())
        }
        ClipboardWitnessPlatform::MacOs => run_macos_kill_gate(),
    }
}

#[cfg(not(target_os = "macos"))]
fn run_macos_kill_gate() -> HarnessResult<ClipboardKillGateEvidence> {
    Ok(ClipboardKillGateEvidence::unsupported_non_macos())
}

#[cfg(target_os = "macos")]
fn run_macos_kill_gate() -> HarnessResult<ClipboardKillGateEvidence> {
    let generation = crate::wk_clipboard_probe::host_issued_generation();
    let epoch = 1;
    let before = match ClipboardWitness::seal_digest() {
        Ok(digest) => digest,
        Err(err) => {
            return Ok(ClipboardKillGateEvidence::macos_fail_closed(
                crate::wk_clipboard_probe::fail_closed_reason_from_harness(&err),
                &err.message,
            ));
        }
    };

    let pull = ProbePull::private(generation, epoch);
    let reply = match WKClipboardProbe::pull(&pull) {
        Ok(reply) => reply,
        Err(err) => {
            return Ok(ClipboardKillGateEvidence::macos_fail_closed(
                crate::wk_clipboard_probe::fail_closed_reason_from_harness(&err),
                &err.message,
            ));
        }
    };

    let after = match ClipboardWitness::seal_digest() {
        Ok(digest) => digest,
        Err(err) => {
            return Ok(ClipboardKillGateEvidence::macos_fail_closed(
                crate::wk_clipboard_probe::fail_closed_reason_from_harness(&err),
                &err.message,
            ));
        }
    };

    seal_macos_result(generation, epoch, before, after, reply)
}

#[cfg(target_os = "macos")]
fn seal_macos_result(
    generation: u64,
    epoch: u64,
    before: HostClipboardDigest,
    after: HostClipboardDigest,
    reply: ProbeReply,
) -> HarnessResult<ClipboardKillGateEvidence> {
    let unchanged = host_clipboard_unchanged(&before, &after);
    if !unchanged {
        let mut evidence = ClipboardKillGateEvidence::honest_shell(
            ClipboardKillGatePlatform::MacOs,
            ClipboardKillGateOutcome::IsolationBroken,
            ClipboardKillGateVerdict::Fail,
        );
        evidence.fail_closed_reason =
            Some(ClipboardProbeFailClosedReason::UnexpectedHostClipboardChange);
        evidence.host_clipboard_before = Some(before);
        evidence.host_clipboard_after = Some(after);
        evidence.host_clipboard_unchanged = false;
        evidence.generation = generation;
        evidence.epoch = epoch;
        evidence.receipts = reply.receipts;
        evidence.script_evaluation_path = reply.path;
        evidence.reply_channel = reply.reply_channel;
        evidence.page_world_participated = reply.page_world_participated;
        evidence.private_world_initiated_operations = reply.private_world_initiated_operations;
        evidence.page_world_evaluate_javascript_used =
            reply.path == ScriptEvaluationPath::PageWorldEvaluateJavaScript;
        evidence.call_async_javascript_used =
            reply.path == ScriptEvaluationPath::CallAsyncJavaScript;
        return Ok(evidence);
    }

    match admit_probe_reply(&ProbePull::private(generation, epoch), &reply, &[]) {
        Ok(admitted) => {
            let mut evidence = ClipboardKillGateEvidence::honest_shell(
                ClipboardKillGatePlatform::MacOs,
                ClipboardKillGateOutcome::MediationProven,
                ClipboardKillGateVerdict::Pass,
            );
            evidence.host_clipboard_before = Some(before);
            evidence.host_clipboard_after = Some(after);
            evidence.host_clipboard_unchanged = true;
            evidence.generation = admitted.generation;
            evidence.epoch = admitted.epoch;
            evidence.receipts = admitted.receipts;
            evidence.script_evaluation_path = ScriptEvaluationPath::PrivateContentWorldPull;
            evidence.reply_channel = ReplyChannel::PrivateWorldScriptMessageHandler;
            evidence.page_world_participated = true;
            evidence.private_world_initiated_operations = false;
            if !clipboard_kill_gate_may_claim_pass(&evidence) {
                evidence.outcome = ClipboardKillGateOutcome::FailClosed;
                evidence.verdict = ClipboardKillGateVerdict::Inconclusive;
                evidence.fail_closed_reason = Some(ClipboardProbeFailClosedReason::Uncertain);
            }
            Ok(evidence)
        }
        Err(reason) => {
            let mut evidence = ClipboardKillGateEvidence::macos_fail_closed(reason, "probe reply");
            evidence.host_clipboard_before = Some(before);
            evidence.host_clipboard_after = Some(after);
            evidence.host_clipboard_unchanged = unchanged;
            evidence.generation = generation;
            evidence.epoch = epoch;
            evidence.receipts = reply.receipts;
            evidence.script_evaluation_path = reply.path;
            evidence.reply_channel = reply.reply_channel;
            evidence.page_world_participated = reply.page_world_participated;
            evidence.private_world_initiated_operations = reply.private_world_initiated_operations;
            evidence.page_world_evaluate_javascript_used =
                reply.path == ScriptEvaluationPath::PageWorldEvaluateJavaScript;
            evidence.call_async_javascript_used =
                reply.path == ScriptEvaluationPath::CallAsyncJavaScript;
            Ok(evidence)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_runner_is_unsupported_and_never_pass() {
        let evidence = run_clipboard_kill_gate().expect("artifact");
        assert!(!evidence.physical_pass_claimed);
        assert!(!evidence.isolation_pass_claimed);
        assert!(!evidence.vf_pass_claimed);
        assert!(!evidence.computer_mode_enabled);
        assert!(!evidence.admission_available);
        assert_eq!(evidence.nonclaim, CLIPBOARD_KILL_GATE_NONCLAIM);
        assert!(!clipboard_kill_gate_may_claim_pass(&evidence));
        verify_clipboard_kill_gate_evidence(&evidence).expect("linux honest");
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(evidence.platform, ClipboardKillGatePlatform::NonMacOs);
            assert_eq!(
                evidence.outcome,
                ClipboardKillGateOutcome::UnsupportedPlatform
            );
            assert_eq!(evidence.verdict, ClipboardKillGateVerdict::Unsupported);
            assert_ne!(evidence.verdict, ClipboardKillGateVerdict::Pass);
        }
    }

    #[test]
    fn synthetic_pass_fixture_is_rejected() {
        let evidence = ClipboardKillGateEvidence::synthetic_verifier_fixture(
            ClipboardKillGateVerdict::Pass,
            None,
        );
        let err = verify_clipboard_kill_gate_evidence(&evidence).expect_err("synthetic pass");
        assert_eq!(err, ClipboardProbeFailClosedReason::Uncertain);
        assert!(!clipboard_kill_gate_may_claim_pass(&evidence));
    }
}
