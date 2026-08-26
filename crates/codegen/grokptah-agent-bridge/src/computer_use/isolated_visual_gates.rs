//! Accessibility, privacy, security, cross-boundary denial, and
//! host-noninterference gates for the isolated visual substrate.
//!
//! These are test-only and run on every host, not only macOS. The existing
//! native-shim gate lives in `macos_native`, which is compiled only on macOS,
//! so on Linux CI nothing was watching the shim at all. The shim is ordinary
//! crate source, so the gate below reads it with `include_str!` and holds
//! everywhere.

use serde_json::json;

use super::isolated_guest::{redact_isolated_capture, IsolatedGuestSession};
use super::isolated_visual::{
    IsolatedVisualLaunchContract, IsolatedVisualManifest, IsolatedVisualResourceLimits,
    IsolatedVisualSecurityProfile, ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION,
    ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION, MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID,
};
use super::isolated_visual_channel::IsolatedVisualChannelBinding;
use super::isolated_visual_frames::IsolatedVisualFrameCarrier;
use super::isolated_visual_input::{IsolatedVisualInputGate, IsolatedVisualInputMessage};
use super::isolated_visual_input_wire::IsolatedVisualInputWire;
use super::isolated_visual_protocol::{
    IsolatedVisualGuestHealth, IsolatedVisualGuestMessage, IsolatedVisualProtocolPayload,
    IsolatedVisualProtocolSession,
};
use super::isolated_visual_status::computer_isolated_visual_status;
use super::types::ComputerSurfaceBinding;

const NONCE: &str = "550e8400-e29b-41d4-a716-446655440000";
const SHIM: &str = include_str!("macos_native_shim.m");

