//! The synthetic world.
//!
//! A deterministic in-process stand-in for an application: a list of elements,
//! a frame sequence, and a scripted set of perturbations that fire at fixed
//! step indices. It has no window server, no accessibility API, no process,
//! and no pixels. Its "region digests" are digests of a seed, not of anything
//! rendered.
//!
//! That is a real limitation and it is stated rather than papered over: a
//! contract that holds here has been shown to hold against *this* world's
//! failure modes -- drift, recycled identities, disabled controls, sensitivity
//! flips, backend refusals, latency spikes, mid-flight cancellation -- and
//! against nothing else. Every receipt carries
//! [`crate::vocabulary::NotClaimed::RealApplicationSemantics`] to say exactly
//! that.
//!
//! What the world *is* good for is being adversarial on demand and identical
//! on replay. A perturbation scheduled for step 17 fires at step 17 on every
//! machine, so a refusal that shows up once shows up every time, and a
//! contract change that stops producing it is visible immediately.

use crate::digest::{digest_str, domain};
use crate::grounding::LiveElement;
use crate::lease::FrameToken;
use crate::redaction::Sensitivity;
use crate::schema::{ElementRef, PostconditionOutcome, StepIntent};
use crate::vocabulary::DenyReason;

use super::rng::DeterministicRng;

/// One control in the synthetic application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntheticElement {
    pub id: String,
    pub generation: u64,
    pub role: String,
    pub enabled: bool,
    pub advertises: bool,
    pub sensitivity: Sensitivity,
    /// Seed for the region digest. Changing it is how the world says "what is
    /// rendered here is not what you saw".
    pub region_seed: u64,
}

impl SyntheticElement {
    #[must_use]
    pub fn live(&self) -> LiveElement {
        LiveElement {
            element: ElementRef {
                element_id: self.id.clone(),
                generation: self.generation,
            },
            role_digest: digest_str(domain::ELEMENT_ROLE, &self.role),
            region_digest: digest_str(domain::REGION, &format!("{}:{}", self.id, self.region_seed)),
            enabled: self.enabled,
            sensitivity: self.sensitivity,
            advertises: self.advertises,
        }
    }
}

/// A scripted change to the world, fired before the step at `at_step`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Perturbation {
    /// The element's identity is recycled: same id, new generation.
    RecycleIdentity { element: String },
    /// The element's role changes underneath its id.
    ChangeRole { element: String, role: String },
    /// The element becomes non-interactive.
    Disable { element: String },
    /// The element stops advertising the action.
    Unadvertise { element: String },
    /// The element becomes sensitive.
    SetSensitivity {
        element: String,
        sensitivity: Sensitivity,
    },
    /// The element disappears.
    Remove { element: String },
    /// What is rendered where the element is changes.
    Redraw { element: String },
    /// The backend refuses the next action.
    BackendRefuses,
    /// The next action reports success and does not take effect.
    PostconditionMisses,
    /// The next step takes far longer than usual.
    LatencySpike { millis: u64 },
    /// An operator takes the target over.
    OperatorTakeover,
}

/// One scheduled perturbation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledPerturbation {
    pub at_step: u32,
    pub perturbation: Perturbation,
}

/// The synthetic application.
#[derive(Debug, Clone)]
pub struct SyntheticWorld {
    elements: Vec<SyntheticElement>,
    schedule: Vec<ScheduledPerturbation>,
    sequence: u64,
    epoch: u64,
    clock_millis: u64,
    /// Seed for the frame digest.
    ///
    /// **Modeling choice, stated because it matters.** Here the frame digest
    /// covers the *rendered* surface: it moves on a redraw and on a dispatched
    /// mutation, and it does not move when an element is merely disabled,
    /// recycled, re-roled, or made sensitive. Those are caught instead by
    /// comparing the claim against the live element in
    /// [`crate::grounding::verify`].
    ///
    /// The production kernel is stricter -- it binds an action to the exact
    /// current observation, which subsumes both checks, so anything refused
    /// here would be refused there too. Splitting them in the synthetic world
    /// is what lets a trace say *which* guard fired: with one combined digest
    /// every perturbation would surface as `StaleFrame` and the drift,
    /// disabled-control, and sensitivity rules would never be observed
    /// failing on their own.
    frame_digest_seed: u64,
    backend_refuses_next: bool,
    postcondition_misses_next: bool,
    pending_latency_millis: u64,
    takeover_requested: bool,
    rng: DeterministicRng,
}

