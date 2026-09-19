//! Thin Tauri chrome over AgentHost coding-worktree disposition.
//!
//! Calls existing `AgentHostHandle::coding_worktree_*` APIs only. No new
//! disposition semantics, no auto-Accept, and no isolated-surface / Computer
//! Mode admission flip.

use std::path::PathBuf;

use grokptah_agent_bridge::{
    AgentHostHandle, CodingWorktreeHostView, SessionError, SessionErrorCode,
};
use tauri::State;
use uuid::Uuid;

use crate::commands::run_blocking;
use crate::AppState;

fn map_err(error: SessionError) -> String {
    error.to_string()
}

fn require_handle(handle: &str) -> Result<(), String> {
    if handle.trim().is_empty() {
        return Err(SessionError::invalid_state("coding worktree handle is required").to_string());
    }
    Ok(())
}

fn require_accept_args(apply_target: &str, expected_patch_digest: &str) -> Result<(), String> {
    if apply_target.trim().is_empty() {
        return Err(
            SessionError::invalid_state("accept requires an explicit apply_target").to_string(),
        );
    }
    if expected_patch_digest.trim().is_empty() {
        return Err(
            SessionError::invalid_state("accept requires an exact sha256: patch digest")
                .to_string(),
        );
    }
    Ok(())
}

pub(crate) fn view(host: &AgentHostHandle, handle: &str) -> Result<CodingWorktreeHostView, String> {
    require_handle(handle)?;
    host.coding_worktree_view(handle).map_err(map_err)
}

/// Bind to the coding worktree owned by an AgentHost session, if any.
/// Absence is `Ok(None)` so the UI can disable/hide without inventing a session.
pub(crate) fn view_for_agent_session(
    host: &AgentHostHandle,
    session_id: Uuid,
) -> Result<Option<CodingWorktreeHostView>, String> {
    match host.coding_worktree_handle_for_session(session_id) {
        Ok(handle) => view(host, &handle).map(Some),
        Err(error) if error.code == SessionErrorCode::InvalidState => Ok(None),
        Err(error) => Err(map_err(error)),
    }
}

pub(crate) fn pause(
    host: &AgentHostHandle,
    handle: &str,
) -> Result<CodingWorktreeHostView, String> {
    require_handle(handle)?;
    host.coding_worktree_pause(handle).map_err(map_err)?;
    view(host, handle)
}

pub(crate) fn stop(host: &AgentHostHandle, handle: &str) -> Result<CodingWorktreeHostView, String> {
    require_handle(handle)?;
    host.coding_worktree_stop(handle).map_err(map_err)?;
    view(host, handle)
}

pub(crate) fn accept(
    host: &AgentHostHandle,
    handle: &str,
    apply_target: &str,
    expected_patch_digest: &str,
) -> Result<CodingWorktreeHostView, String> {
    require_handle(handle)?;
    require_accept_args(apply_target, expected_patch_digest)?;
    host.coding_worktree_accept(
        handle,
        PathBuf::from(apply_target.trim()),
        expected_patch_digest.trim(),
    )
    .map_err(map_err)?;
    view(host, handle)
}

pub(crate) fn discard(
    host: &AgentHostHandle,
    handle: &str,
) -> Result<CodingWorktreeHostView, String> {
    require_handle(handle)?;
    host.coding_worktree_discard(handle).map_err(map_err)?;
    view(host, handle)
}

pub(crate) fn keep_for_review(
    host: &AgentHostHandle,
    handle: &str,
) -> Result<CodingWorktreeHostView, String> {
    require_handle(handle)?;
    host.coding_worktree_keep_for_review(handle)
        .map_err(map_err)?;
    view(host, handle)
}

#[tauri::command]
pub async fn coding_worktree_view(
    state: State<'_, AppState>,
    handle: String,
) -> Result<CodingWorktreeHostView, String> {
    let host = state.host.clone();
    run_blocking(move || view(&host, &handle)).await
}

