use super::*;
use serde_json::Value;

const SERVED_CORPUS: &str =
    "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const SERVED_INDEX: &str =
    "sha256:2222222222222222222222222222222222222222222222222222222222222222";
const SOURCE_DIGEST: &str =
    "sha256:3333333333333333333333333333333333333333333333333333333333333333";

fn principal(capabilities: &[Capability]) -> Principal {
    Principal {
        principal_id: "alice".into(),
        tenant_id: "tenant-a".into(),
        project_ids: vec!["proj-1".into()],
        capabilities: capabilities.to_vec(),
    }
}

fn source(id: &str, visibility: Visibility) -> SourceDescriptor {
    SourceDescriptor {
        source_id: id.into(),
        visibility,
        tenant_id: "tenant-a".into(),
        project_id: None,
        owner_principal_id: None,
        digest: SOURCE_DIGEST.into(),
    }
}

fn request(
    action: Action,
    principal: Principal,
    sources: Vec<SourceDescriptor>,
) -> DecisionRequest {
    DecisionRequest {
        schema: HELP_DECISION_REQUEST_SCHEMA.into(),
        action,
        principal,
        corpus_digest: SERVED_CORPUS.into(),
        index_digest: SERVED_INDEX.into(),
        sources,
    }
}

fn decide(request: &DecisionRequest) -> DecisionResponse {
    authorize(request, SERVED_CORPUS, SERVED_INDEX)
}

// ---------------------------------------------------------------- defaults

#[test]
fn public_source_is_allowed_with_the_base_capability() {
    let response = decide(&request(
        Action::Search,
        principal(&[Capability::HelpSearch]),
        vec![source("pub-1", Visibility::Public)],
    ));
    assert!(response.allowed);
    assert_eq!(
        response.receipt.allowed_source_ids,
        vec!["pub-1".to_string()]
    );
    assert!(response.receipt.denied.is_empty());
}

#[test]
fn non_public_sources_are_denied_by_default() {
    // No project or private capability held: both must deny even though the
    // principal is a member of the project and owns the private source.
    let mut project = source("proj-a", Visibility::Project);
    project.project_id = Some("proj-1".into());
    let mut private = source("priv-a", Visibility::Private);
    private.owner_principal_id = Some("alice".into());

    let response = decide(&request(
        Action::Search,
        principal(&[Capability::HelpSearch]),
        vec![project, private],
    ));
    assert!(response.allowed, "the search action itself is permitted");
    assert!(response.receipt.allowed_source_ids.is_empty());
    assert_eq!(response.receipt.denied.len(), 2);
    for decision in &response.receipt.denied {
        assert_eq!(decision.denied_because, Some(DenyReason::MissingCapability));
    }
}

#[test]
fn a_malformed_scope_denies_instead_of_falling_back_to_public() {
    // A `project` source with no project id must not be treated as unscoped.
    let response = decide(&request(
        Action::Search,
        principal(&[Capability::HelpSearch, Capability::HelpSearchProject]),
        vec![source("proj-d", Visibility::Project)],
    ));
    assert_eq!(
        response.receipt.denied[0].denied_because,
        Some(DenyReason::MalformedScope)
    );
}

#[test]
fn tenant_is_checked_before_scope() {
    // A cross-tenant probe must not be able to distinguish "wrong project"
    // from "wrong tenant" and thereby learn that a project id exists.
    let mut foreign = source("proj-c", Visibility::Project);
    foreign.tenant_id = "tenant-z".into();
    foreign.project_id = Some("proj-1".into());

    let response = decide(&request(
        Action::Search,
        principal(&[Capability::HelpSearch, Capability::HelpSearchProject]),
        vec![foreign],
    ));
    assert_eq!(
        response.receipt.denied[0].denied_because,
        Some(DenyReason::TenantMismatch)
    );
}

