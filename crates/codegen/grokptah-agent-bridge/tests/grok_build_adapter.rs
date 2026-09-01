//! Focused process-adapter tests. All child processes are fakes; no live
//! Grok/provider call is made.

#![cfg(target_os = "macos")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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
    revoked: Arc<AtomicBool>,
}

impl CredentialLeaseResolver for FakeResolver {
    fn resolve(&self, lease_id: &str) -> Result<CredentialLeaseHandle, GrokBuildAdapterError> {
        let _ = lease_id;
        Ok(CredentialLeaseHandle::from_host_path(self.path.clone()))
    }

    fn revoke(&self, lease_id: &str) -> Result<(), GrokBuildAdapterError> {
        assert_eq!(lease_id, "lease-1");
        self.revoked.store(true, Ordering::SeqCst);
        Ok(())
    }
}

struct Fixture {
    repo: tempfile::TempDir,
    isolate: tempfile::TempDir,
    fake_dir: tempfile::TempDir,
    capture_dir: PathBuf,
    identity: GrokBuildGitIdentity,
    lease_file: PathBuf,
    fake_bin: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let repo = tempfile::tempdir().expect("repo");
        let isolate = tempfile::tempdir().expect("isolate");
        fs::set_permissions(isolate.path(), fs::Permissions::from_mode(0o700))
            .expect("private isolate root");
        let fake_dir = tempfile::tempdir().expect("fake");
        git(repo.path(), &["init", "-b", "topic"]).expect("init");
        fs::write(repo.path().join("README"), "base\n").expect("readme");
        fs::write(repo.path().join(".gitignore"), "ignored/\n").expect("gitignore");
        git(repo.path(), &["add", "README", ".gitignore"]).expect("add");
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
        fs::write(
            fake_dir.path().join("source-path"),
            repo.path().as_os_str().as_encoded_bytes(),
        )
        .expect("source path fixture");
        let capture_dir = isolate.path().join("capture");

