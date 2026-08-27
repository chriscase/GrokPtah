//! Packaged isolated-guest boundary proof, reachable only through the crate's
//! public surface.
//!
//! Scope, stated exactly: everything here is **source and protocol proof**. No
//! test in this file boots a guest, spawns the packaged helper, opens a real
//! private channel, renders a frame, dispatches host input, or touches
//! Virtualization.framework. What it proves is the refusal — which launches,
//! bindings, leases, messages, and artifacts are rejected, and that the public
//! projections of an accepted one carry no secret, path, descriptor, or frame
//! byte. Hardware proof (signed helper, guest boot, rendered frames, host
//! input, reap, soak) is a separate macOS campaign and nothing below stands in
//! for it.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;

use grokptah_agent_bridge::computer_use::{
    measure_open_isolated_visual_artifact, measure_open_isolated_visual_artifacts,
    project_captured_artifact, redact_isolated_capture, ComputerErrorCode, ComputerSurfaceBinding,
    IsolatedGuestPhase, IsolatedGuestSession, IsolatedVisualArtifactRole,
    IsolatedVisualChannelRole, IsolatedVisualFrame, IsolatedVisualGuestBinding,
    IsolatedVisualGuestHealth, IsolatedVisualGuestMessage, IsolatedVisualHostMessage,
    IsolatedVisualLaunchContract, IsolatedVisualLaunchDescriptors, IsolatedVisualManifest,
    IsolatedVisualProtocolPayload, IsolatedVisualProtocolSession, IsolatedVisualResourceLimits,
    IsolatedVisualSecurityProfile, ISOLATED_VISUAL_CHANNEL_SECRET_BYTES,
    ISOLATED_VISUAL_FIRST_PRIVATE_DESCRIPTOR, ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION,
    ISOLATED_VISUAL_LAUNCH_CHANNEL_COUNT, ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION,
    ISOLATED_VISUAL_MAX_CONFIGURATION_BYTES, ISOLATED_VISUAL_MAX_DESCRIPTOR,
    MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID,
};
use serde_json::json;
use tempfile::{tempdir, TempDir};

fn manifest() -> IsolatedVisualManifest {
    IsolatedVisualManifest {
        schema_version: ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION,
        backend_id: MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID.into(),
        guest_protocol_version: ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION,
        helper_content_sha256: "a".repeat(64),
        helper_signing_requirement_sha256: "b".repeat(64),
        guest_image_sha256: "c".repeat(64),
        configuration_sha256: "d".repeat(64),
        security_profile: IsolatedVisualSecurityProfile::locked_down(),
        limits: IsolatedVisualResourceLimits::proof_defaults(),
    }
}

fn contract() -> IsolatedVisualLaunchContract {
    IsolatedVisualLaunchContract {
        run_id: "run-boundary".into(),
        surface: surface("surface-boundary", "incarnation-boundary"),
        input_domain_id: "input-boundary".into(),
        manifest: manifest(),
    }
}

fn surface(surface_id: &str, incarnation: &str) -> ComputerSurfaceBinding {
    serde_json::from_value(json!({
        "surfaceId": surface_id,
        "incarnation": incarnation,
    }))
    .expect("surface binding")
}

/// Canonical UUIDv4 request nonces: the protocol refuses anything else.
const NONCE_A: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";

fn descriptors() -> IsolatedVisualLaunchDescriptors {
    IsolatedVisualLaunchDescriptors {
        process_id: 9_001,
        control: 3,
        event: 4,
        input: 5,
        frame: 6,
        challenge: 7,
    }
}

// ---------------------------------------------------------------------------
// Launch descriptor identity and completeness
// ---------------------------------------------------------------------------

#[test]
fn a_complete_private_launch_is_admitted_with_all_five_roles() {
    let admitted = descriptors().admit().expect("complete launch");
    assert_eq!(
        admitted.channel_count(),
        ISOLATED_VISUAL_LAUNCH_CHANNEL_COUNT
    );
    assert_eq!(
        admitted.roles(),
        [
            IsolatedVisualChannelRole::Control,
            IsolatedVisualChannelRole::Event,
            IsolatedVisualChannelRole::Input,
            IsolatedVisualChannelRole::Frame,
            IsolatedVisualChannelRole::Challenge,
        ]
    );
}

