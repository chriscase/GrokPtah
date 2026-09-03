//! Gates 2, 3, and 4 on the one canonical store.
//!
//! Gate 2 seals capabilities and mints one-use effect leases. Gate 3 turns a
//! lease into exactly one physical-send permit and settles it. Gate 4 records
//! both in the typed audit log.
//!
//! The ordering in [`HostAuthority::begin_send`] is the load-bearing part of
//! the design and is spelled out at that function.

use crate::audit::AuditEvent;
use crate::digest::{ContentDigest, RequestIdentity};
use crate::error::AuthorityError;
use crate::ids::*;
use crate::receipt::*;
use crate::state::*;
use crate::store::*;

/// Attempt lifecycle as persisted.
pub(crate) const STATE_PREPARING: &str = "preparing";
pub(crate) const STATE_SENDING: &str = "sending";
pub(crate) const STATE_SETTLED: &str = "settled";
pub(crate) const STATE_FAILED: &str = "failed";
pub(crate) const STATE_UNCERTAIN: &str = "uncertain";
pub(crate) const STATE_DISCARDED: &str = "discarded";

impl HostAuthority {
    /// Replay terminal attempt records from the verified audit WAL into the
    /// state snapshot.
    ///
    /// Settlement deliberately appends the audit outcome before updating the
    /// snapshot: failure before the append leaves `sending`, while failure of
    /// the later snapshot write leaves a durable WAL record that this method
    /// applies on the next open. This makes either cut replay-safe without
    /// pretending two filesystem files can be atomically renamed together.
    pub(crate) fn replay_attempt_settlements(&self) -> Result<(), AuthorityError> {
        let _lifecycle = self.lock_attempt_lifecycle()?;
        self.replay_attempt_settlements_locked()
    }

    /// Replay while the caller holds `attempt_lifecycle`.
    fn replay_attempt_settlements_locked(&self) -> Result<(), AuthorityError> {
        let records = {
            let log = self
                .audit
                .lock()
                .map_err(|_| AuthorityError::Durability("audit log lock poisoned".into()))?;
            // A damaged log remains inspectable by the operator but is never
            // used as a replay source and remains unappendable. Open must not
            // turn evidence damage into silent state mutation.
            if !log.verify_chain()? {
                return Ok(());
            }
            log.records()?
        };

        let mut outcomes = std::collections::BTreeMap::<String, (String, String)>::new();
        let mut reconciliation_truths = std::collections::BTreeMap::<String, String>::new();
        for record in records {
            match record.event {
                AuditEvent::SendOutcome {
                    attempt,
                    outcome,
                    detail,
                } => {
                    let state = match outcome.as_str() {
                        "settled" => STATE_SETTLED,
                        "failed" => STATE_FAILED,
                        "uncertain" => STATE_UNCERTAIN,
                        _ => {
                            return Err(AuthorityError::CorruptState(format!(
                                "unknown audited attempt outcome {outcome}"
                            )));
                        }
                    };
                    if let Some((_, existing_detail)) = outcomes.get(&attempt)
                        && existing_detail.starts_with("reconciled")
                    {
                        continue;
                    }
                    outcomes.insert(attempt, (state.to_string(), detail));
                }
                AuditEvent::AttemptReconciled { attempt, truth } => {
                    let state = match truth.as_str() {
                        "took_effect" => STATE_SETTLED,
                        "no_effect" => STATE_FAILED,
                        "discarded" => STATE_DISCARDED,
                        _ => {
                            return Err(AuthorityError::CorruptState(format!(
                                "unknown audited reconciliation truth {truth}"
                            )));
                        }
                    };
                    if let Some(existing) = reconciliation_truths.get(&attempt) {
                        if existing != &truth {
                            return Err(AuthorityError::CorruptState(
                                "conflicting reconciliation audit records for one attempt".into(),
                            ));
                        }
                    } else {
                        reconciliation_truths.insert(attempt.clone(), truth.clone());
                    }
                    outcomes.insert(attempt, (state.to_string(), "reconciled".into()));
                }
                _ => {}
            }
        }

        if outcomes.is_empty() {
            return Ok(());
        }

        // Avoid rewriting an already-converged snapshot. Besides reducing
        // churn, this lets crash recovery use replay as its first step: a
        // genuine crash with no audited outcome must still be able to append
        // its uncertainty even when the later snapshot write is the failing
        // cut under test.
        let needs_replay = self.read(|state| {
            let mut by_handle = std::collections::BTreeMap::<String, String>::new();
            for key in state.attempts.keys() {
                let attempt: AttemptId = decode_id(key, "attempt")?;
                if by_handle
                    .insert(attempt.public_handle(), key.clone())
                    .is_some()
                {
                    return Err(AuthorityError::CorruptState(
                        "attempt public-handle collision".into(),
                    ));
                }
            }
            for (handle, (outcome, detail)) in &outcomes {
                let key = by_handle.get(handle).ok_or_else(|| {
                    AuthorityError::CorruptState(format!(
                        "audit outcome references unknown attempt {handle}"
                    ))
                })?;
                let attempt = state.attempts.get(key).ok_or_else(|| {
                    AuthorityError::CorruptState("attempt index changed during replay".into())
                })?;
                if attempt.state != *outcome || attempt.settlement.as_deref() != Some(detail) {
                    return Ok(true);
                }
            }
            Ok(false)
        })?;
        if !needs_replay {
            return Ok(());
        }

        self.with_state(|state| {
            let mut by_handle = std::collections::BTreeMap::<String, String>::new();
            for key in state.attempts.keys() {
                let attempt: AttemptId = decode_id(key, "attempt")?;
                if by_handle
                    .insert(attempt.public_handle(), key.clone())
                    .is_some()
                {
                    return Err(AuthorityError::CorruptState(
                        "attempt public-handle collision".into(),
                    ));
                }
            }
            for (handle, (outcome, detail)) in outcomes {
                let key = by_handle.get(&handle).ok_or_else(|| {
                    AuthorityError::CorruptState(format!(
                        "audit outcome references unknown attempt {handle}"
                    ))
                })?;
                let attempt = state.attempts.get_mut(key).ok_or_else(|| {
                    AuthorityError::CorruptState("attempt index changed during replay".into())
                })?;
                attempt.state = outcome;
                attempt.settlement = Some(detail);
            }
            Ok(())
        })
    }

