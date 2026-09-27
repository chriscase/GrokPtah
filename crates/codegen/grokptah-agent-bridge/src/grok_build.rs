//! Bounded local process adapter for the advisory Grok Build contract.
//!
//! This module executes a validated [`GrokBuildLaunchRequest`] against an
//! allowlisted local CLI. It is not a manager, not host authority, not a
//! provider account, not live qualification, not merge authority, and not
//! Computer Use authority. The orchestration service may invoke it as a
//! bounded, proposal-only managed Work executor after host-owned policy and
//! Work/lane fences admit the invocation.
//!
//! Credential material is never accepted as a raw token. A host-injected
//! lease resolver may return only a path/lease handle; this adapter copies
//! that file blindly into a private `GROK_HOME` and never retains, logs, or
//! returns the contents.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub use grokptah_agent_sdk::{
    GrokBuildCleanupState, GrokBuildContractError, GrokBuildGitIdentity, GrokBuildIsolationReceipt,
    GrokBuildLaunchRequest, GrokBuildMutationMode, GrokBuildNonclaim, GrokBuildPolicyState,
    GrokBuildResult, GrokBuildRunState, GrokBuildVerdict, GROK_BUILD_CONTRACT_VERSION,
};

/// Allowlisted child `PATH`. Not inherited from the host process.
const BOUNDED_PATH: &str = "/usr/bin:/bin:/usr/local/bin";
const ISOLATED_CONFIG: &str = "[compat.claude]\nskills = false\nrules = false\nagents = false\nmcps = false\nhooks = false\nsessions = false\n\n[compat.cursor]\nskills = false\nrules = false\nagents = false\nmcps = false\nhooks = false\nsessions = false\n\n[compat.codex]\nskills = false\nrules = false\nagents = false\nmcps = false\nhooks = false\nsessions = false\n\n[marketplace]\nofficial_marketplace_auto_installed = false\ndefault_skills_installs_purged = true\n";
const SANDBOX_CONFIG: &str =
    "[profiles.grokptah_read_only]\nextends = \"read-only\"\nrestrict_network = true\n";
const INSPECT_OUTPUT_MAX: usize = 262_144;
const GIT_OUTPUT_MAX: usize = 32_768;
const LEASE_FILE_MAX: u64 = 1_048_576;
const SESSION_EVIDENCE_MAX: u64 = 8_388_608;
const ADVISORY_SUMMARY_MAX: usize = 262_144;
const OUTPUT_BYTES_MAX: usize = 4_194_304;
const HOME_ALIAS_MAX: usize = 64;
const AUTH_FILE_NAME: &str = "auth.json";
const PROMPT_FILE_NAME: &str = "prompt";
const CONFIG_FILE_NAME: &str = "config.toml";
const SANDBOX_FILE_NAME: &str = "sandbox.toml";
#[cfg(target_os = "macos")]
const MACOS_MUTATION_SANDBOX: &str = r#"(version 1)
(allow default)
(deny file-write*)
(allow file-write*
  (subpath (param "ISOLATE_ROOT"))
  (literal "/dev/null"))
"#;

/// Production children have no operator-home or external network authority.
/// Apple's minimal dyld bootstrap is needed on current macOS snapshot boots.
#[cfg(target_os = "macos")]
const MACOS_MANAGED_SANDBOX: &str = r#"(version 1)
(deny default)
(import "dyld-support.sb")
(allow process-exec process-fork)
(allow signal (target self))
(allow sysctl-read)
(deny sysctl-read (sysctl-name-regex #"^kern\.proc"))
(allow file-read-metadata)
(allow file-read*
  (subpath (param "CANDIDATE")) (subpath (param "CHILD_HOME")) (literal (param "CLI"))
  (subpath "/System/Library") (subpath "/System/Cryptexes") (subpath "/usr/lib") (subpath "/usr/share")
  (subpath "/usr/bin") (subpath "/bin")
  (literal "/dev/null") (literal "/dev/urandom") (literal "/dev/random")
  (literal "/private/etc/localtime") (literal "/private/var/db/timezone/localtime"))
 (allow file-read* (literal "/private/etc/ssl/openssl.cnf"))
(allow file-write* (subpath (param "CANDIDATE")) (subpath (param "CHILD_HOME")) (literal "/dev/null"))
(allow network-outbound (remote ip (param "RELAY")))
"#;

const VERDICT_CLEAN: &[u8] = b"GROK_BUILD_VERDICT=clean";
const VERDICT_FINDINGS: &[u8] = b"GROK_BUILD_VERDICT=findings";
const VERDICT_NOT_COMPLETE: &[u8] = b"GROK_BUILD_VERDICT=not_complete";
const MAX_TURNS_TOKEN: &[u8] = b"max_turns_reached";
const MAX_TURNS_PLAIN: &[u8] = b"Max turns reached";

/// Fail-closed adapter error. Display text is a stable code; it never echoes
/// paths, prompts, stdout, credentials, or provider text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GrokBuildAdapterError {
    #[error("invalid_request")]
    InvalidRequest,
    #[error("identity_mismatch")]
    IdentityMismatch,
    #[error("read_only_mutation")]
    ReadOnlyMutation,
    #[error("dirty_tree")]
    DirtyTree,
    #[error("spawn_failed")]
    SpawnFailed,
    #[error("timeout")]
    Timeout,
    #[error("cancelled")]
    Cancelled,
    #[error("output_overflow")]
    OutputOverflow,
    #[error("credential_lease")]
    CredentialLease,
    #[error("credential_revocation")]
    CredentialRevocation,
    #[error("isolation_failed")]
    IsolationFailed,
    #[error("termination_unproven")]
    TerminationUnproven,
}

/// Host-only launch configuration. Callers cannot supply extra CLI flags or
/// extra child environment entries through this type.
#[derive(Clone)]
pub struct GrokBuildHostLaunchConfig {
    /// Exact Grok Build CLI executable. No extra argv is accepted here.
    pub executable: PathBuf,
    /// Exact git executable used for the pre-launch identity gate.
    pub git_executable: PathBuf,
    /// Working directory that must already be the repository toplevel.
    pub cwd: PathBuf,
    /// Opaque repository id; must match the launch request exactly.
    pub repository_id: String,
    /// Already-local rev that must resolve to the launch base SHA. Never fetched.
    pub base_ref: String,
    /// Prompt body written to the isolated home. Never inherited from argv/env.
    pub prompt: String,
    /// Exact normalized workspace-relative files the isolated review may
    /// change. Empty is never valid for IsolatedReview.
    pub allowed_files: Vec<String>,
    /// Host-owned proof that the canonical work authority approved physical
    /// tool execution for this exact launch. Headless Grok requires a
    /// noninteractive permission mode, so the adapter refuses to construct
    /// that argv unless this bit is present.
    pub execution_approved: bool,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
    pub git_timeout: Duration,
    /// Parent directory for the task-scoped isolated `GROK_HOME`.
    pub isolate_parent: PathBuf,
    /// When true, a verified isolated mutation is retained for operator
    /// review and is not written into the source workspace.
    pub defer_source_apply: bool,
    /// Host-private directory that receives the candidate snapshot when
    /// `defer_source_apply` is set. Never forwarded to the child.
    pub candidate_retention_dir: Option<PathBuf>,
}

impl fmt::Debug for GrokBuildHostLaunchConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrokBuildHostLaunchConfig")
            .field("executable_absolute", &self.executable.is_absolute())
            .field(
                "git_executable_absolute",
                &self.git_executable.is_absolute(),
            )
            .field("cwd_absolute", &self.cwd.is_absolute())
            .field("prompt_bytes", &self.prompt.len())
            .field("allowed_file_count", &self.allowed_files.len())
            .field("execution_approved", &self.execution_approved)
            .field("max_stdout_bytes", &self.max_stdout_bytes)
            .field("defer_source_apply", &self.defer_source_apply)
            .field(
                "candidate_retained",
                &self.candidate_retention_dir.is_some(),
            )
            .field("max_stderr_bytes", &self.max_stderr_bytes)
            .finish_non_exhaustive()
    }
}

impl GrokBuildHostLaunchConfig {
    fn validate(&self) -> Result<(), GrokBuildAdapterError> {
        require_absolute_file_path(&self.executable)?;
        require_absolute_file_path(&self.git_executable)?;
        require_absolute_dir(&self.cwd)?;
        require_absolute_dir(&self.isolate_parent)?;
        validate_isolation_boundary(&self.cwd, &self.isolate_parent)?;
        if self.repository_id.is_empty() || self.base_ref.is_empty() {
            return Err(GrokBuildAdapterError::InvalidRequest);
        }
        if self.base_ref.starts_with('-')
            || self.base_ref.contains("..")
            || self
                .base_ref
                .chars()
                .any(|c| c.is_whitespace() || c == '\0')
        {
            return Err(GrokBuildAdapterError::InvalidRequest);
        }
        if self.prompt.is_empty() || self.prompt.as_bytes().contains(&0) {
            return Err(GrokBuildAdapterError::InvalidRequest);
        }
        validate_allowed_files(&self.allowed_files)?;
        if !self.execution_approved {
            return Err(GrokBuildAdapterError::InvalidRequest);
        }
        if self.max_stdout_bytes == 0
            || self.max_stderr_bytes == 0
            || self.max_stdout_bytes > OUTPUT_BYTES_MAX
            || self.max_stderr_bytes > OUTPUT_BYTES_MAX
        {
            return Err(GrokBuildAdapterError::InvalidRequest);
        }
        if self.git_timeout.is_zero() {
            return Err(GrokBuildAdapterError::InvalidRequest);
        }
        if self.defer_source_apply {
            let Some(retention) = &self.candidate_retention_dir else {
                return Err(GrokBuildAdapterError::InvalidRequest);
            };
            if !retention.is_absolute() {
                return Err(GrokBuildAdapterError::InvalidRequest);
            }
        }
        Ok(())
    }
}

/// Opaque lease handle. The contained location is never shown in Debug.
pub struct CredentialLeaseHandle {
    location: PathBuf,
    managed: Option<(crate::managed_provider::ManagedChildPolicy, String)>,
}

impl CredentialLeaseHandle {
    /// Host-only constructor. The adapter copies but never interprets contents.
    pub fn from_host_path(path: PathBuf) -> Self {
        Self {
            location: path,
            managed: None,
        }
    }

    pub(crate) fn from_managed_relay(
        path: PathBuf,
        policy: crate::managed_provider::ManagedChildPolicy,
        capability: String,
    ) -> Self {
        Self {
            location: path,
            managed: Some((policy, capability)),
        }
    }
}

impl fmt::Debug for CredentialLeaseHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialLeaseHandle")
            .field("present", &true)
            .finish()
    }
}

/// Injected credential lease authority. Tests use fakes.
///
/// `revoke` must invalidate the upstream capability represented by `lease_id`,
/// not merely remove a local credential file. It must be idempotent. The
/// adapter invokes it whenever process-tree termination cannot be proved, so a
/// surviving child cannot retain usable provider authority from an already
/// loaded credential.
pub trait CredentialLeaseResolver: Send + Sync {
    fn resolve(&self, lease_id: &str) -> Result<CredentialLeaseHandle, GrokBuildAdapterError>;

    fn revoke(&self, lease_id: &str) -> Result<(), GrokBuildAdapterError>;

    /// True only when `revoke` invalidates the credential a child already read.
    /// Deleting a local file is not enough.
    fn revokes_upstream(&self) -> bool {
        false
    }

    /// Allocate a request-specific capability before durable dispatch admission.
    fn lease_id_for_request(
        &self,
        alias: &str,
        _request: &str,
    ) -> Result<String, GrokBuildAdapterError> {
        Ok(alias.to_owned())
    }

    fn managed_child_policy(&self) -> Option<crate::managed_provider::ManagedChildPolicy> {
        None
    }
    fn bind_lease_bounds(
        &self,
        _id: &str,
        _max_requests: u32,
        _max_duration_ms: u64,
        _max_total_tokens: u64,
    ) -> Result<(), GrokBuildAdapterError> {
        if self.managed_child_policy().is_some() {
            Err(GrokBuildAdapterError::CredentialLease)
        } else {
            Ok(())
        }
    }
    fn provider_evidence(
        &self,
        _id: &str,
    ) -> Option<crate::managed_provider::ManagedProviderEvidence> {
        None
    }
    fn readiness_error(&self) -> Option<&'static str> {
        None
    }
    fn stop_authority(&self) {}
    fn provider_quiescent(&self, _id: &str) -> bool {
        true
    }
}

/// Test-only file handle. `revoke` deletes the file and does not invalidate a
/// credential the child has already read. Production operator setup must not
/// install this as containment for a live provider child.
pub struct FileCredentialLease {
    lease_id: String,
    path: PathBuf,
}

impl FileCredentialLease {
    pub fn new(lease_id: impl Into<String>, path: PathBuf) -> Self {
        Self {
            lease_id: lease_id.into(),
            path,
        }
    }
}

impl fmt::Debug for FileCredentialLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileCredentialLease")
            .field("lease_id_present", &!self.lease_id.is_empty())
            .finish()
    }
}

impl CredentialLeaseResolver for FileCredentialLease {
    fn resolve(&self, lease_id: &str) -> Result<CredentialLeaseHandle, GrokBuildAdapterError> {
        if lease_id != self.lease_id {
            return Err(GrokBuildAdapterError::CredentialLease);
        }
        Ok(CredentialLeaseHandle::from_host_path(self.path.clone()))
    }

    fn revoke(&self, lease_id: &str) -> Result<(), GrokBuildAdapterError> {
        if lease_id != self.lease_id {
            return Err(GrokBuildAdapterError::CredentialRevocation);
        }
        if self.path.exists() {
            fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&self.path)
                .and_then(|file| file.sync_all())
                .map_err(|_| GrokBuildAdapterError::CredentialRevocation)?;
            fs::remove_file(&self.path).map_err(|_| GrokBuildAdapterError::CredentialRevocation)?;
        }
        Ok(())
    }
}

/// Host registry whose `revoke` rejects a token the caller already holds.
///
/// The file written for the child is only a transport. Acceptance is decided
/// by this registry, so truncating that file does not revoke a token the
/// child has already read.
pub struct HostLeaseAuthority {
    dir: PathBuf,
    scope: String,
    inner: std::sync::Mutex<std::collections::BTreeMap<String, (u64, String, bool, u128)>>,
}

impl HostLeaseAuthority {
    pub fn new(dir: PathBuf) -> Self {
        Self::scoped(dir, "host")
    }

    pub fn scoped(dir: PathBuf, scope: impl Into<String>) -> Self {
        Self {
            dir,
            scope: scope.into(),
            inner: std::sync::Mutex::new(std::collections::BTreeMap::new()),
        }
    }

    pub fn scope(&self) -> &str {
        &self.scope
    }

    pub fn provider_accepts(&self, presented: &str) -> bool {
        let Ok(inner) = self.inner.lock() else {
            return false;
        };
        let now = host_lease_now_ms();
        inner
            .values()
            .any(|(_, token, revoked, expires)| !revoked && *expires > now && token == presented)
    }
}

fn host_lease_now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

impl CredentialLeaseResolver for HostLeaseAuthority {
    fn resolve(&self, lease_id: &str) -> Result<CredentialLeaseHandle, GrokBuildAdapterError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| GrokBuildAdapterError::CredentialLease)?;
        let now = host_lease_now_ms();
        let entry = inner.entry(lease_id.to_string()).or_insert_with(|| {
            (
                1,
                format!("lease:{lease_id}:generation:1"),
                false,
                now.saturating_add(3_600_000),
            )
        });
        if entry.2 || entry.3 <= now {
            if entry.3 <= now {
                entry.0 = entry.0.saturating_add(1);
                entry.1 = format!("lease:{lease_id}:generation:{}", entry.0);
            }
            entry.2 = false;
            entry.3 = now.saturating_add(3_600_000);
        }
        let token = entry.1.clone();
        drop(inner);
        let path = self.dir.join(format!("lease-{lease_id}"));
        if path.exists() {
            fs::remove_file(&path).map_err(|_| GrokBuildAdapterError::CredentialLease)?;
        }
        write_private_file(&path, token.as_bytes())?;
        Ok(CredentialLeaseHandle::from_host_path(path))
    }

    fn revoke(&self, lease_id: &str) -> Result<(), GrokBuildAdapterError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| GrokBuildAdapterError::CredentialRevocation)?;
        let Some(entry) = inner.get_mut(lease_id) else {
            return Err(GrokBuildAdapterError::CredentialRevocation);
        };
        entry.2 = true;
        entry.0 = entry.0.saturating_add(1);
        entry.1 = format!("lease:{lease_id}:generation:{}", entry.0);
        entry.3 = host_lease_now_ms().saturating_add(3_600_000);
        Ok(())
    }

    fn revokes_upstream(&self) -> bool {
        true
    }
}

