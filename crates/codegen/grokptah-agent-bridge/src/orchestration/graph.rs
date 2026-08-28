//! Dependency-graph safety, review quorum, and typed admission reasons.
//!
//! This module is deliberately additive over the durable Work ledger that
//! already exists in [`super::workload`]. It introduces **no** second Work
//! type, no second lease, no second assignment path, and no second projection:
//! every function here reads the existing [`WorkItem`] / [`WorkDependency`]
//! records and returns a decision.
//!
//! It closes three holes the ledger has today:
//!
//! 1. **Nothing rejects a dependency cycle.** `WorkItem::dependency_ready`
//!    only counts succeeded dependencies, so a cycle does not fail — it
//!    silently deadlocks, leaving every item on the ring permanently
//!    un-admittable with no operator-visible cause.
//! 2. **Review is single-reviewer.** `WorkApproval` names one `reviewer_id`,
//!    so there is no way to require agreement among several reviewers, and no
//!    way to observe that agreement has become unreachable.
//! 3. **The admission reason is free-form text.** `WorkItem::blocked_reason`
//!    is a `String` its own documentation tells callers not to rely on, so
//!    "nothing to do" is not mechanically distinguishable from "not allowed
//!    to do it".

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::types::{OrchError, OrchErrorCode};
use super::workload::{WorkItem, WorkState};

/// Maximum reviewers one quorum gate may name.
pub const MAX_QUORUM_REVIEWERS: usize = 16;

fn invalid(message: impl Into<String>) -> OrchError {
    OrchError::new(OrchErrorCode::InvalidRequest, message)
}

// ---------------------------------------------------------------------------
// Dependency graph validation
// ---------------------------------------------------------------------------

/// Validate the whole dependency graph formed by `items`.
///
/// Checks run in a fixed order so a rejection is reproducible across runs and
/// machines, and cycle detection is an iterative Kahn peel rather than a
/// recursive walk, so a deep or adversarial graph cannot exhaust the stack.
///
/// `items` is the complete set under consideration. A dependency naming a work
/// id outside that set is reported rather than silently treated as satisfied —
/// the ledger's `dependency_ready` cannot tell those apart.
pub fn validate_dependency_graph(items: &[WorkItem]) -> Result<(), OrchError> {
    let mut declared: BTreeSet<&str> = BTreeSet::new();
    for item in items {
        if !declared.insert(item.work_id.as_str()) {
            return Err(invalid(format!("duplicate work id {}", item.work_id)));
        }
    }

    for item in items {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for dependency in &item.dependencies {
            let dep_id = dependency.work_id.as_str();
            if dep_id == item.work_id {
                return Err(invalid(format!("work {} depends on itself", item.work_id)));
            }
            if !declared.contains(dep_id) {
                return Err(invalid(format!(
                    "work {} depends on unknown work {dep_id}",
                    item.work_id
                )));
            }
            if !seen.insert(dep_id) {
                return Err(invalid(format!(
                    "work {} declares duplicate dependency {dep_id}",
                    item.work_id
                )));
            }
        }
    }

    detect_cycle(items)
}

/// Iterative Kahn peel. Reports the remaining members deterministically.
fn detect_cycle(items: &[WorkItem]) -> Result<(), OrchError> {
    let mut indegree: BTreeMap<&str, usize> = items
        .iter()
        .map(|item| (item.work_id.as_str(), item.dependencies.len()))
        .collect();
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for item in items {
        for dependency in &item.dependencies {
            dependents
                .entry(dependency.work_id.as_str())
                .or_default()
                .push(item.work_id.as_str());
        }
    }

    let mut ready: VecDeque<&str> = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(id, _)| *id)
        .collect();
    let mut peeled = 0usize;
    while let Some(id) = ready.pop_front() {
        peeled += 1;
        for dependent in dependents.get(id).into_iter().flatten() {
            let degree = indegree.entry(dependent).or_insert(0);
            *degree = degree.saturating_sub(1);
            if *degree == 0 {
                ready.push_back(dependent);
            }
        }
    }

    if peeled == items.len() {
        return Ok(());
    }
    let mut remaining: Vec<&str> = indegree
        .iter()
        .filter(|(_, degree)| **degree > 0)
        .map(|(id, _)| *id)
        .collect();
    remaining.sort_unstable();
    Err(invalid(format!(
        "work dependency cycle among [{}]",
        remaining.join(", ")
    )))
}