    // ────────────────── Gate 2: sealed capabilities and leases ──────────────────

    /// Seal a capability for a host-issued resource.
    ///
    /// The scope is fixed here and cannot be widened later: [`SealedCapability`]
    /// exposes no setter and cannot be constructed outside this crate. The
    /// resource must be one the host issued, so a caller cannot seal a
    /// capability over a resource it merely named.
    pub fn seal_capability(
        &self,
        auth: &AuthContext,
        resource: ResourceIncarnation,
        actor: ActorClass,
        effect: EffectClass,
        ttl_ms: u64,
    ) -> Result<SealedCapability, AuthorityError> {
        if ttl_ms == 0 {
            return Err(AuthorityError::Invalid("capability ttl"));
        }
        let now = unix_time_millis();
        let expires_at_ms = now
            .checked_add(ttl_ms)
            .ok_or(AuthorityError::Invalid("capability ttl"))?;

        let (capability, principal_handle) = self.with_state(|state| {
            require_current_state(state, auth)?;
            let record = state
                .resources
                .get(&resource.to_hex())
                .ok_or_else(deny_resource_access)?
                .clone();
            let binding = binding_from_resource(state, auth, &record)?;
            let id = CapabilityId::mint();
            state.capabilities.insert(
                id.to_hex(),
                StoredCapability {
                    capability_id: id.to_hex(),
                    principal: binding.principal.to_hex(),
                    credential_incarnation: binding.incarnation.to_hex(),
                    auth_generation: binding.auth_generation.raw(),
                    capability_generation: binding.capability_generation.raw(),
                    policy_revision: binding.policy_revision.raw(),
                    session: binding.session.to_hex(),
                    workspace: binding.workspace.to_hex(),
                    resource: binding.resource.to_hex(),
                    control_epoch: binding.control_epoch.raw(),
                    actor: actor.as_str().to_string(),
                    effect: effect.as_str().to_string(),
                    expires_at_ms,
                    consumed: false,
                },
            );
            Ok((
                SealedCapability {
                    id,
                    binding,
                    actor,
                    effect,
                    expires_at_ms,
                },
                auth.principal.public_handle(),
            ))
        })?;

        self.append_audit(
            capability.binding.control_epoch.raw(),
            AuditEvent::CapabilitySealed {
                capability: capability.id.public_handle(),
                principal: principal_handle,
                actor: actor.as_str().to_string(),
                effect: effect.as_str().to_string(),
            },
        )?;
        Ok(capability)
    }

    /// Mint a one-use effect lease for exactly one action.
    ///
    /// Binds the action digest together with the observation revision and
    /// digest it was planned against. If the surface moves before the lease is
    /// spent, [`Self::begin_send`] rejects it with
    /// [`AuthorityError::StaleObservation`].
    pub fn mint_lease(
        &self,
        auth: &AuthContext,
        capability: &SealedCapability,
        action_digest: ContentDigest,
        ttl_ms: u64,
    ) -> Result<EffectLease, AuthorityError> {
        if ttl_ms == 0 {
            return Err(AuthorityError::Invalid("lease ttl"));
        }
        let now = unix_time_millis();
        let expires_at_ms = now
            .checked_add(ttl_ms)
            .ok_or(AuthorityError::Invalid("lease ttl"))?;

        let lease = self.with_state(|state| {
            require_current_state(state, auth)?;
            let stored = state
                .capabilities
                .get(&capability.id.to_hex())
                .ok_or(AuthorityError::StaleCapability)?
                .clone();
            if stored.consumed {
                return Err(AuthorityError::AlreadyConsumed);
            }
            if stored.expires_at_ms <= now {
                return Err(AuthorityError::Expired);
            }
            if stored.capability_generation != state.capability_generation {
                return Err(AuthorityError::StaleCapability);
            }
            if stored.policy_revision != state.policy_revision {
                return Err(AuthorityError::StalePolicy);
            }
            if stored.control_epoch != state.control_epoch {
                return Err(AuthorityError::StaleControlEpoch);
            }
            // The presented capability must match the durable one in full.
            if stored.principal != capability.binding.principal.to_hex()
                || stored.credential_incarnation != capability.binding.incarnation.to_hex()
                || stored.policy_revision != capability.binding.policy_revision.raw()
                || stored.session != capability.binding.session.to_hex()
                || stored.workspace != capability.binding.workspace.to_hex()
                || stored.resource != capability.binding.resource.to_hex()
                || stored.effect != capability.effect.as_str()
                || stored.actor != capability.actor.as_str()
            {
                return Err(AuthorityError::ResourceOwnershipMismatch);
            }
            // The principal presenting it must be the one that holds it.
            if stored.principal != auth.principal.to_hex()
                || stored.credential_incarnation != auth.incarnation.to_hex()
            {
                return Err(AuthorityError::ResourceOwnershipMismatch);
            }
            let resource = state
                .resources
                .get(&stored.resource)
                .ok_or_else(deny_resource_access)?
                .clone();

            // A lease is a narrower grant than its capability, never a longer
            // one: outliving the capability would let a spent or expired grant
            // keep authorising work.
            let expires_at_ms = expires_at_ms.min(stored.expires_at_ms);
            let id = EffectLeaseId::mint();
            state.leases.insert(
                id.to_hex(),
                StoredLease {
                    lease_id: id.to_hex(),
                    capability_id: stored.capability_id.clone(),
                    principal: stored.principal.clone(),
                    credential_incarnation: stored.credential_incarnation.clone(),
                    auth_generation: stored.auth_generation,
                    capability_generation: stored.capability_generation,
                    policy_revision: stored.policy_revision,
                    session: stored.session.clone(),
                    workspace: stored.workspace.clone(),
                    resource: stored.resource.clone(),
                    control_epoch: stored.control_epoch,
                    observation_revision: resource.observation_revision,
                    observation_digest: resource.observation_digest.clone(),
                    action_digest: action_digest.to_hex(),
                    actor: stored.actor.clone(),
                    effect: stored.effect.clone(),
                    expires_at_ms,
                    consumed: false,
                },
            );
            Ok(EffectLease {
                id,
                capability: capability.id,
                binding: capability.binding,
                observation_revision: ObservationRevision::from_raw(resource.observation_revision),
                observation_digest: decode_digest(&resource.observation_digest, "observation")?,
                action_digest,
                actor: capability.actor,
                effect: capability.effect,
                expires_at_ms,
            })
        })?;

        self.append_audit(
            lease.binding.control_epoch.raw(),
            AuditEvent::LeaseMinted {
                lease: lease.id.public_handle(),
                capability: capability.id.public_handle(),
                action_digest: action_digest.public_handle(),
                observation_revision: lease.observation_revision.raw(),
            },
        )?;
        Ok(lease)
    }

