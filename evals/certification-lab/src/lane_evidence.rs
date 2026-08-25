//! Fixture-owned lane cardinality and per-home send-state expectations.
//!
//! The always-on oracle previously carried the setup lane, the manager-plan
//! Work row and the per-home provider budget as constants in Rust. A constant
//! is not evidence: it can be edited to match whatever a candidate produces.
//! This module moves those values into a versioned, digest-pinned fixture
//! parsed with the same exhaustive exact-key discipline as the campaign
//! fixture, so the runtime oracle is owned by a document a reviewer can read.
//!
//! It is deliberately separate from the shared
//! `grokptah.always_on_grokbot_fixture.v1` document, which the shipped service
//! tests also parse with `deny_unknown`: adding lab-only keys there would force
//! edits inside the crate that carries the protected soak.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use serde::{Deserialize, Serialize};

use crate::always_on::{
    deny_unknown, expect_object, parse_duplicate_free, take_object, take_string, take_string_array,
    take_u64, AlwaysOnHappyShape, AlwaysOnLaneEvidence, AlwaysOnSnapshot, LoopbackProviderLane,
    ManagerDecisionBinding, MANAGER_DECISION_KIND, MANAGER_DECISION_STEP_ID, MANAGER_PLAN_KIND,
    MANAGER_PROPOSAL_PURPOSE, SETUP_SEMANTIC_ID,
};
use crate::report::{opaque_durable_id, DiagnosticCode, LoopbackProviderRecord, SendState};

/// Public home identifiers this fixture describes.
pub const HOME_A: &str = "home_a";
/// Public home identifiers this fixture describes.
pub const HOME_B: &str = "home_b";

/// Cardinality the bootstrap submit contributes to a fresh home.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SetupLane {
    pub work: usize,
    pub runs: usize,
    pub intents: usize,
    pub provider_posts: u64,
}

/// Cardinality the manager plan's own Work row contributes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanLane {
    pub work: usize,
    pub runs: usize,
    pub intents: usize,
    pub work_kind: String,
}

/// Public discriminators for the manager-decision lane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagerDecisionSpec {
    pub step_id: String,
    pub work_kind: String,
    pub run_purpose: String,
    pub terminal_work_state: String,
}

/// The exact public states one home's lane must reach.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChainExpectation {
    pub work_state: String,
    pub run_state: String,
    pub run_purpose: String,
}

/// What one isolated home must observe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HomeExpectation {
    /// Terminal send state per declared semantic id, including the semantic
    /// ids that must never have left the process at all.
    pub send_states: BTreeMap<String, SendState>,
    /// Step ids whose full causal chain this home must publish, with the
    /// exact public states that chain must reach in this home. The same step
    /// legitimately ends `succeeded` in the uninterrupted home and `failed` in
    /// the held one, so the states are owned per home, never globally.
    pub chains: BTreeMap<String, ChainExpectation>,
    /// Process restarts this home performs.
    pub restarts: u64,
}

impl HomeExpectation {
    /// Semantic ids this home must have accepted at the provider, in order.
    pub fn accepted_semantics(&self) -> Vec<&str> {
        self.send_states
            .iter()
            .filter(|(_, state)| !state.may_send_once())
            .map(|(semantic, _)| semantic.as_str())
            .collect()
    }

    /// Semantic ids that must show zero send attempts, so the sender still
    /// holds its one-shot budget for them.
    pub fn known_not_sent(&self) -> Vec<&str> {
        self.send_states
            .iter()
            .filter(|(_, state)| state.may_send_once())
            .map(|(semantic, _)| semantic.as_str())
            .collect()
    }
}

/// Restart trace events each restart must publish.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RestartTraceEvents {
    pub disconnect_per_restart: u64,
    pub restart_per_restart: u64,
    pub reconnect_per_restart: u64,
}

impl RestartTraceEvents {
    /// Total restart trace events across `restarts` restarts.
    pub fn total(self, restarts: u64) -> Result<u64, DiagnosticCode> {
        self.disconnect_per_restart
            .checked_add(self.restart_per_restart)
            .and_then(|sum| sum.checked_add(self.reconnect_per_restart))
            .and_then(|per| per.checked_mul(restarts))
            .ok_or(DiagnosticCode::FixtureInvalid)
    }
}

/// Bounds on the evidence one home may publish, so a report stays reviewable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoundedEvidence {
    pub max_send_attempts_per_home: usize,
    pub max_chains_per_home: usize,
}

/// The lab-owned lane fixture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaneEvidenceFixture {
    pub schema: String,
    pub schema_version: u64,
    pub bound_fixture_schema: String,
    pub bound_fixture_version: u64,
    pub claim: String,
    pub provider_network: String,
    /// How a provider row is attributed to a durable lane. The public contract
    /// puts no run, intent or attempt correlation id on the wire, so lane
    /// selection inside one home is semantic. That boundary is declared here
    /// rather than papered over, and the at-send carriers below are what does
    /// bind provider rows to durable identities.
    pub lane_selection: String,
    /// Nonsecret values the loopback observes in the request at send time and
    /// the report cross-checks against public projections.
    pub at_send_carriers: Vec<String>,
    /// Exactly which at-send carriers each semantic id must present. The
    /// manager-decision prompt carries no Agent identity, so that send is
    /// attributable to its home but not to its manager; declaring this per
    /// semantic id means a carrier that goes missing anywhere else fails
    /// closed instead of being excused.
    pub carrier_requirements: BTreeMap<String, BTreeSet<String>>,
    pub setup_lane: SetupLane,
    pub plan_lane: PlanLane,
    pub manager_decision: ManagerDecisionSpec,
    pub homes: BTreeMap<String, HomeExpectation>,
    pub restart_trace_events: RestartTraceEvents,
    pub bounded_evidence: BoundedEvidence,
}

impl LaneEvidenceFixture {
    /// Parse the fixture bundled with the lab.
    pub fn load() -> Result<Self, DiagnosticCode> {
        Self::parse(crate::ALWAYS_ON_LANE_FIXTURE).map_err(|_| DiagnosticCode::FixtureInvalid)
    }

