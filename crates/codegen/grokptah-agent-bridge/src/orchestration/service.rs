//! Orchestration service: reads + bounded mutations over AgentHostHandle (#196).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use chrono::Utc;
use parking_lot::Mutex;
use serde_json::json;
use uuid::Uuid;

use crate::event_bus::{CursorExpiredError, EventBus};
use crate::host::AgentHostHandle;
use crate::session::SessionKind;

use super::authz::{require_workspace_match, AuthContext, WorkspaceAllowlist};
use super::store::{IdempotencyClaim, OrchStore};
use super::types::*;

/// Admission is deliberately bounded so an untrusted coordinator cannot turn
/// queued submissions into an unbounded in-memory prompt store.
const MAX_PENDING_ADMISSIONS: usize = 32;

#[derive(Default)]
struct AdmissionQueueState {
    pending: VecDeque<PendingRun>,
    /// Prefer a different session when one is available, while preserving
    /// FIFO order within each session.
    last_started_session: Option<Uuid>,
}

struct PendingRun {
    run_id: String,
    session_id: Uuid,
    prompt: String,
    execution_mode: RunExecutionMode,
}

#[derive(Clone)]
pub struct OrchestrationConfig {
    pub bearer_token: String,
    pub allowlist: WorkspaceAllowlist,
    pub max_concurrent_runs: usize,
    pub bounds: RunBounds,
}

impl Default for OrchestrationConfig {
    fn default() -> Self {
        Self {
            bearer_token: String::new(),
            allowlist: WorkspaceAllowlist::default(),
            max_concurrent_runs: 4,
            bounds: RunBounds::default(),
        }
    }
}

pub struct OrchestrationService {
    host: AgentHostHandle,
    bus: EventBus,
    store: OrchStore,
    config: Mutex<OrchestrationConfig>,
    self_ref: Weak<OrchestrationService>,
    pending_admissions: Mutex<AdmissionQueueState>,
    scheduler_watcher: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Join handles for in-flight runs (prevents forget + unbounded leaks).
    join_handles: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl Drop for OrchestrationService {
    fn drop(&mut self) {
        if let Some(watcher) = self.scheduler_watcher.get_mut().take() {
            watcher.abort();
        }
    }
}

struct AdmissionGuard {
    host: AgentHostHandle,
    run_id: String,
}

impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        self.host.release_orchestration_turn(&self.run_id);
    }
}

enum IdempotencyStart {
    Perform(IdempotencyLease),
    Replay(serde_json::Value),
}

struct IdempotencyLease {
    store: OrchStore,
    tool: String,
    request_id: String,
    payload_hash: String,
    settled: bool,
}

impl IdempotencyLease {
    fn complete(
        &mut self,
        run_id: Option<String>,
        response: serde_json::Value,
    ) -> Result<(), OrchError> {
        self.store.complete_idempotency(
            &self.tool,
            &self.request_id,
            &self.payload_hash,
            run_id,
            response,
        )?;
        self.settled = true;
        Ok(())
    }

    fn fail(&mut self, run_id: Option<String>, error: OrchError) -> OrchError {
        match self.store.fail_idempotency(
            &self.tool,
            &self.request_id,
            &self.payload_hash,
            run_id,
            error.clone(),
        ) {
            Ok(()) => {
                self.settled = true;
                error
            }
            Err(store_error) => store_error,
        }
    }
}

impl Drop for IdempotencyLease {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let error = OrchError::new(
            OrchErrorCode::Internal,
            "mutation abandoned before its durable outcome completed",
        );
        if self
            .store
            .fail_idempotency(
                &self.tool,
                &self.request_id,
                &self.payload_hash,
                None,
                error,
            )
            .is_ok()
        {
            self.settled = true;
        }
    }
}

impl OrchestrationService {
    pub fn new(
        host: AgentHostHandle,
        bus: EventBus,
        store: OrchStore,
        mut config: OrchestrationConfig,
    ) -> Arc<Self> {
        host.install_orchestration_store(store.clone());
        // The host owns the process-wide ledger. If desktop bootstrap opened
        // it first, use that same handle instead of creating a split history.
        let store = host.ensure_orchestration_store().unwrap_or(store);
        // Register control bearer (and any future secrets) on the *shared* host bus
        // so durable journal redaction covers the shipped desktop path.
        if !config.bearer_token.is_empty() {
            bus.add_control_secrets([config.bearer_token.clone()]);
        }
        config.max_concurrent_runs =
            host.configure_orchestration_capacity(config.max_concurrent_runs);
        let service = Arc::new_cyclic(|self_ref| Self {
            host,
            bus,
            store,
            config: Mutex::new(config),
            self_ref: self_ref.clone(),
            pending_admissions: Mutex::new(AdmissionQueueState::default()),
            scheduler_watcher: Mutex::new(None),
            join_handles: Mutex::new(Vec::new()),
        });
        service.start_scheduler_watcher();
        service
    }

