//! Provider-neutral contracts for external coding-agent workers.
//!
//! These types describe a worker that GrokPtah schedules outside the local
//! authority, such as a cloud coding agent. They intentionally contain no
//! credentials, network client, filesystem path, or execution policy. A
//! trusted adapter owns those concerns and maps provider responses into these
//! bounded projections.
//!
//! Public list/query/summary/page DTOs and [`MAX_EXTERNAL_WORKER_LIST_LIMIT`]
//! are also re-exported from the crate root so another repository can depend
//! on the documented SDK surface without reaching into this module path.

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
/// Maximum number of identity summaries returned in one list page.
pub const MAX_EXTERNAL_WORKER_LIST_LIMIT: u32 = 100;

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
        validate_timestamps(&self.created_at, &self.updated_at, "worker")
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
        validate_timestamps(&self.created_at, &self.updated_at, "run")
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

/// Bounded query for listing provider workers.
///
/// List pages are identity summaries only. Adapters must not invent repository
/// or starting-ref fields that the provider list endpoint does not return.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalWorkerListQuery {
    /// Page size. When omitted, adapters send the provider's documented default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Opaque pagination cursor from a previous page's `next_cursor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// When false, archived workers must not be included. Serializers emit this
    /// flag as an explicit boolean so consumers never see JSON null or inherit a
    /// provider default. Omitted inbound values still deserialize as false.
    #[serde(default)]
    pub include_archived: bool,
}

impl ExternalWorkerListQuery {
    /// Validate list query bounds before they reach a provider adapter.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self
            .limit
            .is_some_and(|limit| !(1..=MAX_EXTERNAL_WORKER_LIST_LIMIT).contains(&limit))
        {
            return Err("list limit must be between 1 and 100");
        }
        if let Some(cursor) = &self.cursor {
            validate_identity(cursor, "cursor")?;
        }
        Ok(())
    }
}

/// Identity-only worker summary projected from a provider list page.
///
/// This type deliberately omits repository, starting ref, and write/PR flags
/// because documented list items do not include them. Call `get_worker` for
/// the full safety-checked record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalWorkerSummary {
    /// Provider family that owns the opaque IDs.
    pub provider: ExternalWorkerProvider,
    /// Provider-specific adapter ID for custom providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// Opaque provider worker/agent identity.
    pub external_agent_id: String,
    /// Current projected lifecycle state.
    pub state: ExternalWorkerState,
    /// Provider URL, if share-safe and explicitly allowed by the adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_url: Option<String>,
    /// Opaque latest run identity, if the provider reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_run_id: Option<String>,
    /// RFC3339 creation timestamp.
    pub created_at: String,
    /// RFC3339 last-update timestamp.
    pub updated_at: String,
}

impl ExternalWorkerSummary {
    /// Validate a redacted list summary before publishing it.
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_identity(&self.external_agent_id, "external_agent_id")?;
        if let Some(provider_id) = &self.provider_id {
            validate_identity(provider_id, "provider_id")?;
        }
        if self.provider == ExternalWorkerProvider::Custom && self.provider_id.is_none() {
            return Err("custom workers require provider_id");
        }
        if let Some(url) = &self.worker_url {
            validate_worker_url(url, self.provider)?;
        }
        if let Some(latest_run_id) = &self.latest_run_id {
            validate_identity(latest_run_id, "external_run_id")?;
        }
        validate_timestamps(&self.created_at, &self.updated_at, "worker")
    }
}

/// One page of redacted worker identity summaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalWorkerListPage {
    /// Identity summaries for this page, newest first.
    pub items: Vec<ExternalWorkerSummary>,
    /// Opaque cursor for the next page. Omitted when no further page exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