#[test]
fn private_sources_are_owner_only() {
    let capabilities = [Capability::HelpSearch, Capability::HelpSearchPrivate];
    let mut mine = source("priv-a", Visibility::Private);
    mine.owner_principal_id = Some("alice".into());
    let mut theirs = source("priv-b", Visibility::Private);
    theirs.owner_principal_id = Some("mallory".into());

    let response = decide(&request(
        Action::Search,
        principal(&capabilities),
        vec![mine, theirs],
    ));
    assert_eq!(
        response.receipt.allowed_source_ids,
        vec!["priv-a".to_string()]
    );
    assert_eq!(
        response.receipt.denied[0].denied_because,
        Some(DenyReason::ScopeMismatch)
    );
}

#[test]
fn answer_requires_its_own_capability() {
    let denied = decide(&request(
        Action::Answer,
        principal(&[Capability::HelpSearch]),
        vec![source("pub-1", Visibility::Public)],
    ));
    assert!(!denied.allowed);
    assert_eq!(denied.denied_because, Some(DenyReason::MissingCapability));

    let allowed = decide(&request(
        Action::Answer,
        principal(&[Capability::HelpSearch, Capability::HelpAnswer]),
        vec![source("pub-1", Visibility::Public)],
    ));
    assert!(allowed.allowed);
}

#[test]
fn read_source_fails_when_its_only_source_is_denied() {
    // An allowed action with an empty allow-list would read as success.
    let response = decide(&request(
        Action::ReadSource,
        principal(&[Capability::HelpSearch]),
        vec![source("priv-a", Visibility::Private)],
    ));
    assert!(!response.allowed);
    assert!(response.receipt.allowed_source_ids.is_empty());
}

// ------------------------------------------------------------- stale index

#[test]
fn a_stale_corpus_or_index_digest_denies_the_whole_action() {
    let mut stale_corpus = request(
        Action::Search,
        principal(&[Capability::HelpSearch]),
        vec![source("pub-1", Visibility::Public)],
    );
    stale_corpus.corpus_digest =
        "sha256:9999999999999999999999999999999999999999999999999999999999999999".into();
    let response = decide(&stale_corpus);
    assert!(!response.allowed);
    assert_eq!(response.denied_because, Some(DenyReason::StaleIndex));

    let mut stale_index = request(
        Action::Search,
        principal(&[Capability::HelpSearch]),
        vec![source("pub-1", Visibility::Public)],
    );
    stale_index.index_digest =
        "sha256:9999999999999999999999999999999999999999999999999999999999999999".into();
    assert_eq!(
        decide(&stale_index).denied_because,
        Some(DenyReason::StaleIndex)
    );
}

// ------------------------------------------------------------------ bounds

#[test]
fn source_count_and_receipt_are_bounded() {
    let sources: Vec<SourceDescriptor> = (0..MAX_SOURCES_PER_DECISION + 40)
        .map(|index| source(&format!("pub-{index}"), Visibility::Public))
        .collect();
    let response = decide(&request(
        Action::Search,
        principal(&[Capability::HelpSearch]),
        sources,
    ));
    assert!(!response.allowed);
    assert_eq!(response.denied_because, Some(DenyReason::Bounds));
    // The receipt must not grow with an oversized request.
    assert!(response.receipt.denied.len() <= MAX_SOURCES_PER_DECISION);
}

#[test]
fn oversized_and_empty_identifiers_are_rejected() {
    let mut oversized = principal(&[Capability::HelpSearch]);
    oversized.principal_id = "a".repeat(MAX_ID_BYTES + 1);
    assert_eq!(
        decide(&request(Action::Search, oversized, vec![])).denied_because,
        Some(DenyReason::Bounds)
    );

    let mut empty = principal(&[Capability::HelpSearch]);
    empty.tenant_id = String::new();
    assert_eq!(
        decide(&request(Action::Search, empty, vec![])).denied_because,
        Some(DenyReason::Bounds)
    );
}

// ---------------------------------------------------------------- receipts

