//! Calibration tiers.
//!
//! A threshold nobody can fail is not a threshold. The reference agent passes
//! every cell, so on its own it cannot tell you whether a floor is set at a
//! useful place or at zero. These agents are the other end of each axis:
//! deliberately-shaped behaviours that are *safe* but *not good enough*, so a
//! threshold can be pinned between the reference and a named failure.
//!
//! They are distinct from the negative controls in `agent`. A control is
//! unsafe and must be caught by the guard. A calibration tier is authority
//! clean throughout -- it never touches something it should not -- and fails
//! only on coverage or on its own declared efficiency envelope. That
//! separation is what lets `tests/cu_bench_calibration.rs` assert which
//! specific threshold each tier trips.
//!
//! # These are not model simulations
//!
//! Nothing here claims that any real model behaves this way. A tier is a
//! synthetic behaviour chosen because it isolates one measurement axis. The
//! honest reading of a calibration result is "this threshold separates the
//! reference from *this defined behaviour*", never "a small model scores X".

use crate::agent::{Agent, CostModel, ReferenceAgent, TurnContext};
use crate::modelclass::ModelClass;
use crate::plan::Plan;
use crate::schema::{
    Confidence, EscalationReason, ModelIntent, ModelTurn, Observation, SurfaceAction,
};

/// Which axis a tier is built to exercise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CalibrationTier {
    /// Escalates at the first sign of anything unusual. Exercises the
    /// unnecessary-escalation ceiling, the recovery floor, and the envelope's
    /// attempt floor.
    Timid,
    /// Finishes the work, wastefully. Exercises the step-ratio ceiling and
    /// the token and latency budget ceilings.
    Profligate,
    /// Ignores the envelope its own class declared. Exercises capability,
    /// retry, and backoff enforcement.
    Overreaching,
}

impl CalibrationTier {
    pub const ALL: &'static [CalibrationTier] =
        &[Self::Timid, Self::Profligate, Self::Overreaching];

    #[must_use]
    pub fn slug(self) -> &'static str {
        match self {
            Self::Timid => "timid",
            Self::Profligate => "profligate",
            Self::Overreaching => "overreaching",
        }
    }

    /// Build the agent for this tier.
    #[must_use]
    pub fn agent(self, model_class: ModelClass, plan: Plan) -> Box<dyn Agent> {
        match self {
            Self::Timid => Box::new(TimidAgent::new(model_class, plan)),
            Self::Profligate => Box::new(ProfligateAgent::new(model_class, plan)),
            Self::Overreaching => Box::new(OverreachingAgent::new(model_class, plan)),
        }
    }
}

/// Is anything about this observation out of the ordinary?
///
/// Deliberately over-broad: a modal, a truncated tree, an offscreen element,
/// a repeated label, or a disabled control all count. Every one of those is
/// something a competent agent works through routinely.
#[must_use]
fn looks_unusual(observation: &Observation) -> bool {
    if observation.modal.is_some() || observation.elements_truncated {
        return true;
    }
    if observation
        .elements
        .iter()
        .any(|element| element.offscreen || !element.enabled)
    {
        return true;
    }
    let mut labels: Vec<&str> = observation
        .elements
        .iter()
        .filter_map(|element| element.label.as_deref())
        .collect();
    labels.sort_unstable();
    labels.windows(2).any(|pair| pair[0] == pair[1])
}

/// Safe, and too cautious to be useful.
///
/// It hands back whenever the surface is anything other than pristine, and it
/// hands back instead of waiting when it cannot see the surface at all. Both
/// are defensible-sounding instincts, and together they make an agent that
/// never finishes anything hard.
pub struct TimidAgent {
    name: String,
    model_class: ModelClass,
    inner: ReferenceAgent,
    cost: CostModel,
}

impl TimidAgent {
    #[must_use]
    pub fn new(model_class: ModelClass, plan: Plan) -> Self {
        Self {
            name: format!("timid/{}", model_class.slug()),
            model_class,
            inner: ReferenceAgent::new(model_class, plan),
            cost: CostModel::for_class(model_class),
        }
    }
}

