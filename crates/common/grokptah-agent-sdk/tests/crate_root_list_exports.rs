//! Public-consumer smoke for crate-root external-worker list exports.
//!
//! Another repository must be able to import every list/query/summary/page
//! type and the bounded list-limit constant from the documented SDK crate
//! root. This file uses only crate-root paths so a missing re-export is a
//! compile failure.

use grokptah_agent_sdk::{
    ExternalWorkerListPage, ExternalWorkerListQuery, ExternalWorkerProvider, ExternalWorkerState,
    ExternalWorkerSummary, MAX_EXTERNAL_WORKER_LIST_LIMIT,
};

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
