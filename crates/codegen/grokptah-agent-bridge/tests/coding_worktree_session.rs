//! Bridge integration for the Coding Worktree Session contract (#288/#286/#267).
//!
//! These tests prove the reversible-code session is reachable from the agent
//! bridge workspace and that AgentHost disposition paths fail closed. They use
//! synthetic temp-dir git fixtures only and never touch the developer's real
//! GrokPtah checkout.

use std::path::{Path, PathBuf};
use std::sync::MutexGuard;

use chrono::Utc;
use grokptah_agent_bridge::{
    home_override_serial, set_grokptah_home_override, snapshot_root, AgentHost,
    CodingWorktreeSession, HostConfig, HostRuntime, SessionDisposition, SessionErrorCode,
    SessionLifecycle, SessionPhase, SessionSnapshot, SYNTHETIC_SESSION_NONCLAIM,
};
use tempfile::TempDir;

fn init_fixture_repo(dir: &Path) {
    let run = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(output.status.success());
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

struct HostEnv {
    host: HostRuntime,
    _home: TempDir,
    _serial: MutexGuard<'static, ()>,
}

impl HostEnv {
    fn new() -> Self {
        let serial = home_override_serial();
        let home = TempDir::new().expect("host home");
        set_grokptah_home_override(Some(home.path().join(".grokptah")));
        let host = AgentHost::create(HostConfig {
            always_approve: true,
            ..HostConfig::default()
        })
        .expect("acquire the GrokPtah instance lock");
        host.start().expect("start host");
        Self {
            host,
            _home: home,
            _serial: serial,
        }
    }

    fn snapshot_base(&self, handle: &str) -> std::path::PathBuf {
        self.host
            .runtime_home()
            .path()
            .join("coding-worktrees")
            .join(handle)
    }
}

impl Drop for HostEnv {
    fn drop(&mut self) {
        let _ = self.host.stop();
        set_grokptah_home_override(None);
    }
}

fn stage_on_host(env: &HostEnv, repo: &Path, handle: &str, agent_session_id: Option<uuid::Uuid>) {
    env.host
        .coding_worktree_create(agent_session_id, repo, "HEAD", handle)
        .expect("host create");
    env.host
        .coding_worktree_write_file(handle, "README.md", "agent edit\n")
        .expect("host write");
}

#[test]
fn bridge_can_create_stage_and_discard_session() {
    assert!(SYNTHETIC_SESSION_NONCLAIM.contains("real GrokPtah checkout"));

    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());

    let mut session = CodingWorktreeSession::create(dir.path(), "HEAD", "bridge-smoke", Utc::now())
        .expect("create");
    session
        .write_worktree_file("README.md", "bridge edit\n")
        .expect("write");
    let patch = session.stage_patch().expect("stage");
    assert!(!patch.bytes.is_empty());

    session.discard(Utc::now()).expect("discard");
    assert_eq!(session.lifecycle().phase, SessionPhase::Settled);
    assert_eq!(
        session.lifecycle().disposition,
        Some(SessionDisposition::Discarded)
    );
}

#[test]
fn bridge_session_identity_records_base_and_worktree_digest() {
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());

    let session = CodingWorktreeSession::create(dir.path(), "HEAD", "identity-test", Utc::now())
        .expect("create");
    let identity = session.identity();
    assert_eq!(identity.session_id, "identity-test");
    assert!(!identity.base_sha.is_empty());
    assert!(identity.worktree_path_digest.starts_with("sha256:"));
    assert!(identity.branch_name.contains("identity-test"));
}

#[test]
fn bridge_pause_fences_then_stop_confirms_destroy() {
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    let mut session = CodingWorktreeSession::create(dir.path(), "HEAD", "pause-stop", Utc::now())
        .expect("create");
    session.pause(Utc::now()).expect("pause");
    assert_eq!(session.lifecycle().phase, SessionPhase::Paused);
    let write_err = session
        .write_worktree_file("README.md", "nope\n")
        .expect_err("paused");
    assert_eq!(write_err.code, SessionErrorCode::InvalidState);
    let evidence = session.stop(Utc::now()).expect("stop");
    assert!(evidence.destroy_confirmed);
    assert_eq!(evidence.phase, SessionPhase::Destroyed);
}

