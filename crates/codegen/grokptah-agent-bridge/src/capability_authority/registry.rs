//! The live authority: what is current, what was qualified, and what is
//! refused.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::boundary::{BoundarySet, CapabilityBoundary};
use super::digest::{
    AuthorityLineage, CapabilityDigest, CapabilityFacts, CredentialIncarnation, DispatchEffect,
    DispatchLease, NormalizedRoute, PolicyRevision, QualificationEvidence,
    QualificationEvidenceKind, QualificationSchema,
};
use super::generation::{CapabilityDenied, CapabilityGeneration};
use super::policy::{resolve_provenance, CapabilityProvenance, DeclaredCapabilityPolicy};
use super::profile::AssuranceProfile;
use crate::gateway_config::{CapabilitySource, ComputerUseTier};

/// Which session qualified which model selection.
///
/// Authority is per session *and* per exact `provider/model` selection. There
/// is no provider-wide entry, so a model whose own capability record is
/// missing cannot inherit a sibling model's qualification.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QualificationKey {
    pub session_id: Uuid,
    pub selection_key: String,
}

impl QualificationKey {
    pub fn new(session_id: Uuid, selection_key: impl Into<String>) -> Self {
        Self {
            session_id,
            selection_key: selection_key.into(),
        }
    }
}

/// A secret-free, durable reference to a binding the live authority holds.
///
/// It confers nothing on its own. The authority half of the binding lives only
/// in memory, so a reference that outlives the process — persisted in a run
/// record, restored from a backup, copied from another host — names nothing
/// and is refused exactly like a forged one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityBindingRef {
    /// Opaque handle into the live authority's binding table.
    binding_id: String,
    /// Sealed capability + evidence digest, for operator display and for
    /// detecting an edited reference.
    digest: CapabilityDigest,
    /// Generation counter the binding was minted at, for diagnostics.
    generation: u64,
}

impl CapabilityBindingRef {
    /// Operator-facing digest of the capability this reference names. Safe to
    /// display and to persist; it confers nothing.
    pub fn digest(&self) -> &CapabilityDigest {
        &self.digest
    }

    /// Generation counter the binding was minted at, for diagnostics.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// A reference that names no binding any authority holds.
    ///
    /// This is the shape a legacy or restored qualification has: a record that
    /// exists but was never issued by the live authority, refused at every
    /// boundary. Test-only — there is deliberately no production constructor
    /// for a binding reference at all, so nothing outside this authority can
    /// produce one.
    #[cfg(test)]
    pub(crate) fn unbound() -> Self {
        Self {
            binding_id: Uuid::new_v4().to_string(),
            digest: CapabilityDigest::unbound(),
            generation: 0,
        }
    }
}

/// What the current policy says about one exact route and model.
///
/// Produced by [`CapabilityRegistry::assess`] and re-produced at every
/// boundary that re-derives. It carries the generation it was taken under, so
/// an assessment that was itself computed against stale state cannot be used
/// to validate anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityAssessment {
    facts: CapabilityFacts,
    digest: CapabilityDigest,
    tier: ComputerUseTier,
    provenance: CapabilityProvenance,
    generation: CapabilityGeneration,
}

impl CapabilityAssessment {
    /// The tier that may actually be exercised, after operator policy and the
    /// assurance profile have been applied.
    pub fn tier(&self) -> ComputerUseTier {
        self.tier
    }

    pub fn provenance(&self) -> &CapabilityProvenance {
        &self.provenance
    }

    pub fn digest(&self) -> &CapabilityDigest {
        &self.digest
    }

    pub fn facts(&self) -> &CapabilityFacts {
        &self.facts
    }

    pub fn profile(&self) -> AssuranceProfile {
        self.facts.profile
    }

    pub fn generation_counter(&self) -> u64 {
        self.generation.counter()
    }
}

/// Inputs for one assessment. Everything here is public identity material;
/// no credential secret enters this module.
#[derive(Debug, Clone)]
pub(crate) struct CapabilityRequest {
    /// Upstream authority this capability descends from.
    pub(crate) lineage: AuthorityLineage,
    pub(crate) route: NormalizedRoute,
    pub(crate) selection_key: String,
    /// Provenance of the capability record as stored, before policy.
    pub(crate) source: CapabilitySource,
    /// Tier the capability record claims, before policy.
    pub(crate) claimed_tier: ComputerUseTier,
    /// Secret-free credential principal fingerprint.
    pub(crate) credential_fingerprint: String,
}

#[derive(Debug, Clone)]
struct CredentialSlot {
    fingerprint: String,
    incarnation: u64,
}

#[derive(Debug, Clone)]
struct BoundQualification {
    key: QualificationKey,
    generation: CapabilityGeneration,
    capability: CapabilityDigest,
    sealed: CapabilityDigest,
    boundaries: BoundarySet,
    profile: AssuranceProfile,
    minted_at: Instant,
    dispatches: u32,
}

struct RegistryState {
    generation: CapabilityGeneration,
    profile: AssuranceProfile,
    declared_policy: DeclaredCapabilityPolicy,
    schema: QualificationSchema,
    policy_revision: u64,
    credentials: BTreeMap<String, CredentialSlot>,
    bindings: HashMap<String, BoundQualification>,
    index: HashMap<QualificationKey, String>,
    quarantined: BTreeSet<QualificationKey>,
}

