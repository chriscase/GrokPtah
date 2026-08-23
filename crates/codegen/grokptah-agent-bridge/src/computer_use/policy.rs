use chrono::{DateTime, Utc};

use super::types::{
    ActionGrant, ComputerAction, ComputerCapabilityProof, ComputerCapabilityTier, ComputerError,
    ComputerErrorCode, ComputerObservation, ComputerPrincipal, ComputerResult, ComputerRun,
    ComputerRunState, ComputerSurfaceBinding, SemanticAction, SurfaceFreshnessFence,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct ComputerPolicy;

impl ComputerPolicy {
    pub fn run_limit_reached(&self, run: &ComputerRun, now: DateTime<Utc>) -> bool {
        run.action_count >= run.limits.max_actions || run.duration_exceeded(now)
    }

    pub fn authorize_caller(
        &self,
        run: &ComputerRun,
        caller: &ComputerPrincipal,
    ) -> ComputerResult<()> {
        caller.validate()?;
        let expected = run.required_principal()?;
        if caller != expected {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "caller principal does not match the initiating principal",
            ));
        }
        Ok(())
    }

    pub fn authorize_surface(
        &self,
        run: &ComputerRun,
        surface: &ComputerSurfaceBinding,
    ) -> ComputerResult<()> {
        if !run.surface.is_issued() || !surface.is_issued() {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "surface identity is missing or unissued",
            ));
        }
        if &run.surface != surface {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenTarget,
                "surface id or incarnation does not match the computer run",
            ));
        }
        Ok(())
    }

    pub fn authorize_live_freshness(
        &self,
        observation: &ComputerObservation,
        live: &SurfaceFreshnessFence,
    ) -> ComputerResult<()> {
        observation.authority.freshness.validate_shape()?;
        live.validate_shape()?;
        if observation.authority.freshness.surface_id != live.surface_id
            || observation.authority.freshness.incarnation != live.incarnation
        {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "freshness fence is not bound to the live surface incarnation",
            ));
        }
        if observation.authority.freshness.tick != live.tick {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "monotonic freshness tick is not the host-minted current surface tick",
            ));
        }
        Ok(())
    }

    pub fn authorize_grant(
        &self,
        run: &ComputerRun,
        grant: &ActionGrant,
        now: DateTime<Utc>,
        caller: &ComputerPrincipal,
    ) -> ComputerResult<()> {
        self.authorize_caller(run, caller)?;
        grant.validate()?;
        if run.target.sensitivity.is_hard_denied() {
            return Err(ComputerError::new(
                ComputerErrorCode::SensitiveSurface,
                "the selected target is hard denied",
            ));
        }
        if run.state != ComputerRunState::AwaitingAuthorization
            && run.state != ComputerRunState::Paused
        {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "computer run is not awaiting authorization",
            ));
        }
        if grant.run_id != run.run_id || grant.target != run.target {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenTarget,
                "grant does not match the computer run and target",
            ));
        }
        if grant.target.generation != run.target.generation {
            return Err(ComputerError::new(
                ComputerErrorCode::TargetChanged,
                "grant target generation does not match the computer run",
            ));
        }
        self.authorize_surface(run, &grant.surface)?;
        if grant.authority_epoch != run.authority_epoch {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "grant authority epoch does not match the computer run",
            ));
        }
        if grant.principal.as_ref() != Some(caller) {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "grant principal does not match the caller",
            ));
        }
        let grant_tier = grant.effective_tier();
        let run_tier = run.capability_proof.tier();
        if matches!(run_tier, ComputerCapabilityTier::Unproven)
            || matches!(grant_tier, ComputerCapabilityTier::Unproven)
        {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "unproven capability cannot issue a grant",
            ));
        }
        if grant_tier != run_tier {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "grant capability tier does not match the run proof",
            ));
        }
        if grant_tier.allows_isolated_input()
            && !run.capability_proof.isolated_input_is_dispatchable()
        {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "grant requests isolated input the run proof cannot dispatch",
            ));
        }
        if matches!(
            grant_tier,
            ComputerCapabilityTier::MeasuredBackgroundSafeSemantic
        ) && !matches!(
            run_tier,
            ComputerCapabilityTier::MeasuredBackgroundSafeSemantic
                | ComputerCapabilityTier::IndependentlyIsolatedVisualInputDomain
        ) {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "grant requests background-safe authority the run proof does not have",
            ));
        }
        if grant.revoked_at.is_some() || now < grant.issued_at || now >= grant.expires_at {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "grant is not currently valid",
            ));
        }
        if grant
            .uses_remaining
            .is_some_and(|uses| uses > run.limits.max_actions)
        {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "grant use count exceeds the run action bound",
            ));
        }
        let lifetime = grant.expires_at.signed_duration_since(grant.issued_at);
        if lifetime.num_seconds() > run.limits.max_duration_secs as i64 {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "grant lifetime exceeds the run duration bound",
            ));
        }
        Ok(())
    }

    pub fn authorize_observation_exposure(
        &self,
        observation: &ComputerObservation,
    ) -> ComputerResult<()> {
        if observation
            .screenshot
            .as_ref()
            .is_some_and(|evidence| !evidence.redacted)
        {
            return Err(ComputerError::new(
                ComputerErrorCode::SensitiveSurface,
                "screenshot evidence has not passed privacy redaction",
            ));
        }
        if observation.target.sensitivity.is_hard_denied()
            || observation.sensitivity.is_hard_denied()
            || observation
                .elements
                .iter()
                .any(|element| element.sensitivity.is_hard_denied())
        {
            return Err(ComputerError::new(
                ComputerErrorCode::SensitiveSurface,
                "the observation contains a hard-denied surface",
            ));
        }
        Ok(())
    }

    pub fn authorize_observation(
        &self,
        run: &ComputerRun,
        now: DateTime<Utc>,
        caller: &ComputerPrincipal,
    ) -> ComputerResult<()> {
        self.authorize_caller(run, caller)?;
        self.authorize_proven_observe(run)?;
        self.authorize_active_run(run, now)?;
        Ok(())
    }

    pub fn authorize_evidence(
        &self,
        run: &ComputerRun,
        caller: &ComputerPrincipal,
    ) -> ComputerResult<()> {
        self.authorize_caller(run, caller)?;
        self.authorize_proven_observe(run)?;
        if run.current_observation.is_none() {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "evidence is not attached to a proven current observation",
            ));
        }
        Ok(())
    }

    pub fn authorize_action(
        &self,
        run: &ComputerRun,
        observation: &ComputerObservation,
        action: &ComputerAction,
        now: DateTime<Utc>,
        caller: &ComputerPrincipal,
        live_fence: &SurfaceFreshnessFence,
    ) -> ComputerResult<()> {
        self.authorize_caller(run, caller)?;
        self.authorize_active_run(run, now)?;
        action.validate(&run.limits)?;
        observation.validate(&run.limits)?;
        self.authorize_surface(run, &observation.authority.surface)?;
        self.authorize_live_freshness(observation, live_fence)?;
        if observation.authority.target_generation != run.target.generation
            || observation.target.generation != run.target.generation
        {
            return Err(ComputerError::new(
                ComputerErrorCode::TargetChanged,
                "observation target generation does not match the run",
            ));
        }
        if observation.authority.authority_epoch != run.authority_epoch {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "observation authority epoch does not match the run",
            ));
        }
        if observation.authority.control_epoch != run.control_epoch {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "observation control epoch does not match the run",
            ));
        }
        if observation.authority.frame_epoch == 0 {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "observation is missing a host-issued frame epoch",
            ));
        }
        self.authorize_capability(run, action)?;

        let grant = run.grant.as_ref().ok_or_else(|| {
            ComputerError::new(ComputerErrorCode::Unauthorized, "computer run has no grant")
        })?;
        if grant.run_id != run.run_id || grant.target != run.target {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenTarget,
                "grant target changed",
            ));
        }
        self.authorize_surface(run, &grant.surface)?;
        if grant.authority_epoch != run.authority_epoch {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "grant authority epoch does not match the run",
            ));
        }
        if grant.principal.as_ref() != Some(caller) {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "grant principal does not match the caller",
            ));
        }
        if grant.revoked_at.is_some() || now >= grant.expires_at {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "computer-use grant expired or was revoked",
            ));
        }
        if grant.uses_remaining == Some(0) {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "computer-use grant has no remaining actions",
            ));
        }
        if !grant.action_classes.contains(&action.class()) {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "action class is outside the grant",
            ));
        }
        if action.is_activate_target() && !grant.effective_tier().allows_activate_target() {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "ActivateTarget is valid only for authorized foreground-semantic execution",
            ));
        }
        if action.requires_isolated_input() && !grant.effective_tier().allows_isolated_input() {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "guest pointer, key, and text input require an independently isolated input-domain proof",
            ));
        }
        if observation.target != run.target {
            return Err(ComputerError::new(
                ComputerErrorCode::TargetChanged,
                "observation target does not match the run",
            ));
        }
        let Some(current) = &run.current_observation else {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "computer run has no current observation",
            ));
        };
        if current.observation_id != observation.observation_id
            || current.sequence != observation.sequence
            || current.authority.frame_epoch != observation.authority.frame_epoch
            || current.authority.surface != observation.authority.surface
        {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "action is not bound to the current observation",
            ));
        }
        let age = now.signed_duration_since(observation.captured_at);
        if age.num_milliseconds() < 0
            || age.num_milliseconds() > run.limits.max_observation_age_millis as i64
        {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "observation is too old or from the future",
            ));
        }
        if observation.sensitivity.is_hard_denied() {
            return Err(ComputerError::new(
                ComputerErrorCode::SensitiveSurface,
                "the observed surface is hard denied",
            ));
        }

        if let Some(element_id) = action.referenced_element() {
            let element = observation.element(element_id).ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::StaleObservation,
                    "action references an unknown element",
                )
            })?;
            if element.sensitivity.is_hard_denied() {
                return Err(ComputerError::new(
                    ComputerErrorCode::SensitiveSurface,
                    "the target element is hard denied",
                ));
            }
            if !element.enabled {
                return Err(ComputerError::new(
                    ComputerErrorCode::ForbiddenAction,
                    "the target element is disabled",
                ));
            }
            if let Some(required) = required_semantic_action(action) {
                if !element.actions.contains(&required) {
                    return Err(ComputerError::new(
                        ComputerErrorCode::ForbiddenAction,
                        "the target element does not advertise this action",
                    ));
                }
            }
        }

        if let ComputerAction::PointerClick { x, y, .. }
        | ComputerAction::PointerMove { x, y }
        | ComputerAction::PointerButton { x, y, .. } = action
        {
            if *x < 0.0
                || *y < 0.0
                || *x >= observation.geometry.width
                || *y >= observation.geometry.height
            {
                return Err(ComputerError::new(
                    ComputerErrorCode::ForbiddenAction,
                    "pointer point is outside the authorized target",
                ));
            }
        }
        Ok(())
    }

    fn authorize_proven_observe(&self, run: &ComputerRun) -> ComputerResult<()> {
        run.capability_proof.validate()?;
        if matches!(
            run.capability_proof.tier(),
            ComputerCapabilityTier::Unproven
        ) || !run.capability_proof.observe()
        {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "unproven or unattested capability cannot observe or read evidence",
            ));
        }
        Ok(())
    }

    fn authorize_capability(
        &self,
        run: &ComputerRun,
        action: &ComputerAction,
    ) -> ComputerResult<()> {
        run.capability_proof.validate()?;
        if matches!(
            run.capability_proof.tier(),
            ComputerCapabilityTier::Unproven
        ) {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "unproven capability cannot dispatch computer actions",
            ));
        }
        if action.is_activate_target() {
            if !run.capability_proof.tier().allows_activate_target() {
                return Err(ComputerError::new(
                    ComputerErrorCode::ForbiddenAction,
                    "ActivateTarget is valid only for authorized foreground-semantic execution",
                ));
            }
            if matches!(
                run.capability_proof,
                ComputerCapabilityProof::MeasuredBackgroundSafeSemantic { .. }
            ) {
                return Err(ComputerError::new(
                    ComputerErrorCode::ForbiddenAction,
                    "background-safe semantic actions cannot activate a target",
                ));
            }
            return Ok(());
        }
        if action.requires_isolated_input() {
            if !run.capability_proof.isolated_input_is_dispatchable() {
                return Err(ComputerError::new(
                    ComputerErrorCode::ForbiddenAction,
                    "guest pointer, key, and text input require an independently isolated input-domain proof",
                ));
            }
            let allowed = match action {
                ComputerAction::KeyChord { .. } => run.capability_proof.key_chords(),
                ComputerAction::PointerClick { .. }
                | ComputerAction::PointerMove { .. }
                | ComputerAction::PointerButton { .. } => run.capability_proof.pointer_fallback(),
                ComputerAction::TextInput { .. } => run.capability_proof.text_entry(),
                _ => false,
            };
            if !allowed {
                return Err(ComputerError::new(
                    ComputerErrorCode::ForbiddenAction,
                    "isolated proof does not include this input class",
                ));
            }
            return Ok(());
        }
        if !run.capability_proof.tier().allows_background_semantic() {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "run capability proof does not authorize semantic execution",
            ));
        }
        Ok(())
    }

    fn authorize_active_run(&self, run: &ComputerRun, now: DateTime<Utc>) -> ComputerResult<()> {
        if run.state != ComputerRunState::Ready {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "computer run is not ready",
            ));
        }
        if self.run_limit_reached(run, now) {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "computer-use action or duration limit reached",
            ));
        }
        let grant = run.grant.as_ref().ok_or_else(|| {
            ComputerError::new(ComputerErrorCode::Unauthorized, "computer run has no grant")
        })?;
        if grant.revoked_at.is_some() || now >= grant.expires_at {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "computer-use grant expired or was revoked",
            ));
        }
        Ok(())
    }
}

