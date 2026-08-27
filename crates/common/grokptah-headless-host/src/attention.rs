//! Needs-attention escalation.
//!
//! A headless host has no human at the keyboard, so a run that reaches a
//! decision it cannot make on its own stops and raises an escalation. The run
//! stays halted until an operator resolves it. Nothing auto-approves: an
//! escalation left past its deadline resolves to *deny*, never to allow.

use serde::{Deserialize, Serialize};

use crate::error::{HostError, HostResult};
use crate::identity::opaque_id;
use crate::redaction::RedactionPolicy;

/// Maximum bytes retained in an escalation detail.
pub const MAX_ATTENTION_DETAIL_BYTES: usize = 512;

/// Why a run stopped and asked for an operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    /// The run needs permission the host cannot grant on its own.
    PermissionRequired,
    /// The run asked for a capability this host does not hold.
    CapabilityDenied,
    /// The engine failed in a way that may be recoverable by an operator.
    EngineFailure,
    /// The run needs an explicit decision after restart recovery.
    RecoveryRequired,
    /// A dispatch may or may not have reached its destination. The run must not
    /// move until a human reconciles it, because retrying could repeat work
    /// that already happened.
    DispatchUncertain,
}

impl AttentionKind {
    /// Stable label for events and receipts.
    pub fn label(self) -> &'static str {
        match self {
            Self::PermissionRequired => "permission_required",
            Self::CapabilityDenied => "capability_denied",
            Self::EngineFailure => "engine_failure",
            Self::RecoveryRequired => "recovery_required",
            Self::DispatchUncertain => "dispatch_uncertain",
        }
    }
}

/// How an operator answered an escalation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionResolution {
    /// Let the run continue. Requires the human-gated capability.
    Allow,
    /// Refuse. The run fails with an explicit reason.
    Deny,
}

/// One raised, unresolved escalation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionRecord {
    /// Opaque escalation identity.
    pub attention_id: String,
    /// Why the run stopped.
    pub kind: AttentionKind,
    /// Stable machine-readable reason.
    pub reason_code: String,
    /// Bounded, redacted operator-facing detail.
    pub detail: String,
    /// When the escalation was raised, RFC3339.
    pub raised_at: String,
    /// Deadline in epoch milliseconds, after which it is denied.
    pub expires_at_ms: u64,
}

impl AttentionRecord {
    /// Raise a bounded, redacted escalation.
    pub fn raise(
        redaction: &RedactionPolicy,
        run_id: &str,
        kind: AttentionKind,
        reason_code: &str,
        detail: &str,
        raised_at: String,
        now_ms: u64,
        ttl_ms: u64,
    ) -> HostResult<Self> {
        if reason_code.trim().is_empty() || reason_code.len() > 64 {
            return Err(HostError::invalid(
                "attention_reason_invalid",
                "escalation reason must be non-empty and bounded",
            ));
        }
        let (detail, _) = redaction.scrub_bounded(detail, MAX_ATTENTION_DETAIL_BYTES);
        Ok(Self {
            attention_id: opaque_id("att", &[run_id, reason_code, &now_ms.to_string()]),
            kind,
            reason_code: reason_code.to_owned(),
            detail,
            raised_at,
            expires_at_ms: now_ms.saturating_add(ttl_ms),
        })
    }

    /// Whether the escalation has passed its deadline.
    pub fn is_expired(&self, now_ms: u64) -> bool {
        now_ms >= self.expires_at_ms
    }

    /// Check that a resolution targets this exact escalation.
    pub fn ensure_matches(&self, attention_id: &str) -> HostResult<()> {
        if self.attention_id == attention_id {
            Ok(())
        } else {
            Err(HostError::stale(
                "attention_stale",
                "the escalation identity does not match the run's open escalation",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raise(detail: &str) -> AttentionRecord {
        AttentionRecord::raise(
            &RedactionPolicy::new("/hosts/home", "/hosts/project"),
            "run-1",
            AttentionKind::PermissionRequired,
            "shell_write_requested",
            detail,
            "2026-01-01T00:00:00.000Z".into(),
            1_000,
            5_000,
        )
        .expect("escalation raised")
    }

    #[test]
    fn escalation_detail_is_redacted_and_bounded() {
        let record =
            raise("wants /hosts/project/src with XAI_API_KEY=xai-abcdefghijklmnopqrstuvwxyz01");
        assert!(record.detail.contains("<workspace>/src"));
        assert!(!record.detail.contains("abcdefghijklmnopqrstuvwxyz"));
        assert!(record.detail.len() <= MAX_ATTENTION_DETAIL_BYTES);
        assert!(record.attention_id.starts_with("att-"));
    }

    #[test]
    fn deadlines_and_identity_fences_hold() {
        let record = raise("needs an operator");
        assert!(!record.is_expired(5_999));
        assert!(record.is_expired(6_000));
        record
            .ensure_matches(&record.attention_id)
            .expect("matches");
        assert_eq!(
            record
                .ensure_matches("att-other")
                .expect_err("mismatch is refused")
                .reason_code(),
            "attention_stale"
        );
    }

    #[test]
    fn a_malformed_reason_is_refused() {
        let error = AttentionRecord::raise(
            &RedactionPolicy::bare(),
            "run-1",
            AttentionKind::EngineFailure,
            "   ",
            "detail",
            "2026-01-01T00:00:00.000Z".into(),
            0,
            1,
        )
        .expect_err("blank reason is refused");
        assert_eq!(error.reason_code(), "attention_reason_invalid");
    }
}
