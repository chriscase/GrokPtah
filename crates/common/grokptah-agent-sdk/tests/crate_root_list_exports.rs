//! Hosted-equivalent public-consumer smoke for crate-root SDK exports.
//!
//! Another repository — and hosted Desktop `cargo test -p grokptah-agent-sdk`
//! — must import launch/lifecycle and list/query/summary/page types from the
//! documented crate root. This file uses only crate-root paths so a missing
//! re-export is a compile failure. It is not an internal `external_worker`
//! module fixture.

use grokptah_agent_sdk::{
    ExternalWorkerExecutionMode, ExternalWorkerLaunchRequest, ExternalWorkerLaunchResult,
    ExternalWorkerListPage, ExternalWorkerListQuery, ExternalWorkerProvider, ExternalWorkerRecord,
    ExternalWorkerRunRecord, ExternalWorkerState, ExternalWorkerSummary,
    MAX_EXTERNAL_WORKER_LIST_LIMIT,
};

fn crate_root_launch_request() -> ExternalWorkerLaunchRequest {
    ExternalWorkerLaunchRequest {
        request_id: "request-1".into(),
        provider: ExternalWorkerProvider::CursorCloud,
        provider_id: None,
        repository: "chriscase/GrokPtah".into(),
        starting_ref: "main".into(),
        prompt: "Review the exact candidate".into(),
        model: Some("composer-2".into()),
        execution_mode: ExternalWorkerExecutionMode::Isolated,
        auto_create_pr: false,
        bounds: None,
    }
}

fn crate_root_worker_record() -> ExternalWorkerRecord {
    ExternalWorkerRecord {
        provider: ExternalWorkerProvider::CursorCloud,
        provider_id: None,
        external_agent_id: "bc-00000000-0000-0000-0000-000000000002".into(),
        repository: "chriscase/GrokPtah".into(),
        starting_ref: "main".into(),
        state: ExternalWorkerState::Ready,
        branch: None,
        worker_url: Some(
            "https://cursor.com/agents/bc-00000000-0000-0000-0000-000000000002".into(),
        ),
        created_at: "2026-08-24T00:00:00Z".into(),
        updated_at: "2026-08-24T00:00:01Z".into(),
    }
}

#[test]
fn crate_root_list_exports_stay_bounded_and_provider_neutral() {
    assert_eq!(MAX_EXTERNAL_WORKER_LIST_LIMIT, 100);

    let query = ExternalWorkerListQuery {
        limit: Some(MAX_EXTERNAL_WORKER_LIST_LIMIT),
        cursor: None,
        include_archived: false,
    };
    query
        .validate()
        .expect("bounded crate-root list query is valid");
    assert!(
        serde_json::from_str::<ExternalWorkerListQuery>(
            r#"{"limit":20,"repository":"org/repo","startingRef":"main"}"#
        )
        .is_err(),
        "list query must keep deny_unknown_fields at the crate-root type"
    );
    let oversized = ExternalWorkerListQuery {
        limit: Some(MAX_EXTERNAL_WORKER_LIST_LIMIT + 1),
        ..ExternalWorkerListQuery::default()
    };
    assert_eq!(
        oversized.validate(),
        Err("list limit must be between 1 and 100")
    );

    let item = ExternalWorkerSummary {
        provider: ExternalWorkerProvider::CursorCloud,
        provider_id: None,
        external_agent_id: "bc-00000000-0000-0000-0000-000000000001".into(),
        state: ExternalWorkerState::Ready,
        worker_url: Some(
            "https://cursor.com/agents/bc-00000000-0000-0000-0000-000000000001".into(),
        ),
        latest_run_id: Some("run-00000000-0000-0000-0000-000000000001".into()),
        created_at: "2026-08-24T00:00:00Z".into(),
        updated_at: "2026-08-24T00:00:01Z".into(),
    };
    item.validate()
        .expect("crate-root identity summary is valid");
    let value = serde_json::to_value(&item).expect("summary serializes");
    assert_eq!(value["provider"], "cursor_cloud");
    assert!(value.get("repository").is_none());
    assert!(value.get("startingRef").is_none());
    assert!(value.get("name").is_none());
    assert!(
        serde_json::from_value::<ExternalWorkerSummary>(serde_json::json!({
            "provider": "cursor_cloud",
            "externalAgentId": "agent-1",
            "repository": "org/repo",
            "startingRef": "main",
            "state": "ready",
            "createdAt": "now",
            "updatedAt": "now"
        }))
        .is_err(),
        "summaries must not accept repository or startingRef at the crate root"
    );
    assert!(
        serde_json::from_value::<ExternalWorkerSummary>(serde_json::json!({
            "provider": "cursor_cloud",
            "externalAgentId": "agent-1",
            "name": "caf\u{FFFD}",
            "state": "ready",
            "createdAt": "now",
            "updatedAt": "now"
        }))
        .is_err(),
        "summaries must not accept a provider name at the crate root"
    );

    let page = ExternalWorkerListPage {
        items: vec![item],
        next_cursor: None,
    };
    page.validate().expect("crate-root list page is valid");
    assert!(
        serde_json::from_value::<ExternalWorkerListPage>(serde_json::json!({
            "items": [],
            "rawProvider": {"authorization": "Bearer secret"}
        }))
        .is_err(),
        "list pages must keep deny_unknown_fields at the crate-root type"
    );
}

