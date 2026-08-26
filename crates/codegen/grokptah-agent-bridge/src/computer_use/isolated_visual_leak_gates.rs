//! Failure, cleanup, restart, and takeover leak-freedom gates.
//!
//! Every test here is deterministic. The only thing that waits is the deadline
//! test, and waiting is the property under test: it asserts that a bounded read
//! against a peer that never answers *returns*, and that it did not return
//! early. There is no upper time bound anywhere, so nothing here can fail
//! because the host was busy.
//!
//! What must not survive a terminal path: an orphan descriptor or socket, an
//! unreaped process, a mount, an overlay, a frame cache, a live lease, or an
//! authority record that still admits control.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use super::isolated_guest::{IsolatedGuestPhase, IsolatedGuestSession};
use super::isolated_visual::{
    IsolatedVisualCleanupEvidence, IsolatedVisualLaunchContract, IsolatedVisualLifecycle,
    IsolatedVisualLifecycleState, IsolatedVisualManifest, IsolatedVisualResourceLimits,
    IsolatedVisualSecurityProfile, IsolatedVisualTerminalDisposition,
    ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION, ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION,
    MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID,
};
use super::isolated_visual_channel::IsolatedVisualChannelBinding;
use super::isolated_visual_driver::IsolatedVisualRuntimeDriver;
use super::isolated_visual_frames::IsolatedVisualFrameCarrier;
use super::isolated_visual_helper::{
    IsolatedVisualHelperEventCode, ISOLATED_VISUAL_HELPER_EVENT_BYTES,
    ISOLATED_VISUAL_HELPER_EVENT_MAGIC, ISOLATED_VISUAL_HELPER_EVENT_VERSION,
};
use super::isolated_visual_helper_control::IsolatedVisualHelperControl;
use super::isolated_visual_input::IsolatedVisualInputMessage;
use super::isolated_visual_runtime::IsolatedVisualRuntimeSession;
use super::isolated_visual_stream::IsolatedVisualStream;
use super::types::ComputerSurfaceBinding;

const CHALLENGE: [u8; 32] = [0x3c; 32];
const NONCE: &str = "550e8400-e29b-41d4-a716-446655440000";
/// `poll` takes a whole-millisecond timeout and the remaining duration is
/// truncated to get it, so a bounded read may return up to one millisecond
/// before its deadline. Two milliseconds is that truncation with a single
/// millisecond of slack; it is not a tolerance for a busy host, and no test
/// here has an upper time bound.
const POLL_TRUNCATION: Duration = Duration::from_millis(2);

fn contract() -> IsolatedVisualLaunchContract {
    IsolatedVisualLaunchContract {
        run_id: "leak-gate-run".into(),
        surface: ComputerSurfaceBinding {
            surface_id: "leak-gate-surface".into(),
            incarnation: "leak-gate-incarnation".into(),
        },
        input_domain_id: "leak-gate-input".into(),
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

fn helper_event(code: IsolatedVisualHelperEventCode) -> [u8; ISOLATED_VISUAL_HELPER_EVENT_BYTES] {
    let mut bytes = [0_u8; ISOLATED_VISUAL_HELPER_EVENT_BYTES];
    bytes[0..4].copy_from_slice(&ISOLATED_VISUAL_HELPER_EVENT_MAGIC.to_be_bytes());
    bytes[4..6].copy_from_slice(&ISOLATED_VISUAL_HELPER_EVENT_VERSION.to_be_bytes());
    bytes[6..8].copy_from_slice(&(code as u16).to_be_bytes());
    bytes
}

fn cleanup_evidence() -> IsolatedVisualCleanupEvidence {
    IsolatedVisualCleanupEvidence::verified(
        contract().surface.clone(),
        true,
        true,
        true,
        true,
        true,
    )
    .expect("complete evidence")
}

/// The kernel object each open descriptor points at, e.g. `socket:[12345]`.
///
/// Counting descriptors would be wrong here: the test binary runs tests in
/// parallel threads and `/proc/self/fd` is process-wide, so an unrelated test
/// opening a file would look like a leak. Socket identities are unique to the
/// sockets this test made, so tracking them is immune to whatever else the
/// process is doing.
#[cfg(target_os = "linux")]
fn open_descriptor_targets() -> std::collections::BTreeSet<String> {
    std::fs::read_dir("/proc/self/fd")
        .expect("/proc/self/fd is readable on Linux")
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| std::fs::read_link(entry.path()).ok())
        .map(|target| target.display().to_string())
        .collect()
}

/// The kernel identity of one live socket, so it can be looked for after the
/// owning value is dropped.
#[cfg(target_os = "linux")]
fn socket_identity(stream: &UnixStream) -> String {
    use std::os::fd::AsRawFd;
    std::fs::read_link(format!("/proc/self/fd/{}", stream.as_raw_fd()))
        .expect("a live socket has a /proc/self/fd entry")
        .display()
        .to_string()
}

/// Identities for the four sockets one session uses.
///
/// Reading them needs `/proc`, so off Linux this is empty and the descriptor
/// assertions below do not run. Driving the session itself does not need
/// `/proc`, so that part still runs everywhere and is what the two
/// all-platform tests below check.
fn session_socket_identities(sockets: [&UnixStream; 4]) -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        sockets
            .iter()
            .map(|socket| socket_identity(socket))
            .collect()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = sockets;
        Vec::new()
    }
}

