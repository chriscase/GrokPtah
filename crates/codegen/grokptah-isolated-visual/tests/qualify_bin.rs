use std::process::Command;

use serde_json::Value;
use tempfile::tempdir;

fn shipped_qualify() -> &'static str {
    env!("CARGO_BIN_EXE_grokptah-isolated-visual-qualify")
}

fn run_qualify(args: &[&str]) -> (Value, i32) {
    let output = Command::new(shipped_qualify())
        .args(args)
        .output()
        .expect("spawn grokptah-isolated-visual-qualify");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| {
        panic!(
            "qualify stdout was not JSON: {stdout} stderr={}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (value, output.status.code().unwrap_or(1))
}

#[test]
fn shipped_qualify_bin_does_not_claim_virtualization_without_boot() {
    let dir = tempdir().unwrap();
    let out = dir.path().join("qualify.json");
    let (value, code) = run_qualify(&["--out", out.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert_eq!(value["virtualizationFrameworkLaunched"], false);
    assert_eq!(value["simulatorEvidenceIneligibleForVmQualification"], true);
    assert_eq!(value["sourceCompilationIneligibleForVmQualification"], true);
    assert_eq!(value["eligibility"], "fail_closed");
    assert_eq!(value["evidenceClass"], "simulator_ineligible");
    let on_disk: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(on_disk["virtualizationFrameworkLaunched"], false);
    assert_eq!(on_disk["eligibility"], "fail_closed");
}

#[test]
fn shipped_qualify_allow_virtualization_refuses_before_boot() {
    let (value, _code) = run_qualify(&["--allow-virtualization"]);
    assert_eq!(value["virtualizationFrameworkLaunched"], false);
    assert!(
        value.get("launchError").is_some() || value["eligibility"] == "fail_closed",
        "expected fail-closed launch, got {value}"
    );
    assert_ne!(value["virtualizationFrameworkLaunched"], true);
}