#[test]
fn a_launch_that_hands_back_a_standard_stream_fails_closed() {
    // The regression this pins: the previous macOS-only check accepted 0, 1,
    // and 2, so a shim could have handed the supervisor stdin/stdout/stderr as
    // private guest channels.
    for stream in [0_i64, 1, 2] {
        let mut raw = descriptors();
        raw.frame = stream;
        let error = raw.admit().expect_err("standard stream must fail closed");
        assert_eq!(error.code, ComputerErrorCode::BackendFailure);
        assert!(
            error.message.contains("standard stream"),
            "{}",
            error.message
        );
    }
    assert_eq!(ISOLATED_VISUAL_FIRST_PRIVATE_DESCRIPTOR, 3);
}

#[test]
fn an_incomplete_or_aliased_launch_fails_closed() {
    let mut missing = descriptors();
    missing.challenge = -1;
    assert_eq!(
        missing.admit().unwrap_err().code,
        ComputerErrorCode::BackendFailure
    );

    let mut aliased = descriptors();
    aliased.input = aliased.event;
    let error = aliased.admit().unwrap_err();
    assert_eq!(error.code, ComputerErrorCode::BackendFailure);
    assert!(error.message.contains("aliases"), "{}", error.message);

    let mut out_of_range = descriptors();
    out_of_range.control = ISOLATED_VISUAL_MAX_DESCRIPTOR + 1;
    assert_eq!(
        out_of_range.admit().unwrap_err().code,
        ComputerErrorCode::LimitReached
    );
}

#[test]
fn a_launch_that_names_this_process_fails_closed() {
    let mut raw = descriptors();
    raw.process_id = i64::from(std::process::id());
    assert_eq!(
        raw.admit().unwrap_err().code,
        ComputerErrorCode::ForbiddenAction
    );

    for pid in [-1_i64, 0, 1] {
        let mut raw = descriptors();
        raw.process_id = pid;
        assert_eq!(
            raw.admit().unwrap_err().code,
            ComputerErrorCode::BackendFailure
        );
    }
}

#[test]
fn an_admitted_launch_never_projects_its_descriptors() {
    // Distinctive numbers so a match can only be a real leak.
    let raw = IsolatedVisualLaunchDescriptors {
        process_id: 90_001,
        control: 40_003,
        event: 40_004,
        input: 40_005,
        frame: 40_006,
        challenge: 40_007,
    };
    let printed = format!("{:?}", raw.admit().unwrap());
    for needle in ["90001", "40003", "40004", "40005", "40006", "40007"] {
        assert!(!printed.contains(needle), "leaked {needle} in {printed}");
    }
    // Roles are public identity and must survive.
    assert!(printed.contains("Control") && printed.contains("Challenge"));
}

// ---------------------------------------------------------------------------
// Guest / session / run binding
// ---------------------------------------------------------------------------

#[test]
fn a_guest_binding_is_the_exact_run_surface_and_input_domain() {
    let contract = contract();
    let binding = IsolatedVisualGuestBinding::from_contract(&contract).unwrap();
    assert_eq!(binding.run_id, contract.run_id);
    assert_eq!(binding.surface, contract.surface);
    assert_eq!(binding.input_domain_id, contract.input_domain_id);
    binding.validate().unwrap();
}

#[test]
fn a_binding_from_a_malformed_contract_is_refused() {
    let mut escaping = contract();
    escaping.run_id = "../../elsewhere".into();
    assert_eq!(
        IsolatedVisualGuestBinding::from_contract(&escaping)
            .unwrap_err()
            .code,
        ComputerErrorCode::InvalidRequest
    );

    let mut aliased_domain = contract();
    aliased_domain.input_domain_id = aliased_domain.surface.surface_id().to_string();
    assert_eq!(
        IsolatedVisualGuestBinding::from_contract(&aliased_domain)
            .unwrap_err()
            .code,
        ComputerErrorCode::InvalidRequest
    );
}

#[test]
fn bindings_that_differ_in_any_component_are_not_equal() {
    let base = IsolatedVisualGuestBinding::from_contract(&contract()).unwrap();

    let mut other_run = base.clone();
    other_run.run_id = "run-elsewhere".into();
    let mut other_surface = base.clone();
    other_surface.surface = surface("surface-elsewhere", "incarnation-boundary");
    let mut rotated = base.clone();
    rotated.surface = surface("surface-boundary", "incarnation-rotated");
    let mut other_domain = base.clone();
    other_domain.input_domain_id = "input-elsewhere".into();

    for wrong in [other_run, other_surface, rotated, other_domain] {
        assert_ne!(base, wrong);
    }
}

