//! Bridge integration for the Coding Worktree Session contract (#288/#286/#267).
//!
//! These tests prove the reversible-code session is reachable from the agent
//! bridge workspace. They use synthetic temp-dir git fixtures only and never
//! touch the developer's real GrokPtah checkout.

use chrono::Utc;
use grokptah_agent_bridge::{
    CodingWorktreeSession, SessionDisposition, SessionPhase, SYNTHETIC_SESSION_NONCLAIM,
};
use tempfile::TempDir;

fn init_fixture_repo(dir: &std::path::Path) {
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
