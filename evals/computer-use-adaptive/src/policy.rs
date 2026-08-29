//! Profile-independent safety plus profile-only extra denials.
//!
//! Economy never weakens a check. It may only add efficiency denials.

use crate::profile::ProfileBudget;
use crate::types::{
    ActionClass, CompactObservation, ModelCapability, ProfileId, Sensitivity, TimeoutClass,
    TypedAction,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyCode {
    Takeover,
    Cancelled,
    MissingObservation,
    StaleObservation,
    TargetChanged,
    GrantMissing,
    GrantExpired,
    GrantExhausted,
    ForbiddenAction,
    VisualUnauthorized,
    LeaseNotGranted,
    LeaseContention,
    UnknownElement,
    DisabledElement,
    SensitiveSurface,
    UnadvertisedAction,
    Contradiction,
    UnsupportedVision,
    Underqualified,
    TimeoutBeforeSend,
    ProfileBudget,
    CapabilityDowngrade,
}

impl DenyCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Takeover => "takeover",
            Self::Cancelled => "cancelled",
            Self::MissingObservation => "missing_observation",
            Self::StaleObservation => "stale_observation",
            Self::TargetChanged => "target_changed",
            Self::GrantMissing => "grant_missing",
            Self::GrantExpired => "grant_expired",
            Self::GrantExhausted => "grant_exhausted",
            Self::ForbiddenAction => "forbidden_action",
            Self::VisualUnauthorized => "visual_unauthorized",
            Self::LeaseNotGranted => "lease_not_granted",
            Self::LeaseContention => "lease_contention",
            Self::UnknownElement => "unknown_element",
            Self::DisabledElement => "disabled_element",
            Self::SensitiveSurface => "sensitive_surface",
            Self::UnadvertisedAction => "unadvertised_action",
            Self::Contradiction => "ax_pixel_contradiction",
            Self::UnsupportedVision => "unsupported_vision",
            Self::Underqualified => "underqualified_model",
            Self::TimeoutBeforeSend => "timeout_before_send",
            Self::ProfileBudget => "profile_budget",
            Self::CapabilityDowngrade => "capability_downgrade",
        }
    }

    pub fn is_stale(self) -> bool {
        matches!(
            self,
            Self::StaleObservation | Self::TargetChanged | Self::MissingObservation
        )
    }

    pub fn is_safety(self) -> bool {
        !matches!(self, Self::ProfileBudget)
    }
}

#[derive(Debug, Clone)]
pub struct PolicyView {
    pub profile: ProfileId,
    pub caps: ModelCapability,
    pub takeover: bool,
    pub cancelled: bool,
    pub timeout_before_send: bool,
    pub grant_present: bool,
    pub grant_expired: bool,
    pub grant_exhausted: bool,
    pub grant_classes: Vec<ActionClass>,
    pub visual_granted: bool,
    pub lease_granted: bool,
    pub domain_busy: bool,
    pub current_observation_id: Option<String>,
    pub surface_generation: u64,
    pub surface_incarnation: u64,
    pub surface_sensitivity: Sensitivity,
}

/// Safety is identical for every profile. Profile is intentionally unused.
pub fn safety_authorize(
    view: &PolicyView,
    observation: &CompactObservation,
    requested_obs: &str,
    action: &TypedAction,
) -> Result<(), DenyCode> {
    let _profile_ignored = view.profile;
    if view.takeover {
        return Err(DenyCode::Takeover);
    }
    if view.cancelled {
        return Err(DenyCode::Cancelled);
    }
    if view.timeout_before_send {
        return Err(DenyCode::TimeoutBeforeSend);
    }
    if !view.caps.tools {
        return Err(DenyCode::Underqualified);
    }
    if !view.grant_present {
        return Err(DenyCode::GrantMissing);
    }
    if view.grant_expired {
        return Err(DenyCode::GrantExpired);
    }
    if view.grant_exhausted {
        return Err(DenyCode::GrantExhausted);
    }
    if !view.lease_granted {
        return Err(DenyCode::LeaseNotGranted);
    }
    if view.domain_busy {
        return Err(DenyCode::LeaseContention);
    }
    match view.current_observation_id.as_deref() {
        None => return Err(DenyCode::MissingObservation),
        Some(current) if current != requested_obs => return Err(DenyCode::StaleObservation),
        Some(_) => {}
    }
    if observation.observation_id != requested_obs {
        return Err(DenyCode::StaleObservation);
    }
    if observation.generation != view.surface_generation
        || observation.incarnation != view.surface_incarnation
    {
        return Err(DenyCode::TargetChanged);
    }
    if view.surface_sensitivity.is_hard_denied() || observation.sensitivity.is_hard_denied() {
        return Err(DenyCode::SensitiveSurface);
    }
    if observation.ax_pixel_contradiction {
        return Err(DenyCode::Contradiction);
    }
    let class = action.class();
    if !view.grant_classes.contains(&class) {
        return Err(DenyCode::ForbiddenAction);
    }
    if class == ActionClass::PointerFallback {
        if !view.caps.vision {
            return Err(DenyCode::UnsupportedVision);
        }
        if !view.visual_granted {
            return Err(DenyCode::VisualUnauthorized);
        }
    }
    if let Some(element_id) = action.referenced_element() {
        let Some(el) = observation
            .elements
            .iter()
            .find(|e| e.element_id == element_id)
        else {
            return Err(DenyCode::UnknownElement);
        };
        if el.sensitivity.is_hard_denied() {
            return Err(DenyCode::SensitiveSurface);
        }
        if !el.enabled {
            return Err(DenyCode::DisabledElement);
        }
        let needed = match action {
            TypedAction::Invoke { .. } => "invoke",
            TypedAction::SetValue { .. } => "set_value",
            TypedAction::Select { .. } => "select",
            TypedAction::Scroll { .. } => "scroll",
            _ => "",
        };
        if !needed.is_empty() && !el.advertised_actions.iter().any(|a| a == needed) {
            return Err(DenyCode::UnadvertisedAction);
        }
    }
    match action {
        TypedAction::SetValue { text, .. } if text.contains('\0') || text.len() > 16 * 1024 => {
            return Err(DenyCode::ForbiddenAction);
        }
        TypedAction::Scroll {
            delta_x, delta_y, ..
        } if delta_x.unsigned_abs() > 10_000 || delta_y.unsigned_abs() > 10_000 => {
            return Err(DenyCode::ForbiddenAction);
        }
        TypedAction::KeyChord { keys } if keys.is_empty() || keys.len() > 4 => {
            return Err(DenyCode::ForbiddenAction);
        }
        TypedAction::Wait { millis } if *millis > 10_000 => return Err(DenyCode::ForbiddenAction),
        _ => {}
    }
    Ok(())
}

