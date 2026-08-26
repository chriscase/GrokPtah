//! Deterministic in-memory adapter.
//!
//! No wall clock, no randomness, no I/O. Every timestamp comes from a counter,
//! so two runs of the same script produce byte-identical output. This exists
//! for three jobs:
//!
//! * a consumer (ContextDesk) can build and test its whole UI before any real
//!   host adapter ships;
//! * the [`conformance`](crate::conformance) battery has something to prove
//!   itself against; and
//! * failure modes that are hard to produce on demand against a live host —
//!   a dropped connection, an uncertain in-flight mutation, an expired cursor,
//!   a corrupted artifact — become one line of setup.
//!
//! It is not a simulator of GrokPtah's agent behavior. It models the
//! **boundary**: identity, scope, idempotency, ordering, and failure shape.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};

use crate::capability::{
    Availability, BoundaryLimits, CapabilityDescriptor, CapabilityDocument, CapabilityId,
    HostDescriptor, HostKind,
};
use crate::client::AgentControlPlane;
use crate::dto::*;
use crate::error::{SdkError, SdkErrorCode, SdkResult};
use crate::ids::*;
use crate::page::{cursor_expired, Cursor, Page, PageRequest, RetainedRange};
use crate::version::{ContractVersion, CONTRACT_VERSION};

/// Fixed epoch for every fake timestamp: 2026-01-01T00:00:00Z.
const EPOCH_SECONDS: i64 = 1_767_225_600;
/// Seconds each deterministic tick advances.
const TICK_SECONDS: i64 = 1;

/// A fault to inject into the next matching call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The adapter could not reach the host, or a stream dropped mid-page.
    LostConnection,
    /// The host recorded a claim then stopped before recording an outcome.
    /// The mutation may or may not have applied.
    UncertainSend,
    /// The request exceeded its deadline. Safe to retry with the same key.
    Timeout,
    /// The credential was rejected.
    Unauthenticated,
}

impl Fault {
    fn into_error(self) -> SdkError {
        match self {
            Self::LostConnection => SdkError::new(
                SdkErrorCode::TransportUnavailable,
                "connection to the agent host was lost",
            ),
            Self::UncertainSend => SdkError::new(
                SdkErrorCode::UncertainOutcome,
                "the host stopped while this mutation was in flight; it will not be retried automatically",
            ),
            Self::Timeout => {
                SdkError::new(SdkErrorCode::Timeout, "request deadline exceeded")
            }
            Self::Unauthenticated => {
                SdkError::new(SdkErrorCode::Unauthenticated, "invalid credential")
            }
        }
    }
}

/// Which operation a queued fault applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Operation {
    Capabilities,
    CreateSession,
    ListSessions,
    SubmitTask,
    ObserveRun,
    StreamEvents,
    RequestFollowUp,
    CancelRun,
    AcquireControl,
    ReleaseControl,
    FetchArtifact,
}

/// How a task run should end when driven to completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptedOutcome {
    Completed,
    Failed,
    LimitReached,
}

#[derive(Debug, Clone)]
struct FakeSession {
    view: SessionView,
    owner: String,
    queue_revision: Revision,
}

#[derive(Debug, Clone)]
struct FakeRun {
    view: RunView,
    events: Vec<PublicEvent>,
    /// Lowest sequence still readable. Everything below is expired.
    retained_from: u64,
    /// Highest sequence ever emitted, retained or not.
    high_water: u64,
    artifacts: BTreeMap<String, ArtifactPayload>,
}

#[derive(Debug, Clone)]
struct Receipt {
    payload_hash: String,
    response: serde_json::Value,
}

#[derive(Debug)]
struct FakeState {
    owner: String,
    host: HostDescriptor,
    contract_version: ContractVersion,
    limits: BoundaryLimits,
    offered: Vec<CapabilityDescriptor>,
    workspaces: BTreeMap<String, Label>,
    sessions: BTreeMap<String, FakeSession>,
    runs: BTreeMap<String, FakeRun>,
    leases: BTreeMap<String, ControlLease>,
    receipts: BTreeMap<String, Receipt>,
    faults: Vec<(Option<Operation>, Fault)>,
    clock_ticks: i64,
    next_id: u64,
}

impl FakeState {
    fn now(&mut self) -> DateTime<Utc> {
        let at = Utc
            .timestamp_opt(EPOCH_SECONDS + self.clock_ticks * TICK_SECONDS, 0)
            .single()
            .expect("fixed epoch is a valid timestamp");
        self.clock_ticks += 1;
        at
    }