/// Validate that adding `candidate`'s dependencies keeps the graph acyclic.
///
/// This is the incremental form used when one item is created or edited
/// against a ledger that is already known to be acyclic.
pub fn validate_new_dependencies(
    existing: &[WorkItem],
    candidate: &WorkItem,
) -> Result<(), OrchError> {
    let mut combined: Vec<WorkItem> = existing
        .iter()
        .filter(|item| item.work_id != candidate.work_id)
        .cloned()
        .collect();
    combined.push(candidate.clone());
    validate_dependency_graph(&combined)
}

// ---------------------------------------------------------------------------
// Review quorum
// ---------------------------------------------------------------------------

/// One reviewer's verdict.
///
/// A verdict is recorded independently of whether the reviewer's own work
/// succeeded: a reviewer that ran to completion and rejected has done its job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    Approve,
    Reject,
}

/// A reviewer quorum gating one work item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewQuorum {
    /// Reviewer identities whose verdicts are counted. Order is irrelevant;
    /// duplicates are rejected.
    pub reviewers: Vec<String>,
    /// Approvals required. Never zero, never more than `reviewers.len()`.
    pub required_approvals: u32,
}

impl ReviewQuorum {
    pub fn validate(&self) -> Result<(), OrchError> {
        if self.reviewers.is_empty() || self.reviewers.len() > MAX_QUORUM_REVIEWERS {
            return Err(invalid(format!(
                "a quorum names 1..={MAX_QUORUM_REVIEWERS} reviewers"
            )));
        }
        let mut seen = BTreeSet::new();
        for reviewer in &self.reviewers {
            if reviewer.trim().is_empty() || reviewer.len() > 256 || reviewer.contains('\0') {
                return Err(invalid("quorum reviewer id is invalid"));
            }
            if !seen.insert(reviewer.as_str()) {
                return Err(invalid(format!("quorum names reviewer {reviewer} twice")));
            }
        }
        if self.required_approvals == 0 {
            return Err(invalid("a quorum requires at least one approval"));
        }
        if self.required_approvals as usize > self.reviewers.len() {
            return Err(invalid(
                "a quorum cannot require more approvals than it has reviewers",
            ));
        }
        Ok(())
    }
}

/// Where a quorum stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuorumOutcome {
    /// Enough approvals have been recorded.
    Met,
    /// Not yet met, but still reachable from the undecided reviewers.
    Pending,
    /// Cannot be met however the remaining reviewers vote.
    Unreachable,
}

/// Evaluate a quorum against the verdicts recorded so far.
///
/// Verdicts from identities the gate does not name are ignored rather than
/// counted, so an unnamed party cannot approve its way past the gate.
pub fn evaluate_quorum(
    quorum: &ReviewQuorum,
    verdicts: &BTreeMap<String, ReviewVerdict>,
) -> Result<QuorumOutcome, OrchError> {
    quorum.validate()?;
    let mut approvals = 0u32;
    let mut undecided = 0u32;
    for reviewer in &quorum.reviewers {
        match verdicts.get(reviewer) {
            Some(ReviewVerdict::Approve) => approvals = approvals.saturating_add(1),
            Some(ReviewVerdict::Reject) => {}
            None => undecided = undecided.saturating_add(1),
        }
    }
    if approvals >= quorum.required_approvals {
        return Ok(QuorumOutcome::Met);
    }
    if approvals.saturating_add(undecided) < quorum.required_approvals {
        return Ok(QuorumOutcome::Unreachable);
    }
    Ok(QuorumOutcome::Pending)
}

