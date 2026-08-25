//! Deterministic whole-graph validation.
//!
//! Validation is a pure function of the specification: the same input always
//! produces the same verdict and the same first error. It runs before any
//! child is dispatched and again whenever durable state is reloaded, so a
//! corrupted or hand-edited record cannot resume as if it were valid.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{SwarmError, SwarmResult};
use crate::ids::TaskId;
use crate::spec::{SwarmSpec, TaskKind};

/// Validate an entire swarm specification.
///
/// Checks, in a fixed order so failures are reproducible:
/// 1. bounds, objective text, admission/budget policy, and catalog shape;
/// 2. unique worker IDs and fail-closed catalog admission per worker;
/// 3. unique task IDs, per-task shape, and worker/role/Computer Use binding;
/// 4. every declared dependency resolves to a task in this graph;
/// 5. no task exceeds the configured fan-out bound;
/// 6. review gates name real review tasks that the synthesis task depends on;
/// 7. the dependency graph is acyclic.
pub fn validate_swarm_spec(spec: &SwarmSpec) -> SwarmResult<()> {
    spec.validate_bounds()?;

    let mut worker_ids = BTreeSet::new();
    for worker in &spec.workers {
        worker.validate_against(&spec.catalog)?;
        if !worker_ids.insert(worker.worker_id.clone()) {
            return Err(SwarmError::invalid(format!(
                "worker ID '{}' is declared more than once",
                worker.worker_id
            )));
        }
    }

    let mut task_ids = BTreeSet::new();
    for task in &spec.tasks {
        task.validate_shape()?;
        if !task_ids.insert(task.task_id.clone()) {
            return Err(SwarmError::invalid(format!(
                "task ID '{}' is declared more than once",
                task.task_id
            )));
        }
        let Some(worker) = spec.worker(&task.worker_id) else {
            return Err(SwarmError::invalid(format!(
                "task '{}' names worker '{}', which this swarm does not declare",
                task.task_id, task.worker_id
            )));
        };
        task.validate_against_worker(worker)?;
    }

    let mut fan_out: BTreeMap<&TaskId, u32> = BTreeMap::new();
    for task in &spec.tasks {
        for dependency in &task.dependencies {
            if !task_ids.contains(dependency) {
                return Err(SwarmError::invalid(format!(
                    "task '{}' depends on '{dependency}', which this swarm does not declare",
                    task.task_id
                )));
            }
            let count = fan_out.entry(dependency).or_insert(0);
            *count = count.saturating_add(1);
            if *count > spec.admission.max_fan_out {
                return Err(SwarmError::bound(format!(
                    "task '{dependency}' has more direct dependents than maxFanOut allows"
                )));
            }
        }
    }

    for task in &spec.tasks {
        let Some(gate) = &task.review_gate else {
            continue;
        };
        let dependencies: BTreeSet<&TaskId> = task.dependencies.iter().collect();
        for reviewer in &gate.reviewers {
            let Some(reviewer_task) = spec.task(reviewer) else {
                return Err(SwarmError::invalid(format!(
                    "task '{}' is gated on '{reviewer}', which this swarm does not declare",
                    task.task_id
                )));
            };
            if reviewer_task.kind != TaskKind::Review {
                return Err(SwarmError::invalid(format!(
                    "task '{}' is gated on '{reviewer}', which is not a review task",
                    task.task_id
                )));
            }
            if !dependencies.contains(reviewer) {
                return Err(SwarmError::invalid(format!(
                    "task '{}' is gated on '{reviewer}' without depending on it",
                    task.task_id
                )));
            }
        }
    }

    assert_acyclic(spec)
}

/// Depth-first cycle detection over the dependency edges.
///
/// Iterative rather than recursive so a deep or adversarial graph cannot
/// exhaust the stack, and ordered by the declared task sequence so the
/// reported cycle is stable across runs.
fn assert_acyclic(spec: &SwarmSpec) -> SwarmResult<()> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mark {
        Open,
        Done,
    }

    let dependencies: BTreeMap<&TaskId, &[TaskId]> = spec
        .tasks
        .iter()
        .map(|task| (&task.task_id, task.dependencies.as_slice()))
        .collect();
    let mut marks: BTreeMap<&TaskId, Mark> = BTreeMap::new();

    for task in &spec.tasks {
        if marks.get(&task.task_id).copied() == Some(Mark::Done) {
            continue;
        }
        // Each frame holds a node and how many of its edges have been walked.
        let mut stack: Vec<(&TaskId, usize)> = vec![(&task.task_id, 0)];
        marks.insert(&task.task_id, Mark::Open);

        while let Some((node, cursor)) = stack.pop() {
            let edges = dependencies.get(node).copied().unwrap_or(&[]);
            if cursor >= edges.len() {
                marks.insert(node, Mark::Done);
                continue;
            }
            stack.push((node, cursor + 1));
            let next = &edges[cursor];
            match marks.get(next).copied() {
                Some(Mark::Open) => {
                    return Err(SwarmError::invalid(format!(
                        "task dependencies form a cycle through '{next}'"
                    )));
                }
                Some(Mark::Done) => {}
                None => {
                    marks.insert(next, Mark::Open);
                    stack.push((next, 0));
                }
            }
        }
    }
    Ok(())
}
