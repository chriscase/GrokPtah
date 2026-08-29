//! Authoritative, credential-free Grok Build *launch truth*.
//!
//! [`crate::account`] answers a narrow question — is there a credential, and
//! has it visibly expired. That is not enough to admit durable work. A run
//! also needs a provider, a route, a base endpoint category, a request
//! dialect, a selected model, and capability evidence, and every one of those
//! has to be *known* before the host may promise anything.
//!
//! This module is that stricter answer. Where [`crate::account`] is
//! deliberately permissive (an unknown fact never blocks), this module is
//! deliberately fail-closed: **unknown, unrecognized, unparseable, and
//! unprobed all block**. The only exception is stated positively rather than
//! inferred — a resolved API-key route may report
//! [`LaunchReason::ResolvedApiKeyNoExpiryClaim`], which says "this route
//! carries no expiry claim at all", not "expiry is unknown".
//!
//! # Non-goals (enforced structurally)
//!
//! Nothing here can carry a bearer, refresh token, API key, keychain
//! reference, credential fingerprint, endpoint URL, hostname, account email,
//! or display name. Every descriptive field is a closed Rust enum with no
//! free-form variant, and the two string-bearing types
//! ([`crate::account::AccountReference`] and [`ModelReference`]) are charset-
//! and length-bounded. An input this module does not recognize collapses to
//! the corresponding `Unrecognized` variant and the raw text is dropped.
//!
//! These facts describe *local* state. They never claim account balance,
//! remaining quota, entitlement, or live provider certification: only a
//! provider round-trip establishes those, and this module performs none.

use serde::{Deserialize, Serialize};

use crate::account::{
    AccountReference, CredentialMethod, ExpiryFacts, ExpiryStatus, GrokAccountFacts, RunAttribution,
};

/// Stable contract identifier for the launch truth projection.
pub const GROK_LAUNCH_CONTRACT_VERSION: &str = "grokptah.launch.v1";
/// Numeric schema revision carried in every launch projection.
pub const GROK_LAUNCH_SCHEMA_VERSION: u32 = 1;
/// Maximum UTF-8 bytes accepted in a public model reference.
pub const MAX_MODEL_REFERENCE_BYTES: usize = 128;

/// Closed vocabulary for *which provider family* serves a run.
///
/// Deliberately not a provider id: a profile id is user-authored text and has
/// no place in a published projection. The family is what a launch decision
/// actually depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderClass {
    /// The built-in first-party xAI provider.
    Xai,
    /// A configured OpenAI-compatible provider profile.
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible,
    /// A provider the projection could not classify. Blocks.
    Unrecognized,
}

impl ProviderClass {
    /// The exact wire value used by the JSON contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Xai => "xai",
            Self::OpenAiCompatible => "openai_compatible",
            Self::Unrecognized => "unrecognized",
        }
    }

    /// Every class in declaration order, for schema and parity pinning.
    pub const ALL: [Self; 3] = [Self::Xai, Self::OpenAiCompatible, Self::Unrecognized];
}

/// Closed vocabulary for *how a request is routed* to that provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteClass {
    /// First-party xAI route, reached with an xAI credential.
    XaiFirstParty,
    /// Configured compatible-provider route with its own credential.
    CompatibleProvider,
    /// A route the projection could not classify. Blocks.
    Unrecognized,
}

impl RouteClass {
    /// The exact wire value used by the JSON contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::XaiFirstParty => "xai_first_party",
            Self::CompatibleProvider => "compatible_provider",
            Self::Unrecognized => "unrecognized",
        }
    }

    /// Every class in declaration order, for schema and parity pinning.
    pub const ALL: [Self; 3] = [
        Self::XaiFirstParty,
        Self::CompatibleProvider,
        Self::Unrecognized,
    ];

    /// The provider family this route is allowed to reach.
    ///
    /// A route and a provider that disagree are a misconfiguration, not a
    /// detail: an xAI credential must never be spent against a corporate
    /// endpoint, and a compatible-provider credential must never be spent
    /// against first-party xAI.
    pub const fn expected_provider(self) -> ProviderClass {
        match self {
            Self::XaiFirstParty => ProviderClass::Xai,
            Self::CompatibleProvider => ProviderClass::OpenAiCompatible,
            Self::Unrecognized => ProviderClass::Unrecognized,
        }
    }
}

/// Closed classification of the base endpoint, carrying no URL.
///
/// The endpoint itself is host configuration and may embed a private
/// hostname, a tenant name, or a path secret, so it is never published. What
/// a launch decision needs is only its *category*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseCategory {
    /// The official first-party xAI API base.
    XaiOfficial,
    /// A compatible provider reached over public HTTPS.
    CompatibleHttps,
    /// A compatible provider on loopback (a local gateway or proxy).
    CompatibleLoopback,
    /// No base endpoint is configured at all. Blocks.
    Unset,
    /// A base endpoint that is not HTTPS and not loopback. Blocks.
    InsecureTransport,
    /// A base endpoint that could not be parsed. Blocks.
    Malformed,
}

impl BaseCategory {
    /// The exact wire value used by the JSON contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::XaiOfficial => "xai_official",
            Self::CompatibleHttps => "compatible_https",
            Self::CompatibleLoopback => "compatible_loopback",
            Self::Unset => "unset",
            Self::InsecureTransport => "insecure_transport",
            Self::Malformed => "malformed",
        }
    }

    /// Every category in declaration order, for schema and parity pinning.
    pub const ALL: [Self; 6] = [
        Self::XaiOfficial,
        Self::CompatibleHttps,
        Self::CompatibleLoopback,
        Self::Unset,
        Self::InsecureTransport,
        Self::Malformed,
    ];

    /// Whether this category may carry a durable run.
    pub const fn is_launchable(self) -> bool {
        matches!(
            self,
            Self::XaiOfficial | Self::CompatibleHttps | Self::CompatibleLoopback
        )
    }
}

/// Closed vocabulary for the exact request dialect a run will speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestDialect {
    /// xAI chat completions.
    XaiChatCompletions,
    /// OpenAI-compatible chat completions.
    #[serde(rename = "openai_chat_completions")]
    OpenAiChatCompletions,
    /// A dialect the projection could not classify. Blocks.
    Unrecognized,
}

impl RequestDialect {
    /// The exact wire value used by the JSON contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::XaiChatCompletions => "xai_chat_completions",
            Self::OpenAiChatCompletions => "openai_chat_completions",
            Self::Unrecognized => "unrecognized",
        }
    }

    /// Every dialect in declaration order, for schema and parity pinning.
    pub const ALL: [Self; 3] = [
        Self::XaiChatCompletions,
        Self::OpenAiChatCompletions,
        Self::Unrecognized,
    ];

    /// Whether this dialect's published contract defines an idempotency key.
    ///
    /// A fact about the *contract*, not about any particular endpoint: it is
    /// decided from the closed dialect vocabulary alone, never measured, and
    /// never read from a response. Sending the header where it is not part of
    /// the contract would be a claim this host cannot support — the request
    /// would be replayable exactly as if no key had been sent, while the
    /// durable record implied the provider could recognise the duplicate.
    ///
    /// `OpenAiChatCompletions` is the *generic compatible-gateway* dialect
    /// here (see `ProviderKind::OpenAiCompatible`). An arbitrary compatible
    /// gateway promises nothing about idempotency, so this returns `false` and
    /// the host falls back to the protection it can actually enforce: an
    /// unreconciled attempt is never retried automatically. That is weaker
    /// than provider-side deduplication and is deliberately not disguised as
    /// equivalent.
    pub const fn permits_idempotency_key(self) -> bool {
        match self {
            Self::XaiChatCompletions => true,
            Self::OpenAiChatCompletions | Self::Unrecognized => false,
        }
    }
}

/// Whether the resolved credential can be renewed without a human.
///
/// This is a fact about the *route*, established from the presence of durable
/// refresh machinery. It is never derived from a token body, and the refresh
/// token itself is never read into this projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refreshability {
    /// Durable refresh material and an issuer are both present.
    Refreshable,
    /// The route has no refresh path; renewal needs a human.
    NotRefreshable,
    /// The route was not classified, so no refresh claim is made. Blocks.
    Unknown,
}