    /// Parse `bytes` with the strict validator, reporting the first violation.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        Self::from_value(parse_duplicate_free(bytes)?)
    }

    /// Back-compat entry point for an already-parsed document. Duplicate keys
    /// are gone by then, so prefer [`LaneEvidenceFixture::parse`].
    pub fn from_json(value: &Value) -> Result<Self, String> {
        Self::from_value(value.clone())
    }

    fn from_value(value: Value) -> Result<Self, String> {
        let mut root = expect_object(value, "lane fixture")?;
        let schema = take_string(&mut root, "schema")?;
        if schema != crate::ALWAYS_ON_LANE_FIXTURE_SCHEMA {
            return Err(format!(
                "schema {schema} != {}",
                crate::ALWAYS_ON_LANE_FIXTURE_SCHEMA
            ));
        }
        let schema_version = take_u64(&mut root, "schemaVersion")?;
        if schema_version != 1 {
            return Err(format!("schemaVersion {schema_version} != 1"));
        }
        let bound_fixture_schema = take_string(&mut root, "boundFixtureSchema")?;
        let bound_fixture_version = take_u64(&mut root, "boundFixtureVersion")?;
        let claim = take_string(&mut root, "claim")?;
        let provider_network = take_string(&mut root, "providerNetwork")?;
        let lane_selection = take_string(&mut root, "laneSelection")?;
        let at_send_carriers = take_string_array(&mut root, "atSendCarriers")?;
        let carrier_requirements = take_carrier_requirements(&mut root)?;

        let mut setup = take_object(&mut root, "setupLane")?;
        let setup_lane = SetupLane {
            work: widen(take_u64(&mut setup, "work")?)?,
            runs: widen(take_u64(&mut setup, "runs")?)?,
            intents: widen(take_u64(&mut setup, "intents")?)?,
            provider_posts: take_u64(&mut setup, "providerPosts")?,
        };
        let setup_run_purpose = take_string(&mut setup, "runPurpose")?;
        deny_unknown(setup, "setupLane")?;

        let mut plan = take_object(&mut root, "planLane")?;
        let plan_lane = PlanLane {
            work: widen(take_u64(&mut plan, "work")?)?,
            runs: widen(take_u64(&mut plan, "runs")?)?,
            intents: widen(take_u64(&mut plan, "intents")?)?,
            work_kind: take_string(&mut plan, "workKind")?,
        };
        deny_unknown(plan, "planLane")?;

        let mut decision = take_object(&mut root, "managerDecision")?;
        let manager_decision = ManagerDecisionSpec {
            step_id: take_string(&mut decision, "stepId")?,
            work_kind: take_string(&mut decision, "workKind")?,
            run_purpose: take_string(&mut decision, "runPurpose")?,
            terminal_work_state: take_string(&mut decision, "terminalWorkState")?,
        };
        deny_unknown(decision, "managerDecision")?;

        let homes = take_homes(&mut root)?;

        let mut events = take_object(&mut root, "restartTraceEvents")?;
        let restart_trace_events = RestartTraceEvents {
            disconnect_per_restart: take_u64(&mut events, "disconnectPerRestart")?,
            restart_per_restart: take_u64(&mut events, "restartPerRestart")?,
            reconnect_per_restart: take_u64(&mut events, "reconnectPerRestart")?,
        };
        deny_unknown(events, "restartTraceEvents")?;

        let mut bounds = take_object(&mut root, "boundedEvidence")?;
        let bounded_evidence = BoundedEvidence {
            max_send_attempts_per_home: widen(take_u64(&mut bounds, "maxSendAttemptsPerHome")?)?,
            max_chains_per_home: widen(take_u64(&mut bounds, "maxChainsPerHome")?)?,
        };
        deny_unknown(bounds, "boundedEvidence")?;
        deny_unknown(root, "lane fixture")?;

        let fixture = Self {
            schema,
            schema_version,
            bound_fixture_schema,
            bound_fixture_version,
            claim,
            provider_network,
            lane_selection,
            at_send_carriers,
            carrier_requirements,
            setup_lane,
            plan_lane,
            manager_decision,
            homes,
            restart_trace_events,
            bounded_evidence,
        };
        fixture.validate(&setup_run_purpose)?;
        Ok(fixture)
    }

    fn validate(&self, setup_run_purpose: &str) -> Result<(), String> {
        if self.bound_fixture_schema != crate::ALWAYS_ON_GROKBOT_FIXTURE_SCHEMA
            || self.bound_fixture_version != 2
        {
            return Err("lane fixture is not bound to the campaign fixture it describes".into());
        }
        if self.lane_selection != "semantic-no-at-send-lane-carrier" {
            return Err("laneSelection must state the observed lane-carrier boundary".into());
        }
        if self.at_send_carriers != vec!["manager".to_owned(), "home".to_owned()] {
            return Err("atSendCarriers must declare exactly the manager and home carriers".into());
        }
        let known: BTreeSet<&str> = self.at_send_carriers.iter().map(String::as_str).collect();
        for (semantic, carriers) in &self.carrier_requirements {
            if carriers.is_empty() {
                return Err(format!("carrierRequirements.{semantic} must not be empty"));
            }
            if carriers.iter().any(|name| !known.contains(name.as_str())) {
                return Err(format!(
                    "carrierRequirements.{semantic} names an undeclared carrier"
                ));
            }
            if !carriers.contains("home") {
                return Err(format!(
                    "carrierRequirements.{semantic} must require the home carrier"
                ));
            }
        }
        if self.plan_lane.work_kind != MANAGER_PLAN_KIND {
            return Err(format!("planLane.workKind must be {MANAGER_PLAN_KIND}"));
        }
        if self.manager_decision.step_id != MANAGER_DECISION_STEP_ID
            || self.manager_decision.work_kind != MANAGER_DECISION_KIND
            || self.manager_decision.run_purpose != MANAGER_PROPOSAL_PURPOSE
        {
            return Err("managerDecision does not describe the public decision lane".into());
        }
        if setup_run_purpose.is_empty() {
            return Err("setupLane.runPurpose must be a non-empty public purpose".into());
        }
        if self.setup_lane.runs == 0 || self.setup_lane.provider_posts == 0 {
            return Err("setupLane must observe at least one Run and one POST".into());
        }
        if self.plan_lane.work == 0 {
            return Err("planLane must contribute the plan's own Work row".into());
        }
        if self.restart_trace_events.disconnect_per_restart == 0
            || self.restart_trace_events.restart_per_restart == 0
            || self.restart_trace_events.reconnect_per_restart == 0
        {
            return Err("every restart must publish disconnect, restart and reconnect".into());
        }
        if self.bounded_evidence.max_send_attempts_per_home == 0
            || self.bounded_evidence.max_chains_per_home == 0
        {
            return Err("boundedEvidence limits must be greater than zero".into());
        }
        let declared: BTreeSet<&str> = self.homes.keys().map(String::as_str).collect();
        let required: BTreeSet<&str> = [HOME_A, HOME_B].into_iter().collect();
        if declared != required {
            return Err("homes must declare exactly home_a and home_b".into());
        }
        for (name, home) in &self.homes {
            if home.chains.is_empty() {
                return Err(format!("homes.{name}.chains must not be empty"));
            }
            if home.chains.len() > self.bounded_evidence.max_chains_per_home {
                return Err(format!("homes.{name}.chains exceeds its own bound"));
            }
            if home.send_states.len() > self.bounded_evidence.max_send_attempts_per_home {
                return Err(format!("homes.{name}.sendStates exceeds its own bound"));
            }
            if home.send_states.get(SETUP_SEMANTIC_ID) != Some(&SendState::Sent) {
                return Err(format!("homes.{name} must record a delivered setup POST"));
            }
            for (semantic, state) in &home.send_states {
                if !state.may_send_once() && !self.carrier_requirements.contains_key(semantic) {
                    return Err(format!(
                        "carrierRequirements is missing {semantic}, which homes.{name} sends"
                    ));
                }
            }
            // A chain is only provable where bytes actually reached the
            // provider, so every published chain needs a non-KnownNotSent send.
            for chain in home.chains.keys() {
                let semantic = semantic_for_step(chain);
                match home.send_states.get(semantic) {
                    Some(state) if !state.may_send_once() => {}
                    Some(_) => {
                        return Err(format!(
                            "homes.{name}.chains includes {chain} whose send state is KnownNotSent"
                        ))
                    }
                    None => {
                        return Err(format!(
                            "homes.{name}.sendStates is missing {semantic} for a published chain"
                        ))
                    }
                }
            }
        }
        Ok(())
    }

    /// The home expectation for `home`.
    pub fn home(&self, home: &str) -> Result<&HomeExpectation, DiagnosticCode> {
        self.homes.get(home).ok_or(DiagnosticCode::FixtureInvalid)
    }

    /// The exact states `step_id` must reach in `home`.
    pub fn chain(&self, home: &str, step_id: &str) -> Result<&ChainExpectation, DiagnosticCode> {
        self.home(home)?
            .chains
            .get(step_id)
            .ok_or(DiagnosticCode::FixtureInvalid)
    }

    /// Terminal public Work state `step_id` must reach on the uninterrupted
    /// happy path, which the home-A lane owns.
    pub fn happy_terminal_state(&self, step_id: &str) -> Result<&str, DiagnosticCode> {
        Ok(self.chain(HOME_A, step_id)?.work_state.as_str())
    }
}

fn widen(value: u64) -> Result<usize, String> {
    usize::try_from(value).map_err(|_| "value does not fit the host usize".to_owned())
}

fn take_homes(root: &mut Map<String, Value>) -> Result<BTreeMap<String, HomeExpectation>, String> {
    let object = take_object(root, "homes")?;
    let mut out = BTreeMap::new();
    for (name, value) in object {
        let mut row = expect_object(value, &format!("homes.{name}"))?;
        let states = take_object(&mut row, "sendStates")?;
        let mut send_states = BTreeMap::new();
        for (semantic, state) in states {
            if semantic.is_empty() {
                return Err(format!("homes.{name}.sendStates has an empty semantic id"));
            }
            let text = state
                .as_str()
                .ok_or_else(|| format!("homes.{name}.sendStates.{semantic} must be a string"))?;
            let parsed = parse_send_state(text).ok_or_else(|| {
                format!("homes.{name}.sendStates.{semantic} has unknown state {text}")
            })?;
            if send_states.insert(semantic.clone(), parsed).is_some() {
                return Err(format!("duplicate homes.{name}.sendStates.{semantic}"));
            }
        }
        if send_states.is_empty() {
            return Err(format!("homes.{name}.sendStates must not be empty"));
        }
        let chain_rows = take_object(&mut row, "chains")?;
        let mut chains = BTreeMap::new();
        for (step, value) in chain_rows {
            if step.is_empty() {
                return Err(format!("homes.{name}.chains has an empty step id"));
            }
            let mut chain = expect_object(value, &format!("homes.{name}.chains.{step}"))?;
            let expectation = ChainExpectation {
                work_state: take_string(&mut chain, "workState")?,
                run_state: take_string(&mut chain, "runState")?,
                run_purpose: take_string(&mut chain, "runPurpose")?,
            };
            deny_unknown(chain, &format!("homes.{name}.chains.{step}"))?;
            if chains.insert(step.clone(), expectation).is_some() {
                return Err(format!("duplicate homes.{name}.chains.{step}"));
            }
        }
        let restarts = take_u64(&mut row, "restarts")?;
        deny_unknown(row, &format!("homes.{name}"))?;
        if out
            .insert(
                name.clone(),
                HomeExpectation {
                    send_states,
                    chains,
                    restarts,
                },
            )
            .is_some()
        {
            return Err(format!("duplicate homes.{name}"));
        }
    }
    Ok(out)
}

/// Build a coherent summary for `lanes`, for tests that need a passing
/// candidate to mutate.
#[cfg(test)]
pub(crate) fn sample_summary(lanes: &LaneEvidenceFixture) -> AlwaysOnCertificationSummary {
    let homes = lanes
        .homes
        .iter()
        .map(|(name, expectation)| {
            let mut ordinal = 0_u64;
            let send_ledger: Vec<SendAttemptEvidence> = expectation
                .send_states
                .iter()
                .map(|(semantic, state)| {
                    if state.may_send_once() {
                        SendAttemptEvidence::known_not_sent(semantic)
                    } else {
                        ordinal += 1;
                        SendAttemptEvidence {
                            ordinal,
                            semantic_id: semantic.clone(),
                            body_digest: format!("digest-{name}-{semantic}"),
                            send_state: *state,
                            accepted: true,
                            route_ok: true,
                            carrier_manager: lanes
                                .carrier_requirements
                                .get(semantic)
                                .is_some_and(|required| required.contains("manager"))
                                .then(|| opaque_durable_id(&format!("{name}-manager"))),
                            carrier_home: Some(opaque_durable_id(&format!("{name}-home"))),
                            work: None,
                            attempt: None,
                            intent: None,
                            run: None,
                        }
                    }
                })
                .collect();
            let chains: Vec<CausalChainEvidence> = expectation
                .chains
                .iter()
                .map(|(step, expected)| {
                    let semantic = semantic_for_step(step);
                    let send = send_ledger
                        .iter()
                        .find(|entry| entry.semantic_id == semantic)
                        .expect("declared send")
                        .clone();
                    let intent = opaque_durable_id(&format!("{name}-{step}-intent"));
                    CausalChainEvidence {
                        step_id: step.clone(),
                        semantic_id: semantic.to_owned(),
                        send,
                        work: opaque_durable_id(&format!("{name}-{step}-work")),
                        attempt: opaque_durable_id(&format!("{name}-{step}-attempt")),
                        intent: intent.clone(),
                        run: opaque_durable_id(&format!("{name}-{step}-run")),
                        request: intent,
                        work_revision: 1,
                        agent_spec_revision: 2,
                        input: opaque_durable_id(&format!("{name}-{step}-input")),
                        work_state: expected.work_state.clone(),
                        run_state: expected.run_state.clone(),
                        run_purpose: expected.run_purpose.clone(),
                        link_projection_agrees: true,
                    }
                })
                .collect();
            let send_ledger = bind_ledger_identities(send_ledger, &chains);
            let chains: Vec<CausalChainEvidence> = chains
                .into_iter()
                .map(|mut chain| {
                    chain.send = send_ledger
                        .iter()
                        .find(|entry| entry.semantic_id == chain.semantic_id)
                        .expect("bound send")
                        .clone();
                    chain
                })
                .collect();
            let decision = chains
                .iter()
                .find(|chain| chain.step_id == lanes.manager_decision.step_id)
                .expect("decision chain")
                .clone();
            HomeEvidence {
                home: name.clone(),
                manager: opaque_durable_id(&format!("{name}-manager")),
                policy: "policy-digest".into(),
                plan_config: "plan-config-digest".into(),
                restarts: expectation.restarts,
                restart_trace_events: lanes
                    .restart_trace_events
                    .total(expectation.restarts)
                    .expect("restart events"),
                send_ledger,
                chains,
                decision,
            }
        })
        .collect();
    AlwaysOnCertificationSummary::new(homes)
}