// ---------------------------------------------------------------------------
// Exactly one agent per guest, over the public guest session
// ---------------------------------------------------------------------------

#[test]
fn a_second_agent_cannot_take_a_leased_guest() {
    let mut guest = IsolatedGuestSession::create(contract(), [7; 32]).unwrap();
    let lease = guest.acquire("agent-a").unwrap();
    assert_eq!(
        guest.acquire("agent-b").unwrap_err().code,
        ComputerErrorCode::Conflict
    );
    // Re-acquiring as the same agent is idempotent, not a second lease.
    assert_eq!(guest.acquire("agent-a").unwrap(), lease);
}

#[test]
fn a_stale_lease_cannot_drive_or_cancel_a_guest() {
    let mut guest = IsolatedGuestSession::create(contract(), [7; 32]).unwrap();
    let lease = guest.acquire("agent-a").unwrap();
    guest.drive_to_ready("agent-a", &lease).unwrap();
    guest.drive_to_running("agent-a", &lease).unwrap();

    let mut stale = lease.clone();
    stale.revision += 1;
    assert_eq!(
        guest.control("agent-a", &stale).unwrap_err().code,
        ComputerErrorCode::StaleObservation
    );
    assert_eq!(
        guest.cancel("agent-a", &stale).unwrap_err().code,
        ComputerErrorCode::StaleObservation
    );
    // A refused stale call does not mutate the guest.
    assert_eq!(guest.phase(), IsolatedGuestPhase::Running);
    guest.control("agent-a", &lease).unwrap();
}

#[test]
fn another_agents_lease_is_refused_even_when_it_is_live() {
    let mut guest = IsolatedGuestSession::create(contract(), [7; 32]).unwrap();
    let lease = guest.acquire("agent-a").unwrap();
    guest.drive_to_ready("agent-a", &lease).unwrap();
    guest.drive_to_running("agent-a", &lease).unwrap();
    assert_eq!(
        guest.control("agent-b", &lease).unwrap_err().code,
        ComputerErrorCode::ForbiddenAction
    );
}

#[test]
fn cancel_revokes_the_lease_and_cleanup_stays_mandatory() {
    let mut guest = IsolatedGuestSession::create(contract(), [7; 32]).unwrap();
    let lease = guest.acquire("agent-a").unwrap();
    guest.drive_to_ready("agent-a", &lease).unwrap();
    guest.drive_to_running("agent-a", &lease).unwrap();
    guest.cancel("agent-a", &lease).unwrap();

    assert!(guest.lease().is_none());
    assert_eq!(guest.phase(), IsolatedGuestPhase::Closing);
    // A second cancel on the same lease has nothing left to revoke.
    assert!(guest.cancel("agent-a", &lease).is_err());
    // The guest is not re-acquirable after close.
    assert_eq!(
        guest.acquire("agent-a").unwrap_err().code,
        ComputerErrorCode::InvalidState
    );
}

#[test]
fn a_lost_helper_fails_the_guest_and_revokes_its_lease() {
    let mut guest = IsolatedGuestSession::create(contract(), [7; 32]).unwrap();
    let lease = guest.acquire("agent-a").unwrap();
    guest.drive_to_ready("agent-a", &lease).unwrap();
    guest.drive_to_running("agent-a", &lease).unwrap();
    guest.fail_guest("agent-a", &lease).unwrap();

    assert_eq!(guest.phase(), IsolatedGuestPhase::Failed);
    assert!(guest.lease().is_none());
    assert_eq!(
        guest.control("agent-a", &lease).unwrap_err().code,
        ComputerErrorCode::Unauthorized
    );
}

// ---------------------------------------------------------------------------
// Capture projection: no frame bytes, no secrets, no host paths
// ---------------------------------------------------------------------------

