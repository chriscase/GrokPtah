//! Desktop-adapter projection of fail-closed Grok Build launch truth.
//!
//! [`crate::account_facts`] answers whether a credential exists and whether it
//! has visibly expired. That is necessary but not sufficient to admit durable
//! work: a run also needs a provider, a route, a base endpoint, a dialect, a
//! model, and capability evidence. This module resolves all of them from live
//! local state and hands them to [`grokptah_agent_sdk::launch`], which decides.
//!
//! The decision rules live in the SDK, which has no filesystem, network, or
//! credential dependency. This module only decides *which* local state to hand
//! it, and never copies credential material across the boundary: the resolved
//! [`WireCredentials`] bearer and refresh token are read here only to the
//! extent of choosing a route and establishing refreshability, never projected.
//!
//! # The admission gate
//!
//! [`admit`] is the one entry point a durable admission may use. It re-resolves
//! every fact, refreshes an expiring credential first, and returns bounded
//! attribution only when the freshly resolved truth is ready *and* still
//! matches the requirement the caller decided on. Everything else returns a
//! typed [`LaunchReason`], never a bare error string.

use grokptah_agent_sdk::account::{GrokAccountFacts, RunAttribution};
use grokptah_agent_sdk::launch::{
    BaseCategory, CapabilityFacts, CapabilityProvenance, GrokLaunchTruth, LaunchObservation,
    LaunchReason, ModelFacts, ModelReference, ProviderClass, Refreshability, RequestDialect,
    RouteClass,
};

/// Re-exported so a consumer can name the exact facts an admission is pinned
/// to without importing the SDK crate directly.
pub use grokptah_agent_sdk::launch::LaunchRequirement;
use grokptah_agent_sdk::outcome::{RunFailureKind, TerminalVerdict};

use crate::account_facts;
use crate::auth_store::{self, WireCredentials};
use crate::gateway_config::{
    CapabilitySource, ModelCapabilities, ProviderDialect, XAI_PROVIDER_ID,
};
use crate::host_helpers::{self, ResolvedModelTarget};

/// Hosts the first-party xAI API and the Grok Build CLI chat proxy.
///
/// Matched as exact hosts or as suffixes below `x.ai`, never by substring: a
/// substring test would accept `api.x.ai.attacker.example`.
const XAI_OFFICIAL_HOSTS: [&str; 2] = ["api.x.ai", "cli-chat-proxy.grok.com"];
const XAI_OFFICIAL_SUFFIX: &str = ".x.ai";

/// What resolving the stored model selection produced.
///
/// Distinguishing these is the difference between "pick a model" and "that
/// model belongs to another provider", which are different operator actions.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ModelTargetOutcome<'a> {
    /// The selection resolved to an exact provider/model target.
    Resolved(&'a ResolvedModelTarget),
    /// No model is selected at all.
    NotSelected,
    /// A selection exists but could not be parsed.
    Unparseable,
    /// The selection names a provider the credential does not belong to.
    ProviderMismatch,
    /// The selection parsed but the provider does not offer that model.
    NotOffered,
}

/// Non-secret, already-resolved inputs for one launch projection.
///
/// Deliberately has no field that can carry a bearer, refresh token, API key,
/// or keychain reference. `refresh_material_present` is a boolean precisely so
/// the refresh token itself never crosses this boundary.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LaunchInputs<'a> {
    /// Credential-free account facts, already projected.
    pub account: &'a GrokAccountFacts,
    /// Provider profile id that owns the resolved credential.
    pub credential_provider_id: &'a str,
    /// Whether durable refresh material is present on the resolved route.
    pub refresh_material_present: bool,
    /// Outcome of resolving the stored selection against that credential.
    pub target: ModelTargetOutcome<'a>,
}

/// A durable admission that passed the gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionFacts {
    /// The freshly re-resolved truth the admission was granted on.
    pub truth: GrokLaunchTruth,
    /// The exact facts this admission is pinned to.
    pub requirement: LaunchRequirement,
    /// Bounded, credential-free attribution to record on the run.
    pub attribution: RunAttribution,
}