/// Bounded host-only evidence captured before the disposable Grok home is
/// removed. This payload is deliberately not serializable and its `Debug`
/// implementation reveals only sizes and digests. The orchestration owner may
/// redact and persist the advisory into durable Work; public projections keep
/// using the opaque refs in [`GrokBuildResult`].
#[derive(Clone, PartialEq, Eq)]
pub struct GrokBuildAdvisoryEvidence {
    cli_request_id: String,
    summary: String,
    session_updates: Vec<u8>,
    summary_ref: String,
    session_ref: String,
}

impl fmt::Debug for GrokBuildAdvisoryEvidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrokBuildAdvisoryEvidence")
            .field("cli_request_id_present", &!self.cli_request_id.is_empty())
            .field("summary_bytes", &self.summary.len())
            .field("session_bytes", &self.session_updates.len())
            .field("summary_ref", &self.summary_ref)
            .field("session_ref", &self.session_ref)
            .finish()
    }
}

impl GrokBuildAdvisoryEvidence {
    pub fn cli_request_id(&self) -> &str {
        &self.cli_request_id
    }

    pub fn summary(&self) -> &str {
        &self.summary
    }

    pub fn session_updates(&self) -> &[u8] {
        &self.session_updates
    }

    pub fn summary_ref(&self) -> &str {
        &self.summary_ref
    }

    pub fn session_ref(&self) -> &str {
        &self.session_ref
    }
}

/// Validated receipt/result pair. Raw process output and credentials are never
/// retained. A completed advisory carries a bounded host-only evidence payload
/// whose digests exactly match the public result refs.
#[derive(Clone, PartialEq, Eq)]
pub struct GrokBuildAdapterOutcome {
    receipt: GrokBuildIsolationReceipt,
    result: GrokBuildResult,
    advisory_evidence: Option<GrokBuildAdvisoryEvidence>,
    mutation_evidence: Option<GrokBuildMutationEvidence>,
}

impl fmt::Debug for GrokBuildAdapterOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrokBuildAdapterOutcome")
            .field("receipt", &self.receipt)
            .field("result", &self.result)
            .field("advisory_evidence", &self.advisory_evidence)
            .field("mutation_evidence", &self.mutation_evidence)
            .finish()
    }
}

impl GrokBuildAdapterOutcome {
    pub fn receipt(&self) -> &GrokBuildIsolationReceipt {
        &self.receipt
    }

    pub fn result(&self) -> &GrokBuildResult {
        &self.result
    }

    pub fn advisory_evidence(&self) -> Option<&GrokBuildAdvisoryEvidence> {
        self.advisory_evidence.as_ref()
    }

    pub fn mutation_evidence(&self) -> Option<&GrokBuildMutationEvidence> {
        self.mutation_evidence.as_ref()
    }
}

/// Bounded post-run proof that the disposable checkout remained on its exact
/// launch identity and changed only the Work-authorized file set.
#[derive(Clone, PartialEq, Eq)]
pub struct GrokBuildMutationEvidence {
    final_head_sha: String,
    final_ref: String,
    changed_paths: Vec<String>,
    diff_digest: String,
}

impl fmt::Debug for GrokBuildMutationEvidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrokBuildMutationEvidence")
            .field("final_head_sha", &self.final_head_sha)
            .field("final_ref", &self.final_ref)
            .field("changed_path_count", &self.changed_paths.len())
            .field("diff_digest", &self.diff_digest)
            .finish()
    }
}

impl GrokBuildMutationEvidence {
    pub fn final_head_sha(&self) -> &str {
        &self.final_head_sha
    }

    pub fn final_ref(&self) -> &str {
        &self.final_ref
    }

    pub fn changed_paths(&self) -> &[String] {
        &self.changed_paths
    }

    pub fn diff_digest(&self) -> &str {
        &self.diff_digest
    }
}

