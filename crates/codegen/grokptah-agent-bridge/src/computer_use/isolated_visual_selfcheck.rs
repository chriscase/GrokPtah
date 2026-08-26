//! Exact, allocation-only rehearsal of the isolated visual substrate.
//!
//! Dispatch is disabled: no Computer Use path launches a guest, spawns the
//! packaged helper, or admits isolated input. That leaves the substrate with no
//! runtime caller, and the two usual ways to keep a `-D warnings` build quiet
//! about that are a crate-root re-export — which would widen exactly the public
//! surface this reconstruction keeps closed — or a blanket `#[allow(dead_code)]`,
//! which would also hide genuine dead code in every later edit. This module is
//! the third option: it drives each substrate entrypoint for real, so the lib
//! target sees the substrate as live because it *is* live.
//!
//! The rehearsal performs no process, VM, filesystem, network, clipboard, or
//! host-surface work. Every endpoint reads and writes an in-memory buffer, the
//! bootstrap challenge is a fixed local constant, and no Computer Use authority,
//! grant, or run record is minted. The manifest digests below are synthetic
//! rehearsal constants and cannot match any real signed package: measuring a
//! reviewed package stays the packaged supervisor's job.

use std::io::Cursor;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::time::Duration;

use serde_json::json;

use super::isolated_guest::{
    project_captured_artifact, redact_isolated_capture, IsolatedGuestLease, IsolatedGuestPhase,
    IsolatedGuestSession,
};
use super::isolated_visual::{
    IsolatedVisualCleanupEvidence, IsolatedVisualLaunchContract, IsolatedVisualLifecycle,
    IsolatedVisualLifecycleState, IsolatedVisualManifest, IsolatedVisualResourceLimits,
    IsolatedVisualSecurityProfile, IsolatedVisualTerminalDisposition,
    ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION, ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION,
    MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID,
};
use super::isolated_visual_artifacts::{
    measure_open_isolated_visual_artifact, measure_open_isolated_visual_artifacts,
    measure_packaged_isolated_visual_artifacts, IsolatedVisualArtifactMeasurement,
    IsolatedVisualArtifactMeasurements, IsolatedVisualArtifactRole,
    IsolatedVisualPackagedArtifactReceipt,
};
use super::isolated_visual_channel::IsolatedVisualChannelBinding;
use super::isolated_visual_driver::IsolatedVisualRuntimeDriver;
use super::isolated_visual_frames::IsolatedVisualFrameCarrier;
use super::isolated_visual_helper::{
    IsolatedVisualHelperEvent, IsolatedVisualHelperEventCode, IsolatedVisualHelperSupervisor,
    IsolatedVisualHelperSupervisorState, ISOLATED_VISUAL_HELPER_CONTROL_BIND,
    ISOLATED_VISUAL_HELPER_CONTROL_START, ISOLATED_VISUAL_HELPER_CONTROL_STOP,
    ISOLATED_VISUAL_HELPER_EVENT_BYTES, ISOLATED_VISUAL_HELPER_EVENT_MAGIC,
    ISOLATED_VISUAL_HELPER_EVENT_VERSION,
};
use super::isolated_visual_helper_control::{
    read_isolated_visual_challenge, IsolatedVisualHelperControl, ISOLATED_VISUAL_CHALLENGE_BYTES,
};
use super::isolated_visual_input::{IsolatedVisualInputKeyState, IsolatedVisualInputMessage};
use super::isolated_visual_input_wire::IsolatedVisualInputWire;
use super::isolated_visual_protocol::{
    IsolatedVisualGuestHealth, IsolatedVisualGuestMessage, IsolatedVisualHostMessage,
    IsolatedVisualProtocolPayload, IsolatedVisualProtocolSession,
};
use super::isolated_visual_runtime::IsolatedVisualRuntimeSession;
use super::isolated_visual_stream::{IsolatedVisualStream, ISOLATED_VISUAL_GUEST_INPUT_COMMAND};
use super::types::{
    ComputerError, ComputerErrorCode, ComputerKey, ComputerResult, ComputerSurfaceBinding,
    PointerButton, PointerButtonState,
};

/// Fixed local bootstrap challenge. It is not a secret and never leaves this
/// process: the packaged supervisor reads a fresh challenge from the helper.
const REHEARSAL_CHALLENGE: [u8; ISOLATED_VISUAL_CHALLENGE_BYTES] = [0x5a; 32];

/// Canonical UUIDv4 used as the rehearsal request nonce, so the check stays
/// deterministic instead of drawing randomness on every call.
const REHEARSAL_NONCE: &str = "550e8400-e29b-41d4-a716-446655440000";

fn selfcheck_error(message: &'static str) -> ComputerError {
    ComputerError::new(ComputerErrorCode::Internal, message)
}

fn require(condition: bool, message: &'static str) -> ComputerResult<()> {
    if condition {
        Ok(())
    } else {
        Err(selfcheck_error(message))
    }
}

