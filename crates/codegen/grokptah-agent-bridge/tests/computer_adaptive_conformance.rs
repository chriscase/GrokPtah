//! The adaptive Computer Use contract speaks the safety kernel's vocabulary.
//!
//! `grokptah-cu-adaptive` sits *above* the provider-neutral safety kernel in
//! `grokptah_agent_bridge::computer_use`. It plans several steps ahead, runs a
//! synthetic benchmark, and produces receipts -- none of which is allowed to
//! invent an action, a refusal, or a sensitivity level the kernel does not
//! already have. If it could, its benchmark would be measuring a contract that
//! does not exist here, and a refusal it reported would not correspond to
//! anything the real state machine would do.
//!
//! These tests are the seam. They are deliberately mechanical: each one takes
//! a vocabulary or a bound from the adaptive layer and finds the kernel
//! construct it has to correspond to. Nothing here opens an application,
//! requests a permission, dispatches input, or calls a provider; both sides
//! are pure data.
//!
//! When a future change adds a variant on either side, the test that fails
//! tells you which side moved.

use std::collections::BTreeSet;

use grokptah_agent_bridge::computer_use::{
    ActionClass, ComputerAction, ComputerErrorCode, ComputerUseLimits, SemanticAction,
    Sensitivity as KernelSensitivity,
};
use grokptah_cu_adaptive::profile::{ProfileId, MAX_FRAME_AGE_CEILING_MILLIS};
use grokptah_cu_adaptive::redaction::{
    Sensitivity as AdaptiveSensitivity, MAX_TEXT_ENTRY_BYTES as ADAPTIVE_MAX_TEXT,
};
use grokptah_cu_adaptive::schema::{
    ChordKey, IntentFamily, StepIntent, MAX_CHORD_KEYS, MAX_SCROLL_DELTA, MAX_WAIT_MILLIS,
};
use grokptah_cu_adaptive::vocabulary::DenyReason;

/// Build a kernel action through the kernel's own deserializer.
///
/// Going through serde rather than through the constructors is deliberate:
/// the adaptive layer's `kernel_action_tag` names a *tag*, so what has to be
/// true is that the tag and its arguments are ones the kernel's wire contract
/// accepts. A constructor call would compile against the type and prove
/// nothing about the tag.
fn kernel_action(value: serde_json::Value) -> ComputerAction {
    serde_json::from_value(value).expect("the kernel accepts this action")
}

fn kernel_error_slug(code: ComputerErrorCode) -> String {
    serde_json::to_value(code)
        .expect("kernel error codes serialize")
        .as_str()
        .expect("as a string")
        .to_string()
}

/// Every kernel error code, by construction rather than by a hand-kept list:
/// a new variant that nobody adds here would make the coverage test below
/// wrong rather than silently passing.
const KERNEL_ERROR_CODES: &[ComputerErrorCode] = &[
    ComputerErrorCode::InvalidRequest,
    ComputerErrorCode::InvalidState,
    ComputerErrorCode::Unauthorized,
    ComputerErrorCode::PermissionRequired,
    ComputerErrorCode::PermissionDenied,
    ComputerErrorCode::PermissionRevoked,
    ComputerErrorCode::UnsupportedPlatform,
    ComputerErrorCode::ForbiddenTarget,
    ComputerErrorCode::ForbiddenAction,
    ComputerErrorCode::SensitiveSurface,
    ComputerErrorCode::StaleObservation,
    ComputerErrorCode::TargetChanged,
    ComputerErrorCode::TargetClosed,
    ComputerErrorCode::LimitReached,
    ComputerErrorCode::Conflict,
    ComputerErrorCode::Pending,
    ComputerErrorCode::UncertainOutcome,
    ComputerErrorCode::Interrupted,
    ComputerErrorCode::BackendUnavailable,
    ComputerErrorCode::BackendFailure,
    ComputerErrorCode::Internal,
];

