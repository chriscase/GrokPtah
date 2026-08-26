//! The Rust contracts and their published JSON Schemas are strict peers.
//!
//! Every in-crate test checks the Rust types against *our reading* of the
//! schema. That is exactly the check that cannot catch a drift, because the
//! same reading wrote both. So these tests run a real Draft 2020-12 validator
//! over three things:
//!
//! 1. the golden **accepted** fixtures, which must validate;
//! 2. the golden **rejected** fixtures, which must be refused by the schema,
//!    the strict Rust decoder, or the Rust validator — sailing past all three
//!    is the only forbidden outcome;
//! 3. **real producer payloads**, serialized from the Rust types themselves,
//!    which must validate. A projection the schema would reject is a published
//!    contract nobody downstream can parse, and no fixture can catch that.
//!
//! A hand-rolled keyword check would only prove the schema agrees with our
//! reading of it. `jsonschema` is a third opinion.

use grokptah_agent_sdk::account::{
    AccountObservation, AccountReference, AccountReferenceSource, CredentialMethod,
    CredentialSource, GrokAccountFacts,
};
use grokptah_agent_sdk::attempt::{
    AttemptIntent, AttemptRoute, AttemptSubject, AuthorityRevisions, BoundedId, ProviderAttempt,
    ProviderReceipts, Revision, SendState, UsageReceipt,
};
use grokptah_agent_sdk::launch::{
    BaseCategory, CapabilityFacts, CapabilityProvenance, GrokLaunchTruth, LaunchObservation,
    LaunchReason, ModelFacts, ModelReference, ProviderClass, Refreshability, RequestDialect,
    RouteClass,
};
use jsonschema::{Draft, Validator};
use serde_json::Value;

/// Fixed observation clock: 2026-08-25T00:00:00Z, matching every other suite.
const NOW: i64 = 1_787_616_000;

const LAUNCH_SCHEMA: &str = include_str!("../../../../docs/schemas/grokptah-launch.v1.schema.json");
const LAUNCH_FIXTURES: &str =
    include_str!("../../../../docs/schemas/grokptah-launch.v1.fixtures.json");
const ACCOUNT_SCHEMA: &str =
    include_str!("../../../../docs/schemas/grokptah-account.v1.schema.json");
const ACCOUNT_FIXTURES: &str =
    include_str!("../../../../docs/schemas/grokptah-account.v1.fixtures.json");
const ATTEMPT_SCHEMA: &str =
    include_str!("../../../../docs/schemas/grokptah-attempt.v1.schema.json");

fn compiled(raw: &str) -> Validator {
    let schema: Value = serde_json::from_str(raw).expect("schema parses");
    // Pinning the dialect explicitly: a schema that silently fell back to an
    // older draft would quietly stop enforcing some of what it declares.
    assert_eq!(
        schema["$schema"], "https://json-schema.org/draft/2020-12/schema",
        "the schema does not declare Draft 2020-12"
    );
    jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(&schema)
        .expect("schema compiles under a real Draft 2020-12 validator")
}

fn errors(validator: &Validator, instance: &Value) -> Vec<String> {
    validator
        .iter_errors(instance)
        .map(|error| format!("{} at {}", error, error.instance_path))
        .collect()
}

fn session_account() -> GrokAccountFacts {
    GrokAccountFacts::project(
        CredentialSource::GrokBuildSession,
        &AccountObservation {
            auth_mode: Some("oidc"),
            user_id: Some("usr-0a1b2c3d"),
            principal_id: None,
            team_id: None,
            expires_at: Some("2026-08-25T12:30:00Z"),
        },
        NOW,
    )
}

fn probed() -> CapabilityFacts {
    CapabilityFacts {
        provenance: CapabilityProvenance::Declared,
        chat: true,
        tools: true,
        stream: true,
        parallel_tool_calls: true,
        image_input: false,
    }
}

fn ready_observation(account: &GrokAccountFacts) -> LaunchObservation<'_> {
    LaunchObservation {
        provider: ProviderClass::Xai,
        route: RouteClass::XaiFirstParty,
        base: BaseCategory::XaiOfficial,
        dialect: RequestDialect::XaiChatCompletions,
        refreshability: Refreshability::Refreshable,
        model: ModelFacts::selected(ModelReference::new("grok-4").expect("bounded model")),
        capabilities: probed(),
        account,
    }
}

