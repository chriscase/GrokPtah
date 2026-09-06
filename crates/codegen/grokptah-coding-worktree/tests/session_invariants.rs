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

fn clone_apply_target(source_repo: &std::path::Path) -> TempDir {
    let apply_root = TempDir::new().expect("apply tempdir");
    let output = std::process::Command::new("git")
        .args(["clone", "-q"])
        .arg(source_repo)
        .arg(apply_root.path())
        .output()
        .expect("clone apply target");
    assert!(output.status.success(), "{}", stderr(&output));
    apply_root
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

    let apply_root = clone_apply_target(dir.path());
    let apply_target = apply_root.path();

    let main_before = std::fs::read_to_string(dir.path().join("README.md")).expect("main read");
    let now = Utc::now();
    let evidence = session
        .accept(apply_target, &patch.digest, now)
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
    let apply_root = clone_apply_target(dir.path());
    let apply_target = apply_root.path();

    let err = session
        .accept(apply_target, "sha256:wrong", Utc::now())
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
    let apply_root = clone_apply_target(dir.path());
    let retry_err = restored
        .accept(apply_root.path(), "sha256:any", Utc::now())
        .expect_err("no auto-retry accept");
    assert_eq!(retry_err.code, SessionErrorCode::UncertainOutcome);
}

#[test]
fn write_worktree_file_rejects_absolute_main_path() {
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    let main_readme = dir.path().join("README.md");
    let session = session_with_snapshot(&dir, "HEAD");

    let err = session
        .write_worktree_file(main_readme.to_str().expect("utf8"), "hostile\n")
        .expect_err("absolute main path");
    assert_eq!(err.code, SessionErrorCode::MainCheckoutProtected);
    assert_eq!(
        std::fs::read_to_string(main_readme).expect("main unchanged"),
        "baseline\n"
    );
}

#[test]
fn write_worktree_file_rejects_parent_dir_escape_to_main() {
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    let main_readme = dir.path().join("README.md");
    let session = session_with_snapshot(&dir, "HEAD");

    let err = session
        .write_worktree_file("../../../README.md", "hostile\n")
        .expect_err("parent escape");
    assert_eq!(err.code, SessionErrorCode::MainCheckoutProtected);
    assert_eq!(
        std::fs::read_to_string(main_readme).expect("main unchanged"),
        "baseline\n"
    );
}

#[test]
fn accept_rejects_apply_target_under_main_checkout() {
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    let mut session = session_with_snapshot(&dir, "HEAD");
    session
        .write_worktree_file("README.md", "agent edit\n")
        .expect("write");
    let patch = session.stage_patch().expect("stage");

    let nested = dir.path().join(".grokptah").join("nested-target");
    std::fs::create_dir_all(&nested).expect("nested");

    let err = session
        .accept(&nested, &patch.digest, Utc::now())
        .expect_err("nested under main");
    assert_eq!(err.code, SessionErrorCode::MainCheckoutProtected);
}

#[test]
fn accept_rejects_tampered_patch_bytes_even_with_stored_digest() {
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    let mut session = session_with_snapshot(&dir, "HEAD");
    session
        .write_worktree_file("README.md", "agent edit\n")
        .expect("write");
    let patch = session.stage_patch().expect("stage");
    let apply_root = clone_apply_target(dir.path());
    let apply_target = apply_root.path();

    let snapshot = session.to_snapshot();
    let mut tampered = snapshot.patch.expect("patch");
    tampered.bytes.push(b'\n');
    let restored = SessionSnapshot::new(
        snapshot.identity,
        snapshot.lifecycle,
        Some(tampered),
        snapshot.main_checkout_digest,
        snapshot.worktree_path,
        snapshot.repo_root,
    );
    let mut session = CodingWorktreeSession::restore_from_snapshot(restored).expect("restore");

    let err = session
        .accept(apply_target, &patch.digest, Utc::now())
        .expect_err("tampered bytes");
    assert_eq!(err.code, SessionErrorCode::PatchDigestMismatch);
}

#[test]
fn stage_patch_includes_untracked_files() {
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    let mut session = session_with_snapshot(&dir, "HEAD");
    session
        .write_worktree_file("new-file.txt", "brand new\n")
        .expect("write new file");

    let patch = session.stage_patch().expect("stage");
    assert!(patch.paths.iter().any(|path| path == "new-file.txt"));
    assert!(patch.bytes.windows(9).any(|window| window == b"brand new"));
}

#[test]
fn discard_clears_git_worktree_registration() {
    let dir = TempDir::new().expect("tempdir");
    init_fixture_repo(dir.path());
    let mut session = session_with_snapshot(&dir, "HEAD");
    let worktree = session.worktree_path().to_path_buf();

    session.discard(Utc::now()).expect("discard");
    assert!(!worktree.exists());

    let list = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(dir.path())
        .output()
        .expect("list");
    assert!(list.status.success());
    let listed = String::from_utf8_lossy(&list.stdout);
    assert!(
        !listed.lines().any(|line| {
            line.strip_prefix("worktree ")
                .is_some_and(|path| path.contains("run-test-session"))
        }),
        "discard left worktree registered: {listed}"
    );
}

#[test]
fn nonclaim_documents_synthetic_harness_only() {
    assert!(SYNTHETIC_SESSION_NONCLAIM.contains("developer"));
    assert!(SYNTHETIC_SESSION_NONCLAIM.contains("real"));
}
