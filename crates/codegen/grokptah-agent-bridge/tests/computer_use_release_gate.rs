//! Deterministic #274 release-gate coverage for the first Computer Run.
//!
//! These tests deliberately use hostile observations and backend failures. They do not
//! open a real application, request macOS permissions, or dispatch OS input. Native and
//! packaged proof remains a separate exact-head/manual gate documented in the threat model.

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{Duration, Utc};
use tempfile::TempDir;

use grokptah_agent_bridge::computer_use::{
    ActionClass, ActionGrant, ComputerAction, ComputerAuthorityToken, ComputerBackend,
    ComputerErrorCode, ComputerRun, ComputerRunState, ComputerStore, ComputerTarget, PointerButton,
    SimulatorBackend,
};
use grokptah_agent_bridge::{
    home_override_serial, set_grokptah_home_override, AgentHost, ComputerUseService, HostConfig,
};

#[derive(Debug, Clone, Copy)]
enum BackendMode {
    PromptInjection,
    SensitiveObservation,
    TargetDrift,
    PermissionRevoked,
    Permissive,
}

fn grant(run: &ComputerRun, classes: BTreeSet<ActionClass>) -> ActionGrant {
    let issued_at = Utc::now() - Duration::seconds(1);
    ActionGrant::for_run(
        run,
        classes,
        issued_at,
        issued_at + Duration::minutes(1),
        Some(4),
    )
}

fn caller(
    run: &ComputerRun,
    host: &grokptah_agent_bridge::AgentHostHandle,
) -> ComputerAuthorityToken {
    host.computer_operator_token(run.owner_session_id)
        .expect("release-gate owner is a valid local operator")
}

fn fixture(
    mode: BackendMode,
    classes: BTreeSet<ActionClass>,
) -> (
    TempDir,
    Arc<SimulatorBackend>,
    ComputerUseService,
    ComputerRun,
    grokptah_agent_bridge::AgentHostHandle,
) {
    let directory = tempfile::tempdir().expect("fixture directory");
    let _guard = home_override_serial();
    set_grokptah_home_override(Some(directory.path().join(".grokptah")));
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().expect("host starts");
    let session = host.session_new().expect("live session");
    let caller = host
        .computer_operator_token(session.id)
        .expect("live operator token");
    set_grokptah_home_override(None);
    let backend = Arc::new(match mode {
        BackendMode::PromptInjection => SimulatorBackend::prompt_injection_fixture(),
        BackendMode::SensitiveObservation => SimulatorBackend::sensitive_observation_fixture(),
        BackendMode::TargetDrift => SimulatorBackend::target_drift_fixture(),
        BackendMode::PermissionRevoked => SimulatorBackend::permission_revoked_fixture(),
        BackendMode::Permissive => SimulatorBackend::new(),
    });
    let store = ComputerStore::open(directory.path().join("computer-use")).expect("store");
    let service = ComputerUseService::new_simulator(backend.clone(), store);
    let run = service
        .create_run(
            "release-gate-create",
            &caller,
            None,
            target(),
            Default::default(),
        )
        .expect("create run");
    let run = service
        .authorize(
            "release-gate-authorize",
            &caller,
            &run.run_id,
            run.version,
            grant(&run, classes),
        )
        .expect("authorize run");
    (directory, backend, service, run, host)
}

fn target() -> ComputerTarget {
    SimulatorBackend::demo_target()
}

#[tokio::test]
async fn observed_instruction_text_cannot_expand_action_scope() {
    let (_directory, backend, service, run, host) = fixture(
        BackendMode::PromptInjection,
        BTreeSet::from([ActionClass::Semantic, ActionClass::TextEntry]),
    );
    let observation = service
        .observe(
            "release-gate-observe-injection",
            &caller(&run, &host),
            &run.run_id,
            run.version,
        )
        .await
        .expect("hostile content is still observable data");
    assert!(observation.elements[0]
        .label
        .as_deref()
        .is_some_and(|label| label.contains("raw pointer")));

    let current = service.get_run(&run.run_id).unwrap().unwrap();
    let error = service
        .act(
            "release-gate-invented-action",
            &caller(&run, &host),
            &run.run_id,
            current.version,
            &observation.observation_id,
            ComputerAction::Invoke {
                element_id: "instruction-generated-submit".into(),
            },
        )
        .await
        .expect_err("observed text must not create a semantic target");
    assert_eq!(error.code, ComputerErrorCode::StaleObservation);
    assert_eq!(backend.action_attempt_count(), 0);
}

