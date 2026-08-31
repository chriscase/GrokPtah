//! Bounded local process adapter for the advisory Grok Build contract.
//!
//! This module executes a validated [`GrokBuildLaunchRequest`] against an
//! allowlisted local CLI. It is not a manager, not host authority, not a
//! provider account, not live qualification, not merge authority, and not
//! Computer Use. It does not wire into orchestration or Work.
//!
//! Credential material is never accepted as a raw token. A host-injected
//! lease resolver may return only a path/lease handle; this adapter copies
//! that file blindly into a private `GROK_HOME` and never retains, logs, or
//! returns the contents.

use std::ffi::OsStr;
use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

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
const ISOLATED_CONFIG: &str = "[compat.claude]\nskills = false\nrules = false\nagents = false\nmcps = false\nhooks = false\nsessions = false\n\n[compat.cursor]\nskills = false\nrules = false\nagents = false\nmcps = false\nhooks = false\nsessions = false\n";
const INSPECT_OUTPUT_MAX: usize = 262_144;
const GIT_OUTPUT_MAX: usize = 32_768;
const LEASE_FILE_MAX: u64 = 1_048_576;
const HOME_ALIAS_MAX: usize = 64;
const AUTH_FILE_NAME: &str = "auth.json";
const PROMPT_FILE_NAME: &str = "prompt";
const CONFIG_FILE_NAME: &str = "config.toml";

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
    #[error("isolation_failed")]
    IsolationFailed,
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
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
    pub git_timeout: Duration,
    /// Parent directory for the task-scoped isolated `GROK_HOME`.
    pub isolate_parent: PathBuf,
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
            .field("max_stdout_bytes", &self.max_stdout_bytes)
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
        if self.max_stdout_bytes == 0 || self.max_stderr_bytes == 0 {
            return Err(GrokBuildAdapterError::InvalidRequest);
        }
        if self.git_timeout.is_zero() {
            return Err(GrokBuildAdapterError::InvalidRequest);
        }
        Ok(())
    }
}

/// Opaque lease handle. The contained location is never shown in Debug.
pub struct CredentialLeaseHandle {
    location: PathBuf,
}

impl CredentialLeaseHandle {
    /// Host-only constructor. The adapter copies but never interprets contents.
    pub fn from_host_path(path: PathBuf) -> Self {
        Self { location: path }
    }
}

impl fmt::Debug for CredentialLeaseHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialLeaseHandle")
            .field("present", &true)
            .finish()
    }
}

/// Injected credential lease resolution. Tests use fakes.
pub trait CredentialLeaseResolver: Send + Sync {
    fn resolve(&self, lease_id: &str) -> Result<CredentialLeaseHandle, GrokBuildAdapterError>;
}

/// Validated receipt/result pair. No raw stdout, stderr, paths, accounts,
/// models, or provider text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrokBuildAdapterOutcome {
    receipt: GrokBuildIsolationReceipt,
    result: GrokBuildResult,
}

impl GrokBuildAdapterOutcome {
    pub fn receipt(&self) -> &GrokBuildIsolationReceipt {
        &self.receipt
    }

    pub fn result(&self) -> &GrokBuildResult {
        &self.result
    }
}