#[test]
fn a_capture_projection_drops_every_frame_byte() {
    // A real frame carries pixels and a request nonce. The projection keeps
    // geometry and a content digest, and nothing that could reconstruct or
    // correlate the image.
    let pixels: Vec<u8> = (0..4_096_u32).map(|byte| (byte % 251) as u8).collect();
    let frame = IsolatedVisualFrame {
        frame_sequence: 12,
        request_nonce: "nonce-capture-should-not-appear".into(),
        width: 800,
        height: 600,
        content_sha256: [0xAB; 32],
        bytes: pixels.clone(),
    };

    let projected = project_captured_artifact(&frame);
    assert_eq!(projected.frame_sequence, 12);
    assert_eq!(projected.width, 800);
    assert_eq!(projected.height, 600);
    assert_eq!(projected.content_sha256, "ab".repeat(32));

    let serialized = serde_json::to_string(&projected).unwrap();
    let mut keys: Vec<String> =
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&serialized)
            .unwrap()
            .keys()
            .cloned()
            .collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["contentSha256", "frameSequence", "height", "width"],
        "the projection must expose geometry and a digest, and nothing else"
    );
    assert!(!serialized.contains("nonce-capture-should-not-appear"));
    assert!(!serialized.contains("bytes"));
    // No run of frame bytes survives into the projection.
    let hex: String = pixels[..32].iter().map(|b| format!("{b:02x}")).collect();
    assert!(!serialized.contains(&hex), "projection leaked frame bytes");
}

#[test]
fn redaction_strips_forbidden_keys_and_refuses_leftover_needles() {
    let redacted = redact_isolated_capture(&json!({
        "title": "Untitled",
        "apiKey": "sk-live-should-vanish",
        "overlayPath": "/private/var/folders/xx/overlay.img",
        "nested": { "channelSecret": "deadbeef", "ok": "kept" },
    }))
    .unwrap();
    let text = serde_json::to_string(&redacted).unwrap();
    for needle in [
        "sk-live-should-vanish",
        "apiKey",
        "overlayPath",
        "channelSecret",
        "deadbeef",
    ] {
        assert!(!text.contains(needle), "leaked {needle} in {text}");
    }
    assert!(text.contains("Untitled"));
    assert!(text.contains("kept"));

    // A needle that survives key-stripping fails the whole capture closed.
    for hostile in [
        json!({ "note": "/Users/someone/Desktop/secret.png" }),
        json!({ "note": "clipboard: card 4111 1111 1111 1111" }),
        json!({ "note": "https://internal.example/callback" }),
        json!({ "deep": [{ "note": "password=hunter2" }] }),
        json!({ "note": "ssid=CorpWifi" }),
    ] {
        assert_eq!(
            redact_isolated_capture(&hostile).unwrap_err().code,
            ComputerErrorCode::ForbiddenAction,
            "hostile capture {hostile} must fail closed"
        );
    }
}

// ---------------------------------------------------------------------------
// Protocol: oversized and misbound messages
// ---------------------------------------------------------------------------

#[test]
fn an_observe_request_beyond_the_measured_manifest_is_refused() {
    let contract = contract();
    let secret = [3_u8; ISOLATED_VISUAL_CHANNEL_SECRET_BYTES];
    let mut host = IsolatedVisualProtocolSession::new_host(&contract, &secret).unwrap();
    let limits = &contract.manifest.limits;

    for oversized in [
        IsolatedVisualHostMessage::Observe {
            maximum_frame_bytes: limits.encoded_frame_bytes + 1,
            maximum_width: limits.display_width,
            maximum_height: limits.display_height,
        },
        IsolatedVisualHostMessage::Observe {
            maximum_frame_bytes: limits.encoded_frame_bytes,
            maximum_width: limits.display_width + 1,
            maximum_height: limits.display_height,
        },
        IsolatedVisualHostMessage::Observe {
            maximum_frame_bytes: limits.encoded_frame_bytes,
            maximum_width: limits.display_width,
            maximum_height: limits.display_height + 1,
        },
        IsolatedVisualHostMessage::Observe {
            maximum_frame_bytes: 0,
            maximum_width: limits.display_width,
            maximum_height: limits.display_height,
        },
    ] {
        assert_eq!(
            host.seal(
                "nonce-observe".into(),
                0,
                1,
                IsolatedVisualProtocolPayload::HostToGuest(oversized),
            )
            .unwrap_err()
            .code,
            ComputerErrorCode::LimitReached
        );
    }
}

