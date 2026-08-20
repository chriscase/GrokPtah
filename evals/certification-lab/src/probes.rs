//! Minimal black-box probe implementations for the public MCP control plane.
//!
//! Every value extracted from MCP is used transiently and either discarded or
//! converted to an opaque SHA-256 label before it can enter report evidence.

use std::time::Instant;

use grokptah_agent_bridge::{McpControlClient, McpRemoteError};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::manifest::ProbeDefinition;
use crate::report::{
    diagnostic_failure_class, opaque_durable_id, ArgumentFieldCode, DiagnosticCode, DurableIdKind,
    DurableStateCode, EntityKind, EvidenceCounters, OpaqueDurableId, PhaseCode, PhaseResult,
    ProbeResult, ProbeStatus, ReconnectEvidence, RestartEvidence, StructuralTrace,
    TraceOperationCode, TraceRecord, TransitionEvidence,
};
use crate::LAB_TRACE_SCHEMA;

const SAFE_TITLE: &str = "Persistent Agent certification probe";

pub struct ProbeExecution {
    pub result: ProbeResult,
    pub trace: StructuralTrace,
}

#[derive(Clone)]
struct TestAgent {
    session_id: String,
    agent_id: String,
}

struct ProbeBuilder<'a> {
    definition: &'a ProbeDefinition,
    started: Instant,
    counters: EvidenceCounters,
    records: Vec<TraceRecord>,
    transitions: Vec<TransitionEvidence>,
    opaque_ids: Vec<OpaqueDurableId>,
}

impl<'a> ProbeBuilder<'a> {
    fn new(definition: &'a ProbeDefinition) -> Self {
        Self {
            definition,
            started: Instant::now(),
            counters: EvidenceCounters::default(),
            records: Vec::new(),
            transitions: Vec::new(),
            opaque_ids: Vec::new(),
        }
    }

    async fn call(
        &mut self,
        client: &mut McpControlClient,
        operation: TraceOperationCode,
        tool: &str,
        arguments: Value,
        argument_fields: Vec<ArgumentFieldCode>,
    ) -> Result<Value, DiagnosticCode> {
        self.push_trace(operation, argument_fields, None)?;
        self.counters.tool_calls = self
            .counters
            .tool_calls
            .checked_add(1)
            .ok_or(DiagnosticCode::BoundExceeded)?;
        match client.call_tool(tool, arguments).await {
            Ok(result) if !result.is_error => Ok(result.structured),
            Ok(_) => {
                self.counters.errors = self
                    .counters
                    .errors
                    .checked_add(1)
                    .ok_or(DiagnosticCode::BoundExceeded)?;
                Err(DiagnosticCode::McpToolError)
            }
            Err(error) => {
                self.counters.errors = self
                    .counters
                    .errors
                    .checked_add(1)
                    .ok_or(DiagnosticCode::BoundExceeded)?;
                let diagnostic = match error
                    .downcast_ref::<McpRemoteError>()
                    .and_then(McpRemoteError::data_code)
                {
                    Some("unauthenticated") => DiagnosticCode::AuthenticationUnavailable,
                    Some("forbidden_scope" | "workspace_mismatch") => DiagnosticCode::ScopeRejected,
                    Some("conflict" | "stale_version") => DiagnosticCode::StateTransitionMismatch,
                    Some("invalid_request") => DiagnosticCode::McpResultMalformed,
                    _ => DiagnosticCode::McpCallFailed,
                };
                Err(diagnostic)
            }
        }
    }

    fn push_trace(
        &mut self,
        operation: TraceOperationCode,
        argument_fields: Vec<ArgumentFieldCode>,
        diagnostic: Option<DiagnosticCode>,
    ) -> Result<(), DiagnosticCode> {
        let next = self
            .records
            .len()
            .checked_add(1)
            .ok_or(DiagnosticCode::BoundExceeded)?;
        let ordinal = u32::try_from(next).map_err(|_| DiagnosticCode::BoundExceeded)?;
        self.records.push(TraceRecord {
            ordinal,
            operation,
            argument_fields,
            diagnostic,
            sequence: None,
        });
        Ok(())
    }