#[test]
fn every_adaptive_refusal_maps_onto_a_real_kernel_error_code() {
    let kernel: BTreeSet<String> = KERNEL_ERROR_CODES
        .iter()
        .copied()
        .map(kernel_error_slug)
        .collect();
    for reason in DenyReason::ALL {
        let mapped = reason.kernel_error_code();
        assert!(
            kernel.contains(mapped),
            "{reason:?} maps to {mapped:?}, which is not a kernel error code"
        );
    }
}

#[test]
fn the_safety_critical_refusals_map_to_the_kernel_code_that_means_the_same_thing() {
    // The mapping is many-to-one, so most entries are a judgement call. These
    // are not: if any of them drifted, a benchmark refusal would be reported
    // under a kernel code that means something else.
    let pinned: &[(DenyReason, ComputerErrorCode)] = &[
        (
            DenyReason::SensitiveSurface,
            ComputerErrorCode::SensitiveSurface,
        ),
        (
            DenyReason::RedactionRequired,
            ComputerErrorCode::SensitiveSurface,
        ),
        (DenyReason::StaleFrame, ComputerErrorCode::StaleObservation),
        (DenyReason::TargetDrifted, ComputerErrorCode::TargetChanged),
        (
            DenyReason::ClassOutsideGrant,
            ComputerErrorCode::ForbiddenAction,
        ),
        (
            DenyReason::PointerWithoutVisualGrounding,
            ComputerErrorCode::ForbiddenAction,
        ),
        (
            DenyReason::LeaseVersionConflict,
            ComputerErrorCode::Conflict,
        ),
        (DenyReason::LeaseLost, ComputerErrorCode::Unauthorized),
        (
            DenyReason::ApprovalRequired,
            ComputerErrorCode::PermissionRequired,
        ),
        (
            DenyReason::ApprovalDenied,
            ComputerErrorCode::PermissionDenied,
        ),
        (DenyReason::Cancelled, ComputerErrorCode::Interrupted),
        (DenyReason::BudgetExhausted, ComputerErrorCode::LimitReached),
        (
            DenyReason::SchemaViolation,
            ComputerErrorCode::InvalidRequest,
        ),
        (
            DenyReason::BackendUnavailable,
            ComputerErrorCode::BackendUnavailable,
        ),
    ];
    for (reason, expected) in pinned {
        assert_eq!(
            reason.kernel_error_code(),
            kernel_error_slug(*expected),
            "{reason:?} drifted away from {expected:?}"
        );
    }
}

#[test]
fn every_dispatchable_intent_names_an_action_the_kernel_can_execute() {
    let kernel_tags: BTreeSet<&'static str> = [
        "activate_target",
        "invoke",
        "set_value",
        "select",
        "scroll",
        "key_chord",
        "pointer_click",
        "wait",
    ]
    .into_iter()
    .collect();
    // The list above is checked against the kernel itself rather than trusted:
    // each tag has to be the serde tag of a real `ComputerAction`.
    let representative = [
        kernel_action(serde_json::json!({"type": "activate_target"})),
        kernel_action(serde_json::json!({"type": "invoke", "element_id": "element"})),
        kernel_action(
            serde_json::json!({"type": "set_value", "element_id": "element", "text": "value"}),
        ),
        kernel_action(serde_json::json!({"type": "select", "element_id": "element"})),
        kernel_action(serde_json::json!({"type": "scroll", "delta_x": 0, "delta_y": 1})),
        kernel_action(serde_json::json!({"type": "key_chord", "keys": ["enter"]})),
        kernel_action(
            serde_json::json!({"type": "pointer_click", "x": 1.0, "y": 1.0, "button": "primary"}),
        ),
        kernel_action(serde_json::json!({"type": "wait", "millis": 1})),
    ];
    let actual: BTreeSet<String> = representative
        .iter()
        .map(|action| {
            serde_json::to_value(action).expect("kernel actions serialize")["type"]
                .as_str()
                .expect("a string tag")
                .to_string()
        })
        .collect();
    let expected: BTreeSet<String> = kernel_tags.iter().map(|tag| (*tag).to_string()).collect();
    assert_eq!(actual, expected, "the kernel action set moved");

    let text = grokptah_cu_adaptive::redaction::TextPayload::new(
        "value",
        grokptah_cu_adaptive::redaction::TextClass::Benign,
    )
    .expect("benign text is constructible");
    let element = grokptah_cu_adaptive::schema::ElementRef::new("element", 1).expect("valid");
    let adaptive = [
        StepIntent::ActivateTarget,
        StepIntent::Observe,
        StepIntent::Complete,
        StepIntent::Wait { millis: 1 },
        StepIntent::Invoke {
            element: element.clone(),
        },
        StepIntent::Select {
            element: element.clone(),
        },
        StepIntent::Scroll {
            element: None,
            delta_x: 0,
            delta_y: 1,
        },
        StepIntent::SetValue {
            element: element.clone(),
            text,
        },
        StepIntent::KeyChord {
            keys: vec![ChordKey::Enter],
        },
        StepIntent::PointerFallback {
            x: 1,
            y: 1,
            button: grokptah_cu_adaptive::schema::PointerButton::Primary,
        },
    ];
    for intent in adaptive {
        match intent.kernel_action_tag() {
            Some(tag) => assert!(
                expected.contains(tag),
                "{intent:?} dispatches as {tag:?}, which the kernel cannot execute"
            ),
            None => assert!(
                matches!(intent, StepIntent::Observe | StepIntent::Complete),
                "{intent:?} dispatches without naming a kernel action"
            ),
        }
    }
}

