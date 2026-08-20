//! Minimal black-box probe implementations for the public MCP control plane.
//!
//! Every value extracted from MCP is used transiently and either discarded or
//! converted to an opaque SHA-256 label before it can enter report evidence.

use std::time::Instant;

use grokptah_agent_bridge::provider_observation::InMemoryObservationRecorder;
use grokptah_agent_bridge::{McpControlClient, McpRemoteError};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::local_service::LocalService;
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
    pub provider_run: Option<ProviderRunEvidence>,
    /// Optional successful finite Run used only to bind a live provider
    /// capture when the scenario under test is intentionally interrupted or
    /// failed. The scenario Run remains in `provider_run` and is still fully
    /// represented by the structural trace and durable IDs.
    pub capture_provider_run: Option<ProviderRunEvidence>,
    pub provider_attempt_start: Option<u32>,
    pub capture_attempt_start: Option<u32>,
}

/// Structural fields from one terminal Run. Model-authored text is never
/// retained here; the runner converts this into the positive-schema capture.
#[derive(Debug, Clone)]
pub struct ProviderRunEvidence {
    pub session_id: String,
    pub agent_id: String,
    pub run_id: String,
    pub agent_spec_revision: u64,
    pub checkpoint_id: Option<String>,
    pub parent_run_id: Option<String>,
    pub continuation_context_hash: Option<String>,
    pub continuation_fidelity: Option<String>,
    pub state: String,
    pub stop_cause: Option<String>,
}

#[derive(Clone)]
struct TestAgent {
    session_id: String,
    agent_id: String,
    seed_run: Option<Value>,
}

struct ProbeBuilder<'a> {
    definition: &'a ProbeDefinition,
    started: Instant,
    counters: EvidenceCounters,
    records: Vec<TraceRecord>,
    transitions: Vec<TransitionEvidence>,
    opaque_ids: Vec<OpaqueDurableId>,
    reconnect: ReconnectEvidence,
    restart: RestartEvidence,
    provider_run: Option<ProviderRunEvidence>,
    capture_provider_run: Option<ProviderRunEvidence>,
    provider_attempt_start: Option<u32>,
    capture_attempt_start: Option<u32>,
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
            reconnect: ReconnectEvidence::default(),
            restart: RestartEvidence::default(),
            provider_run: None,
            capture_provider_run: None,
            provider_attempt_start: None,
            capture_attempt_start: None,
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
                reconnect: self.reconnect,
                restart: self.restart,
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
            provider_run: self.provider_run,
            capture_provider_run: self.capture_provider_run,
            provider_attempt_start: self.provider_attempt_start,
            capture_attempt_start: self.capture_attempt_start,
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
        "core-bounded-run-terminal-v1" => Some(&[
            "ptah_create_session",
            "ptah_list_persistent_agents",
            "ptah_submit_task",
            "ptah_cancel",
            "ptah_get_run",
            "ptah_get_events",
        ]),
        "core-reconnect-cursor-v1" => Some(&[
            "ptah_create_session",
            "ptah_list_persistent_agents",
            "ptah_submit_task",
            "ptah_cancel",
            "ptah_get_run",
            "ptah_get_events",
            "ptah_list_runs",
        ]),
        "core-continuation-resume-v1" => Some(&[
            "ptah_create_session",
            "ptah_submit_task",
            "ptah_cancel",
            "ptah_get_run",
            "ptah_list_persistent_agents",
            "ptah_get_persistent_agent",
            "ptah_resume_persistent_agent",
            "ptah_list_runs",
        ]),
        "core-restart-durable-runs-events-v1" => Some(&[
            "ptah_create_session",
            "ptah_list_persistent_agents",
            "ptah_submit_task",
            "ptah_cancel",
            "ptah_list_runs",
            "ptah_get_run",
            "ptah_get_events",
        ]),
        "work-lifecycle-v1" => Some(&[
            "ptah_create_session",
            "ptah_submit_task",
            "ptah_cancel",
            "ptah_get_run",
            "ptah_list_persistent_agents",
            "ptah_create_work",
            "ptah_offer_work",
            "ptah_accept_work",
            "ptah_claim_work",
            "ptah_renew_work",
            "ptah_report_work_progress",
            "ptah_complete_work",
            "ptah_get_work",
        ]),
        "native-policy-default-off-v1" => Some(&[
            "ptah_create_session",
            "ptah_submit_task",
            "ptah_cancel",
            "ptah_get_run",
            "ptah_list_persistent_agents",
            "ptah_get_managed_execution",
            "ptah_get_capacity",
        ]),
        "native-work-to-run-v1" => Some(&[
            "ptah_create_session",
            "ptah_submit_task",
            "ptah_cancel",
            "ptah_get_run",
            "ptah_list_persistent_agents",
            "ptah_set_managed_execution",
            "ptah_create_work",
            "ptah_assign_work",
            "ptah_authorize_work_execution",
            "ptah_get_work",
            "ptah_list_runs",
        ]),
        "native-permission-park-decisions-v1" => Some(&[
            "ptah_create_session",
            "ptah_submit_task",
            "ptah_cancel",
            "ptah_get_run",
            "ptah_list_persistent_agents",
            "ptah_set_managed_execution",
            "ptah_create_work",
            "ptah_assign_work",
            "ptah_authorize_work_execution",
            "ptah_list_execution_intents",
            "ptah_list_inbox",
            "ptah_resolve_work_input",
            "ptah_get_work",
            "ptah_list_runs",
        ]),
        "native-no-duplicate-run-v1" => Some(&[
            "ptah_create_session",
            "ptah_submit_task",
            "ptah_cancel",
            "ptah_get_run",
            "ptah_list_persistent_agents",
            "ptah_set_managed_execution",
            "ptah_create_work",
            "ptah_assign_work",
            "ptah_get_work",
            "ptah_list_work",
            "ptah_list_runs",
            "ptah_list_execution_intents",
            "ptah_get_capacity",
        ]),
        "native-interruption-retry-policy-v1" => Some(&[
            "ptah_create_session",
            "ptah_submit_task",
            "ptah_cancel",
            "ptah_get_run",
            "ptah_list_persistent_agents",
            "ptah_set_managed_execution",
            "ptah_create_work",
            "ptah_assign_work",
            "ptah_authorize_work_execution",
            "ptah_retry_work",
            "ptah_get_work",
            "ptah_list_work",
            "ptah_claim_work",
            "ptah_list_runs",
            "ptah_list_execution_intents",
            "ptah_get_capacity",
        ]),
        "native-restart-intent-adoption-v1" => Some(&[
            "ptah_create_session",
            "ptah_submit_task",
            "ptah_cancel",
            "ptah_get_run",
            "ptah_list_persistent_agents",
            "ptah_set_managed_execution",
            "ptah_create_work",
            "ptah_assign_work",
            "ptah_authorize_work_execution",
            "ptah_list_execution_intents",
            "ptah_get_work",
            "ptah_list_runs",
            "ptah_get_capacity",
        ]),
        _ => None,
    }
}

pub async fn execute_minimal_probe(
    definition: &ProbeDefinition,
    client: &mut McpControlClient,
    workspace: &str,
    provider_attempt_start: Option<u32>,
    provider_recorder: Option<&InMemoryObservationRecorder>,
) -> ProbeExecution {
    let mut probe = ProbeBuilder::new(definition);
    probe.provider_attempt_start = provider_attempt_start;
    probe.capture_attempt_start = provider_attempt_start;
    let outcome = match definition.id.as_str() {
        "core-service-readiness-v1" => readiness(&mut probe, client).await,
        "core-agent-identity-v1" => identity(&mut probe, client, workspace).await,
        "work-idempotency-conflict-v1" => work_idempotency(&mut probe, client, workspace).await,
        "routine-manual-activation-v1" => routine_manual(&mut probe, client, workspace).await,
        "coordinator-parent-child-work-v1" => {
            coordinator_parent_child(&mut probe, client, workspace).await
        }
        "core-bounded-run-terminal-v1" => {
            bounded_run_terminal(&mut probe, client, workspace, provider_recorder).await
        }
        "core-reconnect-cursor-v1" => reconnect_cursor(&mut probe, client, workspace).await,
        "core-continuation-resume-v1" => continuation_resume(&mut probe, client, workspace).await,
        "work-lifecycle-v1" => work_lifecycle(&mut probe, client, workspace).await,
        "native-policy-default-off-v1" => {
            native_policy_default_off(&mut probe, client, workspace).await
        }
        "native-work-to-run-v1" => {
            native_work_to_run(&mut probe, client, workspace, provider_recorder).await
        }
        "native-permission-park-decisions-v1" => {
            native_permission_park_decisions(&mut probe, client, workspace, provider_recorder).await
        }
        "native-no-duplicate-run-v1" => {
            native_no_duplicate_run(&mut probe, client, workspace, provider_recorder).await
        }
        "native-interruption-retry-policy-v1" | "native-restart-intent-adoption-v1" => {
            Err(DiagnosticCode::ProbeImplementationUnavailable)
        }
        _ => Err(DiagnosticCode::ProbeImplementationUnavailable),
    };
    match outcome {
        Ok(()) => probe.finish(ProbeStatus::Passed, DiagnosticCode::Ok),
        Err(code) => probe.finish(ProbeStatus::Failed, code),
    }
}