impl ExternalWorkerListPage {
    /// Validate a list page before it crosses a broker boundary.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.items.len() > MAX_EXTERNAL_WORKER_LIST_LIMIT as usize {
            return Err("list page exceeds its item bound");
        }
        if self.items.is_empty() && self.next_cursor.is_some() {
            return Err("list cursor must not be published for an empty page");
        }
        if let Some(cursor) = &self.next_cursor {
            validate_identity(cursor, "cursor")?;
        }
        let mut seen = std::collections::BTreeSet::new();
        for item in &self.items {
            item.validate()?;
            if !seen.insert(&item.external_agent_id) {
                return Err("list page contains duplicate worker identities");
            }
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

fn validate_timestamps(
    created_at: &str,
    updated_at: &str,
    field: &str,
) -> Result<(), &'static str> {
    if created_at.trim().is_empty()
        || updated_at.trim().is_empty()
        || contains_control(created_at)
        || contains_control(updated_at)
    {
        return Err(match field {
            "run" => "run timestamps must not be empty",
            _ => "worker timestamps must not be empty",
        });
    }
    Ok(())
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

    fn summary() -> ExternalWorkerSummary {
        ExternalWorkerSummary {
            provider: ExternalWorkerProvider::CursorCloud,
            provider_id: None,
            external_agent_id: "bc-00000000-0000-0000-0000-000000000001".into(),
            state: ExternalWorkerState::Ready,
            worker_url: Some(
                "https://cursor.com/agents/bc-00000000-0000-0000-0000-000000000001".into(),
            ),
            latest_run_id: Some("run-00000000-0000-0000-0000-000000000001".into()),
            created_at: "2026-08-24T00:00:00Z".into(),
            updated_at: "2026-08-24T00:00:01Z".into(),
        }
    }

    #[test]
    fn list_query_is_bounded_and_denies_unknown_fields() {
        let query: ExternalWorkerListQuery =
            serde_json::from_str(r#"{"limit":20,"includeArchived":true}"#)
                .expect("bounded list query deserializes");
        query.validate().expect("list query is valid");
        assert_eq!(query.limit, Some(20));
        assert!(query.include_archived);
        assert!(
            serde_json::from_str::<ExternalWorkerListQuery>(
                r#"{"prUrl":"https://github.com/org/repo/pull/1"}"#
            )
            .is_err()
        );
        let invalid_limit = ExternalWorkerListQuery {
            limit: Some(0),
            ..ExternalWorkerListQuery::default()
        };
        assert_eq!(
            invalid_limit.validate(),
            Err("list limit must be between 1 and 100")
        );
        let oversized = ExternalWorkerListQuery {
            limit: Some(MAX_EXTERNAL_WORKER_LIST_LIMIT + 1),
            ..ExternalWorkerListQuery::default()
        };
        assert_eq!(
            oversized.validate(),
            Err("list limit must be between 1 and 100")
        );
        let control_cursor = ExternalWorkerListQuery {
            cursor: Some("page\n2".into()),
            ..ExternalWorkerListQuery::default()
        };
        assert_eq!(
            control_cursor.validate(),
            Err("worker identity contains a control character")
        );
        let omitted: ExternalWorkerListQuery =
            serde_json::from_str("{}").expect("empty list query deserializes");
        omitted
            .validate()
            .expect("omitted includeArchived is false");
        assert!(!omitted.include_archived);
        assert_eq!(omitted.limit, None);
        let serialized_default =
            serde_json::to_value(&omitted).expect("default list query serializes");
        assert_eq!(serialized_default["includeArchived"], false);
        assert!(
            serialized_default["includeArchived"].is_boolean(),
            "includeArchived must serialize as a boolean, not JSON null"
        );
        assert!(
            serde_json::from_value::<ExternalWorkerListQuery>(serde_json::json!({
                "includeArchived": null
            }))
            .is_err(),
            "includeArchived must fail closed on JSON null"
        );
    }

    #[test]
    fn list_summaries_are_identity_only_and_redacted() {
        let item = summary();
        item.validate().expect("list summary is valid");
        let value = serde_json::to_value(&item).expect("summary serializes");
        assert_eq!(value["provider"], "cursor_cloud");
        assert!(value.get("repository").is_none());
        assert!(value.get("startingRef").is_none());
        assert!(
            serde_json::from_value::<ExternalWorkerSummary>(serde_json::json!({
                "provider": "cursor_cloud",
                "externalAgentId": "agent-1",
                "repository": "org/repo",
                "startingRef": "main",
                "state": "ready",
                "createdAt": "now",
                "updatedAt": "now"
            }))
            .is_err()
        );
        let mut leaked = item.clone();
        leaked.worker_url = Some("https://cursor.com/agents/agent-1?token=secret".into());
        assert_eq!(
            leaked.validate(),
            Err("worker_url must be a bounded credential-free https URL")
        );
        leaked.worker_url = item.worker_url.clone();
        leaked.latest_run_id = Some("run\0secret".into());
        assert_eq!(
            leaked.validate(),
            Err("worker identity contains a control character")
        );
    }

    #[test]
    fn list_pages_fail_closed_on_duplicates_empty_cursors_and_unknown_fields() {
        let page = ExternalWorkerListPage {
            items: vec![summary()],
            next_cursor: Some("bc-00000000-0000-0000-0000-000000000002".into()),
        };
        page.validate().expect("list page is valid");
        let empty_with_cursor = ExternalWorkerListPage {
            items: Vec::new(),
            next_cursor: Some("bc-00000000-0000-0000-0000-000000000002".into()),
        };
        assert_eq!(
            empty_with_cursor.validate(),
            Err("list cursor must not be published for an empty page")
        );
        let mut duplicate = summary();
        duplicate.updated_at = "2026-08-24T00:00:02Z".into();
        let duplicates = ExternalWorkerListPage {
            items: vec![summary(), duplicate],
            next_cursor: None,
        };
        assert_eq!(
            duplicates.validate(),
            Err("list page contains duplicate worker identities")
        );
        assert!(
            serde_json::from_value::<ExternalWorkerListPage>(serde_json::json!({
                "items": [],
                "rawProvider": {"authorization": "Bearer secret"}
            }))
            .is_err()
        );
        let mut oversized_items = Vec::new();
        for index in 0..=MAX_EXTERNAL_WORKER_LIST_LIMIT {
            oversized_items.push(ExternalWorkerSummary {
                external_agent_id: format!("agent-{index}"),
                latest_run_id: None,
                worker_url: None,
                ..summary()
            });
        }
        let oversized = ExternalWorkerListPage {
            items: oversized_items,
            next_cursor: None,
        };
        assert_eq!(
            oversized.validate(),
            Err("list page exceeds its item bound")
        );
    }

    #[test]
    fn archived_is_a_distinct_state_from_cancelled_or_completed() {
        assert_ne!(
            ExternalWorkerState::Archived,
            ExternalWorkerState::Cancelled
        );
        assert_ne!(
            ExternalWorkerState::Archived,
            ExternalWorkerState::Completed
        );
        let archived = ExternalWorkerSummary {
            state: ExternalWorkerState::Archived,
            ..summary()
        };
        archived.validate().expect("archived summary is valid");
    }
}
