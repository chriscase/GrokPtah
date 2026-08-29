//! Adversarial matrix for the untrusted-model-output boundary.
//!
//! Every input here is synthetic. No provider is contacted, no TCC prompt is
//! raised, no VM is booted, and nothing is signed or published.

use std::collections::BTreeSet;

use chrono::{Duration, Utc};
use grokptah_agent_bridge::computer_boundary::{
    accept_model_proposal, note_turn, AcceptedProposal, BoundaryOutcome, BoundaryRejection,
    CompletionVerification, ProposalTicket, MAX_PROPOSAL_BYTES, MAX_STATIONARY_STRIKES,
};
use grokptah_agent_bridge::computer_use::{
    AdaptiveProfile, AdaptiveRecord, ComputerAction, ComputerErrorCode, ComputerObservation,
    ComputerTarget, ObservationGeometry, SemanticAction, SemanticElement, Sensitivity,
};

const CHALLENGE: [u8; 32] = [7u8; 32];
const EPOCH: u64 = 3;

fn observation() -> ComputerObservation {
    ComputerObservation {
        observation_id: "obs-1".into(),
        sequence: 11,
        target: ComputerTarget {
            app_id: "com.grokptah.demo".into(),
            window_id: "main".into(),
            generation: 1,
            display_name: "Demo".into(),
            sensitivity: Sensitivity::None,
        },
        captured_at: Utc::now(),
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
            actions: BTreeSet::from([SemanticAction::SetValue, SemanticAction::Invoke]),
        }],
        elements_truncated: false,
        sensitivity: Sensitivity::None,
    }
}

fn ticket(observation: &ComputerObservation) -> ProposalTicket {
    ProposalTicket::mint(
        "run-1",
        observation,
        EPOCH,
        AdaptiveProfile::Balanced,
        Utc::now(),
        Duration::seconds(30),
        CHALLENGE,
    )
}

fn record() -> AdaptiveRecord {
    AdaptiveRecord::open(AdaptiveProfile::Balanced, "balanced", Utc::now())
}

/// A well-formed body that answers `ticket` exactly.
fn good_body(ticket: &ProposalTicket) -> String {
    format!(
        r#"{{"proposalId":"{}","challenge":"{}","observationId":"{}","sequence":{},"decision":"act","action":{{"type":"set_value","element_id":"field","text":"Ada"}},"summary":"fill the name field"}}"#,
        ticket.proposal_id(),
        ticket.challenge_for_prompt(),
        ticket.observation_id(),
        ticket.sequence(),
    )
}

fn accept(
    ticket: &ProposalTicket,
    body: &str,
    observation: &ComputerObservation,
    record: &AdaptiveRecord,
) -> Result<BoundaryOutcome, BoundaryRejection> {
    accept_model_proposal(
        ticket,
        body.as_bytes(),
        observation,
        EPOCH,
        record,
        None,
        Utc::now(),
    )
}

// ---------------------------------------------------------------------------
// Baseline
// ---------------------------------------------------------------------------

#[test]
fn a_well_formed_proposal_answering_the_exact_ticket_is_accepted() {
    let observation = observation();
    let ticket = ticket(&observation);
    let outcome = accept(&ticket, &good_body(&ticket), &observation, &record()).unwrap();
    let BoundaryOutcome::Act(accepted) = outcome else {
        panic!("expected an action");
    };
    assert_eq!(
        accepted.action,
        ComputerAction::SetValue {
            element_id: "field".into(),
            text: "Ada".into()
        }
    );
    assert_eq!(accepted.evidence.proposal_id, ticket.proposal_id());
    assert_eq!(accepted.evidence.control_epoch, EPOCH);
}

// ---------------------------------------------------------------------------
// Fabricated DTOs
// ---------------------------------------------------------------------------

#[test]
fn a_fabricated_proposal_id_is_refused() {
    let observation = observation();
    let ticket = ticket(&observation);
    let body =
        good_body(&ticket).replace(ticket.proposal_id(), "00000000-dead-beef-0000-000000000000");
    assert_eq!(
        accept(&ticket, &body, &observation, &record()).unwrap_err(),
        BoundaryRejection::ProposalIdMismatch
    );
}

