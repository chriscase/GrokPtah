//! The external-comparison and calibration contract.
//!
//! Every check here is about one question: can two results be shown to be
//! about the same thing, and is the answer allowed to be "no"? A contract
//! that only has a passing path is not a contract, so most of this file is
//! the refusal cases.
//!
//! Nothing here calls a provider. The only submissions that exist are
//! recorded from the synthetic fixtures in this crate, and the one path that
//! *would* involve a provider is tested by constructing a submission that
//! declares itself as one and checking it never becomes qualification.

use std::fs;
use std::path::PathBuf;

use grokptah_cu_bench::agent::Agent;
use grokptah_cu_bench::calibration::CalibrationTier;
use grokptah_cu_bench::comparison::{
    self, BoundaryViolation, COMPARISON_CONTRACT_VERSION, ComparisonEvidence, ComparisonRefusal,
    ComparisonVerdict, EvidenceClass, EvidenceStatus, RejectionReason, SubmissionOutcome,
    TraceFixture,
};
use grokptah_cu_bench::manifest::ARTIFACT_DIR;
use grokptah_cu_bench::modelclass::ModelClass;
use grokptah_cu_bench::profile::ProfileId;
use grokptah_cu_bench::scenario::Scenario;
use grokptah_cu_bench::{catalog, suite};

fn trace_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(ARTIFACT_DIR)
        .join("traces")
        .join(name)
}

fn load(name: &str) -> TraceFixture {
    let text = fs::read_to_string(trace_path(name))
        .unwrap_or_else(|error| panic!("missing trace {name}: {error}"));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("bad trace {name}: {error}"))
}

fn tier_factory(tier: CalibrationTier) -> impl Fn(ModelClass, &Scenario) -> Box<dyn Agent> {
    move |class, scenario: &Scenario| tier.agent(class, scenario.script.clone())
}

/// A freshly recorded reference trace at the canonical cell.
fn reference_trace() -> TraceFixture {
    let (model_class, profile) = suite::CANONICAL_COMPARISON_CELL;
    suite::record_trace(
        "reference",
        EvidenceClass::SyntheticFixture,
        model_class,
        profile,
        &catalog::all(),
        &suite::reference_factory(),
    )
}

// ------------------------------------------------------------ happy path --

#[test]
fn every_published_reference_trace_reproduces_locally() {
    let scenarios = catalog::all();
    let factory = suite::reference_factory();
    for model_class in ModelClass::ALL {
        for profile in ProfileId::ALL {
            let name = format!("reference-{}-{}.json", model_class.slug(), profile.slug());
            let submitted = load(&name);
            assert_eq!(
                suite::verify_trace(&submitted, &scenarios, &factory),
                SubmissionOutcome::ReproducedLocally,
                "{name} no longer reproduces; re-run emit_artifacts if this is intended"
            );
        }
    }
}

#[test]
fn a_reference_trace_without_a_local_rerun_only_reaches_basis_verified() {
    // What an external party's submission can actually reach. The weaker
    // level is the honest one: we checked it is about the same thing, not
    // that the numbers are true.
    let submitted = reference_trace();
    assert_eq!(
        comparison::verify(&submitted, None),
        SubmissionOutcome::BasisVerified
    );
    assert!(comparison::verify(&submitted, None).is_comparable());
    assert!(!comparison::verify(&submitted, None).counts_as_qualification());
}

#[test]
fn two_verified_fixture_submissions_may_be_compared() {
    let scenarios = catalog::all();
    let (model_class, profile) = suite::CANONICAL_COMPARISON_CELL;
    let reference = reference_trace();
    let timid = suite::record_trace(
        "timid",
        EvidenceClass::SyntheticFixture,
        model_class,
        profile,
        &scenarios,
        &tier_factory(CalibrationTier::Timid),
    );

    let left = comparison::verify(&reference, None);
    let right = comparison::verify(&timid, None);
    let verdict = comparison::compare((&reference, &left), (&timid, &right));

    let ComparisonVerdict::Comparable { table } = verdict else {
        panic!("two clean fixture submissions should be comparable: {verdict:?}");
    };
    assert_eq!(table.left_subject, "reference");
    assert_eq!(table.right_subject, "timid");
    assert!(
        table
            .scope_statement
            .contains("Establishes nothing about real models")
    );

    // The comparison should show what the calibration tiers already
    // establish: the timid subject attempts less and finishes less.
    let attempt = table
        .deltas
        .iter()
        .find(|delta| delta.metric == "attempt_bps")
        .expect("attempt delta present");
    assert!(attempt.higher_is_better);
    assert!(
        attempt.left_bps > attempt.right_bps,
        "reference should attempt more than the timid tier"
    );
}

