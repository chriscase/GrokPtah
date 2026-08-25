//! Cursor Cloud Agents API v1 adapter.

use super::{
    checked_id, extract_provider_conflict_code, github_repository_url, refs_equal,
    repository_identity, ExternalWorkerAdapter, ExternalWorkerAdapterError, ProviderConflictCode,
};
use async_trait::async_trait;
use futures::StreamExt;
use grokptah_agent_sdk::{
    ExternalWorkerArtifact, ExternalWorkerExecutionMode, ExternalWorkerFollowUpRequest,
    ExternalWorkerLaunchRequest, ExternalWorkerLaunchResult, ExternalWorkerProvider,
    ExternalWorkerRecord, ExternalWorkerRunRecord, ExternalWorkerState, ExternalWorkerStreamState,
};
use reqwest::{Client, Method, StatusCode, Url};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

/// Public API base for Cursor Cloud Agents v1.
pub const CURSOR_CLOUD_API_BASE: &str = "https://api.cursor.com";

/// Maximum bytes downloaded when hashing a provider artifact.
pub const MAX_EXTERNAL_WORKER_ARTIFACT_BYTES: u64 = 8 * 1024 * 1024;

/// Documented virtual-hosted Cursor artifact prefix.
pub const PRODUCTION_ARTIFACT_HOST_PREFIX: &str = "cloud-agent-artifacts.s3.";

/// Trusted Cursor Cloud Agents API v1 adapter.
pub struct CursorCloudAdapter {
    http: Client,
    base_url: Url,
    api_key: String,
    allowed_repositories: Option<Arc<BTreeSet<String>>>,
    allowed_artifact_hosts: Arc<BTreeSet<String>>,
    max_artifact_bytes: u64,
    allow_http_artifact_hosts: bool,
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
            allowed_artifact_hosts: Arc::new(BTreeSet::new()),
            max_artifact_bytes: MAX_EXTERNAL_WORKER_ARTIFACT_BYTES,
            allow_http_artifact_hosts: false,
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
        let mut allowlist = BTreeSet::new();
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
    pub(crate) fn for_test(base_url: &str) -> Self {
        let parsed = Url::parse(base_url).expect("test server URL is valid");
        let mut hosts = BTreeSet::new();
        if let Some(host) = parsed.host_str() {
            hosts.insert(host.to_string());
        }
        Self {
            http: Client::builder()
                .timeout(Duration::from_secs(10))
                .connect_timeout(Duration::from_secs(2))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("test client is valid"),
            base_url: parsed,
            api_key: "synthetic-cursor-key".into(),
            allowed_repositories: Some(Arc::new(
                ["chriscase/GrokPtah".to_string()].into_iter().collect(),
            )),
            allowed_artifact_hosts: Arc::new(hosts),
            max_artifact_bytes: MAX_EXTERNAL_WORKER_ARTIFACT_BYTES,
            allow_http_artifact_hosts: true,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_max_artifact_bytes(mut self, max_bytes: u64) -> Self {
        self.max_artifact_bytes = max_bytes;
        self
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<T, ExternalWorkerAdapterError> {
        let value = self.request_value(method, path, body).await?;
        serde_json::from_value(value).map_err(|_| {
            ExternalWorkerAdapterError::InvalidResponse("provider response could not be decoded")
        })
    }

    async fn request_value(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, ExternalWorkerAdapterError> {
        let url = self
            .base_url
            .join(path)
            .map_err(|_| ExternalWorkerAdapterError::InvalidResponse("provider URL is invalid"))?;
        self.send(method, url, body, true).await
    }

    async fn send(
        &self,
        method: Method,
        url: Url,
        body: Option<Value>,
        include_basic_auth: bool,
    ) -> Result<Value, ExternalWorkerAdapterError> {
        let mut request = self.http.request(method, url);
        if include_basic_auth {
            // Cursor documents Basic auth with the API key as the username.
            // Never put the key in a URL or a serializable request DTO.
            request = request.basic_auth(&self.api_key, Some(""));
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            // Parse only a closed conflict-code vocabulary. The remainder of
            // the provider body is dropped so credentials cannot enter logs.
            let code = extract_provider_conflict_code(&text);
            return Err(ExternalWorkerAdapterError::Provider { status, code });
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|_| {
            ExternalWorkerAdapterError::InvalidResponse("provider response was not JSON")
        })
    }

    async fn list_provider_runs(
        &self,
        external_agent_id: &str,
    ) -> Result<Vec<CursorRun>, ExternalWorkerAdapterError> {
        let id = checked_id(external_agent_id)?;
        let response: CursorRuns = self
            .request(Method::GET, &format!("/v1/agents/{id}/runs"), None)
            .await?;
        Ok(response.items)
    }

    fn launch_payload(
        request: &ExternalWorkerLaunchRequest,
    ) -> Result<Value, ExternalWorkerAdapterError> {
        let repository_url = github_repository_url(&request.repository)?;
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
        Ok(payload)
    }

    async fn reconcile_launch_conflict(
        &self,
        request: &ExternalWorkerLaunchRequest,
        code: Option<ProviderConflictCode>,
    ) -> Result<ExternalWorkerLaunchResult, ExternalWorkerAdapterError> {
        if code != Some(ProviderConflictCode::AgentConflict) {
            return Err(ExternalWorkerAdapterError::Provider {
                status: StatusCode::CONFLICT,
                code,
            });
        }
        if !is_cursor_agent_id(&request.request_id) {
            return Err(ExternalWorkerAdapterError::Uncertain);
        }
        let worker = self.get_worker(&request.request_id).await?;
        let expected = repository_identity(&github_repository_url(&request.repository)?)?;
        if worker.repository != expected || !refs_equal(&worker.starting_ref, &request.starting_ref)
        {
            return Err(ExternalWorkerAdapterError::InvalidResponse(
                "Cursor conflict agent does not match the exact request",
            ));
        }
        let runs = self.list_provider_runs(&worker.external_agent_id).await?;
        let Some(run) = runs.into_iter().next() else {
            return Err(ExternalWorkerAdapterError::Uncertain);
        };
        let run = run_record(&run, &worker.external_agent_id)?;
        let result = ExternalWorkerLaunchResult { worker, run };
        result
            .validate()
            .map_err(ExternalWorkerAdapterError::InvalidResponse)?;
        Ok(result)
    }

    async fn reconcile_follow_up_conflict(
        &self,
        agent_id: &str,
        code: Option<ProviderConflictCode>,
    ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
        if code != Some(ProviderConflictCode::AgentBusy) {
            return Err(ExternalWorkerAdapterError::Provider {
                status: StatusCode::CONFLICT,
                code,
            });
        }
        let _ = self.get_worker(agent_id).await?;
        let runs = self.list_provider_runs(agent_id).await?;
        if runs.iter().any(|run| {
            matches!(
                run_state(&run.status),
                ExternalWorkerState::Provisioning | ExternalWorkerState::Running
            )
        }) {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "Cursor worker already has an active run",
            ));
        }
        // Busy was claimed, but GET no longer shows an active run. The
        // follow-up may or may not have been created; fail closed.
        Err(ExternalWorkerAdapterError::Uncertain)
    }

    async fn reconcile_cancel(
        &self,
        agent_id: &str,
        run_id: &str,
        code: Option<ProviderConflictCode>,
    ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
        let run = self.get_run(agent_id, run_id).await?;
        if run.state == ExternalWorkerState::Cancelled {
            return Ok(run);
        }
        if matches!(
            run.state,
            ExternalWorkerState::Completed
                | ExternalWorkerState::Failed
                | ExternalWorkerState::Archived
        ) {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "Cursor run is not cancellable",
            ));
        }
        // 409 means GET is authoritative. A still-running run after a refused
        // cancel is Uncertain; do not treat the conflict code as success.
        let _ = code;
        Err(ExternalWorkerAdapterError::Uncertain)
    }