    // ─────────────── Gate 3: the physical-send attempt lattice ───────────────

    /// Turn a one-use lease into the single permit that authorises one
    /// physical provider send.
    ///
    /// This is the only producer of [`PhysicalSendPermit`], and the send path
    /// requires one, so there is no ordinary send that bypasses the lattice.
    ///
    /// Ordering, in this exact sequence:
    ///
    /// 1. Validate the lease against durable state and **consume it**, and
    ///    record the attempt as `preparing`, in one atomic, fsynced transaction.
    /// 2. Append the `SendIntent` audit record and fsync it.
    /// 3. Only then construct the permit.
    ///
    /// Wire admission — transitioning to `sending` immediately before bytes
    /// may move — is [`Self::admit_sending`], which is the only path to
    /// dispatch. Any failure in steps 1 or 2 returns
    /// [`AuthorityError::Durability`] and no permit, so a pre-effect
    /// persistence failure prevents dispatch. A crash after step 1 leaves the
    /// attempt `preparing`, which [`Self::recover_incomplete`] settles as
    /// [`FailedReason::AbandonedBeforeWireAdmission`] rather than silently
    /// retrying. A crash after wire admission leaves the attempt `sending`,
    /// which recovery settles as
    /// [`UncertainReason::CrashBetweenDispatchAndSettlement`].
    ///
    /// The permit is bound to `request`'s full identity — URL, method,
    /// dialect, credential, model, provider request ID, and body — so it
    /// cannot be carried to a different endpoint, credential, model, physical
    /// attempt, or body.
    pub fn begin_send(
        &self,
        auth: &AuthContext,
        lease: EffectLease,
        request: &RequestIdentity,
        route: &str,
    ) -> Result<PhysicalSendPermit, AuthorityError> {
        let _lifecycle = self.lock_attempt_lifecycle()?;
        if lease.effect != EffectClass::ProviderSend {
            return Err(AuthorityError::NotPermitted);
        }
        let now = unix_time_millis();
        let request_digest = request.digest();
        let body_digest = request.body_digest();
        let dialect = request.dialect().to_string();
        let route_digest = route_digest_for(route);
        let request_digest_hex = request_digest.to_hex();

        // Step 1: consume the lease and record the attempt, atomically.
        let admitted = self.with_state(|state| {
            require_current_state(state, auth)?;
            for record in state.attempts.values() {
                if record.principal != auth.principal.to_hex()
                    || record.request_digest != request_digest_hex
                {
                    continue;
                }
                if matches!(
                    record.state.as_str(),
                    STATE_PREPARING | STATE_SENDING | STATE_UNCERTAIN
                ) {
                    return Err(AuthorityError::AmbiguousPriorAttempt);
                }
            }
            let capability_generation = state.capability_generation;
            let control_epoch = state.control_epoch;
            let stored = state
                .leases
                .get(&lease.id.to_hex())
                .ok_or(AuthorityError::AlreadyConsumed)?
                .clone();
            if stored.consumed {
                return Err(AuthorityError::AlreadyConsumed);
            }
            if stored.expires_at_ms <= now {
                return Err(AuthorityError::Expired);
            }
            if stored.control_epoch != state.control_epoch {
                return Err(AuthorityError::StaleControlEpoch);
            }
            if stored.capability_generation != state.capability_generation {
                return Err(AuthorityError::StaleCapability);
            }
            if stored.policy_revision != state.policy_revision {
                return Err(AuthorityError::StalePolicy);
            }
            if stored.principal != auth.principal.to_hex()
                || stored.credential_incarnation != auth.incarnation.to_hex()
                || stored.auth_generation != auth.auth_generation.raw()
            {
                return Err(AuthorityError::ResourceOwnershipMismatch);
            }
            if stored.session != lease.binding.session.to_hex() {
                return Err(AuthorityError::SessionMismatch);
            }
            if stored.workspace != lease.binding.workspace.to_hex() {
                return Err(AuthorityError::WorkspaceMismatch);
            }
            if stored.resource != lease.binding.resource.to_hex() {
                return Err(AuthorityError::ResourceOwnershipMismatch);
            }
            // The action the lease authorised must be the request being sent.
            if stored.action_digest != request_digest.to_hex() {
                return Err(AuthorityError::DigestMismatch);
            }
            // The durable effect class must still parse and still be the one
            // the presented lease claims; a record that no longer parses is
            // corrupt state, not an implicit grant.
            match EffectClass::parse(&stored.effect) {
                Some(EffectClass::ProviderSend) => {}
                Some(_) => return Err(AuthorityError::NotPermitted),
                None => {
                    return Err(AuthorityError::CorruptState(
                        "durable lease effect class is unknown".into(),
                    ));
                }
            }
            // The actor must still parse and must be the one the lease claims.
            // An unrecognised actor is corrupt state, never an implicit
            // operator.
            let Some(stored_actor) = ActorClass::parse(&stored.actor) else {
                return Err(AuthorityError::CorruptState(
                    "durable lease actor class is unknown".into(),
                ));
            };
            if stored_actor != lease.actor {
                return Err(AuthorityError::ResourceOwnershipMismatch);
            }
            // The surface must not have moved since the lease was minted.
            let resource_key = stored.resource.clone();
            let resource = state
                .resources
                .get_mut(&resource_key)
                .ok_or_else(deny_resource_access)?;
            if resource.observation_revision != stored.observation_revision
                || resource.observation_digest != stored.observation_digest
            {
                return Err(AuthorityError::StaleObservation);
            }

            // The parent capability is revalidated and consumed here, in the
            // same transaction that spends the lease. Minting several leases
            // from one capability before spending any would otherwise let that
            // single grant authorise several sends.
            let capability = state
                .capabilities
                .get_mut(&stored.capability_id)
                .ok_or(AuthorityError::StaleCapability)?;
            if capability.consumed {
                return Err(AuthorityError::AlreadyConsumed);
            }
            if capability.expires_at_ms <= now {
                return Err(AuthorityError::Expired);
            }
            if capability.capability_generation != capability_generation
                || capability.policy_revision != state.policy_revision
                || capability.control_epoch != control_epoch
            {
                return Err(AuthorityError::StaleCapability);
            }
            capability.consumed = true;

            // One-use: the lease is spent whether or not the send succeeds.
            state.leases.remove(&lease.id.to_hex());

            let attempt = AttemptId::mint();
            let ordinal = resource.next_attempt_ordinal;
            resource.next_attempt_ordinal = ordinal
                .checked_add(1)
                .ok_or_else(|| AuthorityError::Durability("attempt ordinal exhausted".into()))?;
            let idempotency_key = idempotency_key_for(attempt);
            let wire_idempotency_digest = wire_idempotency_digest_for(&idempotency_key);
            state.attempts.insert(
                attempt.to_hex(),
                StoredAttempt {
                    attempt_id: attempt.to_hex(),
                    lease_id: stored.lease_id.clone(),
                    principal: stored.principal.clone(),
                    credential_incarnation: stored.credential_incarnation.clone(),
                    auth_generation: stored.auth_generation,
                    capability_generation: stored.capability_generation,
                    policy_revision: stored.policy_revision,
                    session: stored.session.clone(),
                    workspace: stored.workspace.clone(),
                    resource: stored.resource.clone(),
                    control_epoch: stored.control_epoch,
                    actor: stored.actor.clone(),
                    ordinal,
                    dialect: dialect.clone(),
                    route_digest: route_digest.to_hex(),
                    request_digest: request_digest.to_hex(),
                    body_digest: body_digest.to_hex(),
                    idempotency_key: idempotency_key.clone(),
                    wire_idempotency_digest: wire_idempotency_digest.to_hex(),
                    state: STATE_PREPARING.to_string(),
                    settlement: None,
                },
            );
            Ok((attempt, lease.binding, idempotency_key, dialect, ordinal))
        });

        // A refusal is recorded against the authenticated principal that asked.
        // Failing to record a *denial* cannot change the denial: no effect
        // occurred and none is being permitted, so the refusal still stands.
        let (attempt, binding, idempotency_key, dialect, ordinal) = match admitted {
            Ok(admitted) => admitted,
            Err(error) => {
                let _ = self.append_audit(
                    auth.control_epoch.raw(),
                    AuditEvent::Denied {
                        principal: auth.principal.public_handle(),
                        reason: format!("{error:?}"),
                    },
                );
                return Err(error);
            }
        };

        // Step 2: the intent must be durable before a permit can exist.
        self.append_audit(
            binding.control_epoch.raw(),
            AuditEvent::SendIntent {
                attempt: attempt.public_handle(),
                lease: lease.id.public_handle(),
                principal: binding.principal.public_handle(),
                auth_generation: binding.auth_generation.raw(),
                capability_generation: binding.capability_generation.raw(),
                policy_revision: binding.policy_revision.raw(),
                session: binding.session.public_handle(),
                workspace: binding.workspace.public_handle(),
                resource: binding.resource.public_handle(),
                actor: lease.actor.as_str().to_string(),
                ordinal,
                dialect: dialect.clone(),
                route_digest: route_digest.public_handle(),
                request_digest: request_digest.public_handle(),
                body_digest: body_digest.public_handle(),
                wire_idempotency_digest: wire_idempotency_digest_for(&idempotency_key)
                    .public_handle(),
            },
        )?;

        // Step 3: only now does the authorisation to send physically exist.
        Ok(PhysicalSendPermit {
            attempt,
            lease: lease.id,
            binding,
            request_digest,
            body_digest,
            idempotency_key,
            dialect,
            wire_admitted: false,
        })
    }