/// Classify a base endpoint without publishing it.
///
/// Mirrors [`crate::gateway_config::validate_base_url`]: anything that
/// validator rejects is [`BaseCategory::Malformed`] or
/// [`BaseCategory::InsecureTransport`] here, so a profile that could never be
/// saved also can never be launched against.
pub(crate) fn classify_base(base_url: &str) -> BaseCategory {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return BaseCategory::Unset;
    }
    let Ok(parsed) = reqwest::Url::parse(trimmed) else {
        return BaseCategory::Malformed;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return BaseCategory::Malformed;
    }
    // Embedded credentials, a query, or a fragment mean this is not a base at
    // all; refusing them here keeps a crafted profile from reaching a provider.
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return BaseCategory::Malformed;
    }
    let Some(host) = parsed.host_str() else {
        return BaseCategory::Malformed;
    };
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if parsed.scheme() == "http" {
        return if loopback {
            BaseCategory::CompatibleLoopback
        } else {
            BaseCategory::InsecureTransport
        };
    }
    if loopback {
        return BaseCategory::CompatibleLoopback;
    }
    let lowered = host.to_ascii_lowercase();
    if XAI_OFFICIAL_HOSTS.contains(&lowered.as_str()) || lowered.ends_with(XAI_OFFICIAL_SUFFIX) {
        BaseCategory::XaiOfficial
    } else {
        BaseCategory::CompatibleHttps
    }
}

/// Classify the provider family behind a resolved credential.
pub(crate) fn classify_provider(credential_provider_id: &str) -> ProviderClass {
    match credential_provider_id.trim() {
        "" => ProviderClass::Unrecognized,
        XAI_PROVIDER_ID => ProviderClass::Xai,
        _ => ProviderClass::OpenAiCompatible,
    }
}

/// Classify how a request reaches that provider.
pub(crate) fn classify_route(provider: ProviderClass) -> RouteClass {
    match provider {
        ProviderClass::Xai => RouteClass::XaiFirstParty,
        ProviderClass::OpenAiCompatible => RouteClass::CompatibleProvider,
        ProviderClass::Unrecognized => RouteClass::Unrecognized,
    }
}

/// Map the wire dialect onto the published closed vocabulary.
pub(crate) fn classify_dialect(dialect: ProviderDialect) -> RequestDialect {
    match dialect {
        ProviderDialect::XaiChatCompletions => RequestDialect::XaiChatCompletions,
        ProviderDialect::OpenAiChatCompletions => RequestDialect::OpenAiChatCompletions,
    }
}

/// Establish whether the resolved route can renew itself without a human.
///
/// Read from the presence of durable refresh machinery on the route, never
/// from a token body. An unclassified route stays
/// [`Refreshability::Unknown`], which blocks.
pub(crate) fn classify_refreshability(
    method: grokptah_agent_sdk::account::CredentialMethod,
    refresh_material_present: bool,
) -> Refreshability {
    use grokptah_agent_sdk::account::CredentialMethod as Method;
    match method {
        Method::Absent | Method::Unknown => Refreshability::Unknown,
        // A helper command re-mints on demand; that *is* the refresh path.
        Method::TokenCommand => Refreshability::Refreshable,
        Method::GrokBuildOidc => {
            if refresh_material_present {
                Refreshability::Refreshable
            } else {
                Refreshability::NotRefreshable
            }
        }
        // Long-lived keys have no renewal path: rotation is a human action.
        Method::ApiKey
        | Method::ProviderEnv
        | Method::ProviderKeychain
        | Method::GrokBuildApiKey => Refreshability::NotRefreshable,
    }
}

/// Project capability evidence for one exact provider/model pair.
///
/// A capability whose provenance is unknown is reported as
/// [`CapabilityProvenance::Unprobed`] with every flag cleared, so an unprobed
/// model can never assert a capability it was never observed to have.
pub(crate) fn classify_capabilities(capabilities: &ModelCapabilities) -> CapabilityFacts {
    let provenance = match capabilities.source {
        CapabilitySource::Measured => CapabilityProvenance::Measured,
        CapabilitySource::Declared => CapabilityProvenance::Declared,
        CapabilitySource::Unknown => CapabilityProvenance::Unprobed,
    };
    if provenance == CapabilityProvenance::Unprobed {
        return CapabilityFacts::unprobed();
    }
    CapabilityFacts {
        provenance,
        chat: capabilities.chat,
        tools: capabilities.tools,
        stream: capabilities.stream,
        parallel_tool_calls: capabilities.parallel_tool_calls,
        image_input: capabilities.image_input,
    }
}

