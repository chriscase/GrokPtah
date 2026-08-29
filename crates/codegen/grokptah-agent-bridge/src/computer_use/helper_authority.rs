//! Host-issued Computer Use helper lease, dispatch, crash-cut, and cleanup (#444).
//!
//! Every helper launch and action is bound to one Computer Run, target
//! incarnation, authority epoch, lease revision, observation generation, and
//! dispatch ID. Uncertain physical input is never replayed. Unknown
//! postconditions are never recorded as verified success.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::package_identity::{ComputerExecutorIdentity, ExecutorKind, SigningClass};
use super::platform::ComputerPermissionStatus;
use super::types::{
    validate_id, ComputerAction, ComputerError, ComputerErrorCode, ComputerResult, ComputerTarget,
    Sensitivity,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelperLease {
    pub lease_id: String,
    pub run_id: String,
    pub target: ComputerTarget,
    pub authority_epoch: u64,
    pub lease_revision: u64,
    pub observation_generation: u64,
    pub observation_id: String,
    pub grant_id: String,
    pub executor: ComputerExecutorIdentity,
}

impl HelperLease {
    pub fn validate(&self) -> ComputerResult<()> {
        validate_id("lease_id", &self.lease_id)?;
        validate_id("run_id", &self.run_id)?;
        validate_id("observation_id", &self.observation_id)?;
        validate_id("grant_id", &self.grant_id)?;
        self.target.validate()?;
        self.executor.validate()?;
        if self.observation_generation == 0 {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "helper lease requires a nonzero observation generation",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperCrashCut {
    BeforeInjection,
    AfterInjectionBeforeReceipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectDisposition {
    Verified,
    Denied,
    Uncertain,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupReceipt {
    pub helper_alive: bool,
    pub temp_artifacts: u32,
    pub lease_released: bool,
    pub frames_released: bool,
    pub replay_blocked: bool,
}

impl CleanupReceipt {
    pub fn exact() -> Self {
        Self {
            helper_alive: false,
            temp_artifacts: 0,
            lease_released: true,
            frames_released: true,
            replay_blocked: true,
        }
    }

    pub fn is_exact(&self) -> bool {
        !self.helper_alive
            && self.temp_artifacts == 0
            && self.lease_released
            && self.frames_released
            && self.replay_blocked
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectReceipt {
    pub dispatch_id: String,
    pub lease_id: String,
    pub run_id: String,
    pub authority_epoch: u64,
    pub lease_revision: u64,
    pub observation_generation: u64,
    pub disposition: EffectDisposition,
    pub injected: bool,
    pub postcondition: Option<bool>,
    pub error_code: Option<ComputerErrorCode>,
    pub foreground_app: String,
    pub pointer: (i32, i32),
    pub cleanup: CleanupReceipt,
}

impl EffectReceipt {
    pub fn leaks_secret(&self, secret: &str) -> bool {
        let encoded = serde_json::to_string(self).unwrap_or_default();
        !secret.is_empty() && encoded.contains(secret)
    }
}

#[derive(Debug, Clone)]
pub struct HelperWorld {
    pub screen_recording: ComputerPermissionStatus,
    pub accessibility: ComputerPermissionStatus,
    pub target: ComputerTarget,
    pub live_target: ComputerTarget,
    pub element_sensitivity: Sensitivity,
    pub takeover: bool,
    pub crash_cut: Option<HelperCrashCut>,
    pub postcondition: Option<bool>,
    pub foreground_app: String,
    pub pointer: (i32, i32),
    pub helper_alive: bool,
    pub temp_artifacts: u32,
    pub injection_count: u32,
}

impl HelperWorld {
    pub fn granted_demo() -> Self {
        let target = ComputerTarget {
            app_id: super::package_identity::DEMO_TARGET_BUNDLE_ID.into(),
            window_id: "macos-window-fixture-1".into(),
            generation: 1,
            display_name: "Computer Use Demo".into(),
            sensitivity: Sensitivity::None,
        };
        Self {
            screen_recording: ComputerPermissionStatus::Granted,
            accessibility: ComputerPermissionStatus::Granted,
            live_target: target.clone(),
            target,
            element_sensitivity: Sensitivity::None,
            takeover: false,
            crash_cut: None,
            postcondition: Some(true),
            foreground_app: "com.apple.TextEdit".into(),
            pointer: (320, 240),
            helper_alive: false,
            temp_artifacts: 0,
            injection_count: 0,
        }
    }
}

#[derive(Debug)]
pub struct HelperSupervisor {
    inner: Mutex<HelperSupervisorInner>,
}

#[derive(Debug)]
struct HelperSupervisorInner {
    world: HelperWorld,
    session: Option<HelperLease>,
    used: HashMap<String, EffectReceipt>,
    recoveries: u32,
}

impl HelperSupervisor {
    pub fn new(world: HelperWorld) -> Self {
        Self {
            inner: Mutex::new(HelperSupervisorInner {
                world,
                session: None,
                used: HashMap::new(),
                recoveries: 0,
            }),
        }
    }

    pub fn world(&self) -> HelperWorld {
        self.inner.lock().expect("helper supervisor").world.clone()
    }

    pub fn launch(
        &self,
        run_id: &str,
        grant_id: &str,
        observation_id: &str,
        observation_generation: u64,
        authority_epoch: u64,
        executor: ComputerExecutorIdentity,
    ) -> ComputerResult<HelperLease> {
        let mut inner = self.inner.lock().expect("helper supervisor");
        executor.validate()?;
        validate_id("run_id", run_id)?;
        validate_id("grant_id", grant_id)?;
        validate_id("observation_id", observation_id)?;
        if executor.kind == ExecutorKind::PackagedHelper
            && executor.signing_class == SigningClass::AdHoc
        {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "ad-hoc helper identity cannot launch a packaged Computer Use helper",
            ));
        }
        inner.session = None;
        inner.used.clear();
        inner.world.helper_alive = true;
        inner.world.temp_artifacts = 1;
        let lease = HelperLease {
            lease_id: Uuid::new_v4().to_string(),
            run_id: run_id.to_string(),
            target: inner.world.target.clone(),
            authority_epoch,
            lease_revision: 1,
            observation_generation,
            observation_id: observation_id.to_string(),
            grant_id: grant_id.to_string(),
            executor,
        };
        lease.validate()?;
        inner.session = Some(lease.clone());
        Ok(lease)
    }

    pub fn dispatch(
        &self,
        dispatch_id: &str,
        lease: &HelperLease,
        action: &ComputerAction,
    ) -> ComputerResult<EffectReceipt> {
        validate_id("dispatch_id", dispatch_id)?;
        lease.validate()?;
        action.validate(&super::types::ComputerUseLimits::default())?;
        let mut inner = self.inner.lock().expect("helper supervisor");
        if let Some(existing) = inner.used.get(dispatch_id) {
            return Ok(existing.clone());
        }
        let current = inner.session.clone().ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::Interrupted,
                "computer-use helper is not launched",
            )
        })?;
        if current.lease_id != lease.lease_id
            || current.run_id != lease.run_id
            || current.authority_epoch != lease.authority_epoch
            || current.lease_revision != lease.lease_revision
            || current.observation_generation != lease.observation_generation
            || current.observation_id != lease.observation_id
            || current.target != lease.target
        {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "helper dispatch is not bound to the current lease incarnation",
            ));
        }

        let mut receipt = EffectReceipt {
            dispatch_id: dispatch_id.to_string(),
            lease_id: lease.lease_id.clone(),
            run_id: lease.run_id.clone(),
            authority_epoch: lease.authority_epoch,
            lease_revision: lease.lease_revision,
            observation_generation: lease.observation_generation,
            disposition: EffectDisposition::Denied,
            injected: false,
            postcondition: None,
            error_code: None,
            foreground_app: inner.world.foreground_app.clone(),
            pointer: inner.world.pointer,
            cleanup: CleanupReceipt::exact(),
        };

        if matches!(
            action,
            ComputerAction::KeyChord { .. } | ComputerAction::PointerClick { .. }
        ) {
            receipt.error_code = Some(ComputerErrorCode::ForbiddenAction);
            return Ok(inner.finish_dispatch(dispatch_id, receipt));
        }
        if inner.world.element_sensitivity.is_hard_denied() {
            receipt.error_code = Some(ComputerErrorCode::SensitiveSurface);
            return Ok(inner.finish_dispatch(dispatch_id, receipt));
        }
        if inner.world.live_target != lease.target {
            receipt.error_code = Some(ComputerErrorCode::TargetChanged);
            return Ok(inner.finish_dispatch(dispatch_id, receipt));
        }
        if inner.world.takeover {
            receipt.disposition = EffectDisposition::Cancelled;
            receipt.error_code = Some(ComputerErrorCode::Interrupted);
            return Ok(inner.finish_dispatch(dispatch_id, receipt));
        }
        if let Some(code) =
            permission_error(inner.world.screen_recording, inner.world.accessibility)
        {
            receipt.error_code = Some(code);
            return Ok(inner.finish_dispatch(dispatch_id, receipt));
        }
        if inner.world.crash_cut == Some(HelperCrashCut::BeforeInjection) {
            inner.world.helper_alive = false;
            receipt.disposition = EffectDisposition::Failed;
            receipt.error_code = Some(ComputerErrorCode::Interrupted);
            return Ok(inner.finish_dispatch(dispatch_id, receipt));
        }

        inner.world.injection_count = inner.world.injection_count.saturating_add(1);
        receipt.injected = true;

        if inner.world.crash_cut == Some(HelperCrashCut::AfterInjectionBeforeReceipt) {
            inner.world.helper_alive = false;
            receipt.disposition = EffectDisposition::Uncertain;
            receipt.error_code = Some(ComputerErrorCode::UncertainOutcome);
            return Ok(inner.finish_dispatch(dispatch_id, receipt));
        }

        match inner.world.postcondition {
            Some(true) => {
                receipt.disposition = EffectDisposition::Verified;
                receipt.postcondition = Some(true);
            }
            Some(false) => {
                receipt.disposition = EffectDisposition::Failed;
                receipt.postcondition = Some(false);
                receipt.error_code = Some(ComputerErrorCode::BackendFailure);
            }
            None => {
                receipt.disposition = EffectDisposition::Uncertain;
                receipt.postcondition = None;
                receipt.error_code = Some(ComputerErrorCode::UncertainOutcome);
            }
        }
        Ok(inner.finish_dispatch(dispatch_id, receipt))
    }

    pub fn cancel(&self) -> CleanupReceipt {
        let mut inner = self.inner.lock().expect("helper supervisor");
        inner.world.takeover = true;
        inner.cleanup(true)
    }

    pub fn recover(&self) -> CleanupReceipt {
        let mut inner = self.inner.lock().expect("helper supervisor");
        inner.recoveries = inner.recoveries.saturating_add(1);
        inner.world.helper_alive = false;
        inner.session = None;
        inner.cleanup(true)
    }

    pub fn recoveries(&self) -> u32 {
        self.inner.lock().expect("helper supervisor").recoveries
    }

    pub fn injection_count(&self) -> u32 {
        self.inner
            .lock()
            .expect("helper supervisor")
            .world
            .injection_count
    }
}

impl HelperSupervisorInner {
    fn finish_dispatch(&mut self, dispatch_id: &str, mut receipt: EffectReceipt) -> EffectReceipt {
        receipt.cleanup = self.cleanup(true);
        self.used.insert(dispatch_id.to_string(), receipt.clone());
        receipt
    }

    fn cleanup(&mut self, release_lease: bool) -> CleanupReceipt {
        self.world.helper_alive = false;
        self.world.temp_artifacts = 0;
        if release_lease {
            self.session = None;
        }
        CleanupReceipt {
            helper_alive: self.world.helper_alive,
            temp_artifacts: self.world.temp_artifacts,
            lease_released: self.session.is_none(),
            frames_released: true,
            replay_blocked: true,
        }
    }
}

fn permission_error(
    screen_recording: ComputerPermissionStatus,
    accessibility: ComputerPermissionStatus,
) -> Option<ComputerErrorCode> {
    for status in [screen_recording, accessibility] {
        match status {
            ComputerPermissionStatus::Granted => {}
            ComputerPermissionStatus::Missing | ComputerPermissionStatus::PromptPending => {
                return Some(ComputerErrorCode::PermissionRequired);
            }
            ComputerPermissionStatus::Denied | ComputerPermissionStatus::Restricted => {
                return Some(ComputerErrorCode::PermissionDenied);
            }
            ComputerPermissionStatus::Revoked => {
                return Some(ComputerErrorCode::PermissionRevoked);
            }
            ComputerPermissionStatus::Unsupported => {
                return Some(ComputerErrorCode::UnsupportedPlatform);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease_and_supervisor(world: HelperWorld) -> (HelperSupervisor, HelperLease) {
        let supervisor = HelperSupervisor::new(world);
        let lease = supervisor
            .launch(
                "run-fixture-1",
                "grant-fixture-1",
                "observation-fixture-1",
                1,
                1,
                ComputerExecutorIdentity::packaged_helper(SigningClass::NotarizedDeveloperId),
            )
            .unwrap();
        (supervisor, lease)
    }

    fn set_value() -> ComputerAction {
        ComputerAction::SetValue {
            element_id: "project-label".into(),
            text: "public-demo-value".into(),
        }
    }

    #[test]
    fn duplicate_dispatch_id_injects_once() {
        let (supervisor, lease) = lease_and_supervisor(HelperWorld::granted_demo());
        let first = supervisor
            .dispatch("dispatch-1", &lease, &set_value())
            .unwrap();
        let second = supervisor
            .dispatch("dispatch-1", &lease, &set_value())
            .unwrap();
        assert_eq!(first.disposition, EffectDisposition::Verified);
        assert_eq!(first, second);
        assert_eq!(supervisor.injection_count(), 1);
        assert!(first.cleanup.is_exact());
    }

    #[test]
    fn crash_before_injection_does_not_inject() {
        let mut world = HelperWorld::granted_demo();
        world.crash_cut = Some(HelperCrashCut::BeforeInjection);
        let (supervisor, lease) = lease_and_supervisor(world);
        let receipt = supervisor
            .dispatch("dispatch-crash-before", &lease, &set_value())
            .unwrap();
        assert!(!receipt.injected);
        assert_eq!(receipt.disposition, EffectDisposition::Failed);
        assert_eq!(receipt.error_code, Some(ComputerErrorCode::Interrupted));
        assert_eq!(supervisor.injection_count(), 0);
        assert!(receipt.cleanup.is_exact());
    }

    #[test]
    fn crash_after_injection_is_uncertain_and_not_replayed() {
        let mut world = HelperWorld::granted_demo();
        world.crash_cut = Some(HelperCrashCut::AfterInjectionBeforeReceipt);
        let (supervisor, lease) = lease_and_supervisor(world);
        let first = supervisor
            .dispatch("dispatch-crash-after", &lease, &set_value())
            .unwrap();
        assert!(first.injected);
        assert_eq!(first.disposition, EffectDisposition::Uncertain);
        assert_eq!(first.error_code, Some(ComputerErrorCode::UncertainOutcome));
        let replay = supervisor
            .dispatch("dispatch-crash-after", &lease, &set_value())
            .unwrap();
        assert_eq!(first, replay);
        assert_eq!(supervisor.injection_count(), 1);
    }

    #[test]
    fn unknown_postcondition_is_not_verified_success() {
        let mut world = HelperWorld::granted_demo();
        world.postcondition = None;
        let (supervisor, lease) = lease_and_supervisor(world);
        let receipt = supervisor
            .dispatch("dispatch-uncertain", &lease, &set_value())
            .unwrap();
        assert!(receipt.injected);
        assert_eq!(receipt.disposition, EffectDisposition::Uncertain);
        assert_ne!(receipt.disposition, EffectDisposition::Verified);
    }

    #[test]
    fn pointer_fallback_never_injects() {
        let (supervisor, lease) = lease_and_supervisor(HelperWorld::granted_demo());
        let receipt = supervisor
            .dispatch(
                "dispatch-pointer",
                &lease,
                &ComputerAction::PointerClick {
                    x: 10.0,
                    y: 10.0,
                    button: crate::computer_use::types::PointerButton::Primary,
                },
            )
            .unwrap();
        assert!(!receipt.injected);
        assert_eq!(receipt.error_code, Some(ComputerErrorCode::ForbiddenAction));
        assert_eq!(supervisor.injection_count(), 0);
    }

    #[test]
    fn two_recoveries_leave_helper_dead_and_block_replay() {
        let (supervisor, lease) = lease_and_supervisor(HelperWorld::granted_demo());
        let first = supervisor.recover();
        let second = supervisor.recover();
        assert_eq!(supervisor.recoveries(), 2);
        assert!(first.is_exact());
        assert!(second.is_exact());
        let error = supervisor
            .dispatch("dispatch-after-restart", &lease, &set_value())
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::Interrupted);
        assert_eq!(supervisor.injection_count(), 0);
    }

    #[test]
    fn receipts_do_not_embed_secure_values() {
        let mut world = HelperWorld::granted_demo();
        world.element_sensitivity = Sensitivity::Secure;
        let (supervisor, lease) = lease_and_supervisor(world);
        let receipt = supervisor
            .dispatch(
                "dispatch-secure",
                &lease,
                &ComputerAction::SetValue {
                    element_id: "demo-password".into(),
                    text: "fixture-secret-never-export".into(),
                },
            )
            .unwrap();
        assert!(!receipt.injected);
        assert_eq!(
            receipt.error_code,
            Some(ComputerErrorCode::SensitiveSurface)
        );
        assert!(!receipt.leaks_secret("fixture-secret-never-export"));
        assert_eq!(receipt.foreground_app, "com.apple.TextEdit");
        assert_eq!(receipt.pointer, (320, 240));
    }
}