    fn mint(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}-{:04}", self.next_id)
    }

    /// Pop a fault for `op`, preferring an operation-specific one.
    fn take_fault(&mut self, op: Operation) -> Option<Fault> {
        if let Some(index) = self
            .faults
            .iter()
            .position(|(target, _)| *target == Some(op))
        {
            return Some(self.faults.remove(index).1);
        }
        let index = self
            .faults
            .iter()
            .position(|(target, _)| target.is_none())?;
        Some(self.faults.remove(index).1)
    }

    /// The single denial used for every out-of-scope read.
    ///
    /// One message for unknown, cross-session, and cross-owner alike, so a
    /// caller cannot use the error text to learn that a resource exists.
    fn scope_denied() -> SdkError {
        SdkError::new(
            SdkErrorCode::ForbiddenScope,
            "resource is not available to this session",
        )
    }

    fn require_workspace(&self, workspace: &WorkspaceRef) -> SdkResult<()> {
        // Allowlist first, and session-independent: a workspace this host does
        // not serve is `workspace_mismatch` whoever asks. Mirrors the runtime,
        // where the allowlist gate precedes the session/cwd gate.
        if self.workspaces.contains_key(workspace.as_str()) {
            Ok(())
        } else {
            Err(SdkError::new(
                SdkErrorCode::WorkspaceMismatch,
                "workspace is not in this host's allowlist",
            ))
        }
    }

    fn require_session(
        &self,
        session_id: &SessionId,
        workspace: &WorkspaceRef,
    ) -> SdkResult<&FakeSession> {
        self.require_workspace(workspace)?;
        let session = self
            .sessions
            .get(session_id.as_str())
            .ok_or_else(Self::scope_denied)?;
        if session.owner != self.owner || &session.view.workspace != workspace {
            return Err(Self::scope_denied());
        }
        Ok(session)
    }

    fn require_run(&self, selector: &RunSelector) -> SdkResult<&FakeRun> {
        self.require_session(&selector.session_id, &selector.workspace)?;
        let run = self
            .runs
            .get(selector.run_id.as_str())
            .ok_or_else(Self::scope_denied)?;
        if run.view.session_id != selector.session_id || run.view.workspace != selector.workspace {
            return Err(Self::scope_denied());
        }
        Ok(run)
    }

    /// Idempotency: replay on an exact match, conflict on a reused key.
    fn claim(&mut self, request_id: &RequestId, payload: &serde_json::Value) -> ClaimOutcome {
        let hash = hash_payload(payload);
        match self.receipts.get(request_id.as_str()) {
            Some(prior) if prior.payload_hash == hash => {
                ClaimOutcome::Replay(prior.response.clone())
            }
            Some(_) => ClaimOutcome::Conflict,
            None => ClaimOutcome::Perform(hash),
        }
    }

    fn record(
        &mut self,
        request_id: &RequestId,
        payload_hash: String,
        response: serde_json::Value,
    ) {
        self.receipts.insert(
            request_id.as_str().to_string(),
            Receipt {
                payload_hash,
                response,
            },
        );
    }
}

enum ClaimOutcome {
    Perform(String),
    Replay(serde_json::Value),
    Conflict,
}

fn hash_payload(payload: &serde_json::Value) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    // Serialize through a canonical form so key order cannot change the hash.
    hasher.update(canonical(payload).as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn canonical(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}:{}", serde_json::to_string(k).unwrap(), canonical(v)))
                .collect();
            parts.sort();
            format!("{{{}}}", parts.join(","))
        }
        serde_json::Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical).collect();
            format!("[{}]", parts.join(","))
        }
        other => other.to_string(),
    }
}

fn replayed_conflict() -> SdkError {
    SdkError::new(
        SdkErrorCode::Conflict,
        "requestId was already used with a different payload",
    )
}

/// Deterministic in-memory [`AgentControlPlane`].
///
/// `Debug` is safe to print: the only secret it holds is a
/// [`LeaseCredential`], which redacts itself.
#[derive(Debug)]
pub struct FakeControlPlane {
    state: Mutex<FakeState>,
}