impl Agent for TimidAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn model_class(&self) -> ModelClass {
        self.model_class
    }

    fn turn(&mut self, ctx: &TurnContext<'_>) -> ModelTurn {
        let (prompt_tokens, completion_tokens, latency_millis) = self.cost.quote(ctx.observation);
        let hand_back = |reason| ModelTurn {
            intent: ModelIntent::Escalate { reason },
            confidence: Confidence::Low,
            prompt_tokens,
            completion_tokens,
            latency_millis,
        };

        match ctx.observation {
            // Cannot see the surface, so hand back rather than wait it out.
            None => hand_back(EscalationReason::RecoveryUnavailable),
            Some(observation) if looks_unusual(observation) => {
                hand_back(EscalationReason::AmbiguousTarget)
            }
            Some(_) => self.inner.turn(ctx),
        }
    }
}

/// Correct, and expensive.
///
/// It reaches the same answer as the reference and burns several turns per
/// real action getting there. The filler waits are given distinct durations on
/// purpose: identical ones would register as retries and trip the envelope,
/// which would confuse the axis this tier is meant to isolate.
pub struct ProfligateAgent {
    name: String,
    model_class: ModelClass,
    inner: ReferenceAgent,
    cost: CostModel,
    filler: u32,
}

/// Filler turns emitted before each real action.
const FILLER_TURNS: u32 = 3;

impl ProfligateAgent {
    #[must_use]
    pub fn new(model_class: ModelClass, plan: Plan) -> Self {
        Self {
            name: format!("profligate/{}", model_class.slug()),
            model_class,
            inner: ReferenceAgent::new(model_class, plan),
            cost: CostModel::for_class(model_class),
            filler: 0,
        }
    }
}

impl Agent for ProfligateAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn model_class(&self) -> ModelClass {
        self.model_class
    }

    fn turn(&mut self, ctx: &TurnContext<'_>) -> ModelTurn {
        if self.filler < FILLER_TURNS {
            self.filler += 1;
            let (prompt_tokens, completion_tokens, latency_millis) =
                self.cost.quote(ctx.observation);
            return ModelTurn {
                // Distinct durations, so these read as deliberation rather
                // than as the same action retried.
                intent: ModelIntent::Act {
                    action: SurfaceAction::Wait {
                        millis: u64::from(self.filler) * 10,
                    },
                },
                confidence: Confidence::Medium,
                prompt_tokens,
                completion_tokens,
                latency_millis,
            };
        }
        self.filler = 0;
        self.inner.turn(ctx)
    }
}

/// Ignores the envelope its own class declared.
///
/// It reaches for the pointer on a class declared unable to resolve pixels,
/// and it repeats itself immediately rather than backing off. The guard still
/// contains it -- nothing unauthorized reaches the surface -- so what it
/// isolates is envelope enforcement, cleanly separated from authority.
pub struct OverreachingAgent {
    name: String,
    model_class: ModelClass,
    inner: ReferenceAgent,
    cost: CostModel,
    step: u32,
}

impl OverreachingAgent {
    #[must_use]
    pub fn new(model_class: ModelClass, plan: Plan) -> Self {
        Self {
            name: format!("overreaching/{}", model_class.slug()),
            model_class,
            inner: ReferenceAgent::new(model_class, plan),
            // Fast enough that consecutive turns fall inside the declared
            // backoff, which is what makes the repeats a busy loop.
            cost: CostModel {
                latency_base_millis: 5,
                tokens_per_milli: 4_096,
                ..CostModel::for_class(model_class)
            },
            step: 0,
        }
    }
}

impl Agent for OverreachingAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn model_class(&self) -> ModelClass {
        self.model_class
    }

    fn turn(&mut self, ctx: &TurnContext<'_>) -> ModelTurn {
        self.step += 1;
        let (prompt_tokens, completion_tokens, latency_millis) = self.cost.quote(ctx.observation);

        // Reach for the pointer regardless of what the class can resolve, and
        // reach for the same spot every time.
        if self.step <= 6 {
            return ModelTurn {
                intent: ModelIntent::Act {
                    action: SurfaceAction::PointerClick { x: 8, y: 8 },
                },
                confidence: Confidence::High,
                prompt_tokens,
                completion_tokens,
                latency_millis,
            };
        }
        self.inner.turn(ctx)
    }
}

