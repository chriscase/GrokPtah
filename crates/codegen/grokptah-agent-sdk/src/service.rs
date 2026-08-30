//! Read-only observatory over current MCP control tools.

use std::collections::BTreeSet;
use std::fmt;

use serde_json::{Value, json};

use crate::capability::{
    Capabilities, TOOL_GET_CAPACITY, TOOL_GET_EVENTS, TOOL_GET_RUN, TOOL_LIST_RUNS,
    TOOL_LIST_SESSIONS,
};
use crate::dto::{
    EventPage, HostCapacity, RunView, SessionView, project_capacity, project_event_page,
    project_run, project_runs, project_sessions,
};
use crate::error::SdkError;
use crate::observe::{EventQuery, RunSelector, SessionScope};
use crate::transport::McpTransport;
use crate::version::{EVENT_PAGE_LIMIT_DEFAULT, EVENT_PAGE_LIMIT_MAX, EVENT_PAGE_LIMIT_MIN};

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

    /// Durable Build runs in one session/workspace. Missing tool → `unsupported`.
    pub async fn list_runs(&self, scope: &SessionScope) -> Result<Vec<RunView>, SdkError> {
        let body = self
            .call_required(
                TOOL_LIST_RUNS,
                json!({
                    "session_id": scope.session_id.as_str(),
                    "workspace": scope.workspace.host_token(),
                }),
            )
            .await?;
        project_runs(&body)
    }

    /// Project one run. Unknown and cross-scope denials are both `forbidden_scope`.
    pub async fn observe_run(&self, selector: &RunSelector) -> Result<RunView, SdkError> {
        let body = self
            .call_required(TOOL_GET_RUN, run_args(selector))
            .await
            .map_err(SdkError::collapse_run_scope)?;
        project_run(&body)
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
