//! The secret-free content binding: what a capability *is*.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::policy::CapabilityProvenance;
use super::profile::AssuranceProfile;
use crate::gateway_config::ComputerUseTier;

/// Domain separator. Changing it retires every binding in existence, which is
/// the intended effect of changing what a capability binding means.
const DOMAIN: &[u8] = b"grokptah.provider-capability-generation.v1\0";

/// Route identity after normalization.
///
/// Normalization exists so a cosmetic re-spelling of the same endpoint does
/// not force a requalification. It is deliberately conservative: anything it
/// cannot confidently parse is kept verbatim, so two spellings that might be
/// different routes stay different routes. Normalization may only ever merge
/// spellings that are provably the same endpoint; over-splitting costs a
/// requalification, under-splitting would let one route inherit another's
/// authority.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedRoute {
    pub provider_id: String,
    pub base_url: String,
    pub wire_model: String,
    pub dialect: String,
}

impl NormalizedRoute {
    pub fn new(
        provider_id: impl Into<String>,
        base_url: &str,
        wire_model: impl Into<String>,
        dialect: impl Into<String>,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            base_url: normalize_base_url(base_url),
            wire_model: wire_model.into(),
            dialect: dialect.into(),
        }
    }
}

/// Normalizes a provider base URL for route identity.
///
/// Lowercases the scheme and host, drops the scheme's default port, and drops
/// a single trailing slash. Userinfo, an unparseable authority, or a missing
/// scheme make the value opaque: it is returned trimmed but otherwise
/// untouched, so it can only ever compare equal to an identical spelling.
pub fn normalize_base_url(value: &str) -> String {
    let trimmed = value.trim();
    let Some((scheme, rest)) = trimmed.split_once("://") else {
        return trimmed.to_string();
    };
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
    {
        return trimmed.to_string();
    }
    let scheme = scheme.to_ascii_lowercase();
    let (authority, tail) = match rest.find(['/', '?', '#']) {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    };
    // Userinfo changes who is calling, not just where. Keep it opaque rather
    // than folding two different callers onto one route identity.
    if authority.is_empty() || authority.contains('@') {
        return trimmed.to_string();
    }
    // A port is a non-empty run of digits after the last colon. Requiring
    // digits keeps an IPv6 literal's internal colons from being read as a
    // port separator, which would silently rewrite the host.
    let (host, port) = match authority.rsplit_once(':') {
        Some((head, tail))
            if !tail.is_empty()
                && tail.chars().all(|c| c.is_ascii_digit())
                && (!head.contains(']') || head.ends_with(']')) =>
        {
            (head, Some(tail))
        }
        _ => (authority, None),
    };
    if host.is_empty() {
        return trimmed.to_string();
    }
    let host = host.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "https" => Some("443"),
        "http" => Some("80"),
        _ => None,
    };
    let authority = match port {
        None => host,
        Some(port) if Some(port) == default_port => host,
        Some(port) => format!("{host}:{port}"),
    };
    let tail = match tail.strip_suffix('/') {
        Some(stripped) if !stripped.contains(['?', '#']) => stripped,
        _ => tail,
    };
    format!("{scheme}://{authority}{tail}")
}

/// The upstream authority a capability descends from.
///
/// # What this is, and what it is not
///
/// A capability generation answers "is this still the capability that was
/// qualified?". It does not, on its own, answer "and is the authority that
/// qualified it still the one in force?" — that is an *upstream* question,
/// and without an answer to it an upstream rotation cannot retire anything
/// downstream.
///
/// This field is where that answer binds. It is folded into the digest, so
/// when the upstream lineage moves, every capability binding taken under the
/// old one stops matching and is refused at its next boundary.
///
/// Today the host populates it with its own process auth lineage: an id drawn
/// at startup and a counter that advances on every credential or policy
/// invalidation. That is a real upstream — a host restart or an auth
/// invalidation does retire every binding — but it is deliberately **not** a
/// claim of verified principal, tenant, scope, or operator identity. None of
/// those exist to bind yet; minting them is separate work (#477), as is the
/// service-scoped auth epoch (#460). When a canonical one lands, it populates
/// this field and its rotation retires capability generations for free.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorityLineage {
    /// Names the upstream authority. Opaque and secret-free.
    pub authority: String,
    /// That authority's own monotonic generation.
    pub generation: u64,
}