async fn native_policy_default_off(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
) -> Result<(), DiagnosticCode> {
    let agent = create_agent(probe, client, workspace).await?;
    let policy = probe
        .call(
            client,
            TraceOperationCode::GetManagedExecution,
            "ptah_get_managed_execution",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
                "agent_id": agent.agent_id,
            }),
            vec![
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::AgentId,
            ],
        )
        .await?;
    if policy["agentId"].as_str() != Some(agent.agent_id.as_str())
        || policy["managedExecution"]["enabled"].as_bool() != Some(false)
        || policy["policyRevision"].as_u64().is_none()
        || policy["executor"]["enabled"].as_bool().is_none()
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let capacity = probe
        .call(
            client,
            TraceOperationCode::GetCapacity,
            "ptah_get_capacity",
            json!({}),
            vec![],
        )
        .await?;
    if capacity["health"]["nativeExecutor"]["enabled"].as_bool() != Some(true)
        || !capacity["health"]["nativeExecutorError"].is_null()
    {
        return Err(DiagnosticCode::ServiceNotReady);
    }
    probe.transition(
        EntityKind::ExecutionIntent,
        DurableStateCode::Absent,
        DurableStateCode::Absent,
        None,
    );
    probe.retain_id(DurableIdKind::Session, &agent.session_id);
    probe.retain_id(DurableIdKind::Agent, &agent.agent_id);
    Ok(())
}