impl FakeControlPlane {
    pub fn builder() -> FakeBuilder {
        FakeBuilder::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ── Test drivers (not part of the boundary) ──────────────────────────

    /// Queue a fault for the next call to any operation.
    pub fn inject(&self, fault: Fault) {
        self.lock().faults.push((None, fault));
    }

    /// Queue a fault for the next call to one operation.
    pub fn inject_for(&self, op: Operation, fault: Fault) {
        self.lock().faults.push((Some(op), fault));
    }

    /// Move a queued run to `running` and emit its opening events.
    pub fn start_run(&self, run_id: &RunId) -> SdkResult<()> {
        let mut state = self.lock();
        let at = state.now();
        let run = state
            .runs
            .get_mut(run_id.as_str())
            .ok_or_else(FakeState::scope_denied)?;
        if run.view.lifecycle != RunLifecycle::Queued {
            return Err(SdkError::new(
                SdkErrorCode::Conflict,
                "run has already started",
            ));
        }
        run.view.lifecycle = RunLifecycle::Running;
        run.view.queue_position = None;
        run.view.revision = Revision::new(run.view.revision.value() + 1);
        run.view.updated_at = at;
        push_event(run, at, PublicEventKind::TurnStarted);
        push_event(
            run,
            at,
            PublicEventKind::Progress {
                round: 1,
                max_rounds: run.view.bounds.max_rounds,
                last_tool: None,
            },
        );
        run.view.event_range = event_range(run);
        Ok(())
    }

    /// Drive a running run to a terminal state with scripted evidence.
    pub fn finish_run(&self, run_id: &RunId, outcome: ScriptedOutcome) -> SdkResult<()> {
        let mut state = self.lock();
        let at = state.now();
        let artifact_id = state.mint("artifact");
        let run = state
            .runs
            .get_mut(run_id.as_str())
            .ok_or_else(FakeState::scope_denied)?;
        if run.view.lifecycle.is_terminal() {
            return Err(SdkError::new(
                SdkErrorCode::Conflict,
                "run is already terminal",
            ));
        }
        let (lifecycle, stop_cause, status) = match outcome {
            ScriptedOutcome::Completed => (
                RunLifecycle::Completed,
                StopCause::Completed,
                VerificationStatus::Verified,
            ),
            ScriptedOutcome::Failed => (
                RunLifecycle::Failed,
                StopCause::Failed,
                VerificationStatus::Failed,
            ),
            ScriptedOutcome::LimitReached => (
                RunLifecycle::LimitReached,
                StopCause::RoundLimit,
                VerificationStatus::Incomplete,
            ),
        };

        let path = RelativePath::new("src/lib.rs").expect("static path is relative");
        push_event(
            run,
            at,
            PublicEventKind::ToolCall {
                call_id: Label::new("call-1").expect("static label"),
                tool: ToolKind::Edit,
                status: ToolStatus::Completed,
            },
        );
        push_event(
            run,
            at,
            PublicEventKind::FileChanged {
                path: path.clone(),
                summary: BoundedText::new("edited"),
            },
        );
        push_event(
            run,
            at,
            PublicEventKind::TestObserved {
                outcome: TestOutcome::Passed,
                exit_code: Some(0),
            },
        );
        push_event(
            run,
            at,
            PublicEventKind::RunTerminal {
                lifecycle,
                stop_cause,
            },
        );

        run.view.changed_files = vec![ChangedFile {
            path,
            summary: BoundedText::new("edited"),
        }];
        let usage = UsageView {
            prompt_tokens: 100,
            completion_tokens: 40,
            total_tokens: 140,
            requests: 2,
            complete: true,
            pending_requests: 0,
        };
        run.view.usage = usage;
        run.view.verification = Some(VerificationView {
            status,
            stop_cause,
            interrupted: false,
            observations: ObservationCounts {
                changed_files: 1,
                tests_observed: 1,
                tests_passed: 1,
                ..ObservationCounts::default()
            },
            usage,
        });
        run.view.lifecycle = lifecycle;
        run.view.stop_cause = Some(stop_cause);
        run.view.revision = Revision::new(run.view.revision.value() + 1);
        run.view.updated_at = at;
        run.view.event_range = event_range(run);

        let content = "diff --git a/src/lib.rs b/src/lib.rs\n+// edited\n".to_string();
        let descriptor = ArtifactDescriptor {
            artifact_id: ArtifactId::new(&artifact_id).expect("minted id is valid"),
            kind: ArtifactKind::ReviewDiff,
            media: ArtifactMedia::UnifiedDiff,
            label: Label::new("review diff").expect("static label"),
            byte_len: content.len() as u64,
            digest: ContentDigest::sha256_of(content.as_bytes()),
            retained_until: None,
        };
        run.view.artifacts = vec![descriptor.clone()];
        run.artifacts.insert(
            artifact_id,
            ArtifactPayload {
                descriptor,
                content,
            },
        );
        Ok(())
    }

    /// Expire every retained event at or below `through`, as journal rollover
    /// would. A cursor below the new window then fails with the retained range.
    pub fn expire_events_through(&self, run_id: &RunId, through: u64) {
        let mut state = self.lock();
        if let Some(run) = state.runs.get_mut(run_id.as_str()) {
            run.events.retain(|e| sequence_of(&e.cursor) > through);
            run.retained_from = through + 1;
            run.view.event_range = event_range(run);
        }
    }

    /// Replace an artifact's stored body without restamping its digest, as a
    /// truncating cache or a tampering proxy would.
    pub fn corrupt_artifact(&self, run_id: &RunId, artifact_id: &ArtifactId, content: &str) {
        let mut state = self.lock();
        if let Some(run) = state.runs.get_mut(run_id.as_str()) {
            if let Some(payload) = run.artifacts.get_mut(artifact_id.as_str()) {
                payload.content = content.to_string();
            }
        }
    }

    /// The session this host's own account owns.
    pub fn seeded_session(&self) -> Option<SessionView> {
        let state = self.lock();
        let owner = state.owner.clone();
        state
            .sessions
            .values()
            .find(|s| s.owner == owner)
            .map(|s| s.view.clone())
    }

    /// A session on this same host owned by a **different** account, seeded by
    /// [`FakeBuilder::foreign_owner`]. Presenting it to this credential must
    /// fail closed with the same denial an unknown resource produces.
    pub fn foreign_session(&self) -> Option<SessionView> {
        let state = self.lock();
        let owner = state.owner.clone();
        state
            .sessions
            .values()
            .find(|s| s.owner != owner)
            .map(|s| s.view.clone())
    }
}

fn sequence_of(cursor: &Cursor) -> u64 {
    cursor.as_str().parse().unwrap_or(0)
}

fn push_event(run: &mut FakeRun, at: DateTime<Utc>, kind: PublicEventKind) {
    run.high_water += 1;
    run.events.push(PublicEvent {
        cursor: Cursor::from_opaque(run.high_water.to_string()),
        at,
        kind,
    });
}

fn event_range(run: &FakeRun) -> Option<RetainedRange> {
    let first = run.events.first()?;
    let last = run.events.last()?;
    Some(RetainedRange {
        start: first.cursor.clone(),
        end: last.cursor.clone(),
    })
}

/// Builder for [`FakeControlPlane`].
#[derive(Debug, Clone)]
pub struct FakeBuilder {
    owner: String,
    foreign_owner: Option<String>,
    contract_version: ContractVersion,
    limits: BoundaryLimits,
    offered: Vec<CapabilityDescriptor>,
    workspaces: Vec<(String, String)>,
    session: bool,
}

impl Default for FakeBuilder {
    fn default() -> Self {
        Self {
            owner: "primary".to_string(),
            foreign_owner: Some("other-account".to_string()),
            contract_version: CONTRACT_VERSION,
            limits: BoundaryLimits::default(),
            offered: default_capabilities(),
            workspaces: vec![("ws-alpha".to_string(), "alpha".to_string())],
            session: true,
        }
    }
}

fn default_capabilities() -> Vec<CapabilityDescriptor> {
    [
        CapabilityId::SessionCreate,
        CapabilityId::SessionList,
        CapabilityId::TaskSubmit,
        CapabilityId::RunObserve,
        CapabilityId::RunEventsPage,
        CapabilityId::RunFollowUp,
        CapabilityId::RunCancel,
        CapabilityId::ControlLease,
        CapabilityId::ArtifactFetch,
    ]
    .into_iter()
    .map(|id| CapabilityDescriptor {
        id,
        since: ContractVersion::new(CONTRACT_VERSION.major, 0),
        availability: Availability::Available,
    })
    .collect()
}

impl FakeBuilder {
    /// The owner account every seeded session belongs to.
    pub fn owner(mut self, owner: impl Into<String>) -> Self {
        self.owner = owner.into();
        self
    }

