//! Operator reconciliation of Uncertain provider attempts.
//!
//! This is not a second attempt ledger. Grants and dispositions operate on the
//! existing [`StoredAttempt`] records, consume one-use operator leases, and
//! never emit a physical send.

use crate::HostAuthority;
use crate::audit::AuditEvent;
use crate::digest::ContentDigest;
use crate::error::AuthorityError;
use crate::gates::{
    AttemptProjection, STATE_DISCARDED, STATE_FAILED, STATE_PREPARING, STATE_SENDING,
    STATE_SETTLED, STATE_UNCERTAIN, attempt_in_scope, attempt_revision, find_attempt_by_handle,
    project_stored_attempt,
};
use crate::ids::*;
use crate::receipt::*;
use crate::state::{StoredAttempt, StoredAuthority};
use crate::store::{decode_digest, decode_id, require_current_state, unix_time_millis};

const RECONCILE_ACTION: &str = "operator-reconcile-v1";
const DEFAULT_GRANT_TTL_MS: u64 = 15_000;
const LEGACY_RECONCILE_MIGRATION: &str =
    "legacy reconcile_attempt is retired; use mint_reconciliation_grant and apply_reconciliation";

impl HostAuthority {
    /// Mint a short-lived grant bound to one attempt at its current revision.
    ///
    /// The grant is bound to the exact attempt, revision, durable state, wire
    /// dialect, route digest, capability and policy generation, principal and
    /// authentication generation, and chosen disposition. It never authorises
    /// a physical send.
    ///
    /// Unknown and foreign attempt handles fail identically as
    /// [`AuthorityError::UnknownResource`].
    pub fn mint_reconciliation_grant(
        &self,
        auth: &AuthContext,
        session: SessionId,
        workspace: WorkspaceId,
        attempt_handle: &str,
        disposition: ReconciliationDisposition,
        ttl_ms: u64,
    ) -> Result<ReconciliationGrant, AuthorityError> {
        let ttl_ms = if ttl_ms == 0 {
            DEFAULT_GRANT_TTL_MS
        } else {
            ttl_ms
        };
        let (attempt, record) = self.read(|state| {
            require_current_state(state, auth)?;
            let (attempt, record) =
                lookup_scoped_attempt(state, auth, session, workspace, attempt_handle)?;
            validate_reconciliation_binding(auth, &record)?;
            Ok((attempt, record))
        })?;
        match disposition {
            ReconciliationDisposition::Review => {}
            ReconciliationDisposition::MarkNotSent => {
                if record.state == STATE_SENDING {
                    return Err(AuthorityError::Invalid(
                        "attempt is still in flight; wait for settlement before reconciling",
                    ));
                }
                if record.state != STATE_UNCERTAIN
                    && !host_has_pre_wire_evidence(self, &attempt, &record)?
                {
                    return Err(AuthorityError::Invalid(
                        "mark-not-sent requires host-proven pre-wire evidence",
                    ));
                }
            }
            ReconciliationDisposition::MarkSettled | ReconciliationDisposition::Discard => {
                if record.state == STATE_SENDING {
                    return Err(AuthorityError::Invalid(
                        "attempt is still in flight; wait for settlement before reconciling",
                    ));
                }
                if record.state != STATE_UNCERTAIN {
                    if already_matches_disposition(&record, disposition) {
                        // Idempotent remint against a completed decision is
                        // allowed so a retry can present the same grant path.
                    } else {
                        return Err(AuthorityError::Invalid("attempt is already settled"));
                    }
                }
            }
        }

        let resource: ResourceIncarnation = decode_id(&record.resource, "resource")?;
        let capability = self.seal_capability(
            auth,
            resource,
            ActorClass::VerifiedOperator,
            EffectClass::OperatorReconcile,
            ttl_ms,
        )?;
        let revision = attempt_revision(attempt, &record);
        let route_digest = decode_digest(&record.route_digest, "route")?;
        let action = reconcile_action_digest(
            auth,
            &attempt,
            &revision,
            &record,
            &route_digest,
            disposition,
        );
        let lease = self.mint_lease(auth, &capability, action, ttl_ms)?;
        Ok(ReconciliationGrant {
            expires_at_ms: lease.expires_at_ms,
            lease,
            attempt,
            revision,
            state: record.state.clone(),
            dialect: record.dialect.clone(),
            route_digest,
            disposition,
        })
    }

