//! Trusted adapters for provider-managed coding workers.
//!
//! The browser and public SDK see only the projections from
//! `grokptah-agent-sdk`. Provider credentials stay in this native/server
//! boundary. The first adapter targets Cursor's Cloud Agents API v1; it does
//! not control a foreground Cursor desktop window and it never grants native
//! Computer Use authority.

use async_trait::async_trait;
use grokptah_agent_sdk::external_worker::{
    ExternalWorkerArtifact, ExternalWorkerExecutionMode, ExternalWorkerFollowUpRequest,
    ExternalWorkerLaunchRequest, ExternalWorkerLaunchResult, ExternalWorkerListPage,
    ExternalWorkerListQuery, ExternalWorkerProvider, ExternalWorkerRecord, ExternalWorkerRunRecord,
    ExternalWorkerState, ExternalWorkerSummary, MAX_EXTERNAL_WORKER_LIST_LIMIT,
};
use parking_lot::RwLock;
use reqwest::{Client, Method, StatusCode, Url};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

/// Public API base for Cursor Cloud Agents v1.
pub const CURSOR_CLOUD_API_BASE: &str = "https://api.cursor.com";

/// Errors returned by a trusted external-worker adapter.
#[derive(Debug, Error)]
pub enum ExternalWorkerAdapterError {
    /// The caller request or provider projection failed a safety check.
    #[error("external worker request is invalid: {0}")]
    InvalidRequest(&'static str),
    /// The adapter cannot service the requested provider.
    #[error("external worker provider is unsupported")]
    UnsupportedProvider,
    /// The provider returned a non-success response.
    #[error("external worker provider returned HTTP {status}")]
    Provider { status: StatusCode },
    /// The provider response could not be decoded or was incomplete.
    #[error("external worker provider response is invalid: {0}")]
    InvalidResponse(&'static str),
    /// The trusted transport failed before a response was received.
    #[error("external worker transport failed: {0}")]
    Transport(#[from] reqwest::Error),
    /// The configured API base URL is not safe for a server-side adapter.
    #[error("external worker API base URL is invalid")]
    InvalidBaseUrl,
}

/// Provider-neutral lifecycle operations required by the manager.
#[async_trait]
pub trait ExternalWorkerAdapter: Send + Sync {
    /// Provider family implemented by this adapter.
    fn provider(&self) -> ExternalWorkerProvider;

    /// Create an isolated worker and its initial run.
    async fn launch(
        &self,
        request: &ExternalWorkerLaunchRequest,
    ) -> Result<ExternalWorkerLaunchResult, ExternalWorkerAdapterError>;

    /// Read a redacted worker projection.
    async fn get_worker(
        &self,
        external_agent_id: &str,
    ) -> Result<ExternalWorkerRecord, ExternalWorkerAdapterError>;

    /// Read a redacted run projection.
    async fn get_run(
        &self,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError>;

    /// Queue a new prompt run on an existing active worker.
    async fn follow_up(
        &self,
        external_agent_id: &str,
        request: &ExternalWorkerFollowUpRequest,
    ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError>;

    /// List provider artifacts only when a content digest is available.
    async fn list_artifacts(
        &self,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<Vec<ExternalWorkerArtifact>, ExternalWorkerAdapterError>;

    /// Cancel an active run and verify its terminal projection.
    async fn cancel(
        &self,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError>;

    /// List redacted worker identity summaries.
    ///
    /// Default implementations fail closed. List pages must not invent
    /// repository or starting-ref fields omitted by the provider.
    async fn list_workers(
        &self,
        query: &ExternalWorkerListQuery,
    ) -> Result<ExternalWorkerListPage, ExternalWorkerAdapterError> {
        let _ = query;
        Err(ExternalWorkerAdapterError::UnsupportedProvider)
    }

    /// Archive a worker. Archive is explicit and reversible via `unarchive`.
    /// Cancellation or completion must not imply archive.
    async fn archive(
        &self,
        external_agent_id: &str,
    ) -> Result<ExternalWorkerRecord, ExternalWorkerAdapterError> {
        let _ = external_agent_id;
        Err(ExternalWorkerAdapterError::UnsupportedProvider)
    }

    /// Restore an archived worker so it can accept new runs.
    async fn unarchive(
        &self,
        external_agent_id: &str,
    ) -> Result<ExternalWorkerRecord, ExternalWorkerAdapterError> {
        let _ = external_agent_id;
        Err(ExternalWorkerAdapterError::UnsupportedProvider)
    }
}

/// Process-local registry for qualified external-worker providers.
///
/// Registration is deliberately explicit. A provider is not available merely
/// because a credential exists; the host must install a qualified adapter and
/// the manager still applies workspace, approval, and promotion policy.
#[derive(Default)]
pub struct ExternalWorkerRegistry {
    adapters: RwLock<HashMap<ExternalWorkerProvider, Arc<dyn ExternalWorkerAdapter>>>,
}

impl ExternalWorkerRegistry {
    /// Create an empty provider registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register or replace one provider adapter during host setup.
    pub fn register(&self, adapter: Arc<dyn ExternalWorkerAdapter>) {
        self.adapters.write().insert(adapter.provider(), adapter);
    }

    /// Return the adapter for a provider, if the host explicitly installed it.
    pub fn get(&self, provider: ExternalWorkerProvider) -> Option<Arc<dyn ExternalWorkerAdapter>> {
        self.adapters.read().get(&provider).cloned()
    }

    /// Return the provider families explicitly installed in this process.
    pub fn providers(&self) -> Vec<ExternalWorkerProvider> {
        let mut providers = self.adapters.read().keys().copied().collect::<Vec<_>>();
        providers.sort_by_key(|provider| *provider as u8);
        providers
    }
}

/// Trusted Cursor Cloud Agents API v1 adapter.
pub struct CursorCloudAdapter {
    http: Client,
    base_url: Url,
    api_key: String,
    allowed_repositories: Option<Arc<std::collections::BTreeSet<String>>>,
}

impl CursorCloudAdapter {
    /// Construct an adapter for the official Cursor API.
    ///
    /// The API key is held only by this trusted object. Callers must not pass
    /// it through browser DTOs, event journals, or prompt text.
    pub fn new(api_key: impl Into<String>) -> Result<Self, ExternalWorkerAdapterError> {
        Self::with_base_url(CURSOR_CLOUD_API_BASE, api_key)
    }

    /// Construct an adapter against a qualified HTTPS-compatible endpoint.
    ///
    /// This is intentionally public for enterprise-compatible Cursor gateway
    /// deployments, but production callers should keep the default host and
    /// qualify any alternate endpoint before enabling it.
    pub fn with_base_url(
        base_url: &str,
        api_key: impl Into<String>,
    ) -> Result<Self, ExternalWorkerAdapterError> {
        let base_url =
            Url::parse(base_url).map_err(|_| ExternalWorkerAdapterError::InvalidBaseUrl)?;
        if base_url.scheme() != "https"
            || base_url.host_str().is_none()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(ExternalWorkerAdapterError::InvalidBaseUrl);
        }
        if !crate::ssrf::check_url(base_url.as_str()).allow {
            return Err(ExternalWorkerAdapterError::InvalidBaseUrl);
        }
        let api_key = api_key.into();
        if api_key.trim().is_empty() || api_key.chars().any(|c| matches!(c, '\r' | '\n' | '\0')) {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "provider API key must be non-empty and free of control characters",
            ));
        }
        let http = Client::builder()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| ExternalWorkerAdapterError::InvalidBaseUrl)?;
        Ok(Self {
            http,
            base_url,
            api_key,
            allowed_repositories: None,
        })
    }

    /// Install the explicit repository allowlist required for live launches.
    /// The adapter refuses to create a worker until this is configured.
    pub fn with_repository_allowlist<I, S>(
        mut self,
        repositories: I,
    ) -> Result<Self, ExternalWorkerAdapterError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut allowlist = std::collections::BTreeSet::new();
        for repository in repositories {
            allowlist.insert(repository_identity(&github_repository_url(
                repository.as_ref(),
            )?)?);
        }
        if allowlist.is_empty() {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "Cursor repository allowlist must not be empty",
            ));
        }
        self.allowed_repositories = Some(Arc::new(allowlist));
        Ok(self)
    }

