//! Shipped CLI entry. Tests call this function; the binary is a thin wrapper.

use std::fs;
use std::path::PathBuf;

use crate::live::refuse_if_not_explicitly_enabled;
use crate::report::{markdown_report, run_campaign_with_source};
use crate::schema::to_canonical_json;
use crate::types::{
    validate_repeats, ProcessVerdict, DEFAULT_REPEATS, DEFAULT_SEED, SOURCE_GATE_SHA,
};
use crate::verifier::{verify_campaign, VerifyMode};

pub fn main(args: impl IntoIterator<Item = String>) -> i32 {
    match run(args) {
        Ok(code) => code,
        Err((verdict, message)) => {
            println!("terminal={} ok=false errors=1", verdict.as_str());
            eprintln!("{message}");
            verdict.exit_code()
        }
    }
}

fn run(args: impl IntoIterator<Item = String>) -> Result<i32, (ProcessVerdict, String)> {
    let mut out = PathBuf::from("campaign-out");
    let mut repeats = DEFAULT_REPEATS;
    let mut seed = DEFAULT_SEED;
    let mut expect_source_gate: Option<String> = None;
    let mut expect_head: Option<String> = None;
    let mut repository = PathBuf::from(".");
    let mut repository_explicit = false;
    let mut verify_report_path: Option<PathBuf> = None;
    let mut verify_evidence_path: Option<PathBuf> = None;
    let mut argv = args.into_iter();
    let _exe = argv.next();
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--verify-report" => {
                verify_report_path = Some(PathBuf::from(argv.next().ok_or((
                    ProcessVerdict::Malformed,
                    "--verify-report requires a path".into(),
                ))?));
            }
            "--verify-evidence" => {
                verify_evidence_path = Some(PathBuf::from(argv.next().ok_or((
                    ProcessVerdict::Malformed,
                    "--verify-evidence requires a path".into(),
                ))?));
            }
            "--out" => {
                out = PathBuf::from(argv.next().ok_or((
                    ProcessVerdict::Malformed,
                    "--out requires a directory".into(),
                ))?);
            }
            "--repeats" => {
                let raw = argv
                    .next()
                    .ok_or((ProcessVerdict::Malformed, "--repeats requires N".into()))?;
                repeats = raw.parse().map_err(|_| {
                    (
                        ProcessVerdict::InvalidRepeats,
                        format!("repeats={raw} is not an integer"),
                    )
                })?;
            }
            "--seed" => {
                let raw = argv
                    .next()
                    .ok_or((ProcessVerdict::Malformed, "--seed requires N".into()))?;
                seed = raw.parse().map_err(|_| {
                    (
                        ProcessVerdict::Malformed,
                        format!("seed={raw} is not an integer"),
                    )
                })?;
            }
            "--source-gate" => {
                expect_source_gate = Some(argv.next().ok_or((
                    ProcessVerdict::Malformed,
                    "--source-gate requires a SHA".into(),
                ))?);
            }
            "--expected-head" => {
                expect_head = Some(argv.next().ok_or((
                    ProcessVerdict::Malformed,
                    "--expected-head requires a SHA".into(),
                ))?);
            }
            "--repository" => {
                repository_explicit = true;
                repository = PathBuf::from(argv.next().ok_or((
                    ProcessVerdict::Malformed,
                    "--repository requires a path".into(),
                ))?);
            }
            "--help" => {
                eprintln!(
                    "grokptah-cu-adaptive-eval --out DIR [--repeats N] [--seed N] --repository PATH --expected-head SHA --source-gate BASE_SHA\n\
                     grokptah-cu-adaptive-eval --verify-report FILE --verify-evidence FILE --repository PATH --expected-head SHA --source-gate BASE_SHA\n\
                     Synthetic Computer Use adaptive evaluation. Zero provider calls by default.\n\
                     PASS is the only zero exit."
                );
                return Ok(0);
            }
            other => {
                return Err((ProcessVerdict::Malformed, format!("unknown arg {other}")));
            }
        }
    }
    if let (Some(report_path), Some(evidence_path)) = (&verify_report_path, &verify_evidence_path) {
        if !repository_explicit || expect_head.is_none() || expect_source_gate.is_none() {
            return Err((
                ProcessVerdict::Malformed,
                "verification requires --repository, --expected-head, and --source-gate".into(),
            ));
        }
        let report_text = fs::read_to_string(report_path)
            .map_err(|err| (ProcessVerdict::Malformed, err.to_string()))?;
        let evidence_text = fs::read_to_string(evidence_path)
            .map_err(|err| (ProcessVerdict::Malformed, err.to_string()))?;
        let report: crate::report::CampaignReport = crate::schema::parse_strict(&report_text)
            .map_err(|err| (ProcessVerdict::Malformed, err.to_string()))?;
        let expected_base = expect_source_gate.as_deref().unwrap_or(SOURCE_GATE_SHA);
        let observed =
            crate::source::observe_source(&repository, expect_head.as_deref(), expected_base)
                .map_err(|err| (ProcessVerdict::Malformed, err.to_string()))?;
        if report.source_gate != observed {
            return Err((
                ProcessVerdict::Malformed,
                "artifact source identity does not match independently observed repository".into(),
            ));
        }
        let verified = crate::verifier::verify_json_with_evidence(&report_text, &evidence_text)
            .map_err(|err| (ProcessVerdict::Malformed, err.to_string()))?;
        println!(
            "terminal={} ok={} errors={}",
            verified.terminal_verdict.as_str(),
            verified.ok,
            verified.errors.len()
        );
        for err in &verified.errors {
            eprintln!("verifier: {err}");
        }
        return Ok(verified.terminal_verdict.exit_code());
    }
    if verify_report_path.is_some() || verify_evidence_path.is_some() {
        return Err((
            ProcessVerdict::Malformed,
            "--verify-report and --verify-evidence must be used together".into(),
        ));
    }
    if let Err(err) = validate_repeats(repeats) {
        return Err((ProcessVerdict::InvalidRepeats, err.to_string()));
    }
    if let Err(err) = refuse_if_not_explicitly_enabled() {
        return Err((ProcessVerdict::LiveRefused, err.to_string()));
    }
    if !repository_explicit || expect_head.is_none() || expect_source_gate.is_none() {
        return Err((
            ProcessVerdict::Malformed,
            "campaign generation requires --repository, --expected-head, and --source-gate".into(),
        ));
    }
    let expected_base = expect_source_gate.as_deref().unwrap_or(SOURCE_GATE_SHA);
    let source = crate::source::observe_source(&repository, expect_head.as_deref(), expected_base)
        .map_err(|err| (ProcessVerdict::Malformed, err.to_string()))?;
    let campaign = run_campaign_with_source(repeats, seed, source)
        .map_err(|err| (ProcessVerdict::Malformed, err.to_string()))?;
    let verified = verify_campaign(
        &campaign.report,
        Some(&campaign.evidence),
        VerifyMode::Synthetic,
    );
    if let Err(err) = fs::create_dir_all(&out) {
        return Err((ProcessVerdict::Malformed, err.to_string()));
    }
    let json = to_canonical_json(&campaign.report)
        .map_err(|err| (ProcessVerdict::Malformed, err.to_string()))?;
    fs::write(out.join("campaign-report.json"), json)
        .map_err(|err| (ProcessVerdict::Malformed, err.to_string()))?;
    fs::write(
        out.join("campaign-report.md"),
        markdown_report(&campaign.report),
    )
    .map_err(|err| (ProcessVerdict::Malformed, err.to_string()))?;
    let evidence = to_canonical_json(&campaign.evidence)
        .map_err(|err| (ProcessVerdict::Malformed, err.to_string()))?;
    fs::write(out.join("campaign-evidence.json"), evidence)
        .map_err(|err| (ProcessVerdict::Malformed, err.to_string()))?;
    let verdict = verified.terminal_verdict;
    fs::write(
        out.join("verifier.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "ok": verified.ok,
            "errors": verified.errors,
            "status": campaign.report.status.as_str(),
            "terminalVerdict": verdict.as_str(),
            "exitCode": verdict.exit_code(),
            "campaignDigest": campaign.report.campaign_digest,
            "syntheticPassDoesNotImplyLiveEligibility": true,
        }))
        .map_err(|err| (ProcessVerdict::Malformed, err.to_string()))?,
    )
    .map_err(|err| (ProcessVerdict::Malformed, err.to_string()))?;
    println!(
        "status={} terminal={} unauthorized={} task={}/{} digest={} out={}",
        campaign.report.status.as_str(),
        verdict.as_str(),
        campaign.report.safety.unauthorized_dispatches,
        campaign.report.task_success.numerator,
        campaign.report.task_success.denominator,
        campaign.report.campaign_digest,
        out.display()
    );
    Ok(verdict.exit_code())
}
