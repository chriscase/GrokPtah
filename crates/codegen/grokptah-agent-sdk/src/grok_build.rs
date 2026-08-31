//! Advisory Grok Build manager contract.
//!
//! This module is a share-safe validation layer for a future isolated Grok
//! Build manager. Documents that pass these types are **not** a manager or
//! authority implementation, not a CLI runner, not a provider call, and not
//! live qualification. Treat every accepted value as advisory until a manager
//! and host-authority implementation exist and record their own evidence.
//!
//! Unknown fields, unknown enumerations, nonempty-but-unsafe strings, and
//! oversized UTF-8 input fail closed.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Contract version stamped on isolation receipts.
pub const GROK_BUILD_CONTRACT_VERSION: &str = "1.0";

/// Maximum JSON document size accepted by this contract.
pub const MAX_DOCUMENT_BYTES: usize = 8_192;

/// Maximum UTF-8 byte length of an opaque identifier.
pub const MAX_OPAQUE_ID_BYTES: usize = 128;

/// Maximum UTF-8 byte length of a git ref name.
pub const MAX_GIT_REF_BYTES: usize = 128;

/// Maximum UTF-8 byte length of the isolated-home alias.
pub const MAX_ALIAS_BYTES: usize = 64;

/// Maximum prompt-byte ceiling a launch may request.
pub const MAX_PROMPT_BYTES: u64 = 65_536;

/// Maximum turn ceiling a launch may request.
pub const MAX_TURNS: u32 = 32;

/// Maximum wall-clock ceiling a launch may request, in milliseconds.
pub const MAX_DURATION_MS: u64 = 1_800_000;

/// Maximum number of evidence references on a result.
pub const MAX_EVIDENCE_REFS: usize = 8;

/// Maximum number of explicit nonclaims on a result.
pub const MAX_NONCLAIMS: usize = 8;

const GIT_SHA1_HEX_LEN: usize = 40;
const GIT_SHA256_HEX_LEN: usize = 64;

/// Fail-closed contract error. Messages are codes; they never echo input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GrokBuildContractError {
    #[error("invalid_request")]
    InvalidRequest,
    #[error("identity_mismatch")]
    IdentityMismatch,
    #[error("read_only_mutation")]
    ReadOnlyMutation,
    #[error("verdict_inconsistent")]
    VerdictInconsistent,
    #[error("missing_evidence_marker")]
    MissingEvidenceMarker,
}

/// Allowed mutation mode for an isolated Grok Build launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrokBuildMutationMode {
    ReadOnly,
    IsolatedReview,
}

/// MCP / hooks / instruction / plugin policy recorded on an isolation receipt.
///
/// Only fail-closed isolation values exist. `enabled` and other unknown states
/// are rejected by serde.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrokBuildPolicyState {
    Disabled,
    Omitted,
}

/// Cleanup recorded on an isolation receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrokBuildCleanupState {
    Pending,
    Complete,
    FailedClosed,
}

/// Lifecycle of an advisory Grok Build run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrokBuildRunState {
    Running,
    NeedsSynthesis,
    CompleteAdvisory,
    FailedClosed,
}

/// Terminal verdict. Absent until a state that may carry one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrokBuildVerdict {
    Clean,
    Findings,
    NotComplete,
}

/// Explicit statements of what a result does not claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrokBuildNonclaim {
    AdvisoryOnly,
    NotManagerImplementation,
    NotHostAuthority,
    NotProviderAccount,
    NotLiveQualified,
    NotMergeAuthority,
    NotComputerUse,
}

/// Exact repository / ref / base / head identity. Git object ids are lowercase
/// SHA-1 or SHA-256 hex so both repository formats can be represented.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrokBuildGitIdentity {
    pub repository_id: String,
    pub git_ref: String,
    pub base_sha: String,
    pub head_sha: String,
}

/// Launch request for a future isolated Grok Build manager.
///
/// The credential lease id is opaque. It is never a filesystem path and never
/// a provider token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrokBuildLaunchRequest {
    pub request_id: String,
    pub identity: GrokBuildGitIdentity,
    pub mutation_mode: GrokBuildMutationMode,
    pub max_prompt_bytes: u64,
    pub max_turns: u32,
    pub max_duration_ms: u64,
    pub credential_lease_id: String,
}