    /// Durably admit one prepared attempt to the wire immediately before bytes
    /// may move. This is the only path from `preparing` to `sending`.
    pub fn admit_sending(
        &self,
        auth: &AuthContext,
        permit: PhysicalSendPermit,
    ) -> Result<PhysicalSendPermit, AuthorityError> {
        if permit.wire_admitted {
            return Err(AuthorityError::Invalid("attempt already wire-admitted"));
        }
        let _lifecycle = self.lock_attempt_lifecycle()?;
        let attempt = permit.attempt;
        let epoch = permit.binding.control_epoch.raw();
        self.with_state(|state| {
            require_current_state(state, auth)?;
            let record = state
                .attempts
                .get_mut(&attempt.to_hex())
                .ok_or_else(|| AuthorityError::CorruptState("attempt record vanished".into()))?;
            if record.principal != auth.principal.to_hex()
                || record.credential_incarnation != auth.incarnation.to_hex()
                || record.auth_generation != auth.auth_generation.raw()
            {
                return Err(AuthorityError::ResourceOwnershipMismatch);
            }
            if record.capability_generation != state.capability_generation
                || record.policy_revision != state.policy_revision
                || record.control_epoch != state.control_epoch
            {
                return Err(AuthorityError::StaleCapability);
            }
            if record.state != STATE_PREPARING {
                return Err(AuthorityError::Invalid("attempt is not preparing"));
            }
            record.state = STATE_SENDING.to_string();
            Ok(())
        })?;
        self.append_audit(
            epoch,
            AuditEvent::SendWireAdmission {
                attempt: attempt.public_handle(),
            },
        )?;
        Ok(PhysicalSendPermit {
            wire_admitted: true,
            ..permit
        })
    }

    /// Settle an attempt whose response was observed.
    ///
    /// The permit is taken by value, so it cannot be presented again.
    pub fn settle_settled(&self, permit: PhysicalSendPermit) -> SendOutcome {
        self.settle(permit, STATE_SETTLED, "settled", None)
    }

    /// Settle an attempt that provably never reached the provider.
    ///
    /// Use only when nothing was written — a refused connection, or a denial
    /// raised before the request was handed to the transport. Anything that
    /// might have been written must use [`Self::settle_uncertain`].
    pub fn settle_failed_before_write(
        &self,
        permit: PhysicalSendPermit,
        reason: FailedReason,
    ) -> SendOutcome {
        self.settle(permit, STATE_FAILED, "failed", Some(Err(reason)))
    }

