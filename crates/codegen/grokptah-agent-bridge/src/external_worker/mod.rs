//! Trusted adapters for provider-managed coding workers.
//!
//! The browser and public SDK see only the projections from
//! `grokptah-agent-sdk`. Provider credentials stay in this native/server
//! boundary. The first adapter targets Cursor's Cloud Agents API v1; it does
//! not control a foreground Cursor desktop window and it never grants native
//! Computer Use authority.

mod authority;
mod cursor;
mod durable;
mod host;
mod ledger;

pub use authority::{
    AuthorityError, AuthorityState, AuthorityStore, ExternalWorkerAction, ExternalWorkerAuthority,
    ExternalWorkerPrincipal, LaunchIntent, NewGrant, MAX_AUTHORITY_FIELD_BYTES,
    MAX_AUTHORITY_LISTING, MAX_AUTHORITY_RUNS,
};
pub use cursor::{
    CursorCloudAdapter, CURSOR_CLOUD_API_BASE, MAX_EXTERNAL_WORKER_ARTIFACT_BYTES,
    MAX_EXTERNAL_WORKER_LISTING_BYTES, PRODUCTION_ARTIFACT_HOST_PREFIX,
};
pub use host::{ExternalWorkerHost, ReconcileReport};
pub use ledger::{
    canonical_cancel_payload_hash, canonical_follow_up_payload_hash, canonical_launch_payload_hash,
    ExternalWorkerLedger, ExternalWorkerLedgerClaim, ExternalWorkerLedgerStatus,
    ExternalWorkerOperation, ExternalWorkerSendState,
};

use async_trait::async_trait;
pub use grokptah_agent_sdk::ExternalWorkerProvider;

use grokptah_agent_sdk::{
    ExternalWorkerArtifact, ExternalWorkerFollowUpRequest, ExternalWorkerLaunchRequest,
    ExternalWorkerLaunchResult, ExternalWorkerRecord, ExternalWorkerRunRecord,
};
use parking_lot::RwLock;
use reqwest::StatusCode;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use thiserror::Error;

/// Known Cursor conflict codes that must be reconciled with GET state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderConflictCode {
    /// Duplicate client-supplied agent identity (`agent_conflict` / `agent_id_conflict`).
    AgentConflict,
    /// An active run already exists (`agent_busy`).
    AgentBusy,
    /// Cancel was refused because the run is not cancellable (`run_not_cancellable`).
    RunNotCancellable,
}

impl ProviderConflictCode {
    pub(crate) fn parse(code: &str) -> Option<Self> {
        match code {
            "agent_conflict" | "agent_id_conflict" => Some(Self::AgentConflict),
            "agent_busy" => Some(Self::AgentBusy),
            "run_not_cancellable" => Some(Self::RunNotCancellable),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::AgentConflict => "agent_conflict",
            Self::AgentBusy => "agent_busy",
            Self::RunNotCancellable => "run_not_cancellable",
        }
    }
}

/// Errors returned by a trusted external-worker adapter or host.
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
    Provider {
        /// HTTP status from the provider.
        status: StatusCode,
        /// Parsed conflict code, when the body carried a known code only.
        code: Option<ProviderConflictCode>,
    },
    /// The provider response could not be decoded or was incomplete.
    #[error("external worker provider response is invalid: {0}")]
    InvalidResponse(&'static str),
    /// The trusted transport failed before a response was received.
    #[error("external worker transport failed: {0}")]
    Transport(#[from] reqwest::Error),
    /// The configured API base URL is not safe for a server-side adapter.
    #[error("external worker API base URL is invalid")]
    InvalidBaseUrl,
    /// An identical in-flight request has not produced a durable result yet.
    #[error("external worker request is pending until reconciled")]
    Pending,
    /// A prior attempt's outcome is unknown; fail closed until an operator reconciles.
    #[error("external worker request outcome is uncertain until reconciled")]
    Uncertain,
    /// The same request_id was reused with a different canonical payload.
    #[error("external worker request_id reused with a different payload")]
    PayloadDrift,
    /// The caller holds no authority for this worker or this action.
    ///
    /// Carries the authority verdict rather than a bare boolean so the durable
    /// audit trail records *that* it was refused without recording which
    /// binding failed, which would tell a caller what to forge next.
    #[error("external worker action is not authorized")]
    Unauthorized(#[from] AuthorityError),
}

/// Who, if anyone, can actually make one requested ceiling true.
///
/// A bound that is accepted and then honored by nobody is worse than a refused
/// one: the caller believes a limit is in force. Every ceiling therefore has to
/// name its enforcer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundEnforcement {
    /// This host enforces it, before or around the provider call.
    Host,
    /// The provider accepts the value and enforces it; it is transmitted.
    Provider,
    /// Nothing here can honor it. Requesting it fails closed.
    Unsupported,
}

/// Which ceilings one adapter can actually make true.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundsSupport {
    /// Enforcement for [`Bounds::max_prompt_bytes`].
    pub max_prompt_bytes: BoundEnforcement,
    /// Enforcement for [`Bounds::max_rounds`].
    pub max_rounds: BoundEnforcement,
    /// Enforcement for [`Bounds::max_duration_ms`].
    pub max_duration_ms: BoundEnforcement,
}