/// Isolation receipt. No paths, secrets, prompts, or provider accounts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrokBuildIsolationReceipt {
    pub contract_version: String,
    pub request_id: String,
    pub identity: GrokBuildGitIdentity,
    pub credential_lease_id: String,
    pub isolated_home_alias: String,
    pub mcp_policy: GrokBuildPolicyState,
    pub hooks_policy: GrokBuildPolicyState,
    pub instruction_policy: GrokBuildPolicyState,
    pub plugin_policy: GrokBuildPolicyState,
    pub permission_policy: GrokBuildMutationMode,
    pub credential_present: bool,
    pub permissions_ok: bool,
    pub cleanup_state: GrokBuildCleanupState,
}

/// Advisory run result. No stdout/stderr, credentials, or paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrokBuildResult {
    pub request_id: String,
    pub session_id: String,
    pub identity: GrokBuildGitIdentity,
    pub state: GrokBuildRunState,
    pub evidence_refs: Vec<String>,
    pub terminal_verdict: Option<GrokBuildVerdict>,
    pub nonclaims: Vec<GrokBuildNonclaim>,
}

impl GrokBuildGitIdentity {
    /// Fail closed unless repository, ref, and SHA-1/SHA-256 object ids are exact.
    pub fn validate(&self) -> Result<(), GrokBuildContractError> {
        validate_opaque_id(&self.repository_id, MAX_OPAQUE_ID_BYTES)?;
        validate_git_ref(&self.git_ref)?;
        validate_git_object_id(&self.base_sha)?;
        validate_git_object_id(&self.head_sha)?;
        Ok(())
    }
}

impl GrokBuildLaunchRequest {
    /// Decode JSON and validate. Unknown fields and malformed input fail closed.
    pub fn from_json_str(raw: &str) -> Result<Self, GrokBuildContractError> {
        let parsed: Self = decode_strict(raw)?;
        parsed.validate()?;
        Ok(parsed)
    }

    /// Decode a JSON value and validate.
    pub fn from_value(value: serde_json::Value) -> Result<Self, GrokBuildContractError> {
        let parsed: Self = decode_value(value)?;
        parsed.validate()?;
        Ok(parsed)
    }

    pub fn validate(&self) -> Result<(), GrokBuildContractError> {
        validate_opaque_id(&self.request_id, MAX_OPAQUE_ID_BYTES)?;
        self.identity.validate()?;
        validate_opaque_id(&self.credential_lease_id, MAX_OPAQUE_ID_BYTES)?;
        if self.max_prompt_bytes == 0 || self.max_prompt_bytes > MAX_PROMPT_BYTES {
            return Err(GrokBuildContractError::InvalidRequest);
        }
        if self.max_turns == 0 || self.max_turns > MAX_TURNS {
            return Err(GrokBuildContractError::InvalidRequest);
        }
        if self.max_duration_ms == 0 || self.max_duration_ms > MAX_DURATION_MS {
            return Err(GrokBuildContractError::InvalidRequest);
        }
        Ok(())
    }
}

impl GrokBuildIsolationReceipt {
    /// Decode JSON and validate. Unknown fields and malformed input fail closed.
    pub fn from_json_str(raw: &str) -> Result<Self, GrokBuildContractError> {
        let parsed: Self = decode_strict(raw)?;
        parsed.validate()?;
        Ok(parsed)
    }

    /// Decode a JSON value and validate.
    pub fn from_value(value: serde_json::Value) -> Result<Self, GrokBuildContractError> {
        let parsed: Self = decode_value(value)?;
        parsed.validate()?;
        Ok(parsed)
    }

    pub fn validate(&self) -> Result<(), GrokBuildContractError> {
        if self.contract_version != GROK_BUILD_CONTRACT_VERSION {
            return Err(GrokBuildContractError::InvalidRequest);
        }
        validate_opaque_id(&self.request_id, MAX_OPAQUE_ID_BYTES)?;
        self.identity.validate()?;
        validate_opaque_id(&self.credential_lease_id, MAX_OPAQUE_ID_BYTES)?;
        validate_opaque_id(&self.isolated_home_alias, MAX_ALIAS_BYTES)?;
        if self.mcp_policy != GrokBuildPolicyState::Disabled
            || self.hooks_policy != GrokBuildPolicyState::Disabled
            || self.plugin_policy != GrokBuildPolicyState::Disabled
        {
            return Err(GrokBuildContractError::InvalidRequest);
        }
        if self.instruction_policy != GrokBuildPolicyState::Omitted {
            return Err(GrokBuildContractError::InvalidRequest);
        }
        Ok(())
    }