    /// Settle an attempt that may or may not have taken effect.
    ///
    /// There is no retry here and no path back to a fresh permit: an
    /// [`SendOutcome::Uncertain`] attempt is resolved by
    /// [`Self::mint_reconciliation_grant`] and [`Self::apply_reconciliation`]
    /// after the host establishes provider truth.
    pub fn settle_uncertain(
        &self,
        permit: PhysicalSendPermit,
        reason: UncertainReason,
    ) -> SendOutcome {
        self.settle(permit, STATE_UNCERTAIN, "uncertain", Some(Ok(reason)))
    }

    /// Common settlement path.
    ///
    /// Persistence trouble here happens *after* dispatch was already possible,
    /// so it can never downgrade to an ordinary failure: it settles
    /// [`UncertainReason::AuditNotDurableAfterDispatch`] when the WAL append
    /// fails, or [`UncertainReason::StateNotDurableAfterDispatch`] when the
    /// WAL is durable but its derived snapshot cannot be updated. The latter
    /// converges from the WAL on the next open.
    fn settle(
        &self,
        permit: PhysicalSendPermit,
        durable_state: &str,
        outcome_label: &str,
        detail: Option<Result<UncertainReason, FailedReason>>,
    ) -> SendOutcome {
        let attempt = permit.attempt;
        let _lifecycle = match self.lock_attempt_lifecycle() {
            Ok(guard) => guard,
            Err(_) => {
                return SendOutcome::Uncertain {
                    attempt,
                    reason: UncertainReason::LifecycleUnavailableAfterDispatch,
                };
            }
        };
        let current_state = match self.read(|state| {
            state
                .attempts
                .get(&attempt.to_hex())
                .cloned()
                .ok_or_else(|| AuthorityError::CorruptState("attempt record vanished".into()))
        }) {
            Ok(state) => state,
            Err(_) => {
                return SendOutcome::Uncertain {
                    attempt,
                    reason: UncertainReason::StateNotDurableAfterDispatch,
                };
            }
        };
        if let Some(outcome) =
            crate::reconciliation::absorb_settlement_for_reconciled_attempt(attempt, &current_state)
        {
            return outcome;
        }
        if durable_state == STATE_SETTLED && !permit.wire_admitted {
            return SendOutcome::Uncertain {
                attempt,
                reason: UncertainReason::ProtocolAfterPossibleEffect,
            };
        }
        if durable_state == STATE_UNCERTAIN
            && !permit.wire_admitted
            && current_state.state != STATE_SENDING
        {
            return SendOutcome::Failed {
                attempt,
                reason: FailedReason::AbandonedBeforeWireAdmission,
            };
        }
        let epoch = permit.binding.control_epoch.raw();
        let detail_text = match detail {
            None => "response observed".to_string(),
            Some(Ok(reason)) => format!("{reason:?}"),
            Some(Err(reason)) => format!("{reason:?}"),
        };

        // Audit before state, deliberately. Failure before the WAL append
        // leaves the attempt sending. Failure after the WAL append leaves a
        // replayable terminal record that the next open applies before crash
        // recovery can classify the attempt. State-before-audit cannot offer
        // that guarantee: it can publish a terminal state with no evidence.
        let audited = self.append_audit(
            epoch,
            AuditEvent::SendOutcome {
                attempt: attempt.public_handle(),
                outcome: outcome_label.to_string(),
                detail: detail_text.clone(),
            },
        );
        if audited.is_err() {
            return SendOutcome::Uncertain {
                attempt,
                reason: UncertainReason::AuditNotDurableAfterDispatch,
            };
        }

        let persisted =
            self.with_state(|state| {
                let record = state.attempts.get_mut(&attempt.to_hex()).ok_or(
                    AuthorityError::CorruptState("attempt record vanished".into()),
                )?;
                record.state = durable_state.to_string();
                record.settlement = Some(detail_text.clone());
                Ok(())
            });
        if persisted.is_err() {
            return SendOutcome::Uncertain {
                attempt,
                reason: UncertainReason::StateNotDurableAfterDispatch,
            };
        }

        match detail {
            None => SendOutcome::Settled { attempt },
            Some(Ok(reason)) => SendOutcome::Uncertain { attempt, reason },
            Some(Err(reason)) => SendOutcome::Failed { attempt, reason },
        }
    }

    /// Settle every attempt left in flight by a previous process as
    /// [`SendOutcome::Uncertain`].
    ///
    /// Called at open time after a crash. It never re-sends: an attempt that
    /// was `sending` when the process stopped may have reached the provider,
    /// so it becomes ambiguous and waits for reconciliation. An attempt left
    /// in `preparing` never reached the wire and is settled as
    /// [`FailedReason::AbandonedBeforeWireAdmission`].
    ///
    /// Requires admin authority. Forcing every in-flight attempt into the
    /// ambiguous state is a host decision, not something a component serving
    /// a request may do to work already in flight.
    pub fn recover_incomplete(
        &self,
        admin: &HostAdminAuthority,
    ) -> Result<Vec<AttemptId>, AuthorityError> {
        self.require_admin(admin)?;
        let _lifecycle = self.lock_attempt_lifecycle()?;
        self.replay_attempt_settlements_locked()?;
        let (preparing, sending) = self.read(|state| {
            let mut preparing = Vec::new();
            let mut sending = Vec::new();
            for (key, record) in &state.attempts {
                match record.state.as_str() {
                    STATE_PREPARING => preparing.push(key.clone()),
                    STATE_SENDING => sending.push(key.clone()),
                    _ => {}
                }
            }
            Ok((preparing, sending))
        })?;
        let epoch = self.read(|state| Ok(state.control_epoch))?;
        for key in &preparing {
            let id: AttemptId = decode_id(key, "attempt")?;
            self.append_audit(
                epoch,
                AuditEvent::SendOutcome {
                    attempt: id.public_handle(),
                    outcome: "failed".to_string(),
                    detail: format!("{:?}", FailedReason::AbandonedBeforeWireAdmission),
                },
            )?;
        }
        for key in &sending {
            let id: AttemptId = decode_id(key, "attempt")?;
            self.append_audit(
                epoch,
                AuditEvent::SendOutcome {
                    attempt: id.public_handle(),
                    outcome: "uncertain".to_string(),
                    detail: format!("{:?}", UncertainReason::CrashBetweenDispatchAndSettlement),
                },
            )?;
        }
        if preparing.is_empty() && sending.is_empty() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::with_capacity(preparing.len() + sending.len());
        self.with_state(|state| {
            for key in &preparing {
                let id: AttemptId = decode_id(key, "attempt")?;
                let record = state.attempts.get_mut(key).ok_or_else(|| {
                    AuthorityError::CorruptState("attempt record vanished during recovery".into())
                })?;
                if record.state == STATE_PREPARING {
                    record.state = STATE_FAILED.to_string();
                    record.settlement =
                        Some(format!("{:?}", FailedReason::AbandonedBeforeWireAdmission));
                    ids.push(id);
                }
            }
            for key in &sending {
                let id: AttemptId = decode_id(key, "attempt")?;
                let record = state.attempts.get_mut(key).ok_or_else(|| {
                    AuthorityError::CorruptState("attempt record vanished during recovery".into())
                })?;
                if record.state == STATE_SENDING {
                    record.state = STATE_UNCERTAIN.to_string();
                    record.settlement = Some(format!(
                        "{:?}",
                        UncertainReason::CrashBetweenDispatchAndSettlement
                    ));
                    ids.push(id);
                }
            }
            Ok(())
        })?;
        Ok(ids)
    }