#[test]
fn host_unknown_handle_disposition_fail_closed() {
    let env = HostEnv::new();
    let err = env
        .host
        .coding_worktree_pause("missing")
        .expect_err("pause");
    assert_eq!(err.code, SessionErrorCode::InvalidState);
    let err = env.host.coding_worktree_stop("missing").expect_err("stop");
    assert_eq!(err.code, SessionErrorCode::InvalidState);
    let err = env
        .host
        .coding_worktree_accept("missing", Path::new("/tmp"), "sha256:dead")
        .expect_err("accept");
    assert_eq!(err.code, SessionErrorCode::InvalidState);
    let err = env
        .host
        .coding_worktree_discard("missing")
        .expect_err("discard");
    assert_eq!(err.code, SessionErrorCode::InvalidState);
    let err = env
        .host
        .coding_worktree_keep_for_review("missing")
        .expect_err("keep");
    assert_eq!(err.code, SessionErrorCode::InvalidState);
    let unknown_session = uuid::Uuid::new_v4();
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    let err = env
        .host
        .coding_worktree_create(Some(unknown_session), dir.path(), "HEAD", "no-session")
        .expect_err("unknown agent session");
    assert_eq!(err.code, SessionErrorCode::InvalidState);
    env.host
        .coding_worktree_create(None, dir.path(), "HEAD", "dup")
        .expect("create");
    let dup = env
        .host
        .coding_worktree_create(None, dir.path(), "HEAD", "dup")
        .expect_err("duplicate handle");
    assert_eq!(dup.code, SessionErrorCode::InvalidState);
}

#[test]
fn host_accept_requires_exact_digest_and_never_writes_main() {
    let env = HostEnv::new();
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    stage_on_host(&env, dir.path(), "host-accept", None);
    let patch = env
        .host
        .coding_worktree_stage_patch("host-accept")
        .expect("stage");
    let apply_root = clone_apply_target(dir.path());

    let mismatch = env
        .host
        .coding_worktree_accept("host-accept", apply_root.path(), "sha256:wrong")
        .expect_err("digest mismatch");
    assert_eq!(mismatch.code, SessionErrorCode::PatchDigestMismatch);
    assert_eq!(
        std::fs::read_to_string(apply_root.path().join("README.md")).expect("target"),
        "baseline\n"
    );

    let protected = env
        .host
        .coding_worktree_accept("host-accept", dir.path(), &patch.digest)
        .expect_err("main checkout");
    assert_eq!(protected.code, SessionErrorCode::MainCheckoutProtected);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("README.md")).expect("main"),
        "baseline\n"
    );

    let evidence = env
        .host
        .coding_worktree_accept("host-accept", apply_root.path(), &patch.digest)
        .expect("accept");
    assert_eq!(evidence.patch_digest, patch.digest);
    assert_eq!(
        std::fs::read_to_string(apply_root.path().join("README.md")).expect("applied"),
        "agent edit\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("README.md")).expect("main unchanged"),
        "baseline\n"
    );
    let view = env.host.coding_worktree_view("host-accept").expect("view");
    assert_eq!(view.phase, SessionPhase::Settled);
    assert_eq!(view.disposition, Some(SessionDisposition::Accepted));
}

#[test]
fn host_accept_rejects_host_project_cwd_even_when_not_session_main() {
    let env = HostEnv::new();
    let dir = TempDir::new().expect("session repo");
    init_fixture_repo(dir.path());
    let project = TempDir::new().expect("project cwd");
    init_fixture_repo(project.path());
    env.host
        .set_project_cwd(project.path())
        .expect("set project cwd");

    stage_on_host(&env, dir.path(), "host-cwd", None);
    let patch = env
        .host
        .coding_worktree_stage_patch("host-cwd")
        .expect("stage");
    let err = env
        .host
        .coding_worktree_accept("host-cwd", project.path(), &patch.digest)
        .expect_err("host project cwd protected");
    assert_eq!(err.code, SessionErrorCode::MainCheckoutProtected);
    assert_eq!(
        std::fs::read_to_string(project.path().join("README.md")).expect("project"),
        "baseline\n"
    );
}