/// Build the identity shape that agrees with `summary`, for tests that need a
/// coherent passing candidate.
#[cfg(test)]
pub(crate) fn sample_shape(summary: &AlwaysOnCertificationSummary) -> AlwaysOnHappyShape {
    let home_a = summary.home(HOME_A).expect("home a");
    let lane = |chain: &CausalChainEvidence| AlwaysOnLaneEvidence {
        step_id: chain.step_id.clone(),
        work: chain.work.clone(),
        attempt: chain.attempt.clone(),
        intent: chain.intent.clone(),
        run: chain.run.clone(),
    };
    let decision_lane = lane(&home_a.decision);
    AlwaysOnHappyShape {
        native_lanes: home_a
            .chains
            .iter()
            .filter(|chain| chain.step_id != MANAGER_DECISION_STEP_ID)
            .map(lane)
            .collect(),
        decision_lane: decision_lane.clone(),
        manager_decision_binding: ManagerDecisionBinding::Bound {
            lane: decision_lane,
        },
    }
}

/// Parse the public wire spelling of a send state.
pub fn parse_send_state(text: &str) -> Option<SendState> {
    match text {
        "known_not_sent" => Some(SendState::KnownNotSent),
        "sending" => Some(SendState::Sending),
        "uncertain" => Some(SendState::Uncertain),
        "sent" => Some(SendState::Sent),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn canonical() -> Value {
        parse_duplicate_free(crate::ALWAYS_ON_LANE_FIXTURE).expect("lane fixture")
    }

    #[test]
    fn canonical_lane_fixture_parses_and_owns_the_cardinality() {
        let fixture = LaneEvidenceFixture::load().expect("lane fixture");
        assert_eq!(fixture.setup_lane.work, 0);
        assert_eq!(fixture.setup_lane.runs, 1);
        assert_eq!(fixture.setup_lane.intents, 0);
        assert_eq!(fixture.setup_lane.provider_posts, 1);
        assert_eq!(fixture.plan_lane.work, 1);
        assert_eq!(fixture.plan_lane.runs, 0);
        assert_eq!(fixture.plan_lane.intents, 0);
        // The happy path's terminal states are owned by the home-A lane.
        assert_eq!(fixture.happy_terminal_state("step-a"), Ok("succeeded"));
        assert_eq!(fixture.happy_terminal_state("step-b"), Ok("failed"));
        assert_eq!(fixture.happy_terminal_state("step-b-fix"), Ok("succeeded"));
        assert_eq!(
            fixture.happy_terminal_state(MANAGER_DECISION_STEP_ID),
            Ok("succeeded")
        );
        assert_eq!(
            fixture.happy_terminal_state("step-unknown"),
            Err(DiagnosticCode::FixtureInvalid)
        );
        // The same step legitimately ends differently in the held home.
        assert_eq!(
            fixture.chain(HOME_B, "step-a").unwrap().work_state,
            "failed"
        );
        assert_eq!(
            fixture.chain(HOME_B, "step-a").unwrap().run_state,
            "interrupted"
        );
        let home_a = fixture.home(HOME_A).unwrap();
        assert_eq!(home_a.restarts, 0);
        assert!(home_a.known_not_sent().is_empty());
        let home_b = fixture.home(HOME_B).unwrap();
        assert_eq!(home_b.restarts, 2);
        assert_eq!(home_b.known_not_sent(), vec!["step-b", "step-b-fix"]);
        assert_eq!(home_b.chains.len(), 2);
        assert_eq!(home_a.chains.len(), 4);
        assert_eq!(
            home_b.send_states.get("step-a"),
            Some(&SendState::Uncertain),
            "the held step is accepted with its response withheld"
        );
        assert_eq!(fixture.restart_trace_events.total(2).unwrap(), 6);
    }

    // -- adversarial evidence mutations --------------------------------------

    fn summary() -> AlwaysOnCertificationSummary {
        sample_summary(&LaneEvidenceFixture::load().expect("lane fixture"))
    }

    fn home_mut<'a>(
        summary: &'a mut AlwaysOnCertificationSummary,
        home: &str,
    ) -> &'a mut HomeEvidence {
        summary
            .homes
            .iter_mut()
            .find(|entry| entry.home == home)
            .expect("home")
    }

    #[test]
    fn canonical_summary_is_coherent() {
        let lanes = LaneEvidenceFixture::load().unwrap();
        assert_eq!(assert_certification_summary(&lanes, &summary()), Ok(()));
        // Both fixture digests are pinned into the published summary.
        let published = summary();
        assert_eq!(
            published.campaign_fixture,
            digest_bytes(crate::ALWAYS_ON_GROKBOT_FIXTURE)
        );
        assert_eq!(
            published.lane_fixture,
            digest_bytes(crate::ALWAYS_ON_LANE_FIXTURE)
        );
        // Home B publishes six restart trace events for its two restarts.
        assert_eq!(published.home(HOME_B).unwrap().restart_trace_events, 6);
        assert_eq!(published.home(HOME_A).unwrap().restart_trace_events, 0);
        // Published evidence must survive the report's redaction scan.
        let value = serde_json::to_value(&published).unwrap();
        grokptah_agent_bridge::scan_value_for_forbidden_data(&value)
            .expect("published summary must be redaction-safe");
    }

    #[test]
    fn summary_rejects_missing_duplicate_and_swapped_evidence() {
        let lanes = LaneEvidenceFixture::load().unwrap();
        let fail = |mutant: &AlwaysOnCertificationSummary, label: &str| {
            assert!(
                assert_certification_summary(&lanes, mutant).is_err(),
                "{label} must fail"
            );
        };

        // missing: a home, a chain, a ledger entry.
        let mut missing_home = summary();
        missing_home.homes.retain(|home| home.home != HOME_B);
        fail(&missing_home, "missing home");
        let mut missing_chain = summary();
        home_mut(&mut missing_chain, HOME_A).chains.pop();
        fail(&missing_chain, "missing chain");
        let mut missing_send = summary();
        home_mut(&mut missing_send, HOME_A).send_ledger.pop();
        fail(&missing_send, "missing ledger entry");

        // duplicate: a home, a chain, a ledger entry.
        let mut duplicate_home = summary();
        let clone = duplicate_home.home(HOME_A).unwrap().clone();
        duplicate_home.homes.push(clone);
        fail(&duplicate_home, "duplicate home");
        let mut duplicate_chain = summary();
        let chain = home_mut(&mut duplicate_chain, HOME_A).chains[0].clone();
        home_mut(&mut duplicate_chain, HOME_A).chains.push(chain);
        fail(&duplicate_chain, "duplicate chain");
        let mut duplicate_send = summary();
        let entry = home_mut(&mut duplicate_send, HOME_A).send_ledger[0].clone();
        home_mut(&mut duplicate_send, HOME_A)
            .send_ledger
            .push(entry);
        fail(&duplicate_send, "duplicate ledger entry");

        // swapped: two lanes wearing each other's step ids.
        let mut swapped = summary();
        {
            let home = home_mut(&mut swapped, HOME_A);
            let first = home.chains[0].step_id.clone();
            let second = home.chains[1].step_id.clone();
            home.chains[0].step_id = second;
            home.chains[1].step_id = first;
        }
        fail(&swapped, "swapped chain step ids");

        // swapped: the two homes wearing each other's labels.
        let mut swapped_homes = summary();
        {
            let a = swapped_homes.home(HOME_A).unwrap().clone();
            let b = swapped_homes.home(HOME_B).unwrap().clone();
            home_mut(&mut swapped_homes, HOME_A).chains = b.chains;
            home_mut(&mut swapped_homes, HOME_B).chains = a.chains;
        }
        fail(&swapped_homes, "swapped home evidence");
    }

    #[test]
    fn summary_rejects_stale_cross_home_cross_run_and_cross_attempt_evidence() {
        let lanes = LaneEvidenceFixture::load().unwrap();
        let fail = |mutant: &AlwaysOnCertificationSummary, label: &str| {
            assert!(
                assert_certification_summary(&lanes, mutant).is_err(),
                "{label} must fail"
            );
        };

        // stale: a chain citing a send the ledger no longer carries.
        let mut stale = summary();
        home_mut(&mut stale, HOME_A).chains[0].send.ordinal += 100;
        fail(&stale, "stale send ordinal");
        let mut stale_digest = summary();
        home_mut(&mut stale_digest, HOME_A).chains[0]
            .send
            .body_digest = "digest-from-an-earlier-run".into();
        fail(&stale_digest, "stale send digest");

        // cross-home: one home publishing another home's durable identity.
        let mut cross_home = summary();
        {
            let borrowed = cross_home.home(HOME_A).unwrap().chains[0].run.clone();
            home_mut(&mut cross_home, HOME_B).chains[0].run = borrowed;
        }
        fail(&cross_home, "cross-home Run identity");
        let mut shared_manager = summary();
        {
            let manager = shared_manager.home(HOME_A).unwrap().manager.clone();
            home_mut(&mut shared_manager, HOME_B).manager = manager;
        }
        fail(&shared_manager, "shared manager identity");

        // cross-run and cross-attempt inside one home.
        let mut cross_run = summary();
        {
            let home = home_mut(&mut cross_run, HOME_A);
            home.chains[0].run = home.chains[1].run.clone();
        }
        fail(&cross_run, "cross-run identity");
        let mut cross_attempt = summary();
        {
            let home = home_mut(&mut cross_attempt, HOME_A);
            home.chains[0].attempt = home.chains[1].attempt.clone();
        }
        fail(&cross_attempt, "cross-attempt identity");

        // The request identity must be the accepted intent, not another one.
        let mut mismatched_request = summary();
        home_mut(&mut mismatched_request, HOME_A).chains[0].request =
            opaque_durable_id("some-other-request");
        fail(&mismatched_request, "request that is not the intent");
    }

    #[test]
    fn summary_rejects_wrong_policy_commit_and_send_state_evidence() {
        let lanes = LaneEvidenceFixture::load().unwrap();
        let fail = |mutant: &AlwaysOnCertificationSummary, label: &str| {
            assert!(
                assert_certification_summary(&lanes, mutant).is_err(),
                "{label} must fail"
            );
        };

        // wrong policy or plan configuration between the two homes.
        let mut wrong_policy = summary();
        home_mut(&mut wrong_policy, HOME_B).policy = "another-policy".into();
        fail(&wrong_policy, "divergent policy");
        let mut wrong_plan = summary();
        home_mut(&mut wrong_plan, HOME_B).plan_config = "another-plan".into();
        fail(&wrong_plan, "divergent plan configuration");
        let mut empty_policy = summary();
        home_mut(&mut empty_policy, HOME_A).policy = String::new();
        home_mut(&mut empty_policy, HOME_B).policy = String::new();
        fail(&empty_policy, "absent policy binding");

        // wrong commit: the fixture digests are part of the published claim.
        let mut wrong_fixture = summary();
        wrong_fixture.lane_fixture = digest_bytes(b"a different lane fixture");
        fail(&wrong_fixture, "wrong lane fixture digest");
        let mut wrong_campaign = summary();
        wrong_campaign.campaign_fixture = digest_bytes(b"a different campaign fixture");
        fail(&wrong_campaign, "wrong campaign fixture digest");

        // send states must match the fixture exactly.
        let mut wrong_state = summary();
        {
            let home = home_mut(&mut wrong_state, HOME_B);
            let entry = home
                .send_ledger
                .iter_mut()
                .find(|entry| entry.semantic_id == "step-a")
                .expect("held step");
            entry.send_state = SendState::Sent;
        }
        fail(&wrong_state, "held step reported as fully delivered");

        // A KnownNotSent entry may not carry an attempt.
        let mut fabricated = summary();
        {
            let home = home_mut(&mut fabricated, HOME_B);
            let entry = home
                .send_ledger
                .iter_mut()
                .find(|entry| entry.send_state.may_send_once())
                .expect("known-not-sent entry");
            entry.ordinal = 9;
            entry.accepted = true;
        }
        fail(&fabricated, "fabricated attempt for a never-sent semantic");

        // Restart trace events may not be quietly dropped.
        let mut fewer_events = summary();
        home_mut(&mut fewer_events, HOME_B).restart_trace_events = 5;
        fail(&fewer_events, "missing restart trace event");
        let mut fewer_restarts = summary();
        home_mut(&mut fewer_restarts, HOME_B).restarts = 1;
        fail(&fewer_restarts, "under-reported restarts");

        // The link cross-check may not be waived.
        let mut waived = summary();
        home_mut(&mut waived, HOME_A).chains[0].link_projection_agrees = false;
        fail(&waived, "waived link cross-check");

        // The decision chain must be a published chain of the decision lane.
        let mut foreign_decision = summary();
        {
            let chain = foreign_decision
                .home(HOME_A)
                .unwrap()
                .chains
                .iter()
                .find(|chain| chain.step_id != MANAGER_DECISION_STEP_ID)
                .expect("a native chain")
                .clone();
            home_mut(&mut foreign_decision, HOME_A).decision = chain;
        }
        fail(&foreign_decision, "decision that is not the decision lane");
        let mut wrong_purpose = summary();
        home_mut(&mut wrong_purpose, HOME_A).decision.run_purpose = "execution".into();
        fail(&wrong_purpose, "decision Run without the proposal purpose");

        // Per-home lane states are owned by the fixture.
        let held_index = |summary: &AlwaysOnCertificationSummary| {
            summary
                .home(HOME_B)
                .unwrap()
                .chains
                .iter()
                .position(|chain| chain.step_id == "step-a")
                .expect("held chain")
        };
        let mut wrong_work_state = summary();
        let index = held_index(&wrong_work_state);
        home_mut(&mut wrong_work_state, HOME_B).chains[index].work_state = "succeeded".into();
        fail(&wrong_work_state, "held Work reported as succeeded");
        let mut wrong_run_state = summary();
        let index = held_index(&wrong_run_state);
        home_mut(&mut wrong_run_state, HOME_B).chains[index].run_state = "completed".into();
        fail(&wrong_run_state, "interrupted Run reported as completed");
    }

    #[test]
    fn carriers_must_be_present_and_agree_with_the_published_identities() {
        let lanes = LaneEvidenceFixture::load().unwrap();
        let fail = |mutant: &AlwaysOnCertificationSummary, label: &str| {
            assert!(
                assert_certification_summary(&lanes, mutant).is_err(),
                "{label} must fail"
            );
        };
        // The fixture declares exactly which at-send carriers exist, and states
        // that no lane-granular carrier does.
        assert_eq!(lanes.at_send_carriers, vec!["manager", "home"]);
        assert_eq!(lanes.lane_selection, "semantic-no-at-send-lane-carrier");

        // Absence fails closed on either carrier.
        for drop_manager in [true, false] {
            let mut absent = summary();
            {
                let home = home_mut(&mut absent, HOME_A);
                let entry = home
                    .send_ledger
                    .iter_mut()
                    .find(|entry| {
                        !entry.send_state.may_send_once() && entry.carrier_manager.is_some()
                    })
                    .expect("accepted send with both carriers");
                if drop_manager {
                    entry.carrier_manager = None;
                } else {
                    entry.carrier_home = None;
                }
            }
            let chains_fixed = fix_chain_sends(&mut absent);
            assert!(chains_fixed);
            fail(
                &absent,
                if drop_manager {
                    "absent manager carrier"
                } else {
                    "absent home carrier"
                },
            );
        }

        // An unexpected carrier is as much a mismatch as a missing one: the
        // decision send carries no Agent identity, so claiming one is false.
        let mut extra_carrier = summary();
        {
            let home = home_mut(&mut extra_carrier, HOME_A);
            let manager = home.manager.clone();
            let entry = home
                .send_ledger
                .iter_mut()
                .find(|entry| entry.semantic_id == MANAGER_DECISION_KIND)
                .expect("decision send");
            entry.carrier_manager = Some(manager);
        }
        fix_chain_sends(&mut extra_carrier);
        fail(&extra_carrier, "decision send claiming a manager carrier");

        // A carrier naming another manager is a mismatch.
        let mut wrong_manager = summary();
        {
            let borrowed = wrong_manager.home(HOME_B).unwrap().manager.clone();
            let home = home_mut(&mut wrong_manager, HOME_A);
            for entry in home.send_ledger.iter_mut() {
                if entry.carrier_manager.is_some() {
                    entry.carrier_manager = Some(borrowed.clone());
                }
            }
        }
        fix_chain_sends(&mut wrong_manager);
        fail(&wrong_manager, "carrier naming the other home's manager");

        // A single provider row carrying the other home's home carrier is the
        // cross-home permutation this carrier exists to catch.
        let mut cross_home_carrier = summary();
        {
            let borrowed = cross_home_carrier
                .home(HOME_B)
                .unwrap()
                .send_ledger
                .iter()
                .find_map(|entry| entry.carrier_home.clone())
                .expect("home carrier");
            let home = home_mut(&mut cross_home_carrier, HOME_A);
            let entry = home
                .send_ledger
                .iter_mut()
                .find(|entry| entry.carrier_home.is_some())
                .expect("accepted send");
            entry.carrier_home = Some(borrowed);
        }
        fix_chain_sends(&mut cross_home_carrier);
        fail(&cross_home_carrier, "row carrying the other home's carrier");

        // Both homes reporting the same home carrier means one home wearing
        // two labels.
        let mut shared_home = summary();
        {
            let borrowed = shared_home
                .home(HOME_A)
                .unwrap()
                .send_ledger
                .iter()
                .find_map(|entry| entry.carrier_home.clone())
                .expect("home carrier");
            let home = home_mut(&mut shared_home, HOME_B);
            for entry in home.send_ledger.iter_mut() {
                if entry.carrier_home.is_some() {
                    entry.carrier_home = Some(borrowed.clone());
                }
            }
        }
        fix_chain_sends(&mut shared_home);
        fail(&shared_home, "both homes sharing one home carrier");

        // A never-sent semantic id may not claim a carrier.
        let mut fabricated_carrier = summary();
        {
            let home = home_mut(&mut fabricated_carrier, HOME_B);
            let entry = home
                .send_ledger
                .iter_mut()
                .find(|entry| entry.send_state.may_send_once())
                .expect("known-not-sent");
            entry.carrier_home = Some(opaque_durable_id("fabricated"));
        }
        fail(&fabricated_carrier, "carrier on a never-sent semantic");
    }

    #[test]
    fn setup_send_must_stay_unbound_and_every_other_send_must_be_bound() {
        let lanes = LaneEvidenceFixture::load().unwrap();
        let fail = |mutant: &AlwaysOnCertificationSummary, label: &str| {
            assert!(
                assert_certification_summary(&lanes, mutant).is_err(),
                "{label} must fail"
            );
        };
        // The bootstrap submit materialises no Work, attempt or intent, so its
        // send must resolve to no durable identity.
        let canonical = summary();
        let setup = canonical
            .home(HOME_A)
            .unwrap()
            .send_ledger
            .iter()
            .find(|entry| entry.semantic_id == SETUP_SEMANTIC_ID)
            .expect("setup entry");
        assert!(setup.is_unbound(), "setup must publish no lane identity");
        for entry in &canonical.home(HOME_A).unwrap().send_ledger {
            if entry.semantic_id != SETUP_SEMANTIC_ID && !entry.send_state.may_send_once() {
                assert!(entry.is_bound(), "{} must be bound", entry.semantic_id);
            }
        }

        // setup Some: the bootstrap send claiming a lane identity.
        let mut setup_bound = summary();
        {
            let home = home_mut(&mut setup_bound, HOME_A);
            let borrowed = home.chains[0].run.clone();
            let entry = home
                .send_ledger
                .iter_mut()
                .find(|entry| entry.semantic_id == SETUP_SEMANTIC_ID)
                .expect("setup entry");
            entry.work = Some(borrowed.clone());
            entry.attempt = Some(borrowed.clone());
            entry.intent = Some(borrowed.clone());
            entry.run = Some(borrowed);
        }
        fail(&setup_bound, "setup send claiming a durable lane");

        // nonsetup None: a lane send that resolved to nothing.
        for field in 0..4 {
            let mut unbound = summary();
            {
                let home = home_mut(&mut unbound, HOME_A);
                let entry = home
                    .send_ledger
                    .iter_mut()
                    .find(|entry| {
                        entry.semantic_id != SETUP_SEMANTIC_ID && !entry.send_state.may_send_once()
                    })
                    .expect("lane send");
                match field {
                    0 => entry.work = None,
                    1 => entry.attempt = None,
                    2 => entry.intent = None,
                    _ => entry.run = None,
                }
            }
            fix_chain_sends(&mut unbound);
            fail(&unbound, "lane send missing a durable identity");
        }

        // shape-to-join: a ledger entry bound to identities its chain does not
        // claim.
        let mut divergent = summary();
        {
            let home = home_mut(&mut divergent, HOME_A);
            let foreign = home.chains[1].run.clone();
            home.chains[0].run = foreign;
        }
        fail(&divergent, "chain identities diverging from the ledger");
    }

    /// Re-point every chain's embedded send at its ledger entry, so a mutation
    /// applied to the ledger is not masked by a stale copy inside the chain.
    fn fix_chain_sends(summary: &mut AlwaysOnCertificationSummary) -> bool {
        for home in summary.homes.iter_mut() {
            let ledger = home.send_ledger.clone();
            for chain in home.chains.iter_mut() {
                if let Some(entry) = ledger
                    .iter()
                    .find(|entry| entry.semantic_id == chain.semantic_id)
                {
                    chain.send = entry.clone();
                }
            }
            let decision_semantic = home.decision.semantic_id.clone();
            if let Some(entry) = ledger
                .iter()
                .find(|entry| entry.semantic_id == decision_semantic)
            {
                home.decision.send = entry.clone();
            }
        }
        true
    }

    fn shape_from(summary: &AlwaysOnCertificationSummary) -> AlwaysOnHappyShape {
        sample_shape(summary)
    }

    #[test]
    fn shape_must_name_the_same_identities_as_the_provider_joins() {
        let published = summary();
        let shape = shape_from(&published);
        assert_eq!(assert_shape_matches_joins(&shape, &published), Ok(()));

        // shape-to-join: a native lane pointing at a different Run.
        let mut swapped_run = shape.clone();
        swapped_run.native_lanes[0].run = opaque_durable_id("some-other-run");
        assert!(assert_shape_matches_joins(&swapped_run, &published).is_err());
        for field in 0..3 {
            let mut mutant = shape.clone();
            let target = opaque_durable_id("substituted");
            match field {
                0 => mutant.native_lanes[0].work = target,
                1 => mutant.native_lanes[0].attempt = target,
                _ => mutant.native_lanes[0].intent = target,
            }
            assert!(
                assert_shape_matches_joins(&mutant, &published).is_err(),
                "substituted field {field} must fail"
            );
        }
        // A lane naming a step the joins never published.
        let mut foreign_step = shape.clone();
        foreign_step.native_lanes[0].step_id = "step-unknown".into();
        assert!(assert_shape_matches_joins(&foreign_step, &published).is_err());

        // Two native lanes permuted while counts and uniqueness are preserved.
        let mut permuted = shape.clone();
        {
            let first = permuted.native_lanes[0].clone();
            let second = permuted.native_lanes[1].clone();
            permuted.native_lanes[0] = AlwaysOnLaneEvidence {
                step_id: first.step_id.clone(),
                ..second.clone()
            };
            permuted.native_lanes[1] = AlwaysOnLaneEvidence {
                step_id: second.step_id,
                ..first
            };
        }
        assert_eq!(permuted.native_lanes.len(), shape.native_lanes.len());
        assert!(
            assert_shape_matches_joins(&permuted, &published).is_err(),
            "a count-preserving two-way permutation must fail"
        );

        // Bound-lane tampering: the payload no longer matches the lane.
        let mut tampered = shape.clone();
        tampered.manager_decision_binding = ManagerDecisionBinding::Bound {
            lane: tampered.native_lanes[0].clone(),
        };
        assert!(assert_shape_matches_joins(&tampered, &published).is_err());
        let mut unbound = shape.clone();
        unbound.manager_decision_binding = ManagerDecisionBinding::PurposeNotProjected {
            work: opaque_durable_id("work-d"),
        };
        assert!(assert_shape_matches_joins(&unbound, &published).is_err());
        let mut decision_swap = shape;
        decision_swap.decision_lane.run = opaque_durable_id("another-run");
        assert!(assert_shape_matches_joins(&decision_swap, &published).is_err());

        // Cross-home: the shape naming Home B's identities.
        let home_b = published.home(HOME_B).unwrap();
        let mut cross = shape_from(&published);
        cross.decision_lane = AlwaysOnLaneEvidence {
            step_id: home_b.decision.step_id.clone(),
            work: home_b.decision.work.clone(),
            attempt: home_b.decision.attempt.clone(),
            intent: home_b.decision.intent.clone(),
            run: home_b.decision.run.clone(),
        };
        cross.manager_decision_binding = ManagerDecisionBinding::Bound {
            lane: cross.decision_lane.clone(),
        };
        assert!(
            assert_shape_matches_joins(&cross, &published).is_err(),
            "a shape naming the other home's decision lane must fail"
        );
    }

    #[test]
    fn summary_must_be_stamped_with_this_campaign_contract_and_commit() {
        let mut published = summary();
        published.manifest = "manifest-digest".into();
        published.commit = "d78aeef".into();
        published.dirty = false;
        assert_eq!(
            assert_summary_binding(&published, "manifest-digest", "d78aeef", false),
            Ok(())
        );
        // wrong commit, wrong contract, wrong tree state.
        assert!(assert_summary_binding(&published, "manifest-digest", "0000000", false).is_err());
        assert!(assert_summary_binding(&published, "another-manifest", "d78aeef", false).is_err());
        assert!(assert_summary_binding(&published, "manifest-digest", "d78aeef", true).is_err());
        // An unstamped summary is not this campaign's evidence.
        let unstamped = summary();
        assert!(assert_summary_binding(&unstamped, "manifest-digest", "d78aeef", false).is_err());
    }

    #[test]
    fn published_evidence_rejects_extra_fields() {
        let published = serde_json::to_value(summary()).unwrap();
        for pointer in [
            "/homes/0",
            "/homes/0/chains/0",
            "/homes/0/chains/0/send",
            "/homes/0/send_ledger/0",
        ] {
            let mut mutant = published.clone();
            mutant
                .pointer_mut(pointer)
                .expect("section")
                .as_object_mut()
                .expect("object")
                .insert("smuggled".into(), serde_json::Value::Bool(true));
            assert!(
                serde_json::from_value::<AlwaysOnCertificationSummary>(mutant).is_err(),
                "{pointer} must reject an extra field"
            );
        }
        let mut root = published;
        root.as_object_mut()
            .unwrap()
            .insert("smuggled".into(), serde_json::Value::Bool(true));
        assert!(serde_json::from_value::<AlwaysOnCertificationSummary>(root).is_err());
    }

    #[test]
    fn send_ledger_and_no_resume_fences_reject_re_drives() {
        let lanes = LaneEvidenceFixture::load().unwrap();
        let home = lanes.home(HOME_B).unwrap();
        let uncertain = crate::always_on::LoopbackProviderLane {
            home: crate::always_on::AlwaysOnHome::HomeB,
            accepted_posts: 1,
            rejected_auth: 0,
            records: vec![LoopbackProviderRecord {
                ordinal: 1,
                method: "POST".into(),
                path: "/v1/chat/completions".into(),
                semantic_id: "step-a".into(),
                body_digest: "digest-held".into(),
                auth_accepted: true,
                route_ok: true,
                send_state: SendState::Uncertain,
                carrier_manager: Some(opaque_durable_id("home_b-manager")),
                carrier_home: Some(opaque_durable_id("home_b-home")),
            }],
        };
        let before = observed_ledger(&uncertain);
        assert_eq!(before.len(), 1);
        // The same single attempt across the cut is the passing case.
        assert_eq!(
            assert_no_resume_after_cut(home, &before, &uncertain),
            Ok(())
        );
        // A second attempt for an uncertain send is a double commit.
        let mut re_driven = uncertain.clone();
        let mut second = re_driven.records[0].clone();
        second.ordinal = 2;
        re_driven.records.push(second);
        re_driven.accepted_posts = 2;
        assert_eq!(
            assert_no_resume_after_cut(home, &before, &re_driven),
            Err(DiagnosticCode::RestartRecoveryFailed)
        );
        // A semantic id the fixture says never runs must stay at zero.
        let mut leaked = uncertain.clone();
        let mut extra = leaked.records[0].clone();
        extra.ordinal = 2;
        extra.semantic_id = "step-b".into();
        leaked.records.push(extra);
        leaked.accepted_posts = 2;
        assert_eq!(
            assert_no_resume_after_cut(home, &before, &leaked),
            Err(DiagnosticCode::RestartRecoveryFailed)
        );
        // An attempt that reappears under a different ordinal is a re-drive.
        let mut renumbered = uncertain.clone();
        renumbered.records[0].ordinal = 7;
        assert_eq!(
            assert_no_resume_after_cut(home, &before, &renumbered),
            Err(DiagnosticCode::RestartRecoveryFailed)
        );
        // A ledger over the fixture's own bound is refused.
        let mut oversized = uncertain;
        oversized.records = (0..64)
            .map(|index| {
                let mut record = oversized.records[0].clone();
                record.ordinal = index + 1;
                record
            })
            .collect();
        assert_eq!(
            assert_home_send_ledger(home, &oversized, lanes.bounded_evidence),
            Err(DiagnosticCode::BoundExceeded)
        );
    }

    #[test]
    fn lane_fixture_rejects_unknown_dropped_and_duplicate_keys() {
        for pointer in [
            vec![],
            vec!["setupLane"],
            vec!["planLane"],
            vec!["managerDecision"],
            vec!["restartTraceEvents"],
            vec!["boundedEvidence"],
            vec!["homes", "home_a"],
            vec!["homes", "home_a", "chains", "step-a"],
        ] {
            let mut mutant = canonical();
            let mut cursor = &mut mutant;
            for key in &pointer {
                cursor = cursor.get_mut(key).expect("section");
            }
            cursor["unexpectedKey"] = json!("smuggled");
            assert!(
                LaneEvidenceFixture::from_json(&mutant)
                    .expect_err("unknown key")
                    .contains("unknown keys"),
                "{pointer:?}"
            );
        }
        let mut dropped = canonical();
        dropped.as_object_mut().unwrap().remove("claim");
        assert!(LaneEvidenceFixture::from_json(&dropped).is_err());
        let text = String::from_utf8(crate::ALWAYS_ON_LANE_FIXTURE.to_vec()).unwrap();
        let duplicated = text.replacen("\"work\": 0,", "\"work\": 0,\n    \"work\": 9,", 1);
        assert!(LaneEvidenceFixture::parse(duplicated.as_bytes())
            .expect_err("duplicate key")
            .contains("duplicate object key"));
    }

    #[test]
    fn lane_fixture_rejects_incoherent_declarations() {
        // A home set that is not exactly the two isolated homes.
        let mut one_home = canonical();
        one_home["homes"].as_object_mut().unwrap().remove(HOME_B);
        assert!(LaneEvidenceFixture::from_json(&one_home).is_err());
        // A published chain whose send state says nothing ever left.
        let mut unsendable = canonical();
        unsendable["homes"][HOME_A]["sendStates"]["step-a"] = json!("known_not_sent");
        assert!(LaneEvidenceFixture::from_json(&unsendable)
            .expect_err("chain without a send")
            .contains("KnownNotSent"));
        // A chain with no send state at all.
        let mut missing_state = canonical();
        missing_state["homes"][HOME_B]["sendStates"]
            .as_object_mut()
            .unwrap()
            .remove("manager-decision");
        assert!(LaneEvidenceFixture::from_json(&missing_state).is_err());
        // Duplicate chains would double-count evidence.
        // A chain row missing one of its exact public states.
        let mut partial_chain = canonical();
        partial_chain["homes"][HOME_A]["chains"]["step-a"]
            .as_object_mut()
            .unwrap()
            .remove("runPurpose");
        assert!(LaneEvidenceFixture::from_json(&partial_chain).is_err());
        // Unknown send-state spellings must not be silently accepted.
        let mut unknown_state = canonical();
        unknown_state["homes"][HOME_A]["sendStates"]["step-a"] = json!("probably");
        assert!(LaneEvidenceFixture::from_json(&unknown_state).is_err());
        // Zero restart trace events would erase the restart evidence.
        for key in [
            "disconnectPerRestart",
            "restartPerRestart",
            "reconnectPerRestart",
        ] {
            let mut zeroed = canonical();
            zeroed["restartTraceEvents"][key] = json!(0);
            assert!(LaneEvidenceFixture::from_json(&zeroed).is_err(), "{key}");
        }
        // The lane fixture must stay bound to the campaign fixture.
        let mut unbound = canonical();
        unbound["boundFixtureVersion"] = json!(3);
        assert!(LaneEvidenceFixture::from_json(&unbound).is_err());
        // Evidence bounds must actually bound something.
        let mut unbounded = canonical();
        unbounded["boundedEvidence"]["maxChainsPerHome"] = json!(0);
        assert!(LaneEvidenceFixture::from_json(&unbounded).is_err());
        // The plan lane must contribute its own Work row.
        let mut no_plan_work = canonical();
        no_plan_work["planLane"]["work"] = json!(0);
        assert!(LaneEvidenceFixture::from_json(&no_plan_work).is_err());
        // The decision lane discriminators must stay public and exact.
        for (key, value) in [
            ("stepId", json!("step-a")),
            ("workKind", json!("native")),
            ("runPurpose", json!("execution")),
        ] {
            let mut mutant = canonical();
            mutant["managerDecision"][key] = value;
            assert!(LaneEvidenceFixture::from_json(&mutant).is_err(), "{key}");
        }
    }
}