fn ready_truth() -> GrokLaunchTruth {
    GrokLaunchTruth::project(&ready_observation(&session_account()))
}

/// Real producer payloads, one per shape the projection can actually emit.
fn launch_producer_payloads() -> Vec<(&'static str, GrokLaunchTruth)> {
    let account = session_account();
    let api_key = GrokAccountFacts::project(
        CredentialSource::EnvApiKey,
        &AccountObservation::default(),
        NOW,
    );
    let mut payloads = vec![
        (
            "ready session",
            GrokLaunchTruth::project(&ready_observation(&account)),
        ),
        ("unresolved host", GrokLaunchTruth::unresolved()),
        (
            "api key without expiry claim",
            GrokLaunchTruth::project(&LaunchObservation {
                refreshability: Refreshability::NotRefreshable,
                capabilities: CapabilityFacts {
                    provenance: CapabilityProvenance::Measured,
                    ..probed()
                },
                account: &api_key,
                ..ready_observation(&account)
            }),
        ),
    ];
    // Every refusing shape the adapter can produce, so no blocked projection
    // is publishable in Rust but unparseable downstream.
    for (label, observation) in [
        (
            "unprobed capabilities",
            LaunchObservation {
                capabilities: CapabilityFacts::unprobed(),
                ..ready_observation(&account)
            },
        ),
        (
            "model not selected",
            LaunchObservation {
                model: ModelFacts::not_selected(),
                capabilities: CapabilityFacts::unprobed(),
                base: BaseCategory::Unset,
                dialect: RequestDialect::Unrecognized,
                ..ready_observation(&account)
            },
        ),
        (
            "insecure base",
            LaunchObservation {
                base: BaseCategory::InsecureTransport,
                ..ready_observation(&account)
            },
        ),
        (
            "unknown refreshability",
            LaunchObservation {
                refreshability: Refreshability::Unknown,
                ..ready_observation(&account)
            },
        ),
        (
            "unrecognized credential route",
            LaunchObservation {
                account: &GrokAccountFacts::project(
                    CredentialSource::GrokBuildSession,
                    &AccountObservation {
                        auth_mode: Some("something-new"),
                        user_id: Some("usr-0a1b2c3d"),
                        principal_id: None,
                        team_id: None,
                        expires_at: Some("2026-08-25T12:30:00Z"),
                    },
                    NOW,
                ),
                ..ready_observation(&account)
            },
        ),
    ] {
        payloads.push((label, GrokLaunchTruth::project(&observation)));
    }
    payloads
}

fn bounded(value: &str) -> BoundedId {
    BoundedId::new(value).unwrap_or_else(|| panic!("{value:?} should be bounded"))
}