pub fn budget_authorize(profile: ProfileId, action: &TypedAction) -> Result<(), DenyCode> {
    let budget = ProfileBudget::for_profile(profile);
    if !budget.allows_class(action.class()) {
        return Err(DenyCode::ProfileBudget);
    }
    Ok(())
}

pub fn authorize(
    view: &PolicyView,
    observation: &CompactObservation,
    requested_obs: &str,
    action: &TypedAction,
) -> Result<(), DenyCode> {
    safety_authorize(view, observation, requested_obs, action)?;
    budget_authorize(view.profile, action)?;
    if action.class() == ActionClass::PointerFallback
        && !ProfileBudget::for_profile(view.profile).allow_screenshot
    {
        return Err(DenyCode::ProfileBudget);
    }
    Ok(())
}

/// If any profile would allow a dispatch, safety would allow it for all profiles.
pub fn safety_is_profile_invariant(
    mut view: PolicyView,
    observation: &CompactObservation,
    requested_obs: &str,
    action: &TypedAction,
) -> bool {
    let mut outcomes = Vec::new();
    for profile in ProfileId::ALL {
        view.profile = profile;
        outcomes.push(safety_authorize(&view, observation, requested_obs, action));
    }
    outcomes.windows(2).all(|w| w[0] == w[1])
}

pub fn timeout_blocks_send(timeout: Option<TimeoutClass>) -> bool {
    matches!(timeout, Some(TimeoutClass::DefinitelyBeforeSend))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Geometry;

    fn obs() -> CompactObservation {
        CompactObservation {
            observation_id: "obs_1".into(),
            sequence: 1,
            surface_id: "surf_a".into(),
            app_id: "app.demo".into(),
            window_id: "win_1".into(),
            generation: 1,
            incarnation: 1,
            captured_at_ms: 0,
            sensitivity: Sensitivity::None,
            ax_pixel_contradiction: false,
            elements: vec![crate::types::CompactElement {
                element_id: "el_submit".into(),
                stable_key: "submit".into(),
                role: "button".into(),
                name: "Submit".into(),
                context: None,
                enabled: true,
                focused: false,
                sensitivity: Sensitivity::None,
                advertised_actions: ["invoke".into()].into_iter().collect(),
                bounds: Some(Geometry::new(0, 0, 40, 16)),
            }],
            frame_regions: None,
            image_bytes: 0,
        }
    }

    fn view() -> PolicyView {
        PolicyView {
            profile: ProfileId::Economy,
            caps: ModelCapability {
                tools: true,
                vision: false,
                structured_output: true,
            },
            takeover: false,
            cancelled: false,
            timeout_before_send: false,
            grant_present: true,
            grant_expired: false,
            grant_exhausted: false,
            grant_classes: vec![ActionClass::Semantic, ActionClass::TextEntry],
            visual_granted: false,
            lease_granted: true,
            domain_busy: false,
            current_observation_id: Some("obs_1".into()),
            surface_generation: 1,
            surface_incarnation: 1,
            surface_sensitivity: Sensitivity::None,
        }
    }

    #[test]
    fn safety_identical_across_profiles() {
        let observation = obs();
        let action = TypedAction::Invoke {
            element_id: "el_submit".into(),
        };
        assert!(safety_is_profile_invariant(
            view(),
            &observation,
            "obs_1",
            &action
        ));
    }

    #[test]
    fn stale_observation_denied_for_every_profile() {
        let observation = obs();
        let action = TypedAction::Invoke {
            element_id: "el_submit".into(),
        };
        for profile in ProfileId::ALL {
            let mut v = view();
            v.profile = profile;
            let err = safety_authorize(&v, &observation, "obs_old", &action).unwrap_err();
            assert_eq!(err, DenyCode::StaleObservation);
        }
    }

    #[test]
    fn economy_does_not_allow_pointer_even_when_safety_would() {
        let mut observation = obs();
        observation.frame_regions = Some(vec![crate::types::FrameRegion {
            label: "Go".into(),
            bounds: Geometry::new(2, 2, 8, 8),
        }]);
        let action = TypedAction::PointerClick {
            x: 6,
            y: 6,
            button: crate::types::PointerButton::Primary,
        };
        let mut v = view();
        v.profile = ProfileId::HighAssurance;
        v.caps.vision = true;
        v.visual_granted = true;
        v.grant_classes.push(ActionClass::PointerFallback);
        assert!(safety_authorize(&v, &observation, "obs_1", &action).is_ok());
        v.profile = ProfileId::Economy;
        assert_eq!(
            budget_authorize(ProfileId::Economy, &action),
            Err(DenyCode::ProfileBudget)
        );
    }
}