/// Project fail-closed launch truth from already-resolved, non-secret inputs.
///
/// Pure: no filesystem, network, keychain, or clock access, so every verdict
/// below is reproducible in a test.
pub(crate) fn project(inputs: &LaunchInputs<'_>) -> GrokLaunchTruth {
    let provider = classify_provider(inputs.credential_provider_id);
    let route = classify_route(provider);
    let refreshability = classify_refreshability(
        inputs.account.credential_method,
        inputs.refresh_material_present,
    );
    let (base, dialect, model, capabilities) = match inputs.target {
        ModelTargetOutcome::Resolved(target) => (
            classify_base(&target.base_url),
            classify_dialect(target.dialect),
            match ModelReference::new(&target.wire_model) {
                Some(reference) => ModelFacts::selected(reference),
                // A model id the projection cannot bound is not publishable,
                // and an unpublishable id must not silently reach a provider.
                None => ModelFacts::unparseable(),
            },
            classify_capabilities(&target.capabilities),
        ),
        // Without a resolved target there is no endpoint, dialect, or
        // capability evidence to report — and saying so is the point.
        ModelTargetOutcome::NotSelected => (
            BaseCategory::Unset,
            RequestDialect::Unrecognized,
            ModelFacts::not_selected(),
            CapabilityFacts::unprobed(),
        ),
        ModelTargetOutcome::Unparseable => (
            BaseCategory::Unset,
            RequestDialect::Unrecognized,
            ModelFacts::unparseable(),
            CapabilityFacts::unprobed(),
        ),
        ModelTargetOutcome::ProviderMismatch => (
            BaseCategory::Unset,
            RequestDialect::Unrecognized,
            ModelFacts::route_mismatch(),
            CapabilityFacts::unprobed(),
        ),
        ModelTargetOutcome::NotOffered => (
            BaseCategory::Unset,
            RequestDialect::Unrecognized,
            ModelFacts::not_offered(),
            CapabilityFacts::unprobed(),
        ),
    };
    GrokLaunchTruth::project(&LaunchObservation {
        provider,
        route,
        base,
        dialect,
        refreshability,
        model,
        capabilities,
        account: inputs.account,
    })
}

/// Whether a resolved credential carries durable refresh machinery.
///
/// Reads only for presence. The refresh token's value is never returned,
/// logged, or projected.
fn refresh_material_present(credentials: &WireCredentials) -> bool {
    credentials
        .refresh_token
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        && credentials
            .auth_scope
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
}

/// Resolve the stored model selection against a resolved credential.
fn resolve_target(credentials: &WireCredentials, model_selection: &str) -> ResolvedTarget {
    if model_selection.trim().is_empty() {
        return ResolvedTarget::NotSelected;
    }
    let selection = match crate::gateway_config::parse_model_selection(model_selection) {
        Ok(selection) => selection,
        Err(_) => return ResolvedTarget::Unparseable,
    };
    if selection.provider_id != credentials.provider_id {
        return ResolvedTarget::ProviderMismatch;
    }
    match host_helpers::resolve_model_target(credentials, model_selection) {
        Ok(target) => ResolvedTarget::Resolved(Box::new(target)),
        // The selection parsed and matches the credential's provider, so the
        // only remaining resolution failure is an unoffered model.
        Err(_) => ResolvedTarget::NotOffered,
    }
}

/// Owned form of [`ModelTargetOutcome`], so the resolver can return a target.
enum ResolvedTarget {
    Resolved(Box<ResolvedModelTarget>),
    NotSelected,
    Unparseable,
    ProviderMismatch,
    NotOffered,
}

impl ResolvedTarget {
    fn as_outcome(&self) -> ModelTargetOutcome<'_> {
        match self {
            Self::Resolved(target) => ModelTargetOutcome::Resolved(target),
            Self::NotSelected => ModelTargetOutcome::NotSelected,
            Self::Unparseable => ModelTargetOutcome::Unparseable,
            Self::ProviderMismatch => ModelTargetOutcome::ProviderMismatch,
            Self::NotOffered => ModelTargetOutcome::NotOffered,
        }
    }
}

/// Resolve the credential a launch would actually spend.
///
/// A launch runs against the provider the *selected model* belongs to, which
/// is not always the built-in xAI profile. Resolving from the selection is the
/// same rule [`crate::host_helpers::resolve_model_target`] applies at request
/// time, so the credential this projection describes is the credential the
/// turn will use.
fn resolve_launch_credentials(model_selection: &str) -> Option<WireCredentials> {
    if model_selection.trim().is_empty() {
        return auth_store::resolve_wire_credentials();
    }
    // An unparseable selection cannot name a provider, so the error case
    // resolves no credential and the projection says so rather than falling
    // back to guessing at xAI.
    auth_store::resolve_wire_credentials_for_model(model_selection).unwrap_or_default()
}

/// Resolve launch truth from live local state without attempting a refresh.
///
/// `now_unix` is a parameter rather than a wall-clock read so callers and
/// tests share one deterministic definition of "now".
pub fn resolve_launch_truth(model_selection: &str, now_unix: i64) -> GrokLaunchTruth {
    let Some(credentials) = resolve_launch_credentials(model_selection) else {
        return GrokLaunchTruth::unresolved();
    };
    project_from_credentials(&credentials, model_selection, now_unix)
}