    fn start_scheduler_watcher(&self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let mut events = self.host.subscribe_events();
        let service_ref = self.self_ref.clone();
        let watcher = runtime.spawn(async move {
            while let Some(update) = events.recv().await {
                if matches!(
                    update,
                    crate::events::SessionUpdate::TurnComplete { .. }
                        | crate::events::SessionUpdate::Error { .. }
                ) {
                    let Some(service) = service_ref.upgrade() else {
                        break;
                    };
                    service.pump_pending();
                }
            }
        });
        *self.scheduler_watcher.lock() = Some(watcher);
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub fn store(&self) -> &OrchStore {
        &self.store
    }

    pub fn set_token(&self, token: String) {
        if !token.is_empty() {
            self.bus.add_control_secrets([token.clone()]);
        }
        self.config.lock().bearer_token = token;
    }

    pub fn set_allowlist(&self, allowlist: WorkspaceAllowlist) {
        self.config.lock().allowlist = allowlist;
    }

    pub(crate) fn audit_transport_result(&self, tool: &str, error: Option<&OrchError>) {
        self.audit(
            tool,
            None,
            None,
            None,
            if error.is_some() {
                "rejected"
            } else {
                "accepted"
            },
            error.map(|e| e.code.as_str()),
            "mcp transport call",
        );
    }

    pub fn auth_header(&self, header: Option<&str>) -> Result<AuthContext, OrchError> {
        let tok = self.config.lock().bearer_token.clone();
        let res = super::authz::require_bearer(header, &tok);
        if let Err(ref e) = res {
            self.audit(
                "auth",
                None,
                None,
                None,
                "rejected",
                Some(e.code.as_str()),
                "auth failed",
            );
        }
        res
    }

    #[allow(clippy::too_many_arguments)]
    fn audit(
        &self,
        tool: &str,
        request_id: Option<&str>,
        session_id: Option<Uuid>,
        workspace: Option<&str>,
        outcome: &str,
        error_code: Option<&str>,
        detail: &str,
    ) {
        let entry = AuditEntry {
            ts: Utc::now(),
            tool: self.bus.redact_text(tool, 100),
            request_id: request_id.map(|value| self.bus.redact_text(value, 256)),
            session_id,
            workspace: workspace.map(|value| self.bus.redact_text(value, 1_000)),
            outcome: self.bus.redact_text(outcome, 100),
            error_code: error_code.map(|value| self.bus.redact_text(value, 100)),
            detail: self.bus.redact_text(detail, 500),
        };
        if let Err(e) = self.store.enqueue_audit(entry) {
            eprintln!("[grokptah] orchestration audit persistence failed: {e}");
        }
    }

    fn audit_err(
        &self,
        tool: &str,
        request_id: Option<&str>,
        session_id: Option<Uuid>,
        workspace: Option<&str>,
        e: &OrchError,
    ) {
        self.audit(
            tool,
            request_id,
            session_id,
            workspace,
            "rejected",
            Some(e.code.as_str()),
            &e.message,
        );
    }

    fn try_reserve_capacity(&self, run_id: &str, session_id: Uuid) -> Result<(), OrchError> {
        self.host
            .reserve_orchestration_turn(run_id, session_id)
            .map_err(|error| {
                let message = error.to_string();
                if message.contains("max concurrent") {
                    OrchError::new(OrchErrorCode::CapacityExhausted, message)
                } else {
                    OrchError::new(OrchErrorCode::SessionBusy, message)
                }
            })
    }

    fn release_capacity(&self, run_id: &str) {
        self.host.release_orchestration_turn(run_id);
        self.pump_pending();
    }

    fn pending_count(&self) -> usize {
        self.pending_admissions.lock().pending.len()
    }

    /// Keep the durable records aligned with the process-local scheduler. The
    /// prompt itself remains in memory by design; this metadata is only for
    /// honest operator/coordinator visibility while the run is pending.
    fn sync_pending_positions(&self) {
        let positions: Vec<(String, usize)> = {
            let queue = self.pending_admissions.lock();
            queue
                .pending
                .iter()
                .enumerate()
                .map(|(index, pending)| (pending.run_id.clone(), index + 1))
                .collect()
        };
        for (run_id, position) in positions {
            if let Err(error) = self.store.update_run(&run_id, |run| {
                if run.state == RunState::Queued {
                    run.queue_position = Some(position);
                    run.updated_at = Utc::now();
                }
                Ok(())
            }) {
                eprintln!("[grokptah] queued run position persistence failed: {error}");
            }
        }
    }

    fn clear_queue_position(&self, run_id: &str) {
        if let Err(error) = self.store.update_run(run_id, |run| {
            if run.queue_position.take().is_some() {
                run.updated_at = Utc::now();
            }
            Ok(())
        }) {
            eprintln!("[grokptah] queued run position clear failed: {error}");
        }
    }

    fn enqueue_pending(&self, pending: PendingRun) -> Result<usize, OrchError> {
        let mut queue = self.pending_admissions.lock();
        if queue.pending.len() >= MAX_PENDING_ADMISSIONS {
            return Err(OrchError::new(
                OrchErrorCode::CapacityExhausted,
                format!("bounded admission queue is full ({MAX_PENDING_ADMISSIONS} pending runs)"),
            ));
        }
        queue.pending.push_back(pending);
        let position = queue.pending.len();
        drop(queue);
        self.sync_pending_positions();
        Ok(position)
    }

    fn remove_pending(&self, run_id: &str) -> bool {
        let mut queue = self.pending_admissions.lock();
        let before = queue.pending.len();
        queue.pending.retain(|pending| pending.run_id != run_id);
        let removed = before != queue.pending.len();
        drop(queue);
        if removed {
            self.sync_pending_positions();
        }
        removed
    }

    /// Choose the oldest eligible task, preferring a session different from
    /// the last started one when possible. Earlier tasks from the same session
    /// remain ahead of later tasks, preventing same-session leapfrogging.
    fn next_pending_index(&self, queue: &AdmissionQueueState) -> Option<usize> {
        let eligible = queue
            .pending
            .iter()
            .enumerate()
            .filter(|(index, pending)| {
                !self.host.session_busy(pending.session_id)
                    && !queue
                        .pending
                        .range(..*index)
                        .any(|prior| prior.session_id == pending.session_id)
            })
            .collect::<Vec<_>>();
        eligible
            .iter()
            .find(|(_, pending)| Some(pending.session_id) != queue.last_started_session)
            .or_else(|| eligible.first())
            .map(|(index, _)| *index)
    }

    /// Promote as many queued tasks as the shared host capacity allows.
    fn pump_pending(&self) {
        loop {
            if self.host.orchestration_active_count() >= self.host.orchestration_capacity_limit() {
                return;
            }
            let pending = {
                let mut queue = self.pending_admissions.lock();
                let Some(index) = self.next_pending_index(&queue) else {
                    return;
                };
                let pending = queue.pending.remove(index).expect("pending index exists");
                queue.last_started_session = Some(pending.session_id);
                pending
            };
            self.clear_queue_position(&pending.run_id);
            self.sync_pending_positions();

            // Cancellation can win after the task left the queue but before
            // promotion. Treat terminal records as a normal, safe skip.
            let Some(current) = self.store.load_run(&pending.run_id).ok().flatten() else {
                continue;
            };
            if current.state != RunState::Queued {
                continue;
            }

            if let Err(error) = self.try_reserve_capacity(&pending.run_id, pending.session_id) {
                let mut queue = self.pending_admissions.lock();
                queue.pending.push_front(pending);
                drop(queue);
                self.sync_pending_positions();
                if !matches!(
                    error.code,
                    OrchErrorCode::SessionBusy | OrchErrorCode::CapacityExhausted
                ) {
                    eprintln!("[grokptah] queued run admission failed: {error}");
                }
                return;
            }

            let start_seq = self.bus.next_seq();
            let transitioned = self.store.update_run(&pending.run_id, |run| {
                if run.state != RunState::Queued {
                    anyhow::bail!("queued run is no longer pending");
                }
                run.state = RunState::Running;
                run.queue_position = None;
                run.start_seq = Some(start_seq);
                run.updated_at = Utc::now();
                Ok(())
            });
            match transitioned {
                Ok(Some(run)) => self.spawn_run(run, pending.prompt, pending.execution_mode),
                Ok(None) | Err(_) => {
                    self.host.release_orchestration_turn(&pending.run_id);
                }
            }
        }
    }

    async fn begin_idempotency(
        &self,
        tool: &str,
        request_id: &str,
        payload_hash: &str,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<IdempotencyStart, OrchError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            match self.store.claim_idempotency(tool, request_id, payload_hash) {
                Ok(IdempotencyClaim::Perform) => {
                    return Ok(IdempotencyStart::Perform(IdempotencyLease {
                        store: self.store.clone(),
                        tool: tool.into(),
                        request_id: request_id.into(),
                        payload_hash: payload_hash.into(),
                        settled: false,
                    }));
                }
                Ok(IdempotencyClaim::Replay(Ok(value))) => {
                    self.audit(
                        tool,
                        Some(request_id),
                        Some(session_id),
                        Some(&workspace.display().to_string()),
                        "replayed",
                        None,
                        "replayed successful mutation outcome",
                    );
                    return Ok(IdempotencyStart::Replay(value));
                }
                Ok(IdempotencyClaim::Replay(Err(error))) => {
                    self.audit(
                        tool,
                        Some(request_id),
                        Some(session_id),
                        Some(&workspace.display().to_string()),
                        "replayed",
                        Some(error.code.as_str()),
                        "replayed rejected mutation outcome",
                    );
                    return Err(error);
                }
                Ok(IdempotencyClaim::Pending) => {
                    if tokio::time::Instant::now() >= deadline {
                        let error = OrchError::new(
                            OrchErrorCode::Conflict,
                            "matching request_id is still in progress",
                        );
                        self.audit_err(
                            tool,
                            Some(request_id),
                            Some(session_id),
                            Some(&workspace.display().to_string()),
                            &error,
                        );
                        return Err(error);
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => {
                    self.audit_err(
                        tool,
                        Some(request_id),
                        Some(session_id),
                        Some(&workspace.display().to_string()),
                        &error,
                    );
                    return Err(error);
                }
            }
        }
    }

    fn fail_claim(
        &self,
        lease: &mut IdempotencyLease,
        run_id: Option<String>,
        session_id: Uuid,
        workspace: &Path,
        error: OrchError,
    ) -> OrchError {
        self.audit_err(
            &lease.tool,
            Some(&lease.request_id),
            Some(session_id),
            Some(&workspace.display().to_string()),
            &error,
        );
        lease.fail(run_id, error)
    }

    fn reaping_handles(&self) {
        let mut h = self.join_handles.lock();
        h.retain(|j| !j.is_finished());
    }

    /// Load run and verify workspace ownership against allowlist + session.
    fn load_authorized_run(&self, run_id: &str) -> Result<RunRecord, OrchError> {
        if safe_id_filename(run_id).is_err() {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "malformed run_id",
            ));
        }
        let run = self
            .store
            .load_run(run_id)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::InvalidRequest, "unknown run_id"))?;
        let allowlist = self.config.lock().allowlist.clone();
        let ws = PathBuf::from(&run.workspace);
        if !allowlist.contains(&ws) {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "run workspace not authorized",
            ));
        }
        // Session must still match claimed workspace when present.
        if let Ok(session) = self.host.session_load(run.session_id) {
            if !session.cwd.is_empty() {
                let _ = require_workspace_match(&allowlist, Some(Path::new(&session.cwd)), &ws)
                    .map_err(|_| {
                        OrchError::new(
                            OrchErrorCode::ForbiddenScope,
                            "run session workspace mismatch",
                        )
                    })?;
            }
        }
        Ok(run)
    }

    // ── reads ──────────────────────────────────────────────────────────

    pub fn list_sessions(&self, _auth: &AuthContext) -> Result<serde_json::Value, OrchError> {
        let allowlist = self.config.lock().allowlist.clone();
        let sessions = self.host.list_sessions_by_kind(SessionKind::Build, false);
        let rows: Vec<serde_json::Value> = sessions
            .into_iter()
            .filter(|s| {
                if s.cwd.is_empty() {
                    return false;
                }
                allowlist.contains(Path::new(&s.cwd))
            })
            .map(|s| {
                let busy = self.host.session_busy(s.id);
                json!({
                    "sessionId": s.id,
                    "title": s.title,
                    "kind": "build",
                    "cwd": s.cwd,
                    "updatedAt": s.updated_at,
                    "busy": busy,
                })
            })
            .collect();
        Ok(json!({ "sessions": rows }))
    }

    pub fn get_capacity(&self, _auth: &AuthContext) -> Result<serde_json::Value, OrchError> {
        let max = self.host.orchestration_capacity_limit();
        let active = self.host.orchestration_active_count();
        let queued = self.pending_admissions.lock().pending.len();
        let event_error = self
            .bus
            .last_persistence_error()
            .map(|error| self.bus.redact_text(&error, 500));
        let audit_error = self
            .store
            .last_audit_error()
            .map(|error| self.bus.redact_text(&error, 500));
        let run_error = self
            .store
            .last_run_error()
            .map(|error| self.bus.redact_text(&error, 500));
        Ok(json!({
            "maxConcurrentRuns": max,
            "activeRuns": active,
            "available": max.saturating_sub(active),
            "queuedRuns": queued,
            "queueLimit": MAX_PENDING_ADMISSIONS,
            "health": {
                "laggedLiveEvents": self.bus.lagged_event_count(),
                "eventJournalPersistenceError": event_error,
                "auditPersistenceError": audit_error,
                "runPersistenceError": run_error,
            },
        }))
    }

    pub fn get_run(
        &self,
        _auth: &AuthContext,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let run = self.load_authorized_run(run_id)?;
        serde_json::to_value(run)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))
    }

    pub fn get_progress(
        &self,
        _auth: &AuthContext,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let run = self.load_authorized_run(run_id)?;
        let busy = self.host.session_busy(run.session_id);
        Ok(json!({
            "runId": run.run_id,
            "sessionId": run.session_id,
            "state": run.state,
            "queuePosition": run.queue_position,
            "busy": busy,
            "startSeq": run.start_seq,
            "endSeq": run.end_seq,
            "promptPreview": run.prompt_preview,
            "progress": run.progress,
            "createdAt": run.created_at,
            "updatedAt": run.updated_at,
            "terminalResult": run.terminal_result,
            "errorCode": run.error_code,
        }))
    }

    pub fn get_events(
        &self,
        _auth: &AuthContext,
        run_id: Option<&str>,
        after_seq: u64,
        limit: usize,
    ) -> Result<serde_json::Value, OrchError> {
        // run_id is required — never fall back to the global journal.
        let rid = run_id.ok_or_else(|| {
            OrchError::new(
                OrchErrorCode::InvalidRequest,
                "run_id is required for get_events",
            )
        })?;
        let run = self.load_authorized_run(rid)?;
        let mut page = self.bus.read_after(after_seq, limit);
        page.entries.retain(|e| {
            session_id_of(&e.update) == Some(run.session_id)
                && run.start_seq.map(|s| e.seq >= s).unwrap_or(true)
                && run.end_seq.map(|s| e.seq <= s).unwrap_or(true)
        });
        if page.cursor_expired {
            return Err(OrchError::new(
                OrchErrorCode::CursorExpired,
                "event cursor expired; restart from seq 0 or latest",
            ));
        }
        Ok(json!({
            "entries": page.entries,
            "nextCursor": page.next_cursor,
            "cursorExpired": false,
        }))
    }

    pub fn get_changes(
        &self,
        _auth: &AuthContext,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let run = self.load_authorized_run(run_id)?;
        // Prefer durable aggregates (survive journal rollover).
        let mut paths: Vec<serde_json::Value> = run
            .aggregates
            .changes
            .iter()
            .map(|c| json!({ "path": c.path, "summary": c.summary }))
            .collect();
        if let Ok(entries) = self.scoped_events_complete(&run) {
            for e in entries {
                if let crate::events::SessionUpdate::FileEdit { path, summary, .. } = e.update {
                    if !paths.iter().any(|p| p["path"] == path) {
                        paths.push(json!({ "path": path, "summary": summary }));
                    }
                }
            }
        }
        Ok(json!({ "runId": run_id, "changes": paths }))
    }

    pub fn get_test_results(
        &self,
        _auth: &AuthContext,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let run = self.load_authorized_run(run_id)?;
        let mut by_id: std::collections::HashMap<String, serde_json::Value> =
            std::collections::HashMap::new();
        // Seed from durable aggregates.
        for t in &run.aggregates.tests {
            by_id.insert(
                t.call_id.clone(),
                json!({
                    "callId": t.call_id,
                    "command": t.command,
                    "status": t.status,
                    "exitCode": t.exit_code,
                    "cancelled": t.cancelled,
                }),
            );
        }
        if let Ok(entries) = self.scoped_events_complete(&run) {
            for e in entries {
                match e.update {
                    crate::events::SessionUpdate::ShellSessionStarted {
                        command, call_id, ..
                    } => {
                        if is_recognized_test_command(&command) {
                            by_id.insert(
                                call_id.clone(),
                                json!({
                                    "callId": call_id,
                                    "command": command,
                                    "status": "started",
                                }),
                            );
                        }
                    }
                    crate::events::SessionUpdate::ShellSessionEnded {
                        call_id,
                        exit_code,
                        cancelled,
                        ..
                    } => {
                        if let Some(prev) = by_id.get_mut(&call_id) {
                            prev["status"] = json!("ended");
                            prev["exitCode"] = json!(exit_code);
                            prev["cancelled"] = json!(cancelled);
                        }
                        // Do NOT record non-test shell ends.
                    }
                    _ => {}
                }
            }
        }
        let observed: Vec<_> = by_id.into_values().collect();
        if observed.is_empty() {
            Ok(json!({
                "runId": run_id,
                "status": "not_observed",
                "results": [],
            }))
        } else {
            Ok(json!({
                "runId": run_id,
                "status": "observed",
                "results": observed,
            }))
        }
    }

    pub fn get_handoff(
        &self,
        _auth: &AuthContext,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let run = self.load_authorized_run(run_id)?;
        Ok(json!({
            "runId": run.run_id,
            "sessionId": run.session_id,
            "state": run.state,
            "finalResponse": run.final_response,
            "terminalResult": run.terminal_result,
            "startSeq": run.start_seq,
            "endSeq": run.end_seq,
            "changes": run.aggregates.changes,
            "tests": run.aggregates.tests,
            "verification": run.aggregates.verification,
            "usage": run.aggregates.usage,
        }))
    }

    fn scoped_events_complete(
        &self,
        run: &RunRecord,
    ) -> Result<Vec<crate::event_bus::JournalEntry>, OrchError> {
        let after = run.start_seq.map(|s| s.saturating_sub(1)).unwrap_or(0);
        match self
            .bus
            .read_range_all(after, run.end_seq, Some(run.session_id))
        {
            Ok(v) => Ok(v),
            Err(CursorExpiredError) => Err(OrchError::new(
                OrchErrorCode::CursorExpired,
                "event cursor expired for run range",
            )),
        }
    }

    fn require_build_session(
        &self,
        session_id: Uuid,
    ) -> Result<crate::session::SessionSummary, OrchError> {
        let session = self
            .host
            .session_load(session_id)
            .map_err(|_| OrchError::new(OrchErrorCode::InvalidRequest, "unknown session"))?;
        if session.kind != SessionKind::Build {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "only Build sessions are controllable in this slice",
            ));
        }
        Ok(session)
    }

    fn authorize_run_request(
        &self,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<RunRecord, OrchError> {
        let run = self.load_authorized_run(run_id)?;
        if run.session_id != session_id {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "run does not belong to the requested session",
            ));
        }
        let session = self.require_build_session(session_id)?;
        let cwd = (!session.cwd.is_empty()).then(|| PathBuf::from(&session.cwd));
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = require_workspace_match(&allowlist, cwd.as_deref(), workspace)?;
        if claimed.display().to_string() != run.workspace {
            return Err(OrchError::new(
                OrchErrorCode::WorkspaceMismatch,
                "run workspace does not match the requested workspace",
            ));
        }
        Ok(run)
    }

    fn isolated_review(
        &self,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<(RunRecord, crate::run_promotion::RunReview), OrchError> {
        let run = self.authorize_run_request(session_id, workspace, run_id)?;
        if run.state != RunState::Completed {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "only completed runs can be reviewed",
            ));
        }
        let Some(execution) = run.execution.as_ref() else {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "run used shared execution and has no isolated diff",
            ));
        };
        if execution.mode != RunExecutionMode::IsolatedWorktree
            || execution.promotion_state != PromotionState::Ready
        {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "isolated run is not ready for review",
            ));
        }
        let review = self
            .host
            .inspect_run(session_id, run_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Conflict, error.to_string()))?;
        if execution.final_fingerprint.as_deref() != Some(review.fingerprint.as_str()) {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "isolated worktree fingerprint changed; review is stale",
            ));
        }
        Ok((run, review))
    }

    pub fn review_run(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let (run, review) = self.isolated_review(session_id, workspace, run_id)?;
        Ok(json!({
            "runId": run.run_id,
            "sessionId": run.session_id,
            "sourceFingerprint": run.execution.as_ref().map(|e| e.source_fingerprint.clone()),
            "finalFingerprint": review.fingerprint,
            "changedFiles": review.changed_files,
            "diff": review.diff,
            "diffTruncated": review.diff_truncated,
            "promotionState": run.execution.as_ref().map(|e| e.promotion_state),
        }))
    }

    #[allow(clippy::too_many_arguments)] // Keeps the approval scope explicit at the control boundary.
    pub async fn approve_run(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
        source_fingerprint: String,
        final_fingerprint: String,
        changed_files: Vec<ChangeRecord>,
        ttl_ms: Option<u64>,
    ) -> Result<serde_json::Value, OrchError> {
        const DEFAULT_TTL_MS: u64 = 5 * 60 * 1_000;
        const MAX_TTL_MS: u64 = 15 * 60 * 1_000;
        let tool = "ptah_approve_run";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "runId": run_id,
            "sourceFingerprint": source_fingerprint,
            "finalFingerprint": final_fingerprint,
            "changedFiles": changed_files,
            "ttlMs": ttl_ms,
        });
        let phash = hash_payload(&payload);
        let fail = |svc: &Self, error: OrchError| {
            svc.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &error,
            );
            error
        };
        let ttl = ttl_ms.unwrap_or(DEFAULT_TTL_MS);
        if ttl == 0 || ttl > MAX_TTL_MS {
            return Err(fail(
                self,
                OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    "ttl_ms must be between 1 and 900000",
                ),
            ));
        }
        let run = match self.authorize_run_request(session_id, workspace, run_id) {
            Ok(run) => run,
            Err(error) => return Err(fail(self, error)),
        };
        let mut lease = match self
            .begin_idempotency(
                tool,
                request_id,
                &phash,
                session_id,
                Path::new(&run.workspace),
            )
            .await
        {
            Ok(IdempotencyStart::Replay(value)) => return Ok(value),
            Ok(IdempotencyStart::Perform(lease)) => lease,
            Err(error) => return Err(error),
        };
        let (run, review) = match self.isolated_review(session_id, workspace, run_id) {
            Ok(value) => value,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    Some(run_id.to_string()),
                    session_id,
                    Path::new(&run.workspace),
                    error,
                ))
            }
        };
        let Some(execution) = run.execution.as_ref() else {
            unreachable!("isolated_review guarantees execution");
        };
        if source_fingerprint != execution.source_fingerprint
            || final_fingerprint != review.fingerprint
            || changed_files != review.changed_files
        {
            return Err(self.fail_claim(
                &mut lease,
                Some(run_id.to_string()),
                session_id,
                Path::new(&run.workspace),
                OrchError::new(
                    OrchErrorCode::Conflict,
                    "approval scope does not match the current reviewed diff",
                ),
            ));
        }
        if let Some(existing) = run.approval.as_ref() {
            if existing.expires_at > Utc::now() {
                let error = OrchError::new(
                    OrchErrorCode::Conflict,
                    "an unexpired approval already exists for this run",
                );
                return Err(self.fail_claim(
                    &mut lease,
                    Some(run_id.to_string()),
                    session_id,
                    Path::new(&run.workspace),
                    error,
                ));
            }
        }
        let issued_at = Utc::now();
        let approval = RunApproval {
            approval_id: Uuid::new_v4().to_string(),
            run_id: run_id.to_string(),
            session_id,
            workspace: run.workspace.clone(),
            source_fingerprint,
            final_fingerprint,
            changed_files,
            issued_at,
            expires_at: issued_at + chrono::Duration::milliseconds(ttl as i64),
        };
        let response = json!({
            "runId": run_id,
            "sessionId": session_id,
            "approvalId": approval.approval_id,
            "expiresAt": approval.expires_at,
            "sourceFingerprint": approval.source_fingerprint,
            "finalFingerprint": approval.final_fingerprint,
            "changedFiles": approval.changed_files,
        });
        let updated = self.store.update_run(run_id, |current| {
            current.approval = Some(approval.clone());
            current.updated_at = Utc::now();
            Ok(())
        });
        let updated = match updated {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(anyhow::anyhow!("run disappeared while approving")),
            Err(error) => Err(error),
        };
        if let Err(error) = updated {
            return Err(self.fail_claim(
                &mut lease,
                Some(run_id.to_string()),
                session_id,
                Path::new(&run.workspace),
                OrchError::new(OrchErrorCode::Internal, error.to_string()),
            ));
        }
        if let Err(error) = lease.complete(Some(run_id.to_string()), response.clone()) {
            return Err(self.fail_claim(
                &mut lease,
                Some(run_id.to_string()),
                session_id,
                Path::new(&run.workspace),
                error,
            ));
        }
        Ok(response)
    }

    pub async fn promote_run(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
        approval_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_promote_run";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "runId": run_id,
            "approvalId": approval_id,
        });
        let phash = hash_payload(&payload);
        let run = self.authorize_run_request(session_id, workspace, run_id)?;
        let mut lease = match self
            .begin_idempotency(
                tool,
                request_id,
                &phash,
                session_id,
                Path::new(&run.workspace),
            )
            .await?
        {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        let promoted =
            match self
                .host
                .promote_run_with_approval(session_id, run_id, Some(approval_id))
            {
                Ok(run) => run,
                Err(error) => {
                    return Err(self.fail_claim(
                        &mut lease,
                        Some(run_id.to_string()),
                        session_id,
                        Path::new(&run.workspace),
                        OrchError::new(OrchErrorCode::Conflict, error.to_string()),
                    ))
                }
            };
        let response = serde_json::to_value(promoted)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        lease.complete(Some(run_id.to_string()), response.clone())?;
        Ok(response)
    }

    pub async fn discard_run(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_discard_run";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "runId": run_id,
        });
        let phash = hash_payload(&payload);
        let run = self.authorize_run_request(session_id, workspace, run_id)?;
        if !run.state.is_terminal() {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "only terminal runs can be discarded",
            ));
        }
        let mut lease = match self
            .begin_idempotency(
                tool,
                request_id,
                &phash,
                session_id,
                Path::new(&run.workspace),
            )
            .await?
        {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        let discarded = match self.host.discard_run(session_id, run_id) {
            Ok(run) => run,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    Some(run_id.to_string()),
                    session_id,
                    Path::new(&run.workspace),
                    OrchError::new(OrchErrorCode::Conflict, error.to_string()),
                ))
            }
        };
        let response = serde_json::to_value(discarded)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        lease.complete(Some(run_id.to_string()), response.clone())?;
        Ok(response)
    }

    // ── mutations ──────────────────────────────────────────────────────

    pub async fn submit_task(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        prompt: String,
        bounds_json: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, OrchError> {
        self.submit_task_with_execution_mode(
            auth,
            request_id,
            session_id,
            workspace,
            prompt,
            bounds_json,
            RunExecutionMode::Shared,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)] // Keeps bounded submission policy explicit at the control boundary.
    pub async fn submit_task_with_execution_mode(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        prompt: String,
        bounds_json: Option<serde_json::Value>,
        execution_mode: RunExecutionMode,
    ) -> Result<serde_json::Value, OrchError> {
        self.submit_task_with_execution_mode_and_queue(
            auth,
            request_id,
            session_id,
            workspace,
            prompt,
            bounds_json,
            execution_mode,
            false,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn submit_task_with_execution_mode_and_queue(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        prompt: String,
        bounds_json: Option<serde_json::Value>,
        execution_mode: RunExecutionMode,
        allow_queue: bool,
    ) -> Result<serde_json::Value, OrchError> {
        let _ = auth;
        let tool = "ptah_submit_task";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "prompt": prompt,
            "bounds": bounds_json,
            "executionMode": execution_mode,
            "allowQueue": allow_queue,
        });
        let phash = hash_payload(&payload);

        let finish_err = |svc: &Self, e: OrchError| -> OrchError {
            svc.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &e,
            );
            e
        };

        if let Err(e) = reject_control_prompt(&prompt) {
            return Err(finish_err(self, e));
        }
        let ceiling = self.config.lock().bounds.clone();
        let bounds = match merge_bounds(&ceiling, bounds_json.as_ref()) {
            Ok(b) => b,
            Err(e) => return Err(finish_err(self, e)),
        };
        if prompt.len() > bounds.max_prompt_bytes {
            let e = OrchError::new(
                OrchErrorCode::InvalidRequest,
                format!(
                    "prompt exceeds max_prompt_bytes ({})",
                    bounds.max_prompt_bytes
                ),
            );
            return Err(finish_err(self, e));
        }

        let session = match self.require_build_session(session_id) {
            Ok(s) => s,
            Err(e) => return Err(finish_err(self, e)),
        };
        let cwd = if session.cwd.is_empty() {
            None
        } else {
            Some(PathBuf::from(&session.cwd))
        };
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = match require_workspace_match(&allowlist, cwd.as_deref(), workspace) {
            Ok(c) => c,
            Err(e) => return Err(finish_err(self, e)),
        };

        let mut lease = match self
            .begin_idempotency(tool, request_id, &phash, session_id, &claimed)
            .await?
        {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };

        // Give older queued work first claim on any newly available capacity.
        self.pump_pending();
        let run_id = Uuid::new_v4().to_string();
        let queue_ahead = self.pending_count() > 0;
        let mut queued = false;
        if allow_queue && queue_ahead {
            queued = true;
        } else if let Err(e) = self.try_reserve_capacity(&run_id, session_id) {
            if allow_queue
                && matches!(
                    e.code,
                    OrchErrorCode::SessionBusy | OrchErrorCode::CapacityExhausted
                )
            {
                queued = true;
            } else {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, e));
            }
        }
        let start_seq = (!queued).then(|| self.bus.next_seq());
        let run = RunRecord {
            run_id: run_id.clone(),
            session_id,
            workspace: claimed.display().to_string(),
            request_id: request_id.into(),
            // Distinguish coordinator-owned work from desktop turns so the
            // desktop can surface external activity without guessing from
            // transport timing.
            client_id: Some("mcp".into()),
            state: if queued {
                RunState::Queued
            } else {
                RunState::Running
            },
            queue_position: None,
            bounds: bounds.clone(),
            prompt_preview: self.bus.redact_text(&prompt_preview(&prompt), 500),
            start_seq,
            end_seq: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            terminal_result: None,
            final_response: None,
            error_code: None,
            aggregates: RunAggregates::default(),
            progress: None,
            execution: None,
            approval: None,
        };
        if let Err(e) = self.store.save_run(&run) {
            if !queued {
                self.host.release_turn_reservation(session_id, &run_id);
                self.release_capacity(&run_id);
            }
            let e = OrchError::new(OrchErrorCode::Internal, e.to_string());
            return Err(self.fail_claim(&mut lease, Some(run_id), session_id, &claimed, e));
        }

        let queued_position = if queued {
            match self.enqueue_pending(PendingRun {
                run_id: run_id.clone(),
                session_id,
                prompt: prompt.clone(),
                execution_mode,
            }) {
                Ok(position) => Some(position),
                Err(error) => {
                    let _ = self.store.update_run(&run_id, |current| {
                        current.state = RunState::Failed;
                        current.queue_position = None;
                        current.terminal_result = Some("failed".into());
                        current.error_code = Some(error.code.as_str().into());
                        current.updated_at = Utc::now();
                        Ok(())
                    });
                    return Err(self.fail_claim(
                        &mut lease,
                        Some(run_id),
                        session_id,
                        &claimed,
                        error,
                    ));
                }
            }
        } else {
            None
        };

        let response = json!({
            "runId": run_id,
            "sessionId": session_id,
            "state": run.state,
            "requestId": request_id,
            "executionMode": execution_mode,
            "queuedPosition": queued_position,
        });
        if let Err(e) = lease.complete(Some(run_id.clone()), response.clone()) {
            let _ = self.store.update_run(&run_id, |r| {
                r.state = RunState::Failed;
                r.terminal_result = Some("failed".into());
                r.error_code = Some("receipt_persistence_failed".into());
                r.updated_at = Utc::now();
                Ok(())
            });
            self.remove_pending(&run_id);
            if !queued {
                self.host.release_turn_reservation(session_id, &run_id);
                self.release_capacity(&run_id);
            }
            return Err(self.fail_claim(&mut lease, Some(run_id), session_id, &claimed, e));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            if queued { "run queued" } else { "run started" },
        );
        if !queued {
            self.spawn_run(run, prompt, execution_mode);
        } else {
            // A capacity release can race the enqueue; this also makes an
            // immediately available slot visible without requiring polling.
            self.pump_pending();
        }

        Ok(response)
    }

    /// Start a run whose host admission has already been reserved.
    fn spawn_run(&self, run: RunRecord, prompt: String, execution_mode: RunExecutionMode) {
        let host = self.host.clone();
        let store = self.store.clone();
        let bus = self.bus.clone();
        let service_ref = self.self_ref.clone();
        let session_id = run.session_id;
        let rid = run.run_id.clone();
        let max_ms = run.bounds.max_duration_ms;
        let max_rounds = run.bounds.max_rounds;

        // Dedicated aggregator task: must not share a biased select with the
        // duration deadline (chatty ShellOutput must not starve max_duration_ms).
        let mut agg_rx = bus.subscribe();
        let store_agg = store.clone();
        let rid_agg = rid.clone();
        let agg_task = tokio::spawn(async move {
            while let Some(update) = agg_rx.recv().await {
                apply_run_aggregate(&store_agg, &rid_agg, session_id, &update);
            }
        });

        let join = tokio::spawn(async move {
            let admission_guard = AdmissionGuard {
                host: host.clone(),
                run_id: rid.clone(),
            };
            let prompt_fut = host.session_prompt_reserved_with_max_rounds_for_run(
                session_id,
                prompt,
                Some(max_rounds.max(1)),
                &rid,
                &rid,
                execution_mode,
            );
            tokio::pin!(prompt_fut);
            let deadline = tokio::time::sleep(Duration::from_millis(max_ms.max(1)));
            tokio::pin!(deadline);

            // Cancellation and teardown are bounded. A backend that ignores its
            // token cannot hold admission capacity forever.
            let (timed_out, result): (bool, Result<String, anyhow::Error>) = tokio::select! {
                biased;
                _ = &mut deadline => {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(5),
                        host.cancel_turn_and_await(Some(session_id)),
                    ).await;
                    let settled = tokio::time::timeout(
                        Duration::from_secs(1),
                        &mut prompt_fut,
                    ).await;
                    let result = match settled {
                        Ok(result) => result,
                        Err(_) => Err(anyhow::anyhow!(
                            "turn did not stop within the teardown deadline"
                        )),
                    };
                    (true, result)
                }
                r = &mut prompt_fut => (false, r),
            };

            // Stop aggregator; then reconcile aggregates from the journal range
            // so late FileEdit/test events are not lost if the task was aborted mid-drain.
            agg_task.abort();
            let _ = agg_task.await;

            let end_seq = bus.current_seq();
            let reconciliation = collect_run_updates(&bus, &store, &rid, end_seq);
            let durable_result = match &result {
                Ok(text) => Ok(bus.redact_text(text, 8_000)),
                Err(error) => Err(bus.redact_text(&error.to_string(), 2_000)),
            };
            let incomplete_stop = result
                .as_ref()
                .is_ok_and(|text| crate::host_helpers::is_incomplete_stop_message(text));
            let mut candidate = store.load_run(&rid).ok().flatten().unwrap_or(run);
            for update in &reconciliation {
                fold_run_update(&mut candidate, update);
            }
            candidate.end_seq = candidate.end_seq.or(Some(end_seq));
            candidate.updated_at = Utc::now();
            if !candidate.state.is_terminal() {
                if timed_out {
                    candidate.state = RunState::LimitReached;
                    candidate.terminal_result = Some("limit_reached".into());
                    candidate.error_code = Some("limit_reached".into());
                    if let Ok(text) = &durable_result {
                        candidate.final_response = Some(text.clone());
                    }
                } else {
                    match &durable_result {
                        Ok(text) => {
                            if incomplete_stop {
                                candidate.state = RunState::LimitReached;
                                candidate.terminal_result = Some("limit_reached".into());
                                candidate.error_code = Some("limit_reached".into());
                            } else {
                                candidate.state = RunState::Completed;
                                candidate.terminal_result = Some("completed".into());
                            }
                            candidate.final_response = Some(text.clone());
                        }
                        Err(error) => {
                            candidate.state = RunState::Failed;
                            candidate.terminal_result = Some("failed".into());
                            candidate.error_code = Some("internal".into());
                            candidate.final_response = Some(error.clone());
                        }
                    }
                }
            }
            if candidate.aggregates.verification.is_none() {
                let observations = crate::completion::observations_from_run(
                    candidate.aggregates.changes.len(),
                    candidate
                        .aggregates
                        .tests
                        .iter()
                        .map(|t| (t.exit_code, t.cancelled)),
                    candidate.aggregates.permissions_requested,
                    candidate.aggregates.permissions_granted,
                    candidate.aggregates.permissions_denied,
                );
                let outcome = candidate.terminal_result.as_deref().unwrap_or("incomplete");
                candidate.aggregates.verification = Some(crate::completion::build_evidence(
                    outcome,
                    candidate.final_response.as_deref(),
                    observations,
                    candidate.aggregates.usage.clone(),
                    matches!(candidate.state, RunState::Cancelled | RunState::Interrupted),
                ));
            }
            // External isolated runs do not pass through the desktop finalizer.
            if let Some(execution) = candidate.execution.as_mut() {
                if execution.mode == RunExecutionMode::IsolatedWorktree {
                    if candidate.state == RunState::Completed {
                        match crate::run_promotion::snapshot(
                            Path::new(&execution.execution_workspace),
                            &execution.base_revision,
                        ) {
                            Ok(snapshot) => {
                                execution.final_fingerprint = Some(snapshot.fingerprint);
                                execution.promotion_state = PromotionState::Ready;
                                if !snapshot.changed_files.is_empty() {
                                    candidate.aggregates.changes = snapshot.changed_files;
                                }
                            }
                            Err(error) => {
                                execution.promotion_state = PromotionState::Conflicted;
                                candidate.error_code = Some("promotion_conflict".into());
                                let _ = store.enqueue_audit(AuditEntry {
                                    ts: Utc::now(),
                                    tool: "run_finalization".into(),
                                    request_id: None,
                                    session_id: Some(session_id),
                                    workspace: Some(candidate.workspace.clone()),
                                    outcome: "promotion_conflict".into(),
                                    error_code: Some("promotion_conflict".into()),
                                    detail: bus.redact_text(&error.to_string(), 500),
                                });
                            }
                        }
                    } else {
                        execution.promotion_state = PromotionState::Conflicted;
                    }
                }
            }
            let mut attempt = 0u32;
            loop {
                let error = match store.persist_finalization(&candidate) {
                    Ok(_) => break,
                    Err(error) => error.to_string(),
                };
                if attempt == 0 {
                    let entry = AuditEntry {
                        ts: Utc::now(),
                        tool: "run_finalization".into(),
                        request_id: None,
                        session_id: Some(session_id),
                        workspace: None,
                        outcome: "retrying".into(),
                        error_code: Some("run_persistence_failed".into()),
                        detail: bus.redact_text(&error, 500),
                    };
                    let _ = store.enqueue_audit(entry);
                    eprintln!("[grokptah] run {rid} finalization retrying: {error}");
                }
                attempt = attempt.saturating_add(1);
                let shift = attempt.min(6);
                let backoff_ms = 25u64.saturating_mul(1u64 << shift).min(1_000);
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
            }

            // Release capacity before waking the scheduler, so a queued task
            // can be promoted immediately and fairly.
            drop(admission_guard);
            if let Some(service) = service_ref.upgrade() {
                service.pump_pending();
            }
        });
        self.reaping_handles();
        self.join_handles.lock().push(join);
    }

    pub async fn queue_prompt(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        prompt: String,
        priority: bool,
    ) -> Result<serde_json::Value, OrchError> {
        let _ = auth;
        let tool = "ptah_queue_prompt";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "prompt": prompt,
            "priority": priority,
        });
        let phash = hash_payload(&payload);
        let fail = |svc: &Self, e: OrchError| {
            svc.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &e,
            );
            e
        };
        if let Err(e) = reject_control_prompt(&prompt) {
            return Err(fail(self, e));
        }
        if let Err(e) = self.require_build_session(session_id) {
            return Err(fail(self, e));
        }
        let session = self.host.session_load(session_id).unwrap();
        let cwd = if session.cwd.is_empty() {
            None
        } else {
            Some(PathBuf::from(&session.cwd))
        };
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = match require_workspace_match(&allowlist, cwd.as_deref(), workspace) {
            Ok(c) => c,
            Err(e) => return Err(fail(self, e)),
        };

        let mut lease = match self
            .begin_idempotency(tool, request_id, &phash, session_id, &claimed)
            .await?
        {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };

        let entries = match self.host.session_queue_add_with_source(
            session_id,
            prompt,
            priority,
            "control",
            Some("mcp".into()),
        ) {
            Ok(e) => e,
            Err(e) => {
                return Err(self.fail_claim(
                    &mut lease,
                    None,
                    session_id,
                    &claimed,
                    OrchError::new(OrchErrorCode::Internal, e.to_string()),
                ));
            }
        };
        let response = json!({
            "requestId": request_id,
            "sessionId": session_id,
            "entries": entries,
        });
        if let Err(e) = lease.complete(None, response.clone()) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, e));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "queued",
        );
        Ok(response)
    }

    pub async fn steer(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        text: String,
    ) -> Result<serde_json::Value, OrchError> {
        let _ = auth;
        let tool = "ptah_steer";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "text": text,
        });
        let phash = hash_payload(&payload);
        let fail = |svc: &Self, e: OrchError| {
            svc.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &e,
            );
            e
        };
        if let Err(e) = reject_control_prompt(&text) {
            return Err(fail(self, e));
        }
        if let Err(e) = self.require_build_session(session_id) {
            return Err(fail(self, e));
        }
        let session = self.host.session_load(session_id).unwrap();
        let cwd = if session.cwd.is_empty() {
            None
        } else {
            Some(PathBuf::from(&session.cwd))
        };
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = match require_workspace_match(&allowlist, cwd.as_deref(), workspace) {
            Ok(c) => c,
            Err(e) => return Err(fail(self, e)),
        };

        let mut lease = match self
            .begin_idempotency(tool, request_id, &phash, session_id, &claimed)
            .await?
        {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };

        let receipt = match self
            .host
            .session_steer_with_owner(session_id, text, Some("mcp".into()))
        {
            Ok(r) => r,
            Err(e) => {
                return Err(self.fail_claim(
                    &mut lease,
                    None,
                    session_id,
                    &claimed,
                    OrchError::new(OrchErrorCode::Internal, e.to_string()),
                ));
            }
        };
        let response = json!({
            "requestId": request_id,
            "sessionId": session_id,
            "disposition": receipt.disposition,
            "entry": receipt.entry,
            "entries": receipt.entries,
        });
        if let Err(e) = lease.complete(None, response.clone()) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, e));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "steer",
        );
        Ok(response)
    }

    pub async fn cancel(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        run_id: Option<&str>,
    ) -> Result<serde_json::Value, OrchError> {
        let _ = auth;
        let tool = "ptah_cancel";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "runId": run_id,
        });
        let phash = hash_payload(&payload);
        let fail = |svc: &Self, e: OrchError| {
            svc.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &e,
            );
            e
        };

        let rid = match run_id {
            Some(r) if !r.is_empty() => r,
            _ => {
                return Err(fail(
                    self,
                    OrchError::new(
                        OrchErrorCode::InvalidRequest,
                        "run_id is required for cancel",
                    ),
                ));
            }
        };

        let run = match self.store.load_run(rid) {
            Ok(Some(r)) => r,
            Ok(None) => {
                return Err(fail(
                    self,
                    OrchError::new(OrchErrorCode::InvalidRequest, "unknown run_id"),
                ));
            }
            Err(e) => {
                return Err(fail(
                    self,
                    OrchError::new(OrchErrorCode::Internal, e.to_string()),
                ));
            }
        };

        if run.session_id != session_id {
            return Err(fail(
                self,
                OrchError::new(
                    OrchErrorCode::ForbiddenScope,
                    "run_id does not belong to session",
                ),
            ));
        }
        let session = match self.host.session_load(session_id) {
            Ok(s) => s,
            Err(_) => {
                return Err(fail(
                    self,
                    OrchError::new(OrchErrorCode::InvalidRequest, "unknown session"),
                ));
            }
        };
        let cwd = if session.cwd.is_empty() {
            None
        } else {
            Some(PathBuf::from(&session.cwd))
        };
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = match require_workspace_match(&allowlist, cwd.as_deref(), workspace) {
            Ok(c) => c,
            Err(e) => return Err(fail(self, e)),
        };
        // Workspace must match the run record as well.
        if claimed.display().to_string() != run.workspace
            && canonical_cmp(&claimed, Path::new(&run.workspace)).is_err()
        {
            return Err(fail(
                self,
                OrchError::new(
                    OrchErrorCode::WorkspaceMismatch,
                    "workspace does not match run",
                ),
            ));
        }

        let mut lease = match self
            .begin_idempotency(tool, request_id, &phash, session_id, &claimed)
            .await?
        {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        if run.state.is_terminal() {
            let error = OrchError::new(
                OrchErrorCode::InvalidRequest,
                format!("run already terminal ({:?})", run.state),
            );
            return Err(self.fail_claim(&mut lease, Some(rid.into()), session_id, &claimed, error));
        }

        // Persist cancelled transactionally before signalling. The closure
        // rechecks state so a concurrent completion can never be overwritten.
        let cancel_update = self.store.update_run(rid, |current| {
            if current.session_id != session_id {
                return Err(anyhow::anyhow!("run_id does not belong to session"));
            }
            if current.state.is_terminal() {
                return Err(anyhow::anyhow!(
                    "run already terminal ({:?})",
                    current.state
                ));
            }
            current.state = RunState::Cancelled;
            current.queue_position = None;
            current.updated_at = Utc::now();
            current.end_seq = None;
            current.terminal_result = Some("cancelled".into());
            Ok(())
        });
        if !matches!(cancel_update, Ok(Some(_))) {
            let message = match cancel_update {
                Ok(None) => "run record disappeared during cancel".into(),
                Err(error) => error.to_string(),
                Ok(Some(_)) => unreachable!(),
            };
            return Err(self.fail_claim(
                &mut lease,
                Some(rid.into()),
                session_id,
                &claimed,
                OrchError::new(OrchErrorCode::Internal, message),
            ));
        }

        let was_pending = self.remove_pending(rid);
        let reservation_released = self.host.release_turn_reservation(session_id, rid);
        let teardown_complete = if was_pending || reservation_released {
            true
        } else {
            tokio::time::timeout(Duration::from_secs(5), async {
                let _ = self.host.cancel_turn_and_await(Some(session_id)).await;
                self.host.wait_turn_idle(session_id).await;
            })
            .await
            .is_ok()
        };

        let response = json!({
            "requestId": request_id,
            "sessionId": session_id,
            "runId": rid,
            "cancelled": true,
            "wasQueued": was_pending,
            "teardownComplete": teardown_complete,
            "state": RunState::Cancelled,
        });
        if let Err(e) = lease.complete(Some(rid.into()), response.clone()) {
            return Err(self.fail_claim(&mut lease, Some(rid.into()), session_id, &claimed, e));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "cancelled",
        );
        if was_pending {
            self.pump_pending();
        }
        Ok(response)
    }
}