        Self {
            repo,
            isolate,
            fake_dir,
            capture_dir,
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
        if self.capture_dir.exists() {
            fs::remove_dir_all(&self.capture_dir).expect("reset capture directory");
        }
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
            allowed_files: vec!["README".into()],
            execution_approved: true,
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
            revoked: Arc::new(AtomicBool::new(false)),
        }
    }

    fn captured_argv(&self) -> Vec<String> {
        fs::read_to_string(self.capture_dir.join("captured-argv"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .filter(|l| !l.is_empty())
            .collect()
    }

    fn captured_env(&self) -> Vec<(String, String)> {
        fs::read_to_string(self.capture_dir.join("captured-env"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn captured_grok_home(&self) -> PathBuf {
        PathBuf::from(
            fs::read_to_string(self.capture_dir.join("captured-grok-home"))
                .unwrap_or_default()
                .trim(),
        )
    }

    fn captured_cwd(&self) -> PathBuf {
        PathBuf::from(
            fs::read_to_string(self.capture_dir.join("captured-cwd"))
                .unwrap_or_default()
                .trim(),
        )
    }

    fn isolate_children(&self) -> Vec<PathBuf> {
        fs::read_dir(self.isolate.path())
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                    .filter(|path| path != &self.capture_dir)
                    .collect()
            })
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
capture="${GROK_HOME%/*}/capture"
mkdir -p "$capture"
behavior=complete
if [ -f "$dir/behavior" ]; then
  behavior=$(cat "$dir/behavior")
fi
if [ "$1" = "inspect" ] && [ "$2" = "--json" ]; then
  printf '%s\n' "$@" > "$capture/captured-inspect-argv"
  env | sort > "$capture/captured-inspect-env"
  instructions='[]'
  if [ "$behavior" = "inspect-dirty" ]; then
    instructions='[{"path":"CLAUDE.md"}]'
  fi
  if [ "$behavior" = "inspect-unknown" ]; then
    extra=',"futureCompatibilitySurface":[]'
  else
    extra=''
  fi
  printf '{"grokVersion":"1.0.5","channel":"stable","cwd":"%s","projectRoot":"%s","projectTrusted":true,"projectInstructions":%s,"permissions":{"loaded":0,"managedSettingsActive":false,"managedSettingsExists":false,"managedSettingsPath":"/managed/settings","marketplaceAllowlist":[],"mcpServerAllowlist":[],"skipped":[],"sources":[]},"loginPolicy":{"apiKeyAuthDisabled":false,"disableApiKeyAuth":null,"forceLoginTeamUuid":null},"hooks":[],"skills":[],"agents":[],"plugins":[],"marketplaces":[],"mcpServers":[],"lspServers":[],"configSources":{"layers":[{"path":"%s/config.toml","role":"user"}]},"externalCompat":{"cells":[{"enabled":false,"source":"config","surface":"skills","vendor":"cursor"},{"enabled":false,"source":"config","surface":"rules","vendor":"cursor"},{"enabled":false,"source":"config","surface":"agents","vendor":"cursor"},{"enabled":false,"source":"config","surface":"mcps","vendor":"cursor"},{"enabled":false,"source":"config","surface":"hooks","vendor":"cursor"},{"enabled":false,"source":"config","surface":"sessions","vendor":"cursor"},{"enabled":false,"source":"config","surface":"skills","vendor":"claude"},{"enabled":false,"source":"config","surface":"rules","vendor":"claude"},{"enabled":false,"source":"config","surface":"agents","vendor":"claude"},{"enabled":false,"source":"config","surface":"mcps","vendor":"claude"},{"enabled":false,"source":"config","surface":"hooks","vendor":"claude"},{"enabled":false,"source":"config","surface":"sessions","vendor":"claude"},{"enabled":false,"source":"config","surface":"sessions","vendor":"codex"}],"remoteSettingsLoaded":false}%s}\n' "$PWD" "$PWD" "$instructions" "$GROK_HOME" "$extra"
  exit 0
fi
printf '%s\n' "$@" > "$capture/captured-argv"
env | sort > "$capture/captured-env"
printf '%s\n' "$GROK_HOME" > "$capture/captured-grok-home"
printf '%s\n' "$PWD" > "$capture/captured-cwd"
/usr/bin/git rev-parse --path-format=absolute --git-dir > "$capture/captured-git-dir"
/usr/bin/git rev-parse --path-format=absolute --git-common-dir > "$capture/captured-common-dir"
/usr/bin/git remote > "$capture/captured-remotes"
if [ -n "$GROK_HOME" ] && [ -f "$GROK_HOME/config.toml" ]; then
  cp "$GROK_HOME/config.toml" "$capture/captured-config.toml"
fi
if [ -n "$GROK_HOME" ] && [ -f "$GROK_HOME/sandbox.toml" ]; then
  cp "$GROK_HOME/sandbox.toml" "$capture/captured-sandbox.toml"
fi
if [ -n "$GROK_HOME" ] && [ -f "$GROK_HOME/auth.json" ]; then
  printf 'present\n' > "$capture/auth-present"
fi
session_id=''
previous=''
for arg in "$@"; do
  if [ "$previous" = '--session-id' ]; then
    session_id="$arg"
  fi
  previous="$arg"
done
if [ -n "$session_id" ] && [ "$behavior" != 'complete-no-session' ]; then
  mkdir -p "$GROK_HOME/sessions/workspace/$session_id"
  if [ "$behavior" = 'complete-root-index' ]; then
    printf 'bounded index fixture\n' > "$GROK_HOME/sessions/session_search.sqlite"
  fi
  if [ "$behavior" = 'complete-unknown-root-file' ]; then
    printf 'unknown surface\n' > "$GROK_HOME/sessions/future-index"
  fi
  printf '{"method":"session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"bounded advisory summary\\nGROK_BUILD_VERDICT=clean"}}},"timestamp":"2026-08-31T00:00:00Z"}\n' "$session_id" > "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
  printf '{"method":"_x.ai/session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":"2026-08-31T00:00:01Z"}\n' "$session_id" >> "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
  if [ "$behavior" = 'complete-bad-session' ]; then
    printf '%s\n' '{"event":"unbound"}' > "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
  fi
  if [ "$behavior" = 'complete-terminal-first' ]; then
    printf '{"method":"_x.ai/session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":"2026-08-31T00:00:00Z"}\n' "$session_id" > "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
    printf '{"method":"session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"bounded advisory summary"}}},"timestamp":"2026-08-31T00:00:01Z"}\n' "$session_id" >> "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
  fi
  if [ "$behavior" = 'complete-empty-chunk' ]; then
    printf '{"method":"session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":""}}},"timestamp":"2026-08-31T00:00:00Z"}\n' "$session_id" > "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
    printf '{"method":"_x.ai/session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":"2026-08-31T00:00:01Z"}\n' "$session_id" >> "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
  fi
  if [ "$behavior" = 'complete-mismatched-summary' ]; then
    printf '{"method":"session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"different summary\\nGROK_BUILD_VERDICT=clean"}}},"timestamp":"2026-08-31T00:00:00Z"}\n' "$session_id" > "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
    printf '{"method":"_x.ai/session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":"2026-08-31T00:00:01Z"}\n' "$session_id" >> "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
  fi
  if [ "$behavior" = 'complete-integer-timestamp' ]; then
    printf '{"method":"session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"bounded advisory summary\\nGROK_BUILD_VERDICT=clean"}}},"timestamp":1788214716}\n' "$session_id" > "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
    printf '{"method":"_x.ai/session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"turn_completed","prompt_id":"11111111-1111-4111-8111-111111111111","stop_reason":"end_turn","usage":{},"numTurns":1,"elapsed_ms":10}},"timestamp":1788214717}\n' "$session_id" >> "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
  fi
  if [ "$behavior" = 'complete-tool-transcript' ]; then
    printf '{"method":"session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"bounded "}}},"timestamp":1788214716}\n' "$session_id" > "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
    printf '{"method":"session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"tool_call","toolCallId":"tool-1","title":"read_file","rawInput":{}}},"timestamp":1788214716}\n' "$session_id" >> "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
    printf '{"method":"session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"tool_call_update","toolCallId":"tool-1","status":"completed","rawOutput":{}}},"timestamp":1788214716}\n' "$session_id" >> "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
    printf '{"method":"session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"advisory summary\\nGROK_BUILD_VERDICT=clean"}}},"timestamp":1788214717}\n' "$session_id" >> "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
    printf '{"method":"_x.ai/session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":1788214718}\n' "$session_id" >> "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
  fi
  if [ "$behavior" = 'complete-invalid-timestamp' ]; then
    printf '{"method":"session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"bounded advisory summary\\nGROK_BUILD_VERDICT=clean"}}},"timestamp":{}}\n' "$session_id" > "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
    printf '{"method":"_x.ai/session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":1788214717}\n' "$session_id" >> "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
  fi
  if [ "$behavior" = 'complete-prefixed-summary' ]; then
    printf '{"method":"session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"contradictory prefix\\nbounded advisory summary\\nGROK_BUILD_VERDICT=clean"}}},"timestamp":"2026-08-31T00:00:00Z"}\n' "$session_id" > "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
    printf '{"method":"_x.ai/session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":"2026-08-31T00:00:01Z"}\n' "$session_id" >> "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
  fi
  if [ "$behavior" = 'complete-duplicate-terminal' ]; then
    printf '{"method":"_x.ai/session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":"2026-08-31T00:00:02Z"}\n' "$session_id" >> "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
  fi
  if [ "$behavior" = 'leak-secret' ]; then
    printf '{"method":"session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"sk-live-secret-not-real\\nbounded advisory summary\\nGROK_BUILD_VERDICT=clean"}}},"timestamp":"2026-08-31T00:00:00Z"}\n' "$session_id" > "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
    printf '{"method":"_x.ai/session/update","params":{"_meta":{},"sessionId":"%s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":"2026-08-31T00:00:01Z"}\n' "$session_id" >> "$GROK_HOME/sessions/workspace/$session_id/updates.jsonl"
  fi
fi
case "$behavior" in
  complete|complete-bad-session|complete-terminal-first|complete-empty-chunk|complete-mismatched-summary|complete-prefixed-summary|complete-duplicate-terminal|complete-integer-timestamp|complete-invalid-timestamp|complete-tool-transcript|complete-root-index|complete-unknown-root-file)
    printf '{"text":"bounded advisory summary\\nGROK_BUILD_VERDICT=clean","stopReason":"end_turn","sessionId":"%s","requestId":"11111111-1111-4111-8111-111111111111","thought":"","usage":{},"num_turns":1,"total_cost_usd":0.0,"total_cost_usd_ticks":0,"modelUsage":{}}\n' "$session_id"
    exit 0
    ;;
  complete-no-session)
    printf '{"text":"bounded advisory summary\\nGROK_BUILD_VERDICT=clean","stopReason":"end_turn","sessionId":"%s","requestId":"11111111-1111-4111-8111-111111111111","thought":"","usage":{},"num_turns":1,"total_cost_usd":0.0,"total_cost_usd_ticks":0,"modelUsage":{}}\n' "$session_id"
    exit 0
    ;;
  marker-only)
    printf '{"text":"GROK_BUILD_VERDICT=clean","stopReason":"end_turn","sessionId":"%s","requestId":"11111111-1111-4111-8111-111111111111","thought":"","usage":{},"num_turns":1,"total_cost_usd":0.0,"total_cost_usd_ticks":0,"modelUsage":{}}\n' "$session_id"
    exit 0
    ;;
  max-turns)
    printf '{"text":"partial output","stopReason":"max_turn_requests","sessionId":"%s","requestId":"11111111-1111-4111-8111-111111111111","thought":"","usage":{},"num_turns":8,"total_cost_usd":0.0,"total_cost_usd_ticks":0,"modelUsage":{}}\n' "$session_id"
    exit 0
    ;;
  partial)
    printf '{"text":"partial output without verdict","stopReason":"end_turn","sessionId":"%s","requestId":"11111111-1111-4111-8111-111111111111","thought":"","usage":{},"num_turns":1,"total_cost_usd":0.0,"total_cost_usd_ticks":0,"modelUsage":{}}\n' "$session_id"
    exit 0
    ;;
  mutate-partial)
    printf 'unqualified mutation\n' > mutated-by-child.txt
    printf '{"text":"partial output without verdict","stopReason":"end_turn","sessionId":"%s","requestId":"11111111-1111-4111-8111-111111111111","thought":"","usage":{},"num_turns":1,"total_cost_usd":0.0,"total_cost_usd_ticks":0,"modelUsage":{}}\n' "$session_id"
    exit 0
    ;;
  overflow)
    dd if=/dev/zero bs=1024 count=64 2>/dev/null
    exit 0
    ;;
  sleep)
    echo $$ > "$capture/pid"
    /bin/sleep 30 &
    descendant=$!
    echo "$descendant" > "$capture/descendant-pid"
    wait "$descendant"
    ;;
  mutate-sleep)
    printf 'unqualified mutation\n' > mutated-by-child.txt
    echo $$ > "$capture/pid"
    /bin/sleep 30 &
    descendant=$!
    echo "$descendant" > "$capture/descendant-pid"
    wait "$descendant"
    ;;
  mutate-commit)
    printf 'committed mutation\n' > committed-by-child.txt
    /usr/bin/git add committed-by-child.txt
    /usr/bin/git -c user.name=test -c user.email=test@example.com commit -m mutation >/dev/null 2>&1
    printf '{"text":"bounded advisory summary\\nGROK_BUILD_VERDICT=clean","stopReason":"end_turn","sessionId":"%s","requestId":"11111111-1111-4111-8111-111111111111","thought":"","usage":{},"num_turns":1,"total_cost_usd":0.0,"total_cost_usd_ticks":0,"modelUsage":{}}\n' "$session_id"
    exit 0
    ;;
  mutate-ref)
    /usr/bin/git update-ref refs/heads/child-ref HEAD >/dev/null 2>&1
    printf '{"text":"bounded advisory summary\\nGROK_BUILD_VERDICT=clean","stopReason":"end_turn","sessionId":"%s","requestId":"11111111-1111-4111-8111-111111111111","thought":"","usage":{},"num_turns":1,"total_cost_usd":0.0,"total_cost_usd_ticks":0,"modelUsage":{}}\n' "$session_id"
    exit 0
    ;;
  mutate-common-git)
    common=$(/usr/bin/git rev-parse --git-common-dir)
    /usr/bin/git config --local grokptah.malicious true
    mkdir -p "$common/hooks"
    printf '#!/bin/sh\nexit 1\n' > "$common/hooks/pre-commit"
    printf '{"text":"bounded advisory summary\\nGROK_BUILD_VERDICT=clean","stopReason":"end_turn","sessionId":"%s","requestId":"11111111-1111-4111-8111-111111111111","thought":"","usage":{},"num_turns":1,"total_cost_usd":0.0,"total_cost_usd_ticks":0,"modelUsage":{}}\n' "$session_id"
    exit 0
    ;;
  mutate-source-absolute)
    source=$(cat "$dir/source-path")
    printf 'source escape\n' > "$source/README" 2>/dev/null || true
    printf '{"text":"bounded advisory summary\\nGROK_BUILD_VERDICT=clean","stopReason":"end_turn","sessionId":"%s","requestId":"11111111-1111-4111-8111-111111111111","thought":"","usage":{},"num_turns":1,"total_cost_usd":0.0,"total_cost_usd_ticks":0,"modelUsage":{}}\n' "$session_id"
    exit 0
    ;;
  mutate-ignored)
    mkdir -p ignored
    printf 'hidden mutation\n' > ignored/result
    printf '{"text":"bounded advisory summary\\nGROK_BUILD_VERDICT=clean","stopReason":"end_turn","sessionId":"%s","requestId":"11111111-1111-4111-8111-111111111111","thought":"","usage":{},"num_turns":1,"total_cost_usd":0.0,"total_cost_usd_ticks":0,"modelUsage":{}}\n' "$session_id"
    exit 0
    ;;
  mutate)
    printf 'mutated\n' > mutated-by-child.txt
    printf '{"text":"bounded advisory summary\\nGROK_BUILD_VERDICT=clean","stopReason":"end_turn","sessionId":"%s","requestId":"11111111-1111-4111-8111-111111111111","thought":"","usage":{},"num_turns":1,"total_cost_usd":0.0,"total_cost_usd_ticks":0,"modelUsage":{}}\n' "$session_id"
    exit 0
    ;;
  leak-secret)
    printf '{"text":"sk-live-secret-not-real\\nbounded advisory summary\\nGROK_BUILD_VERDICT=clean","stopReason":"end_turn","sessionId":"%s","requestId":"11111111-1111-4111-8111-111111111111","thought":"","usage":{},"num_turns":1,"total_cost_usd":0.0,"total_cost_usd_ticks":0,"modelUsage":{}}\n' "$session_id"
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
            "bypassPermissions",
            "--disable-web-search",
            "--no-subagents",
            "--max-turns",
            "8"
        ]
    );
    assert_eq!(argv[8], "--session-id");
    assert!(Uuid::parse_str(&argv[9]).is_ok(), "{argv:?}");
    assert_eq!(&argv[10..], ["--output-format", "json"]);
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
            .find(|(k, _)| k == "TMPDIR")
            .map(|(_, v)| v.as_str()),
        Some(home)
    );
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

    let config = fs::read_to_string(fx.capture_dir.join("captured-config.toml")).expect("config");
    assert!(config.contains("[compat.claude]"));
    assert!(config.contains("[compat.cursor]"));
    assert!(config.contains("[compat.codex]"));
    assert!(config.contains("official_marketplace_auto_installed = false"));
    let sandbox =
        fs::read_to_string(fx.capture_dir.join("captured-sandbox.toml")).expect("sandbox");
    assert!(sandbox.contains("[profiles.grokptah_read_only]"));
    assert!(!sandbox.contains("grokptah_workspace"));
    assert_eq!(
        fs::read_to_string(fx.capture_dir.join("auth-present")).expect("auth"),
        "present\n"
    );
    assert_eq!(outcome.result().state, GrokBuildRunState::CompleteAdvisory);
    assert_eq!(outcome.result().session_id, argv[9]);
    assert_ne!(fx.captured_cwd(), fx.repo.path());
    let captured_git_dir = PathBuf::from(
        fs::read_to_string(fx.capture_dir.join("captured-git-dir"))
            .expect("private git directory")
            .trim(),
    );
    let captured_common_dir = PathBuf::from(
        fs::read_to_string(fx.capture_dir.join("captured-common-dir"))
            .expect("private common directory")
            .trim(),
    );
    assert_eq!(captured_git_dir, captured_common_dir);
    assert!(captured_git_dir.starts_with(fx.captured_cwd()));
    assert_eq!(
        fs::read_to_string(fx.capture_dir.join("captured-remotes")).expect("remote capture"),
        ""
    );
    assert_eq!(
        dunce::canonicalize(
            fx.captured_cwd()
                .parent()
                .expect("captured checkout parent")
        )
        .expect("captured checkout parent exists"),
        dunce::canonicalize(fx.isolate.path()).expect("isolate parent")
    );
}