fn project_from_credentials(
    credentials: &WireCredentials,
    model_selection: &str,
    now_unix: i64,
) -> GrokLaunchTruth {
    let account = account_facts::account_facts_for_resolved_route(credentials, now_unix);
    let target = resolve_target(credentials, model_selection);
    project(&LaunchInputs {
        account: &account,
        credential_provider_id: &credentials.provider_id,
        refresh_material_present: refresh_material_present(credentials),
        target: target.as_outcome(),
    })
}

/// Re-resolve every launch fact from live local state, refreshing first.
///
/// The refresh is attempted *before* projection so an expiring OIDC session is
/// renewed rather than reported as expired. A refresh that does not succeed
/// leaves the original credential in place, and the projection then reports
/// the honest expired state.
pub async fn re_resolve_launch_truth(model_selection: &str, now_unix: i64) -> GrokLaunchTruth {
    let Some(credentials) = resolve_launch_credentials(model_selection) else {
        return GrokLaunchTruth::unresolved();
    };
    let refreshed = auth_store::ensure_fresh_credentials(credentials).await;
    project_from_credentials(&refreshed, model_selection, now_unix)
}

/// The durable-admission gate.
///
/// Re-resolves and refreshes, then requires that the fresh truth is ready and
/// that it still matches `requirement` on every fact that decides where a
/// run's tokens are spent. `requirement` is `None` for a first admission,
/// where the fresh truth *is* the requirement.
///
/// Returns a typed [`LaunchReason`] on refusal. There is no path that returns
/// a bounded attribution for a truth that is not ready.
pub async fn admit(
    model_selection: &str,
    requirement: Option<&LaunchRequirement>,
    now_unix: i64,
) -> Result<AdmissionFacts, LaunchReason> {
    let truth = re_resolve_launch_truth(model_selection, now_unix).await;
    enforce(truth, requirement)
}

/// Enforce a resolved truth against an optional pinned requirement.
///
/// Split out from [`admit`] so the decision is testable without touching the
/// keychain, the filesystem, or the network.
pub fn enforce(
    truth: GrokLaunchTruth,
    requirement: Option<&LaunchRequirement>,
) -> Result<AdmissionFacts, LaunchReason> {
    // A projection that fails its own validator is not evidence of anything.
    if truth.validate().is_err() {
        return Err(LaunchReason::CredentialRouteUnrecognized);
    }
    if !truth.permits_launch() {
        return Err(truth.reason);
    }
    let requirement = match requirement {
        Some(pinned) => {
            truth.enforce(pinned)?;
            pinned.clone()
        }
        None => truth.requirement(),
    };
    let attribution = truth.attribution();
    if attribution.validate().is_err() {
        return Err(LaunchReason::CredentialRouteUnrecognized);
    }
    Ok(AdmissionFacts {
        truth,
        requirement,
        attribution,
    })
}

/// The typed terminal verdict for a launch that was refused.
///
/// Guarantees the refusal is recorded as blocked, failed, or indeterminate —
/// never as a completed run.
pub fn refusal_verdict(reason: LaunchReason) -> TerminalVerdict {
    RunFailureKind::from_launch_reason(reason)
        // A ready reason never reaches a refusal path; recording it as a
        // host-side block is the safe reading if it ever does.
        .unwrap_or(RunFailureKind::LaunchBlocked)
        .verdict()
}

/// Whether this host can reach a provider at all.
///
/// The turn runner short-circuits to a stubbed offline turn *before* it
/// resolves any credential, so an offline host cannot spend one no matter what
/// the launch facts say. This reads the same switch that short-circuit reads,
/// in one place, so the two can never disagree about whether a credential is
/// about to be used.
fn provider_reachable() -> bool {
    std::env::var_os("GROKPTAH_AGENT_OFFLINE").is_none()
}

/// What a durable admission is allowed to record.
///
/// Deliberately not an `Option<AdmissionFacts>`: the two cases mean different
/// things, and collapsing them would let "no credential was spent" be read as
/// "we forgot to record which credential was spent".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Launch truth was established, re-resolved, and enforced.
    Enforced(Box<AdmissionFacts>),
    /// No provider is reachable from this host, so no credential can be
    /// resolved, enforced, or spent. Records no attribution: claiming one
    /// would attribute a run to an account it never touched.
    NoProviderReachable,
}

impl Admission {
    /// The enforced facts, when a credential was actually established.
    pub fn facts(&self) -> Option<&AdmissionFacts> {
        match self {
            Self::Enforced(facts) => Some(facts),
            Self::NoProviderReachable => None,
        }
    }

    /// Bounded attribution to record on the run, when there is any.
    pub fn attribution(&self) -> Option<RunAttribution> {
        self.facts().map(|facts| facts.attribution.clone())
    }