fn canonical_cmp(a: &Path, b: &Path) -> Result<(), ()> {
    let ca = dunce::canonicalize(a).map_err(|_| ())?;
    let cb = dunce::canonicalize(b).map_err(|_| ())?;
    if ca == cb {
        Ok(())
    } else {
        Err(())
    }
}

/// Incrementally persist run-scoped aggregates so journal rollover cannot erase them.
pub(crate) fn apply_run_aggregate(
    store: &OrchStore,
    run_id: &str,
    session_id: Uuid,
    update: &crate::events::SessionUpdate,
) {
    if session_id_of(update) != Some(session_id) {
        return;
    }
    if !matches!(
        update,
        crate::events::SessionUpdate::FileEdit { .. }
            | crate::events::SessionUpdate::ShellSessionStarted { .. }
            | crate::events::SessionUpdate::ShellSessionEnded { .. }
            | crate::events::SessionUpdate::AgentProgress { .. }
            | crate::events::SessionUpdate::CompletionEvidence { .. }
    ) {
        return;
    }
    let _ = store.update_run(run_id, |r| {
        if fold_run_update(r, update) {
            r.updated_at = Utc::now();
        }
        Ok(())
    });
}

fn fold_run_update(run: &mut RunRecord, update: &crate::events::SessionUpdate) -> bool {
    match update {
        crate::events::SessionUpdate::FileEdit { path, summary, .. } => {
            if run.aggregates.changes.iter().any(|c| c.path == *path) {
                return false;
            }
            run.aggregates.changes.push(ChangeRecord {
                path: path.clone(),
                summary: summary.clone(),
            });
            true
        }
        crate::events::SessionUpdate::ShellSessionStarted {
            command, call_id, ..
        } if is_recognized_test_command(command) => {
            if run.aggregates.tests.iter().any(|t| t.call_id == *call_id) {
                return false;
            }
            run.aggregates.tests.push(TestObservation {
                call_id: call_id.clone(),
                command: Some(command.clone()),
                status: "started".into(),
                exit_code: None,
                cancelled: None,
            });
            true
        }
        crate::events::SessionUpdate::ShellSessionEnded {
            call_id,
            exit_code,
            cancelled,
            ..
        } => {
            if let Some(t) = run
                .aggregates
                .tests
                .iter_mut()
                .find(|t| t.call_id == *call_id)
            {
                t.status = "ended".into();
                t.exit_code = *exit_code;
                t.cancelled = Some(*cancelled);
                true
            } else {
                false
            }
        }
        crate::events::SessionUpdate::AgentProgress {
            round,
            max_rounds,
            last_tool,
            detail,
            ..
        } => {
            run.progress = Some(RunProgress {
                round: *round,
                max_rounds: *max_rounds,
                last_tool: last_tool.clone(),
                detail: crate::textutil::truncate_at_char_boundary(detail, 2_000).to_string(),
                updated_at: Utc::now(),
            });
            true
        }
        crate::events::SessionUpdate::CompletionEvidence { evidence, .. } => {
            run.aggregates.usage = evidence.usage.clone();
            run.aggregates.permissions_requested = evidence.observations.permissions_requested;
            run.aggregates.permissions_granted = evidence.observations.permissions_granted;
            run.aggregates.permissions_denied = evidence.observations.permissions_denied;
            run.aggregates.verification = Some(evidence.clone());
            true
        }
        _ => false,
    }
}

