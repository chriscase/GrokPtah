//! The planner/executor loop.
//!
//! One run: observe, plan, let the world move, re-derive against what is
//! actually there, resolve conservatively, and then do exactly one of commit,
//! look again, ask a human, hand upward, or refuse. Everything the loop spends
//! is debited before it is spent, everything it does is recorded, and the
//! receipt at the end is derived from those records rather than from what the
//! loop believes it did.
//!
//! ## Where the interesting failures live
//!
//! The world is perturbed *after* the plan is made and *before* the verdict is
//! taken. That gap is deliberate and is where most real Computer Use bugs
//! actually are: the window was rebound, the control was disabled, an operator
//! took over, a redraw happened. A loop that observed and dispatched in one
//! breath would never see any of it.
//!
//! ## Termination
//!
//! Every path through the loop either advances the step, consumes a bounded
//! allowance (a retry, the one free second look, an escalation), or stops the
//! run. On top of that there is a hard iteration cap, so a contract change that
//! accidentally makes some state self-perpetuating fails the suite instead of
//! hanging it. `tests/cu_adaptive_suite.rs` runs the whole matrix, including
//! 300-step horizons, and would not finish if this were not true.
//!
//! ## What the numbers mean
//!
//! Nothing here is measured. Latency is the tier's declared per-step figure
//! plus whatever the scenario scripted; cost is the tier's declared unit count.
//! Both are synthetic accounting, and the receipt says so.

use std::collections::BTreeMap;

use crate::budget::{BudgetEnvelope, BudgetLedger, BudgetLine};
use crate::cancel::{CancelCause, CleanupLedger, Resource};
use crate::confidence::Disposition;
use crate::escalation::{EscalationContext, EscalationLadder};
use crate::executor::{AdmissionRequest, StepVerdict, evaluate};
use crate::gates::ApprovalDecision;
use crate::grounding::LiveElement;
use crate::horizon::Horizon;
use crate::lease::{EpochBump, RunLease};
use crate::ledger::{LedgerEvent, RunLedger};
use crate::profile::{ProfileId, RegionPolicy};
use crate::receipt::RunReceipt;
use crate::schema::{
    ADAPTIVE_SCHEMA_VERSION, IntentFamily, PlanEnvelope, PostconditionOutcome, StepIntent,
};
use crate::tier::ModelTier;
use crate::vocabulary::{ApprovalReason, DenyReason, StopReason};

use super::agent::{PlanningContext, ReferencePlanner};
use super::scenario::{ApprovalPolicy, Scenario, WORLD_ELEMENTS};
use super::world::SyntheticWorld;

/// Synthetic bytes charged per observation. An accounting unit, not a
/// measurement of any accessibility tree.
const OBSERVATION_BYTES: u64 = 4_096;

/// The most verdicts one outcome retains. Counters in the ledger stay exact
/// past this; see [`crate::ledger`].
pub const MAX_RETAINED_VERDICTS: usize = 512;

/// One cell of the matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunConfig {
    pub scenario: Scenario,
    pub profile: ProfileId,
    pub tier: ModelTier,
}

impl RunConfig {
    #[must_use]
    pub fn label(&self) -> String {
        format!(
            "{}/{}/{}",
            self.scenario.id(),
            self.profile.slug(),
            self.tier.slug()
        )
    }

    #[must_use]
    pub fn horizon(&self) -> Horizon {
        self.scenario.horizon
    }
}

/// Everything one run produced.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub label: String,
    pub config: RunConfig,
    pub receipt: RunReceipt,
    pub ledger: RunLedger,
    pub budget: BudgetLedger,
    pub cleanup: CleanupLedger,
    pub escalation: EscalationLadder,
    /// A bounded tail of verdicts, for tests that want to look at one.
    pub verdicts: Vec<StepVerdict>,
    /// How many task steps were left behind.
    pub steps_reached: u32,
    /// Loop iterations, including retries and second looks.
    pub iterations: u32,
    /// True when the base class handed more work upward than it declared it
    /// would. The "too timid" failure.
    pub breached_escalation_ceiling: bool,
    /// What actually reached the world, by intent family. This is what a
    /// hazard gate checks: not "did it refuse" but "did anything it must not
    /// do get through".
    pub committed_by_family: BTreeMap<IntentFamily, u32>,
}

