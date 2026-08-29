//! What one physical provider-send attempt is bound to (#478).
//!
//! Everything here is secret-free by construction: values that could carry a
//! credential, a prompt, a body, or a private route are hashed into an
//! [`OpaqueId`] at the boundary and the plaintext is never stored. The binding
//! is *re-derivable*: given the durable record plus the same inputs, the same
//! host idempotency identity comes out, which is what lets a restart recognise
//! its own prior attempt instead of inventing a new one.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

use super::dialect::WireDialect;
use super::seams::{
    AuditGeneration, CapabilityGeneration, LifecycleGeneration, PrincipalGeneration,
    QueueOwnershipGeneration,
};

/// Schema version of the attempt binding. Bumped whenever the digest inputs or
/// their order change, because that changes every derived identity.
pub const ATTEMPT_BINDING_VERSION: u32 = 1;

/// Schema version of the host idempotency identity specifically. Kept separate
/// so the host key can be re-derived for an older binding version.
pub const HOST_IDEMPOTENCY_VERSION: u32 = 1;

const OPAQUE_ID_LEN: usize = 64;

/// A hex SHA-256 digest used wherever a value must be stable and comparable but
/// must never be readable.
#[derive(Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OpaqueId(String);

impl OpaqueId {
    /// Accept an already-opaque identifier (64 lowercase hex characters).
    pub fn parse(value: &str) -> Result<Self, IdentityError> {
        if value.len() != OPAQUE_ID_LEN
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(IdentityError::NotOpaque);
        }
        Ok(Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Short, still-opaque prefix for human-facing surfaces.
    pub fn short(&self) -> &str {
        &self.0[..12]
    }
}

// Debug must not be a way to read the value back out of a log line, but an
// opaque id is already a digest — printing a short prefix keeps it useful.
impl fmt::Debug for OpaqueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OpaqueId({}…)", self.short())
    }
}

impl fmt::Display for OpaqueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Domain-separated digest over length-prefixed inputs.
///
/// Length prefixing matters: without it `["ab", "c"]` and `["a", "bc"]` would
/// collide, which would let two different bindings share one idempotency key.
pub fn opaque_digest(domain: &str, inputs: &[&str]) -> OpaqueId {
    let mut hasher = Sha256::new();
    hasher.update((domain.len() as u64).to_be_bytes());
    hasher.update(domain.as_bytes());
    hasher.update((inputs.len() as u64).to_be_bytes());
    for input in inputs {
        hasher.update((input.len() as u64).to_be_bytes());
        hasher.update(input.as_bytes());
    }
    OpaqueId(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum IdentityError {
    NotOpaque,
    EmptyScope,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotOpaque => f.write_str("value is not a 64-character hex digest"),
            Self::EmptyScope => {
                f.write_str("send scope requires a non-empty workspace and session")
            }
        }
    }
}

impl std::error::Error for IdentityError {}

/// Which physical send site produced an attempt.
///
/// Every provider-capable call site in the crate maps to exactly one variant.
/// The structural gate asserts the mapping is exhaustive in both directions, so
/// a new send site cannot be added without appearing here.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallSiteFamily {
    /// Desktop Chat turn.
    DesktopChatTurn,
    /// Desktop Build coding-agent round.
    DesktopBuildRound,
    /// Plan proposal before a build turn.
    PlanProposal,
    /// Session compaction / summary.
    SessionCompaction,
    /// Explore subagent round.
    ExploreSubagent,
    /// General-purpose subagent round.
    GeneralPurposeSubagent,
    /// Computer Use semantic-action round.
    ComputerUseRound,
    /// Computer Use model qualification round.
    ComputerUseQualification,
    /// Provider/model qualification probe (completion and streaming).
    ProviderQualificationProbe,
}