    /// Advertise a different contract version, for negotiation tests.
    pub fn contract_version(mut self, version: ContractVersion) -> Self {
        self.contract_version = version;
        self
    }

    pub fn limits(mut self, limits: BoundaryLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Replace the advertised capability set.
    pub fn capabilities(mut self, offered: Vec<CapabilityDescriptor>) -> Self {
        self.offered = offered;
        self
    }

    /// Add an allowlisted workspace.
    pub fn workspace(mut self, id: impl Into<String>, label: impl Into<String>) -> Self {
        self.workspaces.push((id.into(), label.into()));
        self
    }

    /// Skip seeding an initial Build session.
    pub fn without_session(mut self) -> Self {
        self.session = false;
        self
    }

    /// Seed a second session on the same allowlisted workspace owned by a
    /// different account, so cross-tenant denial can be exercised on **one**
    /// host rather than by comparing two hosts. Pass `None` to seed none.
    pub fn foreign_owner(mut self, owner: Option<&str>) -> Self {
        self.foreign_owner = owner.map(str::to_string);
        self
    }

    pub fn build(self) -> FakeControlPlane {
        let workspaces: BTreeMap<String, Label> = self
            .workspaces
            .iter()
            .map(|(id, label)| {
                (
                    id.clone(),
                    Label::new(label).expect("builder label must be valid"),
                )
            })
            .collect();
        let mut state = FakeState {
            owner: self.owner.clone(),
            host: HostDescriptor {
                kind: HostKind::Fake,
                product: Label::new("GrokPtah").expect("static label"),
                host_version: Label::new("fake").expect("static label"),
            },
            contract_version: self.contract_version,
            limits: self.limits,
            offered: self.offered,
            workspaces,
            sessions: BTreeMap::new(),
            runs: BTreeMap::new(),
            leases: BTreeMap::new(),
            receipts: BTreeMap::new(),
            faults: Vec::new(),
            clock_ticks: 0,
            next_id: 0,
        };
        if self.session {
            let workspace_id = self
                .workspaces
                .first()
                .map(|(id, _)| id.clone())
                .expect("builder always has a workspace");
            let session_id = state.mint("session");
            let at = state.now();
            let view = SessionView {
                session_id: SessionId::new(&session_id).expect("minted id is valid"),
                workspace: WorkspaceRef::new(&workspace_id).expect("workspace id is valid"),
                kind: SessionKind::Build,
                title: Some(Label::new("seeded").expect("static label")),
                revision: Revision::new(1),
                created_at: at,
            };
            state.sessions.insert(
                session_id,
                FakeSession {
                    view,
                    owner: self.owner.clone(),
                    queue_revision: Revision::new(1),
                },
            );
            if let Some(foreign) = self.foreign_owner.clone() {
                let foreign_id = state.mint("session");
                let at = state.now();
                let view = SessionView {
                    session_id: SessionId::new(&foreign_id).expect("minted id is valid"),
                    workspace: WorkspaceRef::new(&workspace_id).expect("workspace id is valid"),
                    kind: SessionKind::Build,
                    title: Some(Label::new("foreign").expect("static label")),
                    revision: Revision::new(1),
                    created_at: at,
                };
                state.sessions.insert(
                    foreign_id,
                    FakeSession {
                        view,
                        owner: foreign,
                        queue_revision: Revision::new(1),
                    },
                );
            }
        }
        FakeControlPlane {
            state: Mutex::new(state),
        }
    }
}

macro_rules! guard {
    ($state:expr, $op:expr) => {
        if let Some(fault) = $state.take_fault($op) {
            return Err(fault.into_error());
        }
    };
}

#[async_trait]
impl AgentControlPlane for FakeControlPlane {
    async fn capabilities(&self) -> SdkResult<CapabilityDocument> {
        let mut state = self.lock();
        guard!(state, Operation::Capabilities);
        let at = state.now();
        let mut document =
            CapabilityDocument::new(state.host.clone(), at, state.limits, state.offered.clone());
        document.contract_version = state.contract_version;
        Ok(document)
    }

