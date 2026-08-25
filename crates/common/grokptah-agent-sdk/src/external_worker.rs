//! Provider-neutral contracts for external coding-agent workers.
//!
//! These types describe a worker that GrokPtah schedules outside the local
//! authority, such as a cloud coding agent. They intentionally contain no
//! credentials, network client, filesystem path, or execution policy. A
//! trusted adapter owns those concerns and maps provider responses into these
//! bounded projections.

use serde::{Deserialize, Serialize};

use crate::run::Bounds;

/// Contract identifier for external-worker DTOs.
pub const EXTERNAL_WORKER_CONTRACT_VERSION: &str = "grokptah.external-workers.v1";

/// Maximum UTF-8 bytes accepted for an external worker prompt.
pub const MAX_EXTERNAL_WORKER_PROMPT_BYTES: usize = 1_048_576;
/// Maximum UTF-8 bytes accepted for a provider or opaque external identity.
pub const MAX_EXTERNAL_WORKER_ID_BYTES: usize = 256;
/// Maximum UTF-8 bytes accepted for a repository/ref identity.
pub const MAX_EXTERNAL_WORKER_REF_BYTES: usize = 512;
/// Maximum UTF-8 bytes accepted for a redacted worker detail string.
pub const MAX_EXTERNAL_WORKER_DETAIL_BYTES: usize = 4_096;

/// Known external worker families supported by the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalWorkerProvider {
    /// Cursor's hosted Cloud Agents API.
    CursorCloud,
    /// A hosted Claude Code adapter, when a host has qualified one.
    ClaudeCodeCloud,
    /// A host-owned worker process or service.
    LocalWorker,
    /// A provider known to the adapter but not yet standardized here.
    Custom,
}

/// Isolation mode requested from an external worker adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalWorkerExecutionMode {
    /// A disposable provider-managed checkout or workspace.
    Isolated,
}

/// Lifecycle state projected by an external worker adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalWorkerState {
    /// The provider is creating the worker.
    Provisioning,
    /// The worker exists but has not started a run.
    Ready,
    /// A provider run is active.
    Running,
    /// The provider reported verified terminal success.
    Completed,
    /// The provider reported terminal failure.
    Failed,
    /// The worker or run was explicitly cancelled.
    Cancelled,
    /// The provider worker was archived after completion.
    Archived,
    /// The provider returned a state this adapter does not recognize.
    Unknown,
}

/// Bounded request for creating an external worker and its initial run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalWorkerLaunchRequest {
    /// Fresh idempotency key for this creation intent.
    pub request_id: String,
    /// Provider family selected by policy.
    pub provider: ExternalWorkerProvider,
    /// Optional provider identifier for custom adapters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// Exact repository identity approved by the authority.
    pub repository: String,
    /// Exact starting Git ref; adapters must not silently substitute main.
    pub starting_ref: String,
    /// Prompt sent to the provider after policy validation.
    pub prompt: String,
    /// Optional model/profile label, never a credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// External work is isolated by contract.
    pub execution_mode: ExternalWorkerExecutionMode,
    /// Reserved for wire compatibility; external launches must keep this
    /// false. Promotion/merge is a separate approval action.
    #[serde(default)]
    pub auto_create_pr: bool,
    /// Optional host/provider ceilings for the initial run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Bounds>,
}

impl ExternalWorkerLaunchRequest {
    /// Validate caller data before it reaches a provider adapter.
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_identity(&self.request_id, "request_id")?;
        validate_ref(&self.repository, "repository")?;
        validate_ref(&self.starting_ref, "starting_ref")?;
        if self.prompt.trim().is_empty() || self.prompt.len() > MAX_EXTERNAL_WORKER_PROMPT_BYTES {
            return Err("prompt must be non-empty and bounded");
        }
        if self.auto_create_pr {
            return Err("external worker launches must not create pull requests");
        }
        if let Some(provider_id) = &self.provider_id {
            validate_identity(provider_id, "provider_id")?;
        }
        if let Some(model) = &self.model {
            validate_identity(model, "model")?;
        }
        if self.provider == ExternalWorkerProvider::Custom && self.provider_id.is_none() {
            return Err("custom workers require provider_id");
        }
        if let Some(bounds) = &self.bounds {
            bounds.validate()?;
        }
        Ok(())
    }
}

/// Bounded prompt for a follow-up run on an existing external worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalWorkerFollowUpRequest {
    /// Fresh idempotency key for this follow-up intent.
    pub request_id: String,
    /// Prompt sent to the provider after policy validation.
    pub prompt: String,
    /// Optional host/provider ceilings for this run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Bounds>,
}