#[tauri::command]
pub async fn coding_worktree_for_session(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Option<CodingWorktreeHostView>, String> {
    let host = state.host.clone();
    let session_id = Uuid::parse_str(&session_id).map_err(|error| error.to_string())?;
    run_blocking(move || view_for_agent_session(&host, session_id)).await
}

#[tauri::command]
pub async fn coding_worktree_pause(
    state: State<'_, AppState>,
    handle: String,
) -> Result<CodingWorktreeHostView, String> {
    let host = state.host.clone();
    run_blocking(move || pause(&host, &handle)).await
}

#[tauri::command]
pub async fn coding_worktree_stop(
    state: State<'_, AppState>,
    handle: String,
) -> Result<CodingWorktreeHostView, String> {
    let host = state.host.clone();
    run_blocking(move || stop(&host, &handle)).await
}

#[tauri::command]
pub async fn coding_worktree_accept(
    state: State<'_, AppState>,
    handle: String,
    apply_target: String,
    expected_patch_digest: String,
) -> Result<CodingWorktreeHostView, String> {
    let host = state.host.clone();
    run_blocking(move || accept(&host, &handle, &apply_target, &expected_patch_digest)).await
}

#[tauri::command]
pub async fn coding_worktree_discard(
    state: State<'_, AppState>,
    handle: String,
) -> Result<CodingWorktreeHostView, String> {
    let host = state.host.clone();
    run_blocking(move || discard(&host, &handle)).await
}

#[tauri::command]
pub async fn coding_worktree_keep_for_review(
    state: State<'_, AppState>,
    handle: String,
) -> Result<CodingWorktreeHostView, String> {
    let host = state.host.clone();
    run_blocking(move || keep_for_review(&host, &handle)).await
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::MutexGuard;

    use grokptah_agent_bridge::{
        home_override_serial, set_grokptah_home_override, AgentHost, CodingWorktreeHostView,
        HostConfig, HostRuntime, SessionDisposition, SessionErrorCode, SessionPhase,
        SessionSnapshot, SYNTHETIC_SESSION_NONCLAIM,
    };
    use tempfile::TempDir;

    use super::*;

    struct DesktopHost {
        runtime: HostRuntime,
        host: AgentHostHandle,
        _home: TempDir,
        _serial: MutexGuard<'static, ()>,
    }

    impl DesktopHost {
        fn new() -> Self {
            let serial = home_override_serial();
            let home = TempDir::new().expect("host home");
            set_grokptah_home_override(Some(home.path().join(".grokptah")));
            let runtime = AgentHost::create(HostConfig {
                always_approve: true,
                ..HostConfig::default()
            })
            .expect("acquire the GrokPtah instance lock");
            runtime.start().expect("start host");
            set_grokptah_home_override(None);
            let host = runtime.handle();
            Self {
                runtime,
                host,
                _home: home,
                _serial: serial,
            }
        }

        fn snapshot_base(&self, handle: &str) -> PathBuf {
            self.runtime
                .runtime_home()
                .path()
                .join("coding-worktrees")
                .join(handle)
        }
    }

    fn init_fixture_repo(dir: &Path) {
        let run = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .expect("git");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run(&["init", "-q"]);
        run(&["config", "user.name", "GrokPtah test"]);
        run(&["config", "user.email", "test@example.invalid"]);
        std::fs::write(dir.join("README.md"), "baseline\n").unwrap();
        run(&["add", "README.md"]);
        run(&["commit", "-qm", "initial"]);
    }

    fn clone_apply_target(source_repo: &Path) -> TempDir {
        let apply_root = TempDir::new().expect("apply tempdir");
        let output = std::process::Command::new("git")
            .args(["clone", "-q"])
            .arg(source_repo)
            .arg(apply_root.path())
            .output()
            .expect("clone apply target");
        assert!(output.status.success());
        apply_root
    }

    fn create_bound(
        env: &DesktopHost,
        repo: &Path,
        handle: &str,
    ) -> (uuid::Uuid, CodingWorktreeHostView) {
        let session = env.host.session_new().expect("agent session");
        env.host
            .coding_worktree_create(Some(session.id), repo, "HEAD", handle)
            .expect("create");
        let view = view(&env.host, handle).expect("view");
        (session.id, view)
    }

    fn assert_command_err_code(err: &str, code: SessionErrorCode) {
        assert!(
            err.starts_with(&format!("{code:?}:")),
            "expected {code:?} host error, got {err}"
        );
    }

    #[test]
    fn missing_session_is_none_and_commands_fail_closed() {
        let env = DesktopHost::new();
        let session = env.host.session_new().expect("agent session");
        assert!(view_for_agent_session(&env.host, session.id)
            .expect("lookup")
            .is_none());
        assert!(view_for_agent_session(&env.host, Uuid::new_v4())
            .expect("unknown agent session")
            .is_none());

        let err = view(&env.host, "missing").expect_err("view");
        assert_command_err_code(&err, SessionErrorCode::InvalidState);
        let err = pause(&env.host, "missing").expect_err("pause");
        assert_command_err_code(&err, SessionErrorCode::InvalidState);
        let err = stop(&env.host, "missing").expect_err("stop");
        assert_command_err_code(&err, SessionErrorCode::InvalidState);
        let err = accept(&env.host, "missing", "/tmp", "sha256:dead").expect_err("accept");
        assert_command_err_code(&err, SessionErrorCode::InvalidState);
        let err = discard(&env.host, "missing").expect_err("discard");
        assert_command_err_code(&err, SessionErrorCode::InvalidState);
        let err = keep_for_review(&env.host, "missing").expect_err("keep");
        assert_command_err_code(&err, SessionErrorCode::InvalidState);
        let err = pause(&env.host, "  ").expect_err("empty handle");
        assert_command_err_code(&err, SessionErrorCode::InvalidState);
        assert!(err.contains("handle is required"));
    }

    #[test]
    fn accept_requires_digest_and_explicit_apply_target() {
        let env = DesktopHost::new();
        let dir = TempDir::new().expect("tempdir");
        init_fixture_repo(dir.path());
        let apply_root = clone_apply_target(dir.path());
        let (session_id, _) = create_bound(&env, dir.path(), "desktop-accept");
        env.host
            .coding_worktree_write_file("desktop-accept", "README.md", "agent edit\n")
            .expect("write");
        let patch = env
            .host
            .coding_worktree_stage_patch("desktop-accept")
            .expect("stage");

        let err =
            accept(&env.host, "desktop-accept", "  ", &patch.digest).expect_err("empty target");
        assert_command_err_code(&err, SessionErrorCode::InvalidState);
        assert!(err.contains("apply_target"));
        let err = accept(
            &env.host,
            "desktop-accept",
            apply_root.path().to_str().unwrap(),
            "  ",
        )
        .expect_err("empty digest");
        assert_command_err_code(&err, SessionErrorCode::InvalidState);
        assert!(err.contains("digest"));

        let err = accept(
            &env.host,
            "desktop-accept",
            apply_root.path().to_str().unwrap(),
            "sha256:wrong",
        )
        .expect_err("digest mismatch");
        assert_command_err_code(&err, SessionErrorCode::PatchDigestMismatch);
        assert_eq!(
            std::fs::read_to_string(apply_root.path().join("README.md")).unwrap(),
            "baseline\n"
        );

        let err = accept(
            &env.host,
            "desktop-accept",
            dir.path().to_str().unwrap(),
            &patch.digest,
        )
        .expect_err("main checkout");
        assert_command_err_code(&err, SessionErrorCode::MainCheckoutProtected);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("README.md")).unwrap(),
            "baseline\n"
        );

        let view = accept(
            &env.host,
            "desktop-accept",
            apply_root.path().to_str().unwrap(),
            &patch.digest,
        )
        .expect("accept");
        assert_eq!(view.phase, SessionPhase::Settled);
        assert_eq!(view.disposition, Some(SessionDisposition::Accepted));
        assert_eq!(
            std::fs::read_to_string(apply_root.path().join("README.md")).unwrap(),
            "agent edit\n"
        );

        let bound = view_for_agent_session(&env.host, session_id)
            .expect("lookup")
            .expect("still bound");
        assert_eq!(bound.disposition, Some(SessionDisposition::Accepted));
        let retry = accept(
            &env.host,
            "desktop-accept",
            apply_root.path().to_str().unwrap(),
            &patch.digest,
        )
        .expect_err("no retry after settle");
        assert_command_err_code(&retry, SessionErrorCode::InvalidState);
    }

    #[test]
    fn pause_fences_settlement_and_stop_remains_legal() {
        let env = DesktopHost::new();
        let dir = TempDir::new().expect("tempdir");
        init_fixture_repo(dir.path());
        let apply_root = clone_apply_target(dir.path());
        let (_session_id, _) = create_bound(&env, dir.path(), "desktop-pause");
        env.host
            .coding_worktree_write_file("desktop-pause", "README.md", "agent edit\n")
            .expect("write");
        let patch = env
            .host
            .coding_worktree_stage_patch("desktop-pause")
            .expect("stage");

        let paused = pause(&env.host, "desktop-pause").expect("pause");
        assert_eq!(paused.phase, SessionPhase::Paused);
        assert!(paused.pause_fenced);

        let err = pause(&env.host, "desktop-pause").expect_err("second pause");
        assert_command_err_code(&err, SessionErrorCode::InvalidState);
        let err = accept(
            &env.host,
            "desktop-pause",
            apply_root.path().to_str().unwrap(),
            &patch.digest,
        )
        .expect_err("accept while paused");
        assert_command_err_code(&err, SessionErrorCode::InvalidState);
        let err = discard(&env.host, "desktop-pause").expect_err("discard while paused");
        assert_command_err_code(&err, SessionErrorCode::InvalidState);
        let err = keep_for_review(&env.host, "desktop-pause").expect_err("keep while paused");
        assert_command_err_code(&err, SessionErrorCode::InvalidState);

        let stopped = stop(&env.host, "desktop-pause").expect("stop");
        assert_eq!(stopped.phase, SessionPhase::Destroyed);
        assert!(stopped.disposition == Some(SessionDisposition::Stopped) || stopped.pause_fenced);
    }

    #[test]
    fn discard_and_keep_for_review_bind_host_disposition() {
        let env = DesktopHost::new();
        let dir = TempDir::new().expect("tempdir");
        init_fixture_repo(dir.path());

        let (_session_id, view) = create_bound(&env, dir.path(), "desktop-discard");
        let worktree = view.worktree_path.clone();
        let discarded = discard(&env.host, "desktop-discard").expect("discard");
        assert_eq!(discarded.phase, SessionPhase::Settled);
        assert_eq!(discarded.disposition, Some(SessionDisposition::Discarded));
        assert!(!worktree.exists());
        let retry = discard(&env.host, "desktop-discard").expect_err("no retry");
        assert_command_err_code(&retry, SessionErrorCode::InvalidState);

        let (_keep_session, _) = create_bound(&env, dir.path(), "desktop-keep");
        env.host
            .coding_worktree_write_file("desktop-keep", "README.md", "keep me\n")
            .expect("write");
        env.host
            .coding_worktree_stage_patch("desktop-keep")
            .expect("stage");
        let kept = keep_for_review(&env.host, "desktop-keep").expect("keep");
        assert_eq!(kept.phase, SessionPhase::Settled);
        assert_eq!(kept.disposition, Some(SessionDisposition::KeptForReview));
        assert!(kept.worktree_path.exists());
        let retry = keep_for_review(&env.host, "desktop-keep").expect_err("no retry");
        assert_command_err_code(&retry, SessionErrorCode::InvalidState);
        let err = pause(&env.host, "desktop-keep").expect_err("pause after keep");
        assert_command_err_code(&err, SessionErrorCode::InvalidState);
    }

    #[test]
    fn uncertain_rejects_auto_retry_and_allows_stop() {
        let env = DesktopHost::new();
        let dir = TempDir::new().expect("tempdir");
        init_fixture_repo(dir.path());
        let apply_root = clone_apply_target(dir.path());
        let (_session_id, _) = create_bound(&env, dir.path(), "desktop-uncertain");
        env.host
            .coding_worktree_detach_live("desktop-uncertain")
            .expect("detach");

        let snap_base = env.snapshot_base("desktop-uncertain");
        let mut snapshot =
            SessionSnapshot::load(grokptah_agent_bridge::snapshot_root(&snap_base)).expect("load");
        snapshot.lifecycle.apply_in_flight = true;
        snapshot.lifecycle.phase = SessionPhase::Settling;
        snapshot
            .save(grokptah_agent_bridge::snapshot_root(&snap_base))
            .expect("save");
        env.host
            .coding_worktree_attach(None, &snap_base)
            .expect("attach recovers");

        let view = view(&env.host, "desktop-uncertain").expect("view");
        assert_eq!(view.disposition, Some(SessionDisposition::Uncertain));
        assert!(view.apply_uncertain);

        let err = pause(&env.host, "desktop-uncertain").expect_err("pause");
        assert_command_err_code(&err, SessionErrorCode::UncertainOutcome);
        let err = accept(
            &env.host,
            "desktop-uncertain",
            apply_root.path().to_str().unwrap(),
            "sha256:any",
        )
        .expect_err("accept");
        assert_command_err_code(&err, SessionErrorCode::UncertainOutcome);
        let err = discard(&env.host, "desktop-uncertain").expect_err("discard");
        assert_command_err_code(&err, SessionErrorCode::UncertainOutcome);
        let err = keep_for_review(&env.host, "desktop-uncertain").expect_err("keep");
        assert_command_err_code(&err, SessionErrorCode::UncertainOutcome);

        let stopped = stop(&env.host, "desktop-uncertain").expect("stop");
        assert!(matches!(
            stopped.phase,
            SessionPhase::Destroyed | SessionPhase::Stopped
        ));
        assert_eq!(
            std::fs::read_to_string(apply_root.path().join("README.md")).unwrap(),
            "baseline\n",
            "uncertain must not auto-accept"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pause_is_refused_after_shutdown() {
        let env = DesktopHost::new();
        let dir = TempDir::new().expect("tempdir");
        init_fixture_repo(dir.path());
        let (_session_id, _) = create_bound(&env, dir.path(), "desktop-shutdown");
        env.runtime.shutdown().await;
        let err = pause(&env.host, "desktop-shutdown").expect_err("pause after shutdown");
        assert!(
            err.to_lowercase().contains("refused") || err.contains("InvalidState"),
            "shutdown must fail closed honestly, got {err}"
        );
    }

    #[test]
    fn desktop_chrome_does_not_flip_admission_or_computer_mode() {
        assert!(!grokptah_agent_bridge::computer_use::isolated_surface_admission_available());
        assert!(!grokptah_agent_bridge::computer_use::computer_use_isolated_surface_admission());
        assert!(SYNTHETIC_SESSION_NONCLAIM.contains("real GrokPtah checkout"));
    }
}