impl RegistryState {
    /// Advances the generation *before* any state is written.
    ///
    /// Exhaustion returns the refusal with nothing mutated, so a host that
    /// cannot rotate cannot half-rotate either. The advance itself invalidates
    /// every binding: bindings are compared against the registry's live
    /// generation, so one increment retires all of them at once.
    fn advance(&mut self) -> Result<(), CapabilityDenied> {
        let next = self.generation.next()?;
        self.generation = next;
        Ok(())
    }
}

/// The single fail-closed provider capability authority.
pub struct CapabilityRegistry {
    state: Mutex<RegistryState>,
}

impl std::fmt::Debug for CapabilityRegistry {
    /// Shows only what an operator may see: the generation counter, the
    /// profile, and how many qualifications stand. The authority id, the
    /// bindings and the credential fingerprints stay out of debug output.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock();
        f.debug_struct("CapabilityRegistry")
            .field("generation", &state.generation.counter())
            .field("profile", &state.profile.as_str())
            .field("policy_revision", &state.policy_revision)
            .field("bindings", &state.bindings.len())
            .field("quarantined", &state.quarantined.len())
            .finish()
    }
}

impl CapabilityRegistry {
    /// Builds a registry for one process.
    ///
    /// Crate-internal: the host is the only thing that may own a capability
    /// authority. A caller that could build its own — or reach the host's —
    /// could mint the authority this type exists to withhold.
    ///
    /// The assurance profile and the declared-capability policy are taken
    /// explicitly: neither has a silent default that a deployment could
    /// inherit without deciding it.
    pub(crate) fn new(
        profile: AssuranceProfile,
        declared_policy: DeclaredCapabilityPolicy,
        schema: QualificationSchema,
    ) -> Self {
        Self {
            state: Mutex::new(RegistryState {
                generation: CapabilityGeneration::new_authority(),
                profile,
                declared_policy,
                schema,
                policy_revision: 0,
                credentials: BTreeMap::new(),
                bindings: HashMap::new(),
                index: HashMap::new(),
                quarantined: BTreeSet::new(),
            }),
        }
    }

    /// State observers. They exist so a test can assert that a *refused*
    /// mutation changed nothing, which is the property that makes exhaustion
    /// safe; nothing in production reads them.
    #[cfg(test)]
    pub(crate) fn generation_counter(&self) -> u64 {
        self.state.lock().generation.counter()
    }

    #[cfg(test)]
    pub(crate) fn profile(&self) -> AssuranceProfile {
        self.state.lock().profile
    }

    #[cfg(test)]
    pub(crate) fn policy_revision(&self) -> PolicyRevision {
        PolicyRevision {
            revision: self.state.lock().policy_revision,
        }
    }

    /// Explicit revocation of every standing qualification.
    pub(crate) fn revoke_all(&self) -> Result<(), CapabilityDenied> {
        let mut state = self.state.lock();
        state.advance()?;
        state.bindings.clear();
        state.index.clear();
        Ok(())
    }

    /// Explicit revocation of one session's qualifications.
    ///
    /// This still advances the generation. A revocation that only removed a
    /// map entry would leave every other binding provably current at the same
    /// stamp, which is fine, but it would also let a caller distinguish "this
    /// one was revoked" from "everything moved on" by timing. One advance for
    /// every revocation keeps that uniform.
    pub(crate) fn revoke_session(&self, session_id: Uuid) -> Result<(), CapabilityDenied> {
        let mut state = self.state.lock();
        state.advance()?;
        let doomed: Vec<QualificationKey> = state
            .index
            .keys()
            .filter(|key| key.session_id == session_id)
            .cloned()
            .collect();
        for key in doomed {
            if let Some(id) = state.index.remove(&key) {
                state.bindings.remove(&id);
            }
        }
        Ok(())
    }

    /// A requalification attempt that did not pass.
    ///
    /// The previous binding does not survive a failed re-proof: whatever it
    /// once demonstrated, the model has just failed to demonstrate now.
    pub(crate) fn record_requalification_failure(
        &self,
        key: &QualificationKey,
    ) -> Result<(), CapabilityDenied> {
        let mut state = self.state.lock();
        state.advance()?;
        if let Some(id) = state.index.remove(key) {
            state.bindings.remove(&id);
        }
        Ok(())
    }

    /// Changes the assurance profile. Every binding taken under the old
    /// profile stops being current.
    ///
    /// Operator capability policy is read once at startup, so there is no
    /// production caller yet: changing it today means restarting, which draws
    /// a fresh authority and retires everything anyway. These setters exist so
    /// the invalidation contract is proven now and holds unchanged when a
    /// runtime settings surface lands.
    #[cfg(test)]
    pub(crate) fn set_profile(&self, profile: AssuranceProfile) -> Result<(), CapabilityDenied> {
        let mut state = self.state.lock();
        if state.profile == profile {
            return Ok(());
        }
        state.advance()?;
        state.profile = profile;
        Ok(())
    }

    /// Changes operator policy for declared capability. See
    /// [`Self::set_profile`] on why this has no production caller yet.
    #[cfg(test)]
    pub(crate) fn set_declared_policy(
        &self,
        declared_policy: DeclaredCapabilityPolicy,
    ) -> Result<(), CapabilityDenied> {
        let mut state = self.state.lock();
        if state.declared_policy == declared_policy {
            return Ok(());
        }
        state.advance()?;
        state.declared_policy = declared_policy;
        Ok(())
    }

    /// Records a change to the qualification schema this host proves against.
    /// See [`Self::set_profile`] on why this has no production caller yet.
    #[cfg(test)]
    pub(crate) fn set_qualification_schema(
        &self,
        schema: QualificationSchema,
    ) -> Result<(), CapabilityDenied> {
        let mut state = self.state.lock();
        if state.schema == schema {
            return Ok(());
        }
        state.advance()?;
        state.schema = schema;
        Ok(())
    }

