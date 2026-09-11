use chrono::{DateTime, Utc};

use super::types::{
    ActionGrant, ComputerAction, ComputerError, ComputerErrorCode, ComputerObservation,
    ComputerResult, ComputerRun, ComputerRunState, SemanticAction,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct ComputerPolicy;

impl ComputerPolicy {
    pub fn run_limit_reached(&self, run: &ComputerRun, now: DateTime<Utc>) -> bool {
        run.action_count >= run.limits.max_actions || run.duration_exceeded(now)
    }

    pub fn authorize_grant(
        &self,
        run: &ComputerRun,
        grant: &ActionGrant,
        now: DateTime<Utc>,
    ) -> ComputerResult<()> {
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
    ) -> ComputerResult<()> {
        self.authorize_active_run(run, now)?;
        Ok(())
    }

    pub fn authorize_action(
        &self,
        run: &ComputerRun,
        observation: &ComputerObservation,
        action: &ComputerAction,
        now: DateTime<Utc>,
    ) -> ComputerResult<()> {
        self.authorize_active_run(run, now)?;
        action.validate(&run.limits)?;
        observation.validate(&run.limits)?;

        let grant = run.grant.as_ref().ok_or_else(|| {
            ComputerError::new(ComputerErrorCode::Unauthorized, "computer run has no grant")
        })?;
        if grant.run_id != run.run_id || grant.target != run.target {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenTarget,
                "grant target changed",
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

        if let ComputerAction::PointerClick { x, y, .. } = action {
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

    /// Completing a run is allowed only when the last action postcondition is
    /// positive and bound to the exact current observation.
    ///
    /// After a successful dispatch the observation is invalidated for further
    /// actions (`current_observation` is `None`) but remains the completion
    /// evidence until a newer observation becomes current. A later observe
    /// either clears `last_outcome` or leaves it bound to a different
    /// identity; both fail closed here as [`ComputerErrorCode::UnverifiedCompletion`].
    pub fn authorize_completion(&self, run: &ComputerRun) -> ComputerResult<()> {
        if run.state != ComputerRunState::Ready {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "computer run is not ready",
            ));
        }
        let Some(outcome) = &run.last_outcome else {
            return Err(unverified_completion(
                "computer run has no verified postcondition on the current observation",
            ));
        };
        if outcome.expected_postcondition_met != Some(true) {
            return Err(unverified_completion(
                "completion requires a positive postcondition on the current observation",
            ));
        }
        if outcome.observation_id.is_none() || outcome.sequence.is_none() {
            return Err(unverified_completion(
                "completion evidence is not bound to an observation",
            ));
        }
        if let Some(current) = &run.current_observation {
            if !outcome.verifies_observation(current) {
                return Err(unverified_completion(
                    "completion evidence is not bound to the current observation",
                ));
            }
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

fn unverified_completion(message: &str) -> ComputerError {
    ComputerError::new(ComputerErrorCode::UnverifiedCompletion, message)
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
        ActionClass, ActionOutcome, ComputerTarget, ComputerUseLimits, EvidenceRef,
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
        let mut run =
            ComputerRun::new(Uuid::new_v4(), None, target.clone(), Default::default()).unwrap();
        let now = Utc::now();
        run.grant = Some(ActionGrant {
            grant_id: "grant-1".into(),
            run_id: run.run_id.clone(),
            target: target.clone(),
            action_classes: BTreeSet::from([ActionClass::Semantic, ActionClass::TextEntry]),
            issued_by: crate::computer_use::GrantIssuer::LocalUser,
            issued_at: now - Duration::seconds(1),
            expires_at: now + Duration::minutes(5),
            uses_remaining: None,
            revoked_at: None,
        });
        run.transition(ComputerRunState::Ready).unwrap();
        let observation = ComputerObservation {
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
        };
        run.current_observation = Some(observation);
        run
    }

    #[test]
    fn completion_requires_positive_postcondition_bound_to_current_observation() {
        let mut run = ready_run();
        let current = run.current_observation.clone().unwrap();

        assert_eq!(
            ComputerPolicy.authorize_completion(&run).unwrap_err().code,
            ComputerErrorCode::UnverifiedCompletion
        );

        run.last_outcome = Some(ActionOutcome::bounded("set demo name", Some(true)));
        assert_eq!(
            ComputerPolicy.authorize_completion(&run).unwrap_err().code,
            ComputerErrorCode::UnverifiedCompletion,
            "unbound positive postcondition is not current-frame evidence"
        );

        run.last_outcome = Some(
            ActionOutcome::bounded("set demo name", Some(false)).bind_to_observation(&current),
        );
        assert_eq!(
            ComputerPolicy.authorize_completion(&run).unwrap_err().code,
            ComputerErrorCode::UnverifiedCompletion
        );

        let mut stale = current.clone();
        stale.observation_id = "obs-old".into();
        stale.sequence = 0;
        run.last_outcome =
            Some(ActionOutcome::bounded("set demo name", Some(true)).bind_to_observation(&stale));
        assert_eq!(
            ComputerPolicy.authorize_completion(&run).unwrap_err().code,
            ComputerErrorCode::UnverifiedCompletion,
            "a prior frame's positive postcondition cannot complete against a newer observation"
        );

        run.last_outcome =
            Some(ActionOutcome::bounded("set demo name", Some(true)).bind_to_observation(&current));
        ComputerPolicy
            .authorize_completion(&run)
            .expect("positive postcondition bound to the current observation is the success path");
    }

    #[test]
    fn completion_accepts_bound_postcondition_after_dispatch_invalidates_the_observation() {
        let mut run = ready_run();
        let dispatched = run.current_observation.clone().unwrap();
        run.last_outcome = Some(
            ActionOutcome::bounded("set demo name", Some(true)).bind_to_observation(&dispatched),
        );
        run.current_observation = None;
        ComputerPolicy
            .authorize_completion(&run)
            .expect("dispatch invalidates the observation for further actions, not the postcondition it just verified");
    }

    #[test]
    fn action_requires_exact_current_observation() {
        let run = ready_run();
        let mut stale = run.current_observation.clone().unwrap();
        stale.observation_id = "obs-old".into();
        let err = ComputerPolicy
            .authorize_action(
                &run,
                &stale,
                &ComputerAction::SetValue {
                    element_id: "field".into(),
                    text: "Ada".into(),
                },
                Utc::now(),
            )
            .unwrap_err();
        assert_eq!(err.code, ComputerErrorCode::StaleObservation);
    }

    #[test]
    fn hard_denied_element_wins_over_grant() {
        let mut run = ready_run();
        run.current_observation.as_mut().unwrap().elements[0].sensitivity = Sensitivity::Secure;
        let observation = run.current_observation.clone().unwrap();
        let err = ComputerPolicy
            .authorize_action(
                &run,
                &observation,
                &ComputerAction::SetValue {
                    element_id: "field".into(),
                    text: "secret".into(),
                },
                Utc::now(),
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
    fn pointer_fallback_needs_its_own_grant_class() {
        let run = ready_run();
        let observation = run.current_observation.clone().unwrap();
        let err = ComputerPolicy
            .authorize_action(
                &run,
                &observation,
                &ComputerAction::PointerClick {
                    x: 10.0,
                    y: 10.0,
                    button: crate::computer_use::PointerButton::Primary,
                },
                Utc::now(),
            )
            .unwrap_err();
        assert_eq!(err.code, ComputerErrorCode::ForbiddenAction);
    }

    #[test]
    fn grant_lifetime_cannot_exceed_run_limit() {
        let run = ComputerRun::new(
            Uuid::new_v4(),
            None,
            ready_run().target,
            ComputerUseLimits::default(),
        )
        .unwrap();
        let now = Utc::now();
        let grant = ActionGrant {
            grant_id: "grant-long".into(),
            run_id: run.run_id.clone(),
            target: run.target.clone(),
            action_classes: BTreeSet::from([ActionClass::Semantic]),
            issued_by: crate::computer_use::GrantIssuer::LocalUser,
            issued_at: now,
            expires_at: now + Duration::hours(1),
            uses_remaining: None,
            revoked_at: None,
        };
        assert_eq!(
            ComputerPolicy
                .authorize_grant(&run, &grant, now)
                .unwrap_err()
                .code,
            ComputerErrorCode::InvalidRequest
        );
    }
}
