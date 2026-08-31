//! Focused process-adapter tests. All child processes are fakes; no live
//! Grok/provider call is made.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use grokptah_agent_bridge::grok_build::{
    GrokBuildCleanupState, GrokBuildGitIdentity, GrokBuildLaunchRequest, GrokBuildMutationMode,
    GrokBuildPolicyState, GrokBuildRunState, GrokBuildVerdict,
};
use grokptah_agent_bridge::{
    launch_grok_build, CredentialLeaseHandle, CredentialLeaseResolver, GrokBuildAdapterError,
    GrokBuildHostLaunchConfig,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const GIT: &str = "/usr/bin/git";
const SECRET: &str = "sk-live-secret-not-real";
const LEAK_ENV: &str = "GROKPTAH_TEST_LEAK";

struct FakeResolver {
    path: PathBuf,
}

impl CredentialLeaseResolver for FakeResolver {
    fn resolve(&self, lease_id: &str) -> Result<CredentialLeaseHandle, GrokBuildAdapterError> {
        let _ = lease_id;
        Ok(CredentialLeaseHandle::from_host_path(self.path.clone()))
    }
}

struct Fixture {
    repo: tempfile::TempDir,
    isolate: tempfile::TempDir,
    fake_dir: tempfile::TempDir,
    identity: GrokBuildGitIdentity,
    lease_file: PathBuf,
    fake_bin: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let repo = tempfile::tempdir().expect("repo");
        let isolate = tempfile::tempdir().expect("isolate");
        let fake_dir = tempfile::tempdir().expect("fake");
        git(repo.path(), &["init", "-b", "topic"]).expect("init");
        fs::write(repo.path().join("README"), "base\n").expect("readme");
        git(repo.path(), &["add", "README"]).expect("add");
        git(repo.path(), &["commit", "-m", "base"]).expect("commit base");
        let base = git_stdout(repo.path(), &["rev-parse", "HEAD"]);
        fs::write(repo.path().join("README"), "head\n").expect("readme head");
        git(repo.path(), &["add", "README"]).expect("add head");
        git(repo.path(), &["commit", "-m", "head"]).expect("commit head");
        let head = git_stdout(repo.path(), &["rev-parse", "HEAD"]);
        git(repo.path(), &["branch", "base", &base]).expect("base branch");

        let fake_bin = fake_dir.path().join("grok-fake");
        fs::write(&fake_bin, FAKE_SCRIPT).expect("fake script");
        let mut perms = fs::metadata(&fake_bin).expect("meta").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake_bin, perms).expect("chmod");

        let lease_file = fake_dir.path().join("lease");
        fs::write(&lease_file, SECRET).expect("lease");
        let mut lease_perms = fs::metadata(&lease_file).expect("lease meta").permissions();
        lease_perms.set_mode(0o600);
        fs::set_permissions(&lease_file, lease_perms).expect("lease chmod");

        Self {
            repo,
            isolate,
            fake_dir,
            identity: GrokBuildGitIdentity {
                repository_id: "repo-acme".into(),
                git_ref: "refs/heads/topic".into(),
                base_sha: base,
                head_sha: head,
            },
            lease_file,
            fake_bin,
        }
    }

    fn set_behavior(&self, behavior: &str) {
        fs::write(self.fake_dir.path().join("behavior"), behavior).expect("behavior");
    }

    fn host(&self) -> GrokBuildHostLaunchConfig {
        GrokBuildHostLaunchConfig {
            executable: self.fake_bin.clone(),
            git_executable: PathBuf::from(GIT),
            cwd: self.repo.path().to_path_buf(),
            repository_id: "repo-acme".into(),
            base_ref: "base".into(),
            prompt: "review the isolated tree".into(),
            max_stdout_bytes: 8_192,
            max_stderr_bytes: 8_192,
            git_timeout: Duration::from_secs(5),
            isolate_parent: self.isolate.path().to_path_buf(),
        }
    }

    fn launch(&self, mode: GrokBuildMutationMode, duration_ms: u64) -> GrokBuildLaunchRequest {
        let request = GrokBuildLaunchRequest {
            request_id: "req-1".into(),
            identity: self.identity.clone(),
            mutation_mode: mode,
            max_prompt_bytes: 4096,
            max_turns: 8,
            max_duration_ms: duration_ms,
            credential_lease_id: "lease-1".into(),
        };
        request.validate().expect("valid launch");
        request
    }

    fn resolver(&self) -> FakeResolver {
        FakeResolver {
            path: self.lease_file.clone(),
        }
    }

    fn captured_argv(&self) -> Vec<String> {
        fs::read_to_string(self.fake_dir.path().join("captured-argv"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .filter(|l| !l.is_empty())
            .collect()
    }

    fn captured_env(&self) -> Vec<(String, String)> {
        fs::read_to_string(self.fake_dir.path().join("captured-env"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn captured_grok_home(&self) -> PathBuf {
        PathBuf::from(
            fs::read_to_string(self.fake_dir.path().join("captured-grok-home"))
                .unwrap_or_default()
                .trim(),
        )
    }

    fn isolate_children(&self) -> Vec<PathBuf> {
        fs::read_dir(self.isolate.path())
            .map(|entries| entries.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default()
    }
}

fn git(repo: &Path, args: &[&str]) -> std::io::Result<std::process::ExitStatus> {
    git_cmd(repo).args(args).status()
}

fn git_stdout(repo: &Path, args: &[&str]) -> String {
    let out = git_cmd(repo).args(args).output().expect("git stdout");
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8(out.stdout)
        .expect("utf8")
        .trim()
        .to_string()
}

fn git_cmd(repo: &Path) -> Command {
    let mut cmd = Command::new(GIT);
    cmd.current_dir(repo);
    cmd.env_clear();
    cmd.env("PATH", "/usr/bin:/bin");
    cmd.env("GIT_CONFIG_NOSYSTEM", "1");
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
    cmd.env("GIT_CONFIG_SYSTEM", "/dev/null");
    cmd.env("GIT_AUTHOR_NAME", "test");
    cmd.env("GIT_AUTHOR_EMAIL", "test@example.com");
    cmd.env("GIT_COMMITTER_NAME", "test");
    cmd.env("GIT_COMMITTER_EMAIL", "test@example.com");
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.args([
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "init.defaultBranch=topic",
        "-c",
        "advice.defaultBranchName=false",
    ]);
    cmd
}

fn assert_no_secret(rendered: &str) {
    let lower = rendered.to_ascii_lowercase();
    assert!(!rendered.contains(SECRET), "secret leaked");
    assert!(!lower.contains("sk-live"));
    assert!(!rendered.contains("/private/"));
    assert!(!rendered.contains("/var/folders/"));
    assert!(!rendered.contains("auth.json"));
}

const FAKE_SCRIPT: &str = r#"#!/bin/sh
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
behavior=complete
if [ -f "$dir/behavior" ]; then
  behavior=$(cat "$dir/behavior")
fi
if [ "$1" = "inspect" ] && [ "$2" = "--json" ]; then
  printf '%s\n' "$@" > "$dir/captured-inspect-argv"
  env | sort > "$dir/captured-inspect-env"
  if [ "$behavior" = "inspect-dirty" ]; then
    printf '%s\n' '{"projectInstructions":[{"path":"CLAUDE.md"}],"hooks":[],"plugins":[],"mcpServers":[],"lspServers":[]}'
    exit 0
  fi
  printf '%s\n' '{"projectInstructions":[],"hooks":[],"plugins":[],"mcpServers":[],"lspServers":[]}'
  exit 0
fi
printf '%s\n' "$@" > "$dir/captured-argv"
env | sort > "$dir/captured-env"
printf '%s\n' "$GROK_HOME" > "$dir/captured-grok-home"
if [ -n "$GROK_HOME" ] && [ -f "$GROK_HOME/config.toml" ]; then
  cp "$GROK_HOME/config.toml" "$dir/captured-config.toml"
fi
if [ -n "$GROK_HOME" ] && [ -f "$GROK_HOME/auth.json" ]; then
  printf 'present\n' > "$dir/auth-present"
fi
case "$behavior" in
  complete)
    printf '%s\n' 'GROK_BUILD_VERDICT=clean'
    exit 0
    ;;
  max-turns)
    printf '%s\n' 'max_turns_reached'
    exit 1
    ;;
  partial)
    printf '%s\n' 'partial output without verdict'
    exit 0
    ;;
  overflow)
    dd if=/dev/zero bs=1024 count=64 2>/dev/null
    exit 0
    ;;
  sleep)
    echo $$ > "$dir/pid"
    /bin/sleep 30
    exit 0
    ;;
  mutate)
    printf 'mutated\n' > mutated-by-child.txt
    printf '%s\n' 'GROK_BUILD_VERDICT=clean'
    exit 0
    ;;
  leak-secret)
    if [ -f "$GROK_HOME/auth.json" ]; then
      cat "$GROK_HOME/auth.json"
      printf '\n'
    fi
    printf '%s\n' 'GROK_BUILD_VERDICT=clean'
    exit 0
    ;;