/// Synthetic rehearsal contract. Digests are constants, not measurements.
fn rehearsal_contract() -> IsolatedVisualLaunchContract {
    IsolatedVisualLaunchContract {
        run_id: "isolated-visual-selfcheck".into(),
        surface: ComputerSurfaceBinding {
            surface_id: "isolated-visual-selfcheck-surface".into(),
            incarnation: "isolated-visual-selfcheck-incarnation".into(),
        },
        input_domain_id: "isolated-visual-selfcheck-input".into(),
        manifest: IsolatedVisualManifest {
            schema_version: ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION,
            backend_id: MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID.into(),
            guest_protocol_version: ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION,
            helper_content_sha256: "0".repeat(64),
            helper_signing_requirement_sha256: "1".repeat(64),
            guest_image_sha256: "2".repeat(64),
            configuration_sha256: "3".repeat(64),
            security_profile: IsolatedVisualSecurityProfile::locked_down(),
            limits: IsolatedVisualResourceLimits::proof_defaults(),
        },
    }
}

/// Encodes one fixed-size helper event exactly as the packaged helper emits it.
fn helper_event_bytes(code: u16, detail: u32) -> [u8; ISOLATED_VISUAL_HELPER_EVENT_BYTES] {
    let mut bytes = [0_u8; ISOLATED_VISUAL_HELPER_EVENT_BYTES];
    bytes[0..4].copy_from_slice(&ISOLATED_VISUAL_HELPER_EVENT_MAGIC.to_be_bytes());
    bytes[4..6].copy_from_slice(&ISOLATED_VISUAL_HELPER_EVENT_VERSION.to_be_bytes());
    bytes[6..8].copy_from_slice(&code.to_be_bytes());
    bytes[8..12].copy_from_slice(&detail.to_be_bytes());
    bytes
}

/// Length-delimits one packet the way the private guest transport frames it.
fn length_delimited(packet: &[u8]) -> Vec<u8> {
    let mut framed = Vec::with_capacity(4 + packet.len());
    framed.extend_from_slice(&(packet.len() as u32).to_be_bytes());
    framed.extend_from_slice(packet);
    framed
}

/// Runs the whole rehearsal. `Ok(())` means every substrate contract below held
/// in this build; the error carries the first contract that did not.
pub(crate) fn run_isolated_visual_selfcheck() -> ComputerResult<()> {
    check_locked_down_profile()?;
    check_lifecycle_paths()?;
    check_surface_binding_rotation()?;
    check_authenticated_channel()?;
    check_one_agent_lease()?;
    check_secret_free_capture()?;
    check_protocol_envelopes()?;
    check_helper_supervisor()?;
    check_driven_session()?;
    check_driven_failure()?;
    check_artifact_receipts()?;
    #[cfg(unix)]
    bind_deadline_entrypoints();
    Ok(())
}

/// The closed profile must reject every host bridge: network, shared folder,
/// clipboard, credential forwarding, host input, USB, camera, microphone.
fn check_locked_down_profile() -> ComputerResult<()> {
    let locked = IsolatedVisualSecurityProfile::locked_down();
    locked.validate()?;
    for opened in [
        IsolatedVisualSecurityProfile {
            network_devices: 1,
            ..IsolatedVisualSecurityProfile::locked_down()
        },
        IsolatedVisualSecurityProfile {
            host_clipboard: true,
            ..IsolatedVisualSecurityProfile::locked_down()
        },
        IsolatedVisualSecurityProfile {
            shared_directories: true,
            ..IsolatedVisualSecurityProfile::locked_down()
        },
        IsolatedVisualSecurityProfile {
            credential_forwarding: true,
            ..IsolatedVisualSecurityProfile::locked_down()
        },
        IsolatedVisualSecurityProfile {
            host_input_forwarding: true,
            ..IsolatedVisualSecurityProfile::locked_down()
        },
        IsolatedVisualSecurityProfile {
            usb_passthrough: true,
            ..IsolatedVisualSecurityProfile::locked_down()
        },
        IsolatedVisualSecurityProfile {
            camera: true,
            ..IsolatedVisualSecurityProfile::locked_down()
        },
        IsolatedVisualSecurityProfile {
            microphone: true,
            ..IsolatedVisualSecurityProfile::locked_down()
        },
    ] {
        require(
            opened.validate().is_err(),
            "isolated visual profile admitted an unreviewed host bridge",
        )?;
    }
    IsolatedVisualResourceLimits::proof_defaults().validate()?;
    rehearsal_contract().validate()
}

/// Cancel, helper failure, and restart each land in a terminal state that still
/// demands exact cleanup evidence, and none of them resumes the lifecycle.
fn check_lifecycle_paths() -> ComputerResult<()> {
    let contract = rehearsal_contract();

    let mut cancelled = IsolatedVisualLifecycle::new(contract.clone())?;
    require(
        cancelled.state() == IsolatedVisualLifecycleState::Prepared,
        "isolated lifecycle did not start Prepared",
    )?;
    cancelled.begin_start()?;
    cancelled.mark_read_only_ready()?;
    cancelled.begin_stop(IsolatedVisualTerminalDisposition::Cancelled)?;
    cancelled.require_cleanup()?;
    require(
        cancelled.state() == IsolatedVisualLifecycleState::CleanupPending,
        "isolated cancel did not require cleanup",
    )?;
    let mut session = IsolatedVisualRuntimeSession::new(contract.clone(), REHEARSAL_CHALLENGE)?;
    require(
        session
            .complete_observed_cleanup(false, true, true, true)
            .is_err()
            && session
                .complete_observed_cleanup(true, false, true, true)
                .is_err()
            && session
                .complete_observed_cleanup(true, true, false, true)
                .is_err()
            && session
                .complete_observed_cleanup(true, true, true, false)
                .is_err(),
        "isolated cleanup accepted incomplete process/handle/overlay/cache evidence",
    )?;

    let mut failed = IsolatedVisualLifecycle::new(contract.clone())?;
    failed.begin_start()?;
    failed.fail()?;
    require(
        failed.state() == IsolatedVisualLifecycleState::CleanupPending,
        "isolated helper failure skipped cleanup",
    )?;

    let mut restarted = IsolatedVisualLifecycle::new(contract)?;
    restarted.begin_start()?;
    restarted.mark_read_only_ready()?;
    restarted.interrupt_on_restart()?;
    require(
        restarted.state() == IsolatedVisualLifecycleState::CleanupPending,
        "isolated restart resumed instead of interrupting",
    )?;
    restarted.validate()
}

