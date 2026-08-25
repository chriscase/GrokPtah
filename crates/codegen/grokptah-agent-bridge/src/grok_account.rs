//! Secret-free Grok Build account status and Run credential attribution.
//!
//! The editor is driven by the user's existing Grok Build / OIDC session in
//! `~/.grok/auth.json` (the same file as the official `grok` CLI). Until this
//! module existed, that session reached the UI as
//! [`crate::types::AuthState`] — `signed_in` plus two free-form strings. That
//! surface cannot answer the three questions a Codex-like editor has to answer
//! before it spends a token:
//!
//! 1. *Which* credential is this run about to use — the Grok Build session, a
//!    pasted xAI API key, or a corporate gateway key?
//! 2. Is that session still valid, and how long do I have?
//! 3. Which account does the resulting usage belong to?
//!
//! This module answers them with a bounded, allowlisted projection.
//!
//! # Invariants
//!
//! * **No secrets.** [`GrokAccountFacts`] is the only input type, and it has no
//!   field that can hold a bearer token, a refresh token, or an API key. The
//!   projection is therefore structurally incapable of serializing credential
//!   material — it is not a runtime filter that a later edit can forget.
//! * **No raw host paths.** Nothing here reports where `auth.json` lives.
//! * **No raw account address.** [`mask_account_email`] keeps the domain and
//!   the first local-part character; a display name that carries a human name
//!   alongside the address contributes nothing.
//! * **Fail closed.** Unrecognized credential references become
//!   [`GrokCredentialMethod::Unknown`] rather than a guess, and an OIDC session
//!   that cannot prove an expiry becomes [`GrokSessionState::Unknown`] rather
//!   than `Active`.
//! * **Headless and browser safe.** The projection is a pure function of
//!   ([`GrokAccountFacts`], `now`). It performs no I/O, reads no environment,
//!   and pulls in no async runtime, so another product — ContextDesk, a
//!   headless certification lab — can import the same contract and reproduce
//!   the exact bytes the desktop UI renders.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Version stamp carried by every [`PublicGrokAccountStatus`].
pub const GROK_ACCOUNT_STATUS_SCHEMA: &str = "grokptah.grok-account-status.v1";

/// How long before expiry a Grok Build session reports
/// [`GrokSessionState::Expiring`]. The editor warns inside this window instead
/// of letting a long run die on a mid-flight `401`.
pub const GROK_ACCOUNT_EXPIRY_WARN_SECONDS: i64 = 600;

/// Exact public key allowlist for a serialized [`PublicGrokAccountStatus`].
///
/// Mirrors the discipline of
/// [`crate::orchestration::PUBLIC_PROVIDER_ROUTE_KEYS`]: a field that is not
/// named here cannot appear on the wire.
pub const GROK_ACCOUNT_STATUS_KEYS: &[&str] = &[
    "accountLabel",
    "accountRef",
    "expiresAt",
    "expiresInSeconds",
    "method",
    "providerId",
    "schema",
    "session",
    "usable",
];

/// Longest masked account label this module will emit.
const MAX_ACCOUNT_LABEL_BYTES: usize = 254;

/// Closed credential vocabulary.
///
/// The wire names match [`crate::provider_observation::CredentialMethod`] for
/// every shared variant so the diagnostics recorder and the editor cannot drift
/// apart; `grok_account_method_names_match_provider_observation` pins that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GrokCredentialMethod {
    /// Grok Build OIDC session from `~/.grok/auth.json`.
    GrokBuildOidc,
    /// Host-managed xAI API key (`XAI_API_KEY` or the desktop keychain entry).
    XaiApiKey,
    /// Compatible gateway profile whose key is owned by the environment.
    GatewayManaged,
    /// Compatible gateway profile whose key is stored in the OS keychain.
    GatewayApiKey,
    /// Nothing resolved, or a reference this build does not recognize.
    #[default]
    Unknown,
}