    fn retain_id(&mut self, kind: DurableIdKind, actual: &str) -> String {
        let value = opaque_durable_id(actual);
        if !self
            .opaque_ids
            .iter()
            .any(|item| item.kind == kind && item.value == value)
        {
            self.opaque_ids.push(OpaqueDurableId {
                kind,
                value: value.clone(),
            });
        }
        value
    }

    fn transition(
        &mut self,
        entity: EntityKind,
        from: DurableStateCode,
        to: DurableStateCode,
        actual_id: Option<&str>,
    ) {
        self.transitions.push(TransitionEvidence {
            entity,
            from,
            to,
            opaque_id: actual_id.map(opaque_durable_id),
        });
    }

    fn finish(self, status: ProbeStatus, diagnostic: DiagnosticCode) -> ProbeExecution {
        let elapsed_millis = u64::try_from(self.started.elapsed().as_millis())
            .unwrap_or(crate::report::MAX_PHASE_MILLIS);
        let failure_class = diagnostic_failure_class(status, diagnostic);
        let phase = PhaseResult {
            phase: PhaseCode::Oracle,
            status,
            elapsed_millis,
            diagnostics: vec![diagnostic],
        };
        ProbeExecution {
            result: ProbeResult {
                probe_id: self.definition.id.clone(),
                catalog_scenario_ids: self.definition.catalog_scenario_ids.clone(),
                status,
                supported: status != ProbeStatus::Skipped,
                failure_class,
                diagnostics: vec![diagnostic],
                verified_actions: if status == ProbeStatus::Passed {
                    self.definition.actions.clone()
                } else {
                    Vec::new()
                },
                verified_oracles: if status == ProbeStatus::Passed {
                    self.definition.oracle_codes.clone()
                } else {
                    Vec::new()
                },
                phases: vec![phase],
                transitions: self.transitions,
                counters: self.counters,
                reconnect: ReconnectEvidence::default(),
                restart: RestartEvidence::default(),
                opaque_ids: self.opaque_ids,
                trace: None,
                capture_refs: Vec::new(),
                elapsed_millis,
            },
            trace: StructuralTrace {
                schema: LAB_TRACE_SCHEMA.into(),
                probe_id: self.definition.id.clone(),
                records: self.records,
                truncated: false,
                dropped_records: 0,
            },
        }
    }
}

impl ProbeExecution {
    pub fn timed_out(definition: &ProbeDefinition) -> Self {
        ProbeBuilder::new(definition).finish(ProbeStatus::Failed, DiagnosticCode::Timeout)
    }
}

pub fn has_implementation(probe_id: &str) -> bool {
    implementation_tools(probe_id).is_some()
}

/// Exact public MCP tools an implemented probe may invoke, including its
/// self-contained setup. Manifest tests keep this allowlist and capability
/// discovery in lockstep, so an implementation cannot silently call an
/// undeclared operation.
pub fn implementation_tools(probe_id: &str) -> Option<&'static [&'static str]> {
    match probe_id {
        "core-service-readiness-v1" => Some(&["ptah_get_capacity", "ptah_list_sessions"]),
        "core-agent-identity-v1" => Some(&[
            "ptah_create_session",
            "ptah_submit_task",
            "ptah_cancel",
            "ptah_get_run",
            "ptah_list_persistent_agents",
        ]),
        "work-idempotency-conflict-v1" => Some(&[
            "ptah_create_session",
            "ptah_submit_task",
            "ptah_cancel",
            "ptah_get_run",
            "ptah_list_persistent_agents",
            "ptah_create_work",
            "ptah_get_work",
        ]),
        "routine-manual-activation-v1" => Some(&[
            "ptah_create_session",
            "ptah_submit_task",
            "ptah_cancel",
            "ptah_get_run",
            "ptah_list_persistent_agents",
            "ptah_create_routine",
            "ptah_fire_routine",
            "ptah_list_activations",
        ]),
        "coordinator-parent-child-work-v1" => Some(&[
            "ptah_create_session",
            "ptah_submit_task",
            "ptah_cancel",
            "ptah_get_run",
            "ptah_list_persistent_agents",
            "ptah_create_work",
            "ptah_get_work",
        ]),
        _ => None,
    }
}