impl Refreshability {
    /// The exact wire value used by the JSON contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Refreshable => "refreshable",
            Self::NotRefreshable => "not_refreshable",
            Self::Unknown => "unknown",
        }
    }

    /// Every value in declaration order, for schema and parity pinning.
    pub const ALL: [Self; 3] = [Self::Refreshable, Self::NotRefreshable, Self::Unknown];
}

/// A bounded, non-secret model identifier.
///
/// Model ids are provider-published and safe to show, but they arrive through
/// user-editable configuration, so they are charset- and length-bounded
/// before publication exactly like an account reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelReference {
    /// Opaque provider model identifier, charset- and length-bounded.
    pub value: String,
}

impl ModelReference {
    /// Accept a model id only when it is safe to publish verbatim.
    ///
    /// Rejects empty, over-long, and non-opaque values (anything outside
    /// `[A-Za-z0-9._:/-]`) so no control characters, whitespace, or markup can
    /// reach a UI, a receipt, or an accessibility tree.
    ///
    /// `/` is permitted because provider-namespaced ids like
    /// `openai/gpt-4o-mini` are ordinary, but a value that *reads* as a path —
    /// one that starts or ends with a separator, or contains `..` — is
    /// refused. Model ids are never used as paths here; the point is that a
    /// traversal-shaped string must not be publishable as a model name.
    pub fn new(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.is_empty() || value.len() > MAX_MODEL_REFERENCE_BYTES {
            return None;
        }
        if !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        }) {
            return None;
        }
        if value.contains("..") {
            return None;
        }
        let edge = |byte: u8| matches!(byte, b'/' | b'.' | b':' | b'-' | b'_');
        if value.bytes().next().is_some_and(edge) || value.bytes().next_back().is_some_and(edge) {
            return None;
        }
        Some(Self {
            value: value.to_string(),
        })
    }
}

/// What the projection could establish about the selected model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelStatus {
    /// A bounded model id was selected and belongs to the resolved route.
    Selected,
    /// No model is selected at all. Blocks.
    NotSelected,
    /// A selection exists but could not be parsed into provider + model.
    /// Blocks.
    Unparseable,
    /// The selection parsed but names a different provider than the resolved
    /// route. Blocks.
    RouteMismatch,
    /// The selection parsed and matches the route, but the provider does not
    /// offer that model. Blocks.
    NotOffered,
}

impl ModelStatus {
    /// The exact wire value used by the JSON contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Selected => "selected",
            Self::NotSelected => "not_selected",
            Self::Unparseable => "unparseable",
            Self::RouteMismatch => "route_mismatch",
            Self::NotOffered => "not_offered",
        }
    }

    /// Every status in declaration order, for schema and parity pinning.
    pub const ALL: [Self; 5] = [
        Self::Selected,
        Self::NotSelected,
        Self::Unparseable,
        Self::RouteMismatch,
        Self::NotOffered,
    ];
}

/// Parsed model evidence for the selected route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelFacts {
    /// What the projection could establish.
    pub status: ModelStatus,
    /// Bounded model id, present only when [`ModelStatus::Selected`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<ModelReference>,
}

impl ModelFacts {
    /// Model evidence that is honest about having no selection.
    pub const fn not_selected() -> Self {
        Self {
            status: ModelStatus::NotSelected,
            selected: None,
        }
    }

    /// Model evidence for a selection that could not be parsed.
    pub const fn unparseable() -> Self {
        Self {
            status: ModelStatus::Unparseable,
            selected: None,
        }
    }

    /// Model evidence for a selection naming another provider.
    pub const fn route_mismatch() -> Self {
        Self {
            status: ModelStatus::RouteMismatch,
            selected: None,
        }
    }

    /// Model evidence for a selection the provider does not offer.
    pub const fn not_offered() -> Self {
        Self {
            status: ModelStatus::NotOffered,
            selected: None,
        }
    }

    /// Model evidence for a bounded selection that matches the route.
    pub fn selected(reference: ModelReference) -> Self {
        Self {
            status: ModelStatus::Selected,
            selected: Some(reference),
        }
    }

    /// Reject evidence whose parts disagree with its status.
    ///
    /// Without this a producer could report "no model selected" while still
    /// attaching one, which is exactly the unbounded text this projection
    /// exists to strip.
    pub fn validate(&self) -> Result<(), &'static str> {
        match self.status {
            ModelStatus::Selected => match &self.selected {
                None => Err("a selected model must carry a bounded reference"),
                Some(reference)
                    if ModelReference::new(&reference.value).as_ref() != Some(reference) =>
                {
                    Err("model reference is not a bounded opaque identifier")
                }
                Some(_) => Ok(()),
            },
            _ if self.selected.is_some() => Err("an unselected model must not carry a reference"),
            _ => Ok(()),
        }
    }
}

/// Where a capability statement came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityProvenance {
    /// Observed by an actual provider probe against this exact model.
    Measured,
    /// Declared by the catalog or the profile without a probe.
    Declared,
    /// Never established. Blocks.
    Unprobed,
}

impl CapabilityProvenance {
    /// The exact wire value used by the JSON contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Measured => "measured",
            Self::Declared => "declared",
            Self::Unprobed => "unprobed",
        }
    }

    /// Every provenance in declaration order, for schema and parity pinning.
    pub const ALL: [Self; 3] = [Self::Measured, Self::Declared, Self::Unprobed];
}

/// Capability evidence for the exact provider/model pair a run will use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityFacts {
    /// Where these statements came from.
    pub provenance: CapabilityProvenance,
    /// Whether the model can serve a chat completion at all.
    pub chat: bool,
    /// Whether the model accepts tool definitions.
    pub tools: bool,
    /// Whether the model can stream a response.
    pub stream: bool,
    /// Whether the model accepts parallel tool calls.
    pub parallel_tool_calls: bool,
    /// Whether the model accepts image input.
    pub image_input: bool,
}

impl CapabilityFacts {
    /// Capability evidence that is honest about never having probed.
    pub const fn unprobed() -> Self {
        Self {
            provenance: CapabilityProvenance::Unprobed,
            chat: false,
            tools: false,
            stream: false,
            parallel_tool_calls: false,
            image_input: false,
        }
    }

    /// Reject evidence that claims capabilities it never established.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.provenance == CapabilityProvenance::Unprobed
            && (self.chat
                || self.tools
                || self.stream
                || self.parallel_tool_calls
                || self.image_input)
        {
            return Err("unprobed capabilities must not assert any capability");
        }
        Ok(())
    }
}

/// Whether the host may admit a durable Grok Build run right now.
///
/// Unlike [`crate::account::AccountReadiness`], this verdict is fail-closed:
/// only [`LaunchReadiness::Ready`] permits admission. Both
/// [`LaunchReadiness::Blocked`] and [`LaunchReadiness::Indeterminate`] refuse,
/// and they are distinct so an operator can tell "this is wrong" from "this is
/// unestablished".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchReadiness {
    /// Every required fact is established and consistent.
    Ready,
    /// Positive evidence a launch cannot succeed.
    Blocked,
    /// A required fact is unknown, unrecognized, unparseable, or unprobed.
    Indeterminate,
}