    /// The exact facts to pin this admission to, when there are any.
    pub fn requirement(&self) -> Option<LaunchRequirement> {
        self.facts().map(|facts| facts.requirement.clone())
    }
}

/// The durable-admission gate, as an injectable seam.
///
/// Production installs [`HostLaunchGate`], which re-resolves and refreshes
/// live local credentials. Tests install a deterministic gate so admission
/// policy is exercised without a keychain, a network, or a wall clock.
#[async_trait::async_trait]
pub trait LaunchGate: Send + Sync + 'static {
    /// Re-resolve launch truth and enforce it, optionally against a pinned
    /// requirement from an earlier decision.
    async fn admit(
        &self,
        requirement: Option<&LaunchRequirement>,
    ) -> Result<Admission, LaunchReason>;
}

/// The production gate: live local credentials, refreshed, at the wall clock.
#[derive(Clone)]
pub struct HostLaunchGate {
    model_selection: std::sync::Arc<dyn Fn() -> String + Send + Sync>,
}

impl std::fmt::Debug for HostLaunchGate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The reader closure has no share-safe representation, and the model
        // it returns is live state rather than a property of the gate.
        formatter.write_str("HostLaunchGate")
    }
}

impl HostLaunchGate {
    /// Build a gate that reads the currently selected model on every call.
    ///
    /// The selection is read fresh rather than captured, so a model switched
    /// between deciding and admitting is caught by the requirement check
    /// instead of being silently honoured.
    pub fn new(model_selection: impl Fn() -> String + Send + Sync + 'static) -> Self {
        Self {
            model_selection: std::sync::Arc::new(model_selection),
        }
    }
}