esac
exit 0
"#;

#[tokio::test]
async fn exact_command_argv_and_env() {
    let fx = Fixture::new();
    fx.set_behavior("complete");
    let prev_key = std::env::var_os("XAI_API_KEY");
    let prev_leak = std::env::var_os(LEAK_ENV);
    unsafe {
        std::env::set_var("XAI_API_KEY", SECRET);
        std::env::set_var(LEAK_ENV, SECRET);
    }
    let launch = fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000);
    let outcome = launch_grok_build(
        &launch,
        &fx.host(),
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect("launch");
    unsafe {
        match prev_key {
            Some(v) => std::env::set_var("XAI_API_KEY", v),
            None => std::env::remove_var("XAI_API_KEY"),
        }
        match prev_leak {
            Some(v) => std::env::set_var(LEAK_ENV, v),
            None => std::env::remove_var(LEAK_ENV),
        }
    }

    let argv = fx.captured_argv();
    assert_eq!(argv[0], "--prompt-file");
    assert!(argv[1].ends_with("/prompt"), "{argv:?}");
    assert_eq!(
        &argv[2..8],
        [
            "--permission-mode",
            "acceptEdits",
            "--disable-web-search",
            "--no-subagents",
            "--max-turns",
            "8"
        ]
    );
    assert_eq!(argv[8], "--session-id");
    assert!(Uuid::parse_str(&argv[9]).is_ok(), "{argv:?}");
    assert_eq!(&argv[10..], ["--output-format", "plain"]);
    assert!(!argv
        .iter()
        .any(|a| a.contains("yolo") || a.contains("--model")));

    let env = fx.captured_env();
    let grok_home = env
        .iter()
        .find(|(k, _)| k == "GROK_HOME")
        .map(|(_, v)| v.as_str())
        .expect("GROK_HOME");
    let home = env
        .iter()
        .find(|(k, _)| k == "HOME")
        .map(|(_, v)| v.as_str())
        .expect("HOME");
    assert_eq!(grok_home, home);
    assert_eq!(
        env.iter()
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| v.as_str()),
        Some("/usr/bin:/bin:/usr/local/bin")
    );
    assert!(!env.iter().any(|(k, v)| k == "XAI_API_KEY"
        || k == LEAK_ENV
        || k.contains("TOKEN")
        || k.contains("SECRET")
        || k.contains("CLAUDE")
        || v.contains(SECRET)));

    let config =
        fs::read_to_string(fx.fake_dir.path().join("captured-config.toml")).expect("config");
    assert_eq!(
        config,
        "[compat.claude]\nskills = false\nrules = false\nagents = false\nmcps = false\nhooks = false\nsessions = false\n\n[compat.cursor]\nskills = false\nrules = false\nagents = false\nmcps = false\nhooks = false\nsessions = false\n"
    );
    assert_eq!(
        fs::read_to_string(fx.fake_dir.path().join("auth-present")).expect("auth"),
        "present\n"
    );
    assert_eq!(outcome.result().state, GrokBuildRunState::CompleteAdvisory);
    assert_eq!(outcome.result().session_id, argv[9]);
}