impl RunOutcome {
    /// Re-check the receipt against the parts it was derived from.
    pub fn reconciles(&self) -> Result<(), crate::receipt::ReceiptError> {
        self.receipt
            .reconcile(&self.ledger, &self.budget, &self.cleanup, &self.escalation)
    }

    /// Refusals recorded, by reason.
    #[must_use]
    pub fn denials(&self) -> &std::collections::BTreeMap<DenyReason, u32> {
        self.ledger.denials()
    }

    /// True when the run refused for this reason at least once.
    #[must_use]
    pub fn refused_for(&self, reason: DenyReason) -> bool {
        self.ledger.denials().contains_key(&reason)
    }

    /// How many steps of one intent family actually reached the world.
    #[must_use]
    pub fn committed(&self, family: IntentFamily) -> u32 {
        self.committed_by_family.get(&family).copied().unwrap_or(0)
    }
}

/// Per-step state, reset when the loop moves on.
#[derive(Debug, Default)]
struct StepState {
    retries: u32,
    disambiguated: bool,
    /// Set when this step climbed the ladder for a reason that will not
    /// outlive it. The ladder drops back when the loop moves on, so a one-off
    /// need does not buy strong-model prices for the rest of the run.
    climbed_transiently: bool,
    /// Set when this step climbed for a standing property of the class. Wins
    /// over `climbed_transiently`: a step that was first ambiguous and then
    /// turned out to need a capability this class does not have must stay
    /// climbed, or the next step re-discovers the same gap and pays for it
    /// again.
    climbed_persistently: bool,
}

/// Move to the next task step, dropping any per-step climb.
fn advance(step_index: &mut u32, state: &mut StepState, escalation: &mut EscalationLadder) {
    if state.climbed_transiently && !state.climbed_persistently {
        escalation.settle();
    }
    *step_index += 1;
    *state = StepState::default();
}

