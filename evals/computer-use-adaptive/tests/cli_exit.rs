use grokptah_cu_adaptive_eval::cli;
use grokptah_cu_adaptive_eval::report::run_campaign;
use grokptah_cu_adaptive_eval::schema::to_canonical_json;
use grokptah_cu_adaptive_eval::types::{CampaignStatus, ProcessVerdict};

fn out_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "grokptah-cu-eval-cli-{}-{}",
        std::process::id(),
        tag
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn shipped_bin() -> &'static str {
    env!("CARGO_BIN_EXE_grokptah-cu-adaptive-eval")
}

fn run_shipped(args: &[&str]) -> std::process::Output {
    std::process::Command::new(shipped_bin())
        .args(args)
        .output()
        .expect("spawn shipped grokptah-cu-adaptive-eval")
}

#[test]
fn repeats_zero_exits_nonzero_before_work() {
    let out = out_dir("repeats0");
    let code = cli::main([
        "grokptah-cu-adaptive-eval".into(),
        "--out".into(),
        out.display().to_string(),
        "--repeats".into(),
        "0".into(),
    ]);
    assert_eq!(code, ProcessVerdict::InvalidRepeats.exit_code());
    assert!(!out.join("campaign-report.json").exists());
}

#[test]
fn shipped_repeats_zero_prints_terminal_invalid_repeats() {
    let out = out_dir("repeats0-bin");
    let output = run_shipped(&["--out", out.to_str().unwrap(), "--repeats", "0"]);
    assert_ne!(output.status.code(), Some(0));
    assert_eq!(
        output.status.code(),
        Some(ProcessVerdict::InvalidRepeats.exit_code())
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("terminal=INVALID_REPEATS"),
        "stdout={stdout:?} stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!out.join("campaign-report.json").exists());
}

#[test]
fn unknown_arg_is_malformed() {
    let code = cli::main(["grokptah-cu-adaptive-eval".into(), "--not-a-flag".into()]);
    assert_eq!(code, ProcessVerdict::Malformed.exit_code());
}

#[test]
fn shipped_unknown_arg_prints_terminal_malformed() {
    let output = run_shipped(&["--not-a-flag"]);
    assert_eq!(
        output.status.code(),
        Some(ProcessVerdict::Malformed.exit_code())
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("terminal=MALFORMED"), "stdout={stdout:?}");
}

#[test]
fn shipped_schema_extra_verify_prints_terminal_malformed() {
    let campaign = run_campaign(1, 435_272).unwrap();
    let json = to_canonical_json(&campaign.report).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("extra".into(), serde_json::json!(true));
    let dir = out_dir("schema-extra-bin");
    std::fs::create_dir_all(&dir).unwrap();
    let report = dir.join("campaign-report.json");
    let evidence = dir.join("campaign-evidence.json");
    std::fs::write(&report, value.to_string()).unwrap();
    std::fs::write(&evidence, to_canonical_json(&campaign.evidence).unwrap()).unwrap();
    let output = run_shipped(&[
        "--verify-report",
        report.to_str().unwrap(),
        "--verify-evidence",
        evidence.to_str().unwrap(),
    ]);
    assert_eq!(
        output.status.code(),
        Some(ProcessVerdict::Malformed.exit_code())
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("terminal=MALFORMED"),
        "stdout={stdout:?} stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn verify_report_gamed_partial_exits_nonzero() {
    let campaign = run_campaign(1, 435_272).unwrap();
    let mut report = campaign.report.clone();
    report.episodes.pop();
    report.status = CampaignStatus::Partial;
    let dir = out_dir("gamed-partial");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("campaign-report.json"),
        to_canonical_json(&report).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join("campaign-evidence.json"),
        to_canonical_json(&campaign.evidence).unwrap(),
    )
    .unwrap();
    let code = cli::main([
        "grokptah-cu-adaptive-eval".into(),
        "--verify-report".into(),
        dir.join("campaign-report.json").display().to_string(),
        "--verify-evidence".into(),
        dir.join("campaign-evidence.json").display().to_string(),
    ]);
    assert_ne!(code, 0, "gamed PARTIAL must not exit 0");
    assert_eq!(code, ProcessVerdict::VerifierError.exit_code());
}

#[test]
fn verify_report_rewritten_metrics_exits_nonzero() {
    let campaign = run_campaign(1, 435_272).unwrap();
    let mut report = campaign.report.clone();
    report.metrics.invalid_actions = report.metrics.invalid_actions.saturating_add(11);
    let dir = out_dir("gamed-metrics");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("campaign-report.json"),
        to_canonical_json(&report).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join("campaign-evidence.json"),
        to_canonical_json(&campaign.evidence).unwrap(),
    )
    .unwrap();
    let code = cli::main([
        "grokptah-cu-adaptive-eval".into(),
        "--verify-report".into(),
        dir.join("campaign-report.json").display().to_string(),
        "--verify-evidence".into(),
        dir.join("campaign-evidence.json").display().to_string(),
    ]);
    assert_ne!(code, 0);
}

#[test]
fn pass_campaign_exits_zero() {
    let out = out_dir("pass1");
    let code = cli::main([
        "grokptah-cu-adaptive-eval".into(),
        "--out".into(),
        out.display().to_string(),
        "--repeats".into(),
        "1".into(),
        "--seed".into(),
        "435272".into(),
    ]);
    assert_eq!(code, 0, "PASS must exit 0");
    let verifier = std::fs::read_to_string(out.join("verifier.json")).unwrap();
    assert!(verifier.contains("\"terminalVerdict\": \"PASS\""));
    assert!(verifier.contains("\"exitCode\": 0"));
}
