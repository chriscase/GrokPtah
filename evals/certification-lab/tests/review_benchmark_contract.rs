//! Mutation-resistant contract for the code-review certification slice.
//!
//! This is a fake-provider contract. It must not be read as a live quality
//! certification or as proof that the enterprise review lane is shipped.

use grokptah_certification_lab::cli::{
    execute, Cli, Command, ExitClass, InspectArgs, RepositoryArgs, ReviewArgs,
};
use grokptah_certification_lab::review_manifest::ReviewBundle;
use grokptah_certification_lab::review_report::{
    inspect_review_campaign, QualityClaim, ReviewMode, ReviewVerdict,
};
use grokptah_certification_lab::review_runner::{
    apply_mutation, contract_verdict, preflight_review, run_fake_evaluable, run_review,
    ContractMutation, ReviewOptions,
};
use grokptah_certification_lab::REVIEW_REPORT_SCHEMA;
use std::path::PathBuf;

fn repository_root() -> PathBuf {
    dunce::canonicalize(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap()
}

fn all_mutations() -> &'static [ContractMutation] {
    &[
        ContractMutation::RemoveTrueIssue,
        ContractMutation::AddLureFp,
        ContractMutation::DuplicateFinding,
        ContractMutation::ShiftRegion,
        ContractMutation::CorruptCausalAtom,
        ContractMutation::InflateConfidence,
        ContractMutation::OmitCase,
        ContractMutation::DuplicateCase,
        ContractMutation::SwapArmLabels,
        ContractMutation::SwapArmNonces,
        ContractMutation::ChangeCorpusDigest,
        ContractMutation::ChangeOracleDigest,
        ContractMutation::ChangeScorerDigest,
        ContractMutation::MixRouteAttestation,
        ContractMutation::MixModelAttestation,
        ContractMutation::ExceedBound,
        ContractMutation::MutateWorkspace,
        ContractMutation::DropProviderObservation,
        ContractMutation::MarkFakeQualityEligible,
    ]
}

#[test]
fn bundled_manifest_corpus_and_fake_provider_bind() {
    let bundle = ReviewBundle::bundled().unwrap();
    assert_eq!(bundle.campaign.case_ids().len(), 24);
    assert_eq!(bundle.corpus.families.len(), 8);
    let (runtime, oracles) = bundle.corpus.split().unwrap();
    assert_eq!(oracles.issues.len(), 24);
    assert_eq!(oracles.lures.len(), 24);
    assert_eq!(runtime.families.len(), 8);
    let debug = format!("{runtime:?}");
    assert!(!debug.contains("hidden_oracle"));
    assert!(!debug.contains("accepted_region"));
}

#[test]
fn fake_contract_passes_and_cannot_prove_quality() {
    let bundle = ReviewBundle::bundled().unwrap();
    let state = run_fake_evaluable(&bundle).unwrap();
    assert_eq!(contract_verdict(&state), ReviewVerdict::ContractPassed);
    assert!(!state.report.quality_claim_eligible);
    assert_eq!(
        state.report.quality_claim,
        QualityClaim::FakeCannotProveQuality
    );
    assert_eq!(state.report.mode, ReviewMode::Fake);
    assert_eq!(state.report.schema, REVIEW_REPORT_SCHEMA);
    assert_eq!(state.report.cardinalities.cases_scored, 24);
    assert_eq!(state.report.cardinalities.families_scored, 8);
    assert_eq!(state.report.cardinalities.restart_count, 1);
    assert_eq!(state.report.cardinalities.implicit_resend_count, 0);
    assert_eq!(
        state.report.cardinalities.duplicate_finding_after_restart,
        0
    );
    assert_eq!(state.report.cardinalities.in_flight_frozen, 1);
    assert_eq!(state.report.cardinalities.admissions_blocked_on_drift, 1);
    assert_eq!(state.report.cardinalities.quota_one_under_admitted, 1);
    assert_eq!(state.report.cardinalities.quota_exhausted_blocked, 1);
    assert_eq!(state.report.cardinalities.quota_window_advances, 1);
    assert!(state.report.cardinalities.mutator_denials > 0);
    assert!(state.report.cardinalities.publish_denials > 0);
    assert_eq!(state.report.cardinalities.canary_request_hits, 0);
    assert_eq!(state.report.cardinalities.canary_evidence_hits, 0);
    assert_eq!(
        state.report.workspace.pre_merkle_root,
        state.report.workspace.post_merkle_root
    );
    assert!(state.report.completeness.authoritative_usage_complete);
    assert!(!state.report.cis.efficiency_superiority_claimable);
    assert_eq!(state.oracles.issues.len(), 24);
}

