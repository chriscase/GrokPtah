//! Dependency-graph safety, durable review quorum, and the canonical
//! admission reason.
//!
//! This module is additive over the durable Work ledger in
//! [`super::workload`]. It introduces no second Work type, lease, assignment
//! path, or projection: every function reads the ledger's own records.
//!
//! Three properties matter here, and each is a correction to something the
//! ledger could not express:
//!
//! * **A dependency cycle is rejected.** `WorkItem::dependency_ready` only
//!   counts succeeded dependencies, so a cycle never fails — it silently
//!   deadlocks every item on the ring with no operator-visible cause.
//! * **Resolution is scope-relative and parity-preserving.** A dependency that
//!   belongs to another principal, workspace, or session is reported exactly
//!   as one that does not exist at all, so a caller cannot use dependency
//!   declaration as an existence oracle for work it may not observe.
//! * **The admission reason explains the canonical persisted state.** The
//!   ledger reconciles a dependency wait to [`WorkState::Blocked`] and an
//!   exceeded deadline to [`WorkState::Failed`], so a reason derived only from
//!   "is it claimable" collapses every case into one. [`evaluate_admission`]
//!   is the single evaluator the store, the claim path, and the projection all
//!   use.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::types::{OrchError, OrchErrorCode};
use super::workload::{WorkItem, WorkState};

/// Maximum work items one scope may hold before dependency graphs in that
/// scope are refused rather than validated. Bounds the work a single
/// dependency-carrying write can cause.
pub const MAX_GRAPH_SCOPE_ITEMS: usize = 4_096;
/// Maximum dependency edges considered across one validation pass.
pub const MAX_GRAPH_EDGES: usize = 16_384;
/// Maximum ledger files examined while collecting one scope. Bounds the work a
/// single validation can do on an installation far larger than the scope.
pub const MAX_GRAPH_SCAN_FILES: usize = 65_536;
/// Maximum reviewers one quorum may name.
pub const MAX_QUORUM_REVIEWERS: usize = 16;
/// Maximum bytes in a reviewer or principal identity.
pub const MAX_REVIEW_IDENTITY_BYTES: usize = 256;

fn invalid(message: impl Into<String>) -> OrchError {
    OrchError::new(OrchErrorCode::InvalidRequest, message)
}

fn exhausted(message: impl Into<String>) -> OrchError {
    OrchError::new(OrchErrorCode::CapacityExhausted, message)
}

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

/// The observation scope a dependency graph is resolved within.
///
/// Nothing outside the scope is visible to validation, which is what keeps a
/// dependency declaration from reporting on work the caller may not observe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphScope<'a> {
    pub session_id: uuid::Uuid,
    pub workspace: &'a str,
    /// The principal that owns the work. Two principals sharing one session
    /// and workspace are still distinct scopes, so neither can use dependency
    /// declaration to probe for the other's work.
    ///
    /// Delegation is deliberately *not* modelled: there is no delegation
    /// authority on this spine yet, so a delegated principal is treated as a
    /// separate scope rather than being widened into its delegator's. That
    /// fails closed; widening lands with the principal/delegation authority.
    pub principal: &'a str,
}

impl GraphScope<'_> {
    /// The scope one item belongs to.
    pub fn of(item: &WorkItem) -> GraphScope<'_> {
        GraphScope {
            session_id: item.session_id,
            workspace: item.workspace.as_str(),
            principal: item.created_by.as_str(),
        }
    }

    /// True when `item` belongs to this scope.
    ///
    /// Workspace comparison is delegated to the ledger's own canonicalizing
    /// comparison so a symlinked or non-normalized path cannot straddle scopes.
    pub fn contains(&self, item: &WorkItem) -> bool {
        item.session_id == self.session_id
            && item.created_by == self.principal
            && super::store::workspaces_match(&item.workspace, self.workspace)
    }
}

/// An authenticated principal, as verified by the host.
///
/// The inner identity is private and the type can only be built inside this
/// crate from an already-authenticated context, so no caller outside the
/// bridge can present a reviewer identity of its own choosing. This is a
/// deliberately narrow stand-in: the canonical principal authority, with
/// generations and delegation, is being built in #460. Until it assembles,
/// every path that needs a verified principal fails closed for external
/// callers rather than trusting a string.
///
/// It is deliberately **not** part of this crate's public surface: freezing a
/// provisional principal type would create a contract that #460 then has to
/// break. Callers reach the verdict path through the orchestration service,
/// which takes an authenticated context and mints this internally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedPrincipal {
    owner_id: String,
    token_id: String,
}