    /// Receipt identity and permission must match the launch exactly, and its
    /// pre-launch isolation posture must be live rather than failed closed.
    pub fn validate_for_launch(
        &self,
        launch: &GrokBuildLaunchRequest,
    ) -> Result<(), GrokBuildContractError> {
        launch.validate()?;
        self.validate()?;
        if self.request_id != launch.request_id
            || self.identity != launch.identity
            || self.credential_lease_id != launch.credential_lease_id
        {
            return Err(GrokBuildContractError::IdentityMismatch);
        }
        match (launch.mutation_mode, self.permission_policy) {
            (GrokBuildMutationMode::ReadOnly, GrokBuildMutationMode::IsolatedReview) => {
                return Err(GrokBuildContractError::ReadOnlyMutation);
            }
            (left, right) if left != right => {
                return Err(GrokBuildContractError::InvalidRequest);
            }
            _ => {}
        }
        if !self.credential_present
            || !self.permissions_ok
            || self.cleanup_state != GrokBuildCleanupState::Pending
        {
            return Err(GrokBuildContractError::InvalidRequest);
        }
        Ok(())
    }
}

impl GrokBuildResult {
    /// Decode JSON and validate. Unknown fields and malformed input fail closed.
    pub fn from_json_str(raw: &str) -> Result<Self, GrokBuildContractError> {
        let parsed: Self = decode_strict(raw)?;
        parsed.validate()?;
        Ok(parsed)
    }

    /// Decode a JSON value and validate.
    pub fn from_value(value: serde_json::Value) -> Result<Self, GrokBuildContractError> {
        let parsed: Self = decode_value(value)?;
        parsed.validate()?;
        Ok(parsed)
    }

    pub fn validate(&self) -> Result<(), GrokBuildContractError> {
        validate_opaque_id(&self.request_id, MAX_OPAQUE_ID_BYTES)?;
        validate_opaque_id(&self.session_id, MAX_OPAQUE_ID_BYTES)?;
        self.identity.validate()?;
        if self.evidence_refs.len() > MAX_EVIDENCE_REFS {
            return Err(GrokBuildContractError::InvalidRequest);
        }
        let mut seen_evidence = Vec::new();
        for marker in &self.evidence_refs {
            validate_opaque_id(marker, MAX_OPAQUE_ID_BYTES)?;
            if seen_evidence.contains(&marker.as_str()) {
                return Err(GrokBuildContractError::InvalidRequest);
            }
            seen_evidence.push(marker.as_str());
        }
        if self.nonclaims.is_empty() || self.nonclaims.len() > MAX_NONCLAIMS {
            return Err(GrokBuildContractError::InvalidRequest);
        }
        let mut seen_nonclaims = Vec::new();
        for nonclaim in &self.nonclaims {
            if seen_nonclaims.contains(nonclaim) {
                return Err(GrokBuildContractError::InvalidRequest);
            }
            seen_nonclaims.push(*nonclaim);
        }
        let required_nonclaims = [
            GrokBuildNonclaim::AdvisoryOnly,
            GrokBuildNonclaim::NotManagerImplementation,
            GrokBuildNonclaim::NotHostAuthority,
            GrokBuildNonclaim::NotProviderAccount,
            GrokBuildNonclaim::NotLiveQualified,
            GrokBuildNonclaim::NotMergeAuthority,
            GrokBuildNonclaim::NotComputerUse,
        ];
        if required_nonclaims
            .iter()
            .any(|required| !self.nonclaims.contains(required))
        {
            return Err(GrokBuildContractError::InvalidRequest);
        }
        self.validate_state_verdict()?;
        Ok(())
    }

    /// Result identity must match the launch exactly. Use
    /// [`Self::validate_for_launch_and_receipt`] for lifecycle admission.
    pub fn validate_for_launch(
        &self,
        launch: &GrokBuildLaunchRequest,
    ) -> Result<(), GrokBuildContractError> {
        launch.validate()?;
        self.validate()?;
        if self.request_id != launch.request_id || self.identity != launch.identity {
            return Err(GrokBuildContractError::IdentityMismatch);
        }
        Ok(())
    }

