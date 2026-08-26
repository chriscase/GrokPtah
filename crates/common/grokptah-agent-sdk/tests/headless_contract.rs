//! Contract tests for the host-neutral headless authority entry point.
//!
//! These prove the properties a Linux or cloud worker depends on: the entry
//! point carries no platform dependency, canonical types round-trip without
//! semantic drift, and every malformed, unknown, oversized, out-of-scope,
//! stale, or unsupported input fails closed.

use grokptah_agent_sdk::CONTRACT_VERSION;
use grokptah_agent_sdk::capability::{
    CapabilityAvailability, CapabilityDescriptor, CapabilitySet, CapabilityTier,
};
use grokptah_agent_sdk::computer::{
    ComputerActionClass, ComputerControlRequest, ComputerControlResponse, ComputerEvent,
};
use grokptah_agent_sdk::error::ErrorCode;
use grokptah_agent_sdk::headless::{
    CapabilityRevision, HEADLESS_CONTRACT_VERSION, HeadlessAdmission, HeadlessAuthority,
    HeadlessHostInfo, HeadlessLimits, HeadlessOperation, HeadlessPlatform,
};
use grokptah_agent_sdk::run::{
    AuthorityBounds, Bounds, BoundsConversionError, ChangedFile, DurableRun, DurableRunState,
    ExecutionMode, MAX_ROUNDS, ReviewReceipt, RunEvent, RunEventPage, RunNotification, RunScope,
    SubmitTaskRequest,
};
use serde_json::json;

/// The crate's own manifest, so a platform dependency cannot be added without
/// this test failing.
const MANIFEST: &str = include_str!("../Cargo.toml");

const SESSION: &str = "session-1";
const WORKSPACE: &str = "/approved";

fn ceiling() -> AuthorityBounds {
    AuthorityBounds {
        max_prompt_bytes: 100_000,
        max_rounds: 24,
        max_duration_ms: 15 * 60 * 1000,
    }
}

fn limits() -> HeadlessLimits {
    HeadlessLimits {
        bounds: ceiling(),
        max_concurrent_runs: 2,
    }
}

fn capabilities() -> CapabilitySet {
    CapabilitySet {
        contract: CONTRACT_VERSION.into(),
        capabilities: vec![
            CapabilityDescriptor {
                id: "session.observe".into(),
                tier: CapabilityTier::Observe,
                mutating: false,
                human_gate: false,
                availability: CapabilityAvailability::Available,
                description: "Observe bounded session state.".into(),
            },
            CapabilityDescriptor {
                id: "run.execute".into(),
                tier: CapabilityTier::Execute,
                mutating: true,
                human_gate: false,
                availability: CapabilityAvailability::Available,
                description: "Submit and cancel bounded runs.".into(),
            },
            CapabilityDescriptor {
                id: "run.review".into(),
                tier: CapabilityTier::Review,
                mutating: false,
                human_gate: false,
                availability: CapabilityAvailability::Available,
                description: "Read bounded review projections.".into(),
            },
            CapabilityDescriptor {
                id: "run.promote".into(),
                tier: CapabilityTier::Promote,
                mutating: true,
                human_gate: true,
                availability: CapabilityAvailability::Gated,
                description: "Promote a reviewed run after a human grant.".into(),
            },
        ],
    }
}

fn host(revision: u64) -> HeadlessHostInfo {
    HeadlessHostInfo {
        host_id: "worker-1".into(),
        contract: CONTRACT_VERSION.into(),
        headless_contract: HEADLESS_CONTRACT_VERSION.into(),
        platform: HeadlessPlatform::Linux,
        revision: CapabilityRevision(revision),
        capabilities: capabilities(),
    }
}

fn admission() -> HeadlessAdmission {
    HeadlessAdmission::bind(host(3), limits(), SESSION, WORKSPACE).expect("admission binds")
}

fn scope() -> RunScope {
    RunScope {
        session_id: SESSION.into(),
        workspace: WORKSPACE.into(),
        run_id: "run-1".into(),
    }
}