#[test]
fn a_proposal_that_never_saw_the_challenge_is_refused() {
    let observation = observation();
    let ticket = ticket(&observation);
    let body = good_body(&ticket).replace(ticket.challenge_for_prompt(), &"0".repeat(64));
    assert_eq!(
        accept(&ticket, &body, &observation, &record()).unwrap_err(),
        BoundaryRejection::ChallengeMismatch
    );
}

#[test]
fn a_ticket_cannot_be_forged_from_wire_bytes() {
    // The type implements neither Serialize nor Deserialize and its challenge
    // field is private, so the only way to obtain one is `mint`. This test
    // pins the observable half of that: the challenge never appears in any
    // serialized form the host produces for a surface.
    let observation = observation();
    let ticket = ticket(&observation);
    let accepted = match accept(&ticket, &good_body(&ticket), &observation, &record()).unwrap() {
        BoundaryOutcome::Act(accepted) => accepted,
        other => panic!("expected an action, got {other:?}"),
    };
    let projected = serde_json::to_string(&accepted.evidence.project()).unwrap();
    assert!(!projected.contains(ticket.challenge_for_prompt()));
    assert!(!projected.contains("challengeDigest"));
    assert!(!projected.contains("actionDigest"));
}

#[test]
fn a_replayed_response_from_an_earlier_turn_is_refused() {
    let observation = observation();
    let first = ticket(&observation);
    let body = good_body(&first);
    // A second turn mints a fresh ticket with a different challenge.
    let second = ProposalTicket::mint(
        "run-1",
        &observation,
        EPOCH,
        AdaptiveProfile::Balanced,
        Utc::now(),
        Duration::seconds(30),
        [9u8; 32],
    );
    assert_eq!(
        accept(&second, &body, &observation, &record()).unwrap_err(),
        BoundaryRejection::ProposalIdMismatch
    );
}

// ---------------------------------------------------------------------------
// Malformed / unknown-field / coercion
// ---------------------------------------------------------------------------

#[test]
fn prose_and_fenced_json_are_refused_without_recovery() {
    let observation = observation();
    let ticket = ticket(&observation);
    let inner = good_body(&ticket);
    for body in [
        "I think you should click the button.".to_string(),
        format!("```json\n{inner}\n```"),
        format!("Here is my answer:\n{inner}"),
        format!("{inner}\ntrailing prose"),
    ] {
        let rejection = accept(&ticket, &body, &observation, &record()).unwrap_err();
        assert!(
            matches!(
                rejection,
                BoundaryRejection::NotJson | BoundaryRejection::TrailingContent
            ),
            "{body} produced {rejection:?}"
        );
    }
}

#[test]
fn a_duplicate_key_is_refused_rather_than_last_write_wins() {
    let observation = observation();
    let ticket = ticket(&observation);
    // Two summaries: serde_json alone would silently keep the last one.
    let body = good_body(&ticket);
    let body = format!("{{\"summary\":\"benign\",{}", &body[1..]);
    assert_eq!(
        accept(&ticket, &body, &observation, &record()).unwrap_err(),
        BoundaryRejection::DuplicateKey
    );
}

#[test]
fn a_duplicate_key_nested_inside_the_action_is_also_refused() {
    let observation = observation();
    let ticket = ticket(&observation);
    let body = good_body(&ticket).replace(
        r#"{"type":"set_value","element_id":"field","text":"Ada"}"#,
        r#"{"type":"set_value","element_id":"field","text":"Ada","text":"rm -rf"}"#,
    );
    assert_eq!(
        accept(&ticket, &body, &observation, &record()).unwrap_err(),
        BoundaryRejection::DuplicateKey
    );
}