/// Run one cell of the matrix.
#[must_use]
pub fn run(config: RunConfig) -> RunOutcome {
    let label = config.label();
    let profile = config.profile.spec();
    let horizon = config.horizon();
    let steps = horizon.steps();

    let envelope = BudgetEnvelope::for_run(&profile, config.tier, horizon)
        .scaled(config.scenario.budget_scale_bps());
    let mut budget = BudgetLedger::new(envelope);
    let mut ledger = RunLedger::new();
    let mut cleanup = CleanupLedger::new();
    cleanup.acquire(Resource::Lease);
    cleanup.acquire(Resource::EvidenceHandles);

    let mut lease = RunLease::new(
        label.clone(),
        // Generous relative to the run deadline: the budget is what bounds a
        // run, and a lease that expired first would mask every other refusal.
        1_000 + envelope.run_deadline_millis.saturating_mul(4) + 60_000,
    );
    let mut escalation = EscalationLadder::new(config.tier);
    let mut context =
        EscalationContext::new(config.scenario.family.granted_families(), lease.epoch);
    let mut world = SyntheticWorld::new(&label, WORLD_ELEMENTS, lease.epoch);
    config.scenario.script(&mut world);
    let mut planner = ReferencePlanner::new(&label, config.scenario.planner_bias());

    let mut verdicts: Vec<StepVerdict> = Vec::new();
    let mut committed_by_family: BTreeMap<IntentFamily, u32> = BTreeMap::new();
    let mut step_index = 0_u32;
    let mut iterations = 0_u32;
    let max_iterations = steps.saturating_mul(4).saturating_add(16);
    let mut state = StepState::default();
    let mut stop: Option<StopReason> = None;

    while step_index < steps && iterations < max_iterations {
        iterations += 1;

        if let Err(reason) = cleanup.check_admits() {
            ledger.record(LedgerEvent::Refused { step_index, reason });
            stop = Some(StopReason::Cancelled);
            break;
        }

        // --- observe -------------------------------------------------------
        if budget.debit(BudgetLine::Observations, 1).is_err()
            || budget
                .debit(BudgetLine::ObservationBytes, OBSERVATION_BYTES)
                .is_err()
        {
            ledger.record(LedgerEvent::Refused {
                step_index,
                reason: DenyReason::BudgetExhausted,
            });
            stop = Some(StopReason::BudgetExhausted);
            break;
        }
        ledger.record(LedgerEvent::Observed { step_index });
        let plan_frame = world.observe(lease.epoch);
        let observed = world.elements().to_vec();

        let capture = match profile.region_policy {
            RegionPolicy::Never => false,
            RegionPolicy::OnUncertainty => state.disambiguated,
            RegionPolicy::EveryStep => true,
        };
        if capture {
            if budget.debit(BudgetLine::RegionCaptures, 1).is_err() {
                ledger.record(LedgerEvent::Refused {
                    step_index,
                    reason: DenyReason::BudgetExhausted,
                });
                stop = Some(StopReason::BudgetExhausted);
                break;
            }
            ledger.record(LedgerEvent::RegionCaptured { step_index });
        }

        // --- plan ----------------------------------------------------------
        let tier_now = escalation.current();
        let declared = tier_now.declared();
        if budget.debit(BudgetLine::PlannerCalls, 1).is_err() {
            ledger.record(LedgerEvent::Refused {
                step_index,
                reason: DenyReason::BudgetExhausted,
            });
            stop = Some(StopReason::BudgetExhausted);
            break;
        }
        let proposal = planner.propose(&PlanningContext {
            step_index,
            profile: &profile,
            tier: tier_now,
            elements: &observed,
            already_disambiguated: state.disambiguated,
            is_final_step: step_index + 1 == steps,
        });
        if budget
            .debit(BudgetLine::PlannerCostUnits, u64::from(proposal.cost_units))
            .is_err()
        {
            ledger.record(LedgerEvent::Refused {
                step_index,
                reason: DenyReason::BudgetExhausted,
            });
            stop = Some(StopReason::BudgetExhausted);
            break;
        }
        ledger.record(LedgerEvent::Planned { step_index });

        // One plan per iteration, holding one step. `PlannedStep::index` is
        // the step's position *within its plan*, not its position in the task,
        // so it is zero here; the task's step number is carried by
        // `step_index` and lands in the ledger. Conflating the two would make
        // every plan after the first fail its own schema check.
        let mut planned_step = proposal.step.clone();
        planned_step.index = 0;
        let plan = PlanEnvelope {
            schema_version: ADAPTIVE_SCHEMA_VERSION,
            plan_id: format!("plan-{step_index}-{iterations}"),
            objective_digest: crate::digest::digest_str(
                crate::digest::domain::OBJECTIVE,
                &config.scenario.id(),
            ),
            frame: plan_frame.clone(),
            profile: config.profile,
            tier: tier_now,
            horizon,
            steps: vec![planned_step],
        };
        let plan_digest = plan.digest().unwrap_or_default();

        // --- the world moves between decision and dispatch -----------------
        world.advance_to(step_index);
        if world.take_takeover_request() {
            lease.bump_epoch(EpochBump::OperatorTakeover);
        }
        let extra_latency = world.take_pending_latency();
        let step_latency = proposal.latency_millis.saturating_add(extra_latency);

        // A step that would take longer than the deadline is abandoned before
        // it is dispatched. Noticing afterwards would mean the action already
        // happened, which is not what a deadline is for.
        if step_latency > envelope.step_deadline_millis {
            ledger.record(LedgerEvent::Refused {
                step_index,
                reason: DenyReason::StepDeadlineExceeded,
            });
            world.tick(step_latency);
            advance(&mut step_index, &mut state, &mut escalation);
            continue;
        }

        let live_frame = world.current_frame(lease.epoch, plan_frame.captured_at_millis);
        let live_element = live_element_for(&proposal.step, &world);

        // --- decide --------------------------------------------------------
        if budget.debit(BudgetLine::ExecutorCalls, 1).is_err()
            || budget
                .debit(
                    BudgetLine::ExecutorCostUnits,
                    u64::from(declared.executor_cost_units),
                )
                .is_err()
        {
            ledger.record(LedgerEvent::Refused {
                step_index,
                reason: DenyReason::BudgetExhausted,
            });
            stop = Some(StopReason::BudgetExhausted);
            break;
        }

        let mut planner_disposition = proposal.disposition;
        let mut verdict = evaluate(&AdmissionRequest {
            profile: &profile,
            tier: tier_now,
            plan: &plan,
            plan_digest: &plan_digest,
            step: &plan.steps[0],
            planner_disposition,
            live_frame: &live_frame,
            live_element: live_element.as_ref(),
            lease: &lease,
            cleanup: &cleanup,
            context: &context,
            approval: None,
            now_millis: world.clock_millis(),
            already_disambiguated: state.disambiguated,
        });

        // Recorded from the first evaluation, before any approval. A
        // disagreement that a human then resolves still happened, and a run
        // that only recorded the post-approval verdict would report none.
        let first_disagreement = verdict.disagreement;
        if let Some(disagreement) = first_disagreement {
            ledger.record(LedgerEvent::Disagreed {
                step_index,
                kind: disagreement.kind,
            });
        }

        // --- ask a human, at most once per plan ----------------------------
        if let Disposition::RequestApproval { reason } = verdict.resolved {
            if budget.debit(BudgetLine::ApprovalRequests, 1).is_err() {
                ledger.record(LedgerEvent::Refused {
                    step_index,
                    reason: DenyReason::BudgetExhausted,
                });
                stop = Some(StopReason::BudgetExhausted);
                break;
            }
            cleanup.acquire(Resource::ApprovalPrompt);
            ledger.record(LedgerEvent::ApprovalRequested { step_index, reason });
            let approved = answer(
                config.scenario.approval_policy(),
                proposal.step.reversibility,
            );
            ledger.record(LedgerEvent::ApprovalAnswered {
                step_index,
                approved,
            });
            cleanup.release(Resource::ApprovalPrompt);
            if !approved {
                ledger.record(LedgerEvent::Refused {
                    step_index,
                    reason: DenyReason::ApprovalDenied,
                });
                stop = Some(StopReason::HumanRejected);
                break;
            }
            // The prompt shows every request open on this step at once -- the
            // step's own gates, the request the resolved disposition raised,
            // and the planner's own request if it had one. Asking about one of
            // them and then re-asking about the next would be three prompts
            // for one decision, and a run that re-asks forever never gets
            // anywhere.
            let mut granted: Vec<ApprovalReason> = verdict.gates.clone();
            for open in [
                Some(reason),
                match planner_disposition {
                    Disposition::RequestApproval { reason } => Some(reason),
                    _ => None,
                },
            ]
            .into_iter()
            .flatten()
            {
                if !granted.contains(&open) {
                    granted.push(open);
                }
            }
            let answered = ApprovalDecision {
                plan_digest: plan_digest.clone(),
                // Plan-local index, matching the step the plan holds.
                step_index: 0,
                granted,
                approved: true,
                epoch: lease.epoch,
            };
            // The planner's own request has been answered too, so it no longer
            // holds the step back.
            if matches!(
                planner_disposition,
                Disposition::RequestApproval { reason: asked } if answered.granted.contains(&asked)
            ) {
                planner_disposition = Disposition::Commit;
            }
            verdict = evaluate(&AdmissionRequest {
                profile: &profile,
                tier: tier_now,
                plan: &plan,
                plan_digest: &plan_digest,
                step: &plan.steps[0],
                planner_disposition,
                live_frame: &live_frame,
                live_element: live_element.as_ref(),
                lease: &lease,
                cleanup: &cleanup,
                context: &context,
                approval: Some(&answered),
                now_millis: world.clock_millis(),
                already_disambiguated: state.disambiguated,
            });
        }

        // A second, different disagreement after the answer is its own
        // observation; the same one again is not.
        if let Some(disagreement) = verdict
            .disagreement
            .filter(|later| first_disagreement.map(|first| first.kind) != Some(later.kind))
        {
            ledger.record(LedgerEvent::Disagreed {
                step_index,
                kind: disagreement.kind,
            });
        }
        if verdicts.len() < MAX_RETAINED_VERDICTS {
            // Re-stamped with the task step, so a reader of the retained tail
            // sees where in the run the verdict happened rather than the
            // plan-local zero.
            let mut retained = verdict.clone();
            retained.step_index = step_index;
            verdicts.push(retained);
        }

        // --- act on the resolved disposition -------------------------------
        match verdict.resolved {
            Disposition::Commit => {
                if budget.debit(BudgetLine::CommittedActions, 1).is_err() {
                    ledger.record(LedgerEvent::Refused {
                        step_index,
                        reason: DenyReason::BudgetExhausted,
                    });
                    stop = Some(StopReason::BudgetExhausted);
                    break;
                }
                match world.dispatch(&proposal.step.intent) {
                    Ok(outcome) => {
                        ledger.record(LedgerEvent::Committed { step_index });
                        *committed_by_family
                            .entry(proposal.step.intent.family())
                            .or_default() += 1;
                        let reported = if profile.verify_postcondition {
                            outcome
                        } else {
                            // The cheap profile does not look, so it does not
                            // get to claim the postcondition held.
                            PostconditionOutcome::NotChecked
                        };
                        ledger.record(LedgerEvent::Postcondition {
                            step_index,
                            outcome: reported,
                        });
                        world.tick(step_latency);
                        if budget.advance(step_latency).is_err() {
                            ledger.record(LedgerEvent::Refused {
                                step_index,
                                reason: DenyReason::RunDeadlineExceeded,
                            });
                            stop = Some(StopReason::DeadlineExceeded);
                            break;
                        }
                        if proposal.step.intent == StepIntent::Complete {
                            stop = Some(StopReason::ObjectiveComplete);
                            break;
                        }
                        if reported == PostconditionOutcome::Missed
                            && state.retries < profile.max_retries_per_step
                            && budget.debit(BudgetLine::Retries, 1).is_ok()
                        {
                            state.retries += 1;
                            ledger.record(LedgerEvent::Retried { step_index });
                            continue;
                        }
                        advance(&mut step_index, &mut state, &mut escalation);
                    }
                    Err(reason) => {
                        ledger.record(LedgerEvent::Refused { step_index, reason });
                        world.tick(step_latency);
                        if state.retries < profile.max_retries_per_step
                            && budget.debit(BudgetLine::Retries, 1).is_ok()
                        {
                            state.retries += 1;
                            ledger.record(LedgerEvent::Retried { step_index });
                            continue;
                        }
                        advance(&mut step_index, &mut state, &mut escalation);
                    }
                }
            }
            Disposition::Disambiguate => {
                ledger.record(LedgerEvent::Disambiguated { step_index });
                world.tick(step_latency);
                if state.disambiguated {
                    // The one free look is spent and it did not help.
                    advance(&mut step_index, &mut state, &mut escalation);
                } else {
                    state.disambiguated = true;
                }
            }
            Disposition::RequestApproval { .. } => {
                // Defensive. The answer above covers every request that was
                // open on this step, and nothing else about the step changes
                // between the two evaluations, so reaching here means a
                // request appeared that the prompt did not show. An
                // unanswered request is an outstanding requirement, never
                // consent, so the step is refused rather than taken.
                ledger.record(LedgerEvent::Refused {
                    step_index,
                    reason: DenyReason::ApprovalRequired,
                });
                advance(&mut step_index, &mut state, &mut escalation);
            }
            Disposition::Escalate { reason } => {
                match escalation.climb(step_index, reason, &context, &mut budget) {
                    Ok(next) => {
                        context = next;
                        ledger.record(LedgerEvent::Escalated { step_index, reason });
                        world.tick(step_latency);
                        // The point of handing upward is to try again at the
                        // stronger tier, so the step is retried rather than
                        // skipped. A transient reason drops back down once the
                        // step is done with; a standing one does not.
                        if reason.is_persistent() {
                            state.climbed_persistently = true;
                        } else {
                            state.climbed_transiently = true;
                        }
                    }
                    Err(deny) => {
                        ledger.record(LedgerEvent::Refused {
                            step_index,
                            reason: deny,
                        });
                        stop = Some(match deny {
                            DenyReason::BudgetExhausted => StopReason::BudgetExhausted,
                            _ => StopReason::Denied,
                        });
                        break;
                    }
                }
            }
            Disposition::Refuse { reason } => {
                ledger.record(LedgerEvent::Refused { step_index, reason });
                world.tick(step_latency);
                if reason.is_run_terminal() {
                    stop = Some(terminal_stop(reason));
                    break;
                }
                if reason.is_retryable()
                    && state.retries < profile.max_retries_per_step
                    && budget.debit(BudgetLine::Retries, 1).is_ok()
                {
                    state.retries += 1;
                    ledger.record(LedgerEvent::Retried { step_index });
                    continue;
                }
                advance(&mut step_index, &mut state, &mut escalation);
            }
        }
    }

    let stop_reason = stop.unwrap_or(StopReason::HorizonExhausted);
    if stop_reason == StopReason::Cancelled || cleanup.cancellation().is_some() {
        cleanup.cancel(
            &mut lease,
            CancelCause::OperatorTakeover,
            world.clock_millis(),
        );
    } else if !stop_reason.is_orderly() && stop_reason != StopReason::HorizonExhausted {
        cleanup.cancel(&mut lease, cancel_cause(stop_reason), world.clock_millis());
    } else {
        // An orderly end still has to give everything back.
        cleanup.release(Resource::Lease);
        cleanup.release(Resource::EvidenceHandles);
        cleanup.release(Resource::ApprovalPrompt);
    }

    let breached_escalation_ceiling = escalation.breaches_declared_ceiling(ledger.attempts());
    let receipt = RunReceipt::build(
        config.scenario.id(),
        config.profile,
        config.tier,
        horizon,
        &ledger,
        &budget,
        &cleanup,
        &escalation,
        stop_reason,
    );

    RunOutcome {
        label,
        config,
        receipt,
        ledger,
        budget,
        cleanup,
        escalation,
        verdicts,
        steps_reached: step_index,
        iterations,
        breached_escalation_ceiling,
        committed_by_family,
    }
}