    /// Mint a reconciliation grant from an attempt id alone.
    ///
    /// Scope is taken from the durable attempt record. Foreign or unknown
    /// attempts still fail as [`AuthorityError::UnknownResource`].
    pub fn mint_reconciliation_grant_for_attempt(
        &self,
        auth: &AuthContext,
        attempt: AttemptId,
        disposition: ReconciliationDisposition,
        ttl_ms: u64,
    ) -> Result<ReconciliationGrant, AuthorityError> {
        let (session, workspace, handle) = self.read(|state| {
            require_current_state(state, auth)?;
            let record = state
                .attempts
                .get(&attempt.to_hex())
                .ok_or(AuthorityError::UnknownResource)?
                .clone();
            if record.principal != auth.principal.to_hex() {
                return Err(AuthorityError::UnknownResource);
            }
            validate_reconciliation_binding(auth, &record)?;
            let session: SessionId = decode_id(&record.session, "session")?;
            let workspace: WorkspaceId = decode_id(&record.workspace, "workspace")?;
            Ok((session, workspace, attempt.public_handle()))
        })?;
        self.mint_reconciliation_grant(auth, session, workspace, &handle, disposition, ttl_ms)
    }

    /// Spend a reconciliation grant. No provider I/O and no resend.
    ///
    /// Revision-CAS: a stale grant mutates nothing. A repeated decision that
    /// already matches durable truth is idempotent. Conflicting decisions fail.
    pub fn apply_reconciliation(
        &self,
        auth: &AuthContext,
        grant: ReconciliationGrant,
        evidence: ReconciliationEvidence,
    ) -> Result<AttemptProjection, AuthorityError> {
        let _lifecycle = self.lock_attempt_lifecycle()?;
        let now = unix_time_millis();
        if grant.expires_at_ms <= now {
            return Err(AuthorityError::Expired);
        }
        if grant.lease.effect != EffectClass::OperatorReconcile {
            return Err(AuthorityError::NotPermitted);
        }
        if !grant.lease.actor.is_operator() {
            return Err(AuthorityError::NotPermitted);
        }

        let current = self.read(|state| {
            require_current_state(state, auth)?;
            let record = state
                .attempts
                .get(&grant.attempt.to_hex())
                .ok_or(AuthorityError::UnknownResource)?
                .clone();
            if record.principal != auth.principal.to_hex() {
                return Err(AuthorityError::UnknownResource);
            }
            validate_reconciliation_binding(auth, &record)?;
            Ok(record)
        })?;

        let current_revision = attempt_revision(grant.attempt, &current);
        let already_applied = already_matches_disposition(&current, grant.disposition);
        if current_revision != grant.revision {
            if already_applied {
                validate_reconciliation_binding(auth, &current)?;
                self.consume_reconciliation_lease(auth, &grant, now)?;
                return project_from_current(grant.attempt, &current);
            }
            return Err(AuthorityError::Invalid("stale revision"));
        }
        if grant.state != current.state
            || grant.dialect != current.dialect
            || grant.route_digest.to_hex() != current.route_digest
        {
            return Err(AuthorityError::Invalid("grant binding mismatch"));
        }

        match grant.disposition {
            ReconciliationDisposition::Review => {}
            ReconciliationDisposition::MarkNotSent => {
                if !can_mark_not_sent(self, &grant.attempt, &current, &evidence)? {
                    return Err(AuthorityError::Invalid(
                        "mark-not-sent requires host-proven pre-wire evidence or operator observation",
                    ));
                }
            }
            ReconciliationDisposition::MarkSettled => {
                if !evidence.has_identity_proof() {
                    return Err(AuthorityError::Invalid(
                        "mark-settled requires provider receipt or operator observation",
                    ));
                }
                if current.state != STATE_UNCERTAIN && !already_applied {
                    return Err(AuthorityError::Invalid("attempt is already settled"));
                }
            }
            ReconciliationDisposition::Discard => {
                if current.state != STATE_UNCERTAIN && !already_applied {
                    return Err(AuthorityError::Invalid("attempt is already settled"));
                }
            }
        }

        if current.state == STATE_SENDING
            && grant.disposition != ReconciliationDisposition::Review
            && !already_applied
        {
            return Err(AuthorityError::Invalid(
                "attempt is still in flight; wait for settlement before reconciling",
            ));
        }

        let expected_action = reconcile_action_digest(
            auth,
            &grant.attempt,
            &grant.revision,
            &current,
            &grant.route_digest,
            grant.disposition,
        );
        if grant.lease.action_digest() != expected_action {
            return Err(AuthorityError::DigestMismatch);
        }

        if already_applied && grant.disposition != ReconciliationDisposition::Review {
            validate_reconciliation_binding(auth, &current)?;
            self.consume_reconciliation_lease(auth, &grant, now)?;
            return project_from_current(grant.attempt, &current);
        }

        if let Some(truth) = grant.disposition.truth()
            && let Some(existing) = audited_reconciliation_truth(self, &grant.attempt)?
            && existing != truth
        {
            return Err(AuthorityError::Invalid("conflicting reconciliation"));
        }

        let (current, epoch) = self.read(|state| {
            require_current_state(state, auth)?;
            let record = state
                .attempts
                .get(&grant.attempt.to_hex())
                .ok_or(AuthorityError::UnknownResource)?
                .clone();
            validate_reconciliation_binding(auth, &record)?;
            let current_revision = attempt_revision(grant.attempt, &record);
            if current_revision != grant.revision {
                return Err(AuthorityError::Invalid("stale revision"));
            }
            Ok((record, state.control_epoch))
        })?;

        let event = match grant.disposition.truth() {
            Some(truth) => AuditEvent::AttemptReconciled {
                attempt: grant.attempt.public_handle(),
                truth: truth.to_string(),
            },
            None => AuditEvent::AttemptReviewed {
                attempt: grant.attempt.public_handle(),
                principal: auth.principal.public_handle(),
                disposition: grant.disposition.as_str().to_string(),
            },
        };
        self.append_audit(epoch, event)?;
        let next_state = match grant.disposition {
            ReconciliationDisposition::Review => current.state.clone(),
            ReconciliationDisposition::MarkNotSent => STATE_FAILED.to_string(),
            ReconciliationDisposition::MarkSettled => STATE_SETTLED.to_string(),
            ReconciliationDisposition::Discard => STATE_DISCARDED.to_string(),
        };
        let settlement = match grant.disposition {
            ReconciliationDisposition::Review => current.settlement.clone(),
            other => Some(format!("reconciled:{}", other.as_str())),
        };

        self.with_state(|state| {
            require_current_state(state, auth)?;
            consume_grant_records(state, &grant, now)?;
            let record = state
                .attempts
                .get_mut(&grant.attempt.to_hex())
                .ok_or(AuthorityError::UnknownResource)?;
            if attempt_revision(grant.attempt, record) != grant.revision {
                return Err(AuthorityError::Invalid("stale revision"));
            }
            record.state = next_state.clone();
            record.settlement = settlement.clone();
            Ok(())
        })?;

        self.read(|state| {
            let record = state
                .attempts
                .get(&grant.attempt.to_hex())
                .ok_or(AuthorityError::UnknownResource)?;
            project_stored_attempt(grant.attempt, record)
        })
    }
}