    /// Return the opaque identities of attempts that require operator/provider
    /// truth before any replay can be considered. The IDs contain no request,
    /// credential, model, URL, content, or path material.
    pub fn ambiguous_attempts(
        &self,
        admin: &HostAdminAuthority,
    ) -> Result<Vec<AttemptId>, AuthorityError> {
        self.require_admin(admin)?;
        self.read(|state| {
            state
                .attempts
                .iter()
                .filter(|(_, record)| {
                    record.state == STATE_SENDING || record.state == STATE_UNCERTAIN
                })
                .map(|(key, _)| decode_id(key, "attempt"))
                .collect()
        })
    }

    /// Resolve an ambiguous attempt with established provider truth.
    ///
    /// **Migration seam:** this legacy admin-only path is retired. Callers must
    /// use [`Self::mint_reconciliation_grant`] and [`Self::apply_reconciliation`]
    /// with the current evidence/grant contract instead.
    pub fn reconcile_attempt(
        &self,
        admin: &HostAdminAuthority,
        attempt: AttemptId,
        took_effect: bool,
    ) -> Result<(), AuthorityError> {
        let _ = (admin, attempt, took_effect);
        Err(AuthorityError::Invalid(
            crate::reconciliation::LEGACY_RECONCILE_ATTEMPT_RETIRED,
        ))
    }

    /// A secret-, content-, and path-free view of an attempt.
    ///
    /// Scoped: a principal sees only its own attempts. An attempt belonging to
    /// another principal is reported exactly as a missing one, so this is not
    /// an oracle for which attempt identifiers exist.
    pub fn attempt_projection(
        &self,
        auth: &AuthContext,
        attempt: AttemptId,
    ) -> Result<Option<AttemptProjection>, AuthorityError> {
        self.read(|state| {
            require_current_state(state, auth)?;
            let Some(record) = state.attempts.get(&attempt.to_hex()) else {
                return Ok(None);
            };
            if record.principal != auth.principal.to_hex()
                || record.credential_incarnation != auth.incarnation.to_hex()
            {
                return Ok(None);
            }
            Ok(Some(project_stored_attempt(attempt, record)?))
        })
    }

    /// Secret-free list of attempts owned by this principal in this session and
    /// workspace. Foreign work is omitted, never distinguished.
    pub fn list_scoped_attempt_projections(
        &self,
        auth: &AuthContext,
        session: SessionId,
        workspace: WorkspaceId,
    ) -> Result<Vec<AttemptProjection>, AuthorityError> {
        self.read(|state| {
            require_current_state(state, auth)?;
            let mut out = Vec::new();
            for (key, record) in &state.attempts {
                if !attempt_in_scope(record, auth, session, workspace) {
                    continue;
                }
                let attempt: AttemptId = decode_id(key, "attempt")?;
                out.push(project_stored_attempt(attempt, record)?);
            }
            out.sort_by(|left, right| {
                left.ordinal
                    .cmp(&right.ordinal)
                    .then_with(|| left.attempt.cmp(&right.attempt))
            });
            Ok(out)
        })
    }

    /// Secret-free get by public attempt handle, scoped to principal, session,
    /// and workspace. Unknown and foreign handles return `None` identically.
    pub fn scoped_attempt_projection(
        &self,
        auth: &AuthContext,
        session: SessionId,
        workspace: WorkspaceId,
        attempt_handle: &str,
    ) -> Result<Option<AttemptProjection>, AuthorityError> {
        self.read(|state| {
            require_current_state(state, auth)?;
            let Some((attempt, record)) = find_attempt_by_handle(state, attempt_handle)? else {
                return Ok(None);
            };
            if !attempt_in_scope(&record, auth, session, workspace) {
                return Ok(None);
            }
            Ok(Some(project_stored_attempt(attempt, &record)?))
        })
    }

    // ───────────────────────── Gate 4: typed audit ─────────────────────────

    /// Append one audit record and fsync it.
    pub(crate) fn append_audit(
        &self,
        control_epoch: u64,
        event: AuditEvent,
    ) -> Result<(), AuthorityError> {
        let mut log = self
            .audit
            .lock()
            .map_err(|_| AuthorityError::Durability("audit log lock poisoned".into()))?;
        log.append(control_epoch, event)?;
        Ok(())
    }

