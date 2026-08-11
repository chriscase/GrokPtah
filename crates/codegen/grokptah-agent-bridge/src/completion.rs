//! Evidence-backed completion summaries shared by the desktop and MCP paths.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::events::{SessionUpdate, ToolCallStatus};
use crate::orchestration::is_recognized_test_command;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CompletionClaims {
    pub present: bool,
    pub mentions_changes: bool,
    pub mentions_tests: bool,
    pub mentions_verification: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CompletionObservations {
    pub changed_files: u32,
    pub tests_observed: u32,
    pub tests_passed: u32,
    pub tests_failed: u32,
    pub tests_incomplete: u32,
    pub permissions_requested: u32,
    pub permissions_granted: u32,
    pub permissions_denied: u32,
    pub permissions_unresolved: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CompletionUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub requests: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CompletionEvidence {
    /// verified | unverified | failed | incomplete
    pub status: String,
    pub stop_reason: String,
    pub interrupted: bool,
    pub claims: CompletionClaims,
    pub observations: CompletionObservations,
    pub usage: CompletionUsage,
}

pub(crate) fn claims_from_response(response: Option<&str>) -> CompletionClaims {
    let Some(text) = response.map(str::trim).filter(|text| !text.is_empty()) else {
        return CompletionClaims::default();
    };
    let lower = text.to_ascii_lowercase();
    CompletionClaims {
        present: true,
        mentions_changes: [
            "changed",
            "modified",
            "created",
            "removed",
            "edited",
            "files changed",
            "no files changed",
        ]
        .iter()
        .any(|needle| lower.contains(needle)),
        mentions_tests: lower.contains("test") || lower.contains("verification"),
        mentions_verification: ["verified", "passed", "green", "verification"]
            .iter()
            .any(|needle| lower.contains(needle)),
    }
}

/// Observe only typed bridge events. Raw tool output and model prose are not
/// treated as authoritative evidence.
pub(crate) fn observe_updates(updates: &[&SessionUpdate]) -> CompletionObservations {
    let mut changed_paths = HashSet::new();
    let mut test_calls = HashSet::new();
    let mut pending_permissions = 0u32;
    let mut observations = CompletionObservations::default();

    for update in updates {
        match update {
            SessionUpdate::FileEdit { path, .. } => {
                changed_paths.insert(path.clone());
            }
            SessionUpdate::ShellSessionStarted {
                call_id, command, ..
            } if is_recognized_test_command(command) => {
                if test_calls.insert(call_id.clone()) {
                    observations.tests_observed += 1;
                }
            }
            SessionUpdate::ShellSessionEnded {
                call_id,
                exit_code,
                cancelled,
                ..
            } if test_calls.contains(call_id) => {
                if *cancelled || exit_code.is_none() {
                    observations.tests_incomplete += 1;
                } else if *exit_code == Some(0) {
                    observations.tests_passed += 1;
                } else {
                    observations.tests_failed += 1;
                }
            }
            SessionUpdate::PermissionRequired { .. } => {
                observations.permissions_requested += 1;
                pending_permissions = pending_permissions.saturating_add(1);
            }
            SessionUpdate::ToolCall { status, .. }
                if pending_permissions > 0
                    && matches!(status, ToolCallStatus::Running | ToolCallStatus::Denied) =>
            {
                pending_permissions = pending_permissions.saturating_sub(1);
                if *status == ToolCallStatus::Denied {
                    observations.permissions_denied += 1;
                } else {
                    observations.permissions_granted += 1;
                }
            }
            _ => {}
        }
    }

    observations.changed_files = changed_paths.len() as u32;
    observations.permissions_unresolved = pending_permissions;
    observations
}

pub(crate) fn observations_from_run(
    changed_files: usize,
    tests: impl IntoIterator<Item = (Option<i32>, Option<bool>)>,
    permissions_requested: u32,
    permissions_granted: u32,
    permissions_denied: u32,
) -> CompletionObservations {
    let mut observations = CompletionObservations {
        changed_files: changed_files as u32,
        permissions_requested,
        permissions_granted,
        permissions_denied,
        permissions_unresolved: permissions_requested
            .saturating_sub(permissions_granted.saturating_add(permissions_denied)),
        ..CompletionObservations::default()
    };
    for (exit_code, cancelled) in tests {
        observations.tests_observed += 1;
        if cancelled == Some(true) || exit_code.is_none() {
            observations.tests_incomplete += 1;
        } else if exit_code == Some(0) {
            observations.tests_passed += 1;
        } else {
            observations.tests_failed += 1;
        }
    }
    observations
}

pub(crate) fn build_evidence(
    outcome: &str,
    final_response: Option<&str>,
    observations: CompletionObservations,
    usage: CompletionUsage,
    interrupted: bool,
) -> CompletionEvidence {
    let claims = claims_from_response(final_response);
    let status = if outcome == "failed" || observations.tests_failed > 0 {
        "failed"
    } else if interrupted
        || matches!(outcome, "cancelled" | "interrupted" | "limit_reached")
        || !claims.present
    {
        "incomplete"
    } else if observations.tests_observed == 0
        || observations.tests_incomplete > 0
        || observations.permissions_unresolved > 0
        || (observations.changed_files > 0 && !claims.mentions_changes)
        || (observations.tests_observed > 0
            && (!claims.mentions_tests || !claims.mentions_verification))
    {
        "unverified"
    } else {
        "verified"
    };

    CompletionEvidence {
        status: status.into(),
        stop_reason: outcome.into(),
        interrupted,
        claims,
        observations,
        usage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::ToolCallKind;
    use uuid::Uuid;

    #[test]
    fn claims_are_conservative_and_redacted() {
        let claims = claims_from_response(Some(
            "Changed src/lib.rs; cargo test passed and verification is green.",
        ));
        assert!(claims.present);
        assert!(claims.mentions_changes);
        assert!(claims.mentions_tests);
        assert!(claims.mentions_verification);
    }

    #[test]
    fn typed_events_produce_authoritative_observations() {
        let session_id = Uuid::new_v4();
        let updates = vec![
            SessionUpdate::FileEdit {
                session_id,
                path: "src/lib.rs".into(),
                summary: "updated".into(),
                unified_diff: String::new(),
            },
            SessionUpdate::ShellSessionStarted {
                session_id,
                call_id: "test-1".into(),
                command: "cargo test".into(),
            },
            SessionUpdate::ShellSessionEnded {
                session_id,
                call_id: "test-1".into(),
                exit_code: Some(0),
                cancelled: false,
            },
            SessionUpdate::PermissionRequired {
                session_id,
                request: crate::permission::PermissionRequest {
                    id: Uuid::new_v4(),
                    session_id,
                    tool_name: "apply_patch".into(),
                    summary: "edit".into(),
                    detail: serde_json::json!({}),
                },
            },
            SessionUpdate::ToolCall {
                session_id,
                call_id: "edit-1".into(),
                title: "apply_patch".into(),
                kind: ToolCallKind::Edit,
                status: ToolCallStatus::Denied,
                input: serde_json::json!({}),
            },
        ];
        let refs = updates.iter().collect::<Vec<_>>();
        let observations = observe_updates(&refs);
        assert_eq!(observations.changed_files, 1);
        assert_eq!(observations.tests_passed, 1);
        assert_eq!(observations.permissions_requested, 1);
        assert_eq!(observations.permissions_granted, 0);
        assert_eq!(observations.permissions_denied, 1);
        assert_eq!(observations.permissions_unresolved, 0);
    }

    #[test]
    fn verification_requires_test_and_claim_evidence() {
        let observations = CompletionObservations {
            changed_files: 1,
            tests_observed: 1,
            tests_passed: 1,
            ..CompletionObservations::default()
        };
        let evidence = build_evidence(
            "completed",
            Some("Changed src/lib.rs; cargo test passed; verification green."),
            observations.clone(),
            CompletionUsage::default(),
            false,
        );
        assert_eq!(evidence.status, "verified");

        let incomplete_claims = build_evidence(
            "completed",
            Some("Implemented the fix."),
            observations,
            CompletionUsage::default(),
            false,
        );
        assert_eq!(incomplete_claims.status, "unverified");
    }
}