#[tokio::test]
async fn identity_mismatch_is_rejected() {
    let fx = Fixture::new();
    let mut launch = fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000);
    launch.identity.head_sha = "cccccccccccccccccccccccccccccccccccccccc".into();
    let err = launch_grok_build(
        &launch,
        &fx.host(),
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect_err("mismatch");
    assert_eq!(err, GrokBuildAdapterError::IdentityMismatch);
    assert!(fx.isolate_children().is_empty());
    assert!(!fx.fake_dir.path().join("captured-argv").exists());
}

#[tokio::test]
async fn discovered_instruction_or_plugin_surface_fails_before_task_launch() {
    let fx = Fixture::new();
    fx.set_behavior("inspect-dirty");
    let err = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
        &fx.host(),
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect_err("discovered surface");
    assert_eq!(err, GrokBuildAdapterError::IsolationFailed);
    assert!(fx.fake_dir.path().join("captured-inspect-argv").exists());
    assert!(!fx.fake_dir.path().join("captured-argv").exists());
    assert!(fx.isolate_children().is_empty());
}

#[tokio::test]
async fn dirty_tree_is_rejected() {
    let fx = Fixture::new();
    fs::write(fx.repo.path().join("unapproved.txt"), "nope\n").expect("dirty");
    let err = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
        &fx.host(),
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect_err("dirty");
    assert_eq!(err, GrokBuildAdapterError::DirtyTree);
    assert!(fx.isolate_children().is_empty());
}