    #[cfg(test)]
    fn for_test(base_url: &str) -> Self {
        Self {
            http: Client::builder()
                .timeout(Duration::from_secs(10))
                .connect_timeout(Duration::from_secs(2))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("test client is valid"),
            base_url: Url::parse(base_url).expect("test server URL is valid"),
            api_key: "synthetic-cursor-key".into(),
            allowed_repositories: Some(Arc::new(
                ["chriscase/GrokPtah".to_string()].into_iter().collect(),
            )),
        }
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<T, ExternalWorkerAdapterError> {
        self.request_with_query(method, path, &[], body).await
    }

    async fn request_with_query<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
    ) -> Result<T, ExternalWorkerAdapterError> {
        let mut url = self
            .base_url
            .join(path)
            .map_err(|_| ExternalWorkerAdapterError::InvalidResponse("provider URL is invalid"))?;
        if !query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in query {
                pairs.append_pair(key, value);
            }
        }
        let mut request = self
            .http
            .request(method, url)
            // Cursor documents Basic auth with the API key as the username.
            // Never put the key in a URL or a serializable request DTO.
            .basic_auth(&self.api_key, Some(""));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            // Deliberately do not include the provider body: it may echo
            // credential-bearing diagnostics and would enter our error log.
            return Err(ExternalWorkerAdapterError::Provider { status });
        }
        let bytes = response.bytes().await?;
        serde_json::from_slice(&bytes).map_err(|_| {
            ExternalWorkerAdapterError::InvalidResponse("provider response is malformed")
        })
    }

    fn checked_id(value: &str) -> Result<&str, ExternalWorkerAdapterError> {
        Self::checked_opaque(value, false)
    }

    fn checked_cursor(
        value: &str,
        from_provider: bool,
    ) -> Result<&str, ExternalWorkerAdapterError> {
        Self::checked_opaque(value, from_provider)
    }

    fn checked_opaque(
        value: &str,
        from_provider: bool,
    ) -> Result<&str, ExternalWorkerAdapterError> {
        if value.is_empty()
            || value.len() > 256
            || value
                .chars()
                .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '=')))
        {
            let message = "provider identity is not a safe opaque ID";
            return Err(if from_provider {
                ExternalWorkerAdapterError::InvalidResponse(message)
            } else {
                ExternalWorkerAdapterError::InvalidRequest(message)
            });
        }
        Ok(value)
    }

    async fn list_provider_runs(
        &self,
        external_agent_id: &str,
    ) -> Result<Vec<CursorRun>, ExternalWorkerAdapterError> {
        let id = Self::checked_id(external_agent_id)?;
        let response: CursorRuns = self
            .request(Method::GET, &format!("/v1/agents/{id}/runs"), None)
            .await?;
        Ok(response.items)
    }

    async fn set_archived(
        &self,
        external_agent_id: &str,
        archived: bool,
    ) -> Result<ExternalWorkerRecord, ExternalWorkerAdapterError> {
        let agent_id = Self::checked_id(external_agent_id)?;
        let before = self.get_worker(agent_id).await?;
        if archived {
            if before.state == ExternalWorkerState::Archived {
                return Ok(before);
            }
            if before.state != ExternalWorkerState::Ready {
                return Err(ExternalWorkerAdapterError::InvalidRequest(
                    "Cursor worker is not eligible for archive",
                ));
            }
        } else if before.state != ExternalWorkerState::Archived {
            if matches!(
                before.state,
                ExternalWorkerState::Unknown | ExternalWorkerState::Failed
            ) {
                return Err(ExternalWorkerAdapterError::InvalidRequest(
                    "Cursor worker is not eligible for unarchive",
                ));
            }
            return Ok(before);
        }
        let path = if archived {
            format!("/v1/agents/{agent_id}/archive")
        } else {
            format!("/v1/agents/{agent_id}/unarchive")
        };
        let response: CursorIdResponse = self.request(Method::POST, &path, None).await?;
        if response.id != agent_id {
            return Err(ExternalWorkerAdapterError::InvalidResponse(
                "Cursor archive identity does not match the request",
            ));
        }
        let after = self.get_worker(agent_id).await?;
        if after.external_agent_id != agent_id
            || after.provider != ExternalWorkerProvider::CursorCloud
            || after.repository != before.repository
            || after.starting_ref != before.starting_ref
        {
            return Err(ExternalWorkerAdapterError::InvalidResponse(
                "Cursor archive identity does not match the request",
            ));
        }
        if archived {
            if after.state != ExternalWorkerState::Archived {
                return Err(ExternalWorkerAdapterError::InvalidResponse(
                    "Cursor archive did not return an archived worker",
                ));
            }
        } else if matches!(
            after.state,
            ExternalWorkerState::Archived
                | ExternalWorkerState::Unknown
                | ExternalWorkerState::Failed
        ) {
            return Err(ExternalWorkerAdapterError::InvalidResponse(
                "Cursor unarchive did not restore an active worker",
            ));
        }
        Ok(after)
    }
}

#[async_trait]
impl ExternalWorkerAdapter for CursorCloudAdapter {
    fn provider(&self) -> ExternalWorkerProvider {
        ExternalWorkerProvider::CursorCloud
    }