// ------------------------------------------------------------ descriptors and sockets

/// Drives one complete session over real socket pairs, then drops everything.
/// Returns the kernel identity of every socket it made where the platform
/// exposes that, so a caller can prove none of them is still open.
fn drive_one_session_over_sockets() -> Vec<String> {
    let (control_host, control_peer) = UnixStream::pair().expect("control socket pair");
    let (frame_host, frame_peer) = UnixStream::pair().expect("frame socket pair");
    let identities =
        session_socket_identities([&control_host, &control_peer, &frame_host, &frame_peer]);

    // The peer plays the helper and guest: it queues exactly the events and the
    // one frame this session consumes. It stays open for the whole session,
    // because closing it early would break the pipe under the host's first
    // control write.
    let mut control_peer = control_peer;
    let mut frame_peer = frame_peer;
    {
        let peer = &mut control_peer;
        for code in [
            IsolatedVisualHelperEventCode::Prepared,
            IsolatedVisualHelperEventCode::Running,
            IsolatedVisualHelperEventCode::Bound,
            IsolatedVisualHelperEventCode::Stopped,
        ] {
            peer.write_all(&helper_event(code)).expect("queue event");
        }
        peer.flush().expect("flush events");

        let contract = contract();
        let secret = IsolatedVisualChannelBinding::from_contract(&contract)
            .unwrap()
            .derive_channel_secret(&CHALLENGE)
            .unwrap();
        let mut guest = IsolatedVisualFrameCarrier::new_guest(&contract, &secret).unwrap();
        let frame_peer = &mut frame_peer;
        for chunk in guest.seal_frame(1, NONCE, 2, 2, &[8, 8, 8, 8]).unwrap() {
            frame_peer
                .write_all(&(chunk.len() as u32).to_be_bytes())
                .expect("queue frame length");
            frame_peer.write_all(&chunk).expect("queue frame");
        }
        frame_peer.flush().expect("flush frame");
    }

    let runtime = IsolatedVisualRuntimeSession::new(contract(), CHALLENGE).unwrap();
    let mut driver = IsolatedVisualRuntimeDriver::new(
        runtime,
        IsolatedVisualHelperControl::new(
            control_host.try_clone().expect("clone control reader"),
            control_host,
        ),
        IsolatedVisualStream::new(
            frame_host.try_clone().expect("clone frame reader"),
            frame_host,
        ),
    );

    driver.receive_helper_event().unwrap();
    driver.start().unwrap();
    driver.receive_helper_event().unwrap();
    driver.bind().unwrap();
    driver.receive_helper_event().unwrap();
    let frame = driver.read_frame().unwrap().expect("one complete frame");
    assert_eq!(frame.bytes, vec![8, 8, 8, 8]);
    driver
        .write_input(
            1,
            NONCE,
            IsolatedVisualInputMessage::PointerMove { x: 1, y: 1 },
        )
        .unwrap();
    driver
        .stop(IsolatedVisualTerminalDisposition::Cancelled)
        .unwrap();
    driver.receive_helper_event().unwrap();
    driver
        .complete_observed_cleanup(true, true, true, true)
        .unwrap();

    let (_runtime, helper, stream) = driver.into_parts();
    let (event_reader, control_writer) = helper.into_parts();
    let (frame_reader, input_writer) = stream.into_parts();
    drop((event_reader, control_writer, frame_reader, input_writer));
    drop((control_peer, frame_peer));
    identities
}