    fn artifact_path_is_safe(path: &str) -> bool {
        path.starts_with("artifacts/")
            && !path.contains('\\')
            && !path.contains('?')
            && !path.contains('#')
            && !path.contains('\0')
            && path
                .split('/')
                .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
    }

    fn presigned_url_allowed(&self, url: &Url) -> Result<(), ExternalWorkerAdapterError> {
        if !url.username().is_empty() || url.password().is_some() {
            return Err(ExternalWorkerAdapterError::InvalidResponse(
                "Cursor artifact URL must not contain credentials",
            ));
        }
        let host = url
            .host_str()
            .ok_or(ExternalWorkerAdapterError::InvalidResponse(
                "Cursor artifact URL is missing a host",
            ))?;
        if self.allow_http_artifact_hosts {
            if url.scheme() != "http" && url.scheme() != "https" {
                return Err(ExternalWorkerAdapterError::InvalidResponse(
                    "Cursor artifact URL scheme is not allowed",
                ));
            }
            if !self.allowed_artifact_hosts.contains(host) {
                return Err(ExternalWorkerAdapterError::InvalidResponse(
                    "Cursor artifact host is not approved",
                ));
            }
            return Ok(());
        }
        if url.scheme() != "https" {
            return Err(ExternalWorkerAdapterError::InvalidResponse(
                "Cursor artifact URL must be HTTPS",
            ));
        }
        if !crate::ssrf::check_url(url.as_str()).allow {
            return Err(ExternalWorkerAdapterError::InvalidResponse(
                "Cursor artifact URL failed the SSRF preflight",
            ));
        }
        if !production_artifact_host_allowed(host) {
            return Err(ExternalWorkerAdapterError::InvalidResponse(
                "Cursor artifact host is not approved",
            ));
        }
        Ok(())
    }

