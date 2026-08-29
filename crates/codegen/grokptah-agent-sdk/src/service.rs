//! Service adapter: [`AgentControlPlane`] over the GrokPtah authenticated MCP
//! control plane (`grokptah-service`, and the desktop's embedded control
//! server, which expose the same `ptah_*` tool surface).
//!
//! # No transport in this crate
//!
//! The adapter speaks the domain contract; it does not open sockets. Framing
//! lives behind [`McpTransport`], which an embedder implements over whatever
//! HTTP client it already has. That is not only a dependency choice — it makes
//! "no live-route assumptions" structurally true: this crate has no HTTP
//! client, so no code path here can reach a provider, a gateway, or a network
//! route of any kind. Every test in this crate drives a scripted transport.
//!
//! # What this adapter deliberately does not map
//!
//! The runtime's tool surface is larger than this contract. The adapter maps
//! only tools this contract already declares, and refuses to grow into:
//!
//! | Not mapped | Why |
//! |---|---|
//! | `ptah_create_manager_plan`, `ptah_advance_manager_plan`, `ptah_tick_manager_plan`, `ptah_replan_manager_plan` | Manager plans are an active line (bridge #337–#339, newest commits on `main`). Their schema is at version 1 and still moving. |
//! | `ptah_set_managed_execution`, `ptah_get_managed_execution`, `ptah_authorize_work_execution`, `ptah_resolve_work_input`, `ptah_list_execution_intents` | Managed execution is where a **mutation grant** is issued and recorded. That authority stays host-owned; a consumer of this seam must not be able to issue, replay, or infer one. |
//! | `ptah_approve_run`, `ptah_promote_run`, `ptah_discard_run`, `ptah_review_run` | Operator authority: approving and promoting reviewed code. ADR-002 §5 keeps these operator-equivalent. |
//! | `ptah_*_computer_*` | Computer Use reads are redaction-safe but unmapped in this build; Computer Use *control* is permanently forbidden. |
//! | Queue mutators, routines, workers, messages, work lifecycle beyond claim/release | Outside the declared v1 capability set. |
//!
//! # Read-only by default
//!
//! [`ServiceControlPlane::read_only`] is the only constructor that needs no
//! assertion from the embedder. In that mode every mutating method fails with
//! `forbidden_scope` **before the transport is touched**, and the capability
//! document advertises those capabilities as [`Availability::Forbidden`] so a
//! consumer can grey out the control instead of discovering the refusal on
//! click. Mutations require [`ServiceControlPlane::with_operator_authority`],
//! which is the embedder asserting what ADR-002 §5 already states: possession
//! of a configured service bearer is privileged operator access.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::RwLock;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::capability::{
    Availability, BoundaryLimits, CapabilityDescriptor, CapabilityDocument, CapabilityId,
    HostDescriptor, HostKind,
};
use crate::client::AgentControlPlane;
use crate::dto::*;
use crate::error::{SdkError, SdkErrorCode, SdkResult};
use crate::ids::*;
use crate::page::{Cursor, Page, PageRequest, RetainedRange};
use crate::version::{ContractVersion, CONTRACT_VERSION};

// ── Transport ─────────────────────────────────────────────────────────────

/// A failure that happened below the domain contract.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransportFault {
    /// The host answered with a JSON-RPC error. `code` is `error.data.code`
    /// when the host supplied one; the adapter, not the transport, decides
    /// what it means.
    Rpc {
        code: Option<String>,
        message: String,
        data: Value,
    },
    /// The host could not be reached, or an established connection dropped.
    Unreachable { detail: String },
    /// A response arrived but was not the shape the contract requires.
    Malformed { detail: String },
}

impl TransportFault {
    /// Parse a JSON-RPC `error` member into a fault.
    ///
    /// Provided so every transport extracts `error.data.code` the same way;
    /// the control plane puts its typed code there and merges any extra
    /// diagnostic fields (a `cursor_expired` may carry `eventRange`) into the
    /// same object.
    pub fn from_jsonrpc_error(error: &Value) -> Self {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("control plane returned an error")
            .to_string();
        let data = error.get("data").cloned().unwrap_or(Value::Null);
        let code = data.get("code").and_then(Value::as_str).map(str::to_string);
        Self::Rpc {
            code,
            message,
            data,
        }
    }

    fn into_sdk_error(self) -> SdkError {
        match self {
            Self::Rpc {
                code,
                message,
                data,
            } => {
                let code = code
                    .as_deref()
                    .map(SdkErrorCode::from_wire)
                    // A host error with no typed code is not something to
                    // guess at. `internal` is the conservative reading: the
                    // caller learns the request failed and that retrying the
                    // same idempotency key is safe.
                    .unwrap_or(SdkErrorCode::Internal);
                let mut error = SdkError::new(code, message);
                if let Some(range) = data.get("eventRange").filter(|v| !v.is_null()) {
                    if let Some(start) = range.get("startSeq").and_then(Value::as_u64) {
                        error = error.with_detail("retainedStart", start.to_string());
                    }
                    if let Some(end) = range.get("endSeq").and_then(Value::as_u64) {
                        error = error.with_detail("retainedEnd", end.to_string());
                    }
                }
                error
            }
            Self::Unreachable { detail } => {
                SdkError::new(SdkErrorCode::TransportUnavailable, detail)
            }
            Self::Malformed { detail } => SdkError::new(
                SdkErrorCode::Internal,
                format!("control plane response was malformed: {detail}"),
            ),
        }
    }
}

/// One authenticated MCP control-plane connection.
///
/// Implementors own framing, authentication, timeouts, and body limits. They
/// must return the `structuredContent` body of a successful `tools/call`, and
/// a [`TransportFault`] for anything else.
#[async_trait]
pub trait McpTransport: Send + Sync {
    /// `tools/list` — the host-owned tool registry. The adapter derives its
    /// capability document from this and never asserts a tool the host did
    /// not advertise.
    async fn list_tools(&self) -> Result<Vec<String>, TransportFault>;

    /// `tools/call` — returns the `structuredContent` body.
    async fn call_tool(&self, tool: &str, arguments: Value) -> Result<Value, TransportFault>;
}

// ── Host-owned workspace registry ─────────────────────────────────────────

