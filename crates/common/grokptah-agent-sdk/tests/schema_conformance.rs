//! Versioned schema conformance fixture.
//!
//! `docs/schemas/*.v1.schema.json` is the published contract non-Rust
//! consumers integrate against. Before this fixture existed nothing checked
//! that the Rust DTOs still matched it, so the two could drift silently — and
//! they had: every published definition declares
//! `"additionalProperties": false` while several Rust types accepted unknown
//! fields.
//!
//! The schemas are pulled in with `include_str!` so the fixture is hermetic
//! and fails to compile if a schema is moved or deleted.

use grokptah_agent_sdk::computer::{
    ComputerActionClass, ComputerControlRequest, ComputerControlResponse, ComputerEvent,
    ComputerEventPage,
};
use grokptah_agent_sdk::error::{ErrorCode, ErrorEnvelope, ErrorEventRange};
use grokptah_agent_sdk::headless::{HEADLESS_CONTRACT_VERSION, HeadlessHostInfo, HeadlessPlatform};
use grokptah_agent_sdk::run::{
    Bounds, ChangedFile, DurableRun, DurableRunState, ExecutionMode, ReviewReceipt, RunEvent,
    RunEventPage, RunNotification, RunScope, SubmitTaskRequest,
};
use grokptah_agent_sdk::{
    CONTRACT_VERSION, CapabilityRevision, CapabilitySet, EXTERNAL_WORKER_CONTRACT_VERSION,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const RUN_SCHEMA: &str = include_str!("../../../../docs/schemas/grokptah-run.v1.schema.json");
const CAPABILITIES_SCHEMA: &str =
    include_str!("../../../../docs/schemas/grokptah-capabilities.v1.schema.json");
const EXTERNAL_WORKER_SCHEMA: &str =
    include_str!("../../../../docs/schemas/grokptah-external-worker.v1.schema.json");

fn parse(raw: &str) -> Value {
    serde_json::from_str(raw).expect("published schema is valid JSON")
}

/// Property names declared for `$defs/<name>`.
fn declared_properties(schema: &Value, name: &str) -> BTreeSet<String> {
    schema["$defs"][name]["properties"]
        .as_object()
        .unwrap_or_else(|| panic!("schema has no $defs/{name}/properties"))
        .keys()
        .cloned()
        .collect()
}

/// Property names a fully populated Rust value actually serializes.
fn serialized_properties<T: serde::Serialize>(value: &T) -> BTreeSet<String> {
    serde_json::to_value(value)
        .expect("value serializes")
        .as_object()
        .expect("value serializes to a JSON object")
        .keys()
        .cloned()
        .collect()
}

fn scope() -> RunScope {
    RunScope {
        session_id: "session-1".into(),
        workspace: "/approved".into(),
        run_id: "run-1".into(),
    }
}

fn assert_same_keys(label: &str, rust: BTreeSet<String>, schema: BTreeSet<String>) {
    assert_eq!(
        rust, schema,
        "{label}: Rust wire keys and published schema properties disagree"
    );
}

#[test]
fn published_schema_versions_are_pinned_to_the_rust_contract_constants() {
    let run = parse(RUN_SCHEMA);
    let capabilities = parse(CAPABILITIES_SCHEMA);
    let external = parse(EXTERNAL_WORKER_SCHEMA);

    assert_eq!(run["$id"], "urn:grokptah:schema:run:v1");
    assert_eq!(capabilities["$id"], "urn:grokptah:schema:capabilities:v1");
    assert_eq!(external["$id"], "urn:grokptah:schema:external-worker:v1");

    // The identifier a consumer negotiates on must be the same string on both
    // sides, or discovery silently succeeds against the wrong contract.
    assert_eq!(
        capabilities["properties"]["contract"]["const"], CONTRACT_VERSION,
        "capability schema and CONTRACT_VERSION disagree"
    );
    assert_eq!(
        external["properties"]["contract"]["const"], EXTERNAL_WORKER_CONTRACT_VERSION,
        "external worker schema and EXTERNAL_WORKER_CONTRACT_VERSION disagree"
    );
}

#[test]
fn every_published_object_definition_is_closed() {
    for (label, raw) in [
        ("run", RUN_SCHEMA),
        ("capabilities", CAPABILITIES_SCHEMA),
        ("external-worker", EXTERNAL_WORKER_SCHEMA),
    ] {
        let schema = parse(raw);
        let Some(defs) = schema["$defs"].as_object() else {
            continue;
        };
        for (name, def) in defs {
            if def["type"] != json!("object") {
                continue;
            }
            assert_eq!(
                def["additionalProperties"],
                json!(false),
                "{label}: $defs/{name} must stay closed to unknown properties"
            );
        }
    }
}

#[test]
fn run_contract_wire_keys_match_the_published_schema() {
    let schema = parse(RUN_SCHEMA);

    assert_same_keys(
        "scope",
        serialized_properties(&scope()),
        declared_properties(&schema, "scope"),
    );

    let bounds = Bounds {
        max_prompt_bytes: Some(4096),
        max_rounds: Some(8),
        max_duration_ms: Some(120_000),
    };
    assert_same_keys(
        "bounds",
        serialized_properties(&bounds),
        declared_properties(&schema, "bounds"),
    );

    let submit = SubmitTaskRequest {
        request_id: "req-1".into(),
        session_id: "session-1".into(),
        workspace: "/approved".into(),
        prompt: "review".into(),
        bounds: Some(bounds.clone()),
        execution_mode: Some(ExecutionMode::IsolatedWorktree),
        allow_queue: Some(true),
    };
    assert_same_keys(
        "submitTask",
        serialized_properties(&submit),
        declared_properties(&schema, "submitTask"),
    );

    let durable = DurableRun {
        run_id: "run-1".into(),
        session_id: "session-1".into(),
        workspace: "/approved".into(),
        request_id: "req-1".into(),
        state: DurableRunState::Running,
        prompt_preview: "review".into(),
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:01Z".into(),
    };
    assert_same_keys(
        "durableRun",
        serialized_properties(&durable),
        declared_properties(&schema, "durableRun"),
    );

    let event = RunEvent {
        seq: 1,
        ts: "2026-01-01T00:00:00Z".into(),
        update: json!({"kind": "tool_call"}),
    };
    assert_same_keys(
        "event",
        serialized_properties(&event),
        declared_properties(&schema, "event"),
    );

    let page = RunEventPage {
        entries: vec![event.clone()],
        next_cursor: Some(2),
        cursor_expired: false,
    };
    assert_same_keys(
        "runEventPage",
        serialized_properties(&page),
        declared_properties(&schema, "runEventPage"),
    );

    let changed = ChangedFile {
        path: "src/lib.rs".into(),
        summary: "edited".into(),
    };
    assert_same_keys(
        "changedFile",
        serialized_properties(&changed),
        declared_properties(&schema, "changedFile"),
    );

    let receipt = ReviewReceipt {
        changed_files: vec![changed],
        diff: "diff".into(),
        diff_truncated: false,
        fingerprint: "fp".into(),
    };
    assert_same_keys(
        "reviewReceipt",
        serialized_properties(&receipt),
        declared_properties(&schema, "reviewReceipt"),
    );

    let envelope = ErrorEnvelope {
        code: ErrorCode::StaleOrRecovery,
        message: "resume".into(),
        request_id: Some("req-1".into()),
        reason_code: Some("cursor_expired".into()),
        event_range: Some(ErrorEventRange {
            start_seq: 1,
            end_seq: 2,
        }),
    };
    assert_same_keys(
        "errorEnvelope",
        serialized_properties(&envelope),
        declared_properties(&schema, "errorEnvelope"),
    );
}

#[test]
fn computer_contract_wire_keys_match_the_published_schema() {
    let schema = parse(RUN_SCHEMA);

    let request = ComputerControlRequest {
        request_id: "req-1".into(),
        scope: scope(),
        expected_version: 4,
        action_classes: vec![ComputerActionClass::Semantic],
        ttl_ms: 30_000,
    };
    assert_same_keys(
        "computerControlRequest",
        serialized_properties(&request),
        declared_properties(&schema, "computerControlRequest"),
    );

    let response = ComputerControlResponse {
        scope: scope(),
        version: 5,
        disposition: "granted".into(),
    };
    assert_same_keys(
        "computerControlResponse",
        serialized_properties(&response),
        declared_properties(&schema, "computerControlResponse"),
    );

    let event = ComputerEvent {
        seq: 1,
        ts: "2026-01-01T00:00:00Z".into(),
        kind: "observation".into(),
        detail: json!({"targets": 2}),
    };
    assert_same_keys(
        "computerEvent",
        serialized_properties(&event),
        declared_properties(&schema, "computerEvent"),
    );

    let page = ComputerEventPage {
        entries: vec![event],
        next_cursor: Some(2),
        cursor_expired: false,
    };
    assert_same_keys(
        "computerEventPage",
        serialized_properties(&page),
        declared_properties(&schema, "computerEventPage"),
    );
}

#[test]
fn notification_variants_match_the_published_one_of_branches() {
    let schema = parse(RUN_SCHEMA);
    let branches = schema["$defs"]["notification"]["oneOf"]
        .as_array()
        .expect("notification declares oneOf branches");

    let branch_keys = |index: usize| -> BTreeSet<String> {
        branches[index]["properties"]
            .as_object()
            .expect("branch declares properties")
            .keys()
            .cloned()
            .collect()
    };

    let event = RunNotification::Event {
        scope: scope(),
        event: RunEvent {
            seq: 1,
            ts: "2026-01-01T00:00:00Z".into(),
            update: json!({"kind": "message"}),
        },
    };
    assert_same_keys(
        "notification.event",
        serialized_properties(&event),
        branch_keys(0),
    );

    let recovery = RunNotification::Recovery {
        scope: scope(),
        after_seq: 7,
        reason: "cursor_expired".into(),
        poll_tool: "ptah_get_events".into(),
    };
    assert_same_keys(
        "notification.recovery",
        serialized_properties(&recovery),
        branch_keys(1),
    );
}

#[test]
fn enumerations_match_the_published_value_sets() {
    let schema = parse(RUN_SCHEMA);

    let schema_values = |pointer: &Value| -> BTreeSet<String> {
        pointer
            .as_array()
            .expect("enum is an array")
            .iter()
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect()
    };

    let rust_values = |values: Vec<Value>| -> BTreeSet<String> {
        values
            .into_iter()
            .map(|value| {
                value
                    .as_str()
                    .expect("enum serializes to a string")
                    .to_owned()
            })
            .collect()
    };

    let states = [
        DurableRunState::Queued,
        DurableRunState::Running,
        DurableRunState::Completed,
        DurableRunState::Failed,
        DurableRunState::Cancelled,
        DurableRunState::Interrupted,
        DurableRunState::LimitReached,
    ]
    .iter()
    .map(|state| serde_json::to_value(state).expect("state serializes"))
    .collect();
    assert_eq!(
        rust_values(states),
        schema_values(&schema["$defs"]["runState"]["enum"]),
        "durable run states drifted from the published schema"
    );

    let codes = [
        ErrorCode::InvalidRequest,
        ErrorCode::Unauthenticated,
        ErrorCode::ForbiddenScope,
        ErrorCode::NotFound,
        ErrorCode::StaleOrRecovery,
        ErrorCode::Capacity,
        ErrorCode::AuthorityUnavailable,
        ErrorCode::Internal,
    ]
    .iter()
    .map(|code| serde_json::to_value(code).expect("code serializes"))
    .collect();
    assert_eq!(
        rust_values(codes),
        schema_values(&schema["$defs"]["errorEnvelope"]["properties"]["code"]["enum"]),
        "error codes drifted from the published schema"
    );

    let classes = [
        ComputerActionClass::Semantic,
        ComputerActionClass::TextEntry,
    ]
    .iter()
    .map(|class| serde_json::to_value(class).expect("class serializes"))
    .collect();
    assert_eq!(
        rust_values(classes),
        schema_values(
            &schema["$defs"]["computerControlRequest"]["properties"]["actionClasses"]["items"]["enum"]
        ),
        "computer action classes drifted from the published schema"
    );
}

#[test]
fn published_bounds_ceiling_matches_the_rust_contract_ceiling() {
    let schema = parse(RUN_SCHEMA);
    let ceiling = schema["$defs"]["bounds"]["properties"]["maxRounds"]["maximum"]
        .as_u64()
        .expect("schema pins a maxRounds ceiling");
    assert_eq!(
        ceiling,
        u64::from(grokptah_agent_sdk::run::MAX_ROUNDS),
        "schema maxRounds ceiling and MAX_ROUNDS disagree"
    );

    // A value one above the published ceiling must be refused by Rust too.
    assert!(
        Bounds {
            max_rounds: Some(u16::try_from(ceiling + 1).expect("ceiling fits u16")),
            ..Bounds::default()
        }
        .validate()
        .is_err(),
        "Rust accepted a round count the published schema forbids"
    );
}

#[test]
fn a_contract_version_mismatch_is_rejected() {
    // Capability discovery must refuse a set advertised under another version.
    let mut set = CapabilitySet::empty();
    assert!(set.is_current());
    set.contract = "grokptah.capabilities.v2".into();
    assert!(
        !set.is_current(),
        "a mismatched capability contract must not be treated as current"
    );

    // The headless advertisement must refuse both contract identifiers.
    let host = |contract: &str, headless: &str| HeadlessHostInfo {
        host_id: "worker-1".into(),
        contract: contract.into(),
        headless_contract: headless.into(),
        platform: HeadlessPlatform::Linux,
        revision: CapabilityRevision::INITIAL,
        capabilities: CapabilitySet::empty(),
    };

    assert!(
        host(CONTRACT_VERSION, HEADLESS_CONTRACT_VERSION)
            .validate()
            .is_ok()
    );

    let wrong_capability = host("grokptah.capabilities.v2", HEADLESS_CONTRACT_VERSION)
        .validate()
        .expect_err("a capability contract mismatch must fail closed");
    assert_eq!(
        wrong_capability.reason_code.as_deref(),
        Some("capability_contract_mismatch")
    );

    let wrong_headless = host(CONTRACT_VERSION, "grokptah.headless.v2")
        .validate()
        .expect_err("a headless contract mismatch must fail closed");
    assert_eq!(
        wrong_headless.reason_code.as_deref(),
        Some("headless_contract_mismatch")
    );
}

#[test]
fn unknown_fields_are_refused_exactly_where_the_schema_closes_the_object() {
    // Each probe adds one property the published schema does not declare.
    let probes: Vec<(&str, Value)> = vec![
        (
            "scope",
            json!({"sessionId": "s", "workspace": "/w", "runId": "r", "extra": 1}),
        ),
        ("bounds", json!({"maxRounds": 4, "extra": 1})),
        (
            "submitTask",
            json!({"requestId": "q", "sessionId": "s", "workspace": "/w", "prompt": "p", "extra": 1}),
        ),
        (
            "durableRun",
            json!({
                "runId": "r", "sessionId": "s", "workspace": "/w", "requestId": "q",
                "state": "running", "promptPreview": "p",
                "createdAt": "t", "updatedAt": "t", "extra": 1
            }),
        ),
        (
            "event",
            json!({"seq": 1, "ts": "t", "update": {}, "extra": 1}),
        ),
        (
            "runEventPage",
            json!({"entries": [], "cursorExpired": false, "extra": 1}),
        ),
        (
            "changedFile",
            json!({"path": "a", "summary": "b", "extra": 1}),
        ),
        (
            "reviewReceipt",
            json!({"changedFiles": [], "diff": "", "diffTruncated": false, "fingerprint": "f", "extra": 1}),
        ),
        (
            "computerControlRequest",
            json!({
                "requestId": "q",
                "scope": {"sessionId": "s", "workspace": "/w", "runId": "r"},
                "expectedVersion": 1, "actionClasses": ["semantic"], "ttlMs": 10, "extra": 1
            }),
        ),
        (
            "computerControlResponse",
            json!({
                "scope": {"sessionId": "s", "workspace": "/w", "runId": "r"},
                "version": 1, "disposition": "ok", "extra": 1
            }),
        ),
        (
            "computerEvent",
            json!({"seq": 1, "ts": "t", "kind": "k", "detail": {}, "extra": 1}),
        ),
    ];

    // Sanity: the fixture must actually cover every closed run-contract object.
    assert_eq!(probes.len(), 11);

    for (name, payload) in probes {
        let rejected = match name {
            "scope" => serde_json::from_value::<RunScope>(payload).is_err(),
            "bounds" => serde_json::from_value::<Bounds>(payload).is_err(),
            "submitTask" => serde_json::from_value::<SubmitTaskRequest>(payload).is_err(),
            "durableRun" => serde_json::from_value::<DurableRun>(payload).is_err(),
            "event" => serde_json::from_value::<RunEvent>(payload).is_err(),
            "runEventPage" => serde_json::from_value::<RunEventPage>(payload).is_err(),
            "changedFile" => serde_json::from_value::<ChangedFile>(payload).is_err(),
            "reviewReceipt" => serde_json::from_value::<ReviewReceipt>(payload).is_err(),
            "computerControlRequest" => {
                serde_json::from_value::<ComputerControlRequest>(payload).is_err()
            }
            "computerControlResponse" => {
                serde_json::from_value::<ComputerControlResponse>(payload).is_err()
            }
            "computerEvent" => serde_json::from_value::<ComputerEvent>(payload).is_err(),
            other => panic!("unmapped probe {other}"),
        };
        assert!(
            rejected,
            "{name} accepted an unknown field the published schema forbids"
        );
    }
}

#[test]
fn tagged_notification_rejects_unknown_fields_in_both_branches() {
    assert!(
        serde_json::from_value::<RunNotification>(json!({
            "kind": "event",
            "scope": {"sessionId": "s", "workspace": "/w", "runId": "r"},
            "event": {"seq": 1, "ts": "t", "update": {}},
            "extra": 1
        }))
        .is_err()
    );

    assert!(
        serde_json::from_value::<RunNotification>(json!({
            "kind": "recovery",
            "scope": {"sessionId": "s", "workspace": "/w", "runId": "r"},
            "afterSeq": 1,
            "reason": "cursor_expired",
            "pollTool": "ptah_get_events",
            "extra": 1
        }))
        .is_err()
    );

    // An unknown discriminant is refused rather than defaulted.
    assert!(
        serde_json::from_value::<RunNotification>(json!({
            "kind": "somethingElse",
            "scope": {"sessionId": "s", "workspace": "/w", "runId": "r"}
        }))
        .is_err()
    );
}
