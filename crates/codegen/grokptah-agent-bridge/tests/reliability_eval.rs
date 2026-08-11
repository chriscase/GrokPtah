//! CI entry point for the same deterministic campaign used by the operator
//! report-producing example. Keeping one scenario implementation prevents the
//! manual and CI paths from drifting apart.

#[allow(dead_code)]
#[path = "../examples/reliability_eval.rs"]
mod reliability_runner;

use std::fs;

use grokptah_agent_bridge::reliability_eval::write_report;

#[tokio::test]
async fn deterministic_reliability_campaign_writes_safe_report() {
    let report = reliability_runner::run_campaign().await.unwrap();
    assert_eq!(
        report.summary.failed, 0,
        "reliability failures: {report:#?}"
    );
    assert_eq!(report.summary.passed, 7);
    let output = tempfile::tempdir().unwrap();
    let path = write_report(output.path(), &report).unwrap();
    let json = fs::read_to_string(path).unwrap();
    assert!(!json.contains("GROKPTAH_AGENT_OFFLINE"));
    assert!(!json.contains("/private/"));
    assert!(json.len() < 2 * 1024 * 1024);
}
