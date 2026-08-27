//! The plan and verdict schema is closed, bounded, and content-free.
//!
//! Everything here is about what a *parser* will accept, because that is the
//! boundary a model's output crosses. A schema that quietly ignores an
//! unrecognized key is a schema that lets a newer planner's extra argument
//! through unvalidated; one that accepts an unbounded string is a schema that
//! lets a plan exhaust a budget by being enormous.

mod common;

use common::{Fixture, element, frame, invoke_step, plan_for};
use grokptah_cu_adaptive::confidence::{AmbiguityAssessment, Disposition, Reversibility};
use grokptah_cu_adaptive::digest::{digest_str, domain, is_digest};
use grokptah_cu_adaptive::grounding::GroundingClaim;
use grokptah_cu_adaptive::profile::ProfileId;
use grokptah_cu_adaptive::redaction::{MAX_TEXT_ENTRY_BYTES, TextClass, TextPayload};
use grokptah_cu_adaptive::schema::{
    ADAPTIVE_SCHEMA_VERSION, ChordKey, ElementRef, MAX_CHORD_KEYS, MAX_SCROLL_DELTA,
    MAX_WAIT_MILLIS, PointerButton, Postcondition, StepIntent,
};
use grokptah_cu_adaptive::tier::ModelTier;
use grokptah_cu_adaptive::vocabulary::{
    ApprovalReason, DenyReason, EscalationReason, NotClaimed, StopReason,
};

#[test]
fn an_unrecognized_intent_is_refused_not_ignored() {
    for candidate in [
        serde_json::json!({"intent": "run_shell", "argv": ["sh", "-c", "id"]}),
        serde_json::json!({"intent": "read_clipboard"}),
        serde_json::json!({"intent": "screenshot", "path": "/tmp/out.png"}),
        serde_json::json!({"intent": "invoke_v2", "element": {"elementId": "a", "generation": 1}}),
    ] {
        let parsed: Result<StepIntent, _> = serde_json::from_value(candidate.clone());
        assert!(parsed.is_err(), "{candidate} parsed as a known intent");
    }
}

#[test]
fn an_extra_argument_on_a_known_intent_is_refused() {
    let smuggled = serde_json::json!({
        "intent": "invoke",
        "element": {"elementId": "save-button", "generation": 1},
        "andAlso": {"intent": "pointer_fallback", "x": 0, "y": 0, "button": "primary"}
    });
    assert!(serde_json::from_value::<StepIntent>(smuggled).is_err());

    let extra_field_on_element = serde_json::json!({
        "intent": "invoke",
        "element": {"elementId": "save-button", "generation": 1, "handle": 140_735_000}
    });
    assert!(serde_json::from_value::<StepIntent>(extra_field_on_element).is_err());
}

#[test]
fn every_closed_vocabulary_rejects_an_unknown_slug() {
    assert!(serde_json::from_str::<DenyReason>("\"unknown\"").is_err());
    assert!(serde_json::from_str::<EscalationReason>("\"unknown\"").is_err());
    assert!(serde_json::from_str::<ApprovalReason>("\"unknown\"").is_err());
    assert!(serde_json::from_str::<StopReason>("\"unknown\"").is_err());
    assert!(serde_json::from_str::<NotClaimed>("\"unknown\"").is_err());
    assert!(serde_json::from_str::<ChordKey>("\"f13\"").is_err());
    assert!(serde_json::from_str::<PointerButton>("\"middle\"").is_err());
}

#[test]
fn a_chord_cannot_carry_printable_text() {
    // Text goes through `set_value`, where it is redactable. A chord that
    // could name a character would be an unredactable typing channel.
    for candidate in ["\"a\"", "\"1\"", "\"comma\"", "\"key_a\""] {
        assert!(
            serde_json::from_str::<ChordKey>(candidate).is_err(),
            "{candidate} parsed as a chord key"
        );
    }
    assert!(serde_json::from_str::<ChordKey>("\"enter\"").is_ok());
}

