//! Host-neutral Grok Build account readiness facts.
//!
//! This module answers one bounded question for an editor: *may this host
//! start a new Grok Build run right now, and against which account?* It is a
//! deliberately lossy projection of local credential state.
//!
//! # Non-goals (enforced structurally)
//!
//! [`GrokAccountFacts`] has no field that can carry a bearer token, refresh
//! token, API key, keychain reference, credential fingerprint, or a free-form
//! `auth_mode` string. [`CredentialMethod`] is a closed vocabulary with no
//! `Other(String)` variant, so an unrecognized `auth_mode` collapses to
//! [`CredentialMethod::Unknown`] instead of being echoed. The timestamp in
//! [`ExpiryFacts::expires_at`] is re-serialized from parsed components rather
//! than copied from input, so no caller-controlled text survives projection.
//!
//! Credential material is never hashed into an account oracle: the account
//! reference is read only from durable, non-secret account identity fields
//! (`user_id`, `principal_id`, `team_id`) and is dropped entirely when none of
//! them is present or in bounds.
//!
//! These facts describe *local* state only. They never claim account balance,
//! remaining quota, entitlement, or live provider certification — only a
//! provider round-trip can establish those.

use serde::{Deserialize, Serialize};

/// Stable contract identifier for the account readiness projection.
pub const GROK_ACCOUNT_CONTRACT_VERSION: &str = "grokptah.account.v1";
/// Numeric schema revision carried in every projection.
pub const GROK_ACCOUNT_SCHEMA_VERSION: u32 = 1;
/// Maximum UTF-8 bytes accepted in a public account reference.
pub const MAX_ACCOUNT_REFERENCE_BYTES: usize = 64;

/// Where a credential was observed, decided by the host adapter.
///
/// This is projection *input*, not part of the published contract. It carries
/// no credential material — only the route that produced one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialSource {
    /// No credential resolved on any route.
    Absent,
    /// `XAI_API_KEY` process environment.
    EnvApiKey,
    /// OS keychain API key for the built-in xAI profile.
    KeychainApiKey,
    /// Rotating token helper command.
    TokenCommand,
    /// Compatible-provider environment credential.
    ProviderEnv,
    /// Compatible-provider keychain credential.
    ProviderKeychain,
    /// `~/.grok/auth.json` Grok Build session (browser/CLI login).
    GrokBuildSession,
}

/// Closed vocabulary describing how a run authenticates.
///
/// There is intentionally no free-form variant: an `auth_mode` the projection
/// does not recognize becomes [`CredentialMethod::Unknown`], and the raw text
/// is discarded rather than forwarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialMethod {
    /// No credential is available on any route.
    Absent,
    /// A direct xAI API key (environment or OS keychain).
    ApiKey,
    /// A rotating token helper command.
    TokenCommand,
    /// A compatible-provider environment credential.
    ProviderEnv,
    /// A compatible-provider keychain credential.
    ProviderKeychain,
    /// A Grok Build session using the OIDC/user token route.
    GrokBuildOidc,
    /// A Grok Build session pinned to the API-key route.
    GrokBuildApiKey,
    /// A credential exists but its route could not be classified.
    Unknown,
}

impl CredentialMethod {
    /// The exact wire value used by the JSON contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::ApiKey => "api_key",
            Self::TokenCommand => "token_command",
            Self::ProviderEnv => "provider_env",
            Self::ProviderKeychain => "provider_keychain",
            Self::GrokBuildOidc => "grok_build_oidc",
            Self::GrokBuildApiKey => "grok_build_api_key",
            Self::Unknown => "unknown",
        }
    }

    /// Every method in declaration order, for schema and parity pinning.
    pub const ALL: [Self; 8] = [
        Self::Absent,
        Self::ApiKey,
        Self::TokenCommand,
        Self::ProviderEnv,
        Self::ProviderKeychain,
        Self::GrokBuildOidc,
        Self::GrokBuildApiKey,
        Self::Unknown,
    ];
}

/// Which durable, non-secret identity field produced an account reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountReferenceSource {
    /// `user_id` from the session record.
    UserId,
    /// `principal_id` from the session record.
    PrincipalId,
    /// `team_id` from the session record.
    TeamId,
}

impl AccountReferenceSource {
    /// The exact wire value used by the JSON contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UserId => "user_id",
            Self::PrincipalId => "principal_id",
            Self::TeamId => "team_id",
        }
    }

    /// Every source in preference order, for schema and parity pinning.
    pub const ALL: [Self; 3] = [Self::UserId, Self::PrincipalId, Self::TeamId];
}

/// A bounded, non-secret handle for *which* account a run bills against.
///
/// Never derived from credential material. Email addresses and display names
/// are deliberately excluded: they are personal data, and an opaque durable
/// identifier already disambiguates accounts for a power user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountReference {
    /// Opaque durable account identifier, charset- and length-bounded.
    pub value: String,
    /// Which identity field produced [`AccountReference::value`].
    pub source: AccountReferenceSource,
}

impl AccountReference {
    /// Accept a durable identifier only when it is safe to publish verbatim.
    ///
    /// Rejects empty, over-long, and non-opaque values (anything outside
    /// `[A-Za-z0-9._:-]`) so no control characters, whitespace, path
    /// fragments, or markup can reach a UI or a receipt.
    pub fn new(value: &str, source: AccountReferenceSource) -> Option<Self> {
        let value = value.trim();
        if value.is_empty() || value.len() > MAX_ACCOUNT_REFERENCE_BYTES {
            return None;
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
        {
            return None;
        }
        Some(Self {
            value: value.to_string(),
            source,
        })
    }
}

/// What is known about credential expiry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpiryStatus {
    /// The record carried no expiry field.
    Absent,
    /// An expiry field was present but is not a valid RFC3339 instant.
    Unparseable,
    /// Expiry parsed and is strictly in the future.
    Valid,
    /// Expiry parsed and is at or before the observation instant.
    Expired,
}