pub async fn execute_minimal_probe(
    definition: &ProbeDefinition,
    client: &mut McpControlClient,
    workspace: &str,
) -> ProbeExecution {
    let mut probe = ProbeBuilder::new(definition);
    let outcome = match definition.id.as_str() {
        "core-service-readiness-v1" => readiness(&mut probe, client).await,
        "core-agent-identity-v1" => identity(&mut probe, client, workspace).await,
        "work-idempotency-conflict-v1" => work_idempotency(&mut probe, client, workspace).await,
        "routine-manual-activation-v1" => routine_manual(&mut probe, client, workspace).await,
        "coordinator-parent-child-work-v1" => {
            coordinator_parent_child(&mut probe, client, workspace).await
        }
        _ => Err(DiagnosticCode::ProbeImplementationUnavailable),
    };
    match outcome {
        Ok(()) => probe.finish(ProbeStatus::Passed, DiagnosticCode::Ok),
        Err(code) => probe.finish(ProbeStatus::Failed, code),
    }
}

async fn readiness(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
) -> Result<(), DiagnosticCode> {
    let capacity = probe
        .call(
            client,
            TraceOperationCode::GetCapacity,
            "ptah_get_capacity",
            json!({}),
            vec![],
        )
        .await?;
    let maximum = capacity["maxConcurrentRuns"]
        .as_u64()
        .filter(|value| *value > 0)
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    let active = capacity["activeRuns"]
        .as_u64()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    let available = capacity["available"]
        .as_u64()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    let queued = capacity["queuedRuns"]
        .as_u64()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    let queue_limit = capacity["queueLimit"]
        .as_u64()
        .filter(|value| *value > 0)
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    let accounted = active
        .checked_add(available)
        .ok_or(DiagnosticCode::BoundExceeded)?;
    if active > maximum || available > maximum || accounted != maximum || queued > queue_limit {
        return Err(DiagnosticCode::ServiceNotReady);
    }
    let health = capacity["health"]
        .as_object()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    validate_capacity_health(health)?;
    let sessions = probe
        .call(
            client,
            TraceOperationCode::ListSessions,
            "ptah_list_sessions",
            json!({}),
            vec![],
        )
        .await?;
    if !sessions["sessions"].is_array() {
        return Err(DiagnosticCode::McpResultMalformed);
    }
    probe.transition(
        EntityKind::Service,
        DurableStateCode::Starting,
        DurableStateCode::Ready,
        None,
    );
    Ok(())
}

fn validate_capacity_health(health: &serde_json::Map<String, Value>) -> Result<(), DiagnosticCode> {
    for field in [
        "eventJournalPersistenceError",
        "auditPersistenceError",
        "runPersistenceError",
        "workloadSupervisorError",
        "routineSupervisorError",
    ] {
        match health.get(field) {
            Some(Value::Null) => {}
            Some(_) => return Err(DiagnosticCode::ServiceNotReady),
            None => return Err(DiagnosticCode::McpResultMalformed),
        }
    }
    if !health
        .get("workloadSupervisor")
        .is_some_and(Value::is_object)
        || !health
            .get("routineSupervisor")
            .is_some_and(Value::is_object)
    {
        return Err(DiagnosticCode::McpResultMalformed);
    }
    if health
        .get("nativeExecutor")
        .is_some_and(|value| !value.is_object() && !value.is_null())
    {
        return Err(DiagnosticCode::ServiceNotReady);
    }
    Ok(())
}