    /// Records any other operator policy or allowlist change that a capability
    /// depends on.
    pub(crate) fn bump_policy_revision(&self) -> Result<(), CapabilityDenied> {
        let mut state = self.state.lock();
        let next_revision = state
            .policy_revision
            .checked_add(1)
            .ok_or(CapabilityDenied)?;
        state.advance()?;
        state.policy_revision = next_revision;
        Ok(())
    }

    /// Records the credential principal currently serving a provider.
    ///
    /// A fingerprint that differs from the stored one is a rotation: the
    /// incarnation advances and every binding is retired. An unchanged
    /// fingerprint is a no-op, so ordinary token refresh on a stable principal
    /// does not churn authority.
    pub(crate) fn observe_credential(
        &self,
        provider_id: &str,
        fingerprint: &str,
    ) -> Result<CredentialIncarnation, CapabilityDenied> {
        let mut state = self.state.lock();
        if let Some(slot) = state.credentials.get(provider_id) {
            if slot.fingerprint == fingerprint {
                return Ok(CredentialIncarnation {
                    fingerprint: slot.fingerprint.clone(),
                    incarnation: slot.incarnation,
                });
            }
        }
        let next_incarnation = state
            .credentials
            .get(provider_id)
            .map(|slot| slot.incarnation.checked_add(1).ok_or(CapabilityDenied))
            .transpose()?
            .unwrap_or(0);
        state.advance()?;
        let slot = CredentialSlot {
            fingerprint: fingerprint.to_string(),
            incarnation: next_incarnation,
        };
        let incarnation = CredentialIncarnation {
            fingerprint: slot.fingerprint.clone(),
            incarnation: slot.incarnation,
        };
        state.credentials.insert(provider_id.to_string(), slot);
        state.bindings.clear();
        state.index.clear();
        Ok(incarnation)
    }

    /// Records that a provider's credential was removed.
    ///
    /// The slot is kept with its incarnation advanced rather than deleted, so
    /// re-adding byte-identical credential material lands on a *new*
    /// incarnation and cannot inherit the deleted credential's authority.
    pub(crate) fn forget_credential(&self, provider_id: &str) -> Result<(), CapabilityDenied> {
        let mut state = self.state.lock();
        let Some(slot) = state.credentials.get(provider_id).cloned() else {
            return Ok(());
        };
        let next_incarnation = slot.incarnation.checked_add(1).ok_or(CapabilityDenied)?;
        state.advance()?;
        state.credentials.insert(
            provider_id.to_string(),
            CredentialSlot {
                fingerprint: String::new(),
                incarnation: next_incarnation,
            },
        );
        state.bindings.clear();
        state.index.clear();
        Ok(())
    }

    /// Computes what current policy says about one exact route and model.
    ///
    /// The credential must already have been observed: an unobserved provider
    /// has no incarnation, and inventing one would let a rotation that this
    /// authority never saw pass unnoticed.
    pub(crate) fn assess(
        &self,
        request: &CapabilityRequest,
    ) -> Result<CapabilityAssessment, CapabilityDenied> {
        let state = self.state.lock();
        if state.generation.is_exhausted() {
            return Err(CapabilityDenied);
        }
        let slot = state
            .credentials
            .get(&request.route.provider_id)
            .filter(|slot| slot.fingerprint == request.credential_fingerprint)
            .ok_or(CapabilityDenied)?;
        let (tier, provenance) = resolve_provenance(
            request.source,
            request.claimed_tier,
            state.profile,
            &state.declared_policy,
        );
        let facts = CapabilityFacts {
            lineage: request.lineage.clone(),
            route: request.route.clone(),
            selection_key: request.selection_key.clone(),
            tier,
            profile: state.profile,
            provenance: provenance.clone(),
            schema: state.schema.clone(),
            credential: CredentialIncarnation {
                fingerprint: slot.fingerprint.clone(),
                incarnation: slot.incarnation,
            },
            policy: PolicyRevision {
                revision: state.policy_revision,
            },
        };
        Ok(CapabilityAssessment {
            digest: facts.digest(),
            facts,
            tier,
            provenance,
            generation: state.generation,
        })
    }

    /// Records one qualification and mints its binding.
    ///
    /// Minting does not advance the generation: qualifying one session must
    /// not retire another session's standing authority.
    pub(crate) fn qualify(
        &self,
        key: &QualificationKey,
        assessment: &CapabilityAssessment,
        evidence: &QualificationEvidence,
    ) -> Result<CapabilityBindingRef, CapabilityDenied> {
        let mut state = self.state.lock();
        if state.generation.is_exhausted() || assessment.generation != state.generation {
            return Err(CapabilityDenied);
        }
        if assessment.facts.selection_key != key.selection_key
            || assessment.facts.profile != state.profile
        {
            return Err(CapabilityDenied);
        }
        if assessment.tier == ComputerUseTier::None {
            return Err(CapabilityDenied);
        }
        if assessment.tier >= ComputerUseTier::SemanticAct
            && !action_evidence_is_sufficient(&assessment.provenance, state.profile, evidence.kind)
        {
            return Err(CapabilityDenied);
        }
        let sealed = assessment.digest.sealed_with(evidence);
        let binding_id = Uuid::new_v4().to_string();
        let record = BoundQualification {
            key: key.clone(),
            generation: state.generation,
            capability: assessment.digest.clone(),
            sealed: sealed.clone(),
            boundaries: BoundarySet::for_tier(assessment.tier),
            profile: state.profile,
            minted_at: Instant::now(),
            dispatches: 0,
        };
        let reference = CapabilityBindingRef {
            binding_id: binding_id.clone(),
            digest: sealed,
            generation: state.generation.counter(),
        };
        if let Some(previous) = state.index.insert(key.clone(), binding_id.clone()) {
            state.bindings.remove(&previous);
        }
        state.quarantined.remove(key);
        state.bindings.insert(binding_id, record);
        Ok(reference)
    }

