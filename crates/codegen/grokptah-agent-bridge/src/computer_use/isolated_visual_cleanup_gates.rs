//! Cleanup-proof gates: terminal cleanup may never claim more than the host
//! observed.
//!
//! Terminating a run is the strongest statement the isolated visual lifecycle
//! makes — it asserts that no guest, helper, handle, overlay, or frame cache
//! survives. Four of those facts are host-observed resource checks. The fifth,
//! *the guest itself stopped*, is derived from the helper state this session
//! actually observed rather than accepted from the caller, because writing the
//! stop control byte is not the same as the helper acknowledging it.
//!
//! These gates are deterministic and provider-free. They drive the helper ABI
//! directly and open no process, socket, VM, or package. Nothing here certifies
//! real hardware: they prove the refusal, not the launch.

use super::isolated_visual::{
    IsolatedVisualLaunchContract, IsolatedVisualLifecycleState, IsolatedVisualManifest,
    IsolatedVisualResourceLimits, IsolatedVisualSecurityProfile, IsolatedVisualTerminalDisposition,
    ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION, ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION,
    MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID,
};
use super::isolated_visual_helper::{
    IsolatedVisualHelperEvent, IsolatedVisualHelperFailure, IsolatedVisualHelperSupervisorState,
};
use super::isolated_visual_runtime::IsolatedVisualRuntimeSession;
use super::types::{ComputerErrorCode, ComputerSurfaceBinding};

const CHALLENGE: [u8; 32] = [11; 32];

const EVENT_PREPARED: u16 = 1;
const EVENT_RUNNING: u16 = 2;
const EVENT_STOPPED: u16 = 3;
const EVENT_FAILURE: u16 = 4;
const EVENT_BOUND: u16 = 5;

fn contract() -> IsolatedVisualLaunchContract {
    IsolatedVisualLaunchContract {
        run_id: "run-cleanup-gate".into(),
        surface: ComputerSurfaceBinding {
            surface_id: "surface-cleanup-gate".into(),
            incarnation: "incarnation-cleanup-gate".into(),
        },
        input_domain_id: "input-cleanup-gate".into(),
        manifest: IsolatedVisualManifest {
            schema_version: ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION,
            backend_id: MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID.into(),
            guest_protocol_version: ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION,
            helper_content_sha256: "a".repeat(64),
            helper_signing_requirement_sha256: "b".repeat(64),
            guest_image_sha256: "c".repeat(64),
            configuration_sha256: "d".repeat(64),
            security_profile: IsolatedVisualSecurityProfile::locked_down(),
            limits: IsolatedVisualResourceLimits::proof_defaults(),
        },
    }
}

fn event_bytes(code: u16, detail: u32) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&0x4750_5449u32.to_be_bytes());
    bytes[4..6].copy_from_slice(&1u16.to_be_bytes());
    bytes[6..8].copy_from_slice(&code.to_be_bytes());
    bytes[8..12].copy_from_slice(&detail.to_be_bytes());
    bytes
}

fn accept(session: &mut IsolatedVisualRuntimeSession, code: u16, detail: u32) {
    let event = IsolatedVisualHelperEvent::decode(&event_bytes(code, detail)).expect("valid event");
    session.accept_helper_event(event).expect("accepted event");
}

/// A session whose guest the helper has reported live and bound.
fn bound_session() -> IsolatedVisualRuntimeSession {
    let mut session =
        IsolatedVisualRuntimeSession::new(contract(), CHALLENGE).expect("runtime session");
    accept(&mut session, EVENT_PREPARED, 0);
    session.start_control().expect("start control");
    accept(&mut session, EVENT_RUNNING, 0);
    session.bind_control().expect("bind control");
    accept(&mut session, EVENT_BOUND, 0);
    assert_eq!(
        session.helper_state(),
        IsolatedVisualHelperSupervisorState::Bound
    );
    session
}

/// Complete, truthful resource evidence. Only the guest-stopped fact varies
/// across these gates, so a refusal can only come from that derivation.
fn complete_resource_cleanup(session: &mut IsolatedVisualRuntimeSession) -> ComputerErrorCode {
    session
        .complete_observed_cleanup(true, true, true, true)
        .expect_err("cleanup must fail closed")
        .code
}

#[test]
fn a_stop_written_but_never_acknowledged_cannot_complete_cleanup() {
    let mut session = bound_session();
    // The stop control byte is written, but the helper never answers.
    session
        .stop_control(IsolatedVisualTerminalDisposition::Cancelled)
        .expect("stop control");
    assert_eq!(
        session.helper_state(),
        IsolatedVisualHelperSupervisorState::StopSent
    );
    session.fail().expect("fail requires cleanup");
    assert_eq!(
        session.lifecycle_state(),
        IsolatedVisualLifecycleState::CleanupPending
    );

    // Every resource check is truthful; the guest stop was never observed.
    assert_eq!(
        complete_resource_cleanup(&mut session),
        ComputerErrorCode::Conflict
    );
    assert_ne!(
        session.lifecycle_state(),
        IsolatedVisualLifecycleState::Terminated
    );
}