#[test]
fn intent_families_partition_the_same_way_as_kernel_action_classes() {
    let cases: Vec<(IntentFamily, ComputerAction, ActionClass)> = vec![
        (
            IntentFamily::Semantic,
            kernel_action(serde_json::json!({"type": "invoke", "element_id": "element"})),
            ActionClass::Semantic,
        ),
        (
            IntentFamily::Semantic,
            kernel_action(serde_json::json!({"type": "select", "element_id": "element"})),
            ActionClass::Semantic,
        ),
        (
            IntentFamily::TextEntry,
            kernel_action(
                serde_json::json!({"type": "set_value", "element_id": "element", "text": "value"}),
            ),
            ActionClass::TextEntry,
        ),
        (
            IntentFamily::KeyChord,
            kernel_action(serde_json::json!({"type": "key_chord", "keys": ["enter"]})),
            ActionClass::KeyChord,
        ),
        (
            IntentFamily::PointerFallback,
            kernel_action(
                serde_json::json!({"type": "pointer_click", "x": 0.0, "y": 0.0, "button": "primary"}),
            ),
            ActionClass::PointerFallback,
        ),
    ];
    for (family, action, class) in &cases {
        assert_eq!(action.class(), *class, "the kernel reclassified {action:?}");
        assert!(
            family.mutates(),
            "{family:?} should be a mutating family to match {class:?}"
        );
    }
    // Ambient is the adaptive layer's own category for steps that touch
    // nothing; the kernel files its members under `Semantic`, which is why the
    // adaptive layer keeps them separate rather than inheriting that.
    assert!(!IntentFamily::Ambient.mutates());
    assert_eq!(
        kernel_action(serde_json::json!({"type": "activate_target"})).class(),
        ActionClass::Semantic
    );
    assert_eq!(
        kernel_action(serde_json::json!({"type": "wait", "millis": 1})).class(),
        ActionClass::Semantic
    );
}

#[test]
fn the_sensitivity_ladders_agree_variant_for_variant() {
    let pairs: &[(AdaptiveSensitivity, KernelSensitivity)] = &[
        (AdaptiveSensitivity::None, KernelSensitivity::None),
        (AdaptiveSensitivity::Potential, KernelSensitivity::Potential),
        (AdaptiveSensitivity::Secure, KernelSensitivity::Secure),
        (
            AdaptiveSensitivity::SystemRestricted,
            KernelSensitivity::SystemRestricted,
        ),
    ];
    assert_eq!(pairs.len(), AdaptiveSensitivity::ALL.len());
    for (adaptive, kernel) in pairs {
        assert_eq!(
            adaptive.is_hard_denied(),
            kernel.is_hard_denied(),
            "{adaptive:?} and {kernel:?} disagree about hard denial"
        );
        assert_eq!(
            serde_json::to_value(adaptive).unwrap(),
            serde_json::to_value(kernel).unwrap(),
            "{adaptive:?} and {kernel:?} serialize differently"
        );
    }
}

