//! Disposable ContextDesk-shaped consumer of published crate-root SDK DTOs.
//!
//! This integration test stands in for a second product importing
//! `grokptah-agent-sdk` from another repository. It uses only documented
//! crate-root exports so a missing re-export is a compile failure. It is not
//! a live ContextDesk HTTP integration, a private-module fixture, or a native
//! desktop-authority test.

use grokptah_agent_sdk::{
    ErrorCode, ErrorEnvelope, ErrorEventRange, ExternalWorkerListPage, ExternalWorkerListQuery,
    ExternalWorkerProvider, ExternalWorkerRecord, ExternalWorkerState, ExternalWorkerSummary,
    MAX_EXTERNAL_WORKER_LIST_LIMIT,
};

fn utf8_summary(state: ExternalWorkerState) -> ExternalWorkerSummary {
    ExternalWorkerSummary {
        provider: ExternalWorkerProvider::CursorCloud,
        provider_id: None,
        external_agent_id: "agent-审查-1".into(),
        state,
        worker_url: Some("https://cursor.com/agents/agent-审查-1".into()),
        latest_run_id: Some("run-审查-1".into()),
        created_at: "2026-08-25T00:00:00Z".into(),
        updated_at: "2026-08-25T00:00:01Z".into(),
    }
}

fn utf8_worker(state: ExternalWorkerState) -> ExternalWorkerRecord {
    ExternalWorkerRecord {
        provider: ExternalWorkerProvider::CursorCloud,
        provider_id: None,
        external_agent_id: "agent-审查-1".into(),
        repository: "chriscase/GrokPtah".into(),
        starting_ref: "main".into(),
        state,
        branch: None,
        worker_url: Some("https://cursor.com/agents/agent-审查-1".into()),
        created_at: "2026-08-25T00:00:00Z".into(),
        updated_at: "2026-08-25T00:00:01Z".into(),
    }
}

#[test]
fn context_desk_consumer_imports_only_crate_root_surfaces() {
    let source = include_str!("context_desk_consumer.rs");
    let manifest = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
    let module_path = ["grokptah_agent_sdk", "external_worker"].join("::");
    let error_module = ["grokptah_agent_sdk", "error", ""].join("::");
    let native_crate = ["grokptah", "agent", "bridge"].join("-");
    let native_module = ["grokptah", "agent", "bridge"].join("_");
    let tauri_package = ["@", "tauri", "-apps"].join("");
    let trusted_export = ["client", "trusted"].join("/");
    let bearer_header = ["Authorization", "Bearer"].join(": ");
    let desktop_src = ["src", "tauri"].join("-");
    assert!(
        source
            .lines()
            .filter(|line| line.trim_start().starts_with("use "))
            .all(|line| line.trim_start().starts_with("use grokptah_agent_sdk::{")),
        "ContextDesk consumer must import only crate-root SDK surfaces"
    );
    assert!(
        !source.contains(&module_path),
        "ContextDesk consumer must not reach through the external_worker module path"
    );
    assert!(
        !source.contains(&error_module),
        "ContextDesk consumer must not reach through the error module path"
    );
    assert!(
        !source.contains(&native_crate)
            && !source.contains(&native_module)
            && !source.contains(&desktop_src),
        "ContextDesk consumer must not depend on the native desktop bridge"
    );
    assert!(
        !source.contains(&tauri_package) && !source.contains(&trusted_export),
        "ContextDesk consumer must stay Tauri-free and must not import trusted modules"
    );
    assert!(
        !source.contains(&bearer_header),
        "ContextDesk consumer must not carry bearer credentials"
    );
    assert!(
        !manifest.contains(&native_crate)
            && !manifest.contains(&tauri_package)
            && !manifest.contains("tauri"),
        "public SDK package must stay host-neutral and Tauri-free"
    );
}