/// A rotated incarnation is a new surface: cleanup evidence bound to the old one
/// must not satisfy it.
fn check_surface_binding_rotation() -> ComputerResult<()> {
    let issued = ComputerSurfaceBinding::issue();
    require(
        issued.is_issued(),
        "issued surface binding did not validate",
    )?;
    let rotated = issued.rotate_incarnation()?;
    require(
        rotated.surface_id() == issued.surface_id()
            && rotated.incarnation() != issued.incarnation(),
        "surface rotation changed identity instead of incarnation",
    )?;
    require(
        !ComputerSurfaceBinding::default().is_issued(),
        "an unissued surface binding validated",
    )
}

/// Binding packets, frame chunks, and input packets are authenticated and
/// bounded: tampering, a wrong challenge, and a stale frame all fail closed.
fn check_authenticated_channel() -> ComputerResult<()> {
    let contract = rehearsal_contract();
    let binding = IsolatedVisualChannelBinding::from_contract(&contract)?;
    let packet = binding.encode_header_and_payload(&REHEARSAL_CHALLENGE)?;
    require(
        IsolatedVisualChannelBinding::decode_header_and_payload(&packet, &REHEARSAL_CHALLENGE)?
            == binding,
        "isolated binding packet did not round-trip",
    )?;
    let mut tampered = packet.clone();
    tampered[16] ^= 1;
    require(
        IsolatedVisualChannelBinding::decode_header_and_payload(&tampered, &REHEARSAL_CHALLENGE)
            .is_err(),
        "isolated binding accepted a tampered packet",
    )?;
    require(
        IsolatedVisualChannelBinding::decode_header_and_payload(&packet, &[0x17; 32]).is_err(),
        "isolated binding accepted a foreign challenge",
    )?;
    let secret = binding.derive_channel_secret(&REHEARSAL_CHALLENGE)?;
    require(
        binding.confirmation_tag(&REHEARSAL_CHALLENGE)? != secret,
        "confirmation tag collided with the channel secret",
    )?;

    let mut guest =
        IsolatedVisualFrameCarrier::new_guest_with_challenge(&contract, &REHEARSAL_CHALLENGE)?;
    let mut host = IsolatedVisualFrameCarrier::new_host(&contract, &secret)?;
    let chunks = guest.seal_frame(1, REHEARSAL_NONCE, 2, 2, &[1, 2, 3, 4])?;
    let mut opened = None;
    for chunk in &chunks {
        opened = host.open_chunk(chunk)?;
    }
    let frame = opened.ok_or_else(|| selfcheck_error("isolated frame never completed"))?;
    require(
        frame.bytes == vec![1, 2, 3, 4] && frame.width == 2 && frame.height == 2,
        "isolated frame did not reassemble exactly",
    )?;
    let mut replay = IsolatedVisualFrameCarrier::new_host(&contract, &secret)?;
    let mut corrupt = chunks[0].clone();
    let last = corrupt.len() - 1;
    corrupt[last] ^= 1;
    require(
        replay.open_chunk(&corrupt).is_err(),
        "isolated frame carrier accepted a tampered chunk",
    )?;

    let mut gate = super::isolated_visual_input::IsolatedVisualInputGate::new(
        contract.manifest.limits.clone(),
    )?;
    let sender = IsolatedVisualInputWire::new_host_with_challenge(&contract, &REHEARSAL_CHALLENGE)?;
    let receiver = IsolatedVisualInputWire::new_guest(&contract, &secret)?;
    let challenge_receiver =
        IsolatedVisualInputWire::new_guest_with_challenge(&contract, &REHEARSAL_CHALLENGE)?;
    // The secret-taking and challenge-deriving constructors must agree, or a
    // supervisor could bind one endpoint to a key the other cannot open.
    let secret_sender = IsolatedVisualInputWire::new_host(&contract, &secret)?;
    gate.bind_frame(1, 2, 2)?;
    let sealed = sender.seal(
        &mut gate,
        1,
        1,
        REHEARSAL_NONCE,
        IsolatedVisualInputMessage::PointerMove { x: 1, y: 1 },
    )?;
    require(
        sealed.len() <= super::isolated_visual_input_wire::ISOLATED_VISUAL_INPUT_MAX_PACKET_BYTES,
        "isolated input packet exceeded its bound",
    )?;
    let mut guest_gate = super::isolated_visual_input::IsolatedVisualInputGate::new(
        contract.manifest.limits.clone(),
    )?;
    guest_gate.bind_frame(1, 2, 2)?;
    require(
        receiver.open(&mut guest_gate, &sealed)?
            == IsolatedVisualInputMessage::PointerMove { x: 1, y: 1 },
        "isolated input packet did not round-trip",
    )?;
    let mut challenge_gate = super::isolated_visual_input::IsolatedVisualInputGate::new(
        contract.manifest.limits.clone(),
    )?;
    challenge_gate.bind_frame(1, 2, 2)?;
    require(
        challenge_receiver.open(&mut challenge_gate, &sealed)?
            == IsolatedVisualInputMessage::PointerMove { x: 1, y: 1 },
        "challenge-derived and secret-derived input keys disagree",
    )?;
    require(
        secret_sender
            .seal(
                &mut gate,
                1,
                1,
                REHEARSAL_NONCE,
                IsolatedVisualInputMessage::PointerMove { x: 1, y: 1 },
            )
            .is_err(),
        "isolated input gate admitted a replayed sequence",
    )?;
    require(
        sender
            .seal(
                &mut gate,
                0,
                2,
                REHEARSAL_NONCE,
                IsolatedVisualInputMessage::Scroll {
                    delta_x: 0,
                    delta_y: 1,
                },
            )
            .is_err(),
        "isolated input gate admitted input against a stale frame",
    )?;

    // A held button or key must block termination, and poison must be terminal.
    let mut held = super::isolated_visual_input::IsolatedVisualInputGate::new(
        contract.manifest.limits.clone(),
    )?;
    held.bind_frame(1, 2, 2)?;
    held.admit(
        1,
        1,
        IsolatedVisualInputMessage::PointerButton {
            x: 1,
            y: 1,
            button: PointerButton::Primary,
            state: PointerButtonState::Down,
        },
    )?;
    require(
        held.terminal_check().is_err(),
        "isolated input terminated with a held pointer button",
    )?;
    held.admit(
        1,
        2,
        IsolatedVisualInputMessage::PointerButton {
            x: 1,
            y: 1,
            button: PointerButton::Primary,
            state: PointerButtonState::Up,
        },
    )?;
    held.admit(
        1,
        3,
        IsolatedVisualInputMessage::Key {
            key: ComputerKey::Enter,
            state: IsolatedVisualInputKeyState::Down,
        },
    )?;
    require(
        held.terminal_check().is_err(),
        "isolated input terminated with a pressed key",
    )?;
    held.admit(
        1,
        4,
        IsolatedVisualInputMessage::Key {
            key: ComputerKey::Enter,
            state: IsolatedVisualInputKeyState::Up,
        },
    )?;
    held.admit(
        1,
        5,
        IsolatedVisualInputMessage::Text {
            text: "rehearsal".into(),
        },
    )?;
    held.terminal_check()?;
    require(
        held.frame_sequence() == 1
            && held.next_input_sequence() == 5
            && held.accepted_events() == 5,
        "isolated input gate lost its exact admission counters",
    )?;
    held.poison();
    require(
        held.terminal_check().is_err()
            && held
                .admit(
                    1,
                    6,
                    IsolatedVisualInputMessage::Scroll {
                        delta_x: 0,
                        delta_y: 1,
                    },
                )
                .is_err(),
        "isolated input gate recovered from poison",
    )
}

