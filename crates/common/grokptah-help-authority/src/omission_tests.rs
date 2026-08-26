//! What happens when a field is simply not there.
//!
//! Every check in this crate is a check on a value. The interesting question
//! is what the value is when a caller does not send one — because "absent"
//! is the cheapest possible attack, and a default that means "allowed" turns
//! a default-deny boundary into allow-by-omission.

use crate::grant::*;
use crate::manifest::*;
use crate::*;

const CORPUS: &str = "sha256:corpus";
const INDEX: &str = "sha256:index";

fn key() -> GrantMintingKey {
    GrantMintingKey::new(vec![4u8; 32]).expect("key")
}

// ------------------------------------------------------------- the request

#[test]
fn a_request_with_no_capabilities_field_is_denied() {
    // `capabilities` is `#[serde(default)]`, so it parses. What it must not do
    // is parse into something permissive.
    let raw = r#"{
        "schema": "grokptah.help-authority-request.v1",
        "action": "search",
        "principal": { "principal_id": "alice", "tenant_id": "tenant-a" },
        "corpus_digest": "sha256:corpus",
        "index_digest": "sha256:index"
    }"#;
    let request: DecisionRequest = serde_json::from_str(raw).expect("parses");
    assert!(request.principal.capabilities.is_empty());
    let response = authorize(&request, CORPUS, INDEX);
    assert!(!response.allowed);
    assert_eq!(response.denied_because, Some(DenyReason::MissingCapability));
}

#[test]
fn a_request_with_no_project_membership_cannot_reach_a_project_source() {
    let raw = r#"{
        "schema": "grokptah.help-authority-request.v1",
        "action": "search",
        "principal": {
            "principal_id": "alice",
            "tenant_id": "tenant-a",
            "capabilities": ["help_search", "help_search_project"]
        },
        "corpus_digest": "sha256:corpus",
        "index_digest": "sha256:index",
        "sources": [{
            "source_id": "s1",
            "visibility": "project",
            "tenant_id": "tenant-a",
            "project_id": "proj-1",
            "digest": "sha256:d"
        }]
    }"#;
    let request: DecisionRequest = serde_json::from_str(raw).expect("parses");
    // Holding the capability is not the same as being in the project. An
    // omitted membership list must not read as "all projects".
    assert!(request.principal.project_ids.is_empty());
    let response = authorize(&request, CORPUS, INDEX);
    assert!(
        response
            .receipt
            .denied
            .iter()
            .any(|decision| decision.source_id == "s1"),
        "a project source must be denied to a principal with no membership"
    );
    assert!(response.receipt.allowed_source_ids.is_empty());
}

#[test]
fn a_scoped_source_that_omits_its_scope_is_denied_rather_than_treated_as_public() {
    for (label, raw) in [
        (
            "project source with no project_id",
            r#"{"source_id":"s1","visibility":"project","tenant_id":"tenant-a","digest":"sha256:d"}"#,
        ),
        (
            "private source with no owner",
            r#"{"source_id":"s1","visibility":"private","tenant_id":"tenant-a","digest":"sha256:d"}"#,
        ),
    ] {
        let descriptor: SourceDescriptor = serde_json::from_str(raw).expect("parses");
        let request = DecisionRequest {
            schema: HELP_DECISION_REQUEST_SCHEMA.into(),
            action: Action::Search,
            principal: Principal {
                principal_id: "alice".into(),
                tenant_id: "tenant-a".into(),
                project_ids: vec!["proj-1".into()],
                capabilities: vec![
                    Capability::HelpSearch,
                    Capability::HelpSearchProject,
                    Capability::HelpSearchPrivate,
                ],
            },
            corpus_digest: CORPUS.into(),
            index_digest: INDEX.into(),
            sources: vec![descriptor],
        };
        let response = authorize(&request, CORPUS, INDEX);
        assert!(
            response.receipt.allowed_source_ids.is_empty(),
            "{label} must not be surfaced"
        );
        assert_eq!(
            response
                .receipt
                .denied
                .first()
                .and_then(|d| d.denied_because),
            Some(DenyReason::MalformedScope),
            "{label}"
        );
    }
}

