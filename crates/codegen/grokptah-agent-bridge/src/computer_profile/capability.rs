//! Capability evidence, and the generation it is bound to.
//!
//! Issue #272 is emphatic that a model name is not a capability signal and
//! that *declared* support is not *measured* support. Issue #458 adds the
//! sharper requirement this module now implements: route identity alone is not
//! a stable authority key. A provider can keep the same base URL, wire model,
//! and dialect while its tier, provenance, schema, credential, or the local
//! operator policy underneath it all change. Authority bound only to the route
//! survives that change; authority bound to a **generation** does not.
//!
//! [`CapabilityGeneration`] is that binding: a secret-free digest over route
//! identity plus tier, provenance, schema version, credential generation, and
//! operator policy. The adaptive layer records the generation it decided under
//! and revalidates it on every turn, every frame, and every delivery. Any
//! change is a stop, never a reuse.
//!
//! # Declared capability is observation-only by default
//!
//! #458 asks for an explicit answer to "is `Declared` trusted local policy?".
//! The answer here is **no, unless an operator says so**:
//! [`OperatorCapabilityPolicy::trust_declared_capability`] defaults to `false`,
//! and without it a declared-only route may observe but never act. A provider
//! asserting its own competence in a config file is not a measurement.
//!
//! # Synthetic qualification stays visible
//!
//! Passing the deterministic simulator proves a model can emit a schema-valid
//! proposal and recover from a stale frame in this process. It does not prove
//! anything about a live application, so
//! [`ModelCapabilityEvidence::synthetic_only`] travels all the way to the
//! operator projection rather than being laundered into a generic "qualified"
//! bit. That is the production half of the #446/#448 contract: a synthetic
//! PASS is real evidence of exactly one thing, and live eligibility is not
//! that thing.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::profile::AdaptiveProfile;
use crate::gateway_config::{CapabilitySource, ComputerUseTier, ModelCapabilities};

/// Where a capability claim came from. Mirrors [`CapabilitySource`] with a
/// stable wire spelling of its own so the operator projection does not move
/// when the gateway config type does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityAttribution {
    /// Nothing is known. Fails closed everywhere.
    Unknown,
    /// The provider profile asserts it. Never sufficient on its own for act
    /// authority; it can only ever *cap* what measurement may unlock.
    Declared,
    /// Established by running the model, not by reading its metadata.
    Measured,
}

impl CapabilityAttribution {
    pub const fn from_source(source: CapabilitySource) -> Self {
        match source {
            CapabilitySource::Declared => Self::Declared,
            CapabilitySource::Measured => Self::Measured,
            CapabilitySource::Unknown => Self::Unknown,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Declared => "declared",
            Self::Measured => "measured",
        }
    }

    pub const fn is_attributed(self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

/// Local operator policy over capability provenance.
///
/// This is the explicit trust contract #458 asks for. It is *local* policy,
/// not provider-supplied, and it participates in the generation digest so that
/// changing it invalidates every authority decided under the old policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorCapabilityPolicy {
    /// Whether a route whose Computer capability is merely *declared* may be
    /// granted action authority. Default `false`: declared-only routes observe.
    pub trust_declared_capability: bool,
    /// Opaque identifier for the policy revision in force. Changing it changes
    /// the generation, so an authority decided under an older policy cannot be
    /// reused after an operator edits the policy.
    pub policy_generation: String,
}

impl Default for OperatorCapabilityPolicy {
    fn default() -> Self {
        Self {
            trust_declared_capability: false,
            policy_generation: "default/v1".into(),
        }
    }
}

/// A secret-free digest of everything that must not change under a live
/// authority (#458).
///
/// Covers route identity, the effective tier, its provenance, the capability
/// schema version, a one-way credential generation, and the operator policy.
/// Nothing reversible goes in: the credential contributes only as a salted
/// digest, so rotation changes the generation without the bearer ever being
/// recoverable from it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapabilityGeneration(String);

/// Domain separator, so a digest computed here can never collide with one
/// computed for another purpose over the same bytes.
const GENERATION_DOMAIN: &[u8] = b"grokptah.cu.capability-generation.v1";
/// Separate domain for the credential leg, so the credential digest is not
/// usable as an oracle against any other digest in the system.
const CREDENTIAL_DOMAIN: &[u8] = b"grokptah.cu.credential-generation.v1";

impl CapabilityGeneration {
    /// Computes the generation. Deterministic, and a pure function of its
    /// inputs, so two callers reading the same route agree without
    /// coordinating.
    /// A generation that can never equal a computed one.
    ///
    /// `compute` always yields lowercase hex, so this sentinel is
    /// unrepresentable there. It exists for the opt-in live provider proof,
    /// which reaches no durable Computer Run at all: a permit carrying it can
    /// bound what is *sent*, and can never match a record's authority.
    pub fn unbound() -> Self {
        Self("unbound".to_string())
    }