/// One Agent holds one guest. A second Agent is denied, a stale lease revision
/// cannot control or mutate, and cancel still requires exact cleanup.
fn check_one_agent_lease() -> ComputerResult<()> {
    let mut guest = IsolatedGuestSession::create(rehearsal_contract(), REHEARSAL_CHALLENGE)?;
    require(
        guest.phase() == IsolatedGuestPhase::Create && guest.lease().is_none(),
        "isolated guest did not start in Create without a lease",
    )?;
    let lease = guest.acquire("selfcheck-agent-a")?;
    guest.drive_to_ready("selfcheck-agent-a", &lease)?;
    require(
        guest.phase() == IsolatedGuestPhase::Ready,
        "isolated guest did not reach Ready",
    )?;
    guest.drive_to_running("selfcheck-agent-a", &lease)?;
    require(
        guest.phase() == IsolatedGuestPhase::Running,
        "isolated guest did not reach Running",
    )?;
    guest.control("selfcheck-agent-a", &lease)?;
    require(
        guest.acquire("selfcheck-agent-b").is_err(),
        "isolated guest admitted a second agent",
    )?;
    require(
        guest.control("selfcheck-agent-b", &lease).is_err(),
        "isolated guest accepted control from an unleased agent",
    )?;
    let stale = IsolatedGuestLease {
        lease_id: lease.lease_id.clone(),
        agent_id: lease.agent_id.clone(),
        revision: lease.revision + 1,
    };
    let phase_before = guest.phase();
    require(
        guest.control("selfcheck-agent-a", &stale).is_err(),
        "isolated guest accepted a stale lease revision",
    )?;
    require(
        guest.cancel("selfcheck-agent-a", &stale).is_err(),
        "isolated guest cancelled on a stale lease revision",
    )?;
    require(
        guest.phase() == phase_before,
        "a denied stale-lease call mutated isolated guest state",
    )?;
    guest.cancel("selfcheck-agent-a", &lease)?;
    require(
        guest.phase() == IsolatedGuestPhase::Closing,
        "isolated cancel did not close the guest",
    )?;
    let contract = rehearsal_contract();
    // Evidence refuses to exist unless every check passed, so a coordinator
    // cannot even hold a half-satisfied cleanup receipt.
    require(
        IsolatedVisualCleanupEvidence::verified(contract.surface.clone(), true, true, true, false)
            .is_err(),
        "isolated cleanup evidence was minted with a surviving frame cache",
    )?;
    guest.complete_cleanup(&IsolatedVisualCleanupEvidence::verified(
        contract.surface.clone(),
        true,
        true,
        true,
        true,
    )?)?;

    let mut crashed = IsolatedGuestSession::create(rehearsal_contract(), REHEARSAL_CHALLENGE)?;
    let crashed_lease = crashed.acquire("selfcheck-agent-a")?;
    crashed.drive_to_ready("selfcheck-agent-a", &crashed_lease)?;
    crashed.fail_guest("selfcheck-agent-a", &crashed_lease)?;
    require(
        crashed.phase() == IsolatedGuestPhase::Failed,
        "isolated guest failure did not land in Failed",
    )?;
    require(
        crashed.acquire("selfcheck-agent-b").is_err(),
        "a failed isolated guest was re-acquirable",
    )
}

