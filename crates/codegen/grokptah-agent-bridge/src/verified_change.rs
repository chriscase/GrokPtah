//! Host-controlled readiness and candidate verification for one bounded
//! managed Grok Build change.
//!
//! Readiness only inspects the installed CLI. Required checks run as explicit
//! argv against the retained candidate, never as a frontend shell, and never
//! with the operator environment.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
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
#[cfg(test)]
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
    #[serde(default)]
    pub source_fingerprint: String,
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
    #[serde(default)]
    pub reconciliation_required: bool,
    #[serde(default)]
    pub check_profile_id: String,
    #[serde(default)]
    pub check_profile_revision: u64,
    #[serde(default)]
    pub check_authority_digest: String,
    #[serde(default)]
    pub apply_bundle_digest: String,
    #[serde(default)]
    pub execution_envelope_digest: String,
}

impl CandidateVerification {
    pub fn authorizes_applied_success(
        &self,
        approved_digest: Option<&str>,
        target_revision: Option<&str>,
        profile_id: &str,
        profile_revision: u64,
        authority_digest: &str,
        approved_bundle: Option<&str>,
    ) -> bool {
        self.schema_version == CANDIDATE_VERIFICATION_SCHEMA
            && self.applied
            && !self.invalidated
            && self.checks_passed
            && self.worker_stopped
            && self.change_proposed
            && approved_digest == Some(self.content_digest.as_str())
            && approved_bundle == Some(self.apply_bundle_digest.as_str())
            && !self.apply_bundle_digest.is_empty()
            && target_revision == Some(self.source_revision.as_str())
            && self.checks.iter().all(|check| check.outcome == "passed")
            && !self.checks.is_empty()
            && !self.reconciliation_required
            && !profile_id.is_empty()
            && self.check_profile_id == profile_id
            && self.check_profile_revision == profile_revision
            && self.check_profile_revision > 0
            && !authority_digest.is_empty()
            && self.check_authority_digest == authority_digest
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

#[cfg(test)]
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

pub fn retain_candidate_snapshot(
    source: &Path,
    dest: &Path,
    base_revision: &str,
) -> Result<(), VerifiedChangeError> {
    let parent = dest.parent().ok_or_else(|| {
        VerifiedChangeError::new("the candidate snapshot directory could not be created")
    })?;
    fs::create_dir_all(parent).map_err(|_| {
        VerifiedChangeError::new("the candidate snapshot directory could not be created")
    })?;
    let before = capture_identity(source, base_revision)?;
    let patch_paths = patch_path_set(&before.patch)?;
    let manifest_paths: BTreeSet<String> = before
        .manifest
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    if patch_paths != manifest_paths {
        return Err(VerifiedChangeError::new(
            "the candidate patch paths do not equal the manifest",
        ));
    }
    let staging = parent.join(format!(".capture-{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(staging.join("tree")).map_err(|_| {
        VerifiedChangeError::new("the candidate snapshot directory could not be created")
    })?;
    crate::run_promotion::materialize_manifest(source, &staging.join("tree"), &before.manifest)
        .map_err(|error| VerifiedChangeError::new(error.to_string()))?;
    let after = capture_identity(source, base_revision)?;
    if before != after {
        let _ = fs::remove_dir_all(&staging);
        return Err(VerifiedChangeError::new(
            "the candidate changed while it was being captured",
        ));
    }
    prove_patch_reproduces_checked_tree(
        source,
        base_revision,
        &before.patch,
        &before.manifest,
        &staging.join("tree"),
    )?;
    let digest = crate::run_promotion::manifest_digest(base_revision, &before.manifest);
    let mut files = Vec::new();
    for entry in &before.manifest {
        if entry.state == "delete" {
            files.push(CandidateFileRecord {
                path: entry.path.clone(),
                digest: "absent".into(),
                kind: format!("delete:{}", entry.before_mode),
            });
            continue;
        }
        let bytes = fs::read(staging.join("tree").join(&entry.path)).unwrap_or_default();
        files.push(CandidateFileRecord {
            path: entry.path.clone(),
            digest: digest_bytes(&bytes),
            kind: entry.after_mode.clone(),
        });
    }
    let record = json!({
        "baseRevision": base_revision,
        "head": before.head,
        "gitRef": before.git_ref,
        "status": before.status,
        "finalFingerprint": before.final_fingerprint,
        "contentDigest": format!("sha256:{digest}"),
        "manifest": before.manifest,
        "files": files,
    });
    let manifest_bytes = serde_json::to_vec(&record)
        .map_err(|_| VerifiedChangeError::new("the candidate manifest could not be stored"))?;
    atomic_publish(
        &staging.join("promotion.patch"),
        &parent.join("promotion.patch"),
        &before.patch,
    )?;
    atomic_publish(
        &staging.join("manifest.json"),
        &parent.join("manifest.json"),
        &manifest_bytes,
    )?;
    if dest.exists() {
        fs::remove_dir_all(dest).map_err(|_| {
            VerifiedChangeError::new("the candidate snapshot directory could not be created")
        })?;
    }
    fs::rename(staging.join("tree"), dest).map_err(|_| {
        VerifiedChangeError::new("the candidate snapshot directory could not be created")
    })?;
    let _ = fs::remove_dir_all(&staging);
    Ok(())
}

#[derive(Clone, PartialEq, Eq)]
struct CaptureIdentity {
    head: String,
    git_ref: String,
    status: String,
    patch: Vec<u8>,
    manifest: Vec<crate::run_promotion::PathManifestEntry>,
    final_fingerprint: String,
}

fn capture_identity(
    source: &Path,
    base_revision: &str,
) -> Result<CaptureIdentity, VerifiedChangeError> {
    let captured = crate::run_promotion::capture_worktree_changes(source, base_revision)
        .map_err(|error| VerifiedChangeError::new(error.to_string()))?;
    let head = git_text(source, &["rev-parse", "HEAD"])?.trim().to_string();
    let git_ref = git_text(source, &["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap_or_else(|_| "HEAD".into())
        .trim()
        .to_string();
    let status = git_text(
        source,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    Ok(CaptureIdentity {
        head,
        git_ref,
        status,
        patch: captured.patch,
        manifest: captured.manifest,
        final_fingerprint: captured.final_fingerprint,
    })
}

fn atomic_publish(temp: &Path, dest: &Path, bytes: &[u8]) -> Result<(), VerifiedChangeError> {
    fs::write(temp, bytes)
        .map_err(|_| VerifiedChangeError::new("the candidate artifact could not be stored"))?;
    fs::rename(temp, dest)
        .map_err(|_| VerifiedChangeError::new("the candidate artifact could not be stored"))
}

fn patch_path_set(patch: &[u8]) -> Result<BTreeSet<String>, VerifiedChangeError> {
    let mut paths = BTreeSet::new();
    let text = String::from_utf8_lossy(patch);
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("diff --git ") else {
            continue;
        };
        let mut parts = rest.split_whitespace();
        let Some(a) = parts.next() else { continue };
        let Some(b) = parts.next() else { continue };
        let path = b
            .strip_prefix("b/")
            .or_else(|| a.strip_prefix("a/"))
            .unwrap_or(b);
        paths.insert(path.to_string());
    }
    Ok(paths)
}

fn prove_patch_reproduces_checked_tree(
    source: &Path,
    base_revision: &str,
    patch: &[u8],
    manifest: &[crate::run_promotion::PathManifestEntry],
    materialized: &Path,
) -> Result<(), VerifiedChangeError> {
    let scratch = std::env::temp_dir().join(format!(
        "grokptah-reproduce-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&scratch).map_err(|_| {
        VerifiedChangeError::new("the candidate reproduction checkout could not be created")
    })?;
    let checkout = scratch.join("checkout");
    let added = std::process::Command::new("git")
        .args(["worktree", "add", "--detach", "--force"])
        .arg(&checkout)
        .arg(base_revision)
        .current_dir(source)
        .output()
        .map_err(|_| {
            VerifiedChangeError::new("the candidate reproduction checkout could not be created")
        })?;
    if !added.status.success() {
        let _ = fs::remove_dir_all(&scratch);
        return Err(VerifiedChangeError::new(
            "the candidate reproduction checkout could not be created",
        ));
    }
    if !patch.is_empty() {
        let applied = std::process::Command::new("git")
            .args(["apply", "--binary", "--whitespace=nowarn"])
            .current_dir(&checkout)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                if let Some(stdin) = child.stdin.as_mut() {
                    stdin.write_all(patch)?;
                }
                child.wait()
            })
            .map_err(|_| VerifiedChangeError::new("the retained patch could not be reproduced"))?;
        if !applied.success() {
            let _ = std::process::Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(&checkout)
                .current_dir(source)
                .status();
            let _ = fs::remove_dir_all(&scratch);
            return Err(VerifiedChangeError::new(
                "the retained patch does not reproduce the checked tree",
            ));
        }
    }
    for entry in manifest {
        let checked = materialized.join(&entry.path);
        let reproduced = checkout.join(&entry.path);
        if entry.state == "delete" {
            if checked.exists() || reproduced.exists() {
                let _ = std::process::Command::new("git")
                    .args(["worktree", "remove", "--force"])
                    .arg(&checkout)
                    .current_dir(source)
                    .status();
                let _ = fs::remove_dir_all(&scratch);
                return Err(VerifiedChangeError::new(
                    "the retained patch does not reproduce the checked tree",
                ));
            }
            continue;
        }
        let checked_bytes = fs::read(&checked).unwrap_or_default();
        let reproduced_bytes = fs::read(&reproduced).unwrap_or_default();
        if checked_bytes != reproduced_bytes {
            let _ = std::process::Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(&checkout)
                .current_dir(source)
                .status();
            let _ = fs::remove_dir_all(&scratch);
            return Err(VerifiedChangeError::new(
                "the retained patch does not reproduce the checked tree",
            ));
        }
        let checked_mode = fs::symlink_metadata(&checked)
            .map(|meta| meta.permissions().mode() & 0o777)
            .unwrap_or(0);
        let reproduced_mode = fs::symlink_metadata(&reproduced)
            .map(|meta| meta.permissions().mode() & 0o777)
            .unwrap_or(0);
        if checked_mode != reproduced_mode {
            let _ = std::process::Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(&checkout)
                .current_dir(source)
                .status();
            let _ = fs::remove_dir_all(&scratch);
            return Err(VerifiedChangeError::new(
                "the retained patch does not reproduce the checked tree",
            ));
        }
    }
    let _ = std::process::Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&checkout)
        .current_dir(source)
        .status();
    let _ = fs::remove_dir_all(&scratch);
    Ok(())
}

fn git_text(source: &Path, args: &[&str]) -> Result<String, VerifiedChangeError> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(source)
        .output()
        .map_err(|_| VerifiedChangeError::new("git identity could not be read"))?;
    if !output.status.success() {
        return Err(VerifiedChangeError::new("git identity could not be read"));
    }
    String::from_utf8(output.stdout)
        .map_err(|_| VerifiedChangeError::new("git identity could not be read"))
}

