//! Safe projections of current OrchestrationService / MCP structured content.
//!
//! [`PublicRunV1`] and `parse_public_run_*` are the fail-closed
//! `grokptah.public-run.v1` seam used by
//! [`crate::ReadObservatory::list_public_runs`],
//! [`crate::ReadObservatory::observe_public_run`],
//! [`crate::ReadObservatory::observe_public_progress`], and
//! [`crate::ReadObservatory::observe_public_handoff`].
//! [`PublicEventV1`] / `parse_public_event_*` are the fail-closed
//! `grokptah.public-event.v1` seam used by
//! [`crate::ReadObservatory::stream_public_events`]. Session and workspace
//! stay on the request scope and are never read from these documents. Legacy
//! `project_run` / `project_event_page` are unit parsers for camelCase
//! `RunRecord` / `JournalPage` JSON and are not public MCP consumers.
//! `list_runs` / `observe_run` / `stream_events` return `unsupported` instead
//! of calling those public tools.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::SdkError;
use crate::ids::{RunId, SessionId, WorkspaceRef};
use crate::page::{Cursor, RetainedRange};
use crate::version::{CONTRACT_VERSION, PUBLIC_EVENT_SCHEMA_VERSION, PUBLIC_RUN_SCHEMA_VERSION};

/// One `ptah_get_events` page after stripping unsafe `SessionUpdate` bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventPage {
    pub events: Vec<PublicEvent>,
    pub next_cursor: Option<Cursor>,
}

const BUILD_KIND: &str = "build";

/// Build-only session row. Filesystem `cwd` is retained only inside [`WorkspaceRef`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionView {
    pub contract_version: String,
    pub session_id: SessionId,
    pub title: String,
    pub kind: String,
    pub workspace: WorkspaceRef,
    pub workspace_status: String,
    pub updated_at: String,
    pub busy: bool,
}

/// Lifecycle, bounds, usage, stop cause, and event range. No prompt/path/secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunView {
    pub contract_version: String,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub workspace: WorkspaceRef,
    pub state: String,
    pub queue_position: Option<u64>,
    pub bounds: RunBoundsView,
    pub usage: UsageView,
    pub usage_complete: bool,
    pub stop_cause: Option<String>,
    pub event_range: Option<EventRange>,
    pub created_at: String,
    pub updated_at: String,
}

/// Host run ceilings from `RunBounds` (camelCase wire).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunBoundsView {
    pub max_prompt_bytes: u64,
    pub max_rounds: u32,
    pub max_duration_ms: u64,
    pub max_total_tokens: Option<u64>,
}

/// `aggregates.usage` / `CompletionUsage` totals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct UsageView {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub requests: u64,
}

/// Inclusive durable sequence range stamped on the run (`startSeq` / `endSeq`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EventRange {
    pub start_seq: Option<u64>,
    pub end_seq: Option<u64>,
}

/// Lifecycle states allowlisted on `grokptah.public-run.v1`. Unknown values fail closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicRunState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    LimitReached,
}

/// Allowlisted `ptah_get_run` document. Session and workspace are request-scoped
/// and are never deserialized from the remote body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicRunV1 {
    pub schema_version: String,
    pub run_id: String,
    pub state: PublicRunState,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub queue_position: Option<u64>,
    #[serde(default)]
    pub event_start_seq: Option<u64>,
    #[serde(default)]
    pub event_end_seq: Option<u64>,
    pub change_count: u64,
    pub test_count: u64,
    pub permission_requested_count: u64,
    pub permission_granted_count: u64,
    pub permission_denied_count: u64,
    pub usage_prompt_tokens: u64,
    pub usage_completion_tokens: u64,
    pub usage_total_tokens: u64,
    pub usage_request_count: u64,
    pub usage_complete: bool,
    pub usage_pending_request_count: u64,
    #[serde(default)]
    pub progress_round: Option<u32>,
    #[serde(default)]
    pub progress_max_rounds: Option<u32>,
}

/// Allowlisted `ptah_list_runs` envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicRunListV1 {
    pub schema_version: String,
    pub runs: Vec<PublicRunV1>,
}