impl ExpiryStatus {
    /// The exact wire value used by the JSON contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Unparseable => "unparseable",
            Self::Valid => "valid",
            Self::Expired => "expired",
        }
    }

    /// Every status in declaration order, for schema and parity pinning.
    pub const ALL: [Self; 4] = [Self::Absent, Self::Unparseable, Self::Valid, Self::Expired];
}

/// Parsed expiry evidence for the selected credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExpiryFacts {
    /// What the projection could establish about expiry.
    pub status: ExpiryStatus,
    /// Normalized `YYYY-MM-DDTHH:MM:SSZ`, re-serialized from parsed parts.
    ///
    /// `None` whenever expiry is absent or unparseable, so caller-controlled
    /// text never survives into a receipt or the accessibility tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Whole seconds until expiry; negative once elapsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds_remaining: Option<i64>,
}

impl ExpiryFacts {
    /// Expiry evidence that is honest about knowing nothing.
    pub const fn absent() -> Self {
        Self {
            status: ExpiryStatus::Absent,
            expires_at: None,
            seconds_remaining: None,
        }
    }

    /// Expiry evidence for a field that was present but not RFC3339.
    pub const fn unparseable() -> Self {
        Self {
            status: ExpiryStatus::Unparseable,
            expires_at: None,
            seconds_remaining: None,
        }
    }

    /// Reject evidence whose parts disagree with its status.
    ///
    /// Without this, a producer could report "expiry unreadable" while still
    /// attaching a timestamp, which is exactly the caller-controlled text the
    /// projection exists to strip.
    pub fn validate(&self) -> Result<(), &'static str> {
        let has_parts = self.expires_at.is_some() || self.seconds_remaining.is_some();
        match self.status {
            ExpiryStatus::Absent | ExpiryStatus::Unparseable if has_parts => {
                Err("expiry without evidence must not carry a timestamp")
            }
            ExpiryStatus::Valid | ExpiryStatus::Expired
                if self.expires_at.is_none() || self.seconds_remaining.is_none() =>
            {
                Err("parsed expiry must carry a normalized instant and a remainder")
            }
            _ => Ok(()),
        }
    }
}

/// Whether the editor may start a new Grok Build run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountReadiness {
    /// Positive evidence the credential is currently valid.
    Usable,
    /// No evidence either way. Never blocks a launch.
    Unknown,
    /// Positive evidence the credential cannot work. Blocks new launches.
    Unusable,
}

impl AccountReadiness {
    /// The exact wire value used by the JSON contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Usable => "usable",
            Self::Unknown => "unknown",
            Self::Unusable => "unusable",
        }
    }

    /// Every readiness state in declaration order, for parity pinning.
    pub const ALL: [Self; 3] = [Self::Usable, Self::Unknown, Self::Unusable];

    /// Whether a *new* run may be launched.
    ///
    /// Only positive negative-evidence blocks. Unknown stays permissive so a
    /// credential with no expiry field is never locked out by our ignorance.
    pub const fn permits_launch(self) -> bool {
        !matches!(self, Self::Unusable)
    }
}

/// Why a readiness verdict was reached, for exact UI copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessReason {
    /// No credential resolved on any route.
    NoCredential,
    /// A parsed expiry is at or before the observation instant.
    CredentialExpired,
    /// A parsed expiry is strictly in the future.
    ExpiryInFuture,
    /// The record carried no expiry field.
    ExpiryNotProvided,
    /// An expiry field was present but is not a valid RFC3339 instant.
    ExpiryUnparseable,
    /// The credential route could not be classified, so no claim is made.
    MethodUnrecognized,
}

impl ReadinessReason {
    /// The exact wire value used by the JSON contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoCredential => "no_credential",
            Self::CredentialExpired => "credential_expired",
            Self::ExpiryInFuture => "expiry_in_future",
            Self::ExpiryNotProvided => "expiry_not_provided",
            Self::ExpiryUnparseable => "expiry_unparseable",
            Self::MethodUnrecognized => "method_unrecognized",
        }
    }

    /// Every reason in declaration order, for schema and parity pinning.
    pub const ALL: [Self; 6] = [
        Self::NoCredential,
        Self::CredentialExpired,
        Self::ExpiryInFuture,
        Self::ExpiryNotProvided,
        Self::ExpiryUnparseable,
        Self::MethodUnrecognized,
    ];
}

/// Non-secret observation of one credential record, before classification.
///
/// Host adapters build this from local state. It must never be populated with
/// a bearer, refresh token, API key, or keychain reference — those fields do
/// not exist here, and the projection never asks for them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccountObservation<'a> {
    /// Raw `auth_mode`, meaningful only for [`CredentialSource::GrokBuildSession`].
    pub auth_mode: Option<&'a str>,
    /// Durable `user_id`, preferred account reference source.
    pub user_id: Option<&'a str>,
    /// Durable `principal_id`, used when `user_id` is absent.
    pub principal_id: Option<&'a str>,
    /// Durable `team_id`, used when no individual identity is present.
    pub team_id: Option<&'a str>,
    /// Raw expiry candidate, parsed strictly and never echoed verbatim.
    pub expires_at: Option<&'a str>,
}

