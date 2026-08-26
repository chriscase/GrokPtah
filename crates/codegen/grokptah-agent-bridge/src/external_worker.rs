//! Trusted adapters for provider-managed coding workers.
//!
//! The browser and public SDK see only the projections from
//! `grokptah-agent-sdk`. Provider credentials stay in this native/server
//! boundary. The first adapter targets Cursor's Cloud Agents API v1; it does
//! not control a foreground Cursor desktop window and it never grants native
//! Computer Use authority.

use async_trait::async_trait;
use grokptah_agent_sdk::{
    ExternalWorkerArtifact, ExternalWorkerCapabilityStatus, ExternalWorkerExecutionMode,
    ExternalWorkerFollowUpRequest, ExternalWorkerLaunchRequest, ExternalWorkerLaunchResult,
    ExternalWorkerProvider, ExternalWorkerRecord, ExternalWorkerRunRecord, ExternalWorkerState,
    EXTERNAL_WORKER_CONTRACT_VERSION,
};
use parking_lot::RwLock;
use reqwest::{Client, Method, StatusCode, Url};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
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
    /// A qualified adapter is already installed for this provider identity.
    ///
    /// Registration never replaces an installed adapter. Silently swapping one
    /// would let a later registration inherit the authority, capability
    /// revision, and durable receipts minted against its predecessor.
    #[error("external worker provider is already registered")]
    ProviderAlreadyRegistered,
    /// The host-minted admission did not authorize this mutation.
    #[error("external worker admission was rejected: {0}")]
    AdmissionRejected(&'static str),
    /// The mutation conflicts with a durable receipt or tombstone.
    #[error("external worker mutation conflicts with durable state: {0}")]
    Conflict(&'static str),
    /// The outcome of a sent mutation is unknown and must be reconciled.
    #[error("external worker mutation outcome is uncertain: {0}")]
    Uncertain(&'static str),
    /// The capability is not advertised, so the mutation is not available.
    #[error("external worker capability is unavailable: {0}")]
    Unavailable(&'static str),
    /// A durable authority record could not be read or written.
    #[error("external worker durable state failed: {0}")]
    Durable(String),
}

/// Provider-neutral lifecycle operations required by the manager.
#[async_trait]
pub trait ExternalWorkerAdapter: Send + Sync {
    /// Provider family implemented by this adapter.
    fn provider(&self) -> ExternalWorkerProvider;

    /// Adapter identity for custom providers; `None` for standardized ones.
    ///
    /// Two custom adapters are distinct providers, so the registry keys on
    /// this alongside the family and refuses a collision on either.
    fn provider_id(&self) -> Option<&str> {
        None
    }

    /// External-worker contract version this adapter projects into.
    ///
    /// The default is deliberately the current contract: an adapter compiled
    /// against this crate speaks this contract. An adapter that knowingly
    /// lags overrides it and is then advertised as version-incompatible.
    fn contract_version(&self) -> &str {
        EXTERNAL_WORKER_CONTRACT_VERSION
    }

    /// Bounded reachability probe used to decide capability truth.
    ///
    /// The default fails closed. An adapter that cannot prove it is reachable
    /// is never advertised, so a mis-implemented adapter cannot turn a
    /// credential into an advertised production capability.
    async fn probe(&self) -> Result<ExternalWorkerProbe, ExternalWorkerAdapterError> {
        Err(ExternalWorkerAdapterError::Unavailable(
            "adapter does not implement a reachability probe",
        ))
    }

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
}

/// Result of a bounded adapter reachability probe.
///
/// A probe answers "can this host reach the provider right now", never "is
/// the provider healthy". It carries no provider payload: the detail is a
/// bounded redacted label suitable for a capability projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalWorkerProbe {
    /// Whether the adapter reached its provider within its bounded timeout.
    pub reachable: bool,
    /// Contract version the adapter answered with.
    pub contract_version: String,
    /// Bounded redacted detail for an operator projection.
    pub detail: String,
}

impl ExternalWorkerProbe {
    /// Build a reachable probe result for the current contract version.
    pub fn reachable(detail: impl Into<String>) -> Self {
        Self {
            reachable: true,
            contract_version: EXTERNAL_WORKER_CONTRACT_VERSION.to_string(),
            detail: detail.into(),
        }
    }

    /// Build an unreachable probe result for the current contract version.
    pub fn unreachable(detail: impl Into<String>) -> Self {
        Self {
            reachable: false,
            contract_version: EXTERNAL_WORKER_CONTRACT_VERSION.to_string(),
            detail: detail.into(),
        }
    }
}

/// Stable registry key: a provider family plus an adapter identity.
pub type ExternalWorkerProviderKey = (ExternalWorkerProvider, Option<String>);

/// Process-local registry for qualified external-worker providers.
///
/// Registration is deliberately explicit and single-shot. A provider is not
/// available merely because a credential exists: the host must install a
/// qualified adapter, and installing a second adapter for the same identity
/// fails closed rather than replacing the first. The registry revision is
/// bumped on every successful registration so an admission minted against one
/// adapter set cannot be spent after that set changes.
#[derive(Default)]
pub struct ExternalWorkerRegistry {
    inner: RwLock<ExternalWorkerRegistryInner>,
}

#[derive(Default)]
struct ExternalWorkerRegistryInner {
    adapters: BTreeMap<ExternalWorkerProviderKey, Arc<dyn ExternalWorkerAdapter>>,
    revision: u64,
}

impl ExternalWorkerRegistry {
    /// Create an empty provider registry at revision zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install one provider adapter during host setup.
    ///
    /// Returns the new capability revision. A duplicate registration for the
    /// same `(provider, provider_id)` is an error, never a silent replace.
    pub fn register(
        &self,
        adapter: Arc<dyn ExternalWorkerAdapter>,
    ) -> Result<u64, ExternalWorkerAdapterError> {
        let provider = adapter.provider();
        let provider_id = adapter.provider_id().map(str::to_owned);
        if provider == ExternalWorkerProvider::Custom && provider_id.is_none() {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "custom workers require provider_id",
            ));
        }
        if let Some(provider_id) = &provider_id {
            checked_opaque_id(provider_id)?;
        }
        let mut inner = self.inner.write();
        let key = (provider, provider_id);
        if inner.adapters.contains_key(&key) {
            return Err(ExternalWorkerAdapterError::ProviderAlreadyRegistered);
        }
        inner.adapters.insert(key, adapter);
        inner.revision = inner.revision.saturating_add(1);
        Ok(inner.revision)
    }

    /// Return the adapter for an exact provider identity, if installed.
    pub fn get(
        &self,
        provider: ExternalWorkerProvider,
        provider_id: Option<&str>,
    ) -> Option<Arc<dyn ExternalWorkerAdapter>> {
        self.inner
            .read()
            .adapters
            .get(&(provider, provider_id.map(str::to_owned)))
            .cloned()
    }

    /// Current capability revision; bumped by each successful registration.
    pub fn revision(&self) -> u64 {
        self.inner.read().revision
    }

    /// Return the exact provider identities explicitly installed here.
    pub fn provider_keys(&self) -> Vec<ExternalWorkerProviderKey> {
        self.inner.read().adapters.keys().cloned().collect()
    }

    /// Return the provider families explicitly installed in this process.
    pub fn providers(&self) -> Vec<ExternalWorkerProvider> {
        let mut providers = self
            .inner
            .read()
            .adapters
            .keys()
            .map(|(provider, _)| *provider)
            .collect::<Vec<_>>();
        providers.sort_by_key(|provider| *provider as u8);
        providers.dedup();
        providers
    }

    /// Compute capability truth for one installed provider identity.
    ///
    /// Every gate is observed, not assumed: registration comes from this
    /// registry, reachability and contract version come from the adapter's own
    /// bounded probe, and policy comes from the caller's allowlist.
    pub async fn capability_status(
        &self,
        provider: ExternalWorkerProvider,
        provider_id: Option<&str>,
        policy_allowed: bool,
    ) -> ExternalWorkerCapabilityStatus {
        let revision = self.revision();
        let mut status = ExternalWorkerCapabilityStatus {
            provider,
            provider_id: provider_id.map(str::to_owned),
            registered: false,
            reachable: false,
            version_compatible: false,
            policy_allowed,
            capability_revision: revision,
            reason: None,
        };
        let Some(adapter) = self.get(provider, provider_id) else {
            status.reason = Some("no qualified adapter is registered".into());
            return status;
        };
        status.registered = true;
        status.version_compatible = adapter.contract_version() == EXTERNAL_WORKER_CONTRACT_VERSION;
        match adapter.probe().await {
            Ok(probe) => {
                status.reachable = probe.reachable;
                status.version_compatible = status.version_compatible
                    && probe.contract_version == EXTERNAL_WORKER_CONTRACT_VERSION;
            }
            Err(_) => status.reachable = false,
        }
        if !status.is_available() {
            status.reason = Some(
                match (
                    status.registered,
                    status.reachable,
                    status.version_compatible,
                    status.policy_allowed,
                ) {
                    (_, false, _, _) => "adapter did not answer a reachability probe",
                    (_, _, false, _) => "adapter contract version is not compatible",
                    _ => "host policy does not allow this provider",
                }
                .into(),
            );
        }
        status
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
        let url = self
            .base_url
            .join(path)
            .map_err(|_| ExternalWorkerAdapterError::InvalidResponse("provider URL is invalid"))?;
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
        Ok(response.json().await?)
    }

    fn checked_id(value: &str) -> Result<&str, ExternalWorkerAdapterError> {
        checked_opaque_id(value)
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
}

#[async_trait]
impl ExternalWorkerAdapter for CursorCloudAdapter {
    fn provider(&self) -> ExternalWorkerProvider {
        ExternalWorkerProvider::CursorCloud
    }

    async fn probe(&self) -> Result<ExternalWorkerProbe, ExternalWorkerAdapterError> {
        // Reachability is only half the gate: an adapter without an explicit
        // repository allowlist cannot legally launch, so it must not be
        // advertised as an available production capability either.
        if self.allowed_repositories.is_none() {
            return Ok(ExternalWorkerProbe::unreachable(
                "Cursor repository allowlist is not configured",
            ));
        }
        match self
            .request::<CursorIdResponse>(Method::GET, "/v1/me", None)
            .await
        {
            Ok(_) => Ok(ExternalWorkerProbe::reachable("Cursor Cloud v1 answered")),
            // A provider status is a bounded, share-safe fact; the body is
            // deliberately never read, so nothing provider-authored is kept.
            Err(ExternalWorkerAdapterError::Provider { status }) => Ok(
                ExternalWorkerProbe::unreachable(format!("Cursor Cloud v1 returned HTTP {status}")),
            ),
            Err(_) => Ok(ExternalWorkerProbe::unreachable(
                "Cursor Cloud v1 was not reachable",
            )),
        }
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
    // Cursor's v1 payloads spell this `autoCreatePR`, which is not what
    // `rename_all = "camelCase"` derives. Without the alias the adapter can
    // never observe the flag, so every provider response fails closed on the
    // PR-safety proof below and no launch can ever be accepted.
    #[serde(default, alias = "autoCreatePR")]
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
    #[allow(dead_code)]
    id: String,
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

/// Accept only opaque provider identities that are safe in a URL path.
///
/// The same check guards adapter identities in the registry: a provider id is
/// a durable ledger key and a filename component, so it must never carry a
/// separator, a control character, or an unbounded blob.
fn checked_opaque_id(value: &str) -> Result<&str, ExternalWorkerAdapterError> {
    if value.is_empty()
        || value.len() > 256
        || value
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    {
        return Err(ExternalWorkerAdapterError::InvalidRequest(
            "provider identity is not a safe opaque ID",
        ));
    }
    Ok(value)
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

/// Scripted in-tree adapters used by the external-worker authority tests.
///
/// These exist so admission, receipt, and capability behaviour can be proven
/// deterministically without a provider account, a credential, or a network.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// What one scripted adapter call should do.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum FakeOutcome {
        /// The provider accepted and returned a verified projection.
        Accept,
        /// The provider refused before any provider state changed.
        RejectBeforeSend,
        /// The request left this host and no verified answer came back.
        AmbiguousAfterSend,
    }

    /// A programmable adapter with no network, credential, or provider state.
    pub(crate) struct FakeAdapter {
        pub(crate) provider: ExternalWorkerProvider,
        pub(crate) provider_id: Option<String>,
        pub(crate) contract_version: String,
        pub(crate) reachable: bool,
        script: Mutex<Vec<FakeOutcome>>,
        pub(crate) launches: AtomicUsize,
        pub(crate) follow_ups: AtomicUsize,
        pub(crate) cancels: AtomicUsize,
        pub(crate) probes: AtomicUsize,
    }

    impl FakeAdapter {
        /// A custom-provider adapter that always accepts.
        pub(crate) fn custom(provider_id: &str, reachable: bool) -> Self {
            Self {
                provider: ExternalWorkerProvider::Custom,
                provider_id: Some(provider_id.to_owned()),
                contract_version: EXTERNAL_WORKER_CONTRACT_VERSION.to_owned(),
                reachable,
                script: Mutex::new(Vec::new()),
                launches: AtomicUsize::new(0),
                follow_ups: AtomicUsize::new(0),
                cancels: AtomicUsize::new(0),
                probes: AtomicUsize::new(0),
            }
        }

        /// Queue outcomes consumed in order by launch/follow-up/cancel.
        pub(crate) fn script(self, outcomes: impl IntoIterator<Item = FakeOutcome>) -> Self {
            let mut queued = outcomes.into_iter().collect::<Vec<_>>();
            queued.reverse();
            *self.script.lock() = queued;
            self
        }

        /// Total provider-facing sends this adapter observed.
        pub(crate) fn sends(&self) -> usize {
            self.launches.load(Ordering::SeqCst)
                + self.follow_ups.load(Ordering::SeqCst)
                + self.cancels.load(Ordering::SeqCst)
        }

        fn next(&self) -> FakeOutcome {
            self.script.lock().pop().unwrap_or(FakeOutcome::Accept)
        }

        fn apply(&self, outcome: FakeOutcome) -> Result<(), ExternalWorkerAdapterError> {
            match outcome {
                FakeOutcome::Accept => Ok(()),
                FakeOutcome::RejectBeforeSend => Err(ExternalWorkerAdapterError::InvalidRequest(
                    "synthetic provider refusal before send",
                )),
                FakeOutcome::AmbiguousAfterSend => Err(
                    ExternalWorkerAdapterError::InvalidResponse("synthetic ambiguity after send"),
                ),
            }
        }

        fn worker(&self, request: &ExternalWorkerLaunchRequest) -> ExternalWorkerRecord {
            ExternalWorkerRecord {
                provider: self.provider,
                provider_id: self.provider_id.clone(),
                external_agent_id: "fake-agent".into(),
                repository: request.repository.clone(),
                starting_ref: request.starting_ref.clone(),
                state: ExternalWorkerState::Running,
                branch: None,
                worker_url: None,
                created_at: "2026-08-24T00:00:00Z".into(),
                updated_at: "2026-08-24T00:00:00Z".into(),
            }
        }

        fn run(&self, state: ExternalWorkerState, run_id: &str) -> ExternalWorkerRunRecord {
            ExternalWorkerRunRecord {
                external_agent_id: "fake-agent".into(),
                external_run_id: run_id.into(),
                state,
                last_seq: 0,
                terminal_result: None,
                created_at: "2026-08-24T00:00:00Z".into(),
                updated_at: "2026-08-24T00:00:00Z".into(),
            }
        }
    }

    #[async_trait]
    impl ExternalWorkerAdapter for FakeAdapter {
        fn provider(&self) -> ExternalWorkerProvider {
            self.provider
        }

        fn provider_id(&self) -> Option<&str> {
            self.provider_id.as_deref()
        }

        fn contract_version(&self) -> &str {
            &self.contract_version
        }

        async fn probe(&self) -> Result<ExternalWorkerProbe, ExternalWorkerAdapterError> {
            self.probes.fetch_add(1, Ordering::SeqCst);
            let mut probe = if self.reachable {
                ExternalWorkerProbe::reachable("synthetic adapter answered")
            } else {
                ExternalWorkerProbe::unreachable("synthetic adapter is offline")
            };
            probe.contract_version = self.contract_version.clone();
            Ok(probe)
        }

        async fn launch(
            &self,
            request: &ExternalWorkerLaunchRequest,
        ) -> Result<ExternalWorkerLaunchResult, ExternalWorkerAdapterError> {
            let outcome = self.next();
            self.launches.fetch_add(1, Ordering::SeqCst);
            self.apply(outcome)?;
            Ok(ExternalWorkerLaunchResult {
                worker: self.worker(request),
                run: self.run(ExternalWorkerState::Running, "fake-run-1"),
            })
        }

        async fn get_worker(
            &self,
            external_agent_id: &str,
        ) -> Result<ExternalWorkerRecord, ExternalWorkerAdapterError> {
            Ok(ExternalWorkerRecord {
                external_agent_id: external_agent_id.into(),
                ..self.worker(&fake_launch_request())
            })
        }

        async fn get_run(
            &self,
            _external_agent_id: &str,
            external_run_id: &str,
        ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
            Ok(self.run(ExternalWorkerState::Running, external_run_id))
        }

        async fn follow_up(
            &self,
            _external_agent_id: &str,
            _request: &ExternalWorkerFollowUpRequest,
        ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
            let outcome = self.next();
            self.follow_ups.fetch_add(1, Ordering::SeqCst);
            self.apply(outcome)?;
            Ok(self.run(ExternalWorkerState::Running, "fake-run-2"))
        }

        async fn list_artifacts(
            &self,
            _external_agent_id: &str,
            _external_run_id: &str,
        ) -> Result<Vec<ExternalWorkerArtifact>, ExternalWorkerAdapterError> {
            Ok(Vec::new())
        }

        async fn cancel(
            &self,
            _external_agent_id: &str,
            external_run_id: &str,
        ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
            let outcome = self.next();
            self.cancels.fetch_add(1, Ordering::SeqCst);
            self.apply(outcome)?;
            Ok(self.run(ExternalWorkerState::Cancelled, external_run_id))
        }
    }

    /// An adapter that never implements a probe, exercising the closed default.
    pub(crate) struct ProbelessAdapter;

    #[async_trait]
    impl ExternalWorkerAdapter for ProbelessAdapter {
        fn provider(&self) -> ExternalWorkerProvider {
            ExternalWorkerProvider::LocalWorker
        }

        async fn launch(
            &self,
            _request: &ExternalWorkerLaunchRequest,
        ) -> Result<ExternalWorkerLaunchResult, ExternalWorkerAdapterError> {
            Err(ExternalWorkerAdapterError::UnsupportedProvider)
        }

        async fn get_worker(
            &self,
            _external_agent_id: &str,
        ) -> Result<ExternalWorkerRecord, ExternalWorkerAdapterError> {
            Err(ExternalWorkerAdapterError::UnsupportedProvider)
        }

        async fn get_run(
            &self,
            _external_agent_id: &str,
            _external_run_id: &str,
        ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
            Err(ExternalWorkerAdapterError::UnsupportedProvider)
        }

        async fn follow_up(
            &self,
            _external_agent_id: &str,
            _request: &ExternalWorkerFollowUpRequest,
        ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
            Err(ExternalWorkerAdapterError::UnsupportedProvider)
        }

        async fn list_artifacts(
            &self,
            _external_agent_id: &str,
            _external_run_id: &str,
        ) -> Result<Vec<ExternalWorkerArtifact>, ExternalWorkerAdapterError> {
            Err(ExternalWorkerAdapterError::UnsupportedProvider)
        }

        async fn cancel(
            &self,
            _external_agent_id: &str,
            _external_run_id: &str,
        ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
            Err(ExternalWorkerAdapterError::UnsupportedProvider)
        }
    }

    /// A bounded synthetic launch request with no host path or credential.
    pub(crate) fn fake_launch_request() -> ExternalWorkerLaunchRequest {
        ExternalWorkerLaunchRequest {
            request_id: "req-1".into(),
            provider: ExternalWorkerProvider::Custom,
            provider_id: Some("gateway-a".into()),
            repository: "chriscase/GrokPtah".into(),
            starting_ref: "refs/heads/codex/review".into(),
            prompt: "Review the exact candidate".into(),
            model: None,
            execution_mode: ExternalWorkerExecutionMode::Isolated,
            auto_create_pr: false,
            bounds: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{FakeAdapter, ProbelessAdapter};
    use super::*;
    use axum::extract::State;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct FakeCursorState {
        launch_requests: Arc<Mutex<Vec<Value>>>,
        cancelled: Arc<Mutex<bool>>,
    }

    fn fake_agent() -> Value {
        json!({
            "id": "bc-00000000-0000-0000-0000-000000000001",
            "url": "https://cursor.com/agents/bc-00000000-0000-0000-0000-000000000001",
            "status": "ACTIVE",
            "repos": [{"url": "https://github.com/chriscase/GrokPtah", "startingRef": "main"}],
            "autoCreatePR": false,
            "workOnCurrentBranch": false,
            "env": {"type": "cloud"},
            "createdAt": "2026-08-24T00:00:00Z",
            "updatedAt": "2026-08-24T00:00:01Z"
        })
    }

    async fn fake_create(
        State(state): State<FakeCursorState>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        state.launch_requests.lock().unwrap().push(body);
        Json(json!({
            "agent": fake_agent(),
            "run": {
                "id": "run-00000000-0000-0000-0000-000000000001",
                "agentId": "bc-00000000-0000-0000-0000-000000000001",
                "status": "CREATING",
                "createdAt": "2026-08-24T00:00:00Z",
                "updatedAt": "2026-08-24T00:00:01Z"
            }
        }))
    }

    async fn fake_agent_read(State(_state): State<FakeCursorState>) -> Json<Value> {
        Json(fake_agent())
    }

    async fn fake_run_read(State(state): State<FakeCursorState>) -> Json<Value> {
        let cancelled = *state.cancelled.lock().unwrap();
        Json(json!({
            "id": "run-00000000-0000-0000-0000-000000000001",
            "agentId": "bc-00000000-0000-0000-0000-000000000001",
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
                "agentId": "bc-00000000-0000-0000-0000-000000000001",
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
        Json(json!({"id": "run-00000000-0000-0000-0000-000000000001"}))
    }

    async fn fake_artifacts(State(_state): State<FakeCursorState>) -> Json<Value> {
        Json(json!({
            "items": [{
                "path": "artifacts/report.md",
                "runId": "run-00000000-0000-0000-0000-000000000001",
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
    fn pr_safety_proof_reads_both_cursor_spellings_and_still_fails_closed() {
        let mut value = fake_agent();
        // Cursor's documented spelling.
        let agent: CursorAgent = serde_json::from_value(value.clone()).expect("agent parses");
        assert_eq!(agent.auto_create_pr, Some(false));
        assert!(worker_record(&agent, Some("chriscase/GrokPtah"), Some("main")).is_ok());

        // The strict camelCase spelling is accepted too.
        let object = value.as_object_mut().expect("agent is an object");
        object.remove("autoCreatePR");
        object.insert("autoCreatePr".into(), json!(false));
        let agent: CursorAgent = serde_json::from_value(value.clone()).expect("agent parses");
        assert_eq!(agent.auto_create_pr, Some(false));

        // An absent or true flag is still not a proof, so it fails closed.
        let object = value.as_object_mut().expect("agent is an object");
        object.remove("autoCreatePr");
        let agent: CursorAgent = serde_json::from_value(value.clone()).expect("agent parses");
        assert!(matches!(
            worker_record(&agent, None, None),
            Err(ExternalWorkerAdapterError::InvalidResponse(_))
        ));
        let object = value.as_object_mut().expect("agent is an object");
        object.insert("autoCreatePR".into(), json!(true));
        let agent: CursorAgent = serde_json::from_value(value).expect("agent parses");
        assert!(matches!(
            worker_record(&agent, None, None),
            Err(ExternalWorkerAdapterError::InvalidResponse(_))
        ));
    }

    #[test]
    fn registry_exposes_only_explicitly_installed_providers() {
        let registry = ExternalWorkerRegistry::new();
        assert!(registry.providers().is_empty());
        assert_eq!(registry.revision(), 0);
        let adapter = Arc::new(CursorCloudAdapter::new("synthetic-key").unwrap());
        assert_eq!(registry.register(adapter).expect("first install"), 1);
        assert_eq!(
            registry.providers(),
            vec![ExternalWorkerProvider::CursorCloud]
        );
        assert!(registry
            .get(ExternalWorkerProvider::CursorCloud, None)
            .is_some());
        assert!(registry
            .get(ExternalWorkerProvider::ClaudeCodeCloud, None)
            .is_none());
    }

    #[test]
    fn duplicate_provider_registration_fails_closed_without_replacing() {
        let registry = ExternalWorkerRegistry::new();
        let first = Arc::new(CursorCloudAdapter::new("first-key").unwrap());
        let first_ptr = Arc::as_ptr(&first) as *const () as usize;
        assert_eq!(registry.register(first).expect("first install"), 1);

        let second = Arc::new(CursorCloudAdapter::new("second-key").unwrap());
        let error = registry
            .register(second)
            .expect_err("a second adapter must not silently replace the first");
        assert!(matches!(
            error,
            ExternalWorkerAdapterError::ProviderAlreadyRegistered
        ));

        // The installed adapter is unchanged and the revision did not move,
        // so admissions minted against revision 1 stay spendable.
        let installed = registry
            .get(ExternalWorkerProvider::CursorCloud, None)
            .expect("first adapter is still installed");
        assert_eq!(
            Arc::as_ptr(&installed) as *const () as usize,
            first_ptr,
            "the original adapter must remain installed"
        );
        assert_eq!(registry.revision(), 1);
    }

    #[test]
    fn custom_provider_identities_are_distinct_registry_keys() {
        let registry = ExternalWorkerRegistry::new();
        assert_eq!(
            registry
                .register(Arc::new(FakeAdapter::custom("gateway-a", true)))
                .expect("first custom adapter"),
            1
        );
        assert_eq!(
            registry
                .register(Arc::new(FakeAdapter::custom("gateway-b", true)))
                .expect("a different custom identity is a different provider"),
            2
        );
        assert!(matches!(
            registry
                .register(Arc::new(FakeAdapter::custom("gateway-a", true)))
                .expect_err("the same custom identity collides"),
            ExternalWorkerAdapterError::ProviderAlreadyRegistered
        ));
        assert_eq!(registry.revision(), 2);
        assert!(registry
            .get(ExternalWorkerProvider::Custom, Some("gateway-b"))
            .is_some());
        assert!(registry
            .get(ExternalWorkerProvider::Custom, Some("gateway-c"))
            .is_none());
    }

    #[test]
    fn custom_registration_without_an_identity_fails_closed() {
        let registry = ExternalWorkerRegistry::new();
        let mut adapter = FakeAdapter::custom("gateway-a", true);
        adapter.provider_id = None;
        assert!(matches!(
            registry
                .register(Arc::new(adapter))
                .expect_err("no identity"),
            ExternalWorkerAdapterError::InvalidRequest("custom workers require provider_id")
        ));
        assert_eq!(registry.revision(), 0);

        let mut unsafe_id = FakeAdapter::custom("gateway-a", true);
        unsafe_id.provider_id = Some("../escape".into());
        assert!(matches!(
            registry
                .register(Arc::new(unsafe_id))
                .expect_err("unsafe identity"),
            ExternalWorkerAdapterError::InvalidRequest("provider identity is not a safe opaque ID")
        ));
        assert_eq!(registry.revision(), 0);
    }

    #[tokio::test]
    async fn capability_truth_requires_registration_probe_version_and_policy() {
        let registry = ExternalWorkerRegistry::new();

        // Not registered.
        let status = registry
            .capability_status(ExternalWorkerProvider::Custom, Some("gateway-a"), true)
            .await;
        assert!(!status.is_available());
        assert_eq!(
            status.reason.as_deref(),
            Some("no qualified adapter is registered")
        );
        status.validate().expect("unavailable status is valid");

        // Registered and reachable, but policy refuses it.
        registry
            .register(Arc::new(FakeAdapter::custom("gateway-a", true)))
            .expect("install");
        let status = registry
            .capability_status(ExternalWorkerProvider::Custom, Some("gateway-a"), false)
            .await;
        assert!(status.registered && status.reachable && status.version_compatible);
        assert!(!status.is_available());
        assert_eq!(
            status.reason.as_deref(),
            Some("host policy does not allow this provider")
        );

        // Registered and policy-allowed, but the probe says unreachable.
        registry
            .register(Arc::new(FakeAdapter::custom("gateway-down", false)))
            .expect("install");
        let status = registry
            .capability_status(ExternalWorkerProvider::Custom, Some("gateway-down"), true)
            .await;
        assert!(!status.is_available());
        assert_eq!(
            status.reason.as_deref(),
            Some("adapter did not answer a reachability probe")
        );

        // Registered, reachable, policy-allowed, but a lagging contract.
        let mut stale = FakeAdapter::custom("gateway-old", true);
        stale.contract_version = "grokptah.external-workers.v0".into();
        registry.register(Arc::new(stale)).expect("install");
        let status = registry
            .capability_status(ExternalWorkerProvider::Custom, Some("gateway-old"), true)
            .await;
        assert!(!status.is_available());
        assert_eq!(
            status.reason.as_deref(),
            Some("adapter contract version is not compatible")
        );

        // All four gates hold.
        let status = registry
            .capability_status(ExternalWorkerProvider::Custom, Some("gateway-a"), true)
            .await;
        assert!(status.is_available());
        assert!(status.reason.is_none());
        status.validate().expect("available status is valid");
    }

    #[tokio::test]
    async fn an_adapter_without_a_probe_is_never_advertised() {
        let registry = ExternalWorkerRegistry::new();
        registry
            .register(Arc::new(ProbelessAdapter))
            .expect("install");
        let status = registry
            .capability_status(ExternalWorkerProvider::LocalWorker, None, true)
            .await;
        assert!(status.registered);
        assert!(!status.reachable);
        assert!(!status.is_available());
    }

    #[test]
    fn provider_statuses_fail_closed() {
        assert_eq!(run_state("RUNNING"), ExternalWorkerState::Running);
        assert_eq!(run_state("CANCELLED"), ExternalWorkerState::Cancelled);
        assert_eq!(
            run_state("future-provider-state"),
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
        let app = Router::new()
            .route("/v1/agents", post(fake_create))
            .route("/v1/agents/bc-00000000-0000-0000-0000-000000000001", get(fake_agent_read))
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
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let adapter = CursorCloudAdapter::for_test(&format!("http://{address}"));
        let request = ExternalWorkerLaunchRequest {
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
        };
        let launch = adapter.launch(&request).await.unwrap();
        assert_eq!(
            launch.worker.external_agent_id,
            "bc-00000000-0000-0000-0000-000000000001"
        );
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
    }
}
