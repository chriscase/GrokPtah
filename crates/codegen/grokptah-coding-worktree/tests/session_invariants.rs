//! Fail-closed invariant regression suite for CodingWorktreeSession.
//!
//! All tests use synthetic temp-dir git fixtures. They never touch the
//! developer's real GrokPtah checkout.

use chrono::Utc;
use grokptah_coding_worktree::{
    digest_path, snapshot_root, CodingWorktreeSession, SessionDisposition, SessionErrorCode,
    SessionLifecycle, SessionPhase, SessionSnapshot, SYNTHETIC_SESSION_NONCLAIM,
};
use tempfile::TempDir;

fn init_fixture_repo(dir: &std::path::Path) {
    let run = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            stderr(&output)
        );
    };
    run(&["init", "-q"]);
    run(&["config", "user.name", "GrokPtah test"]);
    run(&["config", "user.email", "test@example.invalid"]);
    std::fs::write(dir.join("README.md"), "baseline\n").expect("write");
    run(&["add", "README.md"]);
    run(&["commit", "-qm", "initial"]);
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

fn session_with_snapshot(dir: &TempDir, base_sha: &str) -> CodingWorktreeSession {
    let now = Utc::now();
    CodingWorktreeSession::create(dir.path(), base_sha, "test-session", now)
        .expect("create session")
        .with_snapshot_root(dir.path().join("snapshots"))
}

#[test]
fn accept_applies_exact_patch_to_explicit_target_not_main() {
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    let base_sha = "HEAD";
    let mut session = session_with_snapshot(&dir, base_sha);
    session
        .write_worktree_file("README.md", "agent edit\n")
        .expect("write");
    let patch = session.stage_patch().expect("stage");

    let apply_target = dir.path().join("apply-target");
    let output = std::process::Command::new("git")
        .args(["clone", "-q"])
        .arg(dir.path())
        .arg(&apply_target)
        .output()
        .expect("clone apply target");
    assert!(output.status.success());

    let main_before = std::fs::read_to_string(dir.path().join("README.md")).expect("main read");
    let now = Utc::now();
    let evidence = session
        .accept(&apply_target, &patch.digest, now)
        .expect("accept");
    assert_eq!(evidence.patch_digest, patch.digest);
    assert_eq!(
        std::fs::read_to_string(apply_target.join("README.md")).expect("target read"),
        "agent edit\n"
    );
    assert_eq!(
        main_before,
        std::fs::read_to_string(dir.path().join("README.md")).expect("main still baseline")
    );
    assert_eq!(session.lifecycle().phase, SessionPhase::Settled);
    assert_eq!(
        session.lifecycle().disposition,
        Some(SessionDisposition::Accepted)
    );
    assert!(!session.worktree_path().exists());
}

#[test]
fn main_checkout_never_written_by_session_helpers() {
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    let mut session = session_with_snapshot(&dir, "HEAD");
    session
        .write_worktree_file("README.md", "worktree only\n")
        .expect("worktree write");

    let err = session
        .accept(dir.path(), "sha256:deadbeef", Utc::now())
        .expect_err("accept to main forbidden");
    assert_eq!(err.code, SessionErrorCode::MainCheckoutProtected);

    let main_contents = std::fs::read_to_string(dir.path().join("README.md")).expect("main read");
    assert_eq!(main_contents, "baseline\n");
}

#[test]
fn accept_requires_exact_patch_digest_match() {
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    let mut session = session_with_snapshot(&dir, "HEAD");
    session
        .write_worktree_file("README.md", "x\n")
        .expect("write");
    let patch = session.stage_patch().expect("stage");
    let apply_target = dir.path().join("apply-target");
    std::fs::create_dir_all(&apply_target).expect("target");
    std::fs::write(apply_target.join("README.md"), "baseline\n").expect("seed");

    let err = session
        .accept(&apply_target, "sha256:wrong", Utc::now())
        .expect_err("digest mismatch");
    assert_eq!(err.code, SessionErrorCode::PatchDigestMismatch);
    assert_eq!(
        std::fs::read_to_string(apply_target.join("README.md")).expect("target unchanged"),
        "baseline\n"
    );
    assert_eq!(patch.digest, session.patch().expect("patch").digest);
}

#[test]
fn discard_removes_active_worktree() {
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    let mut session = session_with_snapshot(&dir, "HEAD");
    let worktree = session.worktree_path().to_path_buf();
    assert!(worktree.exists());

    session.discard(Utc::now()).expect("discard");
    assert!(!worktree.exists());
    assert_eq!(
        session.lifecycle().disposition,
        Some(SessionDisposition::Discarded)
    );
}

#[test]
fn keep_for_review_retains_worktree_and_patch_without_apply() {
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    let mut session = session_with_snapshot(&dir, "HEAD");
    session
        .write_worktree_file("README.md", "review me\n")
        .expect("write");
    let patch = session.stage_patch().expect("stage");
    let worktree = session.worktree_path().to_path_buf();

    let evidence = session.keep_for_review(Utc::now()).expect("keep");
    assert!(worktree.exists());
    assert_eq!(evidence.patch_digest, Some(patch.digest));
    assert_eq!(
        session.lifecycle().disposition,
        Some(SessionDisposition::KeptForReview)
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("README.md")).expect("worktree retained"),
        "review me\n"
    );
}

#[test]
fn restart_with_apply_in_flight_becomes_uncertain_no_auto_accept() {
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    let snap_root = dir.path().join("snapshots");
    let session = CodingWorktreeSession::create(dir.path(), "HEAD", "restart-test", Utc::now())
        .expect("create")
        .with_snapshot_root(&snap_root);
    let identity = session.identity().clone();
    let patch = session.patch().cloned();
    let worktree_path = session.worktree_path().to_path_buf();
    let main_digest = digest_path(dir.path()).expect("digest");

    let mut lifecycle = SessionLifecycle::new(Utc::now());
    lifecycle.begin_settlement(Utc::now()).unwrap();
    lifecycle.mark_apply_in_flight(Utc::now()).unwrap();

    let snapshot = SessionSnapshot::new(
        identity,
        lifecycle,
        patch,
        main_digest,
        worktree_path,
        dir.path().to_path_buf(),
    );
    snapshot
        .save(snapshot_root(&snap_root))
        .expect("save interrupted snapshot");

    let loaded =
        SessionSnapshot::load(snapshot_root(&snap_root)).expect("load interrupted snapshot");
    let mut restored = CodingWorktreeSession::restore_from_snapshot(loaded).expect("restore");
    restored.recover_after_restart(Utc::now()).expect("recover");

    assert_eq!(
        restored.lifecycle().disposition,
        Some(SessionDisposition::Uncertain)
    );
    let apply_target = dir.path().join("apply-target");
    std::fs::create_dir_all(&apply_target).expect("target");
    let retry_err = restored
        .accept(&apply_target, "sha256:any", Utc::now())
        .expect_err("no auto-retry accept");
    assert_eq!(retry_err.code, SessionErrorCode::UncertainOutcome);
}

#[test]
fn nonclaim_documents_synthetic_harness_only() {
    assert!(SYNTHETIC_SESSION_NONCLAIM.contains("developer"));
    assert!(SYNTHETIC_SESSION_NONCLAIM.contains("real"));
}
