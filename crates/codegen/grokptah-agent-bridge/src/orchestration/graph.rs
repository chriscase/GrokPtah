//! The single durable work-graph authority (#305).
//!
//! This module is additive over the durable Work ledger in
//! [`super::workload`]. It introduces no second Work type, lease, assignment
//! path, or claim path: every function reads the ledger's own records, and the
//! store, the claim path, and the operator projection all call the one
//! evaluator here rather than each deriving its own answer.
//!
//! Four properties matter, and each corrects something the ledger could not
//! express before:
//!
//! * **A dependency is resolved inside one lane.** The reconciler used to look
//!   a dependency up by id across the whole installation, so an item in one
//!   session could name work in another and read that work's progress out of
//!   its own `Blocked`/`Queued` transitions. Resolution is now scoped, and an
//!   id outside the scope is reported exactly as one that does not exist.
//! * **A dependency cycle is rejected at write time.** `WorkItem::validate`
//!   only rejects a self-edge, and `WorkItem::dependency_ready` only counts
//!   succeeded dependencies, so a two-item ring never fails — it silently
//!   deadlocks every item on the ring with no operator-visible cause.
//! * **A block remembers who placed it.** Reconciliation lifted any `Blocked`
//!   item whose dependencies were satisfied, and an item blocked by an
//!   operator has no dependencies to satisfy — so the next reconciliation tick
//!   silently re-queued, and then executed, work a human had stopped. A block
//!   now carries typed provenance and only a derived one is lifted.
//! * **The admission reason explains the canonical persisted state.** The
//!   ledger reconciles a dependency wait to [`WorkState::Blocked`] and an
//!   exceeded deadline to [`WorkState::Failed`], so a reason derived from "is
//!   it claimable" collapses every case into one.
//!
//! Ordering is a total order here rather than the previous partial one:
//! priority and creation instant alone leave ties to `read_dir`, which is not
//! reproducible across hosts or restarts.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::store::workspaces_match;
use super::types::{OrchError, OrchErrorCode};
use super::workload::{BlockProvenance, WorkItem, WorkState};

/// Maximum work items one lane may hold before dependency graphs in that lane
/// are refused rather than validated. Bounds the work a single
/// dependency-carrying write can cause.
pub const MAX_GRAPH_SCOPE_ITEMS: usize = 4_096;
/// Maximum dependency edges considered across one validation pass.
pub const MAX_GRAPH_EDGES: usize = 16_384;

fn invalid(message: impl Into<String>) -> OrchError {
    OrchError::new(OrchErrorCode::InvalidRequest, message)
}

fn exhausted(message: impl Into<String>) -> OrchError {
    OrchError::new(OrchErrorCode::CapacityExhausted, message)
}

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

/// The lane a dependency graph is resolved within.
///
/// Nothing outside the scope is visible to validation, resolution, or
/// projection, which is what keeps a dependency declaration from reporting on
/// work the caller may not observe.
///
/// The scope is deliberately the *lane* — session plus workspace — and not the
/// creating principal. One durable manager plan legitimately spans principals
/// inside a single lane: `OrchestrationService` advances a plan under the
/// caller's own token, while `ManagerSupervisor` advances the same plan under
/// its own identity, so a later step and the earlier step it depends on can
/// carry different `created_by` values. Adding the principal dimension here
/// would strand every supervisor-advanced step behind a dependency it is not
/// permitted to see. Principal-level separation belongs to the canonical
/// principal authority, which owns delegation and generations; until it lands,
/// this seam binds the lane and says so rather than inventing a weaker copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphScope<'a> {
    pub session_id: Uuid,
    pub workspace: &'a str,
}

impl GraphScope<'_> {
    /// The lane one item belongs to.
    pub fn of(item: &WorkItem) -> GraphScope<'_> {
        GraphScope {
            session_id: item.session_id,
            workspace: item.workspace.as_str(),
        }
    }

    /// True when `item` belongs to this lane.
    ///
    /// Workspace comparison is delegated to the ledger's own canonicalizing
    /// comparison so a symlinked or non-normalized path cannot straddle lanes.
    pub fn contains(&self, item: &WorkItem) -> bool {
        item.session_id == self.session_id && workspaces_match(&item.workspace, self.workspace)
    }
}

// ---------------------------------------------------------------------------
// Deterministic ordering
// ---------------------------------------------------------------------------