#[tokio::test]
async fn sensitive_observation_fails_before_model_visible_action_or_dispatch() {
    let (_directory, backend, service, run, host) = fixture(
        BackendMode::SensitiveObservation,
        BTreeSet::from([ActionClass::Semantic, ActionClass::TextEntry]),
    );
    let error = service
        .observe(
            "release-gate-observe-sensitive",
            &caller(&run, &host),
            &run.run_id,
            run.version,
        )
        .await
        .expect_err("secure element must be denied");
    assert_eq!(error.code, ComputerErrorCode::SensitiveSurface);
    let persisted = service.get_run(&run.run_id).unwrap().unwrap();
    assert_eq!(persisted.state, ComputerRunState::Failed);
    assert!(persisted.current_observation.is_none());
    assert!(persisted
        .grant
        .as_ref()
        .is_some_and(|grant| grant.revoked_at.is_some()));
    assert_eq!(backend.action_attempt_count(), 0);
}

#[tokio::test]
async fn observation_target_drift_fails_inflight_run_and_revokes_authority() {
    let (_directory, _backend, service, run, host) = fixture(
        BackendMode::TargetDrift,
        BTreeSet::from([ActionClass::Semantic]),
    );
    let error = service
        .observe(
            "release-gate-observe-drift",
            &caller(&run, &host),
            &run.run_id,
            run.version,
        )
        .await
        .expect_err("a changed target must not be committed");
    assert_eq!(error.code, ComputerErrorCode::TargetChanged);
    let persisted = service.get_run(&run.run_id).unwrap().unwrap();
    assert_eq!(persisted.state, ComputerRunState::Failed);
    assert!(persisted.current_observation.is_none());
    assert!(persisted
        .grant
        .as_ref()
        .is_some_and(|grant| grant.revoked_at.is_some()));
}

#[tokio::test]
async fn permission_revocation_fails_action_and_clears_authority() {
    let (_directory, backend, service, run, host) = fixture(
        BackendMode::PermissionRevoked,
        BTreeSet::from([ActionClass::TextEntry]),
    );
    let observation = service
        .observe(
            "release-gate-observe-revoked",
            &caller(&run, &host),
            &run.run_id,
            run.version,
        )
        .await
        .expect("observation");
    let current = service.get_run(&run.run_id).unwrap().unwrap();
    let element_id = observation.elements[0].element_id.clone();
    let error = service
        .act(
            "release-gate-act-revoked",
            &caller(&run, &host),
            &run.run_id,
            current.version,
            &observation.observation_id,
            ComputerAction::SetValue {
                element_id,
                text: "safe test value".into(),
            },
        )
        .await
        .expect_err("revoked permission must fail closed");
    assert_eq!(error.code, ComputerErrorCode::PermissionRevoked);
    let persisted = service.get_run(&run.run_id).unwrap().unwrap();
    assert_eq!(persisted.state, ComputerRunState::Failed);
    assert!(persisted.current_observation.is_none());
    assert!(persisted
        .grant
        .as_ref()
        .is_some_and(|grant| grant.revoked_at.is_some()));
    assert_eq!(backend.action_attempt_count(), 1);
}