fn required_semantic_action(action: &ComputerAction) -> Option<SemanticAction> {
    match action {
        ComputerAction::Invoke { .. } => Some(SemanticAction::Invoke),
        ComputerAction::SetValue { .. } => Some(SemanticAction::SetValue),
        ComputerAction::Select { .. } => Some(SemanticAction::Select),
        ComputerAction::Scroll {
            element_id: Some(_),
            ..
        } => Some(SemanticAction::Scroll),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::Duration;
    use uuid::Uuid;

    use super::*;
    use crate::computer_use::{
        ActionClass, ComputerTarget, ComputerUseLimits, EvidenceRef, ObservationAuthority,
        ObservationGeometry, SemanticElement, Sensitivity,
    };

    fn ready_run() -> ComputerRun {
        let target = ComputerTarget {
            app_id: "com.grokptah.demo".into(),
            window_id: "main".into(),
            generation: 1,
            display_name: "Demo".into(),
            sensitivity: Sensitivity::None,
        };
        let mut run = ComputerRun::attested_foreground_for_test(
            Uuid::new_v4(),
            None,
            target.clone(),
            Default::default(),
        )
        .unwrap();
        let now = Utc::now();
        run.grant = Some(ActionGrant::for_run(
            &run,
            BTreeSet::from([ActionClass::Semantic, ActionClass::TextEntry]),
            now - Duration::seconds(1),
            now + Duration::minutes(5),
            None,
        ));
        run.transition(ComputerRunState::Ready).unwrap();
        let mut observation = ComputerObservation {
            observation_id: "obs-1".into(),
            sequence: 1,
            target,
            captured_at: now,
            geometry: ObservationGeometry {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
                scale_factor: 2.0,
            },
            screenshot: None,
            elements: vec![SemanticElement {
                element_id: "field".into(),
                role: "text_field".into(),
                label: Some("Name".into()),
                value: None,
                bounds: None,
                enabled: true,
                focused: false,
                sensitivity: Sensitivity::None,
                actions: BTreeSet::from([SemanticAction::SetValue]),
            }],
            elements_truncated: false,
            sensitivity: Sensitivity::None,
            authority: Default::default(),
        };
        run.freshness_tick = 1;
        observation.authority = ObservationAuthority::bind(&run, 1, live_fence(&run)).unwrap();
        run.current_observation = Some(observation);
        run
    }

    fn ready_isolated_run() -> ComputerRun {
        let mut run = ready_run();
        run.capability_proof = ComputerCapabilityProof::IndependentlyIsolatedVisualInputDomain {
            backend_id: crate::computer_use::SIMULATOR_ISOLATED_BACKEND_ID.into(),
            surface_id: run.surface.surface_id.clone(),
            incarnation: run.surface.incarnation.clone(),
            input_domain_id: format!("input-domain-{}", Uuid::new_v4()),
            origin: crate::computer_use::IsolationProofOrigin::SimulatorFixture,
            observe: true,
            semantic_actions: true,
            text_entry: true,
            key_chords: true,
            pointer_fallback: true,
        };
        let now = Utc::now();
        run.grant = Some(ActionGrant::for_run(
            &run,
            BTreeSet::from([
                ActionClass::Semantic,
                ActionClass::TextEntry,
                ActionClass::KeyChord,
                ActionClass::PointerFallback,
            ]),
            now - Duration::seconds(1),
            now + Duration::minutes(5),
            None,
        ));
        run
    }

    fn live_fence(run: &ComputerRun) -> SurfaceFreshnessFence {
        SurfaceFreshnessFence {
            surface_id: run.surface.surface_id.clone(),
            incarnation: run.surface.incarnation.clone(),
            tick: run.freshness_tick.max(1),
            wall_clock: Some(Utc::now()),
        }
    }

    fn caller(run: &ComputerRun) -> ComputerPrincipal {
        run.required_principal().unwrap().clone()
    }

    fn authorize(
        run: &ComputerRun,
        observation: &ComputerObservation,
        action: ComputerAction,
    ) -> ComputerResult<()> {
        ComputerPolicy.authorize_action(
            run,
            observation,
            &action,
            Utc::now(),
            &caller(run),
            &live_fence(run),
        )
    }

    #[test]
    fn action_requires_exact_current_observation() {
        let run = ready_run();
        let mut stale = run.current_observation.clone().unwrap();
        stale.observation_id = "obs-old".into();
        let err = authorize(
            &run,
            &stale,
            ComputerAction::SetValue {
                element_id: "field".into(),
                text: "Ada".into(),
            },
        )
        .unwrap_err();
        assert_eq!(err.code, ComputerErrorCode::StaleObservation);
    }

    #[test]
    fn hard_denied_element_wins_over_grant() {
        let mut run = ready_run();
        run.current_observation.as_mut().unwrap().elements[0].sensitivity = Sensitivity::Secure;
        let observation = run.current_observation.clone().unwrap();
        let err = authorize(
            &run,
            &observation,
            ComputerAction::SetValue {
                element_id: "field".into(),
                text: "secret".into(),
            },
        )
        .unwrap_err();
        assert_eq!(err.code, ComputerErrorCode::SensitiveSurface);
    }

    #[test]
    fn hard_denied_element_prevents_observation_exposure() {
        let mut observation = ready_run().current_observation.unwrap();
        observation.elements[0].sensitivity = Sensitivity::SystemRestricted;
        let err = ComputerPolicy
            .authorize_observation_exposure(&observation)
            .unwrap_err();
        assert_eq!(err.code, ComputerErrorCode::SensitiveSurface);
    }

    #[test]
    fn unredacted_screenshot_is_never_exposed() {
        let mut observation = ready_run().current_observation.unwrap();
        observation.screenshot = Some(EvidenceRef {
            content_sha256: "a".repeat(64),
            media_type: "image/png".into(),
            byte_len: 1024,
            width: 800,
            height: 600,
            redacted: false,
            asset_id: "asset-1".into(),
        });
        let err = ComputerPolicy
            .authorize_observation_exposure(&observation)
            .unwrap_err();
        assert_eq!(err.code, ComputerErrorCode::SensitiveSurface);
    }

    #[test]
    fn pointer_fallback_needs_isolated_input_proof() {
        let run = ready_run();
        let observation = run.current_observation.clone().unwrap();
        let actions = [
            ComputerAction::PointerClick {
                x: 10.0,
                y: 10.0,
                button: crate::computer_use::PointerButton::Primary,
            },
            ComputerAction::PointerMove { x: 10.0, y: 10.0 },
            ComputerAction::PointerButton {
                x: 10.0,
                y: 10.0,
                button: crate::computer_use::PointerButton::Primary,
                state: crate::computer_use::PointerButtonState::Down,
            },
            ComputerAction::TextInput { text: "Ada".into() },
        ];
        for action in actions {
            assert_eq!(
                authorize(&run, &observation, action).unwrap_err().code,
                ComputerErrorCode::ForbiddenAction
            );
        }
    }

    #[test]
    fn isolated_input_is_bound_to_guest_surface_geometry() {
        let run = ready_isolated_run();
        let observation = run.current_observation.clone().unwrap();
        authorize(
            &run,
            &observation,
            ComputerAction::PointerMove { x: 10.0, y: 10.0 },
        )
        .unwrap();
        authorize(
            &run,
            &observation,
            ComputerAction::TextInput { text: "Ada".into() },
        )
        .unwrap();
        for action in [
            ComputerAction::PointerMove { x: 800.0, y: 1.0 },
            ComputerAction::PointerButton {
                x: 1.0,
                y: 600.0,
                button: crate::computer_use::PointerButton::Primary,
                state: crate::computer_use::PointerButtonState::Up,
            },
        ] {
            assert_eq!(
                authorize(&run, &observation, action).unwrap_err().code,
                ComputerErrorCode::ForbiddenAction
            );
        }
    }

    #[test]
    fn grant_lifetime_cannot_exceed_run_limit() {
        let run = ComputerRun::attested_foreground_for_test(
            Uuid::new_v4(),
            None,
            ready_run().target,
            ComputerUseLimits::default(),
        )
        .unwrap();
        let now = Utc::now();
        let grant = ActionGrant::for_run(
            &run,
            BTreeSet::from([ActionClass::Semantic]),
            now,
            now + Duration::hours(1),
            None,
        );
        assert_eq!(
            ComputerPolicy
                .authorize_grant(&run, &grant, now, &caller(&run))
                .unwrap_err()
                .code,
            ComputerErrorCode::InvalidRequest
        );
    }

    #[test]
    fn principal_mismatch_denies_action_grant_and_observation() {
        let run = ready_run();
        let observation = run.current_observation.clone().unwrap();
        let intruder = ComputerPrincipal::local_operator(Uuid::new_v4());
        let other_owner = ComputerPrincipal::local_operator(Uuid::new_v4());
        let now = Utc::now();
        for caller in [intruder, other_owner] {
            assert_eq!(
                ComputerPolicy
                    .authorize_action(
                        &run,
                        &observation,
                        &ComputerAction::SetValue {
                            element_id: "field".into(),
                            text: "Ada".into(),
                        },
                        now,
                        &caller,
                        &live_fence(&run),
                    )
                    .unwrap_err()
                    .code,
                ComputerErrorCode::Unauthorized
            );
            assert_eq!(
                ComputerPolicy
                    .authorize_observation(&run, now, &caller)
                    .unwrap_err()
                    .code,
                ComputerErrorCode::Unauthorized
            );
            assert_eq!(
                ComputerPolicy
                    .authorize_grant(&run, run.grant.as_ref().unwrap(), now, &caller,)
                    .unwrap_err()
                    .code,
                ComputerErrorCode::Unauthorized
            );
        }
    }

    #[test]
    fn surface_incarnation_and_epoch_mismatches_deny_before_dispatch() {
        let run = ready_run();
        let observation = run.current_observation.clone().unwrap();
        let action = ComputerAction::SetValue {
            element_id: "field".into(),
            text: "Ada".into(),
        };
        let now = Utc::now();
        let caller = caller(&run);

        let mut surface = observation.clone();
        surface.authority.surface.incarnation = format!("incarnation-{}", Uuid::new_v4());
        assert_eq!(
            ComputerPolicy
                .authorize_action(&run, &surface, &action, now, &caller, &live_fence(&run))
                .unwrap_err()
                .code,
            ComputerErrorCode::ForbiddenTarget
        );

        let mut generation = observation.clone();
        generation.authority.target_generation = run.target.generation + 1;
        assert_eq!(
            ComputerPolicy
                .authorize_action(&run, &generation, &action, now, &caller, &live_fence(&run))
                .unwrap_err()
                .code,
            ComputerErrorCode::TargetChanged
        );

        let mut authority = observation.clone();
        authority.authority.authority_epoch = run.authority_epoch + 3;
        assert_eq!(
            ComputerPolicy
                .authorize_action(&run, &authority, &action, now, &caller, &live_fence(&run))
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );

        let mut control = observation.clone();
        control.authority.control_epoch = run.control_epoch + 2;
        assert_eq!(
            ComputerPolicy
                .authorize_action(&run, &control, &action, now, &caller, &live_fence(&run))
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );

        let mut frame = observation.clone();
        frame.authority.frame_epoch = 99;
        assert_eq!(
            ComputerPolicy
                .authorize_action(&run, &frame, &action, now, &caller, &live_fence(&run))
                .unwrap_err()
                .code,
            ComputerErrorCode::StaleObservation
        );

        let mut future_tick = live_fence(&run);
        future_tick.tick = 1;
        let mut future_obs = observation.clone();
        future_obs.authority.freshness.tick = 9;
        assert_eq!(
            ComputerPolicy
                .authorize_action(&run, &future_obs, &action, now, &caller, &future_tick)
                .unwrap_err()
                .code,
            ComputerErrorCode::StaleObservation
        );

        let mut foreign = live_fence(&run);
        foreign.incarnation = format!("incarnation-{}", Uuid::new_v4());
        assert_eq!(
            ComputerPolicy
                .authorize_action(&run, &observation, &action, now, &caller, &foreign)
                .unwrap_err()
                .code,
            ComputerErrorCode::StaleObservation
        );
    }

    #[test]
    fn background_semantic_cannot_activate_target() {
        let mut run = ready_run();
        run.capability_proof = ComputerCapabilityProof::MeasuredBackgroundSafeSemantic {
            backend_id: crate::computer_use::SIMULATOR_BACKGROUND_BACKEND_ID.into(),
            observe: true,
            semantic_actions: true,
            text_entry: true,
            measurement_id: format!("measurement-{}", Uuid::new_v4()),
        };
        run.grant.as_mut().unwrap().capability_tier =
            ComputerCapabilityTier::MeasuredBackgroundSafeSemantic;
        let observation = run.current_observation.clone().unwrap();
        let err = authorize(&run, &observation, ComputerAction::ActivateTarget).unwrap_err();
        assert_eq!(err.code, ComputerErrorCode::ForbiddenAction);
    }

    #[test]
    fn pointer_and_key_require_isolated_proof_even_with_grant_class() {
        let mut run = ready_run();
        run.grant.as_mut().unwrap().action_classes = BTreeSet::from([
            ActionClass::PointerFallback,
            ActionClass::KeyChord,
            ActionClass::Semantic,
        ]);
        let observation = run.current_observation.clone().unwrap();
        assert_eq!(
            authorize(
                &run,
                &observation,
                ComputerAction::PointerClick {
                    x: 10.0,
                    y: 10.0,
                    button: crate::computer_use::PointerButton::Primary,
                },
            )
            .unwrap_err()
            .code,
            ComputerErrorCode::ForbiddenAction
        );
        assert_eq!(
            authorize(
                &run,
                &observation,
                ComputerAction::KeyChord {
                    keys: vec![crate::computer_use::ComputerKey::Enter],
                },
            )
            .unwrap_err()
            .code,
            ComputerErrorCode::ForbiddenAction
        );
    }

    #[test]
    fn older_freshness_tick_is_stale_not_live() {
        let run = ready_run();
        let mut observation = run.current_observation.clone().unwrap();
        let action = ComputerAction::SetValue {
            element_id: "field".into(),
            text: "Ada".into(),
        };
        let mut live = live_fence(&run);
        live.tick = 2;
        observation.authority.freshness.tick = 1;
        assert_eq!(
            ComputerPolicy
                .authorize_action(
                    &run,
                    &observation,
                    &action,
                    Utc::now(),
                    &caller(&run),
                    &live,
                )
                .unwrap_err()
                .code,
            ComputerErrorCode::StaleObservation
        );
    }

    #[test]
    fn unproven_capability_cannot_observe_or_read_evidence() {
        let mut run = ready_run();
        run.capability_proof = ComputerCapabilityProof::Unproven;
        assert_eq!(
            ComputerPolicy
                .authorize_observation(&run, Utc::now(), &caller(&run))
                .unwrap_err()
                .code,
            ComputerErrorCode::ForbiddenAction
        );
        assert_eq!(
            ComputerPolicy
                .authorize_evidence(&run, &caller(&run))
                .unwrap_err()
                .code,
            ComputerErrorCode::ForbiddenAction
        );
        run.initiating_principal = None;
        assert_eq!(
            ComputerPolicy
                .authorize_observation(
                    &run,
                    Utc::now(),
                    &ComputerPrincipal::local_operator(run.owner_session_id)
                )
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );
    }

    #[test]
    fn missing_grant_principal_and_cross_session_mismatch_deny() {
        let owner = Uuid::new_v4();
        let mut awaiting = ComputerRun::attested_foreground_for_test(
            owner,
            None,
            ready_run().target,
            Default::default(),
        )
        .unwrap();
        let now = Utc::now();
        awaiting.grant = Some(ActionGrant::for_run(
            &awaiting,
            BTreeSet::from([ActionClass::Semantic]),
            now - Duration::seconds(1),
            now + Duration::minutes(5),
            None,
        ));
        awaiting.grant.as_mut().unwrap().principal = None;
        assert_eq!(
            ComputerPolicy
                .authorize_grant(
                    &awaiting,
                    awaiting.grant.as_ref().unwrap(),
                    now,
                    awaiting.required_principal().unwrap(),
                )
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );

        let mut run = ready_run();
        let observation = run.current_observation.clone().unwrap();
        let caller = caller(&run);
        run.grant.as_mut().unwrap().principal = None;
        assert_eq!(
            ComputerPolicy
                .authorize_action(
                    &run,
                    &observation,
                    &ComputerAction::SetValue {
                        element_id: "field".into(),
                        text: "Ada".into(),
                    },
                    now,
                    &caller,
                    &live_fence(&run),
                )
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );

        let other = ComputerPrincipal::local_operator(Uuid::new_v4());
        run.grant.as_mut().unwrap().principal = Some(caller.clone());
        assert_eq!(
            ComputerPolicy
                .authorize_caller(&run, &other)
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );
    }

    #[test]
    fn wall_clock_alone_and_future_observation_are_not_dispatch_proof() {
        let mut run = ready_run();
        let mut observation = run.current_observation.clone().unwrap();
        let action = ComputerAction::SetValue {
            element_id: "field".into(),
            text: "Ada".into(),
        };
        let now = Utc::now();
        let caller = caller(&run);

        observation.authority.freshness.tick = 0;
        observation.authority.freshness.wall_clock = Some(now);
        run.current_observation = Some(observation.clone());
        let mut wall_only = live_fence(&run);
        wall_only.tick = 0;
        wall_only.wall_clock = Some(now);
        assert_eq!(
            ComputerPolicy
                .authorize_action(&run, &observation, &action, now, &caller, &wall_only)
                .unwrap_err()
                .code,
            ComputerErrorCode::StaleObservation
        );

        let mut future = ready_run();
        future.current_observation.as_mut().unwrap().captured_at = now + Duration::seconds(30);
        let future_obs = future.current_observation.clone().unwrap();
        assert_eq!(
            authorize(&future, &future_obs, action).unwrap_err().code,
            ComputerErrorCode::StaleObservation
        );
    }
}
