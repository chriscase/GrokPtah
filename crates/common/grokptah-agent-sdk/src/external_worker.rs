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
/// Maximum bytes a single external worker artifact may report.
///
/// The trusted adapter also refuses to download more than this when it hashes
/// an artifact itself. Stating the ceiling here bounds the metadata on the
/// path where a provider supplies its own digest and nothing is downloaded.
pub const MAX_EXTERNAL_WORKER_ARTIFACT_BYTES: u64 = 8 * 1024 * 1024;
/// Maximum artifacts accepted in one run listing.
pub const MAX_EXTERNAL_WORKER_ARTIFACTS: usize = 256;
/// The only content-digest algorithm this contract accepts.
pub const EXTERNAL_WORKER_DIGEST_PREFIX: &str = "sha256:";
/// Hex characters in a SHA-256 digest.
const EXTERNAL_WORKER_DIGEST_HEX_LEN: usize = 64;

/// v1 does not claim a sequenced provider event stream.
///
/// Adapters must set [`ExternalWorkerRunRecord::stream`] to
/// [`ExternalWorkerStreamState::Unsupported`] and `last_seq` to `None`.
/// Synthesizing `last_seq = 0` as continuity is a contract violation.
pub const EXTERNAL_WORKER_STREAMING_SUPPORTED: bool = false;

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

