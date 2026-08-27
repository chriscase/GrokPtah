//! Short-lived, revision-fenced control leases.
//!
//! Observation needs only a capability. Changing a run in flight additionally
//! needs a lease: an explicit, expiring grant bound to one run scope, one set
//! of control classes, and the run revision the operator actually saw. That
//! makes a stale steer or a replayed pause fail closed instead of landing on a
//! run that has since moved on.
//!
//! Leases live in memory only. A host restart invalidates every outstanding
//! lease, so control authority cannot outlive the process that issued it.
//!
//! This lease governs run steering and pausing only. It grants no Computer Use
//! authority; that surface has its own lease contract and is not implemented
//! here.

use std::collections::BTreeMap;

use grokptah_agent_sdk::RunScope;
use serde::{Deserialize, Serialize};

use crate::error::{HostError, HostResult};
use crate::identity::opaque_id;

/// One class of in-flight run control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlClass {
    /// Append a bounded steering directive to a running run.
    Steer,
    /// Halt a run so it can be resumed later.
    Pause,
    /// Continue a halted run.
    Resume,
    /// Terminate a run.
    Cancel,
}

impl ControlClass {
    /// Stable label for events and receipts.
    pub fn label(self) -> &'static str {
        match self {
            Self::Steer => "steer",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Cancel => "cancel",
        }
    }
}

/// A granted control lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlLease {
    /// Opaque lease identity.
    pub lease_id: String,
    /// Exact run scope the lease is bound to.
    pub scope: RunScope,
    /// Control classes the lease grants.
    pub classes: Vec<ControlClass>,
    /// Run revision observed when the lease was granted.
    pub granted_revision: u64,
    /// Expiry as epoch milliseconds.
    pub expires_at_ms: u64,
}

/// In-memory registry of live leases.
#[derive(Debug, Default)]
pub struct LeaseBook {
    leases: BTreeMap<String, ControlLease>,
    issued: u64,
}

impl LeaseBook {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Grant a lease bound to an exact scope, revision, and class set.
    pub fn grant(
        &mut self,
        scope: RunScope,
        classes: Vec<ControlClass>,
        revision: u64,
        ttl_ms: u64,
        now_ms: u64,
    ) -> HostResult<ControlLease> {
        scope.validate().map_err(|reason| {
            HostError::invalid("scope_invalid", format!("scope is not valid: {reason}"))
        })?;
        if classes.is_empty() {
            return Err(HostError::invalid(
                "lease_classes_empty",
                "a lease must grant at least one control class",
            ));
        }
        if ttl_ms == 0 {
            return Err(HostError::invalid(
                "lease_ttl_invalid",
                "lease ttl must be greater than zero",
            ));
        }

        let mut classes = classes;
        classes.sort_unstable();
        classes.dedup();

        self.issued += 1;
        let lease_id = opaque_id(
            "lease",
            &[
                &scope.session_id,
                &scope.run_id,
                &revision.to_string(),
                &self.issued.to_string(),
            ],
        );
        let lease = ControlLease {
            lease_id: lease_id.clone(),
            scope,
            classes,
            granted_revision: revision,
            expires_at_ms: now_ms.saturating_add(ttl_ms),
        };
        self.leases.insert(lease_id, lease.clone());
        Ok(lease)
    }

    /// Authorize one control action, or fail closed with the exact reason.
    pub fn authorize(
        &mut self,
        lease_id: &str,
        scope: &RunScope,
        class: ControlClass,
        expected_revision: u64,
        current_revision: u64,
        now_ms: u64,
    ) -> HostResult<()> {
        let Some(lease) = self.leases.get(lease_id) else {
            return Err(HostError::not_found(
                "lease_unknown",
                "no such control lease",
            ));
        };
        if lease.expires_at_ms <= now_ms {
            self.leases.remove(lease_id);
            return Err(HostError::stale(
                "lease_expired",
                "the control lease has expired",
            ));
        }
        if &lease.scope != scope {
            return Err(HostError::forbidden(
                "lease_scope_mismatch",
                "the control lease is bound to a different run scope",
            ));
        }
        if !lease.classes.contains(&class) {
            return Err(HostError::forbidden(
                "lease_class_denied",
                "the control lease does not grant this action",
            ));
        }
        if expected_revision != current_revision {
            return Err(HostError::stale(
                "revision_stale",
                "the run advanced past the observed revision",
            ));
        }
        Ok(())
    }