/// Maps adapter-issued [`WorkspaceRef`]s to the canonical workspace identities
/// the control plane authorizes against.
///
/// The registry is **learned, never declared**: entries appear only when the
/// host reports a workspace in a `ptah_list_sessions` or `ptah_create_session`
/// response. A consumer cannot mint a ref, and a ref the host has not reported
/// resolves to [`SdkErrorCode::WorkspaceMismatch`] without a round trip —
/// matching the runtime, where the allowlist gate is session-independent and
/// precedes every scope check.
///
/// # Ref opacity
///
/// A ref is `ws-` plus 16 hex characters of `SHA-256(key ‖ 0x00 ‖ path)`. With
/// the default key this obfuscates the path but does not hide it: workspace
/// paths are low-entropy, so an attacker who can guess a path can confirm it
/// against a ref. Embedders that need real opacity pass a persistent secret
/// with [`WorkspaceRegistry::with_ref_key`]. The durable fix is host-issued
/// refs, which the control plane does not yet provide — see
/// `docs/AGENT_SDK_SEAM.md`.
#[derive(Debug)]
pub struct WorkspaceRegistry {
    key: Vec<u8>,
    by_ref: BTreeMap<String, String>,
}

/// Key used when an embedder does not supply one. Not a secret.
const DEFAULT_REF_KEY: &[u8] = b"grokptah-agent-sdk/workspace-ref/v1";

impl Default for WorkspaceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkspaceRegistry {
    pub fn new() -> Self {
        Self::with_ref_key(DEFAULT_REF_KEY)
    }

    /// Derive refs under a caller-supplied key. Use a persistent secret when
    /// refs must not be confirmable against guessed paths.
    pub fn with_ref_key(key: impl AsRef<[u8]>) -> Self {
        Self {
            key: key.as_ref().to_vec(),
            by_ref: BTreeMap::new(),
        }
    }

    fn mint(&self, canonical_path: &str) -> SdkResult<WorkspaceRef> {
        let mut hasher = Sha256::new();
        hasher.update(&self.key);
        hasher.update([0u8]);
        hasher.update(canonical_path.as_bytes());
        let digest = hasher.finalize();
        let mut hex = String::with_capacity(16);
        for byte in digest.iter().take(8) {
            hex.push(char::from_digit((byte >> 4) as u32, 16).expect("nibble is hex"));
            hex.push(char::from_digit((byte & 0x0f) as u32, 16).expect("nibble is hex"));
        }
        WorkspaceRef::new(format!("ws-{hex}"))
    }

    /// Record a workspace the host reported and return its ref.
    fn learn(&mut self, canonical_path: &str) -> SdkResult<WorkspaceRef> {
        let path = canonical_path.trim();
        if path.is_empty() {
            return Err(SdkError::new(
                SdkErrorCode::Internal,
                "host reported a session with an empty workspace",
            ));
        }
        let reference = self.mint(path)?;
        self.by_ref
            .insert(reference.as_str().to_string(), path.to_string());
        Ok(reference)
    }

    fn resolve(&self, reference: &WorkspaceRef) -> SdkResult<String> {
        self.by_ref.get(reference.as_str()).cloned().ok_or_else(|| {
            SdkError::new(
                SdkErrorCode::WorkspaceMismatch,
                "workspace is not in this host's allowlist",
            )
        })
    }

    /// How many workspaces the host has reported so far.
    pub fn len(&self) -> usize {
        self.by_ref.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_ref.is_empty()
    }
}

// ── Mutation authority ────────────────────────────────────────────────────

/// Whether this adapter may mutate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationAuthority {
    /// Default. Reads only; every mutation is refused locally.
    ///
    /// This realizes ADR-002 §5's read-only observer tier at the seam. The
    /// control plane itself does not yet offer a narrower-than-operator
    /// credential, so the restriction is enforced here rather than negotiated
    /// with the host — which means it protects against consumer mistakes, not
    /// against a malicious consumer holding the same bearer.
    Observer,
    /// The embedder asserts this credential is operator-equivalent.
    ///
    /// ADR-002 §5: "Deployments must treat possession of any configured bearer
    /// as privileged operator access." Nothing here widens that; it only stops
    /// pretending the adapter is read-only when the caller intends to mutate.
    OperatorEquivalent,
}

impl MutationAuthority {
    fn require(self, what: &str) -> SdkResult<()> {
        match self {
            Self::OperatorEquivalent => Ok(()),
            Self::Observer => Err(SdkError::new(
                SdkErrorCode::ForbiddenScope,
                format!(
                    "{what} is a mutation and this adapter is read-only; \
                     construct it with operator-equivalent authority to mutate"
                ),
            )
            .with_detail("mutationAuthority", "observer")),
        }
    }
}

/// Non-secret identity of the host this adapter is pointed at.
///
/// The control plane exposes no version tool, so the embedder supplies this
/// from whatever it used to connect. It is advertised verbatim in the
/// capability document and is useful only for correlating a bug report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceHostInfo {
    pub product: Label,
    pub host_version: Label,
}

impl Default for ServiceHostInfo {
    fn default() -> Self {
        Self {
            product: Label::new("GrokPtah").expect("static label"),
            host_version: Label::new("unknown").expect("static label"),
        }
    }
}

// ── The adapter ───────────────────────────────────────────────────────────

/// Tool names this adapter calls. Nothing outside this list is ever invoked.
mod tools {
    pub const LIST_SESSIONS: &str = "ptah_list_sessions";
    pub const CREATE_SESSION: &str = "ptah_create_session";
    pub const SUBMIT_TASK: &str = "ptah_submit_task";
    pub const GET_RUN: &str = "ptah_get_run";
    pub const GET_EVENTS: &str = "ptah_get_events";
    pub const GET_TEST_RESULTS: &str = "ptah_get_test_results";
    pub const STEER: &str = "ptah_steer";
    pub const CANCEL: &str = "ptah_cancel";
    pub const CLAIM_WORK: &str = "ptah_claim_work";
    pub const RELEASE_WORK: &str = "ptah_release_work";

    pub const ALL: &[&str] = &[
        LIST_SESSIONS,
        CREATE_SESSION,
        SUBMIT_TASK,
        GET_RUN,
        GET_EVENTS,
        GET_TEST_RESULTS,
        STEER,
        CANCEL,
        CLAIM_WORK,
        RELEASE_WORK,
    ];
}

/// [`AgentControlPlane`] over the authenticated MCP control plane.
#[derive(Debug)]
pub struct ServiceControlPlane<T: McpTransport> {
    transport: T,
    registry: RwLock<WorkspaceRegistry>,
    authority: MutationAuthority,
    host: ServiceHostInfo,
    limits: BoundaryLimits,
}

impl<T: McpTransport> ServiceControlPlane<T> {
    /// Read-only adapter. Mutations are refused before the transport is used.
    pub fn read_only(transport: T) -> Self {
        Self {
            transport,
            registry: RwLock::new(WorkspaceRegistry::new()),
            authority: MutationAuthority::Observer,
            host: ServiceHostInfo::default(),
            limits: BoundaryLimits::default(),
        }
    }

    /// Allow mutations. See [`MutationAuthority::OperatorEquivalent`].
    pub fn with_operator_authority(mut self) -> Self {
        self.authority = MutationAuthority::OperatorEquivalent;
        self
    }