#[test]
fn host_accept_rejects_bound_session_cwd_distinct_from_project() {
    let env = HostEnv::new();
    let worktree_repo = TempDir::new().expect("worktree repo");
    init_fixture_repo(worktree_repo.path());
    let project = TempDir::new().expect("project cwd");
    init_fixture_repo(project.path());
    let bound_cwd = TempDir::new().expect("bound session cwd");
    init_fixture_repo(bound_cwd.path());

    env.host
        .set_project_cwd(project.path())
        .expect("set project cwd");
    let bound_session = env.host.session_new().expect("bound session");
    let _active = env.host.session_new().expect("active other session");
    env.host
        .session_set_cwd(bound_session.id, bound_cwd.path())
        .expect("set inactive session cwd");
    let reported = PathBuf::from(env.host.status().project_cwd.expect("project cwd"));
    assert_eq!(
        dunce::canonicalize(&reported).expect("canon reported"),
        dunce::canonicalize(project.path()).expect("canon project")
    );

    stage_on_host(
        &env,
        worktree_repo.path(),
        "host-bound-cwd",
        Some(bound_session.id),
    );
    let patch = env
        .host
        .coding_worktree_stage_patch("host-bound-cwd")
        .expect("stage");
    let err = env
        .host
        .coding_worktree_accept("host-bound-cwd", bound_cwd.path(), &patch.digest)
        .expect_err("bound session cwd protected");
    assert_eq!(err.code, SessionErrorCode::MainCheckoutProtected);
    assert_eq!(
        std::fs::read_to_string(bound_cwd.path().join("README.md")).expect("bound cwd"),
        "baseline\n"
    );
    assert_eq!(
        std::fs::read_to_string(worktree_repo.path().join("README.md")).expect("main"),
        "baseline\n"
    );
}

#[test]
fn host_discard_and_keep_for_review_are_terminal() {
    let env = HostEnv::new();
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    stage_on_host(&env, dir.path(), "host-discard", None);
    let worktree = env
        .host
        .coding_worktree_view("host-discard")
        .expect("view")
        .worktree_path;
    env.host
        .coding_worktree_discard("host-discard")
        .expect("discard");
    assert!(!worktree.exists());
    let view = env
        .host
        .coding_worktree_view("host-discard")
        .expect("view after discard");
    assert_eq!(view.disposition, Some(SessionDisposition::Discarded));
    let retry = env
        .host
        .coding_worktree_discard("host-discard")
        .expect_err("already settled");
    assert_eq!(retry.code, SessionErrorCode::InvalidState);

    stage_on_host(&env, dir.path(), "host-keep", None);
    let patch = env
        .host
        .coding_worktree_stage_patch("host-keep")
        .expect("stage");
    let kept_tree = env
        .host
        .coding_worktree_view("host-keep")
        .expect("view")
        .worktree_path;
    let evidence = env
        .host
        .coding_worktree_keep_for_review("host-keep")
        .expect("keep");
    assert!(kept_tree.exists());
    assert_eq!(
        evidence.patch_digest.as_deref(),
        Some(patch.digest.as_str())
    );
    assert_eq!(
        std::fs::read_to_string(kept_tree.join("README.md")).expect("retained"),
        "agent edit\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("README.md")).expect("main"),
        "baseline\n"
    );
}

#[test]
fn host_pause_fences_staging_and_settlement_stop_still_legal() {
    let env = HostEnv::new();
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    stage_on_host(&env, dir.path(), "host-pause", None);
    let patch = env
        .host
        .coding_worktree_stage_patch("host-pause")
        .expect("stage");
    let apply_root = clone_apply_target(dir.path());

    env.host.coding_worktree_pause("host-pause").expect("pause");
    let view = env.host.coding_worktree_view("host-pause").expect("view");
    assert_eq!(view.phase, SessionPhase::Paused);
    assert!(view.pause_fenced);

    let write_err = env
        .host
        .coding_worktree_write_file("host-pause", "README.md", "paused\n")
        .expect_err("write fenced");
    assert_eq!(write_err.code, SessionErrorCode::InvalidState);
    let stage_err = env
        .host
        .coding_worktree_stage_patch("host-pause")
        .expect_err("stage fenced");
    assert_eq!(stage_err.code, SessionErrorCode::InvalidState);
    let accept_err = env
        .host
        .coding_worktree_accept("host-pause", apply_root.path(), &patch.digest)
        .expect_err("accept fenced");
    assert_eq!(accept_err.code, SessionErrorCode::InvalidState);
    let discard_err = env
        .host
        .coding_worktree_discard("host-pause")
        .expect_err("discard fenced");
    assert_eq!(discard_err.code, SessionErrorCode::InvalidState);
    let keep_err = env
        .host
        .coding_worktree_keep_for_review("host-pause")
        .expect_err("keep fenced");
    assert_eq!(keep_err.code, SessionErrorCode::InvalidState);

    let evidence = env.host.coding_worktree_stop("host-pause").expect("stop");
    assert!(evidence.destroy_confirmed);
    assert_eq!(evidence.phase, SessionPhase::Destroyed);
    assert_eq!(evidence.disposition, Some(SessionDisposition::Stopped));
}