async fn identity(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
) -> Result<(), DiagnosticCode> {
    let agent = create_agent(probe, client, workspace).await?;
    // Current main exposes the exact Agent read as a checkpoint-backed resume
    // plan. An MCP-submitted external Run does not seed that checkpoint, so
    // identity projection is proved here by two independent durable list
    // reads. The separate checkpoint probe remains indeterminate rather than
    // treating a conflict as successful evidence.
    let projection = probe
        .call(
            client,
            TraceOperationCode::ListAgents,
            "ptah_list_persistent_agents",
            json!({}),
            vec![],
        )
        .await?;
    if !projection["agents"].as_array().is_some_and(|agents| {
        agents.iter().any(|candidate| {
            candidate["agentId"].as_str() == Some(agent.agent_id.as_str())
                && candidate["sessionId"].as_str() == Some(agent.session_id.as_str())
                && candidate["workspace"].as_str() == Some(workspace)
        })
    }) {
        return Err(DiagnosticCode::McpResultMalformed);
    }
    probe.transition(
        EntityKind::Agent,
        DurableStateCode::Absent,
        DurableStateCode::Created,
        Some(&agent.agent_id),
    );
    probe.retain_id(DurableIdKind::Session, &agent.session_id);
    probe.retain_id(DurableIdKind::Agent, &agent.agent_id);
    Ok(())
}

async fn work_idempotency(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
) -> Result<(), DiagnosticCode> {
    let agent = create_agent(probe, client, workspace).await?;
    let request_id = request_id("work");
    let arguments = json!({
        "request_id": request_id,
        "session_id": agent.session_id,
        "workspace": workspace,
        "kind": "certification",
        "objective": "Bounded black-box idempotency evidence",
    });
    let created = probe
        .call(
            client,
            TraceOperationCode::CreateWork,
            "ptah_create_work",
            arguments.clone(),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::Kind,
                ArgumentFieldCode::Objective,
            ],
        )
        .await?;
    let work_id = required_string(&created, &["work", "workId"])?;
    let replay = probe
        .call(
            client,
            TraceOperationCode::CreateWork,
            "ptah_create_work",
            arguments,
            vec![ArgumentFieldCode::RequestId],
        )
        .await?;
    if replay["work"]["workId"].as_str() != Some(work_id.as_str()) {
        return Err(DiagnosticCode::IdempotencyReplayMismatch);
    }
    let conflict = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": request_id,
                "session_id": agent.session_id,
                "workspace": workspace,
                "kind": "certification",
                "objective": "Changed payload must conflict",
            }),
        )
        .await;
    probe.counters.tool_calls = probe
        .counters
        .tool_calls
        .checked_add(1)
        .ok_or(DiagnosticCode::BoundExceeded)?;
    match conflict {
        Ok(result) if !result.is_error => {
            probe.push_trace(
                TraceOperationCode::CreateWork,
                vec![ArgumentFieldCode::RequestId],
                Some(DiagnosticCode::IdempotencyConflictAccepted),
            )?;
            return Err(DiagnosticCode::IdempotencyConflictAccepted);
        }
        Ok(_) => {
            probe.counters.errors = probe
                .counters
                .errors
                .checked_add(1)
                .ok_or(DiagnosticCode::BoundExceeded)?;
            probe.push_trace(
                TraceOperationCode::CreateWork,
                vec![ArgumentFieldCode::RequestId],
                Some(DiagnosticCode::IdempotencyConflictUnproven),
            )?;
            return Err(DiagnosticCode::IdempotencyConflictUnproven);
        }
        Err(error)
            if error
                .downcast_ref::<McpRemoteError>()
                .and_then(McpRemoteError::data_code)
                == Some("conflict") =>
        {
            probe.counters.errors = probe
                .counters
                .errors
                .checked_add(1)
                .ok_or(DiagnosticCode::BoundExceeded)?;
            probe.push_trace(
                TraceOperationCode::CreateWork,
                vec![ArgumentFieldCode::RequestId],
                Some(DiagnosticCode::IdempotencyConflictObserved),
            )?;
        }
        Err(_) => {
            probe.counters.errors = probe
                .counters
                .errors
                .checked_add(1)
                .ok_or(DiagnosticCode::BoundExceeded)?;
            probe.push_trace(
                TraceOperationCode::CreateWork,
                vec![ArgumentFieldCode::RequestId],
                Some(DiagnosticCode::IdempotencyConflictUnproven),
            )?;
            return Err(DiagnosticCode::IdempotencyConflictUnproven);
        }
    }
    let observed = probe
        .call(
            client,
            TraceOperationCode::GetWork,
            "ptah_get_work",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
                "work_id": work_id,
            }),
            vec![
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::WorkId,
            ],
        )
        .await?;
    if observed["work"]["workId"].as_str() != Some(work_id.as_str())
        || observed["work"]["objective"].as_str() != Some("Bounded black-box idempotency evidence")
        || observed["work"]["kind"].as_str() != Some("certification")
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    probe.transition(
        EntityKind::Work,
        DurableStateCode::Created,
        DurableStateCode::Deduplicated,
        Some(&work_id),
    );
    probe.retain_id(DurableIdKind::Work, &work_id);
    Ok(())
}