    pub fn with_host_info(mut self, host: ServiceHostInfo) -> Self {
        self.host = host;
        self
    }

    /// Derive workspace refs under a persistent secret key.
    pub fn with_ref_key(self, key: impl AsRef<[u8]>) -> Self {
        *self.registry.write().unwrap_or_else(|e| e.into_inner()) =
            WorkspaceRegistry::with_ref_key(key);
        self
    }

    pub fn authority(&self) -> MutationAuthority {
        self.authority
    }

    /// Borrow the transport, for embedders that need to inspect or drive it.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// How many workspaces the host has reported. Zero until the first
    /// `list_sessions` or `create_session`.
    pub fn known_workspaces(&self) -> usize {
        self.registry
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    fn resolve_workspace(&self, reference: &WorkspaceRef) -> SdkResult<String> {
        self.registry
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .resolve(reference)
    }

    fn learn_workspace(&self, canonical_path: &str) -> SdkResult<WorkspaceRef> {
        self.registry
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .learn(canonical_path)
    }

    async fn call(&self, tool: &str, arguments: Value) -> SdkResult<Value> {
        debug_assert!(
            tools::ALL.contains(&tool),
            "adapter called an unmapped tool: {tool}"
        );
        self.transport
            .call_tool(tool, arguments)
            .await
            .map_err(TransportFault::into_sdk_error)
    }

    /// Exact scope binding shared by every run-scoped read.
    fn run_scope(&self, selector: &RunSelector) -> SdkResult<Value> {
        Ok(json!({
            "session_id": selector.session_id.as_str(),
            "workspace": self.resolve_workspace(&selector.workspace)?,
            "run_id": selector.run_id.as_str(),
        }))
    }
}

// ── Wire helpers ──────────────────────────────────────────────────────────

fn malformed(what: &str) -> SdkError {
    SdkError::new(
        SdkErrorCode::Internal,
        format!("control plane response is missing or malformed: {what}"),
    )
}

/// Read a field under either spelling.
///
/// The control plane is camelCase at the envelope level (`nextCursor`,
/// `queuedPosition`) but `SessionUpdate` carries plain serde field names,
/// which are snake_case (`src/events.rs`: the enum sets `rename_all` for
/// variants only). Accepting both keeps one projection correct across the two
/// conventions and survives a host that later normalizes them.
fn field<'a>(object: &'a Value, snake: &str, camel: &str) -> Option<&'a Value> {
    object
        .get(snake)
        .or_else(|| object.get(camel))
        .filter(|value| !value.is_null())
}

