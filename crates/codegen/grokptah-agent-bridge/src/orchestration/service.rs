//! Orchestration service: reads + bounded mutations over AgentHostHandle (#196).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use parking_lot::Mutex;
use serde_json::json;
use uuid::Uuid;

use crate::event_bus::{EventBus, JournalPage};
use crate::host::AgentHostHandle;
use crate::session::SessionKind;

use super::authz::{require_workspace_match, AuthContext, WorkspaceAllowlist};
use super::store::OrchStore;
use super::types::*;

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
    /// Active run_ids currently executing (capacity).
    active: Arc<Mutex<Vec<String>>>,
}

impl OrchestrationService {
    pub fn new(
        host: AgentHostHandle,
        bus: EventBus,
        store: OrchStore,
        config: OrchestrationConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            host,
            bus,
            store,
            config: Mutex::new(config),
            active: Arc::new(Mutex::new(Vec::new())),
        })
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub fn store(&self) -> &OrchStore {
        &self.store
    }

    pub fn set_token(&self, token: String) {
        self.config.lock().bearer_token = token;
    }

    pub fn set_allowlist(&self, allowlist: WorkspaceAllowlist) {
        self.config.lock().allowlist = allowlist;
    }

    pub fn auth_header(&self, header: Option<&str>) -> Result<AuthContext, OrchError> {
        let tok = self.config.lock().bearer_token.clone();
        super::authz::require_bearer(header, &tok)
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
            tool: tool.into(),
            request_id: request_id.map(str::to_string),
            session_id,
            workspace: workspace.map(str::to_string),
            outcome: outcome.into(),
            error_code: error_code.map(str::to_string),
            detail: detail.chars().take(500).collect(),
        };
        let _ = self.store.append_audit(&entry);
    }

    fn check_idempotency(
        &self,
        tool: &str,
        request_id: &str,
        payload: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>, OrchError> {
        let hash = hash_payload(payload);
        match self.store.load_idempotency(request_id) {
            Ok(Some(prev)) => {
                if prev.payload_hash != hash || prev.tool != tool {
                    return Err(OrchError::new(
                        OrchErrorCode::Conflict,
                        "request_id reused with different payload",
                    ));
                }
                Ok(Some(prev.response))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(OrchError::new(OrchErrorCode::Internal, e.to_string())),
        }
    }

    fn save_idempotency(
        &self,
        tool: &str,
        request_id: &str,
        payload: &serde_json::Value,
        run_id: Option<String>,
        response: serde_json::Value,
    ) -> Result<(), OrchError> {
        let receipt = IdempotencyReceipt {
            request_id: request_id.into(),
            payload_hash: hash_payload(payload),
            run_id,
            tool: tool.into(),
            response,
            created_at: Utc::now(),
        };
        self.store
            .save_idempotency(&receipt)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))
    }

    // ── reads ──────────────────────────────────────────────────────────

    pub fn list_sessions(&self, _auth: &AuthContext) -> Result<serde_json::Value, OrchError> {
        let sessions = self.host.list_sessions_by_kind(SessionKind::Build, false);
        let rows: Vec<serde_json::Value> = sessions
            .into_iter()
            .map(|s| {
                json!({
                    "sessionId": s.id,
                    "title": s.title,
                    "kind": "build",
                    "cwd": s.cwd,
                    "updatedAt": s.updated_at,
                    "busy": false,
                })
            })
            .collect();
        Ok(json!({ "sessions": rows }))
    }

    pub fn get_capacity(&self, _auth: &AuthContext) -> Result<serde_json::Value, OrchError> {
        let cfg = self.config.lock();
        let active = self.active.lock().len();
        Ok(json!({
            "maxConcurrentRuns": cfg.max_concurrent_runs,
            "activeRuns": active,
            "available": cfg.max_concurrent_runs.saturating_sub(active),
        }))
    }

    pub fn get_run(
        &self,
        _auth: &AuthContext,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let run = self
            .store
            .load_run(run_id)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::InvalidRequest, "unknown run_id"))?;
        serde_json::to_value(run)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))
    }

    pub fn get_progress(
        &self,
        _auth: &AuthContext,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let run = self
            .store
            .load_run(run_id)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::InvalidRequest, "unknown run_id"))?;
        let busy = self.host.session_busy(run.session_id);
        Ok(json!({
            "runId": run.run_id,
            "sessionId": run.session_id,
            "state": run.state,
            "busy": busy,
            "startSeq": run.start_seq,
            "endSeq": run.end_seq,
            "promptPreview": run.prompt_preview,
        }))
    }

    pub fn get_events(
        &self,
        _auth: &AuthContext,
        run_id: Option<&str>,
        after_seq: u64,
        limit: usize,
    ) -> Result<serde_json::Value, OrchError> {
        let mut page = self.bus.read_after(after_seq, limit);
        if let Some(rid) = run_id {
            if let Ok(Some(run)) = self.store.load_run(rid) {
                page.entries.retain(|e| {
                    let sid = session_id_of(&e.update);
                    sid == Some(run.session_id)
                        && run.start_seq.map(|s| e.seq >= s).unwrap_or(true)
                        && run.end_seq.map(|s| e.seq <= s).unwrap_or(true)
                });
            }
        }
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
        let run = self
            .store
            .load_run(run_id)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::InvalidRequest, "unknown run_id"))?;
        let page = self.scoped_events(&run);
        let mut paths = Vec::new();
        for e in page.entries {
            if let crate::events::SessionUpdate::FileEdit { path, summary, .. } = e.update {
                paths.push(json!({ "path": path, "summary": summary }));
            }
        }
        Ok(json!({ "runId": run_id, "changes": paths }))
    }

    pub fn get_test_results(
        &self,
        _auth: &AuthContext,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let run = self
            .store
            .load_run(run_id)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::InvalidRequest, "unknown run_id"))?;
        let page = self.scoped_events(&run);
        let mut observed = Vec::new();
        for e in page.entries {
            match e.update {
                crate::events::SessionUpdate::ShellSessionStarted {
                    command, call_id, ..
                } => {
                    if command.to_ascii_lowercase().contains("test")
                        || command.contains("cargo test")
                        || command.contains("npm test")
                        || command.contains("pytest")
                    {
                        observed.push(json!({
                            "callId": call_id,
                            "command": command,
                            "status": "started",
                        }));
                    }
                }
                crate::events::SessionUpdate::ShellSessionEnded {
                    call_id,
                    exit_code,
                    cancelled,
                    ..
                } => {
                    observed.push(json!({
                        "callId": call_id,
                        "exitCode": exit_code,
                        "cancelled": cancelled,
                        "status": "ended",
                    }));
                }
                _ => {}
            }
        }
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
        let run = self
            .store
            .load_run(run_id)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::InvalidRequest, "unknown run_id"))?;
        Ok(json!({
            "runId": run.run_id,
            "sessionId": run.session_id,
            "state": run.state,
            "finalResponse": run.final_response,
            "terminalResult": run.terminal_result,
            "startSeq": run.start_seq,
            "endSeq": run.end_seq,
        }))
    }

    fn scoped_events(&self, run: &RunRecord) -> JournalPage {
        let after = run.start_seq.map(|s| s.saturating_sub(1)).unwrap_or(0);
        let mut page = self.bus.read_after(after, 500);
        page.entries.retain(|e| {
            session_id_of(&e.update) == Some(run.session_id)
                && run.end_seq.map(|end| e.seq <= end).unwrap_or(true)
        });
        page
    }

    // ── mutations ──────────────────────────────────────────────────────

    pub async fn submit_task(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        prompt: String,
        bounds: Option<RunBounds>,
    ) -> Result<serde_json::Value, OrchError> {
        let _ = auth;
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "prompt": prompt,
            "bounds": bounds,
        });
        if let Some(prev) = self.check_idempotency("ptah_submit_task", request_id, &payload)? {
            return Ok(prev);
        }
        reject_control_prompt(&prompt)?;
        let bounds = bounds.unwrap_or_else(|| self.config.lock().bounds.clone());
        if prompt.len() > bounds.max_prompt_bytes {
            self.audit(
                "ptah_submit_task",
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                "rejected",
                Some("invalid_request"),
                "prompt too large",
            );
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                format!(
                    "prompt exceeds max_prompt_bytes ({})",
                    bounds.max_prompt_bytes
                ),
            ));
        }

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
        let cwd = if session.cwd.is_empty() {
            None
        } else {
            Some(PathBuf::from(&session.cwd))
        };
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = require_workspace_match(&allowlist, cwd.as_deref(), workspace)?;

        if self.host.session_busy(session_id) {
            self.audit(
                "ptah_submit_task",
                Some(request_id),
                Some(session_id),
                Some(&claimed.display().to_string()),
                "rejected",
                Some("session_busy"),
                "session busy",
            );
            return Err(OrchError::new(
                OrchErrorCode::SessionBusy,
                "session already has an active turn",
            ));
        }

        {
            let active = self.active.lock();
            let max = self.config.lock().max_concurrent_runs;
            if active.len() >= max {
                return Err(OrchError::new(
                    OrchErrorCode::CapacityExhausted,
                    "max concurrent runs reached",
                ));
            }
        }

        let run_id = Uuid::new_v4().to_string();
        let start_seq = self.bus.current_seq().saturating_add(1);
        let run = RunRecord {
            run_id: run_id.clone(),
            session_id,
            workspace: claimed.display().to_string(),
            request_id: request_id.into(),
            client_id: None,
            state: RunState::Running,
            bounds: bounds.clone(),
            prompt_preview: prompt_preview(&prompt),
            start_seq: Some(start_seq),
            end_seq: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            terminal_result: None,
            final_response: None,
            error_code: None,
        };
        self.store
            .save_run(&run)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))?;
        self.active.lock().push(run_id.clone());

        let host = self.host.clone();
        let store = OrchStore::open(self.store.root())
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))?;
        let bus = self.bus.clone();
        let active_slot = self.active.clone();
        let rid = run_id.clone();
        let prompt_owned = prompt.clone();
        let max_ms = bounds.max_duration_ms;

        let join = tokio::spawn(async move {
            let result = tokio::time::timeout(
                std::time::Duration::from_millis(max_ms.max(1)),
                host.session_prompt(session_id, prompt_owned),
            )
            .await;
            let end_seq = bus.current_seq();
            if let Ok(Some(mut r)) = store.load_run(&rid) {
                r.end_seq = Some(end_seq);
                r.updated_at = Utc::now();
                match result {
                    Ok(Ok(text)) => {
                        r.state = RunState::Completed;
                        r.terminal_result = Some("completed".into());
                        r.final_response = Some(text.chars().take(8_000).collect());
                    }
                    Ok(Err(e)) => {
                        r.state = RunState::Failed;
                        r.terminal_result = Some("failed".into());
                        r.error_code = Some("internal".into());
                        r.final_response = Some(e.to_string().chars().take(2_000).collect());
                    }
                    Err(_) => {
                        let _ = host.cancel_turn(Some(session_id));
                        r.state = RunState::LimitReached;
                        r.terminal_result = Some("limit_reached".into());
                        r.error_code = Some("limit_reached".into());
                    }
                }
                let _ = store.save_run(&r);
            }
            active_slot.lock().retain(|id| id != &rid);
        });
        // Don't await full completion for the RPC response — return running receipt.
        // Drop join handle intentionally (process-local); tests can poll get_run.
        std::mem::forget(join);

        let response = json!({
            "runId": run_id,
            "sessionId": session_id,
            "state": RunState::Running,
            "requestId": request_id,
        });
        self.save_idempotency(
            "ptah_submit_task",
            request_id,
            &payload,
            Some(run_id),
            response.clone(),
        )?;
        self.audit(
            "ptah_submit_task",
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "run started",
        );
        let _ = run;
        Ok(response)
    }

    pub fn queue_prompt(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        prompt: String,
        priority: bool,
    ) -> Result<serde_json::Value, OrchError> {
        let _ = auth;
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "prompt": prompt,
            "priority": priority,
        });
        if let Some(prev) = self.check_idempotency("ptah_queue_prompt", request_id, &payload)? {
            return Ok(prev);
        }
        reject_control_prompt(&prompt)?;
        let session = self
            .host
            .session_load(session_id)
            .map_err(|_| OrchError::new(OrchErrorCode::InvalidRequest, "unknown session"))?;
        let cwd = if session.cwd.is_empty() {
            None
        } else {
            Some(PathBuf::from(&session.cwd))
        };
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = require_workspace_match(&allowlist, cwd.as_deref(), workspace)?;
        let entries = self
            .host
            .session_queue_add(session_id, prompt, priority)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))?;
        let response = json!({
            "requestId": request_id,
            "sessionId": session_id,
            "entries": entries,
        });
        self.save_idempotency(
            "ptah_queue_prompt",
            request_id,
            &payload,
            None,
            response.clone(),
        )?;
        self.audit(
            "ptah_queue_prompt",
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "queued",
        );
        Ok(response)
    }

    pub fn steer(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        text: String,
    ) -> Result<serde_json::Value, OrchError> {
        let _ = auth;
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "text": text,
        });
        if let Some(prev) = self.check_idempotency("ptah_steer", request_id, &payload)? {
            return Ok(prev);
        }
        reject_control_prompt(&text)?;
        let session = self
            .host
            .session_load(session_id)
            .map_err(|_| OrchError::new(OrchErrorCode::InvalidRequest, "unknown session"))?;
        let cwd = if session.cwd.is_empty() {
            None
        } else {
            Some(PathBuf::from(&session.cwd))
        };
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = require_workspace_match(&allowlist, cwd.as_deref(), workspace)?;
        let receipt = self
            .host
            .session_steer(session_id, text)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))?;
        let response = json!({
            "requestId": request_id,
            "sessionId": session_id,
            "disposition": receipt.disposition,
            "entry": receipt.entry,
            "entries": receipt.entries,
        });
        self.save_idempotency("ptah_steer", request_id, &payload, None, response.clone())?;
        self.audit(
            "ptah_steer",
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "steer",
        );
        Ok(response)
    }

    pub fn cancel(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        run_id: Option<&str>,
    ) -> Result<serde_json::Value, OrchError> {
        let _ = auth;
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "runId": run_id,
        });
        if let Some(prev) = self.check_idempotency("ptah_cancel", request_id, &payload)? {
            return Ok(prev);
        }
        let session = self
            .host
            .session_load(session_id)
            .map_err(|_| OrchError::new(OrchErrorCode::InvalidRequest, "unknown session"))?;
        let cwd = if session.cwd.is_empty() {
            None
        } else {
            Some(PathBuf::from(&session.cwd))
        };
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = require_workspace_match(&allowlist, cwd.as_deref(), workspace)?;
        let cancelled = self.host.cancel_turn(Some(session_id)).is_ok();
        if let Some(rid) = run_id {
            if let Ok(Some(mut run)) = self.store.load_run(rid) {
                if run.session_id == session_id {
                    run.state = RunState::Cancelled;
                    run.updated_at = Utc::now();
                    run.end_seq = Some(self.bus.current_seq());
                    run.terminal_result = Some("cancelled".into());
                    let _ = self.store.save_run(&run);
                }
            }
        }
        self.active.lock().retain(|id| Some(id.as_str()) != run_id);
        let response = json!({
            "requestId": request_id,
            "sessionId": session_id,
            "cancelled": cancelled,
        });
        self.save_idempotency("ptah_cancel", request_id, &payload, None, response.clone())?;
        self.audit(
            "ptah_cancel",
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            if cancelled {
                "cancelled"
            } else {
                "no active turn"
            },
        );
        Ok(response)
    }
}

fn session_id_of(u: &crate::events::SessionUpdate) -> Option<Uuid> {
    use crate::events::SessionUpdate::*;
    match u {
        AgentMessageChunk { session_id, .. }
        | AgentThoughtChunk { session_id, .. }
        | ToolCall { session_id, .. }
        | ToolCallUpdate { session_id, .. }
        | Plan { session_id, .. }
        | PermissionRequired { session_id, .. }
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