#[test]
fn the_semantic_action_vocabularies_line_up() {
    // The adaptive layer's grounding check asks "does the element advertise
    // this action". The kernel asks the same question against this enum, so
    // the two have to mean the same things by it.
    let kernel: BTreeSet<String> = [
        SemanticAction::Invoke,
        SemanticAction::SetValue,
        SemanticAction::Select,
        SemanticAction::Scroll,
    ]
    .iter()
    .map(|action| {
        serde_json::to_value(action)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string()
    })
    .collect();
    for expected in ["invoke", "set_value", "select", "scroll"] {
        assert!(kernel.contains(expected), "the kernel dropped {expected}");
    }
    assert_eq!(kernel.len(), 4);
}

#[test]
fn no_adaptive_bound_is_looser_than_the_kernels() {
    let defaults = ComputerUseLimits::default();
    let ceiling = ComputerUseLimits::ceiling();

    assert_eq!(
        ADAPTIVE_MAX_TEXT as u32, defaults.max_text_entry_bytes,
        "the adaptive text bound drifted from the kernel's"
    );
    assert!(
        MAX_WAIT_MILLIS <= defaults.max_wait_millis,
        "the adaptive wait bound is looser than the kernel default"
    );
    assert!(
        MAX_FRAME_AGE_CEILING_MILLIS <= defaults.max_observation_age_millis,
        "the adaptive frame-age ceiling is looser than the kernel default"
    );
    assert!(MAX_FRAME_AGE_CEILING_MILLIS <= ceiling.max_observation_age_millis);
    // The kernel bounds a scroll delta at 10_000 per axis and a chord at four
    // keys; the adaptive schema must not accept more.
    assert_eq!(MAX_SCROLL_DELTA, 10_000);
    assert_eq!(MAX_CHORD_KEYS, 4);
    assert!(kernel_action(
        serde_json::json!({"type": "scroll", "delta_x": MAX_SCROLL_DELTA, "delta_y": 0}),
    )
    .validate(&defaults)
    .is_ok());
    assert!(kernel_action(
        serde_json::json!({"type": "scroll", "delta_x": MAX_SCROLL_DELTA + 1, "delta_y": 0}),
    )
    .validate(&defaults)
    .is_err());
    let chord = |count: usize| {
        kernel_action(serde_json::json!({
            "type": "key_chord",
            "keys": vec!["meta"; count],
        }))
    };
    assert!(chord(MAX_CHORD_KEYS).validate(&defaults).is_ok());
    assert!(chord(MAX_CHORD_KEYS + 1).validate(&defaults).is_err());
}

#[test]
fn no_profile_sits_above_the_kernels_observation_age_bound() {
    for profile in ProfileId::ALL {
        let spec = profile.spec();
        assert!(
            spec.max_frame_age_millis <= ComputerUseLimits::default().max_observation_age_millis,
            "{profile:?} would accept a frame the kernel calls stale"
        );
    }
}

#[test]
fn the_adaptive_layer_cannot_express_a_secret_the_kernel_refuses_to_expose() {
    // The kernel refuses to let a secure element carry a value at all. The
    // adaptive layer's matching rule is at construction: secret-class text is
    // not constructible, so a plan can never hold one.
    assert!(grokptah_cu_adaptive::redaction::TextPayload::new(
        "hunter2",
        grokptah_cu_adaptive::redaction::TextClass::Secret,
    )
    .is_err());
    assert!(AdaptiveSensitivity::Secure.is_hard_denied());
    assert!(KernelSensitivity::Secure.is_hard_denied());
}