/// Execute one bounded Grok Build CLI run. Never retries a spawned process.
pub async fn launch_grok_build(
    launch: &GrokBuildLaunchRequest,
    host: &GrokBuildHostLaunchConfig,
    credentials: &dyn CredentialLeaseResolver,
    cancel: CancellationToken,
) -> Result<GrokBuildAdapterOutcome, GrokBuildAdapterError> {
    let managed = credentials.managed_child_policy().is_some();
    let result = launch_grok_build_inner(launch, host, credentials, cancel).await;
    // Includes readiness/inspect/spawn errors, cancellation and normal exits.
    if managed && credentials.revoke(&launch.credential_lease_id).is_err() {
        return Err(GrokBuildAdapterError::CredentialRevocation);
    }
    if managed {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !credentials.provider_quiescent(&launch.credential_lease_id) {
            if Instant::now() >= deadline {
                return Err(GrokBuildAdapterError::TerminationUnproven);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    result
}

async fn launch_grok_build_inner(
    launch: &GrokBuildLaunchRequest,
    host: &GrokBuildHostLaunchConfig,
    credentials: &dyn CredentialLeaseResolver,
    cancel: CancellationToken,
) -> Result<GrokBuildAdapterOutcome, GrokBuildAdapterError> {
    launch.validate().map_err(map_contract)?;
    host.validate()?;
    // Grok 1.0.x accepts named sandbox profiles but `grok inspect --json`
    // does not expose the active profile, writable roots, or network policy.
    // Until the host can observe that enforcement, this adapter refuses to
    // claim read-only execution. IsolatedReview remains available only for a
    // disposable checkout whose exact mutation is reviewed by Work authority.
    if launch.mutation_mode == GrokBuildMutationMode::ReadOnly {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    if launch.mutation_mode == GrokBuildMutationMode::IsolatedReview
        && host.allowed_files.is_empty()
    {
        return Err(GrokBuildAdapterError::InvalidRequest);
    }
    if host.repository_id != launch.identity.repository_id {
        return Err(GrokBuildAdapterError::IdentityMismatch);
    }
    if (host.prompt.len() as u64) > launch.max_prompt_bytes {
        return Err(GrokBuildAdapterError::InvalidRequest);
    }

    gate_git_identity(launch, host, cancel.clone()).await?;
    gate_no_publish_remote(launch, host, cancel.clone()).await?;
    if cancel.is_cancelled() {
        return Err(GrokBuildAdapterError::Cancelled);
    }
    let isolation_scope = IsolatedScope::acquire(&host.isolate_parent)?;
    let source_fingerprint =
        crate::run_promotion::fingerprint_at(&host.cwd, &launch.identity.head_sha)
            .map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    let source_control_fingerprint = repository_control_fingerprint(host).await?;
    let checkout = IsolatedCheckout::create(launch, host, cancel.clone()).await?;
    let execution_host = checkout.execution_host(host);

    let lease = credentials.resolve(&launch.credential_lease_id)?;
    validate_credential_boundary(&lease.location, &host.isolate_parent)?;
    let isolated = IsolatedHome::create(&host.isolate_parent)?;
    isolated.write_minimal_config()?;
    if let Some((policy, _)) = &lease.managed {
        if launch.max_duration_ms > policy.max_duration_ms || launch.max_turns > policy.max_turns {
            return Err(GrokBuildAdapterError::InvalidRequest);
        }
        isolated.configure_managed_provider(policy)?;
    }
    isolated.install_prompt(&host.prompt)?;
    isolated.install_lease(&lease)?;

    verify_isolation(
        &execution_host,
        &isolated,
        credentials,
        &launch.credential_lease_id,
        lease.managed.as_ref(),
        cancel.clone(),
    )
    .await?;
    // Close the inspect-to-launch window against an externally changed ref or
    // newly introduced project-local compatibility surface.
    gate_git_identity(launch, host, cancel.clone()).await?;
    verify_source_repository_unchanged(
        host,
        &launch.identity.head_sha,
        &source_fingerprint,
        &source_control_fingerprint,
    )
    .await?;
    gate_detached_checkout_identity(launch, &execution_host, cancel.clone()).await?;
    checkout.verify_control_state(&execution_host).await?;

    if cancel.is_cancelled() {
        let _ = checkout.cleanup().await;
        return Err(GrokBuildAdapterError::Cancelled);
    }

    let session_id = Uuid::new_v4().to_string();
    let execution = AllowlistedExecution {
        source_host: host,
        execution_host: &execution_host,
        checkout: &checkout,
        source_fingerprint: &source_fingerprint,
        source_control_fingerprint: &source_control_fingerprint,
        isolated: &isolated,
        credentials,
        managed: lease.managed.as_ref(),
        session_id: &session_id,
    };
    let outcome = execute_allowlisted(launch, execution, cancel).await;
    drop(isolation_scope);
    outcome
}

fn managed_material_leaked(root: &Path, capability: &str) -> bool {
    let mut total = 0u64;
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let Ok(entry) = entry else {
            return true;
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(file) = std::fs::File::open(entry.path()) else {
            return true;
        };
        let Ok(meta) = file.metadata() else {
            return true;
        };
        total = total.saturating_add(meta.len());
        if meta.len() > SESSION_EVIDENCE_MAX || total > 4 * SESSION_EVIDENCE_MAX {
            return true;
        }
        let mut bytes = Vec::new();
        if file
            .take(SESSION_EVIDENCE_MAX + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() as u64 > SESSION_EVIDENCE_MAX
            || bytes
                .windows(capability.len())
                .any(|part| part == capability.as_bytes())
        {
            return true;
        }
    }
    false
}

fn map_contract(err: GrokBuildContractError) -> GrokBuildAdapterError {
    match err {
        GrokBuildContractError::IdentityMismatch => GrokBuildAdapterError::IdentityMismatch,
        GrokBuildContractError::ReadOnlyMutation => GrokBuildAdapterError::ReadOnlyMutation,
        _ => GrokBuildAdapterError::InvalidRequest,
    }
}

fn permission_flag(mode: GrokBuildMutationMode) -> &'static str {
    match mode {
        GrokBuildMutationMode::ReadOnly => "plan",
        // `acceptEdits` still prompts for the write tool in Grok 1.0.13 and
        // headless stdin is deliberately closed. The host maps an explicit,
        // revision-bound Work approval into this noninteractive mode. This CLI
        // flag is not isolation authority: the host separately enforces an OS
        // write sandbox, a private Git directory, an empty inherited
        // environment, a no-remote gate, and an exact post-run allowlist.
        GrokBuildMutationMode::IsolatedReview => "bypassPermissions",
    }
}

fn allowlisted_args(
    prompt_file: &Path,
    mode: GrokBuildMutationMode,
    max_turns: u32,
    session_id: &str,
) -> Result<Vec<String>, GrokBuildAdapterError> {
    let prompt_file = prompt_file
        .to_str()
        .ok_or(GrokBuildAdapterError::IsolationFailed)?;
    let mut args = vec![
        "--prompt-file".to_string(),
        prompt_file.to_string(),
        "--permission-mode".to_string(),
        permission_flag(mode).to_string(),
        "--disable-web-search".to_string(),
        "--no-subagents".to_string(),
    ];
    if mode == GrokBuildMutationMode::ReadOnly {
        args.extend(["--sandbox".to_string(), "grokptah_read_only".to_string()]);
    }
    args.extend([
        "--max-turns".to_string(),
        max_turns.to_string(),
        "--session-id".to_string(),
        session_id.to_string(),
        "--output-format".to_string(),
        "json".to_string(),
    ]);
    Ok(args)
}

fn allowlisted_env(grok_home: &Path) -> Result<Vec<(String, String)>, GrokBuildAdapterError> {
    let home = grok_home
        .to_str()
        .ok_or(GrokBuildAdapterError::IsolationFailed)?;
    let claude_config_dir = grok_home.join("claude-config");
    let claude_config_dir = claude_config_dir
        .to_str()
        .ok_or(GrokBuildAdapterError::IsolationFailed)?;
    Ok(vec![
        ("GROK_HOME".to_string(), home.to_string()),
        ("HOME".to_string(), home.to_string()),
        ("TMPDIR".to_string(), home.to_string()),
        ("PATH".to_string(), BOUNDED_PATH.to_string()),
        (
            "CLAUDE_CONFIG_DIR".to_string(),
            claude_config_dir.to_string(),
        ),
    ])
}

#[cfg(target_os = "macos")]
fn mutation_sandbox_command(
    host: &GrokBuildHostLaunchConfig,
    args: &[String],
) -> Result<Command, GrokBuildAdapterError> {
    let sandbox = Path::new("/usr/bin/sandbox-exec");
    require_absolute_file_path(sandbox)?;
    let isolate_root = dunce::canonicalize(&host.isolate_parent)
        .map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    let isolate_root = isolate_root
        .to_str()
        .ok_or(GrokBuildAdapterError::IsolationFailed)?;
    let mut command = Command::new(sandbox);
    command
        .arg("-p")
        .arg(MACOS_MUTATION_SANDBOX)
        .arg("-D")
        .arg(format!("ISOLATE_ROOT={isolate_root}"))
        .arg(&host.executable)
        .args(args);
    Ok(command)
}

#[cfg(target_os = "macos")]
fn managed_sandbox_command(
    host: &GrokBuildHostLaunchConfig,
    args: &[String],
    policy: &crate::managed_provider::ManagedChildPolicy,
    home: &Path,
) -> Result<Command, GrokBuildAdapterError> {
    require_absolute_file_path(Path::new("/usr/bin/sandbox-exec"))?;
    let candidate =
        dunce::canonicalize(&host.cwd).map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    let home = dunce::canonicalize(home).map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    let cli = dunce::canonicalize(&host.executable)
        .map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command
        .arg("-p")
        .arg(MACOS_MANAGED_SANDBOX)
        .arg("-D")
        .arg(format!("CANDIDATE={}", candidate.display()))
        .arg("-D")
        .arg(format!("CHILD_HOME={}", home.display()))
        .arg("-D")
        .arg(format!("CLI={}", cli.display()))
        .arg("-D")
        .arg(format!("RELAY=localhost:{}", policy.port))
        .arg(cli)
        .args(args);
    Ok(command)
}

#[cfg(not(target_os = "macos"))]
fn managed_sandbox_command(
    _host: &GrokBuildHostLaunchConfig,
    _args: &[String],
    _policy: &crate::managed_provider::ManagedChildPolicy,
    _home: &Path,
) -> Result<Command, GrokBuildAdapterError> {
    Err(GrokBuildAdapterError::IsolationFailed)
}

/// No credential is resolved and no inference endpoint is reachable in this
/// probe. The actual isolated inspect contract is checked again before launch.
pub(crate) fn managed_offline_readiness(
    executable: &Path,
    isolate_parent: &Path,
) -> Result<String, &'static str> {
    validate_isolate_parent(isolate_parent).map_err(|_| "private worker root is unavailable")?;
    // Status/readiness may run while admission acquires its empty worker root.
    // Never create probe files in that root or expose them to the child.
    let probe_root = IsolatedHome::create(&std::env::temp_dir())
        .map_err(|_| "private CLI readiness directory unavailable")?;
    let isolated = IsolatedHome::create(&probe_root.path)
        .map_err(|_| "private CLI readiness directory is not writable")?;
    isolated
        .write_minimal_config()
        .map_err(|_| "private CLI configuration is unavailable")?;
    let git = std::process::Command::new("/usr/bin/git")
        .args(["init", "--quiet"])
        .current_dir(&isolated.path)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| "private CLI inspection repository is unavailable")?;
    if !git.success() {
        return Err("private CLI inspection repository is unavailable");
    }
    let host = GrokBuildHostLaunchConfig {
        executable: executable.into(),
        git_executable: "/usr/bin/git".into(),
        cwd: isolated.path.clone(),
        repository_id: "offline-probe".into(),
        base_ref: "refs/heads/probe".into(),
        prompt: "offline".into(),
        allowed_files: vec!["probe".into()],
        execution_approved: false,
        max_stdout_bytes: 1024,
        max_stderr_bytes: 1024,
        git_timeout: Duration::from_secs(3),
        isolate_parent: probe_root.path.clone(),
        defer_source_apply: false,
        candidate_retention_dir: None,
    };
    let policy = crate::managed_provider::ManagedChildPolicy {
        endpoint: "http://127.0.0.1:1/v1".into(),
        port: 1,
        model: "grok-build-0.1".into(),
        max_duration_ms: 1,
        max_turns: 1,
        max_output_tokens: 1,
    };
    let probe = |args: &[String], limit: usize| -> Result<Vec<u8>, &'static str> {
        let mut cmd = managed_sandbox_command(&host, args, &policy, &isolated.path)
            .map_err(|_| "managed worker confinement is unsupported")?;
        cmd.env_clear()
            .envs(
                allowlisted_env(&isolated.path)
                    .map_err(|_| "private CLI environment unavailable")?,
            )
            .current_dir(&isolated.path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        crate::process_tree::configure(&mut cmd);
        let mut child = cmd
            .into_std()
            .spawn()
            .map_err(|_| "managed CLI cannot start inside confinement")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("managed CLI version unavailable")?;
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .take(limit as u64 + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if started.elapsed() < Duration::from_secs(3) => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => break None,
            }
        };
        #[cfg(unix)]
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.kill();
        let _ = child.wait();
        let bytes = reader
            .join()
            .ok()
            .and_then(Result::ok)
            .ok_or("managed CLI version unavailable")?;
        if !status.is_some_and(|status| status.success()) || bytes.len() > limit {
            return Err("managed CLI confined inspection is unsupported");
        }
        Ok(bytes)
    };
    let version = probe(&["--version".into()], 1024)?;
    if !String::from_utf8_lossy(&version).starts_with("grok 1.0.41 ") {
        return Err("managed CLI version or confined startup is unsupported; requires Grok 1.0.41");
    }
    let report = probe(&["inspect".into(), "--json".into()], INSPECT_OUTPUT_MAX)?;
    let value: serde_json::Value = serde_json::from_slice(&report)
        .map_err(|_| "managed CLI inspect contract is unsupported")?;
    if value["grokVersion"].as_str() != Some("1.0.41") {
        return Err("managed CLI inspect version is unsupported");
    }
    verify_inspect_report(&host, &isolated, &value)
        .map_err(|_| "managed CLI private inspect contract is unsupported")?;
    Ok("1.0.41".into())
}

#[cfg(not(target_os = "macos"))]
fn mutation_sandbox_command(
    _host: &GrokBuildHostLaunchConfig,
    _args: &[String],
) -> Result<Command, GrokBuildAdapterError> {
    Err(GrokBuildAdapterError::IsolationFailed)
}

struct IsolatedHome {
    path: PathBuf,
    cleanup_on_drop: AtomicBool,
}

struct IsolatedScope {
    lock_path: PathBuf,
}

impl IsolatedScope {
    fn acquire(parent: &Path) -> Result<Self, GrokBuildAdapterError> {
        validate_isolate_parent(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::symlink_metadata(parent)
                .map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
            if metadata.mode() & 0o077 != 0 {
                return Err(GrokBuildAdapterError::IsolationFailed);
            }
        }
        let mut entries =
            std::fs::read_dir(parent).map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
        if entries.next().is_some() {
            return Err(GrokBuildAdapterError::IsolationFailed);
        }
        let lock_path = parent.join(".grokptah-isolation-scope");
        write_private_file(&lock_path, b"grokptah-isolation-scope-v1\n")?;
        Ok(Self { lock_path })
    }
}

impl Drop for IsolatedScope {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

/// A self-contained private clone with no shared Git directory or remote.
/// The untrusted child is additionally confined by a host-observed OS write
/// sandbox. The authoritative Work checkout is changed only after a complete
/// advisory has been verified and the child process tree is known to be gone.
struct IsolatedCheckout {
    path: PathBuf,
    control_fingerprint: String,
    cleanup_on_drop: AtomicBool,
}

impl IsolatedCheckout {
    async fn create(
        launch: &GrokBuildLaunchRequest,
        host: &GrokBuildHostLaunchConfig,
        cancel: CancellationToken,
    ) -> Result<Self, GrokBuildAdapterError> {
        validate_isolate_parent(&host.isolate_parent)?;
        validate_isolation_boundary(&host.cwd, &host.isolate_parent)?;
        let path = host
            .isolate_parent
            .join(format!("gw-{}", Uuid::new_v4().simple()));
        let source =
            dunce::canonicalize(&host.cwd).map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
        let source_str = source
            .to_str()
            .ok_or(GrokBuildAdapterError::IsolationFailed)?;
        let path_str = path
            .to_str()
            .ok_or(GrokBuildAdapterError::IsolationFailed)?;
        git_status_ok(
            host,
            &[
                "clone",
                "--no-local",
                "--no-hardlinks",
                "--no-checkout",
                "--no-tags",
                "--",
                source_str,
                path_str,
            ],
            cancel.clone(),
        )
        .await
        .map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
        set_private_dir_permissions(&path)?;
        let mut checkout = Self {
            path,
            control_fingerprint: String::new(),
            cleanup_on_drop: AtomicBool::new(true),
        };
        let execution_host = checkout.execution_host(host);
        let prepared = async {
            git_status_ok(
                &execution_host,
                &["config", "--local", "core.hooksPath", "/dev/null"],
                cancel.clone(),
            )
            .await?;
            git_status_ok(
                &execution_host,
                &["remote", "remove", "origin"],
                cancel.clone(),
            )
            .await?;
            git_status_ok(
                &execution_host,
                &["checkout", "--detach", "--force", &launch.identity.head_sha],
                cancel.clone(),
            )
            .await?;
            gate_private_repository(&execution_host, cancel.clone()).await?;
            gate_no_publish_remote(launch, &execution_host, cancel.clone()).await?;
            gate_detached_checkout_identity(launch, &execution_host, cancel.clone()).await?;
            repository_control_fingerprint(&execution_host).await
        }
        .await;
        let Ok(control_fingerprint) = prepared else {
            let _ = checkout.cleanup().await;
            return Err(GrokBuildAdapterError::IsolationFailed);
        };
        checkout.control_fingerprint = control_fingerprint;
        Ok(checkout)
    }

    fn execution_host(&self, source: &GrokBuildHostLaunchConfig) -> GrokBuildHostLaunchConfig {
        let mut host = source.clone();
        host.cwd = self.path.clone();
        host
    }

    async fn verify_control_state(
        &self,
        host: &GrokBuildHostLaunchConfig,
    ) -> Result<(), GrokBuildAdapterError> {
        gate_private_repository(host, CancellationToken::new()).await?;
        if repository_control_fingerprint(host).await? != self.control_fingerprint {
            return Err(GrokBuildAdapterError::IsolationFailed);
        }
        Ok(())
    }

    async fn cleanup(&self) -> bool {
        if !self.cleanup_on_drop.load(Ordering::Acquire) {
            return true;
        }
        let cleaned = std::fs::remove_dir_all(&self.path).is_ok() && !self.path.exists();
        if cleaned {
            self.cleanup_on_drop.store(false, Ordering::Release);
        }
        cleaned
    }

    /// A possibly live child still owns this checkout. Keep it quarantined and
    /// on disk for later owner-verified reclamation; most importantly, never
    /// let Drop race the unproved process tree.
    fn quarantine(&self) {
        self.cleanup_on_drop.store(false, Ordering::Release);
    }
}

impl Drop for IsolatedCheckout {
    fn drop(&mut self) {
        if self.cleanup_on_drop.load(Ordering::Acquire) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

impl IsolatedHome {
    fn create(parent: &Path) -> Result<Self, GrokBuildAdapterError> {
        validate_isolate_parent(parent)?;
        let path = parent.join(format!("gb-{}", Uuid::new_v4().simple()));
        std::fs::create_dir(&path).map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
        set_private_dir_permissions(&path)?;
        Ok(Self {
            path,
            cleanup_on_drop: AtomicBool::new(true),
        })
    }

    fn write_minimal_config(&self) -> Result<(), GrokBuildAdapterError> {
        let claude_config_dir = self.path.join("claude-config");
        std::fs::create_dir(&claude_config_dir)
            .map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
        set_private_dir_permissions(&claude_config_dir)?;
        write_private_file(
            &self.path.join(CONFIG_FILE_NAME),
            ISOLATED_CONFIG.as_bytes(),
        )?;
        write_private_file(
            &self.path.join(SANDBOX_FILE_NAME),
            SANDBOX_CONFIG.as_bytes(),
        )
    }

    fn install_prompt(&self, prompt: &str) -> Result<(), GrokBuildAdapterError> {
        write_private_file(&self.path.join(PROMPT_FILE_NAME), prompt.as_bytes())
    }

    fn configure_managed_provider(
        &self,
        policy: &crate::managed_provider::ManagedChildPolicy,
    ) -> Result<(), GrokBuildAdapterError> {
        // The config contains a local address and an env variable NAME only.
        // The local capability is injected into the cleared child environment.
        let config = format!("{ISOLATED_CONFIG}\n[models]\ndefault = \"managed-assignment\"\nmax_retries = 0\nmax_completion_tokens = {}\n\n[model.managed-assignment]\nbase_url = \"{}\"\nmodel = \"{}\"\nenv_key = \"GROKPTAH_MANAGED_CAPABILITY\"\n\n[endpoints]\nmodels_base_url = \"{}\"\n[cli]\nauto_update = false\n[features]\ntelemetry = false\ntitle_refresh = false\nturn_summary = false\n[workflows]\nenabled = false\n", policy.max_output_tokens, policy.endpoint, policy.model, policy.endpoint);
        std::fs::write(self.path.join(CONFIG_FILE_NAME), config)
            .map_err(|_| GrokBuildAdapterError::IsolationFailed)
    }

    fn prompt_path(&self) -> PathBuf {
        self.path.join(PROMPT_FILE_NAME)
    }

    fn install_lease(&self, lease: &CredentialLeaseHandle) -> Result<(), GrokBuildAdapterError> {
        require_absolute_file_path(&lease.location)
            .map_err(|_| GrokBuildAdapterError::CredentialLease)?;
        let source = open_credential_source(&lease.location)?;
        let meta = source
            .metadata()
            .map_err(|_| GrokBuildAdapterError::CredentialLease)?;
        validate_credential_metadata(&meta)?;
        let dest = self.path.join(AUTH_FILE_NAME);
        let mut target =
            create_private_file(&dest).map_err(|_| GrokBuildAdapterError::CredentialLease)?;
        let copied = io::copy(&mut source.take(LEASE_FILE_MAX + 1), &mut target)
            .map_err(|_| GrokBuildAdapterError::CredentialLease)?;
        if copied == 0 || copied > LEASE_FILE_MAX || copied != meta.len() {
            return Err(GrokBuildAdapterError::CredentialLease);
        }
        target
            .sync_all()
            .map_err(|_| GrokBuildAdapterError::CredentialLease)?;
        Ok(())
    }

    fn cleanup(&self) -> bool {
        let cleaned = std::fs::remove_dir_all(&self.path).is_ok() && !self.path.exists();
        if cleaned {
            self.cleanup_on_drop.store(false, Ordering::Release);
        }
        cleaned
    }

    /// Revoke path-based access to secrets before any uncertain termination
    /// result is returned. Truncation affects an already-open descriptor on
    /// Unix; unlinking prevents a surviving process from reopening it. The
    /// final directory cleanup remains best-effort and never disables Drop.
    fn revoke_sensitive_material(&self) -> bool {
        let mut revoked = true;
        for name in [AUTH_FILE_NAME, PROMPT_FILE_NAME] {
            let path = self.path.join(name);
            let mut options = OpenOptions::new();
            options.write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
            }
            let truncated = match options.open(&path) {
                Ok(file) => file.set_len(0).and_then(|()| file.sync_all()).is_ok(),
                Err(error) if error.kind() == io::ErrorKind::NotFound => true,
                Err(_) => false,
            };
            let unlinked = match std::fs::remove_file(&path) {
                Ok(()) => true,
                Err(error) if error.kind() == io::ErrorKind::NotFound => true,
                Err(_) => false,
            };
            revoked &= truncated && unlinked;
        }
        revoked
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        if self.cleanup_on_drop.load(Ordering::Acquire) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

fn validate_isolate_parent(parent: &Path) -> Result<(), GrokBuildAdapterError> {
    let meta =
        std::fs::symlink_metadata(parent).map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    if !meta.file_type().is_dir() || meta.file_type().is_symlink() {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o022 != 0 {
            return Err(GrokBuildAdapterError::IsolationFailed);
        }
    }
    Ok(())
}

fn validate_credential_boundary(
    credential: &Path,
    isolate_parent: &Path,
) -> Result<(), GrokBuildAdapterError> {
    let credential =
        dunce::canonicalize(credential).map_err(|_| GrokBuildAdapterError::CredentialLease)?;
    let isolate_parent =
        dunce::canonicalize(isolate_parent).map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    if credential.starts_with(isolate_parent) {
        return Err(GrokBuildAdapterError::CredentialLease);
    }
    Ok(())
}

fn validate_isolation_boundary(
    source: &Path,
    isolate_parent: &Path,
) -> Result<(), GrokBuildAdapterError> {
    let source = dunce::canonicalize(source).map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    let isolate_parent =
        dunce::canonicalize(isolate_parent).map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    if source.starts_with(&isolate_parent) || isolate_parent.starts_with(&source) {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    Ok(())
}

fn write_private_file(path: &Path, contents: &[u8]) -> Result<(), GrokBuildAdapterError> {
    let mut file = create_private_file(path).map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    file.write_all(contents)
        .and_then(|()| file.sync_all())
        .map_err(|_| GrokBuildAdapterError::IsolationFailed)
}

fn create_private_file(path: &Path) -> io::Result<std::fs::File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn open_credential_source(path: &Path) -> Result<std::fs::File, GrokBuildAdapterError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options
        .open(path)
        .map_err(|_| GrokBuildAdapterError::CredentialLease)
}

fn validate_credential_metadata(meta: &std::fs::Metadata) -> Result<(), GrokBuildAdapterError> {
    if !meta.file_type().is_file() || meta.len() == 0 || meta.len() > LEASE_FILE_MAX {
        return Err(GrokBuildAdapterError::CredentialLease);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } {
            return Err(GrokBuildAdapterError::CredentialLease);
        }
    }
    Ok(())
}

fn set_private_dir_permissions(path: &Path) -> Result<(), GrokBuildAdapterError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    }
    Ok(())
}

async fn verify_isolation(
    host: &GrokBuildHostLaunchConfig,
    isolated: &IsolatedHome,
    credentials: &dyn CredentialLeaseResolver,
    credential_lease_id: &str,
    managed: Option<&(crate::managed_provider::ManagedChildPolicy, String)>,
    cancel: CancellationToken,
) -> Result<(), GrokBuildAdapterError> {
    let env = allowlisted_env(&isolated.path)?;
    let mut cmd = if let Some((policy, _)) = managed {
        managed_sandbox_command(
            host,
            &["inspect".into(), "--json".into()],
            policy,
            &isolated.path,
        )?
    } else {
        let mut cmd = Command::new(&host.executable);
        cmd.args(["inspect", "--json"]);
        cmd
    };
    cmd.current_dir(&host.cwd);
    cmd.env_clear();
    cmd.envs(env);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    crate::process_tree::configure(&mut cmd);

    let mut child = cmd
        .spawn()
        .map_err(|_| GrokBuildAdapterError::SpawnFailed)?;
    let harvest = harvest_child(
        &mut child,
        INSPECT_OUTPUT_MAX,
        GIT_OUTPUT_MAX,
        host.git_timeout,
        cancel,
    )
    .await;
    if harvest.kind == HarvestKind::TerminationUnproven {
        return Err(contain_uncertain_termination(
            credentials,
            credential_lease_id,
            isolated,
        ));
    }
    if harvest.kind != HarvestKind::Exited(0) {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    let value: serde_json::Value = serde_json::from_slice(&harvest.stdout)
        .map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    if managed.is_some() && value["grokVersion"].as_str() != Some("1.0.41") {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    verify_inspect_report(host, isolated, &value)
}

fn verify_inspect_report(
    host: &GrokBuildHostLaunchConfig,
    isolated: &IsolatedHome,
    value: &serde_json::Value,
) -> Result<(), GrokBuildAdapterError> {
    let report = value
        .as_object()
        .ok_or(GrokBuildAdapterError::IsolationFailed)?;
    require_exact_keys(
        report,
        &[
            "grokVersion",
            "channel",
            "cwd",
            "projectRoot",
            "projectTrusted",
            "projectInstructions",
            "permissions",
            "loginPolicy",
            "hooks",
            "skills",
            "agents",
            "plugins",
            "marketplaces",
            "mcpServers",
            "lspServers",
            "configSources",
            "externalCompat",
        ],
    )?;
    for field in [
        "projectInstructions",
        "hooks",
        "plugins",
        "marketplaces",
        "mcpServers",
        "lspServers",
    ] {
        require_empty_array(report.get(field))?;
    }
    for field in ["grokVersion", "channel"] {
        if report
            .get(field)
            .and_then(serde_json::Value::as_str)
            .is_none_or(|value| value.is_empty())
        {
            return Err(GrokBuildAdapterError::IsolationFailed);
        }
    }
    let expected_cwd =
        dunce::canonicalize(&host.cwd).map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    for field in ["cwd", "projectRoot"] {
        let observed = report
            .get(field)
            .and_then(serde_json::Value::as_str)
            .ok_or(GrokBuildAdapterError::IsolationFailed)?;
        if dunce::canonicalize(observed).ok().as_ref() != Some(&expected_cwd) {
            return Err(GrokBuildAdapterError::IsolationFailed);
        }
    }
    if !report
        .get("projectTrusted")
        .is_some_and(serde_json::Value::is_boolean)
    {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    verify_permissions(report.get("permissions"))?;
    verify_login_policy(report.get("loginPolicy"))?;
    verify_builtin_skills(report.get("skills"), isolated)?;
    verify_builtin_agents(report.get("agents"))?;
    verify_config_sources(report.get("configSources"), isolated)?;
    verify_external_compat(report.get("externalCompat"))
}

fn require_exact_keys(
    object: &serde_json::Map<String, serde_json::Value>,
    expected: &[&str],
) -> Result<(), GrokBuildAdapterError> {
    if object.len() != expected.len()
        || object
            .keys()
            .any(|key| !expected.iter().any(|item| key == item))
    {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    Ok(())
}

fn require_allowed_keys(
    object: &serde_json::Map<String, serde_json::Value>,
    required: &[&str],
    allowed: &[&str],
) -> Result<(), GrokBuildAdapterError> {
    if required.iter().any(|key| !object.contains_key(*key))
        || object
            .keys()
            .any(|key| !allowed.iter().any(|item| key == item))
    {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    Ok(())
}

fn require_empty_array(value: Option<&serde_json::Value>) -> Result<(), GrokBuildAdapterError> {
    if !value
        .and_then(serde_json::Value::as_array)
        .is_some_and(Vec::is_empty)
    {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    Ok(())
}

fn verify_permissions(value: Option<&serde_json::Value>) -> Result<(), GrokBuildAdapterError> {
    let object = value
        .and_then(serde_json::Value::as_object)
        .ok_or(GrokBuildAdapterError::IsolationFailed)?;
    require_allowed_keys(
        object,
        &[
            "loaded",
            "managedSettingsActive",
            "managedSettingsExists",
            "managedSettingsPath",
            "marketplaceAllowlist",
            "mcpServerAllowlist",
            "skipped",
            "sources",
        ],
        &[
            "loaded",
            "managedSettingsActive",
            "managedSettingsExists",
            "managedSettingsPath",
            "marketplaceAllowlist",
            "mcpServerAllowlist",
            "skipped",
            "sources",
            "mcpLockdownSources",
            "mcpManagedServersOnly",
            "marketplaceLockdownSources",
            "managedMarketplaces",
            "claudeBypassLockAdvisory",
        ],
    )?;
    if object.get("loaded").and_then(serde_json::Value::as_u64) != Some(0)
        || object
            .get("managedSettingsActive")
            .and_then(serde_json::Value::as_bool)
            != Some(false)
        || object
            .get("managedSettingsExists")
            .and_then(serde_json::Value::as_bool)
            != Some(false)
        || !object
            .get("managedSettingsPath")
            .is_some_and(serde_json::Value::is_string)
    {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    for field in [
        "marketplaceAllowlist",
        "mcpServerAllowlist",
        "skipped",
        "sources",
        "mcpLockdownSources",
        "marketplaceLockdownSources",
        "managedMarketplaces",
    ] {
        if let Some(value) = object.get(field) {
            require_empty_array(Some(value))?;
        }
    }
    if let Some(value) = object.get("mcpManagedServersOnly") {
        if value.as_str() != Some("off") {
            return Err(GrokBuildAdapterError::IsolationFailed);
        }
    }
    if let Some(value) = object.get("claudeBypassLockAdvisory") {
        if value.as_bool() != Some(false) {
            return Err(GrokBuildAdapterError::IsolationFailed);
        }
    }
    Ok(())
}

fn verify_login_policy(value: Option<&serde_json::Value>) -> Result<(), GrokBuildAdapterError> {
    let object = value
        .and_then(serde_json::Value::as_object)
        .ok_or(GrokBuildAdapterError::IsolationFailed)?;
    require_exact_keys(
        object,
        &[
            "apiKeyAuthDisabled",
            "disableApiKeyAuth",
            "forceLoginTeamUuid",
        ],
    )?;
    if !object
        .get("apiKeyAuthDisabled")
        .is_some_and(serde_json::Value::is_boolean)
        || !object
            .get("disableApiKeyAuth")
            .is_some_and(|value| value.is_null() || value.is_boolean())
        || !object
            .get("forceLoginTeamUuid")
            .is_some_and(|value| value.is_null() || value.is_string())
    {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    Ok(())
}

fn verify_builtin_skills(
    value: Option<&serde_json::Value>,
    isolated: &IsolatedHome,
) -> Result<(), GrokBuildAdapterError> {
    let entries = value
        .and_then(serde_json::Value::as_array)
        .ok_or(GrokBuildAdapterError::IsolationFailed)?;
    let bundled = isolated.path.join("bundled").join("skills");
    for entry in entries {
        let object = entry
            .as_object()
            .ok_or(GrokBuildAdapterError::IsolationFailed)?;
        require_allowed_keys(
            object,
            &["name", "description", "source", "userInvocable"],
            &[
                "name",
                "description",
                "source",
                "userInvocable",
                "collidesWith",
                "invocableAs",
            ],
        )?;
        let source = object
            .get("source")
            .and_then(serde_json::Value::as_object)
            .ok_or(GrokBuildAdapterError::IsolationFailed)?;
        require_exact_keys(source, &["type", "path"])?;
        if source.get("type").and_then(serde_json::Value::as_str) != Some("bundled") {
            return Err(GrokBuildAdapterError::IsolationFailed);
        }
        let path = source
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from)
            .ok_or(GrokBuildAdapterError::IsolationFailed)?;
        if !path.starts_with(&bundled) {
            return Err(GrokBuildAdapterError::IsolationFailed);
        }
    }
    Ok(())
}

fn verify_builtin_agents(value: Option<&serde_json::Value>) -> Result<(), GrokBuildAdapterError> {
    let entries = value
        .and_then(serde_json::Value::as_array)
        .ok_or(GrokBuildAdapterError::IsolationFailed)?;
    for entry in entries {
        let object = entry
            .as_object()
            .ok_or(GrokBuildAdapterError::IsolationFailed)?;
        require_exact_keys(object, &["name", "description", "source"])?;
        let source = object
            .get("source")
            .and_then(serde_json::Value::as_object)
            .ok_or(GrokBuildAdapterError::IsolationFailed)?;
        require_exact_keys(source, &["type"])?;
        if source.get("type").and_then(serde_json::Value::as_str) != Some("builtin") {
            return Err(GrokBuildAdapterError::IsolationFailed);
        }
    }
    Ok(())
}

fn verify_config_sources(
    value: Option<&serde_json::Value>,
    isolated: &IsolatedHome,
) -> Result<(), GrokBuildAdapterError> {
    let object = value
        .and_then(serde_json::Value::as_object)
        .ok_or(GrokBuildAdapterError::IsolationFailed)?;
    require_exact_keys(object, &["layers"])?;
    let layers = object
        .get("layers")
        .and_then(serde_json::Value::as_array)
        .ok_or(GrokBuildAdapterError::IsolationFailed)?;
    if layers.len() != 1 {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    let layer = layers[0]
        .as_object()
        .ok_or(GrokBuildAdapterError::IsolationFailed)?;
    require_exact_keys(layer, &["path", "role"])?;
    if layer.get("role").and_then(serde_json::Value::as_str) != Some("user")
        || layer.get("path").and_then(serde_json::Value::as_str)
            != isolated.path.join(CONFIG_FILE_NAME).to_str()
    {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    Ok(())
}

fn verify_external_compat(value: Option<&serde_json::Value>) -> Result<(), GrokBuildAdapterError> {
    let object = value
        .and_then(serde_json::Value::as_object)
        .ok_or(GrokBuildAdapterError::IsolationFailed)?;
    require_exact_keys(object, &["cells", "remoteSettingsLoaded"])?;
    if object
        .get("remoteSettingsLoaded")
        .and_then(serde_json::Value::as_bool)
        != Some(false)
    {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    let cells = object
        .get("cells")
        .and_then(serde_json::Value::as_array)
        .ok_or(GrokBuildAdapterError::IsolationFailed)?;
    let mut observed = Vec::new();
    for cell in cells {
        let cell = cell
            .as_object()
            .ok_or(GrokBuildAdapterError::IsolationFailed)?;
        require_exact_keys(cell, &["enabled", "source", "surface", "vendor"])?;
        if cell.get("enabled").and_then(serde_json::Value::as_bool) != Some(false)
            || cell.get("source").and_then(serde_json::Value::as_str) != Some("config")
        {
            return Err(GrokBuildAdapterError::IsolationFailed);
        }
        observed.push((
            cell.get("vendor")
                .and_then(serde_json::Value::as_str)
                .ok_or(GrokBuildAdapterError::IsolationFailed)?,
            cell.get("surface")
                .and_then(serde_json::Value::as_str)
                .ok_or(GrokBuildAdapterError::IsolationFailed)?,
        ));
    }
    observed.sort_unstable();
    let mut expected = Vec::new();
    for vendor in ["claude", "cursor"] {
        for surface in ["agents", "hooks", "mcps", "rules", "sessions", "skills"] {
            expected.push((vendor, surface));
        }
    }
    expected.push(("codex", "sessions"));
    expected.sort_unstable();
    if observed != expected {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    Ok(())
}

async fn gate_git_identity(
    launch: &GrokBuildLaunchRequest,
    host: &GrokBuildHostLaunchConfig,
    cancel: CancellationToken,
) -> Result<(), GrokBuildAdapterError> {
    let cwd =
        dunce::canonicalize(&host.cwd).map_err(|_| GrokBuildAdapterError::IdentityMismatch)?;
    let toplevel = git_stdout(host, &["rev-parse", "--show-toplevel"], cancel.clone()).await?;
    let toplevel = dunce::canonicalize(bytes_to_trimmed_path(&toplevel)?)
        .map_err(|_| GrokBuildAdapterError::IdentityMismatch)?;
    if toplevel != cwd {
        return Err(GrokBuildAdapterError::IdentityMismatch);
    }

    let head = git_stdout(host, &["rev-parse", "--verify", "HEAD"], cancel.clone()).await?;
    if bytes_to_trimmed_str(&head)? != launch.identity.head_sha {
        return Err(GrokBuildAdapterError::IdentityMismatch);
    }

    let git_ref = git_stdout(host, &["symbolic-ref", "HEAD"], cancel.clone()).await?;
    if bytes_to_trimmed_str(&git_ref)? != launch.identity.git_ref {
        return Err(GrokBuildAdapterError::IdentityMismatch);
    }

    let base = git_stdout(
        host,
        &["rev-parse", "--verify", "--end-of-options", &host.base_ref],
        cancel.clone(),
    )
    .await?;
    if bytes_to_trimmed_str(&base)? != launch.identity.base_sha {
        return Err(GrokBuildAdapterError::IdentityMismatch);
    }

    git_status_ok(
        host,
        &[
            "merge-base",
            "--is-ancestor",
            "--end-of-options",
            &launch.identity.base_sha,
            "HEAD",
        ],
        cancel.clone(),
    )
    .await?;

    let status = git_stdout(
        host,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
        ],
        cancel,
    )
    .await?;
    let dirty = porcelain_paths(&status)?;
    if !dirty.is_empty() {
        return Err(GrokBuildAdapterError::DirtyTree);
    }
    Ok(())
}

async fn gate_detached_checkout_identity(
    launch: &GrokBuildLaunchRequest,
    host: &GrokBuildHostLaunchConfig,
    cancel: CancellationToken,
) -> Result<(), GrokBuildAdapterError> {
    let cwd =
        dunce::canonicalize(&host.cwd).map_err(|_| GrokBuildAdapterError::IdentityMismatch)?;
    let toplevel = git_stdout(host, &["rev-parse", "--show-toplevel"], cancel.clone()).await?;
    let toplevel = dunce::canonicalize(bytes_to_trimmed_path(&toplevel)?)
        .map_err(|_| GrokBuildAdapterError::IdentityMismatch)?;
    if toplevel != cwd {
        return Err(GrokBuildAdapterError::IdentityMismatch);
    }
    let head = git_stdout(host, &["rev-parse", "--verify", "HEAD"], cancel.clone()).await?;
    if bytes_to_trimmed_str(&head)? != launch.identity.head_sha {
        return Err(GrokBuildAdapterError::IdentityMismatch);
    }
    let symbolic = git_output(host, &["symbolic-ref", "-q", "HEAD"], cancel.clone()).await?;
    if symbolic.status || !symbolic.stdout.is_empty() {
        return Err(GrokBuildAdapterError::IdentityMismatch);
    }
    git_status_ok(
        host,
        &[
            "merge-base",
            "--is-ancestor",
            "--end-of-options",
            &launch.identity.base_sha,
            "HEAD",
        ],
        cancel.clone(),
    )
    .await?;
    let status = git_stdout(
        host,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
        ],
        cancel,
    )
    .await?;
    if !porcelain_paths(&status)?.is_empty() {
        return Err(GrokBuildAdapterError::DirtyTree);
    }
    Ok(())
}

async fn gate_no_publish_remote(
    launch: &GrokBuildLaunchRequest,
    host: &GrokBuildHostLaunchConfig,
    cancel: CancellationToken,
) -> Result<(), GrokBuildAdapterError> {
    if launch.mutation_mode != GrokBuildMutationMode::IsolatedReview {
        return Ok(());
    }
    let remotes = git_stdout(host, &["remote"], cancel).await?;
    if !bytes_to_trimmed_str(&remotes)?.is_empty() {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    Ok(())
}

async fn gate_private_repository(
    host: &GrokBuildHostLaunchConfig,
    cancel: CancellationToken,
) -> Result<(), GrokBuildAdapterError> {
    let root =
        dunce::canonicalize(&host.cwd).map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    let git_dir = git_stdout(
        host,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
        cancel.clone(),
    )
    .await?;
    let common_dir = git_stdout(
        host,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        cancel.clone(),
    )
    .await?;
    let git_dir = dunce::canonicalize(bytes_to_trimmed_path(&git_dir)?)
        .map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    let common_dir = dunce::canonicalize(bytes_to_trimmed_path(&common_dir)?)
        .map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    if git_dir != common_dir
        || !git_dir.starts_with(&root)
        || git_dir.join("commondir").exists()
        || !git_dir.is_dir()
    {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    let hooks = git_stdout(
        host,
        &["config", "--local", "--get", "core.hooksPath"],
        cancel.clone(),
    )
    .await?;
    if bytes_to_trimmed_str(&hooks)? != "/dev/null" {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    let remotes = git_stdout(host, &["remote"], cancel).await?;
    if !bytes_to_trimmed_str(&remotes)?.is_empty() {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    Ok(())
}

async fn repository_control_fingerprint(
    host: &GrokBuildHostLaunchConfig,
) -> Result<String, GrokBuildAdapterError> {
    let cancel = CancellationToken::new();
    let refs = git_stdout(
        host,
        &[
            "for-each-ref",
            "--sort=refname",
            "--format=%(refname)%00%(objectname)%00%(symref)%00",
        ],
        cancel.clone(),
    )
    .await?;
    let config = git_stdout(
        host,
        &["config", "--local", "--null", "--list"],
        cancel.clone(),
    )
    .await?;
    let head = git_stdout(host, &["rev-parse", "--verify", "HEAD"], cancel.clone()).await?;
    let symbolic = git_output(host, &["symbolic-ref", "-q", "HEAD"], cancel.clone()).await?;
    let common_dir = git_stdout(
        host,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        cancel,
    )
    .await?;
    let common_dir = dunce::canonicalize(bytes_to_trimmed_path(&common_dir)?)
        .map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    let hooks = common_dir.join("hooks");

    let mut hasher = Sha256::new();
    hasher.update(b"grokptah-repository-control-v1\0");
    for bytes in [&refs, &config, &head, &symbolic.stdout] {
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    let mut entries = 0usize;
    let mut bytes = 0usize;
    hash_bounded_control_path(&hooks, &mut hasher, &mut entries, &mut bytes)?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn hash_bounded_control_path(
    path: &Path,
    hasher: &mut Sha256,
    entries: &mut usize,
    bytes: &mut usize,
) -> Result<(), GrokBuildAdapterError> {
    const MAX_CONTROL_ENTRIES: usize = 256;
    const MAX_CONTROL_BYTES: usize = 1_048_576;
    *entries = entries
        .checked_add(1)
        .ok_or(GrokBuildAdapterError::IsolationFailed)?;
    if *entries > MAX_CONTROL_ENTRIES {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            hasher.update(b"missing\0");
            return Ok(());
        }
        Err(_) => return Err(GrokBuildAdapterError::IsolationFailed),
    };
    if metadata.file_type().is_symlink() {
        let target =
            std::fs::read_link(path).map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
        let target = target.as_os_str().as_encoded_bytes();
        hasher.update(b"symlink\0");
        hasher.update((target.len() as u64).to_be_bytes());
        hasher.update(target);
        return Ok(());
    }
    if metadata.file_type().is_file() {
        let contents = std::fs::read(path).map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
        *bytes = bytes
            .checked_add(contents.len())
            .ok_or(GrokBuildAdapterError::IsolationFailed)?;
        if *bytes > MAX_CONTROL_BYTES {
            return Err(GrokBuildAdapterError::IsolationFailed);
        }
        hasher.update(b"file\0");
        hasher.update((contents.len() as u64).to_be_bytes());
        hasher.update(contents);
        return Ok(());
    }
    if !metadata.file_type().is_dir() {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    hasher.update(b"dir\0");
    let mut children = std::fs::read_dir(path)
        .map_err(|_| GrokBuildAdapterError::IsolationFailed)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    children.sort_by_key(std::fs::DirEntry::file_name);
    for child in children {
        let name = child.file_name();
        let name = name.as_encoded_bytes();
        hasher.update((name.len() as u64).to_be_bytes());
        hasher.update(name);
        hash_bounded_control_path(&child.path(), hasher, entries, bytes)?;
    }
    Ok(())
}

async fn verify_source_repository_unchanged(
    host: &GrokBuildHostLaunchConfig,
    head_sha: &str,
    worktree_fingerprint: &str,
    control_fingerprint: &str,
) -> Result<(), GrokBuildAdapterError> {
    let current = crate::run_promotion::fingerprint_at(&host.cwd, head_sha)
        .map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    if current != worktree_fingerprint
        || repository_control_fingerprint(host).await? != control_fingerprint
    {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    Ok(())
}

struct AllowlistedExecution<'a> {
    source_host: &'a GrokBuildHostLaunchConfig,
    execution_host: &'a GrokBuildHostLaunchConfig,
    checkout: &'a IsolatedCheckout,
    source_fingerprint: &'a str,
    source_control_fingerprint: &'a str,
    isolated: &'a IsolatedHome,
    credentials: &'a dyn CredentialLeaseResolver,
    managed: Option<&'a (crate::managed_provider::ManagedChildPolicy, String)>,
    session_id: &'a str,
}

async fn execute_allowlisted(
    launch: &GrokBuildLaunchRequest,
    execution: AllowlistedExecution<'_>,
    cancel: CancellationToken,
) -> Result<GrokBuildAdapterOutcome, GrokBuildAdapterError> {
    let AllowlistedExecution {
        source_host,
        execution_host,
        checkout,
        source_fingerprint,
        source_control_fingerprint,
        isolated,
        credentials,
        managed,
        session_id,
    } = execution;
    let mut args = allowlisted_args(
        &isolated.prompt_path(),
        launch.mutation_mode,
        launch.max_turns,
        session_id,
    )?;
    let mut env = allowlisted_env(&isolated.path)?;
    let mut cmd = if let Some((policy, capability)) = managed {
        args.extend(["--model".into(), "managed-assignment".into(), "--tools".into(), "read_file,write,search_replace,list_dir,grep".into(), "--system-prompt-override".into(), "Inspect the source and implement the requested bounded change. Use only the provided file tools. Do not inspect credentials or execute commands. End with GROK_BUILD_VERDICT=clean when done, otherwise GROK_BUILD_VERDICT=not_complete.".into()]);
        env.push(("GROKPTAH_MANAGED_CAPABILITY".into(), capability.clone()));
        // Grok's startup auth gate requires this even for per-model env_key.
        // It is the same relay-only bearer, never an upstream xAI credential.
        env.push(("XAI_API_KEY".into(), capability.clone()));
        managed_sandbox_command(execution_host, &args, policy, &isolated.path)?
    } else {
        mutation_sandbox_command(execution_host, &args)?
    };
    cmd.current_dir(&execution_host.cwd);
    cmd.env_clear();
    cmd.envs(env);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    crate::process_tree::configure(&mut cmd);

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(_) => return Err(GrokBuildAdapterError::SpawnFailed),
    };

    let harvest = harvest_child(
        &mut child,
        execution_host.max_stdout_bytes,
        execution_host.max_stderr_bytes,
        Duration::from_millis(launch.max_duration_ms),
        cancel,
    )
    .await;
    if harvest.kind == HarvestKind::TerminationUnproven {
        // The original Work checkout was never exposed to this process. Keep
        // the possibly-live disposable checkout quarantined rather than
        // racing it with cleanup.
        checkout.quarantine();
        return Err(contain_uncertain_termination(
            credentials,
            &launch.credential_lease_id,
            isolated,
        ));
    }

    if let Some((_, capability)) = managed {
        let leaked = [&harvest.stdout, &harvest.stderr].iter().any(|bytes| {
            bytes
                .windows(capability.len())
                .any(|part| part == capability.as_bytes())
        }) || managed_material_leaked(&checkout.path, capability)
            || managed_material_leaked(&isolated.path, capability);
        if leaked {
            let _ = isolated.cleanup();
            let _ = checkout.cleanup().await;
            return Err(GrokBuildAdapterError::CredentialLease);
        }
    }

    if checkout.verify_control_state(execution_host).await.is_err()
        || verify_source_repository_unchanged(
            source_host,
            &launch.identity.head_sha,
            source_fingerprint,
            source_control_fingerprint,
        )
        .await
        .is_err()
    {
        let _ = isolated.cleanup();
        let _ = checkout.cleanup().await;
        return Err(GrokBuildAdapterError::IsolationFailed);
    }

    let readonly_violation = if launch.mutation_mode == GrokBuildMutationMode::ReadOnly {
        gate_detached_checkout_identity(launch, execution_host, CancellationToken::new())
            .await
            .is_err()
    } else {
        false
    };
    // A current production CLI omits accounting from stdout. Preserve the
    // legacy parser unchanged; production completion instead requires BOTH
    // its durable usage/turn journal and independently settled host sends.
    let classified = if managed.is_some() {
        credentials.revoke(&launch.credential_lease_id)?;
        classify_managed_harvest(&harvest, isolated, session_id, launch, credentials)
    } else {
        classify_harvest(&harvest, readonly_violation, isolated, session_id)
    };
    let cleaned = isolated.cleanup();
    let promote_mutation = classified.state == GrokBuildRunState::CompleteAdvisory
        && matches!(
            classified.verdict,
            Some(GrokBuildVerdict::Clean | GrokBuildVerdict::Findings)
        )
        && cleaned;
    let mutation_evidence = if launch.mutation_mode == GrokBuildMutationMode::IsolatedReview
        && promote_mutation
    {
        let captured = if source_host.defer_source_apply {
            let Some(retention) = source_host.candidate_retention_dir.as_ref() else {
                let _ = checkout.cleanup().await;
                return Err(GrokBuildAdapterError::InvalidRequest);
            };
            match capture_isolated_review_mutation(launch, execution_host, true).await {
                Ok(evidence) => {
                    if let Err(error) = crate::verified_change::retain_candidate_snapshot(
                        &execution_host.cwd,
                        retention,
                        &launch.identity.head_sha,
                    ) {
                        let _ = checkout.cleanup().await;
                        let _ = std::fs::remove_dir_all(retention);
                        return Err(
                            match error.message.contains("symlink")
                                || error.message.contains("escape")
                            {
                                true => GrokBuildAdapterError::ReadOnlyMutation,
                                false => GrokBuildAdapterError::IsolationFailed,
                            },
                        );
                    }
                    let binding = match crate::verified_change::bind_retained_candidate(
                        &execution_host.cwd,
                        retention,
                    ) {
                        Ok(binding) => binding,
                        Err(_) => {
                            let _ = checkout.cleanup().await;
                            let _ = std::fs::remove_dir_all(retention);
                            return Err(GrokBuildAdapterError::IsolationFailed);
                        }
                    };
                    let mut live_paths = evidence.changed_paths().to_vec();
                    live_paths.sort();
                    if binding.head != launch.identity.head_sha
                        || live_paths != binding.changed_paths
                        || (binding.git_ref != "HEAD" && binding.git_ref != launch.identity.git_ref)
                    {
                        let _ = checkout.cleanup().await;
                        let _ = std::fs::remove_dir_all(retention);
                        return Err(GrokBuildAdapterError::IsolationFailed);
                    }
                    Ok(GrokBuildMutationEvidence {
                        final_head_sha: binding.head,
                        final_ref: launch.identity.git_ref.clone(),
                        changed_paths: binding.changed_paths,
                        diff_digest: binding.diff_digest,
                    })
                }
                Err(error) => Err(error),
            }
        } else {
            promote_verified_isolated_review_mutation(
                launch,
                source_host,
                execution_host,
                source_fingerprint,
            )
            .await
        };
        match captured {
            Ok(evidence) => Some(evidence),
            Err(error) => {
                let _ = checkout.cleanup().await;
                return Err(error);
            }
        }
    } else {
        None
    };
    let checkout_cleaned = checkout.cleanup().await;
    if !checkout_cleaned {
        if mutation_evidence.is_some() {
            restore_failed_isolated_review_workspace(launch, source_host).await?;
        }
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    finish_outcome(
        launch,
        session_id,
        classified,
        !readonly_violation,
        cleaned && checkout_cleaned,
        mutation_evidence,
    )
}

async fn promote_verified_isolated_review_mutation(
    launch: &GrokBuildLaunchRequest,
    source_host: &GrokBuildHostLaunchConfig,
    execution_host: &GrokBuildHostLaunchConfig,
    source_fingerprint: &str,
) -> Result<GrokBuildMutationEvidence, GrokBuildAdapterError> {
    let isolated_evidence = capture_isolated_review_mutation(launch, execution_host, true).await?;
    gate_git_identity(launch, source_host, CancellationToken::new()).await?;
    gate_no_publish_remote(launch, source_host, CancellationToken::new()).await?;

    let snapshot = crate::run_promotion::snapshot(&execution_host.cwd, &launch.identity.head_sha)
        .map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    crate::run_promotion::promote(
        &source_host.cwd,
        &execution_host.cwd,
        &launch.identity.head_sha,
        source_fingerprint,
        &snapshot.fingerprint,
    )
    .map_err(|_| GrokBuildAdapterError::IsolationFailed)?;

    if let Err(error) = reset_index_for_paths(source_host, isolated_evidence.changed_paths()).await
    {
        let _ = restore_failed_isolated_review_workspace(launch, source_host).await;
        return Err(error);
    }
    let promoted_evidence = match capture_isolated_review_mutation(launch, source_host, false).await
    {
        Ok(evidence) => evidence,
        Err(error) => {
            let _ = restore_failed_isolated_review_workspace(launch, source_host).await;
            return Err(error);
        }
    };
    if promoted_evidence.changed_paths != isolated_evidence.changed_paths
        || promoted_evidence.diff_digest != isolated_evidence.diff_digest
    {
        restore_failed_isolated_review_workspace(launch, source_host).await?;
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    Ok(promoted_evidence)
}

async fn reset_index_for_paths(
    host: &GrokBuildHostLaunchConfig,
    paths: &[String],
) -> Result<(), GrokBuildAdapterError> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args = vec!["reset", "--quiet", "HEAD", "--"];
    args.extend(paths.iter().map(String::as_str));
    git_status_ok(host, &args, CancellationToken::new())
        .await
        .map_err(|_| GrokBuildAdapterError::IsolationFailed)
}

/// Restore the initially clean, exact-ref checkout after any untrusted child
/// run that cannot advance to bounded operator review. The adapter never uses
/// this to make a successful mutation look clean: failure to prove the exact
/// launch identity and an empty tree after restoration is itself fail closed.
async fn restore_failed_isolated_review_workspace(
    launch: &GrokBuildLaunchRequest,
    host: &GrokBuildHostLaunchConfig,
) -> Result<(), GrokBuildAdapterError> {
    let cancel = CancellationToken::new();
    git_status_ok(
        host,
        &["symbolic-ref", "HEAD", &launch.identity.git_ref],
        cancel.clone(),
    )
    .await
    .map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    git_status_ok(
        host,
        &["reset", "--hard", &launch.identity.head_sha],
        cancel.clone(),
    )
    .await
    .map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    git_status_ok(host, &["clean", "-ffdx", "--"], cancel.clone())
        .await
        .map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    gate_git_identity(launch, host, cancel.clone()).await?;
    gate_no_publish_remote(launch, host, cancel).await?;
    Ok(())
}

fn contain_uncertain_termination(
    credentials: &dyn CredentialLeaseResolver,
    credential_lease_id: &str,
    isolated: &IsolatedHome,
) -> GrokBuildAdapterError {
    // Evaluate every containment action even when an earlier one fails. The
    // stable error distinguishes an unproved process-tree stop with complete
    // authority containment from a failure to revoke that authority.
    let upstream_revoked = credentials.revoke(credential_lease_id).is_ok();
    let local_revoked = isolated.revoke_sensitive_material();
    let cleaned = isolated.cleanup();
    if upstream_revoked && local_revoked && cleaned {
        GrokBuildAdapterError::TerminationUnproven
    } else {
        GrokBuildAdapterError::CredentialRevocation
    }
}

fn finish_outcome(
    launch: &GrokBuildLaunchRequest,
    session_id: &str,
    classified: ClassifiedRun,
    permissions_ok: bool,
    cleaned: bool,
    mutation_evidence: Option<GrokBuildMutationEvidence>,
) -> Result<GrokBuildAdapterOutcome, GrokBuildAdapterError> {
    let mut state = classified.state;
    let mut verdict = classified.verdict;
    let mut cleanup = match state {
        GrokBuildRunState::CompleteAdvisory => GrokBuildCleanupState::Complete,
        GrokBuildRunState::NeedsSynthesis | GrokBuildRunState::Running => {
            GrokBuildCleanupState::Pending
        }
        GrokBuildRunState::FailedClosed => GrokBuildCleanupState::FailedClosed,
    };
    if !cleaned && state == GrokBuildRunState::CompleteAdvisory {
        state = GrokBuildRunState::FailedClosed;
        verdict = None;
        cleanup = GrokBuildCleanupState::FailedClosed;
    }

    let evidence = match state {
        GrokBuildRunState::CompleteAdvisory => classified.evidence_refs.clone(),
        GrokBuildRunState::NeedsSynthesis => vec!["nonresumable-run".to_string()],
        GrokBuildRunState::FailedClosed | GrokBuildRunState::Running => classified
            .evidence_refs
            .clone()
            .into_iter()
            .next()
            .map_or_else(|| vec!["closed-run".to_string()], |value| vec![value]),
    };

    let receipt = GrokBuildIsolationReceipt {
        contract_version: GROK_BUILD_CONTRACT_VERSION.to_string(),
        request_id: launch.request_id.clone(),
        identity: launch.identity.clone(),
        credential_lease_id: launch.credential_lease_id.clone(),
        isolated_home_alias: derived_id("h-", &launch.request_id, HOME_ALIAS_MAX),
        mcp_policy: GrokBuildPolicyState::Disabled,
        hooks_policy: GrokBuildPolicyState::Disabled,
        instruction_policy: GrokBuildPolicyState::Omitted,
        plugin_policy: GrokBuildPolicyState::Disabled,
        permission_policy: launch.mutation_mode,
        credential_present: true,
        permissions_ok,
        cleanup_state: cleanup,
    };
    let result = GrokBuildResult {
        request_id: launch.request_id.clone(),
        session_id: session_id.to_string(),
        identity: launch.identity.clone(),
        state,
        evidence_refs: evidence,
        terminal_verdict: verdict,
        nonclaims: required_nonclaims(),
    };
    result
        .validate_for_launch_and_receipt(launch, &receipt)
        .map_err(map_contract)?;
    let advisory_evidence = if state == GrokBuildRunState::CompleteAdvisory {
        classified.advisory_evidence
    } else {
        None
    };
    Ok(GrokBuildAdapterOutcome {
        receipt,
        result,
        advisory_evidence,
        mutation_evidence,
    })
}

struct Harvest {
    kind: HarvestKind,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HarvestKind {
    Exited(i32),
    Timeout,
    Cancelled,
    Overflow,
    TerminationUnproven,
}

struct ClassifiedRun {
    state: GrokBuildRunState,
    verdict: Option<GrokBuildVerdict>,
    evidence_refs: Vec<String>,
    advisory_evidence: Option<GrokBuildAdvisoryEvidence>,
}

fn classify_managed_harvest(
    harvest: &Harvest,
    isolated: &IsolatedHome,
    session: &str,
    launch: &GrokBuildLaunchRequest,
    credentials: &dyn CredentialLeaseResolver,
) -> ClassifiedRun {
    if harvest.kind != HarvestKind::Exited(0) {
        return failed_closed_classification("managed-worker-did-not-exit-cleanly");
    }
    let result = (|| {
        let provider = credentials
            .provider_evidence(&launch.credential_lease_id)
            .ok_or("managed-sends-missing")?;
        if !provider.accounting_complete
            || !provider.usage_observed
            || provider.uncertain
            || !provider.revoked
            || provider.wire_attempts == 0
            || provider.wire_attempts > launch.max_turns
            || provider.authority_attempts.len() != provider.wire_attempts as usize
        {
            return Err("managed-sends-unsettled");
        }
        let value: serde_json::Value =
            serde_json::from_slice(&harvest.stdout).map_err(|_| "managed-stdout-json")?;
        let object = value.as_object().ok_or("managed-stdout-shape")?;
        require_exact_keys(
            object,
            &[
                "text",
                "stopReason",
                "sessionId",
                "requestId",
                "usage",
                "modelUsage",
                "num_turns",
            ],
        )
        .map_err(|_| "managed-stdout-shape")?;
        if !managed_stdout_usage_matches(&value, &provider) {
            return Err("managed-stdout-usage-mismatch");
        }
        if value["stopReason"] != "end_turn" || value["sessionId"].as_str() != Some(session) {
            return Err("managed-terminal-identity");
        }
        let request = value["requestId"]
            .as_str()
            .ok_or("managed-request-identity")?;
        Uuid::parse_str(request).map_err(|_| "managed-request-identity")?;
        let text = value["text"]
            .as_str()
            .filter(|text| !text.is_empty() && text.len() <= ADVISORY_SUMMARY_MAX)
            .ok_or("managed-summary")?;
        let verdict = explicit_verdict(text.as_bytes(), &[]).ok_or("managed-verdict")?;
        if text
            .lines()
            .filter(|line| {
                !line.trim().is_empty() && !line.trim().starts_with("GROK_BUILD_VERDICT=")
            })
            .count()
            == 0
        {
            return Err("managed-summary");
        }
        let updates =
            retained_session_evidence(isolated, session, text).ok_or("managed-session-evidence")?;
        let terminal = updates
            .split(|byte| *byte == b'\n')
            .filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
            .find(|event| event["params"]["update"]["sessionUpdate"] == "turn_completed")
            .ok_or("managed-terminal-journal")?;
        if terminal["params"]["update"]["prompt_id"].as_str() != Some(request) {
            return Err("managed-terminal-request-binding");
        }
        let mut usage_paths = Vec::new();
        for workspace in
            fs::read_dir(isolated.path.join("sessions")).map_err(|_| "managed-usage-missing")?
        {
            let workspace = workspace.map_err(|_| "managed-usage-missing")?;
            let path = workspace.path().join(session).join("usage.json");
            if path.is_file() {
                usage_paths.push(path);
            }
        }
        if usage_paths.len() != 1 {
            return Err("managed-usage-identity");
        }
        let file = open_credential_source(&usage_paths[0]).map_err(|_| "managed-usage-missing")?;
        let mut bytes = Vec::new();
        file.take(INSPECT_OUTPUT_MAX as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "managed-usage-missing")?;
        if bytes.len() > INSPECT_OUTPUT_MAX {
            return Err("managed-usage-overflow");
        }
        let usage: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| "managed-usage-json")?;
        let stats = &usage["session"];
        if usage["sessionId"].as_str() != Some(session)
            || stats["modelCalls"].as_u64() != Some(u64::from(provider.wire_attempts))
            || stats["totalTokens"].as_u64()
                != Some(provider.input_tokens.saturating_add(provider.output_tokens))
            || stats["turnCount"]
                .as_u64()
                .is_none_or(|turns| turns == 0 || turns > u64::from(launch.max_turns))
            || stats["turnCount"] != value["num_turns"]
            || usage["turns"].as_array().is_none_or(Vec::is_empty)
        {
            return Err("managed-usage-does-not-match-host-sends");
        }
        let summary_ref = sha256_evidence_ref("summary", text.as_bytes());
        let session_ref = sha256_evidence_ref("session", &updates);
        let evidence = GrokBuildAdvisoryEvidence {
            cli_request_id: request.into(),
            summary: text.into(),
            session_updates: updates,
            summary_ref,
            session_ref,
        };
        Ok((verdict, evidence, sha256_evidence_ref("usage", &bytes)))
    })();
    match result {
        Ok((verdict, evidence, usage_ref)) => ClassifiedRun {
            state: GrokBuildRunState::CompleteAdvisory,
            verdict: Some(verdict),
            evidence_refs: vec![
                evidence.summary_ref.clone(),
                evidence.session_ref.clone(),
                usage_ref,
            ],
            advisory_evidence: Some(evidence),
        },
        Err(reason) => failed_closed_classification(reason),
    }
}

fn managed_stdout_usage_matches(
    value: &serde_json::Value,
    provider: &crate::managed_provider::ManagedProviderEvidence,
) -> bool {
    let Some(usage) = value["usage"].as_object() else {
        return false;
    };
    if require_exact_keys(
        usage,
        &[
            "input_tokens",
            "output_tokens",
            "total_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
            "reasoning_tokens",
        ],
    )
    .is_err()
        || usage.values().any(|value| value.as_u64().is_none())
        || value["usage"]["input_tokens"].as_u64() != Some(provider.input_tokens)
        || value["usage"]["output_tokens"].as_u64() != Some(provider.output_tokens)
        || value["usage"]["total_tokens"].as_u64()
            != Some(provider.input_tokens.saturating_add(provider.output_tokens))
        || value["num_turns"]
            .as_u64()
            .is_none_or(|turns| turns == 0 || turns > u64::from(provider.wire_attempts))
    {
        return false;
    }
    let Some(models) = value["modelUsage"].as_object() else {
        return false;
    };
    if models.len() != 1 {
        return false;
    }
    let Some(model) = models
        .get(&provider.model)
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    require_exact_keys(
        model,
        &[
            "inputTokens",
            "outputTokens",
            "modelCalls",
            "cacheCreationInputTokens",
            "cacheReadInputTokens",
        ],
    )
    .is_ok()
        && model.values().all(|value| value.as_u64().is_some())
        && model["inputTokens"].as_u64() == Some(provider.input_tokens)
        && model["outputTokens"].as_u64() == Some(provider.output_tokens)
        && model["modelCalls"].as_u64() == Some(u64::from(provider.wire_attempts))
        && model["cacheReadInputTokens"] == usage["cache_read_input_tokens"]
        && model["cacheCreationInputTokens"] == usage["cache_creation_input_tokens"]
}

fn classify_harvest(
    harvest: &Harvest,
    readonly_violation: bool,
    isolated: &IsolatedHome,
    expected_session_id: &str,
) -> ClassifiedRun {
    if readonly_violation {
        return ClassifiedRun {
            state: GrokBuildRunState::FailedClosed,
            verdict: None,
            evidence_refs: vec!["closed-run:read-only-mutation".into()],
            advisory_evidence: None,
        };
    }
    match harvest.kind {
        HarvestKind::Overflow => ClassifiedRun {
            state: GrokBuildRunState::FailedClosed,
            verdict: None,
            evidence_refs: vec!["closed-run:output-overflow".into()],
            advisory_evidence: None,
        },
        HarvestKind::Timeout => failed_closed_classification("timeout"),
        HarvestKind::Cancelled => failed_closed_classification("cancelled"),
        HarvestKind::TerminationUnproven => failed_closed_classification("termination-unproven"),
        HarvestKind::Exited(code) => {
            if has_max_turns(&harvest.stdout, &harvest.stderr) {
                return failed_closed_classification("max-turns");
            }
            if code != 0 {
                return failed_closed_classification("nonzero-exit");
            }
            match verified_advisory_evidence(&harvest.stdout, isolated, expected_session_id) {
                Ok((verdict, evidence)) => ClassifiedRun {
                    state: GrokBuildRunState::CompleteAdvisory,
                    verdict: Some(verdict),
                    evidence_refs: vec![evidence.summary_ref.clone(), evidence.session_ref.clone()],
                    advisory_evidence: Some(evidence),
                },
                Err(error) => failed_closed_classification(error.code()),
            }
        }
    }
}

fn failed_closed_classification(reason: &str) -> ClassifiedRun {
    ClassifiedRun {
        state: GrokBuildRunState::FailedClosed,
        verdict: None,
        evidence_refs: vec![format!("closed-run:{reason}")],
        advisory_evidence: None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdvisoryEvidenceError {
    StdoutJson,
    StdoutShape,
    Summary,
    StopReason,
    SessionId,
    RequestId,
    Usage,
    Verdict,
    SessionEvidence,
}

impl AdvisoryEvidenceError {
    const fn code(self) -> &'static str {
        match self {
            Self::StdoutJson => "stdout-json",
            Self::StdoutShape => "stdout-shape",
            Self::Summary => "summary",
            Self::StopReason => "stop-reason",
            Self::SessionId => "session-id",
            Self::RequestId => "request-id",
            Self::Usage => "usage-shape",
            Self::Verdict => "verdict",
            Self::SessionEvidence => "session-evidence",
        }
    }
}

fn verified_advisory_evidence(
    stdout: &[u8],
    isolated: &IsolatedHome,
    expected_session_id: &str,
) -> Result<(GrokBuildVerdict, GrokBuildAdvisoryEvidence), AdvisoryEvidenceError> {
    let value: serde_json::Value =
        serde_json::from_slice(stdout).map_err(|_| AdvisoryEvidenceError::StdoutJson)?;
    let object = value
        .as_object()
        .ok_or(AdvisoryEvidenceError::StdoutShape)?;
    require_exact_keys(
        object,
        &[
            "text",
            "stopReason",
            "sessionId",
            "requestId",
            "thought",
            "usage",
            "num_turns",
            "total_cost_usd",
            "total_cost_usd_ticks",
            "modelUsage",
        ],
    )
    .map_err(|_| AdvisoryEvidenceError::StdoutShape)?;
    let text = object
        .get("text")
        .and_then(serde_json::Value::as_str)
        .ok_or(AdvisoryEvidenceError::Summary)?;
    if text.trim().is_empty() || text.len() > ADVISORY_SUMMARY_MAX {
        return Err(AdvisoryEvidenceError::Summary);
    }
    if object.get("stopReason").and_then(serde_json::Value::as_str) != Some("end_turn") {
        return Err(AdvisoryEvidenceError::StopReason);
    }
    if object.get("sessionId").and_then(serde_json::Value::as_str) != Some(expected_session_id) {
        return Err(AdvisoryEvidenceError::SessionId);
    }
    let cli_request_id = object
        .get("requestId")
        .and_then(serde_json::Value::as_str)
        .ok_or(AdvisoryEvidenceError::RequestId)?;
    Uuid::parse_str(cli_request_id).map_err(|_| AdvisoryEvidenceError::RequestId)?;
    if object
        .get("thought")
        .and_then(serde_json::Value::as_str)
        .is_none()
        || object
            .get("usage")
            .and_then(serde_json::Value::as_object)
            .is_none()
        || object
            .get("num_turns")
            .and_then(serde_json::Value::as_u64)
            .is_none()
        || object
            .get("total_cost_usd")
            .and_then(serde_json::Value::as_f64)
            .is_none()
        || object
            .get("total_cost_usd_ticks")
            .and_then(serde_json::Value::as_u64)
            .is_none()
        || object
            .get("modelUsage")
            .and_then(serde_json::Value::as_object)
            .is_none()
    {
        return Err(AdvisoryEvidenceError::Usage);
    }
    let verdict = explicit_verdict(text.as_bytes(), &[]).ok_or(AdvisoryEvidenceError::Verdict)?;
    let summary_lines = text
        .as_bytes()
        .split(|byte| *byte == b'\n')
        .map(trim_ascii)
        .filter(|line| !line.is_empty())
        .filter(|line| {
            *line != VERDICT_CLEAN && *line != VERDICT_FINDINGS && *line != VERDICT_NOT_COMPLETE
        })
        .count();
    if summary_lines == 0 {
        return Err(AdvisoryEvidenceError::Summary);
    }
    let session_updates = retained_session_evidence(isolated, expected_session_id, text)
        .ok_or(AdvisoryEvidenceError::SessionEvidence)?;
    let summary_ref = sha256_evidence_ref("summary", text.as_bytes());
    let session_ref = sha256_evidence_ref("session", &session_updates);
    Ok((
        verdict,
        GrokBuildAdvisoryEvidence {
            cli_request_id: cli_request_id.to_string(),
            summary: text.to_string(),
            session_updates,
            summary_ref,
            session_ref,
        },
    ))
}

fn retained_session_evidence(
    isolated: &IsolatedHome,
    session_id: &str,
    expected_summary: &str,
) -> Option<Vec<u8>> {
    let root = isolated.path.join("sessions");
    let mut matches = Vec::new();
    for workspace in std::fs::read_dir(root).ok()? {
        let workspace = workspace.ok()?;
        let file_type = workspace.file_type().ok()?;
        if file_type.is_file() && allowed_session_root_metadata(&workspace.file_name()) {
            continue;
        }
        if !file_type.is_dir() {
            return None;
        }
        let candidate = workspace.path().join(session_id);
        match std::fs::symlink_metadata(&candidate) {
            Ok(meta) if meta.file_type().is_dir() && !meta.file_type().is_symlink() => {
                matches.push(candidate)
            }
            Ok(_) => return None,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    if matches.len() != 1 {
        return None;
    }
    let updates = matches.pop()?.join("updates.jsonl");
    let metadata = std::fs::symlink_metadata(&updates).ok()?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > SESSION_EVIDENCE_MAX
    {
        return None;
    }
    let file = open_credential_source(&updates).ok()?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(SESSION_EVIDENCE_MAX + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 != metadata.len() || bytes.len() as u64 > SESSION_EVIDENCE_MAX {
        return None;
    }
    let mut agent_message_bytes = Vec::new();
    let mut saw_agent_message = false;
    let mut saw_terminal = false;
    let mut last_update_was_agent_message = false;
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_slice(line).ok()?;
        let object = value.as_object()?;
        require_exact_keys(object, &["method", "params", "timestamp"]).ok()?;
        if !valid_session_timestamp(object.get("timestamp")?) {
            return None;
        }
        let method = object.get("method")?.as_str()?;
        if method != "session/update" && method != "_x.ai/session/update" {
            return None;
        }
        let params = object.get("params")?.as_object()?;
        require_exact_keys(params, &["_meta", "sessionId", "update"]).ok()?;
        if params.get("sessionId")?.as_str()? != session_id {
            return None;
        }
        let update = params.get("update")?.as_object()?;
        let update_type = update.get("sessionUpdate")?.as_str()?;
        if saw_terminal {
            return None;
        }
        if update_type == "agent_message_chunk" {
            let content = update.get("content")?.as_object()?;
            require_exact_keys(content, &["type", "text"]).ok()?;
            if content.get("type")?.as_str()? != "text" {
                return None;
            }
            let text = content.get("text")?.as_str()?;
            if text.is_empty() {
                return None;
            }
            agent_message_bytes.extend_from_slice(text.as_bytes());
            saw_agent_message = true;
            last_update_was_agent_message = true;
            continue;
        }
        if update_type == "turn_completed" {
            if !last_update_was_agent_message
                || update
                    .get("stop_reason")
                    .and_then(serde_json::Value::as_str)
                    != Some("end_turn")
            {
                return None;
            }
            saw_terminal = true;
            continue;
        }
        last_update_was_agent_message = false;
    }
    (saw_terminal && saw_agent_message && agent_message_bytes == expected_summary.as_bytes())
        .then_some(bytes)
}

fn allowed_session_root_metadata(name: &OsStr) -> bool {
    matches!(
        name.to_str(),
        Some("session_search.sqlite" | "session_search.sqlite-shm" | "session_search.sqlite-wal")
    )
}

fn valid_session_timestamp(value: &serde_json::Value) -> bool {
    match value {
        // Grok 1.0.5 emitted RFC3339 strings. Grok 1.0.13 emits integer Unix
        // seconds. Accept only those two bounded, nonempty representations;
        // floats, negative values, null, arrays, and objects remain invalid.
        serde_json::Value::String(value) => !value.is_empty() && value.len() <= 128,
        serde_json::Value::Number(value) => value.as_u64().is_some(),
        _ => false,
    }
}

fn sha256_evidence_ref(kind: &str, bytes: &[u8]) -> String {
    format!("{kind}-sha256-{digest:x}", digest = Sha256::digest(bytes))
}

fn explicit_verdict(stdout: &[u8], stderr: &[u8]) -> Option<GrokBuildVerdict> {
    if [stdout, stderr]
        .into_iter()
        .flat_map(|buf| buf.split(|b| *b == b'\n'))
        .map(trim_ascii)
        .filter(|line| {
            *line == VERDICT_CLEAN || *line == VERDICT_FINDINGS || *line == VERDICT_NOT_COMPLETE
        })
        .count()
        != 1
    {
        return None;
    }
    let last = stdout
        .split(|b| *b == b'\n')
        .map(trim_ascii)
        .rfind(|line| !line.is_empty())?;
    match last {
        VERDICT_CLEAN => Some(GrokBuildVerdict::Clean),
        VERDICT_FINDINGS => Some(GrokBuildVerdict::Findings),
        VERDICT_NOT_COMPLETE => Some(GrokBuildVerdict::NotComplete),
        _ => None,
    }
}

fn has_max_turns(stdout: &[u8], stderr: &[u8]) -> bool {
    for buf in [stdout, stderr] {
        if has_line(buf, MAX_TURNS_TOKEN) || has_line(buf, MAX_TURNS_PLAIN) {
            return true;
        }
    }
    false
}

fn has_line(buf: &[u8], token: &[u8]) -> bool {
    buf.split(|b| *b == b'\n').any(|line| {
        let line = trim_ascii(line);
        line == token
    })
}

fn trim_ascii(mut line: &[u8]) -> &[u8] {
    while line
        .first()
        .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r'))
    {
        line = &line[1..];
    }
    while line
        .last()
        .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r'))
    {
        line = &line[..line.len() - 1];
    }
    line
}

async fn harvest_child(
    child: &mut tokio::process::Child,
    max_stdout: usize,
    max_stderr: usize,
    limit: Duration,
    cancel: CancellationToken,
) -> Harvest {
    let mut stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let kind = if terminate_and_confirm(child).await {
                HarvestKind::Cancelled
            } else {
                HarvestKind::TerminationUnproven
            };
            return Harvest {
                kind,
                stdout: Vec::new(),
                stderr: Vec::new(),
            };
        }
    };
    let mut stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let kind = if terminate_and_confirm(child).await {
                HarvestKind::Cancelled
            } else {
                HarvestKind::TerminationUnproven
            };
            return Harvest {
                kind,
                stdout: Vec::new(),
                stderr: Vec::new(),
            };
        }
    };

    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut out_tmp = [0u8; 4096];
    let mut err_tmp = [0u8; 4096];
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut status: Option<i32> = None;
    let process_group = child.id();
    let deadline = Instant::now() + limit;

    let kind = loop {
        if status.is_some() && stdout_done && stderr_done {
            #[cfg(unix)]
            if let Some(pid) = process_group {
                if !process_group_gone(pid) {
                    let _ = terminate_and_confirm(child).await;
                    break HarvestKind::TerminationUnproven;
                }
            }
            break HarvestKind::Exited(status.unwrap_or(1));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break if terminate_and_confirm(child).await {
                HarvestKind::Timeout
            } else {
                HarvestKind::TerminationUnproven
            };
        }
        tokio::select! {
            _ = cancel.cancelled() => {
                break if terminate_and_confirm(child).await {
                    HarvestKind::Cancelled
                } else {
                    HarvestKind::TerminationUnproven
                };
            }
            _ = tokio::time::sleep(remaining) => {
                break if terminate_and_confirm(child).await {
                    HarvestKind::Timeout
                } else {
                    HarvestKind::TerminationUnproven
                };
            }
            n = stdout.read(&mut out_tmp), if !stdout_done => {
                match n {
                    Ok(0) => stdout_done = true,
                    Ok(n) if out.len().saturating_add(n) > max_stdout => {
                        out.clear();
                        err.clear();
                        break if terminate_and_confirm(child).await {
                            HarvestKind::Overflow
                        } else {
                            HarvestKind::TerminationUnproven
                        };
                    }
                    Ok(n) => out.extend_from_slice(&out_tmp[..n]),
                    Err(_) => stdout_done = true,
                }
            }
            n = stderr.read(&mut err_tmp), if !stderr_done => {
                match n {
                    Ok(0) => stderr_done = true,
                    Ok(n) if err.len().saturating_add(n) > max_stderr => {
                        out.clear();
                        err.clear();
                        break if terminate_and_confirm(child).await {
                            HarvestKind::Overflow
                        } else {
                            HarvestKind::TerminationUnproven
                        };
                    }
                    Ok(n) => err.extend_from_slice(&err_tmp[..n]),
                    Err(_) => stderr_done = true,
                }
            }
            wait_res = child.wait(), if status.is_none() => {
                status = Some(wait_res.map(|s| s.code().unwrap_or(1)).unwrap_or(1));
            }
        }
    };

    if !matches!(kind, HarvestKind::Exited(_)) {
        out.clear();
        err.clear();
        return Harvest {
            kind,
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
    }

    Harvest {
        kind,
        stdout: out,
        stderr: err,
    }
}

#[cfg(unix)]
fn process_group_gone(pid: u32) -> bool {
    let status = unsafe { libc::kill(-(pid as i32), 0) };
    status != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

async fn terminate_and_confirm(child: &mut tokio::process::Child) -> bool {
    let process_group = child.id();
    #[cfg(windows)]
    let tree_killed = if let Some(pid) = process_group {
        let mut taskkill = Command::new("taskkill");
        taskkill.args(["/PID", &pid.to_string(), "/T", "/F"]);
        crate::spawn_env::scrub_tokio_command(&mut taskkill);
        matches!(
            tokio::time::timeout(Duration::from_secs(3), taskkill.status()).await,
            Ok(Ok(status)) if status.success()
        )
    } else {
        false
    };
    crate::process_tree::terminate_now(child);
    let leader_reaped = matches!(
        tokio::time::timeout(Duration::from_secs(3), child.wait()).await,
        Ok(Ok(_))
    ) || matches!(child.try_wait(), Ok(Some(_)));
    if !leader_reaped {
        return false;
    }
    #[cfg(unix)]
    {
        let Some(process_group) = process_group else {
            return true;
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let status = unsafe { libc::kill(-(process_group as i32), 0) };
            if status != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    #[cfg(not(unix))]
    {
        #[cfg(windows)]
        {
            tree_killed
        }
        #[cfg(not(windows))]
        {
            // No platform tree-kill receipt is available. Reaping only the
            // leader is not descendant proof, so retain a fail-closed result.
            false
        }
    }
}

async fn git_stdout(
    host: &GrokBuildHostLaunchConfig,
    args: &[&str],
    cancel: CancellationToken,
) -> Result<Vec<u8>, GrokBuildAdapterError> {
    let output = git_output(host, args, cancel).await?;
    if !output.status {
        return Err(GrokBuildAdapterError::IdentityMismatch);
    }
    Ok(output.stdout)
}

fn validate_allowed_files(files: &[String]) -> Result<(), GrokBuildAdapterError> {
    if files.len() > 64 {
        return Err(GrokBuildAdapterError::InvalidRequest);
    }
    let mut seen = HashSet::with_capacity(files.len());
    for path in files {
        if path.is_empty()
            || path.len() > 512
            || path.starts_with('/')
            || path.ends_with('/')
            || path.contains('\0')
            || path.contains('\\')
            || path.contains("//")
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || !seen.insert(path.as_str())
        {
            return Err(GrokBuildAdapterError::InvalidRequest);
        }
    }
    Ok(())
}

async fn capture_isolated_review_mutation(
    launch: &GrokBuildLaunchRequest,
    host: &GrokBuildHostLaunchConfig,
    detached: bool,
) -> Result<GrokBuildMutationEvidence, GrokBuildAdapterError> {
    let cancel = CancellationToken::new();
    let head = git_stdout(host, &["rev-parse", "--verify", "HEAD"], cancel.clone()).await?;
    let head = bytes_to_trimmed_str(&head)?.to_string();
    if head != launch.identity.head_sha {
        return Err(GrokBuildAdapterError::IdentityMismatch);
    }
    let git_ref = if detached {
        let symbolic = git_output(host, &["symbolic-ref", "-q", "HEAD"], cancel.clone()).await?;
        if symbolic.status || !symbolic.stdout.is_empty() {
            return Err(GrokBuildAdapterError::IdentityMismatch);
        }
        launch.identity.git_ref.clone()
    } else {
        let observed = git_stdout(host, &["symbolic-ref", "HEAD"], cancel.clone()).await?;
        let observed = bytes_to_trimmed_str(&observed)?.to_string();
        if observed != launch.identity.git_ref {
            return Err(GrokBuildAdapterError::IdentityMismatch);
        }
        observed
    };
    let status = git_stdout(
        host,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
        ],
        cancel.clone(),
    )
    .await?;
    let mut changed_paths = porcelain_paths(&status)?;
    changed_paths.sort();
    changed_paths.dedup();
    if changed_paths
        .iter()
        .any(|path| !host.allowed_files.iter().any(|allowed| allowed == path))
    {
        return Err(GrokBuildAdapterError::ReadOnlyMutation);
    }

    let mut hasher = Sha256::new();
    hasher.update(b"grokptah-managed-grok-mutation-v1\0");
    hasher.update((status.len() as u64).to_be_bytes());
    hasher.update(&status);
    let mut total_bytes = 0usize;
    for path in &changed_paths {
        hasher.update((path.len() as u64).to_be_bytes());
        hasher.update(path.as_bytes());
        let absolute = host.cwd.join(path);
        match std::fs::symlink_metadata(&absolute) {
            Ok(meta) if meta.file_type().is_file() => {
                let file_len = usize::try_from(meta.len())
                    .map_err(|_| GrokBuildAdapterError::OutputOverflow)?;
                total_bytes = total_bytes
                    .checked_add(file_len)
                    .ok_or(GrokBuildAdapterError::OutputOverflow)?;
                if total_bytes > OUTPUT_BYTES_MAX {
                    return Err(GrokBuildAdapterError::OutputOverflow);
                }
                let contents =
                    std::fs::read(&absolute).map_err(|_| GrokBuildAdapterError::DirtyTree)?;
                if contents.len() != file_len {
                    return Err(GrokBuildAdapterError::DirtyTree);
                }
                hasher.update(b"file\0");
                hasher.update((contents.len() as u64).to_be_bytes());
                hasher.update(contents);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                hasher.update(b"deleted\0");
            }
            _ => return Err(GrokBuildAdapterError::DirtyTree),
        }
    }

    // Close the status/content capture window. A concurrent mutation or ref
    // move makes the proof unusable rather than producing stale evidence.
    let final_status = git_stdout(
        host,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
        ],
        cancel.clone(),
    )
    .await?;
    let final_head = git_stdout(host, &["rev-parse", "--verify", "HEAD"], cancel.clone()).await?;
    let final_ref_matches = if detached {
        let symbolic = git_output(host, &["symbolic-ref", "-q", "HEAD"], cancel).await?;
        !symbolic.status && symbolic.stdout.is_empty()
    } else {
        let final_ref = git_stdout(host, &["symbolic-ref", "HEAD"], cancel).await?;
        bytes_to_trimmed_str(&final_ref)? == git_ref
    };
    if final_status != status || bytes_to_trimmed_str(&final_head)? != head || !final_ref_matches {
        return Err(GrokBuildAdapterError::DirtyTree);
    }

    Ok(GrokBuildMutationEvidence {
        final_head_sha: head,
        final_ref: git_ref,
        changed_paths,
        diff_digest: format!("sha256:{:x}", hasher.finalize()),
    })
}

async fn git_status_ok(
    host: &GrokBuildHostLaunchConfig,
    args: &[&str],
    cancel: CancellationToken,
) -> Result<(), GrokBuildAdapterError> {
    let output = git_output(host, args, cancel).await?;
    if !output.status {
        return Err(GrokBuildAdapterError::IdentityMismatch);
    }
    Ok(())
}

struct GitOutput {
    status: bool,
    stdout: Vec<u8>,
}

async fn git_output(
    host: &GrokBuildHostLaunchConfig,
    args: &[&str],
    cancel: CancellationToken,
) -> Result<GitOutput, GrokBuildAdapterError> {
    let mut cmd = Command::new(&host.git_executable);
    cmd.current_dir(&host.cwd);
    cmd.env_clear();
    cmd.env("GIT_CONFIG_NOSYSTEM", "1");
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
    cmd.env("GIT_CONFIG_SYSTEM", "/dev/null");
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.env("GIT_OPTIONAL_LOCKS", "0");
    cmd.env("PATH", BOUNDED_PATH);
    cmd.args([
        "--no-replace-objects",
        "--no-optional-locks",
        "-c",
        "core.hooksPath=/dev/null",
    ]);
    cmd.args(args);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|_| GrokBuildAdapterError::IdentityMismatch)?;
    let harvest = harvest_child(
        &mut child,
        GIT_OUTPUT_MAX,
        GIT_OUTPUT_MAX,
        host.git_timeout,
        cancel,
    )
    .await;
    match harvest.kind {
        HarvestKind::Exited(0) => Ok(GitOutput {
            status: true,
            stdout: harvest.stdout,
        }),
        HarvestKind::Exited(_) => Ok(GitOutput {
            status: false,
            stdout: Vec::new(),
        }),
        _ => Err(GrokBuildAdapterError::IdentityMismatch),
    }
}

fn porcelain_paths(raw: &[u8]) -> Result<Vec<String>, GrokBuildAdapterError> {
    let mut paths = Vec::new();
    let mut rest = raw;
    while !rest.is_empty() {
        if rest == b"\n" || rest == b"\0" {
            break;
        }
        if rest.len() < 3 {
            return Err(GrokBuildAdapterError::DirtyTree);
        }
        let code0 = rest[0];
        let rename = code0 == b'R' || code0 == b'C';
        rest = &rest[3..];
        let (first, after) = split_nul(rest)?;
        paths.push(bytes_to_str(first)?);
        rest = after;
        if rename {
            let (second, after) = split_nul(rest)?;
            paths.push(bytes_to_str(second)?);
            rest = after;
        }
    }
    Ok(paths)
}

fn split_nul(raw: &[u8]) -> Result<(&[u8], &[u8]), GrokBuildAdapterError> {
    let idx = raw
        .iter()
        .position(|b| *b == 0)
        .ok_or(GrokBuildAdapterError::DirtyTree)?;
    Ok((&raw[..idx], &raw[idx + 1..]))
}

fn bytes_to_str(raw: &[u8]) -> Result<String, GrokBuildAdapterError> {
    let text = std::str::from_utf8(raw).map_err(|_| GrokBuildAdapterError::DirtyTree)?;
    if text.is_empty() || text.contains('\0') {
        return Err(GrokBuildAdapterError::DirtyTree);
    }
    Ok(text.to_string())
}

fn bytes_to_trimmed_str(raw: &[u8]) -> Result<&str, GrokBuildAdapterError> {
    let text = std::str::from_utf8(raw).map_err(|_| GrokBuildAdapterError::IdentityMismatch)?;
    Ok(text.trim())
}

fn bytes_to_trimmed_path(raw: &[u8]) -> Result<&Path, GrokBuildAdapterError> {
    Ok(Path::new(bytes_to_trimmed_str(raw)?))
}

fn required_nonclaims() -> Vec<GrokBuildNonclaim> {
    vec![
        GrokBuildNonclaim::AdvisoryOnly,
        GrokBuildNonclaim::NotManagerImplementation,
        GrokBuildNonclaim::NotHostAuthority,
        GrokBuildNonclaim::NotProviderAccount,
        GrokBuildNonclaim::NotLiveQualified,
        GrokBuildNonclaim::NotMergeAuthority,
        GrokBuildNonclaim::NotComputerUse,
    ]
}

fn derived_id(prefix: &str, request_id: &str, max: usize) -> String {
    let mut out = String::with_capacity(prefix.len() + request_id.len());
    out.push_str(prefix);
    out.push_str(request_id);
    if out.len() > max {
        out.truncate(max);
        while out
            .as_bytes()
            .last()
            .is_some_and(|b| matches!(*b, b'.' | b'_' | b'-' | b':'))
        {
            out.pop();
        }
    }
    out
}

fn require_absolute_file_path(path: &Path) -> Result<(), GrokBuildAdapterError> {
    if !path.is_absolute() || !path_bytes_ok(path) {
        return Err(GrokBuildAdapterError::InvalidRequest);
    }
    if path
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.starts_with('-'))
    {
        return Err(GrokBuildAdapterError::InvalidRequest);
    }
    Ok(())
}

fn require_absolute_dir(path: &Path) -> Result<(), GrokBuildAdapterError> {
    if !path.is_absolute() || !path_bytes_ok(path) || !path.is_dir() {
        return Err(GrokBuildAdapterError::InvalidRequest);
    }
    Ok(())
}

fn path_bytes_ok(path: &Path) -> bool {
    !path.as_os_str().as_encoded_bytes().contains(&0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct RevocationProbe {
        called: AtomicBool,
        fail: bool,
    }

    impl CredentialLeaseResolver for RevocationProbe {
        fn resolve(&self, _lease_id: &str) -> Result<CredentialLeaseHandle, GrokBuildAdapterError> {
            Err(GrokBuildAdapterError::CredentialLease)
        }

        fn revoke(&self, lease_id: &str) -> Result<(), GrokBuildAdapterError> {
            assert_eq!(lease_id, "lease-1");
            self.called.store(true, Ordering::SeqCst);
            if self.fail {
                Err(GrokBuildAdapterError::CredentialRevocation)
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn file_truncation_does_not_revoke_an_already_read_lease() {
        let dir = tempfile::tempdir().expect("dir");
        let authority = HostLeaseAuthority::scoped(dir.path().to_path_buf(), "workspace-a");
        authority.resolve("lease-1").expect("resolve");
        let lease_path = dir.path().join("lease-lease-1");
        let token = std::fs::read_to_string(&lease_path).expect("child read");
        assert!(authority.provider_accepts(&token));
        std::fs::write(&lease_path, []).unwrap();
        std::fs::remove_file(&lease_path).unwrap();
        assert!(
            authority.provider_accepts(&token),
            "deleting the lease file must not revoke a token the child already read"
        );
        let opaque = dir.path().join("opaque");
        std::fs::write(&opaque, token.as_bytes()).unwrap();
        FileCredentialLease::new("lease-1", opaque.clone())
            .revoke("lease-1")
            .unwrap();
        assert!(!opaque.exists());
        assert!(
            authority.provider_accepts(&token),
            "FileCredentialLease revoke does not invalidate the token the child read"
        );
        authority.revoke("lease-1").expect("revoke");
        assert!(!authority.provider_accepts(&token));
    }

    #[test]
    fn uncertain_termination_revokes_upstream_and_local_authority() {
        let parent = tempfile::tempdir().expect("parent");
        let isolated = IsolatedHome::create(parent.path()).expect("home");
        isolated.install_prompt("bounded prompt").expect("prompt");
        let resolver = RevocationProbe {
            called: AtomicBool::new(false),
            fail: false,
        };
        let error = contain_uncertain_termination(&resolver, "lease-1", &isolated);
        assert_eq!(error, GrokBuildAdapterError::TerminationUnproven);
        assert!(resolver.called.load(Ordering::SeqCst));
        assert!(!isolated.path.exists());

        let isolated = IsolatedHome::create(parent.path()).expect("second home");
        isolated.install_prompt("bounded prompt").expect("prompt");
        let resolver = RevocationProbe {
            called: AtomicBool::new(false),
            fail: true,
        };
        let error = contain_uncertain_termination(&resolver, "lease-1", &isolated);
        assert_eq!(error, GrokBuildAdapterError::CredentialRevocation);
        assert!(resolver.called.load(Ordering::SeqCst));
        assert!(!isolated.path.exists());
    }

    #[test]
    fn allowlisted_argv_is_exact() {
        let args = allowlisted_args(
            Path::new("/isolated/prompt"),
            GrokBuildMutationMode::IsolatedReview,
            8,
            "00000000-0000-4000-8000-000000000001",
        )
        .expect("args");
        assert_eq!(
            args,
            [
                "--prompt-file",
                "/isolated/prompt",
                "--permission-mode",
                "bypassPermissions",
                "--disable-web-search",
                "--no-subagents",
                "--max-turns",
                "8",
                "--session-id",
                "00000000-0000-4000-8000-000000000001",
                "--output-format",
                "json",
            ]
        );
        assert!(!args.iter().any(|a| a.contains("yolo")
            || a.contains("model")
            || a.contains("resume")
            || a.contains("subagent") && !a.contains("no-subagents")));
        let readonly = allowlisted_args(
            Path::new("/isolated/prompt"),
            GrokBuildMutationMode::ReadOnly,
            2,
            "00000000-0000-4000-8000-000000000002",
        )
        .expect("readonly args");
        assert_eq!(readonly[3], "plan");
        assert_eq!(readonly[7], "grokptah_read_only");
        assert!(!readonly
            .iter()
            .any(|a| a == "acceptEdits" || a == "bypassPermissions"));
    }

    #[test]
    fn allowlisted_env_is_exact() {
        let env = allowlisted_env(Path::new("/isolated/home")).expect("env");
        assert_eq!(
            env,
            [
                ("GROK_HOME".to_string(), "/isolated/home".to_string()),
                ("HOME".to_string(), "/isolated/home".to_string()),
                ("TMPDIR".to_string(), "/isolated/home".to_string()),
                ("PATH".to_string(), BOUNDED_PATH.to_string()),
                (
                    "CLAUDE_CONFIG_DIR".to_string(),
                    "/isolated/home/claude-config".to_string(),
                ),
            ]
        );
        assert!(!env.iter().any(|(k, _)| k.contains("KEY")
            || k.contains("TOKEN")
            || k.contains("SECRET")
            || k.contains("XAI")));
    }

    #[test]
    fn lease_handle_debug_redacts_location() {
        let handle =
            CredentialLeaseHandle::from_host_path(PathBuf::from("/tmp/sk-live-secret-not-real"));
        let rendered = format!("{handle:?}");
        assert!(!rendered.contains("sk-"));
        assert!(!rendered.contains("/tmp"));
        assert!(!rendered.contains("secret"));
        assert!(rendered.contains("present"));
    }

    #[test]
    fn host_config_debug_omits_prompt_and_paths() {
        let host = GrokBuildHostLaunchConfig {
            executable: PathBuf::from("/usr/bin/grok"),
            git_executable: PathBuf::from("/usr/bin/git"),
            cwd: PathBuf::from("/tmp/repo"),
            repository_id: "repo-acme".into(),
            base_ref: "base".into(),
            prompt: "review the private key sk-live-secret-not-real".into(),
            allowed_files: vec!["README.md".into()],
            execution_approved: true,
            max_stdout_bytes: 32,
            max_stderr_bytes: 32,
            git_timeout: Duration::from_secs(1),
            isolate_parent: PathBuf::from("/tmp/iso"),
            defer_source_apply: false,
            candidate_retention_dir: None,
        };
        let rendered = format!("{host:?}");
        assert!(!rendered.contains("sk-"));
        assert!(!rendered.contains("private key"));
        assert!(!rendered.contains("/usr/bin/grok"));
        assert!(!rendered.contains("/tmp/repo"));
    }

    #[test]
    fn max_turns_never_classifies_complete() {
        let parent = tempfile::tempdir().expect("parent");
        let isolated = IsolatedHome::create(parent.path()).expect("home");
        let harvest = Harvest {
            kind: HarvestKind::Exited(0),
            stdout: b"GROK_BUILD_VERDICT=clean\nmax_turns_reached\n".to_vec(),
            stderr: Vec::new(),
        };
        let classified = classify_harvest(&harvest, false, &isolated, "session-1");
        assert_eq!(classified.state, GrokBuildRunState::FailedClosed);
        assert_eq!(classified.verdict, None);
    }

    #[test]
    fn missing_verdict_fails_closed() {
        let parent = tempfile::tempdir().expect("parent");
        let isolated = IsolatedHome::create(parent.path()).expect("home");
        let harvest = Harvest {
            kind: HarvestKind::Exited(0),
            stdout: b"review complete without marker\n".to_vec(),
            stderr: Vec::new(),
        };
        let classified = classify_harvest(&harvest, false, &isolated, "session-1");
        assert_eq!(classified.state, GrokBuildRunState::FailedClosed);
    }

    #[test]
    fn porcelain_parses_nul_records() {
        let raw = b" M src/lib.rs\0?? notes.md\0";
        let paths = porcelain_paths(raw).expect("paths");
        assert_eq!(paths, ["src/lib.rs", "notes.md"]);
    }

    #[cfg(unix)]
    #[test]
    fn isolated_home_and_credential_files_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let parent = tempfile::tempdir().expect("parent");
        let source_dir = tempfile::tempdir().expect("source");
        let source = source_dir.path().join("auth-source");
        std::fs::write(&source, b"credential").expect("source");
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600))
            .expect("source mode");
        let isolated = IsolatedHome::create(parent.path()).expect("home");
        isolated.write_minimal_config().expect("config");
        isolated.install_prompt("prompt").expect("prompt");
        isolated
            .install_lease(&CredentialLeaseHandle::from_host_path(source))
            .expect("lease");

        assert_eq!(
            std::fs::metadata(&isolated.path)
                .expect("home metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        for name in [CONFIG_FILE_NAME, PROMPT_FILE_NAME, AUTH_FILE_NAME] {
            assert_eq!(
                std::fs::metadata(isolated.path.join(name))
                    .expect("file metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600,
                "{name}"
            );
        }

        let mut open_auth = OpenOptions::new()
            .read(true)
            .open(isolated.path.join(AUTH_FILE_NAME))
            .expect("open auth before revocation");
        assert!(isolated.revoke_sensitive_material());
        assert!(!isolated.path.join(AUTH_FILE_NAME).exists());
        assert!(!isolated.path.join(PROMPT_FILE_NAME).exists());
        let mut remaining = Vec::new();
        open_auth
            .read_to_end(&mut remaining)
            .expect("read revoked descriptor");
        assert!(
            remaining.is_empty(),
            "open auth descriptor was not truncated"
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn managed_os_confinement_blocks_host_data_oracle_writes_and_other_ports() {
        use axum::{routing::get, Router};
        let root = tempfile::tempdir().unwrap();
        let candidate = root.path().join("candidate");
        let home = root.path().join("child-home");
        fs::create_dir(&candidate).unwrap();
        fs::create_dir(&home).unwrap();
        let secret = root.path().join("operator-credential-canary");
        let oracle = root.path().join("host-oracle");
        fs::write(&secret, "must-not-be-readable").unwrap();
        fs::write(&oracle, "host-authority").unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let other = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let other_port = other.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/", get(|| async { "local relay" })),
            )
            .await
            .unwrap();
        });
        let other_server = tokio::spawn(async move {
            axum::serve(
                other,
                Router::new().route("/", get(|| async { "outside relay" })),
            )
            .await
            .unwrap();
        });
        let policy = crate::managed_provider::ManagedChildPolicy {
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            port,
            model: "grok-build-0.1".into(),
            max_duration_ms: 3000,
            max_turns: 1,
            max_output_tokens: 1,
        };
        let host = GrokBuildHostLaunchConfig {
            executable: "/bin/sh".into(),
            git_executable: "/usr/bin/git".into(),
            cwd: candidate.clone(),
            repository_id: "test".into(),
            base_ref: "refs/heads/test".into(),
            prompt: "test".into(),
            allowed_files: vec!["source".into()],
            execution_approved: true,
            max_stdout_bytes: 1024,
            max_stderr_bytes: 1024,
            git_timeout: Duration::from_secs(2),
            isolate_parent: root.path().into(),
            defer_source_apply: true,
            candidate_retention_dir: Some(root.path().join("retained")),
        };
        let script = r#"cat "$1" >/dev/null 2>&1 && exit 11
(echo bad > "$2") 2>/dev/null && exit 12
/usr/bin/curl --silent --max-time 1 "$3" >/dev/null 2>&1 && exit 13
/usr/bin/curl --silent --max-time 1 "$4" || exit 14
printf 'candidate' > source
"#;
        let args = vec![
            "-c".into(),
            script.into(),
            "probe".into(),
            secret.display().to_string(),
            oracle.display().to_string(),
            format!("http://127.0.0.1:{other_port}/"),
            format!("http://127.0.0.1:{port}/"),
        ];
        let mut command = managed_sandbox_command(&host, &args, &policy, &home).unwrap();
        command
            .env_clear()
            .envs(allowlisted_env(&home).unwrap())
            .current_dir(&candidate)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = tokio::time::timeout(Duration::from_secs(5), command.output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "local relay");
        assert_eq!(fs::read_to_string(&oracle).unwrap(), "host-authority");
        assert_eq!(
            fs::read_to_string(candidate.join("source")).unwrap(),
            "candidate"
        );
        server.abort();
        other_server.abort();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unsupported_cli_version_is_denied_before_a_provider_capability_is_issued() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("unsupported-cli");
        fs::write(
            &executable,
            "#!/bin/sh\nprintf 'grok 9.0.0 unsupported\\n'\n",
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(managed_offline_readiness(&executable, root.path())
            .unwrap_err()
            .contains("requires Grok 1.0.41"));
    }

    #[test]
    fn managed_stdout_requires_matching_host_usage_model_and_turns() {
        let provider = crate::managed_provider::ManagedProviderEvidence {
            model: "grok-build-0.1".into(),
            wire_attempts: 1,
            input_tokens: 100,
            output_tokens: 10,
            ..Default::default()
        };
        let good = serde_json::json!({"usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"reasoning_tokens":0},"num_turns":1,"modelUsage":{"grok-build-0.1":{"inputTokens":100,"outputTokens":10,"modelCalls":1,"cacheCreationInputTokens":0,"cacheReadInputTokens":0}}});
        assert!(managed_stdout_usage_matches(&good, &provider));
        for pointer in [
            "/usage/input_tokens",
            "/usage/output_tokens",
            "/usage/total_tokens",
            "/num_turns",
            "/modelUsage/grok-build-0.1/modelCalls",
        ] {
            let mut bad = good.clone();
            *bad.pointer_mut(pointer).unwrap() = serde_json::json!(999);
            assert!(!managed_stdout_usage_matches(&bad, &provider));
        }
        let mut bad = good.clone();
        bad.as_object_mut().unwrap().remove("usage");
        assert!(!managed_stdout_usage_matches(&bad, &provider));
        let mut bad = good;
        bad["modelUsage"]["other-model"] = serde_json::json!({});
        assert!(!managed_stdout_usage_matches(&bad, &provider));
    }

    #[test]
    fn managed_capability_is_rejected_from_candidate_and_session_material() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("source"), "ordinary candidate").unwrap();
        assert!(!managed_material_leaked(
            root.path(),
            "private-local-capability"
        ));
        fs::write(
            root.path().join("updates.jsonl"),
            "private-local-capability",
        )
        .unwrap();
        assert!(managed_material_leaked(
            root.path(),
            "private-local-capability"
        ));
    }
}