    async fn download_and_hash(
        &self,
        agent_id: &str,
        path: &str,
        reported_size: Option<u64>,
    ) -> Result<(String, u64), ExternalWorkerAdapterError> {
        if reported_size.is_some_and(|size| size > self.max_artifact_bytes) {
            return Err(ExternalWorkerAdapterError::InvalidResponse(
                "Cursor artifact exceeds the download byte ceiling",
            ));
        }
        let mut url = self
            .base_url
            .join(&format!("/v1/agents/{agent_id}/artifacts/download"))
            .map_err(|_| ExternalWorkerAdapterError::InvalidResponse("provider URL is invalid"))?;
        url.query_pairs_mut().append_pair("path", path);
        let envelope: CursorArtifactDownload = self
            .send(Method::GET, url, None, true)
            .await
            .and_then(|value| {
                serde_json::from_value(value).map_err(|_| {
                    ExternalWorkerAdapterError::InvalidResponse(
                        "Cursor artifact download envelope is invalid",
                    )
                })
            })?;
        if envelope.expires_at.trim().is_empty() {
            return Err(ExternalWorkerAdapterError::InvalidResponse(
                "Cursor artifact download is missing an expiry",
            ));
        }
        let download = Url::parse(&envelope.url).map_err(|_| {
            ExternalWorkerAdapterError::InvalidResponse("Cursor artifact URL is invalid")
        })?;
        self.presigned_url_allowed(&download)?;
        let response = self
            .http
            .get(download)
            .send()
            .await
            .map_err(ExternalWorkerAdapterError::Transport)?;
        if !response.status().is_success() {
            return Err(ExternalWorkerAdapterError::InvalidResponse(
                "Cursor artifact download failed",
            ));
        }
        if let Some(content_length) = response.content_length() {
            if content_length > self.max_artifact_bytes {
                return Err(ExternalWorkerAdapterError::InvalidResponse(
                    "Cursor artifact exceeds the download byte ceiling",
                ));
            }
        }
        let mut total = 0u64;
        let mut hasher = Sha256::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(ExternalWorkerAdapterError::Transport)?;
            total = total.saturating_add(chunk.len() as u64);
            if total > self.max_artifact_bytes {
                return Err(ExternalWorkerAdapterError::InvalidResponse(
                    "Cursor artifact exceeds the download byte ceiling",
                ));
            }
            hasher.update(&chunk);
        }
        if total == 0 {
            return Err(ExternalWorkerAdapterError::InvalidResponse(
                "Cursor artifact download was empty",
            ));
        }
        Ok((
            format!("sha256:{}", hex_sha256(hasher.finalize().as_slice())),
            total,
        ))
    }
}