#[test]
fn context_desk_consumer_round_trips_list_archive_unarchive_with_utf8_names() {
    assert_eq!(MAX_EXTERNAL_WORKER_LIST_LIMIT, 100);

    let query = ExternalWorkerListQuery {
        limit: Some(20),
        cursor: Some("page-审查-1".into()),
        include_archived: false,
    };
    query
        .validate()
        .expect("UTF-8 identity cursor is valid at the crate root");
    let query_json = serde_json::to_value(&query).expect("list query serializes");
    assert_eq!(query_json["cursor"], "page-审查-1");
    assert_eq!(query_json["includeArchived"], false);
    assert!(
        query_json["includeArchived"].is_boolean(),
        "includeArchived false must serialize as a boolean, not JSON null"
    );
    let query_round: ExternalWorkerListQuery =
        serde_json::from_value(query_json).expect("list query deserializes");
    query_round
        .validate()
        .expect("round-tripped query validates");
    assert_eq!(query_round, query);

    let archived_query = ExternalWorkerListQuery {
        include_archived: true,
        ..ExternalWorkerListQuery::default()
    };
    archived_query
        .validate()
        .expect("includeArchived true is valid");
    let archived_query_json =
        serde_json::to_value(&archived_query).expect("archived list query serializes");
    assert_eq!(archived_query_json["includeArchived"], true);

    let unknown_cursor = ExternalWorkerListQuery {
        cursor: Some("page\u{0000}2".into()),
        ..ExternalWorkerListQuery::default()
    };
    assert_eq!(
        unknown_cursor.validate(),
        Err("worker identity contains a control character")
    );

    let ready = utf8_summary(ExternalWorkerState::Ready);
    ready.validate().expect("UTF-8 identity summary is valid");
    let ready_json = serde_json::to_value(&ready).expect("summary serializes");
    assert!(ready_json.get("name").is_none());
    assert!(ready_json.get("repository").is_none());
    assert!(ready_json.get("authorization").is_none());
    assert_eq!(ready_json["externalAgentId"], "agent-审查-1");

    assert!(
        serde_json::from_value::<ExternalWorkerSummary>(serde_json::json!({
            "provider": "cursor_cloud",
            "externalAgentId": "agent-审查-1",
            "name": "审查候选用例",
            "state": "ready",
            "createdAt": "now",
            "updatedAt": "now"
        }))
        .is_err(),
        "consumer summaries must reject a provider name field"
    );

    let page = ExternalWorkerListPage {
        items: vec![ready.clone()],
        next_cursor: Some("page-审查-2".into()),
    };
    page.validate().expect("UTF-8 list page is valid");
    let page_round: ExternalWorkerListPage =
        serde_json::from_value(serde_json::to_value(&page).expect("list page serializes"))
            .expect("list page deserializes");
    page_round.validate().expect("round-tripped page validates");
    assert_eq!(page_round.items[0].external_agent_id, "agent-审查-1");
    assert_ne!(page_round.items[0].state, ExternalWorkerState::Archived);

    let archived = utf8_worker(ExternalWorkerState::Archived);
    archived.validate().expect("archived UTF-8 worker is valid");
    let archived_json = serde_json::to_value(&archived).expect("archived worker serializes");
    assert_eq!(archived_json["state"], "archived");
    assert_eq!(archived_json["externalAgentId"], "agent-审查-1");
    assert!(archived_json.get("name").is_none());
    assert!(archived_json.get("apiKey").is_none());
    assert!(archived_json.get("authorization").is_none());
    let archived_round: ExternalWorkerRecord =
        serde_json::from_value(archived_json).expect("archived worker deserializes");
    archived_round
        .validate()
        .expect("round-tripped archive validates");
    assert_eq!(archived_round.state, ExternalWorkerState::Archived);
    assert_eq!(archived_round.external_agent_id, "agent-审查-1");

    let restored = utf8_worker(ExternalWorkerState::Ready);
    restored.validate().expect("unarchived worker is valid");
    let restored_json = serde_json::to_value(&restored).expect("unarchived worker serializes");
    assert_eq!(restored_json["state"], "ready");
    assert_eq!(restored_json["externalAgentId"], "agent-审查-1");
    assert_ne!(restored_json["state"], "archived");
    let restored_round: ExternalWorkerRecord =
        serde_json::from_value(restored_json).expect("unarchived worker deserializes");
    restored_round
        .validate()
        .expect("round-tripped unarchive validates");
    assert_ne!(restored_round.state, ExternalWorkerState::Archived);
    assert_ne!(restored_round.state, ExternalWorkerState::Cancelled);
}