#[test]
fn an_unknown_top_level_field_is_refused() {
    let observation = observation();
    let ticket = ticket(&observation);
    let body = good_body(&ticket).replace(r#","summary":"#, r#","authority":"granted","summary":"#);
    assert_eq!(
        accept(&ticket, &body, &observation, &record()).unwrap_err(),
        BoundaryRejection::UnknownField
    );
}

#[test]
fn an_unknown_field_inside_the_action_is_refused() {
    let observation = observation();
    let ticket = ticket(&observation);
    let body = good_body(&ticket).replace(r#""text":"Ada"}"#, r#""text":"Ada","force":true}"#);
    assert_eq!(
        accept(&ticket, &body, &observation, &record()).unwrap_err(),
        BoundaryRejection::UnknownField
    );
}

#[test]
fn a_coerced_numeric_string_is_refused() {
    let observation = observation();
    let ticket = ticket(&observation);
    let body = good_body(&ticket).replace(
        &format!(r#""sequence":{}"#, ticket.sequence()),
        r#""sequence":"11""#,
    );
    assert_eq!(
        accept(&ticket, &body, &observation, &record()).unwrap_err(),
        BoundaryRejection::WrongType
    );
}

#[test]
fn an_unknown_action_type_is_refused() {
    let observation = observation();
    let ticket = ticket(&observation);
    let body = good_body(&ticket).replace(r#""type":"set_value""#, r#""type":"run_shell""#);
    assert_eq!(
        accept(&ticket, &body, &observation, &record()).unwrap_err(),
        BoundaryRejection::UnknownAction
    );
}

#[test]
fn operator_only_actions_are_named_as_such_not_as_unknown() {
    let observation = observation();
    let ticket = ticket(&observation);
    for action in [
        r#"{"type":"pointer_click","x":10.0,"y":10.0,"button":"primary"}"#,
        r#"{"type":"key_chord","keys":["enter"]}"#,
    ] {
        let body = good_body(&ticket).replace(
            r#"{"type":"set_value","element_id":"field","text":"Ada"}"#,
            action,
        );
        assert_eq!(
            accept(&ticket, &body, &observation, &record()).unwrap_err(),
            BoundaryRejection::OperatorOnlyAction,
            "{action}"
        );
    }
}

#[test]
fn an_oversized_body_is_refused_before_parsing() {
    let observation = observation();
    let ticket = ticket(&observation);
    let body = "x".repeat(MAX_PROPOSAL_BYTES + 1);
    assert_eq!(
        accept(&ticket, &body, &observation, &record()).unwrap_err(),
        BoundaryRejection::TooLarge
    );
}

// ---------------------------------------------------------------------------
// Stale observation / revision
// ---------------------------------------------------------------------------

#[test]
fn a_proposal_against_a_superseded_observation_is_refused() {
    let observation = observation();
    let ticket = ticket(&observation);
    let body = good_body(&ticket);
    // The host advanced to a new observation before the model answered.
    let mut fresh = observation.clone();
    fresh.observation_id = "obs-2".into();
    assert_eq!(
        accept(&ticket, &body, &fresh, &record()).unwrap_err(),
        BoundaryRejection::ObservationMismatch
    );
}

#[test]
fn a_revision_bump_on_the_same_observation_id_is_refused() {
    let observation = observation();
    let ticket = ticket(&observation);
    let body = good_body(&ticket);
    let mut bumped = observation.clone();
    bumped.sequence += 1;
    assert_eq!(
        accept(&ticket, &body, &bumped, &record()).unwrap_err(),
        BoundaryRejection::SequenceMismatch
    );
}

#[test]
fn a_model_echoing_a_different_sequence_is_refused() {
    let observation = observation();
    let ticket = ticket(&observation);
    let body = good_body(&ticket).replace(
        &format!(r#""sequence":{}"#, ticket.sequence()),
        r#""sequence":99"#,
    );
    assert_eq!(
        accept(&ticket, &body, &observation, &record()).unwrap_err(),
        BoundaryRejection::SequenceMismatch
    );
}

#[test]
fn an_element_absent_from_the_current_observation_is_refused() {
    let observation = observation();
    let ticket = ticket(&observation);
    let body = good_body(&ticket).replace(r#""element_id":"field""#, r#""element_id":"ghost""#);
    assert_eq!(
        accept(&ticket, &body, &observation, &record()).unwrap_err(),
        BoundaryRejection::UnknownElement
    );
}

#[test]
fn an_expired_ticket_is_refused() {
    let observation = observation();
    let issued = Utc::now() - Duration::seconds(120);
    let ticket = ProposalTicket::mint(
        "run-1",
        &observation,
        EPOCH,
        AdaptiveProfile::Balanced,
        issued,
        Duration::seconds(30),
        CHALLENGE,
    );
    let rejection = accept_model_proposal(
        &ticket,
        good_body(&ticket).as_bytes(),
        &observation,
        EPOCH,
        &record(),
        None,
        Utc::now(),
    )
    .unwrap_err();
    assert_eq!(rejection, BoundaryRejection::TicketExpired);
}

// ---------------------------------------------------------------------------
// Lease loss
// ---------------------------------------------------------------------------

#[test]
fn a_control_epoch_move_strands_an_outstanding_proposal() {
    let observation = observation();
    let ticket = ticket(&observation);
    // Pause, takeover, stop, or restart advanced the epoch mid-flight.
    let rejection = accept_model_proposal(
        &ticket,
        good_body(&ticket).as_bytes(),
        &observation,
        EPOCH + 1,
        &record(),
        None,
        Utc::now(),
    )
    .unwrap_err();
    assert_eq!(rejection, BoundaryRejection::LeaseLost);
    assert_eq!(rejection.error_code(), ComputerErrorCode::Conflict);
}

#[test]
fn lease_loss_is_checked_before_the_body_is_parsed() {
    let observation = observation();
    let ticket = ticket(&observation);
    // Even a body that could never parse must report the lease, not the syntax:
    // a fenced run has no business paying to inspect untrusted bytes.
    let rejection = accept_model_proposal(
        &ticket,
        b"this is not json at all",
        &observation,
        EPOCH + 1,
        &record(),
        None,
        Utc::now(),
    )
    .unwrap_err();
    assert_eq!(rejection, BoundaryRejection::LeaseLost);
}

// ---------------------------------------------------------------------------
// No-op / stationarity
// ---------------------------------------------------------------------------

#[test]
fn a_repeated_action_eventually_stops_the_run_for_no_progress() {
    let observation = observation();
    let mut record = record();
    let mut last: Option<Box<AcceptedProposal>> = None;

    // Same action, fresh ticket each turn: the run makes no progress.
    for turn in 0..=MAX_STATIONARY_STRIKES {
        let ticket = ProposalTicket::mint(
            "run-1",
            &observation,
            EPOCH,
            AdaptiveProfile::Balanced,
            Utc::now(),
            Duration::seconds(30),
            [turn as u8; 32],
        );
        match accept(&ticket, &good_body(&ticket), &observation, &record) {
            Ok(BoundaryOutcome::Act(accepted)) => {
                note_turn(&mut record, Ok(&accepted));
                last = Some(accepted);
            }
            other => panic!("turn {turn} should still be admitted, got {other:?}"),
        }
    }
    assert!(last.is_some());
    assert_eq!(record.stationary_strikes, MAX_STATIONARY_STRIKES);

    // One more repeat is refused.
    let ticket = ProposalTicket::mint(
        "run-1",
        &observation,
        EPOCH,
        AdaptiveProfile::Balanced,
        Utc::now(),
        Duration::seconds(30),
        [200u8; 32],
    );
    let rejection = accept(&ticket, &good_body(&ticket), &observation, &record).unwrap_err();
    assert_eq!(rejection, BoundaryRejection::Stationary);
    assert_eq!(rejection.error_code(), ComputerErrorCode::LimitReached);
}

#[test]
fn a_different_action_clears_the_no_progress_counter() {
    let observation = observation();
    let mut record = record();
    record.stationary_strikes = MAX_STATIONARY_STRIKES;
    record.last_action_digest = Some("stale-digest".into());

    let ticket = ticket(&observation);
    let accepted = match accept(&ticket, &good_body(&ticket), &observation, &record).unwrap() {
        BoundaryOutcome::Act(accepted) => accepted,
        other => panic!("expected an action, got {other:?}"),
    };
    note_turn(&mut record, Ok(&accepted));
    assert_eq!(record.stationary_strikes, 0);
}

#[test]
fn a_refused_turn_still_costs_an_attempt() {
    let mut record = record();
    note_turn(&mut record, Err(()));
    note_turn(&mut record, Err(()));
    assert_eq!(record.spend.attempts, 2);
    assert_eq!(record.spend.accepted, 0);
    assert_eq!(record.spend.rejected, 2);
    assert!(record.spend.is_balanced());
    assert_eq!(record.spend.reported_tokens, None, "never fabricate a zero");
}

#[test]
fn an_exhausted_budget_refuses_before_parsing() {
    let observation = observation();
    let ticket = ticket(&observation);
    let mut record = AdaptiveRecord::open(AdaptiveProfile::Economy, "economy", Utc::now());
    record.spend.attempts = AdaptiveProfile::Economy.budget().max_model_turns;
    assert_eq!(
        accept(&ticket, &good_body(&ticket), &observation, &record).unwrap_err(),
        BoundaryRejection::BudgetExhausted
    );
}

// ---------------------------------------------------------------------------
// Model-authored completion
// ---------------------------------------------------------------------------

#[test]
fn model_authored_completion_without_host_verification_is_refused() {
    let observation = observation();
    let ticket = ticket(&observation);
    let body = format!(
        r#"{{"proposalId":"{}","challenge":"{}","observationId":"{}","sequence":{},"decision":"complete","summary":"all done"}}"#,
        ticket.proposal_id(),
        ticket.challenge_for_prompt(),
        ticket.observation_id(),
        ticket.sequence(),
    );
    let rejection = accept(&ticket, &body, &observation, &record()).unwrap_err();
    assert_eq!(rejection, BoundaryRejection::CompletionNotHostVerified);
    assert_eq!(rejection.error_code(), ComputerErrorCode::UncertainOutcome);

    // A host observation that found the postcondition unmet is not a pass.
    let rejection = accept_model_proposal(
        &ticket,
        body.as_bytes(),
        &observation,
        EPOCH,
        &record(),
        Some(CompletionVerification::observed(false)),
        Utc::now(),
    )
    .unwrap_err();
    assert_eq!(rejection, BoundaryRejection::CompletionNotHostVerified);

    // Only an exact host verification admits completion.
    let outcome = accept_model_proposal(
        &ticket,
        body.as_bytes(),
        &observation,
        EPOCH,
        &record(),
        Some(CompletionVerification::observed(true)),
        Utc::now(),
    )
    .unwrap();
    assert!(matches!(outcome, BoundaryOutcome::Complete { .. }));
}

#[test]
fn a_completion_carrying_an_action_is_refused() {
    let observation = observation();
    let ticket = ticket(&observation);
    let body = good_body(&ticket).replace(r#""decision":"act""#, r#""decision":"complete""#);
    assert_eq!(
        accept_model_proposal(
            &ticket,
            body.as_bytes(),
            &observation,
            EPOCH,
            &record(),
            Some(CompletionVerification::observed(true)),
            Utc::now(),
        )
        .unwrap_err(),
        BoundaryRejection::CompletionCarriedAction
    );
}

// ---------------------------------------------------------------------------
// Simulator / live-claim confusion
// ---------------------------------------------------------------------------

#[test]
fn the_boundary_never_widens_what_the_kernel_would_allow() {
    // Acceptance yields a plain ComputerAction with no extra authority: the
    // action a caller hands to the kernel is exactly the closed-grammar value,
    // carrying no grant, no class override, and no bypass token.
    let observation = observation();
    let ticket = ticket(&observation);
    let accepted = match accept(&ticket, &good_body(&ticket), &observation, &record()).unwrap() {
        BoundaryOutcome::Act(accepted) => accepted,
        other => panic!("expected an action, got {other:?}"),
    };
    let rendered = serde_json::to_string(&accepted.action).unwrap();
    for forbidden in ["grant", "authority", "bypass", "class", "epoch"] {
        assert!(
            !rendered.contains(forbidden),
            "{rendered} must not carry {forbidden}"
        );
    }
}