    async fn launch(
        &self,
        request: &ExternalWorkerLaunchRequest,
    ) -> Result<ExternalWorkerLaunchResult, ExternalWorkerAdapterError> {
        request
            .validate()
            .map_err(ExternalWorkerAdapterError::InvalidRequest)?;
        if request.provider != ExternalWorkerProvider::CursorCloud {
            return Err(ExternalWorkerAdapterError::UnsupportedProvider);
        }
        if request.execution_mode != ExternalWorkerExecutionMode::Isolated {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "Cursor workers must be isolated",
            ));
        }
        if request.auto_create_pr {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "pull-request creation requires a separate approval action",
            ));
        }
        let repository_url = github_repository_url(&request.repository)?;
        let repository_identity = repository_identity(&repository_url)?;
        let Some(allowlist) = &self.allowed_repositories else {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "Cursor repository allowlist is not configured",
            ));
        };
        if !allowlist.contains(&repository_identity) {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "repository is not in the Cursor allowlist",
            ));
        }
        let mut payload = json!({
            "prompt": { "text": request.prompt },
            "repos": [{
                "url": repository_url,
                "startingRef": request.starting_ref,
            }],
            // Cursor's v1 API uses the presence of `repos` to select its
            // hosted cloud environment; an explicit named `env` is mutually
            // exclusive with `repos` and would make this request invalid.
            "workOnCurrentBranch": false,
            "autoCreatePR": false,
        });
        if let Some(model) = &request.model {
            payload["model"] = json!({ "id": model });
        }
        // GrokPtah's durable idempotency ledger owns retries. Cursor's
        // client-supplied agentId is accepted only for its strict bc-<uuid>
        // shape; arbitrary request IDs must not be sent as provider IDs.
        if is_cursor_agent_id(&request.request_id) {
            payload["agentId"] = json!(request.request_id);
        }
        let response: CursorCreateResponse = self
            .request(Method::POST, "/v1/agents", Some(payload))
            .await?;
        let worker = worker_record(
            &response.agent,
            Some(&request.repository),
            Some(&request.starting_ref),
        )?;
        let run = run_record(&response.run, &response.agent.id)?;
        let result = ExternalWorkerLaunchResult { worker, run };
        result
            .validate()
            .map_err(ExternalWorkerAdapterError::InvalidResponse)?;
        Ok(result)
    }

    async fn get_worker(
        &self,
        external_agent_id: &str,
    ) -> Result<ExternalWorkerRecord, ExternalWorkerAdapterError> {
        let id = Self::checked_id(external_agent_id)?;
        let response: CursorAgent = self
            .request(Method::GET, &format!("/v1/agents/{id}"), None)
            .await?;
        worker_record(&response, None, None)
    }

    async fn get_run(
        &self,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
        let agent_id = Self::checked_id(external_agent_id)?;
        let run_id = Self::checked_id(external_run_id)?;
        let response: CursorRun = self
            .request(
                Method::GET,
                &format!("/v1/agents/{agent_id}/runs/{run_id}"),
                None,
            )
            .await?;
        run_record(&response, agent_id)
    }

    async fn follow_up(
        &self,
        external_agent_id: &str,
        request: &ExternalWorkerFollowUpRequest,
    ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
        request
            .validate()
            .map_err(ExternalWorkerAdapterError::InvalidRequest)?;
        if request.bounds.is_some() {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "Cursor follow-up bounds are not supported by the provider API",
            ));
        }
        let agent_id = Self::checked_id(external_agent_id)?;
        let worker = self.get_worker(agent_id).await?;
        if matches!(
            worker.state,
            ExternalWorkerState::Unknown
                | ExternalWorkerState::Failed
                | ExternalWorkerState::Cancelled
                | ExternalWorkerState::Archived
        ) {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "Cursor worker is not eligible for a follow-up",
            ));
        }
        let active_runs = self.list_provider_runs(agent_id).await?;
        if active_runs.iter().any(|run| {
            matches!(
                run_state(&run.status),
                ExternalWorkerState::Provisioning | ExternalWorkerState::Running
            )
        }) {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "Cursor worker already has an active run",
            ));
        }
        let response: CursorFollowUpResponse = self
            .request(
                Method::POST,
                &format!("/v1/agents/{agent_id}/runs"),
                Some(json!({ "prompt": { "text": request.prompt } })),
            )
            .await?;
        run_record(&response.run, agent_id)
    }

    async fn list_artifacts(
        &self,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<Vec<ExternalWorkerArtifact>, ExternalWorkerAdapterError> {
        let id = Self::checked_id(external_agent_id)?;
        let run_id = Self::checked_id(external_run_id)?;
        let response: CursorArtifacts = self
            .request(Method::GET, &format!("/v1/agents/{id}/artifacts"), None)
            .await?;
        response
            .items
            .into_iter()
            .map(|item| {
                if !item.path.starts_with("artifacts/") {
                    return Err(ExternalWorkerAdapterError::InvalidResponse(
                        "Cursor artifact path is not provider-relative",
                    ));
                }
                if item.run_id.as_deref() != Some(run_id) {
                    return Err(ExternalWorkerAdapterError::InvalidResponse(
                        "Cursor artifact is not attributed to the requested run",
                    ));
                }
                let digest = item
                    .digest
                    .ok_or(ExternalWorkerAdapterError::InvalidResponse(
                        "Cursor artifact listing did not provide a content digest",
                    ))?;
                let artifact = ExternalWorkerArtifact {
                    path: item.path,
                    digest,
                    size_bytes: item.size_bytes,
                };
                artifact
                    .validate()
                    .map_err(ExternalWorkerAdapterError::InvalidResponse)?;
                Ok(artifact)
            })
            .collect()
    }

    async fn cancel(
        &self,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
        let agent_id = Self::checked_id(external_agent_id)?;
        let run_id = Self::checked_id(external_run_id)?;
        let before = self.get_run(agent_id, run_id).await?;
        if before.state == ExternalWorkerState::Cancelled {
            return Ok(before);
        }
        if !matches!(
            before.state,
            ExternalWorkerState::Provisioning | ExternalWorkerState::Running
        ) {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "Cursor run is not cancellable",
            ));
        }
        match self
            .request::<CursorIdResponse>(
                Method::POST,
                &format!("/v1/agents/{agent_id}/runs/{run_id}/cancel"),
                None,
            )
            .await
        {
            Ok(_) => {}
            Err(ExternalWorkerAdapterError::Provider { status })
                if status == StatusCode::CONFLICT => {}
            Err(error) => return Err(error),
        }
        let run = self.get_run(agent_id, run_id).await?;
        if run.state != ExternalWorkerState::Cancelled {
            return Err(ExternalWorkerAdapterError::InvalidResponse(
                "Cursor cancellation did not return a terminal cancelled run",
            ));
        }
        Ok(run)
    }

    async fn list_workers(
        &self,
        query: &ExternalWorkerListQuery,
    ) -> Result<ExternalWorkerListPage, ExternalWorkerAdapterError> {
        query
            .validate()
            .map_err(ExternalWorkerAdapterError::InvalidRequest)?;
        let limit = query.limit.unwrap_or(20);
        if limit > MAX_EXTERNAL_WORKER_LIST_LIMIT {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "list limit must be between 1 and 100",
            ));
        }
        let limit_value = limit.to_string();
        let include_archived = if query.include_archived {
            "true"
        } else {
            "false"
        };
        let mut pairs = vec![
            ("limit", limit_value.as_str()),
            ("includeArchived", include_archived),
        ];
        if let Some(cursor) = &query.cursor {
            let cursor = Self::checked_cursor(cursor, false)?;
            pairs.push(("cursor", cursor));
        }
        let response: CursorAgentList = self
            .request_with_query(Method::GET, "/v1/agents", &pairs, None)
            .await?;
        if response.items.len() > limit as usize {
            return Err(ExternalWorkerAdapterError::InvalidResponse(
                "Cursor list exceeded the requested page size",
            ));
        }
        let items = response
            .items
            .iter()
            .map(worker_summary)
            .collect::<Result<Vec<_>, _>>()?;
        if !query.include_archived
            && items
                .iter()
                .any(|item| item.state == ExternalWorkerState::Archived)
        {
            return Err(ExternalWorkerAdapterError::InvalidResponse(
                "Cursor list included archived workers without includeArchived",
            ));
        }
        if let Some(next_cursor) = &response.next_cursor {
            Self::checked_cursor(next_cursor, true)?;
        }
        let page = ExternalWorkerListPage {
            items,
            next_cursor: response.next_cursor,
        };
        page.validate()
            .map_err(ExternalWorkerAdapterError::InvalidResponse)?;
        Ok(page)
    }

    async fn archive(
        &self,
        external_agent_id: &str,
    ) -> Result<ExternalWorkerRecord, ExternalWorkerAdapterError> {
        self.set_archived(external_agent_id, true).await
    }

    async fn unarchive(
        &self,
        external_agent_id: &str,
    ) -> Result<ExternalWorkerRecord, ExternalWorkerAdapterError> {
        self.set_archived(external_agent_id, false).await
    }
}

