//! Public projections: redaction, credential exclusion, and progress counts.

mod support;

use grokptah_swarm_control_plane::{
    CredentialRef, DispatchProbe, EvidenceEntry, MAX_PROJECTED_EVIDENCE_BYTES,
    MAX_PROJECTED_LINE_BYTES, QuorumRule, SwarmController, SwarmLifecycle, TaskOutcome, TaskState,
    project_evidence, project_progress,
};
use support::*;

/// A key shaped like the ones the repository's sanitizer already recognizes.
const XAI_KEY: &str = "xai-abcdefghij0123456789ABCD";
const GITHUB_TOKEN: &str = "ghp_abcdefghijklmnopqrstuvwxyz012345";
const REDACTED: &str = "[REDACTED_SECRET]";

#[test]
fn a_public_projection_carries_no_credential_reference() {
    let mut spec = single_task_spec();
    spec.workers[0].credential_ref =
        Some(CredentialRef::parse("grok-api-key-slot").expect("valid reference"));
    let swarm = SwarmController::new(spec, at(0)).expect("valid");

    let projection = project_progress(swarm.state());
    let json = serde_json::to_string(&projection).expect("projection serializes");

    assert!(
        !json.contains("grok-api-key-slot"),
        "a credential reference must never reach a public projection: {json}"
    );
    assert!(
        !json.contains("credentialRef"),
        "the projection has no field that could carry a credential: {json}"
    );
    // The specification itself still holds the reference for the host to use.
    assert!(swarm.spec().workers[0].credential_ref.is_some());
}

#[test]
fn secrets_in_operator_and_worker_text_are_scrubbed() {
    let mut spec = single_task_spec();
    spec.objective = format!("ship the slice using {XAI_KEY} for the gateway");
    spec.tasks[0].title = format!("configure {GITHUB_TOKEN}");
    let mut swarm = SwarmController::new(spec, at(0)).expect("valid");

    let intent = swarm.plan_dispatches(at(1)).remove(0);
    let record = swarm
        .record_dispatch_requested(&intent, None, at(1))
        .expect("write");
    swarm
        .record_task_outcome(
            &record.dispatch_id,
            TaskOutcome::failed(format!("auth rejected Bearer {GITHUB_TOKEN}")).with_evidence(
                vec![EvidenceEntry::new(
                    "gateway log",
                    format!("request used {XAI_KEY} and failed"),
                )],
            ),
            at(2),
        )
        .expect("outcome is legal");

    let progress = project_progress(swarm.state());
    let evidence = project_evidence(swarm.state());
    let json = format!(
        "{}{}",
        serde_json::to_string(&progress).expect("serializes"),
        serde_json::to_string(&evidence).expect("serializes")
    );

    assert!(
        !json.contains(XAI_KEY),
        "objective/evidence key leaked: {json}"
    );
    assert!(
        !json.contains(GITHUB_TOKEN),
        "title/summary token leaked: {json}"
    );
    assert!(
        progress.objective.contains(REDACTED),
        "the scrubbed objective should show the redaction marker: {}",
        progress.objective
    );
    assert!(evidence.entries[0].detail.contains(REDACTED));
    assert!(
        progress.tasks[0]
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains(REDACTED))
    );
}

#[test]
fn projected_text_is_bounded_on_a_character_boundary() {
    let mut swarm = SwarmController::new(single_task_spec(), at(0)).expect("valid");
    let intent = swarm.plan_dispatches(at(1)).remove(0);
    let record = swarm
        .record_dispatch_requested(&intent, None, at(1))
        .expect("write");

    // Three-byte characters, so a naive byte cut would land mid-character.
    let detail = "\u{2603}".repeat(600);
    let summary = "\u{2603}".repeat(400);
    swarm
        .record_task_outcome(
            &record.dispatch_id,
            TaskOutcome::failed(summary)
                .with_evidence(vec![EvidenceEntry::new("snowfield", detail)]),
            at(2),
        )
        .expect("outcome is legal");

    let progress = project_progress(swarm.state());
    let evidence = project_evidence(swarm.state());

    let projected_summary = progress.tasks[0].summary.as_deref().expect("summary");
    assert!(projected_summary.len() <= MAX_PROJECTED_LINE_BYTES + '…'.len_utf8());
    assert!(projected_summary.ends_with('…'));

    let projected_detail = &evidence.entries[0].detail;
    assert!(projected_detail.len() <= MAX_PROJECTED_EVIDENCE_BYTES + '…'.len_utf8());
    assert!(projected_detail.ends_with('…'));
    // Reaching here at all proves no panic on a mid-character byte bound.
    assert!(
        projected_detail
            .chars()
            .all(|c| c == '\u{2603}' || c == '…')
    );
}