fn submit() -> SubmitTaskRequest {
    SubmitTaskRequest {
        request_id: "req-1".into(),
        session_id: SESSION.into(),
        workspace: WORKSPACE.into(),
        prompt: "review the diff".into(),
        bounds: Some(Bounds {
            max_prompt_bytes: Some(4096),
            max_rounds: Some(8),
            max_duration_ms: Some(120_000),
        }),
        execution_mode: Some(ExecutionMode::IsolatedWorktree),
        allow_queue: Some(true),
    }
}

// ---------------------------------------------------------------------------
// Portability
// ---------------------------------------------------------------------------

#[test]
fn headless_entry_carries_no_platform_or_ui_dependency() {
    // Everything after `[dependencies]` up to the next section header.
    let deps = MANIFEST
        .split("[dependencies]")
        .nth(1)
        .expect("manifest declares dependencies");
    let deps = deps.split("\n[").next().expect("dependency section ends");

    let declared: Vec<&str> = deps
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split('=').next())
        .map(str::trim)
        .collect();

    assert_eq!(
        declared,
        vec!["serde", "serde_json"],
        "the headless entry point must depend only on serde; \
         a platform, UI, transport, or credential dependency breaks portability"
    );

    // Named explicitly so an accidental reintroduction is obvious in the diff.
    for forbidden in [
        "tauri",
        "keyring",
        "dbus",
        "secret-service",
        "objc",
        "core-foundation",
        "cocoa",
        "reqwest",
        "tokio",
        "libc",
        "windows-sys",
    ] {
        assert!(
            !deps.contains(forbidden),
            "portable contract must not depend on {forbidden}"
        );
    }
}

#[test]
fn the_port_behaves_identically_on_every_platform() {
    // Platform is descriptive metadata; it must not gate admission.
    for platform in [
        HeadlessPlatform::Linux,
        HeadlessPlatform::MacOs,
        HeadlessPlatform::Windows,
        HeadlessPlatform::Unknown,
    ] {
        let mut info = host(1);
        info.platform = platform;
        let gate = HeadlessAdmission::bind(info, limits(), SESSION, WORKSPACE)
            .expect("admission binds on every platform");
        assert!(gate.admit_submit(&submit(), CapabilityRevision(1)).is_ok());
    }

    // Native Computer Use remains a macOS-only *possibility*, never a grant.
    assert!(HeadlessPlatform::MacOs.supports_native_computer_use());
    for platform in [
        HeadlessPlatform::Linux,
        HeadlessPlatform::Windows,
        HeadlessPlatform::Unknown,
    ] {
        assert!(!platform.supports_native_computer_use());
    }
}

// ---------------------------------------------------------------------------
// Round-trip without semantic drift
// ---------------------------------------------------------------------------

#[test]
fn canonical_types_round_trip_without_semantic_drift() {
    macro_rules! round_trip {
        ($value:expr, $ty:ty) => {{
            let original: $ty = $value;
            let encoded = serde_json::to_vec(&original).expect("value serializes");
            let decoded: $ty = serde_json::from_slice(&encoded).expect("value round-trips");
            assert_eq!(decoded, original, "semantic drift in {}", stringify!($ty));
            // Re-encoding the decoded value must be byte-identical, so the
            // projection is canonical and not merely equal.
            let re_encoded = serde_json::to_vec(&decoded).expect("value re-serializes");
            assert_eq!(
                encoded,
                re_encoded,
                "non-canonical encoding for {}",
                stringify!($ty)
            );
        }};
    }

    round_trip!(scope(), RunScope);
    round_trip!(submit(), SubmitTaskRequest);
    round_trip!(
        Bounds {
            max_prompt_bytes: Some(u32::MAX),
            max_rounds: Some(MAX_ROUNDS),
            max_duration_ms: Some(u64::MAX),
        },
        Bounds
    );
    round_trip!(
        DurableRun {
            run_id: "run-1".into(),
            session_id: SESSION.into(),
            workspace: WORKSPACE.into(),
            request_id: "req-1".into(),
            state: DurableRunState::LimitReached,
            prompt_preview: "review".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:01Z".into(),
        },
        DurableRun
    );
    round_trip!(
        RunEventPage {
            entries: vec![RunEvent {
                seq: u64::MAX,
                ts: "2026-01-01T00:00:00Z".into(),
                update: json!({"kind": "tool_call", "n": 1}),
            }],
            next_cursor: None,
            cursor_expired: true,
        },
        RunEventPage
    );
    round_trip!(
        RunNotification::Recovery {
            scope: scope(),
            after_seq: 7,
            reason: "cursor_expired".into(),
            poll_tool: "ptah_get_events".into(),
        },
        RunNotification
    );
    round_trip!(
        ReviewReceipt {
            changed_files: vec![ChangedFile {
                path: "src/lib.rs".into(),
                summary: "edited".into(),
            }],
            diff: "diff --git a/src/lib.rs".into(),
            diff_truncated: true,
            fingerprint: "sha256:abc".into(),
        },
        ReviewReceipt
    );
    round_trip!(
        ComputerControlRequest {
            request_id: "req-1".into(),
            scope: scope(),
            expected_version: u64::MAX,
            action_classes: vec![
                ComputerActionClass::Semantic,
                ComputerActionClass::TextEntry
            ],
            ttl_ms: 30_000,
        },
        ComputerControlRequest
    );
    round_trip!(host(9), HeadlessHostInfo);
}