/// The ledger's total order over work: priority descending, then creation
/// instant ascending, then work id ascending.
///
/// The work id tiebreak is what makes the order total. Priority and creation
/// instant alone are a partial order — two items created inside the same clock
/// tick compare equal and fall back to directory-read order, which differs
/// between hosts and between restarts on one host.
pub fn compare_work_order(left: &WorkItem, right: &WorkItem) -> std::cmp::Ordering {
    right
        .priority
        .cmp(&left.priority)
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| left.work_id.cmp(&right.work_id))
}

/// Sort work into the ledger's total order in place.
pub fn order_work(items: &mut [WorkItem]) {
    items.sort_by(compare_work_order);
}

// ---------------------------------------------------------------------------
// Dependency resolution
// ---------------------------------------------------------------------------

/// Resolved state of one dependency, from the depending item's lane.
///
/// `None` means unresolvable — unknown to the installation, or outside the
/// lane. The two are not distinguished, here or anywhere a caller can observe.
pub type DependencyStates = BTreeMap<String, Option<WorkState>>;

/// Resolve `candidate`'s declared dependencies against `scope_items`.
///
/// `scope_items` need not be pre-filtered; anything outside `scope` is ignored
/// rather than rejected, so one foreign record cannot make an in-lane
/// resolution fail. Every declared dependency gets an entry, so a caller can
/// tell "declared and unresolvable" from "not declared".
pub fn resolve_dependency_states(
    scope_items: &[WorkItem],
    candidate: &WorkItem,
    scope: GraphScope<'_>,
) -> DependencyStates {
    let mut visible: BTreeMap<&str, WorkState> = BTreeMap::new();
    for item in scope_items {
        if scope.contains(item) {
            visible.insert(item.work_id.as_str(), item.state);
        }
    }
    candidate
        .dependencies
        .iter()
        .map(|dependency| {
            let resolved = visible.get(dependency.work_id.as_str()).copied();
            (dependency.work_id.clone(), resolved)
        })
        .collect()
}

/// Validate the dependency graph formed by `scope_items` plus `candidate`.
///
/// Any dependency naming an id outside the lane is reported as unresolved with
/// the caller's own id echoed back — identically whether the id is unknown to
/// the installation or merely belongs to another lane. Echoing an id the
/// caller supplied reveals nothing; distinguishing the two cases would turn
/// dependency declaration into an existence oracle for work in other lanes.
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
    // The candidate counts toward the ceiling: a lane exactly at the limit
    // must not be pushed one over by the very write being validated.
    if scope_items.len().saturating_add(1) > MAX_GRAPH_SCOPE_ITEMS {
        return Err(exhausted(format!(
            "scope holds more than {MAX_GRAPH_SCOPE_ITEMS} work items; \
             dependency validation is refused rather than unbounded"
        )));
    }

    // Build the working set: the lane, with the candidate replacing any stored
    // copy of itself.
    let mut nodes: Vec<&WorkItem> = Vec::with_capacity(scope_items.len().saturating_add(1));
    let mut declared: BTreeSet<&str> = BTreeSet::new();
    for item in scope_items {
        if item.work_id == candidate.work_id || !scope.contains(item) {
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
    // let one malformed record block unrelated writers in the same lane.
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
            // lane": the two must not be distinguishable.
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
/// reporting the unpeeled set names innocent nodes as cycle members. This
/// peels first to isolate the unpeelable region, then walks that region to
/// recover an actual closed walk, which is the only set every member of which
/// is provably on a cycle.
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
        match dependency_targets.get(current).and_then(|t| t.first()) {
            Some(next) => current = next,
            // Cannot happen: every residue node has a residue dependency.
            None => return None,
        }
    }
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
    /// Dependencies satisfied, deadline in the future, retry budget left.
    Admissible,
    /// One or more dependencies have not reached their required state.
    DependenciesPending,
    /// A dependency reached a terminal state other than the required one, so
    /// this item can never become ready.
    DependencyUnsatisfiable,
    /// A declared dependency is not resolvable in this item's lane. Reported
    /// identically whether it is unknown or belongs to another lane.
    DependencyUnresolved,
    /// An attempt currently holds the item.
    AttemptActive,
    /// Waiting on an explicit human or coordinator input.
    AwaitingInput,
    /// Waiting on the ledger's own approval gate.
    AwaitingApproval,
    /// Parked for review of a completed or approval-gated result.
    UnderReview,
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
                | Self::AttemptsExhausted
                | Self::DeadlineExceeded
                | Self::ManuallyBlocked
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admissible => "admissible",
            Self::DependenciesPending => "dependencies_pending",
            Self::DependencyUnsatisfiable => "dependency_unsatisfiable",
            Self::DependencyUnresolved => "dependency_unresolved",
            Self::AttemptActive => "attempt_active",
            Self::AwaitingInput => "awaiting_input",
            Self::AwaitingApproval => "awaiting_approval",
            Self::UnderReview => "under_review",
            Self::AttemptsExhausted => "attempts_exhausted",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Container => "container",
            Self::ManuallyBlocked => "manually_blocked",
        }
    }
}