impl SyntheticWorld {
    /// Build a world with `element_count` ordinary controls.
    #[must_use]
    pub fn new(label: &str, element_count: u32, epoch: u64) -> Self {
        let mut rng = DeterministicRng::from_label(label);
        let elements = (0..element_count)
            .map(|index| SyntheticElement {
                id: format!("element-{index}"),
                generation: 1,
                role: if index % 3 == 0 {
                    "button"
                } else {
                    "text_field"
                }
                .to_string(),
                enabled: true,
                advertises: true,
                sensitivity: Sensitivity::None,
                region_seed: rng.next_u64(),
            })
            .collect();
        Self {
            elements,
            schedule: Vec::new(),
            sequence: 0,
            epoch,
            clock_millis: 1_000,
            frame_digest_seed: rng.next_u64(),
            backend_refuses_next: false,
            postcondition_misses_next: false,
            pending_latency_millis: 0,
            takeover_requested: false,
            rng,
        }
    }

    /// Add a scripted perturbation.
    pub fn schedule(&mut self, at_step: u32, perturbation: Perturbation) {
        self.schedule.push(ScheduledPerturbation {
            at_step,
            perturbation,
        });
    }

    #[must_use]
    pub fn clock_millis(&self) -> u64 {
        self.clock_millis
    }

    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Advance the synthetic clock.
    pub fn tick(&mut self, millis: u64) {
        self.clock_millis = self.clock_millis.saturating_add(millis);
    }

    /// True when a scripted operator takeover fired and has not been consumed.
    pub fn take_takeover_request(&mut self) -> bool {
        std::mem::take(&mut self.takeover_requested)
    }

    /// The extra latency the next step should be charged, consumed on read.
    pub fn take_pending_latency(&mut self) -> u64 {
        std::mem::take(&mut self.pending_latency_millis)
    }

    /// Fire every perturbation scheduled for this step.
    pub fn advance_to(&mut self, step_index: u32) {
        let due: Vec<Perturbation> = self
            .schedule
            .iter()
            .filter(|scheduled| scheduled.at_step == step_index)
            .map(|scheduled| scheduled.perturbation.clone())
            .collect();
        for perturbation in due {
            self.apply_perturbation(&perturbation);
        }
    }

    fn apply_perturbation(&mut self, perturbation: &Perturbation) {
        match perturbation {
            Perturbation::RecycleIdentity { element } => {
                if let Some(target) = self.element_mut(element) {
                    target.generation = target.generation.saturating_add(1);
                }
            }
            Perturbation::ChangeRole { element, role } => {
                if let Some(target) = self.element_mut(element) {
                    target.role = role.clone();
                }
            }
            Perturbation::Disable { element } => {
                if let Some(target) = self.element_mut(element) {
                    target.enabled = false;
                }
            }
            Perturbation::Unadvertise { element } => {
                if let Some(target) = self.element_mut(element) {
                    target.advertises = false;
                }
            }
            Perturbation::SetSensitivity {
                element,
                sensitivity,
            } => {
                if let Some(target) = self.element_mut(element) {
                    target.sensitivity = *sensitivity;
                }
            }
            Perturbation::Remove { element } => {
                self.elements.retain(|candidate| &candidate.id != element);
            }
            Perturbation::Redraw { element } => {
                let next = self.rng.next_u64();
                if let Some(target) = self.element_mut(element) {
                    target.region_seed = next;
                }
                // A redraw is the one perturbation that moves the frame
                // digest; see the note on `frame_digest_seed`.
                self.frame_digest_seed = self.frame_digest_seed.wrapping_add(1);
            }
            Perturbation::BackendRefuses => self.backend_refuses_next = true,
            Perturbation::PostconditionMisses => self.postcondition_misses_next = true,
            Perturbation::LatencySpike { millis } => self.pending_latency_millis = *millis,
            Perturbation::OperatorTakeover => self.takeover_requested = true,
        }
    }