/// Capture projections carry no bytes and no host secret, and redaction fails
/// closed on anything it cannot strip.
fn check_secret_free_capture() -> ComputerResult<()> {
    let contract = rehearsal_contract();
    let binding = IsolatedVisualChannelBinding::from_contract(&contract)?;
    let secret = binding.derive_channel_secret(&REHEARSAL_CHALLENGE)?;
    let mut guest = IsolatedVisualFrameCarrier::new_guest(&contract, &secret)?;
    let mut host = IsolatedVisualFrameCarrier::new_host(&contract, &secret)?;
    let mut frame = None;
    for chunk in guest.seal_frame(1, REHEARSAL_NONCE, 2, 2, &[9, 9, 9, 9])? {
        frame = host.open_chunk(&chunk)?;
    }
    let frame = frame.ok_or_else(|| selfcheck_error("isolated capture frame never completed"))?;
    let artifact = project_captured_artifact(&frame);
    let encoded = serde_json::to_string(&artifact)
        .map_err(|_| selfcheck_error("isolated capture projection is not serializable"))?;
    require(
        !encoded.contains("bytes") && artifact.content_sha256.len() == 64,
        "isolated capture projection leaked frame bytes or lost its digest",
    )?;

    let redacted = redact_isolated_capture(&json!({
        "frameSequence": 1,
        "apiKey": "must-not-survive",
        "token": "must-not-survive",
        "overlayPath": "/must/not/survive",
        "clipboard": "must-not-survive",
    }))?;
    let redacted_text = redacted.to_string();
    for needle in [
        "apiKey",
        "token",
        "overlayPath",
        "clipboard",
        "must-not-survive",
    ] {
        require(
            !redacted_text.contains(needle),
            "isolated capture redaction left a forbidden field",
        )?;
    }
    require(
        redact_isolated_capture(&json!({"note": "host home is /Users/someone"})).is_err(),
        "isolated capture redaction admitted a host path needle",
    )
}

/// Signed protocol envelopes are directional, bounded, and replay resistant.
fn check_protocol_envelopes() -> ComputerResult<()> {
    let contract = rehearsal_contract();
    let secret = IsolatedVisualChannelBinding::from_contract(&contract)?
        .derive_channel_secret(&REHEARSAL_CHALLENGE)?;
    let mut host = IsolatedVisualProtocolSession::new_host(&contract, &secret)?;
    let mut guest = IsolatedVisualProtocolSession::new_guest(&contract, &secret)?;

    let observe = host.seal(
        REHEARSAL_NONCE.to_string(),
        0,
        0,
        IsolatedVisualProtocolPayload::HostToGuest(IsolatedVisualHostMessage::Observe {
            maximum_frame_bytes: contract.manifest.limits.encoded_frame_bytes,
            maximum_width: contract.manifest.limits.display_width,
            maximum_height: contract.manifest.limits.display_height,
        }),
    )?;
    require(
        observe.surface.surface_id() == contract.surface.surface_id()
            && observe.surface.incarnation() == contract.surface.incarnation(),
        "isolated protocol envelope lost its exact surface binding",
    )?;
    guest.open(observe.clone())?;
    require(
        guest.open(observe.clone()).is_err(),
        "isolated protocol accepted a replayed envelope",
    )?;
    require(
        guest
            .seal(
                REHEARSAL_NONCE.to_string(),
                0,
                0,
                IsolatedVisualProtocolPayload::HostToGuest(IsolatedVisualHostMessage::Stop),
            )
            .is_err(),
        "isolated protocol guest sealed a host-direction payload",
    )?;

    let health = guest.seal(
        REHEARSAL_NONCE.to_string(),
        0,
        0,
        IsolatedVisualProtocolPayload::GuestToHost(IsolatedVisualGuestMessage::Health {
            state: IsolatedVisualGuestHealth::ReadOnlyReady,
        }),
    )?;
    host.open(health.clone())?;
    let mut forged = health.clone();
    forged.authenticator_sha256 = "f".repeat(64);
    require(
        host.open(forged).is_err(),
        "isolated protocol accepted a forged authenticator",
    )
}