/// The single admission evaluator.
///
/// The store's reconciliation, the claim path, and the operator projection all
/// call this, so none of them can drift into its own answer. It is pure:
/// callers supply resolved dependency states and `now`.
pub fn evaluate_admission(
    item: &WorkItem,
    dependency_states: &DependencyStates,
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
    // Ambiguous provenance fails closed. A record written before provenance
    // was typed carries none, and reading it as derived would let an upgrade
    // silently re-queue -- and then execute -- work a human had stopped. Such
    // a record is lifted by an explicit `unblock_work`, never by a tick.
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
        WorkState::AwaitingApproval => return AdmissionBlock::AwaitingApproval,
        WorkState::Review => return AdmissionBlock::UnderReview,
        _ => {}
    }

    // Dependencies are evaluated for Queued and derived-Blocked alike:
    // `Blocked` is the ledger's reconciled encoding of "waiting", not an
    // independent fact.
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

    AdmissionBlock::Admissible
}

// ---------------------------------------------------------------------------
// Redacted projection
// ---------------------------------------------------------------------------

/// One node of the lane-scoped operator view of the work graph.
///
/// Every field here is either the caller's own lane coordinate or a typed
/// enumeration. Deliberately absent, because an operator view is rendered
/// wherever a client runs: the workspace path, the session id, the creating
/// principal, the assigned agent, the free-text objective and progress and
/// result (which carry paths, provider names, and tool names), the free-text
/// `blocked_reason`, source routine/activation/manager links, and every
/// attempt, claimant, and lease identifier. A reader learns the shape of its
/// own lane's graph and nothing about who or what is executing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkGraphNode {
    pub work_id: String,
    pub kind: String,
    pub state: WorkState,
    pub admission: AdmissionBlock,
    pub priority: i32,
    pub revision: u64,
    pub attempt_count: u32,
    pub max_attempts: u32,
    pub is_container: bool,
    pub deadline: Option<DateTime<Utc>>,
    /// Dependency ids resolvable inside this lane, in the ledger's order.
    pub dependencies: Vec<String>,
    /// How many declared dependencies were not resolvable in this lane. A
    /// count, never the ids: the ids of another lane's work are exactly what
    /// this projection exists to withhold.
    pub unresolved_dependencies: usize,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Project one lane's work graph, in the ledger's total order.
///
/// Items outside `scope` are dropped rather than rejected, so one foreign
/// record cannot deny an operator the view of its own lane.
pub fn project_scoped_graph(
    scope_items: &[WorkItem],
    scope: GraphScope<'_>,
    now: DateTime<Utc>,
) -> Vec<WorkGraphNode> {
    let mut lane: Vec<WorkItem> = scope_items
        .iter()
        .filter(|item| scope.contains(item))
        .cloned()
        .collect();
    order_work(&mut lane);
    lane.iter()
        .map(|item| {
            let states = resolve_dependency_states(&lane, item, scope);
            let mut dependencies: Vec<String> = Vec::new();
            let mut unresolved = 0usize;
            for dependency in &item.dependencies {
                match states.get(&dependency.work_id) {
                    Some(Some(_)) => dependencies.push(dependency.work_id.clone()),
                    _ => unresolved = unresolved.saturating_add(1),
                }
            }
            WorkGraphNode {
                work_id: item.work_id.clone(),
                kind: item.kind.clone(),
                state: item.state,
                admission: evaluate_admission(item, &states, now),
                priority: item.priority,
                revision: item.revision,
                attempt_count: item.attempt_count,
                max_attempts: item.policy.retry.max_attempts,
                is_container: item.is_container,
                deadline: item.deadline,
                dependencies,
                unresolved_dependencies: unresolved,
                created_at: item.created_at,
                updated_at: item.updated_at,
            }
        })
        .collect()
}