    async fn create_session(&self, request: CreateSessionRequest) -> SdkResult<SessionView> {
        let mut state = self.lock();
        guard!(state, Operation::CreateSession);
        state.require_workspace(&request.workspace)?;
        let payload = serde_json::to_value(&request)
            .map_err(|e| SdkError::new(SdkErrorCode::Internal, e.to_string()))?;
        let hash = match state.claim(&request.request_id, &payload) {
            ClaimOutcome::Replay(value) => {
                return serde_json::from_value(value)
                    .map_err(|e| SdkError::new(SdkErrorCode::Internal, e.to_string()))
            }
            ClaimOutcome::Conflict => return Err(replayed_conflict()),
            ClaimOutcome::Perform(hash) => hash,
        };
        let session_id = state.mint("session");
        let at = state.now();
        let view = SessionView {
            session_id: SessionId::new(&session_id)?,
            workspace: request.workspace.clone(),
            kind: SessionKind::Build,
            title: request.title.clone(),
            revision: Revision::new(1),
            created_at: at,
        };
        let owner = state.owner.clone();
        state.sessions.insert(
            session_id,
            FakeSession {
                view: view.clone(),
                owner,
                queue_revision: Revision::new(1),
            },
        );
        let response = serde_json::to_value(&view)
            .map_err(|e| SdkError::new(SdkErrorCode::Internal, e.to_string()))?;
        state.record(&request.request_id, hash, response);
        Ok(view)
    }

    async fn list_sessions(&self, page: PageRequest) -> SdkResult<Page<SessionView>> {
        let mut state = self.lock();
        guard!(state, Operation::ListSessions);
        // One advertised page ceiling governs every paged read on this
        // boundary; there is no separate session-page limit to diverge from it.
        let limit = page.resolve_limit(state.limits.max_event_page)? as usize;
        let after = page.after.as_ref().map(|c| c.as_str().to_string());
        let owner = state.owner.clone();
        let mut items: Vec<SessionView> = state
            .sessions
            .iter()
            .filter(|(_, session)| session.owner == owner)
            .filter(|(id, _)| after.as_deref().map(|a| id.as_str() > a).unwrap_or(true))
            .map(|(_, session)| session.view.clone())
            .collect();
        let has_more = items.len() > limit;
        items.truncate(limit);
        let next_cursor = has_more
            .then(|| {
                items
                    .last()
                    .map(|s| Cursor::from_opaque(s.session_id.as_str()))
            })
            .flatten();
        Ok(Page::new(items, next_cursor))
    }