/// The qualification contract a binding was taken under.
///
/// A code change that alters what qualification proves must bump `version`;
/// every binding taken under the old version then fails to match live facts
/// and is refused rather than silently inherited.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QualificationSchema {
    pub id: String,
    pub version: u32,
}

impl QualificationSchema {
    pub fn new(id: impl Into<String>, version: u32) -> Self {
        Self {
            id: id.into(),
            version,
        }
    }
}

/// Secret-free credential identity plus the incarnation it belongs to.
///
/// `fingerprint` is a one-way digest of the credential *principal*, never the
/// credential. `incarnation` is a registry-local monotonic counter that
/// advances on every rotation and on every deletion, so a credential removed
/// and re-added with byte-identical material still lands on a new incarnation
/// and cannot inherit the deleted one's authority.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialIncarnation {
    pub fingerprint: String,
    pub incarnation: u64,
}

/// Operator policy and allowlist state that a capability depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRevision {
    pub revision: u64,
}

/// How a capability statement was established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationEvidenceKind {
    /// No evidence was taken. Never durable action authority.
    Absent,
    /// An operator- or provider-*declared* statement. It is metadata, not
    /// evidence, and is only ever honoured under an explicit operator trust
    /// policy — see [`super::DeclaredCapabilityPolicy`].
    Declared,
    /// A bounded local probe actually ran and passed.
    Measured,
    /// A measured probe whose transcript carries an operator-verified
    /// signature.
    Signed,
}

impl QualificationEvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Declared => "declared",
            Self::Measured => "measured",
            Self::Signed => "signed",
        }
    }
}

/// The evidence a qualification produced, reduced to a secret-free digest.
///
/// The transcript itself never enters this module; only its digest does, so a
/// binding can prove "the same evidence" without holding observed content.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QualificationEvidence {
    pub kind: QualificationEvidenceKind,
    pub digest: String,
}

impl QualificationEvidence {
    /// Evidence for a qualification that never ran.
    ///
    /// Test-only: no production path mints a binding on no evidence, and one
    /// that could would be a way to buy authority with nothing.
    #[cfg(test)]
    pub(crate) fn absent() -> Self {
        Self {
            kind: QualificationEvidenceKind::Absent,
            digest: hex_digest(&[(b"evidence".as_slice(), b"absent".as_slice())]),
        }
    }

    /// Reduces a transcript to a one-way digest. The transcript is consumed
    /// here and never stored.
    ///
    /// Crate-internal: evidence is the authority's own record of what it
    /// proved. A caller that could construct `Signed` evidence could
    /// manufacture the authority that evidence buys.
    pub(crate) fn of(kind: QualificationEvidenceKind, transcript: &[u8]) -> Self {
        Self {
            kind,
            digest: hex_digest(&[
                (b"kind".as_slice(), kind.as_str().as_bytes()),
                (b"transcript".as_slice(), transcript),
            ]),
        }
    }
}

/// Everything that must still hold for a qualification to still mean what it
/// meant when it was taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityFacts {
    /// The upstream authority this capability descends from.
    pub lineage: AuthorityLineage,
    pub route: NormalizedRoute,
    /// The operator-visible `provider/model` selection key. Bound separately
    /// from the wire model so an aliased selection cannot silently retarget.
    pub selection_key: String,
    pub tier: ComputerUseTier,
    pub profile: AssuranceProfile,
    pub provenance: CapabilityProvenance,
    pub schema: QualificationSchema,
    pub credential: CredentialIncarnation,
    pub policy: PolicyRevision,
}

