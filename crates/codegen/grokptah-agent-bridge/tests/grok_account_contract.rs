//! Contract tests for the Grok Build account + Run attribution surface.
//!
//! Everything here is reached through the crate's public exports only — no
//! `pub(crate)` helper, no internal module path. That is the point: another
//! product (ContextDesk, a headless certification lab) must be able to import
//! the same contract and reproduce the exact bytes the desktop editor renders.

use chrono::{DateTime, Duration, Utc};
use grokptah_agent_bridge::orchestration::{
    public_provider_route_keys_are_allowlisted, public_run_contains_forbidden_fields,
    PublicProviderRouteSummary, PUBLIC_PROVIDER_ROUTE_KEYS,
};
use grokptah_agent_bridge::{
    project_grok_account_status, AuthState, CapabilitySource, EffortLevel, GrokAccountFacts,
    GrokCredentialMethod, GrokSessionState, ProviderDeadlineClass, ProviderDialect, ProviderKind,
    PublicGrokAccountStatus, GROK_ACCOUNT_STATUS_KEYS, GROK_ACCOUNT_STATUS_SCHEMA,
};
use serde_json::{json, Value};

fn fixed_now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-08-25T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn route_summary(credential_method: GrokCredentialMethod) -> PublicProviderRouteSummary {
    PublicProviderRouteSummary {
        provider_id: "xai".into(),
        kind: ProviderKind::Xai,
        dialect: ProviderDialect::XaiChatCompletions,
        model_id: "grok-4.5".into(),
        wire_model_id: "grok-4-5".into(),
        credential_method,
        capability_source: CapabilitySource::Measured,
        qualification_schema: None,
        deadline_class: ProviderDeadlineClass::Standard,
        effort: EffortLevel::Medium,
        snapshot_hash: "v1-sha256:route".into(),
    }
}

fn grok_build_facts() -> GrokAccountFacts {
    GrokAccountFacts {
        provider_id: "xai".into(),
        method: GrokCredentialMethod::GrokBuildOidc,
        display_name: Some("Ada (ada.lovelace@example.com)".into()),
        oidc_issuer: Some("https://issuer.example".into()),
        oidc_client_id: Some("dynamic-client".into()),
        principal_type: Some("user".into()),
        principal_id: Some("principal-9f2".into()),
        user_id: Some("user-771".into()),
        team_id: None,
        expires_at: Some(fixed_now() + Duration::hours(4)),
    }
}

#[test]
fn public_route_summary_carries_credential_attribution_and_stays_allowlisted() {
    let value = serde_json::to_value(route_summary(GrokCredentialMethod::GrokBuildOidc)).unwrap();

    assert_eq!(value["credentialMethod"], "grok_build_oidc");
    assert!(
        PUBLIC_PROVIDER_ROUTE_KEYS.contains(&"credentialMethod"),
        "the new field must be on the exact route allowlist"
    );
    assert!(
        public_provider_route_keys_are_allowlisted(&value),
        "route keys must stay exact-allowlisted: {value}"
    );
    assert!(
        !public_run_contains_forbidden_fields(&value),
        "attribution must not reintroduce a forbidden key: {value}"
    );

    // Every serialized key is on the allowlist, and the allowlist has no key
    // the struct does not emit.
    let object = value.as_object().unwrap();
    for key in object.keys() {
        assert!(PUBLIC_PROVIDER_ROUTE_KEYS.contains(&key.as_str()), "{key}");
    }
}

#[test]
fn route_attribution_reports_the_credential_class_without_the_reference() {
    // Exactly the references `resolve_wire_credentials_for_route` admits.
    let cases = [
        ("managed:xai:oidc", GrokCredentialMethod::GrokBuildOidc),
        ("managed:xai:api-key", GrokCredentialMethod::XaiApiKey),
        ("env:GROKPTAH_API_KEY", GrokCredentialMethod::GatewayManaged),
        ("env:OPENAI_API_KEY", GrokCredentialMethod::GatewayManaged),
        (
            "keychain:provider/acme-corp/api-key",
            GrokCredentialMethod::GatewayApiKey,
        ),
        // Fails closed rather than guessing.
        ("inline:pasted-secret", GrokCredentialMethod::Unknown),
        ("env:XAI_API_KEY", GrokCredentialMethod::Unknown),
    ];

    for (reference, expected) in cases {
        let method = GrokCredentialMethod::from_credential_ref(reference);
        assert_eq!(method, expected, "{reference}");

        let encoded = serde_json::to_string(&route_summary(method)).unwrap();
        // The reference itself, and any profile name inside it, stay off the wire.
        assert!(
            !encoded.contains(reference),
            "{reference} leaked: {encoded}"
        );
        assert!(!encoded.contains("acme-corp"), "profile name leaked");
        assert!(!encoded.to_ascii_lowercase().contains("fingerprint"));
    }
}

#[test]
fn legacy_route_receipts_decode_as_unknown_attribution() {
    // A receipt written before this field existed must still decode, and must
    // not be reported as a Grok Build run.
    let legacy = json!({
        "providerId": "xai",
        "kind": "xai",
        "dialect": "xai_chat_completions",
        "modelId": "grok-4.5",
        "wireModelId": "grok-4-5",
        "capabilitySource": "measured",
        "deadlineClass": "standard",
        "effort": "medium",
        "snapshotHash": "v1-sha256:route",
    });

    let decoded: PublicProviderRouteSummary = serde_json::from_value(legacy).unwrap();
    assert_eq!(decoded.credential_method, GrokCredentialMethod::Unknown);
    assert!(!decoded.credential_method.is_grok_build_session());
}