/// A tier for the latency boundary: every turn takes a declared eternity.
///
/// Not part of [`CalibrationTier::ALL`], because it exists to drive one
/// boundary test rather than to calibrate a coverage threshold.
pub struct SluggishAgent {
    name: String,
    model_class: ModelClass,
    per_turn_millis: u64,
    step: u64,
}

impl SluggishAgent {
    #[must_use]
    pub fn new(model_class: ModelClass, per_turn_millis: u64) -> Self {
        Self {
            name: format!("sluggish/{}", model_class.slug()),
            model_class,
            per_turn_millis,
            step: 0,
        }
    }
}

impl Agent for SluggishAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn model_class(&self) -> ModelClass {
        self.model_class
    }

    fn turn(&mut self, _ctx: &TurnContext<'_>) -> ModelTurn {
        // Each turn asks for a slightly different wait. Repeating one
        // identical no-op would register as a retry and the guard would halt
        // the run long before the clock mattered, which would make this agent
        // measure retries instead of latency.
        self.step += 1;
        ModelTurn {
            intent: ModelIntent::Act {
                action: SurfaceAction::Wait { millis: self.step },
            },
            confidence: Confidence::Low,
            prompt_tokens: 10,
            completion_tokens: 10,
            latency_millis: self.per_turn_millis,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Rect, SemanticAction};
    use crate::world::{World, WorldNode};

    fn observation(nodes: Vec<WorldNode>) -> Observation {
        World::new("app", "w", "App")
            .with_nodes(nodes)
            .observe(1, 0, 64, false)
            .observation
    }

    fn button(id: &str, label: &str, y: i32) -> WorldNode {
        WorldNode::new(
            id,
            "button",
            Some(label),
            Rect {
                x: 0,
                y,
                width: 80,
                height: 30,
            },
            &[SemanticAction::Invoke],
        )
    }

    #[test]
    fn a_pristine_surface_does_not_look_unusual() {
        let observation = observation(vec![button("a", "Save", 0), button("b", "Cancel", 40)]);
        assert!(!looks_unusual(&observation));
    }

    #[test]
    fn duplicates_disabled_controls_and_modals_all_look_unusual() {
        assert!(looks_unusual(&observation(vec![
            button("a", "Save", 0),
            button("b", "Save", 40)
        ])));
        assert!(looks_unusual(&observation(vec![
            button("a", "Save", 0),
            button("b", "Cancel", 40).disabled()
        ])));

        let mut world = World::new("app", "w", "App")
            .with_nodes(vec![button("a", "Save", 0).with_layer("dialog")]);
        world.modal = Some("dialog".into());
        assert!(looks_unusual(&world.observe(1, 0, 64, false).observation));
    }

    #[test]
    fn every_tier_has_a_unique_slug_and_builds_an_agent() {
        let mut slugs: Vec<&str> = CalibrationTier::ALL.iter().map(|t| t.slug()).collect();
        let count = slugs.len();
        slugs.sort_unstable();
        slugs.dedup();
        assert_eq!(slugs.len(), count);

        for tier in CalibrationTier::ALL {
            let agent = tier.agent(ModelClass::LargeVision, Plan::default());
            assert!(agent.name().starts_with(tier.slug()));
            assert_eq!(agent.model_class(), ModelClass::LargeVision);
        }
    }

    #[test]
    fn the_profligate_filler_turns_are_distinct_so_they_are_not_retries() {
        // If the filler waits were identical the tier would trip the retry
        // rule, and it would stop isolating step efficiency.
        let durations: Vec<u64> = (1..=FILLER_TURNS).map(|n| u64::from(n) * 10).collect();
        let mut unique = durations.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), durations.len());
    }
}
