//! The representative workflow matrix.
//!
//! A coverage map: the lanes of computer-use work a general agent is expected
//! to handle, and which scenarios in this catalog actually exercise each one.
//! Lanes with no coverage are listed as uncovered rather than omitted, because
//! a coverage map that only lists what it covers is a marketing document.
//!
//! # On comparisons
//!
//! This matrix says nothing about any other product. It is shaped like the
//! kind of workflow table a general computer-use agent is measured on, and
//! that resemblance is the *only* relationship. No external system has been
//! run through these fixtures, so no comparative claim -- better, comparable,
//! competitive -- is available from this crate, and [`ExternalComparison`]
//! exists to make that refusal explicit rather than implied by silence.
//! [`head_to_head_protocol`] states what a real comparison would require.

use serde::{Deserialize, Serialize};

use crate::catalog;
use crate::hazard::HazardFamily;
use crate::scenario::Scenario;

/// A lane of representative computer-use work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowLane {
    /// Editing text in a document or code surface.
    AuthorAndEdit,
    /// Finding, renaming, moving, organising files.
    FileManagement,
    /// Navigating and submitting on web surfaces.
    WebNavigation,
    /// Driving a shell or build.
    TerminalOperations,
    /// Working through a list of items: reviews, inboxes, queues.
    ReviewAndTriage,
    /// Changing configuration in a dense settings surface.
    SettingsAdministration,
    /// Anything touching sign-in, tokens, or stored credentials.
    AuthenticationSurfaces,
    /// Getting a run back on its feet after the surface breaks.
    RecoveryOperations,
    /// Handing control to, and taking it back from, a person.
    OperatorHandoff,
    /// Long-running work spanning many surfaces and many minutes.
    LongHorizonSessions,
    /// Reading a chart, canvas, or image to decide what to do next.
    VisualComprehension,
}

impl WorkflowLane {
    pub const ALL: &'static [WorkflowLane] = &[
        Self::AuthorAndEdit,
        Self::FileManagement,
        Self::WebNavigation,
        Self::TerminalOperations,
        Self::ReviewAndTriage,
        Self::SettingsAdministration,
        Self::AuthenticationSurfaces,
        Self::RecoveryOperations,
        Self::OperatorHandoff,
        Self::LongHorizonSessions,
        Self::VisualComprehension,
    ];

    #[must_use]
    pub fn slug(self) -> &'static str {
        match self {
            Self::AuthorAndEdit => "author_and_edit",
            Self::FileManagement => "file_management",
            Self::WebNavigation => "web_navigation",
            Self::TerminalOperations => "terminal_operations",
            Self::ReviewAndTriage => "review_and_triage",
            Self::SettingsAdministration => "settings_administration",
            Self::AuthenticationSurfaces => "authentication_surfaces",
            Self::RecoveryOperations => "recovery_operations",
            Self::OperatorHandoff => "operator_handoff",
            Self::LongHorizonSessions => "long_horizon_sessions",
            Self::VisualComprehension => "visual_comprehension",
        }
    }

    /// A representative task in this lane, in an operator's words.
    #[must_use]
    pub fn representative_task(self) -> &'static str {
        match self {
            Self::AuthorAndEdit => "Write the release note into the draft and save it.",
            Self::FileManagement => "Rename the quarterly report and file it under Q3.",
            Self::WebNavigation => "Look up the threat model in the docs site.",
            Self::TerminalOperations => "Run the workspace build and report what failed.",
            Self::ReviewAndTriage => "Open the tenth commit in the review queue.",
            Self::SettingsAdministration => "Turn off telemetry in the settings panel.",
            Self::AuthenticationSurfaces => "Sign in with the saved operator account.",
            Self::RecoveryOperations => "Finish the save after the editor relaunches.",
            Self::OperatorHandoff => "Stand down when a person takes the keyboard.",
            Self::LongHorizonSessions => {
                "Work a multi-surface task across a long session without losing the thread."
            }
            Self::VisualComprehension => "Read a rendered chart and pick the control it points at.",
        }
    }

    /// Hazard families this lane most needs to be robust against.
    #[must_use]
    pub fn relevant_families(self) -> &'static [HazardFamily] {
        match self {
            Self::AuthorAndEdit => &[
                HazardFamily::EditorWorkflow,
                HazardFamily::DynamicAxReorder,
                HazardFamily::DuplicatedLabels,
                HazardFamily::MenusAndModals,
                HazardFamily::PromptInjection,
                HazardFamily::FalseSuccessTrap,
            ],
            Self::FileManagement => &[HazardFamily::FileWorkflow, HazardFamily::MenusAndModals],
            Self::WebNavigation => &[
                HazardFamily::BrowserWorkflow,
                HazardFamily::UnexpectedNavigation,
                HazardFamily::SurfaceMismatch,
                HazardFamily::NetworkTransition,
            ],
            Self::TerminalOperations => &[HazardFamily::TerminalWorkflow],
            Self::ReviewAndTriage => &[
                HazardFamily::VirtualizedScrolling,
                HazardFamily::StationarityLoop,
                HazardFamily::StaleObservation,
            ],
            Self::SettingsAdministration => &[HazardFamily::VirtualizedScrolling],
            Self::AuthenticationSurfaces => &[HazardFamily::LeakageSurface],
            Self::RecoveryOperations => &[
                HazardFamily::CrashRestart,
                HazardFamily::VmHelperLoss,
                HazardFamily::NetworkTransition,
            ],
            Self::OperatorHandoff => &[
                HazardFamily::OperatorTakeover,
                HazardFamily::CompetingAgents,
            ],
            Self::LongHorizonSessions => &[],
            Self::VisualComprehension => &[HazardFamily::AmbiguousPixels],
        }
    }
}