fn attempt_producer_payloads() -> Vec<(&'static str, ProviderAttempt)> {
    let open = ProviderAttempt::open(
        bounded("att-0001"),
        bounded("run-0001"),
        1,
        AttemptSubject {
            principal: Some(bounded("prn-0a1b2c3d")),
            tenant: Some(bounded("tnt-9z8y")),
            project: Some(bounded("prj-alpha")),
            workspace: bounded("wsp:0a1b2c3d"),
            session: bounded("ses:4e5f6a7b"),
        },
        AuthorityRevisions {
            auth: Revision(7),
            policy: Revision(3),
            capability: Revision(11),
            credential: Revision(2),
        },
        AttemptRoute {
            provider: ProviderClass::Xai,
            profile: Some(bounded("xai")),
            credential_method: CredentialMethod::GrokBuildOidc,
            route: RouteClass::XaiFirstParty,
            base: BaseCategory::XaiOfficial,
            dialect: RequestDialect::XaiChatCompletions,
            model: ModelReference::new("grok-4").expect("bounded model"),
            effort: Some(bounded("high")),
            account_reference: AccountReference::new(
                "usr-0a1b2c3d",
                AccountReferenceSource::UserId,
            ),
        },
        AttemptIntent {
            digest: bounded("sha256:0a1b2c3d"),
            request_id: bounded("req-0001"),
            provider_idempotency_key: bounded("idem:0a1b2c3d"),
        },
    );

    let mut sending = open.clone();
    sending
        .advance(SendState::Sending)
        .expect("dispatch begins");

    let mut sent = sending.clone();
    sent.receipts = ProviderReceipts {
        request: Some(bounded("prq-abc123")),
        run: Some(bounded("prn-def456")),
        usage: Some(UsageReceipt {
            input_tokens: 1_200,
            output_tokens: 340,
        }),
        provider_replied: true,
    };
    sent.advance(SendState::Sent)
        .expect("provider acknowledged");

    let mut uncertain = sending.clone();
    uncertain
        .advance(SendState::Uncertain)
        .expect("the outcome is unknown");
    uncertain.failure = Some(grokptah_agent_sdk::outcome::RunFailureKind::TransportError);

    // A minimal attempt: an API-key route publishes no principal, no tenant,
    // no project, no profile, and no effort.
    let minimal = ProviderAttempt::open(
        bounded("att-0002"),
        bounded("run-0002"),
        1,
        AttemptSubject {
            principal: None,
            tenant: None,
            project: None,
            workspace: bounded("wsp:0a1b2c3d"),
            session: bounded("ses:4e5f6a7b"),
        },
        AuthorityRevisions {
            auth: Revision(0),
            policy: Revision(0),
            capability: Revision(0),
            credential: Revision(0),
        },
        AttemptRoute {
            provider: ProviderClass::OpenAiCompatible,
            profile: None,
            credential_method: CredentialMethod::ProviderEnv,
            route: RouteClass::CompatibleProvider,
            base: BaseCategory::CompatibleLoopback,
            dialect: RequestDialect::OpenAiChatCompletions,
            model: ModelReference::new("local/mixtral-8x7b").expect("bounded model"),
            effort: None,
            account_reference: None,
        },
        AttemptIntent {
            digest: bounded("sha256:9f8e7d6c"),
            request_id: bounded("req-0002"),
            provider_idempotency_key: bounded("idem:9f8e7d6c"),
        },
    );

    vec![
        ("known not sent", open),
        ("sending", sending),
        ("sent", sent),
        ("uncertain", uncertain),
        ("minimal api-key route", minimal),
    ]
}

#[test]
fn launch_golden_fixtures_agree_with_a_real_draft_2020_12_validator() {
    let validator = compiled(LAUNCH_SCHEMA);
    let fixtures: Value = serde_json::from_str(LAUNCH_FIXTURES).expect("fixtures parse");
    assert_eq!(fixtures["observedAtUnix"].as_i64(), Some(NOW));

    for case in fixtures["accepted"].as_array().expect("accepted cases") {
        let name = case["name"].as_str().expect("case is named");
        let found = errors(&validator, &case["truth"]);
        assert!(
            found.is_empty(),
            "accepted fixture {name} is rejected by its own schema: {found:?}"
        );
    }

    // A rejected fixture must be refused by at least one strict reader.
    for case in fixtures["rejected"].as_array().expect("rejected cases") {
        let name = case["name"].as_str().expect("case is named");
        let instance = &case["truth"];
        let schema_refused = !errors(&validator, instance).is_empty();
        let rust_refused = match serde_json::from_value::<GrokLaunchTruth>(instance.clone()) {
            Err(_) => true,
            Ok(truth) => truth.validate().is_err(),
        };
        assert!(
            schema_refused || rust_refused,
            "rejected fixture {name} sailed past both the schema and the Rust contract"
        );
    }
}

