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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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
        if let Some(url) = &self.worker_url
            && (url.trim().is_empty()
                || url.len() > MAX_EXTERNAL_WORKER_REF_BYTES
                || !url.starts_with("https://")
                || url
                    .chars()
                    .any(|character| matches!(character, '\n' | '\r' | '\0')))
        {
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
    if value.len() > MAX_EXTERNAL_WORKER_REF_BYTES
        || value.starts_with('/')
        || value
            .chars()
            .any(|character| matches!(character, '\\' | '\n' | '\r' | '\0'))
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
    Ok(())
}

// ---------------------------------------------------------------------------
// Production authority: host-minted admission, receipts, and capability truth.
//
// External-worker mutations create state on a third-party system that GrokPtah
// cannot roll back. Everything below exists so that a mutation can only leave
// this host when the authority itself minted a ticket that names the exact
// principal, session, workspace, run, mutation, provider, capability revision,
// payload, and lifetime. These are contracts only: minting, revalidation, and
// durability live in the trusted host, which is the sole authority.
// ---------------------------------------------------------------------------

/// Maximum UTF-8 bytes accepted for a redacted receipt or admission reason.
pub const MAX_EXTERNAL_WORKER_REASON_BYTES: usize = 512;
/// Maximum lifetime a host may mint into one external-worker admission.
pub const MAX_EXTERNAL_WORKER_ADMISSION_TTL_MS: u64 = 15 * 60 * 1_000;
/// Byte length of the hexadecimal body of a `sha256:` digest.
const SHA256_HEX_BYTES: usize = 64;

/// The exact identity fence an external-worker mutation is bound to.
///
/// `workspace` is a host-chosen alias, never a filesystem path: a public
/// projection must not disclose where a workspace lives on the host.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalWorkerScope {
    /// Authenticated principal that owns the mutation intent.
    pub principal_id: String,
    /// Authenticated GrokPtah session identity.
    pub session_id: String,
    /// Approved workspace alias selected by the authority.
    pub workspace: String,
    /// Durable GrokPtah run identity the external work belongs to.
    pub run_id: String,
}

impl ExternalWorkerScope {
    /// Validate the identity fence before it crosses a product boundary.
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_identity(&self.principal_id, "principal_id")?;
        validate_identity(&self.session_id, "session_id")?;
        validate_identity(&self.run_id, "run_id")?;
        validate_identity(&self.workspace, "workspace")?;
        if is_host_path(&self.workspace) {
            return Err("workspace must be an alias, not a host path");
        }
        Ok(())
    }
}

/// The single mutation an admission authorizes.
///
/// One admission never covers two kinds of mutation. A follow-up ticket cannot
/// be spent on a cancel, and a launch ticket cannot be spent on a follow-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalWorkerMutation {
    /// Create an isolated worker and its initial run.
    Launch,
    /// Queue one bounded follow-up run on an existing worker.
    FollowUp,
    /// Cancel one active provider run.
    Cancel,
}

impl ExternalWorkerMutation {
    /// Stable wire label used in receipts and derived provider identities.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Launch => "launch",
            Self::FollowUp => "follow_up",
            Self::Cancel => "cancel",
        }
    }

    /// Whether this mutation must name an existing provider worker.
    pub fn requires_worker_target(self) -> bool {
        matches!(self, Self::FollowUp | Self::Cancel)
    }

    /// Whether this mutation must name an existing provider run.
    pub fn requires_run_target(self) -> bool {
        matches!(self, Self::Cancel)
    }
}

/// The opaque provider object an admission is spent against.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalWorkerTarget {
    /// Opaque provider worker/agent identity.
    pub external_agent_id: String,
    /// Opaque provider run identity, when the mutation names one run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_run_id: Option<String>,
}

impl ExternalWorkerTarget {
    /// Validate the opaque provider identities carried by an admission.
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_identity(&self.external_agent_id, "external_agent_id")?;
        if let Some(run_id) = &self.external_run_id {
            validate_identity(run_id, "external_run_id")?;
        }
        Ok(())
    }
}