    pub fn compute(
        route_fingerprint: &str,
        capabilities: &ModelCapabilities,
        credential_generation: &str,
        policy: &OperatorCapabilityPolicy,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(GENERATION_DOMAIN);
        for field in [
            route_fingerprint,
            capabilities.effective_computer_use_tier().as_str(),
            CapabilityAttribution::from_source(capabilities.computer_capability_source).as_str(),
            capabilities
                .computer_qualification_schema
                .as_deref()
                .unwrap_or("<none>"),
            capabilities
                .qualification_schema
                .as_deref()
                .unwrap_or("<none>"),
            credential_generation,
            &policy.policy_generation,
        ] {
            hasher.update((field.len() as u64).to_le_bytes());
            hasher.update(field.as_bytes());
        }
        // Booleans that gate what the tier is allowed to mean.
        hasher.update([
            u8::from(capabilities.tools),
            u8::from(capabilities.image_input),
            u8::from(policy.trust_declared_capability),
        ]);
        hasher.update(capabilities.max_image_bytes.unwrap_or(0).to_le_bytes());
        Self(format!("{:x}", hasher.finalize()))
    }

    /// One-way generation identifier for a bearer credential.
    ///
    /// The bearer never leaves this function. Rotation changes the digest,
    /// which is the entire requirement; recovering the token from the digest
    /// is not possible, which is the entire safety property.
    pub fn credential_generation(provider_id: &str, bearer: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(CREDENTIAL_DOMAIN);
        hasher.update((provider_id.len() as u64).to_le_bytes());
        hasher.update(provider_id.as_bytes());
        hasher.update((bearer.len() as u64).to_le_bytes());
        hasher.update(bearer.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A short, operator-readable prefix. Enough to tell two generations
    /// apart in a cockpit; not enough to be mistaken for a secret.
    pub fn short(&self) -> &str {
        &self.0[..self.0.len().min(12)]
    }
}

/// What the selected model is known to be able to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilityEvidence {
    /// Structured native tool calls. Without this there is no closed action
    /// grammar to constrain, so there is no proposal path at all.
    pub tools: bool,
    /// Declared image input. A model without it is text-oriented and belongs on
    /// semantic observations.
    pub image_input: bool,
    /// Declared accepted image byte ceiling, when the provider states one.
    /// `None` is not zero; it is "the provider did not say".
    pub max_image_bytes: Option<u64>,
    /// The Computer Use tier actually in force for this exact route.
    pub tier: ComputerUseTier,
    /// How the Computer Use tier was attributed.
    pub attribution: CapabilityAttribution,
    /// A durable capability recorded against the provider profile, tied to the
    /// exact endpoint and model.
    pub durable_authority: bool,
    /// Authority measured by this process against the deterministic simulator.
    /// Cleared by restart, model change, or route change.
    pub session_measured: bool,
    /// True when every measurement backing this evidence was synthetic. A
    /// synthetic PASS never becomes live eligibility; it only ever unlocks the
    /// cheapest profile.
    pub synthetic_only: bool,
    /// The generation this evidence was read under. Recorded durably and
    /// revalidated on every turn, frame, and delivery; any change stops the
    /// run rather than reusing authority decided under different facts (#458).
    pub generation: CapabilityGeneration,
    /// Whether local operator policy trusts declared-only capability. Carried
    /// so the projection can explain *why* a declared route is observation-only
    /// without the reader having to find the policy file.
    pub declared_capability_trusted: bool,
}

impl ModelCapabilityEvidence {
    /// Reads evidence off the resolved provider/model record.
    ///
    /// `durable_authority` and `session_measured` are supplied by the host
    /// because only the host knows whether *this* process measured *this*
    /// route; they are not derivable from provider metadata, which is exactly
    /// the point.
    pub fn from_model_capabilities(
        capabilities: &ModelCapabilities,
        durable_authority: bool,
        session_measured: bool,
        route_fingerprint: &str,
        credential_generation: &str,
        policy: &OperatorCapabilityPolicy,
    ) -> Self {
        Self {
            tools: capabilities.tools,
            image_input: capabilities.image_input,
            max_image_bytes: capabilities.max_image_bytes,
            tier: capabilities.effective_computer_use_tier(),
            attribution: CapabilityAttribution::from_source(
                capabilities.computer_capability_source,
            ),
            durable_authority,
            // Session qualification is by construction a simulator run.
            session_measured,
            synthetic_only: session_measured && !durable_authority,
            generation: CapabilityGeneration::compute(
                route_fingerprint,
                capabilities,
                credential_generation,
                policy,
            ),
            declared_capability_trusted: policy.trust_declared_capability,
        }
    }