/// Allowlisted `ptah_get_progress` document. Nested `progress` objects are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicRunProgressV1 {
    pub schema_version: String,
    pub run_id: String,
    pub state: PublicRunState,
    pub busy: bool,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub queue_position: Option<u64>,
    #[serde(default)]
    pub event_start_seq: Option<u64>,
    #[serde(default)]
    pub event_end_seq: Option<u64>,
    #[serde(default)]
    pub progress_round: Option<u32>,
    #[serde(default)]
    pub progress_max_rounds: Option<u32>,
}

/// Allowlisted `ptah_get_handoff` document. Prompt, response, and path aggregates
/// are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicRunHandoffV1 {
    pub schema_version: String,
    pub run_id: String,
    pub state: PublicRunState,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub event_start_seq: Option<u64>,
    #[serde(default)]
    pub event_end_seq: Option<u64>,
    pub change_count: u64,
    pub test_count: u64,
    pub usage_prompt_tokens: u64,
    pub usage_completion_tokens: u64,
    pub usage_total_tokens: u64,
    pub usage_request_count: u64,
    pub usage_complete: bool,
    pub usage_pending_request_count: u64,
}

/// Safe event classification allowlisted on `grokptah.public-event.v1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicEventKindV1 {
    AgentMessage,
    AgentThought,
    TurnStarted,
    ToolCall,
    ToolCallUpdate,
    Plan,
    PermissionRequired,
    TurnComplete,
    Completion,
    Error,
    SubagentSpawned,
    SubagentUpdate,
    BackgroundTask,
    ShellSessionStarted,
    ShellOutput,
    ShellSessionEnded,
    FileEdit,
    AgentProgress,
    RateLimited,
    SteeringInjected,
    PromptQueueChanged,
}

/// Allowlisted `ptah_get_events` row. Session and workspace are request-scoped
/// and are never deserialized from the remote body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicEventV1 {
    pub schema_version: String,
    pub seq: u64,
    pub ts: String,
    pub kind: PublicEventKindV1,
    #[serde(default)]
    pub tool_kind: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub step_count: Option<u32>,
    #[serde(default)]
    pub cancelled: Option<bool>,
    #[serde(default)]
    pub interrupted: Option<bool>,
    #[serde(default)]
    pub round: Option<u32>,
    #[serde(default)]
    pub max_rounds: Option<u32>,
    #[serde(default)]
    pub retry_after_ms: Option<u64>,
    #[serde(default)]
    pub revision: Option<u64>,
}

/// Allowlisted `ptah_get_events` page. Cursor expiry remains the 410 error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicEventPageV1 {
    pub schema_version: String,
    pub events: Vec<PublicEventV1>,
    #[serde(default)]
    pub next_cursor: Option<u64>,
}

/// Occupancy plus persistence/supervisor health flags. Supervisor objects are dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostCapacity {
    pub contract_version: String,
    pub max_concurrent_runs: u64,
    pub active_runs: u64,
    pub available: u64,
    pub queued_runs: u64,
    pub queue_limit: u64,
    pub health: HostHealth,
}

/// Boolean health derived from host `*Error` fields and `laggedLiveEvents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostHealth {
    pub lagged_live_events: u64,
    pub event_journal_ok: bool,
    pub audit_ok: bool,
    pub run_persistence_ok: bool,
    pub workload_supervisor_ok: bool,
    pub routine_supervisor_ok: bool,
    pub manager_supervisor_ok: bool,
    pub native_executor_ok: bool,
}

/// Versioned public event. Bodies, paths, prompts, and tool I/O are stripped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublicEvent {
    pub contract_version: String,
    pub seq: u64,
    pub ts: String,
    pub kind: PublicEventKind,
}

/// Safe event classification copied from current `SessionUpdate` `type` tags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PublicEventKind {
    AgentMessage,
    AgentThought,
    TurnStarted,
    ToolCall {
        kind: Option<String>,
        status: Option<String>,
    },
    ToolCallUpdate {
        status: Option<String>,
    },
    Plan {
        step_count: u32,
    },
    PermissionRequired,
    TurnComplete {
        cancelled: Option<bool>,
    },
    Completion {
        status: Option<String>,
        interrupted: Option<bool>,
    },
    Error,
    SubagentSpawned {
        kind: Option<String>,
    },
    SubagentUpdate {
        status: Option<String>,
    },
    BackgroundTask {
        status: Option<String>,
    },
    ShellSessionStarted,
    ShellOutput,
    ShellSessionEnded {
        cancelled: Option<bool>,
    },
    FileEdit,
    AgentProgress {
        round: Option<u32>,
        max_rounds: Option<u32>,
    },
    RateLimited {
        retry_after_ms: Option<u64>,
    },
    SteeringInjected,
    PromptQueueChanged {
        revision: Option<u64>,
    },
    Unknown,
}