    pub(crate) fn lock_attempt_lifecycle(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, ()>, AuthorityError> {
        self.attempt_lifecycle
            .lock()
            .map_err(|_| AuthorityError::Durability("attempt lifecycle lock poisoned".into()))
    }

    /// Every audit record, oldest first.
    ///
    /// Exporting the whole log is an operator action, not something a served
    /// request can do: it spans every principal this root has served.
    pub fn audit_records(
        &self,
        admin: &HostAdminAuthority,
    ) -> Result<Vec<crate::audit::AuditRecord>, AuthorityError> {
        self.require_admin(admin)?;
        let log = self
            .audit
            .lock()
            .map_err(|_| AuthorityError::Durability("audit log lock poisoned".into()))?;
        log.records()
    }

    /// Whether the audit hash chain is intact. An operator read.
    pub fn audit_chain_intact(&self, admin: &HostAdminAuthority) -> Result<bool, AuthorityError> {
        self.require_admin(admin)?;
        let log = self
            .audit
            .lock()
            .map_err(|_| AuthorityError::Durability("audit log lock poisoned".into()))?;
        log.verify_chain()
    }

    /// Seal, lease, begin, and durably admit one operator-backed provider send.
    ///
    /// This is the smallest shared caller seam on the existing lattice: it
    /// does not open a root, mint a second ledger, or perform HTTP. The
    /// returned permit has already passed [`Self::admit_sending`]; the
    /// caller must physically send immediately afterwards and consume the
    /// permit by settlement. Actor class is [`ActorClass::VerifiedOperator`].
    pub fn admit_operator_send(
        &self,
        auth: &AuthContext,
        request: &RequestIdentity,
        target_scope: &str,
    ) -> Result<PhysicalSendPermit, AuthorityError> {
        let cwd = std::env::current_dir()
            .map_err(|error| AuthorityError::Durability(error.to_string()))?;
        let workspace = self.issue_workspace(auth, &cwd)?;
        let resource = self.obtain_provider_send_surface(auth, workspace, target_scope)?;
        let capability = self.seal_capability(
            auth,
            resource,
            ActorClass::VerifiedOperator,
            EffectClass::ProviderSend,
            60_000,
        )?;
        let lease = self.mint_lease(auth, &capability, request.digest(), 30_000)?;
        let permit = self.begin_send(auth, lease, request, target_scope)?;
        self.admit_sending(auth, permit)
    }
}

/// A public, secret-free, content-free, path-free view of one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptProjection {
    pub attempt: String,
    pub state: String,
    pub ordinal: u64,
    pub request_digest: String,
    pub body_digest: String,
    pub settled: bool,
    pub ambiguous: bool,
    /// Public session handle. Not a filesystem path.
    pub session: String,
    /// Public workspace handle. Not a filesystem path.
    pub workspace: String,
    /// Wire dialect class (for example `openai-chat`). Never a URL or raw route.
    pub dialect: String,
    /// Public handle of the logical route digest. Never the raw route string.
    pub route: String,
    /// CAS token for this durable attempt record.
    pub revision: String,
    pub auth_generation: u64,
    pub capability_generation: u64,
    pub policy_revision: u64,
}

pub(crate) fn project_stored_attempt(
    attempt: AttemptId,
    record: &StoredAttempt,
) -> Result<AttemptProjection, AuthorityError> {
    let session: SessionId = decode_id(&record.session, "session")?;
    let workspace: WorkspaceId = decode_id(&record.workspace, "workspace")?;
    let route = decode_digest(&record.route_digest, "route")?;
    Ok(AttemptProjection {
        attempt: attempt.public_handle(),
        state: record.state.clone(),
        ordinal: record.ordinal,
        request_digest: decode_digest(&record.request_digest, "request")?.public_handle(),
        body_digest: decode_digest(&record.body_digest, "body")?.public_handle(),
        settled: record.state != STATE_PREPARING && record.state != STATE_SENDING,
        ambiguous: record.state == STATE_UNCERTAIN,
        session: session.public_handle(),
        workspace: workspace.public_handle(),
        dialect: record.dialect.clone(),
        route: route.public_handle(),
        revision: attempt_revision(attempt, record).public_handle(),
        auth_generation: record.auth_generation,
        capability_generation: record.capability_generation,
        policy_revision: record.policy_revision,
    })
}

pub(crate) fn attempt_revision(attempt: AttemptId, record: &StoredAttempt) -> ContentDigest {
    ContentDigest::of_fields(&[
        ("attempt-revision-v1", attempt.to_hex().as_bytes()),
        ("state", record.state.as_bytes()),
        (
            "settlement",
            record.settlement.as_deref().unwrap_or("").as_bytes(),
        ),
        ("ordinal", &record.ordinal.to_le_bytes()),
        ("dialect", record.dialect.as_bytes()),
        ("route", record.route_digest.as_bytes()),
        ("auth_generation", &record.auth_generation.to_le_bytes()),
        (
            "capability_generation",
            &record.capability_generation.to_le_bytes(),
        ),
        ("policy_revision", &record.policy_revision.to_le_bytes()),
    ])
}

pub(crate) fn attempt_in_scope(
    record: &StoredAttempt,
    auth: &AuthContext,
    session: SessionId,
    workspace: WorkspaceId,
) -> bool {
    record.principal == auth.principal.to_hex()
        && record.session == session.to_hex()
        && record.workspace == workspace.to_hex()
}

pub(crate) fn find_attempt_by_handle(
    state: &StoredAuthority,
    attempt_handle: &str,
) -> Result<Option<(AttemptId, StoredAttempt)>, AuthorityError> {
    let mut found = None;
    for (key, record) in &state.attempts {
        let attempt: AttemptId = decode_id(key, "attempt")?;
        if attempt.public_handle() == attempt_handle {
            if found.is_some() {
                return Err(AuthorityError::CorruptState(
                    "attempt public-handle collision".into(),
                ));
            }
            found = Some((attempt, record.clone()));
        }
    }
    Ok(found)
}

/// Deterministic idempotency key for an attempt, so a provider that supports
/// one deduplicates a repeat of the *same* attempt rather than acting twice.
fn idempotency_key_for(attempt: AttemptId) -> String {
    format!("grokptah-{}", attempt.public_handle())
}

fn route_digest_for(route: &str) -> ContentDigest {
    ContentDigest::of_fields(&[("provider-route-v1", route.as_bytes())])
}