#[test]
fn an_oversized_or_malformed_guest_frame_is_refused() {
    let contract = contract();
    let secret = [3_u8; ISOLATED_VISUAL_CHANNEL_SECRET_BYTES];
    let mut guest = IsolatedVisualProtocolSession::new_guest(&contract, &secret).unwrap();
    let limits = &contract.manifest.limits;

    assert_eq!(
        guest
            .seal(
                "nonce-frame".into(),
                1,
                1,
                IsolatedVisualProtocolPayload::GuestToHost(IsolatedVisualGuestMessage::Frame {
                    content_sha256: "e".repeat(64),
                    encoded_bytes: limits.encoded_frame_bytes + 1,
                    width: limits.display_width,
                    height: limits.display_height,
                }),
            )
            .unwrap_err()
            .code,
        ComputerErrorCode::LimitReached
    );

    assert_eq!(
        guest
            .seal(
                "nonce-frame".into(),
                1,
                1,
                IsolatedVisualProtocolPayload::GuestToHost(IsolatedVisualGuestMessage::Frame {
                    content_sha256: "not-a-digest".into(),
                    encoded_bytes: 1_024,
                    width: limits.display_width,
                    height: limits.display_height,
                }),
            )
            .unwrap_err()
            .code,
        ComputerErrorCode::InvalidRequest
    );
}

#[test]
fn an_endpoint_cannot_send_the_opposite_direction() {
    let contract = contract();
    let secret = [3_u8; ISOLATED_VISUAL_CHANNEL_SECRET_BYTES];
    let mut host = IsolatedVisualProtocolSession::new_host(&contract, &secret).unwrap();
    assert_eq!(
        host.seal(
            "nonce-spoof".into(),
            0,
            1,
            IsolatedVisualProtocolPayload::GuestToHost(IsolatedVisualGuestMessage::Health {
                state: IsolatedVisualGuestHealth::ReadOnlyReady,
            }),
        )
        .unwrap_err()
        .code,
        ComputerErrorCode::ForbiddenAction
    );
}

#[test]
fn a_protocol_session_refuses_a_missing_or_empty_channel_secret() {
    let contract = contract();
    for secret in [
        vec![0_u8; ISOLATED_VISUAL_CHANNEL_SECRET_BYTES],
        vec![1_u8; ISOLATED_VISUAL_CHANNEL_SECRET_BYTES - 1],
        vec![1_u8; ISOLATED_VISUAL_CHANNEL_SECRET_BYTES + 1],
        Vec::new(),
    ] {
        assert_eq!(
            IsolatedVisualProtocolSession::new_host(&contract, &secret)
                .unwrap_err()
                .code,
            ComputerErrorCode::InvalidRequest
        );
    }
}

#[test]
fn a_message_sealed_for_one_guest_does_not_open_on_another() {
    let secret = [3_u8; ISOLATED_VISUAL_CHANNEL_SECRET_BYTES];
    let mut host = IsolatedVisualProtocolSession::new_host(&contract(), &secret).unwrap();
    let envelope = host
        .seal(
            NONCE_A.into(),
            0,
            // The isolated protocol is read-only: a nonzero input sequence is
            // itself refused, so the binding proof uses the read-only path.
            0,
            IsolatedVisualProtocolPayload::HostToGuest(IsolatedVisualHostMessage::Stop),
        )
        .unwrap();

    let mut other = contract();
    other.run_id = "run-somewhere-else".into();
    let mut stranger = IsolatedVisualProtocolSession::new_guest(&other, &secret).unwrap();
    assert!(
        stranger.open(envelope.clone()).is_err(),
        "an envelope must not open against another run"
    );

    let mut rotated = contract();
    rotated.surface = surface("surface-boundary", "incarnation-rotated");
    let mut rotated_guest = IsolatedVisualProtocolSession::new_guest(&rotated, &secret).unwrap();
    assert!(
        rotated_guest.open(envelope).is_err(),
        "an envelope must not open against a rotated incarnation"
    );
}

// ---------------------------------------------------------------------------
// Artifact measurement: redirected paths, symlinks, oversize, wrong modes
// ---------------------------------------------------------------------------

fn artifact(name: &str, bytes: &[u8], executable: bool) -> (TempDir, std::path::PathBuf) {
    let directory = tempdir().unwrap();
    let path = directory.path().join(name);
    let mut writer = File::create(&path).unwrap();
    writer.write_all(bytes).unwrap();
    drop(writer);
    std::fs::set_permissions(
        &path,
        std::fs::Permissions::from_mode(if executable { 0o500 } else { 0o400 }),
    )
    .unwrap();
    (directory, path)
}

