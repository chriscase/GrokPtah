//! Read-only source inspection commands.
//!
//! This module is a shim. Every containment, authorization, and bounding rule
//! lives in `xai-source-view`, which has no Tauri, network, or credential
//! surface and is unit-tested against synthetic fixtures. What is here is the
//! part that cannot live there: reading the host's live authorization state.
//!
//! The authorization context is rebuilt from live state **twice** — once when
//! a snapshot is issued and again on every read. That is what makes the
//! authorization action-time rather than snapshot-time: a run discarded, a
//! project closed, or a permission mode changed after a snapshot was issued
//! drifts the policy fingerprint and refuses every token derived from it.

use std::path::Path;
use std::sync::Arc;

use grokptah_agent_bridge::{AgentHostHandle, RunExecutionMode};
use tauri::State;
use uuid::Uuid;
use xai_source_view::{
    boundary_message, is_managed_run_worktree, path_digest, AuthorizationContext, CandidateRoot,
    PathPolicy, PolicyInputs, Principal, ReadCursor, RequestedLimits, RootSnapshot, SnapshotStore,
    SourceDocument, SourceRequest, SourceViewError, SystemClock,
};

use crate::AppState;

/// Live source-inspection authority for this process.
#[derive(Debug)]
pub struct SourceViewService {
    store: SnapshotStore,
    /// Stable for the process lifetime; participates in the principal so
    /// tokens cannot be carried between two runs of the app.
    instance_id: String,
}

impl SourceViewService {
    /// Build the service with fresh, process-unique key material.
    ///
    /// The key is the only thing standing between a caller and a forged token.
    /// It is two v4 UUIDs from the platform CSPRNG — 244 bits of entropy once
    /// the version and variant bits are discounted — and never leaves this
    /// process: not to disk, not to the frontend, not into any receipt. It is
    /// also regenerated per launch, so a token cannot outlive the run of the
    /// app that issued it.
    pub fn new() -> Self {
        let mut key = [0u8; 32];
        key[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        key[16..].copy_from_slice(Uuid::new_v4().as_bytes());
        Self {
            store: SnapshotStore::new(key, Arc::new(SystemClock)),
            instance_id: Uuid::new_v4().to_string(),
        }
    }

    pub fn store(&self) -> &SnapshotStore {
        &self.store
    }
}

impl Default for SourceViewService {
    fn default() -> Self {
        Self::new()
    }
}

/// One run's contribution to the authorization picture.
struct RunFact {
    run_id: String,
    promotion_state: String,
    source_workspace: String,
    execution_workspace: String,
}

/// Everything the authorization decision reads, gathered once.
///
/// Gathering the inputs separately from using them keeps the issue path and
/// the read path provably identical: both call this, and both derive the
/// principal and the policy from its result.
struct AuthorizationFacts {
    project_cwd: Option<String>,
    session_cwd: Option<String>,
    session_id: Option<Uuid>,
    auth_method: String,
    signed_in: bool,
    permission_mode: String,
    runs: Vec<RunFact>,
}

fn gather(host: &AgentHostHandle, session_id: Option<Uuid>) -> AuthorizationFacts {
    let workspace = host.workspace_ui_state();
    let auth = host.auth_state();
    let settings = host.settings_snapshot();
    let permission_mode = settings
        .get("permission_mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    let session_cwd = session_id
        .and_then(|id| host.session_load(id).ok())
        .map(|session| session.cwd)
        .filter(|cwd| !cwd.is_empty());

    let runs = session_id
        .map(|id| host.list_session_runs(id).unwrap_or_default())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|run| {
            let execution = run.execution?;
            if execution.mode != RunExecutionMode::IsolatedWorktree {
                return None;
            }
            Some(RunFact {
                run_id: run.run_id,
                promotion_state: format!("{:?}", execution.promotion_state),
                source_workspace: execution.source_workspace,
                execution_workspace: execution.execution_workspace,
            })
        })
        .collect();

    AuthorizationFacts {
        project_cwd: workspace.project_cwd,
        session_cwd,
        session_id,
        auth_method: auth.method.unwrap_or_else(|| "none".into()),
        signed_in: auth.signed_in,
        permission_mode,
        runs,
    }
}

/// Digest a path so an authorization fact identifies a directory without
/// carrying its location.
fn digest_path(value: &str) -> String {
    path_digest("grokptah.source-view.fact.v1", Path::new(value))
}

impl AuthorizationFacts {
    /// The acting identity.
    ///
    /// The desktop is single-tenant and single-project by construction, so the
    /// tenant is this process instance and the project is the open workspace.
    /// A hosted broker populates the same four fields from its own directory;
    /// the contract does not change, only where the values come from.
    fn principal(&self, service: &SourceViewService) -> Principal {
        Principal::new(
            format!("local:{}", self.auth_method),
            format!("instance:{}", service.instance_id),
            self.project_cwd
                .as_deref()
                .map(digest_path)
                .unwrap_or_else(|| "no-project".into()),
            self.session_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "no-session".into()),
        )
    }