// ---------------------------------------------------------------------------
// Typed admission reasons
// ---------------------------------------------------------------------------

/// Why a work item may not be admitted right now.
///
/// This is the mechanical counterpart to `WorkItem::blocked_reason`, whose own
/// documentation tells callers not to rely on it. `Admissible` and
/// `DependenciesPending` mean "nothing to do yet"; every other variant means
/// "not allowed to do it", which is the distinction an operator surface needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionBlock {
    /// The item may be claimed now.
    Admissible,
    /// The item is not in a claimable state.
    NotClaimable,
    /// One or more dependencies have not succeeded yet.
    DependenciesPending,
    /// A dependency reached a terminal non-success state, so this item can
    /// never become ready.
    DependencyUnsatisfiable,
    /// A declared dependency is missing from the ledger.
    DependencyMissing,
    /// A review quorum has not yet been met.
    QuorumPending,
    /// A review quorum can no longer be met.
    QuorumUnreachable,
    /// The retry budget is spent.
    AttemptsExhausted,
    /// The item's deadline has passed.
    DeadlineExceeded,
}

impl AdmissionBlock {
    /// True when the item can be claimed.
    pub fn is_admissible(self) -> bool {
        matches!(self, Self::Admissible)
    }

    /// True when waiting will not help — an operator has to intervene.
    pub fn needs_operator_attention(self) -> bool {
        matches!(
            self,
            Self::DependencyUnsatisfiable
                | Self::DependencyMissing
                | Self::QuorumUnreachable
                | Self::AttemptsExhausted
                | Self::DeadlineExceeded
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admissible => "admissible",
            Self::NotClaimable => "not_claimable",
            Self::DependenciesPending => "dependencies_pending",
            Self::DependencyUnsatisfiable => "dependency_unsatisfiable",
            Self::DependencyMissing => "dependency_missing",
            Self::QuorumPending => "quorum_pending",
            Self::QuorumUnreachable => "quorum_unreachable",
            Self::AttemptsExhausted => "attempts_exhausted",
            Self::DeadlineExceeded => "deadline_exceeded",
        }
    }
}

/// Decide, and name, whether one item may be admitted.
///
/// `dependency_states` maps dependency work id to its current state. A
/// dependency absent from the map is reported as missing rather than assumed
/// satisfied. `quorum` is optional; when present it gates admission after
/// dependencies are satisfied.
pub fn admission_block(
    item: &WorkItem,
    dependency_states: &BTreeMap<String, WorkState>,
    quorum: Option<(&ReviewQuorum, &BTreeMap<String, ReviewVerdict>)>,
    now: DateTime<Utc>,
) -> Result<AdmissionBlock, OrchError> {
    if !item.state.is_claimable() {
        return Ok(AdmissionBlock::NotClaimable);
    }
    if let Some(deadline) = item.deadline {
        if now >= deadline {
            return Ok(AdmissionBlock::DeadlineExceeded);
        }
    }
    if item.attempt_count >= item.policy.retry.max_attempts {
        return Ok(AdmissionBlock::AttemptsExhausted);
    }

    let mut pending = false;
    for dependency in &item.dependencies {
        match dependency_states.get(&dependency.work_id) {
            None => return Ok(AdmissionBlock::DependencyMissing),
            Some(WorkState::Succeeded) => {}
            Some(state) if state.is_terminal() => {
                return Ok(AdmissionBlock::DependencyUnsatisfiable)
            }
            Some(_) => pending = true,
        }
    }
    if pending {
        return Ok(AdmissionBlock::DependenciesPending);
    }

    if let Some((quorum, verdicts)) = quorum {
        return Ok(match evaluate_quorum(quorum, verdicts)? {
            QuorumOutcome::Met => AdmissionBlock::Admissible,
            QuorumOutcome::Pending => AdmissionBlock::QuorumPending,
            QuorumOutcome::Unreachable => AdmissionBlock::QuorumUnreachable,
        });
    }
    Ok(AdmissionBlock::Admissible)
}

