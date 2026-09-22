//! Host-controlled readiness and candidate verification for one bounded
//! managed Grok Build change.
//!
//! Readiness only inspects the installed CLI. Required checks run as explicit
//! argv against the retained candidate, never as a frontend shell, and never
//! with the operator environment.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const CANDIDATE_VERIFICATION_SCHEMA: u32 = 1;
const MAX_CHECKS: usize = 8;
const MAX_ARGS: usize = 16;
const MAX_ARG_BYTES: usize = 512;
const MAX_ENV: usize = 8;
const MAX_FILES: usize = 256;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_TREE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_DIFF_BYTES: usize = 8 * 1024;
const INSPECT_TIMEOUT: Duration = Duration::from_secs(3);

const INSPECT_KEYS: &[&str] = &[
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
];

const SKIP_DIRS: &[&str] = &[".git", ".grok", "target", "node_modules", "dist", "build"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedChangeError {
    pub message: String,
}

impl VerifiedChangeError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for VerifiedChangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequiredCheckCwd {
    Candidate,
    Oracle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequiredCheckEnv {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequiredCheckSpec {
    pub check_id: String,
    pub executable: String,
    pub args: Vec<String>,
    pub cwd: RequiredCheckCwd,
    pub env: Vec<RequiredCheckEnv>,
    pub timeout_ms: u64,
    pub max_output_bytes: u32,
}

impl RequiredCheckSpec {
    pub fn spec_digest(checks: &[Self]) -> Result<String, VerifiedChangeError> {
        let value = serde_json::to_value(checks)
            .map_err(|_| VerifiedChangeError::new("required checks could not be sealed"))?;
        Ok(format!(
            "sha256:{}",
            hex_bytes(&Sha256::digest(canonical(&value).as_bytes()))
        ))
    }
}

pub fn validate_required_checks(checks: &[RequiredCheckSpec]) -> Result<(), VerifiedChangeError> {
    if checks.len() > MAX_CHECKS {
        return Err(VerifiedChangeError::new(
            "required checks exceed the host bound",
        ));
    }
    let mut seen = BTreeMap::<String, ()>::new();
    for check in checks {
        if !valid_check_id(&check.check_id) || seen.insert(check.check_id.clone(), ()).is_some() {
            return Err(VerifiedChangeError::new(
                "required check id must be a unique lowercase token",
            ));
        }
        let executable = Path::new(&check.executable);
        if !executable.is_absolute() || check.executable.contains('\0') {
            return Err(VerifiedChangeError::new(
                "required check executable must be an absolute path",
            ));
        }
        if check.args.len() > MAX_ARGS {
            return Err(VerifiedChangeError::new(
                "required check has too many arguments",
            ));
        }
        for arg in &check.args {
            if arg.len() > MAX_ARG_BYTES || arg.contains('\0') {
                return Err(VerifiedChangeError::new(
                    "required check argument is empty, too long, or contains NUL",
                ));
            }
        }
        if check.env.len() > MAX_ENV {
            return Err(VerifiedChangeError::new(
                "required check environment is too large",
            ));
        }
        for entry in &check.env {
            if !valid_env_key(&entry.key)
                || secret_env_key(&entry.key)
                || entry.value.contains('\0')
            {
                return Err(VerifiedChangeError::new(
                    "required check environment refuses operator credentials, PATH, and HOME",
                ));
            }
        }
        if check.timeout_ms == 0 || check.timeout_ms > 60_000 {
            return Err(VerifiedChangeError::new(
                "required check timeout must be between 1 and 60000 milliseconds",
            ));
        }
        if check.max_output_bytes == 0 || check.max_output_bytes > 16 * 1024 {
            return Err(VerifiedChangeError::new(
                "required check output bound must be between 1 and 16384 bytes",
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolchainFact {
    pub check_id: String,
    pub present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedChangeReadiness {
    pub ready: bool,
    pub reasons: Vec<String>,
    pub cli_version: Option<String>,
    pub cli_contract: Option<String>,
    pub platform: String,
    pub mutation_mode: String,
    pub toolchain: Vec<ToolchainFact>,
    pub workers_dispatched: u32,
    pub provider_invocations: u32,
}

pub struct ReadinessInput<'a> {
    pub executable: &'a Path,
    pub mutation_mode: &'a str,
    pub platform: &'a str,
    pub checks: &'a [RequiredCheckSpec],
    pub oracle_root: &'a Path,
    pub workspace: &'a Path,
}

/// Inspect the installed CLI and declared validators. This never sends a
/// prompt and never records a dispatched worker.
pub fn inspect_assignment_readiness(input: &ReadinessInput<'_>) -> VerifiedChangeReadiness {
    let mut reasons = Vec::new();
    let mut cli_version = None;
    let mut cli_contract = None;
    if input.mutation_mode == "read_only" {
        reasons.push(
            "Read-only mutation mode is refused until the host can observe sandbox enforcement."
                .into(),
        );
    } else if input.mutation_mode != "isolated_review" {
        reasons.push("The selected execution mode is not supported for a managed change.".into());
    }
    if input.platform != "macos" {
        reasons.push(
            "Non-macOS mutation is refused. This host does not enable managed mutation off macOS."
                .into(),
        );
    }
    if input.checks.is_empty() {
        reasons.push("Declare at least one host-controlled required check before dispatch.".into());
    }
    if let Err(error) = validate_required_checks(input.checks) {
        reasons.push(error.message);
    }
    if oracle_inside_workspace(input.oracle_root, input.workspace) {
        reasons.push(
            "The required-check oracle is inside the worker workspace. Move it outside the writable checkout."
                .into(),
        );
    } else if !input.oracle_root.is_dir() {
        reasons.push("The required-check oracle directory is missing.".into());
    }
    let mut toolchain = Vec::new();
    for check in input.checks {
        let present = Path::new(&check.executable).is_file();
        if !present {
            reasons.push(format!(
                "Required check '{}' executable is not installed.",
                check.check_id
            ));
        }
        toolchain.push(ToolchainFact {
            check_id: check.check_id.clone(),
            present,
        });
    }
    if !input.executable.is_file() {
        reasons.push(
            "The managed Grok CLI executable is missing. Install it or choose the configured host binary before dispatch."
                .into(),
        );
    } else {
        match inspect_cli(input.executable) {
            Ok(version) => {
                cli_version = Some(version);
                cli_contract = Some("inspect-json-v1".into());
            }
            Err(error) => reasons.push(error.message),
        }
    }
    reasons.sort();
    reasons.dedup();
    VerifiedChangeReadiness {
        ready: reasons.is_empty(),
        reasons,
        cli_version,
        cli_contract,
        platform: input.platform.to_string(),
        mutation_mode: input.mutation_mode.to_string(),
        toolchain,
        workers_dispatched: 0,
        provider_invocations: 0,
    }
}

fn inspect_cli(executable: &Path) -> Result<String, VerifiedChangeError> {
    let mut command = Command::new(executable);
    command
        .arg("inspect")
        .arg("--json")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|_| VerifiedChangeError::new("The managed Grok CLI could not be inspected."))?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= INSPECT_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(VerifiedChangeError::new(
                    "The managed Grok CLI inspection timed out before it reported a contract.",
                ));
            }
            Ok(None) => thread::sleep(Duration::from_millis(15)),
            Err(_) => {
                return Err(VerifiedChangeError::new(
                    "The managed Grok CLI inspection ended before a contract was read.",
                ));
            }
        }
    };
    let mut stdout = Vec::new();
    if let Some(mut pipe) = child.stdout {
        let _ = pipe.read_to_end(&mut stdout);
    }
    if !status.success() {
        return Err(VerifiedChangeError::new(
            "The managed Grok CLI inspection failed. Dispatch stays closed.",
        ));
    }
    let value: Value = serde_json::from_slice(&stdout).map_err(|_| {
        VerifiedChangeError::new(
            "The installed CLI inspect output is not the supported JSON contract.",
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        VerifiedChangeError::new("The installed CLI inspect output is not a JSON object.")
    })?;
    if object.len() != INSPECT_KEYS.len()
        || object
            .keys()
            .any(|key| !INSPECT_KEYS.iter().any(|expected| expected == key))
    {
        return Err(VerifiedChangeError::new(
            "The installed CLI inspect contract has unsupported keys. Dispatch stays closed.",
        ));
    }
    let version = object
        .get("grokVersion")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 64 && !value.contains('\0'))
        .ok_or_else(|| {
            VerifiedChangeError::new("The installed CLI did not report a grokVersion.")
        })?;
    if !object.get("permissions").is_some_and(Value::is_object) {
        return Err(VerifiedChangeError::new(
            "The installed CLI inspect contract omitted permissions.",
        ));
    }
    Ok(version.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateFileRecord {
    pub path: String,
    pub digest: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateContentIdentity {
    pub source_revision: String,
    pub content_digest: String,
    pub files: Vec<CandidateFileRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequiredCheckExecution {
    pub check_id: String,
    pub outcome: String,
    pub exit_code: Option<i32>,
    pub output_digest: String,
    pub output_truncated: bool,
    pub duration_ms: u64,
    pub spec_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateVerification {
    pub schema_version: u32,
    pub work_id: String,
    pub run_id: Option<String>,
    pub attempt_id: Option<String>,
    pub source_revision: String,
    pub content_digest: String,
    pub spec_digest: String,
    pub files: Vec<CandidateFileRecord>,
    pub checks: Vec<RequiredCheckExecution>,
    pub worker_stopped: bool,
    pub change_proposed: bool,
    pub checks_passed: bool,
    pub invalidated: bool,
    pub applied: bool,
    pub diff_digest: Option<String>,
    pub changed_paths: Vec<String>,
    pub bounded_diff: String,
    pub diff_truncated: bool,
}

impl CandidateVerification {
    pub fn authorizes_applied_success(
        &self,
        approved_digest: Option<&str>,
        target_revision: Option<&str>,
    ) -> bool {
        self.schema_version == CANDIDATE_VERIFICATION_SCHEMA
            && self.applied
            && !self.invalidated
            && self.checks_passed
            && self.worker_stopped
            && self.change_proposed
            && approved_digest == Some(self.content_digest.as_str())
            && target_revision == Some(self.source_revision.as_str())
            && self.checks.iter().all(|check| check.outcome == "passed")
            && !self.checks.is_empty()
    }
}

pub fn source_revision(git: &Path, repo: &Path) -> Result<String, VerifiedChangeError> {
    let output = git_output(git, repo, &["rev-parse", "HEAD"])?;
    let revision = String::from_utf8(output)
        .map_err(|_| VerifiedChangeError::new("git HEAD is not valid UTF-8"))?
        .trim()
        .to_string();
    if revision.len() != 40 || !revision.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(VerifiedChangeError::new(
            "the source revision is not a full git SHA",
        ));
    }
    Ok(revision)
}

pub fn worktree_is_clean(git: &Path, repo: &Path) -> Result<bool, VerifiedChangeError> {
    let output = git_output(
        git,
        repo,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    Ok(output.iter().all(u8::is_ascii_whitespace) || output.is_empty())
}

pub fn candidate_content_identity(
    root: &Path,
    source_revision: &str,
) -> Result<CandidateContentIdentity, VerifiedChangeError> {
    let root = canonical_dir(root)?;
    let mut files = Vec::new();
    let mut total = 0u64;
    walk_identity(&root, &root, &mut files, &mut total)?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    if files.len() > MAX_FILES {
        return Err(VerifiedChangeError::new(
            "the candidate has more source files than the host identity bound",
        ));
    }
    let mut hasher = Sha256::new();
    hasher.update(b"grokptah-candidate-source-v1\0");
    hasher.update(source_revision.as_bytes());
    hasher.update(b"\0");
    for file in &files {
        hasher.update(file.path.as_bytes());
        hasher.update(b"\0");
        hasher.update(file.kind.as_bytes());
        hasher.update(b"\0");
        hasher.update(file.digest.as_bytes());
        hasher.update(b"\n");
    }
    Ok(CandidateContentIdentity {
        source_revision: source_revision.to_string(),
        content_digest: format!("sha256:{}", hex_bytes(&hasher.finalize())),
        files,
    })
}

pub fn retain_candidate_snapshot(source: &Path, dest: &Path) -> Result<(), VerifiedChangeError> {
    let source = canonical_dir(source)?;
    if dest.exists() {
        fs::remove_dir_all(dest).map_err(|_| {
            VerifiedChangeError::new("an older candidate snapshot could not be replaced")
        })?;
    }
    fs::create_dir_all(dest).map_err(|_| {
        VerifiedChangeError::new("the candidate snapshot directory could not be created")
    })?;
    let copied = copy_tree(&source, &source, dest);
    if copied.is_err() {
        let _ = fs::remove_dir_all(dest);
    }
    copied
}

pub fn execute_required_checks(
    checks: &[RequiredCheckSpec],
    candidate_root: &Path,
    oracle_root: &Path,
) -> Result<Vec<RequiredCheckExecution>, VerifiedChangeError> {
    validate_required_checks(checks)?;
    if oracle_inside_workspace(oracle_root, candidate_root) {
        return Err(VerifiedChangeError::new(
            "the required-check oracle is inside the candidate tree",
        ));
    }
    let spec_digest = RequiredCheckSpec::spec_digest(checks)?;
    let mut results = Vec::with_capacity(checks.len());
    for check in checks {
        results.push(run_one_check(
            check,
            candidate_root,
            oracle_root,
            &spec_digest,
        ));
    }
    Ok(results)
}

pub fn checks_passed(checks: &[RequiredCheckSpec], results: &[RequiredCheckExecution]) -> bool {
    !checks.is_empty()
        && checks.len() == results.len()
        && checks.iter().zip(results.iter()).all(|(spec, result)| {
            result.check_id == spec.check_id
                && result.outcome == "passed"
                && !result.output_truncated
                && result.exit_code == Some(0)
        })
}

pub fn bounded_source_diff(
    before: &Path,
    after: &Path,
    allowed: &[String],
) -> Result<(String, bool, Vec<String>), VerifiedChangeError> {
    let mut changed = Vec::new();
    let mut body = String::new();
    for path in allowed {
        let left = fs::read(before.join(path)).unwrap_or_default();
        let right = fs::read(after.join(path)).unwrap_or_default();
        if left != right {
            changed.push(path.clone());
            body.push_str(&format!("--- a/{path}\n+++ b/{path}\n"));
            push_text_diff(&mut body, &left, &right);
        }
    }
    let truncated = body.len() > MAX_DIFF_BYTES;
    if truncated {
        let mut end = MAX_DIFF_BYTES;
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        body.truncate(end);
        body.push_str("\n...[diff truncated]\n");
    }
    Ok((body, truncated, changed))
}

pub fn apply_candidate_tree(
    git: &Path,
    source: &Path,
    retained: &Path,
    expected: &CandidateContentIdentity,
    allowed: &[String],
) -> Result<(), VerifiedChangeError> {
    let observed = source_revision(git, source)?;
    if observed != expected.source_revision {
        return Err(VerifiedChangeError::new(
            "the target revision changed after verification",
        ));
    }
    if !worktree_is_clean(git, source)? {
        return Err(VerifiedChangeError::new(
            "the target workspace is not clean at the verified revision",
        ));
    }
    let retained_identity = candidate_content_identity(retained, &expected.source_revision)?;
    if retained_identity.content_digest != expected.content_digest
        || retained_identity.files != expected.files
    {
        return Err(VerifiedChangeError::new(
            "the retained candidate no longer matches the verified content identity",
        ));
    }
    let source_identity = candidate_content_identity(source, &expected.source_revision)?;
    let mut differing = Vec::new();
    let source_map: BTreeMap<_, _> = source_identity
        .files
        .iter()
        .map(|file| (file.path.clone(), file.clone()))
        .collect();
    let retained_map: BTreeMap<_, _> = retained_identity
        .files
        .iter()
        .map(|file| (file.path.clone(), file.clone()))
        .collect();
    for path in source_map.keys().chain(retained_map.keys()) {
        if source_map.get(path) != retained_map.get(path) && !differing.contains(path) {
            differing.push(path.clone());
        }
    }
    if differing
        .iter()
        .any(|path| !allowed.iter().any(|allowed| allowed == path))
    {
        return Err(VerifiedChangeError::new(
            "the candidate changes a path outside the allowed file scope",
        ));
    }
    for path in &differing {
        let from = retained.join(path);
        let to = source.join(path);
        if !retained_map.contains_key(path) {
            if to.exists() {
                fs::remove_file(&to).map_err(|_| {
                    VerifiedChangeError::new("an allowed candidate deletion could not be applied")
                })?;
            }
            continue;
        }
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(|_| {
                VerifiedChangeError::new("the candidate destination directory could not be created")
            })?;
        }
        fs::copy(&from, &to)
            .map_err(|_| VerifiedChangeError::new("the candidate file could not be applied"))?;
    }
    let applied = candidate_content_identity(source, &expected.source_revision)?;
    if applied.content_digest != expected.content_digest {
        restore_clean_worktree(git, source, &expected.source_revision)?;
        return Err(VerifiedChangeError::new(
            "applying the candidate did not reproduce its verified content identity",
        ));
    }
    Ok(())
}

pub fn restore_clean_worktree(
    git: &Path,
    source: &Path,
    revision: &str,
) -> Result<(), VerifiedChangeError> {
    git_status(git, source, &["reset", "--hard", revision])?;
    git_status(git, source, &["clean", "-ffdx", "--"])?;
    Ok(())
}

fn run_one_check(
    check: &RequiredCheckSpec,
    candidate_root: &Path,
    oracle_root: &Path,
    spec_digest: &str,
) -> RequiredCheckExecution {
    let started = Instant::now();
    let base = |outcome: &str, exit_code: Option<i32>, truncated: bool| RequiredCheckExecution {
        check_id: check.check_id.clone(),
        outcome: outcome.into(),
        exit_code,
        output_digest: digest_bytes(b""),
        output_truncated: truncated,
        duration_ms: started.elapsed().as_millis() as u64,
        spec_digest: spec_digest.into(),
    };
    if !Path::new(&check.executable).is_file() {
        return base("missing", None, false);
    }
    let cwd = match check.cwd {
        RequiredCheckCwd::Candidate => candidate_root,
        RequiredCheckCwd::Oracle => oracle_root,
    };
    let candidate = match canonical_dir(candidate_root) {
        Ok(path) => path,
        Err(_) => return base("incomplete", None, false),
    };
    let oracle = match canonical_dir(oracle_root) {
        Ok(path) => path,
        Err(_) => return base("incomplete", None, false),
    };
    let mut command = Command::new(&check.executable);
    command
        .args(&check.args)
        .current_dir(cwd)
        .env_clear()
        .env("CANDIDATE_ROOT", candidate.to_string_lossy().as_ref())
        .env("ORACLE_ROOT", oracle.to_string_lossy().as_ref())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for entry in &check.env {
        command.env(&entry.key, &entry.value);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return base("missing", None, false),
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let cap = check.max_output_bytes as usize;
    let stdout_thread = thread::spawn(move || read_capped(stdout, cap));
    let stderr_thread = thread::spawn(move || read_capped(stderr, cap));
    let timeout = Duration::from_millis(check.timeout_ms);
    let waited = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => thread::sleep(Duration::from_millis(15)),
            Err(_) => break None,
        }
    };
    let stdout = stdout_thread.join().unwrap_or(CappedRead {
        bytes: Vec::new(),
        truncated: true,
    });
    let stderr = stderr_thread.join().unwrap_or(CappedRead {
        bytes: Vec::new(),
        truncated: true,
    });
    let truncated = stdout.truncated || stderr.truncated;
    let mut output = stdout.bytes;
    output.extend(stderr.bytes);
    let digest = digest_bytes(&output);
    let (outcome, exit_code) = match waited {
        None => ("timed_out", None),
        Some(status) if truncated => ("truncated", status.code()),
        Some(status) if status.success() => ("passed", status.code()),
        Some(status) => ("failed", status.code()),
    };
    RequiredCheckExecution {
        check_id: check.check_id.clone(),
        outcome: outcome.into(),
        exit_code,
        output_digest: digest,
        output_truncated: truncated,
        duration_ms: started.elapsed().as_millis() as u64,
        spec_digest: spec_digest.into(),
    }
}

struct CappedRead {
    bytes: Vec<u8>,
    truncated: bool,
}

fn read_capped(pipe: Option<impl Read>, cap: usize) -> CappedRead {
    let mut bytes = Vec::new();
    let mut truncated = false;
    let Some(mut pipe) = pipe else {
        return CappedRead { bytes, truncated };
    };
    let mut buf = [0u8; 1024];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if bytes.len() < cap {
                    let room = cap - bytes.len();
                    let take = n.min(room);
                    bytes.extend_from_slice(&buf[..take]);
                    if take < n {
                        truncated = true;
                    }
                } else {
                    truncated = true;
                }
            }
            Err(_) => {
                truncated = true;
                break;
            }
        }
    }
    CappedRead { bytes, truncated }
}

fn walk_identity(
    root: &Path,
    dir: &Path,
    files: &mut Vec<CandidateFileRecord>,
    total: &mut u64,
) -> Result<(), VerifiedChangeError> {
    let entries = fs::read_dir(dir)
        .map_err(|_| VerifiedChangeError::new("a candidate directory could not be read"))?;
    for entry in entries {
        let entry = entry.map_err(|_| {
            VerifiedChangeError::new("a candidate directory entry could not be read")
        })?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if dir == root && SKIP_DIRS.contains(&name.as_ref()) {
            continue;
        }
        let path = entry.path();
        let relative = relative_path(root, &path)?;
        let meta = fs::symlink_metadata(&path)
            .map_err(|_| VerifiedChangeError::new("candidate metadata could not be read"))?;
        if meta.file_type().is_symlink() {
            reject_symlink_escape(root, &path)?;
            let target = fs::read_link(&path)
                .map_err(|_| VerifiedChangeError::new("a candidate symlink could not be read"))?;
            files.push(CandidateFileRecord {
                path: relative,
                digest: digest_bytes(target.to_string_lossy().as_bytes()),
                kind: "symlink".into(),
            });
            continue;
        }
        if meta.is_dir() {
            walk_identity(root, &path, files, total)?;
            continue;
        }
        if !meta.is_file() {
            return Err(VerifiedChangeError::new(
                "the candidate contains a non-regular source file",
            ));
        }
        if meta.len() > MAX_FILE_BYTES {
            return Err(VerifiedChangeError::new(
                "a candidate source file exceeds the identity bound",
            ));
        }
        *total = total.checked_add(meta.len()).ok_or_else(|| {
            VerifiedChangeError::new("the candidate tree exceeds the identity bound")
        })?;
        if *total > MAX_TREE_BYTES {
            return Err(VerifiedChangeError::new(
                "the candidate tree exceeds the identity bound",
            ));
        }
        let bytes = fs::read(&path)
            .map_err(|_| VerifiedChangeError::new("a candidate source file could not be read"))?;
        files.push(CandidateFileRecord {
            path: relative,
            digest: digest_bytes(&bytes),
            kind: "file".into(),
        });
    }
    Ok(())
}

fn copy_tree(root: &Path, dir: &Path, dest_root: &Path) -> Result<(), VerifiedChangeError> {
    let entries = fs::read_dir(dir)
        .map_err(|_| VerifiedChangeError::new("the candidate checkout could not be read"))?;
    for entry in entries {
        let entry =
            entry.map_err(|_| VerifiedChangeError::new("a candidate entry could not be read"))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if dir == root && SKIP_DIRS.contains(&name.as_ref()) {
            continue;
        }
        let path = entry.path();
        let relative = relative_path(root, &path)?;
        let dest = dest_root.join(&relative);
        let meta = fs::symlink_metadata(&path)
            .map_err(|_| VerifiedChangeError::new("candidate metadata could not be read"))?;
        if meta.file_type().is_symlink() {
            return Err(VerifiedChangeError::new(
                "a candidate symlink is not part of the retained source snapshot",
            ));
        }
        if meta.is_dir() {
            fs::create_dir_all(&dest).map_err(|_| {
                VerifiedChangeError::new("the candidate snapshot directory could not be created")
            })?;
            copy_tree(root, &path, dest_root)?;
            continue;
        }
        if !meta.is_file() {
            return Err(VerifiedChangeError::new(
                "the candidate contains a non-regular source file",
            ));
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|_| {
                VerifiedChangeError::new("the candidate snapshot directory could not be created")
            })?;
        }
        fs::copy(&path, &dest)
            .map_err(|_| VerifiedChangeError::new("a candidate file could not be retained"))?;
    }
    Ok(())
}

fn reject_symlink_escape(root: &Path, link: &Path) -> Result<(), VerifiedChangeError> {
    let target = fs::read_link(link)
        .map_err(|_| VerifiedChangeError::new("a candidate symlink could not be read"))?;
    let combined = if target.is_absolute() {
        target
    } else {
        link.parent().unwrap_or(root).join(target)
    };
    let root = canonical_dir(root)?;
    match dunce::canonicalize(&combined) {
        Ok(canon) if canon.starts_with(&root) => Ok(()),
        _ => Err(VerifiedChangeError::new(
            "a candidate symlink escapes the checkout",
        )),
    }
}

fn relative_path(root: &Path, path: &Path) -> Result<String, VerifiedChangeError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| VerifiedChangeError::new("a candidate path escaped its checkout root"))?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            _ => {
                return Err(VerifiedChangeError::new(
                    "a candidate path contains a parent or prefix escape",
                ))
            }
        }
    }
    if parts.is_empty() {
        return Err(VerifiedChangeError::new("a candidate path is empty"));
    }
    Ok(parts.join("/"))
}

