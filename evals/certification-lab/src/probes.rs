//! Minimal black-box probe implementations for the public MCP control plane.
//!
//! Every value extracted from MCP is used transiently and either discarded or
//! converted to an opaque SHA-256 label before it can enter report evidence.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use grokptah_agent_bridge::orchestration::hash_payload;
use grokptah_agent_bridge::provider_observation::InMemoryObservationRecorder;
use grokptah_agent_bridge::{McpControlClient, McpRemoteError};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::local_service::LocalService;
use crate::manifest::{OracleCode, ProbeAction, ProbeDefinition};
use crate::process_service::{scan_mcp_value, ProviderDisposition};
use crate::report::{
    diagnostic_failure_class, opaque_durable_id, ArgumentFieldCode, DiagnosticCode, DurableIdKind,
    DurableStateCode, EntityKind, EvidenceCounters, LoopbackProviderObservation, OpaqueDurableId,
    PhaseCode, PhaseResult, ProbeResult, ProbeStatus, ReconnectEvidence, RestartEvidence,
    StructuralTrace, TraceOperationCode, TraceRecord, TransitionEvidence,
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
    observed_actions: Vec<ProbeAction>,
    observed_oracles: Vec<OracleCode>,
    provider_run: Option<ProviderRunEvidence>,
    capture_provider_run: Option<ProviderRunEvidence>,
    provider_attempt_start: Option<u32>,
    capture_attempt_start: Option<u32>,
    provider_observation: Option<LoopbackProviderObservation>,
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
            observed_actions: Vec::new(),
            observed_oracles: Vec::new(),
            provider_run: None,
            capture_provider_run: None,
            provider_attempt_start: None,
            capture_attempt_start: None,
            provider_observation: None,
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
            Ok(result) if !result.is_error => {
                if matches!(
                    tool,
                    "ptah_get_run"
                        | "ptah_list_runs"
                        | "ptah_get_progress"
                        | "ptah_promote_run"
                        | "ptah_discard_run"
                ) {
                    let text = result
                        .raw
                        .get("content")
                        .and_then(|content| content.get(0))
                        .and_then(|item| item.get("text"))
                        .and_then(|text| text.as_str())
                        .unwrap_or("");
                    let parsed_text = serde_json::from_str::<Value>(text).ok();
                    if grokptah_agent_bridge::orchestration::public_run_contains_forbidden_fields(
                        &result.structured,
                    ) || parsed_text.as_ref().is_some_and(
                        grokptah_agent_bridge::orchestration::public_run_contains_forbidden_fields,
                    ) {
                        return Err(DiagnosticCode::McpResultMalformed);
                    }
                }
                if let Some(last) = self.records.last_mut() {
                    last.result_digest = Some(hash_payload(&result.structured));
                    last.opaque_entity_id = opaque_from_value(&result.structured);
                }
                Ok(result.structured)
            }
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
            result_digest: None,
            opaque_entity_id: None,
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

    fn observe_action(&mut self, action: ProbeAction) {
        if !self.observed_actions.contains(&action) {
            self.observed_actions.push(action);
        }
    }

    fn observe_oracle(&mut self, oracle: OracleCode) {
        if !self.observed_oracles.contains(&oracle) {
            self.observed_oracles.push(oracle);
        }
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
        let always_on = self.definition.id == "always-on-grokbot-lifecycle-v1";
        let verified_actions = if status == ProbeStatus::Passed {
            if always_on {
                self.definition
                    .actions
                    .iter()
                    .copied()
                    .filter(|action| self.observed_actions.contains(action))
                    .collect()
            } else {
                self.definition.actions.clone()
            }
        } else {
            Vec::new()
        };
        let verified_oracles = if status == ProbeStatus::Passed {
            if always_on {
                self.definition
                    .oracle_codes
                    .iter()
                    .copied()
                    .filter(|oracle| self.observed_oracles.contains(oracle))
                    .collect()
            } else {
                self.definition.oracle_codes.clone()
            }
        } else {
            Vec::new()
        };
        ProbeExecution {
            result: ProbeResult {
                probe_id: self.definition.id.clone(),
                catalog_scenario_ids: self.definition.catalog_scenario_ids.clone(),
                status,
                supported: status != ProbeStatus::Skipped,
                failure_class,
                diagnostics: vec![diagnostic],
                verified_actions,
                verified_oracles,
                phases: vec![phase],
                transitions: self.transitions,
                counters: self.counters,
                reconnect: self.reconnect,
                restart: self.restart,
                opaque_ids: self.opaque_ids,
                trace: None,
                capture_refs: Vec::new(),
                elapsed_millis,
                provider_observation: self.provider_observation,
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

fn opaque_from_value(value: &Value) -> Option<String> {
    value
        .pointer("/plan/planId")
        .or_else(|| value.pointer("/run/runId"))
        .or_else(|| value.pointer("/runId"))
        .or_else(|| value.pointer("/work/workId"))
        .or_else(|| value.pointer("/workId"))
        .or_else(|| value.pointer("/sessionId"))
        .or_else(|| value.pointer("/agentId"))
        .and_then(Value::as_str)
        .map(opaque_durable_id)
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
        "manager-plan-lifecycle-v1" => Some(&[
            "ptah_create_session",
            "ptah_submit_task",
            "ptah_cancel",
            "ptah_get_run",
            "ptah_list_persistent_agents",
            "ptah_create_manager_plan",
            "ptah_get_manager_plan",
            "ptah_advance_manager_plan",
            "ptah_tick_manager_plan",
            "ptah_replan_manager_plan",
            "ptah_get_work",
            "ptah_claim_work",
            "ptah_complete_work",
            "ptah_fail_work",
            "ptah_cancel_work",
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
        "always-on-grokbot-lifecycle-v1" => Some(&[
            "ptah_create_session",
            "ptah_submit_task",
            "ptah_cancel",
            "ptah_get_run",
            "ptah_list_persistent_agents",
            "ptah_set_managed_execution",
            "ptah_create_manager_plan",
            "ptah_tick_manager_plan",
            "ptah_get_manager_plan",
            "ptah_list_work",
            "ptah_get_work",
            "ptah_list_runs",
            "ptah_list_execution_intents",
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
        "manager-plan-lifecycle-v1" => manager_plan_lifecycle(&mut probe, client, workspace).await,
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
        "always-on-grokbot-lifecycle-v1" => always_on_grokbot(&mut probe).await,
        _ => Err(DiagnosticCode::ProbeImplementationUnavailable),
    };
    match outcome {
        Ok(()) => probe.finish(ProbeStatus::Passed, DiagnosticCode::Ok),
        Err(code) if code == DiagnosticCode::PermissionCapabilityAbsent => {
            probe.finish(ProbeStatus::Skipped, code)
        }
        Err(code) if code == DiagnosticCode::ProbeImplementationUnavailable => {
            probe.finish(ProbeStatus::Indeterminate, code)
        }
        Err(code) => probe.finish(ProbeStatus::Failed, code),
    }
}

async fn always_on_grokbot(probe: &mut ProbeBuilder<'_>) -> Result<(), DiagnosticCode> {
    let fixture = AlwaysOnFixture::load()?;
    always_on_home_a(probe, &fixture).await?;
    always_on_home_b(probe, &fixture).await?;
    assert_observed_contract(probe)
}

fn assert_observed_contract(probe: &ProbeBuilder<'_>) -> Result<(), DiagnosticCode> {
    if probe
        .definition
        .actions
        .iter()
        .any(|action| !probe.observed_actions.contains(action))
        || probe
            .definition
            .oracle_codes
            .iter()
            .any(|oracle| !probe.observed_oracles.contains(oracle))
    {
        Err(DiagnosticCode::OracleMismatch)
    } else {
        Ok(())
    }
}

fn plan_step_identity(steps: &Value) -> Value {
    Value::Array(
        steps
            .as_array()
            .into_iter()
            .flatten()
            .map(|step| {
                json!({
                    "stepId": step["stepId"],
                    "kind": step["kind"],
                    "objective": step["objective"],
                    "dependencies": step["dependencies"],
                    "assignedAgentId": step["assignedAgentId"],
                })
            })
            .collect(),
    )
}

fn plan_identity_hash(plan: &Value) -> String {
    hash_payload(&json!({
        "planId": plan.pointer("/plan/planId"),
        "objective": plan.pointer("/plan/objective"),
        "steps": plan_step_identity(plan.pointer("/plan/steps").unwrap_or(&Value::Null)),
    }))
}

fn plan_state_survived_restart(pre: Option<&str>, post: Option<&str>) -> bool {
    pre == post || matches!((pre, post), (Some("active"), Some("needs_replan")))
}

fn always_on_scan(value: &Value) -> Result<(), DiagnosticCode> {
    scan_mcp_value("mcp", value).map_err(|_| DiagnosticCode::RedactionRejected)
}

fn work_items(work: &Value) -> &[Value] {
    work.get("work")
        .or_else(|| work.get("items"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn work_for_step<'a>(work: &'a Value, step_id: &str) -> Vec<&'a Value> {
    work_items(work)
        .iter()
        .filter(|item| item["sourceManagerStepId"].as_str() == Some(step_id))
        .collect()
}

fn native_step(step_id: &str, objective: &str, deps: &[&str], agent_id: &str) -> Value {
    json!({
        "stepId": step_id,
        "kind": "native",
        "objective": objective,
        "assignedAgentId": agent_id,
        "dependencies": deps,
        "policy": {
            "bounds": {
                "maxPromptBytes": 16384,
                "maxRounds": 4,
                "maxDurationMs": 45000,
                "maxTotalTokens": 8000
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
    })
}

fn pending_usage(run: &Value) -> u64 {
    run.pointer("/aggregates/usagePendingRequests")
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AlwaysOnFixture {
    step_first: String,
    step_failing: String,
    step_replacement: String,
    decision_work: u64,
    proposal_runs: u64,
    native_work_by_step: BTreeMap<String, u64>,
    provider_posts_by_semantic: BTreeMap<String, u64>,
    zero_growth_window: Duration,
}

impl AlwaysOnFixture {
    fn load() -> Result<Self, DiagnosticCode> {
        let value: Value = serde_json::from_slice(crate::ALWAYS_ON_GROKBOT_FIXTURE)
            .map_err(|_| DiagnosticCode::FixtureInvalid)?;
        Self::from_value(&value)
    }

    fn from_value(value: &Value) -> Result<Self, DiagnosticCode> {
        if value["schema"].as_str() != Some(crate::ALWAYS_ON_GROKBOT_FIXTURE_SCHEMA)
            || value["schemaVersion"].as_u64() != Some(2)
        {
            return Err(DiagnosticCode::FixtureInvalid);
        }
        let string = |value: &Value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or(DiagnosticCode::FixtureInvalid)
        };
        let step_first = string(&value["steps"]["first"])?;
        let step_failing = string(&value["steps"]["failing"])?;
        let step_replacement = string(&value["steps"]["replacement"])?;
        let decision_work = value["happyPath"]["decisionWork"]
            .as_u64()
            .ok_or(DiagnosticCode::FixtureInvalid)?;
        let proposal_runs = value["happyPath"]["proposalRunsObserved"]
            .as_u64()
            .ok_or(DiagnosticCode::FixtureInvalid)?;
        let count_map = |name: &str| -> Result<BTreeMap<String, u64>, DiagnosticCode> {
            value["happyPath"][name]
                .as_object()
                .ok_or(DiagnosticCode::FixtureInvalid)?
                .iter()
                .map(|(key, value)| {
                    Ok((
                        key.clone(),
                        value.as_u64().ok_or(DiagnosticCode::FixtureInvalid)?,
                    ))
                })
                .collect()
        };
        let native_work_by_step = count_map("nativeWorkByStep")?;
        let provider_posts_by_semantic = count_map("providerPostsBySemanticId")?;
        let period = value["supervisorPeriodMs"]
            .as_u64()
            .ok_or(DiagnosticCode::FixtureInvalid)?;
        let periods = value["zeroGrowthSupervisorPeriods"]
            .as_u64()
            .ok_or(DiagnosticCode::FixtureInvalid)?;
        let zero_growth_window = Duration::from_millis(
            period
                .checked_mul(periods)
                .ok_or(DiagnosticCode::FixtureInvalid)?,
        );
        let fixture = Self {
            step_first,
            step_failing,
            step_replacement,
            decision_work,
            proposal_runs,
            native_work_by_step,
            provider_posts_by_semantic,
            zero_growth_window,
        };
        if fixture.native_work_by_step.len() != 3
            || fixture.provider_posts_by_semantic.len() != 4
            || fixture.decision_work != 1
            || fixture.proposal_runs != 1
            || fixture.native_steps().iter().any(|step| {
                !fixture.native_work_by_step.contains_key(*step)
                    || !fixture.provider_posts_by_semantic.contains_key(*step)
            })
        {
            return Err(DiagnosticCode::FixtureInvalid);
        }
        for step in [
            &fixture.step_first,
            &fixture.step_failing,
            &fixture.step_replacement,
        ] {
            if fixture.native_work_by_step.get(step) != Some(&1)
                || fixture.provider_posts_by_semantic.get(step) != Some(&1)
            {
                return Err(DiagnosticCode::FixtureInvalid);
            }
        }
        if fixture.provider_posts_by_semantic.get("manager-decision") != Some(&1) {
            return Err(DiagnosticCode::FixtureInvalid);
        }
        Ok(fixture)
    }

    fn plan_arguments(
        &self,
        request_id: &str,
        session_id: &str,
        workspace: &str,
        agent_id: &str,
    ) -> Value {
        json!({
            "request_id": request_id,
            "session_id": session_id,
            "workspace": workspace,
            "manager_agent_id": agent_id,
            "objective": "always-on grokbot dependent DAG",
            "autonomous": true,
            "max_replans": 2,
            "max_in_flight": 2,
            "steps": [
                native_step(&self.step_first, "GROKBOT_SUCCESS first native unit", &[], agent_id),
                native_step(
                    &self.step_failing,
                    "GROKBOT_FORCE_FAIL child that must be replaced",
                    &[self.step_first.as_str()],
                    agent_id
                )
            ]
        })
    }

    fn native_steps(&self) -> [&str; 3] {
        [
            self.step_first.as_str(),
            self.step_failing.as_str(),
            self.step_replacement.as_str(),
        ]
    }

    fn expected_happy_cardinality(&self) -> Result<AlwaysOnCardinality, DiagnosticCode> {
        let native_work = self
            .native_work_by_step
            .values()
            .try_fold(0_u64, |total, count| total.checked_add(*count))
            .ok_or(DiagnosticCode::FixtureInvalid)?;
        Ok(AlwaysOnCardinality {
            work: usize::try_from(
                native_work
                    .checked_add(self.decision_work)
                    .and_then(|total| total.checked_add(1))
                    .ok_or(DiagnosticCode::FixtureInvalid)?,
            )
            .map_err(|_| DiagnosticCode::FixtureInvalid)?,
            runs: usize::try_from(
                native_work
                    .checked_add(self.proposal_runs)
                    .and_then(|total| total.checked_add(1))
                    .ok_or(DiagnosticCode::FixtureInvalid)?,
            )
            .map_err(|_| DiagnosticCode::FixtureInvalid)?,
            intents: usize::try_from(
                native_work
                    .checked_add(1)
                    .ok_or(DiagnosticCode::FixtureInvalid)?,
            )
            .map_err(|_| DiagnosticCode::FixtureInvalid)?,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct AlwaysOnCardinality {
    work: usize,
    runs: usize,
    intents: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AlwaysOnHeldJoin {
    work_id: String,
    attempt_id: String,
    run_id: String,
}

fn exact_array<'a>(value: &'a Value, key: &str) -> Result<&'a [Value], DiagnosticCode> {
    value[key]
        .as_array()
        .map(Vec::as_slice)
        .ok_or(DiagnosticCode::McpResultMalformed)
}

fn exact_single_linked_run(
    attempt: &Value,
    expected_run_id: Option<&str>,
) -> Result<String, DiagnosticCode> {
    let linked = attempt["linkedRunIds"]
        .as_array()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    if linked.len() != 1 {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let run_id = linked[0]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    if expected_run_id.is_some_and(|expected| expected != run_id) {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    Ok(run_id.to_owned())
}

async fn always_on_snapshot(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    session_id: &str,
    workspace: &str,
) -> Result<AlwaysOnCardinality, DiagnosticCode> {
    let scope = json!({ "session_id": session_id, "workspace": workspace });
    let fields = vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace];
    let work = probe
        .call(
            client,
            TraceOperationCode::ListWork,
            "ptah_list_work",
            scope.clone(),
            fields.clone(),
        )
        .await?;
    let runs = probe
        .call(
            client,
            TraceOperationCode::ListRuns,
            "ptah_list_runs",
            scope.clone(),
            fields.clone(),
        )
        .await?;
    let intents = probe
        .call(
            client,
            TraceOperationCode::ListExecutionIntents,
            "ptah_list_execution_intents",
            scope,
            fields,
        )
        .await?;
    Ok(AlwaysOnCardinality {
        work: work_items(&work).len(),
        runs: exact_array(&runs, "runs")?.len(),
        intents: exact_array(&intents, "intents")?.len(),
    })
}

fn assert_exact_cardinality(
    expected: AlwaysOnCardinality,
    actual: AlwaysOnCardinality,
) -> Result<(), DiagnosticCode> {
    if expected == actual {
        Ok(())
    } else {
        Err(DiagnosticCode::StateTransitionMismatch)
    }
}

fn assert_happy_path_counts(
    fixture: &AlwaysOnFixture,
    service: &crate::process_service::ProcessService,
    work: &Value,
    runs: &Value,
    intents: &Value,
) -> Result<(), DiagnosticCode> {
    let expected = fixture.expected_happy_cardinality()?;
    let actual = AlwaysOnCardinality {
        work: work_items(work).len(),
        runs: exact_array(runs, "runs")?.len(),
        intents: exact_array(intents, "intents")?.len(),
    };
    assert_exact_cardinality(expected, actual)?;
    if work_items(work)
        .iter()
        .filter(|item| item["kind"].as_str() == Some("manager-decision"))
        .count() as u64
        != fixture.decision_work
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    if exact_array(runs, "runs")?
        .iter()
        .filter(|run| run["purpose"].as_str() == Some("manager_proposal"))
        .count() as u64
        != fixture.proposal_runs
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    for step in fixture.native_steps() {
        if work_for_step(work, step).len() as u64 != fixture.native_work_by_step[step] {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        if service.provider.count_for(step) != fixture.provider_posts_by_semantic[step] {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
    }
    if service.provider.count_for("manager-decision")
        != fixture.provider_posts_by_semantic["manager-decision"]
        || service.provider.count_for("setup") != 1
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let expected_posts = fixture
        .provider_posts_by_semantic
        .values()
        .try_fold(1_u64, |total, count| total.checked_add(*count))
        .ok_or(DiagnosticCode::FixtureInvalid)?;
    if service.send_count() != expected_posts {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let expected_intents = fixture
        .native_work_by_step
        .values()
        .try_fold(1_u64, |total, count| total.checked_add(*count))
        .ok_or(DiagnosticCode::FixtureInvalid)?;
    if exact_array(intents, "intents")?.len() as u64 != expected_intents {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    if exact_array(runs, "runs")?.iter().any(|run| {
        matches!(
            run["state"].as_str(),
            Some("completed" | "failed" | "cancelled" | "interrupted")
        ) && pending_usage(run) != 0
    }) {
        return Err(DiagnosticCode::RestartRecoveryFailed);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn always_on_find_in_flight(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    session_id: &str,
    workspace: &str,
    step_id: &str,
) -> Result<AlwaysOnHeldJoin, DiagnosticCode> {
    for _ in 0..1_800 {
        let work = probe
            .call(
                client,
                TraceOperationCode::ListWork,
                "ptah_list_work",
                json!({ "session_id": session_id, "workspace": workspace }),
                vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
            )
            .await?;
        let rows = work_for_step(&work, step_id);
        if rows.len() > 1 {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        if let Some(row) = rows.first() {
            let work_id = required_string(row, &["workId"])?;
            if !matches!(row["state"].as_str(), Some("running" | "leased")) {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            let detailed = probe
                .call(
                    client,
                    TraceOperationCode::GetWork,
                    "ptah_get_work",
                    json!({
                        "session_id": session_id,
                        "workspace": workspace,
                        "work_id": work_id
                    }),
                    vec![
                        ArgumentFieldCode::SessionId,
                        ArgumentFieldCode::Workspace,
                        ArgumentFieldCode::WorkId,
                    ],
                )
                .await?;
            if detailed["work"]["workId"].as_str() != Some(work_id.as_str()) {
                return Err(DiagnosticCode::StateTransitionMismatch);
            }
            let attempts = exact_array(&detailed, "attempts")?;
            if attempts.len() != 1 {
                return Err(DiagnosticCode::StateTransitionMismatch);
            }
            let attempt_id = required_string(&attempts[0], &["attemptId"])?;
            let run_id = exact_single_linked_run(&attempts[0], None)?;
            let intents = probe
                .call(
                    client,
                    TraceOperationCode::ListExecutionIntents,
                    "ptah_list_execution_intents",
                    json!({ "session_id": session_id, "workspace": workspace }),
                    vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
                )
                .await?;
            let matching_intents: Vec<_> = exact_array(&intents, "intents")?
                .iter()
                .filter(|intent| {
                    intent["workId"].as_str() == Some(work_id.as_str())
                        && intent["attemptId"].as_str() == Some(attempt_id.as_str())
                })
                .collect();
            if matching_intents.len() != 1
                || matching_intents[0]["runId"].as_str() != Some(run_id.as_str())
            {
                return Err(DiagnosticCode::StateTransitionMismatch);
            }
            let runs = probe
                .call(
                    client,
                    TraceOperationCode::ListRuns,
                    "ptah_list_runs",
                    json!({ "session_id": session_id, "workspace": workspace }),
                    vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
                )
                .await?;
            let matching_runs: Vec<_> = exact_array(&runs, "runs")?
                .iter()
                .filter(|run| run["runId"].as_str() == Some(run_id.as_str()))
                .collect();
            if matching_runs.len() == 1 && matching_runs[0]["state"].as_str() == Some("running") {
                probe.retain_id(DurableIdKind::Work, &work_id);
                probe.retain_id(DurableIdKind::Attempt, &attempt_id);
                probe.retain_id(DurableIdKind::Run, &run_id);
                return Ok(AlwaysOnHeldJoin {
                    work_id,
                    attempt_id,
                    run_id,
                });
            }
            if matching_runs.len() > 1 {
                return Err(DiagnosticCode::StateTransitionMismatch);
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(DiagnosticCode::Timeout)
}

fn validate_plan_steps(plan: &Value, expected: &Value) -> Result<(), DiagnosticCode> {
    if plan_step_identity(plan.pointer("/plan/steps").unwrap_or(&Value::Null))
        != plan_step_identity(expected)
    {
        Err(DiagnosticCode::StateTransitionMismatch)
    } else {
        Ok(())
    }
}

async fn always_on_bootstrap(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
) -> Result<(String, String), DiagnosticCode> {
    let created = probe
        .call(
            client,
            TraceOperationCode::CreateSession,
            "ptah_create_session",
            json!({ "workspace": workspace, "title": SAFE_TITLE }),
            vec![ArgumentFieldCode::Workspace],
        )
        .await?;
    always_on_scan(&created)?;
    let session_id = required_string(&created, &["sessionId"])?;
    let submitted = probe
        .call(
            client,
            TraceOperationCode::SubmitRun,
            "ptah_submit_task",
            json!({
                "request_id": request_id("setup"),
                "session_id": session_id,
                "workspace": workspace,
                "prompt": "GROKBOT_SETUP materialize the lane Agent"
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
            ],
        )
        .await?;
    always_on_scan(&submitted)?;
    let setup_run = required_string(&submitted, &["runId"])?;
    wait_run_terminal(client, &session_id, workspace, &setup_run).await?;
    let agents = probe
        .call(
            client,
            TraceOperationCode::ListAgents,
            "ptah_list_persistent_agents",
            json!({}),
            vec![],
        )
        .await?;
    always_on_scan(&agents)?;
    let listed_agents = agents["agents"]
        .as_array()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    if listed_agents.len() != 1 {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let agent_id = listed_agents[0]["agentId"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or(DiagnosticCode::McpResultMalformed)?
        .to_string();
    probe.retain_id(DurableIdKind::Agent, &agent_id);
    let _ = probe
        .call(
            client,
            TraceOperationCode::SetManagedExecution,
            "ptah_set_managed_execution",
            json!({
                "session_id": session_id,
                "workspace": workspace,
                "agent_id": agent_id,
                "policy": {
                    "enabled": true,
                    "maxConcurrentRuns": 2,
                    "bounds": {
                        "maxPromptBytes": 16384,
                        "maxRounds": 4,
                        "maxDurationMs": 45000,
                        "maxTotalTokens": 8000
                    },
                    "retryEligible": false,
                    "requiresApprovalBeforeExecution": false
                }
            }),
            vec![
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::AgentId,
            ],
        )
        .await?;
    Ok((session_id, agent_id))
}

fn process_service_spawn_diagnostic(error: anyhow::Error) -> DiagnosticCode {
    if error.chain().any(|cause| {
        matches!(
            cause.to_string().as_str(),
            "GROKPTAH_SERVICE_BIN" | "GROKPTAH_SERVICE_BIN is not a file"
        )
    }) {
        DiagnosticCode::ProbeImplementationUnavailable
    } else {
        DiagnosticCode::RestartControlUnavailable
    }
}

async fn always_on_home_a(
    probe: &mut ProbeBuilder<'_>,
    fixture: &AlwaysOnFixture,
) -> Result<(), DiagnosticCode> {
    use crate::process_service::ProcessService;

    let service = ProcessService::spawn().map_err(process_service_spawn_diagnostic)?;
    let mut client = service
        .client()
        .await
        .map_err(|_| DiagnosticCode::ServiceUnreachable)?;
    let workspace = service.workspace.display().to_string();
    let (session_id, agent_id) = always_on_bootstrap(probe, &mut client, &workspace).await?;
    let plan_request = request_id("plan");
    let args = fixture.plan_arguments(&plan_request, &session_id, &workspace, &agent_id);
    let created_plan = probe
        .call(
            &mut client,
            TraceOperationCode::CreateManagerPlan,
            "ptah_create_manager_plan",
            args.clone(),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::AgentId,
            ],
        )
        .await?;
    always_on_scan(&created_plan)?;
    let plan_id = required_string(&created_plan, &["plan", "planId"])?;
    validate_plan_steps(&created_plan, &args["steps"])?;
    probe.observe_action(ProbeAction::CreateAutonomousManagerPlan);

    let deadline = Instant::now() + std::time::Duration::from_secs(90);
    let mut saw_queued = false;
    while Instant::now() < deadline {
        let listed = client
            .call_tool(
                "ptah_list_work",
                json!({ "session_id": session_id, "workspace": workspace }),
            )
            .await
            .map_err(|_| DiagnosticCode::McpCallFailed)?;
        if listed.is_error {
            return Err(DiagnosticCode::McpToolError);
        }
        always_on_scan(&listed.structured)?;
        let items = work_for_step(&listed.structured, &fixture.step_first);
        if items.len() > 1 {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        if items.len() == 1
            && items[0]["workId"].as_str().is_some()
            && items[0]["state"].as_str() == Some("queued")
        {
            saw_queued = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    if !saw_queued {
        return Err(DiagnosticCode::Timeout);
    }
    wait_plan_succeeded(&mut client, &session_id, &workspace, &plan_id).await?;
    let listed = probe
        .call(
            &mut client,
            TraceOperationCode::ListWork,
            "ptah_list_work",
            json!({ "session_id": session_id, "workspace": workspace }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    let work = work_for_step(&listed, &fixture.step_first);
    if work.len() != 1 || work[0]["state"].as_str() != Some("succeeded") {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    probe.transition(
        EntityKind::Work,
        DurableStateCode::Queued,
        DurableStateCode::Succeeded,
        work[0]["workId"].as_str(),
    );

    let plan = probe
        .call(
            &mut client,
            TraceOperationCode::GetManagerPlan,
            "ptah_get_manager_plan",
            json!({
                "session_id": session_id,
                "workspace": workspace,
                "plan_id": plan_id
            }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    always_on_scan(&plan)?;
    if plan["plan"]["state"].as_str() != Some("succeeded") {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    probe.observe_action(ProbeAction::InspectManagerPlan);
    probe.observe_oracle(OracleCode::AlwaysOnPlanSucceeded);

    let work = probe
        .call(
            &mut client,
            TraceOperationCode::ListWork,
            "ptah_list_work",
            json!({ "session_id": session_id, "workspace": workspace }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    always_on_scan(&work)?;
    probe.observe_action(ProbeAction::InspectWorkSet);
    let runs = probe
        .call(
            &mut client,
            TraceOperationCode::ListRuns,
            "ptah_list_runs",
            json!({ "session_id": session_id, "workspace": workspace }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    always_on_scan(&runs)?;
    probe.observe_action(ProbeAction::InspectRunSet);
    let intents = probe
        .call(
            &mut client,
            TraceOperationCode::ListExecutionIntents,
            "ptah_list_execution_intents",
            json!({ "session_id": session_id, "workspace": workspace }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    always_on_scan(&intents)?;
    probe.observe_action(ProbeAction::InspectExecutionIntent);
    let _ = probe
        .call(
            &mut client,
            TraceOperationCode::GetCapacity,
            "ptah_get_capacity",
            json!({}),
            vec![],
        )
        .await?;

    for step in fixture.native_steps() {
        let items = work_for_step(&work, step);
        let expected_state = if step == fixture.step_failing {
            "failed"
        } else {
            "succeeded"
        };
        if items.len() != 1 || items[0]["state"].as_str() != Some(expected_state) {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        let work_id = items[0]["workId"]
            .as_str()
            .ok_or(DiagnosticCode::McpResultMalformed)?;
        let detailed = probe
            .call(
                &mut client,
                TraceOperationCode::GetWork,
                "ptah_get_work",
                json!({
                    "session_id": session_id,
                    "workspace": workspace,
                    "work_id": work_id
                }),
                vec![
                    ArgumentFieldCode::SessionId,
                    ArgumentFieldCode::Workspace,
                    ArgumentFieldCode::WorkId,
                ],
            )
            .await?;
        always_on_scan(&detailed)?;
        let attempts = detailed["attempts"]
            .as_array()
            .ok_or(DiagnosticCode::McpResultMalformed)?;
        if attempts.len() != 1
            || attempts[0]["linkedRunIds"].as_array().map(|ids| ids.len()) != Some(1)
        {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        let matching: Vec<_> = intents["intents"]
            .as_array()
            .ok_or(DiagnosticCode::McpResultMalformed)?
            .iter()
            .filter(|intent| {
                intent["workId"].as_str() == Some(work_id)
                    && intent["attemptId"].as_str() == attempts[0]["attemptId"].as_str()
            })
            .collect();
        if matching.len() != 1 {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        let intent = matching[0];
        let run_id = exact_single_linked_run(&attempts[0], None)?;
        if intent["runId"].as_str() != Some(run_id.as_str()) {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        always_on_require_unique_join(&work, &detailed, &intents, &runs, work_id, &run_id)?;
        if service.provider.count_for(step) != fixture.provider_posts_by_semantic[step] {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        let campaign_runs: Vec<_> = runs["runs"]
            .as_array()
            .ok_or(DiagnosticCode::McpResultMalformed)?
            .iter()
            .filter(|run| run["runId"].as_str() == Some(run_id.as_str()))
            .collect();
        if campaign_runs.len() != 1 {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        let campaign_run = campaign_runs[0];
        if campaign_run["requestId"].as_str() != intent["intentId"].as_str() {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        if intent["inputHash"].as_str().is_none_or(str::is_empty)
            || intent["workRevision"].as_u64().is_none()
            || intent["agentSpecRevision"].as_u64().is_none()
        {
            return Err(DiagnosticCode::McpResultMalformed);
        }
        if step == fixture.step_first
            && (campaign_run["state"].as_str() != Some("completed")
                || campaign_run["purpose"].as_str() == Some("manager_proposal"))
        {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        probe.retain_id(DurableIdKind::Work, work_id);
        probe.retain_id(
            DurableIdKind::Attempt,
            attempts[0]["attemptId"]
                .as_str()
                .ok_or(DiagnosticCode::McpResultMalformed)?,
        );
        probe.retain_id(DurableIdKind::Run, &run_id);
    }
    probe.observe_action(ProbeAction::InspectWorkAttempts);
    if work_items(&work)
        .iter()
        .filter(|item| item["kind"].as_str() == Some("manager-decision"))
        .count() as u64
        != fixture.decision_work
        || service.provider.count_for("manager-decision")
            != fixture.provider_posts_by_semantic["manager-decision"]
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let proposal = runs["runs"]
        .as_array()
        .ok_or(DiagnosticCode::McpResultMalformed)?
        .iter()
        .filter(|run| run["purpose"].as_str() == Some("manager_proposal"))
        .count() as u64;
    if proposal != fixture.proposal_runs {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    for run in runs["runs"]
        .as_array()
        .ok_or(DiagnosticCode::McpResultMalformed)?
    {
        if matches!(
            run["state"].as_str(),
            Some("completed" | "failed" | "cancelled" | "interrupted")
        ) && pending_usage(run) != 0
        {
            return Err(DiagnosticCode::RestartRecoveryFailed);
        }
    }
    probe.observe_oracle(OracleCode::NoDuplicateNativeRun);
    let before_post_success_tick =
        always_on_snapshot(probe, &mut client, &session_id, &workspace).await?;

    let replay = probe
        .call(
            &mut client,
            TraceOperationCode::CreateManagerPlan,
            "ptah_create_manager_plan",
            args.clone(),
            vec![ArgumentFieldCode::RequestId],
        )
        .await?;
    always_on_scan(&replay)?;
    if replay["plan"]["planId"].as_str() != Some(plan_id.as_str()) {
        return Err(DiagnosticCode::IdempotencyReplayMismatch);
    }
    let after_replay = always_on_snapshot(probe, &mut client, &session_id, &workspace).await?;
    assert_exact_cardinality(before_post_success_tick, after_replay)?;
    probe.observe_action(ProbeAction::ReplayRequest);
    probe.observe_oracle(OracleCode::RequestReplaySameResource);

    let mut conflict_args = args;
    conflict_args["objective"] = json!("Changed payload must conflict");
    let conflict = client
        .call_tool("ptah_create_manager_plan", conflict_args)
        .await;
    probe.counters.tool_calls = probe
        .counters
        .tool_calls
        .checked_add(1)
        .ok_or(DiagnosticCode::BoundExceeded)?;
    match conflict {
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
                TraceOperationCode::CreateManagerPlan,
                vec![ArgumentFieldCode::RequestId],
                Some(DiagnosticCode::IdempotencyConflictObserved),
            )?;
        }
        _ => return Err(DiagnosticCode::IdempotencyConflictUnproven),
    }
    let after_conflict = always_on_snapshot(probe, &mut client, &session_id, &workspace).await?;
    assert_exact_cardinality(after_replay, after_conflict)?;
    probe.observe_action(ProbeAction::ReplayChangedPayload);
    probe.observe_oracle(OracleCode::ChangedPayloadConflict);

    let _ = probe
        .call(
            &mut client,
            TraceOperationCode::TickManagerPlan,
            "ptah_tick_manager_plan",
            json!({
                "request_id": request_id("post-success"),
                "session_id": session_id,
                "workspace": workspace,
                "plan_id": plan_id
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
            ],
        )
        .await?;
    probe.observe_action(ProbeAction::TickManagerPlan);
    let after_post_success_tick =
        always_on_snapshot(probe, &mut client, &session_id, &workspace).await?;
    assert_exact_cardinality(before_post_success_tick, after_post_success_tick)?;
    assert_happy_path_counts(fixture, &service, &work, &runs, &intents)?;
    service
        .scan_artifacts()
        .map_err(|_| DiagnosticCode::OracleMismatch)?;
    Ok(())
}

async fn always_on_home_b(
    probe: &mut ProbeBuilder<'_>,
    fixture: &AlwaysOnFixture,
) -> Result<(), DiagnosticCode> {
    use crate::process_service::ProcessService;

    let mut service = ProcessService::spawn().map_err(process_service_spawn_diagnostic)?;
    let mut client = service
        .client()
        .await
        .map_err(|_| DiagnosticCode::ServiceUnreachable)?;
    let workspace = service.workspace.display().to_string();
    let (session_id, agent_id) = always_on_bootstrap(probe, &mut client, &workspace).await?;
    service
        .provider
        .arm(&fixture.step_first, ProviderDisposition::Hold);
    let plan_args =
        fixture.plan_arguments(&request_id("plan-b"), &session_id, &workspace, &agent_id);
    let expected_plan_steps = plan_args["steps"].clone();
    let created_plan = probe
        .call(
            &mut client,
            TraceOperationCode::CreateManagerPlan,
            "ptah_create_manager_plan",
            plan_args,
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::AgentId,
            ],
        )
        .await?;
    always_on_scan(&created_plan)?;
    let plan_id = required_string(&created_plan, &["plan", "planId"])?;
    validate_plan_steps(&created_plan, &expected_plan_steps)?;
    service
        .provider
        .wait_accepted(&fixture.step_first, Duration::from_secs(90))
        .map_err(|_| DiagnosticCode::Timeout)?;
    let join = always_on_find_in_flight(
        probe,
        &mut client,
        &session_id,
        &workspace,
        &fixture.step_first,
    )
    .await?;
    let work_id = join.work_id.clone();
    let run_id = join.run_id.clone();
    if service.provider.count_for(&fixture.step_first)
        != fixture.provider_posts_by_semantic[fixture.step_first.as_str()]
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let pre_plan = probe
        .call(
            &mut client,
            TraceOperationCode::GetManagerPlan,
            "ptah_get_manager_plan",
            json!({
                "session_id": session_id,
                "workspace": workspace,
                "plan_id": plan_id
            }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    always_on_scan(&pre_plan)?;
    if pre_plan["plan"]["planId"].as_str() != Some(plan_id.as_str()) {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let pre_plan_state = pre_plan["plan"]["state"].as_str().map(str::to_string);
    let pre_plan_steps = pre_plan["plan"]["steps"].clone();
    let pre_plan_hash = plan_identity_hash(&pre_plan);
    let pid0 = service.pid();
    probe.push_trace(TraceOperationCode::Disconnect, vec![], None)?;
    drop(client);
    probe.observe_action(ProbeAction::DisconnectClient);
    probe.reconnect.attempted = true;
    probe.push_trace(TraceOperationCode::Restart, vec![], None)?;
    probe.observe_action(ProbeAction::RestartService);
    probe.restart.attempted = true;
    probe.restart.host_owned = true;
    probe.counters.restarts = probe
        .counters
        .restarts
        .checked_add(1)
        .ok_or(DiagnosticCode::BoundExceeded)?;
    service
        .respawn()
        .map_err(|_| DiagnosticCode::RestartRecoveryFailed)?;
    if service.pid() == pid0 {
        return Err(DiagnosticCode::RestartRecoveryFailed);
    }
    probe.push_trace(TraceOperationCode::Reconnect, vec![], None)?;
    client = service
        .client()
        .await
        .map_err(|_| DiagnosticCode::ServiceUnreachable)?;
    probe.observe_action(ProbeAction::ReconnectClient);
    probe.reconnect.reinitialized = true;
    probe.observe_oracle(OracleCode::RestartReconnectObserved);
    always_on_assert_durable_plan(
        probe,
        &mut client,
        &session_id,
        &workspace,
        &plan_id,
        pre_plan_state.as_deref(),
        &pre_plan_steps,
        &pre_plan_hash,
    )
    .await?;
    always_on_assert_interrupted_recovery(
        probe,
        &mut client,
        &service,
        &session_id,
        &workspace,
        &work_id,
        &join.attempt_id,
        &run_id,
        &fixture.step_first,
        fixture.provider_posts_by_semantic[fixture.step_first.as_str()],
        true,
    )
    .await?;
    tokio::time::sleep(fixture.zero_growth_window).await;
    always_on_assert_interrupted_recovery(
        probe,
        &mut client,
        &service,
        &session_id,
        &workspace,
        &work_id,
        &join.attempt_id,
        &run_id,
        &fixture.step_first,
        fixture.provider_posts_by_semantic[fixture.step_first.as_str()],
        false,
    )
    .await?;
    if service.provider.count_for(&fixture.step_first)
        != fixture.provider_posts_by_semantic[fixture.step_first.as_str()]
    {
        probe.restart.implicit_execution_observed = true;
        return Err(DiagnosticCode::RestartRecoveryFailed);
    }
    let pid1 = service.pid();
    probe.push_trace(TraceOperationCode::Disconnect, vec![], None)?;
    drop(client);
    probe.push_trace(TraceOperationCode::Restart, vec![], None)?;
    probe.counters.restarts = probe
        .counters
        .restarts
        .checked_add(1)
        .ok_or(DiagnosticCode::BoundExceeded)?;
    service
        .respawn()
        .map_err(|_| DiagnosticCode::RestartRecoveryFailed)?;
    if service.pid() == pid1 || service.pid() == pid0 {
        return Err(DiagnosticCode::RestartRecoveryFailed);
    }
    probe.push_trace(TraceOperationCode::Reconnect, vec![], None)?;
    client = service
        .client()
        .await
        .map_err(|_| DiagnosticCode::ServiceUnreachable)?;
    always_on_assert_durable_plan(
        probe,
        &mut client,
        &session_id,
        &workspace,
        &plan_id,
        pre_plan_state.as_deref(),
        &pre_plan_steps,
        &pre_plan_hash,
    )
    .await?;
    always_on_assert_interrupted_recovery(
        probe,
        &mut client,
        &service,
        &session_id,
        &workspace,
        &work_id,
        &join.attempt_id,
        &run_id,
        &fixture.step_first,
        fixture.provider_posts_by_semantic[fixture.step_first.as_str()],
        false,
    )
    .await?;
    tokio::time::sleep(fixture.zero_growth_window).await;
    always_on_assert_interrupted_recovery(
        probe,
        &mut client,
        &service,
        &session_id,
        &workspace,
        &work_id,
        &join.attempt_id,
        &run_id,
        &fixture.step_first,
        fixture.provider_posts_by_semantic[fixture.step_first.as_str()],
        false,
    )
    .await?;
    if service.provider.count_for(&fixture.step_first)
        != fixture.provider_posts_by_semantic[fixture.step_first.as_str()]
    {
        probe.restart.implicit_execution_observed = true;
        return Err(DiagnosticCode::RestartRecoveryFailed);
    }
    probe.observe_oracle(OracleCode::InterruptedRunNotReadmittedWithinWindow);
    probe.observe_oracle(OracleCode::NoImplicitInvocationResume);
    probe.provider_observation = Some(service.provider.observation());
    service
        .scan_artifacts()
        .map_err(|_| DiagnosticCode::OracleMismatch)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn always_on_assert_durable_plan(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    session_id: &str,
    workspace: &str,
    plan_id: &str,
    pre_state: Option<&str>,
    pre_steps: &Value,
    pre_hash: &str,
) -> Result<(), DiagnosticCode> {
    let recovered_plan = probe
        .call(
            client,
            TraceOperationCode::GetManagerPlan,
            "ptah_get_manager_plan",
            json!({
                "session_id": session_id,
                "workspace": workspace,
                "plan_id": plan_id
            }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    always_on_scan(&recovered_plan)?;
    let recovered_steps = recovered_plan["plan"]["steps"].clone();
    if recovered_plan["plan"]["planId"].as_str() != Some(plan_id)
        || !plan_state_survived_restart(pre_state, recovered_plan["plan"]["state"].as_str())
        || plan_step_identity(&recovered_steps) != plan_step_identity(pre_steps)
        || plan_identity_hash(&recovered_plan) != pre_hash
    {
        return Err(DiagnosticCode::RestartRecoveryFailed);
    }
    probe.restart.durable_read_recovered = true;
    probe.observe_oracle(OracleCode::DurableReadAfterRestart);
    Ok(())
}

fn always_on_require_unique_join(
    work: &Value,
    detailed: &Value,
    intents: &Value,
    runs: &Value,
    work_id: &str,
    run_id: &str,
) -> Result<(), DiagnosticCode> {
    let items: Vec<&Value> = work_items(work)
        .iter()
        .filter(|item| item["workId"].as_str() == Some(work_id))
        .collect();
    if items.len() != 1 || items[0]["state"].as_str() == Some("queued") {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    if detailed["work"]["workId"].as_str() != Some(work_id) {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let attempts = exact_array(detailed, "attempts")?;
    if attempts.len() != 1 {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let attempt_id = attempts[0]["attemptId"]
        .as_str()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    exact_single_linked_run(&attempts[0], Some(run_id))?;
    let matching_intents: Vec<&Value> = exact_array(intents, "intents")?
        .iter()
        .filter(|intent| {
            intent["workId"].as_str() == Some(work_id)
                && intent["attemptId"].as_str() == Some(attempt_id)
        })
        .collect();
    if matching_intents.len() != 1 {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let intent = matching_intents[0];
    if intent["runId"].as_str() != Some(run_id)
        || intent["inputHash"].as_str().is_none_or(str::is_empty)
        || intent["workRevision"].as_u64().is_none()
        || intent["agentSpecRevision"].as_u64().is_none()
    {
        return Err(DiagnosticCode::McpResultMalformed);
    }
    let matching_runs: Vec<&Value> = exact_array(runs, "runs")?
        .iter()
        .filter(|run| run["runId"].as_str() == Some(run_id))
        .collect();
    if matching_runs.len() != 1 {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    if matching_runs[0]["requestId"].as_str() != intent["intentId"].as_str() {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn always_on_assert_interrupted_recovery(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    service: &crate::process_service::ProcessService,
    session_id: &str,
    workspace: &str,
    work_id: &str,
    attempt_id: &str,
    run_id: &str,
    expected_step_id: &str,
    expected_provider_posts: u64,
    stamp_transitions: bool,
) -> Result<(), DiagnosticCode> {
    wait_run_terminal(client, session_id, workspace, run_id).await?;
    let recovered_run = probe
        .call(
            client,
            TraceOperationCode::GetRun,
            "ptah_get_run",
            json!({
                "session_id": session_id,
                "workspace": workspace,
                "run_id": run_id
            }),
            vec![
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::RunId,
            ],
        )
        .await?;
    always_on_scan(&recovered_run)?;
    if recovered_run["runId"].as_str() != Some(run_id)
        || recovered_run["state"].as_str() != Some("interrupted")
        || pending_usage(&recovered_run) != 0
    {
        return Err(DiagnosticCode::RestartRecoveryFailed);
    }
    if stamp_transitions {
        probe.transition(
            EntityKind::Run,
            DurableStateCode::Running,
            DurableStateCode::Interrupted,
            Some(run_id),
        );
    }
    let work_deadline = Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if Instant::now() >= work_deadline {
            return Err(DiagnosticCode::Timeout);
        }
        match client
            .call_tool(
                "ptah_get_work",
                json!({
                    "session_id": session_id,
                    "workspace": workspace,
                    "work_id": work_id
                }),
            )
            .await
        {
            Ok(result) if !result.is_error => {
                always_on_scan(&result.structured)?;
                match result.structured["work"]["state"].as_str() {
                    Some("queued") => {
                        probe.restart.implicit_execution_observed = true;
                        return Err(DiagnosticCode::RestartRecoveryFailed);
                    }
                    Some("failed") => break,
                    _ => {}
                }
            }
            _ => {}
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let recovered_work = probe
        .call(
            client,
            TraceOperationCode::GetWork,
            "ptah_get_work",
            json!({
                "session_id": session_id,
                "workspace": workspace,
                "work_id": work_id
            }),
            vec![
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::WorkId,
            ],
        )
        .await?;
    always_on_scan(&recovered_work)?;
    if recovered_work["work"]["state"].as_str() != Some("failed")
        || recovered_work["work"]["workId"].as_str() != Some(work_id)
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let attempts = exact_array(&recovered_work, "attempts")?;
    if attempts.len() != 1
        || attempts[0]["attemptId"].as_str() != Some(attempt_id)
        || exact_single_linked_run(&attempts[0], Some(run_id)).is_err()
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let listed_work = probe
        .call(
            client,
            TraceOperationCode::ListWork,
            "ptah_list_work",
            json!({ "session_id": session_id, "workspace": workspace }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    always_on_scan(&listed_work)?;
    let listed_intents = probe
        .call(
            client,
            TraceOperationCode::ListExecutionIntents,
            "ptah_list_execution_intents",
            json!({ "session_id": session_id, "workspace": workspace }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    always_on_scan(&listed_intents)?;
    let listed_runs = probe
        .call(
            client,
            TraceOperationCode::ListRuns,
            "ptah_list_runs",
            json!({ "session_id": session_id, "workspace": workspace }),
            vec![ArgumentFieldCode::SessionId, ArgumentFieldCode::Workspace],
        )
        .await?;
    always_on_scan(&listed_runs)?;
    always_on_require_unique_join(
        &listed_work,
        &recovered_work,
        &listed_intents,
        &listed_runs,
        work_id,
        run_id,
    )?;
    if stamp_transitions {
        probe.transition(
            EntityKind::Work,
            DurableStateCode::Running,
            DurableStateCode::Failed,
            Some(work_id),
        );
    }
    if service.provider.count_for(expected_step_id) != expected_provider_posts {
        probe.restart.implicit_execution_observed = true;
        return Err(DiagnosticCode::RestartRecoveryFailed);
    }
    Ok(())
}

async fn wait_run_terminal(
    client: &mut grokptah_agent_bridge::McpControlClient,
    session_id: &str,
    workspace: &str,
    run_id: &str,
) -> Result<(), DiagnosticCode> {
    let deadline = Instant::now() + std::time::Duration::from_secs(60);
    while Instant::now() < deadline {
        match client
            .call_tool(
                "ptah_get_run",
                json!({
                    "session_id": session_id,
                    "workspace": workspace,
                    "run_id": run_id
                }),
            )
            .await
        {
            Ok(result) if !result.is_error => {
                let state = result
                    .structured
                    .pointer("/run/state")
                    .and_then(Value::as_str)
                    .or_else(|| result.structured["state"].as_str());
                if matches!(
                    state,
                    Some("completed" | "failed" | "cancelled" | "interrupted")
                ) {
                    return Ok(());
                }
            }
            _ => {}
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    Err(DiagnosticCode::Timeout)
}

async fn wait_plan_succeeded(
    client: &mut grokptah_agent_bridge::McpControlClient,
    session_id: &str,
    workspace: &str,
    plan_id: &str,
) -> Result<(), DiagnosticCode> {
    let deadline = Instant::now() + std::time::Duration::from_secs(90);
    while Instant::now() < deadline {
        if let Ok(result) = client
            .call_tool(
                "ptah_get_manager_plan",
                json!({
                    "session_id": session_id,
                    "workspace": workspace,
                    "plan_id": plan_id
                }),
            )
            .await
        {
            if !result.is_error && result.structured["plan"]["state"].as_str() == Some("succeeded")
            {
                return Ok(());
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    Err(DiagnosticCode::Timeout)
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
                        "maxTotalTokens": 20000
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
                        "maxTotalTokens": 20000
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
            "Use the write_file tool now to create exactly one small file named permission_probe.txt in the disposable workspace. This action is intentionally permission-gated; do not merely explain it or return until the tool call is attempted. After the permission decision, stop.",
        ),
        (
            "deny",
            "Use the write_file tool now to create exactly one small file named permission_probe.txt in the disposable workspace. This action is intentionally permission-gated; do not merely explain it or return until the tool call is attempted. After the permission decision, stop.",
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
    // A live model must actually request the permission-gated tool. Do not
    // spend the full campaign bound waiting for prose or a non-tool answer;
    // after this bounded capability window the probe is skipped and the
    // disposable native Runs are explicitly cancelled.
    for _ in 0..120 {
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
        for work_id in &work_ids {
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
                .await;
            let run_ids = snapshot
                .ok()
                .and_then(|value| value["attempts"].as_array().cloned())
                .into_iter()
                .flatten()
                .flat_map(|attempt| attempt["linkedRunIds"].as_array().cloned())
                .flatten()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect::<Vec<_>>();
            for run_id in run_ids {
                let _ = probe
                    .call(
                        client,
                        TraceOperationCode::CancelRun,
                        "ptah_cancel",
                        json!({
                            "request_id": request_id("permission-capability-cancel"),
                            "session_id": agent.session_id,
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
                    .await;
            }
        }
        return Err(DiagnosticCode::PermissionCapabilityAbsent);
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
                        "maxTotalTokens": 20000
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
    // This path is the implementation for OracleCode::InterruptedRunNotReadmittedWithinWindow.
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
                        "maxTotalTokens": 20000
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

/// One manager plan driven end to end through the public MCP surface:
/// creation, a non-executable root container, dependency-ordered advance,
/// revision-fenced observation, a failed step, and an explicit replan that
/// supersedes the failure and reaches terminal success.
async fn manager_plan_lifecycle(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
) -> Result<(), DiagnosticCode> {
    let agent = create_agent(probe, client, workspace).await?;
    let created = probe
        .call(
            client,
            TraceOperationCode::CreateManagerPlan,
            "ptah_create_manager_plan",
            json!({
                "request_id": request_id("manager-plan"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "manager_agent_id": agent.agent_id,
                "objective": "Certify the bounded manager plan lifecycle",
                "steps": [
                    {
                        "stepId": "inspect",
                        "kind": "certification",
                        "objective": "Inspect the disposable fixture",
                    },
                    {
                        "stepId": "report",
                        "kind": "certification",
                        "objective": "Report the inspection result",
                        "dependencies": ["inspect"],
                    },
                ],
                "max_in_flight": 1,
                "max_replans": 2,
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::AgentId,
                ArgumentFieldCode::Objective,
                ArgumentFieldCode::Steps,
            ],
        )
        .await?;
    let plan_id = required_string(&created, &["plan", "planId"])?;
    let root_work_id = required_string(&created, &["plan", "rootWorkId"])?;
    if created["plan"]["state"].as_str() != Some("active") {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let created_revision = required_revision(&created, &["plan", "revision"])?;
    probe.transition(
        EntityKind::ManagerPlan,
        DurableStateCode::Absent,
        DurableStateCode::Created,
        Some(&plan_id),
    );
    probe.retain_id(DurableIdKind::ManagerPlan, &plan_id);

    // The plan root is an explicit host-enforced container: it is visible as
    // Work but must never be claimable, so it can never execute.
    let root = probe
        .call(
            client,
            TraceOperationCode::GetWork,
            "ptah_get_work",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
                "work_id": root_work_id,
            }),
            vec![
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::WorkId,
            ],
        )
        .await?;
    if root["work"]["isContainer"].as_bool() != Some(true) {
        return Err(DiagnosticCode::OracleMismatch);
    }
    expect_rejected(
        probe,
        client,
        TraceOperationCode::ClaimWork,
        "ptah_claim_work",
        json!({
            "request_id": request_id("manager-root-claim"),
            "session_id": agent.session_id,
            "workspace": workspace,
            "work_id": root_work_id,
        }),
        vec![
            ArgumentFieldCode::RequestId,
            ArgumentFieldCode::SessionId,
            ArgumentFieldCode::Workspace,
            ArgumentFieldCode::WorkId,
        ],
        DiagnosticCode::OracleMismatch,
    )
    .await?;

    // Advance materializes only the step whose dependencies are satisfied.
    let advance_args = json!({
        "request_id": request_id("manager-advance-first"),
        "session_id": agent.session_id,
        "workspace": workspace,
        "plan_id": plan_id,
        "expected_revision": created_revision,
    });
    let advanced = probe
        .call(
            client,
            TraceOperationCode::AdvanceManagerPlan,
            "ptah_advance_manager_plan",
            advance_args.clone(),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::PlanId,
                ArgumentFieldCode::ExpectedRevision,
            ],
        )
        .await?;
    let first_work = single_created_work(&advanced)?;
    if step_state(&advanced, "report")? != "pending" {
        return Err(DiagnosticCode::OracleMismatch);
    }
    probe.transition(
        EntityKind::ManagerStep,
        DurableStateCode::Created,
        DurableStateCode::Advanced,
        Some(&first_work),
    );

    // Replaying one advance request must return the same materialized Work.
    let replayed = probe
        .call(
            client,
            TraceOperationCode::AdvanceManagerPlan,
            "ptah_advance_manager_plan",
            advance_args,
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::PlanId,
                ArgumentFieldCode::ExpectedRevision,
            ],
        )
        .await?;
    if single_created_work(&replayed)? != first_work {
        return Err(DiagnosticCode::IdempotencyReplayMismatch);
    }

    // A superseded plan revision must not be able to mutate the plan.
    expect_rejected(
        probe,
        client,
        TraceOperationCode::AdvanceManagerPlan,
        "ptah_advance_manager_plan",
        json!({
            "request_id": request_id("manager-advance-stale"),
            "session_id": agent.session_id,
            "workspace": workspace,
            "plan_id": plan_id,
            "expected_revision": created_revision,
        }),
        vec![
            ArgumentFieldCode::RequestId,
            ArgumentFieldCode::SessionId,
            ArgumentFieldCode::Workspace,
            ArgumentFieldCode::PlanId,
            ArgumentFieldCode::ExpectedRevision,
        ],
        DiagnosticCode::OracleMismatch,
    )
    .await?;

    complete_manager_work(probe, client, workspace, &agent, &first_work).await?;

    // One tick projects the terminal child outcome into exactly one durable
    // notification, and repeating it does not notify the same Work revision
    // twice.
    let plan_revision = current_plan_revision(probe, client, workspace, &agent, &plan_id).await?;
    let ticked = probe
        .call(
            client,
            TraceOperationCode::TickManagerPlan,
            "ptah_tick_manager_plan",
            json!({
                "request_id": request_id("manager-tick"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "plan_id": plan_id,
                "expected_revision": plan_revision,
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::PlanId,
                ArgumentFieldCode::ExpectedRevision,
            ],
        )
        .await?;
    let notified = ticked["messages"].as_array().map_or(0, Vec::len);
    if notified == 0 {
        return Err(DiagnosticCode::OracleMismatch);
    }
    let repeat_revision = required_revision(&ticked, &["plan", "revision"])?;
    let repeated = probe
        .call(
            client,
            TraceOperationCode::TickManagerPlan,
            "ptah_tick_manager_plan",
            json!({
                "request_id": request_id("manager-tick-repeat"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "plan_id": plan_id,
                "expected_revision": repeat_revision,
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::PlanId,
                ArgumentFieldCode::ExpectedRevision,
            ],
        )
        .await?;
    if repeated["messages"].as_array().map_or(0, Vec::len) != 0 {
        return Err(DiagnosticCode::OracleMismatch);
    }

    // A tick advances the active plan before it observes, so the dependent
    // step is materialized once its dependency succeeded — and only then.
    let observed = fetch_plan(probe, client, workspace, &agent, &plan_id).await?;
    if step_state(&observed, "inspect")? != "succeeded" {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let second_work = step_work_id(&observed, "report")?;

    fail_manager_work(probe, client, workspace, &agent, &second_work).await?;

    // A failed child stops the plan for an explicit decision. No replacement
    // Work may be invented on the plan's behalf.
    let plan_revision = current_plan_revision(probe, client, workspace, &agent, &plan_id).await?;
    let halted = probe
        .call(
            client,
            TraceOperationCode::AdvanceManagerPlan,
            "ptah_advance_manager_plan",
            json!({
                "request_id": request_id("manager-advance-failure"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "plan_id": plan_id,
                "expected_revision": plan_revision,
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::PlanId,
                ArgumentFieldCode::ExpectedRevision,
            ],
        )
        .await?;
    if halted["plan"]["state"].as_str() != Some("needs_replan") {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    if halted["createdWork"]
        .as_array()
        .is_some_and(|created| !created.is_empty())
    {
        return Err(DiagnosticCode::OracleMismatch);
    }
    probe.transition(
        EntityKind::ManagerPlan,
        DurableStateCode::Active,
        DurableStateCode::NeedsReplan,
        Some(&plan_id),
    );

    // An explicit replan supersedes the failed step and its blocked
    // descendants, and the plan can still reach terminal success.
    let replanned = probe
        .call(
            client,
            TraceOperationCode::ReplanManagerPlan,
            "ptah_replan_manager_plan",
            json!({
                "request_id": request_id("manager-replan"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "plan_id": plan_id,
                "reason": "Supersede the failed certification step",
                "steps": [{
                    "stepId": "replacement",
                    "kind": "certification",
                    "objective": "Report independently of the failed step",
                }],
                "expected_revision": required_revision(&halted, &["plan", "revision"])?,
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::PlanId,
                ArgumentFieldCode::Reason,
                ArgumentFieldCode::Steps,
                ArgumentFieldCode::ExpectedRevision,
            ],
        )
        .await?;
    if step_state(&replanned, "report")? != "superseded" {
        return Err(DiagnosticCode::OracleMismatch);
    }
    probe.transition(
        EntityKind::ManagerStep,
        DurableStateCode::Failed,
        DurableStateCode::Superseded,
        Some(&plan_id),
    );

    let resumed = probe
        .call(
            client,
            TraceOperationCode::AdvanceManagerPlan,
            "ptah_advance_manager_plan",
            json!({
                "request_id": request_id("manager-advance-replanned"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "plan_id": plan_id,
                "expected_revision": required_revision(&replanned, &["plan", "revision"])?,
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::PlanId,
                ArgumentFieldCode::ExpectedRevision,
            ],
        )
        .await?;
    let replacement_work = single_created_work(&resumed)?;
    complete_manager_work(probe, client, workspace, &agent, &replacement_work).await?;

    let plan_revision = current_plan_revision(probe, client, workspace, &agent, &plan_id).await?;
    let settled = probe
        .call(
            client,
            TraceOperationCode::AdvanceManagerPlan,
            "ptah_advance_manager_plan",
            json!({
                "request_id": request_id("manager-advance-final"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "plan_id": plan_id,
                "expected_revision": plan_revision,
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::PlanId,
                ArgumentFieldCode::ExpectedRevision,
            ],
        )
        .await?;
    if settled["plan"]["state"].as_str() != Some("succeeded") {
        return Err(DiagnosticCode::TerminalStateMissing);
    }
    probe.transition(
        EntityKind::ManagerPlan,
        DurableStateCode::NeedsReplan,
        DurableStateCode::Succeeded,
        Some(&plan_id),
    );
    Ok(())
}

fn required_revision(value: &Value, path: &[&str]) -> Result<u64, DiagnosticCode> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(*key).ok_or(DiagnosticCode::McpResultMalformed)?;
    }
    cursor.as_u64().ok_or(DiagnosticCode::McpResultMalformed)
}

fn single_created_work(value: &Value) -> Result<String, DiagnosticCode> {
    let created = value["createdWork"]
        .as_array()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    if created.len() != 1 {
        return Err(DiagnosticCode::OracleMismatch);
    }
    required_string(&created[0], &["workId"])
}

fn step_work_id(value: &Value, step_id: &str) -> Result<String, DiagnosticCode> {
    value["plan"]["steps"]
        .as_array()
        .ok_or(DiagnosticCode::McpResultMalformed)?
        .iter()
        .find(|step| step["stepId"].as_str() == Some(step_id))
        .and_then(|step| step["workId"].as_str())
        .map(str::to_owned)
        .ok_or(DiagnosticCode::McpResultMalformed)
}

fn step_state(value: &Value, step_id: &str) -> Result<String, DiagnosticCode> {
    value["plan"]["steps"]
        .as_array()
        .ok_or(DiagnosticCode::McpResultMalformed)?
        .iter()
        .find(|step| step["stepId"].as_str() == Some(step_id))
        .and_then(|step| step["state"].as_str())
        .map(str::to_owned)
        .ok_or(DiagnosticCode::McpResultMalformed)
}

async fn fetch_plan(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
    agent: &TestAgent,
    plan_id: &str,
) -> Result<Value, DiagnosticCode> {
    probe
        .call(
            client,
            TraceOperationCode::GetManagerPlan,
            "ptah_get_manager_plan",
            json!({
                "session_id": agent.session_id,
                "workspace": workspace,
                "plan_id": plan_id,
            }),
            vec![
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::PlanId,
            ],
        )
        .await
}

async fn current_plan_revision(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
    agent: &TestAgent,
    plan_id: &str,
) -> Result<u64, DiagnosticCode> {
    let plan = fetch_plan(probe, client, workspace, agent, plan_id).await?;
    required_revision(&plan, &["plan", "revision"])
}

async fn claim_manager_work(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
    agent: &TestAgent,
    work_id: &str,
    prefix: &str,
) -> Result<(String, String), DiagnosticCode> {
    let claimed = probe
        .call(
            client,
            TraceOperationCode::ClaimWork,
            "ptah_claim_work",
            json!({
                "request_id": request_id(prefix),
                "session_id": agent.session_id,
                "workspace": workspace,
                "work_id": work_id,
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::WorkId,
            ],
        )
        .await?;
    Ok((
        required_string(&claimed, &["attempt", "attemptId"])?,
        required_string(&claimed, &["leaseToken"])?,
    ))
}

async fn complete_manager_work(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
    agent: &TestAgent,
    work_id: &str,
) -> Result<(), DiagnosticCode> {
    let (attempt_id, lease_token) =
        claim_manager_work(probe, client, workspace, agent, work_id, "manager-claim").await?;
    probe
        .call(
            client,
            TraceOperationCode::CompleteWork,
            "ptah_complete_work",
            json!({
                "request_id": request_id("manager-complete"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "work_id": work_id,
                "attempt_id": attempt_id,
                "lease_token": lease_token,
                "summary": "Certification step completed",
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
    probe.retain_id(DurableIdKind::Work, work_id);
    probe.transition(
        EntityKind::Work,
        DurableStateCode::Claimed,
        DurableStateCode::Completed,
        Some(work_id),
    );
    Ok(())
}

async fn fail_manager_work(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    workspace: &str,
    agent: &TestAgent,
    work_id: &str,
) -> Result<(), DiagnosticCode> {
    let (attempt_id, lease_token) = claim_manager_work(
        probe,
        client,
        workspace,
        agent,
        work_id,
        "manager-claim-fail",
    )
    .await?;
    let failed = probe
        .call(
            client,
            TraceOperationCode::FailWork,
            "ptah_fail_work",
            json!({
                "request_id": request_id("manager-fail"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "work_id": work_id,
                "attempt_id": attempt_id,
                "lease_token": lease_token,
                "summary": "Certification step failed",
                "failure": "synthetic certification failure",
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
    // Retain the failed outcome for the manager decision instead of letting
    // Work retry policy re-queue it.
    probe
        .call(
            client,
            TraceOperationCode::CancelWork,
            "ptah_cancel_work",
            json!({
                "request_id": request_id("manager-fail-seal"),
                "session_id": agent.session_id,
                "workspace": workspace,
                "work_id": work_id,
                "reason": "Preserve the failed outcome for an explicit replan",
                "expected_revision": required_revision(&failed, &["work", "revision"])?,
            }),
            vec![
                ArgumentFieldCode::RequestId,
                ArgumentFieldCode::SessionId,
                ArgumentFieldCode::Workspace,
                ArgumentFieldCode::WorkId,
                ArgumentFieldCode::Reason,
                ArgumentFieldCode::ExpectedRevision,
            ],
        )
        .await?;
    probe.retain_id(DurableIdKind::Work, work_id);
    probe.transition(
        EntityKind::Work,
        DurableStateCode::Claimed,
        DurableStateCode::Failed,
        Some(work_id),
    );
    Ok(())
}

/// Call a tool that the host must refuse, and fail the probe when it is
/// accepted. A rejected call is evidence, so it is still traced.
async fn expect_rejected(
    probe: &mut ProbeBuilder<'_>,
    client: &mut McpControlClient,
    operation: TraceOperationCode,
    tool: &str,
    arguments: Value,
    argument_fields: Vec<ArgumentFieldCode>,
    accepted: DiagnosticCode,
) -> Result<(), DiagnosticCode> {
    probe.counters.tool_calls = probe
        .counters
        .tool_calls
        .checked_add(1)
        .ok_or(DiagnosticCode::BoundExceeded)?;
    let outcome = client.call_tool(tool, arguments).await;
    match outcome {
        Ok(result) if !result.is_error => {
            probe.push_trace(operation, argument_fields, Some(accepted))?;
            Err(accepted)
        }
        _ => {
            probe.counters.errors = probe
                .counters
                .errors
                .checked_add(1)
                .ok_or(DiagnosticCode::BoundExceeded)?;
            probe.push_trace(operation, argument_fields, None)?;
            Ok(())
        }
    }
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
                    "maxDurationMs": 15000,
                    "maxTotalTokens": 10000
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
    fn always_on_manifest_probe_has_the_exact_concrete_process_allowlist() {
        let manifest = CampaignManifest::bundled().unwrap();
        let definition = manifest
            .probe("always-on-grokbot-lifecycle-v1")
            .expect("manifest always-on probe");
        let implemented = implementation_tools(&definition.id).expect("concrete implementation");
        assert_eq!(
            implemented,
            &[
                "ptah_create_session",
                "ptah_submit_task",
                "ptah_cancel",
                "ptah_get_run",
                "ptah_list_persistent_agents",
                "ptah_set_managed_execution",
                "ptah_create_manager_plan",
                "ptah_tick_manager_plan",
                "ptah_get_manager_plan",
                "ptah_list_work",
                "ptah_get_work",
                "ptah_list_runs",
                "ptah_list_execution_intents",
                "ptah_get_capacity",
            ]
        );
        assert_eq!(implemented.len(), 14);
        assert_eq!(definition.required_tools.len(), 14);
    }

    #[test]
    fn missing_process_service_binary_stays_configuration_indeterminate() {
        let diagnostic =
            process_service_spawn_diagnostic(anyhow::anyhow!("GROKPTAH_SERVICE_BIN is not a file"));
        assert_eq!(diagnostic, DiagnosticCode::ProbeImplementationUnavailable);
        let manifest = CampaignManifest::bundled().unwrap();
        let definition = manifest
            .probe("always-on-grokbot-lifecycle-v1")
            .expect("manifest always-on probe");
        let execution =
            ProbeBuilder::new(definition).finish(ProbeStatus::Indeterminate, diagnostic);
        assert_eq!(execution.result.status, ProbeStatus::Indeterminate);
        assert_eq!(
            execution.result.failure_class,
            crate::report::FailureClass::Configuration
        );
        assert_eq!(
            execution.result.diagnostics,
            vec![DiagnosticCode::ProbeImplementationUnavailable]
        );
    }

    #[test]
    fn process_service_lifecycle_failure_remains_a_hard_failure() {
        let diagnostic =
            process_service_spawn_diagnostic(anyhow::anyhow!("spawn grokptah-service failed"));
        assert_eq!(diagnostic, DiagnosticCode::RestartControlUnavailable);
        let manifest = CampaignManifest::bundled().unwrap();
        let definition = manifest
            .probe("always-on-grokbot-lifecycle-v1")
            .expect("manifest always-on probe");
        let execution = ProbeBuilder::new(definition).finish(ProbeStatus::Failed, diagnostic);
        assert_eq!(execution.result.status, ProbeStatus::Failed);
        assert_eq!(
            execution.result.failure_class,
            crate::report::FailureClass::Oracle
        );
    }

    #[test]
    fn always_on_probe_process_service_launch_is_concrete_when_configured() {
        let Ok(binary) = std::env::var("GROKPTAH_SERVICE_BIN") else {
            return;
        };
        if !std::path::Path::new(&binary).is_file() {
            return;
        }
        let service = crate::process_service::ProcessService::spawn()
            .expect("configured Always-On service binary must launch");
        assert!(service.pid() > 0);
        assert!(service.addr.parse::<std::net::SocketAddr>().is_ok());
    }

    #[test]
    fn always_on_fixture_rejects_each_happy_path_identity_and_count_mutation() {
        let canonical: Value = serde_json::from_slice(crate::ALWAYS_ON_GROKBOT_FIXTURE).unwrap();
        let mut mutants = Vec::new();
        for path in [
            ["steps", "first"],
            ["steps", "failing"],
            ["steps", "replacement"],
        ] {
            let mut mutant = canonical.clone();
            mutant[path[0]][path[1]] = json!("unexpected-step");
            mutants.push(mutant);
        }
        for path in [
            ["happyPath", "decisionWork"],
            ["happyPath", "proposalRunsObserved"],
        ] {
            let mut mutant = canonical.clone();
            mutant[path[0]][path[1]] = json!(2);
            mutants.push(mutant);
        }
        for (section, key) in [
            ("nativeWorkByStep", "step-a"),
            ("providerPostsBySemanticId", "step-a"),
            ("providerPostsBySemanticId", "manager-decision"),
        ] {
            let mut mutant = canonical.clone();
            mutant["happyPath"][section][key] = json!(2);
            mutants.push(mutant);
        }
        let mut missing_replacement = canonical.clone();
        missing_replacement["happyPath"]["nativeWorkByStep"]
            .as_object_mut()
            .unwrap()
            .remove("step-b-fix");
        mutants.push(missing_replacement);
        let mut missing_provider = canonical.clone();
        missing_provider["happyPath"]["providerPostsBySemanticId"]
            .as_object_mut()
            .unwrap()
            .remove("manager-decision");
        mutants.push(missing_provider);
        for mutant in mutants {
            assert_eq!(
                AlwaysOnFixture::from_value(&mutant),
                Err(DiagnosticCode::FixtureInvalid)
            );
        }
    }

    #[test]
    fn always_on_observed_contract_rejects_each_missing_action_and_oracle() {
        let manifest = CampaignManifest::bundled().unwrap();
        let definition = manifest
            .probe("always-on-grokbot-lifecycle-v1")
            .expect("manifest always-on probe");
        let mut complete = ProbeBuilder::new(definition);
        for action in &definition.actions {
            complete.observe_action(*action);
        }
        for oracle in &definition.oracle_codes {
            complete.observe_oracle(*oracle);
        }
        assert!(assert_observed_contract(&complete).is_ok());
        let unobserved = ProbeBuilder::new(definition);
        let finished = unobserved.finish(ProbeStatus::Passed, DiagnosticCode::Ok);
        assert!(finished.result.verified_actions.is_empty());
        assert!(finished.result.verified_oracles.is_empty());
        for missing in &definition.actions {
            let mut candidate = ProbeBuilder::new(definition);
            for action in &definition.actions {
                if action != missing {
                    candidate.observe_action(*action);
                }
            }
            for oracle in &definition.oracle_codes {
                candidate.observe_oracle(*oracle);
            }
            assert_eq!(
                assert_observed_contract(&candidate),
                Err(DiagnosticCode::OracleMismatch),
                "missing action {missing:?} must fail"
            );
        }
        for missing in &definition.oracle_codes {
            let mut candidate = ProbeBuilder::new(definition);
            for action in &definition.actions {
                candidate.observe_action(*action);
            }
            for oracle in &definition.oracle_codes {
                if oracle != missing {
                    candidate.observe_oracle(*oracle);
                }
            }
            assert_eq!(
                assert_observed_contract(&candidate),
                Err(DiagnosticCode::OracleMismatch),
                "missing oracle {missing:?} must fail"
            );
        }
    }

    #[test]
    fn always_on_cardinality_comparison_rejects_each_growth_dimension() {
        let expected = AlwaysOnCardinality {
            work: 5,
            runs: 5,
            intents: 4,
        };
        for mutant in [
            AlwaysOnCardinality {
                work: 6,
                ..expected
            },
            AlwaysOnCardinality {
                runs: 6,
                ..expected
            },
            AlwaysOnCardinality {
                intents: 5,
                ..expected
            },
        ] {
            assert_eq!(
                assert_exact_cardinality(expected, mutant),
                Err(DiagnosticCode::StateTransitionMismatch)
            );
        }
    }

    #[test]
    fn always_on_unique_join_rejects_each_identity_and_cardinality_mutant() {
        let work = json!({
            "work": [{
                "workId": "work-a",
                "sourceManagerStepId": "step-a",
                "state": "failed"
            }]
        });
        let detailed = json!({
            "work": {"workId": "work-a"},
            "attempts": [{
                "attemptId": "attempt-a",
                "linkedRunIds": ["run-a"]
            }]
        });
        let intents = json!({
            "intents": [{
                "intentId": "intent-a",
                "workId": "work-a",
                "attemptId": "attempt-a",
                "runId": "run-a",
                "inputHash": "opaque-input",
                "workRevision": 1,
                "agentSpecRevision": 1
            }]
        });
        let runs = json!({
            "runs": [{
                "runId": "run-a",
                "requestId": "intent-a",
                "state": "interrupted"
            }]
        });
        assert!(always_on_require_unique_join(
            &work, &detailed, &intents, &runs, "work-a", "run-a"
        )
        .is_ok());
        let mut mutants = Vec::new();
        let mut value = work.clone();
        value["work"][0]["workId"] = json!("work-other");
        mutants.push((value, detailed.clone(), intents.clone(), runs.clone()));
        let mut value = work.clone();
        value["work"]
            .as_array_mut()
            .unwrap()
            .push(work["work"][0].clone());
        mutants.push((value, detailed.clone(), intents.clone(), runs.clone()));
        let mut value = detailed.clone();
        value["work"]["workId"] = json!("work-other");
        mutants.push((work.clone(), value, intents.clone(), runs.clone()));
        let mut value = detailed.clone();
        value["attempts"]
            .as_array_mut()
            .unwrap()
            .push(detailed["attempts"][0].clone());
        mutants.push((work.clone(), value, intents.clone(), runs.clone()));
        let mut value = detailed.clone();
        value["attempts"][0]["attemptId"] = json!("attempt-other");
        mutants.push((work.clone(), value, intents.clone(), runs.clone()));
        let mut value = detailed.clone();
        value["attempts"][0]["linkedRunIds"] = json!(["run-a", "run-other"]);
        mutants.push((work.clone(), value, intents.clone(), runs.clone()));
        let mut value = detailed.clone();
        value["attempts"][0]["linkedRunIds"] = json!(["run-other"]);
        mutants.push((work.clone(), value, intents.clone(), runs.clone()));
        let mut value = intents.clone();
        value["intents"]
            .as_array_mut()
            .unwrap()
            .push(intents["intents"][0].clone());
        mutants.push((work.clone(), detailed.clone(), value, runs.clone()));
        let mut value = intents.clone();
        value["intents"][0]["runId"] = json!("run-other");
        mutants.push((work.clone(), detailed.clone(), value, runs.clone()));
        let mut value = runs.clone();
        value["runs"]
            .as_array_mut()
            .unwrap()
            .push(runs["runs"][0].clone());
        mutants.push((work.clone(), detailed.clone(), intents.clone(), value));
        let mut value = runs.clone();
        value["runs"][0]["requestId"] = json!("intent-other");
        mutants.push((work.clone(), detailed.clone(), intents.clone(), value));
        for (work, detailed, intents, runs) in mutants {
            assert!(
                always_on_require_unique_join(&work, &detailed, &intents, &runs, "work-a", "run-a")
                    .is_err(),
                "identity/cardinality mutant must fail"
            );
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
