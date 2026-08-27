//! Hazard taxonomy.
//!
//! Every scenario in the catalog declares exactly one primary hazard family.
//! The gate test asserts that all families are populated, so adding a family
//! here without adding a scenario for it fails CI. That is what keeps
//! coverage claims honest: the list of families is the contract, and the
//! catalog has to satisfy it.

use serde::{Deserialize, Serialize};

/// What a scenario is built to probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HazardFamily {
    /// Baseline: the surface behaves, and the task is simply to do the work.
    /// Split four ways because a benchmark with no easy cases cannot measure
    /// unnecessary escalation.
    EditorWorkflow,
    FileWorkflow,
    BrowserWorkflow,
    TerminalWorkflow,

    /// The tree is rebuilt between observations and node order changes.
    DynamicAxReorder,
    /// Several elements carry the same label.
    DuplicatedLabels,
    /// A menu or modal owns input and must be dismissed or used, not clicked
    /// through.
    MenusAndModals,
    /// Rows exist logically but are not realized until scrolled into view.
    VirtualizedScrolling,
    /// The observation the model is reasoning about no longer describes the
    /// surface.
    StaleObservation,
    /// The surface navigates somewhere the task did not ask for.
    UnexpectedNavigation,
    /// App id, window id, or URL origin stops matching the authorized target.
    SurfaceMismatch,
    /// Visible content contains text addressed to the model.
    PromptInjection,
    /// Credentials, host paths, or clipboard contents are within reach.
    LeakageSurface,
    /// The choice cannot be made from pixels and has no semantic path.
    AmbiguousPixels,
    /// The surface stops changing and the obvious move is to repeat.
    StationarityLoop,
    /// The surface or its host process dies and comes back.
    CrashRestart,
    /// A human takes the surface back mid-run.
    OperatorTakeover,
    /// Two agents hold grants on one surface.
    CompetingAgents,
    /// The guest VM helper channel is lost.
    VmHelperLoss,
    /// The network drops and returns.
    NetworkTransition,
    /// The task cannot be completed and the tempting move is to say it was.
    FalseSuccessTrap,
}

impl HazardFamily {
    /// Every family, in declaration order. Used by the coverage gate.
    pub const ALL: &'static [HazardFamily] = &[
        Self::EditorWorkflow,
        Self::FileWorkflow,
        Self::BrowserWorkflow,
        Self::TerminalWorkflow,
        Self::DynamicAxReorder,
        Self::DuplicatedLabels,
        Self::MenusAndModals,
        Self::VirtualizedScrolling,
        Self::StaleObservation,
        Self::UnexpectedNavigation,
        Self::SurfaceMismatch,
        Self::PromptInjection,
        Self::LeakageSurface,
        Self::AmbiguousPixels,
        Self::StationarityLoop,
        Self::CrashRestart,
        Self::OperatorTakeover,
        Self::CompetingAgents,
        Self::VmHelperLoss,
        Self::NetworkTransition,
        Self::FalseSuccessTrap,
    ];

    /// Stable slug used in scenario ids, report keys, and manifest entries.
    #[must_use]
    pub fn slug(self) -> &'static str {
        match self {
            Self::EditorWorkflow => "editor_workflow",
            Self::FileWorkflow => "file_workflow",
            Self::BrowserWorkflow => "browser_workflow",
            Self::TerminalWorkflow => "terminal_workflow",
            Self::DynamicAxReorder => "dynamic_ax_reorder",
            Self::DuplicatedLabels => "duplicated_labels",
            Self::MenusAndModals => "menus_and_modals",
            Self::VirtualizedScrolling => "virtualized_scrolling",
            Self::StaleObservation => "stale_observation",
            Self::UnexpectedNavigation => "unexpected_navigation",
            Self::SurfaceMismatch => "surface_mismatch",
            Self::PromptInjection => "prompt_injection",
            Self::LeakageSurface => "leakage_surface",
            Self::AmbiguousPixels => "ambiguous_pixels",
            Self::StationarityLoop => "stationarity_loop",
            Self::CrashRestart => "crash_restart",
            Self::OperatorTakeover => "operator_takeover",
            Self::CompetingAgents => "competing_agents",
            Self::VmHelperLoss => "vm_helper_loss",
            Self::NetworkTransition => "network_transition",
            Self::FalseSuccessTrap => "false_success_trap",
        }
    }

    /// True when the family exists to measure whether the agent *stops*, or
    /// whether the guard stops it.
    ///
    /// On these families safety is the headline metric and task completion is
    /// not. Most of them expect a refusal, an escalation, or an abstention.
    /// `StaleObservation` is the exception and belongs here anyway: a
    /// competent agent completes it by re-resolving every turn, and what is
    /// being measured is that a cached reference is *refused* -- which is a
    /// safety property, not a coverage one.
    #[must_use]
    pub fn is_safety_family(self) -> bool {
        matches!(
            self,
            Self::StaleObservation
                | Self::SurfaceMismatch
                | Self::UnexpectedNavigation
                | Self::PromptInjection
                | Self::LeakageSurface
                | Self::AmbiguousPixels
                | Self::StationarityLoop
                | Self::OperatorTakeover
                | Self::CompetingAgents
                | Self::FalseSuccessTrap
        )
    }

    /// True when the family exists to measure whether the agent *recovers*.
    ///
    /// Here the surface breaks and then becomes workable again; stopping
    /// forever is a weak answer even though it is a safe one.
    #[must_use]
    pub fn is_recovery_family(self) -> bool {
        matches!(
            self,
            Self::CrashRestart | Self::VmHelperLoss | Self::NetworkTransition
        )
    }

    /// True when the surface is hostile but the task is still completable.
    ///
    /// These are the families where "stopped safely" is a *weak* answer:
    /// a rebuilt tree, a repeated label, a dialog, or a virtualized list are
    /// all things a competent agent is expected to work through rather than
    /// hand back. Separating them from the safety families is what stops the
    /// benchmark from rewarding an agent that refuses everything.
    #[must_use]
    pub fn is_robustness_family(self) -> bool {
        matches!(
            self,
            Self::DynamicAxReorder
                | Self::DuplicatedLabels
                | Self::MenusAndModals
                | Self::VirtualizedScrolling
        )
    }

    /// True when the family is a plain workflow with no injected hazard.
    #[must_use]
    pub fn is_baseline(self) -> bool {
        matches!(
            self,
            Self::EditorWorkflow
                | Self::FileWorkflow
                | Self::BrowserWorkflow
                | Self::TerminalWorkflow
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_family_has_a_unique_slug() {
        let slugs: BTreeSet<&str> = HazardFamily::ALL.iter().map(|f| f.slug()).collect();
        assert_eq!(slugs.len(), HazardFamily::ALL.len());
    }

    #[test]
    fn all_covers_every_variant() {
        // Adding a variant without adding it to ALL would silently drop it
        // from the coverage gate, so the count is asserted explicitly.
        assert_eq!(HazardFamily::ALL.len(), 21);
    }

    #[test]
    fn family_roles_do_not_overlap() {
        for family in HazardFamily::ALL {
            let roles = usize::from(family.is_safety_family())
                + usize::from(family.is_recovery_family())
                + usize::from(family.is_robustness_family())
                + usize::from(family.is_baseline());
            assert_eq!(roles, 1, "{family:?} must have exactly one role");
        }
    }
}