fn canonical_dir(path: &Path) -> Result<PathBuf, VerifiedChangeError> {
    dunce::canonicalize(path)
        .map_err(|_| VerifiedChangeError::new("a required directory is unavailable"))
}

fn oracle_inside_workspace(oracle: &Path, workspace: &Path) -> bool {
    let Ok(oracle) = dunce::canonicalize(oracle) else {
        return false;
    };
    let Ok(workspace) = dunce::canonicalize(workspace) else {
        return false;
    };
    oracle == workspace || oracle.starts_with(&workspace)
}

fn git_output(git: &Path, repo: &Path, args: &[&str]) -> Result<Vec<u8>, VerifiedChangeError> {
    let output = Command::new(git)
        .current_dir(repo)
        .args(args)
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .map_err(|_| VerifiedChangeError::new("git could not be executed"))?;
    if !output.status.success() {
        return Err(VerifiedChangeError::new(
            "git refused the source identity query",
        ));
    }
    Ok(output.stdout)
}

fn git_status(git: &Path, repo: &Path, args: &[&str]) -> Result<(), VerifiedChangeError> {
    let _ = git_output(git, repo, args)?;
    Ok(())
}

fn push_text_diff(body: &mut String, left: &[u8], right: &[u8]) {
    let left = String::from_utf8_lossy(left);
    let right = String::from_utf8_lossy(right);
    for line in left.lines() {
        body.push('-');
        body.push_str(line);
        body.push('\n');
    }
    for line in right.lines() {
        body.push('+');
        body.push_str(line);
        body.push('\n');
    }
}