#[test]
fn host_stop_records_destroyed_only_when_destroy_confirmed() {
    let env = HostEnv::new();
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    env.host
        .coding_worktree_create(None, dir.path(), "HEAD", "host-stop")
        .expect("create");
    env.host
        .coding_worktree_fail_next_destroy_for_test("host-stop")
        .expect("inject");
    let evidence = env
        .host
        .coding_worktree_stop("host-stop")
        .expect("stop unconfirmed");
    assert!(!evidence.destroy_confirmed);
    assert_eq!(evidence.phase, SessionPhase::Stopped);
    assert_ne!(evidence.phase, SessionPhase::Destroyed);
    let view = env.host.coding_worktree_view("host-stop").expect("view");
    assert_eq!(view.phase, SessionPhase::Stopped);
    assert!(view.worktree_path.exists());
}

fn assert_host_fenced(env: &HostEnv, handle: &str, apply_target: &Path, digest: &str) {
    let pause_err = env.host.coding_worktree_pause(handle).expect_err("pause");
    assert_eq!(pause_err.code, SessionErrorCode::InvalidState);
    let accept_err = env
        .host
        .coding_worktree_accept(handle, apply_target, digest)
        .expect_err("accept");
    assert_eq!(accept_err.code, SessionErrorCode::InvalidState);
    let discard_err = env
        .host
        .coding_worktree_discard(handle)
        .expect_err("discard");
    assert_eq!(discard_err.code, SessionErrorCode::InvalidState);
    let keep_err = env
        .host
        .coding_worktree_keep_for_review(handle)
        .expect_err("keep");
    assert_eq!(keep_err.code, SessionErrorCode::InvalidState);
}

#[test]
fn host_attach_after_settlement_cannot_retry() {
    let env = HostEnv::new();
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    let apply_root = clone_apply_target(dir.path());

    stage_on_host(&env, dir.path(), "host-reattach-accept", None);
    let patch = env
        .host
        .coding_worktree_stage_patch("host-reattach-accept")
        .expect("stage");
    env.host
        .coding_worktree_accept("host-reattach-accept", apply_root.path(), &patch.digest)
        .expect("accept");
    let snap = env.snapshot_base("host-reattach-accept");
    env.host
        .coding_worktree_detach_live("host-reattach-accept")
        .expect("detach");
    env.host
        .coding_worktree_attach(None, &snap)
        .expect("attach after accept");
    let view = env
        .host
        .coding_worktree_view("host-reattach-accept")
        .expect("view");
    assert_eq!(view.disposition, Some(SessionDisposition::Accepted));
    assert!(view.settlement_fenced);
    assert!(view.pause_fenced);
    assert_host_fenced(
        &env,
        "host-reattach-accept",
        apply_root.path(),
        &patch.digest,
    );

    stage_on_host(&env, dir.path(), "host-reattach-discard", None);
    env.host
        .coding_worktree_discard("host-reattach-discard")
        .expect("discard");
    let snap = env.snapshot_base("host-reattach-discard");
    env.host
        .coding_worktree_detach_live("host-reattach-discard")
        .expect("detach");
    env.host
        .coding_worktree_attach(None, &snap)
        .expect("attach after discard");
    assert_host_fenced(
        &env,
        "host-reattach-discard",
        apply_root.path(),
        &patch.digest,
    );

    stage_on_host(&env, dir.path(), "host-reattach-keep", None);
    env.host
        .coding_worktree_stage_patch("host-reattach-keep")
        .expect("stage");
    env.host
        .coding_worktree_keep_for_review("host-reattach-keep")
        .expect("keep");
    let snap = env.snapshot_base("host-reattach-keep");
    env.host
        .coding_worktree_detach_live("host-reattach-keep")
        .expect("detach");
    env.host
        .coding_worktree_attach(None, &snap)
        .expect("attach after keep");
    assert_host_fenced(&env, "host-reattach-keep", apply_root.path(), &patch.digest);
}