#[derive(Debug, Deserialize)]
struct CursorCreateResponse {
    agent: CursorAgent,
    run: CursorRun,
}

#[derive(Debug, Deserialize)]
struct CursorFollowUpResponse {
    run: CursorRun,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CursorAgent {
    id: String,
    #[serde(default)]
    url: Option<String>,
    status: String,
    #[serde(default)]
    repos: Vec<CursorRepo>,
    #[serde(default, rename = "autoCreatePR")]
    auto_create_pr: Option<bool>,
    #[serde(default)]
    work_on_current_branch: Option<bool>,
    #[serde(default)]
    env: Option<CursorEnvironment>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Deserialize)]
struct CursorEnvironment {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CursorRepo {
    url: String,
    #[serde(default)]
    starting_ref: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CursorRun {
    id: String,
    agent_id: String,
    status: String,
    created_at: String,
    updated_at: String,
    #[serde(default)]
    result: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CursorIdResponse {
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CursorAgentList {
    items: Vec<CursorAgentListItem>,
    #[serde(default)]
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CursorAgentListItem {
    id: String,
    #[serde(default)]
    url: Option<String>,
    status: String,
    #[serde(default)]
    env: Option<CursorEnvironment>,
    created_at: String,
    updated_at: String,
    #[serde(default)]
    latest_run_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CursorArtifacts {
    #[serde(default)]
    items: Vec<CursorArtifact>,
}

#[derive(Debug, Deserialize)]
struct CursorRuns {
    #[serde(default)]
    items: Vec<CursorRun>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CursorArtifact {
    path: String,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    digest: Option<String>,
    #[serde(default)]
    size_bytes: Option<u64>,
}

fn github_repository_url(repository: &str) -> Result<String, ExternalWorkerAdapterError> {
    if repository.starts_with("https://github.com/") {
        if repository.contains('?')
            || repository.contains('#')
            || repository.ends_with('/')
            || repository[19..].split('/').count() != 2
        {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "repository must identify exactly one GitHub repository",
            ));
        }
        return Ok(repository.to_string());
    }
    if repository.is_empty()
        || repository.starts_with('/')
        || repository.contains('\\')
        || repository.split('/').count() != 2
        || repository.split('/').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || part
                    .chars()
                    .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        })
    {
        return Err(ExternalWorkerAdapterError::InvalidRequest(
            "repository must be owner/name or a GitHub HTTPS URL",
        ));
    }
    Ok(format!("https://github.com/{repository}"))
}

fn is_cursor_agent_id(value: &str) -> bool {
    let Some(uuid) = value.strip_prefix("bc-") else {
        return false;
    };
    uuid.len() == 36
        && uuid.chars().enumerate().all(|(index, character)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                character == '-'
            } else {
                character.is_ascii_hexdigit()
            }
        })
}

fn worker_record(
    agent: &CursorAgent,
    expected_repository: Option<&str>,
    expected_ref: Option<&str>,
) -> Result<ExternalWorkerRecord, ExternalWorkerAdapterError> {
    if agent.status.trim().is_empty() {
        return Err(ExternalWorkerAdapterError::InvalidResponse(
            "Cursor worker status is missing",
        ));
    }
    if agent.auto_create_pr != Some(false) || agent.work_on_current_branch != Some(false) {
        return Err(ExternalWorkerAdapterError::InvalidResponse(
            "Cursor response did not prove PR creation and current-branch writes are disabled",
        ));
    }
    if let Some(env) = &agent.env {
        if env.kind != "cloud" {
            return Err(ExternalWorkerAdapterError::InvalidResponse(
                "Cursor response environment is not hosted cloud",
            ));
        }
    }
    if agent.repos.len() != 1 {
        return Err(ExternalWorkerAdapterError::InvalidResponse(
            "Cursor worker must have exactly one repository",
        ));
    }
    let repo = &agent.repos[0];
    let repository = repository_identity(&repo.url)?;
    if let Some(expected) = expected_repository {
        let expected = repository_identity(&github_repository_url(expected)?)?;
        if repository != expected {
            return Err(ExternalWorkerAdapterError::InvalidResponse(
                "Cursor response repository differs from the exact request",
            ));
        }
    }
    if let (Some(expected), Some(actual)) = (expected_ref, repo.starting_ref.as_deref()) {
        if !refs_equal(expected, actual) {
            return Err(ExternalWorkerAdapterError::InvalidResponse(
                "Cursor response starting ref differs from the exact request",
            ));
        }
    }
    let worker = ExternalWorkerRecord {
        provider: ExternalWorkerProvider::CursorCloud,
        provider_id: None,
        external_agent_id: agent.id.clone(),
        repository,
        starting_ref: repo.starting_ref.clone().ok_or(
            ExternalWorkerAdapterError::InvalidResponse(
                "Cursor worker did not return a starting ref",
            ),
        )?,
        state: agent_state(&agent.status),
        branch: None,
        worker_url: agent.url.clone(),
        created_at: agent.created_at.clone(),
        updated_at: agent.updated_at.clone(),
    };
    worker
        .validate()
        .map_err(ExternalWorkerAdapterError::InvalidResponse)?;
    Ok(worker)
}

fn worker_summary(
    agent: &CursorAgentListItem,
) -> Result<ExternalWorkerSummary, ExternalWorkerAdapterError> {
    if agent.status.trim().is_empty() {
        return Err(ExternalWorkerAdapterError::InvalidResponse(
            "Cursor worker status is missing",
        ));
    }
    if let Some(env) = &agent.env {
        if env.kind != "cloud" {
            return Err(ExternalWorkerAdapterError::InvalidResponse(
                "Cursor response environment is not hosted cloud",
            ));
        }
    }
    if let Some(latest_run_id) = &agent.latest_run_id {
        CursorCloudAdapter::checked_opaque(latest_run_id, true)?;
    }
    CursorCloudAdapter::checked_opaque(&agent.id, true)?;
    let summary = ExternalWorkerSummary {
        provider: ExternalWorkerProvider::CursorCloud,
        provider_id: None,
        external_agent_id: agent.id.clone(),
        state: agent_state(&agent.status),
        worker_url: agent.url.clone(),
        latest_run_id: agent.latest_run_id.clone(),
        created_at: agent.created_at.clone(),
        updated_at: agent.updated_at.clone(),
    };
    summary
        .validate()
        .map_err(ExternalWorkerAdapterError::InvalidResponse)?;
    Ok(summary)
}

fn run_record(
    run: &CursorRun,
    expected_agent_id: &str,
) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
    if run.agent_id != expected_agent_id {
        return Err(ExternalWorkerAdapterError::InvalidResponse(
            "Cursor run belongs to a different agent",
        ));
    }
    let run = ExternalWorkerRunRecord {
        external_agent_id: run.agent_id.clone(),
        external_run_id: run.id.clone(),
        state: run_state(&run.status),
        last_seq: 0,
        // Cursor's final reply is provider text, not a trusted projection.
        // Keep it only when it is already bounded and free of path/credential
        // needles; a suspicious result is omitted rather than leaked.
        terminal_result: run.result.as_deref().and_then(safe_terminal_result),
        created_at: run.created_at.clone(),
        updated_at: run.updated_at.clone(),
    };
    run.validate()
        .map_err(ExternalWorkerAdapterError::InvalidResponse)?;
    Ok(run)
}