pub const CHECK_CONFINEMENT_BACKEND: &str = "macos-sandbox-exec";
pub const CHECK_CONFINEMENT_REVISION: u64 = 1;
pub const CHECK_WRITE_POLICY: &str = "deny-source-and-candidate";

pub static CHECK_CONFINEMENT_EXECUTABLE: std::sync::Mutex<Option<PathBuf>> =
    std::sync::Mutex::new(None);
pub static SKIP_VERIFIED_DRIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub fn skip_verified_drive() -> bool {
    SKIP_VERIFIED_DRIVE.load(std::sync::atomic::Ordering::SeqCst)
}

pub fn confinement_executable() -> PathBuf {
    CHECK_CONFINEMENT_EXECUTABLE
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .unwrap_or_else(|| PathBuf::from("/usr/bin/sandbox-exec"))
}

pub fn confinement_available() -> bool {
    confinement_executable().is_file()
}

#[derive(Clone)]
pub struct CheckAuthority {
    pub profile_id: String,
    pub profile_revision: u64,
    pub executable_path: String,
    pub executable_digest: String,
    pub oracle_root: String,
    pub oracle_digest: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: Vec<RequiredCheckEnv>,
    pub timeout_ms: u64,
    pub max_output_bytes: u64,
    pub write_policy: String,
    pub network: String,
    pub source_root: PathBuf,
    pub output_limit_bytes: u64,
    pub confinement_backend: String,
    pub confinement_revision: u64,
    pub work_id: String,
    pub session_id: String,
    pub workspace: String,
    pub authority_digest: String,
}

fn check_cwd_name(cwd: RequiredCheckCwd) -> &'static str {
    match cwd {
        RequiredCheckCwd::Candidate => "candidate",
        RequiredCheckCwd::Oracle => "oracle",
    }
}