impl CapabilityFacts {
    /// The content address of this capability.
    pub fn digest(&self) -> CapabilityDigest {
        let mut digest = Sha256::new();
        digest.update(DOMAIN);
        field(&mut digest, "lineage", self.lineage.authority.as_bytes());
        field(
            &mut digest,
            "lineage_generation",
            &self.lineage.generation.to_be_bytes(),
        );
        field(&mut digest, "provider", self.route.provider_id.as_bytes());
        field(&mut digest, "base_url", self.route.base_url.as_bytes());
        field(&mut digest, "wire_model", self.route.wire_model.as_bytes());
        field(&mut digest, "dialect", self.route.dialect.as_bytes());
        field(&mut digest, "selection_key", self.selection_key.as_bytes());
        field(&mut digest, "tier", self.tier.as_str().as_bytes());
        field(&mut digest, "profile", self.profile.as_str().as_bytes());
        field(
            &mut digest,
            "provenance",
            self.provenance.label().as_bytes(),
        );
        field(
            &mut digest,
            "provenance_id",
            self.provenance
                .provenance_id()
                .unwrap_or_default()
                .as_bytes(),
        );
        field(&mut digest, "schema_id", self.schema.id.as_bytes());
        field(
            &mut digest,
            "schema_version",
            &self.schema.version.to_be_bytes(),
        );
        field(
            &mut digest,
            "credential_fingerprint",
            self.credential.fingerprint.as_bytes(),
        );
        field(
            &mut digest,
            "credential_incarnation",
            &self.credential.incarnation.to_be_bytes(),
        );
        field(
            &mut digest,
            "policy_revision",
            &self.policy.revision.to_be_bytes(),
        );
        CapabilityDigest(format!("v1-sha256:{:x}", digest.finalize()))
    }
}

/// The content address of a [`CapabilityFacts`].
///
/// It is a one-way digest over public, secret-free identity material. It is
/// safe to show an operator and safe to persist, and it confers nothing: only
/// the live registry can say whether a digest is still current.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CapabilityDigest(String);

impl CapabilityDigest {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The digest carried by a reference that names no binding.
    #[cfg(test)]
    pub(super) fn unbound() -> Self {
        Self(hex_digest(&[(
            b"binding".as_slice(),
            b"unbound".as_slice(),
        )]))
    }

    /// Seals live capability facts together with the evidence a qualification
    /// actually produced.
    ///
    /// The two halves are separate on purpose. The capability half is
    /// re-derivable from live state at any boundary, which is what makes a
    /// downgrade detectable immediately before dispatch. The evidence half is
    /// historical — it records what was proved, once — so it is folded in here
    /// rather than re-derived. The sealed value is what a durable binding
    /// reference carries, so a reference cannot be edited to point at a
    /// different capability or a different proof.
    pub fn sealed_with(&self, evidence: &QualificationEvidence) -> Self {
        Self(hex_digest(&[
            (b"capability".as_slice(), self.0.as_bytes()),
            (
                b"evidence_kind".as_slice(),
                evidence.kind.as_str().as_bytes(),
            ),
            (b"evidence_digest".as_slice(), evidence.digest.as_bytes()),
        ]))
    }
}

impl fmt::Display for CapabilityDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The exact effect one dispatch authorization covers.
///
/// A dispatch authorization that named only the binding could be paired with
/// any action: it would say "this model may act", not "this model may perform
/// *this*". Binding the effect closes that, and makes the authorization
/// meaningless anywhere but at the action it was taken for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DispatchEffect {
    run_id: String,
    observation_id: String,
    action_class: String,
}

impl DispatchEffect {
    pub(crate) fn new(
        run_id: impl Into<String>,
        observation_id: impl Into<String>,
        action_class: impl Into<String>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            observation_id: observation_id.into(),
            action_class: action_class.into(),
        }
    }

    fn digest(&self, binding: &CapabilityDigest) -> CapabilityDigest {
        CapabilityDigest(hex_digest(&[
            (b"binding".as_slice(), binding.0.as_bytes()),
            (b"run".as_slice(), self.run_id.as_bytes()),
            (b"observation".as_slice(), self.observation_id.as_bytes()),
            (b"action_class".as_slice(), self.action_class.as_bytes()),
        ]))
    }
}