pub(crate) const LEGACY_RECONCILE_ATTEMPT_RETIRED: &str = LEGACY_RECONCILE_MIGRATION;

fn validate_reconciliation_binding(
    auth: &AuthContext,
    record: &StoredAttempt,
) -> Result<(), AuthorityError> {
    if record.credential_incarnation != auth.incarnation.to_hex() {
        return Err(AuthorityError::StalePrincipal);
    }
    if record.auth_generation != auth.auth_generation.raw() {
        return Err(AuthorityError::StalePrincipal);
    }
    if record.capability_generation != auth.capability_generation.raw() {
        return Err(AuthorityError::StaleCapability);
    }
    if record.policy_revision != auth.policy_revision.raw() {
        return Err(AuthorityError::StalePolicy);
    }
    Ok(())
}

fn audited_reconciliation_truth(
    authority: &HostAuthority,
    attempt: &AttemptId,
) -> Result<Option<&'static str>, AuthorityError> {
    let handle = attempt.public_handle();
    let log = authority
        .audit
        .lock()
        .map_err(|_| AuthorityError::Durability("audit log lock poisoned".into()))?;
    if !log.verify_chain()? {
        return Err(AuthorityError::CorruptState(
            "audit chain is damaged".into(),
        ));
    }
    let mut truth = None;
    for record in log.records()? {
        if let AuditEvent::AttemptReconciled {
            attempt: audited,
            truth: audited_truth,
        } = record.event
        {
            if audited != handle {
                continue;
            }
            let Some(normalized) = normalize_reconciliation_truth(&audited_truth) else {
                return Err(AuthorityError::CorruptState(format!(
                    "unknown audited reconciliation truth {audited_truth}"
                )));
            };
            if let Some(existing) = truth {
                if existing != normalized {
                    return Err(AuthorityError::CorruptState(
                        "conflicting reconciliation audit records for one attempt".into(),
                    ));
                }
            } else {
                truth = Some(normalized);
            }
        }
    }
    Ok(truth)
}