// ---------------------------------------------------------------------------
// Adapter seams — declared, deliberately not implemented here
// ---------------------------------------------------------------------------
//
// Each trait below names an authority that is being built in its own pull
// request. They are declared so this module's shape is honest about what it
// will consume, and so a reviewer can see exactly where the graph stops and
// another authority begins. **No implementation ships in this change**, and
// `admission_block` does not consult any of them: wiring one in before its
// owning PR qualifies would mean inventing a second copy of that authority,
// which is precisely what this slice exists to avoid.

/// Whether a work item may be retried in place after a provider send.
///
/// A send whose fate is unknown must not be repeated implicitly; only an
/// authority that observed the send can say so. **Blocked on #454** (put every
/// physical provider send under attempt authority). Until then the ledger's
/// existing `WorkRetryPolicy` governs retries unchanged.
pub trait ProviderSendAdmission {
    /// `Ok(true)` only when the attempt is proven safe to repeat in place.
    fn same_work_retry_permitted(
        &self,
        work_id: &str,
        attempt_number: u32,
    ) -> Result<bool, OrchError>;
}

/// Sink for graph decisions in the durable append-only audit generation.
///
/// **Blocked on #459** (durable append-only audit generations, v2 authority).
/// Until then graph decisions reach the existing orchestration audit log
/// through the service, and this module writes nothing itself.
pub trait AuditGenerationSink {
    fn record_admission(&self, work_id: &str, block: AdmissionBlock) -> Result<(), OrchError>;
}

/// The authenticated principal and authentication epoch a decision was made
/// under, so a stale principal cannot act on a fresh graph.
///
/// **Blocked on #460** (stale-authentication epoch + principal ownership).
/// Until then callers scope through the service's existing session and
/// workspace checks, which this module does not duplicate.
pub trait PrincipalScope {
    fn principal_id(&self) -> &str;
    fn auth_epoch(&self) -> u64;
}

/// Whether an item that requires Computer Use may be admitted right now.
///
/// The graph never issues, extends, or revalidates a Computer Use grant.
/// **Blocked on #463 and #455** (packaged Computer Use authority: OS-verified
/// identity, operator trust root, one host authority).
pub trait ComputerUseGate {
    fn admission_permitted(&self, work_id: &str) -> Result<bool, OrchError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::workload::{WorkDependency, WorkPolicy};
    use chrono::Duration;
    use uuid::Uuid;

    fn item(id: &str, deps: &[&str]) -> WorkItem {
        let mut item = WorkItem::new(
            "build",
            "synthetic objective",
            Uuid::new_v4(),
            "/tmp/ws",
            "tester",
            WorkPolicy::default(),
        )
        .expect("work item");
        item.work_id = id.to_string();
        item.dependencies = deps
            .iter()
            .map(|dep| WorkDependency {
                work_id: (*dep).to_string(),
                required_state: WorkState::Succeeded,
            })
            .collect();
        item
    }

    fn states(pairs: &[(&str, WorkState)]) -> BTreeMap<String, WorkState> {
        pairs
            .iter()
            .map(|(id, state)| ((*id).to_string(), *state))
            .collect()
    }

    #[test]
    fn a_dependency_cycle_is_rejected_deterministically() {
        let graph = vec![item("a", &["c"]), item("b", &["a"]), item("c", &["b"])];
        let first = validate_dependency_graph(&graph).expect_err("a cycle must be rejected");
        assert!(first.message.contains("cycle"), "{}", first.message);
        assert!(first.message.contains("a, b, c"), "{}", first.message);
        // Same graph, same message, every time: a rejection must be reproducible.
        for _ in 0..8 {
            let again = validate_dependency_graph(&graph).expect_err("still rejected");
            assert_eq!(again.message, first.message);
        }
    }