/// The live element a step's claim points at, if the world still has one.
fn live_element_for(
    step: &crate::schema::PlannedStep,
    world: &SyntheticWorld,
) -> Option<LiveElement> {
    let reference = step.grounding.element().or_else(|| step.intent.element())?;
    world
        .element(&reference.element_id)
        .map(super::world::SyntheticElement::live)
}

/// The scripted approver. Not a model of a person; see
/// [`crate::vocabulary::NotClaimed::HumanOperatorBehavior`].
fn answer(policy: ApprovalPolicy, reversibility: crate::confidence::Reversibility) -> bool {
    match policy {
        ApprovalPolicy::ApproveAll => true,
        ApprovalPolicy::RefuseAll => false,
        ApprovalPolicy::ApproveReversibleOnly => {
            reversibility != crate::confidence::Reversibility::Irreversible
        }
    }
}

fn terminal_stop(reason: DenyReason) -> StopReason {
    match reason {
        DenyReason::Cancelled => StopReason::Cancelled,
        DenyReason::RunDeadlineExceeded => StopReason::DeadlineExceeded,
        DenyReason::ApprovalDenied => StopReason::HumanRejected,
        // The agent is no longer driving; from the run's point of view that is
        // a cancellation whether a person asked for it or the lease lapsed.
        DenyReason::LeaseLost => StopReason::Cancelled,
        _ => StopReason::Denied,
    }
}

