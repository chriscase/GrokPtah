//! Black-box certification runner for the public GrokPtah MCP control plane.
//!
//! The lab may bootstrap an isolated loopback host, but every probe observes
//! and mutates product state through [`grokptah_agent_bridge::McpControlClient`].

pub mod artifact;
pub mod authority_stage3;
pub mod capture;
pub mod cli;
pub mod local_service;
pub mod manifest;
pub mod memory_stage5;
pub mod normalize;
pub mod probes;
pub mod process_service;
pub mod report;
pub mod review_manifest;
pub mod review_report;
pub mod review_runner;
pub mod review_score;
pub mod runner;

pub const LAB_MANIFEST_SCHEMA: &str = "grokptah.certification_lab_manifest.v1";
pub const LAB_REPORT_SCHEMA: &str = "grokptah.certification_lab_report.v1";
pub const LAB_TRACE_SCHEMA: &str = "grokptah.certification_lab_trace.v1";
pub const REVIEW_CANDIDATE_SCHEMA: &str = "grokptah.certification_review_candidate.v1";
pub const REPLAY_FIXTURE_SCHEMA: &str = "grokptah.certification_structural_replay_fixture.v1";
pub const REVIEW_CAMPAIGN_SCHEMA: &str = "grokptah.code-review-benchmark-campaign.v1";
pub const REVIEW_CORPUS_SCHEMA: &str = "grokptah.code-review-benchmark-corpus.v1";
pub const REVIEW_FAKE_PROVIDER_SCHEMA: &str = "grokptah.code-review-benchmark-fake-provider.v1";
pub const REVIEW_REPORT_SCHEMA: &str = "grokptah.code-review-benchmark.v1";
pub const REVIEW_FINGERPRINT_SCHEMA: &str = "grokptah.code-review-benchmark-fingerprint.v1";
pub const REVIEW_IMPLEMENTATION_SCHEMA: &str = "grokptah.code-review-benchmark-implementation.v1";
pub const AUTHORITY_STAGE3_REPORT_SCHEMA: &str = authority_stage3::AUTHORITY_STAGE3_REPORT_SCHEMA;
pub const MEMORY_STAGE5_REPORT_SCHEMA: &str = memory_stage5::MEMORY_STAGE5_REPORT_SCHEMA;
pub const ALWAYS_ON_GROKBOT_FIXTURE: &[u8] = crate::process_service::FIXTURE_BYTES;
pub const ALWAYS_ON_GROKBOT_FIXTURE_SCHEMA: &str = crate::process_service::FIXTURE_SCHEMA;

#[cfg(test)]
mod always_on_tests {
    use super::*;

    #[test]
    fn always_on_fixture_schema_is_exported() {
        let value: serde_json::Value = serde_json::from_slice(ALWAYS_ON_GROKBOT_FIXTURE).unwrap();
        assert_eq!(value["schema"], ALWAYS_ON_GROKBOT_FIXTURE_SCHEMA);
        assert_eq!(value["schemaVersion"], 2);
        assert_eq!(
            value["failClosed"]["malformed"]["stopCause"],
            "token_accounting_unavailable"
        );
    }
}
