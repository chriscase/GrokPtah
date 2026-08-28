//! Adversarial coverage for the production adaptive seam.
//!
//! These tests exercise the public bridge boundary, not a standalone evaluator
//! runtime. They never call a provider or dispatch a native action.

use std::collections::BTreeSet;

use chrono::Utc;
use grokptah_agent_bridge::{
    render_computer_observation, validate_computer_proposal,
    validate_computer_proposal_safety_only, AdaptiveController, AdaptiveProfile,
    CapabilityAttribution, CapabilityEvidence, ComputerAction, ComputerAgentProposal,
    ComputerObservation, ComputerTarget, HostCapabilityEvidence, ModelCapabilityEvidence,
    ObservationGeometry, ProfileReason, ProfileTransition, RuntimeSignal, SafetyFloor,
    SemanticAction, SemanticElement, Sensitivity, TaskPolicy, TaskRisk,
};

fn observation() -> ComputerObservation {
    ComputerObservation {
        observation_id: "observation-current".into(),
        sequence: 1,
        target: ComputerTarget {
            app_id: "com.example.fixture".into(),
            window_id: "window-1".into(),
            generation: 1,
            display_name: "Fixture".into(),
            sensitivity: Sensitivity::None,
        },
        captured_at: Utc::now(),
        geometry: ObservationGeometry {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
            scale_factor: 1.0,
        },
        screenshot: None,
        elements: vec![SemanticElement {
            element_id: "safe".into(),
            role: "button".into(),
            label: Some("Save".into()),
            value: None,
            bounds: None,
            enabled: true,
            focused: true,
            sensitivity: Sensitivity::None,
            actions: BTreeSet::from([SemanticAction::Invoke]),
        }],
        elements_truncated: false,
        sensitivity: Sensitivity::None,
    }
}

fn evidence() -> CapabilityEvidence {
    CapabilityEvidence::new(
        ModelCapabilityEvidence {
            tools: true,
            image_input: true,
            max_image_bytes: Some(4 * 1024 * 1024),
            tier: grokptah_agent_bridge::ComputerUseTier::VisualFallbackAct,
            attribution: CapabilityAttribution::Measured,
            durable_authority: true,
            session_measured: false,
            synthetic_only: false,
        },
        HostCapabilityEvidence {
            semantic_observation: true,
            screenshot_capture: true,
            independent_verifier: true,
            isolated_guest: false,
        },
    )
}

#[test]
fn economy_does_not_change_universal_safety_verdicts() {
    let current = observation();
    let hostile = [
        serde_json::json!({
            "observation_id": "stale",
            "action_type": "invoke",
            "element_id": "safe",
            "summary": "stale",
        }),
        serde_json::json!({
            "observation_id": current.observation_id,
            "action_type": "invoke",
            "element_id": "missing",
            "summary": "invented",
        }),
        serde_json::json!({
            "observation_id": current.observation_id,
            "action_type": "invoke",
            "element_id": "safe",
            "summary": "smuggle",
            "shell": "rm -rf /",
        }),
        serde_json::json!({
            "observation_id": current.observation_id,
            "action_type": "complete",
            "element_id": "safe",
            "summary": "complete and mutate",
        }),
    ];
    for payload in hostile {
        let raw = payload.to_string();
        assert!(validate_computer_proposal_safety_only(&raw, &current).is_err());
        for profile in AdaptiveProfile::ALL {
            assert!(
                validate_computer_proposal(&raw, &current, profile).is_err(),
                "{profile} accepted hostile payload {raw}"
            );
        }
    }
}

#[test]
fn profile_transitions_are_bounded_and_explicit() {
    let decision = match grokptah_agent_bridge::AdaptivePolicyEngine.select(
        &evidence(),
        TaskPolicy {
            risk: TaskRisk::Routine,
            minimum_profile: None,
        },
    ) {
        grokptah_agent_bridge::PolicyOutcome::Proceed(decision) => decision,
        other => panic!("unexpected policy result: {other:?}"),
    };
    let mut controller = AdaptiveController::new("run-1", decision);
    assert_eq!(controller.profile(), AdaptiveProfile::Economy);
    assert!(matches!(
        controller.apply_signal(RuntimeSignal::AmbiguousObservation),
        ProfileTransition::Escalate {
            from: AdaptiveProfile::Economy,
            to: AdaptiveProfile::Balanced,
            reason: ProfileReason::AmbiguousObservation
        }
    ));
    assert!(matches!(
        controller.apply_signal(RuntimeSignal::CapabilityRevoked),
        ProfileTransition::Stop(_)
    ));
    assert_eq!(
        controller.terminal().map(|terminal| terminal.reason),
        Some(ProfileReason::CapabilityRevoked)
    );
}