/// Versioned, credential-free account readiness projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrokAccountFacts {
    /// Stable contract identifier, always [`GROK_ACCOUNT_CONTRACT_VERSION`].
    pub contract: String,
    /// Numeric schema revision, always [`GROK_ACCOUNT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Closed-vocabulary credential route.
    pub credential_method: CredentialMethod,
    /// Bounded non-secret account handle, when durable identity is present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_reference: Option<AccountReference>,
    /// Parsed expiry evidence.
    pub expiry: ExpiryFacts,
    /// Whether a new run may launch.
    pub readiness: AccountReadiness,
    /// Why [`GrokAccountFacts::readiness`] holds.
    pub readiness_reason: ReadinessReason,
}

impl GrokAccountFacts {
    /// Facts for a host with no credential on any route.
    pub fn absent() -> Self {
        Self {
            contract: GROK_ACCOUNT_CONTRACT_VERSION.to_string(),
            schema_version: GROK_ACCOUNT_SCHEMA_VERSION,
            credential_method: CredentialMethod::Absent,
            account_reference: None,
            expiry: ExpiryFacts::absent(),
            readiness: AccountReadiness::Unusable,
            readiness_reason: ReadinessReason::NoCredential,
        }
    }

    /// Project readiness facts from a non-secret observation.
    ///
    /// `now_unix` is the observation instant in whole seconds since the Unix
    /// epoch. It is an explicit parameter so every verdict is reproducible
    /// under a fixed clock in tests.
    pub fn project(
        source: CredentialSource,
        observation: &AccountObservation<'_>,
        now_unix: i64,
    ) -> Self {
        if source == CredentialSource::Absent {
            return Self::absent();
        }
        let credential_method = classify_method(source, observation.auth_mode);
        let expiry = project_expiry(observation.expires_at, now_unix);
        let (readiness, readiness_reason) = decide_readiness(credential_method, expiry.status);
        Self {
            contract: GROK_ACCOUNT_CONTRACT_VERSION.to_string(),
            schema_version: GROK_ACCOUNT_SCHEMA_VERSION,
            credential_method,
            account_reference: project_account_reference(observation),
            expiry,
            readiness,
            readiness_reason,
        }
    }

    /// Project readiness facts from a parsed `~/.grok/auth.json` document.
    ///
    /// Selection mirrors the Grok Build session loader: the first record with
    /// a credential that is not positively expired wins; otherwise the last
    /// expired record is reported so the UI can explain *why* it is blocked.
    /// The credential value itself is only tested for presence — never read
    /// into the projection.
    pub fn from_auth_json(document: &serde_json::Value, now_unix: i64) -> Self {
        let Some(entries) = document.as_object() else {
            return Self::absent();
        };
        let mut blocked: Option<Self> = None;
        for record in entries.values() {
            let Some(record) = record.as_object() else {
                continue;
            };
            let has_credential = record
                .get("key")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| !value.is_empty());
            if !has_credential {
                continue;
            }
            let observation = AccountObservation {
                auth_mode: record.get("auth_mode").and_then(serde_json::Value::as_str),
                user_id: record.get("user_id").and_then(serde_json::Value::as_str),
                principal_id: record
                    .get("principal_id")
                    .and_then(serde_json::Value::as_str),
                team_id: record.get("team_id").and_then(serde_json::Value::as_str),
                expires_at: record.get("expires_at").and_then(serde_json::Value::as_str),
            };
            let facts = Self::project(CredentialSource::GrokBuildSession, &observation, now_unix);
            if facts.expiry.status != ExpiryStatus::Expired {
                return facts;
            }
            blocked = Some(facts);
        }
        blocked.unwrap_or_else(Self::absent)
    }

    /// Whether the editor may start a *new* run against this account.
    pub const fn permits_launch(&self) -> bool {
        self.readiness.permits_launch()
    }

    /// Bounded public attribution for a run started against this account.
    pub fn attribution(&self) -> RunAttribution {
        RunAttribution {
            credential_method: self.credential_method,
            account_reference: self.account_reference.clone(),
        }
    }

    /// Validate the bounded public projection before publishing it.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.contract != GROK_ACCOUNT_CONTRACT_VERSION {
            return Err("account contract identifier does not match this revision");
        }
        if self.schema_version != GROK_ACCOUNT_SCHEMA_VERSION {
            return Err("account schema version does not match this revision");
        }
        if let Some(reference) = &self.account_reference
            && AccountReference::new(&reference.value, reference.source).as_ref() != Some(reference)
        {
            return Err("account reference is not a bounded opaque identifier");
        }
        self.expiry.validate()?;
        let (readiness, reason) = decide_readiness(self.credential_method, self.expiry.status);
        if self.readiness != readiness || self.readiness_reason != reason {
            return Err("readiness verdict does not follow from method and expiry");
        }
        Ok(())
    }
}

/// Bounded credential attribution attached to a durable run.
///
/// Records *how* a run authenticated and *which* account it billed against.
/// It deliberately carries no balance, quota, entitlement, or certification
/// claim: those require a provider round-trip this projection never performs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunAttribution {
    /// Closed-vocabulary credential route used by the run.
    pub credential_method: CredentialMethod,
    /// Bounded non-secret account handle, when durable identity was present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_reference: Option<AccountReference>,
}

impl RunAttribution {
    /// Validate bounded attribution before attaching it to a receipt.
    pub fn validate(&self) -> Result<(), &'static str> {
        match &self.account_reference {
            Some(reference)
                if AccountReference::new(&reference.value, reference.source).as_ref()
                    != Some(reference) =>
            {
                Err("account reference is not a bounded opaque identifier")
            }
            _ => Ok(()),
        }
    }
}