    #[test]
    fn a_two_node_cycle_and_a_self_edge_are_both_rejected() {
        assert!(validate_dependency_graph(&[item("a", &["b"]), item("b", &["a"])]).is_err());
        let error = validate_dependency_graph(&[item("a", &["a"])])
            .expect_err("a self edge must be rejected");
        assert!(
            error.message.contains("depends on itself"),
            "{}",
            error.message
        );
    }

    #[test]
    fn a_cycle_reachable_only_from_a_healthy_root_is_still_rejected() {
        // `root` peels cleanly; the b/c ring never does. A checker that stopped
        // at the first peelable node would wrongly accept this graph.
        let graph = vec![
            item("root", &[]),
            item("b", &["root", "c"]),
            item("c", &["b"]),
        ];
        let error = validate_dependency_graph(&graph).expect_err("cycle must be rejected");
        assert!(error.message.contains("b, c"), "{}", error.message);
    }

    #[test]
    fn a_deep_chain_does_not_exhaust_the_stack() {
        // Iterative, so depth is not a recursion limit. 10k nodes would blow a
        // recursive walk long before this returns.
        let mut graph = vec![item("n0", &[])];
        for index in 1..10_000 {
            let previous = format!("n{}", index - 1);
            graph.push(item(&format!("n{index}"), &[previous.as_str()]));
        }
        assert!(validate_dependency_graph(&graph).is_ok());

        // Close the chain into one enormous ring: still rejected, still no panic.
        let last = graph.len() - 1;
        graph[0].dependencies = vec![WorkDependency {
            work_id: format!("n{last}"),
            required_state: WorkState::Succeeded,
        }];
        assert!(validate_dependency_graph(&graph).is_err());
    }

    #[test]
    fn unknown_duplicate_and_repeated_ids_are_rejected() {
        let error = validate_dependency_graph(&[item("a", &["ghost"])])
            .expect_err("unknown dependency must be rejected");
        assert!(
            error.message.contains("unknown work ghost"),
            "{}",
            error.message
        );

        let mut duplicated = item("a", &["b"]);
        duplicated.dependencies.push(WorkDependency {
            work_id: "b".into(),
            required_state: WorkState::Succeeded,
        });
        let error = validate_dependency_graph(&[duplicated, item("b", &[])])
            .expect_err("duplicate dependency must be rejected");
        assert!(
            error.message.contains("duplicate dependency"),
            "{}",
            error.message
        );

        let error = validate_dependency_graph(&[item("a", &[]), item("a", &[])])
            .expect_err("duplicate work id must be rejected");
        assert!(
            error.message.contains("duplicate work id"),
            "{}",
            error.message
        );
    }

    #[test]
    fn an_acyclic_diamond_is_accepted() {
        let graph = vec![
            item("root", &[]),
            item("left", &["root"]),
            item("right", &["root"]),
            item("join", &["left", "right"]),
        ];
        assert!(validate_dependency_graph(&graph).is_ok());
    }

    #[test]
    fn the_incremental_check_rejects_an_edge_that_closes_a_ring() {
        let existing = vec![item("a", &[]), item("b", &["a"])];
        // b -> a already exists; adding a -> b closes the ring.
        let candidate = item("a", &["b"]);
        assert!(validate_new_dependencies(&existing, &candidate).is_err());

        // An edge that keeps the graph acyclic is accepted, and re-submitting
        // an existing item replaces rather than duplicates it.
        let candidate = item("c", &["b"]);
        assert!(validate_new_dependencies(&existing, &candidate).is_ok());
        let candidate = item("b", &["a"]);
        assert!(validate_new_dependencies(&existing, &candidate).is_ok());
    }