#[test]
fn every_per_step_bound_is_enforced_at_its_edge() {
    assert!(
        StepIntent::Wait {
            millis: MAX_WAIT_MILLIS
        }
        .validate()
        .is_ok()
    );
    assert_eq!(
        StepIntent::Wait {
            millis: MAX_WAIT_MILLIS + 1
        }
        .validate()
        .unwrap_err(),
        DenyReason::SchemaViolation
    );

    assert!(
        StepIntent::KeyChord {
            keys: vec![ChordKey::Meta; MAX_CHORD_KEYS]
        }
        .validate()
        .is_ok()
    );
    assert_eq!(
        StepIntent::KeyChord {
            keys: vec![ChordKey::Meta; MAX_CHORD_KEYS + 1]
        }
        .validate()
        .unwrap_err(),
        DenyReason::SchemaViolation
    );

    assert!(
        StepIntent::Scroll {
            element: None,
            delta_x: MAX_SCROLL_DELTA,
            delta_y: -MAX_SCROLL_DELTA,
        }
        .validate()
        .is_ok()
    );
    assert_eq!(
        StepIntent::Scroll {
            element: None,
            delta_x: MAX_SCROLL_DELTA + 1,
            delta_y: 0,
        }
        .validate()
        .unwrap_err(),
        DenyReason::SchemaViolation
    );
}

#[test]
fn element_references_cannot_be_paths_or_handles() {
    for candidate in ["../escape", "/absolute/path", "with space", "", "a/b", ".."] {
        assert!(
            ElementRef::new(candidate, 1).is_err(),
            "{candidate:?} was accepted as an element reference"
        );
    }
    assert!(ElementRef::new("save-button", 1).is_ok());
    assert!(ElementRef::new("a".repeat(257), 1).is_err());
}

#[test]
fn text_of_secret_class_cannot_be_put_into_a_plan_at_all() {
    assert!(TextPayload::new("hunter2", TextClass::Secret).is_err());
    // And a payload that claims to be secret after the fact fails its own
    // shape check, so a hand-built plan cannot smuggle one in.
    let json = serde_json::json!({
        "digest": digest_str(domain::TEXT_PAYLOAD, "hunter2"),
        "byteLen": 7,
        "charLen": 7,
        "class": "secret"
    });
    let payload: TextPayload = serde_json::from_value(json).expect("shape parses");
    assert!(!payload.is_well_formed());
    let step = common::step(
        StepIntent::SetValue {
            element: element(),
            text: payload,
        },
        Reversibility::Reversible,
    );
    assert_eq!(step.validate().unwrap_err(), DenyReason::SchemaViolation);
}

#[test]
fn a_plan_that_round_trips_through_json_loses_its_typed_text() {
    let text = TextPayload::new("Ada Lovelace", TextClass::Benign).unwrap();
    assert!(text.is_replayable());
    let plan = plan_for(
        ProfileId::Balanced,
        ModelTier::StrongHosted,
        common::step(
            StepIntent::SetValue {
                element: element(),
                text,
            },
            Reversibility::Reversible,
        ),
    );
    plan.validate().unwrap();

    let serialized = serde_json::to_string(&plan).unwrap();
    assert!(!serialized.contains("Ada"));
    let restored: grokptah_cu_adaptive::schema::PlanEnvelope =
        serde_json::from_str(&serialized).unwrap();
    restored.validate().unwrap();
    match &restored.steps[0].intent {
        StepIntent::SetValue { text, .. } => {
            assert!(!text.is_replayable());
            assert!(text.matches("Ada Lovelace"));
            assert!(text.dispatch_literal().is_none());
        }
        other => panic!("intent changed shape on round trip: {other:?}"),
    }
    // The digest survives, so evidence still binds even though content does
    // not.
    assert_eq!(plan.digest(), restored.digest());
}

#[test]
fn oversized_text_is_refused_before_it_costs_anything() {
    let too_long = "a".repeat(MAX_TEXT_ENTRY_BYTES + 1);
    assert!(TextPayload::new(&too_long, TextClass::Benign).is_err());
}