    async fn submit_task(&self, request: TaskSubmission) -> SdkResult<RunAccepted> {
        let mut state = self.lock();
        guard!(state, Operation::SubmitTask);
        let session = state
            .require_session(&request.session_id, &request.workspace)?
            .clone();
        if session.view.kind != SessionKind::Build {
            return Err(SdkError::new(
                SdkErrorCode::InvalidRequest,
                "only Build sessions accept task submission",
            ));
        }
        let max_prompt = state.limits.max_prompt_bytes;
        if request.prompt.len() as u64 > max_prompt {
            return Err(SdkError::new(
                SdkErrorCode::InvalidRequest,
                format!("prompt exceeds maxPromptBytes ({max_prompt})"),
            ));
        }
        if request.prompt.trim().is_empty() {
            return Err(SdkError::new(
                SdkErrorCode::InvalidRequest,
                "prompt must not be empty",
            ));
        }
        let payload = serde_json::to_value(&request)
            .map_err(|e| SdkError::new(SdkErrorCode::Internal, e.to_string()))?;
        let hash = match state.claim(&request.request_id, &payload) {
            ClaimOutcome::Replay(value) => {
                let mut accepted: RunAccepted = serde_json::from_value(value)
                    .map_err(|e| SdkError::new(SdkErrorCode::Internal, e.to_string()))?;
                accepted.replayed = true;
                return Ok(accepted);
            }
            ClaimOutcome::Conflict => return Err(replayed_conflict()),
            ClaimOutcome::Perform(hash) => hash,
        };

        let run_id_raw = state.mint("run");
        let at = state.now();
        let defaults = AppliedBounds {
            max_prompt_bytes: state.limits.max_prompt_bytes,
            max_rounds: 24,
            max_duration_ms: 15 * 60 * 1000,
            max_total_tokens: None,
        };
        // Requested bounds may only narrow. Widening is rejected, never
        // silently accepted, so a caller cannot escalate its own ceiling.
        let bounds = match request.bounds {
            None => defaults,
            Some(requested) => {
                let narrowed = AppliedBounds {
                    max_prompt_bytes: defaults.max_prompt_bytes,
                    max_rounds: requested.max_rounds.unwrap_or(defaults.max_rounds),
                    max_duration_ms: requested
                        .max_duration_ms
                        .unwrap_or(defaults.max_duration_ms),
                    max_total_tokens: requested.max_total_tokens.or(defaults.max_total_tokens),
                };
                if narrowed.max_rounds > defaults.max_rounds
                    || narrowed.max_duration_ms > defaults.max_duration_ms
                    || narrowed.max_rounds == 0
                    || narrowed.max_duration_ms == 0
                {
                    return Err(SdkError::new(
                        SdkErrorCode::InvalidRequest,
                        "requested bounds may only narrow the host ceiling",
                    ));
                }
                narrowed
            }
        };
        let queue_position = request.allow_queue.then_some(1);
        let view = RunView {
            run_id: RunId::new(&run_id_raw)?,
            session_id: request.session_id.clone(),
            workspace: request.workspace.clone(),
            lifecycle: RunLifecycle::Queued,
            stop_cause: None,
            revision: Revision::new(1),
            execution_mode: request.execution_mode,
            queue_position,
            bounds,
            usage: UsageView::default(),
            progress: None,
            changed_files: Vec::new(),
            artifacts: Vec::new(),
            verification: None,
            terminal_marker: None,
            event_range: None,
            created_at: at,
            updated_at: at,
        };
        let accepted = RunAccepted {
            run_id: view.run_id.clone(),
            lifecycle: view.lifecycle,
            queue_position: view.queue_position,
            revision: view.revision,
            replayed: false,
        };
        state.runs.insert(
            run_id_raw,
            FakeRun {
                view,
                events: Vec::new(),
                retained_from: 1,
                high_water: 0,
                artifacts: BTreeMap::new(),
            },
        );
        let response = serde_json::to_value(&accepted)
            .map_err(|e| SdkError::new(SdkErrorCode::Internal, e.to_string()))?;
        state.record(&request.request_id, hash, response);
        Ok(accepted)
    }

    async fn observe_run(&self, selector: RunSelector) -> SdkResult<RunView> {
        let mut state = self.lock();
        guard!(state, Operation::ObserveRun);
        Ok(state.require_run(&selector)?.view.clone())
    }