/// The helper supervisor refuses out-of-order events and controls, and a
/// failure event is terminal.
fn check_helper_supervisor() -> ComputerResult<()> {
    let contract = rehearsal_contract();
    let binding = IsolatedVisualChannelBinding::from_contract(&contract)?;
    let mut supervisor = IsolatedVisualHelperSupervisor::new();
    require(
        supervisor.state() == IsolatedVisualHelperSupervisorState::AwaitingPrepared,
        "helper supervisor did not start awaiting Prepared",
    )?;
    require(
        supervisor.start().is_err(),
        "helper supervisor started before Prepared",
    )?;
    let prepared = IsolatedVisualHelperEvent::decode(&helper_event_bytes(
        IsolatedVisualHelperEventCode::Prepared as u16,
        0,
    ))?;
    supervisor.accept_event(prepared)?;
    require(
        supervisor.start()? == ISOLATED_VISUAL_HELPER_CONTROL_START,
        "helper supervisor emitted the wrong start control",
    )?;
    supervisor.accept_event(IsolatedVisualHelperEvent::decode(&helper_event_bytes(
        IsolatedVisualHelperEventCode::Running as u16,
        0,
    ))?)?;
    let bind = supervisor.bind(&binding, &REHEARSAL_CHALLENGE)?;
    require(
        bind.first() == Some(&ISOLATED_VISUAL_HELPER_CONTROL_BIND),
        "helper supervisor emitted the wrong bind control",
    )?;
    supervisor.accept_event(IsolatedVisualHelperEvent::decode(&helper_event_bytes(
        IsolatedVisualHelperEventCode::Bound as u16,
        0,
    ))?)?;
    require(
        supervisor.stop()? == ISOLATED_VISUAL_HELPER_CONTROL_STOP,
        "helper supervisor emitted the wrong stop control",
    )?;

    require(
        IsolatedVisualHelperEvent::decode(&[0_u8; ISOLATED_VISUAL_HELPER_EVENT_BYTES]).is_err(),
        "helper event decode accepted an unmagicked event",
    )?;
    let mut failing = IsolatedVisualHelperSupervisor::new();
    failing.accept_event(IsolatedVisualHelperEvent::decode(&helper_event_bytes(
        IsolatedVisualHelperEventCode::Failure as u16,
        5,
    ))?)?;
    require(
        matches!(
            failing.state(),
            IsolatedVisualHelperSupervisorState::Failed(_)
        ) && failing.start().is_err(),
        "helper supervisor restarted after a failure event",
    )?;

    let mut challenge_source = Cursor::new(REHEARSAL_CHALLENGE.to_vec());
    require(
        read_isolated_visual_challenge(&mut challenge_source)? == REHEARSAL_CHALLENGE,
        "helper challenge read did not round-trip",
    )?;
    require(
        read_isolated_visual_challenge(&mut Cursor::new(vec![0_u8; 8])).is_err(),
        "helper challenge read accepted a truncated challenge",
    )
}

/// Drives the whole seam the packaged supervisor drives — helper controls,
/// frame reads, input writes, stop, and cleanup — over in-memory endpoints.
fn check_driven_session() -> ComputerResult<()> {
    let contract = rehearsal_contract();
    let runtime = IsolatedVisualRuntimeSession::new(contract.clone(), REHEARSAL_CHALLENGE)?;
    require(
        runtime.lifecycle_state() == IsolatedVisualLifecycleState::Prepared
            && runtime.input_frame_sequence() == 0
            && runtime.input_sequence() == 0,
        "isolated runtime session did not start clean",
    )?;

    let mut events = Vec::new();
    for code in [
        IsolatedVisualHelperEventCode::Prepared,
        IsolatedVisualHelperEventCode::Running,
        IsolatedVisualHelperEventCode::Bound,
        IsolatedVisualHelperEventCode::Stopped,
    ] {
        events.extend_from_slice(&helper_event_bytes(code as u16, 0));
    }

    let secret = IsolatedVisualChannelBinding::from_contract(&contract)?
        .derive_channel_secret(&REHEARSAL_CHALLENGE)?;
    let mut sender = IsolatedVisualFrameCarrier::new_guest(&contract, &secret)?;
    let mut frame_bytes = Vec::new();
    for chunk in sender.seal_frame(1, REHEARSAL_NONCE, 2, 2, &[4, 3, 2, 1])? {
        frame_bytes.extend_from_slice(&length_delimited(&chunk));
    }

    let mut driver = IsolatedVisualRuntimeDriver::new(
        runtime,
        IsolatedVisualHelperControl::new(Cursor::new(events), Vec::new()),
        IsolatedVisualStream::new(Cursor::new(frame_bytes), Vec::new()),
    );

    driver.receive_helper_event()?;
    driver.start()?;
    driver.receive_helper_event()?;
    driver.bind()?;
    driver.receive_helper_event()?;
    let frame = driver
        .read_frame()?
        .ok_or_else(|| selfcheck_error("driven isolated frame never completed"))?;
    require(
        frame.bytes == vec![4, 3, 2, 1],
        "driven isolated frame did not reassemble exactly",
    )?;
    driver.write_input(
        1,
        REHEARSAL_NONCE,
        IsolatedVisualInputMessage::PointerMove { x: 1, y: 1 },
    )?;
    driver.stop(IsolatedVisualTerminalDisposition::Cancelled)?;
    driver.receive_helper_event()?;
    require(
        driver.runtime().lifecycle_state() == IsolatedVisualLifecycleState::CleanupPending,
        "driven isolated stop did not require cleanup",
    )?;
    require(
        driver
            .complete_observed_cleanup(true, true, false, true)
            .is_err(),
        "driven isolated cleanup accepted a surviving overlay",
    )?;
    driver.complete_observed_cleanup(true, true, true, true)?;
    require(
        driver.runtime().lifecycle_state() == IsolatedVisualLifecycleState::Terminated,
        "driven isolated cleanup did not terminate the lifecycle",
    )?;

    let (mut runtime, helper, stream) = driver.into_parts();
    let (_events, controls) = helper.into_parts();
    let (_frames, inputs) = stream.into_parts();
    require(
        controls.first() == Some(&ISOLATED_VISUAL_HELPER_CONTROL_START),
        "driven isolated session did not emit the start control first",
    )?;
    require(
        controls.contains(&ISOLATED_VISUAL_HELPER_CONTROL_STOP),
        "driven isolated session never emitted the stop control",
    )?;
    require(
        inputs.len() > 4 && !inputs.is_empty(),
        "driven isolated session never wrote a length-delimited input packet",
    )?;
    require(
        ISOLATED_VISUAL_GUEST_INPUT_COMMAND != 0,
        "isolated guest input command byte is unset",
    )?;
    require(
        runtime.fail().is_err(),
        "a terminated isolated runtime accepted a later failure",
    )?;
    runtime.interrupt_on_restart()?;
    require(
        runtime.terminal_check().is_err(),
        "restart interruption left the isolated input gate live",
    )
}

