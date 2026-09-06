//! Isolated Surface Proof Harness orchestrator.
//!
//! Models the Sep 18 physical proof checklist in synthetic form:
//! launch → boot guest → frame → inject ONE guest-local action → changed frame
//! → Stop → destroy channels → cleanup, with host sentinels unchanged.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::backend::IsolatedSurfaceBackend;
use crate::channels::ChannelRegistry;
use crate::error::{HarnessError, HarnessErrorCode, HarnessResult};
use crate::lifecycle::{
    GuestLifecycle, GuestLifecycleDisposition, GuestLifecyclePhase, ProofEvidenceClass,
};
use crate::sentinel::{HostSentinelRegistry, HostSentinelSnapshot, SyntheticHostProbe};
use crate::simulator::{GuestFrame, GuestLocalAction, InjectOutcome, SyntheticGuest};
use crate::store::HarnessSnapshot;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopEvidence {
    pub surface_id: String,
    pub channels_destroyed: usize,
    pub host_sentinels_unchanged: bool,
    pub host_sentinel_probe_error: Option<HarnessError>,
    pub disposition: Option<GuestLifecycleDisposition>,
}

pub struct IsolatedSurfaceHarness<B: IsolatedSurfaceBackend = SyntheticGuest> {
    lifecycle: GuestLifecycle,
    sentinels: HostSentinelRegistry,
    host_probe: SyntheticHostProbe,
    backend: B,
    channels: ChannelRegistry,
    snapshot_root: Option<std::path::PathBuf>,
    auto_retry_attempts: u32,
    last_channels_destroyed: usize,
    declared_evidence_class: ProofEvidenceClass,
}

impl IsolatedSurfaceHarness<SyntheticGuest> {
    pub fn new(baseline: HostSentinelSnapshot) -> Self {
        Self::with_backend(baseline, SyntheticGuest::new())
    }
}

impl<B: IsolatedSurfaceBackend> IsolatedSurfaceHarness<B> {
    pub fn with_backend(baseline: HostSentinelSnapshot, backend: B) -> Self {
        let surface_id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let declared_evidence_class = backend.evidence_class();
        let mut lifecycle = GuestLifecycle::new(surface_id, now);
        lifecycle.evidence_class = declared_evidence_class;
        Self {
            lifecycle,
            sentinels: HostSentinelRegistry::capture(baseline.clone()),
            host_probe: SyntheticHostProbe::new(baseline),
            backend,
            channels: ChannelRegistry::new(),
            snapshot_root: None,
            auto_retry_attempts: 0,
            last_channels_destroyed: 0,
            declared_evidence_class,
        }
    }