#[test]
fn authority_width_conversions_are_lossless_in_both_directions() {
    for (prompt, rounds, duration) in [
        (1usize, 1u32, 1u64),
        (4096, 8, 120_000),
        (100_000, 24, 900_000),
        (u32::MAX as usize, u32::from(MAX_ROUNDS), u64::MAX),
    ] {
        let public = Bounds::from_authority_widths(prompt, rounds, duration)
            .expect("authority widths project into the public contract");
        assert_eq!(public.max_prompt_bytes, Some(prompt as u32));
        assert_eq!(public.max_rounds, Some(rounds as u16));
        assert_eq!(public.max_duration_ms, Some(duration));

        let ceiling = AuthorityBounds {
            max_prompt_bytes: prompt,
            max_rounds: rounds,
            max_duration_ms: duration,
        };
        let resolved = public
            .resolve_authority_widths(ceiling)
            .expect("public bounds resolve back to authority widths");
        assert_eq!(resolved, ceiling, "width round-trip lost a value");
    }
}

#[test]
fn narrowing_conversions_fail_closed_instead_of_truncating() {
    // u32 rounds above the public u16 must not wrap to a small, valid value.
    assert_eq!(
        Bounds::from_authority_widths(4096, 70_000, 1000),
        Err(BoundsConversionError::RoundsOverflow)
    );
    // 65_560 wraps to 24 in a `as u16` cast — exactly the silent drift the
    // seam exists to prevent.
    assert_eq!(
        Bounds::from_authority_widths(4096, 65_560, 1000),
        Err(BoundsConversionError::RoundsOverflow)
    );
    // Representable, but above the versioned contract ceiling.
    assert_eq!(
        Bounds::from_authority_widths(4096, u32::from(MAX_ROUNDS) + 1, 1000),
        Err(BoundsConversionError::RoundsAboveContract)
    );
    assert_eq!(
        Bounds::from_authority_widths(0, 8, 1000),
        Err(BoundsConversionError::ZeroValue)
    );

    #[cfg(target_pointer_width = "64")]
    assert_eq!(
        Bounds::from_authority_widths((u32::MAX as usize) + 1, 8, 1000),
        Err(BoundsConversionError::PromptBytesOverflow)
    );

    // A caller may only narrow, never widen past the authority ceiling.
    let small = AuthorityBounds {
        max_prompt_bytes: 1024,
        max_rounds: 4,
        max_duration_ms: 1000,
    };
    for widening in [
        Bounds {
            max_prompt_bytes: Some(2048),
            ..Bounds::default()
        },
        Bounds {
            max_rounds: Some(8),
            ..Bounds::default()
        },
        Bounds {
            max_duration_ms: Some(2000),
            ..Bounds::default()
        },
    ] {
        assert_eq!(
            widening.resolve_authority_widths(small),
            Err(BoundsConversionError::AboveCeiling)
        );
    }

    // An absent field inherits the ceiling rather than becoming unbounded.
    assert_eq!(
        Bounds::default()
            .resolve_authority_widths(small)
            .expect("absent bounds inherit the ceiling"),
        small
    );

    // A zero ceiling admits nothing.
    assert_eq!(
        Bounds::default().resolve_authority_widths(AuthorityBounds {
            max_prompt_bytes: 0,
            max_rounds: 4,
            max_duration_ms: 1000,
        }),
        Err(BoundsConversionError::ZeroValue)
    );
}