#[test]
fn a_plan_digest_binds_every_field_that_changes_its_meaning() {
    let baseline = plan_for(ProfileId::Balanced, ModelTier::StrongHosted, invoke_step());
    let base_digest = baseline.digest().unwrap();
    assert!(is_digest(&base_digest));

    let mut different_profile = baseline.clone();
    different_profile.profile = ProfileId::Economy;
    assert_ne!(different_profile.digest(), Some(base_digest.clone()));

    let mut different_frame = baseline.clone();
    different_frame.frame.sequence += 1;
    assert_ne!(different_frame.digest(), Some(base_digest.clone()));

    let mut different_grounding = baseline.clone();
    different_grounding.steps[0].grounding = GroundingClaim::Semantic {
        element: element(),
        role_digest: common::role_digest(),
    };
    assert_ne!(different_grounding.digest(), Some(base_digest.clone()));

    let mut different_confidence = baseline.clone();
    different_confidence.steps[0].ambiguity = AmbiguityAssessment::unambiguous(1);
    assert_ne!(different_confidence.digest(), Some(base_digest));
}

#[test]
fn a_plan_from_a_different_schema_version_is_refused() {
    let mut plan = plan_for(ProfileId::Balanced, ModelTier::StrongHosted, invoke_step());
    plan.schema_version = ADAPTIVE_SCHEMA_VERSION + 1;
    assert_eq!(plan.validate().unwrap_err(), DenyReason::SchemaViolation);
    plan.schema_version = ADAPTIVE_SCHEMA_VERSION.saturating_sub(1);
    assert_eq!(plan.validate().unwrap_err(), DenyReason::SchemaViolation);
}

#[test]
fn an_objective_never_travels_as_text() {
    let plan = plan_for(ProfileId::Balanced, ModelTier::StrongHosted, invoke_step());
    let serialized = serde_json::to_string(&plan).unwrap();
    assert!(!serialized.contains(common::OBJECTIVE));
    assert!(is_digest(&plan.objective_digest));
}

#[test]
fn a_frame_token_cannot_name_a_path() {
    let mut token = frame();
    for candidate in ["../frame", "/dev/frame", "frame with space", ""] {
        token.frame_id = candidate.into();
        assert!(
            !token.is_well_formed(),
            "{candidate:?} was accepted as a frame id"
        );
    }
}

#[test]
fn an_ambient_step_that_claims_a_postcondition_is_refused() {
    let mut fixture = Fixture::new(ProfileId::Balanced, ModelTier::StrongHosted);
    fixture.plan.steps[0].intent = StepIntent::Observe;
    fixture.plan.steps[0].grounding = GroundingClaim::None;
    fixture.plan.steps[0].expected = Postcondition::FrameChanged;
    fixture.plan_digest = fixture.plan.digest().unwrap();
    assert_eq!(
        fixture.evaluate().refusal(),
        Some(DenyReason::SchemaViolation)
    );
}

#[test]
fn a_malformed_step_is_refused_before_anything_else_is_asked() {
    // Sensitivity, staleness, and the grant are all also wrong here. Schema
    // still wins, because a step that does not parse is not a proposal about
    // which those questions have answers.
    let mut fixture = Fixture::new(ProfileId::Balanced, ModelTier::StrongHosted);
    fixture.plan.steps[0].ambiguity = AmbiguityAssessment {
        candidate_count: 1,
        top_confidence_bps: 100,
        runner_up_confidence_bps: 9_000,
    };
    if let Some(live) = fixture.live_element.as_mut() {
        live.sensitivity = grokptah_cu_adaptive::redaction::Sensitivity::Secure;
    }
    fixture.live_frame.sequence += 1;
    fixture.plan_digest = fixture.plan.digest().unwrap();
    assert_eq!(
        fixture.evaluate().refusal(),
        Some(DenyReason::SchemaViolation)
    );
}

#[test]
fn a_verdict_serializes_without_carrying_the_step_it_judged() {
    let fixture = Fixture::new(ProfileId::Balanced, ModelTier::StrongHosted);
    let verdict = fixture.evaluate();
    let serialized = serde_json::to_string(&verdict).unwrap();
    assert!(!serialized.contains(common::OBJECTIVE));
    assert!(!serialized.contains(common::ELEMENT_ROLE));
    // The verdict binds to the plan by digest, which is how a reader ties the
    // two together without the verdict repeating the plan's contents.
    assert!(serialized.contains(&fixture.plan_digest));
    assert_eq!(verdict.planner, Disposition::Commit);
}