fn cancel_cause(stop: StopReason) -> CancelCause {
    match stop {
        StopReason::BudgetExhausted | StopReason::DeadlineExceeded => CancelCause::BudgetExhausted,
        StopReason::Cancelled => CancelCause::OperatorRequest,
        _ => CancelCause::TerminalRefusal,
    }
}

#[cfg(test)]
mod tests {
    use super::super::scenario::ScenarioFamily;
    use super::*;

    fn config(
        family: ScenarioFamily,
        profile: ProfileId,
        tier: ModelTier,
        horizon: Horizon,
    ) -> RunConfig {
        RunConfig {
            scenario: Scenario::new(family, horizon),
            profile,
            tier,
        }
    }

    #[test]
    fn a_reference_run_completes_and_reconciles() {
        let outcome = run(config(
            ScenarioFamily::Reference,
            ProfileId::Balanced,
            ModelTier::StrongHosted,
            Horizon::Short,
        ));
        outcome.reconciles().unwrap();
        assert_eq!(outcome.receipt.stop_reason, StopReason::ObjectiveComplete);
        assert!(outcome.receipt.is_orderly());
        assert!(outcome.receipt.steps_committed > 0);
    }

    #[test]
    fn runs_are_reproducible_to_the_digest() {
        for horizon in Horizon::ALL {
            let first = run(config(
                ScenarioFamily::DriftingFrame,
                ProfileId::Balanced,
                ModelTier::StrongHosted,
                *horizon,
            ));
            let second = run(config(
                ScenarioFamily::DriftingFrame,
                ProfileId::Balanced,
                ModelTier::StrongHosted,
                *horizon,
            ));
            assert_eq!(first.receipt.trace_digest, second.receipt.trace_digest);
            assert_eq!(first.receipt, second.receipt);
        }
    }