#[tokio::test]
async fn isolation_parent_must_not_overlap_the_authoritative_checkout() {
    let fx = Fixture::new();
    fx.set_behavior("complete");
    let mut host = fx.host();
    host.isolate_parent = fx.repo.path().to_path_buf();
    let error = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
        &host,
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect_err("source and writable isolation root must be disjoint");
    assert_eq!(error, GrokBuildAdapterError::IsolationFailed);
    assert!(!fx.capture_dir.join("captured-argv").exists());
}

#[tokio::test]
async fn os_sandbox_denies_absolute_source_checkout_writes() {
    let fx = Fixture::new();
    fx.set_behavior("mutate-source-absolute");
    let before = fs::read(fx.repo.path().join("README")).expect("source before");
    let outcome = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
        &fx.host(),
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect("denied source escape leaves a complete no-change advisory");
    assert_eq!(outcome.result().state, GrokBuildRunState::CompleteAdvisory);
    assert_eq!(
        fs::read(fx.repo.path().join("README")).expect("source after"),
        before
    );
    assert!(git_stdout(
        fx.repo.path(),
        &["status", "--porcelain=v1", "--untracked-files=all"]
    )
    .is_empty());
}

#[tokio::test]
async fn private_clone_rejects_commit_ref_and_git_control_mutations() {
    for behavior in ["mutate-commit", "mutate-ref", "mutate-common-git"] {
        let fx = Fixture::new();
        fx.set_behavior(behavior);
        let mut host = fx.host();
        host.allowed_files = vec!["committed-by-child.txt".into()];
        let error = launch_grok_build(
            &fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
            &host,
            &fx.resolver(),
            CancellationToken::new(),
        )
        .await
        .expect_err("private Git control mutation must fail closed");
        assert_eq!(error, GrokBuildAdapterError::IsolationFailed, "{behavior}");
        assert!(git_stdout(
            fx.repo.path(),
            &["status", "--porcelain=v1", "--untracked-files=all"]
        )
        .is_empty());
        assert!(!fx.repo.path().join("committed-by-child.txt").exists());
        assert!(fx.isolate_children().is_empty());
    }
}

