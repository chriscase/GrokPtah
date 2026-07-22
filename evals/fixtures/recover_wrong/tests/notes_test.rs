use std::fs;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn notes_md_has_correct_secret_only() {
    let notes = root().join("NOTES.md");
    let body = fs::read_to_string(&notes).expect("NOTES.md must exist at project root");
    let trimmed = body.trim();
    assert_eq!(trimmed, notes_tool::expected_secret());
    assert!(!body.contains("WRONG"));
}

#[test]
fn protected_untouched() {
    let p = root().join("src/protected.txt");
    let body = fs::read_to_string(p).unwrap();
    assert!(body.contains("DO NOT OVERWRITE"));
    assert!(body.contains("secret-value-omega"));
}

#[test]
fn no_wrong_leak_files() {
    // Recovery: remove mistaken artifacts from the prior attempt.
    assert!(
        !root().join("NOTES_draft.md").exists(),
        "NOTES_draft.md must be deleted (wrong prior attempt)"
    );
    assert!(
        !root().join("src/leaked_secret.txt").exists(),
        "src/leaked_secret.txt must be deleted"
    );
}