// ---------------------------------------------------------------------------
// Fail-closed admission
// ---------------------------------------------------------------------------

#[test]
fn admission_rejects_a_scope_outside_the_binding() {
    let gate = admission();
    let revision = CapabilityRevision(3);

    for (session, workspace) in [
        ("other-session", WORKSPACE),
        (SESSION, "/elsewhere"),
        ("other-session", "/elsewhere"),
    ] {
        let mut request = submit();
        request.session_id = session.into();
        request.workspace = workspace.into();
        let error = gate
            .admit_submit(&request, revision)
            .expect_err("out-of-scope submit must fail closed");
        assert_eq!(error.code, ErrorCode::ForbiddenScope);
        assert_eq!(error.reason_code.as_deref(), Some("scope_mismatch"));

        let mut probe = scope();
        probe.session_id = session.into();
        probe.workspace = workspace.into();
        let error = gate
            .admit(HeadlessOperation::Events, &probe, revision)
            .expect_err("out-of-scope read must fail closed");
        assert_eq!(error.code, ErrorCode::ForbiddenScope);
    }

    // The exact bound scope is admitted.
    assert!(
        gate.admit(HeadlessOperation::Events, &scope(), revision)
            .is_ok()
    );
}

#[test]
fn admission_rejects_a_stale_capability_revision() {
    let gate = admission(); // authority revision 3
    for stale in [
        CapabilityRevision(0),
        CapabilityRevision(2),
        CapabilityRevision(4),
    ] {
        let error = gate
            .admit_submit(&submit(), stale)
            .expect_err("a mismatched revision must fail closed");
        assert_eq!(error.code, ErrorCode::StaleOrRecovery);
        assert_eq!(
            error.reason_code.as_deref(),
            Some("capability_revision_stale")
        );

        assert_eq!(
            gate.admit(HeadlessOperation::Review, &scope(), stale)
                .expect_err("a mismatched revision must fail closed")
                .code,
            ErrorCode::StaleOrRecovery
        );
    }
    assert!(gate.admit_submit(&submit(), CapabilityRevision(3)).is_ok());

    // A revision only ever moves forward.
    assert_eq!(CapabilityRevision::INITIAL.next(), CapabilityRevision(1));
    assert_eq!(
        CapabilityRevision(u64::MAX).next(),
        CapabilityRevision(u64::MAX)
    );
}

#[test]
fn admission_never_widens_beyond_the_advertised_capabilities() {
    let revision = CapabilityRevision(3);

    // A capability the host did not advertise is refused.
    let mut info = host(3);
    info.capabilities
        .capabilities
        .retain(|c| c.id != "run.review");
    let gate = HeadlessAdmission::bind(info, limits(), SESSION, WORKSPACE).expect("binds");
    let error = gate
        .admit(HeadlessOperation::Review, &scope(), revision)
        .expect_err("an unadvertised capability must fail closed");
    assert_eq!(error.code, ErrorCode::ForbiddenScope);
    assert_eq!(
        error.reason_code.as_deref(),
        Some("capability_not_advertised")
    );

    // A gated capability still needs a human grant this port cannot issue,
    // so no operation may map onto one implicitly.
    let mut info = host(3);
    for descriptor in &mut info.capabilities.capabilities {
        if descriptor.id == "run.execute" {
            descriptor.availability = CapabilityAvailability::Gated;
            descriptor.human_gate = true;
        }
    }
    let gate = HeadlessAdmission::bind(info, limits(), SESSION, WORKSPACE).expect("binds");
    assert_eq!(
        gate.admit_submit(&submit(), revision)
            .expect_err("a gated capability must fail closed")
            .reason_code
            .as_deref(),
        Some("capability_gated")
    );

    // An unavailable capability is refused.
    let mut info = host(3);
    for descriptor in &mut info.capabilities.capabilities {
        if descriptor.id == "run.execute" {
            descriptor.availability = CapabilityAvailability::Unavailable;
        }
    }
    let gate = HeadlessAdmission::bind(info, limits(), SESSION, WORKSPACE).expect("binds");
    assert_eq!(
        gate.admit_submit(&submit(), revision)
            .expect_err("an unavailable capability must fail closed")
            .reason_code
            .as_deref(),
        Some("capability_unavailable")
    );

    // The operation → capability map introduces no identifier of its own.
    let set = capabilities();
    let advertised: Vec<&str> = set.capabilities.iter().map(|c| c.id.as_str()).collect();
    for operation in [
        HeadlessOperation::Submit,
        HeadlessOperation::Events,
        HeadlessOperation::Review,
        HeadlessOperation::Cancel,
    ] {
        assert!(
            advertised.contains(&operation.capability_id()),
            "{:?} maps to an identifier the host does not advertise",
            operation
        );
    }
}

