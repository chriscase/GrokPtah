use grokptah_cu_adaptive_eval::cli;
use grokptah_cu_adaptive_eval::types::ProcessVerdict;

fn out_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "grokptah-cu-eval-cli-{}-{}",
        std::process::id(),
        tag
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
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
fn unknown_arg_is_malformed() {
    let code = cli::main(["grokptah-cu-adaptive-eval".into(), "--not-a-flag".into()]);
    assert_eq!(code, ProcessVerdict::Malformed.exit_code());
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
