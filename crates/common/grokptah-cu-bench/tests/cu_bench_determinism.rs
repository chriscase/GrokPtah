//! Determinism.
//!
//! Every number this crate publishes is only worth reading if the same inputs
//! give the same outputs. These tests check that at three levels: one run, one
//! cell, and the whole suite.

use grokptah_cu_bench::agent::{Agent, ReferenceAgent};
use grokptah_cu_bench::digest::digest_of;
use grokptah_cu_bench::modelclass::{BPS_FULL, ModelClass};
use grokptah_cu_bench::profile::{ExecutionProfile, ProfileId};
use grokptah_cu_bench::runner::execute;
use grokptah_cu_bench::{catalog, manifest, matrix, report, suite};

#[test]
fn one_run_replays_to_the_same_transcript() {
    for profile_id in ProfileId::ALL {
        let profile = ExecutionProfile::for_id(*profile_id);
        for model_class in ModelClass::ALL {
            for scenario in catalog::all() {
                let mut first: Box<dyn Agent> =
                    Box::new(ReferenceAgent::new(*model_class, scenario.script.clone()));
                let mut second: Box<dyn Agent> =
                    Box::new(ReferenceAgent::new(*model_class, scenario.script.clone()));
                let a = execute(&scenario, &profile, first.as_mut());
                let b = execute(&scenario, &profile, second.as_mut());
                assert_eq!(
                    a.transcript_digest,
                    b.transcript_digest,
                    "{} / {} / {} is not deterministic",
                    scenario.id,
                    model_class.slug(),
                    profile_id.slug()
                );
                assert_eq!(a, b, "records diverged beyond the digest");
            }
        }
    }
}

#[test]
fn a_scenario_run_leaves_its_fixture_untouched() {
    // The runner clones the world. If it did not, running the matrix would
    // mean every cell after the first saw a surface the previous cell had
    // already edited -- and the cross-profile comparison would be nonsense.
    let profile = ExecutionProfile::balanced();
    for scenario in catalog::all() {
        let before = digest_of(&scenario);
        let mut agent: Box<dyn Agent> = Box::new(ReferenceAgent::new(
            ModelClass::LargeVision,
            scenario.script.clone(),
        ));
        let _ = execute(&scenario, &profile, agent.as_mut());
        assert_eq!(
            before,
            digest_of(&scenario),
            "{} was mutated by its run",
            scenario.id
        );
    }
}

#[test]
fn every_cell_reports_full_replay_determinism() {
    let scenarios = catalog::all();
    let factory = suite::reference_factory();
    for cell in suite::run_matrix(&scenarios, &factory).cells {
        assert_eq!(
            cell.deterministic_replay_bps,
            BPS_FULL,
            "{}/{} did not replay exactly",
            cell.model_class.slug(),
            cell.profile.slug()
        );
    }
}

#[test]
fn the_suite_digest_is_stable() {
    let scenarios = catalog::all();
    let factory = suite::reference_factory();
    let first = suite::run_matrix(&scenarios, &factory);
    let second = suite::run_matrix(&scenarios, &factory);
    assert_eq!(first.suite_digest, second.suite_digest);
    assert_eq!(report::to_json(&first), report::to_json(&second));
    assert_eq!(
        report::to_markdown(&first, &matrix::workflow_matrix()),
        report::to_markdown(&second, &matrix::workflow_matrix())
    );
}

#[test]
fn cell_order_is_stable_so_reports_diff_cleanly() {
    let scenarios = catalog::all();
    let factory = suite::reference_factory();
    let report = suite::run_matrix(&scenarios, &factory);
    let order: Vec<(&str, &str)> = report
        .cells
        .iter()
        .map(|cell| (cell.model_class.slug(), cell.profile.slug()))
        .collect();
    assert_eq!(
        order,
        vec![
            ("small_local_gateway", "economy"),
            ("small_local_gateway", "balanced"),
            ("small_local_gateway", "high_assurance"),
            ("large_vision", "economy"),
            ("large_vision", "balanced"),
            ("large_vision", "high_assurance"),
        ]
    );
}

#[test]
fn the_manifest_digest_pins_the_whole_fixture_set() {
    let first = manifest::manifest();
    let second = manifest::manifest();
    assert_eq!(first, second);
    // The scenarios artifact is the bulk of what a lab would run; make sure
    // it is actually covered by the manifest digest.
    assert!(
        first
            .entries
            .iter()
            .any(|entry| entry.kind == manifest::ArtifactKind::Scenarios)
    );
}