#[test]
fn admission_rejects_bounds_and_prompts_above_the_worker_ceiling() {
    let gate = admission();
    let revision = CapabilityRevision(3);

    let mut request = submit();
    request.bounds = Some(Bounds {
        max_rounds: Some(MAX_ROUNDS + 1),
        ..Bounds::default()
    });
    assert_eq!(
        gate.admit_submit(&request, revision)
            .expect_err("above-contract rounds must fail closed")
            .code,
        ErrorCode::InvalidRequest
    );

    let mut request = submit();
    request.bounds = Some(Bounds {
        max_duration_ms: Some(u64::MAX),
        ..Bounds::default()
    });
    assert_eq!(
        gate.admit_submit(&request, revision)
            .expect_err("above-ceiling duration must fail closed")
            .reason_code
            .as_deref(),
        Some("bounds_above_ceiling")
    );

    // The prompt is checked against the *resolved* bound, not the ceiling.
    let mut request = submit();
    request.bounds = Some(Bounds {
        max_prompt_bytes: Some(16),
        ..Bounds::default()
    });
    request.prompt = "x".repeat(17);
    assert_eq!(
        gate.admit_submit(&request, revision)
            .expect_err("an oversized prompt must fail closed")
            .reason_code
            .as_deref(),
        Some("prompt_above_bounds")
    );

    request.prompt = "x".repeat(16);
    let resolved = gate
        .admit_submit(&request, revision)
        .expect("a prompt within the resolved bound is admitted");
    assert_eq!(resolved.max_prompt_bytes, 16);
    assert_eq!(resolved.max_rounds, ceiling().max_rounds);
}

#[test]
fn a_malformed_host_or_ceiling_admits_nothing() {
    // Empty and non-share-safe host identities.
    for host_id in [
        "",
        "   ",
        "/var/run/worker",
        "https://worker.test",
        "sk-live-abc123def456",
    ] {
        let mut info = host(1);
        info.host_id = host_id.into();
        assert!(
            HeadlessAdmission::bind(info, limits(), SESSION, WORKSPACE).is_err(),
            "host_id {host_id:?} must not bind"
        );
    }

    // A malformed capability set cannot be advertised.
    let mut info = host(1);
    info.capabilities.capabilities.push(CapabilityDescriptor {
        id: "notdotted".into(),
        tier: CapabilityTier::Observe,
        mutating: false,
        human_gate: false,
        availability: CapabilityAvailability::Available,
        description: "bad".into(),
    });
    assert!(HeadlessAdmission::bind(info, limits(), SESSION, WORKSPACE).is_err());

    // Zero ceilings admit nothing.
    for bad in [
        HeadlessLimits {
            bounds: AuthorityBounds {
                max_prompt_bytes: 0,
                ..ceiling()
            },
            max_concurrent_runs: 1,
        },
        HeadlessLimits {
            bounds: ceiling(),
            max_concurrent_runs: 0,
        },
    ] {
        assert!(HeadlessAdmission::bind(host(1), bad, SESSION, WORKSPACE).is_err());
    }

    // An unbound scope cannot be admitted against.
    assert!(HeadlessAdmission::bind(host(1), limits(), "", WORKSPACE).is_err());
    assert!(HeadlessAdmission::bind(host(1), limits(), SESSION, "  ").is_err());
}