#[test]
fn a_live_bound_guest_cannot_be_cleaned_up_on_a_callers_word() {
    let mut session = bound_session();
    session.fail().expect("fail requires cleanup");
    assert_eq!(
        session.lifecycle_state(),
        IsolatedVisualLifecycleState::CleanupPending
    );
    assert_eq!(
        session.helper_state(),
        IsolatedVisualHelperSupervisorState::Bound
    );

    assert_eq!(
        complete_resource_cleanup(&mut session),
        ComputerErrorCode::Conflict
    );
    assert_ne!(
        session.lifecycle_state(),
        IsolatedVisualLifecycleState::Terminated
    );
}

#[test]
fn failures_that_leave_the_guests_fate_unknown_fail_cleanup_closed() {
    for failure in [
        IsolatedVisualHelperFailure::StartFailed,
        IsolatedVisualHelperFailure::ControlLost,
        IsolatedVisualHelperFailure::StopFailed,
        IsolatedVisualHelperFailure::GuestProtocol,
    ] {
        let mut session = bound_session();
        accept(&mut session, EVENT_FAILURE, failure as u32);
        assert_eq!(
            session.lifecycle_state(),
            IsolatedVisualLifecycleState::CleanupPending
        );
        assert_eq!(
            complete_resource_cleanup(&mut session),
            ComputerErrorCode::Conflict,
            "{failure:?} must not prove the guest stopped"
        );
    }
}

#[test]
fn an_observed_guest_stop_completes_cleanup() {
    // The helper reported that the guest itself stopped.
    let mut session = bound_session();
    accept(
        &mut session,
        EVENT_FAILURE,
        IsolatedVisualHelperFailure::GuestStopped as u32,
    );
    session
        .complete_observed_cleanup(true, true, true, true)
        .expect("an observed guest stop completes cleanup");
    assert_eq!(
        session.lifecycle_state(),
        IsolatedVisualLifecycleState::Terminated
    );
}

#[test]
fn an_acknowledged_clean_stop_completes_cleanup() {
    let mut session = bound_session();
    session
        .stop_control(IsolatedVisualTerminalDisposition::Cancelled)
        .expect("stop control");
    accept(&mut session, EVENT_STOPPED, 0);
    assert_eq!(
        session.helper_state(),
        IsolatedVisualHelperSupervisorState::Stopped
    );
    session
        .complete_observed_cleanup(true, true, true, true)
        .expect("an acknowledged stop completes cleanup");
    assert_eq!(
        session.lifecycle_state(),
        IsolatedVisualLifecycleState::Terminated
    );
}

#[test]
fn restart_interruption_still_requires_observed_helper_absence() {
    let mut session = bound_session();
    session.interrupt_on_restart().expect("restart interrupt");
    assert_eq!(
        session.lifecycle_state(),
        IsolatedVisualLifecycleState::CleanupPending
    );
    assert_eq!(
        session.lifecycle().terminal_disposition,
        Some(IsolatedVisualTerminalDisposition::Interrupted)
    );

    // A helper that is still present proves nothing about its guest.
    assert_eq!(
        session
            .complete_observed_cleanup(false, true, true, true)
            .expect_err("a surviving helper cannot terminate an interrupted run")
            .code,
        ComputerErrorCode::Conflict
    );

    // The helper owns the guest, so a helper that is gone leaves none behind.
    session
        .complete_observed_cleanup(true, true, true, true)
        .expect("an interrupted run with an absent helper completes cleanup");
    assert_eq!(
        session.lifecycle_state(),
        IsolatedVisualLifecycleState::Terminated
    );
}

#[test]
fn the_guest_not_running_proof_is_closed_and_deliberate() {
    use IsolatedVisualHelperSupervisorState as State;

    // Only an observed terminal report proves the guest is not running. A
    // control byte written but unacknowledged, and a live guest, do not.
    for state in [
        State::AwaitingPrepared,
        State::Prepared,
        State::Stopped,
        State::Failed(IsolatedVisualHelperFailure::GuestStopped),
    ] {
        assert!(
            state.proves_guest_not_running(),
            "{state:?} should prove the guest is not running"
        );
    }
    for state in [
        State::StartSent,
        State::Running,
        State::BindingSent,
        State::Bound,
        State::StopSent,
    ] {
        assert!(
            !state.proves_guest_not_running(),
            "{state:?} must not prove the guest is not running"
        );
    }

    // Failures raised before a guest could exist prove it; failures that can
    // leave a live guest behind must not.
    for failure in [
        IsolatedVisualHelperFailure::InvalidInvocation,
        IsolatedVisualHelperFailure::InvalidDescriptor,
        IsolatedVisualHelperFailure::InvalidConfiguration,
        IsolatedVisualHelperFailure::StartNotAuthorized,
        IsolatedVisualHelperFailure::VirtualizationUnavailable,
        IsolatedVisualHelperFailure::ConfigurationRejected,
        IsolatedVisualHelperFailure::GuestStopped,
    ] {
        assert!(
            failure.proves_guest_not_running(),
            "{failure:?} should prove the guest is not running"
        );
    }
    for failure in [
        IsolatedVisualHelperFailure::StartFailed,
        IsolatedVisualHelperFailure::ControlLost,
        IsolatedVisualHelperFailure::StopFailed,
        IsolatedVisualHelperFailure::GuestProtocol,
    ] {
        assert!(
            !failure.proves_guest_not_running(),
            "{failure:?} must not prove the guest is not running"
        );
    }
}