impl CallSiteFamily {
    /// Every family. Used by the coverage gate.
    pub const ALL: [Self; 9] = [
        Self::DesktopChatTurn,
        Self::DesktopBuildRound,
        Self::PlanProposal,
        Self::SessionCompaction,
        Self::ExploreSubagent,
        Self::GeneralPurposeSubagent,
        Self::ComputerUseRound,
        Self::ComputerUseQualification,
        Self::ProviderQualificationProbe,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::DesktopChatTurn => "desktop_chat_turn",
            Self::DesktopBuildRound => "desktop_build_round",
            Self::PlanProposal => "plan_proposal",
            Self::SessionCompaction => "session_compaction",
            Self::ExploreSubagent => "explore_subagent",
            Self::GeneralPurposeSubagent => "general_purpose_subagent",
            Self::ComputerUseRound => "computer_use_round",
            Self::ComputerUseQualification => "computer_use_qualification",
            Self::ProviderQualificationProbe => "provider_qualification_probe",
        }
    }
}

/// Which entry path drove the send. Distinct from the family: the same physical
/// site serves the desktop, the headless service, the broker, and scheduled
/// work, and an attempt has to say honestly which one it was.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendOrigin {
    Desktop,
    Orchestration,
    HeadlessService,
    McpBroker,
    ScheduledRoutine,
    Manager,
    Qualification,
}

impl SendOrigin {
    pub const ALL: [Self; 7] = [
        Self::Desktop,
        Self::Orchestration,
        Self::HeadlessService,
        Self::McpBroker,
        Self::ScheduledRoutine,
        Self::Manager,
        Self::Qualification,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Orchestration => "orchestration",
            Self::HeadlessService => "headless_service",
            Self::McpBroker => "mcp_broker",
            Self::ScheduledRoutine => "scheduled_routine",
            Self::Manager => "manager",
            Self::Qualification => "qualification",
        }
    }
}

/// Session / workspace / run scope of an attempt.
///
/// The plaintext workspace path and session id never reach the durable record —
/// only their opaque digests — but the digests are re-derivable from the same
/// inputs, which is what makes restart recognition work.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendScope {
    workspace: OpaqueId,
    session: OpaqueId,
    run: Option<OpaqueId>,
    origin: SendOrigin,
    family: CallSiteFamily,
}

impl SendScope {
    /// Build a scope from plaintext identifiers, which are hashed immediately.
    pub fn new(
        workspace: &str,
        session: &str,
        run: Option<&str>,
        origin: SendOrigin,
        family: CallSiteFamily,
    ) -> Result<Self, IdentityError> {
        if workspace.is_empty() || session.is_empty() {
            return Err(IdentityError::EmptyScope);
        }
        Ok(Self {
            workspace: opaque_digest("grokptah.provider_send.workspace.v1", &[workspace]),
            session: opaque_digest("grokptah.provider_send.session.v1", &[session]),
            run: run
                .filter(|value| !value.is_empty())
                .map(|value| opaque_digest("grokptah.provider_send.run.v1", &[value])),
            origin,
            family,
        })
    }

    pub fn workspace(&self) -> &OpaqueId {
        &self.workspace
    }

    pub fn session(&self) -> &OpaqueId {
        &self.session
    }

    pub fn run(&self) -> Option<&OpaqueId> {
        self.run.as_ref()
    }

    pub fn origin(&self) -> SendOrigin {
        self.origin
    }

    pub fn family(&self) -> CallSiteFamily {
        self.family
    }

    /// The durable directory key. Ordinals are monotonic within exactly this
    /// key, so two different scopes never contend for one ordinal sequence.
    pub fn ledger_key(&self) -> OpaqueId {
        opaque_digest(
            "grokptah.provider_send.scope_key.v1",
            &[
                self.workspace.as_str(),
                self.session.as_str(),
                self.run.as_ref().map(OpaqueId::as_str).unwrap_or(""),
                self.origin.as_str(),
                self.family.as_str(),
            ],
        )
    }
}

