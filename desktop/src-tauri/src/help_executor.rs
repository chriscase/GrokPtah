//! Tauri adapter for the dedicated one-shot Help executor.
//!
//! The default desktop adapter is deliberately fail-closed when no provider
//! has been explicitly installed. It never delegates to `AgentHost`, so Help
//! cannot create or read Chat/session/transcript/workspace/tool artifacts.

use std::sync::Arc;

use grokptah_agent_bridge::{
    HelpAuthorityRequest, HelpExecutionFailure, HelpExecutor, HelpProvider,
};
use serde::Serialize;
use tauri::State;
use tokio_util::sync::CancellationToken;

use crate::AppState;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TauriHelpExecution {
    pub response: Option<grokptah_agent_bridge::HelpAuthorityResponse>,
    pub failure: Option<String>,
    pub cleanup: grokptah_agent_bridge::HelpCleanupReceipt,
}

pub struct TauriHelpService {
    executor: HelpExecutor,
    provider: Arc<dyn HelpProvider>,
}

impl TauriHelpService {
    pub fn new() -> Self {
        // A real provider adapter must be installed explicitly by a future
        // authenticated host integration. Silent reuse of AgentHost authority
        // is forbidden for this contract.
        let provider: Arc<dyn HelpProvider> = Arc::new(
            |_request: HelpAuthorityRequest, _cancel: CancellationToken| async move {
                Err("no Help provider is configured".to_string())
            },
        );
        Self {
            executor: HelpExecutor::default(),
            provider,
        }
    }

    async fn execute(&self, request: HelpAuthorityRequest) -> TauriHelpExecution {
        let result = self
            .executor
            .execute(request, self.provider.clone(), CancellationToken::new())
            .await;
        TauriHelpExecution {
            response: result.response,
            failure: result.failure.map(failure_name),
            cleanup: result.cleanup,
        }
    }
}

impl Default for TauriHelpService {
    fn default() -> Self {
        Self::new()
    }
}

fn failure_name(failure: HelpExecutionFailure) -> String {
    match failure {
        HelpExecutionFailure::Capacity => "capacity",
        HelpExecutionFailure::Deadline => "deadline",
        HelpExecutionFailure::Cancelled => "cancelled",
        HelpExecutionFailure::Transport => "transport",
        HelpExecutionFailure::Rejected => "rejected",
    }
    .to_string()
}

/// Execute one strict Help request without entering the Chat/session runtime.
#[tauri::command]
pub async fn help_execute_one_shot(
    state: State<'_, AppState>,
    request: HelpAuthorityRequest,
) -> TauriHelpExecution {
    state.help.execute(request).await
}