#[test]
fn route_summary_still_rejects_unknown_fields() {
    let mut leaky = serde_json::to_value(route_summary(GrokCredentialMethod::XaiApiKey)).unwrap();
    leaky["credentialRef"] = Value::String("managed:xai:api-key".into());

    assert!(
        serde_json::from_value::<PublicProviderRouteSummary>(leaky.clone()).is_err(),
        "deny_unknown_fields must still hold"
    );
    assert!(public_run_contains_forbidden_fields(&leaky));
    assert!(!public_provider_route_keys_are_allowlisted(&leaky));
}

#[test]
fn account_status_is_projectable_and_allowlisted_from_the_public_surface() {
    let status = project_grok_account_status(&grok_build_facts(), fixed_now());
    let value = serde_json::to_value(&status).unwrap();

    assert_eq!(value["schema"], GROK_ACCOUNT_STATUS_SCHEMA);
    assert_eq!(value["method"], "grok_build_oidc");
    assert_eq!(value["session"], "active");
    assert_eq!(value["usable"], true);
    assert!(status.grok_build_session_ready());

    for key in value.as_object().unwrap().keys() {
        assert!(
            GROK_ACCOUNT_STATUS_KEYS.contains(&key.as_str()),
            "{key} is not on the account allowlist"
        );
    }
}

#[test]
fn account_status_never_carries_credential_material_or_host_paths() {
    let encoded = serde_json::to_string(&project_grok_account_status(
        &grok_build_facts(),
        fixed_now(),
    ))
    .unwrap();
    let lowered = encoded.to_ascii_lowercase();

    for needle in [
        "bearer",
        "refresh",
        "token",
        "apikey",
        "api_key",
        "secret",
        "password",
        "authorization",
        "auth.json",
        "/home/",
        "/users/",
        // Directory form: the bare suffix also matches the schema string.
        "/.grok",
    ] {
        assert!(!lowered.contains(needle), "{needle} leaked into {encoded}");
    }
    // Address is masked; the human name is dropped entirely.
    assert!(!encoded.contains("ada.lovelace@example.com"));
    assert!(!encoded.contains("Ada"));
    assert!(encoded.contains("a…@example.com"));
}

#[test]
fn expired_grok_build_session_fails_the_run_gate_closed() {
    let mut facts = grok_build_facts();
    facts.expires_at = Some(fixed_now() - Duration::minutes(1));
    let status = project_grok_account_status(&facts, fixed_now());

    assert_eq!(status.session, GrokSessionState::Expired);
    assert!(!status.usable);
    assert!(!status.grok_build_session_ready());

    // The editor payload keeps `signed_in` for older clients while the account
    // view reports the truth a run gate must read.
    let auth = AuthState {
        signed_in: true,
        display_name: Some("Ada (ada.lovelace@example.com)".into()),
        method: Some("grok_build:oidc".into()),
        account: Some(status),
    };
    let value = serde_json::to_value(&auth).unwrap();
    assert_eq!(value["signed_in"], true);
    assert_eq!(value["account"]["session"], "expired");
    assert_eq!(value["account"]["usable"], false);
}

#[test]
fn expiring_grok_build_session_is_visible_before_a_run_dies_mid_flight() {
    let mut facts = grok_build_facts();
    facts.expires_at = Some(fixed_now() + Duration::minutes(5));
    let status = project_grok_account_status(&facts, fixed_now());

    assert_eq!(status.session, GrokSessionState::Expiring);
    // Still usable — the editor warns rather than blocks.
    assert!(status.usable);
    assert_eq!(status.expires_in_seconds, Some(300));
}

#[test]
fn auth_state_from_an_older_build_decodes_without_an_account() {
    let legacy = json!({
        "signed_in": true,
        "display_name": "Ada",
        "method": "grok_build:oidc",
    });

    let decoded: AuthState = serde_json::from_value(legacy).unwrap();
    assert!(decoded.signed_in);
    assert!(decoded.account.is_none());

    // And a payload that carries one round-trips unchanged.
    let with_account = AuthState {
        account: Some(project_grok_account_status(
            &grok_build_facts(),
            fixed_now(),
        )),
        ..decoded
    };
    let encoded = serde_json::to_string(&with_account).unwrap();
    let round_tripped: AuthState = serde_json::from_str(&encoded).unwrap();
    assert_eq!(
        round_tripped.account.map(|a| a.session),
        Some(GrokSessionState::Active)
    );
}

#[test]
fn account_status_is_deterministic_for_a_fixed_clock() {
    // Headless consumers replay the same facts and must get identical bytes.
    let facts = grok_build_facts();
    let first = serde_json::to_string(&project_grok_account_status(&facts, fixed_now())).unwrap();
    let second = serde_json::to_string(&project_grok_account_status(&facts, fixed_now())).unwrap();
    assert_eq!(first, second);

    let absent = serde_json::to_value(PublicGrokAccountStatus::absent()).unwrap();
    assert_eq!(absent["session"], "absent");
    assert_eq!(absent["usable"], false);
    assert_eq!(absent["method"], "unknown");
}