async fn routine_manual(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
) -> Result<(), DiagnosticCode> {
    let agent = create_agent(probe, client, workspace).await?;
    let created = probe
        .call(
            client,
            TraceOperationCode::CreateRoutine,
            "ptah_create_routine",
            json!({
                "request_id": request_id("routine"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "name": "bounded-manual-certification",
                "agent_id": agent.agent_id,
                "trigger": {"kind": "manual"},
                "work_template": {
                    "kind": "certification",
                    "objective": "Bounded manual activation",
                    "priority": 0,
                    "policy": {
                        "bounds": {
                            "maxPromptBytes": 4096,
                            "maxRounds": 2,
                            "maxDurationMs": 30000,
                            "maxTotalTokens": 1000
                        },
                        "retry": {
                            "maxAttempts": 1,
                            "retryFailed": false,
                            "retryExpired": false,
                            "backoffMs": 0
                        },
                        "requiresApproval": false,
                        "maxConcurrentAttempts": 1
                    }
                }
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::AgentId,
                ArgumentFieldCode::Trigger,
                ArgumentFieldCode::WorkTemplate,
            ],
        )
        .await?;
    let routine_id = required_string(&created, &["routine", "routineId"])?;
    let fired = probe
        .call(
            client,
            TraceOperationCode::FireRoutine,
            "ptah_fire_routine",
            json!({
                "request_id": request_id("fire"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "routine_id": routine_id,
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::RoutineId,
            ],
        )
        .await?;
    let activation_id = required_string(&fired, &["activation", "activationId"])?;
    let fired_work_id = required_string(&fired, &["activation", "workId"])?;
    let listed = probe
        .call(
            client,
            TraceOperationCode::ListActivations,
            "ptah_list_activations",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
                "routine_id": routine_id,
            }),
            vec![
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::RoutineId,
            ],
        )
        .await?;
    let activation = listed["activations"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["activationId"].as_str() == Some(activation_id.as_str()))
        })
        .ok_or(DiagnosticCode::StateTransitionMismatch)?;
    if activation["routineId"].as_str() != Some(routine_id.as_str())
        || activation["workId"].as_str() != Some(fired_work_id.as_str())
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    probe.transition(
        EntityKind::Routine,
        DurableStateCode::Created,
        DurableStateCode::Enabled,
        Some(&routine_id),
    );
    probe.transition(
        EntityKind::Activation,
        DurableStateCode::Absent,
        DurableStateCode::Created,
        Some(&activation_id),
    );
    probe.retain_id(DurableIdKind::Routine, &routine_id);
    probe.retain_id(DurableIdKind::Activation, &activation_id);
    probe.retain_id(DurableIdKind::Work, &fired_work_id);
    Ok(())
}