impl VerifiedPrincipal {
    /// Build from an authenticated control-plane context. Crate-private on
    /// purpose: an external caller has no way to construct one.
    pub(crate) fn from_auth(auth: &super::authz::AuthContext) -> Result<Self, OrchError> {
        let principal = Self {
            owner_id: auth.owner_id.clone(),
            token_id: auth.token_id.clone(),
        };
        validate_identity(&principal.owner_id, "principal owner")?;
        validate_identity(&principal.token_id, "principal token")?;
        Ok(principal)
    }

    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    pub fn token_id(&self) -> &str {
        &self.token_id
    }

    /// True when this principal *is* the named reviewer.
    pub fn is(&self, reviewer_id: &str) -> bool {
        self.owner_id == reviewer_id || self.token_id == reviewer_id
    }
}

// ---------------------------------------------------------------------------
// Dependency graph validation
// ---------------------------------------------------------------------------

/// Validate the dependency graph formed by `scope_items` plus `candidate`.
///
/// `scope_items` must already be filtered to the candidate's scope. Any
/// dependency naming an id outside that set is reported as unresolved with the
/// caller's own id echoed back — identically whether the id is unknown to the
/// installation or merely belongs to another scope. Echoing an id the caller
/// supplied reveals nothing; distinguishing the two cases would.
///
/// Checks run in a fixed order so a rejection is reproducible, and cycle
/// detection is iterative so a deep or adversarial graph cannot exhaust the
/// stack.
pub fn validate_scoped_dependency_graph(
    scope_items: &[WorkItem],
    candidate: &WorkItem,
    scope: GraphScope<'_>,
) -> Result<(), OrchError> {
    if !scope.contains(candidate) {
        return Err(OrchError::new(
            OrchErrorCode::WorkspaceMismatch,
            "work item does not belong to the requested scope",
        ));
    }
    // The candidate counts toward the ceiling: a scope exactly at the limit
    // must not be pushed one over by the very write being validated.
    if scope_items.len().saturating_add(1) > MAX_GRAPH_SCOPE_ITEMS {
        return Err(exhausted(format!(
            "scope holds more than {MAX_GRAPH_SCOPE_ITEMS} work items; \
             dependency validation is refused rather than unbounded"
        )));
    }

    // Build the working set: the scope, with the candidate replacing any
    // stored copy of itself.
    let mut nodes: Vec<&WorkItem> = Vec::with_capacity(scope_items.len() + 1);
    let mut declared: BTreeSet<&str> = BTreeSet::new();
    for item in scope_items {
        if item.work_id == candidate.work_id {
            continue;
        }
        if !scope.contains(item) {
            continue;
        }
        if !declared.insert(item.work_id.as_str()) {
            return Err(invalid("scope contains duplicate work ids"));
        }
        nodes.push(item);
    }
    if !declared.insert(candidate.work_id.as_str()) {
        return Err(invalid("scope contains duplicate work ids"));
    }
    nodes.push(candidate);

    let mut edges = 0usize;
    for node in &nodes {
        edges = edges.saturating_add(node.dependencies.len());
        if edges > MAX_GRAPH_EDGES {
            return Err(exhausted(format!(
                "scope declares more than {MAX_GRAPH_EDGES} dependency edges; \
                 validation is refused rather than unbounded"
            )));
        }
    }

    // Only the candidate's own edges are checked for resolvability. Stored
    // items were checked when they were written, and re-reporting them would
    // let one malformed record block unrelated writers in the same scope.
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for dependency in &candidate.dependencies {
        let dep_id = dependency.work_id.as_str();
        if dep_id == candidate.work_id {
            return Err(invalid("work item depends on itself"));
        }
        if !seen.insert(dep_id) {
            return Err(invalid(format!(
                "work item declares dependency {dep_id} more than once"
            )));
        }
        if !declared.contains(dep_id) {
            // Deliberately identical for "no such work" and "work in another
            // principal's scope": the two must not be distinguishable.
            return Err(invalid(format!(
                "dependency {dep_id} is not resolvable in this scope"
            )));
        }
    }

    // A stored item whose own edges dangle cannot make the candidate cyclic,
    // so dangling stored edges are ignored here rather than raised.
    match find_cycle(&nodes, &declared) {
        Some(cycle) => Err(invalid(format!(
            "work dependency cycle: {}",
            cycle.join(" -> ")
        ))),
        None => Ok(()),
    }
}