/// The concrete provider route an attempt targets.
///
/// `credential_incarnation` is an opaque digest of *which* credential was in
/// force, never the credential. Rotating a key changes the incarnation, so an
/// attempt cannot be silently re-bound to a different secret.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteIncarnation {
    /// Opaque digest of the base URL. The raw route never lands durably.
    endpoint: OpaqueId,
    /// Wire model id. Public by construction — it is chosen by the operator and
    /// appears in the request body — so it stays readable for diagnosis.
    wire_model: String,
    dialect: WireDialect,
    credential_incarnation: OpaqueId,
}

impl RouteIncarnation {
    /// Build from plaintext route material, which is hashed at the boundary.
    ///
    /// `credential_material` must be a *non-secret* identifier for the
    /// credential in force (method plus binding id), not the token itself; it
    /// is hashed regardless so a mistake here cannot leak.
    pub fn new(
        base_url: &str,
        wire_model: &str,
        dialect: WireDialect,
        credential_method: &str,
        credential_binding: Option<&str>,
    ) -> Self {
        Self {
            endpoint: opaque_digest("grokptah.provider_send.endpoint.v1", &[base_url]),
            wire_model: wire_model.to_string(),
            dialect,
            credential_incarnation: opaque_digest(
                "grokptah.provider_send.credential.v1",
                &[credential_method, credential_binding.unwrap_or("")],
            ),
        }
    }

    pub fn endpoint(&self) -> &OpaqueId {
        &self.endpoint
    }

    pub fn wire_model(&self) -> &str {
        &self.wire_model
    }

    pub fn dialect(&self) -> WireDialect {
        self.dialect
    }

    pub fn credential_incarnation(&self) -> &OpaqueId {
        &self.credential_incarnation
    }
}

/// Digest of the exact request that would go on the wire.
///
/// Two attempts with the same digest are the same request; a compatibility
/// downgrade that removes a field changes the digest, which is correct — it is
/// a different request and deserves its own ordinal.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestDigest(OpaqueId);

impl RequestDigest {
    /// Digest the serialized request body. The body itself is never retained.
    pub fn of_body(body: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"grokptah.provider_send.request_body.v1");
        hasher.update((body.len() as u64).to_be_bytes());
        hasher.update(body);
        Self(OpaqueId(hex(&hasher.finalize())))
    }

    pub fn as_opaque(&self) -> &OpaqueId {
        &self.0
    }
}

/// The host's own idempotency identity for one attempt.
///
/// Deliberately *not* a provider receipt and never sent on the wire unless the
/// dialect explicitly declares support (see [`super::dialect`]). It exists so
/// the host can recognise its own attempt across a restart.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostIdempotencyIdentity {
    version: u32,
    key: OpaqueId,
}

impl HostIdempotencyIdentity {
    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn key(&self) -> &OpaqueId {
        &self.key
    }

    /// Header-safe rendering. Still opaque, still secret-free.
    pub fn wire_value(&self) -> String {
        format!("gp-{}-{}", self.version, self.key.as_str())
    }
}

/// Everything one attempt is bound to. Constructing this is the only way to
/// reach the physical send path, which is what makes "unbound call site"
/// unrepresentable rather than merely discouraged.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptBinding {
    version: u32,
    scope: SendScope,
    principal: PrincipalGeneration,
    capability: CapabilityGeneration,
    lifecycle: LifecycleGeneration,
    queue: QueueOwnershipGeneration,
    audit: AuditGeneration,
    route: RouteIncarnation,
    ordinal: u64,
    request_digest: RequestDigest,
    host_idempotency: HostIdempotencyIdentity,
}

/// The parts of a binding that are fixed before an ordinal is allocated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttemptBindingSpec {
    pub scope: SendScope,
    pub principal: PrincipalGeneration,
    pub capability: CapabilityGeneration,
    pub lifecycle: LifecycleGeneration,
    pub queue: QueueOwnershipGeneration,
    pub audit: AuditGeneration,
    pub route: RouteIncarnation,
    pub request_digest: RequestDigest,
}