    async fn stream_events(
        &self,
        selector: RunSelector,
        page: PageRequest,
    ) -> SdkResult<Page<PublicEvent>> {
        let mut state = self.lock();
        guard!(state, Operation::StreamEvents);
        let max_page = state.limits.max_event_page;
        let run = state.require_run(&selector)?;
        let limit = page.resolve_limit(max_page)? as usize;
        let after = page.after.as_ref().map(sequence_of).unwrap_or(0);
        if after > 0 && after < run.retained_from.saturating_sub(1) {
            let Some(range) = event_range(run) else {
                return Err(SdkError::new(
                    SdkErrorCode::CursorExpired,
                    "no retained events remain for this run",
                ));
            };
            return Err(cursor_expired(range));
        }
        let mut items: Vec<PublicEvent> = run
            .events
            .iter()
            .filter(|e| sequence_of(&e.cursor) > after)
            .cloned()
            .collect();
        let has_more = items.len() > limit;
        items.truncate(limit);
        let next_cursor = has_more
            .then(|| items.last().map(|e| e.cursor.clone()))
            .flatten();
        Ok(Page::new(items, next_cursor))
    }

    async fn request_follow_up(&self, request: FollowUpRequest) -> SdkResult<FollowUpReceipt> {
        let mut state = self.lock();
        guard!(state, Operation::RequestFollowUp);
        let session = state
            .require_session(&request.session_id, &request.workspace)?
            .clone();
        if request.text.trim().is_empty() {
            return Err(SdkError::new(
                SdkErrorCode::InvalidRequest,
                "follow-up text must not be empty",
            ));
        }
        // Compare-and-set before any effect, so a rejected fence never mutates.
        if let Some(expected) = request.expected_revision {
            if expected != session.queue_revision {
                return Err(SdkError::new(
                    SdkErrorCode::StaleVersion,
                    "session queue revision has moved; re-read and retry",
                )
                .with_detail("expectedRevision", expected.to_string())
                .with_detail("currentRevision", session.queue_revision.to_string()));
            }
        }
        let payload = serde_json::to_value(&request)
            .map_err(|e| SdkError::new(SdkErrorCode::Internal, e.to_string()))?;
        let hash = match state.claim(&request.request_id, &payload) {
            ClaimOutcome::Replay(value) => {
                let mut receipt: FollowUpReceipt = serde_json::from_value(value)
                    .map_err(|e| SdkError::new(SdkErrorCode::Internal, e.to_string()))?;
                receipt.replayed = true;
                return Ok(receipt);
            }
            ClaimOutcome::Conflict => return Err(replayed_conflict()),
            ClaimOutcome::Perform(hash) => hash,
        };
        let active = state.runs.values().any(|run| {
            run.view.session_id == request.session_id && run.view.lifecycle == RunLifecycle::Running
        });
        let next = Revision::new(session.queue_revision.value() + 1);
        if let Some(entry) = state.sessions.get_mut(request.session_id.as_str()) {
            entry.queue_revision = next;
        }
        let receipt = FollowUpReceipt {
            disposition: if active {
                FollowUpDisposition::Pending
            } else {
                FollowUpDisposition::Queued
            },
            revision: next,
            replayed: false,
        };
        let response = serde_json::to_value(&receipt)
            .map_err(|e| SdkError::new(SdkErrorCode::Internal, e.to_string()))?;
        state.record(&request.request_id, hash, response);
        Ok(receipt)
    }

    async fn cancel_run(&self, request: CancelRequest) -> SdkResult<CancelReceipt> {
        let mut state = self.lock();
        guard!(state, Operation::CancelRun);
        state.require_run(&request.selector)?;
        let payload = serde_json::to_value(&request)
            .map_err(|e| SdkError::new(SdkErrorCode::Internal, e.to_string()))?;
        let hash = match state.claim(&request.request_id, &payload) {
            ClaimOutcome::Replay(value) => {
                let mut receipt: CancelReceipt = serde_json::from_value(value)
                    .map_err(|e| SdkError::new(SdkErrorCode::Internal, e.to_string()))?;
                receipt.replayed = true;
                return Ok(receipt);
            }
            ClaimOutcome::Conflict => return Err(replayed_conflict()),
            ClaimOutcome::Perform(hash) => hash,
        };
        let at = state.now();
        let run = state
            .runs
            .get_mut(request.selector.run_id.as_str())
            .ok_or_else(FakeState::scope_denied)?;
        let was_queued = run.view.lifecycle == RunLifecycle::Queued;
        if !run.view.lifecycle.is_terminal() {
            run.view.lifecycle = RunLifecycle::Cancelled;
            run.view.stop_cause = Some(StopCause::Cancelled);
            run.view.queue_position = None;
            run.view.revision = Revision::new(run.view.revision.value() + 1);
            run.view.updated_at = at;
            push_event(
                run,
                at,
                PublicEventKind::RunTerminal {
                    lifecycle: RunLifecycle::Cancelled,
                    stop_cause: StopCause::Cancelled,
                },
            );
            run.view.event_range = event_range(run);
        }
        let receipt = CancelReceipt {
            run_id: run.view.run_id.clone(),
            lifecycle: run.view.lifecycle,
            was_queued,
            revision: run.view.revision,
            replayed: false,
        };
        let response = serde_json::to_value(&receipt)
            .map_err(|e| SdkError::new(SdkErrorCode::Internal, e.to_string()))?;
        state.record(&request.request_id, hash, response);
        Ok(receipt)
    }