/// A host-minted, scope-bound ticket that admits exactly one mutation.
///
/// The public projection is deliberately opaque: it carries identities,
/// bounds, and a payload digest, never a prompt, credential, provider URL, or
/// host path. It is not a bearer credential either — a host revalidates every
/// field against its own durable mint ledger, so an admission that this host
/// did not mint fails closed even when it is perfectly well formed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalWorkerAdmission {
    /// Contract identifier; must equal [`EXTERNAL_WORKER_CONTRACT_VERSION`].
    pub contract: String,
    /// Host-minted admission identity, used for correlation in receipts.
    pub admission_id: String,
    /// Host-minted single-use nonce; the durable ledger key for this ticket.
    pub nonce: String,
    /// Caller idempotency key this admission was minted for.
    pub request_id: String,
    /// Exact principal/session/workspace/run fence.
    pub scope: ExternalWorkerScope,
    /// The one mutation this admission authorizes.
    pub mutation: ExternalWorkerMutation,
    /// Provider family this admission authorizes.
    pub provider: ExternalWorkerProvider,
    /// Adapter identity for custom providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// Capability-registry revision observed when the ticket was minted.
    pub capability_revision: u64,
    /// Mint time in milliseconds since the Unix epoch.
    pub issued_at_ms: u64,
    /// Expiry in milliseconds since the Unix epoch.
    pub expires_at_ms: u64,
    /// `sha256:<hex>` digest of the exact bounded payload being admitted.
    pub payload_digest: String,
    /// Opaque provider target for follow-up and cancel mutations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ExternalWorkerTarget>,
}

impl ExternalWorkerAdmission {
    /// Validate the shape and internal consistency of an admission.
    ///
    /// A structurally valid admission is still not an authorization: the host
    /// must additionally match it against its durable mint ledger.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.contract != EXTERNAL_WORKER_CONTRACT_VERSION {
            return Err("admission contract version is not supported");
        }
        validate_identity(&self.admission_id, "admission_id")?;
        validate_identity(&self.nonce, "nonce")?;
        validate_identity(&self.request_id, "request_id")?;
        self.scope.validate()?;
        if let Some(provider_id) = &self.provider_id {
            validate_identity(provider_id, "provider_id")?;
        }
        if self.provider == ExternalWorkerProvider::Custom && self.provider_id.is_none() {
            return Err("custom workers require provider_id");
        }
        validate_digest(&self.payload_digest)?;
        if self.expires_at_ms <= self.issued_at_ms {
            return Err("admission must have a positive lifetime");
        }
        if self.expires_at_ms - self.issued_at_ms > MAX_EXTERNAL_WORKER_ADMISSION_TTL_MS {
            return Err("admission lifetime exceeds the host ceiling");
        }
        match &self.target {
            Some(target) => {
                if !self.mutation.requires_worker_target() {
                    return Err("launch admissions must not name a provider target");
                }
                target.validate()?;
                if self.mutation.requires_run_target() && target.external_run_id.is_none() {
                    return Err("cancel admissions must name an exact provider run");
                }
                if !self.mutation.requires_run_target() && target.external_run_id.is_some() {
                    return Err("follow-up admissions must not name a provider run");
                }
            }
            None => {
                if self.mutation.requires_worker_target() {
                    return Err("admission is missing its provider target");
                }
            }
        }
        Ok(())
    }

    /// Whether the admission is still inside its minted lifetime.
    ///
    /// Expiry is inclusive of the boundary: an admission is spent strictly
    /// before `expires_at_ms`, so a clock that lands exactly on the boundary
    /// fails closed rather than admitting one last mutation.
    pub fn is_live_at(&self, now_ms: u64) -> bool {
        now_ms >= self.issued_at_ms && now_ms < self.expires_at_ms
    }
}

/// Terminal disposition of one durable external-worker mutation receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalWorkerReceiptState {
    /// The authority admitted the mutation but has not sent it yet.
    Claimed,
    /// The provider accepted the mutation and returned a verified projection.
    Accepted,
    /// The mutation was refused before or by the provider, with no effect.
    Rejected,
    /// The outcome is unknown after the request left this host.
    ///
    /// Uncertain is sticky. It blocks automatic *and* explicit retry until an
    /// operator or a reconciliation read proves what the provider did.
    Uncertain,
}

impl ExternalWorkerReceiptState {
    /// Whether a receipt in this state permanently records provider effect.
    pub fn is_accepted(self) -> bool {
        matches!(self, Self::Accepted)
    }

    /// Whether this state blocks a further attempt on the same request.
    pub fn blocks_retry(self) -> bool {
        matches!(self, Self::Claimed | Self::Uncertain | Self::Accepted)
    }
}