impl GrokCredentialMethod {
    /// Classify the credential reference frozen onto a durable provider route.
    ///
    /// Mirrors the exact references
    /// [`crate::auth_store::resolve_wire_credentials_for_route`] accepts. That
    /// resolver already rejects anything else, so an unrecognized reference here
    /// is a route no run can execute on; reporting [`Self::Unknown`] keeps the
    /// projection honest instead of inventing an attribution.
    ///
    /// This reads only the *shape* of the reference. The profile name inside a
    /// `keychain:` reference never leaves this function.
    pub fn from_credential_ref(reference: &str) -> Self {
        match reference {
            "managed:xai:oidc" => Self::GrokBuildOidc,
            "managed:xai:api-key" => Self::XaiApiKey,
            // `validate_provider_credential_ref` admits exactly these two
            // environment references, each bound to its own profile.
            "env:GROKPTAH_API_KEY" | "env:OPENAI_API_KEY" => Self::GatewayManaged,
            other if other.starts_with("keychain:") => Self::GatewayApiKey,
            _ => Self::Unknown,
        }
    }

    /// Stable wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GrokBuildOidc => "grok_build_oidc",
            Self::XaiApiKey => "xai_api_key",
            Self::GatewayManaged => "gateway_managed",
            Self::GatewayApiKey => "gateway_api_key",
            Self::Unknown => "unknown",
        }
    }

    /// True only for the user's Grok Build browser/CLI session.
    pub const fn is_grok_build_session(self) -> bool {
        matches!(self, Self::GrokBuildOidc)
    }

    /// True when the credential is a long-lived key with no expiry to track.
    const fn is_static_key(self) -> bool {
        matches!(
            self,
            Self::XaiApiKey | Self::GatewayManaged | Self::GatewayApiKey
        )
    }
}

/// Validity of the resolved credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrokSessionState {
    /// OIDC session valid beyond the warn window.
    Active,
    /// OIDC session valid, but inside [`GROK_ACCOUNT_EXPIRY_WARN_SECONDS`].
    Expiring,
    /// OIDC session is past its expiry.
    Expired,
    /// Long-lived API key. There is no expiry to observe.
    NoExpiry,
    /// An OIDC session that did not carry a parseable `expires_at`. Validity is
    /// unproven, so it is reported as unproven rather than as `Active`.
    Unknown,
    /// No credential resolved at all.
    Absent,
}

impl GrokSessionState {
    /// Stable wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Expiring => "expiring",
            Self::Expired => "expired",
            Self::NoExpiry => "no_expiry",
            Self::Unknown => "unknown",
            Self::Absent => "absent",
        }
    }

    /// Whether a run may be started on this credential.
    ///
    /// False only on positive evidence that it cannot work ([`Self::Expired`],
    /// [`Self::Absent`]). [`Self::Unknown`] stays usable: a session whose
    /// `expires_at` this build could not parse still authenticates, and refusing
    /// it would break working installs. The editor should surface the unproven
    /// state rather than block on it.
    pub const fn is_usable(self) -> bool {
        !matches!(self, Self::Expired | Self::Absent)
    }
}

/// Non-secret inputs to the account projection.
///
/// Every field here is an account *identifier* or a timestamp. None of them is
/// credential material, which is what makes [`project_grok_account_status`]
/// structurally unable to leak a token.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GrokAccountFacts {
    /// Provider profile that owns the credential (`xai`, `env-openai`, ...).
    pub provider_id: String,
    /// Classified credential method.
    pub method: GrokCredentialMethod,
    /// Display string as resolved from the credential source. Only a masked
    /// account address is ever taken from it; see [`mask_account_email`].
    pub display_name: Option<String>,
    /// OIDC issuer, when the session recorded one.
    pub oidc_issuer: Option<String>,
    /// OIDC client id, when the session recorded one.
    pub oidc_client_id: Option<String>,
    /// Durable principal type from the session record.
    pub principal_type: Option<String>,
    /// Durable principal id from the session record.
    pub principal_id: Option<String>,
    /// Durable user id from the session record.
    pub user_id: Option<String>,
    /// Durable team id from the session record.
    pub team_id: Option<String>,
    /// Session expiry, when the session recorded one.
    pub expires_at: Option<DateTime<Utc>>,
}

/// Secret-free account status for the editor and for importing products.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicGrokAccountStatus {
    pub schema: String,
    pub provider_id: String,
    pub method: GrokCredentialMethod,
    pub session: GrokSessionState,
    /// Whether a run may start on this credential right now.
    pub usable: bool,
    /// Stable opaque account handle, or `None` when no durable principal exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_ref: Option<String>,
    /// Masked account address, or `None` when the source carried no address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Seconds until expiry; negative once expired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_seconds: Option<i64>,
}