#[tokio::test]
async fn output_overflow_fails_closed_and_kills() {
    let fx = Fixture::new();
    fx.set_behavior("overflow");
    let mut host = fx.host();
    host.max_stdout_bytes = 64;
    let outcome = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
        &host,
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect("overflow outcome");
    assert_eq!(outcome.result().state, GrokBuildRunState::FailedClosed);
    assert!(outcome.receipt().permissions_ok);
    assert_eq!(
        outcome.receipt().cleanup_state,
        GrokBuildCleanupState::FailedClosed
    );
    assert!(fx.isolate_children().is_empty());
}

#[tokio::test]
async fn timeout_kills_process_tree_and_cleans_home() {
    let fx = Fixture::new();
    fx.set_behavior("sleep");
    let outcome = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::IsolatedReview, 400),
        &fx.host(),
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect("timeout outcome");
    assert_eq!(outcome.result().state, GrokBuildRunState::FailedClosed);
    assert!(fx.isolate_children().is_empty());
    if let Ok(pid) = fs::read_to_string(fx.fake_dir.path().join("pid")) {
        let pid = pid.trim();
        let alive = Command::new("/bin/kill")
            .args(["-0", pid])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(!alive, "child still alive after timeout");
    }
}

#[tokio::test]
async fn spawn_failure_cleans_isolated_home() {
    let fx = Fixture::new();
    let mut host = fx.host();
    host.executable = fx.isolate.path().join("missing-bin");
    let err = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
        &host,
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect_err("spawn");
    assert_eq!(err, GrokBuildAdapterError::SpawnFailed);
    assert!(fx.isolate_children().is_empty());
}

#[tokio::test]
async fn permissive_or_symlinked_credential_sources_are_rejected() {
    let fx = Fixture::new();
    let mut perms = fs::metadata(&fx.lease_file)
        .expect("lease meta")
        .permissions();
    perms.set_mode(0o644);
    fs::set_permissions(&fx.lease_file, perms).expect("chmod permissive");
    let err = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
        &fx.host(),
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect_err("permissive source");
    assert_eq!(err, GrokBuildAdapterError::CredentialLease);
    assert!(fx.isolate_children().is_empty());

    let target = fx.fake_dir.path().join("private-target");
    fs::write(&target, SECRET).expect("target");
    let mut target_perms = fs::metadata(&target).expect("target meta").permissions();
    target_perms.set_mode(0o600);
    fs::set_permissions(&target, target_perms).expect("target chmod");
    let link = fx.fake_dir.path().join("lease-link");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");
    let resolver = FakeResolver { path: link };
    let err = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
        &fx.host(),
        &resolver,
        CancellationToken::new(),
    )
    .await
    .expect_err("symlink source");
    assert_eq!(err, GrokBuildAdapterError::CredentialLease);
    assert!(fx.isolate_children().is_empty());
}

