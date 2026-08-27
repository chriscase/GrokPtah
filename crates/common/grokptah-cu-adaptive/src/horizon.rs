//! Task horizons.
//!
//! Three lengths, an order of magnitude apart: 3 steps, 30 steps, 300 steps.
//! The gaps are the point. A contract that only ever runs three steps never
//! finds out what its retry accounting does when the same element drifts
//! forty times, and a contract that only ever runs three hundred hides
//! whether a short task pays a fixed setup cost it cannot amortize.
//!
//! Horizon is an input to the budget envelope, not a budget itself: a 300-step
//! run does not get a hundred times the allowance of a 3-step run, because
//! per-run costs (the first plan, the lease, the final receipt) do not scale
//! with length while per-step costs do. See [`crate::budget`].

use serde::{Deserialize, Serialize};

/// How long the task is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Horizon {
    /// Three steps. Setup cost dominates.
    Short,
    /// Thirty steps. The regime most real tasks sit in.
    Medium,
    /// Three hundred steps. Accounting, drift, and budget pressure dominate.
    Long,
}

impl Horizon {
    pub const ALL: &'static [Horizon] = &[Self::Short, Self::Medium, Self::Long];

    /// The number of task steps at this horizon.
    #[must_use]
    pub fn steps(self) -> u32 {
        match self {
            Self::Short => 3,
            Self::Medium => 30,
            Self::Long => 300,
        }
    }

    #[must_use]
    pub fn slug(self) -> &'static str {
        match self {
            Self::Short => "h3",
            Self::Medium => "h30",
            Self::Long => "h300",
        }
    }

    /// The horizon that contains at least `steps` steps, if any.
    #[must_use]
    pub fn containing(steps: u32) -> Option<Horizon> {
        Self::ALL.iter().copied().find(|h| h.steps() >= steps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizons_are_an_order_of_magnitude_apart() {
        assert_eq!(Horizon::Short.steps(), 3);
        assert_eq!(Horizon::Medium.steps(), 30);
        assert_eq!(Horizon::Long.steps(), 300);
        assert_eq!(Horizon::Medium.steps(), Horizon::Short.steps() * 10);
        assert_eq!(Horizon::Long.steps(), Horizon::Medium.steps() * 10);
    }

    #[test]
    fn horizons_are_ordered_and_slugs_are_distinct() {
        assert!(Horizon::Short < Horizon::Medium);
        assert!(Horizon::Medium < Horizon::Long);
        let slugs: std::collections::BTreeSet<_> = Horizon::ALL.iter().map(|h| h.slug()).collect();
        assert_eq!(slugs.len(), Horizon::ALL.len());
    }

    #[test]
    fn containing_picks_the_smallest_sufficient_horizon() {
        assert_eq!(Horizon::containing(1), Some(Horizon::Short));
        assert_eq!(Horizon::containing(3), Some(Horizon::Short));
        assert_eq!(Horizon::containing(4), Some(Horizon::Medium));
        assert_eq!(Horizon::containing(300), Some(Horizon::Long));
        assert_eq!(Horizon::containing(301), None);
    }
}