/// Find one concrete dependency cycle, or `None` when the graph is acyclic.
///
/// Kahn's algorithm alone cannot answer this honestly: the nodes it fails to
/// peel are the cycle members *plus everything downstream of them*, so
/// reporting the unpeeled set names innocent nodes as cycle members. This peels
/// first to isolate the unpeelable region, then walks that region to recover an
/// actual closed walk, which is the only set every member of which is provably
/// on a cycle.
///
/// The walk always takes the smallest available id, so the reported cycle is
/// deterministic for a given graph.
fn find_cycle<'a>(nodes: &[&'a WorkItem], declared: &BTreeSet<&'a str>) -> Option<Vec<&'a str>> {
    let mut indegree: BTreeMap<&str, usize> = BTreeMap::new();
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for node in nodes {
        let resolvable = node
            .dependencies
            .iter()
            .filter(|dependency| declared.contains(dependency.work_id.as_str()))
            .count();
        indegree.insert(node.work_id.as_str(), resolvable);
        for dependency in &node.dependencies {
            if let Some(dep_id) = declared.get(dependency.work_id.as_str()) {
                dependents
                    .entry(dep_id)
                    .or_default()
                    .push(node.work_id.as_str());
            }
        }
    }

    let mut ready: VecDeque<&str> = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(id, _)| *id)
        .collect();
    let mut peeled: BTreeSet<&str> = BTreeSet::new();
    while let Some(id) = ready.pop_front() {
        peeled.insert(id);
        for dependent in dependents.get(id).into_iter().flatten() {
            let degree = indegree.entry(dependent).or_insert(0);
            *degree = degree.saturating_sub(1);
            if *degree == 0 {
                ready.push_back(dependent);
            }
        }
    }
    if peeled.len() == indegree.len() {
        return None;
    }

    // The unpeelable region. Every cycle lies wholly inside it.
    let residue: BTreeSet<&str> = indegree
        .keys()
        .copied()
        .filter(|id| !peeled.contains(id))
        .collect();

    // Follow dependency edges inside the residue until a node repeats. The
    // repeated node opens an actual closed walk; the prefix before it is a
    // tail leading into the cycle, and is dropped.
    let dependency_targets: BTreeMap<&str, Vec<&str>> = nodes
        .iter()
        .filter(|node| residue.contains(node.work_id.as_str()))
        .map(|node| {
            let mut targets: Vec<&str> = node
                .dependencies
                .iter()
                .filter_map(|dependency| declared.get(dependency.work_id.as_str()).copied())
                .filter(|target| residue.contains(target))
                .collect();
            targets.sort_unstable();
            (node.work_id.as_str(), targets)
        })
        .collect();

    let start = *residue.iter().next()?;
    let mut path: Vec<&str> = Vec::new();
    let mut on_path: BTreeMap<&str, usize> = BTreeMap::new();
    let mut current = start;
    loop {
        if let Some(index) = on_path.get(current).copied() {
            let mut cycle: Vec<&str> = path[index..].to_vec();
            // Close the walk so the reported chain reads as a ring.
            cycle.push(current);
            return Some(cycle);
        }
        on_path.insert(current, path.len());
        path.push(current);
        let next = dependency_targets.get(current).and_then(|t| t.first());
        match next {
            Some(next) => current = next,
            // Cannot happen: every residue node has a residue dependency.
            None => return None,
        }
    }
}

// ---------------------------------------------------------------------------
// Durable review quorum
// ---------------------------------------------------------------------------

/// One reviewer's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    Approve,
    Reject,
}

/// Durable review policy attached to one Work item.
///
/// The reviewer set is immutable once the item exists: a gate whose membership
/// can be edited after the fact approves nothing. `policy_revision` is bound
/// into every receipt, so a receipt recorded under an older policy cannot be
/// counted against a newer one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkReviewPolicy {
    /// Reviewer principal identities whose verdicts are counted.
    pub reviewers: Vec<String>,
    /// Approvals required. Never zero, never more than `reviewers.len()`.
    pub required_approvals: u32,
    /// Monotonic revision of this policy. Bound into every receipt.
    pub policy_revision: u64,
}