#[tokio::test]
async fn noninteractive_tool_execution_requires_explicit_host_approval() {
    let fx = Fixture::new();
    fx.set_behavior("complete");
    let mut host = fx.host();
    host.execution_approved = false;
    let error = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
        &host,
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect_err("unapproved host launch must fail before spawning Grok");
    assert_eq!(error, GrokBuildAdapterError::InvalidRequest);
    assert!(!fx.capture_dir.join("captured-argv").exists());
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
    assert!(!fx.capture_dir.join("captured-argv").exists());
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
    assert!(fx.capture_dir.join("captured-inspect-argv").exists());
    assert!(!fx.capture_dir.join("captured-argv").exists());
    assert!(fx.isolate_children().is_empty());
}

#[tokio::test]
async fn unknown_inspection_surface_fails_before_task_launch() {
    let fx = Fixture::new();
    fx.set_behavior("inspect-unknown");
    let err = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
        &fx.host(),
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect_err("unknown inspection surface");
    assert_eq!(err, GrokBuildAdapterError::IsolationFailed);
    assert!(!fx.capture_dir.join("captured-argv").exists());
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
    if let Ok(pid) = fs::read_to_string(fx.capture_dir.join("pid")) {
        let pid = pid.trim();
        let alive = Command::new("/bin/kill")
            .args(["-0", pid])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(!alive, "child still alive after timeout");
    }
    if let Ok(pid) = fs::read_to_string(fx.capture_dir.join("descendant-pid")) {
        let pid = pid.trim();
        let alive = Command::new("/bin/kill")
            .args(["-0", pid])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(!alive, "descendant still alive after timeout");
    }
}