/// Execute one bounded Grok Build CLI run. Never retries a spawned process.
pub async fn launch_grok_build(
    launch: &GrokBuildLaunchRequest,
    host: &GrokBuildHostLaunchConfig,
    credentials: &dyn CredentialLeaseResolver,
    cancel: CancellationToken,
) -> Result<GrokBuildAdapterOutcome, GrokBuildAdapterError> {
    launch.validate().map_err(map_contract)?;
    host.validate()?;
    if host.repository_id != launch.identity.repository_id {
        return Err(GrokBuildAdapterError::IdentityMismatch);
    }
    if (host.prompt.len() as u64) > launch.max_prompt_bytes {
        return Err(GrokBuildAdapterError::InvalidRequest);
    }

    gate_git_identity(launch, host, cancel.clone()).await?;
    if cancel.is_cancelled() {
        return Err(GrokBuildAdapterError::Cancelled);
    }

    let lease = credentials.resolve(&launch.credential_lease_id)?;
    let isolated = IsolatedHome::create(&host.isolate_parent)?;
    isolated.write_minimal_config()?;
    isolated.install_prompt(&host.prompt)?;
    isolated.install_lease(&lease)?;

    verify_isolation(host, &isolated, cancel.clone()).await?;
    // Close the inspect-to-launch window against an externally changed ref or
    // newly introduced project-local compatibility surface.
    gate_git_identity(launch, host, cancel.clone()).await?;

    if cancel.is_cancelled() {
        return Err(GrokBuildAdapterError::Cancelled);
    }

    let session_id = Uuid::new_v4().to_string();
    execute_allowlisted(launch, host, &isolated, &session_id, cancel).await
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
        GrokBuildMutationMode::IsolatedReview => "acceptEdits",
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
    Ok(vec![
        "--prompt-file".to_string(),
        prompt_file.to_string(),
        "--permission-mode".to_string(),
        permission_flag(mode).to_string(),
        "--disable-web-search".to_string(),
        "--no-subagents".to_string(),
        "--max-turns".to_string(),
        max_turns.to_string(),
        "--session-id".to_string(),
        session_id.to_string(),
        "--output-format".to_string(),
        "plain".to_string(),
    ])
}

fn allowlisted_env(grok_home: &Path) -> Result<Vec<(String, String)>, GrokBuildAdapterError> {
    let home = grok_home
        .to_str()
        .ok_or(GrokBuildAdapterError::IsolationFailed)?;
    Ok(vec![
        ("GROK_HOME".to_string(), home.to_string()),
        ("HOME".to_string(), home.to_string()),
        ("PATH".to_string(), BOUNDED_PATH.to_string()),
    ])
}

struct IsolatedHome {
    path: PathBuf,
}

impl IsolatedHome {
    fn create(parent: &Path) -> Result<Self, GrokBuildAdapterError> {
        let path = parent.join(format!("gb-{}", Uuid::new_v4().simple()));
        std::fs::create_dir(&path).map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
        set_private_dir_permissions(&path)?;
        Ok(Self { path })
    }

    fn write_minimal_config(&self) -> Result<(), GrokBuildAdapterError> {
        write_private_file(
            &self.path.join(CONFIG_FILE_NAME),
            ISOLATED_CONFIG.as_bytes(),
        )
    }

    fn install_prompt(&self, prompt: &str) -> Result<(), GrokBuildAdapterError> {
        write_private_file(&self.path.join(PROMPT_FILE_NAME), prompt.as_bytes())
    }

    fn prompt_path(&self) -> PathBuf {
        self.path.join(PROMPT_FILE_NAME)
    }

    fn install_lease(&self, lease: &CredentialLeaseHandle) -> Result<(), GrokBuildAdapterError> {
        require_absolute_file_path(&lease.location)
            .map_err(|_| GrokBuildAdapterError::CredentialLease)?;
        let mut source = open_credential_source(&lease.location)?;
        let meta = source
            .metadata()
            .map_err(|_| GrokBuildAdapterError::CredentialLease)?;
        validate_credential_metadata(&meta)?;
        let dest = self.path.join(AUTH_FILE_NAME);
        let mut target =
            create_private_file(&dest).map_err(|_| GrokBuildAdapterError::CredentialLease)?;
        io::copy(&mut source, &mut target).map_err(|_| GrokBuildAdapterError::CredentialLease)?;
        target
            .sync_all()
            .map_err(|_| GrokBuildAdapterError::CredentialLease)?;
        Ok(())
    }