/// Why a work item is in the `Blocked` state.
///
/// Durable and typed, so reconciliation never has to infer provenance from the
/// shape of a free-form string. A record written before this existed
/// deserializes to `None`, which is treated as derived — the behavior those
/// records already had.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockProvenance {
    /// Reconciliation blocked it because a dependency was not satisfied. It is
    /// lifted automatically when the dependency is.
    Derived,
    /// A human or coordinator blocked it explicitly. Reconciliation never
    /// lifts it and never overwrites its reason.
    Manual,
}

impl BlockProvenance {
    pub fn is_manual(self) -> bool {
        matches!(self, Self::Manual)
    }
}

/// The exact executing authority a review is bound to.
///
/// A quorum approves *a specific agent running a specific specification*. If
/// the assignment, the agent's specification revision, or its selected model
/// changes, the thing that will execute is not the thing that was approved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionIdentity {
    pub agent_id: String,
    pub agent_spec_revision: u64,
    pub model_selection_key: String,
    /// Owning principal of the executing agent. An agent re-homed to a
    /// different owner is a different executing authority even when its
    /// specification is unchanged.
    pub owner_principal_id: Option<String>,
}

/// Digest of everything a reviewer is actually agreeing to.
///
/// The Work item's ordinary `revision` cannot serve as the review subject:
/// recording a receipt bumps it, so every verdict would invalidate the one
/// before it. This digest instead covers exactly what defines *what is being
/// reviewed and what will run it* — never the revision, the state, the
/// receipts, or any reconciliation-derived field — so it is stable while
/// verdicts accumulate and changes the moment the subject or its executing
/// authority does.
///
/// `execution` is the resolved identity of the assigned agent. Passing `None`
/// means no agent is assigned; it is not a way to omit the binding, because a
/// later assignment changes the digest and retires prior approvals.
pub fn review_subject_digest(item: &WorkItem, execution: Option<&ExecutionIdentity>) -> String {
    let dependencies: Vec<serde_json::Value> = item
        .dependencies
        .iter()
        .map(|dependency| {
            serde_json::json!({
                "workId": dependency.work_id,
                "requiredState": dependency.required_state,
            })
        })
        .collect();
    super::types::hash_payload(&serde_json::json!({
        "workId": item.work_id,
        "kind": item.kind,
        "objective": item.objective,
        "sessionId": item.session_id,
        "workspace": item.workspace,
        "createdBy": item.created_by,
        "parentWorkId": item.parent_work_id,
        "isContainer": item.is_container,
        "deadline": item.deadline,
        "dependencies": dependencies,
        "policy": item.policy,
        "review": item.review,
        // Who will execute it, and under what authority. Reassignment, an
        // agent-specification bump, or a model change all land here.
        "assignedAgentId": item.assigned_agent_id,
        "assignmentStatus": item.assignment_status,
        "execution": execution,
        // Where the work came from. A manager step id such as
        // `__manager_decision__` changes submission behavior, and routine and
        // activation lineage changes what a run is permitted to do, so all of
        // it is part of what a reviewer agreed to.
        "sourceRoutineId": item.source_routine_id,
        "sourceActivationId": item.source_activation_id,
        "sourceManagerPlanId": item.source_manager_plan_id,
        "sourceManagerStepId": item.source_manager_step_id,
    }))
}

impl WorkReviewPolicy {
    pub fn validate(&self) -> Result<(), OrchError> {
        if self.reviewers.is_empty() || self.reviewers.len() > MAX_QUORUM_REVIEWERS {
            return Err(invalid(format!(
                "a review policy names 1..={MAX_QUORUM_REVIEWERS} reviewers"
            )));
        }
        let mut seen = BTreeSet::new();
        for reviewer in &self.reviewers {
            validate_identity(reviewer, "reviewer")?;
            if !seen.insert(reviewer.as_str()) {
                return Err(invalid("a review policy names one reviewer twice"));
            }
        }
        if self.required_approvals == 0 {
            return Err(invalid("a review policy requires at least one approval"));
        }
        if self.required_approvals as usize > self.reviewers.len() {
            return Err(invalid(
                "a review policy cannot require more approvals than it has reviewers",
            ));
        }
        if self.policy_revision == 0 {
            return Err(invalid("review policy revision must be >= 1"));
        }
        Ok(())
    }