fn classify_method(source: CredentialSource, auth_mode: Option<&str>) -> CredentialMethod {
    match source {
        CredentialSource::Absent => CredentialMethod::Absent,
        CredentialSource::EnvApiKey | CredentialSource::KeychainApiKey => CredentialMethod::ApiKey,
        CredentialSource::TokenCommand => CredentialMethod::TokenCommand,
        CredentialSource::ProviderEnv => CredentialMethod::ProviderEnv,
        CredentialSource::ProviderKeychain => CredentialMethod::ProviderKeychain,
        // Exact matches only. The wire loader treats any mode *containing*
        // "oidc" as a user session; this projection is deliberately stricter
        // so a crafted `auth_mode` cannot dress itself up as a known route.
        CredentialSource::GrokBuildSession => match auth_mode {
            None | Some("oidc") | Some("user") | Some("user_token") => {
                CredentialMethod::GrokBuildOidc
            }
            Some("api_key") => CredentialMethod::GrokBuildApiKey,
            Some(_) => CredentialMethod::Unknown,
        },
    }
}

fn project_account_reference(observation: &AccountObservation<'_>) -> Option<AccountReference> {
    let candidates = [
        (observation.user_id, AccountReferenceSource::UserId),
        (
            observation.principal_id,
            AccountReferenceSource::PrincipalId,
        ),
        (observation.team_id, AccountReferenceSource::TeamId),
    ];
    candidates
        .into_iter()
        .find_map(|(value, source)| AccountReference::new(value?, source))
}

fn project_expiry(expires_at: Option<&str>, now_unix: i64) -> ExpiryFacts {
    let Some(raw) = expires_at else {
        return ExpiryFacts::absent();
    };
    let Some(epoch_seconds) = parse_rfc3339_seconds(raw) else {
        return ExpiryFacts::unparseable();
    };
    let status = if epoch_seconds > now_unix {
        ExpiryStatus::Valid
    } else {
        ExpiryStatus::Expired
    };
    ExpiryFacts {
        status,
        expires_at: Some(format_utc_seconds(epoch_seconds)),
        seconds_remaining: Some(epoch_seconds.saturating_sub(now_unix)),
    }
}

fn decide_readiness(
    method: CredentialMethod,
    expiry: ExpiryStatus,
) -> (AccountReadiness, ReadinessReason) {
    if method == CredentialMethod::Absent {
        return (AccountReadiness::Unusable, ReadinessReason::NoCredential);
    }
    match expiry {
        ExpiryStatus::Expired => (
            AccountReadiness::Unusable,
            ReadinessReason::CredentialExpired,
        ),
        // An unclassified route never blocks, but it also never earns a
        // positive "usable" claim: we cannot say what we did not recognize.
        ExpiryStatus::Valid if method == CredentialMethod::Unknown => (
            AccountReadiness::Unknown,
            ReadinessReason::MethodUnrecognized,
        ),
        ExpiryStatus::Valid => (AccountReadiness::Usable, ReadinessReason::ExpiryInFuture),
        ExpiryStatus::Absent if method == CredentialMethod::Unknown => (
            AccountReadiness::Unknown,
            ReadinessReason::MethodUnrecognized,
        ),
        ExpiryStatus::Absent => (
            AccountReadiness::Unknown,
            ReadinessReason::ExpiryNotProvided,
        ),
        ExpiryStatus::Unparseable => (
            AccountReadiness::Unknown,
            ReadinessReason::ExpiryUnparseable,
        ),
    }
}

/// Strict RFC3339 → Unix seconds. Returns `None` for anything else.
///
/// Accepts `YYYY-MM-DDTHH:MM:SS`, an optional fractional part (truncated
/// toward the past, matching whole-second expiry semantics), and either `Z`
/// or a `±HH:MM` offset. Leap seconds and two-digit years are rejected.
fn parse_rfc3339_seconds(raw: &str) -> Option<i64> {
    let bytes = raw.as_bytes();
    if bytes.len() < 20 || !raw.is_ascii() {
        return None;
    }
    let digits = |start: usize, len: usize| -> Option<i64> {
        let slice = raw.get(start..start + len)?;
        if !slice.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        slice.parse::<i64>().ok()
    };
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if !matches!(bytes[10], b'T' | b't') {
        return None;
    }
    if bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let year = digits(0, 4)?;
    let month = digits(5, 2)?;
    let day = digits(8, 2)?;
    let hour = digits(11, 2)?;
    let minute = digits(14, 2)?;
    let second = digits(17, 2)?;
    if !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }

    let mut rest = raw.get(19..)?;
    if let Some(fraction) = rest.strip_prefix('.') {
        let taken = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if taken == 0 {
            return None;
        }
        rest = fraction.get(taken..)?;
    }
    let offset_seconds = match rest.as_bytes() {
        [b'Z' | b'z'] => 0,
        [sign @ (b'+' | b'-'), _, _, b':', _, _] => {
            // `digits` indexes `raw`, so anchor on the offset's absolute start.
            let base = raw.len() - rest.len();
            let offset_hour = digits(base + 1, 2)?;
            let offset_minute = digits(base + 4, 2)?;
            if offset_hour > 23 || offset_minute > 59 {
                return None;
            }
            let magnitude = offset_hour * 3600 + offset_minute * 60;
            if *sign == b'+' { magnitude } else { -magnitude }
        }
        _ => return None,
    };

    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset_seconds)
}

const fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

const fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Hinnant's algorithm).
const fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_index = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Inverse of [`days_from_civil`], used to re-serialize a parsed instant.
const fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Re-serialize whole Unix seconds as `YYYY-MM-DDTHH:MM:SSZ`.
///
/// The projection publishes this instead of the observed string so no
/// caller-controlled text ever reaches a receipt or the accessibility tree.
fn format_utc_seconds(epoch_seconds: i64) -> String {
    let days = epoch_seconds.div_euclid(86_400);
    let seconds_of_day = epoch_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3600;
    let minute = (seconds_of_day % 3600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed observation clock: 2026-08-25T00:00:00Z. Every verdict below is
    /// reproducible against it, so no test reads the wall clock.
    const NOW: i64 = 1_787_616_000;
    /// A sentinel that must never survive projection into public output.
    const SENTINEL_BEARER: &str = "xai-SENTINEL-BEARER-DO-NOT-LEAK";
    const SENTINEL_REFRESH: &str = "xai-SENTINEL-REFRESH-DO-NOT-LEAK";

    fn oidc_session(expires_at: Option<&'static str>) -> serde_json::Value {
        serde_json::json!({
            "default": {
                "key": SENTINEL_BEARER,
                "refresh_token": SENTINEL_REFRESH,
                "email": "operator@example.test",
                "first_name": "Operator",
                "auth_mode": "oidc",
                "user_id": "usr-0a1b2c3d",
                "team_id": "team-9z8y",
                "expires_at": expires_at,
            }
        })
    }

    fn assert_no_credential_needles(facts: &GrokAccountFacts) {
        let encoded = serde_json::to_string(facts).expect("facts serialize");
        for needle in [
            SENTINEL_BEARER,
            SENTINEL_REFRESH,
            "refreshToken",
            "refresh_token",
            "bearer",
            "Bearer",
            "\"key\"",
            "credentialRef",
            "fingerprint",
            "authMode",
            "auth_mode",
            "operator@example.test",
        ] {
            assert!(
                !encoded.contains(needle),
                "public account facts leaked {needle:?}: {encoded}"
            );
        }
    }

    #[test]
    fn fixed_clock_constant_matches_the_strict_rfc3339_parser() {
        assert_eq!(parse_rfc3339_seconds("2026-08-25T00:00:00Z"), Some(NOW));
        assert_eq!(format_utc_seconds(NOW), "2026-08-25T00:00:00Z");
        assert_eq!(parse_rfc3339_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_seconds("2024-02-29T12:00:00Z"),
            Some(1_709_208_000)
        );
        // Offsets normalize to the same instant as their UTC spelling.
        assert_eq!(
            parse_rfc3339_seconds("2026-08-25T02:00:00+02:00"),
            Some(NOW)
        );
        assert_eq!(
            parse_rfc3339_seconds("2026-08-24T22:00:00-02:00"),
            Some(NOW)
        );
        // Fractional seconds truncate toward the past.
        assert_eq!(parse_rfc3339_seconds("2026-08-25T00:00:00.999Z"), Some(NOW));
    }

    #[test]
    fn valid_unexpired_oidc_session_is_usable_and_launchable() {
        let facts =
            GrokAccountFacts::from_auth_json(&oidc_session(Some("2026-08-25T12:30:00Z")), NOW);
        assert_eq!(facts.credential_method, CredentialMethod::GrokBuildOidc);
        assert_eq!(facts.readiness, AccountReadiness::Usable);
        assert_eq!(facts.readiness_reason, ReadinessReason::ExpiryInFuture);
        assert_eq!(facts.expiry.status, ExpiryStatus::Valid);
        assert_eq!(
            facts.expiry.expires_at.as_deref(),
            Some("2026-08-25T12:30:00Z")
        );
        assert_eq!(facts.expiry.seconds_remaining, Some(45_000));
        assert!(facts.permits_launch());
        assert_eq!(facts.validate(), Ok(()));
        assert_no_credential_needles(&facts);
    }

    #[test]
    fn expired_session_is_unusable_and_blocks_new_launches() {
        let facts =
            GrokAccountFacts::from_auth_json(&oidc_session(Some("2026-08-24T23:59:59Z")), NOW);
        assert_eq!(facts.readiness, AccountReadiness::Unusable);
        assert_eq!(facts.readiness_reason, ReadinessReason::CredentialExpired);
        assert_eq!(facts.expiry.status, ExpiryStatus::Expired);
        assert_eq!(facts.expiry.seconds_remaining, Some(-1));
        assert!(!facts.permits_launch());
        assert_eq!(facts.validate(), Ok(()));
        assert_no_credential_needles(&facts);
    }

    #[test]
    fn expiry_exactly_at_the_observation_instant_counts_as_expired() {
        let facts =
            GrokAccountFacts::from_auth_json(&oidc_session(Some("2026-08-25T00:00:00Z")), NOW);
        assert_eq!(facts.expiry.status, ExpiryStatus::Expired);
        assert_eq!(facts.expiry.seconds_remaining, Some(0));
        assert!(!facts.permits_launch());
    }

    #[test]
    fn absent_expiry_stays_unknown_and_never_blocks() {
        let facts = GrokAccountFacts::from_auth_json(&oidc_session(None), NOW);
        assert_eq!(facts.expiry.status, ExpiryStatus::Absent);
        assert_eq!(facts.expiry.expires_at, None);
        assert_eq!(facts.expiry.seconds_remaining, None);
        assert_eq!(facts.readiness, AccountReadiness::Unknown);
        assert_eq!(facts.readiness_reason, ReadinessReason::ExpiryNotProvided);
        assert!(facts.permits_launch());
        assert_no_credential_needles(&facts);
    }

    #[test]
    fn malformed_expiry_stays_unknown_and_never_echoes_the_raw_text() {
        for malformed in [
            "not-a-timestamp",
            "2026-13-01T00:00:00Z",
            "2026-02-30T00:00:00Z",
            "2026-08-25T24:00:00Z",
            "2026-08-25T00:60:00Z",
            "2026-08-25 00:00:00Z",
            "2026-08-25T00:00:00",
            "2026-08-25T00:00:00+0200",
            "2026-08-25T00:00:00.Z",
            "26-08-25T00:00:00Z",
            "\"><script>alert(1)</script>",
        ] {
            let document = serde_json::json!({
                "default": { "key": SENTINEL_BEARER, "auth_mode": "oidc", "expires_at": malformed }
            });
            let facts = GrokAccountFacts::from_auth_json(&document, NOW);
            assert_eq!(
                facts.expiry.status,
                ExpiryStatus::Unparseable,
                "{malformed:?} should not parse"
            );
            assert_eq!(facts.readiness, AccountReadiness::Unknown);
            assert_eq!(facts.readiness_reason, ReadinessReason::ExpiryUnparseable);
            assert!(facts.permits_launch(), "unknown expiry must not block");
            let encoded = serde_json::to_string(&facts).expect("facts serialize");
            assert!(
                !encoded.contains(malformed),
                "unparseable expiry {malformed:?} was echoed: {encoded}"
            );
        }
    }

    #[test]
    fn absent_credential_is_unusable_with_a_no_credential_reason() {
        for document in [
            serde_json::json!({}),
            serde_json::json!({ "default": { "email": "operator@example.test" } }),
            serde_json::json!({ "default": { "key": "" } }),
            serde_json::json!({ "default": "not-an-object" }),
            serde_json::json!([]),
            serde_json::json!("nonsense"),
        ] {
            let facts = GrokAccountFacts::from_auth_json(&document, NOW);
            assert_eq!(facts.credential_method, CredentialMethod::Absent);
            assert_eq!(facts.readiness, AccountReadiness::Unusable);
            assert_eq!(facts.readiness_reason, ReadinessReason::NoCredential);
            assert_eq!(facts.account_reference, None);
            assert!(!facts.permits_launch());
            assert_eq!(facts.validate(), Ok(()));
        }
    }

    #[test]
    fn grok_build_api_key_route_is_classified_separately_from_oidc() {
        let document = serde_json::json!({
            "default": {
                "key": SENTINEL_BEARER,
                "auth_mode": "api_key",
                "user_id": "usr-0a1b2c3d",
                "expires_at": "2027-01-01T00:00:00Z",
            }
        });
        let facts = GrokAccountFacts::from_auth_json(&document, NOW);
        assert_eq!(facts.credential_method, CredentialMethod::GrokBuildApiKey);
        assert_eq!(facts.readiness, AccountReadiness::Usable);
        assert!(facts.permits_launch());
        assert_no_credential_needles(&facts);
    }

    #[test]
    fn direct_api_key_and_helper_routes_classify_without_an_auth_mode() {
        let observation = AccountObservation::default();
        for (source, expected) in [
            (CredentialSource::EnvApiKey, CredentialMethod::ApiKey),
            (CredentialSource::KeychainApiKey, CredentialMethod::ApiKey),
            (
                CredentialSource::TokenCommand,
                CredentialMethod::TokenCommand,
            ),
            (CredentialSource::ProviderEnv, CredentialMethod::ProviderEnv),
            (
                CredentialSource::ProviderKeychain,
                CredentialMethod::ProviderKeychain,
            ),
        ] {
            let facts = GrokAccountFacts::project(source, &observation, NOW);
            assert_eq!(facts.credential_method, expected);
            // No expiry evidence for these routes: honest, and non-blocking.
            assert_eq!(facts.readiness, AccountReadiness::Unknown);
            assert_eq!(facts.readiness_reason, ReadinessReason::ExpiryNotProvided);
            assert!(facts.permits_launch());
            assert_eq!(facts.validate(), Ok(()));
        }
    }

    #[test]
    fn oidc_route_variants_and_a_missing_auth_mode_map_to_the_oidc_method() {
        for mode in [None, Some("oidc"), Some("user"), Some("user_token")] {
            let facts = GrokAccountFacts::project(
                CredentialSource::GrokBuildSession,
                &AccountObservation {
                    auth_mode: mode,
                    ..AccountObservation::default()
                },
                NOW,
            );
            assert_eq!(
                facts.credential_method,
                CredentialMethod::GrokBuildOidc,
                "{mode:?} should route as OIDC"
            );
        }
    }

    #[test]
    fn account_identity_absent_drops_the_reference_without_inventing_one() {
        let document = serde_json::json!({
            "default": {
                "key": SENTINEL_BEARER,
                "refresh_token": SENTINEL_REFRESH,
                "email": "operator@example.test",
                "first_name": "Operator",
                "auth_mode": "oidc",
                "expires_at": "2027-01-01T00:00:00Z",
            }
        });
        let facts = GrokAccountFacts::from_auth_json(&document, NOW);
        assert_eq!(facts.account_reference, None);
        // Missing identity is not a usability signal on its own.
        assert_eq!(facts.readiness, AccountReadiness::Usable);
        assert!(facts.permits_launch());
        assert_no_credential_needles(&facts);
    }

    #[test]
    fn account_reference_prefers_durable_identity_in_a_fixed_order() {
        let facts = GrokAccountFacts::from_auth_json(&oidc_session(None), NOW);
        assert_eq!(
            facts.account_reference,
            Some(AccountReference {
                value: "usr-0a1b2c3d".into(),
                source: AccountReferenceSource::UserId,
            })
        );

        let principal = GrokAccountFacts::project(
            CredentialSource::GrokBuildSession,
            &AccountObservation {
                principal_id: Some("prn-1234"),
                team_id: Some("team-9z8y"),
                ..AccountObservation::default()
            },
            NOW,
        );
        assert_eq!(
            principal.account_reference,
            Some(AccountReference {
                value: "prn-1234".into(),
                source: AccountReferenceSource::PrincipalId,
            })
        );

        let team = GrokAccountFacts::project(
            CredentialSource::GrokBuildSession,
            &AccountObservation {
                team_id: Some("team-9z8y"),
                ..AccountObservation::default()
            },
            NOW,
        );
        assert_eq!(
            team.account_reference.map(|reference| reference.source),
            Some(AccountReferenceSource::TeamId)
        );
    }

    #[test]
    fn hostile_account_identity_is_rejected_rather_than_published() {
        for hostile in [
            "",
            "   ",
            "usr with space",
            "usr/../../etc/passwd",
            "<script>alert(1)</script>",
            "usr\u{0000}nul",
            "usr\nnewline",
            "operator@example.test",
            &"u".repeat(MAX_ACCOUNT_REFERENCE_BYTES + 1),
        ] {
            assert_eq!(
                AccountReference::new(hostile, AccountReferenceSource::UserId),
                None,
                "{hostile:?} must not become a public account reference"
            );
        }
        assert!(
            AccountReference::new(
                &"u".repeat(MAX_ACCOUNT_REFERENCE_BYTES),
                AccountReferenceSource::UserId
            )
            .is_some()
        );
    }

    #[test]
    fn malicious_free_form_auth_mode_collapses_to_unknown_and_never_leaks() {
        for hostile in [
            "oidc\"; DROP TABLE runs; --",
            "<img src=x onerror=alert(1)>",
            "oidc-but-not-really",
            "prefix_oidc_suffix",
            "API_KEY",
            "  oidc  ",
            "../../etc/passwd",
        ] {
            let document = serde_json::json!({
                "default": {
                    "key": SENTINEL_BEARER,
                    "auth_mode": hostile,
                    "user_id": "usr-0a1b2c3d",
                    "expires_at": "2027-01-01T00:00:00Z",
                }
            });
            let facts = GrokAccountFacts::from_auth_json(&document, NOW);
            assert_eq!(
                facts.credential_method,
                CredentialMethod::Unknown,
                "{hostile:?} must not be classified as a known route"
            );
            // Unrecognized never earns "usable", and never blocks either.
            assert_eq!(facts.readiness, AccountReadiness::Unknown);
            assert_eq!(facts.readiness_reason, ReadinessReason::MethodUnrecognized);
            assert!(facts.permits_launch());
            let encoded = serde_json::to_string(&facts).expect("facts serialize");
            assert!(
                !encoded.contains(hostile),
                "hostile auth_mode {hostile:?} was echoed: {encoded}"
            );
            assert_no_credential_needles(&facts);
        }
    }

    #[test]
    fn session_selection_prefers_a_live_record_over_an_expired_one() {
        let document = serde_json::json!({
            "aaa-expired": {
                "key": SENTINEL_BEARER,
                "auth_mode": "oidc",
                "user_id": "usr-expired",
                "expires_at": "2020-01-01T00:00:00Z",
            },
            "bbb-live": {
                "key": SENTINEL_BEARER,
                "auth_mode": "oidc",
                "user_id": "usr-live",
                "expires_at": "2027-01-01T00:00:00Z",
            }
        });
        let facts = GrokAccountFacts::from_auth_json(&document, NOW);
        assert_eq!(facts.readiness, AccountReadiness::Usable);
        assert_eq!(
            facts.account_reference.map(|reference| reference.value),
            Some("usr-live".into())
        );
    }

    #[test]
    fn all_expired_sessions_report_the_blocking_record() {
        let document = serde_json::json!({
            "only": {
                "key": SENTINEL_BEARER,
                "auth_mode": "oidc",
                "user_id": "usr-expired",
                "expires_at": "2020-01-01T00:00:00Z",
            }
        });
        let facts = GrokAccountFacts::from_auth_json(&document, NOW);
        assert_eq!(facts.readiness, AccountReadiness::Unusable);
        assert_eq!(facts.readiness_reason, ReadinessReason::CredentialExpired);
        assert!(!facts.permits_launch());
    }

    #[test]
    fn public_facts_serialize_with_camel_case_and_closed_vocabulary_values() {
        let facts =
            GrokAccountFacts::from_auth_json(&oidc_session(Some("2026-08-25T12:30:00Z")), NOW);
        let value = serde_json::to_value(&facts).expect("facts serialize");
        assert_eq!(value["contract"], GROK_ACCOUNT_CONTRACT_VERSION);
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["credentialMethod"], "grok_build_oidc");
        assert_eq!(value["accountReference"]["value"], "usr-0a1b2c3d");
        assert_eq!(value["accountReference"]["source"], "user_id");
        assert_eq!(value["expiry"]["status"], "valid");
        assert_eq!(value["expiry"]["expiresAt"], "2026-08-25T12:30:00Z");
        assert_eq!(value["expiry"]["secondsRemaining"], 45_000);
        assert_eq!(value["readiness"], "usable");
        assert_eq!(value["readinessReason"], "expiry_in_future");
        assert!(value.get("schema_version").is_none());
        assert!(value.get("credential_method").is_none());
        // Round-trips without loss.
        assert_eq!(
            serde_json::from_value::<GrokAccountFacts>(value).expect("facts decode"),
            facts
        );
    }

    #[test]
    fn attribution_is_bounded_and_claims_no_balance_or_certification() {
        let facts = GrokAccountFacts::from_auth_json(&oidc_session(None), NOW);
        let attribution = facts.attribution();
        assert_eq!(attribution.validate(), Ok(()));
        let value = serde_json::to_value(&attribution).expect("attribution serializes");
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("attribution is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["accountReference", "credentialMethod"]);
        for forbidden in [
            "balance",
            "quota",
            "credits",
            "entitlement",
            "certified",
            "certification",
            "tier",
            "plan",
            "limit",
        ] {
            assert!(
                !value.to_string().contains(forbidden),
                "attribution claimed {forbidden:?}"
            );
        }
    }

    #[test]
    fn attribution_rejects_an_out_of_band_account_reference() {
        let attribution = RunAttribution {
            credential_method: CredentialMethod::GrokBuildOidc,
            account_reference: Some(AccountReference {
                value: "usr with space".into(),
                source: AccountReferenceSource::UserId,
            }),
        };
        assert!(attribution.validate().is_err());
    }

    #[test]
    fn validate_rejects_a_readiness_verdict_that_does_not_follow_from_evidence() {
        let mut facts =
            GrokAccountFacts::from_auth_json(&oidc_session(Some("2026-08-24T23:59:59Z")), NOW);
        assert!(!facts.permits_launch());
        // A tampered projection must not talk its way into a launch.
        facts.readiness = AccountReadiness::Usable;
        facts.readiness_reason = ReadinessReason::ExpiryInFuture;
        assert!(facts.validate().is_err());
    }

    #[test]
    fn shared_golden_fixtures_agree_with_the_rust_projection() {
        let fixtures: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../docs/schemas/grokptah-account.v1.fixtures.json"
        ))
        .expect("account fixtures parse");
        assert_eq!(fixtures["observedAtUnix"].as_i64(), Some(NOW));

        let accepted = fixtures["accepted"]
            .as_array()
            .expect("fixtures declare accepted cases");
        assert!(accepted.len() >= 8, "golden coverage shrank");
        for case in accepted {
            let name = case["name"].as_str().expect("case is named");
            let facts: GrokAccountFacts = serde_json::from_value(case["facts"].clone())
                .unwrap_or_else(|error| panic!("{name} should decode: {error}"));
            assert_eq!(facts.validate(), Ok(()), "{name} should validate");
            assert_eq!(
                facts.permits_launch(),
                case["permitsLaunch"]
                    .as_bool()
                    .expect("case declares gating"),
                "{name} disagreed about launch gating"
            );
            // Re-serializing a golden case must reproduce it byte-for-byte in
            // structure, so the fixture stays an exact contract sample.
            assert_eq!(
                serde_json::to_value(&facts).expect("facts serialize"),
                case["facts"],
                "{name} did not round-trip"
            );
            assert_no_credential_needles(&facts);
        }

        let rejected = fixtures["rejected"]
            .as_array()
            .expect("fixtures declare rejected cases");
        assert!(rejected.len() >= 8, "golden coverage shrank");
        for case in rejected {
            let name = case["name"].as_str().expect("case is named");
            // Fail closed either at decode (unknown key, bad vocabulary) or at
            // validation (verdict or reference that does not hold up).
            let rejected_here =
                match serde_json::from_value::<GrokAccountFacts>(case["facts"].clone()) {
                    Err(_) => true,
                    Ok(facts) => facts.validate().is_err(),
                };
            assert!(rejected_here, "{name} was accepted but must fail closed");
        }
    }

    #[test]
    fn closed_vocabularies_match_the_v1_json_schema_enums() {
        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../docs/schemas/grokptah-account.v1.schema.json"
        ))
        .expect("account schema parses");
        let defs = &schema["$defs"];
        let enum_values = |name: &str| -> Vec<String> {
            defs[name]["enum"]
                .as_array()
                .unwrap_or_else(|| panic!("{name} declares an enum"))
                .iter()
                .map(|value| value.as_str().expect("enum values are strings").to_string())
                .collect()
        };
        assert_eq!(
            enum_values("credentialMethod"),
            CredentialMethod::ALL
                .iter()
                .map(|method| method.as_str().to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            enum_values("accountReferenceSource"),
            AccountReferenceSource::ALL
                .iter()
                .map(|source| source.as_str().to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            enum_values("expiryStatus"),
            ExpiryStatus::ALL
                .iter()
                .map(|status| status.as_str().to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            enum_values("accountReadiness"),
            AccountReadiness::ALL
                .iter()
                .map(|readiness| readiness.as_str().to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            enum_values("readinessReason"),
            ReadinessReason::ALL
                .iter()
                .map(|reason| reason.as_str().to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(schema["$id"], "urn:grokptah:schema:account:v1");
        assert_eq!(
            defs["accountReference"]["properties"]["value"]["maxLength"],
            MAX_ACCOUNT_REFERENCE_BYTES
        );
        // Every enum variant serializes to the string the schema pins.
        for method in CredentialMethod::ALL {
            assert_eq!(
                serde_json::to_value(method).expect("method serializes"),
                serde_json::Value::String(method.as_str().into())
            );
        }
        for status in ExpiryStatus::ALL {
            assert_eq!(
                serde_json::to_value(status).expect("status serializes"),
                serde_json::Value::String(status.as_str().into())
            );
        }
        for readiness in AccountReadiness::ALL {
            assert_eq!(
                serde_json::to_value(readiness).expect("readiness serializes"),
                serde_json::Value::String(readiness.as_str().into())
            );
        }
        for reason in ReadinessReason::ALL {
            assert_eq!(
                serde_json::to_value(reason).expect("reason serializes"),
                serde_json::Value::String(reason.as_str().into())
            );
        }
        for source in AccountReferenceSource::ALL {
            assert_eq!(
                serde_json::to_value(source).expect("source serializes"),
                serde_json::Value::String(source.as_str().into())
            );
        }
    }
}
