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
pub(crate) const STATE_SENDING: &str = "sending";
pub(crate) const STATE_SETTLED: &str = "settled";
pub(crate) const STATE_FAILED: &str = "failed";
pub(crate) const STATE_UNCERTAIN: &str = "uncertain";

impl HostAuthority {
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
                .ok_or(AuthorityError::UnknownResource)?
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
            if stored.control_epoch != state.control_epoch {
                return Err(AuthorityError::StaleControlEpoch);
            }
            // The presented capability must match the durable one in full.
            if stored.principal != capability.binding.principal.to_hex()
                || stored.credential_incarnation != capability.binding.incarnation.to_hex()
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
                .ok_or(AuthorityError::UnknownResource)?
                .clone();

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
    ///    record the attempt as `sending`, in one atomic, fsynced transaction.
    /// 2. Append the `SendIntent` audit record and fsync it.
    /// 3. Only then construct the permit.
    ///
    /// Any failure in steps 1 or 2 returns [`AuthorityError::Durability`] and
    /// no permit, so a pre-effect persistence failure prevents dispatch. A
    /// crash after step 1 leaves the attempt `sending`, which
    /// [`Self::recover_incomplete`] settles as
    /// [`UncertainReason::CrashBetweenDispatchAndSettlement`] rather than
    /// silently retrying.
    ///
    /// The permit is bound to `request`'s full identity — URL, method,
    /// dialect, credential, model, and body — so it cannot be carried to a
    /// different endpoint, credential, model, or body.
    pub fn begin_send(
        &self,
        auth: &AuthContext,
        lease: EffectLease,
        request: &RequestIdentity,
    ) -> Result<PhysicalSendPermit, AuthorityError> {
        if lease.effect != EffectClass::ProviderSend {
            return Err(AuthorityError::NotPermitted);
        }
        let now = unix_time_millis();
        let request_digest = request.digest();
        let body_digest = request.body_digest();

        // Step 1: consume the lease and record the attempt, atomically.
        let admitted = self.with_state(|state| {
            require_current_state(state, auth)?;
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
            let resource = state
                .resources
                .get(&stored.resource)
                .ok_or(AuthorityError::UnknownResource)?;
            if resource.observation_revision != stored.observation_revision
                || resource.observation_digest != stored.observation_digest
            {
                return Err(AuthorityError::StaleObservation);
            }

            // One-use: the lease is spent whether or not the send succeeds.
            state.leases.remove(&lease.id.to_hex());
            if let Some(cap) = state.capabilities.get_mut(&stored.capability_id) {
                cap.consumed = true;
            }

            let attempt = AttemptId::mint();
            state.attempts.insert(
                attempt.to_hex(),
                StoredAttempt {
                    attempt_id: attempt.to_hex(),
                    lease_id: stored.lease_id.clone(),
                    principal: stored.principal.clone(),
                    credential_incarnation: stored.credential_incarnation.clone(),
                    auth_generation: stored.auth_generation,
                    capability_generation: stored.capability_generation,
                    session: stored.session.clone(),
                    workspace: stored.workspace.clone(),
                    resource: stored.resource.clone(),
                    control_epoch: stored.control_epoch,
                    actor: stored.actor.clone(),
                    request_digest: request_digest.to_hex(),
                    body_digest: body_digest.to_hex(),
                    idempotency_key: idempotency_key_for(attempt),
                    state: STATE_SENDING.to_string(),
                    settlement: None,
                },
            );
            Ok((attempt, lease.binding))
        });

        // A refusal is recorded against the authenticated principal that asked.
        // Failing to record a *denial* cannot change the denial: no effect
        // occurred and none is being permitted, so the refusal still stands.
        let (attempt, binding) = match admitted {
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
                session: binding.session.public_handle(),
                workspace: binding.workspace.public_handle(),
                resource: binding.resource.public_handle(),
                actor: lease.actor.as_str().to_string(),
                request_digest: request_digest.public_handle(),
                body_digest: body_digest.public_handle(),
            },
        )?;