// ---------------------------------------------------------------------------
// Send attempts and causality
// ---------------------------------------------------------------------------

/// One durable send attempt as the loopback provider observed it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendAttemptEvidence {
    pub ordinal: u64,
    pub semantic_id: String,
    pub body_digest: String,
    pub send_state: SendState,
    pub accepted: bool,
    pub route_ok: bool,
    /// Opaque manager Agent identity the loopback observed in the request at
    /// send time. Cross-checked against the published manager.
    pub carrier_manager: Option<String>,
    /// Opaque isolated-home identity the loopback observed at send time.
    pub carrier_home: Option<String>,
    /// Durable identities this send resolved to. The bootstrap send resolves
    /// to none of them, and every other accepted send must resolve to all four.
    pub work: Option<String>,
    pub attempt: Option<String>,
    pub intent: Option<String>,
    pub run: Option<String>,
}

impl SendAttemptEvidence {
    fn observed(record: &LoopbackProviderRecord) -> Self {
        Self {
            ordinal: record.ordinal,
            semantic_id: record.semantic_id.clone(),
            body_digest: record.body_digest.clone(),
            send_state: record.send_state,
            accepted: record.auth_accepted,
            route_ok: record.route_ok,
            carrier_manager: record.carrier_manager.clone(),
            carrier_home: record.carrier_home.clone(),
            work: None,
            attempt: None,
            intent: None,
            run: None,
        }
    }