#[tokio::test]
async fn timed_out_mutation_never_reaches_the_work_checkout() {
    let fx = Fixture::new();
    fx.set_behavior("mutate-sleep");
    let mut host = fx.host();
    host.allowed_files = vec!["mutated-by-child.txt".into()];

    let outcome = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::IsolatedReview, 400),
        &host,
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect("timeout outcome");

    assert_eq!(outcome.result().state, GrokBuildRunState::FailedClosed);
    assert!(!fx.repo.path().join("mutated-by-child.txt").exists());
    assert!(git_stdout(
        fx.repo.path(),
        &["status", "--porcelain=v1", "--untracked-files=all"]
    )
    .is_empty());
    assert!(fx.isolate_children().is_empty());
}

#[tokio::test]
async fn cancellation_kills_process_tree_and_cleans_home() {
    let fx = Fixture::new();
    fx.set_behavior("sleep");
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let launch = fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000);
    let host = fx.host();
    let resolver = fx.resolver();
    let launched = fx.capture_dir.join("captured-argv");
    let ((), outcome) = tokio::join!(
        async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while !launched.exists() && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            assert!(launched.exists(), "task process never launched");
            trigger.cancel();
        },
        launch_grok_build(&launch, &host, &resolver, cancel)
    );
    let outcome = outcome.expect("cancelled execution returns a closed outcome");
    assert_eq!(outcome.result().state, GrokBuildRunState::FailedClosed);
    assert!(outcome.advisory_evidence().is_none());
    assert!(fx.isolate_children().is_empty());
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
    let resolver = FakeResolver {
        path: link,
        revoked: Arc::new(AtomicBool::new(false)),
    };
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
async fn isolated_review_captures_only_allowlisted_mutation_evidence() {
    let fx = Fixture::new();
    fx.set_behavior("mutate");
    let mut host = fx.host();
    host.allowed_files = vec!["mutated-by-child.txt".into()];
    let outcome = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
        &host,
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect("allowlisted isolated mutation");
    let mutation = outcome
        .mutation_evidence()
        .expect("bounded mutation evidence");
    assert_eq!(mutation.final_head_sha(), fx.identity.head_sha);
    assert_eq!(mutation.final_ref(), fx.identity.git_ref);
    assert_eq!(mutation.changed_paths(), &["mutated-by-child.txt"]);
    assert!(mutation.diff_digest().starts_with("sha256:"));
    assert_no_secret(&format!("{mutation:?}"));
}