// -------------------------------------------------------- refusal cases --

#[test]
fn a_submission_from_an_older_contract_is_rejected() {
    let mut submitted = reference_trace();
    submitted.basis.contract_version = "grokptah.cu-bench.comparison/0".into();
    assert!(matches!(
        comparison::verify(&submitted, None),
        SubmissionOutcome::Rejected {
            reason: RejectionReason::ContractVersionMismatch { .. }
        }
    ));
}

/// One basis field, the edit that corrupts it, and the reason expected back.
type BasisTamperCase = (
    &'static str,
    fn(&mut TraceFixture),
    fn(&RejectionReason) -> bool,
);

#[test]
fn each_basis_digest_is_actually_checked() {
    let cases: Vec<BasisTamperCase> = vec![
        (
            "manifest",
            |trace| trace.basis.manifest_digest = "0".repeat(64),
            |reason| matches!(reason, RejectionReason::ManifestDigestMismatch),
        ),
        (
            "catalog",
            |trace| trace.basis.catalog_digest = "0".repeat(64),
            |reason| matches!(reason, RejectionReason::CatalogDigestMismatch),
        ),
        (
            "envelope",
            |trace| trace.basis.envelope_digest = "0".repeat(64),
            |reason| matches!(reason, RejectionReason::EnvelopeDigestMismatch),
        ),
    ];
    for (label, tamper, expected) in cases {
        let mut submitted = reference_trace();
        tamper(&mut submitted);
        let outcome = comparison::verify(&submitted, None);
        let SubmissionOutcome::Rejected { reason } = &outcome else {
            panic!("{label} digest tamper was accepted: {outcome:?}");
        };
        assert!(expected(reason), "{label} tamper blamed {reason:?}");
    }
}

#[test]
fn editing_one_row_without_recomputing_the_fold_is_caught() {
    let mut submitted = reference_trace();
    submitted.scenarios[0].transcript_digest = "1".repeat(64);
    assert!(matches!(
        comparison::verify(&submitted, None),
        SubmissionOutcome::Rejected {
            reason: RejectionReason::TraceDigestMismatch
        }
    ));
}

#[test]
fn editing_one_row_and_recomputing_the_fold_is_caught_by_reproduction() {
    // A submitter who understands the fold can make the fixture internally
    // consistent. That is exactly why local reproduction exists as a
    // separate, stronger level.
    let scenarios = catalog::all();
    let factory = suite::reference_factory();
    let mut submitted = reference_trace();
    submitted.scenarios[0].transcript_digest = "1".repeat(64);
    submitted.trace_digest = comparison::fold_scenarios(&submitted.scenarios);

    assert_eq!(
        comparison::verify(&submitted, None),
        SubmissionOutcome::BasisVerified,
        "an internally consistent forgery passes the basis check, as expected"
    );
    let outcome = suite::verify_trace(&submitted, &scenarios, &factory);
    assert!(
        matches!(
            outcome,
            SubmissionOutcome::Rejected {
                reason: RejectionReason::ReproductionMismatch { .. }
            }
        ),
        "reproduction should have caught the edited row: {outcome:?}"
    );
}

#[test]
fn dropping_a_scenario_is_rejected() {
    // The failure mode this closes: a submission that quietly omits the
    // scenarios it did badly on would compare favourably against one that
    // ran everything.
    let mut submitted = reference_trace();
    let dropped = submitted.scenarios.remove(3).scenario_id;
    submitted.trace_digest = comparison::fold_scenarios(&submitted.scenarios);

    let outcome = comparison::verify(&submitted, None);
    let SubmissionOutcome::Rejected {
        reason: RejectionReason::ScenarioSetMismatch { missing, unknown },
    } = outcome
    else {
        panic!("dropping a scenario was accepted: {outcome:?}");
    };
    assert_eq!(missing, vec![dropped]);
    assert!(unknown.is_empty());
}