fn normalize_reconciliation_truth(truth: &str) -> Option<&'static str> {
    match truth {
        "took_effect" => Some("took_effect"),
        "no_effect" => Some("no_effect"),
        "discarded" => Some("discarded"),
        _ => None,
    }
}

fn lookup_scoped_attempt(
    state: &StoredAuthority,
    auth: &AuthContext,
    session: SessionId,
    workspace: WorkspaceId,
    attempt_handle: &str,
) -> Result<(AttemptId, StoredAttempt), AuthorityError> {
    let Some((attempt, record)) = find_attempt_by_handle(state, attempt_handle)? else {
        return Err(AuthorityError::UnknownResource);
    };
    if !attempt_in_scope(&record, auth, session, workspace) {
        return Err(AuthorityError::UnknownResource);
    }
    Ok((attempt, record))
}

fn project_from_current(
    attempt: AttemptId,
    record: &StoredAttempt,
) -> Result<AttemptProjection, AuthorityError> {
    project_stored_attempt(attempt, record)
}

fn already_matches_disposition(
    record: &StoredAttempt,
    disposition: ReconciliationDisposition,
) -> bool {
    match disposition {
        ReconciliationDisposition::Review => true,
        ReconciliationDisposition::MarkNotSent => record.state == STATE_FAILED,
        ReconciliationDisposition::MarkSettled => record.state == STATE_SETTLED,
        ReconciliationDisposition::Discard => record.state == STATE_DISCARDED,
    }
}

fn can_mark_not_sent(
    authority: &HostAuthority,
    attempt: &AttemptId,
    record: &StoredAttempt,
    evidence: &ReconciliationEvidence,
) -> Result<bool, AuthorityError> {
    if host_has_pre_wire_evidence(authority, attempt, record)? {
        return Ok(true);
    }
    if record.state == STATE_UNCERTAIN
        && evidence.has_identity_proof()
        && wire_admission_recorded(authority, attempt)?
    {
        return Ok(true);
    }
    Ok(false)
}

fn is_operator_reconciled(record: &StoredAttempt) -> bool {
    record
        .settlement
        .as_deref()
        .is_some_and(|detail| detail.starts_with("reconciled"))
}