    pub fn names(&self, reviewer: &str) -> bool {
        self.reviewers.iter().any(|named| named == reviewer)
    }
}

fn validate_identity(value: &str, field: &str) -> Result<(), OrchError> {
    if value.trim().is_empty() || value.len() > MAX_REVIEW_IDENTITY_BYTES || value.contains('\0') {
        return Err(invalid(format!("{field} identity is invalid")));
    }
    Ok(())
}

/// A durable, attributable record of one reviewer's verdict.
///
/// The receipt binds the authenticated principal that produced it, the exact
/// Work revision it was cast against, and the review policy revision in force.
/// A verdict that cannot name all three is not counted.
///
/// Binding an authentication *epoch* is deliberately absent: no epoch
/// authority exists on this spine yet (it is being built in #460), and
/// inventing one here would be a second, weaker copy of it. Until it lands, a
/// receipt is attributable but does not survive credential rotation as a
/// distinct fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewReceipt {
    pub reviewer_id: String,
    /// Stable credential identity of the principal that recorded the verdict.
    pub principal_token_id: String,
    /// Owner identity of that principal.
    pub principal_owner_id: String,
    pub verdict: ReviewVerdict,
    /// Digest of the exact subject the reviewer agreed to. A receipt whose
    /// digest no longer matches the item is not counted, so editing the
    /// objective, dependencies, policy, or gate silently invalidates every
    /// verdict cast against the old subject rather than carrying them over.
    pub subject_digest: String,
    /// Work revision at the moment the verdict was cast. Recorded for the
    /// audit trail only: it is *not* the review subject, because recording a
    /// receipt bumps the revision.
    pub work_revision: u64,
    /// Review policy revision in force when it was cast.
    pub policy_revision: u64,
    pub recorded_at: DateTime<Utc>,
    /// Set when the verdict was explicitly withdrawn. A revoked receipt is
    /// retained rather than deleted so the audit trail stays complete, and is
    /// not counted toward the quorum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
}

impl ReviewReceipt {
    pub fn validate(&self) -> Result<(), OrchError> {
        validate_identity(&self.reviewer_id, "reviewer")?;
        validate_identity(&self.principal_token_id, "principal token")?;
        validate_identity(&self.principal_owner_id, "principal owner")?;
        if self.subject_digest.len() != 64
            || !self.subject_digest.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err(invalid("review receipt subject digest is invalid"));
        }
        if self.work_revision == 0 || self.policy_revision == 0 {
            return Err(invalid("review receipt revisions must be >= 1"));
        }
        if self
            .revoked_at
            .is_some_and(|revoked| revoked < self.recorded_at)
        {
            return Err(invalid("review receipt was revoked before it was recorded"));
        }
        Ok(())
    }

    /// True when this receipt counts toward `policy` for `subject_digest`.
    ///
    /// The subject check is what stops a receipt from surviving an arbitrary
    /// later mutation of the work it approved.
    pub fn counts_for(&self, policy: &WorkReviewPolicy, subject_digest: &str) -> bool {
        self.revoked_at.is_none()
            && self.policy_revision == policy.policy_revision
            && self.subject_digest == subject_digest
            && policy.names(&self.reviewer_id)
    }
}

/// Where a quorum stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuorumOutcome {
    Met,
    Pending,
    Unreachable,
}

/// Evaluate a durable review policy against its durable receipts.
///
/// Verdicts are counted, not reviewer success: a reviewer that ran to
/// completion and rejected has done its job and still withholds approval. A
/// receipt from an identity the policy does not name, from a superseded policy
/// revision, or that has been revoked, is ignored rather than counted.
pub fn evaluate_quorum(
    policy: &WorkReviewPolicy,
    receipts: &[ReviewReceipt],
    subject_digest: &str,
) -> Result<QuorumOutcome, OrchError> {
    policy.validate()?;
    let mut latest: BTreeMap<&str, &ReviewReceipt> = BTreeMap::new();
    for receipt in receipts {
        if !receipt.counts_for(policy, subject_digest) {
            continue;
        }
        latest
            .entry(receipt.reviewer_id.as_str())
            .and_modify(|current| {
                if receipt.recorded_at > current.recorded_at {
                    *current = receipt;
                }
            })
            .or_insert(receipt);
    }

    let mut approvals = 0u32;
    let mut undecided = 0u32;
    for reviewer in &policy.reviewers {
        match latest.get(reviewer.as_str()).map(|r| r.verdict) {
            Some(ReviewVerdict::Approve) => approvals = approvals.saturating_add(1),
            Some(ReviewVerdict::Reject) => {}
            None => undecided = undecided.saturating_add(1),
        }
    }
    if approvals >= policy.required_approvals {
        return Ok(QuorumOutcome::Met);
    }
    if approvals.saturating_add(undecided) < policy.required_approvals {
        return Ok(QuorumOutcome::Unreachable);
    }
    Ok(QuorumOutcome::Pending)
}