#[tokio::test]
async fn isolated_review_rejects_forbidden_mutation_and_publish_remote() {
    let fx = Fixture::new();
    fx.set_behavior("mutate");
    let error = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
        &fx.host(),
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect_err("forbidden mutation");
    assert_eq!(error, GrokBuildAdapterError::ReadOnlyMutation);
    assert!(!fx.repo.path().join("mutated-by-child.txt").exists());
    assert!(git_stdout(
        fx.repo.path(),
        &["status", "--porcelain=v1", "--untracked-files=all"]
    )
    .is_empty());

    let remote = Fixture::new();
    remote.set_behavior("complete");
    git(
        remote.repo.path(),
        &["remote", "add", "origin", "https://example.invalid/repo"],
    )
    .expect("add remote");
    let error = launch_grok_build(
        &remote.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
        &remote.host(),
        &remote.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect_err("publish-capable remote");
    assert_eq!(error, GrokBuildAdapterError::IsolationFailed);
    assert!(!remote.capture_dir.join("captured-argv").exists());
}

#[tokio::test]
async fn read_only_mode_is_refused_without_observed_sandbox_authority() {
    let fx = Fixture::new();
    fx.set_behavior("complete");
    let error = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::ReadOnly, 60_000),
        &fx.host(),
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect_err("read-only lacks an observable sandbox receipt");
    assert_eq!(error, GrokBuildAdapterError::IsolationFailed);
    assert!(!fx.capture_dir.join("captured-inspect-argv").exists());
    assert!(!fx.capture_dir.join("captured-argv").exists());
    assert!(fx.isolate_children().is_empty());
}