fn digest_bytes(bytes: &[u8]) -> String {
    format!("sha256:{}", hex_bytes(&Sha256::digest(bytes)))
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

fn canonical(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().cloned().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|key| format!("{}:{}", json!(key), canonical(&map[&key])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical).collect();
            format!("[{}]", parts.join(","))
        }
        other => other.to_string(),
    }
}

fn valid_check_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

fn valid_env_key(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_uppercase() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_') && value.len() <= 64
}

fn secret_env_key(value: &str) -> bool {
    let upper = value.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "KEY",
        "PASSWORD",
        "CREDENTIAL",
        "HOME",
        "PATH",
        "GITHUB",
        "AUTHORIZATION",
        "GROK",
        "XAI",
        "AWS",
        "SSH",
        "COOKIE",
    ]
    .iter()
    .any(|needle| upper.contains(needle))
}

pub fn write_oracle_pointer(dir: &Path, oracle: &Path) -> Result<(), VerifiedChangeError> {
    fs::create_dir_all(dir)
        .map_err(|_| VerifiedChangeError::new("the candidate directory could not be created"))?;
    let canon = canonical_dir(oracle)?;
    fs::write(dir.join("oracle-root"), canon.to_string_lossy().as_bytes())
        .map_err(|_| VerifiedChangeError::new("the oracle pointer could not be stored"))
}

