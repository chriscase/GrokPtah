//! Isolated Surface Proof Harness orchestrator.
//!
//! Models the Sep 18 physical proof checklist in synthetic form:
//! launch → boot guest → frame → inject ONE guest-local action → changed frame
//! → Stop → destroy channels → cleanup, with host sentinels unchanged.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::backend::{honest_harness_evidence_class, IsolatedSurfaceBackend, VfLaunchReceipt};
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
    pub host_sentinel_probes_performed: u32,
    pub host_sentinel_probe_error: Option<HarnessError>,
    pub backend_fence_error: Option<HarnessError>,
    pub persist_snapshot_error: Option<HarnessError>,
    pub backend_destroy_error: Option<HarnessError>,
    pub disposition: Option<GuestLifecycleDisposition>,
}

impl StopEvidence {
    /// True only when backend destroy succeeded (or was not required) and lifecycle
    /// reached `Destroyed`. Failed destroy must not stamp `StopDestroyed`.
    pub fn destroy_confirmed(&self, lifecycle_phase: GuestLifecyclePhase) -> bool {
        self.backend_destroy_error.is_none() && lifecycle_phase == GuestLifecyclePhase::Destroyed
    }
}

/// Teardown outcome collected during Stop — errors are surfaced separately and
/// never skip the destroy attempt.
struct TeardownOutcome {
    backend_destroy_error: Option<HarnessError>,
    persist_snapshot_error: Option<HarnessError>,
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
            .expect("synthetic backend is always permitted")
    }
}

impl<B: IsolatedSurfaceBackend> IsolatedSurfaceHarness<B> {
    /// Construct a harness with an honest evidence class. Rejects
    /// `VirtualizationFramework` unless attached via [`with_vf_backend`].
    pub fn with_backend(baseline: HostSentinelSnapshot, backend: B) -> HarnessResult<Self> {
        let declared_evidence_class = honest_harness_evidence_class(backend.evidence_class())?;
        Ok(Self::with_declared_class(
            baseline,
            backend,
            declared_evidence_class,
        ))
    }

    /// Physical Mac VF proof only — requires a launch receipt; not used until Sep 18 gate.
    pub fn with_vf_backend(
        baseline: HostSentinelSnapshot,
        backend: B,
        receipt: VfLaunchReceipt,
    ) -> HarnessResult<Self> {
        if backend.evidence_class() != ProofEvidenceClass::VirtualizationFramework {
            return Err(HarnessError::invalid_state(
                "with_vf_backend requires a VirtualizationFramework backend",
            ));
        }
        if receipt.physical_mac_proof_id.trim().is_empty() {
            return Err(HarnessError::invalid_state(
                "VF launch receipt requires a non-empty physical_mac_proof_id",
            ));
        }
        Ok(Self::with_declared_class(
            baseline,
            backend,
            ProofEvidenceClass::VirtualizationFramework,
        ))
    }

    fn with_declared_class(
        baseline: HostSentinelSnapshot,
        backend: B,
        declared_evidence_class: ProofEvidenceClass,
    ) -> Self {
        let surface_id = Uuid::new_v4().to_string();
        let now = Utc::now();
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

    /// Mac physical proof hook: compare an externally collected host snapshot to the
    /// harness baseline via [`HostSentinelRegistry::refresh_from_host`].
    pub fn refresh_host_sentinels(&mut self, snapshot: HostSentinelSnapshot) -> HarnessResult<()> {
        self.sentinels.refresh_from_host(snapshot)
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

        let outcome = match self.backend.inject_guest_local(action) {
            Ok(outcome) => outcome,
            Err(err) => {
                if self.lifecycle.guest_input_possible {
                    self.lifecycle
                        .mark_uncertain(Utc::now() + chrono::Duration::milliseconds(1))?;
                    self.persist_snapshot()?;
                }
                return Err(err);
            }
        };
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
        let backend_fence_error = self.backend.stop_fence_first().err();
        let persist_snapshot_error = self.persist_snapshot().err();

        let teardown = self.teardown(now + chrono::Duration::milliseconds(1));
        let persist_snapshot_error = persist_snapshot_error.or(teardown.persist_snapshot_error);

        let probe_error = self.probe_host_sentinels().err();
        let host_sentinels_unchanged = probe_error.is_none() && self.sentinels.verified_via_probe();

        self.channels.assert_all_destroyed()?;

        Ok(StopEvidence {
            surface_id: self.lifecycle.surface_id.clone(),
            channels_destroyed: self.last_channels_destroyed,
            host_sentinels_unchanged,
            host_sentinel_probes_performed: self.sentinels.probes_performed(),
            host_sentinel_probe_error: probe_error,
            backend_fence_error,
            persist_snapshot_error,
            backend_destroy_error: teardown.backend_destroy_error,
            disposition: self.lifecycle.disposition,
        })
    }

    /// Always destroy channels and guest; fallible steps before backend destroy
    /// are collected and never skip the destroy attempt. Lifecycle advances to
    /// `Destroyed` only when backend destroy is confirmed (or not required).
    fn teardown(&mut self, now: DateTime<Utc>) -> TeardownOutcome {
        self.last_channels_destroyed = self.channels.open_count();
        if self.last_channels_destroyed > 0 {
            // Channel teardown must not prevent backend destroy.
            let _ = self.channels.destroy_all();
        }
        let backend_destroy_error = if self.backend.is_booted() {
            self.backend.destroy().err()
        } else {
            None
        };
        if backend_destroy_error.is_none() && self.lifecycle.phase != GuestLifecyclePhase::Destroyed
        {
            let _ = self.lifecycle.complete_destroy(now);
        }
        let persist_snapshot_error = self.persist_snapshot().err();
        TeardownOutcome {
            backend_destroy_error,
            persist_snapshot_error,
        }
    }

    /// Simulated process restart: reload durable snapshot, recover fail-closed, destroy.
    pub fn recover_after_restart(&mut self) -> HarnessResult<StopEvidence> {
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
        let teardown = self.teardown(now + chrono::Duration::milliseconds(1));
        self.channels.assert_all_destroyed()?;
        Ok(StopEvidence {
            surface_id: self.lifecycle.surface_id.clone(),
            channels_destroyed: self.last_channels_destroyed,
            host_sentinels_unchanged: false,
            host_sentinel_probes_performed: self.sentinels.probes_performed(),
            host_sentinel_probe_error: None,
            backend_fence_error: None,
            persist_snapshot_error: teardown.persist_snapshot_error,
            backend_destroy_error: teardown.backend_destroy_error,
            disposition: self.lifecycle.disposition,
        })
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

impl IsolatedSurfaceHarness<crate::contained_browser::ContainedBrowserBackend> {
    pub fn schedule_crash_on_next_inject(&mut self) {
        self.backend.schedule_crash_on_next_inject();
    }

    pub fn schedule_uncertain_on_next_inject(&mut self) {
        self.backend.schedule_uncertain_on_next_inject();
    }
}