impl LaunchReadiness {
    /// The exact wire value used by the JSON contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Blocked => "blocked",
            Self::Indeterminate => "indeterminate",
        }
    }

    /// Every readiness state in declaration order, for parity pinning.
    pub const ALL: [Self; 3] = [Self::Ready, Self::Blocked, Self::Indeterminate];

    /// Whether a durable run may be admitted. Fail-closed by construction.
    pub const fn permits_launch(self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// Why a launch verdict was reached, in a closed and *actionable* vocabulary.
///
/// Every blocking variant names something an operator can do. "Something went
/// wrong" is not a reason, and there is no catch-all variant to fall back on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchReason {
    /// Ready: the credential carries a parsed expiry strictly in the future.
    ResolvedWithFutureExpiry,
    /// Ready: an API-key route resolved and carries no expiry claim at all.
    ///
    /// This is a positive statement about the route, not an admission of
    /// ignorance: API keys do not publish an expiry, so demanding one would
    /// block every key-based host forever.
    ResolvedApiKeyNoExpiryClaim,
    /// Sign in, or set an API key: no credential resolved on any route.
    SignInRequired,
    /// Sign in again: a parsed expiry is at or before the observation instant
    /// and the route cannot refresh itself.
    ReauthenticationRequired,
    /// Retry or sign in again: the credential expired and refresh failed.
    RefreshFailed,
    /// Sign in again: the provider rejected the credential as revoked.
    CredentialRevoked,
    /// Reconfigure the credential: its route could not be classified.
    CredentialRouteUnrecognized,
    /// Reconfigure the credential: an expiry field was present but unreadable.
    ExpiryUnparseable,
    /// Sign in again: a session route carried no expiry at all, and only an
    /// API-key route is allowed to make no expiry claim.
    ExpiryNotEstablished,
    /// Reconfigure the profile: the provider family could not be classified.
    ProviderUnrecognized,
    /// Reconfigure the profile: the request route could not be classified.
    RouteUnrecognized,
    /// Reconfigure the profile: the credential route and the request route
    /// name different providers.
    RouteProviderMismatch,
    /// Configure a base endpoint: none is set for this profile.
    BaseEndpointUnset,
    /// Fix the base endpoint: it is neither HTTPS nor loopback.
    BaseEndpointInsecure,
    /// Fix the base endpoint: it could not be parsed.
    BaseEndpointMalformed,
    /// Reconfigure the profile: the request dialect could not be classified.
    DialectUnrecognized,
    /// Select a model: none is selected.
    ModelNotSelected,
    /// Reselect the model: the stored selection could not be parsed.
    ModelSelectionUnparseable,
    /// Reselect the model: it belongs to a different provider than the route.
    ModelRouteMismatch,
    /// Select another model: this provider does not offer the selected one.
    ModelNotOffered,
    /// Probe the model: its capabilities were never established.
    CapabilitiesUnprobed,
    /// Select another model: this one cannot serve a chat completion.
    ChatUnsupported,
    /// Reconfigure the credential: its refresh behaviour is unestablished.
    RefreshabilityUnknown,
}

impl LaunchReason {
    /// The exact wire value used by the JSON contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ResolvedWithFutureExpiry => "resolved_with_future_expiry",
            Self::ResolvedApiKeyNoExpiryClaim => "resolved_api_key_no_expiry_claim",
            Self::SignInRequired => "sign_in_required",
            Self::ReauthenticationRequired => "reauthentication_required",
            Self::RefreshFailed => "refresh_failed",
            Self::CredentialRevoked => "credential_revoked",
            Self::CredentialRouteUnrecognized => "credential_route_unrecognized",
            Self::ExpiryUnparseable => "expiry_unparseable",
            Self::ExpiryNotEstablished => "expiry_not_established",
            Self::ProviderUnrecognized => "provider_unrecognized",
            Self::RouteUnrecognized => "route_unrecognized",
            Self::RouteProviderMismatch => "route_provider_mismatch",
            Self::BaseEndpointUnset => "base_endpoint_unset",
            Self::BaseEndpointInsecure => "base_endpoint_insecure",
            Self::BaseEndpointMalformed => "base_endpoint_malformed",
            Self::DialectUnrecognized => "dialect_unrecognized",
            Self::ModelNotSelected => "model_not_selected",
            Self::ModelSelectionUnparseable => "model_selection_unparseable",
            Self::ModelRouteMismatch => "model_route_mismatch",
            Self::ModelNotOffered => "model_not_offered",
            Self::CapabilitiesUnprobed => "capabilities_unprobed",
            Self::ChatUnsupported => "chat_unsupported",
            Self::RefreshabilityUnknown => "refreshability_unknown",
        }
    }

    /// Every reason in declaration order, for schema and parity pinning.
    pub const ALL: [Self; 23] = [
        Self::ResolvedWithFutureExpiry,
        Self::ResolvedApiKeyNoExpiryClaim,
        Self::SignInRequired,
        Self::ReauthenticationRequired,
        Self::RefreshFailed,
        Self::CredentialRevoked,
        Self::CredentialRouteUnrecognized,
        Self::ExpiryUnparseable,
        Self::ExpiryNotEstablished,
        Self::ProviderUnrecognized,
        Self::RouteUnrecognized,
        Self::RouteProviderMismatch,
        Self::BaseEndpointUnset,
        Self::BaseEndpointInsecure,
        Self::BaseEndpointMalformed,
        Self::DialectUnrecognized,
        Self::ModelNotSelected,
        Self::ModelSelectionUnparseable,
        Self::ModelRouteMismatch,
        Self::ModelNotOffered,
        Self::CapabilitiesUnprobed,
        Self::ChatUnsupported,
        Self::RefreshabilityUnknown,
    ];

    /// The readiness this reason implies. Only two reasons are ever ready.
    pub const fn readiness(self) -> LaunchReadiness {
        match self {
            Self::ResolvedWithFutureExpiry | Self::ResolvedApiKeyNoExpiryClaim => {
                LaunchReadiness::Ready
            }
            // Positive evidence the launch cannot work.
            Self::SignInRequired
            | Self::ReauthenticationRequired
            | Self::RefreshFailed
            | Self::CredentialRevoked
            | Self::BaseEndpointInsecure
            | Self::ModelNotSelected
            | Self::ModelRouteMismatch
            | Self::ModelNotOffered
            | Self::RouteProviderMismatch
            | Self::BaseEndpointUnset
            | Self::ChatUnsupported => LaunchReadiness::Blocked,
            // A required fact was never established.
            Self::CredentialRouteUnrecognized
            | Self::ExpiryUnparseable
            | Self::ExpiryNotEstablished
            | Self::ProviderUnrecognized
            | Self::RouteUnrecognized
            | Self::BaseEndpointMalformed
            | Self::DialectUnrecognized
            | Self::ModelSelectionUnparseable
            | Self::CapabilitiesUnprobed
            | Self::RefreshabilityUnknown => LaunchReadiness::Indeterminate,
        }
    }
}

/// Non-secret observation of one fully resolved route, before projection.
///
/// Host adapters build this. It has no field that can carry credential
/// material or an endpoint URL: the adapter classifies the endpoint itself and
/// hands over only the resulting [`BaseCategory`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchObservation<'a> {
    /// Provider family behind the resolved credential.
    pub provider: ProviderClass,
    /// How a request will be routed.
    pub route: RouteClass,
    /// Category of the configured base endpoint.
    pub base: BaseCategory,
    /// Exact request dialect.
    pub dialect: RequestDialect,
    /// Whether the resolved credential can renew itself.
    pub refreshability: Refreshability,
    /// Model evidence for this exact route.
    pub model: ModelFacts,
    /// Capability evidence for this exact provider/model pair.
    pub capabilities: CapabilityFacts,
    /// Credential-free account facts, already projected.
    pub account: &'a GrokAccountFacts,
}

/// Versioned, credential-free, fail-closed Grok Build launch truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrokLaunchTruth {
    /// Stable contract identifier, always [`GROK_LAUNCH_CONTRACT_VERSION`].
    pub contract: String,
    /// Numeric schema revision, always [`GROK_LAUNCH_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Provider family behind the resolved credential.
    pub provider: ProviderClass,
    /// Closed-vocabulary credential route, from [`crate::account`].
    pub credential_method: CredentialMethod,
    /// Whether the resolved credential can renew itself.
    pub refreshability: Refreshability,
    /// Parsed expiry evidence, from [`crate::account`].
    pub expiry: ExpiryFacts,
    /// Bounded non-secret account handle, when durable identity is present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_reference: Option<AccountReference>,
    /// How a request will be routed.
    pub route: RouteClass,
    /// Category of the configured base endpoint. Never carries a URL.
    pub base: BaseCategory,
    /// Exact request dialect.
    pub dialect: RequestDialect,
    /// Model evidence for this exact route.
    pub model: ModelFacts,
    /// Capability evidence for this exact provider/model pair.
    pub capabilities: CapabilityFacts,
    /// Fail-closed launch verdict.
    pub readiness: LaunchReadiness,
    /// Closed, actionable reason the verdict holds.
    pub reason: LaunchReason,
}

