use std::path::{Path, PathBuf};
use std::process::Command;

use grokptah_cu_adaptive_eval::{CampaignOutput, SOURCE_GATE_SHA};

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("adaptive evaluator is two levels below the repository root")
        .to_path_buf()
}

pub fn run_campaign(
    repeats: u32,
    seed: u64,
) -> grokptah_cu_adaptive_eval::types::EvalResult<CampaignOutput> {
    let repository = repository();
    let output = Command::new("git")
        .arg("-C")
        .arg(&repository)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git is available");
    assert!(output.status.success(), "git rev-parse HEAD failed");
    let expected_head = String::from_utf8(output.stdout)
        .expect("git head is utf-8")
        .trim()
        .to_owned();
    grokptah_cu_adaptive_eval::run_campaign(
        repeats,
        seed,
        &repository,
        &expected_head,
        SOURCE_GATE_SHA,
    )
}
