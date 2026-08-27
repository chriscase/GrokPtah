//! Lease, compare-and-swap, stale-frame refusal, cancellation, and cleanup.
//!
//! Two fences guard every commit and they answer different questions: the
//! lease answers "am I still driving", the frame token answers "am I still
//! looking at what I decided from". Both are checked at commit time rather
//! than at admission, because the failures that matter happen in between --
//! the operator takes over, the window is rebound, a human takes thirty
//! seconds to answer a prompt.
//!
//! Cleanup is checked for the property that is easy to get almost right:
//! idempotence. A run that releases twice must not report two releases, and a
//! run that stopped without releasing must not be able to call itself orderly.

mod common;

use common::Fixture;
use grokptah_cu_adaptive::bench::runner::{RunConfig, run};
use grokptah_cu_adaptive::bench::scenario::{Scenario, ScenarioFamily};
use grokptah_cu_adaptive::cancel::{CancelCause, CleanupLedger, ReleaseOutcome, Resource};
use grokptah_cu_adaptive::horizon::Horizon;
use grokptah_cu_adaptive::lease::{EpochBump, LeaseHolder, RunLease};
use grokptah_cu_adaptive::profile::ProfileId;
use grokptah_cu_adaptive::tier::ModelTier;
use grokptah_cu_adaptive::vocabulary::{DenyReason, StopReason};

#[test]
fn compare_and_swap_detects_a_lost_update_rather_than_applying_over_it() {
    let mut lease = RunLease::new("run-1", 100_000);
    let held = lease.version;
    assert_eq!(lease.compare_and_swap(held, 0).unwrap(), held + 1);
    assert_eq!(
        lease.compare_and_swap(held, 0).unwrap_err(),
        DenyReason::LeaseVersionConflict
    );
    // The refused swap did not advance the version either.
    assert_eq!(lease.version, held + 1);
}

#[test]
fn every_control_transition_advances_the_epoch_and_never_rewinds_it() {
    let mut lease = RunLease::new("run-1", 100_000);
    let mut last = lease.epoch;
    for bump in [
        EpochBump::Paused,
        EpochBump::Resumed,
        EpochBump::Recovered,
        EpochBump::Paused,
        EpochBump::OperatorTakeover,
        EpochBump::Cancelled,
    ] {
        let next = lease.bump_epoch(bump);
        assert!(next > last, "{bump:?} did not advance the epoch");
        last = next;
    }
}

#[test]
fn a_takeover_cannot_be_undone_by_a_stale_resume() {
    let mut lease = RunLease::new("run-1", 100_000);
    lease.bump_epoch(EpochBump::OperatorTakeover);
    assert_eq!(lease.holder, LeaseHolder::Operator);
    assert_eq!(
        lease.check_agent_may_act(0).unwrap_err(),
        DenyReason::LeaseLost
    );
    // Resuming is itself an epoch move, so the frames decided before the
    // takeover are still refused afterwards.
    let before_epoch = lease.epoch;
    lease.bump_epoch(EpochBump::Resumed);
    assert!(lease.epoch > before_epoch);
    assert_eq!(lease.holder, LeaseHolder::Agent);
}

#[test]
fn an_expired_lease_stops_the_agent_at_the_exact_boundary() {
    let lease = RunLease::new("run-1", 1_000);
    assert!(lease.check_agent_may_act(999).is_ok());
    assert_eq!(
        lease.check_agent_may_act(1_000).unwrap_err(),
        DenyReason::LeaseLost
    );
    assert_eq!(
        lease.check_agent_may_act(u64::MAX).unwrap_err(),
        DenyReason::LeaseLost
    );
}

#[test]
fn a_frame_is_refused_once_it_is_older_than_the_profile_allows() {
    for profile in ProfileId::ALL {
        let spec = profile.spec();
        let mut fixture = Fixture::new(*profile, ModelTier::StrongHosted);
        fixture.now_millis = common::CAPTURED_AT + spec.max_frame_age_millis;
        assert!(
            fixture.evaluate().refusal().is_none(),
            "{profile:?} refused a frame exactly at its bound"
        );
        fixture.now_millis = common::CAPTURED_AT + spec.max_frame_age_millis + 1;
        assert_eq!(
            fixture.evaluate().refusal(),
            Some(DenyReason::StaleFrame),
            "{profile:?} accepted a frame past its bound"
        );
    }
}

#[test]
fn a_frame_from_the_future_is_refused_rather_than_treated_as_fresh() {
    let mut fixture = Fixture::new(ProfileId::Balanced, ModelTier::StrongHosted);
    fixture.now_millis = common::CAPTURED_AT - 1;
    assert_eq!(fixture.evaluate().refusal(), Some(DenyReason::StaleFrame));
}

#[test]
fn a_superseded_frame_is_refused_even_when_it_is_fresh() {
    let mut fixture = Fixture::new(ProfileId::Balanced, ModelTier::StrongHosted);
    fixture.live_frame.sequence += 1;
    assert_eq!(fixture.evaluate().refusal(), Some(DenyReason::StaleFrame));
}

