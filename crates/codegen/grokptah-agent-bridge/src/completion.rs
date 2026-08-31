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
    /// Optional binding to the Work this evidence claims to complete.
    /// Absent on historical run records; required for success authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
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
        ..Default::default()
    }
}

/// Re-check the completion oracle/claims rules. Caller-supplied `status`
/// is not trusted: failed tests, missing claims, or incomplete stops never
/// authorize success even if the status string says `verified`.
pub(crate) fn evidence_authorizes_success(evidence: &CompletionEvidence) -> bool {
    if evidence.status != "verified" || evidence.stop_reason != "completed" || evidence.interrupted
    {
        return false;
    }
    let observations = &evidence.observations;
    let claims = &evidence.claims;
    if observations.tests_failed > 0 {
        return false;
    }
    if !claims.present {
        return false;
    }
    if observations.tests_observed == 0
        || observations.tests_passed == 0
        || observations.tests_incomplete > 0
        || observations.permissions_unresolved > 0
        || (observations.changed_files > 0 && !claims.mentions_changes)
        || (observations.tests_observed > 0
            && (!claims.mentions_tests || !claims.mentions_verification))
    {
        return false;
    }
    true
}

/// Append an evidence-backed trailer when the model final text omits observed
/// changes or test results. Only reports paths/outcomes actually observed —
/// never invents work.
///
/// Incomplete/limit stop *reasons* are preserved as the leading text; when the
/// turn still produced edits or test outcomes we still append that evidence so
/// terminal handoffs remain honest (recovery stops often land after green
/// cargo but before a prose final).
pub fn enrich_terminal_handoff(
    model_text: &str,
    changed_paths: &[String],
    tests_passed: Option<bool>,
    incomplete_stop: bool,
) -> String {
    let claims = claims_from_response(Some(model_text));
    let mut extras: Vec<String> = Vec::new();

    // For incomplete stops, only add evidence that was actually observed —
    // never invent a "no files changed" claim over a limit-stop reason.
    let need_changes = !changed_paths.is_empty() && !claims.mentions_changes;
    if need_changes {
        const MAX_PATHS: usize = 12;
        let mut paths: Vec<&str> = changed_paths.iter().map(String::as_str).collect();
        paths.sort();
        paths.dedup();
        let total = paths.len();
        paths.truncate(MAX_PATHS);
        let joined = paths.join(", ");
        if total > MAX_PATHS {
            extras.push(format!(
                "Changed files: {joined}, … (+{} more).",
                total - MAX_PATHS
            ));
        } else {
            extras.push(format!("Changed files: {joined}."));
        }
    } else if !incomplete_stop
        && changed_paths.is_empty()
        && claims.present
        && !claims.mentions_changes
        && !model_text.to_ascii_lowercase().contains("no files changed")
    {
        extras.push("No files changed.".into());
    }

    // Avoid treating "test-recovery" in stop text as a real verification claim.
    let lower = model_text.to_ascii_lowercase();
    let has_real_test_claim = claims.mentions_tests
        && (lower.contains("cargo test")
            || lower.contains("tests passed")
            || lower.contains("test passed")
            || lower.contains("tests failed")
            || lower.contains("verification"));
    let need_tests =
        tests_passed.is_some() && (!has_real_test_claim || !claims.mentions_verification);
    if need_tests {
        match tests_passed {
            Some(true) => extras.push("cargo test passed.".into()),
            Some(false) => extras.push("cargo test failed (unresolved).".into()),
            None => {}
        }
    }

    if extras.is_empty() {
        return model_text.to_string();
    }
    let trailer = extras.join(" ");
    let base = model_text.trim();
    if base.is_empty() {
        trailer
    } else {
        format!("{base}\n\n{trailer}")
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
    fn enrich_terminal_handoff_fills_missing_change_and_test_claims() {
        let weak = "Done.";
        let enriched = enrich_terminal_handoff(
            weak,
            &["src/math.rs".into(), "src/parse.rs".into()],
            Some(true),
            false,
        );
        assert!(enriched.contains("Done."));
        assert!(enriched.to_ascii_lowercase().contains("changed"));
        assert!(enriched.contains("src/math.rs"));
        assert!(enriched.contains("src/parse.rs"));
        assert!(enriched.to_ascii_lowercase().contains("test"));
        assert!(enriched.to_ascii_lowercase().contains("passed"));
        // Incomplete stops keep the stop reason but still report observed work.
        let stop = "Stopped after 3 tool rounds plus one bounded test-recovery step without a final answer.";
        let enriched_stop = enrich_terminal_handoff(stop, &["src/a.rs".into()], Some(true), true);
        assert!(enriched_stop.starts_with("Stopped after"));
        assert!(enriched_stop.contains("src/a.rs"));
        assert!(enriched_stop.contains("cargo test passed"));
        // Already complete handoffs are left alone.
        let good = "Changed src/lib.rs. cargo test passed.";
        assert_eq!(
            enrich_terminal_handoff(good, &["src/lib.rs".into()], Some(true), false),
            good
        );
    }

    #[test]
    fn typed_events_produce_authoritative_observations() {
        let session_id = Uuid::new_v4();
        let updates = [
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
                    run_id: Some("run-1".into()),
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
        assert!(!evidence_authorizes_success(&incomplete_claims));
        assert!(evidence_authorizes_success(&evidence));
    }

    #[test]
    fn failed_tests_never_authorize_success() {
        let observations = CompletionObservations {
            changed_files: 1,
            tests_observed: 1,
            tests_passed: 0,
            tests_failed: 1,
            ..CompletionObservations::default()
        };
        let evidence = build_evidence(
            "completed",
            Some("Changed src/lib.rs; cargo test passed; verification green."),
            observations,
            CompletionUsage::default(),
            false,
        );
        assert_eq!(evidence.status, "failed");
        let mut forged = evidence.clone();
        forged.status = "verified".into();
        assert!(!evidence_authorizes_success(&forged));

        let mut no_passing_tests = evidence;
        no_passing_tests.status = "verified".into();
        no_passing_tests.observations.tests_failed = 0;
        no_passing_tests.observations.tests_passed = 0;
        assert!(!evidence_authorizes_success(&no_passing_tests));

        no_passing_tests.observations.tests_passed = 1;
        no_passing_tests.stop_reason = "unknown-success".into();
        assert!(!evidence_authorizes_success(&no_passing_tests));
    }
}