/// A packaged receipt only validates against the manifest it was measured for,
/// and every signed-helper boundary flag must hold for it to exist at all.
fn check_artifact_receipts() -> ComputerResult<()> {
    let contract = rehearsal_contract();
    let manifest = &contract.manifest;

    let measurements = IsolatedVisualArtifactMeasurements {
        helper: IsolatedVisualArtifactMeasurement {
            role: IsolatedVisualArtifactRole::HelperExecutable,
            content_sha256: manifest.helper_content_sha256.clone(),
            bytes: 1024,
        },
        guest_image: IsolatedVisualArtifactMeasurement {
            role: IsolatedVisualArtifactRole::GuestImage,
            content_sha256: manifest.guest_image_sha256.clone(),
            bytes: 4096,
        },
        configuration: IsolatedVisualArtifactMeasurement {
            role: IsolatedVisualArtifactRole::Configuration,
            content_sha256: manifest.configuration_sha256.clone(),
            bytes: 512,
        },
    };
    measurements.validate()?;
    measurements.validate_content_against_manifest(manifest)?;

    // An empty artifact and a role swap are both refused.
    require(
        IsolatedVisualArtifactMeasurement {
            role: IsolatedVisualArtifactRole::HelperExecutable,
            content_sha256: manifest.helper_content_sha256.clone(),
            bytes: 0,
        }
        .validate()
        .is_err(),
        "artifact measurement admitted an empty artifact",
    )?;
    let mut swapped = measurements.clone();
    swapped.helper.role = IsolatedVisualArtifactRole::Configuration;
    require(
        swapped.validate().is_err(),
        "artifact receipt admitted an artifact in the wrong role",
    )?;

    // Content that does not match the manifest is unauthorized, not merely invalid.
    let mut foreign = measurements.clone();
    foreign.guest_image.content_sha256 = "9".repeat(64);
    require(
        foreign.validate_content_against_manifest(manifest).is_err(),
        "artifact receipt matched a manifest it was not measured for",
    )?;

    let receipt = IsolatedVisualPackagedArtifactReceipt::verified(
        manifest.helper_signing_requirement_sha256.clone(),
        measurements.clone(),
    )?;
    receipt.validate()?;
    receipt.validate_against_manifest(manifest)?;
    require(
        receipt.measurements() == &measurements,
        "packaged receipt lost its exact measurements",
    )?;
    require(
        receipt.helper_signing_requirement_sha256() == manifest.helper_signing_requirement_sha256,
        "packaged receipt lost its signing requirement digest",
    )?;
    require(
        IsolatedVisualPackagedArtifactReceipt::verified(
            "not-a-digest".into(),
            measurements.clone(),
        )
        .is_err(),
        "packaged receipt was minted without a signing requirement digest",
    )?;

    // A receipt measured for one manifest must not satisfy another.
    let mut other = contract.clone();
    other.manifest.helper_signing_requirement_sha256 = "8".repeat(64);
    require(
        receipt.validate_against_manifest(&other.manifest).is_err(),
        "packaged receipt satisfied a foreign signing requirement",
    )?;

    let encoded = serde_json::to_string(&receipt)
        .map_err(|_| selfcheck_error("packaged receipt is not serializable"))?;
    for needle in ["/", "\\", "descriptor", "pid", "challenge", "secret"] {
        require(
            !encoded.contains(needle),
            "packaged receipt leaked a path, descriptor, or secret",
        )?;
    }

    bind_measurement_entrypoints();
    Ok(())
}

/// Binds the artifact measurement entrypoints.
///
/// Measuring an artifact needs a real read-only descriptor, and the packaged
/// variant needs a signed application bundle, so neither belongs inside a
/// status read. Their addresses are bound here; the unit tests measure real
/// files, and the packaged supervisor is what discovers a real bundle.
fn bind_measurement_entrypoints() {
    let _: fn(
        &mut std::fs::File,
        IsolatedVisualArtifactRole,
    ) -> ComputerResult<IsolatedVisualArtifactMeasurement> = measure_open_isolated_visual_artifact;
    let _: fn(
        &mut std::fs::File,
        &mut std::fs::File,
        &mut std::fs::File,
    ) -> ComputerResult<IsolatedVisualArtifactMeasurements> =
        measure_open_isolated_visual_artifacts;
    let _: fn(
        &super::isolated_visual::IsolatedVisualManifest,
    ) -> ComputerResult<IsolatedVisualPackagedArtifactReceipt> =
        measure_packaged_isolated_visual_artifacts;
}

