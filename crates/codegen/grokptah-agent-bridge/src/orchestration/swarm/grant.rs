//! The single authoritative Computer Use consumption and revocation path.
//!
//! There is deliberately one function that can turn a grant into permission to
//! act, and one that can take that permission away. Both operate on the
//! existing [`ActionGrant`] issued by the Computer Use ledger — nothing here
//! mints, extends, or revalidates a grant of its own, so there is no second
//! credential universe to keep in sync.
//!
//! Consumption is durable and single-winner. The claim key includes the
//! Computer Use run's control epoch, so a pause, takeover, stop, or recovery
//! makes every binding captured before it unusable, and the new owner's
//! binding is a distinct key rather than a contested one.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::computer_use::{ActionClass, ActionGrant, ComputerRun};
use crate::orchestration::types::{hash_payload, OrchError, OrchErrorCode};

use super::ids::{AttemptId, GrantId, LeaseId};
use super::state::{GrantBinding, LeaseRecord};
use super::store::{ClaimOutcome, SwarmStore};

fn forbidden(message: impl Into<String>) -> OrchError {
    OrchError::new(OrchErrorCode::ForbiddenScope, message)
}

fn conflict(message: impl Into<String>) -> OrchError {
    OrchError::new(OrchErrorCode::Conflict, message)
}

/// Stable, secret-free digest of the exact target a grant names.
pub fn target_fingerprint(run: &ComputerRun) -> String {
    hash_payload(&serde_json::json!({ "target": run.target }))
}

/// Bind an externally issued grant to exactly one lease and attempt.
///
/// This does not consume anything. It records what the single consumption path
/// will later have to verify.
pub fn bind_grant(
    grant: &ActionGrant,
    run: &ComputerRun,
    lease_id: &LeaseId,
    attempt_id: &AttemptId,
) -> Result<GrantBinding, OrchError> {
    grant
        .validate()
        .map_err(|error| forbidden(format!("computer use grant is invalid: {}", error.message)))?;
    if grant.run_id != run.run_id {
        return Err(forbidden("grant does not belong to the named computer run"));
    }
    if grant.target != run.target {
        return Err(forbidden("grant target does not match the computer run"));
    }
    GrantBinding::new(
        GrantId::parse(grant.grant_id.clone())?,
        run.run_id.clone(),
        target_fingerprint(run),
        run.owner_session_id,
        run.control_epoch,
        lease_id,
        attempt_id,
    )
}

/// Outcome of the one consumption path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantConsumption {
    pub grant_id: GrantId,
    pub lease_id: LeaseId,
    pub attempt_id: AttemptId,
    pub control_epoch: u64,
    pub action_classes: BTreeSet<ActionClass>,
    pub consumed_at: DateTime<Utc>,
}

/// Consume a bound grant for one action.
///
/// Every check is a refusal, in a fixed order: the binding must name this exact
/// lease and attempt; the live grant and run must still agree with what was
/// bound; the run's control epoch must not have moved; the grant must be live,
/// unrevoked, and cover the requested class; and the durable claim must be won.
/// A caller that loses the claim never acts.
#[allow(clippy::too_many_arguments)]
pub fn consume_grant_for_action(
    store: &SwarmStore,
    lease: &LeaseRecord,
    grant: &ActionGrant,
    run: &ComputerRun,
    action_class: ActionClass,
    owner_session_id: Uuid,
    now: DateTime<Utc>,
) -> Result<GrantConsumption, OrchError> {
    let binding = lease
        .grant
        .as_ref()
        .ok_or_else(|| forbidden("lease carries no computer use grant binding"))?;
    binding.verify_binding(&lease.lease_id, &lease.attempt_id)?;

    if binding.grant_id.as_str() != grant.grant_id {
        return Err(forbidden("presented grant is not the bound grant"));
    }
    if binding.computer_run_id != run.run_id || grant.run_id != run.run_id {
        return Err(forbidden("grant and run identities disagree"));
    }
    if binding.target_fingerprint != target_fingerprint(run) {
        return Err(forbidden("computer run target changed since the binding"));
    }
    if binding.owner_session_id != run.owner_session_id
        || binding.owner_session_id != owner_session_id
    {
        return Err(OrchError::new(
            OrchErrorCode::WorkspaceMismatch,
            "computer run owner session does not match the binding",
        ));
    }
    // A takeover, pause, stop, or recovery bumps the epoch. The binding minted
    // before it is stale by construction, whatever else still looks valid.
    if binding.control_epoch != run.control_epoch {
        return Err(OrchError::new(
            OrchErrorCode::StaleVersion,
            "computer run control epoch moved; this binding was superseded",
        ));
    }
    grant
        .validate()
        .map_err(|error| forbidden(format!("computer use grant is invalid: {}", error.message)))?;
    if grant.revoked_at.is_some() {
        return Err(forbidden("computer use grant was revoked"));
    }
    if now < grant.issued_at || now >= grant.expires_at {
        return Err(forbidden("computer use grant is not currently valid"));
    }
    if grant.uses_remaining == Some(0) {
        return Err(forbidden("computer use grant has no remaining uses"));
    }
    if !grant.action_classes.contains(&action_class) {
        return Err(forbidden("action class is outside the grant"));
    }
    if !lease.is_live() {
        return Err(conflict("lease is not live"));
    }
    if now >= lease.expires_at {
        return Err(conflict("lease execution bound has expired"));
    }

    let holder = consumption_holder(&lease.lease_id, &lease.attempt_id);
    match store.consume_grant(&binding.grant_id, binding.control_epoch, &holder)? {
        ClaimOutcome::Won => {}
        ClaimOutcome::AlreadyHeld => {
            let existing = store.grant_holder(&binding.grant_id, binding.control_epoch)?;
            if existing.as_deref() == Some(holder.as_str()) {
                // Same lease and attempt replaying its own consumption.
            } else {
                return Err(conflict(
                    "computer use grant was already consumed by another holder",
                ));
            }
        }
    }
    Ok(GrantConsumption {
        grant_id: binding.grant_id.clone(),
        lease_id: lease.lease_id.clone(),
        attempt_id: lease.attempt_id.clone(),
        control_epoch: binding.control_epoch,
        action_classes: grant.action_classes.clone(),
        consumed_at: now,
    })
}

/// The one revocation path.
///
/// Revocation is expressed by advancing the run's control epoch: every binding
/// minted before it is stale, so there is no window in which a revoked grant
/// still consumes. Returns the epoch a caller must now present.
pub fn revoke_bound_grants(run: &ComputerRun) -> u64 {
    run.control_epoch.saturating_add(1)
}

fn consumption_holder(lease_id: &LeaseId, attempt_id: &AttemptId) -> String {
    format!("{lease_id}:{attempt_id}")
}