#[async_trait::async_trait]
impl LaunchGate for HostLaunchGate {
    async fn admit(
        &self,
        requirement: Option<&LaunchRequirement>,
    ) -> Result<Admission, LaunchReason> {
        if !provider_reachable() {
            return Ok(Admission::NoProviderReachable);
        }
        let selection = (self.model_selection)();
        admit(&selection, requirement, chrono::Utc::now().timestamp())
            .await
            .map(|facts| Admission::Enforced(Box::new(facts)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grokptah_agent_sdk::account::{
        AccountObservation, CredentialMethod, CredentialSource, GrokAccountFacts,
    };
    use grokptah_agent_sdk::launch::{LaunchReadiness, ModelStatus};

    /// Fixed observation clock: 2026-08-25T00:00:00Z, matching the SDK tests.
    const NOW: i64 = 1_787_616_000;
    const SENTINEL_BEARER: &str = "xai-SENTINEL-BEARER-DO-NOT-LEAK";
    const SENTINEL_REFRESH: &str = "xai-SENTINEL-REFRESH-DO-NOT-LEAK";
    const SENTINEL_HOST: &str = "internal-tenant-7.corp.example";

    fn account(auth_mode: Option<&str>, expires_at: Option<&str>) -> GrokAccountFacts {
        GrokAccountFacts::project(
            CredentialSource::GrokBuildSession,
            &AccountObservation {
                auth_mode,
                user_id: Some("usr-0a1b2c3d"),
                principal_id: None,
                team_id: None,
                expires_at,
            },
            NOW,
        )
    }

    fn target(base_url: &str, dialect: ProviderDialect, model: &str) -> ResolvedModelTarget {
        ResolvedModelTarget {
            base_url: base_url.into(),
            wire_model: model.into(),
            dialect,
            capabilities: ModelCapabilities {
                chat: true,
                tools: true,
                stream: true,
                parallel_tool_calls: true,
                source: CapabilitySource::Declared,
                ..ModelCapabilities::default()
            },
            deadline_class: crate::gateway_config::ProviderDeadlineClass::Standard,
        }
    }

    #[test]
    fn the_base_classifier_matches_the_base_url_validator_it_shadows() {
        // Anything `validate_base_url` refuses must be unlaunchable here, or a
        // profile that could never be saved could still be launched against.
        for (url, expected) in [
            ("https://api.x.ai/v1", BaseCategory::XaiOfficial),
            (
                "https://cli-chat-proxy.grok.com/v1",
                BaseCategory::XaiOfficial,
            ),
            ("https://management.x.ai/v1", BaseCategory::XaiOfficial),
            ("https://corp.example/v1", BaseCategory::CompatibleHttps),
            (
                "http://localhost:11434/v1",
                BaseCategory::CompatibleLoopback,
            ),
            ("http://127.0.0.1:8080/v1", BaseCategory::CompatibleLoopback),
            ("http://[::1]:8080/v1", BaseCategory::CompatibleLoopback),
            (
                "https://127.0.0.1:8443/v1",
                BaseCategory::CompatibleLoopback,
            ),
            ("", BaseCategory::Unset),
            ("   ", BaseCategory::Unset),
            ("http://corp.example/v1", BaseCategory::InsecureTransport),
            (
                "http://169.254.169.254/latest",
                BaseCategory::InsecureTransport,
            ),
            ("ftp://corp.example/v1", BaseCategory::Malformed),
            ("file:///etc/passwd", BaseCategory::Malformed),
            ("not a url", BaseCategory::Malformed),
            (
                "https://user:secret@corp.example/v1",
                BaseCategory::Malformed,
            ),
            (
                "https://corp.example/v1?key=secret",
                BaseCategory::Malformed,
            ),
            ("https://corp.example/v1#token", BaseCategory::Malformed),
        ] {
            assert_eq!(classify_base(url), expected, "{url:?} classified wrongly");
            let validator_accepts = crate::gateway_config::validate_base_url(url).is_ok();
            if !validator_accepts && !url.trim().is_empty() {
                assert!(
                    !classify_base(url).is_launchable(),
                    "{url:?} is refused by the validator but launchable here"
                );
            }
        }
    }

    /// A substring test would accept an attacker-controlled host that merely
    /// *contains* an official one.
    #[test]
    fn a_lookalike_host_never_passes_as_the_official_xai_base() {
        for hostile in [
            "https://api.x.ai.attacker.example/v1",
            "https://cli-chat-proxy.grok.com.attacker.example/v1",
            "https://notx.ai/v1",
            "https://x.ai.evil.test/v1",
            "https://api-x-ai.example/v1",
        ] {
            assert_eq!(
                classify_base(hostile),
                BaseCategory::CompatibleHttps,
                "{hostile:?} was accepted as the official xAI base"
            );
        }
        // Case is not a way past it either.
        assert_eq!(
            classify_base("https://API.X.AI/v1"),
            BaseCategory::XaiOfficial
        );
    }

    #[test]
    fn provider_route_and_dialect_classification_is_closed() {
        assert_eq!(classify_provider("xai"), ProviderClass::Xai);
        assert_eq!(classify_provider("corp"), ProviderClass::OpenAiCompatible);
        assert_eq!(classify_provider(""), ProviderClass::Unrecognized);
        assert_eq!(classify_provider("   "), ProviderClass::Unrecognized);
        assert_eq!(
            classify_route(ProviderClass::Xai),
            RouteClass::XaiFirstParty
        );
        assert_eq!(
            classify_route(ProviderClass::OpenAiCompatible),
            RouteClass::CompatibleProvider
        );
        assert_eq!(
            classify_route(ProviderClass::Unrecognized),
            RouteClass::Unrecognized
        );
        assert_eq!(
            classify_dialect(ProviderDialect::XaiChatCompletions),
            RequestDialect::XaiChatCompletions
        );
        assert_eq!(
            classify_dialect(ProviderDialect::OpenAiChatCompletions),
            RequestDialect::OpenAiChatCompletions
        );
        // Whatever the route is, it must agree with its own provider.
        for provider in ProviderClass::ALL {
            assert_eq!(classify_route(provider).expected_provider(), provider);
        }
    }

    #[test]
    fn refreshability_is_read_from_the_route_not_from_a_token() {
        for (method, present, expected) in [
            (
                CredentialMethod::GrokBuildOidc,
                true,
                Refreshability::Refreshable,
            ),
            (
                CredentialMethod::GrokBuildOidc,
                false,
                Refreshability::NotRefreshable,
            ),
            (
                CredentialMethod::TokenCommand,
                false,
                Refreshability::Refreshable,
            ),
            (
                CredentialMethod::ApiKey,
                true,
                Refreshability::NotRefreshable,
            ),
            (
                CredentialMethod::ProviderEnv,
                true,
                Refreshability::NotRefreshable,
            ),
            (
                CredentialMethod::ProviderKeychain,
                true,
                Refreshability::NotRefreshable,
            ),
            (
                CredentialMethod::GrokBuildApiKey,
                true,
                Refreshability::NotRefreshable,
            ),
            (CredentialMethod::Absent, true, Refreshability::Unknown),
            (CredentialMethod::Unknown, true, Refreshability::Unknown),
        ] {
            assert_eq!(
                classify_refreshability(method, present),
                expected,
                "{method:?}/{present} classified wrongly"
            );
        }
    }

    #[test]
    fn unknown_capability_provenance_asserts_nothing() {
        let optimistic = ModelCapabilities {
            chat: true,
            tools: true,
            stream: true,
            parallel_tool_calls: true,
            image_input: true,
            source: CapabilitySource::Unknown,
            ..ModelCapabilities::default()
        };
        let facts = classify_capabilities(&optimistic);
        assert_eq!(facts.provenance, CapabilityProvenance::Unprobed);
        assert_eq!(facts, CapabilityFacts::unprobed());
        assert_eq!(facts.validate(), Ok(()));

        let measured = ModelCapabilities {
            source: CapabilitySource::Measured,
            ..optimistic
        };
        let facts = classify_capabilities(&measured);
        assert_eq!(facts.provenance, CapabilityProvenance::Measured);
        assert!(facts.chat && facts.tools && facts.stream);
    }

    #[test]
    fn a_fully_resolved_first_party_route_is_ready() {
        let account = account(Some("oidc"), Some("2026-08-25T12:30:00Z"));
        let resolved = target(
            "https://api.x.ai/v1",
            ProviderDialect::XaiChatCompletions,
            "grok-4",
        );
        let truth = project(&LaunchInputs {
            account: &account,
            credential_provider_id: "xai",
            refresh_material_present: true,
            target: ModelTargetOutcome::Resolved(&resolved),
        });
        assert_eq!(truth.readiness, LaunchReadiness::Ready);
        assert_eq!(truth.reason, LaunchReason::ResolvedWithFutureExpiry);
        assert_eq!(truth.provider, ProviderClass::Xai);
        assert_eq!(truth.route, RouteClass::XaiFirstParty);
        assert_eq!(truth.base, BaseCategory::XaiOfficial);
        assert_eq!(truth.dialect, RequestDialect::XaiChatCompletions);
        assert_eq!(truth.model.status, ModelStatus::Selected);
        assert_eq!(truth.validate(), Ok(()));
        assert!(truth.permits_launch());
    }

    #[test]
    fn an_unresolved_model_selection_refuses_and_reports_no_endpoint() {
        let account = account(Some("oidc"), Some("2026-08-25T12:30:00Z"));
        for (outcome, expected_status, expected_reason) in [
            (
                ModelTargetOutcome::NotSelected,
                ModelStatus::NotSelected,
                LaunchReason::ModelNotSelected,
            ),
            (
                ModelTargetOutcome::Unparseable,
                ModelStatus::Unparseable,
                LaunchReason::ModelSelectionUnparseable,
            ),
            (
                ModelTargetOutcome::ProviderMismatch,
                ModelStatus::RouteMismatch,
                LaunchReason::ModelRouteMismatch,
            ),
            (
                ModelTargetOutcome::NotOffered,
                ModelStatus::NotOffered,
                LaunchReason::ModelNotOffered,
            ),
        ] {
            let truth = project(&LaunchInputs {
                account: &account,
                credential_provider_id: "xai",
                refresh_material_present: true,
                target: outcome,
            });
            assert_eq!(truth.model.status, expected_status);
            assert_eq!(truth.reason, expected_reason, "{expected_status:?}");
            assert!(!truth.permits_launch());
            // Nothing was resolved, so nothing may be claimed about the route.
            assert_eq!(truth.base, BaseCategory::Unset);
            assert_eq!(truth.dialect, RequestDialect::Unrecognized);
            assert_eq!(truth.capabilities, CapabilityFacts::unprobed());
            assert!(truth.model.selected.is_none());
            assert_eq!(truth.validate(), Ok(()));
        }
    }

    #[test]
    fn a_model_id_the_projection_cannot_bound_never_reaches_a_provider() {
        let account = account(Some("oidc"), Some("2026-08-25T12:30:00Z"));
        let hostile = target(
            "https://api.x.ai/v1",
            ProviderDialect::XaiChatCompletions,
            "grok-4 <script>alert(1)</script>",
        );
        let truth = project(&LaunchInputs {
            account: &account,
            credential_provider_id: "xai",
            refresh_material_present: true,
            target: ModelTargetOutcome::Resolved(&hostile),
        });
        assert_eq!(truth.model.status, ModelStatus::Unparseable);
        assert_eq!(truth.reason, LaunchReason::ModelSelectionUnparseable);
        assert!(!truth.permits_launch());
        let encoded = serde_json::to_string(&truth).expect("truth serializes");
        assert!(
            !encoded.contains("script"),
            "leaked an unbounded model id: {encoded}"
        );
    }

    #[test]
    fn the_adapter_never_projects_credential_or_endpoint_material() {
        let account = account(Some("oidc"), Some("2026-08-25T12:30:00Z"));
        let resolved = target(
            &format!("https://{SENTINEL_HOST}/v1"),
            ProviderDialect::OpenAiChatCompletions,
            "corp-model-1",
        );
        let truth = project(&LaunchInputs {
            account: &account,
            credential_provider_id: "corp",
            refresh_material_present: true,
            target: ModelTargetOutcome::Resolved(&resolved),
        });
        let encoded = serde_json::to_string(&truth).expect("truth serializes");
        for needle in [
            SENTINEL_BEARER,
            SENTINEL_REFRESH,
            SENTINEL_HOST,
            "corp.example",
            "https://",
            "refresh_token",
            "refreshToken",
            "Bearer",
            "operator@example.test",
            "auth_scope",
            "provider_id",
            "credential_ref",
        ] {
            assert!(
                !encoded.contains(needle),
                "adapter leaked {needle:?}: {encoded}"
            );
        }
        // The endpoint survives only as a category, and the model as a bounded id.
        assert_eq!(truth.base, BaseCategory::CompatibleHttps);
        assert_eq!(
            truth
                .model
                .selected
                .as_ref()
                .map(|model| model.value.as_str()),
            Some("corp-model-1")
        );
    }

    #[test]
    fn enforce_refuses_a_truth_that_is_not_ready_without_touching_attribution() {
        let account = account(Some("oidc"), Some("2026-08-24T23:59:59Z"));
        let resolved = target(
            "https://api.x.ai/v1",
            ProviderDialect::XaiChatCompletions,
            "grok-4",
        );
        let truth = project(&LaunchInputs {
            account: &account,
            credential_provider_id: "xai",
            refresh_material_present: true,
            target: ModelTargetOutcome::Resolved(&resolved),
        });
        assert_eq!(
            enforce(truth, None).unwrap_err(),
            LaunchReason::RefreshFailed
        );
    }

    #[test]
    fn enforce_pins_a_first_admission_and_refuses_any_later_drift() {
        let account = account(Some("oidc"), Some("2026-08-25T12:30:00Z"));
        let resolved = target(
            "https://api.x.ai/v1",
            ProviderDialect::XaiChatCompletions,
            "grok-4",
        );
        let inputs = LaunchInputs {
            account: &account,
            credential_provider_id: "xai",
            refresh_material_present: true,
            target: ModelTargetOutcome::Resolved(&resolved),
        };
        let admitted = enforce(project(&inputs), None).expect("first admission is granted");
        assert_eq!(
            admitted.attribution.credential_method,
            CredentialMethod::GrokBuildOidc
        );

        // Re-resolving identically must still satisfy the pinned requirement.
        enforce(project(&inputs), Some(&admitted.requirement))
            .expect("an unchanged re-resolution stays admissible");

        // The operator switched models between admission and start.
        let switched = target(
            "https://api.x.ai/v1",
            ProviderDialect::XaiChatCompletions,
            "grok-3",
        );
        let drifted = project(&LaunchInputs {
            target: ModelTargetOutcome::Resolved(&switched),
            ..inputs
        });
        assert!(
            enforce(drifted, Some(&admitted.requirement)).is_err(),
            "a model switch was admitted against the pinned requirement"
        );

        // The endpoint was re-pointed at a corporate proxy.
        let repointed = target(
            "https://corp.example/v1",
            ProviderDialect::XaiChatCompletions,
            "grok-4",
        );
        let drifted = project(&LaunchInputs {
            target: ModelTargetOutcome::Resolved(&repointed),
            ..inputs
        });
        assert!(
            enforce(drifted, Some(&admitted.requirement)).is_err(),
            "an endpoint re-point was admitted against the pinned requirement"
        );
    }

    #[test]
    fn a_doctored_projection_is_refused_before_it_can_grant_attribution() {
        let account = account(Some("oidc"), Some("2026-08-25T12:30:00Z"));
        let resolved = target(
            "https://api.x.ai/v1",
            ProviderDialect::XaiChatCompletions,
            "grok-4",
        );
        let ready = project(&LaunchInputs {
            account: &account,
            credential_provider_id: "xai",
            refresh_material_present: true,
            target: ModelTargetOutcome::Resolved(&resolved),
        });
        // Ready verdict, unprobed capabilities: exactly what a forged
        // projection would look like. `validate` catches it, so no attribution
        // is ever handed out.
        let forged = GrokLaunchTruth {
            capabilities: CapabilityFacts::unprobed(),
            ..ready
        };
        assert!(enforce(forged, None).is_err());
    }

    #[test]
    fn every_refusal_records_a_typed_non_success_verdict() {
        for reason in grokptah_agent_sdk::launch::LaunchReason::ALL {
            let verdict = refusal_verdict(reason);
            assert!(
                !verdict.claims_success(),
                "{reason:?} produced a success-claiming verdict"
            );
            assert!(verdict.retains_transcript_help());
        }
    }
}