impl AttemptBinding {
    /// Complete a binding by pinning the monotonic ordinal and deriving the
    /// host idempotency identity from everything above it.
    pub(crate) fn seal(spec: AttemptBindingSpec, ordinal: u64) -> Self {
        let host_idempotency = HostIdempotencyIdentity {
            version: HOST_IDEMPOTENCY_VERSION,
            key: Self::derive_host_key(&spec, ordinal),
        };
        Self {
            version: ATTEMPT_BINDING_VERSION,
            scope: spec.scope,
            principal: spec.principal,
            capability: spec.capability,
            lifecycle: spec.lifecycle,
            queue: spec.queue,
            audit: spec.audit,
            route: spec.route,
            ordinal,
            request_digest: spec.request_digest,
            host_idempotency,
        }
    }

    /// Re-derive the host key from a spec and ordinal.
    ///
    /// This is the function a restart uses to confirm that a durable record is
    /// *this* attempt and not a lookalike, so its input list is part of the
    /// binding version contract.
    pub(crate) fn derive_host_key(spec: &AttemptBindingSpec, ordinal: u64) -> OpaqueId {
        let ordinal = ordinal.to_string();
        opaque_digest(
            "grokptah.provider_send.host_idempotency.v1",
            &[
                &ATTEMPT_BINDING_VERSION.to_string(),
                &HOST_IDEMPOTENCY_VERSION.to_string(),
                &spec.principal.digest_input(),
                &spec.capability.digest_input(),
                &spec.lifecycle.digest_input(),
                &spec.queue.digest_input(),
                &spec.audit.digest_input(),
                spec.scope.workspace.as_str(),
                spec.scope.session.as_str(),
                spec.scope.run.as_ref().map(OpaqueId::as_str).unwrap_or(""),
                spec.scope.origin.as_str(),
                spec.scope.family.as_str(),
                spec.route.endpoint.as_str(),
                spec.route.wire_model.as_str(),
                spec.route.dialect.as_str(),
                spec.route.credential_incarnation.as_str(),
                &ordinal,
                spec.request_digest.as_opaque().as_str(),
            ],
        )
    }