    #[test]
    fn every_run_terminates_well_inside_the_iteration_cap() {
        for family in ScenarioFamily::ALL {
            for horizon in Horizon::ALL {
                let outcome = run(config(
                    *family,
                    ProfileId::Balanced,
                    ModelTier::SmallLocal,
                    *horizon,
                ));
                let cap = horizon.steps() * 4 + 16;
                assert!(
                    outcome.iterations < cap,
                    "{family:?}/{horizon:?} hit the iteration cap"
                );
            }
        }
    }

    #[test]
    fn a_takeover_stops_the_run_and_releases_everything() {
        let outcome = run(config(
            ScenarioFamily::CancellationMidFlight,
            ProfileId::Balanced,
            ModelTier::StrongHosted,
            Horizon::Medium,
        ));
        outcome.reconciles().unwrap();
        assert!(outcome.receipt.cancellation.is_some());
        assert!(outcome.receipt.cleanup_complete);
        assert!(outcome.receipt.cleanup_residue.is_empty());
        assert!(!outcome.receipt.is_orderly());
        assert!(outcome.steps_reached < Horizon::Medium.steps());
    }

    #[test]
    fn a_squeezed_budget_stops_the_run_rather_than_overspending() {
        let outcome = run(config(
            ScenarioFamily::BudgetSqueeze,
            ProfileId::HighAssurance,
            ModelTier::StrongHosted,
            Horizon::Long,
        ));
        outcome.reconciles().unwrap();
        assert_eq!(outcome.receipt.stop_reason, StopReason::BudgetExhausted);
        assert!(outcome.receipt.budget.is_within_envelope());
        assert!(outcome.refused_for(DenyReason::BudgetExhausted));
    }