/// A single-use authorization for one exact effect.
///
/// [`Self::redeem`] takes `self`, so the type system enforces the one-shot
/// property: an authorization cannot cover a second dispatch, and it cannot be
/// transplanted onto a different action, observation, or run. `must_use`
/// because dropping one silently would mean dispatching without redeeming.
#[derive(Debug)]
#[must_use = "a dispatch lease authorizes nothing until it is redeemed"]
pub(crate) struct DispatchLease {
    effect_digest: CapabilityDigest,
}

impl DispatchLease {
    pub(crate) fn issue(binding: &CapabilityDigest, effect: &DispatchEffect) -> Self {
        Self {
            effect_digest: effect.digest(binding),
        }
    }

    /// Consumes the lease against the effect about to happen.
    pub(crate) fn redeem(
        self,
        binding: &CapabilityDigest,
        effect: &DispatchEffect,
    ) -> Result<(), super::generation::CapabilityDenied> {
        if self.effect_digest == effect.digest(binding) {
            Ok(())
        } else {
            Err(super::generation::CapabilityDenied)
        }
    }
}

/// Length-prefixed, labelled field update.
///
/// Without the length prefixes, `("ab", "c")` and `("a", "bc")` would digest
/// identically and two different capabilities could share one address.
fn field(digest: &mut Sha256, label: &str, value: &[u8]) {
    digest.update((label.len() as u64).to_be_bytes());
    digest.update(label.as_bytes());
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn hex_digest(fields: &[(&[u8], &[u8])]) -> String {
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    for (label, value) in fields {
        digest.update((label.len() as u64).to_be_bytes());
        digest.update(label);
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value);
    }
    format!("v1-sha256:{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_authority::policy::CapabilityProvenance;

    fn facts() -> CapabilityFacts {
        CapabilityFacts {
            lineage: AuthorityLineage {
                authority: "test-authority".into(),
                generation: 1,
            },
            route: NormalizedRoute::new(
                "xai",
                "https://api.x.ai/v1",
                "grok-4",
                "xai_chat_completions",
            ),
            selection_key: "xai/grok-4".into(),
            tier: ComputerUseTier::SemanticAct,
            profile: AssuranceProfile::Balanced,
            provenance: CapabilityProvenance::Measured,
            schema: QualificationSchema::new("computer-use-qualification", 1),
            credential: CredentialIncarnation {
                fingerprint: "v1-sha256:aaaa".into(),
                incarnation: 3,
            },
            policy: PolicyRevision { revision: 7 },
        }
    }

    #[test]
    fn every_bound_field_changes_the_digest() {
        let base = facts().digest();
        let mut route = facts();
        route.route.wire_model = "grok-4-fast".into();
        let mut tier = facts();
        tier.tier = ComputerUseTier::Observe;
        let mut profile = facts();
        profile.profile = AssuranceProfile::HighAssurance;
        let mut provenance = facts();
        provenance.provenance = CapabilityProvenance::DeclaredObservationOnly;
        let mut schema = facts();
        schema.schema.version = 2;
        let mut credential = facts();
        credential.credential.incarnation = 4;
        let mut policy = facts();
        policy.policy.revision = 8;
        let mut selection = facts();
        selection.selection_key = "xai/grok-4-alias".into();
        let mut lineage = facts();
        lineage.lineage.generation = 2;
        let mut authority = facts();
        authority.lineage.authority = "other-authority".into();
        for (label, drifted) in [
            ("route", route),
            ("tier", tier),
            ("profile", profile),
            ("provenance", provenance),
            ("schema", schema),
            ("credential", credential),
            ("policy", policy),
            ("selection", selection),
            ("lineage_generation", lineage),
            ("lineage_authority", authority),
        ] {
            assert_ne!(
                base,
                drifted.digest(),
                "{label} must be bound into the capability digest"
            );
        }
    }

    #[test]
    fn sealing_binds_the_evidence_a_qualification_produced() {
        let capability = facts().digest();
        let measured = capability.sealed_with(&QualificationEvidence::of(
            QualificationEvidenceKind::Measured,
            b"transcript",
        ));
        assert_eq!(
            measured,
            capability.sealed_with(&QualificationEvidence::of(
                QualificationEvidenceKind::Measured,
                b"transcript"
            ))
        );
        for other in [
            QualificationEvidence::of(QualificationEvidenceKind::Measured, b"other"),
            QualificationEvidence::of(QualificationEvidenceKind::Signed, b"transcript"),
            QualificationEvidence::absent(),
        ] {
            assert_ne!(measured, capability.sealed_with(&other));
        }
        assert_ne!(measured, capability, "sealing must not be the identity");
    }

    #[test]
    fn digest_is_stable_and_carries_no_secret() {
        let digest = facts().digest();
        assert_eq!(digest, facts().digest());
        assert!(digest.as_str().starts_with("v1-sha256:"));
        assert!(!digest.as_str().contains("api.x.ai"));
        assert!(!digest.as_str().contains("grok-4"));
    }

    #[test]
    fn field_framing_cannot_be_shifted_between_neighbours() {
        let mut left = facts();
        left.route.provider_id = "ab".into();
        left.route.base_url = "c".into();
        let mut right = facts();
        right.route.provider_id = "a".into();
        right.route.base_url = "bc".into();
        assert_ne!(left.digest(), right.digest());
    }

    #[test]
    fn a_dispatch_lease_is_one_shot_and_bound_to_its_exact_effect() {
        let binding = facts().digest();
        let effect = DispatchEffect::new("run-1", "observation-1", "semantic");
        DispatchLease::issue(&binding, &effect)
            .redeem(&binding, &effect)
            .expect("the effect it was issued for");

        for other in [
            DispatchEffect::new("run-2", "observation-1", "semantic"),
            DispatchEffect::new("run-1", "observation-2", "semantic"),
            DispatchEffect::new("run-1", "observation-1", "text_entry"),
        ] {
            assert!(
                DispatchLease::issue(&binding, &effect)
                    .redeem(&binding, &other)
                    .is_err(),
                "a lease must not transplant onto {other:?}"
            );
        }

        let mut drifted = facts();
        drifted.tier = ComputerUseTier::Observe;
        assert!(
            DispatchLease::issue(&binding, &effect)
                .redeem(&drifted.digest(), &effect)
                .is_err(),
            "a lease must not survive the capability it was issued against"
        );
    }

    #[test]
    fn normalization_merges_only_provably_identical_spellings() {
        let canonical = normalize_base_url("https://api.x.ai/v1");
        for spelling in [
            "https://API.X.AI/v1",
            "https://api.x.ai:443/v1",
            "https://api.x.ai/v1/",
            "  https://api.x.ai/v1  ",
        ] {
            assert_eq!(normalize_base_url(spelling), canonical, "{spelling}");
        }
        for distinct in [
            "http://api.x.ai/v1",
            "https://api.x.ai/v2",
            "https://api.x.ai:8443/v1",
            "https://evil.example/v1",
            "https://user@api.x.ai/v1",
            "api.x.ai/v1",
        ] {
            assert_ne!(normalize_base_url(distinct), canonical, "{distinct}");
        }
    }

    #[test]
    fn unparseable_base_urls_stay_opaque_and_distinct() {
        assert_eq!(normalize_base_url("not a url"), "not a url");
        assert_eq!(normalize_base_url("https://"), "https://");
        assert_ne!(
            normalize_base_url("https://user@host/v1"),
            normalize_base_url("https://host/v1")
        );
    }

    #[test]
    fn ipv6_authority_is_not_mangled_into_a_different_host() {
        assert_eq!(
            normalize_base_url("https://[::1]:443/v1"),
            normalize_base_url("https://[::1]/v1")
        );
        assert_ne!(
            normalize_base_url("https://[::1]:8443/v1"),
            normalize_base_url("https://[::1]/v1")
        );
    }
}