#[test]
fn context_desk_consumer_round_trips_typed_public_error_envelopes() {
    let expired = ErrorEnvelope {
        code: ErrorCode::StaleOrRecovery,
        message: "列表游标已过期".into(),
        request_id: Some("req-审查-1".into()),
        reason_code: Some("cursor_expired".into()),
        event_range: Some(ErrorEventRange {
            start_seq: 12,
            end_seq: 18,
        }),
    };
    let expired_json = serde_json::to_value(&expired).expect("expired cursor envelope serializes");
    assert_eq!(expired_json["code"], "stale_or_recovery");
    assert_eq!(expired_json["reasonCode"], "cursor_expired");
    assert_eq!(expired_json["message"], "列表游标已过期");
    assert!(expired_json.get("authorization").is_none());
    assert!(expired_json.get("privilegedPath").is_none());
    let expired_round: ErrorEnvelope =
        serde_json::from_value(expired_json).expect("expired cursor envelope deserializes");
    assert_eq!(expired_round, expired);

    let unknown = ErrorEnvelope {
        code: ErrorCode::StaleOrRecovery,
        message: "list cursor is unknown".into(),
        request_id: None,
        reason_code: Some("unknown_cursor".into()),
        event_range: None,
    };
    let unknown_json = serde_json::to_value(&unknown).expect("unknown cursor envelope serializes");
    assert_eq!(unknown_json["reasonCode"], "unknown_cursor");
    assert!(unknown_json.get("eventRange").is_none());
    let unknown_round: ErrorEnvelope =
        serde_json::from_value(unknown_json).expect("unknown cursor envelope deserializes");
    assert_eq!(unknown_round.reason_code.as_deref(), Some("unknown_cursor"));

    let unauthenticated = ErrorEnvelope {
        code: ErrorCode::Unauthenticated,
        message: "broker session expired".into(),
        request_id: Some("req-2".into()),
        reason_code: Some("invalid_authority".into()),
        event_range: None,
    };
    let unauthenticated_json =
        serde_json::to_value(&unauthenticated).expect("invalid authority envelope serializes");
    assert_eq!(unauthenticated_json["code"], "unauthenticated");
    let unauthenticated_round: ErrorEnvelope = serde_json::from_value(unauthenticated_json)
        .expect("invalid authority envelope deserializes");
    assert_eq!(unauthenticated_round.code, ErrorCode::Unauthenticated);

    let locked = ErrorEnvelope {
        code: ErrorCode::AuthorityUnavailable,
        message: "desktop authority is locked".into(),
        request_id: None,
        reason_code: None,
        event_range: None,
    };
    let locked_round: ErrorEnvelope = serde_json::from_value(
        serde_json::to_value(&locked).expect("authority-unavailable envelope serializes"),
    )
    .expect("authority-unavailable envelope deserializes");
    assert_eq!(locked_round.code, ErrorCode::AuthorityUnavailable);

    assert!(
        serde_json::from_value::<ErrorEnvelope>(serde_json::json!({
            "code": "stale_or_recovery",
            "message": "resume from the retained window",
            "reasonCode": "cursor_expired",
            "bearer": "secret"
        }))
        .is_err(),
        "public error envelopes must fail closed on credential fields"
    );
    assert!(
        serde_json::from_str::<ErrorEnvelope>(r#"{"code":"not_a_public_code","message":"nope"}"#)
            .is_err(),
        "unknown public error codes must fail closed"
    );
}