/// Redacted durable record of one external-worker mutation attempt.
///
/// A receipt is the share-safe evidence that a mutation happened. It holds
/// opaque identities and a payload digest so a duplicate can be recognized
/// without retaining the prompt, provider payload, or any credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalWorkerReceipt {
    /// Contract identifier; must equal [`EXTERNAL_WORKER_CONTRACT_VERSION`].
    pub contract: String,
    /// Caller idempotency key that owns this receipt.
    pub request_id: String,
    /// Admission that authorized the mutation.
    pub admission_id: String,
    /// Mutation kind recorded by this receipt.
    pub mutation: ExternalWorkerMutation,
    /// Exact identity fence the mutation was admitted under.
    pub scope: ExternalWorkerScope,
    /// Provider family that received the mutation.
    pub provider: ExternalWorkerProvider,
    /// Adapter identity for custom providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// Stable provider-facing request identity, constant across attempts.
    pub provider_request_id: String,
    /// One-based attempt counter for this stable provider request.
    pub attempt: u32,
    /// Current durable disposition.
    pub state: ExternalWorkerReceiptState,
    /// Opaque provider target once the provider has named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ExternalWorkerTarget>,
    /// `sha256:<hex>` digest of the exact admitted payload.
    pub payload_digest: String,
    /// Bounded redacted reason; never provider text, paths, or credentials.
    pub reason: String,
    /// Receipt creation time in milliseconds since the Unix epoch.
    pub created_at_ms: u64,
    /// Last transition time in milliseconds since the Unix epoch.
    pub updated_at_ms: u64,
}

impl ExternalWorkerReceipt {
    /// Validate a receipt before publishing it through a typed projection.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.contract != EXTERNAL_WORKER_CONTRACT_VERSION {
            return Err("receipt contract version is not supported");
        }
        validate_identity(&self.request_id, "request_id")?;
        validate_identity(&self.admission_id, "admission_id")?;
        validate_identity(&self.provider_request_id, "provider_request_id")?;
        self.scope.validate()?;
        if let Some(provider_id) = &self.provider_id {
            validate_identity(provider_id, "provider_id")?;
        }
        if self.provider == ExternalWorkerProvider::Custom && self.provider_id.is_none() {
            return Err("custom workers require provider_id");
        }
        validate_digest(&self.payload_digest)?;
        if self.attempt == 0 {
            return Err("receipt attempt must be one-based");
        }
        if let Some(target) = &self.target {
            target.validate()?;
        }
        if self.state.is_accepted() && self.target.is_none() {
            return Err("an accepted receipt must record its provider target");
        }
        if self.updated_at_ms < self.created_at_ms {
            return Err("receipt timestamps must not move backwards");
        }
        validate_reason(&self.reason)
    }
}

/// The four independent facts that must all hold before a host may advertise
/// an external-worker capability.
///
/// Advertising is a claim of production authority, so it is derived from
/// observed truth rather than from configuration intent. Any single false
/// value makes the capability unavailable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalWorkerCapabilityStatus {
    /// Provider family this status describes.
    pub provider: ExternalWorkerProvider,
    /// Adapter identity for custom providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// A qualified adapter is installed in this process.
    pub registered: bool,
    /// The adapter answered a bounded reachability probe.
    pub reachable: bool,
    /// The adapter implements this contract version.
    pub version_compatible: bool,
    /// Host policy allows mutations through this adapter.
    pub policy_allowed: bool,
    /// Capability-registry revision this status was computed at.
    pub capability_revision: u64,
    /// Bounded redacted reason when the capability is not advertised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ExternalWorkerCapabilityStatus {
    /// Whether every gate is satisfied and mutations may be advertised.
    pub fn is_available(&self) -> bool {
        self.registered && self.reachable && self.version_compatible && self.policy_allowed
    }

    /// Validate the projected status before advertising it.
    pub fn validate(&self) -> Result<(), &'static str> {
        if let Some(provider_id) = &self.provider_id {
            validate_identity(provider_id, "provider_id")?;
        }
        if self.provider == ExternalWorkerProvider::Custom && self.provider_id.is_none() {
            return Err("custom workers require provider_id");
        }
        if let Some(reason) = &self.reason {
            validate_reason(reason)?;
        }
        if !self.is_available() && self.reason.is_none() {
            return Err("an unavailable capability must state a redacted reason");
        }
        Ok(())
    }
}