fn production_artifact_host_allowed(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    if host == "cloud-agent-artifacts.s3.amazonaws.com" {
        return true;
    }
    let Some(rest) = host.strip_prefix(PRODUCTION_ARTIFACT_HOST_PREFIX) else {
        return false;
    };
    (rest.ends_with(".amazonaws.com")
        && rest.strip_suffix(".amazonaws.com").is_some_and(|region| {
            !region.is_empty() && !region.contains('/') && !region.contains('.')
        }))
        || rest == "amazonaws.com"
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
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
        let payload = Self::launch_payload(request)?;
        let response = match self
            .request::<CursorCreateResponse>(Method::POST, "/v1/agents", Some(payload))
            .await
        {
            Ok(response) => response,
            Err(ExternalWorkerAdapterError::Provider { status, code })
                if status == StatusCode::CONFLICT =>
            {
                return self.reconcile_launch_conflict(request, code).await;
            }
            Err(error) => return Err(error),
        };
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
        let id = checked_id(external_agent_id)?;
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
        let agent_id = checked_id(external_agent_id)?;
        let run_id = checked_id(external_run_id)?;
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
        let agent_id = checked_id(external_agent_id)?;
        let worker = self.get_worker(agent_id).await?;
        if worker_ineligible_for_follow_up(worker.state) {
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
        let response = match self
            .request::<CursorFollowUpResponse>(
                Method::POST,
                &format!("/v1/agents/{agent_id}/runs"),
                Some(json!({ "prompt": { "text": request.prompt } })),
            )
            .await
        {
            Ok(response) => response,
            Err(ExternalWorkerAdapterError::Provider { status, code })
                if status == StatusCode::CONFLICT =>
            {
                return self.reconcile_follow_up_conflict(agent_id, code).await;
            }
            Err(error) => return Err(error),
        };
        run_record(&response.run, agent_id)
    }

    async fn list_artifacts(
        &self,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<Vec<ExternalWorkerArtifact>, ExternalWorkerAdapterError> {
        let id = checked_id(external_agent_id)?;
        let run_id = checked_id(external_run_id)?;
        let response: CursorArtifacts = self
            .request(Method::GET, &format!("/v1/agents/{id}/artifacts"), None)
            .await?;
        let mut artifacts = Vec::with_capacity(response.items.len());
        for item in response.items {
            if !Self::artifact_path_is_safe(&item.path) {
                return Err(ExternalWorkerAdapterError::InvalidResponse(
                    "Cursor artifact path is not provider-relative",
                ));
            }
            let Some(item_run_id) = item.run_id.as_deref() else {
                return Err(ExternalWorkerAdapterError::InvalidResponse(
                    "Cursor artifact is not attributed to the requested run",
                ));
            };
            if item_run_id != run_id {
                return Err(ExternalWorkerAdapterError::InvalidResponse(
                    "Cursor artifact is not attributed to the requested run",
                ));
            }
            let digest = match item.digest.filter(|value| !value.trim().is_empty()) {
                Some(digest) => digest,
                None => {
                    let (digest, hashed_bytes) = self
                        .download_and_hash(id, &item.path, item.size_bytes)
                        .await?;
                    if let Some(reported) = item.size_bytes {
                        if reported != hashed_bytes {
                            return Err(ExternalWorkerAdapterError::InvalidResponse(
                                "Cursor artifact size did not match the downloaded bytes",
                            ));
                        }
                    }
                    digest
                }
            };
            let artifact = ExternalWorkerArtifact {
                path: item.path,
                digest,
                external_run_id: run_id.to_string(),
                size_bytes: item.size_bytes,
            };
            artifact
                .validate()
                .map_err(ExternalWorkerAdapterError::InvalidResponse)?;
            artifacts.push(artifact);
        }
        Ok(artifacts)
    }

    async fn cancel(
        &self,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
        let agent_id = checked_id(external_agent_id)?;
        let run_id = checked_id(external_run_id)?;
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
            Err(ExternalWorkerAdapterError::Provider { status, code })
                if status == StatusCode::CONFLICT =>
            {
                return self.reconcile_cancel(agent_id, run_id, code).await;
            }
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
    #[serde(default, rename = "autoCreatePR", alias = "autoCreatePr")]
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CursorArtifactDownload {
    url: String,
    expires_at: String,
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
        stream: ExternalWorkerStreamState::Unsupported,
        last_seq: None,
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
    let value = value.replace(['\r', '\n'], " ");
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

fn worker_ineligible_for_follow_up(state: ExternalWorkerState) -> bool {
    matches!(
        state,
        ExternalWorkerState::Unknown
            | ExternalWorkerState::Failed
            | ExternalWorkerState::Cancelled
            | ExternalWorkerState::Archived
    )
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
pub(crate) use tests::{spawn_fake_cursor, FakeCursorState, FAKE_AGENT, FAKE_RUN};

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Query, State};
    use axum::http::StatusCode as AxumStatus;
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use serde::Deserialize;
    use std::sync::{Arc, Mutex};

    pub const FAKE_AGENT: &str = "bc-00000000-0000-0000-0000-000000000001";
    pub const FAKE_RUN: &str = "run-00000000-0000-0000-0000-000000000001";

    #[derive(Clone)]
    pub struct FakeCursorState {
        pub launch_requests: Arc<Mutex<Vec<Value>>>,
        pub follow_up_requests: Arc<Mutex<Vec<Value>>>,
        pub cancel_calls: Arc<Mutex<usize>>,
        pub download_calls: Arc<Mutex<usize>>,
        pub cancelled: Arc<Mutex<bool>>,
        pub config: Arc<Mutex<FakeCursorConfig>>,
        pub public_base: Arc<Mutex<String>>,
    }

    #[derive(Clone)]
    pub struct FakeCursorConfig {
        pub agent_status: String,
        pub run_status: String,
        pub listed_runs: Vec<Value>,
        pub create_status: u16,
        pub create_code: Option<String>,
        pub follow_up_status: u16,
        pub follow_up_code: Option<String>,
        pub cancel_status: u16,
        pub cancel_code: Option<String>,
        pub auto_create_pr: Value,
        pub work_on_current_branch: Value,
        pub env: Option<Value>,
        pub repo_url: String,
        pub starting_ref: Option<String>,
        pub artifacts: Value,
        pub artifact_bytes: Vec<u8>,
        pub download_host_override: Option<String>,
        pub omit_download_expiry: bool,
        pub create_delay_ms: u64,
        pub reconcile_cancelled: bool,
        pub defer_listed_runs_until_follow_up: bool,
    }

    impl Default for FakeCursorConfig {
        fn default() -> Self {
            Self {
                agent_status: "ACTIVE".into(),
                run_status: "RUNNING".into(),
                listed_runs: Vec::new(),
                create_status: 200,
                create_code: None,
                follow_up_status: 200,
                follow_up_code: None,
                cancel_status: 200,
                cancel_code: None,
                auto_create_pr: json!(false),
                work_on_current_branch: json!(false),
                env: Some(json!({"type": "cloud"})),
                repo_url: "https://github.com/chriscase/GrokPtah".into(),
                starting_ref: Some("main".into()),
                artifacts: json!({
                    "items": [{
                        "path": "artifacts/report.md",
                        "runId": FAKE_RUN,
                        "digest": "sha256:abc",
                        "sizeBytes": 12
                    }]
                }),
                artifact_bytes: b"hashed-bytes".to_vec(),
                download_host_override: None,
                omit_download_expiry: false,
                create_delay_ms: 0,
                reconcile_cancelled: false,
                defer_listed_runs_until_follow_up: false,
            }
        }
    }

    impl Default for FakeCursorState {
        fn default() -> Self {
            Self {
                launch_requests: Arc::new(Mutex::new(Vec::new())),
                follow_up_requests: Arc::new(Mutex::new(Vec::new())),
                cancel_calls: Arc::new(Mutex::new(0)),
                download_calls: Arc::new(Mutex::new(0)),
                cancelled: Arc::new(Mutex::new(false)),
                config: Arc::new(Mutex::new(FakeCursorConfig::default())),
                public_base: Arc::new(Mutex::new(String::new())),
            }
        }
    }

    fn fake_agent(config: &FakeCursorConfig) -> Value {
        let mut agent = json!({
            "id": FAKE_AGENT,
            "url": format!("https://cursor.com/agents/{FAKE_AGENT}"),
            "status": config.agent_status,
            "repos": [{
                "url": config.repo_url,
                "startingRef": config.starting_ref,
            }],
            "autoCreatePR": config.auto_create_pr,
            "workOnCurrentBranch": config.work_on_current_branch,
            "createdAt": "2026-08-24T00:00:00Z",
            "updatedAt": "2026-08-24T00:00:01Z"
        });
        if let Some(env) = &config.env {
            agent["env"] = env.clone();
        }
        agent
    }

    fn fake_run(id: &str, status: &str) -> Value {
        json!({
            "id": id,
            "agentId": FAKE_AGENT,
            "status": status,
            "createdAt": "2026-08-24T00:00:00Z",
            "updatedAt": "2026-08-24T00:00:02Z",
            "result": "completed\nwith two lines"
        })
    }

    async fn fake_create(
        State(state): State<FakeCursorState>,
        Json(body): Json<Value>,
    ) -> impl IntoResponse {
        let delay = state.config.lock().unwrap().create_delay_ms;
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        state.launch_requests.lock().unwrap().push(body);
        let config = state.config.lock().unwrap().clone();
        if config.create_status != 200 {
            return (
                AxumStatus::from_u16(config.create_status).unwrap(),
                Json(json!({"code": config.create_code})),
            );
        }
        (
            AxumStatus::OK,
            Json(json!({
                "agent": fake_agent(&config),
                "run": fake_run(FAKE_RUN, "CREATING"),
            })),
        )
    }

    async fn fake_agent_read(State(state): State<FakeCursorState>) -> Json<Value> {
        let config = state.config.lock().unwrap().clone();
        Json(fake_agent(&config))
    }

    async fn fake_run_read(State(state): State<FakeCursorState>) -> Json<Value> {
        let cancelled = *state.cancelled.lock().unwrap();
        let cancel_calls = *state.cancel_calls.lock().unwrap();
        let config = state.config.lock().unwrap().clone();
        let status = if cancelled || (config.reconcile_cancelled && cancel_calls > 0) {
            "CANCELLED"
        } else {
            config.run_status.as_str()
        };
        Json(fake_run(FAKE_RUN, status))
    }

    async fn fake_follow_up(
        State(state): State<FakeCursorState>,
        Json(body): Json<Value>,
    ) -> impl IntoResponse {
        state.follow_up_requests.lock().unwrap().push(body);
        let config = state.config.lock().unwrap().clone();
        if config.follow_up_status != 200 {
            return (
                AxumStatus::from_u16(config.follow_up_status).unwrap(),
                Json(json!({"code": config.follow_up_code})),
            );
        }
        (
            AxumStatus::OK,
            Json(json!({
                "run": fake_run("run-00000000-0000-0000-0000-000000000002", "CREATING")
            })),
        )
    }

    async fn fake_runs(State(state): State<FakeCursorState>) -> Json<Value> {
        let posted = !state.follow_up_requests.lock().unwrap().is_empty();
        let config = state.config.lock().unwrap().clone();
        let items = if config.defer_listed_runs_until_follow_up && !posted {
            Vec::new()
        } else {
            config.listed_runs
        };
        Json(json!({ "items": items }))
    }

    async fn fake_cancel(State(state): State<FakeCursorState>) -> impl IntoResponse {
        *state.cancel_calls.lock().unwrap() += 1;
        let config = state.config.lock().unwrap().clone();
        if config.cancel_status != 200 {
            return (
                AxumStatus::from_u16(config.cancel_status).unwrap(),
                Json(json!({"code": config.cancel_code})),
            );
        }
        *state.cancelled.lock().unwrap() = true;
        (AxumStatus::OK, Json(json!({"id": FAKE_RUN})))
    }

    async fn fake_artifacts(State(state): State<FakeCursorState>) -> Json<Value> {
        Json(state.config.lock().unwrap().artifacts.clone())
    }

    #[derive(Deserialize)]
    struct DownloadQuery {
        path: String,
    }

    async fn fake_download(
        State(state): State<FakeCursorState>,
        Query(query): Query<DownloadQuery>,
    ) -> impl IntoResponse {
        *state.download_calls.lock().unwrap() += 1;
        if !query.path.starts_with("artifacts/") {
            return (
                AxumStatus::BAD_REQUEST,
                Json(json!({"code": "invalid_path"})),
            );
        }
        let config = state.config.lock().unwrap().clone();
        let base = if let Some(host) = config.download_host_override {
            host
        } else {
            state.public_base.lock().unwrap().clone()
        };
        let mut body = json!({ "url": format!("{base}/artifact-bytes") });
        if !config.omit_download_expiry {
            body["expiresAt"] = json!("2099-01-01T00:00:00Z");
        }
        (AxumStatus::OK, Json(body))
    }

    async fn fake_artifact_bytes(State(state): State<FakeCursorState>) -> impl IntoResponse {
        let bytes = state.config.lock().unwrap().artifact_bytes.clone();
        bytes
    }

    pub async fn spawn_fake_cursor(state: FakeCursorState) -> String {
        let app = Router::new()
            .route("/v1/agents", post(fake_create))
            .route(&format!("/v1/agents/{FAKE_AGENT}"), get(fake_agent_read))
            .route(
                &format!("/v1/agents/{FAKE_AGENT}/runs/{FAKE_RUN}"),
                get(fake_run_read),
            )
            .route(
                &format!("/v1/agents/{FAKE_AGENT}/runs"),
                post(fake_follow_up).get(fake_runs),
            )
            .route(
                &format!("/v1/agents/{FAKE_AGENT}/runs/{FAKE_RUN}/cancel"),
                post(fake_cancel),
            )
            .route(
                &format!("/v1/agents/{FAKE_AGENT}/artifacts"),
                get(fake_artifacts),
            )
            .route(
                &format!("/v1/agents/{FAKE_AGENT}/artifacts/download"),
                get(fake_download),
            )
            .route("/artifact-bytes", get(fake_artifact_bytes))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let base = format!("http://{address}");
        *state.public_base.lock().unwrap() = base.clone();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        base
    }

    fn launch_request(request_id: &str) -> ExternalWorkerLaunchRequest {
        ExternalWorkerLaunchRequest {
            request_id: request_id.into(),
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
    fn provider_statuses_fail_closed() {
        assert_eq!(run_state("RUNNING"), ExternalWorkerState::Running);
        assert_eq!(run_state("CANCELLED"), ExternalWorkerState::Cancelled);
        assert_eq!(
            run_state("future-provider-state"),
            ExternalWorkerState::Unknown
        );
        assert_eq!(agent_state("ARCHIVED"), ExternalWorkerState::Archived);
        assert_eq!(agent_state("mystery"), ExternalWorkerState::Unknown);
    }

    #[test]
    fn production_artifact_hosts_are_narrow() {
        assert!(production_artifact_host_allowed(
            "cloud-agent-artifacts.s3.us-east-1.amazonaws.com"
        ));
        assert!(production_artifact_host_allowed(
            "cloud-agent-artifacts.s3.amazonaws.com"
        ));
        assert!(!production_artifact_host_allowed("s3.amazonaws.com"));
        assert!(!production_artifact_host_allowed("evil.example"));
        assert!(!production_artifact_host_allowed(
            "cloud-agent-artifacts.s3.us-east-1.amazonaws.com.evil.example"
        ));
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

    #[test]
    fn cursor_safety_flags_accept_official_auto_create_pr_casing() {
        let agent: CursorAgent = serde_json::from_value(json!({
            "id": FAKE_AGENT,
            "status": "ACTIVE",
            "repos": [{"url": "https://github.com/chriscase/GrokPtah", "startingRef": "main"}],
            "autoCreatePR": false,
            "workOnCurrentBranch": false,
            "createdAt": "2026-08-24T00:00:00Z",
            "updatedAt": "2026-08-24T00:00:01Z"
        }))
        .unwrap();
        assert_eq!(agent.auto_create_pr, Some(false));
        assert_eq!(agent.work_on_current_branch, Some(false));
    }

    #[test]
    fn follow_up_rejects_unknown_failed_cancelled_and_archived_states() {
        for state in [
            ExternalWorkerState::Unknown,
            ExternalWorkerState::Failed,
            ExternalWorkerState::Cancelled,
            ExternalWorkerState::Archived,
        ] {
            assert!(worker_ineligible_for_follow_up(state));
        }
        assert!(!worker_ineligible_for_follow_up(ExternalWorkerState::Ready));
        assert!(!worker_ineligible_for_follow_up(
            ExternalWorkerState::Running
        ));
    }

    #[tokio::test]
    async fn fake_cursor_api_covers_launch_poll_artifacts_and_terminal_cancel() {
        let state = FakeCursorState::default();
        let base = spawn_fake_cursor(state.clone()).await;
        let adapter = CursorCloudAdapter::for_test(&base);
        let request = launch_request("request-1");
        let launch = adapter.launch(&request).await.unwrap();
        assert_eq!(launch.worker.external_agent_id, FAKE_AGENT);
        assert_eq!(launch.run.state, ExternalWorkerState::Provisioning);
        assert_eq!(launch.run.stream, ExternalWorkerStreamState::Unsupported);
        assert_eq!(launch.run.last_seq, None);
        {
            let sent = state.launch_requests.lock().unwrap();
            assert_eq!(sent.len(), 1);
            assert_eq!(sent[0]["repos"][0]["startingRef"], "main");
            assert_eq!(sent[0]["autoCreatePR"], false);
            assert!(sent[0].get("env").is_none());
            assert!(sent[0].get("agentId").is_none());
        }

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
        assert_eq!(run.last_seq, None);
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
        let artifacts = adapter
            .list_artifacts(
                &launch.worker.external_agent_id,
                &launch.run.external_run_id,
            )
            .await
            .unwrap();
        assert_eq!(artifacts[0].path, "artifacts/report.md");
        assert_eq!(artifacts[0].external_run_id, FAKE_RUN);
        let cancelled = adapter
            .cancel(
                &launch.worker.external_agent_id,
                &launch.run.external_run_id,
            )
            .await
            .unwrap();
        assert_eq!(cancelled.state, ExternalWorkerState::Cancelled);
    }

    #[tokio::test]
    async fn negative_response_flags_fail_closed() {
        for mutate in [
            |config: &mut FakeCursorConfig| config.auto_create_pr = json!(true),
            |config: &mut FakeCursorConfig| config.auto_create_pr = Value::Null,
            |config: &mut FakeCursorConfig| config.work_on_current_branch = json!(true),
            |config: &mut FakeCursorConfig| config.work_on_current_branch = Value::Null,
            |config: &mut FakeCursorConfig| config.env = Some(json!({"type": "pool"})),
            |config: &mut FakeCursorConfig| {
                config.repo_url = "https://github.com/other/repo".into();
            },
            |config: &mut FakeCursorConfig| config.starting_ref = Some("other".into()),
            |config: &mut FakeCursorConfig| config.starting_ref = None,
        ] {
            let state = FakeCursorState::default();
            mutate(&mut state.config.lock().unwrap());
            let base = spawn_fake_cursor(state).await;
            let adapter = CursorCloudAdapter::for_test(&base);
            assert!(
                adapter.launch(&launch_request("request-1")).await.is_err(),
                "response-flag mutant must fail closed"
            );
        }
    }

    #[tokio::test]
    async fn unknown_failed_cancelled_and_archived_workers_reject_follow_up() {
        for status in ["UNKNOWN", "DELETED", "ARCHIVED"] {
            let state = FakeCursorState::default();
            state.config.lock().unwrap().agent_status = status.into();
            let base = spawn_fake_cursor(state).await;
            let adapter = CursorCloudAdapter::for_test(&base);
            let error = adapter
                .follow_up(
                    FAKE_AGENT,
                    &ExternalWorkerFollowUpRequest {
                        request_id: "follow-up-1".into(),
                        prompt: "continue".into(),
                        bounds: None,
                    },
                )
                .await
                .unwrap_err();
            assert!(matches!(
                error,
                ExternalWorkerAdapterError::InvalidRequest(
                    "Cursor worker is not eligible for a follow-up"
                )
            ));
        }
    }

    #[tokio::test]
    async fn active_run_and_follow_up_bounds_are_rejected() {
        let state = FakeCursorState::default();
        state.config.lock().unwrap().listed_runs = vec![fake_run(FAKE_RUN, "RUNNING")];
        let base = spawn_fake_cursor(state).await;
        let adapter = CursorCloudAdapter::for_test(&base);
        let busy = adapter
            .follow_up(
                FAKE_AGENT,
                &ExternalWorkerFollowUpRequest {
                    request_id: "follow-up-1".into(),
                    prompt: "continue".into(),
                    bounds: None,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            busy,
            ExternalWorkerAdapterError::InvalidRequest("Cursor worker already has an active run")
        ));
        let bounds = adapter
            .follow_up(
                FAKE_AGENT,
                &ExternalWorkerFollowUpRequest {
                    request_id: "follow-up-2".into(),
                    prompt: "continue".into(),
                    bounds: Some(grokptah_agent_sdk::Bounds {
                        max_rounds: Some(2),
                        ..Default::default()
                    }),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            bounds,
            ExternalWorkerAdapterError::InvalidRequest(
                "Cursor follow-up bounds are not supported by the provider API"
            )
        ));
    }

    #[tokio::test]
    async fn agent_busy_conflict_is_reconciled_with_get_state() {
        let state = FakeCursorState::default();
        {
            let mut config = state.config.lock().unwrap();
            config.follow_up_status = 409;
            config.follow_up_code = Some("agent_busy".into());
            config.listed_runs = vec![fake_run(FAKE_RUN, "RUNNING")];
            config.defer_listed_runs_until_follow_up = true;
        }
        let base = spawn_fake_cursor(state.clone()).await;
        let adapter = CursorCloudAdapter::for_test(&base);
        let error = adapter
            .follow_up(
                FAKE_AGENT,
                &ExternalWorkerFollowUpRequest {
                    request_id: "follow-up-1".into(),
                    prompt: "continue".into(),
                    bounds: None,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ExternalWorkerAdapterError::InvalidRequest("Cursor worker already has an active run")
        ));
        assert_eq!(state.follow_up_requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn launch_agent_conflict_is_reconciled_with_get_without_a_second_create() {
        let state = FakeCursorState::default();
        {
            let mut config = state.config.lock().unwrap();
            config.create_status = 409;
            config.create_code = Some("agent_id_conflict".into());
            config.listed_runs = vec![fake_run(FAKE_RUN, "CREATING")];
        }
        let base = spawn_fake_cursor(state.clone()).await;
        let adapter = CursorCloudAdapter::for_test(&base);
        let result = adapter
            .launch(&launch_request(FAKE_AGENT))
            .await
            .expect("conflict should reconcile via GET");
        assert_eq!(result.worker.external_agent_id, FAKE_AGENT);
        assert_eq!(result.run.external_run_id, FAKE_RUN);
        assert_eq!(state.launch_requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cancellation_409_reconciles_to_observed_cancelled() {
        let state = FakeCursorState::default();
        {
            let mut config = state.config.lock().unwrap();
            config.cancel_status = 409;
            config.cancel_code = Some("run_not_cancellable".into());
            config.reconcile_cancelled = true;
            config.run_status = "RUNNING".into();
        }
        let base = spawn_fake_cursor(state.clone()).await;
        let adapter = CursorCloudAdapter::for_test(&base);
        let run = adapter.cancel(FAKE_AGENT, FAKE_RUN).await.unwrap();
        assert_eq!(run.state, ExternalWorkerState::Cancelled);
        assert_eq!(*state.cancel_calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn cancellation_409_while_still_running_is_uncertain() {
        let state = FakeCursorState::default();
        {
            let mut config = state.config.lock().unwrap();
            config.cancel_status = 409;
            config.cancel_code = Some("run_not_cancellable".into());
            config.run_status = "RUNNING".into();
        }
        let base = spawn_fake_cursor(state).await;
        let adapter = CursorCloudAdapter::for_test(&base);
        assert!(matches!(
            adapter.cancel(FAKE_AGENT, FAKE_RUN).await.unwrap_err(),
            ExternalWorkerAdapterError::Uncertain
        ));
    }

    #[tokio::test]
    async fn artifacts_without_run_attribution_fail_closed_without_download() {
        let state = FakeCursorState::default();
        state.config.lock().unwrap().artifacts = json!({
            "items": [{ "path": "artifacts/report.md", "sizeBytes": 12 }]
        });
        let base = spawn_fake_cursor(state.clone()).await;
        let adapter = CursorCloudAdapter::for_test(&base);
        let error = adapter
            .list_artifacts(FAKE_AGENT, FAKE_RUN)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ExternalWorkerAdapterError::InvalidResponse(
                "Cursor artifact is not attributed to the requested run"
            )
        ));
        assert_eq!(*state.download_calls.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn unsafe_or_mismatched_artifact_paths_fail_closed_without_download() {
        for artifacts in [
            json!({"items":[{"path":"../secret","runId": FAKE_RUN,"digest":"sha256:abc"}]}),
            json!({"items":[{"path":"reports/review.json","runId": FAKE_RUN,"digest":"sha256:abc"}]}),
            json!({"items":[{"path":"artifacts/report.md","runId":"other-run","digest":"sha256:abc"}]}),
        ] {
            let state = FakeCursorState::default();
            state.config.lock().unwrap().artifacts = artifacts;
            let base = spawn_fake_cursor(state.clone()).await;
            let adapter = CursorCloudAdapter::for_test(&base);
            assert!(adapter.list_artifacts(FAKE_AGENT, FAKE_RUN).await.is_err());
            assert_eq!(*state.download_calls.lock().unwrap(), 0);
        }
    }

    #[tokio::test]
    async fn artifacts_with_run_id_and_no_digest_are_downloaded_and_hashed() {
        let state = FakeCursorState::default();
        {
            let mut config = state.config.lock().unwrap();
            config.artifacts = json!({
                "items": [{
                    "path": "artifacts/report.md",
                    "runId": FAKE_RUN,
                    "sizeBytes": 12
                }]
            });
            config.artifact_bytes = b"hashed-bytes".to_vec();
        }
        let base = spawn_fake_cursor(state.clone()).await;
        let adapter = CursorCloudAdapter::for_test(&base);
        let artifacts = adapter.list_artifacts(FAKE_AGENT, FAKE_RUN).await.unwrap();
        assert_eq!(artifacts[0].external_run_id, FAKE_RUN);
        assert_eq!(
            artifacts[0].digest,
            format!("sha256:{}", hex_sha256(&Sha256::digest(b"hashed-bytes")))
        );
        assert_eq!(*state.download_calls.lock().unwrap(), 1);
        assert!(
            adapter.list_artifacts(FAKE_AGENT, FAKE_RUN).await.unwrap()[0]
                .digest
                .starts_with("sha256:")
        );
    }

    #[tokio::test]
    async fn artifact_download_rejects_unapproved_hosts_and_byte_ceiling() {
        let state = FakeCursorState::default();
        {
            let mut config = state.config.lock().unwrap();
            config.artifacts = json!({
                "items": [{
                    "path": "artifacts/report.md",
                    "runId": FAKE_RUN
                }]
            });
            config.download_host_override = Some("https://evil.example".into());
        }
        let base = spawn_fake_cursor(state).await;
        let adapter = CursorCloudAdapter::for_test(&base);
        assert!(adapter.list_artifacts(FAKE_AGENT, FAKE_RUN).await.is_err());

        let state = FakeCursorState::default();
        {
            let mut config = state.config.lock().unwrap();
            config.artifacts = json!({
                "items": [{
                    "path": "artifacts/report.md",
                    "runId": FAKE_RUN,
                    "sizeBytes": 4096
                }]
            });
            config.artifact_bytes = vec![b'x'; 4096];
        }
        let base = spawn_fake_cursor(state).await;
        let adapter = CursorCloudAdapter::for_test(&base).with_max_artifact_bytes(16);
        assert!(adapter.list_artifacts(FAKE_AGENT, FAKE_RUN).await.is_err());
    }
}