/// Whether a run projection claims a sequenced provider event stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalWorkerStreamState {
    /// Streaming is not implemented or not qualified. Poll GET state instead.
    /// `last_seq` must be `None`.
    Unsupported,
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
    /// Whether the provider may create a draft PR. Promotion/merge is never
    /// implied by this field and remains a separate approval action.
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
        if let Some(provider_id) = &self.provider_id {
            validate_identity(provider_id, "provider_id")?;
        }
        if let Some(model) = &self.model {
            validate_identity(model, "model")?;
        }
        if self.provider == ExternalWorkerProvider::Custom && self.provider_id.is_none() {
            return Err("custom workers require provider_id");
        }
        // Enforced here rather than in one adapter, so a second adapter cannot
        // reach a provider with this set. Promotion stays a separate,
        // explicitly approved action; the message matches the one the Cursor
        // adapter has always returned so durable ledger labels are unchanged.
        if self.auto_create_pr {
            return Err("pull-request creation requires a separate approval action");
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
        if self.worker_url.as_ref().is_some_and(|url| {
            url.trim().is_empty()
                || url.len() > MAX_EXTERNAL_WORKER_REF_BYTES
                || !url.starts_with("https://")
                || url
                    .chars()
                    .any(|character| matches!(character, '\n' | '\r' | '\0'))
        }) {
            return Err("worker_url must be a bounded https URL");
        }
        if self.created_at.trim().is_empty() || self.updated_at.trim().is_empty() {
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
    /// Explicit stream contract for this run. v1 Cursor is unsupported.
    pub stream: ExternalWorkerStreamState,
    /// Last provider event sequence when streaming is supported.
    /// Must be `None` when [`Self::stream`] is [`ExternalWorkerStreamState::Unsupported`].
    pub last_seq: Option<u64>,
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
        if self.stream == ExternalWorkerStreamState::Unsupported && self.last_seq.is_some() {
            return Err("unsupported streams must not synthesize a last_seq cursor");
        }
        if let Some(result) = &self.terminal_result {
            validate_detail(result, "terminal_result")?;
        }
        if self.created_at.trim().is_empty() || self.updated_at.trim().is_empty() {
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
        if self.ts.trim().is_empty() || self.kind.trim().is_empty() {
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
    /// Content digest supplied by the provider or a trusted download-and-hash.
    pub digest: String,
    /// Opaque provider run identity this artifact is attributed to.
    /// Serialized as `runId` to match the public JSON Schema and TypeScript parser.
    #[serde(rename = "runId")]
    pub external_run_id: String,
    /// Bounded artifact size, if reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

impl ExternalWorkerArtifact {
    /// Validate an artifact reference without allowing host paths.
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_artifact_path(&self.path)?;
        validate_digest(&self.digest)?;
        validate_identity(&self.external_run_id, "external_run_id")?;
        if self
            .size_bytes
            .is_some_and(|size| size > MAX_EXTERNAL_WORKER_ARTIFACT_BYTES)
        {
            return Err("artifact size exceeds its byte ceiling");
        }
        Ok(())
    }

    /// Validate this artifact against the run a listing was requested for.
    pub fn validate_for_run(&self, external_run_id: &str) -> Result<(), &'static str> {
        self.validate()?;
        if self.external_run_id != external_run_id {
            return Err("artifact is not attributed to the requested run");
        }
        Ok(())
    }
}

/// Validate a whole artifact listing against the run it was requested for.
///
/// Attribution is a property of the listing, not of one artifact, so it cannot
/// live in [`ExternalWorkerArtifact::validate`]. Keeping the rule here means a
/// second adapter cannot publish another provider's artifacts under this run,
/// and no caller has to size a collection from a provider-controlled count.
pub fn validate_artifact_listing(
    artifacts: &[ExternalWorkerArtifact],
    external_run_id: &str,
) -> Result<(), &'static str> {
    if artifacts.len() > MAX_EXTERNAL_WORKER_ARTIFACTS {
        return Err("artifact listing exceeds its item ceiling");
    }
    for artifact in artifacts {
        artifact.validate_for_run(external_run_id)?;
    }
    Ok(())
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
            _ => "worker identity must not be empty",
        });
    }
    if value.len() > MAX_EXTERNAL_WORKER_ID_BYTES {
        return Err("worker identity exceeds its byte bound");
    }
    if value
        .chars()
        .any(|character| matches!(character, '\n' | '\r' | '\0'))
    {
        return Err("worker identity contains a control character");
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
    if value
        .chars()
        .any(|character| matches!(character, '\n' | '\r' | '\0'))
    {
        return Err("worker ref contains a control character");
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

/// Artifact paths are stricter than refs: they name a file a consumer may
/// materialize under a containment root, so every form that can leave that
/// root or make two strings name one file is refused.
fn validate_artifact_path(value: &str) -> Result<(), &'static str> {
    const ERROR: &str = "artifact path must be bounded and relative";
    if value.trim().is_empty() || value.len() > MAX_EXTERNAL_WORKER_REF_BYTES {
        return Err(ERROR);
    }
    // NUL truncates the path for any C-API consumer and CR/LF forge a line in
    // anything that logs the listing.
    if value.chars().any(char::is_control) {
        return Err(ERROR);
    }
    // Absolute in every form a consumer might honour: POSIX and UNC roots, a
    // Windows drive (`C:/x` carries neither a leading slash nor a backslash),
    // and a tilde any shell-expanding consumer resolves to a home directory.
    let bytes = value.as_bytes();
    if value.starts_with('/')
        || value.starts_with('~')
        || value.contains('\\')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
    {
        return Err(ERROR);
    }
    // A query or fragment is not part of a path; accepting one lets a provider
    // smuggle a credential into a value that is presented as a file name.
    if value.contains('?') || value.contains('#') {
        return Err(ERROR);
    }
    // Empty and `.` segments make two spellings name one file, so a consumer
    // that normalizes and one that does not disagree about what was digested.
    // `..` is the traversal itself.
    if value
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(ERROR);
    }
    // One conservative portable grammar, applied everywhere.
    //
    // Refusing anything outside it is deliberate: a permissive path is a
    // portability bug that only shows up on someone else's filesystem. Unicode
    // normalization makes two different byte strings name one file on macOS,
    // case-insensitivity does the same on Windows and macOS, and a name that
    // is merely awkward on POSIX can be unopenable on Windows. A digest over
    // bytes cannot save a consumer that resolved the wrong file.
    for segment in value.split('/') {
        if !segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(ERROR);
        }
        // Windows silently strips a trailing dot or space, so `report.md.` and
        // `report.md` are the same file there and different ones on POSIX.
        if segment.ends_with('.') || segment.ends_with(' ') || segment.starts_with(' ') {
            return Err(ERROR);
        }
        if is_windows_reserved_name(segment) {
            return Err(ERROR);
        }
    }
    Ok(())
}

