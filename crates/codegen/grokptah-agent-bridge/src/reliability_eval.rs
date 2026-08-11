//! Safe, structured evidence for deterministic agent reliability evaluations.
//!
//! The offline scenario suite uses a small report contract that can be compared
//! with the live runner's redacted evidence. Reports contain event counts and
//! relative paths supplied by the caller, never prompts, workspace contents,
//! credentials, or absolute workspace paths.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::events::SessionUpdate;

pub const RELIABILITY_REPORT_SCHEMA: u32 = 1;
pub const MAX_REPORT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioStatus {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioCheck {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

impl ScenarioCheck {
    pub fn pass(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            passed: true,
            detail: detail.into(),
        }
    }

    pub fn fail(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            passed: false,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EventMetrics {
    pub assistant_chunks: u32,
    pub thought_chunks: u32,
    pub tool_calls: u32,
    pub tool_errors: u32,
    pub file_edits: u32,
    pub permission_requests: u32,
    pub shell_started: u32,
    pub shell_ended: u32,
    pub steering_injected: u32,
    pub errors: u32,
    pub turn_started: u32,
    pub completion_evidence: u32,
    pub turn_complete: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalContract {
    pub turn_started: u32,
    pub completion_evidence: u32,
    pub turn_complete: u32,
    pub correlated_evidence: u32,
    pub stale_evidence: u32,
    pub evidence_precedes_completion: bool,
}

/// A bounded, redacted view of one scenario's event stream.
#[derive(Debug, Clone)]
pub struct EventLedger {
    metrics: EventMetrics,
    event_names: BTreeMap<String, u32>,
    active_turn: Option<Uuid>,
    seen_turns: Vec<Uuid>,
    evidence_turns: Vec<Uuid>,
    stale_evidence: u32,
    terminal_order_valid: bool,
    evidence_since_completion: bool,
}

impl Default for EventLedger {
    fn default() -> Self {
        Self {
            metrics: EventMetrics::default(),
            event_names: BTreeMap::new(),
            active_turn: None,
            seen_turns: Vec::new(),
            evidence_turns: Vec::new(),
            stale_evidence: 0,
            terminal_order_valid: true,
            evidence_since_completion: false,
        }
    }
}

impl EventLedger {
    pub fn record(&mut self, update: &SessionUpdate) {
        let name = event_name(update);
        *self.event_names.entry(name).or_default() += 1;
        match update {
            SessionUpdate::AgentMessageChunk { .. } => self.metrics.assistant_chunks += 1,
            SessionUpdate::AgentThoughtChunk { .. } => self.metrics.thought_chunks += 1,
            SessionUpdate::ToolCall { status, .. } => {
                self.metrics.tool_calls += 1;
                if matches!(status, crate::events::ToolCallStatus::Failed) {
                    self.metrics.tool_errors += 1;
                }
            }
            SessionUpdate::ToolCallUpdate { status, .. } => {
                if matches!(status, crate::events::ToolCallStatus::Failed) {
                    self.metrics.tool_errors += 1;
                }
            }
            SessionUpdate::FileEdit { .. } => self.metrics.file_edits += 1,
            SessionUpdate::PermissionRequired { .. } => self.metrics.permission_requests += 1,
            SessionUpdate::ShellSessionStarted { .. } => self.metrics.shell_started += 1,
            SessionUpdate::ShellSessionEnded { .. } => self.metrics.shell_ended += 1,
            SessionUpdate::SteeringInjected { .. } => self.metrics.steering_injected += 1,
            SessionUpdate::Error { .. } | SessionUpdate::RateLimited { .. } => {
                self.metrics.errors += 1
            }
            SessionUpdate::TurnStarted { turn_id, .. } => {
                self.metrics.turn_started += 1;
                self.active_turn = Some(*turn_id);
                self.seen_turns.push(*turn_id);
            }
            SessionUpdate::CompletionEvidence { turn_id, .. } => {
                self.metrics.completion_evidence += 1;
                self.evidence_since_completion = true;
                self.evidence_turns.push(*turn_id);
                if self.active_turn != Some(*turn_id) {
                    self.stale_evidence += 1;
                }
            }
            SessionUpdate::TurnComplete { .. } => {
                self.metrics.turn_complete += 1;
                if !self.evidence_since_completion {
                    self.terminal_order_valid = false;
                }
                self.evidence_since_completion = false;
            }
            SessionUpdate::Plan { .. }
            | SessionUpdate::SubagentSpawned { .. }
            | SessionUpdate::SubagentUpdate { .. }
            | SessionUpdate::BackgroundTask { .. }
            | SessionUpdate::ShellOutput { .. }
            | SessionUpdate::AgentProgress { .. } => {}
        }
    }

    pub fn metrics(&self) -> EventMetrics {
        self.metrics.clone()
    }

    pub fn event_names(&self) -> &BTreeMap<String, u32> {
        &self.event_names
    }

    /// Check that every terminal evidence event belongs to a known turn and
    /// that terminal ordering is evidence -> TurnComplete.
    pub fn terminal_contract(&self) -> TerminalContract {
        let correlated_evidence = self
            .evidence_turns
            .iter()
            .filter(|turn_id| self.seen_turns.contains(turn_id))
            .count() as u32;
        TerminalContract {
            turn_started: self.metrics.turn_started,
            completion_evidence: self.metrics.completion_evidence,
            turn_complete: self.metrics.turn_complete,
            correlated_evidence,
            stale_evidence: self.stale_evidence,
            evidence_precedes_completion: self.metrics.turn_complete > 0
                && self.terminal_order_valid,
        }
    }

    pub fn saw(&self, name: &str) -> bool {
        self.event_names.get(name).copied().unwrap_or_default() > 0
    }

    pub fn active_turn(&self) -> Option<Uuid> {
        self.active_turn
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioResult {
    pub id: String,
    pub status: ScenarioStatus,
    pub duration_ms: u128,
    pub checks: Vec<ScenarioCheck>,
    pub metrics: EventMetrics,
    pub terminal: TerminalContract,
    pub event_names: BTreeMap<String, u32>,
    pub changed_paths: Vec<String>,
    pub failure_reasons: Vec<String>,
}

impl ScenarioResult {
    pub fn from_ledger(
        id: impl Into<String>,
        duration_ms: u128,
        ledger: &EventLedger,
        checks: Vec<ScenarioCheck>,
        changed_paths: impl IntoIterator<Item = String>,
    ) -> Self {
        let failure_reasons = checks
            .iter()
            .filter(|check| !check.passed)
            .map(|check| format!("{}: {}", check.name, check.detail))
            .collect::<Vec<_>>();
        let status = if failure_reasons.is_empty() {
            ScenarioStatus::Passed
        } else {
            ScenarioStatus::Failed
        };
        Self {
            id: id.into(),
            status,
            duration_ms,
            checks,
            metrics: ledger.metrics(),
            terminal: ledger.terminal_contract(),
            event_names: ledger.event_names().clone(),
            changed_paths: changed_paths
                .into_iter()
                .map(|p| sanitize_path(&p))
                .collect(),
            failure_reasons,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReportSummary {
    pub scenarios: u32,
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    pub total_duration_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReliabilityReport {
    pub schema_version: u32,
    pub mode: String,
    pub model: String,
    pub started_at: String,
    pub finished_at: String,
    pub summary: ReportSummary,
    pub scenarios: Vec<ScenarioResult>,
}

impl ReliabilityReport {
    pub fn new(mode: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            schema_version: RELIABILITY_REPORT_SCHEMA,
            mode: mode.into(),
            model: model.into(),
            started_at: Utc::now().to_rfc3339(),
            finished_at: String::new(),
            summary: ReportSummary::default(),
            scenarios: Vec::new(),
        }
    }

    pub fn finish(mut self, scenarios: Vec<ScenarioResult>) -> Self {
        let mut summary = ReportSummary {
            scenarios: scenarios.len() as u32,
            total_duration_ms: scenarios.iter().map(|s| s.duration_ms).sum(),
            ..ReportSummary::default()
        };
        for scenario in &scenarios {
            match scenario.status {
                ScenarioStatus::Passed => summary.passed += 1,
                ScenarioStatus::Failed => summary.failed += 1,
                ScenarioStatus::Skipped => summary.skipped += 1,
            }
        }
        self.finished_at = Utc::now().to_rfc3339();
        self.summary = summary;
        self.scenarios = scenarios;
        self
    }
}

/// Write only a bounded JSON report beneath the caller-selected output root.
/// The filename is fixed so a scenario cannot redirect output elsewhere.
pub fn write_report(output_dir: &Path, report: &ReliabilityReport) -> Result<PathBuf, String> {
    fs::create_dir_all(output_dir).map_err(|error| format!("create report directory: {error}"))?;
    let bytes =
        serde_json::to_vec_pretty(report).map_err(|error| format!("serialize report: {error}"))?;
    if bytes.len() > MAX_REPORT_BYTES {
        return Err(format!("report exceeds {MAX_REPORT_BYTES} bytes"));
    }
    let path = output_dir.join("reliability-report.json");
    fs::write(&path, bytes).map_err(|error| format!("write report: {error}"))?;
    Ok(path)
}

fn sanitize_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let has_windows_drive = normalized.len() >= 3
        && normalized.as_bytes()[0].is_ascii_alphabetic()
        && normalized.as_bytes()[1] == b':'
        && normalized.as_bytes()[2] == b'/';
    let is_unc = normalized.starts_with("//");
    if Path::new(&normalized).is_absolute()
        || has_windows_drive
        || is_unc
        || normalized.split('/').any(|part| part == "..")
    {
        "<outside-workspace>".into()
    } else {
        normalized
    }
}

fn event_name(update: &SessionUpdate) -> String {
    match update {
        SessionUpdate::AgentMessageChunk { .. } => "agent_message_chunk",
        SessionUpdate::AgentThoughtChunk { .. } => "agent_thought_chunk",
        SessionUpdate::TurnStarted { .. } => "turn_started",
        SessionUpdate::ToolCall { .. } => "tool_call",
        SessionUpdate::ToolCallUpdate { .. } => "tool_call_update",
        SessionUpdate::Plan { .. } => "plan",
        SessionUpdate::PermissionRequired { .. } => "permission_required",
        SessionUpdate::TurnComplete { .. } => "turn_complete",
        SessionUpdate::CompletionEvidence { .. } => "completion_evidence",
        SessionUpdate::Error { .. } => "error",
        SessionUpdate::SubagentSpawned { .. } => "subagent_spawned",
        SessionUpdate::SubagentUpdate { .. } => "subagent_update",
        SessionUpdate::BackgroundTask { .. } => "background_task",
        SessionUpdate::ShellSessionStarted { .. } => "shell_session_started",
        SessionUpdate::ShellOutput { .. } => "shell_output",
        SessionUpdate::ShellSessionEnded { .. } => "shell_session_ended",
        SessionUpdate::FileEdit { .. } => "file_edit",
        SessionUpdate::AgentProgress { .. } => "agent_progress",
        SessionUpdate::RateLimited { .. } => "rate_limited",
        SessionUpdate::SteeringInjected { .. } => "steering_injected",
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::{
        CompletionClaims, CompletionEvidence, CompletionObservations, CompletionUsage,
    };

    #[test]
    fn ledger_requires_current_turn_and_terminal_order() {
        let session_id = Uuid::new_v4();
        let first = Uuid::new_v4();
        let evidence = |turn_id| SessionUpdate::CompletionEvidence {
            session_id,
            turn_id,
            evidence: CompletionEvidence {
                status: "completed".into(),
                stop_reason: "completed".into(),
                interrupted: false,
                claims: CompletionClaims::default(),
                observations: CompletionObservations::default(),
                usage: CompletionUsage::default(),
            },
        };
        let mut ledger = EventLedger::default();
        ledger.record(&SessionUpdate::TurnStarted {
            session_id,
            turn_id: first,
        });
        ledger.record(&evidence(first));
        ledger.record(&SessionUpdate::TurnComplete {
            session_id,
            cancelled: false,
        });
        ledger.record(&SessionUpdate::TurnStarted {
            session_id,
            turn_id: Uuid::new_v4(),
        });
        ledger.record(&evidence(first));
        let contract = ledger.terminal_contract();
        assert_eq!(contract.correlated_evidence, 2);
        assert_eq!(contract.stale_evidence, 1);
        assert!(contract.evidence_precedes_completion);
    }

    #[test]
    fn report_sanitizes_paths_and_has_fixed_bounded_output() {
        let ledger = EventLedger::default();
        let scenario = ScenarioResult::from_ledger(
            "paths",
            4,
            &ledger,
            vec![ScenarioCheck::pass("safe", "ok")],
            [
                "src/lib.rs".into(),
                "/private/secret.txt".into(),
                "../escape".into(),
                "C:/private/secret.txt".into(),
                "//server/private/secret.txt".into(),
            ],
        );
        assert_eq!(scenario.changed_paths[0], "src/lib.rs");
        assert_eq!(scenario.changed_paths[1], "<outside-workspace>");
        assert_eq!(scenario.changed_paths[2], "<outside-workspace>");
        assert_eq!(scenario.changed_paths[3], "<outside-workspace>");
        assert_eq!(scenario.changed_paths[4], "<outside-workspace>");
        let report = ReliabilityReport::new("offline", "fixture").finish(vec![scenario]);
        let out = tempfile::tempdir().unwrap();
        let path = write_report(out.path(), &report).unwrap();
        assert_eq!(path.file_name().unwrap(), "reliability-report.json");
        assert!(path.metadata().unwrap().len() < MAX_REPORT_BYTES as u64);
    }
}