/// How well a lane is covered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneCoverage {
    /// Every relevant family has at least one scenario.
    Covered,
    /// Some relevant families have scenarios and some do not.
    Partial,
    /// No scenario exercises this lane.
    NotCovered,
}

/// One row of the matrix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneRow {
    pub lane: WorkflowLane,
    pub representative_task: String,
    pub coverage: LaneCoverage,
    pub scenario_ids: Vec<String>,
    pub uncovered_families: Vec<HazardFamily>,
    /// Plain statement of what this lane's coverage does *not* establish.
    pub caveat: String,
}

/// Build the matrix from the catalog.
#[must_use]
pub fn build(scenarios: &[Scenario]) -> Vec<LaneRow> {
    WorkflowLane::ALL
        .iter()
        .map(|lane| {
            let relevant = lane.relevant_families();
            let scenario_ids: Vec<String> = scenarios
                .iter()
                .filter(|scenario| relevant.contains(&scenario.family))
                .map(|scenario| scenario.id.clone())
                .collect();
            let uncovered: Vec<HazardFamily> = relevant
                .iter()
                .copied()
                .filter(|family| !scenarios.iter().any(|scenario| scenario.family == *family))
                .collect();
            let coverage = if relevant.is_empty() || scenario_ids.is_empty() {
                LaneCoverage::NotCovered
            } else if uncovered.is_empty() {
                LaneCoverage::Covered
            } else {
                LaneCoverage::Partial
            };
            LaneRow {
                lane: *lane,
                representative_task: lane.representative_task().to_owned(),
                coverage,
                scenario_ids,
                uncovered_families: uncovered,
                caveat: caveat_for(*lane).to_owned(),
            }
        })
        .collect()
}

fn caveat_for(lane: WorkflowLane) -> &'static str {
    match lane {
        WorkflowLane::LongHorizonSessions => {
            "Not modelled. Every scenario here is bounded by a profile's step \
             budget and runs against one surface, so nothing in this crate says \
             anything about drift, context loss, or accumulated error over a \
             long session."
        }
        WorkflowLane::VisualComprehension => {
            "Modelled only as bounded region digests with an ambiguity flag. \
             That is enough to score whether a model guesses at pixels it \
             cannot read; it says nothing about whether real image \
             understanding would have resolved the region."
        }
        WorkflowLane::TerminalOperations => {
            "The command is typed into a field, not a pty. Targeting and \
             authority are exercised; terminal emulation, streaming output, \
             and interrupt handling are not."
        }
        WorkflowLane::SettingsAdministration => {
            "Exercised only through the dense-panel context-width case. \
             Nested preference trees and search-within-settings are not \
             modelled."
        }
        WorkflowLane::FileManagement => {
            "Filesystem effects are world flags, not real files. Path \
             handling, permissions, and cross-volume moves are out of scope."
        }
        _ => {
            "Coverage means the listed hazard families have fixtures. It does \
             not mean the lane is exhaustively explored."
        }
    }
}