impl ExternalWorkerFollowUpRequest {
    /// Validate caller data before it reaches a provider adapter.
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_identity(&self.request_id, "request_id")?;
        if self.prompt.trim().is_empty() || self.prompt.len() > MAX_EXTERNAL_WORKER_PROMPT_BYTES {
            return Err("prompt must be non-empty and bounded");
        }
        if let Some(bounds) = &self.bounds {
            bounds.validate()?;
        }
        Ok(())
    }
}

/// Durable, share-safe identity for one external worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalWorkerRecord {
    /// Provider family that owns the opaque IDs.
    pub provider: ExternalWorkerProvider,
    /// Provider-specific adapter ID for custom providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// Opaque provider worker/agent identity.
    pub external_agent_id: String,
    /// Exact repository used for creation.
    pub repository: String,
    /// Exact Git ref used for creation.
    pub starting_ref: String,
    /// Current projected lifecycle state.
    pub state: ExternalWorkerState,
    /// Provider branch, if it has been reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Provider URL, if share-safe and explicitly allowed by the adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_url: Option<String>,
    /// RFC3339 creation timestamp.
    pub created_at: String,
    /// RFC3339 last-update timestamp.
    pub updated_at: String,
}

impl ExternalWorkerRecord {
    /// Validate the bounded worker identity before publishing it.
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_identity(&self.external_agent_id, "external_agent_id")?;
        validate_ref(&self.repository, "repository")?;
        validate_ref(&self.starting_ref, "starting_ref")?;
        if let Some(provider_id) = &self.provider_id {
            validate_identity(provider_id, "provider_id")?;
        }
        if self.provider == ExternalWorkerProvider::Custom && self.provider_id.is_none() {
            return Err("custom workers require provider_id");
        }
        if let Some(branch) = &self.branch {
            validate_ref(branch, "branch")?;
        }
        if let Some(url) = &self.worker_url {
            validate_worker_url(url, self.provider)?;
        }
        if self.created_at.trim().is_empty()
            || self.updated_at.trim().is_empty()
            || contains_control(&self.created_at)
            || contains_control(&self.updated_at)
        {
            return Err("worker timestamps must not be empty");
        }
        Ok(())
    }
}

/// Durable, share-safe identity for one provider run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalWorkerRunRecord {
    /// Opaque provider worker identity.
    pub external_agent_id: String,
    /// Opaque provider run identity.
    pub external_run_id: String,
    /// Current projected lifecycle state.
    pub state: ExternalWorkerState,
    /// Last provider event sequence retained by the adapter.
    pub last_seq: u64,
    /// Bounded terminal label, if terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_result: Option<String>,
    /// RFC3339 creation timestamp.
    pub created_at: String,
    /// RFC3339 last-update timestamp.
    pub updated_at: String,
}

impl ExternalWorkerRunRecord {
    /// Validate a provider run projection before publishing it.
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_identity(&self.external_agent_id, "external_agent_id")?;
        validate_identity(&self.external_run_id, "external_run_id")?;
        if let Some(result) = &self.terminal_result {
            validate_detail(result, "terminal_result")?;
        }
        if self.created_at.trim().is_empty()
            || self.updated_at.trim().is_empty()
            || contains_control(&self.created_at)
            || contains_control(&self.updated_at)
        {
            return Err("run timestamps must not be empty");
        }
        Ok(())
    }
}

/// The share-safe envelope returned after an external worker is created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalWorkerLaunchResult {
    /// Durable worker identity and exact source projection.
    pub worker: ExternalWorkerRecord,
    /// Initial run identity and lifecycle projection.
    pub run: ExternalWorkerRunRecord,
}

impl ExternalWorkerLaunchResult {
    /// Validate both projections before publishing a launch result.
    pub fn validate(&self) -> Result<(), &'static str> {
        self.worker.validate()?;
        self.run.validate()?;
        if self.run.external_agent_id != self.worker.external_agent_id {
            return Err("launch worker and run identities must match");
        }
        Ok(())
    }
}

/// Redacted event retained for status, replay, and UI monitoring.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalWorkerEvent {
    /// Strictly increasing provider sequence.
    pub seq: u64,
    /// RFC3339 event timestamp.
    pub ts: String,
    /// Stable provider-neutral event kind.
    pub kind: String,
    /// Redacted bounded detail; raw tool output must not be placed here.
    pub detail: String,
}