#[test]
fn progress_counts_track_the_graph() {
    let mut swarm =
        SwarmController::new(diamond_spec(QuorumRule::Unanimous), at(0)).expect("valid");
    let start = project_progress(swarm.state());
    assert_eq!(start.counts.ready, 1);
    assert_eq!(start.counts.pending, 5);
    assert_eq!(start.in_flight, 0);
    assert_eq!(start.max_in_flight, 4);
    assert!(!start.needs_operator_attention);

    run_task(&mut swarm, "t-root", TaskOutcome::succeeded(), at(1));
    let after = project_progress(swarm.state());
    assert_eq!(after.counts.succeeded, 1);
    assert_eq!(after.counts.ready, 2, "both branches opened");
    assert_eq!(after.total_dispatches, 1);
    assert_eq!(after.lifecycle, SwarmLifecycle::Active);
}

#[test]
fn an_uncertain_dispatch_raises_the_operator_flag() {
    let mut swarm = SwarmController::new(single_task_spec(), at(0)).expect("valid");
    let intent = swarm.plan_dispatches(at(1)).remove(0);
    swarm
        .record_dispatch_requested(&intent, None, at(1))
        .expect("write");
    swarm
        .record_dispatch_uncertain(&intent.dispatch_id, "the spawn reply was lost", at(2))
        .expect("uncertainty is recordable");

    let projection = project_progress(swarm.state());
    assert!(projection.needs_operator_attention);
    assert_eq!(projection.counts.dispatch_uncertain, 1);
    assert_eq!(projection.in_flight, 1, "capacity stays reserved");
    assert_eq!(projection.tasks[0].state, TaskState::DispatchUncertain);

    swarm
        .reconcile_uncertain(&intent.dispatch_id, DispatchProbe::NotStarted, at(3))
        .expect("evidence arrives");
    assert!(!project_progress(swarm.state()).needs_operator_attention);
}

#[test]
fn a_row_shows_the_isolation_and_lease_a_dispatch_ran_under() {
    let mut spec = single_task_spec();
    spec.workers = vec![computer_use_worker()];
    spec.tasks[0].worker_id = worker_id("cu-cursor");
    spec.tasks[0].requires_computer_use = true;
    let mut swarm = SwarmController::new(spec, at(0)).expect("valid");

    let intent = swarm.plan_dispatches(at(1)).remove(0);
    swarm
        .record_dispatch_requested(&intent, Some(lease("lease-visible", 0, 600)), at(1))
        .expect("write");

    let projection = project_progress(swarm.state());
    let row = &projection.tasks[0];
    assert!(row.requires_computer_use);
    assert_eq!(
        row.isolation,
        grokptah_swarm_control_plane::IsolationRequirement::Worktree
    );
    assert_eq!(
        row.computer_use_lease.as_ref().map(|id| id.as_str()),
        Some("lease-visible")
    );
    assert_eq!(row.provider.as_str(), "cursor");
    assert_eq!(row.model.as_str(), "cursor-composer-1");
}

#[test]
fn a_projection_round_trips_on_the_wire() {
    let swarm = SwarmController::new(diamond_spec(QuorumRule::Unanimous), at(0)).expect("valid");
    let projection = project_progress(swarm.state());
    let json = serde_json::to_string(&projection).expect("serializes");
    let restored: grokptah_swarm_control_plane::SwarmProgressProjection =
        serde_json::from_str(&json).expect("deserializes");
    assert_eq!(restored, projection);
}