fn str_field(object: &Value, snake: &str, camel: &str) -> Option<String> {
    field(object, snake, camel)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn u64_field(object: &Value, snake: &str, camel: &str) -> Option<u64> {
    field(object, snake, camel).and_then(Value::as_u64)
}

fn u32_field(object: &Value, snake: &str, camel: &str) -> Option<u32> {
    u64_field(object, snake, camel).map(|value| value.min(u32::MAX as u64) as u32)
}

fn bool_field(object: &Value, snake: &str, camel: &str) -> bool {
    field(object, snake, camel)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn timestamp(object: &Value, snake: &str, camel: &str) -> Option<DateTime<Utc>> {
    str_field(object, snake, camel)
        .and_then(|raw| DateTime::parse_from_rfc3339(&raw).ok())
        .map(|value| value.with_timezone(&Utc))
}

/// Derive a monotonic revision from a durable timestamp.
///
/// The runtime's Run record carries no revision field, so `updatedAt` in epoch
/// milliseconds is the only monotonic non-decreasing quantity available. Two
/// commits inside the same millisecond collapse to one value, which a
/// [`RevisionWatermark`] then treats as stale — conservative, but it can drop
/// a real update. A host-side monotonic run revision closes this; see
/// `docs/AGENT_SDK_SEAM.md`.
fn revision_from(at: Option<DateTime<Utc>>) -> Revision {
    Revision::new(
        at.map(|value| value.timestamp_millis().max(0) as u64)
            .unwrap_or(0),
    )
}

fn lifecycle_from(raw: &str) -> SdkResult<RunLifecycle> {
    serde_json::from_value(Value::String(raw.to_string()))
        .map_err(|_| malformed(&format!("unknown run state {raw:?}")))
}

fn stop_cause_from(raw: &str) -> Option<StopCause> {
    serde_json::from_value(Value::String(raw.to_string())).ok()
}

fn tool_kind_from(raw: &str) -> ToolKind {
    serde_json::from_value(Value::String(raw.to_string())).unwrap_or(ToolKind::Other)
}

fn tool_status_from(raw: &str) -> Option<ToolStatus> {
    serde_json::from_value(Value::String(raw.to_string())).ok()
}

/// Project the durable Run record onto the public view.
///
/// This is the redaction boundary. `ptah_get_run` returns the **complete**
/// durable `RunRecord`, which includes `promptPreview`, `finalResponse`, the
/// absolute `workspace` path, the originating `requestId`, and `clientId`.
/// None of them survive this function, because none of them exist on
/// [`RunView`].
fn project_run(body: &Value, selector: &RunSelector) -> SdkResult<RunView> {
    let state = str_field(body, "state", "state").ok_or_else(|| malformed("run.state"))?;
    let lifecycle = lifecycle_from(&state)?;
    let updated_at = timestamp(body, "updated_at", "updatedAt");
    let created_at = timestamp(body, "created_at", "createdAt").or(updated_at);

    let bounds_raw = field(body, "bounds", "bounds")
        .cloned()
        .unwrap_or(Value::Null);
    let bounds = AppliedBounds {
        max_prompt_bytes: u64_field(&bounds_raw, "max_prompt_bytes", "maxPromptBytes").unwrap_or(0),
        max_rounds: u32_field(&bounds_raw, "max_rounds", "maxRounds").unwrap_or(0),
        max_duration_ms: u64_field(&bounds_raw, "max_duration_ms", "maxDurationMs").unwrap_or(0),
        max_total_tokens: u64_field(&bounds_raw, "max_total_tokens", "maxTotalTokens"),
    };

    let aggregates = field(body, "aggregates", "aggregates")
        .cloned()
        .unwrap_or(Value::Null);
    let usage_raw = field(&aggregates, "usage", "usage")
        .cloned()
        .unwrap_or(Value::Null);
    let usage = UsageView {
        prompt_tokens: u64_field(&usage_raw, "prompt_tokens", "promptTokens").unwrap_or(0),
        completion_tokens: u64_field(&usage_raw, "completion_tokens", "completionTokens")
            .unwrap_or(0),
        total_tokens: u64_field(&usage_raw, "total_tokens", "totalTokens").unwrap_or(0),
        requests: u64_field(&usage_raw, "requests", "requests").unwrap_or(0),
        complete: bool_field(&aggregates, "usage_complete", "usageComplete"),
        pending_requests: u32_field(
            &aggregates,
            "usage_pending_requests",
            "usagePendingRequests",
        )
        .unwrap_or(0),
    };

    // Changed files: workspace-relative, traversal-checked. A path the host
    // reports as absolute is dropped rather than surfaced, so the seam cannot
    // become a host-layout oracle even if an upstream tool records one.
    let mut changed_files = Vec::new();
    if let Some(changes) = field(&aggregates, "changes", "changes").and_then(Value::as_array) {
        for change in changes {
            let Some(raw) = str_field(change, "path", "path") else {
                continue;
            };
            let Ok(path) = RelativePath::new(&raw) else {
                continue;
            };
            changed_files.push(ChangedFile {
                path,
                summary: BoundedText::new(
                    str_field(change, "summary", "summary").unwrap_or_default(),
                ),
            });
        }
    }

    let verification = field(&aggregates, "verification", "verification").and_then(|raw| {
        let status = str_field(raw, "status", "status")?;
        let status = serde_json::from_value::<VerificationStatus>(Value::String(status)).ok()?;
        let observations_raw = field(raw, "observations", "observations")
            .cloned()
            .unwrap_or(Value::Null);
        let count =
            |snake: &str, camel: &str| u32_field(&observations_raw, snake, camel).unwrap_or(0);
        Some(VerificationView {
            status,
            stop_cause: str_field(raw, "stop_reason", "stopReason")
                .as_deref()
                .and_then(stop_cause_from)
                .unwrap_or(StopCause::Completed),
            interrupted: bool_field(raw, "interrupted", "interrupted"),
            observations: ObservationCounts {
                changed_files: count("changed_files", "changedFiles"),
                tests_observed: count("tests_observed", "testsObserved"),
                tests_passed: count("tests_passed", "testsPassed"),
                tests_failed: count("tests_failed", "testsFailed"),
                tests_incomplete: count("tests_incomplete", "testsIncomplete"),
                permissions_requested: count("permissions_requested", "permissionsRequested"),
                permissions_granted: count("permissions_granted", "permissionsGranted"),
                permissions_denied: count("permissions_denied", "permissionsDenied"),
                permissions_unresolved: count("permissions_unresolved", "permissionsUnresolved"),
            },
            usage,
        })
    });

    let progress = field(body, "progress", "progress").and_then(|raw| {
        Some(RunProgressView {
            round: u32_field(raw, "round", "round")?,
            max_rounds: u32_field(raw, "max_rounds", "maxRounds").unwrap_or(bounds.max_rounds),
            last_tool: str_field(raw, "last_tool", "lastTool")
                .and_then(|value| Label::new(value).ok()),
            updated_at: timestamp(raw, "updated_at", "updatedAt").unwrap_or_else(|| {
                updated_at.unwrap_or_else(|| DateTime::from_timestamp(0, 0).expect("epoch"))
            }),
        })
    });

    let epoch = DateTime::from_timestamp(0, 0).expect("epoch is a valid timestamp");
    Ok(RunView {
        run_id: selector.run_id.clone(),
        session_id: selector.session_id.clone(),
        workspace: selector.workspace.clone(),
        lifecycle,
        stop_cause: str_field(body, "stop_cause", "stopCause")
            .as_deref()
            .and_then(stop_cause_from),
        revision: revision_from(updated_at),
        execution_mode: str_field(body, "execution_mode", "executionMode")
            .and_then(|raw| serde_json::from_value(Value::String(raw)).ok())
            .unwrap_or(ExecutionMode::Shared),
        queue_position: u32_field(body, "queue_position", "queuePosition"),
        bounds,
        usage,
        progress,
        changed_files,
        // Artifacts are advertised, not embedded: the durable record does not
        // carry them, and a descriptor without a verified body would be a
        // promise this adapter cannot keep. `fetch_artifact` builds both.
        artifacts: Vec::new(),
        verification,
        terminal_marker: str_field(body, "error_code", "errorCode")
            .and_then(|value| Label::new(value).ok()),
        // The durable record already carries the readable journal window.
        // `startSeq` is the first event, so the resumable cursor is one below.
        event_range: u64_field(body, "start_seq", "startSeq").map(|start| RetainedRange {
            start: Cursor::from_opaque(start.saturating_sub(1).to_string()),
            end: Cursor::from_opaque(
                u64_field(body, "end_seq", "endSeq")
                    .unwrap_or(start)
                    .to_string(),
            ),
        }),
        created_at: created_at.unwrap_or(epoch),
        updated_at: updated_at.unwrap_or(epoch),
    })
}

/// Project one journal entry onto a bounded public event.
///
/// Returns `None` for entries this contract does not carry: message and
/// thought chunks, shell output, subagent chatter, plans, and tool-call
/// *output*. Those are transcript. The caller still advances its cursor past
/// them, so dropping them cannot stall paging.
fn project_event(entry: &Value) -> Option<PublicEvent> {
    let seq = u64_field(entry, "seq", "seq")?;
    let at = timestamp(entry, "ts", "ts")
        .unwrap_or_else(|| DateTime::from_timestamp(0, 0).expect("epoch is a valid timestamp"));
    let update = field(entry, "update", "update")?;
    let kind_tag = str_field(update, "type", "type")?;

    let kind = match kind_tag.as_str() {
        "turn_started" => PublicEventKind::TurnStarted,
        "agent_progress" => PublicEventKind::Progress {
            round: u32_field(update, "round", "round").unwrap_or(0),
            max_rounds: u32_field(update, "max_rounds", "maxRounds").unwrap_or(0),
            last_tool: str_field(update, "last_tool", "lastTool")
                .and_then(|value| Label::new(value).ok()),
        },
        "tool_call" => PublicEventKind::ToolCall {
            call_id: str_field(update, "call_id", "callId")
                .and_then(|value| Label::new(value).ok())?,
            tool: str_field(update, "kind", "kind")
                .as_deref()
                .map(tool_kind_from)
                .unwrap_or(ToolKind::Other),
            status: str_field(update, "status", "status")
                .as_deref()
                .and_then(tool_status_from)?,
        },
        "file_edit" => PublicEventKind::FileChanged {
            // `unified_diff` is deliberately not read.
            path: str_field(update, "path", "path")
                .and_then(|value| RelativePath::new(value).ok())?,
            summary: BoundedText::new(str_field(update, "summary", "summary").unwrap_or_default()),
        },
        "permission_required" => PublicEventKind::Permission {
            outcome: PermissionOutcome::Requested,
            tool: ToolKind::Other,
        },
        "rate_limited" => PublicEventKind::RateLimited {
            retry_after_ms: u64_field(update, "retry_after_ms", "retryAfterMs"),
        },
        "steering_injected" => PublicEventKind::FollowUpAccepted {
            disposition: FollowUpDisposition::Pending,
        },
        "prompt_queue_changed" => PublicEventKind::QueueChanged {
            revision: Revision::new(u64_field(update, "revision", "revision").unwrap_or(0)),
            entry_count: field(update, "entries", "entries")
                .and_then(Value::as_array)
                .map(|entries| entries.len().min(u32::MAX as usize) as u32)
                .unwrap_or(0),
        },
        // Transcript and internal chatter.
        "agent_message_chunk"
        | "agent_thought_chunk"
        | "tool_call_update"
        | "plan"
        | "turn_complete"
        | "completion_evidence"
        | "error"
        | "subagent_spawned"
        | "subagent_update"
        | "background_task"
        | "shell_session_started"
        | "shell_output"
        | "shell_session_ended" => return None,
        other => PublicEventKind::Unrecognized {
            wire_kind: Label::new(other).ok()?,
        },
    };

    Some(PublicEvent {
        cursor: Cursor::from_opaque(seq.to_string()),
        at,
        kind,
    })
}

fn insert_optional(map: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        map.insert(key.to_string(), value);
    }
}

// ── AgentControlPlane ─────────────────────────────────────────────────────

#[async_trait]
impl<T: McpTransport> AgentControlPlane for ServiceControlPlane<T> {
    async fn capabilities(&self) -> SdkResult<CapabilityDocument> {
        let advertised: BTreeSet<String> = self
            .transport
            .list_tools()
            .await
            .map_err(TransportFault::into_sdk_error)?
            .into_iter()
            .collect();

        let since = ContractVersion::new(CONTRACT_VERSION.major, 0);
        let mutating = self.authority == MutationAuthority::OperatorEquivalent;
        let read_only_reason =
            Label::new("adapter is read-only; construct it with operator-equivalent authority")
                .expect("static label");

        let mut offered = Vec::new();
        let mut declare = |id: CapabilityId, needs: &[&str], is_mutation: bool| {
            let missing: Vec<&str> = needs
                .iter()
                .copied()
                .filter(|tool| !advertised.contains(*tool))
                .collect();
            let availability = if !missing.is_empty() {
                Availability::Unsupported {
                    reason: Label::new(format!("host does not advertise {}", missing.join(", ")))
                        .unwrap_or_else(|_| {
                            Label::new("host does not advertise the required tools")
                                .expect("static label")
                        }),
                }
            } else if is_mutation && !mutating {
                Availability::Forbidden {
                    reason: read_only_reason.clone(),
                }
            } else {
                Availability::Available
            };
            offered.push(CapabilityDescriptor {
                id,
                since,
                availability,
            });
        };

        declare(CapabilityId::SessionList, &[tools::LIST_SESSIONS], false);
        declare(CapabilityId::RunObserve, &[tools::GET_RUN], false);
        declare(CapabilityId::RunEventsPage, &[tools::GET_EVENTS], false);
        declare(
            CapabilityId::ArtifactFetch,
            &[tools::GET_TEST_RESULTS],
            false,
        );
        declare(CapabilityId::SessionCreate, &[tools::CREATE_SESSION], true);
        declare(CapabilityId::TaskSubmit, &[tools::SUBMIT_TASK], true);
        declare(CapabilityId::RunFollowUp, &[tools::STEER], true);
        declare(CapabilityId::RunCancel, &[tools::CANCEL], true);
        declare(
            CapabilityId::ControlLease,
            &[tools::CLAIM_WORK, tools::RELEASE_WORK],
            true,
        );

        // The durable receipts exist on the host, but no tool reads them.
        offered.push(CapabilityDescriptor {
            id: CapabilityId::ReceiptRead,
            since: ContractVersion::new(CONTRACT_VERSION.major, 1),
            availability: Availability::Unsupported {
                reason: Label::new("the control plane exposes no receipt read tool")
                    .expect("static label"),
            },
        });

        // Read-only Computer Run projections exist on the host but are not
        // mapped by this adapter build. Saying so is better than silence: a
        // consumer can tell "not built" from "host cannot".
        offered.push(CapabilityDescriptor {
            id: CapabilityId::ComputerRead,
            since,
            availability: Availability::Unsupported {
                reason: Label::new("Computer Run reads are not mapped by the service adapter")
                    .expect("static label"),
            },
        });

        Ok(CapabilityDocument::new(
            HostDescriptor {
                kind: HostKind::Service,
                product: self.host.product.clone(),
                host_version: self.host.host_version.clone(),
            },
            Utc::now(),
            self.limits,
            offered,
        ))
    }

    /// Create a Build session on an already-reported workspace.
    ///
    /// `ptah_create_session` accepts no `request_id`, so this mutation is **not
    /// idempotent at the host**. `CreateSessionRequest::request_id` is
    /// therefore not transmitted; a retry after a timeout can create a second
    /// session. Callers that need at-most-once creation should list sessions
    /// and reconcile.
    async fn create_session(&self, request: CreateSessionRequest) -> SdkResult<SessionView> {
        self.authority.require("create_session")?;
        let workspace = self.resolve_workspace(&request.workspace)?;

        let mut args = Map::new();
        args.insert("workspace".into(), Value::String(workspace));
        insert_optional(
            &mut args,
            "title",
            request
                .title
                .as_ref()
                .map(|title| Value::String(title.as_str().to_string())),
        );

        let body = self
            .call(tools::CREATE_SESSION, Value::Object(args))
            .await?;
        let reported = str_field(&body, "workspace", "workspace")
            .ok_or_else(|| malformed("createSession.workspace"))?;
        let workspace = self.learn_workspace(&reported)?;
        let updated_at = timestamp(&body, "updated_at", "updatedAt");

        Ok(SessionView {
            session_id: SessionId::new(
                str_field(&body, "session_id", "sessionId")
                    .ok_or_else(|| malformed("createSession.sessionId"))?,
            )?,
            workspace,
            kind: SessionKind::Build,
            title: str_field(&body, "title", "title").and_then(|value| Label::new(value).ok()),
            revision: revision_from(updated_at),
            created_at: updated_at
                .unwrap_or_else(|| DateTime::from_timestamp(0, 0).expect("epoch")),
        })
    }

    /// List the Build sessions this credential may address.
    ///
    /// `ptah_list_sessions` returns the whole allowlisted set in one response,
    /// so paging is applied here rather than by the host. The cursor is the
    /// last session id of the page.
    async fn list_sessions(&self, page: PageRequest) -> SdkResult<Page<SessionView>> {
        let limit = page.resolve_limit(self.limits.max_event_page)? as usize;
        let after = page
            .after
            .as_ref()
            .map(|cursor| cursor.as_str().to_string());

        let body = self.call(tools::LIST_SESSIONS, json!({})).await?;
        let rows = field(&body, "sessions", "sessions")
            .and_then(Value::as_array)
            .ok_or_else(|| malformed("listSessions.sessions"))?;

        let mut items = Vec::new();
        for row in rows {
            let Some(cwd) = str_field(row, "cwd", "cwd") else {
                continue;
            };
            let workspace = self.learn_workspace(&cwd)?;
            let Some(raw_id) = str_field(row, "session_id", "sessionId") else {
                continue;
            };
            if after
                .as_deref()
                .is_some_and(|after| raw_id.as_str() <= after)
            {
                continue;
            }
            let updated_at = timestamp(row, "updated_at", "updatedAt");
            items.push(SessionView {
                session_id: SessionId::new(raw_id)?,
                workspace,
                kind: SessionKind::Build,
                title: str_field(row, "title", "title").and_then(|value| Label::new(value).ok()),
                revision: revision_from(updated_at),
                created_at: updated_at
                    .unwrap_or_else(|| DateTime::from_timestamp(0, 0).expect("epoch")),
            });
        }
        items.sort_by(|a, b| a.session_id.cmp(&b.session_id));

        let has_more = items.len() > limit;
        items.truncate(limit);
        let next_cursor = has_more
            .then(|| {
                items
                    .last()
                    .map(|session| Cursor::from_opaque(session.session_id.as_str()))
            })
            .flatten();
        Ok(Page::new(items, next_cursor))
    }

    async fn submit_task(&self, request: TaskSubmission) -> SdkResult<RunAccepted> {
        self.authority.require("submit_task")?;
        let workspace = self.resolve_workspace(&request.workspace)?;

        let mut args = Map::new();
        args.insert(
            "request_id".into(),
            Value::String(request.request_id.as_str().to_string()),
        );
        args.insert(
            "session_id".into(),
            Value::String(request.session_id.as_str().to_string()),
        );
        args.insert("workspace".into(), Value::String(workspace));
        args.insert("prompt".into(), Value::String(request.prompt.clone()));
        args.insert(
            "execution_mode".into(),
            serde_json::to_value(request.execution_mode)
                .map_err(|error| SdkError::new(SdkErrorCode::Internal, error.to_string()))?,
        );
        args.insert("allow_queue".into(), Value::Bool(request.allow_queue));
        if let Some(bounds) = request.bounds {
            let mut narrowed = Map::new();
            insert_optional(
                &mut narrowed,
                "maxRounds",
                bounds.max_rounds.map(|value| json!(value)),
            );
            insert_optional(
                &mut narrowed,
                "maxDurationMs",
                bounds.max_duration_ms.map(|value| json!(value)),
            );
            // `maxTotalTokens` is accepted by the runtime's bounds merge but is
            // absent from the tool's advertised inputSchema. Sent only when the
            // caller asked for it, so a schema-validating transport rejects it
            // loudly rather than the adapter silently dropping a ceiling.
            insert_optional(
                &mut narrowed,
                "maxTotalTokens",
                bounds.max_total_tokens.map(|value| json!(value)),
            );
            if !narrowed.is_empty() {
                args.insert("bounds".into(), Value::Object(narrowed));
            }
        }

        let body = self.call(tools::SUBMIT_TASK, Value::Object(args)).await?;
        let state = str_field(&body, "state", "state").ok_or_else(|| malformed("submit.state"))?;
        Ok(RunAccepted {
            run_id: RunId::new(
                str_field(&body, "run_id", "runId").ok_or_else(|| malformed("submit.runId"))?,
            )?,
            lifecycle: lifecycle_from(&state)?,
            queue_position: u32_field(&body, "queued_position", "queuedPosition"),
            revision: Revision::new(0),
            // The host replays a stored receipt byte-for-byte, so a replayed
            // response is indistinguishable from a fresh one on the wire.
            // Reporting `false` would be a claim this adapter cannot support.
            replayed: None,
        })
    }

    async fn observe_run(&self, selector: RunSelector) -> SdkResult<RunView> {
        let body = self
            .call(tools::GET_RUN, self.run_scope(&selector)?)
            .await?;
        project_run(&body, &selector)
    }

    async fn stream_events(
        &self,
        selector: RunSelector,
        page: PageRequest,
    ) -> SdkResult<Page<PublicEvent>> {
        let limit = page.resolve_limit(self.limits.max_event_page)?;
        let after = page
            .after
            .as_ref()
            .map(|cursor| {
                cursor.as_str().parse::<u64>().map_err(|_| {
                    SdkError::new(
                        SdkErrorCode::InvalidRequest,
                        "cursor was not issued by this adapter",
                    )
                })
            })
            .transpose()?
            .unwrap_or(0);

        let mut args = self.run_scope(&selector)?;
        let object = args.as_object_mut().expect("run_scope builds an object");
        object.insert("after_seq".into(), json!(after));
        object.insert("limit".into(), json!(limit));

        let body = self.call(tools::GET_EVENTS, args).await?;
        if bool_field(&body, "cursor_expired", "cursorExpired") {
            return Err(SdkError::new(
                SdkErrorCode::CursorExpired,
                "cursor is below the retained window; restart from the beginning",
            ));
        }
        let entries = field(&body, "entries", "entries")
            .and_then(Value::as_array)
            .ok_or_else(|| malformed("getEvents.entries"))?;

        // The cursor advances over every raw entry, including the ones this
        // contract drops. Deriving it from the projected list instead would
        // stall paging on a page that is entirely transcript.
        let last_raw_seq = entries
            .last()
            .and_then(|entry| u64_field(entry, "seq", "seq"));
        let items: Vec<PublicEvent> = entries.iter().filter_map(project_event).collect();
        // The host reports a cursor whenever the page had entries, including a
        // short one: a live run keeps producing, so "short" does not mean
        // "caught up". Passing it through unchanged costs one empty page at the
        // end of a terminal run and never loses a consumer's position.
        let next_cursor = u64_field(&body, "next_cursor", "nextCursor")
            .or(last_raw_seq)
            .map(|seq| Cursor::from_opaque(seq.to_string()));

        Ok(Page::new(items, next_cursor))
    }

    /// Send a non-cancelling follow-up.
    ///
    /// `ptah_steer` takes no compare-and-set fence, so a
    /// [`FollowUpRequest::expected_revision`] cannot be honored. Rather than
    /// drop it — which would leave a caller believing it had fenced — this
    /// returns [`SdkErrorCode::Unsupported`].
    async fn request_follow_up(&self, request: FollowUpRequest) -> SdkResult<FollowUpReceipt> {
        self.authority.require("request_follow_up")?;
        if request.expected_revision.is_some() {
            return Err(SdkError::new(
                SdkErrorCode::Unsupported,
                "this host does not support a revision fence on follow-up; \
                 omit expectedRevision or use the queue mutators directly",
            )
            .with_detail("capability", CapabilityId::RunFollowUp.as_wire()));
        }
        let workspace = self.resolve_workspace(&request.workspace)?;

        let body = self
            .call(
                tools::STEER,
                json!({
                    "request_id": request.request_id.as_str(),
                    "session_id": request.session_id.as_str(),
                    "workspace": workspace,
                    "text": request.text,
                }),
            )
            .await?;

        let disposition = str_field(&body, "disposition", "disposition")
            .and_then(|raw| serde_json::from_value(Value::String(raw)).ok())
            .ok_or_else(|| malformed("steer.disposition"))?;
        Ok(FollowUpReceipt {
            disposition,
            revision: Revision::new(u64_field(&body, "revision", "revision").unwrap_or(0)),
            replayed: None,
        })
    }

    /// Cancel a run, then re-read the durable record.
    ///
    /// The mutation response reports a constant `state: "cancelled"` rather
    /// than a re-read, so the receipt's lifecycle comes from `ptah_get_run`
    /// instead. One extra bounded read buys a receipt that reflects what the
    /// ledger actually holds — including a run that had already reached a
    /// different terminal state.
    async fn cancel_run(&self, request: CancelRequest) -> SdkResult<CancelReceipt> {
        self.authority.require("cancel_run")?;
        let mut args = self.run_scope(&request.selector)?;
        args.as_object_mut()
            .expect("run_scope builds an object")
            .insert(
                "request_id".into(),
                Value::String(request.request_id.as_str().to_string()),
            );

        let body = self.call(tools::CANCEL, args).await?;
        let was_queued = bool_field(&body, "was_queued", "wasQueued");
        let observed = self.observe_run(request.selector.clone()).await?;

        Ok(CancelReceipt {
            run_id: observed.run_id.clone(),
            lifecycle: observed.lifecycle,
            was_queued,
            revision: observed.revision,
            replayed: None,
        })
    }

    async fn acquire_control(&self, request: ControlLeaseRequest) -> SdkResult<ControlLease> {
        self.authority.require("acquire_control")?;
        let workspace = self.resolve_workspace(&request.workspace)?;

        let mut args = Map::new();
        args.insert(
            "request_id".into(),
            Value::String(request.request_id.as_str().to_string()),
        );
        args.insert(
            "session_id".into(),
            Value::String(request.session_id.as_str().to_string()),
        );
        args.insert("workspace".into(), Value::String(workspace));
        args.insert(
            "work_id".into(),
            Value::String(request.work_id.as_str().to_string()),
        );
        args.insert(
            "agent_id".into(),
            Value::String(request.claimant.as_str().to_string()),
        );
        insert_optional(
            &mut args,
            "lease_ms",
            request.requested_ttl_ms.map(|value| json!(value)),
        );

        let body = self.call(tools::CLAIM_WORK, Value::Object(args)).await?;
        let attempt = field(&body, "attempt", "attempt")
            .cloned()
            .ok_or_else(|| malformed("claimWork.attempt"))?;
        let lease_token =
            str_field(&body, "lease_token", "leaseToken").ok_or_else(|| malformed("leaseToken"))?;
        let acquired_at = timestamp(&attempt, "acquired_at", "acquiredAt");
        let epoch = DateTime::from_timestamp(0, 0).expect("epoch is a valid timestamp");

        Ok(ControlLease {
            work_id: request.work_id.clone(),
            attempt_id: AttemptId::new(
                str_field(&attempt, "attempt_id", "attemptId")
                    .ok_or_else(|| malformed("attempt.attemptId"))?,
            )?,
            attempt_number: u32_field(&attempt, "attempt_number", "attemptNumber").unwrap_or(1),
            claimant: request.claimant.clone(),
            acquired_at: acquired_at.unwrap_or(epoch),
            expires_at: timestamp(&attempt, "lease_expires_at", "leaseExpiresAt").unwrap_or(epoch),
            revision: revision_from(timestamp(&attempt, "updated_at", "updatedAt").or(acquired_at)),
            credential: LeaseCredential::new(lease_token),
        })
    }

    async fn release_control(
        &self,
        request: ReleaseLeaseRequest,
    ) -> SdkResult<ReleaseLeaseReceipt> {
        self.authority.require("release_control")?;
        if request.credential.is_empty() {
            return Err(SdkError::new(
                SdkErrorCode::InvalidRequest,
                "releasing a lease requires the credential returned by acquire_control",
            ));
        }
        let workspace = self.resolve_workspace(&request.workspace)?;

        let body = self
            .call(
                tools::RELEASE_WORK,
                json!({
                    "request_id": request.request_id.as_str(),
                    "session_id": request.session_id.as_str(),
                    "workspace": workspace,
                    "work_id": request.work_id.as_str(),
                    "attempt_id": request.attempt_id.as_str(),
                    "lease_token": request.credential.reveal(),
                    "reason": request.reason.as_str(),
                }),
            )
            .await?;

        let attempt = field(&body, "attempt", "attempt")
            .cloned()
            .unwrap_or(Value::Null);
        Ok(ReleaseLeaseReceipt {
            work_id: request.work_id.clone(),
            attempt_id: request.attempt_id.clone(),
            revision: revision_from(timestamp(&attempt, "updated_at", "updatedAt")),
            replayed: None,
        })
    }

    /// Redacted receipts are not reachable over this boundary.
    ///
    /// The control plane's tool registry has no receipt, audit, or idempotency
    /// read among its `ptah_*` names — the durable receipts exist, but nothing
    /// exposes them. Reporting `unsupported` is the honest answer; an empty
    /// page would read as "no mutations happened".
    async fn list_receipts(
        &self,
        _selector: RunSelector,
        _page: PageRequest,
    ) -> SdkResult<ReceiptPage> {
        Err(SdkError::new(
            SdkErrorCode::Unsupported,
            "the control plane exposes no receipt read; see docs/AGENT_SDK_SEAM.md",
        )
        .with_detail("capability", CapabilityId::ReceiptRead.as_wire()))
    }

    /// Fetch one bounded, digest-verified artifact.
    ///
    /// This build serves exactly one artifact: the run's structured test
    /// report, built from `ptah_get_test_results`. The reported `command`
    /// string is dropped — it can carry absolute paths — leaving call identity
    /// and outcome. `ptah_review_run` is deliberately not mapped: reviewed
    /// diffs are entangled with approve/promote operator authority.
    async fn fetch_artifact(&self, request: ArtifactRequest) -> SdkResult<ArtifactPayload> {
        let ceiling = request
            .max_bytes
            .unwrap_or(self.limits.max_artifact_bytes)
            .min(self.limits.max_artifact_bytes);
        if request.artifact_id.as_str() != TEST_REPORT_ARTIFACT_ID {
            return Err(SdkError::new(
                SdkErrorCode::ForbiddenScope,
                "artifact is not available to this session",
            ));
        }

        let body = self
            .call(tools::GET_TEST_RESULTS, self.run_scope(&request.selector)?)
            .await?;
        let tests = field(&body, "tests", "tests")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let projected: Vec<Value> = tests
            .iter()
            .map(|test| {
                json!({
                    "callId": str_field(test, "call_id", "callId").unwrap_or_default(),
                    "status": str_field(test, "status", "status").unwrap_or_default(),
                    "exitCode": field(test, "exit_code", "exitCode").cloned(),
                    "cancelled": bool_field(test, "cancelled", "cancelled"),
                })
            })
            .collect();
        let content = serde_json::to_string_pretty(&json!({ "tests": projected }))
            .map_err(|error| SdkError::new(SdkErrorCode::Internal, error.to_string()))?;

        let payload = ArtifactPayload {
            descriptor: ArtifactDescriptor {
                artifact_id: request.artifact_id.clone(),
                kind: ArtifactKind::TestReport,
                media: ArtifactMedia::Json,
                label: Label::new("test report").expect("static label"),
                byte_len: content.len() as u64,
                digest: ContentDigest::sha256_of(content.as_bytes()),
                retained_until: None,
            },
            content,
        };
        payload.verify(ceiling)?;
        Ok(payload)
    }
}

/// The only artifact id this adapter serves. Stable so a consumer can request
/// it without first observing a descriptor.
pub const TEST_REPORT_ARTIFACT_ID: &str = "run-test-report";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonrpc_errors_carry_their_typed_code_and_retained_range() {
        let fault = TransportFault::from_jsonrpc_error(&json!({
            "code": -32000,
            "message": "computer run event cursor is below the retained window",
            "data": { "code": "cursor_expired", "eventRange": { "startSeq": 41, "endSeq": 99 } }
        }));
        let error = fault.into_sdk_error();
        assert_eq!(error.code, SdkErrorCode::CursorExpired);
        assert_eq!(error.detail("retainedStart"), Some("41"));
        assert_eq!(error.detail("retainedEnd"), Some("99"));
    }

    #[test]
    fn an_untyped_host_error_is_internal_not_a_guess() {
        let fault =
            TransportFault::from_jsonrpc_error(&json!({ "code": -32603, "message": "boom" }));
        let error = fault.into_sdk_error();
        assert_eq!(error.code, SdkErrorCode::Internal);
        assert!(error.code.is_safely_retryable());
    }

    #[test]
    fn workspace_refs_are_stable_key_dependent_and_never_paths() {
        let mut registry = WorkspaceRegistry::new();
        let first = registry.learn("/home/user/project").unwrap();
        let again = registry.learn("/home/user/project").unwrap();
        assert_eq!(first, again, "refs must be stable across restarts");
        assert!(first.as_str().starts_with("ws-"));
        assert!(!first.as_str().contains('/'));
        assert_eq!(registry.len(), 1);

        let mut keyed = WorkspaceRegistry::with_ref_key(b"tenant-secret");
        let salted = keyed.learn("/home/user/project").unwrap();
        assert_ne!(first, salted, "a distinct key must derive a distinct ref");
    }

    #[test]
    fn an_unlearned_ref_is_a_workspace_mismatch() {
        let registry = WorkspaceRegistry::new();
        let forged = WorkspaceRef::new("ws-deadbeefdeadbeef").unwrap();
        assert_eq!(
            registry.resolve(&forged).unwrap_err().code,
            SdkErrorCode::WorkspaceMismatch
        );
    }

    #[test]
    fn transcript_events_have_no_projection() {
        for tag in [
            "agent_message_chunk",
            "agent_thought_chunk",
            "shell_output",
            "tool_call_update",
            "plan",
        ] {
            let entry = json!({
                "seq": 7,
                "ts": "2026-01-01T00:00:00Z",
                "update": { "type": tag, "session_id": "s", "text": "SECRET" }
            });
            assert!(
                project_event(&entry).is_none(),
                "{tag} must not be projected"
            );
        }
    }

    #[test]
    fn file_edits_project_without_their_diff() {
        let entry = json!({
            "seq": 9,
            "ts": "2026-01-01T00:00:00Z",
            "update": {
                "type": "file_edit",
                "session_id": "s",
                "path": "src/lib.rs",
                "summary": "edited",
                "unified_diff": "SECRET DIFF BODY"
            }
        });
        let event = project_event(&entry).expect("file_edit projects");
        let encoded = serde_json::to_string(&event).unwrap();
        assert!(!encoded.contains("SECRET DIFF BODY"), "{encoded}");
        assert!(!encoded.contains("unifiedDiff"), "{encoded}");
    }

    #[test]
    fn an_absolute_edit_path_is_dropped_rather_than_surfaced() {
        let entry = json!({
            "seq": 11,
            "ts": "2026-01-01T00:00:00Z",
            "update": {
                "type": "file_edit",
                "session_id": "s",
                "path": "/home/user/project/src/lib.rs",
                "summary": "edited",
                "unified_diff": ""
            }
        });
        assert!(project_event(&entry).is_none());
    }

    #[test]
    fn unknown_event_kinds_survive_as_unrecognized() {
        let entry = json!({
            "seq": 3,
            "ts": "2026-01-01T00:00:00Z",
            "update": { "type": "future_kind", "session_id": "s" }
        });
        match project_event(&entry).expect("unknown kinds project").kind {
            PublicEventKind::Unrecognized { wire_kind } => {
                assert_eq!(wire_kind.as_str(), "future_kind");
            }
            other => panic!("expected Unrecognized, got {other:?}"),
        }
    }

    #[test]
    fn observer_authority_refuses_before_any_transport_use() {
        assert_eq!(
            MutationAuthority::Observer
                .require("submit_task")
                .unwrap_err()
                .code,
            SdkErrorCode::ForbiddenScope
        );
        assert!(MutationAuthority::OperatorEquivalent
            .require("submit_task")
            .is_ok());
    }

    #[test]
    fn revisions_derive_from_the_durable_timestamp() {
        let at = DateTime::parse_from_rfc3339("2026-01-01T00:00:01.500Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(revision_from(Some(at)), Revision::new(1_767_225_601_500));
        assert_eq!(revision_from(None), Revision::new(0));
    }
}