/// Windows reserved device names, which cannot be created as files even inside
/// a directory, and which match case-insensitively and ignoring an extension.
fn is_windows_reserved_name(segment: &str) -> bool {
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    // `NUL.txt` is as reserved as `NUL`, so compare the stem.
    let stem = segment.split('.').next().unwrap_or(segment);
    RESERVED
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
}

/// A digest is only a safety property if it names an algorithm and a full
/// value. The trusted adapter's own download-and-hash path emits exactly this
/// shape, so a provider-supplied digest is held to the same standard.
fn validate_digest(value: &str) -> Result<(), &'static str> {
    const ERROR: &str = "digest must be sha256:<64 lowercase hex>";
    let Some(hex) = value.strip_prefix(EXTERNAL_WORKER_DIGEST_PREFIX) else {
        return Err(ERROR);
    };
    if hex.len() != EXTERNAL_WORKER_DIGEST_HEX_LEN
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ERROR);
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
            Err("worker ref contains a control character")
        );
    }

    #[test]
    fn newline_in_repository_or_ref_fails_closed() {
        let mut request = launch();
        request.repository = "chriscase/GrokPtah\n".into();
        assert_eq!(
            request.validate(),
            Err("worker ref contains a control character")
        );
        request.repository = "chriscase/GrokPtah".into();
        request.starting_ref = "main\r".into();
        assert_eq!(
            request.validate(),
            Err("worker ref contains a control character")
        );
    }

    #[test]
    fn artifacts_are_relative_run_attributed_and_events_are_bounded() {
        let artifact = artifact();
        assert!(artifact.validate().is_ok());
        let value = serde_json::to_value(&artifact).expect("artifact serializes");
        assert_eq!(value["runId"], "run-1");
        assert!(value.get("externalRunId").is_none());
        let round_trip: ExternalWorkerArtifact =
            serde_json::from_value(value).expect("artifact deserializes runId");
        assert_eq!(round_trip.external_run_id, "run-1");
        let bad_path = ExternalWorkerArtifact {
            path: "../secret".into(),
            ..artifact.clone()
        };
        assert!(bad_path.validate().is_err());
        let missing_run = ExternalWorkerArtifact {
            external_run_id: String::new(),
            ..artifact
        };
        assert_eq!(
            missing_run.validate(),
            Err("external_run_id must not be empty")
        );
        let event = ExternalWorkerEvent {
            seq: 1,
            ts: "2026-08-24T00:00:00Z".into(),
            kind: "run.progress".into(),
            detail: "checking tests".into(),
        };
        assert!(event.validate().is_ok());
    }

    /// The one value every layer accepts. `true` was rejected only inside the
    /// Cursor adapter, so the contract type itself admitted a request that
    /// asks a provider to open a pull request.
    #[test]
    fn auto_create_pr_is_refused_by_the_contract_not_only_by_an_adapter() {
        let mut request = launch();
        assert!(request.validate().is_ok());
        request.auto_create_pr = true;
        assert_eq!(
            request.validate(),
            Err("pull-request creation requires a separate approval action")
        );
    }

    /// `null` is not a bounded boolean and must not decode. A missing field
    /// decodes to `false`, which is the only value the contract allows.
    #[test]
    fn auto_create_pr_rejects_null_and_defaults_missing_to_false() {
        let mut value = serde_json::to_value(launch()).expect("request serializes");
        value["autoCreatePr"] = serde_json::Value::Null;
        assert!(
            serde_json::from_value::<ExternalWorkerLaunchRequest>(value.clone()).is_err(),
            "a null autoCreatePr must not decode as a boolean"
        );

        value
            .as_object_mut()
            .expect("request is an object")
            .remove("autoCreatePr");
        let decoded: ExternalWorkerLaunchRequest =
            serde_json::from_value(value).expect("a missing autoCreatePr defaults");
        assert!(!decoded.auto_create_pr);
        assert!(decoded.validate().is_ok());

        // And a request that asks for a pull request never round-trips into a
        // valid one.
        let mut asking = launch();
        asking.auto_create_pr = true;
        let decoded: ExternalWorkerLaunchRequest =
            serde_json::from_value(serde_json::to_value(&asking).expect("serializes"))
                .expect("round-trips");
        assert!(decoded.validate().is_err());
    }

    const REAL_DIGEST: &str =
        "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    fn artifact() -> ExternalWorkerArtifact {
        ExternalWorkerArtifact {
            path: "artifacts/review.json".into(),
            digest: REAL_DIGEST.into(),
            external_run_id: "run-1".into(),
            size_bytes: Some(42),
        }
    }

    /// The adapter's own download-and-hash path always produces
    /// `sha256:<64 lowercase hex>`. A provider-supplied digest was held to a
    /// strictly weaker standard: any bounded non-control string passed, so an
    /// unverifiable label reached the durable ledger and the browser.
    #[test]
    fn digest_must_be_a_real_sha256_not_an_arbitrary_label() {
        assert!(artifact().validate().is_ok());
        for bogus in [
            "sha256:abc",
            "trust-me",
            "md5:9f86d081884c7d659a2feaa0c55ad015",
            "sha256:9F86D081884C7D659A2FEAA0C55AD015A3BF4F1B2B0B822CD15D6C15B0F00A08",
            "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a0",
            "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a088",
            "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00zzz",
            "sha512:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
        ] {
            let candidate = ExternalWorkerArtifact {
                digest: bogus.into(),
                ..artifact()
            };
            assert_eq!(
                candidate.validate(),
                Err("digest must be sha256:<64 lowercase hex>"),
                "digest {bogus:?} must not be accepted as a content digest",
            );
        }
    }

    /// `starts_with('/')` and `contains('\\')` do not describe every absolute
    /// path. A Windows drive-absolute path uses forward slashes and no `..`,
    /// so it passed a check whose whole purpose was refusing host paths.
    #[test]
    fn artifact_path_refuses_every_absolute_form() {
        for absolute in [
            "C:/Windows/System32/config",
            "c:/Users/secret/.ssh/id_ed25519",
            "Z:/",
            "/etc/passwd",
            "~/.ssh/id_ed25519",
            "~",
        ] {
            let candidate = ExternalWorkerArtifact {
                path: absolute.into(),
                ..artifact()
            };
            assert_eq!(
                candidate.validate(),
                Err("artifact path must be bounded and relative"),
                "absolute path {absolute:?} must not be accepted",
            );
        }
    }

    /// `validate_ref` refuses control characters; the artifact path check was
    /// written inline and forgot them. A NUL truncates the path for any C-API
    /// consumer, and CR/LF forge a line in anything that logs the listing.
    #[test]
    fn artifact_path_refuses_control_characters() {
        for hostile in [
            "artifacts/report\u{0}",
            "artifacts/report\nlog-forged-line",
            "artifacts/report\r\n",
            "artifacts/report\u{7f}",
        ] {
            let candidate = ExternalWorkerArtifact {
                path: hostile.into(),
                ..artifact()
            };
            assert_eq!(
                candidate.validate(),
                Err("artifact path must be bounded and relative"),
                "path {hostile:?} must not be accepted",
            );
        }
    }

    /// Empty and `.` segments make two different strings name one file, so a
    /// consumer that normalizes and a consumer that does not disagree about
    /// what was digested. The Cursor adapter already refused these; the
    /// contract every other adapter shares did not.
    #[test]
    fn artifact_path_refuses_ambiguous_and_cloaked_segments() {
        for ambiguous in [
            "artifacts//review.json",
            "artifacts/./review.json",
            "artifacts/review.json?sig=secret",
            "artifacts/review.json#fragment",
            "artifacts/",
        ] {
            let candidate = ExternalWorkerArtifact {
                path: ambiguous.into(),
                ..artifact()
            };
            assert_eq!(
                candidate.validate(),
                Err("artifact path must be bounded and relative"),
                "path {ambiguous:?} must not be accepted",
            );
        }
    }

    /// The bridge has an 8 MiB download ceiling, but it is only reached when
    /// the adapter downloads to hash. A provider that supplies its own digest
    /// skips that path entirely, so an unbounded `sizeBytes` was published
    /// with no ceiling anywhere.
    #[test]
    fn artifact_size_is_bounded_by_the_contract_not_only_by_a_download() {
        let at_ceiling = ExternalWorkerArtifact {
            size_bytes: Some(MAX_EXTERNAL_WORKER_ARTIFACT_BYTES),
            ..artifact()
        };
        assert!(at_ceiling.validate().is_ok());
        for oversized in [MAX_EXTERNAL_WORKER_ARTIFACT_BYTES + 1, u64::MAX] {
            let candidate = ExternalWorkerArtifact {
                size_bytes: Some(oversized),
                ..artifact()
            };
            assert_eq!(
                candidate.validate(),
                Err("artifact size exceeds its byte ceiling"),
                "size {oversized} must not be accepted",
            );
        }
    }

    /// One conservative grammar, so a path that resolves here resolves the
    /// same way on every consumer's filesystem.
    #[test]
    fn artifact_paths_follow_one_portable_ascii_grammar() {
        for portable in [
            "artifacts/report.md",
            "artifacts/a/b/c-1_2.json",
            "a",
            "artifacts/..hidden",
            "artifacts/UPPER.TXT",
        ] {
            let candidate = ExternalWorkerArtifact {
                path: portable.into(),
                ..artifact()
            };
            assert!(
                candidate.validate().is_ok(),
                "portable path {portable:?} must be accepted",
            );
        }
        for hostile in [
            // Unicode normalization makes these two name one file on macOS.
            "artifacts/café.md",
            "artifacts/cafe\u{301}.md",
            // A right-to-left override reorders how the name is displayed.
            "artifacts/report\u{202e}gnp.md",
            // A zero-width space is invisible next to a legitimate name.
            "artifacts/report\u{200b}.md",
            // Shell and Windows metacharacters.
            "artifacts/report;rm -rf.md",
            "artifacts/report*.md",
            "artifacts/report:stream",
            "artifacts/report|pipe",
            "artifacts/a b.md",
            // Windows strips these, collapsing two names into one.
            "artifacts/report.md.",
            "artifacts/report.md ",
            "artifacts/ report.md",
            // Reserved device names, with and without an extension.
            "artifacts/NUL",
            "artifacts/nul.txt",
            "artifacts/COM1",
            "artifacts/lpt9.log",
            "CON/report.md",
        ] {
            let candidate = ExternalWorkerArtifact {
                path: hostile.into(),
                ..artifact()
            };
            assert_eq!(
                candidate.validate(),
                Err("artifact path must be bounded and relative"),
                "path {hostile:?} must not be accepted",
            );
        }
    }

    /// The same corpus the TypeScript parser and the published schema are
    /// tested against, under a real JSON Schema validator, in
    /// `desktop/src/lib/externalWorkerConformance.test.ts`.
    ///
    /// Three hand-written implementations of one rule drift. Reading the cases
    /// from one file means a rule that changes here and nowhere else fails in
    /// two suites rather than surfacing in a consumer.
    #[test]
    fn the_shared_conformance_corpus_agrees_with_this_validator() {
        const CORPUS: &str = include_str!(
            "../../../../docs/schemas/grokptah-external-worker.v1.conformance.json"
        );
        let corpus: serde_json::Value =
            serde_json::from_str(CORPUS).expect("conformance corpus is valid JSON");
        assert_eq!(corpus["contract"], EXTERNAL_WORKER_CONTRACT_VERSION);
        let valid_digest = corpus["validDigest"]
            .as_str()
            .expect("corpus names a valid digest");

        let strings = |group: &str, verdict: &str| -> Vec<String> {
            corpus[group][verdict]
                .as_array()
                .unwrap_or_else(|| panic!("corpus has {group}.{verdict}"))
                .iter()
                .map(|item| item.as_str().expect("corpus entry is a string").to_string())
                .collect()
        };

        let mut checked = 0usize;
        for path in strings("artifactPath", "accept") {
            let candidate = ExternalWorkerArtifact {
                path: path.clone(),
                digest: valid_digest.into(),
                external_run_id: "run-1".into(),
                size_bytes: None,
            };
            assert!(
                candidate.validate().is_ok(),
                "corpus says accept, validator refused: {path:?}",
            );
            checked += 1;
        }
        for path in strings("artifactPath", "refuse") {
            let candidate = ExternalWorkerArtifact {
                path: path.clone(),
                digest: valid_digest.into(),
                external_run_id: "run-1".into(),
                size_bytes: None,
            };
            assert_eq!(
                candidate.validate(),
                Err("artifact path must be bounded and relative"),
                "corpus says refuse, validator accepted: {path:?}",
            );
            checked += 1;
        }
        for digest in strings("digest", "accept") {
            let candidate = ExternalWorkerArtifact {
                digest: digest.clone(),
                ..ExternalWorkerArtifact {
                    path: "artifacts/report.md".into(),
                    digest: valid_digest.into(),
                    external_run_id: "run-1".into(),
                    size_bytes: None,
                }
            };
            assert!(
                candidate.validate().is_ok(),
                "corpus says accept, validator refused digest {digest:?}",
            );
            checked += 1;
        }
        for digest in strings("digest", "refuse") {
            let candidate = ExternalWorkerArtifact {
                path: "artifacts/report.md".into(),
                digest: digest.clone(),
                external_run_id: "run-1".into(),
                size_bytes: None,
            };
            assert_eq!(
                candidate.validate(),
                Err("digest must be sha256:<64 lowercase hex>"),
                "corpus says refuse, validator accepted digest {digest:?}",
            );
            checked += 1;
        }
        for size in corpus["sizeBytes"]["accept"]
            .as_array()
            .expect("corpus has sizeBytes.accept")
        {
            let size = size.as_u64().expect("an accepted size is unsigned");
            let candidate = ExternalWorkerArtifact {
                path: "artifacts/report.md".into(),
                digest: valid_digest.into(),
                external_run_id: "run-1".into(),
                size_bytes: Some(size),
            };
            assert!(
                candidate.validate().is_ok(),
                "corpus says accept, validator refused size {size}",
            );
            checked += 1;
        }
        // A negative size cannot be spelled in `u64`; the contract makes that
        // case unrepresentable rather than merely invalid, so only the
        // over-ceiling case is exercised here.
        for size in corpus["sizeBytes"]["refuse"]
            .as_array()
            .expect("corpus has sizeBytes.refuse")
            .iter()
            .filter_map(serde_json::Value::as_u64)
        {
            let candidate = ExternalWorkerArtifact {
                path: "artifacts/report.md".into(),
                digest: valid_digest.into(),
                external_run_id: "run-1".into(),
                size_bytes: Some(size),
            };
            assert_eq!(
                candidate.validate(),
                Err("artifact size exceeds its byte ceiling"),
                "corpus says refuse, validator accepted size {size}",
            );
            checked += 1;
        }
        assert!(checked > 40, "the corpus must not silently shrink");
    }

    /// The published schema is what a non-Rust consumer implements against.
    /// Nothing checked that it still described this contract, so the two could
    /// drift silently. Pin the artifact bounds both sides must agree on.
    #[test]
    fn published_schema_states_the_same_artifact_bounds_as_this_contract() {
        const SCHEMA: &str =
            include_str!("../../../../docs/schemas/grokptah-external-worker.v1.schema.json");
        let schema: serde_json::Value =
            serde_json::from_str(SCHEMA).expect("published schema is valid JSON");
        let defs = &schema["$defs"];
        assert_eq!(
            defs["digest"]["pattern"],
            serde_json::json!("^sha256:[0-9a-f]{64}$"),
            "schema digest rule must match validate_digest",
        );
        assert_eq!(
            defs["artifact"]["properties"]["sizeBytes"]["maximum"],
            serde_json::json!(MAX_EXTERNAL_WORKER_ARTIFACT_BYTES),
        );
        assert_eq!(
            schema["properties"]["artifacts"]["maxItems"],
            serde_json::json!(MAX_EXTERNAL_WORKER_ARTIFACTS),
        );
        assert_eq!(
            defs["artifactPath"]["maxLength"],
            serde_json::json!(MAX_EXTERNAL_WORKER_REF_BYTES),
        );
        // The artifact must not fall back to the looser `ref` and `identity`
        // rules the rest of the contract uses.
        assert_eq!(
            defs["artifact"]["properties"]["path"]["$ref"],
            serde_json::json!("#/$defs/artifactPath"),
        );
        assert_eq!(
            defs["artifact"]["properties"]["digest"]["$ref"],
            serde_json::json!("#/$defs/digest"),
        );
        assert_eq!(
            schema["properties"]["contract"]["const"],
            serde_json::json!(EXTERNAL_WORKER_CONTRACT_VERSION),
        );
    }

    /// Per-artifact validation cannot see the run a listing was requested
    /// for, so attribution was enforced in one adapter and nowhere else.
    #[test]
    fn listings_are_run_attributed_and_bounded_in_count() {
        let listing = vec![artifact()];
        assert!(validate_artifact_listing(&listing, "run-1").is_ok());
        assert_eq!(
            validate_artifact_listing(&listing, "run-2"),
            Err("artifact is not attributed to the requested run"),
        );
        let oversized = vec![artifact(); MAX_EXTERNAL_WORKER_ARTIFACTS + 1];
        assert_eq!(
            validate_artifact_listing(&oversized, "run-1"),
            Err("artifact listing exceeds its item ceiling"),
        );
        let at_ceiling = vec![artifact(); MAX_EXTERNAL_WORKER_ARTIFACTS];
        assert!(validate_artifact_listing(&at_ceiling, "run-1").is_ok());
        // A single bad member fails the whole listing closed.
        let mut mixed = vec![artifact()];
        mixed.push(ExternalWorkerArtifact {
            digest: "sha256:abc".into(),
            ..artifact()
        });
        assert_eq!(
            validate_artifact_listing(&mixed, "run-1"),
            Err("digest must be sha256:<64 lowercase hex>"),
        );
    }

    #[test]
    fn unsupported_streams_must_not_synthesize_a_zero_cursor() {
        const { assert!(!EXTERNAL_WORKER_STREAMING_SUPPORTED) };
        let mut run = ExternalWorkerRunRecord {
            external_agent_id: "agent-1".into(),
            external_run_id: "run-1".into(),
            state: ExternalWorkerState::Running,
            stream: ExternalWorkerStreamState::Unsupported,
            last_seq: None,
            terminal_result: None,
            created_at: "2026-08-24T00:00:00Z".into(),
            updated_at: "2026-08-24T00:00:01Z".into(),
        };
        assert!(run.validate().is_ok());
        let value = serde_json::to_value(&run).expect("run serializes");
        assert_eq!(value["stream"], "unsupported");
        assert_eq!(value["lastSeq"], serde_json::Value::Null);
        run.last_seq = Some(0);
        assert_eq!(
            run.validate(),
            Err("unsupported streams must not synthesize a last_seq cursor")
        );
    }
}
