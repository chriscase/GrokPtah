//! Host-minted admission. Public SDK projections are never authority.

use grokptah_agent_sdk::authority::PublicAuthorityProjection;

use super::authority::{
    derive_grant, HostGrant, InternalExecutionSpec, LiveRevisions, MacKey, SpineError, VerifiedSpec,
};
use super::lease::AttemptLease;
use super::lifecycle::{transition_send, ExecutionLifecycle, ProviderSendState, SendRecovery};
use super::spine_persist::{
    ExecutionRecord, IdempotencyTombstone, ProviderSendRecord, SpinePersist,
};

/// One admitted, durable work item.
#[derive(Debug, Clone)]
pub struct AdmittedWork {
    /// Verified specification.
    pub verified: VerifiedSpec,
    /// Derived grant.
    pub grant: HostGrant,
    /// Execution lifecycle.
    pub lifecycle: ExecutionLifecycle,
    /// Provider-send lattice.
    pub send: ProviderSendState,
    /// Durable execution revision.
    pub revision: u64,
}

impl AdmittedWork {
    /// Redacted public projection.
    pub fn project_public(&self) -> Result<PublicAuthorityProjection, SpineError> {
        self.verified
            .spec()
            .project_public(self.lifecycle.as_public(), self.send.as_public())
    }
}

/// Host admission over a durable spine store.
pub struct DurableAdmission {
    persist: SpinePersist,
}

impl DurableAdmission {
    /// Bind to an open persist.
    pub fn new(persist: SpinePersist) -> Self {
        Self { persist }
    }

    /// Persist handle.
    pub fn persist(&self) -> &SpinePersist {
        &self.persist
    }

    /// Verify MAC, revalidate live revisions, persist spec+input+tombstone+Queued.
    ///
    /// Does not deserialize SDK projections. `spec` must already be sealed.
    pub fn admit(
        &self,
        key: &MacKey,
        spec: InternalExecutionSpec,
        live: LiveRevisions,
        private_input: &[u8],
        now_unix_ms: u64,
    ) -> Result<AdmittedWork, SpineError> {
        if private_input.len() as u32 > spec.bounds.max_prompt_bytes {
            return Err(SpineError::Utf8Ceiling);
        }
        if self.persist.load_tombstone(&spec.request_id)?.is_some() {
            return Err(SpineError::DuplicateIdentity);
        }
        live.check(&spec)?;
        let verified = spec.verify(key)?;
        let grant = derive_grant(&verified)?;
        self.persist
            .save_private_input(&verified.spec().work_id, private_input)?;
        let record = ExecutionRecord {
            spec: verified.spec().clone(),
            lifecycle: ExecutionLifecycle::Admitted,
            revision: 0,
        };
        self.persist.create_execution(&record)?;
        let queued = self.persist.cas_lifecycle(
            &verified.spec().run_id,
            0,
            ExecutionLifecycle::Admitted,
            ExecutionLifecycle::Queued,
        )?;
        self.persist.write_tombstone(&IdempotencyTombstone {
            request_id: verified.spec().request_id.clone(),
            work_id: verified.spec().work_id.clone(),
            run_id: verified.spec().run_id.clone(),
            outcome: "queued".into(),
            written_at_unix_ms: now_unix_ms,
        })?;
        let lease = AttemptLease {
            lease_id: verified.spec().lease_id.clone(),
            owner: verified.spec().lease_owner.clone(),
            epoch: verified.spec().lease_epoch,
            expiry_unix_ms: verified.spec().lease_expiry_unix_ms,
            revision: verified.spec().lease_revision,
            run_id: verified.spec().run_id.clone(),
            attempt_id: verified.spec().attempt_id.clone(),
            spec_mac_hex: verified.spec().spec_mac_hex.clone(),
        };
        self.persist.create_lease(&lease)?;
        self.persist.create_send(&ProviderSendRecord {
            provider_request_id: verified.spec().provider_request_id.clone(),
            run_id: verified.spec().run_id.clone(),
            attempt_id: verified.spec().attempt_id.clone(),
            work_id: verified.spec().work_id.clone(),
            state: ProviderSendState::KnownNotSent,
            revision: 0,
            provider_run_id: None,
        })?;
        Ok(AdmittedWork {
            verified,
            grant,
            lifecycle: queued.lifecycle,
            send: ProviderSendState::KnownNotSent,
            revision: queued.revision,
        })
    }