#[cfg(target_os = "linux")]
#[test]
fn a_completed_session_leaves_no_orphan_descriptor_or_socket() {
    let identities = drive_one_session_over_sockets();
    let live = open_descriptor_targets();
    for identity in &identities {
        assert!(
            !live.contains(identity),
            "a completed session left {identity} open"
        );
    }
    assert_eq!(
        identities.len(),
        4,
        "the session should have made four sockets"
    );
}

/// Builds a session and drops it mid-flight — the crash and restart case —
/// without driving it. The supervisor owns the descriptors, so dropping it must
/// still release every one of them.
fn abandon_one_session() -> Vec<String> {
    let (control_host, control_peer) = UnixStream::pair().unwrap();
    let (frame_host, frame_peer) = UnixStream::pair().unwrap();
    let identities =
        session_socket_identities([&control_host, &control_peer, &frame_host, &frame_peer]);

    let runtime = IsolatedVisualRuntimeSession::new(contract(), CHALLENGE).unwrap();
    let driver = IsolatedVisualRuntimeDriver::new(
        runtime,
        IsolatedVisualHelperControl::new(control_host.try_clone().unwrap(), control_host),
        IsolatedVisualStream::new(frame_host.try_clone().unwrap(), frame_host),
    );
    drop(driver);
    drop((control_peer, frame_peer));
    identities
}

#[test]
fn a_session_drives_to_completion_over_real_sockets() {
    // Runs on every unix, so macOS covers the whole seam too. The driving
    // function asserts the frame reassembles and the cleanup completes.
    drive_one_session_over_sockets();
}

#[test]
fn an_abandoned_session_can_be_dropped_mid_flight() {
    abandon_one_session();
}

#[cfg(target_os = "linux")]
#[test]
fn an_abandoned_session_leaves_no_orphan_descriptor_or_socket() {
    let identities = abandon_one_session();
    let live = open_descriptor_targets();
    for identity in &identities {
        assert!(
            !live.contains(identity),
            "an abandoned session left {identity} open"
        );
    }
}

#[test]
fn a_bounded_read_against_a_silent_peer_returns_instead_of_hanging() {
    // The peer never answers. The read must come back, and it must not come
    // back before its deadline. There is deliberately no upper bound, so a busy
    // host cannot fail this.
    let (host, _peer) = UnixStream::pair().unwrap();
    let mut runtime = IsolatedVisualRuntimeSession::new(contract(), CHALLENGE).unwrap();
    let mut helper = IsolatedVisualHelperControl::new(host.try_clone().unwrap(), host);

    let timeout = Duration::from_millis(50);
    let started = Instant::now();
    let result = helper.receive_event_with_timeout(&mut runtime, timeout);
    let elapsed = started.elapsed();

    assert!(
        result.is_err(),
        "a silent peer must not be waited on forever"
    );
    // The poll timeout is computed with `as_millis()`, which truncates, so the
    // read may come back up to one millisecond early. Anything more than that
    // would mean it did not really wait.
    assert!(
        elapsed + POLL_TRUNCATION >= timeout,
        "the bounded read returned before its deadline: {elapsed:?}"
    );
}

#[test]
fn a_bounded_frame_read_against_a_silent_peer_returns_instead_of_hanging() {
    let (host, _peer) = UnixStream::pair().unwrap();
    let mut runtime = IsolatedVisualRuntimeSession::new(contract(), CHALLENGE).unwrap();
    let mut stream = IsolatedVisualStream::new(host.try_clone().unwrap(), host);

    let timeout = Duration::from_millis(50);
    let started = Instant::now();
    let result = stream.read_frame_chunk_with_timeout(&mut runtime, timeout);
    let elapsed = started.elapsed();

    assert!(
        result.is_err(),
        "a silent guest must not be waited on forever"
    );
    assert!(
        elapsed + POLL_TRUNCATION >= timeout,
        "the bounded frame read returned before its deadline: {elapsed:?}"
    );
}

