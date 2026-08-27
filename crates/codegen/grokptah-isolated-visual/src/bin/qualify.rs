use std::env;
use std::fs;
use std::path::PathBuf;

use grokptah_isolated_visual::{IsolatedEvidenceClass, IsolatedPreflight};

fn main() {
    let mut evidence = serde_json::json!({
        "schemaVersion": 1,
        "kind": "isolatedVisualQualification",
        "baseSha": "67e29bd34dc64049432c715c93c2cef2185c63ea",
        "issue": 288,
        "virtualizationFrameworkLaunched": false,
        "simulatorEvidenceIneligibleForVmQualification": true,
        "sourceCompilationIneligibleForVmQualification": true,
    });

    let preflight = IsolatedPreflight::inspect(None).expect("preflight");
    evidence["preflight"] = serde_json::to_value(&preflight).expect("preflight json");
    evidence["eligibility"] = if preflight.allowed_to_launch {
        serde_json::json!("partial")
    } else {
        serde_json::json!("fail_closed")
    };
    evidence["evidenceClass"] = serde_json::json!(IsolatedEvidenceClass::SimulatorIneligible);
    evidence["continuation"] = serde_json::json!({
        "blockedOn": preflight.deny_reason,
        "command": "CARGO_TARGET_DIR=/tmp/grokptah-isolated-visual-target cargo test --locked --manifest-path crates/codegen/grokptah-isolated-visual/Cargo.toml && cargo run --locked --manifest-path crates/codegen/grokptah-isolated-visual/Cargo.toml --bin grokptah-isolated-visual-qualify -- --allow-virtualization"
    });

    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--allow-virtualization") {
        match preflight.fail_closed_launch() {
            Ok(()) => {
                evidence["eligibility"] = serde_json::json!("partial");
                evidence["note"] = serde_json::json!(
                    "launch gate passed but this binary still does not boot a VM; hardware boot remains a later exact-head qualification"
                );
            }
            Err(error) => {
                evidence["launchError"] = serde_json::json!(error.to_string());
            }
        }
    }

    let encoded = serde_json::to_string_pretty(&evidence).expect("encode");
    println!("{encoded}");
    if let Some(out) = args
        .iter()
        .position(|arg| arg == "--out")
        .and_then(|index| args.get(index + 1))
    {
        let path = PathBuf::from(out);
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(path, encoded).expect("write evidence");
    }
}