fn same_directory(left: &Path, right: &Path) -> bool {
    match (dunce::canonicalize(left), dunce::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

impl CheckAuthority {
    pub fn bind_invocation(&mut self, check: &RequiredCheckSpec, oracle_root: &Path) {
        self.executable_path = check.executable.clone();
        self.oracle_root = oracle_root.display().to_string();
        self.args = check.args.clone();
        self.cwd = check_cwd_name(check.cwd).into();
        self.env = check.env.clone();
        self.timeout_ms = check.timeout_ms;
        self.max_output_bytes = u64::from(check.max_output_bytes);
        self.write_policy = CHECK_WRITE_POLICY.into();
    }

    pub fn matches_invocation(&self, check: &RequiredCheckSpec, oracle_root: &Path) -> bool {
        self.executable_path == check.executable
            && same_directory(Path::new(&self.oracle_root), oracle_root)
            && self.args == check.args
            && self.cwd == check_cwd_name(check.cwd)
            && self.env == check.env
            && self.timeout_ms == check.timeout_ms
            && self.max_output_bytes == u64::from(check.max_output_bytes)
            && self.write_policy == CHECK_WRITE_POLICY
    }

    pub fn canonical_digest(&self) -> String {
        fn push(hasher: &mut Sha256, value: &[u8]) {
            hasher.update(value);
            hasher.update([0]);
        }
        let mut hasher = Sha256::new();
        hasher.update(b"grokptah-check-authority-v1\0");
        push(&mut hasher, self.profile_id.as_bytes());
        push(&mut hasher, self.profile_revision.to_string().as_bytes());
        push(&mut hasher, self.executable_path.as_bytes());
        push(&mut hasher, self.executable_digest.as_bytes());
        push(&mut hasher, self.oracle_root.as_bytes());
        push(&mut hasher, self.oracle_digest.as_bytes());
        for arg in &self.args {
            push(&mut hasher, arg.as_bytes());
        }
        hasher.update([1]);
        push(&mut hasher, self.cwd.as_bytes());
        for entry in &self.env {
            push(&mut hasher, entry.key.as_bytes());
            push(&mut hasher, entry.value.as_bytes());
        }
        hasher.update([1]);
        push(&mut hasher, self.timeout_ms.to_string().as_bytes());
        push(&mut hasher, self.max_output_bytes.to_string().as_bytes());
        push(&mut hasher, self.output_limit_bytes.to_string().as_bytes());
        push(&mut hasher, self.write_policy.as_bytes());
        push(&mut hasher, self.network.as_bytes());
        push(&mut hasher, self.source_root.to_string_lossy().as_bytes());
        push(&mut hasher, self.confinement_backend.as_bytes());
        push(
            &mut hasher,
            self.confinement_revision.to_string().as_bytes(),
        );
        push(&mut hasher, self.work_id.as_bytes());
        push(&mut hasher, self.session_id.as_bytes());
        push(&mut hasher, self.workspace.as_bytes());
        digest_bytes(&hasher.finalize())
    }

    pub fn seal(mut self) -> Self {
        self.confinement_backend = CHECK_CONFINEMENT_BACKEND.into();
        self.confinement_revision = CHECK_CONFINEMENT_REVISION;
        self.write_policy = CHECK_WRITE_POLICY.into();
        self.authority_digest = self.canonical_digest();
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedExecutionEnvelopeV1 {
    pub schema_version: u32,
    pub work_id: String,
    pub session_id: String,
    pub workspace: String,
    pub agent_id: String,
    pub agent_revision: u64,
    pub executor: String,
    pub budget: String,
    pub platform: String,
    pub execution_host: String,
    pub check_profile_id: String,
    pub check_profile_revision: u64,
    pub executable_digest: String,
    pub oracle_digest: String,
    pub check_authority_digest: String,
    pub max_prompt_bytes: u64,
    pub max_rounds: u64,
    pub max_duration_ms: u64,
    pub envelope_digest: String,
}

impl VerifiedExecutionEnvelopeV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        work_id: &str,
        session_id: &str,
        workspace: &str,
        agent_id: &str,
        agent_revision: u64,
        budget: &str,
        platform: &str,
        execution_host: &str,
        check_profile_id: &str,
        check_profile_revision: u64,
        executable_digest: &str,
        oracle_digest: &str,
        check_authority_digest: &str,
        max_prompt_bytes: u64,
        max_rounds: u64,
        max_duration_ms: u64,
    ) -> Self {
        let mut envelope = Self {
            schema_version: 1,
            work_id: work_id.into(),
            session_id: session_id.into(),
            workspace: workspace.into(),
            agent_id: agent_id.into(),
            agent_revision,
            executor: "grok_build_isolated_review".into(),
            budget: budget.into(),
            platform: platform.into(),
            execution_host: execution_host.into(),
            check_profile_id: check_profile_id.into(),
            check_profile_revision,
            executable_digest: executable_digest.into(),
            oracle_digest: oracle_digest.into(),
            check_authority_digest: check_authority_digest.into(),
            max_prompt_bytes,
            max_rounds,
            max_duration_ms,
            envelope_digest: String::new(),
        };
        envelope.envelope_digest = envelope.canonical_digest();
        envelope
    }

    pub fn canonical_digest(&self) -> String {
        digest_bytes(
            format!(
                "grokptah-execution-envelope-v1\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
                self.schema_version,
                self.work_id,
                self.session_id,
                self.workspace,
                self.agent_id,
                self.agent_revision,
                self.executor,
                self.budget,
                self.platform,
                self.execution_host,
                self.check_profile_id,
                self.check_profile_revision,
                self.executable_digest,
                self.oracle_digest,
                self.check_authority_digest,
                self.max_prompt_bytes,
                self.max_rounds,
                self.max_duration_ms
            )
            .as_bytes(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn permits(
        &self,
        work_id: &str,
        session_id: &str,
        workspace: &str,
        agent_id: &str,
        agent_revision: u64,
        decision_digest: &str,
        kind: &str,
    ) -> bool {
        self.schema_version == 1
            && kind == "isolated-review"
            && self.work_id == work_id
            && self.session_id == session_id
            && self.workspace == workspace
            && self.agent_id == agent_id
            && self.agent_revision == agent_revision
            && self.envelope_digest == self.canonical_digest()
            && self.envelope_digest == decision_digest
    }
}

pub fn write_check_authority(
    dir: &Path,
    authority: &CheckAuthority,
) -> Result<(), VerifiedChangeError> {
    if authority.authority_digest != authority.canonical_digest() {
        return Err(VerifiedChangeError::new(
            "the check authority digest does not match its fields",
        ));
    }
    let body = json!({
        "schemaVersion": 1,
        "profileId": authority.profile_id,
        "profileRevision": authority.profile_revision,
        "executablePath": authority.executable_path,
        "executableDigest": authority.executable_digest,
        "oracleRoot": authority.oracle_root,
        "oracleDigest": authority.oracle_digest,
        "args": authority.args,
        "cwd": authority.cwd,
        "env": authority.env,
        "timeoutMs": authority.timeout_ms,
        "maxOutputBytes": authority.max_output_bytes,
        "writePolicy": authority.write_policy,
        "network": authority.network,
        "sourceRoot": authority.source_root,
        "outputLimitBytes": authority.output_limit_bytes,
        "confinementBackend": authority.confinement_backend,
        "confinementRevision": authority.confinement_revision,
        "workId": authority.work_id,
        "sessionId": authority.session_id,
        "workspace": authority.workspace,
        "authorityDigest": authority.authority_digest,
    });
    fs::create_dir_all(dir)
        .and_then(|_| fs::write(dir.join("check-authority.json"), body.to_string()))
        .map_err(|_| VerifiedChangeError::new("the check authority could not be stored"))
}

pub fn read_check_authority(dir: &Path) -> Result<CheckAuthority, VerifiedChangeError> {
    let raw = fs::read_to_string(dir.join("check-authority.json"))
        .map_err(|_| VerifiedChangeError::new("the check authority is missing"))?;
    let value: Value = serde_json::from_str(&raw)
        .map_err(|_| VerifiedChangeError::new("the check authority is malformed"))?;
    if value.get("schemaVersion").and_then(Value::as_u64) != Some(1) {
        return Err(VerifiedChangeError::new(
            "the check authority schema is unsupported",
        ));
    }
    let required = |key: &str| {
        value
            .get(key)
            .ok_or_else(|| VerifiedChangeError::new("the check authority is malformed"))
    };
    let authority = CheckAuthority {
        profile_id: required("profileId")?
            .as_str()
            .unwrap_or_default()
            .to_string(),
        profile_revision: required("profileRevision")?.as_u64().unwrap_or(0),
        executable_path: required("executablePath")?
            .as_str()
            .unwrap_or_default()
            .to_string(),
        executable_digest: required("executableDigest")?
            .as_str()
            .unwrap_or_default()
            .to_string(),
        oracle_root: required("oracleRoot")?
            .as_str()
            .unwrap_or_default()
            .to_string(),
        oracle_digest: required("oracleDigest")?
            .as_str()
            .unwrap_or_default()
            .to_string(),
        args: serde_json::from_value(required("args")?.clone())
            .map_err(|_| VerifiedChangeError::new("the check authority is malformed"))?,
        cwd: required("cwd")?.as_str().unwrap_or_default().to_string(),
        env: serde_json::from_value(required("env")?.clone())
            .map_err(|_| VerifiedChangeError::new("the check authority is malformed"))?,
        timeout_ms: required("timeoutMs")?.as_u64().unwrap_or(0),
        max_output_bytes: required("maxOutputBytes")?.as_u64().unwrap_or(0),
        write_policy: required("writePolicy")?
            .as_str()
            .unwrap_or_default()
            .to_string(),
        network: required("network")?
            .as_str()
            .unwrap_or_default()
            .to_string(),
        source_root: PathBuf::from(required("sourceRoot")?.as_str().unwrap_or_default()),
        output_limit_bytes: required("outputLimitBytes")?.as_u64().unwrap_or(0),
        confinement_backend: required("confinementBackend")?
            .as_str()
            .unwrap_or_default()
            .to_string(),
        confinement_revision: required("confinementRevision")?.as_u64().unwrap_or(0),
        work_id: required("workId")?.as_str().unwrap_or_default().to_string(),
        session_id: required("sessionId")?
            .as_str()
            .unwrap_or_default()
            .to_string(),
        workspace: required("workspace")?
            .as_str()
            .unwrap_or_default()
            .to_string(),
        authority_digest: required("authorityDigest")?
            .as_str()
            .unwrap_or_default()
            .to_string(),
    };
    if authority.authority_digest != authority.canonical_digest()
        || authority.confinement_backend != CHECK_CONFINEMENT_BACKEND
        || authority.write_policy != CHECK_WRITE_POLICY
    {
        return Err(VerifiedChangeError::new(
            "the check authority digest does not match its fields",
        ));
    }
    Ok(authority)
}

pub fn execute_required_checks(
    checks: &[RequiredCheckSpec],
    candidate_root: &Path,
    oracle_root: &Path,
) -> Result<Vec<RequiredCheckExecution>, VerifiedChangeError> {
    let mut authority = CheckAuthority {
        profile_id: "direct".into(),
        profile_revision: 1,
        executable_path: String::new(),
        executable_digest: checks
            .first()
            .and_then(|check| file_digest(Path::new(&check.executable)))
            .unwrap_or_else(|| "sha256:missing".into()),
        oracle_root: String::new(),
        oracle_digest: directory_digest(oracle_root).unwrap_or_else(|| "sha256:missing".into()),
        args: Vec::new(),
        cwd: String::new(),
        env: Vec::new(),
        timeout_ms: 0,
        max_output_bytes: 0,
        write_policy: CHECK_WRITE_POLICY.into(),
        network: "none".into(),
        source_root: candidate_root.to_path_buf(),
        output_limit_bytes: 65_536,
        confinement_backend: String::new(),
        confinement_revision: 0,
        work_id: "direct".into(),
        session_id: "direct".into(),
        workspace: candidate_root.display().to_string(),
        authority_digest: String::new(),
    };
    if let Some(check) = checks.first() {
        authority.bind_invocation(check, oracle_root);
        authority.executable_digest =
            file_digest(Path::new(&check.executable)).unwrap_or_else(|| "sha256:missing".into());
        authority.oracle_digest =
            directory_digest(oracle_root).unwrap_or_else(|| "sha256:missing".into());
    }
    let authority = authority.seal();
    execute_required_checks_with_authority(checks, candidate_root, oracle_root, Some(&authority))
}

pub fn execute_required_checks_with_authority(
    checks: &[RequiredCheckSpec],
    candidate_root: &Path,
    oracle_root: &Path,
    authority: Option<&CheckAuthority>,
) -> Result<Vec<RequiredCheckExecution>, VerifiedChangeError> {
    validate_required_checks(checks)?;
    if oracle_inside_workspace(oracle_root, candidate_root) {
        return Err(VerifiedChangeError::new(
            "the required-check oracle is inside the candidate tree",
        ));
    }
    let spec_digest = RequiredCheckSpec::spec_digest(checks)?;
    let Some(authority) = authority else {
        return Ok(checks
            .iter()
            .map(|check| RequiredCheckExecution {
                check_id: check.check_id.clone(),
                outcome: "invalidated".into(),
                exit_code: None,
                output_digest: digest_bytes(b""),
                output_truncated: false,
                duration_ms: 0,
                spec_digest: spec_digest.clone(),
            })
            .collect());
    };
    if authority.authority_digest != authority.canonical_digest()
        || authority.confinement_backend != CHECK_CONFINEMENT_BACKEND
        || !confinement_available()
    {
        return Ok(checks
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
            .collect());
    }
    let before = snapshot_identity(candidate_root);
    let source_before = sealed_tree_token(&authority.source_root);
    let mut results = Vec::with_capacity(checks.len());
    for check in checks {
        if !authority.matches_invocation(check, oracle_root)
            || file_digest(Path::new(&check.executable)).as_deref()
                != Some(authority.executable_digest.as_str())
            || directory_digest(oracle_root).as_deref() != Some(authority.oracle_digest.as_str())
        {
            results.push(RequiredCheckExecution {
                check_id: check.check_id.clone(),
                outcome: "invalidated".into(),
                exit_code: None,
                output_digest: digest_bytes(b""),
                output_truncated: false,
                duration_ms: 0,
                spec_digest: spec_digest.clone(),
            });
            continue;
        }
        results.push(run_one_check(
            check,
            candidate_root,
            oracle_root,
            &spec_digest,
            authority,
        ));
    }
    let source_changed = source_before.is_some_and(|before| {
        sealed_tree_token(&authority.source_root).as_deref() != Some(before.as_str())
    });
    if snapshot_identity(candidate_root) != before || source_changed {
        for result in &mut results {
            if result.outcome == "passed" {
                result.outcome = "failed".into();
            }
        }
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

pub fn retained_matches(
    retained: &Path,
    digest: &str,
    files: &[CandidateFileRecord],
) -> Result<bool, VerifiedChangeError> {
    let record = read_promotion_record(retained)?;
    if record.content_digest != digest || record.files != files {
        return Ok(false);
    }
    for file in &record.files {
        let path = retained.join(&file.path);
        if file.digest == "absent" {
            if path.exists() {
                return Ok(false);
            }
            continue;
        }
        let bytes = fs::read(&path).unwrap_or_default();
        if digest_bytes(&bytes) != file.digest {
            return Ok(false);
        }
        if let Ok(mode) = u32::from_str_radix(&file.kind, 8) {
            let actual = fs::symlink_metadata(&path)
                .map(|meta| meta.permissions().mode() & 0o777)
                .unwrap_or(0);
            if actual != (mode & 0o777) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

#[derive(Debug)]
pub struct RetainedCandidateBinding {
    pub head: String,
    pub git_ref: String,
    pub changed_paths: Vec<String>,
    pub diff_digest: String,
}

/// Digest and changed paths of the single retained patch, manifest, and tree.
/// The checkout must still contain those same bytes or the bind is refused.
pub fn bind_retained_candidate(
    checkout: &Path,
    retained_tree: &Path,
) -> Result<RetainedCandidateBinding, VerifiedChangeError> {
    let record = read_promotion_record(retained_tree)?;
    let parent = retained_tree.parent().unwrap_or(retained_tree);
    let raw = fs::read_to_string(parent.join("manifest.json"))
        .map_err(|_| VerifiedChangeError::new("the candidate manifest is missing"))?;
    let value: Value = serde_json::from_str(&raw)
        .map_err(|_| VerifiedChangeError::new("the candidate manifest is unreadable"))?;
    let head = value
        .get("head")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let git_ref = value
        .get("gitRef")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if !retained_tree_matches_checkout(checkout, retained_tree, &record.manifest)? {
        return Err(VerifiedChangeError::new(
            "the retained candidate bytes do not match the checked tree",
        ));
    }
    let mut changed_paths: Vec<String> = record
        .manifest
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    changed_paths.sort();
    let diff_digest = digest_retained_candidate(&record.patch, &record.manifest, retained_tree)?;
    Ok(RetainedCandidateBinding {
        head,
        git_ref,
        changed_paths,
        diff_digest,
    })
}

pub fn retained_candidate_diff_digest(retained_tree: &Path) -> Result<String, VerifiedChangeError> {
    let record = read_promotion_record(retained_tree)?;
    digest_retained_candidate(&record.patch, &record.manifest, retained_tree)
}

fn digest_retained_candidate(
    patch: &[u8],
    manifest: &[crate::run_promotion::PathManifestEntry],
    tree: &Path,
) -> Result<String, VerifiedChangeError> {
    let mut entries = manifest.to_vec();
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let mut hasher = Sha256::new();
    hasher.update(b"grokptah-retained-candidate-v1\0");
    hasher.update(patch);
    hasher.update([0]);
    for entry in &entries {
        hasher.update(entry.path.as_bytes());
        hasher.update([0]);
        hasher.update(entry.state.as_bytes());
        hasher.update([0]);
        if entry.state == "delete" {
            hasher.update(b"deleted");
            hasher.update([0]);
            continue;
        }
        let bytes = fs::read(tree.join(&entry.path)).map_err(|_| {
            VerifiedChangeError::new("the retained candidate file could not be read")
        })?;
        hasher.update(&bytes);
        hasher.update([0]);
    }
    Ok(format!("sha256:{}", hex_bytes(&hasher.finalize())))
}

fn retained_tree_matches_checkout(
    checkout: &Path,
    retained_tree: &Path,
    manifest: &[crate::run_promotion::PathManifestEntry],
) -> Result<bool, VerifiedChangeError> {
    for entry in manifest {
        let live = snapshot_entry(&checkout.join(&entry.path))?;
        let kept = snapshot_entry(&retained_tree.join(&entry.path))?;
        if entry.state == "delete" {
            if live.is_some() || kept.is_some() {
                return Ok(false);
            }
            continue;
        }
        if live != kept {
            return Ok(false);
        }
    }
    Ok(true)
}

fn snapshot_entry(path: &Path) -> Result<Option<(Vec<u8>, u32)>, VerifiedChangeError> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(VerifiedChangeError::new(
                "the candidate file could not be read",
            ))
        }
    };
    let mode = meta.permissions().mode() & 0o777;
    let bytes = if meta.file_type().is_symlink() {
        fs::read_link(path)
            .map_err(|_| VerifiedChangeError::new("a candidate symlink could not be read"))?
            .to_string_lossy()
            .into_owned()
            .into_bytes()
    } else if meta.is_file() {
        fs::read(path)
            .map_err(|_| VerifiedChangeError::new("the candidate file could not be read"))?
    } else {
        return Err(VerifiedChangeError::new(
            "the candidate contains a non-regular source file",
        ));
    };
    Ok(Some((bytes, mode)))
}

pub fn apply_fault() -> u8 {
    APPLY_FAULT.load(std::sync::atomic::Ordering::SeqCst)
}

pub static APPLY_FAULT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub(crate) struct PromotionRecord {
    pub base_revision: String,
    pub final_fingerprint: String,
    pub content_digest: String,
    pub manifest: Vec<crate::run_promotion::PathManifestEntry>,
    pub files: Vec<CandidateFileRecord>,
    pub patch: Vec<u8>,
}

pub(crate) fn read_promotion_record(
    retained: &Path,
) -> Result<PromotionRecord, VerifiedChangeError> {
    let parent = retained.parent().unwrap_or(retained);
    let raw = fs::read_to_string(parent.join("manifest.json"))
        .map_err(|_| VerifiedChangeError::new("the candidate manifest is missing"))?;
    let value: Value = serde_json::from_str(&raw)
        .map_err(|_| VerifiedChangeError::new("the candidate manifest is unreadable"))?;
    let manifest = serde_json::from_value(value.get("manifest").cloned().unwrap_or(Value::Null))
        .map_err(|_| VerifiedChangeError::new("the candidate manifest is unreadable"))?;
    let files = serde_json::from_value(value.get("files").cloned().unwrap_or(Value::Null))
        .unwrap_or_default();
    let patch = fs::read(parent.join("promotion.patch"))
        .map_err(|_| VerifiedChangeError::new("the promotion patch is missing"))?;
    Ok(PromotionRecord {
        base_revision: value
            .get("baseRevision")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        final_fingerprint: value
            .get("finalFingerprint")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        content_digest: value
            .get("contentDigest")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        manifest,
        files,
        patch,
    })
}

pub fn apply_candidate_tree(
    git: &Path,
    source: &Path,
    retained: &Path,
    expected: &CandidateContentIdentity,
    source_fingerprint: &str,
    allowed: &[String],
) -> Result<(), VerifiedChangeError> {
    let observed = source_revision(git, source)?;
    if observed != expected.source_revision {
        return Err(VerifiedChangeError::new(
            "the target revision changed after verification",
        ));
    }
    let record = read_promotion_record(retained)?;
    if record.base_revision != expected.source_revision
        || record.content_digest != expected.content_digest
        || record.files != expected.files
    {
        return Err(VerifiedChangeError::new(
            "the retained candidate no longer matches the verified content identity",
        ));
    }
    if record
        .manifest
        .iter()
        .any(|entry| !allowed.iter().any(|allowed| allowed == &entry.path))
    {
        return Err(VerifiedChangeError::new(
            "the candidate changes a path outside the allowed file scope",
        ));
    }
    if source_fingerprint.is_empty() || record.final_fingerprint.is_empty() {
        return Err(VerifiedChangeError::new(
            "the launch source fingerprint was not retained",
        ));
    }
    crate::run_promotion::apply_recorded_patch(
        source,
        &expected.source_revision,
        source_fingerprint,
        &record.patch,
        &record.final_fingerprint,
        &record.manifest,
        apply_fault(),
    )
    .map(|_| ())
    .map_err(|error| VerifiedChangeError::new(error.to_string()))
}

fn check_sandbox_profile(output: &Path, network: &str) -> String {
    let network_rule = if network == "qualified" {
        "(allow network*)"
    } else {
        "(deny network*)"
    };
    format!(
        "(version 1)\n(deny default)\n(allow process*)\n(allow signal)\n(allow sysctl-read)\n(allow file-read*)\n(allow file-ioctl (literal \"/dev/null\"))\n(allow file-write* (subpath \"{}\"))\n{network_rule}\n",
        output.display()
    )
}

pub fn file_digest(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    Some(digest_bytes(&bytes))
}

pub fn directory_digest(root: &Path) -> Option<String> {
    let mut files = Vec::new();
    let mut total = 0u64;
    walk_identity(root, root, &mut files, &mut total).ok()?;
    let mut hasher = Sha256::new();
    for file in &files {
        hasher.update(file.path.as_bytes());
        hasher.update([0]);
        hasher.update(file.kind.as_bytes());
        hasher.update([0]);
        hasher.update(file.digest.as_bytes());
        hasher.update(b"\n");
    }
    Some(digest_bytes(&hasher.finalize()))
}

fn directory_size(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

fn snapshot_identity(root: &Path) -> String {
    directory_digest(root).unwrap_or_else(|| "missing".into())
}

fn sealed_tree_token(root: &Path) -> Option<String> {
    if root.as_os_str().is_empty() || !root.is_dir() {
        return None;
    }
    let status = Command::new("/usr/bin/git")
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .current_dir(root)
        .output();
    let diff = Command::new("/usr/bin/git")
        .args(["diff", "--raw", "-z", "--no-ext-diff"])
        .current_dir(root)
        .output();
    if let (Ok(status), Ok(diff)) = (status, diff) {
        if status.status.success() && diff.status.success() {
            let mut bytes = status.stdout;
            bytes.extend(diff.stdout);
            return Some(digest_bytes(&bytes));
        }
    }
    Some(snapshot_identity(root))
}

pub fn resolve_check_profile(
    home: &Path,
    profile_id: &str,
) -> Result<(RequiredCheckSpec, CheckAuthority, PathBuf), VerifiedChangeError> {
    if !valid_check_id(profile_id) {
        return Err(VerifiedChangeError::new(
            "the check profile id is not a host token",
        ));
    }
    let path = home
        .join("check-profiles")
        .join(format!("{profile_id}.json"));
    let raw = fs::read_to_string(&path)
        .map_err(|_| VerifiedChangeError::new("the check profile is not installed on this host"))?;
    let value: Value = serde_json::from_str(&raw)
        .map_err(|_| VerifiedChangeError::new("the check profile is unreadable"))?;
    let executable = value
        .get("executable")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let oracle_root = PathBuf::from(
        value
            .get("oracleRoot")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    let executable_digest = file_digest(Path::new(&executable)).ok_or_else(|| {
        VerifiedChangeError::new("the check profile executable could not be sealed")
    })?;
    let expected_executable = value
        .get("executableDigest")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if expected_executable != executable_digest {
        return Err(VerifiedChangeError::new(
            "the check profile executable no longer matches its sealed digest",
        ));
    }
    let oracle_digest = directory_digest(&oracle_root)
        .ok_or_else(|| VerifiedChangeError::new("the check profile oracle could not be sealed"))?;
    let expected_oracle = value
        .get("oracleDigest")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if expected_oracle != oracle_digest {
        return Err(VerifiedChangeError::new(
            "the check profile oracle no longer matches its sealed digest",
        ));
    }
    let cwd = match value.get("cwd").and_then(Value::as_str).unwrap_or("oracle") {
        "candidate" => RequiredCheckCwd::Candidate,
        "oracle" => RequiredCheckCwd::Oracle,
        _ => {
            return Err(VerifiedChangeError::new(
                "the check profile cwd policy is not supported",
            ))
        }
    };
    let args = value
        .get("args")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let env = value
        .get("env")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(RequiredCheckEnv {
                        key: item.get("key")?.as_str()?.to_string(),
                        value: item.get("value")?.as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let spec = RequiredCheckSpec {
        check_id: profile_id.to_string(),
        executable,
        args,
        cwd,
        env,
        timeout_ms: value.get("timeoutMs").and_then(Value::as_u64).unwrap_or(0),
        max_output_bytes: value
            .get("maxOutputBytes")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
    };
    validate_required_checks(std::slice::from_ref(&spec))?;
    let oracle_canon = canonical_dir(&oracle_root)?;
    let mut authority = CheckAuthority {
        profile_id: profile_id.to_string(),
        profile_revision: value
            .get("profileRevision")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        executable_path: String::new(),
        executable_digest,
        oracle_root: String::new(),
        oracle_digest,
        args: Vec::new(),
        cwd: String::new(),
        env: Vec::new(),
        timeout_ms: 0,
        max_output_bytes: 0,
        write_policy: CHECK_WRITE_POLICY.into(),
        network: value
            .get("network")
            .and_then(Value::as_str)
            .unwrap_or("none")
            .to_string(),
        source_root: PathBuf::new(),
        output_limit_bytes: value
            .get("maxOutputDirBytes")
            .and_then(Value::as_u64)
            .unwrap_or(65_536),
        confinement_backend: String::new(),
        confinement_revision: 0,
        work_id: String::new(),
        session_id: String::new(),
        workspace: String::new(),
        authority_digest: String::new(),
    };
    authority.bind_invocation(&spec, &oracle_canon);
    if authority.profile_revision == 0 {
        return Err(VerifiedChangeError::new(
            "the check profile revision is missing",
        ));
    }
    if authority.network != "none" && authority.network != "qualified" {
        return Err(VerifiedChangeError::new(
            "the check profile network policy is not supported",
        ));
    }
    Ok((spec, authority, oracle_root))
}

fn run_one_check(
    check: &RequiredCheckSpec,
    candidate_root: &Path,
    oracle_root: &Path,
    spec_digest: &str,
    authority: &CheckAuthority,
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
    let output_dir = std::env::temp_dir().join(format!(
        "grokptah-check-{}-{}-{}-{}",
        authority.work_id,
        check.check_id,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0)
    ));
    if fs::create_dir(&output_dir).is_err() {
        return base("incomplete", None, false);
    }
    // sandbox-exec matches the resolved directory. dunce keeps the symlink path,
    // and that path does not authorize writes into the private output directory.
    #[allow(clippy::disallowed_methods)]
    let output_dir = fs::canonicalize(&output_dir).unwrap_or(output_dir);
    let _ = fs::set_permissions(&output_dir, fs::Permissions::from_mode(0o700));
    let network = authority.network.as_str();
    let mut command = Command::new(confinement_executable());
    command
        .arg("-p")
        .arg(check_sandbox_profile(&output_dir, network))
        .arg(&check.executable);
    command
        .args(&check.args)
        .current_dir(cwd)
        .env_clear()
        .env("CANDIDATE_ROOT", candidate.to_string_lossy().as_ref())
        .env("ORACLE_ROOT", oracle.to_string_lossy().as_ref())
        .env("CHECK_OUTPUT", output_dir.to_string_lossy().as_ref())
        .env(
            "SOURCE_ROOT",
            authority.source_root.to_string_lossy().as_ref(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    for entry in &check.env {
        command.env(&entry.key, &entry.value);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return base("incomplete", None, false),
    };
    let group_pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let cap = check.max_output_bytes as usize;
    let deadline = started + Duration::from_millis(check.timeout_ms);
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stdout_stop = stop.clone();
    let stderr_stop = stop.clone();
    let stdout_thread =
        thread::spawn(move || read_capped_until(stdout, cap, deadline, stdout_stop));
    let stderr_thread =
        thread::spawn(move || read_capped_until(stderr, cap, deadline, stderr_stop));
    let mut output_limited = false;
    let waited = loop {
        if directory_size(&output_dir) > authority.output_limit_bytes {
            output_limited = true;
            terminate_check_tree(&mut child);
            break None;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                terminate_check_tree(&mut child);
                break Some(status);
            }
            Ok(None) if Instant::now() >= deadline => {
                terminate_check_tree(&mut child);
                break None;
            }
            Ok(None) => thread::sleep(Duration::from_millis(15)),
            Err(_) => {
                terminate_check_tree(&mut child);
                break None;
            }
        }
    };
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let stdout = stdout_thread.join().unwrap_or(CappedRead {
        bytes: Vec::new(),
        truncated: true,
    });
    let stderr = stderr_thread.join().unwrap_or(CappedRead {
        bytes: Vec::new(),
        truncated: true,
    });
    let truncated = stdout.truncated || stderr.truncated || output_limited;
    let mut output = stdout.bytes;
    output.extend(stderr.bytes);
    let digest = digest_bytes(&output);
    let group_gone = check_group_gone(group_pid);
    let output_too_large = directory_size(&output_dir) > authority.output_limit_bytes;
    let _ = fs::remove_dir_all(&output_dir);
    let (outcome, exit_code) = if !group_gone {
        ("unterminated", None)
    } else {
        match waited {
            None => ("timed_out", None),
            Some(status) if truncated || output_too_large => ("truncated", status.code()),
            Some(status) if status.success() => ("passed", status.code()),
            Some(status) => ("failed", status.code()),
        }
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

fn terminate_check_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        if pid > 0 {
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(200) {
        match child.try_wait() {
            Ok(Some(_)) => return,
            _ => thread::sleep(Duration::from_millis(10)),
        }
    }
}

pub fn run_before_candidate_bind_hook() {
    if let Ok(guard) = BEFORE_CANDIDATE_BIND.lock() {
        if let Some(hook) = guard.as_ref() {
            hook();
        }
    }
}

pub static BEFORE_CANDIDATE_BIND: std::sync::Mutex<Option<fn()>> = std::sync::Mutex::new(None);

struct CappedRead {
    bytes: Vec<u8>,
    truncated: bool,
}

fn check_group_gone(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(200) {
            let status = unsafe { libc::kill(-(pid as i32), 0) };
            if status != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn read_capped_until(
    pipe: Option<impl Read + std::os::unix::io::AsRawFd>,
    cap: usize,
    deadline: Instant,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> CappedRead {
    let Some(pipe) = pipe else {
        return CappedRead {
            bytes: Vec::new(),
            truncated: false,
        };
    };
    #[cfg(unix)]
    {
        let fd = pipe.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags >= 0 {
            unsafe {
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }
    }
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut pipe = pipe;
    let mut buf = [0u8; 1024];
    loop {
        let stopped = stop.load(std::sync::atomic::Ordering::SeqCst);
        if Instant::now() >= deadline || (stopped && bytes.len() >= cap) {
            if Instant::now() >= deadline {
                truncated = true;
            }
            break;
        }
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
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline || stop.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
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
            kind: format!("file:{:o}", meta.permissions().mode() & 0o777),
        });
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignmentContext {
    pub execution_host: String,
    pub platform: String,
    pub mutation_mode: String,
}

fn assignment_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
}

pub fn write_assignment_context(
    dir: &Path,
    execution_host: &str,
    platform: &str,
    mutation_mode: &str,
) -> Result<(), VerifiedChangeError> {
    if !matches!(execution_host, "desktop" | "service")
        || !matches!(mutation_mode, "isolated_review" | "read_only")
        || !assignment_token(platform)
    {
        return Err(VerifiedChangeError::new(
            "assignment context is not a supported host, platform, or mode",
        ));
    }
    fs::create_dir_all(dir)
        .map_err(|_| VerifiedChangeError::new("the candidate directory could not be created"))?;
    let body = json!({
        "executionHost": execution_host,
        "platform": platform,
        "mutationMode": mutation_mode,
    });
    fs::write(
        dir.join("assignment.json"),
        serde_json::to_vec(&body)
            .map_err(|_| VerifiedChangeError::new("the assignment context could not be stored"))?,
    )
    .map_err(|_| VerifiedChangeError::new("the assignment context could not be stored"))
}

pub fn read_assignment_context(dir: &Path) -> Result<AssignmentContext, VerifiedChangeError> {
    let raw = fs::read_to_string(dir.join("assignment.json"))
        .map_err(|_| VerifiedChangeError::new("the assignment context was not recorded"))?;
    let value: Value = serde_json::from_str(&raw)
        .map_err(|_| VerifiedChangeError::new("the assignment context is unreadable"))?;
    let execution_host = value
        .get("executionHost")
        .and_then(Value::as_str)
        .unwrap_or("");
    let platform = value.get("platform").and_then(Value::as_str).unwrap_or("");
    let mutation_mode = value
        .get("mutationMode")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !matches!(execution_host, "desktop" | "service")
        || !matches!(mutation_mode, "isolated_review" | "read_only")
        || !assignment_token(platform)
    {
        return Err(VerifiedChangeError::new(
            "the assignment context is not a supported host, platform, or mode",
        ));
    }
    Ok(AssignmentContext {
        execution_host: execution_host.to_string(),
        platform: platform.to_string(),
        mutation_mode: mutation_mode.to_string(),
    })
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
    source_fingerprint: &str,
    allowed: &[String],
    worker_stopped: bool,
    change_proposed: bool,
    diff_digest: Option<String>,
    authority: Option<&CheckAuthority>,
    bound_authority_digest: &str,
) -> CandidateVerification {
    let spec_digest =
        RequiredCheckSpec::spec_digest(checks).unwrap_or_else(|_| "sha256:invalid".into());
    let record = read_promotion_record(retained).ok();
    let changed_paths = record
        .as_ref()
        .map(|record| {
            record
                .manifest
                .iter()
                .map(|entry| entry.path.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let out_of_scope = changed_paths
        .iter()
        .any(|path| !allowed.iter().any(|allowed| allowed == path));
    let patch_text = record
        .as_ref()
        .map(|record| String::from_utf8_lossy(&record.patch).into_owned())
        .unwrap_or_default();
    let diff_truncated = patch_text.len() > MAX_DIFF_BYTES;
    let mut bounded_diff = patch_text;
    if diff_truncated {
        let mut end = MAX_DIFF_BYTES;
        while !bounded_diff.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        bounded_diff.truncate(end);
        bounded_diff.push_str("\n...[diff truncated]\n");
    }
    let identity_ok = record.as_ref().is_some_and(|record| {
        record.base_revision == source_revision && !record.content_digest.is_empty()
    });
    let manifest_paths: BTreeSet<String> = record
        .as_ref()
        .map(|record| {
            record
                .manifest
                .iter()
                .map(|entry| entry.path.clone())
                .collect()
        })
        .unwrap_or_default();
    let patch_paths_match = record
        .as_ref()
        .is_some_and(|record| patch_path_set(&record.patch).ok().as_ref() == Some(&manifest_paths));
    let tree_matches = record.as_ref().is_some_and(|record| {
        prove_patch_reproduces_checked_tree(
            source,
            &record.base_revision,
            &record.patch,
            &record.manifest,
            retained,
        )
        .is_ok()
    });
    let authority_matches = authority.is_some_and(|item| {
        bound_authority_digest.is_empty() || item.authority_digest == bound_authority_digest
    });
    let recorded_authority_digest = if !bound_authority_digest.is_empty() {
        bound_authority_digest.to_string()
    } else {
        authority
            .map(|item| item.authority_digest.clone())
            .unwrap_or_default()
    };
    let blocked = !patch_paths_match || !tree_matches || !authority_matches;
    let results =
        if !worker_stopped || !change_proposed || !identity_ok || out_of_scope || blocked {
            checks
                .iter()
                .map(|check| RequiredCheckExecution {
                    check_id: check.check_id.clone(),
                    outcome: if !worker_stopped {
                        "incomplete"
                    } else if blocked {
                        "invalidated"
                    } else {
                        "skipped"
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
            execute_required_checks_with_authority(checks, retained, oracle, authority)
                .unwrap_or_else(|_| {
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
    let checks_ok = checks_passed(checks, &results);
    let apply_bundle_digest = record
        .as_ref()
        .and_then(|record| {
            apply_bundle_digest(
                record,
                work_id,
                attempt_id.as_deref().unwrap_or(""),
                source_fingerprint,
                allowed,
                &recorded_authority_digest,
                &spec_digest,
                diff_digest.as_deref().unwrap_or(""),
                run_id.as_deref().unwrap_or(""),
                &results,
                checks_ok,
                retained,
            )
            .ok()
        })
        .unwrap_or_default();
    let passed = identity_ok
        && !out_of_scope
        && patch_paths_match
        && tree_matches
        && !diff_truncated
        && authority_matches
        && !apply_bundle_digest.is_empty()
        && checks_passed(checks, &results);
    let (content_digest, files) = record
        .map(|record| (record.content_digest, record.files))
        .unwrap_or_else(|| ("sha256:missing".into(), Vec::new()));
    CandidateVerification {
        schema_version: CANDIDATE_VERIFICATION_SCHEMA,
        work_id: work_id.to_string(),
        run_id,
        attempt_id,
        source_revision: source_revision.to_string(),
        source_fingerprint: source_fingerprint.to_string(),
        content_digest,
        spec_digest,
        files,
        checks: results,
        worker_stopped,
        change_proposed: change_proposed && !out_of_scope && identity_ok,
        checks_passed: passed,
        invalidated: false,
        applied: false,
        diff_digest,
        changed_paths,
        bounded_diff,
        diff_truncated,
        reconciliation_required: false,
        check_profile_id: authority
            .map(|item| item.profile_id.clone())
            .unwrap_or_default(),
        check_profile_revision: authority.map(|item| item.profile_revision).unwrap_or(0),
        check_authority_digest: recorded_authority_digest,
        apply_bundle_digest,
        execution_envelope_digest: String::new(),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn apply_bundle_digest(
    record: &PromotionRecord,
    work_id: &str,
    attempt_id: &str,
    source_fingerprint: &str,
    allowed: &[String],
    authority_digest: &str,
    spec_digest: &str,
    diff_digest: &str,
    run_id: &str,
    checks: &[RequiredCheckExecution],
    checks_passed: bool,
    retained: &Path,
) -> Result<String, VerifiedChangeError> {
    let derived = crate::run_promotion::fingerprint_bytes(&record.base_revision, &record.patch);
    if derived != record.final_fingerprint {
        return Err(VerifiedChangeError::new(
            "the final fingerprint does not match the base and patch",
        ));
    }
    let manifest = crate::run_promotion::manifest_digest(&record.base_revision, &record.manifest);
    if record.content_digest != format!("sha256:{manifest}") {
        return Err(VerifiedChangeError::new(
            "the manifest digest does not match the candidate bundle",
        ));
    }
    let patch_sha = format!("{:x}", Sha256::digest(&record.patch));
    let mut allowed_files = allowed.to_vec();
    allowed_files.sort();
    let mut hasher = Sha256::new();
    hasher.update(b"grokptah-apply-bundle-v1\0");
    hasher.update(record.base_revision.as_bytes());
    hasher.update([0]);
    hasher.update(source_fingerprint.as_bytes());
    hasher.update([0]);
    hasher.update(work_id.as_bytes());
    hasher.update([0]);
    hasher.update(attempt_id.as_bytes());
    hasher.update([0]);
    hasher.update(authority_digest.as_bytes());
    hasher.update([0]);
    hasher.update(spec_digest.as_bytes());
    hasher.update([0]);
    hasher.update(diff_digest.as_bytes());
    hasher.update([0]);
    hasher.update(run_id.as_bytes());
    hasher.update([0]);
    hasher.update(patch_sha.as_bytes());
    hasher.update([0]);
    hasher.update(derived.as_bytes());
    hasher.update([0]);
    hasher.update(manifest.as_bytes());
    for path in &allowed_files {
        hasher.update(path.as_bytes());
        hasher.update([0]);
    }
    for entry in &record.manifest {
        hasher.update(entry.path.as_bytes());
        hasher.update([0]);
        hasher.update(entry.before_mode.as_bytes());
        hasher.update([0]);
        hasher.update(entry.after_mode.as_bytes());
        hasher.update([0]);
        hasher.update(entry.before_blob.as_bytes());
        hasher.update([0]);
        hasher.update(entry.after_blob.as_bytes());
        hasher.update([0]);
        hasher.update(entry.state.as_bytes());
        hasher.update([0]);
        hasher.update([u8::from(entry.symlink)]);
        hasher.update([u8::from(entry.untracked)]);
    }
    for file in &record.files {
        hasher.update(file.path.as_bytes());
        hasher.update([0]);
        hasher.update(file.digest.as_bytes());
        hasher.update([0]);
        hasher.update(file.kind.as_bytes());
        hasher.update([0]);
    }
    hasher.update([u8::from(checks_passed)]);
    for check in checks {
        hasher.update(check.check_id.as_bytes());
        hasher.update([0]);
        hasher.update(check.outcome.as_bytes());
        hasher.update([0]);
        hasher.update(check.exit_code.unwrap_or(-1).to_string().as_bytes());
        hasher.update([0]);
        hasher.update(check.output_digest.as_bytes());
        hasher.update([0]);
        hasher.update(check.spec_digest.as_bytes());
        hasher.update([0]);
    }
    for file in &record.files {
        let path = retained.join(&file.path);
        if file.digest == "absent" {
            if path.exists() {
                return Err(VerifiedChangeError::new(
                    "a deleted candidate path is still materialized",
                ));
            }
            hasher.update(b"absent");
            hasher.update([0]);
            continue;
        }
        let bytes = fs::read(&path)
            .map_err(|_| VerifiedChangeError::new("materialized candidate bytes are missing"))?;
        let actual = digest_bytes(&bytes);
        if actual != file.digest {
            return Err(VerifiedChangeError::new(
                "materialized candidate bytes do not match the manifest",
            ));
        }
        let mode = fs::symlink_metadata(&path)
            .map(|meta| meta.permissions().mode() & 0o777)
            .unwrap_or(0);
        hasher.update(file.path.as_bytes());
        hasher.update([0]);
        hasher.update(actual.as_bytes());
        hasher.update([0]);
        hasher.update(mode.to_string().as_bytes());
        hasher.update([0]);
    }
    Ok(digest_bytes(&hasher.finalize()))
}

pub fn derived_snapshot_fingerprint(snapshot_dir: &Path) -> Result<String, VerifiedChangeError> {
    let record = read_promotion_record(snapshot_dir)?;
    Ok(crate::run_promotion::fingerprint_bytes(
        &record.base_revision,
        &record.patch,
    ))
}

pub fn recompute_candidate_apply_bundle(
    snapshot_dir: &Path,
    verification: &CandidateVerification,
    allowed: &[String],
) -> Result<String, VerifiedChangeError> {
    let record = read_promotion_record(snapshot_dir)?;
    apply_bundle_digest(
        &record,
        &verification.work_id,
        verification.attempt_id.as_deref().unwrap_or(""),
        &verification.source_fingerprint,
        allowed,
        &verification.check_authority_digest,
        &verification.spec_digest,
        verification.diff_digest.as_deref().unwrap_or(""),
        verification.run_id.as_deref().unwrap_or(""),
        &verification.checks,
        verification.checks_passed,
        snapshot_dir,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    struct OperatorEnvGuard {
        home: Option<std::ffi::OsString>,
        path: Option<std::ffi::OsString>,
        token: Option<std::ffi::OsString>,
    }

    impl OperatorEnvGuard {
        fn install() -> Self {
            let guard = Self {
                home: std::env::var_os("HOME"),
                path: std::env::var_os("PATH"),
                token: std::env::var_os("GITHUB_TOKEN"),
            };
            std::env::set_var("HOME", "/operator/home-leak");
            std::env::set_var("PATH", "/leak-path-marker");
            std::env::set_var("GITHUB_TOKEN", "leak-token-marker");
            guard
        }
    }

    impl Drop for OperatorEnvGuard {
        fn drop(&mut self) {
            restore_env("HOME", &self.home);
            restore_env("PATH", &self.path);
            restore_env("GITHUB_TOKEN", &self.token);
        }
    }

    fn restore_env(key: &str, value: &Option<std::ffi::OsString>) {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    struct ReapSleep(&'static str);
    impl Drop for ReapSleep {
        fn drop(&mut self) {
            let _ = std::process::Command::new("/usr/bin/pkill")
                .args(["-f", self.0])
                .status();
        }
    }

    fn pgrep_empty(pattern: &str) -> bool {
        let listed = std::process::Command::new("/usr/bin/pgrep")
            .args(["-f", pattern])
            .output()
            .unwrap();
        listed.stdout.is_empty()
    }

    fn check_spec(check_id: &str, executable: &Path, timeout_ms: u64) -> RequiredCheckSpec {
        RequiredCheckSpec {
            check_id: check_id.into(),
            executable: executable.display().to_string(),
            args: Vec::new(),
            cwd: RequiredCheckCwd::Oracle,
            env: Vec::new(),
            timeout_ms,
            max_output_bytes: 256,
        }
    }

    fn sealed_authority(
        profile: &str,
        check: &RequiredCheckSpec,
        oracle: &Path,
        source: &Path,
        network: &str,
    ) -> CheckAuthority {
        let mut authority = CheckAuthority {
            profile_id: profile.into(),
            profile_revision: 1,
            executable_path: String::new(),
            executable_digest: file_digest(Path::new(&check.executable)).unwrap(),
            oracle_root: String::new(),
            oracle_digest: directory_digest(oracle).unwrap(),
            args: Vec::new(),
            cwd: String::new(),
            env: Vec::new(),
            timeout_ms: 0,
            max_output_bytes: 0,
            write_policy: CHECK_WRITE_POLICY.into(),
            network: network.into(),
            source_root: source.to_path_buf(),
            output_limit_bytes: 4096,
            confinement_backend: String::new(),
            confinement_revision: 0,
            work_id: "unit".into(),
            session_id: "unit".into(),
            workspace: source.display().to_string(),
            authority_digest: String::new(),
        };
        authority.bind_invocation(check, oracle);
        authority.seal()
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
    fn timed_out_check_kills_the_background_process_group() {
        let _reap = ReapSleep("sleep 47");
        let oracle = temp();
        let candidate = temp();
        let source = temp();
        let script = oracle.path().join("linger.sh");
        fs::write(&script, "#!/bin/sh\nsleep 47 &\nsleep 47\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let check = RequiredCheckSpec {
            check_id: "linger".into(),
            executable: script.display().to_string(),
            args: Vec::new(),
            cwd: RequiredCheckCwd::Oracle,
            env: Vec::new(),
            timeout_ms: 500,
            max_output_bytes: 256,
        };
        let authority = sealed_authority("linger", &check, oracle.path(), source.path(), "none");
        let started = Instant::now();
        let results = execute_required_checks_with_authority(
            &[check],
            candidate.path(),
            oracle.path(),
            Some(&authority),
        )
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the check waited for the background sleep"
        );
        assert_eq!(results[0].outcome, "timed_out");
        assert!(
            pgrep_empty("sleep 47"),
            "background sleep survived the timed-out check"
        );
    }

    #[test]
    fn replaced_executable_or_oracle_invalidates_without_execution() {
        replaced_executable_or_oracle_invalidates_without_running();
    }

    #[test]
    fn replaced_executable_or_oracle_invalidates_without_running() {
        let oracle = temp();
        let bin = temp();
        let candidate = temp();
        let source = temp();
        fs::write(oracle.path().join("fixture.txt"), b"oracle-v1\n").unwrap();
        let script = bin.path().join("check.sh");
        fs::write(&script, "#!/bin/sh\nexit 7\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let check = check_spec("swapbin", &script, 1000);
        let mut authority =
            sealed_authority("swapbin", &check, oracle.path(), source.path(), "none");
        fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        let replaced = execute_required_checks_with_authority(
            std::slice::from_ref(&check),
            candidate.path(),
            oracle.path(),
            Some(&authority),
        )
        .unwrap();
        assert_eq!(replaced[0].outcome, "invalidated");
        assert_eq!(replaced[0].exit_code, None);

        authority.executable_digest = file_digest(&script).unwrap();
        authority = authority.seal();
        fs::write(oracle.path().join("fixture.txt"), b"oracle-v2\n").unwrap();
        let oracle_replaced = execute_required_checks_with_authority(
            std::slice::from_ref(&check),
            candidate.path(),
            oracle.path(),
            Some(&authority),
        )
        .unwrap();
        assert_eq!(oracle_replaced[0].outcome, "invalidated");
        assert_eq!(oracle_replaced[0].exit_code, None);
    }

    #[test]
    fn check_cannot_write_source_or_candidate_or_inherit_operator_env() {
        let _env = OperatorEnvGuard::install();
        let oracle = temp();
        let candidate = temp();
        let source = temp();
        fs::write(candidate.path().join("kept.txt"), b"kept\n").unwrap();
        fs::write(source.path().join("kept.txt"), b"kept\n").unwrap();
        let script = oracle.path().join("write.sh");
        fs::write(
            &script,
            "#!/bin/sh\nif [ \"$HOME\" = \"/operator/home-leak\" ]; then exit 4; fi\nif [ \"$GITHUB_TOKEN\" = \"leak-token-marker\" ]; then exit 4; fi\ncase \"$PATH\" in *leak-path-marker*) exit 4;; esac\nif printf poisoned > \"$SOURCE_ROOT/poisoned.txt\" 2>/dev/null; then exit 3; fi\nif printf poisoned > \"$CANDIDATE_ROOT/poisoned.txt\" 2>/dev/null; then exit 3; fi\nexit 0\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let mut check = check_spec("contain", &script, 2000);
        check.max_output_bytes = 4096;
        let authority = sealed_authority("contain", &check, oracle.path(), source.path(), "none");
        let results = execute_required_checks_with_authority(
            std::slice::from_ref(&check),
            candidate.path(),
            oracle.path(),
            Some(&authority),
        )
        .unwrap();
        assert_eq!(results[0].outcome, "passed", "{results:?}");
        assert!(!source.path().join("poisoned.txt").exists());
        assert!(!candidate.path().join("poisoned.txt").exists());
        assert_eq!(fs::read(source.path().join("kept.txt")).unwrap(), b"kept\n");
        assert_eq!(
            fs::read(candidate.path().join("kept.txt")).unwrap(),
            b"kept\n"
        );
    }

    #[test]
    fn forbidden_network_is_denied_and_qualified_network_can_connect() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let oracle = temp();
        let candidate = temp();
        let source = temp();
        let script = oracle.path().join("net.sh");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\n/usr/bin/nc -z -G 2 127.0.0.1 {port}\nif [ $? -eq 0 ]; then exit 2; fi\nexit 0\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let check = check_spec("netdeny", &script, 3000);
        let denied = sealed_authority("netdeny", &check, oracle.path(), source.path(), "none");
        let blocked = execute_required_checks_with_authority(
            std::slice::from_ref(&check),
            candidate.path(),
            oracle.path(),
            Some(&denied),
        )
        .unwrap();
        assert_eq!(
            blocked[0].outcome, "passed",
            "sandbox allowed the connection"
        );
        let allowed =
            sealed_authority("netdeny", &check, oracle.path(), source.path(), "qualified");
        let opened = execute_required_checks_with_authority(
            std::slice::from_ref(&check),
            candidate.path(),
            oracle.path(),
            Some(&allowed),
        )
        .unwrap();
        assert_eq!(
            opened[0].outcome, "failed",
            "qualified network could not connect"
        );
        assert_eq!(opened[0].exit_code, Some(2));
        drop(listener);
    }

    #[test]
    fn qualified_network_cannot_change_source_bytes_or_candidate_mode() {
        let oracle = temp();
        let candidate = temp();
        let source = temp();
        let kept = candidate.path().join("kept.txt");
        fs::write(&kept, b"kept\n").unwrap();
        fs::set_permissions(&kept, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(source.path().join("kept.txt"), b"source\n").unwrap();
        let script = oracle.path().join("mutate.sh");
        fs::write(
            &script,
            "#!/bin/sh\nprintf poisoned > \"$SOURCE_ROOT/poisoned.txt\" 2>/dev/null\nchmod 755 \"$CANDIDATE_ROOT/kept.txt\" 2>/dev/null\nexit 0\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let mut check = check_spec("qualified", &script, 2000);
        check.max_output_bytes = 4096;
        let authority = sealed_authority(
            "qualified",
            &check,
            oracle.path(),
            source.path(),
            "qualified",
        );
        let results = execute_required_checks_with_authority(
            std::slice::from_ref(&check),
            candidate.path(),
            oracle.path(),
            Some(&authority),
        )
        .unwrap();
        assert_eq!(results[0].outcome, "passed", "{results:?}");
        assert!(!source.path().join("poisoned.txt").exists());
        assert_eq!(
            fs::read(source.path().join("kept.txt")).unwrap(),
            b"source\n"
        );
        assert_eq!(
            fs::symlink_metadata(&kept).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(fs::read(&kept).unwrap(), b"kept\n");
    }

    struct ConfinementGuard;
    impl Drop for ConfinementGuard {
        fn drop(&mut self) {
            *CHECK_CONFINEMENT_EXECUTABLE.lock().unwrap() = None;
        }
    }

    #[test]
    fn missing_check_authority_runs_no_check_and_cannot_verify() {
        let oracle = temp();
        let candidate = temp();
        let marker = candidate.path().join("ran.txt");
        let script = oracle.path().join("run.sh");
        fs::write(
            &script,
            "#!/bin/sh\nprintf ran > \"$CANDIDATE_ROOT/ran.txt\"\nexit 0\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let check = check_spec("missing-auth", &script, 2000);
        let results = execute_required_checks_with_authority(
            std::slice::from_ref(&check),
            candidate.path(),
            oracle.path(),
            None,
        )
        .unwrap();
        assert_eq!(results[0].outcome, "invalidated");
        assert!(results[0].exit_code.is_none());
        assert!(!marker.exists());
        let verification = CandidateVerification {
            schema_version: 1,
            work_id: "work".into(),
            run_id: None,
            attempt_id: None,
            source_revision: "a".repeat(40),
            source_fingerprint: "fp".into(),
            content_digest: "sha256:abc".into(),
            spec_digest: "sha256:spec".into(),
            files: Vec::new(),
            checks: results,
            worker_stopped: true,
            change_proposed: true,
            checks_passed: false,
            invalidated: true,
            applied: true,
            diff_digest: None,
            changed_paths: Vec::new(),
            bounded_diff: String::new(),
            diff_truncated: false,
            reconciliation_required: false,
            check_profile_id: String::new(),
            check_profile_revision: 0,
            check_authority_digest: String::new(),
            apply_bundle_digest: String::new(),
            execution_envelope_digest: String::new(),
        };
        assert!(!verification.authorizes_applied_success(
            Some("sha256:abc"),
            Some(&"a".repeat(40)),
            "balance",
            1,
            "sha256:missing",
            None,
        ));
    }

    #[test]
    fn malformed_check_authority_runs_no_check_and_cannot_verify() {
        let dir = temp();
        fs::write(dir.path().join("check-authority.json"), b"{not json").unwrap();
        assert!(read_check_authority(dir.path()).is_err());
        let oracle = temp();
        let candidate = temp();
        let script = oracle.path().join("run.sh");
        fs::write(
            &script,
            "#!/bin/sh\nprintf ran > \"$CANDIDATE_ROOT/ran.txt\"\nexit 0\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let mut authority = sealed_authority(
            "bad",
            &check_spec("bad", &script, 2000),
            oracle.path(),
            candidate.path(),
            "none",
        );
        authority.authority_digest = "sha256:tampered".into();
        let check = check_spec("bad", &script, 2000);
        let results = execute_required_checks_with_authority(
            std::slice::from_ref(&check),
            candidate.path(),
            oracle.path(),
            Some(&authority),
        )
        .unwrap();
        assert_eq!(results[0].outcome, "incomplete");
        assert!(!candidate.path().join("ran.txt").exists());
    }

    #[test]
    fn tampered_profile_revision_or_network_policy_cannot_verify() {
        let oracle = temp();
        let candidate = temp();
        let script = oracle.path().join("run.sh");
        fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let mut authority = sealed_authority(
            "rev",
            &check_spec("rev", &script, 2000),
            oracle.path(),
            candidate.path(),
            "none",
        );
        authority.profile_revision = 9;
        authority.network = "qualified".into();
        let check = check_spec("rev", &script, 2000);
        let results = execute_required_checks_with_authority(
            std::slice::from_ref(&check),
            candidate.path(),
            oracle.path(),
            Some(&authority),
        )
        .unwrap();
        assert_eq!(results[0].outcome, "incomplete");
        assert!(results[0].exit_code.is_none());
        let verification = CandidateVerification {
            schema_version: 1,
            work_id: "work".into(),
            run_id: None,
            attempt_id: None,
            source_revision: "b".repeat(40),
            source_fingerprint: "fp".into(),
            content_digest: "sha256:abc".into(),
            spec_digest: "sha256:spec".into(),
            files: Vec::new(),
            checks: results,
            worker_stopped: true,
            change_proposed: true,
            checks_passed: true,
            invalidated: false,
            applied: true,
            diff_digest: None,
            changed_paths: Vec::new(),
            bounded_diff: String::new(),
            diff_truncated: false,
            reconciliation_required: false,
            check_profile_id: "rev".into(),
            check_profile_revision: 1,
            check_authority_digest: authority.canonical_digest(),
            apply_bundle_digest: "sha256:bundle".into(),
            execution_envelope_digest: String::new(),
        };
        assert!(!verification.authorizes_applied_success(
            Some("sha256:abc"),
            Some(&"b".repeat(40)),
            "rev",
            9,
            &authority.canonical_digest(),
            None,
        ));
    }

    #[test]
    fn missing_check_sandbox_runs_no_process() {
        let _guard = ConfinementGuard;
        *CHECK_CONFINEMENT_EXECUTABLE.lock().unwrap() =
            Some(PathBuf::from("/no/such/sandbox-exec"));
        let oracle = temp();
        let candidate = temp();
        let script = oracle.path().join("run.sh");
        fs::write(
            &script,
            "#!/bin/sh\nprintf ran > \"$CANDIDATE_ROOT/ran.txt\"\nexit 0\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let authority = sealed_authority(
            "box",
            &check_spec("box", &script, 2000),
            oracle.path(),
            candidate.path(),
            "none",
        );
        let check = check_spec("box", &script, 2000);
        let results = execute_required_checks_with_authority(
            std::slice::from_ref(&check),
            candidate.path(),
            oracle.path(),
            Some(&authority),
        )
        .unwrap();
        assert_eq!(results[0].outcome, "incomplete");
        assert!(!candidate.path().join("ran.txt").exists());
    }

    #[test]
    fn sandbox_launch_failure_cannot_fall_back_unsandboxed() {
        let _guard = ConfinementGuard;
        *CHECK_CONFINEMENT_EXECUTABLE.lock().unwrap() = Some(PathBuf::from("/dev/null"));
        let oracle = temp();
        let candidate = temp();
        let script = oracle.path().join("run.sh");
        fs::write(
            &script,
            "#!/bin/sh\nprintf ran > \"$CANDIDATE_ROOT/ran.txt\"\nexit 0\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let authority = sealed_authority(
            "launch",
            &check_spec("launch", &script, 2000),
            oracle.path(),
            candidate.path(),
            "none",
        );
        let check = check_spec("launch", &script, 2000);
        let results = execute_required_checks_with_authority(
            std::slice::from_ref(&check),
            candidate.path(),
            oracle.path(),
            Some(&authority),
        )
        .unwrap();
        assert_ne!(results[0].outcome, "passed");
        assert!(!candidate.path().join("ran.txt").exists());
    }

    #[test]
    fn rewritten_check_argv_cwd_env_or_timeout_runs_no_process() {
        let oracle = temp();
        let candidate = temp();
        let script = oracle.path().join("run.sh");
        fs::write(
            &script,
            "#!/bin/sh\nprintf ran > \"$CANDIDATE_ROOT/ran.txt\"\nexit 0\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let check = check_spec("argv", &script, 2000);
        let authority = sealed_authority("argv", &check, oracle.path(), candidate.path(), "none");
        let mut rewritten = check.clone();
        rewritten.args = vec!["--rewrite".into()];
        rewritten.cwd = RequiredCheckCwd::Candidate;
        rewritten.env = vec![RequiredCheckEnv {
            key: "CHECK_MODE".into(),
            value: "rewritten".into(),
        }];
        rewritten.timeout_ms = 1500;
        let results = execute_required_checks_with_authority(
            std::slice::from_ref(&rewritten),
            candidate.path(),
            oracle.path(),
            Some(&authority),
        )
        .unwrap();
        assert_eq!(results[0].outcome, "invalidated");
        assert_eq!(results[0].exit_code, None);
        assert!(!candidate.path().join("ran.txt").exists());
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

    #[test]
    fn retained_tree_byte_mismatch_cannot_bind_adapter_evidence() {
        let dir = temp();
        fs::write(dir.path().join("README.md"), b"before\n").unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("/usr/bin/git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}");
        };
        git(&["init", "-q", "-b", "topic"]);
        git(&["config", "user.email", "test@grokptah.invalid"]);
        git(&["config", "user.name", "GrokPtah test"]);
        git(&["add", "README.md"]);
        git(&["commit", "-qm", "base"]);
        let base = Command::new("/usr/bin/git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let base = String::from_utf8(base.stdout).unwrap().trim().to_string();
        fs::write(dir.path().join("README.md"), b"after\n").unwrap();
        let retained = temp();
        let tree = retained.path().join("tree");
        retain_candidate_snapshot(dir.path(), &tree, &base).unwrap();
        fs::write(dir.path().join("README.md"), b"after-mutated\n").unwrap();
        let error = bind_retained_candidate(dir.path(), &tree).unwrap_err();
        assert!(error.message.contains("bytes"), "{error:?}");
        fs::write(dir.path().join("README.md"), b"after\n").unwrap();
        let binding = bind_retained_candidate(dir.path(), &tree).unwrap();
        assert_eq!(
            binding.diff_digest,
            retained_candidate_diff_digest(&tree).unwrap()
        );
        assert_eq!(binding.changed_paths, vec!["README.md".to_string()]);
        let mut kept = fs::read(tree.join("README.md")).unwrap();
        kept.push(b'z');
        fs::write(tree.join("README.md"), &kept).unwrap();
        assert_ne!(
            retained_candidate_diff_digest(&tree).unwrap(),
            binding.diff_digest
        );
    }

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn reseal_work(authority: CheckAuthority, work_id: &str) -> CheckAuthority {
        let mut authority = authority;
        authority.work_id = work_id.into();
        authority.seal()
    }

    #[test]
    fn successful_parent_with_background_pipe_holder_obeys_timeout() {
        let _reap = ReapSleep("sleep 39");
        let oracle = temp();
        let candidate = temp();
        let source = temp();
        let executable = script(
            oracle.path(),
            "pipe.sh",
            "#!/bin/sh\nsleep 39 >&1 &\nexit 0\n",
        );
        let check = check_spec("pipe-holder", &executable, 5_000);
        let authority = sealed_authority("pipe", &check, oracle.path(), source.path(), "none");
        let started = Instant::now();
        let results = execute_required_checks_with_authority(
            std::slice::from_ref(&check),
            candidate.path(),
            oracle.path(),
            Some(&authority),
        )
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(2500),
            "a pipe-holding descendant blocked the check past its deadline"
        );
        assert_ne!(results[0].outcome, "timed_out");
        assert!(
            results[0].outcome == "passed" || results[0].outcome == "unterminated",
            "{results:?}"
        );
        if results[0].outcome == "passed" {
            assert!(
                pgrep_empty("sleep 39"),
                "passed while the pipe holder lived"
            );
        }
        assert!(pgrep_empty("sleep 39"));
    }

    #[test]
    fn normal_check_exit_requires_process_group_quiescence() {
        let _reap = ReapSleep("sleep 41");
        let oracle = temp();
        let candidate = temp();
        let source = temp();
        let executable = script(
            oracle.path(),
            "quiet.sh",
            "#!/bin/sh\nsleep 41 >/dev/null 2>&1 &\nexit 0\n",
        );
        let check = check_spec("quiesce", &executable, 4_000);
        let authority = sealed_authority("quiesce", &check, oracle.path(), source.path(), "none");
        let started = Instant::now();
        let results = execute_required_checks_with_authority(
            std::slice::from_ref(&check),
            candidate.path(),
            oracle.path(),
            Some(&authority),
        )
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "normal exit waited for a surviving descendant"
        );
        assert!(
            results[0].outcome == "passed" || results[0].outcome == "unterminated",
            "{results:?}"
        );
        assert!(
            pgrep_empty("sleep 41"),
            "the check returned while its process group was still alive"
        );
        if results[0].outcome == "passed" {
            assert_eq!(results[0].exit_code, Some(0));
        } else {
            assert!(results[0].exit_code.is_none());
        }
    }

    #[test]
    fn concurrent_checks_have_distinct_private_output_directories() {
        let oracle = temp();
        let candidate = temp();
        let source = temp();
        let executable = script(
            oracle.path(),
            "wait.sh",
            "#!/bin/sh\nprintf '%s\\n' \"$CHECK_OUTPUT\" > \"$CHECK_OUTPUT/path\"\ni=0\nwhile [ ! -f \"$CHECK_OUTPUT/go\" ]; do\n  i=$((i+1))\n  if [ \"$i\" -gt 300 ]; then exit 2; fi\n  sleep 0.05\ndone\nexit 0\n",
        );
        let left_check = check_spec("left-check", &executable, 8_000);
        let right_check = check_spec("right-check", &executable, 8_000);
        let left = reseal_work(
            sealed_authority("dirs", &left_check, oracle.path(), source.path(), "none"),
            "work-left",
        );
        let right = reseal_work(
            sealed_authority("dirs", &right_check, oracle.path(), source.path(), "none"),
            "work-right",
        );
        let left_root = candidate.path().to_path_buf();
        let right_root = candidate.path().to_path_buf();
        let oracle_root = oracle.path().to_path_buf();
        let left_thread = thread::spawn(move || {
            execute_required_checks_with_authority(
                std::slice::from_ref(&left_check),
                &left_root,
                &oracle_root,
                Some(&left),
            )
            .unwrap()
        });
        let oracle_root = oracle.path().to_path_buf();
        let right_thread = thread::spawn(move || {
            execute_required_checks_with_authority(
                std::slice::from_ref(&right_check),
                &right_root,
                &oracle_root,
                Some(&right),
            )
            .unwrap()
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut found = Vec::new();
        while Instant::now() < deadline && found.len() < 2 {
            found = fs::read_dir(std::env::temp_dir())
                .unwrap()
                .flatten()
                .filter_map(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if (name.starts_with("grokptah-check-work-left")
                        || name.starts_with("grokptah-check-work-right"))
                        && entry.path().join("path").is_file()
                    {
                        Some(entry.path())
                    } else {
                        None
                    }
                })
                .collect();
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            found.len(),
            2,
            "expected two private check directories: {found:?}"
        );
        assert_ne!(found[0], found[1]);
        for dir in &found {
            let mode = fs::metadata(dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{dir:?}");
            let written = fs::read_to_string(dir.join("path")).unwrap();
            let name = dir.file_name().unwrap().to_string_lossy();
            assert!(
                written.trim().ends_with(name.as_ref()),
                "the check did not receive its private output directory: {written}"
            );
            fs::write(dir.join("go"), b"1").unwrap();
        }
        let left_results = left_thread.join().unwrap();
        let right_results = right_thread.join().unwrap();
        assert_eq!(left_results[0].outcome, "passed");
        assert_eq!(right_results[0].outcome, "passed");
        for dir in &found {
            assert!(!dir.exists(), "check output directory was reused or leaked");
        }
    }

    #[test]
    fn output_directory_limit_terminates_the_check_tree() {
        let _reap = ReapSleep("sleep 44");
        let oracle = temp();
        let candidate = temp();
        let source = temp();
        let executable = script(
            oracle.path(),
            "grow.sh",
            "#!/bin/sh\nsleep 44 >/dev/null 2>&1 &\nwhile true; do printf '%s' '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' >> \"$CHECK_OUTPUT/blob\" || exit 1; done\n",
        );
        let check = check_spec("grow", &executable, 8_000);
        let mut authority = sealed_authority("grow", &check, oracle.path(), source.path(), "none");
        authority.output_limit_bytes = 1024;
        let authority = authority.seal();
        let started = Instant::now();
        let results = execute_required_checks_with_authority(
            std::slice::from_ref(&check),
            candidate.path(),
            oracle.path(),
            Some(&authority),
        )
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "the output-directory limit did not stop the check"
        );
        assert_ne!(results[0].outcome, "passed", "{results:?}");
        assert!(
            results[0].outcome == "timed_out" || results[0].outcome == "truncated",
            "{results:?}"
        );
        assert!(results[0].output_truncated);
        assert!(pgrep_empty("sleep 44"));
    }
}