    async fn acquire_control(&self, request: ControlLeaseRequest) -> SdkResult<ControlLease> {
        let mut state = self.lock();
        guard!(state, Operation::AcquireControl);
        state.require_session(&request.session_id, &request.workspace)?;
        if let Some(existing) = state.leases.get(request.work_id.as_str()) {
            // At most one active lease per work item. A second claimant is a
            // conflict, not a silent takeover.
            if existing.claimant != request.claimant {
                return Err(SdkError::new(
                    SdkErrorCode::Conflict,
                    "work item already has an active lease",
                ));
            }
        }
        let payload = serde_json::to_value(&request)
            .map_err(|e| SdkError::new(SdkErrorCode::Internal, e.to_string()))?;
        let hash = match state.claim(&request.request_id, &payload) {
            ClaimOutcome::Replay(_) => {
                // The lease credential is never serialized, so a replay returns
                // the live lease rather than reconstructing one from a receipt.
                return state
                    .leases
                    .get(request.work_id.as_str())
                    .cloned()
                    .ok_or_else(|| {
                        SdkError::new(SdkErrorCode::Conflict, "leased attempt is no longer active")
                    });
            }
            ClaimOutcome::Conflict => return Err(replayed_conflict()),
            ClaimOutcome::Perform(hash) => hash,
        };
        let attempt_raw = state.mint("attempt");
        let acquired_at = state.now();
        let ttl_ms = request.requested_ttl_ms.unwrap_or(30_000).min(300_000);
        let expires_at = acquired_at + chrono::Duration::milliseconds(ttl_ms as i64);
        let lease = ControlLease {
            work_id: request.work_id.clone(),
            attempt_id: AttemptId::new(&attempt_raw)?,
            attempt_number: 1,
            claimant: request.claimant.clone(),
            acquired_at,
            expires_at,
            revision: Revision::new(1),
            credential: LeaseCredential::new(format!("lease-secret-{attempt_raw}")),
        };
        state
            .leases
            .insert(request.work_id.as_str().to_string(), lease.clone());
        state.record(&request.request_id, hash, serde_json::Value::Null);
        Ok(lease)
    }

    async fn release_control(
        &self,
        request: ReleaseLeaseRequest,
    ) -> SdkResult<ReleaseLeaseReceipt> {
        let mut state = self.lock();
        guard!(state, Operation::ReleaseControl);
        state.require_session(&request.session_id, &request.workspace)?;
        let payload = serde_json::to_value(&request)
            .map_err(|e| SdkError::new(SdkErrorCode::Internal, e.to_string()))?;
        let hash = match state.claim(&request.request_id, &payload) {
            ClaimOutcome::Replay(value) => {
                let mut receipt: ReleaseLeaseReceipt = serde_json::from_value(value)
                    .map_err(|e| SdkError::new(SdkErrorCode::Internal, e.to_string()))?;
                receipt.replayed = true;
                return Ok(receipt);
            }
            ClaimOutcome::Conflict => return Err(replayed_conflict()),
            ClaimOutcome::Perform(hash) => hash,
        };
        let Some(lease) = state.leases.get(request.work_id.as_str()).cloned() else {
            return Err(FakeState::scope_denied());
        };
        if lease.attempt_id != request.attempt_id {
            return Err(FakeState::scope_denied());
        }
        state.leases.remove(request.work_id.as_str());
        let receipt = ReleaseLeaseReceipt {
            work_id: request.work_id.clone(),
            attempt_id: request.attempt_id.clone(),
            revision: Revision::new(lease.revision.value() + 1),
            replayed: false,
        };
        let response = serde_json::to_value(&receipt)
            .map_err(|e| SdkError::new(SdkErrorCode::Internal, e.to_string()))?;
        state.record(&request.request_id, hash, response);
        Ok(receipt)
    }

    async fn fetch_artifact(&self, request: ArtifactRequest) -> SdkResult<ArtifactPayload> {
        let mut state = self.lock();
        guard!(state, Operation::FetchArtifact);
        let ceiling = request
            .max_bytes
            .unwrap_or(state.limits.max_artifact_bytes)
            .min(state.limits.max_artifact_bytes);
        let run = state.require_run(&request.selector)?;
        let payload = run
            .artifacts
            .get(request.artifact_id.as_str())
            .cloned()
            .ok_or_else(FakeState::scope_denied)?;
        // Verify before returning. A consumer must never be handed bytes the
        // adapter has not checked against the declared size and digest.
        payload.verify(ceiling)?;
        Ok(payload)
    }
}