impl GrokLaunchTruth {
    /// Launch truth for a host with no credential on any route.
    pub fn unresolved() -> Self {
        let account = GrokAccountFacts::absent();
        Self {
            contract: GROK_LAUNCH_CONTRACT_VERSION.to_string(),
            schema_version: GROK_LAUNCH_SCHEMA_VERSION,
            provider: ProviderClass::Unrecognized,
            credential_method: CredentialMethod::Absent,
            refreshability: Refreshability::Unknown,
            expiry: account.expiry.clone(),
            account_reference: None,
            route: RouteClass::Unrecognized,
            base: BaseCategory::Unset,
            dialect: RequestDialect::Unrecognized,
            model: ModelFacts::not_selected(),
            capabilities: CapabilityFacts::unprobed(),
            readiness: LaunchReadiness::Blocked,
            reason: LaunchReason::SignInRequired,
        }
    }

    /// Project fail-closed launch truth from a fully resolved observation.
    pub fn project(observation: &LaunchObservation<'_>) -> Self {
        let account = observation.account;
        let reason = decide(observation);
        Self {
            contract: GROK_LAUNCH_CONTRACT_VERSION.to_string(),
            schema_version: GROK_LAUNCH_SCHEMA_VERSION,
            provider: observation.provider,
            credential_method: account.credential_method,
            refreshability: observation.refreshability,
            expiry: account.expiry.clone(),
            account_reference: account.account_reference.clone(),
            route: observation.route,
            base: observation.base,
            dialect: observation.dialect,
            model: observation.model.clone(),
            capabilities: observation.capabilities,
            readiness: reason.readiness(),
            reason,
        }
    }

    /// Whether the host may admit a durable run against this truth.
    pub const fn permits_launch(&self) -> bool {
        self.readiness.permits_launch()
    }

    /// Bounded public attribution for a run started against this truth.
    pub fn attribution(&self) -> RunAttribution {
        RunAttribution {
            credential_method: self.credential_method,
            account_reference: self.account_reference.clone(),
        }
    }

    /// The exact facts a durable admission must still match at start time.
    pub fn requirement(&self) -> LaunchRequirement {
        LaunchRequirement {
            provider: self.provider,
            credential_method: self.credential_method,
            route: self.route,
            base: self.base,
            dialect: self.dialect,
            model: self.model.selected.clone(),
            account_reference: self.account_reference.clone(),
        }
    }

    /// Validate the bounded public projection before publishing it.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.contract != GROK_LAUNCH_CONTRACT_VERSION {
            return Err("launch contract identifier does not match this revision");
        }
        if self.schema_version != GROK_LAUNCH_SCHEMA_VERSION {
            return Err("launch schema version does not match this revision");
        }
        if let Some(reference) = &self.account_reference
            && AccountReference::new(&reference.value, reference.source).as_ref() != Some(reference)
        {
            return Err("account reference is not a bounded opaque identifier");
        }
        self.expiry.validate()?;
        self.model.validate()?;
        self.capabilities.validate()?;
        if self.readiness != self.reason.readiness() {
            return Err("launch readiness does not follow from its reason");
        }
        // A ready verdict is the only one that promises anything, so it is the
        // only one re-derived here in full.
        if self.readiness == LaunchReadiness::Ready {
            // Rebuild the v1 account facts from the published parts rather
            // than asserting a shape, so a doctored projection cannot smuggle
            // a ready verdict past this check.
            let (account_readiness, account_reason) = match self.expiry.status {
                ExpiryStatus::Valid => (
                    crate::account::AccountReadiness::Usable,
                    crate::account::ReadinessReason::ExpiryInFuture,
                ),
                _ => (
                    crate::account::AccountReadiness::Unknown,
                    crate::account::ReadinessReason::ExpiryNotProvided,
                ),
            };
            let account = GrokAccountFacts {
                contract: crate::account::GROK_ACCOUNT_CONTRACT_VERSION.to_string(),
                schema_version: crate::account::GROK_ACCOUNT_SCHEMA_VERSION,
                credential_method: self.credential_method,
                account_reference: self.account_reference.clone(),
                expiry: self.expiry.clone(),
                readiness: account_readiness,
                readiness_reason: account_reason,
            };
            let observation = LaunchObservation {
                provider: self.provider,
                route: self.route,
                base: self.base,
                dialect: self.dialect,
                refreshability: self.refreshability,
                model: self.model.clone(),
                capabilities: self.capabilities,
                account: &account,
            };
            if decide(&observation).readiness() != LaunchReadiness::Ready {
                return Err("a ready verdict does not follow from these facts");
            }
        }
        Ok(())
    }

    /// Re-check that freshly re-resolved truth still matches an earlier
    /// requirement, and is still ready.
    ///
    /// This is the durable-admission gate: the host resolves once to decide,
    /// then resolves again immediately before writing a run record, and both
    /// answers must agree on every fact that determines where the run's
    /// tokens are spent.
    pub fn enforce(&self, requirement: &LaunchRequirement) -> Result<(), LaunchReason> {
        if !self.permits_launch() {
            return Err(self.reason);
        }
        if self.provider != requirement.provider {
            return Err(LaunchReason::ProviderUnrecognized);
        }
        if self.credential_method != requirement.credential_method {
            return Err(LaunchReason::CredentialRouteUnrecognized);
        }
        if self.route != requirement.route {
            return Err(LaunchReason::RouteUnrecognized);
        }
        if self.base != requirement.base {
            return Err(LaunchReason::BaseEndpointMalformed);
        }
        if self.dialect != requirement.dialect {
            return Err(LaunchReason::DialectUnrecognized);
        }
        if self.model.selected != requirement.model {
            return Err(LaunchReason::ModelRouteMismatch);
        }
        if self.account_reference != requirement.account_reference {
            return Err(LaunchReason::CredentialRouteUnrecognized);
        }
        Ok(())
    }
}

/// The exact facts a durable admission is pinned to.
///
/// Carries no credential material and no endpoint: it is the same closed
/// vocabulary as [`GrokLaunchTruth`], narrowed to the fields that decide
/// *where a run's tokens are spent*.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LaunchRequirement {
    /// Required provider family.
    pub provider: ProviderClass,
    /// Required credential route.
    pub credential_method: CredentialMethod,
    /// Required request route.
    pub route: RouteClass,
    /// Required base endpoint category.
    pub base: BaseCategory,
    /// Required request dialect.
    pub dialect: RequestDialect,
    /// Required bounded model reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelReference>,
    /// Required bounded account handle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_reference: Option<AccountReference>,
}