    fn element_mut(&mut self, id: &str) -> Option<&mut SyntheticElement> {
        self.elements.iter_mut().find(|element| element.id == id)
    }

    #[must_use]
    pub fn element(&self, id: &str) -> Option<&SyntheticElement> {
        self.elements.iter().find(|element| element.id == id)
    }

    #[must_use]
    pub fn elements(&self) -> &[SyntheticElement] {
        &self.elements
    }

    /// Take a fresh observation. Every observation advances the sequence, so
    /// a step decided two observations ago is always stale.
    pub fn observe(&mut self, epoch: u64) -> FrameToken {
        self.sequence = self.sequence.saturating_add(1);
        self.epoch = epoch;
        FrameToken {
            frame_id: "synthetic-frame".into(),
            sequence: self.sequence,
            epoch,
            captured_at_millis: self.clock_millis,
            digest: digest_str(
                domain::FRAME,
                &format!("{}:{}", self.frame_digest_seed, self.sequence),
            ),
        }
    }

    /// The frame as it stands now, without taking a new observation.
    ///
    /// This is what the executor compares a plan against: the same sequence
    /// the plan was made on, but the digest the world has now. A redraw or a
    /// completed mutation in between shows up as a digest mismatch, which is
    /// the stale-frame refusal.
    #[must_use]
    pub fn current_frame(&self, epoch: u64, captured_at_millis: u64) -> FrameToken {
        FrameToken {
            frame_id: "synthetic-frame".into(),
            sequence: self.sequence,
            epoch,
            captured_at_millis,
            digest: digest_str(
                domain::FRAME,
                &format!("{}:{}", self.frame_digest_seed, self.sequence),
            ),
        }
    }