        // Step 3: only now does the authorisation to send physically exist.
        Ok(PhysicalSendPermit {
            attempt,
            lease: lease.id,
            binding,
            request_digest,
            body_digest,
            idempotency_key: idempotency_key_for(attempt),
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
    /// [`Self::reconcile_attempt`] after the host establishes provider truth.
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
    /// [`UncertainReason::AuditNotDurableAfterDispatch`]. That also keeps the
    /// returned outcome consistent with what recovery will conclude from the
    /// durable record, which still reads `sending`.
    fn settle(
        &self,
        permit: PhysicalSendPermit,
        durable_state: &str,
        outcome_label: &str,
        detail: Option<Result<UncertainReason, FailedReason>>,
    ) -> SendOutcome {
        let attempt = permit.attempt;
        let epoch = permit.binding.control_epoch.raw();
        let detail_text = match detail {
            None => "response observed".to_string(),
            Some(Ok(reason)) => format!("{reason:?}"),
            Some(Err(reason)) => format!("{reason:?}"),
        };

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
                reason: UncertainReason::AuditNotDurableAfterDispatch,
            };
        }

        let audited = self.append_audit(
            epoch,
            AuditEvent::SendOutcome {
                attempt: attempt.public_handle(),
                outcome: outcome_label.to_string(),
                detail: detail_text,
            },
        );
        if audited.is_err() {
            return SendOutcome::Uncertain {
                attempt,
                reason: UncertainReason::AuditNotDurableAfterDispatch,
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
    /// so it becomes ambiguous and waits for reconciliation.
    pub fn recover_incomplete(&self) -> Result<Vec<AttemptId>, AuthorityError> {
        let recovered = self.with_state(|state| {
            let mut out = Vec::new();
            for (key, record) in state.attempts.iter_mut() {
                if record.state == STATE_SENDING {
                    record.state = STATE_UNCERTAIN.to_string();
                    record.settlement = Some(format!(
                        "{:?}",
                        UncertainReason::CrashBetweenDispatchAndSettlement
                    ));
                    out.push(key.clone());
                }
            }
            Ok(out)
        })?;
        let epoch = self.read(|state| Ok(state.control_epoch))?;
        let mut ids = Vec::with_capacity(recovered.len());
        for key in recovered {
            let id: AttemptId = decode_id(&key, "attempt")?;
            self.append_audit(
                epoch,
                AuditEvent::SendOutcome {
                    attempt: id.public_handle(),
                    outcome: "uncertain".to_string(),
                    detail: format!("{:?}", UncertainReason::CrashBetweenDispatchAndSettlement),
                },
            )?;
            ids.push(id);
        }
        Ok(ids)
    }

    /// Resolve an ambiguous attempt with established provider truth.
    ///
    /// This is the only exit from [`SendOutcome::Uncertain`], and it takes a
    /// decision the host made by observing the provider, not a retry.
    pub fn reconcile_attempt(
        &self,
        attempt: AttemptId,
        took_effect: bool,
    ) -> Result<(), AuthorityError> {
        let epoch = self.with_state(|state| {
            let epoch = state.control_epoch;
            let record = state
                .attempts
                .get_mut(&attempt.to_hex())
                .ok_or(AuthorityError::UnknownResource)?;
            if record.state != STATE_UNCERTAIN {
                return Err(AuthorityError::Invalid("attempt is not uncertain"));
            }
            record.state = if took_effect {
                STATE_SETTLED
            } else {
                STATE_FAILED
            }
            .to_string();
            record.settlement = Some("reconciled".to_string());
            Ok(epoch)
        })?;
        self.append_audit(
            epoch,
            AuditEvent::AttemptReconciled {
                attempt: attempt.public_handle(),
                truth: if took_effect {
                    "took_effect"
                } else {
                    "no_effect"
                }
                .to_string(),
            },
        )
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
            Ok(Some(AttemptProjection {
                attempt: attempt.public_handle(),
                state: record.state.clone(),
                request_digest: decode_digest(&record.request_digest, "request")?.public_handle(),
                body_digest: decode_digest(&record.body_digest, "body")?.public_handle(),
                settled: record.state != STATE_SENDING,
                ambiguous: record.state == STATE_UNCERTAIN,
            }))
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

    /// Every audit record, oldest first.
    ///
    /// Exporting the whole log is an operator action, not something a served
    /// request can do: it spans every principal this root has served.
    pub fn audit_records(
        &self,
        _admin: &HostAdminAuthority,
    ) -> Result<Vec<crate::audit::AuditRecord>, AuthorityError> {
        let log = self
            .audit
            .lock()
            .map_err(|_| AuthorityError::Durability("audit log lock poisoned".into()))?;
        log.records()
    }

    /// Whether the audit hash chain is intact. An operator read.
    pub fn audit_chain_intact(&self, _admin: &HostAdminAuthority) -> Result<bool, AuthorityError> {
        let log = self
            .audit
            .lock()
            .map_err(|_| AuthorityError::Durability("audit log lock poisoned".into()))?;
        log.verify_chain()
    }
}

/// A public, secret-free, content-free, path-free view of one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptProjection {
    pub attempt: String,
    pub state: String,
    pub request_digest: String,
    pub body_digest: String,
    pub settled: bool,
    pub ambiguous: bool,
}

/// Deterministic idempotency key for an attempt, so a provider that supports
/// one deduplicates a repeat of the *same* attempt rather than acting twice.
fn idempotency_key_for(attempt: AttemptId) -> String {
    format!("grokptah-{}", attempt.public_handle())
}