#[tokio::test]
async fn unsupported_pointer_fallback_never_reaches_backend() {
    let (_directory, backend, service, run, host) = fixture(
        BackendMode::Permissive,
        BTreeSet::from([ActionClass::PointerFallback]),
    );
    let observation = service
        .observe(
            "release-gate-observe-pointer",
            &caller(&run, &host),
            &run.run_id,
            run.version,
        )
        .await
        .expect("observation");
    let current = service.get_run(&run.run_id).unwrap().unwrap();
    let error = service
        .act(
            "release-gate-act-pointer",
            &caller(&run, &host),
            &run.run_id,
            current.version,
            &observation.observation_id,
            ComputerAction::PointerClick {
                x: 10.0,
                y: 10.0,
                button: PointerButton::Primary,
            },
        )
        .await
        .expect_err("host pointer fallback is unsupported by this backend");
    assert_eq!(error.code, ComputerErrorCode::ForbiddenAction);
    assert_eq!(backend.action_attempt_count(), 0);
    assert_eq!(
        service.get_run(&run.run_id).unwrap().unwrap().state,
        ComputerRunState::Ready
    );
}

/// The MCP control plane exposes exactly four read-only Computer Run tools.
/// Any additional computer-prefixed tool — a mutation, an evidence or
/// screenshot fetch, raw input, or admin control — must consciously widen
/// this snapshot rather than slip in silently (#271 read-only slice).
#[test]
fn mcp_surface_exposes_only_the_scoped_computer_read_tools() {
    use grokptah_agent_bridge::{CONTROL_TOOLS, FORBIDDEN_TOOLS};

    let computer_tools: Vec<&str> = CONTROL_TOOLS
        .iter()
        .copied()
        .filter(|name| name.contains("computer"))
        .collect();
    assert_eq!(
        computer_tools,
        vec![
            "ptah_list_computer_runs",
            "ptah_get_computer_run",
            "ptah_get_computer_run_events",
            "ptah_get_computer_capacity",
        ]
    );
    for forbidden_fragment in [
        "submit_computer",
        "computer_action",
        "computer_act",
        "computer_pause",
        "computer_cancel",
        "computer_take_over",
        "computer_grant",
        "computer_approve",
        "computer_evidence",
        "computer_screenshot",
        "computer_observe",
        "computer_input",
        "computer_shell",
    ] {
        assert!(
            !CONTROL_TOOLS
                .iter()
                .any(|name| name.contains(forbidden_fragment)),
            "{forbidden_fragment} must not be reachable through the control plane"
        );
    }
    for forbidden in FORBIDDEN_TOOLS {
        assert!(!CONTROL_TOOLS.contains(forbidden));
    }
}

#[test]
fn rust_computer_use_sources_remain_free_of_global_input_injection() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/computer_use");
    let forbidden = [
        "CGEventCreate",
        "CGEventPost",
        "CGWarpMouseCursorPosition",
        "CGAssociateMouseAndMouseCursorPosition",
        "NSPasteboard",
        "NSAppleScript",
        "kCGKeyboardEventKeycode",
    ];
    for name in [
        "macos_native.rs",
        "macos_observation.rs",
        "macos_native_shim.m",
        "types.rs",
        "policy.rs",
        "service.rs",
        "simulator.rs",
        "store.rs",
        "platform.rs",
    ] {
        let src = std::fs::read_to_string(root.join(name))
            .unwrap_or_else(|error| panic!("read {}: {error}", root.join(name).display()));
        let production = src.split("#[cfg(test)]").next().unwrap_or(&src);
        for needle in forbidden {
            assert!(
                !production.contains(needle),
                "{} production source must not contain {needle}",
                name
            );
        }
    }
}

#[test]
fn default_simulator_advertises_foreground_semantic_only() {
    let simulator = grokptah_agent_bridge::SimulatorBackend::new();
    let caps = simulator.capabilities();
    assert_eq!(
        caps.tier,
        grokptah_agent_bridge::ComputerCapabilityTier::ForegroundSemantic
    );
    assert!(!caps.pointer_fallback);
    assert!(!caps.key_chords);
    assert!(!caps.proof.isolated_input_is_dispatchable());
    assert_ne!(
        caps.backend_id,
        grokptah_agent_bridge::SIMULATOR_ISOLATED_BACKEND_ID
    );
}