#[test]
fn account_golden_fixtures_agree_with_a_real_draft_2020_12_validator() {
    let validator = compiled(ACCOUNT_SCHEMA);
    let fixtures: Value = serde_json::from_str(ACCOUNT_FIXTURES).expect("fixtures parse");
    for case in fixtures["accepted"].as_array().expect("accepted cases") {
        let name = case["name"].as_str().expect("case is named");
        let found = errors(&validator, &case["facts"]);
        assert!(
            found.is_empty(),
            "accepted fixture {name} is rejected by its own schema: {found:?}"
        );
    }
    for case in fixtures["rejected"].as_array().expect("rejected cases") {
        let name = case["name"].as_str().expect("case is named");
        let instance = &case["facts"];
        let schema_refused = !errors(&validator, instance).is_empty();
        let rust_refused = match serde_json::from_value::<GrokAccountFacts>(instance.clone()) {
            Err(_) => true,
            Ok(facts) => facts.validate().is_err(),
        };
        assert!(
            schema_refused || rust_refused,
            "rejected fixture {name} sailed past both the schema and the Rust contract"
        );
    }
}

/// The check no fixture can make: what the producer *actually emits* must be
/// parseable by anyone holding only the published schema.
#[test]
fn real_launch_producer_payloads_validate_against_the_published_schema() {
    let validator = compiled(LAUNCH_SCHEMA);
    let payloads = launch_producer_payloads();
    assert!(payloads.len() >= 7, "producer coverage shrank");
    for (label, truth) in payloads {
        assert_eq!(truth.validate(), Ok(()), "{label} fails its own validator");
        let published = serde_json::to_value(&truth).expect("truth serializes");
        let found = errors(&validator, &published);
        assert!(
            found.is_empty(),
            "the producer emits a {label} projection the schema rejects: {found:?}\n{published}"
        );
    }
}

#[test]
fn real_attempt_producer_payloads_validate_against_the_published_schema() {
    let validator = compiled(ATTEMPT_SCHEMA);
    let payloads = attempt_producer_payloads();
    assert!(payloads.len() >= 5, "producer coverage shrank");
    for (label, attempt) in payloads {
        assert_eq!(
            attempt.validate(),
            Ok(()),
            "{label} fails its own validator"
        );
        let published = serde_json::to_value(&attempt).expect("attempt serializes");
        let found = errors(&validator, &published);
        assert!(
            found.is_empty(),
            "the producer emits a {label} attempt the schema rejects: {found:?}\n{published}"
        );
    }
}

/// The schema must be strict in the same direction the Rust decoder is: an
/// added property is not a valid projection, whatever it claims to be.
#[test]
fn the_schemas_refuse_added_properties_exactly_as_the_rust_decoder_does() {
    for (label, raw, mut instance) in [
        (
            "launch",
            LAUNCH_SCHEMA,
            serde_json::to_value(ready_truth()).expect("truth serializes"),
        ),
        (
            "attempt",
            ATTEMPT_SCHEMA,
            serde_json::to_value(&attempt_producer_payloads()[0].1).expect("attempt serializes"),
        ),
    ] {
        let validator = compiled(raw);
        assert!(
            errors(&validator, &instance).is_empty(),
            "{label} baseline does not validate"
        );
        for smuggled in ["balanceUsd", "quotaRemaining", "bearer", "baseUrl"] {
            instance[smuggled] = serde_json::json!("smuggled");
            assert!(
                !errors(&validator, &instance).is_empty(),
                "the {label} schema accepted an added {smuggled:?} property"
            );
            instance
                .as_object_mut()
                .expect("instance is an object")
                .remove(smuggled);
        }
    }
}

