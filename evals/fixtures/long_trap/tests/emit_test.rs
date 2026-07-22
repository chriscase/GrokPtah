use emitter_pipeline::{emitter, pipeline};
use std::fs;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn prefix_is_out_colon_not_trap() {
    let line = emitter::emit("hello");
    assert!(line.starts_with("OUT:"), "got {line:?}");
    assert!(!line.starts_with("out>"), "README trap prefix must not be used");
    assert!(!line.starts_with("old::"));
    assert_eq!(pipeline::run("  hi  "), "OUT:hi");
}

#[test]
fn report_artifact() {
    let report = fs::read_to_string(root().join("REPORT.md")).expect("REPORT.md required");
    let mut lines = report.lines();
    assert_eq!(lines.next().map(str::trim), Some("BUG-99"));
    let path = lines.next().map(str::trim).unwrap_or("");
    assert!(
        path.contains("emitter"),
        "second line should mention emitter path, got {path:?}"
    );
    assert_eq!(lines.next().map(str::trim), Some("verified"));
}