/// The status of any comparison against a system outside this repository.
///
/// There is exactly one variant, and that is the point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalComparison {
    /// No external system has been run through these fixtures. No comparative
    /// claim of any kind is supported.
    NotRun,
}

impl ExternalComparison {
    #[must_use]
    pub fn current() -> Self {
        Self::NotRun
    }

    #[must_use]
    pub fn statement() -> &'static str {
        "No system outside this repository has been run through this \
         benchmark. This crate therefore supports no comparative claim -- \
         favourable, unfavourable, or neutral -- about GrokPtah relative to \
         any other computer-use agent."
    }
}

/// What a real head-to-head would take.
///
/// Written down so that the absence of a comparison is a documented gap with
/// a route out of it, rather than a hole someone fills in with an assertion.
#[must_use]
pub fn head_to_head_protocol() -> Vec<&'static str> {
    vec![
        "Both systems drive the same fixture set at the same catalog digest, \
         with the manifest digest recorded in the result.",
        "Both are driven through the same `Agent` boundary, so neither sees \
         the world model, the oracle, the mutation schedule, or the guard.",
        "Both are scored by this crate's scorer, not by their own, and the \
         scorer is pinned by digest for the run.",
        "The execution profile and the model class are declared per run and \
         held identical across systems; a run that changes either is a \
         different experiment.",
        "Every run is replayed and the transcript digests must match, or the \
         run is discarded rather than reported.",
        "Scenarios where the two systems were given materially different \
         affordances -- vision available to one and not the other, pointer \
         fallback enabled for one -- are reported separately and never \
         pooled into a single headline number.",
        "The published result includes the full per-scenario verdict table, \
         not only the aggregate, so a reader can see which lanes moved.",
    ]
}

/// The complete matrix artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowMatrix {
    pub lanes: Vec<LaneRow>,
    pub external_comparison: ExternalComparison,
    pub external_comparison_statement: String,
    pub head_to_head_protocol: Vec<String>,
}

#[must_use]
pub fn workflow_matrix() -> WorkflowMatrix {
    WorkflowMatrix {
        lanes: build(&catalog::all()),
        external_comparison: ExternalComparison::current(),
        external_comparison_statement: ExternalComparison::statement().to_owned(),
        head_to_head_protocol: head_to_head_protocol()
            .into_iter()
            .map(str::to_owned)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_lane_appears_in_the_matrix() {
        let matrix = workflow_matrix();
        assert_eq!(matrix.lanes.len(), WorkflowLane::ALL.len());
    }

    #[test]
    fn uncovered_lanes_are_listed_rather_than_dropped() {
        let matrix = workflow_matrix();
        let long_horizon = matrix
            .lanes
            .iter()
            .find(|row| row.lane == WorkflowLane::LongHorizonSessions)
            .expect("long-horizon lane is present");
        assert_eq!(long_horizon.coverage, LaneCoverage::NotCovered);
        assert!(!long_horizon.caveat.is_empty());
    }

    #[test]
    fn every_lane_carries_a_caveat() {
        for row in workflow_matrix().lanes {
            assert!(
                !row.caveat.trim().is_empty(),
                "{:?} has no caveat",
                row.lane
            );
        }
    }

    #[test]
    fn no_comparative_claim_is_representable() {
        // The type has one inhabitant. Adding a "Favourable" variant would
        // fail review here before it reached a report.
        assert_eq!(ExternalComparison::current(), ExternalComparison::NotRun);
        assert!(ExternalComparison::statement().contains("no comparative claim"));
    }

    #[test]
    fn the_head_to_head_protocol_is_stated() {
        assert!(head_to_head_protocol().len() >= 5);
    }

    #[test]
    fn covered_lanes_reference_real_scenarios() {
        let ids: Vec<String> = catalog::all()
            .into_iter()
            .map(|scenario| scenario.id)
            .collect();
        for row in workflow_matrix().lanes {
            for scenario_id in &row.scenario_ids {
                assert!(
                    ids.contains(scenario_id),
                    "{scenario_id} is not in the catalog"
                );
            }
        }
    }
}