async fn native_work_to_run(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
    provider_recorder: Option<&InMemoryObservationRecorder>,
) -> Result<(), DiagnosticCode> {
    let agent = create_agent(probe, client, workspace).await?;
    if let Some(recorder) = provider_recorder {
        probe.provider_attempt_start = recorder
            .snapshot()
            .last()
            .map(|observation| observation.attempt_number() + 1)
            .or(Some(1));
    }
    let policy = probe
        .call(
            client,
            TraceOperationCode::SetManagedExecution,
            "ptah_set_managed_execution",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
                "agent_id": agent.agent_id,
                "policy": {
                    "enabled": true,
                    "maxConcurrentRuns": 1,
                    "bounds": {
                        "maxPromptBytes": 4096,
                        "maxRounds": 2,
                        "maxDurationMs": 30000,
                        "maxTotalTokens": 1000
                    },
                    "retryEligible": false,
                    "requiresApprovalBeforeExecution": true
                }
            }),
            vec![
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::AgentId,
            ],
        )
        .await?;
    if policy["managedExecution"]["enabled"].as_bool() != Some(true)
        || policy["policyRevision"].as_u64().is_none()
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let created = probe
        .call(
            client,
            TraceOperationCode::CreateWork,
            "ptah_create_work",
            json!({
                "request_id": request_id("native-work"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "kind": "native-certification",
                "objective": "Return a short bounded acknowledgement and stop.",
            }),
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
    if created["work"]["state"].as_str() != Some("queued") {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let assigned = probe
        .call(
            client,
            TraceOperationCode::AssignWork,
            "ptah_assign_work",
            json!({
                "request_id": request_id("native-assign"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "work_id": work_id,
                "assigned_agent_id": agent.agent_id,
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::WorkId,
                ArgumentFieldCode::AgentId,
            ],
        )
        .await?;
    if assigned["work"]["state"].as_str() != Some("queued")
        || assigned["work"]["assignedAgentId"].as_str() != Some(agent.agent_id.as_str())
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let authorized = probe
        .call(
            client,
            TraceOperationCode::AuthorizeWorkExecution,
            "ptah_authorize_work_execution",
            json!({
                "request_id": request_id("native-authorize"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "work_id": work_id,
                "reason": "bounded certification authorization",
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::WorkId,
            ],
        )
        .await?;
    if authorized["work"]["workId"].as_str() != Some(work_id.as_str()) {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }

    let mut run_id = None;
    for _ in 0..300 {
        let snapshot = probe
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
        let state = snapshot["work"]["state"]
            .as_str()
            .ok_or(DiagnosticCode::McpResultMalformed)?;
        if matches!(state, "failed" | "cancelled") {
            return Err(DiagnosticCode::TerminalStateMissing);
        }
        run_id = snapshot["attempts"].as_array().and_then(|attempts| {
            attempts.iter().find_map(|attempt| {
                attempt["linkedRunIds"]
                    .as_array()
                    .and_then(|runs| runs.first())
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
        });
        if run_id.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let run_id = run_id.ok_or(DiagnosticCode::Timeout)?;
    let run =
        wait_for_terminal_evidence(probe, client, workspace, &agent.session_id, &run_id).await?;
    if run["state"].as_str() != Some("completed") {
        return Err(DiagnosticCode::TerminalStateMissing);
    }
    let run_agent_id = run["agentId"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    if run_agent_id != agent.agent_id {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    if run["aggregates"]["usageComplete"].as_bool() != Some(true)
        || run["aggregates"]["usagePendingRequests"].as_u64() != Some(0)
    {
        return Err(DiagnosticCode::AuthoritativeUsageMissing);
    }
    let listed = probe
        .call(
            client,
            TraceOperationCode::ListRuns,
            "ptah_list_runs",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
            }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    if !listed["runs"].as_array().is_some_and(|runs| {
        runs.iter()
            .any(|candidate| candidate["runId"].as_str() == Some(run_id.as_str()))
    }) {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let mut final_work = None;
    for _ in 0..100 {
        let snapshot = probe
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
        match snapshot["work"]["state"].as_str() {
            Some("succeeded") => {
                final_work = Some(snapshot);
                break;
            }
            Some("failed" | "cancelled") => return Err(DiagnosticCode::TerminalStateMissing),
            Some("queued" | "leased" | "running" | "awaiting_input" | "awaiting_approval") => {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            _ => return Err(DiagnosticCode::McpResultMalformed),
        }
    }
    let final_work = final_work.ok_or(DiagnosticCode::Timeout)?;
    let linked_count = final_work["attempts"].as_array().map_or(0, |attempts| {
        attempts
            .iter()
            .flat_map(|attempt| attempt["linkedRunIds"].as_array())
            .flatten()
            .filter(|value| value.as_str() == Some(run_id.as_str()))
            .count()
    });
    if final_work["work"]["state"].as_str() != Some("succeeded") || linked_count != 1 {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    probe.provider_run = Some(ProviderRunEvidence {
        session_id: agent.session_id.clone(),
        agent_id: run_agent_id.to_owned(),
        run_id: run_id.clone(),
        agent_spec_revision: run["agentSpecRevision"]
            .as_u64()
            .ok_or(DiagnosticCode::McpResultMalformed)?,
        checkpoint_id: run["checkpointId"].as_str().map(str::to_owned),
        parent_run_id: run["parentRunId"].as_str().map(str::to_owned),
        continuation_context_hash: run["continuationContextHash"].as_str().map(str::to_owned),
        continuation_fidelity: run["continuationFidelity"].as_str().map(str::to_owned),
        state: "completed".into(),
        stop_cause: run["stopCause"].as_str().map(str::to_owned),
    });
    probe.transition(
        EntityKind::Work,
        DurableStateCode::Queued,
        DurableStateCode::Succeeded,
        Some(&work_id),
    );
    probe.transition(
        EntityKind::Run,
        DurableStateCode::Absent,
        DurableStateCode::Completed,
        Some(&run_id),
    );
    probe.retain_id(DurableIdKind::Work, &work_id);
    probe.retain_id(DurableIdKind::Run, &run_id);
    probe.retain_id(DurableIdKind::Session, &agent.session_id);
    probe.retain_id(DurableIdKind::Agent, &agent.agent_id);
    Ok(())
}

/// Exercise the native permission boundary with two independent live Runs.
/// The model must actually elicit a permission request; the lab never treats
/// a prompt or a synthetic question as permission evidence.
async fn native_permission_park_decisions(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
    provider_recorder: Option<&InMemoryObservationRecorder>,
) -> Result<(), DiagnosticCode> {
    let agent = create_agent(probe, client, workspace).await?;
    if let Some(recorder) = provider_recorder {
        probe.provider_attempt_start = recorder
            .snapshot()
            .last()
            .map(|observation| observation.attempt_number() + 1)
            .or(Some(1));
    }
    let policy = probe
        .call(
            client,
            TraceOperationCode::SetManagedExecution,
            "ptah_set_managed_execution",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
                "agent_id": agent.agent_id,
                "policy": {
                    "enabled": true,
                    "maxConcurrentRuns": 2,
                    "bounds": {
                        "maxPromptBytes": 4096,
                        "maxRounds": 4,
                        "maxDurationMs": 30000,
                        "maxTotalTokens": 1000
                    },
                    "retryEligible": false,
                    "requiresApprovalBeforeExecution": true
                }
            }),
            vec![
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::AgentId,
                ArgumentFieldCode::Policy,
            ],
        )
        .await?;
    if policy["managedExecution"]["enabled"].as_bool() != Some(true) {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }

    let objectives = [
        (
            "allow",
            "Request permission to use the write_file tool for one small disposable test file, then stop and report only that the permission was handled.",
        ),
        (
            "deny",
            "Request permission to use the write_file tool for one small disposable test file, then stop and report only that the permission was handled.",
        ),
    ];
    let mut work_ids = Vec::with_capacity(objectives.len());
    for (suffix, objective) in objectives {
        let created = probe
            .call(
                client,
                TraceOperationCode::CreateWork,
                "ptah_create_work",
                json!({
                    "request_id": request_id(&format!("permission-work-{suffix}")),
                    "session_id": agent.session_id,
                    "workspace": workspace,
                    "kind": "native-permission",
                    "objective": objective,
                }),
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
        let assigned = probe
            .call(
                client,
                TraceOperationCode::AssignWork,
                "ptah_assign_work",
                json!({
                    "request_id": request_id(&format!("permission-assign-{suffix}")),
                    "session_id": agent.session_id,
                    "workspace": workspace,
                    "work_id": work_id,
                    "assigned_agent_id": agent.agent_id,
                }),
                vec![
                    ArgumentFieldCode::RequestId,
                    ArgumentFieldCode::SessionId,
                    ArgumentFieldCode::Workspace,
                    ArgumentFieldCode::WorkId,
                    ArgumentFieldCode::AgentId,
                ],
            )
            .await?;
        if assigned["work"]["assignedAgentId"].as_str() != Some(agent.agent_id.as_str()) {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        let authorized = probe
            .call(
                client,
                TraceOperationCode::AuthorizeWorkExecution,
                "ptah_authorize_work_execution",
                json!({
                    "request_id": request_id(&format!("permission-authorize-{suffix}")),
                    "session_id": agent.session_id,
                    "workspace": workspace,
                    "work_id": work_id,
                    "reason": "bounded permission certification authorization",
                }),
                vec![
                    ArgumentFieldCode::RequestId,
                    ArgumentFieldCode::SessionId,
                    ArgumentFieldCode::Workspace,
                    ArgumentFieldCode::WorkId,
                ],
            )
            .await?;
        if authorized["work"]["workId"].as_str() != Some(work_id.as_str()) {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        work_ids.push(work_id);
    }

    let mut permission_cases: Vec<(String, String, String)> = Vec::new();
    for _ in 0..300 {
        let page = probe
            .call(
                client,
                TraceOperationCode::ListInbox,
                "ptah_list_inbox",
                json!({
                    "session_id": agent.session_id,
                    "workspace": workspace,
                    "agent_id": agent.agent_id,
                    "after_seq": 0,
                }),
                vec![
                    ArgumentFieldCode::SessionId,
                    ArgumentFieldCode::Workspace,
                    ArgumentFieldCode::AgentId,
                    ArgumentFieldCode::AfterSequence,
                ],
            )
            .await?;
        let messages = page["messages"]
            .as_array()
            .ok_or(DiagnosticCode::McpResultMalformed)?;
        for message in messages {
            if message["kind"].as_str() != Some("question") {
                continue;
            }
            let Some(work_id) = message["workId"].as_str() else {
                continue;
            };
            if !work_ids.iter().any(|candidate| candidate == work_id)
                || permission_cases
                    .iter()
                    .any(|(candidate, _, _)| candidate == work_id)
            {
                continue;
            }
            let payload = message["payload"]
                .as_object()
                .ok_or(DiagnosticCode::McpResultMalformed)?;
            let permission_id = payload
                .get("permissionId")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or(DiagnosticCode::McpResultMalformed)?
                .to_owned();
            let run_id = payload
                .get("runId")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or(DiagnosticCode::McpResultMalformed)?
                .to_owned();
            Uuid::parse_str(&permission_id).map_err(|_| DiagnosticCode::McpResultMalformed)?;
            permission_cases.push((work_id.to_owned(), permission_id, run_id));
        }
        if permission_cases.len() == work_ids.len() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    if permission_cases.len() != work_ids.len() {
        return Err(DiagnosticCode::Timeout);
    }
    probe.counters.permission_requests = probe
        .counters
        .permission_requests
        .checked_add(
            u64::try_from(permission_cases.len()).map_err(|_| DiagnosticCode::BoundExceeded)?,
        )
        .ok_or(DiagnosticCode::BoundExceeded)?;

    for (work_id, permission_id, run_id) in &permission_cases {
        let intents = probe
            .call(
                client,
                TraceOperationCode::ListExecutionIntents,
                "ptah_list_execution_intents",
                json!({
                    "session_id": agent.session_id,
                    "workspace": workspace,
                }),
                vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
            )
            .await?;
        let intent = intents["intents"]
            .as_array()
            .and_then(|items| {
                items.iter().find(|item| {
                    item["workId"].as_str() == Some(work_id.as_str())
                        && item["runId"].as_str() == Some(run_id.as_str())
                })
            })
            .ok_or(DiagnosticCode::StateTransitionMismatch)?;
        if intent["state"].as_str() != Some("parked")
            || intent["permissionRequestId"].as_str() != Some(permission_id.as_str())
        {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        let work = probe
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
        if work["work"]["state"].as_str() != Some("awaiting_input") {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        probe.transition(
            EntityKind::Permission,
            DurableStateCode::Created,
            DurableStateCode::Parked,
            Some(permission_id),
        );
        probe.retain_id(DurableIdKind::Work, work_id);
        probe.retain_id(DurableIdKind::Run, run_id);
    }

    let invalid_permission = Uuid::new_v4();
    match probe
        .call(
            client,
            TraceOperationCode::ResolveWorkInput,
            "ptah_resolve_work_input",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
                "permission_id": invalid_permission,
                "allow": true,
            }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await
    {
        Err(DiagnosticCode::McpResultMalformed) => {}
        Err(_) => return Err(DiagnosticCode::OracleMismatch),
        Ok(_) => return Err(DiagnosticCode::StateTransitionMismatch),
    }
    for work_id in &work_ids {
        let work = probe
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
        if work["work"]["state"].as_str() != Some("awaiting_input") {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
    }

    let allowed_case = permission_cases
        .iter()
        .find(|(work_id, _, _)| work_id == &work_ids[0])
        .cloned()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    let denied_case = permission_cases
        .iter()
        .find(|(work_id, _, _)| work_id == &work_ids[1])
        .cloned()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    let allowed = resolve_permission(probe, client, workspace, &agent, &allowed_case, true).await?;
    if allowed["allow"].as_bool() != Some(true)
        || allowed["workId"].as_str() != Some(allowed_case.0.as_str())
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    probe.transition(
        EntityKind::Permission,
        DurableStateCode::Parked,
        DurableStateCode::Allowed,
        Some(&allowed_case.1),
    );
    let denied = resolve_permission(probe, client, workspace, &agent, &denied_case, false).await?;
    if denied["allow"].as_bool() != Some(false)
        || denied["workId"].as_str() != Some(denied_case.0.as_str())
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    probe.transition(
        EntityKind::Permission,
        DurableStateCode::Parked,
        DurableStateCode::Denied,
        Some(&denied_case.1),
    );

    let allowed_run =
        wait_for_terminal_evidence(probe, client, workspace, &agent.session_id, &allowed_case.2)
            .await?;
    if allowed_run["state"].as_str() != Some("completed")
        || allowed_run["agentId"].as_str() != Some(agent.agent_id.as_str())
        || allowed_run["aggregates"]["usageComplete"].as_bool() != Some(true)
        || allowed_run["aggregates"]["usagePendingRequests"].as_u64() != Some(0)
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let denied_run =
        wait_for_terminal_evidence(probe, client, workspace, &agent.session_id, &denied_case.2)
            .await?;
    if !matches!(
        denied_run["state"].as_str(),
        Some("completed" | "failed" | "cancelled" | "interrupted" | "limit_reached")
    ) {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let allowed_work =
        wait_for_work_terminal(probe, client, workspace, &agent.session_id, &allowed_case.0)
            .await?;
    let denied_work =
        wait_for_work_terminal(probe, client, workspace, &agent.session_id, &denied_case.0).await?;
    if allowed_work["work"]["state"].as_str() != Some("succeeded")
        || !matches!(
            denied_work["work"]["state"].as_str(),
            Some("succeeded" | "failed" | "cancelled" | "awaiting_approval")
        )
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let runs = probe
        .call(
            client,
            TraceOperationCode::ListRuns,
            "ptah_list_runs",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
            }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    let run_values = runs["runs"]
        .as_array()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    for (_, _, run_id) in [&allowed_case, &denied_case] {
        if run_values
            .iter()
            .filter(|candidate| candidate["runId"].as_str() == Some(run_id.as_str()))
            .count()
            != 1
        {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
    }
    let agent_spec_revision = allowed_run["agentSpecRevision"]
        .as_u64()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    probe.provider_run = Some(ProviderRunEvidence {
        session_id: agent.session_id.clone(),
        agent_id: agent.agent_id.clone(),
        run_id: allowed_case.2.clone(),
        agent_spec_revision,
        checkpoint_id: allowed_run["checkpointId"].as_str().map(str::to_owned),
        parent_run_id: allowed_run["parentRunId"].as_str().map(str::to_owned),
        continuation_context_hash: allowed_run["continuationContextHash"]
            .as_str()
            .map(str::to_owned),
        continuation_fidelity: allowed_run["continuationFidelity"]
            .as_str()
            .map(str::to_owned),
        state: "completed".into(),
        stop_cause: allowed_run["stopCause"].as_str().map(str::to_owned),
    });
    probe.retain_id(DurableIdKind::Session, &agent.session_id);
    probe.retain_id(DurableIdKind::Agent, &agent.agent_id);
    Ok(())
}

async fn resolve_permission(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
    agent: &TestAgent,
    case: &(String, String, String),
    allow: bool,
) -> Result<Value, DiagnosticCode> {
    let permission_id = Uuid::parse_str(&case.1).map_err(|_| DiagnosticCode::McpResultMalformed)?;
    probe
        .call(
            client,
            TraceOperationCode::ResolveWorkInput,
            "ptah_resolve_work_input",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
                "permission_id": permission_id,
                "allow": allow,
            }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await
}

async fn native_no_duplicate_run(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
    provider_recorder: Option<&InMemoryObservationRecorder>,
) -> Result<(), DiagnosticCode> {
    let agent = create_agent(probe, client, workspace).await?;
    if let Some(recorder) = provider_recorder {
        probe.provider_attempt_start = recorder
            .snapshot()
            .last()
            .map(|observation| observation.attempt_number() + 1)
            .or(Some(1));
    }
    let policy = probe
        .call(
            client,
            TraceOperationCode::SetManagedExecution,
            "ptah_set_managed_execution",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
                "agent_id": agent.agent_id,
                "policy": {
                    "enabled": true,
                    "maxConcurrentRuns": 1,
                    "bounds": {
                        "maxPromptBytes": 4096,
                        "maxRounds": 2,
                        "maxDurationMs": 30000,
                        "maxTotalTokens": 1000
                    },
                    "retryEligible": false,
                    "requiresApprovalBeforeExecution": false
                }
            }),
            vec![
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::AgentId,
                ArgumentFieldCode::Policy,
            ],
        )
        .await?;
    if policy["managedExecution"]["enabled"].as_bool() != Some(true) {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let create_request_id = request_id("duplicate-work");
    let create_arguments = json!({
        "request_id": create_request_id,
        "session_id": agent.session_id,
        "workspace": workspace,
        "kind": "native-duplicate-certification",
        "objective": "Return one short bounded acknowledgement and stop without tools.",
    });
    let created = probe
        .call(
            client,
            TraceOperationCode::CreateWork,
            "ptah_create_work",
            create_arguments.clone(),
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
    let replayed = probe
        .call(
            client,
            TraceOperationCode::CreateWork,
            "ptah_create_work",
            create_arguments,
            vec![ArgumentFieldCode::RequestId],
        )
        .await?;
    if replayed["work"]["workId"].as_str() != Some(work_id.as_str()) {
        return Err(DiagnosticCode::IdempotencyReplayMismatch);
    }
    let assign_request_id = request_id("duplicate-assign");
    let assign_arguments = json!({
        "request_id": assign_request_id,
        "session_id": agent.session_id,
        "workspace": workspace,
        "work_id": work_id,
        "assigned_agent_id": agent.agent_id,
    });
    let assigned = probe
        .call(
            client,
            TraceOperationCode::AssignWork,
            "ptah_assign_work",
            assign_arguments.clone(),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::WorkId,
                ArgumentFieldCode::AgentId,
            ],
        )
        .await?;
    let assigned_work_id = required_string(&assigned, &["work", "workId"])?;
    let replayed_assignment = probe
        .call(
            client,
            TraceOperationCode::AssignWork,
            "ptah_assign_work",
            assign_arguments,
            vec![ArgumentFieldCode::RequestId],
        )
        .await?;
    if assigned_work_id != work_id
        || replayed_assignment["work"]["workId"].as_str() != Some(work_id.as_str())
    {
        return Err(DiagnosticCode::IdempotencyReplayMismatch);
    }

    let mut run_id = None;
    for _ in 0..300 {
        let work = probe
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
        run_id = work["attempts"].as_array().and_then(|attempts| {
            attempts.iter().find_map(|attempt| {
                attempt["linkedRunIds"]
                    .as_array()
                    .and_then(|runs| runs.first())
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
        });
        if run_id.is_some() {
            break;
        }
        if matches!(work["work"]["state"].as_str(), Some("failed" | "cancelled")) {
            return Err(DiagnosticCode::TerminalStateMissing);
        }
        let _ = probe
            .call(
                client,
                TraceOperationCode::GetCapacity,
                "ptah_get_capacity",
                json!({}),
                vec![],
            )
            .await?;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let run_id = run_id.ok_or(DiagnosticCode::Timeout)?;
    let run =
        wait_for_terminal_evidence(probe, client, workspace, &agent.session_id, &run_id).await?;
    if run["state"].as_str() != Some("completed")
        || run["agentId"].as_str() != Some(agent.agent_id.as_str())
        || run["aggregates"]["usageComplete"].as_bool() != Some(true)
        || run["aggregates"]["usagePendingRequests"].as_u64() != Some(0)
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let work =
        wait_for_work_terminal(probe, client, workspace, &agent.session_id, &work_id).await?;
    let linked_count = work["attempts"].as_array().map_or(0, |attempts| {
        attempts
            .iter()
            .flat_map(|attempt| attempt["linkedRunIds"].as_array())
            .flatten()
            .filter(|value| value.as_str() == Some(run_id.as_str()))
            .count()
    });
    if work["work"]["state"].as_str() != Some("succeeded") || linked_count != 1 {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let work_set = probe
        .call(
            client,
            TraceOperationCode::ListWork,
            "ptah_list_work",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
            }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    if !work_set["work"].as_array().is_some_and(|items| {
        items
            .iter()
            .filter(|item| item["workId"] == work_id)
            .count()
            == 1
    }) {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let intents = probe
        .call(
            client,
            TraceOperationCode::ListExecutionIntents,
            "ptah_list_execution_intents",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
            }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    let matching_intents = intents["intents"].as_array().map_or(0, |items| {
        items
            .iter()
            .filter(|item| item["workId"].as_str() == Some(work_id.as_str()))
            .count()
    });
    if matching_intents != 1 {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let runs = probe
        .call(
            client,
            TraceOperationCode::ListRuns,
            "ptah_list_runs",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
            }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    if runs["runs"]
        .as_array()
        .is_none_or(|items| items.iter().filter(|item| item["runId"] == run_id).count() != 1)
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    probe.provider_run = Some(ProviderRunEvidence {
        session_id: agent.session_id.clone(),
        agent_id: agent.agent_id.clone(),
        run_id: run_id.clone(),
        agent_spec_revision: run["agentSpecRevision"]
            .as_u64()
            .ok_or(DiagnosticCode::McpResultMalformed)?,
        checkpoint_id: run["checkpointId"].as_str().map(str::to_owned),
        parent_run_id: run["parentRunId"].as_str().map(str::to_owned),
        continuation_context_hash: run["continuationContextHash"].as_str().map(str::to_owned),
        continuation_fidelity: run["continuationFidelity"].as_str().map(str::to_owned),
        state: "completed".into(),
        stop_cause: run["stopCause"].as_str().map(str::to_owned),
    });
    probe.transition(
        EntityKind::ExecutionIntent,
        DurableStateCode::Absent,
        DurableStateCode::Admitted,
        Some(work_id.as_str()),
    );
    probe.transition(
        EntityKind::Attempt,
        DurableStateCode::Absent,
        DurableStateCode::Leased,
        Some(work_id.as_str()),
    );
    probe.transition(
        EntityKind::Run,
        DurableStateCode::Absent,
        DurableStateCode::Queued,
        Some(&run_id),
    );
    probe.retain_id(DurableIdKind::Session, &agent.session_id);
    probe.retain_id(DurableIdKind::Agent, &agent.agent_id);
    probe.retain_id(DurableIdKind::Work, &work_id);
    probe.retain_id(DurableIdKind::Run, &run_id);
    Ok(())
}

/// Execute the owned-local restart probe. Restart control is deliberately
/// supplied by the local service adapter rather than exposed through MCP or
/// accepted from an attached target.
pub async fn execute_restart_probe(
    definition: &ProbeDefinition,
    service: &mut LocalService,
    workspace: &str,
    provider_recorder: Option<&InMemoryObservationRecorder>,
) -> ProbeExecution {
    let mut probe = ProbeBuilder::new(definition);
    let outcome =
        restart_durable_runs_events(&mut probe, service, workspace, provider_recorder).await;
    match outcome {
        Ok(()) => probe.finish(ProbeStatus::Passed, DiagnosticCode::Ok),
        Err(code) => probe.finish(ProbeStatus::Failed, code),
    }
}

/// Execute the native persistent-Agent restart probe. Restart is owned by the
/// local campaign host; attached services are never asked to restart through
/// the public MCP contract.
pub async fn execute_native_restart_probe(
    definition: &ProbeDefinition,
    service: &mut LocalService,
    workspace: &str,
    provider_recorder: Option<&InMemoryObservationRecorder>,
) -> ProbeExecution {
    let mut probe = ProbeBuilder::new(definition);
    let outcome =
        native_restart_intent_adoption(&mut probe, service, workspace, provider_recorder).await;
    match outcome {
        Ok(()) => probe.finish(ProbeStatus::Passed, DiagnosticCode::Ok),
        Err(code) => probe.finish(ProbeStatus::Failed, code),
    }
}

/// Execute the native interruption/retry policy probe. It deliberately uses
/// the public restart boundary to create an interruption; it never treats a
/// cancellation as a provider failure.
pub async fn execute_native_interruption_retry_probe(
    definition: &ProbeDefinition,
    service: &mut LocalService,
    workspace: &str,
    provider_recorder: Option<&InMemoryObservationRecorder>,
) -> ProbeExecution {
    let mut probe = ProbeBuilder::new(definition);
    let outcome =
        native_interruption_retry_policy(&mut probe, service, workspace, provider_recorder).await;
    match outcome {
        Ok(()) => probe.finish(ProbeStatus::Passed, DiagnosticCode::Ok),
        Err(code) => probe.finish(ProbeStatus::Failed, code),
    }
}

struct NativeRunningWork {
    work_id: String,
    attempt_id: String,
    run_id: String,
}

async fn native_restart_intent_adoption(
    probe: &mut ProbeBuilder<'_>,
    service: &mut LocalService,
    workspace: &str,
    provider_recorder: Option<&InMemoryObservationRecorder>,
) -> Result<(), DiagnosticCode> {
    let mut client = service.client();
    client
        .initialize()
        .await
        .map_err(|_| DiagnosticCode::McpInitializeFailed)?;
    let agent = create_agent(probe, &mut client, workspace).await?;
    bind_capture_to_seed(probe, &agent)?;
    mark_native_target_start(probe, provider_recorder);
    let work_id = create_and_authorize_native_work(
        probe,
        &mut client,
        &agent,
        workspace,
        "Perform a bounded multi-step workspace inspection before returning. Do not answer immediately; use the available safe inspection tools and keep the work finite.",
    )
    .await?;
    let running = wait_for_native_running(probe, &mut client, workspace, &agent, &work_id).await?;
    record_native_running(probe, &agent, &running);

    probe.push_trace(TraceOperationCode::Disconnect, vec![], None)?;
    client
        .close_session()
        .await
        .map_err(|_| DiagnosticCode::McpCallFailed)?;
    probe.push_trace(TraceOperationCode::Restart, vec![], None)?;
    service
        .restart()
        .await
        .map_err(|_| DiagnosticCode::RestartControlUnavailable)?;
    probe.counters.restarts = probe
        .counters
        .restarts
        .checked_add(1)
        .ok_or(DiagnosticCode::BoundExceeded)?;
    probe.push_trace(TraceOperationCode::Reconnect, vec![], None)?;
    client = service.client();
    client
        .initialize()
        .await
        .map_err(|_| DiagnosticCode::McpInitializeFailed)?;

    let (work, run, intent) =
        wait_for_native_restart_convergence(probe, &mut client, workspace, &agent, &running)
            .await?;
    if work["work"]["state"].as_str() != Some("failed")
        || run["state"].as_str() != Some("interrupted")
        || intent["state"].as_str() != Some("finalized")
    {
        return Err(DiagnosticCode::RestartRecoveryFailed);
    }
    let attempts = work["attempts"]
        .as_array()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    let attempt = attempts
        .iter()
        .find(|attempt| attempt["attemptId"].as_str() == Some(running.attempt_id.as_str()))
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    if attempt["state"].as_str() != Some("expired") {
        return Err(DiagnosticCode::RestartRecoveryFailed);
    }
    let listed_runs = probe
        .call(
            &mut client,
            TraceOperationCode::ListRuns,
            "ptah_list_runs",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
            }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    let linked_runs = listed_runs["runs"].as_array().map_or(0, |runs| {
        runs.iter()
            .filter(|candidate| candidate["runId"].as_str() == Some(running.run_id.as_str()))
            .count()
    });
    if linked_runs != 1 {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let capacity = probe
        .call(
            &mut client,
            TraceOperationCode::GetCapacity,
            "ptah_get_capacity",
            json!({}),
            vec![],
        )
        .await?;
    if capacity["health"]["nativeExecutor"]["enabled"].as_bool() != Some(true)
        || !capacity["health"]["nativeExecutorError"].is_null()
    {
        return Err(DiagnosticCode::ServiceNotReady);
    }
    probe.provider_run = Some(provider_run_from_value(&agent, &run)?);
    probe.restart = RestartEvidence {
        attempted: true,
        host_owned: true,
        durable_read_recovered: true,
        event_cursor_recovered: false,
        implicit_execution_observed: false,
    };
    probe.reconnect = ReconnectEvidence {
        attempted: true,
        reinitialized: true,
        cursor_before: None,
        cursor_after: None,
        continuity_proven: false,
    };
    probe.retain_id(DurableIdKind::Work, &running.work_id);
    probe.retain_id(DurableIdKind::Attempt, &running.attempt_id);
    probe.retain_id(DurableIdKind::Run, &running.run_id);
    Ok(())
}

async fn native_interruption_retry_policy(
    probe: &mut ProbeBuilder<'_>,
    service: &mut LocalService,
    workspace: &str,
    provider_recorder: Option<&InMemoryObservationRecorder>,
) -> Result<(), DiagnosticCode> {
    let mut client = service.client();
    client
        .initialize()
        .await
        .map_err(|_| DiagnosticCode::McpInitializeFailed)?;
    let agent = create_agent(probe, &mut client, workspace).await?;
    bind_capture_to_seed(probe, &agent)?;
    mark_native_target_start(probe, provider_recorder);
    let work_id = create_and_authorize_native_work(
        probe,
        &mut client,
        &agent,
        workspace,
        "Perform a bounded multi-step workspace inspection before returning. Do not answer immediately; use safe inspection tools and keep the work finite.",
    )
    .await?;
    let running = wait_for_native_running(probe, &mut client, workspace, &agent, &work_id).await?;
    record_native_running(probe, &agent, &running);

    probe.push_trace(TraceOperationCode::Disconnect, vec![], None)?;
    client
        .close_session()
        .await
        .map_err(|_| DiagnosticCode::McpCallFailed)?;
    probe.push_trace(TraceOperationCode::Restart, vec![], None)?;
    service
        .restart()
        .await
        .map_err(|_| DiagnosticCode::RestartControlUnavailable)?;
    probe.counters.restarts = probe
        .counters
        .restarts
        .checked_add(1)
        .ok_or(DiagnosticCode::BoundExceeded)?;
    probe.push_trace(TraceOperationCode::Reconnect, vec![], None)?;
    client = service.client();
    client
        .initialize()
        .await
        .map_err(|_| DiagnosticCode::McpInitializeFailed)?;

    let (failed_work, failed_run, intent) =
        wait_for_native_restart_convergence(probe, &mut client, workspace, &agent, &running)
            .await?;
    if failed_work["work"]["state"].as_str() != Some("failed")
        || failed_run["state"].as_str() != Some("interrupted")
        || intent["state"].as_str() != Some("finalized")
    {
        return Err(DiagnosticCode::RestartRecoveryFailed);
    }
    let attempts = failed_work["attempts"]
        .as_array()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    let original_attempt = attempts
        .iter()
        .find(|attempt| attempt["attemptId"].as_str() == Some(running.attempt_id.as_str()))
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    if original_attempt["state"].as_str() != Some("expired") {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }

    let revision = failed_work["work"]["revision"]
        .as_u64()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    let retried = probe
        .call(
            &mut client,
            TraceOperationCode::RetryWork,
            "ptah_retry_work",
            json!({
                "request_id": request_id("native-retry"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "work_id": work_id,
                "reason": "explicit external retry after owned interruption",
                "expected_revision": revision,
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::WorkId,
                ArgumentFieldCode::ExpectedRevision,
            ],
        )
        .await?;
    if retried["work"]["state"].as_str() != Some("queued") {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    probe.transition(
        EntityKind::Work,
        DurableStateCode::Failed,
        DurableStateCode::Queued,
        Some(&work_id),
    );

    for _ in 0..8 {
        let _ = probe
            .call(
                &mut client,
                TraceOperationCode::GetCapacity,
                "ptah_get_capacity",
                json!({}),
                vec![],
            )
            .await?;
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    let after_ticks = probe
        .call(
            &mut client,
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
    let after_attempts = after_ticks["attempts"]
        .as_array()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    let native_run_count = after_attempts
        .iter()
        .flat_map(|attempt| attempt["linkedRunIds"].as_array())
        .flatten()
        .filter(|run| run.as_str() == Some(running.run_id.as_str()))
        .count();
    if after_ticks["work"]["state"].as_str() != Some("queued")
        || after_attempts.len() != 1
        || native_run_count != 1
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let intents = probe
        .call(
            &mut client,
            TraceOperationCode::ListExecutionIntents,
            "ptah_list_execution_intents",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
            }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    let matching_intents = intents["intents"].as_array().map_or(0, |values| {
        values
            .iter()
            .filter(|value| value["workId"].as_str() == Some(work_id.as_str()))
            .count()
    });
    if matching_intents != 1 {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let listed_runs = probe
        .call(
            &mut client,
            TraceOperationCode::ListRuns,
            "ptah_list_runs",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
            }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    let run_count = listed_runs["runs"].as_array().map_or(0, |runs| {
        runs.iter()
            .filter(|value| value["runId"].as_str() == Some(running.run_id.as_str()))
            .count()
    });
    if run_count != 1 {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }

    let claimed = probe
        .call(
            &mut client,
            TraceOperationCode::ClaimWork,
            "ptah_claim_work",
            json!({
                "request_id": request_id("native-external-claim"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "work_id": work_id,
                "agent_id": agent.agent_id,
                "lease_ms": 30000,
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::WorkId,
                ArgumentFieldCode::AgentId,
                ArgumentFieldCode::LeaseMs,
            ],
        )
        .await?;
    let claimed_attempt = required_string(&claimed, &["attempt", "attemptId"])?;
    if claimed["work"]["state"].as_str() != Some("leased") {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    probe.transition(
        EntityKind::Work,
        DurableStateCode::Queued,
        DurableStateCode::Leased,
        Some(&work_id),
    );
    probe.retain_id(DurableIdKind::Attempt, &claimed_attempt);
    probe.provider_run = Some(provider_run_from_value(&agent, &failed_run)?);
    probe.restart = RestartEvidence {
        attempted: true,
        host_owned: true,
        durable_read_recovered: true,
        event_cursor_recovered: false,
        implicit_execution_observed: false,
    };
    probe.reconnect = ReconnectEvidence {
        attempted: true,
        reinitialized: true,
        cursor_before: None,
        cursor_after: None,
        continuity_proven: false,
    };
    probe.retain_id(DurableIdKind::Work, &work_id);
    probe.retain_id(DurableIdKind::Attempt, &running.attempt_id);
    probe.retain_id(DurableIdKind::Run, &running.run_id);
    Ok(())
}

async fn create_and_authorize_native_work(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    agent: &TestAgent,
    workspace: &str,
    objective: &str,
) -> Result<String, DiagnosticCode> {
    let created = probe
        .call(
            client,
            TraceOperationCode::CreateWork,
            "ptah_create_work",
            json!({
                "request_id": request_id("native-work"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "kind": "native-certification",
                "objective": objective,
                "policy": {
                    "bounds": {
                        "maxPromptBytes": 8192,
                        "maxRounds": 8,
                        "maxDurationMs": 60000,
                        "maxTotalTokens": 2000
                    },
                    "retry": {
                        "maxAttempts": 2,
                        "retryFailed": true,
                        "retryExpired": true,
                        "backoffMs": 0
                    },
                    "requiresApproval": false,
                    "maxConcurrentAttempts": 1,
                    "managedExecution": "inherit"
                }
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::Kind,
                ArgumentFieldCode::Objective,
                ArgumentFieldCode::Policy,
            ],
        )
        .await?;
    let work_id = required_string(&created, &["work", "workId"])?;
    if created["work"]["state"].as_str() != Some("queued") {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let assigned = probe
        .call(
            client,
            TraceOperationCode::AssignWork,
            "ptah_assign_work",
            json!({
                "request_id": request_id("native-assign"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "work_id": work_id,
                "assigned_agent_id": agent.agent_id,
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::WorkId,
                ArgumentFieldCode::AgentId,
            ],
        )
        .await?;
    if assigned["work"]["state"].as_str() != Some("queued") {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let authorized = probe
        .call(
            client,
            TraceOperationCode::AuthorizeWorkExecution,
            "ptah_authorize_work_execution",
            json!({
                "request_id": request_id("native-authorize"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "work_id": work_id,
                "reason": "bounded certification authorization",
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::WorkId,
            ],
        )
        .await?;
    if authorized["work"]["workId"].as_str() != Some(work_id.as_str()) {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    Ok(work_id)
}

fn mark_native_target_start(
    probe: &mut ProbeBuilder<'_>,
    provider_recorder: Option<&InMemoryObservationRecorder>,
) {
    if let Some(recorder) = provider_recorder {
        probe.provider_attempt_start = recorder
            .snapshot()
            .last()
            .map(|observation| observation.attempt_number() + 1)
            .or(Some(1));
    }
}

async fn wait_for_native_running(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
    agent: &TestAgent,
    work_id: &str,
) -> Result<NativeRunningWork, DiagnosticCode> {
    for _ in 0..300 {
        let snapshot = probe
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
        let state = snapshot["work"]["state"]
            .as_str()
            .ok_or(DiagnosticCode::McpResultMalformed)?;
        let attempt = snapshot["attempts"]
            .as_array()
            .and_then(|attempts| attempts.last())
            .cloned();
        if let Some(attempt) = attempt {
            let attempt_id = attempt["attemptId"]
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or(DiagnosticCode::McpResultMalformed)?
                .to_owned();
            let run_id = attempt["linkedRunIds"]
                .as_array()
                .and_then(|runs| runs.first())
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            if let Some(run_id) = run_id {
                let run = probe
                    .call(
                        client,
                        TraceOperationCode::GetRun,
                        "ptah_get_run",
                        json!({
                            "session_id": agent.session_id,
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
                    Some("running") => {
                        return Ok(NativeRunningWork {
                            work_id: work_id.to_owned(),
                            attempt_id,
                            run_id,
                        })
                    }
                    Some(
                        "completed" | "failed" | "cancelled" | "interrupted" | "limit_reached",
                    ) => return Err(DiagnosticCode::TerminalStateMissing),
                    Some("queued" | "waiting") => {}
                    _ => return Err(DiagnosticCode::McpResultMalformed),
                }
            }
        }
        if matches!(state, "failed" | "cancelled" | "succeeded") {
            return Err(DiagnosticCode::TerminalStateMissing);
        }
        if probe.counters.tool_calls.is_multiple_of(5) {
            let _ = probe
                .call(
                    client,
                    TraceOperationCode::GetCapacity,
                    "ptah_get_capacity",
                    json!({}),
                    vec![],
                )
                .await?;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Err(DiagnosticCode::Timeout)
}

fn record_native_running(
    probe: &mut ProbeBuilder<'_>,
    agent: &TestAgent,
    running: &NativeRunningWork,
) {
    probe.transition(
        EntityKind::Work,
        DurableStateCode::Queued,
        DurableStateCode::Running,
        Some(&running.work_id),
    );
    probe.transition(
        EntityKind::Run,
        DurableStateCode::Absent,
        DurableStateCode::Running,
        Some(&running.run_id),
    );
    probe.transition(
        EntityKind::Attempt,
        DurableStateCode::Absent,
        DurableStateCode::Leased,
        Some(&running.attempt_id),
    );
    probe.transition(
        EntityKind::ExecutionIntent,
        DurableStateCode::Absent,
        DurableStateCode::Admitted,
        None,
    );
    probe.retain_id(DurableIdKind::Session, &agent.session_id);
    probe.retain_id(DurableIdKind::Agent, &agent.agent_id);
}

async fn wait_for_native_restart_convergence(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
    agent: &TestAgent,
    running: &NativeRunningWork,
) -> Result<(Value, Value, Value), DiagnosticCode> {
    for _ in 0..300 {
        let work = probe
            .call(
                client,
                TraceOperationCode::GetWork,
                "ptah_get_work",
                json!({
                    "session_id": agent.session_id,
                    "workspace": workspace,
                    "work_id": running.work_id,
                }),
                vec![
                    ArgumentFieldCode::SessionId,
                    ArgumentFieldCode::Workspace,
                    ArgumentFieldCode::WorkId,
                ],
            )
            .await?;
        let run = probe
            .call(
                client,
                TraceOperationCode::GetRun,
                "ptah_get_run",
                json!({
                    "session_id": agent.session_id,
                    "workspace": workspace,
                    "run_id": running.run_id,
                }),
                vec![
                    ArgumentFieldCode::SessionId,
                    ArgumentFieldCode::Workspace,
                    ArgumentFieldCode::RunId,
                ],
            )
            .await?;
        let intents = probe
            .call(
                client,
                TraceOperationCode::ListExecutionIntents,
                "ptah_list_execution_intents",
                json!({
                    "session_id": agent.session_id,
                    "workspace": workspace,
                }),
                vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
            )
            .await?;
        let intent = intents["intents"].as_array().and_then(|values| {
            values.iter().find(|value| {
                value["workId"].as_str() == Some(running.work_id.as_str())
                    && value["runId"].as_str() == Some(running.run_id.as_str())
            })
        });
        if let Some(intent) = intent {
            if work["work"]["state"].as_str() == Some("failed")
                && run["state"].as_str() == Some("interrupted")
                && intent["state"].as_str() == Some("finalized")
            {
                probe.transition(
                    EntityKind::Run,
                    DurableStateCode::Running,
                    DurableStateCode::Interrupted,
                    Some(&running.run_id),
                );
                probe.transition(
                    EntityKind::Attempt,
                    DurableStateCode::Running,
                    DurableStateCode::Expired,
                    Some(&running.attempt_id),
                );
                probe.transition(
                    EntityKind::Work,
                    DurableStateCode::Running,
                    DurableStateCode::Failed,
                    Some(&running.work_id),
                );
                probe.transition(
                    EntityKind::ExecutionIntent,
                    DurableStateCode::Admitted,
                    DurableStateCode::Finalized,
                    intent["intentId"].as_str(),
                );
                return Ok((work, run, intent.clone()));
            }
            if matches!(
                run["state"].as_str(),
                Some("completed" | "failed" | "cancelled" | "limit_reached")
            ) {
                return Err(DiagnosticCode::TerminalStateMissing);
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err(DiagnosticCode::Timeout)
}

fn provider_run_from_value(
    agent: &TestAgent,
    run: &Value,
) -> Result<ProviderRunEvidence, DiagnosticCode> {
    Ok(ProviderRunEvidence {
        session_id: agent.session_id.clone(),
        agent_id: agent.agent_id.clone(),
        run_id: required_string(run, &["runId"])?,
        agent_spec_revision: run["agentSpecRevision"]
            .as_u64()
            .ok_or(DiagnosticCode::McpResultMalformed)?,
        checkpoint_id: run["checkpointId"].as_str().map(str::to_owned),
        parent_run_id: run["parentRunId"].as_str().map(str::to_owned),
        continuation_context_hash: run["continuationContextHash"].as_str().map(str::to_owned),
        continuation_fidelity: run["continuationFidelity"].as_str().map(str::to_owned),
        state: run["state"]
            .as_str()
            .ok_or(DiagnosticCode::McpResultMalformed)?
            .to_owned(),
        stop_cause: run["stopCause"].as_str().map(str::to_owned),
    })
}

async fn restart_durable_runs_events(
    probe: &mut ProbeBuilder<'_>,
    service: &mut LocalService,
    workspace: &str,
    provider_recorder: Option<&InMemoryObservationRecorder>,
) -> Result<(), DiagnosticCode> {
    let mut client = service.client();
    client
        .initialize()
        .await
        .map_err(|_| DiagnosticCode::McpInitializeFailed)?;
    let agent = create_agent(probe, &mut client, workspace).await?;
    if let Some(recorder) = provider_recorder {
        probe.provider_attempt_start = recorder
            .snapshot()
            .last()
            .map(|observation| observation.attempt_number() + 1)
            .or(Some(1));
    }
    let submitted = probe
        .call(
            &mut client,
            TraceOperationCode::SubmitRun,
            "ptah_submit_task",
            json!({
                "request_id": request_id("restart-run"),
                "session_id": agent.session_id,
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
    let run = wait_for_terminal_evidence(probe, &mut client, workspace, &agent.session_id, &run_id)
        .await?;
    if run["state"].as_str() != Some("completed") {
        return Err(DiagnosticCode::TerminalStateMissing);
    }
    let before_events =
        run_events(probe, &mut client, workspace, &agent.session_id, &run_id).await?;
    let before_runs = probe
        .call(
            &mut client,
            TraceOperationCode::ListRuns,
            "ptah_list_runs",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
            }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    if before_events.is_empty()
        || !before_runs["runs"].as_array().is_some_and(|runs| {
            runs.iter()
                .any(|candidate| candidate["runId"].as_str() == Some(run_id.as_str()))
        })
    {
        return Err(DiagnosticCode::McpResultMalformed);
    }

    probe.push_trace(TraceOperationCode::Disconnect, vec![], None)?;
    client
        .close_session()
        .await
        .map_err(|_| DiagnosticCode::McpCallFailed)?;
    probe.push_trace(TraceOperationCode::Restart, vec![], None)?;
    service
        .restart()
        .await
        .map_err(|_| DiagnosticCode::RestartControlUnavailable)?;
    probe.counters.restarts = probe
        .counters
        .restarts
        .checked_add(1)
        .ok_or(DiagnosticCode::BoundExceeded)?;
    probe.push_trace(TraceOperationCode::Reconnect, vec![], None)?;
    client = service.client();
    client
        .initialize()
        .await
        .map_err(|_| DiagnosticCode::McpInitializeFailed)?;
    let after_runs = probe
        .call(
            &mut client,
            TraceOperationCode::ListRuns,
            "ptah_list_runs",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
            }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    let recovered = after_runs["runs"].as_array().and_then(|runs| {
        runs.iter()
            .find(|candidate| candidate["runId"].as_str() == Some(run_id.as_str()))
    });
    if recovered.is_none()
        || recovered.and_then(|value| value["state"].as_str()) != Some("completed")
    {
        return Err(DiagnosticCode::RestartRecoveryFailed);
    }
    let after_events =
        run_events(probe, &mut client, workspace, &agent.session_id, &run_id).await?;
    if before_events != after_events {
        return Err(DiagnosticCode::CursorContinuityLost);
    }
    probe.restart = RestartEvidence {
        attempted: true,
        host_owned: true,
        durable_read_recovered: true,
        event_cursor_recovered: true,
        implicit_execution_observed: false,
    };
    probe.reconnect = ReconnectEvidence {
        attempted: true,
        reinitialized: true,
        cursor_before: before_events.last().copied(),
        cursor_after: after_events.last().copied(),
        continuity_proven: true,
    };
    probe.transition(
        EntityKind::Service,
        DurableStateCode::Ready,
        DurableStateCode::Starting,
        None,
    );
    probe.transition(
        EntityKind::Service,
        DurableStateCode::Starting,
        DurableStateCode::Ready,
        None,
    );
    probe.transition(
        EntityKind::Run,
        DurableStateCode::Completed,
        DurableStateCode::Completed,
        Some(&run_id),
    );
    probe.retain_id(DurableIdKind::Run, &run_id);
    Ok(())
}

async fn work_lifecycle(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
) -> Result<(), DiagnosticCode> {
    let agent = create_agent(probe, client, workspace).await?;
    let created = probe
        .call(
            client,
            TraceOperationCode::CreateWork,
            "ptah_create_work",
            json!({
                "request_id": request_id("lifecycle-work"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "kind": "certification_lifecycle",
                "objective": "Bounded public-MCP Work lifecycle",
            }),
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
    if created["work"]["state"].as_str() != Some("queued") {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let offered = probe
        .call(
            client,
            TraceOperationCode::OfferWork,
            "ptah_offer_work",
            json!({
                "request_id": request_id("lifecycle-offer"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "work_id": work_id,
                "agent_id": agent.agent_id,
                "reason": "bounded certification offer",
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::WorkId,
                ArgumentFieldCode::AgentId,
            ],
        )
        .await?;
    if offered["work"]["state"].as_str() != Some("offered") {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let accepted = probe
        .call(
            client,
            TraceOperationCode::AcceptWork,
            "ptah_accept_work",
            json!({
                "request_id": request_id("lifecycle-accept"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "work_id": work_id,
                "agent_id": agent.agent_id,
                "reason": "bounded certification acceptance",
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::WorkId,
                ArgumentFieldCode::AgentId,
            ],
        )
        .await?;
    if accepted["work"]["state"].as_str() != Some("accepted") {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let claimed = probe
        .call(
            client,
            TraceOperationCode::ClaimWork,
            "ptah_claim_work",
            json!({
                "request_id": request_id("lifecycle-claim"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "work_id": work_id,
                "lease_ms": 60_000,
                "agent_id": agent.agent_id,
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::WorkId,
                ArgumentFieldCode::LeaseMs,
                ArgumentFieldCode::AgentId,
            ],
        )
        .await?;
    if claimed["work"]["state"].as_str() != Some("claimed") {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let attempt_id = required_string(&claimed, &["attempt", "attemptId"])?;
    let lease_token = required_string(&claimed, &["leaseToken"])?;
    probe
        .call(
            client,
            TraceOperationCode::RenewWork,
            "ptah_renew_work",
            json!({
                "request_id": request_id("lifecycle-renew"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "work_id": work_id,
                "attempt_id": attempt_id,
                "lease_token": lease_token,
                "lease_ms": 60_000,
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::WorkId,
                ArgumentFieldCode::AttemptId,
                ArgumentFieldCode::LeaseMs,
            ],
        )
        .await?;
    probe
        .call(
            client,
            TraceOperationCode::ProgressWork,
            "ptah_report_work_progress",
            json!({
                "request_id": request_id("lifecycle-progress"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "work_id": work_id,
                "attempt_id": attempt_id,
                "lease_token": lease_token,
                "summary": "bounded lifecycle progress",
                "percent": 50,
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::WorkId,
                ArgumentFieldCode::AttemptId,
                ArgumentFieldCode::Percent,
            ],
        )
        .await?;
    let completed = probe
        .call(
            client,
            TraceOperationCode::CompleteWork,
            "ptah_complete_work",
            json!({
                "request_id": request_id("lifecycle-complete"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "work_id": work_id,
                "attempt_id": attempt_id,
                "lease_token": lease_token,
                "summary": "bounded lifecycle completed",
                "evidence": ["public_mcp_lifecycle"],
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::WorkId,
                ArgumentFieldCode::AttemptId,
            ],
        )
        .await?;
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
    if completed["work"]["state"].as_str() != Some("completed")
        || observed["work"]["state"].as_str() != Some("completed")
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    for (from, to) in [
        (DurableStateCode::Created, DurableStateCode::Offered),
        (DurableStateCode::Offered, DurableStateCode::Accepted),
        (DurableStateCode::Accepted, DurableStateCode::Claimed),
        (DurableStateCode::Claimed, DurableStateCode::Completed),
    ] {
        probe.transition(EntityKind::Work, from, to, Some(&work_id));
    }
    probe.retain_id(DurableIdKind::Work, &work_id);
    probe.retain_id(DurableIdKind::Attempt, &attempt_id);
    Ok(())
}

async fn reconnect_cursor(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
) -> Result<(), DiagnosticCode> {
    let agent = create_agent(probe, client, workspace).await?;
    let submitted = probe
        .call(
            client,
            TraceOperationCode::SubmitRun,
            "ptah_submit_task",
            json!({
                "request_id": request_id("reconnect-run"),
                "session_id": agent.session_id,
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
    if !wait_for_terminal_seed(probe, client, workspace, &agent.session_id, &run_id).await? {
        return Err(DiagnosticCode::TerminalStateMissing);
    }
    let before = run_events(probe, client, workspace, &agent.session_id, &run_id).await?;
    let listed_before = probe
        .call(
            client,
            TraceOperationCode::ListRuns,
            "ptah_list_runs",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
            }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    if !listed_before["runs"].as_array().is_some_and(|runs| {
        runs.iter()
            .any(|run| run["runId"].as_str() == Some(run_id.as_str()))
    }) {
        return Err(DiagnosticCode::McpResultMalformed);
    }
    probe.push_trace(TraceOperationCode::Disconnect, vec![], None)?;
    client
        .close_session()
        .await
        .map_err(|_| DiagnosticCode::McpCallFailed)?;
    probe.push_trace(TraceOperationCode::Reconnect, vec![], None)?;
    client
        .initialize()
        .await
        .map_err(|_| DiagnosticCode::McpInitializeFailed)?;
    let listed_after = probe
        .call(
            client,
            TraceOperationCode::ListRuns,
            "ptah_list_runs",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
            }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    if !listed_after["runs"].as_array().is_some_and(|runs| {
        runs.iter()
            .any(|run| run["runId"].as_str() == Some(run_id.as_str()))
    }) {
        return Err(DiagnosticCode::RestartRecoveryFailed);
    }
    let after = run_events(probe, client, workspace, &agent.session_id, &run_id).await?;
    if before != after || before.is_empty() {
        return Err(DiagnosticCode::CursorContinuityLost);
    }
    let cursor_before = before.last().copied();
    let cursor_after = after.last().copied();
    probe.reconnect = ReconnectEvidence {
        attempted: true,
        reinitialized: true,
        cursor_before,
        cursor_after,
        continuity_proven: true,
    };
    probe.transition(
        EntityKind::Cursor,
        DurableStateCode::Active,
        DurableStateCode::Advanced,
        None,
    );
    probe.retain_id(DurableIdKind::Run, &run_id);
    Ok(())
}

async fn run_events(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
    session_id: &str,
    run_id: &str,
) -> Result<Vec<u64>, DiagnosticCode> {
    let page = probe
        .call(
            client,
            TraceOperationCode::GetEvents,
            "ptah_get_events",
            json!({
                "session_id": session_id,
                "workspace": workspace,
                "run_id": run_id,
                "after_seq": 0,
                "limit": 500,
            }),
            vec![
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::RunId,
                ArgumentFieldCode::AfterSequence,
                ArgumentFieldCode::Limit,
            ],
        )
        .await?;
    let entries = page["entries"]
        .as_array()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    let mut sequences = Vec::with_capacity(entries.len());
    for entry in entries {
        let sequence = entry["seq"]
            .as_u64()
            .ok_or(DiagnosticCode::McpResultMalformed)?;
        if sequence == 0
            || sequences
                .last()
                .is_some_and(|previous| sequence <= *previous)
        {
            return Err(DiagnosticCode::CursorContinuityLost);
        }
        sequences.push(sequence);
    }
    Ok(sequences)
}

async fn bounded_run_terminal(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
    provider_recorder: Option<&InMemoryObservationRecorder>,
) -> Result<(), DiagnosticCode> {
    let agent = create_agent(probe, client, workspace).await?;
    if let Some(recorder) = provider_recorder {
        probe.provider_attempt_start = recorder
            .snapshot()
            .last()
            .map(|observation| observation.attempt_number() + 1)
            .or(Some(1));
    }
    let submitted = probe
        .call(
            client,
            TraceOperationCode::SubmitRun,
            "ptah_submit_task",
            json!({
                "request_id": request_id("bounded-run"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "prompt": "Return exactly BOUNDED_RUN_OK and stop without tools.",
                "bounds": {
                    "maxPromptBytes": 512,
                    "maxRounds": 2,
                    "maxDurationMs": 15000,
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
    let run =
        wait_for_terminal_evidence(probe, client, workspace, &agent.session_id, &run_id).await?;
    if run["state"].as_str() != Some("completed") {
        return Err(DiagnosticCode::TerminalStateMissing);
    }
    let agent_spec_revision = run["agentSpecRevision"]
        .as_u64()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    let run_agent_id = run["agentId"]
        .as_str()
        .filter(|value| !value.is_empty())
        .unwrap_or(&agent.agent_id)
        .to_owned();
    if run_agent_id != agent.agent_id {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    if run["aggregates"]["usageComplete"].as_bool() != Some(true)
        || run["aggregates"]["usagePendingRequests"].as_u64() != Some(0)
    {
        return Err(DiagnosticCode::AuthoritativeUsageMissing);
    }
    let events = probe
        .call(
            client,
            TraceOperationCode::GetEvents,
            "ptah_get_events",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
                "run_id": run_id,
                "limit": 64,
            }),
            vec![
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::RunId,
                ArgumentFieldCode::Limit,
            ],
        )
        .await?;
    if !events["entries"].is_array() {
        return Err(DiagnosticCode::McpResultMalformed);
    }
    probe.provider_run = Some(ProviderRunEvidence {
        session_id: agent.session_id.clone(),
        agent_id: run_agent_id,
        run_id: run_id.clone(),
        agent_spec_revision,
        checkpoint_id: None,
        parent_run_id: run["parentRunId"].as_str().map(str::to_owned),
        continuation_context_hash: run["continuationContextHash"].as_str().map(str::to_owned),
        continuation_fidelity: run["continuationFidelity"].as_str().map(str::to_owned),
        state: "completed".into(),
        stop_cause: run["stopCause"].as_str().map(str::to_owned),
    });
    probe.transition(
        EntityKind::Run,
        DurableStateCode::Queued,
        DurableStateCode::Running,
        Some(&run_id),
    );
    probe.transition(
        EntityKind::Run,
        DurableStateCode::Running,
        DurableStateCode::Completed,
        Some(&run_id),
    );
    probe.retain_id(DurableIdKind::Run, &run_id);
    Ok(())
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

async fn continuation_resume(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
) -> Result<(), DiagnosticCode> {
    let agent = create_agent(probe, client, workspace).await?;
    let seed_run = agent
        .seed_run
        .as_ref()
        .ok_or(DiagnosticCode::TerminalStateMissing)?;
    let seed_run_id = required_string(seed_run, &["runId"])?;
    if seed_run["state"].as_str() != Some("completed") {
        return Err(DiagnosticCode::TerminalStateMissing);
    }

    let mut plan = None;
    for _ in 0..100 {
        match probe
            .call(
                client,
                TraceOperationCode::GetAgent,
                "ptah_get_persistent_agent",
                json!({
                    "session_id": agent.session_id,
                    "workspace": workspace,
                    "agent_id": agent.agent_id,
                }),
                vec![
                    ArgumentFieldCode::SessionId,
                    ArgumentFieldCode::Workspace,
                    ArgumentFieldCode::AgentId,
                ],
            )
            .await
        {
            Ok(value) if value["checkpoint"]["checkpointId"].as_str().is_some() => {
                plan = Some(value);
                break;
            }
            Err(DiagnosticCode::McpToolError) => {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            Ok(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
            Err(error) => return Err(error),
        }
    }
    let plan = plan.ok_or(DiagnosticCode::Timeout)?;
    let checkpoint_id = required_string(&plan, &["checkpoint", "checkpointId"])?;
    let checkpoint_run_id = required_string(&plan, &["checkpoint", "runId"])?;
    let latest_checkpoint_id = required_string(&plan, &["agent", "latestCheckpointId"])?;
    let checkpoint_context_hash = required_string(&plan, &["checkpoint", "contextHash"])?;
    let checkpoint_revision = plan["checkpoint"]["agentSpecRevision"]
        .as_u64()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    if checkpoint_run_id != seed_run_id
        || latest_checkpoint_id != checkpoint_id
        || checkpoint_context_hash.len() != 64
        || checkpoint_revision
            != seed_run["agentSpecRevision"]
                .as_u64()
                .ok_or(DiagnosticCode::McpResultMalformed)?
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    probe.transition(
        EntityKind::Run,
        DurableStateCode::Completed,
        DurableStateCode::Completed,
        Some(&seed_run_id),
    );
    probe.retain_id(DurableIdKind::Checkpoint, &checkpoint_id);

    let resume_request_id = request_id("continuation-resume");
    let resume_arguments = json!({
        "request_id": resume_request_id,
        "session_id": agent.session_id,
        "workspace": workspace,
        "agent_id": agent.agent_id,
        "prompt": "Continue with one short bounded acknowledgement of the durable checkpoint and stop.",
        "max_rounds": 1,
    });
    let resumed = probe
        .call(
            client,
            TraceOperationCode::ResumeAgent,
            "ptah_resume_persistent_agent",
            resume_arguments.clone(),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::AgentId,
                ArgumentFieldCode::Prompt,
                ArgumentFieldCode::MaxRounds,
            ],
        )
        .await?;
    if resumed["agent"]["agentId"].as_str() != Some(agent.agent_id.as_str())
        || resumed["response"].as_str().is_none()
    {
        return Err(DiagnosticCode::McpResultMalformed);
    }
    let resumed_response = resumed["response"].clone();
    let replayed = probe
        .call(
            client,
            TraceOperationCode::ResumeAgent,
            "ptah_resume_persistent_agent",
            resume_arguments,
            vec![ArgumentFieldCode::RequestId],
        )
        .await?;
    if replayed["response"] != resumed_response {
        return Err(DiagnosticCode::IdempotencyReplayMismatch);
    }

    let runs = probe
        .call(
            client,
            TraceOperationCode::ListRuns,
            "ptah_list_runs",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
            }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    let candidates = runs["runs"]
        .as_array()
        .ok_or(DiagnosticCode::McpResultMalformed)?
        .iter()
        .filter(|run| run["parentRunId"].as_str() == Some(seed_run_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if candidates.len() != 1 {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let resumed_run = &candidates[0];
    let resumed_run_id = required_string(resumed_run, &["runId"])?;
    let continuation_context_id = required_string(resumed_run, &["continuationContextId"])?;
    let continuation_context_hash = required_string(resumed_run, &["continuationContextHash"])?;
    if resumed_run["state"].as_str() != Some("completed")
        || resumed_run["agentId"].as_str() != Some(agent.agent_id.as_str())
        || resumed_run["checkpointId"].as_str() != Some(checkpoint_id.as_str())
        || resumed_run["agentSpecRevision"].as_u64() != Some(checkpoint_revision)
        || continuation_context_id.is_empty()
        || continuation_context_hash.len() != 64
        || resumed_run["aggregates"]["usageComplete"].as_bool() != Some(true)
        || resumed_run["aggregates"]["usagePendingRequests"].as_u64() != Some(0)
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }

    let replay_runs = probe
        .call(
            client,
            TraceOperationCode::ListRuns,
            "ptah_list_runs",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
            }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    let replay_count = replay_runs["runs"].as_array().map_or(0, |values| {
        values
            .iter()
            .filter(|run| run["runId"].as_str() == Some(resumed_run_id.as_str()))
            .count()
    });
    if replay_count != 1 {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    probe.transition(
        EntityKind::Run,
        DurableStateCode::Absent,
        DurableStateCode::Completed,
        Some(&resumed_run_id),
    );
    probe.provider_run = Some(provider_run_from_value(&agent, resumed_run)?);
    probe.retain_id(DurableIdKind::Session, &agent.session_id);
    probe.retain_id(DurableIdKind::Agent, &agent.agent_id);
    probe.retain_id(DurableIdKind::Run, &seed_run_id);
    probe.retain_id(DurableIdKind::Run, &resumed_run_id);
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
    let seed_run = if completed {
        Some(
            probe
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
                .await?,
        )
    } else {
        None
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
        seed_run,
    })
}

fn bind_capture_to_seed(
    probe: &mut ProbeBuilder<'_>,
    agent: &TestAgent,
) -> Result<(), DiagnosticCode> {
    let Some(run) = agent.seed_run.as_ref() else {
        return Err(DiagnosticCode::TerminalStateMissing);
    };
    if run["state"].as_str() != Some("completed") {
        return Err(DiagnosticCode::TerminalStateMissing);
    }
    probe.capture_provider_run = Some(ProviderRunEvidence {
        session_id: agent.session_id.clone(),
        agent_id: agent.agent_id.clone(),
        run_id: run["runId"]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or(DiagnosticCode::McpResultMalformed)?,
        agent_spec_revision: run["agentSpecRevision"]
            .as_u64()
            .ok_or(DiagnosticCode::McpResultMalformed)?,
        checkpoint_id: run["checkpointId"].as_str().map(str::to_owned),
        parent_run_id: run["parentRunId"].as_str().map(str::to_owned),
        continuation_context_hash: run["continuationContextHash"].as_str().map(str::to_owned),
        continuation_fidelity: run["continuationFidelity"].as_str().map(str::to_owned),
        state: "completed".into(),
        stop_cause: run["stopCause"].as_str().map(str::to_owned),
    });
    Ok(())
}

async fn wait_for_terminal_seed(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
    session_id: &str,
    run_id: &str,
) -> Result<bool, DiagnosticCode> {
    for _ in 0..120 {
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
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            _ => return Err(DiagnosticCode::McpResultMalformed),
        }
    }
    Err(DiagnosticCode::Timeout)
}

async fn wait_for_terminal_evidence(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
    session_id: &str,
    run_id: &str,
) -> Result<Value, DiagnosticCode> {
    for _ in 0..300 {
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
            Some("completed" | "failed" | "cancelled" | "interrupted" | "limit_reached") => {
                return Ok(run)
            }
            Some("queued" | "running" | "waiting") => {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            _ => return Err(DiagnosticCode::McpResultMalformed),
        }
    }
    Err(DiagnosticCode::Timeout)
}

async fn wait_for_work_terminal(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
    session_id: &str,
    work_id: &str,
) -> Result<Value, DiagnosticCode> {
    for _ in 0..300 {
        let work = probe
            .call(
                client,
                TraceOperationCode::GetWork,
                "ptah_get_work",
                json!({
                    "session_id": session_id,
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
        match work["work"]["state"].as_str() {
            Some("succeeded" | "failed" | "cancelled" | "awaiting_approval") => return Ok(work),
            Some("queued" | "leased" | "running" | "awaiting_input") => {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
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