impl PublicGrokAccountStatus {
    /// Status for "no credential resolved".
    pub fn absent() -> Self {
        Self {
            schema: GROK_ACCOUNT_STATUS_SCHEMA.to_string(),
            provider_id: String::new(),
            method: GrokCredentialMethod::Unknown,
            session: GrokSessionState::Absent,
            usable: false,
            account_ref: None,
            account_label: None,
            expires_at: None,
            expires_in_seconds: None,
        }
    }

    /// True when the editor is running on the user's Grok Build session and
    /// that session is usable right now.
    pub fn grok_build_session_ready(&self) -> bool {
        self.method.is_grok_build_session() && self.usable
    }
}

/// Mask an account address out of a credential display string.
///
/// `"Ada (ada.lovelace@example.com)"` becomes `"a…@example.com"`. The human
/// name is dropped, not masked: it is not needed to identify the account and
/// keeping it would put a real name into every receipt that carries a status.
///
/// Returns `None` when the input holds no address — an API key display such as
/// `"env:XAI_API_KEY"`, or the `"Grok Build session"` fallback — so a label is
/// only ever present when it is genuinely an account address.
pub fn mask_account_email(display_name: &str) -> Option<String> {
    let candidate = display_name
        .split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == ',' || c == ';')
        .find(|token| is_addressish(token))?;
    let (local, domain) = candidate.split_once('@')?;
    let mut label = String::with_capacity(local.len().min(1) + domain.len() + 4);
    let mut chars = local.chars();
    match chars.next() {
        // A single-character local part would be fully revealed by keeping its
        // first character, so mask it entirely.
        Some(first) if local.chars().count() > 1 => label.push(first),
        _ => {}
    }
    label.push('…');
    label.push('@');
    label.push_str(domain);
    (label.len() <= MAX_ACCOUNT_LABEL_BYTES).then_some(label)
}

/// Whether a token looks like an account address rather than a key reference.
fn is_addressish(token: &str) -> bool {
    let Some((local, domain)) = token.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && !local.contains('@')
        && !domain.contains('@')
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
}