    /// Bind this send to the durable identities its chain resolved.
    fn with_identities(mut self, chain: &CausalChainEvidence) -> Self {
        self.work = Some(chain.work.clone());
        self.attempt = Some(chain.attempt.clone());
        self.intent = Some(chain.intent.clone());
        self.run = Some(chain.run.clone());
        self
    }

    /// True when every durable identity field is absent.
    pub fn is_unbound(&self) -> bool {
        self.work.is_none() && self.attempt.is_none() && self.intent.is_none() && self.run.is_none()
    }

    /// True when every durable identity field is present.
    pub fn is_bound(&self) -> bool {
        self.work.is_some() && self.attempt.is_some() && self.intent.is_some() && self.run.is_some()
    }

    /// The ledger entry for a semantic id that never left the process.
    fn known_not_sent(semantic_id: &str) -> Self {
        Self {
            ordinal: 0,
            semantic_id: semantic_id.to_owned(),
            body_digest: String::new(),
            send_state: SendState::KnownNotSent,
            accepted: false,
            route_ok: false,
            carrier_manager: None,
            carrier_home: None,
            work: None,
            attempt: None,
            intent: None,
            run: None,
        }
    }
}

/// The full causal chain for one lane, anchored on the durable provider
/// observation and closed on the durable Run.
///
/// The chain is built provider -> Work -> intent -> Run. It never reads
/// `attempts[].linkedRunIds` to decide which Run belongs to the lane: that
/// projection is an independent cross-check recorded in
/// `link_projection_agrees`, so a service that mislinks a Run cannot make the
/// chain agree by rewriting the link it also publishes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CausalChainEvidence {
    pub step_id: String,
    pub semantic_id: String,
    pub send: SendAttemptEvidence,
    pub work: String,
    pub attempt: String,
    pub intent: String,
    pub run: String,
    pub request: String,
    pub work_revision: u64,
    pub agent_spec_revision: u64,
    pub input: String,
    pub work_state: String,
    pub run_state: String,
    pub run_purpose: String,
    pub link_projection_agrees: bool,
}