    /// Re-derive this binding's host key and check it matches what is stored.
    ///
    /// A durable record that fails this check is not this attempt, whatever its
    /// filename says.
    pub fn host_key_is_rederivable(&self) -> bool {
        let spec = AttemptBindingSpec {
            scope: self.scope.clone(),
            principal: self.principal.clone(),
            capability: self.capability.clone(),
            lifecycle: self.lifecycle.clone(),
            queue: self.queue.clone(),
            audit: self.audit.clone(),
            route: self.route.clone(),
            request_digest: self.request_digest.clone(),
        };
        Self::derive_host_key(&spec, self.ordinal) == self.host_idempotency.key
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn scope(&self) -> &SendScope {
        &self.scope
    }

    pub fn principal(&self) -> &PrincipalGeneration {
        &self.principal
    }

    pub fn capability(&self) -> &CapabilityGeneration {
        &self.capability
    }

    pub fn lifecycle(&self) -> &LifecycleGeneration {
        &self.lifecycle
    }

    pub fn queue(&self) -> &QueueOwnershipGeneration {
        &self.queue
    }

    pub fn audit(&self) -> &AuditGeneration {
        &self.audit
    }

    pub fn route(&self) -> &RouteIncarnation {
        &self.route
    }

    pub fn ordinal(&self) -> u64 {
        self.ordinal
    }

    pub fn request_digest(&self) -> &RequestDigest {
        &self.request_digest
    }

    pub fn host_idempotency(&self) -> &HostIdempotencyIdentity {
        &self.host_idempotency
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub fn spec(session: &str, ordinal_input: &str) -> AttemptBindingSpec {
        AttemptBindingSpec {
            scope: SendScope::new(
                "/workspace",
                session,
                None,
                SendOrigin::Desktop,
                CallSiteFamily::DesktopChatTurn,
            )
            .expect("scope"),
            principal: PrincipalGeneration::provisional(&["principal"]),
            capability: CapabilityGeneration::provisional(&["capability"]),
            lifecycle: LifecycleGeneration::provisional(&["lifecycle"]),
            queue: QueueOwnershipGeneration::provisional(&["queue"]),
            audit: AuditGeneration::provisional(&["audit"]),
            route: RouteIncarnation::new(
                "https://example.invalid/v1",
                "model-a",
                WireDialect::OpenAiChatCompletions,
                "gateway_api_key",
                None,
            ),
            request_digest: RequestDigest::of_body(ordinal_input.as_bytes()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_prefixing_prevents_binding_collisions() {
        assert_ne!(
            opaque_digest("d", &["ab", "c"]),
            opaque_digest("d", &["a", "bc"])
        );
    }

    #[test]
    fn host_key_is_rederivable_from_the_sealed_binding() {
        let binding = AttemptBinding::seal(fixtures::spec("s1", "body"), 7);
        assert!(binding.host_key_is_rederivable());
        assert_eq!(binding.ordinal(), 7);
        assert_eq!(
            binding.host_idempotency().version(),
            HOST_IDEMPOTENCY_VERSION
        );
    }

    #[test]
    fn ordinal_participates_in_the_host_key() {
        let first = AttemptBinding::seal(fixtures::spec("s1", "body"), 1);
        let second = AttemptBinding::seal(fixtures::spec("s1", "body"), 2);
        assert_ne!(
            first.host_idempotency().key(),
            second.host_idempotency().key()
        );
    }

    #[test]
    fn a_changed_request_changes_the_host_key() {
        let first = AttemptBinding::seal(fixtures::spec("s1", "body-a"), 1);
        let second = AttemptBinding::seal(fixtures::spec("s1", "body-b"), 1);
        assert_ne!(
            first.host_idempotency().key(),
            second.host_idempotency().key()
        );
    }

    #[test]
    fn scope_and_route_plaintext_never_survives_into_the_binding() {
        let scope = SendScope::new(
            "/private/workspace/path",
            "session-secret",
            Some("run-secret"),
            SendOrigin::Orchestration,
            CallSiteFamily::ExploreSubagent,
        )
        .expect("scope");
        let route = RouteIncarnation::new(
            "https://private.gateway.invalid/inference",
            "model-a",
            WireDialect::OpenAiChatCompletions,
            "gateway_api_key",
            Some("binding-1"),
        );
        let serialized = serde_json::to_string(&(scope, route)).expect("serialize");
        for secret in [
            "/private/workspace/path",
            "session-secret",
            "run-secret",
            "private.gateway.invalid",
            "binding-1",
        ] {
            assert!(!serialized.contains(secret), "leaked {secret}");
        }
        // The operator-chosen wire model stays readable on purpose.
        assert!(serialized.contains("model-a"));
    }

    #[test]
    fn credential_rotation_changes_the_incarnation() {
        let first = RouteIncarnation::new(
            "https://e.invalid",
            "m",
            WireDialect::XaiChatCompletions,
            "grok_build_oidc",
            Some("binding-1"),
        );
        let second = RouteIncarnation::new(
            "https://e.invalid",
            "m",
            WireDialect::XaiChatCompletions,
            "grok_build_oidc",
            Some("binding-2"),
        );
        assert_ne!(
            first.credential_incarnation(),
            second.credential_incarnation()
        );
    }

    #[test]
    fn scopes_of_different_families_never_share_an_ordinal_sequence() {
        let chat = SendScope::new(
            "/w",
            "s",
            None,
            SendOrigin::Desktop,
            CallSiteFamily::DesktopChatTurn,
        )
        .expect("scope");
        let explore = SendScope::new(
            "/w",
            "s",
            None,
            SendOrigin::Desktop,
            CallSiteFamily::ExploreSubagent,
        )
        .expect("scope");
        assert_ne!(chat.ledger_key(), explore.ledger_key());
    }

    #[test]
    fn opaque_ids_reject_non_digest_input() {
        assert!(OpaqueId::parse("not-a-digest").is_err());
        assert!(OpaqueId::parse(&"a".repeat(64)).is_ok());
        assert!(OpaqueId::parse(&"A".repeat(64)).is_err());
    }
}