fn collect_run_updates(
    bus: &EventBus,
    store: &OrchStore,
    run_id: &str,
    end_seq: u64,
) -> Vec<crate::events::SessionUpdate> {
    let Ok(Some(run)) = store.load_run(run_id) else {
        return Vec::new();
    };
    let after = run.start_seq.map(|s| s.saturating_sub(1)).unwrap_or(0);
    bus.read_range_all(after, Some(end_seq), Some(run.session_id))
        .map(|entries| entries.into_iter().map(|e| e.update).collect())
        .unwrap_or_default()
}

fn session_id_of(u: &crate::events::SessionUpdate) -> Option<Uuid> {
    use crate::events::SessionUpdate::*;
    match u {
        AgentMessageChunk { session_id, .. }
        | AgentThoughtChunk { session_id, .. }
        | TurnStarted { session_id, .. }
        | ToolCall { session_id, .. }
        | ToolCallUpdate { session_id, .. }
        | Plan { session_id, .. }
        | PermissionRequired { session_id, .. }
        | CompletionEvidence { session_id, .. }
        | TurnComplete { session_id, .. }
        | Error { session_id, .. }
        | SubagentSpawned { session_id, .. }
        | SubagentUpdate { session_id, .. }
        | ShellSessionStarted { session_id, .. }
        | ShellOutput { session_id, .. }
        | ShellSessionEnded { session_id, .. }
        | FileEdit { session_id, .. }
        | AgentProgress { session_id, .. }
        | RateLimited { session_id, .. }
        | SteeringInjected { session_id, .. } => Some(*session_id),
        BackgroundTask { session_id, .. } => *session_id,
    }
}
