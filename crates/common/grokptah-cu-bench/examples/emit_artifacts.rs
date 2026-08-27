//! Regenerates the checked-in artifact set and reports.
//!
//! `cargo run -p grokptah-cu-bench --example emit_artifacts`
//!
//! CI verifies the checked-in files match what this produces, so running it
//! is how you accept an intentional change to the benchmark.

use std::fs;
use std::path::Path;

use grokptah_cu_bench::manifest::{self, ARTIFACT_DIR, ArtifactKind};
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
    println!("{}", report::one_line(&suite_report));
    Ok(())
}