pub(crate) fn absorb_settlement_for_reconciled_attempt(
    attempt: AttemptId,
    record: &StoredAttempt,
) -> Option<SendOutcome> {
    if !is_operator_reconciled(record) {
        return None;
    }
    Some(match record.state.as_str() {
        STATE_SETTLED => SendOutcome::Settled { attempt },
        STATE_FAILED => SendOutcome::Failed {
            attempt,
            reason: FailedReason::AbandonedBeforeWireAdmission,
        },
        STATE_DISCARDED => SendOutcome::Uncertain {
            attempt,
            reason: UncertainReason::TransportAfterPossibleWrite,
        },
        STATE_UNCERTAIN => SendOutcome::Uncertain {
            attempt,
            reason: UncertainReason::TransportAfterPossibleWrite,
        },
        _ => return None,
    })
}

fn host_has_pre_wire_evidence(
    authority: &HostAuthority,
    attempt: &AttemptId,
    record: &StoredAttempt,
) -> Result<bool, AuthorityError> {
    if record.state == STATE_PREPARING {
        return Ok(true);
    }
    if record.state == STATE_FAILED
        && record
            .settlement
            .as_deref()
            .is_some_and(|detail| detail.contains("AbandonedBeforeWireAdmission"))
    {
        return Ok(true);
    }
    if record.state == STATE_SENDING || record.state == STATE_UNCERTAIN {
        return Ok(!wire_admission_recorded(authority, attempt)?);
    }
    Ok(false)
}

