//! Provider-neutral Computer Use adaptive evaluation harness.
//!
//! This crate owns deterministic fixtures, metrics, and reports for issues
//! #435, #272, #274, and #363. It does not implement or edit production
//! adaptive-profile runtime, provider-send ledgers, headless/broker adapters,
//! native helpers, or VM backends.

pub mod adapters;
pub mod catalog;
pub mod host;
pub mod live;
pub mod naming;
pub mod policy;
pub mod profile;
pub mod report;
pub mod runner;
pub mod schema;
pub mod types;
pub mod verifier;

pub use catalog::{catalog, validate_catalog};
pub use naming::NamingRecord;
pub use report::{run_campaign, CampaignOutput, CampaignReport};
pub use types::{CampaignStatus, FamilyId, ProfileId, SOURCE_GATE_SHA};
pub use verifier::{verify_json, verify_report};

pub fn allowlist() -> &'static [&'static str] {
    &[
        "evals/computer-use-adaptive/",
        "docs/COMPUTER_USE_ADAPTIVE_EVAL.md",
    ]
}