#[test]
fn an_unknown_scenario_is_rejected() {
    let mut submitted = reference_trace();
    submitted.scenarios[0].scenario_id = "editor_workflow/invented".into();
    submitted.trace_digest = comparison::fold_scenarios(&submitted.scenarios);
    assert!(matches!(
        comparison::verify(&submitted, None),
        SubmissionOutcome::Rejected {
            reason: RejectionReason::ScenarioSetMismatch { .. }
        }
    ));
}

#[test]
fn a_duplicated_scenario_is_rejected() {
    let mut submitted = reference_trace();
    let duplicate = submitted.scenarios[0].clone();
    submitted.scenarios.push(duplicate);
    submitted.trace_digest = comparison::fold_scenarios(&submitted.scenarios);
    assert!(matches!(
        comparison::verify(&submitted, None),
        SubmissionOutcome::Rejected {
            reason: RejectionReason::DuplicateScenario { .. }
        }
    ));
}

#[test]
fn an_empty_submission_is_rejected_rather_than_vacuously_accepted() {
    let mut submitted = reference_trace();
    submitted.scenarios.clear();
    submitted.trace_digest = comparison::fold_scenarios(&submitted.scenarios);
    assert!(matches!(
        comparison::verify(&submitted, None),
        SubmissionOutcome::Rejected {
            reason: RejectionReason::EmptyEvidence
        }
    ));
}

// ----------------------------------------------- boundaries before numbers --

#[test]
fn the_published_overreaching_trace_is_rejected_on_its_boundaries() {
    // A negative fixture, published on purpose. It proves the rejection path
    // works against a real recorded run rather than only against a struct
    // built by a test.
    let submitted = load("overreaching-large_vision-balanced.json");
    let outcome = comparison::verify(&submitted, None);
    let SubmissionOutcome::Rejected {
        reason: RejectionReason::BoundaryViolations { violations },
    } = outcome
    else {
        panic!("the overreaching trace should be disqualified: {outcome:?}");
    };
    assert!(
        violations.contains(&BoundaryViolation::EnvelopeBreach),
        "{violations:?}"
    );
    assert!(
        violations.contains(&BoundaryViolation::Collateral),
        "{violations:?}"
    );
}

#[test]
fn a_disqualified_submission_cannot_be_placed_in_a_comparison() {
    let reference = reference_trace();
    let disqualified = load("overreaching-large_vision-balanced.json");
    let left = comparison::verify(&reference, None);
    let right = comparison::verify(&disqualified, None);

    let verdict = comparison::compare((&reference, &left), (&disqualified, &right));
    assert!(matches!(
        verdict,
        ComparisonVerdict::Refused {
            reason: ComparisonRefusal::SubmissionNotVerified { .. }
        }
    ));
}

#[test]
fn every_published_clean_trace_attests_freshness_and_redaction() {
    for name in [
        "reference-large_vision-balanced.json",
        "reference-small_local_gateway-economy.json",
        "timid-large_vision-balanced.json",
        "profligate-large_vision-balanced.json",
    ] {
        let trace = load(name);
        let boundaries = &trace.boundaries;
        assert!(
            boundaries.is_clean(),
            "{name}: {:?}",
            boundaries.violations()
        );
        assert!(
            boundaries.observation_age_bound_millis > 0,
            "{name}: no freshness bound recorded"
        );
        assert!(
            boundaries.max_observation_age_at_action_millis
                <= boundaries.observation_age_bound_millis,
            "{name}: acted on an observation older than the bound"
        );
        assert_eq!(
            boundaries.screenshots_exposed, boundaries.screenshots_redacted,
            "{name}: a screenshot reached the model unredacted"
        );
    }
}

// ------------------------------------------------------- provider claims --

#[test]
fn a_provider_claim_never_verifies_however_well_formed_it_is() {
    // Same digests, same scenario set, clean boundaries -- everything the
    // contract can check is satisfied. It still does not verify, because
    // this crate has no way to reproduce a provider run and must not launder
    // a self-reported number into a measurement.
    let mut submitted = reference_trace();
    submitted.subject = "some-operator-run".into();
    submitted.evidence = EvidenceClass::RealProvider {
        run_label: "operator-supplied label, never interpreted here".into(),
    };

    let outcome = comparison::verify(&submitted, None);
    assert_eq!(outcome, SubmissionOutcome::UnverifiedProviderClaim);
    assert!(!outcome.counts_as_qualification());
    assert!(!outcome.is_comparable());
}