/// Decide the single most actionable reason for an observation.
///
/// Order matters: the operator is told the *first* thing they must fix, and
/// credential problems come before configuration problems because a signed-out
/// host cannot verify any of the rest.
fn decide(observation: &LaunchObservation<'_>) -> LaunchReason {
    let account = observation.account;

    // 1. Credential existence and validity.
    if account.credential_method == CredentialMethod::Absent {
        return LaunchReason::SignInRequired;
    }
    if account.credential_method == CredentialMethod::Unknown {
        return LaunchReason::CredentialRouteUnrecognized;
    }
    match account.expiry.status {
        ExpiryStatus::Expired => {
            return match observation.refreshability {
                // Refresh was available and the credential is still expired at
                // the observation instant, so refresh has already failed or
                // was never attempted; either way a human is not yet needed.
                Refreshability::Refreshable => LaunchReason::RefreshFailed,
                Refreshability::NotRefreshable => LaunchReason::ReauthenticationRequired,
                Refreshability::Unknown => LaunchReason::RefreshabilityUnknown,
            };
        }
        ExpiryStatus::Unparseable => return LaunchReason::ExpiryUnparseable,
        ExpiryStatus::Absent | ExpiryStatus::Valid => {}
    }
    if observation.refreshability == Refreshability::Unknown {
        return LaunchReason::RefreshabilityUnknown;
    }

    // 2. Provider and route, both read from the credential.
    if observation.provider == ProviderClass::Unrecognized {
        return LaunchReason::ProviderUnrecognized;
    }
    if observation.route == RouteClass::Unrecognized {
        return LaunchReason::RouteUnrecognized;
    }
    if observation.route.expected_provider() != observation.provider {
        return LaunchReason::RouteProviderMismatch;
    }

    // 3. Model selection, before the endpoint. The base, the dialect, and the
    //    capabilities are all *derived from* the resolved model target, so a
    //    selection that did not resolve leaves them unestablished. Reporting
    //    "no endpoint" there would name a symptom; the operator's actual next
    //    action is to fix the selection.
    match observation.model.status {
        ModelStatus::NotSelected => return LaunchReason::ModelNotSelected,
        ModelStatus::Unparseable => return LaunchReason::ModelSelectionUnparseable,
        ModelStatus::RouteMismatch => return LaunchReason::ModelRouteMismatch,
        ModelStatus::NotOffered => return LaunchReason::ModelNotOffered,
        ModelStatus::Selected if observation.model.selected.is_none() => {
            // A "selected" status with no bounded reference is a producer bug,
            // and an unbounded model id must never reach a run.
            return LaunchReason::ModelSelectionUnparseable;
        }
        ModelStatus::Selected => {}
    }

    // 4. Endpoint, dialect, and capabilities for that exact model.
    match observation.base {
        BaseCategory::Unset => return LaunchReason::BaseEndpointUnset,
        BaseCategory::InsecureTransport => return LaunchReason::BaseEndpointInsecure,
        BaseCategory::Malformed => return LaunchReason::BaseEndpointMalformed,
        BaseCategory::XaiOfficial
        | BaseCategory::CompatibleHttps
        | BaseCategory::CompatibleLoopback => {}
    }
    if observation.dialect == RequestDialect::Unrecognized {
        return LaunchReason::DialectUnrecognized;
    }
    if observation.capabilities.provenance == CapabilityProvenance::Unprobed {
        return LaunchReason::CapabilitiesUnprobed;
    }
    if !observation.capabilities.chat {
        return LaunchReason::ChatUnsupported;
    }

    // 5. Positive readiness. An API-key route publishes no expiry, so it is
    //    allowed to say so explicitly. Every other route must show a parsed,
    //    future expiry: a session with no expiry at all is an unestablished
    //    fact, not a licence to launch.
    match account.expiry.status {
        ExpiryStatus::Valid => LaunchReason::ResolvedWithFutureExpiry,
        ExpiryStatus::Absent if publishes_no_expiry(account.credential_method) => {
            LaunchReason::ResolvedApiKeyNoExpiryClaim
        }
        ExpiryStatus::Absent => LaunchReason::ExpiryNotEstablished,
        // Unreachable: both remaining statuses returned above.
        ExpiryStatus::Expired | ExpiryStatus::Unparseable => LaunchReason::ExpiryUnparseable,
    }
}

