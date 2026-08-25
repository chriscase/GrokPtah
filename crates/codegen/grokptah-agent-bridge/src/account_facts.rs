//! Desktop-adapter projection of local Grok Build account state.
//!
//! The classification and readiness rules live in
//! [`grokptah_agent_sdk::account`], which has no filesystem, network, or
//! credential dependency. This module only decides *which* local state to
//! hand it, and never copies credential material across the boundary: the
//! resolved [`WireCredentials`] bearer and refresh token are read here only
//! to the extent of choosing a route, never projected.
//!
//! Credential precedence deliberately reuses
//! [`crate::auth_store::resolve_wire_credentials`], the same resolver
//! [`crate::auth_store::load_auth_state`] already calls for the signed-in
//! badge, so the readiness badge can never disagree with the auth badge about
//! which route is active.

use grokptah_agent_sdk::account::{AccountObservation, CredentialSource};

/// Re-exported so the desktop adapter can name the contract without taking a
/// direct dependency on the SDK crate.
pub use grokptah_agent_sdk::account::GrokAccountFacts;

use crate::auth_store::{self, WireCredentials};

/// Method prefix written by the Grok Build session loader.
const GROK_BUILD_METHOD_PREFIX: &str = "grok_build:";

/// Project current account readiness facts at an explicit observation instant.
///
/// `now_unix` is a parameter rather than a wall-clock read so callers and
/// tests share one deterministic definition of "now".
pub fn grok_account_facts(now_unix: i64) -> GrokAccountFacts {
    let Some(credentials) = auth_store::resolve_wire_credentials() else {
        return GrokAccountFacts::absent();
    };
    if credentials.method.starts_with(GROK_BUILD_METHOD_PREFIX) {
        // Re-read the session document directly: `WireCredentials` has already
        // parsed `expires_at` with chrono, which folds an unparseable stamp
        // into `None` and would report "no expiry" where the honest answer is
        // "expiry unreadable". Both are non-blocking, but they are not the
        // same fact, and the UI says so.
        if let Some(document) = read_auth_json() {
            return GrokAccountFacts::from_auth_json(&document, now_unix);
        }
    }
    project_resolved_route(&credentials, env_api_key_present(), now_unix)
}

fn env_api_key_present() -> bool {
    std::env::var("XAI_API_KEY").is_ok_and(|value| !value.is_empty())
}