#[test]
fn a_symlink_is_measured_as_its_target_and_a_hostile_target_fails_closed() {
    // Opening a symlink yields the target's handle, so measurement sees the
    // target's mode and type. A redirected artifact therefore cannot smuggle a
    // world-writable or non-regular file past the measurement gate. Refusing
    // to *follow* the link at all is the packaged opener's O_NOFOLLOW job and
    // is macOS hardware work, not proven here.
    let (directory, target) = artifact("real-guest.img", b"guest-bytes", false);
    let link = directory.path().join("link-guest.img");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let mut through_link = File::open(&link).unwrap();
    let via_link = measure_open_isolated_visual_artifact(
        &mut through_link,
        IsolatedVisualArtifactRole::GuestImage,
    )
    .unwrap();
    let mut direct = File::open(&target).unwrap();
    let via_path =
        measure_open_isolated_visual_artifact(&mut direct, IsolatedVisualArtifactRole::GuestImage)
            .unwrap();
    assert_eq!(via_link, via_path);

    // Redirect the link at a world-writable file.
    let hostile = directory.path().join("hostile.img");
    std::fs::write(&hostile, b"hostile").unwrap();
    std::fs::set_permissions(&hostile, std::fs::Permissions::from_mode(0o666)).unwrap();
    let hostile_link = directory.path().join("link-hostile.img");
    std::os::unix::fs::symlink(&hostile, &hostile_link).unwrap();
    let mut redirected = File::open(&hostile_link).unwrap();
    assert_eq!(
        measure_open_isolated_visual_artifact(
            &mut redirected,
            IsolatedVisualArtifactRole::GuestImage,
        )
        .unwrap_err()
        .code,
        ComputerErrorCode::ForbiddenAction
    );

    // Redirect the link at a directory.
    let directory_link = directory.path().join("link-directory");
    std::os::unix::fs::symlink(directory.path(), &directory_link).unwrap();
    let mut opened_directory = File::open(&directory_link).unwrap();
    assert_eq!(
        measure_open_isolated_visual_artifact(
            &mut opened_directory,
            IsolatedVisualArtifactRole::GuestImage,
        )
        .unwrap_err()
        .code,
        ComputerErrorCode::ForbiddenTarget
    );
}

#[test]
fn a_writable_handle_or_wrong_executable_mode_fails_closed() {
    let (directory, path) = artifact("helper", b"helper-bytes", true);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut writable = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    assert_eq!(
        measure_open_isolated_visual_artifact(
            &mut writable,
            IsolatedVisualArtifactRole::HelperExecutable,
        )
        .unwrap_err()
        .code,
        ComputerErrorCode::ForbiddenAction
    );
    drop(writable);

    // Data artifacts must not be executable.
    let executable_data = directory.path().join("guest.img");
    std::fs::write(&executable_data, b"guest").unwrap();
    std::fs::set_permissions(&executable_data, std::fs::Permissions::from_mode(0o500)).unwrap();
    let mut opened = File::open(&executable_data).unwrap();
    assert_eq!(
        measure_open_isolated_visual_artifact(&mut opened, IsolatedVisualArtifactRole::GuestImage)
            .unwrap_err()
            .code,
        ComputerErrorCode::ForbiddenAction
    );
}

#[test]
fn an_oversized_or_empty_artifact_fails_closed() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("configuration.json");
    let file = File::create(&path).unwrap();
    file.set_len(ISOLATED_VISUAL_MAX_CONFIGURATION_BYTES + 1)
        .unwrap();
    drop(file);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
    let mut oversized = File::open(&path).unwrap();
    assert_eq!(
        measure_open_isolated_visual_artifact(
            &mut oversized,
            IsolatedVisualArtifactRole::Configuration,
        )
        .unwrap_err()
        .code,
        ComputerErrorCode::LimitReached
    );

    let empty_path = directory.path().join("empty.json");
    File::create(&empty_path).unwrap();
    std::fs::set_permissions(&empty_path, std::fs::Permissions::from_mode(0o400)).unwrap();
    let mut empty = File::open(&empty_path).unwrap();
    assert_eq!(
        measure_open_isolated_visual_artifact(
            &mut empty,
            IsolatedVisualArtifactRole::Configuration
        )
        .unwrap_err()
        .code,
        ComputerErrorCode::LimitReached
    );
}