/// A helper failure mid-session poisons input, blocks a later stop, and still
/// demands the same exact cleanup evidence an operator stop would.
fn check_driven_failure() -> ComputerResult<()> {
    let contract = rehearsal_contract();
    let mut events = Vec::new();
    for code in [
        IsolatedVisualHelperEventCode::Prepared,
        IsolatedVisualHelperEventCode::Running,
    ] {
        events.extend_from_slice(&helper_event_bytes(code as u16, 0));
    }

    let mut driver = IsolatedVisualRuntimeDriver::new(
        IsolatedVisualRuntimeSession::new(contract.clone(), REHEARSAL_CHALLENGE)?,
        IsolatedVisualHelperControl::new(Cursor::new(events), Vec::new()),
        IsolatedVisualStream::new(Cursor::new(Vec::<u8>::new()), Vec::<u8>::new()),
    );
    driver.receive_helper_event()?;
    driver.start()?;
    driver.receive_helper_event()?;
    driver.fail()?;
    require(
        driver.runtime().lifecycle_state() == IsolatedVisualLifecycleState::CleanupPending,
        "driven isolated failure did not require cleanup",
    )?;
    require(
        driver
            .stop(IsolatedVisualTerminalDisposition::Cancelled)
            .is_err(),
        "a failed isolated session still accepted a stop control",
    )?;
    require(
        IsolatedVisualCleanupEvidence::verified(contract.surface.clone(), false, true, true, true)
            .is_err(),
        "isolated cleanup evidence was minted with a surviving helper process",
    )?;
    driver.complete_cleanup(&IsolatedVisualCleanupEvidence::verified(
        contract.surface.clone(),
        true,
        true,
        true,
        true,
    )?)?;
    require(
        driver.runtime().lifecycle_state() == IsolatedVisualLifecycleState::Terminated,
        "driven isolated failure did not terminate after cleanup",
    )
}

/// Binds the deadline-bounded endpoints.
///
/// These are the only substrate entrypoints that need a real descriptor: they
/// poll for readiness so a stalled guest or a dead helper cannot hold the host
/// forever. Opening a socket pair inside a status read would be the wrong
/// trade, so this takes their addresses instead of calling them, which is
/// enough for the lib target to see them as live code. The deadline behavior
/// itself is exercised for real over a socket pair by the failure and cleanup
/// tests; the packaged supervisor is what drives them in production.
#[cfg(unix)]
fn bind_deadline_entrypoints() {
    type Helper = IsolatedVisualHelperControl<UnixStream, UnixStream>;
    type Stream = IsolatedVisualStream<UnixStream, UnixStream>;
    type Driver = IsolatedVisualRuntimeDriver<UnixStream, UnixStream, UnixStream, UnixStream>;
    type Session = IsolatedVisualRuntimeSession;
    type Frame = super::isolated_visual_frames::IsolatedVisualFrame;

    let _: fn(&mut Helper, &mut Session, Duration) -> ComputerResult<()> =
        Helper::receive_event_with_timeout;
    let _: fn(&mut Helper, &mut Session, Duration) -> ComputerResult<()> =
        Helper::send_start_with_timeout;
    let _: fn(&mut Helper, &mut Session, Duration) -> ComputerResult<()> =
        Helper::send_binding_with_timeout;
    let _: fn(
        &mut Helper,
        &mut Session,
        IsolatedVisualTerminalDisposition,
        Duration,
    ) -> ComputerResult<()> = Helper::send_stop_with_timeout;
    let _: fn(&mut UnixStream, Duration) -> ComputerResult<[u8; 32]> =
        super::isolated_visual_helper_control::read_isolated_visual_challenge_with_timeout;

    let _: fn(&mut Stream, &mut Session, Duration) -> ComputerResult<Option<Frame>> =
        Stream::read_frame_chunk_with_timeout;
    let _: fn(
        &mut Stream,
        &mut Session,
        u64,
        &str,
        IsolatedVisualInputMessage,
        Duration,
    ) -> ComputerResult<()> = Stream::write_input_with_timeout;

    let _: fn(&mut Driver, Duration) -> ComputerResult<()> = Driver::start_with_timeout;
    let _: fn(&mut Driver, Duration) -> ComputerResult<()> =
        Driver::receive_helper_event_with_timeout;
    let _: fn(&mut Driver, Duration) -> ComputerResult<()> = Driver::bind_with_timeout;
    let _: fn(&mut Driver, Duration) -> ComputerResult<Option<Frame>> =
        Driver::read_frame_with_timeout;
    let _: fn(&mut Driver, u64, &str, IsolatedVisualInputMessage, Duration) -> ComputerResult<()> =
        Driver::write_input_with_timeout;
    let _: fn(&mut Driver, IsolatedVisualTerminalDisposition, Duration) -> ComputerResult<()> =
        Driver::stop_with_timeout;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substrate_selfcheck_holds_in_this_build() {
        run_isolated_visual_selfcheck().expect("isolated visual substrate self-check must hold");
    }

    #[test]
    fn selfcheck_is_deterministic_across_runs() {
        for _ in 0..8 {
            run_isolated_visual_selfcheck().unwrap();
        }
    }
}