#[test]
fn an_omitted_visibility_is_a_parse_failure_not_a_public_source() {
    // The most valuable field to leave out. It has no default at all, so the
    // document does not parse rather than parsing as `public`.
    let raw = r#"{"source_id":"s1","tenant_id":"tenant-a","digest":"sha256:d"}"#;
    assert!(serde_json::from_str::<SourceDescriptor>(raw).is_err());
}

#[test]
fn an_omitted_action_or_principal_is_a_parse_failure() {
    for raw in [
        r#"{"schema":"grokptah.help-authority-request.v1","principal":{"principal_id":"a","tenant_id":"t"},"corpus_digest":"c","index_digest":"i"}"#,
        r#"{"schema":"grokptah.help-authority-request.v1","action":"search","corpus_digest":"c","index_digest":"i"}"#,
    ] {
        assert!(authorize_json(raw, CORPUS, INDEX).is_err(), "{raw}");
    }
}

// ------------------------------------------------------------- the manifest

#[test]
fn a_manifest_entry_that_omits_its_visibility_does_not_parse() {
    let raw = r#"[{"sourceId":"s1","path":"p","heading":"h","tenantId":"t","digest":"d"}]"#;
    assert!(matches!(
        SourceManifest::from_json(raw),
        Err(ManifestError::Malformed(_))
    ));
}

#[test]
fn a_descriptor_that_omits_its_scope_is_not_the_manifests_record() {
    let entry = ManifestEntry {
        source_id: "s1".into(),
        path: "docs/A.md".into(),
        heading: "Lifecycle".into(),
        visibility: Visibility::Project,
        tenant_id: "tenant-a".into(),
        project_id: Some("proj-1".into()),
        owner_principal_id: None,
        digest: source_digest(
            "s1",
            "docs/A.md",
            "Lifecycle",
            Visibility::Project,
            "tenant-a",
            Some("proj-1"),
            None,
            "body",
        ),
    };
    let manifest = SourceManifest::from_entries(vec![entry]).expect("builds");
    let genuine = manifest.describe(&["s1".to_string()]);

    let mut unscoped = genuine[0].clone();
    unscoped.project_id = None;
    assert!(
        manifest.enforce_descriptor(&unscoped).is_err(),
        "dropping the project scope must not produce a record the manifest accepts"
    );
}

// --------------------------------------------------------------- the grant

#[test]
fn a_grant_with_no_capabilities_reaches_nothing_beyond_public() {
    let manifest = SourceManifest::from_entries(vec![ManifestEntry {
        source_id: "pub-1".into(),
        path: "docs/A.md".into(),
        heading: "Lifecycle".into(),
        visibility: Visibility::Public,
        tenant_id: "tenant-a".into(),
        project_id: None,
        owner_principal_id: None,
        digest: source_digest(
            "pub-1",
            "docs/A.md",
            "Lifecycle",
            Visibility::Public,
            "tenant-a",
            None,
            None,
            "body",
        ),
    }])
    .expect("builds");

    let served = ServedManifest {
        corpus_digest: CORPUS.into(),
        index_digest: INDEX.into(),
        manifest_digest: manifest.manifest_digest().to_string(),
    };
    let principal = AuthenticatedPrincipal {
        principal_id: "alice".into(),
        tenant_id: "tenant-a".into(),
        project_ids: Vec::new(),
        capabilities: Vec::new(),
        policy_revision: "policy-1".into(),
    };
    let grant = mint_grant(
        &key(),
        &principal,
        &served,
        Action::Search,
        Visibility::Private,
        1,
        1_000,
        60_000,
    )
    .expect("mints");
    let acceptance = GrantAcceptance {
        action: Action::Search,
        manifest: served,
        policy_revision: "policy-1".into(),
        current_revision: 1,
        now_ms: 1_000,
    };

    let response = authorize_against_manifest(
        &key(),
        &grant,
        &acceptance,
        &manifest,
        &["pub-1".to_string()],
    );
    // The grant's visibility cap is `Private`, the widest there is. It still
    // reaches nothing, because a cap is a ceiling and not a grant.
    assert!(!response.allowed);
    assert_eq!(response.denied_because, Some(DenyReason::MissingCapability));
}