    /// Validate the only supported launch/receipt/result lifecycle tuple. The
    /// receipt must still originate from a trusted isolation host; this
    /// advisory schema does not mint authority.
    pub fn validate_for_launch_and_receipt(
        &self,
        launch: &GrokBuildLaunchRequest,
        receipt: &GrokBuildIsolationReceipt,
    ) -> Result<(), GrokBuildContractError> {
        launch.validate()?;
        receipt.validate()?;
        self.validate_for_launch(launch)?;
        if launch.mutation_mode == GrokBuildMutationMode::ReadOnly
            && receipt.permission_policy == GrokBuildMutationMode::IsolatedReview
        {
            return Err(GrokBuildContractError::ReadOnlyMutation);
        }
        if receipt.request_id != launch.request_id
            || receipt.identity != launch.identity
            || receipt.credential_lease_id != launch.credential_lease_id
            || receipt.permission_policy != launch.mutation_mode
        {
            return Err(GrokBuildContractError::IdentityMismatch);
        }
        match self.state {
            GrokBuildRunState::Running | GrokBuildRunState::NeedsSynthesis => {
                if !receipt.credential_present
                    || !receipt.permissions_ok
                    || receipt.cleanup_state != GrokBuildCleanupState::Pending
                {
                    return Err(GrokBuildContractError::InvalidRequest);
                }
            }
            GrokBuildRunState::CompleteAdvisory => {
                if !receipt.credential_present
                    || !receipt.permissions_ok
                    || receipt.cleanup_state != GrokBuildCleanupState::Complete
                {
                    return Err(GrokBuildContractError::InvalidRequest);
                }
            }
            GrokBuildRunState::FailedClosed => {
                if receipt.cleanup_state != GrokBuildCleanupState::FailedClosed {
                    return Err(GrokBuildContractError::InvalidRequest);
                }
            }
        }
        Ok(())
    }

    fn validate_state_verdict(&self) -> Result<(), GrokBuildContractError> {
        match (self.state, self.terminal_verdict) {
            (
                GrokBuildRunState::Running
                | GrokBuildRunState::NeedsSynthesis
                | GrokBuildRunState::FailedClosed,
                Some(_),
            ) => Err(GrokBuildContractError::VerdictInconsistent),
            (
                GrokBuildRunState::Running
                | GrokBuildRunState::NeedsSynthesis
                | GrokBuildRunState::FailedClosed,
                None,
            ) => Ok(()),
            (
                GrokBuildRunState::CompleteAdvisory,
                Some(
                    GrokBuildVerdict::Clean
                    | GrokBuildVerdict::Findings
                    | GrokBuildVerdict::NotComplete,
                ),
            ) if !self.evidence_refs.is_empty() => Ok(()),
            (GrokBuildRunState::CompleteAdvisory, Some(_)) => {
                Err(GrokBuildContractError::MissingEvidenceMarker)
            }
            (GrokBuildRunState::CompleteAdvisory, None) => {
                Err(GrokBuildContractError::VerdictInconsistent)
            }
        }
    }
}

fn decode_strict<T: DeserializeOwned>(raw: &str) -> Result<T, GrokBuildContractError> {
    if raw.len() > MAX_DOCUMENT_BYTES || raw.as_bytes().contains(&0) {
        return Err(GrokBuildContractError::InvalidRequest);
    }
    serde_json::from_str(raw).map_err(|_| GrokBuildContractError::InvalidRequest)
}

fn decode_value<T: DeserializeOwned>(
    value: serde_json::Value,
) -> Result<T, GrokBuildContractError> {
    let encoded =
        serde_json::to_string(&value).map_err(|_| GrokBuildContractError::InvalidRequest)?;
    if encoded.len() > MAX_DOCUMENT_BYTES {
        return Err(GrokBuildContractError::InvalidRequest);
    }
    serde_json::from_value(value).map_err(|_| GrokBuildContractError::InvalidRequest)
}

fn validate_opaque_id(value: &str, max_bytes: usize) -> Result<(), GrokBuildContractError> {
    if value.is_empty() || value.len() > max_bytes {
        return Err(GrokBuildContractError::InvalidRequest);
    }
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(GrokBuildContractError::InvalidRequest);
    };
    if !first.is_ascii_alphanumeric() {
        return Err(GrokBuildContractError::InvalidRequest);
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':')) {
        return Err(GrokBuildContractError::InvalidRequest);
    }
    reject_path_or_secret(value)
}

fn validate_git_ref(value: &str) -> Result<(), GrokBuildContractError> {
    if value.is_empty() || value.len() > MAX_GIT_REF_BYTES {
        return Err(GrokBuildContractError::InvalidRequest);
    }
    if value.starts_with('/')
        || value.ends_with('/')
        || value.starts_with('.')
        || value.ends_with('.')
        || value.contains("..")
        || value.contains("//")
        || value.ends_with(".lock")
    {
        return Err(GrokBuildContractError::InvalidRequest);
    }
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(GrokBuildContractError::InvalidRequest);
    };
    if !first.is_ascii_alphanumeric() {
        return Err(GrokBuildContractError::InvalidRequest);
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/')) {
        return Err(GrokBuildContractError::InvalidRequest);
    }
    if value.split('/').any(|component| {
        component.is_empty() || component.starts_with('.') || component.ends_with(".lock")
    }) {
        return Err(GrokBuildContractError::InvalidRequest);
    }
    reject_path_or_secret(value)
}