/// Validate a `sha256:<64 hex>` digest without computing one.
///
/// The public contract crate deliberately owns no hashing implementation; the
/// trusted host computes digests and this checks only that a digest is the
/// exact shape the contract admits.
pub fn validate_digest(value: &str) -> Result<(), &'static str> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err("digest must use the sha256 prefix");
    };
    if hex.len() != SHA256_HEX_BYTES
        || !hex
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err("digest must be lowercase 64-character hexadecimal");
    }
    Ok(())
}

fn validate_reason(value: &str) -> Result<(), &'static str> {
    if value.trim().is_empty() {
        return Err("redacted reason must not be empty");
    }
    if value.len() > MAX_EXTERNAL_WORKER_REASON_BYTES {
        return Err("redacted reason exceeds its byte bound");
    }
    if value
        .chars()
        .any(|character| matches!(character, '\n' | '\r' | '\0'))
    {
        return Err("redacted reason contains a control character");
    }
    if contains_privileged_needle(value) {
        return Err("redacted reason contains a privileged needle");
    }
    Ok(())
}

/// Whether a string looks like a host path a public projection must not carry.
fn is_host_path(value: &str) -> bool {
    value.starts_with('/')
        || value.starts_with("\\\\")
        || value.contains('\\')
        || value.split('/').any(|segment| segment == "..")
        || value
            .get(1..3)
            .is_some_and(|drive| drive == ":\\" || drive == ":/")
}