#[test]
fn an_unimplemented_operation_reports_unavailable_rather_than_succeeding() {
    struct ObserveOnly;
    impl HeadlessAuthority for ObserveOnly {
        fn host_info(&self) -> Result<HeadlessHostInfo, grokptah_agent_sdk::ErrorEnvelope> {
            Ok(host(1))
        }
        fn limits(&self) -> Result<HeadlessLimits, grokptah_agent_sdk::ErrorEnvelope> {
            Ok(limits())
        }
    }

    let worker = ObserveOnly;
    assert!(worker.host_info().is_ok());

    for error in [
        worker.submit(&submit()).map(|_| ()).unwrap_err(),
        worker.events(&scope(), 0).map(|_| ()).unwrap_err(),
        worker.review(&scope()).map(|_| ()).unwrap_err(),
        worker.cancel(&scope()).unwrap_err(),
    ] {
        assert_eq!(
            error.code,
            ErrorCode::AuthorityUnavailable,
            "an unimplemented operation must not report success"
        );
        assert!(
            error
                .reason_code
                .as_deref()
                .is_some_and(|code| code.starts_with("unimplemented_"))
        );
    }
}

// ---------------------------------------------------------------------------
// Malformed, unknown, and oversized payloads
// ---------------------------------------------------------------------------

#[test]
fn malformed_and_oversized_projections_fail_closed() {
    // Oversized prompt preview.
    let mut run = DurableRun {
        run_id: "run-1".into(),
        session_id: SESSION.into(),
        workspace: WORKSPACE.into(),
        request_id: "req-1".into(),
        state: DurableRunState::Running,
        prompt_preview: "x".repeat(513),
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:01Z".into(),
    };
    assert_eq!(run.validate(), Err("oversized"));
    run.prompt_preview = "ok".into();
    assert!(run.validate().is_ok());

    // Empty identities.
    for empty in ["", "   "] {
        let mut broken = run.clone();
        broken.run_id = empty.into();
        assert!(broken.validate().is_err());
    }

    // Oversized event payload.
    assert_eq!(
        RunEvent {
            seq: 1,
            ts: "2026-01-01T00:00:00Z".into(),
            update: json!({ "blob": "x".repeat(300 * 1024) }),
        }
        .validate(),
        Err("oversized")
    );

    // Non-monotonic and contradictory pages.
    assert!(
        RunEventPage {
            entries: vec![
                RunEvent {
                    seq: 2,
                    ts: "t".into(),
                    update: json!({})
                },
                RunEvent {
                    seq: 1,
                    ts: "t".into(),
                    update: json!({})
                },
            ],
            next_cursor: None,
            cursor_expired: false,
        }
        .validate()
        .is_err()
    );

    assert!(
        RunEventPage {
            entries: Vec::new(),
            next_cursor: Some(5),
            cursor_expired: true,
        }
        .validate()
        .is_err()
    );

    // A lease must be bounded and unambiguous.
    let lease = |ttl: u64, classes: Vec<ComputerActionClass>| ComputerControlRequest {
        request_id: "req-1".into(),
        scope: scope(),
        expected_version: 4,
        action_classes: classes,
        ttl_ms: ttl,
    };
    assert!(
        lease(0, vec![ComputerActionClass::Semantic])
            .validate()
            .is_err()
    );
    assert!(
        lease(u64::MAX, vec![ComputerActionClass::Semantic])
            .validate()
            .is_err()
    );
    assert!(lease(1000, Vec::new()).validate().is_err());
    assert!(
        lease(
            1000,
            vec![ComputerActionClass::Semantic, ComputerActionClass::Semantic]
        )
        .validate()
        .is_err()
    );
    assert!(
        lease(1000, vec![ComputerActionClass::Semantic])
            .validate()
            .is_ok()
    );
}

#[test]
fn a_computer_lease_is_fenced_to_the_observed_revision() {
    let request = ComputerControlRequest {
        request_id: "req-1".into(),
        scope: scope(),
        expected_version: 4,
        action_classes: vec![ComputerActionClass::Semantic],
        ttl_ms: 30_000,
    };
    assert!(request.ensure_fresh_against(4).is_ok());
    // The authority moved on: the human approved something else.
    for authority_version in [0, 3, 5, u64::MAX] {
        assert!(
            request.ensure_fresh_against(authority_version).is_err(),
            "a lease must not replay against revision {authority_version}"
        );
    }
}

// ---------------------------------------------------------------------------
// Redaction invariants
// ---------------------------------------------------------------------------