    #[test]
    fn a_refused_gate_stops_the_run_at_the_gate() {
        let outcome = run(config(
            ScenarioFamily::HumanGateRefused,
            ProfileId::Balanced,
            ModelTier::StrongHosted,
            Horizon::Medium,
        ));
        outcome.reconciles().unwrap();
        assert_eq!(outcome.receipt.stop_reason, StopReason::HumanRejected);
        assert!(outcome.refused_for(DenyReason::ApprovalDenied));
        assert_eq!(outcome.receipt.approvals_refused, 1);
        assert_eq!(outcome.receipt.steps_committed, 0);
    }

    #[test]
    fn a_pixel_blind_class_never_commits_a_pointer_step() {
        for tier in [ModelTier::SmallLocal, ModelTier::MidVision] {
            let outcome = run(config(
                ScenarioFamily::PointerTemptation,
                ProfileId::Balanced,
                tier,
                Horizon::Medium,
            ));
            outcome.reconciles().unwrap();
            assert!(
                outcome.refused_for(DenyReason::PointerWithoutVisualGrounding),
                "{tier:?} did not refuse the pointer step"
            );
            // Nothing but the closing completion step may commit.
            assert!(outcome.receipt.steps_committed <= 1);
        }
    }

    #[test]
    fn latency_spikes_are_abandoned_before_dispatch() {
        let outcome = run(config(
            ScenarioFamily::LatencySpike,
            ProfileId::Balanced,
            ModelTier::StrongHosted,
            Horizon::Medium,
        ));
        outcome.reconciles().unwrap();
        assert!(outcome.refused_for(DenyReason::StepDeadlineExceeded));
    }

    #[test]
    fn the_timid_control_breaches_its_declared_ceiling() {
        let outcome = run(config(
            ScenarioFamily::OverEscalation,
            ProfileId::Balanced,
            ModelTier::SmallLocal,
            Horizon::Medium,
        ));
        outcome.reconciles().unwrap();
        assert!(
            outcome.breached_escalation_ceiling,
            "a run that escalated everything passed the ceiling check"
        );
    }

    #[test]
    fn an_ungranted_family_is_refused_at_every_tier() {
        for tier in ModelTier::ALL {
            let outcome = run(config(
                ScenarioFamily::UngrantedFamily,
                ProfileId::Balanced,
                *tier,
                Horizon::Short,
            ));
            outcome.reconciles().unwrap();
            assert!(
                outcome.refused_for(DenyReason::ClassOutsideGrant),
                "{tier:?} did not refuse an ungranted family"
            );
        }
    }

    #[test]
    fn only_a_verifying_profile_notices_a_silent_failure() {
        let economy = run(config(
            ScenarioFamily::BackendFailure,
            ProfileId::Economy,
            ModelTier::StrongHosted,
            Horizon::Medium,
        ));
        let balanced = run(config(
            ScenarioFamily::BackendFailure,
            ProfileId::Balanced,
            ModelTier::StrongHosted,
            Horizon::Medium,
        ));
        assert_eq!(economy.receipt.postconditions_met, 0);
        assert_eq!(economy.receipt.postconditions_missed, 0);
        assert!(balanced.receipt.postconditions_missed > 0);
    }
}