#[test]
fn a_changed_frame_digest_is_refused_at_the_same_sequence() {
    let mut fixture = Fixture::new(ProfileId::Balanced, ModelTier::StrongHosted);
    fixture.live_frame.digest =
        grokptah_cu_adaptive::digest::digest_str(grokptah_cu_adaptive::digest::domain::FRAME, "x");
    assert_eq!(fixture.evaluate().refusal(), Some(DenyReason::StaleFrame));
}

#[test]
fn the_epoch_is_checked_before_the_frame_is_compared() {
    // Which matters for what a reviewer reads first: "someone else took over"
    // is more important than "and the frame also moved".
    let mut fixture = Fixture::new(ProfileId::Balanced, ModelTier::StrongHosted);
    fixture.lease.bump_epoch(EpochBump::Paused);
    fixture.live_frame.sequence += 1;
    assert_eq!(
        fixture.evaluate().refusal(),
        Some(DenyReason::FrameEpochChanged)
    );
}

#[test]
fn cleanup_is_idempotent_and_reports_what_it_did() {
    let mut cleanup = CleanupLedger::new();
    cleanup.acquire(Resource::Lease);
    assert_eq!(cleanup.release(Resource::Lease), ReleaseOutcome::Released);
    assert_eq!(
        cleanup.release(Resource::Lease),
        ReleaseOutcome::AlreadyReleased
    );
    assert_eq!(
        cleanup.release(Resource::EvidenceHandles),
        ReleaseOutcome::NotHeld
    );
    assert!(cleanup.is_complete());
}

#[test]
fn cancelling_twice_reports_the_first_signal_and_moves_the_epoch_once() {
    let mut lease = RunLease::new("run-1", 100_000);
    let mut cleanup = CleanupLedger::new();
    for resource in Resource::ALL {
        cleanup.acquire(*resource);
    }
    let first = cleanup.cancel(&mut lease, CancelCause::OperatorRequest, 10);
    let epoch = lease.epoch;
    let second = cleanup.cancel(&mut lease, CancelCause::SessionEnded, 20);
    assert_eq!(first, second);
    assert_eq!(lease.epoch, epoch);
    assert!(cleanup.is_complete());
    assert!(cleanup.outstanding().is_empty());
}

#[test]
fn a_cancelled_run_admits_nothing_afterwards() {
    let mut fixture = Fixture::new(ProfileId::Balanced, ModelTier::StrongHosted);
    let mut lease = fixture.lease.clone();
    let mut cleanup = CleanupLedger::new();
    cleanup.acquire(Resource::Lease);
    cleanup.cancel(&mut lease, CancelCause::OperatorRequest, 1);
    fixture.lease = lease;
    fixture.cleanup = cleanup;
    assert_eq!(fixture.evaluate().refusal(), Some(DenyReason::Cancelled));
}

#[test]
fn outstanding_resources_are_reported_not_hidden() {
    let mut cleanup = CleanupLedger::new();
    cleanup.acquire(Resource::Lease);
    cleanup.acquire(Resource::EvidenceHandles);
    cleanup.release(Resource::Lease);
    assert!(!cleanup.is_complete());
    assert_eq!(cleanup.outstanding(), vec![Resource::EvidenceHandles]);
}

#[test]
fn every_run_in_the_matrix_gives_everything_back() {
    for family in ScenarioFamily::ALL {
        for profile in ProfileId::ALL {
            for tier in ModelTier::ALL {
                let outcome = run(RunConfig {
                    scenario: Scenario::new(*family, Horizon::Short),
                    profile: *profile,
                    tier: *tier,
                });
                assert!(
                    outcome.receipt.cleanup_complete,
                    "{} ended holding {:?}",
                    outcome.label, outcome.receipt.cleanup_residue
                );
                assert!(outcome.receipt.cleanup_residue.is_empty());
                outcome.reconciles().expect("receipt reconciles");
            }
        }
    }
}

#[test]
fn a_mid_flight_takeover_stops_the_run_and_is_recorded() {
    for horizon in Horizon::ALL {
        for profile in ProfileId::ALL {
            let outcome = run(RunConfig {
                scenario: Scenario::new(ScenarioFamily::CancellationMidFlight, *horizon),
                profile: *profile,
                tier: ModelTier::StrongHosted,
            });
            outcome.reconciles().expect("receipt reconciles");
            assert_eq!(outcome.receipt.stop_reason, StopReason::Cancelled);
            assert!(outcome.receipt.cancellation.is_some());
            assert!(outcome.refused_for(DenyReason::LeaseLost));
            assert!(
                outcome.steps_reached < horizon.steps(),
                "{} ran to the end despite a takeover",
                outcome.label
            );
            assert!(!outcome.receipt.is_orderly());
        }
    }
}

#[test]
fn a_cancelled_run_can_never_report_an_orderly_end() {
    for family in ScenarioFamily::ALL {
        for horizon in Horizon::ALL {
            let outcome = run(RunConfig {
                scenario: Scenario::new(*family, *horizon),
                profile: ProfileId::Balanced,
                tier: ModelTier::SmallLocal,
            });
            if outcome.receipt.cancellation.is_some() {
                assert!(
                    !outcome.receipt.is_orderly(),
                    "{} was cancelled and still called itself orderly",
                    outcome.label
                );
                assert_ne!(outcome.receipt.stop_reason, StopReason::ObjectiveComplete);
            }
        }
    }
}