#[tokio::test]
async fn read_only_never_reaches_a_mutating_child() {
    for behavior in ["mutate-commit", "mutate-ref", "mutate-ignored"] {
        let fx = Fixture::new();
        fx.set_behavior(behavior);
        let error = launch_grok_build(
            &fx.launch(GrokBuildMutationMode::ReadOnly, 60_000),
            &fx.host(),
            &fx.resolver(),
            CancellationToken::new(),
        )
        .await
        .expect_err("read-only must fail before spawn");
        assert_eq!(
            error,
            GrokBuildAdapterError::IsolationFailed,
            "behavior {behavior} reached the child"
        );
        assert!(!fx.capture_dir.join("captured-argv").exists());
        assert!(fx.isolate_children().is_empty());
    }
}

#[tokio::test]
async fn partial_and_max_turn_are_nonresumable_and_fail_closed() {
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
    assert_eq!(partial.result().state, GrokBuildRunState::FailedClosed);
    assert_eq!(partial.result().terminal_verdict, None);
    assert_eq!(
        partial.receipt().cleanup_state,
        GrokBuildCleanupState::FailedClosed
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
    assert_eq!(maxed.result().state, GrokBuildRunState::FailedClosed);
    assert_eq!(maxed.result().terminal_verdict, None);
    assert_ne!(maxed.result().state, GrokBuildRunState::CompleteAdvisory);
}

#[tokio::test]
async fn partial_isolated_review_restores_allowlisted_mutations() {
    let fx = Fixture::new();
    fx.set_behavior("mutate-partial");
    let mut host = fx.host();
    host.allowed_files = vec!["mutated-by-child.txt".into()];

    let outcome = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
        &host,
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect("partial execution remains an observable failed-closed outcome");

    assert_eq!(outcome.result().state, GrokBuildRunState::FailedClosed);
    assert!(outcome.mutation_evidence().is_none());
    assert!(!fx.repo.path().join("mutated-by-child.txt").exists());
    assert!(git_stdout(
        fx.repo.path(),
        &["status", "--porcelain=v1", "--untracked-files=all"]
    )
    .is_empty());
}

#[tokio::test]
async fn terminal_marker_requires_summary_and_retained_session() {
    for behavior in [
        "marker-only",
        "complete-no-session",
        "complete-bad-session",
        "complete-terminal-first",
        "complete-empty-chunk",
        "complete-mismatched-summary",
        "complete-prefixed-summary",
        "complete-duplicate-terminal",
        "complete-invalid-timestamp",
        "complete-unknown-root-file",
    ] {
        let fx = Fixture::new();
        fx.set_behavior(behavior);
        let outcome = launch_grok_build(
            &fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
            &fx.host(),
            &fx.resolver(),
            CancellationToken::new(),
        )
        .await
        .expect("closed result");
        assert_eq!(outcome.result().state, GrokBuildRunState::FailedClosed);
        assert!(outcome.result().terminal_verdict.is_none());
    }
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
    assert_eq!(result.evidence_refs.len(), 2);
    assert!(result.evidence_refs[0].starts_with("summary-sha256-"));
    assert!(result.evidence_refs[1].starts_with("session-sha256-"));
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
    let evidence = outcome
        .advisory_evidence()
        .expect("completed advisory retains bounded host evidence");
    assert_eq!(
        evidence.cli_request_id(),
        "11111111-1111-4111-8111-111111111111"
    );
    assert_eq!(
        evidence.summary(),
        "bounded advisory summary\nGROK_BUILD_VERDICT=clean"
    );
    assert!(!evidence.session_updates().is_empty());
    assert_eq!(evidence.summary_ref(), result.evidence_refs[0]);
    assert_eq!(evidence.session_ref(), result.evidence_refs[1]);
    result
        .validate_for_launch_and_receipt(&launch, receipt)
        .expect("lifecycle");
    assert!(fx.isolate_children().is_empty());
    let home = fx.captured_grok_home();
    assert!(!home.as_os_str().is_empty());
    assert!(!home.exists(), "isolated GROK_HOME must be cleaned");
    assert_no_secret(&format!("{outcome:?}"));
}

#[tokio::test]
async fn current_integer_session_timestamps_are_verified_without_weakening_shape_checks() {
    let fx = Fixture::new();
    fx.set_behavior("complete-integer-timestamp");
    let outcome = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
        &fx.host(),
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect("current Grok transcript shape");
    assert_eq!(outcome.result().state, GrokBuildRunState::CompleteAdvisory);
    assert_eq!(
        outcome.result().terminal_verdict,
        Some(GrokBuildVerdict::Clean)
    );
    assert!(outcome.advisory_evidence().is_some());
}

#[tokio::test]
async fn tool_using_transcript_binds_every_ordered_agent_chunk_to_stdout() {
    let fx = Fixture::new();
    fx.set_behavior("complete-tool-transcript");
    let outcome = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
        &fx.host(),
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect("tool-using transcript");
    assert_eq!(outcome.result().state, GrokBuildRunState::CompleteAdvisory);
    assert_eq!(
        outcome
            .advisory_evidence()
            .expect("bound advisory")
            .summary(),
        "bounded advisory summary\nGROK_BUILD_VERDICT=clean"
    );
}

#[tokio::test]
async fn current_session_search_index_is_allowed_but_unknown_root_files_fail_closed() {
    let fx = Fixture::new();
    fx.set_behavior("complete-root-index");
    let outcome = launch_grok_build(
        &fx.launch(GrokBuildMutationMode::IsolatedReview, 60_000),
        &fx.host(),
        &fx.resolver(),
        CancellationToken::new(),
    )
    .await
    .expect("known Grok session index");
    assert_eq!(outcome.result().state, GrokBuildRunState::CompleteAdvisory);
    assert!(outcome.advisory_evidence().is_some());
}
