//! Regenerates the checked-in artifact set and reports.
//!
//! `cargo run -p grokptah-cu-bench --example emit_artifacts`
//!
//! CI verifies the checked-in files match what this produces, so running it
//! is how you accept an intentional change to the benchmark.

use std::fs;
use std::path::Path;

use grokptah_cu_bench::agent::Agent;
use grokptah_cu_bench::calibration::CalibrationTier;
use grokptah_cu_bench::comparison::EvidenceClass;
use grokptah_cu_bench::manifest::{self, ARTIFACT_DIR, ArtifactKind};
use grokptah_cu_bench::modelclass::ModelClass;
use grokptah_cu_bench::profile::ProfileId;
use grokptah_cu_bench::report::CalibrationRow;
use grokptah_cu_bench::scenario::Scenario;
use grokptah_cu_bench::{catalog, matrix, report, suite};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(ARTIFACT_DIR);

    for kind in ArtifactKind::ALL {
        let path = root.join(kind.path());
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, kind.render())?;
        println!("wrote {}", path.display());
    }

    let manifest_path = root.join("manifest.json");
    fs::write(&manifest_path, manifest::manifest_json())?;
    println!("wrote {}", manifest_path.display());

    let scenarios = catalog::all();
    let factory = suite::reference_factory();
    let suite_report = suite::run_matrix(&scenarios, &factory);
    let workflow_matrix = matrix::workflow_matrix();

    let reports = root.join("reports");
    fs::create_dir_all(&reports)?;
    fs::write(
        reports.join("reference-suite.json"),
        report::to_json(&suite_report),
    )?;
    fs::write(
        reports.join("reference-suite.md"),
        report::to_markdown(&suite_report, &workflow_matrix),
    )?;
    println!("wrote {}", reports.join("reference-suite.md").display());

    // Calibration evidence: the reference against every named tier, so a
    // reader can check that each threshold sits between two measurements.
    let mut rows = vec![CalibrationRow::from_report("reference", &suite_report)];
    for tier in CalibrationTier::ALL {
        let tier = *tier;
        let tier_factory = move |class: ModelClass, scenario: &Scenario| -> Box<dyn Agent> {
            tier.agent(class, scenario.script.clone())
        };
        let tier_report = suite::run_matrix(&scenarios, &tier_factory);
        rows.push(CalibrationRow::from_report(tier.slug(), &tier_report));
    }
    fs::write(
        reports.join("calibration.md"),
        report::calibration_markdown(&rows),
    )?;
    fs::write(
        reports.join("calibration.json"),
        grokptah_cu_bench::digest::canonical_json_pretty(&rows),
    )?;
    println!("wrote {}", reports.join("calibration.md").display());

    // Comparison traces. The reference is published across the whole matrix,
    // because a lab reproducing this benchmark has to reproduce all of it.
    // The tiers are published only at the canonical cell -- one cell is
    // enough to show the thresholds discriminate, and publishing the rest
    // would be churn without evidence.
    let traces = root.join("traces");
    fs::create_dir_all(&traces)?;
    for model_class in ModelClass::ALL {
        for profile in ProfileId::ALL {
            let trace = suite::record_trace(
                "reference",
                EvidenceClass::SyntheticFixture,
                *model_class,
                *profile,
                &scenarios,
                &factory,
            );
            let path = traces.join(format!(
                "reference-{}-{}.json",
                model_class.slug(),
                profile.slug()
            ));
            fs::write(
                &path,
                grokptah_cu_bench::digest::canonical_json_pretty(&trace),
            )?;
        }
    }
    let (canonical_class, canonical_profile) = suite::CANONICAL_COMPARISON_CELL;
    for tier in CalibrationTier::ALL {
        let tier = *tier;
        let tier_factory = move |class: ModelClass, scenario: &Scenario| -> Box<dyn Agent> {
            tier.agent(class, scenario.script.clone())
        };
        let trace = suite::record_trace(
            tier.slug(),
            EvidenceClass::SyntheticFixture,
            canonical_class,
            canonical_profile,
            &scenarios,
            &tier_factory,
        );
        let path = traces.join(format!(
            "{}-{}-{}.json",
            tier.slug(),
            canonical_class.slug(),
            canonical_profile.slug()
        ));
        fs::write(
            &path,
            grokptah_cu_bench::digest::canonical_json_pretty(&trace),
        )?;
    }
    println!("wrote {} comparison traces", traces.read_dir()?.count());

    println!("{}", report::one_line(&suite_report));
    Ok(())
}