    fn cleanup(&self) -> bool {
        std::fs::remove_dir_all(&self.path).is_ok() && !self.path.exists()
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
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
        options.mode(0o600).custom_flags(libc::O_CLOEXEC);
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
    cancel: CancellationToken,
) -> Result<(), GrokBuildAdapterError> {
    let env = allowlisted_env(&isolated.path)?;
    let mut cmd = Command::new(&host.executable);
    cmd.args(["inspect", "--json"]);
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
    if harvest.kind != HarvestKind::Exited(0) {
        return Err(GrokBuildAdapterError::IsolationFailed);
    }
    let value: serde_json::Value = serde_json::from_slice(&harvest.stdout)
        .map_err(|_| GrokBuildAdapterError::IsolationFailed)?;
    for field in [
        "projectInstructions",
        "hooks",
        "plugins",
        "mcpServers",
        "lspServers",
    ] {
        let empty = value
            .get(field)
            .and_then(serde_json::Value::as_array)
            .is_some_and(Vec::is_empty);
        if !empty {
            return Err(GrokBuildAdapterError::IsolationFailed);
        }
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
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        cancel,
    )
    .await?;
    let dirty = porcelain_paths(&status)?;
    if !dirty.is_empty() {
        return Err(GrokBuildAdapterError::DirtyTree);
    }
    Ok(())
}

async fn execute_allowlisted(
    launch: &GrokBuildLaunchRequest,
    host: &GrokBuildHostLaunchConfig,
    isolated: &IsolatedHome,
    session_id: &str,
    cancel: CancellationToken,
) -> Result<GrokBuildAdapterOutcome, GrokBuildAdapterError> {
    let args = allowlisted_args(
        &isolated.prompt_path(),
        launch.mutation_mode,
        launch.max_turns,
        session_id,
    )?;
    let env = allowlisted_env(&isolated.path)?;
    let mut cmd = Command::new(&host.executable);
    cmd.args(&args);
    cmd.current_dir(&host.cwd);
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
        host.max_stdout_bytes,
        host.max_stderr_bytes,
        Duration::from_millis(launch.max_duration_ms),
        cancel,
    )
    .await;

    let readonly_violation = if launch.mutation_mode == GrokBuildMutationMode::ReadOnly {
        read_only_tree_mutated(host).await.unwrap_or(true)
    } else {
        false
    };

    let classified = classify_harvest(&harvest, readonly_violation);
    let cleaned = isolated.cleanup();
    finish_outcome(launch, session_id, classified, !readonly_violation, cleaned)
}

fn finish_outcome(
    launch: &GrokBuildLaunchRequest,
    session_id: &str,
    classified: ClassifiedRun,
    permissions_ok: bool,
    cleaned: bool,
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
        GrokBuildRunState::CompleteAdvisory => vec!["advisory-summary".to_string()],
        GrokBuildRunState::NeedsSynthesis => vec!["partial-run".to_string()],
        GrokBuildRunState::FailedClosed | GrokBuildRunState::Running => {
            vec!["closed-run".to_string()]
        }
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
    Ok(GrokBuildAdapterOutcome { receipt, result })
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
}

struct ClassifiedRun {
    state: GrokBuildRunState,
    verdict: Option<GrokBuildVerdict>,
}