/// One isolated contract, distinguished by run, surface, and input domain, so
/// two of them model two different sessions on two different workspaces.
fn contract(tag: &str) -> IsolatedVisualLaunchContract {
    IsolatedVisualLaunchContract {
        run_id: format!("gate-run-{tag}"),
        surface: ComputerSurfaceBinding {
            surface_id: format!("gate-surface-{tag}"),
            incarnation: format!("gate-incarnation-{tag}"),
        },
        input_domain_id: format!("gate-input-{tag}"),
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

fn gate() -> IsolatedVisualInputGate {
    let mut gate = IsolatedVisualInputGate::new(IsolatedVisualResourceLimits::proof_defaults())
        .expect("proof limits are valid");
    gate.bind_frame(1, 2, 2).expect("bind the first frame");
    gate
}

// ---------------------------------------------------------------- host noninterference

#[test]
fn the_native_shim_never_samples_or_synthesizes_host_input() {
    // The Quartz event family can both read and inject physical input. None of
    // it may appear, on any platform, including the isolated block added for
    // the packaged supervisor.
    for forbidden in [
        "CGEventCreate",
        "CGEventPost",
        "CGEventCreateMouseEvent",
        "CGEventCreateKeyboardEvent",
        "CGEventTapCreate",
        "CGEventGetLocation",
        "CGWarpMouseCursorPosition",
        "CGAssociateMouseAndMouseCursorPosition",
        "kCGKeyboardEventKeycode",
        "IOHIDPostEvent",
    ] {
        assert!(
            !SHIM.contains(forbidden),
            "native shim reaches host input through {forbidden}"
        );
    }
}

#[test]
fn the_native_shim_never_reaches_the_host_clipboard_or_scripting_bridge() {
    for forbidden in [
        "NSPasteboard",
        "NSAppleScript",
        "osascript",
        "NSWorkspace openURL",
        "system(",
    ] {
        assert!(
            !SHIM.contains(forbidden),
            "native shim reaches the host through {forbidden}"
        );
    }
}

#[test]
fn the_isolated_supervisor_spawns_only_the_packaged_helper() {
    let supervisor = include_str!("macos_isolated_runtime.rs");
    // posix_spawn lives in the shim behind the measured package; the Rust
    // supervisor must not acquire its own way to run a program.
    for forbidden in [
        "std::process::Command",
        "Command::new",
        "execve",
        "system(",
        "dlopen",
    ] {
        assert!(
            !supervisor.contains(forbidden),
            "packaged supervisor gains process authority through {forbidden}"
        );
    }
}

// ---------------------------------------------------------------- accessibility

#[test]
fn the_isolated_path_requests_no_host_accessibility_authority() {
    // Isolated guest input travels the private authenticated channel. It must
    // never fall back to driving host UI through the accessibility API, which
    // is what would let it act on the operator's own windows.
    for source in [
        include_str!("macos_isolated_runtime.rs"),
        include_str!("macos_isolated_artifacts.rs"),
        include_str!("isolated_visual_input.rs"),
        include_str!("isolated_visual_input_wire.rs"),
        include_str!("isolated_visual_stream.rs"),
    ] {
        for forbidden in [
            "AXUIElement",
            "AXIsProcessTrusted",
            "kAXTrustedCheckOptionPrompt",
        ] {
            assert!(
                !source.contains(forbidden),
                "the isolated path reaches host accessibility through {forbidden}"
            );
        }
    }
}

#[test]
fn accessibility_review_is_named_as_an_outstanding_blocker() {
    let status = computer_isolated_visual_status();
    assert!(
        status.blockers.contains(
            &super::isolated_visual_status::ComputerIsolatedVisualBlocker::IndependentAccessibilityReviewPending
        ),
        "accessibility review must remain an explicit blocker until it passes"
    );
}

// ---------------------------------------------------------------- privacy

#[test]
fn no_substrate_projection_carries_a_secret_or_a_host_path() {
    let encoded = serde_json::to_string(&computer_isolated_visual_status()).unwrap();
    for needle in [
        "challenge",
        "secret",
        "token",
        "bearer",
        "overlay",
        "helperPath",
        "/Users/",
        "/home/",
        "pid",
        "descriptor",
        "inode",
    ] {
        assert!(
            !encoded.contains(needle),
            "isolated visual status leaked {needle}"
        );
    }
}

#[test]
fn capture_redaction_strips_every_forbidden_key_including_nested_ones() {
    let redacted = redact_isolated_capture(&json!({
        "frameSequence": 7,
        "apiKey": "leak",
        "channelSecret": "leak",
        "overlayPath": "/leak",
        "clipboard": "leak",
        "macAddress": "leak",
        "nested": {"deeper": {"token": "leak", "hostHome": "leak"}},
        "list": [{"credential": "leak"}],
    }))
    .expect("a capture whose only problem is forbidden keys is cleaned, not refused");
    let text = redacted.to_string();
    assert!(!text.contains("leak"), "a forbidden key survived: {text}");
    assert_eq!(
        redacted["frameSequence"], 7,
        "redaction dropped safe content"
    );
}

#[test]
fn capture_redaction_fails_closed_on_a_value_that_cannot_be_cleaned() {
    // Forbidden keys can be removed. A secret embedded in a *value* cannot be,
    // so redaction refuses the whole capture instead of returning it partly
    // cleaned. This pins the exact needle set the substrate enforces.
    for needle in [
        "/users/someone/notes",
        "/private/var/secret",
        "/home/someone/notes",
        "clipboard:copied text",
        "password=hunter2",
        "token=sk-live-abcdef",
        "api_key=abcdef",
        "ssid=office-wifi",
        "http://10.0.0.1/",
        "https://example.invalid/",
    ] {
        assert!(
            redact_isolated_capture(&json!({"note": needle})).is_err(),
            "redaction admitted a capture containing {needle}"
        );
        // Nested and array positions are checked the same way.
        assert!(
            redact_isolated_capture(&json!({"a": {"b": [needle]}})).is_err(),
            "redaction admitted a nested capture containing {needle}"
        );
    }

    // Boundary, stated so no reviewer assumes broader coverage: the needle set
    // is deliberately narrow to avoid false positives, so free text that merely
    // names a topic without the needle form is not treated as a secret. Real
    // clipboard contents and host paths arrive under forbidden keys or in the
    // needle forms above; both are covered.
    assert!(redact_isolated_capture(&json!({"note": "the clipboard was not read"})).is_ok());
}

// ---------------------------------------------------------------- security

#[test]
fn the_isolated_runtime_has_no_public_surface() {
    // The reconstruction's central promise: no runtime type escapes the crate.
    // This gate fails if a later change re-exports one, which is how the
    // donor's old public surface would come back.
    let module = include_str!("mod.rs");
    let crate_root = include_str!("../lib.rs");
    let exported: String = module
        .lines()
        .chain(crate_root.lines())
        .filter(|line| line.trim_start().starts_with("pub use"))
        .collect::<Vec<_>>()
        .join("\n");
    for runtime_type in [
        "IsolatedVisualRuntimeSession",
        "IsolatedVisualRuntimeDriver",
        "IsolatedVisualPackagedRuntime",
        "IsolatedVisualLaunchContract",
        "IsolatedVisualLifecycle",
        "IsolatedVisualManifest",
        "IsolatedVisualChannelBinding",
        "IsolatedVisualFrameCarrier",
        "IsolatedVisualInputWire",
        "IsolatedVisualInputGate",
        "IsolatedVisualProtocolSession",
        "IsolatedVisualHelperSupervisor",
        "IsolatedGuestSession",
        "IsolatedGuestLease",
        "MeasuredLaunchOptIn",
        "SemanticElement",
    ] {
        assert!(
            !exported.contains(runtime_type),
            "{runtime_type} is re-exported; the isolated runtime must stay crate-private"
        );
    }
}

#[test]
fn dispatch_is_disabled_and_not_configurable() {
    let status = computer_isolated_visual_status();
    assert!(!status.dispatch_enabled);
    assert!(!status.blockers.is_empty());
    assert!(status
        .blockers
        .contains(&super::isolated_visual_status::ComputerIsolatedVisualBlocker::DispatchDisabled));
    // The field is a constant in the source, not a computed value, so no
    // environment or configuration can flip it.
    let source = include_str!("isolated_visual_status.rs");
    assert!(
        source.contains("dispatch_enabled: false"),
        "dispatch_enabled must be a constant false"
    );
    for configurable in ["env::var", "std::env", "from_config", "set_dispatch"] {
        assert!(
            !source.contains(configurable),
            "isolated visual dispatch must not be reachable through {configurable}"
        );
    }
}

// ------------------------------------------- cross-session / agent / workspace denial

#[test]
fn a_frame_sealed_for_one_session_cannot_be_opened_by_another() {
    let a = contract("a");
    let b = contract("b");
    let secret_a = IsolatedVisualChannelBinding::from_contract(&a)
        .unwrap()
        .derive_channel_secret(&[7; 32])
        .unwrap();
    let secret_b = IsolatedVisualChannelBinding::from_contract(&b)
        .unwrap()
        .derive_channel_secret(&[7; 32])
        .unwrap();
    assert_ne!(secret_a, secret_b, "two sessions must not share a key");

    let mut sender_a = IsolatedVisualFrameCarrier::new_guest(&a, &secret_a).unwrap();
    let chunks = sender_a.seal_frame(1, NONCE, 2, 2, &[1, 2, 3, 4]).unwrap();

    let mut receiver_b = IsolatedVisualFrameCarrier::new_host(&b, &secret_b).unwrap();
    assert!(
        receiver_b.open_chunk(&chunks[0]).is_err(),
        "a frame from another session must not open"
    );

    // Even with the other session's key, the identity bound into the frame
    // still belongs to session A.
    let mut impostor = IsolatedVisualFrameCarrier::new_host(&b, &secret_a).unwrap();
    assert!(
        impostor.open_chunk(&chunks[0]).is_err(),
        "a frame must not open against a foreign session identity"
    );
}

#[test]
fn an_input_packet_sealed_for_one_session_cannot_be_opened_by_another() {
    let a = contract("a");
    let b = contract("b");
    let secret_a = IsolatedVisualChannelBinding::from_contract(&a)
        .unwrap()
        .derive_channel_secret(&[7; 32])
        .unwrap();
    let secret_b = IsolatedVisualChannelBinding::from_contract(&b)
        .unwrap()
        .derive_channel_secret(&[7; 32])
        .unwrap();

    let sender_a = IsolatedVisualInputWire::new_host(&a, &secret_a).unwrap();
    let mut gate_a = gate();
    let packet = sender_a
        .seal(
            &mut gate_a,
            1,
            1,
            NONCE,
            IsolatedVisualInputMessage::PointerMove { x: 1, y: 1 },
        )
        .unwrap();

    let receiver_b = IsolatedVisualInputWire::new_guest(&b, &secret_b).unwrap();
    let mut gate_b = gate();
    assert!(
        receiver_b.open(&mut gate_b, &packet).is_err(),
        "input from another session must not open"
    );

    let impostor = IsolatedVisualInputWire::new_guest(&b, &secret_a).unwrap();
    let mut gate_c = gate();
    assert!(
        impostor.open(&mut gate_c, &packet).is_err(),
        "input must not open against a foreign session identity"
    );
}

#[test]
fn a_protocol_envelope_from_one_session_is_rejected_by_another() {
    let a = contract("a");
    let b = contract("b");
    let secret_a = IsolatedVisualChannelBinding::from_contract(&a)
        .unwrap()
        .derive_channel_secret(&[7; 32])
        .unwrap();
    let secret_b = IsolatedVisualChannelBinding::from_contract(&b)
        .unwrap()
        .derive_channel_secret(&[7; 32])
        .unwrap();

    // A guest may only answer a host request it accepted, so session A has to
    // be driven properly before it can produce an envelope at all.
    let mut host_a = IsolatedVisualProtocolSession::new_host(&a, &secret_a).unwrap();
    let mut guest_a = IsolatedVisualProtocolSession::new_guest(&a, &secret_a).unwrap();
    let observe = host_a
        .seal(
            NONCE.to_string(),
            0,
            0,
            IsolatedVisualProtocolPayload::HostToGuest(
                super::isolated_visual_protocol::IsolatedVisualHostMessage::Observe {
                    maximum_frame_bytes: a.manifest.limits.encoded_frame_bytes,
                    maximum_width: a.manifest.limits.display_width,
                    maximum_height: a.manifest.limits.display_height,
                },
            ),
        )
        .unwrap();
    guest_a.open(observe).unwrap();
    let envelope = guest_a
        .seal(
            NONCE.to_string(),
            0,
            0,
            IsolatedVisualProtocolPayload::GuestToHost(IsolatedVisualGuestMessage::Health {
                state: IsolatedVisualGuestHealth::ReadOnlyReady,
            }),
        )
        .unwrap();

    let mut host_b = IsolatedVisualProtocolSession::new_host(&b, &secret_b).unwrap();
    assert!(
        host_b.open(envelope.clone()).is_err(),
        "an envelope from another session must not open"
    );

    let mut impostor = IsolatedVisualProtocolSession::new_host(&b, &secret_a).unwrap();
    assert!(
        impostor.open(envelope).is_err(),
        "an envelope must not open against a foreign session identity"
    );
}

#[test]
fn a_second_agent_is_denied_and_denial_does_not_mutate_the_guest() {
    let mut guest = IsolatedGuestSession::create(contract("a"), [5; 32]).unwrap();
    let lease = guest.acquire("agent-a").unwrap();
    guest.drive_to_ready("agent-a", &lease).unwrap();
    guest.drive_to_running("agent-a", &lease).unwrap();

    let before = guest.phase();
    assert!(guest.acquire("agent-b").is_err());
    assert!(guest.control("agent-b", &lease).is_err());
    assert!(guest.cancel("agent-b", &lease).is_err());
    assert!(guest.fail_guest("agent-b", &lease).is_err());
    assert_eq!(
        guest.phase(),
        before,
        "a denied second agent must not move the guest"
    );
    assert_eq!(
        guest.lease().map(|live| live.agent_id.as_str()),
        Some("agent-a"),
        "a denied second agent must not take the lease"
    );
}

#[test]
fn a_contract_cannot_reuse_its_surface_identity_as_its_input_domain() {
    // The guest's input domain must be independent of the host-facing surface
    // identity, so a workspace that learns one cannot address the other.
    let mut same_as_surface = contract("a");
    same_as_surface.input_domain_id = same_as_surface.surface.surface_id().to_string();
    assert!(same_as_surface.validate().is_err());

    let mut same_as_incarnation = contract("a");
    same_as_incarnation.input_domain_id = same_as_incarnation.surface.incarnation().to_string();
    assert!(same_as_incarnation.validate().is_err());
}
