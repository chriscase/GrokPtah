//! Dedicated one-shot Help executor.
//!
//! This module is intentionally independent of `AgentHost`: it creates no
//! Chat/session row, transcript, workspace context, tool registry, fallback
//! route, or inherited authority. A Tauri adapter or authenticated browser
//! broker supplies one provider callback and receives one typed receipt.

use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use grokptah_agent_sdk::{
    parse_help_response, validate_help_request, HelpAuthorityRequest, HelpAuthorityResponse,
    HelpCleanupReceipt, HelpCleanupStatus, HelpMessageKind, HelpProviderTask, HelpQueueSlot,
    HELP_AUTHORITY_MAX_DURATION_MS,
};

/// Provider future used by the one-shot Help executor.
pub type HelpProviderFuture =
    Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'static>>;

/// A provider callback with no access to host/session state.
pub trait HelpProvider: Send + Sync + 'static {
    /// Execute one already-validated Help request.
    fn execute(
        &self,
        request: HelpAuthorityRequest,
        cancellation: CancellationToken,
    ) -> HelpProviderFuture;
}

impl<F, Fut> HelpProvider for F
where
    F: Fn(HelpAuthorityRequest, CancellationToken) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, String>> + Send + 'static,
{
    fn execute(
        &self,
        request: HelpAuthorityRequest,
        cancellation: CancellationToken,
    ) -> HelpProviderFuture {
        Box::pin(self(request, cancellation))
    }
}

/// Stable failure categories for a one-shot Help attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpExecutionFailure {
    /// Admission was full and no queue slot was available.
    Capacity,
    /// The provider exceeded the absolute deadline.
    Deadline,
    /// The caller cancelled the attempt.
    Cancelled,
    /// The provider failed without exposing its diagnostic.
    Transport,
    /// The provider response failed strict validation.
    Rejected,
}

/// The result and cleanup evidence for one Help attempt.
#[derive(Debug)]
pub struct HelpExecution {
    /// Validated answer when the attempt succeeded.
    pub response: Option<HelpAuthorityResponse>,
    /// Stable failure category when no answer was returned.
    pub failure: Option<HelpExecutionFailure>,
    /// Typed finalization evidence emitted on every path.
    pub cleanup: HelpCleanupReceipt,
}

/// Bounded one-shot Help executor shared by host adapters.
pub struct HelpExecutor {
    permits: Arc<Semaphore>,
    queued: Arc<AtomicUsize>,
    max_concurrent: usize,
    max_queue: usize,
}

impl HelpExecutor {
    /// Construct an executor with bounded concurrent and queued work.
    pub fn new(max_concurrent: usize, max_queue: usize) -> Self {
        let max_concurrent = max_concurrent.clamp(1, 8);
        Self {
            permits: Arc::new(Semaphore::new(max_concurrent)),
            queued: Arc::new(AtomicUsize::new(0)),
            max_concurrent,
            max_queue: max_queue.min(32),
        }
    }

    /// Number of provider tasks currently admitted to execution.
    pub fn active_count(&self) -> usize {
        self.max_concurrent - self.permits.available_permits()
    }

    /// Number of requests currently waiting for an execution permit.
    pub fn queued_count(&self) -> usize {
        self.queued.load(Ordering::Acquire)
    }

    fn cleanup(
        request_id: &str,
        status: HelpCleanupStatus,
        provider_task: HelpProviderTask,
        abort_requested: bool,
        queue_slot: HelpQueueSlot,
    ) -> HelpCleanupReceipt {
        HelpCleanupReceipt {
            schema: grokptah_agent_sdk::HELP_AUTHORITY_SCHEMA.into(),
            kind: HelpMessageKind::Cleanup,
            request_id: request_id.into(),
            status,
            provider_task,
            abort_requested,
            queue_slot,
            artifact_counts: Default::default(),
        }
    }