    #[test]
    fn a_quorum_counts_verdicts_not_reviewer_success() {
        let quorum = ReviewQuorum {
            reviewers: vec!["r1".into(), "r2".into(), "r3".into()],
            required_approvals: 2,
        };
        let mut verdicts = BTreeMap::new();
        assert_eq!(
            evaluate_quorum(&quorum, &verdicts).expect("evaluates"),
            QuorumOutcome::Pending
        );
        verdicts.insert("r1".to_string(), ReviewVerdict::Approve);
        assert_eq!(
            evaluate_quorum(&quorum, &verdicts).expect("evaluates"),
            QuorumOutcome::Pending
        );
        verdicts.insert("r2".to_string(), ReviewVerdict::Approve);
        assert_eq!(
            evaluate_quorum(&quorum, &verdicts).expect("evaluates"),
            QuorumOutcome::Met
        );
    }

    #[test]
    fn a_quorum_that_can_no_longer_be_met_is_unreachable_not_pending() {
        let quorum = ReviewQuorum {
            reviewers: vec!["r1".into(), "r2".into(), "r3".into()],
            required_approvals: 2,
        };
        let mut verdicts = BTreeMap::new();
        verdicts.insert("r1".to_string(), ReviewVerdict::Reject);
        verdicts.insert("r2".to_string(), ReviewVerdict::Reject);
        // One undecided reviewer cannot reach two approvals.
        assert_eq!(
            evaluate_quorum(&quorum, &verdicts).expect("evaluates"),
            QuorumOutcome::Unreachable
        );
    }

    #[test]
    fn a_verdict_from_an_unnamed_identity_cannot_open_the_gate() {
        let quorum = ReviewQuorum {
            reviewers: vec!["r1".into()],
            required_approvals: 1,
        };
        let mut verdicts = BTreeMap::new();
        verdicts.insert("intruder".to_string(), ReviewVerdict::Approve);
        assert_eq!(
            evaluate_quorum(&quorum, &verdicts).expect("evaluates"),
            QuorumOutcome::Pending,
            "an identity the gate does not name must not count"
        );
    }

    #[test]
    fn malformed_quorums_are_rejected() {
        for quorum in [
            ReviewQuorum {
                reviewers: vec![],
                required_approvals: 1,
            },
            ReviewQuorum {
                reviewers: vec!["r1".into()],
                required_approvals: 0,
            },
            ReviewQuorum {
                reviewers: vec!["r1".into()],
                required_approvals: 2,
            },
            ReviewQuorum {
                reviewers: vec!["r1".into(), "r1".into()],
                required_approvals: 1,
            },
            ReviewQuorum {
                reviewers: vec!["  ".into()],
                required_approvals: 1,
            },
            ReviewQuorum {
                reviewers: vec!["x".repeat(MAX_QUORUM_REVIEWERS + 1); MAX_QUORUM_REVIEWERS + 1],
                required_approvals: 1,
            },
        ] {
            assert!(quorum.validate().is_err(), "{quorum:?} must be rejected");
        }
    }

    #[test]
    fn admission_names_the_bound_that_stopped_it() {
        let now = Utc::now();
        let ready = item("a", &[]);
        assert_eq!(
            admission_block(&ready, &BTreeMap::new(), None, now).expect("decides"),
            AdmissionBlock::Admissible
        );

        let gated = item("b", &["a"]);
        assert_eq!(
            admission_block(&gated, &states(&[("a", WorkState::Running)]), None, now)
                .expect("decides"),
            AdmissionBlock::DependenciesPending
        );
        assert_eq!(
            admission_block(&gated, &states(&[("a", WorkState::Failed)]), None, now)
                .expect("decides"),
            AdmissionBlock::DependencyUnsatisfiable
        );
        // A dependency absent from the ledger is reported, never assumed
        // satisfied — which is what `dependency_ready` cannot distinguish.
        assert_eq!(
            admission_block(&gated, &BTreeMap::new(), None, now).expect("decides"),
            AdmissionBlock::DependencyMissing
        );

        let mut expired = item("c", &[]);
        expired.deadline = Some(now - Duration::seconds(1));
        assert_eq!(
            admission_block(&expired, &BTreeMap::new(), None, now).expect("decides"),
            AdmissionBlock::DeadlineExceeded
        );

        let mut spent = item("d", &[]);
        spent.attempt_count = spent.policy.retry.max_attempts;
        assert_eq!(
            admission_block(&spent, &BTreeMap::new(), None, now).expect("decides"),
            AdmissionBlock::AttemptsExhausted
        );
    }