#[test]
fn a_provider_claim_is_refused_on_either_side_of_a_comparison() {
    let reference = reference_trace();
    let mut claim = reference_trace();
    claim.subject = "some-operator-run".into();
    claim.evidence = EvidenceClass::RealProvider {
        run_label: "x".into(),
    };

    let verified = comparison::verify(&reference, None);
    let unverified = comparison::verify(&claim, None);

    for verdict in [
        comparison::compare((&reference, &verified), (&claim, &unverified)),
        comparison::compare((&claim, &unverified), (&reference, &verified)),
    ] {
        assert!(
            matches!(
                verdict,
                ComparisonVerdict::Refused {
                    reason: ComparisonRefusal::ProviderClaimNotComparable { .. }
                }
            ),
            "a provider claim was allowed into a comparison: {verdict:?}"
        );
    }
}

// ------------------------------------------------------------ same basis --

#[test]
fn results_from_different_cells_are_not_compared() {
    let scenarios = catalog::all();
    let factory = suite::reference_factory();
    let balanced = reference_trace();
    let economy = suite::record_trace(
        "reference",
        EvidenceClass::SyntheticFixture,
        ModelClass::LargeVision,
        ProfileId::Economy,
        &scenarios,
        &factory,
    );

    let left = comparison::verify(&balanced, None);
    let right = comparison::verify(&economy, None);
    let verdict = comparison::compare((&balanced, &left), (&economy, &right));
    assert!(
        matches!(
            verdict,
            ComparisonVerdict::Refused {
                reason: ComparisonRefusal::DifferentBasis { .. }
            }
        ),
        "two different profiles were compared: {verdict:?}"
    );
}

#[test]
fn results_from_different_model_classes_are_not_compared() {
    let scenarios = catalog::all();
    let factory = suite::reference_factory();
    let large = reference_trace();
    let small = suite::record_trace(
        "reference",
        EvidenceClass::SyntheticFixture,
        ModelClass::SmallLocalGateway,
        ProfileId::Balanced,
        &scenarios,
        &factory,
    );

    let left = comparison::verify(&large, None);
    let right = comparison::verify(&small, None);
    let verdict = comparison::compare((&large, &left), (&small, &right));
    let ComparisonVerdict::Refused {
        reason: ComparisonRefusal::DifferentBasis { field },
    } = verdict
    else {
        panic!("two model classes were compared: {verdict:?}");
    };
    // The envelope differs before the class name does, and that is the more
    // informative refusal: the two subjects were held to different rules.
    assert!(
        field == "envelope_digest" || field == "model_class",
        "unexpected basis field: {field}"
    );
}

// ----------------------------------------------------- missing evidence --

#[test]
fn a_build_with_no_submissions_reports_that_and_supports_no_comparison() {
    let evidence = ComparisonEvidence::summarise(&[]);
    assert_eq!(evidence.status, EvidenceStatus::NoExternalSubmission);
    assert!(evidence.supports_no_comparison());
    assert_eq!(evidence.contract_version, COMPARISON_CONTRACT_VERSION);
}

#[test]
fn the_suite_report_states_the_absence_rather_than_omitting_it() {
    let scenarios = catalog::all();
    let factory = suite::reference_factory();
    let report = suite::run_matrix(&scenarios, &factory);
    assert_eq!(
        report.comparison_evidence.status,
        EvidenceStatus::NoExternalSubmission
    );

    let markdown = grokptah_cu_bench::report::to_markdown(
        &report,
        &grokptah_cu_bench::matrix::workflow_matrix(),
    );
    assert!(
        markdown.contains("External comparison evidence"),
        "the report does not mention comparison evidence at all"
    );
    assert!(markdown.contains("NoExternalSubmission"));
}

#[test]
fn one_rejected_submission_blocks_the_whole_evidence_set() {
    // Partial evidence is not evidence. If any submission failed, no
    // comparison is available until it is fixed or withdrawn.
    let evidence = ComparisonEvidence::summarise(&[
        SubmissionOutcome::ReproducedLocally,
        SubmissionOutcome::BasisVerified,
        SubmissionOutcome::Rejected {
            reason: RejectionReason::EmptyEvidence,
        },
    ]);
    assert_eq!(
        evidence.status,
        EvidenceStatus::ContainsRejectedSubmissions { rejected: 1 }
    );
    assert!(evidence.supports_no_comparison());
}