impl ExternalWorkerEvent {
    /// Validate a redacted event before it crosses a broker boundary.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.ts.trim().is_empty()
            || self.kind.trim().is_empty()
            || contains_control(&self.ts)
            || contains_control(&self.kind)
        {
            return Err("worker event metadata must not be empty");
        }
        if self.kind.len() > 128 {
            return Err("worker event kind exceeds its byte bound");
        }
        validate_detail(&self.detail, "event detail")
    }
}

/// A bounded provider artifact available for review or download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalWorkerArtifact {
    /// Repository-relative or provider-relative artifact path.
    pub path: String,
    /// Content digest supplied by the provider or adapter.
    pub digest: String,
    /// Bounded artifact size, if reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

impl ExternalWorkerArtifact {
    /// Validate an artifact reference without allowing host paths.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.path.trim().is_empty()
            || self.path.len() > MAX_EXTERNAL_WORKER_REF_BYTES
            || self.path.starts_with('/')
            || self.path.contains('\\')
            || self.path.split('/').any(|segment| segment == "..")
        {
            return Err("artifact path must be bounded and relative");
        }
        validate_identity(&self.digest, "digest")
    }
}

fn validate_identity(value: &str, field: &str) -> Result<(), &'static str> {
    if value.trim().is_empty() {
        return Err(match field {
            "request_id" => "request_id must not be empty",
            "repository" => "repository must not be empty",
            "provider_id" => "provider_id must not be empty",
            "model" => "model must not be empty",
            "external_agent_id" => "external_agent_id must not be empty",
            "external_run_id" => "external_run_id must not be empty",
            "digest" => "digest must not be empty",
            _ => "worker identity must not be empty",
        });
    }
    if value.len() > MAX_EXTERNAL_WORKER_ID_BYTES {
        return Err("worker identity exceeds its byte bound");
    }
    if contains_control(value) {
        return Err("worker identity contains a control character");
    }
    Ok(())
}

fn contains_control(value: &str) -> bool {
    value
        .chars()
        .any(|character| character.is_control() || character == '\u{7f}')
}

fn validate_worker_url(value: &str, provider: ExternalWorkerProvider) -> Result<(), &'static str> {
    if value.trim().is_empty()
        || value.len() > MAX_EXTERNAL_WORKER_REF_BYTES
        || !value.starts_with("https://")
        || value
            .chars()
            .any(|character| character.is_control() || character == '\u{7f}')
        || value.contains('?')
        || value.contains('#')
    {
        return Err("worker_url must be a bounded credential-free https URL");
    }
    let authority = value["https://".len()..]
        .split('/')
        .next()
        .unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err("worker_url must not contain userinfo");
    }
    if provider == ExternalWorkerProvider::CursorCloud {
        let host = authority
            .split(':')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if host != "cursor.com" && !host.ends_with(".cursor.com") {
            return Err("cursor worker_url must use cursor.com");
        }
    }
    Ok(())
}

fn validate_ref(value: &str, field: &str) -> Result<(), &'static str> {
    if value.trim().is_empty() {
        return Err(match field {
            "repository" => "repository must not be empty",
            "starting_ref" => "starting_ref must not be empty",
            "branch" => "branch must not be empty",
            _ => "worker ref must not be empty",
        });
    }
    if contains_control(value) {
        return Err("worker identity contains a control character");
    }
    if value.len() > MAX_EXTERNAL_WORKER_REF_BYTES
        || value.starts_with('/')
        || value.contains('\\')
        || value.split('/').any(|segment| segment == "..")
    {
        return Err("worker ref must be bounded and non-absolute");
    }
    Ok(())
}