pub(crate) fn project_sessions(body: &Value) -> Result<Vec<SessionView>, SdkError> {
    let rows = body
        .get("sessions")
        .and_then(Value::as_array)
        .ok_or(SdkError::Internal)?;
    let mut out = Vec::new();
    for row in rows {
        let kind = row.get("kind").and_then(Value::as_str).unwrap_or_default();
        if kind != BUILD_KIND {
            continue;
        }
        let cwd = row.get("cwd").and_then(Value::as_str).unwrap_or_default();
        if cwd.is_empty() {
            continue;
        }
        let session_id = row
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or(SdkError::Internal)?;
        out.push(SessionView {
            contract_version: CONTRACT_VERSION.to_string(),
            session_id: SessionId::new(session_id),
            title: row
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            kind: BUILD_KIND.to_string(),
            workspace: WorkspaceRef::from_host(cwd),
            workspace_status: row
                .get("workspaceStatus")
                .and_then(Value::as_str)
                .unwrap_or("ready")
                .to_string(),
            updated_at: json_string(row.get("updatedAt")).unwrap_or_default(),
            busy: row.get("busy").and_then(Value::as_bool).unwrap_or(false),
        });
    }
    Ok(out)
}

#[allow(dead_code)]
pub(crate) fn project_runs(body: &Value) -> Result<Vec<RunView>, SdkError> {
    let rows = body
        .get("runs")
        .and_then(Value::as_array)
        .ok_or(SdkError::Internal)?;
    rows.iter().map(project_run).collect()
}