pub fn read_oracle_pointer(dir: &Path) -> Result<PathBuf, VerifiedChangeError> {
    let raw = fs::read_to_string(dir.join("oracle-root"))
        .map_err(|_| VerifiedChangeError::new("the required-check oracle pointer is missing"))?;
    canonical_dir(Path::new(raw.trim()))
}

#[allow(clippy::too_many_arguments)]
pub fn assemble_candidate_verification(
    work_id: &str,
    run_id: Option<String>,
    attempt_id: Option<String>,
    checks: &[RequiredCheckSpec],
    retained: &Path,
    source: &Path,
    oracle: Option<&Path>,
    source_revision: &str,
    allowed: &[String],
    worker_stopped: bool,
    change_proposed: bool,
    diff_digest: Option<String>,
) -> CandidateVerification {
    let spec_digest =
        RequiredCheckSpec::spec_digest(checks).unwrap_or_else(|_| "sha256:invalid".into());
    let identity = candidate_content_identity(retained, source_revision).ok();
    let (bounded_diff, diff_truncated, changed_paths) = if change_proposed {
        bounded_source_diff(source, retained, allowed)
            .unwrap_or_else(|_| (String::new(), true, Vec::new()))
    } else {
        (String::new(), false, Vec::new())
    };
    let out_of_scope = changed_paths
        .iter()
        .any(|path| !allowed.iter().any(|allowed| allowed == path));
    let results = if !worker_stopped || !change_proposed || identity.is_none() || out_of_scope {
        checks
            .iter()
            .map(|check| RequiredCheckExecution {
                check_id: check.check_id.clone(),
                outcome: if worker_stopped {
                    "skipped"
                } else {
                    "incomplete"
                }
                .into(),
                exit_code: None,
                output_digest: digest_bytes(b""),
                output_truncated: false,
                duration_ms: 0,
                spec_digest: spec_digest.clone(),
            })
            .collect()
    } else if let Some(oracle) = oracle {
        execute_required_checks(checks, retained, oracle).unwrap_or_else(|_| {
            checks
                .iter()
                .map(|check| RequiredCheckExecution {
                    check_id: check.check_id.clone(),
                    outcome: "incomplete".into(),
                    exit_code: None,
                    output_digest: digest_bytes(b""),
                    output_truncated: false,
                    duration_ms: 0,
                    spec_digest: spec_digest.clone(),
                })
                .collect()
        })
    } else {
        checks
            .iter()
            .map(|check| RequiredCheckExecution {
                check_id: check.check_id.clone(),
                outcome: "missing".into(),
                exit_code: None,
                output_digest: digest_bytes(b""),
                output_truncated: false,
                duration_ms: 0,
                spec_digest: spec_digest.clone(),
            })
            .collect()
    };
    let passed =
        identity.is_some() && !out_of_scope && !diff_truncated && checks_passed(checks, &results);
    let identity = identity.unwrap_or(CandidateContentIdentity {
        source_revision: source_revision.to_string(),
        content_digest: "sha256:missing".into(),
        files: Vec::new(),
    });
    CandidateVerification {
        schema_version: CANDIDATE_VERIFICATION_SCHEMA,
        work_id: work_id.to_string(),
        run_id,
        attempt_id,
        source_revision: identity.source_revision,
        content_digest: identity.content_digest,
        spec_digest,
        files: identity.files,
        checks: results,
        worker_stopped,
        change_proposed: change_proposed && !out_of_scope,
        checks_passed: passed,
        invalidated: false,
        applied: false,
        diff_digest,
        changed_paths,
        bounded_diff,
        diff_truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn symlink_escape_is_rejected_and_untracked_source_counts() {
        let root = temp();
        fs::write(root.path().join("kept.rs"), b"fn kept() {}\n").unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(root.path().join("src/new.rs"), b"fn fresh() {}\n").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.path().join("src/escape.rs")).unwrap();
        let error = candidate_content_identity(root.path(), &"a".repeat(40)).unwrap_err();
        assert!(error.message.contains("symlink"));
    }

    #[test]
    fn build_outputs_do_not_change_source_identity() {
        let root = temp();
        fs::write(root.path().join("lib.rs"), b"fn value() -> i32 { 1 }\n").unwrap();
        let revision = "0123456789abcdef0123456789abcdef01234567";
        let before = candidate_content_identity(root.path(), revision).unwrap();
        fs::create_dir_all(root.path().join("target/debug")).unwrap();
        fs::write(root.path().join("target/debug/out"), b"not-source").unwrap();
        let after = candidate_content_identity(root.path(), revision).unwrap();
        assert_eq!(before.content_digest, after.content_digest);
        assert!(before.files.iter().any(|file| file.path == "lib.rs"));
    }

    #[test]
    fn readiness_missing_cli_dispatches_nothing() {
        let oracle = temp();
        let workspace = temp();
        let check = RequiredCheckSpec {
            check_id: "balance".into(),
            executable: "/usr/bin/true".into(),
            args: Vec::new(),
            cwd: RequiredCheckCwd::Oracle,
            env: Vec::new(),
            timeout_ms: 1000,
            max_output_bytes: 128,
        };
        let readiness = inspect_assignment_readiness(&ReadinessInput {
            executable: Path::new("/no/such/grok-cli"),
            mutation_mode: "isolated_review",
            platform: "macos",
            checks: &[check],
            oracle_root: oracle.path(),
            workspace: workspace.path(),
        });
        assert!(!readiness.ready);
        assert_eq!(readiness.workers_dispatched, 0);
        assert_eq!(readiness.provider_invocations, 0);
        assert!(readiness
            .reasons
            .iter()
            .any(|reason| reason.contains("missing")));
    }

    #[test]
    fn readiness_refuses_readonly_and_non_macos_without_spawning_work() {
        let oracle = temp();
        let workspace = temp();
        let bin = oracle.path().join("grok");
        fs::write(
            &bin,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$0.argv\"\nexit 0\n",
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let check = RequiredCheckSpec {
            check_id: "balance".into(),
            executable: "/usr/bin/true".into(),
            args: Vec::new(),
            cwd: RequiredCheckCwd::Oracle,
            env: Vec::new(),
            timeout_ms: 1000,
            max_output_bytes: 128,
        };
        for (mode, platform) in [("read_only", "macos"), ("isolated_review", "linux")] {
            let readiness = inspect_assignment_readiness(&ReadinessInput {
                executable: &bin,
                mutation_mode: mode,
                platform,
                checks: std::slice::from_ref(&check),
                oracle_root: oracle.path(),
                workspace: workspace.path(),
            });
            assert!(!readiness.ready, "{mode} {platform}");
            assert_eq!(readiness.workers_dispatched, 0);
            assert_eq!(readiness.provider_invocations, 0);
        }
        let argv = fs::read_to_string(oracle.path().join("grok.argv")).unwrap_or_default();
        assert!(!argv.contains("prompt-file"), "{argv}");
    }

    #[test]
    fn failed_check_does_not_pass_and_secret_env_is_rejected() {
        let candidate = temp();
        let oracle = temp();
        fs::write(candidate.path().join("value.txt"), b"red\n").unwrap();
        let script = oracle.path().join("check.sh");
        fs::write(
            &script,
            "#!/bin/sh\ngrep -q green \"$CANDIDATE_ROOT/value.txt\"\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let check = RequiredCheckSpec {
            check_id: "balance".into(),
            executable: script.display().to_string(),
            args: Vec::new(),
            cwd: RequiredCheckCwd::Oracle,
            env: vec![RequiredCheckEnv {
                key: "VISIBLE".into(),
                value: "1".into(),
            }],
            timeout_ms: 2000,
            max_output_bytes: 256,
        };
        let results = execute_required_checks(
            std::slice::from_ref(&check),
            candidate.path(),
            oracle.path(),
        )
        .unwrap();
        assert_eq!(results[0].outcome, "failed");
        assert!(!checks_passed(std::slice::from_ref(&check), &results));
        let mut secret = check.clone();
        secret.env = vec![RequiredCheckEnv {
            key: "GITHUB_TOKEN".into(),
            value: "nope".into(),
        }];
        assert!(validate_required_checks(&[secret]).is_err());
    }
}