#[tokio::test]
async fn secret_redaction_across_debug_and_public_result() {
    let fx = Fixture::new();
    fx.set_behavior("leak-secret");
    let outcome = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
        &fx.host(),
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect("launch");
    assert_no_secret(&format!("{outcome:?}"));
    assert_no_secret(&format!("{:?}", outcome.result()));
    assert_no_secret(&format!("{:?}", outcome.receipt()));
    assert_no_secret(&format!("{:?}", fx.host()));
    assert_no_secret(&format!("{:?}", fx.resolver().resolve("lease-1").unwrap()));
    assert!(!outcome
        .result()
        .evidence_refs
        .iter()
        .any(|m| m.contains(SECRET)));
    assert_eq!(outcome.result().state, GrokBuildRunState::CompleteAdvisory);
}

#[tokio::test]
async fn read_only_mutation_is_refused() {
    let fx = Fixture::new();
    fx.set_behavior("mutate");
    let outcome = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::ReadOnly, 60_000),
        &fx.host(),
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect("readonly");
    assert_eq!(outcome.result().state, GrokBuildRunState::FailedClosed);
    assert!(!outcome.receipt().permissions_ok);
    assert_eq!(
        outcome.receipt().permission_policy,
        GrokBuildMutationMode::ReadOnly
    );
    let argv = fx.captured_argv();
    assert!(argv.contains(&"plan".to_string()));
    assert!(!argv.contains(&"acceptEdits".to_string()));
}

#[tokio::test]
async fn partial_and_max_turn_need_synthesis_never_complete() {
    let fx = Fixture::new();
    fx.set_behavior("partial");
    let partial = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
        &fx.host(),
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect("partial");
    assert_eq!(partial.result().state, GrokBuildRunState::NeedsSynthesis);
    assert_eq!(partial.result().terminal_verdict, None);
    assert_eq!(
        partial.receipt().cleanup_state,
        GrokBuildCleanupState::Pending
    );

    fx.set_behavior("max-turns");
    let maxed = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
        &fx.host(),
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect("max-turns");
    assert_eq!(maxed.result().state, GrokBuildRunState::NeedsSynthesis);
    assert_eq!(maxed.result().terminal_verdict, None);
    assert_ne!(maxed.result().state, GrokBuildRunState::CompleteAdvisory);
}

#[tokio::test]
async fn valid_complete_advisory_result() {
    let fx = Fixture::new();
    fx.set_behavior("complete");
    let launch = fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000);
    let outcome = launch_grok_build(
        &launch,
        &fx.host(),
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect("complete");
    let result = outcome.result();
    let receipt = outcome.receipt();
    assert_eq!(result.state, GrokBuildRunState::CompleteAdvisory);
    assert_eq!(result.terminal_verdict, Some(GrokBuildVerdict::Clean));
    assert_eq!(result.evidence_refs, ["advisory-summary"]);
    assert_eq!(result.request_id, "req-1");
    assert_eq!(result.identity, fx.identity);
    assert_eq!(receipt.mcp_policy, GrokBuildPolicyState::Disabled);
    assert_eq!(receipt.hooks_policy, GrokBuildPolicyState::Disabled);
    assert_eq!(receipt.plugin_policy, GrokBuildPolicyState::Disabled);
    assert_eq!(receipt.instruction_policy, GrokBuildPolicyState::Omitted);
    assert_eq!(
        receipt.permission_policy,
        GrokBuildMutationMode::IsolatedReview
    );
    assert_eq!(receipt.cleanup_state, GrokBuildCleanupState::Complete);
    assert!(receipt.credential_present);
    assert!(receipt.permissions_ok);
    result
        .validate_for_launch_and_receipt(&launch, receipt)
        .expect("lifecycle");
    assert!(fx.isolate_children().is_empty());
    let home = fx.captured_grok_home();
    assert!(!home.as_os_str().is_empty());
    assert!(!home.exists(), "isolated GROK_HOME must be cleaned");
    assert_no_secret(&format!("{outcome:?}"));
}