    async fn acquire(
        &self,
        deadline: DateTime<Utc>,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, HelpExecutionFailure> {
        if deadline <= Utc::now() {
            return Err(HelpExecutionFailure::Deadline);
        }
        if let Ok(permit) = self.permits.clone().try_acquire_owned() {
            return Ok(permit);
        }
        let previous = self.queued.fetch_add(1, Ordering::AcqRel);
        if previous >= self.max_queue {
            self.queued.fetch_sub(1, Ordering::AcqRel);
            return Err(HelpExecutionFailure::Capacity);
        }
        let remaining = (deadline - Utc::now())
            .to_std()
            .unwrap_or_else(|_| Duration::from_millis(1));
        let acquired = tokio::time::timeout(remaining, self.permits.clone().acquire_owned()).await;
        self.queued.fetch_sub(1, Ordering::AcqRel);
        match acquired {
            Ok(Ok(permit)) => Ok(permit),
            Ok(Err(_)) => Err(HelpExecutionFailure::Capacity),
            Err(_) => Err(HelpExecutionFailure::Deadline),
        }
    }

    /// Execute exactly one provider task under admission, cancellation, and
    /// deadline supervision.
    pub async fn execute(
        &self,
        request: HelpAuthorityRequest,
        provider: Arc<dyn HelpProvider>,
        caller_cancel: CancellationToken,
    ) -> HelpExecution {
        if let Err(_) = validate_help_request(&request) {
            return HelpExecution {
                response: None,
                failure: Some(HelpExecutionFailure::Rejected),
                cleanup: Self::cleanup(
                    &request.request_id,
                    HelpCleanupStatus::Finalized,
                    HelpProviderTask::Joined,
                    false,
                    HelpQueueSlot::Released,
                ),
            };
        }
        let parsed_deadline = DateTime::parse_from_rfc3339(&request.deadline.deadline_at)
            .map(|value| value.with_timezone(&Utc));
        let Ok(deadline) = parsed_deadline else {
            return HelpExecution {
                response: None,
                failure: Some(HelpExecutionFailure::Rejected),
                cleanup: Self::cleanup(
                    &request.request_id,
                    HelpCleanupStatus::Finalized,
                    HelpProviderTask::Joined,
                    false,
                    HelpQueueSlot::Released,
                ),
            };
        };
        if request.deadline.max_duration_ms > HELP_AUTHORITY_MAX_DURATION_MS {
            return HelpExecution {
                response: None,
                failure: Some(HelpExecutionFailure::Rejected),
                cleanup: Self::cleanup(
                    &request.request_id,
                    HelpCleanupStatus::Finalized,
                    HelpProviderTask::Joined,
                    false,
                    HelpQueueSlot::Released,
                ),
            };
        }
        let permit = match self.acquire(deadline).await {
            Ok(permit) => permit,
            Err(failure) => {
                return HelpExecution {
                    response: None,
                    failure: Some(failure),
                    cleanup: Self::cleanup(
                        &request.request_id,
                        HelpCleanupStatus::Finalized,
                        HelpProviderTask::Joined,
                        false,
                        HelpQueueSlot::Released,
                    ),
                };
            }
        };
        if caller_cancel.is_cancelled() {
            drop(permit);
            return HelpExecution {
                response: None,
                failure: Some(HelpExecutionFailure::Cancelled),
                cleanup: Self::cleanup(
                    &request.request_id,
                    HelpCleanupStatus::Finalized,
                    HelpProviderTask::Joined,
                    false,
                    HelpQueueSlot::Released,
                ),
            };
        }

        let provider_cancel = CancellationToken::new();
        let task_cancel = provider_cancel.clone();
        let task_request = request.clone();
        let mut provider_task = tokio::spawn(async move {
            provider.execute(task_request, task_cancel).await
        });
        let remaining = (deadline - Utc::now())
            .to_std()
            .unwrap_or_else(|_| Duration::from_millis(1));
        let mut abort_requested = false;
        let mut failure = None;
        let mut raw = None;
        let task_result = tokio::select! {
            result = &mut provider_task => Some(result),
            _ = caller_cancel.cancelled() => {
                failure = Some(HelpExecutionFailure::Cancelled);
                None
            }
            _ = tokio::time::sleep(remaining) => {
                failure = Some(HelpExecutionFailure::Deadline);
                None
            }
        };
        let task_finished = task_result.is_some();
        if let Some(result) = task_result {
            match result {
                Ok(Ok(value)) => raw = Some(value),
                Ok(Err(_)) => failure = Some(HelpExecutionFailure::Transport),
                Err(_) => failure = Some(HelpExecutionFailure::Transport),
            }
        } else {
            abort_requested = true;
            provider_cancel.cancel();
            provider_task.abort();
        }

        // Abort is only a request. Await the JoinHandle before releasing the
        // permit, including when the provider ignores CancellationToken.
        let joined = if task_finished {
            true
        } else {
            let _ = provider_task.await;
            true
        };
        drop(permit);
        let cleanup = Self::cleanup(
            &request.request_id,
            if joined {
                HelpCleanupStatus::Finalized
            } else {
                HelpCleanupStatus::Uncertain
            },
            if joined {
                HelpProviderTask::Joined
            } else {
                HelpProviderTask::NotJoined
            },
            abort_requested,
            HelpQueueSlot::Released,
        );
        if failure.is_some() {
            return HelpExecution {
                response: None,
                failure,
                cleanup,
            };
        }
        let Some(raw) = raw else {
            return HelpExecution {
                response: None,
                failure: Some(HelpExecutionFailure::Rejected),
                cleanup,
            };
        };
        let mut response_value = raw;
        if let Some(object) = response_value.as_object_mut() {
            if !object.contains_key("cleanup") {
                object.insert(
                    "cleanup".into(),
                    serde_json::to_value(&cleanup).unwrap_or(Value::Null),
                );
            }
        }
        let encoded = serde_json::to_vec(&response_value).unwrap_or_default();
        match parse_help_response(&encoded, &request) {
            Ok(response) => HelpExecution {
                response: Some(response),
                failure: None,
                cleanup,
            },
            Err(_) => HelpExecution {
                response: None,
                failure: Some(HelpExecutionFailure::Rejected),
                cleanup,
            },
        }
    }
}

impl Default for HelpExecutor {
    fn default() -> Self {
        Self::new(1, 8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grokptah_agent_sdk::{
        HelpAccess, HelpAccessMode, HelpAuthorization, HelpContextChunk, HelpDeadline,
        HelpDialect, HelpIdentity, HelpProvider, HelpSourceBinding,
    };

    fn request() -> HelpAuthorityRequest {
        HelpAuthorityRequest {
            schema: grokptah_agent_sdk::HELP_AUTHORITY_SCHEMA.into(),
            kind: HelpMessageKind::Request,
            request_id: "request-1".into(),
            authorization: HelpAuthorization {
                mode: HelpAccessMode::Public,
                authorized_capabilities: vec![],
            },
            identity: HelpIdentity {
                corpus_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                source_digest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                model_digest: "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".into(),
                model_id: "offline-help".into(),
                model_version: "1".into(),
            },
            provider: grokptah_agent_sdk::HelpProvider {
                profile: "profile".into(),
                tenant: "tenant".into(),
                model: "model".into(),
                route_revision: "route-1".into(),
                dialect: HelpDialect::BrokerNative,
            },
            deadline: HelpDeadline {
                deadline_at: (Utc::now() + chrono::Duration::seconds(2)).to_rfc3339(),
                max_duration_ms: 2_000,
            },
            query: "What is Help?".into(),
            context: vec![HelpContextChunk {
                chunk_id: "article#en.body.0".into(),
                article_id: "article".into(),
                access: HelpAccess::Public,
                required_capabilities: vec![],
                text: "Help answers cite source bytes.".into(),
                text_digest: "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd".into(),
                span_start: 0,
                span_end: "Help answers cite source bytes.".len(),
                source_bindings: vec![HelpSourceBinding {
                    source_id: "source".into(),
                    source_section_digest: "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".into(),
                    source_byte_length: 10,
                }],
            }],
            tools_disabled: true,
            conversation_disabled: true,
        }
    }

    #[tokio::test]
    async fn cancellation_aborts_and_joins_a_cancellation_ignoring_provider() {
        let executor = HelpExecutor::default();
        let provider: Arc<dyn HelpProvider> = Arc::new(
            |_request: HelpAuthorityRequest, _cancel: CancellationToken| async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                Err("provider details must not escape".into())
            },
        );
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let future = executor.execute(request(), provider, cancel_for_task);
        cancel.cancel();
        let result = future.await;
        assert_eq!(result.failure, Some(HelpExecutionFailure::Cancelled));
        assert_eq!(result.cleanup.status, HelpCleanupStatus::Finalized);
        assert_eq!(result.cleanup.provider_task, HelpProviderTask::Joined);
        assert_eq!(result.cleanup.queue_slot, HelpQueueSlot::Released);
        assert_eq!(result.cleanup.artifact_counts, Default::default());
    }

    #[tokio::test]
    async fn queue_is_bounded_without_creating_a_second_provider_task() {
        let executor = Arc::new(HelpExecutor::new(1, 0));
        let gate = Arc::new(tokio::sync::Notify::new());
        let gate_for_provider = gate.clone();
        let provider: Arc<dyn HelpProvider> = Arc::new(
            move |_request: HelpAuthorityRequest, _cancel: CancellationToken| {
                let gate = gate_for_provider.clone();
                async move {
                    gate.notified().await;
                    Err("closed".into())
                }
            },
        );
        let first = tokio::spawn({
            let executor = executor.clone();
            let provider = provider.clone();
            async move { executor.execute(request(), provider, CancellationToken::new()).await }
        });
        tokio::task::yield_now().await;
        let second = executor
            .execute(request(), provider.clone(), CancellationToken::new())
            .await;
        assert_eq!(second.failure, Some(HelpExecutionFailure::Capacity));
        gate.notify_one();
        let first_result = first.await.expect("first task joined");
        assert_eq!(first_result.failure, Some(HelpExecutionFailure::Transport));
        assert_eq!(executor.queued_count(), 0);
    }
}