#[test]
fn receipts_carry_no_path_content_or_query_text() {
    let mut project = source("proj-a", Visibility::Project);
    project.project_id = Some("proj-1".into());
    let response = decide(&request(
        Action::Search,
        principal(&[Capability::HelpSearch, Capability::HelpSearchProject]),
        vec![project, source("pub-1", Visibility::Public)],
    ));

    let serialized = serde_json::to_string(&response.receipt).expect("receipt serializes");
    // The receipt is ids and digests only. Nothing that could carry a
    // filesystem path, a heading, source prose, or the user's question.
    for forbidden in [
        "docs/", "README", ".md", "#", "/Users/", "/home/", "heading", "path", "text", "content",
        "query",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "receipt leaked {forbidden}: {serialized}"
        );
    }
}

#[test]
fn receipt_digest_is_deterministic_and_input_sensitive() {
    let base = request(
        Action::Search,
        principal(&[Capability::HelpSearch]),
        vec![source("pub-1", Visibility::Public)],
    );
    assert_eq!(
        decide(&base).receipt.receipt_digest,
        decide(&base).receipt.receipt_digest
    );

    let mut other_principal = base.clone();
    other_principal.principal.principal_id = "bob".into();
    assert_ne!(
        decide(&base).receipt.receipt_digest,
        decide(&other_principal).receipt.receipt_digest
    );

    // Length-prefixed hashing: moving a separator between fields must change
    // the digest. A joined encoding would collide here.
    let mut split_a = base.clone();
    split_a.principal.principal_id = "ali|ce".into();
    let mut split_b = base.clone();
    split_b.principal.principal_id = "ali".into();
    split_b.principal.tenant_id = "ce|tenant-a".into();
    assert_ne!(
        decide(&split_a).receipt.receipt_digest,
        decide(&split_b).receipt.receipt_digest
    );
}

// -------------------------------------------------------- closed contracts

#[test]
fn unknown_fields_are_rejected_rather_than_ignored() {
    // A dropped `visibility` or capability restriction is how default-deny
    // silently becomes allow-by-omission.
    let payloads = [
        r#"{"schema":"grokptah.help-authority-request.v1","action":"search","principal":{"principal_id":"a","tenant_id":"t"},"corpus_digest":"d","index_digest":"i","bypassAuthority":true}"#,
        r#"{"schema":"grokptah.help-authority-request.v1","action":"search","principal":{"principal_id":"a","tenant_id":"t","isAdmin":true},"corpus_digest":"d","index_digest":"i"}"#,
        r#"{"schema":"grokptah.help-authority-request.v1","action":"search","principal":{"principal_id":"a","tenant_id":"t"},"corpus_digest":"d","index_digest":"i","sources":[{"source_id":"s","visibility":"public","tenant_id":"t","digest":"d","visibilityOverride":"public"}]}"#,
    ];
    for payload in payloads {
        assert!(
            authorize_json(payload, SERVED_CORPUS, SERVED_INDEX).is_err(),
            "payload was accepted: {payload}"
        );
    }
}

#[test]
fn unknown_enum_values_are_rejected() {
    for payload in [
        r#"{"schema":"grokptah.help-authority-request.v1","action":"search","principal":{"principal_id":"a","tenant_id":"t"},"corpus_digest":"d","index_digest":"i","sources":[{"source_id":"s","visibility":"internal","tenant_id":"t","digest":"d"}]}"#,
        r#"{"schema":"grokptah.help-authority-request.v1","action":"escalate","principal":{"principal_id":"a","tenant_id":"t"},"corpus_digest":"d","index_digest":"i"}"#,
        r#"{"schema":"grokptah.help-authority-request.v1","action":"search","principal":{"principal_id":"a","tenant_id":"t","capabilities":["help_admin"]},"corpus_digest":"d","index_digest":"i"}"#,
    ] {
        assert!(
            authorize_json(payload, SERVED_CORPUS, SERVED_INDEX).is_err(),
            "payload was accepted: {payload}"
        );
    }
}

#[test]
fn an_unknown_schema_denies_without_evaluating_sources() {
    let mut wrong = request(
        Action::Search,
        principal(&[Capability::HelpSearch]),
        vec![source("pub-1", Visibility::Public)],
    );
    wrong.schema = "grokptah.help-authority-request.v2".into();
    let response = decide(&wrong);
    assert!(!response.allowed);
    assert_eq!(response.denied_because, Some(DenyReason::UnknownSchema));
    assert!(response.receipt.allowed_source_ids.is_empty());
}