fn wire_admission_recorded(
    authority: &HostAuthority,
    attempt: &AttemptId,
) -> Result<bool, AuthorityError> {
    let handle = attempt.public_handle();
    let log = authority
        .audit
        .lock()
        .map_err(|_| AuthorityError::Durability("audit log lock poisoned".into()))?;
    if !log.verify_chain()? {
        return Err(AuthorityError::CorruptState(
            "audit chain is damaged".into(),
        ));
    }
    for record in log.records()? {
        if let AuditEvent::SendWireAdmission { attempt } = record.event
            && attempt == handle
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn reconcile_action_digest(
    auth: &AuthContext,
    attempt: &AttemptId,
    revision: &ContentDigest,
    record: &StoredAttempt,
    route_digest: &ContentDigest,
    disposition: ReconciliationDisposition,
) -> ContentDigest {
    ContentDigest::of_fields(&[
        (RECONCILE_ACTION, b""),
        ("attempt", attempt.to_hex().as_bytes()),
        ("revision", revision.to_hex().as_bytes()),
        ("state", record.state.as_bytes()),
        ("dialect", record.dialect.as_bytes()),
        ("route", route_digest.to_hex().as_bytes()),
        ("disposition", disposition.as_str().as_bytes()),
        ("principal", auth.principal.to_hex().as_bytes()),
        ("auth_generation", &auth.auth_generation.raw().to_le_bytes()),
        (
            "capability_generation",
            &auth.capability_generation.raw().to_le_bytes(),
        ),
        ("policy_revision", &auth.policy_revision.raw().to_le_bytes()),
    ])
}

fn consume_grant_records(
    state: &mut StoredAuthority,
    grant: &ReconciliationGrant,
    now: u64,
) -> Result<(), AuthorityError> {
    let stored = state
        .leases
        .get_mut(&grant.lease.id.to_hex())
        .ok_or(AuthorityError::AlreadyConsumed)?;
    if stored.consumed {
        return Err(AuthorityError::AlreadyConsumed);
    }
    if stored.expires_at_ms <= now {
        return Err(AuthorityError::Expired);
    }
    if stored.effect != EffectClass::OperatorReconcile.as_str() {
        return Err(AuthorityError::NotPermitted);
    }
    if stored.capability_generation != state.capability_generation {
        return Err(AuthorityError::StaleCapability);
    }
    if stored.policy_revision != state.policy_revision {
        return Err(AuthorityError::StalePolicy);
    }
    if stored.auth_generation != grant.lease.binding.auth_generation.raw() {
        return Err(AuthorityError::StalePrincipal);
    }
    if stored.principal != grant.lease.binding.principal.to_hex() {
        return Err(AuthorityError::ResourceOwnershipMismatch);
    }
    stored.consumed = true;
    if let Some(capability) = state.capabilities.get_mut(&stored.capability_id) {
        capability.consumed = true;
    }
    Ok(())
}

impl HostAuthority {
    fn consume_reconciliation_lease(
        &self,
        auth: &AuthContext,
        grant: &ReconciliationGrant,
        now: u64,
    ) -> Result<(), AuthorityError> {
        self.with_state(|state| {
            require_current_state(state, auth)?;
            consume_grant_records(state, grant, now)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{HostAdminCredential, HostCredential};

    #[test]
    fn operator_reconcile_lease_cannot_begin_send() {
        let dir = tempfile::tempdir().unwrap();
        let (authority, admin) = HostAuthority::open(
            dir.path(),
            &HostAdminCredential::new("host-admin-custody-secret-32-bytes-minimum-v1").unwrap(),
        )
        .unwrap();
        authority
            .set_credentials(
                &admin,
                &[HostCredential::new("a", "secret-a-value-32-bytes-minimum!!").unwrap()],
            )
            .unwrap();
        let auth = authority
            .authenticate("secret-a-value-32-bytes-minimum!!")
            .unwrap();
        let workspace = authority.issue_workspace(&auth, dir.path()).unwrap();
        let resource = authority
            .obtain_provider_send_surface(&auth, workspace, "agent-step")
            .unwrap();
        let binding = authority.resource_binding(&auth, resource).unwrap();
        let session = binding.session();
        let capability = authority
            .seal_capability(
                &auth,
                resource,
                ActorClass::VerifiedOperator,
                EffectClass::ProviderSend,
                60_000,
            )
            .unwrap();
        let request = crate::RequestIdentity::new(
            "https://api.example.invalid/v1/chat",
            "POST",
            "openai-chat",
            b"provider-key",
            "grok-4",
            b"body",
        );
        let lease = authority
            .mint_lease(&auth, &capability, request.digest(), 60_000)
            .unwrap();
        let permit = authority
            .begin_send(&auth, lease, &request, "agent-step")
            .unwrap();
        let handle = permit.attempt().public_handle();
        let _ = authority.settle_uncertain(
            authority.admit_sending(&auth, permit).unwrap(),
            UncertainReason::TransportAfterPossibleWrite,
        );
        let grant = authority
            .mint_reconciliation_grant(
                &auth,
                session,
                workspace,
                &handle,
                ReconciliationDisposition::Review,
                60_000,
            )
            .unwrap();
        assert_eq!(grant.effect(), EffectClass::OperatorReconcile);
        let err = authority
            .begin_send(&auth, grant.lease.clone(), &request, "agent-step")
            .unwrap_err();
        assert_eq!(err, AuthorityError::NotPermitted);
    }

    #[test]
    fn replay_preserves_operator_reconciliation_over_later_send_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let (authority, admin) = HostAuthority::open(
            dir.path(),
            &HostAdminCredential::new("host-admin-custody-secret-32-bytes-minimum-v1").unwrap(),
        )
        .unwrap();
        authority
            .set_credentials(
                &admin,
                &[HostCredential::new("a", "secret-a-value-32-bytes-minimum!!").unwrap()],
            )
            .unwrap();
        let auth = authority
            .authenticate("secret-a-value-32-bytes-minimum!!")
            .unwrap();
        let workspace = authority.issue_workspace(&auth, dir.path()).unwrap();
        let resource = authority
            .obtain_provider_send_surface(&auth, workspace, "agent-step")
            .unwrap();
        let binding = authority.resource_binding(&auth, resource).unwrap();
        let session = binding.session();
        let capability = authority
            .seal_capability(
                &auth,
                resource,
                ActorClass::VerifiedOperator,
                EffectClass::ProviderSend,
                60_000,
            )
            .unwrap();
        let request = crate::RequestIdentity::new(
            "https://api.example.invalid/v1/chat",
            "POST",
            "openai-chat",
            b"provider-key",
            "grok-4",
            b"replay",
        );
        let lease = authority
            .mint_lease(&auth, &capability, request.digest(), 60_000)
            .unwrap();
        let permit = authority
            .begin_send(&auth, lease, &request, "agent-step")
            .unwrap();
        let attempt = permit.attempt();
        let handle = attempt.public_handle();
        let _ = authority.settle_uncertain(
            authority.admit_sending(&auth, permit).unwrap(),
            UncertainReason::TransportAfterPossibleWrite,
        );
        let grant = authority
            .mint_reconciliation_grant(
                &auth,
                session,
                workspace,
                &handle,
                ReconciliationDisposition::Discard,
                60_000,
            )
            .unwrap();
        authority
            .apply_reconciliation(&auth, grant, ReconciliationEvidence::default())
            .unwrap();
        let epoch = authority.read(|state| Ok(state.control_epoch)).unwrap();
        authority
            .append_audit(
                epoch,
                AuditEvent::SendOutcome {
                    attempt: handle.clone(),
                    outcome: "uncertain".to_string(),
                    detail: "synthetic late settlement".to_string(),
                },
            )
            .unwrap();
        drop(authority);
        let (authority, _admin) = HostAuthority::open(
            dir.path(),
            &HostAdminCredential::new("host-admin-custody-secret-32-bytes-minimum-v1").unwrap(),
        )
        .unwrap();
        let auth = authority
            .authenticate("secret-a-value-32-bytes-minimum!!")
            .unwrap();
        authority.replay_attempt_settlements().unwrap();
        let projection = authority
            .attempt_projection(&auth, attempt)
            .unwrap()
            .unwrap();
        assert_eq!(projection.state, "discarded");
    }

    #[test]
    fn settle_absorbs_reconciled_attempt_without_competing_audit() {
        let dir = tempfile::tempdir().unwrap();
        let (authority, admin) = HostAuthority::open(
            dir.path(),
            &HostAdminCredential::new("host-admin-custody-secret-32-bytes-minimum-v1").unwrap(),
        )
        .unwrap();
        authority
            .set_credentials(
                &admin,
                &[HostCredential::new("a", "secret-a-value-32-bytes-minimum!!").unwrap()],
            )
            .unwrap();
        let auth = authority
            .authenticate("secret-a-value-32-bytes-minimum!!")
            .unwrap();
        let workspace = authority.issue_workspace(&auth, dir.path()).unwrap();
        let resource = authority
            .obtain_provider_send_surface(&auth, workspace, "agent-step")
            .unwrap();
        let capability = authority
            .seal_capability(
                &auth,
                resource,
                ActorClass::VerifiedOperator,
                EffectClass::ProviderSend,
                60_000,
            )
            .unwrap();
        let request = crate::RequestIdentity::new(
            "https://api.example.invalid/v1/chat",
            "POST",
            "openai-chat",
            b"provider-key",
            "grok-4",
            b"absorb",
        );
        let lease = authority
            .mint_lease(&auth, &capability, request.digest(), 60_000)
            .unwrap();
        let permit = authority
            .begin_send(&auth, lease, &request, "agent-step")
            .unwrap();
        let attempt = permit.attempt();
        let admitted = authority.admit_sending(&auth, permit).unwrap();
        let audit_before = authority.audit_records(&admin).unwrap().len();
        authority
            .with_state(|state| {
                let record = state
                    .attempts
                    .get_mut(&attempt.to_hex())
                    .ok_or(AuthorityError::UnknownResource)?;
                record.state = STATE_DISCARDED.to_string();
                record.settlement = Some("reconciled:discard".to_string());
                Ok(())
            })
            .unwrap();
        let outcome =
            authority.settle_uncertain(admitted, UncertainReason::TransportAfterPossibleWrite);
        assert!(matches!(outcome, SendOutcome::Uncertain { .. }));
        assert_eq!(authority.audit_records(&admin).unwrap().len(), audit_before);
        let projection = authority
            .attempt_projection(&auth, attempt)
            .unwrap()
            .unwrap();
        assert_eq!(projection.state, "discarded");
    }
}