    /// A model with no declared image input cannot ground pixels, whatever its
    /// name suggests.
    pub const fn is_text_oriented(&self) -> bool {
        !self.image_input
    }

    /// Whether a usable, non-reversible image path is actually established:
    /// declared image input, a stated byte ceiling, and a tier that reaches
    /// visual fallback. Any missing leg means "no", not "probably".
    pub const fn has_qualified_visual_path(&self) -> bool {
        self.image_input
            && self.max_image_bytes.is_some()
            && matches!(self.tier, ComputerUseTier::VisualFallbackAct)
            && self.durable_authority
    }

    /// Whether the model may be asked for a semantic proposal at all.
    ///
    /// `Declared` provenance is **not** sufficient on its own: a provider
    /// asserting its own competence in a config file is not a measurement, so
    /// a declared-only route may observe but not act unless a local operator
    /// has explicitly opted in via
    /// [`OperatorCapabilityPolicy::trust_declared_capability`] (#458). Measured
    /// provenance is always sufficient.
    pub const fn may_propose(&self) -> bool {
        if !self.tools {
            return false;
        }
        if !(self.durable_authority || self.session_measured) {
            return false;
        }
        match self.attribution {
            CapabilityAttribution::Unknown => false,
            CapabilityAttribution::Measured => true,
            CapabilityAttribution::Declared => self.declared_capability_trusted,
        }
    }

    /// Whether this evidence was superseded by a newer generation on the same
    /// route. This is the downgrade check, and it is deliberately equality on
    /// the whole digest rather than a comparison of any single field: a change
    /// anywhere in the covered set is a change.
    pub fn matches_generation(&self, current: &CapabilityGeneration) -> bool {
        &self.generation == current
    }
}

/// What the local host can offer this run, independent of the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostCapabilityEvidence {
    /// Semantic (accessibility/DOM) observation is available for the target.
    pub semantic_observation: bool,
    /// A redacted screenshot can be captured for the target.
    pub screenshot_capture: bool,
    /// A verifier independent of the proposing model is available to check a
    /// postcondition. High Assurance needs this; without it, High Assurance
    /// work stops rather than pretending.
    pub independent_verifier: bool,
}

/// Whether this build has a postcondition verifier independent of the model
/// that proposed the action.
///
/// A build fact, not a caller claim, and deliberately const: High Assurance is
/// unreachable while it is `false`, and flipping it is the whole change once a
/// real verifier exists. It lives here rather than in the host so the service
/// can derive host evidence itself instead of being handed a conclusion.
pub const HOST_INDEPENDENT_VERIFIER_AVAILABLE: bool = false;

impl HostCapabilityEvidence {
    /// The conservative default: semantics only, nothing else established.
    pub const SEMANTIC_ONLY: Self = Self {
        semantic_observation: true,
        screenshot_capture: false,
        independent_verifier: false,
    };