#[test]
fn host_uncertain_rejects_auto_retry_stop_remains_legal() {
    let env = HostEnv::new();
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    env.host
        .coding_worktree_create(None, dir.path(), "HEAD", "host-uncertain")
        .expect("create");
    let snap_base = env.snapshot_base("host-uncertain");
    env.host
        .coding_worktree_detach_live("host-uncertain")
        .expect("detach");

    let loaded = SessionSnapshot::load(snapshot_root(&snap_base)).expect("load");
    let mut lifecycle = SessionLifecycle::new(Utc::now());
    lifecycle.begin_settlement(Utc::now()).unwrap();
    lifecycle.mark_apply_in_flight(Utc::now()).unwrap();
    lifecycle.mark_apply_uncertain(Utc::now()).unwrap();
    let snapshot = SessionSnapshot::new(
        loaded.identity,
        lifecycle,
        loaded.patch,
        loaded.main_checkout_digest,
        loaded.worktree_path,
        loaded.repo_root,
    );
    snapshot
        .save(snapshot_root(&snap_base))
        .expect("save uncertain");

    env.host
        .coding_worktree_attach(None, &snap_base)
        .expect("attach uncertain");
    let pause_err = env
        .host
        .coding_worktree_pause("host-uncertain")
        .expect_err("pause");
    assert_eq!(pause_err.code, SessionErrorCode::UncertainOutcome);
    let apply_root = clone_apply_target(dir.path());
    let accept_err = env
        .host
        .coding_worktree_accept("host-uncertain", apply_root.path(), "sha256:any")
        .expect_err("accept");
    assert_eq!(accept_err.code, SessionErrorCode::UncertainOutcome);
    let discard_err = env
        .host
        .coding_worktree_discard("host-uncertain")
        .expect_err("discard");
    assert_eq!(discard_err.code, SessionErrorCode::UncertainOutcome);
    let write_err = env
        .host
        .coding_worktree_write_file("host-uncertain", "README.md", "retry\n")
        .expect_err("write");
    assert_eq!(write_err.code, SessionErrorCode::InvalidState);
    let stage_err = env
        .host
        .coding_worktree_stage_patch("host-uncertain")
        .expect_err("stage");
    assert_eq!(stage_err.code, SessionErrorCode::InvalidState);
    let keep_err = env
        .host
        .coding_worktree_keep_for_review("host-uncertain")
        .expect_err("keep");
    assert_eq!(keep_err.code, SessionErrorCode::UncertainOutcome);

    let stop = env
        .host
        .coding_worktree_stop("host-uncertain")
        .expect("stop still legal");
    assert!(
        stop.disposition == Some(SessionDisposition::Uncertain)
            || stop.disposition == Some(SessionDisposition::Stopped)
    );
}

#[test]
fn host_session_delete_stops_bound_coding_worktree() {
    let env = HostEnv::new();
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    env.host.set_project_cwd(dir.path()).expect("cwd");
    let session = env.host.session_new().expect("agent session");
    env.host
        .coding_worktree_create(Some(session.id), dir.path(), "HEAD", "host-bound")
        .expect("create bound");
    let worktree = env
        .host
        .coding_worktree_view("host-bound")
        .expect("view")
        .worktree_path;
    env.host.session_delete(session.id).expect("delete");
    let view = env.host.coding_worktree_view("host-bound").expect("view");
    assert_eq!(view.phase, SessionPhase::Destroyed);
    assert!(!worktree.exists());
    assert!(env
        .host
        .coding_worktree_handle_for_session(session.id)
        .is_err());
}

#[test]
fn host_session_delete_rejects_unconfirmed_destroy() {
    let env = HostEnv::new();
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    env.host.set_project_cwd(dir.path()).expect("cwd");
    let session = env.host.session_new().expect("agent session");
    env.host
        .coding_worktree_create(Some(session.id), dir.path(), "HEAD", "host-unc")
        .expect("create bound");
    env.host
        .coding_worktree_fail_next_destroy_for_test("host-unc")
        .expect("inject");
    env.host
        .session_delete(session.id)
        .expect_err("unconfirmed destroy");
    let view = env.host.coding_worktree_view("host-unc").expect("view");
    assert_eq!(view.phase, SessionPhase::Stopped);
    assert_ne!(view.phase, SessionPhase::Destroyed);
    assert!(view.worktree_path.exists());
    env.host.session_load(session.id).expect("session retained");
}