fn read_auth_json() -> Option<serde_json::Value> {
    let raw = std::fs::read_to_string(auth_store::auth_json_path()).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Map a resolver `method` string onto the closed input vocabulary.
///
/// Unrecognized methods fall through to a Grok Build session observation with
/// no `auth_mode`, which the SDK classifies as
/// [`grokptah_agent_sdk::account::CredentialMethod::Unknown`]
/// rather than inventing a route.
fn classify_source(method: &str, env_api_key_present: bool) -> (CredentialSource, Option<&str>) {
    match method {
        "api_key" if env_api_key_present => (CredentialSource::EnvApiKey, None),
        "api_key" => (CredentialSource::KeychainApiKey, None),
        "token_command" => (CredentialSource::TokenCommand, None),
        "provider_env" => (CredentialSource::ProviderEnv, None),
        "provider_keychain" => (CredentialSource::ProviderKeychain, None),
        other => (
            CredentialSource::GrokBuildSession,
            other.strip_prefix(GROK_BUILD_METHOD_PREFIX).or(Some(other)),
        ),
    }
}

/// Project facts for a route that is not a `~/.grok/auth.json` session.
///
/// Takes only the non-secret fields of the resolved credential. The bearer and
/// refresh token are never read.
fn project_resolved_route(
    credentials: &WireCredentials,
    env_api_key_present: bool,
    now_unix: i64,
) -> GrokAccountFacts {
    let (source, auth_mode) = classify_source(&credentials.method, env_api_key_present);
    let expires_at = credentials.expires_at.map(|instant| instant.to_rfc3339());
    let observation = AccountObservation {
        auth_mode,
        user_id: credentials.user_id.as_deref(),
        principal_id: credentials.principal_id.as_deref(),
        team_id: credentials.team_id.as_deref(),
        expires_at: expires_at.as_deref(),
    };
    GrokAccountFacts::project(source, &observation, now_unix)
}

/// Project account readiness facts for an already-resolved credential.
///
/// [`grok_account_facts`] answers for the *built-in xAI* route, which is the
/// only one the signed-in badge describes. A launch may run against a
/// compatible provider profile instead, and that credential is resolved from
/// the model selection rather than from the xAI profile — so the launch
/// projection resolves it once and hands the result here, instead of asking
/// this module to resolve a second, possibly different, credential.
pub(crate) fn account_facts_for_resolved_route(
    credentials: &WireCredentials,
    now_unix: i64,
) -> GrokAccountFacts {
    if credentials.method.starts_with(GROK_BUILD_METHOD_PREFIX) {
        // Same reason as `grok_account_facts`: chrono has already folded an
        // unparseable stamp into `None`, and "unreadable" is not "absent".
        if let Some(document) = read_auth_json() {
            return GrokAccountFacts::from_auth_json(&document, now_unix);
        }
    }
    project_resolved_route(credentials, env_api_key_present(), now_unix)
}

/// Whether the host should permit a *new* Grok Build launch right now.
pub fn permits_new_launch(now_unix: i64) -> bool {
    grok_account_facts(now_unix).permits_launch()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use grokptah_agent_sdk::account::{AccountReadiness, CredentialMethod, ReadinessReason};

    /// Fixed observation clock: 2026-08-25T00:00:00Z, matching the SDK tests.
    const NOW: i64 = 1_787_616_000;
    const SENTINEL_BEARER: &str = "xai-SENTINEL-BEARER-DO-NOT-LEAK";

    fn credentials(method: &str) -> WireCredentials {
        WireCredentials {
            provider_id: "xai".into(),
            // Present so the test proves it is never projected, not because
            // the projection needs it.
            bearer: SENTINEL_BEARER.into(),
            oidc_token_auth: false,
            display_name: "Operator (operator@example.test)".into(),
            method: method.into(),
            user_id: Some("usr-0a1b2c3d".into()),
            team_id: Some("team-9z8y".into()),
            auth_scope: Some("default".into()),
            refresh_token: Some("xai-SENTINEL-REFRESH-DO-NOT-LEAK".into()),
            oidc_issuer: None,
            oidc_client_id: None,
            principal_type: None,
            principal_id: None,
            expires_at: None,
        }
    }

    #[test]
    fn resolver_methods_map_onto_the_closed_source_vocabulary() {
        assert_eq!(
            classify_source("api_key", true),
            (CredentialSource::EnvApiKey, None)
        );
        assert_eq!(
            classify_source("api_key", false),
            (CredentialSource::KeychainApiKey, None)
        );
        assert_eq!(
            classify_source("token_command", false),
            (CredentialSource::TokenCommand, None)
        );
        assert_eq!(
            classify_source("provider_env", false),
            (CredentialSource::ProviderEnv, None)
        );
        assert_eq!(
            classify_source("provider_keychain", false),
            (CredentialSource::ProviderKeychain, None)
        );
        assert_eq!(
            classify_source("grok_build:oidc", false),
            (CredentialSource::GrokBuildSession, Some("oidc"))
        );
        assert_eq!(
            classify_source("grok_build:api_key", false),
            (CredentialSource::GrokBuildSession, Some("api_key"))
        );
    }

    #[test]
    fn direct_api_key_routes_are_usable_but_claim_no_expiry() {
        let facts = project_resolved_route(&credentials("api_key"), true, NOW);
        assert_eq!(facts.credential_method, CredentialMethod::ApiKey);
        assert_eq!(facts.readiness, AccountReadiness::Unknown);
        assert_eq!(facts.readiness_reason, ReadinessReason::ExpiryNotProvided);
        assert!(facts.permits_launch());
        assert_eq!(facts.validate(), Ok(()));
    }

    #[test]
    fn a_resolved_expiry_round_trips_through_rfc3339_into_the_projection() {
        let mut expiring = credentials("grok_build:oidc");
        expiring.expires_at = Some(chrono::Utc.timestamp_opt(NOW + 3_600, 0).single().unwrap());
        let facts = project_resolved_route(&expiring, false, NOW);
        assert_eq!(facts.credential_method, CredentialMethod::GrokBuildOidc);
        assert_eq!(facts.readiness, AccountReadiness::Usable);
        assert_eq!(facts.expiry.seconds_remaining, Some(3_600));
        assert_eq!(
            facts.expiry.expires_at.as_deref(),
            Some("2026-08-25T01:00:00Z")
        );

        let mut expired = expiring.clone();
        expired.expires_at = Some(chrono::Utc.timestamp_opt(NOW - 1, 0).single().unwrap());
        let facts = project_resolved_route(&expired, false, NOW);
        assert_eq!(facts.readiness, AccountReadiness::Unusable);
        assert_eq!(facts.readiness_reason, ReadinessReason::CredentialExpired);
        assert!(!facts.permits_launch());
    }

    #[test]
    fn an_unrecognized_resolver_method_never_invents_a_route() {
        let facts = project_resolved_route(&credentials("something_new"), false, NOW);
        assert_eq!(facts.credential_method, CredentialMethod::Unknown);
        assert_eq!(facts.readiness, AccountReadiness::Unknown);
        assert!(facts.permits_launch());
    }

    #[test]
    fn the_adapter_never_projects_credential_material() {
        for method in [
            "api_key",
            "token_command",
            "provider_env",
            "provider_keychain",
            "grok_build:oidc",
            "grok_build:api_key",
            "something_new",
        ] {
            let facts = project_resolved_route(&credentials(method), false, NOW);
            let encoded = serde_json::to_string(&facts).expect("facts serialize");
            for needle in [
                SENTINEL_BEARER,
                "SENTINEL-REFRESH",
                "refresh_token",
                "refreshToken",
                "bearer",
                "Bearer",
                "operator@example.test",
                "Operator",
                "auth_scope",
                "provider_id",
            ] {
                assert!(
                    !encoded.contains(needle),
                    "adapter leaked {needle:?} for {method}: {encoded}"
                );
            }
            // Only the durable, opaque account handle survives.
            assert_eq!(
                facts
                    .account_reference
                    .as_ref()
                    .map(|reference| reference.value.as_str()),
                Some("usr-0a1b2c3d")
            );
        }
    }

    #[test]
    fn a_session_document_keeps_full_expiry_fidelity_through_the_sdk() {
        // The adapter's reason for re-reading auth.json: chrono would fold an
        // unparseable stamp into "absent", losing an honest distinction.
        let document = serde_json::json!({
            "default": {
                "key": SENTINEL_BEARER,
                "auth_mode": "oidc",
                "user_id": "usr-0a1b2c3d",
                "expires_at": "not-a-timestamp",
            }
        });
        let facts = GrokAccountFacts::from_auth_json(&document, NOW);
        assert_eq!(facts.readiness_reason, ReadinessReason::ExpiryUnparseable);
        assert!(facts.permits_launch());
    }
}