// ------------------------------------------------------------ overlay, cache, evidence

#[test]
fn cleanup_evidence_cannot_be_minted_while_anything_survives() {
    let surface = contract().surface.clone();
    for (guest_stopped, process_absent, no_handles, overlay_removed, cache_removed, survivor) in [
        (false, true, true, true, true, "running guest"),
        (true, false, true, true, true, "helper process"),
        (true, true, false, true, true, "open handle"),
        (true, true, true, false, true, "overlay"),
        (true, true, true, true, false, "frame cache"),
    ] {
        assert!(
            IsolatedVisualCleanupEvidence::verified(
                surface.clone(),
                guest_stopped,
                process_absent,
                no_handles,
                overlay_removed,
                cache_removed,
            )
            .is_err(),
            "cleanup evidence was minted with a surviving {survivor}"
        );
    }
    IsolatedVisualCleanupEvidence::verified(surface, true, true, true, true, true).unwrap();
}

#[test]
fn cleanup_evidence_from_another_surface_is_refused() {
    let mut other = contract();
    other.surface.incarnation = "a-different-incarnation".into();
    let foreign = IsolatedVisualCleanupEvidence::verified(
        other.surface.clone(),
        true,
        true,
        true,
        true,
        true,
    )
    .unwrap();

    let mut lifecycle = IsolatedVisualLifecycle::new(contract()).unwrap();
    lifecycle.begin_start().unwrap();
    lifecycle
        .begin_stop(IsolatedVisualTerminalDisposition::Cancelled)
        .unwrap();
    lifecycle.require_cleanup().unwrap();
    assert!(
        lifecycle.complete_cleanup(&foreign).is_err(),
        "evidence from another surface completed this cleanup"
    );
    assert_eq!(
        lifecycle.state(),
        IsolatedVisualLifecycleState::CleanupPending
    );
    lifecycle.complete_cleanup(&cleanup_evidence()).unwrap();
    assert_eq!(lifecycle.state(), IsolatedVisualLifecycleState::Terminated);
}

// ------------------------------------------------------------ restart and takeover

#[test]
fn restart_interrupts_every_nonterminal_state_and_still_demands_cleanup() {
    let build = |steps: usize| {
        let mut lifecycle = IsolatedVisualLifecycle::new(contract()).unwrap();
        if steps >= 1 {
            lifecycle.begin_start().unwrap();
        }
        if steps >= 2 {
            lifecycle.mark_read_only_ready().unwrap();
        }
        if steps >= 3 {
            lifecycle
                .begin_stop(IsolatedVisualTerminalDisposition::Cancelled)
                .unwrap();
        }
        if steps >= 4 {
            lifecycle.require_cleanup().unwrap();
        }
        lifecycle
    };

    // Prepared has nothing to clean up, so restart terminates it outright.
    let mut prepared = build(0);
    prepared.interrupt_on_restart().unwrap();
    assert_eq!(prepared.state(), IsolatedVisualLifecycleState::Terminated);

    // Every other nonterminal state must still produce exact evidence.
    for steps in 1..=4 {
        let mut lifecycle = build(steps);
        lifecycle.interrupt_on_restart().unwrap();
        assert_eq!(
            lifecycle.state(),
            IsolatedVisualLifecycleState::CleanupPending,
            "restart from step {steps} skipped cleanup"
        );
        lifecycle.complete_cleanup(&cleanup_evidence()).unwrap();
        assert_eq!(lifecycle.state(), IsolatedVisualLifecycleState::Terminated);
    }
}

