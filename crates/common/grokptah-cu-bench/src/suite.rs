//! Running the matrix.
//!
//! One suite run is every scenario, under every execution profile, for every
//! model class -- with the reference agent, twice. The second pass exists
//! only to check that the transcript digests match; determinism is claimed in
//! the report, so it is measured rather than asserted.

use serde::{Deserialize, Serialize};

use crate::agent::{Agent, ReferenceAgent};
use crate::digest::fold_digests;
use crate::modelclass::ModelClass;
use crate::profile::{ExecutionProfile, ProfileId};
use crate::runner::{RunRecord, execute};
use crate::scenario::Scenario;
use crate::scoring::{CellQualification, CellScore, qualify, score_cell};

/// Build the agent for one cell.
///
/// Taking a factory rather than an agent means the caller can substitute a
/// candidate implementation without the suite knowing anything about it --
/// which is what makes this a qualification harness rather than a self-test.
pub type AgentFactory<'a> = &'a dyn Fn(ModelClass, &Scenario) -> Box<dyn Agent>;

/// The reference agent factory: a competent, hazard-aware policy.
pub fn reference_factory() -> impl Fn(ModelClass, &Scenario) -> Box<dyn Agent> {
    |model_class, scenario| Box::new(ReferenceAgent::new(model_class, scenario.script.clone()))
}

/// Run every scenario for one (model class, profile) cell.
#[must_use]
pub fn run_cell(
    model_class: ModelClass,
    profile: &ExecutionProfile,
    scenarios: &[Scenario],
    factory: AgentFactory<'_>,
) -> Vec<RunRecord> {
    scenarios
        .iter()
        .map(|scenario| {
            let mut agent = factory(model_class, scenario);
            execute(scenario, profile, agent.as_mut())
        })
        .collect()
}

/// Whether a second identical pass produced identical transcripts.
#[must_use]
pub fn replay_matches(
    model_class: ModelClass,
    profile: &ExecutionProfile,
    scenarios: &[Scenario],
    factory: AgentFactory<'_>,
    first: &[RunRecord],
) -> bool {
    let second = run_cell(model_class, profile, scenarios, factory);
    first.len() == second.len()
        && first
            .iter()
            .zip(second.iter())
            .all(|(a, b)| a.transcript_digest == b.transcript_digest)
}

/// A complete suite result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuiteReport {
    pub schema_version: String,
    pub scenario_count: u32,
    pub scenario_catalog_digest: String,
    pub cells: Vec<CellScore>,
    pub qualifications: Vec<CellQualification>,
    /// Digest over every transcript in the suite, in cell order.
    pub suite_digest: String,
}

impl SuiteReport {
    /// True when every cell met every threshold.
    #[must_use]
    pub fn fully_qualified(&self) -> bool {
        self.qualifications.iter().all(|cell| cell.passed)
    }

    /// True when no cell breached an authority or privacy threshold, even if
    /// coverage fell short somewhere.
    #[must_use]
    pub fn authority_clean(&self) -> bool {
        self.qualifications.iter().all(|cell| cell.authority_clean)
    }

    #[must_use]
    pub fn cell(&self, model_class: ModelClass, profile: ProfileId) -> Option<&CellScore> {
        self.cells
            .iter()
            .find(|cell| cell.model_class == model_class && cell.profile == profile)
    }

    #[must_use]
    pub fn qualification(
        &self,
        model_class: ModelClass,
        profile: ProfileId,
    ) -> Option<&CellQualification> {
        self.qualifications
            .iter()
            .find(|cell| cell.model_class == model_class && cell.profile == profile)
    }
}

/// Run the full matrix.
#[must_use]
pub fn run_matrix(scenarios: &[Scenario], factory: AgentFactory<'_>) -> SuiteReport {
    let mut cells = Vec::new();
    let mut qualifications = Vec::new();
    let mut transcripts = Vec::new();

    for model_class in ModelClass::ALL {
        for profile_id in ProfileId::ALL {
            let profile = ExecutionProfile::for_id(*profile_id);
            let records = run_cell(*model_class, &profile, scenarios, factory);
            let deterministic =
                replay_matches(*model_class, &profile, scenarios, factory, &records);

            transcripts.extend(
                records
                    .iter()
                    .map(|record| record.transcript_digest.clone()),
            );

            let score = score_cell(*model_class, &profile, scenarios, &records, deterministic);
            qualifications.push(qualify(&score));
            cells.push(score);
        }
    }

    SuiteReport {
        schema_version: crate::schema::SCHEMA_VERSION.to_owned(),
        scenario_count: u32::try_from(scenarios.len()).unwrap_or(u32::MAX),
        scenario_catalog_digest: crate::digest::digest_of(&scenarios),
        cells,
        qualifications,
        suite_digest: fold_digests("grokptah.cu-bench/suite", &transcripts),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog;

    #[test]
    fn the_matrix_has_one_cell_per_model_class_and_profile() {
        let scenarios = catalog::all();
        let factory = reference_factory();
        let report = run_matrix(&scenarios, &factory);
        assert_eq!(
            report.cells.len(),
            ModelClass::ALL.len() * ProfileId::ALL.len()
        );
        assert_eq!(report.qualifications.len(), report.cells.len());
    }

    #[test]
    fn two_suite_runs_produce_the_same_digest() {
        let scenarios = catalog::all();
        let factory = reference_factory();
        let first = run_matrix(&scenarios, &factory);
        let second = run_matrix(&scenarios, &factory);
        assert_eq!(first.suite_digest, second.suite_digest);
    }
}