    /// Re-validates a binding at one boundary against freshly derived live
    /// facts.
    ///
    /// Every failure returns the same refusal. A binding from another
    /// authority, a binding that never existed, one that was revoked, one
    /// whose generation moved, one whose facts drifted, one presented by the
    /// wrong session, one past its profile's lifetime or dispatch budget, and
    /// one presented at a boundary its tier does not reach are all the same
    /// answer.
    pub(crate) fn validate(
        &self,
        session_id: Uuid,
        reference: &CapabilityBindingRef,
        boundary: CapabilityBoundary,
        live: &CapabilityAssessment,
    ) -> Result<(), CapabilityDenied> {
        let mut state = self.state.lock();
        if state.generation.is_exhausted() {
            return Err(CapabilityDenied);
        }
        if live.generation != state.generation || live.facts.profile != state.profile {
            return Err(CapabilityDenied);
        }
        let profile = state.profile;
        let ceilings = profile.ceilings();
        let record = state
            .bindings
            .get_mut(&reference.binding_id)
            .ok_or(CapabilityDenied)?;
        if record.key.session_id != session_id
            || record.key.selection_key != live.facts.selection_key
            || record.generation != live.generation
            || record.profile != profile
            || record.capability != live.digest
            || record.sealed != reference.digest
            || reference.generation != record.generation.counter()
            || !record.boundaries.allows(boundary)
        {
            return Err(CapabilityDenied);
        }
        if record.minted_at.elapsed() > Duration::from_secs(ceilings.max_qualification_age_secs) {
            return Err(CapabilityDenied);
        }
        if boundary.consumes_dispatch_budget() {
            let next = record.dispatches.checked_add(1).ok_or(CapabilityDenied)?;
            if next > ceilings.max_dispatches_per_qualification {
                return Err(CapabilityDenied);
            }
            record.dispatches = next;
        }
        Ok(())
    }

    /// Authorizes one exact dispatch and issues its single-use lease.
    ///
    /// This is [`Self::validate`] at the dispatch boundary plus the effect the
    /// authorization covers. A caller cannot hold a general "this model may
    /// act" result and pair it with whatever action it likes: the lease
    /// redeems only against the run, observation, and action class it was
    /// taken for, and only once.
    pub(crate) fn authorize_dispatch(
        &self,
        session_id: Uuid,
        reference: &CapabilityBindingRef,
        live: &CapabilityAssessment,
        effect: &DispatchEffect,
    ) -> Result<DispatchLease, CapabilityDenied> {
        self.validate(session_id, reference, CapabilityBoundary::Dispatch, live)?;
        Ok(DispatchLease::issue(&live.digest, effect))
    }

    /// Records a qualification that exists but is not bound to any capability
    /// generation this authority issued — a legacy in-memory record, a
    /// restored snapshot, or a reference whose binding could not be found.
    ///
    /// A quarantined qualification is never attributed to current authority.
    /// It has to be re-established by a fresh qualification; nothing promotes
    /// it in place.
    ///
    /// The production path is [`Self::quarantine_if_unbound`], which only
    /// quarantines a reference this authority does not hold. This unconditional
    /// form exists so the "never promoted in place" contract can be exercised
    /// directly.
    #[cfg(test)]
    pub(crate) fn quarantine_legacy(&self, key: &QualificationKey) {
        let mut state = self.state.lock();
        if let Some(id) = state.index.remove(key) {
            state.bindings.remove(&id);
        }
        state.quarantined.insert(key.clone());
    }

    /// Quarantines `key` only when `reference` names no binding this authority
    /// holds.
    ///
    /// That is the unbound case: a reference restored from a durable record
    /// written by an earlier process, copied from another host, or otherwise
    /// never issued here. It is recorded as needing explicit re-establishment
    /// rather than left to look like an ordinary stale binding. A reference
    /// this authority *does* hold is left alone, so an ordinary refusal (a
    /// downgrade, a spent budget) does not tear down a binding a
    /// requalification would otherwise refresh in place.
    pub(crate) fn quarantine_if_unbound(
        &self,
        key: &QualificationKey,
        reference: &CapabilityBindingRef,
    ) {
        let mut state = self.state.lock();
        if state.bindings.contains_key(&reference.binding_id) {
            return;
        }
        if let Some(id) = state.index.remove(key) {
            state.bindings.remove(&id);
        }
        state.quarantined.insert(key.clone());
    }

    #[cfg(test)]
    pub(crate) fn is_quarantined(&self, key: &QualificationKey) -> bool {
        self.state.lock().quarantined.contains(key)
    }

    /// Whether this authority currently holds a binding for `key`. It says
    /// nothing about whether that binding would pass a boundary.
    #[cfg(test)]
    pub(crate) fn has_binding(&self, key: &QualificationKey) -> bool {
        self.state.lock().index.contains_key(key)
    }