fn safe_terminal_result(value: &str) -> Option<String> {
    if value.contains('\0') {
        return None;
    }
    // Preserve readable multi-line final replies without allowing control
    // characters to cross the browser projection.
    let value = value.replace('\r', " ").replace('\n', " ");
    let lower = value.to_ascii_lowercase();
    if value.trim().is_empty()
        || value.len() > 4_096
        || value.contains("http://")
        || value.contains("https://")
        || lower.contains("authorization")
        || lower.contains("bearer")
        || lower.contains("api_key")
        || lower.contains("password")
        || lower.contains("cookie")
        || lower.contains("private key")
        || lower.contains("secret")
        || lower.contains("clipboard")
        || value.contains("/Users/")
        || value.contains("/private/")
        || value.contains("/tmp/")
        || value
            .get(1..3)
            .is_some_and(|drive| drive.eq_ignore_ascii_case(":\\"))
        || value.contains("\\Users\\")
    {
        return None;
    }
    Some(value)
}

fn repository_identity(value: &str) -> Result<String, ExternalWorkerAdapterError> {
    let value = value
        .strip_prefix("https://github.com/")
        .or_else(|| value.strip_prefix("github.com/"))
        .ok_or(ExternalWorkerAdapterError::InvalidResponse(
            "Cursor repository is not a GitHub URL",
        ))?;
    let value = value.trim_end_matches('/');
    if value.split('/').count() != 2 || value.contains('?') || value.contains('#') {
        return Err(ExternalWorkerAdapterError::InvalidResponse(
            "Cursor repository identity is malformed",
        ));
    }
    Ok(value.to_string())
}

fn refs_equal(left: &str, right: &str) -> bool {
    left == right
        || left.strip_prefix("refs/heads/") == Some(right)
        || right.strip_prefix("refs/heads/") == Some(left)
}

fn agent_state(status: &str) -> ExternalWorkerState {
    match status.to_ascii_uppercase().as_str() {
        "ACTIVE" => ExternalWorkerState::Ready,
        "ARCHIVED" => ExternalWorkerState::Archived,
        "DELETED" => ExternalWorkerState::Failed,
        _ => ExternalWorkerState::Unknown,
    }
}