/// Resolve the semantic id the loopback provider records for `step_id`.
pub fn semantic_for_step(step_id: &str) -> &str {
    if step_id == MANAGER_DECISION_STEP_ID {
        MANAGER_DECISION_KIND
    } else {
        step_id
    }
}

/// Build the causal chain for `step_id` from the durable provider observation
/// through the accepted intent to the durable Run.
pub fn resolve_causal_chain(
    snapshot: &AlwaysOnSnapshot,
    lane: &LoopbackProviderLane,
    step_id: &str,
) -> Result<CausalChainEvidence, DiagnosticCode> {
    let semantic_id = semantic_for_step(step_id);
    // 1. Exactly one accepted send attempt, and it must be one the sender can
    //    no longer retry.
    let record = lane.exact_accepted_attempt(semantic_id)?;
    if !record.send_state.forbids_resume() {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    // 2. Exactly one Work projected under the step id.
    let work = snapshot.work_for_step(step_id)?;
    let work_state = work
        .state
        .clone()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    // 3. The accepted intent, found by Work identity alone. `linkedRunIds` is
    //    deliberately not consulted here.
    let matching: Vec<&crate::always_on::IntentIdentity> = snapshot
        .intents
        .iter()
        .filter(|intent| intent.work_id == work.work_id)
        .collect();
    let [intent] = matching.as_slice() else {
        return Err(DiagnosticCode::StateTransitionMismatch);
    };
    // 4. The durable Run the accepted intent names, and the request identity
    //    that ties them together.
    let run = snapshot.run(&intent.run_id)?;
    if run.request_id != intent.intent_id {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let run_state = run
        .state
        .clone()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    let run_purpose = run
        .purpose
        .clone()
        .ok_or(DiagnosticCode::McpResultMalformed)?;
    // 5. Independent cross-check of the published attempt link. Disagreement
    //    fails; agreement is recorded, never used as the source.
    let attempts: Vec<&crate::always_on::AttemptIdentity> = work
        .attempts
        .iter()
        .filter(|attempt| attempt.attempt_id == intent.attempt_id)
        .collect();
    let [attempt] = attempts.as_slice() else {
        return Err(DiagnosticCode::StateTransitionMismatch);
    };
    let link_projection_agrees =
        work.attempts.len() == 1 && attempt.linked_run_ids == vec![intent.run_id.clone()];
    if !link_projection_agrees {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    Ok(CausalChainEvidence {
        step_id: step_id.to_owned(),
        semantic_id: semantic_id.to_owned(),
        send: SendAttemptEvidence::observed(record),
        work: opaque_durable_id(&work.work_id),
        attempt: opaque_durable_id(&intent.attempt_id),
        intent: opaque_durable_id(&intent.intent_id),
        run: opaque_durable_id(&intent.run_id),
        request: opaque_durable_id(&run.request_id),
        work_revision: intent.work_revision,
        agent_spec_revision: intent.agent_spec_revision,
        input: opaque_durable_id(&intent.input_hash),
        work_state,
        run_state,
        run_purpose,
        link_projection_agrees,
    })
}

/// Assert one home's send ledger matches the fixture exactly, and return the
/// ledger it proved.
///
/// Every declared semantic id is accounted for: a `KnownNotSent` declaration
/// requires zero observed attempts, so the sender still holds its one-shot
/// budget, and every other declaration requires exactly one accepted attempt in
/// exactly the declared terminal state. A semantic id the home observed but the
/// fixture never declared is unexplained traffic and fails.
pub fn assert_home_send_ledger(
    home: &HomeExpectation,
    lane: &LoopbackProviderLane,
    bounds: BoundedEvidence,
) -> Result<Vec<SendAttemptEvidence>, DiagnosticCode> {
    if lane.records.len() > bounds.max_send_attempts_per_home {
        return Err(DiagnosticCode::BoundExceeded);
    }
    let declared: BTreeSet<&str> = home.send_states.keys().map(String::as_str).collect();
    if lane.observed_semantics().difference(&declared).count() != 0 {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let mut ledger = Vec::new();
    let mut accepted_total: u64 = 0;
    for (semantic, expected) in &home.send_states {
        let attempts = lane.attempts_for(semantic);
        if expected.may_send_once() {
            // Zero send attempts is the whole claim: nothing left the process,
            // so a later restart may still send exactly once.
            if !attempts.is_empty() {
                return Err(DiagnosticCode::StateTransitionMismatch);
            }
            ledger.push(SendAttemptEvidence::known_not_sent(semantic));
            continue;
        }
        let [record] = attempts.as_slice() else {
            return Err(DiagnosticCode::StateTransitionMismatch);
        };
        if record.send_state != *expected || !record.auth_accepted || !record.route_ok {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        accepted_total = accepted_total
            .checked_add(1)
            .ok_or(DiagnosticCode::BoundExceeded)?;
        ledger.push(SendAttemptEvidence::observed(record));
    }
    if lane.accepted_posts != accepted_total {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    ledger.sort_by(|left, right| left.semantic_id.cmp(&right.semantic_id));
    Ok(ledger)
}

/// The ledger as observed right now, one entry per send attempt the provider
/// has recorded. Used to fence a restart cut against re-drives.
pub fn observed_ledger(lane: &LoopbackProviderLane) -> Vec<SendAttemptEvidence> {
    let mut ledger: Vec<SendAttemptEvidence> = lane
        .records
        .iter()
        .map(SendAttemptEvidence::observed)
        .collect();
    ledger.sort_by(|left, right| left.ordinal.cmp(&right.ordinal));
    ledger
}

/// Assert no send attempt was resumed across a restart cut.
///
/// A semantic id whose send is observed or uncertain must never gain a second
/// attempt: the sender cannot distinguish committed from not committed, so
/// re-driving it would double-commit. A semantic id that is `KnownNotSent` must
/// still hold exactly its one-shot budget, so it must remain at zero attempts
/// for as long as the fixture says it never runs.
pub fn assert_no_resume_after_cut(
    home: &HomeExpectation,
    before: &[SendAttemptEvidence],
    after: &LoopbackProviderLane,
) -> Result<(), DiagnosticCode> {
    for entry in before {
        let attempts = after.attempts_for(&entry.semantic_id);
        if entry.send_state.forbids_resume() {
            match attempts.as_slice() {
                [record] if record.ordinal == entry.ordinal => {}
                _ => return Err(DiagnosticCode::RestartRecoveryFailed),
            }
        } else if !attempts.is_empty() {
            return Err(DiagnosticCode::RestartRecoveryFailed);
        }
    }
    for (semantic, expected) in &home.send_states {
        if expected.may_send_once() && !after.attempts_for(semantic).is_empty() {
            return Err(DiagnosticCode::RestartRecoveryFailed);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-home evidence and the public certification summary
// ---------------------------------------------------------------------------

/// Everything one isolated home proved, bounded by the lane fixture.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeEvidence {
    pub home: String,
    /// Opaque identity of the manager Agent this home materialised.
    pub manager: String,
    /// Digest of the managed-execution policy this home installed.
    pub policy: String,
    /// Digest of the manager-plan arguments this home submitted.
    pub plan_config: String,
    pub restarts: u64,
    pub restart_trace_events: u64,
    pub send_ledger: Vec<SendAttemptEvidence>,
    pub chains: Vec<CausalChainEvidence>,
    /// The proposal/decision chain, which must also appear in `chains`.
    pub decision: CausalChainEvidence,
}

/// The public certification summary: what was proved, and against exactly
/// which fixture, contract and commit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlwaysOnCertificationSummary {
    pub campaign_fixture: String,
    pub lane_fixture: String,
    /// Contract digest, stamped from the report's manifest.
    pub manifest: String,
    /// Repository commit, stamped from the report.
    pub commit: String,
    /// Whether the tree was dirty, stamped from the report.
    pub dirty: bool,
    pub homes: Vec<HomeEvidence>,
}

impl AlwaysOnCertificationSummary {
    /// Build the summary for the two isolated homes.
    pub fn new(homes: Vec<HomeEvidence>) -> Self {
        Self {
            campaign_fixture: digest_bytes(crate::ALWAYS_ON_GROKBOT_FIXTURE),
            lane_fixture: digest_bytes(crate::ALWAYS_ON_LANE_FIXTURE),
            manifest: String::new(),
            commit: String::new(),
            dirty: false,
            homes,
        }
    }

    /// The evidence published for `home`.
    pub fn home(&self, home: &str) -> Option<&HomeEvidence> {
        self.homes.iter().find(|entry| entry.home == home)
    }
}

/// Assert the summary is stamped with this campaign's contract and commit.
///
/// Per-home evidence is only this campaign's evidence if it names this
/// campaign's manifest, commit and tree state. A summary lifted from another
/// run carries another commit and is refused.
pub fn assert_summary_binding(
    summary: &AlwaysOnCertificationSummary,
    manifest: &str,
    commit: &str,
    dirty: bool,
) -> Result<(), DiagnosticCode> {
    if summary.manifest != manifest || summary.commit != commit || summary.dirty != dirty {
        return Err(DiagnosticCode::OracleMismatch);
    }
    if summary.manifest.is_empty() || summary.commit.is_empty() {
        return Err(DiagnosticCode::OracleMismatch);
    }
    Ok(())
}

/// Cross-bind the published identity shape to the per-home provider joins.
///
/// The shape and the summary are produced by different code paths from
/// different sources: the shape from the durable snapshot, the joins from the
/// loopback observation forward. Requiring them to name the same opaque
/// identities means neither can be edited alone — a tampered `Bound` payload,
/// or a shape lane swapped for another, no longer agrees with the provider
/// evidence.
pub fn assert_shape_matches_joins(
    shape: &AlwaysOnHappyShape,
    summary: &AlwaysOnCertificationSummary,
) -> Result<(), DiagnosticCode> {
    let home_a = summary.home(HOME_A).ok_or(DiagnosticCode::OracleMismatch)?;
    let same = |lane: &AlwaysOnLaneEvidence, chain: &CausalChainEvidence| {
        lane.step_id == chain.step_id
            && lane.work == chain.work
            && lane.attempt == chain.attempt
            && lane.intent == chain.intent
            && lane.run == chain.run
    };
    for lane in &shape.native_lanes {
        let Some(chain) = home_a
            .chains
            .iter()
            .find(|chain| chain.step_id == lane.step_id)
        else {
            return Err(DiagnosticCode::StateTransitionMismatch);
        };
        if !same(lane, chain) {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
    }
    if !same(&shape.decision_lane, &home_a.decision) {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    // The Bound payload must be exactly the decision lane it claims.
    match &shape.manager_decision_binding {
        ManagerDecisionBinding::Bound { lane } if lane == &shape.decision_lane => Ok(()),
        _ => Err(DiagnosticCode::StateTransitionMismatch),
    }
}

/// Sha256 of `bytes`, lowercase hex.
pub fn digest_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// Bind each ledger entry to the identities its chain resolved, leaving the
/// bootstrap send unbound.
pub fn bind_ledger_identities(
    ledger: Vec<SendAttemptEvidence>,
    chains: &[CausalChainEvidence],
) -> Vec<SendAttemptEvidence> {
    ledger
        .into_iter()
        .map(|entry| {
            match chains
                .iter()
                .find(|chain| chain.semantic_id == entry.semantic_id)
            {
                Some(chain) => entry.with_identities(chain),
                None => entry,
            }
        })
        .collect()
}

/// Assert every accepted send carried the at-send correlation the loopback can
/// observe independently, and that it agrees with the published identities.
///
/// The public contract puts no run, intent or attempt correlation id on the
/// wire — the fixture records that boundary in `laneSelection` — but it does
/// carry the manager Agent identity and the isolated home, both of which are
/// projected publicly. Requiring them means a provider row lifted from the
/// other home, or from another manager, is rejected instead of being matched
/// by its semantic label alone.
pub fn assert_home_carriers(
    fixture: &LaneEvidenceFixture,
    evidence: &HomeEvidence,
) -> Result<String, DiagnosticCode> {
    let mut home_carrier: Option<&str> = None;
    for entry in &evidence.send_ledger {
        if entry.send_state.may_send_once() {
            if entry.carrier_manager.is_some() || entry.carrier_home.is_some() {
                return Err(DiagnosticCode::StateTransitionMismatch);
            }
            continue;
        }
        let required = fixture
            .carrier_requirements
            .get(&entry.semantic_id)
            .ok_or(DiagnosticCode::FixtureInvalid)?;
        let mut present = BTreeSet::new();
        if entry.carrier_manager.is_some() {
            present.insert("manager".to_owned());
        }
        if entry.carrier_home.is_some() {
            present.insert("home".to_owned());
        }
        // Absence fails closed, and so does an unexpected carrier: the present
        // set must equal the declared set exactly.
        if &present != required {
            return Err(DiagnosticCode::ProviderObservationUnavailable);
        }
        if let Some(manager) = &entry.carrier_manager {
            if manager != &evidence.manager {
                return Err(DiagnosticCode::StateTransitionMismatch);
            }
        }
        let home = entry
            .carrier_home
            .as_deref()
            .ok_or(DiagnosticCode::ProviderObservationUnavailable)?;
        match home_carrier {
            None => home_carrier = Some(home),
            Some(seen) if seen == home => {}
            Some(_) => return Err(DiagnosticCode::StateTransitionMismatch),
        }
    }
    home_carrier
        .map(str::to_owned)
        .ok_or(DiagnosticCode::ProviderObservationUnavailable)
}

fn take_carrier_requirements(
    root: &mut Map<String, Value>,
) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    let object = take_object(root, "carrierRequirements")?;
    let mut out = BTreeMap::new();
    for (semantic, value) in object {
        if semantic.is_empty() {
            return Err("carrierRequirements has an empty semantic id".into());
        }
        let names = match value {
            Value::Array(items) => items
                .into_iter()
                .map(|item| match item {
                    Value::String(name) if !name.is_empty() => Ok(name),
                    other => Err(format!("carrierRequirements.{semantic} item {other}")),
                })
                .collect::<Result<Vec<String>, String>>()?,
            other => {
                return Err(format!(
                    "carrierRequirements.{semantic} must be an array, got {other}"
                ))
            }
        };
        let set: BTreeSet<String> = names.iter().cloned().collect();
        if set.len() != names.len() {
            return Err(format!("carrierRequirements.{semantic} must be unique"));
        }
        if out.insert(semantic.clone(), set).is_some() {
            return Err(format!("duplicate carrierRequirements.{semantic}"));
        }
    }
    Ok(out)
}

/// Assert one home's published evidence is internally coherent and matches the
/// lane fixture exactly.
pub fn assert_home_evidence(
    fixture: &LaneEvidenceFixture,
    evidence: &HomeEvidence,
) -> Result<(), DiagnosticCode> {
    let expectation = fixture.home(&evidence.home)?;
    if evidence.restarts != expectation.restarts {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    // Every restart must publish its disconnect, restart and reconnect events.
    if evidence.restart_trace_events != fixture.restart_trace_events.total(evidence.restarts)? {
        return Err(DiagnosticCode::OracleMismatch);
    }
    if evidence.chains.len() > fixture.bounded_evidence.max_chains_per_home
        || evidence.send_ledger.len() > fixture.bounded_evidence.max_send_attempts_per_home
    {
        return Err(DiagnosticCode::BoundExceeded);
    }
    // The published chains are exactly the ones the fixture declares.
    let declared: BTreeSet<&str> = expectation.chains.keys().map(String::as_str).collect();
    let published: BTreeSet<&str> = evidence
        .chains
        .iter()
        .map(|chain| chain.step_id.as_str())
        .collect();
    if declared != published || published.len() != evidence.chains.len() {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    // The ledger accounts for exactly the declared semantic ids.
    let declared_semantics: BTreeSet<&str> =
        expectation.send_states.keys().map(String::as_str).collect();
    let published_semantics: BTreeSet<&str> = evidence
        .send_ledger
        .iter()
        .map(|entry| entry.semantic_id.as_str())
        .collect();
    if declared_semantics != published_semantics
        || published_semantics.len() != evidence.send_ledger.len()
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    for entry in &evidence.send_ledger {
        if expectation.send_states.get(&entry.semantic_id) != Some(&entry.send_state) {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        if entry.send_state.may_send_once() {
            if entry.ordinal != 0
                || entry.accepted
                || !entry.body_digest.is_empty()
                || !entry.is_unbound()
            {
                return Err(DiagnosticCode::StateTransitionMismatch);
            }
        } else if entry.ordinal == 0 || !entry.accepted || !entry.route_ok {
            return Err(DiagnosticCode::StateTransitionMismatch);
        } else if entry.semantic_id == SETUP_SEMANTIC_ID {
            // The bootstrap submit materialises no Work, attempt or intent, so
            // it must resolve to no durable lane identity at all.
            if !entry.is_unbound() {
                return Err(DiagnosticCode::StateTransitionMismatch);
            }
        } else if !entry.is_bound() {
            // Every other accepted send must resolve to a complete lane.
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
    }
    assert_home_carriers(fixture, evidence)?;
    // Every chain rests on a send the ledger also carries, at the same ordinal
    // and digest, so a chain cannot cite an attempt the home never published.
    for chain in &evidence.chains {
        let Some(entry) = evidence
            .send_ledger
            .iter()
            .find(|entry| entry.semantic_id == chain.semantic_id)
        else {
            return Err(DiagnosticCode::StateTransitionMismatch);
        };
        if entry != &chain.send || !chain.link_projection_agrees {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        if entry.work.as_deref() != Some(chain.work.as_str())
            || entry.attempt.as_deref() != Some(chain.attempt.as_str())
            || entry.intent.as_deref() != Some(chain.intent.as_str())
            || entry.run.as_deref() != Some(chain.run.as_str())
        {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        if chain.work.is_empty()
            || chain.attempt.is_empty()
            || chain.intent.is_empty()
            || chain.run.is_empty()
            || chain.request != chain.intent
        {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        // The lane's public states are owned per home by the fixture.
        let Some(expected) = expectation.chains.get(&chain.step_id) else {
            return Err(DiagnosticCode::StateTransitionMismatch);
        };
        if chain.work_state != expected.work_state
            || chain.run_state != expected.run_state
            || chain.run_purpose != expected.run_purpose
        {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
    }
    // The decision chain is one of the published chains, is the decision lane,
    // and carries the manager proposal purpose.
    if !evidence.chains.contains(&evidence.decision)
        || evidence.decision.step_id != fixture.manager_decision.step_id
        || evidence.decision.semantic_id != fixture.manager_decision.work_kind
        || evidence.decision.run_purpose != fixture.manager_decision.run_purpose
        || evidence.decision.work_state != fixture.manager_decision.terminal_work_state
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    if evidence.manager.is_empty() || evidence.policy.is_empty() || evidence.plan_config.is_empty()
    {
        return Err(DiagnosticCode::OracleMismatch);
    }
    Ok(())
}

/// Assert the published summary covers both homes and binds them coherently.
///
/// The two homes are separate isolated `GROKPTAH_HOME`s running the same
/// contract, so they must agree on the policy, plan configuration, manifest and
/// commit, and must differ on the manager identity and on every durable
/// identity they published. Sharing a manager or a Run means the evidence came
/// from one home wearing two labels.
pub fn assert_certification_summary(
    fixture: &LaneEvidenceFixture,
    summary: &AlwaysOnCertificationSummary,
) -> Result<(), DiagnosticCode> {
    if summary.campaign_fixture != digest_bytes(crate::ALWAYS_ON_GROKBOT_FIXTURE)
        || summary.lane_fixture != digest_bytes(crate::ALWAYS_ON_LANE_FIXTURE)
    {
        return Err(DiagnosticCode::FixtureInvalid);
    }
    if summary.homes.len() != fixture.homes.len() {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let published: BTreeSet<&str> = summary
        .homes
        .iter()
        .map(|home| home.home.as_str())
        .collect();
    let declared: BTreeSet<&str> = fixture.homes.keys().map(String::as_str).collect();
    if published != declared || published.len() != summary.homes.len() {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    for home in &summary.homes {
        assert_home_evidence(fixture, home)?;
    }
    let home_a = summary.home(HOME_A).ok_or(DiagnosticCode::OracleMismatch)?;
    let home_b = summary.home(HOME_B).ok_or(DiagnosticCode::OracleMismatch)?;
    if home_a.policy != home_b.policy || home_a.plan_config != home_b.plan_config {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    if home_a.manager == home_b.manager {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    // Two isolated homes must never share the at-send home carrier: a provider
    // row moved between them changes the value and is rejected.
    if assert_home_carriers(fixture, home_a)? == assert_home_carriers(fixture, home_b)? {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let mut seen = BTreeSet::new();
    for home in &summary.homes {
        for chain in &home.chains {
            for identity in [&chain.work, &chain.attempt, &chain.intent, &chain.run] {
                if !seen.insert(identity.clone()) {
                    return Err(DiagnosticCode::StateTransitionMismatch);
                }
            }
        }
    }
    Ok(())
}