    /// Pins the authority one advance short of exhaustion, so exhaustion can
    /// be exercised without 2^64 rotations.
    #[cfg(test)]
    pub(crate) fn pin_near_exhaustion_for_test(&self) {
        let mut state = self.state.lock();
        state.generation = state.generation.pinned_near_exhaustion();
    }
}

/// Whether `evidence` is strong enough to carry action authority.
///
/// The ordinary rule is the profile's minimum evidence class: action authority
/// needs a probe that ran. The one exception is capability the operator has
/// explicitly trusted by provenance — there the operator's own configuration
/// *is* the evidence, and requiring a probe on top of it would make the policy
/// unusable rather than safer. That exception is narrow on purpose: it applies
/// only to `DeclaredTrusted`, which `resolve_provenance` produces only under an
/// explicit named trust policy and only in a profile that honours it, and it
/// still refuses evidence that was never recorded at all.
fn action_evidence_is_sufficient(
    provenance: &CapabilityProvenance,
    profile: AssuranceProfile,
    evidence: QualificationEvidenceKind,
) -> bool {
    match provenance {
        CapabilityProvenance::DeclaredTrusted { .. } => {
            evidence >= QualificationEvidenceKind::Declared
        }
        CapabilityProvenance::Measured | CapabilityProvenance::Signed => {
            profile.admits_action_evidence(evidence)
        }
        CapabilityProvenance::Unknown | CapabilityProvenance::DeclaredObservationOnly => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA_ID: &str = "computer-use-qualification";
    const FINGERPRINT: &str = "v1-sha256:principal-a";

    fn registry(profile: AssuranceProfile) -> CapabilityRegistry {
        let registry = CapabilityRegistry::new(
            profile,
            DeclaredCapabilityPolicy::default(),
            QualificationSchema::new(SCHEMA_ID, 1),
        );
        registry
            .observe_credential("xai", FINGERPRINT)
            .expect("seed");
        registry
    }

    fn request(source: CapabilitySource, tier: ComputerUseTier) -> CapabilityRequest {
        CapabilityRequest {
            lineage: AuthorityLineage {
                authority: "test-host".into(),
                generation: 1,
            },
            route: NormalizedRoute::new(
                "xai",
                "https://api.x.ai/v1",
                "grok-4",
                "xai_chat_completions",
            ),
            selection_key: "xai/grok-4".into(),
            source,
            claimed_tier: tier,
            credential_fingerprint: FINGERPRINT.into(),
        }
    }

    fn evidence() -> QualificationEvidence {
        QualificationEvidence::of(QualificationEvidenceKind::Measured, b"transcript")
    }

    fn qualified(
        registry: &CapabilityRegistry,
        session: Uuid,
    ) -> (QualificationKey, CapabilityBindingRef) {
        let key = QualificationKey::new(session, "xai/grok-4");
        let assessment = registry
            .assess(&request(
                CapabilitySource::Measured,
                ComputerUseTier::SemanticAct,
            ))
            .expect("assess");
        let reference = registry
            .qualify(&key, &assessment, &evidence())
            .expect("qualify");
        (key, reference)
    }

    #[test]
    fn a_fresh_binding_passes_every_boundary_its_tier_reaches() {
        let registry = registry(AssuranceProfile::Balanced);
        let session = Uuid::new_v4();
        let (_, reference) = qualified(&registry, session);
        for boundary in CapabilityBoundary::ALL {
            let live = registry
                .assess(&request(
                    CapabilitySource::Measured,
                    ComputerUseTier::SemanticAct,
                ))
                .expect("assess");
            registry
                .validate(session, &reference, boundary, &live)
                .unwrap_or_else(|_| panic!("{boundary:?} must pass on a current binding"));
        }
    }

    #[test]
    fn a_tier_downgrade_between_qualification_and_dispatch_is_refused() {
        let registry = registry(AssuranceProfile::Balanced);
        let session = Uuid::new_v4();
        let (_, reference) = qualified(&registry, session);
        let downgraded = registry
            .assess(&request(
                CapabilitySource::Measured,
                ComputerUseTier::Observe,
            ))
            .expect("assess");
        assert_eq!(
            registry.validate(
                session,
                &reference,
                CapabilityBoundary::Dispatch,
                &downgraded
            ),
            Err(CapabilityDenied)
        );
    }

    #[test]
    fn revocation_rotation_and_policy_change_each_retire_a_binding() {
        for mutate in [
            (|r: &CapabilityRegistry| r.revoke_all()) as fn(&CapabilityRegistry) -> _,
            |r: &CapabilityRegistry| {
                r.observe_credential("xai", "v1-sha256:principal-b")
                    .map(|_| ())
            },
            |r: &CapabilityRegistry| r.forget_credential("xai"),
            |r: &CapabilityRegistry| r.bump_policy_revision(),
            |r: &CapabilityRegistry| r.set_profile(AssuranceProfile::Economy),
            |r: &CapabilityRegistry| {
                r.set_qualification_schema(QualificationSchema::new(SCHEMA_ID, 2))
            },
            |r: &CapabilityRegistry| {
                r.set_declared_policy(
                    DeclaredCapabilityPolicy::trusted("operator-manifest").expect("policy"),
                )
            },
        ] {
            let registry = registry(AssuranceProfile::Balanced);
            let session = Uuid::new_v4();
            let (_, reference) = qualified(&registry, session);
            mutate(&registry).expect("mutation");
            // A rotation or deletion also removes the credential the request
            // names, so re-seed exactly as a host would before re-deriving.
            registry
                .observe_credential("xai", FINGERPRINT)
                .expect("re-observe");
            let live = registry
                .assess(&request(
                    CapabilitySource::Measured,
                    ComputerUseTier::SemanticAct,
                ))
                .expect("assess");
            assert_eq!(
                registry.validate(session, &reference, CapabilityBoundary::Dispatch, &live),
                Err(CapabilityDenied)
            );
        }
    }

    #[test]
    fn foreign_unknown_revoked_and_stale_denials_are_identical() {
        let registry = registry(AssuranceProfile::Balanced);
        let session = Uuid::new_v4();
        let (_, reference) = qualified(&registry, session);
        let live = registry
            .assess(&request(
                CapabilitySource::Measured,
                ComputerUseTier::SemanticAct,
            ))
            .expect("assess");

        let foreign_session = registry.validate(
            Uuid::new_v4(),
            &reference,
            CapabilityBoundary::Dispatch,
            &live,
        );
        let unknown = registry.validate(
            session,
            &CapabilityBindingRef {
                binding_id: Uuid::new_v4().to_string(),
                digest: reference.digest.clone(),
                generation: reference.generation,
            },
            CapabilityBoundary::Dispatch,
            &live,
        );
        let other_authority = super::tests::registry(AssuranceProfile::Balanced);
        let other_session = Uuid::new_v4();
        let (_, other_reference) = qualified(&other_authority, other_session);
        let foreign_authority = registry.validate(
            other_session,
            &other_reference,
            CapabilityBoundary::Dispatch,
            &live,
        );
        registry.revoke_all().expect("revoke");
        let stale_live = registry
            .assess(&request(
                CapabilitySource::Measured,
                ComputerUseTier::SemanticAct,
            ))
            .expect("assess");
        let revoked = registry.validate(
            session,
            &reference,
            CapabilityBoundary::Dispatch,
            &stale_live,
        );

        for outcome in [foreign_session, unknown, foreign_authority, revoked] {
            assert_eq!(outcome, Err(CapabilityDenied));
            assert_eq!(
                outcome.unwrap_err().to_string(),
                CapabilityDenied::MESSAGE,
                "denials must be byte-identical"
            );
        }
    }

    #[test]
    fn declared_capability_never_mints_action_authority_by_default() {
        let registry = registry(AssuranceProfile::Balanced);
        let session = Uuid::new_v4();
        let key = QualificationKey::new(session, "xai/grok-4");
        let assessment = registry
            .assess(&request(
                CapabilitySource::Declared,
                ComputerUseTier::VisualFallbackAct,
            ))
            .expect("assess");
        assert_eq!(assessment.tier(), ComputerUseTier::Observe);
        let reference = registry
            .qualify(&key, &assessment, &QualificationEvidence::absent())
            .expect("observation-only qualification is allowed");
        let live = registry
            .assess(&request(
                CapabilitySource::Declared,
                ComputerUseTier::VisualFallbackAct,
            ))
            .expect("assess");
        registry
            .validate(session, &reference, CapabilityBoundary::Observation, &live)
            .expect("observation is permitted");
        for boundary in [
            CapabilityBoundary::Proposal,
            CapabilityBoundary::Staging,
            CapabilityBoundary::Approval,
            CapabilityBoundary::Lease,
            CapabilityBoundary::Dispatch,
        ] {
            assert_eq!(
                registry.validate(session, &reference, boundary, &live),
                Err(CapabilityDenied),
                "{boundary:?}"
            );
        }
    }

    #[test]
    fn action_authority_requires_evidence_the_profile_accepts() {
        let registry = registry(AssuranceProfile::HighAssurance);
        let session = Uuid::new_v4();
        let key = QualificationKey::new(session, "xai/grok-4");
        let assessment = registry
            .assess(&request(
                CapabilitySource::Measured,
                ComputerUseTier::SemanticAct,
            ))
            .expect("assess");
        assert_eq!(
            registry.qualify(&key, &assessment, &evidence()),
            Err(CapabilityDenied),
            "high assurance must refuse measured-only action authority"
        );
        let signed = QualificationEvidence::of(QualificationEvidenceKind::Signed, b"transcript");
        registry
            .qualify(&key, &assessment, &signed)
            .expect("signed evidence qualifies");
    }

    #[test]
    fn explicitly_trusted_declaration_carries_action_authority_and_publishes_its_source() {
        let registry = registry(AssuranceProfile::Balanced);
        registry
            .set_declared_policy(
                DeclaredCapabilityPolicy::trusted("operator-manifest").expect("policy"),
            )
            .expect("policy change");
        registry
            .observe_credential("xai", FINGERPRINT)
            .expect("re-observe");
        let session = Uuid::new_v4();
        let key = QualificationKey::new(session, "xai/grok-4");
        let assessment = registry
            .assess(&request(
                CapabilitySource::Declared,
                ComputerUseTier::SemanticAct,
            ))
            .expect("assess");
        assert_eq!(assessment.tier(), ComputerUseTier::SemanticAct);
        assert_eq!(
            assessment.provenance().provenance_id(),
            Some("operator-manifest"),
            "the trusted source must be published in the binding"
        );
        assert_eq!(
            registry.qualify(&key, &assessment, &QualificationEvidence::absent()),
            Err(CapabilityDenied),
            "trust does not excuse a qualification that recorded nothing"
        );
        let declared =
            QualificationEvidence::of(QualificationEvidenceKind::Declared, b"operator-manifest");
        let reference = registry
            .qualify(&key, &assessment, &declared)
            .expect("explicit trust qualifies");
        let live = registry
            .assess(&request(
                CapabilitySource::Declared,
                ComputerUseTier::SemanticAct,
            ))
            .expect("assess");
        registry
            .validate(session, &reference, CapabilityBoundary::Dispatch, &live)
            .expect("trusted declaration dispatches");

        // Withdrawing the trust invalidates it immediately.
        registry
            .set_declared_policy(DeclaredCapabilityPolicy::ObservationOnly)
            .expect("withdraw trust");
        registry
            .observe_credential("xai", FINGERPRINT)
            .expect("re-observe");
        let after = registry
            .assess(&request(
                CapabilitySource::Declared,
                ComputerUseTier::SemanticAct,
            ))
            .expect("assess");
        assert_eq!(after.tier(), ComputerUseTier::Observe);
        assert_eq!(
            registry.validate(session, &reference, CapabilityBoundary::Dispatch, &after),
            Err(CapabilityDenied)
        );
    }

    /// A stored capability record is a file, not a proof.
    ///
    /// The host now presents a stored record as declared-class evidence, so a
    /// record asserting `measured` cannot buy action authority: measured means
    /// this authority measured it.
    #[test]
    fn a_measured_claim_cannot_be_bought_with_declared_class_evidence() {
        let registry = registry(AssuranceProfile::Balanced);
        let session = Uuid::new_v4();
        let key = QualificationKey::new(session, "xai/grok-4");
        let assessment = registry
            .assess(&request(
                CapabilitySource::Measured,
                ComputerUseTier::SemanticAct,
            ))
            .expect("assess");
        assert_eq!(assessment.provenance(), &CapabilityProvenance::Measured);
        for asserted in [
            QualificationEvidence::absent(),
            QualificationEvidence::of(QualificationEvidenceKind::Declared, b"stored-record"),
        ] {
            assert_eq!(
                registry.qualify(&key, &assessment, &asserted),
                Err(CapabilityDenied),
                "a stored assertion must not stand in for a measurement"
            );
        }
        registry
            .qualify(&key, &assessment, &evidence())
            .expect("a measurement this authority took does qualify");
    }

    #[test]
    fn a_dispatch_budget_is_per_qualification_and_never_refills_itself() {
        let registry = registry(AssuranceProfile::HighAssurance);
        let session = Uuid::new_v4();
        let key = QualificationKey::new(session, "xai/grok-4");
        let assessment = registry
            .assess(&request(
                CapabilitySource::Measured,
                ComputerUseTier::SemanticAct,
            ))
            .expect("assess");
        let signed = QualificationEvidence::of(QualificationEvidenceKind::Signed, b"transcript");
        let reference = registry
            .qualify(&key, &assessment, &signed)
            .expect("qualify");
        let live = registry
            .assess(&request(
                CapabilitySource::Measured,
                ComputerUseTier::SemanticAct,
            ))
            .expect("assess");
        registry
            .validate(session, &reference, CapabilityBoundary::Dispatch, &live)
            .expect("first dispatch");
        assert_eq!(
            registry.validate(session, &reference, CapabilityBoundary::Dispatch, &live),
            Err(CapabilityDenied),
            "high assurance allows one dispatch per qualification"
        );
    }

    #[test]
    fn an_edited_binding_reference_is_refused() {
        let registry = registry(AssuranceProfile::Balanced);
        let session = Uuid::new_v4();
        let (_, reference) = qualified(&registry, session);
        let live = registry
            .assess(&request(
                CapabilitySource::Measured,
                ComputerUseTier::SemanticAct,
            ))
            .expect("assess");
        let mut edited = reference.clone();
        edited.generation = reference.generation.wrapping_add(1);
        assert_eq!(
            registry.validate(session, &edited, CapabilityBoundary::Dispatch, &live),
            Err(CapabilityDenied)
        );
        let mut redigested = reference.clone();
        redigested.digest = live.digest().clone();
        assert_eq!(
            registry.validate(session, &redigested, CapabilityBoundary::Dispatch, &live),
            Err(CapabilityDenied)
        );
    }

    #[test]
    fn exhaustion_changes_nothing_and_refuses_everything_after() {
        let registry = registry(AssuranceProfile::Balanced);
        let session = Uuid::new_v4();
        let (key, _) = qualified(&registry, session);
        registry.pin_near_exhaustion_for_test();
        let before = registry.generation_counter();

        // The last advance is a normal revocation: it succeeds and retires
        // everything.
        registry.revoke_all().expect("terminal advance");
        assert!(!registry.has_binding(&key));
        let terminal = registry.generation_counter();
        assert!(terminal > before);

        // Everything after it changes nothing.
        for outcome in [
            registry.revoke_all(),
            registry.bump_policy_revision(),
            registry.set_profile(AssuranceProfile::Economy),
            registry.forget_credential("xai"),
            registry
                .observe_credential("xai", "v1-sha256:principal-c")
                .map(|_| ()),
        ] {
            assert_eq!(outcome, Err(CapabilityDenied));
        }
        assert_eq!(registry.generation_counter(), terminal);
        assert_eq!(registry.profile(), AssuranceProfile::Balanced);
        assert_eq!(
            registry.policy_revision(),
            PolicyRevision { revision: 0 },
            "a refused advance must not have moved policy"
        );
        assert_eq!(
            registry
                .assess(&request(
                    CapabilitySource::Measured,
                    ComputerUseTier::SemanticAct
                ))
                .err(),
            Some(CapabilityDenied),
            "an exhausted authority qualifies nothing"
        );
    }

    #[test]
    fn a_credential_removed_and_re_added_never_lands_on_its_old_incarnation() {
        let registry = registry(AssuranceProfile::Balanced);
        let first = registry
            .observe_credential("xai", FINGERPRINT)
            .expect("observe");
        registry.forget_credential("xai").expect("forget");
        let second = registry
            .observe_credential("xai", FINGERPRINT)
            .expect("re-add");
        assert_eq!(first.fingerprint, second.fingerprint);
        assert!(
            second.incarnation > first.incarnation,
            "re-adding identical credential material must not reuse an incarnation"
        );
    }

    #[test]
    fn a_legacy_qualification_is_quarantined_and_never_promoted_in_place() {
        let registry = registry(AssuranceProfile::Balanced);
        let session = Uuid::new_v4();
        let (key, reference) = qualified(&registry, session);
        registry.quarantine_legacy(&key);
        assert!(registry.is_quarantined(&key));
        assert!(!registry.has_binding(&key));
        let live = registry
            .assess(&request(
                CapabilitySource::Measured,
                ComputerUseTier::SemanticAct,
            ))
            .expect("assess");
        assert_eq!(
            registry.validate(session, &reference, CapabilityBoundary::Observation, &live),
            Err(CapabilityDenied)
        );
        let fresh = registry
            .qualify(&key, &live, &evidence())
            .expect("requalify");
        assert!(!registry.is_quarantined(&key));
        registry
            .validate(session, &fresh, CapabilityBoundary::Observation, &live)
            .expect("an explicit re-establishment restores authority");
    }

    #[test]
    fn an_unobserved_credential_cannot_be_assessed() {
        let registry = CapabilityRegistry::new(
            AssuranceProfile::Balanced,
            DeclaredCapabilityPolicy::default(),
            QualificationSchema::new(SCHEMA_ID, 1),
        );
        assert_eq!(
            registry
                .assess(&request(
                    CapabilitySource::Measured,
                    ComputerUseTier::SemanticAct
                ))
                .err(),
            Some(CapabilityDenied)
        );
    }

    #[test]
    fn a_failed_requalification_retires_the_standing_binding() {
        let registry = registry(AssuranceProfile::Balanced);
        let session = Uuid::new_v4();
        let (key, reference) = qualified(&registry, session);
        registry
            .record_requalification_failure(&key)
            .expect("record failure");
        let live = registry
            .assess(&request(
                CapabilitySource::Measured,
                ComputerUseTier::SemanticAct,
            ))
            .expect("assess");
        assert_eq!(
            registry.validate(session, &reference, CapabilityBoundary::Proposal, &live),
            Err(CapabilityDenied)
        );
    }

    #[test]
    fn a_second_qualification_retires_the_first_reference() {
        let registry = registry(AssuranceProfile::Balanced);
        let session = Uuid::new_v4();
        let (key, first) = qualified(&registry, session);
        let live = registry
            .assess(&request(
                CapabilitySource::Measured,
                ComputerUseTier::SemanticAct,
            ))
            .expect("assess");
        let second = registry
            .qualify(&key, &live, &evidence())
            .expect("requalify");
        assert_ne!(first.binding_id, second.binding_id);
        assert_eq!(
            registry.validate(session, &first, CapabilityBoundary::Dispatch, &live),
            Err(CapabilityDenied)
        );
        registry
            .validate(session, &second, CapabilityBoundary::Dispatch, &live)
            .expect("the current binding still works");
    }

    #[test]
    fn an_upstream_lineage_rotation_retires_every_capability_binding() {
        let registry = registry(AssuranceProfile::Balanced);
        let session = Uuid::new_v4();
        let (_, reference) = qualified(&registry, session);
        let mut rotated = request(CapabilitySource::Measured, ComputerUseTier::SemanticAct);
        rotated.lineage.generation = 2;
        let live = registry.assess(&rotated).expect("assess");
        assert_eq!(
            registry.validate(session, &reference, CapabilityBoundary::Dispatch, &live),
            Err(CapabilityDenied),
            "a capability must not outlive the authority it descends from"
        );
    }

    #[test]
    fn a_dispatch_lease_covers_one_effect_and_no_other() {
        let registry = registry(AssuranceProfile::Balanced);
        let session = Uuid::new_v4();
        let (_, reference) = qualified(&registry, session);
        let live = registry
            .assess(&request(
                CapabilitySource::Measured,
                ComputerUseTier::SemanticAct,
            ))
            .expect("assess");
        let effect = DispatchEffect::new("run-1", "observation-1", "semantic");
        let lease = registry
            .authorize_dispatch(session, &reference, &live, &effect)
            .expect("authorize dispatch");
        assert!(
            lease
                .redeem(
                    live.digest(),
                    &DispatchEffect::new("run-1", "observation-1", "text_entry")
                )
                .is_err(),
            "a lease must not authorize a different action class"
        );
        let lease = registry
            .authorize_dispatch(session, &reference, &live, &effect)
            .expect("authorize dispatch");
        lease.redeem(live.digest(), &effect).expect("redeem once");
    }

    #[test]
    fn no_model_selection_inherits_another_ones_qualification() {
        let registry = registry(AssuranceProfile::Balanced);
        let session = Uuid::new_v4();
        let (_, reference) = qualified(&registry, session);
        let mut sibling = request(CapabilitySource::Measured, ComputerUseTier::SemanticAct);
        sibling.selection_key = "xai/grok-4-fast".into();
        sibling.route.wire_model = "grok-4-fast".into();
        let live = registry.assess(&sibling).expect("assess");
        assert_eq!(
            registry.validate(session, &reference, CapabilityBoundary::Dispatch, &live),
            Err(CapabilityDenied)
        );
    }
}