async fn coordinator_parent_child(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
) -> Result<(), DiagnosticCode> {
    let agent = create_agent(probe, client, workspace).await?;
    let parent = probe
        .call(
            client,
            TraceOperationCode::CreateWork,
            "ptah_create_work",
            json!({
                "request_id": request_id("parent"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "kind": "parent",
                "objective": "Bounded coordinator parent",
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
            ],
        )
        .await?;
    let parent_id = required_string(&parent, &["work", "workId"])?;
    let child = probe
        .call(
            client,
            TraceOperationCode::CreateWork,
            "ptah_create_work",
            json!({
                "request_id": request_id("child"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "kind": "child",
                "objective": "Bounded coordinator child",
                "parent_work_id": parent_id,
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::WorkId,
            ],
        )
        .await?;
    let child_id = required_string(&child, &["work", "workId"])?;
    if child["work"]["parentWorkId"].as_str() != Some(parent_id.as_str()) {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let observed_child = probe
        .call(
            client,
            TraceOperationCode::GetWork,
            "ptah_get_work",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
                "work_id": child_id,
            }),
            vec![
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::WorkId,
            ],
        )
        .await?;
    if observed_child["work"]["workId"].as_str() != Some(child_id.as_str())
        || observed_child["work"]["parentWorkId"].as_str() != Some(parent_id.as_str())
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    probe.transition(
        EntityKind::Work,
        DurableStateCode::Absent,
        DurableStateCode::Created,
        Some(&parent_id),
    );
    probe.transition(
        EntityKind::Work,
        DurableStateCode::Absent,
        DurableStateCode::Created,
        Some(&child_id),
    );
    probe.retain_id(DurableIdKind::Work, &parent_id);
    probe.retain_id(DurableIdKind::Work, &child_id);
    Ok(())
}

async fn create_agent(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
) -> Result<TestAgent, DiagnosticCode> {
    let created = probe
        .call(
            client,
            TraceOperationCode::CreateSession,
            "ptah_create_session",
            json!({"workspace": workspace, "title": SAFE_TITLE}),
            vec![ArgumentFieldCode::Workspace],
        )
        .await?;
    let session_id = required_string(&created, &["sessionId"])?;
    let submitted = probe
        .call(
            client,
            TraceOperationCode::SubmitRun,
            "ptah_submit_task",
            json!({
                "request_id": request_id("agent-seed"),
                "session_id": session_id,
                "workspace": workspace,
                "prompt": "Return a short acknowledgement and stop without tools.",
                "bounds": {
                    "maxPromptBytes": 512,
                    "maxRounds": 1,
                    "maxDurationMs": 5000,
                    "maxTotalTokens": 1000
                },
                "execution_mode": "shared",
                "allow_queue": true
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::Prompt,
                ArgumentFieldCode::Bounds,
                ArgumentFieldCode::ExecutionMode,
                ArgumentFieldCode::AllowQueue,
            ],
        )
        .await?;
    let run_id = required_string(&submitted, &["runId"])?;
    let completed =
        match wait_for_terminal_seed(probe, client, workspace, &session_id, &run_id).await {
            Ok(completed) => completed,
            Err(DiagnosticCode::Timeout) => {
                probe
                    .call(
                        client,
                        TraceOperationCode::CancelRun,
                        "ptah_cancel",
                        json!({
                            "request_id": request_id("agent-seed-cancel"),
                            "session_id": session_id,
                            "workspace": workspace,
                            "run_id": run_id,
                        }),
                        vec![
                            ArgumentFieldCode::RequestId,
                            ArgumentFieldCode::SessionId,
                            ArgumentFieldCode::Workspace,
                            ArgumentFieldCode::RunId,
                        ],
                    )
                    .await?;
                wait_for_terminal_seed(probe, client, workspace, &session_id, &run_id).await?
            }
            Err(error) => return Err(error),
        };
    let listed = probe
        .call(
            client,
            TraceOperationCode::ListAgents,
            "ptah_list_persistent_agents",
            json!({}),
            vec![],
        )
        .await?;
    let agent_id = listed["agents"]
        .as_array()
        .and_then(|agents| {
            agents
                .iter()
                .find(|agent| agent["sessionId"].as_str() == Some(session_id.as_str()))
        })
        .and_then(|agent| agent["agentId"].as_str())
        .map(str::to_owned)
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    probe.retain_id(DurableIdKind::Run, &run_id);
    if !completed && probe.definition.id == "core-agent-identity-v1" {
        return Err(DiagnosticCode::TerminalStateMissing);
    }
    Ok(TestAgent {
        session_id,
        agent_id,
    })
}

async fn wait_for_terminal_seed(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
    session_id: &str,
    run_id: &str,
) -> Result<bool, DiagnosticCode> {
    for _ in 0..100 {
        let run = probe
            .call(
                client,
                TraceOperationCode::GetRun,
                "ptah_get_run",
                json!({
                    "session_id": session_id,
                    "workspace": workspace,
                    "run_id": run_id,
                }),
                vec![
                    ArgumentFieldCode::SessionId,
                    ArgumentFieldCode::Workspace,
                    ArgumentFieldCode::RunId,
                ],
            )
            .await?;
        match run["state"].as_str() {
            Some("completed") => return Ok(true),
            Some("failed" | "cancelled" | "interrupted" | "limit_reached") => return Ok(false),
            Some("queued" | "running" | "waiting") => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            _ => return Err(DiagnosticCode::McpResultMalformed),
        }
    }
    Err(DiagnosticCode::Timeout)
}

fn required_string(value: &Value, path: &[&str]) -> Result<String, DiagnosticCode> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(*key).ok_or(DiagnosticCode::McpResultMalformed)?;
    }
    cursor
        .as_str()
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .map(str::to_owned)
        .ok_or(DiagnosticCode::McpResultMalformed)
}

fn request_id(prefix: &str) -> String {
    format!("cert-{prefix}-{}", Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::CampaignManifest;
    use std::collections::BTreeSet;

    #[test]
    fn smoke_probe_ids_are_real_manifest_entries() {
        let manifest = CampaignManifest::bundled().unwrap();
        for id in [
            "core-service-readiness-v1",
            "core-agent-identity-v1",
            "work-idempotency-conflict-v1",
            "routine-manual-activation-v1",
            "coordinator-parent-child-work-v1",
        ] {
            assert!(has_implementation(id));
            assert!(manifest.probe(id).is_some());
        }
    }

    #[test]
    fn implemented_probe_tool_allowlists_exactly_match_the_manifest() {
        let manifest = CampaignManifest::bundled().unwrap();
        for definition in manifest
            .probes
            .iter()
            .filter(|definition| has_implementation(&definition.id))
        {
            let declared: BTreeSet<_> = definition
                .required_tools
                .iter()
                .map(String::as_str)
                .collect();
            let implemented: BTreeSet<_> = implementation_tools(&definition.id)
                .unwrap()
                .iter()
                .copied()
                .collect();
            assert_eq!(declared, implemented, "{}", definition.id);
        }
    }

    #[test]
    fn readiness_health_requires_every_public_error_slot_and_supervisor_shape() {
        let mut health = json!({
            "eventJournalPersistenceError": null,
            "auditPersistenceError": null,
            "runPersistenceError": null,
            "workloadSupervisorError": null,
            "routineSupervisorError": null,
            "workloadSupervisor": {},
            "routineSupervisor": {}
        });
        assert!(validate_capacity_health(health.as_object().unwrap()).is_ok());
        health
            .as_object_mut()
            .unwrap()
            .remove("runPersistenceError");
        assert_eq!(
            validate_capacity_health(health.as_object().unwrap()),
            Err(DiagnosticCode::McpResultMalformed)
        );
        health["runPersistenceError"] = json!("bounded-error-code");
        assert_eq!(
            validate_capacity_health(health.as_object().unwrap()),
            Err(DiagnosticCode::ServiceNotReady)
        );
    }
}