#[allow(dead_code)]
pub(crate) fn project_run(row: &Value) -> Result<RunView, SdkError> {
    let run_id = row
        .get("runId")
        .and_then(Value::as_str)
        .ok_or(SdkError::Internal)?;
    let session_id = row
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or(SdkError::Internal)?;
    let workspace = row
        .get("workspace")
        .and_then(Value::as_str)
        .ok_or(SdkError::Internal)?;
    let state = row
        .get("state")
        .and_then(Value::as_str)
        .ok_or(SdkError::Internal)?;
    let bounds = row.get("bounds").ok_or(SdkError::Internal)?;
    let aggregates = row.get("aggregates");
    let usage = aggregates.and_then(|value| value.get("usage"));
    let start_seq = row.get("startSeq").and_then(Value::as_u64);
    let end_seq = row.get("endSeq").and_then(Value::as_u64);
    let event_range = match (start_seq, end_seq) {
        (None, None) => None,
        _ => Some(EventRange { start_seq, end_seq }),
    };
    Ok(RunView {
        contract_version: CONTRACT_VERSION.to_string(),
        run_id: RunId::new(run_id),
        session_id: SessionId::new(session_id),
        workspace: WorkspaceRef::from_host(workspace),
        state: state.to_string(),
        queue_position: row.get("queuePosition").and_then(Value::as_u64),
        bounds: RunBoundsView {
            max_prompt_bytes: number_u64(bounds.get("maxPromptBytes"))?,
            max_rounds: number_u64(bounds.get("maxRounds"))? as u32,
            max_duration_ms: number_u64(bounds.get("maxDurationMs"))?,
            max_total_tokens: bounds.get("maxTotalTokens").and_then(Value::as_u64),
        },
        usage: UsageView {
            prompt_tokens: usage
                .and_then(|value| value.get("promptTokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            completion_tokens: usage
                .and_then(|value| value.get("completionTokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            total_tokens: usage
                .and_then(|value| value.get("totalTokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            requests: usage
                .and_then(|value| value.get("requests"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
        },
        usage_complete: aggregates
            .and_then(|value| value.get("usageComplete"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        stop_cause: row
            .get("stopCause")
            .and_then(Value::as_str)
            .map(str::to_string),
        event_range,
        created_at: json_string(row.get("createdAt")).unwrap_or_default(),
        updated_at: json_string(row.get("updatedAt")).unwrap_or_default(),
    })
}

pub(crate) fn project_capacity(body: &Value) -> Result<HostCapacity, SdkError> {
    let health = body.get("health").cloned().unwrap_or(Value::Null);
    Ok(HostCapacity {
        contract_version: CONTRACT_VERSION.to_string(),
        max_concurrent_runs: number_u64(body.get("maxConcurrentRuns"))?,
        active_runs: number_u64(body.get("activeRuns"))?,
        available: number_u64(body.get("available"))?,
        queued_runs: number_u64(body.get("queuedRuns"))?,
        queue_limit: number_u64(body.get("queueLimit"))?,
        health: HostHealth {
            lagged_live_events: health
                .get("laggedLiveEvents")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            event_journal_ok: health_ok(&health, "eventJournalPersistenceError"),
            audit_ok: health_ok(&health, "auditPersistenceError"),
            run_persistence_ok: health_ok(&health, "runPersistenceError"),
            workload_supervisor_ok: health_ok(&health, "workloadSupervisorError"),
            routine_supervisor_ok: health_ok(&health, "routineSupervisorError"),
            manager_supervisor_ok: health_ok(&health, "managerSupervisorError"),
            native_executor_ok: health_ok(&health, "nativeExecutorError"),
        },
    })
}

#[allow(dead_code)]
pub(crate) fn project_event_page(body: &Value) -> Result<EventPage, SdkError> {
    if body.get("cursorExpired").and_then(Value::as_bool) == Some(true) {
        return Err(SdkError::CursorExpired {
            event_range: RetainedRange::from_host(
                body.get("eventRange").or_else(|| body.get("range")),
            ),
        });
    }
    let entries = body
        .get("entries")
        .and_then(Value::as_array)
        .ok_or(SdkError::Internal)?;
    let events = entries
        .iter()
        .map(project_public_event)
        .collect::<Result<Vec<_>, _>>()?;
    let next_cursor = body
        .get("nextCursor")
        .and_then(Value::as_u64)
        .map(Cursor::from_after_seq);
    Ok(EventPage {
        events,
        next_cursor,
    })
}

fn project_public_event(entry: &Value) -> Result<PublicEvent, SdkError> {
    let seq = entry
        .get("seq")
        .and_then(Value::as_u64)
        .ok_or(SdkError::Internal)?;
    let ts = json_string(entry.get("ts")).unwrap_or_default();
    let update = entry.get("update").unwrap_or(&Value::Null);
    Ok(PublicEvent {
        contract_version: CONTRACT_VERSION.to_string(),
        seq,
        ts,
        kind: project_update_kind(update),
    })
}

fn project_update_kind(update: &Value) -> PublicEventKind {
    let tag = update.get("type").and_then(Value::as_str).unwrap_or("");
    match tag {
        "agent_message_chunk" => PublicEventKind::AgentMessage,
        "agent_thought_chunk" => PublicEventKind::AgentThought,
        "turn_started" => PublicEventKind::TurnStarted,
        "tool_call" => PublicEventKind::ToolCall {
            kind: update
                .get("kind")
                .and_then(Value::as_str)
                .map(str::to_string),
            status: update
                .get("status")
                .and_then(Value::as_str)
                .map(str::to_string),
        },
        "tool_call_update" => PublicEventKind::ToolCallUpdate {
            status: update
                .get("status")
                .and_then(Value::as_str)
                .map(str::to_string),
        },
        "plan" => PublicEventKind::Plan {
            step_count: update
                .get("steps")
                .and_then(Value::as_array)
                .map(|steps| steps.len() as u32)
                .unwrap_or(0),
        },
        "permission_required" => PublicEventKind::PermissionRequired,
        "turn_complete" => PublicEventKind::TurnComplete {
            cancelled: update.get("cancelled").and_then(Value::as_bool),
        },
        "completion_evidence" => PublicEventKind::Completion {
            status: update
                .pointer("/evidence/status")
                .and_then(Value::as_str)
                .map(str::to_string),
            interrupted: update
                .pointer("/evidence/interrupted")
                .and_then(Value::as_bool),
        },
        "error" => PublicEventKind::Error,
        "subagent_spawned" => PublicEventKind::SubagentSpawned {
            kind: update
                .get("kind")
                .and_then(Value::as_str)
                .map(str::to_string),
        },
        "subagent_update" => PublicEventKind::SubagentUpdate {
            status: update
                .get("status")
                .and_then(Value::as_str)
                .map(str::to_string),
        },
        "background_task" => PublicEventKind::BackgroundTask {
            status: update
                .get("status")
                .and_then(Value::as_str)
                .map(str::to_string),
        },
        "shell_session_started" => PublicEventKind::ShellSessionStarted,
        "shell_output" => PublicEventKind::ShellOutput,
        "shell_session_ended" => PublicEventKind::ShellSessionEnded {
            cancelled: update.get("cancelled").and_then(Value::as_bool),
        },
        "file_edit" => PublicEventKind::FileEdit,
        "agent_progress" => PublicEventKind::AgentProgress {
            round: update
                .get("round")
                .and_then(Value::as_u64)
                .map(|n| n as u32),
            max_rounds: update
                .get("max_rounds")
                .and_then(Value::as_u64)
                .map(|n| n as u32)
                .or_else(|| {
                    update
                        .get("maxRounds")
                        .and_then(Value::as_u64)
                        .map(|n| n as u32)
                }),
        },
        "rate_limited" => PublicEventKind::RateLimited {
            retry_after_ms: update
                .get("retry_after_ms")
                .and_then(Value::as_u64)
                .or_else(|| update.get("retryAfterMs").and_then(Value::as_u64)),
        },
        "steering_injected" => PublicEventKind::SteeringInjected,
        "prompt_queue_changed" => PublicEventKind::PromptQueueChanged {
            revision: update.get("revision").and_then(Value::as_u64),
        },
        _ => PublicEventKind::Unknown,
    }
}

fn health_ok(health: &Value, key: &str) -> bool {
    match health.get(key) {
        None | Some(Value::Null) => true,
        Some(Value::String(text)) => text.is_empty(),
        Some(_) => false,
    }
}

fn number_u64(value: Option<&Value>) -> Result<u64, SdkError> {
    value.and_then(Value::as_u64).ok_or(SdkError::Internal)
}

fn json_string(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(text)) => Some(text.clone()),
        Some(other) if !other.is_null() => Some(other.to_string()),
        _ => None,
    }
}

/// Parse one `ptah_get_run` `grokptah.public-run.v1` document.
///
/// Used by [`crate::ReadObservatory::observe_public_run`]. Unknown version/field
/// maps to [`SdkError::Internal`] without forwarding serde payloads.
pub fn parse_public_run_v1(value: &Value) -> Result<PublicRunV1, SdkError> {
    parse_versioned(value, PUBLIC_RUN_SCHEMA_VERSION, |row: &PublicRunV1| {
        row.schema_version.as_str()
    })
}

/// Parse one `ptah_list_runs` `grokptah.public-run.v1` envelope.
///
/// Used by [`crate::ReadObservatory::list_public_runs`].
pub fn parse_public_run_list_v1(value: &Value) -> Result<PublicRunListV1, SdkError> {
    let parsed = parse_versioned(value, PUBLIC_RUN_SCHEMA_VERSION, |row: &PublicRunListV1| {
        row.schema_version.as_str()
    })?;
    for run in &parsed.runs {
        require_known_version(&run.schema_version, PUBLIC_RUN_SCHEMA_VERSION)?;
    }
    Ok(parsed)
}

/// Parse one `ptah_get_progress` `grokptah.public-run.v1` document.
///
/// Used by [`crate::ReadObservatory::observe_public_progress`].
pub fn parse_public_run_progress_v1(value: &Value) -> Result<PublicRunProgressV1, SdkError> {
    parse_versioned(
        value,
        PUBLIC_RUN_SCHEMA_VERSION,
        |row: &PublicRunProgressV1| row.schema_version.as_str(),
    )
}

/// Parse one `ptah_get_handoff` `grokptah.public-run.v1` document.
///
/// Used by [`crate::ReadObservatory::observe_public_handoff`].
pub fn parse_public_run_handoff_v1(value: &Value) -> Result<PublicRunHandoffV1, SdkError> {
    parse_versioned(
        value,
        PUBLIC_RUN_SCHEMA_VERSION,
        |row: &PublicRunHandoffV1| row.schema_version.as_str(),
    )
}

/// Parse one `ptah_get_events` `grokptah.public-event.v1` row.
///
/// Used by live compatibility readers. Unknown version/field maps to
/// [`SdkError::Internal`] without forwarding serde payloads.
pub fn parse_public_event_v1(value: &Value) -> Result<PublicEventV1, SdkError> {
    parse_versioned(value, PUBLIC_EVENT_SCHEMA_VERSION, |row: &PublicEventV1| {
        row.schema_version.as_str()
    })
}

/// Parse one `ptah_get_events` `grokptah.public-event.v1` page.
///
/// Used by [`crate::ReadObservatory::stream_public_events`].
pub fn parse_public_event_page_v1(value: &Value) -> Result<PublicEventPageV1, SdkError> {
    let parsed = parse_versioned(
        value,
        PUBLIC_EVENT_SCHEMA_VERSION,
        |row: &PublicEventPageV1| row.schema_version.as_str(),
    )?;
    for event in &parsed.events {
        require_known_version(&event.schema_version, PUBLIC_EVENT_SCHEMA_VERSION)?;
    }
    Ok(parsed)
}

fn parse_versioned<T, F>(value: &Value, expected: &str, version: F) -> Result<T, SdkError>
where
    T: DeserializeOwned,
    F: FnOnce(&T) -> &str,
{
    let parsed = T::deserialize(value).map_err(|_| SdkError::Internal)?;
    require_known_version(version(&parsed), expected)?;
    Ok(parsed)
}

fn require_known_version(version: &str, expected: &str) -> Result<(), SdkError> {
    if version != expected {
        return Err(SdkError::Internal);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{RunView, project_run, project_runs};
    use crate::error::SdkError;
    use crate::version::PUBLIC_RUN_SCHEMA_VERSION;
    use serde_json::json;

    fn run_record() -> serde_json::Value {
        json!({
            "runId": "run-legacy",
            "sessionId": "11111111-1111-4111-8111-111111111111",
            "workspace": "/tmp/project",
            "state": "completed",
            "bounds": {
                "maxPromptBytes": 1000,
                "maxRounds": 2,
                "maxDurationMs": 1000
            },
            "promptPreview": "leak-prompt",
            "startSeq": 1,
            "endSeq": 4,
            "createdAt": "2026-08-01T00:00:00Z",
            "updatedAt": "2026-08-01T00:00:00Z",
            "stopCause": "completed",
            "aggregates": {
                "usage": {
                    "promptTokens": 10,
                    "completionTokens": 4,
                    "totalTokens": 14,
                    "requests": 1
                },
                "usageComplete": true
            }
        })
    }

    fn public_run() -> serde_json::Value {
        json!({
            "schemaVersion": PUBLIC_RUN_SCHEMA_VERSION,
            "runId": "run_public_dto_1",
            "state": "completed",
            "createdAt": "2026-08-01T00:00:00Z",
            "updatedAt": "2026-08-01T00:00:00Z",
            "changeCount": 1,
            "testCount": 1,
            "permissionRequestedCount": 0,
            "permissionGrantedCount": 0,
            "permissionDeniedCount": 0,
            "usagePromptTokens": 10,
            "usageCompletionTokens": 4,
            "usageTotalTokens": 14,
            "usageRequestCount": 1,
            "usageComplete": true,
            "usagePendingRequestCount": 0
        })
    }

    #[test]
    fn legacy_parser_still_projects_run_record() {
        let view: RunView = project_run(&run_record()).unwrap();
        assert_eq!(view.run_id.as_str(), "run-legacy");
        assert_eq!(view.state, "completed");
        let listed = project_runs(&json!({ "runs": [run_record()] })).unwrap();
        assert_eq!(listed.len(), 1);
    }

    #[test]
    fn legacy_parser_does_not_accept_public_run_dto() {
        assert_eq!(project_run(&public_run()).unwrap_err(), SdkError::Internal);
        assert_eq!(
            project_runs(&json!({
                "schemaVersion": PUBLIC_RUN_SCHEMA_VERSION,
                "runs": [public_run()]
            }))
            .unwrap_err(),
            SdkError::Internal
        );
    }
}
