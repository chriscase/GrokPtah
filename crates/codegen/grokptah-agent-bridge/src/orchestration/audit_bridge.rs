//! Canonical `AuditEntry` -> `grokptah-audit.v2` adapter (#462).
//!
//! The shipped orchestration producers keep their existing call shape. This
//! module is the one place that translates it into the authenticated record,
//! and it is where the v1 ledger's privacy defects are closed:
//!
//! - `workspace` was a real filesystem path, redacted only for registered
//!   secrets. It becomes an opaque keyed digest; the path never reaches the
//!   journal, an export, or a health projection.
//! - `detail` was free text and, on the rejected path, carried
//!   `OrchError::message`, which can contain paths and IO strings. It is not
//!   carried into the durable record at all. Everything an operator needs to
//!   identify the event survives as `(op, outcome, code, reason)`, and the
//!   human message still reaches the local process log.
//! - An unrecognised outcome resolves to `Uncertain`, never `Accepted`: the
//!   ledger must not upgrade something it could not classify.

use crate::audit::{AuditEntryInput, EntryOutcome, EntryPhase, EntryReason};

use super::types::{AuditEntry, AuditPhase};

/// Map a producer outcome string onto the closed vocabulary.
///
/// Anything unrecognised is `Uncertain` by design.
fn outcome_of(raw: &str) -> EntryOutcome {
    match raw {
        "accepted" | "ok" | "success" | "allowed" => EntryOutcome::Accepted,
        "rejected" | "denied" | "failed" | "error" => EntryOutcome::Rejected,
        _ => EntryOutcome::Uncertain,
    }
}

/// Map a producer error code onto the closed reason vocabulary.
///
/// Codes outside the mapping are not lost: `AuditRecord::code` keeps the exact
/// string, constrained to a shape that cannot carry a path or a secret.
fn reason_of(code: Option<&str>) -> Option<EntryReason> {
    Some(match code? {
        "unauthenticated" => EntryReason::Unauthenticated,
        "forbidden_scope" => EntryReason::ForbiddenScope,
        "workspace_mismatch" => EntryReason::WorkspaceMismatch,
        "session_busy" => EntryReason::SessionBusy,
        "capacity_exhausted" => EntryReason::CapacityExhausted,
        "stale_version" => EntryReason::StaleRevision,
        "cursor_expired" => EntryReason::CursorExpired,
        "timeout" => EntryReason::Timeout,
        "invalid_request" => EntryReason::InvalidRequest,
        "unsupported" => EntryReason::Unsupported,
        "conflict" => EntryReason::Conflict,
        "internal" | "promotion_conflict" | "run_persistence_failed" => EntryReason::Internal,
        _ => return None,
    })
}

/// Stable producer intent identity.
///
/// Falls back through the strongest identity the producer actually has, so an
/// intent recorded on one process and its outcome recorded on the next
/// correlate without any call-site changes at sites that already carry a
/// request id.
fn producer_identity(entry: &AuditEntry) -> Option<String> {
    if let Some(intent) = entry.intent_id.as_deref() {
        return Some(intent.to_string());
    }
    if let Some(request) = entry.request_id.as_deref() {
        return Some(request.to_string());
    }
    entry.session_id.map(|id| id.to_string())
}

pub(crate) fn to_input(entry: &AuditEntry) -> AuditEntryInput {
    let mut input = AuditEntryInput::new(
        entry.tool.as_str(),
        match entry.phase {
            AuditPhase::Intent => EntryPhase::Intent,
            AuditPhase::Outcome => EntryPhase::Outcome,
        },
        outcome_of(&entry.outcome),
    );
    input.reason = reason_of(entry.error_code.as_deref());
    input.code = entry.error_code.clone();
    input.producer = producer_identity(entry);
    input.actor = entry.session_id.map(|id| id.to_string());
    input.request = entry.request_id.clone();
    // The workspace path is digested, never stored.
    input.scope = entry.workspace.clone();
    input
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrecognised_outcomes_never_become_accepted() {
        assert_eq!(outcome_of("retrying"), EntryOutcome::Uncertain);
        assert_eq!(outcome_of("promotion_conflict"), EntryOutcome::Uncertain);
        assert_eq!(outcome_of(""), EntryOutcome::Uncertain);
        assert_eq!(outcome_of("accepted"), EntryOutcome::Accepted);
        assert_eq!(outcome_of("rejected"), EntryOutcome::Rejected);
    }

    #[test]
    fn every_shipped_error_code_maps_to_a_reason() {
        use super::super::types::OrchErrorCode::*;
        for code in [
            Unauthenticated,
            ForbiddenScope,
            WorkspaceMismatch,
            SessionBusy,
            CapacityExhausted,
            StaleVersion,
            CursorExpired,
            Internal,
            Timeout,
            InvalidRequest,
            Unsupported,
            Conflict,
        ] {
            assert!(
                reason_of(Some(code.as_str())).is_some(),
                "{} has no reason mapping",
                code.as_str()
            );
        }
        // Unmapped codes are still preserved verbatim in `AuditRecord::code`.
        assert!(reason_of(Some("some_future_code")).is_none());
    }

    #[test]
    fn producer_identity_prefers_the_most_specific_available_id() {
        let mut entry = AuditEntry {
            ts: chrono::Utc::now(),
            tool: "ptah_submit_task".into(),
            request_id: Some("req-1".into()),
            session_id: Some(uuid::Uuid::nil()),
            workspace: Some("/private/workspace".into()),
            outcome: "accepted".into(),
            error_code: None,
            detail: "unused".into(),
            intent_id: Some("run-42".into()),
            phase: AuditPhase::Outcome,
        };
        assert_eq!(producer_identity(&entry).as_deref(), Some("run-42"));
        entry.intent_id = None;
        assert_eq!(producer_identity(&entry).as_deref(), Some("req-1"));
        entry.request_id = None;
        assert_eq!(
            producer_identity(&entry).as_deref(),
            Some(uuid::Uuid::nil().to_string().as_str())
        );
        entry.session_id = None;
        assert!(producer_identity(&entry).is_none());
    }
}