/// Stable, one-way account handle over durable principal fields.
///
/// Deliberately narrower than
/// [`crate::auth_store::WireCredentials::qualification_identity_fingerprint`]:
/// that helper falls back to digesting the bearer or refresh token when no
/// principal is recorded, which makes it an oracle for anyone holding a
/// candidate credential. This one has no such fallback. It requires at least
/// one high-entropy account identifier (`principal_id`, `user_id`, or
/// `team_id`) and otherwise returns `None`, so the handle is never derived from
/// credential material and never from a guessable value alone.
///
/// OIDC access-token rotation does not change the handle.
pub fn account_ref_from_principal(facts: &GrokAccountFacts) -> Option<String> {
    let has_strong_identifier =
        facts.principal_id.is_some() || facts.user_id.is_some() || facts.team_id.is_some();
    if !has_strong_identifier {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(b"grokptah.grok-account-ref.v1\0");
    absorb(&mut digest, "provider", Some(&facts.provider_id));
    absorb(&mut digest, "method", Some(facts.method.as_str()));
    absorb(&mut digest, "issuer", facts.oidc_issuer.as_deref());
    absorb(&mut digest, "client", facts.oidc_client_id.as_deref());
    absorb(
        &mut digest,
        "principal_type",
        facts.principal_type.as_deref(),
    );
    absorb(&mut digest, "principal_id", facts.principal_id.as_deref());
    absorb(&mut digest, "user", facts.user_id.as_deref());
    absorb(&mut digest, "team", facts.team_id.as_deref());
    Some(format!("v1-sha256:{:x}", digest.finalize()))
}

/// Length-prefixed field absorption so distinct field splits cannot collide.
fn absorb(digest: &mut Sha256, label: &str, value: Option<&str>) {
    digest.update(label.len().to_be_bytes());
    digest.update(label.as_bytes());
    match value {
        Some(value) => {
            digest.update([1u8]);
            digest.update(value.len().to_be_bytes());
            digest.update(value.as_bytes());
        }
        None => digest.update([0u8]),
    }
}

/// Classify session validity at `now`.
pub fn session_state(facts: &GrokAccountFacts, now: DateTime<Utc>) -> GrokSessionState {
    if facts.method == GrokCredentialMethod::Unknown {
        return GrokSessionState::Absent;
    }
    if facts.method.is_static_key() {
        return GrokSessionState::NoExpiry;
    }
    let Some(expires_at) = facts.expires_at else {
        return GrokSessionState::Unknown;
    };
    let remaining = expires_at.signed_duration_since(now).num_seconds();
    if remaining <= 0 {
        GrokSessionState::Expired
    } else if remaining <= GROK_ACCOUNT_EXPIRY_WARN_SECONDS {
        GrokSessionState::Expiring
    } else {
        GrokSessionState::Active
    }
}

/// Project non-secret account facts into the public status DTO.
///
/// Pure: same `(facts, now)` always yields the same bytes, with no I/O.
pub fn project_grok_account_status(
    facts: &GrokAccountFacts,
    now: DateTime<Utc>,
) -> PublicGrokAccountStatus {
    if facts.method == GrokCredentialMethod::Unknown {
        return PublicGrokAccountStatus::absent();
    }
    let session = session_state(facts, now);
    // Expiry is only meaningful for a session that actually carries one.
    let expires_at = facts.expires_at.filter(|_| !facts.method.is_static_key());
    PublicGrokAccountStatus {
        schema: GROK_ACCOUNT_STATUS_SCHEMA.to_string(),
        provider_id: facts.provider_id.clone(),
        method: facts.method,
        session,
        usable: session.is_usable(),
        account_ref: account_ref_from_principal(facts),
        account_label: facts.display_name.as_deref().and_then(mask_account_email),
        expires_at,
        expires_in_seconds: expires_at.map(|at| at.signed_duration_since(now).num_seconds()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use serde_json::Value;

    fn oidc_facts() -> GrokAccountFacts {
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
            expires_at: None,
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-25T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn credential_ref_classification_is_exact_and_fails_closed() {
        assert_eq!(
            GrokCredentialMethod::from_credential_ref("managed:xai:oidc"),
            GrokCredentialMethod::GrokBuildOidc
        );
        assert_eq!(
            GrokCredentialMethod::from_credential_ref("managed:xai:api-key"),
            GrokCredentialMethod::XaiApiKey
        );
        assert_eq!(
            GrokCredentialMethod::from_credential_ref("env:GROKPTAH_API_KEY"),
            GrokCredentialMethod::GatewayManaged
        );
        assert_eq!(
            GrokCredentialMethod::from_credential_ref("env:OPENAI_API_KEY"),
            GrokCredentialMethod::GatewayManaged
        );
        assert_eq!(
            GrokCredentialMethod::from_credential_ref("keychain:provider/corp-b/api-key"),
            GrokCredentialMethod::GatewayApiKey
        );
        // Anything the route resolver would reject stays Unknown.
        for reference in [
            "",
            "managed:xai",
            "managed:xai:oidc-extra",
            "env:XAI_API_KEY",
            "env:ANYTHING_ELSE",
            "inline:secret",
            "MANAGED:XAI:OIDC",
        ] {
            assert_eq!(
                GrokCredentialMethod::from_credential_ref(reference),
                GrokCredentialMethod::Unknown,
                "{reference} must not be guessed"
            );
        }
    }

    #[test]
    fn keychain_profile_name_never_escapes_classification() {
        let method =
            GrokCredentialMethod::from_credential_ref("keychain:provider/acme-corp/api-key");
        assert_eq!(method, GrokCredentialMethod::GatewayApiKey);
        assert!(!method.as_str().contains("acme"));
    }

    #[test]
    fn expiring_session_is_reported_before_it_dies_mid_run() {
        let mut facts = oidc_facts();

        facts.expires_at = Some(now() + Duration::hours(2));
        assert_eq!(session_state(&facts, now()), GrokSessionState::Active);

        facts.expires_at = Some(now() + Duration::seconds(GROK_ACCOUNT_EXPIRY_WARN_SECONDS));
        assert_eq!(session_state(&facts, now()), GrokSessionState::Expiring);

        facts.expires_at = Some(now() + Duration::seconds(1));
        assert_eq!(session_state(&facts, now()), GrokSessionState::Expiring);

        facts.expires_at = Some(now());
        assert_eq!(session_state(&facts, now()), GrokSessionState::Expired);

        facts.expires_at = Some(now() - Duration::hours(1));
        assert_eq!(session_state(&facts, now()), GrokSessionState::Expired);
    }

    #[test]
    fn expired_grok_build_session_is_not_usable() {
        let mut facts = oidc_facts();
        facts.expires_at = Some(now() - Duration::minutes(5));
        let status = project_grok_account_status(&facts, now());

        assert_eq!(status.session, GrokSessionState::Expired);
        assert!(!status.usable);
        assert!(!status.grok_build_session_ready());
        assert_eq!(status.expires_in_seconds, Some(-300));
    }

    #[test]
    fn oidc_session_without_expiry_is_unknown_not_active() {
        let facts = oidc_facts();
        assert_eq!(facts.expires_at, None);
        let status = project_grok_account_status(&facts, now());

        assert_eq!(status.session, GrokSessionState::Unknown);
        // Unproven, but not blocked: the credential still authenticates.
        assert!(status.usable);
        assert_eq!(status.expires_in_seconds, None);
    }

    #[test]
    fn static_keys_report_no_expiry_and_carry_no_timestamp() {
        for method in [
            GrokCredentialMethod::XaiApiKey,
            GrokCredentialMethod::GatewayManaged,
            GrokCredentialMethod::GatewayApiKey,
        ] {
            let facts = GrokAccountFacts {
                provider_id: "xai".into(),
                method,
                // A stale timestamp on a static key must not be reported.
                expires_at: Some(now() - Duration::hours(9)),
                ..GrokAccountFacts::default()
            };
            let status = project_grok_account_status(&facts, now());
            assert_eq!(status.session, GrokSessionState::NoExpiry, "{method:?}");
            assert!(status.usable, "{method:?}");
            assert_eq!(status.expires_at, None, "{method:?}");
            assert_eq!(status.expires_in_seconds, None, "{method:?}");
        }
    }

    #[test]
    fn unknown_method_projects_absent() {
        let facts = GrokAccountFacts {
            provider_id: "xai".into(),
            method: GrokCredentialMethod::Unknown,
            display_name: Some("ada@example.com".into()),
            principal_id: Some("principal-9f2".into()),
            ..GrokAccountFacts::default()
        };
        let status = project_grok_account_status(&facts, now());

        assert_eq!(status, PublicGrokAccountStatus::absent());
        assert!(!status.usable);
        // An unresolvable route reports nothing about the account behind it.
        assert_eq!(status.account_ref, None);
        assert_eq!(status.account_label, None);
        assert!(status.provider_id.is_empty());
    }

    #[test]
    fn account_label_masks_the_address_and_drops_the_human_name() {
        assert_eq!(
            mask_account_email("Ada (ada.lovelace@example.com)"),
            Some("a…@example.com".into())
        );
        assert_eq!(
            mask_account_email("ada@example.com"),
            Some("a…@example.com".into())
        );
        // A one-character local part is masked entirely.
        assert_eq!(
            mask_account_email("a@example.com"),
            Some("…@example.com".into())
        );

        let label = mask_account_email("Ada (ada.lovelace@example.com)").unwrap();
        assert!(!label.contains("Ada"));
        assert!(!label.contains("lovelace"));
        assert!(!label.contains("ada."));
    }

    #[test]
    fn non_address_display_names_yield_no_label() {
        for display in [
            "env:XAI_API_KEY",
            "Grok Build session",
            "env-openai",
            "",
            "@example.com",
            "ada@",
            "ada@localhost",
            "ada@.com",
        ] {
            assert_eq!(mask_account_email(display), None, "{display:?}");
        }
    }

    #[test]
    fn account_ref_is_stable_across_token_rotation() {
        let mut facts = oidc_facts();
        facts.expires_at = Some(now() + Duration::hours(1));
        let first = project_grok_account_status(&facts, now()).account_ref;

        // A refreshed session: new expiry, same durable principal.
        facts.expires_at = Some(now() + Duration::hours(9));
        let second = project_grok_account_status(&facts, now()).account_ref;

        assert!(first.is_some());
        assert_eq!(first, second);
        assert!(first.unwrap().starts_with("v1-sha256:"));
    }

    #[test]
    fn account_ref_separates_distinct_principals() {
        let base = oidc_facts();
        let mut other = oidc_facts();
        other.principal_id = Some("principal-000".into());
        assert_ne!(
            account_ref_from_principal(&base),
            account_ref_from_principal(&other)
        );
    }

    #[test]
    fn account_ref_requires_a_durable_principal_and_never_digests_a_secret() {
        // Issuer and client alone are not an account identifier.
        let facts = GrokAccountFacts {
            provider_id: "xai".into(),
            method: GrokCredentialMethod::GrokBuildOidc,
            oidc_issuer: Some("https://issuer.example".into()),
            oidc_client_id: Some("dynamic-client".into()),
            ..GrokAccountFacts::default()
        };
        assert_eq!(account_ref_from_principal(&facts), None);

        // A static key has no principal, so it gets no handle.
        let key_facts = GrokAccountFacts {
            provider_id: "xai".into(),
            method: GrokCredentialMethod::XaiApiKey,
            display_name: Some("env:XAI_API_KEY".into()),
            ..GrokAccountFacts::default()
        };
        assert_eq!(account_ref_from_principal(&key_facts), None);
    }

    #[test]
    fn serialized_status_carries_only_allowlisted_keys() {
        let mut facts = oidc_facts();
        facts.expires_at = Some(now() + Duration::hours(3));
        let status = project_grok_account_status(&facts, now());
        let value = serde_json::to_value(&status).unwrap();
        let object = value.as_object().unwrap();

        for key in object.keys() {
            assert!(
                GROK_ACCOUNT_STATUS_KEYS.contains(&key.as_str()),
                "{key} is not on the public allowlist"
            );
        }
        assert_eq!(value["schema"], GROK_ACCOUNT_STATUS_SCHEMA);
        assert_eq!(value["method"], "grok_build_oidc");
        assert_eq!(value["session"], "active");
        assert_eq!(value["usable"], true);
    }

    #[test]
    fn serialized_status_never_carries_credential_material() {
        let mut facts = oidc_facts();
        facts.expires_at = Some(now() + Duration::hours(3));
        let encoded = serde_json::to_string(&project_grok_account_status(&facts, now())).unwrap();
        let lowered = encoded.to_ascii_lowercase();

        for needle in [
            "bearer",
            "refresh",
            "token",
            "apikey",
            "api_key",
            "password",
            "secret",
            "authorization",
            "auth.json",
            "/home/",
            "/users/",
        ] {
            assert!(
                !lowered.contains(needle),
                "{needle} must not appear in {encoded}"
            );
        }
        // The raw address is masked even though the projection received it.
        assert!(!encoded.contains("ada.lovelace@example.com"));
        assert!(encoded.contains("a…@example.com"));
    }

    #[test]
    fn status_round_trips_and_rejects_unknown_fields() {
        let mut facts = oidc_facts();
        facts.expires_at = Some(now() + Duration::hours(3));
        let status = project_grok_account_status(&facts, now());
        let encoded = serde_json::to_string(&status).unwrap();

        let decoded: PublicGrokAccountStatus = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, status);

        let mut leaky: Value = serde_json::from_str(&encoded).unwrap();
        leaky["bearer"] = Value::String("secret".into());
        assert!(serde_json::from_value::<PublicGrokAccountStatus>(leaky).is_err());
    }

    #[test]
    fn projection_is_pure_for_a_fixed_clock() {
        let mut facts = oidc_facts();
        facts.expires_at = Some(now() + Duration::hours(3));
        let first = serde_json::to_string(&project_grok_account_status(&facts, now())).unwrap();
        let second = serde_json::to_string(&project_grok_account_status(&facts, now())).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn grok_account_method_names_match_provider_observation() {
        use crate::provider_observation::CredentialMethod;

        // Shared variants must serialize identically so the editor contract and
        // the attempt recorder cannot describe the same credential differently.
        for (ours, theirs) in [
            (
                GrokCredentialMethod::GrokBuildOidc,
                CredentialMethod::GrokBuildOidc,
            ),
            (GrokCredentialMethod::XaiApiKey, CredentialMethod::XaiApiKey),
            (
                GrokCredentialMethod::GatewayManaged,
                CredentialMethod::GatewayManaged,
            ),
            (
                GrokCredentialMethod::GatewayApiKey,
                CredentialMethod::GatewayApiKey,
            ),
        ] {
            assert_eq!(
                serde_json::to_value(ours).unwrap(),
                serde_json::to_value(theirs).unwrap()
            );
            assert_eq!(serde_json::to_value(ours).unwrap(), ours.as_str());
        }
    }

    #[test]
    fn session_state_names_are_stable() {
        for (state, name) in [
            (GrokSessionState::Active, "active"),
            (GrokSessionState::Expiring, "expiring"),
            (GrokSessionState::Expired, "expired"),
            (GrokSessionState::NoExpiry, "no_expiry"),
            (GrokSessionState::Unknown, "unknown"),
            (GrokSessionState::Absent, "absent"),
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), name);
            assert_eq!(state.as_str(), name);
        }
    }
}