    pub fn with_snapshot_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.snapshot_root = Some(root.into());
        self
    }

    pub fn lifecycle(&self) -> &GuestLifecycle {
        &self.lifecycle
    }

    pub fn sentinels(&self) -> &HostSentinelRegistry {
        &self.sentinels
    }

    pub fn host_probe_mut(&mut self) -> &mut SyntheticHostProbe {
        &mut self.host_probe
    }

    pub fn channels(&self) -> &ChannelRegistry {
        &self.channels
    }

    pub fn evidence_class(&self) -> ProofEvidenceClass {
        self.declared_evidence_class
    }

    pub fn auto_retry_attempts(&self) -> u32 {
        self.auto_retry_attempts
    }

    pub fn guest_is_booted(&self) -> bool {
        self.backend.is_booted()
    }

    fn probe_host_sentinels(&mut self) -> HarnessResult<()> {
        self.sentinels.probe_and_verify(&self.host_probe)
    }

    /// launch → boot guest
    pub fn boot(&mut self) -> HarnessResult<()> {
        let now = Utc::now();
        self.probe_host_sentinels()?;
        self.lifecycle.begin_boot(now)?;
        self.channels.open_channel("frame")?;
        self.channels.open_channel("input")?;
        let frame = self.backend.boot()?;
        self.lifecycle.frame_epoch = frame.epoch;
        self.lifecycle
            .complete_boot(now + chrono::Duration::milliseconds(1))?;
        self.probe_host_sentinels()?;
        self.persist_snapshot()?;
        Ok(())
    }

    /// capture frame (read-only)
    pub fn observe_frame(&self) -> HarnessResult<GuestFrame> {
        if self.lifecycle.phase != GuestLifecyclePhase::Ready
            && self.lifecycle.phase != GuestLifecyclePhase::Acting
        {
            return Err(HarnessError::invalid_state(
                "frame observation requires Ready or Acting",
            ));
        }
        self.backend.observe_frame()
    }

    /// inject ONE guest-local action; changed frame on success
    pub fn inject_guest_action(
        &mut self,
        action: GuestLocalAction,
    ) -> HarnessResult<crate::simulator::FrameDelta> {
        if !self.lifecycle.allows_inject() {
            return Err(HarnessError::inject_fenced("inject is not allowed"));
        }
        let now = Utc::now();
        self.probe_host_sentinels()?;
        self.lifecycle.begin_act(now)?;
        self.persist_snapshot()?;

        let outcome = self.backend.inject_guest_local(action)?;
        match outcome {
            InjectOutcome::Changed(delta) => {
                self.lifecycle
                    .complete_act(Utc::now() + chrono::Duration::milliseconds(1))?;
                self.probe_host_sentinels()?;
                self.persist_snapshot()?;
                Ok(delta)
            }
            InjectOutcome::Uncertain => {
                self.lifecycle
                    .mark_uncertain(Utc::now() + chrono::Duration::milliseconds(1))?;
                self.persist_snapshot()?;
                Err(HarnessError::uncertain_outcome(
                    "guest inject outcome is uncertain",
                ))
            }
            InjectOutcome::Crash => {
                self.lifecycle
                    .mark_uncertain(Utc::now() + chrono::Duration::milliseconds(1))?;
                self.persist_snapshot()?;
                Err(HarnessError::new(
                    HarnessErrorCode::UncertainOutcome,
                    "guest inject crashed before completion",
                ))
            }
        }
    }

    /// Authoritative Stop: fence inject first, always teardown, probe separately.
    pub fn stop(&mut self) -> HarnessResult<StopEvidence> {
        let now = Utc::now();
        self.lifecycle.begin_stop(now)?;
        self.backend.stop_fence_first()?;
        self.persist_snapshot()?;

        self.teardown(now + chrono::Duration::milliseconds(1))?;

        let probe_error = self.probe_host_sentinels().err();
        let host_sentinels_unchanged = probe_error.is_none() && self.sentinels.verified_via_probe();

        self.channels.assert_all_destroyed()?;

        Ok(StopEvidence {
            surface_id: self.lifecycle.surface_id.clone(),
            channels_destroyed: self.last_channels_destroyed,
            host_sentinels_unchanged,
            host_sentinel_probe_error: probe_error,
            disposition: self.lifecycle.disposition,
        })
    }

    fn teardown(&mut self, now: DateTime<Utc>) -> HarnessResult<()> {
        self.last_channels_destroyed = self.channels.open_count();
        if self.last_channels_destroyed > 0 {
            self.channels.destroy_all()?;
        }
        if self.backend.is_booted() {
            self.backend.destroy()?;
        }
        if self.lifecycle.phase != GuestLifecyclePhase::Destroyed {
            self.lifecycle.complete_destroy(now)?;
        }
        self.persist_snapshot()?;
        Ok(())
    }

    /// Simulated process restart: reload durable snapshot, recover fail-closed, destroy.
    pub fn recover_after_restart(&mut self) -> HarnessResult<()> {
        let root = self
            .snapshot_root
            .as_ref()
            .ok_or_else(|| HarnessError::invalid_state("snapshot root is not configured"))?;
        let snapshot = HarnessSnapshot::load(root)?;
        self.lifecycle = snapshot.lifecycle;
        self.channels = snapshot.channels;
        self.auto_retry_attempts = snapshot.auto_retry_attempts;
        self.lifecycle.reconcile_invariants()?;
        let now = Utc::now();
        self.lifecycle.recover_after_restart(now)?;
        self.teardown(now + chrono::Duration::milliseconds(1))?;
        Ok(())
    }

    /// Explicit retry after uncertain — must be rejected (no auto-retry policy).
    pub fn retry_inject_after_uncertain(
        &mut self,
        action: GuestLocalAction,
    ) -> HarnessResult<crate::simulator::FrameDelta> {
        if self.lifecycle.disposition == Some(GuestLifecycleDisposition::Uncertain) {
            self.auto_retry_attempts = self.auto_retry_attempts.saturating_add(1);
            self.persist_snapshot()?;
            return Err(HarnessError::auto_retry_forbidden(
                "inject retry is forbidden after uncertain guest input",
            ));
        }
        self.inject_guest_action(action)
    }

    fn persist_snapshot(&mut self) -> HarnessResult<()> {
        if let Some(root) = &self.snapshot_root {
            let mut snapshot = HarnessSnapshot::new(
                self.lifecycle.clone(),
                self.sentinels.baseline().clone(),
                self.channels.clone(),
            );
            snapshot.auto_retry_attempts = self.auto_retry_attempts;
            snapshot.save(root)?;
        }
        Ok(())
    }

    /// Run the canonical proof sequence used by Sep 18 checklist mapping.
    pub fn run_canonical_proof(&mut self) -> HarnessResult<StopEvidence> {
        self.boot()?;
        let before = self.observe_frame()?;
        let delta = self.inject_guest_action(GuestLocalAction::ClickGuestButton)?;
        let after = self.observe_frame()?;
        if !delta.guest_local_change || before.digest == after.digest {
            return Err(HarnessError::invalid_state(
                "canonical proof requires a guest-local frame change",
            ));
        }
        self.stop()
    }
}

impl IsolatedSurfaceHarness<SyntheticGuest> {
    pub fn schedule_crash_on_next_inject(&mut self) {
        self.backend.schedule_crash_on_inject();
    }

    pub fn schedule_uncertain_on_next_inject(&mut self) {
        self.backend.schedule_uncertain_on_inject();
    }
}