fn validate_git_object_id(value: &str) -> Result<(), GrokBuildContractError> {
    if !matches!(value.len(), GIT_SHA1_HEX_LEN | GIT_SHA256_HEX_LEN) {
        return Err(GrokBuildContractError::InvalidRequest);
    }
    if !value
        .bytes()
        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(GrokBuildContractError::InvalidRequest);
    }
    if value.bytes().all(|b| b == b'0') {
        return Err(GrokBuildContractError::InvalidRequest);
    }
    Ok(())
}

fn reject_path_or_secret(value: &str) -> Result<(), GrokBuildContractError> {
    if looks_like_path(value) || looks_like_secret(value) || looks_like_account(value) {
        Err(GrokBuildContractError::InvalidRequest)
    } else {
        Ok(())
    }
}

fn looks_like_path(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    value.starts_with('/')
        || value.starts_with('\\')
        || value.starts_with('~')
        || value.contains('\\')
        || value.contains("..")
        || lower.contains("file:")
        || lower.contains("/tmp/")
        || lower.contains("/var/")
        || lower.contains("/home/")
        || lower.contains("/users/")
        || lower.contains("/etc/")
        || lower.contains(".ssh")
        || lower.contains(".env")
        || drive_path(value)
}

fn drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes.get(2), None | Some(b'/' | b'\\'))
}

fn looks_like_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("bearer")
        || lower.contains("sk-")
        || lower.contains("api_key")
        || lower.contains("apikey")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.contains("private-key")
        || lower.contains("private_key")
        || lower.contains("begin ")
        || lower.contains("ghp_")
        || lower.contains("gho_")
        || lower.contains("github_pat")
        || lower.contains("token")
}