    /// Drop leases that have expired.
    pub fn expire(&mut self, now_ms: u64) -> usize {
        let before = self.leases.len();
        self.leases.retain(|_, lease| lease.expires_at_ms > now_ms);
        before - self.leases.len()
    }

    /// Revoke every lease bound to a run.
    pub fn revoke_run(&mut self, run_id: &str) {
        self.leases.retain(|_, lease| lease.scope.run_id != run_id);
    }

    /// Number of live leases.
    pub fn len(&self) -> usize {
        self.leases.len()
    }

    /// Whether no lease is live.
    pub fn is_empty(&self) -> bool {
        self.leases.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> RunScope {
        RunScope {
            session_id: "session-1".into(),
            workspace: "/approved".into(),
            run_id: "run-1".into(),
        }
    }

    fn book() -> (LeaseBook, ControlLease) {
        let mut book = LeaseBook::new();
        let lease = book
            .grant(scope(), vec![ControlClass::Steer], 7, 1_000, 0)
            .expect("lease granted");
        (book, lease)
    }

    #[test]
    fn a_matching_lease_authorizes_exactly_its_class_and_revision() {
        let (mut book, lease) = book();
        book.authorize(&lease.lease_id, &scope(), ControlClass::Steer, 7, 7, 100)
            .expect("authorized");

        assert_eq!(
            book.authorize(&lease.lease_id, &scope(), ControlClass::Cancel, 7, 7, 100)
                .expect_err("class is not granted")
                .reason_code(),
            "lease_class_denied"
        );
        assert_eq!(
            book.authorize(&lease.lease_id, &scope(), ControlClass::Steer, 7, 8, 100)
                .expect_err("run advanced")
                .reason_code(),
            "revision_stale"
        );
    }

    #[test]
    fn expiry_and_scope_mismatch_fail_closed() {
        let (mut book, lease) = book();
        let mut other = scope();
        other.run_id = "run-2".into();
        assert_eq!(
            book.authorize(&lease.lease_id, &other, ControlClass::Steer, 7, 7, 100)
                .expect_err("wrong scope")
                .reason_code(),
            "lease_scope_mismatch"
        );

        assert_eq!(
            book.authorize(&lease.lease_id, &scope(), ControlClass::Steer, 7, 7, 5_000)
                .expect_err("expired")
                .reason_code(),
            "lease_expired"
        );
        // An expired lease is dropped, so a retry cannot resurrect it.
        assert_eq!(
            book.authorize(&lease.lease_id, &scope(), ControlClass::Steer, 7, 7, 100)
                .expect_err("gone")
                .reason_code(),
            "lease_unknown"
        );
        assert!(book.is_empty());
    }

    #[test]
    fn revocation_and_bulk_expiry_clear_live_leases() {
        let (mut book, _) = book();
        assert_eq!(book.len(), 1);
        book.revoke_run("run-1");
        assert!(book.is_empty());

        book.grant(scope(), vec![ControlClass::Pause], 1, 10, 0)
            .expect("granted");
        assert_eq!(book.expire(1_000), 1);
        assert!(book.is_empty());
    }

    #[test]
    fn malformed_lease_requests_are_refused() {
        let mut book = LeaseBook::new();
        assert_eq!(
            book.grant(scope(), Vec::new(), 1, 10, 0)
                .expect_err("no classes")
                .reason_code(),
            "lease_classes_empty"
        );
        assert_eq!(
            book.grant(scope(), vec![ControlClass::Steer], 1, 0, 0)
                .expect_err("no ttl")
                .reason_code(),
            "lease_ttl_invalid"
        );
    }
}