    #[test]
    fn admission_distinguishes_waiting_from_needing_an_operator() {
        // "Nothing to do yet" must not look like "not allowed to do it".
        assert!(!AdmissionBlock::DependenciesPending.needs_operator_attention());
        assert!(!AdmissionBlock::QuorumPending.needs_operator_attention());
        assert!(!AdmissionBlock::Admissible.needs_operator_attention());
        for blocked in [
            AdmissionBlock::DependencyUnsatisfiable,
            AdmissionBlock::DependencyMissing,
            AdmissionBlock::QuorumUnreachable,
            AdmissionBlock::AttemptsExhausted,
            AdmissionBlock::DeadlineExceeded,
        ] {
            assert!(blocked.needs_operator_attention(), "{blocked:?}");
            assert!(!blocked.is_admissible());
        }
    }

    #[test]
    fn a_quorum_gates_admission_only_after_dependencies_are_satisfied() {
        let now = Utc::now();
        let gated = item("b", &["a"]);
        let quorum = ReviewQuorum {
            reviewers: vec!["r1".into()],
            required_approvals: 1,
        };
        let empty = BTreeMap::new();

        // Dependencies come first: an unmet quorum is not reported while a
        // dependency is still running.
        assert_eq!(
            admission_block(
                &gated,
                &states(&[("a", WorkState::Running)]),
                Some((&quorum, &empty)),
                now
            )
            .expect("decides"),
            AdmissionBlock::DependenciesPending
        );

        let satisfied = states(&[("a", WorkState::Succeeded)]);
        assert_eq!(
            admission_block(&gated, &satisfied, Some((&quorum, &empty)), now).expect("decides"),
            AdmissionBlock::QuorumPending
        );

        let mut approved = BTreeMap::new();
        approved.insert("r1".to_string(), ReviewVerdict::Approve);
        assert_eq!(
            admission_block(&gated, &satisfied, Some((&quorum, &approved)), now).expect("decides"),
            AdmissionBlock::Admissible
        );

        let mut rejected = BTreeMap::new();
        rejected.insert("r1".to_string(), ReviewVerdict::Reject);
        assert_eq!(
            admission_block(&gated, &satisfied, Some((&quorum, &rejected)), now).expect("decides"),
            AdmissionBlock::QuorumUnreachable
        );
    }

    #[test]
    fn admission_reasons_and_quorum_round_trip_through_serde() {
        for block in [
            AdmissionBlock::Admissible,
            AdmissionBlock::DependencyMissing,
            AdmissionBlock::QuorumUnreachable,
        ] {
            let encoded = serde_json::to_string(&block).expect("serializes");
            let decoded: AdmissionBlock = serde_json::from_str(&encoded).expect("deserializes");
            assert_eq!(decoded, block);
            assert!(encoded.contains(block.as_str()));
        }
        let quorum = ReviewQuorum {
            reviewers: vec!["r1".into()],
            required_approvals: 1,
        };
        let encoded = serde_json::to_string(&quorum).expect("serializes");
        assert_eq!(
            serde_json::from_str::<ReviewQuorum>(&encoded).expect("deserializes"),
            quorum
        );
        // An unrecognized field fails closed rather than being dropped.
        assert!(serde_json::from_str::<ReviewQuorum>(
            r#"{"reviewers":["r1"],"requiredApprovals":1,"smuggled":true}"#
        )
        .is_err());
    }
}