/// Whether a string carries a credential, URL, or host-path needle.
///
/// This is a fail-closed shape check, not a redaction engine: a caller that
/// trips it must fix the projection rather than sanitize the text in place.
pub fn contains_privileged_needle(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    const NEEDLES: [&str; 14] = [
        "http://",
        "https://",
        "authorization",
        "bearer ",
        "api_key",
        "api-key",
        "apikey",
        "password",
        "cookie",
        "private key",
        "secret",
        "session_token",
        "/users/",
        "\\users\\",
    ];
    NEEDLES.iter().any(|needle| lower.contains(needle))
        || is_host_path(value)
        || value.starts_with("/private/")
        || value.starts_with("/tmp/")
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

    const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn scope() -> ExternalWorkerScope {
        ExternalWorkerScope {
            principal_id: "principal-1".into(),
            session_id: "session-1".into(),
            workspace: "grokptah-main".into(),
            run_id: "run-1".into(),
        }
    }

    fn admission(mutation: ExternalWorkerMutation) -> ExternalWorkerAdmission {
        ExternalWorkerAdmission {
            contract: EXTERNAL_WORKER_CONTRACT_VERSION.into(),
            admission_id: "adm-1".into(),
            nonce: "nonce-1".into(),
            request_id: "req-1".into(),
            scope: scope(),
            mutation,
            provider: ExternalWorkerProvider::CursorCloud,
            provider_id: None,
            capability_revision: 7,
            issued_at_ms: 1_000,
            expires_at_ms: 61_000,
            payload_digest: DIGEST.into(),
            target: match mutation {
                ExternalWorkerMutation::Launch => None,
                ExternalWorkerMutation::FollowUp => Some(ExternalWorkerTarget {
                    external_agent_id: "bc-agent".into(),
                    external_run_id: None,
                }),
                ExternalWorkerMutation::Cancel => Some(ExternalWorkerTarget {
                    external_agent_id: "bc-agent".into(),
                    external_run_id: Some("run-a".into()),
                }),
            },
        }
    }

    #[test]
    fn admission_binds_exact_scope_and_serializes_without_private_payload() {
        let admission = admission(ExternalWorkerMutation::Launch);
        admission.validate().expect("launch admission is valid");
        let value = serde_json::to_value(&admission).expect("admission serializes");
        assert_eq!(value["contract"], EXTERNAL_WORKER_CONTRACT_VERSION);
        assert_eq!(value["mutation"], "launch");
        assert_eq!(value["scope"]["principalId"], "principal-1");
        assert_eq!(value["capabilityRevision"], 7);
        assert!(value.get("target").is_none());
        assert!(value.get("prompt").is_none());
        assert!(value.get("apiKey").is_none());
        let decoded: ExternalWorkerAdmission =
            serde_json::from_value(value).expect("admission round-trips");
        assert_eq!(decoded, admission);
    }

    #[test]
    fn admission_rejects_foreign_contract_and_unbounded_lifetime() {
        let mut admission = admission(ExternalWorkerMutation::Launch);
        admission.contract = "grokptah.external-workers.v2".into();
        assert_eq!(
            admission.validate(),
            Err("admission contract version is not supported")
        );
        admission.contract = EXTERNAL_WORKER_CONTRACT_VERSION.into();
        admission.expires_at_ms = admission.issued_at_ms;
        assert_eq!(
            admission.validate(),
            Err("admission must have a positive lifetime")
        );
        admission.expires_at_ms = admission.issued_at_ms + MAX_EXTERNAL_WORKER_ADMISSION_TTL_MS + 1;
        assert_eq!(
            admission.validate(),
            Err("admission lifetime exceeds the host ceiling")
        );
    }

    #[test]
    fn admission_expiry_boundary_fails_closed() {
        let admission = admission(ExternalWorkerMutation::Launch);
        assert!(!admission.is_live_at(999));
        assert!(admission.is_live_at(1_000));
        assert!(admission.is_live_at(60_999));
        assert!(!admission.is_live_at(61_000));
        assert!(!admission.is_live_at(u64::MAX));
    }

    #[test]
    fn admission_target_shape_matches_its_mutation() {
        let mut launch = admission(ExternalWorkerMutation::Launch);
        launch.target = Some(ExternalWorkerTarget {
            external_agent_id: "bc-agent".into(),
            external_run_id: None,
        });
        assert_eq!(
            launch.validate(),
            Err("launch admissions must not name a provider target")
        );

        let mut follow_up = admission(ExternalWorkerMutation::FollowUp);
        follow_up.target = None;
        assert_eq!(
            follow_up.validate(),
            Err("admission is missing its provider target")
        );
        follow_up.target = Some(ExternalWorkerTarget {
            external_agent_id: "bc-agent".into(),
            external_run_id: Some("run-a".into()),
        });
        assert_eq!(
            follow_up.validate(),
            Err("follow-up admissions must not name a provider run")
        );

        let mut cancel = admission(ExternalWorkerMutation::Cancel);
        cancel.target = Some(ExternalWorkerTarget {
            external_agent_id: "bc-agent".into(),
            external_run_id: None,
        });
        assert_eq!(
            cancel.validate(),
            Err("cancel admissions must name an exact provider run")
        );
        assert!(admission(ExternalWorkerMutation::Cancel).validate().is_ok());
        assert!(
            admission(ExternalWorkerMutation::FollowUp)
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn scope_refuses_host_paths_as_workspace_aliases() {
        for workspace in [
            "/Users/dev/GrokPtah",
            "C:\\Users\\dev\\GrokPtah",
            "\\\\share\\GrokPtah",
            "../escape",
        ] {
            let candidate = ExternalWorkerScope {
                workspace: workspace.into(),
                ..scope()
            };
            assert_eq!(
                candidate.validate(),
                Err("workspace must be an alias, not a host path"),
                "workspace {workspace} must fail closed"
            );
        }
        assert!(scope().validate().is_ok());
    }

    #[test]
    fn digest_shape_is_exact_and_lowercase() {
        assert!(validate_digest(DIGEST).is_ok());
        assert_eq!(
            validate_digest("sha1:0123"),
            Err("digest must use the sha256 prefix")
        );
        assert_eq!(
            validate_digest(&DIGEST.to_ascii_uppercase().replace("SHA256:", "sha256:")),
            Err("digest must be lowercase 64-character hexadecimal")
        );
        assert_eq!(
            validate_digest("sha256:abc"),
            Err("digest must be lowercase 64-character hexadecimal")
        );
    }

    fn receipt(state: ExternalWorkerReceiptState) -> ExternalWorkerReceipt {
        ExternalWorkerReceipt {
            contract: EXTERNAL_WORKER_CONTRACT_VERSION.into(),
            request_id: "req-1".into(),
            admission_id: "adm-1".into(),
            mutation: ExternalWorkerMutation::Launch,
            scope: scope(),
            provider: ExternalWorkerProvider::CursorCloud,
            provider_id: None,
            provider_request_id: "pr-abc".into(),
            attempt: 1,
            state,
            target: Some(ExternalWorkerTarget {
                external_agent_id: "bc-agent".into(),
                external_run_id: Some("run-a".into()),
            }),
            payload_digest: DIGEST.into(),
            reason: "provider accepted the admitted launch".into(),
            created_at_ms: 1_000,
            updated_at_ms: 2_000,
        }
    }

    #[test]
    fn receipt_projection_is_redacted_and_round_trippable() {
        let receipt = receipt(ExternalWorkerReceiptState::Accepted);
        receipt.validate().expect("accepted receipt is valid");
        let value = serde_json::to_value(&receipt).expect("receipt serializes");
        assert_eq!(value["state"], "accepted");
        assert_eq!(value["mutation"], "launch");
        assert_eq!(value["providerRequestId"], "pr-abc");
        assert!(value.get("prompt").is_none());
        assert!(value.get("workerUrl").is_none());
        let decoded: ExternalWorkerReceipt =
            serde_json::from_value(value).expect("receipt round-trips");
        assert_eq!(decoded, receipt);
    }

    #[test]
    fn receipt_reason_rejects_privileged_needles_and_control_characters() {
        for reason in [
            "provider replied with Authorization: Bearer abc",
            "see https://api.cursor.com/v1/agents",
            "wrote /Users/dev/GrokPtah/out.json",
            "api_key rotated",
            "line one\nline two",
        ] {
            let candidate = ExternalWorkerReceipt {
                reason: reason.into(),
                ..receipt(ExternalWorkerReceiptState::Rejected)
            };
            assert!(
                candidate.validate().is_err(),
                "reason {reason:?} must fail closed"
            );
        }
    }

    #[test]
    fn accepted_receipt_requires_a_provider_target_and_positive_attempt() {
        let mut candidate = receipt(ExternalWorkerReceiptState::Accepted);
        candidate.target = None;
        assert_eq!(
            candidate.validate(),
            Err("an accepted receipt must record its provider target")
        );
        candidate.target = receipt(ExternalWorkerReceiptState::Accepted).target;
        candidate.attempt = 0;
        assert_eq!(
            candidate.validate(),
            Err("receipt attempt must be one-based")
        );
        candidate.attempt = 2;
        candidate.updated_at_ms = candidate.created_at_ms - 1;
        assert_eq!(
            candidate.validate(),
            Err("receipt timestamps must not move backwards")
        );
    }

    #[test]
    fn receipt_states_block_retry_except_when_rejected() {
        assert!(ExternalWorkerReceiptState::Claimed.blocks_retry());
        assert!(ExternalWorkerReceiptState::Uncertain.blocks_retry());
        assert!(ExternalWorkerReceiptState::Accepted.blocks_retry());
        assert!(!ExternalWorkerReceiptState::Rejected.blocks_retry());
        assert!(ExternalWorkerReceiptState::Accepted.is_accepted());
        assert!(!ExternalWorkerReceiptState::Uncertain.is_accepted());
    }

    #[test]
    fn capability_is_advertised_only_when_every_gate_holds() {
        let base = ExternalWorkerCapabilityStatus {
            provider: ExternalWorkerProvider::CursorCloud,
            provider_id: None,
            registered: true,
            reachable: true,
            version_compatible: true,
            policy_allowed: true,
            capability_revision: 3,
            reason: None,
        };
        assert!(base.is_available());
        base.validate().expect("available status is valid");

        for (label, mutate) in [
            ("registered", 0usize),
            ("reachable", 1),
            ("version", 2),
            ("policy", 3),
        ] {
            let mut candidate = base.clone();
            match mutate {
                0 => candidate.registered = false,
                1 => candidate.reachable = false,
                2 => candidate.version_compatible = false,
                _ => candidate.policy_allowed = false,
            }
            assert!(!candidate.is_available(), "{label} gate must be required");
            assert_eq!(
                candidate.validate(),
                Err("an unavailable capability must state a redacted reason"),
                "{label} gate must explain itself"
            );
            candidate.reason = Some("adapter gate is not satisfied".into());
            assert!(candidate.validate().is_ok());
        }
    }

    #[test]
    fn admission_and_receipt_reject_unknown_projection_fields() {
        let mut value = serde_json::to_value(admission(ExternalWorkerMutation::Launch))
            .expect("admission serializes");
        value["providerUrl"] = serde_json::Value::String("https://api.cursor.com".into());
        assert!(serde_json::from_value::<ExternalWorkerAdmission>(value).is_err());

        let mut value = serde_json::to_value(receipt(ExternalWorkerReceiptState::Accepted))
            .expect("receipt serializes");
        value["apiKey"] = serde_json::Value::String("leak".into());
        assert!(serde_json::from_value::<ExternalWorkerReceipt>(value).is_err());
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
            Err("worker ref must be bounded and non-absolute")
        );
        request.starting_ref = "refs/heads/review".into();
        request.request_id = "req\n1".into();
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
    }
}