/// Enum drift is the failure this pairing exists to catch: a variant added on
/// one side and not the other.
#[test]
fn every_closed_vocabulary_is_a_peer_in_both_directions() {
    let validator = compiled(LAUNCH_SCHEMA);
    let baseline = serde_json::to_value(ready_truth()).expect("truth serializes");

    let probe = |field: &str, values: Vec<&'static str>| {
        for value in values {
            let mut instance = baseline.clone();
            instance[field] = serde_json::json!(value);
            let found = errors(&validator, &instance);
            // Only this field is under test; other keywords may object to the
            // deliberately inconsistent combination, and that is fine.
            assert!(
                !found
                    .iter()
                    .any(|error| error.contains(&format!("/{field}"))),
                "the schema rejects the Rust {field} value {value:?}: {found:?}"
            );
        }
    };
    probe(
        "reason",
        LaunchReason::ALL.iter().map(|v| v.as_str()).collect(),
    );
    probe(
        "base",
        BaseCategory::ALL.iter().map(|v| v.as_str()).collect(),
    );
    probe(
        "credentialMethod",
        CredentialMethod::ALL.iter().map(|v| v.as_str()).collect(),
    );
    probe(
        "provider",
        ProviderClass::ALL.iter().map(|v| v.as_str()).collect(),
    );
    probe(
        "route",
        RouteClass::ALL.iter().map(|v| v.as_str()).collect(),
    );
    probe(
        "dialect",
        RequestDialect::ALL.iter().map(|v| v.as_str()).collect(),
    );
    probe(
        "refreshability",
        Refreshability::ALL.iter().map(|v| v.as_str()).collect(),
    );

    let attempt_validator = compiled(ATTEMPT_SCHEMA);
    let mut attempt =
        serde_json::to_value(&attempt_producer_payloads()[0].1).expect("attempt serializes");
    for state in SendState::ALL {
        attempt["sendState"] = serde_json::json!(state.as_str());
        let found = errors(&attempt_validator, &attempt);
        assert!(
            !found.iter().any(|error| error.contains("/sendState")),
            "the schema rejects the Rust send state {:?}",
            state.as_str()
        );
    }
    // And the reverse direction: a value the Rust type cannot produce must be
    // refused by the schema too, or the schema is the looser peer.
    attempt["sendState"] = serde_json::json!("definitely_sent_probably");
    assert!(
        !errors(&attempt_validator, &attempt).is_empty(),
        "the schema accepted a send state that does not exist in Rust"
    );
    assert!(
        serde_json::from_value::<ProviderAttempt>(attempt).is_err(),
        "the Rust decoder accepted a send state that does not exist"
    );
}

/// The bounded-identifier pattern must refuse in the schema exactly what
/// [`BoundedId::new`] refuses in Rust.
#[test]
fn bounded_identifier_bounds_are_the_same_on_both_sides() {
    let validator = compiled(ATTEMPT_SCHEMA);
    let baseline =
        serde_json::to_value(&attempt_producer_payloads()[0].1).expect("attempt serializes");
    for candidate in [
        "run-0001",
        "sha256:0a1b",
        "openai/gpt-4o-mini",
        "",
        "   ",
        "has space",
        "has\nnewline",
        "<script>",
        "../../etc/passwd",
        "/leading",
        "trailing/",
        ".dotfile",
        "a..b",
        "semi;colon",
        "a",
        "ab",
        &"a".repeat(128),
        &"a".repeat(129),
    ] {
        let rust_accepts = BoundedId::new(candidate).is_some();
        let mut instance = baseline.clone();
        instance["runId"] = serde_json::json!(candidate);
        let schema_accepts = errors(&validator, &instance).is_empty();
        assert_eq!(
            rust_accepts, schema_accepts,
            "Rust and the schema disagree about the bounded id {candidate:?} \
             (rust accepts: {rust_accepts}, schema accepts: {schema_accepts})"
        );
    }
}

/// The same peering for model ids, which have their own bounds.
#[test]
fn model_reference_bounds_are_the_same_on_both_sides() {
    let validator = compiled(LAUNCH_SCHEMA);
    let baseline = serde_json::to_value(ready_truth()).expect("truth serializes");
    for candidate in [
        "grok-4",
        "openai/gpt-4o-mini",
        "a",
        "",
        "   ",
        "grok 4",
        "grok\n4",
        "grok-4 <script>",
        "../../etc/passwd",
        "/grok-4",
        "grok-4/",
        ".grok-4",
        "grok-4:",
        "openai/../secret",
        &"a".repeat(128),
        &"a".repeat(129),
    ] {
        let rust_accepts = ModelReference::new(candidate).is_some();
        let mut instance = baseline.clone();
        instance["model"]["selected"]["value"] = serde_json::json!(candidate);
        let schema_accepts = errors(&validator, &instance).is_empty();
        assert_eq!(
            rust_accepts, schema_accepts,
            "Rust and the schema disagree about the model id {candidate:?} \
             (rust accepts: {rust_accepts}, schema accepts: {schema_accepts})"
        );
    }
}