    /// Duplicate request replay returns the existing tombstone rather than a new mutation.
    pub fn replay_or_admit(
        &self,
        key: &MacKey,
        spec: InternalExecutionSpec,
        live: LiveRevisions,
        private_input: &[u8],
        now_unix_ms: u64,
    ) -> Result<AdmittedWork, SpineError> {
        if let Some(tombstone) = self.persist.load_tombstone(&spec.request_id)? {
            let existing = self.persist.load_execution(&tombstone.run_id)?;
            let verified = existing.spec.verify(key)?;
            if verified.spec().request_id != spec.request_id
                || verified.spec().input_digest != spec.input_digest
                || verified.spec().tenant != spec.tenant
                || verified.spec().project != spec.project
            {
                return Err(SpineError::CrossScope);
            }
            let grant = derive_grant(&verified)?;
            let send = self
                .persist
                .load_send(&verified.spec().provider_request_id)?;
            return Ok(AdmittedWork {
                verified,
                grant,
                lifecycle: existing.lifecycle,
                send: send.state,
                revision: existing.revision,
            });
        }
        self.admit(key, spec, live, private_input, now_unix_ms)
    }

    /// Cancel a Queued run before any physical send.
    pub fn cancel_before_send(
        &self,
        run_id: &str,
        revision: u64,
    ) -> Result<ExecutionLifecycle, SpineError> {
        let record = self.persist.cas_lifecycle(
            run_id,
            revision,
            ExecutionLifecycle::Queued,
            ExecutionLifecycle::Cancelled,
        )?;
        Ok(record.lifecycle)
    }

    /// Record Starting after the closed registration gate is installed.
    pub fn persist_starting(
        &self,
        run_id: &str,
        revision: u64,
    ) -> Result<ExecutionLifecycle, SpineError> {
        Ok(self
            .persist
            .cas_lifecycle(
                run_id,
                revision,
                ExecutionLifecycle::Queued,
                ExecutionLifecycle::Starting,
            )?
            .lifecycle)
    }

    /// Record Running only after start acknowledgement.
    pub fn persist_running(
        &self,
        run_id: &str,
        revision: u64,
    ) -> Result<ExecutionLifecycle, SpineError> {
        Ok(self
            .persist
            .cas_lifecycle(
                run_id,
                revision,
                ExecutionLifecycle::Starting,
                ExecutionLifecycle::Running,
            )?
            .lifecycle)
    }

    /// Begin the physical send. Only KnownNotSent may enter Sending, and only
    /// while the execution is Starting or Running.
    pub fn begin_send(
        &self,
        provider_request_id: &str,
        revision: u64,
    ) -> Result<ProviderSendState, SpineError> {
        let send = self.persist.load_send(provider_request_id)?;
        let execution = self.persist.load_execution(&send.run_id)?;
        if !matches!(
            execution.lifecycle,
            ExecutionLifecycle::Starting | ExecutionLifecycle::Running
        ) {
            return Err(SpineError::TransitionForbidden);
        }
        Ok(self
            .persist
            .cas_send(
                provider_request_id,
                revision,
                ProviderSendState::KnownNotSent,
                ProviderSendState::Sending,
            )?
            .state)
    }

    /// Crash or timeout after Sending begins is Uncertain. Never a fresh identity.
    pub fn mark_send_uncertain(
        &self,
        provider_request_id: &str,
        revision: u64,
        expected: ProviderSendState,
    ) -> Result<ProviderSendState, SpineError> {
        Ok(self
            .persist
            .cas_send(
                provider_request_id,
                revision,
                expected,
                ProviderSendState::Uncertain,
            )?
            .state)
    }

    /// Provider acknowledgement.
    pub fn mark_sent(
        &self,
        provider_request_id: &str,
        revision: u64,
    ) -> Result<ProviderSendState, SpineError> {
        Ok(self
            .persist
            .cas_send(
                provider_request_id,
                revision,
                ProviderSendState::Sending,
                ProviderSendState::Sent,
            )?
            .state)
    }

