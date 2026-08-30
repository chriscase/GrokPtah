//! Read-only observatory over current MCP control tools.

use std::collections::BTreeSet;
use std::fmt;

use serde_json::{Value, json};

use crate::capability::{
    Capabilities, TOOL_GET_CAPACITY, TOOL_GET_EVENTS, TOOL_GET_RUN, TOOL_LIST_RUNS,
    TOOL_LIST_SESSIONS,
};
use crate::dto::{
    EventPage, HostCapacity, PublicRunHandoffV1, PublicRunListV1, PublicRunProgressV1, PublicRunV1,
    RunView, SessionView, parse_public_run_handoff_v1, parse_public_run_list_v1,
    parse_public_run_progress_v1, parse_public_run_v1, project_capacity, project_event_page,
    project_sessions,
};
use crate::error::SdkError;
use crate::observe::{EventQuery, RunSelector, SessionScope};
use crate::transport::McpTransport;
use crate::version::{EVENT_PAGE_LIMIT_DEFAULT, EVENT_PAGE_LIMIT_MAX, EVENT_PAGE_LIMIT_MIN};

const TOOL_GET_PROGRESS: &str = "ptah_get_progress";
const TOOL_GET_HANDOFF: &str = "ptah_get_handoff";

/// Versioned read-only facade. Mutation, computer-control, and credential tools
/// are never invoked.
pub struct ReadObservatory<T: McpTransport> {
    transport: T,
    capabilities: Capabilities,
    tools: BTreeSet<String>,
}

impl<T: McpTransport> fmt::Debug for ReadObservatory<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadObservatory")
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

impl<T: McpTransport> ReadObservatory<T> {
    /// Discover capabilities from `tools/list` and bind the consumer transport.
    pub async fn connect(transport: T) -> Result<Self, SdkError> {
        let listed = transport.list_tools().await?;
        let tools = listed
            .into_iter()
            .map(|tool| tool.name)
            .collect::<BTreeSet<_>>();
        let capabilities = Capabilities::from_tool_names(&tools);
        Ok(Self {
            transport,
            capabilities,
            tools,
        })
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    /// Build sessions only. Non-build rows from the host are dropped, not errored.
    pub async fn list_sessions(&self) -> Result<Vec<SessionView>, SdkError> {
        let body = self.call_required(TOOL_LIST_SESSIONS, json!({})).await?;
        project_sessions(&body)
    }

    /// Legacy `RunRecord` list. Public MCP `ptah_list_runs` emits
    /// `grokptah.public-run.v1` only; this method does not call that tool and
    /// returns [`SdkError::Unsupported`]. Use [`Self::list_public_runs`].
    pub async fn list_runs(&self, _scope: &SessionScope) -> Result<Vec<RunView>, SdkError> {
        let _ = self;
        Err(SdkError::Unsupported)
    }

    /// Legacy `RunRecord` get. Public MCP `ptah_get_run` emits
    /// `grokptah.public-run.v1` only; this method does not call that tool and
    /// returns [`SdkError::Unsupported`]. Use [`Self::observe_public_run`].
    pub async fn observe_run(&self, _selector: &RunSelector) -> Result<RunView, SdkError> {
        let _ = self;
        Err(SdkError::Unsupported)
    }

    /// Allowlisted `grokptah.public-run.v1` list for one session/workspace.
    ///
    /// Session and workspace stay on `scope` and are never read from the body.
    /// Unknown version/field and legacy `RunRecord` decode as [`SdkError::Internal`].
    pub async fn list_public_runs(
        &self,
        scope: &SessionScope,
    ) -> Result<PublicRunListV1, SdkError> {
        let body = self
            .call_required(
                TOOL_LIST_RUNS,
                json!({
                    "session_id": scope.session_id.as_str(),
                    "workspace": scope.workspace.host_token(),
                }),
            )
            .await?;
        parse_public_run_list_v1(&body)
    }

    /// Allowlisted `grokptah.public-run.v1` get. Unknown and cross-scope denials
    /// are both `forbidden_scope`. Session/workspace stay on `selector`.
    pub async fn observe_public_run(
        &self,
        selector: &RunSelector,
    ) -> Result<PublicRunV1, SdkError> {
        let body = self
            .call_required(TOOL_GET_RUN, run_args(selector))
            .await
            .map_err(SdkError::collapse_run_scope)?;
        parse_public_run_v1(&body)
    }

    /// Allowlisted `ptah_get_progress` `grokptah.public-run.v1` document.
    pub async fn observe_public_progress(
        &self,
        selector: &RunSelector,
    ) -> Result<PublicRunProgressV1, SdkError> {
        let body = self
            .call_required(TOOL_GET_PROGRESS, run_args(selector))
            .await
            .map_err(SdkError::collapse_run_scope)?;
        parse_public_run_progress_v1(&body)
    }

    /// Allowlisted `ptah_get_handoff` `grokptah.public-run.v1` document.
    pub async fn observe_public_handoff(
        &self,
        selector: &RunSelector,
    ) -> Result<PublicRunHandoffV1, SdkError> {
        let body = self
            .call_required(TOOL_GET_HANDOFF, run_args(selector))
            .await
            .map_err(SdkError::collapse_run_scope)?;
        parse_public_run_handoff_v1(&body)
    }

    /// Page `ptah_get_events`. `limit` is 1..=500 (host default 50).
    pub async fn stream_events(
        &self,
        selector: &RunSelector,
        query: EventQuery,
    ) -> Result<EventPage, SdkError> {
        let limit = query.limit.unwrap_or(EVENT_PAGE_LIMIT_DEFAULT);
        if !(EVENT_PAGE_LIMIT_MIN..=EVENT_PAGE_LIMIT_MAX).contains(&limit) {
            return Err(SdkError::InvalidRequest);
        }
        let mut arguments = run_args(selector);
        if let Some(after_seq) = query.after_seq {
            arguments["after_seq"] = json!(after_seq);
        }
        arguments["limit"] = json!(limit);
        let body = self
            .call_required(TOOL_GET_EVENTS, arguments)
            .await
            .map_err(SdkError::collapse_run_scope)?;
        project_event_page(&body)
    }

    /// Occupancy and health flags from `ptah_get_capacity`.
    pub async fn host_capacity(&self) -> Result<HostCapacity, SdkError> {
        let body = self.call_required(TOOL_GET_CAPACITY, json!({})).await?;
        project_capacity(&body)
    }

    fn require_tool(&self, name: &str) -> Result<(), SdkError> {
        if self.tools.contains(name) {
            Ok(())
        } else {
            Err(SdkError::Unsupported)
        }
    }

    async fn call_required(&self, name: &str, arguments: Value) -> Result<Value, SdkError> {
        self.require_tool(name)?;
        let raw = self.transport.call_tool(name, arguments).await?;
        unwrap_tool_body(raw)
    }
}

fn run_args(selector: &RunSelector) -> Value {
    json!({
        "session_id": selector.session_id.as_str(),
        "workspace": selector.workspace.host_token(),
        "run_id": selector.run_id.as_str(),
    })
}

fn unwrap_tool_body(raw: Value) -> Result<Value, SdkError> {
    if raw.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(SdkError::Internal);
    }
    Ok(raw.get("structuredContent").cloned().unwrap_or(raw))
}

impl SessionScope {
    pub fn from_session(session: &SessionView) -> Self {
        Self::new(session.session_id.clone(), session.workspace.clone())
    }
}

impl RunSelector {
    pub fn from_parts(session: &SessionView, run_id: crate::ids::RunId) -> Self {
        Self::new(
            session.session_id.clone(),
            session.workspace.clone(),
            run_id,
        )
    }
}