    /// Dispatch a step against the world.
    ///
    /// The world only ever refuses for reasons a backend can have. Policy
    /// refusals happen before dispatch, in [`crate::executor::evaluate`]; if
    /// one reached here it would mean the contract let something through.
    pub fn dispatch(&mut self, intent: &StepIntent) -> Result<PostconditionOutcome, DenyReason> {
        if std::mem::take(&mut self.backend_refuses_next) {
            return Err(DenyReason::BackendUnavailable);
        }
        let outcome = match intent {
            StepIntent::Observe | StepIntent::Wait { .. } | StepIntent::ActivateTarget => {
                PostconditionOutcome::NotApplicable
            }
            StepIntent::Complete => PostconditionOutcome::Met,
            _ => {
                if std::mem::take(&mut self.postcondition_misses_next) {
                    // Reported success, nothing moved. This is the failure a
                    // profile that verifies postconditions exists to catch and
                    // a profile that trusts the reported outcome cannot see.
                    PostconditionOutcome::Missed
                } else {
                    // A mutation moves the frame, which is what a postcondition
                    // of `FrameChanged` is checking for.
                    self.frame_digest_seed = self.frame_digest_seed.wrapping_add(1);
                    PostconditionOutcome::Met
                }
            }
        };
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> SyntheticWorld {
        SyntheticWorld::new("test-world", 4, 0)
    }

    #[test]
    fn observations_always_advance_the_sequence() {
        let mut world = world();
        let first = world.observe(0);
        let second = world.observe(0);
        assert!(second.sequence > first.sequence);
        assert_ne!(first.digest, second.digest);
    }

    #[test]
    fn a_redraw_moves_the_frame_digest_and_a_disable_does_not() {
        let mut world = world();
        let observed = world.observe(0);
        world.schedule(
            1,
            Perturbation::Disable {
                element: "element-1".into(),
            },
        );
        world.advance_to(1);
        // The element table moved; the rendered surface did not.
        assert_eq!(
            observed.digest,
            world.current_frame(0, observed.captured_at_millis).digest
        );
        assert!(!world.element("element-1").unwrap().enabled);

        world.schedule(
            2,
            Perturbation::Redraw {
                element: "element-1".into(),
            },
        );
        world.advance_to(2);
        assert_ne!(
            observed.digest,
            world.current_frame(0, observed.captured_at_millis).digest
        );
    }

    #[test]
    fn a_dispatched_mutation_makes_the_decided_frame_stale() {
        let mut world = world();
        let observed = world.observe(0);
        world
            .dispatch(&StepIntent::Invoke {
                element: crate::schema::ElementRef::new("element-0", 1).unwrap(),
            })
            .unwrap();
        assert_ne!(
            observed.digest,
            world.current_frame(0, observed.captured_at_millis).digest
        );
    }

    #[test]
    fn perturbations_fire_at_their_scheduled_step_and_only_there() {
        let mut world = world();
        world.schedule(
            5,
            Perturbation::RecycleIdentity {
                element: "element-0".into(),
            },
        );
        for step in 0..5 {
            world.advance_to(step);
            assert_eq!(world.element("element-0").unwrap().generation, 1);
        }
        world.advance_to(5);
        assert_eq!(world.element("element-0").unwrap().generation, 2);
        world.advance_to(6);
        assert_eq!(world.element("element-0").unwrap().generation, 2);
    }

    #[test]
    fn a_backend_refusal_is_consumed_once() {
        let mut world = world();
        world.schedule(0, Perturbation::BackendRefuses);
        world.advance_to(0);
        assert_eq!(
            world.dispatch(&StepIntent::ActivateTarget).unwrap_err(),
            DenyReason::BackendUnavailable
        );
        assert!(world.dispatch(&StepIntent::ActivateTarget).is_ok());
    }

    #[test]
    fn a_silent_failure_reports_a_missed_postcondition_once() {
        let mut world = world();
        world.schedule(0, Perturbation::PostconditionMisses);
        world.advance_to(0);
        let element = crate::schema::ElementRef::new("element-0", 1).unwrap();
        let before = world.current_frame(0, 0).digest;
        assert_eq!(
            world
                .dispatch(&StepIntent::Invoke {
                    element: element.clone()
                })
                .unwrap(),
            PostconditionOutcome::Missed
        );
        // Nothing moved, which is exactly why the postcondition missed.
        assert_eq!(world.current_frame(0, 0).digest, before);
        assert_eq!(
            world.dispatch(&StepIntent::Invoke { element }).unwrap(),
            PostconditionOutcome::Met
        );
    }

    #[test]
    fn removing_an_element_makes_it_unfindable() {
        let mut world = world();
        world.schedule(
            0,
            Perturbation::Remove {
                element: "element-2".into(),
            },
        );
        world.advance_to(0);
        assert!(world.element("element-2").is_none());
    }

    #[test]
    fn latency_and_takeover_signals_are_consumed_on_read() {
        let mut world = world();
        world.schedule(0, Perturbation::LatencySpike { millis: 9_000 });
        world.schedule(0, Perturbation::OperatorTakeover);
        world.advance_to(0);
        assert_eq!(world.take_pending_latency(), 9_000);
        assert_eq!(world.take_pending_latency(), 0);
        assert!(world.take_takeover_request());
        assert!(!world.take_takeover_request());
    }

    #[test]
    fn the_world_is_reproducible_from_its_label() {
        let a = SyntheticWorld::new("same-label", 6, 0);
        let b = SyntheticWorld::new("same-label", 6, 0);
        assert_eq!(a.elements(), b.elements());
        let c = SyntheticWorld::new("other-label", 6, 0);
        assert_ne!(a.elements(), c.elements());
    }
}