// ---------------------------------------------------------------------------
// The canonical admission reason
// ---------------------------------------------------------------------------

/// Why a work item is where it is, and whether it may be claimed.
///
/// Every variant explains a *canonical persisted state*: the ledger reconciles
/// a dependency wait to `Blocked` and an exceeded deadline to `Failed`, so a
/// reason computed from claimability alone would report both as "not
/// claimable" and lose the distinction an operator needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionBlock {
    /// Dependencies satisfied, gate met, deadline in the future: claimable.
    Admissible,
    /// One or more dependencies have not reached their required state.
    DependenciesPending,
    /// A dependency reached a terminal state other than the required one, so
    /// this item can never become ready.
    DependencyUnsatisfiable,
    /// A declared dependency is not resolvable in this item's scope. Reported
    /// identically whether it is unknown or belongs to another scope.
    DependencyUnresolved,
    /// A review quorum has not yet been met.
    ReviewPending,
    /// A review quorum can no longer be met.
    ReviewUnreachable,
    /// An attempt currently holds the item.
    AttemptActive,
    /// Waiting on an explicit human or coordinator input.
    AwaitingInput,
    /// Waiting on the ledger's own approval gate.
    AwaitingApproval,
    /// The retry budget is spent.
    AttemptsExhausted,
    /// The deadline has passed.
    DeadlineExceeded,
    /// The item finished successfully.
    Succeeded,
    /// The item finished unsuccessfully.
    Failed,
    /// The item was cancelled.
    Cancelled,
    /// A container item is not itself executed.
    Container,
    /// A human or coordinator blocked this item explicitly. Reconciliation
    /// never clears it and never overwrites its reason.
    ManuallyBlocked,
    /// The item carries a review gate but names no accepted executor, so there
    /// is nothing for a quorum to approve. `Unassigned` and `Declined` work is
    /// claimable by any in-scope worker, so approving it would approve *no
    /// executor* and let whichever worker claimed first become one.
    ExecutorUnbound,
}

impl AdmissionBlock {
    pub fn is_admissible(self) -> bool {
        matches!(self, Self::Admissible)
    }

    /// True when the item has settled and will not move on its own.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    /// True when waiting will not help — an operator has to intervene.
    pub fn needs_operator_attention(self) -> bool {
        matches!(
            self,
            Self::DependencyUnsatisfiable
                | Self::DependencyUnresolved
                | Self::ReviewUnreachable
                | Self::AttemptsExhausted
                | Self::DeadlineExceeded
                | Self::ManuallyBlocked
                | Self::ExecutorUnbound
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admissible => "admissible",
            Self::DependenciesPending => "dependencies_pending",
            Self::DependencyUnsatisfiable => "dependency_unsatisfiable",
            Self::DependencyUnresolved => "dependency_unresolved",
            Self::ReviewPending => "review_pending",
            Self::ReviewUnreachable => "review_unreachable",
            Self::AttemptActive => "attempt_active",
            Self::AwaitingInput => "awaiting_input",
            Self::AwaitingApproval => "awaiting_approval",
            Self::AttemptsExhausted => "attempts_exhausted",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Container => "container",
            Self::ManuallyBlocked => "manually_blocked",
            Self::ExecutorUnbound => "executor_unbound",
        }
    }
}

/// Resolved state of one dependency, from the depending item's scope.
///
/// `None` means unresolvable — unknown, or outside the scope. The two are not
/// distinguished, here or anywhere a caller can observe.
pub type DependencyStates = BTreeMap<String, Option<WorkState>>;