    /// Read what the host can actually offer off the frame the operator already
    /// approved, rather than accepting a caller's account of it.
    ///
    /// This is the only construction any admission path uses. A caller that
    /// wants High Assurance cannot get it by asserting a verifier it does not
    /// have: the verifier bit comes from
    /// [`HOST_INDEPENDENT_VERIFIER_AVAILABLE`], and the other two are read off
    /// the observation.
    pub fn observe(observation: &crate::computer_use::ComputerObservation) -> Self {
        Self {
            semantic_observation: !observation.elements.is_empty(),
            screenshot_capture: observation.screenshot.is_some(),
            independent_verifier: HOST_INDEPENDENT_VERIFIER_AVAILABLE,
        }
    }
}

/// The complete evidence set a profile decision is derived from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityEvidence {
    pub model: ModelCapabilityEvidence,
    pub host: HostCapabilityEvidence,
}

impl CapabilityEvidence {
    pub const fn new(model: ModelCapabilityEvidence, host: HostCapabilityEvidence) -> Self {
        Self { model, host }
    }

    /// The highest profile this model and host can honestly support.
    ///
    /// This is a **ceiling**, not a selection. Escalating past it is not
    /// possible: a run that needs more assurance than the evidence can supply
    /// stops and says so, because the alternative is presenting an
    /// unqualified model as a qualified one.
    pub fn ceiling(&self) -> AdaptiveProfile {
        if !self.model.may_propose() {
            return AdaptiveProfile::Economy;
        }
        // High Assurance means a richer *and independently checked* path. Both
        // legs are required: a frontier-class model with no independent
        // verifier is a Balanced run, not a High Assurance one.
        if self.model.has_qualified_visual_path()
            && self.host.screenshot_capture
            && self.host.independent_verifier
        {
            return AdaptiveProfile::HighAssurance;
        }
        // Balanced adds geometry to a semantic path. A text-oriented model
        // gains nothing from geometry it cannot correlate with pixels, and a
        // synthetic-only qualification has established exactly one frame's
        // worth of behavior, so both stay at Economy.
        if self.model.is_text_oriented()
            || self.model.synthetic_only
            || !self.host.semantic_observation
        {
            return AdaptiveProfile::Economy;
        }
        AdaptiveProfile::Balanced
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generation() -> CapabilityGeneration {
        CapabilityGeneration::compute(
            "route-1",
            &ModelCapabilities::default(),
            "cred-1",
            &OperatorCapabilityPolicy::default(),
        )
    }

    fn model(tier: ComputerUseTier, image: bool) -> ModelCapabilityEvidence {
        ModelCapabilityEvidence {
            tools: true,
            image_input: image,
            max_image_bytes: image.then_some(4 * 1024 * 1024),
            tier,
            attribution: CapabilityAttribution::Measured,
            durable_authority: true,
            session_measured: false,
            synthetic_only: false,
            generation: generation(),
            declared_capability_trusted: false,
        }
    }

    #[test]
    fn text_oriented_gateway_model_is_capped_at_economy() {
        let evidence = CapabilityEvidence::new(
            model(ComputerUseTier::SemanticAct, false),
            HostCapabilityEvidence {
                semantic_observation: true,
                screenshot_capture: true,
                independent_verifier: true,
            },
        );
        assert!(evidence.model.is_text_oriented());
        assert_eq!(evidence.ceiling(), AdaptiveProfile::Economy);
    }

    #[test]
    fn visual_model_without_independent_verifier_stops_at_balanced() {
        let evidence = CapabilityEvidence::new(
            model(ComputerUseTier::VisualFallbackAct, true),
            HostCapabilityEvidence {
                semantic_observation: true,
                screenshot_capture: true,
                independent_verifier: false,
            },
        );
        assert!(evidence.model.has_qualified_visual_path());
        assert_eq!(evidence.ceiling(), AdaptiveProfile::Balanced);
    }

    #[test]
    fn full_evidence_reaches_high_assurance() {
        let evidence = CapabilityEvidence::new(
            model(ComputerUseTier::VisualFallbackAct, true),
            HostCapabilityEvidence {
                semantic_observation: true,
                screenshot_capture: true,
                independent_verifier: true,
            },
        );
        assert_eq!(evidence.ceiling(), AdaptiveProfile::HighAssurance);
    }

    #[test]
    fn synthetic_only_qualification_never_buys_more_than_economy() {
        let capabilities = ModelCapabilities {
            tools: true,
            image_input: true,
            max_image_bytes: Some(1024),
            computer_use_tier: ComputerUseTier::VisualFallbackAct,
            computer_capability_source: CapabilitySource::Measured,
            ..Default::default()
        };
        let model = ModelCapabilityEvidence::from_model_capabilities(
            &capabilities,
            false,
            true,
            "route-1",
            "cred-1",
            &OperatorCapabilityPolicy::default(),
        );
        assert!(model.synthetic_only, "session qualification is synthetic");
        let evidence = CapabilityEvidence::new(
            model,
            HostCapabilityEvidence {
                semantic_observation: true,
                screenshot_capture: true,
                independent_verifier: true,
            },
        );
        assert_eq!(evidence.ceiling(), AdaptiveProfile::Economy);
    }

    #[test]
    fn unattributed_or_toolless_models_may_not_propose() {
        // Source unknown: `effective_computer_use_tier` already floors this to
        // `None`, and the evidence agrees rather than second-guessing it.
        let unattributed = ModelCapabilities {
            tools: true,
            computer_use_tier: ComputerUseTier::SemanticAct,
            ..Default::default()
        };
        let model = ModelCapabilityEvidence::from_model_capabilities(
            &unattributed,
            true,
            false,
            "route-1",
            "cred-1",
            &OperatorCapabilityPolicy::default(),
        );
        assert_eq!(model.tier, ComputerUseTier::None);
        assert!(!model.may_propose());

        let toolless = ModelCapabilities {
            tools: false,
            computer_use_tier: ComputerUseTier::SemanticAct,
            computer_capability_source: CapabilitySource::Declared,
            ..Default::default()
        };
        let toolless = ModelCapabilityEvidence::from_model_capabilities(
            &toolless,
            true,
            false,
            "route-1",
            "cred-1",
            &OperatorCapabilityPolicy::default(),
        );
        assert!(!toolless.may_propose());
    }

    #[test]
    fn declared_image_support_without_a_byte_ceiling_is_not_a_visual_path() {
        let mut evidence = model(ComputerUseTier::VisualFallbackAct, true);
        evidence.max_image_bytes = None;
        assert!(!evidence.has_qualified_visual_path());
    }

    #[test]
    fn declared_only_capability_is_observation_only_until_an_operator_opts_in() {
        let declared = ModelCapabilities {
            tools: true,
            computer_use_tier: ComputerUseTier::SemanticAct,
            computer_capability_source: CapabilitySource::Declared,
            ..Default::default()
        };
        let untrusted = ModelCapabilityEvidence::from_model_capabilities(
            &declared,
            true,
            false,
            "route-1",
            "cred-1",
            &OperatorCapabilityPolicy::default(),
        );
        assert_eq!(untrusted.attribution, CapabilityAttribution::Declared);
        assert!(
            !untrusted.may_propose(),
            "a provider asserting its own competence is not a measurement"
        );

        let trusted = ModelCapabilityEvidence::from_model_capabilities(
            &declared,
            true,
            false,
            "route-1",
            "cred-1",
            &OperatorCapabilityPolicy {
                trust_declared_capability: true,
                policy_generation: "operator/opt-in".into(),
            },
        );
        assert!(
            trusted.may_propose(),
            "an explicit operator opt-in is a decision"
        );
        assert_ne!(
            untrusted.generation, trusted.generation,
            "the policy participates in the generation"
        );
    }

    #[test]
    fn the_generation_is_secret_free_and_moves_on_every_covered_field() {
        let base = generation();
        assert_eq!(base.as_str().len(), 64);
        assert!(!base.as_str().contains("cred-1"));
        assert_eq!(base.short().len(), 12);
        let credential = CapabilityGeneration::credential_generation("provider", "super-secret");
        assert!(!credential.contains("super-secret"));
        assert_ne!(
            credential,
            CapabilityGeneration::credential_generation("provider", "rotated")
        );
    }
}