fn run_state(status: &str) -> ExternalWorkerState {
    match status.to_ascii_uppercase().as_str() {
        "CREATING" => ExternalWorkerState::Provisioning,
        "RUNNING" => ExternalWorkerState::Running,
        "FINISHED" | "COMPLETED" => ExternalWorkerState::Completed,
        "CANCELLED" => ExternalWorkerState::Cancelled,
        "ERROR" | "EXPIRED" | "FAILED" => ExternalWorkerState::Failed,
        _ => ExternalWorkerState::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path, Query, State};
    use axum::http::{HeaderMap, StatusCode, Uri};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};

    const AGENT_1: &str = "bc-00000000-0000-0000-0000-000000000001";
    const AGENT_2: &str = "bc-00000000-0000-0000-0000-000000000002";
    const RUN_1: &str = "run-00000000-0000-0000-0000-000000000001";

    #[derive(Clone, Default)]
    struct FakeCursorState {
        launch_requests: Arc<Mutex<Vec<Value>>>,
        cancelled: Arc<Mutex<bool>>,
        archived: Arc<Mutex<BTreeSet<String>>>,
        list_queries: Arc<Mutex<Vec<Value>>>,
        request_urls: Arc<Mutex<Vec<String>>>,
        authorization_headers: Arc<Mutex<Vec<Option<String>>>>,
        archive_posts: Arc<Mutex<Vec<String>>>,
        unarchive_posts: Arc<Mutex<Vec<String>>>,
    }

    #[derive(Debug, Deserialize, Default)]
    struct FakeListQuery {
        limit: Option<u32>,
        cursor: Option<String>,
        #[serde(rename = "includeArchived")]
        include_archived: Option<bool>,
    }

    fn capture(state: &FakeCursorState, uri: &Uri, headers: &HeaderMap) {
        state.request_urls.lock().unwrap().push(uri.to_string());
        state.authorization_headers.lock().unwrap().push(
            headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
        );
    }

    fn known_agent(id: &str) -> bool {
        id == AGENT_1 || id == AGENT_2
    }

    fn fake_agent() -> Value {
        fake_agent_record(AGENT_1, false)
    }

    fn fake_agent_record(id: &str, archived: bool) -> Value {
        json!({
            "id": id,
            "url": format!("https://cursor.com/agents/{id}"),
            "status": if archived { "ARCHIVED" } else { "ACTIVE" },
            "repos": [{"url": "https://github.com/chriscase/GrokPtah", "startingRef": "main"}],
            "autoCreatePR": false,
            "workOnCurrentBranch": false,
            "env": {"type": "cloud"},
            "createdAt": "2026-08-24T00:00:00Z",
            "updatedAt": "2026-08-24T00:00:01Z"
        })
    }

    fn fake_list_item(id: &str, archived: bool, updated: &str) -> Value {
        json!({
            "id": id,
            "name": "bounded fixture",
            "status": if archived { "ARCHIVED" } else { "ACTIVE" },
            "env": {"type": "cloud"},
            "url": format!("https://cursor.com/agents/{id}"),
            "createdAt": "2026-08-24T00:00:00Z",
            "updatedAt": updated,
            "latestRunId": RUN_1
        })
    }

    fn launch_request() -> ExternalWorkerLaunchRequest {
        ExternalWorkerLaunchRequest {
            request_id: "request-1".into(),
            provider: ExternalWorkerProvider::CursorCloud,
            provider_id: None,
            repository: "chriscase/GrokPtah".into(),
            starting_ref: "main".into(),
            prompt: "Run the bounded fixture".into(),
            model: Some("composer-2".into()),
            execution_mode: ExternalWorkerExecutionMode::Isolated,
            auto_create_pr: false,
            bounds: None,
        }
    }

    async fn spawn_app(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}")
    }

    fn lifecycle_router(state: FakeCursorState) -> Router {
        Router::new()
            .route("/v1/agents", post(fake_create).get(fake_list))
            .route("/v1/agents/{id}", get(fake_agent_read))
            .route("/v1/agents/{id}/archive", post(fake_archive))
            .route("/v1/agents/{id}/unarchive", post(fake_unarchive))
            .route(
                "/v1/agents/bc-00000000-0000-0000-0000-000000000001/runs/run-00000000-0000-0000-0000-000000000001",
                get(fake_run_read),
            )
            .route(
                "/v1/agents/bc-00000000-0000-0000-0000-000000000001/runs",
                post(fake_follow_up).get(fake_runs),
            )
            .route(
                "/v1/agents/bc-00000000-0000-0000-0000-000000000001/runs/run-00000000-0000-0000-0000-000000000001/cancel",
                post(fake_cancel),
            )
            .route(
                "/v1/agents/bc-00000000-0000-0000-0000-000000000001/artifacts",
                get(fake_artifacts),
            )
            .with_state(state)
    }

    async fn fake_create(
        State(state): State<FakeCursorState>,
        uri: Uri,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        capture(&state, &uri, &headers);
        state.launch_requests.lock().unwrap().push(body);
        Json(json!({
            "agent": fake_agent(),
            "run": {
                "id": RUN_1,
                "agentId": AGENT_1,
                "status": "CREATING",
                "createdAt": "2026-08-24T00:00:00Z",
                "updatedAt": "2026-08-24T00:00:01Z"
            }
        }))
    }

    async fn fake_list(
        State(state): State<FakeCursorState>,
        uri: Uri,
        headers: HeaderMap,
        Query(query): Query<FakeListQuery>,
    ) -> Json<Value> {
        capture(&state, &uri, &headers);
        state.list_queries.lock().unwrap().push(json!({
            "limit": query.limit,
            "cursor": query.cursor,
            "includeArchived": query.include_archived,
        }));
        let archived = state.archived.lock().unwrap().clone();
        let mut items = vec![
            fake_list_item(AGENT_2, archived.contains(AGENT_2), "2026-08-24T00:00:05Z"),
            fake_list_item(AGENT_1, archived.contains(AGENT_1), "2026-08-24T00:00:01Z"),
        ];
        if query.include_archived != Some(true) {
            items.retain(|item| item["status"] != "ARCHIVED");
        }
        if let Some(cursor) = &query.cursor {
            if let Some(start) = items.iter().position(|item| item["id"] == *cursor) {
                items = items[start..].to_vec();
            } else {
                items.clear();
            }
        }
        let limit = query.limit.unwrap_or(20) as usize;
        let next_cursor = items
            .get(limit)
            .and_then(|item| item["id"].as_str())
            .map(str::to_owned);
        items.truncate(limit);
        let mut page = json!({ "items": items });
        if let Some(next_cursor) = next_cursor {
            page["nextCursor"] = json!(next_cursor);
        }
        Json(page)
    }

    async fn fake_agent_read(
        State(state): State<FakeCursorState>,
        Path(id): Path<String>,
        uri: Uri,
        headers: HeaderMap,
    ) -> Result<Json<Value>, StatusCode> {
        capture(&state, &uri, &headers);
        if !known_agent(&id) {
            return Err(StatusCode::NOT_FOUND);
        }
        let archived = state.archived.lock().unwrap().contains(&id);
        Ok(Json(fake_agent_record(&id, archived)))
    }

    async fn fake_archive(
        State(state): State<FakeCursorState>,
        Path(id): Path<String>,
        uri: Uri,
        headers: HeaderMap,
    ) -> Result<Json<Value>, StatusCode> {
        capture(&state, &uri, &headers);
        if !known_agent(&id) {
            return Err(StatusCode::NOT_FOUND);
        }
        state.archive_posts.lock().unwrap().push(id.clone());
        state.archived.lock().unwrap().insert(id.clone());
        Ok(Json(json!({ "id": id })))
    }

    async fn fake_unarchive(
        State(state): State<FakeCursorState>,
        Path(id): Path<String>,
        uri: Uri,
        headers: HeaderMap,
    ) -> Result<Json<Value>, StatusCode> {
        capture(&state, &uri, &headers);
        if !known_agent(&id) {
            return Err(StatusCode::NOT_FOUND);
        }
        state.unarchive_posts.lock().unwrap().push(id.clone());
        state.archived.lock().unwrap().remove(&id);
        Ok(Json(json!({ "id": id })))
    }

    async fn fake_run_read(State(state): State<FakeCursorState>) -> Json<Value> {
        let cancelled = *state.cancelled.lock().unwrap();
        Json(json!({
            "id": RUN_1,
            "agentId": AGENT_1,
            "status": if cancelled { "CANCELLED" } else { "RUNNING" },
            "createdAt": "2026-08-24T00:00:00Z",
            "updatedAt": "2026-08-24T00:00:02Z",
            "result": "completed\nwith two lines"
        }))
    }

    async fn fake_follow_up(
        State(_state): State<FakeCursorState>,
        Json(_body): Json<Value>,
    ) -> Json<Value> {
        Json(json!({
            "run": {
                "id": "run-00000000-0000-0000-0000-000000000002",
                "agentId": AGENT_1,
                "status": "CREATING",
                "createdAt": "2026-08-24T00:00:03Z",
                "updatedAt": "2026-08-24T00:00:03Z"
            }
        }))
    }

    async fn fake_runs(State(_state): State<FakeCursorState>) -> Json<Value> {
        Json(json!({ "items": [] }))
    }

    async fn fake_cancel(State(state): State<FakeCursorState>) -> Json<Value> {
        *state.cancelled.lock().unwrap() = true;
        Json(json!({"id": RUN_1}))
    }

    async fn fake_artifacts(State(_state): State<FakeCursorState>) -> Json<Value> {
        Json(json!({
            "items": [{
                "path": "artifacts/report.md",
                "runId": RUN_1,
                "digest": "sha256:abc",
                "sizeBytes": 12
            }]
        }))
    }

    #[test]
    fn repository_urls_are_normalized_without_accepting_paths() {
        assert_eq!(
            github_repository_url("chriscase/GrokPtah").unwrap(),
            "https://github.com/chriscase/GrokPtah"
        );
        assert!(github_repository_url("https://github.com/a/b/c").is_err());
        assert!(github_repository_url("https://github.com/a/b?token=1").is_err());
    }

    #[test]
    fn live_adapter_requires_safe_base_and_explicit_allowlist() {
        assert!(CursorCloudAdapter::with_base_url("https://127.0.0.1", "key").is_err());
        assert!(
            CursorCloudAdapter::with_base_url("https://api.cursor.com", "key")
                .unwrap()
                .with_repository_allowlist(["chriscase/GrokPtah"])
                .is_ok()
        );
        assert!(
            CursorCloudAdapter::with_base_url("https://api.cursor.com", "key")
                .unwrap()
                .with_repository_allowlist(std::iter::empty::<&str>())
                .is_err()
        );
    }

    #[test]
    fn ref_comparison_allows_only_the_unambiguous_heads_prefix() {
        assert!(refs_equal("main", "main"));
        assert!(refs_equal("refs/heads/main", "main"));
        assert!(!refs_equal("main", "refs/tags/main"));
    }

    #[test]
    fn arbitrary_request_ids_are_not_sent_as_cursor_agent_ids() {
        assert!(is_cursor_agent_id(
            "bc-00000000-0000-0000-0000-000000000001"
        ));
        assert!(!is_cursor_agent_id("bc-request-1"));
    }

    #[test]
    fn registry_exposes_only_explicitly_installed_providers() {
        let registry = ExternalWorkerRegistry::new();
        assert!(registry.providers().is_empty());
        let adapter = Arc::new(CursorCloudAdapter::new("synthetic-key").unwrap());
        registry.register(adapter);
        assert_eq!(
            registry.providers(),
            vec![ExternalWorkerProvider::CursorCloud]
        );
        assert!(registry.get(ExternalWorkerProvider::CursorCloud).is_some());
        assert!(registry
            .get(ExternalWorkerProvider::ClaudeCodeCloud)
            .is_none());
    }

    #[test]
    fn documented_auto_create_pr_field_is_required_to_prove_write_safety() {
        let agent: CursorAgent = serde_json::from_value(fake_agent()).unwrap();
        assert_eq!(agent.auto_create_pr, Some(false));
        assert_eq!(agent.work_on_current_branch, Some(false));
        let camel_pr = json!({
            "id": AGENT_1,
            "status": "ACTIVE",
            "repos": [{"url": "https://github.com/chriscase/GrokPtah", "startingRef": "main"}],
            "autoCreatePr": false,
            "workOnCurrentBranch": false,
            "createdAt": "2026-08-24T00:00:00Z",
            "updatedAt": "2026-08-24T00:00:01Z"
        });
        let undocumented: CursorAgent = serde_json::from_value(camel_pr).unwrap();
        assert_eq!(undocumented.auto_create_pr, None);
        assert!(worker_record(&undocumented, None, None).is_err());
    }

    #[test]
    fn provider_statuses_fail_closed() {
        assert_eq!(run_state("RUNNING"), ExternalWorkerState::Running);
        assert_eq!(run_state("CANCELLED"), ExternalWorkerState::Cancelled);
        assert_eq!(agent_state("ACTIVE"), ExternalWorkerState::Ready);
        assert_eq!(agent_state("ARCHIVED"), ExternalWorkerState::Archived);
        assert_eq!(
            run_state("future-provider-state"),
            ExternalWorkerState::Unknown
        );
        assert_eq!(
            agent_state("future-provider-state"),
            ExternalWorkerState::Unknown
        );
    }

    #[test]
    fn terminal_provider_text_is_omitted_when_it_contains_privileged_needles() {
        assert_eq!(
            safe_terminal_result("completed: 2 files"),
            Some("completed: 2 files".into())
        );
        assert_eq!(
            safe_terminal_result("completed\nwith two lines"),
            Some("completed with two lines".into())
        );
        assert_eq!(safe_terminal_result("wrote /Users/alice/project"), None);
        assert_eq!(safe_terminal_result("Authorization: Bearer token"), None);
        assert_eq!(safe_terminal_result("password=secret"), None);
        assert_eq!(
            safe_terminal_result("wrote C:\\Users\\alice\\project"),
            None
        );
    }

    #[tokio::test]
    async fn fake_cursor_api_covers_launch_poll_artifacts_and_terminal_cancel() {
        let state = FakeCursorState::default();
        let address = spawn_app(lifecycle_router(state.clone())).await;
        let adapter = CursorCloudAdapter::for_test(&address);
        let launch = adapter.launch(&launch_request()).await.unwrap();
        assert_eq!(launch.worker.external_agent_id, AGENT_1);
        assert_eq!(launch.run.state, ExternalWorkerState::Provisioning);
        let sent = state.launch_requests.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["repos"][0]["startingRef"], "main");
        assert_eq!(sent[0]["autoCreatePR"], false);
        assert!(sent[0].get("env").is_none());
        drop(sent);

        let worker = adapter
            .get_worker(&launch.worker.external_agent_id)
            .await
            .unwrap();
        assert_eq!(worker.repository, "chriscase/GrokPtah");
        let run = adapter
            .get_run(
                &launch.worker.external_agent_id,
                &launch.run.external_run_id,
            )
            .await
            .unwrap();
        assert_eq!(run.state, ExternalWorkerState::Running);
        assert_eq!(
            run.terminal_result.as_deref(),
            Some("completed with two lines")
        );
        let follow_up = adapter
            .follow_up(
                &launch.worker.external_agent_id,
                &ExternalWorkerFollowUpRequest {
                    request_id: "follow-up-1".into(),
                    prompt: "Now re-check the focused change".into(),
                    bounds: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            follow_up.external_run_id,
            "run-00000000-0000-0000-0000-000000000002"
        );
        assert_eq!(follow_up.state, ExternalWorkerState::Provisioning);
        let artifacts = adapter
            .list_artifacts(
                &launch.worker.external_agent_id,
                &launch.run.external_run_id,
            )
            .await
            .unwrap();
        assert_eq!(artifacts[0].path, "artifacts/report.md");
        let cancelled = adapter
            .cancel(
                &launch.worker.external_agent_id,
                &launch.run.external_run_id,
            )
            .await
            .unwrap();
        assert_eq!(cancelled.state, ExternalWorkerState::Cancelled);
        assert!(state.archive_posts.lock().unwrap().is_empty());
        assert!(state.archived.lock().unwrap().is_empty());
        let worker = adapter.get_worker(AGENT_1).await.unwrap();
        assert_eq!(worker.state, ExternalWorkerState::Ready);
        assert_ne!(worker.state, ExternalWorkerState::Archived);
        let serialized = serde_json::to_string(&worker).unwrap();
        assert!(!serialized.contains("synthetic-cursor-key"));
        assert!(state
            .request_urls
            .lock()
            .unwrap()
            .iter()
            .all(|url| !url.contains("synthetic-cursor-key") && !url.contains('@')));
        assert!(state
            .authorization_headers
            .lock()
            .unwrap()
            .iter()
            .any(|header| header.as_deref().unwrap_or_default().starts_with("Basic ")));
    }

    #[tokio::test]
    async fn fake_cursor_api_lists_with_pagination_and_include_archived() {
        let state = FakeCursorState::default();
        state.archived.lock().unwrap().insert(AGENT_2.to_string());
        let address = spawn_app(lifecycle_router(state.clone())).await;
        let adapter = CursorCloudAdapter::for_test(&address);

        let active = adapter
            .list_workers(&ExternalWorkerListQuery {
                limit: Some(20),
                ..ExternalWorkerListQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(active.items.len(), 1);
        assert_eq!(active.items[0].external_agent_id, AGENT_1);
        assert_eq!(active.items[0].state, ExternalWorkerState::Ready);
        assert_eq!(active.items[0].latest_run_id.as_deref(), Some(RUN_1));
        assert!(active.next_cursor.is_none());
        let active_json = serde_json::to_value(&active).unwrap();
        assert!(active_json["items"][0].get("repository").is_none());
        assert!(active_json["items"][0].get("startingRef").is_none());
        assert!(!active_json.to_string().contains("synthetic-cursor-key"));

        let first = adapter
            .list_workers(&ExternalWorkerListQuery {
                limit: Some(1),
                include_archived: true,
                ..ExternalWorkerListQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(first.items.len(), 1);
        assert_eq!(first.items[0].external_agent_id, AGENT_2);
        assert_eq!(first.items[0].state, ExternalWorkerState::Archived);
        assert_eq!(first.next_cursor.as_deref(), Some(AGENT_1));

        let second = adapter
            .list_workers(&ExternalWorkerListQuery {
                limit: Some(1),
                cursor: first.next_cursor.clone(),
                include_archived: true,
            })
            .await
            .unwrap();
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.items[0].external_agent_id, AGENT_1);
        assert!(second.next_cursor.is_none());

        let queries = state.list_queries.lock().unwrap();
        assert_eq!(queries[0]["includeArchived"], false);
        assert_eq!(queries[0]["limit"], 20);
        assert_eq!(queries[1]["includeArchived"], true);
        assert_eq!(queries[1]["limit"], 1);
        assert_eq!(queries[2]["cursor"], AGENT_1);
        drop(queries);
        assert!(state
            .request_urls
            .lock()
            .unwrap()
            .iter()
            .all(|url| !url.contains("synthetic-cursor-key")));
    }

    #[tokio::test]
    async fn fake_cursor_api_archive_and_unarchive_are_explicit_and_reversible() {
        let state = FakeCursorState::default();
        let address = spawn_app(lifecycle_router(state.clone())).await;
        let adapter = CursorCloudAdapter::for_test(&address);

        let archived = adapter.archive(AGENT_1).await.unwrap();
        assert_eq!(archived.external_agent_id, AGENT_1);
        assert_eq!(archived.state, ExternalWorkerState::Archived);
        assert_eq!(archived.repository, "chriscase/GrokPtah");
        assert_eq!(archived.starting_ref, "main");
        assert_eq!(state.archive_posts.lock().unwrap().as_slice(), [AGENT_1]);

        let archived_again = adapter.archive(AGENT_1).await.unwrap();
        assert_eq!(archived_again.state, ExternalWorkerState::Archived);
        assert_eq!(state.archive_posts.lock().unwrap().len(), 1);

        let restored = adapter.unarchive(AGENT_1).await.unwrap();
        assert_eq!(restored.external_agent_id, AGENT_1);
        assert_eq!(restored.state, ExternalWorkerState::Ready);
        assert_ne!(restored.state, ExternalWorkerState::Archived);
        assert_eq!(state.unarchive_posts.lock().unwrap().as_slice(), [AGENT_1]);

        let restored_again = adapter.unarchive(AGENT_1).await.unwrap();
        assert_eq!(restored_again.state, ExternalWorkerState::Ready);
        assert_eq!(state.unarchive_posts.lock().unwrap().len(), 1);

        let missing = adapter
            .archive("bc-00000000-0000-0000-0000-000000000099")
            .await;
        assert!(matches!(
            missing,
            Err(ExternalWorkerAdapterError::Provider { status }) if status.as_u16() == 404
        ));
    }

    #[tokio::test]
    async fn list_query_is_rejected_before_provider_io() {
        let adapter = CursorCloudAdapter::for_test("http://127.0.0.1:1");
        let error = adapter
            .list_workers(&ExternalWorkerListQuery {
                limit: Some(0),
                ..ExternalWorkerListQuery::default()
            })
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ExternalWorkerAdapterError::InvalidRequest(_)
        ));
        let unsafe_cursor = adapter
            .list_workers(&ExternalWorkerListQuery {
                cursor: Some("page/../secret".into()),
                ..ExternalWorkerListQuery::default()
            })
            .await
            .unwrap_err();
        assert!(matches!(
            unsafe_cursor,
            ExternalWorkerAdapterError::InvalidRequest(_)
        ));
    }

    #[tokio::test]
    async fn fake_cursor_api_fails_closed_on_http_and_malformed_list_responses() {
        let unauthorized = spawn_app(Router::new().route(
            "/v1/agents",
            get(|| async {
                (
                    StatusCode::UNAUTHORIZED,
                    [("content-type", "application/json")],
                    r#"{"error":"synthetic-cursor-key"}"#,
                )
            }),
        ))
        .await;
        let adapter = CursorCloudAdapter::for_test(&unauthorized);
        let error = adapter
            .list_workers(&ExternalWorkerListQuery::default())
            .await
            .unwrap_err();
        let rendered = error.to_string();
        assert!(matches!(
            error,
            ExternalWorkerAdapterError::Provider { status } if status.as_u16() == 401
        ));
        assert!(!rendered.contains("synthetic-cursor-key"));
        assert!(!rendered.contains("Bearer"));

        let malformed = spawn_app(Router::new().route(
            "/v1/agents",
            get(|| async { Json(json!({ "items": [{ "id": 1, "status": "ACTIVE" }] })) }),
        ))
        .await;
        let adapter = CursorCloudAdapter::for_test(&malformed);
        assert!(matches!(
            adapter
                .list_workers(&ExternalWorkerListQuery::default())
                .await
                .unwrap_err(),
            ExternalWorkerAdapterError::InvalidResponse(_)
        ));

        let leaked_url = spawn_app(Router::new().route(
            "/v1/agents",
            get(|| async {
                Json(json!({
                    "items": [{
                        "id": AGENT_1,
                        "status": "ACTIVE",
                        "url": "https://cursor.com/agents/agent-1?token=secret",
                        "createdAt": "2026-08-24T00:00:00Z",
                        "updatedAt": "2026-08-24T00:00:01Z"
                    }]
                }))
            }),
        ))
        .await;
        let adapter = CursorCloudAdapter::for_test(&leaked_url);
        assert!(matches!(
            adapter
                .list_workers(&ExternalWorkerListQuery::default())
                .await
                .unwrap_err(),
            ExternalWorkerAdapterError::InvalidResponse(_)
        ));

        let pool_env = spawn_app(Router::new().route(
            "/v1/agents",
            get(|| async {
                Json(json!({
                    "items": [{
                        "id": AGENT_1,
                        "status": "ACTIVE",
                        "env": {"type": "pool"},
                        "url": format!("https://cursor.com/agents/{AGENT_1}"),
                        "createdAt": "2026-08-24T00:00:00Z",
                        "updatedAt": "2026-08-24T00:00:01Z"
                    }]
                }))
            }),
        ))
        .await;
        let adapter = CursorCloudAdapter::for_test(&pool_env);
        assert!(matches!(
            adapter
                .list_workers(&ExternalWorkerListQuery::default())
                .await
                .unwrap_err(),
            ExternalWorkerAdapterError::InvalidResponse(_)
        ));

        let unexpected_archived = spawn_app(Router::new().route(
            "/v1/agents",
            get(|| async {
                Json(json!({
                    "items": [fake_list_item(AGENT_1, true, "2026-08-24T00:00:01Z")]
                }))
            }),
        ))
        .await;
        let adapter = CursorCloudAdapter::for_test(&unexpected_archived);
        assert!(matches!(
            adapter
                .list_workers(&ExternalWorkerListQuery::default())
                .await
                .unwrap_err(),
            ExternalWorkerAdapterError::InvalidResponse(_)
        ));

        let wrong_archive_id = spawn_app(
            Router::new()
                .route(
                    "/v1/agents/{id}",
                    get(|| async { Json(fake_agent_record(AGENT_1, false)) }),
                )
                .route(
                    "/v1/agents/{id}/archive",
                    post(|| async { Json(json!({ "id": AGENT_2 })) }),
                ),
        )
        .await;
        let adapter = CursorCloudAdapter::for_test(&wrong_archive_id);
        assert!(matches!(
            adapter.archive(AGENT_1).await.unwrap_err(),
            ExternalWorkerAdapterError::InvalidResponse(_)
        ));
    }
}