impl BoundsSupport {
    /// Nothing is supported. The safe default for a new adapter: a ceiling is
    /// only honored once someone has said, explicitly, that they honor it.
    pub const NONE: Self = Self {
        max_prompt_bytes: BoundEnforcement::Unsupported,
        max_rounds: BoundEnforcement::Unsupported,
        max_duration_ms: BoundEnforcement::Unsupported,
    };

    /// Refuse a request that asks for a ceiling nobody will enforce, and apply
    /// every host-enforced ceiling now.
    ///
    /// Returns the disposition to record durably, so the grant proves which
    /// ceilings were real rather than merely requested.
    pub fn admit(
        &self,
        bounds: Option<&grokptah_agent_sdk::Bounds>,
        prompt: &str,
    ) -> Result<BoundsDisposition, ExternalWorkerAdapterError> {
        let Some(bounds) = bounds else {
            return Ok(BoundsDisposition::default());
        };
        let mut disposition = BoundsDisposition::default();
        if let Some(limit) = bounds.max_prompt_bytes {
            match self.max_prompt_bytes {
                BoundEnforcement::Unsupported => {
                    return Err(ExternalWorkerAdapterError::InvalidRequest(
                        "provider cannot honor max_prompt_bytes",
                    ))
                }
                BoundEnforcement::Host => {
                    // Enforced here, at admission, before anything is sent.
                    if prompt.len() as u64 > u64::from(limit) {
                        return Err(ExternalWorkerAdapterError::InvalidRequest(
                            "prompt exceeds the requested max_prompt_bytes",
                        ));
                    }
                    disposition.max_prompt_bytes = Some(BoundEnforcement::Host);
                }
                BoundEnforcement::Provider => {
                    disposition.max_prompt_bytes = Some(BoundEnforcement::Provider);
                }
            }
        }
        if bounds.max_rounds.is_some() {
            match self.max_rounds {
                BoundEnforcement::Unsupported => {
                    return Err(ExternalWorkerAdapterError::InvalidRequest(
                        "provider cannot honor max_rounds",
                    ))
                }
                enforcement => disposition.max_rounds = Some(enforcement),
            }
        }
        if bounds.max_duration_ms.is_some() {
            match self.max_duration_ms {
                BoundEnforcement::Unsupported => {
                    return Err(ExternalWorkerAdapterError::InvalidRequest(
                        "provider cannot honor max_duration_ms",
                    ))
                }
                enforcement => disposition.max_duration_ms = Some(enforcement),
            }
        }
        Ok(disposition)
    }
}

/// What was actually done about each requested ceiling.
///
/// `None` means the caller did not ask for that ceiling. A ceiling that was
/// asked for is always either recorded here with its enforcer or refused at
/// admission; it is never silently dropped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundsDisposition {
    /// Enforcer for a requested prompt-byte ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_prompt_bytes: Option<BoundEnforcement>,
    /// Enforcer for a requested round ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<BoundEnforcement>,
    /// Enforcer for a requested duration ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_duration_ms: Option<BoundEnforcement>,
}

/// Provider-neutral lifecycle operations required by the manager.
#[async_trait]
pub trait ExternalWorkerAdapter: Send + Sync {
    /// Provider family implemented by this adapter.
    fn provider(&self) -> ExternalWorkerProvider;

    /// Non-secret label identifying the provider account this adapter acts as.
    ///
    /// Two accounts under one provider are different authorities: an ID minted
    /// by one must not be actionable with the other's credential. The label is
    /// derived from the credential rather than being the credential, so it can
    /// sit in a durable grant and an audit record safely.
    fn account_identity(&self) -> String;

