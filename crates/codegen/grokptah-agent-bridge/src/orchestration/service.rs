//! Orchestration service: reads + bounded mutations over AgentHostHandle (#196).

use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    active: Arc<Mutex<Vec<String>>>,
    /// Join handles for in-flight runs (prevents forget + unbounded leaks).
    join_handles: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl OrchestrationService {
    pub fn new(
        host: AgentHostHandle,
        bus: EventBus,
        store: OrchStore,
        config: OrchestrationConfig,
    ) -> Arc<Self> {
        // Register control bearer (and any future secrets) on the *shared* host bus
        // so durable journal redaction covers the shipped desktop path.
        if !config.bearer_token.is_empty() {
            bus.add_control_secrets([config.bearer_token.clone()]);
        }
        Arc::new(Self {
            host,
            bus,
            store,
            config: Mutex::new(config),
            active: Arc::new(Mutex::new(Vec::new())),
            join_handles: Mutex::new(Vec::new()),
        })
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
            tool: tool.into(),
            request_id: request_id.map(str::to_string),
            session_id,
            workspace: workspace.map(str::to_string),
            outcome: outcome.into(),
            error_code: error_code.map(str::to_string),
            detail: crate::textutil::truncate_at_char_boundary(detail, 500).to_string(),
        };
        let _ = self.store.append_audit(&entry);
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

    fn try_reserve_capacity(&self, run_id: &str) -> Result<(), OrchError> {
        let max = self.config.lock().max_concurrent_runs;
        let mut active = self.active.lock();
        if active.len() >= max {
            return Err(OrchError::new(
                OrchErrorCode::CapacityExhausted,
                "max concurrent runs reached",
            ));
        }
        active.push(run_id.to_string());
        Ok(())
    }

    fn release_capacity(&self, run_id: &str) {
        self.active.lock().retain(|id| id != run_id);
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
        let max = self.config.lock().max_concurrent_runs;
        let active = self.active.lock().len();
        Ok(json!({
            "maxConcurrentRuns": max,
            "activeRuns": active,
            "available": max.saturating_sub(active),
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
        let _ = auth;
        let tool = "ptah_submit_task";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "prompt": prompt,
            "bounds": bounds_json,
        });
        let phash = hash_payload(&payload);

        match self.store.claim_idempotency(tool, request_id, &phash) {
            Ok(IdempotencyClaim::Replay(prev)) => return Ok(prev),
            Ok(IdempotencyClaim::Perform) => {}
            Err(e) => {
                self.audit_err(tool, Some(request_id), Some(session_id), None, &e);
                return Err(e);
            }
        }

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

        if self.host.session_busy(session_id) {
            let e = OrchError::new(
                OrchErrorCode::SessionBusy,
                "session already has an active turn",
            );
            return Err(finish_err(self, e));
        }

        let run_id = Uuid::new_v4().to_string();
        if let Err(e) = self.try_reserve_capacity(&run_id) {
            return Err(finish_err(self, e));
        }

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
            aggregates: RunAggregates::default(),
        };
        if let Err(e) = self.store.save_run(&run) {
            self.release_capacity(&run_id);
            let e = OrchError::new(OrchErrorCode::Internal, e.to_string());
            return Err(finish_err(self, e));
        }

        let host = self.host.clone();
        let store = self.store.clone();
        let bus = self.bus.clone();
        let active_slot = self.active.clone();
        let rid = run_id.clone();
        let prompt_owned = prompt.clone();
        let max_ms = bounds.max_duration_ms;
        let max_rounds = bounds.max_rounds;

        // Dedicated aggregator task: must not share a biased select with the
        // duration deadline (chatty ShellOutput must not starve max_duration_ms).
        let mut agg_rx = bus.subscribe();
        let store_agg = store.clone();
        let rid_agg = run_id.clone();
        let agg_task = tokio::spawn(async move {
            while let Some(update) = agg_rx.recv().await {
                apply_run_aggregate(&store_agg, &rid_agg, session_id, &update);
            }
        });

        let join = tokio::spawn(async move {
            let prompt_fut = host.session_prompt_with_max_rounds(
                session_id,
                prompt_owned,
                Some(max_rounds.max(1)),
            );
            tokio::pin!(prompt_fut);
            let deadline = tokio::time::sleep(std::time::Duration::from_millis(max_ms.max(1)));
            tokio::pin!(deadline);

            // Drive the turn until completion or duration limit. On timeout we
            // cancel while the future is still alive (turn_cancels still present),
            // await shell teardown, then await the future — never drop it first.
            // Deadline is polled first (biased) so event flood cannot starve it.
            let mut timed_out = false;
            let result: Result<String, anyhow::Error> = loop {
                tokio::select! {
                    biased;
                    _ = &mut deadline, if !timed_out => {
                        timed_out = true;
                        // Future still pinned — cancel_turn finds active turn.
                        let _ = host.cancel_turn_and_await(Some(session_id)).await;
                        // Fall through: keep selecting until prompt_fut completes.
                    }
                    r = &mut prompt_fut => {
                        break r;
                    }
                }
            };

            // Stop aggregator; then reconcile aggregates from the journal range
            // so late FileEdit/test events are not lost if the task was aborted mid-drain.
            agg_task.abort();
            let _ = agg_task.await;

            let end_seq = bus.current_seq();
            if let Ok(Some(mut r)) = store.load_run(&rid) {
                if matches!(r.state, RunState::Cancelled | RunState::Interrupted) {
                    r.end_seq = r.end_seq.or(Some(end_seq));
                    r.updated_at = Utc::now();
                    let _ = store.save_run(&r);
                    active_slot.lock().retain(|id| id != &rid);
                    return;
                }
                r.end_seq = Some(end_seq);
                r.updated_at = Utc::now();
                // Final journal pass (does not replace incremental path; fills gaps).
                reconcile_aggregates_from_bus(&bus, &mut r);
                if timed_out {
                    r.state = RunState::LimitReached;
                    r.terminal_result = Some("limit_reached".into());
                    r.error_code = Some("limit_reached".into());
                    if let Ok(text) = &result {
                        r.final_response = Some(
                            crate::textutil::truncate_at_char_boundary(text, 8_000).to_string(),
                        );
                    }
                } else {
                    match result {
                        Ok(text) => {
                            if crate::host_helpers::is_round_limit_stop_message(&text) {
                                r.state = RunState::LimitReached;
                                r.terminal_result = Some("limit_reached".into());
                                r.error_code = Some("limit_reached".into());
                            } else {
                                r.state = RunState::Completed;
                                r.terminal_result = Some("completed".into());
                            }
                            r.final_response = Some(
                                crate::textutil::truncate_at_char_boundary(&text, 8_000)
                                    .to_string(),
                            );
                        }
                        Err(e) => {
                            r.state = RunState::Failed;
                            r.terminal_result = Some("failed".into());
                            r.error_code = Some("internal".into());
                            r.final_response = Some(
                                crate::textutil::truncate_at_char_boundary(&e.to_string(), 2_000)
                                    .to_string(),
                            );
                        }
                    }
                }
                let _ = store.save_run(&r);
            }
            active_slot.lock().retain(|id| id != &rid);
        });
        self.reaping_handles();
        self.join_handles.lock().push(join);

        let response = json!({
            "runId": run_id,
            "sessionId": session_id,
            "state": RunState::Running,
            "requestId": request_id,
        });
        if let Err(e) = self.store.complete_idempotency(
            tool,
            request_id,
            &phash,
            Some(run_id.clone()),
            response.clone(),
        ) {
            // Action already started — still surface error; retry should replay if claim remains.
            self.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&claimed.display().to_string()),
                &e,
            );
            return Err(e);
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "run started",
        );
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
        let tool = "ptah_queue_prompt";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "prompt": prompt,
            "priority": priority,
        });
        let phash = hash_payload(&payload);
        match self.store.claim_idempotency(tool, request_id, &phash) {
            Ok(IdempotencyClaim::Replay(prev)) => return Ok(prev),
            Ok(IdempotencyClaim::Perform) => {}
            Err(e) => {
                self.audit_err(tool, Some(request_id), Some(session_id), None, &e);
                return Err(e);
            }
        }
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
        let entries = match self.host.session_queue_add_with_source(
            session_id,
            prompt,
            priority,
            "control",
            Some("mcp".into()),
        ) {
            Ok(e) => e,
            Err(e) => {
                return Err(fail(
                    self,
                    OrchError::new(OrchErrorCode::Internal, e.to_string()),
                ));
            }
        };
        let response = json!({
            "requestId": request_id,
            "sessionId": session_id,
            "entries": entries,
        });
        self.store
            .complete_idempotency(tool, request_id, &phash, None, response.clone())?;
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

    pub fn steer(
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
        match self.store.claim_idempotency(tool, request_id, &phash) {
            Ok(IdempotencyClaim::Replay(prev)) => return Ok(prev),
            Ok(IdempotencyClaim::Perform) => {}
            Err(e) => {
                self.audit_err(tool, Some(request_id), Some(session_id), None, &e);
                return Err(e);
            }
        }
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
        let receipt = match self.host.session_steer(session_id, text) {
            Ok(r) => r,
            Err(e) => {
                return Err(fail(
                    self,
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
        self.store
            .complete_idempotency(tool, request_id, &phash, None, response.clone())?;
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

    pub fn cancel(
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
        match self.store.claim_idempotency(tool, request_id, &phash) {
            Ok(IdempotencyClaim::Replay(prev)) => return Ok(prev),
            Ok(IdempotencyClaim::Perform) => {}
            Err(e) => {
                self.audit_err(tool, Some(request_id), Some(session_id), None, &e);
                return Err(e);
            }
        }
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

        let mut run = match self.store.load_run(rid) {
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
        if run.state.is_terminal() {
            return Err(fail(
                self,
                OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    format!("run already terminal ({:?})", run.state),
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

        // Persist cancelled BEFORE signalling cancel so late workers cannot complete.
        run.state = RunState::Cancelled;
        run.updated_at = Utc::now();
        run.end_seq = Some(self.bus.current_seq());
        run.terminal_result = Some("cancelled".into());
        if let Err(e) = self.store.save_run(&run) {
            return Err(fail(
                self,
                OrchError::new(OrchErrorCode::Internal, e.to_string()),
            ));
        }

        let cancelled = self.host.cancel_turn(Some(session_id)).is_ok();
        // Only release this run's capacity.
        self.release_capacity(rid);

        let response = json!({
            "requestId": request_id,
            "sessionId": session_id,
            "runId": rid,
            "cancelled": cancelled,
            "state": RunState::Cancelled,
        });
        self.store.complete_idempotency(
            tool,
            request_id,
            &phash,
            Some(rid.into()),
            response.clone(),
        )?;
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "cancelled",
        );
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
fn apply_run_aggregate(
    store: &OrchStore,
    run_id: &str,
    session_id: Uuid,
    update: &crate::events::SessionUpdate,
) {
    if session_id_of(update) != Some(session_id) {
        return;
    }
    let Ok(Some(mut r)) = store.load_run(run_id) else {
        return;
    };
    if fold_aggregate_update(&mut r.aggregates, update) {
        r.updated_at = Utc::now();
        let _ = store.save_run(&r);
    }
}

fn fold_aggregate_update(
    aggregates: &mut RunAggregates,
    update: &crate::events::SessionUpdate,
) -> bool {
    match update {
        crate::events::SessionUpdate::FileEdit { path, summary, .. } => {
            if aggregates.changes.iter().any(|c| c.path == *path) {
                return false;
            }
            aggregates.changes.push(ChangeRecord {
                path: path.clone(),
                summary: summary.clone(),
            });
            true
        }
        crate::events::SessionUpdate::ShellSessionStarted {
            command, call_id, ..
        } if is_recognized_test_command(command) => {
            if aggregates.tests.iter().any(|t| t.call_id == *call_id) {
                return false;
            }
            aggregates.tests.push(TestObservation {
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
            if let Some(t) = aggregates.tests.iter_mut().find(|t| t.call_id == *call_id) {
                t.status = "ended".into();
                t.exit_code = *exit_code;
                t.cancelled = Some(*cancelled);
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

fn reconcile_aggregates_from_bus(bus: &EventBus, run: &mut RunRecord) {
    let after = run.start_seq.map(|s| s.saturating_sub(1)).unwrap_or(0);
    let Ok(entries) = bus.read_range_all(after, run.end_seq, Some(run.session_id)) else {
        return;
    };
    for e in entries {
        fold_aggregate_update(&mut run.aggregates, &e.update);
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