    /// Recovery decision for a persisted send state.
    pub fn recover_send(&self, provider_request_id: &str) -> Result<SendRecovery, SpineError> {
        Ok(self.persist.load_send(provider_request_id)?.state.recover())
    }

    /// Auto-retry is forbidden unless KnownNotSent.
    pub fn auto_retry_allowed(&self, provider_request_id: &str) -> Result<(), SpineError> {
        let send = self.persist.load_send(provider_request_id)?;
        if send.state.may_auto_retry() {
            Ok(())
        } else {
            Err(SpineError::AutoRetryForbidden)
        }
    }
}

/// In-memory send machine used by focused crash-cut tests that do not need disk.
#[derive(Debug, Default)]
pub struct SendCutTable {
    state: Option<ProviderSendState>,
}

impl SendCutTable {
    /// KnownNotSent after persist of dispatch intent.
    pub fn prepare(&mut self) -> Result<(), SpineError> {
        if self.state.is_some() {
            return Err(SpineError::DuplicateIdentity);
        }
        self.state = Some(ProviderSendState::KnownNotSent);
        Ok(())
    }

    /// Advance the lattice.
    pub fn step(&mut self, next: ProviderSendState) -> Result<ProviderSendState, SpineError> {
        let current = self.state.ok_or(SpineError::InvalidIdentity)?;
        let next = transition_send(current, next)?;
        self.state = Some(next);
        Ok(next)
    }

    /// Current recovery.
    pub fn recover(&self) -> Result<SendRecovery, SpineError> {
        Ok(self.state.ok_or(SpineError::InvalidIdentity)?.recover())
    }
}

/// Proof that a public projection cannot be passed to admit. This function
/// exists so the negative compile gate has a real typed admission entry.
pub fn admit_verified_only(
    key: &MacKey,
    spec: InternalExecutionSpec,
    live: LiveRevisions,
) -> Result<VerifiedSpec, SpineError> {
    live.check(&spec)?;
    spec.verify(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::authority::unsigned_provider_spec;

    #[test]
    fn durable_admit_replay_and_cancel_before_send() {
        let dir = tempfile::tempdir().unwrap();
        let persist = SpinePersist::open(dir.path()).unwrap();
        let admission = DurableAdmission::new(persist);
        let key = MacKey::from_bytes(&[0x33; 32]).unwrap();
        let spec = unsigned_provider_spec("adm", "prompt-a")
            .seal(&key)
            .unwrap();
        let first = admission
            .admit(
                &key,
                spec.clone(),
                LiveRevisions::default(),
                b"prompt-a",
                10,
            )
            .unwrap();
        assert_eq!(first.lifecycle, ExecutionLifecycle::Queued);
        let replay = admission
            .replay_or_admit(
                &key,
                spec.clone(),
                LiveRevisions::default(),
                b"prompt-a",
                11,
            )
            .unwrap();
        assert_eq!(replay.verified.spec().run_id, first.verified.spec().run_id);
        admission
            .cancel_before_send(&first.verified.spec().run_id, first.revision)
            .unwrap();
        assert_eq!(
            admission.begin_send(&first.verified.spec().provider_request_id, 0),
            Err(SpineError::TransitionForbidden)
        );
        admission
            .auto_retry_allowed(&first.verified.spec().provider_request_id)
            .unwrap();
    }

    #[test]
    fn policy_and_credential_drift_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let persist = SpinePersist::open(dir.path()).unwrap();
        let admission = DurableAdmission::new(persist);
        let key = MacKey::from_bytes(&[0x33; 32]).unwrap();
        let spec = unsigned_provider_spec("drift", "prompt-b")
            .seal(&key)
            .unwrap();
        let policy_drift = LiveRevisions {
            policy: crate::orchestration::authority::Revision::new(2),
            ..LiveRevisions::default()
        };
        assert_eq!(
            admission
                .admit(&key, spec.clone(), policy_drift, b"prompt-b", 10)
                .unwrap_err(),
            SpineError::StaleRevision
        );
        let credential_drift = LiveRevisions {
            credential: crate::orchestration::authority::Revision::new(9),
            ..LiveRevisions::default()
        };
        assert_eq!(
            admission
                .admit(&key, spec, credential_drift, b"prompt-b", 10)
                .unwrap_err(),
            SpineError::StaleRevision
        );
    }
}