    /// Which requested ceilings this adapter can actually make true.
    ///
    /// Defaults to none, so an adapter that has not thought about bounds
    /// refuses every ceiling rather than accepting and ignoring it.
    fn bounds_support(&self) -> BoundsSupport {
        BoundsSupport::NONE
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

    /// List provider artifacts only when they are run-attributed and digested.
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

/// Process-local registry for qualified external-worker providers.
///
/// Registration is deliberately explicit. A provider is not available merely
/// because a credential exists; the host must install a qualified adapter and
/// an explicit repository allowlist. The manager still applies workspace,
/// approval, and promotion policy.
#[derive(Default)]
pub struct ExternalWorkerRegistry {
    adapters: RwLock<HashMap<ExternalWorkerProvider, Arc<dyn ExternalWorkerAdapter>>>,
    allowlists: RwLock<HashMap<ExternalWorkerProvider, Arc<BTreeSet<String>>>>,
}

impl ExternalWorkerRegistry {
    /// Create an empty provider registry with no launch rights.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register or replace one provider adapter during host setup.
    ///
    /// This does not grant launch rights. Call [`Self::set_repository_allowlist`]
    /// before the host will launch into any repository.
    pub fn register(&self, adapter: Arc<dyn ExternalWorkerAdapter>) {
        self.adapters.write().insert(adapter.provider(), adapter);
    }

    /// Install the host-level repository allowlist for a provider.
    ///
    /// An empty iterator is rejected. Until this succeeds, the host refuses
    /// launches even if the adapter itself was constructed with repositories.
    pub fn set_repository_allowlist<I, S>(
        &self,
        provider: ExternalWorkerProvider,
        repositories: I,
    ) -> Result<(), ExternalWorkerAdapterError>
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
                "host repository allowlist must not be empty",
            ));
        }
        self.allowlists
            .write()
            .insert(provider, Arc::new(allowlist));
        Ok(())
    }

    /// Return the adapter for a provider, if the host explicitly installed it.
    pub fn get(&self, provider: ExternalWorkerProvider) -> Option<Arc<dyn ExternalWorkerAdapter>> {
        self.adapters.read().get(&provider).cloned()
    }

    /// True only when the host allowlist contains this repository identity.
    pub fn repository_allowed(
        &self,
        provider: ExternalWorkerProvider,
        repository: &str,
    ) -> Result<bool, ExternalWorkerAdapterError> {
        let identity = repository_identity(&github_repository_url(repository)?)?;
        Ok(self
            .allowlists
            .read()
            .get(&provider)
            .is_some_and(|allowlist| allowlist.contains(&identity)))
    }

    /// Return the provider families explicitly installed in this process.
    pub fn providers(&self) -> Vec<ExternalWorkerProvider> {
        let mut providers = self.adapters.read().keys().copied().collect::<Vec<_>>();
        providers.sort_by_key(|provider| *provider as u8);
        providers
    }
}

pub(crate) fn github_repository_url(
    repository: &str,
) -> Result<String, ExternalWorkerAdapterError> {
    if let Some(path) = repository.strip_prefix("https://github.com/") {
        if repository.contains('?')
            || repository.contains('#')
            || repository.ends_with('/')
            || path.split('/').count() != 2
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

pub(crate) fn repository_identity(value: &str) -> Result<String, ExternalWorkerAdapterError> {
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

pub(crate) fn checked_id(value: &str) -> Result<&str, ExternalWorkerAdapterError> {
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

pub(crate) fn refs_equal(left: &str, right: &str) -> bool {
    left == right
        || left.strip_prefix("refs/heads/") == Some(right)
        || right.strip_prefix("refs/heads/") == Some(left)
}

pub(crate) fn extract_provider_conflict_code(body: &str) -> Option<ProviderConflictCode> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let code = value
        .get("code")
        .and_then(|item| item.as_str())
        .or_else(|| {
            value
                .get("error")
                .and_then(|error| error.get("code"))
                .and_then(|item| item.as_str())
        })?;
    ProviderConflictCode::parse(code)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn registry_exposes_only_explicitly_installed_providers_and_allowlists() {
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
        assert!(!registry
            .repository_allowed(ExternalWorkerProvider::CursorCloud, "chriscase/GrokPtah")
            .unwrap());
        registry
            .set_repository_allowlist(ExternalWorkerProvider::CursorCloud, ["chriscase/GrokPtah"])
            .unwrap();
        assert!(registry
            .repository_allowed(ExternalWorkerProvider::CursorCloud, "chriscase/GrokPtah")
            .unwrap());
        assert!(!registry
            .repository_allowed(ExternalWorkerProvider::CursorCloud, "other/repo")
            .unwrap());
        assert!(registry
            .set_repository_allowlist(
                ExternalWorkerProvider::CursorCloud,
                std::iter::empty::<&str>(),
            )
            .is_err());
    }

    /// The registry must not hand out launch rights merely because a
    /// credential exists. This is the state the process starts in.
    #[test]
    fn a_fresh_registry_grants_nothing_until_bootstrap_installs_a_provider() {
        let registry = ExternalWorkerRegistry::new();
        assert!(registry.providers().is_empty());
        assert!(registry.get(ExternalWorkerProvider::CursorCloud).is_none());
        assert!(!registry
            .repository_allowed(ExternalWorkerProvider::CursorCloud, "chriscase/GrokPtah")
            .unwrap());
        // Registering an adapter alone still does not grant launch rights.
        registry.register(Arc::new(CursorCloudAdapter::new("synthetic-key").unwrap()));
        assert!(!registry
            .repository_allowed(ExternalWorkerProvider::CursorCloud, "chriscase/GrokPtah")
            .unwrap());
    }

    #[test]
    fn known_conflict_codes_are_parsed_without_retaining_the_body() {
        assert_eq!(
            extract_provider_conflict_code(r#"{"code":"agent_id_conflict"}"#),
            Some(ProviderConflictCode::AgentConflict)
        );
        assert_eq!(
            extract_provider_conflict_code(r#"{"error":{"code":"agent_busy"}}"#),
            Some(ProviderConflictCode::AgentBusy)
        );
        assert_eq!(
            extract_provider_conflict_code(r#"{"code":"run_not_cancellable"}"#),
            Some(ProviderConflictCode::RunNotCancellable)
        );
        assert_eq!(
            extract_provider_conflict_code(r#"{"code":"unexpected","secret":"token"}"#),
            None
        );
        assert_eq!(ProviderConflictCode::AgentBusy.as_str(), "agent_busy");
    }
}