#[test]
fn artifact_measurements_never_serialize_a_host_path() {
    let (helper_dir, helper_path) = artifact("helper", b"helper-bytes", true);
    let (guest_dir, guest_path) = artifact("guest.img", b"guest-bytes", false);
    let (config_dir, config_path) = artifact("configuration.json", b"{}", false);
    let mut helper = File::open(&helper_path).unwrap();
    let mut guest = File::open(&guest_path).unwrap();
    let mut configuration = File::open(&config_path).unwrap();

    let measurements =
        measure_open_isolated_visual_artifacts(&mut helper, &mut guest, &mut configuration)
            .unwrap();
    let serialized = serde_json::to_string(&measurements).unwrap();
    for directory in [&helper_dir, &guest_dir, &config_dir] {
        assert!(
            !serialized.contains(directory.path().to_string_lossy().as_ref()),
            "measurement leaked a host path: {serialized}"
        );
    }
    assert!(!serialized.to_ascii_lowercase().contains("path"));
    assert!(!serialized.to_ascii_lowercase().contains("/tmp"));
}

#[test]
fn measurements_that_do_not_match_the_manifest_are_unauthorized() {
    let (_helper_dir, helper_path) = artifact("helper", b"helper-bytes", true);
    let (_guest_dir, guest_path) = artifact("guest.img", b"guest-bytes", false);
    let (_config_dir, config_path) = artifact("configuration.json", b"{}", false);
    let mut helper = File::open(&helper_path).unwrap();
    let mut guest = File::open(&guest_path).unwrap();
    let mut configuration = File::open(&config_path).unwrap();
    let measurements =
        measure_open_isolated_visual_artifacts(&mut helper, &mut guest, &mut configuration)
            .unwrap();

    let mut declared = manifest();
    declared.helper_content_sha256 = measurements.helper.content_sha256.clone();
    declared.guest_image_sha256 = measurements.guest_image.content_sha256.clone();
    declared.configuration_sha256 = measurements.configuration.content_sha256.clone();
    measurements
        .validate_content_against_manifest(&declared)
        .unwrap();

    for field in 0..3 {
        let mut drifted = declared.clone();
        match field {
            0 => drifted.helper_content_sha256 = "f".repeat(64),
            1 => drifted.guest_image_sha256 = "f".repeat(64),
            _ => drifted.configuration_sha256 = "f".repeat(64),
        }
        assert_eq!(
            measurements
                .validate_content_against_manifest(&drifted)
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );
    }
}

#[test]
fn a_malformed_manifest_is_refused_before_any_artifact_is_trusted() {
    let mut wrong_backend = manifest();
    wrong_backend.backend_id = "some_other_backend".into();
    assert_eq!(
        wrong_backend.validate().unwrap_err().code,
        ComputerErrorCode::InvalidRequest
    );

    let mut short_digest = manifest();
    short_digest.guest_image_sha256 = "abc".into();
    assert_eq!(
        short_digest.validate().unwrap_err().code,
        ComputerErrorCode::InvalidRequest
    );

    let mut uppercase_digest = manifest();
    uppercase_digest.helper_content_sha256 = "A".repeat(64);
    assert_eq!(
        uppercase_digest.validate().unwrap_err().code,
        ComputerErrorCode::InvalidRequest
    );

    let mut bridged = manifest();
    bridged.security_profile.shared_directories = true;
    assert_eq!(
        bridged.validate().unwrap_err().code,
        ComputerErrorCode::ForbiddenAction
    );

    let mut oversized = manifest();
    oversized.limits.memory_mib = u32::MAX;
    assert_eq!(
        oversized.validate().unwrap_err().code,
        ComputerErrorCode::LimitReached
    );
}

#[test]
fn an_unknown_manifest_field_is_rejected_rather_than_ignored() {
    let mut value = serde_json::to_value(manifest()).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("hostBridge".into(), json!(true));
    assert!(
        serde_json::from_value::<IsolatedVisualManifest>(value).is_err(),
        "an unreviewed manifest field must not deserialize"
    );
}

// ---------------------------------------------------------------------------
// The line between compiled proof and hardware proof
// ---------------------------------------------------------------------------

#[test]
fn packaged_authority_cannot_be_minted_off_a_signed_macos_package() {
    // This is the boundary this whole file sits behind. Admitting a packaged
    // launch requires a verified artifact receipt, and the only way to obtain
    // one is to measure a real signed bundle. In Cloud that path fails closed,
    // so nothing here can claim a launch happened.
    let error = grokptah_agent_bridge::computer_use::measure_packaged_isolated_visual_artifacts(
        &manifest(),
    )
    .expect_err("a non-macOS host must not produce a packaged artifact receipt");
    assert_eq!(error.code, ComputerErrorCode::UnsupportedPlatform);
}