fn classify_harvest(harvest: &Harvest, readonly_violation: bool) -> ClassifiedRun {
    if readonly_violation {
        return ClassifiedRun {
            state: GrokBuildRunState::FailedClosed,
            verdict: None,
        };
    }
    match harvest.kind {
        HarvestKind::Overflow | HarvestKind::Timeout | HarvestKind::Cancelled => ClassifiedRun {
            state: GrokBuildRunState::FailedClosed,
            verdict: None,
        },
        HarvestKind::Exited(code) => {
            if has_max_turns(&harvest.stdout, &harvest.stderr) {
                return ClassifiedRun {
                    state: GrokBuildRunState::NeedsSynthesis,
                    verdict: None,
                };
            }
            if code != 0 {
                return ClassifiedRun {
                    state: GrokBuildRunState::FailedClosed,
                    verdict: None,
                };
            }
            match explicit_verdict(&harvest.stdout, &harvest.stderr) {
                Some(verdict) => ClassifiedRun {
                    state: GrokBuildRunState::CompleteAdvisory,
                    verdict: Some(verdict),
                },
                None => ClassifiedRun {
                    state: GrokBuildRunState::NeedsSynthesis,
                    verdict: None,
                },
            }
        }
    }
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
            crate::process_tree::terminate(child).await;
            return Harvest {
                kind: HarvestKind::Cancelled,
                stdout: Vec::new(),
                stderr: Vec::new(),
            };
        }
    };
    let mut stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            crate::process_tree::terminate(child).await;
            return Harvest {
                kind: HarvestKind::Cancelled,
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
    let deadline = Instant::now() + limit;

    let kind = loop {
        if status.is_some() && stdout_done && stderr_done {
            break HarvestKind::Exited(status.unwrap_or(1));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            crate::process_tree::terminate(child).await;
            break HarvestKind::Timeout;
        }
        tokio::select! {
            _ = cancel.cancelled() => {
                crate::process_tree::terminate(child).await;
                break HarvestKind::Cancelled;
            }
            _ = tokio::time::sleep(remaining) => {
                crate::process_tree::terminate(child).await;
                break HarvestKind::Timeout;
            }
            n = stdout.read(&mut out_tmp), if !stdout_done => {
                match n {
                    Ok(0) => stdout_done = true,
                    Ok(n) if out.len().saturating_add(n) > max_stdout => {
                        crate::process_tree::terminate(child).await;
                        out.clear();
                        err.clear();
                        break HarvestKind::Overflow;
                    }
                    Ok(n) => out.extend_from_slice(&out_tmp[..n]),
                    Err(_) => stdout_done = true,
                }
            }
            n = stderr.read(&mut err_tmp), if !stderr_done => {
                match n {
                    Ok(0) => stderr_done = true,
                    Ok(n) if err.len().saturating_add(n) > max_stderr => {
                        crate::process_tree::terminate(child).await;
                        out.clear();
                        err.clear();
                        break HarvestKind::Overflow;
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
        let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
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

async fn read_only_tree_mutated(
    host: &GrokBuildHostLaunchConfig,
) -> Result<bool, GrokBuildAdapterError> {
    let status = git_stdout(
        host,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        CancellationToken::new(),
    )
    .await?;
    let dirty = porcelain_paths(&status)?;
    Ok(!dirty.is_empty())
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
                "acceptEdits",
                "--disable-web-search",
                "--no-subagents",
                "--max-turns",
                "8",
                "--session-id",
                "00000000-0000-4000-8000-000000000001",
                "--output-format",
                "plain",
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
        assert!(!readonly.iter().any(|a| a == "acceptEdits"));
    }

    #[test]
    fn allowlisted_env_is_exact() {
        let env = allowlisted_env(Path::new("/isolated/home")).expect("env");
        assert_eq!(
            env,
            [
                ("GROK_HOME".to_string(), "/isolated/home".to_string()),
                ("HOME".to_string(), "/isolated/home".to_string()),
                ("PATH".to_string(), BOUNDED_PATH.to_string()),
            ]
        );
        assert!(!env.iter().any(|(k, _)| k.contains("KEY")
            || k.contains("TOKEN")
            || k.contains("SECRET")
            || k.contains("XAI")
            || k.contains("CLAUDE")));
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
            max_stdout_bytes: 32,
            max_stderr_bytes: 32,
            git_timeout: Duration::from_secs(1),
            isolate_parent: PathBuf::from("/tmp/iso"),
        };
        let rendered = format!("{host:?}");
        assert!(!rendered.contains("sk-"));
        assert!(!rendered.contains("private key"));
        assert!(!rendered.contains("/usr/bin/grok"));
        assert!(!rendered.contains("/tmp/repo"));
    }

    #[test]
    fn max_turns_never_classifies_complete() {
        let harvest = Harvest {
            kind: HarvestKind::Exited(0),
            stdout: b"GROK_BUILD_VERDICT=clean\nmax_turns_reached\n".to_vec(),
            stderr: Vec::new(),
        };
        let classified = classify_harvest(&harvest, false);
        assert_eq!(classified.state, GrokBuildRunState::NeedsSynthesis);
        assert_eq!(classified.verdict, None);
    }

    #[test]
    fn missing_verdict_is_needs_synthesis() {
        let harvest = Harvest {
            kind: HarvestKind::Exited(0),
            stdout: b"review complete without marker\n".to_vec(),
            stderr: Vec::new(),
        };
        let classified = classify_harvest(&harvest, false);
        assert_eq!(classified.state, GrokBuildRunState::NeedsSynthesis);
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
    }
}