fn validate_detail(value: &str, field: &str) -> Result<(), &'static str> {
    if value.len() > MAX_EXTERNAL_WORKER_DETAIL_BYTES {
        return Err(match field {
            "terminal_result" => "terminal_result exceeds its byte bound",
            _ => "worker detail exceeds its byte bound",
        });
    }
    let lower = value.to_ascii_lowercase();
    if value
        .chars()
        .any(|character| character.is_control() || character == '\u{7f}')
        || [
            "/users/",
            "/private/",
            "/var/",
            "/tmp/",
            "/home/",
            "/volumes/",
            "\\users\\",
            "http://",
            "https://",
            "authorization",
            "bearer ",
            "api_key",
            "xai_api_key",
            "grokptah_home",
            "clipboard",
            "private_key",
            "password",
            "cookie",
            "session_token",
            "secret",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        return Err("worker detail contains privileged data");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch() -> ExternalWorkerLaunchRequest {
        ExternalWorkerLaunchRequest {
            request_id: "req-1".into(),
            provider: ExternalWorkerProvider::CursorCloud,
            provider_id: None,
            repository: "chriscase/GrokPtah".into(),
            starting_ref: "refs/heads/codex/review".into(),
            prompt: "Review the exact candidate".into(),
            model: Some("composer".into()),
            execution_mode: ExternalWorkerExecutionMode::Isolated,
            auto_create_pr: false,
            bounds: Some(Bounds {
                max_rounds: Some(8),
                ..Bounds::default()
            }),
        }
    }

    #[test]
    fn launch_request_serializes_provider_and_exact_ref() {
        let request = launch();
        request.validate().expect("launch request is valid");
        let value = serde_json::to_value(request).expect("launch serializes");
        assert_eq!(value["provider"], "cursor_cloud");
        assert_eq!(value["startingRef"], "refs/heads/codex/review");
        assert_eq!(value["executionMode"], "isolated");
        assert_eq!(value["autoCreatePr"], false);
    }

    #[test]
    fn launch_rejects_pull_request_creation() {
        let mut request = launch();
        request.auto_create_pr = true;
        assert_eq!(
            request.validate(),
            Err("external worker launches must not create pull requests")
        );
    }

    #[test]
    fn custom_provider_requires_an_opaque_provider_id() {
        let mut request = launch();
        request.provider = ExternalWorkerProvider::Custom;
        assert_eq!(
            request.validate(),
            Err("custom workers require provider_id")
        );
        request.provider_id = Some("company-gateway".into());
        assert!(request.validate().is_ok());
    }

    #[test]
    fn launch_rejects_host_paths_and_control_identities() {
        let mut request = launch();
        request.repository = "/Users/secret/repo".into();
        assert_eq!(
            request.validate(),
            Err("worker ref must be bounded and non-absolute")
        );
        request.repository = "chriscase/GrokPtah".into();
        request.starting_ref = "refs/heads/review\n".into();
        assert_eq!(
            request.validate(),
            Err("worker identity contains a control character")
        );
    }

    #[test]
    fn artifacts_are_relative_and_events_are_bounded() {
        let artifact = ExternalWorkerArtifact {
            path: "reports/review.json".into(),
            digest: "sha256:abc".into(),
            size_bytes: Some(42),
        };
        assert!(artifact.validate().is_ok());
        let bad = ExternalWorkerArtifact {
            path: "../secret".into(),
            ..artifact
        };
        assert!(bad.validate().is_err());
        let event = ExternalWorkerEvent {
            seq: 1,
            ts: "2026-08-24T00:00:00Z".into(),
            kind: "run.progress".into(),
            detail: "checking tests".into(),
        };
        assert!(event.validate().is_ok());
        let mut privileged = event.clone();
        privileged.detail = "Authorization: secret".into();
        assert_eq!(
            privileged.validate(),
            Err("worker detail contains privileged data")
        );
        privileged.detail = "https://example.test/report".into();
        assert_eq!(
            privileged.validate(),
            Err("worker detail contains privileged data")
        );
        privileged.detail = "bounded".into();
        privileged.seq = u64::MAX;
        assert!(privileged.validate().is_ok());
    }

    #[test]
    fn worker_urls_are_credential_free_and_provider_scoped() {
        let mut worker = ExternalWorkerRecord {
            provider: ExternalWorkerProvider::CursorCloud,
            provider_id: None,
            external_agent_id: "agent-1".into(),
            repository: "org/repo".into(),
            starting_ref: "main".into(),
            state: ExternalWorkerState::Running,
            branch: None,
            worker_url: Some("https://cursor.com/agents/agent-1".into()),
            created_at: "now".into(),
            updated_at: "now".into(),
        };
        assert!(worker.validate().is_ok());
        worker.worker_url = Some("https://cursor.com/agents/agent-1?token=secret".into());
        assert_eq!(
            worker.validate(),
            Err("worker_url must be a bounded credential-free https URL")
        );
        worker.worker_url = Some("https://user:secret@cursor.com/agents/agent-1".into());
        assert_eq!(
            worker.validate(),
            Err("worker_url must not contain userinfo")
        );
        worker.worker_url = Some("https://evil.example/agents/agent-1".into());
        assert_eq!(
            worker.validate(),
            Err("cursor worker_url must use cursor.com")
        );
    }
}