    /// Everything that, if it moved, makes an outstanding snapshot stale.
    fn policy(&self) -> PolicyInputs {
        let mut policy = PolicyInputs::new();
        policy.push("signed_in", if self.signed_in { "yes" } else { "no" });
        policy.push("permission_mode", &self.permission_mode);
        policy.push(
            "project",
            &self
                .project_cwd
                .as_deref()
                .map(digest_path)
                .unwrap_or_else(|| "none".into()),
        );
        policy.push(
            "session_cwd",
            &self
                .session_cwd
                .as_deref()
                .map(digest_path)
                .unwrap_or_else(|| "none".into()),
        );
        for run in &self.runs {
            policy.push(
                &format!("run:{}", run.run_id),
                &format!(
                    "{}:{}",
                    run.promotion_state,
                    digest_path(&run.execution_workspace)
                ),
            );
        }
        policy
    }

    /// The directories this principal may inspect, in a stable order.
    ///
    /// A run worktree qualifies only under the *same* test the promotion path
    /// applies, so inspection can never reach a tree promotion would refuse.
    fn candidates(&self) -> Vec<CandidateRoot> {
        let mut candidates = Vec::new();
        if let Some(cwd) = &self.project_cwd {
            candidates.push(CandidateRoot::workspace(cwd));
        }
        if let Some(cwd) = &self.session_cwd {
            if Some(cwd) != self.project_cwd.as_ref() {
                candidates.push(CandidateRoot::workspace(cwd));
            }
        }
        for run in &self.runs {
            if is_managed_run_worktree(
                Path::new(&run.source_workspace),
                Path::new(&run.execution_workspace),
            ) {
                candidates.push(CandidateRoot::worktree(
                    &run.execution_workspace,
                    &run.run_id,
                ));
            }
        }
        candidates
    }
}

fn parse_session(session_id: Option<String>) -> Result<Option<Uuid>, String> {
    match session_id {
        Some(raw) => Uuid::parse_str(&raw)
            .map(Some)
            .map_err(|_| boundary_message(&SourceViewError::PrincipalMismatch)),
        None => Ok(None),
    }
}

fn context(service: &SourceViewService, facts: &AuthorizationFacts) -> AuthorizationContext {
    AuthorizationContext::new(facts.principal(service), facts.policy())
}

async fn run_blocking<T, F>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|error| format!("blocking task join: {error}"))?
}

/// Issue a non-mutating authorization snapshot.
///
/// The result names every inspectable boundary by opaque token. There is no
/// other way to name one, and no ordering rule a caller may rely on: a caller
/// that has no token has no read.
#[tauri::command]
pub async fn source_view_snapshot(
    state: State<'_, AppState>,
    session_id: Option<String>,
) -> Result<RootSnapshot, String> {
    let host = state.host.clone();
    let service = state.source_view.clone();
    let session = parse_session(session_id)?;
    run_blocking(move || {
        let facts = gather(&host, session);
        let context = context(&service, &facts);
        Ok(service.store().issue(&context, &facts.candidates()))
    })
    .await
}

/// Read one bounded slice of one file inside one authorized boundary.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn source_view_read(
    state: State<'_, AppState>,
    token: String,
    path: String,
    session_id: Option<String>,
    start_byte: Option<u64>,
    cursor: Option<ReadCursor>,
    max_bytes: Option<u64>,
    max_lines: Option<usize>,
    max_line_chars: Option<usize>,
) -> Result<SourceDocument, String> {
    let host = state.host.clone();
    let service = state.source_view.clone();
    let session = parse_session(session_id)?;
    run_blocking(move || {
        // Authorization is recomputed here, from live state, at the moment of
        // the read — not carried over from when the snapshot was issued.
        let facts = gather(&host, session);
        let context = context(&service, &facts);
        let mut request = SourceRequest::new(token, path)
            .at_byte(start_byte.unwrap_or(0))
            .with_limits(RequestedLimits {
                max_bytes,
                max_lines,
                max_line_chars,
            });
        if let Some(cursor) = cursor {
            request = request.resume(cursor);
        }
        xai_source_view::open_document(service.store(), &context, &request, PathPolicy::host())
            .map_err(|error| boundary_message(&error))
    })
    .await
}

/// Revoke one snapshot, refusing every token derived from it.
#[tauri::command]
pub async fn source_view_revoke(
    state: State<'_, AppState>,
    snapshot_id: String,
) -> Result<bool, String> {
    let service = state.source_view.clone();
    run_blocking(move || Ok(service.store().revoke(&snapshot_id))).await
}

/// Drop expired snapshots and report how many were released.
///
/// Sweeping also happens on every issue and every read; this is the idle-tick
/// entry point so a long-lived window does not hold expired directory handles.
#[tauri::command]
pub async fn source_view_sweep(state: State<'_, AppState>) -> Result<usize, String> {
    let service = state.source_view.clone();
    run_blocking(move || Ok(service.store().sweep())).await
}

/// Milliseconds a freshly issued snapshot remains valid, so the frontend can
/// refresh before its tokens expire rather than after.
#[tauri::command]
pub fn source_view_ttl_ms(state: State<'_, AppState>) -> u64 {
    state.source_view.store().ttl_ms()
}