#[test]
fn unknown_and_malformed_profiles_fail_closed_without_alias_output() {
    assert!(serde_json::from_str::<AdaptiveProfile>("\"economy\"").is_ok());
    assert!(serde_json::from_str::<AdaptiveProfile>("\"efficient\"").is_ok());
    assert!(serde_json::from_str::<AdaptiveProfile>("\"not-a-profile\"").is_err());
    assert!(serde_json::from_str::<AdaptiveProfile>("null").is_err());
    for profile in AdaptiveProfile::ALL {
        let wire = serde_json::to_string(&profile).unwrap();
        assert!(!wire.contains("efficient"));
        assert!(!wire.contains("frontier"));
    }
    assert_eq!(AdaptiveProfile::ALL.len(), 3);
}

#[test]
fn rendered_projection_has_no_sensitive_observation_or_frame_digest() {
    let mut current = observation();
    current.target.display_name = "SECRET-PATH-WINDOW".into();
    current.elements[0].label = Some("SECRET-CONTROL".into());
    current.elements[0].value = Some("SECRET-VALUE".into());
    let (rendered, _) = render_computer_observation(&current, AdaptiveProfile::HighAssurance);
    // Model rendering may contain semantic labels by design; public projection
    // is the redaction boundary. The rendered packet is never public.
    assert!(rendered.to_string().contains("SECRET-CONTROL"));
    let mut controller = AdaptiveController::new(
        "run-1",
        match grokptah_agent_bridge::AdaptivePolicyEngine.select(&evidence(), TaskPolicy::default())
        {
            grokptah_agent_bridge::PolicyOutcome::Proceed(decision) => decision,
            other => panic!("unexpected policy result: {other:?}"),
        },
    );
    controller.observe_frame(grokptah_agent_bridge::ObservationFingerprint::of(&current));
    let projection = grokptah_agent_bridge::project_adaptive(&controller);
    let wire = serde_json::to_string(&projection).unwrap();
    for needle in [
        "SECRET-PATH-WINDOW",
        "SECRET-CONTROL",
        "SECRET-VALUE",
        "observation-current",
    ] {
        assert!(!wire.contains(needle), "projection leaked {needle}");
    }
    assert_eq!(projection.safety_floor, SafetyFloor::REQUIRED);
}

#[test]
fn recovery_crash_cut_is_terminal_and_cost_is_truthful() {
    let decision = match grokptah_agent_bridge::AdaptivePolicyEngine
        .select(&evidence(), TaskPolicy::default())
    {
        grokptah_agent_bridge::PolicyOutcome::Proceed(decision) => decision,
        other => panic!("unexpected policy result: {other:?}"),
    };
    let mut controller = AdaptiveController::new("run-1", decision);
    controller.begin_turn(0).unwrap();
    controller.recover_interrupted();
    assert_eq!(
        controller.terminal().map(|terminal| terminal.kind),
        Some(grokptah_agent_bridge::TerminalKind::Interrupted)
    );
    assert_eq!(controller.spend().model_calls, 0);
    assert!(controller.begin_turn(0).is_err());
}

#[test]
fn visual_pointer_actions_require_current_grounded_visual_evidence() {
    let current = observation();
    let raw = serde_json::json!({
        "observation_id": current.observation_id,
        "action_type": "pointer_click",
        "x": 20,
        "y": 20,
        "button": "primary",
        "summary": "click the visible control",
    })
    .to_string();
    assert!(validate_computer_proposal(&raw, &current, AdaptiveProfile::HighAssurance).is_err());
    assert!(validate_computer_proposal_safety_only(&raw, &current).is_err());
    // Ensure the typed grammar does not accidentally acquire an unbound raw
    // action through a public enum conversion.
    let _ = ComputerAction::ActivateTarget;
    let _ = ComputerAgentProposal::Complete {
        observation_id: "observation-current".into(),
        summary: "not a dispatch".into(),
    };
}