fn wire_idempotency_digest_for(idempotency_key: &str) -> ContentDigest {
    ContentDigest::of_fields(&[("wire-idempotency-v1", idempotency_key.as_bytes())])
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    const ADMIN_SECRET: &str = "host-admin-custody-secret-32-bytes-minimum-v1";
    const BEARER: &str = "provider-bearer-for-lifecycle-lock-test";

    fn permit_for(
        authority: &HostAuthority,
        auth: &AuthContext,
        resource: ResourceIncarnation,
        body: &[u8],
    ) -> PhysicalSendPermit {
        let request = RequestIdentity::new(
            "https://api.example.invalid/v1/chat",
            "POST",
            "openai-chat",
            b"provider-key",
            "grok-4",
            body,
        );
        let capability = authority
            .seal_capability(
                auth,
                resource,
                ActorClass::VerifiedOperator,
                EffectClass::ProviderSend,
                60_000,
            )
            .unwrap();
        let lease = authority
            .mint_lease(auth, &capability, request.digest(), 60_000)
            .unwrap();
        authority
            .begin_send(auth, lease, &request, "test-route")
            .unwrap()
    }

    fn admit(
        auth: &AuthContext,
        permit: PhysicalSendPermit,
        authority: &HostAuthority,
    ) -> PhysicalSendPermit {
        authority.admit_sending(auth, permit).unwrap()
    }

    #[test]
    fn attempt_lifecycle_transactions_share_one_in_process_lock() {
        let dir = tempfile::tempdir().unwrap();
        let admin_credential = HostAdminCredential::new(ADMIN_SECRET).unwrap();
        let (authority, admin) = HostAuthority::open(dir.path(), &admin_credential).unwrap();
        authority
            .set_credentials(&admin, &[HostCredential::new("primary", BEARER).unwrap()])
            .unwrap();
        let auth = authority.authenticate(BEARER).unwrap();
        let session = authority.issue_session(&auth).unwrap();
        let workspace = authority
            .issue_workspace(&auth, &dir.path().join("workspace"))
            .unwrap();
        let resource = authority
            .issue_resource(&auth, session, workspace, ContentDigest::of_bytes(b"frame"))
            .unwrap();

        // Settlement cannot interleave its WAL append and snapshot update with
        // replay or recovery on another thread.
        let permit = admit(
            &auth,
            permit_for(&authority, &auth, resource, b"settle"),
            &authority,
        );
        let lifecycle = authority.attempt_lifecycle.lock().unwrap();
        std::thread::scope(|scope| {
            let (started_tx, started_rx) = mpsc::channel();
            let (done_tx, done_rx) = mpsc::channel();
            let authority = &authority;
            scope.spawn(move || {
                started_tx.send(()).unwrap();
                done_tx.send(authority.settle_settled(permit)).unwrap();
            });
            started_rx.recv().unwrap();
            assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());
            drop(lifecycle);
            assert!(matches!(
                done_rx.recv().unwrap(),
                SendOutcome::Settled { .. }
            ));
        });

        // Recovery uses the same lock, including its replay, scan, WAL, and
        // snapshot phases. A preparing attempt is settled NotSent.
        let permit = permit_for(&authority, &auth, resource, b"recover-preparing");
        let preparing = permit.attempt();
        std::mem::forget(permit);
        let lifecycle = authority.attempt_lifecycle.lock().unwrap();
        std::thread::scope(|scope| {
            let (started_tx, started_rx) = mpsc::channel();
            let (done_tx, done_rx) = mpsc::channel();
            let authority = &authority;
            let admin = &admin;
            scope.spawn(move || {
                started_tx.send(()).unwrap();
                done_tx.send(authority.recover_incomplete(admin)).unwrap();
            });
            started_rx.recv().unwrap();
            assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());
            drop(lifecycle);
            assert_eq!(done_rx.recv().unwrap().unwrap(), vec![preparing]);
        });
        let projection = authority
            .attempt_projection(&auth, preparing)
            .unwrap()
            .unwrap();
        assert_eq!(projection.state, STATE_FAILED);
        assert!(!projection.ambiguous);

        // A wire-admitted attempt left in flight becomes uncertain and may be
        // reconciled.
        let permit = admit(
            &auth,
            permit_for(&authority, &auth, resource, b"recover-sending"),
            &authority,
        );
        let recovering = permit.attempt();
        std::mem::forget(permit);
        let lifecycle = authority.attempt_lifecycle.lock().unwrap();
        std::thread::scope(|scope| {
            let (started_tx, started_rx) = mpsc::channel();
            let (done_tx, done_rx) = mpsc::channel();
            let authority = &authority;
            let admin = &admin;
            scope.spawn(move || {
                started_tx.send(()).unwrap();
                done_tx.send(authority.recover_incomplete(admin)).unwrap();
            });
            started_rx.recv().unwrap();
            assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());
            drop(lifecycle);
            assert_eq!(done_rx.recv().unwrap().unwrap(), vec![recovering]);
        });

        // Reconciliation is serialized with the same transaction boundary.
        let handle = recovering.public_handle();
        let grant = authority
            .mint_reconciliation_grant(
                &auth,
                session,
                workspace,
                &handle,
                ReconciliationDisposition::MarkSettled,
                60_000,
            )
            .unwrap();
        let auth_for_thread = auth.clone();
        let lifecycle = authority.attempt_lifecycle.lock().unwrap();
        std::thread::scope(|scope| {
            let (started_tx, started_rx) = mpsc::channel();
            let (done_tx, done_rx) = mpsc::channel();
            let authority = &authority;
            scope.spawn(move || {
                started_tx.send(()).unwrap();
                done_tx
                    .send(authority.apply_reconciliation(
                        &auth_for_thread,
                        grant,
                        ReconciliationEvidence::provider_receipt(ContentDigest::of_bytes(b"rcpt")),
                    ))
                    .unwrap();
            });
            started_rx.recv().unwrap();
            assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());
            drop(lifecycle);
            done_rx.recv().unwrap().unwrap();
        });

        let projection = authority
            .attempt_projection(&auth, recovering)
            .unwrap()
            .unwrap();
        assert_eq!(projection.state, STATE_SETTLED);
        assert!(!projection.ambiguous);

        // Poisoning the lifecycle boundary after a permit exists is reported
        // honestly as local lifecycle unavailability, not as an audit write
        // that was never attempted and never as a safe-to-retry failure.
        let permit = permit_for(&authority, &auth, resource, b"poison");
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _lifecycle = authority.attempt_lifecycle.lock().unwrap();
            panic!("poison attempt lifecycle for test");
        }));
        assert!(matches!(
            authority.settle_settled(permit),
            SendOutcome::Uncertain {
                reason: UncertainReason::LifecycleUnavailableAfterDispatch,
                ..
            }
        ));
    }
}