#[test]
fn host_session_archive_pauses_bound_coding_worktree() {
    let env = HostEnv::new();
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    env.host.set_project_cwd(dir.path()).expect("cwd");
    let session = env.host.session_new().expect("agent session");
    stage_on_host(&env, dir.path(), "host-archive", Some(session.id));
    env.host.session_archive(session.id, true).expect("archive");
    let view = env.host.coding_worktree_view("host-archive").expect("view");
    assert_eq!(view.phase, SessionPhase::Paused);
    let write_err = env
        .host
        .coding_worktree_write_file("host-archive", "README.md", "archived\n")
        .expect_err("write fenced");
    assert_eq!(write_err.code, SessionErrorCode::InvalidState);
    let evidence = env
        .host
        .coding_worktree_stop("host-archive")
        .expect("stop still legal");
    assert!(evidence.destroy_confirmed);
}

#[test]
fn host_attach_recovers_hostile_active_apply_in_flight() {
    let env = HostEnv::new();
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    env.host
        .coding_worktree_create(None, dir.path(), "HEAD", "host-hostile")
        .expect("create");
    let snap_base = env.snapshot_base("host-hostile");
    env.host
        .coding_worktree_detach_live("host-hostile")
        .expect("detach");

    let loaded = SessionSnapshot::load(snapshot_root(&snap_base)).expect("load");
    let mut lifecycle = SessionLifecycle::new(Utc::now());
    lifecycle.phase = SessionPhase::Active;
    lifecycle.apply_in_flight = true;
    lifecycle.disposition = None;
    lifecycle.settlement_fenced = false;
    lifecycle.pause_fenced = false;
    let snapshot = SessionSnapshot::new(
        loaded.identity,
        lifecycle,
        loaded.patch,
        loaded.main_checkout_digest,
        loaded.worktree_path,
        loaded.repo_root,
    );
    snapshot
        .save(snapshot_root(&snap_base))
        .expect("save hostile");

    env.host
        .coding_worktree_attach(None, &snap_base)
        .expect("attach recovers");
    let view = env.host.coding_worktree_view("host-hostile").expect("view");
    assert_eq!(view.disposition, Some(SessionDisposition::Uncertain));
    assert_eq!(view.phase, SessionPhase::Settled);
    assert!(view.settlement_fenced);
    assert!(view.pause_fenced);

    let apply_root = clone_apply_target(dir.path());
    let accept_err = env
        .host
        .coding_worktree_accept("host-hostile", apply_root.path(), "sha256:any")
        .expect_err("no auto-retry");
    assert_eq!(accept_err.code, SessionErrorCode::UncertainOutcome);
    let pause_err = env
        .host
        .coding_worktree_pause("host-hostile")
        .expect_err("pause");
    assert_eq!(pause_err.code, SessionErrorCode::UncertainOutcome);
    let write_err = env
        .host
        .coding_worktree_write_file("host-hostile", "README.md", "retry\n")
        .expect_err("write");
    assert_eq!(write_err.code, SessionErrorCode::InvalidState);

    env.host
        .coding_worktree_create(None, dir.path(), "HEAD", "host-settling")
        .expect("create settling");
    let settling_base = env.snapshot_base("host-settling");
    env.host
        .coding_worktree_detach_live("host-settling")
        .expect("detach settling");
    let loaded = SessionSnapshot::load(snapshot_root(&settling_base)).expect("load settling");
    let mut lifecycle = SessionLifecycle::new(Utc::now());
    lifecycle.begin_settlement(Utc::now()).unwrap();
    lifecycle.mark_apply_in_flight(Utc::now()).unwrap();
    let snapshot = SessionSnapshot::new(
        loaded.identity,
        lifecycle,
        loaded.patch,
        loaded.main_checkout_digest,
        loaded.worktree_path,
        loaded.repo_root,
    );
    snapshot
        .save(snapshot_root(&settling_base))
        .expect("save settling");
    env.host
        .coding_worktree_attach(None, &settling_base)
        .expect("attach settling recovers");
    let view = env
        .host
        .coding_worktree_view("host-settling")
        .expect("view settling");
    assert_eq!(view.disposition, Some(SessionDisposition::Uncertain));
    let accept_err = env
        .host
        .coding_worktree_accept("host-settling", apply_root.path(), "sha256:any")
        .expect_err("settling no auto-retry");
    assert_eq!(accept_err.code, SessionErrorCode::UncertainOutcome);
}

#[test]
fn host_does_not_enable_isolated_surface_or_computer_mode() {
    assert!(!grokptah_agent_bridge::computer_use::isolated_surface_admission_available());
    assert!(!grokptah_agent_bridge::computer_use::computer_use_isolated_surface_admission());
    assert!(SYNTHETIC_SESSION_NONCLAIM.contains("real GrokPtah checkout"));
}
