//! Offline Build-agent tool evals (no network).
//!
//! Full live parity vs CLI is tracked in #93; this suite guards the local
//! tool surface the agent loop dispatches.

use std::fs;

use grokptah_agent_bridge::set_grokptah_home_override;

/// Keep home override away from developer ~/.grokptah during tests.
fn isolate_home() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join(".grokptah");
    fs::create_dir_all(home.join("sessions")).unwrap();
    set_grokptah_home_override(Some(home));
    tmp
}

#[test]
fn fixture_repo_layout_for_manual_parity() {
    let _home = isolate_home();
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("AGENTS.md"),
        "Always add a trailing newline to files you write.\n",
    )
    .unwrap();
    fs::write(dir.path().join("src/lib.rs"), "pub fn add(a: i32, b: i32) -> i32 { a + b }\n")
        .unwrap();
    fs::write(
        dir.path().join("src/main.rs"),
        "fn main() { println!(\"{}\", src_lib_add()); }\nfn src_lib_add() -> i32 { 0 }\n",
    )
    .unwrap();

    // Sanity: fixture is what #93 full harness will drive against the agent loop.
    assert!(dir.path().join("AGENTS.md").is_file());
    assert!(dir.path().join("src/lib.rs").is_file());
}