#[test]
fn crate_root_launch_and_get_dtos_validate_and_round_trip_including_fffd() {
    let mut request = crate_root_launch_request();
    request.prompt = "审查候选用例 caf\u{FFFD}".into();
    request
        .validate()
        .expect("crate-root launch DTO accepts U+FFFD as valid Unicode");
    let encoded = serde_json::to_value(&request).expect("launch request serializes");
    assert!(encoded.get("name").is_none());
    assert_eq!(encoded["autoCreatePr"], false);
    let decoded: ExternalWorkerLaunchRequest =
        serde_json::from_value(encoded).expect("launch request deserializes");
    decoded.validate().expect("round-tripped launch validates");
    assert_eq!(decoded.prompt, "审查候选用例 caf\u{FFFD}");
    assert!(decoded.prompt.contains('\u{FFFD}'));
    assert!(
        serde_json::from_value::<ExternalWorkerLaunchRequest>(serde_json::json!({
            "requestId": "request-1",
            "provider": "cursor_cloud",
            "repository": "chriscase/GrokPtah",
            "startingRef": "main",
            "prompt": "Review the exact candidate",
            "executionMode": "isolated",
            "autoCreatePr": false,
            "name": "caf\u{FFFD}"
        }))
        .is_err(),
        "launch requests must not accept a provider name at the crate root"
    );

    let worker = crate_root_worker_record();
    worker
        .validate()
        .expect("crate-root worker record is valid");
    let worker_json = serde_json::to_value(&worker).expect("worker record serializes");
    assert!(worker_json.get("name").is_none());
    assert_eq!(worker_json["externalAgentId"], worker.external_agent_id);
    assert_eq!(worker_json["state"], "ready");
    let worker_round: ExternalWorkerRecord =
        serde_json::from_value(worker_json).expect("worker record deserializes");
    worker_round
        .validate()
        .expect("round-tripped worker record validates");
    assert_eq!(worker_round, worker);
    assert_ne!(worker_round.state, ExternalWorkerState::Cancelled);
    assert_ne!(worker_round.state, ExternalWorkerState::Archived);
    assert!(
        serde_json::from_value::<ExternalWorkerRecord>(serde_json::json!({
            "provider": "cursor_cloud",
            "externalAgentId": "bc-00000000-0000-0000-0000-000000000002",
            "repository": "chriscase/GrokPtah",
            "startingRef": "main",
            "state": "ready",
            "createdAt": "2026-08-24T00:00:00Z",
            "updatedAt": "2026-08-24T00:00:01Z",
            "name": "caf\u{FFFD}"
        }))
        .is_err(),
        "worker records must not accept a provider name at the crate root"
    );

    let run = ExternalWorkerRunRecord {
        external_agent_id: worker.external_agent_id.clone(),
        external_run_id: "run-00000000-0000-0000-0000-000000000001".into(),
        state: ExternalWorkerState::Provisioning,
        last_seq: 0,
        terminal_result: None,
        created_at: "2026-08-24T00:00:00Z".into(),
        updated_at: "2026-08-24T00:00:01Z".into(),
    };
    run.validate().expect("crate-root run record is valid");
    let result = ExternalWorkerLaunchResult {
        worker: worker.clone(),
        run,
    };
    result
        .validate()
        .expect("crate-root launch/get result is valid");
    let result_json = serde_json::to_value(&result).expect("launch result serializes");
    assert!(result_json["worker"].get("name").is_none());
    let result_round: ExternalWorkerLaunchResult =
        serde_json::from_value(result_json).expect("launch result deserializes");
    result_round
        .validate()
        .expect("round-tripped launch result validates");
    assert_eq!(result_round, result);
    assert_eq!(
        result_round.worker.external_agent_id,
        worker.external_agent_id
    );
    assert_eq!(result_round.worker.state, ExternalWorkerState::Ready);
}