/// True when the item names an executor that has accepted the assignment.
///
/// `Offered` is not enough (the offer may still be declined), and `Unassigned`
/// or `Declined` work is claimable by any in-scope worker.
pub fn has_accepted_executor(item: &WorkItem) -> bool {
    item.assignment_status == super::workload::AssignmentStatus::Accepted
        && item
            .assigned_agent_id
            .as_deref()
            .is_some_and(|agent| !agent.trim().is_empty())
}

/// The single admission evaluator.
///
/// The store's reconciliation, the claim path, managed execution, and the
/// public projection all call this, so none of them can drift into its own
/// answer. It is pure: callers supply resolved dependency states and `now`.
pub fn evaluate_admission(
    item: &WorkItem,
    dependency_states: &DependencyStates,
    receipts: &[ReviewReceipt],
    execution: Option<&ExecutionIdentity>,
    now: DateTime<Utc>,
) -> AdmissionBlock {
    match item.state {
        WorkState::Succeeded => return AdmissionBlock::Succeeded,
        WorkState::Failed => {
            // A deadline failure is reported as such rather than as a generic
            // failure, because it is the one the operator can still act on.
            if item.deadline.is_some_and(|deadline| deadline <= now) {
                return AdmissionBlock::DeadlineExceeded;
            }
            return AdmissionBlock::Failed;
        }
        WorkState::Cancelled => return AdmissionBlock::Cancelled,
        _ => {}
    }
    // A container is never executable whatever holds it, and `Container` is
    // the more informative answer than any hold placed on it.
    if item.is_container {
        return AdmissionBlock::Container;
    }
    // An explicit block outranks everything below: a human who stopped this
    // item is not overruled by dependencies becoming ready.
    //
    // Ambiguous provenance fails closed. `OrchStore::open` stamps every legacy
    // `Blocked` record as manual before reconciliation can see it, so `None`
    // here means that migration could not write the record. Reading it as
    // derived would let an upgrade silently re-queue -- and then execute --
    // work a human had stopped.
    if item.state == WorkState::Blocked
        && item.block_provenance.is_none_or(BlockProvenance::is_manual)
    {
        return AdmissionBlock::ManuallyBlocked;
    }
    if item.deadline.is_some_and(|deadline| deadline <= now) {
        return AdmissionBlock::DeadlineExceeded;
    }
    match item.state {
        WorkState::Leased | WorkState::Running => return AdmissionBlock::AttemptActive,
        WorkState::AwaitingInput => return AdmissionBlock::AwaitingInput,
        WorkState::AwaitingApproval | WorkState::Review => return AdmissionBlock::AwaitingApproval,
        _ => {}
    }

    // Dependencies are evaluated for Queued and Blocked alike: `Blocked` is
    // the ledger's reconciled encoding of "waiting", not an independent fact.
    let mut pending = false;
    for dependency in &item.dependencies {
        match dependency_states.get(&dependency.work_id) {
            None | Some(None) => return AdmissionBlock::DependencyUnresolved,
            Some(Some(state)) if *state == dependency.required_state => {}
            Some(Some(state)) if state.is_terminal() => {
                return AdmissionBlock::DependencyUnsatisfiable
            }
            Some(Some(_)) => pending = true,
        }
    }
    if pending {
        return AdmissionBlock::DependenciesPending;
    }

    if item.attempt_count >= item.policy.retry.max_attempts {
        return AdmissionBlock::AttemptsExhausted;
    }

    if let Some(policy) = item.review.as_ref() {
        // A gate can only approve a named executor. Without an accepted
        // assignment the subject would record "no executor", and the ledger
        // lets any in-scope worker claim `Unassigned` or `Declined` work --
        // so the approved authority and the running one would differ.
        if !has_accepted_executor(item) || execution.is_none() {
            return AdmissionBlock::ExecutorUnbound;
        }
        let subject = review_subject_digest(item, execution);
        return match evaluate_quorum(policy, receipts, &subject) {
            Ok(QuorumOutcome::Met) => AdmissionBlock::Admissible,
            Ok(QuorumOutcome::Pending) => AdmissionBlock::ReviewPending,
            Ok(QuorumOutcome::Unreachable) => AdmissionBlock::ReviewUnreachable,
            // A policy that cannot be validated never opens the gate.
            Err(_) => AdmissionBlock::ReviewUnreachable,
        };
    }
    AdmissionBlock::Admissible
}