/// Whether a credential route structurally publishes no expiry.
///
/// Only long-lived API keys qualify. A rotating token command *does* have an
/// expiry, it just does not surface one here, so it stays indeterminate rather
/// than borrowing the API-key exemption; and every OIDC session route carries
/// an expiry by construction, so a missing one is a fact we failed to read.
const fn publishes_no_expiry(method: CredentialMethod) -> bool {
    matches!(
        method,
        CredentialMethod::ApiKey
            | CredentialMethod::ProviderEnv
            | CredentialMethod::ProviderKeychain
            | CredentialMethod::GrokBuildApiKey
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{
        AccountObservation, AccountReferenceSource, CredentialSource,
        GROK_ACCOUNT_CONTRACT_VERSION, GROK_ACCOUNT_SCHEMA_VERSION,
    };

    /// Only a dialect whose published contract defines an idempotency key may
    /// carry one.
    ///
    /// The generic compatible-gateway dialect must not: sending the header to
    /// a gateway that ignores it would leave the request as replayable as if
    /// no key had been sent, while every record downstream implied the
    /// provider could recognise the duplicate.
    #[test]
    fn only_a_published_idempotency_contract_permits_a_wire_key() {
        assert!(RequestDialect::XaiChatCompletions.permits_idempotency_key());
        assert!(!RequestDialect::OpenAiChatCompletions.permits_idempotency_key());
        assert!(!RequestDialect::Unrecognized.permits_idempotency_key());
        // An unclassified dialect blocks a launch outright, so it must never
        // be the one dialect that quietly gains a capability.
        assert!(
            RequestDialect::ALL
                .iter()
                .filter(|dialect| dialect.permits_idempotency_key())
                .all(|dialect| *dialect != RequestDialect::Unrecognized)
        );
    }

    /// Fixed observation clock: 2026-08-25T00:00:00Z, matching the account
    /// contract tests, so no verdict below reads the wall clock.
    const NOW: i64 = 1_787_616_000;
    /// Sentinels that must never survive projection into public output.
    const SENTINEL_BEARER: &str = "xai-SENTINEL-BEARER-DO-NOT-LEAK";
    const SENTINEL_REFRESH: &str = "xai-SENTINEL-REFRESH-DO-NOT-LEAK";
    const SENTINEL_BASE: &str = "https://internal-tenant-7.corp.example/v1";

    fn account(
        source: CredentialSource,
        auth_mode: Option<&str>,
        expires_at: Option<&str>,
    ) -> GrokAccountFacts {
        GrokAccountFacts::project(
            source,
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

    fn probed() -> CapabilityFacts {
        CapabilityFacts {
            provenance: CapabilityProvenance::Declared,
            chat: true,
            tools: true,
            stream: true,
            parallel_tool_calls: true,
            image_input: false,
        }
    }

    fn observation<'a>(account: &'a GrokAccountFacts) -> LaunchObservation<'a> {
        LaunchObservation {
            provider: ProviderClass::Xai,
            route: RouteClass::XaiFirstParty,
            base: BaseCategory::XaiOfficial,
            dialect: RequestDialect::XaiChatCompletions,
            refreshability: Refreshability::Refreshable,
            model: ModelFacts::selected(ModelReference::new("grok-4").expect("bounded id")),
            capabilities: probed(),
            account,
        }
    }

    #[test]
    fn a_fully_established_session_route_is_the_only_ready_shape() {
        let account = account(
            CredentialSource::GrokBuildSession,
            Some("oidc"),
            Some("2026-08-25T12:30:00Z"),
        );
        let truth = GrokLaunchTruth::project(&observation(&account));
        assert_eq!(truth.readiness, LaunchReadiness::Ready);
        assert_eq!(truth.reason, LaunchReason::ResolvedWithFutureExpiry);
        assert!(truth.permits_launch());
        assert_eq!(truth.validate(), Ok(()));
    }

    /// The central inversion against the account contract: there, an
    /// unestablished fact stayed permissive. Here every one of them refuses.
    #[test]
    fn every_unestablished_fact_refuses_a_launch() {
        let account = account(
            CredentialSource::GrokBuildSession,
            Some("oidc"),
            Some("2026-08-25T12:30:00Z"),
        );
        let cases: [(&str, LaunchObservation<'_>, LaunchReason); 8] = [
            (
                "unrecognized provider",
                LaunchObservation {
                    provider: ProviderClass::Unrecognized,
                    ..observation(&account)
                },
                LaunchReason::ProviderUnrecognized,
            ),
            (
                "unrecognized route",
                LaunchObservation {
                    route: RouteClass::Unrecognized,
                    ..observation(&account)
                },
                LaunchReason::RouteUnrecognized,
            ),
            (
                "malformed base",
                LaunchObservation {
                    base: BaseCategory::Malformed,
                    ..observation(&account)
                },
                LaunchReason::BaseEndpointMalformed,
            ),
            (
                "unrecognized dialect",
                LaunchObservation {
                    dialect: RequestDialect::Unrecognized,
                    ..observation(&account)
                },
                LaunchReason::DialectUnrecognized,
            ),
            (
                "unparseable model selection",
                LaunchObservation {
                    model: ModelFacts::unparseable(),
                    ..observation(&account)
                },
                LaunchReason::ModelSelectionUnparseable,
            ),
            (
                "unprobed capabilities",
                LaunchObservation {
                    capabilities: CapabilityFacts::unprobed(),
                    ..observation(&account)
                },
                LaunchReason::CapabilitiesUnprobed,
            ),
            (
                "unknown refreshability",
                LaunchObservation {
                    refreshability: Refreshability::Unknown,
                    ..observation(&account)
                },
                LaunchReason::RefreshabilityUnknown,
            ),
            (
                "unset base",
                LaunchObservation {
                    base: BaseCategory::Unset,
                    ..observation(&account)
                },
                LaunchReason::BaseEndpointUnset,
            ),
        ];
        for (name, observation, expected) in cases {
            let truth = GrokLaunchTruth::project(&observation);
            assert_eq!(truth.reason, expected, "{name} chose the wrong reason");
            assert!(!truth.permits_launch(), "{name} must not permit a launch");
            assert_ne!(
                truth.readiness,
                LaunchReadiness::Ready,
                "{name} must not be ready"
            );
            assert_eq!(
                truth.validate(),
                Ok(()),
                "{name} must still be a valid projection"
            );
        }
    }

    #[test]
    fn an_unrecognized_credential_route_never_earns_a_launch() {
        // The wire loader accepts any mode containing "oidc"; this projection
        // is stricter, so a crafted mode collapses to `unknown` and refuses.
        let account = account(
            CredentialSource::GrokBuildSession,
            Some("oidc-but-actually-mine"),
            Some("2026-08-25T12:30:00Z"),
        );
        assert_eq!(account.credential_method, CredentialMethod::Unknown);
        let truth = GrokLaunchTruth::project(&observation(&account));
        assert_eq!(truth.reason, LaunchReason::CredentialRouteUnrecognized);
        assert_eq!(truth.readiness, LaunchReadiness::Indeterminate);
        assert!(!truth.permits_launch());
    }

    #[test]
    fn only_an_api_key_route_may_report_no_expiry_claim() {
        for (source, auth_mode, expected) in [
            (
                CredentialSource::EnvApiKey,
                None,
                LaunchReason::ResolvedApiKeyNoExpiryClaim,
            ),
            (
                CredentialSource::KeychainApiKey,
                None,
                LaunchReason::ResolvedApiKeyNoExpiryClaim,
            ),
            (
                CredentialSource::ProviderEnv,
                None,
                LaunchReason::ResolvedApiKeyNoExpiryClaim,
            ),
            (
                CredentialSource::ProviderKeychain,
                None,
                LaunchReason::ResolvedApiKeyNoExpiryClaim,
            ),
            (
                CredentialSource::GrokBuildSession,
                Some("api_key"),
                LaunchReason::ResolvedApiKeyNoExpiryClaim,
            ),
            // A session route carries an expiry by construction, so a missing
            // one is a fact we failed to read, not a licence to launch.
            (
                CredentialSource::GrokBuildSession,
                Some("oidc"),
                LaunchReason::ExpiryNotEstablished,
            ),
            // A rotating helper *has* an expiry; it just does not surface one
            // here, so it does not borrow the API-key exemption.
            (
                CredentialSource::TokenCommand,
                None,
                LaunchReason::ExpiryNotEstablished,
            ),
        ] {
            let account = account(source, auth_mode, None);
            let truth = GrokLaunchTruth::project(&LaunchObservation {
                refreshability: Refreshability::NotRefreshable,
                ..observation(&account)
            });
            assert_eq!(
                truth.reason, expected,
                "{source:?}/{auth_mode:?} chose the wrong reason"
            );
            assert_eq!(
                truth.permits_launch(),
                expected == LaunchReason::ResolvedApiKeyNoExpiryClaim,
                "{source:?}/{auth_mode:?} disagreed about gating"
            );
            assert_eq!(truth.validate(), Ok(()));
        }
    }

    #[test]
    fn an_expired_credential_names_the_action_its_route_actually_supports() {
        let account = account(
            CredentialSource::GrokBuildSession,
            Some("oidc"),
            Some("2026-08-24T23:59:59Z"),
        );
        for (refreshability, expected) in [
            (Refreshability::Refreshable, LaunchReason::RefreshFailed),
            (
                Refreshability::NotRefreshable,
                LaunchReason::ReauthenticationRequired,
            ),
            (Refreshability::Unknown, LaunchReason::RefreshabilityUnknown),
        ] {
            let truth = GrokLaunchTruth::project(&LaunchObservation {
                refreshability,
                ..observation(&account)
            });
            assert_eq!(
                truth.reason, expected,
                "{refreshability:?} chose the wrong action"
            );
            assert!(!truth.permits_launch());
        }
    }

    #[test]
    fn a_route_and_a_provider_that_disagree_never_spend_a_credential() {
        let account = account(
            CredentialSource::GrokBuildSession,
            Some("oidc"),
            Some("2026-08-25T12:30:00Z"),
        );
        let truth = GrokLaunchTruth::project(&LaunchObservation {
            route: RouteClass::CompatibleProvider,
            ..observation(&account)
        });
        assert_eq!(truth.reason, LaunchReason::RouteProviderMismatch);
        assert_eq!(truth.readiness, LaunchReadiness::Blocked);
        assert!(!truth.permits_launch());
    }

    #[test]
    fn a_selected_status_without_a_bounded_reference_is_treated_as_unparseable() {
        // A producer bug must not become an unbounded model id on the wire.
        let account = account(
            CredentialSource::GrokBuildSession,
            Some("oidc"),
            Some("2026-08-25T12:30:00Z"),
        );
        let truth = GrokLaunchTruth::project(&LaunchObservation {
            model: ModelFacts {
                status: ModelStatus::Selected,
                selected: None,
            },
            ..observation(&account)
        });
        assert_eq!(truth.reason, LaunchReason::ModelSelectionUnparseable);
        assert!(!truth.permits_launch());
    }

    #[test]
    fn model_and_account_references_reject_anything_not_publishable_verbatim() {
        for hostile in [
            "",
            "   ",
            "grok 4",
            "grok\n4",
            "<script>alert(1)</script>",
            "grok-4\u{0}",
            "../../etc/passwd",
            "grok-4;rm -rf /",
            "/grok-4",
            "grok-4/",
            "openai/../secret",
            ".grok-4",
            "grok-4:",
        ] {
            assert_eq!(ModelReference::new(hostile), None, "accepted {hostile:?}");
        }
        assert!(ModelReference::new(&"a".repeat(MAX_MODEL_REFERENCE_BYTES)).is_some());
        assert_eq!(
            ModelReference::new(&"a".repeat(MAX_MODEL_REFERENCE_BYTES + 1)),
            None
        );
        // Provider-namespaced ids stay publishable.
        assert!(ModelReference::new("openai/gpt-4o-mini").is_some());
        assert!(ModelReference::new("  grok-4  ").is_some_and(|r| r.value == "grok-4"));
    }

    #[test]
    fn the_projection_never_carries_credential_or_endpoint_material() {
        let mut account = account(
            CredentialSource::GrokBuildSession,
            Some("oidc"),
            Some("2026-08-25T12:30:00Z"),
        );
        // Prove the bearer and refresh token are structurally unreachable:
        // there is no field to put them in, so the only way they could appear
        // is if some other field echoed them.
        account.account_reference =
            AccountReference::new("usr-0a1b2c3d", AccountReferenceSource::UserId);
        let truth = GrokLaunchTruth::project(&observation(&account));
        let encoded = serde_json::to_string(&truth).expect("truth serializes");
        for needle in [
            SENTINEL_BEARER,
            SENTINEL_REFRESH,
            SENTINEL_BASE,
            "corp.example",
            "refreshToken",
            "refresh_token",
            "bearer",
            "Bearer",
            "apiKey",
            "api_key_value",
            "keychain",
            "baseUrl",
            "base_url",
            "https://",
            "@",
        ] {
            assert!(!encoded.contains(needle), "leaked {needle:?}: {encoded}");
        }
        assert!(encoded.contains("usr-0a1b2c3d"));
    }

    #[test]
    fn enforce_refuses_every_single_fact_that_drifted_after_admission() {
        let account = account(
            CredentialSource::GrokBuildSession,
            Some("oidc"),
            Some("2026-08-25T12:30:00Z"),
        );
        let truth = GrokLaunchTruth::project(&observation(&account));
        let pinned = truth.requirement();
        assert_eq!(truth.enforce(&pinned), Ok(()));

        let drifted: [(&str, LaunchRequirement); 6] = [
            (
                "provider",
                LaunchRequirement {
                    provider: ProviderClass::OpenAiCompatible,
                    ..pinned.clone()
                },
            ),
            (
                "credential method",
                LaunchRequirement {
                    credential_method: CredentialMethod::ApiKey,
                    ..pinned.clone()
                },
            ),
            (
                "route",
                LaunchRequirement {
                    route: RouteClass::CompatibleProvider,
                    ..pinned.clone()
                },
            ),
            (
                "base",
                LaunchRequirement {
                    base: BaseCategory::CompatibleHttps,
                    ..pinned.clone()
                },
            ),
            (
                "dialect",
                LaunchRequirement {
                    dialect: RequestDialect::OpenAiChatCompletions,
                    ..pinned.clone()
                },
            ),
            (
                "model",
                LaunchRequirement {
                    model: ModelReference::new("grok-3"),
                    ..pinned.clone()
                },
            ),
        ];
        for (name, requirement) in drifted {
            assert!(
                truth.enforce(&requirement).is_err(),
                "{name} drift was accepted"
            );
        }
        let other_account = LaunchRequirement {
            account_reference: AccountReference::new(
                "usr-someone-else",
                AccountReferenceSource::UserId,
            ),
            ..pinned
        };
        assert!(
            truth.enforce(&other_account).is_err(),
            "account drift was accepted"
        );
    }

    #[test]
    fn enforce_refuses_a_requirement_when_the_fresh_truth_is_not_ready() {
        let ready = account(
            CredentialSource::GrokBuildSession,
            Some("oidc"),
            Some("2026-08-25T12:30:00Z"),
        );
        let pinned = GrokLaunchTruth::project(&observation(&ready)).requirement();
        // Same account, same route, but the credential has since expired.
        let expired = account(
            CredentialSource::GrokBuildSession,
            Some("oidc"),
            Some("2026-08-24T23:59:59Z"),
        );
        let fresh = GrokLaunchTruth::project(&observation(&expired));
        assert_eq!(fresh.enforce(&pinned), Err(LaunchReason::RefreshFailed));
    }

    #[test]
    fn validate_rejects_a_ready_verdict_its_own_facts_do_not_support() {
        let account = account(
            CredentialSource::GrokBuildSession,
            Some("oidc"),
            Some("2026-08-25T12:30:00Z"),
        );
        let ready = GrokLaunchTruth::project(&observation(&account));

        // A hand-doctored projection that claims ready with unprobed
        // capabilities is exactly what an attacker would forge.
        let forged = GrokLaunchTruth {
            capabilities: CapabilityFacts::unprobed(),
            ..ready.clone()
        };
        assert!(
            forged.validate().is_err(),
            "a forged ready verdict validated"
        );

        let mismatched = GrokLaunchTruth {
            reason: LaunchReason::SignInRequired,
            ..ready.clone()
        };
        assert!(
            mismatched.validate().is_err(),
            "readiness/reason mismatch validated"
        );

        let wrong_contract = GrokLaunchTruth {
            contract: "grokptah.launch.v2".into(),
            ..ready.clone()
        };
        assert!(wrong_contract.validate().is_err());

        let wrong_version = GrokLaunchTruth {
            schema_version: 2,
            ..ready.clone()
        };
        assert!(wrong_version.validate().is_err());

        let unbounded_model = GrokLaunchTruth {
            model: ModelFacts {
                status: ModelStatus::Selected,
                selected: Some(ModelReference {
                    value: "grok-4 <script>".into(),
                }),
            },
            ..ready
        };
        assert!(
            unbounded_model.validate().is_err(),
            "an unbounded model id validated"
        );
    }

    #[test]
    fn unprobed_capabilities_may_not_assert_anything() {
        let asserted = CapabilityFacts {
            chat: true,
            ..CapabilityFacts::unprobed()
        };
        assert!(asserted.validate().is_err());
        assert_eq!(CapabilityFacts::unprobed().validate(), Ok(()));
    }

    #[test]
    fn unselected_model_evidence_may_not_carry_a_reference() {
        for status in [
            ModelStatus::NotSelected,
            ModelStatus::Unparseable,
            ModelStatus::RouteMismatch,
            ModelStatus::NotOffered,
        ] {
            let smuggled = ModelFacts {
                status,
                selected: ModelReference::new("grok-4"),
            };
            assert!(
                smuggled.validate().is_err(),
                "{status:?} smuggled a model id"
            );
        }
        assert!(
            ModelFacts {
                status: ModelStatus::Selected,
                selected: None
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn the_unresolved_projection_blocks_and_names_the_action() {
        let truth = GrokLaunchTruth::unresolved();
        assert_eq!(truth.reason, LaunchReason::SignInRequired);
        assert_eq!(truth.readiness, LaunchReadiness::Blocked);
        assert!(!truth.permits_launch());
        assert_eq!(truth.validate(), Ok(()));
        assert_eq!(
            truth.attribution().credential_method,
            CredentialMethod::Absent
        );
    }

    #[test]
    fn readiness_is_ready_for_exactly_two_reasons() {
        let ready: Vec<LaunchReason> = LaunchReason::ALL
            .into_iter()
            .filter(|reason| reason.readiness() == LaunchReadiness::Ready)
            .collect();
        assert_eq!(
            ready,
            vec![
                LaunchReason::ResolvedWithFutureExpiry,
                LaunchReason::ResolvedApiKeyNoExpiryClaim
            ]
        );
        // Only `Ready` permits a launch, and nothing else may.
        for readiness in LaunchReadiness::ALL {
            assert_eq!(
                readiness.permits_launch(),
                readiness == LaunchReadiness::Ready
            );
        }
    }

    #[test]
    fn every_closed_vocabulary_has_unique_stable_wire_values() {
        fn unique<T: Copy>(all: &[T], as_str: impl Fn(T) -> &'static str, label: &str) {
            let mut seen: Vec<&'static str> = all.iter().copied().map(&as_str).collect();
            let count = seen.len();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), count, "{label} has duplicate wire values");
            assert!(
                seen.iter().all(|value| !value.is_empty()),
                "{label} has an empty wire value"
            );
        }
        unique(&ProviderClass::ALL, ProviderClass::as_str, "ProviderClass");
        unique(&RouteClass::ALL, RouteClass::as_str, "RouteClass");
        unique(&BaseCategory::ALL, BaseCategory::as_str, "BaseCategory");
        unique(
            &RequestDialect::ALL,
            RequestDialect::as_str,
            "RequestDialect",
        );
        unique(
            &Refreshability::ALL,
            Refreshability::as_str,
            "Refreshability",
        );
        unique(&ModelStatus::ALL, ModelStatus::as_str, "ModelStatus");
        unique(
            &CapabilityProvenance::ALL,
            CapabilityProvenance::as_str,
            "CapabilityProvenance",
        );
        unique(
            &LaunchReadiness::ALL,
            LaunchReadiness::as_str,
            "LaunchReadiness",
        );
        unique(&LaunchReason::ALL, LaunchReason::as_str, "LaunchReason");
        // The wire value must match what serde actually emits.
        for reason in LaunchReason::ALL {
            let encoded = serde_json::to_string(&reason).expect("reason serializes");
            assert_eq!(encoded, format!("\"{}\"", reason.as_str()));
        }
        for category in BaseCategory::ALL {
            let encoded = serde_json::to_string(&category).expect("category serializes");
            assert_eq!(encoded, format!("\"{}\"", category.as_str()));
        }
    }

    #[test]
    fn only_reachable_base_categories_are_launchable() {
        for category in BaseCategory::ALL {
            let launchable = matches!(
                category,
                BaseCategory::XaiOfficial
                    | BaseCategory::CompatibleHttps
                    | BaseCategory::CompatibleLoopback
            );
            assert_eq!(category.is_launchable(), launchable, "{category:?}");
        }
    }

    #[test]
    fn shared_golden_fixtures_agree_with_the_rust_projection() {
        let fixtures: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../docs/schemas/grokptah-launch.v1.fixtures.json"
        ))
        .expect("launch fixtures parse");
        assert_eq!(fixtures["observedAtUnix"].as_i64(), Some(NOW));

        let accepted = fixtures["accepted"]
            .as_array()
            .expect("fixtures declare accepted cases");
        assert!(accepted.len() >= 12, "golden coverage shrank");
        for case in accepted {
            let name = case["name"].as_str().expect("case is named");
            let truth: GrokLaunchTruth = serde_json::from_value(case["truth"].clone())
                .unwrap_or_else(|error| panic!("{name} should decode: {error}"));
            assert_eq!(truth.validate(), Ok(()), "{name} should validate");
            assert_eq!(
                truth.permits_launch(),
                case["permitsLaunch"]
                    .as_bool()
                    .expect("case declares gating"),
                "{name} disagreed about launch gating"
            );
            // Re-serializing a golden case must reproduce it exactly, so the
            // fixture stays a true contract sample rather than a paraphrase.
            assert_eq!(
                serde_json::to_value(&truth).expect("truth serializes"),
                case["truth"],
                "{name} did not round-trip"
            );
            let encoded = serde_json::to_string(&truth).expect("truth serializes");
            for needle in ["https://", "Bearer", "refresh_token", "@", "keychain:"] {
                assert!(!encoded.contains(needle), "{name} leaked {needle:?}");
            }
        }

        let rejected = fixtures["rejected"]
            .as_array()
            .expect("fixtures declare rejected cases");
        assert!(rejected.len() >= 8, "adversarial coverage shrank");
        for case in rejected {
            let name = case["name"].as_str().expect("case is named");
            let decoded: Result<GrokLaunchTruth, _> = serde_json::from_value(case["truth"].clone());
            match decoded {
                // Either the strict decoder refuses it outright...
                Err(_) => {}
                // ...or the validator does. Both are fail-closed; silently
                // accepting it is the only outcome this contract forbids.
                Ok(truth) => assert!(
                    truth.validate().is_err(),
                    "{name} was accepted by both the decoder and the validator"
                ),
            }
        }
    }

    #[test]
    fn the_published_schema_pins_the_same_closed_vocabularies() {
        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../docs/schemas/grokptah-launch.v1.schema.json"
        ))
        .expect("launch schema parses");
        let defs = &schema["$defs"];
        let enum_values = |name: &str| -> Vec<String> {
            defs[name]["enum"]
                .as_array()
                .unwrap_or_else(|| panic!("{name} declares an enum"))
                .iter()
                .map(|value| value.as_str().expect("enum values are strings").to_string())
                .collect()
        };
        let expect = |name: &str, values: Vec<&'static str>| {
            assert_eq!(
                enum_values(name),
                values,
                "{name} drifted between the schema and the Rust contract"
            );
        };
        expect(
            "providerClass",
            ProviderClass::ALL.iter().map(|v| v.as_str()).collect(),
        );
        expect(
            "routeClass",
            RouteClass::ALL.iter().map(|v| v.as_str()).collect(),
        );
        expect(
            "baseCategory",
            BaseCategory::ALL.iter().map(|v| v.as_str()).collect(),
        );
        expect(
            "requestDialect",
            RequestDialect::ALL.iter().map(|v| v.as_str()).collect(),
        );
        expect(
            "refreshability",
            Refreshability::ALL.iter().map(|v| v.as_str()).collect(),
        );
        expect(
            "modelStatus",
            ModelStatus::ALL.iter().map(|v| v.as_str()).collect(),
        );
        expect(
            "capabilityProvenance",
            CapabilityProvenance::ALL
                .iter()
                .map(|v| v.as_str())
                .collect(),
        );
        expect(
            "readiness",
            LaunchReadiness::ALL.iter().map(|v| v.as_str()).collect(),
        );
        expect(
            "reason",
            LaunchReason::ALL.iter().map(|v| v.as_str()).collect(),
        );
        assert_eq!(
            schema["properties"]["contract"]["const"],
            GROK_LAUNCH_CONTRACT_VERSION
        );
        assert_eq!(
            schema["properties"]["schemaVersion"]["const"].as_u64(),
            Some(u64::from(GROK_LAUNCH_SCHEMA_VERSION))
        );
        // The schema must never grow a *place* to put an endpoint or a secret.
        // Only declared property names are checked: the prose descriptions
        // name these fields precisely to say they are excluded.
        fn declared_property_names(node: &serde_json::Value, out: &mut Vec<String>) {
            match node {
                serde_json::Value::Object(map) => {
                    if let Some(serde_json::Value::Object(properties)) = map.get("properties") {
                        out.extend(properties.keys().cloned());
                    }
                    for value in map.values() {
                        declared_property_names(value, out);
                    }
                }
                serde_json::Value::Array(items) => {
                    for item in items {
                        declared_property_names(item, out);
                    }
                }
                _ => {}
            }
        }
        let mut names = Vec::new();
        declared_property_names(&schema, &mut names);
        assert!(
            !names.is_empty(),
            "the schema declares no properties at all"
        );
        for name in &names {
            let lowered = name.to_ascii_lowercase();
            for forbidden in [
                "url",
                "baseurl",
                "endpoint",
                "host",
                "bearer",
                "token",
                "key",
                "secret",
                "credential",
                "email",
                "name",
                "balance",
                "quota",
                "entitlement",
            ] {
                assert!(
                    lowered != forbidden,
                    "the launch schema declares a {name:?} property"
                );
            }
        }
    }

    #[test]
    fn attribution_carries_only_the_route_and_the_account() {
        let account = account(
            CredentialSource::GrokBuildSession,
            Some("oidc"),
            Some("2026-08-25T12:30:00Z"),
        );
        let truth = GrokLaunchTruth::project(&observation(&account));
        let attribution = truth.attribution();
        assert_eq!(attribution.validate(), Ok(()));
        assert_eq!(
            attribution.credential_method,
            CredentialMethod::GrokBuildOidc
        );
        let encoded = serde_json::to_value(&attribution).expect("attribution serializes");
        let object = encoded.as_object().expect("attribution is an object");
        // No balance, quota, entitlement, or certification claim anywhere.
        let mut keys = object.keys().map(String::as_str).collect::<Vec<_>>();
        keys.sort_unstable();
        assert_eq!(keys, vec!["accountReference", "credentialMethod"]);
    }

    #[test]
    fn the_account_contract_it_reads_from_is_still_the_one_it_was_written_against() {
        // A silent bump of the account contract would change what `expiry` and
        // `credentialMethod` mean here without changing this contract's id.
        assert_eq!(GROK_ACCOUNT_CONTRACT_VERSION, "grokptah.account.v1");
        assert_eq!(GROK_ACCOUNT_SCHEMA_VERSION, 1);
        assert_eq!(GROK_LAUNCH_CONTRACT_VERSION, "grokptah.launch.v1");
        assert_eq!(GROK_LAUNCH_SCHEMA_VERSION, 1);
    }
}