// ------------------------------------------------------------ shared gates

/// Fixtures executed by both this crate and the TypeScript mirror.
#[test]
fn shared_parity_fixtures_hold() {
    let raw = include_str!("../fixtures/authority-parity.json");
    let doc: Value = serde_json::from_str(raw).expect("fixtures parse");
    let corpus = doc["servedCorpusDigest"].as_str().expect("corpus digest");
    let index = doc["servedIndexDigest"].as_str().expect("index digest");
    let cases = doc["cases"].as_array().expect("cases");
    assert!(
        cases.len() >= 20,
        "parity set is too small to be meaningful"
    );

    for case in cases {
        let name = case["name"].as_str().unwrap_or("<unnamed>");
        let payload = serde_json::to_string(&case["request"]).expect("request serializes");
        let expect = &case["expect"];
        let parsed = authorize_json(&payload, corpus, index);

        if !expect["parses"].as_bool().unwrap_or(true) {
            assert!(parsed.is_err(), "{name}: expected a parse rejection");
            continue;
        }
        let response = parsed.unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            response.allowed,
            expect["allowed"].as_bool().expect("allowed"),
            "{name}: allowed mismatch"
        );

        let expected_reason = expect["deniedBecause"].as_str();
        let actual_reason = response.denied_because.map(|reason| {
            serde_json::to_value(reason)
                .expect("reason serializes")
                .as_str()
                .expect("reason is a string")
                .to_string()
        });
        assert_eq!(
            actual_reason.as_deref(),
            expected_reason,
            "{name}: reason mismatch"
        );

        let expected_allowed: Vec<String> = expect["allowedSourceIds"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .map(|v| v.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            response.receipt.allowed_source_ids, expected_allowed,
            "{name}: allowed sources mismatch"
        );

        let expected_denied: Vec<String> = expect["deniedSourceIds"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .map(|v| v.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        let actual_denied: Vec<String> = response
            .receipt
            .denied
            .iter()
            .map(|decision| decision.source_id.clone())
            .collect();
        assert_eq!(
            actual_denied, expected_denied,
            "{name}: denied sources mismatch"
        );
    }
}

#[test]
fn the_parity_set_exercises_every_deny_reason() {
    let raw = include_str!("../fixtures/authority-parity.json");
    let doc: Value = serde_json::from_str(raw).expect("fixtures parse");
    let mut seen = std::collections::BTreeSet::new();
    for case in doc["cases"].as_array().expect("cases") {
        if let Some(reason) = case["expect"]["deniedBecause"].as_str() {
            seen.insert(reason.to_string());
        }
        for value in case["expect"]["deniedSourceIds"]
            .as_array()
            .unwrap_or(&vec![])
        {
            let _ = value;
        }
    }
    // Reasons only reachable per-source are asserted by the unit tests above;
    // these are the action-level ones the fixture set must cover.
    for reason in [
        "unknown_schema",
        "missing_capability",
        "stale_index",
        "bounds",
    ] {
        assert!(seen.contains(reason), "parity set never exercises {reason}");
    }
}

#[test]
fn the_checked_in_schema_matches_the_contracts() {
    let checked_in: Value =
        serde_json::from_str(include_str!("../schema/help-authority.v1.schema.json"))
            .expect("checked-in schema parses");
    // Compared parsed, not byte-wise: key order depends on whether the build
    // graph enables `serde_json/preserve_order`.
    assert_eq!(
        checked_in,
        schema::json_schema(),
        "checked-in schema drifted; regenerate it"
    );
}

#[test]
fn every_contract_object_is_closed_in_the_schema() {
    let schema = schema::json_schema();
    let defs = schema["$defs"].as_object().expect("defs");
    for (name, definition) in defs {
        if definition.get("type").and_then(Value::as_str) == Some("object") {
            assert_eq!(
                definition.get("additionalProperties"),
                Some(&Value::Bool(false)),
                "{name} is open in the schema but closed in Rust"
            );
        }
    }
}