fn looks_like_account(value: &str) -> bool {
    value.contains('@') || value.to_ascii_lowercase().contains("account")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const BASE_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HEAD_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn identity_json() -> Value {
        json!({
            "repository_id": "repo-acme",
            "git_ref": "refs/heads/topic",
            "base_sha": BASE_SHA,
            "head_sha": HEAD_SHA
        })
    }

    fn launch_json() -> Value {
        json!({
            "request_id": "req-1",
            "identity": identity_json(),
            "mutation_mode": "isolated_review",
            "max_prompt_bytes": 4096,
            "max_turns": 8,
            "max_duration_ms": 60_000,
            "credential_lease_id": "lease-1"
        })
    }

    fn receipt_json() -> Value {
        json!({
            "contract_version": GROK_BUILD_CONTRACT_VERSION,
            "request_id": "req-1",
            "identity": identity_json(),
            "credential_lease_id": "lease-1",
            "isolated_home_alias": "iso-home-1",
            "mcp_policy": "disabled",
            "hooks_policy": "disabled",
            "instruction_policy": "omitted",
            "plugin_policy": "disabled",
            "permission_policy": "isolated_review",
            "credential_present": true,
            "permissions_ok": true,
            "cleanup_state": "pending"
        })
    }

    fn result_json() -> Value {
        json!({
            "request_id": "req-1",
            "session_id": "sess-1",
            "identity": identity_json(),
            "state": "complete_advisory",
            "evidence_refs": ["advisory-summary"],
            "terminal_verdict": "clean",
            "nonclaims": [
                "advisory_only",
                "not_manager_implementation",
                "not_host_authority",
                "not_provider_account",
                "not_live_qualified",
                "not_merge_authority",
                "not_computer_use"
            ]
        })
    }

    fn parse_launch(value: Value) -> Result<GrokBuildLaunchRequest, GrokBuildContractError> {
        GrokBuildLaunchRequest::from_value(value)
    }

    fn parse_receipt(value: Value) -> Result<GrokBuildIsolationReceipt, GrokBuildContractError> {
        GrokBuildIsolationReceipt::from_value(value)
    }

    fn parse_result(value: Value) -> Result<GrokBuildResult, GrokBuildContractError> {
        GrokBuildResult::from_value(value)
    }

    #[test]
    fn valid_documents_round_trip() {
        let launch = parse_launch(launch_json()).expect("launch");
        let receipt = parse_receipt(receipt_json()).expect("receipt");
        let result = parse_result(result_json()).expect("result");
        receipt
            .validate_for_launch(&launch)
            .expect("receipt matches");
        result.validate_for_launch(&launch).expect("result matches");
        let mut completed_receipt_value = receipt_json();
        completed_receipt_value["cleanup_state"] = json!("complete");
        let completed_receipt = parse_receipt(completed_receipt_value).expect("completed receipt");
        result
            .validate_for_launch_and_receipt(&launch, &completed_receipt)
            .expect("terminal lifecycle tuple matches");

        let launch_again = GrokBuildLaunchRequest::from_json_str(
            &serde_json::to_string(&launch).expect("launch json"),
        )
        .expect("launch round trip");
        let receipt_again = GrokBuildIsolationReceipt::from_json_str(
            &serde_json::to_string(&receipt).expect("receipt json"),
        )
        .expect("receipt round trip");
        let result_again =
            GrokBuildResult::from_json_str(&serde_json::to_string(&result).expect("result json"))
                .expect("result round trip");
        assert_eq!(launch, launch_again);
        assert_eq!(receipt, receipt_again);
        assert_eq!(result, result_again);

        let read_only_launch = parse_launch(json!({
            "request_id": "req-1",
            "identity": identity_json(),
            "mutation_mode": "read_only",
            "max_prompt_bytes": 1024,
            "max_turns": 4,
            "max_duration_ms": 30_000,
            "credential_lease_id": "lease-1"
        }))
        .expect("read-only launch");
        let read_only_receipt = parse_receipt(json!({
            "contract_version": GROK_BUILD_CONTRACT_VERSION,
            "request_id": "req-1",
            "identity": identity_json(),
            "credential_lease_id": "lease-1",
            "isolated_home_alias": "iso-home-1",
            "mcp_policy": "disabled",
            "hooks_policy": "disabled",
            "instruction_policy": "omitted",
            "plugin_policy": "disabled",
            "permission_policy": "read_only",
            "credential_present": true,
            "permissions_ok": true,
            "cleanup_state": "pending"
        }))
        .expect("read-only receipt");
        read_only_receipt
            .validate_for_launch(&read_only_launch)
            .expect("read-only pair");
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let mut launch = launch_json();
        launch["stdout"] = json!("secret-log");
        assert_eq!(
            parse_launch(launch),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut receipt = receipt_json();
        receipt["prompt"] = json!("review the private key");
        assert_eq!(
            parse_receipt(receipt),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut result = result_json();
        result["stderr"] = json!("trace");
        assert_eq!(
            parse_result(result),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut identity = identity_json();
        identity["cwd"] = json!("/tmp/project");
        let mut launch_cwd = launch_json();
        launch_cwd["identity"] = identity;
        assert_eq!(
            parse_launch(launch_cwd),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut enabled = receipt_json();
        enabled["mcp_policy"] = json!("enabled");
        assert_eq!(
            parse_receipt(enabled),
            Err(GrokBuildContractError::InvalidRequest)
        );
    }

    #[test]
    fn paths_and_secrets_are_rejected() {
        let mut lease_path = launch_json();
        lease_path["credential_lease_id"] = json!("/tmp/secret-token");
        assert_eq!(
            parse_launch(lease_path),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut lease_secret = launch_json();
        lease_secret["credential_lease_id"] = json!("Bearer-sk-live-example");
        assert_eq!(
            parse_launch(lease_secret),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut repo_path = launch_json();
        repo_path["identity"]["repository_id"] = json!("/Users/dev/project");
        assert_eq!(
            parse_launch(repo_path),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut alias_path = receipt_json();
        alias_path["isolated_home_alias"] = json!("~/.grokptah");
        assert_eq!(
            parse_receipt(alias_path),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut alias_account = receipt_json();
        alias_account["isolated_home_alias"] = json!("user@x.ai");
        assert_eq!(
            parse_receipt(alias_account),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut evidence_path = result_json();
        evidence_path["evidence_refs"] = json!(["/tmp/stdout.log"]);
        assert_eq!(
            parse_result(evidence_path),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut session_secret = result_json();
        session_secret["session_id"] = json!("api_key-live");
        assert_eq!(
            parse_result(session_secret),
            Err(GrokBuildContractError::InvalidRequest)
        );
    }

    #[test]
    fn bounds_are_enforced() {
        let mut empty_id = launch_json();
        empty_id["request_id"] = json!("");
        assert_eq!(
            parse_launch(empty_id),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut oversized_id = launch_json();
        oversized_id["request_id"] = json!("r".repeat(MAX_OPAQUE_ID_BYTES + 1));
        assert_eq!(
            parse_launch(oversized_id),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut prompt_zero = launch_json();
        prompt_zero["max_prompt_bytes"] = json!(0);
        assert_eq!(
            parse_launch(prompt_zero),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut prompt_over = launch_json();
        prompt_over["max_prompt_bytes"] = json!(MAX_PROMPT_BYTES + 1);
        assert_eq!(
            parse_launch(prompt_over),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut turns_over = launch_json();
        turns_over["max_turns"] = json!(MAX_TURNS + 1);
        assert_eq!(
            parse_launch(turns_over),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut duration_over = launch_json();
        duration_over["max_duration_ms"] = json!(MAX_DURATION_MS + 1);
        assert_eq!(
            parse_launch(duration_over),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut short_sha = launch_json();
        short_sha["identity"]["head_sha"] = json!("abc123");
        assert_eq!(
            parse_launch(short_sha),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut sha1 = launch_json();
        sha1["identity"]["head_sha"] = json!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        parse_launch(sha1).expect("SHA-1 repositories remain representable");

        let mut upper_sha = launch_json();
        upper_sha["identity"]["base_sha"] = json!(BASE_SHA.to_ascii_uppercase());
        assert_eq!(
            parse_launch(upper_sha),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut zero_sha = launch_json();
        zero_sha["identity"]["base_sha"] = json!("0".repeat(64));
        assert_eq!(
            parse_launch(zero_sha),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut too_many_evidence = result_json();
        too_many_evidence["evidence_refs"] = json!(
            (0..=MAX_EVIDENCE_REFS)
                .map(|i| format!("ev-{i}"))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            parse_result(too_many_evidence),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let oversized = format!("{{\"request_id\":\"{}\"}}", "x".repeat(MAX_DOCUMENT_BYTES));
        assert_eq!(
            GrokBuildLaunchRequest::from_json_str(&oversized),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut bad_version = receipt_json();
        bad_version["contract_version"] = json!("2.0");
        assert_eq!(
            parse_receipt(bad_version),
            Err(GrokBuildContractError::InvalidRequest)
        );
    }

    #[test]
    fn identity_mismatch_is_rejected() {
        let launch = parse_launch(launch_json()).expect("launch");
        let mut mismatched = result_json();
        mismatched["identity"]["head_sha"] =
            json!("cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc");
        let result = parse_result(mismatched).expect("result still well-formed");
        assert_eq!(
            result.validate_for_launch(&launch),
            Err(GrokBuildContractError::IdentityMismatch)
        );

        let mut other_request = result_json();
        other_request["request_id"] = json!("req-2");
        let result = parse_result(other_request).expect("result");
        assert_eq!(
            result.validate_for_launch(&launch),
            Err(GrokBuildContractError::IdentityMismatch)
        );
    }

    #[test]
    fn read_only_mode_rejects_isolated_review_permission() {
        let launch = parse_launch(json!({
            "request_id": "req-1",
            "identity": identity_json(),
            "mutation_mode": "read_only",
            "max_prompt_bytes": 1024,
            "max_turns": 2,
            "max_duration_ms": 15_000,
            "credential_lease_id": "lease-1"
        }))
        .expect("read-only launch");
        let receipt = parse_receipt(receipt_json()).expect("isolated-review receipt");
        assert_eq!(
            receipt.validate_for_launch(&launch),
            Err(GrokBuildContractError::ReadOnlyMutation)
        );

        let mut terminal_receipt = receipt_json();
        terminal_receipt["cleanup_state"] = json!("complete");
        let terminal_receipt = parse_receipt(terminal_receipt).expect("terminal receipt");
        let result = parse_result(result_json()).expect("result");
        assert_eq!(
            result.validate_for_launch_and_receipt(&launch, &terminal_receipt),
            Err(GrokBuildContractError::ReadOnlyMutation)
        );

        let isolated_launch = parse_launch(launch_json()).expect("isolated launch");
        for field in ["request_id", "credential_lease_id"] {
            let mut mismatch = receipt_json();
            mismatch[field] = json!("other-1");
            let mismatch = parse_receipt(mismatch).expect("well-formed mismatch");
            assert_eq!(
                mismatch.validate_for_launch(&isolated_launch),
                Err(GrokBuildContractError::IdentityMismatch)
            );
        }
    }

    #[test]
    fn needs_synthesis_rejects_terminal_verdict() {
        let mut value = result_json();
        value["state"] = json!("needs_synthesis");
        value["terminal_verdict"] = json!("clean");
        let mut parsed: GrokBuildResult =
            serde_json::from_value(value).expect("shape is otherwise valid");
        assert_eq!(
            parsed.validate(),
            Err(GrokBuildContractError::VerdictInconsistent)
        );

        parsed.terminal_verdict = None;
        parsed.validate().expect("needs_synthesis with no verdict");
    }

    #[test]
    fn lifecycle_tuple_binds_request_identity_lease_and_cleanup() {
        let launch = parse_launch(launch_json()).expect("launch");
        let result = parse_result(result_json()).expect("result");

        let mut complete_value = receipt_json();
        complete_value["cleanup_state"] = json!("complete");
        let complete = parse_receipt(complete_value).expect("complete receipt");
        result
            .validate_for_launch_and_receipt(&launch, &complete)
            .expect("exact terminal tuple");

        for field in ["request_id", "credential_lease_id"] {
            let mut mismatch = receipt_json();
            mismatch[field] = json!("other-1");
            mismatch["cleanup_state"] = json!("complete");
            let receipt = parse_receipt(mismatch).expect("well-formed mismatch");
            assert_eq!(
                result.validate_for_launch_and_receipt(&launch, &receipt),
                Err(GrokBuildContractError::IdentityMismatch)
            );
        }

        let mut wrong_identity = receipt_json();
        wrong_identity["identity"]["head_sha"] =
            json!("cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc");
        wrong_identity["cleanup_state"] = json!("complete");
        let receipt = parse_receipt(wrong_identity).expect("well-formed identity mismatch");
        assert_eq!(
            result.validate_for_launch_and_receipt(&launch, &receipt),
            Err(GrokBuildContractError::IdentityMismatch)
        );

        let pending = parse_receipt(receipt_json()).expect("pending receipt");
        assert_eq!(
            result.validate_for_launch_and_receipt(&launch, &pending),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut failed_result_value = result_json();
        failed_result_value["state"] = json!("failed_closed");
        failed_result_value["terminal_verdict"] = Value::Null;
        let failed_result = parse_result(failed_result_value).expect("failed result");
        let mut failed_receipt_value = receipt_json();
        failed_receipt_value["credential_present"] = json!(false);
        failed_receipt_value["permissions_ok"] = json!(false);
        failed_receipt_value["cleanup_state"] = json!("failed_closed");
        let failed_receipt = parse_receipt(failed_receipt_value).expect("failed receipt");
        failed_result
            .validate_for_launch_and_receipt(&launch, &failed_receipt)
            .expect("failed-closed tuple remains representable without admission");

        let mut mismatched_permission = receipt_json();
        mismatched_permission["permission_policy"] = json!("read_only");
        mismatched_permission["cleanup_state"] = json!("complete");
        let mismatched_permission =
            parse_receipt(mismatched_permission).expect("permission mismatch receipt");
        assert_eq!(
            result.validate_for_launch_and_receipt(&launch, &mismatched_permission),
            Err(GrokBuildContractError::IdentityMismatch)
        );
    }

    #[test]
    fn failed_isolation_cannot_admit_and_qualification_cannot_be_self_attested() {
        let launch = parse_launch(launch_json()).expect("launch");
        for (field, value) in [
            ("credential_present", json!(false)),
            ("permissions_ok", json!(false)),
            ("cleanup_state", json!("failed_closed")),
        ] {
            let mut failed = receipt_json();
            failed[field] = value;
            let receipt = parse_receipt(failed).expect("failed posture is observable");
            assert_eq!(
                receipt.validate_for_launch(&launch),
                Err(GrokBuildContractError::InvalidRequest)
            );
        }

        let mut self_attested = result_json();
        self_attested["state"] = json!("independently_qualified");
        assert_eq!(
            parse_result(self_attested),
            Err(GrokBuildContractError::InvalidRequest)
        );

        let mut no_evidence = result_json();
        no_evidence["evidence_refs"] = json!([]);
        assert_eq!(
            parse_result(no_evidence),
            Err(GrokBuildContractError::MissingEvidenceMarker)
        );

        let mut missing_nonclaim = result_json();
        missing_nonclaim["nonclaims"] = json!([
            "advisory_only",
            "not_manager_implementation",
            "not_host_authority",
            "not_provider_account",
            "not_live_qualified",
            "not_merge_authority"
        ]);
        assert_eq!(
            parse_result(missing_nonclaim),
            Err(GrokBuildContractError::InvalidRequest)
        );
    }

    #[test]
    fn types_are_share_safe() {
        fn assert_share<T: Clone + Send + Sync + 'static>() {}
        assert_share::<GrokBuildLaunchRequest>();
        assert_share::<GrokBuildIsolationReceipt>();
        assert_share::<GrokBuildResult>();
        assert_share::<GrokBuildGitIdentity>();
        assert_share::<GrokBuildRunState>();
        assert_share::<GrokBuildVerdict>();
        assert_share::<GrokBuildNonclaim>();
        assert_share::<GrokBuildContractError>();
    }
}