#[test]
fn credentials_never_enter_a_public_projection() {
    let secrets = [
        "Authorization: Bearer abc123",
        "-----BEGIN OPENSSH PRIVATE KEY-----",
        "xai-live-abc123def456",
        "api_key=abc",
        "AKIAIOSFODNN7EXAMPLE",
    ];

    for secret in secrets {
        // Prompt preview.
        let run = DurableRun {
            run_id: "run-1".into(),
            session_id: SESSION.into(),
            workspace: WORKSPACE.into(),
            request_id: "req-1".into(),
            state: DurableRunState::Running,
            prompt_preview: secret.into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:01Z".into(),
        };
        assert_eq!(
            run.validate(),
            Err("credential_material"),
            "prompt {secret}"
        );

        // Event payload.
        assert_eq!(
            RunEvent {
                seq: 1,
                ts: "2026-01-01T00:00:00Z".into(),
                update: json!({ "text": secret }),
            }
            .validate(),
            Err("credential_material")
        );

        // Review diff and changed-file summary.
        assert_eq!(
            ReviewReceipt {
                changed_files: Vec::new(),
                diff: secret.into(),
                diff_truncated: false,
                fingerprint: "fp".into(),
            }
            .validate(),
            Err("credential_material")
        );

        // Recovery reason and Computer Use metadata.
        assert_eq!(
            RunNotification::Recovery {
                scope: scope(),
                after_seq: 1,
                reason: secret.into(),
                poll_tool: "ptah_get_events".into(),
            }
            .validate(),
            Err("credential_material")
        );
        assert_eq!(
            ComputerControlResponse {
                scope: scope(),
                version: 1,
                disposition: secret.into(),
            }
            .validate(),
            Err("credential_material")
        );
        assert_eq!(
            ComputerEvent {
                seq: 1,
                ts: "2026-01-01T00:00:00Z".into(),
                kind: "observation".into(),
                detail: json!({ "note": secret }),
            }
            .validate(),
            Err("credential_material")
        );
    }
}

#[test]
fn absolute_paths_and_provider_urls_never_enter_authority_metadata() {
    let receipt = |path: &str| ReviewReceipt {
        changed_files: vec![ChangedFile {
            path: path.into(),
            summary: "edited".into(),
        }],
        diff: String::new(),
        diff_truncated: false,
        fingerprint: "fp".into(),
    };

    assert_eq!(
        receipt("/Users/dev/secret.rs").validate(),
        Err("absolute_path")
    );
    assert_eq!(
        receipt("C:\\Users\\dev\\secret.rs").validate(),
        Err("absolute_path")
    );
    assert_eq!(receipt("~/secret.rs").validate(), Err("absolute_path"));
    assert_eq!(
        receipt("src/../../etc/passwd").validate(),
        Err("parent_escape")
    );
    assert_eq!(
        receipt("https://provider.test/repo.rs").validate(),
        Err("provider_url")
    );
    assert!(receipt("src/lib.rs").validate().is_ok());

    // Recovery metadata is authority-generated: no endpoint may leak through.
    assert_eq!(
        RunNotification::Recovery {
            scope: scope(),
            after_seq: 1,
            reason: "https://api.provider.test/v1/runs".into(),
            poll_tool: "ptah_get_events".into(),
        }
        .validate(),
        Err("provider_url")
    );

    // A diff legitimately contains URLs and paths; it must still be accepted.
    let with_code = ReviewReceipt {
        changed_files: vec![ChangedFile {
            path: "src/net.rs".into(),
            summary: "call the API".into(),
        }],
        diff: "+ get(\"https://example.test\")\n+ open(\"/etc/hosts\")".into(),
        diff_truncated: false,
        fingerprint: "fp".into(),
    };
    assert!(with_code.validate().is_ok());
}

#[test]
fn the_host_advertisement_is_share_safe() {
    let encoded = serde_json::to_string(&host(2)).expect("advertisement serializes");
    for probe in [
        "Bearer", "api_key", "sk-", "/Users/", "/home/", "C:\\", "https://", "token",
    ] {
        assert!(
            !encoded.contains(probe),
            "headless advertisement leaked {probe}: {encoded}"
        );
    }
}