#[test]
fn a_terminated_lifecycle_never_reopens() {
    let mut lifecycle = IsolatedVisualLifecycle::new(contract()).unwrap();
    lifecycle.begin_start().unwrap();
    lifecycle.interrupt_on_restart().unwrap();
    lifecycle.complete_cleanup(&cleanup_evidence()).unwrap();
    assert_eq!(lifecycle.state(), IsolatedVisualLifecycleState::Terminated);

    // Nothing may move it again, and the same evidence may not be replayed.
    assert!(lifecycle.begin_start().is_err());
    assert!(lifecycle.mark_read_only_ready().is_err());
    assert!(lifecycle
        .begin_stop(IsolatedVisualTerminalDisposition::Cancelled)
        .is_err());
    assert!(lifecycle.require_cleanup().is_err());
    assert!(lifecycle.complete_cleanup(&cleanup_evidence()).is_err());
    // A restart of an already terminated lifecycle is a no-op, not a reopen.
    lifecycle.interrupt_on_restart().unwrap();
    assert_eq!(lifecycle.state(), IsolatedVisualLifecycleState::Terminated);
}

#[test]
fn no_lease_or_authority_record_survives_cancel_or_failure() {
    for fail in [false, true] {
        let mut guest = IsolatedGuestSession::create(contract(), CHALLENGE).unwrap();
        let lease = guest.acquire("agent-a").unwrap();
        guest.drive_to_ready("agent-a", &lease).unwrap();
        if fail {
            guest.fail_guest("agent-a", &lease).unwrap();
            assert_eq!(guest.phase(), IsolatedGuestPhase::Failed);
        } else {
            guest.drive_to_running("agent-a", &lease).unwrap();
            guest.cancel("agent-a", &lease).unwrap();
            assert_eq!(guest.phase(), IsolatedGuestPhase::Closing);
        }

        // The lease is gone, the old one no longer authorizes anything, and no
        // agent may take the guest over afterwards.
        assert!(guest.lease().is_none(), "a lease survived a terminal path");
        assert!(guest.control("agent-a", &lease).is_err());
        assert!(guest.cancel("agent-a", &lease).is_err());
        assert!(guest.acquire("agent-a").is_err());
        assert!(guest.acquire("agent-b").is_err());
    }
}

#[test]
fn a_reacquired_lease_supersedes_the_previous_revision() {
    let mut guest = IsolatedGuestSession::create(contract(), CHALLENGE).unwrap();
    let first = guest.acquire("agent-a").unwrap();
    let again = guest.acquire("agent-a").unwrap();
    assert_eq!(
        first, again,
        "the same agent must keep one lease rather than stacking them"
    );
    let stale = super::isolated_guest::IsolatedGuestLease {
        lease_id: first.lease_id.clone(),
        agent_id: first.agent_id.clone(),
        revision: first.revision + 1,
    };
    assert!(
        guest.control("agent-a", &stale).is_err(),
        "a forged higher revision took control"
    );
}

// ------------------------------------------------------------ process reaping

#[test]
fn the_packaged_supervisor_escalates_and_reaps_rather_than_orphaning() {
    let supervisor = include_str!("macos_isolated_runtime.rs");

    // Graceful stop is the protocol stop control plus a bounded wait for the
    // helper's Stopped event. There is deliberately no SIGTERM: signalling a
    // numeric PID is the forced path only.
    for required in [
        "stop_with_timeout",
        "STOPPING_EVENT_TIMEOUT",
        "fn wait_for_exit",
        "FORCE_REAP_TIMEOUT",
        "fn abort_with_error",
    ] {
        assert!(
            supervisor.contains(required),
            "the packaged supervisor has no {required} in its stop path"
        );
    }

    // The forced path reaps *before* it signals, so a PID that already exited
    // and was reused cannot be killed by number.
    for required in [
        "fn terminate_process",
        "waitpid_without_interrupt",
        "SIGKILL",
        "WNOHANG",
        "ECHILD",
        "EINTR",
    ] {
        assert!(
            supervisor.contains(required),
            "the packaged supervisor's forced stop has no {required}"
        );
    }
    let terminate = supervisor
        .split("fn terminate_process")
        .nth(1)
        .expect("terminate_process is defined");
    let reap_first = terminate.find("waitpid_without_interrupt");
    let kill = terminate.find("SIGKILL");
    assert!(
        matches!((reap_first, kill), (Some(reap), Some(kill)) if reap < kill),
        "the supervisor signals a PID before establishing that it still owns it"
    );
}
