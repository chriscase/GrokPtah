use std::env;
use std::fs;
use std::path::PathBuf;

use grokptah_cu_adaptive_eval::live::refuse_if_not_explicitly_enabled;
use grokptah_cu_adaptive_eval::report::{markdown_report, run_campaign};
use grokptah_cu_adaptive_eval::schema::to_canonical_json;
use grokptah_cu_adaptive_eval::types::{DEFAULT_REPEATS, DEFAULT_SEED};
use grokptah_cu_adaptive_eval::verifier::verify_report;

fn main() {
    let mut out = PathBuf::from("campaign-out");
    let mut repeats = DEFAULT_REPEATS;
    let mut seed = DEFAULT_SEED;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = PathBuf::from(args.next().expect("--out directory")),
            "--repeats" => repeats = args.next().expect("--repeats N").parse().expect("repeats"),
            "--seed" => seed = args.next().expect("--seed N").parse().expect("seed"),
            "--help" => {
                eprintln!(
                    "grokptah-cu-adaptive-eval --out DIR [--repeats N] [--seed N]\n\
                     Synthetic Computer Use adaptive evaluation. Zero provider calls by default."
                );
                return;
            }
            other => {
                eprintln!("unknown arg {other}");
                std::process::exit(2);
            }
        }
    }
    if let Err(err) = refuse_if_not_explicitly_enabled() {
        eprintln!("{err}");
        std::process::exit(2);
    }
    let campaign = run_campaign(repeats, seed);
    let verified = verify_report(&campaign.report);
    fs::create_dir_all(&out).expect("out dir");
    let json = to_canonical_json(&campaign.report).expect("json");
    fs::write(out.join("campaign-report.json"), json).expect("write report");
    fs::write(
        out.join("campaign-report.md"),
        markdown_report(&campaign.report),
    )
    .expect("md");
    let evidence = to_canonical_json(&campaign.evidence).expect("evidence");
    fs::write(out.join("campaign-evidence.json"), evidence).expect("write evidence");
    fs::write(
        out.join("verifier.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "ok": verified.ok,
            "errors": verified.errors,
            "status": campaign.report.status.as_str(),
        }))
        .expect("verifier json"),
    )
    .expect("write verifier");
    println!(
        "status={} fixture_match_errors={} unauthorized={} task={}/{} out={}",
        campaign.report.status.as_str(),
        verified.errors.len(),
        campaign.report.safety.unauthorized_dispatches,
        campaign.report.task_success.numerator,
        campaign.report.task_success.denominator,
        out.display()
    );
    if campaign.report.safety.release_failing {
        std::process::exit(1);
    }
}