#[test]
fn each_contract_mutation_flips_pass() {
    let bundle = ReviewBundle::bundled().unwrap();
    let original = run_fake_evaluable(&bundle).unwrap();
    assert_eq!(contract_verdict(&original), ReviewVerdict::ContractPassed);
    for mutation in all_mutations() {
        let mut state = original.clone();
        let verdict = apply_mutation(&mut state, *mutation).unwrap();
        assert_ne!(
            verdict,
            ReviewVerdict::ContractPassed,
            "mutation {mutation:?} must flip PASS"
        );
    }
}

#[test]
fn live_mode_is_indeterminate_without_enterprise_lease() {
    let repository = repository_root();
    let output = tempfile::tempdir().unwrap();
    let options = ReviewOptions {
        repository_root: repository.clone(),
        campaign_path: repository.join("evals/code-review-benchmark/campaign.v1.json"),
        output_root: output.path().to_path_buf(),
        artifact_budget_bytes: 128 * 1024 * 1024,
        mode: ReviewMode::Live,
        preflight_only: false,
    };
    let preflight = preflight_review(&options).unwrap();
    assert!(!preflight.quality_claim_eligible);
    assert!(!preflight.enterprise_gateway_lease);
    assert_eq!(preflight.notice, QualityClaim::LiveAttestationMissing);
    let completion = run_review(&options).unwrap();
    assert_eq!(completion.verdict, ReviewVerdict::Indeterminate);
    assert!(!completion.quality_claim_eligible);
    let campaign =
        grokptah_certification_lab::review_runner::latest_review_campaign_dir(output.path())
            .unwrap();
    let report = inspect_review_campaign(&campaign).unwrap();
    assert_eq!(report.verdict, ReviewVerdict::Indeterminate);
    assert!(!report.quality_claim_eligible);
}

#[tokio::test(flavor = "current_thread")]
async fn fake_review_cli_seals_and_inspects() {
    let repository = repository_root();
    let output = tempfile::tempdir().unwrap();
    let review = Cli {
        command: Command::Review(ReviewArgs {
            repository: RepositoryArgs {
                repository: repository.clone(),
            },
            campaign: None,
            output: Some(output.path().to_path_buf()),
            live: false,
            preflight: false,
            artifact_budget_bytes: 128 * 1024 * 1024,
        }),
    };
    assert_eq!(execute(review).await, ExitClass::Passed);
    let campaign =
        grokptah_certification_lab::review_runner::latest_review_campaign_dir(output.path())
            .unwrap();
    let inspect = Cli {
        command: Command::Inspect(InspectArgs {
            campaign: campaign.clone(),
        }),
    };
    assert_eq!(execute(inspect).await, ExitClass::Passed);
    let report = inspect_review_campaign(&campaign).unwrap();
    assert!(!report.quality_claim_eligible);
    assert_eq!(report.quality_claim, QualityClaim::FakeCannotProveQuality);
}

#[test]
fn unknown_campaign_field_fails_closed() {
    let mut value: serde_json::Value = serde_json::from_slice(
        grokptah_certification_lab::review_manifest::BUNDLED_REVIEW_CAMPAIGN,
    )
    .unwrap();
    value["unexpected"] = serde_json::json!(true);
    assert!(
        grokptah_certification_lab::review_manifest::ReviewCampaign::from_slice(
            &serde_json::to_vec(&value).unwrap()
        )
        .is_err()
    );
}
